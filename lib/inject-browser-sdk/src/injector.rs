// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;

use crate::injection_point_locator::{HeadCloseTagLocator, InjectionPointLocator, Location};

/// Number of bytes at the start of the document within which a character encoding declaration has
/// to be serialized to be honoured by browsers, as per the HTML spec
/// (<https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>).
///
/// We hold back at most this many bytes while looking for the earliest valid injection point:
/// injecting the snippet *before* a `<meta charset>` would push that declaration out of the window
/// and change how the browser decodes the document. Conversely, a declaration that is already
/// outside of the window is ignored by browsers anyway, so there is no point looking further.
const DOCUMENT_CHARSET_SCAN_LIMIT: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
enum InjectorState {
    /// Holding back the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, looking for the earliest valid
    /// injection point within the `<head>` tag.
    BufferingHeadWindow,

    /// The head window held no valid injection point, so we fell back to looking for the `</head>`
    /// tag and stream-scan the rest of the document for it.
    ScanningForHeadCloseTag,

    /// Successfully injected the snippet at a valid injection point.
    SnippetInjected,

    /// Reached the end of the document without finding any injection point.
    NoValidInjectionPoint,
}

/// Stateful object used to inject the provided snippet within a streamed HTML document.
///
/// Injection points are looked for in two successive passes:
/// 1. the earliest valid position within the `<head>` tag, as long as it is within the first
///    DOCUMENT_CHARSET_SCAN_LIMIT bytes. This is the preferred outcome: the earlier the snippet
///    runs, the more of the page lifecycle RUM can observe. It requires holding data back, since we
///    cannot know whether a `<meta charset>` is still coming.
/// 2. failing that, right before the `</head>` tag, wherever it is in the document. Injecting there
///    can never displace a character encoding declaration, so it needs no buffering beyond a
///    partially matched tag.
pub struct Injector<'snippet> {
    /// Snippet to inject.
    snippet: &'snippet [u8],

    /// Tracks the current state of the injection process.
    injector_state: InjectorState,

    /// Locator for the second pass, see [`InjectorState::ScanningForHeadCloseTag`].
    head_close_locator: HeadCloseTagLocator,

    /// Data that cannot be emitted yet.
    ///
    /// While buffering the head window, this accumulates the incoming chunks until we have enough
    /// data to decide where to inject. During the `</head>` pass, it holds a partially matched tag
    /// that may turn out to be an injection point once more data arrives.
    ///
    /// There are two buffers here, because in some cases we need to return previously held data
    /// *and* hold part of the current chunk back. Once emitted, the buffer needs to be emptied, so
    /// we don't emit the same data twice. To keep things simple, as a convention, we only write in
    /// `incomplete_data` and move it to the exposed `incomplete_data_public_ref` buffer when needed.
    ///
    /// Example: imagine we write the following chunks: `foo</he`, `ad>bar` (past the head window).
    /// * When writing the first chunk, the injector will return `foo`, but hold `</he` back, as the
    ///   snippet might be injected before it.
    /// * When writing the second chunk, the injector will realize that `</he` was indeed the
    ///   beginning of the `</head>` tag, so it will emit the snippet, then the held data `</he`,
    ///   then the chunk `ad>bar`.
    ///
    /// Second example: imagine we write the following chunks: `foo</he`, `ader>bar`.
    /// * Same as before, the injector will return `foo` and hold `</he` back
    /// * When writing the second chunk, the injector will realize that `</he` was not the start of
    ///   a `</head>` tag, so it will emit the held data `</he`, then the chunk `ader>bar`.
    ///
    /// Third example: imagine we write the following chunks: `foo</he`, `ader>bar</`
    /// * Same as before, the injector will return `foo` and hold `</he` back
    /// * When writing the second chunk, the injector will realize that `</he` was not the start of
    ///   a `</head>` tag, so it will emit it, then emit `ader>bar`. But it will need to put `</` in
    ///   a second buffer, as it cannot tell yet if it is a valid injection point.
    incomplete_data: Vec<u8>,
    incomplete_data_public_ref: Vec<u8>,

    /// If we don't find any injection point, we need to pad the document with spaces, so the
    /// document has the same size as if the snippet was successfully injected.
    padding_spaces: Vec<u8>,
}

impl<'snippet> Injector<'snippet> {
    pub fn new(snippet: &'snippet [u8]) -> Self {
        return Self {
            snippet,
            injector_state: InjectorState::BufferingHeadWindow,
            head_close_locator: HeadCloseTagLocator::new(),
            incomplete_data: Vec::new(),
            incomplete_data_public_ref: Vec::new(),
            padding_spaces: Vec::new(),
        };
    }

    pub fn write<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        match self.injector_state {
            InjectorState::BufferingHeadWindow => {
                if self.incomplete_data.is_empty() && chunk.len() >= DOCUMENT_CHARSET_SCAN_LIMIT {
                    // Optimization: the chunk already covers the whole head window, so scan it
                    // directly instead of copying it into the buffer first.
                    return self.scan_head_window(Some(chunk), false);
                }

                self.incomplete_data.extend_from_slice(chunk);

                if self.incomplete_data.len() >= DOCUMENT_CHARSET_SCAN_LIMIT {
                    return self.scan_head_window(None, false);
                }

                return self.create_result(&[]);
            }

            InjectorState::ScanningForHeadCloseTag => return self.scan_head_close_tag(chunk),

            InjectorState::SnippetInjected | InjectorState::NoValidInjectionPoint => {
                return self.create_result(&[BytesSlice::new(chunk, true)]);
            }
        }
    }

    pub fn end(&mut self) -> Result<'_> {
        match self.injector_state {
            InjectorState::BufferingHeadWindow => return self.scan_head_window(None, true),

            InjectorState::ScanningForHeadCloseTag => {
                self.injector_state = InjectorState::NoValidInjectionPoint;
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                self.padding_spaces = vec![b' '; self.snippet.len()];
                return self.create_result(&[
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(&self.padding_spaces, false),
                ]);
            }

            InjectorState::SnippetInjected => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                return self
                    .create_result(&[BytesSlice::new(&self.incomplete_data_public_ref, false)]);
            }

            // Already ended: everything has been emitted and padded for, so emit nothing rather
            // than padding twice.
            InjectorState::NoValidInjectionPoint => return self.create_result(&[]),
        }
    }

    /// Looks for the earliest valid injection point within the head window. If there is none, falls
    /// back to the `</head>` tag and scans the very same data for it.
    fn scan_head_window<'a>(
        &'a mut self,
        incoming_chunk: Option<&'a [u8]>,
        eof: bool,
    ) -> Result<'a> {
        let from_incoming_chunk = incoming_chunk.is_some();
        if incoming_chunk.is_none() {
            // Hand the held data out through `incomplete_data_public_ref`, so the `</head>` pass
            // can start refilling `incomplete_data`.
            self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
        }
        let chunk = match incoming_chunk {
            Some(chunk) => chunk,
            None => &self.incomplete_data_public_ref,
        };

        let window = &chunk[..chunk.len().min(DOCUMENT_CHARSET_SCAN_LIMIT)];
        if let Some(point) = InjectionPointLocator::find(window) {
            self.injector_state = InjectorState::SnippetInjected;
            return self.create_result(&[
                BytesSlice::new(&chunk[..point], from_incoming_chunk),
                BytesSlice::new(self.snippet, false),
                BytesSlice::new(&chunk[point..], from_incoming_chunk),
            ]);
        }

        self.injector_state = InjectorState::ScanningForHeadCloseTag;

        match self.head_close_locator.scan(chunk) {
            Location::MatchFromIndex(index) => {
                self.injector_state = InjectorState::SnippetInjected;
                return self.create_result(&[
                    BytesSlice::new(&chunk[..index], from_incoming_chunk),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&chunk[index..], from_incoming_chunk),
                ]);
            }

            // A partial `</head>` at the very end of the data: hold it back, unless the document
            // ends here, in which case it was never a real tag.
            Location::PotentialFromIndex(index) if !eof => {
                self.incomplete_data.extend_from_slice(&chunk[index..]);
                return self
                    .create_result(&[BytesSlice::new(&chunk[..index], from_incoming_chunk)]);
            }

            // `Location::None`, or an `eof` partial match. The `*FromPreviousChunk` variants cannot
            // happen here: this is the first data the `</head>` locator sees.
            _ => {}
        }

        if eof {
            self.injector_state = InjectorState::NoValidInjectionPoint;
            self.padding_spaces = vec![b' '; self.snippet.len()];
            return self.create_result(&[
                BytesSlice::new(chunk, from_incoming_chunk),
                BytesSlice::new(&self.padding_spaces, false),
            ]);
        }

        return self.create_result(&[BytesSlice::new(chunk, from_incoming_chunk)]);
    }

    /// Second pass: stream-scan for the `</head>` tag, holding back only partially matched tags.
    fn scan_head_close_tag<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        match self.head_close_locator.scan(chunk) {
            Location::None => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                return self.create_result(&[
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(chunk, true),
                ]);
            }

            Location::PotentialFromPreviousChunk => {
                self.incomplete_data.extend_from_slice(chunk);
                return self.create_result(&[]);
            }

            Location::PotentialFromIndex(index) => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                self.incomplete_data.extend_from_slice(&chunk[index..]);
                return self.create_result(&[
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(&chunk[..index], true),
                ]);
            }

            Location::MatchFromPreviousChunk => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                self.injector_state = InjectorState::SnippetInjected;
                return self.create_result(&[
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(chunk, true),
                ]);
            }

            Location::MatchFromIndex(index) => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                self.injector_state = InjectorState::SnippetInjected;
                return self.create_result(&[
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(&chunk[..index], true),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&chunk[index..], true),
                ]);
            }
        }
    }

    fn create_result<'a>(&'a self, slices: &[BytesSlice<'a>]) -> Result<'a> {
        let mut result_slices = [BytesSlice::default(); 4];
        let mut length = 0;

        for slice in slices.iter().filter(|s| return !s.bytes.is_empty()).take(4) {
            result_slices[length] = *slice;
            length += 1;
        }

        return Result {
            slices: result_slices,
            length,
            injected: self.injector_state == InjectorState::SnippetInjected,
        };
    }
}

#[derive(Default, Copy, Clone)]
pub struct BytesSlice<'a> {
    pub bytes: &'a [u8],
    pub from_incoming_chunk: bool,
}

impl<'a> BytesSlice<'a> {
    pub fn new(bytes: &'a [u8], from_incoming_chunk: bool) -> Self {
        return Self {
            bytes,
            from_incoming_chunk,
        };
    }
}

/// Stack allocated "array" of at most 4 references to byte slices.
pub struct Result<'a> {
    pub slices: [BytesSlice<'a>; 4],
    pub length: usize,
    pub injected: bool,
}

impl<'a> Result<'a> {
    #[cfg(test)]
    fn iter(&self) -> impl Iterator<Item = BytesSlice<'_>> {
        return self.slices[..self.length].iter().copied();
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use pretty_assertions::assert_eq;
    use rand::distr::Uniform;
    use rand::prelude::IndexedRandom;
    use rand::RngExt;

    use super::*;

    #[test]
    fn basic() {
        test_injector(
            ["<head", ">abc</he", "ad>def"],
            "<head>{{SNIPPET}}abc</head>def",
        );
    }

    #[test]
    fn ignore_head_after_1_kb() {
        let input = "A".repeat(1024) + "<head>def";
        let expected = input.clone() + "           ";

        test_injector([input], &expected);
    }

    #[test]
    fn meta_after_1_kb_fallback_to_head() {
        let input = format!("<head>{}<meta charset='...'></head>def", "a".repeat(1024));
        let expected = format!(
            "<head>{}{}<meta charset='...'></head>def",
            "{{SNIPPET}}",
            "a".repeat(1024)
        );

        test_injector([input], &expected);
    }

    #[test]
    fn padding_at_eof_if_no_injection_point() {
        let input = ["a".repeat(1024), String::from("bb")];
        let expected = input.join("") + "           ";
        test_injector(input, &expected);
    }

    #[test]
    fn no_head_or_meta() {
        test_injector(["<html></html>"], "<html></html>           ");
    }

    #[test]
    fn head_close_tag_without_open_head() {
        // No open `<head>` to inject after, so fall back to injecting before `</head>`.
        test_injector(["abc</head>def"], "abc{{SNIPPET}}</head>def");
    }

    #[test]
    fn head_after_1_kb_falls_back_to_head_close_tag() {
        // The open `<head>` is past the charset window so it cannot be used, but `</head>` can be:
        // injecting there can never displace a character encoding declaration.
        let input = "A".repeat(1024) + "<head>def</head>ghi";
        let expected = "A".repeat(1024) + "<head>def{{SNIPPET}}</head>ghi";

        test_injector([input], &expected);
    }

    #[test]
    fn head_close_tag_straddling_chunks_after_the_window() {
        let expected = "A".repeat(1024) + "abc{{SNIPPET}}</head>def";

        test_injector(
            [
                "A".repeat(1024),
                String::from("abc</he"),
                String::from("ad>def"),
            ],
            &expected,
        );
    }

    #[test]
    fn head_close_tag_split_one_byte_at_a_time() {
        let input = "A".repeat(1024) + "abc</head>def";
        let expected = "A".repeat(1024) + "abc{{SNIPPET}}</head>def";

        // One chunk to get past the window, then a chunk per remaining byte.
        let mut chunks = alloc::vec!["A".repeat(1024)];
        chunks.extend(input[1024..].chars().map(String::from));

        test_injector(chunks, &expected);
    }

    #[test]
    fn only_injects_once_across_passes() {
        // A `</head>` following an already-used injection point must not inject a second time.
        test_injector(
            ["<head>abc</head>def</head>ghi"],
            "<head>{{SNIPPET}}abc</head>def</head>ghi",
        );
    }

    #[test]
    fn empty() {
        test_injector::<&str>([], "           ");
        test_injector([""], "           ");
        test_injector(["", ""], "           ");
    }

    #[test]
    fn fuzzy() {
        struct Part {
            string: &'static str,
            has_head: bool,
        }

        impl Part {
            fn new(string: &'static str, has_head: bool) -> Self {
                return Self { string, has_head };
            }
        }

        let parts: [Part; 16] = [
            Part::new("<head>", true),
            Part::new("<head >", true),
            Part::new("<HeAd>", true),
            Part::new("<HEAD>", true),
            Part::new("<h ead>", false),
            Part::new("<he ad>", false),
            Part::new("<header>", false),
            Part::new("h>", false),
            Part::new("/>", false),
            Part::new(">", false),
            Part::new(" ", false),
            Part::new("foo", false),
            Part::new("bar", false),
            Part::new("&nbsp;", false),
            Part::new("😊", false),
            Part::new("网络", false),
        ];

        let mut rng = rand::rng();

        for _ in 0..1000 {
            // Build a random input from 'parts'
            let mut input = String::new();
            let mut expected = String::new();
            let mut has_head = false;

            let parts_count = rng.random_range(0usize..20);
            for part in parts.choose_multiple(&mut rng, parts_count) {
                input.push_str(part.string);
                expected.push_str(part.string);

                if !has_head && part.has_head {
                    expected.push_str("{{SNIPPET}}");
                    has_head = true;
                }
            }
            if !has_head {
                expected.push_str("           ");
            }

            // Split `input` into chunks of random sizes. Because chunks might not end at a
            // character boundary, we need to use &[u8] chunks instead of &str.
            let chunks = rng
                .clone()
                .sample_iter(Uniform::new(0usize, input.len() + 1).unwrap())
                .scan(input.as_bytes(), |input, chunk_size| {
                    if input.is_empty() {
                        return None;
                    } else {
                        let chunk_size = chunk_size.min(input.len());
                        let chunk = &input[..chunk_size];
                        *input = &input[chunk_size..];
                        return Some(chunk);
                    }
                });

            test_injector(chunks, expected.as_str());
        }
    }

    /// Same as [`fuzzy`], but behind a filler prefix long enough to exhaust the charset window, so
    /// the `</head>` fallback pass is the one under test.
    #[test]
    fn fuzzy_head_close_tag_fallback() {
        struct Part {
            string: &'static str,
            has_head_close: bool,
        }

        impl Part {
            fn new(string: &'static str, has_head_close: bool) -> Self {
                return Self {
                    string,
                    has_head_close,
                };
            }
        }

        // None of these parts can combine into a `</head>` across a boundary, so a match can only
        // ever come from a single `has_head_close` part.
        let parts: [Part; 16] = [
            Part::new("</head>", true),
            Part::new("</ head>", true),
            Part::new("</HeAd>", true),
            Part::new("</HEAD>", true),
            Part::new("</h ead>", false),
            Part::new("</he ad>", false),
            Part::new("</header>", false),
            Part::new("</h", false),
            Part::new("</", false),
            Part::new("<", false),
            Part::new(" ", false),
            Part::new("foo", false),
            Part::new("bar", false),
            Part::new("&nbsp;", false),
            Part::new("😊", false),
            Part::new("网络", false),
        ];

        // Contains no injection point of its own, and is long enough that the `<head>` pass gives up.
        let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
        let mut rng = rand::rng();

        for _ in 0..1000 {
            let mut input = filler.clone();
            let mut expected = filler.clone();
            let mut has_head_close = false;

            let parts_count = rng.random_range(0usize..20);
            for part in parts.choose_multiple(&mut rng, parts_count) {
                input.push_str(part.string);

                // The snippet goes *before* the closing tag, unlike the `<head>` pass.
                if !has_head_close && part.has_head_close {
                    expected.push_str("{{SNIPPET}}");
                    has_head_close = true;
                }

                expected.push_str(part.string);
            }
            if !has_head_close {
                expected.push_str("           ");
            }

            // Split `input` into chunks of random sizes. Because chunks might not end at a
            // character boundary, we need to use &[u8] chunks instead of &str.
            let chunks = rng
                .clone()
                .sample_iter(Uniform::new(0usize, input.len() + 1).unwrap())
                .scan(input.as_bytes(), |input, chunk_size| {
                    if input.is_empty() {
                        return None;
                    } else {
                        let chunk_size = chunk_size.min(input.len());
                        let chunk = &input[..chunk_size];
                        *input = &input[chunk_size..];
                        return Some(chunk);
                    }
                });

            test_injector(chunks, expected.as_str());
        }
    }

    fn test_injector<T: AsRef<[u8]> + core::fmt::Debug>(
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        let snippet = b"{{SNIPPET}}";
        let mut injector = Injector::new(snippet);

        let input_chunks: Vec<T> = input_chunks.into_iter().collect();

        let mut output_slices = Vec::new();
        let mut injected = false;

        for incoming_chunk in input_chunks
            .iter()
            .map(|chunk| return Some(chunk.as_ref()))
            .chain([None])
        {
            let result = match incoming_chunk {
                Some(chunk) => injector.write(chunk),
                None => injector.end(),
            };
            for slice in result.iter() {
                {
                    // Make sure the slice has a correct from_incoming_chunk flag
                    let expected_from_incoming_chunk_value = match incoming_chunk {
                        Some(chunk) => {
                            let pointer = slice.bytes.as_ptr() as usize;
                            let min = chunk.as_ptr() as usize;
                            let max = min + chunk.len();
                            min <= pointer && pointer < max
                        }
                        None => false,
                    };

                    assert_eq!(
                        slice.from_incoming_chunk,
                        expected_from_incoming_chunk_value
                    );
                }

                if !injected {
                    // Look for the snippet in the slice
                    injected = slice
                        .bytes
                        .windows(slice.bytes.len())
                        .any(|w| return w == snippet);
                }
                output_slices.extend_from_slice(slice.bytes);
            }
            assert_eq!(result.injected, injected);
        }

        assert_eq!(
            String::from_utf8(output_slices).unwrap(),
            expected,
            "with chunks {:?}",
            input_chunks
        );
    }
}
