// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;

use crate::configuration::{validate_configuration, Configuration, PrivacyLevel, RumConfiguration};
use crate::error::Error;

const DD_RUM_APPLICATION_ID: &str = "DD_RUM_APPLICATION_ID";
const DD_RUM_CLIENT_TOKEN: &str = "DD_RUM_CLIENT_TOKEN";
const DD_RUM_SITE: &str = "DD_RUM_SITE";
const DD_SITE: &str = "DD_SITE";
const DD_RUM_SERVICE: &str = "DD_RUM_SERVICE";
const DD_SERVICE: &str = "DD_SERVICE";
const DD_RUM_ENVIRONMENT: &str = "DD_RUM_ENVIRONMENT";
const DD_ENV: &str = "DD_ENV";
const DD_VERSION: &str = "DD_VERSION";
const DD_RUM_TRACK_USER_INTERACTIONS: &str = "DD_RUM_TRACK_USER_INTERACTIONS";
const DD_RUM_REMOTE_CONFIGURATION_ID: &str = "DD_RUM_REMOTE_CONFIGURATION_ID";
const DD_RUM_TRACK_RESOURCES: &str = "DD_RUM_TRACK_RESOURCES";
const DD_RUM_TRACK_LONG_TASKS: &str = "DD_RUM_TRACK_LONG_TASKS";
const DD_RUM_DEFAULT_PRIVACY_LEVEL: &str = "DD_RUM_DEFAULT_PRIVACY_LEVEL";
const DD_RUM_SESSION_SAMPLE_RATE: &str = "DD_RUM_SESSION_SAMPLE_RATE";
const DD_RUM_SESSION_REPLAY_SAMPLE_RATE: &str = "DD_RUM_SESSION_REPLAY_SAMPLE_RATE";
const DD_RUM_MAJOR_VERSION: &str = "DD_RUM_MAJOR_VERSION";

fn parse_bool(key: &str, value: &str) -> Result<bool, Error> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::InvalidValue(String::from(key), String::from(value))),
    }
}

fn parse_f32(key: &str, value: &str) -> Result<f32, Error> {
    let v = value
        .parse::<f32>()
        .map_err(|_| Error::InvalidValue(String::from(key), String::from(value)))?;
    if !v.is_finite() {
        return Err(Error::InvalidValue(String::from(key), String::from(value)));
    }
    Ok(v)
}

fn parse_u32(key: &str, value: &str) -> Result<u32, Error> {
    value
        .parse::<u32>()
        .map_err(|_| Error::InvalidValue(String::from(key), String::from(value)))
}

fn parse_privacy_level(key: &str, value: &str) -> Result<PrivacyLevel, Error> {
    match value.to_ascii_lowercase().as_str() {
        "allow" => Ok(PrivacyLevel::Allow),
        "mask" => Ok(PrivacyLevel::Mask),
        "mask-user-input" => Ok(PrivacyLevel::MaskUserInput),
        _ => Err(Error::InvalidValue(String::from(key), String::from(value))),
    }
}

/// Builds a [`Configuration`] by querying environment variable values through the provided getter.
///
/// The `getter` function is called with each known env var key and should return `Some(value)` if
/// the variable is set, or `None` if it is not.
///
/// Required env vars: `DD_RUM_APPLICATION_ID`, `DD_RUM_CLIENT_TOKEN`.
/// If `DD_RUM_MAJOR_VERSION` is not set, it defaults to `5`.
pub fn configuration_from_getter(
    getter: impl Fn(&str) -> Option<String>,
) -> Result<Configuration, Error> {
    let application_id = getter(DD_RUM_APPLICATION_ID).unwrap_or_default();
    let client_token = getter(DD_RUM_CLIENT_TOKEN).unwrap_or_default();

    if application_id.is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from(
            DD_RUM_APPLICATION_ID,
        )));
    }
    if client_token.is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from(DD_RUM_CLIENT_TOKEN)));
    }

    let site = getter(DD_RUM_SITE)
        .or_else(|| getter(DD_SITE))
        .map(|v| Box::from(v.as_str()));
    let service = getter(DD_RUM_SERVICE)
        .or_else(|| getter(DD_SERVICE))
        .map(|v| Box::from(v.as_str()));
    let env = getter(DD_RUM_ENVIRONMENT)
        .or_else(|| getter(DD_ENV))
        .map(|v| Box::from(v.as_str()));
    let version = getter(DD_VERSION).map(|v| Box::from(v.as_str()));

    let track_user_interactions = getter(DD_RUM_TRACK_USER_INTERACTIONS)
        .map(|v| parse_bool(DD_RUM_TRACK_USER_INTERACTIONS, &v))
        .transpose()?;

    let track_resources = getter(DD_RUM_TRACK_RESOURCES)
        .map(|v| parse_bool(DD_RUM_TRACK_RESOURCES, &v))
        .transpose()?;

    let track_long_task = getter(DD_RUM_TRACK_LONG_TASKS)
        .map(|v| parse_bool(DD_RUM_TRACK_LONG_TASKS, &v))
        .transpose()?;

    let default_privacy_level = getter(DD_RUM_DEFAULT_PRIVACY_LEVEL)
        .map(|v| parse_privacy_level(DD_RUM_DEFAULT_PRIVACY_LEVEL, &v))
        .transpose()?;

    let session_sample_rate = getter(DD_RUM_SESSION_SAMPLE_RATE)
        .map(|v| parse_f32(DD_RUM_SESSION_SAMPLE_RATE, &v))
        .transpose()?;

    let session_replay_sample_rate = getter(DD_RUM_SESSION_REPLAY_SAMPLE_RATE)
        .map(|v| parse_f32(DD_RUM_SESSION_REPLAY_SAMPLE_RATE, &v))
        .transpose()?;

    let major_version = getter(DD_RUM_MAJOR_VERSION)
        .map(|v| parse_u32(DD_RUM_MAJOR_VERSION, &v))
        .transpose()?
        .unwrap_or(5);

    let configuration = Configuration {
        major_version,
        rum: RumConfiguration {
            application_id: Box::from(application_id.as_str()),
            client_token: Box::from(client_token.as_str()),
            site,
            service,
            env,
            version,
            track_user_interactions,
            track_resources,
            track_long_task,
            default_privacy_level,
            session_sample_rate,
            session_replay_sample_rate,
            other: {
                let mut m = BTreeMap::new();
                if let Some(v) = getter(DD_RUM_REMOTE_CONFIGURATION_ID) {
                    m.insert(
                        Box::from("remoteConfigurationId"),
                        serde_json::Value::String(v),
                    );
                }
                m
            },
        },
    };

    validate_configuration(&configuration)?;

    Ok(configuration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use pretty_assertions::assert_eq;

    fn make_env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (String::from(*k), String::from(*v)))
            .collect()
    }

    #[test]
    fn all_fields_set() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id-123"),
            ("DD_RUM_CLIENT_TOKEN", "token-abc"),
            ("DD_SITE", "datadoghq.com"),
            ("DD_SERVICE", "my-service"),
            ("DD_ENV", "production"),
            ("DD_VERSION", "1.0.0"),
            ("DD_RUM_TRACK_USER_INTERACTIONS", "true"),
            ("DD_RUM_TRACK_RESOURCES", "false"),
            ("DD_RUM_TRACK_LONG_TASKS", "true"),
            ("DD_RUM_DEFAULT_PRIVACY_LEVEL", "mask-user-input"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "50.5"),
            ("DD_RUM_SESSION_REPLAY_SAMPLE_RATE", "25.0"),
            ("DD_RUM_MAJOR_VERSION", "5"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.application_id.as_ref(), "app-id-123");
        assert_eq!(config.rum.client_token.as_ref(), "token-abc");
        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(config.rum.service.as_deref(), Some("my-service"));
        assert_eq!(config.rum.env.as_deref(), Some("production"));
        assert_eq!(config.rum.version.as_deref(), Some("1.0.0"));
        assert_eq!(config.rum.track_user_interactions, Some(true));
        assert_eq!(config.rum.track_resources, Some(false));
        assert_eq!(config.rum.track_long_task, Some(true));
        assert_eq!(
            config.rum.default_privacy_level,
            Some(PrivacyLevel::MaskUserInput)
        );
        assert_eq!(config.rum.session_sample_rate, Some(50.5));
        assert_eq!(config.rum.session_replay_sample_rate, Some(25.0));
    }

    #[test]
    fn minimal_required_fields_only() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.major_version, 5);
        assert_eq!(config.rum.application_id.as_ref(), "app-id");
        assert_eq!(config.rum.client_token.as_ref(), "token");
        assert_eq!(config.rum.site, None);
        assert_eq!(config.rum.service, None);
        assert_eq!(config.rum.env, None);
        assert_eq!(config.rum.version, None);
        assert_eq!(config.rum.track_user_interactions, None);
        assert_eq!(config.rum.track_resources, None);
        assert_eq!(config.rum.track_long_task, None);
        assert_eq!(config.rum.default_privacy_level, None);
        assert_eq!(config.rum.session_sample_rate, None);
        assert_eq!(config.rum.session_replay_sample_rate, None);
    }

    #[test]
    fn missing_application_id() {
        let env = make_env(&[("DD_RUM_CLIENT_TOKEN", "token")]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(
            err,
            Error::EmptyMandatoryConf(String::from("DD_RUM_APPLICATION_ID"))
        );
    }

    #[test]
    fn missing_client_token() {
        let env = make_env(&[("DD_RUM_APPLICATION_ID", "app-id")]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(
            err,
            Error::EmptyMandatoryConf(String::from("DD_RUM_CLIENT_TOKEN"))
        );
    }

    #[test]
    fn invalid_bool_value() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_TRACK_RESOURCES", "yes"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 6);
        assert_eq!(
            err,
            Error::InvalidValue(String::from("DD_RUM_TRACK_RESOURCES"), String::from("yes"))
        );
    }

    #[test]
    fn invalid_float_value() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "not-a-number"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 6);
        assert_eq!(
            err,
            Error::InvalidValue(
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("not-a-number")
            )
        );
    }

    #[test]
    fn invalid_major_version() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_MAJOR_VERSION", "abc"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 6);
        assert_eq!(
            err,
            Error::InvalidValue(String::from("DD_RUM_MAJOR_VERSION"), String::from("abc"))
        );
    }

    #[test]
    fn invalid_privacy_level() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_DEFAULT_PRIVACY_LEVEL", "block"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 6);
        assert_eq!(
            err,
            Error::InvalidValue(
                String::from("DD_RUM_DEFAULT_PRIVACY_LEVEL"),
                String::from("block")
            )
        );
    }

    #[test]
    fn case_insensitive_bool() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_TRACK_USER_INTERACTIONS", "True"),
            ("DD_RUM_TRACK_RESOURCES", "FALSE"),
            ("DD_RUM_TRACK_LONG_TASKS", "tRuE"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.rum.track_user_interactions, Some(true));
        assert_eq!(config.rum.track_resources, Some(false));
        assert_eq!(config.rum.track_long_task, Some(true));
    }

    #[test]
    fn major_version_6() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_MAJOR_VERSION", "6"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.major_version, 6);
    }

    #[test]
    fn privacy_level_variants() {
        for (input, expected) in [
            ("allow", PrivacyLevel::Allow),
            ("mask", PrivacyLevel::Mask),
            ("mask-user-input", PrivacyLevel::MaskUserInput),
        ] {
            let env = make_env(&[
                ("DD_RUM_APPLICATION_ID", "app-id"),
                ("DD_RUM_CLIENT_TOKEN", "token"),
                ("DD_RUM_DEFAULT_PRIVACY_LEVEL", input),
            ]);

            let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

            assert_eq!(config.rum.default_privacy_level, Some(expected));
        }
    }

    #[test]
    fn rum_specific_overrides_generic() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_SITE", "datadoghq.com"),
            ("DD_RUM_SITE", "datadoghq.eu"),
            ("DD_SERVICE", "generic-service"),
            ("DD_RUM_SERVICE", "rum-service"),
            ("DD_ENV", "generic-env"),
            ("DD_RUM_ENVIRONMENT", "rum-env"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.eu"));
        assert_eq!(config.rum.service.as_deref(), Some("rum-service"));
        assert_eq!(config.rum.env.as_deref(), Some("rum-env"));
    }

    #[test]
    fn generic_fallback_when_rum_specific_absent() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_SITE", "datadoghq.com"),
            ("DD_SERVICE", "generic-service"),
            ("DD_ENV", "generic-env"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.rum.site.as_deref(), Some("datadoghq.com"));
        assert_eq!(config.rum.service.as_deref(), Some("generic-service"));
        assert_eq!(config.rum.env.as_deref(), Some("generic-env"));
    }

    #[test]
    fn rum_specific_only_no_generic() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_SITE", "ddog-gov.com"),
            ("DD_RUM_SERVICE", "rum-only-service"),
            ("DD_RUM_ENVIRONMENT", "rum-only-env"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(config.rum.site.as_deref(), Some("ddog-gov.com"));
        assert_eq!(config.rum.service.as_deref(), Some("rum-only-service"));
        assert_eq!(config.rum.env.as_deref(), Some("rum-only-env"));
    }

    #[test]
    fn remote_configuration_id() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_REMOTE_CONFIGURATION_ID", "remote-cfg-123"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert_eq!(
            config.rum.other.get("remoteConfigurationId"),
            Some(&serde_json::Value::String(String::from("remote-cfg-123")))
        );
    }

    #[test]
    fn remote_configuration_id_absent_leaves_other_empty() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
        ]);

        let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

        assert!(config.rum.other.is_empty());
    }

    #[test]
    fn out_of_range_session_sample_rate() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "150.0"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 4);
    }

    #[test]
    fn nan_session_sample_rate_is_invalid() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_SESSION_SAMPLE_RATE", "NaN"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 6);
        assert_eq!(
            err,
            Error::InvalidValue(
                String::from("DD_RUM_SESSION_SAMPLE_RATE"),
                String::from("NaN")
            )
        );
    }

    #[test]
    fn unsupported_major_version_zero() {
        let env = make_env(&[
            ("DD_RUM_APPLICATION_ID", "app-id"),
            ("DD_RUM_CLIENT_TOKEN", "token"),
            ("DD_RUM_MAJOR_VERSION", "0"),
        ]);

        let err = configuration_from_getter(|key| env.get(key).cloned()).unwrap_err();
        assert_eq!(err.code(), 2);
        assert_eq!(err, Error::UnsupportedMajorVersion(0));
    }

    #[test]
    fn privacy_level_case_insensitive() {
        for (input, expected) in [
            ("Allow", PrivacyLevel::Allow),
            ("MASK", PrivacyLevel::Mask),
            ("Mask-User-Input", PrivacyLevel::MaskUserInput),
        ] {
            let env = make_env(&[
                ("DD_RUM_APPLICATION_ID", "app-id"),
                ("DD_RUM_CLIENT_TOKEN", "token"),
                ("DD_RUM_DEFAULT_PRIVACY_LEVEL", input),
            ]);

            let config = configuration_from_getter(|key| env.get(key).cloned()).unwrap();

            assert_eq!(config.rum.default_privacy_level, Some(expected));
        }
    }
}
