// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Injector API for WASM.
//!
//! Provides functions to inject the Browser SDK snippet into HTML documents.
//!
//! ## Result Buffer Layout
//!
//! The `injector_write` and `injector_end` functions write results to a caller-provided buffer
//! with the following layout (all values are little-endian):
//!
//! ```text
//! Offset  Size  Description
//! ------  ----  -----------
//! 0       4     slices_count (u32) - Number of slices (0-4)
//! 4       1     injected (u8) - 1 if injection occurred, 0 otherwise
//! 5       9     slice[0]: ptr (u32) + len (u32) + from_chunk (u8)
//! 14      9     slice[1]: ptr (u32) + len (u32) + from_chunk (u8)
//! 23      9     slice[2]: ptr (u32) + len (u32) + from_chunk (u8)
//! 32      9     slice[3]: ptr (u32) + len (u32) + from_chunk (u8)
//! ```
//!
//! Total buffer size required: 41 bytes
//!
//! The `from_chunk` flag indicates whether the slice points to data within the
//! input chunk (1) or to internal buffers (0). When `from_chunk` is 1, the Go
//! caller can use the offset from the original chunk pointer to safely access
//! the data without raw pointer manipulation.

use std::slice;

use inject_browser_sdk::injector::{self, Injector as CoreInjector};

use crate::snippet::snippet_get_content;

/// Size of the result buffer required for `injector_write` and `injector_end`.
pub const RESULT_BUFFER_SIZE: u32 = 41;

/// Internal injector wrapper.
struct InjectorInternal {
    inner: CoreInjector<'static>,
}

impl InjectorInternal {
    fn into_handle(self) -> u32 {
        Box::into_raw(Box::new(self)) as u32
    }

    unsafe fn from_handle(handle: u32) -> Box<Self> {
        Box::from_raw(handle as *mut Self)
    }

    unsafe fn borrow_handle_mut(handle: u32) -> &'static mut Self {
        &mut *(handle as *mut Self)
    }
}

/// Create an injector from a snippet.
///
/// # Arguments
///
/// * `snippet_handle` - Handle to a snippet created with `snippet_create_from_json`
///
/// # Returns
///
/// A handle to the created injector. Must be freed with `injector_cleanup`.
#[no_mangle]
pub extern "C" fn injector_create(snippet_handle: u32) -> u32 {
    let snippet_content = snippet_get_content(snippet_handle);

    return InjectorInternal {
        inner: CoreInjector::new(snippet_content),
    }
    .into_handle();
}

/// Write a chunk of HTML to the injector.
///
/// # Arguments
///
/// * `handle` - The injector handle
/// * `chunk_ptr` - Pointer to the HTML chunk data
/// * `chunk_len` - Length of the chunk in bytes
/// * `result_ptr` - Pointer to a buffer of at least 41 bytes for the result
///
/// # Returns
///
/// The number of slices in the result (0-4).
///
/// # Result Buffer
///
/// The result is written to `result_ptr` with the layout described in the module documentation.
/// Each slice contains:
/// - `ptr`: Pointer to the slice data in WASM memory
/// - `len`: Length of the slice in bytes
/// - `from_chunk`: 1 if the pointer is within the input chunk, 0 otherwise
#[no_mangle]
pub extern "C" fn injector_write(
    handle: u32,
    chunk_ptr: u32,
    chunk_len: u32,
    result_ptr: u32,
) -> u32 {
    let injector = unsafe { InjectorInternal::borrow_handle_mut(handle) };
    let chunk = unsafe { slice::from_raw_parts(chunk_ptr as *const u8, chunk_len as usize) };

    let result = injector.inner.write(chunk);
    return write_result_to_buffer(result, result_ptr);
}

/// Signal the end of the HTML document.
///
/// If injection hasn't occurred yet, this returns padding spaces to maintain
/// the expected content length.
///
/// # Arguments
///
/// * `handle` - The injector handle
/// * `result_ptr` - Pointer to a buffer of at least 41 bytes for the result
///
/// # Returns
///
/// The number of slices in the result (0-4).
#[no_mangle]
pub extern "C" fn injector_end(handle: u32, result_ptr: u32) -> u32 {
    let injector = unsafe { InjectorInternal::borrow_handle_mut(handle) };

    let result = injector.inner.end();
    return write_result_to_buffer(result, result_ptr);
}

/// Free the memory associated with an injector.
///
/// # Arguments
///
/// * `handle` - The injector handle to free
///
/// # Safety
///
/// Do not use the handle after calling this function.
#[no_mangle]
pub extern "C" fn injector_cleanup(handle: u32) {
    if handle != 0 {
        unsafe {
            _ = InjectorInternal::from_handle(handle);
        }
    }
}

/// Get the required size for the result buffer.
///
/// # Returns
///
/// The number of bytes required for the result buffer (41 bytes).
#[no_mangle]
pub extern "C" fn injector_result_buffer_size() -> u32 {
    return RESULT_BUFFER_SIZE;
}

/// Write the injector result to a buffer.
fn write_result_to_buffer(result: injector::Result<'_>, result_ptr: u32) -> u32 {
    let buffer =
        unsafe { slice::from_raw_parts_mut(result_ptr as *mut u8, RESULT_BUFFER_SIZE as usize) };

    // Write slices_count (u32, little-endian)
    let slices_count = result.length as u32;
    buffer[0..4].copy_from_slice(&slices_count.to_le_bytes());

    // Write injected flag (u8)
    buffer[4] = if result.injected { 1 } else { 0 };

    // Write each slice (up to 4)
    for i in 0..4 {
        let offset = 5 + i * 9;
        if i < result.length {
            let slice = &result.slices[i];
            let ptr = slice.bytes.as_ptr() as u32;
            let len = slice.bytes.len() as u32;
            let from_chunk: u8 = if slice.from_incoming_chunk { 1 } else { 0 };

            buffer[offset..offset + 4].copy_from_slice(&ptr.to_le_bytes());
            buffer[offset + 4..offset + 8].copy_from_slice(&len.to_le_bytes());
            buffer[offset + 8] = from_chunk;
        } else {
            // Zero out unused slots
            buffer[offset..offset + 9].fill(0);
        }
    }

    return slices_count;
}

