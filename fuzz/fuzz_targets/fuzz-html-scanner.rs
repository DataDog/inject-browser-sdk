#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

extern crate inject_browser_sdk;
extern crate libfuzzer_sys;

use inject_browser_sdk::{
    generate_snippet,
    injector::{InjectionStrategy, Injector},
    Configuration, RumConfiguration,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let conf: Configuration = Configuration {
        major_version: 5,
        rum: RumConfiguration {
            application_id: Some("foo".into()),
            client_token: Some("bar".into()),
            ..Default::default()
        },
    };

    // Spend the first byte on the strategy and on how the document ends, so every combination gets
    // fuzzed. The strategies hold back different amounts of data, so they exercise quite different
    // buffering paths.
    let (strategy, padded, data) = match data.split_first() {
        Some((selector, rest)) => {
            let strategy = match selector % 2 {
                0 => InjectionStrategy::HeadCloseTag,
                _ => InjectionStrategy::HeadOpenTag,
            };
            (strategy, (selector / 2) % 2 == 0, rest)
        }
        None => (InjectionStrategy::default(), true, data),
    };

    let snippet = generate_snippet(&conf).unwrap();
    let mut injector = Injector::with_strategy(snippet.as_slice(), strategy);

    let mut emitted = 0usize;
    let result = injector.write(data);
    emitted += result.slices[..result.length]
        .iter()
        .map(|slice| return slice.bytes.len())
        .sum::<usize>();
    let result = match padded {
        true => injector.end(),
        false => injector.end_without_padding(),
    };
    emitted += result.slices[..result.length]
        .iter()
        .map(|slice| return slice.bytes.len())
        .sum::<usize>();

    // Whatever happens, the injector emits every input byte exactly once, plus either the snippet
    // or the padding that stands in for it (no padding with `end_without_padding`). This catches
    // any arithmetic mistake in the slicing.
    let added = match padded || result.injected {
        true => snippet.len(),
        false => 0,
    };
    assert_eq!(
        emitted,
        data.len() + added,
        "emitted length must be the input length plus the snippet or padding length"
    );
});
