// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use alloc::collections::BTreeMap as Map;
use alloc::format;

use crate::configuration::{validate_configuration, Configuration, RumConfiguration};
use crate::error::Error;
use serde_json::Value;

/// The type of value a config option expects.
#[derive(Clone, Copy, Debug)]
enum ConfigValueType {
    Str,
    Bool,
    F64,
    U32,
    Privacy,
}

/// Metadata describing how to read a config option from DD_* entries and
/// how to map it to a RUM configuration field.
#[derive(Clone, Copy, Debug)]
struct ConfigOptionMeta {
    primary_key: &'static str,
    fallback_key: Option<&'static str>,
    rum_field: &'static str,
    value_type: ConfigValueType,
}

impl ConfigOptionMeta {
    const fn new(
        primary: &'static str,
        rum_field: &'static str,
        value_type: ConfigValueType,
    ) -> Self {
        Self {
            primary_key: primary,
            fallback_key: None,
            rum_field,
            value_type,
        }
    }
    const fn with_fallback(
        primary: &'static str,
        fallback: &'static str,
        rum_field: &'static str,
        value_type: ConfigValueType,
    ) -> Self {
        Self {
            primary_key: primary,
            fallback_key: Some(fallback),
            rum_field,
            value_type,
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
use ConfigValueType::*;

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

    /// Parse a raw `u8` into a [`ConfigOption`]. Returns `None` for unknown values.
    /// Use this at FFI boundaries before materializing a `ConfigOption` from caller input.
    pub const fn from_u8(n: u8) -> Option<ConfigOption> {
        match n {
            0 => Some(ApplicationId),
            1 => Some(ClientToken),
            2 => Some(Site),
            3 => Some(Service),
            4 => Some(Environment),
            5 => Some(Version),
            6 => Some(TrackUserInteractions),
            7 => Some(TrackResources),
            8 => Some(TrackLongTasks),
            9 => Some(DefaultPrivacyLevel),
            10 => Some(SessionSampleRate),
            11 => Some(SessionReplaySampleRate),
            12 => Some(MajorVersion),
            13 => Some(RemoteConfigurationId),
            _ => None,
        }
    }

    const fn meta(&self) -> ConfigOptionMeta {
        match self {
            ApplicationId => ConfigOptionMeta::new("DD_RUM_APPLICATION_ID", "applicationId", Str),
            ClientToken => ConfigOptionMeta::new("DD_RUM_CLIENT_TOKEN", "clientToken", Str),
            Site => ConfigOptionMeta::with_fallback("DD_RUM_SITE", "DD_SITE", "site", Str),
            Service => {
                ConfigOptionMeta::with_fallback("DD_RUM_SERVICE", "DD_SERVICE", "service", Str)
            }
            Environment => {
                ConfigOptionMeta::with_fallback("DD_RUM_ENVIRONMENT", "DD_ENV", "env", Str)
            }
            Version => ConfigOptionMeta::new("DD_VERSION", "version", Str),
            TrackUserInteractions => ConfigOptionMeta::new(
                "DD_RUM_TRACK_USER_INTERACTIONS",
                "trackUserInteractions",
                Bool,
            ),
            TrackResources => {
                ConfigOptionMeta::new("DD_RUM_TRACK_RESOURCES", "trackResources", Bool)
            }
            TrackLongTasks => {
                ConfigOptionMeta::new("DD_RUM_TRACK_LONG_TASKS", "trackLongTasks", Bool)
            }
            DefaultPrivacyLevel => ConfigOptionMeta::new(
                "DD_RUM_DEFAULT_PRIVACY_LEVEL",
                "defaultPrivacyLevel",
                Privacy,
            ),
            SessionSampleRate => {
                ConfigOptionMeta::new("DD_RUM_SESSION_SAMPLE_RATE", "sessionSampleRate", F64)
            }
            SessionReplaySampleRate => ConfigOptionMeta::new(
                "DD_RUM_SESSION_REPLAY_SAMPLE_RATE",
                "sessionReplaySampleRate",
                F64,
            ),
            MajorVersion => ConfigOptionMeta::new("DD_RUM_MAJOR_VERSION", "majorVersion", U32),
            RemoteConfigurationId => ConfigOptionMeta::new(
                "DD_RUM_REMOTE_CONFIGURATION_ID",
                "remoteConfigurationId",
                Str,
            ),
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

fn parse_f64(raw: &str, key: &str) -> Result<f64, Error> {
    let val: f64 = raw
        .parse()
        .map_err(|_| Error::StableConfig(format!("Invalid float value '{raw}' for key '{key}'")))?;
    if !val.is_finite() {
        return Err(Error::StableConfig(format!(
            "Invalid float value '{raw}' for key '{key}'"
        )));
    }
    Ok(val)
}

fn parse_privacy(raw: &str, key: &str) -> Result<&'static str, Error> {
    match raw.to_ascii_lowercase().as_str() {
        "allow" => Ok("allow"),
        "mask" => Ok("mask"),
        "mask-user-input" => Ok("mask-user-input"),
        _ => Err(Error::StableConfig(format!(
            "Invalid privacy level '{raw}' for key '{key}'"
        ))),
    }
}

/// Parse a raw string value into a [`serde_json::Value`] according to the option's type.
fn parse_value(raw: &str, opt: ConfigOption) -> Result<Value, Error> {
    let meta = opt.meta();
    let key = meta.primary_key;
    match meta.value_type {
        Str => Ok(Value::String(raw.into())),
        Bool => parse_bool(raw, key).map(Value::Bool),
        F64 => {
            let v = parse_f64(raw, key)?;
            Ok(Value::Number(
                serde_json::Number::from_f64(v)
                    .expect("parse_f64 already rejects non-finite values"),
            ))
        }
        U32 => {
            let v = parse_u32(raw, key)?;
            Ok(Value::Number(v.into()))
        }
        Privacy => parse_privacy(raw, key).map(|s| Value::String(s.into())),
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
    let mut fields: Map<Box<str>, Value> = Map::new();

    for &opt in ConfigOption::ALL {
        if let Some(raw) = opt.resolve(&lookup) {
            let value = parse_value(raw, opt)?;
            fields.insert(Box::from(opt.meta().rum_field), value);
        }
    }

    // TODO(INPLAT-1104): support unknown DD_RUM_* keys as pass-through string values.

    let major_version = fields
        .remove(MajorVersion.meta().rum_field)
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u32;

    let config = Configuration {
        major_version,
        rum: RumConfiguration { fields },
    };
    validate_configuration(&config)?;
    Ok(config)
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

        assert!((parse_f64("42.5", "k").unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(parse_f64("inf", "k").is_err());
        assert!(parse_f64("NaN", "k").is_err());

        assert_eq!(parse_privacy("allow", "k").unwrap(), "allow");
        assert_eq!(
            parse_privacy("mask-user-input", "k").unwrap(),
            "mask-user-input"
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
        assert_eq!(
            config
                .rum
                .fields
                .get("applicationId")
                .and_then(|v| v.as_str()),
            Some("app-1")
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("clientToken")
                .and_then(|v| v.as_str()),
            Some("tok-1")
        );
        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.fields.get("site"), None);
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
        assert_eq!(
            config
                .rum
                .fields
                .get("applicationId")
                .and_then(|v| v.as_str()),
            Some("app-1")
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("clientToken")
                .and_then(|v| v.as_str()),
            Some("tok-1")
        );
        assert_eq!(
            config.rum.fields.get("site").and_then(|v| v.as_str()),
            Some("datadoghq.com")
        );
        assert_eq!(
            config.rum.fields.get("service").and_then(|v| v.as_str()),
            Some("my-svc")
        );
        assert_eq!(
            config.rum.fields.get("env").and_then(|v| v.as_str()),
            Some("prod")
        );
        assert_eq!(
            config.rum.fields.get("version").and_then(|v| v.as_str()),
            Some("1.0")
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("trackUserInteractions")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("trackResources")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("trackLongTasks")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("defaultPrivacyLevel")
                .and_then(|v| v.as_str()),
            Some("mask")
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("sessionSampleRate")
                .and_then(|v| v.as_f64()),
            Some(80.5)
        );
        assert_eq!(
            config
                .rum
                .fields
                .get("sessionReplaySampleRate")
                .and_then(|v| v.as_f64()),
            Some(50.0)
        );
        assert_eq!(config.major_version, 6);
        assert_eq!(
            config
                .rum
                .fields
                .get("remoteConfigurationId")
                .and_then(|v| v.as_str()),
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
        assert_eq!(
            config.rum.fields.get("site").and_then(|v| v.as_str()),
            Some("datadoghq.com")
        );
        assert_eq!(
            config.rum.fields.get("service").and_then(|v| v.as_str()),
            Some("rum-svc")
        );
        assert_eq!(
            config.rum.fields.get("env").and_then(|v| v.as_str()),
            Some("staging")
        );
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
        // Ensure ALL is updated when new variants are added to the enum.
        assert_eq!(ConfigOption::ALL.len(), RemoteConfigurationId as usize + 1);
    }

    #[test]
    fn from_u8_round_trip() {
        for &opt in ConfigOption::ALL {
            assert_eq!(ConfigOption::from_u8(opt as u8), Some(opt));
        }
    }

    #[test]
    fn from_u8_rejects_unknown() {
        assert_eq!(ConfigOption::from_u8(14), None);
        assert_eq!(ConfigOption::from_u8(255), None);
    }

    #[test]
    fn parse_privacy_case_insensitive() {
        assert_eq!(parse_privacy("MASK", "k").unwrap(), "mask");
        assert_eq!(parse_privacy("Allow", "k").unwrap(), "allow");
        assert_eq!(
            parse_privacy("Mask-User-Input", "k").unwrap(),
            "mask-user-input"
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
        assert_eq!(
            config.rum.fields.get("site").and_then(|v| v.as_str()),
            Some("datadoghq.eu")
        );
        assert_eq!(
            config.rum.fields.get("service").and_then(|v| v.as_str()),
            Some("my-svc")
        );
        assert_eq!(
            config.rum.fields.get("env").and_then(|v| v.as_str()),
            Some("prod")
        );
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
