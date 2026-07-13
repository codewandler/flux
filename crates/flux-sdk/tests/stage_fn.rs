use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use flux_flow::ast::{DraftAst, Node, TypeRef};
use flux_flow::registry::OpRegistry;
use flux_runtime::{AllowApprover, Executor, PermissionManager};
use flux_sdk::stage_fn;
use flux_sdk::tools::{ToolContext, ToolRegistry};
use flux_system::{System, Workspace};

#[derive(Debug, Deserialize, JsonSchema)]
struct ScoreInput {
    subject: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ScoreOutput {
    label: String,
    score: u32,
}

#[tokio::test]
async fn stage_fn_registers_independent_input_and_output_contracts() {
    let stage = stage_fn("score", "score a subject", |input: ScoreInput| async move {
        Ok::<_, String>(ScoreOutput {
            score: input.subject.len() as u32,
            label: "measured".into(),
        })
    });
    let spec = stage.spec();
    assert_eq!(spec.input_schema["required"], json!(["subject"]));
    let output = spec.output_schema.as_ref().expect("typed output schema");
    assert_eq!(output["required"], json!(["label", "score"]));
    assert!(output["properties"].get("subject").is_none());
    assert_eq!(output["x-flux-type"], "ScoreOutput");

    let root = std::env::temp_dir().join(format!("flux-sdk-stage-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(stage);
    let catalog = OpRegistry::new(&registry);
    assert_eq!(
        catalog.get("score").expect("score signature").output,
        TypeRef::Named("ScoreOutput".into()),
        "the registered output schema must become the analyzer's inferred bind type"
    );
    let flow = DraftAst {
        body: vec![Node::Bind {
            name: "scored".into(),
            value: Box::new(Node::Call {
                op: "score".into(),
                args: vec![Node::Obj {
                    fields: [(
                        "subject".into(),
                        Box::new(Node::Lit {
                            value: json!("flux"),
                        }),
                    )]
                    .into_iter()
                    .collect(),
                }],
            }),
            ty: None,
            effect: None,
        }],
        ..Default::default()
    };
    flux_flow::analyze::lower(&flow, &catalog, &Default::default())
        .expect("typed output bind analyzes");
    drop(catalog);
    let executor = Executor::new(
        registry,
        PermissionManager::from_rules(&["score".into()], &[]),
        Arc::new(AllowApprover),
        ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
    );
    let result = executor.dispatch("score", json!({"subject": "flux"})).await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.content).unwrap(),
        json!({"label": "measured", "score": 4})
    );
    std::fs::remove_dir_all(root).ok();
}
