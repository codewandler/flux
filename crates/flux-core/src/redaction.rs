//! The one total redaction walk over a parsed JSON document.
//!
//! Four crates independently hand-rolled the same traversal — flux-web's response-body scrub,
//! flux-flow's evidence flush and its cassette `input_view`, and flux-orchestrate's sub-agent
//! activity reporter — and C-323 had to fix the *same* node-kind hole in all four: each visited
//! string leaves and skipped non-string scalars, and two skipped object keys as well. The
//! duplication is the mechanism by which that hole existed at all; there was no single place where
//! "total" was defined, so every walk was free to narrow on its own (C-338).
//!
//! # Why here
//!
//! In L0, for the reason [`crate::percent_encode_component`] is: every one of those crates already
//! depends on `flux-core`, so consolidating costs no new dependency edge and no layering change.
//!
//! It deliberately does **not** live in `flux-secret`, the crate that owns the `Redactor`. That
//! would have meant giving a published L0 crate a `serde_json` dependency it does not have, plus a
//! new `flux-web` → `flux-secret` edge — and flux-web takes a redaction *closure* precisely to avoid
//! that edge. Taking the closure here keeps the seam exactly where flux-web put it: the walk knows
//! how to visit every node, the caller knows what a secret is, and neither has to learn the other's
//! job.
//!
//! # Why totality is the guarantee
//!
//! The earlier walks visited only strings, on the reasoning that "numbers cannot carry a secret".
//! That is false for the one credential shape with no other protection: an all-digit credential is
//! outside every redaction heuristic *by construction* — no prefix marks it, and the contextual
//! `NAME=VALUE` rule requires a letter precisely so `secret_ttl=3600` survives — so **registration
//! is its only recourse**, and a walk that narrows by node kind makes that recourse conditional on
//! the vendor's choice of JSON type. Object keys are no different: a vendor echoing a request record
//! back, or a model-generated header map, can put a credential in a key as easily as in a value.
//! `add_secret`'s guarantee is total or it is not a guarantee.
//!
//! The anchor is `crates/flux-core/tests/json_redaction_totality.rs`.

use serde_json::Value;

/// What a [`redact_json_total`] walk changed, in the two terms a caller can act on.
///
/// Most callers only need [`JsonRedaction::changed`]. The rest exists for a caller that must patch
/// the document's **original serialization** rather than re-encode it — flux-flow's cassette
/// `input_view`, which is capped, so the caller's field order decides what a person actually reads
/// in the truncated head (`serde_json` is built without `preserve_order`, so re-encoding sorts keys
/// and moves that head).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct JsonRedaction {
    /// One `(raw, redacted)` pair of **encoded JSON string tokens** per string leaf the walk
    /// rewrote — `("\"sk-live-x\"", "\"[redacted]\"")`, not the bare text.
    ///
    /// Encoded, because that is the form a substitution over the serialized document has to match:
    /// a registered value whose spelling changes under JSON escaping (quotes, backslashes,
    /// newlines) would otherwise be missed by a textual patch and stay recoverable.
    pub string_leaf_replacements: Vec<(String, String)>,
    /// Set when the walk rewrote something textual substitution cannot express safely, so the
    /// caller must re-encode the scrubbed value instead of patching the original text.
    ///
    /// Two causes, and both are load-bearing:
    ///
    /// - **A non-string scalar.** `"…"` is a self-delimiting token; a bare number literal is not.
    ///   Replacing `216216` inside `1216216789` would splice a quoted string into the middle of a
    ///   number and leave the document unparseable — and flux-flow's `input_view` is re-parsed by
    ///   the TUI.
    /// - **An object key.** Rewriting a key in the text would have to move its value with it.
    pub needs_reencode: bool,
}

impl JsonRedaction {
    /// Whether redaction fired anywhere in the document.
    ///
    /// Equivalent to comparing the value before and after, without keeping a clone to compare
    /// against.
    pub fn changed(&self) -> bool {
        self.needs_reencode || !self.string_leaf_replacements.is_empty()
    }
}

/// Rewrite **every** node of `value` through `redact`, in place, and report what changed.
///
/// Every node kind is offered to `redact` as the text a reader of the encoded document sees: a
/// string leaf as its contents, an object key as the key, and a non-string scalar as its JSON
/// literal spelling (`216216216`, `true`, `null`).
///
/// A non-string scalar that `redact` changed is **retyped to a string**, and only then. `[redacted]`
/// is not a number, so a node that carried a credential cannot stay one; a sentinel number (`0`,
/// `-1`) was rejected because it is indistinguishable from data the vendor really sent, and `null`
/// has the same ambiguity in weaker form. The string marker is what every other redacted node in a
/// record already looks like. Because the shape change is scoped to nodes redaction actually
/// touched, an ordinary `page: 2` keeps its type — the only node that is retyped is one whose value
/// the caller could not have used anyway, since using it means using the credential.
///
/// `redact` is taken as `&dyn Fn` rather than a generic bound so there is one instantiation of the
/// walk in the whole tree, not one per caller.
pub fn redact_json_total(value: &mut Value, redact: &dyn Fn(&str) -> String) -> JsonRedaction {
    let mut report = JsonRedaction::default();
    walk(value, redact, &mut report);
    report
}

fn walk(value: &mut Value, redact: &dyn Fn(&str) -> String, report: &mut JsonRedaction) {
    // Exhaustive by variant, with **no catch-all arm** — that is the guard, not a style choice.
    // All four copies this replaced ended in a `scalar => …` rest arm, and a rest arm is exactly
    // what let three of them keep a node kind out of the redactor with nothing to notice. A new
    // `serde_json::Value` variant must red this build.
    match value {
        Value::String(text) => {
            let redacted = redact(text);
            if redacted == *text {
                return;
            }
            match (
                serde_json::to_string(text),
                serde_json::to_string(&redacted),
            ) {
                (Ok(raw), Ok(new)) => report.string_leaf_replacements.push((raw, new)),
                // A `String` cannot fail to encode in practice. If one somehow did, a textual patch
                // would be silently missing a replacement it needs, so demand the re-encode rather
                // than under-report.
                _ => report.needs_reencode = true,
            }
            *text = redacted;
        }
        Value::Array(items) => {
            for item in items {
                walk(item, redact, report);
            }
        }
        Value::Object(map) => {
            // Rebuilt rather than iterated with `values_mut`: a key carries a credential as easily
            // as a value does, and a key cannot be rewritten in place.
            let original = std::mem::take(map);
            for (key, mut child) in original {
                walk(&mut child, redact, report);
                let redacted = redact(&key);
                if redacted != key {
                    report.needs_reencode = true;
                }
                map.insert(redacted, child);
            }
        }
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_)) => {
            let literal = scalar.to_string();
            let redacted = redact(&literal);
            if redacted != literal {
                *scalar = Value::String(redacted);
                report.needs_reencode = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nesting_is_walked_to_the_leaves_on_both_container_kinds() {
        let mut value = json!({ "a": [{ "b": [[{ "c": "x" }]] }] });
        let report = redact_json_total(&mut value, &|text| text.replace('x', "y"));
        assert_eq!(value["a"][0]["b"][0][0]["c"], json!("y"));
        assert!(report.changed());
    }

    #[test]
    fn a_key_and_its_value_are_both_rewritten() {
        let mut value = json!({ "x": "x" });
        redact_json_total(&mut value, &|text| text.replace('x', "y"));
        assert_eq!(value, json!({ "y": "y" }));
    }
}
