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

/// The type of value a config option expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Str,
    Bool,
    U32,
    F32,
    PrivacyLevel,
}

/// Metadata describing how to read a config option from DD_* entries.
#[derive(Clone, Copy, Debug)]
pub struct ConfigOptionMeta {
    pub primary_key: &'static str,
    pub fallback_key: Option<&'static str>,
    pub required: bool,
    pub value_type: ValueType,
}

/// A known DD_* configuration option for RUM injection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigOption {
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
}

impl ConfigOption {
    pub const ALL: &[ConfigOption] = &[
        ConfigOption::ApplicationId,
        ConfigOption::ClientToken,
        ConfigOption::Site,
        ConfigOption::Service,
        ConfigOption::Environment,
        ConfigOption::Version,
        ConfigOption::TrackUserInteractions,
        ConfigOption::TrackResources,
        ConfigOption::TrackLongTasks,
        ConfigOption::DefaultPrivacyLevel,
        ConfigOption::SessionSampleRate,
        ConfigOption::SessionReplaySampleRate,
        ConfigOption::MajorVersion,
        ConfigOption::RemoteConfigurationId,
    ];

    pub const fn meta(&self) -> ConfigOptionMeta {
        match self {
            Self::ApplicationId => ConfigOptionMeta {
                primary_key: "DD_RUM_APPLICATION_ID",
                fallback_key: None,
                required: true,
                value_type: ValueType::Str,
            },
            Self::ClientToken => ConfigOptionMeta {
                primary_key: "DD_RUM_CLIENT_TOKEN",
                fallback_key: None,
                required: true,
                value_type: ValueType::Str,
            },
            Self::Site => ConfigOptionMeta {
                primary_key: "DD_RUM_SITE",
                fallback_key: Some("DD_SITE"),
                required: false,
                value_type: ValueType::Str,
            },
            Self::Service => ConfigOptionMeta {
                primary_key: "DD_RUM_SERVICE",
                fallback_key: Some("DD_SERVICE"),
                required: false,
                value_type: ValueType::Str,
            },
            Self::Environment => ConfigOptionMeta {
                primary_key: "DD_RUM_ENVIRONMENT",
                fallback_key: Some("DD_ENV"),
                required: false,
                value_type: ValueType::Str,
            },
            Self::Version => ConfigOptionMeta {
                primary_key: "DD_VERSION",
                fallback_key: None,
                required: false,
                value_type: ValueType::Str,
            },
            Self::TrackUserInteractions => ConfigOptionMeta {
                primary_key: "DD_RUM_TRACK_USER_INTERACTIONS",
                fallback_key: None,
                required: false,
                value_type: ValueType::Bool,
            },
            Self::TrackResources => ConfigOptionMeta {
                primary_key: "DD_RUM_TRACK_RESOURCES",
                fallback_key: None,
                required: false,
                value_type: ValueType::Bool,
            },
            Self::TrackLongTasks => ConfigOptionMeta {
                primary_key: "DD_RUM_TRACK_LONG_TASKS",
                fallback_key: None,
                required: false,
                value_type: ValueType::Bool,
            },
            Self::DefaultPrivacyLevel => ConfigOptionMeta {
                primary_key: "DD_RUM_DEFAULT_PRIVACY_LEVEL",
                fallback_key: None,
                required: false,
                value_type: ValueType::PrivacyLevel,
            },
            Self::SessionSampleRate => ConfigOptionMeta {
                primary_key: "DD_RUM_SESSION_SAMPLE_RATE",
                fallback_key: None,
                required: false,
                value_type: ValueType::F32,
            },
            Self::SessionReplaySampleRate => ConfigOptionMeta {
                primary_key: "DD_RUM_SESSION_REPLAY_SAMPLE_RATE",
                fallback_key: None,
                required: false,
                value_type: ValueType::F32,
            },
            Self::MajorVersion => ConfigOptionMeta {
                primary_key: "DD_RUM_MAJOR_VERSION",
                fallback_key: None,
                required: false,
                value_type: ValueType::U32,
            },
            Self::RemoteConfigurationId => ConfigOptionMeta {
                primary_key: "DD_RUM_REMOTE_CONFIGURATION_ID",
                fallback_key: None,
                required: false,
                value_type: ValueType::Str,
            },
        }
    }

    /// Resolve the value for this option using a lookup function.
    /// Tries the primary key first, then the fallback key if present.
    pub fn resolve<'a>(&self, lookup: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
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

/// A parsed configuration value.
pub enum ParsedValue<'a> {
    Str(&'a str),
    Bool(bool),
    U32(u32),
    F32(f32),
    Privacy(PrivacyLevel),
}

/// Parse a raw string value according to the expected type.
pub fn parse_value<'a>(
    raw: &'a str,
    value_type: ValueType,
    key_name: &str,
) -> Result<ParsedValue<'a>, Error> {
    match value_type {
        ValueType::Str => Ok(ParsedValue::Str(raw)),
        ValueType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" => Ok(ParsedValue::Bool(true)),
            "false" => Ok(ParsedValue::Bool(false)),
            _ => Err(Error::StableConfig(format!(
                "Invalid boolean value '{raw}' for key '{key_name}'"
            ))),
        },
        ValueType::U32 => {
            let val: u32 = raw.parse().map_err(|_| {
                Error::StableConfig(format!("Invalid u32 value '{raw}' for key '{key_name}'"))
            })?;
            Ok(ParsedValue::U32(val))
        }
        ValueType::F32 => {
            let val: f32 = raw.parse().map_err(|_| {
                Error::StableConfig(format!("Invalid float value '{raw}' for key '{key_name}'"))
            })?;
            if !val.is_finite() {
                return Err(Error::StableConfig(format!(
                    "Invalid float value '{raw}' for key '{key_name}'"
                )));
            }
            Ok(ParsedValue::F32(val))
        }
        ValueType::PrivacyLevel => match raw.to_ascii_lowercase().as_str() {
            "allow" => Ok(ParsedValue::Privacy(PrivacyLevel::Allow)),
            "mask" => Ok(ParsedValue::Privacy(PrivacyLevel::Mask)),
            "mask-user-input" => Ok(ParsedValue::Privacy(PrivacyLevel::MaskUserInput)),
            _ => Err(Error::StableConfig(format!(
                "Invalid privacy level '{raw}' for key '{key_name}'"
            ))),
        },
    }
}

/// Build a [`Configuration`] by resolving known config options via a lookup function.
///
/// The `lookup` closure takes a DD_* key name and returns the value if present.
/// This drives extraction directly from any source (e.g., `Vec<LibraryConfig>`,
/// `BTreeMap`, C entry arrays) without an intermediate representation.
pub fn build_configuration<'a>(
    lookup: impl Fn(&str) -> Option<&'a str>,
) -> Result<Configuration, Error> {
    let mut application_id: Option<&'a str> = None;
    let mut client_token: Option<&'a str> = None;
    let mut site: Option<&'a str> = None;
    let mut service: Option<&'a str> = None;
    let mut env: Option<&'a str> = None;
    let mut version: Option<&'a str> = None;
    let mut track_user_interactions: Option<bool> = None;
    let mut track_resources: Option<bool> = None;
    let mut track_long_task: Option<bool> = None;
    let mut default_privacy_level: Option<PrivacyLevel> = None;
    let mut session_sample_rate: Option<f32> = None;
    let mut session_replay_sample_rate: Option<f32> = None;
    let mut major_version: u32 = 5;
    let mut remote_configuration_id: Option<&'a str> = None;

    for option in ConfigOption::ALL {
        let meta = option.meta();
        let raw = match option.resolve(&lookup) {
            Some(v) => v,
            None => {
                if meta.required {
                    return Err(Error::EmptyMandatoryConf(String::from(meta.primary_key)));
                }
                continue;
            }
        };

        if meta.required && raw.is_empty() {
            return Err(Error::EmptyMandatoryConf(String::from(meta.primary_key)));
        }

        let parsed = parse_value(raw, meta.value_type, meta.primary_key)?;

        match option {
            ConfigOption::ApplicationId => {
                if let ParsedValue::Str(v) = parsed {
                    application_id = Some(v);
                }
            }
            ConfigOption::ClientToken => {
                if let ParsedValue::Str(v) = parsed {
                    client_token = Some(v);
                }
            }
            ConfigOption::Site => {
                if let ParsedValue::Str(v) = parsed {
                    site = Some(v);
                }
            }
            ConfigOption::Service => {
                if let ParsedValue::Str(v) = parsed {
                    service = Some(v);
                }
            }
            ConfigOption::Environment => {
                if let ParsedValue::Str(v) = parsed {
                    env = Some(v);
                }
            }
            ConfigOption::Version => {
                if let ParsedValue::Str(v) = parsed {
                    version = Some(v);
                }
            }
            ConfigOption::TrackUserInteractions => {
                if let ParsedValue::Bool(v) = parsed {
                    track_user_interactions = Some(v);
                }
            }
            ConfigOption::TrackResources => {
                if let ParsedValue::Bool(v) = parsed {
                    track_resources = Some(v);
                }
            }
            ConfigOption::TrackLongTasks => {
                if let ParsedValue::Bool(v) = parsed {
                    track_long_task = Some(v);
                }
            }
            ConfigOption::DefaultPrivacyLevel => {
                if let ParsedValue::Privacy(v) = parsed {
                    default_privacy_level = Some(v);
                }
            }
            ConfigOption::SessionSampleRate => {
                if let ParsedValue::F32(v) = parsed {
                    session_sample_rate = Some(v);
                }
            }
            ConfigOption::SessionReplaySampleRate => {
                if let ParsedValue::F32(v) = parsed {
                    session_replay_sample_rate = Some(v);
                }
            }
            ConfigOption::MajorVersion => {
                if let ParsedValue::U32(v) = parsed {
                    major_version = v;
                }
            }
            ConfigOption::RemoteConfigurationId => {
                if let ParsedValue::Str(v) = parsed {
                    remote_configuration_id = Some(v);
                }
            }
        }
    }

    let configuration = Configuration {
        major_version,
        rum: RumConfiguration {
            application_id: Box::from(application_id.unwrap_or("")),
            client_token: Box::from(client_token.unwrap_or("")),
            site: site.map(Box::from),
            service: service.map(Box::from),
            env: env.map(Box::from),
            version: version.map(Box::from),
            track_user_interactions,
            track_resources,
            track_long_task,
            default_privacy_level,
            session_sample_rate,
            session_replay_sample_rate,
            remote_configuration_id: remote_configuration_id.map(Box::from),
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
    fn parse_value_str() {
        let result = parse_value("hello", ValueType::Str, "test").unwrap();
        assert!(matches!(result, ParsedValue::Str("hello")));
    }

    #[test]
    fn parse_value_bool_true() {
        let result = parse_value("True", ValueType::Bool, "test").unwrap();
        assert!(matches!(result, ParsedValue::Bool(true)));
    }

    #[test]
    fn parse_value_bool_false() {
        let result = parse_value("FALSE", ValueType::Bool, "test").unwrap();
        assert!(matches!(result, ParsedValue::Bool(false)));
    }

    #[test]
    fn parse_value_bool_invalid() {
        let result = parse_value("yes", ValueType::Bool, "DD_TEST");
        assert!(result.is_err());
    }

    #[test]
    fn parse_value_u32_valid() {
        let result = parse_value("42", ValueType::U32, "test").unwrap();
        assert!(matches!(result, ParsedValue::U32(42)));
    }

    #[test]
    fn parse_value_u32_invalid() {
        let result = parse_value("abc", ValueType::U32, "DD_TEST");
        assert!(result.is_err());
    }

    #[test]
    fn parse_value_f32_valid() {
        let result = parse_value("42.5", ValueType::F32, "test").unwrap();
        assert!(matches!(result, ParsedValue::F32(v) if (v - 42.5).abs() < f32::EPSILON));
    }

    #[test]
    fn parse_value_f32_infinity() {
        let result = parse_value("inf", ValueType::F32, "DD_TEST");
        assert!(result.is_err());
    }

    #[test]
    fn parse_value_privacy_allow() {
        let result = parse_value("Allow", ValueType::PrivacyLevel, "test").unwrap();
        assert!(matches!(result, ParsedValue::Privacy(PrivacyLevel::Allow)));
    }

    #[test]
    fn parse_value_privacy_mask_user_input() {
        let result = parse_value("mask-user-input", ValueType::PrivacyLevel, "test").unwrap();
        assert!(matches!(
            result,
            ParsedValue::Privacy(PrivacyLevel::MaskUserInput)
        ));
    }

    #[test]
    fn parse_value_privacy_invalid() {
        let result = parse_value("none", ValueType::PrivacyLevel, "DD_TEST");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_primary_key() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_SITE"), String::from("datadoghq.com")),
            (String::from("DD_SITE"), String::from("datadoghq.eu")),
        ]);
        let result = ConfigOption::Site.resolve(lookup_from_map(&map));
        assert_eq!(result, Some("datadoghq.com"));
    }

    #[test]
    fn resolve_fallback_key() {
        let map = BTreeMap::from([(String::from("DD_SITE"), String::from("datadoghq.eu"))]);
        let result = ConfigOption::Site.resolve(lookup_from_map(&map));
        assert_eq!(result, Some("datadoghq.eu"));
    }

    #[test]
    fn resolve_missing() {
        let map: BTreeMap<String, String> = BTreeMap::new();
        let result = ConfigOption::Site.resolve(lookup_from_map(&map));
        assert_eq!(result, None);
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
        assert_eq!(err.code(), 6); // StableConfig error
    }

    #[test]
    fn all_contains_all_variants() {
        assert_eq!(ConfigOption::ALL.len(), 14);
    }
}
