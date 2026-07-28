//! `flux-plugin` — the subprocess plugin protocol, host, and SDK.
//!
//! Plugins are native binaries in any language that speak a line-delimited JSON protocol over
//! stdio. Guest protocol/SDK types remain available without host dependencies; the default host
//! build also provides guarded capability handling, subprocess loading, hooks, and pack support.

#[cfg(feature = "host")]
use std::sync::Arc;

#[cfg(feature = "host")]
use async_trait::async_trait;
#[cfg(feature = "host")]
use base64::Engine as _;
#[cfg(feature = "host")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "host")]
use serde_json::{json, Value};
#[cfg(feature = "host")]
use sha2::{Digest as _, Sha256};

#[cfg(feature = "host")]
use flux_core::{Error, Result};
#[cfg(feature = "host")]
use flux_runtime::{
    authority_requirements_from_declaration, AuthorityRequirement, Tool, ToolContext, ToolResult,
};
#[cfg(feature = "host")]
use flux_spec::{AccessKind, Effect, FlowEffect, Idempotency, Risk, StagingDisposition, ToolSpec};
#[cfg(feature = "host")]
use flux_system::net::PrivateNetAllow;

mod protocol;
pub use protocol::*;

/// JavaScript pre-tool hooks (QuickJS via `rquickjs`).
#[cfg(feature = "hooks")]
pub mod hooks;
#[cfg(feature = "hooks")]
pub use hooks::JsHookEngine;

/// Host-terminated raw-socket authentication helpers.
#[cfg(feature = "host")]
mod pg;

/// Plugin pack distribution: resolve, verify, and install versioned artifacts.
#[cfg(feature = "pack")]
pub mod pack;

#[cfg(feature = "host")]
mod host;
#[cfg(feature = "pack")]
pub(crate) use host::invalid_plugin_name;
#[cfg(feature = "host")]
pub use host::*;
