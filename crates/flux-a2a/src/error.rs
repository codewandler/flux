//! The A2A JSON-RPC error codes (the `-32001..-32007` binding set).
//!
//! JSON-RPC 2.0 reserves `-32000..-32099` for server-defined errors; the A2A protocol assigns this
//! block specific meanings in its JSON-RPC binding (the codes the `a2a-python` / `a2a-js` SDKs
//! emit and decode — the protobuf spec view lists the names without numbers). flux emits the ones
//! that carry more meaning than a generic base code on an already-failing path: [`UNSUPPORTED_OPERATION`]
//! for a *known* method flux chose not to implement (vs a blanket `-32601`), and
//! [`CONTENT_TYPE_NOT_SUPPORTED`] for an inbound message with no usable text part (vs silently
//! running an empty turn). The task-lifecycle codes ([`TASK_NOT_FOUND`], [`TASK_NOT_CANCELABLE`],
//! [`PUSH_NOTIFICATION_NOT_SUPPORTED`]) presuppose an addressable, retained task and land with the
//! stateful task model (A-53); they are defined here so both dispatchers name one set of constants.
//!
//! These are `i32` to match the JSON-RPC envelope's `code` field ([`crate::server::rpc_err`]).

/// `-32001` — the referenced task id does not exist. (Task-lifecycle; needs the retained-task model.)
pub const TASK_NOT_FOUND: i32 = -32001;

/// `-32002` — the referenced task is in a state that cannot be canceled. (Task-lifecycle.)
pub const TASK_NOT_CANCELABLE: i32 = -32002;

/// `-32003` — the agent does not support push notifications. (Task-lifecycle.)
pub const PUSH_NOTIFICATION_NOT_SUPPORTED: i32 = -32003;

/// `-32004` — the operation is recognized by the A2A protocol but not implemented by this agent.
/// More accurate than a generic `-32601 Method not found`, which should be reserved for a
/// genuinely-unrecognized method name.
pub const UNSUPPORTED_OPERATION: i32 = -32004;

/// `-32005` — the agent cannot use the content type(s) supplied. flux returns this when an inbound
/// message carries parts but none is text (it accepts only text input today), instead of running
/// the turn on empty input.
pub const CONTENT_TYPE_NOT_SUPPORTED: i32 = -32005;

/// `-32006` — the agent produced a response that does not conform to the spec. (Reserved.)
pub const INVALID_AGENT_RESPONSE: i32 = -32006;

/// `-32007` — no authenticated extended card is configured for this agent. (Reserved; the
/// extended-card method itself is a later slice.)
pub const AUTHENTICATED_EXTENDED_CARD_NOT_CONFIGURED: i32 = -32007;
