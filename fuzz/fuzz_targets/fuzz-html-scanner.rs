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

fuzz_target!(|data: &[u8]| {
    let conf: Configuration = Configuration {
        major_version: 5,
        enabled: None,
        rum: RumConfiguration {
            application_id: Some("foo".into()),
            client_token: Some("bar".into()),
            ..Default::default()
        },
    };

    let snippet = generate_snippet(&conf).unwrap();
    let mut injector = Injector::new(snippet.as_slice());
    let _res = injector.write(data);
});
