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
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[allow(missing_docs)]
pub struct RumConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_user_interactions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_resources: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_long_tasks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_privacy_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_sample_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_replay_sample_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_configuration_id: Option<String>,

    #[serde(flatten)]
    pub other: Map<Box<str>, Value>,
}

/// Partial [`Configuration`] used to overlay values on top of an existing one.
///
/// Same JSON shape as [`Configuration`], but every field is optional: unset
/// fields leave the corresponding value on the target unchanged.
///
/// # Semantics
///
/// - **Missing keys**: no change on the target.
/// - **Explicit `null`** on a typed field: treated the same as missing —
///   target preserved, *not* cleared. This follows serde's default
///   `Option<T>` handling.
/// - **Unknown keys at the top level** (outside `rum`): rejected with a
///   deserialization error, so typos like `"rumm"` surface instead of being
///   silently dropped.
/// - **Unknown keys inside `rum`**: accumulated into
///   [`RumConfiguration::other`] via `#[serde(flatten)]` so future
///   Browser-SDK additions flow through without an injector release.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[allow(missing_docs)]
pub struct ConfigurationOverlay {
    pub major_version: Option<u32>,
    #[serde(default)]
    pub rum: RumConfiguration,
}

fn overlay_field<T>(dst: &mut Option<T>, src: Option<T>) {
    if src.is_some() {
        *dst = src;
    }
}

impl RumConfiguration {
    /// Merge `overlay` into `self`: overlay `Some(_)` fields win; unknown keys
    /// in `overlay.other` are inserted into `self.other`, replacing any entry
    /// with the same name.
    pub fn merge(&mut self, overlay: RumConfiguration) {
        // Destructure so the compiler catches any field added to the struct but
        // missed here.
        let RumConfiguration {
            application_id,
            client_token,
            site,
            service,
            env,
            version,
            track_user_interactions,
            track_resources,
            track_long_tasks,
            default_privacy_level,
            session_sample_rate,
            session_replay_sample_rate,
            remote_configuration_id,
            other,
        } = overlay;
        overlay_field(&mut self.application_id, application_id);
        overlay_field(&mut self.client_token, client_token);
        overlay_field(&mut self.site, site);
        overlay_field(&mut self.service, service);
        overlay_field(&mut self.env, env);
        overlay_field(&mut self.version, version);
        overlay_field(&mut self.track_user_interactions, track_user_interactions);
        overlay_field(&mut self.track_resources, track_resources);
        overlay_field(&mut self.track_long_tasks, track_long_tasks);
        overlay_field(&mut self.default_privacy_level, default_privacy_level);
        overlay_field(&mut self.session_sample_rate, session_sample_rate);
        overlay_field(
            &mut self.session_replay_sample_rate,
            session_replay_sample_rate,
        );
        overlay_field(&mut self.remote_configuration_id, remote_configuration_id);
        for (k, v) in other {
            self.other.insert(k, v);
        }
    }
}

const SITE_PATTERNS: [&str; 4] = ["datadog", "ddog", "datad0g", "dd0g"];
const MAJOR_VERSIONS: [u32; 2] = [5, 6];

/// Top-level JSON keys accepted by both [`Configuration`] and [`ConfigurationOverlay`].
/// Kept in sync with the struct fields — drift is guarded by a test below.
pub const KNOWN_CONFIG_FIELDS: &[&str] = &["majorVersion", "rum"];

/// Keys accepted under the `rum` object, in camelCase to match the serde
/// rename applied to [`RumConfiguration`]. Drift is guarded by a test below.
pub const KNOWN_RUM_FIELDS: &[&str] = &[
    "applicationId",
    "clientToken",
    "site",
    "service",
    "env",
    "version",
    "trackUserInteractions",
    "trackResources",
    "trackLongTasks",
    "defaultPrivacyLevel",
    "sessionSampleRate",
    "sessionReplaySampleRate",
    "remoteConfigurationId",
];

/// Walks the top-level object and the `rum` sub-object, returning the first
/// key that isn't in the allowlists. Used by [`parse_configuration`] and
/// [`parse_configuration_overlay`] to surface typos like `applicatoinId` as
/// a distinct error code instead of a generic serde parse message.
fn find_unknown_field(value: &Value, top: &[&str], rum: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    for (key, val) in obj {
        if !top.contains(&key.as_str()) {
            return Some(key.clone());
        }
        if key == "rum" {
            if let Some(rum_obj) = val.as_object() {
                for rum_key in rum_obj.keys() {
                    if !rum.contains(&rum_key.as_str()) {
                        return Some(format!("rum.{rum_key}"));
                    }
                }
            }
        }
    }
    None
}

/// Parse a [`Configuration`] from JSON with unknown-field detection.
///
/// A typo like `applicatoinId` returns [`Error::InvalidField`] (code 9) with
/// the offending path, instead of a generic [`Error::Json`] (code 1) whose
/// message embeds a raw serde byte offset. Malformed JSON still produces
/// [`Error::Json`].
pub fn parse_configuration(json: &[u8]) -> Result<Configuration, Error> {
    let value: Value = serde_json::from_slice(json).map_err(|e| Error::Json(format!("{e}")))?;
    if let Some(path) = find_unknown_field(&value, KNOWN_CONFIG_FIELDS, KNOWN_RUM_FIELDS) {
        return Err(Error::InvalidField(path));
    }
    serde_json::from_value::<Configuration>(value).map_err(|e| Error::Json(format!("{e}")))
}

/// Parse a [`ConfigurationOverlay`] from JSON with unknown-field detection.
/// See [`parse_configuration`] for the rationale.
pub fn parse_configuration_overlay(json: &[u8]) -> Result<ConfigurationOverlay, Error> {
    let value: Value = serde_json::from_slice(json).map_err(|e| Error::Json(format!("{e}")))?;
    if let Some(path) = find_unknown_field(&value, KNOWN_CONFIG_FIELDS, KNOWN_RUM_FIELDS) {
        return Err(Error::InvalidField(path));
    }
    serde_json::from_value::<ConfigurationOverlay>(value).map_err(|e| Error::Json(format!("{e}")))
}

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

    /// Overlay another [`Configuration`] on top of this one.
    ///
    /// Typed fields present (`Some(_)`) on `overlay.rum` replace the corresponding
    /// fields on `self.rum`; fields that are `None` on `overlay.rum` are left
    /// untouched. Entries in `overlay.rum.other` are merged key-by-key into
    /// `self.rum.other`, with overlay values winning on conflicts. If
    /// `overlay.major_version` is `Some`, it replaces `self.major_version`.
    pub fn overlay(&mut self, overlay: ConfigurationOverlay) {
        if let Some(v) = overlay.major_version {
            self.major_version = v;
        }
        self.rum.merge(overlay.rum);
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
            return Err(Error::OutOfRangeRate(String::from(label), rate));
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
    use alloc::string::ToString;
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
                expected: Error::OutOfRangeRate(String::from("session sample rate"), 105.0),
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
                expected: Error::OutOfRangeRate(String::from("session replay sample rate"), 2469.0),
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
        // privacy level is passed through as an opaque string; the Browser SDK
        // is the source of truth for valid values. We deliberately do not
        // validate here.
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

    #[test]
    fn overlay_replaces_set_fields_and_preserves_others() {
        let mut config = Configuration {
            major_version: 5,
            rum: RumConfiguration {
                application_id: Some(String::from("app-base")),
                client_token: Some(String::from("tok-base")),
                service: Some(String::from("svc-base")),
                ..Default::default()
            },
        };
        let overlay: ConfigurationOverlay = serde_json::from_slice(
            br#"{"majorVersion":6,"rum":{"service":"svc-overlay","site":"datadoghq.eu"}}"#,
        )
        .unwrap();
        config.overlay(overlay);

        assert_eq!(config.major_version, 6);
        assert_eq!(config.rum.application_id.as_deref(), Some("app-base"));
        assert_eq!(config.rum.client_token.as_deref(), Some("tok-base"));
        assert_eq!(config.rum.service.as_deref(), Some("svc-overlay"));
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.eu"));
    }

    #[test]
    fn overlay_merges_other_map() {
        let mut config = Configuration {
            major_version: 5,
            rum: RumConfiguration {
                application_id: Some(String::from("app")),
                client_token: Some(String::from("tok")),
                other: BTreeMap::from([
                    (Box::from("keep"), Value::from("old")),
                    (Box::from("shared"), Value::from("base")),
                ]),
                ..Default::default()
            },
        };
        let overlay: ConfigurationOverlay =
            serde_json::from_slice(br#"{"rum":{"shared":"overlay","added":true}}"#).unwrap();
        config.overlay(overlay);

        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.other.get("keep"), Some(&Value::from("old")));
        assert_eq!(
            config.rum.other.get("shared"),
            Some(&Value::from("overlay"))
        );
        assert_eq!(config.rum.other.get("added"), Some(&Value::from(true)));
    }

    #[test]
    fn overlay_empty_is_noop() {
        let mut config = Configuration {
            major_version: 5,
            rum: RumConfiguration {
                application_id: Some(String::from("app")),
                ..Default::default()
            },
        };
        let overlay: ConfigurationOverlay = serde_json::from_slice(br#"{}"#).unwrap();
        config.overlay(overlay);
        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.application_id.as_deref(), Some("app"));
    }

    #[test]
    fn overlay_explicit_null_preserves_target() {
        // serde's Option<T> treats `null` as None, and overlay_field only
        // writes when src.is_some(). So `{"rum":{"applicationId":null}}`
        // leaves the base value intact rather than clearing it.
        let mut config = Configuration {
            major_version: 5,
            rum: RumConfiguration {
                application_id: Some(String::from("app-base")),
                client_token: Some(String::from("tok-base")),
                ..Default::default()
            },
        };
        let overlay: ConfigurationOverlay =
            serde_json::from_slice(br#"{"rum":{"applicationId":null}}"#).unwrap();
        config.overlay(overlay);
        assert_eq!(config.rum.application_id.as_deref(), Some("app-base"));
    }

    #[test]
    fn overlay_unknown_top_level_key_is_rejected() {
        // `deny_unknown_fields` at the top level turns typos like "rumm" into
        // a parse error instead of silently dropping them.
        let err =
            serde_json::from_slice::<ConfigurationOverlay>(br#"{"rumm":{"applicationId":"typo"}}"#)
                .expect_err("unknown top-level key must be rejected");
        assert!(
            err.to_string().contains("rumm"),
            "error should name the unknown key, got: {err}"
        );
    }

    #[test]
    fn configuration_unknown_top_level_key_is_rejected() {
        // Symmetric with ConfigurationOverlay: typos like "rumm" must not be
        // silently accepted on the primary JSON path either.
        let err = serde_json::from_slice::<Configuration>(
            br#"{"majorVersion":5,"rum":{"applicationId":"a","clientToken":"b"},"rumm":{}}"#,
        )
        .expect_err("unknown top-level key must be rejected");
        assert!(
            err.to_string().contains("rumm"),
            "error should name the unknown key, got: {err}"
        );
    }

    #[test]
    fn overlay_unknown_rum_key_is_captured_in_other() {
        // Unknown keys inside `rum` still flow into `other` via
        // #[serde(flatten)] so future Browser-SDK additions don't require an
        // injector release.
        let overlay: ConfigurationOverlay = serde_json::from_slice(br#"{"rum":{"newKnob":"x"}}"#)
            .expect("unknown rum key must be accepted");
        assert_eq!(overlay.rum.other.get("newKnob"), Some(&Value::from("x")));
    }

    #[test]
    fn known_field_arrays_match_struct_field_counts() {
        // Drift guard: if a field is added to Configuration/RumConfiguration
        // but the corresponding KNOWN_* array isn't updated, the two-pass
        // parser would reject the new field as unknown. Update both the
        // array and this expected count together.
        assert_eq!(KNOWN_CONFIG_FIELDS.len(), 2);
        assert_eq!(KNOWN_RUM_FIELDS.len(), 13);
    }

    #[test]
    fn parse_configuration_rejects_typo_at_top_level() {
        let err = parse_configuration(
            br#"{"majorVersion":5,"rum":{"applicationId":"a","clientToken":"b"},"applicatoinId":"c"}"#,
        )
        .expect_err("typo at top level must fail");
        assert_eq!(err.code(), 9);
        match err {
            Error::InvalidField(path) => assert_eq!(path, "applicatoinId"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn parse_configuration_rejects_typo_inside_rum() {
        let err = parse_configuration(
            br#"{"majorVersion":5,"rum":{"applicaitonId":"a","clientToken":"b"}}"#,
        )
        .expect_err("typo inside rum must fail");
        assert_eq!(err.code(), 9);
        match err {
            Error::InvalidField(path) => assert_eq!(path, "rum.applicaitonId"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn parse_configuration_surfaces_malformed_json_as_json_error() {
        let err = parse_configuration(br#"{"majorVersion":5,"rum":{"#)
            .expect_err("unclosed brace must fail");
        assert_eq!(err.code(), 1);
    }

    #[test]
    fn parse_configuration_accepts_valid_config() {
        let config = parse_configuration(
            br#"{"majorVersion":5,"rum":{"applicationId":"a","clientToken":"b"}}"#,
        )
        .expect("valid config must parse");
        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.application_id.as_deref(), Some("a"));
        assert_eq!(config.rum.client_token.as_deref(), Some("b"));
    }

    #[test]
    fn parse_configuration_overlay_rejects_typo_inside_rum() {
        let err = parse_configuration_overlay(br#"{"rum":{"applicaitonId":"x"}}"#)
            .expect_err("typo inside overlay rum must fail");
        assert_eq!(err.code(), 9);
        match err {
            Error::InvalidField(path) => assert_eq!(path, "rum.applicaitonId"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }
}
