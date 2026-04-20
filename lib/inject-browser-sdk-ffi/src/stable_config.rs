// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::format;
use core::ffi::{c_char, CStr};

use inject_browser_sdk::{error::Error, snippet_from_stable_config, ConfigurationOverlay};

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

/// Create a snippet from Datadog stable configuration files, with an optional JSON overlay.
///
/// Reads configuration from the Datadog Agent's `application_monitoring.yaml` files
/// (local and fleet-managed). If `json_overlay` is non-null, it is parsed as a JSON
/// object with the same shape as [`snippet_create_from_json`] — except every field
/// is optional — and layered on top of the disk-sourced configuration.
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr.
///
/// # Error handling
///
/// Always returns a valid `Snippet` pointer. Check `error_code` for failures.
/// A malformed `json_overlay` surfaces as error code `1` (JSON error).
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
/// Snippet* s = snippet_create_from_stable_config("browser-sdk", false, NULL);
///
/// // Overlay a JSON blob:
/// const char* overlay =
///     "{\"rum\":{\"applicationId\":\"abc\",\"site\":\"datadoghq.com\"}}";
/// Snippet* s = snippet_create_from_stable_config("browser-sdk", false, overlay);
///
/// snippet_cleanup(s);
/// ```
///
/// # Safety
///
/// - `language` must be a valid, non-null, NUL-terminated C string.
/// - If `json_overlay` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_stable_config(
    language: *const c_char,
    debug_logs: bool,
    json_overlay: *const c_char,
) -> *mut Snippet {
    if language.is_null() {
        return SnippetInternal::stable_config_err("language parameter is NULL");
    }

    let language_str = match cstr_to_str(language, "language") {
        Ok(s) => s,
        Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
    };

    let overlay = if json_overlay.is_null() {
        None
    } else {
        match serde_json::from_slice::<ConfigurationOverlay>(
            CStr::from_ptr(json_overlay).to_bytes(),
        ) {
            Ok(o) => Some(o),
            Err(e) => {
                return SnippetInternal::new(Err(Error::Json(format!("{e}")))).into_public_ptr();
            }
        }
    };

    let result = snippet_from_stable_config(language_str, debug_logs, overlay);
    SnippetInternal::new(result).into_public_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr::null;
    use pretty_assertions::{assert_eq, assert_ne};
    use std::ptr::null_mut;

    use crate::snippet::{snippet_cleanup, SnippetInternal};
    use crate::test_utils::cstr_ptr;

    #[test]
    fn no_files_no_overlay() {
        let snippet =
            unsafe { snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, null()) };
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
        let snippet = unsafe { snippet_create_from_stable_config(null(), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn overlay_supplies_required_fields() {
        let snippet = unsafe {
            snippet_create_from_stable_config(
                cstr_ptr(b"browser-sdk\0"),
                false,
                cstr_ptr(b"{\"rum\":{\"applicationId\":\"app-1\",\"clientToken\":\"tok-1\"}}\0"),
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
    fn invalid_overlay_json() {
        let snippet = unsafe {
            snippet_create_from_stable_config(
                cstr_ptr(b"browser-sdk\0"),
                false,
                cstr_ptr(b"{not json\0"),
            )
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 1);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn overlay_type_mismatch_fails_at_parse() {
        let snippet = unsafe {
            snippet_create_from_stable_config(
                cstr_ptr(b"browser-sdk\0"),
                false,
                cstr_ptr(b"{\"rum\":{\"applicationId\":42}}\0"),
            )
        };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 1);
        unsafe { snippet_cleanup(snippet) };
    }
}
