// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Runs a document through the injector the way lol_html's own harness runs its cases: as a single
//! chunk, then split into chunks, and checks every run against the single one and against the
//! oracle.

use inject_browser_sdk::injector::Injector;

use super::oracle;

pub const SNIPPET: &[u8] = b"{{SNIPPET}}";

/// What a run of the injector produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The output, unpadded.
    pub output: Vec<u8>,
    /// Where in the document the snippet went, if it did.
    pub injected_at: Option<usize>,
}

/// Splits `document` at `points`, which must be sorted.
pub fn split<'a>(document: &'a [u8], points: &[usize]) -> Vec<&'a [u8]> {
    let mut chunks = Vec::with_capacity(points.len() + 1);
    let mut start = 0;
    for &point in points {
        chunks.push(&document[start..point]);
        start = point;
    }
    chunks.push(&document[start..]);
    return chunks;
}

/// Writes `chunks` to an injector and checks the contract of every result along the way: slices
/// flagged `from_incoming_chunk` point into the chunk just written and the others do not, every
/// byte comes out exactly once, and what is held back is reported. Panics on a violation.
pub fn run(chunks: &[&[u8]]) -> Run {
    let mut injector = Injector::new(SNIPPET);
    let mut output = Vec::new();
    let mut injected_at = None;
    let mut written = 0;

    for chunk in chunks.iter().map(|chunk| return Some(*chunk)).chain([None]) {
        let held_before = injector.held_back();
        let result = match chunk {
            Some(chunk) => injector.write(chunk),
            None => injector.end_without_padding(),
        };
        let mut emitted = 0;
        for slice in &result.slices[..result.length] {
            let in_chunk = chunk.is_some_and(|chunk| {
                let range = chunk.as_ptr_range();
                return range.contains(&slice.bytes.as_ptr());
            });
            assert_eq!(
                slice.from_incoming_chunk, in_chunk,
                "from_incoming_chunk flag"
            );
            if slice.bytes.as_ptr() == SNIPPET.as_ptr() {
                assert!(injected_at.is_none(), "injected twice");
                injected_at = Some(output.len());
            } else {
                emitted += slice.bytes.len();
            }
            output.extend_from_slice(slice.bytes);
        }
        assert_eq!(result.injected, injected_at.is_some(), "injected flag");
        let length = chunk.map_or(0, <[u8]>::len);
        assert_eq!(
            held_before + length,
            emitted + injector.held_back(),
            "held-back accounting"
        );
        written += length;
    }
    assert_eq!(injector.held_back(), 0, "data held back after the end");
    assert_eq!(
        output.len(),
        written + injected_at.map_or(0, |_| return SNIPPET.len())
    );

    return Run {
        output,
        injected_at,
    };
}

/// Replays how a server that queues the incoming chunks rebuilds them around the injection, as the
/// IIS module does, see the `chunk_rebuild` unit tests: it must serve what `run` output.
pub fn rebuild(chunks: &[&[u8]]) -> Vec<u8> {
    let mut injector = Injector::new(SNIPPET);
    for (index, chunk) in chunks.iter().enumerate() {
        let held = injector.held_back();
        let result = injector.write(chunk);
        if result.injected {
            let mut served: Vec<u8> = chunks[..index].concat();
            served.truncate(served.len() - held);
            for slice in &result.slices[..result.length] {
                served.extend_from_slice(slice.bytes);
            }
            for later in &chunks[index + 1..] {
                served.extend_from_slice(later);
            }
            return served;
        }
    }
    let held = injector.held_back();
    let result = injector.end_without_padding();
    let mut served: Vec<u8> = chunks.concat();
    if result.injected {
        served.truncate(served.len() - held);
        for slice in &result.slices[..result.length] {
            served.extend_from_slice(slice.bytes);
        }
    }
    return served;
}

/// Checks `document` split at `points` behaves exactly like `single`, the single chunk run.
pub fn check_chunking(document: &[u8], single: &Run, points: &[usize]) {
    let chunks = split(document, points);
    let chunked = run(&chunks);
    assert_eq!(
        &chunked, single,
        "split at {points:?}: output differs from the single chunk one"
    );
    assert_eq!(
        rebuild(&chunks),
        single.output,
        "split at {points:?}: the rebuilt chunks differ from the single chunk output"
    );
}

/// Checks the injector put the snippet where lol_html says it goes in `document`, short of a
/// documented [`oracle::Exception`]. Returns the exception, if one applied.
pub fn check_oracle(document: &[u8], single: &Run) -> Option<oracle::Exception> {
    if oracle::has_foreign_content(document) {
        return Some(oracle::Exception::ForeignContent);
    }
    let Some(observed) = oracle::observe(&[document]) else {
        return Some(oracle::Exception::Ambiguous);
    };
    let expected = oracle::injection_point(&observed);
    if single.injected_at == expected {
        return None;
    }
    let exception = oracle::exception(document, &observed, single.injected_at, expected);
    assert!(
        exception.is_some(),
        "injected at {:?}, lol_html says {:?}, in:\n{}",
        single.injected_at,
        expected,
        String::from_utf8_lossy(document)
    );
    return exception;
}

/// Checks lol_html, itself a streaming rewriter, sees the same thing whatever the chunking, so that
/// the oracle does not depend on it.
pub fn check_oracle_chunking(document: &[u8], points: &[usize]) {
    if oracle::has_foreign_content(document) {
        return;
    }
    assert_eq!(
        oracle::observe(&split(document, points)),
        oracle::observe(&[document]),
        "lol_html split at {points:?}"
    );
}
