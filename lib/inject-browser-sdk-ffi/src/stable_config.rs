// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::string::{String, ToString};
use core::ffi::{c_char, CStr};
use core::ptr::null;

use inject_browser_sdk::{
    error::Error, parse_configuration_overlay, read_stable_config, snippet_from_stable_config,
    ConfigurationOverlay, StableConfig as CoreStableConfig,
};

use crate::snippet::{Snippet, SnippetInternal};

/// UTF-8 validates a borrowed `CStr`. The returned `&str` is tied to the
/// `CStr`'s lifetime so callers cannot accidentally extend it.
fn cstr_to_str<'a>(cstr: &'a CStr, field: &str) -> Result<&'a str, Error> {
    return cstr
        .to_str()
        .map_err(|_| return Error::FfiInvalidUtf8(field.to_string()));
}

/// Create a snippet from Datadog stable configuration files, with an optional JSON overlay.
///
/// Reads configuration from the Datadog Agent's `application_monitoring.yaml` files
/// (local and fleet-managed). If `json_overlay` is non-null, it is parsed as a JSON
/// object with the same shape as [`crate::snippet_create_from_json`] — except every
/// field is optional — and layered on top of the disk-sourced configuration.
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`)
/// and is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, diagnostic messages are written to stderr on Unix.
/// On Windows `debug_logs` produces no stderr output (tracked as
/// DataDog/libdatadog#73); diagnostic messages are always folded into the
/// `Snippet`'s error message so Windows callers can still see them.
///
/// # Overlay semantics
///
/// - **Missing keys**: leave the disk value untouched.
/// - **Present typed keys** (e.g. `"applicationId":"abc"`): replace the disk
///   value.
/// - **Explicit `null`** on a typed key (e.g. `"applicationId":null`): leaves
///   the disk value untouched — it is *not* a way to clear a field. To
///   replace a disk value, pass the new value; omitting the key preserves
///   the disk value.
/// - **Unknown keys at the top level** (outside `rum`): rejected with
///   `error_code = 1` (typos like `"rumm"` surface as JSON parse errors).
/// - **Unknown keys inside `rum`**: passed through into the generated
///   `DD_RUM.init({...})` JSON for forward-compat with new Browser SDK
///   init options. The trade-off is that typos inside `rum` are not caught
///   here and reach the Browser SDK as-is.
///
/// # Error handling
///
/// Always returns a valid `Snippet` pointer. Check `error_code` for failures.
/// See [`Snippet`] for the full error-code table. The codes most relevant to
/// this call are `1` (malformed `json_overlay`, including unknown top-level
/// keys), `6` (a stable-config file was there but could not be read or parsed),
/// `10` (there was no stable configuration to build from — the ordinary case on
/// a host not using it, and worth distinguishing from `6` so callers can stay
/// quiet about it), `7` (NULL `language`), and `8` (non-UTF-8 `language`).
/// Overlay values that fail validation also produce codes `2`–`5`.
///
/// # Resolved values
///
/// The returned snippet reports `rum_enabled`, `application_id`, and
/// `remote_configuration_id` from the merged configuration.
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
/// Snippet* s1 = snippet_create_from_stable_config("browser-sdk", false, NULL);
/// // ... use s1
/// snippet_cleanup(s1);
///
/// // Overlay a JSON blob:
/// const char* overlay =
///     "{\"rum\":{\"applicationId\":\"abc\",\"site\":\"datadoghq.com\"}}";
/// Snippet* s2 = snippet_create_from_stable_config("browser-sdk", false, overlay);
/// // ... use s2
/// snippet_cleanup(s2);
/// ```
///
/// # Safety
///
/// - If `language` is non-null, it must point to a valid NUL-terminated C
///   string. NULL is tolerated and returns error code 7.
/// - If `json_overlay` is non-null, it must point to a valid NUL-terminated
///   C string.
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_stable_config(
    language: *const c_char,
    debug_logs: bool,
    json_overlay: *const c_char,
) -> *mut Snippet {
    if language.is_null() {
        return SnippetInternal::new(Err(Error::FfiNullPointer(String::from("language"))))
            .into_public_ptr();
    }

    let language_cstr = CStr::from_ptr(language);
    let language_str = match cstr_to_str(language_cstr, "language") {
        Ok(s) => s,
        Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
    };

    let overlay = if json_overlay.is_null() {
        None
    } else {
        match parse_configuration_overlay(CStr::from_ptr(json_overlay).to_bytes()) {
            Ok(o) => Some(o),
            Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
        }
    };

    let result = snippet_from_stable_config(language_str, debug_logs, overlay);
    return SnippetInternal::new_from_stable_config(result).into_public_ptr();
}

/// Stable configuration read once for building multiple snippets.
#[repr(C)]
pub struct StableConfig {
    /// The error code, or `0` when the read succeeded.
    pub error_code: u8,
    /// The error message. `NULL` when the read succeeded.
    pub error_message: *const c_char,
    /// Whether no configuration entry matched this process.
    pub is_empty: bool,
}

/// Mirrors [`StableConfig`]'s layout, with the non-FFI-safe payload appended.
#[repr(C)]
struct StableConfigInternal {
    error_code: u8,
    error_message: *const c_char,
    is_empty: bool,
    // Internal: not FFI-safe, may change without a breaking change.
    inner: Result<CoreStableConfig, Error>,
    // Owns the allocation referenced by `error_message` on the error path.
    _error_message_storage: Option<CString>,
}

impl StableConfigInternal {
    fn new(result: Result<CoreStableConfig, Error>) -> Self {
        match result {
            Ok(config) => {
                return Self {
                    error_code: 0,
                    error_message: null(),
                    is_empty: config.is_empty(),
                    inner: Ok(config),
                    _error_message_storage: None,
                }
            }
            Err(error) => {
                let error_string = error.to_string().replace('\0', "\\0");
                let error_message = CString::new(error_string).unwrap_or_else(|_| {
                    return CString::new(
                        "internal error: error message contained unrepresentable bytes",
                    )
                    .expect("static message has no NUL bytes");
                });
                let error_code = error.code();
                return Self {
                    error_code,
                    error_message: error_message.as_ptr(),
                    is_empty: true,
                    inner: Err(error),
                    _error_message_storage: Some(error_message),
                };
            }
        }
    }

    fn into_public_ptr(self) -> *mut StableConfig {
        return Box::into_raw(Box::new(self)) as *mut StableConfig;
    }

    unsafe fn from_public_ptr(config: *mut StableConfig) -> Box<Self> {
        return Box::from_raw(config as *mut StableConfigInternal);
    }

    unsafe fn borrow_public_ptr(config: *const StableConfig) -> &'static Self {
        return &*(config as *const StableConfigInternal);
    }
}

/// Read stable configuration once for reuse across several snippets.
///
/// An absent `application_monitoring.yaml` is not an error: the returned handle
/// has `is_empty == true`. Read and parse failures set `error_code`.
///
/// # Memory ownership
///
/// `language` is owned by the caller. The returned `StableConfig` must be freed
/// with [`stable_config_cleanup`].
///
/// # Safety
///
/// If `language` is non-null, it must point to a valid NUL-terminated C string.
/// NULL is tolerated and returns error code 7.
#[no_mangle]
pub unsafe extern "C" fn stable_config_read(
    language: *const c_char,
    debug_logs: bool,
) -> *mut StableConfig {
    if language.is_null() {
        return StableConfigInternal::new(Err(Error::FfiNullPointer(String::from("language"))))
            .into_public_ptr();
    }

    let language_cstr = CStr::from_ptr(language);
    let language_str = match cstr_to_str(language_cstr, "language") {
        Ok(s) => s,
        Err(e) => return StableConfigInternal::new(Err(e)).into_public_ptr(),
    };

    return StableConfigInternal::new(read_stable_config(language_str, debug_logs))
        .into_public_ptr();
}

/// Build a snippet from an already-read [`StableConfig`]. Does no file I/O.
///
/// # Memory ownership
///
/// `config` stays owned by the caller and may be reused for further calls. The
/// returned `Snippet` must be freed with [`crate::snippet_cleanup`].
///
/// # Safety
///
/// - `config` must be a non-NULL pointer returned by [`stable_config_read`] and
///   not yet passed to [`stable_config_cleanup`].
/// - If `json_overlay` is non-null, it must point to a valid NUL-terminated C
///   string.
#[no_mangle]
pub unsafe extern "C" fn stable_config_snippet_create(
    config: *const StableConfig,
    json_overlay: *const c_char,
) -> *mut Snippet {
    if config.is_null() {
        return SnippetInternal::new(Err(Error::FfiNullPointer(String::from("config"))))
            .into_public_ptr();
    }

    let overlay: Option<ConfigurationOverlay> = if json_overlay.is_null() {
        None
    } else {
        match parse_configuration_overlay(CStr::from_ptr(json_overlay).to_bytes()) {
            Ok(o) => Some(o),
            Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
        }
    };

    let internal = StableConfigInternal::borrow_public_ptr(config);
    let result = match &internal.inner {
        Ok(stable_config) => stable_config.build_snippet(overlay),
        Err(error) => Err(error.clone()),
    };
    return SnippetInternal::new_from_stable_config(result).into_public_ptr();
}

/// Free a [`StableConfig`] created by [`stable_config_read`]. Existing snippets
/// remain valid.
///
/// # Safety
///
/// `config` must be a pointer returned by [`stable_config_read`] and not
/// already freed. NULL is tolerated and does nothing.
#[no_mangle]
pub unsafe extern "C" fn stable_config_cleanup(config: *mut StableConfig) {
    if config.is_null() {
        return;
    }
    drop(StableConfigInternal::from_public_ptr(config));
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
        assert_eq!(s.error_code, 10);
        let msg = unsafe { CStr::from_ptr(s.error_message) }
            .to_string_lossy()
            .into_owned();
        assert!(
            msg.contains("no stable-config entries found"),
            "unexpected message: {msg}"
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn null_language() {
        let snippet = unsafe { snippet_create_from_stable_config(null(), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 7);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn empty_language() {
        let snippet = unsafe { snippet_create_from_stable_config(cstr_ptr(b"\0"), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 10);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn invalid_utf8_language() {
        let snippet =
            unsafe { snippet_create_from_stable_config(cstr_ptr(&[0xFFu8, 0u8]), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 8);
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
                return CStr::from_ptr(s.error_message)
                    .to_string_lossy()
                    .into_owned();
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
    fn handle_read_on_a_host_without_stable_config_is_not_an_error() {
        let config = unsafe { stable_config_read(cstr_ptr(b"browser-sdk\0"), false) };
        assert_ne!(config, null_mut());
        let c = unsafe { StableConfigInternal::borrow_public_ptr(config) };
        assert_eq!(c.error_code, 0, "absent files must not be a read error");
        assert!(c.is_empty);
        unsafe { stable_config_cleanup(config) };
    }

    #[test]
    fn handle_null_language() {
        let config = unsafe { stable_config_read(null(), false) };
        assert_ne!(config, null_mut());
        let c = unsafe { StableConfigInternal::borrow_public_ptr(config) };
        assert_eq!(c.error_code, 7);
        unsafe { stable_config_cleanup(config) };
    }

    #[test]
    fn failed_handle_preserves_error_when_building_a_snippet() {
        let config = unsafe { stable_config_read(null(), false) };
        let snippet = unsafe { stable_config_snippet_create(config, null()) };
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };

        assert_eq!(s.error_code, 7);
        assert_eq!(
            unsafe { CStr::from_ptr(s.error_message) }.to_str().unwrap(),
            "FFI error: language is NULL"
        );

        unsafe { snippet_cleanup(snippet) };
        unsafe { stable_config_cleanup(config) };
    }

    #[test]
    fn handle_builds_several_snippets_and_reports_resolved_values() {
        let config = unsafe { stable_config_read(cstr_ptr(b"browser-sdk\0"), false) };

        for (overlay, expected_app_id) in [
            (
                b"{\"rum\":{\"applicationId\":\"app-1\",\"clientToken\":\"tok-1\"}}\0".to_vec(),
                "app-1",
            ),
            (
                b"{\"rum\":{\"applicationId\":\"app-2\",\"clientToken\":\"tok-2\"}}\0".to_vec(),
                "app-2",
            ),
        ] {
            let snippet = unsafe { stable_config_snippet_create(config, cstr_ptr(&overlay)) };
            let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
            assert_eq!(s.error_code, 0);
            assert!(s.length > 0);
            assert_ne!(s.application_id, null());
            assert_eq!(
                unsafe { CStr::from_ptr(s.application_id) }
                    .to_str()
                    .unwrap(),
                expected_app_id
            );
            assert_eq!(
                s.rum_enabled,
                crate::snippet::RUM_ENABLED_UNSET,
                "nothing set DD_RUM_ENABLED"
            );
            unsafe { snippet_cleanup(snippet) };
        }

        unsafe { stable_config_cleanup(config) };
    }

    #[test]
    fn snippet_outlives_the_handle_it_was_built_from() {
        let config = unsafe { stable_config_read(cstr_ptr(b"browser-sdk\0"), false) };
        let snippet = unsafe {
            stable_config_snippet_create(
                config,
                cstr_ptr(b"{\"rum\":{\"applicationId\":\"app-1\",\"clientToken\":\"tok-1\"}}\0"),
            )
        };
        unsafe { stable_config_cleanup(config) };

        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 0);
        assert_eq!(
            unsafe { CStr::from_ptr(s.application_id) }
                .to_str()
                .unwrap(),
            "app-1"
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn handle_snippet_create_rejects_null_config() {
        let snippet = unsafe { stable_config_snippet_create(null(), null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 7);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn cleanup_tolerates_null() {
        unsafe { stable_config_cleanup(null_mut()) };
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
