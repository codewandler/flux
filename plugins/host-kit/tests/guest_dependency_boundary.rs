//! C-69 structural guard: the SDK consumed by guest plugins must not regain host-only dependencies.

use std::process::Command;

#[test]
fn guest_tree_excludes_host_transport_hooks_and_install_stack() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("plugins workspace");
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--manifest-path",
            "Cargo.toml",
            "-p",
            "codewandler-flux-host-kit",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--locked",
            "--offline",
        ])
        .current_dir(workspace)
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).expect("cargo tree is UTF-8");

    for forbidden in [
        "reqwest ",
        "codewandler-flux-credentials ",
        "codewandler-flux-provider ",
        "codewandler-flux-runtime ",
        "codewandler-flux-system ",
        "rquickjs ",
        "minisign-verify ",
        "tar ",
        "lzma-rs ",
        "zip ",
    ] {
        assert!(
            !tree.lines().any(|line| line.starts_with(forbidden)),
            "guest dependency tree unexpectedly contains `{forbidden}`:\n{tree}"
        );
    }

    let packages = tree
        .lines()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert!(
        packages <= 90,
        "guest tree grew beyond the reviewed C-69 ceiling (80 packages at cutover): {packages}"
    );
}
