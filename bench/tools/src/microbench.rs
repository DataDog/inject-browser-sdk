//! In-process cost of the library alone, `Injector::new` + `write` over the page cut in chunks +
//! `end`, with no server and no I/O, over a matrix of generated pages:
//!
//! * the `<head>`: tiny (a title), 1 KiB of `<meta>`/`<link>` crossing the head window, 8 KiB of
//!   inline script and style, one starting past the window, none at all, and a body that is not
//!   HTML;
//! * the page size: 512 B to 1 MiB, the head included (a head larger than the page is skipped);
//! * the chunking: 8000 B (APR file reads), 32 KiB (nginx output buffers), the whole page.
//!
//! Prints one `ns_per_page <config> <value>` line per configuration, the format
//! `bench/layouts.sh` collects, or a table with `--table`. `--filter SUBSTRING` narrows the
//! configurations, `--scale F` multiplies the iteration counts.

use std::{env, hint::black_box, time::Instant};

use inject_browser_sdk::injector::Injector;

const KINDS: &[&str] = &["head_tiny", "head_1k", "head_8k", "head_late", "no_head", "not_html"];
const SIZES: &[(usize, &str)] =
    &[(512, "512"), (2 << 10, "2k"), (16 << 10, "16k"), (128 << 10, "128k"), (1 << 20, "1m")];
const CHUNKS: &[(usize, &str)] = &[(8000, "8000"), (32 << 10, "32k"), (usize::MAX, "whole")];

/// Body filler: plain markup, like most of a real page past its head.
const FILLER: &str = "<div class=\"card\"><p>Lorem ipsum dolor sit amet, \
                      <a href=\"/item/42\">consectetur</a> adipiscing elit.</p></div>\n";

fn metas(bytes: usize) -> String {
    let mut out = String::from(
        "<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
    );
    let mut i = 0;
    while out.len() < bytes {
        out += &format!(
            "<meta property=\"og:tag{i}\" content=\"Some page description number {i}\">\n\
             <link rel=\"preload\" href=\"/static/asset{i}.js\" as=\"script\">\n"
        );
        i += 1;
    }
    out
}

fn head(kind: &str) -> String {
    let doc = "<!doctype html><html lang=\"en\">";
    match kind {
        "head_tiny" => format!("{doc}<head><title>Page</title></head><body>\n"),
        "head_1k" => format!("{doc}<head>\n{}<title>Page</title></head><body>\n", metas(1000)),
        "head_8k" => format!(
            "{doc}<head>\n{}<style>{}</style><script>{}</script><title>Page</title></head><body>\n",
            metas(1000),
            ".c{color:red;margin:0 auto}\n".repeat(120),
            "window.dataLayer=window.dataLayer||[];if(a<b&&c>d){f('</div>')}\n".repeat(60),
        ),
        "head_late" => format!(
            "{doc}<!-- {} --><head><title>Page</title></head><body>\n",
            "build info ".repeat(200)
        ),
        "no_head" => format!("{doc}<body>\n"),
        "not_html" => "{\"items\":[".into(),
        _ => unreachable!(),
    }
}

fn page(kind: &str, size: usize) -> Option<Vec<u8>> {
    let mut page = head(kind);
    if page.len() > size {
        return None;
    }
    let filler = match kind {
        "not_html" => "{\"id\":42,\"name\":\"lorem ipsum dolor sit amet\"},\n",
        _ => FILLER,
    };
    while page.len() < size {
        page += filler;
    }
    page.truncate(size);
    Some(page.into_bytes())
}

fn run(snippet: &[u8], page: &[u8], chunk: usize) -> usize {
    let mut injector = Injector::new(snippet);
    let mut out = 0;
    for piece in page.chunks(chunk.min(page.len())) {
        let result = injector.write(piece);
        for slice in &result.slices[..result.length] {
            out += black_box(slice.bytes).len();
        }
    }
    let result = injector.end();
    for slice in &result.slices[..result.length] {
        out += black_box(slice.bytes).len();
    }
    out
}

fn main() {
    let mut args = env::args().skip(1);
    let (mut table, mut filter, mut scale) = (false, String::new(), 1.0f64);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--table" => table = true,
            "--filter" => filter = args.next().expect("--filter SUBSTRING"),
            "--scale" => scale = args.next().expect("--scale F").parse().expect("a number"),
            _ => panic!("unknown argument {arg}"),
        }
    }
    let snippet = bench_common::snippet();
    if table {
        println!("| config | ns/page | GB/s |\n|---|---:|---:|");
    }
    for kind in KINDS {
        for &(size, size_name) in SIZES {
            let Some(page) = page(kind, size) else { continue };
            for &(chunk, chunk_name) in CHUNKS {
                if chunk != usize::MAX && chunk >= page.len() {
                    continue;
                }
                let config = format!("{kind}_{size_name}_{chunk_name}");
                if !config.contains(&filter) {
                    continue;
                }
                // About 0.2 s per configuration, whatever its cost.
                let calibrate = Instant::now();
                black_box(run(snippet, black_box(&page), chunk));
                let once = calibrate.elapsed().as_nanos().max(50) as f64;
                let iterations = ((2e8 / once * scale) as usize).clamp(20, 4_000_000);
                for _ in 0..iterations / 10 {
                    black_box(run(snippet, black_box(&page), chunk));
                }
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(run(snippet, black_box(&page), chunk));
                }
                let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
                if table {
                    println!("| {config} | {ns:.0} | {:.2} |", page.len() as f64 / ns);
                } else {
                    println!("ns_per_page {config} {ns:.1}");
                }
            }
        }
    }
}
