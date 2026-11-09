// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Whether a `<meta>` tag is one the snippet has to stay behind: a declaration of the character
//! encoding of the document, which injecting before could push out of the window the browser
//! prescans for it, an `X-UA-Compatible` one, which Internet Explorer only honours ahead of every
//! element but `<title>` and other `<meta>` tags, or a `Content-Security-Policy` one, which only
//! governs what follows it. See [`meta_kind`].

use super::parser::compare_case_insensitive;

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

/// What a `<meta>` tag is to the placement of the snippet.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum MetaKind {
    /// It declares the character encoding of the document, see [`has_encoding_attributes`].
    Encoding,
    /// A `http-equiv="X-UA-Compatible"` one, which Internet Explorer honours only when no element
    /// but `<title>` and other `<meta>` tags precedes it: the snippet must not be one.
    Compatibility,
    /// A `http-equiv="Content-Security-Policy"` one. A meta-delivered policy only governs what the
    /// parser sees after it, so the snippet goes after it, under the site's own policy rather than
    /// ahead of it. Opportunistic: only one the walk reaches counts, nothing more is held back to
    /// find one. `Content-Security-Policy-Report-Only` is ignored in a `<meta>`, so it is not one.
    SecurityPolicy,
    /// Any other: SEO, Open Graph, viewport, the tracer's `dd-trace-id`… The snippet does not have
    /// to stay behind it.
    Other,
}

/// Returns what the given `<meta>` open tag, from `<` to `>` included, is to the placement of the
/// snippet. Attributes are read as [`has_encoding_attributes`] reads them: parsed rather than
/// matched as substrings, the first one of each name being the one that counts.
pub(super) fn meta_kind(tag: &[u8]) -> MetaKind {
    if has_encoding_attributes(tag) {
        return MetaKind::Encoding;
    }
    // The value has to spell it out, so a tag without it is not parsed.
    if !contains_ignore_case(tag, b"x-ua") && !contains_ignore_case(tag, b"content-security-policy")
    {
        return MetaKind::Other;
    }
    let Some(mut index) = tag.iter().position(|&b| return b.is_ascii_whitespace()) else {
        return MetaKind::Other;
    };
    loop {
        index = skip_whitespace(tag, index);
        if index >= tag.len() || tag[index] == b'>' {
            return MetaKind::Other;
        }
        let (name, value, next_index) = parse_attribute(tag, index);
        index = next_index;
        // Only the first `http-equiv` counts, the parser drops the others.
        if compare_case_insensitive(name, b"http-equiv") {
            let value = value.trim_ascii();
            if compare_case_insensitive(value, b"x-ua-compatible") {
                return MetaKind::Compatibility;
            }
            if compare_case_insensitive(value, b"content-security-policy") {
                return MetaKind::SecurityPolicy;
            }
            return MetaKind::Other;
        }
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
/// The values are checked, not just their presence: a `<meta charset="">`, a
/// `<meta charset="bogus">` naming no known encoding, or a
/// `<meta http-equiv="content-type" content="text/html">` gives the browser no encoding, so it goes
/// on looking for the real declaration and the snippet has to do the same.
///
/// The attributes are read in source order, as the prescan reads them, since which one provides the
/// encoding depends on the order they are written in. See [`Charset`].
pub(super) fn has_encoding_attributes(tag: &[u8]) -> bool {
    // Both ways to declare one, a `charset` attribute and a `charset=` parameter in a `content`
    // value, spell it out: a tag without it is not parsed.
    if !contains_ignore_case(tag, b"charset") {
        return false;
    }
    // Attributes start after the tag name, at the first whitespace character.
    let Some(mut index) = tag.iter().position(|&b| return b.is_ascii_whitespace()) else {
        return false;
    };

    let mut charset = Charset::Null;
    // Whether the encoding was taken from a `content` value, which declares one only alongside a
    // `http-equiv="content-type"`. Stays `None` while no attribute has provided an encoding at all.
    let mut need_pragma: Option<bool> = None;
    let mut got_pragma = false;
    let (mut seen_charset, mut seen_http_equiv, mut seen_content) = (false, false, false);

    loop {
        index = skip_whitespace(tag, index);
        if index >= tag.len() || tag[index] == b'>' {
            break;
        }

        let (name, value, next_index) = parse_attribute(tag, index);
        index = next_index;

        // The prescan reads the attributes in source order, and duplicates are dropped by the
        // parser, so only the first one of each name counts.
        if !seen_charset && compare_case_insensitive(name, b"charset") {
            seen_charset = true;
            charset = match is_known_encoding_label(value) {
                true => Charset::Known,
                false => Charset::Failure,
            };
            need_pragma = Some(false);
        } else if !seen_http_equiv && compare_case_insensitive(name, b"http-equiv") {
            seen_http_equiv = true;
            got_pragma = compare_case_insensitive(value.trim_ascii(), b"content-type");
        } else if !seen_content && compare_case_insensitive(name, b"content") {
            seen_content = true;
            // A `content` value fills in an encoding only while none has been provided yet: a
            // `charset` attribute written before it, even a bogus one, is the one that stands.
            if charset == Charset::Null && has_charset_parameter(value) {
                charset = Charset::Known;
                need_pragma = Some(true);
            }
        }
    }

    return match need_pragma {
        // No attribute provided an encoding, so the tag declares none.
        None => false,
        // A `content` value declares the encoding only alongside `http-equiv="content-type"`.
        Some(true) if !got_pragma => false,
        _ => charset == Charset::Known,
    };
}

/// The encoding the prescan has taken from the attributes read so far.
///
/// [`Failure`](Charset::Failure) is not the same as [`Null`](Charset::Null): a `charset` attribute
/// naming no known encoding discards the one a `content` value provided, and leaves the browser
/// looking for the next declaration rather than falling back to it.
#[derive(PartialEq, Eq)]
enum Charset {
    Null,
    Failure,
    Known,
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

/// Whether `tag` holds `needle_lower_case`, a lower case ASCII word, in any case.
fn contains_ignore_case(tag: &[u8], needle_lower_case: &[u8]) -> bool {
    let first = needle_lower_case[0];
    let mut offset = 0;
    while let Some(index) = memchr::memchr2(first, first.to_ascii_uppercase(), &tag[offset..]) {
        offset += index;
        if tag[offset..].len() >= needle_lower_case.len()
            && tag[offset..offset + needle_lower_case.len()].eq_ignore_ascii_case(needle_lower_case)
        {
            return true;
        }
        offset += 1;
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
