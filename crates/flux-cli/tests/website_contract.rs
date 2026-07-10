//! Executable contract for the public website's hand-maintained mirrors.
//!
//! The node/prelude tables and customer changelog have their own generated-block test in
//! `flux-lang`. This suite covers the remaining cross-crate surfaces that are easy to let drift:
//! CLI command names, registered operations, config examples, plugin-pack membership, SDK package
//! names, and complete Flux-Lang snippets.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use flux_cognition::CognitionPack;
use flux_provider::NullProvider;
use flux_runtime::ToolRegistry;

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = repo_path(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|x| x.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn fenced_blocks<'a>(markdown: &'a str, language: &str) -> Vec<&'a str> {
    let open = format!("```{language}\n");
    let mut rest = markdown;
    let mut blocks = Vec::new();
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let end = rest.find("\n```").expect("closed markdown code fence");
        blocks.push(&rest[..end]);
        rest = &rest[end + 4..];
    }
    blocks
}

#[test]
fn cli_reference_covers_every_public_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .arg("--help")
        .output()
        .expect("run flux --help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    let commands = help
        .split_once("Commands:\n")
        .expect("Commands section")
        .1
        .split("\n\n")
        .next()
        .expect("Commands body");
    let docs = read("website/docs/agent/cli.md");
    for name in commands
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
    {
        assert!(
            docs.contains(&format!("`flux {name}")),
            "website CLI reference omits `flux {name}`"
        );
    }
}

#[test]
fn operations_reference_covers_the_registered_public_catalog() {
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    flux_web::register_web(&mut registry, &flux_web::WebOptions::default());
    CognitionPack::new(Arc::new(NullProvider), "mock").register(&mut registry);

    let docs = read("website/docs/language/ops.md");
    let mut names = registry.names();
    names.extend(
        [
            "ask",
            "emit",
            "endpoint.discover",
            "endpoint.import",
            "endpoint.info",
            "endpoint.list",
            "endpoint.select",
            "flow_list",
            "flow_run",
            "op.register",
            "send",
            "spawn",
        ]
        .into_iter()
        .map(str::to_string),
    );
    names.sort();
    names.dedup();
    for name in names {
        assert!(
            docs.contains(&format!("`{name}`")),
            "website operations reference omits `{name}`"
        );
    }
}

#[test]
fn public_config_examples_deserialize_and_have_effect() {
    for rel in [
        "website/docs/reference/config.md",
        "website/docs/troubleshooting.md",
        "website/docs/plugins/using-plugins.md",
        "website/docs/plugins/gitlab.md",
    ] {
        let markdown = read(rel);
        for (index, block) in fenced_blocks(&markdown, "toml").into_iter().enumerate() {
            let cfg: flux_config::Config = toml::from_str(block)
                .unwrap_or_else(|e| panic!("{rel} TOML block {}: {e}", index + 1));
            if block.contains("[private_net]") {
                assert!(
                    !cfg.web_private_hosts().is_empty(),
                    "{rel} TOML block {} declares [private_net] but grants no web scope",
                    index + 1
                );
            }
            if block.contains("[private_net.plugins]") {
                let plugin = block
                    .lines()
                    .skip_while(|line| line.trim() != "[private_net.plugins]")
                    .skip(1)
                    .find_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
                    .expect("plugin grant entry");
                assert!(
                    !cfg.plugin_private_hosts(plugin).is_empty(),
                    "{rel} TOML block {} has an ineffective plugin grant",
                    index + 1
                );
            }
        }
    }
}

#[test]
fn complete_flux_fences_parse_and_legacy_syntax_stays_out() {
    let docs_root = repo_path("website/docs");
    let declarations = [
        "agent ",
        "channel ",
        "datasource ",
        "flow ",
        "journey ",
        "op ",
        "trigger ",
    ];
    let mut checked = 0;
    for path in markdown_files(&docs_root) {
        let markdown = fs::read_to_string(&path).expect("read website doc");
        for (index, block) in fenced_blocks(&markdown, "flux").into_iter().enumerate() {
            assert!(
                !block
                    .lines()
                    .any(|line| line.trim_start().starts_with("let ")),
                "{} Flux block {} uses the retired `let` syntax",
                path.display(),
                index + 1
            );
            let complete = block.lines().any(|line| {
                line.len() == line.trim_start().len()
                    && declarations.iter().any(|decl| line.starts_with(decl))
            });
            if complete {
                flux_lang::parse::parse_program(block)
                    .unwrap_or_else(|e| panic!("{} Flux block {}: {e}", path.display(), index + 1));
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 25,
        "expected a representative Flux example corpus"
    );
}

#[test]
fn plugin_and_sdk_docs_track_the_shipped_surfaces() {
    let plugins_toml: toml::Value = toml::from_str(&read("plugins/Cargo.toml")).unwrap();
    let members = plugins_toml["workspace"]["members"]
        .as_array()
        .expect("plugins workspace members");
    let plugin_docs = read("website/docs/plugins/using-plugins.md");
    for member in members.iter().filter_map(toml::Value::as_str) {
        if matches!(member, "host-kit" | "pack-index") {
            continue;
        }
        assert!(
            plugin_docs.contains(&format!("`{member}`")),
            "plugin pack docs omit `{member}`"
        );
    }
    assert!(plugin_docs.contains("process is still spawned"));

    let sdk = read("website/docs/sdk/overview.md");
    assert!(sdk.contains("cargo add codewandler-flux-sdk codewandler-flux-providers"));
    assert!(sdk.contains("cargo run -p codewandler-flux-sdk --example client_basic"));
    assert!(!sdk.contains("not published to crates.io"));
    assert!(!sdk.contains("tag = \"v0.6.0\""));
    assert!(!sdk.contains("cargo run -p flux-sdk"));
}

/// Every occurrence of the "not OS-sandboxed" disclaimer must be immediately qualified "by
/// default" — the D-133 truth pass. Before D-130..D-132, the claim was unconditionally true
/// (`Backend::Unsupported` on every platform); now a real bubblewrap (Linux) / Seatbelt (macOS)
/// backend exists behind opt-in `[sandbox]` config, so an unqualified "not OS-sandboxed" (whatever
/// punctuation trails it — a colon, semicolon, period, or "processes.") would silently regress the
/// docs back to the old overclaim. Scanning every occurrence (rather than banning one fixed old
/// sentence) is what makes this robust to each page's own old phrasing without also needing to
/// avoid colliding with the new one.
fn assert_every_os_sandboxed_disclaimer_is_qualified(docs: &str, rel: &str) {
    let phrase = "not OS-sandboxed";
    let qualifier = " by default";
    let mut search_from = 0;
    let mut occurrences = 0;
    while let Some(found) = docs[search_from..].find(phrase) {
        let start = search_from + found;
        let after = &docs[start + phrase.len()..];
        assert!(
            after.starts_with(qualifier),
            "{rel}: found \"not OS-sandboxed\" not immediately followed by \" by default\" — the \
             old unqualified claim regressed (context: {:?})",
            &docs[start.saturating_sub(30)..(start + phrase.len() + 30).min(docs.len())]
        );
        occurrences += 1;
        search_from = start + phrase.len();
    }
    assert!(
        occurrences > 0,
        "{rel} must state the trusted-native plugin boundary"
    );
}

#[test]
fn plugin_security_copy_keeps_the_native_code_trust_boundary_explicit() {
    for rel in [
        "website/docs/plugins/using-plugins.md",
        "website/docs/plugins/authoring.md",
        "website/docs/security/plugin-sandbox.md",
        "website/docs/agent/safety.md",
        "website/docs/infrastructure.md",
    ] {
        let docs = read(rel);
        assert_every_os_sandboxed_disclaimer_is_qualified(&docs, rel);
        assert!(!docs.contains("Plugins do **no privileged IO of their own**"));
        assert!(!docs.contains("A plugin never opens a socket"));
    }
}

/// D-133: the new OS-sandbox security page exists and carries its key claims — platform coverage,
/// the config surface, the fail-closed `require` promise, and (verbatim, per the design doc) the
/// honesty list of what v1 does not defend against.
#[test]
fn os_sandbox_page_exists_and_states_its_key_claims() {
    let docs = read("website/docs/security/os-sandbox.md");
    assert!(
        docs.contains("What v1 does not defend against"),
        "os-sandbox.md must carry the honesty-list heading"
    );
    assert!(docs.contains("bubblewrap"), "must name the Linux backend");
    assert!(docs.contains("Seatbelt"), "must name the macOS backend");
    assert!(
        docs.contains("fails closed"),
        "must state the require-mode fail-closed promise"
    );
    assert!(
        docs.contains("[sandbox]"),
        "must reference the `[sandbox]` config table"
    );
    assert!(
        docs.contains("spawn_debug_pipe") || docs.contains("browser"),
        "must document the browser exemption"
    );
}
