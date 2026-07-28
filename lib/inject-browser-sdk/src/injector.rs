// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;

use crate::injection_point_locator::{
    HeadCloseTagLocator, InjectionPoint, InjectionPointLocator, Location,
};

/// Number of bytes at the start of the document within which a character encoding declaration has
/// to be serialized to be honoured by browsers, as per the HTML spec
/// (<https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>).
///
/// We hold back at most this many bytes while looking for the earliest valid injection point:
/// injecting the snippet *before* a `<meta charset>` would push that declaration out of the window
/// and change how the browser decodes the document. Conversely, a declaration that is already
/// outside of the window is ignored by browsers anyway, so there is no point looking further.
const DOCUMENT_CHARSET_SCAN_LIMIT: usize = 1024;

/// Where the snippet should be injected within the HTML document.
///
/// The trade-off is between how early the snippet runs and how much data has to be held back to
/// decide where it goes. The earlier the snippet runs, the more of the page lifecycle RUM observes,
/// but injecting early means proving that no character encoding declaration follows, which cannot be
/// done without buffering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectionStrategy {
    /// Inject right before the `</head>` tag.
    ///
    /// Scans in a streaming fashion and never holds back more than a partially matched tag, at the
    /// cost of the snippet running as late as possible within the head. Injecting here can never
    /// displace a character encoding declaration.
    ///
    /// This is the default, and the behaviour [`Injector::new`] selects.
    #[default]
    HeadCloseTag,

    /// Inject at the earliest valid position within the `<head>` tag, and do not inject at all if
    /// there is none within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes.
    ///
    /// Holds back up to DOCUMENT_CHARSET_SCAN_LIMIT bytes, which delays the start of the response
    /// body by that much.
    HeadOpenTag,

    /// Like [`InjectionStrategy::HeadOpenTag`], but fall back to
    /// [`InjectionStrategy::HeadCloseTag`] when the head window holds no valid injection point,
    /// rather than giving up.
    ///
    /// Same buffering cost as [`InjectionStrategy::HeadOpenTag`], but instruments documents whose
    /// head does not start within the window.
    HeadOpenTagWithCloseTagFallback,
}

/// Where the snippet should sit relative to a `<meta http-equiv="content-security-policy">` element
/// that *precedes* the character encoding declaration.
///
/// The snippet is kept behind the encoding declaration, so when a meta-delivered policy comes first
/// the snippet lands after it and is governed by it. Being an inline script with no nonce, a strict
/// `script-src` then blocks RUM entirely.
///
/// Orthogonal to [`InjectionStrategy`], and meaningless for [`InjectionStrategy::HeadCloseTag`],
/// which always injects past any meta-delivered policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetaCspPlacement {
    /// Default. Never reorder; when a meta-delivered policy precedes the encoding declaration the
    /// snippet lands after it and is governed by it.
    #[default]
    After,

    /// Hoist the encoding declaration above the policy so the snippet precedes it.
    ///
    /// The exemption rests on browser timing: the inline snippet synchronously inserts the CDN
    /// `<script async>`, and that fetch is checked against the policy in force at insertion time,
    /// before the parser reaches the meta element.
    ///
    /// Two caveats:
    /// * this only exempts the snippet from a *meta*-delivered policy. A `Content-Security-Policy`
    ///   response header applies as well, and the injector never sees response headers, so it
    ///   cannot detect one.
    /// * classic scripts within the hoisted span now run *after* the snippet. The
    ///   `h[d] = h[d] || {...}` guard in the snippet copes with that, but a customer script that
    ///   *replaces* `window.DD_RUM` rather than augmenting it changes behaviour.
    Before,
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

    /// Whether the snippet may be hoisted above a meta-delivered Content Security Policy.
    csp_placement: MetaCspPlacement,

    /// Tracks the current state of the injection process.
    injector_state: InjectorState,

    /// Whether the snippet has been emitted. Sticky: once injected, we never inject again.
    injected: bool,

    /// Locator for the `</head>` tag, see [`InjectorState::ScanningForHeadCloseTag`].
    head_close_locator: HeadCloseTagLocator,

    /// Data that cannot be emitted yet.
    ///
    /// While buffering the head window, this accumulates the incoming chunks until we have enough
    /// data to decide where to inject. During the `</head>` pass, it holds a partially matched tag
    /// that may turn out to be an injection point once more data arrives.
    ///
    /// Either way it stays within DOCUMENT_CHARSET_SCAN_LIMIT bytes. While buffering the window, an
    /// incoming chunk only ever contributes the bytes that complete it, never its whole length.
    /// During the `</head>` pass, the locator abandons a match that spans more than
    /// MAX_PENDING_CLOSE_TAG bytes, which is the same figure.
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

    /// The head window, rewritten with the encoding declaration hoisted above a meta-delivered
    /// policy and the snippet behind it. Only ever used by [`MetaCspPlacement::Before`].
    ///
    /// The reordered window is five fragments plus the tail, over what a single [`Result`] can
    /// carry, and raising that cap would be an ABI change. Assembling the window here instead costs
    /// one memcpy of at most DOCUMENT_CHARSET_SCAN_LIMIT bytes, once per hoisted response.
    reordered_window: Vec<u8>,
}

impl<'snippet> Injector<'snippet> {
    /// Creates an injector using the default [`InjectionStrategy`].
    pub fn new(snippet: &'snippet [u8]) -> Self {
        return Self::with_strategy(snippet, InjectionStrategy::default());
    }

    /// Creates an injector using the given [`InjectionStrategy`] and the default
    /// [`MetaCspPlacement`].
    pub fn with_strategy(snippet: &'snippet [u8], strategy: InjectionStrategy) -> Self {
        return Self::with_options(snippet, strategy, MetaCspPlacement::default());
    }

    /// Creates an injector using the given [`InjectionStrategy`] and [`MetaCspPlacement`].
    pub fn with_options(
        snippet: &'snippet [u8],
        strategy: InjectionStrategy,
        csp_placement: MetaCspPlacement,
    ) -> Self {
        return Self {
            snippet,
            strategy,
            csp_placement,
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
            reordered_window: Vec::new(),
        };
    }

    pub fn write<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        match self.injector_state {
            InjectorState::BufferingHeadWindow => {
                // Invariant: while in this state, the buffer holds less than a full window, so at
                // least one more byte is needed to complete it. We leave this state as soon as it
                // is complete, and only ever buffer the bytes that are missing from it.
                debug_assert!(self.incomplete_data.len() < DOCUMENT_CHARSET_SCAN_LIMIT);
                let missing = DOCUMENT_CHARSET_SCAN_LIMIT - self.incomplete_data.len();

                if chunk.len() < missing {
                    // Still not enough data to tell where the injection point is.
                    self.incomplete_data.extend_from_slice(chunk);
                    return self.create_result(&[]);
                }

                if self.incomplete_data.is_empty() {
                    // Optimization: the chunk already covers the whole head window, so scan it
                    // directly instead of copying it into the buffer first.
                    let (window, tail) = chunk.split_at(DOCUMENT_CHARSET_SCAN_LIMIT);
                    return self.scan_head_window(HeadWindow::InPlace { window, tail }, false);
                }

                // Buffer only what completes the window. Everything past it is emitted as part of
                // this very call, so the buffer never grows beyond the window, however large the
                // incoming chunk is.
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

    pub fn end(&mut self) -> Result<'_> {
        match self.injector_state {
            InjectorState::BufferingHeadWindow => {
                return self.scan_head_window(HeadWindow::Buffered { tail: b"" }, true);
            }

            InjectorState::ScanningForHeadCloseTag | InjectorState::PassingThrough => {
                self.injector_state = InjectorState::Ended;
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                if !self.injected {
                    self.padding_spaces = vec![b' '; self.snippet.len()];
                }
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

    /// Looks for the earliest valid injection point within the head window. If there is none, falls
    /// back to the `</head>` tag and scans the very same data for it.
    ///
    /// `input` also carries the data that follows the window within the incoming chunk, if any: it
    /// is emitted by this call rather than buffered, so completing the window never requires holding
    /// more than the window itself.
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

        match InjectionPointLocator::find(window, self.csp_placement) {
            Some(InjectionPoint::Insert(point)) => {
                self.injected = true;
                self.injector_state = InjectorState::PassingThrough;
                return self.create_result(&[
                    BytesSlice::new(&window[..point], from_incoming_chunk),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&window[point..], from_incoming_chunk),
                    BytesSlice::new(tail, true),
                ]);
            }

            Some(InjectionPoint::HoistDeclaration {
                head_end,
                decl_start,
                decl_end,
            }) => {
                // Five fragments do not fit in a `Result`, so the window is reassembled in a buffer
                // of our own and emitted as one slice. `from_incoming_chunk` is `false` for it: the
                // bytes live in the injector, not in the caller's chunk.
                self.reordered_window.clear();
                self.reordered_window.extend_from_slice(&window[..head_end]);
                self.reordered_window
                    .extend_from_slice(&window[decl_start..decl_end]);
                self.reordered_window.extend_from_slice(self.snippet);
                self.reordered_window
                    .extend_from_slice(&window[head_end..decl_start]);
                self.reordered_window.extend_from_slice(&window[decl_end..]);

                self.injected = true;
                self.injector_state = InjectorState::PassingThrough;
                return self.create_result(&[
                    BytesSlice::new(&self.reordered_window, false),
                    BytesSlice::new(tail, true),
                ]);
            }

            None => {}
        }

        if self.strategy == InjectionStrategy::HeadOpenTagWithCloseTagFallback {
            self.injector_state = InjectorState::ScanningForHeadCloseTag;

            // The `</head>` locator is sequential, so scanning the window then the tail is the same
            // as scanning their concatenation. They are scanned separately only because they are
            // emitted separately.
            //
            // `window_location` cannot be one of the `*FromPreviousChunk` variants: this is the
            // first data the locator sees.
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

                // A partial `</head>` at the very end of the data: hold it back, unless the document
                // ends here, in which case it was never a real tag. The locator caps a pending match
                // at MAX_PENDING_CLOSE_TAG bytes, so this does not reintroduce unbounded buffering.
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
            self.padding_spaces = vec![b' '; self.snippet.len()];
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

    /// Hoisting the encoding declaration above a meta-delivered policy, so the snippet precedes it.
    mod meta_csp_placement_before {
        use super::*;

        const CSP: &str =
            "<meta http-equiv='content-security-policy' content=\"script-src 'self'\">";

        fn test_injector<T: AsRef<[u8]> + core::fmt::Debug>(
            input_chunks: impl IntoIterator<Item = T>,
            expected: &str,
        ) {
            return run_injector(
                Injector::with_options(
                    b"{{SNIPPET}}",
                    InjectionStrategy::HeadOpenTagWithCloseTagFallback,
                    MetaCspPlacement::Before,
                ),
                input_chunks,
                expected,
            );
        }

        /// The window is buffered over several writes and flushed by `end`.
        #[test]
        fn buffered_window() {
            test_injector(
                [
                    "<html><head>",
                    CSP,
                    "<meta charset='utf-8'></head><body>hi</body></html>",
                ],
                &format!(
                    "<html><head><meta charset='utf-8'>{{{{SNIPPET}}}}{CSP}</head><body>hi</body></html>"
                ),
            );
        }

        /// A single chunk covers the whole window, so it is scanned in place and the tail past the
        /// window is emitted alongside the reordered buffer.
        #[test]
        fn window_scanned_in_place_with_a_tail() {
            let head = format!("<html><head>{CSP}<meta charset='utf-8'></head>");
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - head.len());
            let input = head + &filler + "tail data";
            let expected = format!(
                "<html><head><meta charset='utf-8'>{{{{SNIPPET}}}}{CSP}</head>{filler}tail data"
            );

            test_injector([input], &expected);
        }

        /// The reorder is length preserving, which is what keeps the padding path able to stand in
        /// for a missing snippet.
        #[test]
        fn preserves_length() {
            let input = format!("<html><head>{CSP}<meta charset='utf-8'></head></html>");
            let mut injector = Injector::with_options(
                b"{{SNIPPET}}",
                InjectionStrategy::HeadOpenTagWithCloseTagFallback,
                MetaCspPlacement::Before,
            );

            let mut emitted: usize = injector
                .write(input.as_bytes())
                .iter()
                .map(|slice| return slice.bytes.len())
                .sum();
            emitted += injector
                .end()
                .iter()
                .map(|slice| return slice.bytes.len())
                .sum::<usize>();

            pretty_assertions::assert_eq!(emitted, input.len() + "{{SNIPPET}}".len());
        }

        /// Nothing to escape, so the output is the plain insertion the default placement produces.
        #[test]
        fn no_policy_falls_back_to_a_plain_insertion() {
            test_injector(
                ["<html><head><meta charset='utf-8'></head></html>"],
                "<html><head><meta charset='utf-8'>{{SNIPPET}}</head></html>",
            );
        }

        /// The `</head>` fallback runs past any meta-delivered policy already, so it never hoists.
        #[test]
        fn head_close_tag_fallback_is_unaffected() {
            let filler = "A".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            test_injector(
                [filler.clone() + "<head>abc</head>def"],
                &(filler + "<head>abc{{SNIPPET}}</head>def"),
            );
        }
    }

    fn test_injector_with_strategy<T: AsRef<[u8]> + core::fmt::Debug>(
        strategy: InjectionStrategy,
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        return run_injector(
            Injector::with_strategy(b"{{SNIPPET}}", strategy),
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
        return run_injector(Injector::new(b"{{SNIPPET}}"), input_chunks, expected);
    }

    fn run_injector<T: AsRef<[u8]> + core::fmt::Debug>(
        mut injector: Injector<'_>,
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        let snippet = b"{{SNIPPET}}";

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

                if !injected && slice.bytes.len() >= snippet.len() {
                    // Look for the snippet anywhere in the slice: it is not necessarily a slice of
                    // its own, as the hoisting path emits it embedded in a reordered buffer.
                    injected = slice
                        .bytes
                        .windows(snippet.len())
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
