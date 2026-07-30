#!/bin/sh
# A stand-in flux worker for C-243's `ProcessRuntime` tests.
#
# It speaks the only two things `ProcessRuntime` actually depends on: it is handed the real worker
# argv (`app run --serve=<addr> --yes [-m <model>]`), and it announces itself on stderr exactly as
# `flux_server::serve` does. That lets the worker lifecycle be proven against a **real guarded spawn**
# and a real OS process without booting a full `flux`, resolving a provider, or binding a port a CI
# box may not let us bind twice.
#
# The announcement below is a copy of `flux_core::readiness::serving_announcement` — a shell script
# cannot import it. `the_stand_in_worker_announces_exactly_what_the_real_server_announces` checks the
# copy against the original on every run, because a fixture that agrees only with the matcher and not
# with the server would keep this whole suite green while every real worker timed out (C-277).
#
# Why this is a committed fixture rather than a file the test writes: writing an executable and then
# exec'ing it races with `fork` on other test threads. A forked child inherits the still-open write
# fd, and `execve` answers ETXTBSY ("Text file busy") for as long as any writer exists — so the suite
# passed one test at a time and failed under `cargo test`'s default parallelism. Git creates this file
# once, no test process ever opens it for writing, and the race cannot occur.
#
# Behaviour is selected by marker files in the **cwd**, which is the worker's checkout (or the
# workspace root when the test names no worktree) — so each test steers it by creating files in a
# directory it already owns:
#
#   no-announce   never print the listening line: the shape of a worker that starts but never binds
#   exit-code     exit with the code this file contains, right after announcing: a crashed worker
#
# With no markers it announces and then stays up until it is killed.

addr=""
for arg in "$@"; do
  case "$arg" in
    --serve=*) addr=${arg#--serve=} ;;
  esac
done

if [ ! -f ./no-announce ]; then
  echo "flux server listening on http://$addr" >&2
fi
# Always reported, so a test can assert where the OS actually put the child rather than where the
# runtime intended to put it.
echo "cwd=$(pwd)" >&2

if [ -f ./exit-code ]; then
  exit "$(cat ./exit-code)"
fi

while true; do
  sleep 1
done
