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

use lol_html::html_content::Element;
use lol_html::{element, end_tag, HtmlRewriter, Settings};

/// The head window of the injector, `DOCUMENT_CHARSET_SCAN_LIMIT`.
pub const WINDOW: usize = 1024;

/// What lol_html saw of a document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Every `<head>` start tag.
    pub heads: Vec<Range<usize>>,
    /// Where the end tag of the first `<head>` starts, if it has one.
    pub first_head_end: Option<usize>,
    /// Every start tag directly within a `<head>`, lower case name and location.
    pub head_children: Vec<(String, Range<usize>)>,
    /// Start of the first `<html>` or `<meta>` start tag.
    pub html_evidence: Option<usize>,
    /// Every `<template>` element, from its start tag to the start of its end tag, if it has one.
    pub templates: Vec<(usize, Option<usize>)>,
}

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
            children.borrow_mut().head_children.push((
                el.tag_name().to_ascii_lowercase(),
                el.source_location().bytes(),
            ));
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

    return Some(observed.borrow().clone());
}

/// Where the snippet must go in a document lol_html saw as `observed`, or `None` if it must not be
/// injected at all:
/// * nowhere without a `<head>` tag, nor when it starts past the window and no `<html>` or `<meta>`
///   start tag starts within it;
/// * otherwise past the last `<meta>` child of the first head ending within the window and coming
///   before any child the head cannot hold, or else right after the `<head>` tag.
pub fn injection_point(observed: &Observed) -> Option<usize> {
    let head = observed.heads.first()?;
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
    let mut point = head.end;
    for (name, location) in &observed.head_children {
        if location.start < head.end || location.start >= head_end {
            continue;
        }
        if !ALLOWED_IN_HEAD.contains(&name.as_str()) {
            break;
        }
        if name == "meta" && location.end <= WINDOW {
            point = location.end;
        }
    }
    return Some(point);
}

/// Why the injector may place the snippet elsewhere than lol_html says. Each one is a deliberate
/// difference, not a bug in either.
#[derive(Debug, PartialEq, Eq)]
pub enum Exception {
    /// lol_html reported the markup as ambiguous, so there is nothing to compare with.
    Ambiguous,
    /// The start tag of a raw text element ends with an attribute whose value is empty and
    /// unquoted, as in `<script c=>`: depending on the selectors registered, lol_html either does
    /// not switch to raw text after it, or never leaves it. The spec, and the injector, switch to
    /// raw text and leave it at the closing tag.
    RawTextEmptyAttributeValue,
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
    /// The injector stops its walk at a `<![CDATA[`, whose end it cannot tell without tracking
    /// foreign content, so its point is earlier.
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
    if has_raw_text_tag_ending_in_empty_value(&lower) {
        return Some(Exception::RawTextEmptyAttributeValue);
    }
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

/// Whether `lower`, a lower case document, holds a raw text element start tag ending in `=>`, see
/// [`Exception::RawTextEmptyAttributeValue`].
fn has_raw_text_tag_ending_in_empty_value(lower: &[u8]) -> bool {
    const RAW_TEXT: &[&[u8]] = &[
        b"script",
        b"style",
        b"title",
        b"textarea",
        b"xmp",
        b"iframe",
        b"noembed",
        b"noframes",
        b"noscript",
        b"plaintext",
    ];
    for (index, _) in lower
        .iter()
        .enumerate()
        .filter(|&(_, &byte)| return byte == b'<')
    {
        let tag = &lower[index + 1..];
        if !RAW_TEXT.iter().any(|name| return tag.starts_with(name)) {
            continue;
        }
        let Some(end) = tag.iter().position(|&byte| return byte == b'>') else {
            return false;
        };
        if tag[..end].trim_ascii_end().ends_with(b"=") {
            return true;
        }
    }
    return false;
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    return haystack
        .windows(needle.len())
        .any(|window| return window == needle);
}
