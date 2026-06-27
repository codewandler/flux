//! Reusable Flux-Lang flow **recipes** — a cookbook of small, parameterized builders that compile to a
//! [`DraftAst`](crate::dsl::DraftAst) out of the DSL primitives.
//!
//! Each recipe is a plain function: hand its result to a [`FlowClient`](crate::FlowClient) to `analyze`
//! and `execute` through the real safety envelope. They are a *construction* convenience, not a
//! type-checker — always `analyze` a built flow before you `execute` it.
//!
//! ## Recipes vs the engine
//!
//! - [`flux-flow`](https://docs.rs/flux-flow) is the *engine* — the `compile → analyze → execute`
//!   lifecycle that **runs** a flow ([`FlowClient`](crate::FlowClient) wraps it).
//! - `recipes` is a *cookbook* — pre-built flows you **run on** that engine. Content for the engine, not
//!   the engine.
//!
//! ## The recipes
//!
//! | Module | Primitive | Recipe(s) |
//! |---|---|---|
//! | [`routing`] | `route` | [`routing::route_intent`] — classify once, then dispatch deterministically |
//! | [`lookup`] | `fallback` + `Answer` | [`lookup::answer_with_fallback`] — graceful degradation into a typed answer |
//! | [`batch`] | `each` / `repeat` / `loop` / `race` | [`batch::map_each`], [`batch::repeat_until`], [`batch::poll_for`], [`batch::race_first`] |
//!
//! ```no_run
//! use flux_sdk::dsl::*;
//! use flux_sdk::recipes::routing::route_intent;
//!
//! // route( classify(utterance) ) { case "book" -> booking.create ; default -> support.ticket }
//! let flow = route_intent(
//!     "intent.classify",
//!     lit("I'd like to book a flight"),
//!     &[("book", "booking.create")],
//!     "support.ticket",
//! );
//! // hand `flow` to a FlowClient: client.analyze(&flow)?; client.execute(&flow).await?;
//! ```

pub mod batch;
pub mod lookup;
pub mod routing;
