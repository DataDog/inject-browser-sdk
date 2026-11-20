//! Library-only chunking check: each page written in random splits must give the same output
//! as written in one piece (padded and unpadded). Usage: splitcheck <iterations> <page>...

use std::{env, fs};

use inject_browser_sdk::injector::{Injector, Result as InjectorResult};

fn collect(out: &mut Vec<u8>, result: InjectorResult) {
    for slice in &result.slices[..result.length] {
        out.extend_from_slice(slice.bytes);
    }
}

fn inject(chunks: &[&[u8]], padded: bool) -> Vec<u8> {
    let mut injector = Injector::new(bench_common::snippet());
    let mut out = Vec::new();
    for chunk in chunks {
        collect(&mut out, injector.write(chunk));
    }
    collect(&mut out, if padded { injector.end() } else { injector.end_without_padding() });
    out
}

/// xorshift, to stay dependency-free.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let iterations: usize = args.next().expect("iterations").parse().unwrap();
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let (mut checked, mut mismatches) = (0, 0);
    for path in args {
        let page = fs::read(&path).unwrap();
        let whole = [inject(&[&page], true), inject(&[&page], false)];
        for _ in 0..iterations {
            let mut cuts = vec![0, page.len()];
            let pieces = 1 + rng.next() % 12;
            for _ in 0..pieces {
                // Bias the cuts towards the head window.
                let limit = if rng.next() % 2 == 0 { 1100.min(page.len()) } else { page.len() };
                cuts.push((rng.next() as usize) % (limit + 1));
            }
            cuts.sort_unstable();
            let chunks: Vec<&[u8]> = cuts.windows(2).map(|w| &page[w[0]..w[1]]).collect();
            for (index, padded) in [true, false].into_iter().enumerate() {
                checked += 1;
                if inject(&chunks, padded) != whole[index] {
                    mismatches += 1;
                    if mismatches <= 5 {
                        let sizes: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
                        println!("MISMATCH {path} padded={padded} chunks={sizes:?}");
                    }
                }
            }
        }
    }
    println!("splitcheck: {mismatches} mismatches out of {checked} runs");
}
