#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Drives the injector through the C API, as lol_html's `fuzz_c_api` does for its own: whatever
//! the document and its chunking, every byte comes out once, plus the snippet or the padding, and
//! the held-back length accounts for the rest.

use std::ffi::CString;
use std::slice;

use inject_browser_sdk_ffi::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The first byte picks the chunk size and how the document ends.
    let Some((&selector, document)) = data.split_first() else {
        return;
    };
    let chunk_size = usize::from(selector >> 1).max(1);
    let padded = selector & 1 == 0;

    let json = CString::new(r#"{"majorVersion":6,"rum":{"applicationId":"a","clientToken":"t"}}"#)
        .unwrap();
    unsafe {
        let snippet = snippet_create_from_json(json.as_ptr());
        assert!(!snippet.is_null() && (*snippet).error_code == 0);
        let snippet_length = (*snippet).length as usize;
        let injector = injector_create(snippet);
        assert!(!injector.is_null());

        let mut emitted = 0usize;
        let mut injected = false;
        let mut consume = |result: InjectorResult| {
            for slice in slice::from_raw_parts(result.slices, result.slices_length as usize) {
                emitted += slice.length as usize;
            }
            injected |= result.injected;
        };
        for chunk in document.chunks(chunk_size) {
            consume(injector_write(injector, chunk.as_ptr(), chunk.len() as u32));
            assert!(injector_held_back_length(injector) as usize <= 1024);
        }
        consume(match padded {
            true => injector_end(injector),
            false => injector_end_without_padding(injector),
        });
        assert_eq!(injector_held_back_length(injector), 0);

        let added = match padded || injected {
            true => snippet_length,
            false => 0,
        };
        assert_eq!(emitted, document.len() + added);

        injector_cleanup(injector);
        snippet_cleanup(snippet);
    }
});
