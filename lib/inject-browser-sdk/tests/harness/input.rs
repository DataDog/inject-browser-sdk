// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! Runs a document through the injector the way lol_html's own harness runs its cases: as a single
//! chunk, then split into chunks, and checks every run against the single one and against the
//! oracles: lol_html's position ([`check_oracle`]) and html5ever's tree ([`check_tree`]).

use inject_browser_sdk::injector::{InjectionStrategy, Injector};

use super::oracle::{self, WALK_LIMIT, WINDOW};
use super::tree::{self, Handle, Tree};

pub const SNIPPET: &[u8] = b"{{SNIPPET}}";

/// The snippet [`check_tree`] injects: one that parses as an element, so that the tree tells where
/// it went.
pub const PROBE: &[u8] = b"<script data-dd-probe></script>";

/// The start tag of [`PROBE`], at whose end the parser creates the element.
const PROBE_START_TAG: &[u8] = b"<script data-dd-probe>";

/// What gives the probe away, in the tree's serialization or in a document.
const PROBE_MARK: &str = "dd-probe";

/// Every strategy, each document runs under all of them.
pub const STRATEGIES: [InjectionStrategy; 2] = [
    InjectionStrategy::HeadCloseTag,
    InjectionStrategy::HeadOpenTag,
];

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
pub fn run(strategy: InjectionStrategy, chunks: &[&[u8]]) -> Run {
    return run_snippet(SNIPPET, strategy, chunks);
}

/// [`run`] injecting [`PROBE`].
pub fn run_with_probe(strategy: InjectionStrategy, chunks: &[&[u8]]) -> Run {
    return run_snippet(PROBE, strategy, chunks);
}

fn run_snippet(snippet: &'static [u8], strategy: InjectionStrategy, chunks: &[&[u8]]) -> Run {
    let mut injector = Injector::with_strategy(snippet, strategy);
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
            if slice.bytes.as_ptr() == snippet.as_ptr() {
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
        written + injected_at.map_or(0, |_| return snippet.len())
    );

    return Run {
        output,
        injected_at,
    };
}

/// Replays how a server that queues the incoming chunks rebuilds them around the injection, as the
/// IIS module does, see the `chunk_rebuild` unit tests: it must serve what `run` output.
pub fn rebuild(strategy: InjectionStrategy, chunks: &[&[u8]]) -> Vec<u8> {
    let mut injector = Injector::with_strategy(SNIPPET, strategy);
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
pub fn check_chunking(
    strategy: InjectionStrategy,
    document: &[u8],
    single: &Run,
    points: &[usize],
) {
    let chunks = split(document, points);
    let chunked = run(strategy, &chunks);
    assert_eq!(
        &chunked, single,
        "{strategy:?} split at {points:?}: output differs from the single chunk one"
    );
    assert_eq!(
        rebuild(strategy, &chunks),
        single.output,
        "{strategy:?} split at {points:?}: the rebuilt chunks differ from the single chunk output"
    );
}

/// Checks the injector put the snippet where lol_html says it goes in `document`, short of a
/// documented [`oracle::Exception`]. Returns the exception, if one applied.
pub fn check_oracle(
    strategy: InjectionStrategy,
    document: &[u8],
    single: &Run,
) -> Option<oracle::Exception> {
    if oracle::has_foreign_content(document) {
        return Some(oracle::Exception::ForeignContent);
    }
    let Some(observed) = oracle::observe(&[document]) else {
        return Some(oracle::Exception::Ambiguous);
    };
    let expected = oracle::injection_point(strategy, &observed);
    if single.injected_at == expected {
        return None;
    }
    let exception = oracle::exception(strategy, document, &observed, single.injected_at, expected);
    assert!(
        exception.is_some(),
        "{strategy:?} injected at {:?}, lol_html says {:?}, in:\n{}",
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
    let (split, single) = (
        oracle::observe(&split(document, points)),
        oracle::observe(&[document]),
    );
    // Where a text node starts may move by the bytes of a character the chunks split: only what the
    // oracle makes of the text has to be the same.
    for strategy in STRATEGIES {
        let point =
            |observed: &oracle::Observed| return oracle::injection_point(strategy, observed);
        assert_eq!(
            split.as_ref().map(point),
            single.as_ref().map(point),
            "lol_html split at {points:?}, {strategy:?}"
        );
    }
    let without_text = |observed: Option<oracle::Observed>| {
        return observed.map(|observed| {
            return oracle::Observed {
                text: Vec::new(),
                data_text: Vec::new(),
                blank_data: Vec::new(),
                ..observed
            };
        });
    };
    assert_eq!(
        without_text(split),
        without_text(single),
        "lol_html split at {points:?}"
    );
}

/// What the browser makes of a document and of the injector's output with [`PROBE`] as the
/// snippet, according to html5ever, see [`check_tree`].
pub struct TreeVerdict {
    pub run: Run,
    /// html5ever's trees of the document and of the output, serialized.
    pub document_tree: String,
    pub output_tree: String,
    /// Indexes among the element children of the document's head of the metas the probe must
    /// follow, see [`required_metas`].
    pub required_metas: Vec<usize>,
    /// Where the document's head closed, if it did: past there, a snippet can only go in the body.
    pub head_closed_at: Option<usize>,
    /// How many of the head's last children the parser added once it had closed the head: an
    /// element the head may hold, after `</head>`, goes there all the same.
    pub closed_tail: usize,
    /// Every property the output fails, empty when not injected.
    pub failures: Vec<String>,
}

/// Injects [`PROBE`] into `document` and checks the properties any injection must have, whatever
/// the document, on html5ever's trees of the document (D) and of the output (O):
/// * P1: O holds exactly one probe, a `<script>` of the HTML namespace outside template contents,
///   made of the probe's own tag; the probe shows up nowhere else, in text, a comment, an attribute.
/// * P2: O without the probe is D.
/// * P3: the probe is a child of the document's head.
/// * P4: it follows every meta of the head declaring the character encoding, `X-UA-Compatible`, or
///   an enforcing `Content-Security-Policy`, short of the ones past where the injector walks, see
///   [`required_metas`].
/// * P5: under [`InjectionStrategy::HeadCloseTag`], nothing but whitespace text follows it in the
///   head, but for what the parser added to the head once closed.
///
/// A document the injector leaves alone, or that holds the probe's mark already, has nothing to
/// check.
pub fn tree_verdict(strategy: InjectionStrategy, document: &[u8]) -> TreeVerdict {
    let run = run_with_probe(strategy, &[document]);
    let original = tree::parse(document);
    let document_tree = original.serialize(None);
    let significant = original.serialize_significant(None);
    let head = original.head().expect("the parser always makes a head");
    let required_metas = required_metas(strategy, &original, &head);
    let closed_tail = head
        .children()
        .iter()
        .filter(|child| {
            return head
                .popped
                .get()
                .is_some_and(|popped| return child.id >= popped);
        })
        .count();
    let mut verdict = TreeVerdict {
        output_tree: document_tree.clone(),
        document_tree,
        required_metas,
        head_closed_at: head.popped_at.get(),
        closed_tail,
        failures: Vec::new(),
        run,
    };
    let Some(at) = verdict.run.injected_at else {
        return verdict;
    };
    if contains(document, PROBE_MARK.as_bytes()) {
        return verdict;
    }
    let output = tree::parse(&verdict.run.output);
    verdict.output_tree = output.serialize(None);
    verdict.failures = tree_failures(strategy, document, &significant, &verdict, &output, at);
    return verdict;
}

/// Asserts the properties of [`tree_verdict`]. Returns whether the snippet was injected.
pub fn check_tree(strategy: InjectionStrategy, document: &[u8]) -> bool {
    let verdict = tree_verdict(strategy, document);
    assert!(
        verdict.failures.is_empty(),
        "{strategy:?} injected at {:?}: {}, in:\n{}\noutput:\n{}\nD:\n{}O:\n{}",
        verdict.run.injected_at,
        verdict.failures.join("; "),
        String::from_utf8_lossy(document),
        String::from_utf8_lossy(&verdict.run.output),
        verdict.document_tree,
        verdict.output_tree,
    );
    return verdict.run.injected_at.is_some();
}

fn tree_failures(
    strategy: InjectionStrategy,
    document: &[u8],
    significant: &str,
    verdict: &TreeVerdict,
    output: &Tree,
    at: usize,
) -> Vec<String> {
    let mut failures = Vec::new();
    // P1.
    let probes = output.elements_with("data-dd-probe");
    // Where markup ahead of the document closed the head, the snippet goes in the body, see P3,
    // which a later `<frameset>` replaces, snippet included: nothing the injector can tell there,
    // and nothing else may change.
    let frameset = output.root().is_some_and(|root| {
        return root
            .children()
            .iter()
            .any(|child| return child.is_html("frameset"));
    });
    if probes.is_empty()
        && frameset
        && verdict
            .head_closed_at
            .is_some_and(|closed| return closed <= at)
    {
        if output.serialize_significant(None) != significant {
            failures.push(String::from(
                "P2: the output differs from the document, a frameset dropping the probe",
            ));
        }
        return failures;
    }
    let [probe] = probes.as_slice() else {
        failures.push(format!("P1: {} probe elements", probes.len()));
        return failures;
    };
    if !probe.is_html("script") {
        failures.push(format!("P1: the probe is a {:?}", probe.name()));
    }
    if probe.in_template() {
        failures.push(String::from("P1: the probe is within a template"));
    }
    if probe.offset != at + PROBE_START_TAG.len() {
        failures.push(format!(
            "P1: the probe element comes from a tag ending at {}, not its own",
            probe.offset
        ));
    }
    let marks = verdict.output_tree.matches(PROBE_MARK).count();
    if marks != 1 {
        failures.push(format!("P1: the probe shows up {marks} times in the tree"));
    }
    // P2.
    if output.serialize_significant(Some(probe)) != significant {
        failures.push(String::from(
            "P2: the output without the probe differs from the document",
        ));
    }
    // P3, short of markup printed ahead of the document, error messages for instance, which closed
    // the head the browser opened: the snippet goes in the body, right before the `<html>` or
    // `<body>` tag starting the document.
    let parent = probe.parent();
    let Some(head) = output.head().filter(|head| {
        return parent
            .as_ref()
            .is_some_and(|parent| return std::rc::Rc::ptr_eq(parent, head));
    }) else {
        let lower = document[at..].to_ascii_lowercase();
        let in_body = || {
            let mut node = parent.clone();
            while let Some(element) = node {
                if element.is_html("body") {
                    return element
                        .parent()
                        .is_some_and(|root| return root.is_html("html"));
                }
                node = element.parent();
            }
            return false;
        };
        let document_start = [b"<html".as_slice(), b"<body"].iter().any(|tag| {
            return lower.starts_with(tag)
                && lower
                    .get(tag.len())
                    .is_none_or(|&b| return b.is_ascii_whitespace() || b == b'/' || b == b'>');
        });
        if !(verdict
            .head_closed_at
            .is_some_and(|closed| return closed <= at)
            && document_start
            && in_body())
        {
            failures.push(String::from("P3: the probe is not a child of the head"));
        }
        return failures;
    };
    // P4: the output's head is the document's with the probe, P2 checks.
    let children = head.children();
    let index = children
        .iter()
        .position(|child| return std::rc::Rc::ptr_eq(child, probe))
        .expect("a child of its parent");
    let elements: Vec<&Handle> = children
        .iter()
        .filter(|child| return child.name().is_some() && !std::rc::Rc::ptr_eq(child, probe))
        .collect();
    let before = children[..index]
        .iter()
        .filter(|child| return child.name().is_some())
        .count();
    for &meta in &verdict.required_metas {
        if meta >= before {
            failures.push(format!(
                "P4: the probe precedes the meta {:?}",
                elements
                    .get(meta)
                    .map(|meta| return meta.attributes.borrow().clone())
            ));
        }
    }
    // P5.
    if strategy == InjectionStrategy::HeadCloseTag {
        let open_end = children.len().saturating_sub(verdict.closed_tail);
        if let Some(child) = children
            .get(index + 1..open_end.max(index + 1))
            .unwrap_or_default()
            .iter()
            .find(|child| return !child.is_blank_text())
        {
            failures.push(format!(
                "P5: {:?} follows the probe in the head",
                child.kind
            ));
        }
    }
    return failures;
}

/// The metas of `head`, by index among its element children, that the snippet must follow, see
/// [`oracle::is_relevant_meta`]: under [`InjectionStrategy::HeadCloseTag`], every one the head held
/// while open; under [`InjectionStrategy::HeadOpenTag`], the ones of them the injector walks to,
/// from a `<head>` tag ending within the window, up to [`WALK_LIMIT`], and past the window as long
/// as every element before is a `<meta>` or a `<title>`, and short of a `<![CDATA[`.
fn required_metas(strategy: InjectionStrategy, tree: &Tree, head: &Handle) -> Vec<usize> {
    let walk = strategy == InjectionStrategy::HeadOpenTag;
    let mut required = Vec::new();
    if walk && head.offset > WINDOW {
        return required;
    }
    // The walk stops at a `<![CDATA[`, whose end it cannot tell without tracking foreign content:
    // a bogus comment wherever it is, the head or ahead of it.
    let cdata = tree
        .nodes()
        .iter()
        .filter(|node| {
            return matches!(node.kind, tree::Kind::Comment)
                && node.text.borrow().starts_with("[CDATA[");
        })
        .map(|node| return node.id)
        .min()
        .unwrap_or(usize::MAX);
    let open = |child: &Handle| {
        return head
            .popped
            .get()
            .is_none_or(|popped| return child.id < popped);
    };
    let mut leading = true;
    let mut elements = 0;
    for child in head.children() {
        if !open(&child) {
            break;
        }
        if walk && child.id > cdata {
            break;
        }
        if child.name().is_none() {
            continue;
        }
        let (element, index) = (child, elements);
        elements += 1;
        leading &= element.is_html("meta") || element.is_html("title");
        // Where its tag ends: one starting within the window and ending past it is not required.
        if walk && ((element.offset > WINDOW && !leading) || element.offset > WALK_LIMIT) {
            break;
        }
        let relevant = element.is_html("meta")
            && oracle::is_relevant_meta(
                element.attribute("charset").as_deref(),
                element.attribute("http-equiv").as_deref(),
                element.attribute("content").as_deref(),
            );
        if relevant {
            required.push(index);
        }
    }
    return required;
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    return haystack
        .windows(needle.len())
        .any(|window| return window == needle);
}
