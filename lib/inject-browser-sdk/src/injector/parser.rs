// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Tokenizer-level scan of the head of an HTML document, looking for the earliest position the
//! snippet can be injected at. See [`InjectionPointLocator`].

use super::encoding::has_encoding_attributes;

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
pub(super) struct InjectionPointLocator;

impl InjectionPointLocator {
    /// Returns the index at which the snippet should be inserted, or `None` if no valid injection
    /// point was found within `buffer`.
    pub(super) fn find(buffer: &[u8]) -> Option<usize> {
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
    /// `textarea`, `iframe`, `xmp`), holding its lower case name. Such content must be skipped
    /// rather than scanned.
    RawText(&'static [u8]),
    /// An element whose content is inert (`noscript` with scripting enabled, `noframes`, `noembed`,
    /// `template`), holding its lower case name. A script injected within one would never run,
    /// while the encoding prescan still honours a declaration written there.
    Inert(&'static [u8]),
    /// `plaintext`, which switches the tokenizer to a state it never leaves: everything that follows
    /// the tag is text, and no closing tag can end it.
    Plaintext,
}

fn find_open_tag(input: &[u8], offset: usize) -> Option<(TagName, usize, usize)> {
    let mut i = offset;

    while i < input.len() {
        // find open tag
        if input[i] != b'<' {
            i += 1;
            continue;
        }

        // The content of a comment is not markup either, and the encoding prescan skips it too.
        if input[i..].starts_with(b"<!--") {
            i = find_comment_end(input, i)?;
            continue;
        }

        // A `<![CDATA[` section runs to `]]>` within `svg` or `math` content and is a bogus comment
        // ending at the first `>` anywhere else. Telling the two apart means tracking foreign
        // content, and guessing wrong would either inject into CDATA text, where the snippet would
        // never run, or skip real markup: decline instead, which leaves the point found so far.
        if input[i..].starts_with(b"<![CDATA[") {
            return None;
        }

        let tag_start = i;
        let rest = &input[tag_start + 1..];

        // A doctype or a bogus comment holds no attribute value a `>` could hide in, so it ends at
        // the very next `>` rather than at the one `find_tag_close` looks for.
        if ends_at_first_greater_than(rest) {
            i = find_bogus_comment_end(input, tag_start)?;
            continue;
        }

        if !starts_markup(rest) {
            // A stray `<`, as in `a < b`, is text rather than the start of a tag.
            i += 1;
            continue;
        }

        // find end of tag
        let tag_close: usize = find_tag_close(rest)?;
        let tag_end = tag_start + tag_close + 2;

        // extract tag name
        let name_end = find_tag_name_end(rest)?;

        match parse_tag_name(&rest[..name_end]) {
            // The content of a raw text element is text, not markup: a `<meta charset>` written
            // within a script, a stylesheet or a title is not an encoding declaration, and
            // injecting right after it would insert the snippet in the middle of that text.
            Some(TagName::RawText(name)) => i = find_raw_text_end(input, tag_end, name)?,
            // A `plaintext` tag turns the rest of the document into text: there is no tag left to
            // find, and no point past it the snippet could run at.
            Some(TagName::Plaintext) => return None,
            Some(relevant_tag_name) => return Some((relevant_tag_name, tag_start, tag_end)),
            None => i = tag_end,
        }
    }

    return None;
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
            i += 1;
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

/// Returns the index just past the closing tag of the inert element named `name` whose content
/// starts at `content_start`, or `None` if the element is never closed within `input`.
fn find_inert_element_end(input: &[u8], content_start: usize, name: &[u8]) -> Option<usize> {
    // `noscript` (when scripting is enabled), `noframes` and `noembed` hold raw text, so they
    // cannot nest and the first matching closing tag ends them, wherever it appears.
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

    while i < input.len() {
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
    return input[start..]
        .iter()
        .position(|&byte| return byte == b'>')
        .map(|index| return start + index + 1);
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
    fn noembed_is_an_inert_element() {
        // `noembed` holds raw text, and its content is only shown by a browser that cannot embed, so
        // the snippet cannot go inside it; the encoding prescan still honours a declaration there.
        test_injector(
            "<html><head><noembed><meta charset='utf-8'></noembed></head></html>",
            "<html><head><noembed><meta charset='utf-8'></noembed>{{SNIPPET}}</head></html>",
        );
        test_injector(
            "<html><head></head><body><noembed><meta charset=utf-8></noembed>",
            "<html><head></head><body><noembed><meta charset=utf-8></noembed>{{SNIPPET}}",
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
        // An `xmp` holds raw text as well: its content is displayed literally, so a snippet written
        // there would be shown rather than run.
        test_injector(
            "<html><head><xmp><meta charset=utf-8></xmp></head></html>",
            "<html><head>{{SNIPPET}}<xmp><meta charset=utf-8></xmp></head></html>",
        );
        test_injector(
            "<html><head><xmp><meta charset=utf-8></xmp><meta charset='utf-8'></head></html>",
            "<html><head><xmp><meta charset=utf-8></xmp><meta charset='utf-8'>{{SNIPPET}}</head></html>",
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
        // The tokenizer ends a comment on `--!>` too, so what follows one is markup: the snippet has
        // to stay behind an encoding declaration written there rather than fall back to the `<head>`
        // point, which would push that declaration further into the document.
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
        // emits the token there, so what follows is markup rather than the rest of the identifier,
        // and the meta tag is a real encoding declaration.
        test_injector(
            "<!DOCTYPE html SYSTEM \"x><meta charset=utf-8>\"><head></head>",
            "<!DOCTYPE html SYSTEM \"x><meta charset=utf-8>{{SNIPPET}}\"><head></head>",
        );
    }

    #[test]
    fn cdata_sections_are_not_scanned() {
        // `<![CDATA[` runs to `]]>` in foreign content and ends at the first `>` anywhere else. Which
        // one it is cannot be told without tracking foreign content, so the scan stops and the point
        // found before it stands rather than risking a snippet inside CDATA text.
        test_injector(
            "<html><head><svg><![CDATA[x > <meta charset=utf-8>]]></svg></head></html>",
            "<html><head>{{SNIPPET}}<svg><![CDATA[x > <meta charset=utf-8>]]></svg></head></html>",
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
        // stays open, so the meta tag that follows is script source rather than a declaration.
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
            "<html><head><plaintext><meta charset=utf-8>",
            "<html><head>{{SNIPPET}}<plaintext><meta charset=utf-8>",
        );
        // Not even a closing tag brings the markup back.
        test_injector(
            "<html><head><plaintext></plaintext><meta charset=utf-8></head></html>",
            "<html><head>{{SNIPPET}}<plaintext></plaintext><meta charset=utf-8></head></html>",
        );
        test_injector("<html><plaintext><head>", "<html><plaintext><head>");
        // A declaration written before the tag is still the one to stay behind.
        test_injector(
            "<html><head><meta charset='utf-8'><plaintext>",
            "<html><head><meta charset='utf-8'>{{SNIPPET}}<plaintext>",
        );
    }

    #[test]
    fn comments_within_inert_elements() {
        // A `</template>` written in a comment closes nothing, so the snippet must not be placed
        // after it: it would sit in both the comment and the template.
        test_injector(
            "<html><head><template><meta charset=utf-8><!-- > </template> --></template><meta charset='utf-8'></head>",
            "<html><head><template><meta charset=utf-8><!-- > </template> --></template>{{SNIPPET}}<meta charset='utf-8'></head>",
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
        // data state, so the meta tag written in the source below is not a declaration.
        test_injector(
            "<html><head><script/>const x = '<meta charset=utf-8>';</script></head></html>",
            "<html><head>{{SNIPPET}}<script/>const x = '<meta charset=utf-8>';</script></head></html>",
        );
        test_injector("<html><head/></html>", "<html><head/>{{SNIPPET}}</html>");
        test_injector(
            "<html><head><meta charset='utf-8'/></head></html>",
            "<html><head><meta charset='utf-8'/>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn tags_within_an_iframe_are_not_markup() {
        // An iframe holds raw text, and a snippet written there would never run.
        test_injector(
            "<html><head></head><body><iframe><meta charset=utf-8></iframe>",
            "<html><head>{{SNIPPET}}</head><body><iframe><meta charset=utf-8></iframe>",
        );
        test_injector(
            "<html><body><iframe><meta charset=utf-8></iframe><head>",
            "<html><body><iframe><meta charset=utf-8></iframe><head>{{SNIPPET}}",
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
}
