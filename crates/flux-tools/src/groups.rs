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
                "git_merge",
                "git_push",
                "git_checkout",
                "git_branch",
                "git_unstage",
                "git_hunks",
                "git_stage_hunks",
                "git_worktree_enter",
                "git_worktree_leave",
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
                          as weak references — URLs + a credential location, never a secret — \
                          select one to connect through, and import it into the local store. \
                          Surfaced when a kubeconfig is present or endpoints are registered in \
                          the endpoints store (`~/.flux/endpoints.toml`)."
                .into(),
            // All FIVE endpoint ops listed explicitly (D-115). NOTE: membership was already
            // effective before `endpoint.import` was added here — `flux_runtime::effective_group`
            // falls back to each op's own `ToolSpec::group` tag ("endpoint") when the manifest
            // doesn't list it — so this is explicitness, not a behavior change: the manifest is
            // what config-based reassignment edits, and a flux-cli test pins it against
            // `endpoint_tools()` so the two can't drift.
            tools: names(&[
                "endpoint.discover",
                "endpoint.select",
                "endpoint.info",
                "endpoint.list",
                "endpoint.import",
            ]),
            // Surfaced by the ambient `kubernetes` signal (a kubeconfig is present), or by the
            // session-ambient `endpoint` signal the CLI injects when its startup-loaded endpoints
            // store is non-empty (D-115).
            surface_when: when_any(&["kubernetes", "endpoint"]),
        },
        ToolGroup {
            name: "agent_invoke".into(),
            description: "Agent-side invocation of a discovered command file or skill (D-187, \
                          absorbs C-93): `command.invoke` runs ONLY when the target is explicitly \
                          marked `agent-triggerable: true` in its own frontmatter (default false \
                          — most commands/skills stay human-only) AND your policy grants it AND \
                          it is discovered in this session. Surfaced only when at least one such \
                          target exists."
                .into(),
            tools: names(&["command.invoke"]),
            surface_when: when("agent_triggerable"),
        },
        ToolGroup {
            name: "consult".into(),
            description: "consult (A-96): ask a DIFFERENT model — often a stronger or \
                          differently-biased one — for a second opinion on a hard sub-question; \
                          pure advice, no tools, no side effects beyond the one model call. \
                          Surfaced only when a consult target is configured (`[consult] model` in \
                          .flux/config.toml) — absent otherwise, so the prompt catalog stays \
                          stable within a session (A-95)."
                .into(),
            tools: names(&["consult"]),
            surface_when: when("consult"),
        },
        ToolGroup {
            name: "fleet".into(),
            description: "Outbound A2A dispatch to remote flux workers (A-116): hand a task to a \
                          worker without waiting (`fleet.dispatch`), poll it (`fleet.status`), stop \
                          it (`fleet.cancel`). The worker endpoint is a per-call argument, not \
                          configuration, so there is no workspace signal that could gate these — a \
                          predicate nothing emits would leave them registered but never advertised, \
                          which is the unreachability A-131 closed. Force-on like `cognition`; the \
                          group exists so `.flux/groups.toml` can reassign or gate them."
                .into(),
            tools: names(&["fleet.dispatch", "fleet.status", "fleet.cancel"]),
            // Force-on (empty predicate). A-116 asked for this to be a deliberate decision rather
            // than a default, so here is the argument, and the reason a gate is not the answer.
            //
            // A board's ambient signal is its DECLARATION NAME (`ambient_signal: domain`), which is
            // per-Program, so there is no stable signal a predicate could name. `when("fleet")`
            // would be a predicate nothing emits: the ops would be registered and never advertised,
            // recreating precisely the unreachability this story exists to close.
            //
            // What makes force-on acceptable is that **advertising is not authority**. Seeing the op
            // grants nothing:
            //   * `fleet_private_net()` is `PrivateNetAllow::None` unless the operator passes the
            //     blanket override, so a worker on a private address is refused outright;
            //   * every call resolves its caller-supplied endpoint through `guard_url_scoped` before
            //     any request;
            //   * `permission_subjects` reports the worker's ORIGIN, so a dispatch to a new worker
            //     cannot match an existing grant and routes to approval — and an endpoint that
            //     cannot be named yields no subject at all, which forces approval rather than
            //     matching a broad one.
            // So the cost of force-on is catalog size and prompt churn, not reachable authority, and
            // a workspace that wants them gone can say so in `.flux/groups.toml` without a code
            // change.
            surface_when: Vec::new(),
        },
        ToolGroup {
            name: "cognition".into(),
            description: "Pure cognition helpers: needs/gaps, list shaping (compare, dedupe, sort, \
                          top, merge, cite, len, first, last, filter), aggregation & predicates \
                          (sum, count_by, group_by, any, all, has), object shaping (pick, omit, \
                          merge_obj, coalesce, keys, values), regex (match, extract), and \
                          strict-review normalize/aggregate."
                .into(),
            tools: names(&[
                "need",
                "gaps",
                "compare",
                "dedupe",
                "sort",
                "top",
                "merge",
                "cite",
                "len",
                "first",
                "last",
                "filter",
                "map",
                "flatten",
                "skip",
                "join",
                "split",
                "sum",
                "count_by",
                "group_by",
                "any",
                "all",
                "has",
                "pick",
                "omit",
                "merge_obj",
                "coalesce",
                "keys",
                "values",
                "regex_match",
                "regex_extract",
                "review.normalize",
                "review.aggregate",
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
        // C-98/C-99: the context-local worktree transition ops are Git-group members too.
        assert!(git.tools.contains(&"git_worktree_enter".to_string()));
        assert!(git.tools.contains(&"git_worktree_leave".to_string()));
        // C-238: the serial-integration verbs are Git-group members too.
        assert!(git.tools.contains(&"git_branch".to_string()));
        assert!(git.tools.contains(&"git_merge".to_string()));
        assert_eq!(git.surface_when[0].signal.as_deref(), Some("git_repo"));
    }

    /// A-96: `consult` carries the `consult` op and is gated on the `consult` signal — the CLI
    /// only injects that signal when `[consult] model` is configured, so the op stays off the
    /// catalog by default (A-95: no unconditioned churn to the prompt prefix).
    #[test]
    fn consult_group_carries_the_op_and_is_gated_on_the_consult_signal() {
        let g = builtin_groups();
        let consult = g.iter().find(|g| g.name == "consult").unwrap();
        assert_eq!(consult.tools, vec!["consult".to_string()]);
        assert_eq!(consult.surface_when[0].signal.as_deref(), Some("consult"));
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
