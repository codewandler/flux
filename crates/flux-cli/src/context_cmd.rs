use super::*;

/// Render the layer table as header + one line per layer, each column widened to its own content.
///
/// Columns are measured rather than hardcoded because an id like `repository.policy.AGENTS.md` is
/// wider than any fixed guess, and one overflowing cell shifts every column after it on that row.
///
/// The enum columns are stringified *before* padding, deliberately. `{:18?}` looks like it pads to
/// eighteen, but a derived `Debug` writes straight to the formatter and ignores width entirely — so
/// the previous version emitted `Harness Harness Static` with the header still spaced for columns
/// that were never padded. Formatting the value first, then padding with Display, is what makes the
/// width apply.
fn context_table(layers: &[PromptLayer]) -> Vec<String> {
    const HEADERS: [&str; 7] = ["ID", "KIND", "TRUST", "CACHE", "BYTES", "SHA256", "SOURCE"];

    let cells: Vec<[String; 7]> = layers
        .iter()
        .map(|layer| {
            let manifest = layer.manifest();
            [
                manifest.id.to_string(),
                format!("{:?}", manifest.kind),
                format!("{:?}", manifest.trust),
                format!("{:?}", manifest.cache_class),
                manifest.bytes.to_string(),
                manifest.sha256[..12].to_string(),
                manifest.source.as_deref().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    let mut widths = HEADERS.map(str::len);
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    // BYTES is the one numeric column, so it right-aligns; the trailing column is never padded,
    // which keeps the line from carrying invisible whitespace to the end.
    let render = |row: &[String; 7]| {
        let mut line = String::new();
        for (index, (cell, width)) in row.iter().zip(widths).enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            match index {
                4 => line.push_str(&format!("{cell:>width$}")),
                6 => line.push_str(cell),
                _ => line.push_str(&format!("{cell:<width$}")),
            }
        }
        line
    };

    let header = render(&HEADERS.map(str::to_string));
    std::iter::once(header)
        .chain(cells.iter().map(render))
        .collect()
}

/// Resolve a layer selector to exactly one layer, or explain what it could have meant.
///
/// An exact id wins outright, so a short id can always be named even when it prefixes a longer one.
/// Otherwise a prefix must be unambiguous: guessing between candidates would show the wrong body
/// under the right heading, which is worse than asking again.
fn select_layer(layers: Vec<PromptLayer>, selector: &str) -> Result<PromptLayer> {
    let available = || {
        layers
            .iter()
            .map(|layer| layer.manifest().id)
            .collect::<Vec<_>>()
            .join(", ")
    };

    if let Some(exact) = layers.iter().position(|l| l.manifest().id == selector) {
        return Ok(layers.into_iter().nth(exact).expect("index just found"));
    }

    let matched: Vec<usize> = layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.manifest().id.starts_with(selector))
        .map(|(index, _)| index)
        .collect();

    match matched.as_slice() {
        [only] => Ok(layers.into_iter().nth(*only).expect("index just found")),
        [] => anyhow::bail!(
            "no context layer `{selector}`. Layers here: {}",
            available()
        ),
        several => {
            let names = several
                .iter()
                .map(|&index| layers[index].manifest().id)
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("`{selector}` matches more than one layer: {names}")
        }
    }
}

/// Render prompt provenance without resolving a provider, loading plugins, or starting a turn.
pub(super) async fn run_context(action: ContextAction) -> Result<()> {
    match action {
        ContextAction::Show {
            layer: selector,
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
            let all = spec.effective_prompt_layers_for_tools(&tools);

            // Naming one layer is a request for its detail, so its body comes along without
            // `--body` — asking for one layer and getting only its hash back would be a dead end.
            let (layers, body) = match selector.as_deref() {
                Some(selector) => (vec![select_layer(all, selector)?], true),
                None => (all, body),
            };
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

            let table = context_table(&layers);
            let (header, rows) = table.split_first().expect("table always has a header");
            println!("{header}");
            for (row, layer) in rows.iter().zip(&layers) {
                println!("{row}");
                if body {
                    println!("\n{}\n", layer.body.trim_end());
                }
            }
            Ok(())
        }
    }
}
