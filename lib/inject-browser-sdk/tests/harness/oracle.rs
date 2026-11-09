// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Where the snippet must go according to lol_html, Cloudflare's streaming HTML rewriter, used as
//! an independent oracle: its tokenizer and element stack decide what is really a tag, and what is
//! really a child of `<head>`. Nothing is rewritten; only source locations are recorded.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use inject_browser_sdk::injector::InjectionStrategy;
use lol_html::html_content::{Element, TextChunk, TextType};
use lol_html::{doc_text, element, end_tag, HtmlRewriter, Settings};

/// The head window of the injector, `DOCUMENT_CHARSET_SCAN_LIMIT`.
pub const WINDOW: usize = 1024;

/// How far the injector walks a head that is a run of `<meta>` and `<title>` tags, `WALK_LIMIT`.
pub const WALK_LIMIT: usize = 10 * 1024;

/// A text node, and whether it holds more than whitespace.
/// A text node: where it is, its content, and whether it is in the data state.
type TextNode = (Range<usize>, String, bool);

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
    /// Every start tag directly within a `<head>`.
    pub head_children: Vec<HeadChild>,
    /// Every text node holding more than whitespace, wherever it is.
    pub text: Vec<Range<usize>>,
    /// The ones of them in the data state, outside raw text, RCDATA and CDATA, a byte order mark
    /// starting the document aside: the browser closes the head on one.
    pub data_text: Vec<Range<usize>>,
    /// Start of the first `<html>` or `<meta>` start tag.
    pub html_evidence: Option<usize>,
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

/// Elements the head can hold; any other start tag closes it.
const ALLOWED_IN_HEAD: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "noscript", "script", "style",
    "template", "title",
];

/// Runs lol_html over `chunks`, and returns what it saw, or `None` when it reports the markup as
/// ambiguous: some markup cannot be tokenized right without a full tree builder (`<select><xmp>`),
/// and lol_html refuses to guess.
pub fn observe<T: AsRef<[u8]>>(chunks: &[T]) -> Option<Observed> {
    let observed = Rc::new(RefCell::new(Observed::default()));
    let (heads, children, evidence) = (observed.clone(), observed.clone(), observed.clone());
    let templates = observed.clone();
    let body = observed.clone();
    // How many templates are open where an element starts: body content within one does not
    // count, its content being inert.
    let depth: Rc<Cell<usize>> = Rc::default();
    let (template_depth, body_depth) = (depth.clone(), depth);
    let settings = Settings::new()
        .append_element_content_handler(element!("head", move |el: &mut Element| {
            let first = heads.borrow().heads.is_empty();
            heads.borrow_mut().heads.push(el.source_location().bytes());
            let end = heads.clone();
            // A `<head/>` is still a start tag, which can have content.
            let _ = el.on_end_tag(end_tag!(move |tag| {
                let start = tag.source_location().bytes().start;
                let mut observed = end.borrow_mut();
                observed.head_ends.push(start);
                if first {
                    observed.first_head_end = Some(start);
                }
                return Ok(());
            }));
            return Ok(());
        }))
        .append_element_content_handler(element!("head > *", move |el: &mut Element| {
            let index = {
                let mut observed = children.borrow_mut();
                observed.head_children.push(HeadChild {
                    name: el.tag_name().to_ascii_lowercase(),
                    location: el.source_location().bytes(),
                    end: None,
                    charset: el.get_attribute("charset"),
                    http_equiv: el.get_attribute("http-equiv"),
                    content: el.get_attribute("content"),
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
            if body_depth.get() == 0
                && observed.body_content.is_none()
                && !MAY_PRECEDE_HEAD.contains(&el.tag_name().to_ascii_lowercase().as_str())
            {
                observed.body_content = Some(el.source_location().bytes().start);
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
    (observed.text, observed.data_text) = text_nodes(chunks);
    return Some(observed);
}

/// Every text node of the document holding more than whitespace, from a run of lol_html of its own:
/// with a text handler, lol_html tokenizes everything rather than scan for tags, and the two ways
/// disagree on some malformed documents, so the text handler must not change what the element
/// handlers see.
fn text_nodes<T: AsRef<[u8]>>(chunks: &[T]) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    // Text nodes come in chunks, which the ones of the input decide: joined up, so that what is
    // observed does not depend on them.
    let nodes: Rc<RefCell<Vec<TextNode>>> = Rc::default();
    let text = nodes.clone();
    let settings =
        Settings::new().append_document_content_handler(doc_text!(move |chunk: &mut TextChunk| {
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
        }));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    for chunk in chunks {
        if rewriter.write(chunk.as_ref()).is_err() {
            return (Vec::new(), Vec::new());
        }
    }
    if rewriter.end().is_err() {
        return (Vec::new(), Vec::new());
    }
    let blank = |content: &str| return content.bytes().all(|b| return b.is_ascii_whitespace());
    let nodes = nodes.borrow();
    let text = nodes
        .iter()
        .filter(|(range, content, _)| return !range.is_empty() && !blank(content))
        .map(|(range, _, _)| return range.clone())
        .collect();
    let data_text = nodes
        .iter()
        .filter(|(range, content, data)| {
            // A chunk split within the byte order mark may move where its text starts by a byte or
            // two. Character references are decoded, as whitespace ones keep the head open.
            let content = match range.start < 3 {
                true => content.strip_prefix('\u{feff}').unwrap_or(content),
                false => content.as_str(),
            };
            return *data && !range.is_empty() && !blank(&decode_references(content));
        })
        .map(|(range, _, _)| return range.clone())
        .collect();
    return (text, data_text);
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
///   when it starts past the window and no `<html>` or `<meta>` start tag starts within it;
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
/// `observed`: right before the end tag of the first head, if the document looks like HTML (an
/// `<html>`, `<meta>` or `<head>` start tag starts within the window) and no body content comes
/// before it. lol_html sees no head without a `<head>` start tag, see [`Exception::HeadStartOmitted`].
fn head_close_point(observed: &Observed) -> Option<usize> {
    let head = first_head(observed)?;
    let end = head_end(observed, head)?;
    let evidence = observed
        .html_evidence
        .map_or(head.start, |start| return start.min(head.start));
    if evidence >= WINDOW {
        return None;
    }
    if observed
        .body_content
        .is_some_and(|start| return start < end)
    {
        return None;
    }
    // Text closes the head too.
    if text_before(observed, end) {
        return None;
    }
    return Some(end);
}

/// Where [`InjectionStrategy::HeadOpenTag`] must put the snippet, see [`injection_point`].
fn head_open_point(observed: &Observed) -> Option<usize> {
    let head = first_head(observed)?;
    if observed
        .body_content
        .is_some_and(|start| return start < head.start)
        || text_before(observed, head.start)
    {
        return None;
    }
    if head.start >= WINDOW
        && !observed
            .html_evidence
            .is_some_and(|start| return start < WINDOW)
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
    let http_equiv = http_equiv.as_deref().map(str::trim);
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
    /// [`InjectionStrategy::HeadCloseTag`] injects before a `</head>` tag closing a head whose start
    /// tag the document omits, which HTML allows, or stops at it when no HTML evidence precedes it:
    /// lol_html, without a tree builder, sees no head to close before it, and pairs a later
    /// `</head>` with a later `<head>` the browser ignores.
    HeadStartOmitted,
    /// The injector stops at a `<![CDATA[`, whose end it cannot tell without tracking foreign
    /// content: before the `<head>` tag it then never injects, within the head its point is earlier.
    /// lol_html reads one outside foreign content as the bogus comment it is there.
    CdataSection,
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
    if strategy == InjectionStrategy::HeadCloseTag {
        return head_close_exception(&lower, observed, ours, oracle);
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
