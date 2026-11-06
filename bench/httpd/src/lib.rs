//! Minimal "injection only" Apache httpd output filter module, in Rust over bindgen'd httpd/APR
//! headers, calling the `inject-browser-sdk` crate directly (no FFI layer). No C shim: the APR
//! ring/bucket macros it needs are re-expressed below.
//!
//! Configuration: load the module and enable the `RUM_INJECT` output filter, e.g.
//! `SetOutputFilter RUM_INJECT`; nothing else, and a fixed snippet. `RUM_INJECT_WHOLE` is the same
//! filter without splitting file buckets (a file bucket is then read whole, mmap'd by APR up to
//! 4 MiB), kept to measure the difference.
//!
//! On its first brigade the filter keeps only `200` main-request `text/html` responses without
//! `Content-Encoding`, and removes itself otherwise. A valid `Content-Length` grows by the snippet
//! length (the library pads with spaces when it does not inject); an invalid one is removed; an
//! absent one stays absent (no padding).
//!
//! Each data bucket is read and written to the injector. Slices from the incoming bucket become
//! copies of that bucket (shared resource, adjusted start/length), so nothing is copied; slices
//! that are not (snippet, held-back head window, padding) are copied into heap buckets, as the
//! injector reuses that memory on the next call. File buckets are split into `FILE_READ_CHUNK`
//! pieces before being read. Once the snippet is in, the filter removes itself and leaves the
//! rest of the brigade, and every later one, untouched (so the file past the injection point
//! still goes out by sendfile).

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#![allow(clippy::missing_safety_doc)]

#[allow(improper_ctypes, unnecessary_transmutes)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/httpd.rs"));
}

use core::ffi::{c_char, c_void, CStr};
use core::{mem, ptr, slice};

use inject_browser_sdk::injector::{Injector, Result as InjectorResult};
use sys::*;

/// Splits file buckets into `FILE_READ_CHUNK` pieces before reading them, so what follows the
/// injection point stays a file bucket (sendfile).
const FILTER_NAME: &CStr = c"RUM_INJECT";
/// Reads buckets as they come: a file bucket is read whole (mmap'd up to 4 MiB by APR).
const WHOLE_FILTER_NAME: &CStr = c"RUM_INJECT_WHOLE";
const FILE_READ_CHUNK: apr_size_t = 32 * 1024;

#[no_mangle]
pub static mut rum_inject_module: module = module {
    version: MODULE_MAGIC_NUMBER_MAJOR as _,
    minor_version: MODULE_MAGIC_NUMBER_MINOR as _,
    module_index: -1,
    name: c"mod_rum_inject.c".as_ptr(),
    dynamic_load_handle: ptr::null_mut(),
    next: ptr::null_mut(),
    magic: MODULE_MAGIC_COOKIE as _,
    rewrite_args: None,
    create_dir_config: None,
    merge_dir_config: None,
    create_server_config: None,
    merge_server_config: None,
    cmds: ptr::null(),
    register_hooks: Some(register_hooks),
    flags: 0,
};

unsafe extern "C" fn register_hooks(_pool: *mut apr_pool_t) {
    let _ = bench_common::snippet();
    unsafe {
        ap_register_output_filter(
            FILTER_NAME.as_ptr(),
            Some(filter::<true>),
            None,
            ap_filter_type_AP_FTYPE_RESOURCE,
        );
        ap_register_output_filter(
            WHOLE_FILTER_NAME.as_ptr(),
            Some(filter::<false>),
            None,
            ap_filter_type_AP_FTYPE_RESOURCE,
        );
    }
}

struct Ctx {
    injector: Injector<'static>,
    padded: bool,
}

unsafe extern "C" fn drop_ctx(data: *mut c_void) -> apr_status_t {
    unsafe { drop(Box::from_raw(data as *mut Ctx)) };
    APR_SUCCESS as apr_status_t
}

// --- APR ring / bucket macros -------------------------------------------------------------

unsafe fn sentinel(bb: *mut apr_bucket_brigade) -> *mut apr_bucket {
    // APR_BRIGADE_SENTINEL: the list head poses as a bucket whose `link` is the head.
    unsafe { (&raw mut (*bb).list as *mut u8).sub(mem::offset_of!(apr_bucket, link)) as *mut apr_bucket }
}

unsafe fn first(bb: *mut apr_bucket_brigade) -> *mut apr_bucket {
    unsafe { (*bb).list.next }
}

unsafe fn next(e: *mut apr_bucket) -> *mut apr_bucket {
    unsafe { (*e).link.next }
}

/// APR_BUCKET_INSERT_BEFORE(e, n)
unsafe fn insert_before(e: *mut apr_bucket, n: *mut apr_bucket) {
    unsafe {
        (*n).link.next = e;
        (*n).link.prev = (*e).link.prev;
        (*(*e).link.prev).link.next = n;
        (*e).link.prev = n;
    }
}

/// apr_bucket_delete(e)
unsafe fn delete(e: *mut apr_bucket) {
    unsafe {
        (*(*e).link.prev).link.next = (*e).link.next;
        (*(*e).link.next).link.prev = (*e).link.prev;
        ((*(*e).type_).destroy.unwrap())((*e).data);
        ((*e).free.unwrap())(e as *mut c_void);
    }
}

unsafe fn is_metadata(e: *mut apr_bucket) -> bool {
    unsafe { (*(*e).type_).is_metadata == apr_bucket_type_t_APR_BUCKET_METADATA }
}

unsafe fn is_file(e: *mut apr_bucket) -> bool {
    unsafe { ptr::eq((*e).type_, &raw const apr_bucket_type_file) }
}

unsafe fn is_eos(e: *mut apr_bucket) -> bool {
    unsafe { ptr::eq((*e).type_, &raw const apr_bucket_type_eos) }
}

fn header<'a>(table: *mut apr_table_t, name: &CStr) -> Option<&'a [u8]> {
    unsafe {
        let value = apr_table_get(table, name.as_ptr());
        (!value.is_null()).then(|| CStr::from_ptr(value).to_bytes())
    }
}

// --- the filter ---------------------------------------------------------------------------

/// Starts the injection for this response, or returns None when it is not eligible.
unsafe fn start(f: *mut ap_filter_t) -> Option<*mut Ctx> {
    unsafe {
        let r = (*f).r;
        if !(*r).main.is_null() || (*r).status != 200 || (*r).content_type.is_null() {
            return None;
        }
        let content_type = CStr::from_ptr((*r).content_type).to_bytes();
        if content_type.len() < 9 || !content_type[..9].eq_ignore_ascii_case(b"text/html") {
            return None;
        }
        if header((*r).headers_out, c"Content-Encoding").is_some_and(|v| !v.is_empty())
            || !(*r).content_encoding.is_null()
        {
            return None;
        }

        let injector = Injector::new(bench_common::snippet());
        let padded = match header((*r).headers_out, c"Content-Length") {
            None => false,
            Some(value) => match injector.content_length_with_snippet(value) {
                Some(length) => {
                    ap_set_content_length(r, length as apr_off_t);
                    true
                }
                None => {
                    apr_table_unset((*r).headers_out, c"Content-Length".as_ptr());
                    false
                }
            },
        };
        apr_table_unset((*r).headers_out, c"ETag".as_ptr());
        if (*r).header_only != 0 {
            return None;
        }

        let ctx = Box::into_raw(Box::new(Ctx { injector, padded }));
        apr_pool_cleanup_register((*r).pool, ctx as *const c_void, Some(drop_ctx), Some(apr_pool_cleanup_null));
        Some(ctx)
    }
}

/// Puts the result's slices before `e`, as bucket copies for slices of `e` itself (`chunk` is
/// `e`'s data) and as heap buckets otherwise.
unsafe fn emit(
    e: *mut apr_bucket,
    chunk: &[u8],
    result: &InjectorResult<'_>,
    list: *mut apr_bucket_alloc_t,
) -> apr_status_t {
    unsafe {
        for s in &result.slices[..result.length] {
            if s.bytes.is_empty() {
                continue;
            }
            let n = if s.from_incoming_chunk {
                let mut copy: *mut apr_bucket = ptr::null_mut();
                let rv = ((*(*e).type_).copy.unwrap())(e, &mut copy);
                if rv != APR_SUCCESS as apr_status_t {
                    return rv;
                }
                (*copy).start += s.bytes.as_ptr().offset_from(chunk.as_ptr()) as apr_off_t;
                (*copy).length = s.bytes.len() as apr_size_t;
                copy
            } else {
                apr_bucket_heap_create(s.bytes.as_ptr() as *const c_char, s.bytes.len() as apr_size_t, None, list)
            };
            insert_before(e, n);
        }
        APR_SUCCESS as apr_status_t
    }
}

unsafe extern "C" fn filter<const SPLIT_FILES: bool>(
    f: *mut ap_filter_t,
    bb: *mut apr_bucket_brigade,
) -> apr_status_t {
    unsafe {
        if (*f).ctx.is_null() {
            match start(f) {
                Some(ctx) => (*f).ctx = ctx as *mut c_void,
                None => {
                    ap_remove_output_filter(f);
                    return ap_pass_brigade((*f).next, bb);
                }
            }
        }
        let ctx = &mut *((*f).ctx as *mut Ctx);
        let list = (*(*f).c).bucket_alloc;
        let end = sentinel(bb);

        let mut e = first(bb);
        while e != end {
            if is_metadata(e) {
                if is_eos(e) {
                    let result = if ctx.padded {
                        ctx.injector.end()
                    } else {
                        ctx.injector.end_without_padding()
                    };
                    let rv = emit(e, &[], &result, list);
                    if rv != APR_SUCCESS as apr_status_t {
                        return rv;
                    }
                    ap_remove_output_filter(f);
                    break;
                }
                e = next(e);
                continue;
            }

            if SPLIT_FILES && is_file(e) && (*e).length > FILE_READ_CHUNK {
                let rv = ((*(*e).type_).split.unwrap())(e, FILE_READ_CHUNK);
                if rv != APR_SUCCESS as apr_status_t {
                    return rv;
                }
            }
            let mut data: *const c_char = ptr::null();
            let mut length: apr_size_t = 0;
            let rv = ((*(*e).type_).read.unwrap())(e, &mut data, &mut length, apr_read_type_e_APR_BLOCK_READ);
            if rv != APR_SUCCESS as apr_status_t {
                return rv;
            }
            // Reading a file bucket may split it: look at what follows only now.
            let following = next(e);
            if length == 0 {
                e = following;
                continue;
            }
            let chunk = slice::from_raw_parts(data as *const u8, length as usize);
            let result = ctx.injector.write(chunk);
            let unchanged = result.length == 1
                && result.slices[0].from_incoming_chunk
                && result.slices[0].bytes.len() == chunk.len();
            if !unchanged {
                let rv = emit(e, chunk, &result, list);
                if rv != APR_SUCCESS as apr_status_t {
                    return rv;
                }
                delete(e);
            }
            if result.injected {
                // Nothing is held back nor padded past the injection.
                ap_remove_output_filter(f);
                break;
            }
            e = following;
        }

        ap_pass_brigade((*f).next, bb)
    }
}
