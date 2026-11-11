// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Where the snippet goes in a streamed HTML document, as an offset into its body, for servers that
//! keep their own buffers.
//!
//! A [`Scanner`] never hands bytes back. It is fed the body as it comes and tells, after each
//! slice, what the server may do with what it holds: send it, keep it, or splice the snippet in at
//! a given offset. A server can then keep the body in its own buffers, or as file ranges it sends
//! with `sendfile`, and hold its headers until the decision, so that `Content-Length` is exact.

use crate::injector::{InjectionStrategy, Injector, Result};

/// What a [`Scanner`] says of the body fed to it so far. Offsets count from the first body byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Undecided: the bytes from `from` on must be held, those before it may be sent. `from` never
    /// moves backwards, and the snippet never goes before it.
    Hold { from: u64 },
    /// Undecided, but nothing has to be held: the snippet would go in data yet to be fed.
    Searching,
    /// The snippet goes at this offset. Final.
    InjectAt(u64),
    /// The snippet never goes in. Final.
    Never,
}

/// Stands for the snippet in the [`Injector`] the scanner decides with: the slice of a result
/// pointing at it marks where the snippet goes.
static MARK: [u8; 1] = [0];

/// Decides where the snippet goes in an HTML document fed in slices, by the rules the [`Injector`]
/// documents for the [`InjectionStrategy`] given, and reports it as an offset.
///
/// It decides with an [`Injector`], so it copies no more than one holds back: the head window with
/// [`InjectionStrategy::HeadOpenTag`], a tag a slice cuts short with
/// [`InjectionStrategy::HeadCloseTag`], and, for a point within the first kilobyte with no encoding
/// declaration before it, the bytes from the point until that kilobyte is complete. The server still holds the bytes it was fed, and sends them
/// itself. Past the decision, nothing is looked at.
pub struct Scanner {
    injector: Injector<'static>,
    /// How many bytes were fed so far.
    fed: u64,
    /// How many of them the injector handed back so far: the snippet goes after them, if it goes in
    /// with the call that hands it back.
    emitted: u64,
    decision: Decision,
}

impl Default for Scanner {
    fn default() -> Self {
        return Self::new();
    }
}

impl Scanner {
    /// A scanner with the default strategy, [`InjectionStrategy::HeadCloseTag`].
    pub fn new() -> Self {
        return Self::with_strategy(InjectionStrategy::default());
    }

    pub fn with_strategy(strategy: InjectionStrategy) -> Self {
        return Self {
            injector: Injector::with_strategy(&MARK, strategy),
            fed: 0,
            emitted: 0,
            decision: Decision::Hold { from: 0 },
        };
    }

    /// Feeds the next slice of the body, and returns the decision on what was fed so far.
    ///
    /// A final decision is returned again by every later call, and the slices that follow it are
    /// not looked at.
    pub fn feed(&mut self, slice: &[u8]) -> Decision {
        self.fed += slice.len() as u64;
        if self.is_final() {
            return self.decision;
        }
        let result = self.injector.write(slice);
        let injected_at = injection_point(&result);
        let emitted = emitted_length(&result);
        return self.decide(injected_at, emitted);
    }

    /// Ends the body: the decision is final.
    pub fn finish(&mut self) -> Decision {
        if self.is_final() {
            return self.decision;
        }
        let result = self.injector.end_without_padding();
        let injected_at = injection_point(&result);
        let emitted = emitted_length(&result);
        self.decide(injected_at, emitted);
        if !self.is_final() {
            self.decision = Decision::Never;
        }
        return self.decision;
    }

    /// Gives up what is held, for a server that has to send it now, a page streamed in pieces that
    /// waits on more data for instance: the returned decision is never [`Decision::Hold`], and never
    /// injects within what was fed. See [`Injector::flush`] for what is given up with it.
    pub fn flush(&mut self) -> Decision {
        if self.is_final() {
            return self.decision;
        }
        let result = self.injector.flush();
        let emitted = emitted_length(&result);
        return self.decide(None, emitted);
    }

    /// The decision on what was fed so far, as the last call returned it.
    pub fn decision(&self) -> Decision {
        return self.decision;
    }

    /// How many bytes were fed so far.
    pub fn fed(&self) -> u64 {
        return self.fed;
    }

    fn is_final(&self) -> bool {
        return matches!(self.decision, Decision::InjectAt(_) | Decision::Never);
    }

    /// Records what the last injector call says: `injected_at`, how many of the bytes it handed back
    /// come before the snippet, if it went in, and `emitted`, how many it handed back in all.
    fn decide(&mut self, injected_at: Option<u64>, emitted: u64) -> Decision {
        let before = self.emitted;
        self.emitted += emitted;
        self.decision = match injected_at {
            Some(point) => Decision::InjectAt(before + point),
            None if !self.injector.is_pending() => Decision::Never,
            None => match self.injector.held_back() as u64 {
                0 => Decision::Searching,
                held => Decision::Hold {
                    from: self.fed - held,
                },
            },
        };
        return self.decision;
    }
}

/// How many bytes of the document `result` hands back before the snippet, if it carries it.
fn injection_point(result: &Result<'_>) -> Option<u64> {
    let mut before = 0;
    for slice in &result.slices[..result.length] {
        if slice.bytes.as_ptr() == MARK.as_ptr() {
            return Some(before);
        }
        before += slice.bytes.len() as u64;
    }
    return None;
}

/// How many bytes of the document `result` hands back, the snippet aside.
fn emitted_length(result: &Result<'_>) -> u64 {
    return result.slices[..result.length]
        .iter()
        .filter(|slice| return slice.bytes.as_ptr() != MARK.as_ptr())
        .map(|slice| return slice.bytes.len() as u64)
        .sum();
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn scan(strategy: InjectionStrategy, chunks: &[&[u8]]) -> Decision {
        let mut scanner = Scanner::with_strategy(strategy);
        for chunk in chunks {
            scanner.feed(chunk);
        }
        return scanner.finish();
    }

    #[test]
    fn decides_as_the_injector_injects() {
        let document =
            b"<!DOCTYPE html><html><head><meta charset=utf-8><title>t</title></head><body>";
        let close = document
            .windows(7)
            .position(|w| return w == b"</head>")
            .unwrap() as u64;
        let open = document
            .windows(7)
            .position(|w| return w == b"<title>")
            .unwrap() as u64;
        for point in 0..=document.len() {
            let (first, second) = document.split_at(point);
            assert_eq!(
                scan(InjectionStrategy::HeadCloseTag, &[first, second]),
                Decision::InjectAt(close)
            );
            assert_eq!(
                scan(InjectionStrategy::HeadOpenTag, &[first, second]),
                Decision::InjectAt(open)
            );
        }
        assert_eq!(
            scan(InjectionStrategy::HeadCloseTag, &[b"{\"a\": \"</head>\"}"]),
            Decision::Never
        );
    }

    #[test]
    fn holds_only_what_the_injector_holds() {
        // Before `</head>`: nothing but a tag cut short.
        let mut scanner = Scanner::new();
        assert_eq!(
            scanner.feed(b"<html><head><meta charset=utf-8><title>t</title>"),
            Decision::Searching
        );
        assert_eq!(scanner.feed(b"</he"), Decision::Hold { from: 48 });
        assert_eq!(scanner.feed(b"ad>"), Decision::InjectAt(48));
        // Final: what follows is not looked at.
        assert_eq!(scanner.feed(b"<head></head>"), Decision::InjectAt(48));
        assert_eq!(scanner.finish(), Decision::InjectAt(48));
        assert_eq!(scanner.fed(), 68);

        // Within the first kilobyte without an encoding declaration before it, from the point
        // until that kilobyte is complete, or the body is: one may follow.
        let mut scanner = Scanner::new();
        assert_eq!(
            scanner.feed(b"<html><head><title>t</title></head>"),
            Decision::Hold { from: 28 }
        );
        assert_eq!(scanner.feed(b"<body>"), Decision::Hold { from: 28 });
        assert_eq!(scanner.finish(), Decision::InjectAt(28));

        // As early as possible: the head window, from the first byte.
        let mut scanner = Scanner::with_strategy(InjectionStrategy::HeadOpenTag);
        assert_eq!(scanner.feed(b"<html><head>"), Decision::Hold { from: 0 });
        assert_eq!(scanner.finish(), Decision::InjectAt(12));
    }

    #[test]
    fn flush_gives_up_what_is_held() {
        let mut scanner = Scanner::new();
        scanner.feed(b"<html><head></he");
        assert_eq!(scanner.flush(), Decision::Searching);
        assert_eq!(scanner.feed(b"ad>"), Decision::Never);

        let mut scanner = Scanner::with_strategy(InjectionStrategy::HeadOpenTag);
        scanner.feed(b"<html><head>");
        assert_eq!(scanner.flush(), Decision::Never);
    }
}
