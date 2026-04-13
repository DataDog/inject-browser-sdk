// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::collections::BTreeMap;
use alloc::string::String;

use crate::config_option::build_configuration;
use crate::configuration::Configuration;
use crate::error::Error;

/// Builds a [`Configuration`] from a map of `DD_*` key-value pairs.
///
/// RUM-specific keys (e.g. `DD_RUM_SITE`) take precedence over generic keys (e.g. `DD_SITE`).
/// Returns a validated `Configuration` or an error.
pub fn configuration_from_entries(
    entries: &BTreeMap<String, String>,
) -> Result<Configuration, Error> {
    build_configuration(|key| entries.get(key).map(|v| v.as_str()))
}
