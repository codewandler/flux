//! `flux-auth` — authenticates *callers to flux* (distinct from `flux-credentials`, which
//! authenticates flux to LLM providers).
//!
//! An [`IdentityProvider`] resolves the caller into a `flux_policy::Caller` + `Trust` *before*
//! a session runs; policy then sees a typed actor. v1 ships [`LocalIdentity`] (the machine owner,
//! `Privileged` trust, no login). OIDC/multi-user is an optional provider added when flux runs as
//! a shared server — implement [`IdentityProvider`] and swap it in.

use flux_policy::{Caller, CallerKind, Principal, Scope, Trust, TrustKind, TrustLevel};

/// Resolves the caller behind a request.
pub trait IdentityProvider: Send + Sync {
    fn resolve(&self) -> (Caller, Trust);
}

/// The local single-user identity: the machine owner, trusted as `Privileged`.
pub struct LocalIdentity {
    user: String,
}

impl LocalIdentity {
    pub fn new(user: impl Into<String>) -> Self {
        Self { user: user.into() }
    }

    /// Derive from `$USER` (falling back to `"local"`).
    pub fn current() -> Self {
        let user = std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "local".to_string());
        Self { user }
    }
}

impl IdentityProvider for LocalIdentity {
    fn resolve(&self) -> (Caller, Trust) {
        let caller = Caller {
            principal: Principal {
                id: self.user.clone(),
                name: self.user.clone(),
                kind: CallerKind::User,
            },
            groups: Vec::new(),
            source: "local".to_string(),
        };
        let trust = Trust {
            kind: TrustKind::Invocation,
            level: TrustLevel::Privileged,
            scopes: Vec::new(),
        };
        (caller, trust)
    }
}

/// An identity resolved from **validated** OIDC claims — the seam for multi-user server
/// deployments. The deployment verifies the JWT / userinfo out of band (that's its IdP integration,
/// not flux's job) and constructs this from the resulting claims; `resolve` then yields the typed
/// `(Caller, Trust)` that the policy layer evaluates. This makes per-user authorization a drop-in:
/// swap `LocalIdentity` for `OidcIdentity` at the surface.
pub struct OidcIdentity {
    subject: String,
    name: String,
    groups: Vec<String>,
    trust_level: TrustLevel,
    scopes: Vec<Scope>,
}

impl OidcIdentity {
    /// Construct from validated claims. `trust_level` reflects how far the deployment trusts this
    /// IdP/user (e.g. `Verified`); `scopes` are the granted OAuth/OIDC scopes.
    pub fn from_claims(
        subject: impl Into<String>,
        name: impl Into<String>,
        groups: Vec<String>,
        trust_level: TrustLevel,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            name: name.into(),
            groups,
            trust_level,
            scopes: scopes.into_iter().map(Scope::from).collect(),
        }
    }
}

impl IdentityProvider for OidcIdentity {
    fn resolve(&self) -> (Caller, Trust) {
        let caller = Caller {
            principal: Principal {
                id: self.subject.clone(),
                name: self.name.clone(),
                kind: CallerKind::User,
            },
            groups: self.groups.clone(),
            source: "oidc".to_string(),
        };
        let trust = Trust {
            kind: TrustKind::Invocation,
            level: self.trust_level,
            scopes: self.scopes.clone(),
        };
        (caller, trust)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_identity_is_privileged_user() {
        let (caller, trust) = LocalIdentity::new("alice").resolve();
        assert_eq!(caller.principal.id, "alice");
        assert_eq!(caller.principal.kind, CallerKind::User);
        assert_eq!(caller.source, "local");
        assert_eq!(trust.level, TrustLevel::Privileged);
    }

    #[test]
    fn current_has_a_user() {
        let (caller, _) = LocalIdentity::current().resolve();
        assert!(!caller.principal.id.is_empty());
    }

    #[test]
    fn oidc_identity_resolves_claims_to_caller_and_trust() {
        let idp = OidcIdentity::from_claims(
            "auth0|abc",
            "Alice",
            vec!["team-eng".to_string()],
            TrustLevel::Verified,
            vec!["workspace:write".to_string()],
        );
        let (caller, trust) = idp.resolve();
        assert_eq!(caller.principal.id, "auth0|abc");
        assert_eq!(caller.principal.name, "Alice");
        assert_eq!(caller.source, "oidc");
        assert_eq!(caller.groups, vec!["team-eng"]);
        assert_eq!(trust.level, TrustLevel::Verified);
        assert_eq!(trust.scopes, vec![Scope::from("workspace:write")]);
    }
}
