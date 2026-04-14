// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::{format, string::String, vec, vec::Vec};
use core::ffi::{c_char, CStr};

use inject_browser_sdk::{config_option::ConfigOption, error::Error, snippet_from_lookup};
use libdd_library_config::{
    ConfigRead, ConfigReadError, Configurator, LibraryConfig, ProcessInfo, MAX_CONFIG_FILE_SIZE,
};

use crate::snippet::{Snippet, SnippetInternal};

// --- Platform-specific ConfigRead and ProcessInfo ---

#[cfg(not(windows))]
mod platform {
    use super::*;

    /// [`ConfigRead`] implementation using `rustix` for no_std file I/O.
    pub struct PlatformConfigRead;

    impl ConfigRead for PlatformConfigRead {
        type IoError = rustix::io::Errno;

        fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<rustix::io::Errno>> {
            use rustix::fs::{fstat, open, Mode, OFlags};
            use rustix::io::read;

            let fd = match open(path, OFlags::RDONLY, Mode::empty()) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Err(ConfigReadError::NotFound),
                Err(e) => return Err(ConfigReadError::Io(e)),
            };

            let file_size = fstat(&fd)
                .map(|s| {
                    if s.st_size < 0 {
                        0usize
                    } else {
                        s.st_size as usize
                    }
                })
                .map_err(ConfigReadError::Io)?;

            if file_size > MAX_CONFIG_FILE_SIZE {
                return Err(ConfigReadError::TooLarge);
            }

            let mut buf = vec![0u8; file_size];
            let mut total = 0;
            while total < file_size {
                match read(&fd, &mut buf[total..]) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(e) => return Err(ConfigReadError::Io(e)),
                }
            }
            buf.truncate(total);
            Ok(buf)
        }
    }

    /// Build a [`ProcessInfo`] by reading `/proc/self/environ` and `/proc/self/cmdline`.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        let envp = read_nul_separated("/proc/self/environ").unwrap_or_default();
        let args = read_nul_separated("/proc/self/cmdline").unwrap_or_default();
        ProcessInfo {
            args,
            envp,
            language: language.as_bytes().to_vec(),
        }
    }

    fn read_nul_separated(path: &str) -> Option<Vec<Vec<u8>>> {
        let reader = PlatformConfigRead;
        let bytes = reader.read(path).ok()?;
        Some(
            bytes
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_vec())
                .collect(),
        )
    }

    pub fn write_stderr(msg: &str) {
        let _ = rustix::io::write(unsafe { rustix::stdio::stderr() }, msg.as_bytes());
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    /// On Windows, use the std-based [`StdConfigRead`] (see issue #73 for no_std windows-sys reader).
    pub use libdd_library_config::StdConfigRead as PlatformConfigRead;

    /// Build a [`ProcessInfo`] using std on Windows.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        ProcessInfo::detect_global(String::from(language))
    }

    pub fn write_stderr(_msg: &str) {
        // On Windows with std, eprintln would work but we're in no_std context for the crate.
        // Debug logs are best-effort; skip on Windows for now.
    }
}

use platform::{detect_process_info, write_stderr, PlatformConfigRead};

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

/// Reads configuration entries from both local and fleet-managed `application_monitoring.yaml`
/// files via `get_config_from_reader`, preserving the full [`LibraryConfig`] metadata.
///
/// When `debug_logs` is `true`, diagnostic messages from the configurator are written to stderr.
///
/// # Safety
/// `language` must be a valid, non-null, NUL-terminated C string.
unsafe fn read_stable_configs(
    language: *const c_char,
    debug_logs: bool,
) -> Result<Vec<LibraryConfig>, String> {
    let language_str = CStr::from_ptr(language)
        .to_str()
        .map_err(|_| String::from("language parameter is not valid UTF-8"))?;

    let process_info = detect_process_info(language_str);
    let configurator = Configurator::new(debug_logs);

    let local_path = Configurator::LOCAL_STABLE_CONFIGURATION_PATH;
    let fleet_path = Configurator::FLEET_STABLE_CONFIGURATION_PATH;

    let logged_result = configurator.get_config_from_reader(
        &PlatformConfigRead,
        local_path,
        fleet_path,
        &process_info,
    );

    if debug_logs {
        for msg in logged_result.logs() {
            write_stderr(&format!("[inject-browser-sdk] {}\n", msg));
        }
    }

    logged_result.data().map_err(|err| format!("{}", err))
}

/// A typed configuration override: a [`ConfigOption`] variant paired with a C string value.
///
/// Used with [`snippet_create_from_stable_config`] to supply or override values
/// on top of what the Agent's stable configuration files provide.
#[repr(C)]
pub struct ConfigOverride {
    pub option: ConfigOption,
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
///     { ConfigOption_Service, "my-custom-service" },
/// };
/// Snippet* s = snippet_create_from_stable_config(
///     "browser-sdk", false, overrides, 1);
///
/// snippet_cleanup(s);
/// ```
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

    // Parse overrides into a map keyed by DD_* key name.
    let override_map = match parse_overrides(overrides, override_count) {
        Ok(map) => map,
        Err(e) => return SnippetInternal::new(Err(e)).into_public_ptr(),
    };

    let configs = match read_stable_configs(language, debug_logs) {
        Ok(configs) => configs,
        Err(err) => {
            return SnippetInternal::new(Err(Error::StableConfig(err))).into_public_ptr();
        }
    };

    let result = snippet_from_lookup(|key| {
        if let Some(v) = override_map.get(key) {
            return Some(v.as_str());
        }
        configs
            .iter()
            .find(|c| c.name == key)
            .map(|c| c.value.as_str())
    });

    SnippetInternal::new(result).into_public_ptr()
}

/// Parse a C array of [`ConfigOverride`] into a `BTreeMap` keyed by DD_* key name.
unsafe fn parse_overrides(
    entries: *const ConfigOverride,
    count: u32,
) -> Result<alloc::collections::BTreeMap<&'static str, String>, Error> {
    use alloc::collections::BTreeMap;

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
    let mut map = BTreeMap::new();
    for entry in slice {
        let key = entry.option.key();
        let value = cstr_to_str(entry.value, &format!("Value for override '{key}'"))?;
        map.insert(key, String::from(value));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::ffi::CString;
    use core::ptr::null;
    use pretty_assertions::{assert_eq, assert_ne};
    use std::ptr::null_mut;

    use crate::snippet::{snippet_cleanup, SnippetInternal};

    fn cstr_ptr(bytes: &[u8]) -> *const i8 {
        bytes.as_ptr() as *const i8
    }

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
                option: ConfigOption::ApplicationId,
                value: app_val.as_ptr(),
            },
            ConfigOverride {
                option: ConfigOption::ClientToken,
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
            option: ConfigOption::Service,
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
