//! Catalog refresh (C-310): re-project a loaded plugin's operations from a **second** `manifest`
//! fetch on the already-open subprocess.
//!
//! [`PluginHost::manifest`] has always been a plain request over the live NDJSON channel rather
//! than a file read, so a plugin whose op set depends on remote state can already answer it
//! differently over time. What was missing was a caller: the manifest was fetched once at load and
//! the resulting [`ToolSpec`]s projected once, so an op that only becomes available after the
//! operator authenticates a provider needed a restart to appear. [`LoadedPlugin::refresh`] is that
//! caller.
//!
//! # A refresh is a re-grant, so it is bounded by the original grant
//!
//! The load-time grant is the operator's decision: `make_caps` binds the host capabilities to the
//! manifest that was fetched **once**, and everything downstream — the approval prompt, the policy
//! floor, the callback gates — is scoped by it. A second `manifest` answer is therefore an attempt
//! to restate that decision, and a plugin must not be able to win authority by restating it.
//!
//! Two independent mechanisms enforce that, because either alone would be a single point of
//! failure:
//!
//! 1. **The capabilities are pinned, not re-derived.** A refresh reuses the `Arc<dyn HostCapabilities>`
//!    built at load and never calls `make_caps` again. Whatever the refreshed manifest claims, the
//!    callback gates keep enforcing the grant the operator's session started with.
//! 2. **A widened declaration is refused outright** ([`capability_widenings`]). Pinning alone would
//!    leave the *declaration* and the *enforcement* disagreeing — the projected `ToolSpec` and the
//!    approval preview would describe programs, secret keys, HTTP hosts or dial targets the host
//!    would then refuse. Disclosure that overstates enforcement teaches an operator to grant policy
//!    they do not need, so the refresh stops instead.
//!
//! On top of those, an op that keeps its **name** across a refresh may not quietly become a
//! differently-scoped op ([`op_scope_weakenings`]): permission subjects, policy rules and session
//! grants all key on the op name, so silently re-pointing or de-tiering that name would reuse a
//! decision the operator made about something else.
//!
//! Everything else about the catalog is free to change — that is the point. Ops may appear and
//! disappear, and their schemas and descriptions may change.
//!
//! # All-or-nothing
//!
//! A refresh that fails at any step — a dead subprocess, an oversized or undecodable frame, a
//! manifest that fails validation, a widening, a re-scope — leaves the [`LoadedPlugin`] and the
//! caller's [`ToolRegistry`] exactly as they were. Nothing is mutated until every check has passed.

use super::loading::{op_coherence_warnings, plugin_tool_spec};
use super::*;
use flux_plugin_protocol::validate_manifest_operations;
use flux_runtime::ToolRegistry;

/// The result of a re-projection: the tools to install, and what changed relative to the catalog
/// that was in force before. Produced by [`LoadedPlugin::refresh`]; installed by
/// [`CatalogRefresh::apply`].
///
/// The three name lists are **projected** tool names (the registry keys, e.g. `zendesk.ticket.create`),
/// not raw operation names, so a caller can act on them without re-deriving the projection.
pub struct CatalogRefresh {
    /// The manifest the refresh accepted.
    pub manifest: PluginManifest,
    /// Every visible operation of the refreshed manifest, projected against the **load-time**
    /// host capabilities.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Ops the plugin now advertises that it did not before (sorted).
    pub added: Vec<String>,
    /// Ops the plugin has withdrawn (sorted). [`CatalogRefresh::apply`] removes these.
    pub removed: Vec<String>,
    /// Ops present before and after (sorted). Their tools are re-projected from the new manifest —
    /// a description or schema may have changed — but their gating scope is guaranteed not to have
    /// weakened.
    pub retained: Vec<String>,
    /// Metadata-coherence violations in the refreshed declarations (C-191), one sentence each.
    /// Computed exactly as at load, and — exactly as at load — non-empty does **not** refuse the
    /// refresh; see [`op_coherence_warnings`] for why an under-declaration is warned about.
    pub coherence_warnings: Vec<String>,
}

/// Hand-written because `Arc<dyn Tool>` is not `Debug`; the projected names carry the same
/// information a reader wants from `tools` anyway.
impl std::fmt::Debug for CatalogRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogRefresh")
            .field("plugin", &self.manifest.name)
            .field("added", &self.added)
            .field("removed", &self.removed)
            .field("retained", &self.retained)
            .field("coherence_warnings", &self.coherence_warnings)
            .finish()
    }
}

impl CatalogRefresh {
    /// Install this refresh into a live [`ToolRegistry`] under `source`, atomically: every
    /// previously projected op of this plugin is withdrawn and the refreshed set registered in one
    /// step. On any error the registry is untouched.
    ///
    /// Withdrawal is a real [`ToolRegistry::remove`], not a shadowing overwrite — an op the plugin
    /// no longer advertises stops being dispatchable, and its name is free again.
    ///
    /// A call already in flight is unaffected: it holds its own `Arc<dyn Tool>`, which keeps both
    /// the subprocess and the `ToolSpec` it was authorized under alive until it returns. Withdrawal
    /// governs future dispatch only — a refresh can never re-scope a call that is already running.
    pub fn apply(&self, registry: &mut ToolRegistry, source: &str) -> Result<()> {
        // Clone-then-swap, the same shape `try_register_all_from` uses: a failure part-way through
        // must not leave the caller with half a catalog.
        let mut assembled = registry.clone();
        for name in self.removed.iter().chain(self.retained.iter()) {
            assembled.remove(name);
        }
        assembled.try_register_all_from(source, self.tools.iter().cloned())?;
        *registry = assembled;
        Ok(())
    }
}

impl LoadedPlugin {
    /// Re-fetch this plugin's manifest over the open subprocess connection and re-project its
    /// operations, without restarting flux or respawning the plugin.
    ///
    /// Every load-time check runs again — [`validate_manifest_operations`], the capability
    /// projection, the authority contract and the coherence warnings — plus the two checks that
    /// exist only because a refresh is a *re-grant*: the refreshed manifest may not widen the
    /// granted capabilities, and a retained op may not weaken its gating scope. See the module
    /// docs for why both are needed.
    ///
    /// On success the plugin's own `tools`, `manifest` and `coherence_warnings` are updated, so a
    /// later refresh diffs against this catalog rather than against the load. On failure nothing
    /// is mutated and the error names what was refused.
    pub async fn refresh(&mut self) -> Result<CatalogRefresh> {
        let plugin = self.manifest.name.clone();
        let manifest = {
            let mut host = self.host.lock().await;
            host.manifest().await?
        };

        // A plugin renaming itself would move every projected op into another plugin's namespace
        // under grants that were never made for it.
        if manifest.name != plugin {
            return Err(Error::Other(format!(
                "plugin `{plugin}`: refusing the refreshed catalog — the manifest now names itself \
                 `{}`; a plugin cannot rename itself across a refresh",
                manifest.name
            )));
        }
        validate_manifest_operations(&manifest).map_err(Error::Other)?;

        let widenings = capability_widenings(&self.manifest.capabilities, &manifest.capabilities);
        if !widenings.is_empty() {
            return Err(Error::Other(format!(
                "plugin `{plugin}`: refusing the refreshed catalog — it requests more authority \
                 than the operator granted at load ({}). Restart flux to adopt a genuinely changed \
                 capability set; a refresh may add and remove operations, never capabilities.",
                widenings.join("; ")
            )));
        }

        let weakenings = retained_op_weakenings(&plugin, &self.manifest, &manifest);
        if !weakenings.is_empty() {
            return Err(Error::Other(format!(
                "plugin `{plugin}`: refusing the refreshed catalog — an operation keeps its name \
                 but not the scope it was gated under ({}). Policy rules, permission subjects and \
                 session grants all key on the operation name, so re-scoping one silently reuses a \
                 decision the operator made about something else.",
                weakenings.join("; ")
            )));
        }

        let coherence_warnings = op_coherence_warnings(&manifest);
        // Projected against the LOAD-TIME caps (`self.caps`), never a freshly derived set — see the
        // module docs. `make_caps` is deliberately not re-run.
        let tools: Vec<Arc<dyn Tool>> = visible_ops(&manifest)
            .map(|op| {
                Arc::new(PluginTool::new(
                    self.host.clone(),
                    self.caps.clone(),
                    &manifest,
                    op,
                )) as Arc<dyn Tool>
            })
            .collect();
        // The authority contract, checked before anything is swapped so an invalid projection is a
        // refusal rather than a half-applied catalog. `ToolRegistry::try_register_from` runs the
        // same check; running it here keeps a registry-less caller (the CLI) equally honest.
        for tool in &tools {
            let input = json!({});
            let subjects = tool.permission_subjects(&input);
            tool.authority_requirements(&input, &subjects)
                .map_err(|err| {
                    Error::Other(format!(
                        "plugin `{plugin}`: refreshed operation `{}` has an invalid authority \
                         contract: {err}",
                        tool.spec().name
                    ))
                })?;
        }

        let before: Vec<String> = projected_names(&self.manifest);
        let after: Vec<String> = projected_names(&manifest);
        let added = after
            .iter()
            .filter(|name| !before.contains(name))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|name| !after.contains(name))
            .cloned()
            .collect();
        let retained = before
            .iter()
            .filter(|name| after.contains(name))
            .cloned()
            .collect();

        // Every check has passed — only now is anything mutated.
        self.tools = tools.clone();
        self.manifest = manifest.clone();
        self.coherence_warnings = coherence_warnings.clone();
        Ok(CatalogRefresh {
            manifest,
            tools,
            added,
            removed,
            retained,
            coherence_warnings,
        })
    }
}

/// The projected (registry-key) names of a manifest's visible ops, sorted.
fn projected_names(manifest: &PluginManifest) -> Vec<String> {
    let mut names: Vec<String> = visible_ops(manifest)
        .map(|op| op.projected_name(&manifest.name))
        .collect();
    names.sort();
    names
}

/// Every way `refreshed` asks for more host authority than `granted`, one phrase each. Empty means
/// the refreshed manifest stays inside the operator's load-time grant.
///
/// Containment is **literal**: a refreshed entry must appear verbatim in the granted list. That is
/// deliberately stricter than the runtime matchers — a refreshed `"kubectl get"` under a granted
/// `"kubectl"` is a genuine narrowing and is still refused, and so is a `tcp:db:5432` under a
/// granted `tcp:*:5432`. The asymmetry is intentional: this is the last gate before a plugin's own
/// second answer re-states the operator's grant, and the cost of being wrong in the permissive
/// direction is a privilege escalation while the cost of being wrong in the strict direction is a
/// refusal the operator resolves with a restart. Grant grammars gain wildcards and prefix rules over
/// time; "must already be in the list" cannot drift.
pub(super) fn capability_widenings(
    granted: &PluginCapabilities,
    refreshed: &PluginCapabilities,
) -> Vec<String> {
    fn gained(field: &str, granted: &[String], refreshed: &[String]) -> Vec<String> {
        refreshed
            .iter()
            .filter(|entry| !granted.contains(entry))
            .map(|entry| format!("`{field}` gains `{entry}`"))
            .collect()
    }
    fn turned_on(field: &str, granted: bool, refreshed: bool) -> Option<String> {
        (refreshed && !granted).then(|| format!("`{field}` turns on"))
    }

    let mut widenings = Vec::new();
    widenings.extend(gained("process", &granted.process, &refreshed.process));
    widenings.extend(gained("secrets", &granted.secrets, &refreshed.secrets));
    widenings.extend(gained(
        "http_hosts",
        &granted.http_hosts,
        &refreshed.http_hosts,
    ));
    widenings.extend(gained(
        "private_hosts",
        &granted.private_hosts,
        &refreshed.private_hosts,
    ));
    widenings.extend(gained("conn", &granted.conn, &refreshed.conn));
    // `FsReadScope` is compared whole, so flipping a scope's `secret` flag off — which would stop
    // its contents being registered with the Redactor — counts as a widening too.
    widenings.extend(
        refreshed
            .fs
            .iter()
            .filter(|scope| !granted.fs.contains(scope))
            .map(|scope| format!("`fs` gains `{}` (secret: {})", scope.path, scope.secret)),
    );
    widenings.extend(turned_on("http", granted.http, refreshed.http));
    widenings.extend(turned_on("blob", granted.blob, refreshed.blob));
    widenings.extend(turned_on("discover", granted.discover, refreshed.discover));
    widenings.extend(turned_on(
        "credential",
        granted.credential,
        refreshed.credential,
    ));
    widenings
}

/// Every retained operation whose gating scope weakened, prefixed with the op's projected name.
fn retained_op_weakenings(
    plugin: &str,
    granted: &PluginManifest,
    refreshed: &PluginManifest,
) -> Vec<String> {
    let mut all = Vec::new();
    for new_op in visible_ops(refreshed) {
        let name = new_op.projected_name(plugin);
        let Some(old_op) = visible_ops(granted).find(|op| op.projected_name(plugin) == name) else {
            continue;
        };
        for weakening in op_scope_weakenings(plugin, &granted.capabilities, old_op, new_op) {
            all.push(format!("`{name}` {weakening}"));
        }
    }
    all
}

/// Every way the retained op `new` gates less than `old` did, one phrase each.
///
/// Both are projected against the **granted** capabilities — which the caller has already proved
/// the refreshed manifest does not exceed — so any difference here comes from the operation's own
/// declaration rather than from a capability change.
pub(super) fn op_scope_weakenings(
    plugin: &str,
    granted: &PluginCapabilities,
    old: &OperationSpec,
    new: &OperationSpec,
) -> Vec<String> {
    let (_, old_spec) = plugin_tool_spec(plugin, old, granted);
    let (_, new_spec) = plugin_tool_spec(plugin, new, granted);
    let mut weakenings = Vec::new();

    // The dispatch identity behind a stable public name. Re-pointing it would send a call the
    // operator authorized for one operation to a different one in the subprocess.
    if new.name != old.name {
        weakenings.push(format!(
            "now dispatches to `{}` instead of `{}`",
            new.name, old.name
        ));
    }
    if new_spec.risk < old_spec.risk {
        weakenings.push(format!(
            "drops `risk` from {:?} to {:?}",
            old_spec.risk, new_spec.risk
        ));
    }
    for effect in &old_spec.effects {
        if !new_spec.effects.contains(effect) {
            weakenings.push(format!("no longer declares the effect {effect:?}"));
        }
    }
    for access in &old_spec.access {
        if !new_spec.access.contains(access) {
            weakenings.push(format!("no longer declares {access:?} access"));
        }
    }
    for purpose in &old.secret_purposes {
        if !new.secret_purposes.contains(purpose) {
            weakenings.push(format!("no longer declares the secret purpose `{purpose}`"));
        }
    }
    // The per-operation `process` narrowing (C-90) is enforced per call by `OpScopedCaps`, so
    // dropping or broadening it genuinely widens what the op may run — not merely what it discloses.
    if !old.process.is_empty() {
        if new.process.is_empty() {
            weakenings.push(
                "drops its per-operation `process` narrowing, widening it to the manifest-wide grant"
                    .to_string(),
            );
        } else {
            for entry in &new.process {
                if !old.process.contains(entry) {
                    weakenings.push(format!("widens its `process` narrowing with `{entry}`"));
                }
            }
        }
    }
    weakenings
}
