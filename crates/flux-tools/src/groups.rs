//! Built-in tool groups (the manifest mapping evidence signals → which ops surface).
//!
//! The generic workspace probe lives in the runtime ([`flux_runtime::detect_signals`]); this module
//! only declares which built-in ops belong to which group and the signal that surfaces each. The
//! group **owns its membership** (`tools`), so no op needs to know it is gated. The runtime resolver
//! ([`flux_evidence::resolve_active_groups`]) turns the current signals into the active group set.

use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};

/// One `surface_when` predicate matching the named `project.signal`.
fn when(signal: &str) -> Vec<SignalMatch> {
    vec![SignalMatch {
        kind: KIND_SIGNAL.into(),
        signal: Some(signal.into()),
    }]
}

/// The built-in tool groups and the signals that surface them. `git` is the live gated group; the
/// language groups (`go`/`node`/`python`/`rust`) currently bundle no ops — they establish the
/// mechanism and are filled as language tools land. The `eval` group is contributed separately by
/// `flux-eval` (co-located with those ops). Signal strings here are the contract with
/// [`flux_runtime::detect_signals`].
pub fn builtin_groups() -> Vec<ToolGroup> {
    let names = |ns: &[&str]| ns.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        ToolGroup {
            name: "git".into(),
            description: "Git version-control operations.".into(),
            tools: names(&[
                "git_stage",
                "git_commit",
                "git_status",
                "git_diff",
                "git_log",
                "git_push",
                "git_checkout",
                "git_unstage",
            ]),
            surface_when: when("git_repo"),
        },
        ToolGroup {
            name: "go".into(),
            description: "Go toolchain operations.".into(),
            tools: Vec::new(),
            surface_when: when("go"),
        },
        ToolGroup {
            name: "rust".into(),
            description: "Rust toolchain operations.".into(),
            tools: Vec::new(),
            surface_when: when("rust"),
        },
        ToolGroup {
            name: "node".into(),
            description: "Node.js toolchain operations.".into(),
            tools: Vec::new(),
            surface_when: when("node"),
        },
        ToolGroup {
            name: "python".into(),
            description: "Python toolchain operations.".into(),
            tools: Vec::new(),
            surface_when: when("python"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_groups_map_git_to_git_repo_signal() {
        let g = builtin_groups();
        let git = g.iter().find(|g| g.name == "git").unwrap();
        assert!(git.tools.contains(&"git_status".to_string()));
        assert_eq!(git.surface_when[0].signal.as_deref(), Some("git_repo"));
    }
}
