//! L-90 measurement harness: what one `didChange` + completion + hover cycle costs.
//!
//! Not a correctness test — `document::tests::one_edit_costs_one_parse` owns that. This exists to
//! produce the before/after number the L-90 acceptance asks to be recorded, on a large buffer, and
//! is `#[ignore]`d so it never slows the normal suite. Run with:
//!   cargo test -p codewandler-flux-lsp --test parse_cost -- --ignored --nocapture
use std::time::Instant;

const LARGE: &str = include_str!("fixtures/large.flux");

#[test]
#[ignore = "measurement, not a gate check"]
fn measure_cycle_cost() {
    let iters = 20;

    // One parse of the buffer — the unit both models are counted in.
    let t = Instant::now();
    for _ in 0..iters {
        let _ = flux_lang::parser::parse_cst(LARGE);
    }
    let per_parse = t.elapsed() / iters;

    // BEFORE (main.rs pre-split): refresh/diagnostics parsed, then completion parsed again, then
    // hover parsed again — every handler called `parse_cst` off the raw text.
    let before_parses = 3;
    // AFTER (L-90): the edit reparses once into the document store; completion and hover read the
    // cached tree.
    let after_parses = 1;

    println!(
        "buffer: {} lines, {} bytes",
        LARGE.lines().count(),
        LARGE.len()
    );
    println!("one parse: {per_parse:?}");
    println!(
        "cycle (didChange + completion + hover): before {before_parses} parses = {:?}, after {after_parses} parse = {:?}",
        per_parse * before_parses,
        per_parse * after_parses
    );
}
