use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn malformed_role_wins_before_provider_and_tool_execution() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "flux-cli-role-startup-{}-{nonce}",
        std::process::id()
    ));
    let home = root.join("home");
    let agents = root.join(".flux/agents");
    std::fs::create_dir_all(&agents).expect("create project agents");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let role_path = agents.join("broken.md");
    std::fs::write(
        &role_path,
        "---\ntools: read\n---\nThis role must never inherit the parent catalog.",
    )
    .expect("write malformed role");

    let marker = root.join("tool-ran.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .current_dir(&root)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        // This provider is deliberately invalid: if eager provider construction runs first, its
        // error masks the security-relevant role path. The mock tool hooks are a second tripwire:
        // even if startup accidentally reaches a model turn, it must not create the marker.
        .env("FLUX_MOCK_TOOL", "write")
        .env(
            "FLUX_MOCK_TOOL_INPUT",
            serde_json::json!({
                "path": marker,
                "content": "tool execution must not happen"
            })
            .to_string(),
        )
        .args([
            "--color",
            "never",
            "run",
            "--model",
            "invalid-provider/invalid-model",
            "--yes",
            "spawn the broken role",
        ])
        .output()
        .expect("run flux");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(".flux/agents/broken.md"),
        "role path must win over provider construction: {stderr}"
    );
    assert!(stderr.contains("tools"), "{stderr}");
    assert!(
        !stderr.contains("invalid-provider"),
        "provider construction ran before strict role loading: {stderr}"
    );
    assert!(
        !marker.exists(),
        "model/tool execution reached the write op"
    );
    assert!(
        !home.join(".flux/events.db").exists(),
        "agent assembly reached the event store before rejecting the role"
    );

    std::fs::remove_dir_all(root).ok();
}
