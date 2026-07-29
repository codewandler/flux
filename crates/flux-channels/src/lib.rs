//! `flux-channels` — event-trigger **channels** for a flux-app [`Program`](flux_lang::program::Program).
//!
//! A *channel* is a long-running external surface. Most are **event sources** — a cron schedule, an
//! inbound webhook, or a Slack socket — that **wake a program on an external event**: they fire under
//! their **own name** as the bus label, so a `trigger { on = "<channel name>", run = "<journey>" }`
//! routes them to a journey. One is different: the **`a2a`** channel ([`A2aChannel`]) *serves* a
//! declared agent over the HTTP/A2A API (sessions + SSE + A2A + agent-card) by mounting
//! [`flux_server::router`] onto that agent's engine — the surface the old `flux serve` command provided.
//!
//! Channels are declared in the `.flux` program as ordinary
//! [`ChannelDecl`](flux_lang::program::ChannelDecl)s (a `kind` of `schedule`/`webhook`/`slack`/`a2a`
//! plus a `settings` bag); the app runner ([`crate::serve`]) starts them and — for event sources —
//! routes each event into the program's event bus via [`flux_app::App::deliver`].
//!
//! flux-app already owns the bus → triggers → journeys machinery; this crate only adds the external I/O
//! adapters (which carry the heavy deps — axum, a cron crate, a feature-gated Slack SDK) and a small host
//! that drives them against a running [`App`](flux_app::App). flux-app is unchanged.
//!
//! ## Concurrency
//! Deliveries run **concurrently**, and cross-channel parallelism is real: a schedule sweep, an
//! inbound webhook and a Slack message are three deliveries in flight at once, not a queue. The
//! shared [`flux_app::App`] isolates them — each delivery collects only the cascade its own
//! journeys `emit`, and spends only its own nesting budget (flux A-112) — so a channel adapter
//! needs no serialization of its own and may call [`flux_app::App::deliver`] from as many tasks as
//! it has events.
//!
//! Concurrent is **bounded**, not unlimited (flux A-129): the App admits at most
//! [`flux_app::DEFAULT_MAX_INFLIGHT_DELIVERIES`] deliveries at once (`FLUX_MAX_INFLIGHT_DELIVERIES`
//! overrides it). A delivery submitted past the bound **waits** — it is never dropped, so a webhook
//! this crate has already acknowledged still runs — and the wait propagates back into
//! [`flux_app::App::deliver`], which is where an adapter under a storm feels it. Below the bound
//! nothing changes: a sweep and an inbound webhook still run in parallel. An adapter that must not
//! block its own protocol loop should spawn its `deliver` call, and can tell backpressure from slow
//! journeys with [`flux_app::App::delivery_load`].
//!
//! What is *not* isolated is the program's own state: agent sessions, ask-parks and the
//! recorded-send log are shared across deliveries. Two events that address the same conversation
//! interleave; a journey that must not overlap with itself has to say so in the program.

mod adapters;
mod channel;
mod config;
mod deliver;
mod host;

#[cfg(feature = "slack")]
pub use adapters::SlackChannel;
pub use adapters::{build_channels, A2aChannel, ScheduleChannel, WebhookChannel};
pub use channel::Channel;
pub use deliver::{AppDeliverer, Deliverer};
pub use host::serve;
