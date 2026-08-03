// This module deliberately defines the whole v1 state/diagnostic vocabulary before the effectful
// lifecycle backend is integrated; production currently constructs only `foreign_or_stale`.
#![allow(dead_code)]

use serde::Serialize;

pub(super) const STATUS_SCHEMA: &str = "flux.exchange-local-status.v1";
pub(crate) const USAGE_EXIT: i32 = 64;
pub(super) const INTERNAL_FAILURE_EXIT: i32 = 70;
const MAX_DIAGNOSTICS: usize = 8;

/// The exhaustive public lifecycle state. Its discriminants are the stable wire spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LocalState {
    NotInstalled,
    InstallVerificationRefused,
    Stopped,
    Starting,
    Healthy,
    Incompatible,
    Unhealthy,
    ForeignOrStale,
    StopFailure,
}

impl LocalState {
    pub(super) const fn exit_code(self) -> i32 {
        match self {
            Self::Healthy => 0,
            Self::NotInstalled => 20,
            Self::Stopped => 21,
            Self::Starting => 22,
            Self::InstallVerificationRefused => 23,
            Self::Incompatible => 24,
            Self::Unhealthy => 25,
            Self::ForeignOrStale => 26,
            Self::StopFailure => 27,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::InstallVerificationRefused => "install_verification_refused",
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Healthy => "healthy",
            Self::Incompatible => "incompatible",
            Self::Unhealthy => "unhealthy",
            Self::ForeignOrStale => "foreign_or_stale",
            Self::StopFailure => "stop_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AcceptedChannel {
    pub(super) name: String,
    pub(super) trust_version: String,
    pub(super) trust_sha256: String,
    pub(super) generation: u64,
    pub(super) index_sha256: String,
    pub(super) expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct InstalledRelease {
    pub(super) tag: String,
    pub(super) version: String,
    pub(super) source_commit: String,
    pub(super) build_id: String,
    pub(super) target: String,
    pub(super) manifest_sha256: String,
    pub(super) executable_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct VerifiedEndpoint {
    pub(super) scheme: String,
    pub(super) host: String,
    pub(super) port: u16,
}

/// Codes emitted only with `component=install`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallDiagnostic {
    TrustInvalid,
    TrustExpired,
    TrustRollback,
    ChannelInvalid,
    ChannelExpired,
    ChannelRollback,
    ManifestMissing,
    SignatureInvalid,
    SigningKeyUnknown,
    OriginRefused,
    ArchiveInvalid,
    ExecutableInvalid,
    CachePermissions,
}

/// Codes emitted only with `component=supervisor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SupervisorDiagnostic {
    SupervisorMismatch,
    ReadinessTimeout,
    ReadinessInvalid,
    BindMismatch,
    ChildExited,
    TerminateFailed,
}

/// Codes emitted only with `component=control`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlDiagnostic {
    ControlUnavailable,
    ControlAuthFailed,
}

/// Codes emitted only with `component=exchange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExchangeDiagnostic {
    HealthFailed,
    ProtocolIncompatible,
}

/// A diagnostic is component-typed, making an invalid component/code pair unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Diagnostic {
    kind: DiagnosticKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticKind {
    Install(InstallDiagnostic),
    Supervisor(SupervisorDiagnostic),
    Control(ControlDiagnostic),
    Exchange(ExchangeDiagnostic),
    Truncated,
}

impl Diagnostic {
    pub(super) const fn install(code: InstallDiagnostic) -> Self {
        Self {
            kind: DiagnosticKind::Install(code),
        }
    }

    pub(super) const fn supervisor(code: SupervisorDiagnostic) -> Self {
        Self {
            kind: DiagnosticKind::Supervisor(code),
        }
    }

    pub(super) const fn control(code: ControlDiagnostic) -> Self {
        Self {
            kind: DiagnosticKind::Control(code),
        }
    }

    pub(super) const fn exchange(code: ExchangeDiagnostic) -> Self {
        Self {
            kind: DiagnosticKind::Exchange(code),
        }
    }

    /// Available only to the bounded constructor, so a backend cannot claim truncation when the
    /// cap was not actually crossed.
    const fn truncated() -> Self {
        Self {
            kind: DiagnosticKind::Truncated,
        }
    }

    pub(super) const fn component(self) -> &'static str {
        match self.kind {
            DiagnosticKind::Install(_) => "install",
            DiagnosticKind::Supervisor(_) | DiagnosticKind::Truncated => "supervisor",
            DiagnosticKind::Control(_) => "control",
            DiagnosticKind::Exchange(_) => "exchange",
        }
    }

    pub(super) const fn code(self) -> &'static str {
        match self.kind {
            DiagnosticKind::Install(code) => match code {
                InstallDiagnostic::TrustInvalid => "trust_invalid",
                InstallDiagnostic::TrustExpired => "trust_expired",
                InstallDiagnostic::TrustRollback => "trust_rollback",
                InstallDiagnostic::ChannelInvalid => "channel_invalid",
                InstallDiagnostic::ChannelExpired => "channel_expired",
                InstallDiagnostic::ChannelRollback => "channel_rollback",
                InstallDiagnostic::ManifestMissing => "manifest_missing",
                InstallDiagnostic::SignatureInvalid => "signature_invalid",
                InstallDiagnostic::SigningKeyUnknown => "signing_key_unknown",
                InstallDiagnostic::OriginRefused => "origin_refused",
                InstallDiagnostic::ArchiveInvalid => "archive_invalid",
                InstallDiagnostic::ExecutableInvalid => "executable_invalid",
                InstallDiagnostic::CachePermissions => "cache_permissions",
            },
            DiagnosticKind::Supervisor(code) => match code {
                SupervisorDiagnostic::SupervisorMismatch => "supervisor_mismatch",
                SupervisorDiagnostic::ReadinessTimeout => "readiness_timeout",
                SupervisorDiagnostic::ReadinessInvalid => "readiness_invalid",
                SupervisorDiagnostic::BindMismatch => "bind_mismatch",
                SupervisorDiagnostic::ChildExited => "child_exited",
                SupervisorDiagnostic::TerminateFailed => "terminate_failed",
            },
            DiagnosticKind::Control(code) => match code {
                ControlDiagnostic::ControlUnavailable => "control_unavailable",
                ControlDiagnostic::ControlAuthFailed => "control_auth_failed",
            },
            DiagnosticKind::Exchange(code) => match code {
                ExchangeDiagnostic::HealthFailed => "health_failed",
                ExchangeDiagnostic::ProtocolIncompatible => "protocol_incompatible",
            },
            DiagnosticKind::Truncated => "diagnostics_truncated",
        }
    }
}

impl Serialize for Diagnostic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct WireDiagnostic {
            component: &'static str,
            code: &'static str,
        }
        WireDiagnostic {
            component: self.component(),
            code: self.code(),
        }
        .serialize(serializer)
    }
}

/// The one typed source for both JSON and human lifecycle output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct LocalStatus {
    schema: &'static str,
    pub(super) state: LocalState,
    pub(super) channel: Option<AcceptedChannel>,
    pub(super) release: Option<InstalledRelease>,
    pub(super) endpoint: Option<VerifiedEndpoint>,
    diagnostics: Vec<Diagnostic>,
}

impl LocalStatus {
    pub(super) fn new(
        state: LocalState,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Self {
        Self::with_identity(state, None, None, None, diagnostics)
    }

    pub(super) fn with_identity(
        state: LocalState,
        channel: Option<AcceptedChannel>,
        release: Option<InstalledRelease>,
        endpoint: Option<VerifiedEndpoint>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Self {
        // Preserve all eight distinct diagnostics. Only when a ninth arrives does the eighth slot
        // become the closed truncation marker. Memory is bounded before any caller-controlled
        // diagnostic sequence is consumed.
        let mut bounded = Vec::with_capacity(MAX_DIAGNOSTICS);
        for diagnostic in diagnostics {
            if bounded.contains(&diagnostic) {
                continue;
            }
            if bounded.len() == MAX_DIAGNOSTICS {
                bounded.pop();
                bounded.push(Diagnostic::truncated());
                break;
            }
            bounded.push(diagnostic);
        }
        Self {
            schema: STATUS_SCHEMA,
            state,
            channel,
            release,
            endpoint,
            diagnostics: bounded,
        }
    }

    pub(super) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub(super) fn render_json(&self) -> String {
        serde_json::to_string(self).expect("closed Exchange status is serializable")
    }

    pub(super) fn render_human(&self) -> String {
        let mut lines = vec![format!("Exchange local: {}", self.state.as_str())];
        if let Some(channel) = &self.channel {
            lines.push(format!(
                "Channel: {} generation {} (trust {})",
                channel.name, channel.generation, channel.trust_version
            ));
        }
        if let Some(release) = &self.release {
            lines.push(format!(
                "Release: {} {} ({})",
                release.tag, release.version, release.target
            ));
        }
        if let Some(endpoint) = &self.endpoint {
            lines.push(format!(
                "Endpoint: {}://{}:{}",
                endpoint.scheme, endpoint.host, endpoint.port
            ));
        }
        for diagnostic in &self.diagnostics {
            lines.push(format!(
                "Diagnostic: {}/{}",
                diagnostic.component(),
                diagnostic.code()
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_closed_state_has_the_contract_exit_code() {
        let cases = [
            (LocalState::Healthy, 0),
            (LocalState::NotInstalled, 20),
            (LocalState::Stopped, 21),
            (LocalState::Starting, 22),
            (LocalState::InstallVerificationRefused, 23),
            (LocalState::Incompatible, 24),
            (LocalState::Unhealthy, 25),
            (LocalState::ForeignOrStale, 26),
            (LocalState::StopFailure, 27),
        ];
        for (state, expected) in cases {
            assert_eq!(state.exit_code(), expected, "{state:?}");
        }
    }

    #[test]
    fn status_json_has_the_exact_closed_shape_and_nulls() {
        let status = LocalStatus::new(LocalState::NotInstalled, []);
        assert_eq!(
            status.render_json(),
            r#"{"schema":"flux.exchange-local-status.v1","state":"not_installed","channel":null,"release":null,"endpoint":null,"diagnostics":[]}"#
        );
    }

    #[test]
    fn populated_status_json_preserves_the_exact_audit_and_endpoint_fields() {
        let status = LocalStatus::with_identity(
            LocalState::Healthy,
            Some(AcceptedChannel {
                name: "stable".into(),
                trust_version: "2".into(),
                trust_sha256: "trust-sha".into(),
                generation: 7,
                index_sha256: "index-sha".into(),
                expires_at: "2030-01-01T00:00:00Z".into(),
            }),
            Some(InstalledRelease {
                tag: "v1.2.3".into(),
                version: "1.2.3".into(),
                source_commit: "source".into(),
                build_id: "build".into(),
                target: "x86_64-unknown-linux-gnu".into(),
                manifest_sha256: "manifest-sha".into(),
                executable_sha256: "executable-sha".into(),
            }),
            Some(VerifiedEndpoint {
                scheme: "http".into(),
                host: "127.0.0.1".into(),
                port: 4242,
            }),
            [Diagnostic::exchange(ExchangeDiagnostic::HealthFailed)],
        );
        assert_eq!(
            status.render_json(),
            r#"{"schema":"flux.exchange-local-status.v1","state":"healthy","channel":{"name":"stable","trust_version":"2","trust_sha256":"trust-sha","generation":7,"index_sha256":"index-sha","expires_at":"2030-01-01T00:00:00Z"},"release":{"tag":"v1.2.3","version":"1.2.3","source_commit":"source","build_id":"build","target":"x86_64-unknown-linux-gnu","manifest_sha256":"manifest-sha","executable_sha256":"executable-sha"},"endpoint":{"scheme":"http","host":"127.0.0.1","port":4242},"diagnostics":[{"component":"exchange","code":"health_failed"}]}"#
        );
    }

    #[test]
    fn typed_diagnostic_inventory_is_closed_and_complete() {
        let cases = [
            (
                Diagnostic::install(InstallDiagnostic::TrustInvalid),
                "install",
                "trust_invalid",
            ),
            (
                Diagnostic::install(InstallDiagnostic::TrustExpired),
                "install",
                "trust_expired",
            ),
            (
                Diagnostic::install(InstallDiagnostic::TrustRollback),
                "install",
                "trust_rollback",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ChannelInvalid),
                "install",
                "channel_invalid",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ChannelExpired),
                "install",
                "channel_expired",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ChannelRollback),
                "install",
                "channel_rollback",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ManifestMissing),
                "install",
                "manifest_missing",
            ),
            (
                Diagnostic::install(InstallDiagnostic::SignatureInvalid),
                "install",
                "signature_invalid",
            ),
            (
                Diagnostic::install(InstallDiagnostic::SigningKeyUnknown),
                "install",
                "signing_key_unknown",
            ),
            (
                Diagnostic::install(InstallDiagnostic::OriginRefused),
                "install",
                "origin_refused",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ArchiveInvalid),
                "install",
                "archive_invalid",
            ),
            (
                Diagnostic::install(InstallDiagnostic::ExecutableInvalid),
                "install",
                "executable_invalid",
            ),
            (
                Diagnostic::install(InstallDiagnostic::CachePermissions),
                "install",
                "cache_permissions",
            ),
            (
                Diagnostic::control(ControlDiagnostic::ControlUnavailable),
                "control",
                "control_unavailable",
            ),
            (
                Diagnostic::control(ControlDiagnostic::ControlAuthFailed),
                "control",
                "control_auth_failed",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::SupervisorMismatch),
                "supervisor",
                "supervisor_mismatch",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::ReadinessTimeout),
                "supervisor",
                "readiness_timeout",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::ReadinessInvalid),
                "supervisor",
                "readiness_invalid",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::BindMismatch),
                "supervisor",
                "bind_mismatch",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::ChildExited),
                "supervisor",
                "child_exited",
            ),
            (
                Diagnostic::supervisor(SupervisorDiagnostic::TerminateFailed),
                "supervisor",
                "terminate_failed",
            ),
            (
                Diagnostic::exchange(ExchangeDiagnostic::HealthFailed),
                "exchange",
                "health_failed",
            ),
            (
                Diagnostic::exchange(ExchangeDiagnostic::ProtocolIncompatible),
                "exchange",
                "protocol_incompatible",
            ),
        ];
        for (diagnostic, component, code) in cases {
            assert_eq!(
                (diagnostic.component(), diagnostic.code()),
                (component, code)
            );
        }
    }

    #[test]
    fn exactly_eight_unique_diagnostics_are_preserved_without_a_marker() {
        let diagnostics = [
            Diagnostic::install(InstallDiagnostic::TrustInvalid),
            Diagnostic::install(InstallDiagnostic::TrustExpired),
            Diagnostic::install(InstallDiagnostic::TrustRollback),
            Diagnostic::install(InstallDiagnostic::ChannelInvalid),
            Diagnostic::install(InstallDiagnostic::ChannelExpired),
            Diagnostic::install(InstallDiagnostic::ChannelRollback),
            Diagnostic::install(InstallDiagnostic::ManifestMissing),
            Diagnostic::install(InstallDiagnostic::SignatureInvalid),
        ];
        let status = LocalStatus::new(LocalState::InstallVerificationRefused, diagnostics);
        assert_eq!(status.diagnostics(), diagnostics);
        assert!(status
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code() != "diagnostics_truncated"));
    }

    #[test]
    fn a_ninth_unique_diagnostic_replaces_the_eighth_with_a_typed_marker() {
        let status = LocalStatus::new(
            LocalState::InstallVerificationRefused,
            [
                Diagnostic::install(InstallDiagnostic::TrustInvalid),
                Diagnostic::install(InstallDiagnostic::TrustInvalid),
                Diagnostic::install(InstallDiagnostic::TrustExpired),
                Diagnostic::install(InstallDiagnostic::TrustRollback),
                Diagnostic::install(InstallDiagnostic::ChannelInvalid),
                Diagnostic::install(InstallDiagnostic::ChannelExpired),
                Diagnostic::install(InstallDiagnostic::ChannelRollback),
                Diagnostic::install(InstallDiagnostic::ManifestMissing),
                Diagnostic::install(InstallDiagnostic::SignatureInvalid),
                Diagnostic::install(InstallDiagnostic::SigningKeyUnknown),
            ],
        );
        assert_eq!(status.diagnostics().len(), 8);
        assert_eq!(
            status.diagnostics().last().unwrap().code(),
            "diagnostics_truncated"
        );
    }
}
