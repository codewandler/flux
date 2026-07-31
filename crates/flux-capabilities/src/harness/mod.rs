//! Local coding-harness state: which harnesses exist, where each one keeps its state, and how to
//! walk that state without falling over.
//!
//! This is the acquisition layer shared by everything that reads another harness's local files.
//! [`scan`] answers *"where does this harness keep its state, and how do I iterate it"*; the
//! per-harness adapters answer *"what was said"* ([`HarnessMessage`], C-214). `flux usage` keeps
//! its own token-shaped projection on top of the same scan — two consumers, two projections, one
//! scan.
//!
//! Three properties are contracts rather than implementation details:
//!
//! - **Read-only, always.** No adapter may write another harness's state — [`open_sqlite_read_only`]
//!   is the only database entry point here, and it passes `SQLITE_OPEN_READ_ONLY`.
//! - **The scan budget degrades, it does not fail.** Over-budget input is skipped *and counted*
//!   (see [`ScanBudget`]); the state itself failing is what propagates as an error — an unreadable
//!   root, or a database that stops yielding rows mid-walk and would otherwise leave a truncated
//!   scan indistinguishable from a complete one.
//! - **Messages stream.** Message extraction never returns a collection: a message record carries
//!   full text where a usage record carries eight integers, so materializing a multi-year history
//!   is not a thing this layer is allowed to do. Adapters push into a [`MessageSink`].

mod claude;
mod codex;
mod message;
mod opencode;
mod scan;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub use claude::claude_messages;
pub use codex::codex_messages;
pub use message::{HarnessMessage, MessageRole, MessageSink, MessageStats};
pub use opencode::opencode_messages;
pub use scan::{
    jsonl_files, open_jsonl, open_sqlite_read_only, sqlite_column_exists, sqlite_table_exists,
    JsonlLine, JsonlLines, JsonlScan, ScanBudget, SkipReason, MAX_JSONL_FILES,
    MAX_JSONL_FILE_BYTES, MAX_JSONL_LINE_BYTES, MAX_MESSAGES, MAX_MESSAGE_BYTES,
    MAX_MESSAGE_TOTAL_BYTES,
};

/// A coding harness whose local state flux can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HarnessKind {
    /// flux itself, via its event store.
    Flux,
    /// OpenAI Codex CLI — JSONL rollouts.
    Codex,
    /// Claude Code — JSONL project transcripts.
    Claude,
    /// opencode — a SQLite database.
    Opencode,
}

impl HarnessKind {
    /// Every harness, in the order surfaces present them.
    pub const ALL: [HarnessKind; 4] = [
        HarnessKind::Flux,
        HarnessKind::Codex,
        HarnessKind::Claude,
        HarnessKind::Opencode,
    ];

    /// The stable machine identifier, as it appears in JSON output and record metadata.
    pub fn id(self) -> &'static str {
        match self {
            HarnessKind::Flux => "flux",
            HarnessKind::Codex => "codex",
            HarnessKind::Claude => "claude-code",
            HarnessKind::Opencode => "opencode",
        }
    }

    /// The human label, as the harness spells its own name.
    pub fn label(self) -> &'static str {
        match self {
            HarnessKind::Flux => "flux",
            HarnessKind::Codex => "Codex",
            HarnessKind::Claude => "Claude Code",
            HarnessKind::Opencode => "opencode",
        }
    }

    /// Parse an [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<Self> {
        HarnessKind::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The environment key whose value overrides this harness's root, if it has one.
    pub fn env_key(self) -> &'static str {
        match self {
            HarnessKind::Flux => "FLUX_HOME",
            HarnessKind::Codex => "CODEX_HOME",
            HarnessKind::Claude => "CLAUDE_CONFIG_DIR",
            HarnessKind::Opencode => "OPENCODE_DATA_DIR",
        }
    }

    /// Where this harness's state *would* live, whether or not it is actually there.
    ///
    /// The precedence is the same for all four and is the point of this function existing:
    /// the [`env_key`](Self::env_key) override wins, then the `~` default. `None` means neither is
    /// available — no override and no `HOME` — so there is no path to even look at.
    pub fn state_path(self, env: &HarnessEnv) -> Option<PathBuf> {
        let root = env.path(self.env_key()).or_else(|| match self {
            HarnessKind::Flux => env.home().map(|h| h.join(".flux")),
            HarnessKind::Codex => env.home().map(|h| h.join(".codex")),
            HarnessKind::Claude => env.home().map(|h| h.join(".claude")),
            HarnessKind::Opencode => env
                .home()
                .map(|h| h.join(".local").join("share").join("opencode")),
        })?;
        Some(match self {
            HarnessKind::Flux => root.join("events.db"),
            HarnessKind::Codex => root.join("sessions"),
            HarnessKind::Claude => root.join("projects"),
            HarnessKind::Opencode => root.join("opencode.db"),
        })
    }

    /// Resolve this harness's state path and check whether it is present.
    pub fn locate(self, env: &HarnessEnv) -> HarnessLocation {
        match self.state_path(env) {
            Some(path) if path.exists() => HarnessLocation::Found(path),
            Some(path) => HarnessLocation::NotFound(path),
            None => HarnessLocation::Unresolved,
        }
    }
}

/// The outcome of discovery, as a value rather than a rendered message.
///
/// Surfaces choose their own wording for each case; nothing here decides how a miss is phrased.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HarnessLocation {
    /// The state path resolved and exists.
    Found(PathBuf),
    /// The state path resolved but nothing is there. The path is kept so a surface can report
    /// *where* it looked.
    NotFound(PathBuf),
    /// Nothing to look at: no environment override and no home directory.
    Unresolved,
}

impl HarnessLocation {
    /// The resolved path, for both the found and the absent case.
    pub fn path(&self) -> Option<&Path> {
        match self {
            HarnessLocation::Found(path) | HarnessLocation::NotFound(path) => Some(path),
            HarnessLocation::Unresolved => None,
        }
    }

    /// The path only when the state is actually present.
    pub fn found(&self) -> Option<&Path> {
        match self {
            HarnessLocation::Found(path) => Some(path),
            _ => None,
        }
    }
}

/// The environment discovery reads: the per-harness overrides plus `HOME`.
///
/// Held as a value rather than read from `std::env` at each site so precedence is testable without
/// mutating process-global state — a shared mutation that is racy across parallel tests and, from
/// Rust 2024 on, `unsafe`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessEnv {
    vars: BTreeMap<String, OsString>,
}

impl HarnessEnv {
    /// Every key discovery consults. Nothing outside this list influences where a harness is
    /// looked for.
    pub const KEYS: [&'static str; 5] = [
        "HOME",
        "FLUX_HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "OPENCODE_DATA_DIR",
    ];

    /// Snapshot [`Self::KEYS`] from the process environment.
    pub fn from_process() -> Self {
        let mut env = Self::default();
        for key in Self::KEYS {
            if let Some(value) = std::env::var_os(key) {
                env.vars.insert(key.to_string(), value);
            }
        }
        env
    }

    /// An environment with nothing set: no override and no home.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Set one key. An empty value is stored as-is and read as unset, matching how the process
    /// environment is interpreted — an exported-but-blank override must not resolve to `""`.
    pub fn with(mut self, key: impl Into<String>, value: impl AsRef<OsStr>) -> Self {
        self.vars.insert(key.into(), value.as_ref().to_os_string());
        self
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        self.vars
            .get(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    fn home(&self) -> Option<PathBuf> {
        self.path("HOME")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_labels_round_trip() {
        for kind in HarnessKind::ALL {
            assert_eq!(HarnessKind::from_id(kind.id()), Some(kind));
            assert!(!kind.label().is_empty());
        }
        assert_eq!(
            HarnessKind::from_id("claude"),
            None,
            "the id is claude-code"
        );
    }

    #[test]
    fn state_paths_hang_off_the_override_root_exactly_as_they_hang_off_home() {
        let env = HarnessEnv::empty()
            .with("HOME", "/home/u")
            .with("CODEX_HOME", "/elsewhere/codex");
        assert_eq!(
            HarnessKind::Codex.state_path(&env),
            Some(PathBuf::from("/elsewhere/codex/sessions"))
        );
        assert_eq!(
            HarnessKind::Claude.state_path(&env),
            Some(PathBuf::from("/home/u/.claude/projects"))
        );
        assert_eq!(
            HarnessKind::Opencode.state_path(&env),
            Some(PathBuf::from("/home/u/.local/share/opencode/opencode.db"))
        );
        assert_eq!(
            HarnessKind::Flux.state_path(&env),
            Some(PathBuf::from("/home/u/.flux/events.db"))
        );
        assert_eq!(HarnessKind::Flux.state_path(&HarnessEnv::empty()), None);
    }
}
