use super::*;

// ── `flux a2a` — remote A2A agent client ───────────────────────────────────────

/// `~/.flux/a2a-history.txt` — separate from the main REPL history.
pub(super) fn a2a_history_path() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".flux");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("a2a-history.txt"))
}

/// The `a2a › ` prompt for the remote-agent REPL.
pub(super) struct A2aPrompt;
impl Prompt for A2aPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("a2a › ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(&self, _s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(reverse-search) ")
    }
}

/// A thin markdown renderer for the remote agent's reply, mirroring `CliSink`'s live rendering.
///
/// It tracks everything rendered so far so it can absorb either streaming convention transparently:
/// agents that send incremental **deltas** and agents that send cumulative **snapshots** (each event
/// is the full text so far). [`A2aRender::push_message`] pushes only the new suffix in the snapshot
/// case, so neither double-renders.
pub(super) struct A2aRender {
    pub(super) live: flux_markdown::render::LiveRenderer,
    pub(super) rendered: String,
}

impl A2aRender {
    fn new() -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
            .filter(|&w| w >= 20)
            .unwrap_or(80);
        A2aRender {
            live: flux_markdown::render::LiveRenderer::new(
                flux_markdown::render::Theme::auto(),
                width,
                stdout_tty,
            ),
            rendered: String::new(),
        }
    }
    /// Append `t` to the live render and to the running record.
    fn push(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = self.live.push(t, &mut out);
        drop(out);
        self.rendered.push_str(t);
    }
    /// Render an agent message whose text may be a **delta** or a cumulative **snapshot**. If it
    /// extends what we've already shown, push only the new tail; otherwise push it as a fresh delta.
    fn push_message(&mut self, t: &str) {
        let suffix = new_render_suffix(&self.rendered, t);
        self.push(suffix);
    }
    /// True if anything has been rendered this turn.
    fn has_output(&self) -> bool {
        !self.rendered.is_empty()
    }
    fn finish(&mut self) {
        if self.live.is_active() {
            let mut out = std::io::stdout().lock();
            let _ = self.live.finish(&mut out);
        }
    }
}

/// What to actually render for an incoming agent message, given what's already on screen: the new
/// tail if `incoming` is a cumulative snapshot that extends `rendered`, else the whole `incoming`
/// (a delta). One code path then absorbs both streaming conventions without double-rendering.
pub(super) fn new_render_suffix<'a>(rendered: &str, incoming: &'a str) -> &'a str {
    incoming.strip_prefix(rendered).unwrap_or(incoming)
}

/// Render one streaming event. Status-update / message text is fed through [`A2aRender::push_message`]
/// so delta- and snapshot-style agents both render correctly. Returns `true` once the stream's
/// final/terminal event arrives.
pub(super) fn handle_a2a_event(ev: flux_a2a::StreamEvent, render: &mut A2aRender) -> bool {
    use flux_a2a::StreamEvent;
    match ev {
        StreamEvent::StatusUpdate(u) => {
            if let Some(m) = &u.status.message {
                render.push_message(&m.text());
            }
            u.is_final
        }
        StreamEvent::Message(m) => {
            render.push_message(&m.text());
            false
        }
        StreamEvent::Task(t) => {
            // A terminal Task on the stream: if nothing streamed, render its text once.
            if !render.has_output() {
                render.push_message(&t.final_text());
            }
            t.status.state.is_terminal()
        }
        StreamEvent::ArtifactUpdate(a) => {
            for p in &a.artifact.parts {
                if let Some(s) = p.as_text() {
                    render.push(s);
                }
            }
            false
        }
    }
}

/// Run one A2A turn: send `text` as a single task and render the remote agent's reply.
pub(super) async fn a2a_turn(
    client: &flux_a2a::A2aClient,
    context_id: &str,
    text: &str,
    streaming: bool,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let msg = flux_a2a::Message::user_text(text, Some(context_id.to_string()));
    if streaming {
        let mut stream = match client.stream(msg).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{} {e}", style::red("error:"));
                return;
            }
        };
        let mut render = A2aRender::new();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    render.finish();
                    eprintln!("{}", style::dim("(cancelled)"));
                    return;
                }
                next = stream.next() => {
                    match next {
                        Some(Ok(ev)) => {
                            if handle_a2a_event(ev, &mut render) {
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            render.finish();
                            eprintln!("{} {e}", style::red("error:"));
                            return;
                        }
                        None => break,
                    }
                }
            }
        }
        render.finish();
        if !render.has_output() {
            eprintln!("{}", style::dim("(no output)"));
        }
    } else {
        // Non-streaming: blocking send, polling `tasks/get` if a general agent answers with a
        // still-running task.
        let outcome = match client.send(msg, true).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{} {e}", style::red("error:"));
                return;
            }
        };
        let reply = match outcome.as_task() {
            Some(t) if !t.status.state.is_terminal() => {
                match client
                    .await_task(&t.id, std::time::Duration::from_millis(700), 120)
                    .await
                {
                    Ok(done) => done.final_text(),
                    Err(e) => {
                        eprintln!("{} {e}", style::red("error:"));
                        return;
                    }
                }
            }
            _ => outcome.final_text(),
        };
        if reply.trim().is_empty() {
            eprintln!("{}", style::dim("(no output)"));
            return;
        }
        let mut render = A2aRender::new();
        render.push(&reply);
        render.finish();
    }
}

/// `flux a2a <URL>` — connect to a remote A2A agent and drive it from the CLI like a local agent.
/// (`token` already carries the `FLUX_A2A_TOKEN` fallback — clap owns that env wiring.)
pub(super) async fn run_a2a(
    url: String,
    prompt_words: Vec<String>,
    token: Option<String>,
) -> Result<()> {
    let mut client = flux_a2a::A2aClient::new(&url)
        .map_err(|e| anyhow::anyhow!("invalid a2a url `{url}`: {e}"))?
        .with_token(token);

    // Discover the remote agent (best-effort): its name + whether it streams. If the card can't be
    // fetched we fall back to non-streaming `message/send` — the lowest-common-denominator that
    // returns a clear result/error, rather than risking a silent non-SSE response.
    let mut streaming = false;
    match client.fetch_agent_card().await {
        Ok(card) => {
            streaming = card.capabilities.streaming;
            let name = if card.name.is_empty() {
                "a2a agent"
            } else {
                card.name.as_str()
            };
            let ver = if card.version.is_empty() {
                String::new()
            } else {
                format!(" v{}", card.version)
            };
            eprintln!(
                "{}",
                style::dim(&format!("connected → {name}{ver} · {}", client.rpc_url()))
            );
            let desc = card.description.lines().next().unwrap_or("").trim();
            if !desc.is_empty() {
                eprintln!("{}", style::dim(desc));
            }
        }
        Err(e) => eprintln!(
            "{}",
            style::dim(&format!(
                "(no agent card: {e}; using non-streaming message/send) → {}",
                client.rpc_url()
            ))
        ),
    }

    // One stable conversation context for this session (forward-compatible with stateful remotes).
    let context_id = flux_a2a::new_id();

    // One-shot when given prompt words, or when stdin is piped (not a TTY).
    let piped = !std::io::stdin().is_terminal();
    if !prompt_words.is_empty() || piped {
        let prompt = if !prompt_words.is_empty() {
            prompt_words.join(" ")
        } else {
            // Read piped stdin off the runtime thread (the codebase convention — see
            // `read_stdin_line`): a fifo that never closes must not park a worker forever.
            tokio::task::spawn_blocking(|| {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map(|_| buf)
            })
            .await
            .context("stdin reader task")??
            .trim()
            .to_string()
        };
        if prompt.is_empty() {
            return Ok(());
        }
        let client_ref = &client;
        let ctx_ref = context_id.as_str();
        let prompt_ref = prompt.as_str();
        run_interruptible(move |c| async move {
            a2a_turn(client_ref, ctx_ref, prompt_ref, streaming, &c).await;
        })
        .await;
        return Ok(());
    }

    // Interactive REPL.
    eprintln!(
        "{}",
        style::dim("a2a chat — /help, Ctrl-C interrupts a turn, Ctrl-D exits")
    );
    let history: Box<dyn reedline::History> = match a2a_history_path() {
        Some(p) => Box::new(
            FileBackedHistory::with_file(1000, p)
                .unwrap_or_else(|_| FileBackedHistory::new(1000).expect("in-memory history")),
        ),
        None => Box::new(FileBackedHistory::new(1000).expect("in-memory history")),
    };
    let mut editor = Reedline::create().with_history(history);
    loop {
        let line = match editor.read_line(&A2aPrompt) {
            Ok(Signal::Success(buf)) => buf,
            Ok(Signal::CtrlC) => continue,
            Ok(Signal::CtrlD) => break,
            Ok(_) => continue,
            Err(_) => break,
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if let Some(rest) = input.strip_prefix('/') {
            match rest.split_whitespace().next().unwrap_or("") {
                "exit" | "quit" => break,
                "help" => {
                    eprintln!("a2a REPL commands:");
                    eprintln!("  /card   show the remote agent card");
                    eprintln!("  /exit   quit");
                    eprintln!("  Ctrl-C  interrupt a running turn   Ctrl-D  exit");
                }
                "card" => match client.fetch_agent_card().await {
                    Ok(card) => {
                        if let Ok(s) = serde_json::to_string_pretty(&card) {
                            println!("{s}");
                        }
                    }
                    Err(e) => eprintln!("{} {e}", style::red("error:")),
                },
                other => eprintln!(
                    "{}",
                    style::dim(&format!("(unknown command /{other} — try /help)"))
                ),
            }
            continue;
        }
        let client_ref = &client;
        let ctx_ref = context_id.as_str();
        run_interruptible(move |c| async move {
            a2a_turn(client_ref, ctx_ref, input, streaming, &c).await;
        })
        .await;
    }
    Ok(())
}
