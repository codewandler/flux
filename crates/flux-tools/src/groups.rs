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

/// A `surface_when` predicate matching ANY of the named `project.signal`s (the resolver OR-s the
/// `SignalMatch` list).
fn when_any(signals: &[&str]) -> Vec<SignalMatch> {
    signals
        .iter()
        .map(|s| SignalMatch {
            kind: KIND_SIGNAL.into(),
            signal: Some((*s).into()),
        })
        .collect()
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
            tools: names(&["go_build", "go_test", "go_vet"]),
            surface_when: when("go"),
        },
        ToolGroup {
            name: "rust".into(),
            description: "Rust toolchain operations.".into(),
            tools: names(&[
                "cargo_check",
                "cargo_build",
                "cargo_test",
                "cargo_clippy",
                "cargo_fmt",
            ]),
            surface_when: when("rust"),
        },
        ToolGroup {
            name: "node".into(),
            description: "Node.js toolchain operations.".into(),
            tools: names(&["npm", "node_run"]),
            surface_when: when("node"),
        },
        ToolGroup {
            name: "python".into(),
            description: "Python toolchain operations.".into(),
            tools: names(&["python_run", "pytest"]),
            surface_when: when("python"),
        },
        ToolGroup {
            name: "make".into(),
            description: "Make build automation.".into(),
            tools: names(&["make"]),
            surface_when: when("make"),
        },
        ToolGroup {
            name: "shell".into(),
            description: "The generic process escape hatches (`bash`, `proc.run`) — off by default. Opt in with \
                          `enable_shell = true` in config or `FLUX_ENABLE_BASH=1` (which inject the \
                          `shell` signal). Prefer the dedicated ops; reach for these only when no op \
                          covers the need."
                .into(),
            tools: names(&["bash", "proc.run"]),
            surface_when: when("shell"),
        },
        ToolGroup {
            name: "endpoint".into(),
            description: "Endpoint discovery (D-28): find live service endpoints (kubernetes \
                          clusters, in-cluster services/ingresses, RDS/SQL databases, monitoring) \
                          as weak references — URLs + a credential location, never a secret — and \
                          select one to connect through. Surfaced when a kubeconfig is present."
                .into(),
            tools: names(&[
                "endpoint.discover",
                "endpoint.select",
                "endpoint.info",
                "endpoint.list",
            ]),
            // Surfaced by the ambient `kubernetes` signal (a kubeconfig is present); a generic
            // `endpoint` signal, if ever injected, surfaces it too.
            surface_when: when_any(&["kubernetes", "endpoint"]),
        },
        ToolGroup {
            name: "cognition".into(),
            description: "Pure cognition helpers: needs/gaps, and list shaping (compare, dedupe, \
                          sort, top, merge, cite, len, first, last, filter)."
                .into(),
            tools: names(&[
                "need", "gaps", "compare", "dedupe", "sort", "top", "merge", "cite", "len",
                "first", "last", "filter",
            ]),
            // Force-on (empty predicate): these deterministic helpers are useful in any session, so
            // they are always advertised rather than gated on a workspace signal.
            surface_when: Vec::new(),
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

    #[test]
    fn endpoint_group_surfaces_on_kubernetes_signal() {
        let g = builtin_groups();
        let ep = g.iter().find(|g| g.name == "endpoint").unwrap();
        for op in [
            "endpoint.discover",
            "endpoint.select",
            "endpoint.info",
            "endpoint.list",
        ] {
            assert!(
                ep.tools.contains(&op.to_string()),
                "endpoint carries `{op}`"
            );
        }
        // The kubernetes ambient signal surfaces it (and a generic `endpoint` signal also does).
        let signals: Vec<&str> = ep
            .surface_when
            .iter()
            .filter_map(|m| m.signal.as_deref())
            .collect();
        assert!(signals.contains(&"kubernetes"));
        assert!(signals.contains(&"endpoint"));
    }

    #[test]
    fn toolchain_groups_carry_their_ops_and_signals() {
        let g = builtin_groups();
        let by = |name: &str| g.iter().find(|g| g.name == name).unwrap();
        for (group, op, signal) in [
            ("go", "go_build", "go"),
            ("node", "npm", "node"),
            ("python", "python_run", "python"),
            ("make", "make", "make"),
        ] {
            let grp = by(group);
            assert!(
                grp.tools.contains(&op.to_string()),
                "group `{group}` should carry `{op}`"
            );
            assert_eq!(grp.surface_when[0].signal.as_deref(), Some(signal));
        }
    }
}
