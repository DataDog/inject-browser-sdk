// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Streaming locator for the first `<head>` start tag, or the first `</head>` end tag. See
//! [`HeadOpenTagLocator`].

use memchr::{memchr, memchr2, memchr3};

/// What a [`HeadOpenTagLocator`] looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// The first `<head>` start tag, to inject right after it.
    HeadStart,
    /// The first `</head>` end tag, to inject right before it: one closing the head of a document
    /// that looked like HTML before it (see [`HeadOpenTagLocator::evidence`]), and coming before any
    /// body content. A `</body>`, `</html>` or `</br>` end tag closes the head first, and ends the
    /// search, as body content does, and so does text other than whitespace (a byte order mark
    /// starting the document aside): the browser closes the head on it.
    HeadEnd,
}

/// The tag a [`HeadOpenTagLocator`] looks for, as found by [`HeadOpenTagLocator::scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeadOpenTag {
    /// Offset of its `<` from the start of the document, across every chunk scanned.
    pub(super) start: usize,

    /// Index just past its `>`, within the chunk the match was completed in.
    pub(super) end: usize,
}

/// Longest tag name the locator has to tell apart, `plaintext`.
const MAX_TAG_NAME: usize = 9;

/// Longest sequence raw text content is matched against: `</noscript` and its delimiter.
const MAX_RECENT: usize = 11;

/// Elements whose content is text rather than markup, so a `<head>` written within one is not a tag.
///
/// `noscript` is among them as browsers run with scripting enabled, and so does lol_html.
/// Longest character reference read after its `&`, to tell a whitespace one: `#x0000000020` and the
/// like are text then, rather than held without bound.
const MAX_REFERENCE: usize = 12;

/// How many bytes of a `<!` declaration are kept, to tell a `<!DOCTYPE html>` one.
const MAX_DECLARATION: usize = 16;

/// What follows the `<!` of a CDATA section opener.
const CDATA_SECTION_OPEN: &[u8] = b"[CDATA[";

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
    /// Within the `[CDATA[` of a `<![CDATA[`, holding how many of its bytes were read.
    CdataSectionOpen {
        matched: usize,
    },
    /// Within a comment: `run` is the number of dashes just read, capped at two, `content_run` the
    /// part of them that is comment content rather than the `<!--` opener, and `bang` whether a `!`
    /// followed two content dashes.
    Comment {
        run: u8,
        content_run: u8,
        bang: bool,
    },
    /// A doctype, a `<?…>`, or any `<!…>` that opens no comment: all end at the very next `>`.
    BogusComment,
    /// Within the content of the raw text element named [`HeadOpenTagLocator::name`].
    RawText {
        script: ScriptState,
    },
    /// Past a `<plaintext>` tag, which nothing ends.
    Plaintext,
    /// A character reference in text, past its `&`: one that decodes to whitespace keeps the head
    /// open, any other is text.
    CharacterReference {
        name: [u8; MAX_REFERENCE],
        length: usize,
    },
    /// Past a `<![CDATA[`, whose end cannot be told without tracking foreign content: it runs to
    /// `]]>` within `svg` or `math` and to the first `>` anywhere else. Guessing wrong would find a
    /// `<head>` written in CDATA text, so nothing is looked for past it, as the window walk does.
    CdataSection,
    /// The tag looked for was found; nothing is left to look for.
    Found,
    /// Past a start tag the head cannot follow, `<body>`, `<div>` or any other but the few
    /// [`MAY_PRECEDE_HEAD`] lists, or past text other than whitespace: the browser has started the
    /// body by then and ignores a `<head>` tag written later, so nothing is looked for past it. A
    /// document without a `<head>` tag, which HTML allows, is not read to its end for nothing.
    PastHead,
}

/// Stateful object used to locate the first `<head>` start tag of a document fed in chunks, so the
/// snippet can be injected right after it.
///
/// It follows the HTML tokenizer closely enough to tell a real `<head>` tag from one written within a
/// comment, an attribute value, or the content of a raw text element (`<script>`, `<title>`…), and
/// carries its state across chunks. The snippet goes after the tag, so the bytes scanned so far can
/// always be emitted: unlike a locator for a closing tag, this one never holds data back.
///
/// It also records whether the document looks like HTML: see [`HeadOpenTagLocator::html_evidence`].
pub(super) struct HeadOpenTagLocator {
    target: Target,

    state: State,

    /// Number of bytes scanned so far, across every chunk.
    offset: usize,

    /// Offset of the `<` that started the markup being read.
    markup_start: usize,

    /// Lower case name of the tag being read, or of the raw text element whose content is being
    /// read. Longer names are cut to [`MAX_TAG_NAME`] bytes, with `name_overflow` set.
    name: [u8; MAX_TAG_NAME],
    name_length: usize,
    name_overflow: bool,

    /// The last bytes of raw text content, lower case, to match its closing tag across chunks.
    recent: [u8; MAX_RECENT],
    recent_length: usize,

    /// Offset of the `<` of the first `<html>` or `<meta>` start tag.
    html_evidence: Option<usize>,

    /// Where the first `<html>`, `<meta>` start tag or `<!DOCTYPE html>` starts, counting only the
    /// bytes that are not [`HeadOpenTagLocator::free`].
    significant_html_evidence: Option<usize>,

    /// Where the first `<head>` start tag starts, counting the same way, looking for `</head>`.
    significant_head_start: Option<usize>,

    /// Bytes that tell nothing about whether the document is HTML, scanned so far: comments,
    /// bogus comments and doctypes, once ended, and whitespace in text. A license banner in a
    /// comment, or blank lines, do not push the evidence out of reach.
    free: usize,

    /// The first bytes of the `<!` declaration being read, lower case, and whether the markup is one.
    declaration: [u8; MAX_DECLARATION],
    declaration_length: usize,
    declaration_open: bool,

    /// Offset of the `<` of the first `<head>` start tag, looking for the `</head>` one.
    head_start: Option<usize>,

    /// How many `<template>` elements are open. Their content is inert: a `<head>` or `</head>`
    /// tag, body content or text within one changes nothing outside it.
    template_depth: usize,
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
            recent: [0; MAX_RECENT],
            recent_length: 0,
            html_evidence: None,
            significant_html_evidence: None,
            significant_head_start: None,
            free: 0,
            declaration: [0; MAX_DECLARATION],
            declaration_length: 0,
            declaration_open: false,
            head_start: None,
            template_depth: 0,
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
            State::MarkupDeclarationOpen { .. }
            | State::Comment { .. }
            | State::BogusComment
            | State::CdataSectionOpen { .. } => self.markup_start - self.free,
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

    /// Offset of the `<` of an end tag being read while it may still turn out to be `</head>`,
    /// looking for one: what precedes the snippet if it is, so what has to be held back meanwhile.
    pub(super) fn pending_end_tag_start(&self) -> Option<usize> {
        if self.target != Target::HeadEnd {
            return None;
        }
        return match self.state {
            State::TagOpen | State::EndTagOpen => Some(self.markup_start),
            // Only while the name read so far may still be `head`: any other end tag is released
            // as soon as it cannot be, however long it goes on.
            State::Tag {
                closing: true,
                position: TagPosition::TagName,
            } if !self.name_overflow && b"head".starts_with(&self.name[..self.name_length]) => {
                Some(self.markup_start)
            }
            // The name is complete: only a `head` end tag is still pending, on its `>`.
            State::Tag { closing: true, .. } if self.name_is(b"head") => Some(self.markup_start),
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

    /// Whether the tag looked for may still be found in what follows: not once it has been, nor past
    /// markup that ends the search, a `<plaintext>` tag or a CDATA section.
    pub(super) fn is_searching(&self) -> bool {
        return !matches!(
            self.state,
            State::Found | State::PastHead | State::Plaintext | State::CdataSection
        );
    }

    /// Scans the next chunk of the document, and returns the tag looked for if it ends within it.
    /// Only the first one is ever returned.
    pub(super) fn scan(&mut self, chunk: &[u8]) -> Option<HeadOpenTag> {
        let mut index = 0;
        loop {
            index += self.skip(&chunk[index..], self.offset + index);
            let Some(&byte) = chunk.get(index) else {
                break;
            };
            if self.step(byte, self.offset + index) {
                self.offset += chunk.len();
                return Some(HeadOpenTag {
                    start: self.markup_start,
                    end: index + 1,
                });
            }
            index += 1;
        }
        self.offset += chunk.len();
        return None;
    }

    /// Returns how many bytes at the start of `rest` leave the state as [`Self::step`] would after
    /// reading them one by one, so they can be skipped at once: text up to the next `<`, a bogus
    /// comment up to its `>`, comment content up to the next byte that may end it, a quoted
    /// attribute value up to its quote, raw text up to the next byte that may start its closing tag
    /// or, in a script, change its state. Most of a document is skipped this way.
    fn skip(&mut self, rest: &[u8], position: usize) -> usize {
        let found = |index: Option<usize>| return index.unwrap_or(rest.len());
        return match self.state {
            State::Found | State::PastHead | State::Plaintext | State::CdataSection => rest.len(),
            // Text within a template does not count.
            State::Data if self.template_depth > 0 => found(memchr(b'<', rest)),
            State::Data => {
                let skipped = found(memchr2(b'<', b'&', rest));
                if holds_text(&rest[..skipped], position) {
                    self.state = State::PastHead;
                    return rest.len();
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

    /// Consumes one byte at `offset`, and returns whether it is the `>` closing the tag looked for.
    fn step(&mut self, byte: u8, offset: usize) -> bool {
        self.state = match self.state {
            State::Found | State::PastHead | State::Plaintext | State::CdataSection => self.state,

            State::Data => match byte {
                b'<' => self.open_markup(offset),
                b'&' if self.template_depth == 0 => State::CharacterReference {
                    name: [0; MAX_REFERENCE],
                    length: 0,
                },
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
                    State::PastHead
                } else if byte == b';' {
                    State::Data
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
                State::PastHead
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

            State::Tag { closing, position } => {
                match self.step_tag(position, byte) {
                    Some(next) => {
                        // The name is complete once the tag moves past it, and only then.
                        let name_done = position == TagPosition::TagName && next != position;
                        if !closing && name_done {
                            self.complete_name();
                            if self.template_depth == 0 && !self.may_precede_head() {
                                self.state = State::PastHead;
                                return false;
                            }
                        }
                        State::Tag {
                            closing,
                            position: next,
                        }
                    }
                    // The tag ends at this `>`.
                    None if closing && self.name_is(b"template") => {
                        self.template_depth = self.template_depth.saturating_sub(1);
                        State::Data
                    }
                    // Within a template, no end tag closes the head.
                    None if closing && self.template_depth > 0 => State::Data,
                    None if closing => match self.target {
                        Target::HeadStart => State::Data,
                        Target::HeadEnd if self.name_is(b"head") => {
                            self.state = State::Found;
                            return true;
                        }
                        // These close the head as well, implicitly: no `</head>` that follows
                        // closes it.
                        Target::HeadEnd
                            if [b"body".as_slice(), b"html", b"br"]
                                .iter()
                                .any(|name| return self.name_is(name)) =>
                        {
                            State::PastHead
                        }
                        Target::HeadEnd => State::Data,
                    },
                    None => {
                        self.complete_name();
                        if self.name_is(b"template") {
                            self.template_depth += 1;
                            return {
                                self.state = State::Data;
                                false
                            };
                        }
                        // Within a template, a start tag neither is the head nor ends it.
                        if self.template_depth > 0 {
                            return {
                                self.state = self.state_after_start_tag();
                                false
                            };
                        }
                        if self.name_is(b"head") {
                            if self.target == Target::HeadEnd {
                                // Its own content is markup: carry on to its end tag.
                                self.state = State::Data;
                                return false;
                            }
                            self.state = State::Found;
                            return true;
                        }
                        if !self.may_precede_head() {
                            self.state = State::PastHead;
                            return false;
                        }
                        self.state_after_start_tag()
                    }
                }
            }

            State::MarkupDeclarationOpen { dashes } => match (dashes, byte) {
                (0, b'[') => State::CdataSectionOpen { matched: 1 },
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

            State::CdataSectionOpen { matched } => {
                match (CDATA_SECTION_OPEN[matched] == byte, byte) {
                    (true, _) if matched + 1 == CDATA_SECTION_OPEN.len() => State::CdataSection,
                    (true, _) => State::CdataSectionOpen {
                        matched: matched + 1,
                    },
                    // Any other `<![…` is a bogus comment, which the very next `>` ends.
                    (false, b'>') => self.end_free_markup(offset),
                    (false, _) => State::BogusComment,
                }
            }

            State::BogusComment => match byte {
                b'>' => {
                    if self.is_html_doctype() && self.significant_html_evidence.is_none() {
                        self.significant_html_evidence = Some(self.markup_start - self.free);
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
        return false;
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

/// Whether `text`, read in the data state from `position` on, holds anything but whitespace or the
/// byte order mark starting a UTF-8 document.
fn holds_text(text: &[u8], position: usize) -> bool {
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    let mut text = text;
    if let Some(mark) = BOM.get(position..) {
        // The part of the mark this text may hold, from where the document is.
        let common = text.len().min(mark.len());
        if text[..common] == mark[..common] {
            text = &text[common..];
        }
    }
    return text
        .iter()
        .any(|&byte| return !matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use pretty_assertions::assert_eq;

    /// Scans `chunks` and returns the offset of the `<head>` tag, from its `<` to past its `>`.
    fn locate(chunks: &[&[u8]]) -> Option<(usize, usize)> {
        let mut locator = HeadOpenTagLocator::new();
        let mut offset = 0;
        for chunk in chunks {
            if let Some(head) = locator.scan(chunk) {
                return Some((head.start, offset + head.end));
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
        assert_eq!(find("</head><head>"), Some((7, 13)));
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
        // A CDATA section ends the search, whatever follows it.
        assert_eq!(find("<![CDATA[<head>]]>"), None);
        assert_eq!(find("<![CDATA[x]]><head>"), None);
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
        assert_eq!(locator.scan(b"'>x"), Some(HeadOpenTag { start: 2, end: 2 }));
        // Only the first one is ever returned.
        assert_eq!(locator.scan(b"<head>"), None);

        let mut locator = HeadOpenTagLocator::new();
        locator.scan(b"<div");
        assert_eq!(locator.pending_tag_start(), Some(0));
        locator.scan(b" ");
        assert_eq!(locator.pending_tag_start(), None);
    }
}
