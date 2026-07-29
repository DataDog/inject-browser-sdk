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
///
/// A meta-delivered policy written *before* the encoding declaration does govern the snippet, and a
/// strict `script-src` blocks it outright. Nothing here works around that: a policy delivered by
/// response header applies regardless of position and is the common case, so the fix belongs in
/// nonce or hash support rather than in where the snippet is placed.
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
                        None => {
                            // The closing tag is not in `buffer`: either the element is never
                            // closed, in which case everything that follows is inert too, or it
                            // closes past the end of the buffer. Either way there is no point past
                            // it to inject at, so fall back to the `<head>` point — unless the part
                            // we can see already holds an encoding declaration, which that fallback
                            // would push further into the document.
                            if contains_encoding_declaration(&buffer[tag_end..]) {
                                return None;
                            }
                            break;
                        }
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
    /// An element whose content is inert (`noscript` with scripting enabled, `noframes`,
    /// `template`), holding its lower case name. A script injected within one would never run,
    /// while the encoding prescan still honours a declaration written there.
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
    // `noscript` (when scripting is enabled) and `noframes` hold raw text, so they cannot nest and
    // the first matching closing tag ends them, wherever it appears.
    if name != b"template" {
        return find_raw_text_end(input, content_start, name);
    }

    // `template` content is markup: its closing tags have to be paired, and a literal `</template>`
    // written within an attribute value, a comment or a raw text element closes nothing.
    let mut depth = 1usize;
    let mut i = content_start;

    while i < input.len() {
        if input[i] != b'<' {
            i += 1;
            continue;
        }

        let rest = &input[i + 1..];
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
            // The content of a raw text element is not markup either.
            let name_end = rest
                .iter()
                .position(|&b| return b.is_ascii_whitespace() || b == b'>')?;
            i = match parse_tag_name(&rest[..name_end]) {
                Some(TagName::RawText(raw_text_name)) => {
                    find_raw_text_end(input, tag_end, raw_text_name)?
                }
                _ => tag_end,
            };
        }
    }

    return None;
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
    } else if compare_case_insensitive(input, b"noframes") {
        return Some(TagName::Inert(b"noframes"));
    } else if compare_case_insensitive(input, b"template") {
        return Some(TagName::Inert(b"template"));
    } else {
        return None;
    }
}

/// Every character encoding label the browser can resolve, in the order the WHATWG Encoding
/// Standard lists them (<https://encoding.spec.whatwg.org/encodings.json>), stored lower case.
///
/// A `charset` value that is not one of these resolves to no encoding, so the browser's prescan goes
/// on looking for the real declaration and the snippet has to do the same.
// Kept packed and grouped by encoding, which is how the standard reads; one label per line would
// run to a few hundred lines and lose that structure.
#[rustfmt::skip]
const ENCODING_LABELS: &[&[u8]] = &[
    // UTF-8
    b"unicode-1-1-utf-8", b"unicode11utf8", b"unicode20utf8", b"utf-8", b"utf8", b"x-unicode20utf8",
    // IBM866
    b"866", b"cp866", b"csibm866", b"ibm866",
    // ISO-8859-2
    b"csisolatin2", b"iso-8859-2", b"iso-ir-101", b"iso8859-2", b"iso88592", b"iso_8859-2",
    b"iso_8859-2:1987", b"l2", b"latin2",
    // ISO-8859-3
    b"csisolatin3", b"iso-8859-3", b"iso-ir-109", b"iso8859-3", b"iso88593", b"iso_8859-3",
    b"iso_8859-3:1988", b"l3", b"latin3",
    // ISO-8859-4
    b"csisolatin4", b"iso-8859-4", b"iso-ir-110", b"iso8859-4", b"iso88594", b"iso_8859-4",
    b"iso_8859-4:1988", b"l4", b"latin4",
    // ISO-8859-5
    b"csisolatincyrillic", b"cyrillic", b"iso-8859-5", b"iso-ir-144", b"iso8859-5", b"iso88595",
    b"iso_8859-5", b"iso_8859-5:1988",
    // ISO-8859-6
    b"arabic", b"asmo-708", b"csiso88596e", b"csiso88596i", b"csisolatinarabic", b"ecma-114",
    b"iso-8859-6", b"iso-8859-6-e", b"iso-8859-6-i", b"iso-ir-127", b"iso8859-6", b"iso88596",
    b"iso_8859-6", b"iso_8859-6:1987",
    // ISO-8859-7
    b"csisolatingreek", b"ecma-118", b"elot_928", b"greek", b"greek8", b"iso-8859-7", b"iso-ir-126",
    b"iso8859-7", b"iso88597", b"iso_8859-7", b"iso_8859-7:1987", b"sun_eu_greek",
    // ISO-8859-8
    b"csiso88598e", b"csisolatinhebrew", b"hebrew", b"iso-8859-8", b"iso-8859-8-e", b"iso-ir-138",
    b"iso8859-8", b"iso88598", b"iso_8859-8", b"iso_8859-8:1988", b"visual",
    // ISO-8859-8-I
    b"csiso88598i", b"iso-8859-8-i", b"logical",
    // ISO-8859-10
    b"csisolatin6", b"iso-8859-10", b"iso-ir-157", b"iso8859-10", b"iso885910", b"l6", b"latin6",
    // ISO-8859-13
    b"iso-8859-13", b"iso8859-13", b"iso885913",
    // ISO-8859-14
    b"iso-8859-14", b"iso8859-14", b"iso885914",
    // ISO-8859-15
    b"csisolatin9", b"iso-8859-15", b"iso8859-15", b"iso885915", b"iso_8859-15", b"l9",
    // ISO-8859-16
    b"iso-8859-16",
    // KOI8-R
    b"cskoi8r", b"koi", b"koi8", b"koi8-r", b"koi8_r",
    // KOI8-U
    b"koi8-ru", b"koi8-u",
    // macintosh
    b"csmacintosh", b"mac", b"macintosh", b"x-mac-roman",
    // windows-874
    b"dos-874", b"iso-8859-11", b"iso8859-11", b"iso885911", b"tis-620", b"windows-874",
    // windows-1250
    b"cp1250", b"windows-1250", b"x-cp1250",
    // windows-1251
    b"cp1251", b"windows-1251", b"x-cp1251",
    // windows-1252
    b"ansi_x3.4-1968", b"ascii", b"cp1252", b"cp819", b"csisolatin1", b"ibm819", b"iso-8859-1",
    b"iso-ir-100", b"iso8859-1", b"iso88591", b"iso_8859-1", b"iso_8859-1:1987", b"l1", b"latin1",
    b"us-ascii", b"windows-1252", b"x-cp1252",
    // windows-1253
    b"cp1253", b"windows-1253", b"x-cp1253",
    // windows-1254
    b"cp1254", b"csisolatin5", b"iso-8859-9", b"iso-ir-148", b"iso8859-9", b"iso88599",
    b"iso_8859-9", b"iso_8859-9:1989", b"l5", b"latin5", b"windows-1254", b"x-cp1254",
    // windows-1255
    b"cp1255", b"windows-1255", b"x-cp1255",
    // windows-1256
    b"cp1256", b"windows-1256", b"x-cp1256",
    // windows-1257
    b"cp1257", b"windows-1257", b"x-cp1257",
    // windows-1258
    b"cp1258", b"windows-1258", b"x-cp1258",
    // x-mac-cyrillic
    b"x-mac-cyrillic", b"x-mac-ukrainian",
    // GBK
    b"chinese", b"csgb2312", b"csiso58gb231280", b"gb2312", b"gb_2312", b"gb_2312-80", b"gbk",
    b"iso-ir-58", b"x-gbk",
    // gb18030
    b"gb18030",
    // Big5
    b"big5", b"big5-hkscs", b"cn-big5", b"csbig5", b"x-x-big5",
    // EUC-JP
    b"cseucpkdfmtjapanese", b"euc-jp", b"x-euc-jp",
    // ISO-2022-JP
    b"csiso2022jp", b"iso-2022-jp",
    // Shift_JIS
    b"csshiftjis", b"ms932", b"ms_kanji", b"shift-jis", b"shift_jis", b"sjis", b"windows-31j",
    b"x-sjis",
    // EUC-KR
    b"cseuckr", b"csksc56011987", b"euc-kr", b"iso-ir-149", b"korean", b"ks_c_5601-1987",
    b"ks_c_5601-1989", b"ksc5601", b"ksc_5601", b"windows-949",
    // replacement
    b"csiso2022kr", b"hz-gb-2312", b"iso-2022-cn", b"iso-2022-cn-ext", b"iso-2022-kr",
    b"replacement",
    // UTF-16BE
    b"unicodefffe", b"utf-16be",
    // UTF-16LE
    b"csunicode", b"iso-10646-ucs-2", b"ucs-2", b"unicode", b"unicodefeff", b"utf-16", b"utf-16le",
    // x-user-defined
    b"x-user-defined",
];

/// Returns whether `value`, the value of a `charset` attribute or parameter, is a label the browser
/// resolves to a character encoding.
fn is_known_encoding_label(value: &[u8]) -> bool {
    let value = value.trim_ascii();
    return ENCODING_LABELS
        .iter()
        .any(|label| return compare_case_insensitive(value, label));
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
/// The values are checked, not just their presence: a `<meta charset="">`, a
/// `<meta charset="bogus">` naming no known encoding, or a
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

    if charset.is_some_and(is_known_encoding_label) {
        return true;
    }

    // A `http-equiv="content-type"` only declares the encoding through the `charset` parameter of
    // its `content` value.
    return http_equiv
        .is_some_and(|value| return compare_case_insensitive(value.trim_ascii(), b"content-type"))
        && content.is_some_and(has_charset_parameter);
}

/// Returns whether the given `content` attribute value carries a `charset` parameter naming a known
/// encoding, as in `text/html; charset=utf-8`.
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
        return is_known_encoding_label(value);
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
/// It is the equivalent of the regex `/<\/head\s*>/i`: case insensitive and allows spaces after
/// 'head'. A `</` followed by anything but a letter starts a bogus comment rather than an end tag,
/// so `</ head>` closes no head in any browser and is not matched here either.
pub struct HeadCloseTagLocator {
    /// Goes from 0 to 6, where each value correspond to an expected character of `</head>`.
    /// 0 -> we are looking for the character '<'
    /// 1 -> we are looking for the character '/'
    /// 2 -> we are looking for the character 'h'
    /// etc.
    index: u8,

    /// Number of bytes consumed by the match currently pending, reset along with `index`. See
    /// [`MAX_PENDING_CLOSE_TAG`].
    pending_length: usize,
}

/// Largest number of bytes a pending `</head>` match may span before it is abandoned.
///
/// The injector holds back every byte of a pending match, so without a cap the whitespace run
/// `</head\s*>` tolerates would let a single `</head` followed by megabytes of whitespace stall the
/// response and grow memory without bound. No real document writes a kilobyte of whitespace inside
/// an end tag, so past this many bytes we give up on the match and let the data flow.
const MAX_PENDING_CLOSE_TAG: usize = 1024;

impl HeadCloseTagLocator {
    pub fn new() -> Self {
        return Self {
            index: 0,
            pending_length: 0,
        };
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
                    self.pending_length = 0;
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

                // Ignore whitespaces before the character '>'.
                (6, byte) if byte.is_ascii_whitespace() => {}

                // Any other character is unexpected, reset the state.
                _ => {
                    self.index = 0;
                    location = Location::None;
                }
            }

            // Whitespace before '>' is the only thing that can stretch a pending match, and the
            // injector holds every byte of it back. Abandon a match that outgrows the cap rather
            // than buffering it, see [`MAX_PENDING_CLOSE_TAG`].
            if self.index > 0 {
                self.pending_length += 1;
                if self.pending_length > MAX_PENDING_CLOSE_TAG {
                    self.index = 0;
                    self.pending_length = 0;
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
            "<html><head><meta charset='utf-8'><meta http-equiv='...'></head></html>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}<meta http-equiv='...'></head></html>",
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
                locator.scan(b"abc</head \n\t>def"),
                Location::MatchFromIndex(3)
            );
        }

        /// `</` followed by anything but a letter is a bogus comment per the HTML tokenizer, so
        /// `</ head>` closes no head.
        #[test]
        fn space_after_the_solidus_is_not_an_end_tag() {
            let mut locator = HeadCloseTagLocator::new();
            assert_matches!(locator.scan(b"abc</\n\r head>def"), Location::None);
        }

        /// A pending match cannot grow without bound: the injector holds every byte of it back.
        #[test]
        fn abandons_a_pending_match_that_outgrows_the_cap() {
            let mut locator = HeadCloseTagLocator::new();
            let mut input = Vec::from(b"abc</head".as_slice());
            // One space too many: the pending match spans MAX_PENDING_CLOSE_TAG + 1 bytes.
            input.resize(3 + MAX_PENDING_CLOSE_TAG + 1, b' ');
            input.push(b'>');

            assert_matches!(locator.scan(&input), Location::None);

            // Exactly at the cap, the very same shape still matches.
            let mut locator = HeadCloseTagLocator::new();
            let mut input = Vec::from(b"abc</head".as_slice());
            input.resize(3 + MAX_PENDING_CLOSE_TAG, b' ');
            input.push(b'>');

            assert_matches!(locator.scan(&input), Location::MatchFromIndex(3));
        }
    }
}
