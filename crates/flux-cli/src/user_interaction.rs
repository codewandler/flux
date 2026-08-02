//! Terminal implementation of the runtime's typed user-interaction contract.

use super::*;

use flux_runtime::{
    InteractionCapabilities, InteractionInputMode, InteractionResponse, UserInteraction,
    UserInteractionRequest,
};
use futures::StreamExt;
use tokio::io::AsyncBufReadExt;

/// The plain terminal responder used by `flux run` and the REPL.
pub(super) struct StdinUserInteraction;

#[async_trait]
impl UserInteraction for StdinUserInteraction {
    fn capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities::text()
    }

    async fn request(
        &self,
        request: UserInteractionRequest,
    ) -> flux_core::Result<InteractionResponse> {
        request_async(&request).await
    }
}

async fn request_async(request: &UserInteractionRequest) -> flux_core::Result<InteractionResponse> {
    // Uses the same line-ownership gate as approval prompts and the live spinner. A user question
    // and a policy decision therefore cannot paint over each other.
    let _line = PromptGate::global().acquire().await;
    eprintln!("\n{}", sanitize_prompt(&request.prompt.text));
    if request.prompt.audio.is_some() {
        return Err(flux_core::Error::Other(
            "this terminal supports text prompts only; omit prompt.audio".into(),
        ));
    }

    jsonschema::validator_for(&request.schema).map_err(|error| {
        flux_core::Error::Other(format!("user interaction schema did not compile: {error}"))
    })?;
    let mut input = TerminalInput::new()?;
    loop {
        let Some(value) = read_value(&request.schema, &mut input).await? else {
            return Ok(InteractionResponse::Cancelled);
        };
        if serde_json::to_vec(&value)
            .map(|encoded| encoded.len() > flux_runtime::MAX_INTERACTION_RESPONSE_BYTES)
            .unwrap_or(true)
        {
            eprintln!(
                "{} response exceeds {} bytes",
                style::red("invalid:"),
                flux_runtime::MAX_INTERACTION_RESPONSE_BYTES
            );
            continue;
        }
        let validator = jsonschema::validator_for(&request.schema).map_err(|error| {
            flux_core::Error::Other(format!("user interaction schema did not compile: {error}"))
        })?;
        if validator.is_valid(&value) {
            return Ok(InteractionResponse::Submitted {
                value,
                input_mode: InteractionInputMode::Controls,
            });
        }
        let detail = validator
            .iter_errors(&value)
            .next()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "value does not match the schema".into());
        eprintln!("{} {detail}", style::red("invalid:"));
    }
}

async fn read_value(
    schema: &serde_json::Value,
    input: &mut TerminalInput,
) -> flux_core::Result<Option<serde_json::Value>> {
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("boolean") {
        loop {
            let Some(line) = prompt_line(input, "[y]es / [n]o · /cancel: ").await? else {
                return Ok(None);
            };
            match line.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => return Ok(Some(serde_json::Value::Bool(true))),
                "n" | "no" => return Ok(Some(serde_json::Value::Bool(false))),
                "/cancel" => return Ok(None),
                _ => eprintln!("enter y, n, or /cancel"),
            }
        }
    }

    if let Some(options) = enum_values(schema) {
        print_options(&options);
        loop {
            let Some(line) = prompt_line(input, "choose a number · /cancel: ").await? else {
                return Ok(None);
            };
            if line.trim() == "/cancel" {
                return Ok(None);
            }
            if let Ok(index) = line.trim().parse::<usize>() {
                if let Some(value) = index.checked_sub(1).and_then(|i| options.get(i)) {
                    return Ok(Some(value.clone()));
                }
            }
            eprintln!("choose one of 1..={}", options.len());
        }
    }

    if let Some(options) = multi_enum_values(schema) {
        print_options(&options);
        loop {
            let Some(line) =
                prompt_line(input, "choose comma-separated numbers · /cancel: ").await?
            else {
                return Ok(None);
            };
            if line.trim() == "/cancel" {
                return Ok(None);
            }
            let mut picked = Vec::new();
            let mut valid = true;
            for raw in line
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                match raw
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| options.get(i))
                {
                    Some(value) if !picked.contains(value) => picked.push(value.clone()),
                    Some(_) => {}
                    None => valid = false,
                }
            }
            if valid {
                return Ok(Some(serde_json::Value::Array(picked)));
            }
            eprintln!("choose only numbers in 1..={}", options.len());
        }
    }

    if let Some(fields) = flat_form_fields(schema) {
        let required: std::collections::HashSet<&str> = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let mut object = serde_json::Map::new();
        for (name, field_schema) in fields {
            let title = field_schema
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(name);
            eprintln!("{}", style::bold(&sanitize_prompt(title)));
            if let Some(description) = field_schema
                .get("description")
                .and_then(serde_json::Value::as_str)
            {
                eprintln!("  {}", sanitize_prompt(description));
            }
            match read_form_field(field_schema, required.contains(name), input).await? {
                FormAnswer::Value(value) => {
                    object.insert(name.to_string(), value);
                }
                FormAnswer::Skip => {}
                FormAnswer::Cancel => return Ok(None),
            }
        }
        return Ok(Some(serde_json::Value::Object(object)));
    }

    loop {
        let Some(line) = prompt_line(input, "JSON value · /cancel: ").await? else {
            return Ok(None);
        };
        if line.trim() == "/cancel" {
            return Ok(None);
        }
        match serde_json::from_str(&line) {
            Ok(value) => return Ok(Some(value)),
            Err(error) => eprintln!("{} {error}", style::red("invalid JSON:")),
        }
    }
}

enum FormAnswer {
    Value(serde_json::Value),
    Skip,
    Cancel,
}

async fn read_form_field(
    schema: &serde_json::Value,
    required: bool,
    input: &mut TerminalInput,
) -> flux_core::Result<FormAnswer> {
    if let Some(default) = schema.get("default") {
        eprintln!("  default: {}", option_label(default));
    }
    loop {
        let optional = if required { "" } else { " · Enter skips" };
        let answer = if schema.get("type").and_then(serde_json::Value::as_str) == Some("boolean") {
            prompt_line(input, &format!("  [y]es / [n]o{optional} · /cancel: ")).await?
        } else if let Some(options) = enum_values(schema) {
            print_options(&options);
            let Some(line) =
                prompt_line(input, &format!("  choose a number{optional} · /cancel: ")).await?
            else {
                return Ok(FormAnswer::Cancel);
            };
            if line.trim() == "/cancel" {
                return Ok(FormAnswer::Cancel);
            }
            if line.trim().is_empty() {
                if let Some(default) = schema.get("default") {
                    return Ok(FormAnswer::Value(default.clone()));
                }
                if !required {
                    return Ok(FormAnswer::Skip);
                }
            }
            if let Ok(index) = line.trim().parse::<usize>() {
                if let Some(value) = index.checked_sub(1).and_then(|i| options.get(i)) {
                    return Ok(FormAnswer::Value(value.clone()));
                }
            }
            eprintln!("choose one of 1..={}", options.len());
            continue;
        } else if let Some(options) = multi_enum_values(schema) {
            print_options(&options);
            let Some(line) = prompt_line(
                input,
                &format!("  choose comma-separated numbers{optional} · /cancel: "),
            )
            .await?
            else {
                return Ok(FormAnswer::Cancel);
            };
            if line.trim() == "/cancel" {
                return Ok(FormAnswer::Cancel);
            }
            if line.trim().is_empty() {
                if let Some(default) = schema.get("default") {
                    return Ok(FormAnswer::Value(default.clone()));
                }
                if !required {
                    return Ok(FormAnswer::Skip);
                }
            }
            let mut values = Vec::new();
            let mut valid = true;
            for raw in line
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                match raw
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| options.get(i))
                {
                    Some(value) if !values.contains(value) => values.push(value.clone()),
                    Some(_) => {}
                    None => valid = false,
                }
            }
            if valid {
                return Ok(FormAnswer::Value(serde_json::Value::Array(values)));
            }
            eprintln!("choose only numbers in 1..={}", options.len());
            continue;
        } else {
            prompt_line(input, &format!("  value{optional} · /cancel: ")).await?
        };

        let Some(line) = answer else {
            return Ok(FormAnswer::Cancel);
        };
        let raw = line.trim();
        if raw == "/cancel" {
            return Ok(FormAnswer::Cancel);
        }
        if raw.is_empty() {
            if let Some(default) = schema.get("default") {
                return Ok(FormAnswer::Value(default.clone()));
            }
            if !required {
                return Ok(FormAnswer::Skip);
            }
        }
        match schema.get("type").and_then(serde_json::Value::as_str) {
            Some("boolean") => match raw.to_ascii_lowercase().as_str() {
                "y" | "yes" => return Ok(FormAnswer::Value(serde_json::Value::Bool(true))),
                "n" | "no" => return Ok(FormAnswer::Value(serde_json::Value::Bool(false))),
                _ => eprintln!("enter y or n"),
            },
            Some("string") => {
                return Ok(FormAnswer::Value(serde_json::Value::String(
                    raw.to_string(),
                )))
            }
            Some("integer") => match raw.parse::<i64>() {
                Ok(value) => return Ok(FormAnswer::Value(value.into())),
                Err(_) => eprintln!("enter an integer"),
            },
            Some("number") => match raw
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
            {
                Some(value) => return Ok(FormAnswer::Value(value.into())),
                None => eprintln!("enter a finite number"),
            },
            _ => unreachable!("flat_form_fields admitted only supported controls"),
        }
    }
}

fn flat_form_fields(schema: &serde_json::Value) -> Option<Vec<(&str, &serde_json::Value)>> {
    if schema.get("type")?.as_str()? != "object" {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    let fields: Vec<_> = properties
        .iter()
        .map(|(name, schema)| (name.as_str(), schema))
        .collect();
    (!fields.is_empty() && fields.iter().all(|(_, schema)| native_field(schema))).then_some(fields)
}

fn native_field(schema: &serde_json::Value) -> bool {
    enum_values(schema).is_some()
        || multi_enum_values(schema).is_some()
        || matches!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("boolean" | "string" | "integer" | "number")
        )
}

async fn prompt_line(input: &mut TerminalInput, prompt: &str) -> flux_core::Result<Option<String>> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    input.read_line().await
}

enum TerminalInput {
    Tty {
        events: crossterm::event::EventStream,
        _raw: crate::session::RawModeGuard,
    },
    Pipe {
        lines: tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>,
    },
}

impl TerminalInput {
    fn new() -> flux_core::Result<Self> {
        if std::io::stdin().is_terminal() {
            let raw = crate::session::RawModeGuard::enable().map_err(|error| {
                flux_core::Error::Other(format!("enable raw mode for user interaction: {error}"))
            })?;
            Ok(Self::Tty {
                events: crossterm::event::EventStream::new(),
                _raw: raw,
            })
        } else {
            Ok(Self::Pipe {
                lines: tokio::io::BufReader::new(tokio::io::stdin()).lines(),
            })
        }
    }

    async fn read_line(&mut self) -> flux_core::Result<Option<String>> {
        match self {
            Self::Pipe { lines } => lines.next_line().await.map_err(|error| {
                flux_core::Error::Other(format!("read user interaction response: {error}"))
            }),
            Self::Tty { events, .. } => {
                use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
                let mut line = String::new();
                loop {
                    let event = events.next().await.transpose().map_err(|error| {
                        flux_core::Error::Other(format!("read user interaction response: {error}"))
                    })?;
                    let Some(event) = event else {
                        return Ok(None);
                    };
                    match event {
                        Event::Paste(text) => {
                            eprint!("{text}");
                            std::io::stderr().flush().ok();
                            line.push_str(&text);
                        }
                        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                            KeyCode::Enter => {
                                eprintln!();
                                return Ok(Some(line));
                            }
                            KeyCode::Esc => {
                                eprintln!();
                                return Ok(Some("/cancel".into()));
                            }
                            KeyCode::Backspace => {
                                if line.pop().is_some() {
                                    eprint!("\u{8} \u{8}");
                                    std::io::stderr().flush().ok();
                                }
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                eprintln!();
                                return Ok(Some("/cancel".into()));
                            }
                            KeyCode::Char(ch)
                                if !key.modifiers.intersects(
                                    KeyModifiers::CONTROL
                                        | KeyModifiers::ALT
                                        | KeyModifiers::SUPER
                                        | KeyModifiers::HYPER
                                        | KeyModifiers::META,
                                ) =>
                            {
                                eprint!("{ch}");
                                std::io::stderr().flush().ok();
                                line.push(ch);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
        }
    }
}

fn enum_values(schema: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .filter(|values| !values.is_empty())
        .cloned()
        .or_else(|| {
            schema
                .get("oneOf")?
                .as_array()?
                .iter()
                .map(|branch| branch.get("const").cloned())
                .collect::<Option<Vec<_>>>()
                .filter(|values| !values.is_empty())
        })
}

fn multi_enum_values(schema: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    (schema.get("type")?.as_str()? == "array"
        && schema
            .get("uniqueItems")
            .and_then(serde_json::Value::as_bool)
            == Some(true))
    .then(|| enum_values(schema.get("items")?))?
}

fn print_options(options: &[serde_json::Value]) {
    for (index, option) in options.iter().enumerate() {
        eprintln!("  {}. {}", index + 1, option_label(option));
    }
}

fn option_label(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(sanitize_prompt)
        .unwrap_or_else(|| value.to_string())
}

/// Strip terminal control sequences/control characters from model-authored display text.
fn sanitize_prompt(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.next_if_eq(&'[').is_some() {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if !ch.is_control() || matches!(ch, '\n' | '\t') {
            clean.push(ch);
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn common_schemas_map_to_native_terminal_choices() {
        assert_eq!(enum_values(&json!({"enum":["a","b"]})).unwrap().len(), 2);
        assert_eq!(
            multi_enum_values(&json!({
                "type":"array", "uniqueItems":true, "items":{"enum":["a","b"]}
            }))
            .unwrap()
            .len(),
            2
        );
        assert!(multi_enum_values(&json!({"type":"array","items":{"enum":["a"]}})).is_none());
        assert!(flat_form_fields(&json!({
            "type":"object",
            "properties": {
                "ship":{"type":"boolean"},
                "env":{"enum":["stage","prod"]},
                "note":{"type":"string"}
            }
        }))
        .is_some());
    }

    #[test]
    fn prompts_cannot_write_terminal_controls() {
        assert_eq!(sanitize_prompt("ok\u{1b}[31m fake\u{7}"), "ok fake");
    }
}
