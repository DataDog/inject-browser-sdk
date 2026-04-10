// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;

use crate::configuration::{validate_configuration, Configuration, PrivacyLevel, RumConfiguration};
use crate::error::Error;

/// Builds a [`Configuration`] from a map of `DD_*` key-value pairs.
///
/// RUM-specific keys (e.g. `DD_RUM_SITE`) take precedence over generic keys (e.g. `DD_SITE`).
/// Returns a validated `Configuration` or an error.
pub fn configuration_from_entries(
    entries: &BTreeMap<String, String>,
) -> Result<Configuration, Error> {
    let get = |key: &str| -> Option<&str> { entries.get(key).map(|v| v.as_str()) };

    let application_id = get("DD_RUM_APPLICATION_ID").unwrap_or("");
    let client_token = get("DD_RUM_CLIENT_TOKEN").unwrap_or("");

    if application_id.is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from(
            "DD_RUM_APPLICATION_ID",
        )));
    }
    if client_token.is_empty() {
        return Err(Error::EmptyMandatoryConf(String::from(
            "DD_RUM_CLIENT_TOKEN",
        )));
    }

    let site = get("DD_RUM_SITE").or_else(|| get("DD_SITE")).map(Box::from);
    let service = get("DD_RUM_SERVICE")
        .or_else(|| get("DD_SERVICE"))
        .map(Box::from);
    let env = get("DD_RUM_ENVIRONMENT")
        .or_else(|| get("DD_ENV"))
        .map(Box::from);
    let version = get("DD_VERSION").map(Box::from);

    let parse_bool = |key: &str| -> Result<Option<bool>, Error> {
        match get(key) {
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                _ => Err(Error::StableConfig(format!(
                    "Invalid boolean value '{v}' for key '{key}'"
                ))),
            },
            None => Ok(None),
        }
    };

    let parse_f32 = |key: &str| -> Result<Option<f32>, Error> {
        match get(key) {
            Some(v) => {
                let val: f32 = v.parse().map_err(|_| {
                    Error::StableConfig(format!("Invalid float value '{v}' for key '{key}'"))
                })?;
                if !val.is_finite() {
                    return Err(Error::StableConfig(format!(
                        "Invalid float value '{v}' for key '{key}'"
                    )));
                }
                Ok(Some(val))
            }
            None => Ok(None),
        }
    };

    let parse_u32 = |key: &str| -> Result<Option<u32>, Error> {
        match get(key) {
            Some(v) => {
                let val: u32 = v.parse().map_err(|_| {
                    Error::StableConfig(format!("Invalid u32 value '{v}' for key '{key}'"))
                })?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    };

    let default_privacy_level = match get("DD_RUM_DEFAULT_PRIVACY_LEVEL") {
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "allow" => Some(PrivacyLevel::Allow),
            "mask" => Some(PrivacyLevel::Mask),
            "mask-user-input" => Some(PrivacyLevel::MaskUserInput),
            _ => {
                return Err(Error::StableConfig(format!(
                    "Invalid privacy level '{v}' for key 'DD_RUM_DEFAULT_PRIVACY_LEVEL'"
                )));
            }
        },
        None => None,
    };

    let major_version = parse_u32("DD_RUM_MAJOR_VERSION")?.unwrap_or(5);

    let mut other = BTreeMap::new();
    if let Some(v) = get("DD_RUM_REMOTE_CONFIGURATION_ID") {
        other.insert(
            Box::from("remoteConfigurationId"),
            serde_json::Value::String(String::from(v)),
        );
    }

    let configuration = Configuration {
        major_version,
        rum: RumConfiguration {
            application_id: Box::from(application_id),
            client_token: Box::from(client_token),
            site,
            service,
            env,
            version,
            track_user_interactions: parse_bool("DD_RUM_TRACK_USER_INTERACTIONS")?,
            track_resources: parse_bool("DD_RUM_TRACK_RESOURCES")?,
            track_long_task: parse_bool("DD_RUM_TRACK_LONG_TASKS")?,
            default_privacy_level,
            session_sample_rate: parse_f32("DD_RUM_SESSION_SAMPLE_RATE")?,
            session_replay_sample_rate: parse_f32("DD_RUM_SESSION_REPLAY_SAMPLE_RATE")?,
            other,
        },
    };

    validate_configuration(&configuration)?;
    Ok(configuration)
}
