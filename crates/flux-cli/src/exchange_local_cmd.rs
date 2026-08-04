//! Typed CLI-facing state for the C-510 local Exchange lifecycle.
//!
//! Exchange owns the signed channel, manifest, compatibility, and readiness wire shapes. This
//! module deliberately does not deserialize any of them. It accepts only values that satisfy the
//! provider's bounded identity domains and turns verifier/lifecycle outcomes into Flux's closed,
//! value-free status vocabulary.

use std::fmt;
use std::num::NonZeroU16;

use anyhow::Result;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::{ExchangeAction, ExchangeLocalAction};

pub const STATUS_SCHEMA: &str = "flux.exchange-local-status.v1";
pub const MAX_STATUS_DIAGNOSTICS: usize = 8;
const JSON_SAFE_MAX: u64 = 9_007_199_254_740_991;

/// The provider-dependent backend cannot yet classify lifecycle state. This is deliberately not a
/// lifecycle diagnostic: C-510 reserves exit 70 for failure before classification and publishes no
/// additional status code for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExchangeLocalInternalFailure;

impl fmt::Display for ExchangeLocalInternalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("internal_failure")
    }
}

impl std::error::Error for ExchangeLocalInternalFailure {}

/// A value-free refusal from a provider identity boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityRefusal {
    OutOfDomain,
    InvalidStableVersion,
    TagVersionMismatch,
    InvalidSourceCommit,
    InvalidBuildId,
    InvalidDigest,
    InvalidTimestamp,
}

impl fmt::Display for IdentityRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutOfDomain => "identity_out_of_domain",
            Self::InvalidStableVersion => "stable_version_invalid",
            Self::TagVersionMismatch => "tag_version_mismatch",
            Self::InvalidSourceCommit => "source_commit_invalid",
            Self::InvalidBuildId => "build_id_invalid",
            Self::InvalidDigest => "digest_invalid",
            Self::InvalidTimestamp => "timestamp_invalid",
        })
    }
}

impl std::error::Error for IdentityRefusal {}

/// An RFC 8785 interoperable positive integer from authenticated Exchange metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderInteger(u64);

impl ProviderInteger {
    pub fn new(value: u64) -> std::result::Result<Self, IdentityRefusal> {
        (value > 0 && value <= JSON_SAFE_MAX)
            .then_some(Self(value))
            .ok_or(IdentityRefusal::OutOfDomain)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha256(String);

impl Sha256 {
    pub fn parse(value: &str) -> std::result::Result<Self, IdentityRefusal> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityRefusal::InvalidDigest)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct StableVersion(String);

impl StableVersion {
    pub fn parse(value: &str) -> std::result::Result<Self, IdentityRefusal> {
        let mut parts = value.split('.');
        let valid = (0..3).all(|_| parts.next().is_some_and(valid_version_component))
            && parts.next().is_none();
        valid
            .then(|| Self(value.to_owned()))
            .ok_or(IdentityRefusal::InvalidStableVersion)
    }
}

fn valid_version_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= 9
        && component.bytes().all(|byte| byte.is_ascii_digit())
        && (component == "0" || !component.starts_with('0'))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ReleaseTag(String);

impl ReleaseTag {
    fn for_version(
        tag: &str,
        version: &StableVersion,
    ) -> std::result::Result<Self, IdentityRefusal> {
        let expected = format!("refs/tags/v{}", version.0);
        (tag == expected)
            .then(|| Self(tag.to_owned()))
            .ok_or(IdentityRefusal::TagVersionMismatch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SourceCommit(String);

impl SourceCommit {
    pub fn parse(value: &str) -> std::result::Result<Self, IdentityRefusal> {
        if value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityRefusal::InvalidSourceCommit)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BuildId(String);

impl BuildId {
    pub fn parse(value: &str) -> std::result::Result<Self, IdentityRefusal> {
        if (1..=128).contains(&value.len())
            && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityRefusal::InvalidBuildId)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderTimestamp(String);

impl ProviderTimestamp {
    pub fn parse(value: &str) -> std::result::Result<Self, IdentityRefusal> {
        let shape = value.len() == 20
            && value.as_bytes().get(4) == Some(&b'-')
            && value.as_bytes().get(7) == Some(&b'-')
            && value.as_bytes().get(10) == Some(&b'T')
            && value.as_bytes().get(13) == Some(&b':')
            && value.as_bytes().get(16) == Some(&b':')
            && value.ends_with('Z');
        let canonical = chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| {
                time.to_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .is_some_and(|rendered| rendered == value);
        (shape && canonical)
            .then(|| Self(value.to_owned()))
            .ok_or(IdentityRefusal::InvalidTimestamp)
    }
}

/// The provider's closed X-126 target set. A host target outside this set cannot become status
/// identity merely because it is a Rust target triple.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum ExchangeTarget {
    #[serde(rename = "aarch64-apple-darwin")]
    Aarch64AppleDarwin,
    #[serde(rename = "x86_64-apple-darwin")]
    X86_64AppleDarwin,
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    Aarch64UnknownLinuxGnu,
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    X86_64UnknownLinuxGnu,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    X86_64PcWindowsMsvc,
}

/// Bounded fields copied only from an authenticated, provider-validated stable channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelIdentity {
    name: ChannelName,
    trust_version: ProviderInteger,
    trust_sha256: Sha256,
    generation: ProviderInteger,
    index_sha256: Sha256,
    expires_at: ProviderTimestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum ChannelName {
    #[serde(rename = "stable")]
    Stable,
}

impl ChannelIdentity {
    pub fn new(
        trust_version: u64,
        trust_sha256: &str,
        generation: u64,
        index_sha256: &str,
        expires_at: &str,
    ) -> std::result::Result<Self, IdentityRefusal> {
        Ok(Self {
            name: ChannelName::Stable,
            trust_version: ProviderInteger::new(trust_version)?,
            trust_sha256: Sha256::parse(trust_sha256)?,
            generation: ProviderInteger::new(generation)?,
            index_sha256: Sha256::parse(index_sha256)?,
            expires_at: ProviderTimestamp::parse(expires_at)?,
        })
    }
}

/// Bounded audit identity copied only after channel, manifest, archive, executable, and
/// compatibility validation agree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReleaseIdentity {
    tag: ReleaseTag,
    version: StableVersion,
    source_commit: SourceCommit,
    build_id: BuildId,
    target: ExchangeTarget,
    manifest_sha256: Sha256,
    executable_sha256: Sha256,
}

impl ReleaseIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tag: &str,
        version: &str,
        source_commit: &str,
        build_id: &str,
        target: ExchangeTarget,
        manifest_sha256: &str,
        executable_sha256: &str,
    ) -> std::result::Result<Self, IdentityRefusal> {
        let version = StableVersion::parse(version)?;
        Ok(Self {
            tag: ReleaseTag::for_version(tag, &version)?,
            version,
            source_commit: SourceCommit::parse(source_commit)?,
            build_id: BuildId::parse(build_id)?,
            target,
            manifest_sha256: Sha256::parse(manifest_sha256)?,
            executable_sha256: Sha256::parse(executable_sha256)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExchangeEndpoint {
    scheme: EndpointScheme,
    host: LoopbackHost,
    port: NonZeroU16,
}

impl ExchangeEndpoint {
    pub fn new(host: LoopbackHost, port: NonZeroU16) -> Self {
        Self {
            scheme: EndpointScheme::Http,
            host,
            port,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum EndpointScheme {
    #[serde(rename = "http")]
    Http,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LoopbackHost {
    #[serde(rename = "127.0.0.1")]
    Ipv4,
    #[serde(rename = "::1")]
    Ipv6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeLocalState {
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

impl ExchangeLocalState {
    pub fn exit_code(self) -> i32 {
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

/// C-510's closed diagnostic component vocabulary. The story does not assign its codes to a
/// component matrix, so this slice preserves component and code as two closed provider/lifecycle
/// outcomes instead of inventing a narrower association.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticComponent {
    Install,
    Supervisor,
    Control,
    Exchange,
}

impl DiagnosticComponent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Supervisor => "supervisor",
            Self::Control => "control",
            Self::Exchange => "exchange",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
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
    ControlUnavailable,
    ControlAuthFailed,
    SupervisorMismatch,
    ReadinessTimeout,
    ReadinessInvalid,
    BindMismatch,
    ChildExited,
    HealthFailed,
    ProtocolIncompatible,
    TerminateFailed,
    DiagnosticsTruncated,
}

impl DiagnosticCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::TrustInvalid => "trust_invalid",
            Self::TrustExpired => "trust_expired",
            Self::TrustRollback => "trust_rollback",
            Self::ChannelInvalid => "channel_invalid",
            Self::ChannelExpired => "channel_expired",
            Self::ChannelRollback => "channel_rollback",
            Self::ManifestMissing => "manifest_missing",
            Self::SignatureInvalid => "signature_invalid",
            Self::SigningKeyUnknown => "signing_key_unknown",
            Self::OriginRefused => "origin_refused",
            Self::ArchiveInvalid => "archive_invalid",
            Self::ExecutableInvalid => "executable_invalid",
            Self::CachePermissions => "cache_permissions",
            Self::ControlUnavailable => "control_unavailable",
            Self::ControlAuthFailed => "control_auth_failed",
            Self::SupervisorMismatch => "supervisor_mismatch",
            Self::ReadinessTimeout => "readiness_timeout",
            Self::ReadinessInvalid => "readiness_invalid",
            Self::BindMismatch => "bind_mismatch",
            Self::ChildExited => "child_exited",
            Self::HealthFailed => "health_failed",
            Self::ProtocolIncompatible => "protocol_incompatible",
            Self::TerminateFailed => "terminate_failed",
            Self::DiagnosticsTruncated => "diagnostics_truncated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LifecycleDiagnostic {
    component: DiagnosticComponent,
    code: DiagnosticCode,
}

impl LifecycleDiagnostic {
    pub fn new(component: DiagnosticComponent, code: DiagnosticCode) -> Self {
        Self { component, code }
    }

    fn truncated_for_component(self) -> Self {
        Self::new(self.component, DiagnosticCode::DiagnosticsTruncated)
    }

    fn is_truncated(self) -> bool {
        self.code == DiagnosticCode::DiagnosticsTruncated
    }
}

impl Serialize for LifecycleDiagnostic {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = serializer.serialize_struct("LifecycleDiagnostic", 2)?;
        object.serialize_field("component", self.component.as_str())?;
        object.serialize_field("code", self.code.as_str())?;
        object.end()
    }
}

/// The sole human/JSON status source. No field can carry an OS error, path, child output, or
/// arbitrary diagnostic value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExchangeLocalStatus {
    schema: &'static str,
    state: ExchangeLocalState,
    channel: Option<ChannelIdentity>,
    release: Option<ReleaseIdentity>,
    endpoint: Option<ExchangeEndpoint>,
    diagnostics: Vec<LifecycleDiagnostic>,
}

impl ExchangeLocalStatus {
    pub fn new(state: ExchangeLocalState) -> Self {
        Self {
            schema: STATUS_SCHEMA,
            state,
            channel: None,
            release: None,
            endpoint: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn with_channel(mut self, channel: ChannelIdentity) -> Self {
        self.channel = Some(channel);
        self
    }

    pub fn with_release(mut self, release: ReleaseIdentity) -> Self {
        self.release = Some(release);
        self
    }

    pub fn with_endpoint(mut self, endpoint: ExchangeEndpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    pub fn push_diagnostic(&mut self, diagnostic: LifecycleDiagnostic) {
        if self.diagnostics.contains(&diagnostic) {
            return;
        }
        if self.diagnostics.len() < MAX_STATUS_DIAGNOSTICS {
            self.diagnostics.push(diagnostic);
            return;
        }
        if self.diagnostics.iter().any(|item| item.is_truncated()) {
            return;
        }
        self.diagnostics[MAX_STATUS_DIAGNOSTICS - 1] = diagnostic.truncated_for_component();
    }

    pub fn exit_code(&self) -> i32 {
        self.state.exit_code()
    }

    pub fn render_json(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn render_human(&self) -> String {
        let mut output = format!("local exchange: {}", self.state.as_str());
        for diagnostic in &self.diagnostics {
            output.push_str("\n  ");
            output.push_str(diagnostic.component.as_str());
            output.push_str(": ");
            output.push_str(diagnostic.code.as_str());
        }
        output
    }
}

/// Parser landing point until the provider fixtures unblock lifecycle execution. The typed
/// pre-classification outcome keeps the accepted grammar wired without pretending that a local
/// install or process state was inspected or adding a lifecycle diagnostic code.
pub(crate) async fn run_exchange(action: ExchangeAction) -> Result<()> {
    let _action = match action {
        ExchangeAction::Local { action } => match action {
            ExchangeLocalAction::Start => "start",
            ExchangeLocalAction::Status { json: false } => "status",
            ExchangeLocalAction::Status { json: true } => "status_json",
            ExchangeLocalAction::Stop => "stop",
            ExchangeLocalAction::Reinstall => "reinstall",
            ExchangeLocalAction::Import { .. } => "import",
        },
    };
    Err(ExchangeLocalInternalFailure.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";

    fn full_status() -> ExchangeLocalStatus {
        ExchangeLocalStatus::new(ExchangeLocalState::Healthy)
            .with_channel(
                ChannelIdentity::new(7, DIGEST_A, 11, DIGEST_B, "2026-08-05T12:00:00Z").unwrap(),
            )
            .with_release(
                ReleaseIdentity::new(
                    "refs/tags/v1.2.3",
                    "1.2.3",
                    SOURCE,
                    "release-build-7",
                    ExchangeTarget::X86_64UnknownLinuxGnu,
                    DIGEST_A,
                    DIGEST_B,
                )
                .unwrap(),
            )
            .with_endpoint(ExchangeEndpoint::new(
                LoopbackHost::Ipv4,
                NonZeroU16::new(4567).unwrap(),
            ))
    }

    #[test]
    fn provider_identity_types_reject_unbounded_or_noncanonical_values() {
        assert_eq!(
            StableVersion::parse("01.2.3"),
            Err(IdentityRefusal::InvalidStableVersion)
        );
        assert_eq!(
            ReleaseIdentity::new(
                "refs/tags/v9.9.9",
                "1.2.3",
                SOURCE,
                "build",
                ExchangeTarget::X86_64UnknownLinuxGnu,
                DIGEST_A,
                DIGEST_B,
            ),
            Err(IdentityRefusal::TagVersionMismatch)
        );
        assert_eq!(
            BuildId::parse(&"x".repeat(129)),
            Err(IdentityRefusal::InvalidBuildId)
        );
        assert_eq!(
            Sha256::parse(&DIGEST_A.to_ascii_uppercase()),
            Err(IdentityRefusal::InvalidDigest)
        );
        assert_eq!(
            ProviderInteger::new(JSON_SAFE_MAX + 1),
            Err(IdentityRefusal::OutOfDomain)
        );
        assert_eq!(
            ProviderTimestamp::parse("2026-08-05T12:00:00+00:00"),
            Err(IdentityRefusal::InvalidTimestamp)
        );
    }

    #[test]
    fn status_json_has_the_exact_bounded_shape_and_no_open_diagnostic_values() {
        let mut status = full_status();
        status.push_diagnostic(LifecycleDiagnostic::new(
            DiagnosticComponent::Install,
            DiagnosticCode::ChannelExpired,
        ));
        let value: serde_json::Value =
            serde_json::from_str(&status.render_json().unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "schema": "flux.exchange-local-status.v1",
                "state": "healthy",
                "channel": {
                    "name": "stable",
                    "trust_version": 7,
                    "trust_sha256": DIGEST_A,
                    "generation": 11,
                    "index_sha256": DIGEST_B,
                    "expires_at": "2026-08-05T12:00:00Z"
                },
                "release": {
                    "tag": "refs/tags/v1.2.3",
                    "version": "1.2.3",
                    "source_commit": SOURCE,
                    "build_id": "release-build-7",
                    "target": "x86_64-unknown-linux-gnu",
                    "manifest_sha256": DIGEST_A,
                    "executable_sha256": DIGEST_B
                },
                "endpoint": {"scheme": "http", "host": "127.0.0.1", "port": 4567},
                "diagnostics": [{"component": "install", "code": "channel_expired"}]
            })
        );
    }

    #[test]
    fn human_status_is_derived_from_closed_state_and_codes_only() {
        let mut status = full_status();
        status.push_diagnostic(LifecycleDiagnostic::new(
            DiagnosticComponent::Control,
            DiagnosticCode::ControlUnavailable,
        ));
        let human = status.render_human();
        assert_eq!(
            human,
            "local exchange: healthy\n  control: control_unavailable"
        );
        for hidden in [DIGEST_A, SOURCE, "refs/tags/v1.2.3", "release-build-7"] {
            assert!(!human.contains(hidden));
        }
    }

    #[test]
    fn diagnostics_are_deduplicated_and_bounded_with_a_closed_truncation_code() {
        let mut status = ExchangeLocalStatus::new(ExchangeLocalState::Unhealthy);
        let values = [
            LifecycleDiagnostic::new(DiagnosticComponent::Install, DiagnosticCode::TrustInvalid),
            LifecycleDiagnostic::new(DiagnosticComponent::Install, DiagnosticCode::TrustExpired),
            LifecycleDiagnostic::new(DiagnosticComponent::Install, DiagnosticCode::TrustRollback),
            LifecycleDiagnostic::new(DiagnosticComponent::Install, DiagnosticCode::ChannelInvalid),
            LifecycleDiagnostic::new(DiagnosticComponent::Install, DiagnosticCode::ChannelExpired),
            LifecycleDiagnostic::new(
                DiagnosticComponent::Install,
                DiagnosticCode::ChannelRollback,
            ),
            LifecycleDiagnostic::new(
                DiagnosticComponent::Install,
                DiagnosticCode::ManifestMissing,
            ),
            LifecycleDiagnostic::new(
                DiagnosticComponent::Install,
                DiagnosticCode::SignatureInvalid,
            ),
            LifecycleDiagnostic::new(
                DiagnosticComponent::Supervisor,
                DiagnosticCode::ReadinessInvalid,
            ),
        ];
        status.push_diagnostic(values[0]);
        for value in values {
            status.push_diagnostic(value);
        }

        let value: serde_json::Value =
            serde_json::from_str(&status.render_json().unwrap()).unwrap();
        let diagnostics = value["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), MAX_STATUS_DIAGNOSTICS);
        assert_eq!(
            diagnostics.last().unwrap(),
            &serde_json::json!({
                "component": "supervisor",
                "code": "diagnostics_truncated"
            })
        );
    }

    #[test]
    fn diagnostic_vocabularies_are_exact_and_value_free() {
        let components = [
            DiagnosticComponent::Install,
            DiagnosticComponent::Supervisor,
            DiagnosticComponent::Control,
            DiagnosticComponent::Exchange,
        ]
        .map(DiagnosticComponent::as_str);
        assert_eq!(components, ["install", "supervisor", "control", "exchange"]);

        let codes = [
            DiagnosticCode::TrustInvalid,
            DiagnosticCode::TrustExpired,
            DiagnosticCode::TrustRollback,
            DiagnosticCode::ChannelInvalid,
            DiagnosticCode::ChannelExpired,
            DiagnosticCode::ChannelRollback,
            DiagnosticCode::ManifestMissing,
            DiagnosticCode::SignatureInvalid,
            DiagnosticCode::SigningKeyUnknown,
            DiagnosticCode::OriginRefused,
            DiagnosticCode::ArchiveInvalid,
            DiagnosticCode::ExecutableInvalid,
            DiagnosticCode::CachePermissions,
            DiagnosticCode::ControlUnavailable,
            DiagnosticCode::ControlAuthFailed,
            DiagnosticCode::SupervisorMismatch,
            DiagnosticCode::ReadinessTimeout,
            DiagnosticCode::ReadinessInvalid,
            DiagnosticCode::BindMismatch,
            DiagnosticCode::ChildExited,
            DiagnosticCode::HealthFailed,
            DiagnosticCode::ProtocolIncompatible,
            DiagnosticCode::TerminateFailed,
            DiagnosticCode::DiagnosticsTruncated,
        ]
        .map(DiagnosticCode::as_str);
        assert_eq!(
            codes,
            [
                "trust_invalid",
                "trust_expired",
                "trust_rollback",
                "channel_invalid",
                "channel_expired",
                "channel_rollback",
                "manifest_missing",
                "signature_invalid",
                "signing_key_unknown",
                "origin_refused",
                "archive_invalid",
                "executable_invalid",
                "cache_permissions",
                "control_unavailable",
                "control_auth_failed",
                "supervisor_mismatch",
                "readiness_timeout",
                "readiness_invalid",
                "bind_mismatch",
                "child_exited",
                "health_failed",
                "protocol_incompatible",
                "terminate_failed",
                "diagnostics_truncated",
            ]
        );
    }

    #[test]
    fn every_status_state_has_the_contract_exit_code() {
        for (state, expected) in [
            (ExchangeLocalState::Healthy, 0),
            (ExchangeLocalState::NotInstalled, 20),
            (ExchangeLocalState::Stopped, 21),
            (ExchangeLocalState::Starting, 22),
            (ExchangeLocalState::InstallVerificationRefused, 23),
            (ExchangeLocalState::Incompatible, 24),
            (ExchangeLocalState::Unhealthy, 25),
            (ExchangeLocalState::ForeignOrStale, 26),
            (ExchangeLocalState::StopFailure, 27),
        ] {
            assert_eq!(ExchangeLocalStatus::new(state).exit_code(), expected);
        }
    }
}
