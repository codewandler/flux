//! Stable, value-free command outcome projection for the integration CLI.
//!
//! Provider response bodies deliberately do not cross this boundary. Callers classify an outcome
//! into the closed vocabulary below; this module then owns the human/JSON rendering and exit code.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    ExchangeLocalStart,
    ExchangeLocalStatus,
    ExchangeLocalStop,
    IntegrationConnect,
    IntegrationGrant,
    IntegrationList,
    IntegrationDoctor,
}

impl Command {
    fn label(self) -> &'static str {
        match self {
            Self::ExchangeLocalStart => "exchange.local.start",
            Self::ExchangeLocalStatus => "exchange.local.status",
            Self::ExchangeLocalStop => "exchange.local.stop",
            Self::IntegrationConnect => "integration.connect",
            Self::IntegrationGrant => "integration.grant",
            Self::IntegrationList => "integration.list",
            Self::IntegrationDoctor => "integration.doctor",
        }
    }

    fn human_label(self) -> &'static str {
        match self {
            Self::ExchangeLocalStart => "exchange local start",
            Self::ExchangeLocalStatus => "exchange local status",
            Self::ExchangeLocalStop => "exchange local stop",
            Self::IntegrationConnect => "integration connect",
            Self::IntegrationGrant => "integration grant",
            Self::IntegrationList => "integration list",
            Self::IntegrationDoctor => "integration doctor",
        }
    }
}

// The provider-independent CLI currently emits `Unsupported`; the remaining closed categories are
// consumed as X-125/C-510 wire the corresponding provider states into this same projection.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    Refused,
    Unavailable,
    AuthenticationRequired,
    Incomplete,
    Unsupported,
    ConnectionConflict { connector: String, label: String },
}

impl Refusal {
    fn category(&self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Unavailable => "unavailable",
            Self::AuthenticationRequired => "authentication_required",
            Self::Incomplete => "incomplete",
            Self::Unsupported => "unsupported",
            Self::ConnectionConflict { .. } => "conflict",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandOutcome {
    Success { command: Command },
    Refused { command: Command, refusal: Refusal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Projection {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: u8,
}

#[derive(Serialize)]
struct JsonProjection<'a> {
    ok: bool,
    category: &'a str,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
}

#[allow(dead_code)]
impl CommandOutcome {
    pub fn success(command: Command) -> Self {
        Self::Success { command }
    }

    pub fn refused(command: Command, refusal: Refusal) -> Self {
        Self::Refused { command, refusal }
    }

    pub fn connection_conflict(connector: impl Into<String>, label: impl Into<String>) -> Self {
        Self::refused(
            Command::IntegrationConnect,
            Refusal::ConnectionConflict {
                connector: connector.into(),
                label: label.into(),
            },
        )
    }

    pub fn render(&self, format: OutputFormat) -> serde_json::Result<Projection> {
        let (command, category, connector, label, ok) = match self {
            Self::Success { command } => (*command, "success", None, None, true),
            Self::Refused { command, refusal } => {
                let (connector, label) = match refusal {
                    Refusal::ConnectionConflict { connector, label } => {
                        (Some(connector.as_str()), Some(label.as_str()))
                    }
                    _ => (None, None),
                };
                (*command, refusal.category(), connector, label, false)
            }
        };

        match format {
            OutputFormat::Json => {
                let mut stdout = serde_json::to_string(&JsonProjection {
                    ok,
                    category,
                    command: command.label(),
                    connector,
                    label,
                })?;
                stdout.push('\n');
                Ok(Projection {
                    stdout,
                    stderr: String::new(),
                    exit_status: u8::from(!ok),
                })
            }
            OutputFormat::Human if ok => Ok(Projection {
                stdout: format!("success: {}\n", command.human_label()),
                stderr: String::new(),
                exit_status: 0,
            }),
            OutputFormat::Human => {
                let detail = match (connector, label) {
                    (Some(connector), Some(label)) => {
                        format!("connector '{connector}' with label '{label}'")
                    }
                    _ => command.human_label().to_owned(),
                };
                Ok(Projection {
                    stdout: String::new(),
                    stderr: format!("refused [{category}]: {detail}\n"),
                    exit_status: 1,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: [Command; 7] = [
        Command::ExchangeLocalStart,
        Command::ExchangeLocalStatus,
        Command::ExchangeLocalStop,
        Command::IntegrationConnect,
        Command::IntegrationGrant,
        Command::IntegrationList,
        Command::IntegrationDoctor,
    ];

    #[test]
    fn success_projection_is_stable_in_both_formats() {
        let human = CommandOutcome::success(Command::IntegrationList)
            .render(OutputFormat::Human)
            .unwrap();
        assert_eq!(human.stdout, "success: integration list\n");
        assert_eq!(human.stderr, "");
        assert_eq!(human.exit_status, 0);

        let json = CommandOutcome::success(Command::IntegrationList)
            .render(OutputFormat::Json)
            .unwrap();
        assert_eq!(
            json.stdout,
            "{\"ok\":true,\"category\":\"success\",\"command\":\"integration.list\"}\n"
        );
        assert_eq!(json.stderr, "");
        assert_eq!(json.exit_status, 0);
    }

    #[test]
    fn refusal_categories_and_exit_status_are_closed_and_deterministic() {
        let cases = [
            (Refusal::Refused, "refused"),
            (Refusal::Unavailable, "unavailable"),
            (Refusal::AuthenticationRequired, "authentication_required"),
            (Refusal::Incomplete, "incomplete"),
            (Refusal::Unsupported, "unsupported"),
        ];

        for command in COMMANDS {
            for (refusal, category) in &cases {
                let projection = CommandOutcome::refused(command, refusal.clone())
                    .render(OutputFormat::Json)
                    .unwrap();
                let value: serde_json::Value =
                    serde_json::from_str(projection.stdout.trim()).unwrap();
                assert_eq!(value["ok"], false);
                assert_eq!(value["category"], *category);
                assert_eq!(projection.stderr, "");
                assert_eq!(projection.exit_status, 1);
            }
        }
    }

    #[test]
    fn human_refusals_are_value_free_and_written_to_stderr() {
        let secret = "Bearer vendor-secret-123";
        let field_value = "https://gitlab.example.invalid/private";
        let projection = CommandOutcome::refused(Command::IntegrationConnect, Refusal::Incomplete)
            .render(OutputFormat::Human)
            .unwrap();

        assert_eq!(
            projection.stderr,
            "refused [incomplete]: integration connect\n"
        );
        assert!(projection.stdout.is_empty());
        assert!(!projection.stderr.contains(secret));
        assert!(!projection.stderr.contains(field_value));
        assert_eq!(projection.exit_status, 1);
    }

    #[test]
    fn conflict_names_only_the_connector_and_label() {
        let connector = "gitlab";
        let label = "company";
        let secret = "glpat-not-for-flux";
        let field_name = "endpoint";
        let field_value = "https://gitlab.example.invalid";

        let human = CommandOutcome::connection_conflict(connector, label)
            .render(OutputFormat::Human)
            .unwrap();
        assert_eq!(
            human.stderr,
            "refused [conflict]: connector 'gitlab' with label 'company'\n"
        );
        for forbidden in [secret, field_name, field_value] {
            assert!(!human.stderr.contains(forbidden), "leaked {forbidden}");
        }

        let json = CommandOutcome::connection_conflict(connector, label)
            .render(OutputFormat::Json)
            .unwrap();
        assert_eq!(
            json.stdout,
            "{\"ok\":false,\"category\":\"conflict\",\"command\":\"integration.connect\",\"connector\":\"gitlab\",\"label\":\"company\"}\n"
        );
        for forbidden in [secret, field_name, field_value] {
            assert!(!json.stdout.contains(forbidden), "leaked {forbidden}");
        }
        assert_eq!(json.exit_status, 1);
    }
}
