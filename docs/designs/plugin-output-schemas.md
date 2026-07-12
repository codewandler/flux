# Design — Plugin operation output schemas

**Story:** D-164 · **Status:** accepted 2026-07-12

Plugin manifests already carry model-facing input JSON Schema, while `ToolSpec` can carry an optional output
schema. Add the missing manifest field and preserve it through plugin projection. The field is optional and
serde-defaulted so existing plugins and manifests remain wire-compatible; consumers may impose stricter
requirements on a particular plugin catalog. Manifest round-trip and projection tests guard the seam.
