//! Subprocess lifecycle, tool projection, and descriptor discovery.

use super::refresh::capability_widenings;
use super::*;
use flux_plugin_protocol::validate_manifest_operations;

// ---------------------------------------------------------------------------
// Plugin host (host side) — async, spawns the subprocess
// ---------------------------------------------------------------------------

/// A running plugin subprocess the host talks to over framed stdio.
pub struct PluginHost {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    system: flux_system::System,
    argv: Vec<String>,
    verified_spawn: Option<(String, PluginDescriptor)>,
    /// What the last manifest fetch did with the grant of record (C-411) — see
    /// [`PluginHost::capability_grant`].
    capability_grant: Option<CapabilityGrant>,
    poisoned: Arc<std::sync::atomic::AtomicBool>,
}

/// Marks an in-flight framed exchange unhealthy unless it reaches a complete response. Async
/// cancellation drops futures at an arbitrary await; this guard makes that observable to the next
/// owner of the host mutex, which restarts the subprocess before writing another request.
struct ProtocolExchange {
    poisoned: Arc<std::sync::atomic::AtomicBool>,
    complete: bool,
}

impl ProtocolExchange {
    fn new(poisoned: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            poisoned,
            complete: false,
        }
    }

    fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for ProtocolExchange {
    fn drop(&mut self) {
        if !self.complete {
            self.poisoned
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

impl PluginHost {
    /// Spawn a plugin binary through flux's **single guarded process path**
    /// ([`flux_system::System::spawn_interactive`]): the plugin runs argv-only, in the workspace root,
    /// with a **cleared environment** (the minimal non-secret allow-list only) — so it cannot read the
    /// host's secrets directly and must request them back through the gated host capabilities. The
    /// framed `flux.plugin.v1` protocol then runs over the piped stdin/stdout.
    pub async fn spawn(
        system: &flux_system::System,
        program: &str,
        args: &[String],
    ) -> Result<Self> {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(program.to_string());
        argv.extend_from_slice(args);
        let flux_system::InteractiveChild {
            child,
            stdin,
            stdout,
        } = system
            .spawn_interactive(&argv)
            .map_err(|e| Error::Other(format!("spawn plugin {program}: {e}")))?;
        Ok(Self {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            next_id: 0,
            system: system.clone(),
            argv,
            verified_spawn: None,
            capability_grant: None,
            poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Spawn from a **descriptor**, enforcing its recorded `sha256` first (D-48): the binary is
    /// re-hashed (1–4 MB, sub-millisecond) and drift is a hard refusal naming the plugin, the
    /// expected, and the actual hash — never a silent fallback. Hashless (dev/local) descriptors
    /// spawn exactly as [`PluginHost::spawn`] always has. Every descriptor-based load path
    /// (agent-startup discovery, `flux plugin call`, `status`) goes through here — the no-bypass
    /// discipline, same as `Executor::dispatch`.
    pub async fn spawn_verified(
        system: &flux_system::System,
        name: &str,
        d: &PluginDescriptor,
    ) -> Result<Self> {
        if let Verification::HashDrift { expected, actual } = verify_descriptor(d) {
            return Err(Error::Other(format!(
                "plugin `{name}`: refusing to spawn — binary hash drift: the descriptor records \
                 sha256 {expected} but `{}` hashes to {actual} (restore a verified binary with \
                 `flux plugin install {name}` or `flux plugin pin {name} <version>`)",
                d.program
            )));
        }
        let mut host = Self::spawn(system, &d.program, &d.args).await?;
        host.verified_spawn = Some((name.to_string(), d.clone()));
        Ok(host)
    }

    /// Replace a protocol session abandoned by cancellation or transport failure. The old process
    /// is killed before a fresh one starts through the same guarded System path; descriptor-backed
    /// plugins repeat their hash verification before every restart.
    async fn restart_if_poisoned(&mut self) -> Result<()> {
        if !self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        if let Some((name, descriptor)) = &self.verified_spawn {
            if let Verification::HashDrift { expected, actual } = verify_descriptor(descriptor) {
                return Err(Error::Other(format!(
                    "plugin `{name}`: refusing to restart after an interrupted protocol exchange — \
                     binary hash drift: expected {expected}, got {actual}"
                )));
            }
        }

        // `kill` also waits/reaps. Ignore an already-exited error: either way the old framed stream
        // is never reused.
        let _ = self.child.kill().await;
        let flux_system::InteractiveChild {
            child,
            stdin,
            stdout,
        } = self
            .system
            .spawn_interactive(&self.argv)
            .map_err(|e| Error::Other(format!("restart plugin {}: {e}", self.argv[0])))?;
        self.child = child;
        self.stdin = stdin;
        self.reader = tokio::io::BufReader::new(stdout);
        self.next_id = 0;
        self.poisoned
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Io)?;
        self.stdin.flush().await.map_err(Error::Io)?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        // Bound a single framed message so a malicious/buggy plugin can't OOM the host by emitting
        // a gigantic line with no newline. `Take` caps the bytes `read_until` will consume.
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(flux_plugin_protocol::MAX_FRAME_BYTES as u64)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Provider("plugin closed the connection".into()));
        }
        // Every plugin→host frame carries the wire-protocol marker. Decode at the shared pure seam
        // so the async host, property corpus, and Miri exercise one implementation.
        flux_plugin_protocol::decode_ndjson_frame(&buf).map_err(Error::Provider)
    }

    async fn request(&mut self, command: &str, payload: Value) -> Result<Frame> {
        let mut exchange = ProtocolExchange::new(self.poisoned.clone());
        self.restart_if_poisoned().await?;
        self.next_id += 1;
        let frame = Frame::request(format!("r{}", self.next_id), command, payload);
        self.write_frame(&frame).await?;
        let response = self.read_frame().await?;
        exchange.complete();
        Ok(response)
    }

    /// Fetch the plugin's manifest — **and bound it by the operator's grant of record** (C-411).
    ///
    /// This is the single point at which a plugin's declaration enters the host, which is why the
    /// grant check lives here and not in [`load_plugin_tools`]: `flux plugin call` spawns through
    /// [`PluginHost::spawn_verified`], fetches the manifest itself, and installs it via
    /// [`SystemHostCaps::with_manifest`] without ever touching the projection helper. A descriptor
    /// spawn records its `(name, descriptor)` on the host, so every such path is covered by one
    /// call; a descriptor-less [`PluginHost::spawn`] has no operator grant to measure against and is
    /// unchanged.
    ///
    /// A widening is a hard refusal. See [`CapabilityGrant`] for the posture and the remedy.
    ///
    /// # Only the **first** fetch on a host is measured
    ///
    /// The record answers "what may this plugin start from?", which is a question about a new
    /// process. A *second* fetch on an already-open host is a catalog refresh, and that has its own
    /// rules in [`refresh`](super::refresh) — strictly stricter ones, bounded by *this session's*
    /// load-time manifest rather than by the ceiling, and covering things the record says nothing
    /// about (a retained op quietly re-scoped). Re-adjudicating it here would also *contradict*
    /// them: `pin_granted_authority` deliberately **accepts and ignores** a refreshed `endpoints` /
    /// `discovers`, and turning that into a refusal would break the classification C-322 settled.
    /// So the check runs once, when the host has no outcome yet, and the refresh path owns
    /// everything after it.
    pub async fn manifest(&mut self) -> Result<PluginManifest> {
        let f = self.request("manifest", Value::Null).await?;
        if !f.ok {
            return Err(Error::Provider(f.error.unwrap_or_default()));
        }
        let manifest: PluginManifest = serde_json::from_value(f.result)?;
        if self.capability_grant.is_none() {
            if let Some((name, descriptor)) = &self.verified_spawn {
                let (outcome, _) = adopt_capability_grant(name, descriptor, &manifest)?;
                self.capability_grant = Some(outcome);
            }
        }
        Ok(manifest)
    }

    /// What the **first** [`PluginHost::manifest`] fetch did with this plugin's grant of record —
    /// `None` before that fetch, or for a descriptor-less spawn that has no grant to measure.
    pub fn capability_grant(&self) -> Option<&CapabilityGrant> {
        self.capability_grant.as_ref()
    }

    /// Call an operation with no host capabilities (callbacks are denied).
    pub async fn call(&mut self, operation: &str, input: Value) -> Result<Value> {
        self.call_with_host(operation, input, &DenyHostCaps).await
    }

    /// Call an operation, servicing any plugin→host capability callbacks via `host` until the
    /// operation's own response arrives. Callbacks and the final response are multiplexed on the
    /// same channel and demultiplexed by frame kind + id.
    pub async fn call_with_host(
        &mut self,
        operation: &str,
        input: Value,
        host: &dyn HostCapabilities,
    ) -> Result<Value> {
        let mut exchange = ProtocolExchange::new(self.poisoned.clone());
        self.restart_if_poisoned().await?;
        self.next_id += 1;
        let call_id = format!("r{}", self.next_id);
        let frame = Frame::request(
            &call_id,
            "operation.call",
            json!({ "operation": operation, "input": input }),
        );
        self.write_frame(&frame).await?;

        loop {
            let f = self.read_frame().await?;
            match f.kind {
                FrameKind::Request => {
                    // A host-capability callback from the plugin — service it and reply.
                    let reply = match host.handle(&f.command, &f.payload).await {
                        Ok(v) => Frame::ok_response(&f.id, v),
                        Err(e) => Frame::err_response(&f.id, e),
                    };
                    self.write_frame(&reply).await?;
                }
                FrameKind::Response => {
                    if f.id == call_id {
                        exchange.complete();
                        return if f.ok {
                            Ok(f.result)
                        } else {
                            Err(Error::Provider(f.error.unwrap_or_default()))
                        };
                    }
                    // A stray/duplicate response — ignore and keep reading.
                }
            }
        }
    }

    /// Terminate the plugin.
    pub async fn shutdown(mut self) -> Result<()> {
        drop(self.stdin); // closing stdin lets a well-behaved plugin exit
        let _ = self.child.kill().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PluginTool — project a plugin operation as an agent tool
// ---------------------------------------------------------------------------

/// Projects a single plugin [`OperationSpec`] as an agent [`Tool`]. All of a plugin's operations
/// share one [`PluginHost`] subprocess behind a mutex; each call goes through the same safety
/// envelope as built-in tools and may issue host-capability callbacks via [`HostCapabilities`].
pub struct PluginTool {
    host: Arc<tokio::sync::Mutex<PluginHost>>,
    caps: Arc<dyn HostCapabilities>,
    plugin: String,
    operation: String,
    spec: ToolSpec,
    /// The manifest-declared semantic-effect tags (D-138), carried alongside `spec` since a
    /// [`ToolSpec`] has no room for them — see [`Tool::semantic_effects`].
    semantic_effects: Vec<String>,
    capabilities: PluginCapabilities,
    secret_purposes: Vec<String>,
    /// The op's declared per-operation `process` narrowing (C-90): argv prefixes this op may use,
    /// enforced at callback time on top of the manifest-level gate and projected as the op's
    /// `process.exec` authority instead of the manifest-wide grants. Empty = no narrowing.
    op_process: Vec<String>,
    staging: StagingDisposition,
    /// The op's declared secret-like field names (GL-031); the op result is passed through
    /// [`redact_secret_fields`] with these before it is stringified into model-visible output.
    redact_fields: Vec<String>,
    /// Whether this op is fronted by a credential-injecting deployment (C-312), and if so what its
    /// responses must look like. Non-default installs the credential boundary — see
    /// [`credential_boundary`](super::credential_boundary).
    platform: PlatformSourcing,
    /// Where this op's real work lands when the deployment — not flux — dials (C-311). Disclosed
    /// at the approval prompt; see [`vendor_disclosure`](super::vendor_disclosure).
    reaches: VendorReach,
}

impl PluginTool {
    pub fn new(
        host: Arc<tokio::sync::Mutex<PluginHost>>,
        caps: Arc<dyn HostCapabilities>,
        manifest: &PluginManifest,
        op: &OperationSpec,
    ) -> Self {
        let (operation, spec) = plugin_tool_spec(&manifest.name, op, &manifest.capabilities);
        let semantic_effects = semantic_effect_tags(&op.semantic_effects);
        Self {
            host,
            caps,
            plugin: manifest.name.clone(),
            operation,
            spec,
            semantic_effects,
            capabilities: manifest.capabilities.clone(),
            secret_purposes: op.secret_purposes.clone(),
            op_process: op.process.clone(),
            staging: op.staging,
            redact_fields: op.redact_fields.clone(),
            platform: op.platform,
            reaches: op.reaches.clone(),
        }
    }
}

/// Report every projected [`ToolSpec`] in `manifest` whose metadata contradicts itself (C-191).
///
/// A plugin's `effects` / `risk` / `idempotency` / `access` are authored outside this repo and are
/// then trusted verbatim by every downstream gate — the gather path, the op cache, the risk
/// approver, and the sentence a human reads at the approval prompt. Built-in ops are held to the
/// same invariants by a build-time test over the registry (`flux-tools`'
/// `toolspec_invariants.rs`); a plugin has no build of ours to run, so for plugin-supplied specs
/// the only place the invariants can be applied is where the declaration enters — here, at load.
///
/// # Why this warns instead of refusing the plugin
///
/// The first cut of this check failed the whole manifest closed. Run against the plugins this repo
/// actually ships, that dropped all 24 `kubernetes` ops off the agent surface over two mis-declared
/// idempotency fields, and dropped an installed third-party plugin entirely over 51 operations that
/// tag `delete`/`money` below `Risk::Destructive` — and it did so on a process that still exits 0,
/// so the agent just quietly lost the capability.
///
/// Refusal is the wrong remedy for an under-declaration. It removes the operation rather than
/// gating it, it lands as a breaking change on plugin authors who have had no window to correct
/// their manifests, and it is *less* safe than loading loudly: an operator who loses `kubernetes`
/// silently learns nothing, while one who is told which 51 operations under-declare destructiveness
/// can act on it. The in-repo half of that population is fixed in this same change
/// (`plugins/kubernetes`); the out-of-repo half needs a deprecation window before a hard cutover,
/// and that cutover belongs to its own story.
///
/// Scope is [`visible_ops`] — the ops that actually become agent tools. An `internal: true` op
/// never reaches a [`ToolRegistry`] and so never reaches the gates these invariants protect; the
/// host dispatches it directly through the shared [`PluginHost`] handle.
///
/// Free function over the manifest so it is unit-testable without spawning a subprocess, like
/// [`plugin_tool_spec`] and [`visible_ops`] beside it. The invariants and their allowlist live in
/// [`flux_spec::coherence`].
pub(super) fn op_coherence_warnings(manifest: &PluginManifest) -> Vec<String> {
    let mut violations = Vec::new();
    for op in visible_ops(manifest) {
        let (_, spec) = plugin_tool_spec(&manifest.name, op, &manifest.capabilities);
        violations.extend(flux_spec::metadata_violations(
            &spec,
            &semantic_effect_tags(&op.semantic_effects),
        ));
    }
    violations
}

pub(super) fn plugin_tool_spec(
    plugin: &str,
    op: &OperationSpec,
    capabilities: &PluginCapabilities,
) -> (String, ToolSpec) {
    // The model-facing tool name is the operation's fully-qualified name. flux plugin ops are
    // authored already qualified (e.g. `slack.message.send`), and the plugin's own dispatch,
    // `flux plugin call`, and the generated skill docs all use that name — so adopt it verbatim when
    // it is already prefixed, and only add the `{plugin}.` prefix for an un-qualified op name.
    // (Unconditionally prefixing double-qualified the common case to `slack.slack.message.send`, so
    // an agent's `tools` grant — `slack.message.send` — never matched and every plugin op was
    // silently dropped from the agent surface.)
    let qualified = op.projected_name(plugin);
    // C-309: every plugin op dispatches to a subprocess, so `Process` access is unconditional —
    // it is not conditioned on `capabilities.process`, which is a narrower thing (the *further*
    // programs the plugin may shell out to via the host callback). A plugin binary is arbitrary
    // code the operator installed; invoking one of its ops is a process interaction whether or not
    // it asked flux to run anything on its behalf.
    //
    // Without this the two safety mechanisms were mutually unsatisfiable. `flux-runtime`'s
    // authority contract refuses any tool declaring an effect it holds no matching access for,
    // while the effect-less default below declares `[Process, Network]` — so every effect-less op
    // of a plugin without a `process` capability was **impossible to load**, which is how
    // `flux-sdk`'s fixture plugin sat red behind a feature no gate compiled. Fixing it on the
    // effects side instead would have been worse than the bug: authority requirements derive from
    // `access`, not `effects` (`authority_requirements_from_declaration`), so an op projected with
    // neither would carry NO requirement at all and skip the authorization floor entirely.
    let mut access = vec![AccessKind::Process];
    if capabilities.http {
        access.push(AccessKind::Network);
    }
    if !capabilities.conn.is_empty() {
        access.push(AccessKind::Connection);
    }
    if !capabilities.secrets.is_empty() || !op.secret_purposes.is_empty() {
        access.push(AccessKind::Secret);
    }
    if capabilities.blob
        || capabilities.discover
        || capabilities.credential
        || !capabilities.fs.is_empty()
    {
        access.push(AccessKind::LocalSystem);
    }
    // Project the operation's declared effects so the authorization floor gates it like any
    // built-in tool. An operation that declares none could still touch the network or run a
    // process via host capabilities, so default to those — under the default grants that forces
    // approval rather than letting the op slip the envelope.
    let effects = if op.effects.is_empty() {
        vec![Effect::Process, Effect::Network]
    } else {
        op.effects.clone()
    };
    let spec = ToolSpec {
        name: qualified,
        description: op.description.clone(),
        input_schema: op.input_schema.clone(),
        output_schema: op.output_schema.clone(),
        effects,
        risk: op.risk.unwrap_or(Risk::Medium),
        idempotency: op.idempotency.unwrap_or(Idempotency::NonIdempotent),
        access,
        group: op.group.clone(),
    };
    (op.name.clone(), spec)
}

/// Project an op's declared [`FlowEffect`]s onto the plain tag strings [`Tool::semantic_effects`]
/// returns (D-138) — a small free function so the manifest→catalog projection is unit-testable
/// without spinning up a full [`PluginTool`] (which needs a live [`PluginHost`] subprocess
/// connection). Order-preserving; duplicates are the caller's concern (the catalog adapter in
/// `flux-flow`'s `OpRegistry` dedupes when it parses these back onto `OpSignature::semantic_effects`).
pub(super) fn semantic_effect_tags(effects: &[FlowEffect]) -> Vec<String> {
    effects.iter().map(|e| e.tag().to_string()).collect()
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        let mut subjects = vec![format!("{}.{}", self.plugin, self.operation)];
        // C-311 — the vendor-host disclosure. The per-op approval prompt (plain CLI and TUI alike)
        // renders exactly this list, so it is the one channel that reaches an operator at the
        // moment they decide. Present only for a platform-sourced op: where flux dials, the
        // destination is already bound by `guard_url_scoped` and named by the op's own
        // `network.fetch` authority, so every existing plugin's subjects are unchanged.
        if let Some(disclosure) = vendor_disclosure::subject(self.platform, &self.reaches) {
            subjects.push(disclosure);
        }
        subjects
    }

    fn semantic_effects(&self) -> Vec<String> {
        self.semantic_effects.clone()
    }

    fn authority_requirements(
        &self,
        params: &Value,
        subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let mut declared_subjects = subjects.to_vec();
        if self
            .semantic_effects
            .iter()
            .any(|effect| effect == "write_db")
        {
            declared_subjects.push(format!("datasource:{}", self.plugin));
        }
        let mut requirements = authority_requirements_from_declaration(
            &self.spec,
            &declared_subjects,
            &self.semantic_effects,
        )?;

        // Replace manifest-wide resource families with the exact scopes the plugin declared. The
        // host capability layer enforces these again at callback time; carrying them here makes the
        // outer authorization/approval preview describe the same authority before the subprocess
        // is called.
        requirements.retain(|requirement| {
            !matches!(
                requirement.action.0.as_str(),
                "network.fetch"
                    | "connection.dial"
                    | "process.exec"
                    | "secret.read"
                    | "host.read"
                    | "host.write"
            )
        });
        if self.capabilities.http {
            let hosts = if self.capabilities.http_hosts.is_empty() {
                vec![self.plugin.as_str()]
            } else {
                self.capabilities
                    .http_hosts
                    .iter()
                    .map(String::as_str)
                    .collect()
            };
            requirements.extend(hosts.into_iter().map(AuthorityRequirement::network_fetch));
        }
        // C-311 — the same disclosure, on the channel the WHOLE-PLAN prompt reads. That prompt
        // renders typed requirements and never permission subjects, and an approved plan skips the
        // per-op gate entirely — so without this a plan-approved batch of platform-sourced ops
        // would disclose nothing at all. For a declared vendor this is usually a no-op after the
        // `dedup` below (the manifest's own `http_hosts` must already admit the host, or the
        // manifest was refused at load); it is here so the op→vendor binding is a property of the
        // op's contract rather than a coincidence of the manifest-wide grant.
        if let Some(resource) = vendor_disclosure::network_resource(self.platform, &self.reaches) {
            requirements.push(AuthorityRequirement::network_fetch(resource));
        }
        requirements.extend(
            self.capabilities
                .conn
                .iter()
                .map(AuthorityRequirement::connection_dial),
        );
        // An op with a per-operation `process` narrowing (C-90) carries THAT as its authority —
        // the prompt/audit then shows `kubectl get` rather than every manifest-wide grant — and
        // the callback-time wrapper in `execute` enforces the same list, so the disclosed
        // authority and the enforced authority are one declaration.
        let process_grants = if self.op_process.is_empty() {
            &self.capabilities.process
        } else {
            &self.op_process
        };
        requirements.extend(
            process_grants
                .iter()
                .map(AuthorityRequirement::process_exec),
        );
        requirements.extend(
            self.capabilities
                .secrets
                .iter()
                .map(AuthorityRequirement::secret_read),
        );
        requirements.extend(
            self.secret_purposes
                .iter()
                .map(AuthorityRequirement::secret_read),
        );
        requirements.extend(
            self.capabilities
                .fs
                .iter()
                .map(|scope| AuthorityRequirement::host_read(&scope.path)),
        );
        if self.capabilities.discover {
            requirements.push(AuthorityRequirement::host_read("endpoint-discovery"));
        }
        if self.capabilities.credential {
            requirements.push(AuthorityRequirement::secret_read("endpoint-credential"));
        }
        if self.capabilities.blob {
            let subject = params
                .get("ref")
                .and_then(Value::as_str)
                .unwrap_or("plugin-blob");
            if self.spec.effects.contains(&Effect::Write) {
                requirements.push(AuthorityRequirement::host_write(subject));
            } else {
                requirements.push(AuthorityRequirement::host_read(subject));
            }
        }
        requirements.sort_by(|left, right| {
            left.action
                .0
                .cmp(&right.action.0)
                .then_with(|| left.resource.id.cmp(&right.resource.id))
        });
        requirements.dedup();
        Ok(requirements)
    }

    fn staging_disposition(&self) -> StagingDisposition {
        self.staging
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut host = self.host.lock().await;
        // Enforce the op's `process` narrowing (C-90) on every callback this call issues. The
        // scoped view sits in FRONT of the shared caps, so the manifest-level gate still runs —
        // a per-op declaration can only narrow the manifest grant, never widen it.
        let scoped;
        let caps: &dyn HostCapabilities = if self.op_process.is_empty() {
            self.caps.as_ref()
        } else {
            scoped = OpScopedCaps {
                inner: self.caps.as_ref(),
                operation: &self.operation,
                process: &self.op_process,
            };
            &scoped
        };
        match host.call_with_host(&self.operation, params, caps).await {
            Ok(mut v) => {
                // C-312 — the credential boundary, at INGEST. This runs on the raw response,
                // before the op's own `redact_fields` masking below and before any
                // stringification: masking first would let a manifest declare its way past its own
                // boundary, and checking at display instead of ingest is the C-215 mistake this is
                // told not to repeat. A no-op unless the manifest declared `platform`.
                if let Some(refusal) = credential_boundary::refuse_response(
                    self.platform,
                    &self.plugin,
                    &self.operation,
                    &v,
                    &ctx.redactor,
                ) {
                    // `v` is dropped here: a refused response is discarded whole, never redacted
                    // and passed on.
                    return Ok(ToolResult::error(refusal));
                }
                // Mask the op's declared secret-like fields (GL-031) before the result is
                // stringified into model-visible output — a no-op unless the op declared any.
                redact_secret_fields(&mut v, &self.redact_fields);
                Ok(ToolResult::ok(
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
                ))
            }
            // The `err` frame is the same ingest surface as the `result` frame — a plugin that
            // answers `{"ok": false, "error": "<vendor 401 body>"}` would otherwise put whatever
            // that body carries straight into a tool result.
            Err(e) => Ok(ToolResult::error(credential_boundary::scrub_error(
                self.platform,
                &self.plugin,
                &self.operation,
                e.to_string(),
                &ctx.redactor,
            ))),
        }
    }
}

/// A per-call view over the shared [`HostCapabilities`] that enforces one operation's `process`
/// narrowing (C-90) before delegating. Only `process.run`/`process.spawn` are pre-checked — every
/// other callback, and the manifest-level argv gate itself, still runs in the wrapped caps, so
/// this can only subtract authority. Empty argv is delegated untouched so the inner gate produces
/// its canonical error.
struct OpScopedCaps<'a> {
    inner: &'a dyn HostCapabilities,
    operation: &'a str,
    process: &'a [String],
}

#[async_trait]
impl HostCapabilities for OpScopedCaps<'_> {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        if matches!(command, "process.run" | "process.spawn") {
            let argv: Vec<String> = payload
                .get("argv")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !argv.is_empty() && !flux_plugin_protocol::process_grant_allows(self.process, &argv)
            {
                return Err(format!(
                    "{command}: `{}` is outside operation `{}`'s declared process constraints",
                    argv.join(" "),
                    self.operation
                ));
            }
        }
        self.inner.handle(command, payload).await
    }
}

/// A loaded plugin: its projected tools plus the live handles the L5 endpoint broker (D-26) needs to
/// fan out a discovery query back to it. Returned by [`load_plugin_tools`].
///
/// The broker keeps the `manifest` (to match its `discovers` products), the shared `host` (to issue
/// an `endpoint.discover` op call), and the `caps` (the same guarded host-capability set the plugin's
/// tools were built with, so a broker-driven call is gated identically).
pub struct LoadedPlugin {
    /// Each plugin operation projected as an agent [`Tool`]; registering them keeps the host alive.
    pub tools: Vec<Arc<dyn Tool>>,
    /// The shared subprocess connection every tool (and the broker) drives behind a mutex.
    pub host: Arc<tokio::sync::Mutex<PluginHost>>,
    /// The plugin's fetched manifest (carries `discovers` + the requested capabilities).
    pub manifest: PluginManifest,
    /// The guarded host capabilities the plugin's ops run under (the output of `make_caps`).
    pub caps: Arc<dyn HostCapabilities>,
    /// Metadata-coherence violations in this plugin's declarations (C-191), one sentence each, for
    /// the caller to surface. Non-empty does **not** block the load — see
    /// [`op_coherence_warnings`] for why an under-declaration is warned about rather than refused.
    pub coherence_warnings: Vec<String>,
    /// What the load path did with the descriptor's **grant of record** (C-411) — recorded for the
    /// first time, matched against one already on record, or not recordable at all. Surfaced rather
    /// than swallowed because [`CapabilityGrant::NotRecorded`] means this plugin's next load is as
    /// unguarded as this one was.
    pub capability_grant: CapabilityGrant,
}

/// Spawn a plugin from its descriptor, fetch its manifest, and project every operation as a
/// [`PluginTool`] sharing one host connection. Returns a [`LoadedPlugin`] (the tools plus the
/// shared host/manifest/caps the endpoint broker fans out to) — keep the host handle alive for
/// the session.
///
/// Descriptor-based so the spawn goes through [`PluginHost::spawn_verified`] — the D-48 sha256
/// check covers agent-startup discovery exactly like `flux plugin call`/`status` (a hashless
/// dev/local descriptor spawns as before; recorded-hash drift is a hard refusal).
///
/// `make_caps` builds the host capabilities *from the fetched manifest*, so the caps can be scoped
/// to exactly what the plugin declared (see [`SystemHostCaps::with_grants`]) — the binding point
/// where a plugin's requested privileges are pinned to its manifest.
///
/// **A load is a grant, so it is bounded by the grant on record** (C-411): before `make_caps` sees
/// the fetched manifest, its capability declaration is measured against the descriptor's
/// [`PluginDescriptor::capabilities`], and a widening is refused here. See [`CapabilityGrant`] for
/// the posture and why it is a refusal rather than a warning.
pub async fn load_plugin_tools(
    system: &flux_system::System,
    name: &str,
    descriptor: &PluginDescriptor,
    make_caps: impl FnOnce(&PluginManifest) -> Arc<dyn HostCapabilities>,
) -> Result<LoadedPlugin> {
    let mut host = PluginHost::spawn_verified(system, name, descriptor).await?;
    // The grant-of-record check runs inside `manifest()` — the one seam every descriptor-based
    // path shares — so a widening has already been refused by the time this returns.
    let manifest = host.manifest().await?;
    validate_manifest_operations(&manifest).map_err(Error::Other)?;
    let capability_grant = host.capability_grant().cloned().unwrap_or_else(|| {
        CapabilityGrant::NotRecorded("the plugin was spawned without a descriptor".to_string())
    });
    let coherence_warnings = op_coherence_warnings(&manifest);
    let caps = make_caps(&manifest);
    let host = Arc::new(tokio::sync::Mutex::new(host));
    // Project only the non-`internal` ops as agent tools (C-09a). A host-only op (`internal: true`,
    // e.g. the aws-bedrock plugin's `auth` op returning raw AWS keys) stays dispatchable by the host
    // via the shared `PluginHost` handle, but is NOT advertised to the LLM — the model must never
    // call it, or the keys would appear in the tool result. The filter is a single free function
    // ([`visible_ops`]) so the projection rule is unit-testable without spawning a subprocess.
    let tools: Vec<Arc<dyn Tool>> = visible_ops(&manifest)
        .map(|op| {
            Arc::new(PluginTool::new(host.clone(), caps.clone(), &manifest, op)) as Arc<dyn Tool>
        })
        .collect();
    Ok(LoadedPlugin {
        tools,
        host,
        manifest,
        caps,
        coherence_warnings,
        capability_grant,
    })
}

// ---------------------------------------------------------------------------
// The grant of record (C-411) — a load is bounded by what the install granted
// ---------------------------------------------------------------------------

/// What [`PluginHost::manifest`] did with the descriptor's grant of record.
///
/// # The posture, and why it is this one
///
/// A plugin's manifest is fetched from the plugin's own subprocess at every load, so the authority
/// the host installs is whatever the plugin last chose to declare. The in-session rules make a
/// *refresh* safe — [`capability_widenings`] refuses a second manifest that asks for more than the
/// first, and `op_scope_weakenings`/`PlatformSourcing::strictness` refuse a retained op that sheds
/// a stated boundary — but all three pin to *this* load's manifest. Nothing bounded the manifest a
/// **new process** starts from, so a plugin that widened what it asked for between two loads had the
/// wider set installed verbatim: the grant an operator reasoned about at install time was no longer
/// necessarily the grant in force, and nothing said so.
///
/// The posture chosen here is the same one the refresh path already takes, extended across the
/// process boundary: **a widening is refused, naming every field that grew.** A diff shown at load
/// and adopted anyway would be a disclosure, not a gate — the load path has no operator attached to
/// accept it (agent startup, `flux plugin call`, a server), so "surface and continue" would mean the
/// wider grant is in force by the time anyone reads the message. Refusing is also what makes the
/// *narrowing* direction safe to ignore: the grant on record is, by construction, known to be a
/// superset of what the plugin last asked for.
///
/// # Where the check lives, and why it is not in `load_plugin_tools`
///
/// It is enforced in [`PluginHost::manifest`], the single point at which a plugin's declaration
/// enters the host — because `load_plugin_tools` is **not** the only path that installs one.
/// `flux plugin call` spawns through [`PluginHost::spawn_verified`], fetches the manifest itself and
/// hands it straight to [`SystemHostCaps::with_manifest`]; this repo's own C-404 census (in
/// `flux-codegate`) lists that command as a *distinct* plugin-response ingest site for exactly this
/// reason. Checking inside the projection helper would have left the one command an operator uses to
/// poke a plugin by hand adopting a widening silently. Every descriptor-based spawn records its
/// `(name, descriptor)` on the host, so putting the check at the fetch covers all of them at once.
///
/// # The grant of record is a ceiling, not a snapshot
///
/// [`PluginDescriptor::grant`] records what the plugin declared the first time it was loaded under
/// this descriptor, and a later load may declare **less** without moving it. Narrowing is not
/// penalised (the host enforces the narrower declaration for that session), and returning to the
/// recorded set is not a widening. Exactly the asymmetry `prepare_refresh` applies within a session,
/// for the same reason: overstating teaches an operator to over-grant, while a surrender costs
/// nothing that is not already granted.
///
/// # Where the record comes from
///
/// A descriptor written before this rule existed — or by a fresh `install`/`add` — carries no grant,
/// which is not a widening but an install: the first fetch writes what the plugin declared back into
/// the descriptor it was read from. That write needs [`PluginDescriptor::origin`]; a descriptor that
/// has one and cannot be written is a **hard error**, because a store that exists but cannot record
/// the grant is a store whose grant can never be enforced — continuing would silently re-enter this
/// branch at every later load and adopt a widening with no signal at all. Only a descriptor with no
/// store behind it (the SDK's `load_tools`, an embedder, a test fixture) yields
/// [`CapabilityGrant::NotRecorded`]. Every rewrite of an existing descriptor carries the record
/// forward ([`add_descriptor`]); removing the `[grant]` table re-grants without discarding the
/// descriptor's `previous`/`sha256` rollback state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityGrant {
    /// No grant was on record, and what this fetch declared was written to the descriptor as the
    /// grant of record. Every later load is measured against it.
    Recorded,
    /// The declaration is within the grant already on record — unchanged, or narrower.
    WithinGrant,
    /// The descriptor has no plugin store behind it ([`PluginDescriptor::origin`] is `None`), so
    /// there is nothing to record into and no operator grant to guard: an embedder built this
    /// descriptor in memory and owns that decision itself. Carries the reason. A store that exists
    /// but cannot be written is an error instead, never this.
    NotRecorded(String),
}

/// The authority a plugin's manifest asks the host to install, captured as the operator's grant of
/// record (C-411) and persisted in [`PluginDescriptor::grant`].
///
/// # Why it is not just `capabilities`
///
/// [`SystemHostCaps::with_manifest`] installs five manifest fields, and `capabilities` is only the
/// most obvious one. `endpoints` is a **second egress surface** — `ensure_http_host_allowed` admits
/// a host when `host_matches(&self.grants.http_hosts, host) || self.endpoint_allows_host(host)`, so
/// a plugin already granted `http: true` could widen its reachable hosts across a load by adding an
/// [`EndpointSpec`], with [`capability_widenings`] reporting nothing at all. `auth` decides which env
/// values the host resolves and injects on the plugin's behalf, `config` is the surface the gated
/// `config` capability exposes (and the substitution source for endpoint templates), and `discovers`
/// enlists the plugin as an authority on where a product lives.
///
/// Those are exactly the five fields `LoadedPlugin::pin_granted_authority` classifies as **PINNED**
/// — the operator's load-time grant. Recording the same five is what makes the load boundary and the
/// refresh boundary bound one thing, rather than two overlapping subsets of it.
///
/// # Equality
///
/// `PluginDescriptor` derives `PartialEq`/`Eq`; the wire structs on the independently versioned
/// protocol line derive neither, and moving a derive there would oblige a wire-contract version
/// decision for an assertion. Equality is defined here instead, as two-way containment under
/// [`GrantOfRecord::widenings`]: two grants are equal when neither asks for anything the other does
/// not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrantOfRecord {
    /// The deny-by-default host capability set (`process`/`secrets`/`http`/…).
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    /// Auth methods by purpose: which env values the host resolves and injects for this plugin.
    #[serde(default)]
    pub auth: Vec<AuthMethod>,
    /// Declared endpoints — the second egress surface beside `capabilities.http_hosts`.
    #[serde(default)]
    pub endpoints: Vec<EndpointSpec>,
    /// Declared non-secret config values the gated `config` capability may return.
    #[serde(default)]
    pub config: Vec<ConfigSpec>,
    /// Products this plugin enlists as a discovery provider for.
    #[serde(default)]
    pub discovers: Vec<String>,
}

impl GrantOfRecord {
    /// The authority a manifest asks for.
    ///
    /// The destructure is exhaustive **on purpose**, exactly as `pin_granted_authority` and
    /// [`capability_widenings`] are: a `..manifest` struct-update would let a newly added manifest
    /// field become installable authority that no record covers — which is precisely the gap this
    /// type exists to close for `endpoints`. Adding a field to [`PluginManifest`] reds here, and at
    /// `the_grant_of_record_covers_every_field_with_manifest_installs`, until someone classifies it.
    pub fn from_manifest(manifest: &PluginManifest) -> Self {
        let PluginManifest {
            // Granted authority — recorded here, and bounded by `widenings`.
            capabilities,
            auth,
            endpoints,
            config,
            discovers,
            // Not authority. `name` cannot move (the descriptor is keyed by it); `version`,
            // `groups` and `datasources` are descriptive or organisational; and `operations` are
            // the thing a manifest exists to change — each op's scope is gated by the authority
            // above, and a retained op name additionally by `op_scope_weakenings`.
            name: _,
            version: _,
            operations: _,
            datasources: _,
            groups: _,
        } = manifest;
        Self {
            capabilities: capabilities.clone(),
            auth: auth.clone(),
            endpoints: endpoints.clone(),
            config: config.clone(),
            discovers: discovers.clone(),
        }
    }

    /// Every way `self` asks for authority `granted` does not already carry, one phrase each. Empty
    /// means this declaration stays inside the grant of record.
    ///
    /// Containment is **literal** throughout, matching [`capability_widenings`]: an entry must
    /// appear verbatim in the record. For the list-shaped fields that means an added *or* an edited
    /// entry both count — an [`EndpointSpec`] re-pointed at another host under the same `name` is a
    /// widening of reach, not a rename, and the same argument applies to an `auth` method that keeps
    /// its purpose and changes its env. The wire structs carry no `PartialEq`, so entries are
    /// compared by canonical JSON; that is exact and costs the protocol line nothing.
    pub fn widenings(&self, granted: &GrantOfRecord) -> Vec<String> {
        fn gained<T: Serialize>(
            field: &str,
            granted: &[T],
            declared: &[T],
            label: impl Fn(&T) -> String,
        ) -> Vec<String> {
            let known: Vec<Value> = granted
                .iter()
                .filter_map(|entry| serde_json::to_value(entry).ok())
                .collect();
            declared
                .iter()
                .filter(|entry| match serde_json::to_value(entry) {
                    Ok(v) => !known.contains(&v),
                    // An entry that will not serialize cannot be shown to be inside the record, and
                    // of the two readings the permissive one is the dangerous one.
                    Err(_) => true,
                })
                .map(|entry| format!("`{field}` gains or re-points `{}`", label(entry)))
                .collect()
        }

        let mut out = capability_widenings(&granted.capabilities, &self.capabilities);
        out.extend(gained("auth", &granted.auth, &self.auth, |m| {
            m.purpose.clone()
        }));
        out.extend(gained(
            "endpoints",
            &granted.endpoints,
            &self.endpoints,
            |e| e.name.clone(),
        ));
        out.extend(gained("config", &granted.config, &self.config, |c| {
            c.name.clone()
        }));
        out.extend(gained(
            "discovers",
            &granted.discovers,
            &self.discovers,
            String::clone,
        ));
        out
    }
}

impl PartialEq for GrantOfRecord {
    fn eq(&self, other: &Self) -> bool {
        self.widenings(other).is_empty() && other.widenings(self).is_empty()
    }
}

impl Eq for GrantOfRecord {}

/// Measure a fetched declaration against the descriptor's grant of record, recording it when there
/// is none. `Err` is the refusal — see [`CapabilityGrant`] for why a widening is refused rather than
/// disclosed, and why this is reached from [`PluginHost::manifest`] rather than from the projection.
///
/// Answers the grant it settled on alongside the outcome. [`PluginHost::manifest`] does not need it
/// (it runs this once per host), but a caller that wants to know *what* was recorded — the status
/// surface, a test — should not have to re-derive it from the manifest.
pub(super) fn adopt_capability_grant(
    name: &str,
    descriptor: &PluginDescriptor,
    manifest: &PluginManifest,
) -> Result<(CapabilityGrant, GrantOfRecord)> {
    let declared = GrantOfRecord::from_manifest(manifest);
    let Some(granted) = &descriptor.grant else {
        let outcome = record_grant_of_record(name, descriptor, &declared)?;
        return Ok((outcome, declared));
    };
    let widenings = declared.widenings(granted);
    if widenings.is_empty() {
        return Ok((CapabilityGrant::WithinGrant, declared));
    }
    // The remedy names the lossless re-grant first: dropping the `[grant]` table leaves `previous`,
    // `version` and `sha256` intact, where an uninstall/install round-trip discards the offline
    // rollback target along with the record.
    let where_recorded = descriptor
        .origin
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("~/.flux/plugins/{name}.toml"));
    // Phrased for both callers: this refuses a manifest, and the same fetch backs a first load and
    // a catalog refresh alike.
    Err(Error::Other(format!(
        "plugin `{name}`: refusing its manifest — it now asks for more host authority than the \
         grant on record ({}). The authority an operator reviewed when this plugin was installed \
         is what stays in force; adopt the wider set deliberately by removing the `[grant]` table \
         from {where_recorded} (or `flux plugin uninstall {name}` and install it again), which \
         re-records the grant at the next load.",
        widenings.join("; ")
    )))
}

/// Write `declared` back into the descriptor file it was read from, as the grant every later load is
/// measured against.
///
/// A descriptor with no [`PluginDescriptor::origin`] has no store to guard, and is reported rather
/// than failed. A descriptor that *has* one and cannot be written is an error — see
/// [`CapabilityGrant`]'s "Where the record comes from" for why silently continuing there is the one
/// outcome that reproduces the defect this rule closes.
fn record_grant_of_record(
    name: &str,
    descriptor: &PluginDescriptor,
    declared: &GrantOfRecord,
) -> Result<CapabilityGrant> {
    let Some(path) = descriptor.origin.clone() else {
        return Ok(CapabilityGrant::NotRecorded(
            "the descriptor was not read from a plugin store".to_string(),
        ));
    };
    let recorded = PluginDescriptor {
        grant: Some(declared.clone()),
        origin: None,
        ..descriptor.clone()
    };
    toml::to_string_pretty(&recorded)
        .map_err(|e| format!("serialize descriptor: {e}"))
        .and_then(|body| std::fs::write(&path, body).map_err(|e| e.to_string()))
        .map_err(|e| {
            Error::Other(format!(
                "plugin `{name}`: refusing its manifest — it has no grant on record yet and one \
                 could not be written to {} ({e}). Without a record, a later manifest could widen \
                 what this plugin may ask the host for and nothing would notice.",
                path.display()
            ))
        })?;
    Ok(CapabilityGrant::Recorded)
}

// ---------------------------------------------------------------------------
// Discovery & lifecycle — descriptors under ~/.flux/plugins/<name>.toml
// ---------------------------------------------------------------------------

/// A persisted plugin descriptor (`~/.flux/plugins/<name>.toml`): how to launch the plugin plus an
/// optional pinned version. `flux plugin add|ls|pin|rollback` manage these; discovery loads them.
///
/// `version`/`sha256`/`source` are serde-defaulted additions (D-47): a remote `install` populates
/// all three (the installed version, the sha256 of the installed binary, and the release it came
/// from, e.g. `plugins-v0.2.0`); a local/dev descriptor (`add`, `install --dir`) carries none of
/// them and stays valid — `ls`/`status` label it `unverified (local)` rather than `verified`.
///
/// `git_url`/`git_commit` are the third install source's provenance (D-87, `install --git`): a
/// from-source build records the git URL it was cloned from and the resolved commit it was built
/// at, but no signed-pack `sha256`. Such a descriptor is [`Verification::UnverifiedFromSource`] —
/// `ls`/`status` label it `from-source (unverified)`, distinct from both a signed pack and a
/// `--dir` local scan.
///
/// `capabilities` is the **grant of record** (C-411): the host authority this install is on record
/// as granting, written by the first load and enforced as a ceiling by every later one. It is the
/// one field no descriptor rewrite may drop — [`add_descriptor`] carries it forward — because a
/// rewrite of *how to launch* a plugin is not a decision to re-grant what it may ask for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// The plugin executable (absolute path or a name on `PATH`).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The pinned version, if any (advisory; surfaced by `flux plugin ls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
    /// The installed version (remote installs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// sha256 of the installed binary (remote installs only) — the supply-chain integrity anchor
    /// that D-48 re-checks at spawn time; absent means the descriptor is unverified/local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Where this install came from, e.g. `plugins-v0.2.0` (remote installs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The version in place before the last version switch (`pin`, or an `install` that changed
    /// versions) — what `flux plugin rollback` flips back to, offline, via the side-by-side
    /// versioned store (D-48).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// The git URL a `--git` source install (D-87) was cloned from — the provenance anchor of a
    /// from-source build (paired with `git_commit`). Present + a **missing** `sha256` is what makes
    /// a descriptor [`Verification::UnverifiedFromSource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    /// The resolved commit sha a `--git` source install (D-87) was built at — the second half of
    /// from-source provenance, shown before the build and recorded so a re-install of the same
    /// commit is an idempotent no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    /// The host authority this install is on record as granting (C-411) — the ceiling every later
    /// manifest fetch is measured against by [`PluginHost::manifest`]. Absent until the first fetch
    /// records what the plugin declared; see [`CapabilityGrant`] for the posture and
    /// [`GrantOfRecord`] for what it covers and why.
    ///
    /// **Last on purpose.** It is the descriptor's only TOML *table*, and `toml` refuses to emit a
    /// value after one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantOfRecord>,
    /// The file this descriptor was read from, set by [`load_descriptor`] and [`discover`] and
    /// **never serialized** — provenance, not configuration.
    ///
    /// It exists so the load path can write the grant of record back into the very file it came
    /// from without every caller having to thread the plugin store through. A descriptor built in
    /// memory (the SDK, tests) has none, and reports [`CapabilityGrant::NotRecorded`].
    #[serde(skip)]
    pub origin: Option<std::path::PathBuf>,
}

/// The outcome of re-hashing a descriptor's binary against its recorded `sha256` (D-48) — the
/// enforcement step that turns `pin` from an advisory label into a supply-chain statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// The binary on disk hashes to exactly the descriptor's recorded `sha256`.
    Verified,
    /// The descriptor records a `sha256` but the binary hashes differently (or cannot be read) —
    /// a hard spawn refusal, never a silent fallback.
    HashDrift { expected: String, actual: String },
    /// The descriptor carries no `sha256` (a dev/local `add` or `install --dir`) — spawns as
    /// always, visibly labeled `unverified (local)` in `ls`/`status`.
    UnverifiedLocal,
    /// The descriptor carries no `sha256` but *does* carry git provenance (`git_url`) — a `--git`
    /// source build (D-87). Building arbitrary source is code execution, so the install was gated
    /// behind explicit consent + commit disclosure; the resulting binary is trusted-because-built,
    /// not signed. Spawns exactly like [`Verification::UnverifiedLocal`] (never a `HashDrift`
    /// refusal), visibly labeled `from-source (unverified)` in `ls`/`status` — distinct from a
    /// signed pack and from a `--dir` local scan.
    UnverifiedFromSource,
}

/// Re-hash the binary a descriptor points at against its recorded `sha256`. A hashless descriptor
/// is [`Verification::UnverifiedFromSource`] when it carries git provenance (a `--git` build, D-87)
/// and [`Verification::UnverifiedLocal`] otherwise (`add`/`install --dir`); an unreadable binary
/// under a recorded hash is reported as drift (the read error stands in for the actual hash) —
/// verification never silently passes.
pub fn verify_descriptor(d: &PluginDescriptor) -> Verification {
    let Some(expected) = &d.sha256 else {
        return if d.git_url.is_some() {
            Verification::UnverifiedFromSource
        } else {
            Verification::UnverifiedLocal
        };
    };
    let actual = match std::fs::read(&d.program) {
        Ok(bytes) => pack::sha256_hex(&bytes),
        Err(e) => {
            return Verification::HashDrift {
                expected: expected.clone(),
                actual: format!("<unreadable: {e}>"),
            }
        }
    };
    if &actual == expected {
        Verification::Verified
    } else {
        Verification::HashDrift {
            expected: expected.clone(),
            actual,
        }
    }
}

/// A discovered plugin: its name (the descriptor file stem) and how to launch it.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub descriptor: PluginDescriptor,
}

/// The path a plugin descriptor for `name` would live at, after rejecting names that could
/// escape `dir` when joined. `Path::join` treats `..` and absolute components literally, so an
/// unsanitized name like `../../config` or `/etc/passwd` would resolve outside the plugins
/// directory — a destructive traversal for `remove_descriptor`. The single guard here covers
/// every caller (`add`/`load`/`remove`, and the pack store paths): a valid plugin name is a bare
/// file name with no path separators, no `..`/`.` component, and no absolute/prefix component.
fn descriptor_path(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    invalid_plugin_name(name)?;
    Ok(dir.join(format!("{name}.toml")))
}

/// Reject a plugin name that is not a bare file name. Empty, a path separator (`/` / `\`),
/// `..`, `.`, or an absolute / Windows-prefix component all fail — `Path::join` would otherwise
/// carry them literally out of the plugins directory.
pub(crate) fn invalid_plugin_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(Error::Other(format!(
            "invalid plugin name `{name}`: must be a bare file name (no path separators, `..`, or absolute components)"
        )));
    }
    use std::path::Component;
    for comp in std::path::Path::new(name).components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(Error::Other(format!(
                "invalid plugin name `{name}`: must be a bare file name (no path separators, `..`, or absolute components)"
            )));
        }
    }
    Ok(())
}

/// Read every `<dir>/*.toml` plugin descriptor (sorted by name). Missing dir → empty; malformed
/// descriptors are skipped (never fail discovery for one bad file).
pub fn discover(dir: &std::path::Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut descriptor) = toml::from_str::<PluginDescriptor>(&text) {
                descriptor.origin = Some(path);
                out.push(DiscoveredPlugin { name, descriptor });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write a plugin descriptor to `<dir>/<name>.toml`, creating `dir` if needed.
///
/// **The grant of record survives the write** (C-411). A descriptor that carries no
/// [`PluginDescriptor::capabilities`] inherits the one already stored under this name, so every
/// rewrite of *how* a plugin is launched — `install` onto a new version, `pin`, `rollback`,
/// re-running `add` — leaves *what it may ask the host for* exactly where the operator left it.
/// Owning the carry-forward here rather than at each caller is what makes it hold for a caller
/// that has not thought about it: an install that re-granted by accident is the whole failure this
/// rule exists to stop. Dropping the record is deliberate: remove the `[grant]` table (which keeps
/// `previous`/`version`/`sha256`, so `rollback` still works offline), or uninstall the plugin.
pub fn add_descriptor(
    dir: &std::path::Path,
    name: &str,
    descriptor: &PluginDescriptor,
) -> Result<()> {
    let path = descriptor_path(dir, name)?;
    // A stored descriptor that will not parse carries no record to keep, and must not turn a write
    // that used to succeed into a failure — the same "never fail for one bad file" posture
    // `discover` takes. There is nothing to lose either way: an unparseable descriptor was already
    // being overwritten wholesale.
    let carried = match &descriptor.grant {
        Some(_) => None,
        None => load_descriptor(dir, name)
            .ok()
            .flatten()
            .and_then(|stored| stored.grant),
    };
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let body = toml::to_string_pretty(&PluginDescriptor {
        grant: descriptor.grant.clone().or(carried),
        origin: None,
        ..descriptor.clone()
    })
    .map_err(|e| Error::Other(format!("serialize descriptor: {e}")))?;
    std::fs::write(path, body).map_err(Error::Io)?;
    Ok(())
}

/// Load a single named descriptor, if present. The result carries its
/// [`PluginDescriptor::origin`], so a load through it can record the grant of record back into the
/// file it came from (C-411).
pub fn load_descriptor(dir: &std::path::Path, name: &str) -> Result<Option<PluginDescriptor>> {
    let path = descriptor_path(dir, name)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut descriptor: PluginDescriptor = toml::from_str(&text)
                .map_err(|e| Error::Other(format!("parse descriptor: {e}")))?;
            descriptor.origin = Some(path);
            Ok(Some(descriptor))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Remove a plugin descriptor (`flux plugin uninstall`); returns whether a descriptor existed
/// (a missing name is `Ok(false)` — a clean "nothing to uninstall", not an error). Other IO
/// failures (permissions, etc.) propagate as `Err`.
pub fn remove_descriptor(dir: &std::path::Path, name: &str) -> Result<bool> {
    match std::fs::remove_file(descriptor_path(dir, name)?) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Io(e)),
    }
}
