//! C-321 — **an empty shared secret is functionally `ServerAuth::Open`, and must not reach a public
//! bind.**
//!
//! This is the *bind* half of the story, deliberately separated from the request half (the 401 test
//! in `src/lib.rs`): asserting only the 401 would leave the actual severity — a listener on a
//! non-loopback interface, in front of a daemon that auto-approves every tool call — unobserved.
//!
//! The fixture binds a **real** non-loopback socket rather than parsing an address string, so the
//! address under test is the one an `[a2a]` channel would actually serve on, and then asks
//! [`flux_server::router_in`] (the single construction-time enforcement point every serving path
//! shares, C-190 — `router` reaches it by delegation) whether that combination may be built. When
//! the guard is missing the test does not
//! merely assert `is_err()` and stop — it goes on to *demonstrate* the exposure by serving the
//! router it was handed and driving an anonymous request through it, so the failure message is the
//! vulnerability report rather than a bare `assertion failed`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use tower::ServiceExt; // for `oneshot`

mod support;

/// The `[a2a]` channel's exact auth mapping for `token ""` (or `token secret "K"` with `K` exported
/// empty): `Some("")` → `SharedSecret { secret: "" }`, never `Open`.
fn empty_shared_secret() -> flux_server::ServerAuth {
    flux_server::ServerAuth::shared_secret(Some(String::new()), None)
}

#[tokio::test]
async fn an_empty_shared_secret_may_not_bind_a_public_address() {
    // A real listener on every interface — the same bind `A2aChannel::start` performs before it
    // mounts `flux_server::router` onto it.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    assert!(
        !addr.ip().is_loopback(),
        "fixture must exercise a non-loopback bind, got {addr}"
    );

    let engine = support::test_engine(Arc::new(support::ProseProvider));
    // `router_in` is the same construction-time enforcement point (`router` delegates to it), so
    // pinning the config env here does not move the guard under test (C-392).
    let built = flux_server::router_in(
        engine,
        empty_shared_secret(),
        flux_server::CardInfo::flux_coding(),
        addr,
        &support::pinned_env(),
    );

    let router = match built {
        Err(e) => {
            // The refusal we want. Pin the message so a *different* construction failure (a
            // malformed fixture, say) cannot masquerade as the guard firing.
            let msg = e.to_string();
            assert!(
                msg.contains("empty") && msg.contains(&addr.to_string()),
                "the refusal must name the empty secret and the address it refused; got: {msg}"
            );
            return;
        }
        Ok(router) => router,
    };

    // No guard: prove the exposure is live rather than theoretical before failing. The router just
    // built is the one that would be served on `addr`; drive an anonymous request through it.
    let status = router
        .oneshot(
            HttpRequest::get("/sessions/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    // The state a reader reaches by performing this story's guard-only mutation: `guard_open_bind`
    // let an empty shared secret build a router for a public bind — the defect this test names —
    // and only `require_auth`'s independent empty-secret check then stopped the anonymous request.
    // The second half doing its job is not the first half being fixed.
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "an empty shared secret built a router for the non-loopback bind {addr}: the public-bind \
         refusal is missing. The `Authorization`-less request that followed was caught by \
         `require_auth`'s empty-secret check (401), not admitted — so the compare half is intact \
         and the bind half is not. `guard_open_bind` must refuse `SharedSecret {{ secret: \"\" }}` \
         exactly as it refuses `Open`"
    );
    panic!(
        "an empty shared secret built a router for the non-loopback bind {addr}, and an \
         `Authorization`-less request past its auth layer answered {status} instead of 401 — \
         `SharedSecret {{ secret: \"\" }}` is functionally `Open`, so `guard_open_bind` must refuse \
         it exactly as it refuses `Open`"
    );
}

/// The loopback counterpart: an empty secret is refused *before* a port is bound is a `flux-channels`
/// concern, but flux-server must at minimum not treat the empty-secret mode as authenticated when it
/// decides whether a bind may be public. A real secret on the same public address still builds — the
/// guard keys on the *property* (nothing to present), not on "shared-secret mode is suspicious".
#[tokio::test]
async fn a_real_shared_secret_still_binds_a_public_address() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let engine = support::test_engine(Arc::new(support::ProseProvider));
    let _router = flux_server::router_in(
        engine,
        flux_server::ServerAuth::shared_secret(Some("s3cr3t".into()), None),
        flux_server::CardInfo::flux_coding(),
        addr,
        &support::pinned_env(),
    )
    .expect("a non-empty shared secret is real authentication; the public bind must still build");
}
