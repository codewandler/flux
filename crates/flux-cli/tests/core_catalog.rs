use std::process::Command;

use serde_json::Value;

fn export() -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["catalog", "core", "--format", "json"])
        .env("NO_COLOR", "1")
        .output()
        .expect("run flux catalog core")
}

#[test]
fn core_catalog_is_a_deterministic_versioned_registry_projection() {
    let first = export();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = export();
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout, "catalog output must be stable");
    assert!(first.stdout.ends_with(b"\n"));

    let catalog: Value = serde_json::from_slice(&first.stdout).expect("JSON catalog");
    assert_eq!(catalog["schema_version"], 1);
    assert_eq!(
        catalog["$schema"],
        "https://flux.codewandler.org/v1/schema/core-catalog.schema.json"
    );
    assert_eq!(
        catalog["$id"],
        "https://flux.codewandler.org/v1/core/index.json"
    );

    let operations = catalog["operations"].as_array().expect("operations");
    assert_eq!(operations.len(), 29);
    let names: Vec<&str> = operations
        .iter()
        .map(|op| op["tool_spec"]["name"].as_str().expect("tool name"))
        .collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(names.contains(&"http.request"));
    assert!(names.contains(&"map"));
    assert!(names.contains(&"filter"));
    assert!(!names.contains(&"noop"));

    let nodes = catalog["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 43);
    let return_node = nodes
        .iter()
        .find(|node| node["name"] == "return")
        .expect("return node");
    assert_eq!(
        return_node["schema_ref"],
        "https://flux.codewandler.org/v1/schema/flux-ast.schema.json#node-return"
    );

    let capabilities = catalog["capabilities"].as_array().expect("capabilities");
    assert_eq!(capabilities.len(), 5);
    let http = capabilities
        .iter()
        .find(|cap| cap["name"] == "http")
        .expect("HTTP capability");
    assert_eq!(http["availability"], "available");
    assert_eq!(http["callable"], true);
    for name in ["dns", "tcp", "udp", "icmp"] {
        let capability = capabilities
            .iter()
            .find(|cap| cap["name"] == name)
            .unwrap_or_else(|| panic!("{name} capability"));
        assert_eq!(capability["availability"], "planned");
        assert_eq!(capability["callable"], false);
        assert!(capability["operation_ids"].as_array().unwrap().is_empty());
    }

    for entry in operations
        .iter()
        .chain(nodes.iter())
        .chain(capabilities.iter())
    {
        let id = entry["$id"].as_str().expect("entry id");
        assert!(id.starts_with("https://flux.codewandler.org/v1/core/"));
        assert!(id.ends_with(".json"));
        assert_eq!(
            entry["$schema"],
            "https://flux.codewandler.org/v1/schema/core-entry.schema.json"
        );
    }

    assert!(catalog["schemas"]["catalog"].is_object());
    assert!(catalog["schemas"]["entry"].is_object());
    assert!(catalog["schemas"]["flux_ast"].is_object());
}
