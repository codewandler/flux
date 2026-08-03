---
id: C-496
title: Gate the opt-in PDF feature
pillar: Core
status: in-progress
priority: 0
note: "The v0.53 release candidate found that codewandler-flux-web/pdf had tests but no disposition in the exhaustive feature-gate ledger"
---

# Gate the opt-in PDF feature

## Goal
The fixed opt-in PDF parser cannot decay behind a feature that the ordinary workspace build never
enables.

## Acceptance
- [x] The exhaustive feature-gate checker fails first because `codewandler-flux-web/pdf` is absent.
- [x] The ledger runs the PDF feature's tests in CI and explains why the default workspace does not.
- [ ] The corrected v0.53.0 release commit passes main CI and the exact-SHA release candidate.

## Notes
- Run 30775933346 refused release commit `77946f50` at
  `scripts/check-feature-gated-tests.sh` before the tag was published.
- The complete checker passes locally, including 87 `codewandler-flux-web --features pdf` tests.
