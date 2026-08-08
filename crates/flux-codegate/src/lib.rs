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
    local_bindings: Vec<HashMap<String, Option<ProcessApi>>>,
    functions: Vec<String>,
    hits: BTreeSet<RawProcessCommand>,
}

impl ProcessVisitor<'_> {
    fn resolve_callable(&self, path: &syn::Path) -> Option<ProcessApi> {
        let mut segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if let [ident] = segments.as_slice() {
            return self
                .local_bindings
                .iter()
                .rev()
                .find_map(|scope| scope.get(ident))
                .copied()
                .flatten();
        }
        // Any associated constructor on either Command type creates a raw process builder. This
        // covers `new`, `from`, and future constructors without maintaining a spelling blacklist.
        segments.pop();
        self.aliases.resolve_type_segments(&segments)
    }

    fn record_call(&mut self, call: &syn::ExprCall) {
        let Some(function) = transparent_expr_path(&call.func) else {
            return;
        };
        let Some(api) = self.resolve_callable(function) else {
            return;
        };
        self.hits.insert(RawProcessCommand {
            line: start_line(function.span()),
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

fn transparent_expr_path(expr: &syn::Expr) -> Option<&syn::Path> {
    match expr {
        syn::Expr::Path(path) => Some(&path.path),
        syn::Expr::Group(group) => transparent_expr_path(&group.expr),
        syn::Expr::Paren(paren) => transparent_expr_path(&paren.expr),
        syn::Expr::Cast(cast) => transparent_expr_path(&cast.expr),
        _ => None,
    }
}

fn simple_binding_ident(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(ident) if ident.subpat.is_none() => Some(ident.ident.to_string()),
        syn::Pat::Paren(paren) => simple_binding_ident(&paren.pat),
        syn::Pat::Reference(reference) => simple_binding_ident(&reference.pat),
        syn::Pat::Type(typed) => simple_binding_ident(&typed.pat),
        _ => None,
    }
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

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.local_bindings.push(HashMap::new());
        syn::visit::visit_block(self, block);
        self.local_bindings.pop();
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        let Some(ident) = simple_binding_ident(&local.pat) else {
            return;
        };
        let binding = local
            .init
            .as_ref()
            .and_then(|init| transparent_expr_path(&init.expr))
            .and_then(|path| self.resolve_callable(path));
        if let Some(scope) = self.local_bindings.last_mut() {
            scope.insert(ident, binding);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        self.record_call(call);
        syn::visit::visit_expr_call(self, call);
    }
}

/// Resolve production raw-process constructions in one Rust source file through imports, renamed
/// imports, module aliases, type aliases, local callable aliases, and multiline syntax. Test-only
/// items are excluded by their parsed `cfg(test)` attributes; comments and strings are invisible.
pub fn raw_process_commands(src: &str) -> syn::Result<Vec<RawProcessCommand>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ProcessAliases::default();
    AliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = ProcessVisitor {
        aliases: &aliases,
        local_bindings: Vec::new(),
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

/// Direct I/O API families forbidden in model-facing operation implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DirectIoApi {
    Filesystem,
    Process,
    Socket,
    Http,
    Database,
}

/// One production direct-I/O construction resolved through Rust imports and aliases.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectIoCall {
    pub line: usize,
    pub api: DirectIoApi,
    pub function: String,
    /// Canonical call path after resolving imports, renames and local aliases.
    pub path: Vec<String>,
}

#[derive(Default)]
struct ImportAliases {
    paths: HashMap<String, Vec<String>>,
    type_aliases: Vec<(String, Vec<String>)>,
}

impl ImportAliases {
    fn add_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.add_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                if name.ident == "self" {
                    if let Some(local) = prefix.last() {
                        self.paths.insert(local.clone(), prefix.clone());
                    }
                } else {
                    let mut full = prefix.clone();
                    full.push(name.ident.to_string());
                    self.paths.insert(name.ident.to_string(), full);
                }
            }
            syn::UseTree::Rename(rename) => {
                let mut full = prefix.clone();
                if rename.ident != "self" {
                    full.push(rename.ident.to_string());
                }
                self.paths.insert(rename.rename.to_string(), full);
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.add_use(tree, prefix);
                }
            }
            syn::UseTree::Glob(_) => {
                let leaves: &[&str] = match prefix
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["std" | "tokio", "fs"] => &[
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
                        "File",
                        "OpenOptions",
                    ],
                    ["std" | "tokio", "process"] => &["Command"],
                    ["std", "net"] => &["TcpStream", "TcpListener", "UdpSocket"],
                    ["tokio", "net"] => &[
                        "TcpStream",
                        "TcpListener",
                        "UdpSocket",
                        "UnixStream",
                        "UnixListener",
                    ],
                    ["std", "os", "unix", "net"] => &["UnixStream", "UnixListener"],
                    ["reqwest"] => &["Client", "get"],
                    ["rusqlite"] => &["Connection"],
                    // The SDK client roots [`pin_seams`] anchors on — a `use flux_sdk::*;` must not
                    // make a shipped builder chain invisible to the pin census.
                    ["flux_sdk"] => &["Client", "FlowClient"],
                    _ => &[],
                };
                for leaf in leaves {
                    let mut full = prefix.clone();
                    full.push((*leaf).to_string());
                    self.paths.insert((*leaf).to_string(), full);
                }
            }
        }
    }

    fn resolve(&self, segments: &[String]) -> Vec<String> {
        let mut resolved = segments.to_vec();
        for _ in 0..8 {
            let Some(first) = resolved.first().cloned() else {
                break;
            };
            let Some(prefix) = self.paths.get(&first) else {
                break;
            };
            let mut next = prefix.clone();
            next.extend(resolved.into_iter().skip(1));
            resolved = next;
        }
        resolved
    }

    fn resolve_type_aliases(&mut self) {
        loop {
            let mut changed = false;
            for (alias, path) in &self.type_aliases {
                if self.paths.contains_key(alias) {
                    continue;
                }
                let resolved = self.resolve(path);
                if resolved != *path {
                    self.paths.insert(alias.clone(), resolved);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

fn classify_direct_io(segments: &[String]) -> Option<DirectIoApi> {
    let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
    if matches!(parts.as_slice(), ["std" | "tokio", "fs", ..]) {
        return Some(DirectIoApi::Filesystem);
    }
    if matches!(
        parts.as_slice(),
        ["std" | "tokio", "process", "Command", ..]
    ) {
        return Some(DirectIoApi::Process);
    }
    if matches!(
        parts.as_slice(),
        ["std", "net", "TcpStream", "connect", ..]
            | ["tokio", "net", "TcpStream", "connect", ..]
            | ["std", "net", "TcpListener", "bind", ..]
            | ["tokio", "net", "TcpListener", "bind", ..]
            | ["std", "net", "UdpSocket", "bind" | "connect", ..]
            | ["tokio", "net", "UdpSocket", "bind" | "connect", ..]
            | ["std", "os", "unix", "net", "UnixStream", "connect", ..]
            | ["tokio", "net", "UnixStream", "connect", ..]
            | ["std", "os", "unix", "net", "UnixListener", "bind", ..]
            | ["tokio", "net", "UnixListener", "bind", ..]
            | [
                "socket2",
                "Socket",
                "new" | "from_raw_fd" | "from_raw_socket",
                ..
            ]
            | [
                "libc",
                "socket" | "bind" | "listen" | "accept" | "accept4",
                ..
            ]
            | [
                "nix",
                "sys",
                "socket",
                "socket" | "bind" | "listen" | "accept" | "accept4",
                ..
            ]
    ) {
        return Some(DirectIoApi::Socket);
    }
    if matches!(
        parts.as_slice(),
        ["reqwest", "Client", "new" | "builder", ..]
            | ["reqwest", "blocking", "Client", "new" | "builder", ..]
    ) || matches!(
        parts.as_slice(),
        ["reqwest", "get", ..] | ["reqwest", "blocking", "get", ..]
    ) {
        return Some(DirectIoApi::Http);
    }
    if matches!(parts.as_slice(), ["rusqlite", "Connection", method, ..] if method.starts_with("open"))
        || matches!(parts.as_slice(), ["sqlx", _, method, ..] if method.starts_with("connect"))
    {
        return Some(DirectIoApi::Database);
    }
    None
}

#[cfg(test)]
fn is_obvious_native_listener_constructor(segments: &[String]) -> bool {
    let parts = segments.iter().map(String::as_str).collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["std" | "tokio", "net", "TcpListener", "bind", ..]
            | [
                "socket2",
                "Socket",
                "new" | "from_raw_fd" | "from_raw_socket",
                ..
            ]
            | [
                "libc",
                "socket" | "bind" | "listen" | "accept" | "accept4",
                ..
            ]
            | [
                "nix",
                "sys",
                "socket",
                "socket" | "bind" | "listen" | "accept" | "accept4",
                ..
            ]
    )
}

/// Collect a production file's import, module, rename and type aliases into [`ImportAliases`].
/// Shared by every scanner that resolves a path to its canonical spelling — nothing here is
/// I/O-specific — so a renamed import cannot be visible to one gate and invisible to the next.
struct ImportAliasCollector<'a>(&'a mut ImportAliases);

impl<'ast> Visit<'ast> for ImportAliasCollector<'_> {
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
            self.0.type_aliases.push((
                item.ident.to_string(),
                path.path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect(),
            ));
        }
        syn::visit::visit_item_type(self, item);
    }
}

struct DirectIoVisitor<'a> {
    aliases: &'a ImportAliases,
    /// Lexical bindings for local values. `None` records a non-path binding that shadows an import
    /// or outer alias; `Some(path)` is a callable path already resolved at the declaration site.
    local_bindings: Vec<HashMap<String, Option<Vec<String>>>>,
    functions: Vec<String>,
    hits: BTreeSet<DirectIoCall>,
}

impl DirectIoVisitor<'_> {
    fn resolve(&self, segments: &[String]) -> Vec<String> {
        let Some(first) = segments.first() else {
            return Vec::new();
        };
        for scope in self.local_bindings.iter().rev() {
            if let Some(binding) = scope.get(first) {
                let Some(prefix) = binding else {
                    return segments.to_vec();
                };
                let mut resolved = prefix.clone();
                resolved.extend(segments.iter().skip(1).cloned());
                return resolved;
            }
        }
        self.aliases.resolve(segments)
    }

    fn record_call(&mut self, call: &syn::ExprCall) {
        let Some(path) = transparent_expr_path(&call.func) else {
            return;
        };
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let resolved = self.resolve(&segments);
        if let Some(api) = classify_direct_io(&resolved) {
            self.hits.insert(DirectIoCall {
                line: start_line(path.span()),
                api,
                path: resolved,
                function: self
                    .functions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "<module>".into()),
            });
        }
    }

    /// Scan the otherwise-opaque body of a macro for an obvious path call. This deliberately does
    /// not pretend to expand macros; it closes a simple macro body containing a listener bind while
    /// preserving the AST scanner's honest boundary around generated code from
    /// dependencies or build scripts.
    fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        let trees = tokens.into_iter().collect::<Vec<_>>();
        let mut index = 0;
        while index < trees.len() {
            match &trees[index] {
                proc_macro2::TokenTree::Ident(first) => {
                    let mut segments = vec![first.to_string()];
                    let mut cursor = index + 1;
                    while cursor + 2 < trees.len()
                        && matches!(&trees[cursor], proc_macro2::TokenTree::Punct(p) if p.as_char() == ':')
                        && matches!(&trees[cursor + 1], proc_macro2::TokenTree::Punct(p) if p.as_char() == ':')
                    {
                        let proc_macro2::TokenTree::Ident(next) = &trees[cursor + 2] else {
                            break;
                        };
                        segments.push(next.to_string());
                        cursor += 3;
                    }

                    if let Some(proc_macro2::TokenTree::Group(arguments)) = trees.get(cursor) {
                        if arguments.delimiter() == proc_macro2::Delimiter::Parenthesis {
                            let resolved = self.resolve(&segments);
                            if let Some(api) = classify_direct_io(&resolved) {
                                self.hits.insert(DirectIoCall {
                                    line: start_line(first.span()),
                                    api,
                                    path: resolved,
                                    function: self
                                        .functions
                                        .last()
                                        .cloned()
                                        .unwrap_or_else(|| "<module>".into()),
                                });
                            }
                            self.scan_macro_tokens(arguments.stream());
                            index = cursor + 1;
                            continue;
                        }
                    }
                    index = cursor.max(index + 1);
                }
                proc_macro2::TokenTree::Group(group) => {
                    self.scan_macro_tokens(group.stream());
                    index += 1;
                }
                _ => index += 1,
            }
        }
    }
}

impl<'ast> Visit<'ast> for DirectIoVisitor<'_> {
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

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_macro(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.local_bindings.push(HashMap::new());
        syn::visit::visit_block(self, block);
        self.local_bindings.pop();
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // The initializer executes before this binding exists. Visit it first so a direct I/O call
        // there is still recorded and an outer binding with the same name remains visible to it.
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }

        let Some(ident) = simple_binding_ident(&local.pat) else {
            return;
        };
        let binding = local
            .init
            .as_ref()
            .and_then(|init| transparent_expr_path(&init.expr))
            .map(|path| {
                let segments = path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>();
                self.resolve(&segments)
            });
        if let Some(scope) = self.local_bindings.last_mut() {
            // Recording non-path bindings is important: they shadow identically named imported or
            // outer I/O aliases and prevent the conservative gate from inventing a false call.
            scope.insert(ident, binding);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        self.record_call(call);
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.scan_macro_tokens(mac.tokens.clone());
        syn::visit::visit_macro(self, mac);
    }
}

/// Resolve direct filesystem, process, socket, HTTP-client, and database opens from parsed Rust.
/// Imports, renamed imports, module aliases, type aliases, local callable aliases, and multiline
/// calls are followed; test-only items, comments, and strings are excluded structurally by `syn`.
pub fn raw_direct_io_calls(src: &str) -> syn::Result<Vec<DirectIoCall>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ImportAliases::default();
    ImportAliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = DirectIoVisitor {
        aliases: &aliases,
        local_bindings: Vec::new(),
        functions: Vec::new(),
        hits: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.hits.into_iter().collect())
}

#[cfg(test)]
struct NativeListenerTypeVisitor<'a> {
    aliases: &'a ImportAliases,
    lines: BTreeSet<usize>,
}

#[cfg(test)]
impl<'ast> Visit<'ast> for NativeListenerTypeVisitor<'_> {
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

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        let segments = ty
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let resolved = self.aliases.resolve(&segments);
        let parts = resolved.iter().map(String::as_str).collect::<Vec<_>>();
        if matches!(parts.as_slice(), ["std" | "tokio", "net", "TcpListener"]) {
            self.lines.insert(start_line(ty.path.span()));
        }
        syn::visit::visit_type_path(self, ty);
    }
}

/// Production references to a native TCP-listener type. A served API accepting this type has
/// already crossed the guarded constructor boundary, even when its caller performed the bind.
#[cfg(test)]
fn native_tcp_listener_type_lines(src: &str) -> syn::Result<Vec<usize>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ImportAliases::default();
    ImportAliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = NativeListenerTypeVisitor {
        aliases: &aliases,
        lines: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.lines.into_iter().collect())
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

// ---------------------------------------------------------------------------
// Guarded-IO port backends (C-269)
// ---------------------------------------------------------------------------

/// The `flux_system::port` traits whose implementations *are* a guarded IO backend. Implementing one
/// is a claim to enforce the process/filesystem guarantees `System` enforces, so the set of
/// implementors has to stay as enumerable as the set of raw `Command` constructions.
const GUARDED_PORT_TRAITS: &[&str] = &[
    "GuardedEnv",
    "GuardedProcess",
    "GuardedHostFiles",
    "GuardedWorkspaceFiles",
    "GuardedNetwork",
    "GuardedMetrics",
    "GuardedHttp",
];

/// The contracts a live datasource / work board backend declares itself by (C-716).
///
/// Scanned with the same machinery as [`GUARDED_PORT_TRAITS`] and for the same reason: the trait is
/// where a type *claims* to be a backend, and a claim nobody enumerates is a claim nobody reviews.
const LIVE_BACKEND_TRAITS: &[&str] = &["LiveDatasource", "WorkBoard"];

/// A production `impl <port trait> for <type>` — a type declaring itself a guarded IO backend.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GuardedPortImpl {
    pub line: usize,
    /// The **canonical** contract name (`GuardedProcess`, `LiveDatasource`), whatever local
    /// spelling reached it. Allowances match on this, so a rename cannot mint a fresh unreviewed
    /// identity.
    pub port: String,
    /// The implementing type's final path segment (`System`), or `<generic>` for a blanket impl.
    pub backend: String,
    /// The local name actually written, when it differs from [`Self::port`] — i.e. the impl came
    /// through a renamed import. Diagnostics only; it makes an aliased violation readable.
    pub spelled_as: Option<String>,
}

/// The canonical port trait a path names, matched on its **final segment**: reached as
/// `GuardedProcess`, `port::GuardedProcess`, `flux_system::port::GuardedProcess`, or through a glob
/// import, it is the same trait, and a gate that demanded the full path would miss the short spellings
/// that are actually idiomatic. Over-reporting an unrelated same-named trait is the safe direction for
/// a security gate — that costs a reviewed allowance, whereas under-reporting costs the invariant.
fn direct_port_trait(segments: &[String], table: &'static [&'static str]) -> Option<&'static str> {
    let last = segments.last()?;
    table.iter().find(|port| *port == last).copied()
}

/// Local names that reach a guarded-IO port trait, so a renamed import cannot hide a backend.
///
/// This mirrors [`ProcessAliases`], which already resolves `use std::process::Command as Exec` for
/// `no_raw_process_command_outside_system` — without the same treatment here the newer gate would be
/// weaker than its sibling against the identical evasion.
struct PortAliases {
    /// The canonical contract names this scan is about — [`GUARDED_PORT_TRAITS`] or
    /// [`LIVE_BACKEND_TRAITS`]. One table per scan so the two censuses cannot report each other's
    /// hits under each other's allowances.
    table: &'static [&'static str],
    /// Local trait name → canonical port trait. Seeded with the identity mapping for every port
    /// trait, so unaliased spellings resolve through the same table as renamed ones.
    traits: HashMap<String, &'static str>,
    /// `use <path> as <local>` pairs whose target was not itself a port trait, resolved to a fixed
    /// point once the whole file is collected — so a rename *chain*
    /// (`use …GuardedProcess as A; use A as B;`) still lands on the canonical name, and so a
    /// `use` that appears textually before the one it depends on is not order-sensitive.
    renames: Vec<(Vec<String>, String)>,
}

impl PortAliases {
    fn new(table: &'static [&'static str]) -> Self {
        let mut aliases = Self {
            table,
            traits: HashMap::new(),
            renames: Vec::new(),
        };
        for port in table {
            aliases.traits.insert((*port).to_string(), *port);
        }
        aliases
    }

    /// The canonical port trait an `impl … for` trait path resolves to.
    fn resolve_path(&self, path: &syn::Path) -> Option<&'static str> {
        let last = path.segments.last()?.ident.to_string();
        self.traits.get(&last).copied()
    }

    fn add_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.add_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                if let Some(port) = direct_port_trait(prefix, self.table) {
                    self.traits.insert(name.ident.to_string(), port);
                }
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                match direct_port_trait(prefix, self.table) {
                    Some(port) => {
                        self.traits.insert(rename.rename.to_string(), port);
                    }
                    // Not (yet) known to be a port trait — it may be a local re-export of one, so
                    // defer rather than drop.
                    None => self
                        .renames
                        .push((prefix.clone(), rename.rename.to_string())),
                }
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.add_use(item, prefix);
                }
            }
            // A glob re-exports the canonical names unchanged, which the identity seeding covers.
            syn::UseTree::Glob(_) => {}
        }
    }

    fn resolve_renames(&mut self) {
        let renames = std::mem::take(&mut self.renames);
        loop {
            let mut changed = false;
            for (segments, local) in &renames {
                if self.traits.contains_key(local) {
                    continue;
                }
                if let Some(port) = segments
                    .last()
                    .and_then(|last| self.traits.get(last).copied())
                {
                    self.traits.insert(local.clone(), port);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

struct PortAliasCollector<'a>(&'a mut PortAliases);

impl<'ast> Visit<'ast> for PortAliasCollector<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !has_cfg_test(&item.attrs) {
            self.0.add_use(&item.tree, &mut Vec::new());
        }
    }
}

struct PortImplVisitor<'a> {
    aliases: &'a PortAliases,
    hits: BTreeSet<GuardedPortImpl>,
}

/// The final path segment of a type, for the shapes an `impl … for T` self-type can take. Anything
/// that is not a plain path (a reference, tuple, generic parameter) is reported as `<generic>` so a
/// blanket impl is still visible to the gate rather than silently dropped.
fn self_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| "<generic>".to_string()),
        _ => "<generic>".to_string(),
    }
}

impl<'ast> Visit<'ast> for PortImplVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        if let Some((path, _)) = &item.trait_ {
            if let Some(port) = self.aliases.resolve_path(path) {
                let written = path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    .unwrap_or_default();
                self.hits.insert(GuardedPortImpl {
                    line: start_line(item.impl_token.span),
                    port: port.to_string(),
                    backend: self_type_name(&item.self_ty),
                    spelled_as: (written != port).then_some(written),
                });
            }
        }
        syn::visit::visit_item_impl(self, item);
    }
}

/// Every production implementation of a guarded-IO port trait in `src`, `#[cfg(test)]` ones excluded.
///
/// Renamed imports are resolved back to the canonical trait, so `use …GuardedProcess as Exec;
/// impl Exec for Rogue {}` reports as `GuardedProcess`. Only `#[cfg(test)]` is skipped — a
/// `#[cfg(feature = "…")]` impl is production code and is reported.
pub fn guarded_port_impls(src: &str) -> syn::Result<Vec<GuardedPortImpl>> {
    contract_impls(src, GUARDED_PORT_TRAITS)
}

/// Every production implementation of a live datasource / work board contract in `src` (C-716),
/// `#[cfg(test)]` ones excluded.
///
/// The same scan as [`guarded_port_impls`] over a different contract table, so an aliased or
/// fully-qualified `impl` is seen here exactly as it is there. [`GuardedPortImpl::port`] carries the
/// canonical contract name (`LiveDatasource`, `WorkBoard`).
pub fn live_backend_impls(src: &str) -> syn::Result<Vec<GuardedPortImpl>> {
    contract_impls(src, LIVE_BACKEND_TRAITS)
}

fn contract_impls(src: &str, table: &'static [&'static str]) -> syn::Result<Vec<GuardedPortImpl>> {
    let file = syn::parse_file(src)?;
    let mut aliases = PortAliases::new(table);
    PortAliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_renames();
    let mut visitor = PortImplVisitor {
        aliases: &aliases,
        hits: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.hits.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Sandbox posture of test spawns (C-266)
// ---------------------------------------------------------------------------

/// argv tokens that select an auto-approving or serving surface. Mirrors the flag arms of
/// `unattended_sandbox_surface` (`crates/flux-cli/src/dispatch.rs`); [`unattended_surface_arms`]
/// keeps the *flagless* half of that function from drifting away from the list below.
pub const UNATTENDED_ARGV_FLAGS: &[&str] = &["--yes", "-y", "--serve"];

/// Subcommands that are unattended with **no flag at all** — the trap `--yes`-keyed matching misses.
/// Kept honest against `dispatch.rs` by the drift check on [`unattended_surface_arms`].
///
/// An entry may name a **subcommand path** (`plugin call`), matched as a contiguous run of argv
/// tokens. C-410 needs that: `flux plugin call` is unattended, while the rest of `flux plugin …` is
/// operator-driven management that is not — a bare `plugin` entry would demand a posture
/// declaration from every `plugin ls`/`status`/`refresh` spawn and buy nothing.
pub const FLAGLESS_UNATTENDED_SUBCOMMANDS: &[&str] = &["review", "plugin call", "system serve"];

/// The file extension that turns a `run` into **program mode** — a channel daemon, pinned to the
/// fail-closed profile with or without `--yes` (C-410).
///
/// This is the one unattended shape keyed on neither a flag nor a subcommand *name* but on an
/// argument, so neither list above can express it: `flux run p.flux` is a daemon while
/// `flux run hello` is a turn, and both are spelled `run`. [`program_mode_argv`] matches it.
pub const UNATTENDED_PROGRAM_SUFFIX: &str = ".flux";

/// Environment variables whose appearance in a spawn's builder chain *is* a posture declaration:
/// each one pins the resolved posture (or forces backend discovery) instead of inheriting whatever
/// the host happens to have installed.
pub const SANDBOX_POSTURE_ENV: &[&str] = &[
    "FLUX_SANDBOX",
    "FLUX_SANDBOXED",
    "FLUX_BWRAP_BIN",
    "FLUX_SANDBOX_EXEC_BIN",
];

/// Why one test spawn of the `flux` binary owes an explicit sandbox posture.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AmbientSandboxKind {
    /// The literal argv already names an auto-approving / serving surface; the token is carried so
    /// the failure message can point at it.
    Unattended(String),
    /// The argv is caller-supplied in bulk (`.args(expr)`), so any call site can turn this spawn
    /// into an unattended one without touching the spawn itself.
    ForwardedArgv,
}

/// One spawn of the `flux` binary from test code that never states which sandbox posture it needs,
/// and therefore silently inherits the host's — green on a developer machine with `bwrap`, red on a
/// runner without it (C-266).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AmbientSandboxSpawn {
    pub line: usize,
    /// Nearest containing function, or `<module>` for a module-level initializer.
    pub function: String,
    pub kind: AmbientSandboxKind,
}

/// Whether `token` names an auto-approving or serving surface on its own. `--serve=<addr>` is the
/// attached-value spelling of `--serve` and counts the same. Multi-word
/// [`FLAGLESS_UNATTENDED_SUBCOMMANDS`] entries are matched by [`unattended_argv`], not here.
fn unattended_argv_token(token: &str) -> bool {
    UNATTENDED_ARGV_FLAGS.contains(&token)
        || token.starts_with("--serve=")
        || FLAGLESS_UNATTENDED_SUBCOMMANDS
            .iter()
            .any(|entry| !entry.contains(' ') && *entry == token)
}

/// Whether this literal argv runs a `<program.flux>` — the C-410 channel daemon, pinned with or
/// without `--yes`, under either of its spellings (`flux run p.flux`, `flux app run p.flux`).
///
/// Keyed on a `run` followed by a `[UNATTENDED_PROGRAM_SUFFIX]` argument, which is as close to
/// `run_targets_a_program` (flux-cli's dispatch.rs) as a scan of argv literals can get. `--entry`
/// selects a named flow inside the file and routes away from the daemon, so it disqualifies.
fn program_mode_argv(argv: &[String]) -> Option<String> {
    let run_at = argv.iter().position(|token| token == "run")?;
    let rest = &argv[run_at + 1..];
    if rest.iter().any(|token| token == "--entry") {
        return None;
    }
    rest.iter()
        .find(|token| token.ends_with(UNATTENDED_PROGRAM_SUFFIX))
        .map(|program| format!("run {program}"))
}

/// The unattended surface this literal argv selects, if any — the token, subcommand path, or
/// program argument to name in the finding. A single token wins over the wider shapes so the
/// message points at the sharpest evidence.
fn unattended_argv(argv: &[String]) -> Option<String> {
    if let Some(token) = argv.iter().find(|token| unattended_argv_token(token)) {
        return Some(token.clone());
    }
    FLAGLESS_UNATTENDED_SUBCOMMANDS
        .iter()
        .filter(|entry| entry.contains(' '))
        .find(|entry| {
            let words: Vec<&str> = entry.split(' ').collect();
            argv.windows(words.len())
                .any(|window| window.iter().zip(&words).all(|(got, want)| got == want))
        })
        .map(|entry| (*entry).to_string())
        .or_else(|| program_mode_argv(argv))
}

/// Everything one builder chain said about itself. Accumulated per Command *binding* so a chain
/// split across statements (`let mut cmd = Command::new(..); cmd.args(..); cmd.env(..)`) is judged
/// as the single spawn it is.
#[derive(Default)]
struct SpawnFacts {
    line: usize,
    function: String,
    literal_argv: Vec<String>,
    forwards_argv: bool,
    declares_posture: bool,
}

impl SpawnFacts {
    /// The finding this spawn owes, if any. A declared posture settles it; otherwise a literal
    /// unattended token is reported in preference to the weaker "could become one" finding.
    fn finding(&self) -> Option<AmbientSandboxKind> {
        // `--no-sandbox` states the posture in argv rather than in the environment; it is the CLI's
        // own kill switch and wins outright (`apply_sandbox_env`, flux-cli's dispatch.rs).
        if self.declares_posture
            || self
                .literal_argv
                .iter()
                .any(|token| token == "--no-sandbox")
        {
            return None;
        }
        if let Some(surface) = unattended_argv(&self.literal_argv) {
            return Some(AmbientSandboxKind::Unattended(surface));
        }
        self.forwards_argv
            .then_some(AmbientSandboxKind::ForwardedArgv)
    }
}

/// The string literal an expression *is*, peeling references and `.to_string()`-style wrappers.
fn literal_str(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => Some(s.value()),
            _ => None,
        },
        syn::Expr::Reference(reference) => literal_str(&reference.expr),
        syn::Expr::Paren(paren) => literal_str(&paren.expr),
        syn::Expr::Group(group) => literal_str(&group.expr),
        _ => None,
    }
}

/// The elements of an array/slice/`vec![]` literal, or `None` when the expression is opaque.
fn array_elements(expr: &syn::Expr) -> Option<Vec<&syn::Expr>> {
    match expr {
        syn::Expr::Array(array) => Some(array.elems.iter().collect()),
        syn::Expr::Reference(reference) => array_elements(&reference.expr),
        syn::Expr::Paren(paren) => array_elements(&paren.expr),
        syn::Expr::Group(group) => array_elements(&group.expr),
        _ => None,
    }
}

struct SandboxSpawnVisitor<'a> {
    aliases: &'a ProcessAliases,
    /// Locals bound to `env!("CARGO_BIN_EXE_flux")`, so an indirected program path still resolves.
    flux_bin_idents: BTreeSet<String>,
    /// Local ident → index into `facts` for a binding holding a `flux` Command builder.
    command_bindings: HashMap<String, usize>,
    functions: Vec<String>,
    facts: Vec<SpawnFacts>,
}

impl SandboxSpawnVisitor<'_> {
    /// Does this expression evaluate to the `flux` binary's path? Keyed on the exact
    /// `CARGO_BIN_EXE_flux` env key — the sibling `CARGO_BIN_EXE_flux_sdk_plugin_fixture` is a
    /// different binary and must not match.
    fn is_flux_bin(&self, expr: &syn::Expr) -> bool {
        match expr {
            syn::Expr::Macro(mac) => {
                mac.mac.path.is_ident("env")
                    && mac
                        .mac
                        .parse_body::<syn::LitStr>()
                        .is_ok_and(|lit| lit.value() == "CARGO_BIN_EXE_flux")
            }
            syn::Expr::Reference(reference) => self.is_flux_bin(&reference.expr),
            syn::Expr::Paren(paren) => self.is_flux_bin(&paren.expr),
            syn::Expr::Group(group) => self.is_flux_bin(&group.expr),
            // `env!("…").to_string()` / `.into()` and friends stay the same program.
            syn::Expr::MethodCall(call) => self.is_flux_bin(&call.receiver),
            syn::Expr::Path(path) => path
                .path
                .get_ident()
                .is_some_and(|ident| self.flux_bin_idents.contains(&ident.to_string())),
            _ => false,
        }
    }

    /// A `Command::new(<flux binary>)`-style construction, as a fresh facts record.
    fn new_flux_command(&mut self, expr: &syn::Expr) -> Option<usize> {
        let syn::Expr::Call(call) = expr else {
            return None;
        };
        let path = transparent_expr_path(&call.func)?;
        let mut segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        segments.pop()?;
        self.aliases.resolve_type_segments(&segments)?;
        if !call.args.iter().any(|arg| self.is_flux_bin(arg)) {
            return None;
        }
        self.facts.push(SpawnFacts {
            line: start_line(path.span()),
            function: self
                .functions
                .last()
                .cloned()
                .unwrap_or_else(|| "<module>".into()),
            ..SpawnFacts::default()
        });
        Some(self.facts.len() - 1)
    }

    /// The facts record a chain root belongs to: a fresh one for a constructor, the existing one for
    /// a local already bound to a `flux` Command.
    fn root_record(&mut self, root: &syn::Expr) -> Option<usize> {
        if let Some(index) = self.new_flux_command(root) {
            return Some(index);
        }
        let syn::Expr::Path(path) = root else {
            return None;
        };
        let ident = path.path.get_ident()?.to_string();
        self.command_bindings.get(&ident).copied()
    }

    /// Fold one builder call into a spawn's facts.
    fn absorb(&mut self, index: usize, call: &syn::ExprMethodCall) {
        let method = call.method.to_string();
        let mut args = call.args.iter();
        match method.as_str() {
            // A single positional argument has a fixed shape at the call site: a non-literal one is
            // a *value* (a path, a session id), never a hidden flag, so it is not "forwarded argv".
            "arg" => {
                if let Some(literal) = args.next().and_then(literal_str) {
                    self.facts[index].literal_argv.push(literal);
                }
            }
            // Plural: an array literal is still auditable element by element; anything else is an
            // opaque, unbounded argv this spawn cannot vouch for.
            "args" => match args.next() {
                Some(expr) => match array_elements(expr) {
                    Some(elements) => {
                        for literal in elements.into_iter().filter_map(literal_str) {
                            self.facts[index].literal_argv.push(literal);
                        }
                    }
                    None => self.facts[index].forwards_argv = true,
                },
                None => self.facts[index].forwards_argv = true,
            },
            "env" | "env_remove" => {
                if let Some(key) = args.next().and_then(literal_str) {
                    if SANDBOX_POSTURE_ENV.contains(&key.as_str()) {
                        self.facts[index].declares_posture = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Walk a method-call chain from the outside in, folding every call into the facts of whichever
    /// spawn the chain's root names. Returns that spawn, so a `let` can bind its ident to it.
    fn chain(&mut self, expr: &syn::Expr) -> Option<usize> {
        let mut calls = Vec::new();
        let mut cursor = expr;
        let root = loop {
            match cursor {
                syn::Expr::MethodCall(call) => {
                    calls.push(call);
                    cursor = &call.receiver;
                }
                syn::Expr::Reference(reference) => cursor = &reference.expr,
                syn::Expr::Paren(paren) => cursor = &paren.expr,
                syn::Expr::Group(group) => cursor = &group.expr,
                syn::Expr::Try(inner) => cursor = &inner.expr,
                other => break other,
            }
        };
        let index = self.root_record(root)?;
        for call in &calls {
            self.absorb(index, call);
        }
        // The chain itself is consumed here, so recurse only into the arguments — a nested spawn
        // inside one of them must still be seen.
        for call in calls {
            for arg in &call.args {
                self.visit_expr(arg);
            }
        }
        if let syn::Expr::Call(call) = root {
            for arg in &call.args {
                self.visit_expr(arg);
            }
        }
        Some(index)
    }
}

impl<'ast> Visit<'ast> for SandboxSpawnVisitor<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.functions.push(item.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        let Some(init) = &local.init else {
            return;
        };
        let ident = simple_binding_ident(&local.pat);
        if let Some(ident) = &ident {
            if self.is_flux_bin(&init.expr) {
                self.flux_bin_idents.insert(ident.clone());
                return;
            }
        }
        let record = self.chain(&init.expr);
        if record.is_none() {
            self.visit_expr(&init.expr);
        }
        // Bind unconditionally when the root resolved: over-binding a terminal call's result
        // (`let out = cmd.output()`) is harmless — nothing later calls a builder method on it.
        if let (Some(ident), Some(index)) = (ident, record) {
            self.command_bindings.insert(ident, index);
        }
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if matches!(expr, syn::Expr::MethodCall(_) | syn::Expr::Call(_))
            && self.chain(expr).is_some()
        {
            return;
        }
        syn::visit::visit_expr(self, expr);
    }
}

/// Resolve every spawn of the `flux` binary in one **test** source that inherits its sandbox posture
/// from the host instead of declaring it. Test code is the point: `cfg(test)` items are deliberately
/// *not* skipped here, unlike the production scanners above.
pub fn ambient_sandbox_spawns(src: &str) -> syn::Result<Vec<AmbientSandboxSpawn>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ProcessAliases::default();
    AliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = SandboxSpawnVisitor {
        aliases: &aliases,
        flux_bin_idents: BTreeSet::new(),
        command_bindings: HashMap::new(),
        functions: Vec::new(),
        facts: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor
        .facts
        .iter()
        .filter_map(|facts| {
            facts.finding().map(|kind| AmbientSandboxSpawn {
                line: facts.line,
                function: facts.function.clone(),
                kind,
            })
        })
        .collect())
}

/// One `match` arm of `flux-cli`'s `unattended_sandbox_surface`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnattendedArm {
    /// The `Commands::<Variant>` this arm matches, lowercased into its CLI subcommand spelling —
    /// extended with the nested `<X>Action::<Variant>` when the arm narrows to one
    /// (`Commands::Plugin { action: Some(PluginAction::Call { .. }) }` ⇒ `plugin call`), so a
    /// classification that covers one subcommand of a group is not read as covering the group.
    pub subcommand: String,
    /// Whether the arm is selected by a flag (`yes`, `serve`, `"--run"`) rather than by the
    /// subcommand alone. A flagless arm is invisible to argv-flag matching.
    pub keyed_on_flag: bool,
    /// Whether the arm is selected by a `<program.flux>` **argument** (`program: Some(_)`, or the
    /// `run_targets_a_program` guard) rather than by a flag or the subcommand name. Neither
    /// [`UNATTENDED_ARGV_FLAGS`] nor [`FLAGLESS_UNATTENDED_SUBCOMMANDS`] can express that shape —
    /// `flux run p.flux` is a daemon and `flux run hello` is a turn, and both are spelled `run` —
    /// so [`program_mode_argv`] is what recognises it in a spawn's argv.
    pub keyed_on_program_arg: bool,
}

/// Collect the surfaces `flux-cli`'s `unattended_sandbox_surface` classifies as unattended, so a
/// newly-added *flagless* one (the `review` shape) cannot silently escape
/// [`FLAGLESS_UNATTENDED_SUBCOMMANDS`] — the argv-flag list needs no such help.
pub fn unattended_surface_arms(src: &str) -> syn::Result<Vec<UnattendedArm>> {
    #[derive(Default)]
    struct FlagWords {
        found: bool,
    }
    impl<'ast> Visit<'ast> for FlagWords {
        fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
            let ident = ident.to_string();
            if ident == "yes" || ident == "serve" {
                self.found = true;
            }
        }
        fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
            // Any dash-prefixed literal in the pattern or guard means the arm is selected by a flag
            // that argv matching can see (`"--run"`, `"--yes"`), not by the subcommand alone.
            if literal.value().starts_with('-') {
                self.found = true;
            }
        }
    }

    /// The program-mode tell: a `program` binding in the pattern, or the shared routing predicate
    /// in the guard.
    #[derive(Default)]
    struct ProgramWords {
        found: bool,
    }
    impl<'ast> Visit<'ast> for ProgramWords {
        fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
            if ident == "program" || ident == "run_targets_a_program" {
                self.found = true;
            }
        }
    }

    struct Arms {
        arms: Vec<UnattendedArm>,
    }
    impl<'ast> Visit<'ast> for Arms {
        fn visit_arm(&mut self, arm: &'ast syn::Arm) {
            // Only arms that actually classify something as unattended matter; `_ => None` does not.
            let mut classifies = Classifies::default();
            classifies.visit_expr(&arm.body);
            if !classifies.found {
                return;
            }
            // syn models `pat if guard` as `Pat::Guard`, so one walk of the arm's pattern covers the
            // guard expression too.
            let mut variant = Variant::default();
            variant.visit_pat(&arm.pat);
            let mut flags = FlagWords::default();
            flags.visit_pat(&arm.pat);
            let mut program = ProgramWords::default();
            program.visit_pat(&arm.pat);
            if let Some(subcommand) = variant.name {
                let mut subcommand = subcommand.to_lowercase();
                if let Some(action) = variant.action {
                    subcommand.push(' ');
                    subcommand.push_str(&action.to_lowercase());
                }
                self.arms.push(UnattendedArm {
                    subcommand,
                    keyed_on_flag: flags.found,
                    keyed_on_program_arg: program.found,
                });
            }
        }
    }

    #[derive(Default)]
    struct Classifies {
        found: bool,
    }
    impl<'ast> Visit<'ast> for Classifies {
        fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
            if ident == "Some" {
                self.found = true;
            }
        }
    }

    /// The `Commands::<Variant>` name inside a pattern, plus the nested `<X>Action::<Variant>` the
    /// arm narrows to (`PluginAction::Call`, `AppAction::Run`) when there is one.
    #[derive(Default)]
    struct Variant {
        name: Option<String>,
        action: Option<String>,
    }
    impl Variant {
        fn take(&mut self, path: &syn::Path) {
            let segments: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if let [enum_name, variant] = segments.as_slice() {
                if enum_name == "Commands" && self.name.is_none() {
                    self.name = Some(variant.clone());
                } else if enum_name.ends_with("Action") && self.action.is_none() {
                    self.action = Some(variant.clone());
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Variant {
        fn visit_pat_struct(&mut self, pat: &'ast syn::PatStruct) {
            self.take(&pat.path);
            syn::visit::visit_pat_struct(self, pat);
        }
        fn visit_pat_tuple_struct(&mut self, pat: &'ast syn::PatTupleStruct) {
            self.take(&pat.path);
            syn::visit::visit_pat_tuple_struct(self, pat);
        }
        // A bare path pattern (`Commands::Review`) is modelled as an expression path in syn.
        fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
            self.take(&path.path);
            syn::visit::visit_expr_path(self, path);
        }
    }

    let file = syn::parse_file(src)?;
    let mut arms = Arms { arms: Vec::new() };
    for item in &file.items {
        if let syn::Item::Fn(function) = item {
            if function.sig.ident == "unattended_sandbox_surface" {
                arms.visit_block(&function.block);
            }
        }
    }
    arms.arms.sort();
    arms.arms.dedup();
    Ok(arms.arms)
}

/// What `flux-cli`'s `unattended_sandbox_surface` says about `enum Commands` as a whole (C-410).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnattendedCoverage {
    /// Every `Commands` variant, in declaration order.
    pub variants: Vec<String>,
    /// The variants the classifier names in at least one arm — pinned or exempt, either counts.
    pub classified: BTreeSet<String>,
    /// Catch-all arms (`_ => …`, or a bare binding) found in the classifier's `match`. Any is a
    /// finding: a wildcard is what lets a *new* subcommand inherit a classification nobody chose.
    pub catch_all_arms: usize,
}

impl UnattendedCoverage {
    /// The variants no arm names, in declaration order.
    pub fn unclassified(&self) -> Vec<String> {
        self.variants
            .iter()
            .filter(|variant| !self.classified.contains(*variant))
            .cloned()
            .collect()
    }
}

/// Read `enum Commands` (`args_src`) against `unattended_sandbox_surface` (`dispatch_src`) and
/// report what the classifier covers.
///
/// The defect C-410 removed was a hand-maintained enumeration drifting from the enum it enumerates:
/// `Commands::Plugin` simply had no arm, so `flux plugin call` fell through `_ => None` and ran
/// headless at the `Off` sandbox default. Rust's exhaustiveness check now catches a *new* variant —
/// but only for as long as nobody re-adds the wildcard, which is precisely the edit that reads like
/// a harmless cleanup. Hence both halves here: every variant is named, and no arm is a catch-all.
pub fn unattended_classifier_coverage(
    args_src: &str,
    dispatch_src: &str,
) -> syn::Result<UnattendedCoverage> {
    let args = syn::parse_file(args_src)?;
    let variants: Vec<String> = args
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(e) if e.ident == "Commands" => Some(e),
            _ => None,
        })
        .map(|e| e.variants.iter().map(|v| v.ident.to_string()).collect())
        .unwrap_or_default();

    /// The classifier's own `match` — the first one in the function body. Nested matches inside an
    /// arm body would belong to that arm's logic, not to the classification, so recursion stops.
    struct TopMatch<'ast> {
        arms: Option<&'ast Vec<syn::Arm>>,
    }
    impl<'ast> Visit<'ast> for TopMatch<'ast> {
        fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
            if self.arms.is_none() {
                self.arms = Some(&node.arms);
            }
        }
    }

    /// Every `Commands::<Variant>` an arm's pattern names — all of them, since an or-pattern
    /// classifies a whole group at once.
    #[derive(Default)]
    struct Named {
        names: BTreeSet<String>,
    }
    impl Named {
        fn take(&mut self, path: &syn::Path) {
            let segments: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if let [enum_name, variant] = segments.as_slice() {
                if enum_name == "Commands" {
                    self.names.insert(variant.clone());
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Named {
        fn visit_pat_struct(&mut self, pat: &'ast syn::PatStruct) {
            self.take(&pat.path);
            syn::visit::visit_pat_struct(self, pat);
        }
        fn visit_pat_tuple_struct(&mut self, pat: &'ast syn::PatTupleStruct) {
            self.take(&pat.path);
            syn::visit::visit_pat_tuple_struct(self, pat);
        }
        fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
            self.take(&path.path);
            syn::visit::visit_expr_path(self, path);
        }
    }

    /// Whether a pattern is a catch-all — every spelling of one, since the point of the check is
    /// that a wildcard cannot come back under a different name:
    ///
    /// - `_` (`Pat::Wild`) and a bare binding `other` (`Pat::Ident` with no subpattern);
    /// - `x @ _` — a `Pat::Ident` **with** a subpattern, which is a catch-all exactly when its
    ///   subpattern is (`x @ Commands::Run { .. }` is not). Missing this recursion left `x @ _`
    ///   caught only by the weaker `unclassified()` backstop;
    /// - `pat if cond` (`Pat::Guard`) and parenthesised forms, unwrapped;
    /// - an or-pattern with a catch-all in any position (`_ | x`).
    fn is_catch_all(pat: &syn::Pat) -> bool {
        match pat {
            syn::Pat::Wild(_) => true,
            syn::Pat::Ident(ident) => match &ident.subpat {
                Some((_, subpat)) => is_catch_all(subpat),
                None => true,
            },
            syn::Pat::Guard(guard) => is_catch_all(&guard.pat),
            syn::Pat::Paren(paren) => is_catch_all(&paren.pat),
            syn::Pat::Or(or) => or.cases.iter().any(is_catch_all),
            _ => false,
        }
    }

    let dispatch = syn::parse_file(dispatch_src)?;
    let mut coverage = UnattendedCoverage {
        variants,
        ..UnattendedCoverage::default()
    };
    for item in &dispatch.items {
        let syn::Item::Fn(function) = item else {
            continue;
        };
        if function.sig.ident != "unattended_sandbox_surface" {
            continue;
        }
        let mut top = TopMatch { arms: None };
        top.visit_block(&function.block);
        for arm in top.arms.into_iter().flatten() {
            if is_catch_all(&arm.pat) {
                coverage.catch_all_arms += 1;
            }
            let mut named = Named::default();
            named.visit_pat(&arm.pat);
            coverage.classified.extend(named.names);
        }
    }
    Ok(coverage)
}

// ---------------------------------------------------------------------------------------------
// The pin census (C-328). A wiring line declares, in-source, the test that dies without it.
// ---------------------------------------------------------------------------------------------

/// The builder roots a [`Seam`] may hang off: the two SDK client builders a shipped surface
/// assembles its safety envelope through. Canonical spellings — [`ImportAliases`] resolves a
/// renamed or glob import to these before the match.
const PINNED_BUILDER_ROOTS: &[&[&str]] = &[
    &["flux_sdk", "Client", "builder"],
    &["flux_sdk", "FlowClient", "builder"],
];

/// Which wiring a [`Seam`] carries into the built client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeamKind {
    /// `.resource_limits(..)` — the operator's `[limits]` ceilings (C-307/C-314).
    ResourceLimits,
}

/// The builder methods the census requires a test for, and the kind each records.
///
/// **Deliberately one entry.** The census is a coverage floor for wiring whose deletion has already
/// been observed to change nothing (C-314), not a general "every builder call needs a test" rule: a
/// predicate wide enough to cover `.model(..)` would flag twelve call sites whose observation is
/// already implied by any end-to-end test of the surface, and a census that mostly reports noise is
/// a census nobody reads. C-330 widens it deliberately, seam family by seam family.
fn pinned_setter(method: &str) -> Option<SeamKind> {
    match method {
        "resource_limits" => Some(SeamKind::ResourceLimits),
        _ => None,
    }
}

/// One production wiring call site that a test is required to observe.
///
/// The span is a **byte range over the source that produced it**, not a line: C-329's runner
/// excises the call to prove the pinned test actually dies, and a builder chain link can span
/// several lines (`.resource_limits(\n    limits,\n)`). `line` is kept only so the waiver reader
/// ([`allow_reason`](fn@crate::layer)'s sibling in the test module) can find the comment block
/// immediately above, and so a violation reads like every other gate's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seam {
    /// 1-based line of the `.method` token.
    pub line: usize,
    /// Byte offset of the leading `.`, inclusive.
    pub span_start: usize,
    /// Byte offset just past the call's closing `)`.
    pub span_end: usize,
    pub kind: SeamKind,
    /// The resolved builder root the receiver chain bottoms out at, e.g. `flux_sdk::Client::builder`.
    pub builder: String,
    /// Nearest containing function/method, or `<module>` for a module-level initializer.
    pub function: String,
}

impl Seam {
    /// The seam's byte range, ready to slice out of the source it was scanned from.
    pub fn span(&self) -> std::ops::Range<usize> {
        self.span_start..self.span_end
    }
}

struct PinSeamVisitor<'a> {
    aliases: &'a ImportAliases,
    /// Local ident → the builder root a binding holds, so a chain split across statements
    /// (`let b = Client::builder(); b.resource_limits(..)`) is still anchored.
    bindings: HashMap<String, String>,
    functions: Vec<String>,
    seams: Vec<Seam>,
}

impl PinSeamVisitor<'_> {
    /// The builder root this chain root names: a pinned `::builder()` construction, or a local
    /// already bound to one.
    fn builder_root(&self, expr: &syn::Expr) -> Option<String> {
        if let syn::Expr::Call(call) = expr {
            let path = transparent_expr_path(&call.func)?;
            let segments = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            let resolved = self.aliases.resolve(&segments);
            let parts = resolved.iter().map(String::as_str).collect::<Vec<_>>();
            return PINNED_BUILDER_ROOTS
                .contains(&parts.as_slice())
                .then(|| resolved.join("::"));
        }
        let syn::Expr::Path(path) = expr else {
            return None;
        };
        let ident = path.path.get_ident()?.to_string();
        self.bindings.get(&ident).cloned()
    }

    /// Walk a method-call chain from the outside in, recording every pinned setter called on a
    /// chain whose root is an SDK client builder. Returns that root, so a `let` can bind its ident.
    fn chain(&mut self, expr: &syn::Expr) -> Option<String> {
        let mut calls = Vec::new();
        let mut cursor = expr;
        let root = loop {
            match cursor {
                syn::Expr::MethodCall(call) => {
                    calls.push(call);
                    cursor = &call.receiver;
                }
                syn::Expr::Reference(reference) => cursor = &reference.expr,
                syn::Expr::Paren(paren) => cursor = &paren.expr,
                syn::Expr::Group(group) => cursor = &group.expr,
                syn::Expr::Try(inner) => cursor = &inner.expr,
                other => break other,
            }
        };
        let builder = self.builder_root(root)?;
        for call in &calls {
            let Some(kind) = pinned_setter(&call.method.to_string()) else {
                continue;
            };
            self.seams.push(Seam {
                line: start_line(call.method.span()),
                span_start: call.dot_token.span().byte_range().start,
                span_end: call.paren_token.span.close().byte_range().end,
                kind,
                builder: builder.clone(),
                function: self
                    .functions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "<module>".into()),
            });
        }
        // The chain itself is consumed here, so recurse only into the arguments — a nested builder
        // inside one of them must still be seen.
        for call in calls {
            for arg in &call.args {
                self.visit_expr(arg);
            }
        }
        if let syn::Expr::Call(call) = root {
            for arg in &call.args {
                self.visit_expr(arg);
            }
        }
        Some(builder)
    }
}

impl<'ast> Visit<'ast> for PinSeamVisitor<'_> {
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

    fn visit_local(&mut self, local: &'ast syn::Local) {
        let Some(init) = &local.init else {
            return;
        };
        let ident = simple_binding_ident(&local.pat);
        let root = self.chain(&init.expr);
        if root.is_none() {
            self.visit_expr(&init.expr);
        }
        if let (Some(ident), Some(root)) = (ident, root) {
            self.bindings.insert(ident, root);
        }
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if matches!(expr, syn::Expr::MethodCall(_) | syn::Expr::Call(_))
            && self.chain(expr).is_some()
        {
            return;
        }
        syn::visit::visit_expr(self, expr);
    }
}

/// Resolve every wiring seam in one **production** Rust source: a [`pinned_setter`] called on a
/// method chain rooted at an SDK client builder. Imports, renamed imports, glob imports, module and
/// type aliases, local bindings, and multiline chains are followed; `#[cfg(test)]` items, comments
/// and strings are excluded structurally by `syn`.
pub fn pin_seams(src: &str) -> syn::Result<Vec<Seam>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ImportAliases::default();
    ImportAliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_type_aliases();
    let mut visitor = PinSeamVisitor {
        aliases: &aliases,
        bindings: HashMap::new(),
        functions: Vec::new(),
        seams: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.seams.sort();
    Ok(visitor.seams)
}

/// Every test function name declared in one Rust source: any `fn` carrying an attribute whose last
/// path segment is `test` (`#[test]`, `#[tokio::test(..)]`, `#[test_log::test]`).
///
/// Unlike the production scanners this walks **into** `#[cfg(test)]` modules — that is where most of
/// the repo's tests live, and a pin that resolves to nothing is the drift this exists to catch.
pub fn test_function_names(src: &str) -> syn::Result<Vec<String>> {
    #[derive(Default)]
    struct Tests {
        names: Vec<String>,
    }

    fn is_test_attr(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        })
    }

    impl<'ast> Visit<'ast> for Tests {
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if is_test_attr(&item.attrs) {
                self.names.push(item.sig.ident.to_string());
            }
            syn::visit::visit_item_fn(self, item);
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if is_test_attr(&item.attrs) {
                self.names.push(item.sig.ident.to_string());
            }
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    let file = syn::parse_file(src)?;
    let mut tests = Tests::default();
    tests.visit_file(&file);
    tests.names.sort();
    tests.names.dedup();
    Ok(tests.names)
}

/// C-393 — one call, from **test** code, to an entry point that resolves flux's user-global
/// discovery roots (`~/.flux/commands`, `~/.claude/commands`, `~/.flux/skills`, `~/.agents/skills`,
/// `~/.claude/skills`, `~/.kube/config`) from the **process** environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AmbientDiscoveryCall {
    pub line: usize,
    /// The called function or method, by its final path segment.
    pub callee: String,
}

/// Which part of a source file is test code, for [`ambient_discovery_calls`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestScope {
    /// An integration-test source (`<crate>/tests/**.rs`): every item in the file is test code.
    WholeFile,
    /// A `src/` module: only items carrying `#[cfg(test)]`, and everything nested inside them.
    /// Anchored on the attribute rather than on a text marker because production code follows an
    /// inline test module in several crates (`flux-cli`'s `execution.rs` has four of them), so a
    /// "scan from the first `#[cfg(test)]` to EOF" heuristic drags production calls in.
    CfgTestItems,
}

/// The entry points whose home-rooted reads answer from the process environment (C-393).
///
/// The first four are the readers themselves, in `flux_runtime`; the rest are the wrapper families
/// that reach them without naming them, which is the class C-392 showed a census misses when it
/// greps for the reader alone. Each has an additive `*_in(.., &DiscoveryEnv)` counterpart — a
/// different identifier, so a pinned call never matches this list.
///
/// Deliberately a frontier rather than a call graph: it is the measured set of names test code can
/// reach the probe through today. A new wrapper belongs here the day it is written, which is the
/// same maintenance contract `flux-server`'s `router_env_is_pinned.rs` carries for its four.
pub const AMBIENT_DISCOVERY_ENTRY_POINTS: [&str; 10] = [
    "detect_signals",
    "discover_skills",
    "discover_skills_from",
    "discover_commands",
    "try_with_default_skills",
    "with_default_skills",
    "try_with_model_invoked_skills",
    "load_command_files",
    "load_skills",
    "load_model_invoked_skill_catalog",
];

/// The pinned counterparts of [`AMBIENT_DISCOVERY_ENTRY_POINTS`], counted so the census cannot pass
/// by scanning nothing.
pub const PINNED_DISCOVERY_ENTRY_POINTS: [&str; 10] = [
    "detect_signals_in",
    "discover_skills_in",
    "discover_commands_in",
    "try_with_default_skills_in",
    "try_with_model_invoked_skills_in",
    "load_command_files_in",
    "load_skills_in",
    "load_model_invoked_skill_catalog_in",
    "with_discovery_env",
    "DiscoveryEnv",
];

struct DiscoveryCallVisitor<'a> {
    names: &'a [&'a str],
    /// `true` once inside a `#[cfg(test)]` item (or from the start, for [`TestScope::WholeFile`]).
    in_test: bool,
    hits: Vec<AmbientDiscoveryCall>,
}

impl DiscoveryCallVisitor<'_> {
    fn record(&mut self, callee: &syn::Ident) {
        if !self.in_test {
            return;
        }
        let name = callee.to_string();
        if self.names.contains(&name.as_str()) {
            self.hits.push(AmbientDiscoveryCall {
                line: start_line(callee.span()),
                callee: name,
            });
        }
    }

    fn scoped<T>(&mut self, gated: bool, node: T, walk: impl FnOnce(&mut Self, T)) {
        let outer = self.in_test;
        self.in_test = outer || gated;
        walk(self, node);
        self.in_test = outer;
    }

    /// Walk a macro's token stream for calls.
    ///
    /// **This is the blind spot that made a first cut of the C-393 census pass while a reverted
    /// call site sat in the tree.** `syn` keeps a macro invocation's body as an opaque
    /// `TokenStream` — it is not an expression tree — and the single most common shape in this
    /// repo's test corpus is exactly `assert!(load_command_files(&root, ..).is_empty())`. An
    /// AST-only scanner therefore reports zero violations over a corpus full of them.
    ///
    /// Token-level, so "a call" is `<ident> ( .. )`: an identifier immediately followed by a
    /// parenthesized group. A path (`flux_runtime::detect_signals(..)`) leaves the *last* segment
    /// pending when the group arrives, which is the identifier that names the callee; a method call
    /// (`.try_with_default_skills()`) works the same way.
    fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        if !self.in_test {
            return;
        }
        let mut pending: Option<proc_macro2::Ident> = None;
        for tree in tokens {
            match tree {
                proc_macro2::TokenTree::Ident(ident) => pending = Some(ident),
                proc_macro2::TokenTree::Group(group) => {
                    if let Some(ident) = pending.take() {
                        if group.delimiter() == proc_macro2::Delimiter::Parenthesis {
                            self.record(&ident);
                        }
                    }
                    self.scan_macro_tokens(group.stream());
                }
                // `::` and `.` continue a call expression; any other punctuation ends it. Turbofish
                // (`f::<T>(..)`) is a `Group`-free sequence that also keeps the callee pending,
                // which the `<`/`>` arms below deliberately do not break.
                proc_macro2::TokenTree::Punct(punct) => {
                    if !matches!(punct.as_char(), ':' | '.' | '<' | '>') {
                        pending = None;
                    }
                }
                proc_macro2::TokenTree::Literal(_) => pending = None,
            }
        }
    }
}

impl<'ast> Visit<'ast> for DiscoveryCallVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_mod(this, item)
        });
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_fn(this, item)
        });
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_impl(this, item)
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_impl_item_fn(this, item)
        });
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(segment) = path.path.segments.last() {
                self.record(&segment.ident);
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.record(&call.method);
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.scan_macro_tokens(mac.tokens.clone());
        syn::visit::visit_macro(self, mac);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        // A bare path (`DiscoveryEnv::empty` passed as a value, or a `use`-free qualified
        // reference) still counts for the pinned tally — it is only ever the *ambient* list that
        // must be a call, and those names are all functions.
        if let Some(segment) = path.path.segments.first() {
            self.record(&segment.ident);
        }
        syn::visit::visit_expr_path(self, path);
    }
}

/// Every call, from test code in `src`, to one of `names`.
///
/// Structural rather than textual: `syn` excludes comments and string literals for free, so prose
/// naming `detect_signals()` is not a call, and `detect_signals_in(..)` is a different identifier
/// rather than a substring that has to be excluded by hand.
fn discovery_calls_named(
    src: &str,
    scope: TestScope,
    names: &[&str],
) -> syn::Result<Vec<AmbientDiscoveryCall>> {
    let file = syn::parse_file(src)?;
    let mut visitor = DiscoveryCallVisitor {
        names,
        in_test: matches!(scope, TestScope::WholeFile),
        hits: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.hits.sort();
    visitor.hits.dedup();
    Ok(visitor.hits)
}

/// Every [`AMBIENT_DISCOVERY_ENTRY_POINTS`] call made from test code in `src` (C-393).
pub fn ambient_discovery_calls(
    src: &str,
    scope: TestScope,
) -> syn::Result<Vec<AmbientDiscoveryCall>> {
    discovery_calls_named(src, scope, &AMBIENT_DISCOVERY_ENTRY_POINTS)
}

/// Every [`PINNED_DISCOVERY_ENTRY_POINTS`] reference made from test code in `src` — the vacuity
/// floor for [`ambient_discovery_calls`].
pub fn pinned_discovery_calls(
    src: &str,
    scope: TestScope,
) -> syn::Result<Vec<AmbientDiscoveryCall>> {
    discovery_calls_named(src, scope, &PINNED_DISCOVERY_ENTRY_POINTS)
}

/// The setter that can replace a `System`'s metric roots (C-673).
///
/// The metrics allowance in `no_unreviewed_guarded_port_backend_outside_system` rests on this name
/// having no production caller: the roots the native reader consults are `/proc` and `/sys`
/// because every entry point constructs `MetricsRoots::native()` and nothing operation-, CLI- or
/// wire-shaped can replace them. That is a claim about the tree, so it is checked against the tree.
pub const METRICS_ROOTS_SETTER: &str = "with_metrics_roots";

/// Every **production** call to [`METRICS_ROOTS_SETTER`] in `src`, by line (C-673).
///
/// Structural, through the same visitor as [`plugin_response_ingest_sites`], which is what makes
/// the answer trustworthy: the `pub fn with_metrics_roots` definition is not a call, prose and
/// string literals naming it are not calls, and — the finding this replaced a text scan for — the
/// production half of a file whose inline test module appears near the top is read rather than
/// skipped.
pub fn metrics_roots_setter_calls(src: &str) -> syn::Result<Vec<usize>> {
    Ok(production_calls_named(src, &[METRICS_ROOTS_SETTER])?
        .into_iter()
        .map(|hit| hit.line)
        .collect())
}

/// C-404 — one **production** call to `PluginHost::call_with_host`: a place where a
/// plugin-authored response enters flux.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginResponseIngest {
    pub line: usize,
}

/// The method whose return value is a plugin-authored response crossing into flux
/// (`PluginHost::call_with_host`).
///
/// One name, not a frontier: unlike C-393's discovery readers there are no wrapper families here —
/// `call_with_host` is the only way a plugin's `operation.call` response is obtained, and
/// `PluginHost::call` is a thin self-delegation this scanner sees as the call it is.
pub const PLUGIN_RESPONSE_INGEST_METHOD: &str = "call_with_host";

/// Every production call to one of `names`, by line.
///
/// Shared by the C-404 plugin-response census and C-673's metrics-roots guard. Both ask the same
/// question — *does non-test code call this?* — and the answer has to be anchored on the
/// `#[cfg(test)]` attribute the way [`TestScope::CfgTestItems`] documents, not on the first
/// occurrence of the marker in the file's text: production code follows an inline test module in
/// 63 of this workspace's source files, and a text scan reads all of it as test code.
struct ProductionCallVisitor<'a> {
    names: &'a [&'a str],
    /// `true` once inside a `#[cfg(test)]` item — test code has no operator-visible surface to
    /// protect and is deliberately not counted.
    in_test: bool,
    hits: Vec<PluginResponseIngest>,
}

impl ProductionCallVisitor<'_> {
    fn record(&mut self, callee: &proc_macro2::Ident) {
        if self.in_test || !self.names.contains(&callee.to_string().as_str()) {
            return;
        }
        self.hits.push(PluginResponseIngest {
            line: start_line(callee.span()),
        });
    }

    fn scoped<T>(&mut self, gated: bool, node: T, walk: impl FnOnce(&mut Self, T)) {
        let outer = self.in_test;
        self.in_test = outer || gated;
        walk(self, node);
        self.in_test = outer;
    }

    /// Walk a macro's token stream for calls — the blind spot C-393 documents: `syn` keeps a macro
    /// body as an opaque `TokenStream`, so an ingest inside `tokio::select!` or an `assert!` is
    /// invisible to an AST-only scan. "A call" is `<ident> ( .. )`.
    fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        if self.in_test {
            return;
        }
        let mut pending: Option<proc_macro2::Ident> = None;
        for tree in tokens {
            match tree {
                proc_macro2::TokenTree::Ident(ident) => pending = Some(ident),
                proc_macro2::TokenTree::Group(group) => {
                    if let Some(ident) = pending.take() {
                        if group.delimiter() == proc_macro2::Delimiter::Parenthesis {
                            self.record(&ident);
                        }
                    }
                    self.scan_macro_tokens(group.stream());
                }
                proc_macro2::TokenTree::Punct(punct) => {
                    if !matches!(punct.as_char(), ':' | '.' | '<' | '>') {
                        pending = None;
                    }
                }
                proc_macro2::TokenTree::Literal(_) => pending = None,
            }
        }
    }
}

impl<'ast> Visit<'ast> for ProductionCallVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_mod(this, item)
        });
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_fn(this, item)
        });
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_item_impl(this, item)
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let gated = has_cfg_test(&item.attrs);
        self.scoped(gated, item, |this, item| {
            syn::visit::visit_impl_item_fn(this, item)
        });
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.record(&call.method);
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // The UFCS spelling — `PluginHost::call_with_host(&mut host, ..)` — is the same ingest.
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(segment) = path.path.segments.last() {
                self.record(&segment.ident);
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.scan_macro_tokens(mac.tokens.clone());
        syn::visit::visit_macro(self, mac);
    }
}

/// Every production plugin-response ingest site in `src` (C-404).
///
/// Structural rather than textual, which is the whole reason this replaced a doc-comment census:
/// `syn` excludes comments and string literals for free, so the table *describing* the sites is not
/// itself counted as one — the failure mode that made the prose census wrong on the day it was
/// written. Items carrying `#[cfg(test)]` are excluded; the method's own `pub async fn` definition
/// is not a call and so is not a hit.
pub fn plugin_response_ingest_sites(src: &str) -> syn::Result<Vec<PluginResponseIngest>> {
    production_calls_named(src, &[PLUGIN_RESPONSE_INGEST_METHOD])
}

/// Every production call to one of `names` in `src`.
///
/// Structural rather than textual, which is the whole reason this replaced a doc-comment census:
/// `syn` excludes comments and string literals for free, so the table *describing* a site is not
/// itself counted as one — the failure mode that made the prose census wrong on the day it was
/// written. Items carrying `#[cfg(test)]` are excluded by the attribute, not by their position in
/// the file; a name's own `fn` definition is not a call and so is not a hit.
fn production_calls_named(src: &str, names: &[&str]) -> syn::Result<Vec<PluginResponseIngest>> {
    let file = syn::parse_file(src)?;
    let mut visitor = ProductionCallVisitor {
        names,
        in_test: false,
        hits: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.hits.sort();
    // Deliberately NOT deduplicated: two ingests chained on one line are two ingests, and a census
    // that pins counts must not let the second hide behind the first's line number.
    Ok(visitor.hits)
}

/// C-325 — the credential shapes a hosted git forge's secret scanning blocks a push on, as
/// `(vendor prefix, minimum credential-body characters after it)`.
///
/// Deliberately looser than the real partner patterns, which add checksums and validity probes this
/// cannot reproduce, and deliberately stricter than "whatever fired last time". The asymmetry is the
/// point: over-flagging costs one fragment split in a fixture, while under-flagging costs a push a
/// human has to unblock by hand — on a commit that then stays blocked *forever*, for every future
/// clone, because the literal is in the history rather than in the working tree.
///
/// The floors are set below every real token's body length and above the placeholder spellings that
/// exist to exercise the redactor's **registered-value** path rather than its shape path
/// (`xoxb-redact-me-1234` and friends, whose bodies are 14 characters).
///
/// This table is safe to spell out: every entry is a prefix followed immediately by `"`, which is
/// not a body character, so the list does not match itself.
pub const PUSH_PROTECTION_SHAPES: &[(&str, usize)] = &[
    ("sk-ant-api", 20),
    ("sk_live_", 20),
    ("sk_test_", 20),
    ("xoxb-", 20),
    ("xoxp-", 20),
    ("xoxa-", 20),
    ("xoxr-", 20),
    ("xoxs-", 20),
    ("xoxe-", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 20),
    ("glpat-", 20),
    ("hf_", 30),
    ("AKIA", 16),
    ("AIza", 30),
    ("ya29.", 20),
];

/// The characters a credential body is spelled from. Base64url plus `-`, which every vendor shape
/// in [`PUSH_PROTECTION_SHAPES`] draws from.
fn credential_body_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Every push-protection-shaped literal in one source, as `(1-based line, the matched text)`.
///
/// **Byte-level on purpose, not AST-level.** The scanner that blocks the push reads the file, not
/// the program: a shape assembled from two fragments — `concat!("sk-ant-", "api03-…")`, or a
/// `format!` of the same two halves — is genuinely absent from the bytes it scans, and so must be
/// absent from the bytes this scans. Which is exactly why a fixture may keep the full credential
/// *shape* at run time while the file on disk carries neither half of it in matchable form.
pub fn push_protection_shapes(src: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (index, line) in src.lines().enumerate() {
        for (prefix, min_body) in PUSH_PROTECTION_SHAPES {
            let mut from = 0;
            while let Some(at) = line[from..].find(prefix) {
                let body_start = from + at + prefix.len();
                let body = line[body_start..]
                    .bytes()
                    .take_while(|b| credential_body_char(*b as char))
                    .count();
                if body >= *min_body {
                    hits.push((index + 1, line[from + at..body_start + body].to_string()));
                }
                from = body_start;
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, TargetKind};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    // One exhaustively reviewed classification for first-party operation implementation packs in
    // the production agent catalog. The direct-I/O script delegates to these tests and carries no
    // second, narrower crate list.
    const MODEL_FACING_OPERATION_CRATES: &[&str] = &[
        "flux-tools",
        "flux-web",
        "flux-capabilities",
        "flux-eval",
        "flux-cognition",
        "flux-flow",
        "flux-orchestrate",
        "flux-app",
    ];
    const EXTERNAL_CFG_TEST_MODULES: &[&str] = &["crates/flux-flow/src/voice/tests.rs"];

    /// C-328. `// flux-pin: <test_name> [prose]` — the wiring line names the test that dies without
    /// it. The first whitespace-delimited token is the test function name and must resolve.
    const PIN_MARKER: &str = "flux-pin:";
    /// `// flux-pin-exempt: <why>` — a seam deliberately left unobserved. The `flux test` replay
    /// client is the shape this exists for: see `lab_cmd.rs`'s `offline_client` doc comment.
    const PIN_EXEMPT_MARKER: &str = "flux-pin-exempt:";
    /// The standing exemption budget. Zero today; one is the room a genuinely-unobservable seam may
    /// take. A story that needs a second is a story that must widen this number in the same diff,
    /// under review — which is the only way "exempt" stays a decision rather than a habit.
    const MAX_PIN_EXEMPTIONS: usize = 1;

    /// Tracked paths that sit below a Cargo output directory. Kept pure so the directory predicate
    /// is pinned independently from the Git-index census that supplies its production input.
    fn tracked_cargo_output_paths(paths: &[&str]) -> Vec<String> {
        let mut violations = paths
            .iter()
            .filter(|path| {
                let components = path.split('/').collect::<Vec<_>>();
                components
                    .iter()
                    .take(components.len().saturating_sub(1))
                    .any(|component| *component == "target" || component.starts_with("target-"))
            })
            .map(|path| (*path).to_string())
            .collect::<Vec<_>>();
        violations.sort();
        violations.dedup();
        violations
    }

    #[test]
    fn tracked_build_output_detector_rejects_forced_cargo_outputs() {
        let paths = [
            "target/debug/flux",
            "target-int/debug/deps/flux",
            "plugins/target/release/plugin",
            "crates/fixture/target-check/debug/helper",
            "docs/target-platform.md",
            "assets/targets/icon.svg",
        ];

        assert_eq!(
            tracked_cargo_output_paths(&paths),
            [
                "crates/fixture/target-check/debug/helper",
                "plugins/target/release/plugin",
                "target-int/debug/deps/flux",
                "target/debug/flux",
            ]
        );
    }

    /// C-489: `.gitignore` is advisory and `git add -f` bypasses it. Read the index itself so a
    /// force-added Cargo tree fails before it can be merged into main.
    #[test]
    fn no_cargo_build_output_is_tracked() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let output = std::process::Command::new("git")
            .args(["-C", repo_root.to_str().unwrap(), "ls-files", "-z"])
            .output()
            .expect("run git ls-files");
        assert!(output.status.success(), "git ls-files failed");
        let tracked = std::str::from_utf8(&output.stdout).expect("tracked paths are UTF-8");
        let paths = tracked
            .split('\0')
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        let violations = tracked_cargo_output_paths(&paths);

        assert!(
            violations.is_empty(),
            "Cargo build output is tracked despite its ignore rule:\n  {}\n\
             remove it from the index and history; never use `git add -f` for target trees (C-489)",
            violations.join("\n  ")
        );
    }

    /// The non-empty reason of a `// <marker> <reason>` waiver in the comment block immediately above
    /// `line`. A bare marker with no reason is not a waiver.
    fn allow_reason(source: &str, line: usize, marker: &str) -> Option<String> {
        let lines = source.lines().collect::<Vec<_>>();
        let mut cursor = line.saturating_sub(1);
        while cursor > 0 {
            cursor -= 1;
            let trimmed = lines.get(cursor)?.trim();
            let comment = trimmed.strip_prefix("//")?.trim();
            if let Some(reason) = comment.strip_prefix(marker) {
                let reason = reason.trim();
                return (!reason.is_empty()).then(|| reason.to_string());
            }
        }
        None
    }

    fn direct_io_allow_reason(source: &str, line: usize) -> Option<String> {
        allow_reason(source, line, "flux-allow-direct-io:")
    }

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

    /// The sources the live-backend census reads: every crate's `src` **and** `examples` (C-716).
    ///
    /// Examples are in scope here where they are not for [`workspace_source_files`], because an SDK
    /// example backend is the shape integrators copy. One that built its own client would teach the
    /// exact thing the census exists to prevent.
    fn live_backend_source_files(repo_root: &Path) -> Vec<PathBuf> {
        let mut files = workspace_source_files(repo_root);
        for workspace_dir in [repo_root.join("crates"), repo_root.join("plugins")] {
            let Ok(entries) = std::fs::read_dir(workspace_dir) else {
                continue;
            };
            for entry in entries {
                let dir = entry.unwrap().path();
                if dir.is_dir() {
                    collect_rs(&dir.join("examples"), &mut files);
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
        // name -> its vanity-prefixed production dependencies, for the ordering check below.
        let mut vanity_deps: Vec<(String, Vec<String>)> = Vec::new();

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
                let mut deps = Vec::new();
                for dependency in &package.dependencies {
                    if dependency.kind == DependencyKind::Development {
                        continue;
                    }
                    if dependency.path.is_some() && dependency.req.to_string() == "*" {
                        path_only.push(format!("{} -> {}", package.name, dependency.name));
                    }
                    if dependency.name.starts_with("codewandler-") {
                        deps.push(dependency.name.clone());
                    }
                }
                vanity_deps.push((package.name.to_string(), deps));
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
        let scripted_order = array
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("codewandler-"))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let scripted = scripted_order.iter().cloned().collect::<BTreeSet<_>>();

        assert_eq!(
            scripted, closure,
            "scripts/publish-crates-io.sh must list every vanity-prefixed package of the ROOT \
             workspace exactly once (packages in plugins/ ship with the pack — see C-146)"
        );

        // ORDER, not just membership. `cargo publish` refuses a crate whose dependency is not yet
        // on crates.io, and the closure is published strictly in the scripted sequence — so a crate
        // listed before one of its own dependencies fails the release AFTER the tag is pushed,
        // which is the most expensive moment to find out. Two such inversions were live at once
        // (flux-spec before flux-policy, from C-141's `FlowEffect` move; flux-plugin-protocol
        // before flux-spec/flux-evidence, from C-142's insert) and the membership check above saw
        // neither, because both sets were identical.
        let position = |name: &str| scripted_order.iter().position(|n| n == name);
        let mut inversions = Vec::new();
        for (name, deps) in &vanity_deps {
            let Some(own) = position(name) else { continue };
            for dep in deps {
                if let Some(dep_at) = position(dep) {
                    if dep_at > own {
                        inversions.push(format!(
                            "{name} (#{own}) is published before its dependency {dep} (#{dep_at})"
                        ));
                    }
                }
            }
        }
        inversions.sort();
        inversions.dedup();
        assert!(
            inversions.is_empty(),
            "scripts/publish-crates-io.sh publishes in order, so every crate must come after its \
             own dependencies — `cargo publish` would reject these, and only after the release tag \
             is pushed:\n{}",
            inversions.join("\n")
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

    /// Where the workspace test gate is written down. `task test` and `task install` both expand to
    /// the Taskfile's `cargo test --workspace …`; `scripts/release-full-gate.sh` runs the unfiltered
    /// workspace suite. Both files are read, so neither can drop a crate on its own.
    const DECLARED_GATE_FILES: &[&str] = &["Taskfile.yaml", "scripts/release-full-gate.sh"];

    /// Every target-selection flag `cargo test` understands. The singular forms take a target name
    /// and select exactly one target, so they are recorded but never count as selecting a kind.
    const CARGO_TEST_SELECTORS: &[&str] = &[
        "--lib",
        "--bins",
        "--bin",
        "--tests",
        "--test",
        "--benches",
        "--bench",
        "--examples",
        "--example",
        "--all-targets",
        "--doc",
    ];

    /// One `cargo test` invocation as written in a gate file.
    #[derive(Debug, PartialEq, Eq)]
    struct CargoTestInvocation {
        /// `--workspace` or its `--all` alias was passed.
        workspace: bool,
        /// The target-selection flags in written order. **Empty means every target**, which is
        /// `cargo test`'s default and the only selection that cannot leave a crate behind.
        selectors: Vec<String>,
    }

    impl CargoTestInvocation {
        fn selects(&self, selector: &str) -> bool {
            self.selectors.is_empty()
                || self
                    .selectors
                    .iter()
                    .any(|written| written == "--all-targets" || written == selector)
        }
    }

    /// Read every `cargo test` invocation out of a gate file's text.
    ///
    /// Deliberately a token scan rather than a YAML or shell parse: the same command is written five
    /// ways across the two gate files — POSIX and Windows, `task test` and `task install`, plus the
    /// release script's `owned_cargo` wrapper — and a parser per dialect would be the thing that
    /// silently stops finding one of them. Comment-only lines are skipped so prose *about* a command
    /// is never mistaken for the command.
    fn cargo_test_invocations(text: &str) -> Vec<CargoTestInvocation> {
        let mut found = Vec::new();
        for line in text.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let tokens = line
                .split_whitespace()
                .map(|token| token.trim_matches(|c| matches!(c, '\'' | '"' | '`')))
                .collect::<Vec<_>>();
            let mut index = 0;
            while index < tokens.len() {
                let is_cargo = tokens[index]
                    .rsplit(['/', '\\'])
                    .next()
                    .is_some_and(|name| matches!(name, "cargo" | "owned-cargo" | "owned_cargo"));
                if !is_cargo || tokens.get(index + 1) != Some(&"test") {
                    index += 1;
                    continue;
                }
                index += 2;
                let mut invocation = CargoTestInvocation {
                    workspace: false,
                    selectors: Vec::new(),
                };
                while let Some(token) = tokens.get(index) {
                    // `--` hands the rest to the test harness; the others end the command.
                    if matches!(*token, "--" | "&&" | "||" | ";" | "|") {
                        break;
                    }
                    if matches!(*token, "--workspace" | "--all") {
                        invocation.workspace = true;
                    }
                    if CARGO_TEST_SELECTORS.contains(token) {
                        invocation.selectors.push((*token).to_string());
                    }
                    index += 1;
                }
                found.push(invocation);
            }
        }
        found
    }

    /// `(selector, target name, entry point)` for every target of `package` that a *default-feature*
    /// unit-test gate can reach: its library and its binaries. Integration tests, examples, benches
    /// and build scripts are not unit-test carriers, so they are excluded here — `tests/` is covered
    /// by the unfiltered release gate instead.
    ///
    /// Targets behind `required-features` are excluded too, and not because they matter less: no
    /// target-selection flag reaches them, so demanding one would be a false claim. `flux-lang`'s
    /// `fluxlang` binary is exactly that shape — 11 tests behind the non-default `cli` feature,
    /// unreachable by `--bins` and by an unfiltered `cargo test --workspace` alike. That class is
    /// C-308's, and `scripts/check-feature-gated-tests.sh` owns it: its ledger runs
    /// `codewandler-flux-lang/cli` and refuses any feature with no stated disposition.
    fn unit_test_targets(
        package: &cargo_metadata::Package,
    ) -> Vec<(&'static str, String, PathBuf)> {
        package
            .targets
            .iter()
            .filter(|target| target.required_features.is_empty())
            .filter_map(|target| {
                let selector = if target.is_kind(TargetKind::Lib)
                    || target.is_kind(TargetKind::RLib)
                    || target.is_kind(TargetKind::ProcMacro)
                    || target.is_kind(TargetKind::CDyLib)
                    || target.is_kind(TargetKind::DyLib)
                    || target.is_kind(TargetKind::StaticLib)
                {
                    "--lib"
                } else if target.is_kind(TargetKind::Bin) {
                    "--bins"
                } else {
                    return None;
                };
                Some((
                    selector,
                    target.name.clone(),
                    target.src_path.as_std_path().to_path_buf(),
                ))
            })
            .collect()
    }

    /// Every `.rs` file a Cargo target compiles, resolved from its entry point by following `mod`
    /// declarations the way rustc does — inline modules nest a directory level, `mod x;` resolves to
    /// `x.rs` or `x/mod.rs`, and `#[path = "…"]` wins over both.
    ///
    /// "Every file under the target's directory" is *not* good enough here. `flux-lsp`'s `main.rs`
    /// and `lib.rs` share one `src/`, and a directory census blames the eleven-line binary for the
    /// library's 53 tests — reporting a gate hole that does not exist. `#[cfg(test)]` modules are
    /// deliberately followed: they are where nearly all of this repo's unit tests live.
    fn target_module_sources(entry: &Path) -> Vec<PathBuf> {
        fn declared_path(attrs: &[syn::Attribute]) -> Option<String> {
            attrs.iter().find_map(|attr| {
                if !attr.path().is_ident("path") {
                    return None;
                }
                let syn::Meta::NameValue(pair) = &attr.meta else {
                    return None;
                };
                let syn::Expr::Lit(literal) = &pair.value else {
                    return None;
                };
                match &literal.lit {
                    syn::Lit::Str(text) => Some(text.value()),
                    _ => None,
                }
            })
        }

        fn descend(items: &[syn::Item], module_dir: &Path, out: &mut Vec<PathBuf>) {
            for item in items {
                let syn::Item::Mod(module) = item else {
                    continue;
                };
                let name = module.ident.to_string();
                if let Some((_, inline)) = &module.content {
                    descend(inline, &module_dir.join(&name), out);
                    continue;
                }
                let file = match declared_path(&module.attrs) {
                    Some(declared) => module_dir.join(declared),
                    None => {
                        let flat = module_dir.join(format!("{name}.rs"));
                        if flat.is_file() {
                            flat
                        } else {
                            module_dir.join(&name).join("mod.rs")
                        }
                    }
                };
                let child_dir = match file.file_stem().and_then(|stem| stem.to_str()) {
                    // `x/mod.rs` owns `x/`; `x.rs` owns the sibling directory `x/`.
                    Some("mod") => file.parent().map(Path::to_path_buf),
                    Some(stem) => file.parent().map(|parent| parent.join(stem)),
                    None => None,
                };
                if let Some(child_dir) = child_dir {
                    walk(&file, &child_dir, out);
                }
            }
        }

        fn walk(file: &Path, module_dir: &Path, out: &mut Vec<PathBuf>) {
            if out.iter().any(|seen| seen == file) || !file.is_file() {
                return;
            }
            out.push(file.to_path_buf());
            let source = std::fs::read_to_string(file).unwrap_or_else(|error| {
                panic!("read {} for the gate census: {error}", file.display())
            });
            let parsed = syn::parse_file(&source).unwrap_or_else(|error| {
                panic!("parse {} for the gate census: {error}", file.display())
            });
            descend(&parsed.items, module_dir, out);
        }

        let mut out = Vec::new();
        if let Some(root_dir) = entry.parent() {
            walk(entry, root_dir, &mut out);
        }
        out
    }

    /// The module walk's own pins: the resolution rules a directory census gets wrong. `orphan.rs`
    /// and `lib.rs` sit in the same directory as the entry point and are reached by nothing, so a
    /// walk that finds them is a walk that would misattribute `flux-lsp`'s library tests to its
    /// binary.
    #[test]
    fn module_walk_follows_declarations_rather_than_the_directory() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "flux-codegate-modwalk-{}-{nonce}",
            std::process::id()
        ));
        let src = root.join("src");
        std::fs::create_dir_all(src.join("flat")).unwrap();
        std::fs::create_dir_all(src.join("inline")).unwrap();
        std::fs::create_dir_all(src.join("folder")).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "mod flat;\n\
             mod folder;\n\
             #[cfg(test)]\n\
             mod harness;\n\
             mod inline {\n    mod deep;\n}\n\
             #[path = \"renamed.rs\"]\n\
             mod aliased;\n\
             fn main() {}\n",
        )
        .unwrap();
        // `flat.rs` owns the sibling `flat/` directory.
        std::fs::write(src.join("flat.rs"), "mod child;\n").unwrap();
        std::fs::write(src.join("flat").join("child.rs"), "").unwrap();
        // `folder/mod.rs` owns `folder/` itself, not `folder/folder/`.
        std::fs::write(src.join("folder").join("mod.rs"), "mod leaf;\n").unwrap();
        std::fs::write(src.join("folder").join("leaf.rs"), "").unwrap();
        std::fs::write(src.join("harness.rs"), "#[test]\nfn t() {}\n").unwrap();
        std::fs::write(src.join("inline").join("deep.rs"), "").unwrap();
        std::fs::write(src.join("renamed.rs"), "").unwrap();
        // Declared by nothing. A directory census would sweep both of these in.
        std::fs::write(src.join("orphan.rs"), "#[test]\nfn never_compiled() {}\n").unwrap();
        std::fs::write(src.join("lib.rs"), "#[test]\nfn a_sibling_target() {}\n").unwrap();

        let mut reached = target_module_sources(&src.join("main.rs"))
            .iter()
            .map(|file| {
                file.strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        reached.sort();
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(
            reached,
            [
                "flat.rs",
                "flat/child.rs",
                "folder/leaf.rs",
                "folder/mod.rs",
                "harness.rs",
                "inline/deep.rs",
                "main.rs",
                "renamed.rs",
            ]
        );
    }

    /// The scanner's own pins. Every dialect the two gate files actually use appears here, including
    /// the two that are easiest to misread: a comment quoting the command, and a `-p`-scoped run.
    #[test]
    fn cargo_test_scanner_reads_every_dialect_the_gate_files_use() {
        let text = concat!(
            "# prose naming `cargo test --workspace --lib` is not a command\n",
            "      - 'FLUX_PYTHON=\"{{.P}}\" scripts/run-python3.sh own.py shared",
            " --workspace-root \"{{.D}}\" -- cargo test --workspace --lib'\n",
            "        'cargo fetch --locked && cargo test --workspace --lib",
            " && cargo install --path crates/flux-cli --force'\n",
            "      - 'set \"P=1\" && scripts\\run-python3.cmd own.py shared",
            " -- cargo test --workspace --lib'\n",
            "gate owned_cargo test --workspace\n",
            "gate owned_cargo test -p flux-codegate\n",
            "run: cargo test --workspace --all-targets -- --nocapture\n",
        );

        let found = cargo_test_invocations(text);
        let shape = found
            .iter()
            .map(|invocation| (invocation.workspace, invocation.selectors.join(" ")))
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            vec![
                (true, "--lib".to_string()),
                (true, "--lib".to_string()),
                (true, "--lib".to_string()),
                (true, String::new()),
                (false, String::new()),
                (true, "--all-targets".to_string()),
            ],
            "the comment must be skipped, `cargo fetch`/`cargo install` must not be read as tests, \
             `--` must hand off to the harness, and `-p` must not look workspace-scoped"
        );

        // An unfiltered or `--all-targets` run reaches every kind; `--lib` reaches only libraries.
        assert!(found[3].selects("--lib") && found[3].selects("--bins"));
        assert!(found[5].selects("--lib") && found[5].selects("--bins"));
        assert!(found[0].selects("--lib") && !found[0].selects("--bins"));
    }

    /// C-664. `cargo test --workspace --lib` runs **nothing at all** in a crate with no library
    /// target. `--lib` is a filter, and at workspace scope a filter that matches no target in a
    /// package is not an error: `cargo test -p flux-cli --lib` fails loudly with "no library targets
    /// found in package `flux-cli`", while the same filter across the workspace simply skips it.
    /// That is how `flux-cli` — the whole board and fleet implementation, and hundreds of its unit
    /// tests — sat outside `task test` and `task install` without a single red line.
    ///
    /// So the invariant is stated the other way round from the command: for every workspace crate
    /// whose own target sources carry test functions, the declared gate must select a target kind
    /// that can run them. A future binary-only crate then fails here on the day it is added, instead
    /// of being quietly untested until someone remembers `--bins`.
    ///
    /// Two kinds of target are deliberately out of scope, each because another gate owns it, and
    /// each pinned so the handoff cannot rot:
    ///
    /// - **Integration tests** (`tests/`). The Taskfile gate is the fast one. `cargo test --workspace`
    ///   in `scripts/release-full-gate.sh` runs them, and that invocation is read here and asserted
    ///   to carry no target filter at all.
    /// - **Targets behind `required-features`.** No target-selection flag reaches them, so requiring
    ///   one would be a false claim; see [`unit_test_targets`] and C-308's
    ///   `scripts/check-feature-gated-tests.sh`.
    #[test]
    fn declared_test_gate_reaches_every_workspace_crate_that_carries_tests() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        let mut invocations: Vec<(&str, CargoTestInvocation)> = Vec::new();
        for gate_file in DECLARED_GATE_FILES {
            let text = std::fs::read_to_string(repo_root.join(gate_file))
                .unwrap_or_else(|error| panic!("read declared gate file {gate_file}: {error}"));
            invocations.extend(
                cargo_test_invocations(&text)
                    .into_iter()
                    .filter(|invocation| invocation.workspace)
                    .map(|invocation| (*gate_file, invocation)),
            );
        }

        // Anti-vacuity, and a reformatting guard: this scanner is line-based, so a gate command
        // folded across lines would parse as a selector-free invocation and pass while running
        // nothing of the sort. Pin the count instead — four Taskfile declarations (POSIX and
        // Windows, `task test` and `task install`) and one release-gate run.
        let taskfile_gates = invocations
            .iter()
            .filter(|(file, _)| *file == "Taskfile.yaml")
            .count();
        assert_eq!(
            taskfile_gates, 4,
            "expected the Taskfile to declare the workspace test gate 4 times (POSIX/Windows × \
             test/install), found {taskfile_gates} — if a task was added or the command was \
             reformatted across lines, update this count in the same diff, because a scanner that \
             stops finding the command makes everything below vacuous"
        );
        let release_gates = invocations
            .iter()
            .filter(|(file, _)| *file == "scripts/release-full-gate.sh")
            .collect::<Vec<_>>();
        assert_eq!(
            release_gates.len(),
            1,
            "expected exactly one workspace-wide `cargo test` in the release gate, found {}",
            release_gates.len()
        );
        assert!(
            release_gates[0].1.selectors.is_empty(),
            "scripts/release-full-gate.sh must run the UNFILTERED workspace suite: integration-test \
             targets are covered nowhere else, and the Taskfile's gate is deliberately narrower — \
             found selectors {:?}",
            release_gates[0].1.selectors
        );

        let metadata = MetadataCommand::new()
            .manifest_path(repo_root.join("Cargo.toml"))
            .other_options(vec!["--locked".into(), "--offline".into()])
            .exec()
            .expect("resolve root workspace metadata");

        // Which crate/target actually needs which selector, counted over the target's own module
        // tree so a binary is never credited with a sibling library's tests.
        let mut requirements: Vec<(String, &'static str, String, String, usize)> = Vec::new();
        for package in metadata.workspace_packages() {
            for (selector, target_name, entry) in unit_test_targets(package) {
                let tests: usize = target_module_sources(&entry)
                    .iter()
                    .map(|file| {
                        let source = std::fs::read_to_string(file).unwrap();
                        test_function_names(&source)
                            .unwrap_or_else(|error| {
                                panic!("parse {} for the gate census: {error}", file.display())
                            })
                            .len()
                    })
                    .sum();
                if tests == 0 {
                    continue;
                }
                let relative = entry
                    .strip_prefix(repo_root)
                    .unwrap_or(&entry)
                    .to_string_lossy()
                    .replace('\\', "/");
                requirements.push((
                    package.name.to_string(),
                    selector,
                    target_name,
                    relative,
                    tests,
                ));
            }
        }

        assert!(
            requirements.len() > 20,
            "the gate census found only {} test-carrying targets — the workspace scan is broken",
            requirements.len()
        );
        assert!(
            requirements
                .iter()
                .any(|(_, selector, ..)| *selector == "--bins"),
            "no default-feature binary target in the workspace carries a unit test, which is the \
             exact thing C-664 is about — `flux-cli`'s bin is the one that must be here, so either \
             this census has stopped seeing it or the board and fleet tests moved out of the \
             binary, and the gate command above should shrink back in the same diff"
        );

        let mut violations = Vec::new();
        for (gate_file, invocation) in &invocations {
            for (package, selector, target, root, tests) in &requirements {
                if invocation.selects(selector) {
                    continue;
                }
                let kind = if *selector == "--lib" {
                    "library"
                } else {
                    "bin"
                };
                violations.push(format!(
                    "{gate_file}: `cargo test --workspace {}` does not select `{selector}`, so the \
                     {tests} test(s) in {package}'s {kind} target `{target}` ({root}) never run",
                    invocation.selectors.join(" ")
                ));
            }
        }
        violations.sort();
        violations.dedup();

        assert!(
            violations.is_empty(),
            "the declared workspace test gate leaves test-carrying crates behind (C-664) — \
             `--lib` matches nothing in a crate with no library target and reports no error:\n  {}",
            violations.join("\n  ")
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
    fn direct_io_scanner_resolves_imports_aliases_and_all_io_families() {
        let fixture = r#"
use std::fs as disk;
use std::process::Command as Process;
use std::net::TcpStream as Tcp;
use std::net::TcpListener as Listener;
use reqwest::Client as Http;
use reqwest::blocking::Client as BlockingHttp;
use rusqlite::Connection as Db;
type Database = Db;
fn opens() {
    disk::read_to_string("file");
    Process::new("program");
    Tcp::connect("127.0.0.1:1");
    Listener::bind("127.0.0.1:1");
    Http::builder();
    BlockingHttp::new();
    Database::open("db");
}
#[cfg(test)]
fn fixture_only() { std::fs::write("ignored", "x"); }
"#;
        let hits = raw_direct_io_calls(fixture).unwrap();
        assert_eq!(hits.len(), 7, "{hits:?}");
        assert_eq!(
            hits.iter().map(|hit| hit.api).collect::<BTreeSet<_>>(),
            BTreeSet::from([
                DirectIoApi::Filesystem,
                DirectIoApi::Process,
                DirectIoApi::Socket,
                DirectIoApi::Http,
                DirectIoApi::Database,
            ])
        );
    }

    #[test]
    fn direct_io_scanner_resolves_local_callable_aliases_for_all_io_families() {
        let fixture = r#"
fn opens() {
    let initial_read = std::fs::read_to_string;
    let read = initial_read;
    let process = std::process::Command::new;
    let connect = std::net::TcpStream::connect;
    let http = reqwest::Client::builder;
    let database = rusqlite::Connection::open;
    read("file");
    process("program");
    connect("127.0.0.1:1");
    http();
    database("db");
}
"#;
        let hits = raw_direct_io_calls(fixture).unwrap();
        assert_eq!(hits.len(), 5, "{hits:?}");
        assert_eq!(
            hits.iter().map(|hit| hit.api).collect::<BTreeSet<_>>(),
            BTreeSet::from([
                DirectIoApi::Filesystem,
                DirectIoApi::Process,
                DirectIoApi::Socket,
                DirectIoApi::Http,
                DirectIoApi::Database,
            ])
        );
        let process_hits = raw_process_commands(fixture).unwrap();
        assert_eq!(process_hits.len(), 1, "{process_hits:?}");
        assert_eq!(process_hits[0].api, ProcessApi::Std);

        let shadowed = r#"
use std::fs::read;
fn harmless(_: &str) {}
fn no_open() {
    let read = harmless;
    read("not-io");
}
"#;
        assert!(raw_direct_io_calls(shadowed).unwrap().is_empty());
    }

    #[test]
    fn direct_io_allowance_requires_a_real_reason_immediately_above_the_call() {
        let valid = "fn f() {\n  // flux-allow-direct-io: reviewed host store\n  std::fs::read(\"x\");\n}\n";
        let hit = raw_direct_io_calls(valid).unwrap().pop().unwrap();
        assert_eq!(
            direct_io_allow_reason(valid, hit.line).as_deref(),
            Some("reviewed host store")
        );

        let empty = "fn f() {\n  // flux-allow-direct-io:\n  std::fs::read(\"x\");\n}\n";
        let hit = raw_direct_io_calls(empty).unwrap().pop().unwrap();
        assert!(direct_io_allow_reason(empty, hit.line).is_none());
    }

    #[test]
    fn direct_io_scanner_resolves_known_io_glob_imports() {
        let fixture = r#"
use std::fs::*;
use reqwest::*;
fn opens() {
    read_to_string("file");
    Client::builder();
}
"#;
        let hits = raw_direct_io_calls(fixture).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].api, DirectIoApi::Filesystem);
        assert_eq!(hits[1].api, DirectIoApi::Http);
    }

    /// The production model-facing packs have one syntax-aware no-direct-I/O gate. Reviewed host
    /// stores and broker implementations stay visible through a reason directly above the call;
    /// every allowance is call-local, so a second call fails independently.
    #[test]
    fn no_unreviewed_direct_io_in_model_facing_operation_crates() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let mut violations = Vec::new();
        let mut scanned = 0usize;

        for crate_name in MODEL_FACING_OPERATION_CRATES {
            let src_dir = repo_root.join("crates").join(crate_name).join("src");
            assert!(
                src_dir.is_dir(),
                "classified operation crate missing: {}",
                src_dir.display()
            );
            let mut files = Vec::new();
            collect_rs(&src_dir, &mut files);
            assert!(
                !files.is_empty(),
                "classified operation crate has no Rust sources: {crate_name}"
            );
            for file in files {
                scanned += 1;
                let relative_path = file
                    .strip_prefix(repo_root)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .replace('\\', "/");
                if EXTERNAL_CFG_TEST_MODULES.contains(&relative_path.as_str()) {
                    let parent = std::fs::read_to_string(file.parent().unwrap().join("mod.rs"))
                        .expect("external test module parent");
                    assert!(
                        parent.contains("#[cfg(test)]\nmod tests;"),
                        "{relative_path} is classified test-only but its cfg(test) parent declaration drifted"
                    );
                    continue;
                }
                let source = std::fs::read_to_string(&file).unwrap();
                for hit in raw_direct_io_calls(&source).unwrap_or_else(|error| {
                    panic!("parse {} for direct-I/O gate: {error}", file.display())
                }) {
                    if direct_io_allow_reason(&source, hit.line).is_none() {
                        violations.push(format!(
                            "{relative_path}:{}: {:?} open in {} has no reasoned flux-allow-direct-io annotation",
                            hit.line, hit.api, hit.function
                        ));
                    }
                }
            }
        }

        assert!(
            scanned > 50,
            "model-facing classification scanned only {scanned} files"
        );
        assert!(
            violations.is_empty(),
            "direct I/O outside flux-system in model-facing operation crates:\n  {}",
            violations.join("\n  ")
        );
    }

    /// C-435: production listeners are guarded resources, not adapter-owned sockets. The native
    /// `TcpListener::bind` constructor exists once, behind `GuardedNetwork::bind_tcp`; every served
    /// surface consumes that port. Obvious raw/socket2 constructors and macro-contained calls are
    /// included as bypass shapes. This is deliberately a source census rather than a grep, so test
    /// doubles and comments do not count and renamed imports cannot hide a second bind.
    #[test]
    fn every_production_tcp_listener_bind_is_inside_flux_system() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let mut found = Vec::new();
        let mut locations = Vec::new();

        for file in workspace_source_files(repo_root) {
            let relative = file
                .strip_prefix(repo_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&file).unwrap();
            for hit in raw_direct_io_calls(&source).unwrap_or_else(|error| {
                panic!("parse {} for listener census: {error}", file.display())
            }) {
                if is_obvious_native_listener_constructor(&hit.path) {
                    locations.push(format!("{}:{} ({})", relative, hit.line, hit.function));
                    found.push((relative.clone(), hit.function));
                }
            }
        }

        // Filesystem traversal order differs across runners; the census is exact, but its
        // comparison and diagnostic must be stable everywhere.
        found.sort();
        locations.sort();

        assert_eq!(
            found,
            vec![
                // Single-use OAuth callbacks are finite loopback protocol handshakes, not served
                // agent surfaces. They close after one code and cannot use the long-lived HTTP
                // listener adapter without changing their protocol lifecycle.
                (
                    "crates/flux-cli/src/auth_cmd.rs".to_string(),
                    "wait_for_codex_callback".to_string()
                ),
                (
                    "crates/flux-cli/src/auth_cmd.rs".to_string(),
                    "wait_for_oauth_callback".to_string()
                ),
                // Public documentation may intentionally be unauthenticated off-loopback and has
                // no execution routes. GuardedNetwork truthfully permits only loopback-open or
                // authenticated public listeners, so this static-only server remains explicit.
                (
                    "crates/flux-server/src/public_docs.rs".to_string(),
                    "serve".to_string()
                ),
                // The sole native constructor implementing GuardedNetwork::bind_tcp.
                (
                    "crates/flux-system/src/net.rs".to_string(),
                    "bind_tcp".to_string()
                ),
                // The native guarded network implementation's reviewed raw-ICMP constructor. A
                // raw socket is not a served listener, but the broad native-socket mutation gate
                // intentionally keeps every such constructor visible here.
                (
                    "crates/flux-system/src/net.rs".to_string(),
                    "open".to_string()
                ),
            ],
            "a production listener bypasses GuardedNetwork::bind_tcp; move the bind behind the \
             selected ExecutionSystem or add a narrowly reasoned single-use disposition. The exact \
             census is a mutation guard: additions and removals both require review: {locations:?}"
        );
    }

    #[test]
    fn listener_census_resolves_renamed_imports() {
        let fixture = r#"
use tokio::net::TcpListener as Listener;
fn serve() { Listener::bind("127.0.0.1:0"); }
"#;
        let hits = raw_direct_io_calls(fixture).unwrap();
        assert!(hits.iter().any(|hit| {
            hit.path
                == ["tokio", "net", "TcpListener", "bind"]
                    .map(str::to_string)
                    .to_vec()
        }));
    }

    #[test]
    fn listener_census_sees_macros_and_obvious_alternative_socket_constructors() {
        let fixture = r#"
macro_rules! raw_listener {
    () => { tokio::net::TcpListener::bind("127.0.0.1:0") };
}
fn alternatives() {
    socket2::Socket::new(domain, kind, protocol);
    libc::socket(domain, kind, protocol);
}
"#;
        let paths = raw_direct_io_calls(fixture)
            .unwrap()
            .into_iter()
            .map(|hit| hit.path.join("::"))
            .collect::<BTreeSet<_>>();
        assert!(paths.contains("tokio::net::TcpListener::bind"), "{paths:?}");
        assert!(paths.contains("socket2::Socket::new"), "{paths:?}");
        assert!(paths.contains("libc::socket"), "{paths:?}");
    }

    #[test]
    fn production_native_listener_types_are_confined_to_the_native_port() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let mut violations = Vec::new();
        let mut native_port_mentions = 0usize;

        for file in workspace_source_files(repo_root) {
            let relative = file
                .strip_prefix(repo_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&file).unwrap();
            for line in native_tcp_listener_type_lines(&source).unwrap_or_else(|error| {
                panic!(
                    "parse {} for native-listener type census: {error}",
                    file.display()
                )
            }) {
                if relative == "crates/flux-system/src/net.rs" {
                    native_port_mentions += 1;
                } else {
                    violations.push(format!("{relative}:{line}"));
                }
            }
        }

        assert!(
            native_port_mentions > 0,
            "native listener implementation disappeared"
        );
        assert!(
            violations.is_empty(),
            "a production API or implementation names an already-bound native TcpListener outside \
             flux-system; accept NetworkListener/GuardedNetwork instead: {violations:?}"
        );
    }

    #[test]
    fn native_listener_type_census_resolves_aliases_and_fully_qualified_types() {
        let fixture = r#"
use tokio::net::TcpListener as Listener;
pub fn aliased(listener: Listener) {}
pub fn qualified(listener: std::net::TcpListener) {}
#[cfg(test)]
pub fn test_only(listener: tokio::net::TcpListener) {}
"#;
        assert_eq!(native_tcp_listener_type_lines(fixture).unwrap().len(), 2);
    }

    #[test]
    fn port_impl_scanner_finds_production_backends_and_ignores_test_doubles() {
        let raw = r#"
use flux_system::port::{GuardedEnv, GuardedProcess};

impl GuardedProcess for MySubstrate {}
impl flux_system::port::GuardedEnv for MySubstrate {}
impl SomeOtherTrait for MySubstrate {}

#[cfg(test)]
mod tests {
    impl GuardedProcess for Double {}
}

#[cfg(test)]
impl GuardedEnv for AnotherDouble {}
"#;
        let hits = guarded_port_impls(raw).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(
            hits.iter()
                .all(|hit| hit.backend == "MySubstrate" && hit.port.starts_with("Guarded")),
            "only the production backends may be reported: {hits:?}"
        );
        assert!(
            hits.iter().any(|hit| hit.port == "GuardedProcess"),
            "a fully-qualified trait path must resolve by its last segment: {hits:?}"
        );

        // A blanket impl is a backend claim over every type, so it must be visible, not dropped.
        let blanket = "impl<T: GuardedProcess> GuardedProcess for &T {}\n";
        let hits = guarded_port_impls(blanket).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].backend, "<generic>");
        assert_eq!(hits[0].spelled_as, None);
    }

    /// C-467's failing-first fixture: workspace confinement is a guarded-IO backend claim too.
    #[test]
    fn port_impl_scanner_rejects_an_unreviewed_workspace_backend() {
        let raw = r#"
use flux_system::port::GuardedWorkspaceFiles;

impl GuardedWorkspaceFiles for Rogue {}
"#;
        let hits = guarded_port_impls(raw).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "an unreviewed workspace backend escaped: {hits:?}"
        );
        assert_eq!(hits[0].port, "GuardedWorkspaceFiles");
        assert_eq!(hits[0].backend, "Rogue");
    }

    /// C-467's drift pin: the scanner's hand-maintained list must equal the public guarded traits
    /// declared by `port.rs`, so a fifth port cannot recreate this blind spot.
    #[test]
    fn guarded_port_trait_census_matches_port_rs() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let port_rs = crates_dir.join("flux-system/src/port.rs");
        let source = std::fs::read_to_string(&port_rs).unwrap();
        let file = syn::parse_file(&source).unwrap();
        let declared: BTreeSet<String> = file
            .items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Trait(item)
                    if matches!(item.vis, syn::Visibility::Public(_))
                        && item.ident.to_string().starts_with("Guarded") =>
                {
                    Some(item.ident.to_string())
                }
                _ => None,
            })
            .collect();
        let guarded: BTreeSet<String> = GUARDED_PORT_TRAITS
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(
            guarded, declared,
            "guarded-port census drifted from {port_rs:?}"
        );
    }

    /// A renamed import must not launder a backend past the gate. This is the exact evasion
    /// [`ProcessAliases`] already defends `no_raw_process_command_outside_system` against
    /// (`use std::process::Command as Exec`), so the port gate has to match its sibling — otherwise
    /// the newer, security-relevant gate is the weaker of the two.
    #[test]
    fn port_impl_scanner_resolves_renamed_trait_imports() {
        // The shape the reviewer used to walk straight through the first cut of this gate.
        let renamed = r#"
use flux_system::port::GuardedProcess as Exec;

impl Exec for Rogue {}
"#;
        let hits = guarded_port_impls(renamed).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "a renamed port trait must still be seen: {hits:?}"
        );
        assert_eq!(
            hits[0].port, "GuardedProcess",
            "the hit must carry the CANONICAL name so an allowance cannot be dodged by renaming"
        );
        assert_eq!(hits[0].backend, "Rogue");
        assert_eq!(
            hits[0].spelled_as.as_deref(),
            Some("Exec"),
            "the local spelling belongs in the diagnostic"
        );

        // A grouped rename, a module rename, and a rename *chain* — the chain also proves resolution
        // is order-insensitive, since `Hop` is defined by a later `use` than the one consuming it.
        let harder = r#"
        use flux_system::port::{GuardedEnv as Env, GuardedHostFiles, GuardedWorkspaceFiles as Workspace};
use flux_system::port as p;
use Hop as Chained;
use flux_system::port::GuardedProcess as Hop;

impl Env for A {}
impl GuardedHostFiles for B {}
        impl p::GuardedEnv for C {}
        impl Chained for D {}
        impl Workspace for E {}
"#;
        let hits = guarded_port_impls(harder).unwrap();
        let mut resolved: Vec<(&str, &str)> = hits
            .iter()
            .map(|hit| (hit.port.as_str(), hit.backend.as_str()))
            .collect();
        resolved.sort_unstable();
        assert_eq!(
            resolved,
            vec![
                ("GuardedEnv", "A"),
                ("GuardedEnv", "C"),
                ("GuardedHostFiles", "B"),
                ("GuardedProcess", "D"),
                ("GuardedWorkspaceFiles", "E"),
            ],
            "every spelling must resolve to its canonical port: {hits:?}"
        );

        // A rename that has nothing to do with the ports must not be dragged in.
        let unrelated = r#"
use std::fmt::Display as Show;

impl Show for Harmless {}
"#;
        assert!(guarded_port_impls(unrelated).unwrap().is_empty());
    }

    /// `#[cfg(test)]` is the *only* configuration this gate excuses. A `#[cfg(feature = "…")]` backend
    /// ships to users, so it is production code and must be reported — including behind an alias.
    #[test]
    fn port_impl_scanner_excuses_only_cfg_test_not_other_cfgs() {
        let src = r#"
use flux_system::port::GuardedProcess as Exec;

#[cfg(feature = "wasm")]
impl GuardedProcess for WasmSubstrate {}

#[cfg(all(unix, feature = "remote"))]
impl Exec for RemoteSubstrate {}

#[cfg(test)]
impl Exec for Double {}
"#;
        let hits = guarded_port_impls(src).unwrap();
        let mut backends: Vec<&str> = hits.iter().map(|hit| hit.backend.as_str()).collect();
        backends.sort_unstable();
        assert_eq!(
            backends,
            vec!["RemoteSubstrate", "WasmSubstrate"],
            "feature-gated backends ship and must be gated; only #[cfg(test)] is excused: {hits:?}"
        );
        assert!(
            hits.iter().all(|hit| hit.port == "GuardedProcess"),
            "{hits:?}"
        );
    }

    /// Architecture guard (C-269): the guarded-IO port made "being a `System`" substitutable, so the
    /// set of types that claim to *be* a guarded process/host-file/env backend must stay enumerated
    /// exactly the way [`no_raw_process_command_outside_system`] enumerates raw command constructions.
    ///
    /// Without this the trait is a blind spot the older lints cannot see: they bound *syscall
    /// construction*, never *the semantics of a guard*, so a second production backend could satisfy
    /// `GuardedProcess` while enforcing none of the argv-only / pinned-cwd / cleared-env / capped-output
    /// guarantees, and no gate would notice. Allowances are single-use, so a second impl of the same
    /// port on the same type in the same file fails independently.
    #[test]
    fn no_unreviewed_guarded_port_backend_outside_system() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        // The reviewed backends: `flux-system`'s native delegations, and the two delegating backends
        // C-399 added beside them. A new entry here is a security review, not a formality — read
        // `crates/flux-system/src/port.rs` first.
        //
        // Why `remote.rs`'s entries are reviewable rather than alarming: `RemoteSystem` and
        // `Loopback` hold no workspace, open no file and start no process. Each operation is a
        // hand-off to a `Delegate`, so neither type can add a permission or remove one — the
        // guarantees remain whatever the far side already enforced. What they own is the *failure
        // semantics* of delegation (a guard's refusal, an unreachable delegate and an unserved
        // operation stay three distinguishable errors), and every operation the delegate does not
        // serve denies in `port.rs`'s own words. This allowance is the cost C-399's ownership
        // decision accepted on purpose: the alternative was an out-of-repo backend this gate cannot
        // see at all.
        const ALLOW: &[(&str, &str, &str)] = &[
            ("crates/flux-system/src/port.rs", "GuardedProcess", "System"),
            (
                "crates/flux-system/src/port.rs",
                "GuardedHostFiles",
                "System",
            ),
            (
                "crates/flux-system/src/port.rs",
                "GuardedWorkspaceFiles",
                "System",
            ),
            ("crates/flux-system/src/port.rs", "GuardedEnv", "System"),
            ("crates/flux-system/src/port.rs", "GuardedNetwork", "System"),
            // C-653. Read-only and measurement-only: the native metrics backend opens no file
            // outside the roots it is given, starts no process, and writes nothing.
            //
            // Which roots those are is the reviewable part, and C-673 narrowed the claim to what
            // the code actually enforces. `MetricsRoots::pinned` accepts *any* pair of paths —
            // nothing type-level keeps it inside `/proc` and `/sys`, and the earlier note claiming
            // a caller could only ever narrow them was describing a containment nothing enforces.
            // What is true is narrower: every production entry point constructs
            // `MetricsRoots::native()`, and the one setter that can replace them,
            // `System::with_metrics_roots`, has no caller anywhere outside a test — no operation,
            // no CLI flag and no wire field reaches it.
            //
            // The second half is checked rather than asserted:
            // `the_metrics_roots_allowance_note_states_only_what_the_code_enforces` runs
            // `metrics_roots_setter_calls` over every workspace source file, structurally, scoped
            // on the `#[cfg(test)]` attribute the way `TestScope::CfgTestItems` documents. That
            // scoping is the whole difference between a check and a claim: this guard's first cut
            // scanned from the first `#[cfg(test)]` marker to end of file, which reads the
            // production half of 63 source files as test code — including `flux-system/src/lib.rs`,
            // where the setter itself lives. What no check covers is the *first* half: nothing
            // stops a future entry point from calling `MetricsRoots::pinned` directly, and a
            // reviewer adding one has to come here.
            //
            // What the review is actually about is the *answer* shape — an unsupported metric must
            // stay explicitly unavailable rather than becoming a zero a projection would read as a
            // measurement, and (C-673) a cap that drops mounts must say so in the answer rather
            // than report a truncated list as a complete one.
            ("crates/flux-system/src/port.rs", "GuardedMetrics", "System"),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedProcess",
                "RemoteSystem",
            ),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedHostFiles",
                "RemoteSystem",
            ),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedWorkspaceFiles",
                "RemoteSystem",
            ),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedEnv",
                "RemoteSystem",
            ),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedNetwork",
                "RemoteSystem",
            ),
            // C-654: metrics now delegate, under the protocol version bump C-653 said they would
            // need. Reviewable on the same grounds as the families above — the backend measures
            // nothing itself, it asks the far side and relays the answer — plus two obligations
            // this family alone carries, both of which are about *not trusting the reporter*:
            //
            // - The reading is re-bounded through `metrics::bounded_reading` on the way out. The
            //   caps on labels, mounts and sensors are a construction-site convention over public
            //   fields, not a type invariant, so a delegate could otherwise hand back a reading no
            //   local reader would produce. The HTTPS decoder bounds too; that is depth, not a
            //   substitute, because `Delegate` is implementable by anyone.
            // - `remotely_reported` is stamped here rather than read off the answer. A far side
            //   that could set it to `false` would be asserting that this process observed the
            //   number — the exact substitution `SubstrateIdentity` exists to prevent.
            //
            // What must not drift: the two negatives stay distinct (`Unserved` = serves no metrics
            // at all; `Unavailable` = serves the family, no such instrument), and neither ever
            // becomes a zero.
            (
                "crates/flux-system/src/remote.rs",
                "GuardedMetrics",
                "RemoteSystem",
            ),
            // C-652 — the HTTP family. Two of these three send nothing of their own and the third
            // is the workspace's one HTTP client, which is why the family adds no new IO path.
            //
            // `System`'s impl constructs nothing: `flux-system` holds no HTTP client by design, so
            // the native backend serves the backend a composition site attached to it (C-675's
            // `System::with_http`, one call on an `Arc<dyn GuardedHttp>` it was handed) and answers
            // the port's fail-closed default when nobody attached one — which is every `System`
            // this crate constructs. The client count is unaffected: attaching decides who may ask
            // an existing backend, never who builds one.
            //
            // `RemoteSystem`'s impl **delegates** since C-674, where C-652 left it answering a typed
            // `Unserved` naming the missing wire. It still constructs no client and opens no socket:
            // it hands the request to a `Delegate` and the far side makes it, which is the same
            // shape as the four families above. What it adds — and what to re-read on any change —
            // is the pair of obligations the metrics entry states, applied here:
            //
            // - The answer is re-bounded through `port::bounded_response` against the *request's*
            //   own `max_response_bytes`, plus the header and admit-list caps, because the caps on
            //   `HttpResponse` are the substrate's promise and `Delegate` is implementable by
            //   anyone. The HTTPS decoder bounds too; that is depth, not a substitute.
            // - `PrivateAdmit::substrate` is stamped here rather than read off the answer. A far
            //   side that could set it to `None` would be asserting that this process admitted the
            //   private destination — the substitution `SubstrateIdentity` exists to prevent.
            //
            // What must not drift: the far side owns admission for every hop (it re-runs the egress
            // guard on the URL it was handed rather than trusting the pins this process vetted), the
            // per-hop secret re-authorization, the redirect bound and the byte cap; and a delegate
            // that does not serve the family still answers `Unserved` without a request rather than
            // falling back to sending from the coordinator's own process.
            ("crates/flux-system/src/port.rs", "GuardedHttp", "System"),
            (
                "crates/flux-system/src/remote.rs",
                "GuardedHttp",
                "RemoteSystem",
            ),
            // The one that actually performs HTTP, and the reason the family exists. `NativeHttp`
            // adds no client: it wraps the redirect-disabled, proxy-free `reqwest::Client` that
            // `flux-web`'s reviewed egress broker already owned, and every request still goes
            // through `flux_system::net`'s admission, C-77's connection pin, the bounded redirect
            // chain and the response-byte cap. What is new is only *who may ask* — so this reads as
            // one reviewed backend on the existing egress path rather than a second path.
            //
            // It lives in `flux-web` (L5) rather than beside the port (L2) because that is where the
            // client is, and `flux-codegate`'s `Http` census keeps the count of clients at one. The
            // layer map forbids the alternative outright.
            (
                "crates/flux-web/src/native_http.rs",
                "GuardedHttp",
                "NativeHttp",
            ),
            // C-651's confinement peer. Reviewable on the same grounds as `remote.rs`'s entries,
            // and on one narrower one: `SandboxedSystem` holds a native `System` and every served
            // operation forwards straight back to that system — to its inherent guarded method
            // where it has one (process, files, env), and to its own `port.rs` trait impl for the
            // families that live there (network, metrics). Either way the callee is the reviewed
            // native backend, so the peer can neither add a permission nor remove one: the
            // guarantees stay exactly the ones `System` enforces, including the single
            // `build_command` spawn choke point the sandbox wraps.
            //
            // What it owns is *admission*, and that is the part to re-read on any change. It
            // refuses to exist unless the composed `Sandbox` confines this process's own spawns,
            // and it will not revive a bare `FLUX_SANDBOXED` marker that the ambient posture left
            // inert — the marker is trusted only where `apply_sandbox_env` already trusted and
            // disclosed it. A future edit that widened this type beyond delegation, or softened
            // that admission, would be a new backend wearing this allowance — read
            // `crates/flux-system/src/sandboxed.rs` before extending it.
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedProcess",
                "SandboxedSystem",
            ),
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedHostFiles",
                "SandboxedSystem",
            ),
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedWorkspaceFiles",
                "SandboxedSystem",
            ),
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedEnv",
                "SandboxedSystem",
            ),
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedNetwork",
                "SandboxedSystem",
            ),
            // Metrics delegate rather than deny (C-653): the peer confines what this process
            // *spawns*, and a metric read happens in this process against this machine — the same
            // machine the composed `System` measures, through the same narrowable `/proc`+`/sys`
            // roots. An `Unserved` here would be a false negative about a host flux can genuinely
            // measure.
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedMetrics",
                "SandboxedSystem",
            ),
            // HTTP (C-675) is one delegating call and nothing else:
            // `GuardedHttp::http_request(&self.inner, request, allow)`, forwarding to the composed
            // `System` exactly as the network and metrics families above do. It adds no IO path
            // because it opens nothing and builds nothing — the callee is the native backend, which
            // itself only serves a backend a composition site attached to it (`System::with_http`),
            // and answers the port's `Unserved` when none was. So a peer composed over an
            // unattached system still refuses by name (pinned by test), and one composed by
            // `flux-cli` over the system carrying `flux_web::NativeHttp` serves web effects through
            // the same reviewed egress client, guard and audit sink an unselected run uses.
            //
            // C-652 left this impl empty for the reason that still holds: the native client is at
            // L5 and this L2 type may not reach it, so inventing a client here would be the second
            // egress path the `Http` census exists to prevent. Delegation is how the peer gets one
            // without reaching upward — an edit that constructed a client inside this allowance
            // would be exactly the thing that reasoning forbids.
            (
                "crates/flux-system/src/sandboxed.rs",
                "GuardedHttp",
                "SandboxedSystem",
            ),
        ];
        let mut allowance_use = vec![0usize; ALLOW.len()];

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
            let hits = guarded_port_impls(&src).unwrap_or_else(|error| {
                panic!("parse {} for guarded-port gate: {error}", file.display())
            });
            for hit in hits {
                if let Some((index, _)) = ALLOW.iter().enumerate().find(|(_, allowed)| {
                    allowed.0 == rel && allowed.1 == hit.port && allowed.2 == hit.backend
                }) {
                    allowance_use[index] += 1;
                    if allowance_use[index] > 1 {
                        violations.push(format!(
                            "{rel}:{}: duplicate use of single-use allowance for {} on {}",
                            hit.line, hit.port, hit.backend
                        ));
                    }
                } else {
                    let via = match &hit.spelled_as {
                        Some(alias) => format!(" (written as `{alias}`)"),
                        None => String::new(),
                    };
                    violations.push(format!(
                        "{rel}:{}: unreviewed guarded-IO backend — {} implemented for {}{via}",
                        hit.line, hit.port, hit.backend
                    ));
                }
            }
        }

        for (index, count) in allowance_use.into_iter().enumerate() {
            if count != 1 {
                violations.push(format!(
                    "reviewed guarded-port allowance {:?} was used {count} times (expected exactly once)",
                    ALLOW[index]
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "guarded-IO port implemented outside the reviewed native backend:\n  {}",
            violations.join("\n  ")
        );
    }

    /// C-716's scanner pins. A live backend is found by the same alias-resolving machinery the
    /// guarded-port census uses, and a backend that constructs its own client is *reported* rather
    /// than left to a reviewer's eye.
    #[test]
    fn live_backend_scanner_finds_production_backends_and_ignores_test_doubles() {
        let raw = r#"
use codewandler_flux_capabilities::LiveDatasource as Live;

impl Live for RogueBackend {}
impl codewandler_flux_capabilities::WorkBoard for Board {}
impl SomeOtherTrait for RogueBackend {}

#[cfg(test)]
impl LiveDatasource for Double {}
"#;
        let hits = live_backend_impls(raw).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(
            hits.iter().any(|hit| hit.port == "LiveDatasource"
                && hit.backend == "RogueBackend"
                && hit.spelled_as.as_deref() == Some("Live")),
            "a renamed contract import must resolve to the canonical name: {hits:?}"
        );
        assert!(
            hits.iter()
                .any(|hit| hit.port == "WorkBoard" && hit.backend == "Board"),
            "a fully-qualified contract path must resolve by its last segment: {hits:?}"
        );
    }

    /// The C-716 census (acceptance 3). **Every production live-datasource / work-board backend is
    /// enumerated here, and none of them constructs its own client.**
    ///
    /// The trait doc has said since the contract shipped that an implementation "must perform real
    /// IO through flux's guarded host surfaces", and a `LiveAccess` declaration names which guarded
    /// surface that is. Nothing checked it: a backend that dialled its own socket or built its own
    /// HTTP client satisfied the trait, advertised exact `network.fetch`/`connection.dial`
    /// authority, and would then ignore a host selection entirely — the connection would leave the
    /// coordinator no matter which substrate the operator selected. That is precisely the class the
    /// guarded-port census exists to make loud, so this is its sibling and not a new idea.
    ///
    /// Two claims, both census-shaped:
    ///
    /// 1. the set of types claiming to *be* a live backend is enumerated, so a new one is a review;
    /// 2. no file declaring one constructs an HTTP client, a database connection or a socket —
    ///    **and unlike [`no_unreviewed_direct_io_in_model_facing_operation_crates`], a
    ///    `flux-allow-direct-io` annotation does not excuse it here.** An annotation answers "is
    ///    this IO reviewed"; the question here is "does this connection follow the selection", and
    ///    no annotation can make a self-built client do that.
    ///
    /// Filesystem calls are deliberately *not* included: a board backed by markdown files reads
    /// them through the workspace guard, and that family is already the direct-I/O gate's job.
    #[test]
    fn no_live_datasource_backend_builds_its_own_client() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        // The reviewed live backends. Adding one is a review of *how it connects*: read
        // `crates/flux-capabilities/src/datasource/live.rs` — specifically `live_connection_system`,
        // which is how a backend reaches the substrate its declared locality names — before
        // extending this list.
        //
        // All three are in-process today: two markdown/in-memory boards and the SDK's hermetic
        // example. None opens a connection at all, which is why the second half of this census
        // currently has nothing to report and why it must stay armed — the first backend that does
        // connect is exactly the one that has to route through the guarded surface it declares.
        const ALLOW: &[(&str, &str, &str)] = &[
            (
                "crates/flux-capabilities/src/datasource/markdown_board.rs",
                "WorkBoard",
                "MarkdownBoard",
            ),
            (
                "crates/flux-capabilities/src/datasource/memory_board.rs",
                "WorkBoard",
                "MemoryBoard",
            ),
            (
                "crates/flux-sdk/examples/support/live_datasource.rs",
                "LiveDatasource",
                "SupportBackend",
            ),
        ];
        let mut allowance_use = vec![0usize; ALLOW.len()];

        let files = live_backend_source_files(repo_root);
        assert!(
            files.len() > 20,
            "expected to scan a representative set of source files, found {}",
            files.len()
        );

        let mut violations = Vec::new();
        let mut backends = 0usize;
        for file in &files {
            let rel = file
                .strip_prefix(repo_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).unwrap();
            let hits = live_backend_impls(&src).unwrap_or_else(|error| {
                panic!("parse {} for the live-backend census: {error}", rel)
            });
            if hits.is_empty() {
                continue;
            }
            backends += hits.len();
            for hit in hits {
                if let Some((index, _)) = ALLOW.iter().enumerate().find(|(_, allowed)| {
                    allowed.0 == rel && allowed.1 == hit.port && allowed.2 == hit.backend
                }) {
                    allowance_use[index] += 1;
                    if allowance_use[index] > 1 {
                        violations.push(format!(
                            "{rel}:{}: duplicate use of single-use allowance for {} on {}",
                            hit.line, hit.port, hit.backend
                        ));
                    }
                } else {
                    let via = match &hit.spelled_as {
                        Some(alias) => format!(" (written as `{alias}`)"),
                        None => String::new(),
                    };
                    violations.push(format!(
                        "{rel}:{}: unreviewed live backend — {} implemented for {}{via}",
                        hit.line, hit.port, hit.backend
                    ));
                }
            }
            for call in raw_direct_io_calls(&src)
                .unwrap_or_else(|error| panic!("parse {rel} for the live-backend census: {error}"))
            {
                if matches!(
                    call.api,
                    DirectIoApi::Http | DirectIoApi::Database | DirectIoApi::Socket
                ) {
                    violations.push(format!(
                        "{rel}:{}: a live backend builds its own {:?} client in {} — the \
                         connection must be made through the guarded surface its `LiveAccess` \
                         declares, so it follows the session's selected host",
                        call.line, call.api, call.function
                    ));
                }
            }
        }

        for (index, count) in allowance_use.into_iter().enumerate() {
            if count != 1 {
                violations.push(format!(
                    "reviewed live-backend allowance {:?} was used {count} times (expected exactly once)",
                    ALLOW[index]
                ));
            }
        }

        // Anti-vacuity: a census that found no backends would pass while pinning nothing.
        assert_eq!(
            backends,
            ALLOW.len(),
            "the live-backend census found {backends} backend(s) — they have moved, been renamed, \
             or the scan roots no longer reach them"
        );
        assert!(
            violations.is_empty(),
            "a live datasource / work board backend does not route through its declared guarded \
             surface (C-716):\n  {}",
            violations.join("\n  ")
        );
    }

    /// The metrics-roots scanner's own pins (C-673 review).
    ///
    /// The first cut of this guard scanned from the first `#[cfg(test)]` marker to EOF and treated
    /// everything below it as test code. That is the heuristic
    /// [`TestScope::CfgTestItems`] exists to refuse: 63 of the 451 scanned files declare an inline
    /// test module near the top and carry production code below it — including
    /// `crates/flux-system/src/lib.rs`, which *defines* the setter at line 1511 while its first
    /// `#[cfg(test)]` sits at line 842, so 4431 lines of the setter's own file went unread. A guard
    /// that cannot see the file it is about is not evidence, and the census note claiming it was
    /// the check that fails is exactly the overclaim acceptance 4 exists to remove.
    ///
    /// So: the early test module must not blind it, the definition and prose must not count, and a
    /// call inside a macro body must.
    #[test]
    fn metrics_roots_setter_scanner_sees_production_calls_below_an_early_test_module() {
        let src = r#"
//! Prose naming `with_metrics_roots` is not a call.
#[cfg(test)]
mod early;

impl System {
    pub fn with_metrics_roots(mut self, roots: MetricsRoots) -> Self {
        self.metrics_roots = roots;
        self
    }
}

fn production(system: System, roots: MetricsRoots) -> System {
    let _ = "with_metrics_roots";
    system.with_metrics_roots(roots)
}

fn ufcs(system: System, roots: MetricsRoots) -> System {
    System::with_metrics_roots(system, roots)
}

fn in_a_macro(system: System, roots: MetricsRoots) {
    assert!(system.with_metrics_roots(roots).is_ok());
}

#[cfg(test)]
mod tests {
    fn fixture(system: System, roots: MetricsRoots) -> System {
        system.with_metrics_roots(roots)
    }
}
"#;
        let hits = metrics_roots_setter_calls(src).unwrap();
        assert_eq!(
            hits.len(),
            3,
            "the method call, the UFCS call and the one inside `assert!` — all of them below an \
             early `#[cfg(test)]`, and none of the definition, the doc comment, the string literal \
             or the real test module: {hits:?}"
        );
        let lines: Vec<&str> = src.lines().collect();
        for line in &hits {
            assert!(
                lines[line - 1].contains("with_metrics_roots"),
                "hit at line {line} does not name the setter: {:?}",
                lines[line - 1]
            );
        }

        // The vacuity floor: a file with no production call must read as none, or the census above
        // passes because the scanner sees nothing anywhere.
        let test_only = r#"
#[cfg(test)]
mod tests {
    fn fixture(system: System, roots: MetricsRoots) -> System {
        system.with_metrics_roots(roots)
    }
}
"#;
        assert!(metrics_roots_setter_calls(test_only).unwrap().is_empty());
    }

    /// C-673: the metrics allowance note must claim only the property the code enforces.
    ///
    /// The note reviewed with C-653 said a caller "can narrow but never widen" the metric roots.
    /// Nothing enforces that: [`MetricsRoots::pinned`] takes any two paths, and `with_metrics_roots`
    /// is a plain setter. The property that *is* true is narrower and grep-provable — every
    /// production entry point takes the native `/proc` + `/sys` roots, and the only caller of the
    /// setter anywhere in the tree is a test. This pins both halves: the wording, and the fact it
    /// rests on.
    #[test]
    fn the_metrics_roots_allowance_note_states_only_what_the_code_enforces() {
        const ENTRY: &str = "\"crates/flux-system/src/port.rs\", \"GuardedMetrics\", \"System\"";
        // The name the census claims about is the one the scanner looks for — two spellings would
        // eventually disagree, and the note would go on describing a check of something else.
        let setter = METRICS_ROOTS_SETTER;

        // The note is the run of comment lines immediately above the allowance entry, so this
        // reads what a reviewer reads rather than the whole file.
        let lines: Vec<&str> = include_str!("lib.rs").lines().collect();
        let entry = lines
            .iter()
            .position(|line| line.contains(ENTRY))
            .expect("the `GuardedMetrics`/`System` allowance entry");
        let start = lines[..entry]
            .iter()
            .rposition(|line| !line.trim_start().starts_with("//"))
            .map_or(0, |last| last + 1);
        let note = lines[start..entry].join("\n");
        assert!(
            !note.contains("never widen"),
            "the metrics allowance still claims a containment nothing enforces — \
             `MetricsRoots::pinned` accepts any pair of paths:\n{note}"
        );
        assert!(
            note.contains(setter),
            "the allowance must name `{setter}`, the fact its narrower claim rests on:\n{note}"
        );

        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let files = workspace_source_files(repo_root);
        assert!(
            files.len() > 20,
            "expected to scan a representative set of source files, found {}",
            files.len()
        );
        let mut callers = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file).unwrap();
            let hits = metrics_roots_setter_calls(&source).unwrap_or_else(|error| {
                panic!(
                    "parse {} for the metrics-roots scan: {error}",
                    file.display()
                )
            });
            let rel = file.strip_prefix(repo_root).unwrap_or(file);
            for line in hits {
                callers.push(format!("{}:{line}", rel.display()));
            }
        }
        assert!(
            callers.is_empty(),
            "the allowance says only tests reach `{setter}`, but production code calls it:\n  {}",
            callers.join("\n  ")
        );
    }

    /// C-651: the **confinement peer** is a reviewed guarded-IO backend, and a complete one.
    ///
    /// Decision 0018 rule 3 makes `sandboxed` a selectable peer rather than only a spawn-time
    /// modifier, which means a second type inside `flux-system` now claims to *be* a guarded
    /// substrate. Two things have to hold, and neither is implied by the other:
    ///
    /// - It serves **every** port `ExecutionSystem` bundles. A peer that implemented four of the
    ///   five would not be an `ExecutionSystem` at all, and the gap would show up as a confusing
    ///   trait-bound error at a distant call site rather than here.
    /// - Each of those impls is in the reviewed ALLOW of
    ///   [`no_unreviewed_guarded_port_backend_outside_system`], which is what makes the claim a
    ///   review rather than a formality. That test enforces the allowance half; this one enforces
    ///   that the backend it allows actually exists and is whole.
    #[test]
    fn the_confinement_peer_backend_is_reviewed_and_complete() {
        // The `ExecutionSystem` constituent ports as of C-651, plus C-653's `GuardedMetrics` and
        // C-652's `GuardedHttp`. Deliberately spelled out rather than read from
        // `GUARDED_PORT_TRAITS`: a port that joins the guarded set but not the execution bundle
        // would otherwise be demanded of this backend by drift alone.
        //
        // Implementing a port is not the same as serving it: every impl here delegates to the
        // composed `System`, and what that answers for `GuardedHttp` depends on whether a
        // composition site attached a backend (C-675). The claim under test is that the peer *is* a
        // complete `ExecutionSystem`, which is what makes an omission a compile error at a distant
        // call site rather than a silent gap here.
        const EXECUTION_PORTS: &[&str] = &[
            "GuardedEnv",
            "GuardedHostFiles",
            "GuardedHttp",
            "GuardedMetrics",
            "GuardedNetwork",
            "GuardedProcess",
            "GuardedWorkspaceFiles",
        ];
        const PEER: &str = "SandboxedSystem";

        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let peer_rs = crates_dir.join("flux-system/src/sandboxed.rs");
        let source = std::fs::read_to_string(&peer_rs).unwrap_or_else(|error| {
            panic!(
                "the `sandboxed` peer backend must live at {}: {error}",
                peer_rs.display()
            )
        });

        let hits = guarded_port_impls(&source).unwrap();
        let mut served: Vec<&str> = hits
            .iter()
            .filter(|hit| hit.backend == PEER)
            .map(|hit| hit.port.as_str())
            .collect();
        served.sort_unstable();
        assert_eq!(
            served, EXECUTION_PORTS,
            "the confinement peer must serve every execution port as a production impl: {hits:?}"
        );
    }

    /// The credential-boundary census (C-404): **every production `call_with_host` is enumerated
    /// here, with what the boundary does about it.**
    ///
    /// C-312 put the boundary on plugin responses and described its scope in a doc comment. That
    /// comment was wrong on the day it was written — it claimed four sites where the tree had five,
    /// and the one it omitted was precisely the site its single carve-out existed to excuse. Nothing
    /// failed. C-403 rewrote it into a five-row table; still prose, still nothing failing when it
    /// goes stale.
    ///
    /// This is the check that fails instead. A new `call_with_host` anywhere in either workspace's
    /// production code is a **new place a plugin-authored response enters flux**, and the author of
    /// that call has to come here and say what the boundary does about it — which is a review, not a
    /// formality: `refuse_response`/`scrub_error` is what stands between a hostile deployment's
    /// answer and a transcript, an evidence observation, or an operator's scrollback.
    ///
    /// Counted per file rather than per line so the pin survives ordinary edits above a call site
    /// and still fails on an added or removed one. `flux-codegate` does not depend on `flux-plugin`
    /// and must not — this scans source text, which is why the rule can live in the layering-lint
    /// crate at all.
    #[test]
    fn every_plugin_response_ingest_site_is_in_the_credential_boundary_census() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        // `(file, ingest sites in it, what the boundary does about them)`.
        //
        // The prose that used to carry this lives at the head of
        // `crates/flux-plugin/src/host/credential_boundary.rs`, which now cites this test. Keep the
        // two in step: this table is the enforced half.
        const CENSUS: &[(&str, usize, &str)] = &[
            (
                "crates/flux-capabilities/src/endpoint/broker.rs",
                2,
                "`endpoint.discover` fan-out — boundary RUNS (C-403); `secret.read` — EXEMPT by \
                 purpose, handing a credential value to host code is its success case (reasoned at \
                 the call site)",
            ),
            (
                "crates/flux-cli/src/plugin_cmd.rs",
                2,
                "`flux plugin call` — boundary RUNS; the `plugin.validate` preflight — boundary \
                 RUNS since C-404 removed the `internal: true` carve-out",
            ),
            (
                "crates/flux-plugin/src/host/loading.rs",
                2,
                "`PluginHost::call` self-delegation with `DenyHostCaps` (no non-test caller, not a \
                 surface of its own); the projected-tool path — boundary RUNS (C-312)",
            ),
        ];

        let rs_files = workspace_source_files(repo_root);
        assert!(
            rs_files.len() > 20,
            "expected to scan a representative set of source files, found {}",
            rs_files.len()
        );

        let mut found: BTreeMap<String, usize> = BTreeMap::new();
        for file in &rs_files {
            let rel = file
                .strip_prefix(repo_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).unwrap();
            let hits = plugin_response_ingest_sites(&src).unwrap_or_else(|error| {
                panic!("parse {} for the ingest census: {error}", file.display())
            });
            if !hits.is_empty() {
                found.insert(rel, hits.len());
            }
        }

        let mut violations = Vec::new();
        for (path, expected, disposition) in CENSUS {
            match found.remove(*path) {
                Some(count) if count == *expected => {}
                Some(count) => violations.push(format!(
                    "{path}: the census records {expected} plugin-response ingest site(s) \
                     ({disposition}) but the tree has {count} — say what the credential boundary \
                     does about the new one, then update this table"
                )),
                None => violations.push(format!(
                    "{path}: the census records {expected} plugin-response ingest site(s) but the \
                     tree has none — the sites moved, and the census now pins nothing"
                )),
            }
        }
        for (path, count) in found {
            violations.push(format!(
                "{path}: {count} uncensused `{PLUGIN_RESPONSE_INGEST_METHOD}` call(s) — a new place \
                 a plugin-authored response enters flux. Decide whether the credential boundary \
                 runs there (`flux_plugin::credential_boundary::refuse_response` / `scrub_error`), \
                 then add the file to CENSUS with that decision"
            ));
        }

        assert!(
            violations.is_empty(),
            "the credential-boundary census no longer describes the tree (C-404):\n  {}",
            violations.join("\n  ")
        );
    }

    /// The scanner's own pins: it must see a method call and a UFCS call, see one inside a macro
    /// body, ignore the `pub async fn` definition and prose naming it, and ignore `#[cfg(test)]`
    /// code.
    ///
    /// The last two are the ones that matter. A scanner that counted the *definition* would make
    /// `loading.rs` read 3 and the census a number nobody could derive; a scanner that counted the
    /// doc comment describing the census would count the census itself — which is exactly how the
    /// prose version stayed wrong.
    #[test]
    fn plugin_ingest_scanner_sees_calls_and_ignores_definitions_prose_and_test_code() {
        let src = r#"
//! The census names `call_with_host` in prose, which is not a call.
impl PluginHost {
    /// Calls `call_with_host` — also prose.
    pub async fn call(&mut self, op: &str) -> Result<Value> {
        self.call_with_host(op, input, &DenyHostCaps).await
    }
    pub async fn call_with_host(&mut self, op: &str) -> Result<Value> {
        let _ = "call_with_host";
        todo!()
    }
}
fn ufcs(host: &mut PluginHost) {
    let _ = PluginHost::call_with_host(host, "op");
}
fn in_a_macro(host: &mut PluginHost) {
    assert!(host.call_with_host("op").is_ok());
}
#[cfg(test)]
mod tests {
    fn fixture(host: &mut PluginHost) {
        let _ = host.call_with_host("op");
    }
}
"#;
        let hits = plugin_response_ingest_sites(src).unwrap();
        assert_eq!(
            hits.len(),
            3,
            "the self-delegation, the UFCS call and the one inside `assert!` — and nothing \
             else: {hits:?}"
        );
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
    /// std or Tokio process command. `flux-system` owns the two reviewed construction points every
    /// effect goes through — the canonical std builder and its Tokio conversion — and the only
    /// admitted constructor outside it is the TUI's self re-exec, which replaces this process
    /// rather than running anything (see its entry below). Allowances are single-use, so a second
    /// constructor even inside an already-allowed function fails.
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
            // `/restart` replaces the TUI process with the newly installed binary. It is the one
            // process construction that must NOT go through the seam above, because that builder
            // exists to run an *effect* under policy: it scrubs the environment down to `SAFE_ENV`,
            // forces the working directory to the workspace root, and may wrap the argv in the
            // sandbox. A re-exec has to preserve the process exactly — same executable, same argv,
            // same environment, same directory — or the session that comes back is a different one
            // wearing the same name, which is precisely what the restart contract forbids.
            //
            // It is admissible because it governs nothing: the program is `current_exe()` and the
            // arguments are this process's own `args_os()`, so no untrusted input reaches it, and
            // `exec` produces no child to confine — the successor inherits the same authority the
            // operator already had.
            (
                "crates/flux-tui/src/lib.rs",
                "exec_replacement",
                ProcessApi::Std,
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

    /// Collect the integration-test sources of both Cargo workspaces. `CARGO_BIN_EXE_*` exists only
    /// in test targets, so this is the whole surface where a `flux` spawn can appear.
    fn workspace_test_files(repo_root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for workspace_dir in [repo_root.join("crates"), repo_root.join("plugins")] {
            let Ok(entries) = std::fs::read_dir(workspace_dir) else {
                continue;
            };
            for entry in entries {
                let dir = entry.unwrap().path();
                if dir.is_dir() {
                    collect_rs(&dir.join("tests"), &mut files);
                }
            }
        }
        files
    }

    #[test]
    fn ambient_sandbox_scanner_flags_unattended_and_forwarded_spawns() {
        // An auto-approved spawn with no posture: the exact 0.38.0 regression class.
        let bare = r#"
use std::process::Command;
#[test]
fn t() {
    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "hi"])
        .output()
        .unwrap();
}
"#;
        let hits = ambient_sandbox_spawns(bare).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].function, "t");
        assert_eq!(
            hits[0].kind,
            AmbientSandboxKind::Unattended("--yes".to_string())
        );

        // The same spawn, posture declared — settled, whatever the host has installed.
        let declared = bare.replace(
            r#".args(["run", "--yes", "-m", "mock", "hi"])"#,
            r#".args(["run", "--yes", "-m", "mock", "hi"]).env("FLUX_SANDBOX", "off")"#,
        );
        assert!(ambient_sandbox_spawns(&declared).unwrap().is_empty());

        // Forcing backend discovery at a nonexistent path is equally a declaration: the spawn is
        // hermetically no-backend and cannot read the host's posture (sandbox_posture.rs's shape).
        let forced = bare.replace(
            ".output()",
            r#".env("FLUX_BWRAP_BIN", "/nonexistent/bwrap").output()"#,
        );
        assert!(ambient_sandbox_spawns(&forced).unwrap().is_empty());

        // `--no-sandbox` states the posture in argv instead of the environment.
        let flagged_off = bare.replace(r#""run", "--yes""#, r#""run", "--no-sandbox", "--yes""#);
        assert!(ambient_sandbox_spawns(&flagged_off).unwrap().is_empty());

        // Chain split across statements, through a `let` binding, with the posture declared last.
        let split = r#"
use std::process::Command;
fn helper(args: &[&str]) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_flux"));
    cmd.arg("app").args(args);
    cmd.env("FLUX_SANDBOXED", "1");
    let _ = cmd.output();
}
"#;
        assert!(ambient_sandbox_spawns(split).unwrap().is_empty());

        // The same helper without a declaration: bulk-forwarded argv can become unattended at any
        // call site, which is precisely why the spawn — not the caller — must state its posture.
        let forwarding = split.replace("\n    cmd.env(\"FLUX_SANDBOXED\", \"1\");", "");
        let hits = ambient_sandbox_spawns(&forwarding).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].kind, AmbientSandboxKind::ForwardedArgv);

        // A flagless unattended surface (`flux review`) carries no `--yes` to key on.
        let review = bare.replace(
            r#""run", "--yes", "-m", "mock", "hi""#,
            r#""review", "-m", "mock""#,
        );
        let hits = ambient_sandbox_spawns(&review).unwrap();
        assert_eq!(
            hits[0].kind,
            AmbientSandboxKind::Unattended("review".to_string()),
            "{hits:?}"
        );
    }

    #[test]
    fn ambient_sandbox_scanner_ignores_attended_spawns_and_other_binaries() {
        // A fixed-shape array with a non-literal *value* is auditable at the site; an interactive
        // `flux run` with no `--yes` is not an unattended surface and owes nothing.
        let attended = r#"
use std::process::Command;
fn t(sid: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["fork", sid, "--at", "0", "-m", "mock"])
        .arg("--store")
        .arg(sid)
        .output()
        .unwrap();
}
"#;
        assert!(ambient_sandbox_spawns(attended).unwrap().is_empty());

        // A different test binary is a different program: `CARGO_BIN_EXE_flux` is matched exactly,
        // never as a prefix of `CARGO_BIN_EXE_flux_sdk_plugin_fixture`.
        let other_binary = r#"
use std::process::Command;
fn t(args: &[&str]) {
    let _ = Command::new(env!("CARGO_BIN_EXE_flux_sdk_plugin_fixture"))
        .args(args)
        .arg("--serve")
        .output();
}
"#;
        assert!(ambient_sandbox_spawns(other_binary).unwrap().is_empty());

        // The binary path reached through a local binding still resolves.
        let indirect = r#"
use std::process::Command;
fn t() {
    let bin = env!("CARGO_BIN_EXE_flux");
    let _ = Command::new(bin).arg("--serve=127.0.0.1:1").output();
}
"#;
        let hits = ambient_sandbox_spawns(indirect).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    /// The guard itself (C-266): a test that spawns `flux` into an auto-approving or serving surface
    /// must say which sandbox posture it needs. Inheriting the host's is what made three rounds of
    /// the same bug pass every developer's gate and red only CI, where no runner has `bwrap`.
    ///
    /// What this covers: `std`/`tokio` Command builders spawning `CARGO_BIN_EXE_flux` in any test
    /// target of either workspace, whose literal argv names an unattended surface, or which forwards
    /// argv in bulk so a caller could make it one.
    ///
    /// What it does NOT cover, deliberately: shell scripts (`scripts/smoke-live.sh` — covered
    /// behaviorally instead, by the two sandbox lanes in `.github/workflows/ci.yml`), a spawn whose
    /// program is computed at runtime rather than from `CARGO_BIN_EXE_flux`, argv assembled through a
    /// helper that returns a `Vec` built elsewhere, and posture injected via `.envs(map)`.
    #[test]
    fn every_unattended_test_spawn_declares_its_sandbox_posture() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();
        let mut violations = Vec::new();
        let mut scanned = 0usize;
        for file in workspace_test_files(repo_root) {
            let relative_path = file
                .strip_prefix(repo_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&file).unwrap();
            scanned += 1;
            for hit in ambient_sandbox_spawns(&source).unwrap_or_else(|error| {
                panic!("parse {relative_path} for the sandbox-posture gate: {error}")
            }) {
                if allow_reason(&source, hit.line, "flux-allow-ambient-sandbox:").is_some() {
                    continue;
                }
                let why = match &hit.kind {
                    AmbientSandboxKind::Unattended(token) => {
                        format!("argv names an unattended surface ({token})")
                    }
                    AmbientSandboxKind::ForwardedArgv => {
                        "argv is forwarded in bulk, so a call site can make it unattended"
                            .to_string()
                    }
                };
                violations.push(format!(
                    "{relative_path}:{} in {}: {why}, but the spawn declares no sandbox posture",
                    hit.line, hit.function
                ));
            }
        }

        assert!(
            scanned > 20,
            "test-source walk scanned only {scanned} files"
        );
        assert!(
            violations.is_empty(),
            "test spawns inherit the host's sandbox posture — C-262 fails these closed on a runner \
             without a backend while they pass on every developer machine. Declare the posture in \
             the spawn ({}), pass --no-sandbox, or waive it with a reasoned \
             `// flux-allow-ambient-sandbox: …` comment:\n  {}",
            SANDBOX_POSTURE_ENV.join(" / "),
            violations.join("\n  ")
        );
    }

    /// The trigger table above is only as good as its agreement with the CLI. A new unattended
    /// surface keyed on a flag is already caught by argv matching; one keyed on anything else is
    /// invisible until the scanner is taught it.
    ///
    /// Three ways an arm can be keyed, and each has its own recogniser: a **flag** (`--yes`,
    /// `--serve` — [`UNATTENDED_ARGV_FLAGS`]), a **subcommand name** (`review`, `plugin call` —
    /// [`FLAGLESS_UNATTENDED_SUBCOMMANDS`]), and a **`<program.flux>` argument** (C-410's channel
    /// daemon — [`program_mode_argv`]). The partition below is exhaustive on purpose: an arm that
    /// falls into the third bucket must not be silently demanded of the second, which is what
    /// happened when program mode was first pinned and the drift check asked for a bare `run`
    /// entry — an entry that would have demanded a posture from every interactive `flux run` spawn
    /// in the tree.
    #[test]
    fn flagless_unattended_surfaces_match_the_cli_classifier() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let dispatch = crates_dir.join("flux-cli/src/dispatch.rs");
        let source = std::fs::read_to_string(&dispatch).expect("read flux-cli dispatch");
        let arms = unattended_surface_arms(&source).expect("parse flux-cli dispatch");

        assert!(
            arms.len() > 4,
            "did not find `unattended_sandbox_surface`'s arms — has it moved or been renamed? {arms:?}"
        );
        // Program-mode arms are recognised by argument, not by name, so they are accounted for
        // separately — and their existence is asserted, or this partition would quietly become a
        // way to drop an arm out of the check entirely.
        let program_keyed: BTreeSet<String> = arms
            .iter()
            .filter(|arm| arm.keyed_on_program_arg)
            .map(|arm| arm.subcommand.clone())
            .collect();
        assert!(
            program_keyed.len() >= 2,
            "expected both spellings of program mode (`run <p.flux>` and `app run <p.flux>`) to be \
             pinned and recognisable as argument-keyed; found {program_keyed:?}"
        );
        for argv in [
            vec!["run".to_string(), "p.flux".to_string()],
            vec!["app".to_string(), "run".to_string(), "p.flux".to_string()],
        ] {
            assert!(
                program_mode_argv(&argv).is_some(),
                "the argv recogniser missed a pinned program-mode spawn: {argv:?}"
            );
        }
        // The controls: a prompt turn and an `--entry` flow run are NOT program mode, and matching
        // them would demand a posture declaration from most `flux run` spawns in the tree.
        for argv in [
            vec!["run".to_string(), "hello".to_string()],
            vec![
                "run".to_string(),
                "--entry".to_string(),
                "main".to_string(),
                "p.flux".to_string(),
            ],
        ] {
            assert!(
                program_mode_argv(&argv).is_none(),
                "the argv recogniser over-matched a non-daemon spawn: {argv:?}"
            );
        }

        let flagless: BTreeSet<String> = arms
            .iter()
            .filter(|arm| !arm.keyed_on_flag && !arm.keyed_on_program_arg)
            .map(|arm| arm.subcommand.clone())
            .collect();
        let declared: BTreeSet<String> = FLAGLESS_UNATTENDED_SUBCOMMANDS
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(
            flagless, declared,
            "FLAGLESS_UNATTENDED_SUBCOMMANDS has drifted from `unattended_sandbox_surface`: a \
             subcommand that is unattended with no flag at all cannot be recognized in a test's \
             argv unless it is listed there"
        );
    }

    /// **C-410.** The classifier is a hand-written enumeration of a machine-generated enum, and the
    /// review found it one variant short: `Commands::Plugin` had no arm, so `flux plugin call` ran
    /// headless at the `Off` sandbox default because of a `_ => None` nobody re-read.
    ///
    /// Rust's exhaustiveness check is the primary guard — a new `Commands` variant does not compile
    /// until it is classified. This is the guard on *that* guard: a re-added wildcard would restore
    /// the original defect silently, and it is exactly the edit that reads like tidying up.
    #[test]
    fn the_unattended_classifier_covers_every_commands_variant() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let args = std::fs::read_to_string(crates_dir.join("flux-cli/src/args.rs"))
            .expect("read flux-cli args");
        let dispatch = std::fs::read_to_string(crates_dir.join("flux-cli/src/dispatch.rs"))
            .expect("read flux-cli dispatch");
        let coverage =
            unattended_classifier_coverage(&args, &dispatch).expect("parse flux-cli sources");

        // Non-vacuity: an empty variant list (a renamed enum, a moved file) would satisfy every
        // assertion below while checking nothing at all.
        assert!(
            coverage.variants.len() > 20,
            "did not find `enum Commands` — has it moved or been renamed? {:?}",
            coverage.variants
        );
        assert_eq!(
            coverage.catch_all_arms, 0,
            "`unattended_sandbox_surface` has a catch-all arm again. That is the C-410 defect \
             itself: a new subcommand then inherits a sandbox classification nobody chose. Give \
             every variant an explicit arm — pinned to the fail-closed profile, or exempt with the \
             reason at the arm."
        );
        assert!(
            coverage.unclassified().is_empty(),
            "`unattended_sandbox_surface` never names {:?} — each `Commands` variant must be \
             classified explicitly against the fail-closed unattended profile (C-262/C-410)",
            coverage.unclassified()
        );
    }

    /// The scanner above is only as good as its ability to *see* a wildcard and a gap. Fixtures
    /// shaped like the real function: one classified, one exempt, one variant with no arm at all,
    /// and a `_ => None` — the pre-C-410 shape.
    #[test]
    fn the_coverage_scanner_sees_a_wildcard_and_a_missing_variant() {
        const ARGS: &str = r#"
            pub(super) enum Commands {
                Run { yes: bool },
                Plugin { action: Option<PluginAction> },
                Doctor { json: bool },
            }
        "#;
        const COVERED: &str = r#"
            fn unattended_sandbox_surface(cli: &Cli) -> Option<&'static str> {
                match cli.command.as_ref()? {
                    Commands::Run { yes, .. } if *yes => Some("auto-approved"),
                    Commands::Plugin { action: Some(PluginAction::Call { .. }) } => Some("headless"),
                    Commands::Run { .. } | Commands::Plugin { .. } | Commands::Doctor { .. } => None,
                }
            }
        "#;
        const WILDCARD: &str = r#"
            fn unattended_sandbox_surface(cli: &Cli) -> Option<&'static str> {
                match cli.command.as_ref()? {
                    Commands::Run { yes, .. } if *yes => Some("auto-approved"),
                    _ => None,
                }
            }
        "#;

        let good =
            unattended_classifier_coverage(ARGS, COVERED).expect("parse the covered fixture");
        assert_eq!(good.catch_all_arms, 0);
        assert!(
            good.unclassified().is_empty(),
            "the fully-classified fixture reported gaps: {:?}",
            good.unclassified()
        );

        let bad =
            unattended_classifier_coverage(ARGS, WILDCARD).expect("parse the wildcard fixture");
        assert_eq!(bad.catch_all_arms, 1, "the `_ => None` arm was not seen");
        assert_eq!(
            bad.unclassified(),
            vec!["Plugin".to_string(), "Doctor".to_string()],
            "the variants the wildcard swallowed were not reported"
        );

        // Every *other* spelling of a catch-all, each on the catch-all assertion in its own right.
        // Without this the fixture proved only the `_` case and the rest survived on the weaker
        // `unclassified()` backstop — which reports nothing at all once someone adds the missing
        // arms and leaves the wildcard in place as a "safety net".
        for (label, arm) in [
            ("bare binding", "other => None,"),
            ("binding with a wildcard subpattern", "x @ _ => None,"),
            ("guarded wildcard", "_ if cli.no_sandbox => None,"),
            (
                "or-pattern with a wildcard",
                "Commands::Doctor { .. } | _ => None,",
            ),
            ("parenthesised wildcard", "(_) => None,"),
        ] {
            let src = format!(
                r#"
                fn unattended_sandbox_surface(cli: &Cli) -> Option<&'static str> {{
                    match cli.command.as_ref()? {{
                        Commands::Run {{ yes, .. }} if *yes => Some("auto-approved"),
                        Commands::Run {{ .. }} => None,
                        Commands::Plugin {{ .. }} => None,
                        Commands::Doctor {{ .. }} => None,
                        {arm}
                    }}
                }}
            "#
            );
            let coverage = unattended_classifier_coverage(ARGS, &src)
                .unwrap_or_else(|e| panic!("parse the {label} fixture: {e}"));
            // Non-vacuity: these fixtures classify every variant, so `unclassified()` is empty and
            // the catch-all assertion is the ONLY thing that can catch them.
            assert!(
                coverage.unclassified().is_empty(),
                "the {label} fixture was meant to leave no gap: {:?}",
                coverage.unclassified()
            );
            assert_eq!(
                coverage.catch_all_arms, 1,
                "a catch-all spelled as a {label} was not seen"
            );
        }
    }

    /// A fixture with both builder roots, a renamed import, a local-binding split, a `#[cfg(test)]`
    /// decoy and a look-alike builder from another crate — the shape
    /// `direct_io_scanner_resolves_imports_aliases_and_all_io_families` already uses.
    const PIN_FIXTURE: &str = r#"
use flux_sdk::Client as C;
use flux_sdk::FlowClient;

fn record() -> C {
    C::builder()
        .model("m")
        .resource_limits(limits())
        .build(provider, ".")
}

fn review() -> FlowClient {
    FlowClient::builder().resource_limits(limits()).build(p, ".")
}

fn split() -> FlowClient {
    let b = flux_sdk::FlowClient::builder();
    b.resource_limits(limits()).build(p, ".")
}

fn not_the_sdk() -> reqwest::Client {
    reqwest::Client::builder().resource_limits(limits()).build()
}

#[cfg(test)]
mod tests {
    fn decoy() {
        flux_sdk::Client::builder().resource_limits(limits());
    }
}
"#;

    #[test]
    fn pin_seam_scanner_resolves_aliases_bindings_and_skips_test_items() {
        let seams = pin_seams(PIN_FIXTURE).unwrap();
        assert_eq!(seams.len(), 3, "{seams:?}");
        assert_eq!(
            seams
                .iter()
                .map(|s| s.function.as_str())
                .collect::<Vec<_>>(),
            ["record", "review", "split"],
            "{seams:?}"
        );
        assert_eq!(
            seams.iter().map(|s| s.builder.as_str()).collect::<Vec<_>>(),
            [
                "flux_sdk::Client::builder",
                "flux_sdk::FlowClient::builder",
                "flux_sdk::FlowClient::builder"
            ],
            "{seams:?}"
        );
        assert!(seams.iter().all(|s| s.kind == SeamKind::ResourceLimits));
    }

    /// C-329's runner excises the span, so it must cover the whole chain link — dot to closing
    /// paren, receiver excluded — even when the call is spread over several lines.
    #[test]
    fn pin_seam_span_is_the_excisable_byte_range_of_the_whole_call() {
        for seam in pin_seams(PIN_FIXTURE).unwrap() {
            assert_eq!(&PIN_FIXTURE[seam.span()], ".resource_limits(limits())");
        }

        let multiline = "fn f() {\n    flux_sdk::Client::builder()\n        .resource_limits(\n            cli_limits(&cfg),\n        )\n        .build(p, \".\")\n}\n";
        let seam = pin_seams(multiline).unwrap().pop().unwrap();
        assert_eq!(
            &multiline[seam.span()],
            ".resource_limits(\n            cli_limits(&cfg),\n        )"
        );
        // A byte span, not a line: excising it leaves a chain that still parses.
        let mut excised = multiline.to_string();
        excised.replace_range(seam.span(), "");
        assert!(syn::parse_file(&excised).is_ok(), "{excised}");
    }

    /// Mirrors `direct_io_allowance_requires_a_real_reason_immediately_above_the_call`: the waiver
    /// reader is `allow_reason`, and a bare marker with nothing after it is not a pin.
    #[test]
    fn a_pin_requires_a_named_test_and_a_bare_marker_is_not_one() {
        let pinned = "fn f() {\n    flux_sdk::Client::builder()\n        // flux-pin: a_test_name and why\n        .resource_limits(l)\n}\n";
        let seam = pin_seams(pinned).unwrap().pop().unwrap();
        assert_eq!(
            pinned_test_name(pinned, seam.line).as_deref(),
            Some("a_test_name")
        );

        let bare = pinned.replace("// flux-pin: a_test_name and why", "// flux-pin:");
        let seam = pin_seams(&bare).unwrap().pop().unwrap();
        assert!(pinned_test_name(&bare, seam.line).is_none());
        assert!(allow_reason(&bare, seam.line, PIN_EXEMPT_MARKER).is_none());
    }

    /// The anti-drift half: a pin only counts when the test it names actually exists.
    #[test]
    fn test_name_resolution_sees_cfg_test_modules_and_attribute_forms() {
        let source = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn a_plain_test() {}

    #[tokio::test(flavor = "multi_thread")]
    async fn an_async_test() {}

    fn a_helper() {}
}
"#;
        assert_eq!(
            test_function_names(source).unwrap(),
            ["a_plain_test", "an_async_test"]
        );

        // A pin naming something outside that universe does not resolve — the whole point.
        let universe: BTreeSet<String> = test_function_names(source).unwrap().into_iter().collect();
        assert!(universe.contains("a_plain_test"));
        assert!(!universe.contains("a_test_that_was_renamed_away"));
    }

    /// The first whitespace-delimited token of a `flux-pin:` reason: the test that must die when
    /// the seam is excised. Anything after it is prose for the reader.
    fn pinned_test_name(source: &str, line: usize) -> Option<String> {
        let reason = allow_reason(source, line, PIN_MARKER)?;
        reason.split_whitespace().next().map(str::to_string)
    }

    /// What the census decides about one seam.
    #[derive(Debug, PartialEq, Eq)]
    enum PinVerdict {
        /// Pinned to a test that resolves.
        Pinned(String),
        /// Pinned to a name no test in either workspace declares — the pin has drifted.
        Drifted(String),
        /// Deliberately unobserved, with a reason.
        Exempt(String),
        /// No marker at all: deleting the line would change nothing.
        Unpinned,
    }

    /// The per-seam decision, factored out so [`pin_verdicts_distinguish_pinned_drifted_and_exempt`]
    /// exercises the same code the workspace walk does rather than a fixture-shaped restatement of
    /// it — the failure mode `guards tested against their own assumptions` names.
    fn pin_verdict(source: &str, seam: &Seam, test_names: &BTreeSet<String>) -> PinVerdict {
        if let Some(name) = pinned_test_name(source, seam.line) {
            return if test_names.contains(&name) {
                PinVerdict::Pinned(name)
            } else {
                PinVerdict::Drifted(name)
            };
        }
        match allow_reason(source, seam.line, PIN_EXEMPT_MARKER) {
            Some(why) => PinVerdict::Exempt(why),
            None => PinVerdict::Unpinned,
        }
    }

    /// The anti-drift half, on fixtures: a pin only counts when the test it names exists, an
    /// exemption needs a reason, and neither marker means unpinned.
    #[test]
    fn pin_verdicts_distinguish_pinned_drifted_and_exempt() {
        let universe: BTreeSet<String> = ["a_live_test".to_string()].into_iter().collect();
        let site = |marker: &str| {
            format!("fn f() {{\n    flux_sdk::Client::builder()\n        {marker}\n        .resource_limits(l)\n}}\n")
        };
        let verdict = |source: &str| {
            let seam = pin_seams(source).unwrap().pop().unwrap();
            pin_verdict(source, &seam, &universe)
        };

        assert_eq!(
            verdict(&site("// flux-pin: a_live_test")),
            PinVerdict::Pinned("a_live_test".into())
        );
        assert_eq!(
            verdict(&site("// flux-pin: a_test_that_was_renamed_away")),
            PinVerdict::Drifted("a_test_that_was_renamed_away".into())
        );
        assert_eq!(
            verdict(&site(
                "// flux-pin-exempt: replays are hermetic by contract"
            )),
            PinVerdict::Exempt("replays are hermetic by contract".into())
        );
        // A bare marker of either kind is not a declaration.
        assert_eq!(verdict(&site("// flux-pin:")), PinVerdict::Unpinned);
        assert_eq!(verdict(&site("// flux-pin-exempt:")), PinVerdict::Unpinned);
        assert_eq!(verdict(&site("// just a comment")), PinVerdict::Unpinned);
    }

    /// **The pin census (C-328).** Every wiring seam a shipped surface assembles through an SDK
    /// client builder names, in-source, a test that observes it — and that test resolves.
    ///
    /// This exists because nineteen stories found production wiring whose deletion changed nothing:
    /// C-305 deleted two `flux-tui` lines and left 474 tests green; C-314 deleted both `[limits]`
    /// wirings and left the whole `flux-cli` suite green. Each was answered by authoring a new
    /// bespoke guard. This is the one mechanism that replaces guard #11.
    ///
    /// It is a **coverage floor, not a proof**: a pin asserts a named test exists, not that the test
    /// dies for the right reason. C-329's runner excises each [`Seam::span`] and proves the named
    /// test actually reds; until then the reviewer still reads the test.
    #[test]
    fn every_sdk_client_wiring_seam_pins_a_test_that_observes_it() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        let relative = |file: &Path| {
            file.strip_prefix(repo_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/")
        };

        // The universe a pin resolves against: the integration-test sources of both workspaces plus
        // every `#[cfg(test)]` module the production walk sees.
        let mut test_names = BTreeSet::new();
        let mut test_files_scanned = 0usize;
        for file in workspace_test_files(repo_root)
            .into_iter()
            .chain(workspace_source_files(repo_root))
        {
            let source = std::fs::read_to_string(&file).unwrap();
            test_files_scanned += 1;
            for name in test_function_names(&source).unwrap_or_else(|error| {
                panic!("parse {} for the pin census: {error}", relative(&file))
            }) {
                test_names.insert(name);
            }
        }

        let mut violations = Vec::new();
        let mut exemptions = Vec::new();
        let mut scanned = 0usize;
        let mut seams_found = 0usize;
        for file in workspace_source_files(repo_root) {
            let path = relative(&file);
            let source = std::fs::read_to_string(&file).unwrap();
            scanned += 1;
            for seam in pin_seams(&source)
                .unwrap_or_else(|error| panic!("parse {path} for the pin census: {error}"))
            {
                seams_found += 1;
                let site = format!("{path}:{} in {}", seam.line, seam.function);
                match pin_verdict(&source, &seam, &test_names) {
                    PinVerdict::Pinned(_) => {}
                    PinVerdict::Drifted(name) => violations.push(format!(
                        "{site}: pins `{name}`, which is not a test anywhere in either workspace \
                         — the pin has drifted from the test it names"
                    )),
                    PinVerdict::Exempt(why) => exemptions.push(format!("{site}: {why}")),
                    PinVerdict::Unpinned => violations.push(format!(
                        "{site}: `.{}(..)` on `{}` names no test — deleting this line would \
                         change nothing",
                        match seam.kind {
                            SeamKind::ResourceLimits => "resource_limits",
                        },
                        seam.builder
                    )),
                }
            }
        }

        // Anti-vacuity, in the idiom of `architecture_source_walk_covers_both_workspaces`: a census
        // that scanned nothing, or that stopped resolving the builder roots, passes silently.
        assert!(scanned > 300, "the pin census scanned only {scanned} files");
        assert!(
            test_files_scanned > 300,
            "the pin-resolution universe scanned only {test_files_scanned} files"
        );
        assert!(
            test_names.len() > 500,
            "the pin-resolution universe found only {} test names — the collector has stopped \
             seeing `#[cfg(test)]` modules and every pin would resolve to nothing",
            test_names.len()
        );
        assert!(
            seams_found >= 2,
            "the pin census found {seams_found} seams — the SDK client builders have moved, been \
             renamed, or stopped resolving, and this gate is now inert"
        );
        assert!(
            exemptions.len() <= MAX_PIN_EXEMPTIONS,
            "{} `flux-pin-exempt` seams exceed the standing budget of {MAX_PIN_EXEMPTIONS}. \
             Exemptions are not a maintenance mode — raise the budget in the same diff, with the \
             reason, or pin the seam:\n  {}",
            exemptions.len(),
            exemptions.join("\n  ")
        );

        assert!(
            violations.is_empty(),
            "wiring that no test observes — deleting the line would change nothing (C-314). \
             Declare the test that dies without it with a reasoned `// flux-pin: <test_name> …` \
             comment directly above the call, or waive it with `// flux-pin-exempt: <why>`:\n  {}",
            violations.join("\n  ")
        );
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

    // =========================================================================================
    // C-325 — no source file carries a literal a forge's secret scanning would block a push on
    // =========================================================================================

    /// The fixture credential this file needs in order to prove the scanner works, assembled from
    /// two fragments so that *this crate's own source* does not carry the shape it forbids.
    ///
    /// It is also the worked example of the remedy: the scanner sees the whole credential, the file
    /// on disk carries neither `sk-ant-api` nor a body long enough to match anything.
    fn fixture_credential(head: &str, tail: &str) -> String {
        format!("{head}{tail}")
    }

    /// The scanner flags a written-out credential and, on the same bytes minus the join, does not.
    ///
    /// Both halves matter. Without the first the gate is inert; without the second the remedy the
    /// gate demands would not actually satisfy it, and there would be no way to keep a corpus that
    /// contains realistic credential shapes.
    #[test]
    fn push_protection_scanner_flags_a_written_literal_but_not_an_assembled_one() {
        let key = fixture_credential("sk-ant-", "api03-c325scannerfixture000000000000");
        let written = format!("const K: &str = \"{key}\";\n");
        let hits = push_protection_shapes(&written);
        assert_eq!(
            hits.len(),
            1,
            "the scanner missed a written credential: {written}"
        );
        assert_eq!(hits[0], (1, key.clone()));

        // The remedy: the same bytes, joined at compile time. `concat!` yields the identical
        // `&'static str`, so every assertion downstream is over the same value as before.
        let assembled =
            "const K: &str = concat!(\"sk-ant-\", \"api03-c325scannerfixture000000000000\");\n";
        assert!(
            push_protection_shapes(assembled).is_empty(),
            "a fragment-joined credential must not be flagged, or the remedy is impossible"
        );
    }

    /// The other direction: the floors exist so ordinary identifiers and placeholder values keep
    /// their spelling. A gate that fires on `hf_hub_download` gets waived, and then it is gone.
    #[test]
    fn the_push_protection_scanner_leaves_ordinary_identifiers_alone() {
        for benign in [
            "let x = hf_hub_download(path);",
            "// glpat-example is not a token",
            "let short = \"sk_live_x\";",
            // The registered-value fixtures: the redactor is proved on these by `add_secret`, not
            // by their shape, so they are below every floor on purpose.
            "redactor.add_secret(\"xoxb-redact-me-1234\");",
            "\"found: xoxb-app-secret-987\"",
            "let sha = \"ghp_0123456789abcdef\";",
        ] {
            assert!(
                push_protection_shapes(benign).is_empty(),
                "over-flagged: {benign}"
            );
        }
    }

    /// **The gate.** No Rust source in either workspace carries a credential-shaped literal.
    ///
    /// C-325's measurement: a push of eleven merged stories was rejected with twelve detections
    /// across three commits and two files, every one a false positive over a provably synthetic
    /// corpus literal. Each rejection needs a human to click through an unblock URL per detector
    /// rule, and the commit stays blocked for every future clone of the repository. Assembling the
    /// literal from fragments costs nothing — the test still sees the whole credential — so the
    /// only durable answer is to make writing one out fail here first.
    #[test]
    fn no_workspace_source_carries_a_push_protection_shaped_literal() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        let mut scanned = 0usize;
        let mut violations = Vec::new();
        for file in workspace_source_files(repo_root)
            .into_iter()
            .chain(workspace_test_files(repo_root))
        {
            let source = std::fs::read_to_string(&file).expect("read source");
            scanned += 1;
            let relative = file
                .strip_prefix(repo_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            for (line, hit) in push_protection_shapes(&source) {
                violations.push(format!("{relative}:{line}: {hit}"));
            }
        }

        // Anti-vacuity: a walk that stopped resolving would report zero violations and look green.
        assert!(
            scanned > 400,
            "the push-protection walk scanned only {scanned} files"
        );
        assert!(
            violations.is_empty(),
            "credential-shaped literals in the source will block `git push` on a forge with secret \
             scanning enabled — and the commit carrying them stays blocked for every future clone. \
             Join the literal from fragments instead (`concat!(\"sk-ant-\", \"api03-…\")`), which \
             leaves the value the test asserts over byte-identical:\n  {}",
            violations.join("\n  ")
        );
    }

    // -----------------------------------------------------------------------
    // C-393 — no test resolves the per-turn workspace probe from the operator's home
    // -----------------------------------------------------------------------

    /// The scanner's own pins. It must see a qualified call and a method call, ignore prose and
    /// string literals, ignore an identifier that merely *contains* a banned name, not confuse the
    /// pinned `*_in` form for the ambient one, and — in a `src/` file — look only inside
    /// `#[cfg(test)]`.
    #[test]
    fn the_discovery_scanner_separates_test_calls_from_production_prose_and_the_pinned_form() {
        let src = r#"
/// Production prose naming detect_signals() and discover_commands() is not a call.
pub fn production() {
    let _ = flux_runtime::detect_signals(cwd);
    let _ = "detect_signals(cwd)";
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let _ = flux_runtime::detect_signals(cwd);
        let _ = flux_runtime::detect_signals_in(cwd, &env);
        let _ = spec.try_with_default_skills();
        let _ = spec.try_with_default_skills_in(&env);
        let _ = my_detect_signals_wrapper(cwd);
        let _ = DiscoveryEnv::empty();
    }
}
"#;
        let hits = ambient_discovery_calls(src, TestScope::CfgTestItems).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.callee.as_str()).collect();
        assert_eq!(
            names,
            ["detect_signals", "try_with_default_skills"],
            "exactly the two ambient calls inside `#[cfg(test)]` — not the production call, not \
             the string literal, not the doc comment, not `my_detect_signals_wrapper`, and not \
             either `_in` form: {hits:?}"
        );

        // The same file read as an integration-test source: production scope is test scope there,
        // so the module-level call counts too.
        let whole = ambient_discovery_calls(src, TestScope::WholeFile).unwrap();
        assert_eq!(whole.len(), 3, "{whole:?}");

        // And the pinned tally sees the counterparts the census floors on.
        let pinned = pinned_discovery_calls(src, TestScope::CfgTestItems).unwrap();
        let pinned_names: Vec<&str> = pinned.iter().map(|h| h.callee.as_str()).collect();
        assert_eq!(
            pinned_names,
            [
                "detect_signals_in",
                "try_with_default_skills_in",
                "DiscoveryEnv"
            ],
            "{pinned:?}"
        );
    }

    /// A `#[cfg(test)]` helper `fn` at module level (not inside a test `mod`) is test code too —
    /// `flux-config` and `flux-cli` both have that shape, and a scanner anchored only on `mod`
    /// would walk straight past it.
    #[test]
    fn the_discovery_scanner_reaches_cfg_test_helper_functions_and_impls() {
        let src = r#"
#[cfg(test)]
fn helper(cwd: &Path) {
    let _ = flux_runtime::metadata::discover_commands(cwd);
}

#[cfg(test)]
impl Fixture {
    fn skills(&self) -> Vec<Skill> {
        self.spec.clone().try_with_model_invoked_skills().unwrap()
    }
}
"#;
        let hits = ambient_discovery_calls(src, TestScope::CfgTestItems).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.callee.as_str()).collect();
        assert_eq!(
            names,
            ["discover_commands", "try_with_model_invoked_skills"],
            "{hits:?}"
        );
    }

    /// **The gate.** No test in either workspace reaches the per-turn workspace probe — or the
    /// command/skill discovery it re-runs — through the process's own `HOME`.
    ///
    /// `detect_signals` decides which evidence-gated tool groups surface, and two of its checks are
    /// rooted at the user's home rather than at `cwd`: `agent_triggerable` re-runs command and skill
    /// discovery (which includes `~/.flux/commands`, `~/.claude/commands` and the three user-global
    /// skill roots) and `kubernetes` tests `~/.kube/config`. A test that reached them through the
    /// process environment asserted against whatever the developer keeps in their own home.
    ///
    /// This is a census, not an inspection: the next such call is flagged by a red gate rather than
    /// by someone remembering. C-333 will generalize the rule; until then this is the workspace-wide
    /// guard for the `detect_signals` tranche, and `flux-server`'s `router_env_is_pinned.rs` is the
    /// crate-local one for the router tranche.
    #[test]
    fn no_test_resolves_the_workspace_probe_from_the_operators_home() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        let mut scanned = 0usize;
        let mut pinned = 0usize;
        let mut violations = Vec::new();
        let sources = workspace_source_files(repo_root)
            .into_iter()
            .map(|path| (path, TestScope::CfgTestItems))
            .chain(
                workspace_test_files(repo_root)
                    .into_iter()
                    .map(|path| (path, TestScope::WholeFile)),
            );
        for (file, scope) in sources {
            let source = std::fs::read_to_string(&file).expect("read source");
            scanned += 1;
            let relative = file
                .strip_prefix(repo_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // A source this crate cannot parse is a scanner blind spot, not a pass.
            let hits = ambient_discovery_calls(&source, scope)
                .unwrap_or_else(|e| panic!("parse {relative}: {e}"));
            for hit in hits {
                violations.push(format!(
                    "{relative}:{}: `{}(..)` resolves flux's user-global discovery roots from the \
                     process $HOME — use the `_in` form with a pinned DiscoveryEnv (C-393)",
                    hit.line, hit.callee
                ));
            }
            pinned += pinned_discovery_calls(&source, scope).unwrap().len();
        }

        assert!(
            violations.is_empty(),
            "tests whose verdict depends on the machine's $HOME:\n  {}",
            violations.join("\n  ")
        );
        // Anti-vacuity, both halves: a walk that stopped resolving, and a needle list that drifted
        // off every real call site, both report zero violations and look green.
        assert!(
            scanned > 400,
            "the discovery-probe walk scanned only {scanned} files"
        );
        assert!(
            pinned >= 25,
            "only {pinned} pinned discovery references found across {scanned} sources — either the \
             migrated tests moved or the needles drifted and this check measures nothing"
        );
    }
}
