// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Corpus tests of the injector, after lol_html's own (cloudflare/lol-html `tests/`): every
//! document runs as a single chunk and as random chunkings, each run checked against the single
//! one and against lol_html as an oracle for where the snippet goes.
//!
//! The documents are generated: real-world shaped fixtures written as the output they must
//! produce (`harness/fixtures.rs`), and random markup, also placed around the end of the head
//! window. Only the inputs fuzzing once found handled wrongly are kept as files.
//!
//! Random chunkings and documents are seeded: `SEED=<n>` reproduces a failing run, and
//! `CHUNK_SIZE=<n>` splits every document into chunks of that size instead.

mod harness;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use harness::input::{self, Run, SNIPPET};
use harness::oracle::{self, Exception, WINDOW};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

fn data_dir() -> PathBuf {
    return Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
}

fn seed() -> u64 {
    let seed = match std::env::var("SEED") {
        Ok(seed) => seed.parse().expect("SEED is a number"),
        Err(_) => rand::rng().random(),
    };
    eprintln!("SEED={seed}");
    return seed;
}

/// Exceptions applied so far, by kind, so a test can report how often lol_html was overruled.
#[derive(Default)]
struct Stats {
    documents: usize,
    exceptions: BTreeMap<String, usize>,
}

impl Stats {
    fn report(&self, name: &str) {
        eprintln!(
            "{name}: {} documents, exceptions {:?}",
            self.documents, self.exceptions
        );
    }
}

/// Runs every check on `document`. `exhaustive` adds every split into two chunks, for documents
/// short enough for that to stay cheap.
fn check(document: &[u8], rng: &mut StdRng, exhaustive: bool, stats: &mut Stats) -> Run {
    let single = input::run(&[document]);
    stats.documents += 1;
    if let Some(exception) = input::check_oracle(document, &single) {
        *stats
            .exceptions
            .entry(format!("{exception:?}"))
            .or_default() += 1;
    }

    let length = document.len();
    if let Ok(size) = std::env::var("CHUNK_SIZE") {
        let size: usize = size.parse().expect("CHUNK_SIZE is a number");
        let points: Vec<usize> = (size..length).step_by(size.max(1)).collect();
        input::check_chunking(document, &single, &points);
        return single;
    }

    for _ in 0..3 {
        let mut points: Vec<usize> = (0..rng.random_range(1..=8usize))
            .map(|_| return rng.random_range(0..=length))
            .collect();
        points.sort_unstable();
        input::check_chunking(document, &single, &points);
        input::check_oracle_chunking(document, &points);
    }
    // One byte at a time, the worst case for state carried across chunks.
    if length < 8 * WINDOW {
        let points: Vec<usize> = (1..length).collect();
        input::check_chunking(document, &single, &points);
    }
    if exhaustive && length < 2 * WINDOW {
        for point in 0..=length {
            input::check_chunking(document, &single, &[point]);
        }
    }
    return single;
}

/// The ways a piece of markup is turned into documents: on its own, and placed before, within and
/// after a head, at the start of the document and around the window end.
fn wrappings(input: &str) -> Vec<String> {
    let padding = |prefix: &str| return " ".repeat(WINDOW.saturating_sub(prefix.len() + 8));
    let straddling = "<html><head>";
    return vec![
        String::from(input),
        format!("<html><head>{input}"),
        format!("<!DOCTYPE html><html><head><meta charset=utf-8>{input}<meta name=a></head><body>"),
        format!("{input}<head><meta name=a>"),
        format!(
            "<html><!-- {} -->{input}<head><meta name=a>",
            "c".repeat(WINDOW)
        ),
        format!(
            "{straddling}{}{input}<meta name=a></head>",
            padding(straddling)
        ),
    ];
}

/// Real-world shaped documents, each written as the output it must produce.
#[test]
fn fixtures() {
    let mut rng = StdRng::seed_from_u64(seed());
    let mut stats = Stats::default();
    for fixture in harness::fixtures::fixtures() {
        let run = check(&fixture.document(), &mut rng, true, &mut stats);
        assert_eq!(
            String::from_utf8_lossy(&run.output),
            String::from_utf8_lossy(&fixture.template),
            "{}",
            fixture.name
        );
    }
    stats.report("fixtures");
}

/// Inputs fuzzing found to be handled wrongly once, kept as they were found.
#[test]
fn regressions() {
    let mut rng = StdRng::seed_from_u64(seed());
    let mut stats = Stats::default();
    for entry in fs::read_dir(data_dir().join("regression")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| return extension == "html")
        {
            check(&fs::read(&path).unwrap(), &mut rng, true, &mut stats);
        }
    }
    stats.report("regressions");
}

/// Documents made of random markup fragments, in the spirit of the fuzz targets, but small and
/// many enough to run with every `cargo test`.
#[test]
fn random_documents() {
    const FRAGMENTS: &[&str] = &[
        "<head>",
        "<HEAD lang=en>",
        "</head>",
        "<html>",
        "<meta charset=utf-8>",
        "<meta name=a>",
        "<META NAME='b'>",
        "<title>",
        "</title>",
        "<script>",
        "</script>",
        "<style>",
        "</style>",
        "<noscript>",
        "</noscript>",
        "<template>",
        "</template>",
        "<!--",
        "-->",
        "--!>",
        "<!DOCTYPE html>",
        "<link rel=icon>",
        "<base href=/>",
        "<div>",
        "<p>",
        "<body>",
        "<textarea>",
        "</textarea>",
        "<a href='",
        "'>",
        "\"",
        "'",
        ">",
        "<",
        "</",
        "/>",
        "=",
        " ",
        "\n",
        "text",
        "<!x>",
        "<?x>",
        "<![CDATA[",
        "]]>",
        "<xmp>",
        "<plaintext>",
        "<frameset>",
        "&amp;",
        "😊",
    ];
    let mut rng = StdRng::seed_from_u64(seed());
    let mut stats = Stats::default();
    for _ in 0..3000 {
        let mut document = String::new();
        if rng.random_bool(0.3) {
            document.push_str(&"x".repeat(rng.random_range(900..1100)));
        }
        for _ in 0..rng.random_range(0..40) {
            document.push_str(FRAGMENTS[rng.random_range(0..FRAGMENTS.len())]);
        }
        if rng.random_bool(0.2) {
            for wrapped in wrappings(&document) {
                check(wrapped.as_bytes(), &mut rng, false, &mut stats);
            }
        } else {
            check(document.as_bytes(), &mut rng, false, &mut stats);
        }
    }
    stats.report("random documents");
}

/// The exceptions are deliberate, and each one is exercised.
#[test]
fn oracle_exceptions() {
    let check = |document: &str| {
        let ours = input::run(&[document.as_bytes()]).injected_at;
        let observed = oracle::observe(&[document.as_bytes()]).expect("unambiguous");
        let expected = oracle::injection_point(&observed);
        return (
            ours,
            expected,
            oracle::exception(document.as_bytes(), &observed, ours, expected),
        );
    };

    let (ours, expected, exception) = check("<head><meta name=a><![CDATA[x]]><meta name=b>");
    assert!(ours < expected);
    assert_eq!(exception, Some(Exception::CdataSection));

    let (ours, expected, exception) = check("<head><script><meta charset=utf-8></script></head>");
    assert!(ours > expected);
    assert_eq!(exception, Some(Exception::EncodingDeclaration));

    let (ours, expected, exception) = check("<head><meta name=a></br><meta name=b>");
    assert!(ours < expected);
    assert_eq!(exception, Some(Exception::EndTagClosingHead));

    let (ours, expected, exception) = check("<script c=><head></script><head><meta name=a>");
    assert_ne!(ours, expected);
    assert_eq!(exception, Some(Exception::RawTextEmptyAttributeValue));

    let (ours, expected, exception) = check("<head></head><body><meta charset=utf-8>");
    assert_eq!((ours, expected), (None, Some(6)));
    assert_eq!(exception, Some(Exception::EncodingDeclaration));

    assert_eq!(
        oracle::observe(&[b"<select><xmp><script>x</script></select>"]),
        None
    );

    // Anything else is a mismatch.
    let observed = oracle::observe(&[b"<head><meta name=a>"]).unwrap();
    assert_eq!(
        oracle::exception(b"<head><meta name=a>", &observed, Some(6), Some(19)),
        None
    );
    assert_eq!(SNIPPET, b"{{SNIPPET}}");
}
