//! `context_slice` — automatic context slicing for planner and model ops (KF4/L-56,
//! `docs/designs/flux-lang-agent-speed.md`).
//!
//! The HIR already knows symbol dependencies and op schemas know what each operation reads. This
//! module turns that structure into the **minimum model-visible context** for one decision — a
//! planner repair round, or a single model-op call — instead of handwritten prompt trimming:
//!
//! - [`required_symbols_in_call`] / [`required_symbols_in_flow`] compute *which* symbols a call or
//!   a whole flow reads, narrowed to the accessed field path when the read is a `jq` access
//!   directly off a `var` (a plain "the whole value" read otherwise).
//! - [`required_symbols_from_diagnostics`] adds the symbols a planner repair diagnostic named —
//!   the model needs to see the thing its previous plan got wrong.
//! - [`slice_context`] combines those required-reads with each candidate symbol's visibility/
//!   secret/policy gating and a token budget (exact, when a host [`TokenCounter`] is supplied, or
//!   the deterministic [`estimate_tokens`] fallback) to decide the final kept set, and returns a
//!   full [`SliceRecord`] auditing why every candidate was in or out.
//!
//! Pure and IO-free like the rest of `flux-lang`: this module never touches a store, a secret, or a
//! policy engine — callers (the interpreter's `ctx`/`ctx_append` evaluation, the planner's repair
//! feedback) populate [`SymbolFlags`] from whatever DID compute visibility/secret/policy facts, and
//! compose the result into the existing `context` projection machinery (`flux-runtime::context`)
//! rather than bypassing it (design Notes, L-56).

use std::collections::{BTreeMap, BTreeSet};

use crate::analyze::Diagnostic;
use crate::ast::{Node, Visibility};
use crate::opspec::OpCatalog;

// ---------------------------------------------------------------------------
// Required symbols: HIR reads + field access paths
// ---------------------------------------------------------------------------

/// How a symbol was referenced: the whole bound value, or only specific `jq` field paths off it.
/// `Whole` absorbs `Fields` when both occur (a symbol read in full anywhere can't be narrowed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ReadKind {
    #[default]
    Whole,
    Fields(BTreeSet<String>),
}

impl ReadKind {
    fn absorb(&mut self, other: ReadKind) {
        match (&mut *self, other) {
            (ReadKind::Whole, _) => {}
            (ReadKind::Fields(_), ReadKind::Whole) => *self = ReadKind::Whole,
            (ReadKind::Fields(a), ReadKind::Fields(b)) => a.extend(b),
        }
    }
}

/// The per-call (or per-flow) dependency set: every symbol read, and how narrowly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequiredSymbols(pub BTreeMap<String, ReadKind>);

impl RequiredSymbols {
    pub fn require_whole(&mut self, name: &str) {
        self.0
            .entry(name.to_string())
            .and_modify(|k| k.absorb(ReadKind::Whole))
            .or_insert(ReadKind::Whole);
    }

    pub fn require_field(&mut self, name: &str, path: &str) {
        let mut fields = BTreeSet::new();
        fields.insert(path.to_string());
        self.0
            .entry(name.to_string())
            .and_modify(|k| k.absorb(ReadKind::Fields(fields.clone())))
            .or_insert(ReadKind::Fields(fields));
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn merge(&mut self, other: RequiredSymbols) {
        for (name, kind) in other.0 {
            self.0
                .entry(name)
                .and_modify(|k| k.absorb(kind.clone()))
                .or_insert(kind);
        }
    }
}

/// Per-call dependency reads (KF4 bullet 1: "HIR symbol reads" + "field access paths"): walk a
/// call's argument/template grammar — the same closed grammar [`crate::runtime::eval_arg`] /
/// `eval_template` accept (`lit`/`var`/`peek`/`thing`/`obj`/`list`/`jq`/`expr`/`fmt`/`parse`/`ctx`) —
/// recording each `$symbol` read, narrowed to the accessed field path when the read is a `jq`
/// access directly off a `var`/`peek`. Deliberately does NOT use the exhaustive control-flow
/// visitor ([`required_symbols_in_flow`]): a call's arguments can never contain a control-flow node
/// (the analyzer/`eval_arg` reject anything else as a call arg), and restricting the grammar here is
/// what lets a `jq`-narrowed read stay narrowed instead of ALSO being recorded as an unqualified
/// whole read of the same `var` node (which the generic exhaustive walker would revisit, collapsing
/// the narrowing).
pub fn required_symbols_in_call(args: &[Node]) -> RequiredSymbols {
    let mut req = RequiredSymbols::default();
    for n in args {
        collect_expr(n, &mut req);
    }
    req
}

fn collect_expr(n: &Node, req: &mut RequiredSymbols) {
    match n {
        Node::Var { name } | Node::Peek { name } => req.require_whole(&name.0),
        Node::Lit { value } => {
            let mut acc = BTreeSet::new();
            crate::optimize::collect_interp_reads(value, &mut acc);
            for name in acc {
                req.require_whole(&name);
            }
        }
        Node::Fmt { template } => {
            let mut acc = BTreeSet::new();
            crate::optimize::collect_interp_reads_str(template, &mut acc);
            for name in acc {
                req.require_whole(&name);
            }
        }
        Node::Obj { fields } => {
            for v in fields.values() {
                collect_expr(v, req);
            }
        }
        Node::List { items } => {
            for it in items {
                collect_expr(it, req);
            }
        }
        Node::Jq { path, input, .. } => match input.as_ref() {
            Node::Var { name } | Node::Peek { name } => req.require_field(&name.0, path),
            other => collect_expr(other, req),
        },
        Node::Expr { vars, .. } => {
            for v in vars.values() {
                collect_expr(v, req);
            }
        }
        Node::Parse { value, .. } => collect_expr(value, req),
        Node::Ctx {
            include, exclude, ..
        } => {
            for s in include.iter().chain(exclude.iter()) {
                req.require_whole(&s.0);
            }
        }
        Node::CtxAppend { ctx, add } => {
            req.require_whole(&ctx.0);
            for s in add {
                req.require_whole(&s.0);
            }
        }
        // `thing`/call/control-flow nodes carry no direct `$symbol` read at this leaf position (a
        // `call` is never a valid arg/template leaf — `eval_arg` rejects it); tolerated rather than
        // panicking so a malformed model-emitted arg never crashes slicing.
        _ => {}
    }
}

/// Whole-flow dependency reads (KF4 bullet 1's "HIR symbol reads" for a coarser, whole-plan
/// boundary — e.g. planner repair feedback, which wants "everything the rejected plan touches", not
/// one call's narrow slice). Reuses the analyzer's exhaustive [`crate::analyze::for_each_node`] walk
/// via [`crate::optimize::collect_var_reads`] (the same soundness-audited traversal the optimizer's
/// liveness pass uses, so this and that pass can never drift on what counts as "read") — always
/// `Whole`: field-path narrowing is [`required_symbols_in_call`]'s job for a single op invocation,
/// not a whole-flow scan.
pub fn required_symbols_in_flow(body: &[Node]) -> RequiredSymbols {
    let mut acc = BTreeSet::new();
    crate::optimize::collect_var_reads(body, &mut acc);
    let mut req = RequiredSymbols::default();
    for name in acc {
        req.require_whole(&name);
    }
    req
}

/// Narrow `required` to only the symbols that fill a **declared** param of `op` (KF4 bullet 1's
/// "operation schemas" dependency source): when `args` is the single named-object argument the
/// calling convention uses and `op` resolves in `ops`, keep only the object's fields whose key
/// matches one of the op's `required_params`/`optional_params` — dropping reads that live only
/// under an incidental extra key the op doesn't actually consume. Falls back to the unnarrowed
/// [`required_symbols_in_call`] result when the op is unknown or the call doesn't use the
/// single-object-argument form (a safe over-approximation: never hides a read).
pub fn required_symbols_for_call(op: &str, args: &[Node], ops: &dyn OpCatalog) -> RequiredSymbols {
    if let (Some(sig), [Node::Obj { fields }]) = (ops.lookup(op), args) {
        let declared: BTreeSet<&str> = sig
            .required_params
            .iter()
            .chain(sig.optional_params.iter())
            .map(|s| s.as_str())
            .collect();
        let mut req = RequiredSymbols::default();
        for (key, value) in fields {
            if declared.contains(key.as_str()) {
                req.merge(required_symbols_in_call(std::slice::from_ref(value)));
            }
        }
        req
    } else {
        required_symbols_in_call(args)
    }
}

/// Extract the symbol names a planner repair [`Diagnostic`] names (KF4 bullet 1's "planner repair
/// diagnostics" dependency source) — e.g. "unbound symbol `$typo`" — a deterministic scan for a `$`
/// followed by an identifier, tolerant of surrounding punctuation (backticks, sentence text).
pub fn required_symbols_from_diagnostics(diags: &[Diagnostic]) -> BTreeSet<String> {
    let mut acc = BTreeSet::new();
    for d in diags {
        scan_dollar_idents(&d.message, &mut acc);
    }
    acc
}

fn scan_dollar_idents(s: &str, acc: &mut BTreeSet<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                acc.insert(s[start..end].to_string());
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Gating: visibility / secret / policy boundary (KF4 bullet 4)
// ---------------------------------------------------------------------------

/// The gating facts about one candidate symbol beyond its being referenced. `flux-lang` has no
/// secret/policy engine of its own (it stays IO-free) — callers populate `secret_derived` /
/// `policy_denied` from whatever DID compute them (`flux-secret`'s `Redactor`, `flux-policy`'s
/// decision), and `visibility` from the store binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolFlags {
    pub visibility: Visibility,
    pub secret_derived: bool,
    pub policy_denied: bool,
}

impl SymbolFlags {
    pub fn visible() -> Self {
        Self {
            visibility: Visibility::Visible,
            secret_derived: false,
            policy_denied: false,
        }
    }

    /// The reason this symbol is gated (never included unless explicitly referenced AND
    /// permitted), or `None` when it crosses the default model-visible boundary freely.
    fn exclude_reason(&self) -> Option<ExcludeReason> {
        match self.visibility {
            Visibility::Private => Some(ExcludeReason::Private),
            Visibility::Hidden => Some(ExcludeReason::Hidden),
            _ if self.secret_derived => Some(ExcludeReason::SecretDerived),
            _ if self.policy_denied => Some(ExcludeReason::PolicyDenied),
            _ => None,
        }
    }
}

/// One candidate symbol for a slice: its name, gating flags, and the text used to size it against a
/// budget.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub flags: SymbolFlags,
    pub text: String,
}

/// Names explicitly permitted to cross a gated tier (Private/Hidden/secret-derived/policy-denied)
/// for THIS slice — the "and permitted for that model-visible boundary" half of bullet 4. Empty by
/// default: being referenced is never sufficient on its own.
#[derive(Debug, Clone, Default)]
pub struct Boundary {
    pub permitted: BTreeSet<String>,
}

impl Boundary {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn allowing(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            permitted: names.into_iter().collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Audit record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeReason {
    /// Always shown regardless of reference (mirrors the store's `Visibility::Pinned` contract).
    Pinned,
    /// Read in full by the HIR/call being sliced.
    SymbolRead,
    /// Read only through the given `jq` field path(s).
    FieldAccess(BTreeSet<String>),
    /// Named by a planner repair diagnostic, but not otherwise read.
    RepairDiagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludeReason {
    /// Not read by the HIR/call, not named by a diagnostic, and not pinned.
    NotReferenced,
    Private,
    Hidden,
    SecretDerived,
    PolicyDenied,
    /// Referenced and permitted, but didn't fit the remaining token budget.
    BudgetExceeded,
}

/// The full audit of one [`slice_context`] call: which candidates made it in and why, which were
/// left out and why, and the budget accounting (KF4 bullet 2 — "an audit record of which
/// symbols/evidence were included and why", bullet 3 — the enforced budget).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliceRecord {
    pub included: Vec<(String, IncludeReason)>,
    pub excluded: Vec<(String, ExcludeReason)>,
    pub budget: Option<u64>,
    pub used: u64,
}

// ---------------------------------------------------------------------------
// Token budgeting (KF4 bullet 3)
// ---------------------------------------------------------------------------

/// A host-provided exact token counter (e.g. a provider's tokenizer). When [`slice_context`] is
/// given one, budgets are enforced against its exact counts; otherwise [`estimate_tokens`] is used.
pub trait TokenCounter {
    fn count(&self, text: &str) -> u64;
}

/// The deterministic fallback estimate used when no host-provided counter is available: ~4
/// chars/token, rounded up so a non-empty string never estimates to zero.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

fn size_of(text: &str, counter: Option<&dyn TokenCounter>) -> u64 {
    counter
        .map(|c| c.count(text))
        .unwrap_or_else(|| estimate_tokens(text))
}

// ---------------------------------------------------------------------------
// The slice
// ---------------------------------------------------------------------------

fn visibility_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Pinned => 4,
        Visibility::Visible => 3,
        Visibility::Hidden => 2,
        Visibility::Expired => 1,
        Visibility::Private => 0,
    }
}

fn reason_rank(r: &IncludeReason) -> u8 {
    match r {
        IncludeReason::Pinned => 3,
        IncludeReason::SymbolRead => 2,
        IncludeReason::FieldAccess(_) => 1,
        IncludeReason::RepairDiagnostic => 0,
    }
}

/// Slice `candidates` down to the ones actually needed for one decision (KF4/L-56, the story's
/// central deterministic function):
///
/// 1. **Reference**: a candidate is a slice candidate at all only if it is `Visibility::Pinned`
///    (always shown, matching the store's existing pinned-symbol contract), read by `required`
///    (whole or field-narrowed), or named by `diagnostic_symbols` — otherwise it is excluded as
///    [`ExcludeReason::NotReferenced`].
/// 2. **Gate** (bullet 4): a referenced candidate whose [`SymbolFlags`] are gated (Private, Hidden,
///    secret-derived, or policy-denied) is still excluded unless its name is in `boundary.permitted`
///    — "explicitly referenced" is necessary but never sufficient on its own.
/// 3. **Budget** (bullet 3): the survivors are ranked (visibility tier, then reason specificity,
///    then declared `candidates` order) and packed **drop-and-continue** into `budget` — sized by
///    `counter` when given (exact host-provided counts) or [`estimate_tokens`] otherwise (the
///    deterministic fallback) — so one oversized early candidate never starves the smaller ones
///    after it. `budget: None` keeps every gate-surviving candidate.
///
/// Returns the kept names in `candidates`' original declared order, plus the full [`SliceRecord`]
/// naming every inclusion/exclusion and why.
pub fn slice_context(
    required: &RequiredSymbols,
    diagnostic_symbols: &BTreeSet<String>,
    candidates: &[Candidate],
    boundary: &Boundary,
    budget: Option<u64>,
    counter: Option<&dyn TokenCounter>,
) -> (Vec<String>, SliceRecord) {
    struct Ranked<'a> {
        idx: usize,
        reason: IncludeReason,
        visibility: Visibility,
        text: &'a str,
    }

    let mut ranked: Vec<Ranked> = Vec::new();
    let mut excluded: Vec<(String, ExcludeReason)> = Vec::new();

    for (idx, c) in candidates.iter().enumerate() {
        let is_pinned = c.flags.visibility == Visibility::Pinned;
        let reason = if is_pinned {
            Some(IncludeReason::Pinned)
        } else if let Some(kind) = required.0.get(&c.name) {
            Some(match kind {
                ReadKind::Whole => IncludeReason::SymbolRead,
                ReadKind::Fields(paths) => IncludeReason::FieldAccess(paths.clone()),
            })
        } else if diagnostic_symbols.contains(&c.name) {
            Some(IncludeReason::RepairDiagnostic)
        } else {
            None
        };

        let Some(reason) = reason else {
            excluded.push((c.name.clone(), ExcludeReason::NotReferenced));
            continue;
        };

        if let Some(gate) = c.flags.exclude_reason() {
            if !boundary.permitted.contains(&c.name) {
                excluded.push((c.name.clone(), gate));
                continue;
            }
        }

        ranked.push(Ranked {
            idx,
            reason,
            visibility: c.flags.visibility,
            text: &c.text,
        });
    }

    let mut order: Vec<usize> = (0..ranked.len()).collect();
    order.sort_by_key(|&k| {
        std::cmp::Reverse((
            visibility_rank(ranked[k].visibility),
            reason_rank(&ranked[k].reason),
        ))
    });

    let mut keep = vec![true; ranked.len()];
    let mut used = 0u64;
    if let Some(b) = budget {
        keep = vec![false; ranked.len()];
        let mut running = 0u64;
        for &k in &order {
            let t = size_of(ranked[k].text, counter);
            if running + t <= b {
                running += t;
                keep[k] = true;
            }
            // else: drop this candidate and keep packing the rest (never `break` — a single
            // oversized candidate must not starve the smaller ones after it).
        }
        used = running;
    } else {
        for r in &ranked {
            used += size_of(r.text, counter);
        }
    }

    let mut included: Vec<(String, IncludeReason)> = Vec::new();
    for (k, r) in ranked.into_iter().enumerate() {
        let name = candidates[r.idx].name.clone();
        if keep[k] {
            included.push((name, r.reason));
        } else {
            excluded.push((name, ExcludeReason::BudgetExceeded));
        }
    }

    // Report both lists in `candidates`' declared order (deterministic, and independent of the
    // budget-packing rank order used above).
    let pos = |name: &str| {
        candidates
            .iter()
            .position(|c| c.name == name)
            .unwrap_or(usize::MAX)
    };
    included.sort_by_key(|(n, _)| pos(n));
    excluded.sort_by_key(|(n, _)| pos(n));

    let kept: Vec<String> = included.iter().map(|(n, _)| n.clone()).collect();
    let record = SliceRecord {
        included,
        excluded,
        budget,
        used,
    };
    (kept, record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SymbolName;
    use crate::opspec::OpSignature;
    use std::collections::BTreeMap as Map;

    fn var(name: &str) -> Node {
        Node::Var {
            name: SymbolName(name.into()),
        }
    }

    fn obj(pairs: Vec<(&str, Node)>) -> Node {
        Node::Obj {
            fields: pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), Box::new(v)))
                .collect(),
        }
    }

    fn candidate(name: &str, vis: Visibility, text: &str) -> Candidate {
        Candidate {
            name: name.into(),
            flags: SymbolFlags {
                visibility: vis,
                secret_derived: false,
                policy_denied: false,
            },
            text: text.into(),
        }
    }

    // -- required_symbols_in_call ---------------------------------------------------------------

    #[test]
    fn call_reads_capture_var_and_narrow_jq_field_access() {
        let args = vec![obj(vec![
            ("ask", var("ask")),
            (
                "claim",
                Node::Jq {
                    path: "text".to_string(),
                    input: Box::new(var("claim")),
                    optional: false,
                },
            ),
        ])];
        let req = required_symbols_in_call(&args);
        assert_eq!(req.0.get("ask"), Some(&ReadKind::Whole));
        assert_eq!(
            req.0.get("claim"),
            Some(&ReadKind::Fields(BTreeSet::from(["text".to_string()])))
        );
    }

    #[test]
    fn a_field_access_and_a_separate_whole_read_of_the_same_symbol_stays_whole() {
        let args = vec![
            var("x"),
            Node::Jq {
                path: "a".to_string(),
                input: Box::new(var("x")),
                optional: false,
            },
        ];
        let req = required_symbols_in_call(&args);
        assert_eq!(
            req.0.get("x"),
            Some(&ReadKind::Whole),
            "whole absorbs field"
        );
    }

    #[test]
    fn required_symbols_in_flow_sees_a_read_nested_in_control_flow() {
        // A read inside a `when` body is invisible to `required_symbols_in_call` (that function
        // never walks control flow) but must be visible to the whole-flow pass.
        let body = vec![Node::When {
            cond: Box::new(var("gate")),
            then: vec![Node::Return {
                value: Box::new(var("payload")),
            }],
            otherwise: vec![],
        }];
        let req = required_symbols_in_flow(&body);
        assert!(req.contains("gate"));
        assert!(req.contains("payload"));
    }

    // -- required_symbols_for_call (operation schemas) ------------------------------------------

    fn op_sig(required: &[&str], optional: &[&str]) -> OpSignature {
        OpSignature {
            name: "ai.reason".into(),
            description: String::new(),
            effects: vec![],
            risk: flux_spec::Risk::Low,
            idempotency: flux_spec::Idempotency::Idempotent,
            required_params: required.iter().map(|s| s.to_string()).collect(),
            optional_params: optional.iter().map(|s| s.to_string()).collect(),
            param_types: Map::new(),
            output: crate::ast::TypeRef::Any,
            semantic_effects: Vec::new(),
        }
    }

    struct FixedCatalog(Option<OpSignature>);
    impl OpCatalog for FixedCatalog {
        fn lookup(&self, _name: &str) -> Option<OpSignature> {
            self.0.clone()
        }
    }

    #[test]
    fn op_schema_narrows_to_declared_params_only() {
        let ops = FixedCatalog(Some(op_sig(&["ask"], &["ctx"])));
        let args = vec![obj(vec![
            ("ask", var("ask")),
            ("ctx", var("pack")),
            // `junk` is not a declared param of the op — the analyzer would already reject a
            // real plan carrying it, but a defensive slicer must not pull in its reads either.
            ("junk", var("unrelated")),
        ])];
        let req = required_symbols_for_call("ai.reason", &args, &ops);
        assert!(req.contains("ask"));
        assert!(req.contains("pack"));
        assert!(
            !req.contains("unrelated"),
            "undeclared param's read must not leak in"
        );
    }

    #[test]
    fn op_schema_narrowing_falls_back_to_full_reads_for_an_unknown_op() {
        let ops = FixedCatalog(None);
        let args = vec![obj(vec![("whatever", var("x"))])];
        let req = required_symbols_for_call("no.such.op", &args, &ops);
        assert!(
            req.contains("x"),
            "unknown op is a safe over-approximation, not a hole"
        );
    }

    // -- required_symbols_from_diagnostics ---------------------------------------------------------

    #[test]
    fn diagnostics_scan_extracts_dollar_symbol_names() {
        let diags = vec![Diagnostic::new(
            "unbound symbol `$typo` — it is not a flow param, is never bound",
        )];
        let names = required_symbols_from_diagnostics(&diags);
        assert_eq!(names, BTreeSet::from(["typo".to_string()]));
    }

    // -- slice_context: referenced-only + audit ------------------------------------------------

    #[test]
    fn slices_only_referenced_symbols() {
        let mut required = RequiredSymbols::default();
        required.require_whole("used");
        let candidates = vec![
            candidate("used", Visibility::Visible, "u"),
            candidate("unused", Visibility::Visible, "v"),
        ];
        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert_eq!(kept, vec!["used".to_string()]);
        assert_eq!(
            record.excluded,
            vec![("unused".to_string(), ExcludeReason::NotReferenced)]
        );
        assert_eq!(
            record.included,
            vec![("used".to_string(), IncludeReason::SymbolRead)]
        );
    }

    #[test]
    fn pinned_symbols_are_always_kept_even_when_unreferenced() {
        let candidates = vec![candidate("pin", Visibility::Pinned, "p")];
        let (kept, record) = slice_context(
            &RequiredSymbols::default(),
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert_eq!(kept, vec!["pin".to_string()]);
        assert_eq!(record.included[0].1, IncludeReason::Pinned);
    }

    #[test]
    fn diagnostic_only_reference_is_kept_and_labeled() {
        let candidates = vec![candidate("typo", Visibility::Visible, "t")];
        let diag_symbols = BTreeSet::from(["typo".to_string()]);
        let (kept, record) = slice_context(
            &RequiredSymbols::default(),
            &diag_symbols,
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert_eq!(kept, vec!["typo".to_string()]);
        assert_eq!(record.included[0].1, IncludeReason::RepairDiagnostic);
    }

    // -- slice_context: gating (bullet 4) -------------------------------------------------------

    #[test]
    fn excludes_private_and_hidden_and_secret_and_policy_denied_by_default() {
        let mut required = RequiredSymbols::default();
        for n in ["priv", "hid", "secret", "denied"] {
            required.require_whole(n);
        }
        let candidates = vec![
            candidate("priv", Visibility::Private, "x"),
            candidate("hid", Visibility::Hidden, "x"),
            Candidate {
                name: "secret".into(),
                flags: SymbolFlags {
                    visibility: Visibility::Visible,
                    secret_derived: true,
                    policy_denied: false,
                },
                text: "x".into(),
            },
            Candidate {
                name: "denied".into(),
                flags: SymbolFlags {
                    visibility: Visibility::Visible,
                    secret_derived: false,
                    policy_denied: true,
                },
                text: "x".into(),
            },
        ];
        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert!(
            kept.is_empty(),
            "every gated symbol excluded by default: {kept:?}"
        );
        assert_eq!(
            record.excluded,
            vec![
                ("priv".to_string(), ExcludeReason::Private),
                ("hid".to_string(), ExcludeReason::Hidden),
                ("secret".to_string(), ExcludeReason::SecretDerived),
                ("denied".to_string(), ExcludeReason::PolicyDenied),
            ]
        );
    }

    #[test]
    fn a_gated_symbol_is_included_only_when_explicitly_referenced_and_permitted() {
        let candidates = vec![candidate("priv", Visibility::Private, "x")];

        // Referenced but NOT permitted: still excluded.
        let mut required = RequiredSymbols::default();
        required.require_whole("priv");
        let (kept, _) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert!(kept.is_empty());

        // Permitted but NOT referenced: still excluded (permission alone isn't a reference).
        let (kept, record) = slice_context(
            &RequiredSymbols::default(),
            &BTreeSet::new(),
            &candidates,
            &Boundary::allowing(["priv".to_string()]),
            None,
            None,
        );
        assert!(kept.is_empty());
        assert_eq!(record.excluded[0].1, ExcludeReason::NotReferenced);

        // Both referenced AND permitted: included.
        let (kept, _) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::allowing(["priv".to_string()]),
            None,
            None,
        );
        assert_eq!(kept, vec!["priv".to_string()]);
    }

    // -- slice_context: budget (bullet 3) -------------------------------------------------------

    struct FixedCounter(u64);
    impl TokenCounter for FixedCounter {
        fn count(&self, _text: &str) -> u64 {
            self.0
        }
    }

    #[test]
    fn budget_uses_the_exact_host_provided_counter_when_given() {
        let mut required = RequiredSymbols::default();
        required.require_whole("a");
        let candidates = vec![candidate(
            "a",
            Visibility::Visible,
            "anything, any length at all",
        )];
        let counter = FixedCounter(5);
        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(5),
            Some(&counter),
        );
        assert_eq!(kept, vec!["a".to_string()]);
        assert_eq!(
            record.used, 5,
            "exact count from the host counter, not the char estimate"
        );
    }

    #[test]
    fn budget_falls_back_to_the_deterministic_estimator_without_a_counter() {
        let text = "x".repeat(40); // ~10 tokens at the 4-chars/token fallback
        assert_eq!(estimate_tokens(&text), 10);
        let mut required = RequiredSymbols::default();
        required.require_whole("a");
        let candidates = vec![candidate("a", Visibility::Visible, &text)];
        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(10),
            None,
        );
        assert_eq!(kept, vec!["a".to_string()]);
        assert_eq!(record.used, 10);
    }

    #[test]
    fn budget_trims_deterministically_dropping_the_lowest_priority_member() {
        let mut required = RequiredSymbols::default();
        for n in ["a", "b", "c"] {
            required.require_whole(n);
        }
        let text40 = "x".repeat(40); // 10 tokens (fallback estimator)
        let candidates = vec![
            candidate("a", Visibility::Visible, &text40),
            candidate("b", Visibility::Visible, &text40),
            candidate("c", Visibility::Pinned, &text40),
        ];
        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(25), // fits exactly two of three ~10-token members
            None,
        );
        assert_eq!(kept.len(), 2, "budget kept exactly two of three: {kept:?}");
        assert!(
            kept.contains(&"c".to_string()),
            "pinned member always survives"
        );
        assert_eq!(
            record.excluded,
            vec![("b".to_string(), ExcludeReason::BudgetExceeded)],
            "the lower-priority, later-ranked member is the one dropped"
        );
    }

    #[test]
    fn budget_drop_and_continue_never_lets_one_oversized_candidate_starve_the_rest() {
        // Mirrors `runtime::build_ctx`'s s_251 fix: an oversized EARLY (higher-priority) member
        // must not evict every smaller member ranked after it.
        let mut required = RequiredSymbols::default();
        for n in ["big", "small1", "small2"] {
            required.require_whole(n);
        }
        let candidates = vec![
            candidate("big", Visibility::Pinned, &"x".repeat(4000)), // ~1000 tokens, won't fit
            candidate("small1", Visibility::Visible, "abcd"),        // 1 token
            candidate("small2", Visibility::Visible, "abcd"),        // 1 token
        ];
        let (kept, _) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(2),
            None,
        );
        assert_eq!(
            kept,
            vec!["small1".to_string(), "small2".to_string()],
            "the oversized pinned member is dropped, not the small ones after it"
        );
    }

    #[test]
    fn slicing_is_deterministic_across_repeated_calls() {
        let mut required = RequiredSymbols::default();
        for n in ["a", "b", "c", "d"] {
            required.require_whole(n);
        }
        let candidates = vec![
            candidate("a", Visibility::Visible, "aaaa"),
            candidate("b", Visibility::Pinned, "bbbb"),
            candidate("c", Visibility::Hidden, "cccc"),
            candidate("d", Visibility::Visible, "dddd"),
        ];
        let first = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(3),
            None,
        );
        for _ in 0..5 {
            let again = slice_context(
                &required,
                &BTreeSet::new(),
                &candidates,
                &Boundary::none(),
                Some(3),
                None,
            );
            assert_eq!(
                first, again,
                "same inputs must always slice to the same output"
            );
        }
    }

    // -- equivalence when the full context would exceed the budget (bullet 5) -------------------

    #[test]
    fn equivalence_for_a_flow_whose_full_context_would_exceed_the_budget() {
        // A flow that reads five session symbols via a mix of whole reads and field access; the
        // full rendered context would be ~50 tokens, but the budget only allows ~20. The slice
        // must (a) never exceed the budget, (b) always keep every PINNED/permitted-referenced
        // symbol that fits on its own, and (c) explain every drop in the audit record — i.e. it
        // behaves like the unbudgeted slice up to the point the budget runs out, not like a
        // different, unrelated selection.
        let body = vec![
            Node::Bind {
                name: SymbolName("out1".into()),
                value: Box::new(var("s1")),
                ty: None,
                effect: None,
            },
            Node::Bind {
                name: SymbolName("out2".into()),
                value: Box::new(Node::Jq {
                    path: "field".into(),
                    input: Box::new(var("s2")),
                    optional: false,
                }),
                ty: None,
                effect: None,
            },
        ];
        let required = required_symbols_in_flow(&body);
        // `required_symbols_in_flow` doesn't narrow (that's the per-call function's job) but must
        // still see both reads.
        assert!(required.contains("s1"));
        assert!(required.contains("s2"));

        let big = "x".repeat(400); // 100 tokens (fallback estimator) each
        let candidates = vec![
            candidate("s1", Visibility::Visible, &big),
            candidate("s2", Visibility::Visible, &big),
            candidate("pinned", Visibility::Pinned, "tiny"),
            candidate("irrelevant", Visibility::Visible, &big),
        ];
        let unbudgeted = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            None,
            None,
        );
        assert_eq!(
            unbudgeted.0,
            vec!["s1".to_string(), "s2".to_string(), "pinned".to_string()],
            "unbudgeted: every referenced/pinned symbol included, the unread one excluded"
        );

        let (kept, record) = slice_context(
            &required,
            &BTreeSet::new(),
            &candidates,
            &Boundary::none(),
            Some(105),
            None,
        );
        assert!(
            record.used <= 105,
            "budget is never exceeded: used={}",
            record.used
        );
        assert!(
            kept.contains(&"pinned".to_string()),
            "the pinned member always fits and is always kept"
        );
        assert!(
            !kept.contains(&"irrelevant".to_string()),
            "the never-read symbol stays excluded regardless of budget"
        );
        assert!(
            record
                .excluded
                .iter()
                .any(|(n, r)| n == "irrelevant" && *r == ExcludeReason::NotReferenced),
            "the audit still explains the irrelevant exclusion by reference, not budget"
        );
    }
}
