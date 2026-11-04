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

use head_open_tag::{HeadOpenTag, HeadOpenTagLocator};

/// Window at the start of the document within which a character encoding declaration is honoured by
/// browsers, as per the HTML spec
/// (<https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>).
///
/// Injecting before a `<meta charset>` could push it out of this window and change how the document
/// is decoded, so the head is only walked within these bytes and no further. They are also the bytes
/// that tell whether the document is HTML at all.
const DOCUMENT_CHARSET_SCAN_LIMIT: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
enum InjectorState {
    /// Holding back the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, which decide where the snippet goes.
    BufferingHeadWindow,

    /// The window holds no `<head>` start tag but looks like an HTML document: stream-scanning for
    /// the `<head>` tag, to inject right after it.
    ScanningForHeadOpenTag,

    /// Nothing left to look for, either because the snippet was injected or because it never will
    /// be. Chunks are passed through untouched.
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
/// The snippet only ever goes within the `<head>` of the document:
/// * if the `<head>` start tag is within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes, right after
///   the last `<meta>` tag of the head within those bytes, or right after the `<head>` tag if there
///   is none (see [`parser::find_injection_point`] for the details);
/// * otherwise, if those bytes look like an HTML document, as they hold an `<html>` or a `<meta>`
///   start tag, right after the first `<head>` start tag that follows;
/// * otherwise not at all: the document passes through untouched, padded as [`Injector::end`]
///   describes. This keeps the snippet out of fragments, JSON, plain text and any other content
///   served as `text/html`, and out of documents that have no `<head>` tag.
///
/// The first DOCUMENT_CHARSET_SCAN_LIMIT bytes are held back until they are complete, which delays
/// the start of the response body by as much. Past them nothing is held back.
pub struct Injector<'snippet> {
    /// Snippet to inject.
    snippet: &'snippet [u8],

    /// Tracks the current state of the injection process.
    injector_state: InjectorState,

    /// Whether the snippet has been emitted. Sticky: once injected, we never inject again.
    injected: bool,

    /// Locator for the `<head>` start tag, over the head window and then the rest of the document.
    head_locator: HeadOpenTagLocator,

    /// Whether [`Injector::flush`] handed the head window back before it was complete. What it held
    /// is gone then, so no injection point within the window can be proven anymore.
    window_cut: bool,

    /// The head window as it accumulates, bounded by DOCUMENT_CHARSET_SCAN_LIMIT: a chunk only ever
    /// contributes the bytes that complete the window.
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
    pub fn new(snippet: &'snippet [u8]) -> Self {
        return Self {
            snippet,
            injector_state: InjectorState::BufferingHeadWindow,
            injected: false,
            head_locator: HeadOpenTagLocator::new(),
            window_cut: false,
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

            InjectorState::ScanningForHeadOpenTag => return self.scan_head_open_tag(chunk),

            InjectorState::PassingThrough | InjectorState::Ended => {
                return self.create_result(&[BytesSlice::new(chunk, true)]);
            }
        }
    }

    /// Hands back everything held back, for a server that has to send what it has now, a page
    /// streamed in pieces that waits on more data for instance, which would otherwise stall until
    /// the head window fills. Never injects, and past the window there is nothing to hand back.
    ///
    /// The window is given up in exchange: what it held is gone, so no injection point within it can
    /// be proven anymore. A `<head>` tag in what is handed back, or starting later within the window,
    /// is not injected after. One starting past the window still is, if the window looks like an
    /// HTML document, as when the window holds no `<head>` tag at all.
    pub fn flush(&mut self) -> Result<'_> {
        if self.injector_state != InjectorState::BufferingHeadWindow {
            return self.create_result(&[]);
        }
        self.window_cut = true;
        self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
        self.injector_state = match self.head_locator.scan(&self.incomplete_data_public_ref) {
            // Its injection point is in what goes out now.
            Some(_) => InjectorState::PassingThrough,
            None if self.may_find_injection_point() => InjectorState::ScanningForHeadOpenTag,
            None => InjectorState::PassingThrough,
        };
        return self.create_result(&[BytesSlice::new(&self.incomplete_data_public_ref, false)]);
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

            // Already ended: everything has been emitted and padded for, so emit nothing rather
            // than padding twice.
            InjectorState::Ended => return self.create_result(&[]),
        }
    }

    /// Decides where the snippet goes from the head window, and scans whatever of the chunk follows
    /// it for the `<head>` tag when the window holds none.
    ///
    /// The data following the window within the chunk is emitted by this call rather than buffered,
    /// so completing the window never requires holding more than the window itself.
    fn scan_head_window<'a>(&'a mut self, input: HeadWindow<'a>, eof: bool) -> Result<'a> {
        let (window, tail, from_incoming_chunk) = match input {
            HeadWindow::InPlace { window, tail } => (window, tail, true),
            HeadWindow::Buffered { tail } => {
                self.incomplete_data_public_ref = mem::take(&mut self.incomplete_data);
                (&self.incomplete_data_public_ref[..], tail, false)
            }
        };
        self.injector_state = InjectorState::PassingThrough;

        if let Some(head) = self.head_locator.scan(window) {
            if let Some(point) = parser::find_injection_point(window, head.end) {
                self.injected = true;
                return self.create_result(&[
                    BytesSlice::new(&window[..point], from_incoming_chunk),
                    BytesSlice::new(self.snippet, false),
                    BytesSlice::new(&window[point..], from_incoming_chunk),
                    BytesSlice::new(tail, true),
                ]);
            }
        } else if let Some(head) = self.head_locator.scan(tail) {
            // The locator is sequential, so scanning the window then the tail is the same as
            // scanning their concatenation; they are split only because they are emitted separately.
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

    /// Whether the snippet may go after `head`, a `<head>` tag found past the head window or ending
    /// past it: only if the tag starts within the window, or if the window looks like an HTML
    /// document.
    fn may_inject_after(&self, head: HeadOpenTag) -> bool {
        if self.window_cut {
            // Within a window cut short, the meta walk cannot tell where the snippet may go.
            return head.start >= DOCUMENT_CHARSET_SCAN_LIMIT && self.window_holds_html_evidence();
        }
        return head.start < DOCUMENT_CHARSET_SCAN_LIMIT || self.window_holds_html_evidence();
    }

    /// Whether a `<head>` tag found in what follows may still be injected after, see
    /// [`Injector::may_inject_after`]. Once it may not, scanning the rest of the document is pointless.
    fn may_find_injection_point(&self) -> bool {
        if !self.head_locator.is_searching() {
            return false;
        }
        // The window can still hold evidence while it has not all been scanned, which only happens
        // after a flush cut it short.
        return self.window_holds_html_evidence()
            || self.head_locator.scanned() < DOCUMENT_CHARSET_SCAN_LIMIT
            || self
                .head_locator
                .pending_tag_start()
                .is_some_and(|start| return start < DOCUMENT_CHARSET_SCAN_LIMIT);
    }

    fn window_holds_html_evidence(&self) -> bool {
        return self
            .head_locator
            .html_evidence()
            .is_some_and(|start| return start < DOCUMENT_CHARSET_SCAN_LIMIT);
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

    /// Generates documents from `parts` and checks the snippet lands right after the first part
    /// holding a `<head>` tag, if `injects` lets it in at all, whatever the chunking.
    fn fuzz_head_open_tag(prefix: &str, injects: bool) {
        struct Part {
            string: &'static str,
            has_head: bool,
        }

        impl Part {
            fn new(string: &'static str, has_head: bool) -> Self {
                return Self { string, has_head };
            }
        }

        // None of these parts can combine into a tag across a boundary, so a `<head>` can only ever
        // come from a single `has_head` part, and no part can follow it within the head as a meta.
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
            let mut input = String::from(prefix);
            let mut expected = String::from(prefix);
            let mut has_head = false;

            let parts_count = rng.random_range(0usize..20);
            for part in parts.choose_multiple(&mut rng, parts_count) {
                input.push_str(part.string);
                expected.push_str(part.string);

                if injects && !has_head && part.has_head {
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
            test_injector(["abc<head>def"], "abc<head>{{SNIPPET}}def");
            test_injector(["<HeAd lang=en>def"], "<HeAd lang=en>{{SNIPPET}}def");
        }

        #[test]
        fn after_the_last_meta() {
            test_injector(
                ["<html><head><meta charset='utf-8'><meta name=viewport content=x><title>t</title><meta name=x></head><body><meta name=y>"],
                "<html><head><meta charset='utf-8'><meta name=viewport content=x><title>t</title><meta name=x>{{SNIPPET}}</head><body><meta name=y>",
            );
            test_injector(
                ["<head><link rel=icon><meta name=a><script>1</script><style>a{}</style>x</head>"],
                "<head><link rel=icon><meta name=a>{{SNIPPET}}<script>1</script><style>a{}</style>x</head>",
            );
        }

        /// The head ends at the first start tag it cannot hold, and so does the walk.
        #[test]
        fn stops_at_a_tag_the_head_cannot_hold() {
            test_injector(
                ["<head><meta name=a><div><meta name=b>"],
                "<head><meta name=a>{{SNIPPET}}<div><meta name=b>",
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

        /// What lies past the window is never walked, so the snippet can never push an encoding
        /// declaration out of it.
        #[test]
        fn ignores_meta_tags_past_the_window() {
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            test_injector(
                [format!("<head>{filler}<meta charset='utf-8'></head>def")],
                &format!("<head>{{{{SNIPPET}}}}{filler}<meta charset='utf-8'></head>def"),
            );

            // A meta tag the window ends within is past it too.
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - "<head><meta name=a>".len() - 4);
            test_injector(
                [format!("<head><meta name=a>{filler}<meta name=b></head>")],
                &format!("<head><meta name=a>{{{{SNIPPET}}}}{filler}<meta name=b></head>"),
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
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 3);
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
                let mut injector = Injector::new(b"{{SNIPPET}}");
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

    /// No `<head>` start tag within the first DOCUMENT_CHARSET_SCAN_LIMIT bytes.
    mod head_past_the_window {
        use super::*;
        use pretty_assertions::assert_eq;

        /// A comment long enough to fill the window, which holds no HTML evidence of its own.
        fn filler() -> String {
            return format!("<!-- {} -->", "c".repeat(DOCUMENT_CHARSET_SCAN_LIMIT));
        }

        #[test]
        fn injects_after_the_head_tag_when_the_window_looks_like_html() {
            for evidence in [
                "<html>",
                "<!DOCTYPE html><HTML lang=en>",
                "<meta charset=utf-8>",
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
            // A doctype alone is no evidence, and neither are tags within comments or raw text.
            for prefix in [
                "",
                "<!DOCTYPE html>",
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
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 2);
            test_injector(
                [filler.clone() + "<meta charset=utf-8>x<head>y"],
                &(filler.clone() + "<meta charset=utf-8>x<head>{{SNIPPET}}y"),
            );
            test_injector(
                [
                    filler.clone(),
                    String::from("<m"),
                    String::from("eta>x<head>y"),
                ],
                &(filler.clone() + "<meta>x<head>{{SNIPPET}}y"),
            );
            // A tag that starts there and turns out to be something else is no evidence.
            test_injector(
                [
                    filler.clone(),
                    String::from("<m"),
                    String::from("ark>x<head>y"),
                ],
                &(filler.clone() + "<mark>x<head>y           "),
            );
            // Nor is one that only starts past the window.
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            test_injector(
                [filler.clone() + "<html><head>y"],
                &(filler + "<html><head>y           "),
            );
        }

        #[test]
        fn head_tag_split_one_byte_at_a_time() {
            let input = String::from("<html>") + &filler() + "abc<head lang=en>def";
            let expected = String::from("<html>") + &filler() + "abc<head lang=en>{{SNIPPET}}def";

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
            let filler = String::from("<html>") + &"a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT - 7);
            let input = [filler.clone(), String::from("Bcd<head>efg")];
            let expected = filler + "Bcd<head>{{SNIPPET}}efg";

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
            let mut injector = Injector::new(b"{{SNIPPET}}");

            injector.write(b"<html>");
            let big_chunk = alloc::vec![b'b'; 8 * DOCUMENT_CHARSET_SCAN_LIMIT];
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
            let mut injector = Injector::new(b"{{SNIPPET}}");
            injector.write(filler().as_bytes());
            assert_eq!(injector.injector_state, InjectorState::PassingThrough);
        }

        #[test]
        fn fuzzy() {
            fuzz_head_open_tag(&(String::from("<html>") + &filler()), true);
            fuzz_head_open_tag(&filler(), false);
        }
    }

    /// [`Injector::flush`] hands the held-back window out, and gives up any `<head>` within it.
    mod flush {
        use super::*;
        use pretty_assertions::assert_eq;

        /// Writes `before`, flushes, writes `after` and ends the document, checking every result
        /// carries the bytes it should; returns the output.
        fn flushed(before: &str, after: &str) -> String {
            let mut injector = Injector::new(b"{{SNIPPET}}");
            let mut output = Vec::new();
            let collect = |output: &mut Vec<u8>, result: Result<'_>| {
                for slice in result.iter() {
                    output.extend_from_slice(slice.bytes);
                }
            };
            collect(&mut output, injector.write(before.as_bytes()));
            let held = injector.held_back();
            let flush = injector.flush();
            assert!(!flush.injected);
            assert!(flush.iter().all(|slice| return !slice.from_incoming_chunk));
            assert_eq!(
                flush
                    .iter()
                    .map(|slice| return slice.bytes.len())
                    .sum::<usize>(),
                held
            );
            collect(&mut output, flush);
            assert_eq!(injector.held_back(), 0);
            collect(&mut output, injector.write(after.as_bytes()));
            assert_eq!(injector.held_back(), 0, "nothing is held back past a flush");
            collect(&mut output, injector.end_without_padding());
            return String::from_utf8(output).unwrap();
        }

        #[test]
        fn hands_the_window_out() {
            assert_eq!(flushed("<html>", ""), "<html>");
            // Past the window, there is nothing left to hand out.
            let mut injector = Injector::new(b"{{SNIPPET}}");
            injector.write("a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT).as_bytes());
            assert_eq!(injector.flush().length, 0);
        }

        #[test]
        fn injects_after_a_head_tag_past_the_window() {
            let comment = format!("<!-- {} -->", "c".repeat(DOCUMENT_CHARSET_SCAN_LIMIT));
            assert_eq!(
                flushed("<html>", &format!("{comment}<head><meta name=a>")),
                format!("<html>{comment}<head>{{{{SNIPPET}}}}<meta name=a>")
            );
            // The evidence may come after the flush, as long as it is within the window.
            assert_eq!(
                flushed("<!DOCTYPE html>", &format!("<html>{comment}<head>")),
                format!("<!DOCTYPE html><html>{comment}<head>{{{{SNIPPET}}}}")
            );
        }

        #[test]
        fn gives_up_a_head_tag_within_the_window() {
            // In what was handed out: its injection point is gone.
            assert_eq!(
                flushed("<html><head>", "<title>t</title>"),
                "<html><head><title>t</title>"
            );
            // After the flush but within the window: a charset declaration may follow it there.
            assert_eq!(
                flushed("<html>", "<head><meta charset=utf-8>"),
                "<html><head><meta charset=utf-8>"
            );
        }

        #[test]
        fn does_not_inject_without_html_evidence() {
            let filler = "a".repeat(DOCUMENT_CHARSET_SCAN_LIMIT);
            assert_eq!(
                flushed("{\"a\": 1", &format!("{filler}<head>")),
                format!("{{\"a\": 1{filler}<head>")
            );
        }

        #[test]
        fn pads_when_ended_with_padding() {
            let mut injector = Injector::new(b"{{SNIPPET}}");
            injector.write(b"<html>");
            injector.flush();
            let end = injector.end();
            assert!(!end.injected);
            assert_eq!(
                end.iter()
                    .map(|slice| return slice.bytes.len())
                    .sum::<usize>(),
                11
            );
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
        let injector = Injector::new(b"{{SNIPPET}}");
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
            let mut injector = Injector::new(b"{{SNIPPET}}");
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
