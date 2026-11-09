// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Tokenizer-level scan of the head of an HTML document, looking for where within it the snippet
//! goes. See [`find_injection_point`].

use memchr::{memchr, memchr2};

use super::encoding::{has_encoding_attributes, meta_kind, MetaKind};
use super::{DOCUMENT_CHARSET_SCAN_LIMIT, WALK_LIMIT};

/// Where [`find_injection_point`] says the snippet goes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Placement {
    /// At this index of the window.
    At(usize),
    /// The window ends before the walk can tell, and more data may still come: the same call on a
    /// longer window will. Never returned for a window of [`WALK_LIMIT`] bytes or more.
    NeedMore,
    /// Nowhere: any point would push a character encoding declaration further into the document.
    Decline,
}

/// Returns where to inject the snippet within `window`, the first bytes of the document, given
/// `head_end`, the index just past the `<head>` start tag. `complete` tells whether `window` is the
/// whole document; when it is not, it must hold at least [`DOCUMENT_CHARSET_SCAN_LIMIT`] bytes.
///
/// The snippet goes right after the `<head>` tag, unless the head holds a `<meta>` tag the snippet
/// has to stay behind: then right after the last of them. Those are the ones declaring the character
/// encoding, and the `http-equiv="X-UA-Compatible"` ones (see [`meta_kind`]). Every other `<meta>`
/// tag, SEO, Open Graph, viewport, a content security policy or the tracer's `dd-trace-id`, is left
/// behind the snippet.
///
/// The tags of the head are walked from the `<head>` tag on, and the walk stops at:
/// * the `</head>` tag, or a `</body>`, `</html>` or `</br>` one, which close the head as well,
/// * a start tag the head cannot hold (anything but `base`, `basefont`, `bgsound`, `link`, `meta`,
///   `noframes`, `noscript`, `script`, `style`, `template` and `title`), where the browser closes the
///   head,
/// * a `<![CDATA[` section, whose end cannot be told without tracking foreign content,
/// * a tag starting past the first [`DOCUMENT_CHARSET_SCAN_LIMIT`] bytes, unless every start tag
///   since the `<head>` one is a `<meta>` or a `<title>`, with nothing but whitespace and comments
///   between them: only within such a run can an `X-UA-Compatible` declaration still be honoured,
/// * the first [`WALK_LIMIT`] bytes, past which nothing is looked at: the snippet goes at the best
///   point found within them.
///
/// When the window ends before one of those, while more data may come, [`Placement::NeedMore`] is
/// returned: within the run of `<meta>` and `<title>` tags, or within a `<meta>` tag starting in the
/// first [`DOCUMENT_CHARSET_SCAN_LIMIT`] bytes, which could turn out to be one to stay behind. The
/// outcome therefore never depends on where the window ends: called on any longer window of the same
/// document, the walk would give the same answer.
///
/// The character encoding declaration is also what keeps the snippet behind a raw text element
/// (`<script>`, `<title>`…) or an inert one (`<noscript>`, `<template>`) holding one within the first
/// [`DOCUMENT_CHARSET_SCAN_LIMIT`] bytes: the encoding prescan reads the raw bytes and honours a
/// declaration written there, while the snippet can go within neither. And should the first
/// declaration within those bytes follow the point where the walk stops, out of the head, or within
/// a tag or an element that does not end within the window, there is no point to inject at that
/// would not push it further into the document: injection is declined.
///
/// The snippet is deliberately injected *before* a `<meta http-equiv="content-security-policy">`:
/// a meta-delivered policy only governs what the parser sees after it. A policy delivered by
/// response header applies regardless of position and is the common case, so the fix for a strict
/// `script-src` belongs in nonce or hash support rather than in where the snippet is placed.
pub(super) fn find_injection_point(window: &[u8], head_end: usize, complete: bool) -> Placement {
    let window = &window[..window.len().min(WALK_LIMIT)];
    // What follows the window may change the outcome only if it can come, and be walked.
    let more = !complete && window.len() < WALK_LIMIT;
    debug_assert!(complete || window.len() >= DOCUMENT_CHARSET_SCAN_LIMIT);
    let charset_window = &window[..window.len().min(DOCUMENT_CHARSET_SCAN_LIMIT)];
    // A declaration the prescan honours, within the given range of the window: only the first
    // DOCUMENT_CHARSET_SCAN_LIMIT bytes count.
    let declares_within = |start: usize, end: usize| {
        let end = end.min(charset_window.len());
        return contains_encoding_declaration(&charset_window[start.min(end)..end]);
    };

    let mut point = head_end;
    // The prescan stops at the first declaration it resolves, so once one lies behind the point the
    // ones that follow no longer matter.
    let mut declared = contains_encoding_declaration(&window[..head_end]);
    // Whether every start tag since the `<head>` one is a `<meta>` or a `<title>`, with no text but
    // whitespace between them.
    let mut leading = true;
    let mut offset = head_end;

    let stop = loop {
        let markup = next_markup(window, offset);
        if leading {
            // Text closes the head, so the run ends at any but whitespace.
            let text_end = match markup {
                Markup::None => window.len(),
                Markup::Unfinished(start) | Markup::CdataSection(start) => start,
                Markup::Tag { start, .. } => start,
            };
            leading = !holds_text(&window[offset..text_end]);
        }
        let (closing, name, tag_start, tag_end) = match markup {
            Markup::None if more && leading => return Placement::NeedMore,
            Markup::None => break window.len(),
            Markup::Unfinished(start) => {
                let walked = leading || start < DOCUMENT_CHARSET_SCAN_LIMIT;
                if more && walked && (leading || could_be_meta(&window[start + 1..])) {
                    return Placement::NeedMore;
                }
                break start;
            }
            Markup::CdataSection(start) => break start,
            Markup::Tag {
                closing,
                name,
                start,
                end,
            } => (closing, name, start, end),
        };

        if tag_start >= DOCUMENT_CHARSET_SCAN_LIMIT && !leading {
            break tag_start;
        }

        if closing {
            // The `in head` insertion mode closes the head on these end tags, and ignores the others.
            if [b"head".as_slice(), b"body", b"html", b"br"]
                .iter()
                .any(|closes_head| return compare_case_insensitive(name, closes_head))
            {
                break tag_start;
            }
            offset = tag_end;
            continue;
        }

        let tag_name = parse_tag_name(name);
        if !matches!(tag_name, Some(TagName::Meta | TagName::RawText(b"title"))) {
            leading = false;
            if tag_start >= DOCUMENT_CHARSET_SCAN_LIMIT {
                break tag_start;
            }
        }

        match tag_name {
            Some(TagName::Meta) => {
                let kind = meta_kind(&window[tag_start..tag_end]);
                declared |= kind == MetaKind::Encoding;
                if kind != MetaKind::Other {
                    point = tag_end;
                }
                offset = tag_end;
            }
            Some(TagName::RawText(element) | TagName::Inert(element))
                if is_allowed_in_head(element) =>
            {
                // The snippet can go within neither: in a raw text element it would be text rather
                // than markup, and in an inert one it would never run.
                let Some(element_end) = find_element_end(window, tag_end, element) else {
                    // Where it ends matters if it is a `<title>` within the run, or if the content
                    // seen so far holds a declaration: the point would move past the element.
                    if more && (leading || declares_within(tag_end, window.len())) {
                        return Placement::NeedMore;
                    }
                    break tag_start;
                };
                if declares_within(tag_end, element_end) {
                    declared = true;
                    point = element_end;
                }
                offset = element_end;
            }
            _ if is_allowed_in_head(name) => offset = tag_end,
            _ => break tag_start,
        }
    };

    if !declared && declares_within(stop, charset_window.len()) {
        return Placement::Decline;
    }
    return Placement::At(point);
}

/// Returns whether `input`, what [`next_markup`] skipped on its way to the next tag, holds text other
/// than whitespace, rather than only comments, doctypes and bogus comments.
fn holds_text(input: &[u8]) -> bool {
    let mut i = 0;
    loop {
        while i < input.len() && input[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == input.len() {
            return false;
        }
        // Whatever `next_markup` skipped ends within `input`.
        let end = match input[i] {
            b'<' if input[i..].starts_with(b"<!--") => find_comment_end(input, i),
            b'<' if ends_at_first_greater_than(&input[i + 1..]) => find_bogus_comment_end(input, i),
            // A stray `<` is text as well.
            _ => return true,
        };
        match end {
            Some(end) => i = end,
            None => return false,
        }
    }
}

/// Returns whether `rest`, the bytes just after the `<` of markup the window ends within, may be the
/// start of a `<meta>` tag.
fn could_be_meta(rest: &[u8]) -> bool {
    if rest.len() <= b"meta".len() {
        return compare_case_insensitive(rest, &b"meta"[..rest.len()]);
    }
    return is_tag_named(rest, false, b"meta");
}

/// Returns whether the start tag named `name` is one the head can hold. Any other closes the head,
/// as the tree builder does in the `in head` insertion mode.
fn is_allowed_in_head(name: &[u8]) -> bool {
    return [
        b"base".as_slice(),
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
    ]
    .iter()
    .any(|allowed| return compare_case_insensitive(name, allowed));
}

/// The next piece of markup [`next_markup`] finds. Comments, doctypes and bogus comments are
/// skipped rather than reported.
enum Markup<'a> {
    /// There is no markup left.
    None,
    /// Markup that starts at the index, but does not end within the input.
    Unfinished(usize),
    /// A `<![CDATA[` section starting at the index.
    CdataSection(usize),
    /// A start or end tag, from its `<` at `start` to just past its `>` at `end`.
    Tag {
        closing: bool,
        name: &'a [u8],
        start: usize,
        end: usize,
    },
}

/// Returns the next tag starting at or after `offset` in `input`, skipping text, comments, doctypes
/// and bogus comments.
fn next_markup(input: &[u8], offset: usize) -> Markup<'_> {
    let mut i = offset;

    while i < input.len() {
        match memchr(b'<', &input[i..]) {
            Some(text) => i += text,
            None => return Markup::None,
        }

        // The content of a comment is not markup either, and the encoding prescan skips it too.
        if input[i..].starts_with(b"<!--") {
            match find_comment_end(input, i) {
                Some(end) => i = end,
                None => return Markup::Unfinished(i),
            }
            continue;
        }

        // A `<![CDATA[` section runs to `]]>` within `svg` or `math` content and is a bogus comment
        // ending at the first `>` anywhere else. Telling the two apart means tracking foreign
        // content, and guessing wrong would either inject into CDATA text, where the snippet would
        // never run, or skip real markup. Hand it to the caller, which stops there.
        if input[i..].starts_with(b"<![CDATA[") {
            return Markup::CdataSection(i);
        }

        let tag_start = i;
        let rest = &input[tag_start + 1..];

        // A `<` ending the input may start a tag as much as text: what follows tells.
        if rest.is_empty() {
            return Markup::Unfinished(tag_start);
        }

        // A doctype or a bogus comment holds no attribute value a `>` could hide in, so it ends at
        // the very next `>` rather than at the one `find_tag_close` looks for.
        if ends_at_first_greater_than(rest) {
            match find_bogus_comment_end(input, tag_start) {
                Some(end) => i = end,
                None => return Markup::Unfinished(tag_start),
            }
            continue;
        }

        if !starts_markup(rest) {
            // A stray `<`, as in `a < b`, is text rather than the start of a tag.
            i += 1;
            continue;
        }

        let Some(tag_close) = find_tag_close(rest) else {
            return Markup::Unfinished(tag_start);
        };
        let closing = rest[0] == b'/';
        let name = &rest[usize::from(closing)..];
        // The tag is closed, so its name ends at the latest at the `>`.
        let name_end = find_tag_name_end(name).unwrap_or(name.len());

        return Markup::Tag {
            closing,
            name: &name[..name_end],
            start: tag_start,
            end: tag_start + tag_close + 2,
        };
    }

    return Markup::None;
}

#[derive(Debug, PartialEq, Eq)]
enum TagName {
    Meta,
    /// An element whose content is text rather than markup (`script`, `style`, `title`,
    /// `textarea`, `iframe`, `xmp`), holding its lower case name. The snippet cannot go within such
    /// content, while the encoding prescan still honours a declaration written there.
    RawText(&'static [u8]),
    /// An element whose content is inert (`noscript` with scripting enabled, `noframes`, `noembed`,
    /// `template`), holding its lower case name. A script injected within one would never run,
    /// while the encoding prescan still honours a declaration written there.
    Inert(&'static [u8]),
    /// `plaintext`, which switches the tokenizer to a state it never leaves: everything that follows
    /// the tag is text, and no closing tag can end it.
    Plaintext,
}

/// The state script content can be in. A `<!--` written in a script opens the escaped state, a
/// `<script` tag written while escaped the double escaped one, and a `-->` returns to plain script
/// data from either.
///
/// Only `script` has these states: a `<!--` written in a `title` or a `style` is plain text.
#[derive(PartialEq, Eq)]
enum ScriptState {
    Data,
    Escaped,
    DoubleEscaped,
}

/// Returns the index just past the closing tag of the raw text element named `name` whose content
/// starts at `content_start`, or `None` if the element is never closed within `input`.
///
/// A `</script>` read while the script is double escaped, as in `<script><!--<script></script>`, only
/// leaves that state: the element stays open, and the script source goes on past it.
fn find_raw_text_end(input: &[u8], content_start: usize, name: &[u8]) -> Option<usize> {
    let script = name == b"script";
    let mut state = ScriptState::Data;
    let mut i = content_start;

    while i < input.len() {
        if script {
            if state == ScriptState::Data && input[i..].starts_with(b"<!--") {
                // Resume from the dashes: they are also the ones that end `<!-->` and `<!--->`.
                state = ScriptState::Escaped;
                i += b"<!".len();
                continue;
            }
            if state != ScriptState::Data && input[i..].starts_with(b"-->") {
                state = ScriptState::Data;
                i += b"-->".len();
                continue;
            }
        }

        if input[i] != b'<' {
            // Only a `<` matters past here, and in a script a `-` as well, which the checks above
            // look at.
            let next = match script {
                true => memchr2(b'<', b'-', &input[i + 1..]),
                false => memchr(b'<', &input[i + 1..]),
            };
            i = next.map_or(input.len(), |next| return i + 1 + next);
            continue;
        }

        let rest = &input[i + 1..];

        if state == ScriptState::Escaped && is_tag_named(rest, false, b"script") {
            state = ScriptState::DoubleEscaped;
            i += 1;
            continue;
        }

        if is_tag_named(rest, true, name) {
            if state == ScriptState::DoubleEscaped {
                state = ScriptState::Escaped;
                i += 1;
                continue;
            }
            return Some(i + find_tag_close(rest)? + 2);
        }

        i += 1;
    }

    return None;
}

/// Returns the index just past the closing tag of the raw text or inert element named `name` whose
/// content starts at `content_start`, or `None` if the element is never closed within `input`.
fn find_element_end(input: &[u8], content_start: usize, name: &[u8]) -> Option<usize> {
    // Every element handled here but `template` holds raw text — `noscript` does when scripting is
    // enabled — so they cannot nest and the first matching closing tag ends them, wherever it
    // appears.
    if name != b"template" {
        return find_raw_text_end(input, content_start, name);
    }

    // `template` content is markup: its closing tags have to be paired, and a literal `</template>`
    // written within an attribute value, a comment or a raw text element closes nothing.
    let mut depth = 1usize;
    let mut i = content_start;

    while i < input.len() {
        i += memchr(b'<', &input[i..])?;

        // A `</template>` written within a comment closes nothing, so the comment has to be skipped
        // through its own `-->` terminator rather than through the first `>` it holds.
        if input[i..].starts_with(b"<!--") {
            i = find_comment_end(input, i)?;
            continue;
        }

        // A CDATA section cannot be told from a bogus comment without tracking foreign content, and
        // the two end in different places, so where the template ends cannot be told either.
        if input[i..].starts_with(b"<![CDATA[") {
            return None;
        }

        let rest = &input[i + 1..];

        // A `</template>` written in a doctype or a bogus comment closes nothing either, and those
        // end at the very next `>`, quoted or not.
        if ends_at_first_greater_than(rest) {
            i = find_bogus_comment_end(input, i)?;
            continue;
        }

        if !starts_markup(rest) {
            // A stray `<`, as in `a < b`, is text rather than the start of a tag.
            i += 1;
            continue;
        }

        // Skipping the whole tag, rather than a single byte, is what keeps a `</template>` written
        // within one of its attribute values from being mistaken for the closing tag.
        let tag_end = i + find_tag_close(rest)? + 2;

        if is_tag_named(rest, true, name) {
            depth -= 1;
            if depth == 0 {
                return Some(tag_end);
            }
            i = tag_end;
        } else if is_tag_named(rest, false, name) {
            depth += 1;
            i = tag_end;
        } else {
            // The content of a raw text element is not markup either. `noscript`, `noframes` and
            // `noembed` hold raw text as well, so a `</template>` written in one of them closes
            // nothing and must not end the scan.
            let name_end = find_tag_name_end(rest)?;
            i = match parse_tag_name(&rest[..name_end]) {
                Some(TagName::RawText(raw_text_name)) => {
                    find_raw_text_end(input, tag_end, raw_text_name)?
                }
                Some(TagName::Inert(inert_name)) if inert_name != b"template" => {
                    find_raw_text_end(input, tag_end, inert_name)?
                }
                // Everything past a `plaintext` tag is text, so the closing tag never comes.
                Some(TagName::Plaintext) => return None,
                _ => tag_end,
            };
        }
    }

    return None;
}

/// Returns the index just past the `-->` or `--!>` closing the comment whose `<!--` starts at
/// `start`, or `None` if the comment is never closed within `input`.
///
/// The `-->` may reuse the dashes of the opening `<!--`, which is what ends the empty comments
/// `<!-->` and `<!--->`; the encoding prescan reads it the same way, as the `>` it looks for only has
/// to be preceded by two dashes.
///
/// The `--!>` form cannot reuse them: `<!--!>` and `<!---!>` are unterminated comments, so its dashes
/// have to fall within the comment content. The prescan does not know that form at all, but ending
/// the comment at the earlier of the two terminators only ever keeps the snippet behind more of the
/// document, never less.
fn find_comment_end(input: &[u8], start: usize) -> Option<usize> {
    let content_start = start + b"<!--".len();
    let mut i = start + b"<!".len();

    // Both terminators start with a dash.
    while let Some(dash) = memchr(b'-', &input[i..]) {
        i += dash;
        if input[i..].starts_with(b"-->") {
            return Some(i + b"-->".len());
        }
        if i >= content_start && input[i..].starts_with(b"--!>") {
            return Some(i + b"--!>".len());
        }
        i += 1;
    }

    return None;
}

/// Returns the index at which the tag name starting `rest`, the bytes just after a `<` character,
/// ends, or `None` if it does not end within `rest`.
///
/// `/` delimits the name as whitespace and `>` do: browsers ignore the self-closing flag on a
/// non-void element, so `<script/>` opens a script element like `<script>` does.
fn find_tag_name_end(rest: &[u8]) -> Option<usize> {
    return rest
        .iter()
        .position(|&b| return b.is_ascii_whitespace() || b == b'>' || b == b'/');
}

/// Returns whether `rest`, the bytes just after a `<` character, starts a tag, a comment or a
/// declaration rather than plain text.
fn starts_markup(rest: &[u8]) -> bool {
    return match rest.first() {
        Some(b'!' | b'?') => true,
        Some(b'/') => rest.get(1).is_some_and(|b| return b.is_ascii_alphabetic()),
        Some(byte) => byte.is_ascii_alphabetic(),
        None => false,
    };
}

/// Returns whether `rest`, the bytes just after a `<` character, starts markup that ends at the very
/// next `>`: a doctype, or a bogus comment such as `<?x>`, `<!x>` or `</3>`.
///
/// Only a tag has attribute values a `>` can hide in, so those are the only ones whose end has to be
/// looked for with [`find_tag_close`]. The tokenizer ends a bogus comment at the first `>` it reads,
/// and so does the encoding prescan, which skips `<!`, `</` and `<?` up to the next `>`.
fn ends_at_first_greater_than(rest: &[u8]) -> bool {
    return match rest.first() {
        Some(b'!' | b'?') => true,
        Some(b'/') => !rest.get(1).is_some_and(|b| return b.is_ascii_alphabetic()),
        _ => false,
    };
}

/// Returns the index just past the `>` ending the markup that starts with the `<` at `start`, for the
/// markup [`ends_at_first_greater_than`] describes, or `None` if there is no `>` within `input`.
fn find_bogus_comment_end(input: &[u8], start: usize) -> Option<usize> {
    return memchr(b'>', &input[start..]).map(|index| return start + index + 1);
}

/// Returns whether `rest`, the bytes just after a `<` character, starts an open (`closing` false) or
/// close (`closing` true) tag for the element named `name`.
///
/// The name has to be followed by whitespace, `/` or `>`: `</scriptfoo>` does not close a `script`.
fn is_tag_named(rest: &[u8], closing: bool, name: &[u8]) -> bool {
    let start = if closing {
        if rest.first() != Some(&b'/') {
            return false;
        }
        1
    } else {
        0
    };

    return rest.len() > start + name.len()
        && compare_case_insensitive(&rest[start..start + name.len()], name)
        && matches!(
            rest[start + name.len()],
            b'>' | b'/' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' '
        );
}

/// Returns whether `content` holds a character encoding declaration.
///
/// Used to decide whether the snippet has to be kept behind an element or declined: the browser's
/// encoding prescan reads the raw bytes, so it honours a declaration wherever it is nested.
///
/// A `<meta>` tag that `content` ends in the middle of counts as a declaration if the part of it
/// that can be seen is one: the snippet is never placed before a declaration that more data might
/// complete.
fn contains_encoding_declaration(content: &[u8]) -> bool {
    // A declaration is a `<meta` tag, whose name has to be there in full: without one, walking the
    // markup cannot find any.
    if !holds_meta_tag(content) {
        return false;
    }
    let mut offset = 0;

    loop {
        match next_markup(content, offset) {
            Markup::None => return false,
            Markup::Unfinished(start) => {
                return is_tag_named(&content[start + 1..], false, b"meta")
                    && has_encoding_attributes(&content[start..]);
            }
            // The prescan knows no CDATA section and reads what it holds as markup.
            Markup::CdataSection(start) => offset = start + b"<![CDATA[".len(),
            Markup::Tag {
                closing: false,
                name,
                start,
                end,
            } if compare_case_insensitive(name, b"meta")
                && has_encoding_attributes(&content[start..end]) =>
            {
                return true;
            }
            Markup::Tag { end, .. } => offset = end,
        }
    }
}

/// Whether `content` holds `<meta`, in any case, anywhere, markup or not: the cheap necessary
/// condition for it to hold a `<meta>` tag. One pass over five shifted views without branches or
/// early exit, which the compiler vectorizes, rather than a search stopping at every `<`.
fn holds_meta_tag(content: &[u8]) -> bool {
    let Some(positions) = content.len().checked_sub(b"<meta".len() - 1) else {
        return false;
    };
    let view = |shift: usize| return &content[shift..shift + positions];
    // ASCII letters only differ from their upper case by 0x20, and `<` is compared as is.
    return view(0)
        .iter()
        .zip(view(1))
        .zip(view(2))
        .zip(view(3))
        .zip(view(4))
        .fold(false, |found, ((((&lt, &m), &e), &t), &a)| {
            return found
                | ((lt == b'<')
                    & (m | 0x20 == b'm')
                    & (e | 0x20 == b'e')
                    & (t | 0x20 == b't')
                    & (a | 0x20 == b'a'));
        });
}

/// Where within a tag [`find_tag_close`] is reading, which is what decides whether a quote opens an
/// attribute value.
#[derive(Clone, Copy)]
enum TagPosition {
    /// The tag name, where an `=` is part of the name rather than the start of a value.
    TagName,
    /// Between two attributes, where an `=` starts an attribute name of its own.
    BeforeAttributeName,
    /// An attribute name, or the whitespace that may follow it before its `=`.
    AttributeName,
    /// Just past the `=` of an attribute, the one place a quote opens a quoted value.
    BeforeAttributeValue,
    /// An unquoted attribute value.
    UnquotedAttributeValue,
    /// A quoted attribute value, holding the quote that ends it.
    QuotedAttributeValue(u8),
}

/// The bytes [`find_tag_close`] may change its position on: whitespace, `/`, `=`, quotes and `>`.
/// Past the first byte of a name or an unquoted value, any other leaves the position as it is.
const TAG_DELIMITERS: [bool; 256] = {
    let mut table = [false; 256];
    let delimiters = *b"\t\n\x0C\r /='\">";
    let mut i = 0;
    while i < delimiters.len() {
        table[delimiters[i] as usize] = true;
        i += 1;
    }
    table
};

fn find_tag_close(input: &[u8]) -> Option<usize> {
    let mut position = TagPosition::TagName;
    // The `/` of an end tag comes before its name, which a quote or an `=` is part of as much as of
    // a start tag's: `</a="b>` ends at its `>`.
    let name_start = usize::from(input.first() == Some(&b'/'));

    let mut index = name_start;
    while index < input.len() {
        let byte = input[index];
        position = match (position, byte) {
            // Checked before `>`, which does not close the tag while within a quoted value: the
            // whole value is skipped, up to its closing quote.
            (TagPosition::QuotedAttributeValue(quote), _) => {
                index += memchr(quote, &input[index..])?;
                TagPosition::BeforeAttributeName
            }

            (_, b'>') => return Some(index),

            (TagPosition::BeforeAttributeValue, b'"' | b'\'') => {
                TagPosition::QuotedAttributeValue(byte)
            }
            (TagPosition::BeforeAttributeValue, byte) if byte.is_ascii_whitespace() => position,
            (TagPosition::BeforeAttributeValue, _) => TagPosition::UnquotedAttributeValue,

            (TagPosition::AttributeName, b'=') => TagPosition::BeforeAttributeValue,

            // The `/` of a self-closing tag ends the name preceding it, leaving the next attribute
            // to start, and so does one between attributes: in `<meta /="x>">` the `=` starts an
            // attribute name and the quote is part of it. Written in an unquoted value, a `/` is
            // part of the value.
            (
                TagPosition::TagName
                | TagPosition::AttributeName
                | TagPosition::BeforeAttributeName,
                b'/',
            ) => TagPosition::BeforeAttributeName,
            // A value ends at the whitespace following it.
            (TagPosition::TagName | TagPosition::UnquotedAttributeValue, byte)
                if byte.is_ascii_whitespace() =>
            {
                TagPosition::BeforeAttributeName
            }

            (TagPosition::BeforeAttributeName, byte) if byte.is_ascii_whitespace() => position,
            (TagPosition::BeforeAttributeName, _) => TagPosition::AttributeName,

            _ => position,
        };
        index += 1;
        // Within a name or an unquoted value, the bytes up to the next delimiter change nothing.
        if matches!(
            position,
            TagPosition::TagName | TagPosition::AttributeName | TagPosition::UnquotedAttributeValue
        ) {
            while index < input.len() && !TAG_DELIMITERS[usize::from(input[index])] {
                index += 1;
            }
        }
    }

    return None;
}

fn parse_tag_name(input: &[u8]) -> Option<TagName> {
    if compare_case_insensitive(input, b"meta") {
        return Some(TagName::Meta);
    } else if compare_case_insensitive(input, b"script") {
        return Some(TagName::RawText(b"script"));
    } else if compare_case_insensitive(input, b"style") {
        return Some(TagName::RawText(b"style"));
    } else if compare_case_insensitive(input, b"title") {
        return Some(TagName::RawText(b"title"));
    } else if compare_case_insensitive(input, b"textarea") {
        return Some(TagName::RawText(b"textarea"));
    } else if compare_case_insensitive(input, b"iframe") {
        return Some(TagName::RawText(b"iframe"));
    } else if compare_case_insensitive(input, b"xmp") {
        return Some(TagName::RawText(b"xmp"));
    } else if compare_case_insensitive(input, b"plaintext") {
        return Some(TagName::Plaintext);
    } else if compare_case_insensitive(input, b"noscript") {
        return Some(TagName::Inert(b"noscript"));
    } else if compare_case_insensitive(input, b"noframes") {
        return Some(TagName::Inert(b"noframes"));
    } else if compare_case_insensitive(input, b"noembed") {
        return Some(TagName::Inert(b"noembed"));
    } else if compare_case_insensitive(input, b"template") {
        return Some(TagName::Inert(b"template"));
    } else {
        return None;
    }
}

pub(super) fn compare_case_insensitive(a: &[u8], b_lower_case: &[u8]) -> bool {
    if a.len() != b_lower_case.len() {
        return false;
    }
    return a
        .iter()
        .zip(b_lower_case)
        .all(|(a, &b_lower_case)| return a.to_ascii_lowercase() == b_lower_case);
}

#[cfg(test)]
mod tests {
    use super::super::head_open_tag::HeadOpenTagLocator;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn meta_with_charset() {
        test_injector(
            "<html><head><meta charset='utf-8'></head></html>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn meta_with_http_equiv_content_type() {
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html; charset=utf-8'></head></html>",
            "<html><head><meta http-equiv='content-type' content='text/html; charset=utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn spaces_around_attribute_equal_sign() {
        test_injector(
            "<html><head><meta charset = 'utf-8'></head></html>",
            "<html><head><meta charset = 'utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn unquoted_attribute_value() {
        test_injector(
            "<html><head><meta charset=utf-8></head></html>",
            "<html><head><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn meta_without_encoding_declaration_does_not_hide_a_later_one() {
        // Attributes that merely contain 'charset', and `http-equiv` values other than
        // 'content-type', are not encoding declarations: the snippet must still land after the
        // actual one.
        test_injector(
            "<html><head><meta data-charset='x'><meta charset='utf-8'></head></html>",
            "<html><head><meta data-charset='x'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta http-equiv='refresh' content='0'><meta charset='utf-8'></head></html>",
            "<html><head><meta http-equiv='refresh' content='0'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn meta_declaring_no_encoding_does_not_hide_a_later_one() {
        // An empty `charset`, and a `http-equiv='content-type'` whose `content` carries no charset
        // parameter, give the browser no encoding: it keeps looking, and so must the snippet.
        test_injector(
            "<html><head><meta charset=''><meta charset='utf-8'></head></html>",
            "<html><head><meta charset=''><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html'><meta charset='utf-8'></head></html>",
            "<html><head><meta http-equiv='content-type' content='text/html'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta http-equiv='content-type'><meta charset='utf-8'></head></html>",
            "<html><head><meta http-equiv='content-type'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn meta_with_unknown_charset_does_not_hide_a_later_one() {
        // A label the browser cannot resolve to an encoding leaves its prescan looking for the real
        // declaration, so injecting after it would push that declaration out of the window.
        test_injector(
            "<html><head><meta charset='bogus'><meta charset='utf-8'></head></html>",
            "<html><head><meta charset='bogus'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html; charset=bogus'><meta charset='utf-8'></head></html>",
            "<html><head><meta http-equiv='content-type' content='text/html; charset=bogus'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn unusual_but_valid_encoding_labels_are_declarations() {
        // The label set is the WHATWG one, not just 'utf-8': matching is case insensitive and
        // surrounding whitespace is trimmed.
        for label in [
            "iso-8859-1",
            "latin1",
            "csisolatin1",
            "x-user-defined",
            "UTF-8",
            "  uTf-8\n",
            "windows-1252",
            "Shift_JIS",
        ] {
            test_injector(
                &alloc::format!("<html><head><meta charset='{label}'></head></html>"),
                &alloc::format!(
                    "<html><head><meta charset='{label}'>{{{{SNIPPET}}}}</head></html>"
                ),
            );
        }
    }

    #[test]
    fn content_type_charset_parameter() {
        // The `content` attribute may be written before `http-equiv`, and the charset parameter may
        // be quoted.
        test_injector(
            "<html><head><meta content='text/html; charset=utf-8' http-equiv='Content-Type'></head></html>",
            "<html><head><meta content='text/html; charset=utf-8' http-equiv='Content-Type'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta http-equiv='content-type' content=\"text/html; charset='utf-8'\"></head></html>",
            "<html><head><meta http-equiv='content-type' content=\"text/html; charset='utf-8'\">{{SNIPPET}}</head></html>",
        );
        // A `content` mentioning 'charset' without a value declares nothing: the snippet need not
        // stay behind it.
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html; charset='></head></html>",
            "<html><head>{{SNIPPET}}<meta http-equiv='content-type' content='text/html; charset='></head></html>",
        );
    }

    #[test]
    fn attribute_order_decides_which_one_declares_the_encoding() {
        // The prescan reads the attributes in source order, and a `charset` naming no known encoding
        // discards the encoding the `content` value provided rather than falling back to it. The
        // browser goes on looking, so the snippet stays behind the declaration that follows.
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html; charset=utf-8' charset='bogus'><meta charset='utf-8'></head></html>",
            "<html><head><meta http-equiv='content-type' content='text/html; charset=utf-8' charset='bogus'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // Written first, a bogus `charset` keeps the `content` value from providing one at all.
        test_injector(
            "<html><head><meta charset='bogus' http-equiv='content-type' content='text/html; charset=utf-8'><meta charset='utf-8'></head></html>",
            "<html><head><meta charset='bogus' http-equiv='content-type' content='text/html; charset=utf-8'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // A duplicate attribute is dropped, so the first `charset` is the one that counts, whether it
        // is the valid one or the bogus one.
        test_injector(
            "<html><head><meta charset='utf-8' charset='bogus'></head></html>",
            "<html><head><meta charset='utf-8' charset='bogus'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><meta charset='bogus' charset='utf-8'><meta charset='utf-8'></head></html>",
            "<html><head><meta charset='bogus' charset='utf-8'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // A `content` written before the `http-equiv` naming the pragma still declares the encoding.
        test_injector(
            "<html><head><meta content='text/html; charset=utf-8' http-equiv='content-type'></head></html>",
            "<html><head><meta content='text/html; charset=utf-8' http-equiv='content-type'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn inert_elements_are_not_injection_points() {
        // A script injected inside a `noscript` or a `template` would never run, but the encoding
        // prescan does honour a declaration written there, so the snippet goes after the element.
        test_injector(
            "<html><head><noscript><meta charset='utf-8'></noscript></head></html>",
            "<html><head><noscript><meta charset='utf-8'></noscript>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><template><meta charset='utf-8'></template></head></html>",
            "<html><head><template><meta charset='utf-8'></template>{{SNIPPET}}</head></html>",
        );
        // Without an encoding declaration inside, the earlier injection point still stands.
        test_injector(
            "<html><head><noscript><link rel='stylesheet' href='x.css'></noscript></head></html>",
            "<html><head>{{SNIPPET}}<noscript><link rel='stylesheet' href='x.css'></noscript></head></html>",
        );
        // Templates nest, so the outer closing tag is the one that ends the element.
        test_injector(
            "<html><head><template><template><meta charset='utf-8'></template></template></head></html>",
            "<html><head><template><template><meta charset='utf-8'></template></template>{{SNIPPET}}</head></html>",
        );
        // A declaration that follows the inert element is still the one to stay behind.
        test_injector(
            "<html><head><noscript><p>x</p></noscript><meta charset='utf-8'></head></html>",
            "<html><head><noscript><p>x</p></noscript><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn noframes_is_an_inert_element() {
        // `noframes` holds raw text, so the snippet cannot go inside it, but the encoding prescan
        // still honours a declaration written there.
        test_injector(
            "<html><head><noframes><meta charset='utf-8'></noframes></head></html>",
            "<html><head><noframes><meta charset='utf-8'></noframes>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><noframes>no frames here</noframes></head></html>",
            "<html><head>{{SNIPPET}}<noframes>no frames here</noframes></head></html>",
        );
    }

    #[test]
    fn noembed_is_an_inert_element() {
        // `noembed` holds raw text, and its content is only shown by a browser that cannot embed, so
        // the snippet cannot go inside it; the encoding prescan still honours a declaration there.
        // The head cannot hold one, so the declaration within is out of the head.
        test_injector(
            "<html><head><noembed><meta charset='utf-8'></noembed></head></html>",
            "<html><head><noembed><meta charset='utf-8'></noembed></head></html>",
        );
        test_injector(
            "<html><head></head><body><noembed><meta charset=utf-8></noembed>",
            "<html><head></head><body><noembed><meta charset=utf-8></noembed>",
        );
        test_injector(
            "<html><head><noembed>no embed here</noembed></head></html>",
            "<html><head>{{SNIPPET}}<noembed>no embed here</noembed></head></html>",
        );
    }

    #[test]
    fn literal_template_closing_tag_does_not_end_the_element() {
        // A `</template>` written within an attribute value or a raw text element is not markup, so
        // the outer one is the one that ends the element. Injecting at the literal occurrence would
        // land the snippet inside the attribute value or the script.
        test_injector(
            "<html><head><template><meta charset='utf-8'><div data-x=\"</template>\"></div></template></head></html>",
            "<html><head><template><meta charset='utf-8'><div data-x=\"</template>\"></div></template>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><template><meta charset='utf-8'><script>var x = \"</template>\";</script></template></head></html>",
            "<html><head><template><meta charset='utf-8'><script>var x = \"</template>\";</script></template>{{SNIPPET}}</head></html>",
        );
        // `noscript`, `noframes` and `noembed` hold raw text within a template as well, so a
        // `</template>` written in one of them is text too.
        test_injector(
            "<html><head><template><meta charset='utf-8'><noscript></template></noscript></template></head></html>",
            "<html><head><template><meta charset='utf-8'><noscript></template></noscript></template>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><template><meta charset='utf-8'><noembed></template></noembed></template></head></html>",
            "<html><head><template><meta charset='utf-8'><noembed></template></noembed></template>{{SNIPPET}}</head></html>",
        );
        // A nested template does pair up: its closing tag is markup, not text.
        test_injector(
            "<html><head><template><meta charset='utf-8'><template></template></template></head></html>",
            "<html><head><template><meta charset='utf-8'><template></template></template>{{SNIPPET}}</head></html>",
        );
        // A stray `<` within the template is text, and must not swallow the real closing tag.
        test_injector(
            "<html><head><template><meta charset='utf-8'>a < b</template></head></html>",
            "<html><head><template><meta charset='utf-8'>a < b</template>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn unterminated_inert_element() {
        // The closing tag being absent does not tell an unclosed element apart from one closing past
        // the end of the buffer: with an encoding declaration in the part we can see, there is no
        // point we can prove valid, so we decline rather than inject before that declaration.
        test_injector(
            "<html><head><template><meta charset='utf-8'>",
            "<html><head><template><meta charset='utf-8'>",
        );
        // Without a declaration inside, everything following is inert either way, so the point found
        // before the element still stands.
        test_injector(
            "<html><head><template><p>x</p>",
            "<html><head>{{SNIPPET}}<template><p>x</p>",
        );
        test_injector("<html><noscript><head>", "<html><noscript><head>");
    }

    #[test]
    fn several_tags_before_the_encoding_declaration() {
        test_injector(
            "<html><head><meta name='x'><meta name='y'><meta charset='utf-8'></head></html>",
            "<html><head><meta name='x'><meta name='y'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn greater_than_in_quoted_attribute_value() {
        // The '>' inside the attribute value does not close the tag, so the snippet must not be
        // inserted within the value.
        test_injector(
            "<html><head data-x='>'><meta charset='utf-8' data-y='>'></head></html>",
            "<html><head data-x='>'><meta charset='utf-8' data-y='>'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head data-x='>'><meta http-equiv=X-UA-Compatible content='>'></head></html>",
            "<html><head data-x='>'><meta http-equiv=X-UA-Compatible content='>'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn a_quote_outside_an_attribute_value_does_not_quote_anything() {
        // The tokenizer opens a quoted value only right after an attribute's `=`. Elsewhere a quote
        // is a parse error it keeps as an ordinary character, so these tags end at their first `>`
        // and the `<plaintext>` that follows is a real tag: reading to the matching quote instead
        // would hand out a point past it, where the snippet is displayed rather than run.
        test_injector(
            "<html><head '><plaintext>'></head></html>",
            "<html><head '>{{SNIPPET}}<plaintext>'></head></html>",
        );
        // Same for a quote written within an unquoted value, and for one in a tag name.
        test_injector(
            "<html><head data-x=a'><plaintext>'></head></html>",
            "<html><head data-x=a'>{{SNIPPET}}<plaintext>'></head></html>",
        );
        // A quote in an attribute name is an ordinary character too, so the tag ends at the first
        // `>` and the meta tag written after it is markup the snippet goes after.
        test_injector(
            "<html><head><link data'><meta charset=utf-8>'></head></html>",
            "<html><head><link data'><meta charset=utf-8>{{SNIPPET}}'></head></html>",
        );
        // A quote in a tag name as well: the tag is one the head cannot hold, and the declaration
        // that follows it is out of the head, so injection is declined.
        test_injector(
            "<html><head><p'><meta charset=utf-8>'></head></html>",
            "<html><head><p'><meta charset=utf-8>'></head></html>",
        );
        // A value quoted the usual way still hides the `>` it holds, whitespace around the `=`
        // included.
        test_injector(
            "<html><head data-x = '>'><meta charset='utf-8'></head></html>",
            "<html><head data-x = '>'><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn a_slash_between_attributes_starts_a_new_one() {
        test_injector(
            "<html><head><meta /=\"x><meta charset=utf-8>\"></head>",
            "<html><head><meta /=\"x><meta charset=utf-8>{{SNIPPET}}\"></head>",
        );
    }

    #[test]
    fn a_quote_in_an_end_tag_name_does_not_quote_anything() {
        test_injector(
            "<html><head><template></a=\"b><meta charset=utf-8></template><meta charset=utf-8>\"</head>",
            "<html><head><template></a=\"b><meta charset=utf-8></template><meta charset=utf-8>{{SNIPPET}}\"</head>",
        );
    }

    #[test]
    fn head_with_no_meta_tag() {
        test_injector(
            "<html><head></head></html>",
            "<html><head>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn head_with_non_encoding_meta() {
        test_injector(
            "<html><head><meta foo='something'></head></html>",
            "<html><head>{{SNIPPET}}<meta foo='something'></head></html>",
        )
    }

    #[test]
    fn before_metas_it_need_not_stay_behind() {
        // SEO, Open Graph, viewport, a content security policy, the tracer's metas, and `http-equiv`
        // values other than the two that matter: none of them hold the snippet back.
        for meta in [
            "<meta name=description content=x>",
            "<meta property='og:title' content=x>",
            "<meta name=viewport content='width=device-width'>",
            "<meta http-equiv=Content-Security-Policy content=\"script-src 'self'\">",
            "<meta name=dd-trace-id content=123><meta name=dd-trace-time content=456>",
            "<meta http-equiv=refresh content=0>",
            "<meta data-charset=utf-8>",
            "<meta name=x-ua-compatible content=IE=edge>",
        ] {
            test_injector(
                &alloc::format!("<html><head>{meta}<title>t</title></head>"),
                &alloc::format!("<html><head>{{{{SNIPPET}}}}{meta}<title>t</title></head>"),
            );
            test_injector(
                &alloc::format!("<html><head><meta charset=utf-8>{meta}</head>"),
                &alloc::format!("<html><head><meta charset=utf-8>{{{{SNIPPET}}}}{meta}</head>"),
            );
        }
    }

    #[test]
    fn after_an_x_ua_compatible_meta() {
        test_injector(
            "<html><head><meta http-equiv='X-UA-Compatible' content='IE=edge'><meta name=a></head>",
            "<html><head><meta http-equiv='X-UA-Compatible' content='IE=edge'>{{SNIPPET}}<meta name=a></head>",
        );
        // Matched case insensitively, surrounding whitespace trimmed, the first `http-equiv` being
        // the one that counts.
        test_injector(
            "<html><head><META HTTP-EQUIV=' x-ua-COMPATIBLE\n' CONTENT=IE=edge></head>",
            "<html><head><META HTTP-EQUIV=' x-ua-COMPATIBLE\n' CONTENT=IE=edge>{{SNIPPET}}</head>",
        );
        test_injector(
            "<html><head><meta http-equiv=refresh http-equiv=x-ua-compatible></head>",
            "<html><head>{{SNIPPET}}<meta http-equiv=refresh http-equiv=x-ua-compatible></head>",
        );
        test_injector(
            "<html><head><meta http-equiv=x-ua-compatible http-equiv=refresh></head>",
            "<html><head><meta http-equiv=x-ua-compatible http-equiv=refresh>{{SNIPPET}}</head>",
        );
        // Written in a comment, a raw text element or an attribute value, it is no tag.
        test_injector(
            "<html><head><!-- <meta http-equiv=x-ua-compatible> --><script>'<meta http-equiv=x-ua-compatible>'</script><link href='<meta http-equiv=x-ua-compatible>'></head>",
            "<html><head>{{SNIPPET}}<!-- <meta http-equiv=x-ua-compatible> --><script>'<meta http-equiv=x-ua-compatible>'</script><link href='<meta http-equiv=x-ua-compatible>'></head>",
        );
    }

    #[test]
    fn after_the_last_relevant_meta_tag() {
        test_injector(
            "<html><head><meta charset='utf-8'><meta http-equiv='...'></head></html>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}<meta http-equiv='...'></head></html>",
        );
        test_injector(
            "<html><head><title>t</title><meta charset=utf-8><link rel=icon><script>1</script><meta http-equiv=x-ua-compatible content=IE=edge><meta name=b><style></style></head></html>",
            "<html><head><title>t</title><meta charset=utf-8><link rel=icon><script>1</script><meta http-equiv=x-ua-compatible content=IE=edge>{{SNIPPET}}<meta name=b><style></style></head></html>",
        );
        // The head only holds what the `in head` insertion mode keeps in it.
        test_injector(
            "<html><head><base href=/><basefont><bgsound><noframes></noframes><template></template><meta charset=utf-8></head></html>",
            "<html><head><base href=/><basefont><bgsound><noframes></noframes><template></template><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
        // End tags close nothing the walk cares about, but the head's own and the few the `in head`
        // insertion mode closes the head on.
        test_injector(
            "<html><head></p></meta><meta charset=utf-8></HEAD ><meta charset=utf-8>",
            "<html><head></p></meta><meta charset=utf-8>{{SNIPPET}}</HEAD ><meta charset=utf-8>",
        );
        for end_tag in ["</body>", "</html>", "</BR>"] {
            test_injector(
                &alloc::format!("<html><head><meta charset=utf-8>{end_tag}<meta charset=utf-8>"),
                &alloc::format!(
                    "<html><head><meta charset=utf-8>{{{{SNIPPET}}}}{end_tag}<meta charset=utf-8>"
                ),
            );
        }
    }

    #[test]
    fn stops_at_a_tag_the_head_cannot_hold() {
        for tag in [
            "<div>",
            "<body>",
            "<p>",
            "<svg>",
            "<head>",
            "<html>",
            "<textarea>",
            "<noembed>",
            "<iframe>",
            "<xmp>",
        ] {
            test_injector(
                &alloc::format!("<head><meta charset=utf-8>{tag}<meta charset=utf-8>"),
                &alloc::format!(
                    "<head><meta charset=utf-8>{{{{SNIPPET}}}}{tag}<meta charset=utf-8>"
                ),
            );
        }
    }

    #[test]
    fn declines_before_an_encoding_declaration_out_of_the_head() {
        test_injector(
            "<html><head></head><script></script><meta charset='utf-8'></html>",
            "<html><head></head><script></script><meta charset='utf-8'></html>",
        );
        test_injector(
            "<html><head><div><meta charset='utf-8'></html>",
            "<html><head><div><meta charset='utf-8'></html>",
        );
        // Not when the declaration was already read, whether it lies behind the point or not.
        test_injector(
            "<html><meta charset='utf-8'><head></head><meta charset='utf-8'></html>",
            "<html><meta charset='utf-8'><head>{{SNIPPET}}</head><meta charset='utf-8'></html>",
        );
        // Nor when what follows declares nothing.
        test_injector(
            "<html><head></head><meta charset='bogus'><meta name=a></html>",
            "<html><head>{{SNIPPET}}</head><meta charset='bogus'><meta name=a></html>",
        );
    }

    #[test]
    fn unfinished_tags() {
        // A meta tag the document ends within is not one the snippet goes after.
        test_injector(
            "<html><head><meta charset=utf-8><meta http-equiv=x-ua-compatible",
            "<html><head><meta charset=utf-8>{{SNIPPET}}<meta http-equiv=x-ua-compatible",
        );
        // But injection is declined if what can be seen of it is a declaration: more data could
        // complete it.
        test_injector(
            "<html><head><meta name=a><meta charset=utf-8",
            "<html><head><meta name=a><meta charset=utf-8",
        );
    }

    #[test]
    fn tags_within_raw_text_elements_are_not_markup() {
        // A tag written in a script or a stylesheet source is text: it opens no element, so the
        // `<head>` written after the script below is the one the snippet goes after.
        test_injector(
            "<html><script>const x = '<head>';</script><head></head></html>",
            "<html><script>const x = '<head>';</script><head>{{SNIPPET}}</head></html>",
        );
        // Without a declaration inside, the earlier injection point still stands, and the snippet
        // never lands within the text itself.
        test_injector(
            "<html><head><script>const x = 1;</script></head></html>",
            "<html><head>{{SNIPPET}}<script>const x = 1;</script></head></html>",
        );
        test_injector(
            "<html><head><title>A <b>title</b></title></head></html>",
            "<html><head>{{SNIPPET}}<title>A <b>title</b></title></head></html>",
        );
    }

    #[test]
    fn declarations_within_raw_text_elements_are_honoured_by_the_prescan() {
        // A meta tag written in raw text declares no encoding for the tokenizer, but the prescan
        // reads the raw bytes and knows no raw text state, so it honours the declaration all the
        // same. The snippet cannot go within the text, so it goes after the element as a whole:
        // injecting before it could push the declaration out of the prescan window.
        test_injector(
            "<html><head><script>const x = '<meta charset=utf-8>';</script></head></html>",
            "<html><head><script>const x = '<meta charset=utf-8>';</script>{{SNIPPET}}</head></html>",
        );
        // The snippet goes after the last declaration all the same.
        test_injector(
            "<html><head><style>/* <meta charset=utf-8> */</style><meta charset='utf-8'></head></html>",
            "<html><head><style>/* <meta charset=utf-8> */</style><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The closing tag is matched case insensitively and may carry trailing spaces.
        test_injector(
            "<html><head><SCRIPT><meta charset=utf-8></SCRIPT ><meta charset='utf-8'></head></html>",
            "<html><head><SCRIPT><meta charset=utf-8></SCRIPT ><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The content of a title is text as well: a meta tag written there is displayed, not parsed,
        // and the prescan honours it nonetheless.
        test_injector(
            "<html><head><title>Using <meta charset=utf-8></title></head></html>",
            "<html><head><title>Using <meta charset=utf-8></title>{{SNIPPET}}</head></html>",
        );
        // An `xmp` holds raw text as well, but the head cannot hold one, so the declaration within is
        // out of the head and injection is declined.
        test_injector(
            "<html><head><xmp><meta charset=utf-8></xmp></head></html>",
            "<html><head><xmp><meta charset=utf-8></xmp></head></html>",
        );
        // A `</scriptfoo>` tag does not close the script element, so the declaration is still part of
        // its source.
        test_injector(
            "<html><head><script></scriptfoo><meta charset=utf-8></script></head></html>",
            "<html><head><script></scriptfoo><meta charset=utf-8></script>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn unterminated_raw_text_element() {
        // Everything following an unclosed script tag is script source, so the only valid injection
        // point left is the one found before it.
        test_injector(
            "<html><head><script>const x = 1;",
            "<html><head>{{SNIPPET}}<script>const x = 1;",
        );
        test_injector("<html><script><head>", "<html><script><head>");
        // Unless the source we can see holds a declaration the prescan would honour: there is no
        // point past the element to inject at, and the `<head>` point would push that declaration
        // further into the document, so early injection is declined.
        test_injector(
            "<html><head><script><meta charset=utf-8>",
            "<html><head><script><meta charset=utf-8>",
        );
    }

    #[test]
    fn tags_within_comments_are_not_markup() {
        // A tag written in a comment is text: the snippet would never run there, and the encoding
        // prescan skips comments too, so a meta tag written in one is not a declaration.
        test_injector("<!--<html><head></head>-->", "<!--<html><head></head>-->");
        test_injector(
            "<html><head><!-- <meta charset=utf-8> --></head></html>",
            "<html><head>{{SNIPPET}}<!-- <meta charset=utf-8> --></head></html>",
        );
        // A '>' within the comment does not end it, so the meta tag that follows it is text as
        // well: the snippet stays behind the real declaration written after the comment.
        test_injector(
            "<html><head><!-- > <meta charset=utf-8> --><meta charset='utf-8'></head></html>",
            "<html><head><!-- > <meta charset=utf-8> --><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><!-- > <meta charset=utf-8> --></head></html>",
            "<html><head>{{SNIPPET}}<!-- > <meta charset=utf-8> --></head></html>",
        );
        // An unclosed comment swallows everything that follows, so only the point found before it
        // stands.
        test_injector(
            "<html><head><!-- <meta charset=utf-8>",
            "<html><head>{{SNIPPET}}<!-- <meta charset=utf-8>",
        );
        test_injector("<!-- <head> -->", "<!-- <head> -->");
    }

    #[test]
    fn comment_closed_by_a_bang_terminator() {
        // The tokenizer ends a comment on `--!>` too, so what follows one is markup: a meta tag the
        // snippet goes after.
        test_injector(
            "<html><head><!-- --!><meta charset=utf-8></head></html>",
            "<html><head><!-- --!><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
        // `--!` not followed by `>` does not end the comment.
        test_injector(
            "<html><head><!-- --!- <meta charset=utf-8> --><meta charset='utf-8'></head></html>",
            "<html><head><!-- --!- <meta charset=utf-8> --><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The dashes of a `--!>` cannot be the ones of the opening `<!--`, so these are unterminated
        // comments and everything that follows them is comment text.
        test_injector(
            "<html><head><!--!><meta charset=utf-8>",
            "<html><head>{{SNIPPET}}<!--!><meta charset=utf-8>",
        );
        test_injector(
            "<html><head><!---!><meta charset=utf-8>",
            "<html><head>{{SNIPPET}}<!---!><meta charset=utf-8>",
        );
    }

    #[test]
    fn bogus_comments_end_at_the_first_greater_than() {
        // A processing instruction is a bogus comment: it holds no attribute value, so the quoted `>`
        // ends it and the meta tag that follows is a real encoding declaration.
        test_injector(
            "<html><head><?x '><meta charset=windows-1252>'></head></html>",
            "<html><head><?x '><meta charset=windows-1252>{{SNIPPET}}'></head></html>",
        );
        // Same for a `<!` that opens neither a comment nor a doctype, and for a `</` that is not
        // followed by a tag name.
        test_injector(
            "<html><head><!x '><meta charset=utf-8>'></head></html>",
            "<html><head><!x '><meta charset=utf-8>{{SNIPPET}}'></head></html>",
        );
        test_injector(
            "<html><head></3 '><meta charset=utf-8>'></head></html>",
            "<html><head></3 '><meta charset=utf-8>{{SNIPPET}}'></head></html>",
        );
        // A doctype ends at its first `>` as well, quoted or not, so the `<head>` after one is found.
        test_injector(
            "<!DOCTYPE html SYSTEM \"x>\"><html><head></head></html>",
            "<!DOCTYPE html SYSTEM \"x>\"><html><head>{{SNIPPET}}</head></html>",
        );
        // A `>` written within a quoted identifier ends the doctype where it stands: the tokenizer
        // emits the token there, so what follows is markup rather than the rest of the identifier.
        test_injector(
            "<!DOCTYPE html SYSTEM \"x><head>\"></head>",
            "<!DOCTYPE html SYSTEM \"x><head>{{SNIPPET}}\"></head>",
        );
    }

    #[test]
    fn cdata_sections_are_not_scanned() {
        // `<![CDATA[` runs to `]]>` in foreign content and ends at the first `>` anywhere else. Which
        // one it is cannot be told without tracking foreign content, so the scan stops and the point
        // found before it stands rather than risking a snippet inside CDATA text.
        test_injector(
            "<html><head><svg><![CDATA[x]]></svg></head></html>",
            "<html><head>{{SNIPPET}}<svg><![CDATA[x]]></svg></head></html>",
        );
        // The prescan ends its `<!` skip at the first `>` and reads what follows as markup, so it
        // honours a declaration written past that `>` whichever way the section ends. There is no
        // point past the section to inject at, and the `<head>` point would push that declaration
        // further into the document, so early injection is declined.
        test_injector(
            "<html><head><svg><![CDATA[x > <meta charset=utf-8>]]></svg></head></html>",
            "<html><head><svg><![CDATA[x > <meta charset=utf-8>]]></svg></head></html>",
        );
        // A declaration written before the section is still the one to stay behind.
        test_injector(
            "<html><head><meta charset='utf-8'><svg><![CDATA[x]]></svg></head></html>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}<svg><![CDATA[x]]></svg></head></html>",
        );
        test_injector(
            "<html><svg><![CDATA[<head>]]>",
            "<html><svg><![CDATA[<head>]]>",
        );
    }

    #[test]
    fn double_escaped_script_content() {
        // A `</script>` read while the script is double escaped only leaves that state: the element
        // stays open, so the meta tag that follows is script source rather than a declaration. The
        // prescan skips it too, as the `<!--` it opens is never closed, so the declaration written
        // after the element is the one the snippet stays behind.
        test_injector(
            "<html><head><script><!--<script></script><meta charset=utf-8></script><meta charset='utf-8'></head></html>",
            "<html><head><script><!--<script></script><meta charset=utf-8></script><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // A `-->` returns to plain script data, where the next `</script>` does close the element.
        test_injector(
            "<html><head><script><!--<script>--></script><meta charset='utf-8'></head></html>",
            "<html><head><script><!--<script>--></script><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // Without a `<!--` opening the escaped state, a `<script>` written in the source changes
        // nothing: the first closing tag is the one that ends the element.
        test_injector(
            "<html><head><script>var s = \"<script>\";</script><meta charset='utf-8'></head></html>",
            "<html><head><script>var s = \"<script>\";</script><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The double escaped state is left for good: the second closing tag ends the element.
        test_injector(
            "<html><head><script><!--<script></script></script><meta charset='utf-8'></head></html>",
            "<html><head><script><!--<script></script></script><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn abruptly_closed_empty_comment() {
        // `<!-->` and `<!--->` are complete, empty comments: the `-->` may reuse the dashes of the
        // opening `<!--`, for the prescan as much as for the tokenizer, so what follows is markup.
        test_injector(
            "<html><head><!--><meta charset=utf-8></head></html>",
            "<html><head><!--><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><!---><meta charset=utf-8></head></html>",
            "<html><head><!---><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
        // One dash more is a comment that closes on its own dashes rather than an empty one.
        test_injector(
            "<html><head><!----><meta charset=utf-8></head></html>",
            "<html><head><!----><meta charset=utf-8>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head><!--> <meta charset=utf-8> --></head></html>",
            "<html><head><!--> <meta charset=utf-8>{{SNIPPET}} --></head></html>",
        );
    }

    #[test]
    fn a_plaintext_tag_ends_the_markup() {
        // `plaintext` switches the tokenizer to a state it never leaves: everything that follows is
        // text, so the only valid injection point left is the one found before it.
        test_injector(
            "<html><head><plaintext>hello",
            "<html><head>{{SNIPPET}}<plaintext>hello",
        );
        // Unless the text holds a declaration the prescan would honour, as it knows no more about
        // the plaintext state than about the raw text ones: injecting before it could push it out of
        // the prescan window, and there is no point past it to inject at instead.
        test_injector(
            "<html><head><plaintext><meta charset=utf-8>",
            "<html><head><plaintext><meta charset=utf-8>",
        );
        // Not even a closing tag brings the markup back.
        test_injector(
            "<html><head><plaintext></plaintext><meta charset=utf-8></head></html>",
            "<html><head><plaintext></plaintext><meta charset=utf-8></head></html>",
        );
        test_injector("<html><plaintext><head>", "<html><plaintext><head>");
        // A declaration written before the tag is still the one to stay behind.
        test_injector(
            "<html><head><meta charset='utf-8'><plaintext>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}<plaintext>",
        );
    }

    #[test]
    fn a_frameset_ends_the_executable_markup() {
        // A script start tag is ignored in the `in frameset` insertion mode as well as in the
        // `after frameset` one, so nothing from the tag onwards can run: the point found before it is
        // the only one left, and a script placed there still runs.
        test_injector(
            "<html><head></head><frameset><frame src='a.html'></frameset></html>",
            "<html><head>{{SNIPPET}}</head><frameset><frame src='a.html'></frameset></html>",
        );
        // The prescan honours a declaration written within the frameset all the same, and the point
        // found before it would push it further into the document, so injection is declined.
        test_injector(
            "<html><head></head><frameset><meta charset=utf-8><frame src='a.html'></frameset>",
            "<html><head></head><frameset><meta charset=utf-8><frame src='a.html'></frameset>",
        );
        // A declaration written before the tag is still the one to stay behind.
        test_injector(
            "<html><head><meta charset='utf-8'></head><frameset>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}</head><frameset>",
        );
        // With no point found before it, there is nowhere left to inject.
        test_injector(
            "<html><frameset><frame></frameset>",
            "<html><frameset><frame></frameset>",
        );
    }

    #[test]
    fn comments_within_inert_elements() {
        // A `</template>` written in a comment closes nothing, so the snippet must not be placed
        // after it: it would sit in both the comment and the template.
        test_injector(
            "<html><head><template><meta charset=utf-8><!-- > </template> --></template><meta charset='utf-8'></head>",
            "<html><head><template><meta charset=utf-8><!-- > </template> --></template><meta charset='utf-8'>{{SNIPPET}}</head>",
        );
        // A comment left unclosed within a template is one the template never closes either.
        test_injector(
            "<html><head><template><p>x</p><!-- </template>",
            "<html><head>{{SNIPPET}}<template><p>x</p><!-- </template>",
        );
    }

    #[test]
    fn self_closing_start_tag() {
        // Browsers ignore the self-closing flag on a non-void element: `<script/>` stays in script
        // data state, so what follows is source the snippet cannot go within, up to the closing tag.
        test_injector(
            "<html><head><script/>const x = '<meta charset=utf-8>';</script></head></html>",
            "<html><head><script/>const x = '<meta charset=utf-8>';</script>{{SNIPPET}}</head></html>",
        );
        test_injector("<html><head/></html>", "<html><head/>{{SNIPPET}}</html>");
        test_injector(
            "<html><head><meta charset='utf-8'/></head></html>",
            "<html><head><meta charset='utf-8'/>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn tags_within_an_iframe_are_not_markup() {
        // An iframe holds raw text, and a snippet written there would never run. The prescan still
        // honours the declaration below, which is out of the head, so injection is declined.
        test_injector(
            "<html><head></head><body><iframe><meta charset=utf-8></iframe>",
            "<html><head></head><body><iframe><meta charset=utf-8></iframe>",
        );
        // Past the `<body>`, a `<head>` tag is ignored by the browser, and not looked for.
        test_injector(
            "<html><body><iframe>a <b>frame</b></iframe><head>",
            "<html><body><iframe>a <b>frame</b></iframe><head>",
        );
    }

    #[test]
    fn head_in_tag_attribute() {
        test_injector(
            "<html><link foo='<head></head>' /><head></head></html>",
            "<html><link foo='<head></head>' /><head>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn no_head_or_meta() {
        test_injector("<html></html>", "<html></html>");
    }

    #[test]
    fn casing() {
        test_injector("<HeAd></head>", "<HeAd>{{SNIPPET}}</head>");
        test_injector(
            "<HEAD><Meta charset='UtF-8'> ",
            "<HEAD><Meta charset='UtF-8'>{{SNIPPET}} ",
        );
    }

    #[test]
    fn spaces() {
        test_injector("abc <head>def", "abc <head>{{SNIPPET}}def");
        test_injector("abc<head >def", "abc<head >{{SNIPPET}}def");
        test_injector("abc<head >def", "abc<head >{{SNIPPET}}def");
        test_injector("abc<head> def", "abc<head>{{SNIPPET}} def");
    }

    mod no_supported_cases {
        use super::*;

        #[test]
        fn malformed_html() {
            // In case of malformed HTML we won't inject
            test_injector(
                "<html><foo <head ></head></html>",
                "<html><foo <head ></head></html>",
            );
        }
    }

    fn test_injector(input: &str, expected: &str) {
        let mut input_bytes: Vec<u8> = input.as_bytes().to_vec();
        let head = HeadOpenTagLocator::new().scan(&input_bytes);
        if let Some(Placement::At(index)) =
            head.map(|head| return find_injection_point(input.as_bytes(), head.end, true))
        {
            input_bytes.splice(index..index, b"{{SNIPPET}}".iter().copied());
        }
        let result = String::from_utf8(input_bytes).expect("Valid UTF-8");
        assert_eq!(result, expected);
    }
}
