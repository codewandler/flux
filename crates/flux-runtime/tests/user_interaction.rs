use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_runtime::{
    InteractionCapabilities, InteractionInputMode, InteractionOrigin, InteractionResponse,
    ToolContext, UserInteraction, UserInteractionRequest, UserPrompt,
};
use serde_json::json;

#[derive(Default)]
struct FixtureInteraction {
    seen: Mutex<Vec<UserInteractionRequest>>,
}

struct HangingInteraction;

#[async_trait]
impl UserInteraction for HangingInteraction {
    fn capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities::text()
    }

    async fn request(
        &self,
        _request: UserInteractionRequest,
    ) -> flux_core::Result<InteractionResponse> {
        std::future::pending().await
    }
}

#[async_trait]
impl UserInteraction for FixtureInteraction {
    fn capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities::text()
    }

    async fn request(
        &self,
        request: UserInteractionRequest,
    ) -> flux_core::Result<InteractionResponse> {
        self.seen.lock().unwrap().push(request);
        Ok(InteractionResponse::Submitted {
            value: json!(["staging", "production"]),
            input_mode: InteractionInputMode::Controls,
        })
    }
}

#[tokio::test]
async fn turn_cancellation_stops_a_waiting_interaction() {
    let root = std::env::current_dir().unwrap();
    let system = Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&root).unwrap(),
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    let ctx = ToolContext::new(system);
    ctx.set_cancel(cancel.clone());
    ctx.set_user_interaction(Arc::new(HangingInteraction));

    let reporter = ctx.user_interaction().unwrap();
    let waiting = reporter.request(
        InteractionOrigin::Agent,
        UserPrompt::text("wait"),
        json!({"type":"boolean"}),
    );
    tokio::pin!(waiting);
    tokio::select! {
        _ = &mut waiting => panic!("interaction completed before cancellation"),
        _ = tokio::task::yield_now() => {}
    }
    cancel.cancel();
    assert!(waiting
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled with the turn"));
}

#[tokio::test]
async fn user_ask_waits_for_a_schema_valid_response() {
    let root = std::env::current_dir().unwrap();
    let system = Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&root).unwrap(),
    ));
    let interaction = Arc::new(FixtureInteraction::default());
    let ctx = ToolContext::new(system);
    ctx.redactor.add_secret("hidden-interaction-value");
    ctx.set_user_interaction(interaction.clone());

    let response = ctx
        .user_interaction()
        .expect("installed interaction")
        .request(
            InteractionOrigin::Agent,
            UserPrompt::text("Choose environments; do not show hidden-interaction-value"),
            json!({
                "type": "array",
                "items": {"enum": ["staging", "production"]},
                "uniqueItems": true
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        response,
        InteractionResponse::Submitted {
            value: json!(["staging", "production"]),
            input_mode: InteractionInputMode::Controls,
        }
    );
    let seen = interaction.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].prompt.text.contains("hidden-interaction-value"));
}
