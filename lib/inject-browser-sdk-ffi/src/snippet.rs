#![allow(clippy::missing_safety_doc)]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::{
    boxed::Box,
    ffi::CString,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    ffi::{c_char, CStr},
    ptr::null,
};

use inject_browser_sdk::{
    error::Error, generate_snippet, parse_configuration, StableConfigSnippet,
};

/// A Browser SDK snippet. Use [`snippet_create_from_json`] or
/// [`crate::snippet_create_from_stable_config`] to create a new one.
///
/// In practice, a Browser SDK snippet looks like a HTML &lt;script&gt; tag containing what is
/// needed to load and initialize the SDK. See [public documentation][1].
///
/// # Memory ownership
///
/// `Snippet` instances are owned by the library and should be freed by [`snippet_cleanup`].
///
/// # Error handling
///
/// Snippet creation can fail for the following reasons:
///
/// | Reason                         | `error_code` | `error_message` |
/// |--------------------------------|--------------|-----------------|
/// | JSON parse failure             | `1`          | A message from [`serde_json`][2] with the error position. |
/// | Unsupported major version      | `2`          | `"Validation error: The major version '{v}' is not supported. ..."` |
/// | Unsupported site               | `3`          | `"Validation error: The site '{s}' is not a valid Datadog site. ..."` |
/// | Rate out of range              | `4`          | `"Validation error: The provided {key} is invalid. ..."` |
/// | Missing required field         | `5`          | `"Validation error: Mandatory field '{key}' is empty."` |
/// | Stable config error            | `6`          | `"Stable config error: ..."` |
/// | NULL pointer passed to FFI     | `7`          | `"FFI error: {field} is NULL"` |
/// | Non-UTF-8 input passed to FFI  | `8`          | `"FFI error: {field} is not valid UTF-8"` |
/// | Unknown JSON field             | `9`          | `"Validation error: Unknown field '{path}'."` |
/// | No stable config to read from  | `10`         | `"Stable config error: no stable-config entries found; ..."` |
///
///
/// [1]: https://docs.datadoghq.com/real_user_monitoring/browser/#cdn-async
/// [2]: https://docs.rs/serde_json
#[repr(C)]
pub struct Snippet {
    /// The error code. `0` when the snippet was created successfully.
    pub error_code: u8,
    /// The error message. `NULL` when the snippet was created successfully.
    pub error_message: *const c_char,
    /// The snippet bytes length.
    pub length: u32,
    /// Resolved `DD_RUM_ENABLED`, using the `RUM_ENABLED_*` constants below.
    /// Plain JSON snippets always report [`RUM_ENABLED_UNSET`]: the JSON shape
    /// accepted by [`snippet_create_from_json`] has no `enabled` key.
    pub rum_enabled: u8,
    /// Resolved `applicationId`, or `NULL` when unset.
    pub application_id: *const c_char,
    /// Resolved `remoteConfigurationId`, or `NULL` when unset.
    pub remote_configuration_id: *const c_char,
}

/// `DD_RUM_ENABLED` was not configured; the host decides.
pub const RUM_ENABLED_UNSET: u8 = 0;
/// `DD_RUM_ENABLED` resolved to true.
pub const RUM_ENABLED_TRUE: u8 = 1;
/// `DD_RUM_ENABLED` resolved to false.
pub const RUM_ENABLED_FALSE: u8 = 2;

/// The [`Snippet`] structure exposes only public fields, but we want to store internal data that
/// is not FFI-safe in the snippet instance. This is why this `SnippetInternal` struct need to
/// follow the same layout as the public `Snippet` struct.
#[repr(C)]
pub(crate) struct SnippetInternal {
    pub error_code: u8,
    pub error_message: *const c_char,
    pub length: u32,
    pub rum_enabled: u8,
    pub application_id: *const c_char,
    pub remote_configuration_id: *const c_char,
    // The following fields are kept internal, so they don't need to be FFI-safe and they can be
    // changed at any time without breaking change.
    pub inner: Result<Vec<u8>, CString>,
    pub owned_ids: [Option<CString>; 2],
}

impl SnippetInternal {
    pub(crate) fn new(result: Result<Vec<u8>, Error>) -> Self {
        match result {
            Ok(content) => {
                return Self {
                    error_message: null(),
                    error_code: 0,
                    length: content.len() as u32,
                    rum_enabled: RUM_ENABLED_UNSET,
                    application_id: null(),
                    remote_configuration_id: null(),
                    inner: Ok(content),
                    owned_ids: [None, None],
                }
            }
            Err(error) => {
                // Error messages can embed user-controlled strings (e.g. raw bytes
                // from libdd-library-config). Scrub NUL bytes so CString::new
                // cannot fail; fall back to a static message if it still does,
                // because panicking here brings down the host web server.
                let error_string = error.to_string().replace('\0', "\\0");
                let error_message = CString::new(error_string).unwrap_or_else(|_| {
                    return CString::new(
                        "internal error: error message contained unrepresentable bytes",
                    )
                    .expect("static message has no NUL bytes");
                });
                return Self {
                    error_message: error_message.as_ptr(),
                    error_code: error.code(),
                    length: 0,
                    rum_enabled: RUM_ENABLED_UNSET,
                    application_id: null(),
                    remote_configuration_id: null(),
                    inner: Err(error_message),
                    owned_ids: [None, None],
                };
            }
        }
    }

    pub(crate) fn new_from_stable_config(result: Result<StableConfigSnippet, Error>) -> Self {
        let snippet = match result {
            Ok(snippet) => snippet,
            Err(error) => return Self::new(Err(error)),
        };

        // Values are user-controlled, so a NUL byte would make CString::new
        // fail; treat that as "not set" rather than failing the whole snippet.
        let to_cstring =
            |value: Option<String>| return value.and_then(|v| return CString::new(v).ok());
        let owned_ids = [
            to_cstring(snippet.application_id),
            to_cstring(snippet.remote_configuration_id),
        ];

        let mut this = Self::new(Ok(snippet.bytes));
        this.rum_enabled = match snippet.enabled {
            None => RUM_ENABLED_UNSET,
            Some(true) => RUM_ENABLED_TRUE,
            Some(false) => RUM_ENABLED_FALSE,
        };
        this.application_id = owned_ids[0].as_ref().map_or(null(), |s| return s.as_ptr());
        this.remote_configuration_id = owned_ids[1].as_ref().map_or(null(), |s| return s.as_ptr());
        this.owned_ids = owned_ids;
        return this;
    }

    pub(crate) fn into_public_ptr(self) -> *mut Snippet {
        return Box::into_raw(Box::new(self)) as *mut Snippet;
    }

    unsafe fn from_public_ptr(snippet: *const Snippet) -> Box<Self> {
        return Box::from_raw(snippet as *mut SnippetInternal);
    }

    pub unsafe fn borrow_public_ptr(snippet: *const Snippet) -> &'static Self {
        return &*(snippet as *mut SnippetInternal);
    }
}

/// Create a snippet from a C string representing a JSON-encoded object.
///
/// # JSON format
///
/// The JSON object must be an object with the following properties:
///
/// * `majorVersion`: the major version of the Browser SDK to use. Currently, versions `5` and `6` are supported.
///
/// * `rum`: an object containing the RUM Browser SDK configuration. See [public documentation][1].
///   The following parameters are the most common ones:
///
/// | Parameter                 | Type      | Default value |
/// |---------------------------|-----------|---------------|
/// | `clientToken`             | `string`  | **Required**  |
/// | `applicationId`           | `string`  | **Required**  |
/// | `site`                    | `string`  | `datadoghq.com` |
/// | `service`                 | `string`  |               |
/// | `env`                     | `string`  |               |
/// | `version`                 | `string`  |               |
/// | `sessionSampleRate`       | `number`  | `100`         |
/// | `sessionReplaySampleRate` | `number`  | `0`           |
/// | `defaultPrivacyLevel`     | `string`  | `"mask"`      |
/// | `trackUserInteractions`   | `boolean` | `false`       |
/// | `trackResources`          | `boolean` | `false`       |
/// | `trackLongTasks`          | `boolean` | `false`       |
///
/// Note: not all parameters supported by the Browser SDK can be serialized as JSON (ex:
/// `beforeSend` is expected to be a function). Those parameters are not supported yet by this
/// library.
///
/// # Error handling
///
/// This function always returns a valid `Snippet` pointer. When an error occurs during the snippet
/// formatting, the `Snippet` instance contains an error code and an error message. See [`Snippet`]
/// documentation for details.
///
/// # Memory ownership
///
/// The `json` pointer is owned by the caller and should be freed by the caller. No reference to
/// the provided memory is kept by the library.
///
/// The `Snippet` instance returned by this function is owned by the library and should be freed
/// by using [`snippet_cleanup`] once done using it, even if an error occurred during its creation.
///
/// # Example
///
/// ```c
/// Snippet* snippet = snippet_create_from_json(
///    "{\"majorVersion\":5,\"rum\":{\"clientToken\":\"foo\",\"applicationId\":\"bar\"}}");
///
/// if (snippet->error_code != 0) {
///    printf("Error: %s\n", snippet->error_message);
///    snippet_cleanup(snippet);
///    return;
/// }
///
/// // ... use the snippet
///
/// snippet_cleanup(snippet);
/// ```
///
/// [1]: https://docs.datadoghq.com/real_user_monitoring/browser/#initialization-parameters
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_json(json: *const c_char) -> *mut Snippet {
    if json.is_null() {
        return SnippetInternal::new(Err(Error::FfiNullPointer(String::from("json"))))
            .into_public_ptr();
    }
    let json_bytes = CStr::from_ptr(json).to_bytes();

    // Report the resolved ids the same way the stable-config path does, so
    // hosts can derive telemetry tags from the snippet regardless of which
    // constructor produced it. `enabled` has no place in this JSON shape, so
    // `rum_enabled` stays RUM_ENABLED_UNSET.
    let result = parse_configuration(json_bytes).and_then(|configuration| {
        return Ok(StableConfigSnippet {
            bytes: generate_snippet(&configuration)?,
            enabled: None,
            application_id: configuration.rum.application_id.clone(),
            remote_configuration_id: configuration.rum.remote_configuration_id.clone(),
        });
    });

    return SnippetInternal::new_from_stable_config(result).into_public_ptr();
}

/// Free the [`Snippet`] associated memory.
///
/// # Memory safety
///
/// Do not use the `snippet` instance after calling this function. Passing `NULL` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn snippet_cleanup(snippet: *mut Snippet) {
    if !snippet.is_null() {
        _ = SnippetInternal::from_public_ptr(snippet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr::null;
    use pretty_assertions::{assert_eq, assert_ne};
    use std::{borrow::ToOwned, ptr::null_mut, string::String};

    #[test]
    fn snippet_creation() {
        let snippet =
            unsafe { snippet_create_from_json(cstr_ptr(b"{\"majorVersion\":5,\"rum\":{}}\0")) };
        assert_ne!(snippet, null_mut());
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn json_snippet_reports_resolved_ids() {
        // The IIS module derives its telemetry tags from these fields, so both
        // snippet constructors have to populate them.
        let snippet = unsafe {
            snippet_create_from_json(
                cstr_ptr(b"{\"majorVersion\":5,\"rum\":{\"applicationId\":\"app-json\",\"clientToken\":\"tok-json\",\"remoteConfigurationId\":\"rc-json\"}}\0"),
            )
        };
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 0);
        assert_eq!(
            unsafe { CStr::from_ptr(s.application_id) }
                .to_str()
                .unwrap(),
            "app-json"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(s.remote_configuration_id) }
                .to_str()
                .unwrap(),
            "rc-json"
        );
        assert_eq!(
            s.rum_enabled, RUM_ENABLED_UNSET,
            "the JSON shape has no enabled key"
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn json_snippet_without_ids_reports_null() {
        let snippet = unsafe {
            snippet_create_from_json(
                cstr_ptr(b"{\"majorVersion\":5,\"rum\":{\"applicationId\":\"app\",\"clientToken\":\"tok\"}}\0"),
            )
        };
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 0);
        assert_eq!(s.remote_configuration_id, null());
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn snippet_creation_errors() {
        // Invalid UTF-8
        assert_eq!(
            get_error(unsafe { snippet_create_from_json(cstr_ptr(&[0xAA, 0])) }),
            (
                1,
                "JSON error: expected value at line 1 column 1".to_owned()
            )
        );

        // Invalid JSON
        assert_eq!(
            get_error(unsafe { snippet_create_from_json(cstr_ptr(b"{\0")) }),
            (
                1,
                "JSON error: EOF while parsing an object at line 1 column 1".to_owned()
            )
        );

        // Invalid major version
        assert_eq!(
            unsafe {
                get_error(snippet_create_from_json(cstr_ptr(
                    b"{\"majorVersion\":1,\"rum\":{\"applicationId\":\"foo\", \"clientToken\":\"bar\", \"site\":\"datadoghq.com\"}}\0",
                )))
            },
            (2, "Validation error: The major version '1' is not supported. Supported RUM SDK versions: [5, 6]".to_owned())
        );

        // Missing "rum".
        assert_eq!(
            get_error(unsafe { snippet_create_from_json(cstr_ptr(b"{\"majorVersion\":5}\0")) }),
            (
                1,
                "JSON error: missing field `rum` at line 1 column 18".to_owned()
            )
        );
    }

    use crate::test_utils::cstr_ptr;

    fn get_error(snippet: *const Snippet) -> (u8, String) {
        let snippet = unsafe { SnippetInternal::from_public_ptr(snippet) };
        return (
            snippet.error_code,
            String::from_utf8(snippet.inner.unwrap_err().as_bytes().to_vec()).unwrap(),
        );
    }

    #[test]
    fn snippet_create_from_json_null() {
        let snippet = unsafe { snippet_create_from_json(null()) };
        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        assert_eq!(s.error_code, 7);
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn error_message_nul_bytes_are_scrubbed() {
        use inject_browser_sdk::error::Error;
        // User-controlled bytes inside an error message must not cause
        // CString::new to fail. Regression guard for the NUL-scrub path in
        // SnippetInternal::new — panicking here takes down the host server.
        let s = SnippetInternal::new(Err(Error::StableConfig(String::from("before\0after"))));
        assert_eq!(s.error_code, 6);
        let msg_bytes = unsafe { CStr::from_ptr(s.error_message).to_bytes() };
        let msg = core::str::from_utf8(msg_bytes).unwrap();
        assert!(!msg.contains('\0'), "NUL must be scrubbed before CString");
        assert!(msg.contains("before\\0after"), "got: {msg}");
    }

    #[test]
    fn disabled_stable_config_without_bytes_surfaces_switch() {
        let snippet = SnippetInternal::new_from_stable_config(Ok(StableConfigSnippet {
            bytes: Vec::new(),
            enabled: Some(false),
            application_id: None,
            remote_configuration_id: None,
        }));

        assert_eq!(snippet.error_code, 0);
        assert_eq!(snippet.length, 0);
        assert_eq!(snippet.rum_enabled, RUM_ENABLED_FALSE);
    }

    #[test]
    fn snippet_cleanup_null_is_noop() {
        unsafe { snippet_cleanup(null_mut()) };
    }

    #[test]
    fn snippet_layout() {
        use core::mem;
        assert_eq!(
            mem::offset_of!(Snippet, error_code),
            mem::offset_of!(SnippetInternal, error_code),
        );
        assert_eq!(
            mem::offset_of!(Snippet, error_message),
            mem::offset_of!(SnippetInternal, error_message),
        );
        assert_eq!(
            mem::offset_of!(Snippet, length),
            mem::offset_of!(SnippetInternal, length),
        );
        assert_eq!(
            mem::offset_of!(Snippet, rum_enabled),
            mem::offset_of!(SnippetInternal, rum_enabled),
        );
        assert_eq!(
            mem::offset_of!(Snippet, application_id),
            mem::offset_of!(SnippetInternal, application_id),
        );
        assert_eq!(
            mem::offset_of!(Snippet, remote_configuration_id),
            mem::offset_of!(SnippetInternal, remote_configuration_id),
        );
    }
}
