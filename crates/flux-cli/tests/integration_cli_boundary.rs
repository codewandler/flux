//! C-509's dependency-independent CLI boundary while provider-owned contracts are unavailable.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "flux-c509-cli-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn provider_bound_commands_emit_one_stable_value_free_json_refusal() {
    let temp = TempDir::new();
    let cases: &[(&[&str], &str)] = &[
        (
            &["exchange", "local", "start", "--json"],
            "exchange.local.start",
        ),
        (
            &["exchange", "local", "status", "--json"],
            "exchange.local.status",
        ),
        (
            &["exchange", "local", "stop", "--json"],
            "exchange.local.stop",
        ),
        (
            &[
                "integration",
                "connect",
                "custom-connector",
                "--name",
                "company",
                "--json",
                "--no-prompt",
            ],
            "integration.connect",
        ),
        (
            &[
                "integration",
                "grant",
                "custom-connector",
                "--name",
                "company",
                "--selector",
                "opaque-selector=must-not-appear",
                "--json",
                "--no-prompt",
            ],
            "integration.grant",
        ),
        (&["integration", "list", "--json"], "integration.list"),
        (&["integration", "doctor", "--json"], "integration.doctor"),
    ];

    for (args, command) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_flux"))
            .args(*args)
            .current_dir(temp.path())
            .env("HOME", temp.path())
            .env("NO_COLOR", "1")
            .env("FLUX_SANDBOX", "off")
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("run {args:?}: {error}"));

        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout:?}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stdout).unwrap(),
            serde_json::json!({
                "ok": false,
                "category": "unsupported",
                "command": command,
            }),
            "{args:?}"
        );
        assert!(!stdout.contains("must-not-appear"), "{args:?}: {stdout}");
    }
}

#[test]
fn connect_refuses_generic_field_values_until_the_provider_plan_can_classify_them() {
    let temp = TempDir::new();
    let secret = "glpat-must-not-enter-flux-argv";
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args([
            "integration",
            "connect",
            "gitlab",
            "--name",
            "company",
            "--field",
            &format!("password={secret}"),
            "--json",
            "--no-prompt",
        ])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("run integration connect with an unavailable plan-bound field");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unexpected argument '--field'"), "{stderr}");
    assert!(!stderr.contains(secret), "{stderr}");
    assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());
}

#[test]
fn malformed_selector_is_a_stable_value_free_json_refusal() {
    let temp = TempDir::new();
    let secret = "selector-value-must-not-reach-diagnostics";
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args([
            "integration",
            "grant",
            "gitlab",
            "--name",
            "company",
            "--selector",
            secret,
            "--json",
            "--no-prompt",
        ])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("run integration grant with a malformed selector");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap(),
        serde_json::json!({
            "ok": false,
            "category": "invalid_input",
            "command": "integration.grant",
        })
    );
    assert!(!stdout.contains(secret));
}

#[test]
fn provider_bound_human_command_writes_only_the_typed_refusal_to_stderr() {
    let temp = TempDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["integration", "list"])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("run provider-bound human command");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refused [unsupported]: integration list\n"
    );
}
