//! `flux-policy` — the pure, IO-free authorization core (distilled from `fluxplane-policy`).
//!
//! Authorization is **default-deny**: a [`Request`] is permitted only if some [`Grant`] matches
//! its subject, action, and resource, the caller's trust meets the grant's `required_trust`, and
//! the caller holds the grant's `required_scopes`. Missing scopes or a `requires_approval` grant
//! escalate to [`Decision::ApprovalRequired`] rather than allowing. There is no IO here — the
//! runtime calls [`evaluate`] and enforces the decision.

mod glob;
pub use glob::wildcard_match;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------

/// Trust levels, ordered ascending: an actor at a higher level satisfies any lower requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    #[default]
    Untrusted,
    Verified,
    Privileged,
    System,
}

/// What the trust assertion is about (where it came from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustKind {
    Invocation,
    Source,
    Target,
}

/// A fine-grained permission scope (opaque string, e.g. `"workspace:write"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope(pub String);

impl<T: Into<String>> From<T> for Scope {
    fn from(s: T) -> Self {
        Scope(s.into())
    }
}

/// The resolved trust attached to a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trust {
    pub kind: TrustKind,
    pub level: TrustLevel,
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

// ---------------------------------------------------------------------------
// Subjects / caller
// ---------------------------------------------------------------------------

/// The kind of principal making a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallerKind {
    User,
    Agent,
    System,
}

/// The resolved principal behind a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: CallerKind,
}

/// The caller of an operation, with its groups and origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Caller {
    pub principal: Principal,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub source: String,
}

/// The kind of subject a grant targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectKind {
    User,
    Group,
    Agent,
    System,
}

/// A subject reference in a grant; `id` may be a wildcard (e.g. `"*"`, `"team-*"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectRef {
    pub kind: SubjectKind,
    pub id: String,
}

// ---------------------------------------------------------------------------
// Resources / actions
// ---------------------------------------------------------------------------

/// The kind of resource an action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Operation,
    Workspace,
    Path,
    Process,
    Network,
    Datasource,
    Secret,
}

/// A resource reference; `id` is wildcard-matchable and `path` (when set) uses glob matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRef {
    pub kind: ResourceKind,
    #[serde(default = "star")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn star() -> String {
    "*".to_string()
}

impl ResourceRef {
    /// A resource of `kind` with id `"*"`.
    pub fn any(kind: ResourceKind) -> Self {
        Self {
            kind,
            id: star(),
            name: None,
            path: None,
        }
    }

    /// A filesystem path resource.
    pub fn path(p: impl Into<String>) -> Self {
        Self {
            kind: ResourceKind::Path,
            id: star(),
            name: None,
            path: Some(p.into()),
        }
    }
}

/// An action verb (dotted, e.g. `"workspace.read"`, `"process.exec"`). Supports `"domain.*"` and
/// `"*"` wildcards on the grant side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action(pub String);

impl<T: Into<String>> From<T> for Action {
    fn from(s: T) -> Self {
        Action(s.into())
    }
}

// ---------------------------------------------------------------------------
// Grants / policy
// ---------------------------------------------------------------------------

/// A single grant: subjects × resources × actions, gated by trust, scopes, and approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub subjects: Vec<SubjectRef>,
    pub resources: Vec<ResourceRef>,
    pub actions: Vec<Action>,
    #[serde(default)]
    pub required_trust: TrustLevel,
    #[serde(default)]
    pub required_scopes: Vec<Scope>,
    #[serde(default)]
    pub requires_approval: bool,
}

/// A policy is an ordered set of grants. An empty policy denies everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthorizationPolicy {
    #[serde(default)]
    pub grants: Vec<Grant>,
}

/// An authorization request: a caller (with trust) wants to perform `action` on `resource`.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub caller: &'a Caller,
    pub trust: &'a Trust,
    pub action: &'a Action,
    pub resource: &'a ResourceRef,
}

/// The decision flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    ApprovalRequired,
}

/// The full evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluation {
    pub decision: Decision,
    #[serde(default)]
    pub missing_scopes: Vec<Scope>,
    pub reason: String,
}

impl Evaluation {
    pub fn allowed(&self) -> bool {
        self.decision == Decision::Allow
    }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

fn subject_matches(s: &SubjectRef, caller: &Caller) -> bool {
    match s.kind {
        SubjectKind::User => {
            caller.principal.kind == CallerKind::User && wildcard_match(&s.id, &caller.principal.id)
        }
        SubjectKind::Agent => {
            caller.principal.kind == CallerKind::Agent
                && wildcard_match(&s.id, &caller.principal.id)
        }
        SubjectKind::System => caller.principal.kind == CallerKind::System,
        SubjectKind::Group => caller.groups.iter().any(|g| wildcard_match(&s.id, g)),
    }
}

fn action_matches(grant: &Action, req: &Action) -> bool {
    wildcard_match(&grant.0, &req.0)
}

fn resource_matches(grant: &ResourceRef, req: &ResourceRef) -> bool {
    if grant.kind != req.kind {
        return false;
    }
    if !wildcard_match(&grant.id, &req.id) {
        return false;
    }
    // If the grant constrains a path, the request must carry a matching path. Normalize the request
    // path lexically (collapse `.`/`..`) first so a traversal like `workspace/../../etc/passwd`
    // can't be widened to match a `workspace/*` grant — defense-in-depth alongside flux-system's
    // canonicalizing IO boundary.
    if let Some(gp) = &grant.path {
        match &req.path {
            Some(rp) => wildcard_match(gp, &normalize_path_lexically(rp)),
            None => false,
        }
    } else {
        true
    }
}

/// Collapse `.`/`..` segments in a path string without touching the filesystem. `..` that would
/// escape the leading segment is preserved literally (so it simply fails to match an in-root grant
/// glob rather than silently climbing out).
fn normalize_path_lexically(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

fn grant_applies(g: &Grant, req: &Request) -> bool {
    g.subjects.iter().any(|s| subject_matches(s, req.caller))
        && g.actions.iter().any(|a| action_matches(a, req.action))
        && g.resources
            .iter()
            .any(|r| resource_matches(r, req.resource))
}

fn missing_scopes(required: &[Scope], held: &[Scope]) -> Vec<Scope> {
    required
        .iter()
        .filter(|r| !held.iter().any(|h| h.0 == r.0))
        .cloned()
        .collect()
}

/// A permissive-but-gated default policy for the local single-user case: the local `User` may read
/// and write anywhere in the workspace and reach the network freely, but executing a process
/// requires approval. Without this, an empty [`AuthorizationPolicy`] is default-deny and would
/// reject every operation — bricking the agent. Surfaces layer their own/config grants on top.
pub fn default_local_grants() -> AuthorizationPolicy {
    let user = || SubjectRef {
        kind: SubjectKind::User,
        id: "*".into(),
    };
    // A Path resource whose glob matches any path (the matcher treats `*` as matching `/` too).
    let path_any = || ResourceRef {
        kind: ResourceKind::Path,
        id: "*".into(),
        name: None,
        path: Some("*".into()),
    };
    let grant =
        |actions: Vec<Action>, resources: Vec<ResourceRef>, requires_approval: bool| Grant {
            subjects: vec![user()],
            resources,
            actions,
            required_trust: TrustLevel::Untrusted,
            required_scopes: Vec::new(),
            requires_approval,
        };
    AuthorizationPolicy {
        grants: vec![
            grant(
                vec![Action::from("workspace.read")],
                vec![path_any()],
                false,
            ),
            grant(
                vec![Action::from("workspace.write")],
                vec![path_any()],
                false,
            ),
            grant(
                vec![Action::from("network.fetch")],
                vec![ResourceRef::any(ResourceKind::Network)],
                false,
            ),
            grant(
                vec![Action::from("process.exec")],
                vec![ResourceRef::any(ResourceKind::Process)],
                true,
            ),
        ],
    }
}

/// Evaluate a request against a policy. Default-deny; the first fully-satisfied grant allows,
/// otherwise a matched-but-gated grant escalates to approval, otherwise deny.
pub fn evaluate(policy: &AuthorizationPolicy, req: &Request) -> Evaluation {
    let mut pending_approval: Option<Vec<Scope>> = None;

    for g in &policy.grants {
        if !grant_applies(g, req) {
            continue;
        }
        // Trust gate: insufficient trust means this grant doesn't apply.
        if req.trust.level < g.required_trust {
            continue;
        }
        let missing = missing_scopes(&g.required_scopes, &req.trust.scopes);
        if !missing.is_empty() {
            // Escalate (but keep looking — another grant may fully allow).
            let entry = pending_approval.get_or_insert_with(Vec::new);
            for m in missing {
                if !entry.iter().any(|e| e.0 == m.0) {
                    entry.push(m);
                }
            }
            continue;
        }
        if g.requires_approval {
            pending_approval.get_or_insert_with(Vec::new);
            continue;
        }
        return Evaluation {
            decision: Decision::Allow,
            missing_scopes: Vec::new(),
            reason: "granted".to_string(),
        };
    }

    match pending_approval {
        Some(missing) if !missing.is_empty() => Evaluation {
            decision: Decision::ApprovalRequired,
            missing_scopes: missing,
            reason: "missing required scopes".to_string(),
        },
        Some(_) => Evaluation {
            decision: Decision::ApprovalRequired,
            missing_scopes: Vec::new(),
            reason: "grant requires approval".to_string(),
        },
        None => Evaluation {
            decision: Decision::Deny,
            missing_scopes: Vec::new(),
            reason: "no matching grant (default deny)".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> Caller {
        Caller {
            principal: Principal {
                id: id.to_string(),
                name: id.to_string(),
                kind: CallerKind::User,
            },
            groups: vec![],
            source: "cli".into(),
        }
    }

    fn trust(level: TrustLevel, scopes: &[&str]) -> Trust {
        Trust {
            kind: TrustKind::Invocation,
            level,
            scopes: scopes.iter().map(|s| Scope::from(*s)).collect(),
        }
    }

    fn grant(actions: &[&str], res: ResourceRef) -> Grant {
        Grant {
            subjects: vec![SubjectRef {
                kind: SubjectKind::User,
                id: "*".into(),
            }],
            resources: vec![res],
            actions: actions.iter().map(|a| Action::from(*a)).collect(),
            required_trust: TrustLevel::Untrusted,
            required_scopes: vec![],
            requires_approval: false,
        }
    }

    fn eval(
        policy: &AuthorizationPolicy,
        caller: &Caller,
        tr: &Trust,
        action: &str,
        res: &ResourceRef,
    ) -> Evaluation {
        evaluate(
            policy,
            &Request {
                caller,
                trust: tr,
                action: &Action::from(action),
                resource: res,
            },
        )
    }

    #[test]
    fn empty_policy_denies() {
        let p = AuthorizationPolicy::default();
        let e = eval(
            &p,
            &user("alice"),
            &trust(TrustLevel::System, &[]),
            "workspace.read",
            &ResourceRef::any(ResourceKind::Workspace),
        );
        assert_eq!(e.decision, Decision::Deny);
    }

    #[test]
    fn matching_grant_allows_and_path_glob_works() {
        let p = AuthorizationPolicy {
            grants: vec![grant(&["workspace.read"], ResourceRef::path("src/**"))],
        };
        let c = user("alice");
        let t = trust(TrustLevel::Untrusted, &[]);
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "workspace.read",
                &ResourceRef::path("src/main.rs")
            )
            .decision,
            Decision::Allow
        );
        // path outside the glob → deny
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "workspace.read",
                &ResourceRef::path("etc/passwd")
            )
            .decision,
            Decision::Deny
        );
        // different action → deny
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "workspace.write",
                &ResourceRef::path("src/main.rs")
            )
            .decision,
            Decision::Deny
        );
    }

    #[test]
    fn path_grant_not_widened_by_traversal() {
        let grant = ResourceRef::path("workspace/*");
        assert!(resource_matches(
            &grant,
            &ResourceRef::path("workspace/sub/file.rs")
        ));
        assert!(
            !resource_matches(&grant, &ResourceRef::path("workspace/../../etc/passwd")),
            "a `..` traversal must not match an in-root grant glob"
        );
        // A `*` grant still matches anything (default-local-grant behavior preserved).
        assert!(resource_matches(
            &ResourceRef::path("*"),
            &ResourceRef::path("anything/at/all")
        ));
    }

    #[test]
    fn normalize_lexical_collapses_traversal() {
        assert_eq!(
            normalize_path_lexically("workspace/../../etc/passwd"),
            "../etc/passwd"
        );
        assert_eq!(normalize_path_lexically("src/./a/b"), "src/a/b");
        assert_eq!(normalize_path_lexically("/a/b/../c"), "/a/c");
    }

    #[test]
    fn action_wildcard_matches() {
        let p = AuthorizationPolicy {
            grants: vec![grant(
                &["workspace.*"],
                ResourceRef::any(ResourceKind::Workspace),
            )],
        };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Untrusted, &[]),
            "workspace.write",
            &ResourceRef::any(ResourceKind::Workspace),
        );
        assert_eq!(e.decision, Decision::Allow);
    }

    #[test]
    fn insufficient_trust_denies() {
        let mut g = grant(&["secret.use"], ResourceRef::any(ResourceKind::Secret));
        g.required_trust = TrustLevel::Privileged;
        let p = AuthorizationPolicy { grants: vec![g] };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Verified, &[]),
            "secret.use",
            &ResourceRef::any(ResourceKind::Secret),
        );
        assert_eq!(e.decision, Decision::Deny);
    }

    #[test]
    fn missing_scope_escalates_to_approval() {
        let mut g = grant(&["process.exec"], ResourceRef::any(ResourceKind::Process));
        g.required_scopes = vec![Scope::from("danger:exec")];
        let p = AuthorizationPolicy { grants: vec![g] };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Untrusted, &[]),
            "process.exec",
            &ResourceRef::any(ResourceKind::Process),
        );
        assert_eq!(e.decision, Decision::ApprovalRequired);
        assert_eq!(e.missing_scopes, vec![Scope::from("danger:exec")]);
    }

    #[test]
    fn requires_approval_flag_escalates() {
        let mut g = grant(&["process.exec"], ResourceRef::any(ResourceKind::Process));
        g.requires_approval = true;
        let p = AuthorizationPolicy { grants: vec![g] };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Untrusted, &[]),
            "process.exec",
            &ResourceRef::any(ResourceKind::Process),
        );
        assert_eq!(e.decision, Decision::ApprovalRequired);
    }

    #[test]
    fn a_full_allow_grant_wins_over_an_approval_grant() {
        let mut approval = grant(&["process.exec"], ResourceRef::any(ResourceKind::Process));
        approval.requires_approval = true;
        let full = grant(&["process.exec"], ResourceRef::any(ResourceKind::Process));
        let p = AuthorizationPolicy {
            grants: vec![approval, full],
        };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Untrusted, &[]),
            "process.exec",
            &ResourceRef::any(ResourceKind::Process),
        );
        assert_eq!(e.decision, Decision::Allow);
    }

    #[test]
    fn default_local_grants_allow_fs_and_net_but_gate_process() {
        let p = default_local_grants();
        let c = user("local");
        let t = trust(TrustLevel::Privileged, &[]);
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "workspace.read",
                &ResourceRef::path("src/main.rs")
            )
            .decision,
            Decision::Allow
        );
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "workspace.write",
                &ResourceRef::path("README.md")
            )
            .decision,
            Decision::Allow
        );
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "network.fetch",
                &ResourceRef::any(ResourceKind::Network)
            )
            .decision,
            Decision::Allow
        );
        // process execution is granted but requires approval
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "process.exec",
                &ResourceRef::any(ResourceKind::Process)
            )
            .decision,
            Decision::ApprovalRequired
        );
        // an action with no grant is denied (default-deny floor)
        assert_eq!(
            eval(
                &p,
                &c,
                &t,
                "secret.use",
                &ResourceRef::any(ResourceKind::Secret)
            )
            .decision,
            Decision::Deny
        );
    }

    #[test]
    fn held_scope_satisfies_requirement() {
        let mut g = grant(&["process.exec"], ResourceRef::any(ResourceKind::Process));
        g.required_scopes = vec![Scope::from("danger:exec")];
        let p = AuthorizationPolicy { grants: vec![g] };
        let e = eval(
            &p,
            &user("a"),
            &trust(TrustLevel::Untrusted, &["danger:exec"]),
            "process.exec",
            &ResourceRef::any(ResourceKind::Process),
        );
        assert_eq!(e.decision, Decision::Allow);
    }
}
