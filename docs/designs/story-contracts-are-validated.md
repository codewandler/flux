# Story contracts are validated

Why `flux board` should refuse a story with no acceptance criteria, what to borrow from
spec-driven frameworks, and what to leave alone.

## The problem, measured

`flux board check` never opens a story's body. It validates frontmatter, ids, priorities and document
links and stops. Across 1,251 stories on 2026-08-08:

| Finding | Count |
|---|---|
| Carrying the literal `- [ ] Define acceptance.` | 13 (3 `ready`) |
| Zero checkboxes under `## Acceptance` | 10 |
| Missing or empty `## Goal` | 14 |
| **`ready` stories with no usable contract** | **5** |
| `done` stories closed with zero criteria | 5 |

Three defects produced those numbers, each independently sufficient:

1. **`board done` cannot tell absence from satisfaction.** It refuses when `remaining > 0`, and
   `remaining == 0` is also true when `total == 0`. `D-08`, `D-14`, `D-15`, `D-16`, `D-17` closed
   that way.
2. **The heading match is exact.** `## Acceptance (for the epic)` and `## Acceptance — stage 1`
   report zero criteria, so ten stories have contracts no tool can read.
3. **A malformed story is dropped, not failed.** `C-320` carries `status: active`, exists on disk,
   and is invisible to every board read while `check` exits 0.

This matters because acceptance is machine-read in **five** places: `board done` counts its
checkboxes, C-723's driver parses the symbols and paths it names to decide whether to withhold,
`board reconcile` derives `acceptance-complete`, `board stats` aggregates it, and C-587's reviewer
**refuses to review at all** without it. The last is the only place a missing contract is already a
hard stop — and it fires long after dispatch.

## What the research found

Surveyed: GitHub Spec Kit, Amazon Kiro (EARS), BDD/Gherkin, SAFe/Agile hierarchies, Jira / Azure
DevOps / Linear, and tools claiming to enforce acceptance criteria.

**The most useful finding is negative.** Across the three most-used trackers, *nothing enforces
content quality by default*. Jira can with an app; Azure DevOps has the field and never requires it;
Linear can require a field's presence in a template and never validates its content. A CLI that has
the field, requires it at a transition, **and lints its content** is doing something none of them do.

### Worth taking

**Addressable criteria** — Kiro's `_Requirements: 1.1, 2.3_` backlinks, Spec Kit's `FR-###`. Stable
ids turn "does this story have criteria" into "is every criterion covered, by what evidence". Our
checkboxes are most of the way there. Highest leverage of anything surveyed. → **C-739**

**A per-criterion verification handle** — the command, test name or artifact that proves it. This
targets our most documented failure class directly (*"verify the artifact, never the message"*), and
C-723 already approximates it by scraping backticked symbols out of prose. → **C-739**

**A greppable ambiguity marker** — Spec Kit's `[NEEDS CLARIFICATION: …]`, Example Mapping's red card.
An agent will never ask; it guesses, implements the guess, and the result compiles. Making
not-asking illegal at `backlog → ready` is the cheapest defence available. → **C-740**

**Transition validators with a recorded escape** — Jira attaches checks to a `from → to` edge rather
than to a type globally. OpenSpec's `skip_specs: true` is the escape-hatch pattern: opting out is
possible but leaves a reason in the file. We already have the precedent — `board done
--override-reason` writes `done_override:` into frontmatter. Escape hatches that leave a trace beat
escape hatches that don't exist, because people just write `- [ ] it works`. → **C-736**

**A kind discriminator** — `feature | enabler | spike | bug`. Not bureaucracy: it selects which rules
apply. A spike's contract is "question answered, findings recorded", not "behaviour implemented with
a failing-first test". Kiro already forks this way. → **C-741**

**A glossary** — DDD's ubiquitous language. Our vocabulary is dense and precise and agents mangle it;
`AGENTS.md` keeps correcting the same confusions in prose, which does not survive a fresh agent.
→ **C-744**

**The one content lint worth writing first**: an acceptance bullet containing `fast|robust|properly|
correctly|gracefully|as expected` with no observable artifact, command or number is not a criterion.
Spec Kit's `/analyze` does this. Probably catches more real defects than everything else combined.

### Deliberately not taking

- **Full Gherkin `.feature` files.** They exist to bind to step definitions. If we are not executing
  them they are pure ceremony. Take the Given/When/Then *shape* and the Rule↔Example distinction;
  skip Scenario Outlines, Backgrounds, tags.
- **EARS as a hard requirement.** Its `shall` phrasing is aerospace-flavoured and reads stiffly for
  infrastructure work. Our bullets are already essentially EARS unwanted-behaviour clauses without
  the keyword. Lint for a trigger or state clause plus a verifiable outcome; recommend the keywords,
  don't mandate them.
- **Spec Kit's Phase-0/1 fan-out** — `research.md`, `data-model.md`, `contracts/`, `quickstart.md`,
  branch-per-spec numbering. A greenfield-web-app ritual.
- **SAFe's portfolio layer** — WSJF, lean business cases, capability/portfolio-epic tiers.
- **A hand-maintained traceability matrix.** The archetypal dead artifact. If criteria carry ids and
  commits cite them, traceability is a derived query (`board trace`), never a file anyone updates.
- **A risk register**, unless something consumes it. It earns its keep only if `risk: high` actually
  changes agent behaviour — forces review, disables auto-promotion, narrows the ceiling.

## The stories

| Story | |
|---|---|
| **C-736** | `check` validates the contract a story is dispatched against |
| **C-737** | A missing or suffixed Acceptance heading cannot pass for a satisfied contract |
| **C-738** | Creating a story generates the template and refuses a `ready` story with no contract |
| **C-739** | Acceptance criteria are addressable and carry their own verification handle |
| **C-740** | An unresolved question blocks the `ready` transition |
| **C-741** | A story declares its kind and is validated as that kind |
| **C-742** | An epic is one entity with its own measurable contract |
| **C-744** | A glossary the fleet's agents are held to |

C-736 and C-737 close what already bit us and introduce no new concepts. The rest are additive.

## A note on epics

An epic is currently **three inconsistent things**: a free-text `epic:` string on 1,048 stories that
is never resolved (~39 distinct slugs point at no document, silently, while `design:` *is* resolved);
a design doc written by `create --kind epic` with no frontmatter, no id, no status and no acceptance,
whose `E-` id is allocated and discarded; and 58 ordinary story files that are epic trackers by
convention. No rule anywhere distinguishes an epic from a story. → **C-742**

## Sources

Spec Kit: https://github.com/github/spec-kit · https://github.com/github/spec-kit/blob/main/spec-driven.md ·
Kiro: https://kiro.dev/docs/specs/ · EARS: https://alistairmavin.com/ears/ ·
Gherkin: https://cucumber.io/docs/gherkin/reference/ · Example Mapping: https://cucumber.io/docs/bdd/example-mapping/ ·
Ubiquitous language for agents: https://dev.to/aws/your-agent-keeps-using-that-word--4g36
