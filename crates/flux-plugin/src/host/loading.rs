//! Subprocess lifecycle, tool projection, and descriptor discovery.

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
        const MAX_FRAME: usize = 8 * 1024 * 1024;
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_FRAME as u64)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Provider("plugin closed the connection".into()));
        }
        if buf.last() != Some(&b'\n') {
            return Err(Error::Provider(
                "plugin frame exceeded the size limit (no newline within bound)".into(),
            ));
        }
        let line = std::str::from_utf8(&buf)
            .map_err(|e| Error::Provider(format!("plugin frame not valid UTF-8: {e}")))?;
        let frame: Frame = serde_json::from_str(line.trim())?;
        // Every plugin→host frame carries the wire-protocol marker; checking it here — the single
        // choke point for inbound frames — turns an incompatible plugin into an actionable message
        // instead of a downstream deserialization failure (C-144).
        flux_plugin_protocol::check_protocol(&frame.protocol).map_err(Error::Provider)?;
        Ok(frame)
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

    /// Fetch the plugin's manifest.
    pub async fn manifest(&mut self) -> Result<PluginManifest> {
        let f = self.request("manifest", Value::Null).await?;
        if !f.ok {
            return Err(Error::Provider(f.error.unwrap_or_default()));
        }
        Ok(serde_json::from_value(f.result)?)
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
        }
    }
}

pub(super) fn plugin_tool_spec(
    plugin: &str,
    op: &OperationSpec,
    capabilities: &PluginCapabilities,
) -> (String, ToolSpec) {
    // Project the operation's declared effects so the authorization floor gates it like any
    // built-in tool. An operation that declares none could still touch the network or run a
    // process via host capabilities, so default to those — under the default grants that forces
    // approval rather than letting the op slip the envelope.
    let effects = if op.effects.is_empty() {
        vec![Effect::Process, Effect::Network]
    } else {
        op.effects.clone()
    };
    // The model-facing tool name is the operation's fully-qualified name. flux plugin ops are
    // authored already qualified (e.g. `slack.message.send`), and the plugin's own dispatch,
    // `flux plugin call`, and the generated skill docs all use that name — so adopt it verbatim when
    // it is already prefixed, and only add the `{plugin}.` prefix for an un-qualified op name.
    // (Unconditionally prefixing double-qualified the common case to `slack.slack.message.send`, so
    // an agent's `tools` grant — `slack.message.send` — never matched and every plugin op was
    // silently dropped from the agent surface.)
    let qualified = op.projected_name(plugin);
    let mut access = Vec::new();
    if capabilities.http {
        access.push(AccessKind::Network);
    }
    if !capabilities.conn.is_empty() {
        access.push(AccessKind::Connection);
    }
    if !capabilities.process.is_empty() {
        access.push(AccessKind::Process);
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
        vec![format!("{}.{}", self.plugin, self.operation)]
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

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
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
                // Mask the op's declared secret-like fields (GL-031) before the result is
                // stringified into model-visible output — a no-op unless the op declared any.
                redact_secret_fields(&mut v, &self.redact_fields);
                Ok(ToolResult::ok(
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
                ))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
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
pub async fn load_plugin_tools(
    system: &flux_system::System,
    name: &str,
    descriptor: &PluginDescriptor,
    make_caps: impl FnOnce(&PluginManifest) -> Arc<dyn HostCapabilities>,
) -> Result<LoadedPlugin> {
    let mut host = PluginHost::spawn_verified(system, name, descriptor).await?;
    let manifest = host.manifest().await?;
    validate_manifest_operations(&manifest).map_err(Error::Other)?;
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
    })
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
            if let Ok(descriptor) = toml::from_str::<PluginDescriptor>(&text) {
                out.push(DiscoveredPlugin { name, descriptor });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write a plugin descriptor to `<dir>/<name>.toml`, creating `dir` if needed.
pub fn add_descriptor(
    dir: &std::path::Path,
    name: &str,
    descriptor: &PluginDescriptor,
) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let body = toml::to_string_pretty(descriptor)
        .map_err(|e| Error::Other(format!("serialize descriptor: {e}")))?;
    std::fs::write(descriptor_path(dir, name)?, body).map_err(Error::Io)?;
    Ok(())
}

/// Load a single named descriptor, if present.
pub fn load_descriptor(dir: &std::path::Path, name: &str) -> Result<Option<PluginDescriptor>> {
    match std::fs::read_to_string(descriptor_path(dir, name)?) {
        Ok(text) => {
            Ok(Some(toml::from_str(&text).map_err(|e| {
                Error::Other(format!("parse descriptor: {e}"))
            })?))
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
