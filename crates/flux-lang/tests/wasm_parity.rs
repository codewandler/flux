//! C-271 — the portable core's first end-to-end proof.
//!
//! Runs one model-free `.flux` source through the reference interpreter twice: once compiled for
//! the host (this test binary) and once compiled for `wasm32-unknown-unknown` and executed inside a
//! Wasm engine. The two results must be byte-identical.
//!
//! Parity is asserted **against the native engine**, not a checked-in golden file. A golden would
//! let both substrates drift together and still pass.
//!
//! The Wasm half needs the artifact `scripts/build-portable-wasm.sh` produces, which in turn needs
//! the `wasm32-unknown-unknown` target installed. When the artifact is absent the Wasm tests report
//! why and skip; the build script re-runs them with `FLUX_PORTABLE_WASM_REQUIRED=1`, which turns
//! absence into a failure so the recorded command cannot pass vacuously.

#[path = "../examples/portable/core.rs"]
mod portable_core;

use wasmi::{Engine, Instance, Linker, Memory, Module, Store};

/// The parity program, read from disk so both substrates evaluate the same bytes.
fn parity_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/portable/parity.flux");
    std::fs::read_to_string(path).expect("the parity program is checked in next to the core")
}

/// Where `scripts/build-portable-wasm.sh` leaves the module, overridable for an out-of-tree
/// `CARGO_TARGET_DIR`.
fn wasm_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FLUX_PORTABLE_WASM") {
        return p.into();
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/release/examples/flux_portable.wasm")
}

/// The module bytes, or `None` when the artifact has not been built.
///
/// Returning `None` is a *skip* by default and a *failure* under `FLUX_PORTABLE_WASM_REQUIRED=1`,
/// so a developer without the wasm target still gets a green `cargo test --workspace` while the
/// recorded build command still proves something.
fn module_bytes() -> Option<Vec<u8>> {
    let path = wasm_path();
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            let missing = format!(
                "the portable wasm module is not built at {} ({e}) — run scripts/build-portable-wasm.sh",
                path.display()
            );
            assert!(
                std::env::var("FLUX_PORTABLE_WASM_REQUIRED").is_err(),
                "{missing}"
            );
            eprintln!("SKIP: {missing}");
            None
        }
    }
}

/// An instantiated module plus the handles the ABI needs.
struct PortableModule {
    store: Store<()>,
    instance: Instance,
    memory: Memory,
}

impl PortableModule {
    /// Instantiate with an **empty** import table. If the module ever grows an import this fails
    /// here, which is the point: the baseline is zero ambient authority.
    fn instantiate(bytes: &[u8]) -> Self {
        let engine = Engine::default();
        let module =
            Module::new(&engine, bytes).expect("the portable module is a valid wasm module");
        let mut store = Store::new(&engine, ());
        let linker = <Linker<()>>::new(&engine);
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .expect("instantiating with an empty import table");
        let memory = instance
            .get_memory(&store, "memory")
            .expect("the module exports its linear memory");
        Self {
            store,
            instance,
            memory,
        }
    }

    /// Run `src` through the module's `flux_alloc` / `flux_eval` / `flux_dealloc` ABI.
    fn eval(&mut self, src: &str) -> String {
        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&self.store, "flux_alloc")
            .expect("flux_alloc");
        let dealloc = self
            .instance
            .get_typed_func::<(i32, i32), ()>(&self.store, "flux_dealloc")
            .expect("flux_dealloc");
        let eval = self
            .instance
            .get_typed_func::<(i32, i32), i64>(&self.store, "flux_eval")
            .expect("flux_eval");

        let len = src.len() as i32;
        let ptr = alloc.call(&mut self.store, len).expect("flux_alloc call");
        self.memory
            .write(&mut self.store, ptr as usize, src.as_bytes())
            .expect("writing the source into linear memory");

        let packed = eval
            .call(&mut self.store, (ptr, len))
            .expect("flux_eval call");
        dealloc
            .call(&mut self.store, (ptr, len))
            .expect("flux_dealloc call");

        let out_ptr = (packed >> 32) as usize;
        let out_len = (packed & 0xffff_ffff) as usize;
        let mut buf = vec![0u8; out_len];
        self.memory
            .read(&self.store, out_ptr, &mut buf)
            .expect("reading the result out of linear memory");
        dealloc
            .call(&mut self.store, (out_ptr as i32, out_len as i32))
            .expect("flux_dealloc call for the result");
        String::from_utf8(buf).expect("the module returns UTF-8 JSON")
    }
}

#[test]
fn the_native_engine_evaluates_the_parity_program() {
    let out = portable_core::eval_to_json(&parity_source());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["ok"], true,
        "the parity program must run host-free: {out}"
    );
    assert_eq!(
        v["result"], "flux-portable answer=42 verdict=big",
        "unexpected native result: {out}"
    );
}

/// The security claim of the epic, made machine-checkable: a model-free program's module asks the
/// embedder for **nothing**. Every future host import (C-272) shows up right here.
#[test]
fn the_portable_module_declares_no_imports() {
    let Some(bytes) = module_bytes() else { return };
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).expect("a valid wasm module");
    let imports: Vec<String> = module
        .imports()
        .map(|i| format!("{}::{}", i.module(), i.name()))
        .collect();
    assert!(
        imports.is_empty(),
        "the portable core must have no ambient authority, but the module imports: {imports:?}"
    );
}

/// The parity assertion itself: the same source, the same result, on two substrates.
#[test]
fn the_wasm_module_and_the_native_engine_agree() {
    let Some(bytes) = module_bytes() else { return };
    let src = parity_source();

    let native = portable_core::eval_to_json(&src);
    let wasm = PortableModule::instantiate(&bytes).eval(&src);

    assert_eq!(
        wasm, native,
        "the wasm32 build of the portable core disagreed with the native engine on the same source"
    );
    // Guard against the degenerate agreement where both sides failed identically.
    let v: serde_json::Value = serde_json::from_str(&native).unwrap();
    assert_eq!(
        v["ok"], true,
        "both substrates failed the same way: {native}"
    );
}

/// A program that reaches for an operation is refused on both substrates, identically — the
/// model-free scope limit, asserted rather than merely documented.
#[test]
fn an_op_call_is_refused_identically_on_both_substrates() {
    let Some(bytes) = module_bytes() else { return };
    let src = "flow needs_a_host\n  $x = read(\"/etc/passwd\")\n  return $x\n";

    let native = portable_core::eval_to_json(src);
    let wasm = PortableModule::instantiate(&bytes).eval(src);

    assert_eq!(wasm, native, "the two substrates refused differently");
    let v: serde_json::Value = serde_json::from_str(&native).unwrap();
    assert_eq!(
        v["ok"], false,
        "an op call must not succeed host-free: {native}"
    );
}
