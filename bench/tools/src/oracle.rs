//! Expected output for the accuracy check: runs the library's `Injector` over each whole page
//! in-process, in a single write, and writes `<out>/<name>.padded` (fixed-length responses,
//! `end()`) and `<out>/<name>.unpadded` (chunked responses, `end_without_padding()`), plus
//! `<out>/snippet` with the snippet bytes.
//!
//! Usage: oracle <out_dir> <page>...

use std::{env, fs, path::Path, process};

use inject_browser_sdk::injector::{Injector, Result as InjectorResult};

fn collect(out: &mut Vec<u8>, result: InjectorResult) {
    for slice in &result.slices[..result.length] {
        out.extend_from_slice(slice.bytes);
    }
}

fn expected(page: &[u8], padded: bool) -> Vec<u8> {
    let mut injector = Injector::new(bench_common::snippet());
    let mut out = Vec::with_capacity(page.len() + bench_common::snippet().len());
    collect(&mut out, injector.write(page));
    collect(&mut out, if padded { injector.end() } else { injector.end_without_padding() });
    out
}

fn main() {
    let mut args = env::args().skip(1);
    let Some(out_dir) = args.next() else {
        eprintln!("usage: oracle <out_dir> <page>...");
        process::exit(2);
    };
    let out_dir = Path::new(&out_dir);
    fs::create_dir_all(out_dir).expect("create out dir");
    fs::write(out_dir.join("snippet"), bench_common::snippet()).expect("write snippet");
    for page in args {
        let bytes = fs::read(&page).unwrap_or_else(|e| panic!("read {page}: {e}"));
        let name = Path::new(&page).file_name().unwrap().to_string_lossy().into_owned();
        fs::write(out_dir.join(format!("{name}.padded")), expected(&bytes, true)).unwrap();
        fs::write(out_dir.join(format!("{name}.unpadded")), expected(&bytes, false)).unwrap();
    }
}
