#!/usr/bin/env python3
"""Cross-platform regression tests for the Cargo target ownership wrapper."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
WRAPPER = ROOT / "scripts" / "build_ownership.py"
POSIX_LAUNCHER = ROOT / "scripts" / "run-python3.sh"
WINDOWS_LAUNCHER = ROOT / "scripts" / "run-python3.cmd"


def load_wrapper():
    spec = importlib.util.spec_from_file_location("build_ownership", WRAPPER)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {WRAPPER}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PathResolutionTests(unittest.TestCase):
    def test_unset_target_uses_workspace_target(self) -> None:
        module = load_wrapper()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            self.assertEqual(module.resolve_target(root, None), root / "target")

    def test_absolute_target_is_preserved(self) -> None:
        module = load_wrapper()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            selected = root / "operator-target"
            self.assertEqual(module.resolve_target(root, str(selected)), selected)

    def test_relative_target_is_workspace_relative(self) -> None:
        module = load_wrapper()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            self.assertEqual(
                module.resolve_target(root, "cache/flux"), root / "cache" / "flux"
            )

    def test_fleet_shared_target_shape_is_not_rewritten(self) -> None:
        module = load_wrapper()
        root = Path("/workspace/flux")
        selected = Path("/cache/flux-workers/build-targets/flux")
        self.assertEqual(module.resolve_target(root, str(selected)), selected)


class PythonLauncherTests(unittest.TestCase):
    def test_explicit_unsupported_python_has_fixed_actionable_diagnostic(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            sentinel = root / "sensitive-python-location"
            if os.name == "nt":
                fake = sentinel.with_suffix(".cmd")
                fake.write_text("@exit /b 1\r\n", encoding="utf-8")
                command = [
                    os.environ.get("COMSPEC", "cmd.exe"),
                    "/d",
                    "/s",
                    "/c",
                    subprocess.list2cmdline([str(WINDOWS_LAUNCHER), str(WRAPPER), "--help"]),
                ]
            else:
                fake = sentinel
                fake.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
                fake.chmod(0o755)
                command = [str(POSIX_LAUNCHER), str(WRAPPER), "--help"]
            environment = os.environ.copy()
            environment["FLUX_PYTHON"] = str(fake)
            result = subprocess.run(
                command,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 69, result.stderr)
            self.assertIn("Python 3.10+", result.stderr)
            self.assertIn("set PYTHON", result.stderr)
            self.assertNotIn(str(fake), result.stderr)


class OwnershipProcessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.target = self.root / "target"
        self.env = os.environ.copy()
        self.env["CARGO_TARGET_DIR"] = str(self.target)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def command(self, mode: str, *child: str, refuse: bool = False) -> list[str]:
        args = [
            sys.executable,
            str(WRAPPER),
            mode,
            "--workspace-root",
            str(self.root),
        ]
        if refuse:
            args.append("--refuse")
        return [*args, "--", *child]

    def make_directory_alias(self, alias: Path, target: Path) -> None:
        if os.name == "nt":
            result = subprocess.run(
                [os.environ.get("COMSPEC", "cmd.exe"), "/d", "/c", "mklink", "/J", str(alias), str(target)],
                text=True,
                capture_output=True,
                check=False,
            )
            if result.returncode != 0:
                self.skipTest(f"cannot create a Windows junction: {result.stderr}")
        else:
            alias.symlink_to(target, target_is_directory=True)

    def assert_owner_exit(
        self, owner: subprocess.Popen[str], expected: int, close_stdin: bool = True
    ) -> None:
        if close_stdin and owner.stdin is not None and not owner.stdin.closed:
            owner.stdin.close()
        status = owner.wait(timeout=10)
        if owner.stdin is not None and not owner.stdin.closed:
            owner.stdin.close()
        diagnostics = owner.stderr.read() if owner.stderr is not None else ""
        if owner.stdout is not None:
            owner.stdout.close()
        if owner.stderr is not None:
            owner.stderr.close()
        self.assertEqual(status, expected, diagnostics)

    def test_target_is_untouched_when_the_lease_is_acquired(self) -> None:
        module = load_wrapper()
        original_acquire = module._acquire
        observed = False

        def assert_prelease_boundary(lock_file, exclusive, blocking):
            nonlocal observed
            self.assertFalse(
                self.target.exists(),
                "the governed target was touched before build ownership acquisition",
            )
            observed = True
            return original_acquire(lock_file, exclusive, blocking)

        child = (
            "import os, pathlib; "
            "pathlib.Path(os.environ['CARGO_TARGET_DIR']).mkdir(parents=True); "
            "raise SystemExit(0)"
        )
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.target)}), mock.patch.object(
            module, "_acquire", side_effect=assert_prelease_boundary
        ):
            result = module.run("shared", self.root, [sys.executable, "-c", child], False)
        self.assertEqual(result, 0)
        self.assertTrue(observed)
        self.assertTrue(self.target.is_dir())

    def test_absolute_and_relative_targets_build_and_consume_the_same_artifact(self) -> None:
        selections = (str(self.root / "fleet" / "flux"), "relative-cache/flux")
        for selection in selections:
            with self.subTest(selection=selection):
                environment = self.env.copy()
                environment["CARGO_TARGET_DIR"] = selection
                expected = (
                    Path(selection)
                    if Path(selection).is_absolute()
                    else self.root / selection
                ).resolve()
                producer = (
                    "import os, pathlib; "
                    "target=pathlib.Path(os.environ['CARGO_TARGET_DIR']); "
                    "target.mkdir(parents=True, exist_ok=True); "
                    "(target/'sentinel').write_text(str(target))"
                )
                built = subprocess.run(
                    self.command("shared", sys.executable, "-c", producer),
                    env=environment,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(built.returncode, 0, built.stderr)
                resolved = subprocess.run(
                    [
                        sys.executable,
                        str(WRAPPER),
                        "resolve",
                        "--workspace-root",
                        str(self.root),
                    ],
                    env=environment,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(resolved.returncode, 0, resolved.stderr)
                consumed = Path(resolved.stdout.strip())
                self.assertEqual(consumed, expected)
                self.assertEqual((consumed / "sentinel").read_text(), str(expected))

    def test_shared_owner_makes_exclusive_cleanup_refuse_without_a_race(self) -> None:
        child = (
            "import sys; print('ready', flush=True); "
            "sys.stdin.buffer.read(1); print('done', flush=True)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "ready")

        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 75, cleanup.stderr)
        self.assertIn(f"target={self.target}", cleanup.stderr)
        self.assertIn("retry after active builds finish", cleanup.stderr)

        assert owner.stdin is not None
        owner.stdin.write("x")
        owner.stdin.flush()
        self.assert_owner_exit(owner, 0)

        after = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(after.returncode, 0, after.stderr)

    def test_physical_target_aliases_share_one_lock_identity(self) -> None:
        module = load_wrapper()
        real_target = self.root / "real-target"
        real_target.mkdir()
        alias_target = self.root / "alias-target"
        self.make_directory_alias(alias_target, real_target)
        self.assertEqual(module.lock_path_for(alias_target), module.lock_path_for(real_target))

        for selected in (alias_target, self.root / "alias-parent" / "target"):
            if selected.parent.name == "alias-parent":
                real_parent = self.root / "real-parent"
                (real_parent / "target").mkdir(parents=True)
                self.make_directory_alias(selected.parent, real_parent)
                physical = real_parent / "target"
                self.assertEqual(module.lock_path_for(selected), module.lock_path_for(physical))
            else:
                physical = real_target
            environment = self.env.copy()
            environment["CARGO_TARGET_DIR"] = str(selected)
            child = "import sys; print('alias-owner-ready', flush=True); sys.stdin.buffer.read(1)"
            owner = subprocess.Popen(
                self.command("shared", sys.executable, "-c", child),
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            assert owner.stdout is not None
            self.assertEqual(owner.stdout.readline().strip(), "alias-owner-ready")
            contender_env = self.env.copy()
            contender_env["CARGO_TARGET_DIR"] = str(physical)
            contender = subprocess.run(
                self.command(
                    "exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True
                ),
                env=contender_env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(contender.returncode, 75, contender.stderr)
            assert owner.stdin is not None
            owner.stdin.write("x")
            owner.stdin.flush()
            self.assert_owner_exit(owner, 0)

    def test_abnormal_parent_exit_keeps_ownership_until_live_descendant_exits(self) -> None:
        grandchild = "import sys; print('descendant-ready', flush=True); sys.stdin.buffer.read(1)"
        child = (
            "import subprocess, sys; "
            f"subprocess.Popen([sys.executable, '-c', {grandchild!r}], close_fds=False); "
            "raise SystemExit(23)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "descendant-ready")
        try:
            cleanup = subprocess.run(
                self.command(
                    "exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True
                ),
                env=self.env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(cleanup.returncode, 75, cleanup.stderr)
        finally:
            assert owner.stdin is not None
            owner.stdin.write("x")
            owner.stdin.flush()
        self.assert_owner_exit(owner, 23)
        after = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(after.returncode, 0, after.stderr)

    @unittest.skipIf(os.name == "nt", "Windows wrapper termination is proved separately")
    def test_wrapper_sigkill_leaves_inherited_lease_until_grandchild_exits(self) -> None:
        grandchild = (
            "import sys; print('hard-death-descendant-ready', flush=True); "
            "sys.stdin.buffer.read(1); print('hard-death-descendant-done', flush=True)"
        )
        child = (
            "import subprocess, sys; "
            f"subprocess.Popen([sys.executable, '-c', {grandchild!r}], close_fds=False); "
            "raise SystemExit(0)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "hard-death-descendant-ready")
        owner.kill()
        self.assertEqual(owner.wait(timeout=10), -signal.SIGKILL)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 75, cleanup.stderr)
        assert owner.stdin is not None
        owner.stdin.write("x")
        owner.stdin.close()
        self.assertIn("hard-death-descendant-done", owner.stdout.read())
        after = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(after.returncode, 0, after.stderr)
        if owner.stderr is not None:
            owner.stderr.close()
        owner.stdout.close()

    @unittest.skipUnless(os.name == "nt", "native Windows Job teardown ordering")
    def test_windows_wrapper_termination_drains_job_before_exclusive_command(self) -> None:
        held = self.target / "job-held"
        moved = self.target / "job-held-after-cleanup"
        environment = self.env.copy()
        environment["FLUX_JOB_HELD"] = str(held)
        grandchild = (
            "import ctypes, os, sys; from ctypes import wintypes; "
            "os.makedirs(os.environ['CARGO_TARGET_DIR'], exist_ok=True); "
            "create=ctypes.windll.kernel32.CreateFileW; "
            "create.argtypes=[wintypes.LPCWSTR,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p,"
            "wintypes.DWORD,wintypes.DWORD,wintypes.HANDLE]; create.restype=wintypes.HANDLE; "
            "h=create(os.environ['FLUX_JOB_HELD'],0x40000000,0,None,2,0x80,None); "
            "invalid=ctypes.c_void_p(-1).value; "
            "sys.exit(93) if h == invalid else None; "
            "print(f'windows-job-descendant-ready {os.getpid()}', flush=True); "
            "sys.stdin.buffer.read(1)"
        )
        child = (
            "import subprocess, sys; "
            f"subprocess.Popen([sys.executable, '-c', {grandchild!r}], close_fds=False); "
            "raise SystemExit(0)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=environment,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert owner.stdout is not None
        ready = owner.stdout.readline().strip().split()
        self.assertEqual(ready[:1], ["windows-job-descendant-ready"])
        descendant_pid = int(ready[1])

        environment["FLUX_JOB_DESCENDANT_PID"] = str(descendant_pid)
        environment["FLUX_JOB_MOVED"] = str(moved)
        cleanup_probe = (
            "import ctypes, os; from ctypes import wintypes; "
            "open_process=ctypes.windll.kernel32.OpenProcess; "
            "open_process.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD]; "
            "open_process.restype=wintypes.HANDLE; "
            "wait=ctypes.windll.kernel32.WaitForSingleObject; "
            "wait.argtypes=[wintypes.HANDLE,wintypes.DWORD]; wait.restype=wintypes.DWORD; "
            "close=ctypes.windll.kernel32.CloseHandle; "
            "close.argtypes=[wintypes.HANDLE]; close.restype=wintypes.BOOL; "
            "pid=int(os.environ['FLUX_JOB_DESCENDANT_PID']); h=open_process(0x00100000,False,pid); "
            "live=bool(h) and wait(h,0) != 0; close(h) if h else None; "
            "raise SystemExit(94) if live else None; "
            "os.replace(os.environ['FLUX_JOB_HELD'], os.environ['FLUX_JOB_MOVED'])"
        )
        contender = subprocess.Popen(
            self.command("exclusive", sys.executable, "-c", cleanup_probe),
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert contender.stderr is not None
        waiting = contender.stderr.readline().strip()
        self.assertIn("build ownership waiting", waiting)

        owner.kill()
        owner.wait(timeout=10)
        contender_status = contender.wait(timeout=10)
        diagnostics = waiting + "\n" + contender.stderr.read()
        self.assertEqual(contender_status, 0, diagnostics)
        self.assertTrue(moved.is_file())
        if owner.stdin is not None:
            owner.stdin.close()
        owner.stdout.close()
        if owner.stderr is not None:
            owner.stderr.close()
        if contender.stdout is not None:
            contender.stdout.close()
        contender.stderr.close()

    def test_signal_between_spawn_and_owner_publication_is_deferred_to_the_tree(self) -> None:
        module = load_wrapper()
        child = "import sys; sys.stdin.buffer.read(1)"

        def inject_pending_signal(_child) -> None:
            module._forward_signal(signal.SIGTERM, None)

        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.target)}), mock.patch.object(
            module, "_SPAWN_PUBLISH_HOOK", inject_pending_signal
        ):
            result = module.run("shared", self.root, [sys.executable, "-c", child], False)
        self.assertEqual(result, 128 + signal.SIGTERM)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 0, cleanup.stderr)

    def test_signalled_parent_cannot_release_while_descendant_remains_live(self) -> None:
        forwarded = signal.SIGBREAK if os.name == "nt" else signal.SIGTERM
        grandchild = (
            "import signal, sys; "
            f"signal.signal({int(forwarded)}, signal.SIG_IGN); "
            "print('signal-descendant-ready', flush=True); sys.stdin.buffer.read(1)"
        )
        child = (
            "import signal, subprocess, sys; "
            f"signal.signal({int(forwarded)}, lambda *_: sys.exit(29)); "
            f"subprocess.Popen([sys.executable, '-c', {grandchild!r}], close_fds=False); "
            "sys.stdin.buffer.read(1)"
        )
        popen_args = {}
        if os.name == "nt":
            popen_args["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            **popen_args,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "signal-descendant-ready")
        owner.send_signal(signal.CTRL_BREAK_EVENT if os.name == "nt" else signal.SIGTERM)
        try:
            cleanup = subprocess.run(
                self.command(
                    "exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True
                ),
                env=self.env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(cleanup.returncode, 75, cleanup.stderr)
        finally:
            assert owner.stdin is not None
            owner.stdin.write("x")
            owner.stdin.flush()
        self.assert_owner_exit(owner, 29)
        after = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(after.returncode, 0, after.stderr)

    @unittest.skipUnless(os.name == "nt", "real Windows handle lifecycle")
    def test_windows_staged_spawn_failures_terminate_and_close_every_handle(self) -> None:
        module = load_wrapper()
        original_close = module._close_handle
        original_terminate = module._terminate_process
        original_terminate_job = module._terminate_job
        expected = {
            "job": (0, 0, 1),
            "port": (0, 0, 2),
            "process": (1, 0, 7),
            "assigned": (0, 1, 7),
            "resumed": (0, 1, 7),
        }
        for failed_stage, (process_terminations, job_terminations, close_count) in expected.items():
            with self.subTest(stage=failed_stage):
                closed = []
                terminated = []
                terminated_jobs = []

                def record_close(handle):
                    closed.append(handle)
                    return original_close(handle)

                def record_terminate(handle, status):
                    terminated.append((handle, status))
                    return original_terminate(handle, status)

                def record_terminate_job(handle, status):
                    terminated_jobs.append((handle, status))
                    return original_terminate_job(handle, status)

                def fail_at_stage(stage):
                    if stage == failed_stage:
                        raise RuntimeError(f"injected Windows spawn failure at {stage}")

                with mock.patch.object(module, "_close_handle", side_effect=record_close), mock.patch.object(
                    module, "_terminate_process", side_effect=record_terminate
                ), mock.patch.object(
                    module, "_terminate_job", side_effect=record_terminate_job
                ), mock.patch.object(module, "_WINDOWS_STAGE_HOOK", fail_at_stage):
                    lock_file = module._open_lock(module.lock_path_for(self.target))
                    try:
                        with self.assertRaisesRegex(RuntimeError, failed_stage):
                            module._spawn_child_tree(
                                [sys.executable, "-c", "import sys; sys.stdin.buffer.read(1)"],
                                self.root,
                                self.env,
                                lock_file,
                            )
                    finally:
                        lock_file.close()
                self.assertEqual(len(terminated), process_terminations)
                self.assertEqual(len(terminated_jobs), job_terminations)
                self.assertEqual(len(closed), close_count)

    @unittest.skipUnless(os.name == "nt", "real Windows inherited-handle allowlist")
    def test_windows_child_does_not_inherit_an_unrelated_inheritable_handle(self) -> None:
        import msvcrt

        read_fd, write_fd = os.pipe()
        try:
            os.set_inheritable(write_fd, True)
            canary = msvcrt.get_osfhandle(write_fd)
            environment = self.env.copy()
            environment["FLUX_HANDLE_CANARY"] = str(canary)
            child = (
                "import ctypes, os; from ctypes import wintypes; "
                "flags=wintypes.DWORD(); h=wintypes.HANDLE(int(os.environ['FLUX_HANDLE_CANARY'])); "
                "raise SystemExit(91 if ctypes.windll.kernel32.GetHandleInformation(h, ctypes.byref(flags)) else 0)"
            )
            result = subprocess.run(
                self.command("shared", sys.executable, "-c", child),
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            os.close(read_fd)
            os.close(write_fd)
    @unittest.skipIf(os.name == "nt", "Windows cancellation uses SIGBREAK below")
    def test_cancellation_is_forwarded_before_ownership_is_released(self) -> None:
        child = (
            "import signal, sys; "
            "signal.signal(signal.SIGTERM, lambda *_: sys.exit(29)); "
            "print('ready', flush=True); sys.stdin.buffer.read(1)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "ready")
        owner.send_signal(signal.SIGTERM)
        self.assert_owner_exit(owner, 29, close_stdin=False)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 0, cleanup.stderr)

    @unittest.skipUnless(os.name == "nt", "Windows CTRL_BREAK forwarding")
    def test_windows_cancellation_is_forwarded_before_release(self) -> None:
        child = (
            "import signal, sys; "
            "signal.signal(signal.SIGBREAK, lambda *_: sys.exit(29)); "
            "print('ready', flush=True); sys.stdin.buffer.read(1)"
        )
        owner = subprocess.Popen(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
        )
        assert owner.stdout is not None
        self.assertEqual(owner.stdout.readline().strip(), "ready")
        owner.send_signal(signal.CTRL_BREAK_EVENT)
        self.assert_owner_exit(owner, 29, close_stdin=False)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 0, cleanup.stderr)

    def test_abnormal_child_exit_releases_ownership_and_preserves_status(self) -> None:
        failed = subprocess.run(
            self.command("shared", sys.executable, "-c", "raise SystemExit(23)"),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(failed.returncode, 23, failed.stderr)
        self.assertIn("build command failed", failed.stderr)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 0, cleanup.stderr)

    @unittest.skipIf(os.name == "nt", "POSIX signal number assertion")
    def test_signal_child_exit_releases_ownership(self) -> None:
        child = "import os, signal; os.kill(os.getpid(), signal.SIGTERM)"
        failed = subprocess.run(
            self.command("shared", sys.executable, "-c", child),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(failed.returncode, 128 + signal.SIGTERM, failed.stderr)
        cleanup = subprocess.run(
            self.command("exclusive", sys.executable, "-c", "raise SystemExit(0)", refuse=True),
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(cleanup.returncode, 0, cleanup.stderr)


class CleanupSafetyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name).resolve()
        self.root = self.base / "primary"
        self.other = self.base / "other"
        self.cargo_home = self.base / "cargo-state" / "home"
        self.cargo_home.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "config", "user.name", "C-517"], check=True)
        subprocess.run(
            ["git", "-C", str(self.root), "config", "user.email", "c517@example.invalid"],
            check=True,
        )
        tracked = self.root / "crates" / "fixture"
        tracked.mkdir(parents=True)
        (tracked / "tracked.txt").write_text("tracked\n")
        (self.root / ".gitignore").write_text("/ignored-target/\n")
        subprocess.run(["git", "-C", str(self.root), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "-qm", "seed"], check=True)
        subprocess.run(
            ["git", "-C", str(self.root), "worktree", "add", "-q", "--detach", str(self.other)],
            check=True,
        )
        ignored = self.root / "ignored-target"
        ignored.mkdir()
        (ignored / "build-output").write_text("disposable\n")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def make_directory_alias(self, alias: Path, target: Path) -> None:
        if os.name == "nt":
            result = subprocess.run(
                [
                    os.environ.get("COMSPEC", "cmd.exe"),
                    "/d",
                    "/c",
                    "mklink",
                    "/J",
                    str(alias),
                    str(target),
                ],
                text=True,
                capture_output=True,
                check=False,
            )
            if result.returncode != 0:
                self.skipTest(f"cannot create a Windows junction: {result.stderr}")
        else:
            alias.symlink_to(target, target_is_directory=True)

    def cleanup(self, selected: str) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["CARGO_HOME"] = str(self.cargo_home)
        environment["CARGO_TARGET_DIR"] = selected
        return subprocess.run(
            [
                sys.executable,
                str(WRAPPER),
                "exclusive",
                "--refuse",
                "--workspace-root",
                str(self.root),
                "--",
                sys.executable,
                "-c",
                "raise SystemExit(0)",
            ],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def assert_refused(self, selected: str, diagnostic: str) -> None:
        result = self.cleanup(selected)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn(diagnostic, result.stderr)

    def test_cleanup_refuses_workspace_ancestors_and_registered_worktree_roots(self) -> None:
        for selected in ("..", str(self.root), str(self.other)):
            with self.subTest(selected=selected):
                self.assert_refused(selected, "build cleanup refused")

    def test_cleanup_refuses_tracked_subtrees_in_every_worktree(self) -> None:
        for selected in (self.root / "crates", self.other / "crates"):
            with self.subTest(selected=selected):
                self.assert_refused(str(selected), "tracked checkout/worktree content")

    def test_cleanup_refuses_every_cargo_home_overlap(self) -> None:
        for selected in (
            self.cargo_home,
            self.cargo_home.parent,
            self.cargo_home / "nested-target",
        ):
            with self.subTest(selected=selected):
                self.assert_refused(str(selected), "overlaps Cargo home")

    def test_cleanup_allows_existing_ignored_and_new_relative_targets(self) -> None:
        for selected in ("ignored-target", "new-relative-target"):
            with self.subTest(selected=selected):
                result = self.cleanup(selected)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_cleanup_refuses_physical_aliases_of_other_worktree_content(self) -> None:
        checkout_alias = self.base / "other-alias"
        self.make_directory_alias(checkout_alias, self.other)
        self.assert_refused(str(checkout_alias), "registered checkout/worktree")

        tracked_alias = self.base / "other-crates-alias"
        self.make_directory_alias(tracked_alias, self.other / "crates")
        self.assert_refused(str(tracked_alias), "tracked checkout/worktree content")


class TaskInstallBootstrapTests(unittest.TestCase):
    def test_fresh_task_install_acquires_before_first_target_touch(self) -> None:
        task = shutil.which("task")
        if task is None:
            self.skipTest("task is not installed")
        with tempfile.TemporaryDirectory() as raw:
            owned = Path(raw).resolve()
            fake_bin = owned / "bin"
            fake_bin.mkdir()
            fake_cargo = owned / "fake_cargo.py"
            count = owned / "calls"
            log = owned / "cargo.log"
            target = owned / "cargo-target"
            install_root = owned / "install"
            cargo_home = owned / "cargo-home"
            home = owned / "home"
            sentinel = owned / "operator-bin"
            sentinel.mkdir()
            sentinel_file = sentinel / "flux"
            sentinel_file.write_bytes(b"operator-owned\n")
            fake_cargo.write_text(
                """import importlib.util, os, pathlib, sys
wrapper = pathlib.Path(os.environ['FLUX_OWNERSHIP_WRAPPER'])
spec = importlib.util.spec_from_file_location('ownership_for_fake_cargo', wrapper)
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
target = pathlib.Path(os.environ['CARGO_TARGET_DIR'])
count_path = pathlib.Path(os.environ['FAKE_CARGO_COUNT'])
count = int(count_path.read_text()) if count_path.exists() else 0
if count == 0 and target.exists():
    print('governed target existed before the first Cargo child', file=sys.stderr); raise SystemExit(91)
lock_file = module._open_lock(module.lock_path_for(target))
try:
    module._acquire(lock_file, True, False)
except OSError as error:
    if not module._would_block(error): raise
else:
    module._release(lock_file); print('Cargo child started without shared ownership', file=sys.stderr); raise SystemExit(92)
finally:
    lock_file.close()
if sys.argv[1:2] != ['fetch']:
    target.mkdir(parents=True, exist_ok=True)
count_path.write_text(str(count + 1))
with pathlib.Path(os.environ['FAKE_CARGO_LOG']).open('a') as stream: stream.write(' '.join(sys.argv[1:]) + '\\n')
if sys.argv[1:2] == ['install']:
    binary = 'flux-lsp' if any(arg.endswith('flux-lsp') for arg in sys.argv) else 'flux'
    if os.name == 'nt': binary += '.exe'
    destination = pathlib.Path(os.environ['CARGO_INSTALL_ROOT']) / 'bin' / binary
    destination.parent.mkdir(parents=True, exist_ok=True); destination.write_bytes(binary.encode())
""",
                encoding="utf-8",
            )
            unix_launcher = fake_bin / "cargo"
            unix_launcher.write_text(
                '#!/bin/sh\nexec "$FAKE_CARGO_PYTHON" "$FAKE_CARGO_SCRIPT" "$@"\n',
                encoding="utf-8",
            )
            unix_launcher.chmod(0o755)
            (fake_bin / "cargo.bat").write_text(
                '@"%FAKE_CARGO_PYTHON%" "%FAKE_CARGO_SCRIPT%" %*\r\n', encoding="utf-8"
            )

            environment = os.environ.copy()
            environment.pop("PYTHON", None)
            environment.pop("FLUX_PYTHON", None)
            environment.update(
                {
                    "PATH": os.pathsep.join((str(fake_bin), environment["PATH"])),
                    "HOME": str(home),
                    "USERPROFILE": str(home),
                    "CARGO_HOME": str(cargo_home),
                    "CARGO_INSTALL_ROOT": str(install_root),
                    "CARGO_TARGET_DIR": str(target),
                    "FLUX_OWNERSHIP_WRAPPER": str(WRAPPER),
                    "FAKE_CARGO_COUNT": str(count),
                    "FAKE_CARGO_LOG": str(log),
                    "FAKE_CARGO_SCRIPT": str(fake_cargo),
                    "FAKE_CARGO_PYTHON": sys.executable,
                }
            )
            result = subprocess.run(
                [task, "install"],
                cwd=ROOT,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(
                log.read_text(encoding="utf-8").splitlines(),
                [
                    "fetch --locked",
                    "test --workspace --lib",
                    "install --path crates/flux-cli --force",
                    "install --path crates/flux-lsp --force",
                ],
            )
            suffix = ".exe" if os.name == "nt" else ""
            self.assertTrue((install_root / "bin" / f"flux{suffix}").is_file())
            self.assertTrue((install_root / "bin" / f"flux-lsp{suffix}").is_file())
            self.assertEqual(sentinel_file.read_bytes(), b"operator-owned\n")

    def test_plugins_install_builds_and_consumes_the_resolved_target(self) -> None:
        task = shutil.which("task")
        if task is None:
            self.skipTest("task is not installed")
        with tempfile.TemporaryDirectory() as raw:
            owned = Path(raw).resolve()
            fake_bin = owned / "bin"
            fake_bin.mkdir()
            fake_tool = owned / "fake_tool.py"
            fake_tool.write_text(
                """import os, pathlib, sys
tool, args = sys.argv[1], sys.argv[2:]
target = pathlib.Path(os.environ['CARGO_TARGET_DIR']).resolve()
expected = pathlib.Path(os.environ['FAKE_EXPECTED_TARGET']).resolve()
if target != expected:
    print(f'target mismatch: {target} != {expected}', file=sys.stderr); raise SystemExit(95)
release = expected / 'release'
sentinel = release / 'flux-plugin-fake'
log = pathlib.Path(os.environ['FAKE_TOOL_LOG'])
if tool == 'cargo':
    if args != ['build', '--workspace', '--release']:
        print(f'unexpected cargo args: {args!r}', file=sys.stderr); raise SystemExit(96)
    release.mkdir(parents=True, exist_ok=True); sentinel.write_text('built')
    with log.open('a') as stream: stream.write('builder\\n')
elif tool == 'flux' and args[:2] == ['plugin', 'install']:
    if len(args) != 3 or pathlib.Path(args[2]).resolve() != release or not sentinel.is_file():
        print(f'wrong plugin consume path: {args!r}', file=sys.stderr); raise SystemExit(97)
    if pathlib.Path(args[2]).resolve() == pathlib.Path(os.environ['FAKE_AMBIENT_TARGET']).resolve():
        print('ambient plugins/target was consumed', file=sys.stderr); raise SystemExit(98)
    with log.open('a') as stream: stream.write(f'flux install {args[2]}\\n')
elif tool == 'flux' and args == ['plugin', 'ls']:
    with log.open('a') as stream: stream.write('flux ls\\n')
else:
    print(f'unexpected tool invocation: {tool} {args!r}', file=sys.stderr); raise SystemExit(99)
""",
                encoding="utf-8",
            )
            for tool in ("cargo", "flux"):
                unix_launcher = fake_bin / tool
                unix_launcher.write_text(
                    f'#!/bin/sh\nexec "$FAKE_TOOL_PYTHON" "$FAKE_TOOL_SCRIPT" {tool} "$@"\n',
                    encoding="utf-8",
                )
                unix_launcher.chmod(0o755)
                (fake_bin / f"{tool}.bat").write_text(
                    f'@"%FAKE_TOOL_PYTHON%" "%FAKE_TOOL_SCRIPT%" {tool} %*\r\n',
                    encoding="utf-8",
                )

            absolute = owned / "absolute plugin target with spaces"
            relative_physical = owned / "relative plugin target with spaces"
            relative = os.path.relpath(relative_physical, ROOT / "plugins")
            for selection, expected in (
                (str(absolute), absolute),
                (relative, relative_physical),
            ):
                with self.subTest(selection=selection):
                    log = owned / f"tool-{expected.name}.log"
                    environment = os.environ.copy()
                    environment.pop("PYTHON", None)
                    environment.pop("FLUX_PYTHON", None)
                    environment.update(
                        {
                            "PATH": os.pathsep.join((str(fake_bin), environment["PATH"])),
                            "CARGO_HOME": str(owned / "cargo-home"),
                            "CARGO_TARGET_DIR": selection,
                            "FAKE_AMBIENT_TARGET": str(ROOT / "plugins" / "target" / "release"),
                            "FAKE_EXPECTED_TARGET": str(expected),
                            "FAKE_TOOL_LOG": str(log),
                            "FAKE_TOOL_PYTHON": sys.executable,
                            "FAKE_TOOL_SCRIPT": str(fake_tool),
                        }
                    )
                    result = subprocess.run(
                        [task, "plugins:install"],
                        cwd=ROOT,
                        env=environment,
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    lines = log.read_text(encoding="utf-8").splitlines()
                    self.assertEqual(lines[0], "builder")
                    self.assertEqual(lines[1], f"flux install {expected / 'release'}")
                    self.assertEqual(lines[2], "flux ls")


if __name__ == "__main__":
    unittest.main()
