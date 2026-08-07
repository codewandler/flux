//! Executable contract for the shipped remote-system deployment artifacts (C-480).
//!
//! `deploy/` turns the BYO recipe in the deployment guide into artifacts an operator applies. That
//! only helps if the artifacts stay true to the daemon they package, so this suite derives what it
//! can from the code — the flags the shipped binary accepts, the protocol version it enforces — and
//! pins the rest: the controls each profile promises, the secret material none of them may carry,
//! and the provisioning boundary all of them state.
//!
//! The container profile's *behaviour* is proved separately and needs Docker:
//! `crates/flux-server/tests/remote_system_container.rs`. Everything here runs in ordinary CI.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DOCKERFILE: &str = "deploy/container/Dockerfile";
const BUILD_IMAGE: &str = "deploy/container/build-image.sh";
const KUSTOMIZATION: &str = "deploy/kubernetes/kustomization.yaml";
const DEPLOYMENT: &str = "deploy/kubernetes/deployment.yaml";
const SERVICE: &str = "deploy/kubernetes/service.yaml";
const PVC: &str = "deploy/kubernetes/workspace-pvc.yaml";
const NETWORKPOLICY: &str = "deploy/kubernetes/networkpolicy.yaml";
const UNIT: &str = "deploy/vm/flux-system.service";
const INSTALL: &str = "deploy/vm/install-flux-system.sh";
const CLOUD_INIT: &str = "deploy/vm/cloud-init.yaml";
const DEPLOY_README: &str = "deploy/README.md";
const VM_README: &str = "deploy/vm/README.md";
const KUBERNETES_README: &str = "deploy/kubernetes/README.md";
const PUBLIC_GUIDE: &str = "website/docs/remote-system-deployment.md";

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = repo_path(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every artifact in the profile set. A new one that nothing checks is the failure mode this list
/// exists to make loud.
fn all_artifacts() -> Vec<&'static str> {
    vec![
        DOCKERFILE,
        BUILD_IMAGE,
        KUSTOMIZATION,
        DEPLOYMENT,
        SERVICE,
        PVC,
        NETWORKPOLICY,
        UNIT,
        INSTALL,
        CLOUD_INIT,
    ]
}

/// `flux <path…> --help`, as the shipped binary renders it.
///
/// `FLUX_SANDBOX=off` is declared rather than inherited, per C-266: the subcommand path is
/// forwarded in bulk, so the posture gate cannot see that every call renders help and executes
/// nothing. Off is the honest declaration for a spawn that never reaches an effect.
fn flux_help(path: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .env("FLUX_SANDBOX", "off")
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("run `flux {} --help`: {e}", path.join(" ")));
    assert!(
        output.status.success(),
        "`flux {} --help` failed: {}",
        path.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Lines starting at the first one matching `start`, continuing while the previous line ends in a
/// backslash. Shared by the Dockerfile's `ENTRYPOINT`/`CMD` and the unit's `ExecStart=`.
fn continued_directive(source: &str, start: &str) -> String {
    let mut collected = Vec::new();
    let mut active = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if !active && trimmed.starts_with(start) {
            active = true;
        }
        if active {
            collected.push(trimmed.trim_end_matches('\\').trim_end());
            if !trimmed.ends_with('\\') {
                active = false;
            }
        }
    }
    collected.join(" ")
}

/// The `args:` list a Kubernetes container hands to the image entrypoint.
fn yaml_args(source: &str) -> String {
    source
        .lines()
        .skip_while(|line| line.trim() != "args:")
        .skip(1)
        .take_while(|line| line.trim().starts_with("- "))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Only the argv the artifact hands to `flux system serve` — never the surrounding `useradd`,
/// `apt-get` or systemd directives, whose flags belong to other programs entirely.
fn daemon_argv(artifact: &str, source: &str) -> String {
    match artifact {
        DOCKERFILE => format!(
            "{} {}",
            continued_directive(source, "ENTRYPOINT"),
            continued_directive(source, "CMD")
        ),
        DEPLOYMENT => yaml_args(source),
        UNIT => continued_directive(source, "ExecStart="),
        other => panic!("no argv extractor for {other}"),
    }
}

/// Every `--long-flag` an artifact hands to the daemon.
fn flags_in(source: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    for (index, _) in source.match_indices("--") {
        let rest = &source[index..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(rest.len());
        let flag = &rest[..end];
        // `--` alone, and the `── section ──` rules in the artifacts' comments.
        if flag.len() > 2 && flag.starts_with("--") && !flag.ends_with('-') {
            flags.push(flag.to_string());
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

/// C-480: the image runs one thing, as somebody who is not root.
#[test]
fn the_container_image_runs_only_the_serving_daemon_as_non_root() {
    let dockerfile = read(DOCKERFILE);

    assert!(
        dockerfile.contains(r#"ENTRYPOINT ["/usr/local/bin/flux", "system", "serve"]"#),
        "{DOCKERFILE} must exec `flux system serve` directly — a shell wrapper is one more thing \
         between an operator and what is actually running"
    );
    assert!(
        dockerfile.contains("USER 10001:10001"),
        "{DOCKERFILE} must drop to a non-root identity"
    );
    assert!(
        dockerfile.contains("EXPOSE 8790"),
        "{DOCKERFILE} must declare the daemon's port"
    );

    // Exactly one thing is copied in: the binary. A second COPY is how a workspace or a key gets
    // into a layer by accident.
    let copies: Vec<&str> = dockerfile
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("COPY ") || line.starts_with("ADD "))
        .collect();
    assert_eq!(
        copies,
        vec!["COPY flux /usr/local/bin/flux"],
        "{DOCKERFILE} must copy the `flux` binary and nothing else"
    );

    // The mount points are declared, never baked.
    for expected in ["/srv/flux/workspace", "/run/flux-tls"] {
        assert!(
            dockerfile.contains(expected),
            "{DOCKERFILE} no longer mentions the mount point `{expected}`"
        );
    }
}

/// C-480: no artifact may carry the two things the deployment contract says must never be committed.
#[test]
fn no_deployment_artifact_carries_a_token_or_a_private_key() {
    let mut violations = Vec::new();
    for artifact in all_artifacts() {
        let source = read(artifact);
        for (number, line) in source.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            if lower.contains("private key-----") || lower.contains("begin private key") {
                violations.push(format!("{artifact}:{}: a private key", number + 1));
            }
            // `FLUX_REMOTE_SYSTEM_TOKEN=` with anything after it, outside a comment. The guest
            // profile writes the variable with a deliberately empty value, which is what makes a
            // guest that never received a secret fail closed.
            if let Some(rest) = line.split_once("FLUX_REMOTE_SYSTEM_TOKEN=") {
                let trimmed = rest.1.trim().trim_matches(|c| c == '\'' || c == '"');
                let commented = line.trim_start().starts_with('#');
                let placeholder = trimmed.is_empty()
                    || trimmed.starts_with("\\n")
                    || trimmed.starts_with('$')
                    || trimmed.contains('…');
                if !commented && !placeholder {
                    violations.push(format!(
                        "{artifact}:{}: a bearer token value ({trimmed})",
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "deployment artifacts must never carry secret material — the whole point of mounting it:\n  {}",
        violations.join("\n  ")
    );

    // A committed Secret manifest is the same defect wearing Kubernetes clothes.
    for manifest in [KUSTOMIZATION, DEPLOYMENT, SERVICE, PVC, NETWORKPOLICY] {
        assert!(
            !read(manifest).contains("kind: Secret"),
            "{manifest} declares a Secret — create both Secrets out of band instead"
        );
    }
}

/// C-480: the image is built from a release, and reads the release version the same way every other
/// release entry point does.
#[test]
fn the_image_build_rides_the_binary_release_identity() {
    let build = read(BUILD_IMAGE);
    let cut = read("scripts/cut-release.sh");

    // Derived, not restated: whatever expression the cut uses to read the workspace version, the
    // image build uses the same one. A version source that drifts is an image that lies.
    let version_read = cut
        .lines()
        .map(str::trim)
        .find(|line| line.contains("grep -m1 '^version = '"))
        .expect("scripts/cut-release.sh reads [workspace.package].version");
    // Both halves of the cut's expression, so the path to Cargo.toml may differ but the thing being
    // read and the way it is parsed may not.
    let sed_program = version_read
        .split_once("sed -E ")
        .map(|(_, rest)| rest.trim_end_matches(')').trim())
        .expect("the version-parsing sed program");
    for half in ["grep -m1 '^version = '", sed_program] {
        assert!(
            build.contains(half),
            "{BUILD_IMAGE} must read the release version as scripts/cut-release.sh does (`{half}`), \
             or the image tag and the release can disagree"
        );
    }

    // The release path repacks published, attested bytes and proves it did.
    for required in [
        "flux-cli-$TARGET.tar.xz",
        "sha256sum --check",
        "gh release download",
    ] {
        assert!(
            build.contains(required),
            "{BUILD_IMAGE} no longer pins the published archive (`{required}`)"
        );
    }
    assert!(
        build.contains("org.opencontainers.image.version")
            || read(DOCKERFILE).contains("org.opencontainers.image.version"),
        "the image must label the release it carries"
    );

    // The archive name has to be one the release actually publishes.
    let verify = read("scripts/verify-github-release.sh");
    assert!(
        verify.contains("flux-cli") && verify.contains("x86_64-unknown-linux-gnu"),
        "scripts/verify-github-release.sh no longer describes the archive the image repacks — \
         re-derive {BUILD_IMAGE}'s archive name"
    );

    // The SBOM/provenance path is documented rather than assumed.
    let readme = read(DEPLOY_README);
    for required in ["gh attestation verify", "sha256"] {
        assert!(
            readme.contains(required),
            "{DEPLOY_README} must document the provenance path (`{required}`)"
        );
    }
}

/// C-480: the Kubernetes profile carries every control the acceptance names, in the file that would
/// actually be applied.
#[test]
fn the_kubernetes_profile_carries_every_required_control() {
    let deployment = read(DEPLOYMENT);
    for (required, why) in [
        ("replicas: 1", "one replica per canonical workspace"),
        ("type: Recreate", "two pods must never hold one workspace"),
        ("runAsNonRoot: true", "non-root"),
        ("type: RuntimeDefault", "seccomp"),
        ("readOnlyRootFilesystem: true", "read-only root filesystem"),
        ("allowPrivilegeEscalation: false", "no privilege escalation"),
        ("- ALL", "all capabilities dropped"),
        ("fsGroup: 10001", "the daemon can write its own ledger"),
        ("readinessProbe", "readiness"),
        ("livenessProbe", "liveness"),
        (
            "tcpSocket",
            "a TCP probe, because every route is authenticated",
        ),
        ("secretKeyRef", "the bearer token comes from a Secret"),
        ("secretName: flux-system-tls", "the TLS Secret"),
        ("persistentVolumeClaim", "the durable workspace"),
        (
            "--no-sandbox",
            "the sandbox floor is waived explicitly, not silently",
        ),
    ] {
        assert!(
            deployment.contains(required),
            "{DEPLOYMENT} no longer carries `{required}` ({why})"
        );
    }

    assert!(
        read(SERVICE).contains("type: ClusterIP"),
        "{SERVICE} must stay ClusterIP — this daemon is not a public endpoint"
    );
    assert!(
        read(PVC).contains("kind: PersistentVolumeClaim"),
        "{PVC} must claim durable storage for the canonical workspace"
    );

    // Default-deny means an empty pod selector and *both* policy types; naming only Ingress leaves
    // egress unrestricted while looking like a boundary.
    let policy = read(NETWORKPOLICY);
    let default_deny = policy
        .split("---")
        .find(|document| document.contains("name: default-deny"))
        .expect("a default-deny NetworkPolicy");
    assert!(
        default_deny.contains("podSelector: {}")
            && default_deny.contains("- Ingress")
            && default_deny.contains("- Egress"),
        "{NETWORKPOLICY}'s default-deny must select every pod and deny both directions"
    );

    // The base has to name every manifest, or an applied profile is missing a control the file set
    // claims to have.
    let kustomization = read(KUSTOMIZATION);
    for manifest in [
        "namespace.yaml",
        "workspace-pvc.yaml",
        "deployment.yaml",
        "service.yaml",
        "networkpolicy.yaml",
    ] {
        assert!(
            kustomization.contains(manifest),
            "{KUSTOMIZATION} does not include {manifest} — an unlisted manifest is never applied"
        );
    }
}

/// C-480: the guest profile is hardened, keeps the sandbox floor, and states its file modes.
#[test]
fn the_guest_profile_is_hardened_and_keeps_the_sandbox_floor() {
    let unit = read(UNIT);
    for (required, why) in [
        ("User=flux", "a dedicated non-root service identity"),
        ("NoNewPrivileges=true", "no privilege escalation"),
        (
            "ProtectSystem=strict",
            "the guest is read-only to this service",
        ),
        (
            "ReadWritePaths=/srv/flux/workspace",
            "the workspace is the blast radius",
        ),
        ("PrivateTmp=true", "no shared /tmp"),
        ("ProtectHome=true", "no home directories"),
        (
            "EnvironmentFile=/etc/flux/remote-system.env",
            "the token never reaches argv",
        ),
        ("RestrictAddressFamilies=", "no exotic sockets"),
        ("RequiresMountsFor=/srv/flux/workspace", "the durable disk"),
    ] {
        assert!(
            unit.contains(required),
            "{UNIT} no longer sets `{required}` ({why})"
        );
    }

    // The guest keeps the floor. That is the reason to run a guest rather than a container, so a
    // `--no-sandbox` appearing in ExecStart is a silent downgrade of the whole profile.
    let exec_start: String = unit
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("ExecStart="))
        .take_while(|line| !line.trim().is_empty())
        .collect();
    assert!(
        !exec_start.contains("--no-sandbox"),
        "{UNIT}'s ExecStart waives the sandbox floor. A guest owns its kernel, so bubblewrap works \
         here; waiving it removes the reason to run a guest at all"
    );

    let install = read(INSTALL);
    for (required, why) in [
        (
            "sha256sum --check",
            "the release archive is verified, not trusted",
        ),
        (
            "chmod 0600 /etc/flux/remote-system.env",
            "the token file mode",
        ),
        ("chmod 0640", "the TLS private key mode"),
        ("useradd", "the non-root service identity"),
        ("flux.previous", "a rollback that does not need the network"),
        ("bwrap", "the sandbox floor's backend is checked for"),
    ] {
        assert!(
            install.contains(required),
            "{INSTALL} no longer does `{required}` ({why})"
        );
    }

    let cloud_init = read(CLOUD_INIT);
    assert!(
        cloud_init.starts_with("#cloud-config"),
        "{CLOUD_INIT} must begin with the #cloud-config marker or cloud-init ignores it"
    );
    for (required, why) in [
        (
            "/srv/flux/workspace",
            "the durable workspace disk is mounted",
        ),
        ("bubblewrap", "the guest keeps the sandbox floor"),
        ("8790", "the firewall admits only the daemon's port"),
        ("policy drop", "the firewall default is deny"),
    ] {
        assert!(
            cloud_init.contains(required),
            "{CLOUD_INIT} no longer covers `{required}` ({why})"
        );
    }
}

/// C-480: every flag the artifacts pass is one the shipped binary still accepts. A renamed flag
/// turns every profile into a daemon that will not start, and nothing else in the tree would notice.
#[test]
fn every_flag_the_profiles_pass_is_one_the_shipped_binary_accepts() {
    let serve_help = flux_help(&["system", "serve"]);
    let global_help = flux_help(&[]);

    let mut checked = 0usize;
    let mut missing = Vec::new();
    for artifact in [DOCKERFILE, DEPLOYMENT, UNIT] {
        let argv = daemon_argv(artifact, &read(artifact));
        let flags = flags_in(&argv);
        assert!(
            flags.len() >= 4,
            "recovered only {} flag(s) from {artifact}'s argv — the extractor stopped matching, \
             which would make this whole check vacuous",
            flags.len()
        );
        for flag in flags {
            if !serve_help.contains(&flag) && !global_help.contains(&flag) {
                missing.push(format!("{artifact}: {flag}"));
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 10,
        "expected to recover the profiles' argv, found only {checked} flags"
    );
    assert!(
        missing.is_empty(),
        "these deployment artifacts pass flags `flux system serve` does not accept — every profile \
         would fail to start:\n  {}",
        missing.join("\n  ")
    );

    // The floor waiver has to be a real flag, or the container profiles silently mean nothing.
    assert!(
        global_help.contains("--no-sandbox"),
        "`--no-sandbox` is gone from the CLI, but the container and pod profiles depend on it"
    );
}

/// C-480: upgrade, rollback, and what a mismatched pair actually does — documented with the strings
/// the code produces, not with a paraphrase that can drift away from them.
#[test]
fn upgrade_rollback_and_protocol_mismatch_match_the_shipped_behavior() {
    let system = read("crates/flux-server/src/system.rs");
    let version = system
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pub const PROTOCOL_VERSION: u32 = ")?
                .strip_suffix(';')
        })
        .expect("the remote-system protocol version constant")
        .to_string();

    // Both halves of the refusal, taken from the source that emits them.
    for template in [
        "unsupported remote-system protocol version",
        "remote-system protocol mismatch: local",
    ] {
        assert!(
            system.contains(template),
            "crates/flux-server/src/system.rs no longer emits `{template}` — re-derive the guide"
        );
    }

    let guide = read(PUBLIC_GUIDE);
    for template in [
        "unsupported remote-system protocol version",
        "remote-system protocol mismatch: local",
    ] {
        assert!(
            guide.contains(template),
            "{PUBLIC_GUIDE} does not state what a mismatched peer actually sees (`{template}`)"
        );
    }
    assert!(
        guide.contains(&format!("remote-system protocol mismatch: local {version}")),
        "{PUBLIC_GUIDE}'s protocol-mismatch example is not the shipped protocol version \
         ({version}) — an operator reading it would compare against the wrong number"
    );

    // Upgrade and rollback, for each profile, in the public guide.
    let upgrade_section = guide
        .split("## ")
        .find(|section| section.starts_with("Upgrade, rollback, and protocol mismatch"))
        .expect("the public guide has an upgrade/rollback section");
    for required in [
        "build-image.sh",
        "kustomization.yaml",
        "rollout undo",
        "install-flux-system.sh",
        "flux.previous",
    ] {
        assert!(
            upgrade_section.contains(required),
            "{PUBLIC_GUIDE}'s upgrade/rollback section omits `{required}` — a profile with no \
             stated way back is a profile nobody can safely upgrade"
        );
    }
    assert!(
        upgrade_section.contains("Unknown"),
        "{PUBLIC_GUIDE} must say what a restart does to an accepted-but-unanswered operation"
    );
}

/// C-480: the public guide points at the artifacts that ship, and no artifact claims Flux
/// provisions the infrastructure it runs on.
#[test]
fn the_public_guide_uses_the_shipped_artifacts_and_states_the_provisioning_boundary() {
    let guide = read(PUBLIC_GUIDE);
    for artifact in [
        "deploy/container/build-image.sh",
        "deploy/kubernetes",
        "deploy/vm",
    ] {
        assert!(
            guide.contains(artifact),
            "{PUBLIC_GUIDE} does not point at the shipped `{artifact}` — the guide is the only \
             place an operator learns these exist"
        );
    }
    assert!(
        !guide.contains("does not publish an official image"),
        "{PUBLIC_GUIDE} still tells operators to bring their own image"
    );

    // The boundary, stated wherever an operator or a contributor could form the wrong expectation.
    for document in [PUBLIC_GUIDE, DEPLOY_README, VM_README] {
        let source = read(document).to_ascii_lowercase();
        assert!(
            source.contains("firecracker")
                && source.contains("kata")
                && (source.contains("does not provision") || source.contains("does not create")),
            "{document} must state that Flux does not provision Docker hosts, clusters or microVMs"
        );
    }

    // Every artifact is indexed somewhere a reader will land: the top-level map, or the README
    // sitting beside it. An artifact nothing indexes is an artifact nobody applies.
    let index = read(DEPLOY_README);
    let profile_readmes: String = [KUBERNETES_README, VM_README]
        .iter()
        .map(|r| read(r))
        .collect();
    let mut unindexed = Vec::new();
    for artifact in all_artifacts() {
        let file_name = artifact.rsplit('/').next().expect("artifact file name");
        let relative = artifact.trim_start_matches("deploy/");
        let indexed = [artifact, relative, file_name]
            .iter()
            .any(|needle| index.contains(needle) || profile_readmes.contains(needle));
        if !indexed {
            unindexed.push(artifact);
        }
    }
    assert!(
        unindexed.is_empty(),
        "no README mentions {unindexed:?} — document each artifact in {DEPLOY_README} or in the \
         README beside it"
    );
    assert!(
        read(KUBERNETES_README).contains("kubectl apply -k"),
        "{KUBERNETES_README} must show how the profile is applied"
    );
}
