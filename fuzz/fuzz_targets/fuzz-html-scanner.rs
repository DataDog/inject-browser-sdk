#![no_main]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

extern crate inject_browser_sdk;
extern crate libfuzzer_sys;

use inject_browser_sdk::{generate_snippet, injector::Injector, Configuration, RumConfiguration};
use libfuzzer_sys::fuzz_target;
use serde_json::json;
use std::collections::BTreeMap as Map;

fuzz_target!(|data: &[u8]| {
    let conf: Configuration = Configuration {
        major_version: 5,
        rum: RumConfiguration {
            fields: Map::from([
                (Box::from("applicationId"), json!("foo")),
                (Box::from("clientToken"), json!("bar")),
            ]),
        },
    };

    let snippet = generate_snippet(&conf).unwrap();
    let mut injector = Injector::new(snippet.as_slice());
    let _res = injector.write(data);
});
