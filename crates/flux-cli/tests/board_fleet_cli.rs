use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flux-board-fleet-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("docs/stories")).unwrap();
    root
}

fn flux(root: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_flux"))
        .current_dir(root)
        // These fixtures exercise planning/control semantics, not process confinement. Pin the
        // posture so unattended child Flux invocations never inherit a developer/CI ambient mode.
        .env("FLUX_SANDBOX", "off")
        .args(args)
        .output()
        .unwrap()
}

fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for character in line.chars() {
        match (quote, character) {
            (Some(expected), found) if expected == found => quote = None,
            (Some(_), found) => word.push(found),
            (None, '"' | '\'') => quote = Some(character),
            (None, found) if found.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, found) => word.push(found),
        }
    }
    assert!(
        quote.is_none(),
        "unterminated quote in skill example: {line}"
    );
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn skill_examples(markdown: &str) -> Vec<Vec<String>> {
    let mut fenced = false;
    let mut examples = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced && line.trim_start().starts_with("flux ") {
            let mut words = shell_words(line.trim());
            assert_eq!(words.remove(0), "flux");
            examples.push(words);
        }
    }
    examples
}

fn git(root: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn one_story_wave(name: &str) -> (PathBuf, PathBuf) {
    let root = fixture(name);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    assert!(flux(&root, &["fleet", "start"]).status.success());
    let dispatched = flux(
        &root,
        &[
            "fleet",
            "run",
            "repo/C-1",
            "--prepare-only",
            "--output",
            "json",
        ],
    );
    assert!(dispatched.status.success());
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let story = PathBuf::from(
        dispatched["data"]["topology"]["repositories"][0]["stories"][0]["worktree"]
            .as_str()
            .unwrap(),
    );
    (root, story)
}

fn commit_result(story: &PathBuf, value: &str) -> String {
    fs::write(story.join("result.txt"), format!("{value}\n")).unwrap();
    assert!(git(story, &["add", "result.txt"]).status.success());
    assert!(git(story, &["commit", "-qm", value]).status.success());
    String::from_utf8(git(story, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn submit_result_handoff(root: &PathBuf, commit: &str) -> serde_json::Value {
    let handoff = flux(
        root,
        &[
            "fleet",
            "handoff",
            "wave-2",
            "repo/C-1",
            "--commit",
            commit,
            "--write-set",
            "result.txt",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "result.txt",
            "--failing-before",
            "--passing-after",
            "--summary",
            "Implemented the reviewed contract",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "{}",
        String::from_utf8_lossy(&handoff.stdout)
    );
    serde_json::from_slice(&handoff.stdout).unwrap()
}

#[test]
fn board_and_fleet_skills_are_valid_small_agent_skills() {
    let root = fixture("skills");
    for family in ["board", "fleet"] {
        let output = flux(&root, &[family, "skill"]);
        assert!(
            output.status.success(),
            "{family}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let body = String::from_utf8(output.stdout).unwrap();
        assert!(body.starts_with("---\nname: flux-"), "{body}");
        assert!(body.contains("description:"), "{body}");
        assert!(body.contains(&format!("flux {family} schema")), "{body}");
        assert!(
            body.len() < 4_096,
            "skill must stay prompt-sized: {} bytes",
            body.len()
        );
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn every_board_and_fleet_skill_example_executes_against_an_offline_fixture() {
    let board_root = fixture("board-skill-examples");
    fs::write(
        board_root.join("docs/stories/C-1-ready.md"),
        "---\nid: C-1\ntitle: Ready\nstatus: ready\npriority: 1\n---\n\n# Ready\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    let board_skill = flux(&board_root, &["board", "skill"]);
    assert!(board_skill.status.success());
    let board_skill = String::from_utf8(board_skill.stdout).unwrap();
    let shown = flux(&board_root, &["board", "show", "--output", "json"]);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let board_revision = shown["revision"].as_str().unwrap().to_string();
    for (index, mut example) in skill_examples(&board_skill).into_iter().enumerate() {
        for argument in &mut example {
            if argument == "REV" {
                *argument = board_revision.clone();
            } else if argument == "KEY" {
                *argument = format!("board-skill-{index}");
            }
        }
        let args = example.iter().map(String::as_str).collect::<Vec<_>>();
        let output = flux(&board_root, &args);
        assert!(
            output.status.success(),
            "{example:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(board_root).ok();

    let fleet_root = fixture("fleet-skill-examples");
    fs::write(fleet_root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        fleet_root.join("docs/stories/C-1-ready.md"),
        "---\nid: C-1\ntitle: Ready\nstatus: ready\npriority: 1\n---\n\n# Ready\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(fleet_root.join(".flux/fleet/agents")).unwrap();
    fs::write(
        fleet_root.join(".flux/fleet/main.md"),
        "Coordinate the offline fixture.\n",
    )
    .unwrap();
    fs::write(
        fleet_root.join(".flux/fleet/agents/story-worker.md"),
        "Implement the assigned fixture story.\n",
    )
    .unwrap();
    fs::write(
        fleet_root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[main]\ninstructions = \".flux/fleet/main.md\"\nmodel = \"mock\"\n\n[[agent_templates]]\nid = \"story-worker\"\nrole = \"writer\"\ninstructions = \".flux/fleet/agents/story-worker.md\"\nmodel = \"mock\"\nmode = \"write\"\nmax_instances = 1\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n",
    )
    .unwrap();
    assert!(git(&fleet_root, &["init", "-q"]).status.success());
    assert!(
        git(&fleet_root, &["config", "user.email", "fleet@example.test"])
            .status
            .success()
    );
    assert!(
        git(&fleet_root, &["config", "user.name", "Flux Fleet Test"])
            .status
            .success()
    );
    assert!(git(&fleet_root, &["add", "."]).status.success());
    assert!(git(&fleet_root, &["commit", "-qm", "fixture"])
        .status
        .success());
    assert!(flux(&fleet_root, &["fleet", "start"]).status.success());
    let fleet_skill = flux(&fleet_root, &["fleet", "skill"]);
    assert!(fleet_skill.status.success());
    let fleet_skill = String::from_utf8(fleet_skill.stdout).unwrap();
    let mut wave = String::new();
    let mut worker = String::new();
    let mut story_worktree = PathBuf::new();
    let mut fleet_revision = String::new();
    for (index, mut example) in skill_examples(&fleet_skill).into_iter().enumerate() {
        if example.get(1).map(String::as_str) == Some("apply") {
            fs::remove_file(story_worktree.join("flux-mock.txt")).ok();
            fs::remove_file(fleet_root.join("flux-mock.txt")).ok();
            fs::write(story_worktree.join("result.txt"), "skill example\n").unwrap();
            assert!(git(&story_worktree, &["add", "result.txt"])
                .status
                .success());
            assert!(git(
                &story_worktree,
                &["commit", "-qm", "complete skill fixture"]
            )
            .status
            .success());
            let commit = String::from_utf8(git(&story_worktree, &["rev-parse", "HEAD"]).stdout)
                .unwrap()
                .trim()
                .to_string();
            let handoff = flux(
                &fleet_root,
                &[
                    "fleet",
                    "handoff",
                    &wave,
                    "repo/C-1",
                    "--commit",
                    &commit,
                    "--write-set",
                    "result.txt",
                    "--test-arg",
                    "test",
                    "--test-arg",
                    "-f",
                    "--test-arg",
                    "result.txt",
                    "--failing-before",
                    "--passing-after",
                    "--summary",
                    "skill fixture complete",
                    "--output",
                    "json",
                ],
            );
            assert!(
                handoff.status.success(),
                "{}",
                String::from_utf8_lossy(&handoff.stdout)
            );
            let integrated = flux(
                &fleet_root,
                &["fleet", "integrate", &wave, "--output", "json"],
            );
            assert!(
                integrated.status.success(),
                "{}",
                String::from_utf8_lossy(&integrated.stdout)
            );
            let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
            fleet_revision = integrated["revision"].as_str().unwrap().to_string();
        }
        for argument in &mut example {
            match argument.as_str() {
                "KEY" => *argument = format!("fleet-skill-{index}"),
                "WORKER" => *argument = worker.clone(),
                "WAVE" => *argument = wave.clone(),
                "REV" => *argument = fleet_revision.clone(),
                _ => {}
            }
        }
        let args = example.iter().map(String::as_str).collect::<Vec<_>>();
        let output = flux(&fleet_root, &args);
        assert!(
            output.status.success(),
            "{example:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if example.get(1).map(String::as_str) == Some("run") {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            wave = value["data"]["wave"].as_str().unwrap().to_string();
            let story = &value["data"]["topology"]["repositories"][0]["stories"][0];
            story_worktree = PathBuf::from(story["worktree"].as_str().unwrap());
            worker = value["data"]["receipts"][0]["agent"]
                .as_str()
                .unwrap_or_else(|| story["worker"].as_str().unwrap_or(""))
                .to_string();
            if worker.is_empty() {
                worker = format!("{wave}-worker-1");
            }
        }
    }
    assert!(fleet_root.join("result.txt").is_file());
    fs::remove_dir_all(fleet_root).ok();
}

#[test]
fn machine_schema_uses_the_versioned_envelope_and_clean_stdout() {
    let root = fixture("schema");
    let output = flux(&root, &["board", "schema", "--output", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "machine diagnostics leaked: {:?}",
        output.stderr
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "flux.cli/v1");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["family"], "board");
    assert!(value["data"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op["name"] == "stats"));
    fs::remove_dir_all(root).ok();
}

#[test]
fn public_schema_catalogue_matches_every_installed_board_and_fleet_command() {
    let root = fixture("schema-catalogue");
    for family in ["board", "fleet"] {
        let schema = flux(&root, &[family, "schema", "--output", "json"]);
        assert!(schema.status.success(), "{family}");
        let schema: serde_json::Value = serde_json::from_slice(&schema.stdout).unwrap();
        let declared = schema["data"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|operation| operation["name"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>();

        let help = flux(&root, &[family, "--help"]);
        assert!(help.status.success(), "{family}");
        let help = String::from_utf8(help.stdout).unwrap();
        let mut in_commands = false;
        let mut installed = std::collections::BTreeSet::new();
        for line in help.lines() {
            match line.trim() {
                "Commands:" => {
                    in_commands = true;
                    continue;
                }
                "Options:" => break,
                _ => {}
            }
            if in_commands {
                if let Some(command) = line.split_whitespace().next() {
                    if command != "help" {
                        installed.insert(command.to_string());
                    }
                }
            }
        }
        assert_eq!(declared, installed, "{family} schema drifted from clap");
        for operation in declared {
            let output = flux(&root, &[family, &operation, "--help"]);
            assert!(output.status.success(), "{family} {operation}");
        }
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn machine_failures_pin_exit_classes_and_never_leak_diagnostics() {
    let root = fixture("failure-envelopes");
    fs::write(
        root.join("docs/stories/C-1-ready-without-priority.md"),
        "---\nid: C-1\ntitle: Needs priority\nstatus: ready\n---\n\n# Needs priority\n",
    )
    .unwrap();

    let cases: &[(&[&str], i32, &str)] = &[
        (
            &["board", "show", "--if-revision", "old", "--output", "json"],
            2,
            "input/schema",
        ),
        (
            &["board", "get", "C-404", "--output", "json"],
            3,
            "not-found",
        ),
        (
            &["board", "transition", "C-1", "done", "--output", "json"],
            4,
            "conflict/precondition",
        ),
        (
            &["board", "check", "--output", "json"],
            7,
            "validation/gate",
        ),
    ];
    for (args, exit, class) in cases {
        let output = flux(&root, args);
        assert_eq!(output.status.code(), Some(*exit), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}: machine stderr leaked");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema"], "flux.cli/v1", "{args:?}");
        assert_eq!(value["ok"], false, "{args:?}");
        assert_eq!(value["error"]["class"], *class, "{args:?}");
        assert_eq!(value["error"]["code"], *exit, "{args:?}");
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn sensitive_board_content_is_redacted_at_every_output_renderer() {
    let root = fixture("output-redaction");
    fs::write(
        root.join("docs/stories/C-1-secret.md"),
        "---\nid: C-1\ntitle: Secret\nstatus: backlog\nnote: token=supersecret\n---\n\n# Secret\n",
    )
    .unwrap();

    for mode in ["human", "json", "ndjson"] {
        let output = flux(&root, &["board", "get", "C-1", "--output", mode]);
        assert!(output.status.success(), "{mode}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("supersecret"), "{mode}: {stdout}");
        assert!(stdout.contains("[redacted]"), "{mode}: {stdout}");
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn board_check_resolves_repo_story_relative_and_slug_design_links() {
    let root = fixture("design-links");
    fs::create_dir_all(root.join("docs/designs")).unwrap();
    fs::create_dir_all(root.join("docs/archive/designs")).unwrap();
    fs::write(root.join("docs/designs/direct.md"), "# Direct\n").unwrap();
    fs::write(root.join("docs/designs/relative.md"), "# Relative\n").unwrap();
    fs::write(root.join("docs/designs/shorthand.md"), "# Shorthand\n").unwrap();
    fs::write(
        root.join("docs/archive/designs/archived.md"),
        "# Archived\n",
    )
    .unwrap();
    for (id, design) in [
        ("C-1", "docs/designs/direct.md"),
        ("C-2", "../designs/relative.md"),
        ("C-3", "shorthand"),
        ("C-4", "docs/archive/designs/archived.md"),
    ] {
        fs::write(
            root.join(format!("docs/stories/{id}-design-link.md")),
            format!(
                "---\nid: {id}\ntitle: Design link {id}\nstatus: backlog\ndesign: {design}\n---\n\n# Design link {id}\n"
            ),
        )
        .unwrap();
    }

    let output = flux(&root, &["board", "check", "--output", "json"]);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["valid"], true);
    assert_eq!(value["data"]["stories"], 4);
    fs::remove_dir_all(root).ok();
}

#[test]
fn generic_json_call_can_reach_real_board_and_fleet_mutations() {
    let root = fixture("json-call");
    fs::write(
        root.join("docs/stories/README.md"),
        "# Board\n\n<!-- BEGIN track:board -->\n<!-- END track:board -->\n",
    )
    .unwrap();
    let board_request = root.join("board-request.json");
    fs::write(
        &board_request,
        r#"{"schema":"flux.cli/v1","request_id":"board-create","args":["--kind","story","--id","C-1","--title","Created through call"]}"#,
    )
    .unwrap();
    let created = flux(
        &root,
        &[
            "board",
            "call",
            "create",
            "--request",
            board_request.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stdout)
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(created["request_id"], "board-create");
    assert_eq!(created["data"]["id"], "C-1");
    assert!(root
        .join("docs/stories/C-1-created-through-call.md")
        .is_file());

    let fleet_request = root.join("fleet-request.json");
    fs::write(
        &fleet_request,
        r#"{"schema":"flux.cli/v1","request_id":"goal-set","args":["set","project","flux","Replace helper scripts"]}"#,
    )
    .unwrap();
    let goal = flux(
        &root,
        &[
            "fleet",
            "call",
            "goal",
            "--request",
            fleet_request.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        goal.status.success(),
        "{}",
        String::from_utf8_lossy(&goal.stdout)
    );
    let goal: serde_json::Value = serde_json::from_slice(&goal.stdout).unwrap();
    assert_eq!(goal["request_id"], "goal-set");
    assert_eq!(goal["data"]["goal"]["scope"], "project");
    assert_eq!(goal["data"]["goal"]["statement"], "Replace helper scripts");
    fs::remove_dir_all(root).ok();
}

#[test]
fn board_export_import_round_trips_authored_resources_without_clobbering() {
    let source = fixture("export-source");
    fs::write(
        source.join("docs/stories/C-7-round-trip.md"),
        "---\nid: C-7\ntitle: Round trip\nstatus: ready\npriority: 7\ndesign: docs/designs/round-trip.md\n---\n\n# Round trip\n\n## Goal\n\nPreserve this authored body.\n\n## Acceptance\n\n- [ ] imported\n",
    )
    .unwrap();
    fs::create_dir_all(source.join("docs/designs")).unwrap();
    fs::create_dir_all(source.join("docs/decisions")).unwrap();
    fs::write(source.join("docs/VISION.md"), "# Vision\n\nKeep it.\n").unwrap();
    fs::write(source.join("docs/ROADMAP.md"), "# Roadmap\n\nShip it.\n").unwrap();
    fs::write(
        source.join("docs/designs/round-trip.md"),
        "# Round-trip design\n",
    )
    .unwrap();
    fs::write(
        source.join("docs/decisions/D-1.md"),
        "---\nid: D-1\nstatus: decided\n---\n\n# Decision\n",
    )
    .unwrap();
    let export = flux(&source, &["board", "export", "-o", "export.json"]);
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );

    let target = fixture("import-target");
    let initialized = flux(&target, &["board", "init", "--scaffold"]);
    assert!(initialized.status.success());
    fs::copy(source.join("export.json"), target.join("export.json")).unwrap();
    let imported = flux(
        &target,
        &["board", "import", "export.json", "--output", "json"],
    );
    assert!(
        imported.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&imported.stdout),
        String::from_utf8_lossy(&imported.stderr)
    );
    let imported: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(imported["data"]["items"], 1);
    assert_eq!(imported["data"]["resources"], 5);
    for relative in [
        "docs/stories/C-7-round-trip.md",
        "docs/VISION.md",
        "docs/ROADMAP.md",
        "docs/designs/round-trip.md",
        "docs/decisions/D-1.md",
    ] {
        assert_eq!(
            fs::read(source.join(relative)).unwrap(),
            fs::read(target.join(relative)).unwrap(),
            "{relative} did not round trip exactly"
        );
    }
    let board = fs::read_to_string(target.join("docs/stories/README.md")).unwrap();
    assert!(board.contains("C-7"), "{board}");
    let replay = flux(&target, &["board", "import", "export.json"]);
    assert!(
        !replay.status.success(),
        "create-only import must not clobber"
    );
    fs::remove_dir_all(source).ok();
    fs::remove_dir_all(target).ok();
}

#[test]
fn workspace_board_federates_namespaced_items_and_routes_member_writes() {
    let workspace = fixture("workspace-board");
    let api = workspace.join("members/api");
    let web = workspace.join("members/web");
    fs::create_dir_all(api.join("docs/stories")).unwrap();
    fs::create_dir_all(web.join("docs/stories")).unwrap();
    fs::write(
        api.join("docs/stories/C-1-api.md"),
        "---\nid: C-1\ntitle: API contract\nstatus: done\n---\n\n# API contract\n",
    )
    .unwrap();
    fs::write(
        web.join("docs/stories/C-1-web.md"),
        "---\nid: C-1\ntitle: Web client\nstatus: ready\npriority: 1\ndepends_on: [api/C-1]\n---\n\n# Web client\n",
    )
    .unwrap();
    fs::create_dir_all(workspace.join(".flux")).unwrap();
    fs::write(
        workspace.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\n\n[[repositories]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"product-api\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n\n[[repositories]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"product-web\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n",
    )
    .unwrap();
    let items = flux(
        &workspace,
        &["board", "--scope", "workspace", "items", "--output", "json"],
    );
    assert!(
        items.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&items.stdout),
        String::from_utf8_lossy(&items.stderr)
    );
    let items: serde_json::Value = serde_json::from_slice(&items.stdout).unwrap();
    assert_eq!(items["data"]["items"][0]["id"], "api/C-1");
    assert_eq!(items["data"]["items"][1]["id"], "web/C-1");
    let next = flux(
        &workspace,
        &["board", "--scope", "workspace", "next", "--output", "json"],
    );
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["data"]["items"][0]["id"], "web/C-1");

    let ambiguous = flux(
        &workspace,
        &["board", "--scope", "workspace", "start", "C-1"],
    );
    assert!(
        !ambiguous.status.success(),
        "workspace writes require a member"
    );
    let started = flux(
        &workspace,
        &[
            "board",
            "--scope",
            "workspace",
            "--board",
            "web",
            "start",
            "C-1",
            "--output",
            "json",
        ],
    );
    assert!(
        started.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(fs::read_to_string(web.join("docs/stories/C-1-web.md"))
        .unwrap()
        .contains("status: in-progress"));
    assert!(!workspace.join("docs/stories/C-1-web.md").exists());
    fs::remove_dir_all(workspace).ok();
}

#[test]
fn workspace_board_refuses_missing_cycles_absent_members_and_ambiguous_selectors() {
    let workspace = fixture("workspace-refusals");
    let api = workspace.join("members/api");
    let web = workspace.join("members/web");
    fs::create_dir_all(api.join("docs/stories")).unwrap();
    fs::create_dir_all(web.join("docs/stories")).unwrap();
    fs::create_dir_all(workspace.join("members/absent")).unwrap();
    fs::create_dir_all(workspace.join(".flux")).unwrap();
    fs::write(
        workspace.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\n\n[[repositories]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"shared\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n\n[[repositories]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"shared\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n",
    )
    .unwrap();
    fs::write(
        api.join("docs/stories/C-1-api.md"),
        "---\nid: C-1\ntitle: API\nstatus: ready\npriority: 1\ndepends_on: [web/C-404]\n---\n\n# API\n",
    )
    .unwrap();
    fs::write(
        web.join("docs/stories/C-1-web.md"),
        "---\nid: C-1\ntitle: Web\nstatus: ready\npriority: 2\n---\n\n# Web\n",
    )
    .unwrap();

    let missing = flux(
        &workspace,
        &["board", "--scope", "workspace", "items", "--output", "json"],
    );
    assert_eq!(missing.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("web/C-404"));

    fs::write(
        api.join("docs/stories/C-1-api.md"),
        "---\nid: C-1\ntitle: API\nstatus: ready\npriority: 1\ndepends_on: [web/C-1]\n---\n\n# API\n",
    )
    .unwrap();
    fs::write(
        web.join("docs/stories/C-1-web.md"),
        "---\nid: C-1\ntitle: Web\nstatus: ready\npriority: 2\ndepends_on: [api/C-1]\n---\n\n# Web\n",
    )
    .unwrap();
    let cycle = flux(&workspace, &["fleet", "schedule", "--output", "json"]);
    assert_eq!(cycle.status.code(), Some(7));
    let cycle_text = String::from_utf8_lossy(&cycle.stdout);
    assert!(cycle_text.contains("api/C-1"), "{cycle_text}");
    assert!(cycle_text.contains("web/C-1"), "{cycle_text}");
    assert!(
        cycle_text.contains("-&gt;") || cycle_text.contains("->"),
        "{cycle_text}"
    );

    let ambiguous = flux(
        &workspace,
        &[
            "board",
            "--scope",
            "workspace",
            "--board",
            "shared",
            "start",
            "C-1",
            "--output",
            "json",
        ],
    );
    assert_eq!(ambiguous.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&ambiguous.stdout).contains("api, web"));

    fs::write(
        workspace.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\n\n[[repositories]]\nid = \"absent\"\nroot = \"members/absent\"\nboard = \"missing\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n",
    )
    .unwrap();
    let absent = flux(&workspace, &["fleet", "schedule", "--output", "json"]);
    assert_eq!(absent.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&absent.stdout).contains("docs/stories"));
    fs::remove_dir_all(workspace).ok();
}

#[test]
fn track_render_and_stats_preserve_the_expected_board_metrics() {
    let root = fixture("track");
    fs::write(
        root.join("docs/stories/README.md"),
        "# Board\n\nHand written.\n\n<!-- BEGIN track:board -->\nstale\n<!-- END track:board -->\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/stories/C-1-done.md"),
        "---\nid: C-1\ntitle: Done story\npillar: Core\nstatus: done\n---\n\n# Done\n\n## Acceptance\n\n- [x] first\n- [x] second\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/stories/C-2-ready.md"),
        "---\nid: C-2\ntitle: Ready story\npillar: Core\nstatus: ready\npriority: 1\n---\n\n# Ready\n\n## Acceptance\n\n- [x] first\n- [ ] second\n\n## Tasks\n\n- [ ] optional\n",
    )
    .unwrap();

    let rendered = flux(&root, &["board", "render"]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let board = fs::read_to_string(root.join("docs/stories/README.md")).unwrap();
    assert!(board.contains("Hand written."));
    assert!(board.contains("C-2 — Ready story"));
    assert!(board.contains("C-1 — Done story"));

    let stats = flux(&root, &["board", "stats", "--output", "json"]);
    assert!(
        stats.status.success(),
        "{}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(value["data"]["stories"]["done"], 1);
    assert_eq!(value["data"]["stories"]["total"], 2);
    assert_eq!(value["data"]["criteria"]["done"], 3);
    assert_eq!(value["data"]["criteria"]["total"], 4);
    assert_eq!(value["data"]["tasks"]["schema"], "present");
    assert_eq!(value["data"]["tasks"]["total"], 1);
    assert_eq!(value["data"]["implementation"]["percent"], 50.0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn track_render_is_byte_compatible_with_the_plugin_golden_algorithm() {
    let root = fixture("track-golden");
    fs::create_dir_all(root.join("docs/designs")).unwrap();
    fs::write(
        root.join("docs/stories/README.md"),
        "# Board\n\nPrecious intro.\n\n<!-- BEGIN track:board -->\nstale\n<!-- END track:board -->\n\nPrecious tail.\n",
    )
    .unwrap();
    let stories = [
        (
            "A-10",
            "Later",
            "ready",
            "priority: P2\nepic: focus\n",
            "Core",
            "",
        ),
        (
            "A-2",
            "First",
            "ready",
            "priority: rank 1\n",
            "Agent",
            "top",
        ),
        ("B-1", "Building", "in_progress", "", "Runtime", ""),
        ("C-1", "Waiting", "blocked", "", "Core", "human"),
        ("D-1", "Eventually", "backlog", "epic: focus\n", "Docs", ""),
        ("E-1", "Shipped", "done", "", "Core", "released"),
    ];
    for (id, title, status, extra, pillar, note) in stories {
        let note = if note.is_empty() {
            String::new()
        } else {
            format!("note: {note}\n")
        };
        fs::write(
            root.join(format!("docs/stories/{id}-{title}.md")),
            format!(
                "---\nid: {id}\ntitle: {title}\nstatus: {status}\npillar: {pillar}\n{extra}{note}---\n\n# {title}\n"
            ),
        )
        .unwrap();
    }
    fs::write(
        root.join("docs/designs/focus.md"),
        "# Design — Focus Area\n\n## Why\n\nKeep related work understandable.\n",
    )
    .unwrap();

    let output = flux(&root, &["board", "render", "--output", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let actual = fs::read_to_string(root.join("docs/stories/README.md")).unwrap();
    let expected = "# Board\n\nPrecious intro.\n\n<!-- BEGIN track:board -->\n<!-- Generated by /track:board (track plugin) from story frontmatter. Do not hand-edit this region; edit the stories and re-run /track:board. -->\n\n## Now (in progress)\n- [B-1 — Building](B-1-Building.md) · Runtime\n\n## Next (ready — take the top one unless the user named a story)\n- [A-2 — First](A-2-First.md) · Agent · top\n\n### Focus Area\n_Keep related work understandable._\n- [A-10 — Later](A-10-Later.md) · Core\n\n## Blocked\n- [C-1 — Waiting](C-1-Waiting.md) · Core · human\n\n## Backlog\n\n### Focus Area\n_Keep related work understandable._\n- [D-1 — Eventually](D-1-Eventually.md) · Docs\n\n## Done\n- [E-1 — Shipped](E-1-Shipped.md) · Core · released\n\n_See [CHANGELOG.md](../../CHANGELOG.md) for the full released history._\n<!-- END track:board -->\n\nPrecious tail.\n";
    assert_eq!(actual, expected);
    let second = flux(&root, &["board", "render", "--output", "json"]);
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["data"]["changed"], false);
    fs::remove_dir_all(root).ok();
}

#[test]
fn mutations_are_idempotent_and_stale_revisions_are_typed_conflicts() {
    let root = fixture("revisions");
    fs::write(
        root.join("docs/stories/README.md"),
        "# Board\n\n<!-- BEGIN track:board -->\n<!-- END track:board -->\n",
    )
    .unwrap();

    let before = flux(&root, &["board", "show", "--output", "json"]);
    assert!(before.status.success());
    let before: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    let revision = before["revision"].as_str().unwrap();

    let args = [
        "board",
        "create",
        "--kind",
        "story",
        "--id",
        "C-1",
        "--title",
        "Once",
        "--if-revision",
        revision,
        "--idempotency-key",
        "create-C-1",
        "--output",
        "json",
    ];
    let first = flux(&root, &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = flux(&root, &args);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "retry returns the original result"
    );
    assert_eq!(
        fs::read_dir(root.join("docs/stories"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("C-1-"))
            .count(),
        1
    );

    let stale = flux(
        &root,
        &[
            "board",
            "create",
            "--kind",
            "story",
            "--id",
            "C-2",
            "--title",
            "Stale",
            "--if-revision",
            revision,
            "--output",
            "json",
        ],
    );
    assert_eq!(stale.status.code(), Some(4));
    assert!(stale.stderr.is_empty());
    let stale: serde_json::Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["error"]["class"], "conflict/precondition");
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_has_one_main_coordinator_for_goals_and_all_intake() {
    let root = fixture("main-coordinator");
    let initialized = flux(&root, &["fleet", "init", "--output", "json"]);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let initialized: serde_json::Value = serde_json::from_slice(&initialized.stdout).unwrap();
    assert_eq!(initialized["data"]["main_agent"]["id"], "main");
    assert_eq!(initialized["data"]["main_agent"]["role"], "coordinator");

    let goal = flux(
        &root,
        &[
            "fleet",
            "goal",
            "set",
            "project",
            "flux",
            "Make agent automation inspectable",
            "--output",
            "json",
        ],
    );
    assert!(
        goal.status.success(),
        "{}",
        String::from_utf8_lossy(&goal.stderr)
    );
    let goal: serde_json::Value = serde_json::from_slice(&goal.stdout).unwrap();
    assert_eq!(goal["data"]["main_agent"], "main");
    assert_eq!(goal["data"]["goal"]["scope"], "project");

    let intake = flux(
        &root,
        &[
            "fleet",
            "ingest",
            "Please schedule the next story",
            "--source",
            "user",
            "--output",
            "json",
        ],
    );
    assert!(
        intake.status.success(),
        "{}",
        String::from_utf8_lossy(&intake.stderr)
    );
    let intake: serde_json::Value = serde_json::from_slice(&intake.stdout).unwrap();
    assert_eq!(intake["data"]["target"], "main");
    assert_eq!(intake["data"]["goals_revision"], 1);

    let agents = flux(&root, &["fleet", "agents", "--output", "json"]);
    assert!(agents.status.success());
    let agents: serde_json::Value = serde_json::from_slice(&agents.stdout).unwrap();
    assert_eq!(agents["data"]["main_agent"]["id"], "main");
    assert_eq!(agents["data"]["workers"].as_object().unwrap().len(), 0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn open_decisions_block_only_linked_work_and_fleet_prompts_for_a_human() {
    let root = fixture("decisions");
    fs::write(
        root.join("docs/stories/README.md"),
        "# Board\n\n<!-- BEGIN track:board -->\n<!-- END track:board -->\n",
    )
    .unwrap();
    for (id, priority) in [("C-1", 1), ("C-2", 2)] {
        fs::write(
            root.join(format!("docs/stories/{id}-story.md")),
            format!(
                "---\nid: {id}\ntitle: Story {id}\nstatus: ready\npriority: {priority}\n---\n\n# {id}\n\n## Acceptance\n\n- [ ] ship\n"
            ),
        )
        .unwrap();
    }

    let opened = flux(
        &root,
        &[
            "board",
            "decision",
            "open",
            "D-1",
            "--title",
            "Choose storage",
            "--question",
            "Which durable store should we use?",
            "--blocks",
            "C-1",
            "--option",
            "sqlite",
            "--option",
            "postgres",
            "--tradeoff",
            "sqlite=zero-ops local storage",
            "--tradeoff",
            "postgres=shared service with operational cost",
            "--recommended",
            "sqlite",
            "--output",
            "json",
        ],
    );
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let next = flux(&root, &["board", "next", "--output", "json"]);
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["data"]["items"][0]["id"], "C-2");

    let fleet = flux(&root, &["fleet", "init", "--output", "json"]);
    assert!(fleet.status.success());
    let prompts = flux(&root, &["fleet", "decisions", "--output", "json"]);
    assert!(prompts.status.success());
    let prompts: serde_json::Value = serde_json::from_slice(&prompts.stdout).unwrap();
    assert_eq!(prompts["data"]["attention_required"], true);
    assert_eq!(prompts["data"]["decisions"][0]["ref"], "workspace/D-1");
    assert_eq!(prompts["data"]["decisions"][0]["recommended"], "sqlite");
    assert_eq!(
        prompts["data"]["decisions"][0]["suggestions"][0]["tradeoff"],
        "zero-ops local storage"
    );
    assert_eq!(
        prompts["data"]["decisions"][0]["suggestions"][0]["recommended"],
        true
    );

    let auto = flux(
        &root,
        &[
            "fleet",
            "decisions",
            "--auto",
            "--idempotency-key",
            "auto-D-1",
            "--output",
            "json",
        ],
    );
    assert!(
        auto.status.success(),
        "{}",
        String::from_utf8_lossy(&auto.stderr)
    );
    let agents = flux(&root, &["fleet", "agents", "--output", "json"]);
    let agents: serde_json::Value = serde_json::from_slice(&agents.stdout).unwrap();
    assert_eq!(
        agents["data"]["workers"]["decision-workspace-D-1"]["role"],
        "adversarial-decision-agent"
    );
    assert_eq!(
        agents["data"]["workers"]["decision-workspace-D-1"]["recommendation_must_be_challenged"],
        true
    );

    let decided = flux(
        &root,
        &[
            "board",
            "decision",
            "decide",
            "D-1",
            "--outcome",
            "sqlite",
            "--rationale",
            "local durability",
            "--output",
            "json",
        ],
    );
    assert!(
        decided.status.success(),
        "{}",
        String::from_utf8_lossy(&decided.stderr)
    );
    let story = fs::read_to_string(root.join("docs/stories/C-1-story.md")).unwrap();
    assert!(story.contains("status: ready"), "{story}");
    assert!(story.contains("priority: 1"), "{story}");
    assert!(!story.contains("blocked_decision"), "{story}");
    let prompts = flux(&root, &["fleet", "decisions", "--output", "json"]);
    let prompts: serde_json::Value = serde_json::from_slice(&prompts.stdout).unwrap();
    assert_eq!(prompts["data"]["attention_required"], false);
    fs::remove_dir_all(root).ok();
}

#[test]
fn session_board_cli_reopens_the_event_projection_and_conflicts_stale_writers() {
    let root = fixture("session-board");
    let store_dir = root.join("store");
    fs::create_dir_all(&store_dir).unwrap();
    let events = flux_events::EventStore::open(store_dir.join("events.db")).unwrap();
    let session = events.create_session("fixture/model").unwrap();
    drop(events);

    let created = flux(
        &root,
        &[
            "--store",
            "store",
            "board",
            "--scope",
            "session",
            "--session",
            &session,
            "create",
            "--id",
            "S-1",
            "--title",
            "Scratch task",
            "--if-revision",
            "0",
            "--idempotency-key",
            "create-S-1",
            "--output",
            "json",
        ],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stdout)
    );
    let created_again = flux(
        &root,
        &[
            "--store",
            "store",
            "board",
            "--scope",
            "session",
            "--session",
            &session,
            "create",
            "--id",
            "S-1",
            "--title",
            "Scratch task",
            "--if-revision",
            "0",
            "--idempotency-key",
            "create-S-1",
            "--output",
            "json",
        ],
    );
    assert!(created_again.status.success());
    assert_eq!(created.stdout, created_again.stdout);

    let reopened = flux(
        &root,
        &[
            "--store",
            "store",
            "board",
            "--scope",
            "session",
            "--session",
            &session,
            "get",
            "S-1",
            "--output",
            "json",
        ],
    );
    assert!(reopened.status.success());
    let reopened: serde_json::Value = serde_json::from_slice(&reopened.stdout).unwrap();
    assert_eq!(reopened["revision"], "1");
    assert_eq!(reopened["data"]["title"], "Scratch task");
    assert_eq!(reopened["data"]["status"], "backlog");

    let stale = flux(
        &root,
        &[
            "--store",
            "store",
            "board",
            "--scope",
            "session",
            "--session",
            &session,
            "create",
            "--id",
            "S-2",
            "--title",
            "Stale",
            "--if-revision",
            "0",
            "--output",
            "json",
        ],
    );
    assert_eq!(stale.status.code(), Some(4));
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_admits_configured_and_on_the_fly_agents_but_never_a_second_main() {
    let root = fixture("agent-admission");
    fs::create_dir_all(root.join(".flux/fleet/agents")).unwrap();
    fs::write(
        root.join(".flux/fleet/main.md"),
        "Coordinate against goals and board authority.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/agents/scout.md"),
        "Inspect the assigned scope and return evidence.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nmax_workers = 3\nmax_wave = 10\nmax_rework = 2\nallow_ad_hoc_agents = true\n\n[main]\ninstructions = \".flux/fleet/main.md\"\n\n[[agent_templates]]\nid = \"scout\"\nrole = \"researcher\"\ninstructions = \".flux/fleet/agents/scout.md\"\nmode = \"read-only\"\nmax_instances = 1\n",
    )
    .unwrap();
    assert!(flux(&root, &["fleet", "start"]).status.success());

    let configured = flux(
        &root,
        &[
            "fleet",
            "spawn",
            "--template",
            "scout",
            "--name",
            "scout-1",
            "--output",
            "json",
        ],
    );
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stdout)
    );
    let configured: serde_json::Value = serde_json::from_slice(&configured.stdout).unwrap();
    assert_eq!(configured["data"]["parent"], "main");
    assert_eq!(configured["data"]["template"], "scout");
    assert_eq!(configured["data"]["ephemeral"], false);

    let ad_hoc = flux(
        &root,
        &[
            "fleet",
            "spawn",
            "--role",
            "critic",
            "--instructions",
            "Challenge the recommendation against project goals",
            "--name",
            "critic-1",
            "--output",
            "json",
        ],
    );
    assert!(ad_hoc.status.success());
    let ad_hoc: serde_json::Value = serde_json::from_slice(&ad_hoc.stdout).unwrap();
    assert_eq!(ad_hoc["data"]["ephemeral"], true);
    assert_eq!(ad_hoc["data"]["created_by"], "main");

    let impostor = flux(
        &root,
        &[
            "fleet",
            "spawn",
            "--role",
            "coordinator",
            "--instructions",
            "replace main",
            "--output",
            "json",
        ],
    );
    assert_eq!(impostor.status.code(), Some(2));
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_dispatch_creates_a_pinned_wave_and_inheriting_story_worktrees() {
    let root = fixture("wave-topology");
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    let base = String::from_utf8(git(&root, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(flux(&root, &["fleet", "start"]).status.success());

    let dispatched = flux(
        &root,
        &[
            "fleet",
            "run",
            "repo/C-1",
            "--prepare-only",
            "--output",
            "json",
        ],
    );
    assert!(
        dispatched.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&dispatched.stdout),
        String::from_utf8_lossy(&dispatched.stderr)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let repository = &dispatched["data"]["topology"]["repositories"][0];
    assert_eq!(repository["base_commit"], base);
    assert_eq!(repository["stories"][0]["base_commit"], base);
    let integration = PathBuf::from(repository["integration"]["worktree"].as_str().unwrap());
    let story = PathBuf::from(repository["stories"][0]["worktree"].as_str().unwrap());
    assert!(integration.is_dir());
    assert!(story.is_dir());
    assert_eq!(
        String::from_utf8(git(&integration, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim(),
        base
    );
    assert_eq!(
        String::from_utf8(git(&story, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim(),
        base
    );
    assert_eq!(repository["stories"][0]["board_ref"], "repo/C-1");
}

#[test]
fn fleet_verifies_handoff_runs_one_final_gate_and_applies_only_explicitly() {
    let root = fixture("wave-integration");
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\nfences = [\".flux/fleet/**\"]\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    assert!(flux(&root, &["fleet", "start"]).status.success());
    let dispatched = flux(
        &root,
        &[
            "fleet",
            "run",
            "repo/C-1",
            "--prepare-only",
            "--output",
            "json",
        ],
    );
    assert!(dispatched.status.success());
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let story = PathBuf::from(
        dispatched["data"]["topology"]["repositories"][0]["stories"][0]["worktree"]
            .as_str()
            .unwrap(),
    );
    fs::write(story.join("result.txt"), "implemented\n").unwrap();
    assert!(git(&story, &["add", "result.txt"]).status.success());
    assert!(git(&story, &["commit", "-qm", "implement story"])
        .status
        .success());
    let commit = String::from_utf8(git(&story, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();

    let handoff = flux(
        &root,
        &[
            "fleet",
            "handoff",
            "wave-2",
            "repo/C-1",
            "--commit",
            &commit,
            "--write-set",
            "result.txt",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "result.txt",
            "--failing-before",
            "--passing-after",
            "--summary",
            "Implemented the story contract",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&handoff.stdout),
        String::from_utf8_lossy(&handoff.stderr)
    );
    let handoff: serde_json::Value = serde_json::from_slice(&handoff.stdout).unwrap();
    assert_eq!(handoff["data"]["schema"], "flux.fleet-handoff/v1");
    assert_eq!(handoff["data"]["commit"], commit);
    assert_eq!(handoff["data"]["failing_before"]["success"], false);
    assert_eq!(handoff["data"]["passing_after"]["success"], true);

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    assert_eq!(
        integrated["data"]["topology"]["repositories"][0]["gate"]["runs"],
        1
    );
    assert!(
        !root.join("result.txt").exists(),
        "integrate must not modify main"
    );

    let applied = flux(&root, &["fleet", "apply", "wave-2", "--output", "json"]);
    assert!(
        applied.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&applied.stdout),
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(applied["data"]["merged_locally"], true);
    assert_eq!(applied["data"]["pushed"], false);
    assert_eq!(applied["data"]["released"], false);
    assert_eq!(applied["data"]["deployed"], false);
    assert_eq!(
        fs::read_to_string(root.join("result.txt")).unwrap(),
        "implemented\n"
    );
}

#[test]
fn fleet_combined_only_failure_runs_the_final_gate_once_and_preserves_candidate() {
    let root = fixture("combined-only-red");
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    for (id, title, priority) in [("C-1", "One", 1), ("C-2", "Two", 2)] {
        fs::write(
            root.join(format!("docs/stories/{id}-story.md")),
            format!(
                "---\nid: {id}\ntitle: {title}\nstatus: ready\npriority: {priority}\n---\n\n# {title}\n\n## Acceptance\n\n- [ ] ship\n"
            ),
        )
        .unwrap();
    }
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"sh\", \"-c\", \"test ! -f one.txt || test ! -f two.txt\"]\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    let base = String::from_utf8(git(&root, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(flux(&root, &["fleet", "start"]).status.success());
    let dispatched = flux(
        &root,
        &[
            "fleet",
            "run",
            "repo/C-1",
            "repo/C-2",
            "--prepare-only",
            "--output",
            "json",
        ],
    );
    assert!(dispatched.status.success());
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let stories = dispatched["data"]["topology"]["repositories"][0]["stories"]
        .as_array()
        .unwrap();
    for (index, filename) in ["one.txt", "two.txt"].iter().enumerate() {
        let story = PathBuf::from(stories[index]["worktree"].as_str().unwrap());
        fs::write(story.join(filename), format!("story {}\n", index + 1)).unwrap();
        assert!(git(&story, &["add", filename]).status.success());
        assert!(git(&story, &["commit", "-qm", &format!("add {filename}")])
            .status
            .success());
        let commit = String::from_utf8(git(&story, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let item = format!("repo/C-{}", index + 1);
        let handoff = flux(
            &root,
            &[
                "fleet",
                "handoff",
                "wave-2",
                &item,
                "--commit",
                &commit,
                "--write-set",
                filename,
                "--test-arg",
                "test",
                "--test-arg",
                "-f",
                "--test-arg",
                filename,
                "--failing-before",
                "--passing-after",
                "--summary",
                "targeted child change",
                "--output",
                "json",
            ],
        );
        assert!(
            handoff.status.success(),
            "{}",
            String::from_utf8_lossy(&handoff.stdout)
        );
    }

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert_eq!(integrated.status.code(), Some(7));
    let error: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(error["error"]["class"], "validation/gate");
    assert_eq!(
        String::from_utf8(git(&root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim(),
        base,
        "a red integration must not touch the source checkout"
    );
    let inspected = flux(
        &root,
        &[
            "fleet",
            "inspect",
            "integration",
            "wave-2",
            "--output",
            "json",
        ],
    );
    assert!(inspected.status.success());
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let repository = &inspected["data"]["data"]["repositories"][0];
    assert_eq!(inspected["data"]["data"]["status"], "red");
    assert_eq!(repository["gate"]["runs"], 1);
    assert_eq!(repository["gate"]["status"], "red");
    let candidate = repository["candidate"].as_str().unwrap();
    assert!(!candidate.is_empty());
    let integration = PathBuf::from(repository["integration"]["worktree"].as_str().unwrap());
    assert!(integration.join("one.txt").is_file());
    assert!(integration.join("two.txt").is_file());

    let retry = flux(&root, &["fleet", "integrate", "wave-2"]);
    assert!(
        !retry.status.success(),
        "a red wave cannot spend a second gate run"
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_rework_stays_with_one_session_twice_and_the_third_request_parks() {
    let (root, story) = one_story_wave("rework-budget");
    let mut commit = commit_result(&story, "attempt one");
    let first_handoff = submit_result_handoff(&root, &commit);
    let session = first_handoff["data"]["session"]
        .as_str()
        .unwrap()
        .to_string();

    for attempt in 1..=2 {
        let key = format!("rework-{attempt}");
        let reviewed = commit.clone();
        let rework = flux(
            &root,
            &[
                "fleet",
                "rework",
                "wave-2",
                "repo/C-1",
                "--reviewer",
                "fresh-reviewer",
                "--reviewed-commit",
                &reviewed,
                "--path",
                "result.txt:1:Clarify the result",
                "--idempotency-key",
                &key,
                "--output",
                "json",
            ],
        );
        assert!(rework.status.success());
        let replay = flux(
            &root,
            &[
                "fleet",
                "rework",
                "wave-2",
                "repo/C-1",
                "--reviewer",
                "fresh-reviewer",
                "--reviewed-commit",
                &reviewed,
                "--path",
                "result.txt:1:Clarify the result",
                "--idempotency-key",
                &key,
                "--output",
                "json",
            ],
        );
        assert_eq!(
            rework.stdout, replay.stdout,
            "replay must not consume a round"
        );
        let rework: serde_json::Value = serde_json::from_slice(&rework.stdout).unwrap();
        assert_eq!(rework["data"]["decision"], "REWORK");
        assert_eq!(rework["data"]["ack"], "delivered");
        assert_eq!(rework["data"]["attempt"], attempt);
        assert_eq!(rework["data"]["session"], session);

        commit = commit_result(&story, &format!("attempt {} fixed", attempt));
        let next_handoff = submit_result_handoff(&root, &commit);
        assert_eq!(next_handoff["data"]["session"], session);
    }

    let parked = flux(
        &root,
        &[
            "fleet",
            "rework",
            "wave-2",
            "repo/C-1",
            "--reviewer",
            "fresh-reviewer",
            "--reviewed-commit",
            &commit,
            "--invariant",
            "The contract is still ambiguous",
            "--output",
            "json",
        ],
    );
    assert!(parked.status.success());
    let parked: serde_json::Value = serde_json::from_slice(&parked.stdout).unwrap();
    assert_eq!(parked["data"]["decision"], "PARK");
    assert_eq!(parked["data"]["ack"], "not-dispatched");
    assert_eq!(parked["data"]["attempt"], 2);
    assert_eq!(parked["data"]["session"], session);
    let integration = flux(&root, &["fleet", "integrate", "wave-2"]);
    assert!(
        !integration.status.success(),
        "parked work cannot integrate"
    );
}

#[test]
fn scriptless_inspection_and_report_surfaces_are_bounded_and_deterministic() {
    let (root, _story) = one_story_wave("scriptless-inspection");

    for (view, target) in [
        ("snapshot", None),
        ("wave", Some("wave-2")),
        ("worker", Some("wave-2-worker-1")),
        ("result", Some("repo/C-1")),
        ("activity", None),
        ("worktree", None),
        ("integration", Some("wave-2")),
        ("source", Some("repo")),
        ("search", Some("wave")),
        ("story", Some("repo/C-1")),
        ("pull-request", Some("wave-2")),
    ] {
        let mut args = vec!["fleet", "inspect", view];
        if let Some(target) = target {
            args.push(target);
        }
        args.extend(["--limit", "7", "--output", "json"]);
        let first = flux(&root, &args);
        let second = flux(&root, &args);
        assert!(
            first.status.success(),
            "{view}: {}",
            String::from_utf8_lossy(&first.stdout)
        );
        assert_eq!(first.stdout, second.stdout, "{view} changed between reads");
        let value: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(value["data"]["bounded"], true, "{view}");
        assert_eq!(value["data"]["limit"], 7, "{view}");
    }

    for args in [
        vec!["fleet", "status", "--output", "json"],
        vec!["fleet", "schedule", "--output", "json"],
        vec!["fleet", "worktrees", "--output", "json"],
        vec!["fleet", "events", "--limit", "7", "--output", "json"],
        vec!["fleet", "logs", "--limit", "7", "--output", "json"],
        vec!["fleet", "agents", "--output", "json"],
        vec!["fleet", "dashboard", "--output", "json"],
    ] {
        let output = flux(&root, &args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema"], "flux.cli/v1", "{args:?}");
    }

    let stats = flux(&root, &["board", "stats", "--history", "--output", "json"]);
    assert!(stats.status.success());
    let cube: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(cube["data"]["schema"], "flux.board-stats/v1");
    assert!(cube["data"]["history"]["days"].is_array());
    for (format, marker) in [
        ("json", "flux.board-stats/v1"),
        ("tsv", "dimension\tdone"),
        ("html", "<!doctype html>"),
        ("svg", "<svg"),
    ] {
        let output = flux(&root, &["board", "report", "--format", format]);
        assert!(output.status.success(), "{format}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(marker),
            "{format}"
        );
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_delivers_to_a_real_durable_main_agent_session() {
    let root = fixture("durable-main-turn");
    fs::create_dir_all(root.join(".flux/fleet")).unwrap();
    fs::write(
        root.join(".flux/fleet/main.md"),
        "Act as the only main coordinator and acknowledge the request.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\n\n[main]\ninstructions = \".flux/fleet/main.md\"\nmodel = \"mock\"\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    assert!(flux(&root, &["fleet", "start"]).status.success());

    let first = flux(
        &root,
        &[
            "fleet",
            "message",
            "main",
            "Inspect the durable intake",
            "--wait",
            "completed",
            "--output",
            "json",
        ],
    );
    assert!(
        first.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["data"]["receipt"]["ack"], "completed");
    assert_eq!(first["data"]["receipt"]["session"], "s_1");
    assert!(first["data"]["receipt"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "turn_start")
        .unwrap()
        .get("input")
        .is_none());

    let second = flux(
        &root,
        &[
            "fleet",
            "message",
            "main",
            "Continue in the same coordinator session",
            "--wait",
            "delivered",
            "--output",
            "json",
        ],
    );
    assert!(
        second.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["data"]["requested_ack"], "delivered");
    assert_eq!(second["data"]["receipt"]["ack"], "completed");
    assert_eq!(second["data"]["receipt"]["session"], "s_1");
    assert!(root
        .join(".git/flux-fleet/sessions/main/events.db")
        .is_file());
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_run_launches_a_real_local_story_agent_in_its_child_worktree() {
    let root = fixture("real-story-agent");
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/stories/C-2-story.md"),
        "---\nid: C-2\ntitle: Second story\nstatus: ready\npriority: 2\n---\n\n# Second story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux/fleet/agents")).unwrap();
    fs::write(
        root.join(".flux/fleet/agents/story-worker.md"),
        "Work only in the assigned story worktree and report evidence.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[[agent_templates]]\nid = \"story-worker\"\nrole = \"writer\"\ninstructions = \".flux/fleet/agents/story-worker.md\"\nmodel = \"mock\"\nmode = \"write\"\nmax_instances = 3\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n",
    )
    .unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "fleet@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    assert!(flux(&root, &["fleet", "start"]).status.success());

    let run = flux(
        &root,
        &["fleet", "run", "repo/C-1", "repo/C-2", "--output", "json"],
    );
    assert!(
        run.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let run: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(run["data"]["ack"], "completed");
    assert_eq!(run["data"]["receipts"][0]["session"], "s_1");
    assert_eq!(run["data"]["receipts"].as_array().unwrap().len(), 2);
    let stories = run["data"]["topology"]["repositories"][0]["stories"]
        .as_array()
        .unwrap();
    for story in stories {
        let story_worktree = PathBuf::from(story["worktree"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(story_worktree.join("flux-mock.txt")).unwrap(),
            "created by flux mock\n"
        );
    }

    let first_worktree = PathBuf::from(stories[0]["worktree"].as_str().unwrap());
    fs::remove_file(first_worktree.join("flux-mock.txt")).unwrap();
    fs::write(first_worktree.join("result.txt"), "first implementation\n").unwrap();
    assert!(git(&first_worktree, &["add", "result.txt"])
        .status
        .success());
    assert!(
        git(&first_worktree, &["commit", "-qm", "implement first story"])
            .status
            .success()
    );
    let first_commit = String::from_utf8(git(&first_worktree, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let handoff = flux(
        &root,
        &[
            "fleet",
            "handoff",
            "wave-2",
            "repo/C-1",
            "--commit",
            &first_commit,
            "--write-set",
            "result.txt",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "result.txt",
            "--failing-before",
            "--passing-after",
            "--summary",
            "implemented first story",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&handoff.stdout),
        String::from_utf8_lossy(&handoff.stderr)
    );
    let handoff: serde_json::Value = serde_json::from_slice(&handoff.stdout).unwrap();
    assert_eq!(handoff["data"]["session"], "s_1");
    let rework = flux(
        &root,
        &[
            "fleet",
            "rework",
            "wave-2",
            "repo/C-1",
            "--reviewer",
            "fresh-reviewer",
            "--reviewed-commit",
            &first_commit,
            "--path",
            "result.txt:1:Clarify the implementation",
            "--output",
            "json",
        ],
    );
    assert!(
        rework.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&rework.stdout),
        String::from_utf8_lossy(&rework.stderr)
    );
    let rework: serde_json::Value = serde_json::from_slice(&rework.stdout).unwrap();
    assert_eq!(rework["data"]["ack"], "completed");
    assert_eq!(rework["data"]["turn_receipt"]["session"], "s_1");
    assert!(
        git(&root, &["status", "--short"]).stdout.is_empty(),
        "the source checkout remains untouched"
    );
    fs::remove_dir_all(root).ok();
}
