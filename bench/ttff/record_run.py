#!/usr/bin/env python3
"""PTY recorder for time-to-first-feedback (TTFF) measurement — I-03.

Runs a command under a pseudo-terminal (so the flux CLI renders exactly as it
does for a user: spinner on, colors on, stdout+stderr merged in arrival order)
and records every output chunk with a monotonic timestamp relative to spawn.

The output is raw evidence, not a derived metric: one JSONL file per run with a
`meta` header, one `chunk` row per PTY read (base64 payload), and a final
`exit` row. `report.py` derives TTFF from these files, so the metric's
definition can be refined and recomputed later without re-running (runs cost
API credits; recordings are free to re-analyze).

Stdlib only. Usage:
    record_run.py --out chunks.jsonl [--timeout 300] [--cwd DIR] -- CMD ARGS...
"""

import argparse
import base64
import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import sys
import termios
import time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="chunks JSONL output path")
    ap.add_argument("--timeout", type=float, default=300.0, help="kill after N seconds")
    ap.add_argument("--cwd", default=None, help="working directory for the child")
    ap.add_argument("cmd", nargs=argparse.REMAINDER, help="-- CMD ARGS...")
    args = ap.parse_args()

    cmd = args.cmd
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]
    if not cmd:
        ap.error("no command given (pass it after --)")

    out = open(args.out, "w", encoding="utf-8")

    def emit(row: dict) -> None:
        out.write(json.dumps(row) + "\n")
        out.flush()

    pid, master = pty.fork()
    if pid == 0:  # child
        try:
            if args.cwd:
                os.chdir(args.cwd)
            os.execvpe(cmd[0], cmd, os.environ)
        except Exception as e:  # pragma: no cover - child-side failure path
            os.write(2, f"record_run: exec failed: {e}\n".encode())
            os._exit(127)

    # A sane fixed window so both legs render identically.
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

    t0 = time.monotonic()
    emit(
        {
            "type": "meta",
            "cmd": cmd,
            "cwd": args.cwd or os.getcwd(),
            "t0_epoch_ms": int(time.time() * 1000),
            "timeout_s": args.timeout,
        }
    )

    timed_out = False
    while True:
        remaining = args.timeout - (time.monotonic() - t0)
        if remaining <= 0:
            timed_out = True
            break
        try:
            ready, _, _ = select.select([master], [], [], min(remaining, 1.0))
        except InterruptedError:
            continue
        if not ready:
            continue
        try:
            data = os.read(master, 65536)
        except OSError as e:
            if e.errno == errno.EIO:  # child closed its side: normal EOF on Linux PTYs
                break
            raise
        if not data:
            break
        emit(
            {
                "type": "chunk",
                "t_ms": round((time.monotonic() - t0) * 1000, 3),
                "b64": base64.b64encode(data).decode("ascii"),
            }
        )

    if timed_out:
        os.kill(pid, signal.SIGKILL)
    _, status = os.waitpid(pid, 0)
    exit_code = os.waitstatus_to_exitcode(status) if not timed_out else None
    emit(
        {
            "type": "exit",
            "t_ms": round((time.monotonic() - t0) * 1000, 3),
            "exit_code": exit_code,
            "timed_out": timed_out,
        }
    )
    out.close()
    os.close(master)
    return 0


if __name__ == "__main__":
    sys.exit(main())
