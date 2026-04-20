// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::configuration::Configuration;
use crate::error::Error;
use crate::generate_snippet;

/// Build a [`crate::Configuration`] from a lookup function and generate the snippet bytes.
///
/// The `lookup` closure takes a `DD_RUM_*` key name and returns the value if present.
pub fn snippet_from_lookup<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Result<Vec<u8>, Error> {
    Configuration::from_lookup(lookup).and_then(|config| generate_snippet(&config))
}

/// Build a [`crate::Configuration`] from a map of `DD_RUM_*` key-value pairs and generate the snippet bytes.
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
/// `overrides` is a slice of `(name, value)` pairs where `name` is a `DD_RUM_*` key
/// accepted by [`Configuration::from_lookup`]. Names not recognised by `from_lookup`
/// are silently ignored (no setter is called for them).
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr.
#[cfg(feature = "stable-config")]
pub fn snippet_from_stable_config(
    language: &str,
    debug_logs: bool,
    overrides: &[(&str, &str)],
) -> Result<Vec<u8>, Error> {
    let configs =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;

    snippet_from_lookup(|key| {
        for &(name, value) in overrides {
            if name == key {
                return Some(value);
            }
        }
        configs
            .iter()
            .find(|c| c.name == key)
            .map(|c| c.value.as_str())
    })
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
