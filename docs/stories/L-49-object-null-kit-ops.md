---
id: L-49
title: Object & null-kit ops — `pick`, `omit`, `merge_obj`, `coalesce`, `keys`, `values`
pillar: Language
status: done
priority:
epic: data-transforms
design: docs/designs/data-transforms.md
note: "gitlab-plugin payload trimming (D-94 residual): today you rebuild issue objects field-by-field with `obj` templates; `pick` + `coalesce` make it one line"
---

# Object & null-kit ops — `pick`, `omit`, `merge_obj`, `coalesce`, `keys`, `values`

## Goal
Ship the small, deterministic object-shaping surface that isn't currently expressible:
pick a subset of fields, complement it, shallow-merge, coalesce over a candidate list, and
enumerate keys/values. Independent of the expr engine — parallelizable with L-50 from
day 1 of the epic. Motivating case: the gitlab plugin returns raw ~50-key issue/MR
objects; plans want a slim projection.

## Acceptance
- [x] `pick({items, keys})` — keep only listed keys; **`items` may be one object or an
      array of objects** (applied per element). Missing keys are simply absent (no error).
      Failing-first tests: `pick_single_object`, `pick_over_array_of_objects`.
- [x] `omit({items, keys})` — complement of `pick`; same single-object-or-array shape.
      Failing-first test: `omit_removes_keys_leaves_others`.
- [x] `merge_obj({objects})` — shallow merge an array of objects, later keys win. Named
      to avoid collision with the existing list-concat `merge`. Non-object element →
      clear error. Failing-first test: `merge_obj_shallow_later_wins`.
- [x] `coalesce({values, default?})` — first value that is not `null` and not `""`; else
      `default`; else `null`. `0` and `false` are **kept** values. Failing-first tests:
      `coalesce_returns_first_non_empty`, `coalesce_keeps_zero_and_false`.
- [x] `keys({item})` / `values({item})` — return an array of the object's keys / values
      in deterministic (`serde_json::Map`) order. Non-object → clear error. Failing-first
      test: `keys_and_values_deterministic_order`.
- [x] All six new ops registered in `register_cognition` and the `cognition` group; group
      description updated.
- [x] `website/docs/language/ops.md` cognition-tools table gains a row per new op with
      one native-text example.
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress
- Implemented in `flux-tools` cognition ops with acceptance tests for single-object and array
  `pick`, `omit`, shallow `merge_obj`, `coalesce`, and deterministic `keys`/`values`.
- Documented in the website operations page and engine ops reference, with a CHANGELOG entry under
  `[Unreleased]`.

## Notes
- Distinct from list-concat `merge` — the name `merge_obj` is deliberate; do not overload.
- No dependency on L-46 (no expr formulas) — this story is safe to work in parallel with
  L-50 while L-46 is in flight.
- Related payload-trimming context: [D-94](D-94-gitlab-output-ergonomics.md) discusses the
  gitlab plugin's raw payloads; `pick`/`omit` don't close that story but make its
  agent-side ergonomics dramatically better.
