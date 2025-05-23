pub struct InjectionPointLocator;

impl InjectionPointLocator {
    pub fn find(buffer: &[u8]) -> Option<usize> {
        let mut head_tag_point: Option<usize> = None;
        let mut offset = 0;

        while let Some((tag_name, tag_start, tag_end)) = find_open_tag(buffer, offset) {
            match tag_name {
                TagName::Head if head_tag_point.is_none() => {
                    head_tag_point = Some(tag_end);
                }
                TagName::Meta if has_encoding_attributes(&buffer[tag_start..tag_end]) => {
                    return Some(tag_end)
                }
                _ => {
                    // Skip unknown tags, continue scanning
                }
            }
            offset += tag_end;
        }
        head_tag_point
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TagName {
    Meta,
    Head,
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
        let tag_close: usize = rest.iter().position(|&b| b == b'>')?;
        let tag_end = tag_start + tag_close + 2;

        // extract tag name
        let name_end = rest
            .iter()
            .position(|&b| b.is_ascii_whitespace() || b == b'>')?;

        let tag_name = &rest[..name_end];
        let relevant_tag_name = if compare_case_insensitive(tag_name, b"meta") {
            Some(TagName::Meta)
        } else if compare_case_insensitive(tag_name, b"head") {
            Some(TagName::Head)
        } else {
            None
        };

        if let Some(relevant_tag_name) = relevant_tag_name {
            return Some((relevant_tag_name, tag_start, tag_end));
        }

        i = tag_end;
    }

    None
}

fn has_encoding_attributes(buffer: &[u8]) -> bool {
    find_case_insensitive(buffer, b"charset=") || find_case_insensitive(buffer, b"http-equiv=")
}

fn compare_case_insensitive(a: &[u8], b_lower_case: &[u8]) -> bool {
    if a.len() != b_lower_case.len() {
        return false;
    }
    a.iter()
        .zip(b_lower_case)
        .all(|(a, &b_lower_case)| a.to_ascii_lowercase() == b_lower_case)
}

fn find_case_insensitive(input: &[u8], pattern: &[u8]) -> bool {
    input
        .windows(pattern.len())
        .any(|window| compare_case_insensitive(window, pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_with_charset() {
        test_injector(
            "<html><head><meta charset='...'></head></html>",
            "<html><head><meta charset='...'>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn meta_with_http_equiv() {
        test_injector(
            "<html><head><meta http-equiv='...'></head></html>",
            "<html><head><meta http-equiv='...'>{{SNIPPET}}</head></html>",
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
    fn head_in_tag_attribute() {
        test_injector(
            "<html><title foo='<head></head>' /><head></head></html>",
            "<html><title foo='<head></head>' /><head>{{SNIPPET}}</head></html>",
        );
    }

    #[test]
    fn no_head_or_meta() {
        test_injector("<html></html>", "<html></html>");
    }

    #[test]
    fn casing() {
        test_injector("<HeAd></head>", "<HeAd>{{SNIPPET}}</head>");
        test_injector("<Meta charset=''> ", "<Meta charset=''>{{SNIPPET}} ");
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
                "<html><head></head><script></script><meta charset=''></html>",
                "<html><head></head><script></script><meta charset=''>{{SNIPPET}}</html>",
            );
        }
    }

    fn test_injector(input: &str, expected: &str) {
        let mut input_bytes = input.as_bytes().to_vec();
        if let Some(index) = InjectionPointLocator::find(&input_bytes) {
            input_bytes.splice(index..index, b"{{SNIPPET}}".iter().copied());
        }
        let result = String::from_utf8(input_bytes).expect("Valid UTF-8");
        assert_eq!(result, expected);
    }
}
