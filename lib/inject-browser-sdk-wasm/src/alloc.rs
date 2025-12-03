// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

//! Memory allocation functions for WASM linear memory.
//!
//! These functions allow the host (Go/wazero) to allocate and deallocate
//! memory within the WASM module's linear memory space.

use std::alloc::{alloc as std_alloc, dealloc as std_dealloc, Layout};

/// Allocate a block of memory in WASM linear memory.
///
/// # Arguments
///
/// * `size` - The number of bytes to allocate
///
/// # Returns
///
/// A pointer to the allocated memory block, or 0 if allocation fails.
///
/// # Safety
///
/// The caller must ensure:
/// - The returned pointer is eventually freed with `dealloc`
/// - The memory is not accessed after being freed
#[no_mangle]
pub extern "C" fn alloc(size: u32) -> u32 {
    if size == 0 {
        return 0;
    }

    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(layout) => layout,
        Err(_) => return 0,
    };

    unsafe {
        let ptr = std_alloc(layout);
        if ptr.is_null() {
            return 0;
        }
        return ptr as u32;
    }
}

/// Deallocate a block of memory previously allocated with `alloc`.
///
/// # Arguments
///
/// * `ptr` - Pointer to the memory block to free
/// * `size` - The size of the memory block (must match the size passed to `alloc`)
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` was returned by a previous call to `alloc`
/// - `size` matches the size used in the corresponding `alloc` call
/// - The memory has not already been freed
#[no_mangle]
pub extern "C" fn dealloc(ptr: u32, size: u32) {
    if ptr == 0 || size == 0 {
        return;
    }

    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(layout) => layout,
        Err(_) => return,
    };

    unsafe {
        std_dealloc(ptr as *mut u8, layout);
    }
}

