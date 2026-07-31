//! Fixture source file for `crates/flux-sdk/tests/strict_review.rs` (story C-319).
//!
//! NOT a compiled target: it lives under `tests/fixtures/`, a subdirectory cargo does not scan for
//! test targets. It exists so the `strict_review` flow's `read_many` step reads a PINNED file
//! instead of a live crate source whose content changes with every unrelated edit in the checkout.

/// The one thing the reviewer fan-out is nominally looking at.
pub fn parse_ticket_id(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    let digits = trimmed.strip_prefix("TICKET-")?;
    digits.parse().ok()
}

/// A deliberately unremarkable second function, so the fixture is not a one-liner.
pub fn render_ticket_id(id: u32) -> String {
    format!("TICKET-{id}")
}
