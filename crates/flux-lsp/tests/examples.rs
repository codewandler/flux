use flux_lang::ast::DraftAst;
use flux_lang::program::Module;
use flux_lsp::convert::LineIndex;

fn executable_flows(module: &Module) -> Vec<&DraftAst> {
    match module {
        Module::Flow(flow) => vec![flow],
        Module::Program(program) => program
            .agent_loops
            .iter()
            .map(|agent_loop| &agent_loop.flow)
            .chain(program.journeys.iter().map(|journey| &journey.flow))
            .chain(program.ops.iter().map(|op| &op.body))
            .chain(program.flows.iter())
            .collect(),
    }
}

#[test]
fn root_examples_are_lsp_clean_and_round_trip_every_projection() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = std::fs::canonicalize(manifest.parent().unwrap().parent().unwrap())
        .expect("canonical repository root");
    let mut examples = std::fs::read_dir(repo.join("examples"))
        .expect("examples directory")
        .map(|entry| entry.expect("example entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("flux"))
        .collect::<Vec<_>>();
    examples.sort();
    assert_eq!(examples.len(), 17, "root .flux example census changed");

    for path in examples {
        let source = std::fs::read_to_string(&path).expect("read example");
        let label = path.strip_prefix(&repo).unwrap_or(&path).display();
        let parsed = flux_lang::parser::parse_cst(&source);
        let diagnostics =
            flux_lsp::diagnostics::parse_errors(&parsed, &source, &LineIndex::new(&source));
        assert!(
            diagnostics.is_empty(),
            "{label}: LSP parse diagnostics: {diagnostics:?}"
        );
        assert_eq!(
            parsed.syntax().text().to_string(),
            source,
            "{label}: CST loss"
        );

        let lowered = flux_lang::lower_cst::cst_to_module(&parsed)
            .unwrap_or_else(|errors| panic!("{label}: strict lowering failed: {errors:?}"));
        for flow in executable_flows(&lowered.module) {
            for (projection, formatted) in [
                ("canonical", flux_lang::format::format(flow)),
                ("compact", flux_lang::format::format_compact(flow)),
            ] {
                let reparsed = flux_lang::parse::parse(&formatted).unwrap_or_else(|error| {
                    panic!("{label}: {projection} output failed to parse: {error}\n{formatted}")
                });
                assert_eq!(reparsed, *flow, "{label}: {projection} AST drift");
            }
            let json = serde_json::to_value(flow).expect("flow serializes as JSON");
            let from_json: DraftAst =
                serde_json::from_value(json).expect("JSON flow projection parses");
            assert_eq!(from_json, *flow, "{label}: JSON projection drift");
        }

        let cst_formatted = flux_lang::format_cst::format_module(&parsed)
            .unwrap_or_else(|| panic!("{label}: CST projection rejected a clean example"));
        let cst_reparsed = flux_lang::parser::parse_cst(&cst_formatted);
        assert!(
            cst_reparsed.errors.is_empty(),
            "{label}: CST-formatted output has errors: {:?}",
            cst_reparsed.errors
        );
        assert_eq!(
            flux_lang::lower_cst::cst_to_module(&cst_reparsed)
                .expect("formatted CST lowers")
                .module,
            lowered.module,
            "{label}: CST projection changed semantics"
        );
    }
}
