// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Streaming locator for the first `<head>` start tag. See [`HeadOpenTagLocator`].

/// The first `<head>` start tag, as found by [`HeadOpenTagLocator::scan`].
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
    /// A doctype, a `<?…>`, or any `<!…>` that opens no comment: all end at the very next `>`.
    BogusComment,
    /// Within the content of the raw text element named [`HeadOpenTagLocator::name`].
    RawText {
        script: ScriptState,
    },
    /// Past a `<plaintext>` tag, which nothing ends.
    Plaintext,
    /// The `<head>` start tag was found; nothing is left to look for.
    Found,
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
}

impl HeadOpenTagLocator {
    pub(super) fn new() -> Self {
        return Self {
            state: State::Data,
            offset: 0,
            markup_start: 0,
            name: [0; MAX_TAG_NAME],
            name_length: 0,
            name_overflow: false,
            recent: [0; MAX_RECENT],
            recent_length: 0,
            html_evidence: None,
        };
    }

    /// Offset of the first `<html>` or `<meta>` start tag scanned, if any: the sign that the document
    /// is HTML rather than a fragment or some other content served as `text/html`.
    pub(super) fn html_evidence(&self) -> Option<usize> {
        return self.html_evidence;
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

    /// Scans the next chunk of the document, and returns the `<head>` start tag if it ends within it.
    /// Only the first one is ever returned.
    pub(super) fn scan(&mut self, chunk: &[u8]) -> Option<HeadOpenTag> {
        for (index, &byte) in chunk.iter().enumerate() {
            let offset = self.offset + index;
            if self.step(byte, offset) {
                self.offset += chunk.len();
                return Some(HeadOpenTag {
                    start: self.markup_start,
                    end: index + 1,
                });
            }
        }
        self.offset += chunk.len();
        return None;
    }

    /// Consumes one byte at `offset`, and returns whether it is the `>` closing the `<head>` tag.
    fn step(&mut self, byte: u8, offset: usize) -> bool {
        self.state = match self.state {
            State::Found | State::Plaintext => self.state,

            State::Data => match byte {
                b'<' => self.open_markup(offset),
                _ => State::Data,
            },

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
                _ if byte.is_ascii_alphabetic() => State::Tag {
                    closing: true,
                    position: TagPosition::TagName,
                },
                // `</` followed by anything but a letter is a bogus comment.
                _ => State::BogusComment,
            },

            State::Tag { closing, position } => {
                match self.step_tag(position, byte) {
                    Some(position) => {
                        if !closing && position != TagPosition::TagName {
                            self.complete_name();
                        }
                        State::Tag { closing, position }
                    }
                    // The tag ends at this `>`.
                    None if closing => State::Data,
                    None => {
                        self.complete_name();
                        if self.name_is(b"head") {
                            self.state = State::Found;
                            return true;
                        }
                        self.state_after_start_tag()
                    }
                }
            }

            State::MarkupDeclarationOpen { dashes } => match (dashes, byte) {
                (0, b'-') => State::MarkupDeclarationOpen { dashes: 1 },
                (_, b'-') => State::Comment {
                    run: 2,
                    content_run: 0,
                    bang: false,
                },
                (_, b'>') => State::Data,
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
                b'>' if (!bang && run == 2) || (bang && content_run == 2) => State::Data,
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
                b'>' => State::Data,
                _ => State::BogusComment,
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
        return State::TagOpen;
    }

    fn push_name(&mut self, byte: u8) {
        if self.name_length < MAX_TAG_NAME {
            self.name[self.name_length] = byte.to_ascii_lowercase();
            self.name_length += 1;
        } else {
            self.name_overflow = true;
        }
    }

    /// Called once the name of a start tag is complete, and possibly again for its attributes: the
    /// first `<html>` or `<meta>` start tag is HTML evidence.
    fn complete_name(&mut self) {
        if self.html_evidence.is_none() && (self.name_is(b"html") || self.name_is(b"meta")) {
            self.html_evidence = Some(self.markup_start);
        }
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
        assert_eq!(find("abc<head>def"), Some((3, 9)));
        assert_eq!(find("<html><HeAd lang=en>"), Some((6, 20)));
        assert_eq!(find("<head/>"), Some((0, 7)));
        assert_eq!(find("<head\n>"), Some((0, 7)));
    }

    #[test]
    fn other_tags() {
        assert_eq!(find("<header><heads><hea><h ead>"), None);
        assert_eq!(find("</head><head>"), Some((7, 13)));
        assert_eq!(find("< head><head>"), Some((7, 13)));
        assert_eq!(find("<<head>"), Some((1, 7)));
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
        assert_eq!(find("<![CDATA[<head>]]>"), None);
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
        assert_eq!(find("<textarea><head></textarea><head>"), Some((27, 33)));
        assert_eq!(find("<script><head>"), None);
        assert_eq!(find("<plaintext></plaintext><head>"), None);
        // Template content is markup.
        assert_eq!(find("<template><head>"), Some((10, 16)));
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
        locator.scan(b"<!-- <html> --><p><meta");
        assert_eq!(locator.html_evidence(), None);
        assert_eq!(locator.pending_tag_start(), Some(18));
        locator.scan(b" charset=utf-8><html>");
        assert_eq!(locator.html_evidence(), Some(18));
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
        locator.scan(b"ab<head");
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
