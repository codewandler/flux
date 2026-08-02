use flux_lang::ast::{DraftAst, Node};
use flux_lang::editor::{lower, lower_source, project, project_source, EditorNodeKind};
use flux_lang::parse::parse;

const VISUAL: &str = r#"flow enrich(items: List) -> String
  first = crm.find({"query": "open"})
  when $first != ""
    each item in items -> seen
      crm.update({"id": item})
  else
    repeat 2
      crm.retry({"id": "fallback"})
  parallel
    branch left
      cognition.left({"value": first})
    branch right
      cognition.right({"value": first})
  return first
"#;

#[test]
fn visual_subset_roundtrips_through_the_editor_ir() {
    let ast = parse(VISUAL).expect("fixture parses");
    let projection = project(&ast, None);
    assert!(
        projection.diagnostics.is_empty(),
        "{:?}",
        projection.diagnostics
    );
    let graph = projection.graph.expect("visual graph");

    assert!(matches!(graph.body[0].kind, EditorNodeKind::Call { .. }));
    assert!(matches!(graph.body[1].kind, EditorNodeKind::When { .. }));
    assert!(matches!(
        graph.body[2].kind,
        EditorNodeKind::Parallel { .. }
    ));
    assert!(matches!(graph.body[3].kind, EditorNodeKind::Return { .. }));
    assert_eq!(lower(&graph).expect("lowers"), ast);
    let canonical = lower_source(&graph).expect("renders source");
    assert_eq!(parse(&canonical).expect("rendered source parses"), ast);

    let again = project(&ast, Some(&graph)).graph.expect("projects again");
    let before: Vec<_> = graph.body.iter().map(|node| node.id.clone()).collect();
    let after: Vec<_> = again.body.iter().map(|node| node.id.clone()).collect();
    assert_eq!(after, before, "unchanged source keeps editor identities");
}

#[test]
fn unsupported_valid_flux_is_source_only_and_keeps_a_real_range() {
    let source = "flow guarded\n  retry 2\n    safe({\"value\": 1})\n";
    let projection = project_source(source, None).expect("valid Flux stays valid");
    assert!(projection.graph.is_none());
    assert_eq!(projection.diagnostics.len(), 1);
    assert_eq!(projection.diagnostics[0].code, "editor.unsupported_node");
    assert!(projection.diagnostics[0].range.is_some());
    assert!(projection.diagnostics[0].node_id.is_some());
    let again = project_source(source, None).expect("projection is deterministic");
    assert_eq!(
        again.diagnostics[0].node_id,
        projection.diagnostics[0].node_id
    );
}

#[test]
fn comments_make_source_mode_explicit_instead_of_being_discarded() {
    let source = "flow kept\n  # this explanation must survive\n  return 1\n";
    let projection = project_source(source, None).expect("valid Flux stays valid");
    assert!(projection.graph.is_none());
    assert_eq!(projection.diagnostics[0].code, "editor.source_trivia");
}

#[test]
fn duplicate_calls_receive_distinct_node_ids() {
    let ast = DraftAst {
        body: vec![
            Node::Call {
                op: "same".into(),
                args: vec![],
            },
            Node::Call {
                op: "same".into(),
                args: vec![],
            },
        ],
        ..DraftAst::default()
    };
    let graph = project(&ast, None).graph.expect("visual graph");
    assert_ne!(graph.body[0].id, graph.body[1].id);
}

#[test]
fn an_in_place_source_edit_keeps_the_existing_node_identity() {
    let original = DraftAst {
        body: vec![Node::Call {
            op: "before".into(),
            args: vec![],
        }],
        ..DraftAst::default()
    };
    let previous = project(&original, None).graph.expect("original graph");
    let edited = DraftAst {
        body: vec![Node::Call {
            op: "after".into(),
            args: vec![],
        }],
        ..DraftAst::default()
    };

    let projected = project(&edited, Some(&previous))
        .graph
        .expect("edited graph");
    assert_eq!(
        projected.body[0].id, previous.body[0].id,
        "editing one statement in place must not create a different visual node"
    );
}

#[test]
fn a_shifted_unchanged_node_keeps_its_semantic_identity() {
    let original = DraftAst {
        body: vec![
            Node::Call {
                op: "removed".into(),
                args: vec![],
            },
            Node::Call {
                op: "kept".into(),
                args: vec![],
            },
        ],
        ..DraftAst::default()
    };
    let previous = project(&original, None).graph.expect("original graph");
    let edited = DraftAst {
        body: vec![Node::Call {
            op: "kept".into(),
            args: vec![],
        }],
        ..DraftAst::default()
    };

    let projected = project(&edited, Some(&previous))
        .graph
        .expect("edited graph");
    assert_eq!(
        projected.body[0].id, previous.body[1].id,
        "deleting an earlier statement must not transfer its identity to the shifted successor"
    );
}

#[test]
fn node_map_tracks_graph_structure_after_nodes_are_reordered() {
    let ast = DraftAst {
        body: vec![
            Node::Call {
                op: "first".into(),
                args: vec![],
            },
            Node::Call {
                op: "second".into(),
                args: vec![],
            },
        ],
        ..DraftAst::default()
    };
    let mut graph = project(&ast, None).graph.expect("visual graph");
    let first_id = graph.body[0].id.clone();
    let second_id = graph.body[1].id.clone();

    graph.body.swap(0, 1);

    let map = graph.node_map();
    assert_eq!(map["body[0]"], second_id);
    assert_eq!(map["body[1]"], first_id);
}
