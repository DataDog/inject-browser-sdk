// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
use libdd_library_config::ConfigRead;
use libdd_library_config::LibraryConfig;

use crate::configuration::{parse_bool, Configuration, ConfigurationOverlay};
use crate::error::Error;
use crate::generate_snippet;

/// A generated snippet and its resolved host-side values.
#[cfg_attr(test, derive(Debug))]
pub struct StableConfigSnippet {
    /// The generated Browser SDK snippet, empty when injection is disabled.
    pub bytes: Vec<u8>,
    /// Resolved `DD_RUM_ENABLED`, or `None` when nothing configured it. Hosts
    /// own the default for `None`.
    pub enabled: Option<bool>,
    /// Resolved `applicationId`, for host-side telemetry tagging.
    pub application_id: Option<String>,
    /// Resolved `remoteConfigurationId`, for host-side telemetry tagging.
    pub remote_configuration_id: Option<String>,
}

/// Entries read once from the `application_monitoring.yaml` files, reusable across overlays.
pub struct StableConfig {
    configs: Vec<LibraryConfig>,
    logs: Vec<String>,
    empty_file_present: bool,
}

impl StableConfig {
    fn lookup(&self, key: &str) -> Option<&str> {
        return self
            .configs
            .iter()
            .find(|config| return config.name == key)
            .map(|config| return config.value.as_str());
    }

    fn disabled_snippet(
        &self,
        overlay: Option<&ConfigurationOverlay>,
        enabled: Option<bool>,
    ) -> StableConfigSnippet {
        let application_id = overlay
            .and_then(|value| return value.rum.application_id.clone())
            .or_else(|| {
                return self.lookup("DD_RUM_APPLICATION_ID").map(String::from);
            });
        let remote_configuration_id = overlay
            .and_then(|value| return value.rum.remote_configuration_id.clone())
            .or_else(|| {
                return self
                    .lookup("DD_RUM_REMOTE_CONFIGURATION_ID")
                    .map(String::from);
            });
        return StableConfigSnippet {
            bytes: Vec::new(),
            enabled,
            application_id,
            remote_configuration_id,
        };
    }

    /// Whether no supported RUM stable-configuration entry matched this process.
    pub fn is_empty(&self) -> bool {
        return !self
            .configs
            .iter()
            .any(|config| return crate::configuration::is_rum_configuration_key(&config.name));
    }

    /// Build a snippet from this configuration, with an optional overlay merged
    /// on top of it via [`Configuration::overlay`]. Does no I/O.
    ///
    /// Returns [`Error::StableConfigAbsent`] when there is nothing to build
    /// from: no entries matched and no overlay was supplied.
    pub fn build_snippet(
        &self,
        overlay: Option<ConfigurationOverlay>,
    ) -> Result<StableConfigSnippet, Error> {
        if self.is_empty() && overlay.is_none() {
            let message = empty_config_message(&self.logs);
            if self.empty_file_present {
                return Err(Error::StableConfig(message));
            }
            return Err(Error::StableConfigAbsent(message));
        }

        // Resolve the host-side switch independently first. If it disables
        // injection, invalid or incomplete SDK options must not hide that
        // explicit opt-out from the host.
        let enabled = match overlay.as_ref().and_then(|value| return value.enabled) {
            Some(enabled) => Some(enabled),
            None => self
                .lookup("DD_RUM_ENABLED")
                .map(|raw| return parse_bool(raw, "DD_RUM_ENABLED"))
                .transpose()?,
        };
        if enabled == Some(false) {
            return Ok(self.disabled_snippet(overlay.as_ref(), enabled));
        }

        let mut config = Configuration::from_lookup(|key| return self.lookup(key))?;

        if let Some(o) = overlay {
            config.overlay(o);
        }

        return Ok(StableConfigSnippet {
            bytes: generate_snippet(&config)?,
            enabled,
            application_id: config.rum.application_id.clone(),
            remote_configuration_id: config.rum.remote_configuration_id.clone(),
        });
    }
}

fn empty_config_message(logs: &[String]) -> String {
    let base = "no stable-config entries found; \
         expected DD_RUM_* keys in application_monitoring.yaml \
         (local or fleet-managed) or an overlay";
    if logs.is_empty() {
        return String::from(base);
    }
    return alloc::format!("{base}\n{}", logs.join("\n"));
}

fn stable_config_from_read(read: crate::platform::StableConfigRead) -> Result<StableConfig, Error> {
    // The configurator can turn a source failure such as TooLarge into Ok, keeping
    // the entries of a lower-priority source. Reject that partial result: the source
    // it could not read may hold an opt-out or other RUM settings that override them.
    if read.read_failed {
        return Err(Error::StableConfig(empty_config_message(&read.logs)));
    }
    return Ok(StableConfig {
        configs: read.configs,
        logs: read.logs,
        empty_file_present: read.empty_file_present,
    });
}

/// Read and parse the local and fleet-managed stable configuration once.
/// Missing files return an empty [`StableConfig`].
pub fn read_stable_config(language: &str, debug_logs: bool) -> Result<StableConfig, Error> {
    let read =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;
    return stable_config_from_read(read);
}

/// Build a snippet from Datadog stable configuration files, with an optional overlay.
///
/// Convenience wrapper over [`read_stable_config`] followed by
/// [`StableConfig::build_snippet`]. Re-reads the files on every call; hold a
/// [`StableConfig`] to build several snippets from one read.
pub fn snippet_from_stable_config(
    language: &str,
    debug_logs: bool,
    overlay: Option<ConfigurationOverlay>,
) -> Result<StableConfigSnippet, Error> {
    return read_stable_config(language, debug_logs)?.build_snippet(overlay);
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
) -> Result<StableConfigSnippet, Error> {
    let read = crate::platform::read_stable_configs_with_reader(reader, language, debug_logs)
        .map_err(Error::StableConfig)?;
    return stable_config_from_read(read)?.build_snippet(overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    /// Extract the JSON object passed to `DD_RUM.init(...)`, so tests assert on fields
    /// rather than on raw bytes that match by accident within the CDN URL ("us1" also
    /// appears in `/us1/v5/`).
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
        return serde_json::from_str(&text[start..end]).expect("init arg must be JSON");
    }

    #[test]
    fn default_major_version_selects_the_v6_cdn_url() {
        // No DD_RUM_MAJOR_VERSION configured, so the default applies. The URL shape is the one
        // documented by the browser-sdk: `/{region}/v{major}/datadog-rum.js`.
        let snippet = snippet_from_stable_config_with_reader(
            &crate::platform::tests::PopulatedReader,
            "browser-sdk",
            false,
            None,
        )
        .expect("should build snippet");

        let text = core::str::from_utf8(&snippet.bytes).expect("snippet is UTF-8");
        assert!(
            text.contains("https://www.datadoghq-browser-agent.com/us1/v6/datadog-rum.js"),
            "expected the v6 CDN URL, got:\n{text}"
        );
    }

    #[test]
    fn overlay_wins_over_populated_file_config() {
        use crate::configuration::RumConfiguration;

        let overlay = ConfigurationOverlay {
            rum: RumConfiguration {
                site: Some(String::from("us3.datadoghq.com")),
                ..Default::default()
            },
            ..Default::default()
        };
        let snippet = snippet_from_stable_config_with_reader(
            &crate::platform::tests::PopulatedReader,
            "browser-sdk",
            false,
            Some(overlay),
        )
        .expect("should build snippet");

        let init = extract_init_json(&snippet.bytes);
        assert_eq!(init["site"], Value::from("us3.datadoghq.com"));
        assert_eq!(init["applicationId"], Value::from("app-populated"));
        assert_eq!(init["clientToken"], Value::from("tok-populated"));
    }

    /// Reader whose files always exceed the configurator's size gate, which reclassifies
    /// the read as `Ok(empty)` with a `TooLarge` diagnostic in the logs. On Windows stderr
    /// is a no-op, so those diagnostics have to reach the error message.
    struct TooLargeReader;

    impl ConfigRead for TooLargeReader {
        fn read(&self, _path: &str) -> libdd_library_config::io::Result<alloc::vec::Vec<u8>> {
            return Err(libdd_library_config::io::ErrorKind::FileTooLarge.into());
        }
    }

    const DISABLED_CONFIG: &[u8] = br#"apm_configuration_default:
  DD_RUM_ENABLED: false
  DD_RUM_APPLICATION_ID: app-disabled
  DD_RUM_CLIENT_TOKEN: tok-disabled
  DD_RUM_REMOTE_CONFIGURATION_ID: rc-disabled
"#;

    const INVALID_ENABLED_CONFIG: &[u8] = br#"apm_configuration_default:
  DD_RUM_ENABLED: not-a-boolean
  DD_RUM_APPLICATION_ID: app-enabled
  DD_RUM_CLIENT_TOKEN: tok-enabled
"#;

    const UNRELATED_CONFIG: &[u8] = br#"apm_configuration_default:
  DD_SERVICE: unrelated-service
"#;

    struct ContentReader(Option<&'static [u8]>);

    impl ConfigRead for ContentReader {
        fn read(&self, path: &str) -> libdd_library_config::io::Result<alloc::vec::Vec<u8>> {
            if path == libdd_library_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH {
                if let Some(contents) = self.0 {
                    return Ok(contents.into());
                }
            }
            return Err(libdd_library_config::io::ErrorKind::NotFound.into());
        }
    }

    #[test]
    fn stable_config_reports_enabled_false_and_ids() {
        let snippet = snippet_from_stable_config_with_reader(
            &ContentReader(Some(DISABLED_CONFIG)),
            "browser-sdk",
            false,
            None,
        )
        .expect("snippet should build");

        assert_eq!(snippet.enabled, Some(false));
        assert_eq!(
            snippet.application_id.as_deref(),
            Some("app-disabled"),
            "application_id must be reported for host telemetry tagging"
        );
        assert_eq!(
            snippet.remote_configuration_id.as_deref(),
            Some("rc-disabled")
        );
        assert!(snippet.bytes.is_empty());
    }

    #[test]
    fn disabled_config_skips_snippet_validation() {
        for (contents, expected_remote_id) in [
            (
                &br#"apm_configuration_default:
  DD_RUM_ENABLED: false
  DD_RUM_REMOTE_CONFIGURATION_ID: rc-disabled
"#[..],
                Some("rc-disabled"),
            ),
            (
                &br#"apm_configuration_default:
  DD_RUM_ENABLED: false
  DD_RUM_SESSION_SAMPLE_RATE: not-a-number
"#[..],
                None,
            ),
        ] {
            let snippet = snippet_from_stable_config_with_reader(
                &ContentReader(Some(contents)),
                "browser-sdk",
                false,
                None,
            )
            .expect("disabled configuration should not validate SDK options");

            assert_eq!(snippet.enabled, Some(false));
            assert!(snippet.bytes.is_empty());
            assert_eq!(
                snippet.remote_configuration_id.as_deref(),
                expected_remote_id
            );
        }
    }

    #[test]
    fn empty_existing_config_is_not_reported_as_absent() {
        let reader = ContentReader(Some(b""));
        let error = snippet_from_stable_config_with_reader(&reader, "browser-sdk", false, None)
            .expect_err("an existing empty file should be reported as unusable");

        assert_eq!(error.code(), 6);
    }

    #[test]
    fn overlay_can_override_stable_config_enabled() {
        let overlay = ConfigurationOverlay {
            enabled: Some(true),
            ..Default::default()
        };
        let snippet = snippet_from_stable_config_with_reader(
            &ContentReader(Some(DISABLED_CONFIG)),
            "browser-sdk",
            false,
            Some(overlay),
        )
        .expect("should build snippet");

        assert_eq!(snippet.enabled, Some(true));
    }

    #[test]
    fn overlay_enabled_wins_before_the_file_value_is_parsed() {
        for overlay_enabled in [false, true] {
            let overlay = ConfigurationOverlay {
                enabled: Some(overlay_enabled),
                ..Default::default()
            };
            let snippet = snippet_from_stable_config_with_reader(
                &ContentReader(Some(INVALID_ENABLED_CONFIG)),
                "browser-sdk",
                false,
                Some(overlay),
            )
            .expect("the overlay should replace an invalid lower-priority file value");

            assert_eq!(snippet.enabled, Some(overlay_enabled));
            assert_eq!(snippet.bytes.is_empty(), !overlay_enabled);
        }
    }

    #[test]
    fn unrelated_stable_configuration_is_empty_for_rum() {
        let read = crate::platform::read_stable_configs_with_reader(
            &ContentReader(Some(UNRELATED_CONFIG)),
            "browser-sdk",
            false,
        )
        .expect("the unrelated stable configuration should parse");
        assert!(
            !read.configs.is_empty(),
            "the configurator should preserve the unrelated entry"
        );

        let config = stable_config_from_read(read).expect("the read itself should succeed");
        assert!(config.is_empty());

        let error = config
            .build_snippet(None)
            .expect_err("unrelated stable configuration should be absent for RUM");
        assert_eq!(error.code(), 10);
    }

    struct LocalConfigFleetTooLargeReader;

    impl ConfigRead for LocalConfigFleetTooLargeReader {
        fn read(&self, path: &str) -> libdd_library_config::io::Result<alloc::vec::Vec<u8>> {
            if path == libdd_library_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH {
                return Ok(DISABLED_CONFIG.into());
            }
            if path == libdd_library_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH {
                return Err(libdd_library_config::io::ErrorKind::FileTooLarge.into());
            }
            return Err(libdd_library_config::io::ErrorKind::NotFound.into());
        }
    }

    struct CountingReader {
        reads: core::cell::Cell<usize>,
    }

    impl ConfigRead for CountingReader {
        fn read(&self, path: &str) -> libdd_library_config::io::Result<alloc::vec::Vec<u8>> {
            self.reads.set(self.reads.get() + 1);
            return ContentReader(Some(DISABLED_CONFIG)).read(path);
        }
    }

    #[test]
    fn one_read_serves_many_snippets() {
        let reader = CountingReader {
            reads: core::cell::Cell::new(0),
        };
        let read = crate::platform::read_stable_configs_with_reader(&reader, "browser-sdk", false)
            .expect("read must succeed");
        let after_read = reader.reads.get();
        assert!(after_read > 0, "the files should have been read once");

        let config = StableConfig {
            configs: read.configs,
            logs: read.logs,
            empty_file_present: read.empty_file_present,
        };

        for _ in 0..5 {
            config
                .build_snippet(Some(ConfigurationOverlay::default()))
                .expect("should build snippet");
        }

        assert_eq!(
            reader.reads.get(),
            after_read,
            "building snippets from a StableConfig must not re-read the files"
        );
    }

    #[test]
    fn absent_stable_config_is_distinct_from_a_broken_one() {
        let err = snippet_from_stable_config_with_reader(
            &ContentReader(None),
            "browser-sdk",
            false,
            None,
        )
        .expect_err("no entries and no overlay must error");
        assert_eq!(
            err.code(),
            10,
            "absent stable config must not share code 6 with a broken one"
        );
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

    #[test]
    fn reclassified_read_failure_fails_before_creating_a_reusable_config() {
        let read =
            crate::platform::read_stable_configs_with_reader(&TooLargeReader, "browser-sdk", false)
                .expect("the configurator reclassifies TooLarge as an empty result");
        assert!(read.read_failed);

        let error = match stable_config_from_read(read) {
            Err(error) => error,
            Ok(_) => panic!("a reusable handle must expose the reclassified read failure"),
        };
        assert_eq!(error.code(), 6);
    }

    #[test]
    fn higher_priority_read_failure_rejects_lower_priority_entries() {
        let read = crate::platform::read_stable_configs_with_reader(
            &LocalConfigFleetTooLargeReader,
            "browser-sdk",
            false,
        )
        .expect("the configurator preserves entries from the readable source");
        assert!(read.read_failed);
        assert!(
            !read.configs.is_empty(),
            "the regression requires a partial result, not an empty one"
        );

        let error = match stable_config_from_read(read) {
            Err(error) => error,
            Ok(_) => panic!("a partial stable configuration must not be accepted"),
        };
        assert_eq!(error.code(), 6);
    }
}
