# Shell operation

`bash` is a non-interactive escape hatch. Prefer dedicated operations; use argv-safe, non-interactive
commands with no pager or prompt, and do not start watchers.

Before writing files that require an external runtime, verify it with `command -v <tool>`; if it is
missing, stop and report clearly rather than writing files that cannot run. When the deliverable is a
persistent server, start it in the background (for example `nohup node server.js &`) and confirm the
port accepts connections (`curl --retry 5 --retry-connrefused ...` or `ss -tlnp`) before finishing;
never write files and exit silently when the server never started.
