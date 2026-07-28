// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

/// Stateless helper used to locate the "injection point", in other words where we should inject the
/// snippet in an HTML document.
///
/// The current implementation looks for the earliest valid position within the `<head>` tag:
/// * right after the open `<head>` tag,
/// * or right after a character encoding declaration (`<meta charset=...>` or
///   `<meta http-equiv="content-type" content="...; charset=...">`), as the snippet must not push
///   such a declaration further into the document,
/// * or right after an inert element (`<noscript>`, `<template>`) holding such a declaration, since
///   a script injected within one would never run.
///
/// The character encoding declaration is the only thing the snippet is kept behind. In particular,
/// the snippet is deliberately injected *before* a `<meta http-equiv="content-security-policy">`
/// element that follows the encoding declaration: injecting as early as possible is the point of
/// this locator, and a meta-delivered policy only governs what the parser sees after it.
pub struct InjectionPointLocator;

impl InjectionPointLocator {
    /// Returns the index at which the snippet should be inserted, or `None` if no valid injection
    /// point was found within `buffer`.
    pub fn find(buffer: &[u8]) -> Option<usize> {
        let mut head_tag_point: Option<usize> = None;
        let mut offset = 0;

        while let Some((tag_name, tag_start, tag_end)) = find_open_tag(buffer, offset) {
            match tag_name {
                TagName::Head if head_tag_point.is_none() => {
                    head_tag_point = Some(tag_end);
                }
                TagName::Meta if has_encoding_attributes(&buffer[tag_start..tag_end]) => {
                    return Some(tag_end);
                }
                TagName::Inert(name) => {
                    // The snippet cannot go within an inert element, but the encoding prescan runs
                    // over the raw bytes and honours a declaration written there, so a declaration
                    // inside keeps the snippet behind the element as a whole.
                    let element_end = match find_inert_element_end(buffer, tag_end, name) {
                        Some(element_end) => element_end,
                        // The element is never closed, so everything that follows is inert too.
                        None => break,
                    };
                    if contains_encoding_declaration(&buffer[tag_end..element_end]) {
                        return Some(element_end);
                    }
                    offset = element_end;
                    continue;
                }
                _ => {
                    // Skip unknown tags, continue scanning
                }
            }
            offset = tag_end;
        }

        return head_tag_point;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TagName {
    Meta,
    Head,
    /// An element whose content is text rather than markup (`script`, `style`, `title`,
    /// `textarea`), holding its lower case name. Such content must be skipped rather than scanned.
    RawText(&'static [u8]),
    /// An element whose content is inert (`noscript` with scripting enabled, `template`), holding
    /// its lower case name. Its content *is* markup, unlike [`TagName::RawText`], but a script
    /// injected within it would never run.
    Inert(&'static [u8]),
}

fn find_open_tag(input: &[u8], offset: usize) -> Option<(TagName, usize, usize)> {
    let mut i = offset;

    while i < input.len() {
        // find open tag
        if input[i] != b'<' {
            i += 1;
            continue;
        }

        let tag_start = i;
        let rest = &input[tag_start + 1..];

        // find end of tag
        let tag_close: usize = find_tag_close(rest)?;
        let tag_end = tag_start + tag_close + 2;

        // extract tag name
        let name_end = rest
            .iter()
            .position(|&b| return b.is_ascii_whitespace() || b == b'>')?;

        match parse_tag_name(&rest[..name_end]) {
            // The content of a raw text element is text, not markup: a `<meta charset>` written
            // within a script, a stylesheet or a title is not an encoding declaration, and
            // injecting right after it would insert the snippet in the middle of that text.
            Some(TagName::RawText(name)) => i = find_raw_text_end(input, tag_end, name)?,
            Some(relevant_tag_name) => return Some((relevant_tag_name, tag_start, tag_end)),
            None => i = tag_end,
        }
    }

    return None;
}

/// Returns the index just past the closing tag of the raw text element named `name` whose content
/// starts at `content_start`, or `None` if the element is never closed within `input`.
fn find_raw_text_end(input: &[u8], content_start: usize, name: &[u8]) -> Option<usize> {
    let mut i = content_start;

    while i < input.len() {
        if input[i] != b'<' {
            i += 1;
            continue;
        }

        let rest = &input[i + 1..];
        if is_tag_named(rest, true, name) {
            return Some(i + find_tag_close(rest)? + 2);
        }

        i += 1;
    }

    return None;
}

/// Returns the index just past the closing tag of the inert element named `name` whose content
/// starts at `content_start`, or `None` if the element is never closed within `input`.
fn find_inert_element_end(input: &[u8], content_start: usize, name: &[u8]) -> Option<usize> {
    // `template` elements nest, so their closing tags have to be paired. `noscript` content is raw
    // text when scripting is enabled, so it cannot nest.
    let nests = name == b"template";
    let mut depth = 1usize;
    let mut i = content_start;

    while i < input.len() {
        if input[i] != b'<' {
            i += 1;
            continue;
        }

        let rest = &input[i + 1..];
        if is_tag_named(rest, true, name) {
            let tag_end = i + find_tag_close(rest)? + 2;
            depth -= 1;
            if depth == 0 {
                return Some(tag_end);
            }
            i = tag_end;
        } else if nests && is_tag_named(rest, false, name) {
            depth += 1;
            i += find_tag_close(rest)? + 2;
        } else {
            i += 1;
        }
    }

    return None;
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
/// Used to decide whether the snippet has to be kept behind an inert element: the browser's encoding
/// prescan reads the raw bytes, so it honours a declaration wherever it is nested.
fn contains_encoding_declaration(content: &[u8]) -> bool {
    let mut offset = 0;

    while let Some((tag_name, tag_start, tag_end)) = find_open_tag(content, offset) {
        if tag_name == TagName::Meta && has_encoding_attributes(&content[tag_start..tag_end]) {
            return true;
        }
        offset = tag_end;
    }

    return false;
}

/// Returns the index of the `>` character closing the tag whose content starts at the beginning of
/// `input`, or `None` if there is none.
///
/// `>` characters appearing within a quoted attribute value do not close the tag, so they are
/// skipped: injecting at such a position would insert the snippet inside the attribute value.
fn find_tag_close(input: &[u8]) -> Option<usize> {
    let mut quote: Option<u8> = None;

    for (index, &byte) in input.iter().enumerate() {
        match quote {
            Some(quote_byte) => {
                if byte == quote_byte {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Some(index),
                _ => {}
            },
        }
    }

    return None;
}

fn parse_tag_name(input: &[u8]) -> Option<TagName> {
    if compare_case_insensitive(input, b"meta") {
        return Some(TagName::Meta);
    } else if compare_case_insensitive(input, b"head") {
        return Some(TagName::Head);
    } else if compare_case_insensitive(input, b"script") {
        return Some(TagName::RawText(b"script"));
    } else if compare_case_insensitive(input, b"style") {
        return Some(TagName::RawText(b"style"));
    } else if compare_case_insensitive(input, b"title") {
        return Some(TagName::RawText(b"title"));
    } else if compare_case_insensitive(input, b"textarea") {
        return Some(TagName::RawText(b"textarea"));
    } else if compare_case_insensitive(input, b"noscript") {
        return Some(TagName::Inert(b"noscript"));
    } else if compare_case_insensitive(input, b"template") {
        return Some(TagName::Inert(b"template"));
    } else {
        return None;
    }
}

/// Returns whether the given open tag, from `<` to `>` included, declares the character encoding of
/// the document, in other words whether it carries a `charset` attribute or is a
/// `http-equiv="content-type"` declaration.
///
/// Attributes are parsed rather than matched as substrings, so that attributes merely containing
/// `charset` (`data-charset`) and unrelated `http-equiv` values (`refresh`) are not mistaken for an
/// encoding declaration: injecting right after those would place the snippet *before* the actual
/// declaration.
///
/// Their values are checked as well, for the same reason: a `<meta charset="">` or a
/// `<meta http-equiv="content-type" content="text/html">` gives the browser no encoding, so it goes
/// on looking for the real declaration and the snippet has to do the same.
fn has_encoding_attributes(tag: &[u8]) -> bool {
    // Attributes start after the tag name, at the first whitespace character.
    let Some(mut index) = tag.iter().position(|&b| return b.is_ascii_whitespace()) else {
        return false;
    };

    let mut charset: Option<&[u8]> = None;
    let mut http_equiv: Option<&[u8]> = None;
    let mut content: Option<&[u8]> = None;

    loop {
        index = skip_whitespace(tag, index);
        if index >= tag.len() || tag[index] == b'>' {
            break;
        }

        let (name, value, next_index) = parse_attribute(tag, index);
        for (attribute_name, slot) in [
            (&b"charset"[..], &mut charset),
            (&b"http-equiv"[..], &mut http_equiv),
            (&b"content"[..], &mut content),
        ] {
            // Duplicate attributes are dropped by the parser, so only the first one counts. The
            // attributes are collected rather than matched in place because `content` may be
            // written before `http-equiv`.
            if slot.is_none() && compare_case_insensitive(name, attribute_name) {
                *slot = Some(value);
            }
        }
        index = next_index;
    }

    if charset.is_some_and(|value| return !value.trim_ascii().is_empty()) {
        return true;
    }

    // A `http-equiv="content-type"` only declares the encoding through the `charset` parameter of
    // its `content` value.
    return http_equiv
        .is_some_and(|value| return compare_case_insensitive(value.trim_ascii(), b"content-type"))
        && content.is_some_and(has_charset_parameter);
}

/// Returns whether the given `content` attribute value carries a non-empty `charset` parameter, as
/// in `text/html; charset=utf-8`.
fn has_charset_parameter(content: &[u8]) -> bool {
    let mut offset = 0;

    while let Some(found) = position_case_insensitive(&content[offset..], b"charset") {
        let mut index = skip_whitespace(content, offset + found + b"charset".len());
        offset += found + b"charset".len();

        if index >= content.len() || content[index] != b'=' {
            continue;
        }
        index = skip_whitespace(content, index + 1);

        let value = match content.get(index) {
            Some(&quote) if quote == b'"' || quote == b'\'' => {
                take_while(&content[index + 1..], |b| return b != quote)
            }
            _ => take_while(&content[index..], |b| {
                return b != b';' && !b.is_ascii_whitespace();
            }),
        };
        return !value.trim_ascii().is_empty();
    }

    return false;
}

fn take_while(input: &[u8], predicate: impl Fn(u8) -> bool) -> &[u8] {
    let end = input
        .iter()
        .position(|&b| return !predicate(b))
        .unwrap_or(input.len());
    return &input[..end];
}

fn position_case_insensitive(haystack: &[u8], needle_lower_case: &[u8]) -> Option<usize> {
    if haystack.len() < needle_lower_case.len() {
        return None;
    }
    return haystack
        .windows(needle_lower_case.len())
        .position(|window| return compare_case_insensitive(window, needle_lower_case));
}

/// Parses the attribute starting at `index`, returning its name, its value (empty when the attribute
/// has none) and the index at which the next attribute may start.
///
/// `tag[index]` must be neither a whitespace character nor `>`, which guarantees that the returned
/// index is greater than `index`.
fn parse_attribute(tag: &[u8], index: usize) -> (&[u8], &[u8], usize) {
    let mut i = index;

    while i < tag.len() && !tag[i].is_ascii_whitespace() && tag[i] != b'=' && tag[i] != b'>' {
        i += 1;
    }
    let name = &tag[index..i];

    // Whitespace is allowed around the '=' sign.
    i = skip_whitespace(tag, i);
    if i >= tag.len() || tag[i] != b'=' {
        return (name, b"", i);
    }
    i = skip_whitespace(tag, i + 1);

    if i < tag.len() && (tag[i] == b'"' || tag[i] == b'\'') {
        let quote = tag[i];
        let value_start = i + 1;
        i = value_start;
        while i < tag.len() && tag[i] != quote {
            i += 1;
        }
        return (name, &tag[value_start..i], (i + 1).min(tag.len()));
    }

    let value_start = i;
    while i < tag.len() && !tag[i].is_ascii_whitespace() && tag[i] != b'>' {
        i += 1;
    }
    return (name, &tag[value_start..i], i);
}

fn skip_whitespace(input: &[u8], mut index: usize) -> usize {
    while index < input.len() && input[index].is_ascii_whitespace() {
        index += 1;
    }
    return index;
}

fn compare_case_insensitive(a: &[u8], b_lower_case: &[u8]) -> bool {
    if a.len() != b_lower_case.len() {
        return false;
    }
    return a
        .iter()
        .zip(b_lower_case)
        .all(|(a, &b_lower_case)| return a.to_ascii_lowercase() == b_lower_case);
}

/// The location of an injection point, as computed by the [`HeadCloseTagLocator`].
#[cfg_attr(test, derive(Debug))]
pub enum Location {
    /// Nothing was found. If a potential injection point was previously notified, it should be
    /// dismissed.
    None,

    /// A potential injection point was found in the current chunk. More data is needed to confirm
    /// that it is a valid injection point. If a potential injection point was previously notified,
    /// it should be dismissed.
    PotentialFromIndex(usize),

    /// The potential injection point that started within a previous chunk is still unconfirmed.
    /// More data is needed to confirm that it is a valid injection point.
    PotentialFromPreviousChunk,

    /// An injection point was found in the current chunk. If a potential injection point was
    /// previously notified, it should be dismissed.
    MatchFromIndex(usize),

    /// The potential injection point that started within a previous chunk is confirmed.
    MatchFromPreviousChunk,
}

/// Stateful object used to locate the `</head>` tag, so we can inject the snippet right before it.
///
/// This is the fallback used when [`InjectionPointLocator`] found no earlier valid injection point.
/// Injecting here is always safe with respect to the character encoding declaration, since a
/// closing `</head>` tag can never precede a `<meta charset>` within the same `<head>`. Unlike
/// [`InjectionPointLocator`] it scans in a streaming fashion, so it never needs to hold back more
/// than the length of a partially matched tag.
///
/// It is the equivalent of the regex `/<\/\s*head\s*>/i`: case insensitive and allows spaces before
/// and after 'head'.
pub struct HeadCloseTagLocator {
    /// Goes from 0 to 6, where each value correspond to an expected character of `</head>`.
    /// 0 -> we are looking for the character '<'
    /// 1 -> we are looking for the character '/'
    /// 2 -> we are looking for the character 'h'
    /// etc.
    index: u8,
}

impl HeadCloseTagLocator {
    pub fn new() -> Self {
        return Self { index: 0 };
    }

    pub fn scan(&mut self, chunk: &[u8]) -> Location {
        let mut location = if self.index > 0 {
            Location::PotentialFromPreviousChunk
        } else {
            Location::None
        };

        for (index, mut byte) in chunk.iter().copied().enumerate() {
            if self.index > 1 {
                byte = byte.to_ascii_lowercase();
            }

            match (self.index, byte) {
                // Consider any '<' character as a potential injection point.
                (_, b'<') => {
                    self.index = 1;
                    location = Location::PotentialFromIndex(index);
                }

                // If we find the expected character, increment `index` to look for the next one.
                (1, b'/') | (2, b'h') | (3, b'e') | (4, b'a') | (5, b'd') => {
                    self.index += 1;
                }

                // If we find the last character '>', reset the state and return the matched
                // location.
                (6, b'>') => {
                    self.index = 0;
                    location = match location {
                        Location::PotentialFromPreviousChunk => Location::MatchFromPreviousChunk,
                        Location::PotentialFromIndex(index) => Location::MatchFromIndex(index),
                        _ => unreachable!(),
                    };
                    break;
                }

                // Ignore whitespaces before characters 'h' and '>'.
                (2 | 6, byte) if byte.is_ascii_whitespace() => {}

                // Any other character is unexpected, reset the state.
                _ => {
                    self.index = 0;
                    location = Location::None;
                }
            }
        }

        return location;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
    use pretty_assertions::assert_matches;

    #[test]
    fn meta_with_charset() {
        test_injector(
            "<html><head><meta charset='...'></head></html>",
            "<html><head><meta charset='...'>{{SNIPPET}}</head></html>",
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
        // A `content` mentioning 'charset' without a value declares nothing.
        test_injector(
            "<html><head><meta http-equiv='content-type' content='text/html; charset='></head></html>",
            "<html><head>{{SNIPPET}}<meta http-equiv='content-type' content='text/html; charset='></head></html>",
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
    fn unterminated_inert_element() {
        // Everything following an unclosed template is inert, so the only valid injection point left
        // is the one found before it.
        test_injector(
            "<html><head><template><meta charset='utf-8'>",
            "<html><head>{{SNIPPET}}<template><meta charset='utf-8'>",
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
            "<html><head data-x='>'><meta charset='>'></head></html>",
            "<html><head data-x='>'><meta charset='>'>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head data-x='>'><meta name='>'></head></html>",
            "<html><head data-x='>'>{{SNIPPET}}<meta name='>'></head></html>",
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
    fn head_and_meta_tags() {
        test_injector(
            "<html><head><meta charset='...'><meta http-equiv='...'></head></html>",
            "<html><head><meta charset='...'>{{SNIPPET}}<meta http-equiv='...'></head></html>",
        );
    }

    #[test]
    fn tags_within_raw_text_elements_are_not_markup() {
        // A meta tag written in a script or a stylesheet source is not an encoding declaration:
        // injecting after it would corrupt the script or the stylesheet.
        test_injector(
            "<html><head><script>const x = '<meta charset=utf-8>';</script></head></html>",
            "<html><head>{{SNIPPET}}<script>const x = '<meta charset=utf-8>';</script></head></html>",
        );
        test_injector(
            "<html><head><style>/* <meta charset=utf-8> */</style><meta charset='utf-8'></head></html>",
            "<html><head><style>/* <meta charset=utf-8> */</style><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The closing tag is matched case insensitively and may carry trailing spaces.
        test_injector(
            "<html><head><SCRIPT><meta charset=utf-8></SCRIPT ><meta charset='utf-8'></head></html>",
            "<html><head><SCRIPT><meta charset=utf-8></SCRIPT ><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // The content of a title is text as well: a meta tag written there is displayed, not parsed.
        test_injector(
            "<html><head><title>Using <meta charset=utf-8></title></head></html>",
            "<html><head>{{SNIPPET}}<title>Using <meta charset=utf-8></title></head></html>",
        );
        test_injector(
            "<html><head><title>Using <meta charset=utf-8></title><meta charset='utf-8'></head></html>",
            "<html><head><title>Using <meta charset=utf-8></title><meta charset='utf-8'>{{SNIPPET}}</head></html>",
        );
        // A `</scriptfoo>` tag does not close the script element.
        test_injector(
            "<html><head><script></scriptfoo><meta charset=utf-8></script></head></html>",
            "<html><head>{{SNIPPET}}<script></scriptfoo><meta charset=utf-8></script></head></html>",
        );
    }

    #[test]
    fn unterminated_raw_text_element() {
        // Everything following an unclosed script tag is script source, so the only valid injection
        // point left is the one found before it.
        test_injector(
            "<html><head><script><meta charset=utf-8>",
            "<html><head>{{SNIPPET}}<script><meta charset=utf-8>",
        );
        test_injector("<html><script><head>", "<html><script><head>");
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
            "<Meta charset='UtF-8'> ",
            "<Meta charset='UtF-8'>{{SNIPPET}} ",
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

        #[test]
        fn comment_tags() {
            // If we first find a commented open head tag we would inject in the comment
            test_injector(
                "<!--<html><head></head></html>-->",
                "<!--<html><head>{{SNIPPET}}</head></html>-->",
            );
        }

        #[test]
        fn script_before_meta() {
            // We would inject after a meta tag even if some script are defined before
            test_injector(
                "<html><head></head><script></script><meta charset='utf-8'></html>",
                "<html><head></head><script></script><meta charset='utf-8'>{{SNIPPET}}</html>",
            );
        }
    }

    fn test_injector(input: &str, expected: &str) {
        let mut input_bytes: Vec<u8> = input.as_bytes().to_vec();
        if let Some(index) = InjectionPointLocator::find(&input_bytes) {
            input_bytes.splice(index..index, b"{{SNIPPET}}".iter().copied());
        }
        let result = String::from_utf8(input_bytes).expect("Valid UTF-8");
        assert_eq!(result, expected);
    }

    mod head_close_tag {
        use super::*;

        #[test]
        fn basic() {
            let mut locator = HeadCloseTagLocator::new();
            assert_matches!(locator.scan(b"abc</head>def"), Location::MatchFromIndex(3));
        }

        #[test]
        fn streaming() {
            let mut locator = HeadCloseTagLocator::new();
            assert_matches!(locator.scan(b""), Location::None);
            assert_matches!(locator.scan(b"a"), Location::None);
            assert_matches!(locator.scan(b" "), Location::None);
            assert_matches!(locator.scan(b" <"), Location::PotentialFromIndex(1));
            assert_matches!(locator.scan(b""), Location::PotentialFromPreviousChunk);
            assert_matches!(locator.scan(b"/"), Location::PotentialFromPreviousChunk);
            assert_matches!(locator.scan(b"head>"), Location::MatchFromPreviousChunk);
            assert_matches!(locator.scan(b""), Location::None);
            assert_matches!(locator.scan(b"a"), Location::None);
        }

        #[test]
        fn casing() {
            let mut locator = HeadCloseTagLocator::new();
            assert_matches!(locator.scan(b"abc</HEAD>def"), Location::MatchFromIndex(3));
        }

        #[test]
        fn space_tolerant() {
            let mut locator = HeadCloseTagLocator::new();
            assert_matches!(
                locator.scan(b"abc</\n\r head \n\t>def"),
                Location::MatchFromIndex(3)
            );
        }
    }
}
