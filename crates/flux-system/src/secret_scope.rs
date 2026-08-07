//! Per-secret scope — **where a named secret may go, and on whose behalf** (C-459).
//!
//! flux already scopes which secret may be *named*: `http.request`'s `$secret` allowlist refuses an
//! env-var name the operator did not opt in, before the value is read (C-76), and a plugin manifest
//! grants secret keys one at a time. It scoped nothing about **where a named secret may travel**.
//! [`net::guard_url_scoped`](crate::net::guard_url_scoped) decides whether *this caller* may reach
//! *this host*; it has never known which secret is in the request. Once a value is resolved it is an
//! ordinary string, bound for any host the caller may already reach.
//!
//! This module is the second half: a grant carries the destinations, principals and injection sites
//! it permits, and every axis it declares is **default-deny**.
//!
//! # The scope is matched against a vetted destination, never a hostname
//!
//! Matching a scope against the hostname the caller typed is a bypass, and the reason is the same
//! one [`net::guard_url_scoped_pinned`](crate::net::guard_url_scoped_pinned) exists: a DNS answer
//! that changes between the check and the connection sends the value somewhere the check never saw.
//! So the only thing this module will match against is a [`Destination`], and only
//! [`net::guard_url_scoped_for_secret`](crate::net::guard_url_scoped_for_secret) can mint one. It
//! carries the URL the egress guard resolved **together with the socket addresses it vetted** — an
//! empty set means the guard vetted nothing and produces no destination token rather than matching a
//! name whose address is still open. The same guard result carries the connection pins, so the
//! address the scope authorized is the address dialled.
//!
//! # Grant grammar
//!
//! One allowlist entry is an env-var name, optionally followed by `;`-separated scope parameters —
//! the shape a media type uses for its parameters, chosen because a bare name has to keep meaning
//! exactly what it meant before scoping existed:
//!
//! ```text
//! GITHUB_TOKEN                                          # unscoped — valid, and says so
//! GITHUB_TOKEN;to=api.github.com                        # this host only
//! GITHUB_TOKEN;to=*.github.com;in=header                # a suffix pattern, headers only
//! GITHUB_TOKEN;to=api.github.com;by=alice               # and only when alice is the principal
//! ```
//!
//! `to` takes the same host patterns as
//! [`PrivateNetAllow::from_hosts`](crate::net::PrivateNetAllow::from_hosts) (exact, `*.suffix`, or
//! `*`), `by` a principal id, `in` one of `header` / `query`. A repeated key unions, so two entries
//! naming the same secret widen it; an unrecognized key, an empty value or an empty name makes the
//! entry **unusable** — it still matches its own name and refuses every use, because an entry an
//! operator meant as a restriction must never degrade into an absent one.
//!
//! # An unscoped secret keeps working, and is visible as unscoped
//!
//! A bare `NAME` is [`SecretGrant::is_unscoped`], authorizes every use, and behaves exactly as it
//! did before this module existed. That is deliberate: breaking every existing `secret "NAME"` to
//! introduce scoping would guarantee nobody adopted it. It is not *silent*, though —
//! [`SecretGrant::describe`] names every axis including the open ones, and
//! [`SecretAllowlist::unscoped_names`] reports which entries carry no scope at all.

use std::fmt;
use std::net::SocketAddr;

use flux_core::{Error, Result};

use crate::net::host_matches;

// ---------------------------------------------------------------------------
// Destination
// ---------------------------------------------------------------------------

/// A destination a secret is about to be sent to: the host of a URL the egress guard has already
/// admitted, plus the socket addresses that guard vetted and the connection will be pinned to.
///
/// Minted only inside [`net::guard_url_scoped_for_secret`](crate::net::guard_url_scoped_for_secret),
/// so a scope can never be matched against a caller-asserted name/address pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    host: String,
    pinned: Vec<SocketAddr>,
}

impl Destination {
    /// Build a destination from the egress guard's own output.
    ///
    /// An **empty** `pinned` set is refused. Empty means the guard resolved nothing, so nothing was
    /// vetted: the host still has an open address, and authorizing a secret for it would be
    /// authorizing a name rather than a destination. `flux-web`'s `pinned_client` already refuses to
    /// connect unpinned for the same reason; this refuses to *authorize* unpinned, one step earlier.
    pub(crate) fn from_guard(url: &url::Url, pinned: &[SocketAddr]) -> Result<Self> {
        let host = url
            .host_str()
            .ok_or_else(|| Error::Other("the guarded url has no host".into()))?;
        if pinned.is_empty() {
            return Err(Error::Other(format!(
                "no address was vetted for `{host}`, so a secret scoped to a destination cannot be \
                 authorized for it: the egress guard resolved nothing, and matching the name alone \
                 would leave the address it reaches open"
            )));
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            pinned: pinned.to_vec(),
        })
    }

    /// The admitted host, lower-cased.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The addresses the guard vetted, which the connection is pinned to.
    pub fn pinned(&self) -> &[SocketAddr] {
        &self.pinned
    }
}

/// One correlated egress-guard result for a request that may carry a scoped secret.
///
/// The fields are private and construction is crate-private: external callers can obtain this only
/// from [`net::guard_url_scoped_for_secret`](crate::net::guard_url_scoped_for_secret). Keeping the
/// parsed URL, its exact connection pins and the destination token in one value prevents a caller
/// from claiming an arbitrary pair was guard-vetted.
#[derive(Debug)]
pub struct GuardedSecretTarget {
    url: url::Url,
    pinned: Vec<SocketAddr>,
    destination: Result<Destination>,
}

impl GuardedSecretTarget {
    pub(crate) fn from_guard(url: url::Url, pinned: Vec<SocketAddr>) -> Self {
        let destination = Destination::from_guard(&url, &pinned);
        Self {
            url,
            pinned,
            destination,
        }
    }

    /// Split the correlated result into the URL to send, the addresses to pin the connection to,
    /// and the destination token used for secret authorization.
    ///
    /// The token is an error only when the guard resolved no address. An unscoped grant does not
    /// inspect it, preserving the pre-C-459 behavior; the pinned HTTP client will then refuse the
    /// empty set before connecting.
    pub fn into_parts(self) -> (url::Url, Vec<SocketAddr>, Result<Destination>) {
        (self.url, self.pinned, self.destination)
    }

    /// The admitted URL, borrowed. The non-consuming counterpart to [`into_parts`](Self::into_parts),
    /// for a caller that has to *keep* the correlated value — a guarded HTTP request carries it to
    /// the substrate that will send it, and splitting it first would be handing over three values
    /// that are only trustworthy together.
    pub fn url(&self) -> &url::Url {
        &self.url
    }

    /// The addresses the guard vetted, borrowed. The connection must pin to exactly these.
    pub fn pinned(&self) -> &[SocketAddr] {
        &self.pinned
    }

    /// The destination token for secret authorization, borrowed, or the reason there is none.
    pub fn destination(&self) -> std::result::Result<&Destination, String> {
        self.destination
            .as_ref()
            .map_err(std::string::ToString::to_string)
    }

    /// Re-aim this admitted target at `url`, keeping the vetted addresses and destination token —
    /// refused unless the **authority** is unchanged.
    ///
    /// A caller that appends a query *after* admission (so a `$secret` can be authorized against the
    /// vetted destination before its value is ever read) still has to send the URL it built.
    /// Appending a percent-encoded query ahead of the fragment cannot move the authority — and this
    /// refuses rather than trusting that argument, because the alternative to checking is
    /// re-resolving, which is the very TOCTOU the pin closes.
    ///
    /// Neither URL is quoted in the refusal: a query-placed credential lives in one of them.
    pub fn with_url(mut self, url: url::Url) -> Result<Self> {
        fn authority(url: &url::Url) -> (&str, Option<&str>, Option<u16>) {
            (url.scheme(), url.host_str(), url.port_or_known_default())
        }
        if authority(&url) != authority(&self.url) {
            let admitted = self.url.host_str().unwrap_or("<no host>").to_string();
            let sent = url.host_str().unwrap_or("<no host>").to_string();
            return Err(Error::Other(format!(
                "the egress guard admitted `{admitted}` and the url to send names `{sent}`: \
                 refusing to send to an authority the guard never vetted"
            )));
        }
        self.url = url;
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// Injection site
// ---------------------------------------------------------------------------

/// Where in a request a secret is placed.
///
/// Vaults' `injection_location` names header, body or both, reasoning that a request body is the
/// broader exposure surface because it is assembled from content the agent is working with. flux's
/// shape differs: `http.request` accepts a `{"$secret": …}` marker **only** in `headers` and in the
/// `query` record — the `body` is a plain string with no marker resolution at all — so the body case
/// does not exist here. The axis that does exist is header versus query, and it is not cosmetic: a
/// query-placed credential lands in a URL, which is what proxies, gateways and access logs record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionSite {
    /// A request header.
    Header,
    /// A query-string parameter.
    Query,
}

impl InjectionSite {
    /// The spelling used by the `in=` grant parameter.
    pub fn as_str(&self) -> &'static str {
        match self {
            InjectionSite::Header => "header",
            InjectionSite::Query => "query",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "header" => Some(InjectionSite::Header),
            "query" => Some(InjectionSite::Query),
            _ => None,
        }
    }
}

impl fmt::Display for InjectionSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// The use being authorized
// ---------------------------------------------------------------------------

/// One about-to-happen use of a named secret, as the authorizing side sees it.
pub struct SecretUse<'a> {
    /// The vetted destination, or the reason there is not one.
    ///
    /// It is a `Result` rather than an `Option` so an unresolvable host keeps its own diagnostic,
    /// and — the point — so that **only a grant which declares a destination scope has to have
    /// one**. An unscoped secret bound for a host that fails to resolve behaves exactly as it did
    /// before this module existed: the request is attempted and fails at connect, not here.
    pub destination: std::result::Result<&'a Destination, String>,
    /// The principal this turn runs as, from the surface's `TurnIdentity`. `None` when the surface
    /// resolved no principal — which refuses any grant that declares `by=`, because "we do not know
    /// who this is" cannot satisfy "only alice".
    pub principal: Option<&'a str>,
    /// Where in the request the value is being placed.
    pub site: InjectionSite,
}

// ---------------------------------------------------------------------------
// Refusal
// ---------------------------------------------------------------------------

/// Why a secret was not authorized.
///
/// The two variants stay distinct because they are different operator actions: the first says "you
/// never opted this name in" (C-76's gate, checked before the value is read), the second says "you
/// opted it in, narrowly, and this use is outside that". A refusal never contains a secret value —
/// nothing on this path has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No entry names this secret at all.
    NotAllowlisted,
    /// Entries name it, but none of them authorizes this use. One reason per candidate grant.
    OutOfScope { reasons: Vec<String> },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotAllowlisted => f.write_str("it is not on the allowlist"),
            Refusal::OutOfScope { reasons } => f.write_str(&reasons.join("; also ")),
        }
    }
}

// ---------------------------------------------------------------------------
// Grant
// ---------------------------------------------------------------------------

/// One allowlist entry: an env-var name and the scope it carries.
///
/// Each axis is `None` when the entry leaves it open and `Some(non-empty)` when it constrains it —
/// there is no "declared but empty" state, so an axis can never be narrowed into accidentally
/// allowing everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretGrant {
    name: String,
    destinations: Option<Vec<String>>,
    principals: Option<Vec<String>>,
    sites: Option<Vec<InjectionSite>>,
    /// `Some` when the entry did not parse. The grant keeps its name — so the operator's intent to
    /// restrict this secret is still felt — and refuses every use, quoting the reason.
    unusable: Option<String>,
}

impl SecretGrant {
    /// Parse one allowlist entry. Total: a malformed entry becomes a grant that refuses rather than
    /// an error the caller could drop, because dropping it would silently widen the allowlist to
    /// "this name is simply absent" and an absent name reads as an operator decision.
    pub fn parse(entry: &str) -> Self {
        let entry = entry.trim();
        let mut parts = entry.split(';');
        let name = parts.next().unwrap_or_default().trim().to_string();
        let mut grant = Self {
            name: name.clone(),
            destinations: None,
            principals: None,
            sites: None,
            unusable: None,
        };
        if name.is_empty() {
            grant.unusable = Some(format!("the entry {entry:?} names no environment variable"));
            return grant;
        }
        for part in parts {
            let part = part.trim();
            let Some((key, value)) = part.split_once('=') else {
                grant.unusable = Some(format!(
                    "the scope parameter {part:?} is not `key=value` (expected `to=`, `by=` or \
                     `in=`)"
                ));
                return grant;
            };
            let value = value.trim();
            if value.is_empty() {
                grant.unusable = Some(format!("the scope parameter {part:?} has an empty value"));
                return grant;
            }
            match key.trim().to_ascii_lowercase().as_str() {
                "to" => grant
                    .destinations
                    .get_or_insert_with(Vec::new)
                    .push(value.to_ascii_lowercase()),
                "by" => grant
                    .principals
                    .get_or_insert_with(Vec::new)
                    .push(value.to_string()),
                "in" => match InjectionSite::parse(value) {
                    Some(site) => grant.sites.get_or_insert_with(Vec::new).push(site),
                    None => {
                        grant.unusable = Some(format!(
                            "`in={value}` is not an injection site (expected `header` or `query`)"
                        ));
                        return grant;
                    }
                },
                other => {
                    grant.unusable = Some(format!(
                        "`{other}` is not a scope parameter (expected `to`, `by` or `in`)"
                    ));
                    return grant;
                }
            }
        }
        grant
    }

    /// The environment-variable name this grant covers.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this grant constrains nothing — the bare `NAME` spelling that behaves exactly as it
    /// did before scoping existed. An unusable entry is **not** unscoped: it constrains everything.
    pub fn is_unscoped(&self) -> bool {
        self.unusable.is_none()
            && self.destinations.is_none()
            && self.principals.is_none()
            && self.sites.is_none()
    }

    /// A one-line rendering naming **every** axis, open ones included — so reading the allowlist
    /// tells an operator what is unconstrained without having to notice an absence.
    pub fn describe(&self) -> String {
        if let Some(reason) = &self.unusable {
            return format!("{} (unusable, refuses every use: {reason})", self.name);
        }
        let to = match &self.destinations {
            Some(hosts) => format!("to {}", hosts.join(" or ")),
            None => "to any destination".to_string(),
        };
        let by = match &self.principals {
            Some(ids) => format!("by {}", ids.join(" or ")),
            None => "by any principal".to_string(),
        };
        let sites = match &self.sites {
            Some(sites) => format!(
                "in {}",
                sites
                    .iter()
                    .map(InjectionSite::as_str)
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            None => "in header or query".to_string(),
        };
        let unscoped = if self.is_unscoped() { "unscoped: " } else { "" };
        format!("{} ({unscoped}{to}; {by}; {sites})", self.name)
    }

    /// Whether this grant authorizes `use_`, and if not, why. Every declared axis is default-deny.
    fn authorize(&self, use_: &SecretUse<'_>) -> std::result::Result<(), String> {
        if let Some(reason) = &self.unusable {
            return Err(format!(
                "the allowlist entry for `{}` cannot be read, so it authorizes nothing: {reason}",
                self.name
            ));
        }
        if let Some(patterns) = &self.destinations {
            let destination = use_
                .destination
                .as_ref()
                .map_err(|why| format!("`{}` is scoped to a destination, but {why}", self.name))?;
            if !host_matches(patterns, destination.host()) {
                return Err(format!(
                    "`{}` is scoped to {} and this request goes to {}",
                    self.name,
                    patterns.join(" or "),
                    destination.host()
                ));
            }
        }
        if let Some(ids) = &self.principals {
            match use_.principal {
                Some(principal) if ids.iter().any(|id| id == principal) => {}
                Some(principal) => {
                    return Err(format!(
                        "`{}` may only be used by {} and this turn runs as {principal}",
                        self.name,
                        ids.join(" or ")
                    ))
                }
                None => {
                    return Err(format!(
                        "`{}` may only be used by {}, and this surface resolved no principal for \
                         the turn",
                        self.name,
                        ids.join(" or ")
                    ))
                }
            }
        }
        if let Some(sites) = &self.sites {
            if !sites.contains(&use_.site) {
                return Err(format!(
                    "`{}` may only be placed in {} and this request places it in the {}",
                    self.name,
                    sites
                        .iter()
                        .map(InjectionSite::as_str)
                        .collect::<Vec<_>>()
                        .join(" or "),
                    use_.site
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------------

/// The parsed `$secret` allowlist: which env-var names may be resolved at all, and the scope each
/// one carries. Deny-all when empty — the fail-closed default C-76 established.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretAllowlist {
    grants: Vec<SecretGrant>,
}

impl SecretAllowlist {
    /// Parse a list of entries. Two entries naming the same secret **union**: a use one of them
    /// authorizes is authorized, which is how a secret reaches two hosts without inventing a
    /// separator inside `to=`.
    pub fn parse<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            grants: entries
                .into_iter()
                .map(|entry| SecretGrant::parse(entry.as_ref()))
                .collect(),
        }
    }

    /// Whether the allowlist admits nothing at all.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Every parsed grant, in the order the operator wrote them.
    pub fn grants(&self) -> &[SecretGrant] {
        &self.grants
    }

    /// The names carrying no scope at all. An unscoped secret stays valid on purpose; this is what
    /// makes it legible rather than silent.
    pub fn unscoped_names(&self) -> Vec<&str> {
        self.grants
            .iter()
            .filter(|grant| grant.is_unscoped())
            .map(SecretGrant::name)
            .collect()
    }

    /// Whether **any** entry names this secret. Separate from [`authorize`](Self::authorize) so a
    /// caller can keep C-76's ordering — refuse a name the operator never opted in *before* reading
    /// its value, and only then ask whether this particular use is in scope.
    pub fn names(&self, name: &str) -> bool {
        self.grants.iter().any(|grant| grant.name == name)
    }

    /// Authorize one use of `name`. `Ok` when some grant covering that name permits it.
    pub fn authorize(&self, name: &str, use_: &SecretUse<'_>) -> std::result::Result<(), Refusal> {
        let mut reasons = Vec::new();
        for grant in self.grants.iter().filter(|grant| grant.name == name) {
            match grant.authorize(use_) {
                Ok(()) => return Ok(()),
                Err(reason) => reasons.push(reason),
            }
        }
        if reasons.is_empty() {
            Err(Refusal::NotAllowlisted)
        } else {
            Err(Refusal::OutOfScope { reasons })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn destination(host: &str) -> Destination {
        let url = url::Url::parse(&format!("https://{host}/p")).unwrap();
        let pinned = [SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            443,
        )];
        Destination::from_guard(&url, &pinned).unwrap()
    }

    fn use_at<'a>(destination: &'a Destination) -> SecretUse<'a> {
        SecretUse {
            destination: Ok(destination),
            principal: None,
            site: InjectionSite::Header,
        }
    }

    /// The destination axis, both directions: a declared scope is default-deny, and it admits the
    /// host it names.
    #[test]
    fn a_declared_destination_scope_admits_its_host_and_refuses_every_other() {
        let list = SecretAllowlist::parse(["GITHUB_TOKEN;to=api.github.com"]);
        assert!(list
            .authorize("GITHUB_TOKEN", &use_at(&destination("api.github.com")))
            .is_ok());
        let refusal = list
            .authorize("GITHUB_TOKEN", &use_at(&destination("attacker.example")))
            .expect_err("a host outside the scope is refused");
        assert!(
            matches!(&refusal, Refusal::OutOfScope { .. }),
            "an in-allowlist name out of scope is not the same refusal as an unknown name: \
             {refusal:?}"
        );
        assert!(
            refusal.to_string().contains("attacker.example")
                && refusal.to_string().contains("api.github.com"),
            "the refusal names both the destination and the scope: {refusal}"
        );
        // A name nobody granted is the other refusal, and it stays distinguishable.
        assert_eq!(
            list.authorize("OTHER", &use_at(&destination("api.github.com"))),
            Err(Refusal::NotAllowlisted)
        );
    }

    /// The `*.suffix` spelling is the guard's own host-pattern vocabulary, so a scope and a
    /// private-net grant can never disagree about what a pattern means.
    #[test]
    fn a_destination_scope_uses_the_guards_host_patterns() {
        let list = SecretAllowlist::parse(["T;to=*.github.com"]);
        assert!(list
            .authorize("T", &use_at(&destination("api.github.com")))
            .is_ok());
        assert!(list
            .authorize("T", &use_at(&destination("uploads.api.github.com")))
            .is_ok());
        // The suffix rule requires a real label boundary — `evilgithub.com` is not `*.github.com`.
        assert!(list
            .authorize("T", &use_at(&destination("evilgithub.com")))
            .is_err());
        assert!(list
            .authorize("T", &use_at(&destination("github.com")))
            .is_err());
    }

    /// ⚠ The bypass this module exists to refuse. A scope matched against a hostname nobody resolved
    /// is a scope over a name whose address is still free to move, so there is no way to construct
    /// the destination at all — and a destination-scoped grant refuses rather than falling back to
    /// the name.
    #[test]
    fn a_destination_with_no_vetted_address_cannot_be_built_and_refuses_a_scoped_secret() {
        let url = url::Url::parse("https://unresolvable.example/p").unwrap();
        let err = Destination::from_guard(&url, &[])
            .expect_err("an empty pin set means nothing was vetted")
            .to_string();
        assert!(
            err.contains("unresolvable.example") && err.contains("vetted"),
            "the diagnostic says what is missing: {err}"
        );

        let unvetted = SecretUse {
            destination: Err(err),
            principal: None,
            site: InjectionSite::Header,
        };
        let refusal = SecretAllowlist::parse(["T;to=api.github.com"])
            .authorize("T", &unvetted)
            .expect_err("a destination-scoped secret needs a vetted destination");
        assert!(
            refusal.to_string().contains("scoped to a destination"),
            "{refusal}"
        );

        // …and the other half of the bargain: an UNSCOPED secret is unaffected by the same
        // unresolvable host, because it never asks where it is going.
        assert!(SecretAllowlist::parse(["T"])
            .authorize("T", &unvetted)
            .is_ok());
    }

    /// The principal axis. `by=` is default-deny, and "the surface resolved nobody" is a refusal,
    /// not a wildcard — a shared surface with an unidentified speaker is exactly the case the axis
    /// exists for.
    #[test]
    fn a_principal_scope_refuses_a_stranger_and_an_unidentified_turn() {
        let list = SecretAllowlist::parse(["T;by=alice"]);
        let dest = destination("api.github.com");
        let with = |principal| SecretUse {
            destination: Ok(&dest),
            principal,
            site: InjectionSite::Header,
        };
        assert!(list.authorize("T", &with(Some("alice"))).is_ok());
        let stranger = list
            .authorize("T", &with(Some("bob")))
            .expect_err("bob is not alice");
        assert!(stranger.to_string().contains("bob"), "{stranger}");
        let anonymous = list
            .authorize("T", &with(None))
            .expect_err("an unresolved principal cannot satisfy `by=`");
        assert!(
            anonymous.to_string().contains("resolved no principal"),
            "{anonymous}"
        );
    }

    /// The injection-site axis: flux has no body path for a `$secret`, so the axis worth having is
    /// header versus query — a query-placed credential lands in a URL that proxies and access logs
    /// keep.
    #[test]
    fn an_injection_site_scope_separates_a_header_from_a_query_parameter() {
        let list = SecretAllowlist::parse(["T;in=header"]);
        let dest = destination("api.github.com");
        let at = |site| SecretUse {
            destination: Ok(&dest),
            principal: None,
            site,
        };
        assert!(list.authorize("T", &at(InjectionSite::Header)).is_ok());
        let refusal = list
            .authorize("T", &at(InjectionSite::Query))
            .expect_err("a header-only secret may not go in the query string");
        assert!(refusal.to_string().contains("query"), "{refusal}");
        // Both sites, declared explicitly, admit both.
        let both = SecretAllowlist::parse(["T;in=header;in=query"]);
        assert!(both.authorize("T", &at(InjectionSite::Query)).is_ok());
    }

    /// Two entries for one name union, which is how a secret reaches a second host without a
    /// separator inside `to=` that would have to be escaped in an env var.
    #[test]
    fn two_entries_for_one_name_union() {
        let list = SecretAllowlist::parse(["T;to=a.example", "T;to=b.example"]);
        assert!(list
            .authorize("T", &use_at(&destination("a.example")))
            .is_ok());
        assert!(list
            .authorize("T", &use_at(&destination("b.example")))
            .is_ok());
        let refusal = list
            .authorize("T", &use_at(&destination("c.example")))
            .expect_err("neither entry covers c.example");
        let text = refusal.to_string();
        assert!(
            text.contains("a.example") && text.contains("b.example"),
            "every candidate grant reports its own reason: {text}"
        );
    }

    /// ⚠ A malformed entry must not degrade into an absent one. An operator who wrote a restriction
    /// and misspelled it has said "restrict this"; reading that as "this name was never granted"
    /// would be a different, quieter answer to a different question.
    #[test]
    fn a_malformed_entry_refuses_rather_than_disappearing() {
        for (entry, needle) in [
            ("T;host=api.github.com", "`host` is not a scope parameter"),
            ("T;to=", "empty value"),
            ("T;api.github.com", "is not `key=value`"),
            ("T;in=body", "not an injection site"),
        ] {
            let list = SecretAllowlist::parse([entry]);
            let refusal = list
                .authorize("T", &use_at(&destination("api.github.com")))
                .expect_err("a malformed entry must refuse every use");
            let text = format!("{refusal:?}");
            assert!(
                text.contains("OutOfScope"),
                "a malformed entry still claims its name, so this is not `NotAllowlisted`: {text}"
            );
            assert!(
                list.grants()[0].describe().contains(needle),
                "the reason is legible: {}",
                list.grants()[0].describe()
            );
            assert!(!list.grants()[0].is_unscoped(), "unusable is not unscoped");
        }
        // A nameless entry is unusable too, and cannot be reached by any name.
        assert!(SecretAllowlist::parse([";to=a"]).grants()[0]
            .describe()
            .contains("names no environment variable"));
    }

    /// An unscoped grant keeps working, and says so. Both halves matter: the first is why anyone
    /// would adopt scoping incrementally, the second is why a half-scoped allowlist is not a false
    /// sense of safety.
    #[test]
    fn an_unscoped_grant_authorizes_everything_and_is_legible_as_unscoped() {
        let list = SecretAllowlist::parse(["PLAIN", "SCOPED;to=api.github.com"]);
        assert!(list
            .authorize("PLAIN", &use_at(&destination("anywhere.example")))
            .is_ok());
        assert_eq!(list.unscoped_names(), vec!["PLAIN"]);
        assert_eq!(
            list.grants()[0].describe(),
            "PLAIN (unscoped: to any destination; by any principal; in header or query)"
        );
        // Every axis is named even when only one is constrained — an open axis is stated, not
        // inferred from a blank.
        assert_eq!(
            list.grants()[1].describe(),
            "SCOPED (to api.github.com; by any principal; in header or query)"
        );
        assert!(list.names("PLAIN") && !list.names("ABSENT"));
        assert!(SecretAllowlist::default().is_empty());
    }

    /// Whitespace around an entry and its parameters is an operator's formatting, not part of a
    /// host or a name.
    #[test]
    fn entries_tolerate_surrounding_whitespace_and_case() {
        let list = SecretAllowlist::parse([" T ; TO = API.GitHub.com ; In = Header "]);
        assert!(list.names("T"));
        let dest = destination("api.github.com");
        assert!(list
            .authorize(
                "T",
                &SecretUse {
                    destination: Ok(&dest),
                    principal: None,
                    site: InjectionSite::Header,
                }
            )
            .is_ok());
    }

    /// The vetted destination carries the addresses forward, so a caller can pin the connection to
    /// exactly what the scope was matched against rather than re-resolving.
    #[test]
    fn a_vetted_destination_keeps_the_addresses_it_was_authorized_against() {
        let url = url::Url::parse("https://api.github.com/p").unwrap();
        let pinned = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(140, 82, 121, 6)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(140, 82, 121, 5)), 443),
        ];
        let destination = Destination::from_guard(&url, &pinned).unwrap();
        assert_eq!(destination.host(), "api.github.com");
        assert_eq!(destination.pinned(), pinned);
        // A URL with no host cannot be a destination.
        let opaque = url::Url::parse("data:text/plain,hi").unwrap();
        assert!(Destination::from_guard(&opaque, &pinned).is_err());
    }
}
