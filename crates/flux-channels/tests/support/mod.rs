//! Shared scaffolding for the `flux-channels` integration suite.
//!
//! A plain module (`tests/support/mod.rs`, not its own test binary) included by a `tests/*.rs` file
//! via `mod support;`. Each test binary compiles it separately, so not every helper is used by every
//! includer — `dead_code` is silenced here rather than per-binary.
#![allow(dead_code)]

pub mod xmpp_double;
