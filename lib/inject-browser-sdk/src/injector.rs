// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.
use crate::injection_point_locator::InjectionPointLocator;

/// Stateful object used to inject the provided snippet within a streamed HTML document.
pub struct Injector<'snippet> {
    /// Snippet to inject.
    snippet: &'snippet [u8],

    /// True if we attempted to inject during `write` API; used to decide if `end` API should try.
    tried_injected: bool,

    /// True if the snippet was successfully injected; used to decide if `end` API should add extra padding space
    injected: bool,

    /// Buffers up to 1024 bytes before attempting injection.
    /// According to the spec (https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element),
    /// meta charset tags must appear within the first 1024 bytes or they will be ignored by browsers.
    /// The `<head>` tag is also typically found within this range.
    buffer: Vec<u8>,

    /// If we don't find any injection point, we need to pad the document with spaces, so the
    /// document has the same size as if the snippet was successfully injected.
    padding_spaces: Vec<u8>,
}

impl<'snippet> Injector<'snippet> {
    pub fn new(snippet: &'snippet [u8]) -> Self {
        Self {
            snippet,
            injected: false,
            tried_injected: false,
            padding_spaces: Vec::new(),
            buffer: Vec::new(),
        }
    }

    pub fn write<'a>(&'a mut self, chunk: &'a [u8]) -> Result<'a> {
        if self.tried_injected {
            return self.create_result(&[chunk]);
        }

        self.buffer.extend_from_slice(chunk);

        if self.buffer.len() >= 1024 {
            return self.try_inject();
        }

        self.create_result(&[])
    }

    pub fn end(&mut self) -> Result<'_> {
        if self.injected {
            return self.create_result(&[]);
        }

        self.padding_spaces = vec![b' '; self.snippet.len()];

        if !self.tried_injected {
            return self.try_inject();
        }

        self.create_result(&[&self.padding_spaces])
    }

    fn try_inject<'a>(&'a mut self) -> Result<'a> {
        let needed = self.buffer.len().min(1024);
        let slice = &self.buffer[..needed];
        self.tried_injected = true;

        if let Some(point) = InjectionPointLocator::find(slice) {
            self.injected = true;
            return self.create_result(&[&slice[..point], self.snippet, &self.buffer[point..]]);
        }

        self.create_result(&[&self.buffer, &self.padding_spaces])
    }

    fn create_result<'a>(&'a self, slices: &[&'a [u8]]) -> Result<'a> {
        let mut length = 0;
        let mut non_empty_slices = slices.iter().filter(|slice| !slice.is_empty());

        Result {
            slices: std::array::from_fn(|_| match non_empty_slices.next() {
                Some(slice) => {
                    length += 1;
                    BytesSlice {
                        bytes: slice,
                        from_incoming_chunk: false,
                    }
                }
                None => BytesSlice::default(),
            }),
            length,
            injected: self.injected,
        }
    }
}

#[derive(Default, Copy, Clone)]
pub struct BytesSlice<'a> {
    pub bytes: &'a [u8],
    pub from_incoming_chunk: bool,
}

/// Stack allocated "array" of at most 4 references to byte slices.
pub struct Result<'a> {
    pub slices: [BytesSlice<'a>; 4],
    pub length: usize,
    pub injected: bool,
}

impl<'a> Result<'a> {
    #[cfg(test)]
    fn iter(&self) -> impl Iterator<Item = BytesSlice<'_>> {
        return self.slices[..self.length].iter().copied();
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rand::{distributions::Uniform, prelude::Distribution, seq::SliceRandom, Rng};

    use super::*;

    #[test]
    fn basic() {
        test_injector(
            ["<head", ">abc</he", "ad>def"],
            "<head><snippet>abc</head>def",
        );
    }

    #[test]
    fn head_after_1_kb() {
        let input = "A".repeat(1024) + "<head>def";
        let expected = input.clone() + "         ";

        test_injector([input], &expected);
    }

    #[test]
    fn no_head_or_meta() {
        test_injector(["<html></html>"], "<html></html>         ");
    }

    #[test]
    fn empty() {
        test_injector::<&str>([], "         ");
        test_injector([""], "         ");
        test_injector(["", ""], "         ");
    }

    #[test]
    fn fuzzy() {
        struct Part {
            string: &'static str,
            has_head: bool,
        }

        impl Part {
            fn new(string: &'static str, has_head: bool) -> Self {
                Self { string, has_head }
            }
        }

        let parts: [Part; 16] = [
            Part::new("<head>", true),
            Part::new("<head >", true),
            Part::new("<HeAd>", true),
            Part::new("<HEAD>", true),
            Part::new("<h ead>", false),
            Part::new("<he ad>", false),
            Part::new("<header>", false),
            Part::new("h>", false),
            Part::new("/>", false),
            Part::new(">", false),
            Part::new(" ", false),
            Part::new("foo", false),
            Part::new("bar", false),
            Part::new("&nbsp;", false),
            Part::new("😊", false),
            Part::new("网络", false),
        ];

        let mut rng = rand::thread_rng();

        for _ in 0..1000 {
            // Build a random input from 'parts'
            let mut input = String::new();
            let mut expected = String::new();
            let mut has_head = false;

            let parts_count = Uniform::new(0, 20).sample(&mut rng);
            for part in parts.choose_multiple(&mut rng, parts_count) {
                input.push_str(part.string);
                expected.push_str(part.string);

                if !has_head && part.has_head {
                    expected.push_str("<snippet>");
                    has_head = true;
                }
            }
            if !has_head {
                expected.push_str("         ");
            }

            // Split `input` into chunks of random sizes. Because chunks might not end at a
            // character boundary, we need to use &[u8] chunks instead of &str.
            let chunks = rng
                .clone()
                .sample_iter(Uniform::new(0, input.len() + 1))
                .scan(input.as_bytes(), |input, chunk_size| {
                    if input.is_empty() {
                        None
                    } else {
                        let chunk_size = chunk_size.min(input.len());
                        let chunk = &input[..chunk_size];
                        *input = &input[chunk_size..];
                        Some(chunk)
                    }
                });

            test_injector(chunks, expected.as_str());
        }
    }

    fn test_injector<T: AsRef<[u8]> + std::fmt::Debug>(
        input_chunks: impl IntoIterator<Item = T>,
        expected: &str,
    ) {
        let snippet = b"<snippet>";
        let mut injector = Injector::new(snippet);

        let input_chunks: Vec<T> = input_chunks.into_iter().collect();

        let mut output_slices = Vec::new();
        let mut injected = false;

        for incoming_chunk in input_chunks
            .iter()
            .map(|chunk| return Some(chunk.as_ref()))
            .chain([None])
        {
            let result = match incoming_chunk {
                Some(chunk) => injector.write(chunk),
                None => injector.end(),
            };
            for slice in result.iter() {
                {
                    // Make sure the slice has a correct from_incoming_chunk flag
                    let expected_from_incoming_chunk_value = match incoming_chunk {
                        Some(chunk) => {
                            let pointer = slice.bytes.as_ptr() as usize;
                            let min = chunk.as_ptr() as usize;
                            let max = min + chunk.len();
                            min <= pointer && pointer < max
                        }
                        None => false,
                    };

                    assert_eq!(
                        slice.from_incoming_chunk,
                        expected_from_incoming_chunk_value
                    );
                }

                if !injected {
                    // Look for the snippet in the slice
                    injected = slice.bytes.windows(slice.bytes.len()).any(|w| w == snippet);
                }
                output_slices.extend_from_slice(slice.bytes);
            }
            assert_eq!(result.injected, injected);
        }

        assert_eq!(
            String::from_utf8(output_slices).unwrap(),
            expected,
            "with chunks {:?}",
            input_chunks
        );
    }
}
