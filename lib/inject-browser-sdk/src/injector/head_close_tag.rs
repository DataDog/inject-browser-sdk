// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Streaming locator for the `</head>` tag, used by
//! [`InjectionStrategy::HeadCloseTag`](super::InjectionStrategy::HeadCloseTag) and as the fallback
//! of [`InjectionStrategy::HeadOpenTagWithCloseTagFallback`](super::InjectionStrategy::HeadOpenTagWithCloseTagFallback).

/// The location of an injection point, as computed by the [`HeadCloseTagLocator`].
#[cfg_attr(test, derive(Debug))]
pub(super) enum Location {
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
pub(super) struct HeadCloseTagLocator {
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
    pub(super) fn new() -> Self {
        return Self {
            index: 0,
            pending_length: 0,
        };
    }

    pub(super) fn scan(&mut self, chunk: &[u8]) -> Location {
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
    use alloc::vec::Vec;
    use pretty_assertions::assert_matches;

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
