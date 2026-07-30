//! The **portable evaluation core**: parse a `.flux` source and run it through the reference
//! interpreter with no host authority at all, returning a canonical JSON result string.
//!
//! This file is not compiled on its own. It is `#[path]`-included verbatim by **both** sides of the
//! C-271 parity proof:
//!
//! - `examples/portable/wasm_abi.rs` — the `wasm32-unknown-unknown` cdylib
//! - `tests/wasm_parity.rs` — the native half of the parity test
//!
//! Sharing one file is deliberate: a parity test whose two halves are separately written proves
//! that two *harnesses* agree, not that two *substrates* do. With one source, the only difference
//! between the two runs is the target triple.
//!
//! **Model-free by construction.** [`NoAuthority`] has an empty op catalog and a `dispatch` that
//! always denies, so a program reaching *any* op — a model call, a read, a shell — fails loudly
//! instead of silently taking a different path on one substrate. What executes here is exactly the
//! pure fragment of Flux-Lang: literals, operator formulas (`expr`), `fmt`, field access, the value
//! constructors, and control flow. See `docs/designs/portable-wasm-runtime.md`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use flux_lang::host::{ApprovalChoice, OpHost, OpOutcome};
use flux_lang::opspec::{OpCatalog, OpSignature};
use flux_lang::sink::FlowSink;
use flux_lang::store::MemStore;

/// The session id every portable evaluation runs under. Fixed, because the interpreter keys its
/// store on it and parity requires the two runs to be indistinguishable.
pub const SESSION_ID: &str = "portable-parity";

/// How many times [`block_on`] will poll before giving up. The portable core has no reactor and no
/// clock, so a future that stays `Pending` is one that wanted a host import (a timer, an awaited
/// input) — a bounded poll budget turns that into an error instead of a hang.
const MAX_POLLS: u32 = 1_000_000;

/// A host that grants **nothing**. Every op dispatch is a denial and the catalog is empty, so the
/// analyzer and the interpreter both see a world with no operations in it.
struct NoAuthority;

impl OpCatalog for NoAuthority {
    fn lookup(&self, _name: &str) -> Option<OpSignature> {
        None
    }
}

#[async_trait::async_trait]
impl OpHost for NoAuthority {
    async fn dispatch(&self, op: &str, _input: serde_json::Value) -> OpOutcome {
        OpOutcome::denial(format!(
            "the portable core has no host authority: op `{op}` is unavailable"
        ))
    }

    fn catalog(&self) -> &dyn OpCatalog {
        self
    }

    async fn request_approval(
        &self,
        _label: &str,
        _intents: &flux_spec::IntentSet,
    ) -> ApprovalChoice {
        ApprovalChoice::Deny
    }

    fn trim_output(&self, view: String, _op: &str) -> String {
        view
    }
}

/// A sink that observes nothing — every `FlowSink` method already defaults to a no-op.
struct SilentSink;
impl FlowSink for SilentSink {}

/// Drive a future to completion on the calling thread with a no-op waker.
///
/// This is the portable core's answer to "what replaces `tokio` on `wasm32`": **nothing**. The
/// interpreter's pure path is a single non-concurrent future whose only suspension point is
/// `tokio::task::yield_now`, which re-wakes immediately, so a bounded poll loop is a complete
/// executor for it. No reactor, no timer wheel, no threads, no `Instant::now` — all of which are
/// either unavailable or panicking on `wasm32-unknown-unknown`.
fn block_on<F: Future>(fut: F) -> Result<F::Output, &'static str> {
    // Safety: `fut` is a local that is never moved again after this shadowing binding.
    let mut fut = fut;
    let mut fut = unsafe { Pin::new_unchecked(&mut fut) };

    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    // Safety: the vtable's every entry is a no-op over a null data pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);

    for _ in 0..MAX_POLLS {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return Ok(v);
        }
    }
    Err("the portable core has no reactor: this program needs a host import to make progress")
}

/// Parse and evaluate `src`, returning the canonical JSON result the parity test compares.
///
/// The shape is deliberately small and fully determined by the program: `{"ok":true,"result":…,
/// "steps":…,"transcript":…}` on success, `{"ok":false,"error":…}` otherwise. Nothing
/// environment-derived (no time, no path, no id) may enter it, or parity would be meaningless.
pub fn eval_to_json(src: &str) -> String {
    let ast = match flux_lang::parse::parse(src) {
        Ok(ast) => ast,
        Err(e) => return failure(&format!("parse: {e}")),
    };
    let store = MemStore::new();
    let host = NoAuthority;
    let mut sink = SilentSink;
    let run = flux_lang::runtime::execute_flow(&store, &host, SESSION_ID, &ast, &mut sink);
    match block_on(run) {
        Ok(Ok(outcome)) => serde_json::json!({
            "ok": true,
            "result": outcome.result,
            "steps": outcome.steps,
            "transcript": outcome.transcript,
        })
        .to_string(),
        Ok(Err(e)) => failure(&format!("execute: {e}")),
        Err(e) => failure(e),
    }
}

fn failure(message: &str) -> String {
    serde_json::json!({ "ok": false, "error": message }).to_string()
}
