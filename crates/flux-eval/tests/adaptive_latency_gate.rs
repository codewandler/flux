use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const HEADER: &str = "phase\tarm\tmodel\tworkload\ttrial\tstatus\ttotal_ms\tstartup_ms\tprovider_calls\tintent_calls\tintent_ms\tintent_ttft_ms\trepairs\tinput_tokens\tcached_tokens\toutput_tokens\tsystem_bytes\tmessage_bytes\tschema_bytes\tapproval_wait_ms\texecution_ms\tfamilies\tsession\tlog\n";

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn test_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("flux-{label}-{}-{nonce}", std::process::id()))
}

fn row(phase: &str, arm: &str, workload: &str, trial: u8) -> String {
    let (total_ms, intent_ms) = match arm {
        "baseline" => (100, 100),
        "cap512" => (if workload == "support" { 100 } else { 80 }, 50),
        other => panic!("unexpected arm: {other}"),
    };
    format!(
        "{phase}\t{arm}\ttest/model\t{workload}\t{trial}\tPASS\t{total_ms}\t0\t2\t1\t{intent_ms}\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t[]\ttest-session\ttest.log\n"
    )
}

fn complete_confirmation() -> String {
    let mut rows = String::new();
    for arm in ["baseline", "cap512"] {
        for workload in ["greeting", "time", "support"] {
            for trial in 1..=2 {
                rows.push_str(&row("confirm_paired", arm, workload, trial));
            }
        }
    }
    rows
}

fn run_gate(case: &str, rows: &str) -> Output {
    let root = test_dir(case);
    let fixture = root.join("fixture");
    let results = root.join("results");
    fs::create_dir_all(&results).expect("create result directory");
    fs::write(results.join("trials.tsv"), format!("{HEADER}{rows}")).expect("write trial summary");

    let output = Command::new("bash")
        .arg(repo_path("scripts/eval-adaptive-latency.sh"))
        .arg("gate")
        .env("FLUX_BIN", "/bin/true")
        .env("FIXTURE_DIR", &fixture)
        .env("RESULTS_DIR", &results)
        .env("MODELS", "test/model")
        .env("CONFIRM_TRIALS", "2")
        .env("CONFIRM_ARM", "cap512")
        .output()
        .expect("run adaptive latency gate");
    fs::remove_dir_all(root).expect("remove test directory");
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

#[test]
fn gate_requires_the_complete_confirmation_and_slack_matrix() {
    let header_only = run_gate("adaptive-gate-empty", "");
    assert!(!header_only.status.success());
    assert!(
        stdout(&header_only).contains("matrix:confirm_paired:expected=12:found=0"),
        "{}",
        stdout(&header_only)
    );

    let mut missing_confirmation =
        complete_confirmation().replace(&row("confirm_paired", "cap512", "support", 2), "");
    missing_confirmation.push_str(&row("confirm_paired", "baseline", "greeting", 1));
    let missing_confirmation = run_gate("adaptive-gate-partial", &missing_confirmation);
    assert!(!missing_confirmation.status.success());
    assert!(
        stdout(&missing_confirmation)
            .contains("missing:confirm_paired:cap512:test/model:support:2"),
        "{}",
        stdout(&missing_confirmation)
    );
    assert!(
        stdout(&missing_confirmation)
            .contains("duplicate:confirm_paired:baseline:test/model:greeting:1:count=2"),
        "{}",
        stdout(&missing_confirmation)
    );

    let mut stale_confirmation = complete_confirmation();
    stale_confirmation.push_str(&row("confirm_paired", "cap512", "support", 3));
    stale_confirmation.push_str(&row("slack", "cap512", "slack", 1));
    let stale_confirmation = run_gate("adaptive-gate-stale", &stale_confirmation);
    assert!(!stale_confirmation.status.success());
    assert!(
        stdout(&stale_confirmation)
            .contains("unexpected:confirm_paired:cap512:test/model:support:3:count=1"),
        "{}",
        stdout(&stale_confirmation)
    );

    let no_slack = run_gate("adaptive-gate-no-slack", &complete_confirmation());
    assert!(!no_slack.status.success());
    assert!(
        stdout(&no_slack).contains("missing:slack:cap512:test/model:slack:1"),
        "{}",
        stdout(&no_slack)
    );

    let mut complete = complete_confirmation();
    complete.push_str(&row("slack", "cap512", "slack", 1));
    let complete = run_gate("adaptive-gate-complete", &complete);
    assert!(complete.status.success(), "{}", stdout(&complete));
    assert!(stdout(&complete).starts_with("KEEP cap512:"));
}
