//! `flux-codegate` — the architecture lint (fluxplane's `codegate` analog).
//!
//! flux's crates are stratified into layers; a crate may depend only on its own layer or lower
//! ones. This crate encodes the layer of every workspace crate and a pure [`violations`] checker;
//! its test reads each `crates/*/Cargo.toml` and fails the build on any inner→outer dependency (or
//! any unclassified crate). Run via `cargo test -p flux-codegate`.
//!
//! Note the deliberate placements that keep the deep decisions honest: `flux-evidence`, `flux-skill`,
//! `flux-config`, and `flux-lang` are **L0 leaves** (no flux deps beyond other L0), so the
//! runtime/agent layers may depend on them. `flux-lang` is the Flux-Lang language **and its reference
//! interpreter**: its L0-purity means "no L1+ flux deps; all effects (op dispatch, value store,
//! observation sink) injected via traits" — not "no async/IO" (it uses tokio). And `flux-auth` is L5,
//! so `flux-runtime` (L2) must NOT depend on it — surfaces resolve identity and pass `(Caller, Trust)` in.

/// The layer of a flux crate (0 = innermost contracts, 6 = outermost surfaces), or `None` if the
/// crate is unknown (which the lint treats as a failure — new crates must be classified here).
pub fn layer(name: &str) -> Option<u8> {
    // The published flux-sdk/flux-providers closure carries a `codewandler-` vanity prefix on its
    // crates.io *package* names (the bare `flux-*` names are squatted); the crate keeps its bare
    // identity everywhere else (import paths, `[workspace.dependencies]` alias keys). Normalize so
    // the layer map stays keyed on the logical `flux-*` name.
    let name = name.strip_prefix("codewandler-").unwrap_or(name);
    Some(match name {
        // L0 — pure contracts: no IO, no flux deps except other L0. Safe for anything to use.
        "flux-core"
        | "flux-policy"
        | "flux-secret"
        | "flux-spec"
        | "flux-config"
        | "flux-evidence"
        | "flux-skill"
        | "flux-lang"
        | "flux-markdown"
        | "flux-datasource"
        | "flux-audio"
        | "flux-plugin-protocol" => 0,
        // L1 — the provider abstraction, the concrete providers (Anthropic/OpenAI/OpenRouter/
        // Ollama + the shared Messages protocol core, all in `flux-providers`), credentials, the
        // A2A agent-protocol client + wire types (`flux-a2a`; no flux deps — a network client), and
        // the Postgres driver-owner (`flux-pg`; owns the sole sqlx dep + pool + sync↔async bridge)
        "flux-provider" | "flux-providers" | "flux-credentials" | "flux-a2a" | "flux-pg" => 1,
        // L2 — runtime: execution + guarded IO + the safety envelope (context-projector module
        // now lives inside flux-runtime)
        "flux-system" | "flux-runtime" | "flux-tools" | "flux-events" => 2,
        // L3 — agent + orchestration + eval/self-improvement harness + cognition ops
        "flux-agent" | "flux-orchestrate" | "flux-flow" | "flux-eval" | "flux-cognition" => 3,
        // L4 — extensibility (subprocess plugins + the JS pre-tool hooks module)
        "flux-plugin" => 4,
        // L5 — heavy capabilities (web + datasource tools in flux-capabilities; caller identity
        // in flux-auth, kept separate as a distinct concern from tool capabilities)
        "flux-capabilities" | "flux-auth" | "flux-web" => 5,
        // L6 — surfaces / apps (and this lint crate itself)
        "flux-sdk" | "flux-server" | "flux-tui" | "flux-cli" | "flux-codegate" | "flux-app"
        | "flux-channels" | "flux-lsp" => 6,
        _ => return None,
    })
}

/// Check a `(crate, its flux-* dependencies)` graph for layering violations. Returns a human-
/// readable message per problem: an unclassified crate, or a dependency on a higher layer.
pub fn violations(deps_by_crate: &[(String, Vec<String>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (krate, deps) in deps_by_crate {
        let Some(kl) = layer(krate) else {
            out.push(format!(
                "crate `{krate}` is not classified in the layer map"
            ));
            continue;
        };
        for dep in deps {
            match layer(dep) {
                Some(dl) if dl > kl => out.push(format!(
                    "layering violation: `{krate}` (L{kl}) depends on `{dep}` (L{dl})"
                )),
                None => out.push(format!(
                    "`{krate}` depends on unclassified flux crate `{dep}`"
                )),
                _ => {}
            }
        }
    }
    out
}

use proc_macro2::Span;
use std::collections::{BTreeSet, HashMap};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Which unguarded process constructor a syntax-tree scan resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessApi {
    Std,
    Tokio,
}

/// One production raw-process construction resolved from a Rust syntax tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RawProcessCommand {
    pub line: usize,
    pub api: ProcessApi,
    /// Nearest containing function/method, or `<module>` for a module-level initializer.
    pub function: String,
}

#[derive(Default)]
struct ProcessAliases {
    commands: HashMap<String, ProcessApi>,
    modules: HashMap<String, ProcessApi>,
    type_aliases: Vec<(String, syn::Path)>,
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr.meta.require_list().is_ok_and(|list| {
                list.tokens
                    .to_string()
                    .split_whitespace()
                    .any(|t| t == "test")
            })
    })
}

fn direct_process_type(segments: &[String]) -> Option<ProcessApi> {
    match segments {
        [root, process, command]
            if root == "std" && process == "process" && command == "Command" =>
        {
            Some(ProcessApi::Std)
        }
        [root, process, command]
            if root == "tokio" && process == "process" && command == "Command" =>
        {
            Some(ProcessApi::Tokio)
        }
        _ => None,
    }
}

impl ProcessAliases {
    fn resolve_type_segments(&self, segments: &[String]) -> Option<ProcessApi> {
        direct_process_type(segments).or_else(|| match segments {
            [command] => self.commands.get(command).copied(),
            [module, command] if command == "Command" => self.modules.get(module).copied(),
            _ => None,
        })
    }

    fn resolve_path(&self, path: &syn::Path) -> Option<ProcessApi> {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        self.resolve_type_segments(&segments)
    }

    fn add_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.add_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                if name.ident == "self" {
                    if let Some(api) = process_module(prefix) {
                        if let Some(local) = prefix.last() {
                            self.modules.insert(local.clone(), api);
                        }
                    }
                    return;
                }
                prefix.push(name.ident.to_string());
                if let Some(api) = direct_process_type(prefix) {
                    self.commands.insert(name.ident.to_string(), api);
                }
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                if rename.ident == "self" {
                    if let Some(api) = process_module(prefix) {
                        self.modules.insert(rename.rename.to_string(), api);
                    }
                    return;
                }
                prefix.push(rename.ident.to_string());
                if let Some(api) = direct_process_type(prefix) {
                    self.commands.insert(rename.rename.to_string(), api);
                } else if let Some(api) = process_module(prefix) {
                    self.modules.insert(rename.rename.to_string(), api);
                }
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.add_use(item, prefix);
                }
            }
            syn::UseTree::Glob(_) => {
                if let Some(api) = process_module(prefix) {
                    self.commands.insert("Command".into(), api);
                }
            }
        }
    }

    fn resolve_type_aliases(&mut self) {
        loop {
            let mut changed = false;
            for (alias, path) in &self.type_aliases {
                if self.commands.contains_key(alias) {
                    continue;
                }
                if let Some(api) = self.resolve_path(path) {
                    self.commands.insert(alias.clone(), api);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

fn process_module(segments: &[String]) -> Option<ProcessApi> {
    match segments {
        [root, process] if root == "std" && process == "process" => Some(ProcessApi::Std),
        [root, process] if root == "tokio" && process == "process" => Some(ProcessApi::Tokio),
        _ => None,
    }
}

struct AliasCollector<'a>(&'a mut ProcessAliases);

impl<'ast> Visit<'ast> for AliasCollector<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !has_cfg_test(&item.attrs) {
            self.0.add_use(&item.tree, &mut Vec::new());
        }
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        if let syn::Type::Path(path) = item.ty.as_ref() {
            self.0
                .type_aliases
                .push((item.ident.to_string(), path.path.clone()));
        }
        syn::visit::visit_item_type(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_trait_item_fn(self, item);
        }
    }
}

struct ProcessVisitor<'a> {
    aliases: &'a ProcessAliases,
    functions: Vec<String>,
    hits: BTreeSet<RawProcessCommand>,
}

impl ProcessVisitor<'_> {
    fn record_call(&mut self, call: &syn::ExprCall) {
        let syn::Expr::Path(function) = call.func.as_ref() else {
            return;
        };
        let mut segments: Vec<String> = function
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if segments.len() < 2 {
            return;
        }
        // Any associated constructor on either Command type creates a raw process builder. This
        // covers `new`, `from`, and future constructors without maintaining a spelling blacklist.
        segments.pop();
        let Some(api) = self.aliases.resolve_type_segments(&segments) else {
            return;
        };
        self.hits.insert(RawProcessCommand {
            line: start_line(function.path.span()),
            api,
            function: self
                .functions
                .last()
                .cloned()
                .unwrap_or_else(|| "<module>".into()),
        });
    }
}

fn start_line(span: Span) -> usize {
    span.start().line.max(1)
}

impl<'ast> Visit<'ast> for ProcessVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_trait_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        self.record_call(call);
        syn::visit::visit_expr_call(self, call);
    }
}

/// Resolve production raw-process constructions in one Rust source file through imports, renamed
/// imports, module aliases, type aliases, and multiline syntax. Test-only items are excluded by
/// their parsed `cfg(test)` attributes; comments and string literals are naturally invisible.
pub fn raw_process_commands(src: &str) -> syn::Result<Vec<RawProcessCommand>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ProcessAliases::default();
    AliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = ProcessVisitor {
        aliases: &aliases,
        functions: Vec::new(),
        hits: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.hits.into_iter().collect())
}

/// Backwards-compatible line-only view of [`raw_process_commands`]. A parse failure is returned as
/// line 1 so callers that have not migrated to the fallible API still fail closed.
pub fn raw_process_command_lines(src: &str) -> Vec<usize> {
    raw_process_commands(src)
        .map(|hits| hits.into_iter().map(|hit| hit.line).collect())
        .unwrap_or_else(|_| vec![1])
}

/// One raw filesystem call in a function that names project-controlled metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RawProjectMetadataIo {
    pub line: usize,
    pub function: String,
}

#[derive(Default)]
struct FsAliases {
    modules: BTreeSet<String>,
    types: BTreeSet<String>,
    functions: BTreeSet<String>,
}

fn fs_module(segments: &[String]) -> bool {
    matches!(segments, [root, fs] if (root == "std" || root == "tokio") && fs == "fs")
}

impl FsAliases {
    fn add_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.add_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                if name.ident == "self" {
                    if fs_module(prefix) {
                        if let Some(local) = prefix.last() {
                            self.modules.insert(local.clone());
                        }
                    }
                    return;
                }
                prefix.push(name.ident.to_string());
                self.add_fs_leaf(prefix, name.ident.to_string());
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                if rename.ident == "self" {
                    if fs_module(prefix) {
                        self.modules.insert(rename.rename.to_string());
                    }
                    return;
                }
                prefix.push(rename.ident.to_string());
                if fs_module(prefix) {
                    self.modules.insert(rename.rename.to_string());
                } else if prefix.len() >= 3 && fs_module(&prefix[..2]) {
                    if matches!(rename.ident.to_string().as_str(), "File" | "OpenOptions") {
                        self.types.insert(rename.rename.to_string());
                    } else {
                        self.functions.insert(rename.rename.to_string());
                    }
                }
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.add_use(tree, prefix);
                }
            }
            syn::UseTree::Glob(_) => {
                if fs_module(prefix) {
                    for function in [
                        "read",
                        "read_to_string",
                        "write",
                        "copy",
                        "rename",
                        "remove_file",
                        "remove_dir_all",
                        "create_dir",
                        "create_dir_all",
                        "canonicalize",
                        "metadata",
                        "symlink_metadata",
                        "read_dir",
                    ] {
                        self.functions.insert(function.into());
                    }
                    self.types.insert("File".into());
                    self.types.insert("OpenOptions".into());
                }
            }
        }
    }

    fn add_fs_leaf(&mut self, segments: &[String], local: String) {
        if fs_module(segments) {
            self.modules.insert(local);
        } else if segments.len() >= 3 && fs_module(&segments[..2]) {
            if matches!(
                segments.last().map(String::as_str),
                Some("File" | "OpenOptions")
            ) {
                self.types.insert(local);
            } else {
                self.functions.insert(local);
            }
        }
    }

    fn resolves_call(&self, segments: &[String]) -> bool {
        if segments.len() >= 3 && fs_module(&segments[..2]) {
            return true;
        }
        match segments {
            [function] => self.functions.contains(function),
            [owner, _function] => self.modules.contains(owner) || self.types.contains(owner),
            [module, _ty, _function] => self.modules.contains(module),
            _ => false,
        }
    }
}

struct FsAliasCollector<'a>(&'a mut FsAliases);

impl<'ast> Visit<'ast> for FsAliasCollector<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !has_cfg_test(&item.attrs) {
            self.0.add_use(&item.tree, &mut Vec::new());
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }
}

#[derive(Default)]
struct ProjectMetadataMarker(bool);

impl<'ast> Visit<'ast> for ProjectMetadataMarker {
    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        let value = literal.value().replace('\\', "/");
        if value == "AGENTS.md"
            || value == "CLAUDE.md"
            || value.contains(".flux/")
            || value.ends_with("/.flux")
        {
            self.0 = true;
        }
    }
}

struct ProjectIoVisitor<'a> {
    aliases: &'a FsAliases,
    contexts: Vec<(String, bool)>,
    hits: BTreeSet<RawProjectMetadataIo>,
}

impl ProjectIoVisitor<'_> {
    fn enter_function(&mut self, name: String, block: &syn::Block) {
        let mut marker = ProjectMetadataMarker::default();
        marker.visit_block(block);
        self.contexts.push((name, marker.0));
    }

    fn record_call(&mut self, call: &syn::ExprCall) {
        let Some((function, true)) = self.contexts.last() else {
            return;
        };
        let syn::Expr::Path(path) = call.func.as_ref() else {
            return;
        };
        let segments: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if self.aliases.resolves_call(&segments) {
            self.hits.insert(RawProjectMetadataIo {
                line: start_line(path.path.span()),
                function: function.clone(),
            });
        }
    }
}

impl<'ast> Visit<'ast> for ProjectIoVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.enter_function(item.sig.ident.to_string(), &item.block);
        syn::visit::visit_item_fn(self, item);
        self.contexts.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.enter_function(item.sig.ident.to_string(), &item.block);
        syn::visit::visit_impl_item_fn(self, item);
        self.contexts.pop();
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        self.record_call(call);
        syn::visit::visit_expr_call(self, call);
    }
}

/// Find production raw std/Tokio filesystem calls in functions that name repository-controlled
/// metadata (`AGENTS.md`, `CLAUDE.md`, or `.flux/*`). This conservative dataflow-lite check catches
/// a path built in one statement and read in another, including imported aliases; guarded
/// `System`/`Workspace` calls are not raw filesystem calls and therefore remain valid.
pub fn raw_project_metadata_io(src: &str) -> syn::Result<Vec<RawProjectMetadataIo>> {
    let file = syn::parse_file(src)?;
    let mut aliases = FsAliases::default();
    FsAliasCollector(&mut aliases).visit_file(&file);
    let mut visitor = ProjectIoVisitor {
        aliases: &aliases,
        contexts: Vec::new(),
        hits: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.hits.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::{DependencyKind, Metadata, MetadataCommand};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Collect production Rust sources from both Cargo workspaces. Keeping this traversal shared
    /// makes it impossible for one architecture gate to quietly forget the separately-built
    /// integration plugins.
    fn workspace_source_files(repo_root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for workspace_dir in [repo_root.join("crates"), repo_root.join("plugins")] {
            let Ok(entries) = std::fs::read_dir(workspace_dir) else {
                continue;
            };
            for entry in entries {
                let dir = entry.unwrap().path();
                if dir.is_dir() {
                    collect_rs(&dir.join("src"), &mut files);
                }
            }
        }
        files
    }

    /// Build the layer graph from Cargo's resolved package metadata. Dependency keys are never
    /// consulted: `Dependency::name` is the actual package identity, so `package =` renames cannot
    /// hide an edge. Cargo reports normal/build/dev kind and target predicates independently; the
    /// architecture contract includes normal + build edges on every target and explicitly excludes
    /// dev-only edges.
    fn metadata_layer_graph(metadata: &Metadata) -> Vec<(String, Vec<String>)> {
        let workspace_names: BTreeSet<String> = metadata
            .workspace_packages()
            .into_iter()
            .map(|package| package.name.to_string())
            .collect();

        metadata
            .workspace_packages()
            .into_iter()
            .map(|package| {
                let dependencies = package
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.kind != DependencyKind::Development)
                    .map(|dependency| dependency.name.to_string())
                    .filter(|name| workspace_names.contains(name))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                (package.name.to_string(), dependencies)
            })
            .collect()
    }

    /// Resolve the real Cargo package graph and assert the whole workspace respects layering.
    #[test]
    fn workspace_respects_layering() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let metadata = MetadataCommand::new()
            .manifest_path(repo_root.join("Cargo.toml"))
            .other_options(vec!["--locked".into(), "--offline".into()])
            .exec()
            .expect("resolve root workspace metadata");
        let deps_by_crate = metadata_layer_graph(&metadata);

        // sanity: we actually found the workspace crates
        assert!(
            deps_by_crate.len() > 20,
            "expected to scan the workspace crates"
        );

        let v = violations(&deps_by_crate);
        assert!(
            v.is_empty(),
            "architecture layering violations:\n  {}",
            v.join("\n  ")
        );
    }

    /// Crates a guest plugin build must never pull in. A plugin is a subprocess that speaks NDJSON
    /// over stdio: its contract is the wire format, not flux's internals. `flux-lang` in
    /// particular is the language front-end (parser, CST, analyzer) and drags a ~75-crate subtree
    /// through `flux-plugin → host-kit → every plugin`, so any change to it rebuilds the whole
    /// pack. Keep this list to crates whose presence in a plugin build is a design error, not
    /// merely undesirable (C-141).
    const GUEST_FORBIDDEN: &[&str] = &["codewandler-flux-lang"];

    /// The plugin pack's build graph must stay clear of the host-only crates above. Resolving the
    /// nested workspace is the honest check — a manifest read would miss an edge inherited through
    /// `host-kit`, which is exactly how `flux-lang` reached all 21 plugins.
    ///
    /// `--locked` but NOT `--offline`: this resolves the FULL dependency graph, which needs every
    /// package in the registry cache, and the CI job that runs this test never builds the plugins
    /// workspace — so plugins-only third-party deps (`pulldown-cmark`, via `confluence`) simply are
    /// not there and an offline resolve fails with "no matching package found". It passes on a
    /// developer machine only because their cache happens to be warm, which is exactly the kind of
    /// works-for-me that this gate exists to prevent. `--locked` is what actually matters: it
    /// forbids the resolve from mutating `plugins/Cargo.lock`. Contrast
    /// [`publish_script_covers_a_registry_resolvable_closure`], which passes `--offline` safely
    /// because `--no-deps` means it never resolves a graph at all.
    #[test]
    fn plugin_builds_exclude_host_only_crates() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let metadata = MetadataCommand::new()
            .manifest_path(repo_root.join("plugins/Cargo.toml"))
            .other_options(vec!["--locked".into()])
            .exec()
            .expect("resolve plugins workspace metadata");

        let present: Vec<String> = metadata
            .packages
            .iter()
            .map(|package| package.name.to_string())
            .filter(|name| GUEST_FORBIDDEN.contains(&name.as_str()))
            .collect();

        assert!(
            present.is_empty(),
            "the plugin build graph pulls host-only crate(s): {}\n\
             a plugin's contract is the wire format, not flux's internals — find the edge with \
             `cd plugins && cargo tree -i <crate>` and move the shared type to a serde-only crate",
            present.join(", ")
        );
    }

    /// The roadmap's "Status as of **X.Y.Z (DATE)**" line must name the version this workspace
    /// actually is. `scripts/cut-release.sh` restamps it on every cut (C-147); this test is what
    /// makes that stamp trustworthy rather than something someone remembers to hand-edit — it was
    /// stale through several releases before the cut script owned it.
    #[test]
    fn roadmap_status_line_matches_the_workspace_version() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        let manifest =
            std::fs::read_to_string(repo_root.join("Cargo.toml")).expect("read Cargo.toml");
        let version = manifest
            .lines()
            .find_map(|line| line.trim().strip_prefix("version = \""))
            .and_then(|rest| rest.split('"').next())
            .expect("workspace.package.version in the root Cargo.toml");

        let roadmap = std::fs::read_to_string(repo_root.join("docs/roadmap.md"))
            .expect("read docs/roadmap.md");
        let status = roadmap
            .lines()
            .find(|line| line.starts_with("Status as of "))
            .expect("docs/roadmap.md must carry a `Status as of **X.Y.Z (DATE)**` line");

        assert!(
            status.contains(&format!("**{version} (")),
            "docs/roadmap.md says `{status}` but this workspace is {version} — \
             scripts/cut-release.sh restamps this line on a cut; if you got here another way, \
             update it by hand"
        );
    }

    /// A vanity-prefixed package is part of the crates.io closure. Every production path
    /// dependency in that closure needs a registry version, and the ordered publisher must include
    /// every closure member. Otherwise `cargo publish` fails only after a release tag is pushed.
    ///
    /// The closure has TWO publishers since C-146, because it spans two version lines: the root
    /// workspace ships on the flux line via `scripts/publish-crates-io.sh`, while the nested
    /// `plugins/` workspace sits on the independent 1.x protocol line and ships with the pack via
    /// `.github/workflows/release-plugins.yml`. Both halves are checked — a vanity-prefixed package
    /// that no publisher names is the failure this test exists to catch, and moving one between
    /// lines must not create a gap.
    #[test]
    fn publish_script_covers_a_registry_resolvable_closure() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let manifests = [
            repo_root.join("Cargo.toml"),
            repo_root.join("plugins/Cargo.toml"),
        ];
        let mut closure = BTreeSet::new();
        let mut pack_closure = BTreeSet::new();
        let mut path_only = Vec::new();

        for (index, manifest) in manifests.into_iter().enumerate() {
            let is_pack = index == 1;
            let metadata = MetadataCommand::new()
                .manifest_path(manifest)
                .other_options(vec![
                    "--locked".into(),
                    "--offline".into(),
                    "--no-deps".into(),
                ])
                .exec()
                .expect("resolve workspace metadata");
            for package in metadata.workspace_packages() {
                if !package.name.starts_with("codewandler-") {
                    continue;
                }
                if is_pack {
                    pack_closure.insert(package.name.to_string());
                } else {
                    closure.insert(package.name.to_string());
                }
                for dependency in &package.dependencies {
                    if dependency.kind != DependencyKind::Development
                        && dependency.path.is_some()
                        && dependency.req.to_string() == "*"
                    {
                        path_only.push(format!("{} -> {}", package.name, dependency.name));
                    }
                }
            }
        }

        assert!(
            path_only.is_empty(),
            "publishable packages have path-only production dependencies:\n{}",
            path_only.join("\n")
        );

        let script = std::fs::read_to_string(repo_root.join("scripts/publish-crates-io.sh"))
            .expect("read publish script");
        let array = script
            .split_once("CRATES=(")
            .and_then(|(_, rest)| rest.split_once("\n)"))
            .map(|(array, _)| array)
            .expect("find CRATES array in publish script");
        let scripted = array
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("codewandler-"))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            scripted, closure,
            "scripts/publish-crates-io.sh must list every vanity-prefixed package of the ROOT \
             workspace exactly once (packages in plugins/ ship with the pack — see C-146)"
        );

        // The pack half: every vanity-prefixed package in plugins/ must be named by the workflow
        // that releases the pack, or nothing publishes it at all.
        let workflow =
            std::fs::read_to_string(repo_root.join(".github/workflows/release-plugins.yml"))
                .expect("read release-plugins.yml");
        let unpublished: Vec<&String> = pack_closure
            .iter()
            .filter(|name| !workflow.contains(name.as_str()))
            .collect();
        assert!(
            unpublished.is_empty(),
            ".github/workflows/release-plugins.yml publishes the plugin-pack half of the closure, \
             but never mentions: {unpublished:?}"
        );
    }

    fn fixture_metadata(dependency_section: &str) -> Metadata {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "flux-codegate-metadata-{}-{nonce}",
            std::process::id()
        ));
        let inner = root.join("inner");
        let outer = root.join("outer");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&outer).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"inner\", \"outer\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(
            inner.join("Cargo.toml"),
            format!(
                "[package]\nname = \"flux-runtime\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n{dependency_section}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            outer.join("Cargo.toml"),
            "[package]\nname = \"flux-auth\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(inner.join("src.rs"), "").unwrap();
        std::fs::write(outer.join("src.rs"), "").unwrap();
        // Explicit lib paths keep the fixture tiny and avoid creating source trees.
        for manifest in [inner.join("Cargo.toml"), outer.join("Cargo.toml")] {
            let mut text = std::fs::read_to_string(&manifest).unwrap();
            text.push_str("\n[lib]\npath = \"src.rs\"\n");
            std::fs::write(manifest, text).unwrap();
        }

        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .other_options(vec!["--offline".into()])
            .exec()
            .expect("resolve fixture metadata");
        std::fs::remove_dir_all(root).ok();
        metadata
    }

    #[test]
    fn metadata_gate_sees_renamed_target_and_build_dependencies() {
        for section in [
            "[dependencies]\nidentity = { package = \"flux-auth\", path = \"../outer\" }",
            "[target.'cfg(unix)'.dependencies]\nidentity = { package = \"flux-auth\", path = \"../outer\" }",
            "[build-dependencies]\nidentity = { package = \"flux-auth\", path = \"../outer\" }",
        ] {
            let graph = metadata_layer_graph(&fixture_metadata(section));
            let found = violations(&graph);
            assert_eq!(found.len(), 1, "section `{section}` produced {graph:?}");
            assert!(found[0].contains("flux-auth"), "{found:?}");
        }

        let dev = metadata_layer_graph(&fixture_metadata(
            "[dev-dependencies]\nidentity = { package = \"flux-auth\", path = \"../outer\" }",
        ));
        assert!(
            violations(&dev).is_empty(),
            "dev-only upward dependencies are explicitly outside the production layer contract"
        );
    }

    #[test]
    fn detects_inner_depending_on_outer() {
        // flux-runtime (L2) depending on flux-auth (L5) is the canonical violation the design avoids.
        let bad = vec![("flux-runtime".to_string(), vec!["flux-auth".to_string()])];
        let v = violations(&bad);
        assert_eq!(v.len(), 1);
        assert!(
            v[0].contains("flux-runtime") && v[0].contains("flux-auth"),
            "{v:?}"
        );
    }

    #[test]
    fn same_and_lower_layers_are_allowed() {
        let ok = vec![(
            "flux-orchestrate".to_string(), // L3
            vec![
                "flux-agent".to_string(),   // L3 (same)
                "flux-runtime".to_string(), // L2 (lower)
                "flux-core".to_string(),    // L0 (lower)
            ],
        )];
        assert!(violations(&ok).is_empty());
    }

    #[test]
    fn unclassified_crate_is_flagged() {
        let bad = vec![("flux-mystery".to_string(), vec![])];
        assert_eq!(violations(&bad).len(), 1);
    }

    #[test]
    fn raw_command_scanner_flags_production_but_ignores_tests_and_comments() {
        // A real production reference is reported (line 2).
        let prod = "fn f() {\n    let c = std::process::Command::new(\"x\");\n}\n";
        assert_eq!(raw_process_command_lines(prod), vec![2]);

        // A `#[cfg(test)] mod tests { … }` block is ignored, however the brace is placed.
        let in_test_mod =
            "#[cfg(test)]\nmod tests {\n    use std::process::Command;\n    fn run() { let c = std::process::Command::new(\"x\"); }\n}\n";
        assert!(raw_process_command_lines(in_test_mod).is_empty());
        let same_line =
            "#[cfg(test)] mod tests {\n    fn run() { let c = std::process::Command::new(\"x\"); }\n}\n";
        assert!(raw_process_command_lines(same_line).is_empty());

        // A single-line `#[cfg(test)]` item (a test-only import) is ignored.
        let cfg_use = "#[cfg(test)]\nuse std::process::Command;\nfn f() {}\n";
        assert!(raw_process_command_lines(cfg_use).is_empty());

        // Comments (line and doc) are ignored.
        let commented =
            "/// never use std::process::Command here\n// std::process::Command\nfn f() {}\n";
        assert!(raw_process_command_lines(commented).is_empty());

        // Tokio process creation is a second raw seam and is equally forbidden.
        let tokio = "fn f() {\n    let c = tokio::process::Command::new(\"x\");\n}\n";
        assert_eq!(raw_process_command_lines(tokio), vec![2]);

        // Imported aliases, module aliases, type aliases, and multiline calls all resolve to the
        // same two underlying process APIs.
        let aliases = r#"
use std::process::Command as StdCommand;
use tokio::process as async_process;
type AsyncCommand = async_process::Command;
fn spawn() {
    StdCommand
        ::new("one");
    AsyncCommand::new("two");
}
"#;
        let hits = raw_process_commands(aliases).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].api, ProcessApi::Std);
        assert_eq!(hits[1].api, ProcessApi::Tokio);
        assert!(hits.iter().all(|hit| hit.function == "spawn"));

        // Production code after a test module is still scanned (regions close on brace balance).
        let after =
            "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn prod() {\n    std::process::Command::new(\"y\");\n}\n";
        assert_eq!(raw_process_command_lines(after), vec![6]);
    }

    #[test]
    fn project_metadata_io_scanner_resolves_aliases_and_ignores_guarded_io() {
        let raw = r#"
use std::fs as disk;
fn load(root: &std::path::Path) {
    let path = root.join(".flux/config.toml");
    let _ = disk::read_to_string(path);
}
"#;
        let hits = raw_project_metadata_io(raw).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].function, "load");

        let guarded = r#"
fn load(system: &flux_system::System) {
    let _ = system.read_file("AGENTS.md");
}
"#;
        assert!(raw_project_metadata_io(guarded).unwrap().is_empty());

        let test_only = r#"
#[cfg(test)]
fn fixture() { std::fs::read_to_string(".flux/config.toml"); }
"#;
        assert!(raw_project_metadata_io(test_only).unwrap().is_empty());
    }

    /// Architecture guard: no production (non-test) tool/runtime/plugin path may construct a raw
    /// std or Tokio process command. `flux-system` owns exactly two reviewed construction points:
    /// the canonical std builder and its Tokio conversion. Allowances are single-use, so a second
    /// constructor even inside either function fails.
    #[test]
    fn no_raw_process_command_outside_system() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        // Documented, reviewed single-use exceptions: `(path, containing function, API)`.
        const ALLOW: &[(&str, &str, ProcessApi)] = &[
            (
                "crates/flux-system/src/lib.rs",
                "build_command",
                ProcessApi::Std,
            ),
            (
                "crates/flux-system/src/lib.rs",
                "build_tokio_command",
                ProcessApi::Tokio,
            ),
        ];
        let mut allowance_use = vec![0usize; ALLOW.len()];

        // Root crates include flux-system itself: its two constructors are admitted only by the
        // single-use entries above. The shared traversal also covers every nested plugin `src/`.
        let rs_files = workspace_source_files(repo_root);

        assert!(
            rs_files.len() > 20,
            "expected to scan a representative set of source files, found {}",
            rs_files.len()
        );

        let mut violations = Vec::new();
        for file in &rs_files {
            let rel = file
                .strip_prefix(repo_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).unwrap();
            let hits = raw_process_commands(&src).unwrap_or_else(|error| {
                panic!("parse {} for process gate: {error}", file.display())
            });
            for hit in hits {
                if let Some((index, _)) = ALLOW.iter().enumerate().find(|(_, allowed)| {
                    allowed.0 == rel && allowed.1 == hit.function && allowed.2 == hit.api
                }) {
                    allowance_use[index] += 1;
                    if allowance_use[index] > 1 {
                        violations.push(format!(
                            "{rel}:{}: duplicate use of single-use allowance for {} ({:?})",
                            hit.line, hit.function, hit.api
                        ));
                    }
                } else {
                    violations.push(format!(
                        "{rel}:{}: raw {:?} Command construction in {}",
                        hit.line, hit.api, hit.function
                    ));
                }
            }
        }

        for (index, count) in allowance_use.into_iter().enumerate() {
            if count != 1 {
                violations.push(format!(
                    "reviewed process allowance {:?} was used {count} times (expected exactly once)",
                    ALLOW[index]
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "raw process construction outside the canonical flux-system seam:\n  {}",
            violations.join("\n  ")
        );
    }

    /// Automatic project metadata must go through `System`/`Workspace`; the L0 parsers receive
    /// injected bytes and have no file-wide exemption. A new direct AGENTS/config/role/skill read
    /// in either Cargo workspace therefore fails here.
    #[test]
    fn no_raw_project_metadata_io_outside_guarded_boundary() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        // These long CLI orchestration functions mention `.flux` control paths but their raw read
        // is of an explicit user argument (`fork --edit` / `app run <program>`), not automatic
        // project metadata. Keep the exception function-scoped rather than exempting the CLI file.
        const EXPLICIT_INPUT_READS: &[(&str, &str)] = &[
            ("crates/flux-cli/src/session.rs", "run_fork"),
            ("crates/flux-cli/src/app_cmd.rs", "run_app"),
        ];

        let files = workspace_source_files(repo_root);

        let mut violations = Vec::new();
        for file in files {
            let rel = file
                .strip_prefix(repo_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&file).unwrap();
            for hit in raw_project_metadata_io(&source).unwrap_or_else(|error| {
                panic!(
                    "parse {} for project metadata IO gate: {error}",
                    file.display()
                )
            }) {
                if EXPLICIT_INPUT_READS.contains(&(rel.as_str(), hit.function.as_str())) {
                    continue;
                }
                violations.push(format!("{rel}:{} ({})", hit.line, hit.function));
            }
        }

        assert!(
            violations.is_empty(),
            "raw project metadata IO outside the guarded boundary:\n  {}",
            violations.join("\n  ")
        );
    }

    /// Recursively collect `.rs` files under `dir` (missing dirs are simply skipped).
    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn architecture_source_walk_covers_both_workspaces() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let relative = workspace_source_files(repo_root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(repo_root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<BTreeSet<_>>();

        assert!(relative.contains("crates/flux-system/src/lib.rs"));
        assert!(relative.contains("plugins/host-kit/src/lib.rs"));
    }
}
