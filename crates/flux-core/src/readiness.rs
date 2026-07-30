//! The announcement a flux server prints once it is serving — one wording, owned by neither the
//! crate that prints it nor the crate that waits for it.
//!
//! `flux-server` (L6) prints it to stderr the moment its `bind` returns; `flux-orchestrate`'s
//! `fleet.start` (L3) scans a worker's captured stderr for it to decide the worker is live. The
//! layering rule forbids the consumer from importing the producer, so before C-277 each side spelled
//! the wording out as a private literal and **no test could pin the pair from either side**.
//!
//! The failure mode that motivates this is quiet, which is the whole problem: a rewording on the
//! producing side does not break a build or trip an assertion. `fleet.start` simply never sees its
//! marker, runs out its full 60-second readiness budget, and reports a worker that never announced
//! itself — which at the call site is indistinguishable from a slow or hung worker rather than the
//! broken contract it actually is. Living at L0, this module is a legal dependency for both sides,
//! so the wording now exists once and the drift cannot happen.
//!
//! **What the announcement proves, and what it does not.** It is the worker's own word that its
//! `bind` returned. It is *not* evidence that the endpoint is reachable from the coordinator: a
//! worker spawned into its own network namespace announces this exact line and is unreachable
//! anyway (C-243's netns finding). That gap is closed where it belongs — `fleet.start` refuses a
//! network-isolated posture up front — and not by strengthening this signal, because anything
//! stronger than "it bound" requires a request, and those ops deliberately declare no network
//! access at all.

/// The substring a readiness scanner matches. Deliberately the middle of the line rather than the
/// whole of it: the consumer greps an accumulated, possibly interleaved stderr buffer, not a single
/// tidy line, and the address is not known to it in the form the server renders.
pub const SERVING_MARKER: &str = "listening on http://";

/// The full line a server prints once bound to `addr`.
///
/// The one place this wording exists. Changing it here moves the producer and the consumer
/// together, which is the point; changing it at a call site instead is what the source-pin tests in
/// `flux-server` and `flux-orchestrate` exist to catch.
pub fn serving_announcement(addr: &str) -> String {
    format!("flux server {SERVING_MARKER}{addr}")
}

/// Whether `output` contains a server's serving announcement.
///
/// Takes a whole captured buffer rather than one line, because that is what the caller has: a
/// worker's stderr is drained in chunks and accumulated, so the announcement may arrive alongside
/// other output and must still be recognised.
pub fn announces_serving(output: &str) -> bool {
    output.contains(SERVING_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_announcement_is_recognised_by_the_matcher() {
        assert!(announces_serving(&serving_announcement("127.0.0.1:8790")));
    }

    #[test]
    fn the_announcement_carries_the_address_it_was_given() {
        assert!(serving_announcement("127.0.0.1:8790").ends_with("127.0.0.1:8790"));
    }

    /// The matcher scans an accumulated buffer, so the announcement must still be found when the
    /// worker has said other things before and after it.
    #[test]
    fn the_matcher_finds_an_announcement_amid_other_output() {
        let captured = format!(
            "resolving provider\n{}\ncwd=/tmp/x\n",
            serving_announcement("127.0.0.1:8791")
        );
        assert!(announces_serving(&captured));
    }

    #[test]
    fn unrelated_output_does_not_announce_serving() {
        assert!(!announces_serving(
            "flux server failed to bind 127.0.0.1:8790"
        ));
        assert!(!announces_serving(""));
    }
}
