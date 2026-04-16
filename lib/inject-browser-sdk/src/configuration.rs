// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use crate::error::Error;
use alloc::boxed::Box;
use alloc::collections::BTreeMap as Map;
use alloc::string::{String, ToString};
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
/// Internally stored as a flat `BTreeMap<Box<str>, Value>` to support both
/// known fields and forward-compatible unknown fields from future SDK versions.
///
/// Serializes as a flat JSON object (e.g. `{"applicationId":"...","clientToken":"...",...}`).
/// Deserializes from the same flat JSON object via `#[serde(flatten)]`.
#[derive(Default)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct RumConfiguration {
    pub fields: Map<Box<str>, Value>,
}

impl Serialize for RumConfiguration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (k, v) in &self.fields {
            map.serialize_entry(k.as_ref(), v)?;
        }
        map.end()
    }
}

/// Helper struct used only for deserialization: captures all JSON keys into a flat map.
#[derive(Deserialize)]
struct RumConfigurationHelper {
    #[serde(flatten)]
    fields: Map<Box<str>, Value>,
}

impl<'de> Deserialize<'de> for RumConfiguration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let helper = RumConfigurationHelper::deserialize(deserializer)?;
        Ok(RumConfiguration {
            fields: helper.fields,
        })
    }
}

const SITE_PATTERNS: [&str; 4] = ["datadog", "ddog", "datad0g", "dd0g"];
const MAJOR_VERSIONS: [u32; 2] = [5, 6];

pub fn validate_configuration(conf: &Configuration) -> Result<(), Error> {
    if !MAJOR_VERSIONS.contains(&conf.major_version) {
        return Err(Error::UnsupportedMajorVersion(conf.major_version));
    }

    let get_str = |key: &str| conf.rum.fields.get(key).and_then(|v| v.as_str());
    let get_f64 = |key: &str| conf.rum.fields.get(key).and_then(|v| v.as_f64());

    if get_str("applicationId").unwrap_or("").is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from("applicationId")));
    }

    if get_str("clientToken").unwrap_or("").is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from("clientToken")));
    }

    if let Some(site) = get_str("site") {
        if !SITE_PATTERNS.iter().any(|&pattern| site.contains(pattern)) {
            return Err(Error::UnsupportedSite(site.to_string()));
        }
    }

    let validate_rate = |key: &str, label: &str| {
        if let Some(rate) = get_f64(key) {
            if !(0.0..=100.0).contains(&rate) {
                return Err(Error::OutOfRangeRate(String::from(label), rate as f32));
            }
        }
        Ok(())
    };

    validate_rate("sessionSampleRate", "session sample rate")?;
    validate_rate("sessionReplaySampleRate", "session replay sample rate")?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use alloc::vec;
    use pretty_assertions::assert_eq;

    fn rum_config(pairs: &[(&str, Value)]) -> RumConfiguration {
        RumConfiguration {
            fields: pairs
                .iter()
                .map(|(k, v)| (Box::from(*k), v.clone()))
                .collect(),
        }
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
        assert_eq!(
            configuration
                .rum
                .fields
                .get("clientToken")
                .and_then(|v| v.as_str()),
            Some("foo")
        );
        assert_eq!(
            configuration
                .rum
                .fields
                .get("applicationId")
                .and_then(|v| v.as_str()),
            Some("bar")
        );
        assert_eq!(
            configuration.rum.fields.get("newConfig"),
            Some(&Value::from("a"))
        );
        assert_eq!(
            configuration.rum.fields.get("newConfig2"),
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
                    rum: rum_config(&[
                        ("applicationId", Value::from("")),
                        ("clientToken", Value::from("foo")),
                    ]),
                },
                expected: Error::EmptyMandatoryConf(String::from("applicationId")),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: rum_config(&[
                        ("applicationId", Value::from("bar")),
                        ("clientToken", Value::from("")),
                    ]),
                },
                expected: Error::EmptyMandatoryConf(String::from("clientToken")),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: rum_config(&[
                        ("clientToken", Value::from("foo")),
                        ("applicationId", Value::from("bar")),
                        ("sessionSampleRate", serde_json::json!(105.0)),
                    ]),
                },
                expected: Error::OutOfRangeRate(String::from("session sample rate"), 105.),
            },
            TestCase {
                input: Configuration {
                    major_version: 5,
                    rum: rum_config(&[
                        ("clientToken", Value::from("foo")),
                        ("applicationId", Value::from("bar")),
                        ("sessionReplaySampleRate", serde_json::json!(2469.0)),
                    ]),
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
                    rum: rum_config(&[
                        ("clientToken", Value::from("foo")),
                        ("applicationId", Value::from("bar")),
                        ("site", Value::from("example.com")),
                    ]),
                },
                expected: Error::UnsupportedSite(String::from("example.com")),
            },
        ];

        for test_case in test_cases {
            assert_eq!(
                validate_configuration(&test_case.input).unwrap_err().code(),
                test_case.expected.code()
            );
        }
    }
}
