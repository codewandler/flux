#!/usr/bin/env python3
"""Adversarial fixtures for the release-candidate receipt v3 and its raw-ZIP trust boundary (C-355).

Receipt v2 bound a version, a commit and a run ID. That authenticates *which run* produced the
release, and nothing about *what came out of it*: the tag run then downloaded `artifacts-*` by
pattern and trusted `merge-multiple: true` to assemble the published bytes. v3 closes that by
binding each of the seven expected uploads to its API-reported name, immutable database ID, size and
SHA-256 digest, and by making the consumer hash the raw ZIP bytes before it opens them.

Every test below is one way that handoff can be corrupted. They are written against the module's
public surface rather than against a workflow file, because the properties are about bytes.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parent.parent
MODULE = ROOT / "scripts" / "candidate_artifacts.py"
HELPER = ROOT / "scripts" / "release-candidate.sh"


def load_module():
    spec = importlib.util.spec_from_file_location("candidate_artifacts", MODULE)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    # Register before executing: dataclasses resolves annotations through sys.modules, and a
    # module loaded purely by path is invisible there.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


ca = load_module()

VERSION = "1.2.3"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
RUN_ID = 123456789


def zip_bytes(members, *, mode=None, truncate=0):
    """A ZIP whose member list is given exactly as passed — duplicates and all."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, payload in members:
            info = zipfile.ZipInfo(name)
            info.date_time = (2026, 1, 1, 0, 0, 0)
            if mode is not None:
                info.create_system = 3
                info.external_attr = mode << 16
            archive.writestr(info, payload)
    raw = buffer.getvalue()
    return raw[: len(raw) - truncate] if truncate else raw


def api_artifact(name, artifact_id, raw, *, expired=False, run_id=RUN_ID, digest=None, size=None):
    return {
        "id": artifact_id,
        "name": name,
        "size_in_bytes": len(raw) if size is None else size,
        "digest": digest if digest is not None else "sha256:" + hashlib.sha256(raw).hexdigest(),
        "expired": expired,
        "workflow_run": {"id": run_id},
    }


def producer_closure(**overrides):
    """The exact five-target + global + plan set a healthy candidate run produces."""
    payload = {}
    artifacts = []
    for index, name in enumerate(ca.EXPECTED_ARTIFACTS):
        raw = zip_bytes([(f"{name}-payload.txt", f"content of {name}")])
        payload[name] = raw
        artifacts.append(api_artifact(name, 4000 + index, raw, **overrides.get(name, {})))
    return artifacts, payload


class ReceiptShapeTests(unittest.TestCase):
    def setUp(self):
        self.artifacts, self.payload = producer_closure()
        self.records = ca.records_from_api(self.artifacts, RUN_ID)

    def test_a_healthy_run_round_trips_deterministically(self):
        text = ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records)
        self.assertTrue(text.startswith("schema=flux-release-candidate-v3\n"))
        self.assertEqual(text, ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records))
        version, commit, run_id, records = ca.parse_receipt(text)
        self.assertEqual((version, commit, run_id), (VERSION, COMMIT, RUN_ID))
        self.assertEqual([r.name for r in records], list(ca.EXPECTED_ARTIFACTS))
        self.assertEqual(text, ca.render_receipt(version, commit, run_id, records))

    def test_the_receipt_names_the_exact_seven_uploads(self):
        text = ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records)
        for name in ca.EXPECTED_ARTIFACTS:
            self.assertIn(name, text)
        self.assertEqual(text.count("\nartifact "), 7)

    def test_every_record_binds_identity_size_and_digest(self):
        for record in self.records:
            self.assertGreater(record.identifier, 0)
            self.assertGreater(record.size, 0)
            self.assertRegex(record.digest, r"\Asha256:[0-9a-f]{64}\Z")

    def test_a_missing_upload_fails_closed(self):
        artifacts = [a for a in self.artifacts if a["name"] != "artifacts-build-global"]
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_an_extra_artifacts_upload_fails_closed(self):
        raw = zip_bytes([("surprise.txt", "surprise")])
        artifacts = self.artifacts + [api_artifact("artifacts-build-local-extra", 9999, raw)]
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_an_expired_upload_fails_closed(self):
        artifacts, _ = producer_closure(**{"artifacts-build-global": {"expired": True}})
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_a_duplicate_name_fails_closed(self):
        artifacts = list(self.artifacts)
        artifacts.append(dict(artifacts[0], id=8888))
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_a_duplicate_id_fails_closed(self):
        artifacts = [dict(a) for a in self.artifacts]
        artifacts[1]["id"] = artifacts[0]["id"]
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_an_upload_from_another_run_fails_closed(self):
        artifacts, _ = producer_closure(**{"artifacts-build-global": {"run_id": RUN_ID + 1}})
        with self.assertRaises(ca.CandidateError):
            ca.records_from_api(artifacts, RUN_ID)

    def test_a_nonpositive_id_or_size_fails_closed(self):
        for field, value in (("id", 0), ("id", -1), ("size_in_bytes", 0)):
            artifacts = [dict(a) for a in self.artifacts]
            artifacts[0][field] = value
            with self.assertRaises(ca.CandidateError):
                ca.records_from_api(artifacts, RUN_ID)

    def test_an_absent_uppercase_or_malformed_digest_fails_closed(self):
        good = self.artifacts[0]["digest"]
        for digest in (
            "",
            None,
            good.upper(),
            good.replace("sha256:", "SHA256:"),
            good.replace("sha256:", ""),
            good[:-1],
            good + "0",
            "sha512:" + "a" * 128,
        ):
            artifacts = [dict(a) for a in self.artifacts]
            artifacts[0]["digest"] = digest
            with self.assertRaises(ca.CandidateError):
                ca.records_from_api(artifacts, RUN_ID)

    def test_a_reordered_receipt_is_not_canonical(self):
        text = ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records)
        lines = text.splitlines()
        head = [line for line in lines if not line.startswith("artifact ")]
        body = [line for line in lines if line.startswith("artifact ")]
        reordered = "\n".join(head + list(reversed(body))) + "\n"
        self.assertNotEqual(reordered, text)
        with self.assertRaises(ca.CandidateError):
            ca.parse_receipt(reordered)

    def test_a_noncanonical_encoding_is_rejected(self):
        text = ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records)
        for mutation in (
            text.replace("\n", "\r\n"),
            text + "extra=untrusted\n",
            text.replace("artifact name=", "artifact  name=", 1),
            text.rstrip("\n"),
        ):
            with self.assertRaises(ca.CandidateError):
                ca.parse_receipt(mutation)

    def test_v2_is_not_accepted_as_a_compatibility_substitute(self):
        v2 = (
            "schema=flux-release-candidate-v2\n"
            f"version={VERSION}\n"
            f"tag=v{VERSION}\n"
            f"commit={COMMIT}\n"
            "gate=mandatory-full-v1\n"
            f"gate_commit={COMMIT}\n"
            f"run_id={RUN_ID}\n"
        )
        with self.assertRaises(ca.CandidateError) as caught:
            ca.parse_receipt(v2)
        self.assertIn("v3", str(caught.exception))

    def test_verification_rejects_a_wrong_version_commit_or_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "release-candidate.txt"
            path.write_text(ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records))
            ca.verify_receipt(path, VERSION, COMMIT, RUN_ID)
            for version, commit, run_id in (
                ("1.2.4", COMMIT, RUN_ID),
                (VERSION, "a" + COMMIT[1:], RUN_ID),
                (VERSION, COMMIT, RUN_ID + 1),
            ):
                with self.assertRaises(ca.CandidateError):
                    ca.verify_receipt(path, version, commit, run_id)


class RawByteBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.artifacts, self.payload = producer_closure()
        self.raw_by_id = {a["id"]: self.payload[a["name"]] for a in self.artifacts}
        self.records = ca.records_from_api(self.artifacts, RUN_ID)
        self.receipt = self.root / "release-candidate.txt"
        self.receipt.write_text(ca.render_receipt(VERSION, COMMIT, RUN_ID, self.records))

    def source(self, payload=None, artifacts=None):
        """A local stand-in for the artifacts API: metadata JSON plus the raw ZIP per ID."""
        directory = Path(tempfile.mkdtemp(dir=self.root))
        entries = artifacts or self.artifacts
        payload = payload or self.payload
        for entry in entries:
            (directory / f"{entry['id']}.json").write_text(json.dumps(entry))
            # Fall back to the ID when a fixture has renamed the artifact: the bytes served on that
            # immutable ID are unchanged, only the metadata describing them has moved.
            raw = payload.get(entry["name"], self.raw_by_id.get(entry["id"]))
            (directory / f"{entry['id']}.zip").write_bytes(raw)
        return ca.LocalDownloader(directory)

    def fetch(self, **kwargs):
        return ca.fetch(self.receipt, self.root / "consume", self.source(**kwargs), RUN_ID)

    def test_a_healthy_candidate_assembles_every_namespace(self):
        merged = self.fetch()
        for name in ca.EXPECTED_ARTIFACTS:
            self.assertTrue((self.root / "consume" / "namespaces" / name).is_dir())
            self.assertTrue((merged / f"{name}-payload.txt").is_file())

    def test_a_byte_tampered_zip_fails_before_it_is_opened(self):
        payload = dict(self.payload)
        target = "artifacts-build-global"
        raw = bytearray(payload[target])
        raw[-1] ^= 0xFF
        payload[target] = bytes(raw)
        with self.assertRaises(ca.CandidateError):
            self.fetch(payload=payload)
        self.assertFalse((self.root / "consume" / "namespaces" / target).exists())

    def test_a_non_zip_response_is_refused(self):
        payload = dict(self.payload)
        html = b"<html>sign in to continue</html>"
        payload["artifacts-plan-dist-manifest"] = html
        artifacts = [dict(a) for a in self.artifacts]
        for entry in artifacts:
            if entry["name"] == "artifacts-plan-dist-manifest":
                entry["size_in_bytes"] = len(html)
                entry["digest"] = "sha256:" + hashlib.sha256(html).hexdigest()
        receipt_records = ca.records_from_api(artifacts, RUN_ID)
        self.receipt.write_text(ca.render_receipt(VERSION, COMMIT, RUN_ID, receipt_records))
        with self.assertRaises(ca.CandidateError):
            self.fetch(payload=payload, artifacts=artifacts)

    def test_a_truncated_zip_is_refused(self):
        raw = zip_bytes([("a.txt", "a" * 4096)], truncate=64)
        payload = dict(self.payload)
        payload["artifacts-build-global"] = raw
        artifacts = [dict(a) for a in self.artifacts]
        for entry in artifacts:
            if entry["name"] == "artifacts-build-global":
                entry["size_in_bytes"] = len(raw)
                entry["digest"] = "sha256:" + hashlib.sha256(raw).hexdigest()
        self.receipt.write_text(
            ca.render_receipt(VERSION, COMMIT, RUN_ID, ca.records_from_api(artifacts, RUN_ID))
        )
        with self.assertRaises(ca.CandidateError):
            self.fetch(payload=payload, artifacts=artifacts)

    def test_metadata_that_resolves_to_another_artifact_is_refused(self):
        for field, value in (
            ("name", "artifacts-build-local-something-else"),
            ("id", 7777),
            ("size_in_bytes", 11),
            ("expired", True),
        ):
            artifacts = [dict(a) for a in self.artifacts]
            artifacts[0] = dict(artifacts[0])
            original_id = artifacts[0]["id"]
            artifacts[0][field] = value
            if field == "id":
                # The download still answers on the receipt-bound ID, but the object it describes
                # is a different one. That is the redirect-substitution case.
                artifacts[0]["id"] = original_id
                artifacts[0]["name"] = "artifacts-build-global"
            with self.assertRaises(ca.CandidateError):
                self.fetch(artifacts=artifacts)

    def test_a_run_id_mismatch_is_refused(self):
        artifacts = [dict(a) for a in self.artifacts]
        artifacts[0]["workflow_run"] = {"id": RUN_ID + 1}
        with self.assertRaises(ca.CandidateError):
            self.fetch(artifacts=artifacts)


class SafeExtractionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def extract(self, members, *, mode=None):
        archive = self.root / "archive.zip"
        archive.write_bytes(zip_bytes(members, mode=mode))
        return ca.safe_extract(archive, self.root / "out", set())

    def test_a_plain_archive_extracts(self):
        written = self.extract([("dir/file.txt", "ok"), ("top.txt", "ok")])
        self.assertEqual(sorted(written), ["dir/file.txt", "top.txt"])
        self.assertTrue((self.root / "out" / "dir" / "file.txt").is_file())

    def test_zip_slip_is_refused(self):
        for name in ("../escape.txt", "a/../../escape.txt", "./../escape.txt"):
            with self.subTest(name=name), self.assertRaises(ca.CandidateError):
                self.extract([(name, "x")])

    def test_an_absolute_path_is_refused(self):
        for name in ("/etc/passwd", "//host/share/x", "///x"):
            with self.subTest(name=name), self.assertRaises(ca.CandidateError):
                self.extract([(name, "x")])

    def test_a_drive_or_unc_path_is_refused(self):
        for name in ("C:/windows/x", "C:x", r"\\server\share\x"):
            with self.subTest(name=name), self.assertRaises(ca.CandidateError):
                self.extract([(name, "x")])

    def test_a_backslash_separator_is_refused(self):
        with self.assertRaises(ca.CandidateError):
            self.extract([(r"dir\file.txt", "x")])

    def test_control_characters_are_refused(self):
        for name in ("bad\nname", "bad\tname", "bad\x7fname", "bad\x01name"):
            with self.subTest(name=repr(name)), self.assertRaises(ca.CandidateError):
                self.extract([(name, "x")])

    def test_a_nul_never_reaches_a_destination_path(self):
        # Python's ZIP reader truncates a member name at the first NUL, so the extractor is not the
        # only line of defence here — but the property that matters is the outcome: nothing with a
        # NUL in it is ever created, and whatever survives stays inside the namespace.
        written = self.extract([("bad\x00name", "x")])
        for relative in written:
            self.assertNotIn("\x00", relative)
            resolved = (self.root / "out" / relative).resolve()
            self.assertTrue(str(resolved).startswith(str((self.root / "out").resolve())))

    def test_a_symlink_member_is_refused(self):
        with self.assertRaises(ca.CandidateError):
            self.extract([("link", "/etc/passwd")], mode=stat.S_IFLNK | 0o777)

    def test_a_device_or_fifo_member_is_refused(self):
        for mode in (stat.S_IFIFO | 0o666, stat.S_IFCHR | 0o666, stat.S_IFBLK | 0o666,
                     stat.S_IFSOCK | 0o666):
            with self.subTest(mode=oct(mode)), self.assertRaises(ca.CandidateError):
                self.extract([("special", "")], mode=mode)

    def test_a_duplicate_member_is_refused(self):
        with self.assertRaises(ca.CandidateError):
            self.extract([("same.txt", "first"), ("same.txt", "second")])

    def test_a_cross_archive_collision_is_refused(self):
        first = self.root / "first.zip"
        second = self.root / "second.zip"
        first.write_bytes(zip_bytes([("shared.txt", "one")]))
        second.write_bytes(zip_bytes([("shared.txt", "two")]))
        taken = set()
        ca.safe_extract(first, self.root / "a", taken)
        with self.assertRaises(ca.CandidateError):
            ca.safe_extract(second, self.root / "b", taken)

    def test_extraction_targets_a_fresh_namespace(self):
        target = self.root / "out"
        target.mkdir()
        (target / "stale.txt").write_text("left over")
        archive = self.root / "archive.zip"
        archive.write_bytes(zip_bytes([("new.txt", "x")]))
        with self.assertRaises(ca.CandidateError):
            ca.safe_extract(archive, target, set())


class HelperCliTests(unittest.TestCase):
    """The shell entry point the workflows call must speak v3 and nothing else."""

    def test_the_helper_writes_and_verifies_a_v3_receipt(self):
        artifacts, _ = producer_closure()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inventory = root / "artifacts.json"
            inventory.write_text(json.dumps({"artifacts": artifacts}))
            receipt = root / "release-candidate.txt"
            subprocess.run(
                [str(HELPER), "write", str(receipt), VERSION, COMMIT, str(RUN_ID),
                 "--artifacts", str(inventory)],
                check=True, capture_output=True,
            )
            text = receipt.read_text()
            self.assertTrue(text.startswith("schema=flux-release-candidate-v3\n"), text[:200])
            self.assertEqual(text.count("\nartifact "), 7)
            subprocess.run(
                [str(HELPER), "verify", str(receipt), VERSION, COMMIT, str(RUN_ID)],
                check=True, capture_output=True,
            )

    def test_the_helper_refuses_a_v2_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            receipt = Path(tmp) / "release-candidate.txt"
            receipt.write_text(
                "schema=flux-release-candidate-v2\n"
                f"version={VERSION}\ntag=v{VERSION}\ncommit={COMMIT}\n"
                f"gate=mandatory-full-v1\ngate_commit={COMMIT}\nrun_id={RUN_ID}\n"
            )
            done = subprocess.run(
                [str(HELPER), "verify", str(receipt), VERSION, COMMIT, str(RUN_ID)],
                capture_output=True,
            )
            self.assertNotEqual(done.returncode, 0)

    def test_the_helper_refuses_to_write_through_a_symlink(self):
        artifacts, _ = producer_closure()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inventory = root / "artifacts.json"
            inventory.write_text(json.dumps({"artifacts": artifacts}))
            real = root / "real.txt"
            real.write_text("")
            link = root / "link.txt"
            os.symlink(real, link)
            done = subprocess.run(
                [str(HELPER), "write", str(link), VERSION, COMMIT, str(RUN_ID),
                 "--artifacts", str(inventory)],
                capture_output=True,
            )
            self.assertNotEqual(done.returncode, 0)

    def test_the_helper_rejects_malformed_scalars(self):
        artifacts, _ = producer_closure()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inventory = root / "artifacts.json"
            inventory.write_text(json.dumps({"artifacts": artifacts}))
            receipt = root / "release-candidate.txt"
            for version, commit, run_id in (
                ("v1.2.3", COMMIT, str(RUN_ID)),
                ("1.2", COMMIT, str(RUN_ID)),
                (VERSION, COMMIT[:-1], str(RUN_ID)),
                (VERSION, COMMIT.upper(), str(RUN_ID)),
                (VERSION, COMMIT, "run-1"),
                (VERSION, COMMIT, "0"),
            ):
                done = subprocess.run(
                    [str(HELPER), "write", str(receipt), version, commit, run_id,
                     "--artifacts", str(inventory)],
                    capture_output=True,
                )
                self.assertNotEqual(done.returncode, 0, f"{version} {commit} {run_id}")


if __name__ == "__main__":
    unittest.main()
