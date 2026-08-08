# Design — Flux as CI: the fleet is the build cluster

## Why

A repository gate is currently a shell string in `.flux/fleet.toml`. The flux gate is six commands
joined by `&&` inside one `bash -lc`, run by whichever process happens to reach that line, with its
output appended to a log file. That has four consequences, each observed:

1. **A gate result is not evidence.** There is no record of what base it ran against, what argv it
   ran, or which toolchain produced it — only an exit code in a log. Handoff verification had to be
   reconstructed by hand from worker transcripts precisely because the gate itself recorded nothing
   citable.
2. **Nothing is cached.** Integrating two waves against an unchanged base runs the same six commands
   twice. On this workspace a full gate is the single largest cost in the pipeline, larger than the
   model calls.
3. **Building competes with thinking.** A story worker holds a model session, a capability ceiling and
   a worktree, and then spends most of its wall-clock in `cargo`. Concurrency is therefore budgeted
   for the expensive resource (a worker) when the scarce one is different (disk, then RAM).
4. **A failing gate is opaque while it runs.** Its output lands in a file, so the surface can say only
   "integrating" for the many minutes a gate takes.

The fleet already has everything a build cluster needs — pinned bases, isolated worktrees, a
supervisor, an event journal, and evidence records. It runs builds today; it just does not *model*
them. Modelling them is what turns "the fleet implements stories" into "the fleet is also the CI that
proves them".

## Approach

**A gate becomes a job, not a command string.** A job is `(repository, base commit, argv, toolchain
fingerprint)` and produces a result record: exit code, duration, the runner's own test summary, and
the bounded output. The job is addressable, so it can be cited by a handoff, a candidate, or a
release — the same way a commit is cited today.

**Results are cached by that key.** Re-gating an unchanged base is a cache hit, not a rebuild. This is
the single largest throughput win available, and it is only possible once the key exists. The cache is
content-addressed rather than time-based: a hit must mean *this exact base with this exact argv under
this exact toolchain*, never "recently green".

**A build executor is not a story worker.** It holds no model session and no write authority beyond
its build directory and its worktree. It is therefore cheap to schedule, safe to run many of, and
budgeted against disk and memory rather than against model concurrency. This is also what makes the
`max_workers`-versus-actual-width gap tractable: today one number governs two unlike resources.

**Gate progress streams.** The executor projects its runner's structural output — phase, current
crate/suite, pass/fail counts — as events, so a surface can show a gate advancing rather than a
spinner. Structural only: no environment, no command output bodies, no paths outside the repository.

**What this deliberately is not.** Not a general CI product, not a remote runner fleet, and not a
replacement for the repository's own release pipeline. The gate stays defined by the repository and
keeps producing exactly the artifact a release already requires. This design changes *who runs it and
what is recorded*, not what "green" means.

## Stories

- A repository gate is declared as a job with a pinned base, argv and toolchain fingerprint, and
  produces a citable result record instead of an exit code in a log.
- Gate results are cached by that key; an unchanged base is a cache hit, and a hit is distinguishable
  from a fresh run in the record.
- A build executor role exists, separable from a story worker: no model session, write authority
  limited to its build directory, scheduled against disk and memory.
- Gate progress streams as structural events, so a running gate is observable in the surface.
- The flux repository's own gate runs this way, as the proof, with its result cited by a candidate.
