// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;

mod encoding;
mod head_open_tag;
mod parser;

use head_open_tag::{HeadOpenTag, HeadOpenTagLocator, Target};
use parser::Placement;

/// Window at the start of the document within which a character encoding declaration is honoured by
/// browsers, as per the HTML spec
/// (<https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>).
///
/// Injecting before a `<meta charset>` could push it out of this window and change how the document
/// is decoded, so the head is walked within these bytes, and past them only as long as it is a run
/// of `<meta>` and `<title>` tags, see [`WALK_LIMIT`]. They are also the bytes that tell whether the
/// document is HTML at all, and the ones a `<head>` tag has to start within to be walked from.
const DOCUMENT_CHARSET_SCAN_LIMIT: usize = 1024;

/// How far into the document the walk of the head may go, when it is still a run of `<meta>` and
/// `<title>` tags past [`DOCUMENT_CHARSET_SCAN_LIMIT`]: an `X-UA-Compatible` declaration is honoured
/// anywhere within such a run. Past this limit the snippet goes at the best point found so far, so
/// that no more than this is ever held back.
const WALK_LIMIT: usize = 10 * 1024;

/// How far into the document HTML evidence is looked for: an `<html>`, `<meta>` (or, looking for
/// `</head>`, `<head>`) start tag or a `<!DOCTYPE html>` has to start within this many bytes,
/// comments, doctypes and whitespace not counted, for the document to be taken for HTML. A license
/// banner or blank lines at the top do not push the evidence out of reach, while a body that is not
/// HTML, JSON or text, is scanned no further. Nothing is held back for it.
const EVIDENCE_LIMIT: usize = 10 * 1024;

/// The longest `</head>` end tag injected before, from its `<` to its `>`, and so the most that is
/// held back looking for its `>`: a valid one has no attributes, so only an end tag stuffed with
/// them is given up on, rather than held back without bound.
const MAX_PENDING_END_TAG: usize = 1024;

/// Where the snippet goes within the `<head>` of an HTML document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InjectionStrategy {
    /// Right before the `</head>` end tag, the default and what [`Injector::new`] selects. Only the
    /// first real one counts (not one written in a comment, an attribute value or a raw text
    /// element), and only in a document that looks like HTML: an `<html>`, `<meta>` or `<head>`
    /// start tag or a `<!DOCTYPE html>` starts within EVIDENCE_LIMIT bytes, comments, doctypes and
    /// whitespace not counted, before it. Body content
    /// (`<body>`, `<div>`…), text outside tags (whitespace and a leading byte order mark aside), a
    /// `</body>`, `</html>` or `</br>` end tag, all of which close the head first, and a CDATA
    /// section end the search; without HTML evidence within the limit, the rest of the document is
    /// not scanned. The `</head>` tag may come anywhere past it.
    ///
    /// Holds back nothing but a `</head` tag that a chunk cuts short.
    #[default]
    HeadCloseTag,

    /// As early as possible within the head: right after the `<head>` start tag, or after the last
    /// `<meta>` tag the snippet has to stay behind (see [`parser::find_injection_point`]).
    ///
    /// Holds back the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, up to WALK_LIMIT for a head that is
    /// a long run of `<meta>` and `<title>` tags, delaying the start of the response body by as
    /// much.
    HeadOpenTag,
}

#[derive(Debug, PartialEq, Eq)]
enum InjectorState {
    /// Holding back the first bytes of the document, which decide where the snippet goes: the first
    /// DOCUMENT_CHARSET_SCAN_LIMIT, and more, up to WALK_LIMIT, while the head they hold is a run of
    /// `<meta>` and `<title>` tags that has not ended.
    BufferingHeadWindow,

    /// The window holds no `<head>` start tag but looks like an HTML document: stream-scanning for
    /// the `<head>` tag, to inject right after it.
    ScanningForHeadOpenTag,

    /// [`InjectionStrategy::HeadCloseTag`]: stream-scanning for the `</head>` tag, to inject right
    /// before it, holding back only a `</head` tag a chunk cuts short.
    ScanningForHeadCloseTag,

    /// Nothing left to look for, either because the snippet was injected or because it never will
    /// be. Chunks are passed through untouched.
    PassingThrough,

    /// [`Injector::end`] has been called, there is nothing left to emit.
    Ended,
}

/// The data [`Injector::scan_head_window`] works on: the head window itself, and whatever of the
/// incoming chunk follows it.
#[derive(Clone, Copy)]
enum HeadWindow<'a> {
    /// The window was accumulated in `incomplete_data` over several writes. `tail` is the part of
    /// the incoming chunk that completed it, minus the bytes that went into the window. It is empty
    /// at EOF, and when the window was completed exactly.
    Buffered { tail: &'a [u8] },

    /// Nothing was held back, and the incoming chunk covers the whole window, so it is scanned in
    /// place.
    InPlace { chunk: &'a [u8] },
}

/// Stateful object used to inject the provided snippet within a streamed HTML document.
///
/// The snippet only ever goes within the `<head>` of the document, where the [`InjectionStrategy`]
/// says: by default right before the `</head>` tag ([`InjectionStrategy::HeadCloseTag`]). With
/// [`InjectionStrategy::HeadOpenTag`]:
/// * if the `<head>` start tag is within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, right after
///   it, unless the head holds `<meta>` tags declaring the character encoding, `X-UA-Compatible`
///   or a `Content-Security-Policy`: then right after the last of them. Other `<meta>` tags, SEO, viewport or
///   the tracer's, do not hold the snippet back (see [`parser::find_injection_point`] for the
///   details);
/// * otherwise, if the document looks like HTML, holding an `<html>` or `<meta>` start tag or a
///   `<!DOCTYPE html>` within EVIDENCE_LIMIT bytes, comments, doctypes and whitespace not counted,
///   right after the first `<head>` start tag that follows;
/// * otherwise not at all: the document passes through untouched, padded as [`Injector::end`]
///   describes. This keeps the snippet out of fragments, JSON, plain text and any other content
///   served as `text/html`, and out of documents that have no `<head>` tag.
///
/// Either way, a `<head>` tag only counts before any start tag but `<html>` and the ones the head
/// may hold, and before any text but whitespace: past a `<body>`, a `<div>`, any other body content
/// or text, the browser ignores one, and the search for it ends there rather than read the rest of
/// the document. Within a `<template>`, whose content is inert, none of these count.
///
/// The first DOCUMENT_CHARSET_SCAN_LIMIT bytes are held back until they are complete, which delays
/// the start of the response body by as much. When they end within a head that is still a run of
/// `<meta>` and `<title>` tags, more is held back, up to WALK_LIMIT bytes, until the run ends. Once
/// the snippet is placed, or the window holds no `<head>` tag, nothing is held back.
pub struct Injector<'snippet> {
    /// Snippet to inject.
    snippet: &'snippet [u8],

    /// Tracks the current state of the injection process.
    injector_state: InjectorState,

    /// Whether the snippet has been emitted. Sticky: once injected, we never inject again.
    injected: bool,

    /// Locator for the `<head>` start tag, over the head window and then the rest of the document.
    head_locator: HeadOpenTagLocator,

    /// Index just past the `<head>` tag the head window holds, once it has been found there: the
    /// walk of the head starts from it. Set while the walk needs more data than the window.
    head_end: Option<usize>,

    /// How many bytes to hold back before walking the head again: DOCUMENT_CHARSET_SCAN_LIMIT at
    /// first, then twice what the last walk was given, up to WALK_LIMIT, so that walking again and
    /// again over a growing window stays linear in its size.
    hold_target: usize,

    /// The head window as it accumulates, bounded by `hold_target`: a chunk only ever contributes the
    /// bytes that complete it, unless the walk asks for more of them.
    ///
    /// It moves into `incomplete_data_public_ref` when the window is complete, so that the slices
    /// returned can point into it while `incomplete_data` is left empty: nothing is held back past
    /// the window.
    incomplete_data: Vec<u8>,
    incomplete_data_public_ref: Vec<u8>,

    /// Spaces standing in for the snippet when no injection point was found, so that the response
    /// keeps the length it would have had. Filled by [`Injector::end`] only, and emitted at the end
    /// of the document; empty otherwise.
    padding_spaces: Vec<u8>,
}

impl<'snippet> Injector<'snippet> {
    /// An injector with the default strategy, [`InjectionStrategy::HeadCloseTag`].
    pub fn new(snippet: &'snippet [u8]) -> Self {
        return Self::with_strategy(snippet, InjectionStrategy::default());
    }

    pub fn with_strategy(snippet: &'snippet [u8], strategy: InjectionStrategy) -> Self {
        let (injector_state, target) = match strategy {
            InjectionStrategy::HeadCloseTag => {
                (InjectorState::ScanningForHeadCloseTag, Target::HeadEnd)
            }
            InjectionStrategy::HeadOpenTag => {
                (InjectorState::BufferingHeadWindow, Target::HeadStart)
            }
        };
        return Self {
            snippet,
            injector_state,
            injected: false,
            head_locator: HeadOpenTagLocator::with_target(target),
            head_end: None,
            hold_target: DOCUMENT_CHARSET_SCAN_LIMIT,
            incomplete_data: Vec::new(),
            incomplete_data_public_ref: Vec::new(),
            padding_spaces: Vec::new(),
        };
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
                // This state is left or the target raised as soon as the window reaches it, so it is
                // always short of at least one byte here.
                debug_assert!(self.incomplete_data.len() < self.hold_target);
                let missing = self.hold_target - self.incomplete_data.len();

                if chunk.len() < missing {
                    // Still not enough data to tell where the injection point is.
                    self.incomplete_data.extend_from_slice(chunk);
                    return self.create_result(&[]);
                }

                if self.incomplete_data.is_empty() {
                    // The chunk already covers the whole window: scan it in place rather than copy
                    // it into the buffer first.
                    return self.scan_head_window(HeadWindow::InPlace { chunk }, false);
                }

                // Buffer only what completes the window; the rest is emitted by this very call, so
                // the buffer never outgrows the window however large the chunk is.
                let (completion, tail) = chunk.split_at(missing);
                self.incomplete_data.extend_from_slice(completion);
                return self.scan_head_window(HeadWindow::Buffered { tail }, false);
            }

            InjectorState::ScanningForHeadOpenTag => return self.scan_head_open_tag(chunk),

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

            InjectorState::ScanningForHeadOpenTag | InjectorState::PassingThrough => {
                self.injector_state = InjectorState::Ended;
                return self.create_result(&[BytesSlice::new(&self.padding_spaces, false)]);
            }

            // A `</head` tag the document ends in comes out as it is.
            InjectorState::ScanningForHeadCloseTag => {
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

    /// Decides where the snippet goes from the head window, and scans whatever of the chunk follows
    /// it for the `<head>` tag when the window holds none.
    ///
    /// The data following the window within the chunk is emitted by this call rather than buffered,
    /// so completing the window never requires holding more than the window itself, short of a walk
    /// of the head that needs more of it.
    fn scan_head_window<'a>(&'a mut self, mut input: HeadWindow<'a>, eof: bool) -> Result<'a> {
        let head_end = match self.head_end {
            Some(head_end) => head_end,
            None => {
                let held = match input {
                    HeadWindow::InPlace { chunk } => chunk,
                    HeadWindow::Buffered { .. } => &self.incomplete_data[..],
                };
                let window = &held[..held.len().min(DOCUMENT_CHARSET_SCAN_LIMIT)];
                match self.head_locator.scan(window) {
                    Some(head) => *self.head_end.insert(head.end),
                    None => return self.scan_past_head_window(input, eof),
                }
            }
        };

        let placement = loop {
            let held = match input {
                HeadWindow::InPlace { chunk } => chunk,
                HeadWindow::Buffered { .. } => &self.incomplete_data[..],
            };
            let placement = parser::find_injection_point(held, head_end, eof);
            if placement != Placement::NeedMore {
                break placement;
            }
            match input {
                // Hold it all back: the walk is short of data, so the chunk is shorter than the
                // limit.
                HeadWindow::InPlace { chunk } => self.incomplete_data.extend_from_slice(chunk),
                // Walk the rest of the chunk as well, as much of it as the limit lets.
                HeadWindow::Buffered { tail } if !tail.is_empty() => {
                    let taken = tail.len().min(WALK_LIMIT - self.incomplete_data.len());
                    self.incomplete_data.extend_from_slice(&tail[..taken]);
                    input = HeadWindow::Buffered {
                        tail: &tail[taken..],
                    };
                    continue;
                }
                HeadWindow::Buffered { .. } => {}
            }
            debug_assert!(self.incomplete_data.len() < WALK_LIMIT);
            self.hold_target = (2 * self.incomplete_data.len()).min(WALK_LIMIT);
            return self.create_result(&[]);
        };

        let (held, tail, from_incoming_chunk) = {
            if let HeadWindow::Buffered { .. } = input {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
            }
            take_head_window(&self.incomplete_data_public_ref, input)
        };
        self.injector_state = match eof {
            true => InjectorState::Ended,
            false => InjectorState::PassingThrough,
        };
        if let Placement::At(point) = placement {
            self.injected = true;
            return self.create_result(&[
                BytesSlice::new(&held[..point], from_incoming_chunk),
                BytesSlice::new(self.snippet, false),
                BytesSlice::new(&held[point..], from_incoming_chunk),
                BytesSlice::new(tail, true),
            ]);
        }
        return self.create_result(&[
            BytesSlice::new(held, from_incoming_chunk),
            BytesSlice::new(tail, true),
            BytesSlice::new(&self.padding_spaces, false),
        ]);
    }

    /// The head window holds no `<head>` tag: scans whatever of the chunk follows it for one, when
    /// the window looks like an HTML document.
    fn scan_past_head_window<'a>(&'a mut self, input: HeadWindow<'a>, eof: bool) -> Result<'a> {
        let (held, tail, from_incoming_chunk) = {
            if let HeadWindow::Buffered { .. } = input {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
            }
            take_head_window(&self.incomplete_data_public_ref, input)
        };
        // Before the walk asks for more, the window is all that is held back.
        let (window, tail) = match from_incoming_chunk {
            true => held.split_at(held.len().min(DOCUMENT_CHARSET_SCAN_LIMIT)),
            false => (held, tail),
        };
        self.injector_state = InjectorState::PassingThrough;

        if self.may_find_injection_point() {
            // Without HTML evidence in the window, only a `<head>` tag starting within it could be
            // injected after: the tail is not scanned when none can, a body that is not HTML
            // would otherwise be read to the end of the chunk for nothing.
            //
            // The locator is sequential, so scanning the window then the tail is the same as
            // scanning their concatenation; they are split only because they are emitted separately.
            if let Some(head) = self.head_locator.scan(tail) {
                if self.may_inject_after(head) {
                    self.injected = true;
                    return self.create_result(&[
                        BytesSlice::new(window, from_incoming_chunk),
                        BytesSlice::new(&tail[..head.end], true),
                        BytesSlice::new(self.snippet, false),
                        BytesSlice::new(&tail[head.end..], true),
                    ]);
                }
            } else if !eof && self.may_find_injection_point() {
                self.injector_state = InjectorState::ScanningForHeadOpenTag;
            }
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

    /// Past the head window: stream-scan for the `<head>` tag. The snippet goes after it, so nothing
    /// has to be held back.
    fn scan_head_open_tag<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        match self.head_locator.scan(chunk) {
            Some(head) => {
                self.injector_state = InjectorState::PassingThrough;
                if self.may_inject_after(head) {
                    self.injected = true;
                    return self.create_result(&[
                        BytesSlice::new(&chunk[..head.end], true),
                        BytesSlice::new(self.snippet, false),
                        BytesSlice::new(&chunk[head.end..], true),
                    ]);
                }
            }
            None if !self.may_find_injection_point() => {
                self.injector_state = InjectorState::PassingThrough;
            }
            None => {}
        }
        return self.create_result(&[BytesSlice::new(chunk, true)]);
    }

    /// [`InjectionStrategy::HeadCloseTag`]: stream-scans for the `</head>` tag, and injects right
    /// before it. What was held back, a `</head` tag the previous chunk cut short, comes out first;
    /// a `</head` tag this chunk cuts short is held back in turn.
    fn scan_head_close_tag<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
        let chunk_start = self.head_locator.offset();
        let held_start = chunk_start - self.incomplete_data_public_ref.len();

        if let Some(tag) = self.head_locator.scan(chunk) {
            self.injector_state = InjectorState::PassingThrough;
            let held = &self.incomplete_data_public_ref[..];
            // One longer than could be held back is given up on whatever the chunking, so that the
            // outcome does not depend on it.
            let too_long = chunk_start + tag.end - tag.start > MAX_PENDING_END_TAG;
            if too_long || !self.holds_html_evidence() {
                return self
                    .create_result(&[BytesSlice::new(held, false), BytesSlice::new(chunk, true)]);
            }
            self.injected = true;
            if tag.start >= chunk_start {
                let point = tag.start - chunk_start;
                return self.create_result(&[
                    BytesSlice::new(held, false),
                    BytesSlice::new(&chunk[..point], true),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&chunk[point..], true),
                ]);
            }
            let point = tag.start - held_start;
            return self.create_result(&[
                BytesSlice::new(&held[..point], false),
                BytesSlice::new(self.snippet, false),
                BytesSlice::new(&held[point..], false),
                BytesSlice::new(chunk, true),
            ]);
        }

        let pending = match self.may_find_head_close_tag() {
            true => self.head_locator.pending_end_tag_start(),
            false => {
                self.injector_state = InjectorState::PassingThrough;
                None
            }
        };
        // Hold back from the `<` of a `</head` tag being read, unless it outgrows any valid one.
        if let Some(start) = pending.filter(|&start| {
            return chunk_start + chunk.len() - start <= MAX_PENDING_END_TAG;
        }) {
            let held = &self.incomplete_data_public_ref[..];
            let (emitted_held, emitted_chunk) = match start >= chunk_start {
                true => {
                    let point = start - chunk_start;
                    self.incomplete_data.extend_from_slice(&chunk[point..]);
                    (held, &chunk[..point])
                }
                false => {
                    let point = start - held_start;
                    self.incomplete_data.extend_from_slice(&held[point..]);
                    self.incomplete_data.extend_from_slice(chunk);
                    (&held[..point], &chunk[..0])
                }
            };
            return self.create_result(&[
                BytesSlice::new(emitted_held, false),
                BytesSlice::new(emitted_chunk, true),
            ]);
        }
        if pending.is_some() {
            self.injector_state = InjectorState::PassingThrough;
        }
        return self.create_result(&[
            BytesSlice::new(&self.incomplete_data_public_ref, false),
            BytesSlice::new(chunk, true),
        ]);
    }

    /// Whether a `</head>` tag found in what follows may still be injected before: not once the
    /// locator stopped, nor once the first DOCUMENT_CHARSET_SCAN_LIMIT bytes went by without HTML
    /// evidence.
    fn may_find_head_close_tag(&self) -> bool {
        return self.head_locator.is_searching()
            && (self.holds_html_evidence() || self.may_still_find_evidence());
    }

    /// [`InjectionStrategy::HeadCloseTag`]'s evidence that the document is HTML: an `<html>`,
    /// `<meta>` or `<head>` start tag or a `<!DOCTYPE html>`, within EVIDENCE_LIMIT bytes.
    fn holds_html_evidence(&self) -> bool {
        return self
            .head_locator
            .significant_evidence()
            .is_some_and(|start| return start < EVIDENCE_LIMIT);
    }

    /// Whether evidence may still come within EVIDENCE_LIMIT bytes, comments, doctypes and
    /// whitespace not counted: in what follows, or in a tag still being read.
    fn may_still_find_evidence(&self) -> bool {
        return self.head_locator.significant_scanned() < EVIDENCE_LIMIT
            || self
                .head_locator
                .significant_pending_tag_start()
                .is_some_and(|start| return start < EVIDENCE_LIMIT);
    }

    /// Whether the snippet may go after `head`, a `<head>` tag found past the head window or ending
    /// past it: only if the tag starts within the window, or if the document looks like HTML.
    fn may_inject_after(&self, head: HeadOpenTag) -> bool {
        return head.start < DOCUMENT_CHARSET_SCAN_LIMIT || self.looks_like_html();
    }

    /// Whether a `<head>` tag found in what follows may still be injected after, see
    /// [`Injector::may_inject_after`]. Once it may not, scanning the rest of the document is pointless.
    fn may_find_injection_point(&self) -> bool {
        if !self.head_locator.is_searching() {
            return false;
        }
        return self.looks_like_html() || self.may_still_find_evidence();
    }

    /// [`InjectionStrategy::HeadOpenTag`]'s evidence that the document is HTML: an `<html>` or
    /// `<meta>` start tag or a `<!DOCTYPE html>`, within EVIDENCE_LIMIT bytes, comments, doctypes and
    /// whitespace not counted.
    fn looks_like_html(&self) -> bool {
        return self
            .head_locator
            .significant_html_evidence()
            .is_some_and(|start| return start < EVIDENCE_LIMIT);
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

/// The head window, once decided on, with what of the incoming chunk follows it and whether it is
/// part of that chunk, given `public_ref`, what `incomplete_data` held: nothing is held back anymore.
fn take_head_window<'a>(public_ref: &'a [u8], input: HeadWindow<'a>) -> (&'a [u8], &'a [u8], bool) {
    return match input {
        HeadWindow::InPlace { chunk } => (chunk, b"", true),
        HeadWindow::Buffered { tail } => (public_ref, tail, false),
    };
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

    /// Generates documents from `parts` and checks the snippet lands right after the first part
    /// holding a `<head>` tag, if `injects` lets it in at all, whatever the chunking.
    fn fuzz_head_open_tag(prefix: &str, injects: bool) {
        struct Part {
            string: &'static str,
            has_head: bool,
            /// A start tag the head cannot follow, or text, which ends the search for one.
            ends_search: bool,
        }

        impl Part {
            fn new(string: &'static str, has_head: bool) -> Self {
                return Self {
                    string,
                    has_head,
                    ends_search: false,
                };
            }

            fn body_content(string: &'static str) -> Self {
                return Self {
                    string,
                    has_head: false,
                    ends_search: true,
                };
            }
        }

        // None of these parts can combine into a tag across a boundary, so a `<head>` can only ever
        // come from a single `has_head` part, and no part can follow it within the head as a meta
        // the snippet has to stay behind.
        let parts: [Part; 19] = [
            Part::new("<head>", true),
            Part::new("<head >", true),
            Part::new("<HeAd>", true),
            Part::new("<HEAD>", true),
            Part::body_content("<h ead>"),
            Part::body_content("<he ad>"),
            Part::body_content("<header>"),
            // Text ends the search for the `<head>` tag, as body content does: the browser closes
            // the head on it. Whitespace does not, nor does a reference to it.
            Part::body_content("h>"),
            Part::body_content("/>"),
            Part::body_content(">"),
            Part::new(" ", false),
            Part::new("&#32;", false),
            Part::body_content("foo"),
            Part::body_content("bar"),
            Part::body_content("&nbsp;"),
            Part::body_content("😊"),
            Part::body_content("网络"),
            // Metas the snippet need not stay behind: it still goes right after the `<head>` tag.
            Part::new("<meta name=description content=x>", false),
            Part::new("<meta name=dd-trace-id content=1>", false),
        ];

        let mut rng = rand::rng();

        for _ in 0..1000 {
            let mut input = String::from(prefix);
            let mut expected = String::from(prefix);
            let mut has_head = false;
            let mut search_ended = false;

            let parts_count = rng.random_range(0usize..20);
            for part in parts.choose_multiple(&mut rng, parts_count) {
                input.push_str(part.string);
                expected.push_str(part.string);

                if injects && !has_head && !search_ended && part.has_head {
                    expected.push_str("{{SNIPPET}}");
                    has_head = true;
                }
                search_ended |= part.ends_search;
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

    /// A `<head>` start tag within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes.
    mod head_in_the_window {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn basic() {
            test_injector(
                ["<head", ">abc</he", "ad>def"],
                "<head>{{SNIPPET}}abc</head>def",
            );
            test_injector([" \n<head>def"], " \n<head>{{SNIPPET}}def");
            test_injector(["<HeAd lang=en>def"], "<HeAd lang=en>{{SNIPPET}}def");
        }

        #[test]
        fn after_the_last_relevant_meta() {
            test_injector(
                ["<html><head><meta charset='utf-8'><meta name=viewport content=x><title>t</title><meta http-equiv=X-UA-Compatible content=IE=edge><meta name=x></head><body><meta name=y>"],
                "<html><head><meta charset='utf-8'><meta name=viewport content=x><title>t</title><meta http-equiv=X-UA-Compatible content=IE=edge>{{SNIPPET}}<meta name=x></head><body><meta name=y>",
            );
            test_injector(
                ["<head><link rel=icon><meta charset=utf-8><script>1</script><style>a{}</style>x</head>"],
                "<head><link rel=icon><meta charset=utf-8>{{SNIPPET}}<script>1</script><style>a{}</style>x</head>",
            );
            // Other metas, the tracer's among them, do not hold the snippet back.
            test_injector(
                ["<html><head><meta name=dd-trace-id content=1><meta name=dd-trace-time content=2><meta name=description content=x></head>"],
                "<html><head>{{SNIPPET}}<meta name=dd-trace-id content=1><meta name=dd-trace-time content=2><meta name=description content=x></head>",
            );
        }

        /// The head ends at the first start tag it cannot hold, and so does the walk.
        #[test]
        fn stops_at_a_tag_the_head_cannot_hold() {
            test_injector(
                ["<head><meta charset=utf-8><div><meta charset=utf-8>"],
                "<head><meta charset=utf-8>{{SNIPPET}}<div><meta charset=utf-8>",
            );
            test_injector(
                ["<head><head><meta name=a>"],
                "<head>{{SNIPPET}}<head><meta name=a>",
            );
            test_injector(
                ["<head><body><meta name=a>"],
                "<head>{{SNIPPET}}<body><meta name=a>",
            );
        }

        /// Past the window, the head is only walked while it is a run of `<meta>` and `<title>`
        /// tags: text or any other tag ends it.
        #[test]
        fn ignores_meta_tags_past_the_window() {
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            test_injector(
                [format!("<head>{filler}<meta charset='utf-8'></head>def")],
                &format!("<head>{{{{SNIPPET}}}}{filler}<meta charset='utf-8'></head>def"),
            );
            let script = format!("<script>{filler}</script>");
            test_injector(
                [format!("<head><meta charset=utf-8>{script}<meta http-equiv=x-ua-compatible>")],
                &format!(
                    "<head><meta charset=utf-8>{{{{SNIPPET}}}}{script}<meta http-equiv=x-ua-compatible>"
                ),
            );
        }

        /// The first encoding declaration of the document lies past where the snippet would go: any
        /// point would push it further, so the document is left alone.
        #[test]
        fn declines_before_an_encoding_declaration_out_of_the_head() {
            test_injector(
                ["<head></head><body><meta charset=utf-8>"],
                "<head></head><body><meta charset=utf-8>           ",
            );
            // Unless one already lies behind the point.
            test_injector(
                ["<head><meta charset=utf-8></head><body><meta charset=utf-8>"],
                "<head><meta charset=utf-8>{{SNIPPET}}</head><body><meta charset=utf-8>",
            );
        }

        /// The `<head>` tag starts within the window but ends past it: the snippet goes right after it.
        #[test]
        fn head_tag_straddling_the_window_end() {
            let filler = " ".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 3);
            test_injector(
                [filler.clone() + "<head><meta name=a>"],
                &(filler.clone() + "<head>{{SNIPPET}}<meta name=a>"),
            );
            test_injector(
                [filler.clone(), String::from("<he"), String::from("ad>x")],
                &(filler + "<head>{{SNIPPET}}x"),
            );
        }

        /// The chunk that completes the window carries more data than the window needs: the rest of
        /// it has to be emitted, not buffered.
        #[test]
        fn emits_the_rest_of_the_chunk_completing_the_window() {
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - "<head>".len());
            let input = [String::from("<head>"), filler.clone() + "tail data"];
            let expected = String::from("<head>{{SNIPPET}}") + &filler + "tail data";

            test_injector(input, &expected);
        }

        #[test]
        fn only_injects_once() {
            test_injector(
                ["<head>abc</head>def<head>ghi"],
                "<head>{{SNIPPET}}abc</head>def<head>ghi",
            );
        }

        #[test]
        fn ending_again_after_injecting_at_eof_emits_nothing() {
            for padded in [true, false] {
                let mut injector =
                    Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
                injector.write(b"<head>abc");
                assert!(injector.end().injected);

                let second_end = match padded {
                    true => injector.end(),
                    false => injector.end_without_padding(),
                };
                assert_eq!(second_end.length, 0);
            }
        }

        #[test]
        fn fuzzy() {
            fuzz_head_open_tag("", true);
        }
    }

    /// The head is a run of `<meta>` and `<title>` tags past the first DOCUMENT_CHARSET_SCAN_LIMIT
    /// bytes: the walk goes on, up to WALK_LIMIT, for an `X-UA-Compatible` declaration may follow.
    mod meta_run_past_the_window {
        use super::*;
        use pretty_assertions::assert_eq;

        const CHARSET: &str = "<meta charset=utf-8>";
        const COMPATIBLE: &str = "<meta http-equiv=X-UA-Compatible content=IE=edge>";

        /// Non relevant metas, one per line, at least `length` bytes of them.
        fn metas(length: usize) -> String {
            let mut metas = String::new();
            while metas.len() < length {
                metas.push_str(&format!(
                    "<meta name=description content=\"{}\">\n",
                    metas.len()
                ));
            }
            return metas;
        }

        /// Checks `template`, with `{{SNIPPET}}` where the snippet goes, whatever the chunking.
        fn check(template: &str) {
            let document = template.replace("{{SNIPPET}}", "");
            let expected = match template.contains("{{SNIPPET}}") {
                true => String::from(template),
                false => format!("{template}           "),
            };
            let bytes = document.as_bytes();
            test_injector([bytes], &expected);
            for size in [1, 7, 100, 1000, 1024, 1025, 3000] {
                test_injector(bytes.chunks(size), &expected);
            }
            for point in [
                1,
                1023,
                1024,
                1025,
                2047,
                2048,
                4096,
                10 * 1024 - 1,
                10 * 1024,
            ] {
                if point < bytes.len() {
                    test_injector([&bytes[..point], &bytes[point..]], &expected);
                }
            }
        }

        #[test]
        fn x_ua_compatible_after_a_long_meta_run() {
            let run = metas(3000);
            check(&format!(
                "<!DOCTYPE html><html><head>{CHARSET}\n<title>t</title>\n{run}{COMPATIBLE}{{{{SNIPPET}}}}\n<link rel=icon></head><body></body></html>"
            ));
        }

        #[test]
        fn relevant_meta_cut_by_the_window_end() {
            // Within the run.
            let prefix = "<html><head>";
            let run = metas(DOCUMENT_CHARSET_SCAN_LIMIT - prefix.len() - 60);
            let run = &run[..run.rfind("<meta").unwrap()];
            let padding = " ".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - prefix.len() - run.len() - 10);
            check(&format!(
                "{prefix}{run}{padding}{COMPATIBLE}{{{{SNIPPET}}}}<link rel=icon></head>"
            ));
            // Past the end of the run: a meta starting within the window is walked all the same.
            let href = "x".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - prefix.len() - 30);
            check(&format!(
                "{prefix}<link href={href}>{COMPATIBLE}{{{{SNIPPET}}}}<meta name=a></head>"
            ));
            // But not one starting past it.
            check(&format!(
                "{prefix}{{{{SNIPPET}}}}<link href={href}><meta name=a content=xxxxxxxxxxxxxxxxxxxxxxx>{COMPATIBLE}</head>"
            ));
        }

        #[test]
        fn run_still_going_at_the_walk_limit() {
            // The best point found within the limit stands, even one the walk found past the window.
            let run = metas(WALK_LIMIT);
            check(&format!(
                "<html><head>{CHARSET}{{{{SNIPPET}}}}{run}{COMPATIBLE}</head>"
            ));
            let run = metas(4000);
            let more = metas(WALK_LIMIT);
            check(&format!(
                "<html><head>{run}{COMPATIBLE}{{{{SNIPPET}}}}{more}{COMPATIBLE}</head>"
            ));
            // A meta tag the limit cuts is not walked.
            let run = metas(WALK_LIMIT - 30);
            let run = &run[..run.rfind("<meta").unwrap()];
            let padding = " ".repeat(WALK_LIMIT - "<html><head>".len() - run.len() - 10);
            check(&format!(
                "<html><head>{{{{SNIPPET}}}}{run}{padding}{COMPATIBLE}</head>"
            ));
        }

        #[test]
        fn tracer_and_seo_metas_stay_behind_the_snippet() {
            let run = metas(2000);
            check(&format!(
                "<html><head>{CHARSET}{{{{SNIPPET}}}}{run}<meta name=dd-trace-id content=1><meta name=dd-trace-time content=2><meta property=og:title content=t></head>"
            ));
        }

        #[test]
        fn declarations_that_are_no_tags_past_the_window() {
            let run = metas(2000);
            check(&format!(
                "<html><head>{CHARSET}{{{{SNIPPET}}}}{run}<!-- {COMPATIBLE} --><title>{COMPATIBLE}</title><meta name=a content='{COMPATIBLE}'></head>"
            ));
            // An encoding declaration in raw text past the window is out of the prescan's reach.
            check(&format!(
                "<html><head>{{{{SNIPPET}}}}{run}<title>{CHARSET}</title></head>"
            ));
        }

        #[test]
        fn text_or_any_other_tag_ends_the_run() {
            let run = metas(2000);
            for end in [
                "text",
                "<link rel=icon>",
                "<script></script>",
                "<base href=/>",
            ] {
                check(&format!(
                    "<html><head>{CHARSET}{{{{SNIPPET}}}}{run}{end}{COMPATIBLE}</head>"
                ));
            }
            // Whitespace, comments and stray end tags do not.
            check(&format!(
                "<html><head>{CHARSET}{run} \n\t<!-- c --></p><title>t</title>{COMPATIBLE}{{{{SNIPPET}}}}</head>"
            ));
        }

        #[test]
        fn mixed_case_and_bogus_charset() {
            let run = metas(2000);
            check(&format!(
                "<HTML><HEAD>{run}<META HTTP-EQUIV=\"x-ua-compatible\" CONTENT=\"IE=edge\">{{{{SNIPPET}}}}<META CHARSET=BOGUS></HEAD>"
            ));
            check(&format!(
                "<html><head>{{{{SNIPPET}}}}<meta charset=bogus>{run}</head>"
            ));
        }

        #[test]
        fn late_head_tag() {
            // Starting within the window, it is walked from, past the window as well.
            let comment = format!("<!-- {} -->", "c".repeat(900));
            let run = metas(1000);
            check(&format!(
                "<html>{comment}<head>{run}{COMPATIBLE}{{{{SNIPPET}}}}<link rel=icon></head>"
            ));
            // Starting past it, the snippet goes right after it.
            let comment = format!("<!-- {} -->", "c".repeat(DOCUMENT_CHARSET_SCAN_LIMIT));
            check(&format!(
                "<html>{comment}<head>{{{{SNIPPET}}}}{COMPATIBLE}</head>"
            ));
        }

        #[test]
        fn short_documents() {
            check(&format!("<head>{CHARSET}{{{{SNIPPET}}}}<meta name=a>"));
            check("<head>{{SNIPPET}}<meta name=a><title>t</title>");
            check(&format!("<head>{}{COMPATIBLE}{{{{SNIPPET}}}}", metas(500)));
            check("<head>{{SNIPPET}}<meta http-equiv=x-ua-compatible");
            check("<head><meta charset=utf-8");
        }

        #[test]
        fn holds_back_while_the_run_goes_on() {
            let mut injector =
                Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
            let run = metas(3000);
            // Walked again once twice as much is held back.
            let body = "b".repeat(3000);
            let document = format!("<html><head>{run}{COMPATIBLE}<link rel=icon>{body}");
            let bytes = document.as_bytes();
            let result = injector.write(&bytes[..2500]);
            assert_eq!(result.length, 0);
            assert_eq!(injector.held_back(), 2500);
            assert!(injector.write(&bytes[2500..]).injected);
            assert_eq!(injector.held_back(), 0);

            // Never past the limit, however the run goes on.
            let mut injector =
                Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
            let document = format!("<html><head>{}", metas(3 * WALK_LIMIT));
            for chunk in document.as_bytes().chunks(500) {
                injector.write(chunk);
                assert!(injector.held_back() < WALK_LIMIT);
            }
        }
    }

    /// No `<head>` start tag within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes.
    mod head_past_the_window {
        use super::*;
        use pretty_assertions::assert_eq;

        /// A comment long enough to fill the window, which holds no HTML evidence of its own, and does
        /// not count towards EVIDENCE_LIMIT either.
        fn filler() -> String {
            return format!("<!-- {} -->", "c".repeat(DOCUMENT_CHARSET_SCAN_LIMIT));
        }

        /// Markup past which no evidence is looked for: more than EVIDENCE_LIMIT bytes of `<link>`
        /// tags, which are no evidence, and which the head may hold.
        fn opaque_filler() -> String {
            return "<link rel=icon>".repeat(EVIDENCE_LIMIT / 15 + 1);
        }

        #[test]
        fn injects_after_the_head_tag_when_the_window_looks_like_html() {
            for evidence in [
                "<html>",
                "<!DOCTYPE html><HTML lang=en>",
                "<meta charset=utf-8>",
                "<!DOCTYPE html>",
                "<!doctype HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\">",
            ] {
                let input = format!("{evidence}{}<head><meta name=a></head>", filler());
                let expected = format!(
                    "{evidence}{}<head>{{{{SNIPPET}}}}<meta name=a></head>",
                    filler()
                );
                test_injector([input], &expected);
            }
        }

        #[test]
        fn does_not_inject_when_the_window_does_not_look_like_html() {
            // Tags within comments or raw text are no evidence, nor is another doctype.
            for prefix in [
                "",
                "<!DOCTYPE htmlx>",
                "<!-- <html> -->",
                "<script>'<html>'</script>",
                "<div><p>",
                "<metadata><htmlx>",
            ] {
                let input = format!("{prefix}{}<head></head>", filler());
                let expected = input.clone() + "           ";
                test_injector([input], &expected);
            }
        }

        /// Non-HTML content served as `text/html` passes through untouched.
        #[test]
        fn non_html_content() {
            let json = format!(
                "{{\"data\": \"{}\", \"html\": \"<head>\"}}",
                "x".repeat(1100)
            );
            test_injector([json.clone()], &(json + "           "));

            let text = "Lorem ipsum <head> dolor\n".repeat(10);
            let text = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT) + &text;
            test_injector([text.clone()], &(text + "           "));
        }

        /// Evidence that starts within the window counts, wherever it ends.
        #[test]
        fn evidence_straddling_the_window_end() {
            let filler = " ".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 2);
            test_injector(
                [filler.clone() + "<meta charset=utf-8> <head>y"],
                &(filler.clone() + "<meta charset=utf-8> <head>{{SNIPPET}}y"),
            );
            test_injector(
                [
                    filler.clone(),
                    String::from("<m"),
                    String::from("eta> <head>y"),
                ],
                &(filler.clone() + "<meta> <head>{{SNIPPET}}y"),
            );
            // A tag that starts there and turns out to be something else is no evidence.
            test_injector(
                [
                    filler.clone(),
                    String::from("<m"),
                    String::from("ark> <head>y"),
                ],
                &(filler.clone() + "<mark> <head>y           "),
            );
            // Whitespace does not count towards the limit, nor do comments: evidence past them is.
            let filler = " ".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            test_injector(
                [filler.clone() + "<html><head>y"],
                &(filler + "<html><head>{{SNIPPET}}y"),
            );
            // Evidence past EVIDENCE_LIMIT bytes of anything else is not.
            let filler = opaque_filler();
            test_injector(
                [filler.clone() + "<html><head>y"],
                &(filler + "<html><head>y           "),
            );
        }

        #[test]
        fn head_tag_split_one_byte_at_a_time() {
            let input = String::from("<html>") + &filler() + " \n\t<head lang=en>def";
            let expected = String::from("<html>") + &filler() + " \n\t<head lang=en>{{SNIPPET}}def";

            // One chunk to get past the window, then a chunk per remaining byte.
            let mut chunks = alloc::vec![String::from(&input[..DOCUMENT_CHARSET_SCAN_LIMIT])];
            chunks.extend(
                input[DOCUMENT_CHARSET_SCAN_LIMIT..]
                    .chars()
                    .map(String::from),
            );

            test_injector(chunks, &expected);
        }

        /// The `<head>` tag is in the rest of the chunk that completes the window.
        #[test]
        fn head_tag_in_the_rest_of_the_chunk_completing_the_window() {
            let filler = String::from("<html>") + &" ".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 7);
            let input = [filler.clone(), String::from("   <head>efg")];
            let expected = filler + "   <head>{{SNIPPET}}efg";

            test_injector(input, &expected);
        }

        #[test]
        fn no_head() {
            let input = String::from("<html>") + &filler() + "<body>hi</body></html>";
            test_injector([input.clone()], &(input + "           "));
        }

        /// Completing the window must not pull a whole large chunk into the buffers: everything past
        /// the window is emitted straight away, and nothing is held back past it.
        #[test]
        fn buffering_stays_bounded_by_the_window_size() {
            let mut injector =
                Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);

            injector.write(b"<html>");
            let big_chunk = alloc::vec![b' '; 8 * DOCUMENT_CHARSET_SCAN_LIMIT];
            let emitted: usize = injector
                .write(&big_chunk)
                .iter()
                .map(|slice| return slice.bytes.len())
                .sum();

            assert_eq!(emitted, "<html>".len() + big_chunk.len());
            assert_eq!(injector.held_back(), 0);
            assert_eq!(
                injector.injector_state,
                InjectorState::ScanningForHeadOpenTag
            );
        }

        /// Without evidence, the rest of the document is not even scanned.
        #[test]
        fn stops_scanning_without_evidence() {
            let mut injector =
                Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
            injector.write(opaque_filler().as_bytes());
            assert_eq!(injector.injector_state, InjectorState::PassingThrough);
        }

        #[test]
        fn fuzzy() {
            fuzz_head_open_tag(&(String::from("<html>") + &filler()), true);
            fuzz_head_open_tag(&opaque_filler(), false);
        }
    }

    #[test]
    fn no_head() {
        test_injector(["abc"], "abc           ");
        test_injector(["<html></html>"], "<html></html>           ");
        test_injector(["abc</head>def"], "abc</head>def           ");
    }

    #[test]
    fn empty() {
        test_injector::<&str>([], "           ");
        test_injector([""], "           ");
        test_injector(["", ""], "           ");
    }

    #[test]
    fn content_length_with_snippet() {
        let injector = Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
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

        fn rebuild(chunks: &[&[u8]]) -> Vec<u8> {
            let mut injector =
                Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag);
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
            assert_eq!(
                String::from_utf8(rebuild(&chunks)).unwrap(),
                String::from_utf8(rebuild(&[document])).unwrap(),
                "split at {:?}",
                split_points
            );
        }

        fn documents() -> Vec<Vec<u8>> {
            let mut long_head =
                String::from("<!DOCTYPE html><html><head><meta charset='utf-8'><title>");
            long_head.push_str(&"x".repeat(1500));
            long_head.push_str("</title></head><body>hi</body></html>");
            let mut head_past_window = String::from("<html><!-- ");
            head_past_window.push_str(&"c".repeat(1100));
            head_past_window.push_str(" --><head><title>t</title></head><body>hi</body></html>");
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

        // Every split within and around the window end and the `<head>` tag that follows it, where
        // the window hands over to the stream scan.
        #[test]
        fn around_the_window_end_and_head_tag() {
            for document in documents() {
                let Some(tag) = document.windows(6).position(|w| return w == b"<head>") else {
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

    fn test_injector<T: AsRef<[u8]> + core::fmt::Debug>(
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        return run_injector(
            || return Injector::with_strategy(b"{{SNIPPET}}", InjectionStrategy::HeadOpenTag),
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

    /// [`InjectionStrategy::HeadCloseTag`], the default.
    mod head_close_tag {
        use super::*;
        use pretty_assertions::assert_eq;

        /// Checks `document` comes out as `expected`, `{{SNIPPET}}` marking where the snippet goes
        /// (padding is added to an expected output without it), whatever the chunking: in one
        /// chunk, split in two anywhere, and one byte at a time.
        fn check(document: &str, expected: &str) {
            let padding = " ".repeat(b"{{SNIPPET}}".len());
            let expected = match expected.contains("{{SNIPPET}}") {
                true => String::from(expected),
                false => format!("{expected}{padding}"),
            };
            let bytes = document.as_bytes();
            run_injector(|| return Injector::new(b"{{SNIPPET}}"), [bytes], &expected);
            for point in 0..=bytes.len() {
                run_injector(
                    || return Injector::new(b"{{SNIPPET}}"),
                    [&bytes[..point], &bytes[point..]],
                    &expected,
                );
            }
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                bytes.chunks(1),
                &expected,
            );
        }

        #[test]
        fn is_the_default() {
            assert_eq!(
                InjectionStrategy::default(),
                InjectionStrategy::HeadCloseTag
            );
        }

        #[test]
        fn right_before_the_head_end_tag() {
            check(
                "<!DOCTYPE html><html><head><meta charset=utf-8><title>t</title></head><body>hi</body></html>",
                "<!DOCTYPE html><html><head><meta charset=utf-8><title>t</title>{{SNIPPET}}</head><body>hi</body></html>",
            );
            // Its name is matched case insensitively, and an end tag may carry whitespace or
            // attributes, which the tokenizer drops.
            check(
                "<HTML><HEAD><TITLE>t</TITLE></HEAD >",
                "<HTML><HEAD><TITLE>t</TITLE>{{SNIPPET}}</HEAD >",
            );
            check(
                "<html><head></head foo='>'>",
                "<html><head>{{SNIPPET}}</head foo='>'>",
            );
            // A `<head>` start tag, the HTML spec lets a document omit.
            check(
                "<!DOCTYPE html><html><meta charset=utf-8><title>t</title></head><body>",
                "<!DOCTYPE html><html><meta charset=utf-8><title>t</title>{{SNIPPET}}</head><body>",
            );
            check(
                "<meta charset=utf-8><title>t</title></head>",
                "<meta charset=utf-8><title>t</title>{{SNIPPET}}</head>",
            );
        }

        #[test]
        fn behind_every_meta_tag() {
            check(
                "<html><head><meta http-equiv=Content-Security-Policy content=x><meta charset=utf-8><meta http-equiv=X-UA-Compatible content=IE=edge><link rel=icon></head>",
                "<html><head><meta http-equiv=Content-Security-Policy content=x><meta charset=utf-8><meta http-equiv=X-UA-Compatible content=IE=edge><link rel=icon>{{SNIPPET}}</head>",
            );
        }

        #[test]
        fn whatever_the_size_of_the_head() {
            // Past the window and the walk limit: inline scripts and styles, as large as they come.
            let script = format!("<script>{}</script>", "var a = '</head>';".repeat(1000));
            let style = format!("<style>{}</style>", "p { color: red }".repeat(1000));
            let head = format!("<html><head><meta charset=utf-8>{script}{style}<title>t</title>");
            let document = format!("{head}</head><body></body>");
            let expected = format!("{head}{{{{SNIPPET}}}}</head><body></body>");
            assert!(head.len() > 2 * WALK_LIMIT);
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                [&document],
                &expected,
            );
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                document.as_bytes().chunks(1000),
                &expected,
            );
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                document.as_bytes().chunks(7),
                &expected,
            );
        }

        #[test]
        fn not_written_in_text_or_attributes() {
            check(
                "<html><head><!-- </head> --><script>'</head>'</script><style>/* </head> */</style><title></head></title><noscript></head></noscript><link title='</head>'></head>",
                "<html><head><!-- </head> --><script>'</head>'</script><style>/* </head> */</style><title></head></title><noscript></head></noscript><link title='</head>'>{{SNIPPET}}</head>",
            );
            check(
                "<html><head></headx></header></head>",
                "<html><head></headx></header>{{SNIPPET}}</head>",
            );
        }

        #[test]
        fn only_in_a_document_that_looks_like_html() {
            // No `<html>`, `<meta>` or `<head>` start tag before it.
            check(
                "<title>t</title></head><body>",
                "<title>t</title></head><body>",
            );
            check("Some text </head> more text", "Some text </head> more text");
            check("{\"a\": \"</head>\"}", "{\"a\": \"</head>\"}");
            // Nor with text first, which closes the head the browser opens for it.
            let preamble = "x".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            let document = format!("{preamble}<html><head></head>");
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                [&document],
                &format!("{document}           "),
            );
            // A doctype is evidence, and neither comments nor whitespace count towards the limit: a
            // license banner or blank lines ahead of the document do not keep the snippet out.
            let banner = format!(
                "<!--\n{}-->\n",
                "  Licensed under the Apache License, Version 2.0\n".repeat(30)
            );
            check(
                &format!("<!DOCTYPE html>\n{banner}<html><head><title>t</title></head>"),
                &format!(
                    "<!DOCTYPE html>\n{banner}<html><head><title>t</title>{{{{SNIPPET}}}}</head>"
                ),
            );
            let blank = "\n".repeat(1500);
            check(
                &format!("{blank}<html><head><title>t</title></head>"),
                &format!("{blank}<html><head><title>t</title>{{{{SNIPPET}}}}</head>"),
            );
            check(
                "<!DOCTYPE html><title>t</title></head>",
                "<!DOCTYPE html><title>t</title>{{SNIPPET}}</head>",
            );
            // Past EVIDENCE_LIMIT bytes of anything else, the rest is not scanned.
            let links = "<link rel=icon>".repeat(EVIDENCE_LIMIT / 15 + 1);
            let document = format!("{links}<html><head></head>");
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                [&document],
                &format!("{document}           "),
            );
            // Evidence starting within it counts, wherever the `</head>` tag comes.
            let comment = format!("<!-- {} -->", "c".repeat(3 * DOCUMENT_CHARSET_SCAN_LIMIT));
            check(
                &format!("<html>{comment}<head>{comment}</head>"),
                &format!("<html>{comment}<head>{comment}{{{{SNIPPET}}}}</head>"),
            );
        }

        #[test]
        fn not_past_the_end_of_the_head() {
            // Body content closes the head: a `</head>` tag past it closes nothing.
            check(
                "<html><head><title>t</title><div>x</div></head>",
                "<html><head><title>t</title><div>x</div></head>",
            );
            check("<div><head></head>", "<div><head></head>");
            // So do these end tags.
            for end in ["</body>", "</html>", "</br>", "</BODY >"] {
                check(
                    &format!("<html><head><title>t</title>{end}</head>"),
                    &format!("<html><head><title>t</title>{end}</head>"),
                );
            }
            // Text closes the head too: the snippet would not go in it.
            check(
                "<html><head><title>t</title>stray text</head>",
                "<html><head><title>t</title>stray text</head>",
            );
            check("<html>text<head></head>", "<html>text<head></head>");
            check("<html><head>&nbsp;</head>", "<html><head>&nbsp;</head>");
            // A character reference to whitespace is whitespace, the `;` optional for a numeric one;
            // a reference to anything else is text.
            for whitespace in [
                "&#32;",
                "&#x20;",
                "&#X9;",
                "&Tab;",
                "&NewLine;",
                "&#10 ",
                "&#0013;",
            ] {
                check(
                    &format!("<html><head>{whitespace}</head>"),
                    &format!("<html><head>{whitespace}{{{{SNIPPET}}}}</head>"),
                );
            }
            for text in [
                "&#65;",
                "&nbsp;",
                "&Tab",
                "&#;",
                "&amp;",
                "&#x0000000000020;",
            ] {
                check(
                    &format!("<html><head>{text}</head>"),
                    &format!("<html><head>{text}</head>"),
                );
            }
            // A stray `<` is text.
            check(
                "<html><head><<meta name=a></head>",
                "<html><head><<meta name=a></head>",
            );
            check("<html><head><'</head>", "<html><head><'</head>");
            // Not whitespace, nor text within a raw text or RCDATA element, nor a byte order mark
            // starting the document.
            check(
                "<html>\n <head>\t<title> t </title>\r\n</head>",
                "<html>\n <head>\t<title> t </title>\r\n{{SNIPPET}}</head>",
            );
            check(
                "\u{feff}<!DOCTYPE html><html><head></head>",
                "\u{feff}<!DOCTYPE html><html><head>{{SNIPPET}}</head>",
            );
            // Other end tags are ignored, as the browser does in the head.
            check(
                "<html><head></p></div></head>",
                "<html><head></p></div>{{SNIPPET}}</head>",
            );
            // A CDATA section ends the search, as its end cannot be told.
            check(
                "<html><head><![CDATA[x]]></head>",
                "<html><head><![CDATA[x]]></head>",
            );
            // A document without one is passed through.
            check(
                "<html><head><title>t</title>",
                "<html><head><title>t</title>",
            );
        }

        #[test]
        fn not_within_a_template() {
            // Template content is inert: a `</head>` tag in one closes nothing, and neither text nor
            // body content in one ends the search.
            check(
                "<html><head><template></head><div>x</div></template><title>t</title></head>",
                "<html><head><template></head><div>x</div></template><title>t</title>{{SNIPPET}}</head>",
            );
            // Nor is a `<head>` tag in one the head.
            check(
                "<html><template><head></template><head></head>",
                "<html><template><head></template><head>{{SNIPPET}}</head>",
            );
        }

        #[test]
        fn holds_back_only_an_end_tag_cut_short() {
            let mut injector = Injector::new(b"{{SNIPPET}}");
            let result = injector.write(b"<html><head><title>t</title></he");
            assert!(!result.injected);
            assert_eq!(injector.held_back(), "</he".len());
            assert!(injector.write(b"ad>").injected);
            assert_eq!(injector.held_back(), 0);

            // Released as soon as it turns out to be another tag.
            let mut injector = Injector::new(b"{{SNIPPET}}");
            injector.write(b"<html><head></he");
            assert_eq!(injector.held_back(), "</he".len());
            injector.write(b"ader>");
            assert_eq!(injector.held_back(), 0);
        }

        #[test]
        fn releases_another_end_tag_as_soon_as_its_name_differs() {
            // However long it goes on, it is never held back past where its name differs from
            // `head`, so the cap never applies to it, whatever the chunking.
            let name = "x".repeat(2 * MAX_PENDING_END_TAG);
            check(
                &format!("<html><head></{name}></head>"),
                &format!("<html><head></{name}>{{{{SNIPPET}}}}</head>"),
            );
            let mut injector = Injector::new(b"{{SNIPPET}}");
            injector.write(b"<html><head></hx");
            assert_eq!(injector.held_back(), 0);
        }

        #[test]
        fn gives_up_on_an_end_tag_stuffed_with_attributes() {
            let attributes = "a".repeat(MAX_PENDING_END_TAG);
            let document = format!("<html><head></head {attributes}>");
            let mut injector = Injector::new(b"{{SNIPPET}}");
            for chunk in document.as_bytes().chunks(100) {
                injector.write(chunk);
                assert!(injector.held_back() <= MAX_PENDING_END_TAG);
            }
            run_injector(
                || return Injector::new(b"{{SNIPPET}}"),
                document.as_bytes().chunks(100),
                &format!("{document}           "),
            );
        }
    }
}
