//! `flux-channels` — event-trigger **channels** for a flux-app [`Program`](flux_lang::program::Program).
//!
//! A *channel* is a long-running event source — a cron schedule, an inbound webhook, or a Slack socket —
//! that **wakes a program on an external event**. Channels are declared in the `.flux` program as
//! ordinary [`ChannelDecl`](flux_lang::program::ChannelDecl)s (a `kind` of `schedule`/`webhook`/`slack`
//! plus a `settings` bag); the app runner ([`crate::serve`]) starts them and routes each event into the
//! program's event bus via [`flux_app::App::deliver`]. A channel fires its events under its **own name**
//! as the bus label, so a `trigger { on = "<channel name>", run = "<journey>" }` routes it to a journey.
//!
//! flux-app already owns the bus → triggers → journeys machinery; this crate only adds the external I/O
//! adapters (which carry the heavy deps — axum, a cron crate, a feature-gated Slack SDK) and a small host
//! that drives them against a running [`App`](flux_app::App). flux-app is unchanged.
//!
//! ## Concurrency
//! Deliveries are **serialized** ([`AppDeliverer`]): `App::deliver` subscribes to the broadcast bus and
//! drains the cascade events its journeys emit, so concurrent deliveries would double-process via
//! broadcast fan-out. Journeys themselves run on independent per-run stores, so this is the only
//! serialization point; cross-channel parallelism is a follow-up (needs per-delivery bus isolation).

mod adapters;
mod channel;
mod config;
mod deliver;
mod host;

#[cfg(feature = "slack")]
pub use adapters::SlackChannel;
pub use adapters::{build_channels, ScheduleChannel, WebhookChannel};
pub use channel::Channel;
pub use deliver::{AppDeliverer, Deliverer};
pub use host::serve;
