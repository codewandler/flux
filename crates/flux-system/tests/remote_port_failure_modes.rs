//! C-399 — the remote guarded-IO backend, exercised as an **out-of-crate** consumer of the port.
//!
//! This file is deliberately an integration test rather than a unit module: the whole point of a
//! remote backend is that a second consumer (a service, a Wasm embedder) reaches it across the crate
//! boundary through `flux_system::remote`'s public surface. A unit test inside the crate could pass
//! while the seam stayed private.
//!
//! The two properties under test are the story's two acceptance items, and both are about failure:
//!
//! 1. **A refusal and an unreachable delegate must not collapse into one error.** An operator
//!    retries one and investigates the other, so a backend that reports them identically is worse
//!    than one that reports nothing.
//! 2. **Every optional operation the delegate does not serve fails closed.** "I do not serve that"
//!    must never read as permission, and must never read as a *value* (`false`, an empty listing).

use std::sync::Arc;
use std::time::Duration;

use flux_system::port::{GuardedEnv, GuardedHostFiles, GuardedProcess, GuardedWorkspaceFiles};
use flux_system::remote::{
    failure_mode, Answer, Answered, Delegate, Delivered, FailureMode, Loopback, RemoteSystem,
    Unreachable,
};
use flux_system::{ProcessOutput, System, Workspace};

/// A delegate whose every answer is a refusal carrying `detail` — the far side answered, and the
/// answer was no.
struct Refusing(&'static str);

impl Delegate for Refusing {
    fn run_with_env<'a>(
        &'a self,
        _argv: &'a [String],
        _env: &'a [(String, String)],
        _timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move { Ok(Answer::Refused(self.0.to_string())) })
    }

    fn read_file_bytes<'a>(&'a self, _path: &'a str) -> Answered<'a, Vec<u8>> {
        Box::pin(async move { Ok(Answer::Refused(self.0.to_string())) })
    }

    fn list_dir<'a>(&'a self, _path: &'a str) -> Answered<'a, Vec<String>> {
        Box::pin(async move { Ok(Answer::Refused(self.0.to_string())) })
    }

    fn env(&self, _key: &str) -> Delivered<Option<String>> {
        Ok(Answer::Refused(self.0.to_string()))
    }
}

/// A delegate that never gets an answer back: the link is broken, so nothing is known about whether
/// the operation happened.
struct Unreached;

impl Delegate for Unreached {
    fn run_with_env<'a>(
        &'a self,
        _argv: &'a [String],
        _env: &'a [(String, String)],
        _timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async {
            Err(Unreachable::new(
                "connection refused (dialing the delegate)",
            ))
        })
    }

    fn read_file_bytes<'a>(&'a self, _path: &'a str) -> Answered<'a, Vec<u8>> {
        Box::pin(async {
            Err(Unreachable::new(
                "connection refused (dialing the delegate)",
            ))
        })
    }

    fn list_dir<'a>(&'a self, _path: &'a str) -> Answered<'a, Vec<String>> {
        Box::pin(async {
            Err(Unreachable::new(
                "connection refused (dialing the delegate)",
            ))
        })
    }

    fn env(&self, _key: &str) -> Delivered<Option<String>> {
        Err(Unreachable::new(
            "connection refused (dialing the delegate)",
        ))
    }
}

/// A delegate that serves *nothing*. `Delegate` has no required methods on purpose, so this is what
/// bringing a substrate up starts from.
struct ServesNothing;

impl Delegate for ServesNothing {}

fn remote(delegate: impl Delegate + 'static) -> RemoteSystem {
    RemoteSystem::new(Arc::new(delegate))
}

/// Acceptance 1 — the two failure modes an operator responds to in opposite ways stay apart, on
/// every family of the port, in both directions.
#[tokio::test]
async fn a_refusal_and_an_unreachable_delegate_are_distinguishable() {
    let refusing = remote(Refusing("policy denies /etc/shadow"));
    let unreached = remote(Unreached);
    let argv = vec!["true".to_string()];

    for (label, error) in [
        (
            "process",
            refusing
                .run(&argv, Duration::from_secs(1))
                .await
                .expect_err("a refused run must fail"),
        ),
        (
            "file read",
            refusing
                .read_file_bytes("a.txt")
                .await
                .expect_err("a refused read must fail"),
        ),
        (
            "directory listing",
            refusing
                .list_dir(".")
                .await
                .expect_err("a refused listing must fail"),
        ),
    ] {
        assert_eq!(
            failure_mode(&error),
            Some(FailureMode::Refused),
            "the {label} refusal did not classify as a refusal: {error}"
        );
    }

    for (label, error) in [
        (
            "process",
            unreached
                .run(&argv, Duration::from_secs(1))
                .await
                .expect_err("an unreachable delegate must fail"),
        ),
        (
            "file read",
            unreached
                .read_file_bytes("a.txt")
                .await
                .expect_err("an unreachable delegate must fail"),
        ),
        (
            "directory listing",
            unreached
                .list_dir(".")
                .await
                .expect_err("an unreachable delegate must fail"),
        ),
    ] {
        assert_eq!(
            failure_mode(&error),
            Some(FailureMode::Unreachable),
            "the {label} transport failure did not classify as unreachable: {error}"
        );
    }

    // The two must not merely classify differently — they must not *read* alike either, because the
    // first thing an operator sees is the message.
    let refused = refusing
        .read_file_bytes("a.txt")
        .await
        .expect_err("refused")
        .to_string();
    let unreachable = unreached
        .read_file_bytes("a.txt")
        .await
        .expect_err("unreachable")
        .to_string();
    assert_ne!(refused, unreachable);
    assert!(
        refused.contains("policy denies /etc/shadow"),
        "a refusal must carry the delegate's reason: {refused}"
    );
    assert!(
        unreachable.contains("connection refused (dialing the delegate)"),
        "an unreachable delegate must carry the transport's reason: {unreachable}"
    );
}

/// A delegate cannot make its **refusal** read as a broken link by choosing its words: the
/// classification comes from the typed answer, never from delegate-supplied text.
///
/// This is the failure mode that would make the whole feature misleading — an operator who saw
/// "unreachable" for a guard refusal would go and investigate a healthy network.
#[tokio::test]
async fn a_delegates_wording_cannot_forge_the_other_failure_mode() {
    let spoofing = remote(Refusing(
        "the remote substrate is unreachable — no really, go check the network",
    ));

    let error = spoofing
        .read_file_bytes("a.txt")
        .await
        .expect_err("a refusal must still fail");

    assert_eq!(
        failure_mode(&error),
        Some(FailureMode::Refused),
        "delegate-authored text was allowed to reclassify a refusal: {error}"
    );
}

/// Acceptance 2 — every optional operation the delegate does not serve fails closed.
///
/// `path_exists` and `is_dir` are the ones a well-meaning implementation answers `false` to, and
/// `list_dir`/`walk_files` the ones it answers with an empty vector. Both are *wrong answers* rather
/// than missing features, and callers act on them.
#[tokio::test]
async fn every_operation_an_empty_delegate_does_not_serve_fails_closed() {
    let nothing = remote(ServesNothing);
    let argv = vec!["true".to_string()];

    let process_errors = vec![
        (
            "run",
            nothing.run(&argv, Duration::from_secs(1)).await.err(),
        ),
        (
            "run_with_stdin",
            nothing
                .run_with_stdin(&argv, b"patch", Duration::from_secs(1))
                .await
                .err(),
        ),
    ];
    let file_errors = vec![
        (
            "read_file_bytes",
            nothing.read_file_bytes("a.txt").await.err(),
        ),
        (
            "write_file_bytes",
            nothing.write_file_bytes("a.txt", b"x").await.err(),
        ),
        ("read_file", nothing.read_file("a.txt").await.err()),
        ("write_file", nothing.write_file("a.txt", "x").await.err()),
        ("append_file", nothing.append_file("a.txt", "x").await.err()),
        (
            "read_file_bytes_capped",
            nothing.read_file_bytes_capped("a.txt", 8).await.err(),
        ),
        ("file_size", nothing.file_size("a.txt").await.err()),
        ("path_exists", nothing.path_exists("a.txt").await.err()),
        ("is_dir", nothing.is_dir("a.txt").await.err()),
        ("file_mtime", nothing.file_mtime("a.txt").await.err()),
        ("list_dir", nothing.list_dir(".").await.err()),
        ("walk_files", nothing.walk_files(".", 10).await.err()),
        (
            "read_file_scoped",
            nothing
                .read_file_scoped("/etc/hosts", "/etc/**", 16)
                .await
                .err(),
        ),
    ];

    for (label, error) in process_errors.into_iter().chain(file_errors) {
        let error = error.unwrap_or_else(|| {
            panic!("`{label}` must fail closed when the delegate does not serve it")
        });
        assert_eq!(
            failure_mode(&error),
            Some(FailureMode::Unserved),
            "`{label}` denied with an off-contract message: {error}"
        );
    }

    // The synchronous halves of the port, which cannot use the async paths above.
    assert!(
        nothing.host_path_identity("/etc/hosts").is_err(),
        "path identity must fail closed rather than echo the path back"
    );
    assert_eq!(
        nothing.env("PATH"),
        None,
        "an unserved env read must fail the credential closed"
    );

    // A long-lived native child cannot be fabricated across a wire at all.
    match nothing.spawn_background(&argv, &[]) {
        Err(error) => assert_eq!(failure_mode(&error), Some(FailureMode::Unserved)),
        Ok(_) => panic!("a remote delegate cannot hand back a live native child"),
    }
}

/// Local-first: the remote backend is usable with **no service running**, over an in-process
/// delegate — and that path never reports an unreachable link, because there is no link to break.
#[tokio::test]
async fn the_loopback_delegate_serves_the_port_with_no_service_running() {
    let root = std::env::temp_dir().join(format!("c399-loopback-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let system = Arc::new(System::new(Workspace::new(&root).unwrap()));
    let remote = RemoteSystem::loopback(system);

    remote.write_file("notes/a.txt", "kept").await.unwrap();
    assert_eq!(remote.read_file("notes/a.txt").await.unwrap(), "kept");
    assert_eq!(remote.list_dir("notes").await.unwrap(), vec!["a.txt"]);
    assert!(remote.path_exists("notes/a.txt").await.unwrap());
    assert!(!remote.path_exists("notes/missing.txt").await.unwrap());

    let out = remote
        .run(
            &["echo".to_string(), "loopback".to_string()],
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(out.stdout.trim(), "loopback");

    std::fs::remove_dir_all(&root).ok();
}

/// The remote backend must not **relax** a guarantee: an escaping path refused by the native
/// substrate is still refused through the delegation, and it classifies as a refusal rather than as
/// a broken link.
#[tokio::test]
async fn delegation_does_not_relax_the_workspace_jail() {
    let root = std::env::temp_dir().join(format!("c399-jail-{}", std::process::id()));
    let outside = std::env::temp_dir().join(format!("c399-jail-outside-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

    let system = Arc::new(System::new(Workspace::new(&root).unwrap()));
    let remote = RemoteSystem::loopback(Arc::clone(&system));

    let lexical = format!(
        "../{}/secret.txt",
        outside.file_name().unwrap().to_string_lossy()
    );
    for path in [lexical.as_str(), "link/secret.txt"] {
        let read = remote
            .read_file(path)
            .await
            .expect_err("the delegation must refuse an escaping read");
        assert_eq!(
            failure_mode(&read),
            Some(FailureMode::Refused),
            "an escape refusal must not look like a broken link: {read}"
        );
        assert!(
            remote.write_file(path, "owned").await.is_err(),
            "the delegation must refuse an escaping write of {path:?}"
        );
    }

    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "outside",
        "a refused write through the delegation still reached the outside file"
    );
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        1,
        "a refused write through the delegation created a file outside the workspace"
    );

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

/// The classification survives a hop. A delegate wrapping a substrate that does not serve an
/// operation must relay *unserved*, not convert it into a refusal — otherwise a chain of delegations
/// turns "nobody implements this" into "the guard said no", and an operator retries forever.
#[tokio::test]
async fn an_unserved_operation_stays_unserved_across_a_delegation_hop() {
    let inner = Arc::new(RemoteSystem::new(Arc::new(ServesNothing)));
    let outer = RemoteSystem::loopback(inner);

    let error = outer
        .list_dir(".")
        .await
        .expect_err("an unserved listing must fail closed through two hops");
    assert_eq!(
        failure_mode(&error),
        Some(FailureMode::Unserved),
        "a hop turned an unserved operation into something else: {error}"
    );
}

/// `GuardedEnv::env` returns `Option<String>`, so it has nowhere to put the distinction — both
/// failure modes fail the credential closed as `None`. `env_checked` is the inherent escape hatch
/// that keeps them apart for an operator, and this test pins both halves of that trade-off.
#[tokio::test]
async fn the_env_read_fails_closed_and_env_checked_keeps_the_modes_apart() {
    let refusing = remote(Refusing("no env for you"));
    let unreached = remote(Unreached);

    assert_eq!(refusing.env("SECRET"), None);
    assert_eq!(unreached.env("SECRET"), None);

    assert_eq!(
        failure_mode(&refusing.env_checked("SECRET").expect_err("refused")),
        Some(FailureMode::Refused)
    );
    assert_eq!(
        failure_mode(&unreached.env_checked("SECRET").expect_err("unreachable")),
        Some(FailureMode::Unreachable)
    );
}

/// A delegate is a `Loopback` over anything that serves the four port families — including another
/// `RemoteSystem` — so the type-level claim "the port is delegable" is exercised, not just asserted
/// in prose.
#[tokio::test]
async fn a_remote_system_is_itself_a_delegable_substrate() {
    let root = std::env::temp_dir().join(format!("c399-chain-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let native = Arc::new(System::new(Workspace::new(&root).unwrap()));

    let one_hop = Arc::new(RemoteSystem::loopback(native));
    let two_hops = RemoteSystem::new(Arc::new(Loopback::new(one_hop)));

    two_hops
        .write_file("chained.txt", "through two")
        .await
        .unwrap();
    assert_eq!(
        two_hops.read_file("chained.txt").await.unwrap(),
        "through two"
    );

    std::fs::remove_dir_all(&root).ok();
}
