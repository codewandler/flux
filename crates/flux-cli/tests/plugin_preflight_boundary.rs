//! C-404 — the **host-dispatched** plugin response is an ingest surface like every other.
//!
//! C-312 put the credential boundary where `call_with_host` returns, and excused exactly one class:
//! a host-dispatched `internal: true` op, on the grounds that it is never advertised to the model
//! and its result "goes to host code, not to a log or a transcript". Half of that is true. The other
//! half is not: the one site the carve-out was load-bearing for is the `plugin.validate` preflight
//! in `flux plugin call --dry-run`, whose plugin-authored `problems`/`warnings` are lifted into the
//! printed verdict and whose error frame is printed to stderr — the same operator-scrollback and
//! shell-history surface C-312 cites as the reason `flux plugin call` needs the check at all.
//!
//! **Why the fixture speaks the raw protocol.** `host-kit`'s builder auto-injects `plugin.validate`
//! through `internal_op`, which takes `..OperationSpec::default()` and therefore
//! `PlatformSourcing::None` — so on every plugin in this repository the boundary would be a no-op
//! at that site even if it ran, and wiring it there would be untestable through them. The gap is
//! narrower than the carve-out and precisely this: a plugin that speaks the NDJSON protocol without
//! `host-kit` may declare its own `plugin.validate` with `platform` set. `platform_plugin`'s
//! `*-validate` modes are that plugin. Nothing in the tree ships one; nothing prevented one.
//!
//! These drive the real binary end to end — `flux plugin call <name> <op> --dry-run` over a
//! descriptor in a pinned `$HOME` — rather than the `refuse_platform_response` helper directly.
//! That is deliberate: the helper already has unit tests in `plugin_cmd.rs`, and what this story is
//! about is a *wiring* claim made in prose. A test that called the helper would re-assert the
//! helper.
//!
//! The `local-validate` control is what keeps this from being a blanket filter: the same
//! host-dispatched internal op, the same leak, and no `platform` declaration — i.e. exactly what
//! `host-kit` produces — must still print, because the boundary is a declaration and not a content
//! filter on plugin output.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The vendor credential `platform_plugin` holds, kept in step with the fixture by
/// [`the_fixture_and_this_test_agree_on_the_credential`].
///
/// Joined at compile time (C-325) so a forge's secret scanner finds no credential in this file.
const VENDOR_TOKEN: &str = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");

/// A temp dir that removes itself on drop, so a failing assertion cannot leak it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-c404-{tag}-{}-{:?}",
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

/// The `platform_plugin` fixture binary, built on demand when the current run has not produced it.
///
/// `CARGO_BIN_EXE_*` is set only for the package that declares the bin, and `platform_plugin`
/// belongs to `flux-plugin` — so derive the path from this test binary instead: an integration test
/// runs out of `<target>/<profile>/deps/`, and a workspace bin lands in `<target>/<profile>/`.
/// Deriving it rather than hard-coding `target/debug` is what makes this follow a custom
/// `CARGO_TARGET_DIR`; a hard-coded path would silently drive *another* checkout's stale fixture.
///
/// Building on demand rather than skipping: `cargo test --workspace` compiles every member's
/// binaries first, so the fixture is simply there — but `cargo test -p flux-cli` does not build
/// another package's bins, and a security test that quietly no-ops when its fixture is missing is
/// worse than no test at all. Same shape, and the same reasoning, as
/// `flux-capabilities`' `credential_boundary_broker.rs`.
fn platform_plugin_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().expect("this test binary has a path");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("a test binary lives at <target>/<profile>/deps/<name>");
        let bin = profile_dir.join(format!("platform_plugin{}", std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            build_platform_plugin(profile_dir, &bin);
        }
        bin
    })
    .clone()
}

/// Build the `platform_plugin` fixture into `profile_dir`, or panic saying why it could not be.
fn build_platform_plugin(profile_dir: &Path, bin: &Path) {
    let target_dir = profile_dir
        .parent()
        .expect("a profile directory lives under the target directory");
    let profile = match profile_dir.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("debug") => "dev",
        Some(name) => name,
        None => panic!(
            "the profile directory {} has no name to recover a cargo profile from",
            profile_dir.display()
        ),
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args([
            "build",
            "-p",
            "codewandler-flux-plugin",
            "--bin",
            "platform_plugin",
            "--profile",
            profile,
        ])
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "could not run `{cargo} build -p codewandler-flux-plugin --bin platform_plugin` \
                 to produce the fixture this credential-boundary test drives: {err}"
            )
        });
    assert!(
        output.status.success(),
        "building the `platform_plugin` fixture failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        bin.exists(),
        "the fixture build reported success but left no binary at {}",
        bin.display()
    );
}

/// The fixture installed as a local plugin descriptor under a pinned `$HOME`, plus the mode file
/// that decides what it answers.
struct Installed {
    dir: TempDir,
}

impl Installed {
    /// Write `~/.flux/plugins/platform.toml` pointing at the fixture in `mode`.
    ///
    /// A descriptor with no `sha256` is "unverified/local", which `spawn_verified` admits — the
    /// same posture a `flux plugin add`-ed development plugin has.
    fn new(tag: &str, mode: &str) -> Self {
        let dir = TempDir::new(tag);
        let mode_file = dir.path().join("mode");
        std::fs::write(&mode_file, mode).unwrap();

        let plugins = dir.path().join("home").join(".flux").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let descriptor = format!(
            "program = {}\nargs = [{}]\n",
            toml_string(&platform_plugin_bin().to_string_lossy()),
            toml_string(&mode_file.to_string_lossy()),
        );
        std::fs::write(plugins.join("platform.toml"), descriptor).unwrap();

        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        Self { dir }
    }

    /// `flux plugin call platform echo --dry-run` — the D-88 preflight path — and everything the
    /// operator's terminal receives from it.
    fn dry_run(&self) -> Verdict {
        let out = Command::new(env!("CARGO_BIN_EXE_flux"))
            .args(["plugin", "call", "platform", "echo", "--dry-run"])
            .current_dir(self.dir.path().join("work"))
            .env("HOME", self.dir.path().join("home"))
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .expect("spawn flux");
        Verdict {
            success: out.status.success(),
            printed: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
}

/// Everything one `flux plugin call --dry-run` puts on the operator's terminal, plus whether it
/// exited 0. Both halves matter: a refusal that still exits 0 reads as a green preflight.
struct Verdict {
    success: bool,
    printed: String,
}

/// Quote a string as a TOML basic string. Fixture paths are temp-dir paths, but a `\` on Windows or
/// a `"` in a temp dir name would otherwise produce a descriptor that does not parse.
fn toml_string(raw: &str) -> String {
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The fixture's credential and this file's copy of it must be the same bytes, or every "the
/// credential is absent" assertion below is vacuous.
///
/// `local-validate` is the mode that proves it: the same leak with **no** `platform` declaration, so
/// nothing filters it and whatever the fixture is willing to emit is printed verbatim.
#[test]
fn the_fixture_and_this_test_agree_on_the_credential() {
    let installed = Installed::new("agree", "local-validate");
    let verdict = installed.dry_run();
    assert!(
        verdict.printed.contains(VENDOR_TOKEN),
        "the fixture did not emit the credential this test asserts about:\n{}",
        verdict.printed
    );
}

// ---------------------------------------------------------------------------
// The failing-first test
// ---------------------------------------------------------------------------

/// **The story's failing-first test.** A host-dispatched `internal: true`, platform-sourced
/// `plugin.validate` whose verdict carries credential material is REFUSED instead of printed.
///
/// Three assertions, and the second is what makes the first mean something: the credential is
/// absent from everything the terminal received, the run *fails* (a printed-but-redacted verdict
/// would satisfy "the token is not in the output" while reporting a preflight that never happened),
/// and the refusal says a credential is why — so the failure is distinguishable from an unrelated
/// one. The refusal itself never quotes the value, which the first assertion also covers.
#[test]
fn a_platform_sourced_preflight_carrying_a_vendor_credential_is_refused() {
    let installed = Installed::new("refused", "leak-validate");
    let verdict = installed.dry_run();
    assert!(
        !verdict.printed.contains(VENDOR_TOKEN),
        "the host-dispatched preflight put a vendor credential on the operator's terminal:\n{}",
        verdict.printed
    );
    assert!(
        !verdict.success,
        "a credential-bearing preflight verdict was accepted, not refused:\n{}",
        verdict.printed
    );
    assert!(
        verdict.printed.contains("credential"),
        "the run failed without saying the boundary refused it, so the failure is not \
         distinguishable from an unrelated one:\n{}",
        verdict.printed
    );
}

/// The failure path of the same seam — and the one place this story deliberately does **not**
/// escalate to a refusal.
///
/// A preflight that errors has always been non-fatal here: plugins predating D-88 do not serve
/// `plugin.validate` at all, so `--dry-run` falls back to the schema-only verdict by design. The
/// hole was never that fallback, it was that the plugin-authored error string was printed raw. So
/// the message is scrubbed and the fallback is kept: the credential never reaches the terminal, and
/// a dry run does not start failing for a reason it never failed for before.
#[test]
fn a_platform_sourced_preflight_error_frame_is_scrubbed_rather_than_printed() {
    let installed = Installed::new("scrubbed", "leak-error-validate");
    let verdict = installed.dry_run();
    assert!(
        !verdict.printed.contains(VENDOR_TOKEN),
        "the preflight's error frame put a vendor credential on the operator's terminal:\n{}",
        verdict.printed
    );
    assert!(
        verdict.printed.contains("credential"),
        "the discarded error message did not say a credential is why it was discarded:\n{}",
        verdict.printed
    );
    assert!(
        verdict.success,
        "an unavailable preflight must still fall back to the schema-only verdict:\n{}",
        verdict.printed
    );
}

/// The positive control: an honest deployment's preflight still merges into the dry-run verdict. A
/// boundary that refused every preflight would pass the test above and break `--dry-run` for every
/// platform-sourced plugin.
#[test]
fn an_honest_platform_sourced_preflight_still_merges_into_the_verdict() {
    let installed = Installed::new("flows", "honest-validate");
    let verdict = installed.dry_run();
    assert!(
        verdict.success,
        "an honest preflight must not fail the dry run:\n{}",
        verdict.printed
    );
    assert!(
        verdict
            .printed
            .contains("the deployment has not seen this vendor in 30 days"),
        "the honest preflight's warning did not reach the verdict:\n{}",
        verdict.printed
    );
}

/// The other direction, and the reason the boundary is a declaration rather than a filter: the same
/// host-dispatched internal op with **no** `platform` declaration — what `host-kit`'s `internal_op`
/// produces for every plugin in this repository — keeps its existing posture.
///
/// This is the case that matters for what ships today. A wiring that had quietly become a content
/// filter on all preflight output would break `--dry-run` for every host-kit plugin and would still
/// pass the refusal test above.
#[test]
fn a_preflight_that_is_not_platform_sourced_is_not_refused() {
    let installed = Installed::new("scoped", "local-validate");
    let verdict = installed.dry_run();
    assert!(
        verdict.printed.contains(VENDOR_TOKEN),
        "an undeclared preflight is not subject to the boundary:\n{}",
        verdict.printed
    );
}
