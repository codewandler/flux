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
//! Three mechanisms enforce that. The first two are a matched pair — the authority a plugin's ops
//! *declare* and the authority the host *enforces* must be one thing, and each direction of
//! disagreement is dangerous in its own way:
//!
//! 1. **The enforced capabilities are pinned, not re-derived.** A refresh reuses the
//!    `Arc<dyn HostCapabilities>` built at load and never calls `make_caps` again. Whatever the
//!    refreshed manifest claims, the callback gates keep enforcing the grant the operator's session
//!    started with.
//! 2. **The declared capabilities are pinned too** ([`LoadedPlugin::pin_granted_authority`]).
//!    Pinning only the enforcement is not enough, because an op's `access` — and through it every
//!    `AuthorityRequirement` the authorization floor reads — is derived from the *manifest's own*
//!    capability declaration. Leaving that half mutable lets the two disagree, and the **narrowing**
//!    direction is the worse one: a manifest that surrenders `secrets`/`http`/`conn` projects an op
//!    requiring no authority at all while the pinned caps still hand it the raw secret. Pinning both
//!    halves to one value makes the disagreement unrepresentable rather than merely checked.
//! 3. **A widened declaration is refused outright** ([`capability_widenings`]). Pinning silently
//!    ignores a widening, which would leave the operator's approval preview describing less
//!    authority than the plugin asked for; refusing says so instead. It is also the check that
//!    keeps a surrender safe to ignore — because the pinned grant is, by then, known to be a
//!    superset of what the plugin last asked for.
//!
//! On top of those, an op that keeps its **name** across a refresh may not quietly become a
//! differently-scoped op ([`op_scope_weakenings`]): permission subjects, policy rules and session
//! grants all key on the op name, so silently re-pointing or de-tiering that name would reuse a
//! decision the operator made about something else.
//!
//! The net rule: **a refresh changes the operation set, never the grant.** Ops may appear and
//! disappear and their schemas may change; the capability declaration and the capability
//! enforcement both stay at the load-time grant until a restart makes it again.
//!
//! "Until a restart makes it again" was, until C-411, an unbounded escape: a restart re-derived the
//! grant from whatever the plugin's manifest declared *that* time, so a widening merely had to wait
//! for one. [`PluginHost::manifest`](super::PluginHost::manifest) now applies the same asymmetry at
//! every fetch, against the grant persisted in the descriptor —
//! [`GrantOfRecord`](super::GrantOfRecord), which covers the same five manifest fields
//! [`LoadedPlugin::pin_granted_authority`] pins here and reuses [`capability_widenings`] for the
//! capability half, so both boundaries answer "is this more authority?" identically.
//!
//! The two are not redundant. This module's checks are the stricter pair — they bound a refresh to
//! *this session's* load-time manifest, which may be narrower than the record — and they are what
//! catches a re-scoped op, which the record says nothing about. The record bounds what a **new
//! process** may start from, which nothing here can see.
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
            // Withdraw only what this plugin's own source registered. `ToolRegistry::remove` is
            // name-keyed and source-blind, so an unguarded removal would let a refresh silently
            // evict an identically named op belonging to another pack — a privilege swap by
            // collision. Leaving it in place turns that case into the duplicate error below.
            if assembled.source(name) == Some(source) {
                assembled.remove(name);
            }
        }
        assembled.try_register_all_from(source, self.tools.iter().cloned())?;
        *registry = assembled;
        Ok(())
    }
}

impl LoadedPlugin {
    /// Re-fetch this plugin's manifest over the open subprocess connection, re-project its
    /// operations, and install them into `registry` — without restarting flux or respawning the
    /// plugin. **The entry point to prefer**: the registry and the plugin move together or not at
    /// all.
    ///
    /// The registry is written first, because it is the fallible half (a refreshed op can collide
    /// with a name another source already registered). Only once it has been swapped is the
    /// plugin's own catalog committed, so a rejected `apply` can never leave the plugin believing
    /// it published ops the registry never took — a divergence that would strand those names
    /// forever, since the next refresh would diff against the newer manifest and never withdraw
    /// them.
    pub async fn refresh_into(
        &mut self,
        registry: &mut ToolRegistry,
        source: &str,
    ) -> Result<CatalogRefresh> {
        let prepared = self.prepare_refresh().await?;
        prepared.apply(registry, source)?;
        Ok(self.commit(prepared))
    }

    /// Re-fetch and re-project without a registry, committing the result to this plugin.
    ///
    /// For a caller that holds no [`ToolRegistry`] (or holds several). If you do hold one, prefer
    /// [`LoadedPlugin::refresh_into`] — pairing this with a separate [`CatalogRefresh::apply`]
    /// commits the plugin before the registry, so a failed `apply` diverges the two.
    pub async fn refresh(&mut self) -> Result<CatalogRefresh> {
        let prepared = self.prepare_refresh().await?;
        Ok(self.commit(prepared))
    }

    /// Fetch, validate, and project — with **no mutation of `self`**, so every refusal below is
    /// automatically all-or-nothing.
    ///
    /// Every load-time check runs again — [`validate_manifest_operations`], the capability
    /// projection, the authority contract and the coherence warnings — plus the two that exist
    /// only because a refresh is a *re-grant*: the refreshed manifest may not widen the granted
    /// capabilities, and a retained op may not weaken its gating scope.
    async fn prepare_refresh(&self) -> Result<CatalogRefresh> {
        let plugin = self.manifest.name.clone();
        let fetched = {
            let mut host = self.host.lock().await;
            host.manifest().await?
        };

        // A plugin renaming itself would move every projected op into another plugin's namespace
        // under grants that were never made for it.
        if fetched.name != plugin {
            return Err(Error::Other(format!(
                "plugin `{plugin}`: refusing the refreshed catalog — the manifest now names itself \
                 `{}`; a plugin cannot rename itself across a refresh",
                fetched.name
            )));
        }
        // Validated as the plugin actually answered it, exactly as at load — so an authoring error
        // in the refreshed declaration reads the same either way.
        validate_manifest_operations(&fetched).map_err(Error::Other)?;

        let widenings = capability_widenings(&self.manifest.capabilities, &fetched.capabilities);
        if !widenings.is_empty() {
            return Err(Error::Other(format!(
                "plugin `{plugin}`: refusing the refreshed catalog — it requests more authority \
                 than the operator granted at load ({}). Restart flux to adopt a genuinely changed \
                 capability set; a refresh may add and remove operations, never capabilities.",
                widenings.join("; ")
            )));
        }

        let manifest = self.pin_granted_authority(fetched);
        // Cheap re-run against the manifest actually installed. Literal containment makes the grant
        // matchers monotone, so this cannot fail where the check above passed — it is here so that
        // stays true by test rather than by argument.
        validate_manifest_operations(&manifest).map_err(Error::Other)?;

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
        // Projected from the pinned manifest against the LOAD-TIME caps (`self.caps`) — the same
        // authority declaration on both sides. `make_caps` is deliberately never re-run.
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
        // same check; running it here keeps a registry-less caller equally honest.
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

        Ok(CatalogRefresh {
            manifest,
            tools,
            added,
            removed,
            retained,
            coherence_warnings,
        })
    }

    /// Carry the operator's grant across the refresh: replace every manifest field the host
    /// capability layer was built from with the load-time one, keeping only the operations (and the
    /// descriptive surface) the plugin just sent.
    ///
    /// # Why the declaration is pinned, not just the enforcement
    ///
    /// `self.caps` is fixed at load, but a plugin's `ToolSpec` — its `access`, and through it every
    /// [`AuthorityRequirement`] the authorization floor reads — is *derived from the manifest's own
    /// capability declaration* (`plugin_tool_spec`). Projecting a refreshed declaration against
    /// pinned enforcement lets the two disagree, and the dangerous direction is a **surrender**: a
    /// manifest that drops `secrets`/`http`/`conn` projects an op that requires **no** authority at
    /// all while the pinned caps still hand it the raw secret, still reach the declared host, and
    /// still run the granted program. That is the failure `plugin_tool_spec` already warns about
    /// for a different reason — an op with neither effects nor access "would carry NO requirement
    /// at all and skip the authorization floor entirely" — and it is worse than the widening this
    /// module's second mechanism catches: overstating teaches an operator to over-grant, while
    /// understating removes the requirement to grant at all.
    ///
    /// Pinning the declaration makes the disagreement unrepresentable rather than merely checked:
    /// the spec the registry installs and the capabilities the host enforces are computed from one
    /// value. A surrender is therefore accepted and ignored — the grant of record stands until a
    /// restart makes it again.
    ///
    /// # The classification of every manifest field (C-322)
    ///
    /// **PINNED** — the operator's load-time grant, re-stated here so the refreshed answer cannot
    /// move it:
    ///
    /// - `capabilities` — the grant itself; [`SystemHostCaps::with_manifest`] installs it verbatim.
    /// - `auth` — read by `with_manifest`; the host resolves secrets *by declared purpose*, so a
    ///   refreshed purpose list would redirect which secret an op is handed.
    /// - `endpoints` — read by `with_manifest`, and a second egress surface: the host admits a
    ///   plugin's declared endpoint hosts alongside `http_hosts`, so leaving it mutable would let
    ///   the stored manifest advertise reach the pinned caps do not back.
    /// - `config` — read by `with_manifest`; the surface the gated `config` capability exposes
    ///   (D-32), and the substitution source for endpoint templates.
    /// - `discovers` — *not* read by `with_manifest`, and pinned anyway. It is the provider side of
    ///   the D-26 discovery fan-out: `PluginRegistry::providers_for` routes a consumer's query for
    ///   product X to every plugin whose manifest `discovers` X, and the broker commits what that
    ///   provider answers into the shared `EndpointRegistry` other components resolve against.
    ///   Enlisting for a new product across a refresh is a plugin appointing itself the authority on
    ///   where that product lives — and `plugin list` discloses `discovers` in the approval surface,
    ///   so it is something the operator reviewed and granted for a specific set. It is inert today
    ///   only because `ProviderEntry` snapshots the manifest at load and refresh never re-registers
    ///   it; [C-318] wires refresh into a live session and removes that accident.
    ///
    /// **ADOPTED** — the point of a refresh, or descriptive only:
    ///
    /// - `operations` — *the* thing a refresh exists to change. Safe because retained names are
    ///   guarded by [`retained_op_weakenings`] and new ops are re-validated against the **pinned**
    ///   capabilities by the second [`validate_manifest_operations`] call in
    ///   [`LoadedPlugin::prepare_refresh`].
    /// - `name` — cannot change at all: a rename is a hard refusal earlier in `prepare_refresh`,
    ///   before this runs, so `fetched.name` is already `self.manifest.name`. Adopted rather than
    ///   pinned so that the refusal stays the single place a rename is handled; pinning here would
    ///   silently *accept* a rename this function was never meant to adjudicate.
    /// - `version` — descriptive; nothing reads it for a decision.
    /// - `groups` — tool organisation only, and consumed once at load when the assembly is built;
    ///   no authority attaches to a group.
    /// - `datasources` — display-only at both of its consumers.
    ///
    /// The destructure below is exhaustive **on purpose**, exactly as [`capability_widenings`] is
    /// for `PluginCapabilities`: a `..fetched` struct-update would adopt a newly added field from
    /// the plugin's *second* answer in silence, which is how this module's round-1 defect would come
    /// back on a new surface. Adding a field to `PluginManifest` reds here and at its test anchor
    /// (`every_manifest_field_is_classified_pinned_or_adopted`) until someone classifies it above.
    ///
    /// [C-318]: https://github.com/codewandler/flux/blob/main/docs/stories/C-318-live-session-registry-refresh.md
    fn pin_granted_authority(&self, fetched: PluginManifest) -> PluginManifest {
        let PluginManifest {
            name,
            version,
            operations,
            datasources,
            groups,
            // Pinned: bound and dropped, so a reader sees at a glance that the refreshed value is
            // deliberately discarded rather than merely forgotten.
            auth: _,
            endpoints: _,
            config: _,
            discovers: _,
            capabilities: _,
        } = fetched;

        PluginManifest {
            name,
            version,
            operations,
            datasources,
            groups,
            auth: self.manifest.auth.clone(),
            endpoints: self.manifest.endpoints.clone(),
            config: self.manifest.config.clone(),
            discovers: self.manifest.discovers.clone(),
            capabilities: self.manifest.capabilities.clone(),
        }
    }

    /// Adopt a prepared refresh. Infallible by construction — every check ran in
    /// [`LoadedPlugin::prepare_refresh`] — so callers can order it after the registry write.
    fn commit(&mut self, refresh: CatalogRefresh) -> CatalogRefresh {
        self.tools = refresh.tools.clone();
        self.manifest = refresh.manifest.clone();
        self.coherence_warnings = refresh.coherence_warnings.clone();
        refresh
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

    // Destructured, not field-accessed, on purpose: `PluginCapabilities` is the deny-by-default
    // authority surface, and an eleventh field must not be able to land unchecked. Adding one reds
    // this function until it is classified here.
    let PluginCapabilities {
        process,
        secrets,
        http,
        http_hosts,
        private_hosts,
        conn,
        blob,
        discover,
        credential,
        fs,
    } = refreshed;

    let mut widenings = Vec::new();
    widenings.extend(gained("process", &granted.process, process));
    widenings.extend(gained("secrets", &granted.secrets, secrets));
    widenings.extend(gained("http_hosts", &granted.http_hosts, http_hosts));
    widenings.extend(gained(
        "private_hosts",
        &granted.private_hosts,
        private_hosts,
    ));
    widenings.extend(gained("conn", &granted.conn, conn));
    // `FsReadScope` is compared whole, so flipping a scope's `secret` flag off — which would stop
    // its contents being registered with the Redactor — counts as a widening too.
    widenings.extend(
        fs.iter()
            .filter(|scope| !granted.fs.contains(scope))
            .map(|scope| format!("`fs` gains `{}` (secret: {})", scope.path, scope.secret)),
    );
    widenings.extend(turned_on("http", granted.http, *http));
    widenings.extend(turned_on("blob", granted.blob, *blob));
    widenings.extend(turned_on("discover", granted.discover, *discover));
    widenings.extend(turned_on("credential", granted.credential, *credential));
    widenings
}

/// Every retained operation whose gating scope weakened, prefixed with the op's projected name.
///
/// `refreshed` must already be the **pinned** manifest ([`LoadedPlugin::pin_granted_authority`]),
/// so its `capabilities` are `granted`'s: both sides are then projected against the very
/// declaration the installed tools carry, and a difference can only come from the operation's own
/// fields. Comparing against a manifest whose capabilities had not been pinned would make this
/// check blind to exactly the capability drift the projection introduced.
fn retained_op_weakenings(
    plugin: &str,
    granted: &PluginManifest,
    refreshed: &PluginManifest,
) -> Vec<String> {
    // Two-way containment is equality under the literal rule `capability_widenings` applies, and it
    // needs no `PartialEq` on the wire struct (which lives on the independently versioned protocol
    // line and is not worth a derive for an assertion).
    // Two-way containment is equality under the literal rule `capability_widenings` applies, and it
    // needs no `PartialEq` on the wire struct (which lives on the independently versioned protocol
    // line and is not worth a derive for an assertion).
    debug_assert!(
        capability_widenings(&granted.capabilities, &refreshed.capabilities).is_empty()
            && capability_widenings(&refreshed.capabilities, &granted.capabilities).is_empty(),
        "retained-op comparison requires the pinned manifest"
    );
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
/// Both are projected against `granted` — which, because the refreshed manifest's authority fields
/// are pinned to it before this runs, is also the declaration the *installed* tool carries. So the
/// comparison describes the specs the registry actually gets, not a hypothetical pair.
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
    // C-312 — the platform-sourcing declaration is what installs the credential boundary on this
    // op's responses, so shedding or downgrading it removes a check the operator's session was
    // running under. Ranked rather than compared for equality: `Activation` is `Operation` plus one
    // more check, so tightening is free and only a drop in `strictness` is a weakening. This is the
    // same class as dropping a `process` narrowing — the declaration is a gate, not a label.
    if new.platform.strictness() < old.platform.strictness() {
        weakenings.push(format!(
            "drops platform sourcing from {:?} to {:?}, removing the credential boundary from its \
             responses",
            old.platform, new.platform
        ));
    }
    // C-311 — the vendor-reach disclosure is the compensating control for this seam, and a refresh
    // is a re-grant: an op the operator approved knowing it reached `api.zendesk.com` must not keep
    // its name while going quiet, or while pointing somewhere else. Ranked like `platform` above —
    // naming a vendor discloses more than "served locally", which discloses more than silence — and
    // a *changed* host is a weakening too, because the approval the session is carrying was given
    // about the old one.
    if new.reaches.disclosure_rank() < old.reaches.disclosure_rank() {
        weakenings.push(format!(
            "drops its vendor-reach disclosure from {:?} to {:?}",
            old.reaches, new.reaches
        ));
    } else if let (Some(old_host), Some(new_host)) = (old.reaches.host(), new.reaches.host()) {
        if old_host != new_host {
            weakenings.push(format!(
                "re-points its declared vendor host from `{old_host}` to `{new_host}`"
            ));
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
