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

#[cfg(not(windows))]
mod inner {
    use super::*;

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

            let file_size = fstat(&fd).map_err(ConfigReadError::Io).and_then(|s| {
                usize::try_from(s.st_size)
                    .map_err(|_| ConfigReadError::Io(rustix::io::Errno::INVAL))
            })?;

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

    /// On Linux, `/proc/self/cmdline` and `/proc/self/environ` can be mutated by
    /// the host process and are attacker-influenceable from an injected-library
    /// context. Leave `args`/`envp` empty so rule-based process matching falls
    /// back to `language`-only until an injection-safe channel is available.
    /// TODO(INPLAT-1110): supply args/envp via an injection-safe mechanism.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        ProcessInfo {
            args: Vec::new(),
            envp: Vec::new(),
            language: language.as_bytes().to_vec(),
        }
    }

    #[allow(unused_unsafe)]
    pub fn write_stderr(msg: &str) {
        // SAFETY: rustix::stdio::stderr() requires fd 2 has not been closed;
        // this crate never closes stderr.
        let stderr = unsafe { rustix::stdio::stderr() };
        let bytes = msg.as_bytes();
        let mut written = 0;
        while written < bytes.len() {
            match rustix::io::write(stderr, &bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(rustix::io::Errno::INTR) => continue,
                // EBADF/EPIPE and other errors: no recovery from inside an
                // injected library; stop writing rather than spinning.
                Err(_) => break,
            }
        }
    }
}

#[cfg(windows)]
mod inner {
    use super::*;

    // Tracked upstream: DataDog/libdatadog#73 (no_std windows-sys reader).
    pub use libdd_library_config::StdConfigRead as PlatformConfigRead;

    /// On Windows (IIS hosting), argv/envp aren't mutable by other entities in
    /// the host process model, so reading them via `std::env` is safe. This is
    /// intentionally different from the Unix branch, where /proc/self/{cmdline,
    /// environ} can't be trusted from an injected library.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        ProcessInfo::detect_global(String::from(language))
    }

    /// No-op on Windows — no stderr sink is linked in the no_std path.
    /// `debug_logs=true` produces no output until a no_std windows-sys sink is
    /// added (tracked upstream as DataDog/libdatadog#73).
    pub fn write_stderr(_msg: &str) {}
}

pub use inner::{detect_process_info, write_stderr, PlatformConfigRead};

/// Reads configuration entries from both local and fleet-managed `application_monitoring.yaml`
/// files, preserving the full [`LibraryConfig`] metadata.
///
/// When `debug_logs` is `true`, diagnostic messages from the configurator are written to stderr.
/// On Windows, `debug_logs` is a no-op (see [`write_stderr`]).
pub fn read_stable_configs(language: &str, debug_logs: bool) -> Result<Vec<LibraryConfig>, String> {
    read_stable_configs_with_reader(&PlatformConfigRead, language, debug_logs)
}

/// Variant of [`read_stable_configs`] that accepts a caller-supplied reader.
/// Intended for tests that need to exercise populated YAML input without
/// touching the real filesystem.
pub fn read_stable_configs_with_reader<R: ConfigRead>(
    reader: &R,
    language: &str,
    debug_logs: bool,
) -> Result<Vec<LibraryConfig>, String> {
    let process_info = detect_process_info(language);
    let configurator = Configurator::new(debug_logs);

    let local_path = Configurator::LOCAL_STABLE_CONFIGURATION_PATH;
    let fleet_path = Configurator::FLEET_STABLE_CONFIGURATION_PATH;

    let logged_result =
        configurator.get_config_from_reader(reader, local_path, fleet_path, &process_info);

    // Buffer logs before consuming `logged_result` — `data()` takes self.
    let logs: Vec<String> = logged_result.logs().iter().map(String::from).collect();
    let data = logged_result.data();
    // Always surface configurator diagnostics when the result is an error —
    // dropping them when debug_logs=false means the user only sees the final
    // terminal message with no indication of which file/rule failed.
    if debug_logs || data.is_err() {
        for msg in &logs {
            write_stderr(&format!("[inject-browser-sdk] {}\n", msg));
        }
    }

    data.map_err(|err| format!("{}", err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    struct NotFoundReader;

    impl ConfigRead for NotFoundReader {
        type IoError = core::convert::Infallible;

        fn read(&self, _path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
            Err(ConfigReadError::NotFound)
        }
    }

    struct MalformedReader;

    impl ConfigRead for MalformedReader {
        type IoError = core::convert::Infallible;

        fn read(&self, _path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
            // Syntactically broken YAML — forces the configurator into an
            // error path so we can check that the caller-visible error is
            // non-empty.
            Ok(b"::: not : yaml :::".to_vec())
        }
    }

    #[test]
    fn missing_files_return_empty_configs() {
        let configs = read_stable_configs_with_reader(&NotFoundReader, "browser-sdk", false)
            .expect("reader NotFound should not surface as error");
        assert_eq!(configs.len(), 0);
    }

    #[test]
    fn malformed_yaml_surfaces_error_to_caller() {
        // Guards the err-path in read_stable_configs_with_reader: when
        // .data() is Err, diagnostic logs are emitted regardless of
        // debug_logs, and the error is propagated as a non-empty String.
        let err = read_stable_configs_with_reader(&MalformedReader, "browser-sdk", false)
            .expect_err("malformed YAML must surface as error");
        assert!(!err.is_empty(), "error message should not be empty");
    }

    #[cfg(not(windows))]
    mod platform_read {
        use super::PlatformConfigRead;
        use alloc::format;
        use libdd_library_config::{ConfigRead, ConfigReadError, MAX_CONFIG_FILE_SIZE};
        use pretty_assertions::assert_eq;
        use std::fs;
        use std::io::Write;
        use std::path::PathBuf;

        fn tmp_path(name: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            // Use pid + name to avoid collisions between parallel tests.
            p.push(format!(
                "inject-browser-sdk-test-{}-{}",
                std::process::id(),
                name
            ));
            p
        }

        #[test]
        fn read_ok_returns_file_contents() {
            let path = tmp_path("read_ok.yaml");
            fs::write(&path, b"application_id: hello").unwrap();
            let result = PlatformConfigRead.read(path.to_str().unwrap()).unwrap();
            let _ = fs::remove_file(&path);
            assert_eq!(result, b"application_id: hello");
        }

        #[test]
        fn read_missing_path_maps_to_not_found() {
            let path = tmp_path("definitely-does-not-exist");
            let err = PlatformConfigRead
                .read(path.to_str().unwrap())
                .expect_err("missing path must be NotFound");
            assert!(matches!(err, ConfigReadError::NotFound));
        }

        #[test]
        fn read_directory_surfaces_io_error() {
            // /tmp is a directory; opening it O_RDONLY succeeds but reading
            // from a directory fd returns an error on Linux/macOS. Either
            // path must surface as ConfigReadError::Io, not a panic.
            let dir = std::env::temp_dir();
            let result = PlatformConfigRead.read(dir.to_str().unwrap());
            assert!(
                matches!(result, Err(ConfigReadError::Io(_))),
                "expected Io error, got {:?}",
                result.as_ref().err().map(core::mem::discriminant),
            );
        }

        #[test]
        fn read_oversize_rejects() {
            // Create a file one byte bigger than the configurator's gate.
            // We don't want to actually allocate MAX_CONFIG_FILE_SIZE+1
            // bytes on disk in all CI environments, so we write it via
            // seek + single-byte write to make the file "sparse" where
            // possible.
            let path = tmp_path("oversize.yaml");
            let mut f = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            // Seek past the max and write one byte — produces a file whose
            // fstat st_size > MAX_CONFIG_FILE_SIZE.
            f.set_len((MAX_CONFIG_FILE_SIZE as u64) + 1).unwrap();
            f.flush().unwrap();
            drop(f);

            let result = PlatformConfigRead.read(path.to_str().unwrap());
            let _ = fs::remove_file(&path);
            assert!(
                matches!(result, Err(ConfigReadError::TooLarge)),
                "expected TooLarge, got {:?}",
                result.as_ref().err().map(core::mem::discriminant),
            );
        }
    }
}
