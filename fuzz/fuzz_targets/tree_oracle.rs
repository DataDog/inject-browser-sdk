#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! Wherever the snippet goes, the browser's tree holds it in the head, behind the metas it must
//! follow, and is otherwise unchanged, according to html5ever: properties rather than an expected
//! position, see `check_tree`.

#[path = "../../lib/inject-browser-sdk/tests/harness/input.rs"]
#[allow(dead_code)]
mod input;
#[path = "../../lib/inject-browser-sdk/tests/harness/oracle.rs"]
#[allow(dead_code)]
mod oracle;
#[path = "../../lib/inject-browser-sdk/tests/harness/tree.rs"]
#[allow(dead_code)]
mod tree;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The input format of `oracle_differential`: the first byte picks the strategy, the rest is
    // the document.
    let Some((&selector, document)) = data.split_first() else {
        return;
    };
    let strategy = input::STRATEGIES[usize::from(selector) % input::STRATEGIES.len()];
    input::check_tree(strategy, document);
});
