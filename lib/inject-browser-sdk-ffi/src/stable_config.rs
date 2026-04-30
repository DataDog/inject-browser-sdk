// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::string::{String, ToString};
use core::ffi::{c_char, CStr};

use inject_browser_sdk::{error::Error, parse_configuration_overlay, snippet_from_stable_config};

use crate::snippet::{Snippet, SnippetInternal};

/// UTF-8 validates a borrowed `CStr`. The returned `&str` is tied to the
/// `CStr`'s lifetime so callers cannot accidentally extend it.
fn cstr_to_str<'a>(cstr: &'a CStr, field: &str) -> Result<&'a str, Error> {
    cstr.to_str()
        .map_err(|_| Error::FfiInvalidUtf8(field.to_string()))
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
/// keys), `6` (stable-config I/O or YAML parse failures), `7` (NULL
/// `language`), and `8` (non-UTF-8 `language`). Overlay values that fail
/// validation also produce codes `2`–`5`.
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
        // With no config files and no overlay, the error is now the
        // descriptive "no stable-config entries found" (code 6) — not the
        // misleading "mandatory field applicationId empty" (code 5) we used
        // to surface.
        let snippet =
            unsafe { snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
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
        // Empty language passes NULL+UTF-8 guards but matches no rule
        // selectors. Must surface as the descriptive "no stable-config
        // entries found" (code 6), not as a lower-level failure.
        let snippet = unsafe { snippet_create_from_stable_config(cstr_ptr(b"\0"), false, null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 6);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn invalid_utf8_language() {
        // 0xFF is not valid UTF-8; must surface as FFI non-UTF-8 error (8),
        // distinct from NULL-pointer (7) and stable-config (6).
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
