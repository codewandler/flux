use std::process::Command;

#[test]
fn exchange_local_unavailable_backend_is_a_value_free_preclassification_exit() {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["exchange", "local", "status", "--json"])
        .env("NO_COLOR", "1")
        .output()
        .expect("run flux exchange local status");

    assert_eq!(output.status.code(), Some(70));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"error: internal_failure\n");
}
