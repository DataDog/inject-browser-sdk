#![allow(clippy::missing_safety_doc)]
// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::{
    boxed::Box,
    ffi::CString,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    ffi::{c_char, CStr},
    ptr::null,
};

use inject_browser_sdk::{
    configuration_from_getter, error::Error, generate_snippet, Configuration,
};

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

/// A Browser SDK snippet. Use [`snippet_create_from_json`],
/// [`snippet_create_from_stable_config`], or [`snippet_create_from_entries`] to create a new one.
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
/// | Invalid field value            | `6`          | `"Validation error: Invalid value '{v}' for key '{key}'."` |
/// | Stable config read failure     | `7`          | `"Stable config error: ..."` |
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
}

/// The [`Snippet`] structure exposes only public fields, but we want to store internal data that
/// is not FFI-safe in the snippet instance. This is why this `SnippetInternal` struct need to
/// follow the same layout as the public `Snippet` struct.
#[repr(C)]
pub(crate) struct SnippetInternal {
    pub error_code: u8,
    pub error_message: *const c_char,
    pub length: u32,
    // The following fields are kept internal, so they don't need to be FFI-safe and they can be
    // changed at any time without breaking change.
    pub inner: Result<Vec<u8>, CString>,
}

impl SnippetInternal {
    fn new(result: Result<Vec<u8>, Error>) -> Self {
        match result {
            Ok(content) => Self {
                error_message: null(),
                error_code: 0,
                length: content.len() as u32,
                inner: Ok(content),
            },
            Err(error) => {
                let raw = error.to_string().replace('\0', "\\0");
                let error_message = CString::new(raw)
                    .unwrap_or_else(|_| CString::new("Error (message unavailable)").unwrap());
                Self {
                    error_message: error_message.as_ptr(),
                    error_code: error.code(),
                    length: 0,
                    inner: Err(error_message),
                }
            }
        }
    }

    fn stable_config_err(msg: &str) -> *mut Snippet {
        Self::new(Err(Error::StableConfig(String::from(msg)))).into_public_ptr()
    }

    fn into_public_ptr(self) -> *mut Snippet {
        Box::into_raw(Box::new(self)) as *mut Snippet
    }

    unsafe fn from_public_ptr(snippet: *const Snippet) -> Box<Self> {
        Box::from_raw(snippet as *mut SnippetInternal)
    }

    pub unsafe fn borrow_public_ptr(snippet: *const Snippet) -> &'static Self {
        &*(snippet as *mut SnippetInternal)
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
    let json_bytes = CStr::from_ptr(json).to_bytes();

    let result = serde_json::from_slice::<Configuration>(json_bytes)
        .map_err(|err| Error::Json(format!("{}", err)))
        .and_then(|configuration| generate_snippet(&configuration));

    SnippetInternal::new(result).into_public_ptr()
}

/// Reads configuration entries from both local and fleet-managed `application_monitoring.yaml`
/// files and returns them as a `Vec<(name, value)>`. Handles UTF-8 validation of `language` and
/// formats file-read errors.
///
/// When `debug_logs` is `true`, diagnostic messages from the configurator are written to stderr.
///
/// # Safety
/// `language` must be a valid, non-null, NUL-terminated C string.
#[cfg(feature = "std")]
unsafe fn read_stable_configs(
    language: *const c_char,
    debug_logs: bool,
) -> Result<Vec<(String, String)>, String> {
    let language_str = CStr::from_ptr(language)
        .to_str()
        .map_err(|_| String::from("language parameter is not valid UTF-8"))?
        .to_owned();

    let process_info = libdd_library_config::ProcessInfo::detect_global(language_str);
    let configurator = libdd_library_config::Configurator::new(debug_logs);

    let local_path =
        std::path::Path::new(libdd_library_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH);
    let fleet_path =
        std::path::Path::new(libdd_library_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH);

    let logged_result = configurator.get_config_from_file(local_path, fleet_path, &process_info);

    if debug_logs {
        for msg in logged_result.logs() {
            eprintln!("[inject-browser-sdk] {}", msg);
        }
    }

    logged_result
        .data()
        .map(|configs| configs.into_iter().map(|c| (c.name, c.value)).collect())
        .map_err(|err| format!("{}", err))
}

/// Create a snippet from Datadog stable configuration files.
///
/// This function provides an alternative to [`snippet_create_from_json`] that reads configuration
/// from the Datadog Agent's `application_monitoring.yaml` files (local and fleet-managed).
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`) and
/// is used for rule-based process matching in the configuration files.
///
/// When `debug_logs` is `true`, the configurator will produce detailed log messages about the
/// configuration resolution process.
///
/// # Error handling
///
/// This function always returns a valid `Snippet` pointer. When an error occurs during the snippet
/// creation, the `Snippet` instance contains an error code and an error message. See [`Snippet`]
/// documentation for details.
///
/// # Memory ownership
///
/// The `language` pointer is owned by the caller and should be freed by the caller. No reference
/// to the provided memory is kept by the library.
///
/// The `Snippet` instance returned by this function is owned by the library and should be freed
/// by using [`snippet_cleanup`] once done using it, even if an error occurred during its creation.
///
/// # Example
///
/// ```c
/// Snippet* snippet = snippet_create_from_stable_config("browser-sdk", false);
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
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_stable_config(
    language: *const c_char,
    debug_logs: bool,
) -> *mut Snippet {
    use std::collections::HashMap;

    if language.is_null() {
        return SnippetInternal::stable_config_err("language parameter is NULL");
    }

    let configs = match read_stable_configs(language, debug_logs) {
        Ok(configs) => configs,
        Err(err) => {
            return SnippetInternal::new(Err(Error::StableConfig(err))).into_public_ptr();
        }
    };

    let config_map: HashMap<String, String> = configs.into_iter().collect();
    let result = configuration_from_getter(|key| config_map.get(key).cloned())
        .and_then(|config| generate_snippet(&config));

    SnippetInternal::new(result).into_public_ptr()
}

/// Create a snippet from an array of stable configuration entries.
///
/// This is a companion to [`stable_config_get_entries`]: call that function to retrieve
/// the raw `DD_*` key-value pairs from the Datadog Agent's configuration files, inspect or
/// filter them as needed, then pass them to this function to produce the snippet.
///
/// # Error handling
///
/// This function always returns a valid `Snippet` pointer. When an error occurs, the `Snippet`
/// contains an error code and message. See [`Snippet`] documentation for details.
///
/// # Memory ownership
///
/// The `entries` pointer and all strings within it are owned by the caller. This function copies
/// any data it needs and does not retain references to the provided memory.
///
/// The returned `Snippet` is owned by the library and must be freed with [`snippet_cleanup`].
///
/// # Example
///
/// ```c
/// StableConfigEntries* entries = stable_config_get_entries("browser-sdk", false);
/// if (entries->error_code != 0) { ... }
///
/// Snippet* snippet = snippet_create_from_entries(entries->entries, entries->count);
/// stable_config_entries_cleanup(entries);
///
/// if (snippet->error_code != 0) { ... }
/// // ... use the snippet
/// snippet_cleanup(snippet);
/// ```
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn snippet_create_from_entries(
    entries: *const StableConfigEntry,
    count: u32,
) -> *mut Snippet {
    use std::collections::HashMap;

    let entries_slice = if count == 0 || entries.is_null() {
        &[]
    } else {
        core::slice::from_raw_parts(entries, count as usize)
    };
    let mut config_map = HashMap::new();

    for entry in entries_slice {
        let name = match cstr_to_str(entry.name, "Entry name") {
            Ok(s) => s,
            Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
        };
        let value = match cstr_to_str(entry.value, &format!("Value for entry '{name}'")) {
            Ok(s) => s,
            Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
        };
        config_map.insert(name.to_owned(), value.to_owned());
    }

    let result = configuration_from_getter(|key| config_map.get(key).cloned())
        .and_then(|config| generate_snippet(&config));

    SnippetInternal::new(result).into_public_ptr()
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

/// A single configuration entry from stable config (name/value pair as C strings).
#[repr(C)]
pub struct StableConfigEntry {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// A list of stable configuration entries with error handling.
///
/// When `error_code` is 0, `entries` points to `count` valid entries.
/// When `error_code` is non-zero, `error_message` contains a description.
///
/// Free with [`stable_config_entries_cleanup`].
#[repr(C)]
pub struct StableConfigEntries {
    pub entries: *const StableConfigEntry,
    pub count: u32,
    pub error_code: u8,
    pub error_message: *const c_char,
}

/// Internal struct that mirrors the public fields and owns the backing memory.
/// Must have the same leading layout as [`StableConfigEntries`].
#[repr(C)]
struct StableConfigEntriesInternal {
    // --- public fields (same layout as StableConfigEntries) ---
    entries_ptr: *const StableConfigEntry,
    count: u32,
    error_code: u8,
    error_message: *const c_char,
    // --- private backing data ---
    _entry_vec: Vec<StableConfigEntry>,
    _strings: Vec<CString>,
    _error_cstring: Option<CString>,
}

impl StableConfigEntriesInternal {
    fn new_ok(
        entry_vec: Vec<StableConfigEntry>,
        strings: Vec<CString>,
    ) -> *mut StableConfigEntries {
        let mut internal = Box::new(Self {
            entries_ptr: null(),
            count: entry_vec.len() as u32,
            error_code: 0,
            error_message: null(),
            _entry_vec: entry_vec,
            _strings: strings,
            _error_cstring: None,
        });
        internal.entries_ptr = internal._entry_vec.as_ptr();
        Box::into_raw(internal) as *mut StableConfigEntries
    }

    fn new_err(msg: &str) -> *mut StableConfigEntries {
        let sanitized = msg.replace('\0', "\\0");
        let cstring = CString::new(sanitized)
            .unwrap_or_else(|_| CString::new("Stable config error").unwrap());
        let mut internal = Box::new(Self {
            entries_ptr: null(),
            count: 0,
            error_code: 7,
            error_message: null(),
            _entry_vec: Vec::new(),
            _strings: Vec::new(),
            _error_cstring: Some(cstring),
        });
        internal.error_message = internal._error_cstring.as_ref().unwrap().as_ptr();
        Box::into_raw(internal) as *mut StableConfigEntries
    }
}

/// Read configuration entries from Datadog stable configuration files.
///
/// Returns key-value pairs (e.g. `DD_RUM_APPLICATION_ID` = `"app-123"`) from
/// the Datadog Agent's `application_monitoring.yaml` files.
///
/// The `language` parameter identifies the current process's language (e.g. `"browser-sdk"`).
///
/// # Memory ownership
///
/// The returned pointer must be freed with [`stable_config_entries_cleanup`].
///
/// # Example
///
/// ```c
/// StableConfigEntries* entries = stable_config_get_entries("browser-sdk", false);
/// if (entries->error_code == 0) {
///     for (uint32_t i = 0; i < entries->count; i++) {
///         printf("%s = %s\n", entries->entries[i].name, entries->entries[i].value);
///     }
/// }
/// stable_config_entries_cleanup(entries);
/// ```
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn stable_config_get_entries(
    language: *const c_char,
    debug_logs: bool,
) -> *mut StableConfigEntries {
    if language.is_null() {
        return StableConfigEntriesInternal::new_err("language parameter is NULL");
    }

    let configs = match read_stable_configs(language, debug_logs) {
        Ok(configs) => configs,
        Err(err) => return StableConfigEntriesInternal::new_err(&err),
    };

    let mut strings = Vec::with_capacity(configs.len() * 2);
    let mut entries = Vec::with_capacity(configs.len());

    for (name_str, value_str) in configs {
        let name = match CString::new(name_str.clone()) {
            Ok(s) => s,
            Err(_) => {
                return StableConfigEntriesInternal::new_err(&format!(
                    "Config entry name contains a NUL byte: {:?}",
                    name_str
                ));
            }
        };
        let value = match CString::new(value_str.clone()) {
            Ok(s) => s,
            Err(_) => {
                return StableConfigEntriesInternal::new_err(&format!(
                    "Config entry '{}' value contains a NUL byte",
                    name_str
                ));
            }
        };
        let entry = StableConfigEntry {
            name: name.as_ptr(),
            value: value.as_ptr(),
        };
        strings.push(name);
        strings.push(value);
        entries.push(entry);
    }

    StableConfigEntriesInternal::new_ok(entries, strings)
}

/// Free the [`StableConfigEntries`] associated memory.
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn stable_config_entries_cleanup(entries: *mut StableConfigEntries) {
    if !entries.is_null() {
        let _ = Box::from_raw(entries as *mut StableConfigEntriesInternal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // Missing "rum"
        assert_eq!(
            get_error(unsafe { snippet_create_from_json(cstr_ptr(b"{\"majorVersion\":5}\0")) }),
            (
                1,
                "JSON error: missing field `rum` at line 1 column 18".to_owned()
            )
        );
    }

    fn cstr_ptr(bytes: &[u8]) -> *const i8 {
        bytes.as_ptr() as *const i8
    }

    fn get_error(snippet: *const Snippet) -> (u8, String) {
        let snippet = unsafe { SnippetInternal::from_public_ptr(snippet) };
        (
            snippet.error_code,
            String::from_utf8(snippet.inner.unwrap_err().as_bytes().to_vec()).unwrap(),
        )
    }

    #[test]
    fn snippet_create_from_stable_config_no_files() {
        let snippet =
            unsafe { snippet_create_from_stable_config(cstr_ptr(b"browser-sdk\0"), false) };

        assert_ne!(snippet, null_mut());
        let s = unsafe { SnippetInternal::borrow_public_ptr(snippet) };
        // Without config files, stable config returns empty entries and the required
        // DD_RUM_APPLICATION_ID / DD_RUM_CLIENT_TOKEN are absent → error code 5
        // (EmptyMandatoryConf). If the underlying library errors on missing files instead,
        // code 7 (StableConfig) is also acceptable.
        assert!(
            matches!(s.error_code, 5 | 7),
            "expected error code 5 or 7, got {}",
            s.error_code
        );
        unsafe { snippet_cleanup(snippet) };
    }

    #[test]
    fn snippet_create_from_stable_config_with_temp_files() {
        use std::io::Write;

        let local_yaml = b"
apm_configuration_default:
  DD_RUM_APPLICATION_ID: test-app-id
  DD_RUM_CLIENT_TOKEN: test-token
";
        let mut local_file = tempfile::NamedTempFile::new().unwrap();
        local_file.write_all(local_yaml).unwrap();
        let local_path = local_file.path();

        // Test the full config-entries-to-snippet pipeline via snippet_create_from_entries.
        // (snippet_create_from_stable_config itself uses hardcoded agent paths and cannot be
        // given custom paths in tests; its file-reading is covered by snippet_create_from_stable_config_no_files.)
        let process_info =
            libdd_library_config::ProcessInfo::detect_global("browser-sdk".to_owned());
        let configurator = libdd_library_config::Configurator::new(false);
        let raw_configs = configurator
            .get_config_from_file(
                local_path,
                std::path::Path::new("/nonexistent"),
                &process_info,
            )
            .data()
            .unwrap();

        let cstrings: Vec<(CString, CString)> = raw_configs
            .into_iter()
            .map(|c| {
                (
                    CString::new(c.name).unwrap(),
                    CString::new(c.value).unwrap(),
                )
            })
            .collect();
        let ffi_entries: Vec<StableConfigEntry> = cstrings
            .iter()
            .map(|(n, v)| StableConfigEntry {
                name: n.as_ptr(),
                value: v.as_ptr(),
            })
            .collect();

        let snippet =
            unsafe { snippet_create_from_entries(ffi_entries.as_ptr(), ffi_entries.len() as u32) };

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
    fn stable_config_entries_layout() {
        use core::mem;
        assert_eq!(
            mem::offset_of!(StableConfigEntries, entries),
            mem::offset_of!(StableConfigEntriesInternal, entries_ptr),
        );
        assert_eq!(
            mem::offset_of!(StableConfigEntries, count),
            mem::offset_of!(StableConfigEntriesInternal, count),
        );
        assert_eq!(
            mem::offset_of!(StableConfigEntries, error_code),
            mem::offset_of!(StableConfigEntriesInternal, error_code),
        );
        assert_eq!(
            mem::offset_of!(StableConfigEntries, error_message),
            mem::offset_of!(StableConfigEntriesInternal, error_message),
        );
    }
}
