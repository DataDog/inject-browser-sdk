// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use crate::configuration::{validate_configuration, Configuration, PrivacyLevel, RumConfiguration};
use crate::error::Error;

/// Metadata describing how to read a config option from DD_* entries.
#[derive(Clone, Copy, Debug)]
struct ConfigOptionMeta {
    primary_key: &'static str,
    fallback_key: Option<&'static str>,
}

impl ConfigOptionMeta {
    const fn key(primary: &'static str) -> Self {
        Self {
            primary_key: primary,
            fallback_key: None,
        }
    }
    const fn key_fb(primary: &'static str, fallback: &'static str) -> Self {
        Self {
            primary_key: primary,
            fallback_key: Some(fallback),
        }
    }
}

/// A known configuration option for RUM injection.
///
/// Each variant maps to one or more `DD_*` environment-variable keys used in
/// Datadog stable configuration files. Use [`ConfigOption::key`] to obtain the
/// primary key name at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ConfigOption {
    ApplicationId = 0,
    ClientToken = 1,
    Site = 2,
    Service = 3,
    Environment = 4,
    Version = 5,
    TrackUserInteractions = 6,
    TrackResources = 7,
    TrackLongTasks = 8,
    DefaultPrivacyLevel = 9,
    SessionSampleRate = 10,
    SessionReplaySampleRate = 11,
    MajorVersion = 12,
    RemoteConfigurationId = 13,
}

use ConfigOption::*;

impl ConfigOption {
    /// All known config options.
    pub const ALL: &[ConfigOption] = &[
        ApplicationId,
        ClientToken,
        Site,
        Service,
        Environment,
        Version,
        TrackUserInteractions,
        TrackResources,
        TrackLongTasks,
        DefaultPrivacyLevel,
        SessionSampleRate,
        SessionReplaySampleRate,
        MajorVersion,
        RemoteConfigurationId,
    ];

    /// The primary `DD_*` key name for this option.
    pub const fn key(&self) -> &'static str {
        self.meta().primary_key
    }

    const fn meta(&self) -> ConfigOptionMeta {
        match self {
            ApplicationId => ConfigOptionMeta::key("DD_RUM_APPLICATION_ID"),
            ClientToken => ConfigOptionMeta::key("DD_RUM_CLIENT_TOKEN"),
            Site => ConfigOptionMeta::key_fb("DD_RUM_SITE", "DD_SITE"),
            Service => ConfigOptionMeta::key_fb("DD_RUM_SERVICE", "DD_SERVICE"),
            Environment => ConfigOptionMeta::key_fb("DD_RUM_ENVIRONMENT", "DD_ENV"),
            Version => ConfigOptionMeta::key("DD_VERSION"),
            TrackUserInteractions => ConfigOptionMeta::key("DD_RUM_TRACK_USER_INTERACTIONS"),
            TrackResources => ConfigOptionMeta::key("DD_RUM_TRACK_RESOURCES"),
            TrackLongTasks => ConfigOptionMeta::key("DD_RUM_TRACK_LONG_TASKS"),
            DefaultPrivacyLevel => ConfigOptionMeta::key("DD_RUM_DEFAULT_PRIVACY_LEVEL"),
            SessionSampleRate => ConfigOptionMeta::key("DD_RUM_SESSION_SAMPLE_RATE"),
            SessionReplaySampleRate => ConfigOptionMeta::key("DD_RUM_SESSION_REPLAY_SAMPLE_RATE"),
            MajorVersion => ConfigOptionMeta::key("DD_RUM_MAJOR_VERSION"),
            RemoteConfigurationId => ConfigOptionMeta::key("DD_RUM_REMOTE_CONFIGURATION_ID"),
        }
    }

    /// Resolve the value for this option using a lookup function.
    /// Tries the primary key first, then the fallback key if present.
    fn resolve<'a>(&self, lookup: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
        let meta = self.meta();
        if let Some(v) = lookup(meta.primary_key) {
            return Some(v);
        }
        if let Some(fallback) = meta.fallback_key {
            return lookup(fallback);
        }
        None
    }
}

// --- Typed parse helpers ---

fn parse_bool(raw: &str, key: &str) -> Result<bool, Error> {
    match raw.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::StableConfig(format!(
            "Invalid boolean value '{raw}' for key '{key}'"
        ))),
    }
}

fn parse_u32(raw: &str, key: &str) -> Result<u32, Error> {
    raw.parse()
        .map_err(|_| Error::StableConfig(format!("Invalid u32 value '{raw}' for key '{key}'")))
}

fn parse_f32(raw: &str, key: &str) -> Result<f32, Error> {
    let val: f32 = raw
        .parse()
        .map_err(|_| Error::StableConfig(format!("Invalid float value '{raw}' for key '{key}'")))?;
    if !val.is_finite() {
        return Err(Error::StableConfig(format!(
            "Invalid float value '{raw}' for key '{key}'"
        )));
    }
    Ok(val)
}

fn parse_privacy(raw: &str, key: &str) -> Result<PrivacyLevel, Error> {
    match raw.to_ascii_lowercase().as_str() {
        "allow" => Ok(PrivacyLevel::Allow),
        "mask" => Ok(PrivacyLevel::Mask),
        "mask-user-input" => Ok(PrivacyLevel::MaskUserInput),
        _ => Err(Error::StableConfig(format!(
            "Invalid privacy level '{raw}' for key '{key}'"
        ))),
    }
}

/// Build a [`Configuration`] by resolving known config options via a lookup function.
///
/// The `lookup` closure takes a DD_* key name and returns the value if present.
/// This drives extraction directly from any source (e.g., `Vec<LibraryConfig>`,
/// `BTreeMap`, C entry arrays) without an intermediate representation.
pub(crate) fn build_configuration<'a>(
    lookup: impl Fn(&str) -> Option<&'a str>,
) -> Result<Configuration, Error> {
    let resolve = |opt: ConfigOption| opt.resolve(&lookup);

    let require_str = |opt: ConfigOption| -> Result<&'a str, Error> {
        match resolve(opt) {
            Some(v) if !v.is_empty() => Ok(v),
            _ => Err(Error::EmptyMandatoryConf(String::from(
                opt.meta().primary_key,
            ))),
        }
    };

    let opt_bool = |opt: ConfigOption| -> Result<Option<bool>, Error> {
        resolve(opt)
            .map(|v| parse_bool(v, opt.meta().primary_key))
            .transpose()
    };

    let opt_f32 = |opt: ConfigOption| -> Result<Option<f32>, Error> {
        resolve(opt)
            .map(|v| parse_f32(v, opt.meta().primary_key))
            .transpose()
    };

    let configuration = Configuration {
        major_version: match resolve(MajorVersion) {
            Some(v) => parse_u32(v, MajorVersion.meta().primary_key)?,
            None => 5,
        },
        rum: RumConfiguration {
            application_id: Box::from(require_str(ApplicationId)?),
            client_token: Box::from(require_str(ClientToken)?),
            site: resolve(Site).map(Box::from),
            service: resolve(Service).map(Box::from),
            env: resolve(Environment).map(Box::from),
            version: resolve(Version).map(Box::from),
            track_user_interactions: opt_bool(TrackUserInteractions)?,
            track_resources: opt_bool(TrackResources)?,
            track_long_task: opt_bool(TrackLongTasks)?,
            default_privacy_level: resolve(DefaultPrivacyLevel)
                .map(|v| parse_privacy(v, DefaultPrivacyLevel.meta().primary_key))
                .transpose()?,
            session_sample_rate: opt_f32(SessionSampleRate)?,
            session_replay_sample_rate: opt_f32(SessionReplaySampleRate)?,
            remote_configuration_id: resolve(RemoteConfigurationId).map(Box::from),
            other: Default::default(),
        },
    };

    validate_configuration(&configuration)?;
    Ok(configuration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use pretty_assertions::assert_eq;

    fn lookup_from_map<'a>(map: &'a BTreeMap<String, String>) -> impl Fn(&str) -> Option<&'a str> {
        |key| map.get(key).map(|v| v.as_str())
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_bool("True", "k").unwrap(), true);
        assert_eq!(parse_bool("FALSE", "k").unwrap(), false);
        assert!(parse_bool("yes", "k").is_err());

        assert_eq!(parse_u32("42", "k").unwrap(), 42);
        assert!(parse_u32("abc", "k").is_err());

        assert!((parse_f32("42.5", "k").unwrap() - 42.5).abs() < f32::EPSILON);
        assert!(parse_f32("inf", "k").is_err());
        assert!(parse_f32("NaN", "k").is_err());

        assert_eq!(parse_privacy("allow", "k").unwrap(), PrivacyLevel::Allow);
        assert_eq!(
            parse_privacy("mask-user-input", "k").unwrap(),
            PrivacyLevel::MaskUserInput
        );
        assert!(parse_privacy("none", "k").is_err());
    }

    #[test]
    fn resolve_primary_over_fallback() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_SITE"), String::from("datadoghq.com")),
            (String::from("DD_SITE"), String::from("datadoghq.eu")),
        ]);
        assert_eq!(Site.resolve(lookup_from_map(&map)), Some("datadoghq.com"));
    }

    #[test]
    fn resolve_fallback() {
        let map = BTreeMap::from([(String::from("DD_SITE"), String::from("datadoghq.eu"))]);
        assert_eq!(Site.resolve(lookup_from_map(&map)), Some("datadoghq.eu"));
    }

    #[test]
    fn resolve_missing() {
        let map: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(Site.resolve(lookup_from_map(&map)), None);
    }

    #[test]
    fn build_configuration_minimal() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
        ]);
        let config = build_configuration(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.application_id.as_ref(), "app-1");
        assert_eq!(config.rum.client_token.as_ref(), "tok-1");
        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.site, None);
    }

    #[test]
    fn build_configuration_full() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_RUM_SITE"), String::from("datadoghq.com")),
            (String::from("DD_RUM_SERVICE"), String::from("my-svc")),
            (String::from("DD_RUM_ENVIRONMENT"), String::from("prod")),
            (String::from("DD_VERSION"), String::from("1.0")),
            (
                String::from("DD_RUM_TRACK_USER_INTERACTIONS"),
                String::from("true"),
            ),
            (
                String::from("DD_RUM_TRACK_RESOURCES"),
                String::from("false"),
            ),
            (
                String::from("DD_RUM_TRACK_LONG_TASKS"),
                String::from("true"),
            ),
            (
                String::from("DD_RUM_DEFAULT_PRIVACY_LEVEL"),
                String::from("mask"),
            ),
            (
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("80.5"),
            ),
            (
                String::from("DD_RUM_SESSION_REPLAY_SAMPLE_RATE"),
                String::from("50"),
            ),
            (String::from("DD_RUM_MAJOR_VERSION"), String::from("6")),
            (
                String::from("DD_RUM_REMOTE_CONFIGURATION_ID"),
                String::from("rc-123"),
            ),
        ]);
        let config = build_configuration(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.application_id.as_ref(), "app-1");
        assert_eq!(config.rum.client_token.as_ref(), "tok-1");
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(config.rum.service.as_deref(), Some("my-svc"));
        assert_eq!(config.rum.env.as_deref(), Some("prod"));
        assert_eq!(config.rum.version.as_deref(), Some("1.0"));
        assert_eq!(config.rum.track_user_interactions, Some(true));
        assert_eq!(config.rum.track_resources, Some(false));
        assert_eq!(config.rum.track_long_task, Some(true));
        assert_eq!(config.rum.default_privacy_level, Some(PrivacyLevel::Mask));
        assert_eq!(config.rum.session_sample_rate, Some(80.5));
        assert_eq!(config.rum.session_replay_sample_rate, Some(50.0));
        assert_eq!(config.major_version, 6);
        assert_eq!(
            config.rum.remote_configuration_id.as_deref(),
            Some("rc-123")
        );
    }

    #[test]
    fn build_configuration_precedence() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_RUM_SITE"), String::from("datadoghq.com")),
            (String::from("DD_SITE"), String::from("datadoghq.eu")),
            (String::from("DD_RUM_SERVICE"), String::from("rum-svc")),
            (String::from("DD_SERVICE"), String::from("generic-svc")),
            (String::from("DD_RUM_ENVIRONMENT"), String::from("staging")),
            (String::from("DD_ENV"), String::from("production")),
        ]);
        let config = build_configuration(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(config.rum.service.as_deref(), Some("rum-svc"));
        assert_eq!(config.rum.env.as_deref(), Some("staging"));
    }

    #[test]
    fn build_configuration_missing_application_id() {
        let map = BTreeMap::from([(String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1"))]);
        let err = build_configuration(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 5);
    }

    #[test]
    fn build_configuration_empty_application_id() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
        ]);
        let err = build_configuration(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 5);
    }

    #[test]
    fn build_configuration_invalid_bool() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_RUM_TRACK_RESOURCES"), String::from("yes")),
        ]);
        let err = build_configuration(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 6);
    }

    #[test]
    fn all_contains_all_variants() {
        assert_eq!(ConfigOption::ALL.len(), 14);
    }

    #[test]
    fn parse_privacy_case_insensitive() {
        assert_eq!(parse_privacy("MASK", "k").unwrap(), PrivacyLevel::Mask);
        assert_eq!(parse_privacy("Allow", "k").unwrap(), PrivacyLevel::Allow);
        assert_eq!(
            parse_privacy("Mask-User-Input", "k").unwrap(),
            PrivacyLevel::MaskUserInput
        );
    }

    #[test]
    fn build_configuration_fallback_only() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_SITE"), String::from("datadoghq.eu")),
            (String::from("DD_SERVICE"), String::from("my-svc")),
            (String::from("DD_ENV"), String::from("prod")),
        ]);
        let config = build_configuration(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.eu"));
        assert_eq!(config.rum.service.as_deref(), Some("my-svc"));
        assert_eq!(config.rum.env.as_deref(), Some("prod"));
    }

    #[test]
    fn build_configuration_invalid_float() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("abc"),
            ),
        ]);
        let err = build_configuration(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 6);
    }

    #[test]
    fn build_configuration_out_of_range_rate() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("105.0"),
            ),
        ]);
        let err = build_configuration(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 4);
    }
}
