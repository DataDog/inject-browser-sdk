#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! The snippet goes where lol_html says it does, short of a documented exception.

#[path = "../../lib/inject-browser-sdk/tests/harness/input.rs"]
#[allow(dead_code)]
mod input;
#[path = "../../lib/inject-browser-sdk/tests/harness/oracle.rs"]
mod oracle;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|document: &[u8]| {
    let single = input::run(&[document]);
    input::check_oracle(document, &single);
});
