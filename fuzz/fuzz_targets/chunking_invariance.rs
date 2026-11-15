#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! Any chunking of a document gives the output of the single chunk run, and the chunks a server
//! rebuilds around the injection serve it too.

#[path = "../../lib/inject-browser-sdk/tests/harness/input.rs"]
#[allow(dead_code)]
mod input;
#[path = "../../lib/inject-browser-sdk/tests/harness/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The first byte picks the strategy and how many split points follow, two bytes each, then the
    // document.
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let strategy = input::STRATEGIES[usize::from(selector) % input::STRATEGIES.len()];
    let count = selector / 2;
    let count = usize::from(count % 16).min(rest.len() / 2);
    let (points, document) = rest.split_at(count * 2);
    let mut points: Vec<usize> = points
        .chunks(2)
        .map(|point| {
            return usize::from(u16::from_le_bytes([point[0], point[1]])) % (document.len() + 1);
        })
        .collect();
    points.sort_unstable();

    let single = input::run(strategy, &[document]);
    input::check_chunking(strategy, document, &single, &points);
});
