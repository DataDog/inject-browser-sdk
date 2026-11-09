// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;

mod encoding;
mod head_close_tag;
mod parser;

use head_close_tag::{HeadCloseTagLocator, Location};
use parser::InjectionPointLocator;

/// Window at the start of the document within which a character encoding declaration is honoured by
/// browsers, as per the HTML spec
/// (<https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>).
///
/// Injecting before a `<meta charset>` could push it out of this window and change how the document
/// is decoded, so the earliest valid point is looked for within these bytes and no further.
const DOCUMENT_CHARSET_SCAN_LIMIT: usize = 1024;

/// Where the snippet should be injected within the HTML document.
///
/// The earlier the snippet runs, the more of the page lifecycle RUM observes, but injecting early
/// means proving that no character encoding declaration follows, which takes buffering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InjectionStrategy {
    /// Inject right before the `</head>` tag, the default and what [`Injector::new`] selects.
    ///
    /// Holds back no more than a partially matched tag, at the cost of the snippet running as late
    /// as possible within the head.
    #[default]
    HeadCloseTag,

    /// Inject at the earliest valid position within the `<head>` tag, and not at all if there is
    /// none within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes.
    ///
    /// Holds that many bytes back, delaying the start of the response body by as much.
    HeadOpenTag,

    /// Like [`InjectionStrategy::HeadOpenTag`], but falls back to
    /// [`InjectionStrategy::HeadCloseTag`] rather than giving up, which instruments documents whose
    /// head does not start within the window. Same buffering cost.
    HeadOpenTagWithCloseTagFallback,
}

#[derive(Debug, PartialEq, Eq)]
enum InjectorState {
    /// Holding back the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, looking for the earliest valid
    /// injection point within the `<head>` tag.
    BufferingHeadWindow,

    /// Stream-scanning for the `</head>` tag.
    ScanningForHeadCloseTag,

    /// Nothing left to look for, either because the snippet was injected or because no injection
    /// point will be found. Chunks are passed through untouched.
    PassingThrough,

    /// [`Injector::end`] has been called, there is nothing left to emit.
    Ended,
}

/// The data [`Injector::scan_head_window`] works on: the head window itself, and whatever of the
/// incoming chunk follows it.
enum HeadWindow<'a> {
    /// The window was accumulated in `incomplete_data` over several writes. `tail` is the part of
    /// the incoming chunk that completed it, minus the bytes that went into the window. It is empty
    /// at EOF, and when the window was completed exactly.
    Buffered { tail: &'a [u8] },

    /// A single incoming chunk covered the whole window, so it is scanned in place: `window` is its
    /// first DOCUMENT_CHARSET_SCAN_LIMIT bytes, and `tail` everything after them.
    InPlace { window: &'a [u8], tail: &'a [u8] },
}

/// Stateful object used to inject the provided snippet within a streamed HTML document.
///
/// See [`InjectionStrategy`] for where the snippet is injected. [`Injector::new`] keeps the
/// historical `</head>` behaviour; use [`Injector::with_strategy`] to opt into injecting earlier.
pub struct Injector<'snippet> {
    /// Snippet to inject.
    snippet: &'snippet [u8],

    /// Where to inject the snippet.
    strategy: InjectionStrategy,

    /// Tracks the current state of the injection process.
    injector_state: InjectorState,

    /// Whether the snippet has been emitted. Sticky: once injected, we never inject again.
    injected: bool,

    /// Locator for the `</head>` tag, see [`InjectorState::ScanningForHeadCloseTag`].
    head_close_locator: HeadCloseTagLocator,

    /// Data that cannot be emitted yet: the head window as it accumulates, or a partially matched
    /// `</head>` that more data may turn into an injection point.
    ///
    /// Bounded by DOCUMENT_CHARSET_SCAN_LIMIT either way. A chunk only ever contributes the bytes
    /// that complete the window, and the `</head>` locator abandons a match spanning more than
    /// MAX_PENDING_CLOSE_TAG bytes, which is the same figure.
    ///
    /// Two buffers, because a single call may have to emit previously held data *and* hold part of
    /// the incoming chunk back. By convention only `incomplete_data` is written to, and it is moved
    /// into `incomplete_data_public_ref` when it goes out, so nothing is emitted twice: writing
    /// `foo</he` then `ader>bar</` emits `foo`, then `</he` and `ader>bar` once `</he` turns out not
    /// to be a tag, while `</` goes back into the buffer.
    incomplete_data: Vec<u8>,
    incomplete_data_public_ref: Vec<u8>,

    /// Spaces standing in for the snippet when no injection point was found, so that the response
    /// keeps the length it would have had. Filled by [`Injector::end`] only, and emitted at the end
    /// of the document; empty otherwise.
    padding_spaces: Vec<u8>,
}

impl<'snippet> Injector<'snippet> {
    /// Creates an injector using the default [`InjectionStrategy`].
    pub fn new(snippet: &'snippet [u8]) -> Self {
        return Self::with_strategy(snippet, InjectionStrategy::default());
    }

    /// How many bytes of the input written so far are held back: not returned in any result yet.
    ///
    /// They are always the end of that input, and come back out, in order, in later results, in
    /// slices that are not flagged `from_incoming_chunk`. A server that queues the incoming chunks
    /// can therefore leave them untouched until a result injects, then drop this many bytes (read
    /// before the call) from the end of the chunks already queued and serve the result in their
    /// place: see the `chunk_rebuild` tests.
    pub fn held_back(&self) -> usize {
        return self.incomplete_data.len();
    }

    /// Creates an injector using the given [`InjectionStrategy`].
    pub fn with_strategy(snippet: &'snippet [u8], strategy: InjectionStrategy) -> Self {
        return Self {
            snippet,
            strategy,
            injector_state: match strategy {
                // No head window to buffer: start scanning for `</head>` right away.
                InjectionStrategy::HeadCloseTag => InjectorState::ScanningForHeadCloseTag,
                InjectionStrategy::HeadOpenTag
                | InjectionStrategy::HeadOpenTagWithCloseTagFallback => {
                    InjectorState::BufferingHeadWindow
                }
            },
            injected: false,
            head_close_locator: HeadCloseTagLocator::new(),
            incomplete_data: Vec::new(),
            incomplete_data_public_ref: Vec::new(),
            padding_spaces: Vec::new(),
        };
    }

    /// The length a response that announced `content_length`, the value of its `Content-Length`
    /// header, has to announce once the snippet is in it, or `None` if that value is not a valid
    /// length. The snippet is all the injection adds, and the padding [`Injector::end`] emits instead
    /// is as long, so the announced length grows by exactly the snippet length either way.
    ///
    /// A list of identical lengths, as a proxy may produce by merging headers, is valid per RFC 9110
    /// (<https://www.rfc-editor.org/rfc/rfc9110#section-8.6>); anything else is not, and a server
    /// should remove the header rather than announce a wrong length.
    pub fn content_length_with_snippet(&self, content_length: &[u8]) -> Option<u64> {
        let mut announced: Option<u64> = None;
        for value in content_length.split(|&byte| return byte == b',') {
            let value = value.trim_ascii();
            if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let length = core::str::from_utf8(value).ok()?.parse::<u64>().ok()?;
            if announced.is_some_and(|announced| return announced != length) {
                return None;
            }
            announced = Some(length);
        }
        return announced?.checked_add(u64::try_from(self.snippet.len()).ok()?);
    }

    pub fn write<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        match self.injector_state {
            InjectorState::BufferingHeadWindow => {
                // This state is left as soon as the window is complete, so it is always short of at
                // least one byte here.
                debug_assert!(self.incomplete_data.len() < DOCUMENT_CHARSET_SCAN_LIMIT);
                let missing = DOCUMENT_CHARSET_SCAN_LIMIT - self.incomplete_data.len();

                if chunk.len() < missing {
                    // Still not enough data to tell where the injection point is.
                    self.incomplete_data.extend_from_slice(chunk);
                    return self.create_result(&[]);
                }

                if self.incomplete_data.is_empty() {
                    // The chunk already covers the whole window: scan it in place rather than copy
                    // it into the buffer first.
                    let (window, tail) = chunk.split_at(DOCUMENT_CHARSET_SCAN_LIMIT);
                    return self.scan_head_window(HeadWindow::InPlace { window, tail }, false);
                }

                // Buffer only what completes the window; the rest is emitted by this very call, so
                // the buffer never outgrows the window however large the chunk is.
                let (completion, tail) = chunk.split_at(missing);
                self.incomplete_data.extend_from_slice(completion);
                return self.scan_head_window(HeadWindow::Buffered { tail }, false);
            }

            InjectorState::ScanningForHeadCloseTag => return self.scan_head_close_tag(chunk),

            InjectorState::PassingThrough | InjectorState::Ended => {
                return self.create_result(&[BytesSlice::new(chunk, true)]);
            }
        }
    }

    /// Ends the document. When no injection point was found, the output is padded with spaces to
    /// the length it would have had with the snippet.
    pub fn end(&mut self) -> Result<'_> {
        return self.finish(true);
    }

    /// Ends the document like [`Injector::end`], but never pads the output. For consumers that do
    /// not announce the response length up front.
    pub fn end_without_padding(&mut self) -> Result<'_> {
        return self.finish(false);
    }

    fn finish(&mut self, padded: bool) -> Result<'_> {
        if padded && !self.injected {
            self.padding_spaces = vec![b' '; self.snippet.len()];
        } else {
            self.padding_spaces.clear();
        }

        match self.injector_state {
            InjectorState::BufferingHeadWindow => {
                return self.scan_head_window(HeadWindow::Buffered { tail: b"" }, true);
            }

            InjectorState::ScanningForHeadCloseTag | InjectorState::PassingThrough => {
                self.injector_state = InjectorState::Ended;
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                return self.create_result(&[
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(&self.padding_spaces, false),
                ]);
            }

            // Already ended: everything has been emitted and padded for, so emit nothing rather
            // than padding twice.
            InjectorState::Ended => return self.create_result(&[]),
        }
    }

    /// Looks for the earliest valid injection point within the head window, falling back to the
    /// `</head>` tag over the very same data when there is none.
    ///
    /// The data following the window within the chunk is emitted by this call rather than buffered,
    /// so completing the window never requires holding more than the window itself.
    fn scan_head_window<'a>(&'a mut self, input: HeadWindow<'a>, eof: bool) -> Result<'a> {
        let (window, tail, from_incoming_chunk) = match input {
            HeadWindow::InPlace { window, tail } => (window, tail, true),
            HeadWindow::Buffered { tail } => {
                // Hand the held data out through `incomplete_data_public_ref`, so the `</head>`
                // pass can start refilling `incomplete_data`.
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                (&self.incomplete_data_public_ref[..], tail, false)
            }
        };

        if let Some(point) = InjectionPointLocator::find(window) {
            self.injected = true;
            self.injector_state = InjectorState::PassingThrough;
            return self.create_result(&[
                BytesSlice::new(&window[..point], from_incoming_chunk),
                BytesSlice::new(self.snippet, false),
                BytesSlice::new(&window[point..], from_incoming_chunk),
                BytesSlice::new(tail, true),
            ]);
        }

        if self.strategy == InjectionStrategy::HeadOpenTagWithCloseTagFallback {
            self.injector_state = InjectorState::ScanningForHeadCloseTag;

            // The locator is sequential, so scanning the window then the tail is the same as
            // scanning their concatenation; they are split only because they are emitted separately.
            // `window_location` cannot be a `*FromPreviousChunk` variant, being the first data the
            // locator sees.
            let window_location = self.head_close_locator.scan(window);
            if let Location::MatchFromIndex(index) = window_location {
                self.injected = true;
                self.injector_state = InjectorState::PassingThrough;
                return self.create_result(&[
                    BytesSlice::new(&window[..index], from_incoming_chunk),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&window[index..], from_incoming_chunk),
                    BytesSlice::new(tail, true),
                ]);
            }

            match self.head_close_locator.scan(tail) {
                // A `</head>` that starts and ends within the tail. Any partial match left by the
                // window was invalidated along the way, so the window is emitted whole.
                Location::MatchFromIndex(index) => {
                    self.injected = true;
                    self.injector_state = InjectorState::PassingThrough;
                    return self.create_result(&[
                        BytesSlice::new(window, from_incoming_chunk),
                        BytesSlice::new(&tail[..index], true),
                        BytesSlice::new(self.snippet, false),
                        BytesSlice::new(&tail[index..], true),
                    ]);
                }

                // The partial match left by the window turned out to be a real `</head>`.
                Location::MatchFromPreviousChunk => {
                    let Location::PotentialFromIndex(index) = window_location else {
                        unreachable!("a match from previous data implies a pending partial match")
                    };
                    self.injected = true;
                    self.injector_state = InjectorState::PassingThrough;
                    return self.create_result(&[
                        BytesSlice::new(&window[..index], from_incoming_chunk),
                        BytesSlice::new(self.snippet, false),
                        BytesSlice::new(&window[index..], from_incoming_chunk),
                        BytesSlice::new(tail, true),
                    ]);
                }

                // A partial `</head>` at the very end of the data: hold it back, unless the
                // document ends here and it was never a tag. The locator caps a pending match at
                // MAX_PENDING_CLOSE_TAG bytes, so buffering stays bounded.
                Location::PotentialFromIndex(index) if !eof => {
                    self.incomplete_data.extend_from_slice(&tail[index..]);
                    return self.create_result(&[
                        BytesSlice::new(window, from_incoming_chunk),
                        BytesSlice::new(&tail[..index], true),
                    ]);
                }

                // Same, for a partial match that started within the window and runs to the end of
                // the tail.
                Location::PotentialFromPreviousChunk if !eof => {
                    let Location::PotentialFromIndex(index) = window_location else {
                        unreachable!("a partial match from previous data implies a pending one")
                    };
                    self.incomplete_data.extend_from_slice(&window[index..]);
                    self.incomplete_data.extend_from_slice(tail);
                    return self
                        .create_result(&[BytesSlice::new(&window[..index], from_incoming_chunk)]);
                }

                // `Location::None`, or an `eof` partial match.
                _ => {}
            }
        } else {
            // No fallback: there is nothing left to look for.
            self.injector_state = InjectorState::PassingThrough;
        }

        if eof {
            self.injector_state = InjectorState::Ended;
            return self.create_result(&[
                BytesSlice::new(window, from_incoming_chunk),
                BytesSlice::new(tail, true),
                BytesSlice::new(&self.padding_spaces, false),
            ]);
        }

        return self.create_result(&[
            BytesSlice::new(window, from_incoming_chunk),
            BytesSlice::new(tail, true),
        ]);
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
                self.injected = true;
                self.injector_state = InjectorState::PassingThrough;
                return self.create_result(&[
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&self.incomplete_data_public_ref, false),
                    BytesSlice::new(chunk, true),
                ]);
            }

            Location::MatchFromIndex(index) => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                self.injected = true;
                self.injector_state = InjectorState::PassingThrough;
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

        // Emitting more than `Result` can carry would silently truncate the response body.
        debug_assert!(
            slices.iter().filter(|s| return !s.bytes.is_empty()).count() <= result_slices.len(),
            "too many non-empty slices for a single result"
        );

        for slice in slices.iter().filter(|s| return !s.bytes.is_empty()).take(4) {
            result_slices[length] = *slice;
            length += 1;
        }

        return Result {
            slices: result_slices,
            length,
            injected: self.injected,
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

    /// The default strategy, and the behaviour every existing consumer of [`Injector::new`] gets.
    /// These cases are the historical `</head>` test suite and must keep passing unchanged.
    mod head_close_tag {
        use super::*;

        fn test_injector<T: AsRef<[u8]> + core::fmt::Debug>(
            input_chunks: impl IntoIterator<Item = T>,
            expected: &str,
        ) {
            // Goes through `Injector::new`, not `with_strategy`, so this also pins the default.
            return test_injector_with_default_strategy(input_chunks, expected);
        }

        #[test]
        fn basic() {
            test_injector(["abc</head>def"], "abc{{SNIPPET}}</head>def");
            test_injector(["abc</he", "ad>def"], "abc{{SNIPPET}}</head>def");
            test_injector(["abc", "</head>def"], "abc{{SNIPPET}}</head>def");
            test_injector(["abc</head>", "def"], "abc{{SNIPPET}}</head>def");
            test_injector(["abc</h", "ea", "d>def"], "abc{{SNIPPET}}</head>def");
            test_injector(["abc", "</hea", "d>def"], "abc{{SNIPPET}}</head>def");
        }

        #[test]
        fn no_head() {
            test_injector(["abc"], "abc           ");
            test_injector(["abc</hea"], "abc</hea           ");
        }

        #[test]
        fn empty() {
            test_injector::<&str>([], "           ");
            test_injector([""], "           ");
            test_injector(["", ""], "           ");
        }

        #[test]
        fn multiple_head() {
            test_injector(
                ["abc</head>def</head>ghi"],
                "abc{{SNIPPET}}</head>def</head>ghi",
            );
            test_injector(
                ["abc</head>def</h", "ead>ghi"],
                "abc{{SNIPPET}}</head>def</head>ghi",
            );
            test_injector(
                ["abc</head>d", "ef</head>ghi"],
                "abc{{SNIPPET}}</head>def</head>ghi",
            );
        }

        #[test]
        fn incomplete_head() {
            test_injector(["abc</he</head>def"], "abc</he{{SNIPPET}}</head>def");
            test_injector(["abc</he", "</head>def"], "abc</he{{SNIPPET}}</head>def");
            test_injector(
                ["abc</he", "</", "head>def"],
                "abc</he{{SNIPPET}}</head>def",
            );
        }

        #[test]
        fn casing() {
            test_injector(["abc</HeAd>def"], "abc{{SNIPPET}}</HeAd>def");
            test_injector(["abc</HEAD>def"], "abc{{SNIPPET}}</HEAD>def");
        }

        #[test]
        fn spaces() {
            test_injector(["abc </head>def"], "abc {{SNIPPET}}</head>def");
            test_injector(["abc< /head>def"], "abc< /head>def           ");
            // `</` followed by anything but a letter is a bogus comment, not an end tag.
            test_injector(["abc</ head>def"], "abc</ head>def           ");
            test_injector(["abc</h ead>def"], "abc</h ead>def           ");
            test_injector(["abc</he ad>def"], "abc</he ad>def           ");
            test_injector(["abc</hea d>def"], "abc</hea d>def           ");
            test_injector(["abc</head >def"], "abc{{SNIPPET}}</head >def");
            test_injector(["abc</head> def"], "abc{{SNIPPET}}</head> def");
        }

        /// The open `<head>` tag is never an injection point for this strategy, however early it is.
        #[test]
        fn ignores_head_open_tag() {
            test_injector(["<head>abc"], "<head>abc           ");
            test_injector(
                ["<head><meta charset='utf-8'>abc</head>def"],
                "<head><meta charset='utf-8'>abc{{SNIPPET}}</head>def",
            );
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
                Part::new("</head>", true),
                Part::new("</ head>", false),
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

            let mut rng = rand::rng();

            for _ in 0..1000 {
                // Build a random input from 'parts'
                let mut input = String::new();
                let mut expected = String::new();
                let mut has_head = false;

                let parts_count = rng.random_range(0usize..20);
                for part in parts.choose_multiple(&mut rng, parts_count) {
                    input.push_str(part.string);

                    if !has_head && part.has_head {
                        expected.push_str("{{SNIPPET}}");
                        has_head = true;
                    }

                    expected.push_str(part.string);
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
    }

    /// Inject inside the `<head>` tag or not at all.
    mod head_open_tag {
        use super::*;

        fn test_injector<T: AsRef<[u8]> + core::fmt::Debug>(
            input_chunks: impl IntoIterator<Item = T>,
            expected: &str,
        ) {
            return test_injector_with_strategy(
                InjectionStrategy::HeadOpenTag,
                input_chunks,
                expected,
            );
        }

        #[test]
        fn basic() {
            test_injector(
                ["<head", ">abc</he", "ad>def"],
                "<head>{{SNIPPET}}abc</head>def",
            );
        }

        #[test]
        fn injects_after_meta_charset() {
            test_injector(
                ["<head><meta charset='utf-8'>abc</head>def"],
                "<head><meta charset='utf-8'>{{SNIPPET}}abc</head>def",
            );
        }

        /// Unlike the fallback strategy, a `</head>` is never used as a second chance.
        #[test]
        fn does_not_fall_back_to_head_close_tag() {
            test_injector(["abc</head>def"], "abc</head>def           ");

            let input = "A".repeat(1024) + "<head>def</head>ghi";
            let expected = input.clone() + "           ";
            test_injector([input], &expected);
        }

        #[test]
        fn pads_at_eof_when_head_is_past_the_window() {
            let input = "A".repeat(1024) + "<head>def";
            let expected = input.clone() + "           ";
            test_injector([input], &expected);
        }

        #[test]
        fn empty() {
            test_injector::<&str>([], "           ");
            test_injector([""], "           ");
        }

        /// The chunk that completes the window carries more data than the window needs: the rest of
        /// it has to be emitted, not buffered.
        #[test]
        fn emits_the_rest_of_the_chunk_completing_the_window() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 1);
            let input = [filler.clone(), String::from("B<head>def")];
            let expected = filler + "B<head>def           ";

            test_injector(input, &expected);
        }
    }

    /// Inject inside the `<head>` tag, falling back to just before `</head>`.
    mod head_open_tag_with_close_tag_fallback {
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
            let input = format!("<head>{}<meta charset='utf-8'></head>def", "a".repeat(1024));
            let expected = format!(
                "<head>{}{}<meta charset='utf-8'></head>def",
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
        fn ending_again_after_injecting_at_eof_emits_nothing() {
            for padded in [true, false] {
                let mut injector = Injector::with_strategy(
                    b"{{SNIPPET}}",
                    InjectionStrategy::HeadOpenTagWithCloseTagFallback,
                );
                injector.write(b"<head>abc");
                assert!(injector.end().injected);

                let second_end = match padded {
                    true => injector.end(),
                    false => injector.end_without_padding(),
                };
                pretty_assertions::assert_eq!(second_end.length, 0);
            }
        }

        #[test]
        fn partial_head_close_tag_in_the_window_at_eof() {
            test_injector(["abc</hea"], "abc</hea           ");
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

        /// The chunk that completes the window carries the injection point of the `</head>` pass:
        /// the window and the rest of that chunk are scanned and emitted within the same call.
        #[test]
        fn head_close_tag_in_the_rest_of_the_chunk_completing_the_window() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 1);
            let input = [filler.clone(), String::from("Bcd</head>efg")];
            let expected = filler + "Bcd{{SNIPPET}}</head>efg";

            test_injector(input, &expected);
        }

        #[test]
        fn head_close_tag_straddling_the_window_boundary() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 4);
            // The window ends within the `</head>` tag, four bytes in.
            let input = [filler.clone(), String::from("</head>xyz")];
            let expected = filler + "{{SNIPPET}}</head>xyz";

            test_injector(input, &expected);
        }

        #[test]
        fn partial_head_close_tag_at_the_end_of_the_chunk_completing_the_window() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 1);
            let input = [
                filler.clone(),
                String::from("Bxy</he"),
                String::from("ad>z"),
            ];
            let expected = filler + "Bxy{{SNIPPET}}</head>z";

            test_injector(input, &expected);
        }

        /// A partial `</head>` that starts within the window and runs to the end of the chunk
        /// completing it: both parts have to be held back together.
        #[test]
        fn partial_head_close_tag_straddling_the_window_boundary() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 2);
            let input = [filler.clone(), String::from("</h"), String::from("ead>z")];
            let expected = filler + "{{SNIPPET}}</head>z";

            test_injector(input, &expected);
        }

        #[test]
        fn injects_in_the_buffered_window_and_emits_the_rest_of_the_chunk() {
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - "<head>".len());
            let input = [String::from("<head>"), filler.clone() + "tail data"];
            let expected = String::from("<head>{{SNIPPET}}") + &filler + "tail data";

            test_injector(input, &expected);
        }

        /// Completing the window must not pull a whole large chunk into the buffers: everything past
        /// the window is emitted straight away.
        #[test]
        fn buffering_stays_bounded_by_the_window_size() {
            let mut injector = Injector::with_strategy(
                b"{{SNIPPET}}",
                InjectionStrategy::HeadOpenTagWithCloseTagFallback,
            );

            injector.write(b"a");
            let big_chunk = alloc::vec![b'b'; 8 * DOCUMENT_CHARSET_SCAN_LIMIT];
            let emitted: usize = injector
                .write(&big_chunk)
                .iter()
                .map(|slice| return slice.bytes.len())
                .sum();

            pretty_assertions::assert_eq!(emitted, 1 + big_chunk.len());
            assert!(
                injector.incomplete_data.len() + injector.incomplete_data_public_ref.len()
                    <= DOCUMENT_CHARSET_SCAN_LIMIT
            );
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
                Part::new("</ head>", false),
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
            return test_injector_with_strategy(
                InjectionStrategy::HeadOpenTagWithCloseTagFallback,
                input_chunks,
                expected,
            );
        }
    }

    #[test]
    fn content_length_with_snippet() {
        let injector = Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadCloseTag);
        assert_eq!(injector.content_length_with_snippet(b"100"), Some(111));
        assert_eq!(injector.content_length_with_snippet(b" 0\t"), Some(11));
        assert_eq!(
            injector.content_length_with_snippet(b"100, 100,100"),
            Some(111)
        );
        for invalid in [
            b"".as_slice(),
            b" ",
            b"12x",
            b"-1",
            b"+1",
            b"1 2",
            b"100, 101",
            b"100,",
            b"18446744073709551615",
            b"99999999999999999999",
        ] {
            assert_eq!(
                injector.content_length_with_snippet(invalid),
                None,
                "{:?}",
                core::str::from_utf8(invalid)
            );
        }
    }

    /// Replays how a server that queues the incoming chunks rebuilds them around the injection, as
    /// the IIS module does: the queued chunks stay as they are until a result injects; then the
    /// bytes held back from them ([`Injector::held_back`] before that call) are dropped from their
    /// end, the result's slices take the place of the chunk just written, and the later chunks are
    /// served unchanged. Whatever the chunking, that must serve what a single write would.
    mod chunk_rebuild {
        use super::*;
        use alloc::vec::Vec;
        use pretty_assertions::assert_eq;

        const STRATEGIES: [InjectionStrategy; 3] = [
            InjectionStrategy::HeadCloseTag,
            InjectionStrategy::HeadOpenTag,
            InjectionStrategy::HeadOpenTagWithCloseTagFallback,
        ];

        fn rebuild(strategy: InjectionStrategy, chunks: &[&[u8]]) -> Vec<u8> {
            let mut injector = Injector::with_strategy(b"{{SNIPPET}}", strategy);
            for (index, chunk) in chunks.iter().enumerate() {
                let held = injector.held_back();
                let result = injector.write(chunk);
                if result.injected {
                    let mut served: Vec<u8> = chunks[..index].concat();
                    served.truncate(served.len() - held);
                    for slice in result.iter() {
                        served.extend_from_slice(slice.bytes);
                    }
                    for later in &chunks[index + 1..] {
                        served.extend_from_slice(later);
                    }
                    return served;
                }
            }
            let held = injector.held_back();
            let result = injector.end_without_padding();
            let mut served: Vec<u8> = chunks.concat();
            if result.injected {
                served.truncate(served.len() - held);
                for slice in result.iter() {
                    served.extend_from_slice(slice.bytes);
                }
            }
            return served;
        }

        fn check(document: &[u8], split_points: &[usize]) {
            let mut chunks = Vec::new();
            let mut start = 0;
            for &point in split_points {
                chunks.push(&document[start..point]);
                start = point;
            }
            chunks.push(&document[start..]);
            for strategy in STRATEGIES {
                assert_eq!(
                    String::from_utf8(rebuild(strategy, &chunks)).unwrap(),
                    String::from_utf8(rebuild(strategy, &[document])).unwrap(),
                    "{:?} split at {:?}",
                    strategy,
                    split_points
                );
            }
        }

        fn documents() -> Vec<Vec<u8>> {
            let mut long_head =
                String::from("<!DOCTYPE html><html><head><meta charset='utf-8'><title>");
            long_head.push_str(&"x".repeat(1500));
            long_head.push_str("</title></head><body>hi</body></html>");
            let mut head_past_window = String::from("<!-- ");
            head_past_window.push_str(&"c".repeat(1100));
            head_past_window
                .push_str(" --><html><head><title>t</title></head><body>hi</body></html>");
            return [
                long_head.as_str(),
                head_past_window.as_str(),
                "<!DOCTYPE html><html><head><meta charset='utf-8'><title>t</title></head><body>hi</body></html>",
                "<html><head><title>t</title></head><body>hi</body><p>more</p></html>",
                "<body><pre>hello</pre></body>",
                "<html><body>hi</body></he",
            ]
            .iter()
            .map(|document| return document.as_bytes().to_vec())
            .collect();
        }

        #[test]
        fn two_chunks_at_every_point() {
            for document in documents() {
                for point in 0..=document.len() {
                    check(&document, &[point]);
                }
            }
        }

        #[test]
        fn three_chunks() {
            for document in documents() {
                let stride = (document.len() / 40).max(1);
                for first in (0..=document.len()).step_by(stride) {
                    for second in (first..=document.len()).step_by(stride) {
                        check(&document, &[first, second]);
                    }
                }
            }
        }

        // Every split within and around the window end and the `</head>` that follows it, where
        // the head-open fallback hands over to the `</head>` scan.
        #[test]
        fn around_the_window_end_and_head_close_tag() {
            for document in documents() {
                let Some(tag) = document.windows(7).position(|w| return w == b"</head>") else {
                    continue;
                };
                let window_end = DOCUMENT_CHARSET_SCAN_LIMIT.min(document.len());
                for first in window_end.saturating_sub(8)..=(window_end + 8).min(document.len()) {
                    for second in tag.saturating_sub(2).max(first)..=(tag + 8).min(document.len()) {
                        check(&document, &[first, second]);
                    }
                }
            }
        }
    }

    fn test_injector_with_strategy<T: AsRef<[u8]> + core::fmt::Debug>(
        strategy: InjectionStrategy,
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        return run_injector(
            || return Injector::with_strategy(b"{{SNIPPET}}", strategy),
            input_chunks,
            expected,
        );
    }

    /// Exercises [`Injector::new`], so the default strategy is pinned through the API that existing
    /// consumers actually call.
    fn test_injector_with_default_strategy<T: AsRef<[u8]> + core::fmt::Debug>(
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        return run_injector(
            || return Injector::new(b"{{SNIPPET}}"),
            input_chunks,
            expected,
        );
    }

    /// Checks both ways to end the document: [`Injector::end_without_padding`] must give the same
    /// output as [`Injector::end`], minus the padding.
    fn run_injector<T: AsRef<[u8]> + core::fmt::Debug>(
        make_injector: impl Fn() -> Injector<'static>,
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        let input_chunks: Vec<T> = input_chunks.into_iter().collect();
        check_output(make_injector(), &input_chunks, true, expected);

        let padding = " ".repeat(b"{{SNIPPET}}".len());
        let unpadded = match expected.contains("{{SNIPPET}}") {
            true => expected,
            false => expected
                .strip_suffix(padding.as_str())
                .expect("output without injection ends with padding"),
        };
        check_output(make_injector(), &input_chunks, false, unpadded);
    }

    fn check_output<T: AsRef<[u8]> + core::fmt::Debug>(
        mut injector: Injector<'_>,
        input_chunks: &[T],
        padded: bool,
        expected: &str,
    ) {
        let snippet = b"{{SNIPPET}}";

        let mut output_slices = Vec::new();
        let mut injected = false;

        for incoming_chunk in input_chunks
            .iter()
            .map(|chunk| return Some(chunk.as_ref()))
            .chain([None])
        {
            let held_before = injector.held_back();
            let was_injected = injected;
            let mut emitted = 0;
            let result = match incoming_chunk {
                Some(chunk) => injector.write(chunk),
                None if padded => injector.end(),
                None => injector.end_without_padding(),
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

                if !injected && slice.bytes.len() >= snippet.len() {
                    // Look for the snippet anywhere in the slice: it is not necessarily a slice of
                    // its own, as the hoisting path emits it embedded in a reordered buffer.
                    injected = slice
                        .bytes
                        .windows(snippet.len())
                        .any(|w| return w == snippet);
                }
                output_slices.extend_from_slice(slice.bytes);
                emitted += slice.bytes.len();
            }
            assert_eq!(result.injected, injected);

            // Every input byte comes out exactly once: what was held back plus this chunk is what
            // was emitted, minus the snippet (or the padding standing in for it), plus what is
            // still held back.
            let added = match (incoming_chunk, injected && !was_injected) {
                (_, true) => snippet.len(),
                (None, false) if padded && !injected => snippet.len(),
                _ => 0,
            };
            let held_after = injector.held_back();
            assert_eq!(
                held_before + incoming_chunk.map_or(0, <[u8]>::len) + added,
                emitted + held_after,
                "held-back accounting with chunks {:?}",
                input_chunks
            );
            if incoming_chunk.is_none() {
                assert_eq!(held_after, 0, "data still held back after the end");
            }
        }

        assert_eq!(
            String::from_utf8(output_slices).unwrap(),
            expected,
            "with chunks {:?}, padded: {}",
            input_chunks,
            padded
        );
    }
}
