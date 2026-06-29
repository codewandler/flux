//! Resolving `secret "ENV_NAME"` references in a [`Program`]'s settings.
//!
//! The native-text grammar lowers `secret "NAME"` to the reserved marker `{"$secret":"NAME"}` — never
//! inline plaintext (see `flux_lang::parse`). The host resolves those markers to real values at load,
//! once, from the environment. A missing variable is a clean startup error that names the variable but
//! never any value; resolved secrets live only in memory and are never logged.

use flux_core::{Error, Result};
use flux_lang::program::{as_secret_ref, Program};
use serde_json::Value;

/// Resolve every `secret "NAME"` marker in `program`'s declaration settings to the value of the
/// environment variable `NAME`. Walks the agent / channel / datasource settings bags. A missing
/// variable errors, naming the variable (never its value); on success the program carries no markers.
pub fn resolve_secrets(program: &mut Program) -> Result<()> {
    for a in &mut program.agents {
        resolve_in(&mut a.settings)?;
    }
    for c in &mut program.channels {
        resolve_in(&mut c.settings)?;
    }
    for d in &mut program.datasources {
        resolve_in(&mut d.settings)?;
    }
    Ok(())
}

/// Recursively replace each `{"$secret":"NAME"}` marker with the env value of `NAME`, in place.
fn resolve_in(value: &mut Value) -> Result<()> {
    if let Some(name) = as_secret_ref(value) {
        let name = name.to_string(); // end the borrow of `value` before mutating it
        let resolved = std::env::var(&name).map_err(|_| {
            Error::Config(format!(
                "secret env var `{name}` is not set (referenced via `secret \"{name}\"`)"
            ))
        })?;
        *value = Value::String(resolved);
        return Ok(());
    }
    match value {
        Value::Object(map) => {
            for v in map.values_mut() {
                resolve_in(v)?;
            }
        }
        Value::Array(items) => {
            for v in items {
                resolve_in(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_lang::program::{ChannelDecl, DatasourceDecl};
    use serde_json::json;

    #[test]
    fn resolves_a_channel_secret_from_the_environment() {
        std::env::set_var("FLUX_TEST_SECRET_TOKEN", "s3cr3t");
        let mut program = Program {
            channels: vec![ChannelDecl {
                name: "slack".into(),
                kind: "slack".into(),
                settings: json!({ "bot_token": { "$secret": "FLUX_TEST_SECRET_TOKEN" } }),
            }],
            ..Default::default()
        };
        resolve_secrets(&mut program).unwrap();
        assert_eq!(program.channels[0].settings["bot_token"], json!("s3cr3t"));
    }

    #[test]
    fn a_missing_secret_errors_naming_the_var_not_the_value() {
        let mut program = Program {
            datasources: vec![DatasourceDecl {
                name: "d".into(),
                kind: "markdown".into(),
                path: None,
                settings: json!({ "token": { "$secret": "FLUX_TEST_DEFINITELY_UNSET_VAR" } }),
            }],
            ..Default::default()
        };
        let err = resolve_secrets(&mut program).unwrap_err().to_string();
        assert!(
            err.contains("FLUX_TEST_DEFINITELY_UNSET_VAR"),
            "names the var: {err}"
        );
    }

    #[test]
    fn resolves_secrets_nested_in_records() {
        std::env::set_var("FLUX_TEST_NESTED_SECRET", "deep");
        let mut settings =
            json!({ "auth": { "headers": { "x-key": { "$secret": "FLUX_TEST_NESTED_SECRET" } } } });
        resolve_in(&mut settings).unwrap();
        assert_eq!(settings["auth"]["headers"]["x-key"], json!("deep"));
    }
}
