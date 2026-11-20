//! Minimal "injection only" nginx dynamic module, in Rust on top of the `ngx` crate, calling
//! the `inject-browser-sdk` crate directly (no FFI layer, no C shim).
//!
//! Configuration: `rum_inject on|off;` (http/server/location, default off), a fixed snippet, and
//! `rum_inject_sendfile on|off;` (default off) to pick how file responses are read, see below.
//!
//! Header filter: only `200` main-request `text/html` responses without `Content-Encoding`.
//! A known `Content-Length` grows by the snippet length (the library pads with spaces when it
//! does not inject); an unknown one stays unknown (chunked, no padding). Ranges are disabled and
//! the ETag is weakened, as the body changes.
//!
//! Body filter: every in-memory buffer is written to the injector. Slices from the incoming
//! buffer are emitted zero-copy: the last of them reuses the incoming buffer itself (its `pos`
//! moved forward), earlier ones are new buffers pointing into the same memory, linked before
//! it, so the producer only gets the incoming buffer back once all of them have been sent.
//! Slices that are not from the incoming buffer (snippet, held-back head window, padding) point
//! at injector memory reused by the next call, so they are copied into pool memory. Once the
//! snippet is in, buffers pass through untouched.
//!
//! File responses:
//! * `rum_inject_sendfile off`: `filter_need_in_memory`, as nginx-datadog does: the copy filter
//!   reads the whole file into memory (`output_buffers`) before this filter sees it.
//! * `rum_inject_sendfile on`: file buffers reach this filter as they are; it reads them itself,
//!   `FILE_READ_CHUNK` at a time, only until the snippet is in, and leaves the rest of the file
//!   region to sendfile. A page that never gets the snippet is read whole into pool memory (no
//!   flow control: fine for a benchmark, not for production with huge non-injectable pages).
//!
//! `rum_inject_mode injector|scan|read|split|split_snippet;` (default `injector`) picks the
//! integration: the `Injector` as above, or the offset-based `Scanner` and the ablations that
//! locate its remaining cost, see the `scan` module. `rum_inject_cork on|off;` (default off) keeps
//! `tcp_nopush` on for the responses the module touches, which it otherwise turns off.
//!
//! The filter must sit below the copy filter: see `ngx_module_order`.

mod scan;

use core::ffi::c_void;
use core::{mem, ptr, slice};

use inject_browser_sdk::injector::{Injector, Result as InjectorResult};
use ngx::core::Pool;
use ngx::ffi::{
    ngx_alloc_chain_link, ngx_buf_t, ngx_chain_t, ngx_chain_update_chains, ngx_command_t,
    ngx_conf_enum_t, ngx_conf_set_enum_slot, ngx_conf_set_flag_slot, ngx_conf_t, ngx_create_temp_buf, ngx_flag_t, ngx_http_module_t,
    ngx_http_output_body_filter_pt, ngx_http_output_header_filter_pt, ngx_http_request_t,
    ngx_http_top_body_filter, ngx_http_top_header_filter, ngx_http_weak_etag, ngx_int_t,
    ngx_module_t, ngx_pcalloc, ngx_pool_t, ngx_read_file, ngx_uint_t, NGX_CONF_FLAG, NGX_CONF_TAKE1,
    NGX_HTTP_LOC_CONF,
    NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MAIN_CONF, NGX_HTTP_MODULE, NGX_HTTP_SRV_CONF,
};
use ngx::http::{HttpModule, HttpModuleLocationConf, Merge, MergeConfigError};
use ngx::ngx_string;

const NGX_OK: ngx_int_t = 0;
const NGX_ERROR: ngx_int_t = -1;
const NGX_CONF_UNSET: ngx_flag_t = -1;
const NGX_CONF_UNSET_UINT: ngx_uint_t = ngx_uint_t::MAX;
const NGX_HTTP_OK: usize = 200;

struct Module;

#[derive(Debug)]
struct LocationConf {
    enable: ngx_flag_t,
    /// `rum_inject_sendfile`: leave file buffers to sendfile, reading only the part the
    /// injector needs, instead of having the copy filter read whole files into memory.
    sendfile: ngx_flag_t,
    /// `rum_inject_mode`, one of the `scan::MODE_*`.
    mode: ngx_uint_t,
    /// `rum_inject_cork`: keep `tcp_nopush` on.
    cork: ngx_flag_t,
}

impl Default for LocationConf {
    fn default() -> Self {
        Self {
            enable: NGX_CONF_UNSET,
            sendfile: NGX_CONF_UNSET,
            mode: NGX_CONF_UNSET_UINT,
            cork: NGX_CONF_UNSET,
        }
    }
}

fn merge_flag(value: &mut ngx_flag_t, prev: ngx_flag_t) {
    if *value == NGX_CONF_UNSET {
        *value = if prev == NGX_CONF_UNSET { 0 } else { prev };
    }
}

/// Whether any location of the configuration being loaded turns injection on. Without one the
/// filters are not even installed: with RUM disabled everywhere, a request costs nothing more than
/// without the module. Locations are merged before `postconfiguration`, which reads it.
static ANY_ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl Merge for LocationConf {
    fn merge(&mut self, prev: &Self) -> Result<(), MergeConfigError> {
        merge_flag(&mut self.enable, prev.enable);
        if self.enable == 1 {
            ANY_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        merge_flag(&mut self.sendfile, prev.sendfile);
        merge_flag(&mut self.cork, prev.cork);
        if self.mode == NGX_CONF_UNSET_UINT {
            self.mode = if prev.mode == NGX_CONF_UNSET_UINT { scan::MODE_INJECTOR } else { prev.mode };
        }
        Ok(())
    }
}

/// How much of a file buffer is read per injector write in `rum_inject_sendfile` mode.
const FILE_READ_CHUNK: usize = 32 * 1024;
/// The first read is the injector's head window, which decides where the snippet goes for most
/// pages: past it, the file goes out by sendfile without being read.
const FIRST_READ: usize = 1024;

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = LocationConf;
}

pub(crate) static mut NEXT_HEADER_FILTER: ngx_http_output_header_filter_pt = None;
pub(crate) static mut NEXT_BODY_FILTER: ngx_http_output_body_filter_pt = None;

impl HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { &*ptr::addr_of!(ngx_http_rum_inject_module) }
    }

    unsafe extern "C" fn preconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
        // Each configuration load, a reload included, decides afresh.
        ANY_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        NGX_OK
    }

    unsafe extern "C" fn postconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
        if !ANY_ENABLED.load(core::sync::atomic::Ordering::Relaxed) {
            return NGX_OK;
        }
        // Build the snippet in the master, before workers fork.
        let _ = bench_common::snippet();
        let _ = scan::memory_below();
        let _ = scan::split_at();
        unsafe {
            NEXT_HEADER_FILTER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
            NEXT_BODY_FILTER = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
        NGX_OK
    }
}

static mut MODES: [ngx_conf_enum_t; 6] = [
    ngx_conf_enum_t { name: ngx_string!("injector"), value: scan::MODE_INJECTOR },
    ngx_conf_enum_t { name: ngx_string!("scan"), value: scan::MODE_SCAN },
    ngx_conf_enum_t { name: ngx_string!("read"), value: scan::MODE_READ },
    ngx_conf_enum_t { name: ngx_string!("split"), value: scan::MODE_SPLIT },
    ngx_conf_enum_t { name: ngx_string!("split_snippet"), value: scan::MODE_SPLIT_SNIPPET },
    ngx_conf_enum_t { name: ngx::ffi::ngx_str_t { len: 0, data: ptr::null_mut() }, value: 0 },
];

static mut COMMANDS: [ngx_command_t; 5] = [
    ngx_command_t {
        name: ngx_string!("rum_inject"),
        type_: (NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF | NGX_CONF_FLAG)
            as ngx::ffi::ngx_uint_t,
        set: Some(ngx_conf_set_flag_slot),
        conf: NGX_HTTP_LOC_CONF_OFFSET as ngx::ffi::ngx_uint_t,
        offset: mem::offset_of!(LocationConf, enable) as ngx::ffi::ngx_uint_t,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rum_inject_sendfile"),
        type_: (NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF | NGX_CONF_FLAG)
            as ngx::ffi::ngx_uint_t,
        set: Some(ngx_conf_set_flag_slot),
        conf: NGX_HTTP_LOC_CONF_OFFSET as ngx::ffi::ngx_uint_t,
        offset: mem::offset_of!(LocationConf, sendfile) as ngx::ffi::ngx_uint_t,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("rum_inject_mode"),
        type_: (NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1)
            as ngx::ffi::ngx_uint_t,
        set: Some(ngx_conf_set_enum_slot),
        conf: NGX_HTTP_LOC_CONF_OFFSET as ngx::ffi::ngx_uint_t,
        offset: mem::offset_of!(LocationConf, mode) as ngx::ffi::ngx_uint_t,
        post: unsafe { &raw mut MODES[0] } as *mut c_void,
    },
    ngx_command_t {
        name: ngx_string!("rum_inject_cork"),
        type_: (NGX_HTTP_MAIN_CONF | NGX_HTTP_SRV_CONF | NGX_HTTP_LOC_CONF | NGX_CONF_FLAG)
            as ngx::ffi::ngx_uint_t,
        set: Some(ngx_conf_set_flag_slot),
        conf: NGX_HTTP_LOC_CONF_OFFSET as ngx::ffi::ngx_uint_t,
        offset: mem::offset_of!(LocationConf, cork) as ngx::ffi::ngx_uint_t,
        post: ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

static MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

// The `ngx_modules` export table, as `ngx::ngx_modules!` generates it, but with a module order
// that places this filter below the copy filter (as nginx's own build does for HTTP_FILTER
// modules), so it sees file contents in memory. With the empty order of the macro, a dynamic
// module's filters land on top of the chain, ahead of the copy filter, and get file buffers.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_modules: [*const ngx_module_t; 2] =
    [&raw const ngx_http_rum_inject_module, ptr::null()];

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_module_names: [*const core::ffi::c_char; 2] =
    [c"ngx_http_rum_inject_module".as_ptr(), ptr::null()];

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_module_order: [*const core::ffi::c_char; 3] = [
    c"ngx_http_rum_inject_module".as_ptr(),
    c"ngx_http_copy_filter_module".as_ptr(),
    ptr::null(),
];

#[used]
#[allow(non_upper_case_globals)]
pub static mut ngx_http_rum_inject_module: ngx_module_t = ngx_module_t {
    ctx: &raw const MODULE_CTX as *mut c_void,
    commands: unsafe { &raw mut COMMANDS[0] },
    type_: NGX_HTTP_MODULE as ngx::ffi::ngx_uint_t,
    ..ngx_module_t::default()
};

/// Per-request state, allocated in the request pool (dropped by a pool cleanup).
struct Ctx {
    injector: Injector<'static>,
    padded: bool,
    injected: bool,
    done: bool,
    read_any: bool,
    free: *mut ngx_chain_t,
    busy: *mut ngx_chain_t,
}

pub(crate) fn tag() -> *mut c_void {
    ptr::addr_of!(ngx_http_rum_inject_module) as *mut c_void
}

pub(crate) unsafe fn module_index() -> usize {
    unsafe { (*ptr::addr_of!(ngx_http_rum_inject_module)).ctx_index }
}

unsafe fn get_ctx(r: *mut ngx_http_request_t) -> *mut Ctx {
    unsafe { *(*r).ctx.add(module_index()) as *mut Ctx }
}

unsafe fn location_conf(r: *mut ngx_http_request_t) -> *const LocationConf {
    unsafe { *(*r).loc_conf.add(module_index()) as *const LocationConf }
}

fn starts_with_ignore_case(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
    unsafe {
        let next = NEXT_HEADER_FILTER.expect("next header filter");
        let req = &mut *r;
        let conf = &*location_conf(r);
        let out = &mut req.headers_out;

        let content_type = slice::from_raw_parts(
            if out.content_type.len == 0 { ptr::NonNull::dangling().as_ptr() } else { out.content_type.data },
            out.content_type.len,
        );
        if conf.enable != 1
            || req.main != r
            || out.status != NGX_HTTP_OK
            || out.content_length_n == 0
            || (!out.content_encoding.is_null() && (*out.content_encoding).value.len != 0)
            || !starts_with_ignore_case(content_type, b"text/html")
        {
            return next(r);
        }

        let snippet = bench_common::snippet();
        let length = out.content_length_n;
        if conf.mode != scan::MODE_INJECTOR {
            return scan_header_filter(r, conf, next);
        }
        if length > 0 {
            if !out.content_length.is_null() {
                (*out.content_length).hash = 0;
                out.content_length = ptr::null_mut();
            }
            out.content_length_n = length + snippet.len() as i64;
        }
        // ngx_http_clear_accept_ranges()
        if !out.accept_ranges.is_null() {
            (*out.accept_ranges).hash = 0;
            out.accept_ranges = ptr::null_mut();
        }
        req.set_allow_ranges(0);
        ngx_http_weak_etag(r);

        if req.header_only() != 0 {
            return next(r);
        }

        let pool = Pool::from_ngx_pool(req.pool);
        let ctx = pool.allocate(Ctx {
            injector: Injector::new(snippet),
            padded: length > 0,
            injected: false,
            done: false,
            read_any: false,
            free: ptr::null_mut(),
            busy: ptr::null_mut(),
        });
        if ctx.is_null() {
            return NGX_ERROR;
        }
        *req.ctx.add(module_index()) = ctx as *mut c_void;
        if conf.sendfile != 1 {
            // The copy filter reads files into memory for us (no sendfile for the response).
            req.set_filter_need_in_memory(1);
        } else if conf.cork != 1 {
            // The response is now memory buffers ahead of the file. Corked with `tcp_nopush`,
            // the last of it can wait on Nagle for the client's delayed ACK, 40 ms a response.
            (*req.connection).set_tcp_nopush(ngx::ffi::ngx_connection_tcp_nopush_e_NGX_TCP_NOPUSH_DISABLED as _);
        }

        next(r)
    }
}

/// The header filter of the `scan` module's modes: the headers wait for the decision in `scan`,
/// and go on right away for the ablations.
unsafe fn scan_header_filter(
    r: *mut ngx_http_request_t,
    conf: &LocationConf,
    next: unsafe extern "C" fn(*mut ngx_http_request_t) -> ngx_int_t,
) -> ngx_int_t {
    unsafe {
        let req = &mut *r;
        let out = &mut req.headers_out;
        let length = out.content_length_n;
        if !out.content_length.is_null() {
            (*out.content_length).hash = 0;
            out.content_length = ptr::null_mut();
        }
        if conf.mode == scan::MODE_SPLIT_SNIPPET && length >= 0 {
            out.content_length_n = length + bench_common::snippet().len() as i64;
        }
        if !out.accept_ranges.is_null() {
            (*out.accept_ranges).hash = 0;
            out.accept_ranges = ptr::null_mut();
        }
        req.set_allow_ranges(0);
        ngx_http_weak_etag(r);
        if req.header_only() != 0 {
            return next(r);
        }
        if !scan::create(r, conf.mode, length) {
            return NGX_ERROR;
        }
        if conf.cork != 1 {
            (*req.connection).set_tcp_nopush(ngx::ffi::ngx_connection_tcp_nopush_e_NGX_TCP_NOPUSH_DISABLED as _);
        }
        if conf.mode == scan::MODE_SCAN {
            // The headers wait for the decision, see `scan::commit`.
            return NGX_OK;
        }
        next(r)
    }
}

unsafe fn buf_in_memory(b: *const ngx_buf_t) -> bool {
    unsafe { (*b).temporary() != 0 || (*b).memory() != 0 || (*b).mmap() != 0 }
}

pub(crate) unsafe fn calloc_buf(pool: *mut ngx_pool_t) -> *mut ngx_buf_t {
    unsafe { ngx_pcalloc(pool, mem::size_of::<ngx_buf_t>()) as *mut ngx_buf_t }
}

/// Appends `b` to the chain whose next link pointer is `*ll`.
pub(crate) unsafe fn link(pool: *mut ngx_pool_t, ll: &mut *mut *mut ngx_chain_t, b: *mut ngx_buf_t) -> bool {
    unsafe {
        let cl = ngx_alloc_chain_link(pool);
        if cl.is_null() {
            return false;
        }
        (*cl).buf = b;
        (*cl).next = ptr::null_mut();
        **ll = cl;
        *ll = &raw mut (*cl).next;
        true
    }
}

pub(crate) unsafe fn special(pool: *mut ngx_pool_t, ll: &mut *mut *mut ngx_chain_t, last_buf: bool) -> bool {
    unsafe {
        let b = calloc_buf(pool);
        if b.is_null() {
            return false;
        }
        (*b).tag = tag();
        if last_buf {
            (*b).set_last_buf(1);
        } else {
            (*b).set_flush(1);
        }
        link(pool, ll, b)
    }
}

/// Turns an injector result into buffers. `incoming` is the buffer that was written, or null for
/// the result of `end()`.
unsafe fn emit(
    pool: *mut ngx_pool_t,
    ll: &mut *mut *mut ngx_chain_t,
    incoming: *mut ngx_buf_t,
    result: &InjectorResult<'_>,
) -> bool {
    unsafe {
        let slices = &result.slices[..result.length];
        let last_incoming = if incoming.is_null() {
            None
        } else {
            slices.iter().rposition(|s| s.from_incoming_chunk && !s.bytes.is_empty())
        };

        for (index, s) in slices.iter().enumerate() {
            if s.bytes.is_empty() {
                continue;
            }
            let start = s.bytes.as_ptr() as *mut u8;
            let end = start.add(s.bytes.len());
            let b = if s.from_incoming_chunk && Some(index) == last_incoming {
                // Reuse the incoming buffer: the producer gets it back once it is sent. What the
                // injector held back from its end, a `</head` tag cut short, it copied: the buffer
                // is cut short of it, or it would never be consumed.
                let b = incoming;
                if (*b).in_file() != 0 {
                    // Read into memory for us, but still sent by sendfile: keep both in step.
                    (*b).file_pos += start.offset_from((*b).pos) as i64;
                    (*b).file_last -= (*b).last.offset_from(end) as i64;
                }
                (*b).pos = start;
                (*b).last = end;
                (*b).set_last_buf(0);
                (*b).set_last_in_chain(0);
                b
            } else if s.from_incoming_chunk {
                // Points into `incoming`, which is linked after it.
                let b = calloc_buf(pool);
                if b.is_null() {
                    return false;
                }
                (*b).set_memory(1);
                (*b).start = start;
                (*b).pos = start;
                (*b).last = end;
                (*b).end = end;
                (*b).tag = tag();
                b
            } else {
                let b = ngx_create_temp_buf(pool, s.bytes.len());
                if b.is_null() {
                    return false;
                }
                ptr::copy_nonoverlapping(s.bytes.as_ptr(), (*b).pos, s.bytes.len());
                (*b).last = (*b).pos.add(s.bytes.len());
                (*b).tag = tag();
                b
            };
            if !link(pool, ll, b) {
                return false;
            }
        }

        if !incoming.is_null() && last_incoming.is_none() {
            // Everything was held back by the injector: the buffer is consumed.
            (*incoming).pos = (*incoming).last;
            (*incoming).file_pos = (*incoming).file_last;
            if (*incoming).flush() != 0 && !special(pool, ll, false) {
                return false;
            }
        }
        true
    }
}

/// Links `b` as it is, past the point where the injector needs to see the data: the snippet is in,
/// or never will be. The last buffer is followed by the padding then owed, if any.
unsafe fn pass_through(
    pool: *mut ngx_pool_t,
    ll: &mut *mut *mut ngx_chain_t,
    ctx: &mut Ctx,
    b: *mut ngx_buf_t,
    last: bool,
) -> bool {
    unsafe {
        if !last || ctx.done {
            return link(pool, ll, b);
        }
        ctx.done = true;
        let result = if ctx.padded { ctx.injector.end() } else { ctx.injector.end_without_padding() };
        if result.length == 0 || result.slices[..result.length].iter().all(|s| s.bytes.is_empty()) {
            return link(pool, ll, b);
        }
        // An empty last buffer only carries the end of the response, which the padding takes over:
        // linked without its flag, it would be a zero size buffer the writer refuses.
        let empty = (*b).last == (*b).pos && ((*b).in_file() == 0 || (*b).file_last == (*b).file_pos);
        if !empty {
            (*b).set_last_buf(0);
            (*b).set_last_in_chain(0);
            if !link(pool, ll, b) {
                return false;
            }
        }
        emit(pool, ll, ptr::null_mut(), &result) && special(pool, ll, true)
    }
}

unsafe extern "C" fn body_filter(r: *mut ngx_http_request_t, input: *mut ngx_chain_t) -> ngx_int_t {
    unsafe {
        let next = NEXT_BODY_FILTER.expect("next body filter");
        if !get_ctx(r).is_null() && (*location_conf(r)).mode != scan::MODE_INJECTOR {
            return scan::body_filter(r, input);
        }
        let ctx = get_ctx(r);
        if ctx.is_null() || ((*ctx).done || input.is_null()) && (*ctx).busy.is_null() {
            return next(r, input);
        }
        let ctx = &mut *ctx;
        let pool = (*r).pool;

        let mut out: *mut ngx_chain_t = ptr::null_mut();
        if ctx.done || input.is_null() {
            out = input;
        } else {
            let mut ll: *mut *mut ngx_chain_t = &raw mut out;
            let mut cl = input;
            while !cl.is_null() {
                let b = (*cl).buf;
                cl = (*cl).next;
                let last = (*b).last_buf() != 0;
                if ctx.done || !ctx.injector.is_pending() {
                    // Injected, or given up: nothing is held back, so the rest passes as is, files
                    // by sendfile, and only the padding is left to add at the end.
                    if !pass_through(pool, &mut ll, ctx, b, last) {
                        return NGX_ERROR;
                    }
                    continue;
                }

                if buf_in_memory(b) && (*b).last > (*b).pos {
                    let chunk = slice::from_raw_parts((*b).pos, (*b).last.offset_from((*b).pos) as usize);
                    let result = ctx.injector.write(chunk);
                    ctx.injected = result.injected;
                    if !emit(pool, &mut ll, b, &result) {
                        return NGX_ERROR;
                    }
                } else if (*b).in_file() != 0 && (*b).file_last > (*b).file_pos {
                    // Only with rum_inject_sendfile: read the file region a chunk at a time
                    // until the snippet is in, leave the rest of it to sendfile.
                    while ctx.injector.is_pending() && (*b).file_pos < (*b).file_last {
                        let piece = if ctx.read_any { FILE_READ_CHUNK } else { FIRST_READ };
                        ctx.read_any = true;
                        let size = piece.min(((*b).file_last - (*b).file_pos) as usize);
                        let m = ngx_create_temp_buf(pool, size);
                        if m.is_null() {
                            return NGX_ERROR;
                        }
                        let n = ngx_read_file((*b).file, (*m).pos, size, (*b).file_pos);
                        if n != size as isize {
                            return NGX_ERROR;
                        }
                        (*m).last = (*m).pos.add(size);
                        (*m).tag = tag();
                        (*b).file_pos += size as i64;
                        let result = ctx.injector.write(slice::from_raw_parts((*m).pos, size));
                        ctx.injected = result.injected;
                        if !emit(pool, &mut ll, m, &result) {
                            return NGX_ERROR;
                        }
                    }
                    if (*b).file_pos < (*b).file_last {
                        if !pass_through(pool, &mut ll, ctx, b, last) {
                            return NGX_ERROR;
                        }
                        continue;
                    }
                } else if !last && !link(pool, &mut ll, b) {
                    return NGX_ERROR;
                }

                if last {
                    let padded = ctx.padded;
                    let result = if padded {
                        ctx.injector.end()
                    } else {
                        ctx.injector.end_without_padding()
                    };
                    if !emit(pool, &mut ll, ptr::null_mut(), &result) || !special(pool, &mut ll, true) {
                        return NGX_ERROR;
                    }
                    ctx.done = true;
                }
            }
            if out.is_null() {
                return NGX_OK;
            }
        }

        let rc = next(r, out);
        ngx_chain_update_chains(pool, &mut ctx.free, &mut ctx.busy, &mut out, tag());
        rc
    }
}
