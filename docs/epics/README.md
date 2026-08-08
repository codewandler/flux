# Epics

One epic, one file: `docs/epics/<slug>.md`. The slug is the key every member story already carries in
its `epic:` frontmatter, and `flux board check` refuses a story whose slug names no file here — the
same way it refuses a `design:` that points at nothing.

## Schema

```yaml
---
id: E-12                                  # required, unique, `E-` prefixed
title: "What this epic delivers"          # required
design: docs/designs/<slug>.md            # optional: the design behind it
tracker: C-420                            # optional: the story that used to be its tracker
---
```

Then `## Why`, `## Success criteria` and `## Exit criteria`. Both criteria sections are required and
each needs at least one checkbox; an epic with no measurable contract is the bag of stories this
document type exists to stop being.

## An epic never declares its status

There is no `status:` field, and `check` refuses one. An epic's completion is **derived** from the
stories carrying its slug and reported by `flux board epics`:

```bash
flux board epics --output json               # every epic, with its member ratio
flux board epics --slug connector-platform   # one of them
```

That is the whole reason this file type exists. The epic trackers it replaces sat at
`status: backlog` while every member story was `done`, and nothing could tell.

## Authoring one

```bash
flux board create --kind epic --title "…" --design docs/designs/<slug>.md
flux board commit docs/epics/<slug>.md -m "board: file the <slug> epic"
```

Creation allocates the next `E-` id from the epics that exist and writes the contract skeleton. The
seeded success criterion carries `[NEEDS AUTHORING]`; `check` counts every epic still holding that
marker and reports the total as a warning, so unwritten contracts stay visible instead of passing for
authored ones.
