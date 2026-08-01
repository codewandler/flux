//! The anchor for [`flux_core::redact_json_total`]'s **totality** (C-338).
//!
//! C-323 fixed the same node-kind hole in four independently hand-rolled JSON redaction walks:
//! each visited string leaves and skipped numbers, and two skipped object keys as well. The
//! duplication is why one walk could narrow while the others did not — there was no single place
//! where "total" was defined. This file is that definition restated from outside the crate, so the
//! walk and its anchor cannot narrow together.
//!
//! Two mechanisms, matching the shape `capability_widenings` uses in `flux-plugin`:
//!
//! 1. **A new `serde_json::Value` variant fails the build.** [`classify`] below matches every
//!    variant with no catch-all arm, and so does the production walk. Neither can absorb a new node
//!    kind as "some other scalar" — which is precisely the arm all four copies ended in.
//! 2. **Narrowing the walk reds a named test.** [`probe`] names, per node kind, the exact text only
//!    a walk that reaches that kind could hand the redactor; dropping a kind from the walk fails
//!    `the_total_walk_offers_every_json_node_kind_to_the_redactor` by name.

use serde_json::{json, Value};

/// Every variant of [`serde_json::Value`], restated as this file's own enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

const ALL: [NodeKind; 6] = [
    NodeKind::Null,
    NodeKind::Bool,
    NodeKind::Number,
    NodeKind::String,
    NodeKind::Array,
    NodeKind::Object,
];

/// Classify a node. **Exhaustive with no catch-all arm on purpose**: a `serde_json::Value` variant
/// added upstream must red this file rather than fall through to an existing kind.
fn classify(value: &Value) -> NodeKind {
    match value {
        Value::Null => NodeKind::Null,
        Value::Bool(_) => NodeKind::Bool,
        Value::Number(_) => NodeKind::Number,
        Value::String(_) => NodeKind::String,
        Value::Array(_) => NodeKind::Array,
        Value::Object(_) => NodeKind::Object,
    }
}

/// A bare node of each kind, so [`ALL`] is checked against `serde_json` itself rather than merely
/// compiling.
fn bare(kind: NodeKind) -> Value {
    match kind {
        NodeKind::Null => Value::Null,
        NodeKind::Bool => json!(true),
        NodeKind::Number => json!(1),
        NodeKind::String => json!("s"),
        NodeKind::Array => json!([]),
        NodeKind::Object => json!({}),
    }
}

/// A document carrying a marker at — and only at — a node of `kind`, with the marker text.
///
/// The marker is the exact string a walk must hand the redactor to have *reached* that kind: the
/// JSON literal spelling for a non-string scalar, the element for an array (only a walk that
/// descends sees it), the key for an object (only a walk that visits keys sees it).
///
/// Exhaustive on [`NodeKind`]: a kind added above reds this until it names the text that proves the
/// walk gets there.
fn probe(kind: NodeKind) -> (Value, String) {
    match kind {
        NodeKind::Null => (json!({ "field": null }), "null".to_string()),
        NodeKind::Bool => (json!({ "field": true }), "true".to_string()),
        // All-digit: outside every redaction heuristic by construction, so registration is its only
        // recourse and a walk that skips numbers makes that recourse conditional on the vendor's
        // choice of JSON type. This is the C-323 defect itself.
        NodeKind::Number => (json!({ "field": 216_216_216 }), "216216216".to_string()),
        NodeKind::String => (
            json!({ "field": "only-in-a-string-leaf" }),
            "only-in-a-string-leaf".to_string(),
        ),
        NodeKind::Array => (
            json!({ "field": ["only-in-an-array-element"] }),
            "only-in-an-array-element".to_string(),
        ),
        NodeKind::Object => (
            json!({ "only-as-an-object-key": 1 }),
            "only-as-an-object-key".to_string(),
        ),
    }
}

#[test]
fn the_kind_table_matches_serde_json() {
    for kind in ALL {
        assert_eq!(classify(&bare(kind)), kind);
    }
}

/// The structural guard: every node kind is *offered* to the redactor. A walk that stops visiting a
/// kind fails here by name, whether or not any current caller happens to notice.
#[test]
fn the_total_walk_offers_every_json_node_kind_to_the_redactor() {
    for kind in ALL {
        let (mut value, marker) = probe(kind);
        let seen = std::cell::RefCell::new(Vec::new());
        flux_core::redact_json_total(&mut value, &|text| {
            seen.borrow_mut().push(text.to_string());
            text.to_string()
        });
        let seen = seen.into_inner();
        assert!(
            seen.contains(&marker),
            "the total walk never offered the {kind:?} node (`{marker}`) to the redactor; it saw {seen:?}"
        );
    }
}

/// The behaviour that guard buys: a registered value is redacted wherever it sits.
#[test]
fn a_registered_value_is_redacted_at_every_node_kind() {
    for kind in ALL {
        let (mut value, marker) = probe(kind);
        flux_core::redact_json_total(&mut value, &|text| text.replace(&marker, "[redacted]"));
        let encoded = serde_json::to_string(&value).expect("re-encodes");
        assert!(
            !encoded.contains(&marker),
            "{kind:?}: `{marker}` survived the walk in {encoded}"
        );
        assert!(
            encoded.contains("[redacted]"),
            "{kind:?}: nothing was marked redacted in {encoded}"
        );
    }
}

/// A redacted non-string scalar is retyped to a string, and only when redaction actually fired —
/// `[redacted]` is not a number, and a sentinel number would be indistinguishable from real data.
#[test]
fn only_a_scalar_redaction_actually_fired_on_changes_type() {
    let mut value = json!({ "page": 2, "account_id": 216_216_216, "ok": true });
    flux_core::redact_json_total(&mut value, &|text| text.replace("216216216", "[redacted]"));
    assert_eq!(value["page"], json!(2), "an ordinary number kept its type");
    assert_eq!(value["ok"], json!(true), "an ordinary bool kept its type");
    assert_eq!(value["account_id"], json!("[redacted]"));
}

/// The report drives the cassette's two-path `input_view` split (C-323), so the condition it
/// reports is pinned here rather than left to the caller to re-derive.
///
/// Textual substitution on the *original serialization* preserves the caller's field order, which
/// matters because the view is capped and a truncated head is what a person reads. It is only safe
/// for a string leaf: `"…"` is a self-delimiting token, whereas replacing `216216` inside
/// `1216216789` would splice a quoted string into the middle of a number literal and leave the text
/// unparseable — and the TUI re-parses it.
#[test]
fn the_report_demands_a_reencode_exactly_when_textual_substitution_is_unsafe() {
    let string_leaf =
        flux_core::redact_json_total(&mut json!({ "token": "sk-live-abcdef" }), &|text| {
            text.replace("sk-live-abcdef", "[redacted]")
        });
    assert!(string_leaf.changed());
    assert!(
        !string_leaf.needs_reencode,
        "a string leaf is substitutable in the original text"
    );
    assert_eq!(
        string_leaf.string_leaf_replacements,
        vec![(
            "\"sk-live-abcdef\"".to_string(),
            "\"[redacted]\"".to_string()
        )],
        "the pair is reported as encoded JSON tokens, not raw text"
    );

    let number = flux_core::redact_json_total(&mut json!({ "id": 216_216_216 }), &|text| {
        text.replace("216216216", "[redacted]")
    });
    assert!(
        number.needs_reencode,
        "a bare number literal has no self-delimiting token to substitute"
    );

    let key = flux_core::redact_json_total(&mut json!({ "sk-live-abcdef": 1 }), &|text| {
        text.replace("sk-live-abcdef", "[redacted]")
    });
    assert!(
        key.needs_reencode,
        "rewriting a key in the text would have to move its value with it"
    );

    let untouched =
        flux_core::redact_json_total(&mut json!({ "page": 2, "q": "hello" }), &|text| {
            text.to_string()
        });
    assert!(
        !untouched.changed(),
        "an input that needed no redaction is reported unchanged, and is returned verbatim"
    );
}
