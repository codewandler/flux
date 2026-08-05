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
        .args(args)
        .output()
        .unwrap()
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
    let dispatched = flux(&root, &["fleet", "run", "repo/C-1", "--output", "json"]);
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

    let dispatched = flux(&root, &["fleet", "run", "repo/C-1", "--output", "json"]);
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
    let dispatched = flux(&root, &["fleet", "run", "repo/C-1", "--output", "json"]);
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
