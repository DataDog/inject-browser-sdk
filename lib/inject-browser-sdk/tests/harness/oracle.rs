// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Where the snippet must go according to lol_html, Cloudflare's streaming HTML rewriter, used as
//! an independent oracle: its tokenizer and element stack decide what is really a tag, and what is
//! really a child of `<head>`. Nothing is rewritten; only source locations are recorded.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use lol_html::html_content::{Element, TextChunk};
use lol_html::{doc_text, element, end_tag, HtmlRewriter, Settings};

/// The head window of the injector, `DOCUMENT_CHARSET_SCAN_LIMIT`.
pub const WINDOW: usize = 1024;

/// How far the injector walks a head that is a run of `<meta>` and `<title>` tags, `WALK_LIMIT`.
pub const WALK_LIMIT: usize = 10 * 1024;

/// A text node, and whether it holds more than whitespace.
type TextNode = (Range<usize>, bool);

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
    /// Every start tag directly within a `<head>`.
    pub head_children: Vec<HeadChild>,
    /// Every text node holding more than whitespace, wherever it is.
    pub text: Vec<Range<usize>>,
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
    let settings = Settings::new()
        .append_element_content_handler(element!("head", move |el: &mut Element| {
            let first = heads.borrow().heads.is_empty();
            heads.borrow_mut().heads.push(el.source_location().bytes());
            if first {
                let end = heads.clone();
                // A `<head/>` is still a start tag, which can have content.
                let _ = el.on_end_tag(end_tag!(move |tag| {
                    end.borrow_mut().first_head_end = Some(tag.source_location().bytes().start);
                    return Ok(());
                }));
            }
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
            let _ = el.on_end_tag(end_tag!(move |tag| {
                end.borrow_mut().templates[index].1 = Some(tag.source_location().bytes().start);
                return Ok(());
            }));
            return Ok(());
        }))
        .append_element_content_handler(element!("*", move |el: &mut Element| {
            let mut observed = body.borrow_mut();
            if observed.body_content.is_none()
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
    observed.text = text_nodes(chunks);
    return Some(observed);
}

/// Every text node of the document holding more than whitespace, from a run of lol_html of its own:
/// with a text handler, lol_html tokenizes everything rather than scan for tags, and the two ways
/// disagree on some malformed documents, so the text handler must not change what the element
/// handlers see.
fn text_nodes<T: AsRef<[u8]>>(chunks: &[T]) -> Vec<Range<usize>> {
    // Text nodes come in chunks, which the ones of the input decide: joined up, so that what is
    // observed does not depend on them, along with whether they hold more than whitespace.
    let nodes: Rc<RefCell<Vec<TextNode>>> = Rc::default();
    let text = nodes.clone();
    let settings =
        Settings::new().append_document_content_handler(doc_text!(move |chunk: &mut TextChunk| {
            let range = chunk.source_location().bytes();
            let blank = chunk
                .as_str()
                .bytes()
                .all(|b| return b.is_ascii_whitespace());
            let mut nodes = text.borrow_mut();
            match nodes.last_mut() {
                Some((last, holds_text)) if last.end == range.start => {
                    last.end = range.end;
                    *holds_text |= !blank;
                }
                _ => nodes.push((range, !blank)),
            }
            return Ok(());
        }));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    for chunk in chunks {
        if rewriter.write(chunk.as_ref()).is_err() {
            return Vec::new();
        }
    }
    if rewriter.end().is_err() {
        return Vec::new();
    }
    return nodes
        .borrow()
        .iter()
        .filter(|(range, holds_text)| return *holds_text && !range.is_empty())
        .map(|(range, _)| return range.clone())
        .collect();
}

/// Where the snippet must go in a document lol_html saw as `observed`, or `None` if it must not be
/// injected at all:
/// * nowhere without a `<head>` tag, nor past body content (see [`Observed::body_content`]), nor
///   when it starts past the window and no `<html>` or `<meta>` start tag starts within it;
/// * otherwise past the last `<meta>` child of the first head declaring the character encoding or
///   `X-UA-Compatible`, and coming before any child the head cannot hold, or else right after the
///   `<head>` tag. Children are looked at within the window, and past it while every one since the
///   `<head>` tag is a `<meta>` or a `<title>`, with no text but whitespace between them, up to
///   [`WALK_LIMIT`].
pub fn injection_point(observed: &Observed) -> Option<usize> {
    let head = observed.heads.first()?;
    if observed
        .body_content
        .is_some_and(|start| return start < head.start)
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

    // A later `<head>` is nested in the first one or follows it, and the tags after it are its own.
    let head_end = observed.first_head_end.unwrap_or(usize::MAX).min(
        observed
            .heads
            .get(1)
            .map_or(usize::MAX, |head| return head.start),
    );
    // Found past the window, the `<head>` tag is not walked from: the snippet goes right after it.
    if head.end > WINDOW {
        return Some(head.end);
    }
    let mut point = head.end;
    let mut leading = true;
    let mut gap_start = head.end;
    for child in &observed.head_children {
        let location = &child.location;
        if location.start < head.end || location.start >= head_end {
            continue;
        }
        let name = child.name.as_str();
        if !ALLOWED_IN_HEAD.contains(&name) {
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
/// spec's prescan, or is `X-UA-Compatible`.
fn is_relevant(meta: &HeadChild) -> bool {
    let known = |label: &str| return encoding_rs::Encoding::for_label(label.as_bytes()).is_some();
    let http_equiv = meta.http_equiv.as_deref().map(str::trim);
    if http_equiv.is_some_and(|value| return value.eq_ignore_ascii_case("x-ua-compatible")) {
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
    document: &[u8],
    observed: &Observed,
    ours: Option<usize>,
    oracle: Option<usize>,
) -> Option<Exception> {
    let lower = document.to_ascii_lowercase();
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
