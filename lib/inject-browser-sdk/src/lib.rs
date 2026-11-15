#![no_std]
#![allow(deprecated)] // allow internal usage of deprecated things

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

mod configuration;
pub mod error;
pub mod injector;
mod snippet;

// The filesystem-backed stable-configuration surface only exists on platforms
// with an OS: it needs rustix and libdd-library-config to read the Datadog
// stable configuration files. wasm32-unknown-unknown (the bare, no-OS Wasm
// target used when embedding this crate in a Wasm guest such as a Cocoon
// shim) has no file system, so hosts read stable configuration themselves and
// pass resolved values through the ordinary configuration API instead.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod platform;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod stable_config;

pub use configuration::*;
pub use snippet::*;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use stable_config::*;
