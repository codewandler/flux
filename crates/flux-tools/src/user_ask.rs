//! `user.ask` — a schema-driven question over an attached human interaction responder.

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_runtime::{
    InteractionCapabilities, InteractionOrigin, PromptAudioRef, Tool, ToolContext, ToolRegistry,
    ToolResult, UserPrompt,
};
use flux_spec::{tool_output_schema, FlowEffect, Idempotency, StagingDisposition, ToolSpec};
use serde_json::Value;

pub const USER_ASK_OP: &str = "user.ask";

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserAskInput {
    /// Content the attached human surface presents as an agent-authored question.
    prompt: UserAskPrompt,
    /// JSON Schema the submitted value must satisfy.
    schema: Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserAskPrompt {
    text: String,
    #[serde(default)]
    audio: Option<UserAskAudio>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserAskAudio {
    asset_id: String,
    media_type: String,
    transcript: String,
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum UserAskInputModeSchema {
    Controls,
    Audio,
    Mixed,
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UserAskOutputSchema {
    Submitted {
        value: Value,
        input_mode: UserAskInputModeSchema,
    },
    Cancelled,
}

struct UserAskTool {
    capabilities: InteractionCapabilities,
}

/// Register `user.ask` only when the assembling host proved a responder is present.
pub fn try_register_user_interaction(
    registry: &mut ToolRegistry,
    capabilities: Option<InteractionCapabilities>,
) -> Result<()> {
    let Some(capabilities) = capabilities else {
        return Ok(());
    };
    registry.try_register_from(
        "flux-tools user interaction operation",
        Arc::new(UserAskTool { capabilities }),
    )
}

#[async_trait]
impl Tool for UserAskTool {
    fn spec(&self) -> ToolSpec {
        let audio = match (
            self.capabilities.prompt_audio,
            self.capabilities.reply_audio,
        ) {
            (true, true) => {
                " This host supports opaque prompt-audio assets and reviewed audio replies."
            }
            (true, false) => {
                " This host can play opaque prompt-audio assets but accepts control replies only."
            }
            (false, true) => {
                " This host accepts reviewed audio replies but cannot play prompt audio; omit prompt.audio."
            }
            (false, false) => " This host supports text and form controls only; omit prompt.audio.",
        };
        let mut spec = ToolSpec::read_only_typed::<UserAskInput>(
            USER_ASK_OP,
            format!(
                "Ask the attached human a question and wait for a JSON value matching the supplied JSON Schema. Use boolean for yes/no, enum for one choice, or a unique array of enum values for multiple choices. Cancellation is explicit and is not approval.{audio}"
            ),
        )
        .with_output_schema(tool_output_schema::<UserAskOutputSchema>());
        spec.idempotency = Idempotency::NonIdempotent;
        spec
    }

    fn staging_disposition(&self) -> StagingDisposition {
        StagingDisposition::Gather
    }

    fn semantic_effects(&self) -> Vec<String> {
        vec![FlowEffect::HumanVisible.tag().to_string()]
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: UserAskInput = crate::parse_params(params, USER_ASK_OP)?;
        let interaction = ctx.user_interaction().ok_or_else(|| {
            Error::Other("user.ask: no user interaction responder is attached".into())
        })?;
        let prompt = UserPrompt {
            text: input.prompt.text,
            audio: input.prompt.audio.map(|audio| PromptAudioRef {
                asset_id: audio.asset_id,
                media_type: audio.media_type,
                transcript: audio.transcript,
            }),
        };
        let response = interaction
            .request(InteractionOrigin::Agent, prompt, input.schema)
            .await?;
        let content = serde_json::to_string(&response)
            .map_err(|error| Error::Other(format!("user.ask: serialize response: {error}")))?;
        Ok(ToolResult::ok(content))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use flux_runtime::{
        InteractionInputMode, InteractionResponse, UserInteraction, UserInteractionRequest,
    };
    use serde_json::json;

    use super::*;

    struct Answer(Mutex<Option<UserInteractionRequest>>);

    #[async_trait]
    impl UserInteraction for Answer {
        fn capabilities(&self) -> InteractionCapabilities {
            InteractionCapabilities::text()
        }

        async fn request(&self, request: UserInteractionRequest) -> Result<InteractionResponse> {
            *self.0.lock().unwrap() = Some(request);
            Ok(InteractionResponse::Submitted {
                value: json!(["staging"]),
                input_mode: InteractionInputMode::Controls,
            })
        }
    }

    #[test]
    fn registration_is_surface_gated_and_metadata_is_honest() {
        let mut absent = ToolRegistry::new();
        try_register_user_interaction(&mut absent, None).unwrap();
        assert!(absent.get(USER_ASK_OP).is_none());

        let mut present = ToolRegistry::new();
        try_register_user_interaction(&mut present, Some(InteractionCapabilities::text())).unwrap();
        let tool = present.get(USER_ASK_OP).unwrap();
        let spec = tool.spec();
        assert_eq!(spec.idempotency, Idempotency::NonIdempotent);
        assert_eq!(tool.staging_disposition(), StagingDisposition::Gather);
        assert_eq!(tool.semantic_effects(), vec!["human_visible"]);
        assert!(spec.description.contains("omit prompt.audio"));
        assert_eq!(
            flux_spec::metadata_violations(&spec, &tool.semantic_effects()),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn operation_returns_the_explicit_status_wrapper() {
        let answer = Arc::new(Answer(Mutex::new(None)));
        let root = std::env::current_dir().unwrap();
        let system = Arc::new(
            flux_system::System::new(flux_system::Workspace::new(root).unwrap())
                .with_worktree_base(crate::test_worktrees::pinned_worktree_base()),
        );
        let ctx = ToolContext::new(system);
        ctx.set_user_interaction(answer.clone());
        let tool = UserAskTool {
            capabilities: InteractionCapabilities::text(),
        };
        let result = tool
            .execute(
                &ctx,
                json!({
                    "prompt": {"text": "where?"},
                    "schema": {
                        "type": "array",
                        "items": {"enum": ["staging", "production"]},
                        "uniqueItems": true
                    }
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).unwrap(),
            json!({"status":"submitted", "value":["staging"], "input_mode":"controls"})
        );
        assert_eq!(
            answer.0.lock().unwrap().as_ref().unwrap().prompt.text,
            "where?"
        );
    }
}
