use super::*;

// ── `flux tui --attach` — attaching the TUI to an agent that lives on a host (C-686) ───────────
//
// The glue between two crates that cannot see each other: `flux-tui` (L6) declares a protocol-free
// [`flux_tui::attach::AttachedAgent`], and `flux-a2a` (L1) implements the protocol. This file is the
// one place both are visible, so it owns the selection (URL or named binding), the credential
// resolution (by reference — never argv) and the near-1:1 vocabulary translation.
//
// ⚠ This is the **agent** axis of `docs/designs/operating-a-deployed-host.md`, not the substrate
// axis. `--remote`/`--host` keep the agent here and move where its effects land; `--attach` moves
// the whole agent, including the approval stage. clap refuses them together, and this file never
// touches the execution-system selection.

/// How the operator named the agent to attach to, plus where its credential lives.
pub(super) struct AttachSelection {
    /// A served agent's URL, or the id of an `[[endpoint.static]]` binding.
    pub(super) target: String,
    /// The environment variable holding the bearer token for the URL form.
    pub(super) token_env: String,
    /// Continue this remote conversation instead of starting a new one.
    pub(super) context_id: Option<String>,
}

/// A resolved attach target: where to connect and the credential *value* pulled from its
/// reference. The only thing built from `token` is the client's `Authorization` header.
struct ResolvedAttachTarget {
    url: String,
    token: Option<String>,
    /// How the credential was located, for the one status line that mentions it. A *location*
    /// (`env/FLUX_A2A_TOKEN`), never a value.
    credential_source: String,
}

/// Non-leaking by construction, the same posture as `flux_secret`'s runtime endpoint form: the
/// token is reported as present-or-absent and never rendered, so no `{:?}` anywhere — a log line, a
/// panic message, an `expect_err` — can put a bearer credential on a terminal.
impl std::fmt::Debug for ResolvedAttachTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAttachTarget")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("credential_source", &self.credential_source)
            .finish()
    }
}

/// The wire protocol an `[[endpoint.static]]` binding must declare to be selectable as an agent.
///
/// Deliberately `[[endpoint.static]]` and **not** `[[host]]`: a `[[host]]` binding names an
/// execution *substrate* (`flux system serve` on the far side), and reusing it here would re-fuse
/// the two axes this feature exists to keep apart.
const AGENT_ENDPOINT_PROTOCOL: &str = "a2a";

/// Resolve `--attach <URL|NAME>` into an endpoint plus its bearer credential.
///
/// A value that parses as an `http`/`https` URL is used as-is, with the credential read from the
/// environment variable `--attach-token-env` names. Anything else is a binding id, looked up in the
/// merged `[[endpoint.static]]` declarations; its `credential_ref` is a *location* and is resolved
/// here the same way a `[[host]]` binding's is — `env` resolves, and any other scheme is refused by
/// name rather than silently ignored.
fn resolve_attach_target(selection: &AttachSelection) -> Result<ResolvedAttachTarget> {
    if selection.target.starts_with("http://") || selection.target.starts_with("https://") {
        // `user:pass@host` in the URL is a credential in argv — the one shape this flag exists to
        // avoid. Refused through the *same* predicate `flux endpoint add` and `[[endpoint.static]]`
        // use, so the three surfaces cannot disagree about what a credential-free URL is. The
        // message never echoes the value back.
        if url_has_userinfo(&selection.target) {
            bail!(
                "--attach must be a credential-free URL — put the bearer token in the \
                 environment variable named by --attach-token-env instead"
            );
        }
        let token = std::env::var(&selection.token_env)
            .ok()
            .filter(|value| !value.is_empty());
        return Ok(ResolvedAttachTarget {
            url: selection.target.clone(),
            token,
            credential_source: format!("env/{}", selection.token_env),
        });
    }
    resolve_named_agent_binding(&selection.target)
}

/// The named-binding half of [`resolve_attach_target`].
fn resolve_named_agent_binding(name: &str) -> Result<ResolvedAttachTarget> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let entry = cfg
        .endpoint
        .static_endpoints
        .iter()
        .find(|ep| ep.id == name)
        .ok_or_else(|| {
            let known: Vec<&str> = cfg
                .endpoint
                .static_endpoints
                .iter()
                .filter(|ep| ep.protocol.as_deref() == Some(AGENT_ENDPOINT_PROTOCOL))
                .map(|ep| ep.id.as_str())
                .collect();
            if known.is_empty() {
                anyhow::anyhow!(
                    "no agent binding `{name}`: it is not a URL and no [[endpoint.static]] with \
                     that id is declared. Declare one with `protocol = \"a2a\"`, or pass the \
                     served agent's URL."
                )
            } else {
                anyhow::anyhow!(
                    "no agent binding `{name}` — declared a2a bindings: {}",
                    known.join(", ")
                )
            }
        })?;
    if entry.protocol.as_deref() != Some(AGENT_ENDPOINT_PROTOCOL) {
        bail!(
            "endpoint binding `{name}` does not declare `protocol = \"a2a\"`, so it is not a \
             served agent. `--attach` selects an agent; `--host`/`--remote` select an execution \
             substrate."
        );
    }
    let (token, credential_source) = match entry.credential_ref.as_deref() {
        None => (None, "none (unauthenticated)".to_string()),
        Some(raw) => {
            let reference = flux_secret::Ref::parse(raw)
                .map_err(|e| anyhow::anyhow!("endpoint binding `{name}`: {e}"))?;
            if reference.scheme != flux_secret::Scheme::Env {
                bail!(
                    "endpoint binding `{name}`: only an env-scheme credential resolves for an \
                     agent attachment today (got `{reference}`)"
                );
            }
            let value = std::env::var(&reference.slot)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "endpoint binding `{name}`: credential `{reference}` unavailable — \
                         environment variable `{}` is unset or empty",
                        reference.slot
                    )
                })?;
            (Some(value), reference.to_string())
        }
    };
    Ok(ResolvedAttachTarget {
        url: entry.url.clone(),
        token,
        credential_source,
    })
}

/// The `flux-tui` driver backed by `flux_a2a::attach`.
///
/// Everything here is a translation between two vocabularies that were deliberately kept separate
/// by layering (see the file header). Both matches are exhaustive with no wildcard arm, so a new
/// protocol event cannot be silently dropped on the floor by this file.
struct A2aAttachment {
    agent: flux_a2a::attach::AttachedA2aAgent,
    label: String,
    capabilities: flux_tui::attach::AttachCapabilities,
}

fn availability(source: &flux_a2a::attach::Availability) -> flux_tui::attach::Availability {
    match source {
        flux_a2a::attach::Availability::Available => flux_tui::attach::Availability::Available,
        flux_a2a::attach::Availability::Unavailable(why) => {
            flux_tui::attach::Availability::Unavailable(why.clone())
        }
    }
}

fn approval_reach(source: &flux_a2a::attach::ApprovalReach) -> flux_tui::attach::ApprovalReach {
    use flux_a2a::attach::ApprovalReach as Wire;
    use flux_tui::attach::ApprovalReach as Surface;
    match source {
        Wire::Answerable { caveat } => Surface::Answerable {
            caveat: caveat.clone(),
        },
        Wire::NotRaised(why) => Surface::NotRaised(why.clone()),
        Wire::Unanswerable(why) => Surface::Unanswerable(why.clone()),
        Wire::Unknown(why) => Surface::Unknown(why.clone()),
    }
}

fn attach_update(event: flux_a2a::attach::AttachEvent) -> Option<flux_tui::attach::AttachUpdate> {
    use flux_a2a::attach::AttachEvent as Wire;
    use flux_tui::attach::AttachUpdate as Surface;
    match event {
        Wire::Text(text) => Some(Surface::Text(text)),
        Wire::State { state, terminal } => Some(Surface::State { state, terminal }),
        Wire::Artifact { name, text } => Some(Surface::Artifact { name, text }),
        Wire::Notice { text, error } => Some(Surface::Notice { text, error }),
        // The surface learns a turn ended from its own `Finished` bookkeeping, which fires on every
        // exit path including a panicked driver; a second signal here would be a second source of
        // truth for the same fact.
        Wire::Ended => None,
    }
}

#[async_trait::async_trait]
impl flux_tui::attach::AttachedAgent for A2aAttachment {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn capabilities(&self) -> flux_tui::attach::AttachCapabilities {
        self.capabilities.clone()
    }

    async fn send(
        &self,
        input: String,
        out: tokio::sync::mpsc::UnboundedSender<flux_tui::attach::AttachUpdate>,
    ) {
        self.agent
            .send(&input, &mut |event| {
                if let Some(update) = attach_update(event) {
                    let _ = out.send(update);
                }
            })
            .await;
    }

    async fn cancel(&self) -> String {
        use flux_a2a::attach::CancelOutcome;
        match self.agent.cancel().await {
            CancelOutcome::Requested => {
                "cancel delivered — the remote agent is stopping this turn".to_string()
            }
            CancelOutcome::Idle => "nothing is running on the remote agent".to_string(),
            CancelOutcome::AlreadyTerminal => {
                "the remote turn had already finished — nothing to cancel".to_string()
            }
            // The whole point of returning a line rather than `()`: an interrupt this agent cannot
            // honour must not look like one it did.
            CancelOutcome::Unsupported(why) => format!(
                "the remote turn is STILL RUNNING — this agent cannot be cancelled from here: \
                 {why}"
            ),
        }
    }

    async fn history(&self) -> std::result::Result<Vec<flux_tui::attach::AttachTurn>, String> {
        Ok(self
            .agent
            .history()
            .await?
            .into_iter()
            .map(|turn| flux_tui::attach::AttachTurn {
                from_user: turn.from_user,
                text: turn.text,
            })
            .collect())
    }

    async fn pending_approvals(
        &self,
    ) -> std::result::Result<Vec<flux_tui::attach::AttachApproval>, String> {
        Ok(self
            .agent
            .pending_approvals()
            .await?
            .into_iter()
            .map(|a| flux_tui::attach::AttachApproval {
                id: a.id,
                fingerprint: a.fingerprint,
                tool: a.tool,
                subjects: a.subjects,
                summary: a.summary,
                destructive: a.destructive,
                mutating: a.mutating,
            })
            .collect())
    }

    async fn decide_approval(
        &self,
        id: &str,
        fingerprint: &str,
        allow: bool,
        reason: Option<String>,
    ) -> std::result::Result<(), String> {
        self.agent
            .decide_approval(id, fingerprint, allow, reason.as_deref())
            .await
    }
}

/// Connect to the selected agent and hand the TUI a driver for it.
///
/// Prints one orientation line before the terminal takes over — where the attachment points and
/// which credential *location* was used. Never the credential value.
pub(super) async fn connect_attachment(
    selection: &AttachSelection,
) -> Result<Arc<dyn flux_tui::attach::AttachedAgent>> {
    let target = resolve_attach_target(selection)?;
    let agent = flux_a2a::attach::AttachedA2aAgent::connect(
        &target.url,
        target.token,
        selection.context_id.clone(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("attach to `{}`: {e}", target.url))?;
    let support = agent.support();
    let capabilities = flux_tui::attach::AttachCapabilities {
        streaming: availability(&support.streaming),
        cancel: availability(&support.cancel),
        history: availability(&support.history),
        approvals: approval_reach(&support.approvals),
    };
    let label = format!("{} · {}", agent.label(), agent.endpoint());
    eprintln!(
        "{}",
        style::dim(&format!(
            "attaching to {label} · credential {} · context {}",
            target.credential_source,
            agent.context_id()
        ))
    );
    Ok(Arc::new(A2aAttachment {
        agent,
        label,
        capabilities,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(target: &str, token_env: &str) -> AttachSelection {
        AttachSelection {
            target: target.to_string(),
            token_env: token_env.to_string(),
            context_id: None,
        }
    }

    #[test]
    fn a_url_target_reads_its_credential_from_the_named_environment_variable() {
        // The credential is a *reference* on the command line: the flag carries the variable name,
        // the value is only ever read from the environment.
        let var = format!("FLUX_TEST_ATTACH_TOKEN_{}", std::process::id());
        std::env::set_var(&var, "s3cret");
        let resolved = resolve_attach_target(&selection("https://agent.example:8787", &var))
            .expect("a plain https url resolves");
        std::env::remove_var(&var);
        assert_eq!(resolved.url, "https://agent.example:8787");
        assert_eq!(resolved.token.as_deref(), Some("s3cret"));
        assert_eq!(resolved.credential_source, format!("env/{var}"));
        assert!(
            !resolved.credential_source.contains("s3cret"),
            "the displayed credential source must be a location, never the value"
        );
    }

    #[test]
    fn a_url_carrying_embedded_credentials_is_refused() {
        let error = resolve_attach_target(&selection(
            "https://user:pass@agent.example:8787",
            "FLUX_A2A_TOKEN",
        ))
        .expect_err("an embedded credential is refused rather than used");
        assert!(error.to_string().contains("credential-free"), "{error}");
        assert!(
            !error.to_string().contains("pass@"),
            "the refusal must not echo the credential: {error}"
        );
    }

    #[test]
    fn an_unknown_name_is_refused_with_the_two_ways_to_fix_it() {
        let error = resolve_attach_target(&selection("no-such-agent", "FLUX_A2A_TOKEN"))
            .expect_err("an unknown binding name is refused");
        let message = error.to_string();
        assert!(message.contains("no-such-agent"), "{message}");
        assert!(message.contains("a2a"), "{message}");
    }

    /// The vocabulary translation is total by construction (no wildcard arm in either match); this
    /// pins the two decisions that are *not* mechanical — `Ended` is dropped, and every other
    /// event survives.
    #[test]
    fn every_protocol_event_survives_the_translation_except_the_end_marker() {
        use flux_a2a::attach::AttachEvent as Wire;
        use flux_tui::attach::AttachUpdate as Surface;

        assert_eq!(
            attach_update(Wire::Text("hello".into())),
            Some(Surface::Text("hello".into()))
        );
        assert_eq!(
            attach_update(Wire::State {
                state: "working".into(),
                terminal: false
            }),
            Some(Surface::State {
                state: "working".into(),
                terminal: false
            })
        );
        assert_eq!(
            attach_update(Wire::Artifact {
                name: "plan".into(),
                text: "body".into()
            }),
            Some(Surface::Artifact {
                name: "plan".into(),
                text: "body".into()
            })
        );
        assert_eq!(
            attach_update(Wire::Notice {
                text: "stream broke".into(),
                error: true
            }),
            Some(Surface::Notice {
                text: "stream broke".into(),
                error: true
            })
        );
        assert_eq!(attach_update(Wire::Ended), None);
    }

    #[test]
    fn an_unsupported_cancel_says_the_remote_turn_is_still_running() {
        // The exact failure this wording exists to prevent: an interrupt that stops the client but
        // not the agent, reported as if it had stopped both.
        let outcome = flux_a2a::attach::CancelOutcome::Unsupported(
            "this agent does not implement tasks/cancel".into(),
        );
        let rendered = match outcome {
            flux_a2a::attach::CancelOutcome::Unsupported(why) => format!(
                "the remote turn is STILL RUNNING — this agent cannot be cancelled from here: \
                 {why}"
            ),
            _ => unreachable!(),
        };
        assert!(rendered.contains("STILL RUNNING"), "{rendered}");
    }
}
