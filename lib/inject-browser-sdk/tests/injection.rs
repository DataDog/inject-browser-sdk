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
//! produce (`harness/fixtures.rs`), the markup fuzzing once found handled wrongly, reduced and
//! written the same way, and random markup, also placed around the end of the head window. No
//! document is stored as a file: the fuzz targets are seeded from these too, see
//! [`write_fuzz_seeds`].
//!
//! Random chunkings and documents are seeded: `SEED=<n>` reproduces a failing run, and
//! `CHUNK_SIZE=<n>` splits every document into chunks of that size instead.

mod harness;

use std::collections::BTreeMap;
use std::fs;

use harness::input::{self, Run, SNIPPET};
use harness::oracle::{self, Exception, WINDOW};
use inject_browser_sdk::injector::InjectionStrategy;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

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

/// Runs every check on `document`, under every strategy, and returns the run of each. `exhaustive`
/// adds every split into two chunks, for documents short enough for that to stay cheap.
fn check(
    document: &[u8],
    rng: &mut StdRng,
    exhaustive: bool,
    stats: &mut Stats,
) -> Vec<(InjectionStrategy, Run)> {
    return input::STRATEGIES
        .iter()
        .map(|&strategy| {
            return (
                strategy,
                check_strategy(strategy, document, rng, exhaustive, stats),
            );
        })
        .collect();
}

fn check_strategy(
    strategy: InjectionStrategy,
    document: &[u8],
    rng: &mut StdRng,
    exhaustive: bool,
    stats: &mut Stats,
) -> Run {
    let single = input::run(strategy, &[document]);
    stats.documents += 1;
    if let Some(exception) = input::check_oracle(strategy, document, &single) {
        *stats
            .exceptions
            .entry(format!("{strategy:?}/{exception:?}"))
            .or_default() += 1;
    }

    let length = document.len();
    if let Ok(size) = std::env::var("CHUNK_SIZE") {
        let size: usize = size.parse().expect("CHUNK_SIZE is a number");
        let points: Vec<usize> = (size..length).step_by(size.max(1)).collect();
        input::check_chunking(strategy, document, &single, &points);
        return single;
    }

    for _ in 0..3 {
        let mut points: Vec<usize> = (0..rng.random_range(1..=8usize))
            .map(|_| return rng.random_range(0..=length))
            .collect();
        points.sort_unstable();
        input::check_chunking(strategy, document, &single, &points);
        input::check_oracle_chunking(document, &points);
    }
    // One byte at a time, the worst case for state carried across chunks.
    if length < 8 * WINDOW {
        let points: Vec<usize> = (1..length).collect();
        input::check_chunking(strategy, document, &single, &points);
    }
    if exhaustive && length < 2 * WINDOW {
        for point in 0..=length {
            input::check_chunking(strategy, document, &single, &[point]);
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

/// Real-world shaped documents, each written as the output it must produce under every strategy:
/// the oracle's exceptions let some mismatches through, the fixtures none.
#[test]
fn fixtures() {
    check_fixtures(&harness::fixtures::fixtures(), "fixtures");
}

/// Runs every check on each of `fixtures`, which must produce its expected output under every
/// strategy.
fn check_fixtures(fixtures: &[harness::fixtures::Fixture], name: &str) {
    let mut rng = StdRng::seed_from_u64(seed());
    let mut stats = Stats::default();
    for fixture in fixtures {
        for (strategy, run) in check(&fixture.document(), &mut rng, true, &mut stats) {
            assert_eq!(
                String::from_utf8_lossy(&run.output),
                String::from_utf8_lossy(&fixture.expected(strategy)),
                "{} under {strategy:?}",
                fixture.name
            );
        }
    }
    stats.report(name);
}

/// The markup fuzzing once found handled wrongly, reduced to the construct at fault.
#[test]
fn regressions() {
    check_fixtures(&harness::fixtures::regressions(), "regressions");
}

/// Writes every fixture and regression, one `.html` file for each strategy, to the directory
/// `FUZZ_SEEDS_DIR` names, for the fuzz targets to start from; does nothing without it:
///
/// ```text
/// FUZZ_SEEDS_DIR=fuzz/seeds cargo test -p inject-browser-sdk --test injection write_fuzz_seeds
/// ```
#[test]
fn write_fuzz_seeds() {
    let Some(dir) = std::env::var_os("FUZZ_SEEDS_DIR") else {
        return;
    };
    fs::create_dir_all(&dir).unwrap();
    let fixtures = harness::fixtures::fixtures();
    let regressions = harness::fixtures::regressions();
    // Every target spends the first byte of its input on how it runs, and 0 and 1 leave the
    // document whole under each strategy: one seed for each, the document after it.
    for fixture in fixtures.iter().chain(&regressions) {
        for selector in [0u8, 1] {
            let path = std::path::Path::new(&dir).join(format!("{}-{selector}.html", fixture.name));
            fs::write(path, [[selector].as_slice(), &fixture.document()].concat()).unwrap();
        }
    }
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
        "<meta charset=bogus>",
        "<meta http-equiv=X-UA-Compatible content=IE=edge>",
        "<meta http-equiv=Content-Security-Policy content=\"script-src 'self'\">",
        "<meta http-equiv=content-security-policy-report-only content=x>",
        "<META HTTP-EQUIV='content-type' CONTENT='text/html; charset=latin1'>",
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

/// Heads made of long runs of `<meta>` and `<title>` tags, which the walk follows past the window up
/// to its limit, ended or not by text or another tag, with the metas the snippet stays behind
/// sprinkled in.
#[test]
fn random_meta_runs() {
    const RUN: &[&str] = &[
        "<meta name=description content='a long enough description of the page'>",
        "<meta property=og:image content=https://cdn.example.com/a.jpg>",
        "<meta name=dd-trace-id content=123>",
        "<title>t</title>",
        "\n  ",
        "<!-- comment -->",
        "</p>",
    ];
    const RELEVANT: &[&str] = &[
        "<meta charset=utf-8>",
        "<meta http-equiv=X-UA-Compatible content=IE=edge>",
        "<meta http-equiv=Content-Security-Policy content=x>",
        "<meta http-equiv=content-type content='text/html; charset=utf-8'>",
        "<meta charset=bogus>",
    ];
    const ENDS: &[&str] = &[
        "<link rel=icon>",
        "<script>x</script>",
        "text",
        "</head>",
        "<body>",
        "<style>",
        "<title>",
        "<meta name=unfinished",
        "<!-- unfinished",
    ];
    let mut rng = StdRng::seed_from_u64(seed());
    let mut stats = Stats::default();
    for _ in 0..300 {
        let mut document =
            String::from(["<html><head>", "<!DOCTYPE html><HEAD>"][rng.random_range(0..2)]);
        if rng.random_bool(0.2) {
            document.insert_str(
                0,
                &format!("<!-- {} -->", "c".repeat(rng.random_range(800..1000))),
            );
        }
        let length = rng.random_range(500..2 * oracle::WALK_LIMIT);
        while document.len() < length {
            let fragment = match rng.random_bool(0.05) {
                true => RELEVANT[rng.random_range(0..RELEVANT.len())],
                false => RUN[rng.random_range(0..RUN.len())],
            };
            document.push_str(fragment);
            if rng.random_bool(0.002) {
                document.push_str(ENDS[rng.random_range(0..ENDS.len())]);
            }
        }
        if rng.random_bool(0.5) {
            document.push_str(ENDS[rng.random_range(0..ENDS.len())]);
            document.push_str(RELEVANT[rng.random_range(0..RELEVANT.len())]);
        }
        check(document.as_bytes(), &mut rng, false, &mut stats);
    }
    stats.report("random meta runs");
}

/// The exceptions are deliberate, and each one is exercised.
#[test]
fn oracle_exceptions() {
    let check_with = |strategy: InjectionStrategy, document: &str| {
        let ours = input::run(strategy, &[document.as_bytes()]).injected_at;
        let observed = oracle::observe(&[document.as_bytes()]).expect("unambiguous");
        let expected = oracle::injection_point(strategy, &observed);
        return (
            ours,
            expected,
            oracle::exception(strategy, document.as_bytes(), &observed, ours, expected),
        );
    };
    let check = |document: &str| return check_with(InjectionStrategy::HeadOpenTag, document);
    let close = |document: &str| return check_with(InjectionStrategy::HeadCloseTag, document);

    // Before `</head>`: a head whose start tag is omitted, which lol_html does not see.
    let (ours, expected, exception) = close("<html><meta charset=utf-8><title>t</title></head>");
    assert_eq!((ours, expected), (Some(42), None));
    assert_eq!(exception, Some(Exception::HeadStartOmitted));
    // Out of foreign content, a CDATA section is a bogus comment, for both.
    let (ours, expected, _) = close("<html><head><![CDATA[x]]></head>");
    assert_eq!((ours, expected), (Some(25), Some(25)));
    // Where the head closes without one: body content, text, for both.
    let (ours, expected, _) = close("<!DOCTYPE html><head><title>t</title><img src=a></head>");
    assert_eq!((ours, expected), (Some(37), Some(37)));
    let (ours, expected, _) = close("<html><head><title>t</title>\n Warning</head>");
    assert_eq!((ours, expected), (Some(30), Some(30)));
    // Past an error printed ahead of the document, before its `<html>` tag.
    let (ours, expected, _) = close("<br><b>Notice</b><br><!DOCTYPE html><html><head></head>");
    assert_eq!((ours, expected), (Some(36), Some(36)));
    // A `</body>` tag closes the head, which lol_html keeps open.
    let (ours, expected, exception) = close("<html><head><title>t</title></body></head>");
    assert_eq!((ours, expected), (Some(28), Some(35)));
    assert_eq!(exception, Some(Exception::EndTagClosingHead));
    // The end of the document closes it too.
    let (ours, expected, exception) = close("<!DOCTYPE html><title>t</title>");
    assert_eq!((ours, expected), (Some(31), None));
    assert_eq!(exception, Some(Exception::EndOfDocument));
    // Never in an AMP document.
    let (ours, expected, _) = close("<html amp><head><img></head>");
    assert_eq!((ours, expected), (None, None));
    // A head whose start tag is omitted, walked from past the `<html>` tag.
    let (ours, expected, exception) = check("<html><meta charset=utf-8><title>t</title><p>x");
    assert_eq!((ours, expected), (Some(26), None));
    assert_eq!(exception, Some(Exception::HeadStartOmitted));

    let (ours, expected, exception) =
        check("<head><meta charset=utf-8><![CDATA[x]]><meta charset=utf-8>");
    assert!(ours < expected);
    assert_eq!(exception, Some(Exception::CdataSection));

    let (ours, expected, exception) = check("<head><script><meta charset=utf-8></script></head>");
    assert!(ours > expected);
    assert_eq!(exception, Some(Exception::EncodingDeclaration));

    let (ours, expected, exception) = check("<head><meta charset=utf-8></br><meta charset=utf-8>");
    assert!(ours < expected);
    assert_eq!(exception, Some(Exception::EndTagClosingHead));

    // A body content tag before the `<head>` tag, which the browser ignores then: the snippet goes
    // where the head it opened closes, which lol_html does not see.
    let (ours, expected, exception) = check("<html><div></div><head><meta name=a>");
    assert_eq!((ours, expected), (Some(6), None));
    assert_eq!(exception, Some(Exception::HeadStartOmitted));
    // Without HTML evidence first, neither injects.
    let (ours, expected, _) = check("<div></div><head><meta name=a>");
    assert_eq!((ours, expected), (None, None));

    let (ours, expected, exception) = check("<head></head><body><meta charset=utf-8>");
    assert_eq!((ours, expected), (None, Some(6)));
    assert_eq!(exception, Some(Exception::EncodingDeclaration));

    // An empty unquoted attribute value on a raw text element, which lol_html reads past.
    let (ours, expected, exception) = close("<html><head><script c=><script </script>x</head>");
    assert_eq!((ours, expected), (Some(40), Some(41)));
    assert_eq!(exception, Some(Exception::EmptyAttributeValue));
    // A `</head>` tag longer than the injector holds back.
    let long = format!("<html><head></head {}>", "a".repeat(1024));
    let (ours, expected, exception) = close(&long);
    assert_eq!((ours, expected), (None, Some(12)));
    assert_eq!(exception, Some(Exception::LongEndTag));

    assert_eq!(
        oracle::observe(&[b"<select><xmp><script>x</script></select>"]),
        None
    );

    // Anything else is a mismatch.
    let observed = oracle::observe(&[b"<head><meta name=a>"]).unwrap();
    assert_eq!(
        oracle::exception(
            InjectionStrategy::HeadOpenTag,
            b"<head><meta name=a>",
            &observed,
            Some(6),
            Some(19)
        ),
        None
    );
    assert_eq!(SNIPPET, b"{{SNIPPET}}");
}
