#!/usr/bin/env python3
"""Release-candidate receipt v3 and the raw-ZIP trust boundary it authenticates (C-355).

WHAT v2 PROVED, AND WHAT IT DID NOT
-----------------------------------
Receipt v2 bound a version, a commit and an immutable run ID, so a tag could only promote artifacts
from one specific successful run. That is a statement about *which run*. It says nothing about *what
came out of it*: the publishing run then downloaded `artifacts-*` by glob and let
`merge-multiple: true` decide what the assembled directory contained. Anything that could add,
replace or expire an upload between recording and promotion was inside the trust boundary, and the
checksums inside the archives authenticate neither the transport nor the API handoff — they are
produced by the same run and travel in the same bytes.

v3 binds the closure. The receipt names each of the seven expected uploads with the API-reported
name, the immutable database ID, the size and GitHub's own SHA-256 of the ZIP. The consumer then
downloads by ID, hashes the raw response bytes BEFORE opening them, and only then extracts — into a
fresh namespace per record, with an extractor that refuses every archive-member shape that can write
outside its namespace.

THE ORDER MATTERS AND IS NOT NEGOTIABLE
---------------------------------------
    metadata identity -> raw byte digest -> ZIP structure -> namespaced extraction -> merge

Each stage may only run on input the previous stage accepted. Hashing after opening the archive
would authenticate a parse of the bytes rather than the bytes; extracting before hashing would have
already written attacker-chosen paths by the time the digest failed.

    scripts/candidate_artifacts.py write  <receipt> <version> <commit> <run-id> [--artifacts FILE]
    scripts/candidate_artifacts.py verify <receipt> <version> <commit> <run-id>
    scripts/candidate_artifacts.py fetch  <receipt> <dest> --run-id N [--source DIR]
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import zipfile


SCHEMA = "flux-release-candidate-v3"
GATE = "mandatory-full-v1"

# The exact producer closure of `.github/workflows/release.yml`: one plan upload, five target
# uploads and one global upload. The receipt is written in this order, and only this order is
# canonical — a reordered receipt is a different document, not a formatting variant.
EXPECTED_ARTIFACTS = (
    "artifacts-plan-dist-manifest",
    "artifacts-build-local-aarch64-apple-darwin",
    "artifacts-build-local-aarch64-unknown-linux-gnu",
    "artifacts-build-local-x86_64-apple-darwin",
    "artifacts-build-local-x86_64-unknown-linux-gnu",
    "artifacts-build-local-x86_64-pc-windows-msvc",
    "artifacts-build-global",
)
ARTIFACT_PREFIX = "artifacts-"

VERSION_RE = re.compile(r"\A[0-9]+\.[0-9]+\.[0-9]+\Z")
COMMIT_RE = re.compile(r"\A[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"\Asha256:[0-9a-f]{64}\Z")
ARTIFACT_LINE_RE = re.compile(
    r"\Aartifact name=(?P<name>\S+) id=(?P<id>[0-9]+) size=(?P<size>[0-9]+) "
    r"digest=(?P<digest>\S+)\Z"
)
# A ZIP local file header. An artifact download that does not start with one is not an artifact —
# most often it is an HTML sign-in page or an API error document served with a 200.
ZIP_MAGIC = (b"PK\x03\x04", b"PK\x05\x06")
# Extraction destinations are release assets, not arbitrary paths. Every path component must be an
# ordinary file name; the depth cap keeps a pathological archive from exhausting the filesystem.
MEMBER_COMPONENT_RE = re.compile(r"\A[A-Za-z0-9][A-Za-z0-9._+-]*\Z")
MAX_MEMBER_DEPTH = 6


class CandidateError(Exception):
    """Any failure of the candidate handoff. Every one of them is fail-closed."""


@dataclass(frozen=True)
class Record:
    name: str
    identifier: int
    size: int
    digest: str


# ---------------------------------------------------------------------------
# Receipt encoding
# ---------------------------------------------------------------------------
def render_receipt(version: str, commit: str, run_id: int, records) -> str:
    _check_scalars(version, commit, run_id)
    ordered = _canonical_records(records)
    lines = [
        f"schema={SCHEMA}",
        f"version={version}",
        f"tag=v{version}",
        f"commit={commit}",
        f"gate={GATE}",
        f"gate_commit={commit}",
        f"run_id={run_id}",
    ]
    lines += [
        f"artifact name={r.name} id={r.identifier} size={r.size} digest={r.digest}"
        for r in ordered
    ]
    return "\n".join(lines) + "\n"


def parse_receipt(text: str):
    """Parse strictly, then prove the input WAS the canonical encoding of what was parsed.

    Re-rendering and comparing byte-for-byte is what makes "one deterministic encoding" checkable:
    reordered records, CRLF, padded separators, a missing or extra trailing newline and appended
    lines all survive a field-by-field parse and none of them survive this.
    """
    if "\r" in text:
        raise CandidateError("candidate receipt must use LF line endings")

    lines = text.split("\n")
    if not lines or lines[-1] != "":
        raise CandidateError("candidate receipt must end with exactly one newline")
    lines = lines[:-1]

    fields = {}
    records = []
    for line in lines:
        if line.startswith("artifact "):
            match = ARTIFACT_LINE_RE.match(line)
            if not match:
                raise CandidateError(f"malformed artifact record: {line!r}")
            records.append(
                Record(
                    name=match["name"],
                    identifier=int(match["id"]),
                    size=int(match["size"]),
                    digest=match["digest"],
                )
            )
            continue
        key, separator, value = line.partition("=")
        if not separator:
            raise CandidateError(f"unrecognized candidate receipt line: {line!r}")
        if key in fields:
            raise CandidateError(f"duplicate candidate receipt field: {key}")
        fields[key] = value

    schema = fields.get("schema")
    if schema != SCHEMA:
        raise CandidateError(
            f"candidate receipt schema is {schema!r}; this consumer requires {SCHEMA}. "
            "A v2 receipt binds no artifact identities or digests and is not accepted as a "
            "compatibility substitute."
        )
    expected_keys = {"schema", "version", "tag", "commit", "gate", "gate_commit", "run_id"}
    if set(fields) != expected_keys:
        unexpected = sorted(set(fields) ^ expected_keys)
        raise CandidateError(f"candidate receipt has unexpected fields: {unexpected}")

    version = fields["version"]
    commit = fields["commit"]
    try:
        run_id = int(fields["run_id"])
    except ValueError as error:
        raise CandidateError("candidate run ID must be a positive integer") from error
    _check_scalars(version, commit, run_id)
    if fields["tag"] != f"v{version}":
        raise CandidateError("candidate receipt tag does not match its version")
    if fields["gate"] != GATE:
        raise CandidateError(f"candidate receipt gate marker must be {GATE}")
    if fields["gate_commit"] != commit:
        raise CandidateError("candidate receipt gate commit does not match its commit")

    _validate_records(records)
    if render_receipt(version, commit, run_id, records) != text:
        raise CandidateError("candidate receipt is not in the one canonical order and encoding")
    return version, commit, run_id, records


def verify_receipt(path, version: str, commit: str, run_id: int):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise CandidateError(f"candidate receipt is missing or is not a regular file: {path}")
    parsed_version, parsed_commit, parsed_run, records = parse_receipt(
        path.read_text(encoding="utf-8")
    )
    if (parsed_version, parsed_commit, parsed_run) != (version, commit, int(run_id)):
        raise CandidateError(
            f"candidate receipt does not match version {version}, commit {commit} and "
            f"run {run_id}"
        )
    return records


def _check_scalars(version, commit, run_id):
    if not VERSION_RE.match(str(version)):
        raise CandidateError(f"candidate version must be plain X.Y.Z, got: {version}")
    if not COMMIT_RE.match(str(commit)):
        raise CandidateError("candidate commit must be a full lowercase 40-hex SHA")
    if not isinstance(run_id, int) or run_id <= 0:
        raise CandidateError("candidate workflow run ID must be a positive integer")


def _validate_records(records):
    names = [r.name for r in records]
    if names != list(EXPECTED_ARTIFACTS):
        missing = [n for n in EXPECTED_ARTIFACTS if n not in names]
        extra = [n for n in names if n not in EXPECTED_ARTIFACTS]
        raise CandidateError(
            "candidate receipt must bind exactly the seven expected uploads in canonical order "
            f"(missing={missing}, extra={extra}, got={names})"
        )
    identifiers = [r.identifier for r in records]
    if len(set(identifiers)) != len(identifiers):
        raise CandidateError("candidate receipt binds the same artifact ID twice")
    for record in records:
        if record.identifier <= 0:
            raise CandidateError(f"{record.name}: artifact ID must be positive")
        if record.size <= 0:
            raise CandidateError(f"{record.name}: artifact size must be positive")
        if not DIGEST_RE.match(record.digest):
            raise CandidateError(
                f"{record.name}: digest must be spelled sha256:<64 lowercase hex>, "
                f"got {record.digest!r}"
            )


def _canonical_records(records):
    by_name = {}
    for record in records:
        if record.name in by_name:
            raise CandidateError(f"duplicate artifact record for {record.name}")
        by_name[record.name] = record
    ordered = [by_name[name] for name in EXPECTED_ARTIFACTS if name in by_name]
    _validate_records(ordered if len(ordered) == len(by_name) else list(by_name.values()))
    return ordered


# ---------------------------------------------------------------------------
# Recording: identity comes from the artifacts API, never from a file inside the archive
# ---------------------------------------------------------------------------
def records_from_api(artifacts, run_id: int):
    run_id = int(run_id)
    seen_names = set()
    seen_ids = set()
    records = []
    for entry in artifacts:
        name = entry.get("name")
        if not isinstance(name, str) or not name.startswith(ARTIFACT_PREFIX):
            # The candidate receipt itself is uploaded under a different name on purpose, so it can
            # never become a release asset. Anything else outside the prefix is not our closure.
            continue
        if entry.get("expired"):
            raise CandidateError(f"{name}: the upload has expired; the candidate cannot be promoted")
        if name in seen_names:
            raise CandidateError(f"{name}: the run reports two uploads with the same name")
        seen_names.add(name)

        identifier = entry.get("id")
        if not isinstance(identifier, int) or isinstance(identifier, bool) or identifier <= 0:
            raise CandidateError(f"{name}: artifact ID must be a positive integer, got {identifier!r}")
        if identifier in seen_ids:
            raise CandidateError(f"{name}: the run reports two uploads with artifact ID {identifier}")
        seen_ids.add(identifier)

        size = entry.get("size_in_bytes")
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
            raise CandidateError(f"{name}: size_in_bytes must be a positive integer, got {size!r}")

        digest = entry.get("digest")
        if not isinstance(digest, str) or not DIGEST_RE.match(digest):
            raise CandidateError(
                f"{name}: the artifacts API reported digest {digest!r}; the receipt requires the "
                "exact spelling sha256:<64 lowercase hex>"
            )

        reported_run = (entry.get("workflow_run") or {}).get("id")
        if reported_run is not None and int(reported_run) != run_id:
            raise CandidateError(
                f"{name}: belongs to run {reported_run}, not the candidate run {run_id}"
            )
        records.append(Record(name=name, identifier=identifier, size=size, digest=digest))

    return _canonical_records(records)


# ---------------------------------------------------------------------------
# Consuming: raw bytes first, structure second, filesystem last
# ---------------------------------------------------------------------------
class GhDownloader:
    """The artifacts API through `gh`. Metadata and bytes are fetched for one immutable ID."""

    def __init__(self, repository: str, gh: str = "gh"):
        self.repository = repository
        self.gh = gh

    def metadata(self, identifier: int):
        done = subprocess.run(
            [self.gh, "api", f"repos/{self.repository}/actions/artifacts/{identifier}"],
            capture_output=True,
        )
        if done.returncode != 0:
            raise CandidateError(
                f"could not read artifact {identifier}: {done.stderr.decode(errors='replace').strip()}"
            )
        return json.loads(done.stdout)

    def download(self, identifier: int) -> bytes:
        # `gh api` follows the storage redirect itself. What lands here is whatever the redirect
        # resolved to, which is exactly why the caller hashes it before believing it.
        done = subprocess.run(
            [self.gh, "api", f"repos/{self.repository}/actions/artifacts/{identifier}/zip"],
            capture_output=True,
        )
        if done.returncode != 0:
            raise CandidateError(
                f"could not download artifact {identifier}: "
                f"{done.stderr.decode(errors='replace').strip()}"
            )
        return done.stdout


class LocalDownloader:
    """A directory of `<id>.json` + `<id>.zip`, for hermetic tests of the same code path."""

    def __init__(self, directory):
        self.directory = Path(directory)

    def metadata(self, identifier: int):
        path = self.directory / f"{identifier}.json"
        if not path.is_file():
            raise CandidateError(f"no artifact {identifier} in this run")
        return json.loads(path.read_text())

    def download(self, identifier: int) -> bytes:
        path = self.directory / f"{identifier}.zip"
        if not path.is_file():
            raise CandidateError(f"no artifact bytes for {identifier}")
        return path.read_bytes()


def fetch(receipt_path, destination, downloader, run_id: int):
    """Download, authenticate and safely assemble the seven receipt-bound archives."""
    run_id = int(run_id)
    version, commit, receipt_run, records = parse_receipt(
        Path(receipt_path).read_text(encoding="utf-8")
    )
    if receipt_run != run_id:
        raise CandidateError(
            f"candidate receipt names run {receipt_run}, but promotion is consuming run {run_id}"
        )

    destination = Path(destination)
    raw_dir = destination / "raw"
    namespaces = destination / "namespaces"
    merged = destination / "merged"
    for directory in (raw_dir, namespaces, merged):
        if directory.exists():
            raise CandidateError(f"candidate consumption directory already exists: {directory}")
        directory.mkdir(parents=True)

    taken: set[str] = set()
    for record in records:
        metadata = downloader.metadata(record.identifier)
        _check_metadata(record, metadata, run_id)

        raw = downloader.download(record.identifier)
        _check_raw_bytes(record, raw)

        raw_path = raw_dir / f"{record.name}.zip"
        raw_path.write_bytes(raw)
        _check_zip_structure(record, raw_path)

        safe_extract(raw_path, namespaces / record.name, taken)

    # Only now, with all seven namespaces verified, is the host input assembled. `merge-multiple`
    # is a convenience in the download action; it is not, and was never, a trust boundary.
    for record in records:
        namespace = namespaces / record.name
        for source in sorted(p for p in namespace.rglob("*") if p.is_file()):
            relative = source.relative_to(namespace)
            target = merged / relative
            if target.exists():
                raise CandidateError(
                    f"{record.name}: {relative} collides with a member of another archive"
                )
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    return merged


def _check_metadata(record: Record, metadata, run_id: int):
    if metadata.get("expired"):
        raise CandidateError(f"{record.name}: the receipt-bound upload has expired")
    for field, expected, actual in (
        ("id", record.identifier, metadata.get("id")),
        ("name", record.name, metadata.get("name")),
        ("size_in_bytes", record.size, metadata.get("size_in_bytes")),
        ("digest", record.digest, metadata.get("digest")),
    ):
        if actual != expected:
            raise CandidateError(
                f"{record.name}: artifact {record.identifier} reports {field}={actual!r}, but the "
                f"receipt binds {expected!r}; this download resolves to a different artifact"
            )
    reported_run = (metadata.get("workflow_run") or {}).get("id")
    if reported_run is not None and int(reported_run) != run_id:
        raise CandidateError(
            f"{record.name}: artifact {record.identifier} belongs to run {reported_run}, "
            f"not the candidate run {run_id}"
        )


def _check_raw_bytes(record: Record, raw: bytes):
    if len(raw) != record.size:
        raise CandidateError(
            f"{record.name}: the response is {len(raw)} bytes, the receipt binds {record.size}"
        )
    actual = "sha256:" + hashlib.sha256(raw).hexdigest()
    if actual != record.digest:
        raise CandidateError(
            f"{record.name}: the raw response hashes to {actual}, the receipt binds {record.digest}"
        )
    if not raw.startswith(ZIP_MAGIC):
        raise CandidateError(
            f"{record.name}: the response is not a ZIP archive (no local file header). An HTML "
            "sign-in page or API error document served with a 200 lands here."
        )


def _check_zip_structure(record: Record, path: Path):
    if not zipfile.is_zipfile(path):
        raise CandidateError(f"{record.name}: the archive has no readable central directory")
    try:
        with zipfile.ZipFile(path) as archive:
            broken = archive.testzip()
    except (zipfile.BadZipFile, OSError, EOFError) as error:
        raise CandidateError(f"{record.name}: the archive is truncated or corrupt ({error})") from error
    if broken is not None:
        raise CandidateError(f"{record.name}: member {broken!r} failed its CRC check")


def safe_extract(archive_path, target, taken: set):
    """Extract into a FRESH directory, refusing every member that can write outside it.

    The rejections are syntactic and checked before anything is written, because a containment check
    performed after a write has already lost. `taken` carries destination paths across archives, so
    two archives cannot land on the same file even though each is individually well formed.
    """
    target = Path(target)
    if target.exists():
        raise CandidateError(f"extraction namespace is not fresh: {target}")

    written = []
    with zipfile.ZipFile(archive_path) as archive:
        infos = archive.infolist()
        members = set()
        planned = []
        for info in infos:
            name = info.filename
            is_directory = info.is_dir()
            _check_member_name(name, is_directory)
            if name in members:
                raise CandidateError(f"duplicate archive member: {name!r}")
            members.add(name)
            _check_member_kind(info)
            if is_directory:
                continue
            relative = name.rstrip("/")
            if relative in taken:
                raise CandidateError(f"{relative}: collides with a member of another archive")
            planned.append((info, relative))

        # Reserve every destination before writing any of them: a half-extracted archive that then
        # fails is still a set of attacker-chosen files on disk.
        for _, relative in planned:
            taken.add(relative)
        target.mkdir(parents=True)
        for info, relative in planned:
            destination = target / relative
            resolved = os.path.realpath(destination)
            if not (resolved == os.path.realpath(target)
                    or resolved.startswith(os.path.realpath(target) + os.sep)):
                raise CandidateError(f"{relative}: resolves outside its extraction namespace")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(info) as source, open(destination, "wb") as sink:
                shutil.copyfileobj(source, sink)
            written.append(relative)
    return written


def _check_member_name(name: str, is_directory: bool):
    if not name or name in (".", "./"):
        raise CandidateError("archive member has an empty name")
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in name):
        raise CandidateError(f"archive member name contains a NUL or control character: {name!r}")
    if "\\" in name:
        raise CandidateError(f"archive member name contains a backslash separator: {name!r}")
    if name.startswith("/"):
        raise CandidateError(f"archive member name is an absolute path: {name!r}")
    if re.match(r"\A[A-Za-z]:", name):
        raise CandidateError(f"archive member name carries a drive letter: {name!r}")

    components = [part for part in name.split("/") if part != ""]
    if not components:
        raise CandidateError(f"archive member name has no path components: {name!r}")
    if ".." in components or "." in components:
        raise CandidateError(f"archive member name traverses its namespace: {name!r}")
    if len(components) > MAX_MEMBER_DEPTH:
        raise CandidateError(f"archive member name is nested too deeply: {name!r}")
    for component in components:
        if not MEMBER_COMPONENT_RE.match(component):
            raise CandidateError(
                f"archive member name is not an allowlisted release asset path: {name!r}"
            )
    if not is_directory and name.endswith("/"):
        raise CandidateError(f"archive member is ambiguous about being a directory: {name!r}")


def _check_member_kind(info: zipfile.ZipInfo):
    mode = info.external_attr >> 16
    if mode == 0:
        return
    file_type = stat.S_IFMT(mode)
    if file_type in (0, stat.S_IFREG, stat.S_IFDIR):
        return
    kinds = {
        stat.S_IFLNK: "a symbolic link",
        stat.S_IFIFO: "a FIFO",
        stat.S_IFCHR: "a character device",
        stat.S_IFBLK: "a block device",
        stat.S_IFSOCK: "a socket",
    }
    raise CandidateError(
        f"archive member {info.filename!r} is {kinds.get(file_type, 'not a regular file')}; "
        "release archives contain regular files and directories only"
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def _artifacts_from_gh(repository: str, run_id: int, gh: str = "gh"):
    done = subprocess.run(
        [
            gh, "api", "--paginate",
            f"repos/{repository}/actions/runs/{run_id}/artifacts?per_page=100",
            "--jq", ".artifacts[]",
        ],
        capture_output=True,
    )
    if done.returncode != 0:
        raise CandidateError(
            "could not list the candidate run's artifacts: "
            f"{done.stderr.decode(errors='replace').strip()}"
        )
    return [json.loads(line) for line in done.stdout.decode().splitlines() if line.strip()]


def _load_artifacts(path):
    payload = json.loads(Path(path).read_text())
    return payload["artifacts"] if isinstance(payload, dict) else payload


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    for name in ("write", "verify"):
        command = sub.add_parser(name)
        command.add_argument("receipt")
        command.add_argument("version")
        command.add_argument("commit")
        command.add_argument("run_id")
        command.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
        if name == "write":
            command.add_argument("--artifacts", help="artifacts API payload, instead of calling gh")

    sub.add_parser("names", help="print the canonical producer closure, one name per line")

    consume = sub.add_parser("fetch")
    consume.add_argument("receipt")
    consume.add_argument("destination")
    consume.add_argument("--run-id", required=True)
    consume.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    consume.add_argument("--source", help="a local <id>.json/<id>.zip directory, instead of gh")

    args = parser.parse_args(argv)
    gh = os.environ.get("GH_CLI", "gh")

    if args.command == "names":
        print("\n".join(EXPECTED_ARTIFACTS))
        return 0

    try:
        if args.command in ("write", "verify"):
            try:
                run_id = int(args.run_id)
            except ValueError as error:
                raise CandidateError("candidate workflow run ID must be a positive integer") from error
            _check_scalars(args.version, args.commit, run_id)

            if args.command == "verify":
                verify_receipt(args.receipt, args.version, args.commit, run_id)
                return 0

            receipt = Path(args.receipt)
            if receipt.is_symlink():
                raise CandidateError(f"refusing to write a candidate receipt through a symlink: {receipt}")
            artifacts = (
                _load_artifacts(args.artifacts)
                if args.artifacts
                else _artifacts_from_gh(_require_repo(args.repo), run_id, gh)
            )
            records = records_from_api(artifacts, run_id)
            text = render_receipt(args.version, args.commit, run_id, records)
            # Prove the document we are about to hand on parses as its own canonical encoding.
            parse_receipt(text)
            receipt.write_text(text, encoding="utf-8")
            return 0

        downloader = (
            LocalDownloader(args.source)
            if args.source
            else GhDownloader(_require_repo(args.repo), gh)
        )
        merged = fetch(args.receipt, args.destination, downloader, int(args.run_id))
        print(merged)
        return 0
    except CandidateError as error:
        marker = "::error::" if os.environ.get("GITHUB_ACTIONS") == "true" else "error: "
        print(f"{marker}{error}", file=sys.stderr)
        return 1


def _require_repo(repository: str) -> str:
    if not re.match(r"\A[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z", repository or ""):
        raise CandidateError("a valid owner/name repository is required (set GITHUB_REPOSITORY)")
    return repository


if __name__ == "__main__":
    sys.exit(main())
