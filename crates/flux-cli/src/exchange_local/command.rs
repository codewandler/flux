// The effect trait intentionally exposes success/error constructors that the inert production
// backend does not use yet; fakes pin them until the verified backend is integrated.
#![allow(dead_code)]

use std::path::PathBuf;

use async_trait::async_trait;

use crate::ExchangeLocalAction;

use super::status::{
    ControlDiagnostic, Diagnostic, LocalState, LocalStatus, INTERNAL_FAILURE_EXIT,
};

/// The complete offline artifact bundle accepted by `import`. Paths are operator inputs only and
/// never enter status or diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImportBundle {
    pub(super) trust: PathBuf,
    pub(super) root_signatures: Vec<PathBuf>,
    pub(super) channel: PathBuf,
    pub(super) channel_signatures: Vec<PathBuf>,
    pub(super) manifest: PathBuf,
    pub(super) release_signatures: Vec<PathBuf>,
    pub(super) archive: PathBuf,
    pub(super) provenance: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleError {
    /// An internal failure happened before the backend could classify a public status.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MutationOutcome {
    pub(super) status: LocalStatus,
    /// True only when the requested final state has been reached. This represents idempotent
    /// already-started/already-stopped success without adding a wire-visible state.
    pub(super) request_satisfied: bool,
}

impl MutationOutcome {
    pub(super) fn reached(status: LocalStatus) -> Self {
        Self {
            status,
            request_satisfied: true,
        }
    }

    pub(super) fn refused(status: LocalStatus) -> Self {
        Self {
            status,
            request_satisfied: false,
        }
    }
}

/// Effect boundary for the local lifecycle. C-510's installer, authenticated control client and
/// supervisor will implement this; the CLI owns only parsing, rendering and exit semantics.
#[async_trait]
pub(super) trait ExchangeLocalLifecycle {
    async fn start(&mut self) -> Result<MutationOutcome, LifecycleError>;
    async fn status(&mut self) -> Result<LocalStatus, LifecycleError>;
    async fn stop(&mut self) -> Result<MutationOutcome, LifecycleError>;
    async fn import(&mut self, bundle: ImportBundle) -> Result<MutationOutcome, LifecycleError>;
    async fn reinstall(&mut self) -> Result<MutationOutcome, LifecycleError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandOutcome {
    pub(super) stdout: Option<String>,
    pub(super) stderr: Option<&'static str>,
    pub(super) exit_code: i32,
}

impl CommandOutcome {
    fn classified(status: LocalStatus, json: bool, exit_code: i32) -> Self {
        Self {
            stdout: Some(if json {
                status.render_json()
            } else {
                status.render_human()
            }),
            stderr: None,
            exit_code,
        }
    }

    fn internal_failure() -> Self {
        Self {
            stdout: None,
            stderr: Some("exchange local lifecycle failed before status classification"),
            exit_code: INTERNAL_FAILURE_EXIT,
        }
    }
}

/// Pure dispatch over an injected lifecycle backend. Mutating commands return zero only when the
/// backend attests that the requested final state was reached; otherwise their classified status
/// decides the exit code. `status` always uses the exhaustive status table.
pub(super) async fn dispatch_with(
    lifecycle: &mut dyn ExchangeLocalLifecycle,
    action: ExchangeLocalAction,
) -> CommandOutcome {
    match action {
        ExchangeLocalAction::Status { json } => match lifecycle.status().await {
            Ok(status) => {
                let exit_code = status.state.exit_code();
                CommandOutcome::classified(status, json, exit_code)
            }
            Err(LifecycleError::Internal) => CommandOutcome::internal_failure(),
        },
        ExchangeLocalAction::Start => {
            mutation_outcome(lifecycle.start().await, Some(LocalState::Healthy))
        }
        ExchangeLocalAction::Stop => {
            mutation_outcome(lifecycle.stop().await, Some(LocalState::Stopped))
        }
        ExchangeLocalAction::Import {
            trust,
            root_signature,
            channel,
            channel_signature,
            manifest,
            release_signature,
            archive,
            provenance,
        } => mutation_outcome(
            lifecycle
                .import(ImportBundle {
                    trust,
                    root_signatures: root_signature,
                    channel,
                    channel_signatures: channel_signature,
                    manifest,
                    release_signatures: release_signature,
                    archive,
                    provenance,
                })
                .await,
            None,
        ),
        ExchangeLocalAction::Reinstall => mutation_outcome(lifecycle.reinstall().await, None),
    }
}

fn mutation_outcome(
    result: Result<MutationOutcome, LifecycleError>,
    required_state: Option<LocalState>,
) -> CommandOutcome {
    match result {
        Ok(result) => {
            let final_state_matches =
                required_state.is_none_or(|state| result.status.state == state);
            let exit_code = if result.request_satisfied && final_state_matches {
                0
            } else {
                result.status.state.exit_code()
            };
            CommandOutcome::classified(result.status, false, exit_code)
        }
        Err(LifecycleError::Internal) => CommandOutcome::internal_failure(),
    }
}

/// Deliberately inert until the installer/control implementation is integrated. It returns a typed,
/// value-free status rather than reaching for a process, socket, PID, PATH entry or provider field.
struct UnavailableLifecycle;

impl UnavailableLifecycle {
    fn status() -> LocalStatus {
        LocalStatus::new(
            LocalState::ForeignOrStale,
            [Diagnostic::control(ControlDiagnostic::ControlUnavailable)],
        )
    }
}

#[async_trait]
impl ExchangeLocalLifecycle for UnavailableLifecycle {
    async fn start(&mut self) -> Result<MutationOutcome, LifecycleError> {
        Ok(MutationOutcome::refused(Self::status()))
    }

    async fn status(&mut self) -> Result<LocalStatus, LifecycleError> {
        Ok(Self::status())
    }

    async fn stop(&mut self) -> Result<MutationOutcome, LifecycleError> {
        Ok(MutationOutcome::refused(Self::status()))
    }

    async fn import(&mut self, _bundle: ImportBundle) -> Result<MutationOutcome, LifecycleError> {
        Ok(MutationOutcome::refused(Self::status()))
    }

    async fn reinstall(&mut self) -> Result<MutationOutcome, LifecycleError> {
        Ok(MutationOutcome::refused(Self::status()))
    }
}

pub(crate) async fn run(action: ExchangeLocalAction) -> i32 {
    let outcome = dispatch_with(&mut UnavailableLifecycle, action).await;
    if let Some(stdout) = outcome.stdout {
        println!("{stdout}");
    }
    if let Some(stderr) = outcome.stderr {
        eprintln!("{} {stderr}", crate::style::red("error:"));
    }
    outcome.exit_code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands, ExchangeAction, ExchangeLocalAction};
    use clap::Parser;

    use crate::exchange_local::status::{
        ExchangeDiagnostic, InstallDiagnostic, SupervisorDiagnostic,
    };

    #[derive(Default)]
    struct FakeLifecycle {
        calls: Vec<&'static str>,
    }

    #[async_trait]
    impl ExchangeLocalLifecycle for FakeLifecycle {
        async fn start(&mut self) -> Result<MutationOutcome, LifecycleError> {
            self.calls.push("start");
            Ok(MutationOutcome::reached(LocalStatus::new(
                LocalState::Healthy,
                [],
            )))
        }

        async fn status(&mut self) -> Result<LocalStatus, LifecycleError> {
            self.calls.push("status");
            Ok(LocalStatus::new(LocalState::Stopped, []))
        }

        async fn stop(&mut self) -> Result<MutationOutcome, LifecycleError> {
            self.calls.push("stop");
            Ok(MutationOutcome::reached(LocalStatus::new(
                LocalState::Stopped,
                [],
            )))
        }

        async fn import(
            &mut self,
            bundle: ImportBundle,
        ) -> Result<MutationOutcome, LifecycleError> {
            self.calls.push("import");
            assert_eq!(bundle.root_signatures.len(), 2);
            Ok(MutationOutcome::reached(LocalStatus::new(
                LocalState::Stopped,
                [],
            )))
        }

        async fn reinstall(&mut self) -> Result<MutationOutcome, LifecycleError> {
            self.calls.push("reinstall");
            Ok(MutationOutcome::reached(LocalStatus::new(
                LocalState::Stopped,
                [],
            )))
        }
    }

    #[test]
    fn lifecycle_commands_and_strict_import_bundle_parse() {
        for verb in ["start", "stop", "reinstall"] {
            let cli = Cli::try_parse_from(["flux", "exchange", "local", verb]).unwrap();
            assert!(matches!(cli.command, Some(Commands::Exchange { .. })));
        }
        let cli = Cli::try_parse_from(["flux", "exchange", "local", "status", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Exchange {
                action: ExchangeAction::Local {
                    action: ExchangeLocalAction::Status { json: true }
                }
            })
        ));

        let cli = Cli::try_parse_from([
            "flux",
            "exchange",
            "local",
            "import",
            "--trust",
            "trust.json",
            "--root-signature",
            "root-a.minisig",
            "--root-signature",
            "root-b.minisig",
            "--channel",
            "channel.json",
            "--channel-signature",
            "channel.minisig",
            "--manifest",
            "manifest.json",
            "--release-signature",
            "release.minisig",
            "--archive",
            "exchange.tar.zst",
            "--provenance",
            "provenance.json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Exchange {
                action:
                    ExchangeAction::Local {
                        action: ExchangeLocalAction::Import { root_signature, .. },
                    },
            }) => assert_eq!(root_signature.len(), 2),
            other => panic!("expected exchange import, got {other:?}"),
        }
    }

    #[test]
    fn import_refuses_every_incomplete_bundle_and_start_has_no_artifact_override() {
        let complete = [
            "--trust",
            "trust.json",
            "--root-signature",
            "root.minisig",
            "--channel",
            "channel.json",
            "--channel-signature",
            "channel.minisig",
            "--manifest",
            "manifest.json",
            "--release-signature",
            "release.minisig",
            "--archive",
            "exchange.tar.zst",
            "--provenance",
            "provenance.json",
        ];
        for required in [
            "--trust",
            "--root-signature",
            "--channel",
            "--channel-signature",
            "--manifest",
            "--release-signature",
            "--archive",
            "--provenance",
        ] {
            let mut argv = vec!["flux", "exchange", "local", "import"];
            let mut skip_value = false;
            for value in complete {
                if value == required {
                    skip_value = true;
                } else if skip_value {
                    skip_value = false;
                } else {
                    argv.push(value);
                }
            }
            assert!(Cli::try_parse_from(argv).is_err(), "missing {required}");
        }
        assert!(Cli::try_parse_from([
            "flux",
            "exchange",
            "local",
            "start",
            "--archive",
            "local.tar.zst"
        ])
        .is_err());

        let argv = ["flux", "exchange", "local", "import"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(crate::exchange_usage_exit_code(&argv, 2), 64);

        let unrelated = ["flux", "run", "exchange", "local", "--unknown"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(crate::exchange_usage_exit_code(&unrelated, 2), 2);

        let with_globals = [
            "flux",
            "--color",
            "never",
            "exchange",
            "--store=state",
            "local",
            "import",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
        assert_eq!(crate::exchange_usage_exit_code(&with_globals, 2), 64);
    }

    #[tokio::test]
    async fn fake_dispatch_proves_status_exits_and_idempotent_mutation_success() {
        let mut fake = FakeLifecycle::default();
        let status = dispatch_with(&mut fake, ExchangeLocalAction::Status { json: true }).await;
        assert_eq!(status.exit_code, 21);
        assert!(status.stdout.unwrap().contains(r#""state":"stopped""#));

        // A reached stop is success even though querying the same `stopped` status uses exit 21.
        let first = dispatch_with(&mut fake, ExchangeLocalAction::Stop).await;
        let repeated = dispatch_with(&mut fake, ExchangeLocalAction::Stop).await;
        assert_eq!((first.exit_code, repeated.exit_code), (0, 0));
        assert_eq!(fake.calls, ["status", "stop", "stop"]);
    }

    #[tokio::test]
    async fn fake_dispatch_routes_start_import_and_reinstall_without_exposing_paths() {
        let mut fake = FakeLifecycle::default();
        let start = dispatch_with(&mut fake, ExchangeLocalAction::Start).await;
        let import = dispatch_with(
            &mut fake,
            ExchangeLocalAction::Import {
                trust: "trust.json".into(),
                root_signature: vec!["root-a.sig".into(), "root-b.sig".into()],
                channel: "channel.json".into(),
                channel_signature: vec!["channel.sig".into()],
                manifest: "manifest.json".into(),
                release_signature: vec!["release.sig".into()],
                archive: "exchange.tar.zst".into(),
                provenance: "provenance.json".into(),
            },
        )
        .await;
        let reinstall = dispatch_with(&mut fake, ExchangeLocalAction::Reinstall).await;

        assert_eq!(
            (start.exit_code, import.exit_code, reinstall.exit_code),
            (0, 0, 0)
        );
        assert_eq!(fake.calls, ["start", "import", "reinstall"]);
        assert!(!import.stdout.unwrap().contains("trust.json"));
    }

    #[tokio::test]
    async fn mutation_refusal_uses_the_reported_status_and_internal_failure_is_70() {
        struct Refusing;
        #[async_trait]
        impl ExchangeLocalLifecycle for Refusing {
            async fn start(&mut self) -> Result<MutationOutcome, LifecycleError> {
                Ok(MutationOutcome::refused(LocalStatus::new(
                    LocalState::Unhealthy,
                    [Diagnostic::exchange(ExchangeDiagnostic::HealthFailed)],
                )))
            }
            async fn status(&mut self) -> Result<LocalStatus, LifecycleError> {
                Err(LifecycleError::Internal)
            }
            async fn stop(&mut self) -> Result<MutationOutcome, LifecycleError> {
                unreachable!()
            }
            async fn import(
                &mut self,
                _bundle: ImportBundle,
            ) -> Result<MutationOutcome, LifecycleError> {
                unreachable!()
            }
            async fn reinstall(&mut self) -> Result<MutationOutcome, LifecycleError> {
                unreachable!()
            }
        }

        let mut fake = Refusing;
        assert_eq!(
            dispatch_with(&mut fake, ExchangeLocalAction::Start)
                .await
                .exit_code,
            25
        );
        assert_eq!(
            dispatch_with(&mut fake, ExchangeLocalAction::Status { json: false })
                .await
                .exit_code,
            70
        );
    }

    #[test]
    fn human_rendering_is_deterministic_and_uses_only_typed_codes() {
        let status = LocalStatus::new(
            LocalState::Unhealthy,
            [
                Diagnostic::supervisor(SupervisorDiagnostic::ChildExited),
                Diagnostic::install(InstallDiagnostic::ExecutableInvalid),
            ],
        );
        assert_eq!(
            status.render_human(),
            "Exchange local: unhealthy\nDiagnostic: supervisor/child_exited\nDiagnostic: install/executable_invalid"
        );
    }
}
