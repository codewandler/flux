#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
fn fixture_backend_path() -> OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let bin = std::env::temp_dir().join(format!(
            "flux-board-fleet-test-bin-{}",
            std::process::id()
        ));
        fs::create_dir_all(&bin).unwrap();
        let bwrap = bin.join("bwrap");
        fs::write(
            &bwrap,
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--\" ]; then\n    shift\n    exec \"$@\"\n  fi\n  shift\ndone\nexit 64\n",
        )
        .unwrap();
        fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o755)).unwrap();

        let ambient = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![bin];
        entries.extend(std::env::split_paths(&ambient));
        std::env::join_paths(entries).unwrap()
    })
    .clone()
}

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

const TEST_FLEET_LOOP_POLICY: &str = r#"
[loop_profiles.implementation]
revision = "1"
source = ".flux/fleet/loops/implementation.flux"
entry = "work"

[loop_profiles.research]
revision = "1"
source = ".flux/fleet/loops/research.flux"
entry = "research"

[loop_policy]
implementation = "implementation"
research = "research"
"#;

fn install_test_fleet_loops(root: &Path) {
    fs::create_dir_all(root.join(".flux/fleet/loops")).unwrap();
    fs::write(
        root.join(".flux/fleet/loops/implementation.flux"),
        "flow work -> string\n  $turn = ai_segment({ goal: \"implement the exact assignment\", tools: [\"read\", \"write\"], max_rounds: 8, current_turn: true })\n  return $turn.result\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/loops/research.flux"),
        "flow research -> string\n  $turn = ai_segment({ goal: \"research the exact request\", tools: [\"read\"], max_rounds: 4, current_turn: true })\n  return $turn.result\n",
    )
    .unwrap();
}

fn flux(root: &PathBuf, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flux"));
    command
        .current_dir(root)
        // These fixtures exercise planning/control semantics, not process confinement. Pin the
        // outer posture so the board/fleet command itself never inherits a developer/CI mode.
        .env("FLUX_SANDBOX", "off")
        .args(args);
    #[cfg(target_os = "linux")]
    command.env("PATH", fixture_backend_path());
    command.output().unwrap()
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
            // An unquoted `#` at the start of a word opens a shell comment, exactly as a shell would
            // read it. The skill examples are documentation as much as they are executable, and the
            // trailing note on `board next --independent` — that it returns a wave-safe set rather
            // than a priority prefix — is the reason a coordinator reaches for the flag at all.
            // Without this the example only stays runnable by being stripped of what it teaches.
            (None, '#') if word.is_empty() => break,
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

#[test]
fn a_skill_example_may_carry_a_trailing_comment_without_becoming_unrunnable() {
    assert_eq!(
        shell_words("flux board next --limit 8 --independent  # a wave-safe set"),
        vec!["flux", "board", "next", "--limit", "8", "--independent"]
    );
    // Only a `#` that opens a word is a comment. One inside a word, or inside quotes, is data — a
    // reason string or an issue reference must survive intact, or the stripping would silently
    // rewrite the very command the example promises is executable.
    assert_eq!(
        shell_words("flux fleet park wave-7 --reason \"blocked on #42\" --tag a#b"),
        vec![
            "flux",
            "fleet",
            "park",
            "wave-7",
            "--reason",
            "blocked on #42",
            "--tag",
            "a#b"
        ]
    );
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
    install_test_fleet_loops(&root);
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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

/// Record an independent PASS over one exact candidate, the way a dispatched reviewer would.
///
/// C-587 gates integration on a review by an agent that is not the story's writer, so every fixture
/// that integrates now has to have been examined by one. These fixtures run offline against no
/// provider, so they submit the reviewer's typed document through `--from` — the same parser, the
/// same closed vocabularies, and the same refusal of a reviewer that is the story's own writer.
fn record_passing_review(root: &PathBuf, wave: &str, item: &str, commit: &str) {
    let document = root.join(format!("review-{item}-{commit}.json").replace('/', "-"));
    fs::write(
        &document,
        serde_json::to_string(&serde_json::json!({
            "schema": "flux.fleet-review/v1",
            "reviewer": "fixture-reviewer",
            "reviewed_commit": commit,
            "verdict": "PASS",
            "findings": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let reviewed = flux(
        root,
        &[
            "fleet",
            "review",
            wave,
            "--item",
            item,
            "--from",
            document.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        reviewed.status.success(),
        "review of {item}@{commit} failed: {} {}",
        String::from_utf8_lossy(&reviewed.stdout),
        String::from_utf8_lossy(&reviewed.stderr)
    );
    fs::remove_file(&document).ok();
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
    // C-735: the skill documents `board commit`, so its fixture needs a branch for a document to
    // land on. The examples run in order, which means the transition example dirties C-1 and the
    // commit example genuinely commits it.
    assert!(git(&board_root, &["init", "-q"]).status.success());
    assert!(
        git(&board_root, &["config", "user.email", "board@example.test"])
            .status
            .success()
    );
    assert!(
        git(&board_root, &["config", "user.name", "Flux Board Test"])
            .status
            .success()
    );
    assert!(git(&board_root, &["add", "."]).status.success());
    assert!(git(&board_root, &["commit", "-qm", "fixture"])
        .status
        .success());
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
    fs::create_dir_all(fleet_root.join(".flux/fleet/loops")).unwrap();
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
        fleet_root.join(".flux/fleet/loops/main.flux"),
        "flow fleet-main -> string\n  return \"fixture main\"\n",
    )
    .unwrap();
    fs::write(
        fleet_root.join(".flux/fleet/loops/research.flux"),
        "flow fleet-research -> string\n  return \"fixture research\"\n",
    )
    .unwrap();
    fs::write(
        fleet_root.join(".flux/fleet/loops/implementation.flux"),
        "flow work -> string\n  return \"fixture worker\"\n",
    )
    .unwrap();
    fs::write(
        fleet_root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[main]\ninstructions = \".flux/fleet/main.md\"\nmodel = \"mock\"\nloop = \".flux/fleet/loops/main.flux\"\nresearch_loop = \".flux/fleet/loops/research.flux\"\n{TEST_FLEET_LOOP_POLICY}\n[[agent_templates]]\nid = \"story-worker\"\nrole = \"writer\"\ntask_kind = \"implementation\"\ninstructions = \".flux/fleet/agents/story-worker.md\"\nmodel = \"mock\"\nmode = \"write\"\ncapabilities = [\"read\", \"edit\", \"git\", \"shell\"]\nmax_instances = 1\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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
            record_passing_review(&fleet_root, &wave, "repo/C-1", &commit);
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
    // C-619: the documented examples end at `fleet apply`, which accepts rather than merges — so the
    // merged file must be absent and the accepted tag present. Asserting the tag keeps this test's
    // point (the whole documented sequence really runs end to end) without asserting the one step the
    // contract deliberately no longer performs.
    assert!(
        !fleet_root.join("result.txt").is_file(),
        "apply must not merge into the source checkout"
    );
    let tags = git(
        &fleet_root,
        &["tag", "--list", &format!("fleet/accepted/{wave}/*")],
    );
    assert!(
        !String::from_utf8_lossy(&tags.stdout).trim().is_empty(),
        "apply must pin the candidate with an accepted tag"
    );
    fs::remove_dir_all(fleet_root).ok();
}

/// A board fixture with one tracked file, a first commit, and a configured committer.
///
/// git records no empty directory, so a board needs one tracked file before it has a first commit.
fn commit_fixture(name: &str) -> PathBuf {
    let root = fixture(name);
    fs::write(root.join("docs/stories/README.md"), "# Board\n").unwrap();
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.email", "board@example.test"])
        .status
        .success());
    assert!(git(&root, &["config", "user.name", "Flux Board Test"])
        .status
        .success());
    assert!(git(&root, &["add", "."]).status.success());
    assert!(git(&root, &["commit", "-qm", "fixture"]).status.success());
    root
}

fn board_json(root: &PathBuf, args: &[&str]) -> serde_json::Value {
    let output = flux(root, args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn head(root: &PathBuf) -> String {
    String::from_utf8(git(root, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn paths_in(root: &PathBuf, sha: &str) -> Vec<String> {
    String::from_utf8(git(root, &["show", "--name-only", "--pretty=format:", sha]).stdout)
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Status with every untracked file named, rather than collapsed to its directory.
fn porcelain(root: &PathBuf) -> String {
    String::from_utf8(git(root, &["status", "--porcelain", "-uall"]).stdout).unwrap()
}

/// C-735, failing first: authoring never commits, and exactly one verb does.
///
/// `create` used to be the single mutating op that committed, and what it committed was the stub whose
/// Acceptance reads `- [ ] Define acceptance.` — so a story's committed form was the one where its
/// definition of done did not exist, while a board read resolves items at a git ref and could schedule
/// it anyway. Deferring every commit is only safe if the resulting window is reported, so `board check`
/// names the uncommitted document, and `board commit` commits exactly what it is told about.
#[test]
fn authoring_defers_and_one_verb_commits_exactly_what_it_names() {
    let root = commit_fixture("commit-verb");

    // Unrelated uncommitted work that must survive untouched.
    fs::write(root.join("UNRELATED.md"), "someone else's work\n").unwrap();

    let before = head(&root);
    let created = board_json(
        &root,
        &[
            "board",
            "create",
            "--kind",
            "story",
            "--title",
            "Authored then committed",
            "--status",
            "ready",
            "--priority",
            "7",
            "--output",
            "json",
        ],
    );
    assert!(
        created["data"]["commit"].is_null(),
        "creation authors a document and commits nothing: {created}"
    );
    let id = created["data"]["id"].as_str().unwrap().to_string();
    // `data.file` is the absolute authored path; git speaks board-relative paths.
    let file = Path::new(created["data"]["file"].as_str().unwrap())
        .strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(before, head(&root), "creation moved HEAD");
    assert!(
        String::from_utf8(git(&root, &["ls-tree", "HEAD", "--", &file]).stdout)
            .unwrap()
            .trim()
            .is_empty(),
        "the stub must not be the committed form of {id}"
    );

    // The window between authoring and committing is reported, not silent.
    let checked = board_json(&root, &["board", "check", "--output", "json"]);
    assert!(
        checked["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap_or_default().contains(&file)),
        "check must report the uncommitted document: {checked}"
    );
    assert!(
        checked["data"]["uncommitted"]
            .as_array()
            .expect("check reports uncommitted documents as machine-readable data")
            .iter()
            .any(|entry| entry["path"].as_str() == Some(file.as_str())),
        "check data must name the uncommitted document: {checked}"
    );

    // The meaningful edit — the one that used to arrive after the commit — lands first.
    let story = root.join(&file);
    let authored = fs::read_to_string(&story)
        .unwrap()
        .replace("- [ ] Define acceptance.", "- [ ] The verb commits.");
    fs::write(&story, authored).unwrap();

    let committed = board_json(
        &root,
        &["board", "commit", "--item", &id, "--output", "json"],
    );
    let sha = committed["data"]["commit"]
        .as_str()
        .expect("the verb reports the commit it made")
        .to_string();
    assert_eq!(
        sha,
        head(&root),
        "the reported sha is the commit that landed"
    );
    assert_eq!(
        paths_in(&root, &sha),
        vec![file.clone()],
        "path-scoped commit"
    );
    assert_eq!(
        committed["data"]["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![file.clone()],
        "the verb reports the documents that actually landed: {committed}"
    );
    // The committed form now carries the authored Acceptance, not the stub.
    let landed = String::from_utf8(git(&root, &["show", &format!("{sha}:{file}")]).stdout).unwrap();
    assert!(
        landed.contains("- [ ] The verb commits."),
        "the committed form is the authored one: {landed}"
    );

    // The unrelated file was neither committed nor staged.
    assert_eq!(
        porcelain(&root).trim(),
        "?? UNRELATED.md",
        "board commit never sweeps the checkout it runs in"
    );

    // Idempotent: nothing left to commit, said plainly, exit 0, HEAD untouched.
    let again = board_json(
        &root,
        &["board", "commit", "--item", &id, "--output", "json"],
    );
    assert!(
        again["data"]["commit"].is_null(),
        "a second commit of the same document makes no commit: {again}"
    );
    assert_eq!(sha, head(&root), "the idempotent call moved HEAD");
    let spoken = flux(&root, &["board", "commit", "--item", &id]);
    assert!(spoken.status.success());
    let spoken = String::from_utf8(spoken.stdout).unwrap();
    assert!(
        spoken.contains("nothing to commit"),
        "it must say plainly that there was nothing to commit: {spoken:?}"
    );

    // And the check finding clears once the document is on the branch.
    let checked = board_json(&root, &["board", "check", "--output", "json"]);
    assert!(
        checked["data"]["uncommitted"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the finding clears once the document is committed: {checked}"
    );
    fs::remove_dir_all(root).ok();
}

/// C-735: `--all` means every planning document, never every dirty file.
///
/// Four concurrent writers share this repository. "Commit path-scoped, never sweep another session's
/// work" is the hard rule, so the board's own document roots are the fence — a dirty manifest, a
/// lockfile or a source file is out of scope by construction, and an explicit path outside those roots
/// is refused rather than quietly committed.
#[test]
fn board_commit_all_is_scoped_to_the_boards_own_documents() {
    let root = commit_fixture("commit-all-scope");
    fs::create_dir_all(root.join("crates/thing/src")).unwrap();
    fs::write(root.join("Cargo.lock"), "# another session\n").unwrap();
    fs::write(root.join("crates/thing/src/lib.rs"), "fn other() {}\n").unwrap();

    for title in ["First document", "Second document"] {
        board_json(
            &root,
            &[
                "board", "create", "--kind", "story", "--title", title, "--output", "json",
            ],
        );
    }

    let committed = board_json(&root, &["board", "commit", "--all", "--output", "json"]);
    let sha = committed["data"]["commit"].as_str().unwrap().to_string();
    let landed = paths_in(&root, &sha);
    assert_eq!(
        landed.len(),
        2,
        "exactly the two documents landed: {landed:?}"
    );
    assert!(
        landed.iter().all(|path| path.starts_with("docs/stories/")),
        "`--all` is scoped to planning documents: {landed:?}"
    );

    let status = porcelain(&root);
    assert!(
        status.contains("Cargo.lock") && status.contains("crates/thing/src/lib.rs"),
        "another session's work is untouched: {status:?}"
    );

    // An explicit path outside the board's document roots is refused, not committed.
    let refused = flux(
        &root,
        &[
            "board",
            "commit",
            "crates/thing/src/lib.rs",
            "--output",
            "json",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(5),
        "an out-of-scope path is a permission refusal: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(sha, head(&root), "the refusal committed nothing");
    fs::remove_dir_all(root).ok();
}

/// C-735: mid-merge, the verb refuses and the documents stay on disk.
///
/// Committing into someone else's in-progress merge is how a conflict resolution acquires files nobody
/// resolved. The document is already written, so the refusal names it and loses nothing.
#[test]
fn board_commit_refuses_mid_merge_and_keeps_the_document_on_disk() {
    let root = commit_fixture("commit-mid-merge");
    let trunk = String::from_utf8(git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(git(&root, &["checkout", "-q", "-b", "side"])
        .status
        .success());
    fs::write(root.join("docs/stories/README.md"), "# Board (side)\n").unwrap();
    assert!(git(&root, &["commit", "-qam", "side"]).status.success());
    assert!(git(&root, &["checkout", "-q", &trunk]).status.success());
    fs::write(root.join("docs/stories/README.md"), "# Board (trunk)\n").unwrap();
    assert!(git(&root, &["commit", "-qam", "trunk"]).status.success());
    assert!(
        !git(&root, &["merge", "side"]).status.success(),
        "the fixture needs a conflicted merge"
    );

    let created = board_json(
        &root,
        &[
            "board",
            "create",
            "--kind",
            "story",
            "--title",
            "Written mid merge",
            "--output",
            "json",
        ],
    );
    let file = Path::new(created["data"]["file"].as_str().unwrap())
        .strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let refused = flux(&root, &["board", "commit", "--all", "--output", "json"]);
    assert_eq!(
        refused.status.code(),
        Some(4),
        "mid-merge is a conflict/precondition refusal: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&file),
        "the refusal names the document it did not commit: {refused}"
    );
    assert!(
        root.join(&file).is_file(),
        "the document stays on disk: {file}"
    );
    fs::remove_dir_all(root).ok();
}

/// C-735: `commit` joins the family without bending the envelope it joins.
///
/// The session backend's refusal is not exercised here: reaching it needs a recorded session, and no
/// sibling file-backed operation (`reconcile`, `render`, `sync`, `import`, …) is covered that way
/// either. `board_action_mutates`/`board_action_requires_member` carry the classification below.
#[test]
fn board_commit_is_a_published_mutation_that_never_widens_its_own_scope() {
    let root = commit_fixture("commit-schema");
    let schema = board_json(&root, &["board", "schema", "--output", "json"]);
    let published = schema["data"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["name"] == "commit")
        .unwrap_or_else(|| panic!("commit must be a published board operation: {schema}"));
    assert_eq!(published["mutation"], true);
    assert_eq!(published["supports"]["dry_run"], true);
    assert_eq!(published["supports"]["if_revision"], true);

    // `board call` routes it like every other operation.
    let request = root.join("commit-request.json");
    fs::write(
        &request,
        r#"{"schema":"flux.cli/v1","request_id":"board-commit","args":["--all"]}"#,
    )
    .unwrap();
    let called = board_json(
        &root,
        &[
            "board",
            "call",
            "commit",
            "--request",
            request.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert_eq!(called["request_id"], "board-commit");
    assert!(
        called["data"]["commit"].is_null(),
        "a clean board has nothing to commit: {called}"
    );
    fs::remove_file(&request).ok();

    // The verb refuses to guess its own scope.
    let unscoped = flux(&root, &["board", "commit", "--output", "json"]);
    assert_eq!(
        unscoped.status.code(),
        Some(2),
        "commit without a scope is refused, never widened: {}",
        String::from_utf8_lossy(&unscoped.stdout)
    );
    fs::remove_dir_all(root).ok();
}

/// C-735: a deferred `create` no longer refuses the next workspace mutation.
///
/// The clean-checkout guard exists so a board mutation cannot land on top of somebody else's
/// in-progress work. Now that authoring commits nothing, the board's own uncommitted documents are the
/// normal state between an op and `flux board commit`, and a `create` that blocked the following
/// `update` would just reinstate the per-story git-amend loop. Dirt the board does not own still refuses.
#[test]
fn a_workspace_mutation_tolerates_the_boards_own_uncommitted_documents() {
    let workspace = fixture("workspace-board-dirt");
    let member = workspace.join("members/repo");
    fs::create_dir_all(member.join("docs/stories")).unwrap();
    fs::create_dir_all(workspace.join(".flux")).unwrap();
    fs::write(
        workspace.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"workspace\"\ndefault = true\nactive_milestone = \"current\"\n\n[[members]]\nid = \"repo\"\nroot = \"members/repo\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\n",
    )
    .unwrap();
    fs::write(member.join("docs/stories/README.md"), "# Board\n").unwrap();
    assert!(git(&member, &["init", "-q"]).status.success());
    assert!(
        git(&member, &["config", "user.email", "board@example.test"])
            .status
            .success()
    );
    assert!(git(&member, &["config", "user.name", "Flux Board Test"])
        .status
        .success());
    assert!(git(&member, &["add", "."]).status.success());
    assert!(git(&member, &["commit", "-qm", "fixture"]).status.success());

    let created = board_json(
        &workspace,
        &[
            "board",
            "--board",
            "repo",
            "create",
            "--kind",
            "story",
            "--title",
            "Authored in a member",
            "--output",
            "json",
        ],
    );
    let id = created["data"]["id"].as_str().unwrap().to_string();

    // The member checkout is now dirty with exactly the document the board just wrote.
    let updated = flux(
        &workspace,
        &[
            "board",
            "--board",
            "repo",
            "update",
            &id,
            "--priority",
            "4",
            "--output",
            "json",
        ],
    );
    assert!(
        updated.status.success(),
        "the board's own uncommitted document must not refuse the next mutation: {}",
        String::from_utf8_lossy(&updated.stdout)
    );

    // Dirt the board does not own still refuses.
    fs::write(member.join("Cargo.toml"), "# another session\n").unwrap();
    let refused = flux(
        &workspace,
        &[
            "board",
            "--board",
            "repo",
            "update",
            &id,
            "--priority",
            "5",
            "--output",
            "json",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(4),
        "foreign dirt still refuses: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    fs::remove_dir_all(workspace).ok();
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
        workspace.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"product\"\ndefault = true\nactive_milestone = \"current\"\n\n[[members]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"product-api\"\ncanonical_ref = \"HEAD\"\n\n[[members]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"product-web\"\ncanonical_ref = \"HEAD\"\n\n[[program]]\nid = \"web-client\"\nitem = \"web/C-1\"\nmilestone = \"current\"\norder = 1\ndepends_on = [\"api/C-1\"]\n",
    )
    .unwrap();
    let items = flux(&workspace, &["board", "items", "--output", "json"]);
    assert!(
        items.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&items.stdout),
        String::from_utf8_lossy(&items.stderr)
    );
    let items: serde_json::Value = serde_json::from_slice(&items.stdout).unwrap();
    assert_eq!(items["data"]["items"][0]["id"], "api/C-1");
    assert_eq!(items["data"]["items"][1]["id"], "web/C-1");
    let next = flux(&workspace, &["board", "next", "--output", "json"]);
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["data"]["items"][0]["id"], "web/C-1");

    let checked = flux(
        &workspace,
        &["board", "--scope", "workspace", "check", "--output", "json"],
    );
    assert!(
        checked.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    let checked: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(checked["data"]["valid"], true);
    assert_eq!(checked["data"]["stories"], 2);
    assert_eq!(checked["data"]["members"]["api"]["stories"], 1);
    assert_eq!(checked["data"]["members"]["web"]["stories"], 1);

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
fn native_workspace_board_owns_program_while_fleet_config_and_state_stay_separate() {
    let workspace = fixture("native-program");
    let api = workspace.join("members/api");
    let web = workspace.join("members/web");
    fs::create_dir_all(api.join("docs/stories")).unwrap();
    fs::create_dir_all(web.join("docs/stories")).unwrap();
    fs::create_dir_all(workspace.join("decisions")).unwrap();
    fs::create_dir_all(workspace.join(".flux")).unwrap();
    fs::write(
        api.join("docs/stories/C-1-api.md"),
        "---\nid: C-1\ntitle: API\nstatus: ready\npriority: 99\n---\n\n# API\n",
    )
    .unwrap();
    fs::write(
        api.join("docs/stories/C-2-unscheduled.md"),
        "---\nid: C-2\ntitle: Unscheduled\nstatus: ready\npriority: 1\n---\n\n# Unscheduled\n",
    )
    .unwrap();
    fs::write(
        web.join("docs/stories/C-1-web.md"),
        "---\nid: C-1\ntitle: Web\nstatus: ready\npriority: 1\n---\n\n# Web\n",
    )
    .unwrap();
    fs::write(
        workspace.join("decisions/0001-accepted.md"),
        "# Accepted architecture\n\n**Status:** accepted\n",
    )
    .unwrap();
    let board_config = "schema = \"flux.board-workspace/v1\"\nid = \"program\"\ndefault = true\nactive_milestone = \"m1\"\ndecisions = \"decisions\"\n\n[[members]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"api\"\ncanonical_ref = \"HEAD\"\n\n[[members]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"web\"\ncanonical_ref = \"HEAD\"\n\n[[program]]\nid = \"web-first\"\nitem = \"web/C-1\"\nmilestone = \"m1\"\norder = 1\n\n[[program]]\nid = \"api-second\"\nitem = \"api/C-1\"\nmilestone = \"m1\"\norder = 2\n\n[[waves]]\nid = \"web-wave\"\nstate = \"active\"\nrepository = \"web\"\nitems = [\"web/C-1\"]\n\n[[waves]]\nid = \"api-wave\"\nstate = \"active\"\nrepository = \"api\"\nitems = [\"api/C-1\"]\n";
    let fleet_config = "schema = \"flux.fleet/v1\"\nmax_workers = 2\nmax_wave = 10\nmax_rework = 2\n\n[[repositories]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"api\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n\n[[repositories]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"web\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n";
    fs::write(workspace.join(".flux/board.toml"), board_config).unwrap();
    fs::write(workspace.join(".flux/fleet.toml"), fleet_config).unwrap();

    let next = flux(
        &workspace,
        &["board", "next", "--limit", "10", "--output", "json"],
    );
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stdout)
    );
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["data"]["items"][0]["id"], "web/C-1");
    assert_eq!(next["data"]["items"][1]["id"], "api/C-1");
    assert_eq!(next["data"]["items"].as_array().unwrap().len(), 2);

    let schedule = flux(&workspace, &["fleet", "schedule", "--output", "json"]);
    assert!(
        schedule.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&schedule.stdout),
        String::from_utf8_lossy(&schedule.stderr)
    );
    let schedule: serde_json::Value = serde_json::from_slice(&schedule.stdout).unwrap();
    assert_eq!(schedule["data"]["active_milestone"], "m1");
    assert_eq!(
        schedule["data"]["program_items"].as_array().unwrap().len(),
        2
    );
    assert!(schedule["data"].get("active_tranche").is_none());
    assert!(schedule["data"].get("tranches").is_none());
    assert_eq!(schedule["data"]["attention_required"], false);

    let stats = flux(&workspace, &["board", "stats", "--output", "json"]);
    assert!(
        stats.status.success(),
        "{}",
        String::from_utf8_lossy(&stats.stdout)
    );
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["data"]["milestone_lanes"]["total"], 2);
    assert!(stats["data"].get("tranche_lanes").is_none());

    let started = flux(&workspace, &["fleet", "start", "--output", "json"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stdout)
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".flux/board.toml")).unwrap(),
        board_config
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".flux/fleet.toml")).unwrap(),
        fleet_config
    );
    assert!(workspace.join(".flux/fleet/state.json").is_file());
    let runtime_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join(".flux/fleet/state.json")).unwrap(),
    )
    .unwrap();
    assert!(runtime_state.get("max_workers").is_none());
    assert!(runtime_state.get("max_wave").is_none());
    assert!(runtime_state.get("max_rework").is_none());
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
        workspace.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"workspace\"\ndefault = true\nactive_milestone = \"current\"\n\n[[members]]\nid = \"api\"\nroot = \"members/api\"\nboard = \"shared\"\ncanonical_ref = \"HEAD\"\n\n[[members]]\nid = \"web\"\nroot = \"members/web\"\nboard = \"shared\"\ncanonical_ref = \"HEAD\"\n\n[[program]]\nid = \"api\"\nitem = \"api/C-1\"\nmilestone = \"current\"\norder = 1\n\n[[program]]\nid = \"web\"\nitem = \"web/C-1\"\nmilestone = \"current\"\norder = 2\n",
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
        workspace.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"workspace\"\ndefault = true\nactive_milestone = \"current\"\n\n[[members]]\nid = \"absent\"\nroot = \"members/absent\"\nboard = \"missing\"\ncanonical_ref = \"HEAD\"\n",
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
    install_test_fleet_loops(&root);
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
        format!("schema = \"flux.fleet/v1\"\nmax_workers = 3\nmax_wave = 10\nmax_rework = 2\nallow_ad_hoc_agents = true\n\n[main]\ninstructions = \".flux/fleet/main.md\"\n{TEST_FLEET_LOOP_POLICY}\n[[agent_templates]]\nid = \"scout\"\nrole = \"researcher\"\ntask_kind = \"research\"\ninstructions = \".flux/fleet/agents/scout.md\"\nmode = \"read-only\"\ncapabilities = [\"read\"]\nmax_instances = 1\n"),
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
fn fleet_rejects_a_template_with_exact_missing_capability_names_before_launch() {
    let root = fixture("missing-worker-capabilities");
    install_test_fleet_loops(&root);
    fs::create_dir_all(root.join(".flux/fleet/agents")).unwrap();
    fs::write(
        root.join(".flux/fleet/agents/story-worker.md"),
        "Implement only the assigned story.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\n{TEST_FLEET_LOOP_POLICY}\n[[agent_templates]]\nid = \"story-worker\"\nrole = \"writer\"\ntask_kind = \"implementation\"\ninstructions = \".flux/fleet/agents/story-worker.md\"\nmode = \"write\"\ncapabilities = [\"read\", \"edit\"]\nmax_instances = 1\n"),
    )
    .unwrap();

    let rejected = flux(&root, &["fleet", "validate", "--output", "json"]);
    assert_eq!(rejected.status.code(), Some(7));
    let rejected: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(rejected["error"]["class"], "validation/gate");
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("missing required capabilities: git"));
    fs::remove_dir_all(root).ok();
}

#[test]
fn fleet_dispatch_creates_a_pinned_wave_and_inheriting_story_worktrees() {
    let root = fixture("wave-topology");
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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
    assert_eq!(dispatched["data"]["agents"].as_array().unwrap().len(), 1);
    assert_eq!(dispatched["data"]["agents"][0]["id"], "wave-2-worker-1");
    assert_eq!(dispatched["data"]["agents"][0]["role"], "writer");
    assert_eq!(
        dispatched["data"]["agents"][0]["task_kind"],
        "implementation"
    );
    assert_eq!(
        dispatched["data"]["agents"][0]["loop_binding"]["profile"],
        "implementation"
    );
    assert_eq!(
        dispatched["data"]["agents"][0]["loop_binding"]["source_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let repository = &dispatched["data"]["topology"]["repositories"][0];
    assert_eq!(repository["base_commit"], base);
    assert_eq!(repository["stories"][0]["base_commit"], base);

    let census = flux(&root, &["fleet", "agents", "--output", "json"]);
    assert!(census.status.success());
    let census: serde_json::Value = serde_json::from_slice(&census.stdout).unwrap();
    assert_eq!(census["data"]["schema"], "flux.fleet-agents/v1");
    assert_eq!(census["data"]["workers_total"], 1);
    assert_eq!(
        census["data"]["workers"]["wave-2-worker-1"]["id"],
        "wave-2-worker-1"
    );
    assert_eq!(
        census["data"]["workers"]["wave-2-worker-1"]["task_kind"],
        "implementation"
    );
    assert!(census["data"]["workers"]["wave-2-worker-1"]
        .get("instructions")
        .is_none());
    assert!(census["data"]["workers"]["wave-2-worker-1"]
        .get("last_turn")
        .is_none());

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
    assert_eq!(repository["stories"][0]["wave"], "wave-2");
}

/// Failing first: `fleet repair` rebuilds the structure a wave's topology names and disk lacks.
///
/// Reclamation removed an integration worktree an unfinished wave still needed, and putting it back
/// took a hand-written `git worktree add` against a base read out of `state.json` — the same class of
/// hand repair as the `git reset --hard <base>` an integration worktree needed before handoffs would
/// verify. Both facts are recorded; neither had a verb.
///
/// The three assertions below are the whole contract. A missing checkout returns **on its own
/// branch**, so a worker's delivered commit survives the repair. A worktree holding an uncommitted
/// change is refused rather than reset, because the only place that change exists is the one this
/// verb would overwrite. And once nothing would be discarded, a derived worktree goes back to the
/// base it is pinned to, leaving every story commit alone.
#[test]
fn fleet_repair_rebuilds_missing_structure_without_discarding_work() {
    fn head_of(worktree: &PathBuf) -> String {
        String::from_utf8(git(worktree, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }
    fn entry_for(repaired: &serde_json::Value, worktree: &Path) -> serde_json::Value {
        repaired["data"]["worktrees"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|entry| entry["worktree"] == worktree.display().to_string())
            .cloned()
            .unwrap_or_else(|| panic!("{} is missing from {repaired}", worktree.display()))
    }

    let (root, story) = one_story_wave("wave-repair");
    let base = head_of(&root);
    let delivered = commit_result(&story, "delivered");
    let integration = story
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("integration");
    assert!(integration.is_dir());

    fs::remove_dir_all(&story).unwrap();
    let repaired = flux(&root, &["fleet", "repair", "wave-2", "--output", "json"]);
    assert!(
        repaired.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&repaired.stdout),
        String::from_utf8_lossy(&repaired.stderr)
    );
    let repaired: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(repaired["data"]["wave"], "wave-2");
    assert_eq!(
        entry_for(&repaired, &story)["repair"]["action"],
        "recreated"
    );
    assert!(story.is_dir(), "the checkout the topology names is back");
    assert_eq!(
        head_of(&story),
        delivered,
        "rebuilt on its recorded branch, so the delivered commit came back with it"
    );

    assert!(git(
        &integration,
        &["commit", "--allow-empty", "-qm", "assembly"]
    )
    .status
    .success());
    let assembled = head_of(&integration);
    fs::write(integration.join("scratch.txt"), "unsaved\n").unwrap();
    let refused = flux(&root, &["fleet", "repair", "wave-2", "--output", "json"]);
    assert!(refused.status.success());
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(
        entry_for(&refused, &integration)["repair"]["action"],
        "refused"
    );
    assert_eq!(
        head_of(&integration),
        assembled,
        "a refusal leaves the worktree exactly as it was"
    );
    assert!(integration.join("scratch.txt").is_file());

    fs::remove_file(integration.join("scratch.txt")).unwrap();
    let reset = flux(&root, &["fleet", "repair", "wave-2", "--output", "json"]);
    assert!(reset.status.success());
    let reset: serde_json::Value = serde_json::from_slice(&reset.stdout).unwrap();
    assert_eq!(entry_for(&reset, &integration)["repair"]["action"], "reset");
    assert_eq!(head_of(&integration), base, "back on its pinned base");
    assert_eq!(
        head_of(&story),
        delivered,
        "repairing the assembly never touches a story's commits"
    );
}

/// C-722, end to end through the binary: an interrupted worker's uncommitted work is reported, it
/// survives the sweep, and the command `doctor` prints is the command that saves it.
///
/// wave-745 died overnight with a 531-line failing-first specification untracked in its story
/// worktree, and every mechanism the fleet had agreed the work did not exist — the write set comes
/// from `base..HEAD`, the branch was still at its pinned base, and the fix `doctor` prescribed was
/// `reclaim`, which is documented to delete worktrees that provably hold no work. The only recovery
/// was hand-running `git` inside a directory the fleet owns.
///
/// So the last step here runs the finding's OWN `fix` string verbatim rather than a command this
/// test composed. If the prescription and the verb ever drift apart, the operator is back to
/// improvising with `git` under time pressure, which is the defect and not a detail of it.
#[test]
fn fleet_reports_and_captures_work_a_story_worker_never_committed() {
    let (root, story) = one_story_wave("wave-capture");
    let specification = story.join("crates/flux-runtime/tests/resource_receipts.rs");
    fs::create_dir_all(specification.parent().unwrap()).unwrap();
    fs::write(
        &specification,
        "// the turn ended before this was committed\n",
    )
    .unwrap();
    // Cancelled is the wave whose worktrees reclamation may actually remove, which is exactly the
    // state an operator puts a dead wave into before sweeping it.
    assert!(flux(&root, &["fleet", "cancel", "wave-2"]).status.success());

    let doctored = flux(&root, &["fleet", "doctor", "--output", "json"]);
    assert!(
        doctored.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&doctored.stdout),
        String::from_utf8_lossy(&doctored.stderr)
    );
    let doctored: serde_json::Value = serde_json::from_slice(&doctored.stdout).unwrap();
    let findings = doctored["data"]["runtime"]["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let dirty = findings
        .iter()
        .find(|finding| finding["check"] == "story-worktree-holds-uncommitted-work")
        .unwrap_or_else(|| panic!("the uncommitted specification must be reported: {doctored}"));
    assert_eq!(dirty["subject"], story.display().to_string());
    let detail = dirty["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("wave-2")
            && detail.contains("repo/C-1")
            && detail.contains("1 uncommitted"),
        "the finding names the wave, the story and the count: {detail}"
    );
    assert!(
        findings.iter().all(|finding| !finding["fix"]
            .as_str()
            .unwrap_or_default()
            .contains("reclaim")),
        "nothing may prescribe the sweep while the only copy of the work is here: {doctored}"
    );

    let reclaimed = flux(&root, &["fleet", "reclaim", "--output", "json"]);
    assert!(reclaimed.status.success());
    assert!(
        specification.is_file(),
        "the sweep must not remove the checkout holding the only copy: {}",
        String::from_utf8_lossy(&reclaimed.stdout)
    );

    // The operator runs exactly what the finding told them to run.
    let fix = dirty["fix"].as_str().unwrap().to_string();
    let mut argv = fix.split_whitespace().collect::<Vec<_>>();
    assert_eq!(argv.remove(0), "flux", "the fix is a flux command: {fix}");
    argv.extend(["--output", "json"]);
    let captured = flux(&root, &argv);
    assert!(
        captured.status.success(),
        "the prescribed command must work: {fix}\nstdout={} stderr={}",
        String::from_utf8_lossy(&captured.stdout),
        String::from_utf8_lossy(&captured.stderr)
    );
    let captured: serde_json::Value = serde_json::from_slice(&captured.stdout).unwrap();
    let capture = &captured["data"]["stories"][0]["capture"];
    assert_eq!(capture["action"], "captured", "{captured}");
    assert_eq!(capture["still_uncommitted"], 0, "verified, not assumed");

    // On the story's OWN branch, so the work is reachable without the worktree.
    let branch = String::from_utf8(git(&story, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(capture["branch"], branch);
    let listed =
        String::from_utf8(git(&root, &["ls-tree", "-r", "--name-only", &branch, "crates/"]).stdout)
            .unwrap();
    assert!(
        listed.contains("crates/flux-runtime/tests/resource_receipts.rs"),
        "the specification is committed on the story branch: {listed}"
    );

    let healthy = flux(&root, &["fleet", "doctor", "--output", "json"]);
    let healthy: serde_json::Value = serde_json::from_slice(&healthy.stdout).unwrap();
    assert!(
        healthy["data"]["runtime"]["findings"]
            .as_array()
            .is_some_and(|findings| findings
                .iter()
                .all(|finding| finding["check"] != "story-worktree-holds-uncommitted-work")),
        "once the work is on its branch there is nothing left to report: {healthy}"
    );
}

#[test]
fn fleet_verifies_handoff_runs_one_final_gate_and_applies_only_explicitly() {
    let root = fixture("wave-integration");
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\nfences = [\".flux/fleet/**\"]\n"),
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

    record_passing_review(&root, "wave-2", "repo/C-1", &commit);
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
    // C-619: apply ACCEPTS the candidate, it does not merge it.
    //
    // The old contract merged in the repository's source checkout, which Fleet keeps DETACHED at the
    // pinned base — so the merge commit landed on no branch, `main` never moved, and the only thing
    // reachable from it was that worktree's HEAD. Acceptance now pins the candidate with an annotated
    // tag that outlives the wave, the integration branch and worktree reclamation; `main` is written
    // exactly once, later, by the gated accumulation snapshot.
    assert_eq!(applied["data"]["accepted"], true);
    assert_eq!(applied["data"]["merged_locally"], false);
    assert_eq!(applied["data"]["pushed"], false);
    assert_eq!(applied["data"]["released"], false);
    assert_eq!(applied["data"]["deployed"], false);
    assert!(
        !root.join("result.txt").exists(),
        "apply must not merge into the source checkout"
    );
    // What acceptance must guarantee is that the work cannot be lost: the tag resolves, and the story's
    // content is reachable through it even though no branch points at the candidate.
    let tag = "fleet/accepted/wave-2/repo";
    let shown = git(&root, &["show", &format!("{tag}:result.txt")]);
    assert!(
        shown.status.success(),
        "accepted tag {tag} must resolve: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&shown.stdout), "implemented\n");
}

#[test]
fn fleet_combined_only_failure_runs_the_final_gate_once_and_preserves_candidate() {
    let root = fixture("combined-only-red");
    install_test_fleet_loops(&root);
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
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"sh\", \"-c\", \"test ! -f one.txt || test ! -f two.txt\"]\n"),
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
        record_passing_review(&root, "wave-2", &item, &commit);
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

    // Failing first: a wave that failed integration can be RETRIED once the cause is fixed, but a retry
    // that recomputes the identical candidate must not spend a second gate run.
    //
    // `conflict` and `red` used to be terminal, so a wave that failed kept a memory of failing and no
    // later `fleet integrate` would touch it. That made fixing the cause pointless: three delivered
    // stories stayed unreachable after the defect that stranded them was fixed and installed, refused
    // with "not ready for integration". The guard worth keeping belongs to the candidate, not the wave.
    let reretry = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        !reretry.status.success(),
        "an unchanged candidate must not be re-gated"
    );
    let reretry: serde_json::Value = serde_json::from_slice(&reretry.stdout).unwrap();
    assert_eq!(reretry["error"]["class"], "validation/gate");
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
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let gate = &inspected["data"]["data"]["repositories"][0]["gate"];
    assert_eq!(
        gate["runs"], 1,
        "still exactly one gate run for this candidate: {gate}"
    );
    assert!(
        gate["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("unchanged"),
        "and the refusal says why rather than repeating the old verdict: {gate}"
    );

    // Failing first: a NAMED apply is judged per repository, not by the wave rollup.
    //
    // Integration assembles and gates one candidate per repository, so a wave can hold a green candidate
    // beside a conflicted one — and apply demanding the whole wave be green stranded that green candidate
    // behind a collision it had no part in. The two refusals must therefore be distinguishable: the
    // whole-wave apply is refused by the rollup, while `--only` reaches the per-repository gate check and
    // is refused by THAT. Here the one repository is genuinely red, so both refuse — but for different,
    // observable reasons, which is exactly the contract change.
    let whole = flux(&root, &["fleet", "apply", "wave-2", "--output", "json"]);
    assert!(!whole.status.success());
    let whole: serde_json::Value = serde_json::from_slice(&whole.stdout).unwrap();
    assert!(
        whole["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("has no recorded green final gate"),
        "whole-wave apply is refused by the rollup: {}",
        whole["error"]["message"]
    );

    let named = flux(
        &root,
        &[
            "fleet", "apply", "wave-2", "--only", "repo", "--output", "json",
        ],
    );
    assert!(!named.status.success());
    let named: serde_json::Value = serde_json::from_slice(&named.stdout).unwrap();
    assert!(
        named["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("lacks exactly one recorded green final gate"),
        "a named apply must reach the per-repository gate check: {}",
        named["error"]["message"]
    );

    // Naming a repository the wave does not contain is a not-found, not a silent no-op that reports
    // success for accepting nothing.
    let absent = flux(
        &root,
        &[
            "fleet", "apply", "wave-2", "--only", "absent", "--output", "json",
        ],
    );
    assert!(!absent.status.success());
    let absent: serde_json::Value = serde_json::from_slice(&absent.stdout).unwrap();
    assert_eq!(absent["error"]["class"], "not-found");

    fs::remove_dir_all(root).ok();
}

/// A member checkout with one committed story, on a local `main`.
fn promote_member(root: &Path, id: &str, stories: &[(&str, i64)]) -> PathBuf {
    let member = root.join("members").join(id);
    fs::create_dir_all(member.join("docs/stories")).unwrap();
    fs::write(member.join("README.md"), format!("# {id}\n")).unwrap();
    for (story, priority) in stories {
        fs::write(
            member.join(format!("docs/stories/{story}-story.md")),
            format!(
                "---\nid: {story}\ntitle: Story {story}\nstatus: ready\npriority: {priority}\n---\n\n# Story {story}\n\n## Acceptance\n\n- [ ] ship\n"
            ),
        )
        .unwrap();
    }
    let member = member.to_path_buf();
    assert!(git(&member, &["init", "-q", "-b", "main"]).status.success());
    assert!(
        git(&member, &["config", "user.email", "fleet@example.test"])
            .status
            .success()
    );
    assert!(git(&member, &["config", "user.name", "Flux Fleet Test"])
        .status
        .success());
    assert!(git(&member, &["add", "."]).status.success());
    assert!(git(&member, &["commit", "-qm", "fixture"]).status.success());
    member
}

fn head_of(repository: &PathBuf, reference: &str) -> String {
    let resolved = git(repository, &["rev-parse", reference]);
    assert!(
        resolved.status.success(),
        "{reference} must resolve: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    String::from_utf8(resolved.stdout)
        .unwrap()
        .trim()
        .to_string()
}

/// Dispatch one wave holding `items`, returning its id and the worktree of each story.
fn promote_dispatch(root: &PathBuf, items: &[&str]) -> (String, Vec<PathBuf>) {
    let mut args = vec!["fleet", "run"];
    args.extend_from_slice(items);
    args.extend_from_slice(&["--prepare-only", "--output", "json"]);
    let dispatched = flux(root, &args);
    assert!(
        dispatched.status.success(),
        "dispatch {items:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&dispatched.stdout),
        String::from_utf8_lossy(&dispatched.stderr)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let wave = dispatched["data"]["wave"].as_str().unwrap().to_string();
    let worktrees = items
        .iter()
        .map(|item| {
            let found = dispatched["data"]["topology"]["repositories"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|repository| repository["stories"].as_array().unwrap())
                .find(|story| story["board_ref"].as_str() == Some(item))
                .unwrap_or_else(|| panic!("{item} must be in the dispatched topology"));
            PathBuf::from(found["worktree"].as_str().unwrap())
        })
        .collect();
    (wave, worktrees)
}

/// Commit `contents` in a story worktree and hand it off as that story's delivered work.
fn promote_deliver(
    root: &PathBuf,
    wave: &str,
    item: &str,
    worktree: &PathBuf,
    file: &str,
    contents: &str,
) {
    fs::write(worktree.join(file), contents).unwrap();
    assert!(git(worktree, &["add", file]).status.success());
    assert!(git(worktree, &["commit", "-qm", &format!("write {file}")])
        .status
        .success());
    let commit = head_of(worktree, "HEAD");
    let handoff = flux(
        root,
        &[
            "fleet",
            "handoff",
            wave,
            item,
            "--commit",
            &commit,
            "--write-set",
            file,
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            file,
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
        "handoff {item}: stdout={} stderr={}",
        String::from_utf8_lossy(&handoff.stdout),
        String::from_utf8_lossy(&handoff.stderr)
    );
    // C-587 landed alongside this story and gates integration on a review by an agent that is not
    // the writer, so a delivered candidate is no longer integrable on its author's word alone.
    // These fixtures exist to exercise promotion, not to re-test the review refusal, so they record
    // the PASS a dispatched reviewer would have written — through the same parser and the same
    // closed vocabularies.
    record_passing_review(root, wave, item, &commit);
}

/// Gate the wave and accept every green candidate, leaving one annotated tag per member.
fn promote_accept(root: &PathBuf, wave: &str) {
    let integrated = flux(root, &["fleet", "integrate", wave, "--output", "json"]);
    assert!(
        integrated.status.success(),
        "integrate {wave}: stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let applied = flux(root, &["fleet", "apply", wave, "--output", "json"]);
    assert!(
        applied.status.success(),
        "apply {wave}: stdout={} stderr={}",
        String::from_utf8_lossy(&applied.stdout),
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    // The state this story exists to resolve: accepted, pinned, and NOT on any canonical branch.
    assert_eq!(applied["data"]["merged_locally"], false);
    assert_eq!(applied["data"]["delivered"], false);
}

fn promote_fleet_config(threshold: u64) -> String {
    format!(
        "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n\
         [promote]\nthreshold = {threshold}\n\n\
         [[repositories]]\nid = \"client\"\nroot = \"members/client\"\nboard = \"default\"\ncanonical_ref = \"main\"\ndepends_on = [\"engine\"]\ngate = [\"git\", \"status\", \"--short\"]\n\n\
         [[repositories]]\nid = \"engine\"\nroot = \"members/engine\"\nboard = \"default\"\ncanonical_ref = \"main\"\ngate = [\"git\", \"status\", \"--short\"]\n\n\
         [[repositories]]\nid = \"mirror\"\nroot = \"members/mirror\"\nboard = \"default\"\ncanonical_ref = \"origin/main\"\ngate = [\"git\", \"status\", \"--short\"]\n"
    )
}

/// Failing first: `flux fleet promote` lands accepted work on every member's LOCAL canonical branch.
///
/// This is the last mile, and until now nothing in the product walked it. `fleet apply` accepts a
/// green candidate, pins it with an annotated tag and reports `merged_locally: false`; C-619 removed
/// the local merge it used to attempt because that merge landed on a detached worktree HEAD and never
/// reached a branch at all. What actually advanced `main` was `snapshot_and_merge()` in one operator's
/// `autopilot.sh` — called once, hardcoded to a single member, with that machine's absolute path as
/// its argument. In any other deployment the release train silently never ran, and in that one the
/// other two members were simply never promoted.
///
/// Four properties are proved here, and each is a defect if it regresses.
///
///  * **The order comes from the declared graph, not from the file or the alphabet.** `client` is
///    declared first and also sorts first, and it declares `depends_on = ["engine"]` — so decision
///    0005's `engine → client` order is only produced by reading the graph. Both orders a naive
///    implementation falls into are wrong here.
///  * **The threshold is configuration.** With `[promote] threshold = 2` one accepted candidate per
///    member is withheld and every canonical ref is untouched; the identical command lands the
///    identical state once the configuration says one is enough.
///  * **A member whose canonical ref is remote-tracking is refused by name.** Only a push can move
///    `origin/main`, and the fleet never pushes, so reporting anything but a refusal would be the
///    over-claim C-721 was written to close.
///  * **Landing is verified by re-reading the ref**, not by trusting the merge's exit code.
#[test]
fn fleet_promote_lands_every_member_on_its_local_canonical_branch_in_dependency_order() {
    let root = fixture("fleet-promote-order");
    install_test_fleet_loops(&root);
    let client = promote_member(&root, "client", &[("C-2", 2)]);
    let engine = promote_member(&root, "engine", &[("C-1", 1)]);
    let mirror = promote_member(&root, "mirror", &[]);
    // A remote-tracking ref that resolves without a network: `origin/main` exists, and nothing but a
    // push could ever move it.
    let mirror_head = head_of(&mirror, "HEAD");
    assert!(git(
        &mirror,
        &["update-ref", "refs/remotes/origin/main", &mirror_head]
    )
    .status
    .success());

    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"product\"\ndefault = true\nactive_milestone = \"current\"\n\n\
         [[members]]\nid = \"client\"\nroot = \"members/client\"\nboard = \"default\"\ncanonical_ref = \"main\"\n\n\
         [[members]]\nid = \"engine\"\nroot = \"members/engine\"\nboard = \"default\"\ncanonical_ref = \"main\"\n",
    )
    .unwrap();
    fs::write(root.join(".flux/fleet.toml"), promote_fleet_config(2)).unwrap();
    let started = flux(&root, &["fleet", "start"]);
    assert!(
        started.status.success(),
        "the promotion threshold is configuration the fleet must accept: stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );

    let engine_base = head_of(&engine, "main");
    let client_base = head_of(&client, "main");

    let (wave, worktrees) = promote_dispatch(&root, &["engine/C-1", "client/C-2"]);
    promote_deliver(
        &root,
        &wave,
        "engine/C-1",
        &worktrees[0],
        "engine.txt",
        "engine landed\n",
    );
    promote_deliver(
        &root,
        &wave,
        "client/C-2",
        &worktrees[1],
        "client.txt",
        "client landed\n",
    );
    promote_accept(&root, &wave);

    // A dry run reports the exact merges it would make and writes nothing.
    let preview = flux(
        &root,
        &["fleet", "promote", "--dry-run", "--output", "json"],
    );
    assert!(
        preview.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&preview.stdout),
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    let previewed = preview["data"]["members"].as_array().unwrap();
    assert_eq!(
        previewed
            .iter()
            .map(|member| member["member"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["engine", "client", "mirror"],
        "the preview walks the declared dependency graph: {}",
        preview["data"]
    );
    assert_eq!(
        head_of(&engine, "main"),
        engine_base,
        "a preview writes nothing"
    );

    // The threshold is configuration, and below it nothing is landed.
    let withheld = flux(&root, &["fleet", "promote", "--output", "json"]);
    assert!(
        withheld.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&withheld.stdout),
        String::from_utf8_lossy(&withheld.stderr)
    );
    let withheld: serde_json::Value = serde_json::from_slice(&withheld.stdout).unwrap();
    assert_eq!(withheld["data"]["threshold"], 2);
    let engine_withheld = withheld["data"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["member"] == "engine")
        .unwrap()
        .clone();
    assert_eq!(engine_withheld["status"], "withheld", "{engine_withheld}");
    assert!(
        engine_withheld["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("1/2"),
        "a withheld member says how far it is from its threshold: {engine_withheld}"
    );
    assert_eq!(head_of(&engine, "main"), engine_base);
    assert_eq!(head_of(&client, "main"), client_base);

    // The same state, the same command, one configuration value changed.
    fs::write(root.join(".flux/fleet.toml"), promote_fleet_config(1)).unwrap();
    let promoted = flux(&root, &["fleet", "promote", "--output", "json"]);
    assert!(
        promoted.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&promoted.stdout),
        String::from_utf8_lossy(&promoted.stderr)
    );
    let promoted: serde_json::Value = serde_json::from_slice(&promoted.stdout).unwrap();
    assert_eq!(
        promoted["data"]["promoted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["engine", "client"],
        "the upstream member lands first: {}",
        promoted["data"]
    );
    assert_eq!(promoted["data"]["pushed"], false);
    assert_eq!(promoted["data"]["released"], false);
    assert_eq!(promoted["data"]["deployed"], false);

    // Landing is a git fact, re-read from the member's own checkout.
    assert_ne!(
        head_of(&engine, "main"),
        engine_base,
        "engine main advanced"
    );
    assert_ne!(
        head_of(&client, "main"),
        client_base,
        "client main advanced"
    );
    let engine_file = git(&engine, &["show", "main:engine.txt"]);
    assert!(
        engine_file.status.success(),
        "the accepted work is on the member's local main: {}",
        String::from_utf8_lossy(&engine_file.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&engine_file.stdout),
        "engine landed\n"
    );
    assert!(git(&client, &["show", "main:client.txt"]).status.success());

    // A member whose canonical ref only a push could move is refused BY NAME, and the refusal names
    // the ref rather than reporting an unlanded member as done.
    let refused = promoted["data"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["member"] == "mirror")
        .unwrap()
        .clone();
    assert_eq!(refused["status"], "refused", "{refused}");
    assert!(
        refused["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("origin/main"),
        "the refusal names the ref: {refused}"
    );

    // C-721's check must now agree: the wave's canonical refs contain its accepted candidates, so the
    // wave is `applied` rather than `awaiting-delivery`.
    let status = flux(
        &root,
        &["fleet", "inspect", "wave", &wave, "--output", "json"],
    );
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        status["data"]["data"]["status"], "applied",
        "promotion resolves the delivery question it just answered: {}",
        status["data"]["data"]
    );
    assert_eq!(
        status["data"]["data"]["delivery"]["delivered"], true,
        "and records the containment it re-read, not the fact that it merged"
    );

    fs::remove_dir_all(root).ok();
}

/// Failing first: one candidate that will not combine must not cost the others their promotion.
///
/// Two waves accepted from the same base, each rewriting one file, cannot both merge. The bash
/// snapshot got this right and it is the property most easily lost: forcing the conflict would land
/// an ungated tree, and abandoning the whole accumulation would strand delivered work behind a
/// collision it had no part in. So the conflicting candidate is left out, reported by name, and the
/// rest still land.
#[test]
fn fleet_promote_excludes_a_conflicting_candidate_and_lands_the_rest() {
    let root = fixture("fleet-promote-conflict");
    install_test_fleet_loops(&root);
    let engine = promote_member(&root, "engine", &[("C-1", 1), ("C-2", 2)]);
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/board.toml"),
        "schema = \"flux.board-workspace/v1\"\nid = \"product\"\ndefault = true\nactive_milestone = \"current\"\n\n\
         [[members]]\nid = \"engine\"\nroot = \"members/engine\"\nboard = \"default\"\ncanonical_ref = \"main\"\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!(
            "schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n\
             [[repositories]]\nid = \"engine\"\nroot = \"members/engine\"\nboard = \"default\"\ncanonical_ref = \"main\"\ngate = [\"git\", \"status\", \"--short\"]\n"
        ),
    )
    .unwrap();
    assert!(flux(&root, &["fleet", "start"]).status.success());
    let base = head_of(&engine, "main");

    let (first, worktrees) = promote_dispatch(&root, &["engine/C-1"]);
    promote_deliver(
        &root,
        &first,
        "engine/C-1",
        &worktrees[0],
        "shared.txt",
        "from the first wave\n",
    );
    promote_accept(&root, &first);

    // Accepting does not move `main`, so the second wave is assembled from the same base and its
    // candidate rewrites the same file.
    assert_eq!(head_of(&engine, "main"), base);
    let (second, worktrees) = promote_dispatch(&root, &["engine/C-2"]);
    promote_deliver(
        &root,
        &second,
        "engine/C-2",
        &worktrees[0],
        "shared.txt",
        "from the second wave\n",
    );
    promote_accept(&root, &second);

    let promoted = flux(&root, &["fleet", "promote", "--output", "json"]);
    assert!(
        promoted.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&promoted.stdout),
        String::from_utf8_lossy(&promoted.stderr)
    );
    let promoted: serde_json::Value = serde_json::from_slice(&promoted.stdout).unwrap();
    let member = promoted["data"]["members"][0].clone();
    assert_eq!(member["status"], "promoted", "{member}");
    let excluded = member["excluded"].as_array().unwrap();
    assert_eq!(
        excluded.len(),
        1,
        "exactly one candidate is left out: {member}"
    );
    assert_eq!(
        excluded[0]["wave"], second,
        "and it is named, so it can be re-integrated rather than lost: {member}"
    );
    assert!(
        excluded[0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("conflict"),
        "{member}"
    );

    // The surviving candidate really landed, and the excluded one did not.
    assert_eq!(
        String::from_utf8_lossy(&git(&engine, &["show", "main:shared.txt"]).stdout),
        "from the first wave\n"
    );
    assert_ne!(head_of(&engine, "main"), base);

    fs::remove_dir_all(root).ok();
}

/// Failing first: concurrent handoffs against one wave all land.
///
/// A handoff writes the wave record and its worker's record, and at width every worker reaches that point
/// at once. The write used to be computed from the snapshot the call started with, so the loser of the
/// compare-and-set lost its whole handoff — evidence, write set and commit — and the wave then looked as
/// though that story had never been delivered. Two lost updates already cost a dispatch and a completed
/// integration with a SINGLE worker running; N workers is the contention this closes.
#[test]
fn concurrent_handoffs_against_one_wave_all_land() {
    let root = fixture("concurrent-handoffs");
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    let ids = ["C-1", "C-2", "C-3", "C-4"];
    for (index, id) in ids.iter().enumerate() {
        fs::write(
            root.join(format!("docs/stories/{id}-story.md")),
            format!(
                "---\nid: {id}\ntitle: Story {index}\nstatus: ready\npriority: {}\n---\n\n# Story {index}\n\n## Acceptance\n\n- [ ] ship\n",
                index + 1
            ),
        )
        .unwrap();
    }
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"true\"]\n"),
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

    let items: Vec<String> = ids.iter().map(|id| format!("repo/{id}")).collect();
    let mut argv: Vec<&str> = vec!["fleet", "run"];
    argv.extend(items.iter().map(String::as_str));
    argv.extend(["--prepare-only", "--output", "json"]);
    let dispatched = flux(&root, &argv);
    assert!(
        dispatched.status.success(),
        "{}",
        String::from_utf8_lossy(&dispatched.stdout)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let stories = dispatched["data"]["topology"]["repositories"][0]["stories"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(stories.len(), 4);

    // Each story commits its own file, so the handoffs are independent in content and simultaneous in time.
    let mut prepared = Vec::new();
    for (index, story) in stories.iter().enumerate() {
        let worktree = PathBuf::from(story["worktree"].as_str().unwrap());
        let name = format!("file-{index}.txt");
        fs::write(worktree.join(&name), "done\n").unwrap();
        assert!(git(&worktree, &["add", &name]).status.success());
        assert!(git(&worktree, &["commit", "-qm", &name]).status.success());
        let commit = String::from_utf8(git(&worktree, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        prepared.push((
            story["board_ref"].as_str().unwrap().to_string(),
            name,
            commit,
        ));
    }

    // Fire them at once. Any one losing its compare-and-set is the defect.
    let handles: Vec<_> = prepared
        .into_iter()
        .map(|(item, name, commit)| {
            let root = root.clone();
            std::thread::spawn(move || {
                let output = flux(
                    &root,
                    &[
                        "fleet",
                        "handoff",
                        "wave-2",
                        &item,
                        "--commit",
                        &commit,
                        "--write-set",
                        &name,
                        "--test-arg",
                        "test",
                        "--test-arg",
                        "-f",
                        "--test-arg",
                        &name,
                        "--failing-before",
                        "--passing-after",
                        "--summary",
                        "one file",
                        "--output",
                        "json",
                    ],
                );
                (
                    item,
                    output.status.success(),
                    String::from_utf8_lossy(&output.stdout).to_string(),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for (item, ok, out) in &results {
        assert!(ok, "{item} lost its handoff: {out}");
    }

    // And every one is recorded, not merely reported: the wave must hold four accepted handoffs.
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
    // The system's own aggregate is the strongest assertion available: a wave only reaches
    // `handoffs-ready` when EVERY story in it holds an accepted handoff. Had any of the four lost its
    // compare-and-set, the wave would still be waiting on that story.
    assert_eq!(
        inspected["data"]["data"]["status"], "handoffs-ready",
        "every concurrent handoff must survive in state: {}",
        inspected["data"]["data"]
    );
    fs::remove_dir_all(root).ok();
}

/// Failing first: a derived artifact is regenerated on the CANDIDATE, once, before the gate.
///
/// Some checked-in artifacts are derived from many stories at once — a documentation mirror, a generated
/// index. They belong to the candidate, not to any one story: two stories regenerating the same artifact
/// collide, and regenerating it on either branch alone yields an artifact missing the other's
/// contribution. `wave-346`'s flux gate refused a candidate with `embedded docs are stale` for exactly
/// that reason, with both stories correct in isolation.
#[test]
fn a_derived_artifact_is_regenerated_on_the_candidate_before_the_gate() {
    let root = fixture("candidate-prepare");
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: One\nstatus: ready\npriority: 1\n---\n\n# One\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    // `INDEX` is derived from every `part-*` file. The gate demands it be current; only a step that runs
    // after all stories are cherry-picked can make that true.
    fs::write(root.join("INDEX"), "").unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\nprepare = [\"sh\", \"-c\", \"ls part-* > INDEX\"]\ngate = [\"sh\", \"-c\", \"ls part-* | diff - INDEX\"]\n"),
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
    // The story adds its part and deliberately does NOT touch the derived index.
    fs::write(story.join("part-a"), "a\n").unwrap();
    assert!(git(&story, &["add", "part-a"]).status.success());
    assert!(git(&story, &["commit", "-qm", "add part-a"])
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
            "part-a",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "part-a",
            "--failing-before",
            "--passing-after",
            "--summary",
            "one part",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "{}",
        String::from_utf8_lossy(&handoff.stdout)
    );

    record_passing_review(&root, "wave-2", "repo/C-1", &commit);
    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "preparation must make the derived artifact current: stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    let integration = PathBuf::from(
        integrated["data"]["topology"]["repositories"][0]["integration"]["worktree"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        fs::read_to_string(integration.join("INDEX"))
            .unwrap()
            .trim(),
        "part-a",
        "the regenerated artifact is part of the candidate"
    );
    // And it is committed, not left dirty — an uncommitted change would not survive into the tag.
    let dirt = String::from_utf8(git(&integration, &["status", "--porcelain"]).stdout).unwrap();
    assert!(dirt.trim().is_empty(), "candidate left dirty: {dirt:?}");
    fs::remove_dir_all(root).ok();
}

/// Failing first: a story that made several commits integrates all of them.
///
/// A handoff names one commit and a worker legitimately makes several — implementation then
/// documentation is the shape the contract asks for. Integration cherry-picked the single cited commit,
/// so it applied only the LAST one and silently dropped the rest: on one real wave a two-commit story
/// contributed only its docs commit, and a five-commit story likewise. That surfaced as a conflict,
/// which was luck; a clean apply would have produced a candidate documenting code that was not in it.
///
/// The evidence already assumed the range — handoff verification computes the write set with
/// `diff <base> <commit>` — so the record described a range the integration never applied.
#[test]
fn a_story_that_made_several_commits_integrates_all_of_them() {
    let (root, story) = one_story_wave("multi-commit-story");
    // Two commits, exactly as a worker is asked to produce: the change, then its record. Only the
    // second is cited by the handoff.
    fs::write(story.join("result.txt"), "implemented\n").unwrap();
    assert!(git(&story, &["add", "result.txt"]).status.success());
    assert!(git(&story, &["commit", "-qm", "implement the thing"])
        .status
        .success());
    fs::write(story.join("EVIDENCE.md"), "why it is right\n").unwrap();
    assert!(git(&story, &["add", "EVIDENCE.md"]).status.success());
    assert!(git(&story, &["commit", "-qm", "record the evidence"])
        .status
        .success());
    let cited = String::from_utf8(git(&story, &["rev-parse", "HEAD"]).stdout)
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
            &cited,
            "--write-set",
            "result.txt",
            "--write-set",
            "EVIDENCE.md",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "result.txt",
            "--failing-before",
            "--passing-after",
            "--summary",
            "two commits, one handoff",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "{}",
        String::from_utf8_lossy(&handoff.stdout)
    );

    record_passing_review(&root, "wave-2", "repo/C-1", &cited);
    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    let integration = PathBuf::from(
        integrated["data"]["topology"]["repositories"][0]["integration"]["worktree"]
            .as_str()
            .unwrap(),
    );
    // The IMPLEMENTATION must be there, not only the commit that was named.
    assert!(
        integration.join("result.txt").is_file(),
        "the cited commit's ancestor carried the implementation and must have been applied too"
    );
    assert!(integration.join("EVIDENCE.md").is_file());
    fs::remove_dir_all(root).ok();
}

/// Failing first: a handoff derives the write set and the owning worker from the story worktree.
///
/// Both facts are already recorded — the range `base..HEAD` in that worktree, and the agent whose
/// assignment names it — yet every handoff restated them by hand. Harvesting six delivered stories in
/// one evening meant reading `state.json` to find the owning worker for each and running
/// `git diff base..HEAD` to retype its write set. A retyped write set is not a typo when it is wrong,
/// it is false evidence; and a story attempted by more than one wave made the item-wide worker lookup
/// "ambiguous" even though the wave records exactly which worker was given this worktree.
#[test]
fn handoff_derives_the_write_set_and_the_owning_worker_from_the_worktree() {
    let (root, story) = one_story_wave("handoff-from-worktree");
    let commit = commit_result(&story, "derived");
    // A second wave holding an attempt at the same item, which is what made the identity ambiguous:
    // two agents now carry `repo/C-1`, and only the one assigned THIS worktree owns this handoff.
    let second = flux(
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
        second.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );

    let handoff = flux(
        &root,
        &[
            "fleet",
            "handoff",
            "wave-2",
            "repo/C-1",
            "--commit",
            &commit,
            "--from-worktree",
            "--test-arg",
            "test",
            "--test-arg",
            "-f",
            "--test-arg",
            "result.txt",
            "--failing-before",
            "--passing-after",
            "--summary",
            "Derived the write set and the owning worker",
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
    // The range proves the write set; nothing was claimed by hand.
    assert_eq!(
        handoff["data"]["write_set"],
        serde_json::json!(["result.txt"])
    );
    // And the worker is the one this wave gave the worktree, not merely one that carries the item.
    assert_eq!(handoff["data"]["worker"], "wave-2-worker-1");
    assert_eq!(
        handoff["data"]["worktree"],
        story.to_string_lossy().as_ref()
    );

    // A hand-typed claim and a derived one are mutually exclusive: one handoff, one source of truth.
    let both = flux(
        &root,
        &[
            "fleet",
            "handoff",
            "wave-2",
            "repo/C-1",
            "--commit",
            &commit,
            "--from-worktree",
            "--write-set",
            "result.txt",
            "--test-arg",
            "test",
            "--summary",
            "both at once",
            "--output",
            "json",
        ],
    );
    assert!(
        !both.status.success(),
        "a derived handoff must refuse a hand-typed write set: {}",
        String::from_utf8_lossy(&both.stdout)
    );
    fs::remove_dir_all(root).ok();
}

/// Failing first: two stories in ONE repository that both write the SAME file integrate, as long as
/// their commits actually combine.
///
/// Integration used to refuse the wave outright whenever two write sets intersected. That is a proxy
/// for "these commits will not combine", and within one repository it is wrong far more often than it
/// is right: `wave-346` was parked because two delivered stories each appended an entry to one
/// changelog, exactly as the worker contract told them to. Nearly every harness story also edits the
/// same hub module, so the proxy made more than one story per repository per wave impossible — the
/// width the fleet exists to provide.
///
/// The real test is the cherry-pick that follows, which fails on a genuine conflict and records the
/// conflicting files and git's own stderr. Disjoint edits to one file must therefore reach the gate.
#[test]
fn two_stories_sharing_one_file_integrate_when_their_commits_combine() {
    let root = fixture("shared-path-combines");
    install_test_fleet_loops(&root);
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
    // A shared append-only ledger, long enough that edits at opposite ends are separated by more than
    // git's three lines of diff context — which is precisely the shape of a changelog.
    let filler = (1..=20)
        .map(|n| format!("- entry {n}\n"))
        .collect::<String>();
    fs::write(root.join("LEDGER.md"), format!("# Ledger\n\n{filler}")).unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"sh\", \"-c\", \"grep -q 'from one' LEDGER.md && grep -q 'from two' LEDGER.md\"]\n"),
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
            "repo/C-2",
            "--prepare-only",
            "--output",
            "json",
        ],
    );
    assert!(
        dispatched.status.success(),
        "{}",
        String::from_utf8_lossy(&dispatched.stdout)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let stories = dispatched["data"]["topology"]["repositories"][0]["stories"]
        .as_array()
        .unwrap()
        .clone();
    for (index, marker) in ["from one", "from two"].iter().enumerate() {
        let story = PathBuf::from(stories[index]["worktree"].as_str().unwrap());
        let ledger = story.join("LEDGER.md");
        let text = fs::read_to_string(&ledger).unwrap();
        // One story edits the head of the file, the other its tail.
        let edited = if index == 0 {
            text.replacen("# Ledger\n", &format!("# Ledger\n\n- {marker}\n"), 1)
        } else {
            format!("{text}- {marker}\n")
        };
        fs::write(&ledger, edited).unwrap();
        assert!(git(&story, &["add", "LEDGER.md"]).status.success());
        assert!(git(&story, &["commit", "-qm", marker]).status.success());
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
                // The identical write set for both stories: the case that used to be refused.
                "--write-set",
                "LEDGER.md",
                "--test-arg",
                "grep",
                "--test-arg",
                "-q",
                "--test-arg",
                marker,
                "--test-arg",
                "LEDGER.md",
                "--failing-before",
                "--passing-after",
                "--summary",
                "appended one ledger entry",
                "--output",
                "json",
            ],
        );
        assert!(
            handoff.status.success(),
            "{}",
            String::from_utf8_lossy(&handoff.stdout)
        );
        record_passing_review(&root, "wave-2", &item, &commit);
    }

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "a shared path is not a conflict: stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    assert_eq!(
        integrated["data"]["topology"]["repositories"][0]["gate"]["runs"],
        1
    );
    // Both entries are in the one candidate, so the combination really happened rather than one story
    // silently winning.
    let integration = PathBuf::from(
        integrated["data"]["topology"]["repositories"][0]["integration"]["worktree"]
            .as_str()
            .unwrap(),
    );
    let combined = fs::read_to_string(integration.join("LEDGER.md")).unwrap();
    assert!(combined.contains("- from one"), "{combined}");
    assert!(combined.contains("- from two"), "{combined}");
    fs::remove_dir_all(root).ok();
}

/// Scaffold a repository whose stories carry a Goal and whose changelog has an empty `[Unreleased]`.
///
/// The empty section is the whole point: it is exactly the state `cut-release.sh` rolls into a
/// version heading, so a fixture that starts with one reproduces how v0.59.2 came to be cut with a
/// three-line release section over 5977 insertions.
fn changelog_wave_fixture(name: &str, stories: &[(&str, &str, &str)]) -> PathBuf {
    let root = fixture(name);
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    for (index, (id, title, goal)) in stories.iter().enumerate() {
        fs::write(
            root.join(format!("docs/stories/{id}-story.md")),
            format!(
                "---\nid: {id}\ntitle: {title}\nstatus: ready\npriority: {}\n---\n\n# {title}\n\n## Goal\n\n{goal}\n\n## Acceptance\n\n- [ ] ship\n",
                index + 1
            ),
        )
        .unwrap();
    }
    fs::write(
        root.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-01-01\n\n### Added\n\n- The first release.\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".flux")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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
    root
}

/// The `[Unreleased]` section body, which is what a release cut actually rolls into its heading.
fn unreleased_section(changelog: &str) -> &str {
    let start = changelog
        .find("## [Unreleased]")
        .expect("fixture changelog has an [Unreleased] heading")
        + "## [Unreleased]".len();
    let rest = &changelog[start..];
    match rest.find("\n## ") {
        Some(offset) => &rest[..offset],
        None => rest,
    }
}

/// C-743 (failing first): the whole gap in one test.
///
/// `.flux/fleet.toml` fences story workers out of `CHANGELOG.md` — correctly, since `wave-346`
/// became unintegrable when two stories each appended an entry — with the note that assembling a
/// wave-level changelog is the integrator's job. Nothing in the integrator did it, which is the same
/// shape as the integrator role itself: configured, and dispatched by nothing until C-730. So
/// v0.59.2 was tagged with an empty `## [0.59.2]` section and published with empty release notes,
/// because `cut-release.sh` rolls `## [Unreleased]` into the version heading and an empty section
/// rolls to an empty section.
#[test]
fn integration_composes_one_unreleased_entry_naming_every_user_visible_story() {
    let root = changelog_wave_fixture(
        "changelog-composed",
        &[
            (
                "C-1",
                "An endpoint records the host it is reachable through",
                "A ClusterIP endpoint looked identical to a public one, and the record could not tell the two apart.",
            ),
            (
                "C-2",
                "A tag build that publishes nothing is red",
                "The release workflow reported success while creating no Release at all.",
            ),
        ],
    );
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
    assert!(
        dispatched.status.success(),
        "{}",
        String::from_utf8_lossy(&dispatched.stdout)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let stories = dispatched["data"]["topology"]["repositories"][0]["stories"]
        .as_array()
        .unwrap()
        .clone();
    for (index, marker) in ["endpoint host", "publishing tag"].iter().enumerate() {
        let worktree = PathBuf::from(stories[index]["worktree"].as_str().unwrap());
        let source = format!("src/change-{}.txt", index + 1);
        fs::create_dir_all(worktree.join("src")).unwrap();
        fs::write(worktree.join(&source), format!("{marker}\n")).unwrap();
        assert!(git(&worktree, &["add", &source]).status.success());
        assert!(git(&worktree, &["commit", "-qm", marker]).status.success());
        let commit = String::from_utf8(git(&worktree, &["rev-parse", "HEAD"]).stdout)
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
                &source,
                "--test-arg",
                "grep",
                "--test-arg",
                "-q",
                "--test-arg",
                marker,
                "--test-arg",
                &source,
                "--failing-before",
                "--passing-after",
                "--summary",
                "shipped a user-visible change",
                "--output",
                "json",
            ],
        );
        assert!(
            handoff.status.success(),
            "{}",
            String::from_utf8_lossy(&handoff.stdout)
        );
        record_passing_review(&root, "wave-2", &item, &commit);
    }

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    let repository = &integrated["data"]["topology"]["repositories"][0];
    assert_eq!(
        repository["changelog"]["composed"], true,
        "integration must report composing the wave's entry: {repository}"
    );

    let integration = PathBuf::from(repository["integration"]["worktree"].as_str().unwrap());
    let on_disk = fs::read_to_string(integration.join("CHANGELOG.md")).unwrap();
    let section = unreleased_section(&on_disk);
    // One entry, not one per story: a wave-level entry is the unit no single story could have
    // written, which is the whole reason the integrator owns it.
    assert_eq!(
        section
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        1,
        "the wave contributes exactly one [Unreleased] entry: {section}"
    );
    // The entry is wrapped like every other entry in the file, so match against the unwrapped prose
    // rather than against one physical line.
    let prose = section.split_whitespace().collect::<Vec<_>>().join(" ");
    for expected in [
        "(C-1, C-2)",
        "An endpoint records the host it is reachable through (C-1)",
        "A tag build that publishes nothing is red (C-2)",
        "A ClusterIP endpoint looked identical to a public one, and the record could not tell the two apart.",
        "The release workflow reported success while creating no Release at all.",
    ] {
        assert!(
            prose.contains(expected),
            "the entry must name {expected}: {section}"
        );
    }
    // Wrapped, not one 400-column line that announces itself as machine-written.
    assert!(
        section.lines().all(|line| line.chars().count() <= 100),
        "the composed entry must be wrapped like the rest of the file: {section}"
    );
    // Already-released sections are not the integrator's to touch.
    assert!(on_disk.contains("- The first release."), "{on_disk}");

    // Reported and real are different claims. The entry has to be in the candidate's TREE, which is
    // what the gate ran against and what an accepted tag would pin — not merely on disk.
    let in_tree = String::from_utf8(git(&integration, &["show", "HEAD:CHANGELOG.md"]).stdout)
        .expect("the candidate carries a CHANGELOG.md");
    assert_eq!(
        unreleased_section(&in_tree),
        section,
        "the composed entry must be committed into the candidate, not left uncommitted"
    );
    fs::remove_dir_all(root).ok();
}

/// C-743: an entry padded to prove the step ran is worse than no entry.
///
/// A story that wrote only its own story document changed nothing a reader of the changelog can
/// observe, so it contributes no line — and integration says which story it dropped and why, rather
/// than silently producing an empty section that looks the same as the defect.
#[test]
fn a_wave_whose_stories_are_all_internal_composes_no_changelog_entry() {
    let root = changelog_wave_fixture(
        "changelog-internal",
        &[("C-1", "Record the plan", "Write down where this stands.")],
    );
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
        "{}",
        String::from_utf8_lossy(&dispatched.stdout)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let worktree = PathBuf::from(
        dispatched["data"]["topology"]["repositories"][0]["stories"][0]["worktree"]
            .as_str()
            .unwrap(),
    );
    let story_file = "docs/stories/C-1-story.md";
    let existing = fs::read_to_string(worktree.join(story_file)).unwrap();
    fs::write(
        worktree.join(story_file),
        format!("{existing}\n## Progress\n\n- Recorded the plan.\n"),
    )
    .unwrap();
    assert!(git(&worktree, &["add", story_file]).status.success());
    assert!(git(&worktree, &["commit", "-qm", "record the plan"])
        .status
        .success());
    let commit = String::from_utf8(git(&worktree, &["rev-parse", "HEAD"]).stdout)
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
            story_file,
            "--test-arg",
            "grep",
            "--test-arg",
            "-q",
            "--test-arg",
            "Recorded the plan",
            "--test-arg",
            story_file,
            "--failing-before",
            "--passing-after",
            "--summary",
            "recorded the plan on the story",
            "--output",
            "json",
        ],
    );
    assert!(
        handoff.status.success(),
        "{}",
        String::from_utf8_lossy(&handoff.stdout)
    );
    record_passing_review(&root, "wave-2", "repo/C-1", &commit);

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    let repository = &integrated["data"]["topology"]["repositories"][0];
    assert_eq!(
        repository["changelog"]["composed"], false,
        "an internal-only wave composes nothing: {repository}"
    );
    // "Says so" is the acceptance, not just "writes nothing": a silent skip is indistinguishable
    // from the defect this story exists to close.
    let dropped = repository["changelog"]["stories"][0]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        dropped.contains(story_file),
        "integration must name what it dropped and why: {repository}"
    );

    let integration = PathBuf::from(repository["integration"]["worktree"].as_str().unwrap());
    let on_disk = fs::read_to_string(integration.join("CHANGELOG.md")).unwrap();
    assert_eq!(
        unreleased_section(&on_disk).trim(),
        "",
        "no filler line: {on_disk}"
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

/// C-587 (failing first): the whole gap in one test.
///
/// Nothing adversarial ever looked at a Fleet candidate. `"reviewer"` appeared once in the entire
/// fleet command — as a *string a rework may cite* — and `.flux/fleet.toml` pointed the `review` task
/// kind at the read-only research loop, which no code path ever selected. So the only evidence a
/// candidate carried into integration was its own writer's claim, and the repository gate was spent on
/// it before anyone but the author had read a line.
#[test]
fn fleet_integrate_refuses_a_candidate_no_independent_reviewer_examined() {
    let (root, story) = one_story_wave("review-gate");
    let commit = commit_result(&story, "first attempt");
    let handoff = submit_result_handoff(&root, &commit);
    let writer = handoff["data"]["worker"].as_str().unwrap().to_string();

    let unreviewed = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        !unreviewed.status.success(),
        "an unreviewed candidate must never reach the gate: stdout={} stderr={}",
        String::from_utf8_lossy(&unreviewed.stdout),
        String::from_utf8_lossy(&unreviewed.stderr)
    );
    let unreviewed: serde_json::Value = serde_json::from_slice(&unreviewed.stdout).unwrap();
    assert!(
        unreviewed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("independent review")),
        "the refusal must name what is missing: {unreviewed}"
    );

    // The writer cannot clear its own candidate, whatever it puts in the file.
    let self_review = root.join("self-review.json");
    fs::write(
        &self_review,
        serde_json::to_string(&serde_json::json!({
            "schema": "flux.fleet-review/v1",
            "reviewer": writer,
            "reviewed_commit": commit,
            "verdict": "PASS",
            "findings": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let refused = flux(
        &root,
        &[
            "fleet",
            "review",
            "wave-2",
            "--item",
            "repo/C-1",
            "--from",
            self_review.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        !refused.status.success(),
        "a writer reviewing itself is not review: {}",
        String::from_utf8_lossy(&refused.stdout)
    );

    // A review that names findings routes the story back to its writer rather than to the gate.
    let rework_review = root.join("rework-review.json");
    fs::write(
        &rework_review,
        serde_json::to_string(&serde_json::json!({
            "schema": "flux.fleet-review/v1",
            "reviewer": "reviewer-1",
            "reviewed_commit": commit,
            "verdict": "REWORK",
            "findings": [{
                "category": "contract",
                "severity": "blocker",
                "confidence": "high",
                "component": "result.txt",
                "evidence": {"path": "result.txt", "line": 1},
                "detail": "the acceptance item is not satisfied by this line",
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let reworked = flux(
        &root,
        &[
            "fleet",
            "review",
            "wave-2",
            "--item",
            "repo/C-1",
            "--from",
            rework_review.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        reworked.status.success(),
        "{}",
        String::from_utf8_lossy(&reworked.stdout)
    );
    let reworked: serde_json::Value = serde_json::from_slice(&reworked.stdout).unwrap();
    assert_eq!(reworked["data"]["reviews"][0]["verdict"], "REWORK");
    assert_eq!(reworked["data"]["reviews"][0]["state"], "reviewed");
    assert_eq!(
        reworked["data"]["reviews"][0]["rework"]["decision"],
        "REWORK"
    );
    let still_refused = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        !still_refused.status.success(),
        "findings are not advisory: {}",
        String::from_utf8_lossy(&still_refused.stdout)
    );

    // The repair, and the independent PASS over the exact repaired commit, is what unblocks the gate.
    let repaired = commit_result(&story, "second attempt");
    submit_result_handoff(&root, &repaired);
    let pass_review = root.join("pass-review.json");
    fs::write(
        &pass_review,
        serde_json::to_string(&serde_json::json!({
            "schema": "flux.fleet-review/v1",
            "reviewer": "reviewer-1",
            "reviewed_commit": repaired,
            "verdict": "PASS",
            "findings": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let passed = flux(
        &root,
        &[
            "fleet",
            "review",
            "wave-2",
            "--item",
            "repo/C-1",
            "--from",
            pass_review.to_str().unwrap(),
            "--output",
            "json",
        ],
    );
    assert!(
        passed.status.success(),
        "{}",
        String::from_utf8_lossy(&passed.stdout)
    );
    let passed: serde_json::Value = serde_json::from_slice(&passed.stdout).unwrap();
    assert_eq!(passed["data"]["reviews"][0]["verdict"], "PASS");
    // A clean review and a review that never happened are different records, not both "no findings".
    assert_eq!(passed["data"]["reviews"][0]["examined"], true);
    assert_eq!(passed["data"]["reviews"][0]["reviewed_commit"], repaired);

    let integrated = flux(&root, &["fleet", "integrate", "wave-2", "--output", "json"]);
    assert!(
        integrated.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&integrated.stdout),
        String::from_utf8_lossy(&integrated.stderr)
    );
    let integrated: serde_json::Value = serde_json::from_slice(&integrated.stdout).unwrap();
    assert_eq!(integrated["data"]["status"], "green");
    fs::remove_dir_all(root).ok();
}

/// C-631 (R-10, failing first): harvest before the pause. A worker that commits its deliverable and
/// then runs out of turn leaves that work in a worktree nothing has recorded; parking on top of it
/// buries a finished story under a decision — three separate waves once held the same completed story,
/// were parked as failures, and a human dug the commits out days later. `park` records what the
/// worktrees already prove, and says what it harvested.
#[test]
fn parking_a_wave_harvests_committed_work_before_the_pause() {
    let (root, story) = one_story_wave("park-harvest");
    let commit = commit_result(&story, "delivered");

    let parked = flux(
        &root,
        &[
            "fleet",
            "park",
            "wave-2",
            "--reason",
            "waiting on the API decision",
            "--output",
            "json",
        ],
    );
    assert!(
        parked.status.success(),
        "{}",
        String::from_utf8_lossy(&parked.stdout)
    );
    let parked: serde_json::Value = serde_json::from_slice(&parked.stdout).unwrap();
    assert_eq!(parked["data"]["status"], "parked");
    let harvested = parked["data"]["harvested"]
        .as_array()
        .expect("the park reports what it harvested");
    let recorded = harvested
        .iter()
        .find(|report| report["item"] == "repo/C-1")
        .unwrap_or_else(|| panic!("the parked wave's story is reported: {harvested:?}"));
    assert_eq!(recorded["recorded"], true, "{recorded}");
    assert_eq!(recorded["commit"], commit);

    // The harvest is journalled, so the commit the pause could have buried survives a restart as a
    // recorded handoff rather than as an unread worktree.
    let events = flux(
        &root,
        &["fleet", "events", "--limit", "200", "--output", "json"],
    );
    let events: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    assert!(
        events["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "wave.park.harvested"),
        "the harvest is journalled: {}",
        events["data"]["events"]
    );

    fs::remove_dir_all(root).ok();
}

/// C-639: parking used to live in a driver-owned text file — invisible to `fleet status`, so a parked
/// wave was re-decided every minute, and unparking meant editing text. Parking is a lifecycle state of
/// the wave, with a reason, and returning from it is a verb.
#[test]
fn parking_a_wave_records_the_reason_and_unparking_restores_the_state_it_held() {
    let (root, _story) = one_story_wave("park-lifecycle");

    let parked = flux(
        &root,
        &[
            "fleet",
            "park",
            "wave-2",
            "--reason",
            "waiting on a human decision",
            "--output",
            "json",
        ],
    );
    assert!(
        parked.status.success(),
        "{}",
        String::from_utf8_lossy(&parked.stdout)
    );
    let parked: serde_json::Value = serde_json::from_slice(&parked.stdout).unwrap();
    assert_eq!(parked["data"]["wave"], "wave-2");
    assert_eq!(parked["data"]["status"], "parked");
    assert_eq!(
        parked["data"]["park"]["reason"],
        "waiting on a human decision"
    );
    assert_eq!(parked["data"]["park"]["previous_status"], "accepted");
    assert!(parked["data"]["park"]["revision"].is_number());

    // The whole point: the pause and its reason are visible without reading `state.json`.
    let status = flux(&root, &["fleet", "status", "--output", "json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let wave = status["data"]["waves"]["listed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|wave| wave["id"] == "wave-2")
        .expect("the parked wave is listed");
    assert_eq!(wave["status"], "parked");
    assert_eq!(wave["park"]["reason"], "waiting on a human decision");
    let human = flux(&root, &["fleet", "status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(
        human.contains("parked: waiting on a human decision"),
        "the human status must name the park reason: {human}"
    );

    // A second park is a typed conflict, never a silent overwrite of the recorded reason.
    let again = flux(
        &root,
        &[
            "fleet",
            "park",
            "wave-2",
            "--reason",
            "another reason",
            "--output",
            "json",
        ],
    );
    assert_eq!(again.status.code(), Some(4));

    let unparked = flux(&root, &["fleet", "unpark", "wave-2", "--output", "json"]);
    assert!(
        unparked.status.success(),
        "{}",
        String::from_utf8_lossy(&unparked.stdout)
    );
    let unparked: serde_json::Value = serde_json::from_slice(&unparked.stdout).unwrap();
    assert_eq!(unparked["data"]["status"], "accepted");
    assert_eq!(unparked["data"]["previous_status"], "parked");
    let status = flux(&root, &["fleet", "status", "--output", "json"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let wave = status["data"]["waves"]["listed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|wave| wave["id"] == "wave-2")
        .expect("the unparked wave is listed");
    assert_eq!(wave["status"], "accepted");
    assert!(wave["park"].is_null(), "the park record is cleared");

    // Unparking a wave that is not parked, and parking one that does not exist, are typed failures.
    let twice = flux(&root, &["fleet", "unpark", "wave-2", "--output", "json"]);
    assert_eq!(twice.status.code(), Some(4));
    let missing = flux(
        &root,
        &[
            "fleet", "park", "wave-404", "--reason", "absent", "--output", "json",
        ],
    );
    assert_eq!(missing.status.code(), Some(3));

    // Both verbs are journalled, so the pause survives a restart with its reason.
    let events = flux(
        &root,
        &["fleet", "events", "--limit", "200", "--output", "json"],
    );
    let events: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    let kinds = events["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"wave.parked"), "{kinds:?}");
    assert!(kinds.contains(&"wave.unparked"), "{kinds:?}");

    fs::remove_dir_all(root).ok();
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
        ("gate", Some("wave-2")),
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
    fs::create_dir_all(root.join(".flux/fleet/loops")).unwrap();
    fs::write(
        root.join(".flux/fleet/main.md"),
        "Act as the only main coordinator and acknowledge the request.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/loops/main.flux"),
        "flow fleet-main -> string\n  result = task({ role: \"scout\", task: \"read-only fixture research\" })\n  return result\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/loops/research.flux"),
        "flow fleet-research -> string\n  return \"acknowledged\"\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        "schema = \"flux.fleet/v1\"\n\n[main]\ninstructions = \".flux/fleet/main.md\"\nmodel = \"mock\"\nloop = \".flux/fleet/loops/main.flux\"\nresearch_loop = \".flux/fleet/loops/research.flux\"\n",
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
    assert_eq!(first["data"]["receipt"]["answer"], "acknowledged");
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
    install_test_fleet_loops(&root);
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
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n{TEST_FLEET_LOOP_POLICY}\n[[agent_templates]]\nid = \"story-worker\"\nrole = \"writer\"\ntask_kind = \"implementation\"\ninstructions = \".flux/fleet/agents/story-worker.md\"\nmodel = \"mock\"\nmode = \"write\"\ncapabilities = [\"read\", \"edit\", \"git\", \"shell\"]\nmax_instances = 3\n\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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
    let receipts = run["data"]["receipts"].as_array().unwrap();
    assert_eq!(receipts[0]["context_origin"]["kind"], "story-assignment");
    assert_eq!(receipts[0]["context_origin"]["board_ref"], "repo/C-1");
    assert_eq!(receipts[1]["context_origin"]["board_ref"], "repo/C-2");
    assert_eq!(receipts[0]["context_origin"]["session_mode"], "fresh");
    assert_eq!(receipts[1]["context_origin"]["session_mode"], "fresh");
    assert_ne!(receipts[0]["store"], receipts[1]["store"]);
    for receipt in receipts {
        assert_eq!(receipt["loop_binding"]["profile"], "implementation");
        assert_eq!(
            receipt["loop_binding"]["source_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        let context = serde_json::to_string(&receipt["context_origin"]).unwrap();
        assert!(!context.contains("Work only in the assigned story worktree"));
        assert!(!context.contains("flux-mock"));
        assert!(context.len() < 512, "context origin was {context}");
        let capability_set = serde_json::to_string(&receipt["capability_set"]).unwrap();
        assert_eq!(
            receipt["capability_set"]["schema"],
            "flux.fleet-capability-set/v1"
        );
        assert_eq!(
            receipt["capability_set"]["digest_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(!capability_set.contains("operations"));
        assert!(!capability_set.contains("Work only"));
        assert!(!capability_set.contains(&root.display().to_string()));
        assert!(
            capability_set.len() < 512,
            "capability manifest was {capability_set}"
        );
    }
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

    let first_agent = receipts[0]["agent"].as_str().unwrap();
    let first_capability_set = receipts[0]["capability_set"].clone();
    let first_worker_contract = receipts[0]["context_origin"]["worker_contract_sha256"].clone();
    let original_config = fs::read_to_string(root.join(".flux/fleet.toml")).unwrap();
    let original_instructions =
        fs::read_to_string(root.join(".flux/fleet/agents/story-worker.md")).unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        original_config.replace(
            "capabilities = [\"read\", \"edit\", \"git\", \"shell\"]",
            "capabilities = [\"read\", \"edit\", \"git\", \"shell\", \"task\"]",
        ),
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/agents/story-worker.md"),
        "A widened replacement template that existing workers must never load.\n",
    )
    .unwrap();
    let continued = flux(
        &root,
        &[
            "fleet",
            "message",
            first_agent,
            "Continue only this exact story",
            "--wait",
            "completed",
            "--output",
            "json",
        ],
    );
    assert!(
        continued.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&continued.stdout),
        String::from_utf8_lossy(&continued.stderr)
    );
    let continued: serde_json::Value = serde_json::from_slice(&continued.stdout).unwrap();
    assert_eq!(continued["data"]["receipt"]["agent"], first_agent);
    assert_eq!(continued["data"]["receipt"]["session"], "s_1");
    assert_eq!(
        continued["data"]["receipt"]["context_origin"]["session_mode"],
        "continue"
    );
    assert_eq!(continued["data"]["receipt"]["store"], receipts[0]["store"]);
    assert_ne!(continued["data"]["receipt"]["store"], receipts[1]["store"]);
    assert_eq!(
        continued["data"]["receipt"]["capability_set"],
        first_capability_set
    );
    assert_eq!(
        continued["data"]["receipt"]["context_origin"]["worker_contract_sha256"],
        first_worker_contract
    );
    let status = flux(&root, &["fleet", "status", "--output", "json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    // C-562: the default projection is bounded operational truth. It names the worker, its exact
    // BoardRef and its session, and never copies the raw agent record, its instruction body or its
    // admitted operation catalogue out of durable state.
    assert_eq!(status["data"]["schema"], "flux.fleet-status/v1");
    assert_eq!(status["data"]["bounded"], true);
    assert!(
        status["data"]["state"].is_null(),
        "default status embedded raw Fleet state: {status}"
    );
    let listed = status["data"]["workers"]["listed"].as_array().unwrap();
    let row = listed
        .iter()
        .find(|worker| worker["id"] == first_agent)
        .unwrap_or_else(|| panic!("{first_agent} missing from {listed:?}"));
    assert_eq!(row["board_ref"], receipts[0]["context_origin"]["board_ref"]);
    let projected = status.to_string();
    assert!(
        !projected.contains("Work only in the assigned story worktree"),
        "default status embedded the worker instruction body: {projected}"
    );
    assert!(
        !projected.contains("read_roots"),
        "default status embedded the admitted operation scope: {projected}"
    );

    // Detail remains reachable through the explicitly bounded inspect route.
    let inspected = flux(
        &root,
        &[
            "fleet",
            "inspect",
            "worker",
            first_agent,
            "--limit",
            "50",
            "--output",
            "json",
        ],
    );
    assert!(
        inspected.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&inspected.stdout),
        String::from_utf8_lossy(&inspected.stderr)
    );
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected["data"]["bounded"], true);
    let admitted = &inspected["data"]["data"];
    assert_eq!(
        admitted["capabilities"],
        serde_json::json!(["edit", "git", "read", "shell"])
    );
    assert_eq!(admitted["mode"], "write");
    assert_eq!(admitted["writable_root"], stories[0]["worktree"]);
    assert_eq!(admitted["read_roots"], serde_json::json!([]));
    assert_eq!(admitted["capability_set"], first_capability_set);
    assert_eq!(
        admitted["instructions"],
        "Work only in the assigned story worktree and report evidence.\n"
    );

    let resumed = flux(&root, &["fleet", "resume", first_agent, "--output", "json"]);
    assert!(
        resumed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["data"]["receipt"]["session"], "s_1");
    assert_eq!(
        resumed["data"]["receipt"]["capability_set"],
        first_capability_set
    );
    assert_eq!(
        resumed["data"]["receipt"]["context_origin"]["session_mode"],
        "continue"
    );

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
    assert_eq!(
        rework["data"]["turn_receipt"]["capability_set"],
        first_capability_set
    );
    fs::write(root.join(".flux/fleet.toml"), original_config).unwrap();
    fs::write(
        root.join(".flux/fleet/agents/story-worker.md"),
        original_instructions,
    )
    .unwrap();
    assert!(
        git(&root, &["status", "--short"]).stdout.is_empty(),
        "the source checkout remains untouched"
    );
    fs::remove_dir_all(root).ok();
}

/// C-642: quiescing before an install is a verb, and it is safe in the order it does its two jobs.
///
/// The window is recorded before liveness is inspected, so the refusal that follows a live worker
/// cannot be raced by a dispatch; and the verb itself fails while a worker turn is in flight, so
/// `flux fleet quiesce && install` cannot walk past one the way a process-table scan did.
#[test]
fn fleet_quiesce_stops_dispatch_and_refuses_to_confirm_while_a_worker_is_in_flight() {
    let root = fixture("quiesce-install-window");
    install_test_fleet_loops(&root);
    fs::write(root.join(".gitignore"), ".flux/fleet/\n").unwrap();
    fs::write(
        root.join("docs/stories/C-1-story.md"),
        "---\nid: C-1\ntitle: First story\nstatus: ready\npriority: 1\n---\n\n# First story\n\n## Acceptance\n\n- [ ] ship\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/main.md"),
        "Act as the only main coordinator and acknowledge the request.\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/loops/main.flux"),
        "flow fleet-main -> string\n  result = task({ role: \"scout\", task: \"read-only fixture research\" })\n  return result\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet/loops/main-research.flux"),
        "flow fleet-research -> string\n  return \"acknowledged\"\n",
    )
    .unwrap();
    fs::write(
        root.join(".flux/fleet.toml"),
        format!("schema = \"flux.fleet/v1\"\nworktree_root = \".flux/fleet/worktrees\"\n\n[main]\ninstructions = \".flux/fleet/main.md\"\nmodel = \"mock\"\nloop = \".flux/fleet/loops/main.flux\"\nresearch_loop = \".flux/fleet/loops/main-research.flux\"\n{TEST_FLEET_LOOP_POLICY}\n[[repositories]]\nid = \"repo\"\nroot = \".\"\nboard = \"repo\"\ncanonical_ref = \"HEAD\"\ngate = [\"git\", \"status\", \"--short\"]\n"),
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
    assert!(
        dispatched.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&dispatched.stdout),
        String::from_utf8_lossy(&dispatched.stderr)
    );
    let dispatched: serde_json::Value = serde_json::from_slice(&dispatched.stdout).unwrap();
    let worker = dispatched["data"]["agents"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A prepared worker has no turn behind it, so record the state an operator actually installs
    // into by mistake: a worker that is genuinely mid-turn, with no terminal receipt to settle it.
    let state_path = root.join(".flux/fleet/state.json");
    let set_worker_status = |status: &str| {
        let mut state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        state["agents"][worker.as_str()]["status"] = serde_json::json!(status);
        fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    };
    set_worker_status("working");

    let refused = flux(
        &root,
        &[
            "fleet",
            "quiesce",
            "--reason",
            "install a new binary",
            "--output",
            "json",
        ],
    );
    assert!(
        !refused.status.success(),
        "quiesce confirmed while a worker was in flight: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused["error"]["class"], "conflict/precondition");
    let refusal = refused["error"]["message"].as_str().unwrap();
    assert!(
        refusal.contains(&worker),
        "refusal did not name the live worker: {refusal}"
    );

    // ...and dispatch stopped anyway, because the window is recorded before liveness is inspected.
    let blocked = flux(
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
        !blocked.status.success(),
        "a quiesced fleet dispatched a wave: {}",
        String::from_utf8_lossy(&blocked.stdout)
    );
    let blocked: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked["error"]["class"], "conflict/precondition");
    let blocked = blocked["error"]["message"].as_str().unwrap();
    assert!(
        blocked.contains("quiesced") && blocked.contains("install a new binary"),
        "dispatch refusal did not name the recorded window: {blocked}"
    );

    let status = flux(&root, &["fleet", "status", "--output", "json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["data"]["quiesce"]["reason"], "install a new binary");

    // Once the worker settles, the same command confirms instead of refusing.
    set_worker_status("completed");
    let confirmed = flux(
        &root,
        &[
            "fleet",
            "quiesce",
            "--reason",
            "install a new binary",
            "--output",
            "json",
        ],
    );
    assert!(
        confirmed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&confirmed.stdout),
        String::from_utf8_lossy(&confirmed.stderr)
    );
    let confirmed: serde_json::Value = serde_json::from_slice(&confirmed.stdout).unwrap();
    assert_eq!(confirmed["data"]["safe_to_install"], true);
    assert_eq!(confirmed["data"]["in_flight"].as_array().unwrap().len(), 0);

    // `resume` is the inverse: it lifts the recorded window and dispatch works again.
    let resumed = flux(&root, &["fleet", "resume", "--output", "json"]);
    assert!(
        resumed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(
        resumed["data"]["quiesce_lifted"]["reason"],
        "install a new binary"
    );

    let status = flux(&root, &["fleet", "status", "--output", "json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(
        status["data"]["quiesce"].is_null(),
        "resume left the window recorded: {status}"
    );

    let redispatched = flux(
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
        redispatched.status.success(),
        "dispatch stayed refused after resume: stdout={} stderr={}",
        String::from_utf8_lossy(&redispatched.stdout),
        String::from_utf8_lossy(&redispatched.stderr)
    );
    fs::remove_dir_all(root).ok();
}
