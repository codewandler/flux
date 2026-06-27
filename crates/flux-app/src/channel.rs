//! Channel entry points: the thin I/O wrappers that bridge the outside world to the bus. Today only
//! the `cli` channel (stdin → `user_input` events, journey output → stdout) is implemented; an HTTP or
//! Slack channel would be another such wrapper over [`App::deliver`].

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

use flux_core::{Error, Result};
use flux_lang::program::{Module, Program};
use flux_provider::Provider;

use crate::App;

/// Load a `.flux` program file, build an [`App`], fire `startup`, then serve the stdin `cli` channel.
/// This is the function a `flux run app.flux` CLI command calls; the CLI wiring itself lives elsewhere.
///
/// A bare single-flow file (not a program) is accepted too — it loads as a top-level flow with no
/// triggers, so nothing fires automatically; that case is mainly for symmetry with the loader.
pub async fn run_program_file(
    path: &Path,
    provider: Option<Arc<dyn Provider>>,
    model: impl Into<String>,
    auto_approve: bool,
) -> Result<()> {
    let src = std::fs::read_to_string(path)?;
    let program = match Module::parse_str(&src).map_err(|e| Error::Other(e.to_string()))? {
        Module::Program(p) => p,
        Module::Flow(flow) => Program {
            flows: vec![flow],
            ..Default::default()
        },
    };
    let app = App::with_options(program, provider, model, auto_approve);
    // Fire the one-shot startup event so any `{on:"startup"}` trigger runs before we read input.
    app.deliver("startup", json!({})).await?;
    run_stdin(app).await
}

/// The `cli` channel loop: read stdin line by line, deliver each line as a `user_input` event
/// (`{"text": <line>}`), and print every triggered journey's result to stdout. Returns on EOF.
pub async fn run_stdin(app: App) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let runs = app.deliver("user_input", json!({ "text": line })).await?;
        for run in runs {
            if !run.result.trim().is_empty() {
                println!("{}", run.result);
            }
        }
    }
    Ok(())
}
