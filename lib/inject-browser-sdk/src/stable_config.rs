// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::configuration::Configuration;
#[cfg(feature = "stable-config")]
use crate::configuration::ConfigurationOverlay;
use crate::error::Error;
use crate::generate_snippet;

/// `lookup` is called with `DD_RUM_*` keys; missing keys become field absences.
pub fn snippet_from_lookup<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Result<Vec<u8>, Error> {
    Configuration::from_lookup(lookup).and_then(|config| generate_snippet(&config))
}

/// Convenience wrapper around [`snippet_from_lookup`].
pub fn snippet_from_entries(entries: &BTreeMap<String, String>) -> Result<Vec<u8>, Error> {
    snippet_from_lookup(|key| entries.get(key).map(|v| v.as_str()))
}

/// Build a snippet from Datadog stable configuration files, with an optional overlay.
///
/// Reads configuration from the Datadog Agent's `application_monitoring.yaml` files
/// (local and fleet-managed) using the `DD_RUM_*` key set. If `overlay` is
/// provided, its typed fields and `other` entries are merged on top of the
/// disk-sourced configuration via [`Configuration::overlay`].
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr.
#[cfg(feature = "stable-config")]
pub fn snippet_from_stable_config(
    language: &str,
    debug_logs: bool,
    overlay: Option<ConfigurationOverlay>,
) -> Result<Vec<u8>, Error> {
    let configs =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;

    let mut config = Configuration::from_lookup(|key| {
        configs
            .iter()
            .find(|c| c.name == key)
            .map(|c| c.value.as_str())
    })?;

    if let Some(o) = overlay {
        config.overlay(o);
    }

    generate_snippet(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn entries(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (String::from(*k), String::from(*v)))
            .collect()
    }

    #[test]
    fn snippet_from_entries_minimal() {
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
        ]);
        let result = snippet_from_entries(&map);
        assert!(result.is_ok(), "unexpected error: {:?}", result.err());
        let bytes = result.unwrap();
        assert!(bytes
            .windows(b"applicationId".len())
            .any(|w| w == b"applicationId"));
        assert!(bytes.windows(b"app-1".len()).any(|w| w == b"app-1"));
    }

    #[test]
    fn snippet_from_entries_missing_required() {
        let map: BTreeMap<String, String> = BTreeMap::new();
        let err = snippet_from_entries(&map).unwrap_err();
        assert_eq!(err.code(), 5);
    }

    #[test]
    fn snippet_from_lookup_invalid_rate() {
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "150.0"),
        ]);
        let err = snippet_from_lookup(|key| map.get(key).map(|v| v.as_str())).unwrap_err();
        assert_eq!(err.code(), 4);
    }

    #[test]
    fn snippet_from_entries_precedence() {
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
            ("DD_RUM_SITE", "datadoghq.com"),
            ("DD_SITE", "datadoghq.eu"),
        ]);
        let bytes = snippet_from_entries(&map).unwrap();
        assert!(bytes
            .windows(b"datadoghq.com".len())
            .any(|w| w == b"datadoghq.com"));
        assert!(!bytes
            .windows(b"datadoghq.eu".len())
            .any(|w| w == b"datadoghq.eu"));
    }
}
