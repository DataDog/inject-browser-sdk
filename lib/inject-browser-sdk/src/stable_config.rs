// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config_option::build_configuration;
use crate::error::Error;
use crate::generate_snippet;

/// Build a [`Configuration`] from a lookup function and generate the snippet bytes.
///
/// The `lookup` closure takes a DD_* key name and returns the value if present.
/// RUM-specific keys (e.g. `DD_RUM_SITE`) take precedence over generic keys (e.g. `DD_SITE`).
pub fn snippet_from_lookup<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Result<Vec<u8>, Error> {
    build_configuration(lookup).and_then(|config| generate_snippet(&config))
}

/// Build a [`Configuration`] from a map of `DD_*` key-value pairs and generate the snippet bytes.
///
/// Convenience wrapper around [`snippet_from_lookup`].
pub fn snippet_from_entries(entries: &BTreeMap<String, String>) -> Result<Vec<u8>, Error> {
    snippet_from_lookup(|key| entries.get(key).map(|v| v.as_str()))
}

/// Build a snippet from Datadog stable configuration files, with optional overrides.
///
/// Reads configuration from the Datadog Agent's `application_monitoring.yaml` files
/// (local and fleet-managed). Caller-supplied `overrides` take precedence over values
/// read from disk.
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr.
#[cfg(feature = "stable-config")]
pub fn snippet_from_stable_config(
    language: &str,
    debug_logs: bool,
    overrides: &[(&crate::config_option::ConfigOption, &str)],
) -> Result<Vec<u8>, Error> {
    let configs =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;

    snippet_from_lookup(|key| {
        // Check overrides first
        for &(opt, value) in overrides {
            if opt.key() == key {
                return Some(value);
            }
        }
        // Then check stable config entries
        configs
            .iter()
            .find(|c| c.name == key)
            .map(|c| c.value.as_str())
    })
}
