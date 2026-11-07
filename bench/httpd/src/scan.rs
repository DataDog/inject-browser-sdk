//! The offset-based integration (`RUM_INJECT_SCAN`) and the ablations that locate its remaining
//! cost (`RUM_INJECT_READ`, `RUM_INJECT_SPLIT`, `RUM_INJECT_SPLIT_SNIPPET`).
//!
//! `RUM_INJECT_SCAN` asks the library for a decision, a body offset, rather than for the bytes:
//! * the first brigade is kept, and the ones after it until the decision: the headers go out with
//!   the first brigade passed, so they wait too, and announce the exact `Content-Length`, the
//!   snippet length more only if it goes in;
//! * the held buckets are httpd's own, but for files: a file bucket no larger than
//!   `RUM_INJECT_MEMORY_BELOW` bytes (environment, default 8 KiB) is read whole, and goes out from
//!   memory around the snippet, an immortal bucket. A larger one is split into the pieces the
//!   decision needs (usually the first 1 KiB), which are read and go out from memory, and the rest,
//!   which stays a file bucket, sent by sendfile. Buckets held across calls are set aside, which
//!   copies transient ones (at most 10 KiB);
//! * early commit: a decision that stays open past the head window (the `<head>` past it) passes
//!   what is held with the snippet length announced, and pads the end with spaces if the snippet
//!   never goes in. A FLUSH bucket does not commit: mod_proxy's `flushpackets` sends one after
//!   every read, and at most 10 KiB is held anyway.
//!
//! Ablations, no library call at all: `READ` reads the first KiB of the response and passes it
//! unchanged; `SPLIT` sends that KiB from memory and the rest of a file by sendfile; `SPLIT_SNIPPET`
//! is `SPLIT` with the snippet bucket in after the first KiB.

use core::ffi::{c_char, c_void};
use core::{ptr, slice};

use inject_browser_sdk::injector::content_length_plus;
use inject_browser_sdk::scanner::{Decision, Scanner};

use crate::sys::*;
use crate::{first, header, insert_before, is_eos, is_file, is_metadata, next, sentinel};

pub(crate) const MODE_SCAN: u8 = 1;
pub(crate) const MODE_READ: u8 = 2;
pub(crate) const MODE_SPLIT: u8 = 3;
pub(crate) const MODE_SPLIT_SNIPPET: u8 = 4;

/// How much of a file bucket is read per scanner feed. The scanner asks for 1 KiB first.
const FIRST_READ: apr_size_t = 1024;
const FILE_READ_CHUNK: apr_size_t = 4096;
const SPLIT_AT: apr_size_t = 1024;
const SUCCESS: apr_status_t = APR_SUCCESS as apr_status_t;

#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    Holding,
    Streaming,
    Done,
}

struct ScanCtx {
    scanner: Scanner,
    state: State,
    /// The `Content-Length` the response came with, if valid.
    length: Option<u64>,
    /// Whether the announced length counts the snippet before it is in: padding is owed then.
    padded: bool,
    injected: bool,
    /// The held buckets.
    held: *mut apr_bucket_brigade,
    /// Body offset of the first byte of the next bucket to come.
    offset: u64,
}

unsafe extern "C" fn drop_ctx(data: *mut c_void) -> apr_status_t {
    unsafe { drop(Box::from_raw(data as *mut ScanCtx)) };
    SUCCESS
}

/// APR_BUCKET_REMOVE(e)
unsafe fn remove(e: *mut apr_bucket) {
    unsafe {
        (*(*e).link.prev).link.next = (*e).link.next;
        (*(*e).link.next).link.prev = (*e).link.prev;
    }
}

/// apr_bucket_destroy(e)
unsafe fn destroy(e: *mut apr_bucket) {
    unsafe {
        ((*(*e).type_).destroy.unwrap())((*e).data);
        ((*e).free.unwrap())(e as *mut c_void);
    }
}

unsafe fn split(e: *mut apr_bucket, at: apr_size_t) -> apr_status_t {
    unsafe { ((*(*e).type_).split.unwrap())(e, at) }
}

unsafe fn read(e: *mut apr_bucket) -> Result<&'static [u8], apr_status_t> {
    unsafe {
        let mut data: *const c_char = ptr::null();
        let mut length: apr_size_t = 0;
        let rv = ((*(*e).type_).read.unwrap())(e, &mut data, &mut length, apr_read_type_e_APR_BLOCK_READ);
        if rv != SUCCESS {
            return Err(rv);
        }
        Ok(if length == 0 { &[] } else { slice::from_raw_parts(data as *const u8, length as usize) })
    }
}

/// Reads `length` bytes of file bucket `e` from `at` on, through a copy of it: `e` stays a file.
unsafe fn read_file_piece(
    e: *mut apr_bucket,
    at: apr_size_t,
    length: apr_size_t,
    each: &mut dyn FnMut(&[u8]),
) -> apr_status_t {
    unsafe {
        let mut copy: *mut apr_bucket = ptr::null_mut();
        let rv = ((*(*e).type_).copy.unwrap())(e, &mut copy);
        if rv != SUCCESS {
            return rv;
        }
        (*copy).start += at as apr_off_t;
        (*copy).length = length;
        let rv = match read(copy) {
            Ok(data) => {
                each(data);
                SUCCESS
            }
            Err(rv) => rv,
        };
        destroy(copy);
        rv
    }
}

extern "C" {
    fn pread(fd: core::ffi::c_int, buf: *mut c_void, count: usize, offset: i64) -> isize;
}

/// Reads `length` bytes of file bucket `e` from `at` on into request pool memory, with a plain
/// `pread` on its descriptor. Reading the bucket through APR maps the file instead, with
/// `EnableMMAP On`: an mmap, a munmap and page faults per response, which is what made reading the
/// first KiB cost httpd several times what it costs nginx.
pub(crate) unsafe fn pread_bucket(
    e: *mut apr_bucket,
    at: apr_size_t,
    length: apr_size_t,
    pool: *mut apr_pool_t,
) -> Result<*mut u8, apr_status_t> {
    unsafe {
        let file = (*e).data as *mut apr_bucket_file;
        let mut fd: apr_os_file_t = 0;
        let rv = apr_os_file_get(&mut fd, (*file).fd);
        if rv != SUCCESS {
            return Err(rv);
        }
        let buf = apr_palloc(pool, length.max(1)) as *mut u8;
        let offset = (*e).start + at as apr_off_t;
        if pread(fd, buf as *mut c_void, length as usize, offset) != length as isize {
            // APR_EGENERAL
            return Err(20014);
        }
        Ok(buf)
    }
}

/// Replaces the first `length` bytes of file bucket `e` with a pool bucket holding them, read with
/// [`pread_bucket`]: the rest stays a file bucket, `e` itself moved forward, or goes if there is
/// none. Returns the new bucket, and the bytes it holds.
pub(crate) unsafe fn read_head(
    e: *mut apr_bucket,
    length: apr_size_t,
    pool: *mut apr_pool_t,
    list: *mut apr_bucket_alloc_t,
) -> Result<(*mut apr_bucket, *const u8), apr_status_t> {
    unsafe {
        let buf = pread_bucket(e, 0, length, pool)?;
        let m = apr_bucket_pool_create(buf as *const c_char, length, pool, list);
        insert_before(e, m);
        if length == (*e).length {
            remove(e);
            destroy(e);
        } else {
            (*e).start += length as apr_off_t;
            (*e).length -= length;
        }
        Ok((m, buf))
    }
}

/// Files up to this size are read whole and sent from memory, see the module documentation.
fn memory_below() -> apr_size_t {
    static BELOW: std::sync::OnceLock<apr_size_t> = std::sync::OnceLock::new();
    *BELOW.get_or_init(|| {
        std::env::var("RUM_INJECT_MEMORY_BELOW").ok().and_then(|v| v.parse().ok()).unwrap_or(8 << 10)
    })
}

/// While holding: feeds the scanner held file bucket `e`, read into a pool bucket that takes its
/// place: the whole file if it is small, otherwise only its first piece, the rest of `e` going back
/// to the head of `bb`, the next bucket the holding loop takes. Returns the decision and that
/// bucket.
unsafe fn feed_held_file(
    f: *mut ap_filter_t,
    ctx: &mut ScanCtx,
    e: *mut apr_bucket,
    bb: *mut apr_bucket_brigade,
) -> Result<(Decision, *mut apr_bucket), apr_status_t> {
    unsafe {
        let piece = match (*e).length <= memory_below() {
            true => (*e).length,
            false if ctx.scanner.fed() == 0 => FIRST_READ,
            false => FILE_READ_CHUNK,
        }
        .min((*e).length);
        let whole = piece == (*e).length;
        let (m, buf) = read_head(e, piece, (*(*f).r).pool, (*(*f).c).bucket_alloc)?;
        if !whole {
            remove(e);
            insert_before(first(bb), e);
        }
        let data = slice::from_raw_parts(buf, piece as usize);
        Ok((ctx.scanner.feed(data), m))
    }
}

/// Feeds the scanner `e`'s data from `*fed` bytes in, until it decides or `e` runs out.
unsafe fn feed(ctx: &mut ScanCtx, e: *mut apr_bucket, fed: &mut apr_size_t) -> Result<Decision, apr_status_t> {
    unsafe {
        if !is_file(e) {
            if *fed > 0 {
                return Ok(ctx.scanner.decision());
            }
            let data = read(e)?;
            *fed = data.len() as apr_size_t;
            return Ok(ctx.scanner.feed(data));
        }
        let mut decision = ctx.scanner.decision();
        while matches!(decision, Decision::Hold { .. } | Decision::Searching) && *fed < (*e).length {
            let piece = if ctx.scanner.fed() == 0 { FIRST_READ } else { FILE_READ_CHUNK };
            let size = piece.min((*e).length - *fed);
            let scanner = &mut ctx.scanner;
            let rv = read_file_piece(e, *fed, size, &mut |data| decision = scanner.feed(data));
            if rv != SUCCESS {
                return Err(rv);
            }
            *fed += size;
        }
        Ok(decision)
    }
}

unsafe fn snippet_bucket(list: *mut apr_bucket_alloc_t) -> *mut apr_bucket {
    let snippet = bench_common::snippet();
    unsafe { apr_bucket_immortal_create(snippet.as_ptr() as *const c_char, snippet.len() as apr_size_t, list) }
}

/// Splits data bucket `e` at `at` bytes in, and links `insert` in between.
unsafe fn splice(e: *mut apr_bucket, at: apr_size_t, insert: *mut apr_bucket) -> apr_status_t {
    unsafe {
        if at == 0 {
            insert_before(e, insert);
            return SUCCESS;
        }
        if at < (*e).length {
            let rv = split(e, at);
            if rv != SUCCESS {
                return rv;
            }
        }
        insert_before(next(e), insert);
        SUCCESS
    }
}

/// Sets the length the headers announce for `decision`, and splices the snippet into the held
/// buckets at its offset.
unsafe fn commit(f: *mut ap_filter_t, ctx: &mut ScanCtx, decision: Decision) -> apr_status_t {
    unsafe {
        let r = (*f).r;
        let snippet = bench_common::snippet().len() as u64;
        match (decision, ctx.length) {
            (Decision::InjectAt(_), Some(length)) => ap_set_content_length(r, (length + snippet) as apr_off_t),
            (Decision::Searching, Some(length)) => {
                ap_set_content_length(r, (length + snippet) as apr_off_t);
                ctx.padded = true;
            }
            _ => {}
        }
        ctx.state = match decision {
            Decision::Searching => State::Streaming,
            _ => State::Done,
        };
        let Decision::InjectAt(point) = decision else {
            return SUCCESS;
        };
        let held = ctx.held;
        let mut start = 0u64;
        let mut e = first(held);
        while e != sentinel(held) {
            if !is_metadata(e) {
                let length = (*e).length as u64;
                if point <= start + length {
                    ctx.injected = true;
                    return splice(e, (point - start) as apr_size_t, snippet_bucket((*(*f).c).bucket_alloc));
                }
                start += length;
            }
            e = next(e);
        }
        // Past every data bucket: at the end, before the EOS if there is one.
        ctx.injected = true;
        let snippet = snippet_bucket((*(*f).c).bucket_alloc);
        let mut e = first(held);
        while e != sentinel(held) && !is_eos(e) {
            e = next(e);
        }
        insert_before(e, snippet);
        SUCCESS
    }
}

/// Starts the filter for this response, or returns None when it is not eligible.
unsafe fn start(f: *mut ap_filter_t, mode: u8) -> Option<*mut ScanCtx> {
    unsafe {
        let r = (*f).r;
        if !(*r).main.is_null() || (*r).status != 200 || (*r).content_type.is_null() {
            return None;
        }
        let content_type = core::ffi::CStr::from_ptr((*r).content_type).to_bytes();
        if content_type.len() < 9 || !content_type[..9].eq_ignore_ascii_case(b"text/html") {
            return None;
        }
        if header((*r).headers_out, c"Content-Encoding").is_some_and(|v| !v.is_empty())
            || !(*r).content_encoding.is_null()
        {
            return None;
        }
        let length = header((*r).headers_out, c"Content-Length").and_then(|value| content_length_plus(value, 0));
        if length.is_none() {
            apr_table_unset((*r).headers_out, c"Content-Length".as_ptr());
        }
        if let (MODE_SPLIT_SNIPPET, Some(length)) = (mode, length) {
            ap_set_content_length(r, (length + bench_common::snippet().len() as u64) as apr_off_t);
        }
        apr_table_unset((*r).headers_out, c"ETag".as_ptr());
        if (*r).header_only != 0 {
            return None;
        }
        let ctx = Box::into_raw(Box::new(ScanCtx {
            scanner: Scanner::new(),
            state: if mode == MODE_SCAN { State::Holding } else { State::Streaming },
            length,
            padded: false,
            injected: false,
            held: apr_brigade_create((*r).pool, (*(*f).c).bucket_alloc),
            offset: 0,
        }));
        apr_pool_cleanup_register((*r).pool, ctx as *const c_void, Some(drop_ctx), Some(apr_pool_cleanup_null));
        Some(ctx)
    }
}

/// The ablations, on the first data bucket.
unsafe fn ablate(f: *mut ap_filter_t, mode: u8, e: *mut apr_bucket) -> apr_status_t {
    unsafe {
        let at = SPLIT_AT.min((*e).length);
        let pool = (*(*f).r).pool;
        if mode == MODE_READ {
            if is_file(e) {
                return pread_bucket(e, 0, at, pool).map(|buf| {
                    core::hint::black_box(buf);
                }).err().unwrap_or(SUCCESS);
            }
            return read(e).err().unwrap_or(SUCCESS);
        }
        if is_file(e) {
            // The first KiB from memory, the rest by sendfile.
            let m = match read_head(e, at, pool, (*(*f).c).bucket_alloc) {
                Ok((m, _)) => m,
                Err(rv) => return rv,
            };
            if mode == MODE_SPLIT_SNIPPET {
                insert_before(next(m), snippet_bucket((*(*f).c).bucket_alloc));
            }
            return SUCCESS;
        }
        if at < (*e).length {
            let rv = split(e, at);
            if rv != SUCCESS {
                return rv;
            }
        }
        // The first KiB from memory: reading a file bucket turns it into one.
        if let Err(rv) = read(e) {
            return rv;
        }
        if mode == MODE_SPLIT_SNIPPET {
            insert_before(next(e), snippet_bucket((*(*f).c).bucket_alloc));
        }
        SUCCESS
    }
}

pub(crate) unsafe extern "C" fn filter<const MODE: u8>(
    f: *mut ap_filter_t,
    bb: *mut apr_bucket_brigade,
) -> apr_status_t {
    unsafe {
        if (*f).ctx.is_null() {
            match start(f, MODE) {
                Some(ctx) => (*f).ctx = ctx as *mut c_void,
                None => {
                    ap_remove_output_filter(f);
                    return ap_pass_brigade((*f).next, bb);
                }
            }
        }
        let ctx = &mut *((*f).ctx as *mut ScanCtx);
        let list = (*(*f).c).bucket_alloc;

        // Holding: buckets move to `held` until the decision.
        let mut resume: Option<apr_size_t> = None;
        while ctx.state == State::Holding && first(bb) != sentinel(bb) {
            let mut e = first(bb);
            remove(e);
            insert_before(sentinel(ctx.held), e);
            let mut decision = Decision::Hold { from: 0 };
            let mut fed = 0;
            if is_eos(e) {
                decision = ctx.scanner.finish();
            } else if is_file(e) {
                let m;
                (decision, m) = match feed_held_file(f, ctx, e, bb) {
                    Ok(fed) => fed,
                    Err(rv) => return rv,
                };
                // The pool bucket is the one held now, fed whole.
                e = m;
                fed = (*e).length;
                if matches!(decision, Decision::Hold { .. }) {
                    ctx.offset += (*e).length as u64;
                }
            } else if !is_metadata(e) {
                decision = match feed(ctx, e, &mut fed) {
                    Ok(decision) => decision,
                    Err(rv) => return rv,
                };
                if matches!(decision, Decision::Hold { .. }) {
                    ctx.offset += (*e).length as u64;
                }
            }
            match decision {
                Decision::Hold { .. } => {}
                Decision::Searching => {
                    // The `<head>` may be further on in this bucket: it is streamed instead.
                    remove(e);
                    insert_before(first(bb), e);
                    resume = Some(fed);
                    let rv = commit(f, ctx, decision);
                    if rv != SUCCESS {
                        return rv;
                    }
                }
                _ => {
                    if !is_metadata(e) {
                        ctx.offset += (*e).length as u64;
                    }
                    let rv = commit(f, ctx, decision);
                    if rv != SUCCESS {
                        return rv;
                    }
                }
            }
        }
        if ctx.state == State::Holding {
            // Kept past this call: whatever lives in the caller's memory is copied now.
            let mut e = first(ctx.held);
            while e != sentinel(ctx.held) {
                let rv = ((*(*e).type_).setaside.unwrap())(e, (*(*f).r).pool);
                if rv != SUCCESS {
                    return rv;
                }
                e = next(e);
            }
            return SUCCESS;
        }
        if first(ctx.held) != sentinel(ctx.held) {
            let rv = ap_pass_brigade((*f).next, ctx.held);
            if rv != SUCCESS {
                return rv;
            }
        }

        let mut e = first(bb);
        while e != sentinel(bb) && ctx.state != State::Done {
            let following = next(e);
            if is_eos(e) {
                break;
            }
            if is_metadata(e) {
                e = following;
                continue;
            }
            if MODE != MODE_SCAN {
                ctx.state = State::Done;
                let rv = ablate(f, MODE, e);
                if rv != SUCCESS {
                    return rv;
                }
                break;
            }
            let start = ctx.offset;
            let mut fed = resume.take().unwrap_or(0);
            let decision = match feed(ctx, e, &mut fed) {
                Ok(decision) => decision,
                Err(rv) => return rv,
            };
            ctx.offset += (*e).length as u64;
            match decision {
                Decision::InjectAt(point) => {
                    ctx.state = State::Done;
                    ctx.injected = true;
                    let rv = splice(e, (point - start) as apr_size_t, snippet_bucket(list));
                    if rv != SUCCESS {
                        return rv;
                    }
                }
                Decision::Never => ctx.state = State::Done,
                _ => {}
            }
            e = following;
        }

        if ctx.padded && !ctx.injected {
            // Owed at the end of stream, before the EOS.
            let mut e = first(bb);
            while e != sentinel(bb) && !is_eos(e) {
                e = next(e);
            }
            if e != sentinel(bb) {
                let length = bench_common::snippet().len();
                let spaces = vec![b' '; length];
                insert_before(e, apr_bucket_heap_create(spaces.as_ptr() as *const c_char, length as apr_size_t, None, list));
                ctx.padded = false;
                ap_remove_output_filter(f);
            }
        } else if ctx.state == State::Done {
            ap_remove_output_filter(f);
        }
        ap_pass_brigade((*f).next, bb)
    }
}
