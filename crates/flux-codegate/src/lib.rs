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

struct DirectIoAliasCollector<'a>(&'a mut ImportAliases);

impl<'ast> Visit<'ast> for DirectIoAliasCollector<'_> {
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
                function: self
                    .functions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "<module>".into()),
            });
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
}

/// Resolve direct filesystem, process, socket, HTTP-client, and database opens from parsed Rust.
/// Imports, renamed imports, module aliases, type aliases, local callable aliases, and multiline
/// calls are followed; test-only items, comments, and strings are excluded structurally by `syn`.
pub fn raw_direct_io_calls(src: &str) -> syn::Result<Vec<DirectIoCall>> {
    let file = syn::parse_file(src)?;
    let mut aliases = ImportAliases::default();
    DirectIoAliasCollector(&mut aliases).visit_file(&file);
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
const GUARDED_PORT_TRAITS: &[&str] = &["GuardedProcess", "GuardedHostFiles", "GuardedEnv"];

/// A production `impl <port trait> for <type>` — a type declaring itself a guarded IO backend.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GuardedPortImpl {
    pub line: usize,
    /// The **canonical** port trait name (`GuardedProcess`), whatever local spelling reached it.
    /// Allowances match on this, so a rename cannot mint a fresh unreviewed identity.
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
fn direct_port_trait(segments: &[String]) -> Option<&'static str> {
    let last = segments.last()?;
    GUARDED_PORT_TRAITS
        .iter()
        .find(|port| *port == last)
        .copied()
}

/// Local names that reach a guarded-IO port trait, so a renamed import cannot hide a backend.
///
/// This mirrors [`ProcessAliases`], which already resolves `use std::process::Command as Exec` for
/// `no_raw_process_command_outside_system` — without the same treatment here the newer gate would be
/// weaker than its sibling against the identical evasion.
#[derive(Default)]
struct PortAliases {
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
    fn new() -> Self {
        let mut aliases = Self::default();
        for port in GUARDED_PORT_TRAITS {
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
                if let Some(port) = direct_port_trait(prefix) {
                    self.traits.insert(name.ident.to_string(), port);
                }
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                match direct_port_trait(prefix) {
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
    let file = syn::parse_file(src)?;
    let mut aliases = PortAliases::new();
    PortAliasCollector(&mut aliases).visit_file(&file);
    aliases.resolve_renames();
    let mut visitor = PortImplVisitor {
        aliases: &aliases,
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

    fn direct_io_allow_reason(source: &str, line: usize) -> Option<String> {
        let lines = source.lines().collect::<Vec<_>>();
        let mut cursor = line.saturating_sub(1);
        while cursor > 0 {
            cursor -= 1;
            let trimmed = lines.get(cursor)?.trim();
            let comment = trimmed.strip_prefix("//")?.trim();
            if let Some(reason) = comment.strip_prefix("flux-allow-direct-io:") {
                let reason = reason.trim();
                return (!reason.is_empty()).then(|| reason.to_string());
            }
        }
        None
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
use flux_system::port::{GuardedEnv as Env, GuardedHostFiles};
use flux_system::port as p;
use Hop as Chained;
use flux_system::port::GuardedProcess as Hop;

impl Env for A {}
impl GuardedHostFiles for B {}
impl p::GuardedEnv for C {}
impl Chained for D {}
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

        // The reviewed backends: `flux-system`'s native delegations, and nothing else. A new entry
        // here is a security review, not a formality — read `crates/flux-system/src/port.rs` first.
        const ALLOW: &[(&str, &str, &str)] = &[
            ("crates/flux-system/src/port.rs", "GuardedProcess", "System"),
            (
                "crates/flux-system/src/port.rs",
                "GuardedHostFiles",
                "System",
            ),
            ("crates/flux-system/src/port.rs", "GuardedEnv", "System"),
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
