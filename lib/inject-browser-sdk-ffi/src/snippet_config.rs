// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! A native alternative to the JSON overlay: settings are set one by one by
//! name, then layered over stable configuration when the snippet is built.

use alloc::boxed::Box;
use core::ffi::{c_char, CStr};

use inject_browser_sdk::ConfigurationOverlay;
use serde_json::Value;

use crate::snippet::Snippet;
use crate::stable_config::{build_snippet, StableConfig};

/// Settings for one snippet, built with the `snippet_config_*` functions.
///
/// Names follow the JSON overlay: `enabled` and `majorVersion` configure the
/// snippet itself, and every other name is a Browser SDK init option such as
/// `applicationId` or `sessionSampleRate`. Known options are typed; others pass
/// through to `DD_RUM.init({...})` unchanged. A set value takes precedence over
/// the stable-configuration value it overlaps. Setting a name again replaces
/// its value.
///
/// Each setter returns `false`, leaving the configuration unchanged, when a
/// pointer is NULL, a string is not UTF-8, or the value does not fit the
/// type of a known name. Callers skip that setting and keep the rest.
pub struct SnippetConfig(ConfigurationOverlay);

/// Create an empty [`SnippetConfig`]. Free it with [`snippet_config_cleanup`].
#[no_mangle]
pub extern "C" fn snippet_config_create() -> *mut SnippetConfig {
    return Box::into_raw(Box::new(SnippetConfig(ConfigurationOverlay::default())));
}

/// Free a [`SnippetConfig`]. Snippets built from it remain valid.
///
/// # Safety
///
/// `config` must be NULL or a pointer from [`snippet_config_create`] not yet freed.
#[no_mangle]
pub unsafe extern "C" fn snippet_config_cleanup(config: *mut SnippetConfig) {
    if !config.is_null() {
        drop(Box::from_raw(config));
    }
}

unsafe fn str_arg<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    return CStr::from_ptr(ptr).to_str().ok();
}

unsafe fn apply(
    config: *mut SnippetConfig,
    name: *const c_char,
    set: impl FnOnce(&mut ConfigurationOverlay, &str) -> bool,
) -> bool {
    let (Some(config), Some(name)) = (config.as_mut(), str_arg(name)) else {
        return false;
    };
    return set(&mut config.0, name);
}

/// Set a string setting.
///
/// # Safety
///
/// `config` must be NULL or a live [`SnippetConfig`]; `name` and `value` must
/// be NULL or NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn snippet_config_set_string(
    config: *mut SnippetConfig,
    name: *const c_char,
    value: *const c_char,
) -> bool {
    let Some(value) = str_arg(value) else {
        return false;
    };
    return apply(config, name, |c, n| {
        return c.set(n, Value::from(value)).is_ok();
    });
}

/// Set a boolean setting.
///
/// # Safety
///
/// As [`snippet_config_set_string`].
#[no_mangle]
pub unsafe extern "C" fn snippet_config_set_bool(
    config: *mut SnippetConfig,
    name: *const c_char,
    value: bool,
) -> bool {
    return apply(config, name, |c, n| {
        return c.set(n, Value::from(value)).is_ok();
    });
}

/// Set a numeric setting. Whole numbers also fit integer settings such as
/// `majorVersion`.
///
/// # Safety
///
/// As [`snippet_config_set_string`].
#[no_mangle]
pub unsafe extern "C" fn snippet_config_set_number(
    config: *mut SnippetConfig,
    name: *const c_char,
    value: f64,
) -> bool {
    let value = if value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&value) {
        Value::from(value as u64)
    } else {
        Value::from(value)
    };
    return apply(config, name, |c, n| return c.set(n, value).is_ok());
}

/// Append a string to a list setting such as `allowedTracingUrls`, creating
/// the list when it is not set yet.
///
/// # Safety
///
/// As [`snippet_config_set_string`].
#[no_mangle]
pub unsafe extern "C" fn snippet_config_append_string(
    config: *mut SnippetConfig,
    name: *const c_char,
    value: *const c_char,
) -> bool {
    let Some(value) = str_arg(value) else {
        return false;
    };
    return apply(config, name, |c, n| {
        return c.append(n, Value::from(value)).is_ok();
    });
}

/// Set a setting from its text form, as configuration files hold it: the text
/// is read as the type the name takes, and list options are split on commas.
/// Unknown names keep the text as a string.
///
/// # Safety
///
/// As [`snippet_config_set_string`].
#[no_mangle]
pub unsafe extern "C" fn snippet_config_set_text(
    config: *mut SnippetConfig,
    name: *const c_char,
    text: *const c_char,
) -> bool {
    let Some(text) = str_arg(text) else {
        return false;
    };
    return apply(config, name, |c, n| return c.set_text(n, text).is_ok());
}

/// Build a snippet from `settings` layered over `stable`, or from `settings`
/// alone when `stable` is NULL. A NULL `settings` builds from `stable` alone.
/// Neither argument is consumed. See [`crate::stable_config_snippet_create`]
/// for the error codes and resolved values.
///
/// # Safety
///
/// `stable` must be NULL or a live handle from [`crate::stable_config_read`];
/// `settings` must be NULL or a live [`SnippetConfig`].
#[no_mangle]
pub unsafe extern "C" fn stable_config_snippet_create_with_config(
    stable: *const StableConfig,
    settings: *const SnippetConfig,
) -> *mut Snippet {
    let overlay = settings.as_ref().map(|settings| return settings.0.clone());
    return build_snippet(stable, overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet::{snippet_cleanup, RUM_ENABLED_FALSE};
    use crate::test_utils::cstr_ptr;
    use core::ptr::null;

    unsafe fn snippet_text(snippet: *mut Snippet) -> alloc::string::String {
        let internal = crate::snippet::SnippetInternal::borrow_public_ptr(snippet);
        let text = match &internal.inner {
            Ok(bytes) => alloc::string::String::from_utf8(bytes.clone()).unwrap(),
            Err(message) => panic!("snippet error: {message:?}"),
        };
        snippet_cleanup(snippet);
        return text;
    }

    unsafe fn credentials() -> *mut SnippetConfig {
        let config = snippet_config_create();
        assert!(snippet_config_set_string(
            config,
            cstr_ptr(b"applicationId\0"),
            cstr_ptr(b"app\0")
        ));
        assert!(snippet_config_set_string(
            config,
            cstr_ptr(b"clientToken\0"),
            cstr_ptr(b"tok\0")
        ));
        return config;
    }

    #[test]
    fn typed_setters_reach_the_snippet() {
        unsafe {
            let config = credentials();
            assert!(snippet_config_set_number(
                config,
                cstr_ptr(b"majorVersion\0"),
                5.0
            ));
            assert!(snippet_config_set_number(
                config,
                cstr_ptr(b"sessionSampleRate\0"),
                12.5
            ));
            assert!(snippet_config_set_bool(
                config,
                cstr_ptr(b"trackResources\0"),
                true
            ));
            assert!(snippet_config_append_string(
                config,
                cstr_ptr(b"allowedTracingUrls\0"),
                cstr_ptr(b"https://a\0")
            ));
            assert!(snippet_config_append_string(
                config,
                cstr_ptr(b"allowedTracingUrls\0"),
                cstr_ptr(b"https://b\0")
            ));
            assert!(snippet_config_set_string(
                config,
                cstr_ptr(b"futureOption\0"),
                cstr_ptr(b"x\0")
            ));

            let text = snippet_text(stable_config_snippet_create_with_config(null(), config));
            snippet_config_cleanup(config);
            assert!(text.contains("/v5/datadog-rum.js"), "{text}");
            assert!(text.contains(r#""sessionSampleRate":12.5"#), "{text}");
            assert!(text.contains(r#""trackResources":true"#), "{text}");
            assert!(
                text.contains(r#""allowedTracingUrls":["https://a","https://b"]"#),
                "{text}"
            );
            assert!(text.contains(r#""futureOption":"x""#), "{text}");
        }
    }

    #[test]
    fn text_setter_types_values_by_name() {
        unsafe {
            let config = credentials();
            for (name, text) in [
                (&b"majorVersion\0"[..], &b"5\0"[..]),
                (b"sessionSampleRate\0", b"50\0"),
                (b"trackLongTasks\0", b"TRUE\0"),
                (b"allowedTracingUrls\0", b" https://a , ,https://b\0"),
                (b"service\0", b"42\0"),
            ] {
                assert!(snippet_config_set_text(
                    config,
                    cstr_ptr(name),
                    cstr_ptr(text)
                ));
            }
            let text = snippet_text(stable_config_snippet_create_with_config(null(), config));
            snippet_config_cleanup(config);
            assert!(text.contains("/v5/datadog-rum.js"), "{text}");
            assert!(text.contains(r#""sessionSampleRate":50"#), "{text}");
            assert!(text.contains(r#""trackLongTasks":true"#), "{text}");
            assert!(
                text.contains(r#""allowedTracingUrls":["https://a","https://b"]"#),
                "{text}"
            );
            assert!(text.contains(r#""service":"42""#), "{text}");
        }
    }

    #[test]
    fn mismatched_values_are_rejected_and_leave_the_rest() {
        unsafe {
            let config = credentials();
            let rate = cstr_ptr(b"sessionSampleRate\0");
            assert!(!snippet_config_set_text(config, rate, cstr_ptr(b"abc\0")));
            assert!(!snippet_config_set_bool(config, rate, true));
            assert!(!snippet_config_set_string(
                config,
                cstr_ptr(b"enabled\0"),
                cstr_ptr(b"x\0")
            ));
            assert!(!snippet_config_set_text(
                config,
                cstr_ptr(b"allowedTracingUrls\0"),
                cstr_ptr(b" , \0")
            ));
            assert!(!snippet_config_append_string(
                config,
                cstr_ptr(b"site\0"),
                cstr_ptr(b"x\0")
            ));
            assert!(!snippet_config_set_string(config, null(), cstr_ptr(b"x\0")));
            assert!(!snippet_config_set_string(
                null_mut_config(),
                rate,
                cstr_ptr(b"x\0")
            ));

            let text = snippet_text(stable_config_snippet_create_with_config(null(), config));
            snippet_config_cleanup(config);
            assert!(!text.contains("sessionSampleRate"), "{text}");
            assert!(text.contains(r#""applicationId":"app""#), "{text}");
        }
    }

    fn null_mut_config() -> *mut SnippetConfig {
        return core::ptr::null_mut();
    }

    #[test]
    fn disabled_setting_reports_rum_disabled() {
        unsafe {
            let config = snippet_config_create();
            assert!(snippet_config_set_text(
                config,
                cstr_ptr(b"enabled\0"),
                cstr_ptr(b"false\0")
            ));
            let snippet = stable_config_snippet_create_with_config(null(), config);
            snippet_config_cleanup(config);
            assert_eq!((*snippet).error_code, 0);
            assert_eq!((*snippet).rum_enabled, RUM_ENABLED_FALSE);
            snippet_cleanup(snippet);
        }
    }

    #[test]
    fn missing_credentials_fail_validation() {
        unsafe {
            let config = snippet_config_create();
            let snippet = stable_config_snippet_create_with_config(null(), config);
            snippet_config_cleanup(config);
            assert_eq!((*snippet).error_code, 5);
            snippet_cleanup(snippet);
        }
    }

    #[test]
    fn settings_overlay_stable_config_and_host_keys_are_readable() {
        extern crate std;
        let path = std::env::temp_dir().join("snippet_config_test_application_monitoring.yaml");
        std::fs::write(
            &path,
            "apm_configuration_default:\n  DD_RUM_APPLICATION_ID: stable-app\n  \
             DD_RUM_CLIENT_TOKEN: stable-tok\n  DD_RUM_SERVICE: stable-svc\n  \
             DD_RUM_INJECTION_STRATEGY: head_open_tag\n",
        )
        .unwrap();
        let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let missing = cstr_ptr(b"/nonexistent/application_monitoring.yaml\0");
        unsafe {
            let stable = crate::stable_config_read_with_paths(
                cstr_ptr(b"browser-sdk\0"),
                false,
                path.as_ptr(),
                missing,
            );
            assert_eq!((*stable).error_code, 0);
            let strategy =
                crate::stable_config_get(stable, cstr_ptr(b"DD_RUM_INJECTION_STRATEGY\0"));
            assert_eq!(CStr::from_ptr(strategy).to_str(), Ok("head_open_tag"));
            assert!(crate::stable_config_get(stable, cstr_ptr(b"DD_RUM_SITE\0")).is_null());

            let config = snippet_config_create();
            assert!(snippet_config_set_text(
                config,
                cstr_ptr(b"service\0"),
                cstr_ptr(b"iis-svc\0")
            ));
            let text = snippet_text(stable_config_snippet_create_with_config(stable, config));
            snippet_config_cleanup(config);
            crate::stable_config_cleanup(stable);
            assert!(text.contains(r#""applicationId":"stable-app""#), "{text}");
            assert!(text.contains(r#""service":"iis-svc""#), "{text}");
        }
    }
}
