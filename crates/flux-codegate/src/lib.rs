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
    Some(match name {
        // L0 — pure contracts: no IO, no flux deps except other L0. Safe for anything to use.
        "flux-core" | "flux-policy" | "flux-secret" | "flux-spec" | "flux-config"
        | "flux-evidence" | "flux-skill" | "flux-lang" | "flux-markdown" => 0,
        // L1 — the provider abstraction, the concrete providers (Anthropic/OpenAI/OpenRouter/
        // Ollama + the shared Messages protocol core, all in `flux-providers`), credentials, and
        // the A2A agent-protocol client + wire types (`flux-a2a`; no flux deps — a network client)
        "flux-provider" | "flux-providers" | "flux-credentials" | "flux-a2a" => 1,
        // L2 — runtime: execution + guarded IO + the safety envelope (context-projector module
        // now lives inside flux-runtime)
        "flux-system" | "flux-runtime" | "flux-tools" | "flux-events" => 2,
        // L3 — agent + orchestration + eval/self-improvement harness + cognition ops
        "flux-agent" | "flux-orchestrate" | "flux-flow" | "flux-eval" | "flux-cognition" => 3,
        // L4 — extensibility (subprocess plugins + the JS pre-tool hooks module)
        "flux-plugin" => 4,
        // L5 — heavy capabilities (web + datasource tools in flux-capabilities; caller identity
        // in flux-auth, kept separate as a distinct concern from tool capabilities)
        "flux-capabilities" | "flux-auth" => 5,
        // L6 — surfaces / apps (and this lint crate itself)
        "flux-sdk" | "flux-server" | "flux-tui" | "flux-cli" | "flux-codegate" | "flux-app"
        | "flux-channels" => 6,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Read every `crates/*/Cargo.toml`, collect its `flux-*` runtime dependencies, and assert the
    /// whole workspace respects the layering (no inner crate depends on an outer one).
    #[test]
    fn workspace_respects_layering() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut deps_by_crate: Vec<(String, Vec<String>)> = Vec::new();

        for entry in std::fs::read_dir(crates_dir).unwrap() {
            let manifest = entry.unwrap().path().join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let txt = std::fs::read_to_string(&manifest).unwrap();
            let val: toml::Value = toml::from_str(&txt).unwrap();
            let name = val["package"]["name"].as_str().unwrap().to_string();
            // Only [dependencies] constrain layering; [dev-dependencies] may point upward for tests.
            let deps = val
                .get("dependencies")
                .and_then(|d| d.as_table())
                .map(|t| {
                    t.keys()
                        .filter(|k| k.starts_with("flux-"))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            deps_by_crate.push((name, deps));
        }

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
}
