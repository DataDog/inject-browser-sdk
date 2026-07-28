// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use crate::error::Error;
use crate::injector::{InjectionStrategy, MetaCspPlacement};
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
/// Known fields are strongly typed. [`RumConfiguration::other`] is a
/// `#[serde(flatten)]` catch-all: unknown keys inside `rum` are captured
/// into it on deserialization and flattened back into the generated
/// `DD_RUM.init({...})` JSON on serialization, so new Browser SDK init
/// options pass through without a code change here.
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
///   silently dropped. There is no `other` catch-all at this level.
/// - **Unknown keys inside `rum`**: captured into
///   [`RumConfiguration::other`] and merged into the target's `other` on
///   overlay. This is the forward-compat path for new Browser SDK init
///   options; the trade-off is that typos inside `rum` are silently passed
///   through rather than rejected.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[allow(missing_docs)]
pub struct ConfigurationOverlay {
    pub major_version: Option<u32>,
    pub enabled: Option<bool>,
    /// Where the host should inject the snippet. A host concern rather than a
    /// Browser SDK init option, so it sits here rather than on
    /// [`RumConfiguration`], whose fields are serialised into `DD_RUM.init`.
    pub injection_strategy: Option<InjectionStrategy>,
    /// Whether the host may hoist the encoding declaration above a
    /// meta-delivered Content Security Policy. Same reasoning as
    /// [`ConfigurationOverlay::injection_strategy`].
    pub meta_csp_placement: Option<MetaCspPlacement>,
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

/// Datadog domains that accept an optional datacenter prefix
/// (`[a-z]{2,}[0-9]{1,2}.`), mirroring the agent's `ddDomainPattern`.
const PREFIXED_DOMAINS: &[&str] = &["datadoghq.com", "datadoghq.eu", "datad0g.com", "datad0g.eu"];

/// Gov domains: must match exactly, no datacenter prefix is allowed.
#[cfg(feature = "support_gov_domain")]
const FLAT_ONLY_DOMAINS: &[&str] = &["ddog-gov.com", "ddog-gov.mil"];

pub(crate) struct ParsedSite<'a> {
    pub prefix: Option<&'a str>,
    pub domain: &'a str,
}

/// Parse and validate a site string against the agent's `ddSitePattern`.
///
/// Accepted formats:
/// - `([a-z]{2,}[0-9]{1,2}\.)?(datad(oghq|0g)\.(com|eu))`
/// - `ddog-gov.(com|mil)` (US1-FED, exact match only — no prefix)
pub(crate) fn parse_site(site: &str) -> Option<ParsedSite<'_>> {
    #[cfg(feature = "support_gov_domain")]
    for &domain in FLAT_ONLY_DOMAINS {
        if site == domain {
            return Some(ParsedSite {
                prefix: None,
                domain,
            });
        }
    }
    for &domain in PREFIXED_DOMAINS {
        if site == domain {
            return Some(ParsedSite {
                prefix: None,
                domain,
            });
        }
        if let Some(prefix_dot) = site.strip_suffix(domain) {
            if let Some(prefix) = prefix_dot.strip_suffix('.') {
                if is_valid_datacenter_prefix(prefix) {
                    return Some(ParsedSite {
                        prefix: Some(prefix),
                        domain,
                    });
                }
            }
        }
    }
    return None;
}

/// Validates that a datacenter prefix matches `[a-z]{2,}[0-9]{1,2}`.
fn is_valid_datacenter_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_lowercase() {
        i += 1;
    }
    if i < 2 {
        return false;
    }
    let letters_end = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let digits = i - letters_end;
    return (1..=2).contains(&digits) && i == b.len();
}

const MAJOR_VERSIONS: [u32; 2] = [5, 6];

/// Browser SDK major version used when `DD_RUM_MAJOR_VERSION` is not set.
const DEFAULT_MAJOR_VERSION: u32 = 6;

/// Parse a [`Configuration`] from JSON.
///
/// Unknown top-level keys (e.g. a typo like `"rumm"`) are rejected by
/// `deny_unknown_fields` and surface as [`Error::Json`]. Unknown keys
/// inside `rum` flow into [`RumConfiguration::other`] for forward-compat
/// with future Browser SDK init options.
pub fn parse_configuration(json: &[u8]) -> Result<Configuration, Error> {
    return serde_json::from_slice(json).map_err(|e| return Error::Json(format!("{e}")));
}

/// Parse a [`ConfigurationOverlay`] from JSON. See [`parse_configuration`]
/// for the unknown-field semantics.
pub fn parse_configuration_overlay(json: &[u8]) -> Result<ConfigurationOverlay, Error> {
    return serde_json::from_slice(json).map_err(|e| return Error::Json(format!("{e}")));
}

type Setter = fn(&mut Configuration, &str) -> Result<(), Error>;

const DD_FIELDS: &[(&str, Setter)] = &[
    ("DD_RUM_APPLICATION_ID", |c, r| {
        c.rum.application_id = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_CLIENT_TOKEN", |c, r| {
        c.rum.client_token = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_SITE", |c, r| {
        c.rum.site = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_SERVICE", |c, r| {
        c.rum.service = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_ENVIRONMENT", |c, r| {
        c.rum.env = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_VERSION", |c, r| {
        c.rum.version = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_TRACK_USER_INTERACTIONS", |c, r| {
        c.rum.track_user_interactions = Some(parse_bool(r, "DD_RUM_TRACK_USER_INTERACTIONS")?);
        return Ok(());
    }),
    ("DD_RUM_TRACK_RESOURCES", |c, r| {
        c.rum.track_resources = Some(parse_bool(r, "DD_RUM_TRACK_RESOURCES")?);
        return Ok(());
    }),
    ("DD_RUM_TRACK_LONG_TASKS", |c, r| {
        c.rum.track_long_tasks = Some(parse_bool(r, "DD_RUM_TRACK_LONG_TASKS")?);
        return Ok(());
    }),
    ("DD_RUM_DEFAULT_PRIVACY_LEVEL", |c, r| {
        c.rum.default_privacy_level = Some(r.into());
        return Ok(());
    }),
    ("DD_RUM_SESSION_SAMPLE_RATE", |c, r| {
        c.rum.session_sample_rate = Some(parse_f64(r, "DD_RUM_SESSION_SAMPLE_RATE")?);
        return Ok(());
    }),
    ("DD_RUM_SESSION_REPLAY_SAMPLE_RATE", |c, r| {
        c.rum.session_replay_sample_rate = Some(parse_f64(r, "DD_RUM_SESSION_REPLAY_SAMPLE_RATE")?);
        return Ok(());
    }),
    ("DD_RUM_MAJOR_VERSION", |c, r| {
        c.major_version = parse_u32(r, "DD_RUM_MAJOR_VERSION")?;
        return Ok(());
    }),
    ("DD_RUM_REMOTE_CONFIGURATION_ID", |c, r| {
        c.rum.remote_configuration_id = Some(r.into());
        return Ok(());
    }),
];

/// Stable-configuration keys that configure the *host* rather than the Browser
/// SDK, and so are resolved by [`crate::stable_config::StableConfig`] rather
/// than [`Configuration::from_lookup`]: they never reach `DD_RUM.init`.
pub(crate) const HOST_KEYS: &[&str] = &[
    "DD_RUM_ENABLED",
    "DD_RUM_INJECTION_STRATEGY",
    "DD_RUM_META_CSP_PLACEMENT",
];

/// Whether a stable-configuration key is consumed by the RUM configuration
/// path, either as a Browser SDK option or as one of the [`HOST_KEYS`].
pub(crate) fn is_rum_configuration_key(name: &str) -> bool {
    return HOST_KEYS.contains(&name)
        || DD_FIELDS
            .iter()
            .any(|(field_name, _)| return *field_name == name);
}

impl Configuration {
    /// Resolve a [`Configuration`] from `DD_RUM_*` keys via a lookup function.
    ///
    /// Only the `DD_RUM_*` keys listed in the internal table are consulted;
    /// unknown keys are ignored and Datadog-wide keys (e.g. `DD_SITE`) are
    /// not read.
    pub fn from_lookup<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Result<Self, Error> {
        let mut cfg = Configuration {
            major_version: DEFAULT_MAJOR_VERSION,
            rum: RumConfiguration::default(),
        };
        for &(name, setter) in DD_FIELDS {
            if let Some(raw) = lookup(name) {
                setter(&mut cfg, raw)?;
            }
        }
        return Ok(cfg);
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
            if parse_site(site).is_none() {
                return Err(Error::UnsupportedSite(site.clone()));
            }
        }
        validate_rate("session sample rate", self.rum.session_sample_rate)?;
        validate_rate(
            "session replay sample rate",
            self.rum.session_replay_sample_rate,
        )?;
        return Ok(());
    }
}

fn validate_rate(label: &str, value: Option<f64>) -> Result<(), Error> {
    if let Some(rate) = value {
        if rate.is_nan() || !(0.0..=100.0).contains(&rate) {
            return Err(Error::OutOfRangeRate(String::from(label), rate));
        }
    }
    return Ok(());
}

pub(crate) fn parse_bool(raw: &str, key: &str) -> Result<bool, Error> {
    match raw.to_ascii_lowercase().as_str() {
        "true" => return Ok(true),
        "false" => return Ok(false),
        _ => {
            return Err(Error::StableConfig(format!(
                "Invalid boolean value '{raw}' for key '{key}'"
            )))
        }
    }
}

/// Accepted values for `DD_RUM_INJECTION_STRATEGY`, matching the JSON overlay
/// spelling of [`InjectionStrategy`].
const INJECTION_STRATEGIES: &[(&str, InjectionStrategy)] = &[
    ("head-close-tag", InjectionStrategy::HeadCloseTag),
    ("head-open-tag", InjectionStrategy::HeadOpenTag),
    (
        "head-open-tag-with-close-tag-fallback",
        InjectionStrategy::HeadOpenTagWithCloseTagFallback,
    ),
];

/// Accepted values for `DD_RUM_META_CSP_PLACEMENT`, matching the JSON overlay
/// spelling of [`MetaCspPlacement`].
const META_CSP_PLACEMENTS: &[(&str, MetaCspPlacement)] = &[
    ("after", MetaCspPlacement::After),
    ("before", MetaCspPlacement::Before),
];

fn parse_enum<T: Copy>(table: &[(&str, T)], raw: &str, key: &str) -> Result<T, Error> {
    let lower_case = raw.to_ascii_lowercase();
    for (name, value) in table {
        if *name == lower_case {
            return Ok(*value);
        }
    }
    let accepted = table
        .iter()
        .map(|(name, _)| return *name)
        .collect::<alloc::vec::Vec<_>>()
        .join(", ");
    return Err(Error::StableConfig(format!(
        "Invalid value '{raw}' for key '{key}', expected one of {accepted}"
    )));
}

pub(crate) fn parse_injection_strategy(raw: &str, key: &str) -> Result<InjectionStrategy, Error> {
    return parse_enum(INJECTION_STRATEGIES, raw, key);
}

pub(crate) fn parse_meta_csp_placement(raw: &str, key: &str) -> Result<MetaCspPlacement, Error> {
    return parse_enum(META_CSP_PLACEMENTS, raw, key);
}

pub(crate) fn parse_u32(raw: &str, key: &str) -> Result<u32, Error> {
    return raw.parse().map_err(|_| {
        return Error::StableConfig(format!("Invalid u32 value '{raw}' for key '{key}'"));
    });
}

pub(crate) fn parse_f64(raw: &str, key: &str) -> Result<f64, Error> {
    let val: f64 = raw.parse().map_err(|_| {
        return Error::StableConfig(format!("Invalid float value '{raw}' for key '{key}'"));
    })?;
    if !val.is_finite() {
        return Err(Error::StableConfig(format!(
            "Invalid float value '{raw}' for key '{key}'"
        )));
    }
    return Ok(val);
}

#[cfg(test)]
mod test {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use alloc::vec;
    use pretty_assertions::assert_eq;

    fn lookup_from_map<'a>(map: &'a BTreeMap<String, String>) -> impl Fn(&str) -> Option<&'a str> {
        return |key| return map.get(key).map(|v| return v.as_str());
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
                        session_sample_rate: Some(f64::NAN),
                        ..Default::default()
                    },
                },
                expected: Error::OutOfRangeRate(String::from("session sample rate"), f64::NAN),
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
    fn parse_host_injection_enums() {
        assert_eq!(
            parse_injection_strategy("head-open-tag", "k").unwrap(),
            InjectionStrategy::HeadOpenTag
        );
        // Matching is case insensitive, like the boolean parser.
        assert_eq!(
            parse_injection_strategy("Head-Close-Tag", "k").unwrap(),
            InjectionStrategy::HeadCloseTag
        );
        assert_eq!(
            parse_meta_csp_placement("before", "k").unwrap(),
            MetaCspPlacement::Before
        );

        // An unknown value is an error rather than a silent fallback to the default: it is almost
        // certainly a typo, and silently ignoring it would leave the operator with no signal.
        let err = parse_injection_strategy("head-middle-tag", "DD_RUM_INJECTION_STRATEGY")
            .expect_err("unknown strategy must fail");
        assert_eq!(err.code(), 6);
        assert!(
            err.to_string().contains("head-open-tag"),
            "error should list the accepted values, got: {err}"
        );
        assert!(parse_meta_csp_placement("maybe", "k").is_err());
    }

    #[test]
    fn overlay_carries_host_injection_options() {
        let overlay = parse_configuration_overlay(
            br#"{"injectionStrategy":"head-open-tag-with-close-tag-fallback","metaCspPlacement":"before"}"#,
        )
        .expect("host options must parse");
        assert_eq!(
            overlay.injection_strategy,
            Some(InjectionStrategy::HeadOpenTagWithCloseTagFallback)
        );
        assert_eq!(overlay.meta_csp_placement, Some(MetaCspPlacement::Before));
    }

    #[test]
    fn overlay_rejects_unknown_host_injection_value() {
        let err =
            serde_json::from_slice::<ConfigurationOverlay>(br#"{"injectionStrategy":"nowhere"}"#)
                .expect_err("unknown variant must be rejected");
        assert!(
            err.to_string().contains("nowhere"),
            "error should name the bad value, got: {err}"
        );
    }

    #[test]
    fn host_keys_are_rum_configuration_keys() {
        // They must be recognised, so a file that only sets them is not reported as empty, but
        // they must not reach `Configuration`: everything on `RumConfiguration` is serialised into
        // `DD_RUM.init`, and these configure the host.
        for key in HOST_KEYS {
            assert!(is_rum_configuration_key(key), "{key} must be recognised");
            assert!(
                !DD_FIELDS.iter().any(|(name, _)| return name == key),
                "{key} must not be a Browser SDK option"
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
        assert_eq!(config.major_version, DEFAULT_MAJOR_VERSION);
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
    fn configuration_from_lookup_ignores_non_dd_rum_keys() {
        // Only DD_RUM_* keys are consulted; Datadog-wide keys like DD_SITE
        // are ignored. Guards against a future refactor that adds DD_SITE
        // as a silent fallback and breaks the documented precedence contract.
        let map = BTreeMap::from([
            (String::from("DD_RUM_APPLICATION_ID"), String::from("app-1")),
            (String::from("DD_RUM_CLIENT_TOKEN"), String::from("tok-1")),
            (String::from("DD_SITE"), String::from("datadoghq.eu")),
        ]);
        let config = Configuration::from_lookup(lookup_from_map(&map)).unwrap();
        assert_eq!(config.rum.site, None, "DD_SITE must not leak into rum.site");
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
        // At the struct level, `#[serde(flatten)] other` captures unknown
        // keys inside `rum` when ConfigurationOverlay is deserialized
        // directly. The public `parse_configuration_overlay` helper rejects
        // them for typo safety — see
        // `parse_configuration_overlay_rejects_typo_inside_rum`.
        let overlay: ConfigurationOverlay = serde_json::from_slice(br#"{"rum":{"newKnob":"x"}}"#)
            .expect("unknown rum key must be accepted");
        assert_eq!(overlay.rum.other.get("newKnob"), Some(&Value::from("x")));
    }

    #[test]
    fn parse_configuration_rejects_typo_at_top_level() {
        // `deny_unknown_fields` on Configuration catches typos like a stray
        // top-level "applicatoinId". There is no `other` catch-all at this
        // level, so a silent drop would mask the bug.
        let err = parse_configuration(
            br#"{"majorVersion":5,"rum":{"applicationId":"a","clientToken":"b"},"applicatoinId":"c"}"#,
        )
        .expect_err("typo at top level must fail");
        assert_eq!(err.code(), 1);
        assert!(
            err.to_string().contains("applicatoinId"),
            "error should name the unknown key, got: {err}"
        );
    }

    #[test]
    fn parse_configuration_passes_unknown_rum_field_through_other() {
        // Unknown keys inside `rum` are intentionally accepted and routed to
        // `RumConfiguration::other` so new Browser SDK init options pass
        // through without a code change here.
        let config = parse_configuration(
            br#"{"majorVersion":5,"rum":{"applicationId":"a","clientToken":"b","newKnob":"x"}}"#,
        )
        .expect("unknown rum keys must flow into `other`");
        assert_eq!(config.rum.other.get("newKnob"), Some(&Value::from("x")));
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
    fn parse_configuration_overlay_passes_unknown_rum_field_through_other() {
        let overlay = parse_configuration_overlay(br#"{"rum":{"newKnob":"x"}}"#)
            .expect("unknown rum keys must flow into `other`");
        assert_eq!(overlay.rum.other.get("newKnob"), Some(&Value::from("x")));
    }

    #[test]
    fn test_parse_site_valid() {
        let valid_sites = vec![
            ("datadoghq.com", None, "datadoghq.com"),
            ("datadoghq.eu", None, "datadoghq.eu"),
            ("us1.datadoghq.com", Some("us1"), "datadoghq.com"),
            ("us3.datadoghq.com", Some("us3"), "datadoghq.com"),
            ("us5.datadoghq.com", Some("us5"), "datadoghq.com"),
            ("ap1.datadoghq.com", Some("ap1"), "datadoghq.com"),
            ("ap2.datadoghq.com", Some("ap2"), "datadoghq.com"),
            ("eu2.datadoghq.eu", Some("eu2"), "datadoghq.eu"),
            ("datad0g.com", None, "datad0g.com"),
            ("datad0g.eu", None, "datad0g.eu"),
            ("us3.datad0g.com", Some("us3"), "datad0g.com"),
            // prefix boundary: 2 letters + 2 digits
            ("us12.datadoghq.com", Some("us12"), "datadoghq.com"),
            // prefix boundary: 3 letters + 1 digit
            ("abc1.datadoghq.com", Some("abc1"), "datadoghq.com"),
        ];

        for (site, expected_prefix, expected_domain) in valid_sites {
            let parsed =
                parse_site(site).unwrap_or_else(|| panic!("expected '{site}' to be valid"));
            assert_eq!(
                parsed.prefix, expected_prefix,
                "prefix mismatch for '{site}'"
            );
            assert_eq!(
                parsed.domain, expected_domain,
                "domain mismatch for '{site}'"
            );
        }
    }

    #[test]
    fn test_parse_site_invalid() {
        let invalid_sites = vec![
            "example.com",
            "x1.datadoghq.com",    // prefix too short (1 letter)
            "a1.datadoghq.com",    // prefix too short (1 letter)
            "us.datadoghq.com",    // no digit in prefix
            "US3.datadoghq.com",   // uppercase
            "123.datadoghq.com",   // no letters in prefix
            "us123.datadoghq.com", // prefix has 3 digits (max 2)
            "xdatadoghq.com",      // no dot separator before domain
            "",
        ];

        for site in invalid_sites {
            assert!(
                parse_site(site).is_none(),
                "expected '{site}' to be invalid"
            );
        }
    }

    #[cfg(feature = "support_gov_domain")]
    #[test]
    fn test_parse_site_gov() {
        for (site, expected_domain) in [
            ("ddog-gov.com", "ddog-gov.com"),
            ("ddog-gov.mil", "ddog-gov.mil"),
        ] {
            let parsed =
                parse_site(site).unwrap_or_else(|| panic!("expected '{site}' to be valid"));
            assert_eq!(parsed.prefix, None, "prefix mismatch for '{site}'");
            assert_eq!(
                parsed.domain, expected_domain,
                "domain mismatch for '{site}'"
            );
        }
    }

    #[cfg(not(feature = "support_gov_domain"))]
    #[test]
    fn test_parse_site_gov_rejected() {
        for site in ["ddog-gov.com", "ddog-gov.mil"] {
            assert!(
                parse_site(site).is_none(),
                "expected '{site}' to be invalid without gov feature"
            );
        }
    }

    #[cfg(feature = "support_gov_domain")]
    #[test]
    fn test_parse_site_gov_prefixed_rejected() {
        for site in [
            "us1.ddog-gov.com",
            "us3.ddog-gov.com",
            "abc1.ddog-gov.mil",
            "us1.ddog-gov.mil",
        ] {
            assert!(
                parse_site(site).is_none(),
                "expected prefixed gov '{site}' to be invalid (gov is flat-only)"
            );
        }
    }
}
