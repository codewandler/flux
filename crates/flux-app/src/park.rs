//! Reply-parking for the `ask` op (A-11): a journey that `ask`s a channel **suspends** on the flow
//! engine's existing suspension seam and is resumed by [`crate::App::deliver`] when the correlated
//! reply arrives — the reply text becomes the ask's bound result.
//!
//! ## How `ask` suspends (the lowering)
//! `ask` is an ordinary registry op, and an op result cannot suspend a flow. So before the host
//! executes a journey it rewrites the flow body ([`rewrite_asks`]): every **top-level** `ask` call
//! is lowered to the same call (unbound — it still records the expects-reply message and prints the
//! question on a `cli` channel) followed by a top-level `await` whose binding is the ask's original
//! bind symbol and whose `source` is the [`ASK_REPLY_SOURCE`] marker. The interpreter then suspends
//! at that `await` exactly as it would for a hand-written one (`FlowOutcome::suspension`), the host
//! parks it via [`flux_flow::state::FlowStore::save_suspension`], and the resume re-enters through
//! `flux_flow::runtime::resume_flow*` over a full-envelope executor — no side-channel execution of
//! flow bodies anywhere.
//!
//! Only **top-level** asks park: `await` is a top-level-only statement in flux-lang (the analyzer
//! enforces it — nested bodies can never suspend), so an `ask` nested inside
//! `when`/`repeat`/`each`/`seq`/`parallel` keeps the fire-and-forget behavior (it returns
//! `ask:<channel>` immediately).
//!
//! ## The correlation rule (documented choice)
//! A park is keyed by the **asked channel**, resolved at park time from the bus's recorded
//! expects-reply sends (runtime truth — a dynamically-computed channel name works too). An inbound
//! event resumes the *oldest* pending park it [`event_correlates`] with:
//!
//! - the event label equals the asked channel's name (real channels deliver under their name), or
//! - the event label is `user_input` and the asked channel renders to the CLI — the interactive
//!   stdin loop of `flux app run` delivers each line as a `user_input` event, so the printed
//!   question plus the next typed line resolve the ask.
//!
//! A correlated event is **consumed** by the park: it resumes that journey instead of routing
//! through triggers (otherwise the reply line would also start a fresh journey). Uncorrelated
//! events (other labels/channels) route normally and leave every park alone. There is no explicit
//! correlation-id matching: channels deliver plain messages (a stdin line, a chat post) with no
//! reliable place to carry an id, so first-message-on-the-asked-channel is the rule.

use std::sync::Arc;

use flux_flow::state::FlowStore;
use flux_lang::ast::{Node, SymbolName};
use flux_lang::program::ChannelDecl;

use crate::ops::is_cli_channel;

/// The `await` source marker the [`rewrite_asks`] lowering stamps on an ask's suspension point, so
/// the host can tell an ask-park from a hand-written `await` (which it leaves alone).
pub(crate) const ASK_REPLY_SOURCE: &str = "ask.reply";

/// The event label the interactive stdin loop (`flux app run`) delivers lines under — see
/// `flux-channels`' `stdin_loop`. A park on a CLI-rendered channel treats this label as its channel.
const CLI_INPUT_LABEL: &str = "user_input";

/// A journey parked on an `ask`, waiting for the correlated reply.
pub(crate) struct ParkedAsk {
    /// The asked channel — the correlation key (see the module docs for the rule).
    pub(crate) channel: String,
    /// The journey name (reported on the resumed [`crate::JourneyRun`]).
    pub(crate) journey: String,
    /// The run's session id — symbols and the persisted suspension live under it.
    pub(crate) session_id: String,
    /// The run's private store: it holds the executed prefix's bound symbols **and** the persisted
    /// suspension (`FlowStore` suspensions) this park resumes from.
    pub(crate) store: Arc<FlowStore>,
    /// Ops dispatched before the park (prior segments); the resumed run's count adds to this.
    pub(crate) steps: usize,
}

/// True when a `label`-tagged inbound event is the reply a park on `asked` channel waits for:
/// the label is the channel's name, or it is the stdin loop's `user_input` and the channel renders
/// to the CLI. (The full rule, including consumption semantics, is documented on the module.)
pub(crate) fn event_correlates(label: &str, channels: &[ChannelDecl], asked: &str) -> bool {
    label == asked || (label == CLI_INPUT_LABEL && is_cli_channel(channels, asked))
}

/// Lower every **top-level** `ask` call into `ask` (unbound — sends the question) + `await` (the
/// suspension point, binding the reply under the ask's original symbol). Non-top-level nodes are
/// untouched — nested asks keep the fire-and-forget behavior, because only a top-level `await` can
/// suspend a flow.
pub(crate) fn rewrite_asks(body: Vec<Node>) -> Vec<Node> {
    let mut out = Vec::with_capacity(body.len());
    for node in body {
        match node {
            // `$reply = ask({..})` → the unbound ask call, then `await` binding `$reply`. The
            // bind's declared type rides along as the await's `as_type` (the resume coerces the
            // reply against it); the ask's own return (the `ask:<channel>` correlation id) is
            // discarded — the symbol only ever holds the reply text.
            Node::Bind {
                name, value, ty, ..
            } if is_ask_call(&value) => {
                out.push(*value);
                out.push(ask_await(Some(name), ty));
            }
            // A bare top-level `ask({..})` still expects a reply — park, discard the reply text.
            call @ Node::Call { .. } if is_ask_call(&call) => {
                out.push(call);
                out.push(ask_await(None, None));
            }
            other => out.push(other),
        }
    }
    out
}

/// The suspension point the lowering plants right after an ask call.
fn ask_await(binding: Option<SymbolName>, as_type: Option<flux_lang::ast::TypeRef>) -> Node {
    Node::Await {
        binding,
        source: ASK_REPLY_SOURCE.to_string(),
        as_type,
    }
}

/// Is this node a call to the `ask` op?
fn is_ask_call(node: &Node) -> bool {
    matches!(node, Node::Call { op, .. } if op == "ask")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask_call() -> Node {
        Node::Call {
            op: "ask".into(),
            args: vec![Node::Lit {
                value: json!({ "channel": "cli", "message": "q?" }),
            }],
        }
    }

    #[test]
    fn top_level_bound_ask_lowers_to_call_plus_await() {
        let body = vec![Node::Bind {
            name: SymbolName("reply".into()),
            value: Box::new(ask_call()),
            ty: None,
            effect: None,
        }];
        let out = rewrite_asks(body);
        assert_eq!(out.len(), 2);
        assert!(is_ask_call(&out[0]), "the send half stays a plain call");
        match &out[1] {
            Node::Await {
                binding, source, ..
            } => {
                assert_eq!(binding.as_ref().map(|s| s.0.as_str()), Some("reply"));
                assert_eq!(source, ASK_REPLY_SOURCE);
            }
            other => panic!("expected an await, got {other:?}"),
        }
    }

    #[test]
    fn nested_ask_is_left_alone() {
        // An ask inside a `when` body cannot suspend (await is top-level-only) — not rewritten.
        let body = vec![Node::When {
            cond: Box::new(Node::Lit { value: json!(true) }),
            then: vec![ask_call()],
            otherwise: vec![],
        }];
        let out = rewrite_asks(body.clone());
        assert_eq!(out, body);
    }

    #[test]
    fn correlation_matches_channel_name_and_cli_user_input() {
        let channels = vec![ChannelDecl {
            name: "cli".into(),
            kind: "cli".into(),
            settings: json!(null),
        }];
        // A real channel's events arrive under its name.
        assert!(event_correlates("cli", &channels, "cli"));
        // The stdin loop delivers lines as `user_input` — a CLI-rendered park matches them.
        assert!(event_correlates("user_input", &channels, "cli"));
        // Other labels leave the park alone.
        assert!(!event_correlates("ping", &channels, "cli"));
        // `user_input` does not resolve a park on a non-CLI channel.
        let slack = vec![ChannelDecl {
            name: "team".into(),
            kind: "slack".into(),
            settings: json!(null),
        }];
        assert!(!event_correlates("user_input", &slack, "team"));
        assert!(event_correlates("team", &slack, "team"));
    }
}
