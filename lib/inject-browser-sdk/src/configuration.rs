// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use crate::error::Error;
use alloc::boxed::Box;
use alloc::collections::BTreeMap as Map;
use alloc::format;
use alloc::string::String;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Debug))]
#[allow(missing_docs)]
pub struct Configuration {
    pub major_version: u32,
    pub rum: RumConfiguration,
}

/// Configuration settings for Real User Monitoring (RUM).
///
/// Known fields are strongly typed. Unknown JSON keys are captured in
/// [`RumConfiguration::other`] so future Browser-SDK additions flow through
/// without an injector release.
///
/// Serializes as a flat JSON object with keys sorted alphabetically.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[allow(missing_docs)]
pub struct RumConfiguration {
    pub application_id: Option<String>,
    pub client_token: Option<String>,
    pub site: Option<String>,
    pub service: Option<String>,
    pub env: Option<String>,
    pub version: Option<String>,
    pub track_user_interactions: Option<bool>,
    pub track_resources: Option<bool>,
    pub track_long_tasks: Option<bool>,
    pub default_privacy_level: Option<String>,
    pub session_sample_rate: Option<f64>,
    pub session_replay_sample_rate: Option<f64>,
    pub remote_configuration_id: Option<String>,

    #[serde(flatten)]
    pub other: Map<Box<str>, Value>,
}

enum SerializedValue<'a> {
    Str(&'a str),
    Bool(bool),
    F64(f64),
    Json(&'a Value),
}

impl Serialize for SerializedValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SerializedValue::Str(v) => serializer.serialize_str(v),
            SerializedValue::Bool(v) => serializer.serialize_bool(*v),
            SerializedValue::F64(v) => serializer.serialize_f64(*v),
            SerializedValue::Json(v) => v.serialize(serializer),
        }
    }
}

impl Serialize for RumConfiguration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries: Map<&str, SerializedValue<'_>> = Map::new();
        if let Some(v) = &self.application_id {
            entries.insert("applicationId", SerializedValue::Str(v));
        }
        if let Some(v) = &self.client_token {
            entries.insert("clientToken", SerializedValue::Str(v));
        }
        if let Some(v) = &self.site {
            entries.insert("site", SerializedValue::Str(v));
        }
        if let Some(v) = &self.service {
            entries.insert("service", SerializedValue::Str(v));
        }
        if let Some(v) = &self.env {
            entries.insert("env", SerializedValue::Str(v));
        }
        if let Some(v) = &self.version {
            entries.insert("version", SerializedValue::Str(v));
        }
        if let Some(v) = self.track_user_interactions {
            entries.insert("trackUserInteractions", SerializedValue::Bool(v));
        }
        if let Some(v) = self.track_resources {
            entries.insert("trackResources", SerializedValue::Bool(v));
        }
        if let Some(v) = self.track_long_tasks {
            entries.insert("trackLongTasks", SerializedValue::Bool(v));
        }
        if let Some(v) = &self.default_privacy_level {
            entries.insert("defaultPrivacyLevel", SerializedValue::Str(v));
        }
        if let Some(v) = self.session_sample_rate {
            entries.insert("sessionSampleRate", SerializedValue::F64(v));
        }
        if let Some(v) = self.session_replay_sample_rate {
            entries.insert("sessionReplaySampleRate", SerializedValue::F64(v));
        }
        if let Some(v) = &self.remote_configuration_id {
            entries.insert("remoteConfigurationId", SerializedValue::Str(v));
        }
        for (k, v) in &self.other {
            entries.insert(k.as_ref(), SerializedValue::Json(v));
        }

        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (k, v) in &entries {
            map.serialize_entry(*k, v)?;
        }
        map.end()
    }
}

const SITE_PATTERNS: [&str; 4] = ["datadog", "ddog", "datad0g", "dd0g"];
const MAJOR_VERSIONS: [u32; 2] = [5, 6];

type Setter = fn(&mut Configuration, &str) -> Result<(), Error>;

const DD_FIELDS: &[(&str, Setter)] = &[
    ("DD_RUM_APPLICATION_ID", |c, r| {
        c.rum.application_id = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_CLIENT_TOKEN", |c, r| {
        c.rum.client_token = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_SITE", |c, r| {
        c.rum.site = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_SERVICE", |c, r| {
        c.rum.service = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_ENVIRONMENT", |c, r| {
        c.rum.env = Some(r.into());
        Ok(())
    }),
    // Service/app version (e.g. "1.2.3"), emitted as snippet field "version".
    // Distinct from DD_RUM_MAJOR_VERSION, which selects the RUM library major version.
    ("DD_RUM_VERSION", |c, r| {
        c.rum.version = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_TRACK_USER_INTERACTIONS", |c, r| {
        c.rum.track_user_interactions = Some(parse_bool(r, "DD_RUM_TRACK_USER_INTERACTIONS")?);
        Ok(())
    }),
    ("DD_RUM_TRACK_RESOURCES", |c, r| {
        c.rum.track_resources = Some(parse_bool(r, "DD_RUM_TRACK_RESOURCES")?);
        Ok(())
    }),
    ("DD_RUM_TRACK_LONG_TASKS", |c, r| {
        c.rum.track_long_tasks = Some(parse_bool(r, "DD_RUM_TRACK_LONG_TASKS")?);
        Ok(())
    }),
    ("DD_RUM_DEFAULT_PRIVACY_LEVEL", |c, r| {
        c.rum.default_privacy_level = Some(r.into());
        Ok(())
    }),
    ("DD_RUM_SESSION_SAMPLE_RATE", |c, r| {
        c.rum.session_sample_rate = Some(parse_f64(r, "DD_RUM_SESSION_SAMPLE_RATE")?);
        Ok(())
    }),
    ("DD_RUM_SESSION_REPLAY_SAMPLE_RATE", |c, r| {
        c.rum.session_replay_sample_rate = Some(parse_f64(r, "DD_RUM_SESSION_REPLAY_SAMPLE_RATE")?);
        Ok(())
    }),
    // Writes to the outer Configuration, not RumConfiguration — not a snippet field.
    ("DD_RUM_MAJOR_VERSION", |c, r| {
        c.major_version = parse_u32(r, "DD_RUM_MAJOR_VERSION")?;
        Ok(())
    }),
    ("DD_RUM_REMOTE_CONFIGURATION_ID", |c, r| {
        c.rum.remote_configuration_id = Some(r.into());
        Ok(())
    }),
];

impl Configuration {
    /// Resolve a [`Configuration`] from `DD_RUM_*` keys via a lookup function.
    ///
    /// Only the `DD_RUM_*` keys listed in the internal table are consulted;
    /// unknown keys are ignored and Datadog-wide keys (e.g. `DD_SITE`) are
    /// not read.
    pub fn from_lookup<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Result<Self, Error> {
        let mut cfg = Configuration {
            major_version: 5,
            rum: RumConfiguration::default(),
        };
        for &(name, setter) in DD_FIELDS {
            if let Some(raw) = lookup(name) {
                setter(&mut cfg, raw)?;
            }
        }
        Ok(cfg)
    }

    /// Validate the resolved configuration.
    pub fn validate(&self) -> Result<(), Error> {
        if !MAJOR_VERSIONS.contains(&self.major_version) {
            return Err(Error::UnsupportedMajorVersion(self.major_version));
        }

        if self.rum.application_id.as_deref().unwrap_or("").is_empty() {
            return Err(Error::EmptyMandatoryConf(String::from("applicationId")));
        }
        if self.rum.client_token.as_deref().unwrap_or("").is_empty() {
            return Err(Error::EmptyMandatoryConf(String::from("clientToken")));
        }
        if let Some(site) = &self.rum.site {
            if !SITE_PATTERNS.iter().any(|p| site.contains(p)) {
                return Err(Error::UnsupportedSite(site.clone()));
            }
        }
        validate_rate("session sample rate", self.rum.session_sample_rate)?;
        validate_rate(
            "session replay sample rate",
            self.rum.session_replay_sample_rate,
        )?;
        Ok(())
    }
}

fn validate_rate(label: &str, value: Option<f64>) -> Result<(), Error> {
    if let Some(rate) = value {
        if !(0.0..=100.0).contains(&rate) {
            return Err(Error::OutOfRangeRate(String::from(label), rate as f32));
        }
    }
    Ok(())
}

pub(crate) fn parse_bool(raw: &str, key: &str) -> Result<bool, Error> {
    match raw.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::StableConfig(format!(
            "Invalid boolean value '{raw}' for key '{key}'"
        ))),
    }
}

pub(crate) fn parse_u32(raw: &str, key: &str) -> Result<u32, Error> {
    raw.parse()
        .map_err(|_| Error::StableConfig(format!("Invalid u32 value '{raw}' for key '{key}'")))
}

pub(crate) fn parse_f64(raw: &str, key: &str) -> Result<f64, Error> {
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

#[cfg(test)]
mod test {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use pretty_assertions::assert_eq;

    fn lookup_from_map<'a>(map: &'a BTreeMap<String, String>) -> impl Fn(&str) -> Option<&'a str> {
        |key| map.get(key).map(|v| v.as_str())
    }

    #[test]
    fn from_json() {
        let configuration: Configuration = serde_json::from_slice(
            br#"
{
  "majorVersion": 5,
  "rum": {
    "clientToken": "foo",
    "applicationId": "bar",
    "site": "datadoghq.com",
    "newConfig": "a",
    "newConfig2": 42
  }
}
"#,
        )
        .unwrap();

        assert_eq!(configuration.major_version, 5);
        assert_eq!(configuration.rum.client_token.as_deref(), Some("foo"));
        assert_eq!(configuration.rum.application_id.as_deref(), Some("bar"));
        assert_eq!(configuration.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(
            configuration.rum.other.get("newConfig"),
            Some(&Value::from("a"))
        );
        assert_eq!(
            configuration.rum.other.get("newConfig2"),
            Some(&Value::from(42))
        );
    }

    #[test]
    fn test_invalid_configurations() {
        struct TestCase {
            input: Configuration,
            expected: Error,
        }

        let test_cases = vec![
            TestCase {
                input: Configuration {
                    major_version: 6,
                    rum: RumConfiguration {
                        application_id: Some(String::new()),
                        client_token: Some(String::from("foo")),
                        ..Default::default()
                    },
                },
                expected: Error::EmptyMandatoryConf(String::from("applicationId")),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::new()),
                        ..Default::default()
                    },
                },
                expected: Error::EmptyMandatoryConf(String::from("clientToken")),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::from("foo")),
                        session_sample_rate: Some(105.0),
                        ..Default::default()
                    },
                },
                expected: Error::OutOfRangeRate(String::from("session sample rate"), 105.),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::from("foo")),
                        session_replay_sample_rate: Some(2469.0),
                        ..Default::default()
                    },
                },
                expected: Error::OutOfRangeRate(String::from("session replay sample rate"), 2469.),
            },
            TestCase {
                input: Configuration {
                    major_version: 4,
                    rum: RumConfiguration::default(),
                },
                expected: Error::UnsupportedMajorVersion(4),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::from("foo")),
                        site: Some(String::from("example.com")),
                        ..Default::default()
                    },
                },
                expected: Error::UnsupportedSite(String::from("example.com")),
            },
        ];

        for test_case in test_cases {
            assert_eq!(
                test_case.input.validate().unwrap_err().code(),
                test_case.expected.code()
            );
        }
    }

    #[test]
    fn parse_helpers() {
        assert!(parse_bool("True", "k").unwrap());
        assert!(!parse_bool("FALSE", "k").unwrap());
        assert!(parse_bool("yes", "k").is_err());

        assert_eq!(parse_u32("42", "k").unwrap(), 42);
        assert!(parse_u32("abc", "k").is_err());

        assert!((parse_f64("42.5", "k").unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(parse_f64("inf", "k").is_err());
        assert!(parse_f64("NaN", "k").is_err());
    }

    #[test]
    fn configuration_from_lookup_minimal() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
        ]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.application_id.as_deref(), Some("app-1"));
        assert_eq!(config.rum.client_token.as_deref(), Some("tok-1"));
        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.site, None);
    }

    #[test]
    fn configuration_from_lookup_full() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_RUM_SITE"), String::from("datadoghq.com")),
            (String::from("DD_RUM_SERVICE"), String::from("my-svc")),
            (String::from("DD_RUM_ENVIRONMENT"), String::from("prod")),
            (String::from("DD_RUM_VERSION"), String::from("1.0")),
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
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.application_id.as_deref(), Some("app-1"));
        assert_eq!(config.rum.client_token.as_deref(), Some("tok-1"));
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(config.rum.service.as_deref(), Some("my-svc"));
        assert_eq!(config.rum.env.as_deref(), Some("prod"));
        assert_eq!(config.rum.version.as_deref(), Some("1.0"));
        assert_eq!(config.rum.track_user_interactions, Some(true));
        assert_eq!(config.rum.track_resources, Some(false));
        assert_eq!(config.rum.track_long_tasks, Some(true));
        assert_eq!(config.rum.default_privacy_level.as_deref(), Some("mask"));
        assert_eq!(config.rum.session_sample_rate, Some(80.5));
        assert_eq!(config.rum.session_replay_sample_rate, Some(50.0));
        assert_eq!(config.major_version, 6);
        assert_eq!(
            config.rum.remote_configuration_id.as_deref(),
            Some("rc-123")
        );
    }

    #[test]
    fn configuration_from_lookup_privacy_passthrough() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (
                String::from("DD_RUM_DEFAULT_PRIVACY_LEVEL"),
                String::from("MaSk"),
            ),
        ]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.default_privacy_level.as_deref(), Some("MaSk"));
    }

    #[test]
    fn configuration_from_lookup_invalid_bool() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_RUM_TRACK_RESOURCES"), String::from("yes")),
        ]);
        let err = Configuration::from_lookup(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 6);
    }

    #[test]
    fn configuration_from_lookup_invalid_float() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("abc"),
            ),
        ]);
        let err = Configuration::from_lookup(lookup_from_map(&map)).unwrap_err();
        assert_eq!(err.code(), 6);
    }

    #[test]
    fn configuration_from_lookup_out_of_range_rate() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("105.0"),
            ),
        ]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        let err = config.validate().unwrap_err();
        assert_eq!(err.code(), 4);
    }

    #[test]
    fn configuration_from_lookup_missing_application_id() {
        let map = BTreeMap::from([(String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1"))]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        let err = config.validate().unwrap_err();
        assert_eq!(err.code(), 5);
    }

    #[test]
    fn configuration_from_lookup_empty_application_id() {
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
        ]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        let err = config.validate().unwrap_err();
        assert_eq!(err.code(), 5);
    }
}
