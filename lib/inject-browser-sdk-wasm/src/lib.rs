// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! WASM bindings for inject-browser-sdk, designed for wazero (Go) consumption.
//!
//! This crate provides a C-ABI compatible interface for WebAssembly, allowing
//! Go applications using wazero to inject the Datadog Browser SDK into HTML documents.
//!
//! ## Memory Model
//!
//! The caller (Go/wazero) is responsible for:
//! 1. Calling `alloc(size)` to allocate memory in WASM linear memory
//! 2. Writing data to the allocated pointer
//! 3. Passing pointers to WASM functions
//! 4. Reading results from WASM memory
//! 5. Calling `dealloc(ptr, size)` to free memory when done
//!
//! ## Usage
//!
//! 1. Create a snippet with `snippet_create_from_json(json_ptr, json_len)`
//! 2. Check for errors with `snippet_error_code(handle)`
//! 3. Create an injector with `injector_create(snippet_handle)`
//! 4. Process HTML chunks with `injector_write(handle, chunk_ptr, chunk_len, result_ptr)`
//! 5. Finalize with `injector_end(handle, result_ptr)`
//! 6. Clean up with `injector_cleanup(handle)` and `snippet_cleanup(handle)`

#![warn(missing_docs)]
#![allow(clippy::missing_safety_doc)]

mod alloc;
mod injector;
mod snippet;

pub use alloc::*;
pub use injector::*;
pub use snippet::*;

