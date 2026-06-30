//! Channel adapters (one per `kind`) and the [`build_channels`] dispatcher.

mod a2a;
mod schedule;
#[cfg(feature = "slack")]
mod slack;
mod webhook;

pub use a2a::A2aChannel;
pub use schedule::ScheduleChannel;
#[cfg(feature = "slack")]
pub use slack::SlackChannel;
pub use webhook::WebhookChannel;

use flux_lang::program::{as_secret_ref, ChannelDecl};
use serde_json::Value;

use crate::Channel;

/// The env-var name of the first unresolved `{"$secret":…}` marker anywhere in `v`, if any. Secrets must
/// be resolved (`flux_app::resolve_secrets`) before a channel's settings are deserialized — this catches
/// a caller that skipped that step, turning an opaque "expected a string" serde error into a clear one.
fn first_unresolved_secret(v: &Value) -> Option<&str> {
    if let Some(name) = as_secret_ref(v) {
        return Some(name);
    }
    match v {
        Value::Object(m) => m.values().find_map(first_unresolved_secret),
        Value::Array(a) => a.iter().find_map(first_unresolved_secret),
        _ => None,
    }
}

/// Build the long-running channels declared by a program. The in-process `cli` channel is skipped here
/// (it is served by the host's stdin loop, not as a background task); an unknown `kind` is an error.
pub fn build_channels(decls: &[ChannelDecl]) -> anyhow::Result<Vec<Box<dyn Channel>>> {
    let mut out: Vec<Box<dyn Channel>> = Vec::new();
    for d in decls {
        if let Some(name) = first_unresolved_secret(&d.settings) {
            anyhow::bail!(
                "channel `{}` has an unresolved secret reference `{name}` — resolve secrets \
                 (flux_app::resolve_secrets) before building channels",
                d.name
            );
        }
        match d.kind.as_str() {
            "schedule" | "cron" => out.push(Box::new(ScheduleChannel::from_decl(d)?)),
            "webhook" | "http" => out.push(Box::new(WebhookChannel::from_decl(d)?)),
            "slack" => {
                #[cfg(feature = "slack")]
                out.push(Box::new(SlackChannel::from_decl(d)?));
                #[cfg(not(feature = "slack"))]
                anyhow::bail!(
                    "channel `{}` has kind `slack` — rebuild with `--features slack`",
                    d.name
                );
            }
            "cli" => { /* served by the host's stdin loop, not a background channel */ }
            // `a2a` is built by the host ([`crate::serve`]), which can resolve the target agent's
            // engine from the live `App` — it cannot be constructed from the decl alone.
            "a2a" => { /* built by the host, not here */ }
            other => anyhow::bail!("unknown channel kind `{other}` for channel `{}`", d.name),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_channels_rejects_an_unresolved_secret_marker() {
        // A caller that forgot to resolve secrets gets a clear, actionable error (not an opaque serde
        // "expected a string") — secrets must be host-resolved before channels are built.
        let decls = vec![ChannelDecl {
            name: "wh".into(),
            kind: "webhook".into(),
            settings: json!({ "addr": "127.0.0.1:8799", "token": { "$secret": "WEBHOOK_TOKEN" } }),
        }];
        let err = match build_channels(&decls) {
            Ok(_) => panic!("expected an unresolved-secret error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("unresolved secret"), "clear error: {err}");
        assert!(err.contains("WEBHOOK_TOKEN"), "names the var: {err}");
    }
}
