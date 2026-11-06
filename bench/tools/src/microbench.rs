//! In-process cost of the library alone: `Injector::new` + `write` over a page cut in fixed-size
//! chunks + `end`, no server, no I/O. Usage: microbench <page>...
//! Chunk sizes: 32 KiB (nginx output_buffers), 8000 B (APR file read), whole page.

use std::{env, fs, hint::black_box, path::Path, time::Instant};

use inject_browser_sdk::injector::Injector;

fn run(page: &[u8], chunk: usize) -> usize {
    let mut injector = Injector::new(bench_common::snippet());
    let mut out = 0;
    for piece in page.chunks(chunk) {
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
    println!("| page | size | chunk | ns/page | GB/s |");
    println!("|---|---:|---:|---:|---:|");
    for path in env::args().skip(1) {
        let page = fs::read(&path).unwrap();
        let name = Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
        for chunk in [32 * 1024, 8000, page.len().max(1)] {
            let iterations = (200_000_000 / page.len().max(1)).clamp(1_000, 2_000_000);
            for _ in 0..iterations / 10 {
                black_box(run(&page, chunk));
            }
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(run(black_box(&page), chunk));
            }
            let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
            let label = if chunk >= page.len() { "whole".to_string() } else { chunk.to_string() };
            println!("| {name} | {} | {label} | {ns:.0} | {:.1} |", page.len(), page.len() as f64 / ns);
        }
    }
}
