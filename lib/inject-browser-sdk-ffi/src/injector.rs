#![allow(clippy::missing_safety_doc)]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use core::{ptr::null, slice};

use crate::{Snippet, SnippetInternal};
use inject_browser_sdk::injector;

/// A stateful object used to inject a Browser SDK [`Snippet`] inside a HTML document. Use
/// [`injector_create`] to create a new one.
///
/// Note: this struct is exposed as an FFI opaque pointer. Structure layout is irrelevant to the
/// library consumer as it is only allocated and accessed by the library.
pub struct Injector {
    // Use an internal [`injector::Injector`] with a static lifetime. Because this struct will be
    // used via a raw pointer, no lifetime can be guaranteed.
    inner: injector::Injector<'static>,
    // Store the last slices, so we keep ownership and return a pointer to the library consumer.
    slices_memory_storage: [BytesSlice; 4],
}

/// Where the snippet goes within the `<head>` of an HTML document, see [`injector_create_with_strategy`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionStrategy {
    /// Right before the `</head>` end tag, what [`injector_create`] uses.
    ///
    /// Only the first real one counts (not one written in a comment, an attribute value, a script or
    /// a style), and only in a document that looks like HTML: an `<html>`, `<meta>` or `<head>`
    /// start tag or a `<!DOCTYPE html>` within its first 10 KiB, comments, doctypes and whitespace
    /// not counted, before it. Body content (`<body>`, `<div>`…), text
    /// outside tags (whitespace and a leading byte order mark aside), a `</body>`, `</html>` or
    /// `</br>` end tag and a CDATA section end the search. The `</head>` tag may come anywhere. Holds back nothing but a `</head` tag a write cuts short.
    HeadCloseTag = 0,

    /// As early as possible within the head: right after the `<head>` start tag when it is within
    /// the first 1024 bytes, unless the head holds `<meta>` tags declaring the character encoding,
    /// `X-UA-Compatible` or a `Content-Security-Policy`: then right after the last of them. Other
    /// `<meta>` tags, SEO, viewport or the tracer's, do not hold the snippet back. Past those bytes,
    /// right after a `<head>` tag if the document looks like HTML, as `HeadCloseTag` tells it. A `<head>` tag past body
    /// content does not count: the browser ignores it.
    ///
    /// Holds the first 1024 bytes back until they are complete (see [`injector_held_back_length`]),
    /// which delays the start of the response body by as much, and more, up to 10 KiB, while the
    /// head is still a run of `<meta>` and `<title>` tags past them.
    HeadOpenTag = 1,
}

impl From<InjectionStrategy> for injector::InjectionStrategy {
    fn from(strategy: InjectionStrategy) -> Self {
        return match strategy {
            InjectionStrategy::HeadCloseTag => injector::InjectionStrategy::HeadCloseTag,
            InjectionStrategy::HeadOpenTag => injector::InjectionStrategy::HeadOpenTag,
        };
    }
}

/// Create an [`Injector`] injector based on a [`Snippet`], injecting right before the `</head>`
/// tag: [`injector_create_with_strategy`] with [`InjectionStrategy::HeadCloseTag`]. Where no
/// injection point is found, [`injector_end`] pads the response instead.
///
/// Returns `NULL` when `snippet` is `NULL` or when `snippet->error_code != 0`
/// (the snippet failed to build and has no content to inject). Callers must
/// null-check the return value and skip injection when it is `NULL`, otherwise
/// the [`Injector`] would silently pass the response through unmodified and
/// mask the upstream configuration error.
///
/// # Memory ownership
///
/// The `Injector` instance returned by this function is owned by the library and should be freed
/// by using [`injector_cleanup`] once done using it.
///
/// # Example
///
/// ```c
/// if (snippet == NULL || snippet->error_code != 0) {
///   // log and skip injection
///   return;
/// }
/// Injector* injector = injector_create(snippet);
/// if (injector == NULL) {
///   // log and skip injection
///   return;
/// }
///
/// // ... use injector
///
/// injector_cleanup(injector);
/// ```
#[no_mangle]
pub unsafe extern "C" fn injector_create(snippet: *const Snippet) -> *mut Injector {
    return injector_create_with_strategy(snippet, InjectionStrategy::HeadCloseTag);
}

/// Create an [`Injector`] like [`injector_create`], injecting where `strategy` says. Same return
/// value and memory ownership.
#[no_mangle]
pub unsafe extern "C" fn injector_create_with_strategy(
    snippet: *const Snippet,
    strategy: InjectionStrategy,
) -> *mut Injector {
    if snippet.is_null() {
        return core::ptr::null_mut();
    }
    let snippet_content = match SnippetInternal::borrow_public_ptr(snippet).inner {
        Ok(ref content) => content.as_slice(),
        Err(_) => return core::ptr::null_mut(),
    };

    return Box::into_raw(Box::new(Injector {
        inner: injector::Injector::with_strategy(snippet_content, strategy.into()),
        slices_memory_storage: core::array::from_fn(|_| return BytesSlice::empty()),
    }));
}

/// Free the [`Injector`] associated memory.
///
/// # Memory safety
///
/// Do not use the `injector` instance after calling this function. Passing `NULL` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn injector_cleanup(injector: *mut Injector) {
    if !injector.is_null() {
        _ = Box::from_raw(injector);
    }
}

/// Write part of an HTML document to the [`Injector`].
///
/// This function should not be called while another [`InjectorResult`] from the same `Injector` is
/// still being processed.
///
/// # Memory ownership
///
/// The library consumer owns the memory related to the `chunk` argument, and should free it once
/// it's not used anymore.
///
/// See [`InjectorResult`] documentation for details about the returned value memory ownership.
///
/// # Example
///
/// ```c
/// InjectorResult result = injector_write(injector, buffer, buffer_length);
/// for (int index = 0; index < result.slices_length; index += 1) {
///   BytesSlice slice = result.slices[index];
///
///   write(outgoing_stream, slice.start, slice.length);
/// }
/// ```
#[no_mangle]
pub unsafe extern "C" fn injector_write(
    injector: *mut Injector,
    chunk: *const u8,
    length: u32,
) -> InjectorResult {
    if injector.is_null() {
        return InjectorResult::empty();
    }
    let ffi_injector = &mut *injector;
    // A NULL chunk with length==0 is a legitimate "no more data yet" call;
    // treat it as an empty slice rather than constructing a dangling slice.
    let chunk: &[u8] = if chunk.is_null() || length == 0 {
        &[]
    } else {
        slice::from_raw_parts(chunk, length as usize)
    };
    return InjectorResult::new(
        ffi_injector.inner.write(chunk),
        &mut ffi_injector.slices_memory_storage,
    );
}

/// How many bytes of the document written so far the [`Injector`] holds back, not returned in
/// any [`InjectorResult`] yet.
///
/// They are always the end of what was written, and come back out in later results, in slices
/// whose `from_incoming_chunk` is false. A server that queues the chunks it writes can leave them
/// as they are until a result injects, then drop this many bytes (read before that call) from the
/// end of the chunks already queued, and serve the result's slices in place of the chunk just
/// written. Passing `NULL` returns 0.
#[no_mangle]
pub unsafe extern "C" fn injector_held_back_length(injector: *const Injector) -> u32 {
    if injector.is_null() {
        return 0;
    }
    return (*injector).inner.held_back() as u32;
}

/// Computes the `Content-Length` a response has to announce once the snippet is in it, given the
/// `length` bytes at `value`, the `Content-Length` it announced (not NUL-terminated). The snippet is
/// all the injection adds, and the padding [`injector_end`] emits instead is as long, so the length
/// grows by exactly the snippet length either way.
///
/// Returns false, leaving `*extended` untouched, when `value` is not a valid length (a list of
/// identical lengths is): the server should then remove the header rather than announce a wrong
/// length. Passing `NULL` for `injector` or `extended` returns false.
///
/// # Example
///
/// ```c
/// uint64_t extended;
/// if (injector_extend_content_length(injector, value, value_length, &extended)) {
///   // announce `extended`
/// } else {
///   // remove Content-Length
/// }
/// ```
#[no_mangle]
pub unsafe extern "C" fn injector_extend_content_length(
    injector: *const Injector,
    value: *const u8,
    length: u32,
    extended: *mut u64,
) -> bool {
    if injector.is_null() || extended.is_null() {
        return false;
    }
    let value: &[u8] = if value.is_null() || length == 0 {
        &[]
    } else {
        slice::from_raw_parts(value, length as usize)
    };
    return match (*injector).inner.content_length_with_snippet(value) {
        Some(length) => {
            *extended = length;
            true
        }
        None => false,
    };
}

/// Notify the [`Injector`] that the full HTML document has been written.
///
/// If the injection did not happen yet, a slice of the same size as the snippet containing only
/// white space will be returned to "pad" the response, so it has the same length as the announced
/// `content-length`.
///
/// This function should not be called while another [`InjectorResult`] from the same `Injector` is
/// still being processed.
///
/// # Memory ownership
///
/// See [`InjectorResult`] documentation for details about the returned value memory ownership.
///
/// # Example
///
/// ```c
/// InjectorResult result = injector_end(injector);
/// for (int index = 0; index < result.slices_length; index += 1) {
///   BytesSlice slice = result.slices[index];
///
///   write(outgoing_stream, slice.start, slice.length);
/// }
#[no_mangle]
pub unsafe extern "C" fn injector_end(ffi_injector: *mut Injector) -> InjectorResult {
    if ffi_injector.is_null() {
        return InjectorResult::empty();
    }
    let ffi_injector = &mut *ffi_injector;
    return InjectorResult::new(
        ffi_injector.inner.end(),
        &mut ffi_injector.slices_memory_storage,
    );
}

/// Same as [`injector_end`], but never returns padding: when the injection did not happen, only
/// the data still held back by the [`Injector`] is returned.
///
/// Use it when the response length is not announced up front (for example, when the
/// `content-length` header is removed), so padding is not needed.
///
/// This function should not be called while another [`InjectorResult`] from the same `Injector` is
/// still being processed.
///
/// # Memory ownership
///
/// See [`InjectorResult`] documentation for details about the returned value memory ownership.
#[no_mangle]
pub unsafe extern "C" fn injector_end_without_padding(
    ffi_injector: *mut Injector,
) -> InjectorResult {
    if ffi_injector.is_null() {
        return InjectorResult::empty();
    }
    let ffi_injector = &mut *ffi_injector;
    return InjectorResult::new(
        ffi_injector.inner.end_without_padding(),
        &mut ffi_injector.slices_memory_storage,
    );
}

/// The result of a call to [`injector_write`], [`injector_end`] or
/// [`injector_end_without_padding`]. It represents a list of binary slices to be written to the
/// outgoing stream.
///
/// In its current implementation, at most 4 slices can be returned. While rare, it is possible
/// that 0 slices are returned.
///
/// [`InjectorResult`] instances (and the memory they point to) should *not* be used anymore once
/// `injector_write`, `injector_end` or `injector_end_without_padding` are called with the same
/// `Injector` instance.
///
/// # Memory ownership
///
/// `InjectorResult` is allocated on the stack. By itself it does not own any memory, but can
/// contain pointers to:
///
/// * portions of the chunk provided to [`injector_write`]. This chunk is owned by the library
///   consumer and should be freed once it is not used anymore.
///
/// * internal buffers owned by the [`Injector`] instance. Those buffers will be cleared by
///   [`injector_cleanup`].
#[repr(C)]
pub struct InjectorResult {
    /// Number of returned slices (any number from 0 to 4).
    pub slices_length: u32,

    /// A pointer to the first returned slice.
    pub slices: *const BytesSlice,

    /// Whether the injection occurred while processing the current chunk.
    pub injected: bool,
}

impl InjectorResult {
    fn new(result: injector::Result<'static>, slices_memory_storage: &mut [BytesSlice; 4]) -> Self {
        *slices_memory_storage = core::array::from_fn(|index| return result.slices[index].into());
        return Self {
            slices_length: result.length as u32,
            slices: slices_memory_storage as *const _,
            injected: result.injected,
        };
    }

    /// Defensive default returned when a NULL injector is passed to [`injector_write`],
    /// [`injector_end`] or [`injector_end_without_padding`]. No slices, nothing injected.
    fn empty() -> Self {
        return Self {
            slices_length: 0,
            slices: null(),
            injected: false,
        };
    }
}

/// A pointer to a sub-array of bytes, used within [`InjectorResult`].
///
/// # Memory ownership
///
/// See [`InjectorResult`].
#[repr(C)]
pub struct BytesSlice {
    /// Number of bytes.
    pub length: u32,

    /// A pointer to the first byte of the sub-array.
    pub start: *const u8,

    /// Whether the pointer refers to the chunk provided to the [`injector_write`] function. This
    /// is useful when using the library from a language providing safer ways to access memory than
    /// using raw pointers.
    ///
    /// By using this flag, one could avoid some unsafe pointer manipulation by computing the slice
    /// index relative to the start of the chunk, and use this index to access the memory-owned
    /// instance of the chunk.
    ///
    /// # Example (in "simplified" Rust as an illustration)
    ///
    /// ```text
    /// let chunk = b"foobar";
    /// let result = injector_write(injector, chunk.as_ptr(), chunk.len());
    /// for slice in result.slices {
    ///     if slice.from_incoming_chunk {
    ///         let start_index = slice.start - chunk.as_ptr();
    ///         let end_index = start_index + slice.end;
    ///         outgoing_stream.write(&chunk[start_index..end_index])
    ///     } else {
    ///         outgoing_stream.write(unsafe { std::slice::from_raw_parts(slice.start, slice.len) })
    ///     }
    /// }
    /// ```
    pub from_incoming_chunk: bool,
}

impl BytesSlice {
    fn empty() -> Self {
        return Self {
            start: null(),
            length: 0,
            from_incoming_chunk: false,
        };
    }
}

impl<'a> From<injector::BytesSlice<'a>> for BytesSlice {
    fn from(slice: injector::BytesSlice<'a>) -> Self {
        return Self {
            start: slice.bytes as *const _ as *const u8,
            length: slice.bytes.len() as u32,
            from_incoming_chunk: slice.from_incoming_chunk,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet::{snippet_cleanup, snippet_create_from_json};
    use crate::test_utils::cstr_ptr;
    use alloc::string::String;
    use core::ptr::null_mut;
    use pretty_assertions::assert_eq;

    #[test]
    fn injector_create_null_snippet_returns_null() {
        let injector = unsafe { injector_create(null()) };
        assert!(injector.is_null());
    }

    #[test]
    fn injector_create_failed_snippet_returns_null() {
        // A snippet that failed to build (error_code != 0) has no content to
        // inject. Returning NULL forces the host to skip injection instead of
        // silently producing a pass-through injector.
        let snippet = unsafe { snippet_create_from_json(cstr_ptr(b"{not json\0")) };
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_ne!(s.error_code, 0);
        let injector = unsafe { injector_create(snippet) };
        assert!(injector.is_null());
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn injector_write_null_injector_returns_empty() {
        let chunk = b"<html></html>";
        let result = unsafe { injector_write(null_mut(), chunk.as_ptr(), chunk.len() as u32) };
        assert_eq!(result.slices_length, 0);
        assert!(result.slices.is_null());
        assert!(!result.injected);
    }

    #[test]
    fn injector_end_null_injector_returns_empty() {
        let result = unsafe { injector_end(null_mut()) };
        assert_eq!(result.slices_length, 0);
        assert!(result.slices.is_null());
        assert!(!result.injected);
    }

    #[test]
    fn injector_end_without_padding_null_injector_returns_empty() {
        let result = unsafe { injector_end_without_padding(null_mut()) };
        assert_eq!(result.slices_length, 0);
        assert!(result.slices.is_null());
        assert!(!result.injected);
    }

    /// Without padding, the end returns only the data still held back: here the whole document,
    /// shorter than the head window and holding no `<head>`.
    #[test]
    fn injector_end_without_padding_returns_held_data_only() {
        assert_eq!(inject(b"abc</head>", false), "abc</head>");
    }

    #[test]
    fn injector_create_injects_in_head() {
        // Right before `</head>`, by default.
        assert_eq!(
            inject(
                b"<head><meta charset=utf-8><title>t</title></head>def",
                true
            ),
            "<head><meta charset=utf-8><title>t</title><SNIPPET></head>def"
        );
        // As early as possible, on demand.
        assert_eq!(
            inject_with(
                InjectionStrategy::HeadOpenTag,
                b"<head><meta charset=utf-8><title>t</title></head>def",
                true
            ),
            "<head><meta charset=utf-8><SNIPPET><title>t</title></head>def"
        );
        // No `<head>` to inject after: the document is padded to the length it would have had,
        // and left otherwise untouched.
        let html = "abc</head>def";
        let mut expected = String::from(html);
        expected.extend(core::iter::repeat_n(' ', snippet_len()));
        assert_eq!(inject(html.as_bytes(), true), expected);
    }

    /// Length of the snippet built by [`test_snippet`], used to check padding.
    fn snippet_len() -> usize {
        let snippet = test_snippet();
        let len = unsafe { SnippetInternal::borrow_public_ptr(snippet) }
            .inner
            .as_ref()
            .expect("valid snippet")
            .len();
        unsafe { snippet_cleanup(snippet) };
        return len;
    }

    /// Drives a full injection through the C API and returns the concatenated output, with the
    /// snippet itself replaced by `<SNIPPET>` so assertions stay readable. `padded` selects
    /// [`injector_end`] over [`injector_end_without_padding`].
    fn inject(html: &[u8], padded: bool) -> String {
        return inject_with(InjectionStrategy::HeadCloseTag, html, padded);
    }

    /// [`inject`], with the injector [`injector_create_with_strategy`] creates for `strategy`.
    fn inject_with(strategy: InjectionStrategy, html: &[u8], padded: bool) -> String {
        let snippet = test_snippet();
        let injector = unsafe { injector_create_with_strategy(snippet, strategy) };
        assert!(!injector.is_null());

        let snippet_content = unsafe { SnippetInternal::borrow_public_ptr(snippet) }
            .inner
            .as_ref()
            .expect("valid snippet")
            .clone();

        // Each result must be fully consumed before the next call, as documented on
        // `InjectorResult`: consecutive calls reuse the same slice storage.
        let mut output = String::new();
        let mut append = |result: InjectorResult| {
            for index in 0..result.slices_length as usize {
                let slice = unsafe { &*result.slices.add(index) };
                let bytes = unsafe { slice::from_raw_parts(slice.start, slice.length as usize) };
                output.push_str(core::str::from_utf8(bytes).expect("valid UTF-8"));
            }
        };
        append(unsafe { injector_write(injector, html.as_ptr(), html.len() as u32) });
        append(match padded {
            true => unsafe { injector_end(injector) },
            false => unsafe { injector_end_without_padding(injector) },
        });

        unsafe { injector_cleanup(injector) };
        unsafe { snippet_cleanup(snippet) };

        return output.replace(
            core::str::from_utf8(&snippet_content).expect("valid UTF-8"),
            "<SNIPPET>",
        );
    }

    fn test_snippet() -> *mut Snippet {
        return unsafe {
            snippet_create_from_json(cstr_ptr(
                b"{\"majorVersion\":5,\"rum\":{\"applicationId\":\"a\",\"clientToken\":\"t\"}}\0",
            ))
        };
    }

    #[test]
    fn injector_extend_content_length_adds_the_snippet() {
        let snippet = test_snippet();
        let injector = unsafe { injector_create(snippet) };
        let mut extended = 0u64;
        let value = b"1000";
        assert!(unsafe {
            injector_extend_content_length(
                injector,
                value.as_ptr(),
                value.len() as u32,
                &mut extended,
            )
        });
        assert_eq!(extended, 1000 + snippet_len() as u64);

        let mut untouched = 7u64;
        let value = b"12x";
        assert!(!unsafe {
            injector_extend_content_length(
                injector,
                value.as_ptr(),
                value.len() as u32,
                &mut untouched,
            )
        });
        assert_eq!(untouched, 7);
        assert!(!unsafe { injector_extend_content_length(injector, null(), 0, &mut untouched) });
        assert!(!unsafe {
            injector_extend_content_length(null(), value.as_ptr(), 3, &mut untouched)
        });
        assert!(!unsafe {
            injector_extend_content_length(injector, value.as_ptr(), 3, core::ptr::null_mut())
        });

        unsafe { injector_cleanup(injector) };
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn injector_held_back_length_null_injector_returns_zero() {
        assert_eq!(unsafe { injector_held_back_length(null()) }, 0);
    }

    /// The head window is held back until it fills, then comes back out in slices that are not
    /// flagged as coming from the chunk just written, ahead of the snippet.
    #[test]
    fn injector_held_back_length_reports_the_head_window() {
        let snippet = test_snippet();
        let injector =
            unsafe { injector_create_with_strategy(snippet, InjectionStrategy::HeadOpenTag) };
        assert!(!injector.is_null());

        let head = b"<html><head>";
        let result = unsafe { injector_write(injector, head.as_ptr(), head.len() as u32) };
        assert_eq!(result.slices_length, 0);
        assert_eq!(
            unsafe { injector_held_back_length(injector) },
            head.len() as u32
        );

        let rest = [b'x'; 2048];
        let result = unsafe { injector_write(injector, rest.as_ptr(), rest.len() as u32) };
        assert!(result.injected);
        let slices =
            unsafe { core::slice::from_raw_parts(result.slices, result.slices_length as usize) };
        assert!(
            !slices[0].from_incoming_chunk,
            "the held-back window is not part of this chunk"
        );
        assert_eq!(
            unsafe { core::slice::from_raw_parts(slices[0].start, slices[0].length as usize) },
            head
        );
        assert_eq!(unsafe { injector_held_back_length(injector) }, 0);

        unsafe { injector_cleanup(injector) };
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn injector_write_null_chunk_is_treated_as_empty() {
        let snippet = unsafe {
            snippet_create_from_json(cstr_ptr(
                b"{\"majorVersion\":5,\"rum\":{\"applicationId\":\"a\",\"clientToken\":\"t\"}}\0",
            ))
        };
        let injector = unsafe { injector_create(snippet) };
        assert!(!injector.is_null());
        let result = unsafe { injector_write(injector, null(), 0) };
        assert_eq!(result.slices_length, 0);
        unsafe { injector_cleanup(injector) };
        unsafe { snippet_cleanup(snippet) };
    }
}
