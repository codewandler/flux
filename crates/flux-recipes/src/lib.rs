//! `flux-recipes` — a cookbook of reusable Flux-Lang flow **recipes**, authored with the Rust DSL.
//!
//! This is the first real in-repo consumer of [`flux_sdk`]: every recipe is a small, parameterized
//! function that builds a [`DraftAst`](dsl::DraftAst) out of the DSL primitives. You hand the result to
//! a [`flux_sdk::FlowClient`] to `analyze` + `execute` it through the real safety envelope.
//!
//! ## Roles — recipes vs the engine
//!
//! Keep two crates distinct:
//!
//! - **`flux-flow`** is the *engine* — the pure-DAG `compile → analyze → execute` lifecycle that **runs**
//!   a flow. [`flux_sdk::FlowClient`] wraps it.
//! - **`flux-recipes`** (this crate) is a *cookbook* — pre-built flows you **run on** that engine. It is
//!   content for the engine, not the engine.
//!
//! ## The recipes
//!
//! | Module | Primitive | Recipe |
//! |---|---|---|
//! | [`routing`] | `route` | [`routing::route_intent`] — classify once, then dispatch deterministically |
//! | [`lookup`] | `fallback` + `Answer` | [`lookup::answer_with_fallback`] — graceful degradation into a typed answer |
//! | [`batch`] | `each` / `repeat` / `loop` / `race` | [`batch::map_each`], [`batch::repeat_until`], [`batch::poll_for`], [`batch::race_first`] |
//!
//! ```no_run
//! use flux_recipes::dsl::*;
//! use flux_recipes::routing::route_intent;
//!
//! // route( classify(utterance) ) { case "book" -> booking.create ; default -> support.ticket }
//! let flow = route_intent(
//!     "intent.classify",
//!     lit("I'd like to book a flight"),
//!     &[("book", "booking.create")],
//!     "support.ticket",
//! );
//! // hand `flow` to a flux_sdk::FlowClient: client.analyze(&flow)?; client.execute(&flow).await?;
//! ```
//!
//! Recipes are a **construction** convenience, not a type-checker: semantic validity (op resolution,
//! bounded loops, `route` labels) stays the analyzer's job — always `analyze` a built flow before you
//! `execute` it.

#![warn(missing_docs)]

/// The Flux-Lang authoring DSL, re-exported so a consumer can `use flux_recipes::dsl::*` for the
/// expression free-functions (`call`/`var`/`lit`/…), the `Flow`/`Block` builders, and `DraftAst`/`Node`.
pub use flux_sdk::dsl;

pub mod batch;
pub mod lookup;
pub mod routing;
