//! The channel host: [`serve`] runs a program's channels against a live [`App`] until shutdown.

use std::sync::Arc;

use serde_json::json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use flux_app::App;
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
        set.spawn(async move { stdin_loop(d, c).await });
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
