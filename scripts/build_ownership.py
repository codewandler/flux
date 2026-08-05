#!/usr/bin/env python3
"""Run a command while owning the resolved Flux Cargo target.

Builders use a shared advisory lock. Repository cleanup uses the exclusive form of the same lock.
The lock file is a persistent sibling of the target so cleaning the target cannot replace the inode
that live owners locked.
"""

from __future__ import annotations

import argparse
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
from typing import BinaryIO, Callable, Protocol, Sequence


OWNERSHIP_REFUSED = 75

class _OwnedChildTree(Protocol):
    def wait_tree(self) -> int: ...

    def send_signal(self, signum: int) -> None: ...

    def close(self) -> None: ...


_ACTIVE_CHILD: _OwnedChildTree | None = None
_SPAWNING_CHILD = False
_PENDING_SIGNALS: list[int] = []
_SPAWN_PUBLISH_HOOK: Callable[[_OwnedChildTree], None] | None = None


def resolve_target(workspace_root: Path, override: str | None) -> Path:
    """Resolve CARGO_TARGET_DIR once using the child Cargo process's workspace cwd."""
    root = Path(os.path.abspath(os.path.normpath(workspace_root)))
    if override is None:
        return root / "target"
    if not override:
        raise ValueError("CARGO_TARGET_DIR is set but empty")
    selected = Path(override)
    if not selected.is_absolute():
        selected = root / selected
    return Path(os.path.abspath(os.path.normpath(selected)))


def lock_path_for(target: Path) -> Path:
    canonical = target.resolve(strict=False)
    if canonical.parent == canonical:
        raise ValueError("the filesystem root cannot be a Cargo target")
    return canonical.with_name(f"{canonical.name}.flux-build.lock")


def _contains_path(parent: Path, child: Path) -> bool:
    parent = parent.resolve(strict=False)
    child = child.resolve(strict=False)
    return child == parent or parent in child.parents


def _contains_directory_entry(parent: Path, child: Path) -> bool:
    parent = parent.resolve(strict=False)
    child = child.parent.resolve(strict=False) / child.name
    return child == parent or parent in child.parents


def _registered_worktrees(root: Path) -> list[Path]:
    result = subprocess.run(
        ["git", "-C", str(root), "worktree", "list", "--porcelain"],
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return []
    return [
        Path(line.removeprefix("worktree "))
        for line in result.stdout.splitlines()
        if line.startswith("worktree ")
    ]


def _paths_overlap(left: Path, right: Path) -> bool:
    return _contains_path(left, right) or _contains_path(right, left)


def _cargo_home(root: Path) -> Path:
    selected = os.environ.get("CARGO_HOME")
    if selected:
        path = Path(selected)
        if not path.is_absolute():
            path = root / path
        return path.resolve(strict=False)
    return (Path.home() / ".cargo").resolve(strict=False)


def _contains_tracked_content(target: Path, checkout: Path) -> bool:
    result = subprocess.run(
        ["git", "-C", str(checkout), "ls-files", "-z"],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return False
    for encoded in result.stdout.split(b"\0"):
        tracked = checkout / Path(os.fsdecode(encoded))
        if encoded and (
            _contains_path(target, tracked) or _contains_directory_entry(target, tracked)
        ):
            return True
    return False


def cleanup_refusal(root: Path, target: Path) -> str | None:
    if _paths_overlap(target, _cargo_home(root)):
        return "the selected target overlaps Cargo home"
    if _contains_path(target, root):
        return "the selected target is the workspace or one of its ancestors"
    for checkout in _registered_worktrees(root):
        if _contains_path(target, checkout):
            return "the selected target contains a registered checkout/worktree"
        if target.exists() and _contains_path(checkout, target) and _contains_tracked_content(
            target, checkout
        ):
            return "the selected target contains tracked checkout/worktree content"
    return None


def _open_lock(path: Path) -> BinaryIO:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    lock_file = os.fdopen(descriptor, "r+b", buffering=0)
    if os.fstat(descriptor).st_size == 0:
        lock_file.write(b"\0")
        lock_file.seek(0)
    return lock_file


if os.name == "nt":
    import ctypes
    from ctypes import wintypes
    import msvcrt

    _LOCKFILE_FAIL_IMMEDIATELY = 0x00000001
    _LOCKFILE_EXCLUSIVE_LOCK = 0x00000002
    _ERROR_LOCK_VIOLATION = 33
    _INFINITE = 0xFFFFFFFF
    _CREATE_SUSPENDED = 0x00000004
    _CREATE_NEW_PROCESS_GROUP = 0x00000200
    _CREATE_UNICODE_ENVIRONMENT = 0x00000400
    _EXTENDED_STARTUPINFO_PRESENT = 0x00080000
    _STARTF_USESTDHANDLES = 0x00000100
    _STD_INPUT_HANDLE = -10
    _STD_OUTPUT_HANDLE = -11
    _STD_ERROR_HANDLE = -12
    _JOB_OBJECT_ASSOCIATE_COMPLETION_PORT_INFORMATION = 7
    _JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9
    _JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000
    _JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO = 4
    _CTRL_BREAK_EVENT = 1
    _DUPLICATE_SAME_ACCESS = 0x00000002
    _PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002
    _WAIT_OBJECT_0 = 0
    _WINDOWS_STAGE_HOOK: Callable[[str], None] | None = None

    class _Overlapped(ctypes.Structure):
        _fields_ = [
            ("Internal", ctypes.c_void_p),
            ("InternalHigh", ctypes.c_void_p),
            ("Offset", wintypes.DWORD),
            ("OffsetHigh", wintypes.DWORD),
            ("hEvent", wintypes.HANDLE),
        ]

    class _StartupInfo(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("lpReserved", wintypes.LPWSTR),
            ("lpDesktop", wintypes.LPWSTR),
            ("lpTitle", wintypes.LPWSTR),
            ("dwX", wintypes.DWORD),
            ("dwY", wintypes.DWORD),
            ("dwXSize", wintypes.DWORD),
            ("dwYSize", wintypes.DWORD),
            ("dwXCountChars", wintypes.DWORD),
            ("dwYCountChars", wintypes.DWORD),
            ("dwFillAttribute", wintypes.DWORD),
            ("dwFlags", wintypes.DWORD),
            ("wShowWindow", wintypes.WORD),
            ("cbReserved2", wintypes.WORD),
            ("lpReserved2", ctypes.POINTER(wintypes.BYTE)),
            ("hStdInput", wintypes.HANDLE),
            ("hStdOutput", wintypes.HANDLE),
            ("hStdError", wintypes.HANDLE),
        ]

    class _ProcessInformation(ctypes.Structure):
        _fields_ = [
            ("hProcess", wintypes.HANDLE),
            ("hThread", wintypes.HANDLE),
            ("dwProcessId", wintypes.DWORD),
            ("dwThreadId", wintypes.DWORD),
        ]

    class _StartupInfoEx(ctypes.Structure):
        _fields_ = [("StartupInfo", _StartupInfo), ("lpAttributeList", ctypes.c_void_p)]

    class _JobAssociateCompletionPort(ctypes.Structure):
        _fields_ = [("CompletionKey", ctypes.c_void_p), ("CompletionPort", wintypes.HANDLE)]

    class _JobBasicLimitInformation(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong),
            ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class _IoCounters(ctypes.Structure):
        _fields_ = [
            ("ReadOperationCount", ctypes.c_ulonglong),
            ("WriteOperationCount", ctypes.c_ulonglong),
            ("OtherOperationCount", ctypes.c_ulonglong),
            ("ReadTransferCount", ctypes.c_ulonglong),
            ("WriteTransferCount", ctypes.c_ulonglong),
            ("OtherTransferCount", ctypes.c_ulonglong),
        ]

    class _JobExtendedLimitInformation(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", _JobBasicLimitInformation),
            ("IoInfo", _IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _lock_file_ex = _kernel32.LockFileEx
    _lock_file_ex.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.POINTER(_Overlapped),
    ]
    _lock_file_ex.restype = wintypes.BOOL
    _unlock_file_ex = _kernel32.UnlockFileEx
    _unlock_file_ex.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.POINTER(_Overlapped),
    ]
    _unlock_file_ex.restype = wintypes.BOOL
    _create_job_object = _kernel32.CreateJobObjectW
    _create_job_object.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    _create_job_object.restype = wintypes.HANDLE
    _set_job_information = _kernel32.SetInformationJobObject
    _set_job_information.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    _set_job_information.restype = wintypes.BOOL
    _create_completion_port = _kernel32.CreateIoCompletionPort
    _create_completion_port.argtypes = [wintypes.HANDLE, wintypes.HANDLE, ctypes.c_size_t, wintypes.DWORD]
    _create_completion_port.restype = wintypes.HANDLE
    _create_process = _kernel32.CreateProcessW
    _create_process.argtypes = [
        wintypes.LPCWSTR,
        wintypes.LPWSTR,
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.BOOL,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.LPCWSTR,
        ctypes.c_void_p,
        ctypes.POINTER(_ProcessInformation),
    ]
    _create_process.restype = wintypes.BOOL
    _assign_process_to_job = _kernel32.AssignProcessToJobObject
    _assign_process_to_job.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    _assign_process_to_job.restype = wintypes.BOOL
    _resume_thread = _kernel32.ResumeThread
    _resume_thread.argtypes = [wintypes.HANDLE]
    _resume_thread.restype = wintypes.DWORD
    _wait_for_single_object = _kernel32.WaitForSingleObject
    _wait_for_single_object.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    _wait_for_single_object.restype = wintypes.DWORD
    _get_exit_code_process = _kernel32.GetExitCodeProcess
    _get_exit_code_process.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    _get_exit_code_process.restype = wintypes.BOOL
    _get_queued_completion_status = _kernel32.GetQueuedCompletionStatus
    _get_queued_completion_status.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.POINTER(ctypes.c_size_t),
        ctypes.POINTER(ctypes.c_void_p),
        wintypes.DWORD,
    ]
    _get_queued_completion_status.restype = wintypes.BOOL
    _terminate_job = _kernel32.TerminateJobObject
    _terminate_job.argtypes = [wintypes.HANDLE, wintypes.UINT]
    _terminate_job.restype = wintypes.BOOL
    _terminate_process = _kernel32.TerminateProcess
    _terminate_process.argtypes = [wintypes.HANDLE, wintypes.UINT]
    _terminate_process.restype = wintypes.BOOL
    _get_current_process = _kernel32.GetCurrentProcess
    _get_current_process.argtypes = []
    _get_current_process.restype = wintypes.HANDLE
    _duplicate_handle = _kernel32.DuplicateHandle
    _duplicate_handle.argtypes = [
        wintypes.HANDLE,
        wintypes.HANDLE,
        wintypes.HANDLE,
        ctypes.POINTER(wintypes.HANDLE),
        wintypes.DWORD,
        wintypes.BOOL,
        wintypes.DWORD,
    ]
    _duplicate_handle.restype = wintypes.BOOL
    _initialize_attribute_list = _kernel32.InitializeProcThreadAttributeList
    _initialize_attribute_list.argtypes = [ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(ctypes.c_size_t)]
    _initialize_attribute_list.restype = wintypes.BOOL
    _update_attribute = _kernel32.UpdateProcThreadAttribute
    _update_attribute.argtypes = [
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_size_t,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_void_p,
        ctypes.c_void_p,
    ]
    _update_attribute.restype = wintypes.BOOL
    _delete_attribute_list = _kernel32.DeleteProcThreadAttributeList
    _delete_attribute_list.argtypes = [ctypes.c_void_p]
    _delete_attribute_list.restype = None
    _generate_console_event = _kernel32.GenerateConsoleCtrlEvent
    _generate_console_event.argtypes = [wintypes.DWORD, wintypes.DWORD]
    _generate_console_event.restype = wintypes.BOOL
    _get_std_handle = _kernel32.GetStdHandle
    _get_std_handle.argtypes = [wintypes.DWORD]
    _get_std_handle.restype = wintypes.HANDLE
    _close_handle = _kernel32.CloseHandle
    _close_handle.argtypes = [wintypes.HANDLE]
    _close_handle.restype = wintypes.BOOL

    def _windows_handle(lock_file: BinaryIO) -> int:
        return msvcrt.get_osfhandle(lock_file.fileno())

    def _acquire(lock_file: BinaryIO, exclusive: bool, blocking: bool) -> None:
        flags = _LOCKFILE_EXCLUSIVE_LOCK if exclusive else 0
        if not blocking:
            flags |= _LOCKFILE_FAIL_IMMEDIATELY
        overlapped = _Overlapped()
        if not _lock_file_ex(
            _windows_handle(lock_file), flags, 0, 1, 0, ctypes.byref(overlapped)
        ):
            code = ctypes.get_last_error()
            if code == _ERROR_LOCK_VIOLATION:
                raise BlockingIOError(errno.EAGAIN, "build target is owned")
            raise ctypes.WinError(code)

    def _release(lock_file: BinaryIO) -> None:
        overlapped = _Overlapped()
        if not _unlock_file_ex(
            _windows_handle(lock_file), 0, 1, 0, ctypes.byref(overlapped)
        ):
            raise ctypes.WinError(ctypes.get_last_error())

    class _WindowsChildTree:
        def __init__(self, process: wintypes.HANDLE, job: wintypes.HANDLE, port: wintypes.HANDLE, pid: int):
            self.process = process
            self.job = job
            self.port = port
            self.pid = pid

        def wait_tree(self) -> int:
            if _wait_for_single_object(self.process, _INFINITE) != 0:
                raise ctypes.WinError(ctypes.get_last_error())
            status = wintypes.DWORD()
            if not _get_exit_code_process(self.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            while True:
                message = wintypes.DWORD()
                key = ctypes.c_size_t()
                overlapped = ctypes.c_void_p()
                if not _get_queued_completion_status(
                    self.port,
                    ctypes.byref(message),
                    ctypes.byref(key),
                    ctypes.byref(overlapped),
                    _INFINITE,
                ):
                    raise ctypes.WinError(ctypes.get_last_error())
                if message.value == _JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO:
                    return status.value

        def send_signal(self, signum: int) -> None:
            if signum == signal.SIGBREAK:
                leader_status = _wait_for_single_object(self.process, 0)
                if leader_status == _WAIT_OBJECT_0 or not _generate_console_event(
                    _CTRL_BREAK_EVENT, self.pid
                ):
                    if not _terminate_job(self.job, 128 + signum):
                        raise ctypes.WinError(ctypes.get_last_error())
            elif not _terminate_job(self.job, 128 + signum):
                raise ctypes.WinError(ctypes.get_last_error())

        def close(self) -> None:
            _close_handle(self.process)
            _close_handle(self.port)
            _close_handle(self.job)

    def _spawn_child_tree(
        command: Sequence[str], root: Path, environment: dict[str, str], _lock_file: BinaryIO
    ) -> _OwnedChildTree:
        job = _create_job_object(None, None)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        port = wintypes.HANDLE()
        process = wintypes.HANDLE()
        thread = wintypes.HANDLE()
        assigned = False
        inherited_std: list[wintypes.HANDLE] = []
        attribute_buffer = None
        attribute_list = None
        try:
            if _WINDOWS_STAGE_HOOK is not None:
                _WINDOWS_STAGE_HOOK("job")
            limits = _JobExtendedLimitInformation()
            limits.BasicLimitInformation.LimitFlags = _JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            if not _set_job_information(
                job,
                _JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                ctypes.byref(limits),
                ctypes.sizeof(limits),
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            invalid_handle = wintypes.HANDLE(ctypes.c_void_p(-1).value)
            port = _create_completion_port(invalid_handle, None, 0, 1)
            if not port:
                raise ctypes.WinError(ctypes.get_last_error())
            if _WINDOWS_STAGE_HOOK is not None:
                _WINDOWS_STAGE_HOOK("port")
            association = _JobAssociateCompletionPort(ctypes.c_void_p(job), port)
            if not _set_job_information(
                job,
                _JOB_OBJECT_ASSOCIATE_COMPLETION_PORT_INFORMATION,
                ctypes.byref(association),
                ctypes.sizeof(association),
            ):
                raise ctypes.WinError(ctypes.get_last_error())

            current_process = _get_current_process()
            for std_id in (_STD_INPUT_HANDLE, _STD_OUTPUT_HANDLE, _STD_ERROR_HANDLE):
                duplicate = wintypes.HANDLE()
                if not _duplicate_handle(
                    current_process,
                    _get_std_handle(std_id),
                    current_process,
                    ctypes.byref(duplicate),
                    0,
                    True,
                    _DUPLICATE_SAME_ACCESS,
                ):
                    raise ctypes.WinError(ctypes.get_last_error())
                inherited_std.append(duplicate)

            attribute_size = ctypes.c_size_t()
            _initialize_attribute_list(None, 1, 0, ctypes.byref(attribute_size))
            attribute_buffer = ctypes.create_string_buffer(attribute_size.value)
            attribute_list = ctypes.cast(attribute_buffer, ctypes.c_void_p)
            if not _initialize_attribute_list(
                attribute_list, 1, 0, ctypes.byref(attribute_size)
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            handle_array = (wintypes.HANDLE * len(inherited_std))(*inherited_std)
            if not _update_attribute(
                attribute_list,
                0,
                _PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                ctypes.cast(handle_array, ctypes.c_void_p),
                ctypes.sizeof(handle_array),
                None,
                None,
            ):
                raise ctypes.WinError(ctypes.get_last_error())

            startup = _StartupInfoEx()
            startup.StartupInfo.cb = ctypes.sizeof(startup)
            startup.StartupInfo.dwFlags = _STARTF_USESTDHANDLES
            startup.StartupInfo.hStdInput = inherited_std[0]
            startup.StartupInfo.hStdOutput = inherited_std[1]
            startup.StartupInfo.hStdError = inherited_std[2]
            startup.lpAttributeList = attribute_list
            info = _ProcessInformation()
            command_line = ctypes.create_unicode_buffer(subprocess.list2cmdline(command))
            environment_block = ctypes.create_unicode_buffer(
                "\0".join(f"{key}={value}" for key, value in sorted(environment.items(), key=lambda item: item[0].casefold()))
                + "\0\0"
            )
            flags = (
                _CREATE_SUSPENDED
                | _CREATE_NEW_PROCESS_GROUP
                | _CREATE_UNICODE_ENVIRONMENT
                | _EXTENDED_STARTUPINFO_PRESENT
            )
            if not _create_process(
                None,
                command_line,
                None,
                None,
                True,
                flags,
                environment_block,
                str(root),
                ctypes.byref(startup),
                ctypes.byref(info),
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            _delete_attribute_list(attribute_list)
            attribute_list = None
            for handle in inherited_std:
                _close_handle(handle)
            inherited_std = []
            process = info.hProcess
            thread = info.hThread
            if _WINDOWS_STAGE_HOOK is not None:
                _WINDOWS_STAGE_HOOK("process")
            if not _assign_process_to_job(job, process):
                raise ctypes.WinError(ctypes.get_last_error())
            assigned = True
            if _WINDOWS_STAGE_HOOK is not None:
                _WINDOWS_STAGE_HOOK("assigned")
            if _resume_thread(thread) == 0xFFFFFFFF:
                raise ctypes.WinError(ctypes.get_last_error())
            if _WINDOWS_STAGE_HOOK is not None:
                _WINDOWS_STAGE_HOOK("resumed")
            _close_handle(thread)
            thread = wintypes.HANDLE()
            child = _WindowsChildTree(process, job, port, info.dwProcessId)
            if _SPAWN_PUBLISH_HOOK is not None:
                _SPAWN_PUBLISH_HOOK(child)
            return child
        except BaseException:
            if process:
                terminated = _terminate_job(job, 126) if assigned else _terminate_process(process, 126)
                if not terminated:
                    termination_error = ctypes.WinError(ctypes.get_last_error())
                else:
                    termination_error = None
                    _wait_for_single_object(process, _INFINITE)
            if thread:
                _close_handle(thread)
            if process:
                _close_handle(process)
            if port:
                _close_handle(port)
            if attribute_list is not None:
                _delete_attribute_list(attribute_list)
            for handle in inherited_std:
                _close_handle(handle)
            _close_handle(job)
            if process and termination_error is not None:
                raise termination_error
            raise

else:
    import fcntl

    def _acquire(lock_file: BinaryIO, exclusive: bool, blocking: bool) -> None:
        operation = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        if not blocking:
            operation |= fcntl.LOCK_NB
        fcntl.flock(lock_file.fileno(), operation)

    def _release(lock_file: BinaryIO) -> None:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)

    class _UnixChildTree:
        def __init__(self, process: subprocess.Popen[bytes], lifetime_reader: int):
            self.process = process
            self.lifetime_reader = lifetime_reader

        def wait_tree(self) -> int:
            status = self.process.wait()
            while os.read(self.lifetime_reader, 4096):
                pass
            return status

        def send_signal(self, signum: int) -> None:
            os.killpg(self.process.pid, signum)

        def close(self) -> None:
            os.close(self.lifetime_reader)

    def _spawn_child_tree(
        command: Sequence[str], root: Path, environment: dict[str, str], lock_file: BinaryIO
    ) -> _OwnedChildTree:
        lifetime_reader, lifetime_writer = os.pipe()
        try:
            process = subprocess.Popen(
                command,
                cwd=root,
                env=environment,
                start_new_session=True,
                pass_fds=(lifetime_writer, lock_file.fileno()),
            )
        except BaseException:
            os.close(lifetime_reader)
            raise
        finally:
            os.close(lifetime_writer)
        child = _UnixChildTree(process, lifetime_reader)
        if _SPAWN_PUBLISH_HOOK is not None:
            _SPAWN_PUBLISH_HOOK(child)
        return child


def _would_block(error: OSError) -> bool:
    return isinstance(error, BlockingIOError) or error.errno in (errno.EACCES, errno.EAGAIN)


def _forward_signal(signum: int, _frame: object) -> None:
    global _PENDING_SIGNALS
    child = _ACTIVE_CHILD
    if child is None:
        if _SPAWNING_CHILD:
            _PENDING_SIGNALS.append(signum)
            return
        raise SystemExit(128 + signum)
    try:
        child.send_signal(signum)
    except (OSError, ValueError):
        pass


def _spawn_and_publish_child(
    command: Sequence[str], root: Path, environment: dict[str, str], lock_file: BinaryIO
) -> _OwnedChildTree:
    global _ACTIVE_CHILD, _PENDING_SIGNALS, _SPAWNING_CHILD
    _SPAWNING_CHILD = True
    try:
        child = _spawn_child_tree(command, root, environment, lock_file)
        _ACTIVE_CHILD = child
    except BaseException:
        _SPAWNING_CHILD = False
        pending, _PENDING_SIGNALS = _PENDING_SIGNALS, []
        if pending:
            raise SystemExit(128 + pending[0])
        raise
    _SPAWNING_CHILD = False
    pending, _PENDING_SIGNALS = _PENDING_SIGNALS, []
    for signum in pending:
        try:
            child.send_signal(signum)
        except (OSError, ValueError):
            pass
    return child


def _run_child(
    command: Sequence[str], root: Path, target: Path, mode: str, lock_file: BinaryIO
) -> int:
    global _ACTIVE_CHILD
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target)
    try:
        _ACTIVE_CHILD = _spawn_and_publish_child(command, root, environment, lock_file)
    except FileNotFoundError:
        print(
            f"build ownership acquired target={target} mode={mode}; command executable was not found",
            file=sys.stderr,
        )
        return 127
    except OSError as error:
        print(
            f"build ownership acquired target={target} mode={mode}; "
            f"command could not start: {error.strerror}",
            file=sys.stderr,
        )
        return 126
    try:
        result = _ACTIVE_CHILD.wait_tree()
    finally:
        _ACTIVE_CHILD.close()
        _ACTIVE_CHILD = None
    if result < 0:
        return 128 - result
    return result


def run(mode: str, workspace_root: Path, command: Sequence[str], refuse: bool) -> int:
    try:
        root = Path(os.path.abspath(os.path.normpath(workspace_root)))
        target = resolve_target(root, os.environ.get("CARGO_TARGET_DIR"))
        lock_path = lock_path_for(target)
    except ValueError as error:
        print(f"build ownership configuration refused: {error}", file=sys.stderr)
        return 2

    unsafe_cleanup = cleanup_refusal(root, target) if mode == "exclusive" else None
    if unsafe_cleanup is not None:
        print(
            f"build cleanup refused target={target}: {unsafe_cleanup}; "
            "select a dedicated Cargo target directory",
            file=sys.stderr,
        )
        return OWNERSHIP_REFUSED

    try:
        lock_file = _open_lock(lock_path)
    except OSError as error:
        print(
            f"build ownership refused target={target}: cannot open ownership file: {error.strerror}",
            file=sys.stderr,
        )
        return OWNERSHIP_REFUSED

    exclusive = mode == "exclusive"
    acquired = False
    try:
        try:
            _acquire(lock_file, exclusive, blocking=False)
            acquired = True
            print(f"build ownership acquired target={target} mode={mode}", file=sys.stderr)
        except OSError as error:
            if not _would_block(error):
                print(
                    f"build ownership refused target={target}: ownership primitive failed: "
                    f"{error.strerror}",
                    file=sys.stderr,
                )
                return OWNERSHIP_REFUSED
            if refuse:
                print(
                    f"build ownership refused target={target} mode={mode}; "
                    "retry after active builds finish",
                    file=sys.stderr,
                )
                return OWNERSHIP_REFUSED
            print(f"build ownership waiting target={target} mode={mode}", file=sys.stderr)
            try:
                _acquire(lock_file, exclusive, blocking=True)
            except InterruptedError:
                print(
                    f"build ownership refused target={target}: waiting was interrupted; retry",
                    file=sys.stderr,
                )
                return OWNERSHIP_REFUSED
            acquired = True
            print(f"build ownership acquired target={target} mode={mode}", file=sys.stderr)

        result = _run_child(command, root, target, mode, lock_file)
        if result != 0:
            subject = "cleanup" if exclusive else "build"
            print(
                f"{subject} command failed under acquired {mode} ownership "
                f"target={target} exit={result}",
                file=sys.stderr,
            )
        return result
    finally:
        if acquired:
            _release(lock_file)
        lock_file.close()


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="run a command under shared build or exclusive cleanup ownership"
    )
    modes = parser.add_subparsers(dest="mode", required=True)
    resolve = modes.add_parser("resolve", help="print the selected absolute Cargo target")
    resolve.add_argument("--workspace-root", required=True, type=Path)
    resolve.set_defaults(command=[], refuse=False)
    shared = modes.add_parser("shared", help="hold shared build ownership")
    exclusive = modes.add_parser("exclusive", help="hold exclusive cleanup ownership")
    for mode in (shared, exclusive):
        mode.add_argument("--workspace-root", required=True, type=Path)
        mode.add_argument("command", nargs=argparse.REMAINDER)
    exclusive.add_argument(
        "--refuse",
        action="store_true",
        help="refuse immediately instead of waiting when ownership is unavailable",
    )
    shared.set_defaults(refuse=False)
    args = parser.parse_args(argv)
    if args.mode == "resolve":
        return args
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.mode == "resolve":
        try:
            print(resolve_target(args.workspace_root, os.environ.get("CARGO_TARGET_DIR")))
        except ValueError as error:
            print(f"build ownership configuration refused: {error}", file=sys.stderr)
            return 2
        return 0
    forwarded_signals = [signal.SIGINT, signal.SIGTERM]
    if hasattr(signal, "SIGBREAK"):
        forwarded_signals.append(signal.SIGBREAK)
    for signum in forwarded_signals:
        signal.signal(signum, _forward_signal)
    return run(args.mode, args.workspace_root, args.command, args.refuse)


if __name__ == "__main__":
    raise SystemExit(main())
