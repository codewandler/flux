---
id: C-652
title: "HTTP joins the guarded port"
pillar: "Core"
status: done
epic: first-class-hosts
areas: [flux-system, flux-web]
design: first-class-hosts
note: "Decision 0018 rule 5: GuardedHttp on the port so web effects can follow the selected substrate; remote wire support stays a separate versioned change"
---

# HTTP joins the guarded port

## Goal

HTTP joins the port so web effects can follow the selected substrate. A `GuardedHttp` trait joins
`ExecutionSystem` in `crates/flux-system/src/port.rs` with the existing egress guard, redaction
and size/time caps; the native implementation wraps the delivered `flux-web` egress client; and
`flux-web` operations move from `NativeSystemOnly` to `SelectedExecutionSystem` where their
semantics allow. Wire support on the remote protocol is explicitly out of scope — it is a separate
versioned protocol change.

## Acceptance

- [x] `GuardedHttp` joins the port with a fail-closed `Unserved` default and a reviewed codegate
      census entry.
- [x] The native implementation routes through the existing pinned egress guard and redactor; no
      second HTTP client appears anywhere (the codegate `Http` census stays clean).
- [x] `http.request` and `web.fetch` placement moves to `SelectedExecutionSystem`; `browser.*`
      stays `NativeSystemOnly`; the placement census test is updated deliberately.
- [x] `RemoteSystem` answers HTTP with a typed `Unserved` naming the missing wire support rather
      than approximating.


## Comments

- Review open questions routed: sub-agent ToolContext propagation of the selected substrate is C-675 acceptance; the kind-sniffing gate in Executor::non_native_target is settled by C-651's every_admission_path_reports_the_same_non_native_kind.
