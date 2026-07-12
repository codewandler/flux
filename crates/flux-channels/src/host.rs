//! The channel host: [`serve`] runs a program's channels against a live [`App`] until shutdown.

use std::io::IsTerminal;
use std::sync::Arc;

use serde_json::json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use flux_app::{App, JourneyRun};
use flux_lang::program::ChannelDecl;

use crate::{A2aChannel, AppDeliverer, Channel, Deliverer};

/// Run `channels` against `app` until Ctrl-C (or `cancel`). Fires the one-shot `startup` event first,
/// then spawns each channel; when `run_stdin` is set, also serves the interactive `cli` stdin loop
/// (read a line → deliver it as `user_input` → print the triggered journeys' results).
///
/// Returns when Ctrl-C / `cancel` fires, when a channel task ends or errors, or — if there is nothing to
/// wait on (no channels and no stdin) — right after `startup`.
pub async fn serve(
    app: Arc<App>,
    channels: Vec<Box<dyn Channel>>,
    run_stdin: bool,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // `a2a` channels serve an agent over HTTP/A2A and need the live `App` to resolve the target agent's
    // engine, so they are built here rather than in the decl-only `build_channels`. Collect the decls
    // first (ending the `program()` borrow) so `app` is free to move into the deliverer below.
    let a2a_decls: Vec<ChannelDecl> = app
        .program()
        .channels
        .iter()
        .filter(|c| c.kind == "a2a")
        .cloned()
        .collect();
    let mut a2a_channels: Vec<Box<dyn Channel>> = Vec::with_capacity(a2a_decls.len());
    for decl in &a2a_decls {
        a2a_channels.push(Box::new(A2aChannel::from_decl_and_app(decl, &app).await?));
    }

    let deliverer: Arc<dyn Deliverer> = Arc::new(AppDeliverer::new(app));

    // Fire the one-shot startup event before any channel events, so `{on:"startup"}` triggers run first.
    deliverer.deliver("startup", json!({})).await?;

    let mut set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    for ch in channels.into_iter().chain(a2a_channels) {
        let d = deliverer.clone();
        let c = cancel.clone();
        set.spawn(async move { ch.start(d, c).await });
    }
    if run_stdin {
        let d = deliverer.clone();
        let c = cancel.clone();
        // C-33: loaded once here (the same builtin-table + `~/.flux/pricing.toml` overlay every
        // other cost sink uses — flux-cli's turn-end annotation, `flux usage`) rather than per
        // completed run, so `flux app run`'s interactive console pays that IO cost once per process.
        let pricing = Arc::new(flux_credentials::load_pricing_table());
        set.spawn(async move { stdin_loop(d, c, pricing).await });
    }

    // Run until Ctrl-C / external cancel, until a channel *errors* (fatal), or until every channel has
    // finished on its own. A channel that ends normally (e.g. a one-shot `startup` schedule) must NOT
    // tear down the others, so a normal completion just continues the loop.
    let shutdown = flux_server::shutdown_signal();
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            _ = cancel.cancelled() => break Ok(()),
            joined = set.join_next() => match joined {
                Some(Ok(Ok(()))) => continue,                 // a channel ended normally; keep the rest
                Some(Ok(Err(e))) => break Err(e),             // fatal channel error
                Some(Err(e)) => break Err(anyhow::anyhow!("channel task panicked: {e}")),
                None => break Ok(()),                         // all channels finished
            },
        }
    };

    // Tell every channel to stop, then drain in-flight tasks.
    cancel.cancel();
    while set.join_next().await.is_some() {}
    result
}

/// The interactive `cli` channel: read stdin line by line, deliver each as a `user_input` event, and
/// print every triggered journey's non-empty result. Returns on EOF or cancellation.
///
/// C-33: this is the one place `flux app run` has an **operator console** — a local terminal, not a
/// channel-facing reply — so each completed run also gets a dim STDERR line carrying its cost
/// annotation (see [`cost_line`]). `run.result` on stdout is untouched: a piped consumer or a
/// downstream channel adapter (Slack, webhook — see their own modules) must see exactly the
/// journey's own output, never a dollar suffix appended to it.
async fn stdin_loop(
    d: Arc<dyn Deliverer>,
    cancel: CancellationToken,
    pricing: Arc<flux_core::PricingTable>,
) -> anyhow::Result<()> {
    // `tokio::io::stdin()` delegates blocking terminal reads to a runtime worker. Such a read cannot
    // be cancelled, and Tokio waits for the worker during runtime shutdown — so Ctrl-C hung forever
    // while an interactive terminal remained open. A detached standard thread may stay blocked after
    // cancellation, but it is not owned by the Tokio runtime and therefore cannot hold process exit.
    let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("flux-stdin".to_string())
        .spawn(move || {
            use std::io::BufRead;

            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            line = lines.recv() => match line {
                Some(Ok(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let runs = d.deliver("user_input", json!({ "text": line })).await?;
                    for run in runs {
                        if !run.result.trim().is_empty() {
                            println!("{}", run.result);
                        }
                        eprintln!("{}", dim(&cost_line(&run, &pricing)));
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => break, // EOF or the reader thread ended
            },
        }
    }
    Ok(())
}

/// One operator-console STDERR line for a completed [`JourneyRun`] (C-33): the journey name, its op
/// count, and — when the run drove a priced turn — the same cost contract every other turn-end sink
/// renders (see [`cost_suffix`]). Never reaches a channel-facing payload; see [`stdin_loop`]'s doc.
fn cost_line(run: &JourneyRun, pricing: &flux_core::PricingTable) -> String {
    format!(
        "· {} — {} step(s){}",
        run.journey,
        run.steps,
        cost_suffix(run, pricing)
    )
}

/// The dollar-cost suffix for a completed run's console line (C-33) — mirrors flux-cli's
/// `cost_suffix`/`cost_annotation` rules exactly (that function is crate-private to flux-cli, and
/// flux-channels can't depend on a sibling L6 crate, so this is a small, deliberate duplicate rather
/// than a shared dependency): priced → ` · $X.XXXX` (subscription spend prefixed `~`/suffixed
/// `(sub)`, since it's an equivalent metered figure, not a charge); a metered-cloud pricing-table
/// miss (real spend, no row — see [`flux_core::is_metered_cloud_spec`]) → ` · $? (unpriced)`; no
/// usage at all (a pure-op journey, or a local/mock model) → empty.
fn cost_suffix(run: &JourneyRun, pricing: &flux_core::PricingTable) -> String {
    let Some(usage) = &run.usage else {
        return String::new();
    };
    match pricing.cost(usage, &run.model) {
        Some(money) if money.usd > 0.0 => {
            let usd = format!("${:.4}", money.usd);
            if money.subscription {
                format!(" · ~{usd} (sub)")
            } else {
                format!(" · {usd}")
            }
        }
        Some(_) => String::new(),
        None if flux_core::is_metered_cloud_spec(&run.model) => " · $? (unpriced)".to_string(),
        None => String::new(),
    }
}

/// A dim ANSI-styled line for STDERR-only operator diagnostics (C-33) — the spirit of flux-cli's
/// `style::dim`, but this crate carries no `--color` flag to gate a persistent toggle on, so it
/// degrades on `NO_COLOR` or a non-tty stderr only.
fn dim(s: &str) -> String {
    if std::env::var_os("NO_COLOR").is_some() || !std::io::stderr().is_terminal() {
        s.to_string()
    } else {
        format!("\x1b[2m{s}\x1b[0m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Usage;

    fn make_run(model: &str, usage: Option<Usage>) -> JourneyRun {
        JourneyRun {
            journey: "j".to_string(),
            result: String::new(),
            steps: 2,
            usage,
            model: model.to_string(),
        }
    }

    /// C-33's named failing-first test for the flux-channels annotation formatter: a priced turn
    /// renders the same `$X.XXXX` figure `flux-cli`'s turn-end suffix would.
    #[test]
    fn priced_turn_renders_the_dollar_figure() {
        let pricing = flux_core::PricingTable::builtin();
        let run = make_run(
            "anthropic/claude-sonnet-4-6",
            Some(Usage {
                input_tokens: 1_000_000,
                output_tokens: 100_000,
                ..Default::default()
            }),
        );
        // cost = 1·3 + 0.1·15 = 3 + 1.5 = 4.5
        assert_eq!(cost_suffix(&run, &pricing), " · $4.5000");
        assert_eq!(cost_line(&run, &pricing), "· j — 2 step(s) · $4.5000");
    }

    /// A metered-cloud spec with no pricing-table row (and no provider-reported cost) is real,
    /// invisible spend — the `$?` marker, not silence.
    #[test]
    fn metered_cloud_table_miss_renders_the_unpriced_marker() {
        let pricing = flux_core::PricingTable::builtin();
        let run = make_run(
            "anthropic/claude-nonexistent-model",
            Some(Usage {
                input_tokens: 1_000,
                output_tokens: 500,
                ..Default::default()
            }),
        );
        assert_eq!(cost_suffix(&run, &pricing), " · $? (unpriced)");
    }

    /// A local/mock spec renders no cost segment at all — nothing is billed there, so a table miss
    /// must stay silent (never the `$?` marker).
    #[test]
    fn local_spec_renders_no_cost_segment() {
        let pricing = flux_core::PricingTable::builtin();
        let run = make_run(
            "ollama/llama3",
            Some(Usage {
                input_tokens: 1_000,
                output_tokens: 500,
                ..Default::default()
            }),
        );
        assert_eq!(cost_suffix(&run, &pricing), "");

        let mock_run = make_run(
            "mock",
            Some(Usage {
                input_tokens: 1_000,
                output_tokens: 500,
                ..Default::default()
            }),
        );
        assert_eq!(cost_suffix(&mock_run, &pricing), "");
    }

    /// A pure-op journey (no model call) carries no usage at all — the console line names the
    /// journey with no cost segment, never a stray marker.
    #[test]
    fn no_usage_renders_no_cost_segment() {
        let pricing = flux_core::PricingTable::builtin();
        let run = make_run("anthropic/claude-sonnet-4-6", None);
        assert_eq!(cost_suffix(&run, &pricing), "");
        assert_eq!(cost_line(&run, &pricing), "· j — 2 step(s)");
    }
}
