// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
use libdd_library_config::ConfigRead;
use libdd_library_config::LibraryConfig;

use crate::configuration::{Configuration, ConfigurationOverlay};
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
/// When `debug_logs` is `true`, diagnostic messages are written to stderr on
/// Unix. On Windows stderr is a no-op (see [`crate::platform::write_stderr`]);
/// diagnostics are folded into the returned error message so they survive.
pub fn snippet_from_stable_config(
    language: &str,
    debug_logs: bool,
    overlay: Option<ConfigurationOverlay>,
) -> Result<Vec<u8>, Error> {
    let (configs, logs) =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;
    build_snippet_from_configs(configs, logs, overlay)
}

/// Variant of [`snippet_from_stable_config`] that accepts a caller-supplied
/// reader. Intended for tests that need to exercise populated YAML input
/// without touching the real filesystem.
#[cfg(test)]
pub(crate) fn snippet_from_stable_config_with_reader<R: ConfigRead>(
    reader: &R,
    language: &str,
    debug_logs: bool,
    overlay: Option<ConfigurationOverlay>,
) -> Result<Vec<u8>, Error> {
    let (configs, logs) =
        crate::platform::read_stable_configs_with_reader(reader, language, debug_logs)
            .map_err(Error::StableConfig)?;
    build_snippet_from_configs(configs, logs, overlay)
}

fn build_snippet_from_configs(
    configs: Vec<LibraryConfig>,
    logs: Vec<String>,
    overlay: Option<ConfigurationOverlay>,
) -> Result<Vec<u8>, Error> {
    // Distinguish "no config source at all" from "config source present but
    // missing required fields". Without this, the caller gets code 5
    // ("mandatory field 'applicationId' is empty") and spends time
    // troubleshooting file contents when the real issue is that no files
    // matched or existed.
    if configs.is_empty() && overlay.is_none() {
        // Surface any diagnostics captured on the Ok(empty) path (e.g. a file
        // reclassified as TooLarge by the configurator) so Windows callers,
        // who never see stderr, still get the full context.
        let base = "no stable-config entries found; \
             expected DD_RUM_* keys in application_monitoring.yaml \
             (local or fleet-managed) or an overlay";
        if logs.is_empty() {
            return Err(Error::StableConfig(String::from(base)));
        }
        return Err(Error::StableConfig(alloc::format!(
            "{base}\n{}",
            logs.join("\n")
        )));
    }

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
    use serde_json::Value;

    fn entries(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (String::from(*k), String::from(*v)))
            .collect()
    }

    /// Extract the JSON object passed to `DD_RUM.init(...)` so tests can
    /// assert on structured fields instead of raw-byte windows (which match
    /// by accident inside the CDN URL, e.g. "us1" appears in `/us1/v5/`).
    fn extract_init_json(bytes: &[u8]) -> Value {
        let text = core::str::from_utf8(bytes).expect("snippet is UTF-8");
        let start_marker = "DD_RUM.init(";
        let start = text
            .find(start_marker)
            .expect("snippet must contain DD_RUM.init(")
            + start_marker.len();
        let end = text[start..]
            .find(");")
            .expect("DD_RUM.init(...) must be terminated")
            + start;
        serde_json::from_str(&text[start..end]).expect("init arg must be JSON")
    }

    #[test]
    fn snippet_from_entries_minimal() {
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
        ]);
        let bytes = snippet_from_entries(&map).expect("minimal entries must succeed");
        let init = extract_init_json(&bytes);
        assert_eq!(init["applicationId"], Value::from("app-1"));
        assert_eq!(init["clientToken"], Value::from("tok-1"));
        assert_eq!(init.get("site"), None, "site should be unset by default");
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
    fn snippet_from_lookup_negative_rate() {
        // -5.0 is outside 0..=100 and must be rejected at validate-time (code 4).
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "-5.0"),
        ]);
        let err = snippet_from_lookup(|key| map.get(key).map(|v| v.as_str())).unwrap_err();
        assert_eq!(err.code(), 4);
    }

    #[test]
    fn snippet_from_entries_ignores_non_dd_rum_keys() {
        // Only DD_RUM_* keys are consulted; Datadog-wide keys like DD_SITE
        // are ignored. The embedded init config must have no `site` key and
        // thus default to the "us1" CDN region at runtime.
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
            ("DD_SITE", "datadoghq.eu"),
        ]);
        let bytes = snippet_from_entries(&map).unwrap();
        let init = extract_init_json(&bytes);
        assert_eq!(init.get("site"), None, "DD_SITE must not leak into init");
    }

    #[test]
    fn snippet_from_entries_dd_rum_site_wins_when_both_are_set() {
        // DD_RUM_SITE must win over DD_SITE. Guards against a future refactor
        // that adds DD_SITE as a fallback silently breaking the documented
        // precedence contract.
        let map = entries(&[
            ("DD_RUM_APPLICATION_ID", "app-1"),
            ("DD_RUM_CLIENT_TOKEN", "tok-1"),
            ("DD_SITE", "datadoghq.eu"),
            ("DD_RUM_SITE", "datadoghq.com"),
        ]);
        let bytes = snippet_from_entries(&map).unwrap();
        let init = extract_init_json(&bytes);
        assert_eq!(init["site"], Value::from("datadoghq.com"));
    }

    #[test]
    fn overlay_wins_over_populated_disk_config() {
        use crate::configuration::RumConfiguration;

        let overlay = ConfigurationOverlay {
            rum: RumConfiguration {
                site: Some(String::from("us3.datadoghq.com")),
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = snippet_from_stable_config_with_reader(
            &crate::platform::tests::PopulatedReader,
            "browser-sdk",
            false,
            Some(overlay),
        )
        .expect("should build snippet");

        let init = extract_init_json(&bytes);
        assert_eq!(init["site"], Value::from("us3.datadoghq.com"));
        assert_eq!(init["applicationId"], Value::from("app-populated"));
        assert_eq!(init["clientToken"], Value::from("tok-populated"));
    }

    /// Reader whose files always exceed the configurator's size gate; the
    /// configurator reclassifies the read as `Ok(empty)` while pushing a
    /// `TooLarge` diagnostic into logs. Exercises the Windows-visible path
    /// where stderr is a no-op: the diagnostics must be folded into the final
    /// error message rather than silently dropped.
    struct TooLargeReader;

    impl ConfigRead for TooLargeReader {
        type IoError = core::convert::Infallible;

        fn read(
            &self,
            _path: &str,
        ) -> Result<alloc::vec::Vec<u8>, libdd_library_config::ConfigReadError<Self::IoError>>
        {
            Err(libdd_library_config::ConfigReadError::TooLarge)
        }
    }

    #[test]
    fn too_large_reader_with_no_overlay_surfaces_diagnostic_in_error() {
        let err =
            snippet_from_stable_config_with_reader(&TooLargeReader, "browser-sdk", false, None)
                .expect_err("TooLarge reclassified to empty + no overlay must error");
        assert_eq!(err.code(), 6);
        let msg = alloc::format!("{err}");
        assert!(
            msg.contains("no stable-config entries found"),
            "expected base message, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("too large") || msg.contains("TooLarge"),
            "expected TooLarge diagnostic to be folded into error, got: {msg}"
        );
    }
}
