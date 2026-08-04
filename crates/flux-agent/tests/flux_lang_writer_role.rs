use std::path::Path;

use flux_agent::{try_parse_role, AgentProfile};

const ROLE_SOURCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.flux/agents/flux-lang-writer.md"
);

#[test]
fn flux_lang_writer_is_narrow_and_honest_about_validation() {
    let source = std::fs::read_to_string(ROLE_SOURCE)
        .unwrap_or_else(|error| panic!("read tracked role {ROLE_SOURCE}: {error}"));
    let role = try_parse_role(&source, "flux-lang-writer").expect("parse flux-lang-writer role");
    let normalized_instructions = role
        .instructions
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    assert_eq!(role.profile, AgentProfile::Coding);
    assert_eq!(
        role.tools
            .as_ref()
            .map(|tools| tools.iter().map(String::as_str).collect::<Vec<_>>()),
        Some(vec![
            "read", "glob", "grep", "write", "edit", "patch", "proc.run"
        ]),
        "the writer needs an explicit, narrow tool allow-list"
    );

    for required in [
        "crates/flux-lang/AGENTS.md",
        "crates/flux-lang/docs/syntax.md",
        "crates/flux-lang/docs/reference.md",
        "workspace-relative",
        "smallest",
        "syntax",
        "analysis",
        "execution",
        "exact command",
        "exit status",
        "authorization",
        "approval",
        "guarded IO",
        "sandboxing",
        "redaction",
    ] {
        assert!(
            role.instructions.contains(required),
            "writer instructions must contain `{required}`"
        );
    }

    assert!(
        normalized_instructions.contains("never run an effectful flow merely to validate it"),
        "static validation must not be replaced with execution"
    );
    assert!(
        role.instructions.contains("flux flow run"),
        "explicitly requested execution must use the ordinary runtime"
    );
    assert!(Path::new(ROLE_SOURCE).ends_with(".flux/agents/flux-lang-writer.md"));
}
