// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Snippet API for WASM.
//!
//! Provides functions to create and manage Browser SDK snippets from JSON configuration.

use std::slice;

use inject_browser_sdk::{error::Error, generate_snippet, Configuration};

/// Internal snippet storage.
struct SnippetInternal {
    /// The snippet content on success, or error message on failure.
    inner: Result<Vec<u8>, String>,
}

impl SnippetInternal {
    fn new(result: Result<Vec<u8>, Error>) -> Self {
        match result {
            Ok(content) => Self {
                inner: Ok(content),
            },
            Err(error) => Self {
                inner: Err(error.to_string()),
            },
        }
    }

    fn into_handle(self) -> u32 {
        Box::into_raw(Box::new(self)) as u32
    }

    unsafe fn from_handle(handle: u32) -> Box<Self> {
        Box::from_raw(handle as *mut Self)
    }

    unsafe fn borrow_handle(handle: u32) -> &'static Self {
        &*(handle as *const Self)
    }
}

/// Create a snippet from a JSON-encoded configuration string.
///
/// # Arguments
///
/// * `json_ptr` - Pointer to the JSON string in WASM memory
/// * `json_len` - Length of the JSON string in bytes
///
/// # Returns
///
/// A handle to the created snippet. Use `snippet_error_code` to check if creation succeeded.
/// The handle must be freed with `snippet_cleanup` when no longer needed.
///
/// # JSON Format
///
/// The JSON object must contain:
/// * `majorVersion`: The major version of the Browser SDK (5 or 6)
/// * `rum`: An object with RUM configuration (clientToken, applicationId, etc.)
#[no_mangle]
pub extern "C" fn snippet_create_from_json(json_ptr: u32, json_len: u32) -> u32 {
    let json_bytes = unsafe { slice::from_raw_parts(json_ptr as *const u8, json_len as usize) };

    let result = serde_json::from_slice::<Configuration>(json_bytes)
        .map_err(|err| Error::Json(err.to_string()))
        .and_then(|configuration| generate_snippet(&configuration));

    return SnippetInternal::new(result).into_handle();
}

/// Get the error code for a snippet.
///
/// # Arguments
///
/// * `handle` - The snippet handle returned by `snippet_create_from_json`
///
/// # Returns
///
/// * `0` - Snippet was created successfully
/// * `1` - JSON parse error
/// * `2` - Unsupported major version
/// * `3` - Unsupported site
/// * `4` - Out of range sample rate
/// * `5` - Empty mandatory configuration field
#[no_mangle]
pub extern "C" fn snippet_error_code(handle: u32) -> u8 {
    let snippet = unsafe { SnippetInternal::borrow_handle(handle) };
    match &snippet.inner {
        Ok(_) => 0,
        Err(_) => 1, // Generic error code for WASM; use error_message for details
    }
}

/// Get the length of the error message for a snippet.
///
/// # Arguments
///
/// * `handle` - The snippet handle
///
/// # Returns
///
/// The length of the error message in bytes, or 0 if there was no error.
#[no_mangle]
pub extern "C" fn snippet_error_message_len(handle: u32) -> u32 {
    let snippet = unsafe { SnippetInternal::borrow_handle(handle) };
    match &snippet.inner {
        Ok(_) => 0,
        Err(msg) => msg.len() as u32,
    }
}

/// Copy the error message to a buffer.
///
/// # Arguments
///
/// * `handle` - The snippet handle
/// * `out_ptr` - Pointer to the output buffer (must be at least `snippet_error_message_len` bytes)
///
/// # Returns
///
/// The number of bytes written to the buffer.
#[no_mangle]
pub extern "C" fn snippet_error_message(handle: u32, out_ptr: u32) -> u32 {
    let snippet = unsafe { SnippetInternal::borrow_handle(handle) };
    match &snippet.inner {
        Ok(_) => 0,
        Err(msg) => {
            let out_slice =
                unsafe { slice::from_raw_parts_mut(out_ptr as *mut u8, msg.len()) };
            out_slice.copy_from_slice(msg.as_bytes());
            return msg.len() as u32;
        }
    }
}

/// Get the length of the snippet content.
///
/// # Arguments
///
/// * `handle` - The snippet handle
///
/// # Returns
///
/// The length of the snippet in bytes, or 0 if there was an error.
#[no_mangle]
pub extern "C" fn snippet_length(handle: u32) -> u32 {
    let snippet = unsafe { SnippetInternal::borrow_handle(handle) };
    match &snippet.inner {
        Ok(content) => content.len() as u32,
        Err(_) => 0,
    }
}

/// Free the memory associated with a snippet.
///
/// # Arguments
///
/// * `handle` - The snippet handle to free
///
/// # Safety
///
/// Do not use the handle after calling this function.
#[no_mangle]
pub extern "C" fn snippet_cleanup(handle: u32) {
    if handle != 0 {
        unsafe {
            _ = SnippetInternal::from_handle(handle);
        }
    }
}

/// Get a pointer to the snippet content bytes.
///
/// This is used internally by the injector. The returned pointer is only valid
/// while the snippet handle is valid.
pub(crate) fn snippet_get_content(handle: u32) -> &'static [u8] {
    let snippet = unsafe { SnippetInternal::borrow_handle(handle) };
    match &snippet.inner {
        Ok(content) => content.as_slice(),
        Err(_) => &[],
    }
}

