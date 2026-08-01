//! Vendor-host disclosure at approval (C-311): say what an op reaches when flux is not dialing.
//!
//! # The trade-off this compensates for
//!
//! On the connectors seam a **deployment** holds the vendor credential, injects it, and calls the
//! vendor itself (see [`credential_boundary`](super::credential_boundary) for why that is the right
//! credential boundary). It costs one concrete thing:
//!
//! **`guard_url_scoped` only ever sees the deployment's base URL.** flux's per-vendor egress
//! allowlist — the control that says "this agent may reach `api.zendesk.com` and nothing else" —
//! stops constraining *which vendor* is reached, because from flux's side every platform-sourced
//! operation has the same destination. The platform's own manifest becomes that control instead.
//!
//! An approval prompt that says "call `connectors.zendesk-ticket-create`" while the operator cannot
//! see that this reaches `api.zendesk.com` is an approval given without the material fact. This
//! module is what puts the fact in front of them.
//!
//! # A disclosure, not an enforcement — and the two bounds that make it worth reading
//!
//! flux cannot verify a socket it does not open. What it *can* do is refuse to relay a claim it has
//! no way to bound, and [`validate_manifest_operations`](flux_plugin_protocol::validate_manifest_operations)
//! bounds this one twice, at load and at every refresh:
//!
//! - the declared value must be a **bare host**, never a URL — so "a token embedded in a URL by a
//!   hostile manifest" is unspellable rather than something a renderer must remember to strip;
//! - the host must be admitted by the manifest's **own declared HTTP host allowlist**, using the
//!   same matcher (`http_host_allows`) that gates the plugin's real egress. So the disclosure can
//!   only ever name a host the operator already reviewed at install, and which the approval prompt
//!   already renders as a `network.fetch` authority.
//!
//! What that leaves is stated rather than implied: a manifest declaring `http_hosts: ["*"]` has
//! declared unlimited egress, and its disclosure is bounded only by the host grammar. That is not a
//! hole this module opens — such a manifest already asks for, and already renders, unlimited reach.
//!
//! # Silence is a disclosure too
//!
//! [`VendorReach::Undeclared`] is **not** rendered as "reaches nothing". A platform-sourced op
//! whose manifest says nothing about its destination is the case the operator most needs to see,
//! and before this story it looked exactly like an op that reaches nothing at all. The three states
//! render as three distinct subjects, and only one of them is silence:
//!
//! | Declaration | Subject | Reads as |
//! |---|---|---|
//! | [`VendorReach::Host`] | `platform-reaches:api.zendesk.com` | a named vendor |
//! | [`VendorReach::Local`] | `platform-reaches:none` | the deployment serves it itself |
//! | [`VendorReach::Undeclared`] | `platform-reaches:UNDECLARED` | the manifest does not say |
//!
//! An operation that is **not** platform-sourced gets no subject at all: flux dials it, so
//! `guard_url_scoped` and the op's own `network.fetch` authority are already the disclosure, and a
//! second unverifiable story beside the enforced one would be worse than none.
//!
//! # Where it renders
//!
//! Two surfaces, reached by two different channels, because flux's two approval prompts read
//! different fields:
//!
//! - **the per-op prompt** renders `Tool::permission_subjects` — `StdinApprover` (the plain CLI /
//!   REPL) prints them beside the op name, and `flux-tui`'s approval sheet lists them as the body
//!   of the sheet. [`subject`] is what both of them show;
//! - **the whole-plan prompt** renders `PlanApprovalRequest::requirements`, never subjects (an
//!   approved plan skips the per-op gate, so the subject would never be seen). [`network_resource`]
//!   is what puts the destination into that set.
//!
//! Headless approvers (`AllowApprover`, `DenyApprover`, `RiskApprover`, `SubAgentApprover`) receive
//! both and render neither — there is no operator there to disclose to.

use super::*;

/// The subject prefix every vendor disclosure carries.
///
/// Deliberately says **platform**, not flux: the whole point of the disclosure is that flux is not
/// the process opening the socket, and a prompt line that read `network:api.zendesk.com` would
/// claim the opposite of the fact it exists to convey.
pub const SUBJECT_PREFIX: &str = "platform-reaches";

/// What an op whose manifest declares no destination discloses.
///
/// Uppercase on purpose. It shares a prompt line with hostnames, and the operator has to be able to
/// tell "the manifest did not say" from a host at a glance.
pub const UNDECLARED: &str = "UNDECLARED";

/// What an op that reaches no vendor at all discloses.
pub const NONE: &str = "none";

/// The permission subject disclosing this operation's real destination, or `None` when there is
/// nothing this story has to add.
///
/// `None` for [`PlatformSourcing::None`] — flux dials that op itself, so its destination is already
/// bound by `guard_url_scoped` and named by its own `network.fetch` authority. Adding a second,
/// manifest-authored claim beside an enforced one is how a disclosure becomes a decoy.
///
/// The returned value is safe to render verbatim: `validate_manifest_operations` refused the
/// manifest at load unless the host is a bare `host`/`host:port` drawn from the manifest's own
/// declared HTTP allowlist. Nothing here re-derives that — a second, weaker copy of the rule is
/// exactly how the enforced and the displayed answer drift apart.
pub fn subject(platform: PlatformSourcing, reaches: &VendorReach) -> Option<String> {
    if platform.is_none() {
        return None;
    }
    Some(match reaches {
        VendorReach::Host(host) => format!("{SUBJECT_PREFIX}:{host}"),
        VendorReach::Local => format!("{SUBJECT_PREFIX}:{NONE}"),
        VendorReach::Undeclared => format!("{SUBJECT_PREFIX}:{UNDECLARED}"),
    })
}

/// The `network.fetch` resource this operation's destination contributes to the **whole-plan**
/// approval preview, or `None` when it contributes none.
///
/// The plan prompt renders typed authority requirements and never permission subjects, so without
/// this a plan-approved batch would disclose nothing at all — and a plan approval is precisely the
/// case where the per-op prompt (and therefore [`subject`]) never fires.
///
/// Two deliberate choices:
///
/// - a declared host is named **plainly**, not prefixed. The resource id is what an operator's
///   policy matches on, so `network.fetch → api.zendesk.com` both reads as the destination it is
///   and dedupes against the identical entry the manifest's `http_hosts` already contributes. The
///   "who dials" nuance is carried by [`subject`] at the per-op prompt; at the plan prompt the fact
///   worth having is *that this plan causes a request to that vendor*, which is true either way;
/// - an **undeclared** destination gets a resource of its own rather than nothing, because "the
///   plan reaches somewhere nobody named" is the disclosure. It is spelled with the subject prefix
///   so it cannot collide with a real hostname, and a narrow egress policy that grants only named
///   vendors denies it — which is the correct answer to a destination the manifest would not state.
///
/// [`VendorReach::Local`] contributes nothing: no vendor is reached, so there is no fetch to
/// authorize.
pub fn network_resource(platform: PlatformSourcing, reaches: &VendorReach) -> Option<String> {
    if platform.is_none() {
        return None;
    }
    match reaches {
        VendorReach::Host(host) => Some(host.clone()),
        VendorReach::Local => None,
        VendorReach::Undeclared => Some(format!("{SUBJECT_PREFIX}:{UNDECLARED}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> VendorReach {
        VendorReach::Host(name.to_string())
    }

    #[test]
    fn an_op_flux_dials_itself_discloses_nothing_extra() {
        for reach in [
            VendorReach::Undeclared,
            VendorReach::Local,
            host("api.zendesk.com"),
        ] {
            assert_eq!(subject(PlatformSourcing::None, &reach), None);
            assert_eq!(network_resource(PlatformSourcing::None, &reach), None);
        }
    }

    /// The acceptance criterion in one assertion: an operation that declares no vendor host is
    /// disclosed *as such*, and "unknown destination" and "no destination" do not render alike.
    #[test]
    fn silence_and_no_destination_do_not_look_the_same() {
        let unknown = subject(PlatformSourcing::Operation, &VendorReach::Undeclared).unwrap();
        let nothing = subject(PlatformSourcing::Operation, &VendorReach::Local).unwrap();
        let vendor = subject(PlatformSourcing::Operation, &host("api.zendesk.com")).unwrap();
        assert_ne!(unknown, nothing);
        assert_ne!(unknown, vendor);
        assert_ne!(nothing, vendor);
        // And none of the three is empty or silent — an absent subject is what this story replaces.
        for disclosure in [&unknown, &nothing, &vendor] {
            assert!(disclosure.starts_with(SUBJECT_PREFIX), "{disclosure}");
        }
    }

    #[test]
    fn the_declared_vendor_host_is_what_the_operator_reads() {
        assert_eq!(
            subject(PlatformSourcing::Operation, &host("api.zendesk.com")).unwrap(),
            "platform-reaches:api.zendesk.com"
        );
        assert_eq!(
            subject(
                PlatformSourcing::Activation,
                &host("vendor.example.com:8443")
            )
            .unwrap(),
            "platform-reaches:vendor.example.com:8443"
        );
    }

    /// The plan surface reads requirements, not subjects. A named vendor becomes a plain network
    /// resource (so an operator's egress policy can match it); silence becomes a resource no
    /// hostname can collide with; a locally-served op contributes no fetch at all.
    #[test]
    fn the_plan_surface_names_the_destination_too() {
        assert_eq!(
            network_resource(PlatformSourcing::Operation, &host("api.zendesk.com")).as_deref(),
            Some("api.zendesk.com")
        );
        assert_eq!(
            network_resource(PlatformSourcing::Operation, &VendorReach::Undeclared).as_deref(),
            Some("platform-reaches:UNDECLARED")
        );
        assert_eq!(
            network_resource(PlatformSourcing::Operation, &VendorReach::Local),
            None
        );
    }
}
