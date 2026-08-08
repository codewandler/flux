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

/// C-544 — how many words of an operator's description become the authored loop's profile name.
const PROFILE_WORDS: usize = 4;

/// Columns the generated description block wraps at, so the saved file reads as prose rather than
/// as one very long comment line.
const DESCRIPTION_WRAP: usize = 88;

/// Words a “create this … loop for me” prompt opens with that say nothing about the loop itself.
/// Dropping them keeps the derived profile name about the work rather than about the request.
const PROMPT_FILLER: &[&str] = &[
    "a", "an", "author", "build", "create", "flux", "for", "loop", "make", "me", "my", "new",
    "please", "that", "the", "this", "which", "write",
];

/// The profile name a description becomes: its first few meaningful words, hyphenated — a valid
/// flow name, and the same `profile@revision` identity the selector and header then show. `None`
/// when the description holds no word that can open a flow name.
fn profile_from_prompt(prompt: &str) -> Option<String> {
    let words: Vec<String> = prompt
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_ascii_lowercase())
        .filter(|word| !PROMPT_FILLER.contains(&word.as_str()))
        .skip_while(|word| {
            !word
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
        })
        .take(PROFILE_WORDS)
        .collect();
    (!words.is_empty()).then(|| words.join("-"))
}

/// The description as one Flux string literal. Quote, backslash and brace characters are dropped
/// rather than escaped: the brief is the operator's prose, and a generated program must not depend
/// on their punctuation lexing the way they meant it.
fn brief_literal(description: &str) -> String {
    description
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .filter(|character| !matches!(character, '"' | '\\' | '{' | '}'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The description wrapped into the comment lines [`description_of`] reads back — this is what the
/// C-543 overlay renders as the loop's description.
fn comment_block(description: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in description.split_whitespace() {
        if !current.is_empty()
            && current.chars().count() + 1 + word.chars().count() > DESCRIPTION_WRAP
        {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Generate the loop's source from the operator's description.
///
/// The program is the documented minimal custom loop (`docs/agent-loop.md`): the description leads
/// as the comment block the overlay shows, and the same text is bound and observed as the loop's
/// brief, so the prompt is part of the program rather than a comment about it. What comes out is
/// ordinary Flux-Lang — the operator owns the file from here and can edit it.
fn generate_loop_source(profile: &str, description: &str) -> String {
    let mut source = String::new();
    for line in comment_block(description) {
        source.push_str(&format!("# {line}\n"));
    }
    source.push_str("#\n");
    source
        .push_str("# Authored from an operator prompt. It is ordinary Flux-Lang: edit this file\n");
    source.push_str("# to refine the loop — the selector rescans on every open.\n\n");
    source.push_str(&format!("flow {profile} -> string\n"));
    source.push_str(&format!(
        "  $brief = fmt(\"{}\")\n",
        brief_literal(description)
    ));
    source.push_str("  observe({ kind: \"loop.brief\", data: $brief })\n");
    source.push_str("  $intent = detect_intent()\n");
    source.push_str("  $step = explore({ state: $intent.state })\n");
    source.push_str("  $answer = present_results({ step: $step })\n");
    source.push_str("  return $answer\n");
    source
}

/// C-544 — author a loop from the operator's description and save it as a `*.flux` file.
pub(crate) fn create_loop(dirs: &[PathBuf], prompt: &str) -> Result<LoopEntry, String> {
    create_loop_with(dirs, prompt, generate_loop_source)
}

/// The authoring path, with generation left to `generate` — the seam that keeps *producing* a loop
/// separate from *admitting* one. Whatever produced the source, it reaches disk only after it has
/// passed the exact load-time validation a hand-written loop passes at selection: it parses, and it
/// resolves into a real [`AgentLoopBinding`] with profile, revision, entry point and source digest.
/// An invalid generation returns its error and writes nothing, and an existing loop file is never
/// overwritten — the operator's own loops are theirs.
fn create_loop_with(
    dirs: &[PathBuf],
    prompt: &str,
    generate: impl Fn(&str, &str) -> String,
) -> Result<LoopEntry, String> {
    let description = prompt.trim();
    if description.is_empty() {
        return Err(
            "say what the loop should do, for example `/loop triage inbound support mail`"
                .to_string(),
        );
    }
    let profile = profile_from_prompt(description).ok_or_else(|| {
        format!("no loop name could be derived from `{description}`; describe what it should do")
    })?;
    let dir = dirs
        .first()
        .ok_or_else(|| "this surface has no loop directory to author into".to_string())?;
    let path = dir.join(format!("{profile}.{LOOP_FILE_EXTENSION}"));
    if path.exists() {
        return Err(format!(
            "loop `{profile}` already exists at {}; edit that file or describe a different loop",
            path.display()
        ));
    }

    let source = generate(&profile, description);
    let entry_point = match AgentLoopSpec::parse(&source) {
        Ok(AgentLoopSpec::Flux(ast)) => ast.name.clone().unwrap_or_else(|| profile.clone()),
        Ok(AgentLoopSpec::Builtin(_)) => profile.clone(),
        Err(error) => return Err(format!("the generated loop is not valid flux: {error}")),
    };
    let binding = AgentLoopBinding::native_flux(
        profile.clone(),
        FILE_LOOP_REVISION,
        format!("file:{}", path.display()),
        entry_point,
        source,
    )
    .map_err(|error| format!("the generated loop is not valid flux: {error}"))?;

    std::fs::create_dir_all(dir).map_err(|error| format!("create `{}`: {error}", dir.display()))?;
    std::fs::write(&path, binding.source())
        .map_err(|error| format!("write `{}`: {error}", path.display()))?;
    Ok(LoopEntry {
        profile,
        revision: FILE_LOOP_REVISION.to_string(),
        source: LoopSource::File(path),
    })
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

    /// C-544 — author a loop from the operator's description and offer it immediately.
    ///
    /// On success the selector reopens over a fresh scan with the new loop under the cursor, so the
    /// loop just described is one `Enter` away — the same rescan C-543 already performs, now with a
    /// file it has just been given. Authoring is not an admission: it writes a file and changes
    /// nothing about the loop this session already runs, and an admitted session still takes the
    /// explicit re-admission path in [`ChatState::choose_loop`]. A refusal — an unusable
    /// description, an existing file, an invalid generation — is surfaced in the overlay and
    /// returned, never silently saved.
    pub(crate) fn create_loop_from_prompt(&mut self, prompt: &str) -> Result<LoopEntry, String> {
        let dirs = self.loop_dirs.clone();
        match create_loop(&dirs, prompt) {
            Ok(entry) => {
                self.open_loop_selector();
                if let Some(selector) = self.loop_selector.as_mut() {
                    // The fresh open has an empty query, so the filtered view is `entries` itself
                    // and the entry's index is the cursor position.
                    if let Some(index) = selector.entries.iter().position(|open| *open == entry) {
                        selector.sel = index;
                    }
                }
                Ok(entry)
            }
            Err(reason) => {
                self.loop_overlay = Some(LoopOverlay::refused("new loop", &reason));
                Err(reason)
            }
        }
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

    /// C-544 — describing a loop authors it: flux writes the `*.flux` file, the file is valid flux,
    /// and it carries the operator's description plus stable C-569 identity.
    #[test]
    fn a_described_loop_is_generated_validated_and_saved() {
        let (mut state, dir) = state_with_loops("create");

        let entry = state
            .create_loop_from_prompt(
                "create a loop for me that reviews documentation and never edits code",
            )
            .expect("a described loop is authored");

        let LoopSource::File(path) = &entry.source else {
            panic!("a created loop is a file: {entry:?}");
        };
        assert!(path.starts_with(&dir), "{}", path.display());
        assert!(entry.profile.contains("documentation"), "{entry:?}");
        assert_eq!(entry.label(), format!("{}@1", entry.profile));

        let saved = std::fs::read_to_string(path).expect("the authored loop was saved");
        // The generated file passes the same load-time validation a hand-written loop passes.
        assert!(AgentLoopSpec::parse(&saved).is_ok(), "{saved}");
        // The description the C-543 overlay renders is derived from the operator's prompt.
        let described = description_of(&saved).join(" ");
        assert!(described.contains("reviews documentation"), "{described}");

        // Stable profile/revision/source-digest metadata for C-569 resolution.
        let binding = entry.resolve().expect("the saved loop resolves");
        let metadata = binding.metadata();
        assert_eq!(metadata.profile, entry.profile);
        assert_eq!(metadata.revision, "1");
        assert!(metadata.source_ref.contains(&entry.profile), "{metadata:?}");
        assert_eq!(
            metadata.source_sha256,
            entry.resolve().unwrap().metadata().source_sha256,
            "the saved loop's digest is stable"
        );
    }

    /// The authored loop is in the C-543 selector immediately — no restart, no rescan by hand — and
    /// can be selected and run.
    #[test]
    fn a_created_loop_is_immediately_selectable() {
        let (mut state, _dir) = state_with_loops("created-selectable");

        let entry = state
            .create_loop_from_prompt("a loop that triages inbound support mail")
            .expect("a described loop is authored");

        let index = entry_index(&state, &entry.label());
        match state.choose_loop(index) {
            LoopSwitch::Adopt(binding) => assert_eq!(binding.metadata().profile, entry.profile),
            LoopSwitch::Refused(reason) => panic!("a fresh session may run a new loop: {reason}"),
        }

        let header = header_text(&state, 200);
        assert!(
            header.contains(&format!("loop {}", entry.label())),
            "{header}"
        );
    }

    /// An invalid generation is refused with its error surfaced and never silently saved.
    #[test]
    fn an_invalid_generation_is_refused_and_never_saved() {
        let dir = temp_loop_dir("refused");
        let dirs = vec![dir.clone()];

        let error = create_loop_with(&dirs, "review the docs every turn", |_, description| {
            format!("# {description}\nthis is not a flux program\n")
        })
        .expect_err("an invalid generation is refused");

        assert!(error.contains("not valid flux"), "{error}");
        assert_eq!(
            std::fs::read_dir(&dir).map(Iterator::count).unwrap_or(0),
            0,
            "an invalid generation is never saved"
        );
    }

    /// Authoring a loop is not an admission: it does not alter the loop an admitted session already
    /// runs, and it never overwrites an operator's existing file.
    #[test]
    fn authoring_never_alters_an_admitted_session_or_overwrites_a_loop() {
        let (mut state, _dir) = state_with_loops("admitted-create");
        state.set_loop_admitted(Some(builtin().metadata().clone()));

        let entry = state
            .create_loop_from_prompt("a loop that summarizes the day's commits")
            .expect("authoring a loop is allowed while a session runs");

        let header = header_text(&state, 200);
        assert!(header.contains("loop adaptive@1"), "{header}");
        let index = entry_index(&state, &entry.label());
        assert!(
            matches!(state.choose_loop(index), LoopSwitch::Refused(_)),
            "an admitted session still takes the explicit re-admission path"
        );

        let error = state
            .create_loop_from_prompt("a loop that summarizes the day's commits")
            .expect_err("an existing loop file is never overwritten");
        assert!(error.contains("already exists"), "{error}");
        let overlay = state.loop_overlay.as_ref().expect("a refusal overlay");
        assert!(
            overlay
                .refusal
                .as_deref()
                .is_some_and(|reason| reason.contains("already exists")),
            "{overlay:?}"
        );
    }
}
