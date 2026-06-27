//! The execution **host** seam: the narrow interface the interpreter dispatches operations and
//! approvals through. The engine implements [`OpHost`] over its real safety envelope
//! (`Executor::dispatch` + the approver); the language stays free of any concrete runtime or tool
//! dependency — it knows only this trait.

use async_trait::async_trait;

use flux_spec::IntentSet;

use crate::ast::{ResolvedThing, Selector, ThingId, ThingRef};
use crate::opspec::OpCatalog;

/// The result of dispatching one operation — the language-level mirror of the host's tool result.
#[derive(Debug, Clone)]
pub struct OpOutcome {
    /// The canonical content: bound to symbols, spliced into `{{interpolation}}`, used for control
    /// flow (`when`/`return`). Deterministic execution works with this.
    pub content: String,
    /// An optional model-facing rendering (line-numbered read, diff, …). `None` means it equals
    /// `content`. Surfaced to the sink, never bound or interpolated.
    pub view: Option<String>,
    pub is_error: bool,
}

impl OpOutcome {
    /// A successful outcome whose view equals its content.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: false,
        }
    }

    /// An error outcome (the message is the content).
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: true,
        }
    }

    /// The model-facing view, or the canonical content when no distinct view was set.
    pub fn view(&self) -> &str {
        self.view.as_deref().unwrap_or(&self.content)
    }
}

/// The approval decision for a `confirm` gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Allow,
    Deny,
}

/// The execution host the interpreter runs operations through. Every op a flow calls is dispatched
/// here, so the host is the single point where the language meets the engine's safety envelope.
#[async_trait]
pub trait OpHost: Send + Sync {
    /// Dispatch an op with its resolved named input. Errors are reported in-band via
    /// [`OpOutcome::is_error`] (dispatch itself does not fail).
    async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome;

    /// The catalog used to map a call's positional args onto the op's named parameters, and to look
    /// up op metadata. Resolution is never advertised-filtered (a pre-authored flow may name any op).
    fn catalog(&self) -> &dyn OpCatalog;

    /// Request human approval for an explicit `confirm` gate. `label` is the human-readable prompt.
    async fn request_approval(&self, label: &str, intents: &IntentSet) -> ApprovalChoice;

    /// Trim an op's model-facing view to the host's output budget — applied when building the
    /// round transcript so one huge result can't blow the context budget.
    fn trim_output(&self, view: String, op: &str) -> String;

    /// Resolve an external [`ThingRef`] to an exact identity. The default resolves only the
    /// **unambiguous, self-identifying** selectors deterministically ([`default_resolve_thing`]); a
    /// host overrides this to consult a real resolver (search / directory lookup) for `Name`/`Query`
    /// selectors. The error string surfaces to the flow as a runtime error.
    async fn resolve_thing(&self, thing: &ThingRef) -> std::result::Result<ResolvedThing, String> {
        default_resolve_thing(thing)
    }
}

/// Deterministically resolve the **self-identifying** thing selectors — an explicit `Id`/`Key`, a
/// `File` by `Path`, or a `Url` — to a [`ResolvedThing`] with full confidence and no IO. `Name`/`Query`
/// selectors (which require a search or directory lookup) return an error so a host resolver can take
/// over. This is the PRD §9.1 "deterministic resolver" floor: the same `ThingRef` always resolves to
/// the same identity, runnable before any side effect.
pub fn default_resolve_thing(thing: &ThingRef) -> std::result::Result<ResolvedThing, String> {
    let id = match (&thing.kind, &thing.selector) {
        (_, Selector::Id(s)) | (_, Selector::Key(s)) => s.clone(),
        (crate::ast::ThingKind::File, Selector::Path(p)) => p.clone(),
        (crate::ast::ThingKind::Url, Selector::Path(u))
        | (crate::ast::ThingKind::Url, Selector::Name(u)) => u.clone(),
        _ => {
            return Err(format!(
                "no deterministic resolution for a {:?} by {:?} — a Name/Query selector needs a host resolver",
                thing.kind, thing.selector
            ));
        }
    };
    Ok(ResolvedThing {
        id: ThingId(id.clone()),
        kind: thing.kind.clone(),
        display: id,
        source: "deterministic".to_string(),
        confidence: 1.0,
    })
}
