//! Dynamic composite-op registry for agent-registered operations.
//!
//! Composite execution still lives in `flux-lang`; this module only owns host policy around where
//! definitions are stored, how scopes override each other, and when persisted definitions are loaded.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use flux_core::{Error, Result};
use flux_lang::program::{CompositeOpDecl, Module};
use flux_runtime::{CompositeRegisterRequest, ToolRegistry};
use flux_system::System;

use crate::registry::analyze_composites;
use crate::state::FlowStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeScope {
    Turn,
    Session,
    Project,
    Global,
}

impl CompositeScope {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "turn" => Ok(Self::Turn),
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "global" => Ok(Self::Global),
            other => Err(Error::Other(format!(
                "op.register: unknown scope `{other}` (expected turn, session, project, or global)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Session => "session",
            Self::Project => "project",
            Self::Global => "global",
        }
    }

    pub fn path_for(self, name: &str) -> Option<String> {
        match self {
            Self::Project => Some(format!(".flux/ops/{name}.flux")),
            Self::Global => Some(format!("@global_ops/{name}.flux")),
            Self::Turn | Self::Session => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    decl: CompositeOpDecl,
}

#[derive(Default)]
struct State {
    global: BTreeMap<String, Entry>,
    project: BTreeMap<String, Entry>,
    sessions: HashMap<String, BTreeMap<String, Entry>>,
    turns: HashMap<String, BTreeMap<String, Entry>>,
    loaded_sessions: HashSet<String>,
}

#[derive(Default)]
pub struct DynamicComposites {
    state: Mutex<State>,
}

impl DynamicComposites {
    pub fn load(system: &System) -> Result<Self> {
        let ws = system.workspace();
        // `.flux/flows` (+ the `@global_flows` root) is the unified home: files there may hold a
        // flow, an op, or a whole module — we leniently pull every composite `op` out of them. The
        // legacy `.flux/ops` / `@global_ops` dirs keep their strict single-op-per-file loading.
        // Flows-dir definitions take precedence (loaded first; `merge_entries` keeps the first seen).
        let mut global = BTreeMap::new();
        if ws.has_named_root("global_flows") {
            merge_entries(&mut global, load_flows_dir(system, "@global_flows")?);
        }
        if ws.has_named_root("global_ops") {
            merge_entries(&mut global, load_dir(system, "@global_ops")?);
        }
        let mut project = load_flows_dir(system, ".flux/flows")?;
        merge_entries(&mut project, load_dir(system, ".flux/ops")?);
        Ok(Self {
            state: Mutex::new(State {
                global,
                project,
                ..State::default()
            }),
        })
    }

    pub fn validate_base(&self, tools: &ToolRegistry) -> Result<()> {
        let composites = {
            let st = self.state.lock().unwrap();
            active_from_state(&st, "")
        };
        validate_composites(&composites, tools)
    }

    pub fn ensure_session_loaded(&self, store: &FlowStore, session_id: &str) -> Result<()> {
        {
            let st = self.state.lock().unwrap();
            if st.loaded_sessions.contains(session_id) {
                return Ok(());
            }
        }
        let mut loaded = BTreeMap::new();
        for (name, source) in store.session_composites(session_id)? {
            let decl = parse_one_composite(&source).map_err(|e| {
                Error::Other(format!(
                    "session composite `{name}` for {session_id} is invalid: {e}"
                ))
            })?;
            if decl.name != name {
                return Err(Error::Other(format!(
                    "session composite `{name}` contains op `{}`",
                    decl.name
                )));
            }
            loaded.insert(name, Entry { decl });
        }
        let mut st = self.state.lock().unwrap();
        st.sessions.insert(session_id.to_string(), loaded);
        st.loaded_sessions.insert(session_id.to_string());
        Ok(())
    }

    pub fn active_for_session(&self, session_id: &str) -> Vec<CompositeOpDecl> {
        let st = self.state.lock().unwrap();
        active_from_state(&st, session_id)
    }

    pub fn clear_turn(&self, session_id: &str) {
        let mut st = self.state.lock().unwrap();
        st.turns.remove(session_id);
        // C-87: bound the per-session composite caches. On a long-lived engine shared across
        // conversations (the A2A server) `sessions`/`loaded_sessions` would otherwise accumulate one
        // entry per session_id ever seen and never shrink. At the turn boundary, retain only the
        // just-active session's loaded definitions; any other session reloads lazily (from the durable
        // store) on its next turn via `ensure_session_loaded`. A single-conversation engine (the CLI)
        // only ever holds one session, so this is a no-op there.
        st.sessions.retain(|k, _| k == session_id);
        st.loaded_sessions.retain(|k| k == session_id);
    }

    /// Drop every cached composite definition for a session — its session- and turn-scoped ops and the
    /// "already loaded" marker — so a genuinely finished session leaves nothing behind. Called by a
    /// host on session close (the counterpart to [`ensure_session_loaded`](Self::ensure_session_loaded));
    /// `clear_turn` already bounds growth at the turn boundary, but this reclaims immediately.
    pub fn clear_session(&self, session_id: &str) {
        let mut st = self.state.lock().unwrap();
        st.turns.remove(session_id);
        st.sessions.remove(session_id);
        st.loaded_sessions.remove(session_id);
    }

    pub fn validate_registration(
        &self,
        scope: CompositeScope,
        session_id: &str,
        decl: &CompositeOpDecl,
        replace: bool,
        tools: &ToolRegistry,
    ) -> Result<()> {
        validate_name(&decl.name)?;
        let candidate = {
            let st = self.state.lock().unwrap();
            let target_exists = target_contains(&st, scope, session_id, &decl.name);
            let active_exists = active_map_from_state(&st, session_id).contains_key(&decl.name);
            if (target_exists || active_exists) && !replace {
                return Err(Error::Other(format!(
                    "op.register: op `{}` already exists; pass replace=true to shadow or replace it",
                    decl.name
                )));
            }
            let mut active = active_map_from_state(&st, session_id);
            active.remove(&decl.name);
            active.insert(decl.name.clone(), Entry { decl: decl.clone() });
            active.into_values().map(|e| e.decl).collect::<Vec<_>>()
        };
        validate_composites(&candidate, tools)
    }

    pub fn install(
        &self,
        scope: CompositeScope,
        session_id: &str,
        decl: CompositeOpDecl,
        replace: bool,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        let target = target_map_mut(&mut st, scope, session_id);
        if target.contains_key(&decl.name) && !replace {
            return Err(Error::Other(format!(
                "op.register: op `{}` already exists in {} scope",
                decl.name,
                scope.as_str()
            )));
        }
        target.insert(decl.name.clone(), Entry { decl });
        Ok(())
    }
}

pub fn prepare_registration(
    request: CompositeRegisterRequest,
) -> Result<(CompositeScope, CompositeOpDecl, String, bool)> {
    let scope = CompositeScope::parse(&request.scope)?;
    let mut decl = parse_one_composite(&request.source)?;
    if let Some(expose) = request.expose {
        decl.meta.expose = expose;
    }
    validate_name(&decl.name)?;
    let source = flux_lang::format::format_composite_op(&decl);
    Ok((scope, decl, source, request.replace))
}

fn load_dir(system: &System, dir: &str) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    for (path, source) in system.read_dir_text_files(dir, "flux")? {
        let decl =
            parse_one_composite(&source).map_err(|e| Error::Other(format!("{path}: {e}")))?;
        if out.contains_key(&decl.name) {
            return Err(Error::Other(format!(
                "{path}: duplicate persisted composite op `{}`",
                decl.name
            )));
        }
        out.insert(decl.name.clone(), Entry { decl });
    }
    Ok(out)
}

/// Merge `more` into `out`, keeping the entry already present (first-seen wins → precedence order).
fn merge_entries(out: &mut BTreeMap<String, Entry>, more: BTreeMap<String, Entry>) {
    for (name, entry) in more {
        out.entry(name).or_insert(entry);
    }
}

/// Lenient loader for the unified `.flux/flows` home: a file may hold a bare flow, one op, or a
/// whole module — register every top-level `op` and ignore flows/other declarations. Unparseable
/// files are skipped (they stay runnable via `flow_run`, which surfaces the parse error), so a
/// single malformed file never breaks startup.
fn load_flows_dir(system: &System, dir: &str) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    for (_path, source) in system.read_dir_text_files(dir, "flux")? {
        let Ok(Module::Program(program)) = Module::parse_str(&source) else {
            continue;
        };
        for decl in program.ops {
            out.entry(decl.name.clone()).or_insert(Entry { decl });
        }
    }
    Ok(out)
}

fn parse_one_composite(source: &str) -> Result<CompositeOpDecl> {
    let module = Module::parse_str(source)
        .map_err(|e| Error::Other(format!("invalid composite op source: {e}")))?;
    let Module::Program(program) = module else {
        return Err(Error::Other(
            "composite op source must contain exactly one top-level `op` declaration".into(),
        ));
    };
    let has_only_one_op = program.ops.len() == 1
        && program.agents.is_empty()
        && program.channels.is_empty()
        && program.datasources.is_empty()
        && program.triggers.is_empty()
        && program.journeys.is_empty()
        && program.flows.is_empty();
    if !has_only_one_op {
        return Err(Error::Other(
            "composite op source must contain exactly one top-level `op` declaration and no other \
             module declarations"
                .into(),
        ));
    }
    Ok(program.ops.into_iter().next().expect("checked len"))
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "op.register: composite op name `{name}` is not filename-safe"
        )))
    }
}

fn validate_composites(composites: &[CompositeOpDecl], tools: &ToolRegistry) -> Result<()> {
    analyze_composites(composites, tools).map_err(|diags| {
        let messages = diags
            .into_iter()
            .map(|d| d.message)
            .collect::<Vec<_>>()
            .join("; ");
        Error::Other(format!("composite validation failed: {messages}"))
    })
}

fn active_from_state(st: &State, session_id: &str) -> Vec<CompositeOpDecl> {
    active_map_from_state(st, session_id)
        .into_values()
        .map(|e| e.decl)
        .collect()
}

fn active_map_from_state(st: &State, session_id: &str) -> BTreeMap<String, Entry> {
    let mut out = BTreeMap::new();
    extend_entries(&mut out, &st.global);
    extend_entries(&mut out, &st.project);
    if let Some(session) = st.sessions.get(session_id) {
        extend_entries(&mut out, session);
    }
    if let Some(turn) = st.turns.get(session_id) {
        extend_entries(&mut out, turn);
    }
    out
}

fn extend_entries(out: &mut BTreeMap<String, Entry>, entries: &BTreeMap<String, Entry>) {
    for (name, entry) in entries {
        out.insert(name.clone(), entry.clone());
    }
}

fn target_contains(st: &State, scope: CompositeScope, session_id: &str, name: &str) -> bool {
    match scope {
        CompositeScope::Global => st.global.contains_key(name),
        CompositeScope::Project => st.project.contains_key(name),
        CompositeScope::Session => st
            .sessions
            .get(session_id)
            .is_some_and(|entries| entries.contains_key(name)),
        CompositeScope::Turn => st
            .turns
            .get(session_id)
            .is_some_and(|entries| entries.contains_key(name)),
    }
}

fn target_map_mut<'a>(
    st: &'a mut State,
    scope: CompositeScope,
    session_id: &str,
) -> &'a mut BTreeMap<String, Entry> {
    match scope {
        CompositeScope::Global => &mut st.global,
        CompositeScope::Project => &mut st.project,
        CompositeScope::Session => st.sessions.entry(session_id.to_string()).or_default(),
        CompositeScope::Turn => st.turns.entry(session_id.to_string()).or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str) -> CompositeOpDecl {
        CompositeOpDecl {
            name: name.into(),
            ..Default::default()
        }
    }

    /// C-87: `clear_turn` bounds the per-session composite caches on a long-lived shared engine — it
    /// retains only the just-active session's loaded definitions and evicts every other session's, so
    /// the maps can't grow one-entry-per-session-forever.
    #[test]
    fn clear_turn_bounds_session_caches_to_active() {
        let dc = DynamicComposites::default();
        dc.install(CompositeScope::Session, "a", decl("op_a"), false)
            .unwrap();
        dc.install(CompositeScope::Session, "b", decl("op_b"), false)
            .unwrap();
        assert!(!dc.active_for_session("a").is_empty());
        assert!(!dc.active_for_session("b").is_empty());

        dc.clear_turn("a");
        assert!(
            !dc.active_for_session("a").is_empty(),
            "the just-active session's composites survive the turn boundary"
        );
        assert!(
            dc.active_for_session("b").is_empty(),
            "another session's cached composites are evicted (bounded growth)"
        );
    }

    /// C-87: `clear_session` reclaims a finished session's composite caches immediately.
    #[test]
    fn clear_session_drops_the_entry() {
        let dc = DynamicComposites::default();
        dc.install(CompositeScope::Session, "a", decl("op_a"), false)
            .unwrap();
        dc.install(CompositeScope::Turn, "a", decl("op_turn"), false)
            .unwrap();
        assert_eq!(dc.active_for_session("a").len(), 2);
        dc.clear_session("a");
        assert!(
            dc.active_for_session("a").is_empty(),
            "session and turn scoped composites are both dropped on session end"
        );
    }
}
