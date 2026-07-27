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

use crate::configuration::{Configuration, ConfigurationOverlay};
use crate::error::Error;
use crate::generate_snippet;

/// A snippet plus the host-relevant values resolved alongside it.
///
/// The generated JavaScript is only half of what a host needs: it also has to
/// know whether injection is switched on, and which application the snippet
/// belongs to so its own telemetry can be tagged. Those values live in the
/// configuration the snippet was built from, and returning them here is what
/// keeps hosts from having to re-read `application_monitoring.yaml` themselves.
#[cfg_attr(test, derive(Debug))]
pub struct StableConfigSnippet {
    /// The generated Browser SDK snippet.
    pub bytes: Vec<u8>,
    /// Resolved `DD_RUM_ENABLED`, or `None` when nothing configured it. Hosts
    /// own the default for `None`.
    pub enabled: Option<bool>,
    /// Resolved `applicationId`, for host-side telemetry tagging.
    pub application_id: Option<String>,
    /// Resolved `remoteConfigurationId`, for host-side telemetry tagging.
    pub remote_configuration_id: Option<String>,
}

/// Stable configuration read from disk, parsed once and reusable.
///
/// Reading is the expensive part -- two files off disk, YAML parsed, rules
/// matched -- and it does not depend on any overlay. Hosts that build several
/// snippets per configuration load (one per virtual host, say) read once into
/// this and then call [`StableConfig::build_snippet`] per overlay, instead of
/// paying for the files again each time.
pub struct StableConfig {
    configs: Vec<LibraryConfig>,
    logs: Vec<String>,
    files_present: bool,
}

impl StableConfig {
    /// Whether any stable-configuration entry matched this process. False means
    /// the files were absent or none of their rules matched -- the ordinary case
    /// for anyone not using stable configuration.
    pub fn is_empty(&self) -> bool {
        return self.configs.is_empty();
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
        // Distinguish "no config source at all" from "config source present but
        // missing required fields". Without this, the caller gets code 5
        // ("mandatory field 'applicationId' is empty") and spends time
        // troubleshooting file contents when the real issue is that no files
        // matched or existed.
        if self.is_empty() && overlay.is_none() {
            // Surface any diagnostics captured on the Ok(empty) path (e.g. a
            // file reclassified as TooLarge by the configurator) so Windows
            // callers, who never see stderr, still get the full context.
            let base = "no stable-config entries found; \
                 expected DD_RUM_* keys in application_monitoring.yaml \
                 (local or fleet-managed) or an overlay";
            let message = if self.logs.is_empty() {
                String::from(base)
            } else {
                alloc::format!("{base}\n{}", self.logs.join("\n"))
            };
            // A file that was there but yielded nothing (unreadable, oversized,
            // or matching no rule) is a real problem and keeps code 6, so hosts
            // that stay quiet about absent stable configuration still report it.
            if self.files_present {
                return Err(Error::StableConfig(message));
            }
            return Err(Error::StableConfigAbsent(message));
        }

        let mut config = Configuration::from_lookup(|key| {
            return self
                .configs
                .iter()
                .find(|c| return c.name == key)
                .map(|c| return c.value.as_str());
        })?;

        if let Some(o) = overlay {
            config.overlay(o);
        }

        return Ok(StableConfigSnippet {
            bytes: generate_snippet(&config)?,
            enabled: config.enabled,
            application_id: config.rum.application_id.clone(),
            remote_configuration_id: config.rum.remote_configuration_id.clone(),
        });
    }
}

/// Read the Datadog Agent's `application_monitoring.yaml` files (local and
/// fleet-managed) once, using the `DD_RUM_*` key set.
///
/// The `language` parameter identifies the current process's language (e.g.
/// `"browser-sdk"`) and is used for rule-based process matching in the
/// configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr on
/// Unix. On Windows stderr is a no-op (see [`crate::platform::write_stderr`]);
/// diagnostics are folded into error messages so they survive.
///
/// Absent files are *not* an error here -- the result is simply empty, and
/// [`StableConfig::build_snippet`] reports [`Error::StableConfigAbsent`] if
/// asked to build from nothing. Only a failed read or parse errors.
pub fn read_stable_config(language: &str, debug_logs: bool) -> Result<StableConfig, Error> {
    let read =
        crate::platform::read_stable_configs(language, debug_logs).map_err(Error::StableConfig)?;
    return Ok(StableConfig {
        configs: read.configs,
        logs: read.logs,
        files_present: read.files_present,
    });
}

/// Build a snippet from Datadog stable configuration files, with an optional overlay.
///
/// Convenience wrapper over [`read_stable_config`] followed by
/// [`StableConfig::build_snippet`]. Reads from disk on every call; hold a
/// [`StableConfig`] instead when building more than one snippet per
/// configuration load.
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
    return StableConfig {
        configs: read.configs,
        logs: read.logs,
        files_present: read.files_present,
    }
    .build_snippet(overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

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
        return serde_json::from_str(&text[start..end]).expect("init arg must be JSON");
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
            return Err(libdd_library_config::ConfigReadError::TooLarge);
        }
    }

    /// Reader serving stable config that turns RUM off while still carrying the
    /// RUM data keys. The snippet still builds -- whether to inject is the
    /// host's call -- but `enabled` must come back `false` so the host can
    /// honour it. Before `DD_RUM_ENABLED` was in the key table this was
    /// indistinguishable from "not configured", so hosts injected anyway.
    struct DisabledReader;

    impl ConfigRead for DisabledReader {
        type IoError = core::convert::Infallible;

        fn read(
            &self,
            path: &str,
        ) -> Result<alloc::vec::Vec<u8>, libdd_library_config::ConfigReadError<Self::IoError>>
        {
            if path == libdd_library_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH {
                return Ok(alloc::vec::Vec::from(
                    &br#"apm_configuration_default:
  DD_RUM_ENABLED: false
  DD_RUM_APPLICATION_ID: app-disabled
  DD_RUM_CLIENT_TOKEN: tok-disabled
  DD_RUM_REMOTE_CONFIGURATION_ID: rc-disabled
"#[..],
                ));
            }
            return Ok(alloc::vec::Vec::new());
        }
    }

    #[test]
    fn stable_config_reports_enabled_false_and_ids() {
        let snippet =
            snippet_from_stable_config_with_reader(&DisabledReader, "browser-sdk", false, None)
                .expect("snippet should still build; enabling is the host's decision");

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

        // The host-side switch must not leak into DD_RUM.init({...}).
        let init = extract_init_json(&snippet.bytes);
        assert!(
            init.get("enabled").is_none(),
            "enabled is a host switch, not a Browser SDK init option: {init}"
        );
    }

    #[test]
    fn overlay_can_override_stable_config_enabled() {
        let overlay = ConfigurationOverlay {
            enabled: Some(true),
            ..Default::default()
        };
        let snippet = snippet_from_stable_config_with_reader(
            &DisabledReader,
            "browser-sdk",
            false,
            Some(overlay),
        )
        .expect("should build snippet");

        assert_eq!(snippet.enabled, Some(true));
    }

    /// Counts how many times the configuration files are actually read, so a
    /// test can prove reuse rather than infer it.
    struct CountingReader {
        reads: core::cell::Cell<usize>,
    }

    impl ConfigRead for CountingReader {
        type IoError = core::convert::Infallible;

        fn read(
            &self,
            path: &str,
        ) -> Result<alloc::vec::Vec<u8>, libdd_library_config::ConfigReadError<Self::IoError>>
        {
            self.reads.set(self.reads.get() + 1);
            return DisabledReader.read(path);
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
            files_present: read.files_present,
        };

        // A host with several virtual hosts builds one snippet per overlay. None
        // of them may touch the filesystem again.
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

    /// Reader for a host with no stable configuration at all.
    struct AbsentReader;

    impl ConfigRead for AbsentReader {
        type IoError = core::convert::Infallible;

        fn read(
            &self,
            _path: &str,
        ) -> Result<alloc::vec::Vec<u8>, libdd_library_config::ConfigReadError<Self::IoError>>
        {
            return Err(libdd_library_config::ConfigReadError::NotFound);
        }
    }

    #[test]
    fn absent_stable_config_is_distinct_from_a_broken_one() {
        // Nothing on disk at all: code 10, so a host can stay quiet about it.
        let err = snippet_from_stable_config_with_reader(&AbsentReader, "browser-sdk", false, None)
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
        // Code 6, not the absent code 10: a file that was there but unusable is
        // a real problem, and hosts that stay quiet about 10 must still report
        // this one.
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
