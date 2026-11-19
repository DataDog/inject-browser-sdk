// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! Streaming locator for where the snippet goes: the first `<head>` start tag, the first `</head>`
//! end tag, or the point where the browser closes the head without one. See
//! [`HeadOpenTagLocator`].

use memchr::{memchr, memchr2, memchr3};

/// What a [`HeadOpenTagLocator`] looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// The first `<head>` start tag, to inject right after it.
    HeadStart,
    /// The first `</head>` end tag, to inject right before it.
    HeadEnd,
}

/// Where the snippet goes, as found by [`HeadOpenTagLocator::scan`].
///
/// Either target also stops where the browser closes the head without the tag looked for: at body
/// content (`<body>`, `<div>`, `<img>`…), at text other than whitespace, or at a `</body>`,
/// `</html>` or `</br>` end tag (and a `</head>` one, looking for `<head>`). The snippet goes right
/// before that point, which the browser still parses within the head, provided the document is one
/// (see [`HeadOpenTagLocator::strong_evidence`]): `breaks` is set then.
///
/// When the head closes before any such evidence, and does so on a tag, the document may be one
/// whose head an earlier error closed, a server warning printed ahead of it for instance: the
/// locator then reads on as the browser does, within the body, and stops right before the first
/// `<html>` or `<body>` start tag, `breaks` set, or gives up at foreign content (`<svg>`,
/// `<math>`), which it does not follow, or at a `<frameset>`, where the snippet would be dropped.
/// A `<head>` tag is not enough there: partial responses hold one for a client to merge into the
/// page's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeadOpenTag {
    /// Offset of its `<` from the start of the document, across every chunk scanned: where the
    /// snippet goes when it goes before it. For a break on text, the offset of that text.
    pub(super) start: usize,

    /// Index just past its `>`, within the chunk the match was completed in. Only meaningful for a
    /// `<head>` start tag, which the snippet goes after.
    pub(super) end: usize,

    /// Whether the browser closes the head at `start`, rather than the tag being the one looked for:
    /// the snippet goes right before `start` then.
    pub(super) breaks: bool,
}

/// Longest tag name the locator has to tell apart, `plaintext`.
const MAX_TAG_NAME: usize = 9;

/// Longest sequence raw text content is matched against: `</noscript` and its delimiter.
const MAX_RECENT: usize = 11;

/// Longest character reference read after its `&`, to tell a whitespace one: `#x0000000020` and the
/// like are text then, rather than held without bound.
const MAX_REFERENCE: usize = 12;

/// How many bytes of a `<!` declaration are kept, to tell a `<!DOCTYPE html>` one.
const MAX_DECLARATION: usize = 16;

/// Longest attribute name of the `<html>` tag kept, to tell an AMP document: `amp4email`.
const MAX_ATTRIBUTE: usize = 9;

/// The start tags that may come before the `<head>` tag without the browser ignoring it: `html`,
/// and the ones the head may hold, which the browser moves into it when they come first.
const MAY_PRECEDE_HEAD: &[&[u8]] = &[
    b"html",
    b"base",
    b"basefont",
    b"bgsound",
    b"link",
    b"meta",
    b"noframes",
    b"noscript",
    b"script",
    b"style",
    b"template",
    b"title",
];

/// The end tags the `in head` and `before head` insertion modes do not ignore: each closes the
/// head, implicitly but for `</head>` within it.
const CLOSES_HEAD: &[&[u8]] = &[b"head", b"body", b"html", b"br"];

/// The attributes that make an `<html>` tag the root of an AMP document, whose rules forbid any
/// script but its own: the snippet is kept out of one.
const AMP_ATTRIBUTES: &[&[u8]] = &[
    b"amp",
    "\u{26A1}".as_bytes(),
    b"amp4email",
    "\u{26A1}4email".as_bytes(),
    b"amp4ads",
    "\u{26A1}4ads".as_bytes(),
];

/// Elements whose content is text rather than markup, so a `<head>` written within one is not a tag.
///
/// `noscript` is among them as browsers run with scripting enabled, and so does lol_html.
const RAW_TEXT_ELEMENTS: &[&[u8]] = &[
    b"script",
    b"style",
    b"title",
    b"textarea",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
    b"noscript",
];

/// Where within a tag the locator is reading, following the tokenizer states of the HTML spec
/// (<https://html.spec.whatwg.org/multipage/parsing.html#tag-name-state>), which decide whether a
/// `>` closes the tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagPosition {
    TagName,
    BeforeAttributeName,
    /// An attribute name, or the whitespace that may follow it before its `=`.
    AttributeName,
    BeforeAttributeValue,
    UnquotedAttributeValue,
    QuotedAttributeValue(u8),
    /// Just past a `/`, where only a `>` ends the tag and anything else starts an attribute.
    SelfClosing,
}

/// The state script content can be in, see the `ScriptState` of the parser module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptState {
    Data,
    Escaped,
    DoubleEscaped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    /// Just past a `<`.
    TagOpen,
    /// Just past a `</`.
    EndTagOpen,
    Tag {
        closing: bool,
        position: TagPosition,
    },
    /// Just past a `<!`, holding how many of the dashes opening a comment were read.
    MarkupDeclarationOpen {
        dashes: u8,
    },
    /// Within a comment: `run` is the number of dashes just read, capped at two, `content_run` the
    /// part of them that is comment content rather than the `<!--` opener, and `bang` whether a `!`
    /// followed two content dashes.
    Comment {
        run: u8,
        content_run: u8,
        bang: bool,
    },
    /// A doctype, a `<?…>`, or any `<!…>` that opens no comment: all end at the very next `>`. So
    /// does a `<![CDATA[` out of foreign content, which the tokenizer reads as a bogus comment.
    BogusComment,
    /// Within the content of the raw text element named [`HeadOpenTagLocator::name`].
    RawText {
        script: ScriptState,
    },
    /// Past a `<plaintext>` tag, which nothing ends.
    Plaintext,
    /// A character reference in text, past its `&` (at `markup_start`): one that decodes to
    /// whitespace keeps the head open, any other is text.
    CharacterReference {
        name: [u8; MAX_REFERENCE],
        length: usize,
    },
    /// Where the snippet goes was found; nothing is left to look for.
    Found,
    /// Nothing is left to look for: the head closed in a document that does not look like one, the
    /// body reached foreign content before any evidence, a template holds foreign content, or the
    /// document is an AMP one.
    PastHead,
}

/// Stateful object used to locate where the snippet goes in a document fed in chunks: see
/// [`Target`] and [`HeadOpenTag`].
///
/// It follows the HTML tokenizer closely enough to tell a real tag from one written within a
/// comment, an attribute value, or the content of a raw text element (`<script>`, `<title>`…), and
/// carries its state across chunks. Some points can only be told once the bytes that follow them
/// are read, see [`HeadOpenTagLocator::pending_point_start`].
///
/// It also records whether the document looks like HTML: see [`HeadOpenTagLocator::html_evidence`].
pub(super) struct HeadOpenTagLocator {
    target: Target,

    state: State,

    /// Number of bytes scanned so far, across every chunk.
    offset: usize,

    /// Offset of the `<` that started the markup being read, or of the `&` of a character
    /// reference.
    markup_start: usize,

    /// Lower case name of the tag being read, or of the raw text element whose content is being
    /// read. Longer names are cut to [`MAX_TAG_NAME`] bytes, with `name_overflow` set.
    name: [u8; MAX_TAG_NAME],
    name_length: usize,
    name_overflow: bool,

    /// Whether the start tag being read was already told apart by its name: it is once its name is
    /// complete, or once it outgrows every name that matters.
    name_checked: bool,

    /// The last bytes of raw text content, lower case, to match its closing tag across chunks.
    recent: [u8; MAX_RECENT],
    recent_length: usize,

    /// Offset of the `<` of the first `<html>` or `<meta>` start tag.
    html_evidence: Option<usize>,

    /// Where the first `<html>`, `<meta>` start tag or `<!DOCTYPE html>` starts, counting only the
    /// bytes that are not [`HeadOpenTagLocator::free`].
    significant_html_evidence: Option<usize>,

    /// Where the first `<head>` start tag, or a `</head>` end tag found looking for one, starts,
    /// counted the same way.
    significant_head_start: Option<usize>,

    /// Whether a `<!DOCTYPE html>`, an `<html>` or a `<head>` start tag was read out of any template:
    /// the evidence that lets the snippet go where the head closes implicitly. A `<meta>` tag alone
    /// is not enough there: fragments start with one too.
    strong_evidence: bool,

    /// Bytes that tell nothing about whether the document is HTML, scanned so far: comments,
    /// bogus comments and doctypes, once ended, and whitespace in text. A license banner in a
    /// comment, or blank lines, do not push the evidence out of reach.
    free: usize,

    /// How many bytes of the byte order mark a UTF-8 document may start with have been read, all
    /// of the document so far while it is shorter than the mark: whether they are the mark, or
    /// text, takes what follows to tell when a chunk ends within it. See [`Self::text_start`].
    mark_length: usize,

    /// The first bytes of the `<!` declaration being read, lower case, and whether the markup is one.
    declaration: [u8; MAX_DECLARATION],
    declaration_length: usize,
    declaration_open: bool,

    /// Offset of the `<` of the first `<head>` start tag, looking for the `</head>` one.
    head_start: Option<usize>,

    /// How many `<template>` elements are open. Their content is inert: a `<head>` or `</head>`
    /// tag, body content or text within one changes nothing outside it.
    template_depth: usize,

    /// Whether the head closed before any evidence, on a tag: reading on within the body, for an
    /// `<html>` or `<body>` start tag. See [`HeadOpenTag`].
    body_mode: bool,

    /// Offset just past the last doctype or `<html>` start tag that only comments,
    /// doctypes and whitespace precede, and whether no other markup was read yet: the earliest
    /// point the snippet may go within a head whose start tag is omitted.
    prologue_end: usize,
    prologue_open: bool,

    /// The attribute of the `<html>` tag being read, lower case, and whether whitespace ended its
    /// name: any byte but `=` starts another one then.
    attribute: [u8; MAX_ATTRIBUTE],
    attribute_length: usize,
    attribute_overflow: bool,
    attribute_ended: bool,

    /// Whether the `<html>` tag being read is the root of an AMP document.
    amp: bool,

    /// Where the snippet goes and whether the head closes there, once found.
    found_start: usize,
    /// Where the markup found starts, counting only significant bytes, see
    /// [`HeadOpenTagLocator::significant_scanned`].
    found_significant: usize,
    found_breaks: bool,
}

impl HeadOpenTagLocator {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        return Self::with_target(Target::HeadStart);
    }

    pub(super) fn with_target(target: Target) -> Self {
        return Self {
            target,
            state: State::Data,
            offset: 0,
            markup_start: 0,
            name: [0; MAX_TAG_NAME],
            name_length: 0,
            name_overflow: false,
            name_checked: false,
            recent: [0; MAX_RECENT],
            recent_length: 0,
            html_evidence: None,
            significant_html_evidence: None,
            significant_head_start: None,
            strong_evidence: false,
            free: 0,
            mark_length: 0,
            declaration: [0; MAX_DECLARATION],
            declaration_length: 0,
            declaration_open: false,
            head_start: None,
            template_depth: 0,
            body_mode: false,
            prologue_end: 0,
            prologue_open: true,
            attribute: [0; MAX_ATTRIBUTE],
            attribute_length: 0,
            attribute_overflow: false,
            attribute_ended: false,
            amp: false,
            found_start: 0,
            found_significant: 0,
            found_breaks: false,
        };
    }

    /// Offset of the first `<html>` or `<meta>` start tag scanned, if any: the sign that the document
    /// is HTML rather than a fragment or some other content served as `text/html`.
    #[cfg(test)]
    pub(super) fn html_evidence(&self) -> Option<usize> {
        return self.html_evidence;
    }

    /// Number of bytes scanned so far, across every chunk.
    pub(super) fn offset(&self) -> usize {
        return self.offset;
    }

    /// Offset just past the doctype or `<html>` start tag that opens the document, if only comments,
    /// doctypes and whitespace precede it: where the snippet may go at the earliest within a head
    /// whose `<head>` tag is omitted, rather than before the doctype, which would trigger quirks
    /// mode. 0 when the document opens with neither.
    pub(super) fn prologue_end(&self) -> usize {
        return self.prologue_end;
    }

    /// Where the first `<html>` or `<meta>` start tag or `<!DOCTYPE html>` starts, counting only
    /// bytes that are not free (see [`HeadOpenTagLocator::significant_scanned`]).
    pub(super) fn significant_html_evidence(&self) -> Option<usize> {
        return self.significant_html_evidence;
    }

    /// [`HeadOpenTagLocator::significant_html_evidence`], or where the first `<head>` start tag
    /// starts, counted the same way: the HTML evidence of [`Target::HeadEnd`].
    pub(super) fn significant_evidence(&self) -> Option<usize> {
        return match (self.significant_html_evidence, self.significant_head_start) {
            (Some(html), Some(head)) => Some(html.min(head)),
            (html, head) => html.or(head),
        };
    }

    /// How many of the bytes scanned so far tell whether the document is HTML: all of them but
    /// comments, bogus comments, doctypes and whitespace in text. Within one of those still being
    /// read, where it started.
    pub(super) fn significant_scanned(&self) -> usize {
        return match self.state {
            // A `<` or `</` may still open a bogus comment (`<?`, `</?`).
            State::TagOpen
            | State::EndTagOpen
            | State::MarkupDeclarationOpen { .. }
            | State::Comment { .. }
            | State::BogusComment => self.markup_start - self.free,
            _ => self.offset - self.free,
        };
    }

    /// [`HeadOpenTagLocator::pending_tag_start`], counted as
    /// [`HeadOpenTagLocator::significant_scanned`] does.
    pub(super) fn significant_pending_tag_start(&self) -> Option<usize> {
        return self
            .pending_tag_start()
            .map(|start| return start - self.free);
    }

    /// Offset of the markup or character reference being read while it may still turn out to be
    /// where the snippet goes before: a start tag whose name is not told apart yet, an end tag that
    /// may still be one of [`CLOSES_HEAD`], a character reference that may not be whitespace, or a
    /// `<` that may be text. What precedes the snippet if it is, so what has to be held back
    /// meanwhile; each is short, but an end tag stuffed with attributes.
    pub(super) fn pending_point_start(&self) -> Option<usize> {
        if self.template_depth > 0 || !self.is_searching() {
            return None;
        }
        let may_close_head = |complete: bool| {
            return !self.body_mode
                && !self.name_overflow
                && CLOSES_HEAD.iter().any(|name| {
                    let read = &self.name[..self.name_length];
                    return match complete {
                        true => *name == read,
                        false => name.starts_with(read),
                    };
                });
        };
        return match self.state {
            State::TagOpen | State::CharacterReference { .. } => Some(self.markup_start),
            State::EndTagOpen if !self.body_mode => Some(self.markup_start),
            State::Tag {
                closing: false,
                position: TagPosition::TagName,
            } if !self.name_checked => Some(self.markup_start),
            State::Tag {
                closing: true,
                position: TagPosition::TagName,
            } if may_close_head(false) => Some(self.markup_start),
            // The name is complete: it is pending on its `>`.
            State::Tag { closing: true, .. } if may_close_head(true) => Some(self.markup_start),
            _ => None,
        };
    }

    /// Offset of the `<` of the tag being read, while it may still turn out to be a `<head>`, a
    /// `<html>` or a `<meta>` start tag: more data is needed to tell.
    pub(super) fn pending_tag_start(&self) -> Option<usize> {
        return match self.state {
            State::TagOpen
            | State::Tag {
                closing: false,
                position: TagPosition::TagName,
            } => Some(self.markup_start),
            // The name is complete: only a `head` start tag is still pending, on its `>`.
            State::Tag { closing: false, .. } if self.name_is(b"head") => Some(self.markup_start),
            _ => None,
        };
    }

    /// Whether the document may end right here with the snippet going last: within an open head
    /// of a document that looks like one, which the end of the document closes, rather than within
    /// a tag, a comment, raw text or a template.
    pub(super) fn may_inject_at_end(&self) -> bool {
        return self.state == State::Data
            && self.template_depth == 0
            && !self.body_mode
            && self.strong_evidence;
    }

    /// Whether the head closed before any evidence, the search going on within the body: what is
    /// found then is the evidence, which the snippet goes right before.
    pub(super) fn in_body(&self) -> bool {
        return self.body_mode;
    }

    /// Where what was found starts, counting only significant bytes: how far into the document
    /// the evidence a break found within the body is.
    pub(super) fn found_significant(&self) -> usize {
        return self.found_significant;
    }

    /// Whether where the snippet goes may still be found in what follows: not once it has been, nor
    /// once there is nothing left to look for, nor past a `<plaintext>` tag.
    pub(super) fn is_searching(&self) -> bool {
        return !matches!(
            self.state,
            State::Found | State::PastHead | State::Plaintext
        );
    }

    /// Scans the next chunk of the document, and returns where the snippet goes if that is told
    /// within it. Only the first one is ever returned.
    pub(super) fn scan(&mut self, chunk: &[u8]) -> Option<HeadOpenTag> {
        if !self.is_searching() {
            self.offset += chunk.len();
            return None;
        }
        let mut index = 0;
        loop {
            index += self.skip(&chunk[index..], self.offset + index);
            // Text found while skipping.
            if self.state == State::Found {
                return Some(self.found(chunk.len(), index));
            }
            let Some(&byte) = chunk.get(index) else {
                break;
            };
            if self.step(byte, self.offset + index) {
                return Some(self.found(chunk.len(), index + 1));
            }
            index += 1;
        }
        self.offset += chunk.len();
        return None;
    }

    fn found(&mut self, chunk_length: usize, end: usize) -> HeadOpenTag {
        // Whatever was found, no free byte follows it.
        self.found_significant = self.found_start - self.free.min(self.found_start);
        self.offset += chunk_length;
        return HeadOpenTag {
            start: self.found_start,
            end,
            breaks: self.found_breaks,
        };
    }

    /// Returns how many bytes at the start of `rest` leave the state as [`Self::step`] would after
    /// reading them one by one, so they can be skipped at once: text up to the next `<`, a bogus
    /// comment up to its `>`, comment content up to the next byte that may end it, a quoted
    /// attribute value up to its quote, raw text up to the next byte that may start its closing tag
    /// or, in a script, change its state. Most of a document is skipped this way.
    ///
    /// Text other than whitespace, where the head closes, sets the state to [`State::Found`] or
    /// [`State::PastHead`]; when found, the number returned is the index of the text.
    fn skip(&mut self, rest: &[u8], position: usize) -> usize {
        let found = |index: Option<usize>| return index.unwrap_or(rest.len());
        return match self.state {
            State::Found | State::PastHead | State::Plaintext => rest.len(),
            // Text within a template does not count, nor does it once the head is closed.
            State::Data if self.template_depth > 0 || self.body_mode => found(memchr(b'<', rest)),
            State::Data => {
                let skipped = found(memchr2(b'<', b'&', rest));
                let markup_follows = skipped < rest.len();
                if let Some(text) = self.text_start(&rest[..skipped], position, markup_follows) {
                    self.state = self.head_closes(text, false).unwrap_or(State::PastHead);
                    return match self.state {
                        // Text a broken byte order mark starts in a previous chunk is found at the
                        // start of this one.
                        State::Found => text.saturating_sub(position),
                        _ => rest.len(),
                    };
                }
                // Whitespace, or the byte order mark: nothing that tells the document is HTML.
                self.free += skipped;
                skipped
            }
            State::BogusComment => {
                let skipped = found(memchr(b'>', rest));
                self.push_declaration(&rest[..skipped]);
                skipped
            }
            State::Tag {
                position: TagPosition::QuotedAttributeValue(quote),
                ..
            } => found(memchr(quote, rest)),
            State::Comment { .. } => {
                let skipped = found(memchr3(b'-', b'!', b'>', rest));
                if skipped != 0 {
                    // What any other byte leaves.
                    self.state = State::Comment {
                        run: 0,
                        content_run: 0,
                        bang: false,
                    };
                }
                skipped
            }
            // Every sequence raw text is matched against starts with a `<` or a `-`: once none is
            // among the last bytes read, none is in progress, and the bytes up to the next one can
            // only push the ones read before out of `recent`, which emptying it stands for.
            State::RawText { .. } if !self.recent_holds_sequence_start() => {
                let skipped = match self.name_is(b"script") {
                    true => found(memchr2(b'<', b'-', rest)),
                    false => found(memchr(b'<', rest)),
                };
                if skipped != 0 {
                    self.recent_length = 0;
                }
                skipped
            }
            _ => 0,
        };
    }

    fn recent_holds_sequence_start(&self) -> bool {
        return self.recent[..self.recent_length]
            .iter()
            .any(|&byte| return byte == b'<' || byte == b'-');
    }

    /// The head closes at `at`, on markup when `markup`, on text otherwise. Returns the state that
    /// follows: found, when the document looks like one; nothing left to look for, on text; on a
    /// tag, `None`, the tag carrying on to be read within the body.
    fn head_closes(&mut self, at: usize, markup: bool) -> Option<State> {
        self.prologue_open = false;
        if self.strong_evidence {
            self.found_start = at;
            self.found_breaks = true;
            return Some(State::Found);
        }
        if markup {
            self.body_mode = true;
            return None;
        }
        return Some(State::PastHead);
    }

    /// Consumes one byte at `offset`, and returns whether it tells where the snippet goes.
    fn step(&mut self, byte: u8, offset: usize) -> bool {
        self.state = match self.state {
            State::Found | State::PastHead | State::Plaintext => self.state,

            State::Data => match byte {
                b'<' => self.open_markup(offset),
                b'&' if self.template_depth == 0 && !self.body_mode => {
                    self.markup_start = offset;
                    State::CharacterReference {
                        name: [0; MAX_REFERENCE],
                        length: 0,
                    }
                }
                _ => State::Data,
            },

            State::CharacterReference { mut name, length } => {
                let continues = byte.is_ascii_alphanumeric() || (length == 0 && byte == b'#');
                if continues && length < MAX_REFERENCE {
                    name[length] = byte;
                    return {
                        self.state = State::CharacterReference {
                            name,
                            length: length + 1,
                        };
                        false
                    };
                }
                if !is_whitespace_reference(&name[..length], byte == b';') {
                    self.head_closes(self.markup_start, false)
                        .unwrap_or(State::PastHead)
                } else if byte == b';' {
                    State::Data
                } else if !matches!(byte, b'<' | b'&' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ') {
                    // The reference ends before text, where the head closes.
                    self.head_closes(offset, false).unwrap_or(State::PastHead)
                } else {
                    // The reference ends before this byte, which is read as the data it is.
                    self.state = State::Data;
                    return self.step(byte, offset);
                }
            }

            // Any other byte makes the `<` text, which closes the head, or starts the body before
            // one.
            State::TagOpen
                if self.template_depth == 0
                    && !matches!(byte, b'!' | b'/' | b'?')
                    && !byte.is_ascii_alphabetic() =>
            {
                match (self.body_mode, byte) {
                    (true, b'<') => self.open_markup(offset),
                    (true, _) => State::Data,
                    (false, _) => self
                        .head_closes(self.markup_start, false)
                        .unwrap_or(State::PastHead),
                }
            }

            State::TagOpen => match byte {
                b'!' => State::MarkupDeclarationOpen { dashes: 0 },
                b'/' => State::EndTagOpen,
                b'?' => State::BogusComment,
                b'<' => self.open_markup(offset),
                _ if byte.is_ascii_alphabetic() => {
                    self.push_name(byte);
                    State::Tag {
                        closing: false,
                        position: TagPosition::TagName,
                    }
                }
                // A stray `<`, as in `a < b`, is text.
                _ => State::Data,
            },

            State::EndTagOpen => match byte {
                // `</>` is dropped altogether.
                b'>' => State::Data,
                _ if byte.is_ascii_alphabetic() => {
                    self.push_name(byte);
                    State::Tag {
                        closing: true,
                        position: TagPosition::TagName,
                    }
                }
                // `</` followed by anything but a letter is a bogus comment.
                _ => State::BogusComment,
            },

            State::Tag { closing, position } => match self.step_tag(position, byte) {
                Some(next) => {
                    if !closing && position == TagPosition::TagName && !self.name_checked {
                        // The name is complete once the tag moves past it, and it is told apart
                        // as soon as it outgrows every name that matters.
                        if next != position || self.name_overflow {
                            if let Some(state) = self.start_tag_named() {
                                self.state = state;
                                return state == State::Found;
                            }
                        }
                    }
                    if !closing && self.name_is(b"html") && self.template_depth == 0 {
                        self.read_html_attribute(position, next, byte);
                    }
                    State::Tag {
                        closing,
                        position: next,
                    }
                }
                // The tag ends at this `>`.
                None if closing => self.end_tag_ends(),
                None => {
                    if position == TagPosition::AttributeName && self.name_is(b"html") {
                        self.end_html_attribute();
                    }
                    if !self.name_checked {
                        if let Some(state) = self.start_tag_named() {
                            self.state = state;
                            return state == State::Found;
                        }
                    }
                    self.start_tag_ends(position == TagPosition::SelfClosing, offset)
                }
            },

            State::MarkupDeclarationOpen { dashes } => match (dashes, byte) {
                (0, b'-') => State::MarkupDeclarationOpen { dashes: 1 },
                (_, b'-') => State::Comment {
                    run: 2,
                    content_run: 0,
                    bang: false,
                },
                (_, b'>') => self.end_free_markup(offset),
                (0, _) => {
                    self.declaration_open = true;
                    self.push_declaration(&[byte]);
                    State::BogusComment
                }
                _ => State::BogusComment,
            },

            State::Comment {
                run,
                content_run,
                bang,
            } => match byte {
                b'-' if bang => State::Comment {
                    run: 1,
                    content_run: 1,
                    bang: false,
                },
                b'-' => State::Comment {
                    run: (run + 1).min(2),
                    content_run: (content_run + 1).min(2),
                    bang: false,
                },
                // `-->` may reuse the dashes of the `<!--`, which ends `<!-->` and `<!--->`, while
                // the ones of a `--!>` have to be comment content.
                b'>' if (!bang && run == 2) || (bang && content_run == 2) => {
                    self.end_free_markup(offset)
                }
                b'!' if !bang && content_run == 2 => State::Comment {
                    run,
                    content_run,
                    bang: true,
                },
                _ => State::Comment {
                    run: 0,
                    content_run: 0,
                    bang: false,
                },
            },

            State::BogusComment => match byte {
                b'>' => {
                    // The browser ignores a doctype past any other markup: the snippet goes past
                    // every one that opens the document, whatever its name.
                    if self.is_doctype() && self.template_depth == 0 && self.prologue_open {
                        self.prologue_end = offset + 1;
                    }
                    if self.is_html_doctype() {
                        if self.significant_html_evidence.is_none() {
                            self.significant_html_evidence = Some(self.markup_start - self.free);
                        }
                        if self.template_depth == 0 {
                            self.strong_evidence = true;
                            if self.prologue_open {
                                self.prologue_end = offset + 1;
                            }
                        }
                    }
                    self.end_free_markup(offset)
                }
                _ => {
                    self.push_declaration(&[byte]);
                    State::BogusComment
                }
            },

            State::RawText { script } => self.step_raw_text(script, byte),
        };
        return self.state == State::Found;
    }

    /// Tells apart the start tag being read by its name, complete or too long to matter. Returns
    /// the state that follows when the tag ends the search, `None` when it carries on being read.
    fn start_tag_named(&mut self) -> Option<State> {
        self.name_checked = true;
        self.complete_name();
        if self.template_depth > 0 {
            return None;
        }
        let names = |names: &[&[u8]]| return names.iter().any(|name| return self.name_is(name));
        if self.body_mode {
            if names(&[b"html", b"body"]) {
                self.found_start = self.markup_start;
                self.found_breaks = true;
                return Some(State::Found);
            }
            // Foreign content is not followed, nor a frameset: the browser drops the snippet in
            // one, and stops tokenizing raw text and RCDATA elements it ignores.
            if names(&[b"svg", b"math", b"frameset"]) {
                return Some(State::PastHead);
            }
            return None;
        }
        if names(&[b"html", b"head"]) {
            self.strong_evidence = true;
        }
        if self.may_precede_head() {
            if !self.name_is(b"html") {
                self.prologue_open = false;
            }
            return None;
        }
        // A `<body>` tag is evidence of its own.
        if self.name_is(b"body") {
            self.strong_evidence = true;
            if self.significant_html_evidence.is_none() {
                self.significant_html_evidence = Some(self.markup_start - self.free);
            }
        }
        let state = self.head_closes(self.markup_start, true);
        // Not read on within the body from foreign content or a frameset, see above.
        if state.is_none()
            && (self.name_is(b"svg") || self.name_is(b"math") || self.name_is(b"frameset"))
        {
            return Some(State::PastHead);
        }
        return state;
    }

    /// The state following the `>` of a start tag, self-closing when `self_closing`, at `offset`.
    fn start_tag_ends(&mut self, self_closing: bool, offset: usize) -> State {
        if self.name_is(b"template") {
            self.template_depth += 1;
            return State::Data;
        }
        if self.template_depth > 0 {
            // Where foreign content ends the template, an SVG `<template>` or an HTML integration
            // point (`<foreignObject>`, `<mi>`…) within it decides, which takes namespaces the
            // locator does not track: it gives up rather than inject within the template.
            if !self_closing && (self.name_is(b"svg") || self.name_is(b"math")) {
                return State::PastHead;
            }
            return self.state_after_start_tag();
        }
        if self.name_is(b"html") && !self.body_mode {
            if self.amp {
                return State::PastHead;
            }
            if self.prologue_open {
                self.prologue_end = offset + 1;
            }
        }
        if self.name_is(b"head") && self.target == Target::HeadStart && !self.body_mode {
            self.found_start = self.markup_start;
            self.found_breaks = false;
            return State::Found;
        }
        return self.state_after_start_tag();
    }

    /// The state following the `>` of an end tag.
    fn end_tag_ends(&mut self) -> State {
        if self.name_is(b"template") {
            self.template_depth = self.template_depth.saturating_sub(1);
            return State::Data;
        }
        if self.template_depth > 0 {
            return State::Data;
        }
        if self.body_mode || !CLOSES_HEAD.iter().any(|name| return self.name_is(name)) {
            return State::Data;
        }
        if self.target == Target::HeadEnd && self.name_is(b"head") {
            // The end tag is evidence of its own.
            if self.significant_head_start.is_none() {
                self.significant_head_start = Some(self.markup_start - self.free);
            }
            self.found_start = self.markup_start;
            self.found_breaks = false;
            return State::Found;
        }
        return self
            .head_closes(self.markup_start, true)
            .unwrap_or(State::Data);
    }

    /// Reads an attribute name of the `<html>` tag, moving from `position` to `next` on `byte`.
    fn read_html_attribute(&mut self, position: TagPosition, next: TagPosition, byte: u8) {
        let whitespace = matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ');
        match (position, next) {
            (TagPosition::AttributeName, TagPosition::AttributeName) if whitespace => {
                self.attribute_ended = true;
            }
            (TagPosition::AttributeName, TagPosition::AttributeName) if !self.attribute_ended => {
                self.push_attribute(byte);
            }
            // Past whitespace, a byte that is no `=` starts another attribute.
            (TagPosition::AttributeName, TagPosition::AttributeName) => {
                self.end_html_attribute();
                self.push_attribute(byte);
            }
            (_, TagPosition::AttributeName) => {
                self.attribute_length = 0;
                self.attribute_overflow = false;
                self.attribute_ended = false;
                self.push_attribute(byte);
            }
            (TagPosition::AttributeName, _) => self.end_html_attribute(),
            _ => {}
        }
    }

    fn push_attribute(&mut self, byte: u8) {
        if self.attribute_length < MAX_ATTRIBUTE {
            self.attribute[self.attribute_length] = byte.to_ascii_lowercase();
            self.attribute_length += 1;
        } else {
            self.attribute_overflow = true;
        }
    }

    fn end_html_attribute(&mut self) {
        let attribute = &self.attribute[..self.attribute_length];
        if !self.attribute_overflow && AMP_ATTRIBUTES.contains(&attribute) {
            self.amp = true;
        }
        self.attribute_length = 0;
        self.attribute_overflow = false;
        self.attribute_ended = false;
    }

    /// Advances through a tag, and returns `None` on the `>` closing it.
    fn step_tag(&mut self, position: TagPosition, byte: u8) -> Option<TagPosition> {
        let whitespace = matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ');
        return Some(match (position, byte) {
            // Checked before `>`, which does not close the tag while within a quoted value.
            (TagPosition::QuotedAttributeValue(quote), _) => match byte == quote {
                true => TagPosition::BeforeAttributeName,
                false => position,
            },

            (_, b'>') => return None,

            (TagPosition::TagName, _) if whitespace => TagPosition::BeforeAttributeName,
            (TagPosition::TagName, b'/') => TagPosition::SelfClosing,
            (TagPosition::TagName, _) => {
                self.push_name(byte);
                position
            }

            (TagPosition::BeforeAttributeName, _) if whitespace => position,
            (TagPosition::BeforeAttributeName | TagPosition::AttributeName, b'/') => {
                TagPosition::SelfClosing
            }
            (TagPosition::BeforeAttributeName, _) => TagPosition::AttributeName,

            (TagPosition::AttributeName, b'=') => TagPosition::BeforeAttributeValue,
            (TagPosition::AttributeName, _) => position,

            (TagPosition::BeforeAttributeValue, _) if whitespace => position,
            (TagPosition::BeforeAttributeValue, b'"' | b'\'') => {
                TagPosition::QuotedAttributeValue(byte)
            }
            (TagPosition::BeforeAttributeValue, _) => TagPosition::UnquotedAttributeValue,

            // A `/` is part of an unquoted value, unlike whitespace.
            (TagPosition::UnquotedAttributeValue, _) if whitespace => {
                TagPosition::BeforeAttributeName
            }
            (TagPosition::UnquotedAttributeValue, _) => position,

            (TagPosition::SelfClosing, _) if whitespace => TagPosition::BeforeAttributeName,
            (TagPosition::SelfClosing, b'/') => position,
            (TagPosition::SelfClosing, _) => TagPosition::AttributeName,
        });
    }

    /// The state following a start tag other than `<head>`: text for most, or the content of a raw
    /// text element, which the name is kept for.
    fn state_after_start_tag(&mut self) -> State {
        if self.name_is(b"plaintext") {
            return State::Plaintext;
        }
        if RAW_TEXT_ELEMENTS
            .iter()
            .any(|name| return self.name_is(name))
        {
            self.recent_length = 0;
            return State::RawText {
                script: ScriptState::Data,
            };
        }
        return State::Data;
    }

    /// Advances through raw text content, which only its own closing tag ends. Script content also
    /// has the escaped states the parser module documents, where a `</script>` may end nothing.
    fn step_raw_text(&mut self, script: ScriptState, byte: u8) -> State {
        self.push_recent(byte.to_ascii_lowercase());
        let is_script = self.name_is(b"script");
        let delimiter = matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ' | b'/' | b'>');

        if is_script {
            if script == ScriptState::Data && self.recent_ends_with(&[b"<!--"], false) {
                return State::RawText {
                    script: ScriptState::Escaped,
                };
            }
            if script != ScriptState::Data && self.recent_ends_with(&[b"-->"], false) {
                return State::RawText {
                    script: ScriptState::Data,
                };
            }
            if script == ScriptState::Escaped
                && delimiter
                && self.recent_ends_with(&[b"<script"], true)
            {
                return State::RawText {
                    script: ScriptState::DoubleEscaped,
                };
            }
        }

        if delimiter && self.recent_ends_with(&[b"</", &self.name[..self.name_length]], true) {
            if script == ScriptState::DoubleEscaped {
                return State::RawText {
                    script: ScriptState::Escaped,
                };
            }
            if byte == b'>' {
                return State::Data;
            }
            let position = match byte {
                b'/' => TagPosition::SelfClosing,
                _ => TagPosition::BeforeAttributeName,
            };
            return State::Tag {
                closing: true,
                position,
            };
        }

        return State::RawText { script };
    }

    fn open_markup(&mut self, offset: usize) -> State {
        self.markup_start = offset;
        self.name_length = 0;
        self.name_overflow = false;
        self.name_checked = false;
        self.declaration_length = 0;
        self.declaration_open = false;
        return State::TagOpen;
    }

    /// Ends a comment, a bogus comment or a doctype at the `>` at `offset`: all of it is free.
    fn end_free_markup(&mut self, offset: usize) -> State {
        self.free += offset + 1 - self.markup_start;
        return State::Data;
    }

    fn push_declaration(&mut self, bytes: &[u8]) {
        let taken = bytes.len().min(MAX_DECLARATION - self.declaration_length);
        for (slot, byte) in self.declaration[self.declaration_length..]
            .iter_mut()
            .zip(&bytes[..taken])
        {
            *slot = byte.to_ascii_lowercase();
        }
        self.declaration_length += taken;
    }

    /// Whether the `<!` declaration just read is a doctype, which the tokenizer ends at its first `>`.
    fn is_doctype(&self) -> bool {
        return self.declaration_open
            && self.declaration[..self.declaration_length].starts_with(b"doctype");
    }

    /// Whether the `<!` declaration just read is an HTML doctype: `doctype`, then the name `html`,
    /// whitespace between them optional, as the tokenizer has it, and a public or system
    /// identifier allowed after it.
    fn is_html_doctype(&self) -> bool {
        if !self.declaration_open {
            return false;
        }
        let Some(rest) = self.declaration[..self.declaration_length].strip_prefix(b"doctype")
        else {
            return false;
        };
        let rest = rest.trim_ascii_start();
        let Some(after) = rest.strip_prefix(b"html") else {
            return false;
        };
        // `<!DOCTYPE html>` ends right there, which is all the declaration holds then.
        return after
            .first()
            .is_none_or(|byte| return byte.is_ascii_whitespace());
    }

    fn push_name(&mut self, byte: u8) {
        if self.name_length < MAX_TAG_NAME {
            self.name[self.name_length] = byte.to_ascii_lowercase();
            self.name_length += 1;
        } else {
            self.name_overflow = true;
        }
    }

    /// Called once the name of a start tag is complete: the first `<html>` or `<meta>` start tag is
    /// HTML evidence, and so is the first `<head>` one looking for the `</head>` tag.
    fn complete_name(&mut self) {
        if self.html_evidence.is_none() && (self.name_is(b"html") || self.name_is(b"meta")) {
            self.html_evidence = Some(self.markup_start);
        }
        if self.significant_html_evidence.is_none()
            && (self.name_is(b"html") || self.name_is(b"meta"))
        {
            self.significant_html_evidence = Some(self.markup_start - self.free);
        }
        if self.target == Target::HeadEnd && self.head_start.is_none() && self.name_is(b"head") {
            self.head_start = Some(self.markup_start);
            self.significant_head_start = Some(self.markup_start - self.free);
        }
    }

    /// Whether the start tag whose name was just read may come before the `<head>` tag.
    fn may_precede_head(&self) -> bool {
        return self.name_is(b"head")
            || MAY_PRECEDE_HEAD
                .iter()
                .any(|name| return self.name_is(name));
    }

    fn name_is(&self, name: &[u8]) -> bool {
        return !self.name_overflow && &self.name[..self.name_length] == name;
    }

    fn push_recent(&mut self, byte: u8) {
        if self.recent_length == MAX_RECENT {
            self.recent.copy_within(1.., 0);
            self.recent_length -= 1;
        }
        self.recent[self.recent_length] = byte;
        self.recent_length += 1;
    }

    /// Whether the raw text bytes read so far, the current one included, end with the concatenation
    /// of `parts`, followed by that current byte when `delimited`: a tag name only ends at the byte
    /// following it.
    fn recent_ends_with(&self, parts: &[&[u8]], delimited: bool) -> bool {
        let recent = &self.recent[..self.recent_length - usize::from(delimited)];
        let total: usize = parts.iter().map(|part| return part.len()).sum();
        if recent.len() < total {
            return false;
        }
        let mut index = recent.len() - total;
        for part in parts {
            if &recent[index..index + part.len()] != *part {
                return false;
            }
            index += part.len();
        }
        return true;
    }
}

/// Whether the character reference whose name follows the `&`, ended by a `;` when `terminated`,
/// decodes to whitespace: a numeric one to a tab, a line feed, a form feed, a carriage return or a
/// space (its `;` optional, as the parser has it), or `&Tab;` or `&NewLine;`.
pub(super) fn is_whitespace_reference(name: &[u8], terminated: bool) -> bool {
    let number = match name {
        [b'#', b'x' | b'X', digits @ ..] if !digits.is_empty() => {
            digits.iter().try_fold(0u32, |n, &b| {
                return n.checked_mul(16)?.checked_add((b as char).to_digit(16)?);
            })
        }
        [b'#', digits @ ..] if !digits.is_empty() => digits.iter().try_fold(0u32, |n, &b| {
            return n.checked_mul(10)?.checked_add((b as char).to_digit(10)?);
        }),
        b"Tab" | b"NewLine" => return terminated,
        _ => return false,
    };
    return matches!(number, Some(0x09 | 0x0A | 0x0C | 0x0D | 0x20));
}

impl HeadOpenTagLocator {
    /// The offset from the start of the document of the first byte of `text`, read in the data
    /// state from `position` on, that is neither whitespace nor part of the byte order mark a UTF-8
    /// document may start with, if any. `markup_follows` tells the text ends at a `<` or `&` rather
    /// than with the chunk.
    ///
    /// The mark only counts whole: the bytes of one the text leaves incomplete wait for what
    /// follows, in the next chunk, to tell. If it is not the rest of the mark, they are text, at the
    /// start of the document, whatever the chunking.
    fn text_start(&mut self, text: &[u8], position: usize, markup_follows: bool) -> Option<usize> {
        const BOM: &[u8] = b"\xEF\xBB\xBF";
        let mut skipped = 0;
        // Only a mark the document starts with, all of the document so far.
        if position == self.mark_length && position < BOM.len() {
            let mark = &BOM[position..];
            let common = text.len().min(mark.len());
            let broken =
                text[..common] != mark[..common] || (common < mark.len() && markup_follows);
            match broken {
                // What of it was read in previous chunks is text.
                true if position > 0 => return Some(0),
                true => {}
                false => {
                    self.mark_length += common;
                    skipped = common;
                }
            }
        }
        return text[skipped..]
            .iter()
            .position(|&byte| return !matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '))
            .map(|index| return position + skipped + index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec::Vec;
    use pretty_assertions::assert_eq;

    /// Scans `chunks` and returns the offset of the `<head>` tag, from its `<` to past its `>`.
    fn locate(chunks: &[&[u8]]) -> Option<(usize, usize)> {
        let mut locator = HeadOpenTagLocator::new();
        let mut offset = 0;
        for chunk in chunks {
            if let Some(head) = locator.scan(chunk) {
                // Where the head closes without a `<head>` tag is checked by `locate_break`.
                return (!head.breaks).then_some((head.start, offset + head.end));
            }
            offset += chunk.len();
        }
        return None;
    }

    /// Same as [`locate`] for a single chunk, and checks every split into two chunks gives the same.
    fn find(input: &str) -> Option<(usize, usize)> {
        let input = input.as_bytes();
        let expected = locate(&[input]);
        for split in 0..=input.len() {
            let (first, second) = input.split_at(split);
            assert_eq!(locate(&[first, second]), expected, "split at {}", split);
        }
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(locate(&bytes), expected, "one byte at a time");
        return expected;
    }

    #[test]
    fn basic() {
        assert_eq!(find("<head>"), Some((0, 6)));
        assert_eq!(find(" \n<head>def"), Some((2, 8)));
        assert_eq!(find("<html><HeAd lang=en>"), Some((6, 20)));
        assert_eq!(find("<head/>"), Some((0, 7)));
        assert_eq!(find("<head\n>"), Some((0, 7)));
    }

    #[test]
    fn other_tags() {
        assert_eq!(find("<header><heads><hea><h ead>"), None);
        // The `before head` insertion mode opens the head on a `</head>` tag, which then closes
        // it: no `<head>` tag counts past it.
        assert_eq!(find("</head><head>"), None);
        // A stray `<` is text, which starts the body.
        assert_eq!(find("< head><head>"), None);
        assert_eq!(find("<<head>"), None);
    }

    #[test]
    fn attribute_values() {
        assert_eq!(find("<html a='<head>'><head>"), Some((17, 23)));
        assert_eq!(find("<head a='>'>"), Some((0, 12)));
        assert_eq!(find("<head a = \">\">"), Some((0, 14)));
        // A quote opens a value only right after an `=`.
        assert_eq!(find("<head '>"), Some((0, 8)));
        assert_eq!(find("<head a=b'>"), Some((0, 11)));
        // A `/` is part of an unquoted value, so the quote that follows it is too.
        assert_eq!(find("<head a=b/c='>"), Some((0, 14)));
        // End tags have attributes too.
        assert_eq!(find("</p a='<head>'><head>"), Some((15, 21)));
    }

    #[test]
    fn comments() {
        assert_eq!(find("<!-- <head> --><head>"), Some((15, 21)));
        assert_eq!(find("<!-- > <head> --><head>"), Some((17, 23)));
        assert_eq!(find("<!--><head>"), Some((5, 11)));
        assert_eq!(find("<!---><head>"), Some((6, 12)));
        assert_eq!(find("<!-- --!><head>"), Some((9, 15)));
        assert_eq!(find("<!----!><head>"), Some((8, 14)));
        assert_eq!(find("<!--!><head>"), None);
        assert_eq!(find("<!---!><head>"), None);
        assert_eq!(find("<!-- --!-><head>"), None);
        assert_eq!(find("<!-- <head>"), None);
    }

    #[test]
    fn bogus_comments_and_doctype() {
        assert_eq!(find("<!DOCTYPE html><head>"), Some((15, 21)));
        assert_eq!(find("<!DOCTYPE html SYSTEM \"><head>"), Some((24, 30)));
        assert_eq!(find("<?xml '><head>"), Some((8, 14)));
        assert_eq!(find("<!x <head>"), None);
        assert_eq!(find("</3 <head>"), None);
        assert_eq!(find("</><head>"), Some((3, 9)));
        // Out of foreign content, a CDATA section is a bogus comment, which the first `>` ends:
        // what follows it is text here, which starts the body.
        assert_eq!(find("<![CDATA[<head>]]>"), None);
        assert_eq!(find("<![CDATA[x]]><head>"), Some((13, 19)));
        assert_eq!(
            find("<svg><text><![CDATA[><head>x]]></text></svg><head>"),
            None
        );
        // Anything else is a bogus comment.
        assert_eq!(find("<![CDATAx><head>"), Some((10, 16)));
        assert_eq!(find("<![cdata[><head>"), Some((10, 16)));
        assert_eq!(find("<![><head>"), Some((4, 10)));
    }

    #[test]
    fn raw_text_elements() {
        assert_eq!(find("<script>'<head>'</script><head>"), Some((25, 31)));
        assert_eq!(find("<SCRIPT>'<head>'</Script ><head>"), Some((26, 32)));
        assert_eq!(
            find("<script></scriptx><head></script><head>"),
            Some((33, 39))
        );
        assert_eq!(find("<script a='>'></script><head>"), Some((23, 29)));
        assert_eq!(find("<script>x</script a='>'><head>"), Some((24, 30)));
        assert_eq!(find("<script/><head></script><head>"), Some((24, 30)));
        assert_eq!(find("<title><head></title><head>"), Some((21, 27)));
        assert_eq!(find("<style><head></style><head>"), Some((21, 27)));
        assert_eq!(find("<noscript><head></noscript><head>"), Some((27, 33)));
        // Its content is text, but a `<textarea>` is body content: no `<head>` counts past it.
        assert_eq!(find("<textarea><head></textarea><head>"), None);
        assert_eq!(find("<script><head>"), None);
        assert_eq!(find("<plaintext></plaintext><head>"), None);
        // Template content is inert: a `<head>` tag in one is not the head, nor does text or body
        // content in one end the search.
        assert_eq!(find("<template><head>"), None);
        assert_eq!(
            find("<template><head><div>x</div></template><head>"),
            Some((39, 45))
        );
        assert_eq!(
            find("<template><template></template><head></template><head>"),
            Some((48, 54))
        );
    }

    #[test]
    fn escaped_script_content() {
        assert_eq!(
            find("<script><!--<script></script><head></script><head>"),
            Some((44, 50))
        );
        assert_eq!(
            find("<script><!--<script>--></script><head>"),
            Some((32, 38))
        );
        assert_eq!(find("<script><!--</script><head>"), Some((21, 27)));
        assert_eq!(find("<script><!--></script><head>"), Some((22, 28)));
    }

    #[test]
    fn html_evidence() {
        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"<!-- <html> --><link><meta");
        assert_eq!(locator.html_evidence(), None);
        assert_eq!(locator.pending_tag_start(), Some(21));
        locator.scan(b" charset=utf-8><html>");
        assert_eq!(locator.html_evidence(), Some(21));
        assert_eq!(locator.pending_tag_start(), None);

        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"<script><html></script><HTML");
        assert_eq!(locator.html_evidence(), None);
        locator.scan(b">");
        assert_eq!(locator.html_evidence(), Some(23));

        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"<metadata><htmlx></html>");
        assert_eq!(locator.html_evidence(), None);
    }

    #[test]
    fn pending_head_tag() {
        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"\n\n<head");
        assert_eq!(locator.pending_tag_start(), Some(2));
        locator.scan(b" a='>");
        assert_eq!(locator.pending_tag_start(), Some(2));
        assert_eq!(
            locator.scan(b"'>x"),
            Some(HeadOpenTag {
                start: 2,
                end: 2,
                breaks: false
            })
        );
        // Only the first one is ever returned.
        assert_eq!(locator.scan(b"<head>"), None);

        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"<div");
        assert_eq!(locator.pending_tag_start(), Some(0));
        locator.scan(b" ");
        assert_eq!(locator.pending_tag_start(), None);
    }

    /// Scans `input` whole, one byte at a time and split in two everywhere, looking for `target`,
    /// and returns where the head closes without it, if that is what is found.
    fn locate_break(target: Target, input: &str) -> Option<usize> {
        let scan = |chunks: &[&[u8]]| {
            let mut locator = HeadOpenTagLocator::with_target(target);
            for chunk in chunks {
                if let Some(head) = locator.scan(chunk) {
                    return head.breaks.then_some(head.start);
                }
            }
            return None;
        };
        let input = input.as_bytes();
        let expected = scan(&[input]);
        for split in 0..=input.len() {
            let (first, second) = input.split_at(split);
            assert_eq!(scan(&[first, second]), expected, "split at {}", split);
        }
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(scan(&bytes), expected, "one byte at a time");
        return expected;
    }

    #[test]
    fn where_the_head_closes() {
        for target in [Target::HeadStart, Target::HeadEnd] {
            // Body content, text and these end tags, once the document looks like one.
            assert_eq!(locate_break(target, "<html><img src=a>"), Some(6));
            assert_eq!(locate_break(target, "<!DOCTYPE html>\n x"), Some(17));
            assert_eq!(locate_break(target, "<html>&nbsp;"), Some(6));
            assert_eq!(locate_break(target, "<html></body>"), Some(6));
            assert_eq!(
                locate_break(target, "<html><averyveryverylongname>"),
                Some(6)
            );
            // Not in a fragment, nor before a lone `<meta>`.
            assert_eq!(locate_break(target, "<div>x</div>"), None);
            assert_eq!(locate_break(target, "<meta name=a><div>"), None);
            // Within the body, before an `<html>` or `<body>` tag, and not past foreign content.
            assert_eq!(locate_break(target, "<br>Notice<br><html>"), Some(14));
            assert_eq!(locate_break(target, "<br><head><body>"), Some(10));
            assert_eq!(locate_break(target, "<br><svg><html>"), None);
            assert_eq!(
                locate_break(target, "<br><textarea><html></textarea>"),
                None
            );
            // Never in an AMP document.
            assert_eq!(locate_break(target, "<html amp><img>"), None);
            assert_eq!(locate_break(target, "<html \u{26a1}4email><img>"), None);
            assert_eq!(locate_break(target, "<html ampx><img>"), Some(11));
        }
        // A `</head>` tag closes the head the `before head` mode opens.
        assert_eq!(
            locate_break(Target::HeadStart, "<html></head><head>"),
            Some(6)
        );
        assert_eq!(locate_break(Target::HeadEnd, "<html></head>"), None);
    }

    #[test]
    fn foreign_content_within_a_template() {
        for target in [Target::HeadStart, Target::HeadEnd] {
            // Where it ends the template takes namespaces: the search gives up, rather than inject
            // within the template.
            for content in [
                "<svg><template></template></svg><div>x</div>",
                "<svg><foreignObject><style>/* </template> */</style></foreignObject></svg>",
                "<svg><![CDATA[</template>]]></svg>",
                "<math><mi>",
            ] {
                assert_eq!(
                    locate_break(
                        target,
                        &format!("<html><template>{content}</template><img>")
                    ),
                    None
                );
            }
            // Not on an element that holds nothing.
            assert_eq!(
                locate_break(target, "<html><template><svg/></template><img>"),
                Some(33)
            );
        }
    }
}
