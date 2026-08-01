//! C-266: the **with-a-backend** half of C-262's fail-closed switch. Its sibling
//! `sandbox_posture.rs` owns the without-a-backend half and is hermetic about it — every spawn there
//! forces `FLUX_BWRAP_BIN`/`FLUX_SANDBOX_EXEC_BIN` at nonexistent paths, so it proves the same thing
//! on a host that has `bwrap` and on one that does not. Nothing, therefore, was proving the other
//! side: that flux still *works* when a backend exists, and that the confinement it then claims is
//! real. Until this file, that path ran on no CI runner at all.
//!
//! Every test here is gated on `FLUX_TEST_SANDBOX_BACKEND=1` — the caller's promise that a usable
//! backend is installed — for the same reason the Postgres suites are gated on `TEST_POSTGRES_URL`:
//! the posture a test needs must be *declared*, never inferred from whatever the host happens to have.
//! Unset, they skip. Set, they are unforgiving: [`a_promised_backend_is_real_and_functional`] fails
//! the run when the promise is empty, because a lane that installs `bubblewrap` onto a kernel that
//! refuses unprivileged user namespaces silently degrades into a second copy of the no-backend lane —
//! green, and proving nothing. That specific false assurance is what this story exists to remove.
//!
//! C-276 then landed here for exactly that reason: the guarded spawn forwarded the confinement
//! *marker* and not the posture, and only a lane with a real backend can see that — without one the
//! marker is never stamped either, so the asymmetry has nothing to stand against.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A temp dir that removes itself on drop — so a failing assertion (a panic) can't leak it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
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

/// Whether this run was promised a usable OS sandbox backend. Printed rather than silent, so a
/// skipped posture is visible in `cargo test -- --nocapture` and in the CI log.
fn backend_promised(test: &str) -> bool {
    if std::env::var("FLUX_TEST_SANDBOX_BACKEND").as_deref() == Ok("1") {
        return true;
    }
    println!("skipping {test}: set FLUX_TEST_SANDBOX_BACKEND=1 on a host with a working backend");
    false
}

/// The captured streams of one `flux` invocation.
struct Run {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Run `flux <args>` in an isolated HOME + CWD, with backend discovery left ALONE — this suite is
/// about the real backend, so it must not force the discovery vars the way `sandbox_posture.rs` does.
fn run_flux(tag: &str, extra: &[(&str, &str)], args: &[&str]) -> Run {
    let tmp = TempDir::new(tag);
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // flux-allow-ambient-sandbox: this suite's whole subject is the posture the host resolves, and
    // it runs only when FLUX_TEST_SANDBOX_BACKEND=1 promised a real backend — the one place where
    // reading the host is the point rather than the bug. The per-test posture arrives in `extra`.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_flux"));
    cmd.args(args)
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env_remove("FLUX_SANDBOXED")
        .stdin(Stdio::null());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn flux");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        success: out.status.success(),
    }
}

/// The successful `bash` tool_result line of a mock run — what the child process reported back —
/// or a panic carrying both streams. Every test here keys on a real child having actually run,
/// because "the command succeeded" is also what a silently-degraded lane reports.
fn tool_result_line(run: &Run) -> String {
    run.stdout
        .lines()
        .find(|line| line.contains(r#""name":"bash""#) && line.contains(r#""is_error":false"#))
        .unwrap_or_else(|| {
            panic!(
                "expected a bash tool_result proving a child process actually ran.\nstdout:\n{}\nstderr:\n{}",
                run.stdout, run.stderr
            )
        })
        .to_string()
}

/// The lane's premise, asserted instead of assumed. `doctor` deliberately probes with `on` regardless
/// of the configured posture, so its `sandbox backend` check reports what is ACTUALLY available:
/// `PASS` only when discovery found the wrapper *and* the functional preflight probe succeeded.
/// `WARN` is the shape of the failure this guards against — bwrap installed, namespaces refused.
#[test]
fn a_promised_backend_is_real_and_functional() {
    if !backend_promised("a_promised_backend_is_real_and_functional") {
        return;
    }
    let run = run_flux("c266-doctor", &[], &["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "`doctor --json` stdout must be one JSON document ({e}).\nstdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        )
    });
    let check = report["checks"]
        .as_array()
        .expect("doctor checks array")
        .iter()
        .find(|check| check["name"] == "sandbox backend")
        .expect("doctor reports a `sandbox backend` check")
        .clone();
    assert_eq!(
        check["status"], "PASS",
        "FLUX_TEST_SANDBOX_BACKEND=1 promised a usable backend and this host has none, so every \
         confinement assertion in this lane would pass vacuously. Install `bubblewrap` (Linux) or \
         the Xcode command line tools (macOS), and on Ubuntu 23.10+ check that unprivileged user \
         namespaces are not refused — doctor says: {check}"
    );
}

/// The other side of the switch, behaviorally: an auto-approved turn under `require` starts (it does
/// not fail closed, because a backend exists) and its child process really is inside the sandbox.
///
/// The confinement proof is the child's own pid. The bubblewrap argv includes `--unshare-pid` with a
/// fresh `/proc`, so a confined child sees a single-digit pid; an unconfined one sees a real OS pid.
/// Keying on that rather than on "the command succeeded" is deliberate: success alone is exactly what
/// a silently-degraded lane also reports.
#[test]
fn an_auto_approved_turn_runs_its_children_inside_the_sandbox() {
    if !backend_promised("an_auto_approved_turn_runs_its_children_inside_the_sandbox") {
        return;
    }
    let run = run_flux(
        "c266-confined-spawn",
        &[
            ("FLUX_SANDBOX", "require"),
            // `bash` is opt-in (off-by-default `shell` group), so it must be enabled explicitly.
            ("FLUX_ENABLE_BASH", "1"),
            ("FLUX_MOCK_BASH", "echo pid=$$"),
        ],
        &[
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "run a command",
        ],
    );

    assert!(
        run.success,
        "`require` + a real backend must START, not fail closed.\nstderr:\n{}",
        run.stderr
    );
    let pid_line = tool_result_line(&run);
    let pid: u32 = pid_line
        .split("pid=")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no child pid in the tool_result: {pid_line}"));
    assert!(
        pid < 100,
        "the child reported pid {pid}: it ran OUTSIDE the sandbox's pid namespace, so an \
         auto-approved turn was not actually confined even though a backend was available.\n\
         stdout:\n{}",
        run.stdout
    );
    // `require` is satisfied, so there is nothing unconfined to disclose.
    assert!(
        !run.stderr.contains("UNCONFINED"),
        "a confined run must not disclose an unconfined posture.\nstderr:\n{}",
        run.stderr
    );
}

/// C-276: the guarded spawn's env allow-list forwarded the confinement **marker** — `FLUX_SANDBOXED`,
/// whose entire job is to assert *"you are already confined"* — and none of the variables that decide
/// whether confinement actually happens. A child therefore read its posture out of an environment
/// containing no posture, resolved `off` (sandboxing is opt-in), and declined to confine its own
/// descendants while the operator had demanded `require`.
///
/// This proof has to live in *this* lane rather than in `sandbox_posture.rs`. Without a backend the
/// parent's sandbox is never active, so the marker is never stamped on the child either, and the
/// asymmetry is invisible — `off` and `require`-but-unavailable are indistinguishable from inside the
/// child. With a backend the parent genuinely wraps the spawn, genuinely stamps `FLUX_SANDBOXED=1`,
/// and a missing posture stands alone as the defect. That is exactly how this hid.
///
/// The child here is the sandboxed `bash` the agent ran, reporting its own environment. That is the
/// same `build_command` a child `flux` goes through, and the variables it echoes are verbatim the ones
/// `SandboxSettings::from_env` and backend discovery read back on the other side.
#[test]
fn a_confined_child_inherits_the_posture_and_not_only_the_marker() {
    if !backend_promised("a_confined_child_inherits_the_posture_and_not_only_the_marker") {
        return;
    }
    let run = run_flux(
        "c276-posture-reaches-child",
        &[
            ("FLUX_SANDBOX", "require"),
            ("FLUX_ENABLE_BASH", "1"),
            (
                "FLUX_MOCK_BASH",
                // Exactly one wrapper variable is ever set (bwrap on Linux, sandbox-exec on
                // macOS), so concatenating them reads whichever this host resolved.
                "echo posture=[$FLUX_SANDBOX] marker=[$FLUX_SANDBOXED] \
                 net=[$FLUX_SANDBOX_NET] wrapper=[$FLUX_BWRAP_BIN$FLUX_SANDBOX_EXEC_BIN]",
            ),
        ],
        &[
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "run a command",
        ],
    );
    assert!(
        run.success,
        "`require` + a real backend must START, not fail closed.\nstderr:\n{}",
        run.stderr
    );
    let reported = tool_result_line(&run);

    // The marker travelled before this story and must keep travelling: nested-run detection depends
    // on it surviving every hop. Asserted here so the test states the *asymmetry*, not half of it.
    assert!(
        reported.contains("marker=[1]"),
        "the confinement marker must reach a genuinely wrapped child.\ntool_result: {reported}"
    );
    assert!(
        reported.contains("posture=[require]"),
        "the child was told it is confined (`marker=[1]`) but not with what: it reads no \
         `FLUX_SANDBOX`, resolves `off`, and leaves its own descendants unconfined while the \
         operator demanded `require`.\ntool_result: {reported}"
    );
    // `--yes` is an auto-approved surface, so C-262 narrowed the sandbox network to closed. That
    // decision has to reach the child too, or a descendant re-opens what the parent shut.
    assert!(
        reported.contains("net=[0]"),
        "the resolved network posture did not reach the child.\ntool_result: {reported}"
    );
    // The wrapper path is the sharpest evidence that these values come from the RESOLVED sandbox
    // rather than from the ambient environment: nothing set `FLUX_BWRAP_BIN` on this run, so an
    // env-echoing forwarder has nothing to echo. What the child sees is the absolute binary
    // discovery found and the preflight probe verified — the one actually wrapping it.
    let wrapper = reported
        .split("wrapper=[")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or_else(|| panic!("no wrapper field in the tool_result: {reported}"));
    assert!(
        wrapper.starts_with('/'),
        "the child inherited no resolved wrapper path (got {wrapper:?}), so it would rediscover \
         one through `PATH` instead of using the binary this process verified.\n\
         tool_result: {reported}"
    );
}

/// The behavioural half: a real child `flux`, spawned through the guarded path from a parent running
/// under `require`, must *resolve* `require` — not `off`.
///
/// The observable is the child's own startup audit line. A flux that resolves a non-`off` posture and
/// finds itself already confined by an outer flux sandbox prints the C-217 OUTER-CONFINEMENT trust
/// disclosure; a flux that resolved `off` short-circuits before any of that and is silent, because
/// `Sandbox::resolve` checks `Off` first and never re-reads a marker as confinement. So the line's
/// presence is precisely the difference between the two resolutions, observed from outside.
#[test]
fn a_child_flux_resolves_the_parents_require_posture() {
    if !backend_promised("a_child_flux_resolves_the_parents_require_posture") {
        return;
    }
    // Quoted so a path with spaces still spawns one process; `2>&1 >/dev/null` keeps the child's
    // stderr (the only thing under test) and discards its stdout.
    let nested = format!("'{}' changelog 2>&1 >/dev/null", env!("CARGO_BIN_EXE_flux"));
    let run = run_flux(
        "c276-child-flux-posture",
        &[
            ("FLUX_SANDBOX", "require"),
            ("FLUX_ENABLE_BASH", "1"),
            ("FLUX_MOCK_BASH", nested.as_str()),
        ],
        &[
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "run a command",
        ],
    );
    assert!(
        run.success,
        "`require` + a real backend must START, not fail closed.\nstderr:\n{}",
        run.stderr
    );
    let reported = tool_result_line(&run);
    assert!(
        reported.contains("OUTER-CONFINEMENT"),
        "the child flux said nothing about its confinement, which is what a resolved `off` looks \
         like: it inherited the marker but no posture, so `Sandbox::resolve` returned early on \
         `Off` and the operator's `require` died at the process boundary.\ntool_result: {reported}"
    );
}

/// **C-410.** The confined case owed a disclosure and never paid one.
///
/// Every other line `apply_sandbox_env` prints fires when confinement is *absent* or was opted out
/// of: the `on`-without-a-backend warning, the `--no-sandbox` BYPASSED warning, the
/// OUTER-CONFINEMENT trust note. A run that is genuinely confined said nothing — so the first
/// symptom of the profile is a child process failing with `curl: (6) Could not resolve host`, or a
/// refused write under `$HOME`, with nothing anywhere naming the sandbox as the cause. Choosing the
/// looser posture was loud and choosing the tighter one was silent.
///
/// This lane is the only one that can see it: the line is emitted where `ensure_available()`
/// succeeded and the sandbox is active, which on a host with no backend is unreachable. Both
/// narrowings are asserted because both bite in practice — the network is closed, and writes are
/// limited to the workspace, `$TMPDIR` and the toolchain caches, which is what breaks a plugin that
/// keeps its state in `~/.config/<vendor>`.
#[test]
fn a_confined_unattended_surface_discloses_what_it_narrowed() {
    if !backend_promised("a_confined_unattended_surface_discloses_what_it_narrowed") {
        return;
    }
    // `plugin call` against a plugin that is not installed: the startup posture preflight runs to
    // completion (which is what emits the line) and the command then fails on the missing plugin.
    // That keeps this offline, with no provider and no fixture binary.
    let run = run_flux(
        "c410-confined-note",
        &[],
        &["plugin", "call", "c410-never-installed", "echo"],
    );
    assert!(
        run.stderr.contains("CONFINED") && run.stderr.contains("sandbox:"),
        "a confined unattended surface must say so, in the same breath as the posture it \
         resolved.\nstderr:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("network CLOSED"),
        "the disclosure must name the network narrowing — it is the one that produces a DNS \
         failure with no other explanation.\nstderr:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("$TMPDIR"),
        "the disclosure must name the filesystem narrowing too: a plugin that writes outside the \
         workspace is refused, and nothing else tells the operator that.\nstderr:\n{}",
        run.stderr
    );
    // Routing, same contract as the C-217 disclosure: diagnostics never touch stdout.
    assert!(
        !run.stdout.contains("CONFINED"),
        "the disclosure must not reach stdout.\nstdout:\n{}",
        run.stdout
    );
    // And it must not fire for a surface that was never pinned, or every interactive run would grow
    // a line about a sandbox it is not using.
    let unpinned = run_flux("c410-confined-note-off", &[], &["plugin", "ls"]);
    assert!(
        !unpinned.stderr.contains("CONFINED"),
        "an unpinned surface resolved `off` and has nothing to disclose.\nstderr:\n{}",
        unpinned.stderr
    );
}
