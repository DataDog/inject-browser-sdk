//! The offset-based integration (`rum_inject_mode scan`) and the ablations that locate what the
//! remaining cost is (`read`, `split`, `split_snippet`).
//!
//! `scan`: the library is asked for a decision, a body offset, rather than for the bytes:
//! * the header filter does not call the next one: the headers wait for the decision, and then
//!   announce the exact `Content-Length`, the snippet length more only if it goes in;
//! * the held bytes are nginx's own buffers, but for files: a file no larger than
//!   `RUM_INJECT_MEMORY_BELOW` bytes (environment, default 16 KiB) is read whole, in one read, and
//!   goes out from memory around the snippet, in one write. A larger one is read only until the
//!   decision (usually the first 1 KiB), and what was read goes out from memory, the snippet after
//!   it, the rest of the file by sendfile. Sending both sides of the snippet by sendfile costs a
//!   syscall and a packet more than that, which is what made the first version of this variant
//!   dearer than the injector. A memory buffer from an upstream is copied only if a call ends
//!   undecided, so that the upstream gets its buffer back (at most 10 KiB);
//! * the snippet is a buffer pointing at the static snippet, linked in where the buffer holding the
//!   offset is split;
//! * early commit: a decision that stays open past the head window (`Searching`, the `<head>` past
//!   it) sends the headers with the snippet length announced, and the body is padded with spaces if
//!   the snippet never goes in. An upstream flush does not commit: nginx flags every buffer of an
//!   unbuffered upstream as one, so it says nothing of an application that wants its data out, and
//!   at most 10 KiB is held anyway.
//!
//! Ablations, no library call at all:
//! * `read`: read the first KiB of a file, send the response unchanged;
//! * `split`: send the first KiB from memory, the rest of the file by sendfile;
//! * `split_snippet`: `split`, with the snippet buffer linked in after the first KiB.

use core::ffi::c_void;
use core::{ptr, slice};

use inject_browser_sdk::scanner::{Decision, Scanner};
use ngx::core::Pool;
use ngx::ffi::{
    ngx_buf_t, ngx_chain_t, ngx_chain_update_chains, ngx_create_temp_buf,
    ngx_http_request_t, ngx_int_t, ngx_pool_t, ngx_read_file, ngx_uint_t,
};

use crate::{calloc_buf, link, module_index, special, tag, NEXT_BODY_FILTER, NEXT_HEADER_FILTER};

pub(crate) const MODE_INJECTOR: ngx_uint_t = 0;
pub(crate) const MODE_SCAN: ngx_uint_t = 1;
pub(crate) const MODE_READ: ngx_uint_t = 2;
pub(crate) const MODE_SPLIT: ngx_uint_t = 3;
pub(crate) const MODE_SPLIT_SNIPPET: ngx_uint_t = 4;

const NGX_OK: ngx_int_t = 0;
const NGX_ERROR: ngx_int_t = -1;
/// `NGX_HTTP_IMAGE_BUFFERED`: the image filter is not loaded, so its bit says this filter holds
/// data, which keeps the request from finalizing while the decision is pending.
const BUFFERED: u32 = 0x08;
/// How much of a file is read per scanner feed. The scanner asks for 1 KiB first, then doubles.
const FIRST_READ: usize = 1024;
const FILE_READ_CHUNK: usize = 4096;
/// The ablations' "first KiB".
const SPLIT_AT: usize = 1024;

#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    /// The headers wait, and the buffers in `held` with them.
    Holding,
    /// The headers are out, announcing the snippet: still looking for the `<head>` tag.
    Streaming,
    /// Nothing left to do but pass buffers on, and the padding at the end if it is owed.
    Done,
}

/// Per-request state, allocated in the request pool (dropped by a pool cleanup).
pub(crate) struct ScanCtx {
    mode: ngx_uint_t,
    scanner: Scanner,
    state: State,
    /// The `Content-Length` the response came with, -1 when unknown.
    length: i64,
    /// Whether the announced length counts the snippet before it is in: padding is owed then.
    padded: bool,
    injected: bool,
    /// The held buffers, in a chain of our own, and its next link pointer.
    held: *mut ngx_chain_t,
    held_ll: *mut *mut ngx_chain_t,
    /// Body offset of the first byte of the next buffer to come.
    offset: u64,
    /// How much of the last held buffer the scanner was fed, when it is a file buffer.
    file_fed: i64,
    /// Where file pieces are read into for the scanner past the commit, reused.
    scratch: *mut ngx_buf_t,
    /// While holding: what was read of the file buffer `read_for`, from its `file_pos` on, to go out
    /// from memory. `read_whole` when that is all of it.
    read: *mut ngx_buf_t,
    read_for: *mut ngx_buf_t,
    read_whole: bool,
    free: *mut ngx_chain_t,
    busy: *mut ngx_chain_t,
}

unsafe fn buf_in_memory(b: *const ngx_buf_t) -> bool {
    unsafe { (*b).temporary() != 0 || (*b).memory() != 0 || (*b).mmap() != 0 }
}

/// The size of `b`'s data, in memory or in its file.
unsafe fn buf_size(b: *const ngx_buf_t) -> u64 {
    unsafe {
        if buf_in_memory(b) {
            (*b).last.offset_from((*b).pos) as u64
        } else if (*b).in_file() != 0 {
            ((*b).file_last - (*b).file_pos) as u64
        } else {
            0
        }
    }
}

/// Links `b` unless it is empty and carries no flag: the writer refuses a zero size buffer.
unsafe fn link_data(pool: *mut ngx_pool_t, ll: &mut *mut *mut ngx_chain_t, b: *mut ngx_buf_t) -> bool {
    unsafe {
        if buf_size(b) == 0 && (*b).last_buf() == 0 && (*b).flush() == 0 && (*b).sync() == 0 {
            return true;
        }
        link(pool, ll, b)
    }
}

/// A buffer pointing at the static snippet.
unsafe fn snippet_buf(pool: *mut ngx_pool_t) -> *mut ngx_buf_t {
    unsafe {
        let snippet = bench_common::snippet();
        let b = calloc_buf(pool);
        if b.is_null() {
            return b;
        }
        (*b).set_memory(1);
        (*b).start = snippet.as_ptr() as *mut u8;
        (*b).pos = (*b).start;
        (*b).last = (*b).start.add(snippet.len());
        (*b).end = (*b).last;
        (*b).tag = tag();
        b
    }
}

/// Links `b` split at `at` bytes into it, with `insert` (if not null) in between. The first part is
/// a new buffer pointing into the same memory or file; the second is `b` itself, moved forward, so
/// that whoever owns `b` gets it back only once both are sent. Split at its end, `b` goes whole.
unsafe fn link_split(
    pool: *mut ngx_pool_t,
    ll: &mut *mut *mut ngx_chain_t,
    b: *mut ngx_buf_t,
    at: u64,
    insert: *mut ngx_buf_t,
) -> bool {
    unsafe {
        // `b` always goes out after whatever points into it: linked whole when the split is at its
        // end, rather than left empty, which would hand it back before the part ahead of it is sent.
        if at > 0 && at == buf_size(b) {
            if !link(pool, ll, b) {
                return false;
            }
            return insert.is_null() || link(pool, ll, insert);
        }
        if at > 0 {
            let first = calloc_buf(pool);
            if first.is_null() {
                return false;
            }
            if buf_in_memory(b) {
                (*first).set_memory(1);
                (*first).start = (*b).pos;
                (*first).pos = (*b).pos;
                (*first).last = (*b).pos.add(at as usize);
                (*first).end = (*first).last;
            }
            if (*b).in_file() != 0 {
                (*first).set_in_file(1);
                (*first).file = (*b).file;
                (*first).file_pos = (*b).file_pos;
                (*first).file_last = (*b).file_pos + at as i64;
            }
            (*first).tag = tag();
            if !link(pool, ll, first) {
                return false;
            }
            if buf_in_memory(b) {
                (*b).pos = (*b).pos.add(at as usize);
            }
            if (*b).in_file() != 0 {
                (*b).file_pos += at as i64;
            }
        }
        if !insert.is_null() && !link(pool, ll, insert) {
            return false;
        }
        link_data(pool, ll, b)
    }
}

pub(crate) unsafe fn create(r: *mut ngx_http_request_t, mode: ngx_uint_t, length: i64) -> bool {
    unsafe {
        let req = &mut *r;
        let pool = Pool::from_ngx_pool(req.pool);
        let ctx = pool.allocate(ScanCtx {
            mode,
            scanner: Scanner::new(),
            state: State::Holding,
            length,
            padded: false,
            injected: false,
            held: ptr::null_mut(),
            held_ll: ptr::null_mut(),
            offset: 0,
            file_fed: 0,
            scratch: ptr::null_mut(),
            read: ptr::null_mut(),
            read_for: ptr::null_mut(),
            read_whole: false,
            free: ptr::null_mut(),
            busy: ptr::null_mut(),
        });
        if ctx.is_null() {
            return false;
        }
        (*ctx).held_ll = &raw mut (*ctx).held;
        if mode != MODE_SCAN {
            (*ctx).state = State::Streaming;
        }
        *req.ctx.add(module_index()) = ctx as *mut c_void;
        true
    }
}

unsafe fn get(r: *mut ngx_http_request_t) -> *mut ScanCtx {
    unsafe { *(*r).ctx.add(module_index()) as *mut ScanCtx }
}

/// Feeds the scanner the data of `b` from `ctx.file_fed` on, until it decides or `b` runs out.
unsafe fn feed(pool: *mut ngx_pool_t, ctx: &mut ScanCtx, b: *mut ngx_buf_t) -> Option<Decision> {
    unsafe {
        if buf_in_memory(b) {
            let size = (*b).last.offset_from((*b).pos) as usize;
            return Some(ctx.scanner.feed(slice::from_raw_parts((*b).pos, size)));
        }
        if (*b).in_file() == 0 {
            return Some(ctx.scanner.decision());
        }
        if ctx.state == State::Holding {
            return feed_held_file(pool, ctx, b);
        }
        if ctx.scratch.is_null() {
            ctx.scratch = ngx_create_temp_buf(pool, FILE_READ_CHUNK);
            if ctx.scratch.is_null() {
                return None;
            }
        }
        let mut decision = ctx.scanner.decision();
        while matches!(decision, Decision::Hold { .. } | Decision::Searching)
            && (*b).file_pos + ctx.file_fed < (*b).file_last
        {
            let piece = if ctx.scanner.fed() == 0 { FIRST_READ } else { FILE_READ_CHUNK };
            let size = piece.min(((*b).file_last - (*b).file_pos - ctx.file_fed) as usize);
            let at = (*b).file_pos + ctx.file_fed;
            if ngx_read_file((*b).file, (*ctx.scratch).start, size, at) != size as isize {
                return None;
            }
            ctx.file_fed += size as i64;
            decision = ctx.scanner.feed(slice::from_raw_parts((*ctx.scratch).start, size));
        }
        Some(decision)
    }
}

/// Files up to this size are read whole and sent from memory, see the module documentation. Read
/// first by the master, in `postconfiguration`: nginx strips the environment of its workers.
pub(crate) fn memory_below() -> i64 {
    static BELOW: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *BELOW.get_or_init(|| {
        std::env::var("RUM_INJECT_MEMORY_BELOW").ok().and_then(|v| v.parse().ok()).unwrap_or(16 << 10)
    })
}

/// The most the scanner can ask for before it decides: the walk's limit.
const WALK_LIMIT: usize = 10 << 10;

/// Feeds the scanner file buffer `b` while holding, keeping what it reads in `ctx.read` to send it
/// from memory: the whole file when it is small, see [`memory_below`], otherwise only what the
/// decision needs.
unsafe fn feed_held_file(pool: *mut ngx_pool_t, ctx: &mut ScanCtx, b: *mut ngx_buf_t) -> Option<Decision> {
    unsafe {
        let size = ((*b).file_last - (*b).file_pos) as usize;
        if ctx.read_for != b {
            ctx.read_whole = size as i64 <= memory_below();
            let capacity = if ctx.read_whole { size } else { size.min(WALK_LIMIT) };
            ctx.read = ngx_create_temp_buf(pool, capacity.max(1));
            if ctx.read.is_null() {
                return None;
            }
            ctx.read_for = b;
        }
        let m = ctx.read;
        let mut decision = ctx.scanner.decision();
        while matches!(decision, Decision::Hold { .. }) && (ctx.file_fed as usize) < size {
            let room = (*m).end.offset_from((*m).last) as usize;
            let left = size - ctx.file_fed as usize;
            let piece = match ctx.read_whole {
                true => left,
                false if ctx.scanner.fed() == 0 => FIRST_READ,
                false => FILE_READ_CHUNK,
            }
            .min(left)
            .min(room);
            if piece == 0 {
                break;
            }
            let at = (*b).file_pos + ctx.file_fed;
            if ngx_read_file((*b).file, (*m).last, piece, at) != piece as isize {
                return None;
            }
            decision = ctx.scanner.feed(slice::from_raw_parts((*m).last, piece));
            (*m).last = (*m).last.add(piece);
            ctx.file_fed += piece as i64;
        }
        Some(decision)
    }
}

/// Links held file buffer `b`, which `ctx.read` holds the start of in memory, with the snippet in
/// `at` bytes into it: from memory up to where it was read, the rest by sendfile.
unsafe fn link_read_file(
    pool: *mut ngx_pool_t,
    ctx: &mut ScanCtx,
    ll: &mut *mut *mut ngx_chain_t,
    b: *mut ngx_buf_t,
    at: u64,
) -> bool {
    unsafe {
        let m = ctx.read;
        let read = (*m).last.offset_from((*m).pos) as i64;
        (*m).tag = tag();
        let snippet = snippet_buf(pool);
        if snippet.is_null() || !link_split(pool, ll, m, at, snippet) {
            return false;
        }
        (*b).file_pos += read;
        if buf_size(b) == 0 {
            // Read whole: the memory takes over the end of the response from it.
            if (*b).last_buf() != 0 {
                return special(pool, ll, true);
            }
            return true;
        }
        link(pool, ll, b)
    }
}

/// Copies the held memory buffers that are not ours, and hands the originals back: whoever owns
/// them, an upstream with a single buffer for instance, may need them before the decision comes.
unsafe fn own_held(pool: *mut ngx_pool_t, ctx: &mut ScanCtx) -> bool {
    unsafe {
        let mut cl = ctx.held;
        while !cl.is_null() {
            let b = (*cl).buf;
            if buf_in_memory(b) && (*b).tag != tag() {
                let size = (*b).last.offset_from((*b).pos) as usize;
                let copy = ngx_create_temp_buf(pool, size.max(1));
                if copy.is_null() {
                    return false;
                }
                ptr::copy_nonoverlapping((*b).pos, (*copy).pos, size);
                (*copy).last = (*copy).pos.add(size);
                (*copy).set_flush((*b).flush());
                (*copy).set_last_buf((*b).last_buf());
                (*copy).tag = tag();
                (*b).pos = (*b).last;
                if (*b).in_file() != 0 {
                    (*b).file_pos = (*b).file_last;
                }
                (*cl).buf = copy;
            }
            cl = (*cl).next;
        }
        true
    }
}

/// Sends the headers for `decision`, then the held buffers, the snippet spliced in at its offset.
unsafe fn commit(
    r: *mut ngx_http_request_t,
    ctx: &mut ScanCtx,
    decision: Decision,
    ll: &mut *mut *mut ngx_chain_t,
) -> ngx_int_t {
    unsafe {
        let pool = (*r).pool;
        (*(*r).connection).set_buffered((*(*r).connection).buffered() & !BUFFERED);
        let snippet = bench_common::snippet().len() as i64;
        let out = &mut (*r).headers_out;
        match decision {
            Decision::InjectAt(_) if ctx.length >= 0 => out.content_length_n = ctx.length + snippet,
            Decision::Hold { .. } | Decision::Searching if ctx.length >= 0 => {
                out.content_length_n = ctx.length + snippet;
                ctx.padded = true;
            }
            _ => {}
        }
        let rc = NEXT_HEADER_FILTER.expect("next header filter")(r);
        if rc == NGX_ERROR || rc > NGX_OK || (*r).header_only() != 0 {
            return if rc > NGX_OK { NGX_ERROR } else { rc };
        }

        let mut start = 0u64;
        let mut cl = ctx.held;
        ctx.held = ptr::null_mut();
        ctx.held_ll = &raw mut ctx.held;
        while !cl.is_null() {
            let b = (*cl).buf;
            cl = (*cl).next;
            let size = buf_size(b);
            match decision {
                Decision::InjectAt(point) if !ctx.injected && point <= start + size => {
                    ctx.injected = true;
                    if b == ctx.read_for {
                        if !link_read_file(pool, ctx, ll, b, point - start) {
                            return NGX_ERROR;
                        }
                    } else {
                        let snippet = snippet_buf(pool);
                        if snippet.is_null() || !link_split(pool, ll, b, point - start, snippet) {
                            return NGX_ERROR;
                        }
                    }
                }
                _ => {
                    if !link_data(pool, ll, b) {
                        return NGX_ERROR;
                    }
                }
            }
            start += size;
        }
        ctx.state = match decision {
            Decision::InjectAt(_) | Decision::Never => State::Done,
            _ => State::Streaming,
        };
        NGX_OK
    }
}

/// Past the commit: feeds what comes to the scanner while it still searches, splices the snippet
/// in when it decides, and pads at the end if the snippet never went in.
///
/// `resume`: `b` is the buffer the commit was decided on, which the scanner was fed part of.
unsafe fn stream(
    r: *mut ngx_http_request_t,
    ctx: &mut ScanCtx,
    b: *mut ngx_buf_t,
    ll: &mut *mut *mut ngx_chain_t,
    resume: bool,
) -> bool {
    unsafe {
        let pool = (*r).pool;
        let size = buf_size(b);
        if ctx.state == State::Streaming && ctx.mode == MODE_SCAN {
            let decision = match resume {
                // A memory buffer was fed whole; a file one up to `file_fed`.
                true if buf_in_memory(b) => Some(ctx.scanner.decision()),
                true => feed(pool, ctx, b),
                false => {
                    ctx.file_fed = 0;
                    feed(pool, ctx, b)
                }
            };
            let Some(mut decision) = decision else {
                return false;
            };
            if (*b).last_buf() != 0 && decision == Decision::Searching {
                decision = ctx.scanner.finish();
            }
            match decision {
                Decision::InjectAt(point) => {
                    ctx.state = State::Done;
                    ctx.injected = true;
                    ctx.offset += size;
                    let snippet = snippet_buf(pool);
                    return !snippet.is_null()
                        && link_split(pool, ll, b, point - (ctx.offset - size), snippet);
                }
                Decision::Never => ctx.state = State::Done,
                _ => {}
            }
        } else if ctx.state == State::Streaming {
            // An ablation, on the first buffer with data.
            if size > 0 {
                ctx.state = State::Done;
                return ablate(pool, ctx, b, ll);
            }
        }
        ctx.offset += size;
        if (*b).last_buf() != 0 && ctx.padded && !ctx.injected {
            // The padding takes over the end of the response from `b`.
            let spaces = ngx_create_temp_buf(pool, bench_common::snippet().len());
            if spaces.is_null() {
                return false;
            }
            ptr::write_bytes((*spaces).pos, b' ', bench_common::snippet().len());
            (*spaces).last = (*spaces).end;
            (*spaces).tag = tag();
            ctx.padded = false;
            if size > 0 {
                (*b).set_last_buf(0);
                (*b).set_last_in_chain(0);
                if !link(pool, ll, b) {
                    return false;
                }
            }
            return link(pool, ll, spaces) && special(pool, ll, true);
        }
        link_data(pool, ll, b)
    }
}

/// The ablations, on the first buffer with data.
unsafe fn ablate(
    pool: *mut ngx_pool_t,
    ctx: &mut ScanCtx,
    b: *mut ngx_buf_t,
    ll: &mut *mut *mut ngx_chain_t,
) -> bool {
    unsafe {
        let size = buf_size(b);
        let n = (SPLIT_AT as u64).min(size);
        let insert = match ctx.mode {
            MODE_SPLIT_SNIPPET => {
                ctx.injected = true;
                snippet_buf(pool)
            }
            _ => ptr::null_mut(),
        };
        if buf_in_memory(b) {
            ctx.offset += size;
            if ctx.mode == MODE_READ {
                return link(pool, ll, b);
            }
            return link_split(pool, ll, b, n, insert);
        }
        // A file: read the first KiB.
        let m = ngx_create_temp_buf(pool, n as usize);
        if m.is_null() || ngx_read_file((*b).file, (*m).pos, n as usize, (*b).file_pos) != n as isize {
            return false;
        }
        (*m).last = (*m).pos.add(n as usize);
        (*m).tag = tag();
        ctx.offset += size;
        if ctx.mode == MODE_READ {
            return link(pool, ll, b);
        }
        // From memory, then the rest of the file by sendfile.
        (*b).file_pos += n as i64;
        if !link(pool, ll, m) || (!insert.is_null() && !link(pool, ll, insert)) {
            return false;
        }
        if buf_size(b) > 0 {
            return link(pool, ll, b);
        }
        if (*b).last_buf() != 0 {
            return special(pool, ll, true);
        }
        true
    }
}

pub(crate) unsafe extern "C" fn body_filter(
    r: *mut ngx_http_request_t,
    input: *mut ngx_chain_t,
) -> ngx_int_t {
    unsafe {
        let next = NEXT_BODY_FILTER.expect("next body filter");
        let ctx = &mut *get(r);
        let pool = (*r).pool;
        if ctx.state == State::Done && ctx.busy.is_null() && !ctx.padded {
            return next(r, input);
        }

        let mut out: *mut ngx_chain_t = ptr::null_mut();
        let mut ll: *mut *mut ngx_chain_t = &raw mut out;
        let mut cl = input;
        while !cl.is_null() {
            let b = (*cl).buf;
            cl = (*cl).next;
            if ctx.state != State::Holding {
                if !stream(r, ctx, b, &mut ll, false) {
                    return NGX_ERROR;
                }
                continue;
            }

            // Holding: the buffer joins the held ones, and the scanner is fed its data.
            let before = ctx.held_ll;
            let mut held_ll = ctx.held_ll;
            if !link(pool, &mut held_ll, b) {
                return NGX_ERROR;
            }
            ctx.held_ll = held_ll;
            ctx.file_fed = 0;
            let Some(mut decision) = feed(pool, ctx, b) else {
                return NGX_ERROR;
            };
            if matches!(decision, Decision::Hold { .. }) && (*b).last_buf() != 0 {
                decision = ctx.scanner.finish();
            }
            match decision {
                Decision::Hold { .. } => ctx.offset += buf_size(b),
                Decision::Searching => {
                    // The `<head>` tag may be further on in this buffer, and its end may be the
                    // response's: it is streamed rather than sent with the held ones.
                    *before = ptr::null_mut();
                    ctx.held_ll = before;
                    let rc = commit(r, ctx, decision, &mut ll);
                    if rc != NGX_OK {
                        return rc;
                    }
                    if !stream(r, ctx, b, &mut ll, true) {
                        return NGX_ERROR;
                    }
                }
                _ => {
                    ctx.offset += buf_size(b);
                    let rc = commit(r, ctx, decision, &mut ll);
                    if rc != NGX_OK {
                        return rc;
                    }
                }
            }
        }

        if ctx.state == State::Holding {
            if !own_held(pool, ctx) {
                return NGX_ERROR;
            }
            (*(*r).connection).set_buffered((*(*r).connection).buffered() | BUFFERED);
            return NGX_OK;
        }
        if out.is_null() && ctx.busy.is_null() {
            return NGX_OK;
        }
        let rc = next(r, out);
        ngx_chain_update_chains(pool, &mut ctx.free, &mut ctx.busy, &mut out, tag());
        rc
    }
}
