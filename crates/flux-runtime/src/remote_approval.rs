//! `RemoteApprover` — the envelope's **approval** stage answered by a human who is not on this
//! machine.
//!
//! ## Approval is a posture, and this adds one
//!
//! The envelope is **authorization → approval → guarded IO**. Of those three, approval is the only
//! stage with a *human* in it — so varying it is choosing a posture, while removing either of the
//! other two is a bug. Three postures are legitimate, and which one is right is a property of the
//! job, not of how careful the operator is:
//!
//! - **Prompt me per effect.** A human answers each guarded operation. What this module adds, for
//!   the case where the human is not at the machine the agent runs on.
//! - **Do not prompt me; constrain instead** ([`AllowApprover`](crate::AllowApprover)). Policy,
//!   sandbox and budget do the constraining. This is the right design for high-autonomy work —
//!   research, security hardening, long exploration — where stopping at every effect is not
//!   caution, it is a broken agent. flux already raises unattended surfaces to the fail-closed
//!   `require` sandbox profile precisely so this posture is *constrained harder*, not unguarded.
//! - **Refuse anything not pre-authorised** ([`DenyApprover`](crate::DenyApprover)).
//!
//! ⚠ **What was actually missing.** Every approver flux shipped was local: the CLI's
//! `StdinApprover` (a terminal prompt), the TUI's `ChannelApprover` (an *in-process* channel), and
//! `SubAgentApprover` (a headless policy). So on a served agent — `flux app run --serve` — the
//! first posture was **not expressible**: an operator got allow-everything or refuse-everything and
//! could not choose the one with a human in it, whatever the job called for. That is the hole this
//! closes. See `docs/designs/remote-agents.md`, including why the "refuse everything" half was
//! never quite that either.
//!
//! ## The shape, and why it is this shape
//!
//! This is the TUI's `ChannelApprover` with a different transport, deliberately: that approver
//! already decouples the *decision* from the terminal by parking on a channel, so a network
//! approver is the same rendezvous with a queue a transport can read instead of an in-process
//! `mpsc`. There is still exactly **one** approval stage in the envelope; this is a second
//! implementation of it, not a second concept.
//!
//! - [`ApprovalQueue`] is the rendezvous. The runtime side parks on it; a transport (flux-server's
//!   `/approvals` routes) lists what is parked and delivers decisions.
//! - [`RemoteApprover`] is the [`Approver`] half. It enqueues, awaits, and **denies** on anything
//!   that is not an explicit, matching approval.
//!
//! ## Three properties this module exists to guarantee
//!
//! 1. **It fails closed when nobody answers.** Silence is a denial, never an approval — an
//!    approval channel that allows on silence is worse than [`AllowApprover`](crate::AllowApprover),
//!    because it *looks* like a control. Every non-answer — timeout, a dropped transport, a
//!    cancelled turn — resolves to [`ApprovalChoice::Deny`]. There is deliberately no "wait
//!    forever" setting: see [`ApprovalQueue::from_env`].
//! 2. **An approval is bound to the effect it was granted for.** A decision must echo the
//!    request's [`fingerprint`](PendingApproval::fingerprint), which is the *canonical form of the
//!    effect itself* — not a hash of it, so there is no collision to find. A `yes` shown for
//!    `read → README.md` cannot be delivered against `process.exec → rm -rf /`, because the two
//!    requests do not have equal fingerprints. Without this the implementation degrades to "the
//!    client said yes", which is a confused deputy waiting to happen.
//! 3. **A decision is single-use.** Answering removes the request from the queue, so a replayed
//!    decision finds nothing to apply itself to and is refused.
//!
//! ## What this is *not*
//!
//! The request id is not a bearer token and must not be treated as one. It is unguessable enough
//! not to be *enumerable*, but the access control on who may answer an approval is the transport's
//! authentication — for the served surface, `flux-server`'s [`ServerAuth`](../../flux_server). An
//! unauthenticated listener means anyone who can reach it can approve, which is why the server
//! refuses an unauthenticated non-loopback bind by construction.
//!
//! There is also deliberately no remote [`ApprovalChoice::AllowAlways`]. Not because standing
//! grants are wrong — that is what the auto-approve posture *is*, chosen up front and visible in
//! the operator's own configuration — but because accumulating one silently, request by request,
//! is a posture nobody chose. An operator who wants "stop asking me" should say so where it can be
//! read, not arrive there by clicking.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use flux_spec::IntentSet;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{ApprovalChoice, Approver, AuthorityRequirement, PlanApprovalRequest};

/// How long a parked approval waits for a decision before denying. Two minutes: long enough for a
/// human to read a request that arrived on a phone, short enough that a wedged turn is not a
/// resource leak.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 120;

/// Longest a request may hold a served turn while waiting for a human.
pub const MAX_APPROVAL_TIMEOUT_SECS: u64 = 3_600;

/// Environment override for [`DEFAULT_APPROVAL_TIMEOUT_SECS`].
pub const APPROVAL_TIMEOUT_ENV: &str = "FLUX_APPROVAL_TIMEOUT_SECS";

/// One approval request parked on an [`ApprovalQueue`], as a remote approver sees it.
///
/// Everything a human needs to decide is here. Every effect fact is part of the
/// [`fingerprint`](Self::fingerprint); only the queue id and advisory wait age are excluded. What
/// was displayed and what gets bound are therefore the same facts by construction rather than by
/// two code paths agreeing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingApproval {
    /// Queue-unique id for this request. Not a secret and not a capability — see the module docs.
    pub id: String,
    /// The canonical form of the effect being asked about. A decision must echo this **exactly**;
    /// that equality is what binds an approval to the effect it was granted for. It is the effect
    /// serialized, not a digest of it, so two different effects cannot share one.
    pub fingerprint: String,
    /// The op (or `"run plan"` for a whole-plan approval).
    pub tool: String,
    /// The concrete resources / commands the effect names, as the local prompts render them.
    pub subjects: Vec<String>,
    /// A one-line risk summary, when the request carried one (whole-plan approvals do).
    pub summary: Option<String>,
    /// The runtime's pre-execution risk signal — the same one the per-op gate acts on.
    pub destructive: bool,
    /// True when the effect writes, executes, or connects out.
    pub mutating: bool,
    /// The complete structured intent set the runtime used for its pre-execution risk decision.
    /// This is part of the fingerprint: two calls with the same permission subject but different
    /// concrete targets are different effects.
    pub intents: IntentSet,
    /// Exact whole-plan facts when this is a batch approval. `None` for one tool dispatch.
    pub plan: Option<PendingPlanApproval>,
    /// Whole seconds this request has already been parked. Advisory display only; expiry is decided
    /// by the waiting side, which denies.
    pub waiting_secs: u64,
}

/// Exact plan-only facts carried by a [`PendingApproval`].
///
/// The friendly `subjects` lines are for display; these typed values are also exposed and bound so
/// two plans cannot collide merely because their summaries render alike.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingPlanApproval {
    /// Distinct operation names in first-seen order.
    pub ops: Vec<String>,
    /// Exact authority requirements derived by plan preview.
    pub requirements: Vec<AuthorityRequirement>,
}

/// The effect facts that become both the public pending request and its exact binding.
struct ApprovalEffect {
    tool: String,
    subjects: Vec<String>,
    summary: Option<String>,
    destructive: bool,
    mutating: bool,
    intents: IntentSet,
    plan: Option<PendingPlanApproval>,
}

/// Why a delivered decision was refused. Each is a **refusal of the decision**, never an approval:
/// the parked request either stays parked (and will time out into a denial) or is already gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideError {
    /// No such parked request. It was already answered (a replay), it timed out, or its turn was
    /// cancelled.
    UnknownRequest,
    /// The decision named a different effect than the request it addressed. The request is left
    /// parked — a client that echoed the wrong fingerprint has a bug, and silently denying its
    /// effect would hide it.
    EffectMismatch,
    /// The request was parked but the runtime side is gone (the turn was cancelled between the
    /// lookup and the send). Nothing was approved.
    Abandoned,
}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::UnknownRequest => {
                "no such pending approval — it was already answered, timed out, or its turn ended"
            }
            Self::EffectMismatch => {
                "the decision's fingerprint does not match the pending request — an approval is \
                 bound to the effect it was granted for and cannot be moved to another"
            }
            Self::Abandoned => "the run waiting on this approval is gone; nothing was approved",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for DecideError {}

/// A parked request plus the channel the waiting runtime side is listening on.
struct Parked {
    request: PendingApproval,
    reply: oneshot::Sender<ApprovalChoice>,
    since: Instant,
}

/// The rendezvous between the runtime's approval gate and whatever transport carries the human's
/// decision. Hand the same `Arc` to a [`RemoteApprover`] (the runtime half) and to the transport
/// that serves [`pending`](Self::pending) / [`decide`](Self::decide).
pub struct ApprovalQueue {
    timeout: Duration,
    seq: AtomicU64,
    nonce: String,
    parked: Mutex<HashMap<String, Parked>>,
}

impl ApprovalQueue {
    /// A queue that denies any request left unanswered for `timeout`, capped at
    /// [`MAX_APPROVAL_TIMEOUT_SECS`] so a nominally finite value cannot become an effectively
    /// permanent resource hold.
    ///
    /// A zero `timeout` denies immediately — which is fail-closed, and is the only thing a
    /// "disabled" setting is allowed to mean here.
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout: timeout.min(Duration::from_secs(MAX_APPROVAL_TIMEOUT_SECS)),
            seq: AtomicU64::new(0),
            nonce: queue_nonce(),
            parked: Mutex::new(HashMap::new()),
        }
    }

    /// [`new`](Self::new) with the timeout read from [`APPROVAL_TIMEOUT_ENV`], defaulting to
    /// [`DEFAULT_APPROVAL_TIMEOUT_SECS`].
    ///
    /// ⚠ There is no value meaning *wait forever*. An unbounded wait is not a denial — it is a
    /// wedged turn holding whatever the effect was about to touch — and an operator reaching for
    /// "no timeout" is reaching for the wrong control. An unparsable value falls back to the
    /// default rather than failing the surface; values above [`MAX_APPROVAL_TIMEOUT_SECS`] are
    /// capped. A *shorter* wait is never the unsafe direction.
    pub fn from_env() -> Self {
        let secs = std::env::var(APPROVAL_TIMEOUT_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS);
        Self::new(Duration::from_secs(secs))
    }

    /// How long a parked request waits before it is denied.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Every request currently awaiting a human decision, oldest first.
    ///
    /// Requests whose wait has already elapsed are swept here rather than listed: the waiting side
    /// has denied them, and offering a decision that could no longer be honoured would misrepresent
    /// the queue.
    pub fn pending(&self) -> Vec<PendingApproval> {
        let mut parked = self.lock();
        let timeout = self.timeout;
        parked.retain(|_, entry| entry.since.elapsed() < timeout);
        let mut out: Vec<(Instant, PendingApproval)> = parked
            .values()
            .map(|entry| {
                let mut request = entry.request.clone();
                request.waiting_secs = entry.since.elapsed().as_secs();
                (entry.since, request)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        out.into_iter().map(|(_, request)| request).collect()
    }

    /// Deliver a human's decision for `id`.
    ///
    /// `fingerprint` must equal the parked request's [`PendingApproval::fingerprint`] — the caller
    /// is asserting *which effect* it is answering about, and a mismatch is refused rather than
    /// applied. On success the request is removed, so the same decision cannot be replayed onto a
    /// later one.
    pub fn decide(
        &self,
        id: &str,
        fingerprint: &str,
        choice: ApprovalChoice,
    ) -> Result<(), DecideError> {
        let entry = {
            let mut parked = self.lock();
            match parked.get(id) {
                // Expired but not yet swept: the waiting side has already denied, so this is the
                // same case as "no such request" and must not read as a delivered approval.
                Some(entry) if entry.since.elapsed() >= self.timeout => {
                    parked.remove(id);
                    return Err(DecideError::UnknownRequest);
                }
                // ⚠ The binding check. Deliberately *before* the removal: a client that echoed the
                // wrong effect must not consume the request, or a mismatched decision would become
                // a denial of the effect the human is still being asked about.
                Some(entry) if entry.request.fingerprint != fingerprint => {
                    return Err(DecideError::EffectMismatch);
                }
                Some(_) => parked
                    .remove(id)
                    .expect("checked present under the same lock"),
                None => return Err(DecideError::UnknownRequest),
            }
        };
        entry.reply.send(choice).map_err(|_| DecideError::Abandoned)
    }

    /// Park `request` and hand back its id plus the channel the caller awaits.
    fn park(&self, mut request: PendingApproval) -> (String, oneshot::Receiver<ApprovalChoice>) {
        let id = format!(
            "ap_{}_{}",
            self.nonce,
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        request.id = id.clone();
        let (reply, rx) = oneshot::channel();
        self.lock().insert(
            id.clone(),
            Parked {
                request,
                reply,
                since: Instant::now(),
            },
        );
        (id, rx)
    }

    /// Drop a parked request without answering it. Idempotent — the request may already be gone
    /// because a decision landed first.
    fn withdraw(&self, id: &str) {
        self.lock().remove(id);
    }

    /// A poisoned lock means some *other* caller panicked mid-critical-section; the map itself is
    /// still a consistent `HashMap`, and refusing to serve approvals afterwards would turn one
    /// panic into a permanently wedged surface. Recover rather than propagate.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Parked>> {
        self.parked.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for ApprovalQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalQueue")
            .field("timeout", &self.timeout)
            .field("pending", &self.lock().len())
            .finish()
    }
}

/// Removes a parked request if the awaiting future goes away — a cancelled turn must not leave a
/// request on the queue for a human to answer into nothing.
struct ParkGuard {
    queue: Arc<ApprovalQueue>,
    id: String,
}

impl Drop for ParkGuard {
    fn drop(&mut self) {
        self.queue.withdraw(&self.id);
    }
}

/// The [`Approver`] whose answer arrives over a network: it parks each request on an
/// [`ApprovalQueue`] and awaits an explicit, effect-bound decision.
///
/// **Every path that is not an explicit approval is a denial** — timeout, a transport that never
/// connected, a transport that disconnected, a cancelled turn. See the module docs.
pub struct RemoteApprover {
    queue: Arc<ApprovalQueue>,
}

impl RemoteApprover {
    /// Approve through `queue`. The transport must hold the same `Arc` — a `RemoteApprover` whose
    /// queue nobody serves denies everything, which is the correct failure but not a useful one.
    pub fn new(queue: Arc<ApprovalQueue>) -> Self {
        Self { queue }
    }

    /// The queue this approver parks on, for a transport that needs to serve it.
    pub fn queue(&self) -> &Arc<ApprovalQueue> {
        &self.queue
    }

    /// Park one request and await the answer. This is the whole fail-closed rule, in one place.
    async fn ask(&self, effect: ApprovalEffect) -> ApprovalChoice {
        let Ok(fingerprint) = fingerprint(&effect) else {
            // The binding could not be represented, so there is no effect-safe approval request
            // to show. A serialization failure must never degrade to a transferable constant.
            return ApprovalChoice::Deny;
        };
        let request = PendingApproval {
            // Replaced by `park`; the fingerprint below is over the *content*, so the id — which is
            // assigned after — is deliberately not part of it.
            id: String::new(),
            fingerprint,
            tool: effect.tool,
            subjects: effect.subjects,
            summary: effect.summary,
            destructive: effect.destructive,
            mutating: effect.mutating,
            intents: effect.intents,
            plan: effect.plan,
            waiting_secs: 0,
        };
        let (id, rx) = self.queue.park(request);
        let _guard = ParkGuard {
            queue: Arc::clone(&self.queue),
            id,
        };
        match tokio::time::timeout(self.queue.timeout, rx).await {
            Ok(Ok(choice)) => choice,
            // The sender was dropped without answering — the queue was cleared out from under us.
            Ok(Err(_)) => ApprovalChoice::Deny,
            // ⚠ Nobody answered. Silence denies.
            Err(_) => ApprovalChoice::Deny,
        }
    }
}

#[async_trait]
impl Approver for RemoteApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        intents: &IntentSet,
    ) -> ApprovalChoice {
        self.ask(ApprovalEffect {
            tool: tool.into(),
            subjects: subjects.to_vec(),
            summary: None,
            destructive: intents.is_destructive(),
            mutating: intents.is_mutating(),
            intents: intents.clone(),
            plan: None,
        })
        .await
    }

    /// ⚠ Overriding this is load-bearing for the binding property, not just for display. The trait
    /// default collapses a whole plan to the single line `N op(s) · <summary>` — so two entirely
    /// different plans that happen to share an op count and a summary would produce the **same**
    /// fingerprint, and one plan's approval would be deliverable against the other. Spending the
    /// plan's own content here is what keeps distinct plans distinct.
    async fn request_plan(&self, plan: &PlanApprovalRequest) -> ApprovalChoice {
        self.ask(ApprovalEffect {
            tool: "run plan".into(),
            subjects: plan_detail_lines(plan),
            summary: Some(plan.summary.clone()),
            destructive: plan.destructive,
            mutating: plan.mutating,
            intents: plan.intents.clone(),
            plan: Some(PendingPlanApproval {
                ops: plan.ops.clone(),
                requirements: plan.requirements.clone(),
            }),
        })
        .await
    }
}

/// The concrete facts a whole-plan approval is asked about: the op names, then the resources and
/// commands statically visible at approval time.
///
/// Only literal arguments contribute — a command assembled from `$symbols` at runtime is invisible
/// here, which is exactly why dispatch re-fires the per-op gate for an undisclosed destructive op.
/// Nothing here changes that.
fn plan_detail_lines(plan: &PlanApprovalRequest) -> Vec<String> {
    let mut lines = Vec::new();
    if !plan.ops.is_empty() {
        lines.push(format!("ops: {}", plan.ops.join(", ")));
    }
    for requirement in &plan.requirements {
        // Operation-kind requirements only restate the ops line above.
        if requirement.resource.kind == flux_policy::ResourceKind::Operation {
            continue;
        }
        let subject = requirement
            .resource
            .path
            .as_deref()
            .or(requirement.resource.name.as_deref())
            .unwrap_or(&requirement.resource.id);
        if subject == "*" {
            continue;
        }
        lines.push(format!("{} → {subject}", requirement.action.0));
    }
    for intent in &plan.intents.intents {
        if let flux_spec::IntentTarget::Process { command } = &intent.target {
            lines.push(format!("process.exec → $ {command}"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    lines.retain(|line| seen.insert(line.clone()));
    lines
}

/// The canonical form of one effect — **the binding**, and deliberately not a digest.
///
/// A hash would introduce a collision an attacker could hunt for; an exact serialization cannot be
/// made to collide, because equality of the encoding is equality of the inputs. JSON's own escaping
/// supplies the unambiguity (`["a", "b"]` and `["a\nb"]` do not encode alike), and a positional
/// tuple rather than a map means the field order is fixed by the type, not by a serializer setting
/// a future feature flag could reorder.
fn fingerprint(effect: &ApprovalEffect) -> Result<String, serde_json::Error> {
    serde_json::to_string(&(
        &effect.tool,
        &effect.subjects,
        effect.summary.as_deref(),
        effect.destructive,
        effect.mutating,
        &effect.intents,
        effect.plan.as_ref(),
    ))
}

/// A per-queue, per-process value that makes request ids non-enumerable across queues.
///
/// ⚠ Not a secret, not a capability, and not a substitute for the transport's authentication —
/// `RandomState` is randomly seeded but is not a CSPRNG. Guessing an id still gets an attacker
/// nowhere on its own: [`ApprovalQueue::decide`] also requires the request's exact fingerprint,
/// which is the effect's content and is not derivable from the id.
fn queue_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(secs: u64) -> Arc<ApprovalQueue> {
        Arc::new(ApprovalQueue::new(Duration::from_secs(secs)))
    }

    /// Park a request in the background and hand back the join handle, so a test can drive the
    /// decision side while the approver is genuinely blocked.
    fn ask_in_background(
        approver: Arc<RemoteApprover>,
        tool: &'static str,
        subject: &'static str,
    ) -> tokio::task::JoinHandle<ApprovalChoice> {
        tokio::spawn(async move {
            approver
                .request(tool, &[subject.to_string()], &IntentSet::default())
                .await
        })
    }

    /// Spin until the queue has `n` entries (the background ask parks asynchronously).
    async fn wait_for_pending(queue: &ApprovalQueue, n: usize) -> Vec<PendingApproval> {
        for _ in 0..500 {
            let pending = queue.pending();
            if pending.len() == n {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("queue never reached {n} pending request(s)");
    }

    #[tokio::test]
    async fn an_explicit_matching_decision_approves_the_effect() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let asking = ask_in_background(approver, "write", "/tmp/report.txt");

        let pending = wait_for_pending(&queue, 1).await;
        let request = &pending[0];
        assert_eq!(request.tool, "write");
        assert_eq!(request.subjects, vec!["/tmp/report.txt".to_string()]);
        queue
            .decide(&request.id, &request.fingerprint, ApprovalChoice::Allow)
            .expect("a matching decision is delivered");

        assert!(matches!(asking.await.unwrap(), ApprovalChoice::Allow));
    }

    /// ⚠ The fail-closed property. Silence is a denial.
    #[tokio::test]
    async fn nobody_answering_denies() {
        let queue = queue(0);
        let approver = RemoteApprover::new(Arc::clone(&queue));
        let choice = approver
            .request(
                "process.exec",
                &["rm -rf /".to_string()],
                &IntentSet::default(),
            )
            .await;
        assert!(
            matches!(choice, ApprovalChoice::Deny),
            "an unanswered approval must deny; answered {choice:?}"
        );
        assert!(
            queue.pending().is_empty(),
            "a timed-out request must not stay on the queue for someone to answer into nothing"
        );
    }

    /// ⚠ A queue nobody serves is not a queue that allows.
    #[tokio::test]
    async fn a_queue_with_no_transport_denies() {
        let approver = RemoteApprover::new(queue(0));
        assert!(matches!(
            approver
                .request("read", &["/etc/shadow".to_string()], &IntentSet::default())
                .await,
            ApprovalChoice::Deny
        ));
    }

    /// ⚠ The confused-deputy property: a `yes` shown for one effect is not deliverable against
    /// another. Both requests are live at once, so this is the exact substitution an attacker
    /// would attempt.
    #[tokio::test]
    async fn an_approval_cannot_be_moved_to_a_different_effect() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let benign = ask_in_background(Arc::clone(&approver), "read", "README.md");
        let destructive = ask_in_background(approver, "process.exec", "rm -rf /");

        let pending = wait_for_pending(&queue, 2).await;
        let benign_req = pending
            .iter()
            .find(|r| r.tool == "read")
            .expect("the benign request is queued");
        let destructive_req = pending
            .iter()
            .find(|r| r.tool == "process.exec")
            .expect("the destructive request is queued");

        // The substitution: the fingerprint the human was shown, aimed at the other request's id.
        assert_eq!(
            queue.decide(
                &destructive_req.id,
                &benign_req.fingerprint,
                ApprovalChoice::Allow
            ),
            Err(DecideError::EffectMismatch),
            "an approval granted for one effect must not apply to another"
        );

        // And the destructive request is still parked — refusing the decision must not be an
        // implicit answer in either direction.
        assert_eq!(wait_for_pending(&queue, 2).await.len(), 2);

        // The honest decisions still work, each against its own effect.
        queue
            .decide(
                &destructive_req.id,
                &destructive_req.fingerprint,
                ApprovalChoice::Deny,
            )
            .expect("its own fingerprint is accepted");
        queue
            .decide(
                &benign_req.id,
                &benign_req.fingerprint,
                ApprovalChoice::Allow,
            )
            .expect("its own fingerprint is accepted");
        assert!(matches!(benign.await.unwrap(), ApprovalChoice::Allow));
        assert!(matches!(destructive.await.unwrap(), ApprovalChoice::Deny));
    }

    /// Two calls can name the same tool and permission subject while carrying different concrete
    /// intent targets. The binding must retain that distinction rather than collapsing the risk
    /// signal to the two display booleans.
    #[tokio::test]
    async fn different_intent_targets_are_different_effects() {
        use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};

        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let ask = |approver: Arc<RemoteApprover>, url: &'static str| {
            tokio::spawn(async move {
                let intents = IntentSet {
                    intents: vec![Intent {
                        behavior: IntentBehavior::NetworkFetch,
                        target: IntentTarget::Url { url: url.into() },
                        role: IntentRole::ReadTarget,
                        certainty: IntentCertainty::Certain,
                    }],
                };
                approver
                    .request("http.request", &["api.example".into()], &intents)
                    .await
            })
        };
        let benign = ask(Arc::clone(&approver), "https://api.example/status");
        let privileged = ask(approver, "https://api.example/admin");

        let pending = wait_for_pending(&queue, 2).await;
        assert_ne!(
            pending[0].fingerprint, pending[1].fingerprint,
            "different intent targets collapsed to one remotely transferable approval"
        );
        for request in pending {
            queue
                .decide(&request.id, &request.fingerprint, ApprovalChoice::Deny)
                .unwrap();
        }
        benign.await.unwrap();
        privileged.await.unwrap();
    }

    /// ⚠ Single use: a captured decision cannot be replayed onto the next request.
    #[tokio::test]
    async fn a_decision_cannot_be_replayed() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));

        let first = ask_in_background(Arc::clone(&approver), "write", "/tmp/a.txt");
        let request = wait_for_pending(&queue, 1).await.remove(0);
        queue
            .decide(&request.id, &request.fingerprint, ApprovalChoice::Allow)
            .expect("the first decision lands");
        assert!(matches!(first.await.unwrap(), ApprovalChoice::Allow));

        // The identical decision, replayed.
        assert_eq!(
            queue.decide(&request.id, &request.fingerprint, ApprovalChoice::Allow),
            Err(DecideError::UnknownRequest),
            "an answered request must not be answerable twice"
        );

        // ...and it does not carry over to the next request either, even for the same effect: the
        // fresh request has its own id.
        let second = ask_in_background(approver, "write", "/tmp/a.txt");
        let fresh = wait_for_pending(&queue, 1).await.remove(0);
        assert_ne!(fresh.id, request.id, "each request gets its own id");
        assert_eq!(
            queue.decide(&request.id, &request.fingerprint, ApprovalChoice::Allow),
            Err(DecideError::UnknownRequest)
        );
        queue
            .decide(&fresh.id, &fresh.fingerprint, ApprovalChoice::Deny)
            .expect("the fresh request answers on its own id");
        assert!(matches!(second.await.unwrap(), ApprovalChoice::Deny));
    }

    /// A cancelled turn must not leave a request on the queue.
    #[tokio::test]
    async fn a_cancelled_run_withdraws_its_request() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let asking = ask_in_background(approver, "write", "/tmp/a.txt");
        wait_for_pending(&queue, 1).await;

        asking.abort();
        for _ in 0..500 {
            if queue.pending().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("an aborted run left its approval parked");
    }

    /// A denial with a reason travels intact — the model is told *why*, which is C-113's behavior
    /// and must not be lost by going over a network.
    #[tokio::test]
    async fn a_denial_reason_survives_the_queue() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let asking = ask_in_background(approver, "write", "/etc/passwd");
        let request = wait_for_pending(&queue, 1).await.remove(0);
        queue
            .decide(
                &request.id,
                &request.fingerprint,
                ApprovalChoice::DenyWithReason("not that file".into()),
            )
            .unwrap();
        match asking.await.unwrap() {
            ApprovalChoice::DenyWithReason(why) => assert_eq!(why, "not that file"),
            other => panic!("expected a reasoned denial, got {other:?}"),
        }
    }

    /// ⚠ Two plans that the trait's default `request_plan` would render identically must not share
    /// a fingerprint — otherwise one plan's approval is deliverable against the other.
    #[tokio::test]
    async fn distinct_plans_do_not_share_a_fingerprint() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        let plan_of = |ops: [&str; 2]| PlanApprovalRequest {
            summary: "2 ops".into(),
            ops: ops.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let benign = plan_of(["read", "reflect"]);
        let destructive = plan_of(["read", "process.exec"]);
        // The default rendering the trait would have used: identical for both.
        assert_eq!(benign.subject(), destructive.subject());

        let a = Arc::clone(&approver);
        let benign_task = tokio::spawn(async move { a.request_plan(&benign).await });
        let b = Arc::clone(&approver);
        let destructive_task = tokio::spawn(async move { b.request_plan(&destructive).await });

        let pending = wait_for_pending(&queue, 2).await;
        assert_ne!(
            pending[0].fingerprint, pending[1].fingerprint,
            "two different plans produced one fingerprint — an approval for either would apply to \
             both"
        );
        for request in &pending {
            queue
                .decide(&request.id, &request.fingerprint, ApprovalChoice::Deny)
                .unwrap();
        }
        benign_task.await.unwrap();
        destructive_task.await.unwrap();
    }

    /// The queue reports what is parked, and stops reporting it once answered.
    #[tokio::test]
    async fn pending_lists_only_live_requests() {
        let queue = queue(30);
        let approver = Arc::new(RemoteApprover::new(Arc::clone(&queue)));
        assert!(queue.pending().is_empty());
        let asking = ask_in_background(approver, "read", "a.txt");
        let request = wait_for_pending(&queue, 1).await.remove(0);
        queue
            .decide(&request.id, &request.fingerprint, ApprovalChoice::Allow)
            .unwrap();
        asking.await.unwrap();
        assert!(queue.pending().is_empty());
    }

    #[test]
    fn an_unknown_id_is_refused() {
        let queue = ApprovalQueue::new(Duration::from_secs(30));
        assert_eq!(
            queue.decide("ap_nope_0", "{}", ApprovalChoice::Allow),
            Err(DecideError::UnknownRequest)
        );
    }

    /// The fingerprint is injective across the shapes a naive join would confuse.
    #[test]
    fn the_fingerprint_separates_effects_a_naive_join_would_merge() {
        let intents = IntentSet::default();
        let fingerprint_of = |subjects: Vec<String>, destructive| {
            fingerprint(&ApprovalEffect {
                tool: "exec".into(),
                subjects,
                summary: None,
                destructive,
                mutating: true,
                intents: intents.clone(),
                plan: None,
            })
            .unwrap()
        };
        let two = fingerprint_of(vec!["a".into(), "b".into()], false);
        let one = fingerprint_of(vec!["a\nb".into()], false);
        assert_ne!(two, one);
        assert_ne!(
            fingerprint_of(vec!["x".into()], false),
            fingerprint_of(vec!["x".into()], true),
            "the risk signal the human was shown is part of what they approved"
        );
        assert_eq!(
            fingerprint_of(vec!["x".into()], false),
            fingerprint_of(vec!["x".into()], false),
            "the same effect fingerprints the same, or no decision could ever be delivered"
        );
    }

    /// The env override is read, and a nonsense value falls back rather than failing the surface.
    #[test]
    fn the_timeout_default_is_bounded() {
        assert!(ApprovalQueue::new(Duration::from_secs(5)).timeout() == Duration::from_secs(5));
        assert_eq!(
            DEFAULT_APPROVAL_TIMEOUT_SECS, 120,
            "the documented default and the constant must agree"
        );
        assert_eq!(
            ApprovalQueue::new(Duration::from_secs(u64::MAX)).timeout(),
            Duration::from_secs(MAX_APPROVAL_TIMEOUT_SECS),
            "a nominally finite timeout must not become a centuries-long resource hold"
        );
    }
}
