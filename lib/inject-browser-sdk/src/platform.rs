// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Platform-specific helpers for reading stable configuration files.
//!
//! Gated behind the `stable-config` feature (enabled by default).

use alloc::{format, string::String, vec, vec::Vec};

use libdd_library_config::{
    ConfigRead, ConfigReadError, Configurator, LibraryConfig, ProcessInfo, MAX_CONFIG_FILE_SIZE,
};

// --- Unix platform ---

#[cfg(not(windows))]
mod inner {
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

    #[allow(unused_unsafe)] // stderr() is unsafe without std, safe with std
    pub fn write_stderr(msg: &str) {
        // SAFETY: stderr is always valid; we only write diagnostic messages.
        let _ = rustix::io::write(unsafe { rustix::stdio::stderr() }, msg.as_bytes());
    }
}

// --- Windows platform ---

#[cfg(windows)]
mod inner {
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

pub use inner::{detect_process_info, write_stderr, PlatformConfigRead};

/// Reads configuration entries from both local and fleet-managed `application_monitoring.yaml`
/// files, preserving the full [`LibraryConfig`] metadata.
///
/// When `debug_logs` is `true`, diagnostic messages from the configurator are written to stderr.
pub fn read_stable_configs(language: &str, debug_logs: bool) -> Result<Vec<LibraryConfig>, String> {
    let process_info = detect_process_info(language);
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
