// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! Where the snippet must go according to lol_html, Cloudflare's streaming HTML rewriter, used as
//! an independent oracle: its tokenizer and element stack decide what is really a tag, and what is
//! really a child of `<head>`. Nothing is rewritten; only source locations are recorded.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use inject_browser_sdk::injector::InjectionStrategy;
use lol_html::html_content::{Element, TextChunk, TextType};
use lol_html::{doc_comments, doc_text, doctype, element, end_tag, HtmlRewriter, Settings};

/// The head window of the injector, `DOCUMENT_CHARSET_SCAN_LIMIT`.
pub const WINDOW: usize = 1024;

/// How far the injector looks for HTML evidence, comments, doctypes and whitespace not counted,
/// `EVIDENCE_LIMIT`.
pub const EVIDENCE_LIMIT: usize = 10 * 1024;

/// How far the injector walks a head that is a run of `<meta>` and `<title>` tags, `WALK_LIMIT`.
pub const WALK_LIMIT: usize = 10 * 1024;

/// The longest `</head>` tag the injector injects before, `MAX_PENDING_END_TAG`.
pub const MAX_PENDING_END_TAG: usize = 1024;

/// The raw text and RCDATA elements, whose content is text up to their end tag.
const RAW_TEXT: &[&str] = &[
    "script", "style", "title", "textarea", "xmp", "iframe", "noembed", "noframes", "noscript",
];

/// A text node, and whether it holds more than whitespace.
/// A text node: where it is, its content, and whether it is in the data state.
type TextNode = (Range<usize>, String, bool);

/// Where an attribute value is, `Some(None)` for an attribute without one.
type ValueLocation = Option<Option<Range<usize>>>;

/// A start tag directly within a `<head>`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeadChild {
    /// Lower case name.
    pub name: String,
    /// Location of the start tag.
    pub location: Range<usize>,
    /// Where its end tag ends, if it has one.
    pub end: Option<usize>,
    /// The attributes deciding whether the snippet stays behind a `<meta>`, the first of each name.
    pub charset: Option<String>,
    pub http_equiv: Option<String>,
    pub content: Option<String>,
}

/// What lol_html saw of a document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Every `<head>` start tag.
    pub heads: Vec<Range<usize>>,
    /// Where the end tag of the first `<head>` starts, if it has one.
    pub first_head_end: Option<usize>,
    /// Where the end tag of every `<head>` starts, for those that have one.
    pub head_ends: Vec<usize>,
    /// Every `</head>` end tag, from its `<` to its `>`.
    pub head_end_tags: Vec<Range<usize>>,
    /// Every start tag directly within a `<head>`.
    pub head_children: Vec<HeadChild>,
    /// Every text node holding more than whitespace, wherever it is.
    pub text: Vec<Range<usize>>,
    /// The ones of them in the data state, outside raw text, RCDATA and CDATA, a byte order mark
    /// starting the document aside: the browser closes the head on one.
    pub data_text: Vec<Range<usize>>,
    /// Text nodes in the data state holding nothing but whitespace (a byte order mark starting the
    /// document included), outside templates.
    pub blank_data: Vec<Range<usize>>,
    /// Every comment, bogus ones included, and every doctype.
    pub comments: Vec<Range<usize>>,
    /// Start of the first doctype naming `html`.
    pub doctype_html: Option<usize>,
    /// Start of the first `<html>` or `<meta>` start tag.
    pub html_evidence: Option<usize>,
    /// Starts of every `<html>` start tag outside templates.
    pub html_starts: Vec<usize>,
    /// Starts of every `<body>` start tag outside templates.
    pub body_starts: Vec<usize>,
    /// Whether an `<html>` tag makes the document an AMP one, which the snippet stays out of.
    pub amp: bool,
    /// Start of every start tag, wherever it is.
    pub start_tags: Vec<usize>,
    /// Every `<template>` element, from its start tag to the start of its end tag, if it has one.
    pub templates: Vec<(usize, Option<usize>)>,
    /// Start of the first start tag a `<head>` tag cannot follow: past it, the browser has started
    /// the body and ignores one.
    pub body_content: Option<usize>,
}

/// The start tags a `<head>` tag may follow: `html`, and the ones the head may hold.
const MAY_PRECEDE_HEAD: &[&str] = &[
    "html", "head", "base", "basefont", "bgsound", "link", "meta", "noframes", "noscript",
    "script", "style", "template", "title",
];

/// The attributes of an `<html>` tag that make the document an AMP one.
const AMP_ATTRIBUTES: &[&str] = &[
    "amp",
    "\u{26a1}",
    "amp4email",
    "\u{26a1}4email",
    "amp4ads",
    "\u{26a1}4ads",
];

/// Elements the head can hold; any other start tag closes it.
const ALLOWED_IN_HEAD: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "noscript", "script", "style",
    "template", "title",
];

/// Runs lol_html over `chunks`, and returns what it saw, or `None` when it reports the markup as
/// ambiguous: some markup cannot be tokenized right without a full tree builder (`<select><xmp>`),
/// and lol_html refuses to guess.
///
/// lol_html is given the document as the browser tokenizes it: decoded, every sequence of bytes
/// that is no UTF-8 replaced with U+FFFD, a character of text, where lol_html would drop the bytes
/// from its text nodes. What it saw is mapped back to where it is in `chunks`.
pub fn observe<T: AsRef<[u8]>>(chunks: &[T]) -> Option<Observed> {
    let document: Vec<u8> = chunks
        .iter()
        .flat_map(|chunk| return chunk.as_ref().to_vec())
        .collect();
    let (decoded, origin) = decode(&document);
    // The same splits, moved to the decoded document.
    let mut points = Vec::with_capacity(chunks.len());
    let mut end = 0;
    for chunk in &chunks[..chunks.len().saturating_sub(1)] {
        end += chunk.as_ref().len();
        points.push(origin.partition_point(|&at| return at < end));
    }
    let mut decoded_chunks = Vec::with_capacity(chunks.len());
    let mut start = 0;
    for point in points {
        decoded_chunks.push(&decoded[start..point]);
        start = point;
    }
    decoded_chunks.push(&decoded[start..]);
    let observed = observe_decoded(&decoded_chunks)?;
    return Some(observed.mapped(&|at| return origin[at]));
}

/// `document` decoded as UTF-8, with U+FFFD for every maximal subpart of a sequence that is not, as
/// the WHATWG decoder does, and where each of its bytes comes from in `document`, its length last.
fn decode(document: &[u8]) -> (Vec<u8>, Vec<usize>) {
    let mut decoded = Vec::with_capacity(document.len());
    let mut origin = Vec::with_capacity(document.len() + 1);
    let mut at = 0;
    while at < document.len() {
        let (valid, invalid) = match std::str::from_utf8(&document[at..]) {
            Ok(_) => (document.len() - at, 0),
            Err(error) => (
                error.valid_up_to(),
                error
                    .error_len()
                    .unwrap_or(document.len() - at - error.valid_up_to()),
            ),
        };
        decoded.extend_from_slice(&document[at..at + valid]);
        origin.extend(at..at + valid);
        at += valid;
        if invalid > 0 {
            decoded.extend_from_slice("\u{fffd}".as_bytes());
            origin.extend([at; 3]);
            at += invalid;
        }
    }
    origin.push(document.len());
    return (decoded, origin);
}

impl Observed {
    /// The same, every position mapped by `map`.
    fn mapped(self, map: &dyn Fn(usize) -> usize) -> Self {
        let range = |range: Range<usize>| return map(range.start)..map(range.end);
        let ranges = |ranges: Vec<Range<usize>>| return ranges.into_iter().map(range).collect();
        let positions = |positions: Vec<usize>| return positions.into_iter().map(map).collect();
        return Self {
            heads: ranges(self.heads),
            first_head_end: self.first_head_end.map(map),
            head_ends: positions(self.head_ends),
            head_end_tags: ranges(self.head_end_tags),
            head_children: self
                .head_children
                .into_iter()
                .map(|child| {
                    return HeadChild {
                        location: range(child.location),
                        end: child.end.map(map),
                        ..child
                    };
                })
                .collect(),
            text: ranges(self.text),
            data_text: ranges(self.data_text),
            blank_data: ranges(self.blank_data),
            comments: ranges(self.comments),
            doctype_html: self.doctype_html.map(map),
            html_evidence: self.html_evidence.map(map),
            html_starts: positions(self.html_starts),
            body_starts: positions(self.body_starts),
            amp: self.amp,
            start_tags: positions(self.start_tags),
            templates: self
                .templates
                .into_iter()
                .map(|(start, end)| return (map(start), end.map(map)))
                .collect(),
            body_content: self.body_content.map(map),
        };
    }
}

/// [`observe`] on a document that is UTF-8.
fn observe_decoded(chunks: &[&[u8]]) -> Option<Observed> {
    let observed = Rc::new(RefCell::new(Observed::default()));
    let (heads, children, evidence) = (observed.clone(), observed.clone(), observed.clone());
    let templates = observed.clone();
    let body = observed.clone();
    let roots = observed.clone();
    // How many templates are open where an element starts: body content within one does not
    // count, its content being inert.
    let depth: Rc<Cell<usize>> = Rc::default();
    let (template_depth, body_depth) = (depth.clone(), depth.clone());
    let root_depth = depth;
    // Where the `charset`, `http-equiv` and `content` values of each head child are, see below.
    let values: Rc<RefCell<Vec<[ValueLocation; 3]>>> = Rc::default();
    let child_values = values.clone();
    let settings = Settings::new()
        .append_element_content_handler(element!("head", move |el: &mut Element| {
            let first = heads.borrow().heads.is_empty();
            heads.borrow_mut().heads.push(el.source_location().bytes());
            let end = heads.clone();
            // A `<head/>` is still a start tag, which can have content.
            let _ = el.on_end_tag(end_tag!(move |tag| {
                let location = tag.source_location().bytes();
                let start = location.start;
                let mut observed = end.borrow_mut();
                observed.head_ends.push(start);
                observed.head_end_tags.push(location);
                if first {
                    observed.first_head_end = Some(start);
                }
                return Ok(());
            }));
            return Ok(());
        }))
        .append_element_content_handler(element!("head > *", move |el: &mut Element| {
            // lol_html decodes an attribute value as a document of its own, which drops a byte order
            // mark starting it: where each value is, to read it as the browser does once the
            // document is complete.
            let value = |name: &str| {
                return el
                    .attributes()
                    .iter()
                    .find(|attribute| return attribute.name() == name)
                    .map(|attribute| {
                        return attribute
                            .value_source_location()
                            .map(|location| return location.bytes());
                    });
            };
            values
                .borrow_mut()
                .push([value("charset"), value("http-equiv"), value("content")]);
            let index = {
                let mut observed = children.borrow_mut();
                observed.head_children.push(HeadChild {
                    name: el.tag_name().to_ascii_lowercase(),
                    location: el.source_location().bytes(),
                    ..HeadChild::default()
                });
                observed.head_children.len() - 1
            };
            let end = children.clone();
            // Void elements have no end tag to wait for.
            let _ = el.on_end_tag(end_tag!(move |tag| {
                end.borrow_mut().head_children[index].end = Some(tag.source_location().bytes().end);
                return Ok(());
            }));
            return Ok(());
        }))
        .append_element_content_handler(element!("template", move |el: &mut Element| {
            let index = {
                let mut observed = templates.borrow_mut();
                observed
                    .templates
                    .push((el.source_location().bytes().start, None));
                observed.templates.len() - 1
            };
            let end = templates.clone();
            template_depth.set(template_depth.get() + 1);
            let end_depth = template_depth.clone();
            let _ = el.on_end_tag(end_tag!(move |tag| {
                end.borrow_mut().templates[index].1 = Some(tag.source_location().bytes().start);
                end_depth.set(end_depth.get().saturating_sub(1));
                return Ok(());
            }));
            return Ok(());
        }))
        .append_element_content_handler(element!("*", move |el: &mut Element| {
            let mut observed = body.borrow_mut();
            observed.start_tags.push(el.source_location().bytes().start);
            if body_depth.get() == 0
                && observed.body_content.is_none()
                && !MAY_PRECEDE_HEAD.contains(&el.tag_name().to_ascii_lowercase().as_str())
            {
                observed.body_content = Some(el.source_location().bytes().start);
            }
            return Ok(());
        }))
        .append_element_content_handler(element!("html, body", move |el: &mut Element| {
            if root_depth.get() > 0 {
                return Ok(());
            }
            let mut observed = roots.borrow_mut();
            let start = el.source_location().bytes().start;
            match el.tag_name().eq_ignore_ascii_case("html") {
                true => {
                    observed.html_starts.push(start);
                    observed.amp |= AMP_ATTRIBUTES
                        .iter()
                        .any(|name| return el.has_attribute(name));
                }
                false => observed.body_starts.push(start),
            }
            return Ok(());
        }))
        .append_element_content_handler(element!("html, meta", move |el: &mut Element| {
            let mut evidence = evidence.borrow_mut();
            if evidence.html_evidence.is_none() {
                evidence.html_evidence = Some(el.source_location().bytes().start);
            }
            return Ok(());
        }));

    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    for chunk in chunks {
        rewriter.write(chunk.as_ref()).ok()?;
    }
    rewriter.end().ok()?;

    let mut observed = observed.borrow().clone();
    let document = chunks.concat();
    let read = |value: &ValueLocation| {
        return value.as_ref().map(|location| {
            return location.as_ref().map_or_else(String::new, |location| {
                return String::from_utf8_lossy(&document[location.clone()]).into_owned();
            });
        });
    };
    for (child, [charset, http_equiv, content]) in observed
        .head_children
        .iter_mut()
        .zip(child_values.borrow().iter())
    {
        child.charset = read(charset);
        child.http_equiv = read(http_equiv);
        child.content = read(content);
    }
    let nodes = text_nodes(chunks);
    observed.text = nodes.text;
    observed.data_text = nodes.data_text;
    observed.blank_data = nodes.blank_data;
    observed.comments = nodes.comments;
    observed.doctype_html = nodes.doctype_html;
    return Some(observed);
}

/// Every text node of the document holding more than whitespace, from a run of lol_html of its own:
/// with a text handler, lol_html tokenizes everything rather than scan for tags, and the two ways
/// disagree on some malformed documents, so the text handler must not change what the element
/// handlers see.
/// What [`text_nodes`] gathers from its run of lol_html.
#[derive(Default)]
struct TextNodes {
    text: Vec<Range<usize>>,
    data_text: Vec<Range<usize>>,
    blank_data: Vec<Range<usize>>,
    comments: Vec<Range<usize>>,
    doctype_html: Option<usize>,
}

fn text_nodes<T: AsRef<[u8]>>(chunks: &[T]) -> TextNodes {
    // Text nodes come in chunks, which the ones of the input decide: joined up, so that what is
    // observed does not depend on them. Comments and doctypes come from the same run, which
    // tokenizes everything.
    let nodes: Rc<RefCell<Vec<TextNode>>> = Rc::default();
    let markup: Rc<RefCell<TextNodes>> = Rc::default();
    let (text, comments, doctypes) = (nodes.clone(), markup.clone(), markup.clone());
    let settings = Settings::new()
        .append_document_content_handler(doc_text!(move |chunk: &mut TextChunk| {
            let range = chunk.source_location().bytes();
            let data = chunk.text_type() == TextType::Data;
            let mut nodes = text.borrow_mut();
            match nodes.last_mut() {
                Some((last, content, _)) if last.end == range.start => {
                    last.end = range.end;
                    content.push_str(chunk.as_str());
                }
                _ => nodes.push((range, String::from(chunk.as_str()), data)),
            }
            return Ok(());
        }))
        .append_document_content_handler(doc_comments!(move |comment| {
            comments
                .borrow_mut()
                .comments
                .push(comment.source_location().bytes());
            return Ok(());
        }))
        .append_document_content_handler(doctype!(move |doctype| {
            let location = doctype.source_location().bytes();
            let mut markup = doctypes.borrow_mut();
            let html = doctype
                .name()
                .is_some_and(|name| return name.eq_ignore_ascii_case("html"));
            if html && markup.doctype_html.is_none() {
                markup.doctype_html = Some(location.start);
            }
            markup.comments.push(location);
            return Ok(());
        }));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    for chunk in chunks {
        if rewriter.write(chunk.as_ref()).is_err() {
            return TextNodes::default();
        }
    }
    if rewriter.end().is_err() {
        return TextNodes::default();
    }
    let blank = |content: &str| return content.bytes().all(|b| return b.is_ascii_whitespace());
    // A chunk split within the byte order mark may move where its text starts by a byte or two.
    let without_mark = |range: &Range<usize>, content: &'_ str| -> String {
        return match range.start < 3 {
            true => String::from(content.strip_prefix('\u{feff}').unwrap_or(content)),
            false => String::from(content),
        };
    };
    let mut gathered = std::mem::take(&mut *markup.borrow_mut());
    // A chunk split within a character makes the text after it start at the split: back to the
    // start of the character, which is where the browser reads it. The document is UTF-8, see
    // [`observe`].
    let document: Vec<u8> = chunks
        .iter()
        .flat_map(|chunk| return chunk.as_ref().to_vec())
        .collect();
    let char_start = |mut position: usize| {
        while position > 0
            && document
                .get(position)
                .is_some_and(|&b| return b & 0xC0 == 0x80)
        {
            position -= 1;
        }
        return position;
    };
    for (range, content, data) in nodes.borrow().iter() {
        if range.is_empty() {
            continue;
        }
        if !blank(content) {
            gathered.text.push(range.clone());
        }
        if *data {
            // Character references are decoded, as whitespace ones keep the head open.
            let unmarked = without_mark(range, content);
            match blank(&decode_references(&unmarked)) {
                true => gathered.blank_data.push(range.clone()),
                // The head closes on the first byte that is not blank: where the snippet goes.
                false => {
                    let start =
                        range.start + (content.len() - unmarked.len()) + leading_blank(&unmarked);
                    gathered.data_text.push(char_start(start)..range.end);
                }
            }
        }
    }
    return gathered;
}

/// How many bytes at the start of `text` are whitespace, or character references to it.
fn leading_blank(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index] != b'&' {
            break;
        }
        let length = bytes[index + 1..]
            .iter()
            .position(|&b| return !(b.is_ascii_alphanumeric() || b == b'#'))
            .map_or(bytes.len() - index, |end| return end + 1);
        let end = match bytes.get(index + length) {
            Some(b';') => index + length + 1,
            _ => index + length,
        };
        let decoded = decode_references(&text[index..end]);
        if decoded.is_empty() || !decoded.bytes().all(|b| return b.is_ascii_whitespace()) {
            break;
        }
        index = end;
    }
    return index;
}

/// `text` with its numeric character references (the `;` optional) and `&Tab;` / `&NewLine;`
/// decoded, the ones that can stand for ASCII; any other `&` is left as it is.
fn decode_references(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        decoded.push_str(&rest[..at]);
        rest = &rest[at..];
        let after = &rest[1..];
        if let Some(tail) = after.strip_prefix("Tab;") {
            decoded.push('\t');
            rest = tail;
            continue;
        }
        if let Some(tail) = after.strip_prefix("NewLine;") {
            decoded.push('\n');
            rest = tail;
            continue;
        }
        let (radix, digits) = match after.as_bytes() {
            [b'#', b'x' | b'X', ..] => (16, &after[2..]),
            [b'#', ..] => (10, &after[1..]),
            _ => {
                decoded.push('&');
                rest = after;
                continue;
            }
        };
        let length = digits
            .bytes()
            .take_while(|b| return (*b as char).is_digit(radix))
            .count();
        if length == 0 {
            decoded.push('&');
            rest = after;
            continue;
        }
        let code = u32::from_str_radix(&digits[..length], radix).unwrap_or(u32::MAX);
        decoded.push(
            char::from_u32(code)
                .filter(|c| return *c != '\0')
                .unwrap_or('\u{fffd}'),
        );
        rest = &digits[length..];
        rest = rest.strip_prefix(';').unwrap_or(rest);
    }
    decoded.push_str(rest);
    return decoded;
}

/// Where the snippet must go in a document lol_html saw as `observed`, or `None` if it must not be
/// injected at all:
/// * nowhere without a `<head>` tag, nor past body content (see [`Observed::body_content`]), nor
///   when it starts past the window and no `<html>` or `<meta>` start tag or `<!DOCTYPE html>`
///   precedes it within the evidence limit;
/// * otherwise past the last `<meta>` child of the first head declaring the character encoding,
///   `X-UA-Compatible` or a `Content-Security-Policy`, and coming before any child the head cannot hold, or else right after the
///   `<head>` tag. Children are looked at within the window, and past it while every one since the
///   `<head>` tag is a `<meta>` or a `<title>`, with no text but whitespace between them, up to
///   [`WALK_LIMIT`].
pub fn injection_point(strategy: InjectionStrategy, observed: &Observed) -> Option<usize> {
    return match strategy {
        InjectionStrategy::HeadOpenTag => head_open_point(observed),
        InjectionStrategy::HeadCloseTag => head_close_point(observed),
    };
}

/// `position`, counting only the bytes that tell whether the document is HTML: comments,
/// doctypes and blank text before it are not counted, as the injector does not count them.
fn significant(observed: &Observed, position: usize) -> usize {
    let free: usize = observed
        .comments
        .iter()
        .chain(
            observed
                .blank_data
                .iter()
                .filter(|text| return !in_template(observed, text.start)),
        )
        .filter(|range| return range.end <= position)
        .map(|range| return range.len())
        .sum();
    return position - free;
}

/// Whether `start`, where some HTML evidence starts, is within the limit.
fn within_evidence_limit(observed: &Observed, start: usize) -> bool {
    return significant(observed, start) < EVIDENCE_LIMIT;
}

/// Whether `position` is within the content of a `<template>`, which is inert.
fn in_template(observed: &Observed, position: usize) -> bool {
    return observed.templates.iter().any(|&(start, end)| {
        return start < position && end.is_none_or(|end| return end > position);
    });
}

/// The first `<head>` start tag outside templates, the one the browser opens the head with.
fn first_head(observed: &Observed) -> Option<&Range<usize>> {
    return observed
        .heads
        .iter()
        .find(|head| return !in_template(observed, head.start));
}

/// Where the end tag closing the head starting at `head` starts: the first one past it, outside
/// templates. The browser ignores a `<head>` tag within the head, which lol_html pairs that
/// `</head>` with instead.
fn head_end(observed: &Observed, head: &Range<usize>) -> Option<usize> {
    return observed
        .head_ends
        .iter()
        .copied()
        .filter(|&end| return end > head.start && !in_template(observed, end))
        .min();
}

/// Whether data text outside templates starts before `position`: the browser closes the head on it,
/// and opens the body.
fn text_before(observed: &Observed, position: usize) -> bool {
    return observed.data_text.iter().any(|text| {
        return text.start < position && !in_template(observed, text.start);
    });
}

/// Where [`InjectionStrategy::HeadCloseTag`] must put the snippet in a document lol_html saw as
/// `observed`: right before the end tag of the first head, or right before where the browser
/// closes the head without one, body content or text, whichever comes first.
///
/// Before a `</head>` tag, the document has to look like HTML (an `<html>`, `<meta>` or `<head>`
/// start tag, a doctype, or the end tag itself, within the evidence limit). Before where the head
/// closes, a doctype, an `<html>` or a `<head>` start tag has to precede it, or it has to be a
/// `<body>` tag; without, past body content, the snippet goes right before the first `<html>` or
/// `<body>` tag that follows. Never in an AMP document. lol_html sees no head without a `<head>`
/// start tag, see [`Exception::HeadStartOmitted`], nor the end tags that close one, see
/// [`Exception::EndTagClosingHead`], nor the end of the document, see [`Exception::EndOfDocument`].
fn head_close_point(observed: &Observed) -> Option<usize> {
    if observed.amp {
        return None;
    }
    let end = first_head(observed).and_then(|head| return head_end(observed, head));
    let text = observed
        .data_text
        .iter()
        .map(|text| return text.start)
        .filter(|&start| return !in_template(observed, start))
        .min();
    let closes = [observed.body_content, text].into_iter().flatten().min();
    if let Some(end) = end.filter(|&end| return closes.is_none_or(|closes| return closes > end)) {
        // The first `<head>` start tag counts as evidence wherever it is, as the injector reads it.
        let any_head = observed
            .heads
            .first()
            .map_or(end, |first| return first.start);
        let evidence = [
            observed.html_evidence,
            observed.doctype_html,
            Some(any_head),
            Some(end),
        ]
        .into_iter()
        .flatten()
        .min()?;
        return within_evidence_limit(observed, evidence).then_some(end);
    }
    let point = closes?;
    let heads = observed.heads.first().map(|head| return head.start);
    let strong = [
        observed.html_starts.first().copied(),
        observed.doctype_html,
        heads,
    ]
    .into_iter()
    .flatten()
    .any(|start| return start < point)
        || observed.body_starts.contains(&point);
    if strong {
        let evidence = [
            observed.html_evidence,
            observed.doctype_html,
            heads,
            observed.body_starts.first().copied(),
        ]
        .into_iter()
        .flatten()
        .min()?;
        return within_evidence_limit(observed, evidence).then_some(point);
    }
    // Text ends the search, body content goes on to the evidence the body holds.
    if observed.body_content != Some(point) {
        return None;
    }
    let found = observed
        .html_starts
        .iter()
        .chain(&observed.body_starts)
        .copied()
        .filter(|&start| return start > point)
        .min()?;
    return within_evidence_limit(observed, found).then_some(found);
}

/// Where [`InjectionStrategy::HeadOpenTag`] must put the snippet, see [`injection_point`].
fn head_open_point(observed: &Observed) -> Option<usize> {
    if observed.amp {
        return None;
    }
    let head = first_head(observed)?;
    if observed
        .body_content
        .is_some_and(|start| return start < head.start)
        || text_before(observed, head.start)
    {
        return None;
    }
    // Past the window, the `<head>` tag is injected after only if HTML evidence precedes it: the
    // injector decides there, on what it has seen.
    if head.start >= WINDOW
        && ![observed.html_evidence, observed.doctype_html]
            .into_iter()
            .flatten()
            .min()
            .is_some_and(|start| {
                return start < head.start && within_evidence_limit(observed, start);
            })
    {
        return None;
    }

    let head_end = head_end(observed, head).unwrap_or(usize::MAX);
    // Found past the window, the `<head>` tag is not walked from: the snippet goes right after it.
    if head.end > WINDOW {
        return Some(head.end);
    }
    let mut point = head.end;
    let mut leading = true;
    let mut gap_start = head.end;
    for child in &observed.head_children {
        let location = &child.location;
        // A template's content is inert, children of a `<head>` within one included.
        if location.start < head.end
            || location.start >= head_end
            || in_template(observed, location.start)
        {
            continue;
        }
        let name = child.name.as_str();
        // The browser ignores a `<head>` start tag within the head, and merges an `<html>` one into
        // the root element: neither ends it.
        if name == "head" || name == "html" {
            continue;
        }
        if !ALLOWED_IN_HEAD.contains(&name) {
            break;
        }
        // Text closes the head.
        if observed.data_text.iter().any(|text| {
            return gap_start <= text.start
                && text.start < location.start
                && !in_template(observed, text.start);
        }) {
            break;
        }
        let in_run = name == "meta" || name == "title";
        leading &= in_run
            && !observed
                .text
                .iter()
                .any(|text| return gap_start <= text.start && text.start < location.start);
        if location.start >= WINDOW && !leading {
            break;
        }
        if location.end > WALK_LIMIT {
            break;
        }
        if name == "meta" && is_relevant(child) {
            point = location.end;
        }
        gap_start = child.end.unwrap_or(location.end);
    }
    return Some(point);
}

/// Whether the snippet has to stay behind `meta`: it declares the character encoding, per the HTML
/// spec's prescan, or is `X-UA-Compatible` or `Content-Security-Policy` (the enforcing one: a
/// report-only policy is ignored in a meta).
fn is_relevant(meta: &HeadChild) -> bool {
    let known = |label: &str| return encoding_rs::Encoding::for_label(label.as_bytes()).is_some();
    // The parser decodes character references in attribute values.
    let http_equiv = meta.http_equiv.as_deref().map(decode_references);
    // Only ASCII whitespace is stripped, as the injector does: a no-break space is part of the value.
    let http_equiv = http_equiv.as_deref().map(str::trim_ascii);
    if http_equiv.is_some_and(|value| {
        return value.eq_ignore_ascii_case("x-ua-compatible")
            || value.eq_ignore_ascii_case("content-security-policy");
    }) {
        return true;
    }
    if let Some(charset) = &meta.charset {
        return known(charset);
    }
    return http_equiv.is_some_and(|value| return value.eq_ignore_ascii_case("content-type"))
        && meta
            .content
            .as_deref()
            .and_then(charset_parameter)
            .is_some_and(known);
}

/// The HTML spec's algorithm for extracting a character encoding from a meta element, on a
/// `content` value.
fn charset_parameter(content: &str) -> Option<&str> {
    let lower = content.to_ascii_lowercase();
    let mut position = 0;
    loop {
        position += lower[position..].find("charset")? + "charset".len();
        let rest = content[position..].trim_start_matches(|c: char| return c.is_ascii_whitespace());
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start_matches(|c: char| return c.is_ascii_whitespace());
        return match rest.chars().next()? {
            quote @ ('"' | '\'') => Some(&rest[1..1 + rest[1..].find(quote)?]),
            _ => Some(
                rest.split(|c: char| return c == ';' || c.is_ascii_whitespace())
                    .next()?,
            ),
        };
    }
}

/// Why the injector may place the snippet elsewhere than lol_html says. Each one is a deliberate
/// difference, not a bug in either.
#[derive(Debug, PartialEq, Eq)]
pub enum Exception {
    /// lol_html reported the markup as ambiguous, so there is nothing to compare with.
    Ambiguous,
    /// The first `<head>` tag is within a `<template>`, where lol_html closes it along with the
    /// template. The injector does not track templates before the head, which documents do not
    /// have: it takes the tag for the document's head.
    HeadInTemplate,
    /// Foreign content (`<svg>`, `<math>`): lol_html tracks it, so `<style>` there is markup and
    /// `<![CDATA[` is a section; the injector does not, and documents put none before their head.
    /// lol_html is not even run on such documents: some make it panic (lol_html 3.0.3,
    /// "Tag should be a start tag at this point"), which a fuzz target cannot recover from.
    ForeignContent,
    /// An end tag the `in head` (or `in template`) insertion mode ignores, and the injector too, for
    /// an element lol_html's stack holds below the head or a template: one opened before the
    /// `<head>` tag (`<div><head>…</div>`), a `</head>` within a template in the head, or a `</div>`
    /// within a template nested in another. lol_html pops the head or the template along with it,
    /// so a template the injector still sees open, and stops its walk at, is closed for lol_html.
    StrayEndTag,
    /// The injector stops its walk at a `</br>`, `</body>` or `</html>` end tag, where the `in head`
    /// insertion mode closes the head. lol_html only pops elements its stack holds, and keeps the
    /// head open when it holds none of those.
    EndTagClosingHead,
    /// A `<head>` or `<html>` start tag within the head, which the browser ignores or merges into the
    /// root element, the injector walking on: lol_html takes what follows for the content of that
    /// element, out of the head.
    IgnoredStartTag,
    /// The injector places the snippet within a head whose start tag the document omits, which HTML
    /// allows, or the browser ignores, past where it closed a head it opened: before a `</head>`
    /// tag closing it, or, with [`InjectionStrategy::HeadOpenTag`], walking it from past the
    /// doctype and `<html>` tag. lol_html, without a tree builder, sees no head there, and pairs a
    /// later `</head>` with a later `<head>` the browser ignores.
    HeadStartOmitted,
    /// The injector stops at a `<![CDATA[`, whose end it cannot tell without tracking foreign
    /// content: before the `<head>` tag it then never injects, within the head its point is earlier.
    /// lol_html reads one outside foreign content as the bogus comment it is there.
    CdataSection,
    /// The document ends within a head the browser opened, which closes with it: the injector
    /// injects at its end, where lol_html sees no head end.
    EndOfDocument,
    /// The injector tells body content by the name of its start tag, and injects before one the
    /// document ends within, a quote left open for instance: the browser drops it, closing the head
    /// with the document, while lol_html sees no element at all.
    UnfinishedTag,
    /// A `</head>` tag longer than the injector holds back looking for its `>`,
    /// [`MAX_PENDING_END_TAG`] bytes: only one stuffed with attributes is, and the injector gives up
    /// on it rather than hold back without bound.
    LongEndTag,
    /// A raw text or RCDATA element (`<script>`, `<title>`…) whose start tag ends in an empty
    /// unquoted attribute value (`<script c=>`): lol_html 3.0.3 tokenizes it rightly when it only
    /// looks for tags, and reads on past its `>` when it tokenizes everything, as its text handler
    /// makes it, so the text nodes it reports past it are wrong. The browser, and the injector, end
    /// the tag at its `>`.
    EmptyAttributeValue,
    /// The injector keeps the snippet behind a character encoding declaration the browser's
    /// prescan honours, which lol_html knows nothing about: within a raw text or inert element
    /// (later point), or past where the head ends or within a tag the window ends in (no point).
    EncodingDeclaration,
}

/// Returns the exception explaining why `ours` differs from `oracle` for `document`, which lol_html
/// saw as `observed`, or `None` if nothing does.
pub fn exception(
    strategy: InjectionStrategy,
    document: &[u8],
    observed: &Observed,
    ours: Option<usize>,
    oracle: Option<usize>,
) -> Option<Exception> {
    let lower = document.to_ascii_lowercase();
    // As long as lol_html's tokenizer has it, quoted values hiding any `>` they hold.
    if observed.head_end_tags.iter().any(|tag| {
        return oracle == Some(tag.start) && tag.len() > MAX_PENDING_END_TAG;
    }) {
        return Some(Exception::LongEndTag);
    }
    let last = [ours, oracle].into_iter().flatten().max().unwrap_or(0);
    if empty_attribute_value_on_raw_text(&lower[..last.min(lower.len())]) {
        return Some(Exception::EmptyAttributeValue);
    }
    if ours == Some(document.len()) && oracle.is_none() {
        return Some(Exception::EndOfDocument);
    }
    if let Some(ours) = ours {
        let tag = lower.get(ours..ours + 2).unwrap_or_default();
        if tag.first() == Some(&b'<')
            && tag.get(1).is_some_and(u8::is_ascii_alphabetic)
            && !observed.start_tags.contains(&ours)
            && oracle.is_none_or(|oracle| return oracle > ours)
        {
            return Some(Exception::UnfinishedTag);
        }
    }
    // The injector stops at, or injects before, an end tag the head closes on, which lol_html
    // knows nothing of without a tree builder.
    let first = [ours, oracle]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(document.len());
    if [b"</br".as_slice(), b"</body", b"</html"]
        .iter()
        .any(|end_tag| return contains(&lower[..(first + 6).min(lower.len())], end_tag))
    {
        return Some(Exception::EndTagClosingHead);
    }
    // Declined, or held back, before a declaration that the snippet would push out of the window.
    if let (None, Some(point)) = (ours, oracle) {
        if point < WINDOW && contains(&lower[point..lower.len().min(WINDOW)], b"charset") {
            return Some(Exception::EncodingDeclaration);
        }
    }
    if strategy == InjectionStrategy::HeadCloseTag {
        return head_close_exception(&lower, observed, ours, oracle);
    }
    // A head without a start tag, or past one the browser ignores.
    if let (Some(ours), None) = (ours, oracle) {
        let ignored = |head: &Range<usize>| {
            return head.start > ours
                || observed
                    .body_content
                    .is_some_and(|start| return start < head.start)
                || text_before(observed, head.start);
        };
        if first_head(observed).is_none_or(ignored) {
            return Some(Exception::HeadStartOmitted);
        }
    }
    // A `</head>` before the head lol_html sees closed the head the browser opened for it: the
    // `<head>` tag that follows is ignored, the injector reading on in the body when that head
    // closed ahead of any evidence.
    if first_head(observed).is_some_and(|head| return contains(&lower[..head.start], b"</head")) {
        return Some(Exception::HeadStartOmitted);
    }
    let window = &lower[..lower.len().min(WINDOW)];
    if let Some(head) = observed.heads.first() {
        let in_template = |&(start, end): &(usize, Option<usize>)| {
            return start < head.start && end.is_none_or(|end| return end > head.start);
        };
        if observed.templates.iter().any(in_template) {
            return Some(Exception::HeadInTemplate);
        }
    }
    match (ours, oracle) {
        (Some(ours), Some(oracle))
            if [b"<html".as_slice(), b"<head"]
                .iter()
                .any(|tag| return contains(&lower[ours.min(oracle)..ours.max(oracle)], tag)) =>
        {
            return Some(Exception::IgnoredStartTag);
        }
        (Some(ours), Some(oracle)) if ours < oracle => {
            if contains(&lower[ours..oracle], b"<![cdata[") {
                return Some(Exception::CdataSection);
            }
            let between = &lower[ours..oracle];
            if contains(between, b"<template") && contains(between, b"</") {
                return Some(Exception::StrayEndTag);
            }
            if [b"</br".as_slice(), b"</body", b"</html"]
                .iter()
                .any(|end_tag| return contains(between, end_tag))
            {
                return Some(Exception::EndTagClosingHead);
            }
        }
        (Some(ours), Some(oracle)) if ours > oracle => {
            if contains(&lower[oracle..ours], b"charset") {
                return Some(Exception::EncodingDeclaration);
            }
            if contains(&lower[oracle..ours], b"</") {
                return Some(Exception::StrayEndTag);
            }
        }
        // Declined: the declaration lies somewhere past the `<head>` tag, possibly behind a CDATA
        // section or an unclosed element the walk stops at, so possibly before `oracle` too.
        (None, Some(_)) => {
            let head_start = observed.heads.first().map_or(0, |head| return head.start);
            if contains(&lower[..head_start], b"<![cdata[") {
                return Some(Exception::CdataSection);
            }
            let head_end = observed.heads.first().map_or(0, |head| return head.end);
            if contains(&window[head_end.min(window.len())..], b"charset") {
                return Some(Exception::EncodingDeclaration);
            }
        }
        _ => {}
    }
    return None;
}

/// [`exception`] for [`InjectionStrategy::HeadCloseTag`], given the lower case document.
fn head_close_exception(
    lower: &[u8],
    observed: &Observed,
    ours: Option<usize>,
    oracle: Option<usize>,
) -> Option<Exception> {
    if let Some(head) = observed.heads.first() {
        let in_template = |&(start, end): &(usize, Option<usize>)| {
            return start < head.start && end.is_none_or(|end| return end > head.start);
        };
        if observed.templates.iter().any(in_template) {
            return Some(Exception::HeadInTemplate);
        }
    }
    match (ours, oracle) {
        // lol_html closed a template still open at this `</head>`, along with the head: the
        // browser, and the injector, ignore an end tag within a template.
        (_, Some(end))
            if observed
                .templates
                .iter()
                .any(|&(_, template_end)| return template_end == Some(end)) =>
        {
            return Some(Exception::StrayEndTag);
        }
        // lol_html closed a template at a `</head>` within it, which the browser, and the
        // injector, ignore: the template it took the next `</template>` for still holds what
        // follows, until the one after.
        (_, Some(end))
            if observed.templates.iter().any(|&(_, template_end)| {
                return template_end.is_some_and(|template_end| {
                    return template_end < end && lower[template_end..].starts_with(b"</head");
                });
            }) =>
        {
            return Some(Exception::StrayEndTag);
        }
        // lol_html closed the head on another end tag, a `</template>` popping it along: the
        // `in head` insertion mode, and the injector, ignore those.
        (None, Some(end))
            if !lower[end..].starts_with(b"</head")
                && !lower[end..].starts_with(b"</html")
                && !lower[end..].starts_with(b"</body")
                && !lower[end..].starts_with(b"</br") =>
        {
            return Some(Exception::StrayEndTag);
        }
        (Some(ours), _)
            if observed
                .heads
                .first()
                .is_none_or(|head| return head.start > ours) =>
        {
            return Some(Exception::HeadStartOmitted);
        }
        // A `</head>` before the head lol_html sees: it closed a head whose start tag the document
        // omits, for the browser and the injector alike, and no later one counts.
        (None, Some(_))
            if observed
                .heads
                .first()
                .is_some_and(|head| return contains(&lower[..head.start], b"</head")) =>
        {
            return Some(Exception::HeadStartOmitted);
        }
        (None, Some(end)) => {
            let head_start = observed.heads.first().map_or(0, |head| return head.start);
            // Up to and including the end tag lol_html closes the head at: the one at `end` is not
            // a `</head>` one when one of those closed it.
            let before = &lower[head_start.min(end)..(end + 6).min(lower.len())];
            if contains(&lower[..end], b"<![cdata[") {
                return Some(Exception::CdataSection);
            }
            if [b"</br".as_slice(), b"</body", b"</html"]
                .iter()
                .any(|end_tag| return contains(before, end_tag))
            {
                return Some(Exception::EndTagClosingHead);
            }
        }
        _ => {}
    }
    return None;
}

/// Whether a start tag of a raw text or RCDATA element in `lower` ends in an empty unquoted attribute
/// value, see [`Exception::EmptyAttributeValue`].
fn empty_attribute_value_on_raw_text(lower: &[u8]) -> bool {
    return RAW_TEXT.iter().any(|name| {
        let open = [b"<".as_slice(), name.as_bytes()].concat();
        return lower
            .windows(open.len())
            .enumerate()
            .filter(|(_, window)| return *window == open.as_slice())
            .any(|(at, _)| {
                let tag = &lower[at + open.len()..];
                let Some(end) = tag.iter().position(|&b| return b == b'>') else {
                    return false;
                };
                let attributes = tag[..end].trim_ascii_end();
                return tag.first().is_some_and(u8::is_ascii_whitespace)
                    && attributes.ends_with(b"=");
            });
    });
}

/// Whether `document` holds foreign content, see [`Exception::ForeignContent`].
pub fn has_foreign_content(document: &[u8]) -> bool {
    let lower = document.to_ascii_lowercase();
    return contains(&lower, b"<svg") || contains(&lower, b"<math");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    return haystack
        .windows(needle.len())
        .any(|window| return window == needle);
}
