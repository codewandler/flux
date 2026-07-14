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
        "flux-core" | "flux-policy" | "flux-secret" | "flux-spec" | "flux-config"
        | "flux-evidence" | "flux-skill" | "flux-lang" | "flux-markdown" | "flux-datasource"
        | "flux-audio" => 0,
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

/// Report production (non-test) references to a raw `std::process::Command` in one Rust source file,
/// as 1-based line numbers.
///
/// The guarded process seam lives **only** in `flux-system`; every other tool/runtime/plugin path
/// must route process creation through `flux_system::System` (AGENTS.md, "One guarded path starts
/// every OS process"). This scanner backs the regression guard that fails if a new raw `Command`
/// seam is introduced outside `flux-system`.
///
/// It deliberately ignores:
/// - code inside a `#[cfg(test)]` module or item (test code may spawn processes directly), and
/// - line / inline / doc comments,
///
/// so only real, non-test source references are reported. Matching the fully-qualified
/// `std::process::Command` also catches a `use std::process::Command;` import (which would enable a
/// bare `Command::new`), closing the aliasing gap, while never matching `tokio::process::Command`.
pub fn raw_process_command_lines(src: &str) -> Vec<usize> {
    const NEEDLE: &str = "std::process::Command";
    let mut hits = Vec::new();
    // Net brace depth, and the depth at which the current `#[cfg(test)]` region was entered.
    let mut depth: i32 = 0;
    let mut test_region_depth: Option<i32> = None;
    let mut pending_cfg_test = false;

    for (idx, raw_line) in src.lines().enumerate() {
        // Strip a line/inline/doc comment. A `//` inside a string literal would only ever cause a
        // missed match, never a false positive — acceptable for a lint.
        let code = match raw_line.find("//") {
            Some(pos) => &raw_line[..pos],
            None => raw_line,
        };
        let has_cfg_test = code.contains("#[cfg(test)]");
        let opens = code.matches('{').count() as i32;
        let closes = code.matches('}').count() as i32;

        if has_cfg_test {
            pending_cfg_test = true;
        }

        let mut in_test = test_region_depth.is_some();
        if pending_cfg_test {
            if opens > 0 {
                // The `#[cfg(test)]` item opens a block: this line and its block are test code.
                if test_region_depth.is_none() {
                    test_region_depth = Some(depth);
                }
                in_test = true;
                pending_cfg_test = false;
            } else if !has_cfg_test {
                // A single-line guarded item (e.g. `#[cfg(test)]` then `use …;`): skip this one line.
                in_test = true;
                pending_cfg_test = false;
            }
            // else: the bare `#[cfg(test)]` attribute line itself — keep waiting for its item.
        }

        if !in_test && code.contains(NEEDLE) {
            hits.push(idx + 1);
        }

        depth += opens - closes;
        if let Some(td) = test_region_depth {
            if depth <= td {
                test_region_depth = None;
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

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

    #[test]
    fn raw_command_scanner_flags_production_but_ignores_tests_and_comments() {
        // A real production reference is reported (line 2).
        let prod = "fn f() {\n    let c = std::process::Command::new(\"x\");\n}\n";
        assert_eq!(raw_process_command_lines(prod), vec![2]);

        // A `#[cfg(test)] mod tests { … }` block is ignored, however the brace is placed.
        let in_test_mod =
            "#[cfg(test)]\nmod tests {\n    use std::process::Command;\n    let c = std::process::Command::new(\"x\");\n}\n";
        assert!(raw_process_command_lines(in_test_mod).is_empty());
        let same_line =
            "#[cfg(test)] mod tests {\n    let c = std::process::Command::new(\"x\");\n}\n";
        assert!(raw_process_command_lines(same_line).is_empty());

        // A single-line `#[cfg(test)]` item (a test-only import) is ignored.
        let cfg_use = "#[cfg(test)]\nuse std::process::Command;\nfn f() {}\n";
        assert!(raw_process_command_lines(cfg_use).is_empty());

        // Comments (line and doc) are ignored.
        let commented =
            "/// never use std::process::Command here\n// std::process::Command\nfn f() {}\n";
        assert!(raw_process_command_lines(commented).is_empty());

        // `tokio::process::Command` is not the std seam and must not be flagged.
        let tokio = "fn f() {\n    let c = tokio::process::Command::new(\"x\");\n}\n";
        assert!(raw_process_command_lines(tokio).is_empty());

        // Production code after a test module is still scanned (regions close on brace balance).
        let after =
            "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn prod() {\n    std::process::Command::new(\"y\");\n}\n";
        assert_eq!(raw_process_command_lines(after), vec![6]);
    }

    /// Architecture guard: no production (non-test) tool/runtime/plugin path may spawn a raw
    /// `std::process::Command`. Every process start must route through `flux_system::System`'s one
    /// guarded seam (AGENTS.md). `flux-system` itself owns the seam and is exempt. Adding a new raw
    /// `Command` anywhere else fails this test — an explicit exception must be added below with a
    /// justification if one is ever genuinely warranted.
    #[test]
    fn no_raw_process_command_outside_system() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let repo_root = crates_dir.parent().unwrap();

        // Documented, reviewed exceptions: `(repo-relative path, 1-based line)`. Empty by design —
        // the seam belongs in flux-system only.
        const ALLOW: &[(&str, usize)] = &[];

        let mut rs_files: Vec<PathBuf> = Vec::new();
        // Root workspace crates' `src/` — skip `flux-system` (the guarded owner of the seam) and
        // `flux-codegate` itself (this lint's own source names the pattern as a string needle).
        const EXEMPT_CRATES: &[&str] = &["flux-system", "flux-codegate"];
        for entry in std::fs::read_dir(crates_dir).unwrap() {
            let dir = entry.unwrap().path();
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !dir.is_dir() || EXEMPT_CRATES.contains(&name) {
                continue;
            }
            collect_rs(&dir.join("src"), &mut rs_files);
        }
        // Nested plugins workspace: every `plugins/*/src/`.
        let plugins_dir = repo_root.join("plugins");
        if plugins_dir.is_dir() {
            for entry in std::fs::read_dir(&plugins_dir).unwrap() {
                let dir = entry.unwrap().path();
                if dir.is_dir() {
                    collect_rs(&dir.join("src"), &mut rs_files);
                }
            }
        }

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
            for line in raw_process_command_lines(&src) {
                if !ALLOW.contains(&(rel.as_str(), line)) {
                    violations.push(format!("{rel}:{line}"));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "raw `std::process::Command` outside flux-system (route through flux_system::System \
             instead, or add a justified exception to ALLOW):\n  {}",
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
}
