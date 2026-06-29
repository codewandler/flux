//! The channel host: [`serve`] runs a program's channels against a live [`App`] until shutdown.

use std::sync::Arc;

use serde_json::json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use flux_app::App;

use crate::{AppDeliverer, Channel, Deliverer};

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
    let deliverer: Arc<dyn Deliverer> = Arc::new(AppDeliverer::new(app));

    // Fire the one-shot startup event before any channel events, so `{on:"startup"}` triggers run first.
    deliverer.deliver("startup", json!({})).await?;

    let mut set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    for ch in channels {
        let d = deliverer.clone();
        let c = cancel.clone();
        set.spawn(async move { ch.start(d, c).await });
    }
    if run_stdin {
        let d = deliverer.clone();
        let c = cancel.clone();
        set.spawn(async move { stdin_loop(d, c).await });
    }

    // Run until Ctrl-C / external cancel, until a channel *errors* (fatal), or until every channel has
    // finished on its own. A channel that ends normally (e.g. a one-shot `startup` schedule) must NOT
    // tear down the others, so a normal completion just continues the loop.
    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
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
async fn stdin_loop(d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            line = lines.next_line() => match line? {
                Some(line) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let runs = d.deliver("user_input", json!({ "text": line })).await?;
                    for run in runs {
                        if !run.result.trim().is_empty() {
                            println!("{}", run.result);
                        }
                    }
                }
                None => break, // EOF
            },
        }
    }
    Ok(())
}
