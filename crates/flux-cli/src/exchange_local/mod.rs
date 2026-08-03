// The bounded validator is intentionally integrated before the provider-owned archive reader. Keep
// its tests in the ordinary CLI gate while the coordinator-blocked wire layer remains absent.
#[allow(dead_code)]
pub(super) mod archive;
pub(super) mod command;
pub(super) mod status;
