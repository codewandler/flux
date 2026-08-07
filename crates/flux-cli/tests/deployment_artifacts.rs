//! Executable contract for the shipped deployment artifacts — the remote-system substrate (C-480)
//! and the agent surface (C-685).
//!
//! `deploy/` turns the BYO recipe in the deployment guide into artifacts an operator applies. That
//! only helps if the artifacts stay true to the daemon they package, so this suite derives what it
//! can from the code — the flags the shipped binary accepts, the protocol version it enforces, the
//! routes it leaves auth-exempt — and pins the rest: the controls each profile promises, the secret
//! material none of them may carry, and the provisioning boundary all of them state.
//!
//! Two axes are packaged here, and they are not the same deployment
//! (`docs/designs/operating-a-deployed-host.md`):
//!
//! - **substrate** — `deploy/kubernetes/` runs `flux system serve`, so only guarded effects land in
//!   the pod; the model, the approvals and the session stay on the operator's machine.
//! - **agent** — `deploy/agent/` runs `flux app run --serve`, so the *whole* agent lives in the
//!   pod. It reuses the substrate's released image with a command override; a second image would
//!   be a second thing to attest.
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

// C-685 — the agent surface's Kubernetes profile. A sibling base rather than an overlay on
// `deploy/kubernetes/`: it runs a different program (`flux app run --serve`, not `flux system
// serve`) on a different port, with a different secret, a different volume and a public health
// route, so an overlay would patch every field it inherited and the two would collide on names in
// a cluster running both.
const AGENT_KUSTOMIZATION: &str = "deploy/agent/kustomization.yaml";
const AGENT_NAMESPACE: &str = "deploy/agent/namespace.yaml";
const AGENT_DEPLOYMENT: &str = "deploy/agent/deployment.yaml";
const AGENT_SERVICE: &str = "deploy/agent/service.yaml";
const AGENT_PVC: &str = "deploy/agent/state-pvc.yaml";
const AGENT_NETWORKPOLICY: &str = "deploy/agent/networkpolicy.yaml";
const AGENT_README: &str = "deploy/agent/README.md";
const AGENT_GUIDE: &str = "website/docs/agent/deployment.md";

/// Where the CLI's served-agent auth is resolved, and where the served surface refuses an
/// unauthenticated non-loopback bind. The manifests are checked against these, not against a
/// remembered spelling.
const APP_CMD: &str = "crates/flux-cli/src/app_cmd.rs";
const SERVER: &str = "crates/flux-server/src/lib.rs";

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
        AGENT_KUSTOMIZATION,
        AGENT_NAMESPACE,
        AGENT_DEPLOYMENT,
        AGENT_SERVICE,
        AGENT_PVC,
        AGENT_NETWORKPOLICY,
    ]
}

/// Every Kubernetes manifest that starts a listener, across both profiles. The Dockerfile and the
/// systemd unit are deliberately absent: neither can carry a Kubernetes Secret reference, and the
/// binary's own refusal is what protects them (`guard_open_bind`, flux-server).
fn listener_manifests() -> Vec<&'static str> {
    vec![DEPLOYMENT, AGENT_DEPLOYMENT]
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

/// A `command:`/`args:` block-sequence a Kubernetes container declares, flattened to argv.
///
/// The `take_while` stops at the first line that is not a `- ` item, so a **commented** entry ends
/// the list. That is deliberate and load-bearing for C-685: the agent profile parks its alternative
/// approval posture as a trailing `# - --remote-approval`, and it must read as documentation rather
/// than as a second active flag.
fn yaml_list(source: &str, key: &str) -> String {
    source
        .lines()
        .skip_while(|line| line.trim() != key)
        .skip(1)
        .take_while(|line| line.trim().starts_with("- "))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Only the argv the artifact hands to the flux binary — never the surrounding `useradd`,
/// `apt-get` or systemd directives, whose flags belong to other programs entirely.
fn daemon_argv(artifact: &str, source: &str) -> String {
    match artifact {
        DOCKERFILE => format!(
            "{} {}",
            continued_directive(source, "ENTRYPOINT"),
            continued_directive(source, "CMD")
        ),
        DEPLOYMENT => yaml_list(source, "args:"),
        // The agent profile overrides the image entrypoint, so its subcommand path lives in
        // `command:` and only the flags live in `args:`. Both halves are the argv.
        AGENT_DEPLOYMENT => format!(
            "{} {}",
            yaml_list(source, "command:"),
            yaml_list(source, "args:")
        ),
        UNIT => continued_directive(source, "ExecStart="),
        other => panic!("no argv extractor for {other}"),
    }
}

/// The `key: value` a YAML manifest sets, first occurrence, unquoted and uncommented.
fn yaml_scalar(source: &str, key: &str) -> Option<String> {
    source.lines().map(str::trim).find_map(|line| {
        line.strip_prefix(key).map(|rest| {
            rest.trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        })
    })
}

/// An extracted argv as the words the binary would actually receive: the YAML block-sequence `-`
/// markers and JSON-array punctuation of the source form are separators, not arguments.
fn argv_tokens(argv: &str) -> Vec<String> {
    argv.split_whitespace()
        .map(|token| token.trim_matches(|c| c == '"' || c == ',' || c == '[' || c == ']'))
        .filter(|token| !token.is_empty() && *token != "-")
        .map(str::to_string)
        .collect()
}

/// The value an argv gives an option that takes one, in either spelling: `--flag value` or
/// `--flag=value`.
fn argv_value(argv: &str, flag: &str) -> Option<String> {
    let tokens = argv_tokens(argv);
    for (index, token) in tokens.iter().enumerate() {
        if let Some(inline) = token.strip_prefix(&format!("{flag}=")) {
            return Some(inline.to_string());
        }
        if token == flag {
            return tokens
                .get(index + 1)
                .filter(|n| !n.starts_with('-'))
                .cloned();
        }
    }
    None
}

/// Every address an argv asks a flux listener to bind: `--bind <addr>` (the substrate daemon) and
/// `--serve <addr>` / `--serve=<addr>` (the agent surface).
fn bind_addresses(argv: &str) -> Vec<String> {
    ["--bind", "--serve"]
        .into_iter()
        .filter_map(|flag| argv_value(argv, flag))
        .collect()
}

/// Mirrors `addr_is_loopback` in `crates/flux-cli/src/app_cmd.rs` and `unauthenticated_bind_allowed`
/// in `crates/flux-server/src/lib.rs`: the question both of them ask before admitting an
/// unauthenticated listener. Restated rather than imported — neither is public — and the test below
/// pins that the CLI still asks it, so a rename cannot leave this copy answering about nothing.
fn addr_is_loopback(addr: &str) -> bool {
    use std::net::{IpAddr, SocketAddr};
    if let Ok(socket) = addr.parse::<SocketAddr>() {
        return socket.ip().is_loopback();
    }
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// The env var names a manifest populates from a Kubernetes Secret. A value that is not a
/// `secretKeyRef` is a value somebody typed into a file, which is the thing these profiles exist
/// to avoid.
fn secret_backed_env(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let mut names = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(name) = line.strip_prefix("- name: ") else {
            continue;
        };
        // `valueFrom:` / `secretKeyRef:` / `name:` / `key:` — within a short window of the env entry,
        // and before the next entry starts.
        let following = lines
            .iter()
            .skip(index + 1)
            .take_while(|next| !next.starts_with("- name: "))
            .take(6)
            .any(|next| next.starts_with("secretKeyRef:"));
        if following {
            names.push(name.trim().to_string());
        }
    }
    names
}

/// The routes `flux app run --serve` registers OUTSIDE its authentication layer — the documented
/// health/discovery exemptions `AGENTS.md` carves out of "keep served HTTP routes authenticated".
///
/// Read from the single-agent router (the first `let exempt` block, which is the one `serve` and
/// `serve_with_approvals` build) so a probe in a manifest can be checked against the shipped
/// exemption set rather than against a remembered path.
fn auth_exempt_routes() -> Vec<String> {
    let server = read(SERVER);
    let block: String = server
        .lines()
        .skip_while(|line| !line.trim().starts_with("let exempt = Router::new()"))
        .take_while(|line| !line.trim().starts_with(".layer("))
        .collect::<Vec<_>>()
        .join("\n");
    let mut routes: Vec<String> = block
        .match_indices(".route(\"")
        .filter_map(|(index, _)| {
            let rest = &block[index + ".route(\"".len()..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .collect();
    routes.sort();
    routes.dedup();
    assert!(
        routes.contains(&"/health".to_string()),
        "{SERVER}'s auth-exempt router no longer registers /health — the extractor stopped \
         matching, which would make every probe check below vacuous"
    );
    routes
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
    for manifest in [
        KUSTOMIZATION,
        DEPLOYMENT,
        SERVICE,
        PVC,
        NETWORKPOLICY,
        AGENT_KUSTOMIZATION,
        AGENT_NAMESPACE,
        AGENT_DEPLOYMENT,
        AGENT_SERVICE,
        AGENT_PVC,
        AGENT_NETWORKPOLICY,
    ] {
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
    // C-685: the agent profile drives a different subcommand, so it is checked against that
    // subcommand's own help rather than the daemon's.
    let app_run_help = flux_help(&["app", "run"]);
    let global_help = flux_help(&[]);

    let mut checked = 0usize;
    let mut missing = Vec::new();
    for (artifact, subcommand_help) in [
        (DOCKERFILE, &serve_help),
        (DEPLOYMENT, &serve_help),
        (UNIT, &serve_help),
        (AGENT_DEPLOYMENT, &app_run_help),
    ] {
        let argv = daemon_argv(artifact, &read(artifact));
        let flags = flags_in(&argv);
        assert!(
            flags.len() >= 4,
            "recovered only {} flag(s) from {artifact}'s argv — the extractor stopped matching, \
             which would make this whole check vacuous",
            flags.len()
        );
        for flag in flags {
            if !subcommand_help.contains(&flag) && !global_help.contains(&flag) {
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
    let profile_readmes: String = [KUBERNETES_README, VM_README, AGENT_README]
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

/// C-685: the agent surface runs the image the substrate profile already ships, reached by a
/// command override. A second image is a second thing to build, attest, publish and get wrong.
#[test]
fn the_agent_profile_runs_the_released_image_and_never_a_second_one() {
    let substrate = read(KUSTOMIZATION);
    let agent = read(AGENT_KUSTOMIZATION);

    // Derived, not restated: whatever image the substrate profile pins, the agent profile pins the
    // same name and the same tag. A profile that drifts to its own tag is running a release nobody
    // attested against this one.
    for key in ["newName:", "newTag:"] {
        let expected = yaml_scalar(&substrate, key)
            .unwrap_or_else(|| panic!("{KUSTOMIZATION} pins the image `{key}`"));
        let actual = yaml_scalar(&agent, key)
            .unwrap_or_else(|| panic!("{AGENT_KUSTOMIZATION} must pin the image `{key}`"));
        assert_eq!(
            actual, expected,
            "{AGENT_KUSTOMIZATION} pins `{key} {actual}` while {KUSTOMIZATION} pins `{expected}` — \
             the agent surface must run the SAME released image as the substrate, not a second one"
        );
    }
    let substrate_image =
        yaml_scalar(&read(DEPLOYMENT), "image:").expect("the substrate Deployment names an image");
    let agent_image = yaml_scalar(&read(AGENT_DEPLOYMENT), "image:")
        .expect("the agent Deployment names an image");
    assert_eq!(
        agent_image, substrate_image,
        "{AGENT_DEPLOYMENT} references image `{agent_image}` but {DEPLOYMENT} references \
         `{substrate_image}` — one image, reached two ways"
    );

    // The image's entrypoint is the substrate daemon, so the agent profile can only be a command
    // override. Both halves are derived from the Dockerfile that actually ships.
    let dockerfile = read(DOCKERFILE);
    let entrypoint = continued_directive(&dockerfile, "ENTRYPOINT");
    assert!(
        entrypoint.contains("\"system\", \"serve\""),
        "{DOCKERFILE}'s ENTRYPOINT is no longer `flux system serve` — re-derive what the agent \
         profile has to override"
    );
    let binary = "/usr/local/bin/flux";
    assert!(
        entrypoint.contains(binary),
        "{DOCKERFILE} no longer installs the binary at {binary}"
    );
    let command = yaml_list(&read(AGENT_DEPLOYMENT), "command:");
    for expected in [binary, "app", "run"] {
        assert!(
            command.contains(expected),
            "{AGENT_DEPLOYMENT}'s `command:` must override the image entrypoint with \
             `{binary} app run` (missing `{expected}`); it currently reads `{command}`"
        );
    }
    assert!(
        !command.contains("system"),
        "{AGENT_DEPLOYMENT} still runs the substrate daemon — the agent surface is `flux app run \
         --serve`, a different program in the same image"
    );

    // No build recipe of its own. A Dockerfile under deploy/agent/ is exactly how a second image
    // gets built without anyone deciding to build one.
    let agent_dir = repo_path("deploy/agent");
    let strays: Vec<String> = fs::read_dir(&agent_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", agent_dir.display()))
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            (lower.contains("dockerfile") || lower.ends_with(".sh")).then_some(name)
        })
        .collect();
    assert!(
        strays.is_empty(),
        "deploy/agent/ carries an image build recipe ({strays:?}) — the agent surface reuses \
         {DOCKERFILE}, and a second recipe is a second image to attest"
    );
}

/// C-685: no shipped manifest expresses an unauthenticated non-loopback listener.
///
/// Both halves are structural. The binary refuses such a bind at startup
/// (`guard_open_bind`, flux-server), and the manifest that would provoke the refusal must carry the
/// Secret-backed token that makes the bind legitimate — required, never `optional`, so a cluster
/// missing the Secret fails to start the container instead of starting an open one.
#[test]
fn no_manifest_binds_a_public_address_without_a_token() {
    // The refusal these manifests are written against still exists, so the check below is about
    // something.
    let server = read(SERVER);
    assert!(
        server.contains("refusing to build an unauthenticated router for non-loopback bind"),
        "{SERVER} no longer refuses an unauthenticated non-loopback bind — the release boundary \
         these manifests encode has moved"
    );
    assert!(
        read(APP_CMD).contains("refusing to serve on a non-loopback address"),
        "{APP_CMD} no longer refuses to serve `flux app run --serve` on a non-loopback address \
         without authentication"
    );

    let mut public_binds = 0usize;
    let mut violations = Vec::new();
    for manifest in listener_manifests() {
        let source = read(manifest);
        let addresses = bind_addresses(&daemon_argv(manifest, &source));
        assert!(
            !addresses.is_empty(),
            "recovered no bind address from {manifest} — the extractor stopped matching, which \
             would make this whole check vacuous"
        );
        let secret_env = secret_backed_env(&source);
        for address in addresses {
            if addr_is_loopback(&address) {
                continue;
            }
            public_binds += 1;
            let token_env: Vec<&String> = secret_env
                .iter()
                .filter(|name| name.ends_with("_TOKEN"))
                .collect();
            if token_env.is_empty() {
                violations.push(format!(
                    "{manifest} binds {address} but populates no `*_TOKEN` env var from a \
                     secretKeyRef"
                ));
            }
            if source.contains("optional: true") {
                violations.push(format!(
                    "{manifest} binds {address} and marks a Secret reference `optional: true` — an \
                     optional token is a listener that can come up with no token at all"
                ));
            }
        }
    }
    assert!(
        public_binds >= 2,
        "expected both profiles to bind a non-loopback address, found {public_binds}"
    );
    assert!(
        violations.is_empty(),
        "a deployment manifest expresses an unauthenticated non-loopback listener, which \
         AGENTS.md forbids outright:\n  {}",
        violations.join("\n  ")
    );

    // The agent profile must read the token through the exact env var the CLI resolves its
    // shared-secret auth from — derived from the source, so a rename breaks the test and not the
    // deployment.
    let app_cmd = read(APP_CMD);
    let token_var = app_cmd
        .lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix("let token = std::env::var(\"")?;
            rest.split('"').next().map(str::to_string)
        })
        .expect("`server_auth_from_config` reads the served agent's bearer token from an env var");
    assert!(
        secret_backed_env(&read(AGENT_DEPLOYMENT)).contains(&token_var),
        "{AGENT_DEPLOYMENT} must populate `{token_var}` from a Secret — that is the variable \
         `server_auth_from_config` ({APP_CMD}) reads, and without it the served agent is either \
         open or refuses to start"
    );
}

/// C-685: the agent profile carries every control its acceptance names, in the files that would
/// actually be applied.
#[test]
fn the_agent_profile_carries_every_required_control() {
    let deployment = read(AGENT_DEPLOYMENT);
    for (required, why) in [
        ("replicas: 1", "one replica per session store"),
        ("type: Recreate", "two pods must never hold one store"),
        ("runAsNonRoot: true", "non-root"),
        ("type: RuntimeDefault", "seccomp"),
        ("readOnlyRootFilesystem: true", "read-only root filesystem"),
        ("allowPrivilegeEscalation: false", "no privilege escalation"),
        ("- ALL", "all capabilities dropped"),
        ("fsGroup: 10001", "the agent can write its own store"),
        (
            "automountServiceAccountToken: false",
            "an agent in a pod must not be handed the cluster API",
        ),
        ("readinessProbe", "readiness"),
        ("livenessProbe", "liveness"),
        ("secretKeyRef", "the bearer token comes from a Secret"),
        ("persistentVolumeClaim", "the durable session store"),
        (
            "--no-sandbox",
            "the sandbox floor is waived explicitly, not silently",
        ),
    ] {
        assert!(
            deployment.contains(required),
            "{AGENT_DEPLOYMENT} no longer carries `{required}` ({why})"
        );
    }

    assert!(
        read(AGENT_SERVICE).contains("type: ClusterIP"),
        "{AGENT_SERVICE} must stay ClusterIP — an agent endpoint reached from outside the cluster \
         goes through an ingress that keeps the bearer token intact, not a LoadBalancer added here"
    );
    assert!(
        read(AGENT_NAMESPACE).contains("pod-security.kubernetes.io/enforce: restricted"),
        "{AGENT_NAMESPACE} must enforce the restricted Pod Security Standard"
    );
    assert!(
        read(AGENT_PVC).contains("kind: PersistentVolumeClaim"),
        "{AGENT_PVC} must claim durable storage for the session store"
    );

    // Session durability, derived: the directory the argv names as the store has to be a mount
    // point in the same pod, and the volume behind it has to be the claim.
    let argv = daemon_argv(AGENT_DEPLOYMENT, &deployment);
    let store = argv_value(&argv, "--store").unwrap_or_else(|| {
        panic!("{AGENT_DEPLOYMENT} must name the session store directory with `--store <DIR>`")
    });
    assert!(
        deployment.contains(&format!("mountPath: {store}")),
        "{AGENT_DEPLOYMENT} stores sessions in `{store}` but never mounts a volume there — a \
         restart would lose every session it claims to keep"
    );

    // Default-deny plus ONE explicit operator allowance. An empty `from:` admits the whole cluster
    // and is exactly the shortcut the substrate profile already refuses to take.
    let policy = read(AGENT_NETWORKPOLICY);
    let default_deny = policy
        .split("---")
        .find(|document| document.contains("name: default-deny"))
        .expect("a default-deny NetworkPolicy");
    assert!(
        default_deny.contains("podSelector: {}")
            && default_deny.contains("- Ingress")
            && default_deny.contains("- Egress"),
        "{AGENT_NETWORKPOLICY}'s default-deny must select every pod and deny both directions"
    );
    let ingress = policy
        .split("---")
        .find(|document| document.contains("ingress:"))
        .expect("an explicit ingress allowance for the operator path");
    assert!(
        ingress.contains("namespaceSelector:") || ingress.contains("podSelector:"),
        "{AGENT_NETWORKPOLICY}'s operator ingress must name who may reach the agent"
    );
    assert!(
        !ingress.contains("- from: []") && !ingress.contains("from: []"),
        "{AGENT_NETWORKPOLICY}'s operator ingress admits the whole cluster; a bearer token would \
         be the only barrier left"
    );

    // The base has to name every manifest, or an applied profile is missing a control the file set
    // claims to have.
    let kustomization = read(AGENT_KUSTOMIZATION);
    for manifest in [
        "namespace.yaml",
        "state-pvc.yaml",
        "deployment.yaml",
        "service.yaml",
        "networkpolicy.yaml",
    ] {
        assert!(
            kustomization.contains(manifest),
            "{AGENT_KUSTOMIZATION} does not include {manifest} — an unlisted manifest is never \
             applied"
        );
    }
}

/// C-685: the agent profile chooses an approval posture out loud, chooses exactly one, and
/// documents the other.
#[test]
fn the_agent_profile_states_exactly_one_approval_posture() {
    let deployment = read(AGENT_DEPLOYMENT);
    let argv = daemon_argv(AGENT_DEPLOYMENT, &deployment);
    let tokens = argv_tokens(&argv);
    let active: Vec<&str> = ["--yes", "--remote-approval"]
        .into_iter()
        .filter(|flag| tokens.iter().any(|token| token == flag))
        .collect();
    assert_eq!(
        active.len(),
        1,
        "{AGENT_DEPLOYMENT} passes {active:?} — `flux app run --serve` with no program refuses to \
         start unless exactly one of `--yes` / `--remote-approval` is chosen \
         (`ServedApprovalPosture::select`, {APP_CMD}), so the manifest must choose one and only one"
    );

    // The refusal the manifest is written against still exists.
    assert!(
        read(APP_CMD).contains("needs an approval posture"),
        "{APP_CMD} no longer refuses a served agent with no approval posture"
    );

    // Both options documented, wherever an operator decides between them.
    for document in [AGENT_DEPLOYMENT, AGENT_README, AGENT_GUIDE] {
        let source = read(document);
        for option in ["--yes", "--remote-approval"] {
            assert!(
                source.contains(option),
                "{document} must document the `{option}` posture — a deployed agent's approval \
                 posture is a decision, and half of it is not a decision"
            );
        }
        assert!(
            source.contains("C-687"),
            "{document} must note that `--remote-approval` supports only the shared operator token \
             (or open loopback) until the supervisor authorization model lands (C-687)"
        );
    }
    // Not a paraphrase: the constraint the profile passes on to operators comes from the flag's own
    // documentation, so C-687 landing is what retires it rather than someone remembering to.
    assert!(
        read("crates/flux-cli/src/args.rs")
            .contains("auth is refused until approvals have a distinct supervisor"),
        "the `--remote-approval` flag no longer documents that principal auth is refused — \
         re-derive what the agent profile tells operators about C-687"
    );
}

/// C-685: the pod's probes use a route that is authenticated-exempt by construction, so no manifest
/// ever needs to carry the bearer token to prove liveness.
#[test]
fn the_agent_probes_target_an_auth_exempt_route() {
    let deployment = read(AGENT_DEPLOYMENT);
    let exempt = auth_exempt_routes();
    let probe_paths: Vec<String> = deployment
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("path: ").map(str::to_string))
        .collect();
    assert!(
        !probe_paths.is_empty(),
        "{AGENT_DEPLOYMENT} declares no HTTP probe path — the served agent has a documented \
         unauthenticated `/health`, so it does not need the substrate's TCP-only probe"
    );
    for path in &probe_paths {
        assert!(
            exempt.contains(path),
            "{AGENT_DEPLOYMENT} probes `{path}`, which is not one of the routes \
             {SERVER} registers outside its authentication layer ({exempt:?}) — a probe against a \
             protected route would need the bearer token in the manifest"
        );
    }
    // Uncommented lines only: `--yes` is described in prose as "Authorization policy … constrains
    // this agent", and that sentence is not a header.
    let declared: Vec<&str> = deployment
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .collect();
    for smell in ["httpHeaders", "Authorization"] {
        assert!(
            !declared.iter().any(|line| line.contains(smell)),
            "{AGENT_DEPLOYMENT} declares `{smell}` — a probe that authenticates would need the \
             bearer token in the manifest, which is the thing the Secret exists to avoid"
        );
    }
}

/// C-685: an operator can get from a workstation to the deployed agent, and knows what a restart
/// costs them, without leaving the docs.
#[test]
fn reaching_the_deployed_agent_is_documented_end_to_end() {
    let guide = read(AGENT_GUIDE);
    for (required, why) in [
        ("flux a2a", "the client an operator actually types"),
        ("FLUX_A2A_TOKEN", "how the bearer token reaches that client"),
        ("port-forward", "the zero-config route into a ClusterIP"),
        (
            "/.well-known/agent-card.json",
            "the discovery endpoint that proves the agent answered",
        ),
        (
            "kubectl apply -k deploy/agent",
            "how the profile is applied",
        ),
        (
            "channel",
            "whether a program's channel endpoints are exposed alongside the agent",
        ),
    ] {
        assert!(
            guide.contains(required),
            "{AGENT_GUIDE} does not cover `{required}` ({why})"
        );
    }

    // What survives a restart, stated rather than implied.
    let durability = guide
        .split("## ")
        .find(|section| section.starts_with("What survives a restart"))
        .unwrap_or_else(|| {
            panic!("{AGENT_GUIDE} must have a `## What survives a restart` section")
        });
    for required in ["--store", "does not"] {
        assert!(
            durability.contains(required),
            "{AGENT_GUIDE}'s restart section omits `{required}` — an operator has to be able to \
             tell what the volume keeps from what it does not"
        );
    }

    // The same provisioning boundary every other profile states.
    for document in [AGENT_GUIDE, AGENT_README] {
        let source = read(document).to_ascii_lowercase();
        assert!(
            source.contains("does not provision") || source.contains("does not create"),
            "{document} must state that Flux does not provision the cluster it runs on"
        );
    }

    // An unlinked page is a page nobody reads.
    let sidebar = read("website/sidebars.js");
    let slug = AGENT_GUIDE
        .trim_start_matches("website/docs/")
        .trim_end_matches(".md");
    assert!(
        sidebar.contains(&format!("'{slug}'")),
        "website/sidebars.js does not list `{slug}` — an unlinked deployment guide is one an \
         operator never finds"
    );

    // The contributor's map indexes the new profile beside the ones that already ship.
    assert!(
        read(DEPLOY_README).contains("deploy/agent") || read(DEPLOY_README).contains("agent/"),
        "{DEPLOY_README} does not index the agent-surface profile"
    );
    assert!(
        read(AGENT_README).contains("kubectl apply -k"),
        "{AGENT_README} must show how the profile is applied"
    );
}
