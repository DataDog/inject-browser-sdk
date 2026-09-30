// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Platform-specific helpers for reading stable configuration files.

use alloc::{format, string::String, vec, vec::Vec};
use core::cell::Cell;

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
                return usize::try_from(s.st_size)
                    .map_err(|_| return ConfigReadError::Io(rustix::io::Errno::INVAL));
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

/// Default path of the locally-managed `application_monitoring.yaml`.
pub const LOCAL_STABLE_CONFIGURATION_PATH: &str = Configurator::LOCAL_STABLE_CONFIGURATION_PATH;
/// Default path of the fleet-managed `application_monitoring.yaml`, which takes
/// precedence over the local one.
pub const FLEET_STABLE_CONFIGURATION_PATH: &str = Configurator::FLEET_STABLE_CONFIGURATION_PATH;

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

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Drops a leading UTF-8 byte-order mark.
///
/// Windows produces one readily -- Notepad adds it, and PowerShell 5.1's
/// `Set-Content -Encoding UTF8` always does -- and the YAML parser reads it as
/// the start of a second document, failing with "more than one document is not
/// supported". That names nothing the operator can act on, and it takes RUM
/// injection down on every site relying on stable configuration, so tolerate the
/// mark rather than reporting a broken file.
fn strip_utf8_bom(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(UTF8_BOM) {
        bytes.drain(..UTF8_BOM.len());
    }
    return bytes;
}

struct PresenceRecordingReader<'a, R> {
    inner: &'a R,
    present: Cell<bool>,
    empty_file_present: Cell<bool>,
    read_failed: Cell<bool>,
}

impl<R: ConfigRead> ConfigRead for PresenceRecordingReader<'_, R> {
    type IoError = R::IoError;

    fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
        // Every read funnels through here, so this is the one place the mark has
        // to be handled for both files and both entry points. Stripping before
        // the emptiness check treats a BOM-only file as the empty file it is.
        let outcome = self.inner.read(path).map(strip_utf8_bom);
        match &outcome {
            Ok(bytes) => {
                self.present.set(true);
                if bytes.is_empty() {
                    self.empty_file_present.set(true);
                }
            }
            Err(ConfigReadError::NotFound) => {}
            Err(_) => {
                self.present.set(true);
                self.read_failed.set(true);
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

/// Variant of [`read_stable_configs`] that accepts a caller-supplied reader.
/// Intended for tests that need to exercise populated YAML input without
/// touching the real filesystem.
pub fn read_stable_configs_with_reader<R: ConfigRead>(
    reader: &R,
    language: &str,
    debug_logs: bool,
) -> Result<StableConfigRead, String> {
    return read_stable_configs_with_paths(
        reader,
        language,
        debug_logs,
        LOCAL_STABLE_CONFIGURATION_PATH,
        FLEET_STABLE_CONFIGURATION_PATH,
    );
}

/// Variant of [`read_stable_configs_with_reader`] that also takes the two
/// configuration-file paths.
///
/// The production paths are compile-time constants in libdatadog, so tests that
/// need real files on disk have no other way to point the reader somewhere
/// writable. Mirrors libdatadog's own FFI, which exposes the same escape hatch.
pub fn read_stable_configs_with_paths<R: ConfigRead>(
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
        type IoError = core::convert::Infallible;

        fn read(&self, _path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
            return Err(ConfigReadError::NotFound);
        }
    }

    struct MalformedReader;

    impl ConfigRead for MalformedReader {
        type IoError = core::convert::Infallible;

        fn read(&self, _path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
            // Syntactically broken YAML — forces the configurator into an
            // error path so we can check that the caller-visible error is
            // non-empty.
            return Ok(b"::: not : yaml :::".to_vec());
        }
    }

    struct EmptyReader;

    impl ConfigRead for EmptyReader {
        type IoError = core::convert::Infallible;

        fn read(&self, _path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
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
        type IoError = core::convert::Infallible;

        fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
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
                return Err(ConfigReadError::NotFound);
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

    /// Wraps another reader, prefixing its bytes with a UTF-8 BOM the way a
    /// Windows editor or PowerShell 5.1's `Set-Content -Encoding UTF8` would.
    struct BomPrefixedReader<R>(R);

    impl<R: ConfigRead> ConfigRead for BomPrefixedReader<R> {
        type IoError = R::IoError;

        fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
            return self.0.read(path).map(|bytes| {
                let mut with_bom = UTF8_BOM.to_vec();
                with_bom.extend_from_slice(&bytes);
                return with_bom;
            });
        }
    }

    #[test]
    fn a_utf8_bom_does_not_hide_the_configuration() {
        // Regression: the YAML parser reads a BOM as a second document and fails
        // the whole read, which drops the stable-config handle and silently
        // disables injection on every site that has no IIS section of its own.
        let read = read_stable_configs_with_reader(
            &BomPrefixedReader(PopulatedReader),
            "browser-sdk",
            false,
        )
        .expect("a BOM must not turn a valid file into a read error");
        assert!(!read.read_failed);
        assert_eq!(
            read.configs
                .iter()
                .find(|c| return c.name == "DD_RUM_APPLICATION_ID")
                .map(|c| return c.value.as_str()),
            Some("app-populated"),
            "the entries behind the BOM must still be read"
        );
    }

    #[test]
    fn a_bom_only_file_counts_as_empty() {
        let read =
            read_stable_configs_with_reader(&BomPrefixedReader(EmptyReader), "browser-sdk", false)
                .expect("a BOM-only file should read as empty, not broken");
        assert!(read.configs.is_empty());
        assert!(
            read.empty_file_present,
            "nothing but a mark is the same as nothing at all"
        );
    }

    /// Serves a DD_RUM_SITE value on the local config file and a *different*
    /// one on the fleet-managed file. Used to verify which source wins when
    /// the same key is defined in both places.
    struct BothSourcesReader;

    impl ConfigRead for BothSourcesReader {
        type IoError = core::convert::Infallible;

        fn read(&self, path: &str) -> Result<Vec<u8>, ConfigReadError<Self::IoError>> {
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
                return Err(ConfigReadError::NotFound);
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
                matches!(result, Err(ConfigReadError::TooLarge)),
                "expected TooLarge, got {:?}",
                result.as_ref().err().map(core::mem::discriminant),
            );
        }
    }

    #[cfg(windows)]
    mod platform_read_windows {
        use super::PlatformConfigRead;
        use alloc::format;
        use libdd_library_config::{ConfigRead, ConfigReadError};
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
            assert!(matches!(err, ConfigReadError::NotFound));
        }
    }
}
