//! The portable Flux core as a **WebAssembly module** (C-271).
//!
//! Build it with `scripts/build-portable-wasm.sh`; the artifact lands at
//! `target/wasm32-unknown-unknown/release/examples/flux_portable.wasm`.
//!
//! # The ABI
//!
//! Hand-written and deliberately tiny — three exports, **zero imports**:
//!
//! | export | signature | meaning |
//! |---|---|---|
//! | `flux_alloc` | `(len: i32) -> i32` | reserve `len` bytes in the module's linear memory |
//! | `flux_dealloc` | `(ptr: i32, len: i32)` | release a buffer obtained from `flux_alloc` |
//! | `flux_eval` | `(ptr: i32, len: i32) -> i64` | evaluate the UTF-8 `.flux` source at `ptr..ptr+len`; returns `(out_ptr << 32) \| out_len` for a UTF-8 JSON result the caller must `flux_dealloc` |
//!
//! Zero imports is the headline, not an implementation detail: it is the machine-checkable form of
//! "a model-free program needs no ambient authority". An embedder instantiates this module with an
//! empty import table, so there is no clock, no filesystem, no socket and no model to reach — the
//! guarantee holds by construction rather than by policy. C-272 adds the first *narrow, already
//! guarded* imports on top of this baseline; every one of them will be visible in the module's
//! import section, which is exactly the review surface the epic wants.

#![cfg(target_family = "wasm")]

#[path = "core.rs"]
mod core_impl;

/// Reserve `len` bytes of linear memory and hand the embedder the pointer.
///
/// # Safety
/// The returned buffer must be released with [`flux_dealloc`] using the same `len`.
#[no_mangle]
pub extern "C" fn flux_alloc(len: i32) -> i32 {
    // A boxed slice, so the allocation's capacity is exactly `len` and `flux_dealloc` can rebuild
    // it from `(ptr, len)` alone — a `Vec::with_capacity` may over-allocate, and reconstructing it
    // with the wrong capacity is undefined behaviour.
    let buf = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(buf) as *mut u8 as i32
}

/// Release a buffer previously returned by [`flux_alloc`] or [`flux_eval`].
///
/// # Safety
/// `ptr`/`len` must be exactly a pair this module handed out and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn flux_dealloc(ptr: i32, len: i32) {
    if ptr != 0 && len > 0 {
        let slice = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(slice));
    }
}

/// Evaluate the UTF-8 `.flux` source at `ptr..ptr + len` and return the canonical JSON result,
/// packed as `(out_ptr << 32) | out_len`.
///
/// # Safety
/// `ptr`/`len` must describe an initialized buffer inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn flux_eval(ptr: i32, len: i32) -> i64 {
    let src_bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize);
    let json = match std::str::from_utf8(src_bytes) {
        Ok(src) => core_impl::eval_to_json(src),
        Err(_) => r#"{"ok":false,"error":"source is not valid UTF-8"}"#.to_string(),
    };
    let bytes = json.into_bytes().into_boxed_slice();
    let out_len = bytes.len() as i64;
    let out_ptr = Box::into_raw(bytes) as *mut u8 as i64;
    (out_ptr << 32) | out_len
}
