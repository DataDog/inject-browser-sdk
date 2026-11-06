#![allow(clippy::missing_safety_doc)]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! The C API of [`scanner::Scanner`]: where the snippet goes, as an offset into the body, for
//! servers that keep their own buffers. See [`scanner_create`].

use alloc::boxed::Box;
use core::slice;

use crate::{Snippet, SnippetInternal};
use inject_browser_sdk::{injector, scanner};

/// Decides where a Browser SDK snippet goes in an HTML document, by the rules [`injector_create`]
/// documents, and tells it as an offset into the body rather than handing bytes back. Use
/// [`scanner_create`] to create one.
///
/// Note: this struct is exposed as an FFI opaque pointer. Structure layout is irrelevant to the
/// library consumer as it is only allocated and accessed by the library.
///
/// [`injector_create`]: crate::injector_create
pub struct Scanner {
    inner: scanner::Scanner,
}

/// The kind of a [`ScannerDecision`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScannerDecisionKind {
    /// Undecided: the bytes from `offset` on must be held, those before it may be sent. `offset`
    /// never moves backwards, and the snippet never goes before it.
    Hold = 0,
    /// Undecided, but nothing has to be held: the snippet would go in data yet to be fed.
    Searching = 1,
    /// The snippet goes at `offset`. Final.
    InjectAt = 2,
    /// The snippet never goes in. Final.
    Never = 3,
}

/// What a [`Scanner`] says of the body fed to it so far. Offsets count from the first body byte;
/// `offset` is 0 for the kinds that carry none.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerDecision {
    pub kind: ScannerDecisionKind,
    pub offset: u64,
}

impl From<scanner::Decision> for ScannerDecision {
    fn from(decision: scanner::Decision) -> Self {
        let (kind, offset) = match decision {
            scanner::Decision::Hold { from } => (ScannerDecisionKind::Hold, from),
            scanner::Decision::Searching => (ScannerDecisionKind::Searching, 0),
            scanner::Decision::InjectAt(offset) => (ScannerDecisionKind::InjectAt, offset),
            scanner::Decision::Never => (ScannerDecisionKind::Never, 0),
        };
        return Self { kind, offset };
    }
}

impl ScannerDecision {
    /// What a `NULL` scanner decides: nothing goes in.
    const NEVER: Self = Self {
        kind: ScannerDecisionKind::Never,
        offset: 0,
    };
}

/// Create a [`Scanner`].
///
/// A scanner never takes ownership of the body: it is fed each slice as it comes, with
/// [`scanner_feed`], and answers with a [`ScannerDecision`] on what the server may do with what it
/// holds. While it says `Hold`, the server keeps the bytes from `offset` on, and may hold
/// its headers as well, so that the `Content-Length` it announces is exact: the snippet length more
/// if the decision ends up `InjectAt`, unchanged if `Never`. By default it decides right before
/// `</head>`, and holds nothing but a `</head` tag a slice cuts short; with
/// [`InjectionStrategy::HeadOpenTag`](crate::InjectionStrategy::HeadOpenTag) it holds the first
/// 1 KiB, at most 10 KiB. It copies only what it holds.
///
/// A server that cannot wait, past a size it allows or on an upstream flush, calls
/// [`scanner_flush`], sends everything, and announces the snippet length more up front, padding
/// the body with spaces if the snippet never goes in (see [`scanner_extend_content_length`]).
///
/// # Memory ownership
///
/// The `Scanner` instance returned by this function is owned by the library and should be freed
/// by using [`scanner_cleanup`] once done using it.
///
/// # Example
///
/// ```cpp
/// Scanner* scanner = scanner_create();
/// const ScannerDecision decision = scanner_feed(scanner, buffer, buffer_length);
/// switch (decision.kind) {
///   case ScannerDecisionKind::Hold:      // keep buffers from decision.offset on, wait for more
///   case ScannerDecisionKind::Searching: // send everything, feed the next buffers
///   case ScannerDecisionKind::InjectAt:  // send up to decision.offset, the snippet, then the rest
///   case ScannerDecisionKind::Never:     // send everything, stop feeding
/// }
/// scanner_cleanup(scanner);
/// ```
#[no_mangle]
pub extern "C" fn scanner_create() -> *mut Scanner {
    return scanner_create_with_strategy(crate::InjectionStrategy::HeadCloseTag);
}

/// Create a [`Scanner`] like [`scanner_create`], deciding where `strategy` says (see
/// [`injector_create_with_strategy`](crate::injector_create_with_strategy)). Same memory ownership.
#[no_mangle]
pub extern "C" fn scanner_create_with_strategy(strategy: crate::InjectionStrategy) -> *mut Scanner {
    return Box::into_raw(Box::new(Scanner {
        inner: scanner::Scanner::with_strategy(strategy.into()),
    }));
}

/// Free the [`Scanner`] associated memory.
///
/// # Memory safety
///
/// Do not use the `scanner` instance after calling this function. Passing `NULL` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn scanner_cleanup(scanner: *mut Scanner) {
    if !scanner.is_null() {
        _ = Box::from_raw(scanner);
    }
}

/// Feed the next `length` bytes of the body at `data` to the [`Scanner`], and return its decision
/// on what was fed so far. A final decision is returned again by every later call.
///
/// The library does not keep `data` past the call. Passing `NULL` for `scanner` returns
/// `Never`; a `NULL` `data` with a `length` of 0 is an empty slice.
#[no_mangle]
pub unsafe extern "C" fn scanner_feed(
    scanner: *mut Scanner,
    data: *const u8,
    length: usize,
) -> ScannerDecision {
    if scanner.is_null() {
        return ScannerDecision::NEVER;
    }
    let data: &[u8] = if data.is_null() || length == 0 {
        &[]
    } else {
        slice::from_raw_parts(data, length)
    };
    return (*scanner).inner.feed(data).into();
}

/// End the body: the decision returned is final, `InjectAt` or `Never`. Passing
/// `NULL` returns `Never`.
#[no_mangle]
pub unsafe extern "C" fn scanner_finish(scanner: *mut Scanner) -> ScannerDecision {
    if scanner.is_null() {
        return ScannerDecision::NEVER;
    }
    return (*scanner).inner.finish().into();
}

/// Give up what the [`Scanner`] has yet to decide on, for a server that has to send what it holds
/// now. The decision returned is never `Hold`, and never injects within what was fed:
/// everything fed so far may be sent. A `<head>` tag starting past the first 1024 bytes may still be
/// injected after (`Searching`). Passing `NULL` returns `Never`.
#[no_mangle]
pub unsafe extern "C" fn scanner_flush(scanner: *mut Scanner) -> ScannerDecision {
    if scanner.is_null() {
        return ScannerDecision::NEVER;
    }
    return (*scanner).inner.flush().into();
}

/// The length of the snippet, what an injection adds to the body. Passing `NULL`, or a snippet
/// that failed to build, returns 0.
#[no_mangle]
pub unsafe extern "C" fn snippet_length(snippet: *const Snippet) -> usize {
    if snippet.is_null() {
        return 0;
    }
    return match SnippetInternal::borrow_public_ptr(snippet).inner {
        Ok(ref content) => content.len(),
        Err(_) => 0,
    };
}

/// Computes the `Content-Length` a response has to announce given `decision`, and the `length`
/// bytes at `value`, the `Content-Length` it announced (not NUL-terminated):
/// * `InjectAt`: the snippet length more;
/// * `Never`: unchanged;
/// * undecided, `Hold` or `Searching`: the snippet length more, for a server that
///   sends its headers before the decision; it then pads the body with as many spaces should the
///   snippet never go in.
///
/// Returns false, leaving `*extended` untouched, when `value` is not a valid length (a list of
/// identical lengths is): the server should then remove the header rather than announce a wrong
/// length. Passing `NULL` for `snippet` or `extended` returns false.
#[no_mangle]
pub unsafe extern "C" fn scanner_extend_content_length(
    decision: ScannerDecision,
    snippet: *const Snippet,
    value: *const u8,
    length: u32,
    extended: *mut u64,
) -> bool {
    if snippet.is_null() || extended.is_null() {
        return false;
    }
    let added = match decision.kind {
        ScannerDecisionKind::Never => 0,
        _ => snippet_length(snippet) as u64,
    };
    let value: &[u8] = if value.is_null() || length == 0 {
        &[]
    } else {
        slice::from_raw_parts(value, length as usize)
    };
    return match injector::content_length_plus(value, added) {
        Some(length) => {
            *extended = length;
            true
        }
        None => false,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet::{snippet_cleanup, snippet_create_from_json};
    use crate::test_utils::cstr_ptr;
    use core::ptr::{null, null_mut};
    use pretty_assertions::assert_eq;

    const JSON: &[u8] =
        b"{\"majorVersion\":6,\"rum\":{\"applicationId\":\"a\",\"clientToken\":\"t\"}}\0";

    fn decision(kind: ScannerDecisionKind, offset: u64) -> ScannerDecision {
        return ScannerDecision { kind, offset };
    }

    #[test]
    fn null_scanner() {
        let data = b"<head>";
        let never = decision(ScannerDecisionKind::Never, 0);
        unsafe {
            assert_eq!(scanner_feed(null_mut(), data.as_ptr(), data.len()), never);
            assert_eq!(scanner_finish(null_mut()), never);
            assert_eq!(scanner_flush(null_mut()), never);
            scanner_cleanup(null_mut());
        }
    }

    #[test]
    fn decides_as_an_offset() {
        unsafe {
            // By default, right before `</head>`, holding nothing but a tag cut short.
            let scanner = scanner_create();
            let head = b"<html><head><title>t</title>";
            assert_eq!(
                scanner_feed(scanner, head.as_ptr(), head.len()),
                decision(ScannerDecisionKind::Searching, 0)
            );
            let rest = b"</head><body>";
            assert_eq!(
                scanner_feed(scanner, rest.as_ptr(), rest.len()),
                decision(ScannerDecisionKind::InjectAt, head.len() as u64)
            );
            scanner_cleanup(scanner);

            // As early as possible: the head window is held from the first byte.
            let scanner = scanner_create_with_strategy(crate::InjectionStrategy::HeadOpenTag);
            let head = b"<html><head><meta charset=utf-8>";
            assert_eq!(
                scanner_feed(scanner, head.as_ptr(), head.len()),
                decision(ScannerDecisionKind::Hold, 0)
            );
            // A NULL slice of no bytes is an empty one.
            assert_eq!(
                scanner_feed(scanner, null(), 0),
                decision(ScannerDecisionKind::Hold, 0)
            );
            let rest = b"<title>t</title></head>";
            assert_eq!(
                scanner_feed(scanner, rest.as_ptr(), rest.len()),
                decision(ScannerDecisionKind::Hold, 0)
            );
            let at = decision(ScannerDecisionKind::InjectAt, head.len() as u64);
            assert_eq!(scanner_finish(scanner), at);
            // Final.
            assert_eq!(scanner_feed(scanner, head.as_ptr(), head.len()), at);
            scanner_cleanup(scanner);
        }
    }

    #[test]
    fn flush_and_never() {
        unsafe {
            let scanner = scanner_create_with_strategy(crate::InjectionStrategy::HeadOpenTag);
            let data = b"<html><head>";
            scanner_feed(scanner, data.as_ptr(), data.len());
            assert_eq!(
                scanner_flush(scanner),
                decision(ScannerDecisionKind::Never, 0)
            );
            scanner_cleanup(scanner);

            let scanner = scanner_create();
            let data = [b'x'; 2000];
            assert_eq!(
                scanner_feed(scanner, data.as_ptr(), data.len()),
                decision(ScannerDecisionKind::Never, 0)
            );
            scanner_cleanup(scanner);
        }
    }

    #[test]
    fn snippet_length_and_content_length() {
        unsafe {
            assert_eq!(snippet_length(null()), 0);
            let failed = snippet_create_from_json(cstr_ptr(b"{not json\0"));
            assert_eq!(snippet_length(failed), 0);
            snippet_cleanup(failed);

            let snippet = snippet_create_from_json(cstr_ptr(JSON));
            let length = snippet_length(snippet) as u64;
            assert_eq!(length, (*snippet).length as u64);
            assert!(length > 0);

            let mut extended = 0u64;
            let value = b"100";
            for (kind, expected) in [
                (ScannerDecisionKind::InjectAt, 100 + length),
                (ScannerDecisionKind::Hold, 100 + length),
                (ScannerDecisionKind::Searching, 100 + length),
                (ScannerDecisionKind::Never, 100),
            ] {
                assert!(scanner_extend_content_length(
                    decision(kind, 0),
                    snippet,
                    value.as_ptr(),
                    value.len() as u32,
                    &mut extended
                ));
                assert_eq!(extended, expected, "{kind:?}");
            }
            extended = 7;
            let invalid = b"100, 101";
            assert!(!scanner_extend_content_length(
                decision(ScannerDecisionKind::InjectAt, 0),
                snippet,
                invalid.as_ptr(),
                invalid.len() as u32,
                &mut extended
            ));
            assert_eq!(extended, 7);
            assert!(!scanner_extend_content_length(
                decision(ScannerDecisionKind::InjectAt, 0),
                null(),
                value.as_ptr(),
                value.len() as u32,
                &mut extended
            ));
            assert!(!scanner_extend_content_length(
                decision(ScannerDecisionKind::InjectAt, 0),
                snippet,
                value.as_ptr(),
                value.len() as u32,
                null_mut()
            ));
            snippet_cleanup(snippet);
        }
    }
}
