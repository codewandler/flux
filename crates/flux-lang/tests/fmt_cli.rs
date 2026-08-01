//! `fluxlang fmt` — the canonical formatter as a CLI (L-103).
//!
//! These drive the **real binary**, not a library helper: the story's contract is a command an
//! author (or CI) can run over a `.flux` file, so the exit code and the bytes on stdout are the
//! thing under test. Built only with `--features cli`, which is where the `fluxlang` binary lives;
//! `scripts/check-feature-gated-tests.sh` owns this leg of the gate (`cargo test --workspace`
//! cannot reach it — C-308).
#![cfg(feature = "cli")]

use std::io::Write;
use std::process::{Command, Stdio};

/// Every legacy spelling the parser accepts, in one flow: sigiled locals, a braced single-object
/// call, a `do` call, a bare-ms duration, a space-keyword control header, a body-line `until`, and
/// the legacy `await … when`. Comments sit at module, statement, block and trailing positions.
const MIXED_DIALECT: &str = "\
# what this flow is for
flow triage(ticket: Ticket)
  # find the work
  $hits = grep({ pattern: \"TODO\", glob: \"*.rs\" })  # author order, not alphabetical
  do notify $hits
  timeout 30000 -> $slow
    # inside a block
    slow_scan()
  retry 3 backoff exponential delay 500 -> $out
    flaky()
  repeat 10 -> $c
    until $done
    step()
  await $sig = \"webhook\" when $ready
  return $out
";

/// The canonical dialect: one spelling of each construct, every comment still in place.
const CANONICAL: &str = "\
# what this flow is for
flow triage(ticket: Ticket)
  # find the work
  hits = grep(pattern: \"TODO\", glob: \"*.rs\")  # author order, not alphabetical
  notify(hits)
  timeout 30s -> slow
    # inside a block
    slow_scan()
  retry 3, backoff: exponential, delay: 500ms -> out
    flaky()
  repeat 10, until: done -> c
    step()
  await sig = \"webhook\", when: ready
  return out
";

/// Run `fluxlang <args…>` with `stdin`, returning `(exit code, stdout, stderr)`.
fn fluxlang(args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fluxlang"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fluxlang");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for fluxlang");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn fmt_canonicalizes_every_legacy_spelling() {
    let (code, stdout, stderr) = fluxlang(&["fmt"], MIXED_DIALECT);
    assert_eq!(code, 0, "fmt must succeed; stderr: {stderr}");
    assert_eq!(stdout, CANONICAL, "stderr: {stderr}");

    // The rewrite is a *spelling* change only: both dialects compile to the same AST.
    let before = flux_lang::program::Module::parse_str(MIXED_DIALECT).expect("legacy parses");
    let after = flux_lang::program::Module::parse_str(&stdout).expect("canonical parses");
    assert_eq!(before, after, "fmt changed the meaning of the flow");
}

#[test]
fn fmt_is_idempotent() {
    let (_, once, _) = fluxlang(&["fmt"], MIXED_DIALECT);
    let (code, twice, stderr) = fluxlang(&["fmt"], &once);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(twice, once, "fmt(fmt(x)) != fmt(x)");
}

#[test]
fn fmt_rewrites_files_in_place_and_leaves_canonical_ones_untouched() {
    let dir = std::env::temp_dir().join(format!("fluxlang-fmt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let legacy = dir.join("legacy.flux");
    let canonical = dir.join("canonical.flux");
    std::fs::write(&legacy, MIXED_DIALECT).expect("write legacy fixture");
    std::fs::write(&canonical, CANONICAL).expect("write canonical fixture");

    let (code, stdout, stderr) = fluxlang(
        &["fmt", legacy.to_str().unwrap(), canonical.to_str().unwrap()],
        "",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "", "in-place mode writes files, not stdout");
    assert_eq!(
        std::fs::read_to_string(&legacy).expect("read back"),
        CANONICAL,
        "the legacy file was rewritten in place"
    );
    assert_eq!(
        std::fs::read_to_string(&canonical).expect("read back"),
        CANONICAL,
        "the already-canonical file is byte-identical"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_check_exits_zero_on_canonical_input_and_non_zero_otherwise() {
    let (code, stdout, _) = fluxlang(&["fmt", "--check"], CANONICAL);
    assert_eq!(code, 0, "canonical input is clean");
    assert_eq!(stdout, "", "--check never writes the source out");

    let (code, _, stderr) = fluxlang(&["fmt", "--check"], MIXED_DIALECT);
    assert_ne!(code, 0, "non-canonical input must fail the check");
    assert!(
        stderr.contains("not canonical"),
        "--check reports which input: {stderr}"
    );
    // The diff summary names what would change, so CI output is actionable on its own.
    assert!(
        stderr.contains("-  $hits = grep({ pattern:") && stderr.contains("+  hits = grep(pattern:"),
        "--check prints a diff summary: {stderr}"
    );
}

#[test]
fn fmt_reports_a_bad_file_without_abandoning_the_rest_of_the_batch() {
    // `fmt` is meant to be pointed at a whole tree. Stopping at the first bad file hides every file
    // behind it, so the operator fixes one thing, re-runs, and meets the next one.
    let dir = std::env::temp_dir().join(format!("fluxlang-fmt-batch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let broken = dir.join("a-broken.flux");
    let legacy = dir.join("b-legacy.flux");
    std::fs::write(&broken, "flow x\n  confirm \"y\n").expect("write broken fixture");
    std::fs::write(&legacy, MIXED_DIALECT).expect("write legacy fixture");

    let (code, _, stderr) = fluxlang(
        &["fmt", broken.to_str().unwrap(), legacy.to_str().unwrap()],
        "",
    );
    assert_ne!(code, 0, "a file that cannot be formatted fails the run");
    assert!(stderr.contains("parse error"), "it says why: {stderr}");
    assert_eq!(
        std::fs::read_to_string(&legacy).expect("read back"),
        CANONICAL,
        "the file after the broken one was still formatted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_refuses_source_it_cannot_parse() {
    // Lexically malformed on purpose (an unterminated string): a fixture that is merely "a spelling
    // the parser rejects today" is one the next syntax story silently makes valid.
    let (code, _, stderr) = fluxlang(&["fmt"], "flow x\n  confirm \"y\n");
    assert_ne!(code, 0, "unparseable input is an error, not a silent pass");
    assert!(stderr.contains("parse error"), "got: {stderr}");
}
