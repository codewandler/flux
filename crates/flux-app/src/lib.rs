//! `flux-app` — the L6 runtime **host** that runs a multi-agent flux [`Program`].
//!
//! Where flux-flow (L3) runs *one* compiled flow per turn, flux-app runs a whole **program**: a
//! native flux-lang `.flux` file declaring agents, channels, datasources, triggers, and journeys. The
//! host turns those pure-data declarations into a live system:
//!
//! - an in-process **event bus** ([`Bus`]) — "user input is just an event"; channels inject events,
//!   journeys `emit` events, triggers route them;
//! - a **supervisor** ([`App::run`] / [`App::deliver`]) that, for each event, runs every journey whose
//!   [`trigger`](flux_lang::program::TriggerDecl) matches — by **reusing flux-flow's engine path**
//!   (`flux_flow::runtime::execute_flow` over a real [`Executor`](flux_runtime::Executor), so policy
//!   and approval still apply); and
//! - an **orchestration op-pack** (`emit` / `send` / `ask` / `spawn`) so a journey can fan out, talk to
//!   a channel, or run another journey — with **zero** new language node kinds (orchestration is just
//!   ops). See [`mod@ops`].
//!
//! [`Program`]: flux_lang::program::Program
//!
//! ## Quick shape
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use flux_app::App;
//! use flux_lang::program::Module;
//!
//! let program = match Module::parse_str(SRC)? {
//!     Module::Program(p) => p,
//!     Module::Flow(_) => unreachable!("a program file"),
//! };
//! let app = App::new(program, /* provider */ None, "model-id");
//! app.deliver("startup", serde_json::json!({})).await?; // run the {on:"startup"} journeys
//! # Ok(()) }
//! # const SRC: &str = "trigger boot\n  on \"startup\"\n  run hello\njourney hello\n  flow\n    return \"hi\"";
//! ```

mod app;
mod bus;
mod ops;
mod secrets;

pub use app::{App, JourneyRun, RecordingSink};
pub use bus::{Bus, Event, SentMessage};
pub use secrets::resolve_secrets;
