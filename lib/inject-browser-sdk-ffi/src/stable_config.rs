// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::{format, string::String};
use core::ffi::{c_char, CStr};

use inject_browser_sdk::{config_option::ConfigOption, error::Error, snippet_from_stable_config};

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

/// A typed configuration override: a [`ConfigOption`] variant paired with a C string value.
///
/// Used with [`snippet_create_from_stable_config`] to supply or override values
/// on top of what the Agent's stable configuration files provide.
#[repr(C)]
pub struct ConfigOverride {
    pub config_option: ConfigOption,
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
/// with [`snippet_cleanup`].
///
/// # Example
///
/// ```c
/// // Use stable config as-is:
/// Snippet* s = snippet_create_from_stable_config("browser-sdk", false, NULL, 0);
///
/// // Override the service name:
/// ConfigOverride overrides[] = {
///     { .config_option = ConfigOption_Service, .value = "my-custom-service" },
/// };
/// Snippet* s = snippet_create_from_stable_config(
///     "browser-sdk", false, overrides, 1);
///
/// snippet_cleanup(s);
/// ```
///
/// # Safety
///
/// - `language` must be a valid, non-null, NUL-terminated C string.
/// - If `override_count > 0`, `overrides` must point to a valid array of at least
///   `override_count` [`ConfigOverride`] entries, each with a valid NUL-terminated `value`.
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

    let overrides_refs: alloc::vec::Vec<(&ConfigOption, &str)> = override_vec
        .iter()
        .map(|(opt, val)| (opt, val.as_str()))
        .collect();

    let result = snippet_from_stable_config(language_str, debug_logs, &overrides_refs);
    SnippetInternal::new(result).into_public_ptr()
}

/// Parse a C array of [`ConfigOverride`] into a `Vec` of (ConfigOption, value) pairs.
unsafe fn parse_overrides(
    entries: *const ConfigOverride,
    count: u32,
) -> Result<alloc::vec::Vec<(ConfigOption, String)>, Error> {
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
        let key = entry.config_option.key();
        let value = cstr_to_str(entry.value, &format!("Value for override '{key}'"))?;
        result.push((entry.config_option, String::from(value)));
    }
    Ok(result)
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
                config_option: ConfigOption::ApplicationId,
                value: app_val.as_ptr(),
            },
            ConfigOverride {
                config_option: ConfigOption::ClientToken,
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
            config_option: ConfigOption::Service,
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
}
