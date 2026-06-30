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
        let global = if system.workspace().has_named_root("global_ops") {
            load_dir(system, "@global_ops")?
        } else {
            BTreeMap::new()
        };
        let project = load_dir(system, ".flux/ops")?;
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
        self.state.lock().unwrap().turns.remove(session_id);
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
