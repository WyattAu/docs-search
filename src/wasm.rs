//! Browser-facing reader ABI (0.3.0): query the FST binary index
//! (`search.bin` emitted by docs-compiler) from JavaScript — the
//! in-browser half of the search story. The JSON corpus stays the
//! fallback and the metadata source (titles/urls); this reader answers
//! `doc` ids + BM25 scores, which the JS side maps through the corpus
//! array (doc id = corpus array index, both follow the site's route
//! order).
//!
//! Protocol:
//! - `dsidx_alloc(len) -> ptr` / `dsidx_free(ptr, len)` — host-side
//!   byte buffers (index bytes, query strings, result out-buffer).
//! - `dsidx_open(ptr, len) -> handle` — parse an index (0 = failure).
//! - `dsidx_query(handle, qptr, qlen, limit, out_ptr, out_cap) ->
//!   bytes_written` — BM25 query; the JSON results
//!   (`[{"doc":N,"score":S},...]`) are written into the host's
//!   out-buffer (truncated to fit).
//! - `dsidx_close(handle)` — free the index.
//!
//! This module compiles only for wasm32 and is the crate's one unsafe
//! surface (raw pointers across the wasm boundary); the host side is
//! `deny(unsafe_code)` everywhere else. Memory: exact-length boxed
//! slices, so `dsidx_free` reconstructs with (ptr, len) alone.
#![allow(unsafe_code)]

use crate::Index;
use std::cell::RefCell;

thread_local! {
    /// Handle -> index registry (1-based from the JS side; slot 0 unused).
    static REGISTRY: RefCell<Vec<Option<Index>>> = const { RefCell::new(Vec::new()) };
}

/// Allocate exactly `len` bytes for the host to write index bytes into.
/// Returns the pointer as u32; free with `dsidx_free(ptr, len)`.
#[no_mangle]
pub extern "C" fn dsidx_alloc(len: u32) -> u32 {
    // A boxed slice has capacity == len, so `dsidx_free` can
    // reconstruct the allocation from (ptr, len) alone.
    let slice = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(slice) as *mut u8 as u32
}

/// Free an allocation made by `dsidx_alloc`.
///
/// # Safety
/// `ptr` must be a live `dsidx_alloc` result and `len` its original
/// length; the pointer must not be used after this call.
#[no_mangle]
pub unsafe extern "C" fn dsidx_free(ptr: u32, len: u32) {
    if ptr == 0 {
        return;
    }
    // SAFETY: caller contract above; the boxed slice round-trip uses
    // the exact allocation length.
    let slice = std::slice::from_raw_parts_mut(ptr as *mut u8, len as usize);
    drop(Box::from_raw(slice));
}

/// Parse an index from `len` bytes at `ptr`. Returns a handle (> 0) or
/// 0 on failure (malformed bytes).
///
/// # Safety
/// `ptr` must point to `len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn dsidx_open(ptr: u32, len: u32) -> u32 {
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize);
    let owned = bytes.to_vec();
    match Index::from_bytes(&owned) {
        Ok(index) => REGISTRY.with(|r| {
            let mut registry = r.borrow_mut();
            let handle = registry.len() as u32 + 1;
            registry.push(Some(index));
            handle
        }),
        Err(_) => 0,
    }
}

/// Close a handle and free its index.
///
/// # Safety
/// `handle` must be a live `dsidx_open` result and must not be used
/// after this call.
#[no_mangle]
pub unsafe extern "C" fn dsidx_close(handle: u32) {
    REGISTRY.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = (handle as usize)
            .checked_sub(1)
            .and_then(|i| registry.get_mut(i));
        if let Some(slot) = slot {
            *slot = None;
        }
    });
}

/// Run a BM25 query against an open index: `qlen` UTF-8 bytes at
/// `qptr`, top `limit` hits. The JSON results
/// (`[{"doc":N,"score":S},...]`) are written into the host-provided
/// buffer at `out_ptr` (`out_cap` bytes) and the number of bytes
/// written is returned. 0 = failure (bad handle, non-UTF-8 query, or
/// buffer too small — grow it and retry).
///
/// # Safety
/// `qptr`/`out_ptr` must point to `qlen`/`out_cap` readable/writable
/// bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn dsidx_query(
    handle: u32,
    qptr: u32,
    qlen: u32,
    limit: u32,
    out_ptr: u32,
    out_cap: u32,
) -> u32 {
    let query_bytes = std::slice::from_raw_parts(qptr as *const u8, qlen as usize);
    let query = match std::str::from_utf8(query_bytes) {
        Ok(q) => q.to_string(),
        Err(_) => return 3,
    };
    REGISTRY.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = (handle as usize)
            .checked_sub(1)
            .and_then(|i| registry.get_mut(i));
        let Some(index_slot) = slot else { return 2 };
        let Some(index) = index_slot.as_ref() else {
            return 2;
        };
        let hits = index.query(&query, limit.max(1) as usize);
        let mut json = String::from("[");
        for (n, hit) in hits.iter().enumerate() {
            if n > 0 {
                json.push(',');
            }
            json.push_str(&format!("{{\"doc\":{},\"score\":{}}}", hit.doc, hit.score));
        }
        json.push(']');
        let out = std::slice::from_raw_parts_mut(out_ptr as *mut u8, out_cap as usize);
        let n = json.len().min(out.len());
        out[..n].copy_from_slice(&json.as_bytes()[..n]);
        n as u32
    })
}
