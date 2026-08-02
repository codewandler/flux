use super::*;

/// Render prompt provenance without resolving a provider, loading plugins, or starting a turn.
pub(super) async fn run_context(action: ContextAction) -> Result<()> {
    match action {
        ContextAction::Show {
            profile,
            tools,
            body,
            json,
        } => {
            let cwd = std::env::current_dir().context("current dir")?;
            let spec = AgentSpec {
                profile: profile.into(),
                prompt_layers: project_prompt_layers(&cwd).await?,
                ..AgentSpec::new("context-inspection")
            };
            let layers = spec.effective_prompt_layers_for_tools(&tools);
            if json {
                let rows = layers
                    .iter()
                    .map(|layer| {
                        let mut row = serde_json::to_value(layer.manifest())
                            .expect("prompt manifest is serializable");
                        if body {
                            row["body"] = serde_json::Value::String(layer.body.clone());
                        }
                        row
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }

            println!("ID                       KIND               TRUST        CACHE      BYTES   SHA256        SOURCE");
            for layer in &layers {
                let manifest = layer.manifest();
                println!(
                    "{:24} {:18?} {:12?} {:10?} {:7} {}  {}",
                    manifest.id,
                    manifest.kind,
                    manifest.trust,
                    manifest.cache_class,
                    manifest.bytes,
                    &manifest.sha256[..12],
                    manifest.source.as_deref().unwrap_or("-")
                );
                if body {
                    println!("\n{}\n", layer.body.trim_end());
                }
            }
            Ok(())
        }
    }
}
