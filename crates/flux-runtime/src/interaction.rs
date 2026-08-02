//! Typed request/reply interaction with an attached human surface.
//!
//! This is deliberately separate from [`crate::Approver`]: a person's answer to a question is
//! data, never authorization. Hosts render requests and collect input; this module owns the bounded
//! schema contract, redaction boundary and defensive response validation.

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_secret::{names_a_secret, Redactor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub const MAX_INTERACTION_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_INTERACTION_PROMPT_BYTES: usize = 8 * 1024;
pub const MAX_INTERACTION_RESPONSE_BYTES: usize = 16 * 1024;
pub const MAX_INTERACTION_SCHEMA_DEPTH: usize = 32;
pub const MAX_INTERACTION_CONTROLS: usize = 128;

/// Which optional media paths an installed host can honor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionCapabilities {
    pub prompt_audio: bool,
    pub reply_audio: bool,
}

impl InteractionCapabilities {
    pub const fn text() -> Self {
        Self {
            prompt_audio: false,
            reply_audio: false,
        }
    }

    pub const fn with_audio() -> Self {
        Self {
            prompt_audio: true,
            reply_audio: true,
        }
    }
}

/// Runtime-owned attribution. Prompt content cannot choose this trusted-chrome label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum InteractionOrigin {
    Agent,
}

/// An opaque host-owned audio asset. It is never a path, URL, bearer token or byte payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAudioRef {
    pub asset_id: String,
    pub media_type: String,
    pub transcript: String,
}

/// Content displayed with a schema-driven question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<PromptAudioRef>,
}

impl UserPrompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            audio: None,
        }
    }
}

/// One validated request handed to a surface implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInteractionRequest {
    pub origin: InteractionOrigin,
    pub prompt: UserPrompt,
    pub schema: Value,
}

/// How the host says the reviewed value was entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionInputMode {
    Controls,
    Audio,
    Mixed,
}

/// A human answer. Raw audio is intentionally impossible to put in this type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InteractionResponse {
    Submitted {
        value: Value,
        input_mode: InteractionInputMode,
    },
    Cancelled,
}

/// Host implementation of a user-facing question surface.
#[async_trait]
pub trait UserInteraction: Send + Sync {
    fn capabilities(&self) -> InteractionCapabilities;

    async fn request(&self, request: UserInteractionRequest) -> Result<InteractionResponse>;
}

/// The only handle a tool receives. Requests cross the redaction and validation boundary here.
#[derive(Clone)]
pub struct UserInteractionReporter {
    redactor: Redactor,
    interaction: Arc<dyn UserInteraction>,
    cancel: Option<CancellationToken>,
}

impl std::fmt::Debug for UserInteractionReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserInteractionReporter")
            .field("capabilities", &self.interaction.capabilities())
            .finish_non_exhaustive()
    }
}

impl UserInteractionReporter {
    pub fn capabilities(&self) -> InteractionCapabilities {
        self.interaction.capabilities()
    }

    /// Validate and redact a request, await the host, then validate its response again.
    pub async fn request(
        &self,
        origin: InteractionOrigin,
        mut prompt: UserPrompt,
        schema: Value,
    ) -> Result<InteractionResponse> {
        validate_prompt(&prompt, self.interaction.capabilities(), &self.redactor)?;
        validate_schema(&schema, &self.redactor)?;

        prompt.text = self.redactor.redact(&prompt.text);
        if let Some(audio) = prompt.audio.as_mut() {
            audio.transcript = self.redactor.redact(&audio.transcript);
        }
        let request = UserInteractionRequest {
            origin,
            prompt,
            schema: schema.clone(),
        };
        let response = match &self.cancel {
            Some(cancel) => {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(Error::Other("user interaction cancelled with the turn".into()));
                    }
                    response = self.interaction.request(request) => response?,
                }
            }
            None => self.interaction.request(request).await?,
        };
        validate_response(
            &schema,
            &response,
            self.interaction.capabilities(),
            &self.redactor,
        )?;
        Ok(response)
    }
}

pub(crate) fn reporter(
    redactor: Redactor,
    interaction: Arc<dyn UserInteraction>,
    cancel: Option<CancellationToken>,
) -> UserInteractionReporter {
    UserInteractionReporter {
        redactor,
        interaction,
        cancel,
    }
}

fn validate_prompt(
    prompt: &UserPrompt,
    capabilities: InteractionCapabilities,
    redactor: &Redactor,
) -> Result<()> {
    if prompt.text.len() > MAX_INTERACTION_PROMPT_BYTES {
        return Err(Error::Other(format!(
            "user interaction prompt exceeds {MAX_INTERACTION_PROMPT_BYTES} bytes"
        )));
    }
    let Some(audio) = &prompt.audio else {
        return Ok(());
    };
    if !capabilities.prompt_audio {
        return Err(Error::Other(
            "this user interaction surface does not support prompt audio".into(),
        ));
    }
    if audio.transcript.len() > MAX_INTERACTION_PROMPT_BYTES {
        return Err(Error::Other(format!(
            "user interaction audio transcript exceeds {MAX_INTERACTION_PROMPT_BYTES} bytes"
        )));
    }
    if audio.asset_id.is_empty()
        || audio.asset_id.len() > 128
        || !audio
            .asset_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(Error::Other(
            "user interaction audio asset_id must be 1-128 safe identifier characters".into(),
        ));
    }
    if redactor.redact(&audio.asset_id) != audio.asset_id {
        return Err(Error::Other(
            "user interaction audio asset_id resembles registered secret material".into(),
        ));
    }
    if !audio.media_type.starts_with("audio/")
        || audio.media_type.len() > 128
        || !audio
            .media_type
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'+' | b'-'))
    {
        return Err(Error::Other(
            "user interaction audio media_type must be a bounded audio/* type".into(),
        ));
    }
    Ok(())
}

fn validate_schema(schema: &Value, redactor: &Redactor) -> Result<()> {
    if !schema.is_object() && !schema.is_boolean() {
        return Err(Error::Other(
            "user interaction schema must be a JSON Schema object or boolean".into(),
        ));
    }
    let encoded = serde_json::to_string(schema)
        .map_err(|error| Error::Other(format!("serialize user interaction schema: {error}")))?;
    if encoded.len() > MAX_INTERACTION_SCHEMA_BYTES {
        return Err(Error::Other(format!(
            "user interaction schema exceeds {MAX_INTERACTION_SCHEMA_BYTES} bytes"
        )));
    }
    if redactor.redact(&encoded) != encoded {
        return Err(Error::Other(
            "user interaction schema contains registered or credential-shaped secret material"
                .into(),
        ));
    }
    let mut controls = 0usize;
    inspect_schema(schema, 0, &mut controls)?;
    jsonschema::validator_for(schema)
        .map_err(|error| Error::Other(format!("invalid user interaction schema: {error}")))?;
    Ok(())
}

fn inspect_schema(value: &Value, depth: usize, controls: &mut usize) -> Result<()> {
    if depth > MAX_INTERACTION_SCHEMA_DEPTH {
        return Err(Error::Other(format!(
            "user interaction schema exceeds depth {MAX_INTERACTION_SCHEMA_DEPTH}"
        )));
    }
    match value {
        Value::Object(object) => {
            if object.get("writeOnly").and_then(Value::as_bool) == Some(true)
                || object.get("format").and_then(Value::as_str) == Some("password")
            {
                return Err(Error::Other(
                    "user.ask cannot collect secrets; use the secret store".into(),
                ));
            }
            for keyword in ["$ref", "$dynamicRef", "$recursiveRef"] {
                if let Some(reference) = object.get(keyword).and_then(Value::as_str) {
                    if !reference.starts_with('#') {
                        return Err(Error::Other(format!(
                            "user interaction schemas may use local {keyword} values only"
                        )));
                    }
                }
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for name in properties.keys() {
                    if names_a_secret(name) {
                        return Err(Error::Other(format!(
                            "user.ask cannot collect secret-shaped field `{name}`; use the secret store"
                        )));
                    }
                }
                *controls = controls.saturating_add(properties.len());
            }
            if let Some(options) = object.get("enum").and_then(Value::as_array) {
                *controls = controls.saturating_add(options.len());
            }
            if let Some(options) = object.get("oneOf").and_then(Value::as_array) {
                *controls = controls.saturating_add(options.len());
            }
            if *controls > MAX_INTERACTION_CONTROLS {
                return Err(Error::Other(format!(
                    "user interaction schema exceeds {MAX_INTERACTION_CONTROLS} fields/options"
                )));
            }
            for child in object.values() {
                inspect_schema(child, depth + 1, controls)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                inspect_schema(child, depth + 1, controls)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_response(
    schema: &Value,
    response: &InteractionResponse,
    capabilities: InteractionCapabilities,
    redactor: &Redactor,
) -> Result<()> {
    let InteractionResponse::Submitted { value, input_mode } = response else {
        return Ok(());
    };
    if matches!(
        input_mode,
        InteractionInputMode::Audio | InteractionInputMode::Mixed
    ) && !capabilities.reply_audio
    {
        return Err(Error::Other(
            "user interaction responder reported audio input without audio capability".into(),
        ));
    }
    let encoded = serde_json::to_string(value)
        .map_err(|error| Error::Other(format!("serialize user interaction response: {error}")))?;
    if encoded.len() > MAX_INTERACTION_RESPONSE_BYTES {
        return Err(Error::Other(format!(
            "user interaction response exceeds {MAX_INTERACTION_RESPONSE_BYTES} bytes"
        )));
    }
    if redactor.redact(&encoded) != encoded {
        return Err(Error::Other(
            "user interaction response contains registered or credential-shaped secret material; use the secret store"
                .into(),
        ));
    }
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| Error::Other(format!("invalid user interaction schema: {error}")))?;
    validator.validate(value).map_err(|error| {
        Error::Other(format!(
            "user interaction responder returned a value that does not match the schema: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remote_refs_and_secret_fields_fail_closed() {
        let redactor = Redactor::new();
        assert!(
            validate_schema(&json!({"$ref": "https://example.test/schema"}), &redactor)
                .unwrap_err()
                .to_string()
                .contains("local $ref")
        );
        assert!(validate_schema(
            &json!({"$dynamicRef": "https://example.test/schema"}),
            &redactor
        )
        .unwrap_err()
        .to_string()
        .contains("local $dynamicRef"));
        assert!(validate_schema(
            &json!({"type":"object", "properties":{"api_token":{"type":"string"}}}),
            &redactor
        )
        .unwrap_err()
        .to_string()
        .contains("secret-shaped"));
    }

    #[test]
    fn submitted_value_is_validated_and_secret_scanned() {
        let schema = json!({"type":"boolean"});
        let redactor = Redactor::new();
        assert!(validate_response(
            &schema,
            &InteractionResponse::Submitted {
                value: json!("yes"),
                input_mode: InteractionInputMode::Controls,
            },
            InteractionCapabilities::text(),
            &redactor,
        )
        .is_err());
    }
}
