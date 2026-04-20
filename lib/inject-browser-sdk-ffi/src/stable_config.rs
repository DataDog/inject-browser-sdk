// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::{format, string::String};
use core::ffi::{c_char, CStr};

use inject_browser_sdk::{error::Error, snippet_from_stable_config};

use crate::snippet::{Snippet, SnippetInternal};

/// Validate a `*const c_char`: checks for null, reads the C string, and validates UTF-8.
/// Returns the string slice on success, or `Error::StableConfig` on failure.
///
/// # Safety
/// If non-null, `ptr` must point to a valid NUL-terminated C string.
unsafe fn cstr_to_str<'a>(ptr: *const c_char, field: &str) -> Result<&'a str, Error> {
    if ptr.is_null() {
        return Err(Error::StableConfig(format!("{field} pointer is NULL")));
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map_err(|_| Error::StableConfig(format!("{field} is not valid UTF-8")))
}

/// A configuration override: a `DD_RUM_*` key name paired with a C string value.
///
/// Used with [`snippet_create_from_stable_config`] to supply or override values
/// on top of what the Agent's stable configuration files provide.
#[repr(C)]
pub struct ConfigOverride {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// Create a snippet from Datadog stable configuration files, with optional overrides.
///
/// Reads configuration from the Datadog Agent's `application_monitoring.yaml` files
/// (local and fleet-managed). Caller-supplied `overrides` take precedence over values
/// read from disk.
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr.
///
/// # Overrides
///
/// Pass `overrides` / `override_count` to supply or replace specific options.
/// Pass `NULL` / `0` to use only the on-disk configuration.
///
/// # Error handling
///
/// Always returns a valid `Snippet` pointer. Check `error_code` for failures.
///
/// # Memory ownership
///
/// All input pointers are owned by the caller. The returned `Snippet` must be freed
/// with [`crate::snippet_cleanup`].
///
/// # Example
///
/// ```c
/// // Use stable config as-is:
/// Snippet* s = snippet_create_from_stable_config("browser-sdk", false, NULL, 0);
///
/// // Override the application id:
/// ConfigOverride overrides[] = {
///     { .name = "DD_RUM_APPLICATION_ID", .value = "abc" },
///     { .name = "DD_RUM_SITE",           .value = "datadoghq.com" },
/// };
/// Snippet* s = snippet_create_from_stable_config(
///     "browser-sdk", false, overrides, 2);
///
/// snippet_cleanup(s);
/// ```
///
/// # Safety
///
/// - `language` must be a valid, non-null, NUL-terminated C string.
/// - If `override_count > 0`, `overrides` must point to a valid array of at least
///   `override_count` [`ConfigOverride`] entries, each with valid NUL-terminated
///   `name` and `value`.
/// - When `override_count == 0`, `overrides` is ignored and may be `NULL` or dangling.
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_stable_config(
    language: *const c_char,
    debug_logs: bool,
    overrides: *const ConfigOverride,
    override_count: u32,
) -> *mut Snippet {
    if language.is_null() {
        return SnippetInternal::stable_config_err("language parameter is NULL");
    }

    let override_vec = match parse_overrides(overrides, override_count) {
        Ok(v) => v,
        Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
    };

    let language_str = match cstr_to_str(language, "language") {
        Ok(s) => s,
        Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
    };

    let overrides_refs: alloc::vec::Vec<(&str, &str)> = override_vec
        .iter()
        .map(|(name, val)| (name.as_str(), val.as_str()))
        .collect();

    let result = snippet_from_stable_config(language_str, debug_logs, &overrides_refs);
    SnippetInternal::new(result).into_public_ptr()
}

/// Parse a C array of [`ConfigOverride`] into a `Vec` of (name, value) string pairs.
///
/// Rejects overrides whose `name` is not a known `DD_RUM_*` key accepted by
/// [`inject_browser_sdk::Configuration::from_lookup`], so typos surface at
/// ingestion rather than being silently ignored.
///
/// # Safety
///
/// - If `count > 0`, `entries` must point to a valid, readable array of at
///   least `count` [`ConfigOverride`] elements.
/// - Each `ConfigOverride::name` and `ConfigOverride::value` must be a valid
///   NUL-terminated C string, or null (which returns an error rather than UB).
unsafe fn parse_overrides(
    entries: *const ConfigOverride,
    count: u32,
) -> Result<alloc::vec::Vec<(String, String)>, Error> {
    if entries.is_null() && count > 0 {
        return Err(Error::StableConfig(String::from(
            "overrides pointer is NULL but count is non-zero",
        )));
    }
    let slice = if count == 0 || entries.is_null() {
        &[]
    } else {
        core::slice::from_raw_parts(entries, count as usize)
    };
    let mut result = alloc::vec::Vec::with_capacity(slice.len());
    for entry in slice {
        let name = cstr_to_str(entry.name, "Override name")?;
        if !is_known_override_name(name) {
            return Err(Error::StableConfig(format!(
                "Unknown override name: '{name}'"
            )));
        }
        let value = cstr_to_str(entry.value, &format!("Value for override '{name}'"))?;
        result.push((String::from(name), String::from(value)));
    }
    Ok(result)
}

/// Returns true if `name` is a `DD_RUM_*` key accepted by [`Configuration::from_lookup`].
fn is_known_override_name(name: &str) -> bool {
    // Source of truth: DD_FIELDS in lib/inject-browser-sdk/src/configuration.rs.
    // Listed explicitly here so we fail closed if from_lookup ever accepts a new key
    // we haven't decided to expose through the FFI.
    matches!(
        name,
        "DD_RUM_APPLICATION_ID"
            | "DD_RUM_CLIENT_TOKEN"
            | "DD_RUM_SITE"
            | "DD_RUM_SERVICE"
            | "DD_RUM_ENVIRONMENT"
            | "DD_RUM_VERSION"
            | "DD_RUM_TRACK_USER_INTERACTIONS"
            | "DD_RUM_TRACK_RESOURCES"
            | "DD_RUM_TRACK_LONG_TASKS"
            | "DD_RUM_DEFAULT_PRIVACY_LEVEL"
            | "DD_RUM_SESSION_SAMPLE_RATE"
            | "DD_RUM_SESSION_REPLAY_SAMPLE_RATE"
            | "DD_RUM_MAJOR_VERSION"
            | "DD_RUM_REMOTE_CONFIGURATION_ID"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::ffi::CString;
    use core::ptr::null;
    use pretty_assertions::{assert_eq, assert_ne};
    use std::ptr::null_mut;

    use crate::snippet::{snippet_cleanup, SnippetInternal};
    use crate::test_utils::cstr_ptr;

    #[test]
    fn no_files_no_overrides() {
        let snippet = unsafe {
            snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, null(), 0)
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert!(
            matches!(s.error_code, 5 | 6),
            "expected error code 5 or 6, got {}",
            s.error_code
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn null_language() {
        let snippet = unsafe { snippet_create_from_stable_config(null(), false, null(), 0) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn overrides_supply_required_fields() {
        let app_val = CString::new("app-1").unwrap();
        let tok_val = CString::new("tok-1").unwrap();

        let overrides = [
            ConfigOverride {
                name: cstr_ptr(b"DD_RUM_APPLICATION_ID\0"),
                value: app_val.as_ptr(),
            },
            ConfigOverride {
                name: cstr_ptr(b"DD_RUM_CLIENT_TOKEN\0"),
                value: tok_val.as_ptr(),
            },
        ];

        let snippet = unsafe {
            snippet_create_from_stable_config(
                cstr_ptr(b"browser-sdk\0"),
                false,
                overrides.as_ptr(),
                overrides.len() as u32,
            )
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 0, "unexpected error: {:?}", unsafe {
            s.error_message.as_ref().map(|_| {
                CStr::from_ptr(s.error_message)
                    .to_string_lossy()
                    .into_owned()
            })
        });
        assert!(s.length > 0);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn null_overrides_with_nonzero_count() {
        let snippet = unsafe {
            snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, null(), 5)
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn null_override_value() {
        let entry = ConfigOverride {
            name: cstr_ptr(b"DD_RUM_SERVICE\0"),
            value: null(),
        };
        let snippet = unsafe {
            snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, &entry, 1)
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn unknown_override_name() {
        let val = CString::new("ignored").unwrap();
        let entry = ConfigOverride {
            name: cstr_ptr(b"DD_RUM_BOGUS\0"),
            value: val.as_ptr(),
        };
        let snippet = unsafe {
            snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, &entry, 1)
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        let msg = unsafe {
            CStr::from_ptr(s.error_message)
                .to_string_lossy()
                .into_owned()
        };
        assert!(
            msg.contains("Unknown override name: 'DD_RUM_BOGUS'"),
            "unexpected error message: {msg}"
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn null_override_name() {
        let val = CString::new("ignored").unwrap();
        let entry = ConfigOverride {
            name: null(),
            value: val.as_ptr(),
        };
        let snippet = unsafe {
            snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, &entry, 1)
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }
}
