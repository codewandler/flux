//! C-543 — the agent's loop as a visible, switchable thing in the TUI.
//!
//! The loop this surface's agent runs is a *resolved binding* (C-569), not an ambient filename: the
//! header shows the profile, revision and abbreviated digest the engine actually admitted, the
//! selector lists the `*.flux` loops discoverable right now, and choosing one goes through the same
//! lifecycle rule the engine enforces — a session that has already admitted a binding is not
//! silently switched.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

use flux_core::AgentLoopBindingMetadata;
use flux_flow::engine::{AgentLoopBinding, AgentLoopSpec};
use flux_flow::render::{render_statement, Palette};

use crate::ChatState;

/// Extension every discoverable authored loop carries.
const LOOP_FILE_EXTENSION: &str = "flux";

/// Revision namespace of a loop discovered as a file. A file carries no revision of its own, so
/// every discovery is revision `1` of its profile; the digest the header shows stays the authority
/// for the exact bytes (C-569).
const FILE_LOOP_REVISION: &str = "1";

/// Description rows the selection overlay renders.
const OVERLAY_DESCRIPTION_ROWS: usize = 4;

/// Outer statements the selection overlay visualizes. The overlay is deliberately short: it shows
/// the *outer* loop, not the whole program — a full tree would bury the shape it exists to show.
const OVERLAY_STRUCTURE_ROWS: usize = 12;

/// Where an entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopSource {
    /// Flux's shipped, versioned preset — always offered, so an operator can always get back to it.
    Builtin,
    /// An operator-authored `*.flux` file found under one of the surface's loop directories.
    File(PathBuf),
}

/// One selectable loop, as the selector lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopEntry {
    pub(crate) profile: String,
    pub(crate) revision: String,
    pub(crate) source: LoopSource,
}

impl LoopEntry {
    /// `profile@revision` — the same identity pair the header shows, so the row an operator picks
    /// and the loop the header then names are visibly one thing.
    pub(crate) fn label(&self) -> String {
        format!("{}@{}", self.profile, self.revision)
    }

    /// Short provenance for the row: a loop's *file* is where it came from, never its identity.
    pub(crate) fn origin(&self) -> String {
        match &self.source {
            LoopSource::Builtin => "built in".to_string(),
            LoopSource::File(path) => path.display().to_string(),
        }
    }

    /// Resolve the entry into a binding the engine can admit. Reading and parsing happen here, at
    /// selection time, so an unreadable or unparseable file refuses with its exact error instead of
    /// disappearing from the list.
    fn resolve(&self) -> Result<AgentLoopBinding, String> {
        match &self.source {
            LoopSource::Builtin => Ok(AgentLoopBinding::from_spec(AgentLoopSpec::default())),
            LoopSource::File(path) => {
                let source = std::fs::read_to_string(path)
                    .map_err(|error| format!("read `{}`: {error}", path.display()))?;
                let entry_point = match AgentLoopSpec::parse(&source) {
                    Ok(AgentLoopSpec::Flux(ast)) => {
                        ast.name.clone().unwrap_or_else(|| self.profile.clone())
                    }
                    Ok(AgentLoopSpec::Builtin(_)) => self.profile.clone(),
                    Err(error) => return Err(error.to_string()),
                };
                AgentLoopBinding::native_flux(
                    self.profile.clone(),
                    self.revision.clone(),
                    format!("file:{}", path.display()),
                    entry_point,
                    source,
                )
                .map_err(|error| error.to_string())
            }
        }
    }
}

/// Where the surface looks for authored loops: the workspace's own `.flux/loops` directory, the
/// same location the Fleet loop profiles in `docs/designs/agent-loop-harnesses.md` name.
pub(crate) fn loop_dirs_for(cwd: &Path) -> Vec<PathBuf> {
    vec![cwd.join(".flux").join("loops")]
}

/// The live set of loops, rescanned on every selector open — a loop authored while the TUI runs
/// (C-544) appears without a restart, because nothing here is cached across opens.
///
/// This is the same surface-owned, operator-initiated directory read as C-112's `@` path
/// completion: no agent effect crosses it and nothing it finds is dispatched.
fn discover(dirs: &[PathBuf]) -> Vec<LoopEntry> {
    let builtin = AgentLoopBinding::from_spec(AgentLoopSpec::default());
    let mut entries = vec![LoopEntry {
        profile: builtin.metadata().profile.clone(),
        revision: builtin.metadata().revision.clone(),
        source: LoopSource::Builtin,
    }];
    let mut files: Vec<LoopEntry> = Vec::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in read.flatten() {
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(LOOP_FILE_EXTENSION) {
                continue;
            }
            let Some(profile) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if profile.is_empty() {
                continue;
            }
            files.push(LoopEntry {
                profile: profile.to_string(),
                revision: FILE_LOOP_REVISION.to_string(),
                source: LoopSource::File(path.clone()),
            });
        }
    }
    files.sort_by_key(LoopEntry::label);
    files.dedup_by_key(|entry| entry.label());
    entries.extend(files);
    entries
}

/// The open loop selector: the rescanned entry set plus this open's cursor and filter.
#[derive(Debug)]
pub(crate) struct LoopSelector {
    pub(crate) entries: Vec<LoopEntry>,
    /// Cursor into the *filtered* view, not into `entries`.
    pub(crate) sel: usize,
    pub(crate) query: String,
}

impl LoopSelector {
    fn open(dirs: &[PathBuf]) -> Self {
        Self {
            entries: discover(dirs),
            sel: 0,
            query: String::new(),
        }
    }

    /// Indices into `entries` that survive the filter: a case-insensitive prefix over the
    /// `profile@revision` label. A loop set is small and deliberately named, so a prefix keeps the
    /// filter's result obvious — a fuzzy subsequence would keep `adaptive` on screen for `p`.
    pub(crate) fn matches(&self) -> Vec<usize> {
        let query = self.query.trim().to_ascii_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty() || entry.label().to_ascii_lowercase().starts_with(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Move, filter, choose, close. Every movement clamps against the filtered view the renderer
    /// draws, so typing can never leave the cursor pointing at a row that is not on screen.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> LoopSelectorCommand {
        let visible = self.matches();
        let last = visible.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => LoopSelectorCommand::Close,
            KeyCode::Up => {
                self.sel = self.sel.min(last).saturating_sub(1);
                LoopSelectorCommand::None
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(last);
                LoopSelectorCommand::None
            }
            KeyCode::Enter => match visible.get(self.sel.min(last)) {
                Some(index) => LoopSelectorCommand::Choose(*index),
                None => LoopSelectorCommand::None,
            },
            KeyCode::Backspace => {
                self.query.pop();
                self.sel = 0;
                LoopSelectorCommand::None
            }
            KeyCode::Char(typed) => {
                self.query.push(typed);
                self.sel = 0;
                LoopSelectorCommand::None
            }
            _ => LoopSelectorCommand::None,
        }
    }
}

/// What one key press asked the surface to do with the selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopSelectorCommand {
    None,
    Close,
    /// Choose the entry at this index into [`LoopSelector::entries`].
    Choose(usize),
}

/// The outcome of choosing an entry.
///
/// `Adopt` carries the resolved binding for the caller to hand to the engine — the surface never
/// executes a loop itself. The binding is boxed: it carries the loop's admitted source, so keeping
/// it inline would make every `Refused(String)` pay for it too. `Refused` carries the exact reason,
/// which is also what the operator reads: a selection that cannot take effect says so instead of
/// appearing to have worked.
#[derive(Debug)]
pub(crate) enum LoopSwitch {
    Adopt(Box<AgentLoopBinding>),
    Refused(String),
}

/// The short overlay shown after a selection.
#[derive(Debug, Clone)]
pub(crate) struct LoopOverlay {
    /// `profile@revision · digest` of the chosen loop.
    pub(crate) title: String,
    /// The loop's own description, taken from its leading comment block.
    pub(crate) description: Vec<String>,
    /// The outer loop's structure: its flow header and top-level statements.
    pub(crate) structure: Vec<String>,
    /// Set when the selection was refused; the overlay then explains instead of visualizing.
    pub(crate) refusal: Option<String>,
}

impl LoopOverlay {
    fn selected(binding: &AgentLoopBinding) -> Self {
        let metadata = binding.metadata();
        Self {
            title: format!(
                "{}@{} · {}",
                metadata.profile,
                metadata.revision,
                short_digest(&metadata.source_sha256)
            ),
            description: description_of(binding.source()),
            structure: outer_structure(binding),
            refusal: None,
        }
    }

    fn refused(label: &str, reason: &str) -> Self {
        Self {
            title: label.to_string(),
            description: Vec::new(),
            structure: Vec::new(),
            refusal: Some(reason.to_string()),
        }
    }
}

/// The loop's description: its leading comment block, which is where an authored loop says what it
/// is for. Bounded, so a long preamble cannot push the structure off the overlay.
fn description_of(source: &str) -> Vec<String> {
    source
        .lines()
        .map(str::trim)
        .take_while(|line| line.is_empty() || line.starts_with('#'))
        .filter_map(|line| {
            let text = line.trim_start_matches('#').trim().to_string();
            (!text.is_empty()).then_some(text)
        })
        .take(OVERLAY_DESCRIPTION_ROWS)
        .collect()
}

/// The outer loop as a tree: the flow header plus one line per top-level statement, rendered by
/// Flux-Lang's own statement renderer so the overlay shows the program that will run rather than a
/// hand-written paraphrase of it. It does not recurse — the outer shape is the point.
fn outer_structure(binding: &AgentLoopBinding) -> Vec<String> {
    let ast = match binding.spec() {
        AgentLoopSpec::Flux(ast) => ast.clone(),
        // The shipped preset keeps its selector rather than an AST; its admitted source is exact,
        // so parsing that is the same program the engine loads.
        AgentLoopSpec::Builtin(_) => match AgentLoopSpec::parse(binding.source()) {
            Ok(AgentLoopSpec::Flux(ast)) => ast,
            _ => return Vec::new(),
        },
    };
    let name = ast
        .name
        .clone()
        .unwrap_or_else(|| binding.metadata().entry_point.clone());
    let mut lines = vec![format!("flow {name}")];
    let total = ast.body.len();
    for (index, node) in ast.body.iter().take(OVERLAY_STRUCTURE_ROWS).enumerate() {
        let connector = if index + 1 == total {
            "└─"
        } else {
            "├─"
        };
        lines.push(format!(
            "{connector} {}",
            render_statement(node, &Palette::PLAIN).replace('\n', " ")
        ));
    }
    if total > OVERLAY_STRUCTURE_ROWS {
        lines.push(format!(
            "… {} more outer statements",
            total - OVERLAY_STRUCTURE_ROWS
        ));
    }
    lines
}

/// The abbreviated digest the header and overlay show. Short enough for a bar, long enough to tell
/// two revisions of one profile apart at a glance; the full digest stays in the receipt.
fn short_digest(digest: &str) -> &str {
    &digest[..digest.len().min(8)]
}

impl ChatState {
    /// Record the binding the *next* start will run, exactly as the engine resolved it (C-569).
    pub(crate) fn set_loop_binding(&mut self, binding: Option<AgentLoopBindingMetadata>) {
        self.loop_binding = binding;
    }

    /// Record the binding this session has already admitted. From here the loop is that session's
    /// behavior admission: a selection can no longer change it silently.
    pub(crate) fn set_loop_admitted(&mut self, admitted: Option<AgentLoopBindingMetadata>) {
        if let Some(admitted) = &admitted {
            self.loop_binding = Some(admitted.clone());
        }
        self.loop_admitted = admitted;
    }

    /// The header's loop segment: the resolved profile, revision and abbreviated digest. An
    /// admitted session renders muted — the binding is settled until a new session — while a
    /// selectable one keeps the accent.
    pub(crate) fn loop_segment(&self) -> Option<Vec<Span<'static>>> {
        let binding = self.loop_binding.as_ref()?;
        let style = if self.loop_admitted.is_some() {
            self.theme.muted_style()
        } else {
            self.theme.accent_style()
        };
        Some(vec![Span::styled(
            format!(
                "loop {}@{} {}",
                binding.profile,
                binding.revision,
                short_digest(&binding.source_sha256)
            ),
            style,
        )])
    }

    /// Open the selector over a fresh scan of [`ChatState::loop_dirs`].
    pub(crate) fn open_loop_selector(&mut self) {
        self.loop_overlay = None;
        self.loop_selector = Some(LoopSelector::open(&self.loop_dirs));
    }

    pub(crate) fn close_loop_selector(&mut self) {
        self.loop_selector = None;
    }

    pub(crate) fn close_loop_overlay(&mut self) {
        self.loop_overlay = None;
    }

    /// Choose the entry at `index` into the open selector's entries.
    ///
    /// A session that already admitted a binding is refused here, before any resolution: C-569
    /// makes the first recorded turn binding that session's admission, so switching a started
    /// agent is a new-session/re-admission decision, never a silent one.
    pub(crate) fn choose_loop(&mut self, index: usize) -> LoopSwitch {
        let Some(entry) = self
            .loop_selector
            .as_ref()
            .and_then(|selector| selector.entries.get(index).cloned())
        else {
            return LoopSwitch::Refused("no such loop".to_string());
        };
        self.close_loop_selector();
        if let Some(admitted) = self.loop_admitted.clone() {
            let reason = format!(
                "this session already runs loop {}@{}; selecting {} takes effect in a new session \
                 (explicit re-admission), not in the running one",
                admitted.profile,
                admitted.revision,
                entry.label()
            );
            self.loop_overlay = Some(LoopOverlay::refused(&entry.label(), &reason));
            return LoopSwitch::Refused(reason);
        }
        match entry.resolve() {
            Ok(binding) => {
                self.loop_binding = Some(binding.metadata().clone());
                self.loop_overlay = Some(LoopOverlay::selected(&binding));
                LoopSwitch::Adopt(Box::new(binding))
            }
            Err(reason) => {
                self.loop_overlay = Some(LoopOverlay::refused(&entry.label(), &reason));
                LoopSwitch::Refused(reason)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    use crate::ChatState;

    /// An operator-authored loop with a leading description block and a two-statement outer body.
    const PICKED: &str = "# Picked loop — implement exactly the assigned contract.\n\
                          # Nothing here explores unrelated work.\n\
                          \n\
                          flow picked -> string\n  \
                          $note = fmt(\"read the assignment\")\n  \
                          return \"picked\"\n";

    fn temp_loop_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flux-tui-c543-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn builtin() -> flux_flow::engine::AgentLoopBinding {
        flux_flow::engine::AgentLoopBinding::from_spec(flux_flow::engine::AgentLoopSpec::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn header_text(state: &ChatState, width: u16) -> String {
        state
            .header_line(width)
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state_with_loops(tag: &str) -> (ChatState, PathBuf) {
        let dir = temp_loop_dir(tag);
        std::fs::write(dir.join("picked.flux"), PICKED).unwrap();
        let mut state = ChatState::new("mock".into());
        state.loop_dirs = vec![dir.clone()];
        state.set_loop_binding(Some(builtin().metadata().clone()));
        (state, dir)
    }

    fn entry_index(state: &ChatState, label: &str) -> usize {
        state
            .loop_selector
            .as_ref()
            .expect("selector open")
            .entries
            .iter()
            .position(|entry| entry.label() == label)
            .unwrap_or_else(|| panic!("no entry labelled {label}"))
    }

    /// The resolved binding is visible for the selected agent: profile, revision and an abbreviated
    /// digest, taken from what the engine admitted rather than from a filename on disk.
    #[test]
    fn the_header_shows_the_resolved_loop_profile_revision_and_digest() {
        let mut state = ChatState::new("mock".into());
        state.set_loop_binding(Some(builtin().metadata().clone()));

        let header = header_text(&state, 200);

        assert!(header.contains("loop adaptive@1"), "{header}");
        assert!(
            header.contains(&builtin().metadata().source_sha256[..8]),
            "{header}"
        );
    }

    /// The selector lists every discoverable `*.flux` loop and rescans on every open, so a loop
    /// authored while the TUI runs (C-544) appears without a restart.
    #[test]
    fn the_selector_reflects_the_live_set_of_loop_files() {
        let (mut state, dir) = state_with_loops("live");

        state.open_loop_selector();
        let first: Vec<String> = state
            .loop_selector
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.label())
            .collect();
        assert_eq!(
            first,
            vec!["adaptive@1".to_string(), "picked@1".to_string()]
        );

        std::fs::write(dir.join("second.flux"), PICKED.replace("picked", "second")).unwrap();
        state.close_loop_selector();
        state.open_loop_selector();
        let second: Vec<String> = state
            .loop_selector
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.label())
            .collect();

        assert!(second.contains(&"second@1".to_string()), "{second:?}");
    }

    /// Choosing an entry switches the selected agent's loop: the surface hands the resolved binding
    /// to the engine and the header immediately names the loop the next start runs.
    #[test]
    fn choosing_a_loop_switches_the_agents_next_start() {
        let (mut state, _dir) = state_with_loops("switch");
        state.open_loop_selector();
        let index = entry_index(&state, "picked@1");

        match state.choose_loop(index) {
            LoopSwitch::Adopt(binding) => {
                assert_eq!(binding.metadata().profile, "picked");
                assert_eq!(binding.metadata().entry_point, "picked");
            }
            LoopSwitch::Refused(reason) => panic!("an unstarted session may switch: {reason}"),
        }

        let header = header_text(&state, 200);
        assert!(header.contains("loop picked@1"), "{header}");
    }

    /// A session that already admitted a binding is never silently switched (C-569): the selection
    /// is refused and names the explicit new-session/re-admission path instead.
    #[test]
    fn an_admitted_session_refuses_a_silent_loop_switch() {
        let (mut state, _dir) = state_with_loops("admitted");
        state.set_loop_admitted(Some(builtin().metadata().clone()));
        state.open_loop_selector();
        let index = entry_index(&state, "picked@1");

        match state.choose_loop(index) {
            LoopSwitch::Adopt(_) => panic!("an admitted session must not switch silently"),
            LoopSwitch::Refused(reason) => {
                assert!(reason.contains("new session"), "{reason}");
                assert!(reason.contains("re-admission"), "{reason}");
            }
        }

        let header = header_text(&state, 200);
        assert!(header.contains("loop adaptive@1"), "{header}");
    }

    /// Selecting a loop shows a short overlay that visualizes the outer loop's structure and renders
    /// the loop's own description.
    #[test]
    fn the_selection_overlay_visualizes_the_outer_loop_and_its_description() {
        let (mut state, _dir) = state_with_loops("overlay");
        state.open_loop_selector();
        let index = entry_index(&state, "picked@1");
        let _ = state.choose_loop(index);

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        let content = screen(&terminal);

        assert!(content.contains("Picked loop"), "{content}");
        assert!(content.contains("flow picked"), "{content}");
        assert!(content.contains("$note"), "{content}");
        assert!(content.contains("return"), "{content}");
    }

    /// The selector's own key contract: move, filter, choose, close.
    #[test]
    fn the_selector_routes_movement_filtering_and_selection() {
        let (mut state, _dir) = state_with_loops("keys");
        state.open_loop_selector();
        let selector = state.loop_selector.as_mut().unwrap();

        assert!(matches!(
            selector.handle_key(key(KeyCode::Down)),
            LoopSelectorCommand::None
        ));
        assert_eq!(selector.sel, 1);
        assert!(matches!(
            selector.handle_key(key(KeyCode::Enter)),
            LoopSelectorCommand::Choose(1)
        ));

        selector.handle_key(key(KeyCode::Char('p')));
        assert_eq!(selector.query, "p");
        assert_eq!(selector.matches(), vec![1]);

        assert!(matches!(
            selector.handle_key(key(KeyCode::Esc)),
            LoopSelectorCommand::Close
        ));
    }
}
