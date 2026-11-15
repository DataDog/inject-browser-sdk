// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Platform-specific helpers for reading stable configuration files.

use alloc::{format, string::String, vec::Vec};
use core::cell::Cell;

use libdd_library_config::{io, ConfigRead, Configurator, LibraryConfig, ProcessInfo};

#[cfg(not(windows))]
mod inner {
    use super::*;

    pub struct PlatformConfigRead;

    impl ConfigRead for PlatformConfigRead {
        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            use alloc::vec;
            use libdd_library_config::MAX_CONFIG_FILE_SIZE;
            use rustix::fs::{fstat, open, Mode, OFlags};
            use rustix::io::read;

            let fd = match open(path, OFlags::RDONLY, Mode::empty()) {
                Ok(fd) => fd,
                // A missing file is an empty configuration layer.
                Err(rustix::io::Errno::NOENT) => return Err(io::ErrorKind::NotFound.into()),
                Err(_) => return Err(io::ErrorKind::Other.into()),
            };

            let file_size = match fstat(&fd) {
                Ok(stats) => match usize::try_from(stats.st_size) {
                    Ok(size) => size,
                    Err(_) => return Err(io::Error::from(io::ErrorKind::Other)),
                },
                Err(_) => return Err(io::Error::from(io::ErrorKind::Other)),
            };

            if file_size > MAX_CONFIG_FILE_SIZE {
                return Err(io::ErrorKind::FileTooLarge.into());
            }

            let mut buf = vec![0u8; file_size];
            let mut total = 0;
            while total < file_size {
                match read(&fd, &mut buf[total..]) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => return Err(io::ErrorKind::Other.into()),
                }
            }
            buf.truncate(total);
            return Ok(buf);
        }
    }

    /// On Linux, `/proc/self/cmdline` and `/proc/self/environ` can be mutated by
    /// the host process and are attacker-influenceable from an injected-library
    /// context. Leave `args`/`envp` empty so rule-based process matching falls
    /// back to `language`-only until an injection-safe channel is available.
    /// TODO(INPLAT-1110): supply args/envp via an injection-safe mechanism.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        return ProcessInfo {
            args: Vec::new(),
            envp: Vec::new(),
            language: language.as_bytes().to_vec(),
        };
    }

    pub fn write_stderr(msg: &str) {
        // SAFETY: rustix::stdio::stderr() requires fd 2 to remain open and
        // not be dynamically reused for another purpose. This crate never
        // closes stderr; host processes (IIS module, native injector) are
        // not expected to either.
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
    use core::ptr::null_mut;
    use libdd_library_config::MAX_CONFIG_FILE_SIZE;
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileSizeEx, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Environment::{
        FreeEnvironmentStringsW, GetCommandLineW, GetEnvironmentStringsW,
    };
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// Reads with Win32 calls so the library stays `no_std` inside IIS workers.
    pub struct PlatformConfigRead;

    struct Handle(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful CreateFileW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn other() -> io::Error {
        return io::ErrorKind::Other.into();
    }

    impl ConfigRead for PlatformConfigRead {
        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null_mut(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                // SAFETY: reads this thread's last-error value.
                let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                return Err(match code {
                    ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => io::ErrorKind::NotFound.into(),
                    _ => other(),
                });
            }
            let file = Handle(raw);

            let mut size = 0i64;
            // SAFETY: `file` is an open handle and `size` is a valid out-pointer.
            if unsafe { GetFileSizeEx(file.0, &mut size) } == 0 {
                return Err(other());
            }
            let size = usize::try_from(size).map_err(|_| return other())?;
            if size > MAX_CONFIG_FILE_SIZE {
                return Err(io::ErrorKind::FileTooLarge.into());
            }

            let mut buf = alloc::vec![0u8; size];
            let mut total = 0;
            while total < size {
                let chunk = u32::try_from(size - total).unwrap_or(u32::MAX);
                let mut read = 0u32;
                // SAFETY: the destination range lies inside `buf`.
                let ok = unsafe {
                    ReadFile(
                        file.0,
                        buf[total..].as_mut_ptr(),
                        chunk,
                        &mut read,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(other());
                }
                if read == 0 {
                    break;
                }
                total += read as usize;
            }
            buf.truncate(total);
            return Ok(buf);
        }
    }

    /// Converts a NUL-terminated UTF-16 string; entries that are not valid
    /// Unicode are skipped, as `std::env` does for `into_string()` failures.
    ///
    /// # Safety
    /// `ptr` must point to a NUL-terminated UTF-16 string.
    unsafe fn wide_to_utf8(ptr: *const u16) -> (Option<Vec<u8>>, usize) {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let units = core::slice::from_raw_parts(ptr, len);
        let text = char::decode_utf16(units.iter().copied())
            .collect::<Result<String, _>>()
            .ok()
            .map(String::into_bytes);
        return (text, len);
    }

    fn process_args() -> Vec<Vec<u8>> {
        let mut count = 0;
        // SAFETY: GetCommandLineW returns this process's command line; the
        // array CommandLineToArgvW returns holds `count` strings and is freed
        // with LocalFree.
        unsafe {
            let argv = CommandLineToArgvW(GetCommandLineW(), &mut count);
            if argv.is_null() {
                return Vec::new();
            }
            let count = usize::try_from(count).unwrap_or(0);
            let args = (0..count)
                .filter_map(|i| return wide_to_utf8(*argv.add(i)).0)
                .collect();
            LocalFree(argv.cast());
            return args;
        }
    }

    fn process_envp() -> Vec<Vec<u8>> {
        let mut envp = Vec::new();
        // SAFETY: the environment block is a sequence of NUL-terminated
        // strings ending with an empty string, freed with FreeEnvironmentStringsW.
        unsafe {
            let block = GetEnvironmentStringsW();
            if block.is_null() {
                return envp;
            }
            let mut cursor = block.cast_const();
            while *cursor != 0 {
                let (entry, len) = wide_to_utf8(cursor);
                // Entries such as `=C:=C:\` name per-drive directories, not variables.
                if let Some(entry) = entry.filter(|entry| return !entry.starts_with(b"=")) {
                    envp.push(entry);
                }
                cursor = cursor.add(len + 1);
            }
            FreeEnvironmentStringsW(block);
        }
        return envp;
    }

    /// On Windows (IIS hosting), argv/envp aren't mutable by other entities in
    /// the host process model, so reading them is safe. This is intentionally
    /// different from the Unix branch, where /proc/self/{cmdline,environ} can't
    /// be trusted from an injected library.
    pub fn detect_process_info(language: &str) -> ProcessInfo {
        return ProcessInfo {
            args: process_args(),
            envp: process_envp(),
            language: language.as_bytes().to_vec(),
        };
    }

    /// No-op on Windows: IIS workers have no console, and hosts receive the
    /// diagnostics in the returned error instead.
    pub fn write_stderr(_msg: &str) {}
}

pub use inner::{detect_process_info, write_stderr, PlatformConfigRead};

/// Outcome of reading the stable-configuration files.
#[cfg_attr(test, derive(Debug))]
pub struct StableConfigRead {
    /// Entries whose rules matched this process.
    pub configs: Vec<LibraryConfig>,
    /// Diagnostics captured by the configurator.
    pub logs: Vec<String>,
    /// Whether either configuration file existed.
    pub files_present: bool,
    /// Whether an existing configuration file contained zero bytes.
    pub empty_file_present: bool,
    /// Whether a read failed but the configurator returned an empty result.
    pub read_failed: bool,
}

struct PresenceRecordingReader<'a, R> {
    inner: &'a R,
    present: Cell<bool>,
    empty_file_present: Cell<bool>,
    read_failed: Cell<bool>,
}

impl<R: ConfigRead> ConfigRead for PresenceRecordingReader<'_, R> {
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let outcome = self.inner.read(path);
        match &outcome {
            Ok(bytes) => {
                self.present.set(true);
                if bytes.is_empty() {
                    self.empty_file_present.set(true);
                }
            }
            Err(error) => {
                // NotFound means the layer is simply not installed; every
                // other error is a real read failure.
                if error.kind() != io::ErrorKind::NotFound {
                    self.present.set(true);
                    self.read_failed.set(true);
                }
            }
        }
        return outcome;
    }
}

/// Reads configuration entries from both local and fleet-managed `application_monitoring.yaml`
/// files, preserving the full [`LibraryConfig`] metadata.
///
/// When `debug_logs` is `true`, diagnostic messages from the configurator are written to stderr.
/// On Windows, `debug_logs` is a no-op (see [`write_stderr`]).
pub fn read_stable_configs(language: &str, debug_logs: bool) -> Result<StableConfigRead, String> {
    return read_stable_configs_with_reader(&PlatformConfigRead, language, debug_logs);
}

/// Read the same stable configuration format from explicit paths. `None` keeps
/// the library's normal local or fleet path; IIS uses this for isolated tests.
pub fn read_stable_configs_with_paths(
    language: &str,
    debug_logs: bool,
    local_path: Option<&str>,
    fleet_path: Option<&str>,
) -> Result<StableConfigRead, String> {
    return read_stable_configs_with_reader_at_paths(
        &PlatformConfigRead,
        language,
        debug_logs,
        local_path.unwrap_or(Configurator::LOCAL_STABLE_CONFIGURATION_PATH),
        fleet_path.unwrap_or(Configurator::FLEET_STABLE_CONFIGURATION_PATH),
    );
}

/// Variant of [`read_stable_configs`] that accepts a caller-supplied reader.
/// Intended for tests that need to exercise populated YAML input without
/// touching the real filesystem.
pub fn read_stable_configs_with_reader<R: ConfigRead>(
    reader: &R,
    language: &str,
    debug_logs: bool,
) -> Result<StableConfigRead, String> {
    return read_stable_configs_with_reader_at_paths(
        reader,
        language,
        debug_logs,
        Configurator::LOCAL_STABLE_CONFIGURATION_PATH,
        Configurator::FLEET_STABLE_CONFIGURATION_PATH,
    );
}

fn read_stable_configs_with_reader_at_paths<R: ConfigRead>(
    reader: &R,
    language: &str,
    debug_logs: bool,
    local_path: &str,
    fleet_path: &str,
) -> Result<StableConfigRead, String> {
    let process_info = detect_process_info(language);
    let configurator = Configurator::new(debug_logs);

    let recording = PresenceRecordingReader {
        inner: reader,
        present: Cell::new(false),
        empty_file_present: Cell::new(false),
        read_failed: Cell::new(false),
    };
    let logged_result =
        configurator.get_config_from_reader(&recording, local_path, fleet_path, &process_info);

    // Buffer logs before consuming `logged_result` — `data()` takes self.
    let logs: Vec<String> = logged_result.logs().iter().map(String::from).collect();
    let data = logged_result.data();
    let outcome_empty = matches!(&data, Ok(v) if v.is_empty());
    if debug_logs || data.is_err() || outcome_empty {
        for msg in &logs {
            write_stderr(&format!("[inject-browser-sdk] {}\n", msg));
        }
    }

    match data {
        Ok(configs) => {
            return Ok(StableConfigRead {
                configs,
                logs,
                files_present: recording.present.get(),
                empty_file_present: recording.empty_file_present.get(),
                read_failed: recording.read_failed.get(),
            })
        }
        Err(err) => {
            // Fold captured diagnostics into the error string so callers that
            // never see stderr (notably Windows, where write_stderr is a
            // no-op) still get the full context.
            if logs.is_empty() {
                return Err(format!("{err}"));
            } else {
                return Err(format!("{err}\n{}", logs.join("\n")));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    struct NotFoundReader;

    impl ConfigRead for NotFoundReader {
        fn read(&self, _path: &str) -> io::Result<Vec<u8>> {
            return Err(io::ErrorKind::NotFound.into());
        }
    }

    struct MalformedReader;

    impl ConfigRead for MalformedReader {
        fn read(&self, _path: &str) -> io::Result<Vec<u8>> {
            // Syntactically broken YAML — forces the configurator into an
            // error path so we can check that the caller-visible error is
            // non-empty.
            return Ok(b"::: not : yaml :::".to_vec());
        }
    }

    struct EmptyReader;

    impl ConfigRead for EmptyReader {
        fn read(&self, _path: &str) -> io::Result<Vec<u8>> {
            return Ok(Vec::new());
        }
    }

    /// Serves a DD_RUM_* rule for language=browser-sdk as the local config
    /// and returns NotFound for the fleet config. Used to verify that a
    /// realistic YAML document flows end-to-end through the configurator
    /// (guards against upstream YAML-schema drift that NotFound/Malformed
    /// readers would not catch).
    pub(crate) struct PopulatedReader;

    impl ConfigRead for PopulatedReader {
        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            if path == Configurator::LOCAL_STABLE_CONFIGURATION_PATH {
                return Ok(br#"
rules:
- selectors:
  - origin: language
    matches: ["browser-sdk"]
    operator: equals
  configuration:
    DD_RUM_APPLICATION_ID: app-populated
    DD_RUM_CLIENT_TOKEN: tok-populated
    DD_RUM_SITE: datadoghq.com
"#
                .to_vec());
            } else {
                return Err(io::ErrorKind::NotFound.into());
            }
        }
    }

    #[test]
    fn missing_files_return_empty_configs() {
        let read = read_stable_configs_with_reader(&NotFoundReader, "browser-sdk", false)
            .expect("reader NotFound should not surface as error");
        assert_eq!(read.configs.len(), 0);
        assert!(
            !read.files_present,
            "NotFound on both paths means no stable configuration is installed"
        );
        assert!(!read.empty_file_present);
    }

    #[test]
    fn empty_existing_files_are_present() {
        let read = read_stable_configs_with_reader(&EmptyReader, "browser-sdk", false)
            .expect("empty files should produce an empty configuration");
        assert!(read.configs.is_empty());
        assert!(
            read.files_present,
            "a successful read proves the file exists even when it has zero bytes"
        );
        assert!(read.empty_file_present);
        assert!(!read.read_failed);
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

    #[test]
    fn populated_yaml_with_matching_rule_yields_dd_rum_fields() {
        let read = read_stable_configs_with_reader(&PopulatedReader, "browser-sdk", false)
            .expect("populated reader must succeed");
        assert!(read.files_present, "the local config file was served");
        let configs = read.configs;
        let lookup = |key: &str| {
            return configs
                .iter()
                .find(|c| return c.name == key)
                .map(|c| return c.value.as_str());
        };
        assert_eq!(lookup("DD_RUM_APPLICATION_ID"), Some("app-populated"));
        assert_eq!(lookup("DD_RUM_CLIENT_TOKEN"), Some("tok-populated"));
        assert_eq!(lookup("DD_RUM_SITE"), Some("datadoghq.com"));
    }

    /// Serves a DD_RUM_SITE value on the local config file and a *different*
    /// one on the fleet-managed file. Used to verify which source wins when
    /// the same key is defined in both places.
    struct BothSourcesReader;

    impl ConfigRead for BothSourcesReader {
        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            if path == Configurator::LOCAL_STABLE_CONFIGURATION_PATH {
                return Ok(br#"
rules:
- selectors:
  - origin: language
    matches: ["browser-sdk"]
    operator: equals
  configuration:
    DD_RUM_APPLICATION_ID: app-local
    DD_RUM_CLIENT_TOKEN: tok-local
    DD_RUM_SITE: datadoghq.com
"#
                .to_vec());
            } else if path == Configurator::FLEET_STABLE_CONFIGURATION_PATH {
                return Ok(br#"
rules:
- selectors:
  - origin: language
    matches: ["browser-sdk"]
    operator: equals
  configuration:
    DD_RUM_APPLICATION_ID: app-fleet
    DD_RUM_CLIENT_TOKEN: tok-fleet
    DD_RUM_SITE: datadoghq.eu
"#
                .to_vec());
            } else {
                return Err(io::ErrorKind::NotFound.into());
            }
        }
    }

    #[test]
    fn fleet_overrides_local_when_both_present() {
        // Upstream libdd-library-config semantics: fleet-managed config
        // overrides local. Guards against that contract silently flipping.
        let configs = read_stable_configs_with_reader(&BothSourcesReader, "browser-sdk", false)
            .expect("both-sources reader must succeed")
            .configs;
        let lookup = |key: &str| {
            return configs
                .iter()
                .find(|c| return c.name == key)
                .map(|c| return c.value.as_str());
        };
        assert_eq!(lookup("DD_RUM_SITE"), Some("datadoghq.eu"));
        assert_eq!(lookup("DD_RUM_APPLICATION_ID"), Some("app-fleet"));
        assert_eq!(lookup("DD_RUM_CLIENT_TOKEN"), Some("tok-fleet"));
    }

    #[test]
    fn populated_yaml_skips_non_matching_language() {
        // Rule selects language=browser-sdk; passing a different language
        // must yield zero entries rather than the matching rule's values.
        let read = read_stable_configs_with_reader(&PopulatedReader, "not-browser-sdk", false)
            .expect("populated reader must succeed");
        assert_eq!(read.configs.len(), 0);
        assert!(
            read.files_present,
            "a file was there; it just matched nothing -- distinct from absent"
        );
    }

    #[cfg(not(windows))]
    mod platform_read {
        use super::PlatformConfigRead;
        use alloc::format;
        use libdd_library_config::{io, ConfigRead, MAX_CONFIG_FILE_SIZE};
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
            return p;
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
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }

        #[test]
        fn read_directory_surfaces_io_error() {
            // /tmp is a directory; opening it O_RDONLY succeeds but reading
            // from a directory fd returns an error on Linux/macOS. Either
            // path must surface as an I/O error, not a panic.
            let dir = std::env::temp_dir();
            let result = PlatformConfigRead.read(dir.to_str().unwrap());
            assert!(
                matches!(&result, Err(e) if e.kind() != io::ErrorKind::NotFound),
                "expected an I/O error, got {:?}",
                result.as_ref().err().map(|e| return e.kind()),
            );
        }

        #[test]
        fn read_oversize_rejects() {
            // Create a file one byte bigger than the configurator's gate.
            // set_len zero-extends the file, producing a sparse file where
            // the filesystem supports it — fstat st_size exceeds
            // MAX_CONFIG_FILE_SIZE without materializing that many bytes.
            let path = tmp_path("oversize.yaml");
            let mut f = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            f.set_len((MAX_CONFIG_FILE_SIZE as u64) + 1).unwrap();
            f.flush().unwrap();
            drop(f);

            let result = PlatformConfigRead.read(path.to_str().unwrap());
            let _ = fs::remove_file(&path);
            assert!(
                matches!(&result, Err(e) if e.kind() == io::ErrorKind::FileTooLarge),
                "expected FileTooLarge, got {:?}",
                result.as_ref().err().map(|e| return e.kind()),
            );
        }
    }

    #[cfg(windows)]
    mod platform_read_windows {
        use super::PlatformConfigRead;
        use alloc::format;
        use libdd_library_config::{io, ConfigRead};
        use pretty_assertions::assert_eq;
        use std::fs;
        use std::path::PathBuf;

        fn tmp_path(name: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "inject-browser-sdk-test-{}-{}",
                std::process::id(),
                name
            ));
            p
        }

        #[test]
        fn read_ok_returns_file_contents() {
            // Smoke test that PlatformConfigRead (aliased to StdConfigRead on
            // Windows) actually wires up to std::fs — guards against the
            // Windows inner module silently diverging from Unix.
            let path = tmp_path("win_read_ok.yaml");
            fs::write(&path, b"application_id: hello").unwrap();
            let result = PlatformConfigRead.read(path.to_str().unwrap()).unwrap();
            let _ = fs::remove_file(&path);
            assert_eq!(result, b"application_id: hello");
        }

        #[test]
        fn read_missing_path_maps_to_not_found() {
            let path = tmp_path("win_missing");
            let err = PlatformConfigRead
                .read(path.to_str().unwrap())
                .expect_err("missing path must be NotFound");
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }
    }
}
