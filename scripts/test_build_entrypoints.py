#!/usr/bin/env python3
"""Fail when a repository target-touching entry point bypasses build ownership."""

from __future__ import annotations

import os
from pathlib import Path
import re
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
TARGET_VERBS = "build|test|clippy|check|install|clean|run|bench|doc|miri|publish|package"
CARGO_SHELL = re.compile(
    rf"(?<![\w-])(?:command\s+)?(?:cargo(?:\.exe)?|\$\{{?[A-Za-z_]*CARGO\}}?|"
    rf"\$env:[A-Za-z_]*CARGO|%[A-Za-z_]*CARGO%)(?:\s+\+[^\s]+)?\s+(?:{TARGET_VERBS})\b",
    re.IGNORECASE,
)
CARGO_ARGV = re.compile(
    rf"[\"'](?:cargo(?:\.exe)?|[^\"']*cargo(?:\.exe)?)[\"']\s*,\s*"
    rf"(?:[\"']\+[^\"']+[\"']\s*,\s*)?[\"'](?:{TARGET_VERBS})[\"']",
    re.IGNORECASE,
)
CHECKED_FRONTENDS = (
    re.compile(r"(?<![\w-])(?:command\s+)?dist\s+build\b"),
    re.compile(rf"(?<![\w-])(?:command\s+)?cross\s+(?:{TARGET_VERBS})\b"),
    re.compile(r"(?<![\w-])(?:command\s+)?cargo-nextest\s+(?:run|archive)\b"),
    re.compile(r"(?<![\w-])(?:command\s+)?cargo(?:\.exe)?\s+(?:nextest|llvm-cov|fuzz|zigbuild)\b"),
)
RUBY_STRING_LITERAL = r'''(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')'''
RUBY_REGEX_LITERAL = r"/(?:\\.|[^/\\])*/[a-z]*"
RUBY_ASSIGNMENT_TARGET = rf"[A-Za-z_]\w*(?:\[\s*{RUBY_STRING_LITERAL}\s*\])?"
RUBY_NON_EXECUTABLE_LITERAL_LINE = re.compile(
    rf"^\s*(?:(?:(?:next|return|break)\s+)?(?:if|unless)\s+"
    rf"[A-Za-z_]\w*\.include\?\(\s*{RUBY_STRING_LITERAL}\s*\)"
    rf"|{RUBY_ASSIGNMENT_TARGET}\s*=\s*[A-Za-z_]\w*\.sub\(\s*"
    rf"(?:{RUBY_REGEX_LITERAL}|{RUBY_STRING_LITERAL})\s*,\s*"
    rf"{RUBY_STRING_LITERAL}\s*\))\s*(?:#.*)?$"
)
OWNED_SPELLINGS = (
    "owned_cargo",
    "owned-cargo",
    "with_build_ownership",
    "build_ownership.py",
)
SCRIPT_SUFFIXES = {".sh", ".bash", ".zsh", ".fish", ".py", ".ps1", ".cmd", ".bat"}
FIXED_TARGET_WORKFLOWS = (
    ".github/workflows/adversarial-assurance.yml",
    ".github/workflows/ci.yml",
    ".github/workflows/release-flow.yml",
    ".github/workflows/release-plugins.yml",
    ".github/workflows/release.yml",
)
RESOLVED_CONSUMER_SCRIPTS = (
    "bench/run-synthetic-loop.sh",
    "bench/run-tbench-compare.sh",
    "bench/run-tbench-loop.sh",
    "bench/run-ttff.sh",
    "scripts/build-portable-wasm.sh",
    "scripts/check-plugin-compat.sh",
    "scripts/smoke-live.sh",
    "scripts/smoke-plugins.sh",
)
PREBUILT_CONSUMER_SCRIPTS = (
    "bench/cache-ab.sh",
    "scripts/eval-adaptive-latency.sh",
    "scripts/eval-adaptive-support.sh",
)


def entrypoint_files(root: Path) -> list[Path]:
    selected: set[Path] = set()
    taskfile = root / "Taskfile.yaml"
    if taskfile.is_file():
        selected.add(taskfile)
    workflows = root / ".github" / "workflows"
    if workflows.is_dir():
        selected.update(path for path in workflows.rglob("*") if path.suffix in {".yml", ".yaml"})
    actions = root / ".github" / "actions"
    if actions.is_dir():
        selected.update(path for path in actions.rglob("action.y*ml") if path.is_file())
    for path in root.rglob("*"):
        if not path.is_file() or any(part in {".git", "node_modules", "target"} for part in path.parts):
            continue
        if path.suffix in SCRIPT_SUFFIXES or (not path.suffix and os.access(path, os.X_OK)):
            selected.add(path)
    selected.discard(root / "scripts" / "test_build_entrypoints.py")
    return sorted(selected)


def mask_non_executable_literals(line: str) -> str:
    """Hide standalone Ruby predicate/mutation fixture lines, but never execution sinks."""

    if RUBY_NON_EXECUTABLE_LITERAL_LINE.fullmatch(line):
        return " " * len(line)
    return line


def target_match(line: str) -> re.Match[str] | None:
    searchable = mask_non_executable_literals(line)
    matches = [
        pattern.search(searchable) for pattern in (CARGO_SHELL, CARGO_ARGV, *CHECKED_FRONTENDS)
    ]
    return min((match for match in matches if match), key=lambda match: match.start(), default=None)


def is_comment_or_diagnostic(line: str, match: re.Match[str]) -> bool:
    stripped = line.lstrip()
    if stripped.startswith(("#", "//", "*")):
        return True
    prefix = line[: match.start()]
    if prefix.count("`") % 2 == 1 and "`" in line[match.end() :]:
        return True
    return bool(re.search(r"(?:echo|printf|fail|die|abort)\s+[\"']?[^\n]*$", prefix))


def is_owned(path: Path, lines: list[str], index: int, match: re.Match[str]) -> bool:
    if any(spelling in lines[index][: match.start()] for spelling in OWNED_SPELLINGS):
        return True
    if index and lines[index - 1].rstrip().endswith("\\") and any(
        spelling in lines[index - 1] for spelling in OWNED_SPELLINGS
    ):
        return True
    if path.name == "Taskfile.yaml":
        context = "\n".join(lines[max(0, index - 3) : index])
        return "build_ownership.py shared" in context or "build_ownership.py exclusive" in context
    if path.name == "test-build-ownership.sh":
        context = "\n".join(lines[max(0, index - 4) : index])
        return '"$WRAPPER" shared' in context or '"$WRAPPER" exclusive' in context
    return False


def scan(root: Path) -> list[str]:
    failures: list[str] = []
    for path in entrypoint_files(root):
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        for index, line in enumerate(lines):
            match = target_match(line)
            if match is None or is_comment_or_diagnostic(line, match):
                continue
            if is_owned(path, lines, index, match):
                continue
            failures.append(f"{path.relative_to(root)}:{index + 1}: {line.lstrip()}")
    return failures


def has_fixed_workflow_target(path: Path) -> bool:
    lines = path.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines):
        if line == "env:":
            for child in lines[index + 1 :]:
                if child and not child.startswith(" "):
                    break
                if re.fullmatch(r"  CARGO_TARGET_DIR:\s*['\"]?target['\"]?", child):
                    return True
    return False


def self_test() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        fixtures = {
            "Taskfile.yaml": "tasks:\n  bad:\n    cmds:\n      - $CARGO run -p flux-cli\n",
            ".github/workflows/bare-cargo-dist-build.yml": (
                "jobs:\n  bad:\n    steps:\n      - run: dist build --artifacts=global\n"
            ),
            ".github/actions/nested/action.yml": "runs:\n  steps:\n    - run: command cargo +nightly clippy\n",
            "scripts/release/deep-build.ps1": "& $env:CARGO test --workspace\n",
            "bench/windows/build.cmd": "%CARGO% build --release\n",
            "bench/windows/direct.cmd": "cargo.exe test --workspace\n",
            "scripts/cross-build.sh": "cross build --release\n",
            "scripts/nextest.sh": "cargo nextest run --workspace\n",
            "scripts/ruby-generated-command.sh": (
                'system(run.sub(/owned wrapper/, "dist build"))\n'
            ),
            "crates/fixture/nested-build.py": 'subprocess.run(["cargo", "check"])\n',
        }
        safe_fixtures = {
            "scripts/release-integrity-fixture.sh": (
                'next unless run.include?("-- dist build")\n'
                'step["run"] = run.sub(/build_ownership\\.py shared -- dist build/, "dist build")\n'
            ),
        }
        for relative, content in {**fixtures, **safe_fixtures}.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        failures = scan(root)
        missing = [relative for relative in fixtures if not any(row.startswith(relative + ":") for row in failures)]
        if missing:
            raise AssertionError(f"entry-point self-test missed alternate/nested bypasses: {missing}")
        false_positives = [
            relative
            for relative in safe_fixtures
            if any(row.startswith(relative + ":") for row in failures)
        ]
        if false_positives:
            raise AssertionError(
                f"entry-point self-test treated fixture literals as commands: {false_positives}"
            )


def main() -> int:
    self_test()
    failures = scan(ROOT)
    for relative in FIXED_TARGET_WORKFLOWS:
        path = ROOT / relative
        if not has_fixed_workflow_target(path):
            failures.append(
                f"{relative}: hard-coded target consumers require top-level CARGO_TARGET_DIR: target"
            )
    for relative in RESOLVED_CONSUMER_SCRIPTS:
        text = (ROOT / relative).read_text(encoding="utf-8")
        if "owned_target" not in text and "with_build_ownership" not in text:
            failures.append(f"{relative}: build consumer does not use the resolved governed target")
    for relative in PREBUILT_CONSUMER_SCRIPTS:
        path = ROOT / relative
        text = path.read_text(encoding="utf-8")
        actual_commands = [
            line
            for line in text.splitlines()
            if (match := target_match(line)) is not None and not is_comment_or_diagnostic(line, match)
        ]
        if actual_commands or "Prebuilt-consumer only" not in text or "FLUX_BIN" not in text:
            failures.append(
                f"{relative}: prebuilt-only consumer gained a builder or lost its FLUX_BIN contract"
            )
    if failures:
        print(
            "repository build entry points bypass ownership or target resolution:\n  "
            + "\n  ".join(failures),
            file=sys.stderr,
        )
        return 1
    print("repository Cargo/cargo-dist entry-point inventory is ownership-complete")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
