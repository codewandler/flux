//! Channel adapters (one per `kind`) and the [`build_channels`] dispatcher.

mod schedule;
#[cfg(feature = "slack")]
mod slack;
mod webhook;

pub use schedule::ScheduleChannel;
#[cfg(feature = "slack")]
pub use slack::SlackChannel;
pub use webhook::WebhookChannel;

use flux_lang::program::ChannelDecl;

use crate::Channel;

/// Build the long-running channels declared by a program. The in-process `cli` channel is skipped here
/// (it is served by the host's stdin loop, not as a background task); an unknown `kind` is an error.
pub fn build_channels(decls: &[ChannelDecl]) -> anyhow::Result<Vec<Box<dyn Channel>>> {
    let mut out: Vec<Box<dyn Channel>> = Vec::new();
    for d in decls {
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
            other => anyhow::bail!("unknown channel kind `{other}` for channel `{}`", d.name),
        }
    }
    Ok(out)
}
