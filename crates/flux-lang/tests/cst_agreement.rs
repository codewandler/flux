//! CST cutover corpus guard: every shipped source fixture must be lossless, ERROR-free, and lower
//! to the exact AST produced by the retired line parser.
//!
//! The shipped-fixture oracle is independent of the current parser: its SHA-256 values were frozen
//! from the legacy `parse_program` implementation in a clean `git archive` of
//! `6e691962a1e4c2f435d78d0466d12121af4799c2` before L-80 removed that parser. The hash input is the
//! module kind (`flow\0` or `program\0`) followed by compact `serde_json` for the legacy `DraftAst`
//! or `Program`. The current source is parsed into a CST exactly once before strict lowering and
//! comparison with that frozen evidence.
//!
//! Two additional contracts are swept: every token/comment retains its exact source range, and
//! every executable AST in every fixture survives format→parse. An inline battery covers every node
//! kind's native spelling (mirrors `docs/syntax.md`).

use flux_lang::ast::DraftAst;
use flux_lang::lower_cst::{DeclarationRanges, LoweredModule};
use flux_lang::parser::parse_cst;
use flux_lang::program::Module;
use flux_lang::syntax::SyntaxKind;
use rowan::TextRange;
use sha2::{Digest, Sha256};

struct LegacyFixture {
    path: &'static str,
    ast_sha256: &'static str,
    /// Number of embedded `DraftAst`s that have a public text formatter. A declaration-only program
    /// legitimately has zero; its complete `Program` still remains pinned by `ast_sha256`.
    formatted_asts: usize,
}

/// Frozen pre-cutover evidence for every shipped text fixture the line parser accepted.
const LEGACY_FIXTURES: &[LegacyFixture] = &[
    LegacyFixture {
        path: "crates/flux-app/examples/hello.flux",
        ast_sha256: "373d4375cd5ee12310a42ac8e54ff1d54e8df8317f306c72daf8f0c10534bffd",
        formatted_asts: 2,
    },
    LegacyFixture {
        path: "crates/flux-app/examples/support-bot.flux",
        ast_sha256: "15f8e487fa3987fbf13637e39f619fea6d845d3953b8731fbcde856e59f37aeb",
        formatted_asts: 0,
    },
    LegacyFixture {
        path: "crates/flux-flow/assets/agent-loop.flux",
        ast_sha256: "5d9a75f3a8cac88fadd38ee0ae98e709f12f2767afa7f168f0f59ee18b283938",
        formatted_asts: 1,
    },
    LegacyFixture {
        path: "examples/advanced-code-review.flux",
        ast_sha256: "fca9ecdf1aed3a0702772dccbeee27c3d3c66b748d4aa113f1f9fea7ddacff95",
        formatted_asts: 1,
    },
    LegacyFixture {
        path: "examples/channels-app.flux",
        ast_sha256: "3ddccdcb9816e4302e19affefa0b61a58e1c4f1a73d341986bccb181fec7236b",
        formatted_asts: 3,
    },
    LegacyFixture {
        path: "examples/data-transforms.flux",
        ast_sha256: "7b94b8fc358b3976ac3283299799d80a2d709bd73fbf6e6dd47eee11a5c9e29a",
        formatted_asts: 1,
    },
    LegacyFixture {
        path: "examples/god-review.flux",
        ast_sha256: "6eabbab9ed6dc7fcdc627d3592652a791f46f2cb231ad1575ff941d983fbabc9",
        formatted_asts: 1,
    },
    LegacyFixture {
        path: "examples/multi-perspective.flux",
        ast_sha256: "f442fd032d935baa9616ed4b550c7c38cc5c93a908aa74fa1912a6bbdd29f236",
        formatted_asts: 1,
    },
    LegacyFixture {
        path: "examples/strict_review.flux",
        ast_sha256: "d0e3eff5b7a0db8aec62590af8a00b70d25d9d3b272da12c1a359a3bf3f366e1",
        formatted_asts: 1,
    },
];

/// Shipped fixtures authored *after* the CST cutover (L-80). The legacy `parse_program` no longer
/// exists, so there is no independent oracle to freeze an `ast_sha256` against — pinning one from
/// the current parser would only assert that the parser agrees with itself. They are held to every
/// other contract in this file (losslessness, no ERROR nodes, exact token/comment ranges, and
/// format→parse survival of every executable AST).
const POST_CUTOVER_FIXTURES: &[&str] = &["examples/bitcoin-price.flux"];

fn ast_hash(module: &Module) -> String {
    let mut hash = Sha256::new();
    match module {
        Module::Flow(ast) => {
            hash.update(b"flow\0");
            hash.update(serde_json::to_vec(ast).expect("serialize current flow"));
        }
        Module::Program(program) => {
            hash.update(b"program\0");
            hash.update(serde_json::to_vec(program).expect("serialize current program"));
        }
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn source_slice(src: &str, range: TextRange) -> &str {
    let start = u32::from(range.start()) as usize;
    let end = u32::from(range.end()) as usize;
    src.get(start..end)
        .unwrap_or_else(|| panic!("CST range {range:?} is not on UTF-8 boundaries"))
}

fn assert_token_and_comment_ranges(src: &str, parsed: &flux_lang::parser::Parse, what: &str) {
    let root = parsed.syntax();
    assert_eq!(
        u32::from(root.text_range().start()),
        0,
        "{what}: root start"
    );
    assert_eq!(
        u32::from(root.text_range().end()) as usize,
        src.len(),
        "{what}: root range must cover the complete source"
    );

    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .collect();
    let mut cursor = 0usize;
    for token in &tokens {
        let range = token.text_range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        assert_eq!(start, cursor, "{what}: token ranges must be contiguous");
        assert_eq!(
            source_slice(src, range),
            token.text(),
            "{what}: token range points at different source text"
        );
        cursor = end;
    }
    assert_eq!(cursor, src.len(), "{what}: token ranges must cover source");

    let expected_comments: Vec<String> = src
        .lines()
        .map(str::trim_start)
        .filter(|line| *line == "#" || line.starts_with("# "))
        .map(str::to_string)
        .collect();
    let actual_comments: Vec<String> = tokens
        .iter()
        .filter(|token| token.kind() == SyntaxKind::COMMENT)
        .map(|token| token.text().to_string())
        .collect();
    assert_eq!(
        actual_comments, expected_comments,
        "{what}: every shipped source comment must survive as one ranged CST token"
    );
}

fn assert_declaration_ranges(src: &str, ast: &DraftAst, ranges: &DeclarationRanges, what: &str) {
    assert!(
        !source_slice(src, ranges.declaration).trim().is_empty(),
        "{what}: declaration range must identify source"
    );
    for index in 0..ast.body.len() {
        let path = format!("body[{index}]");
        let range = ranges
            .body
            .get(&path)
            .unwrap_or_else(|| panic!("{what}: missing range for {path}"));
        assert!(
            !source_slice(src, range).trim().is_empty(),
            "{what}: {path} range must identify source"
        );
    }
}

fn assert_module_ranges(src: &str, lowered: &LoweredModule, what: &str) {
    match &lowered.module {
        Module::Flow(ast) => {
            assert_eq!(lowered.flows.len(), 1, "{what}: one bare-flow range map");
            assert!(lowered.ops.is_empty(), "{what}: bare flow has no op ranges");
            assert_declaration_ranges(src, ast, &lowered.flows[0], what);
        }
        Module::Program(program) => {
            assert_eq!(
                lowered.flows.len(),
                program.flows.len(),
                "{what}: every top-level flow has a range map"
            );
            assert_eq!(
                lowered.ops.len(),
                program.ops.len(),
                "{what}: every composite op has a range map"
            );
            for (index, (ast, ranges)) in program.flows.iter().zip(&lowered.flows).enumerate() {
                assert_declaration_ranges(src, ast, ranges, &format!("{what}: flow[{index}]"));
            }
            for (index, (op, ranges)) in program.ops.iter().zip(&lowered.ops).enumerate() {
                assert_declaration_ranges(src, &op.body, ranges, &format!("{what}: op[{index}]"));
            }
        }
    }
}

fn assert_flow_round_trip(ast: &DraftAst, what: &str) {
    let formatted = flux_lang::format::format(ast);
    let reparsed = flux_lang::parse::parse(&formatted)
        .unwrap_or_else(|error| panic!("{what}: formatted AST must parse: {error}\n{formatted}"));
    assert_eq!(reparsed, *ast, "{what}: format→parse changed the AST");
}

fn assert_module_round_trips(module: &Module, what: &str) -> usize {
    let mut checked = 0usize;
    match module {
        Module::Flow(ast) => {
            assert_flow_round_trip(ast, what);
            checked += 1;
        }
        Module::Program(program) => {
            for (index, agent_loop) in program.agent_loops.iter().enumerate() {
                assert_flow_round_trip(&agent_loop.flow, &format!("{what}: agent_loop[{index}]"));
                checked += 1;
            }
            for (index, journey) in program.journeys.iter().enumerate() {
                assert_flow_round_trip(&journey.flow, &format!("{what}: journey[{index}]"));
                checked += 1;
            }
            for (index, op) in program.ops.iter().enumerate() {
                assert_flow_round_trip(&op.body, &format!("{what}: op[{index}]"));
                checked += 1;
            }
            for (index, flow) in program.flows.iter().enumerate() {
                assert_flow_round_trip(flow, &format!("{what}: flow[{index}]"));
                checked += 1;
            }
        }
    }
    checked
}

/// Assert the CST/strict contract for one accepted fixture.
fn assert_agreement(src: &str, what: &str, legacy: Option<&LegacyFixture>) {
    // The original source enters the current parser once. Strict acceptance and the semantic AST
    // both come from this tree; no second current parse is used as an expected value.
    let parsed = parse_cst(src);
    assert_eq!(
        parsed.syntax().text().to_string(),
        src,
        "{what}: CST lost source text"
    );
    let errors = &parsed.errors;
    assert!(
        errors.is_empty(),
        "{what}: strict API accepts but the CST parser reported {} error(s): {:?}\nsource:\n{src}",
        errors.len(),
        errors
    );
    let error_nodes: Vec<_> = parsed
        .syntax()
        .descendants()
        .filter(|n| n.kind() == flux_lang::syntax::SyntaxKind::ERROR)
        .map(|n| format!("{:?} @ {:?}", n.text().to_string(), n.text_range()))
        .collect();
    assert!(
        error_nodes.is_empty(),
        "{what}: strict API accepts but the CST contains ERROR node(s): {error_nodes:?}\nsource:\n{src}"
    );
    let error_tokens: Vec<_> = parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == flux_lang::syntax::SyntaxKind::ERROR)
        .collect();
    assert!(
        error_tokens.is_empty(),
        "{what}: strict API accepts but the CST contains ERROR tokens: {error_tokens:?}"
    );

    assert_token_and_comment_ranges(src, &parsed, what);
    let lowered = flux_lang::lower_cst::cst_to_module(&parsed)
        .unwrap_or_else(|errors| panic!("{what}: strict CST lowering failed: {errors:?}"));
    if let Some(fixture) = legacy {
        assert_eq!(
            ast_hash(&lowered.module),
            fixture.ast_sha256,
            "{what}: CST AST differs from the frozen pre-cutover line-parser AST"
        );
    }
    assert_module_ranges(src, &lowered, what);
    let formatted_asts = assert_module_round_trips(&lowered.module, what);
    if let Some(fixture) = legacy {
        assert_eq!(
            formatted_asts, fixture.formatted_asts,
            "{what}: executable AST census changed"
        );
    }
}

#[test]
fn shipped_flux_corpus_agreement() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = std::fs::canonicalize(manifest.parent().unwrap().parent().unwrap())
        .expect("canonical repository root");
    let dirs = [
        repo.join("examples"),
        repo.join("crates/flux-lang/examples"),
        repo.join("crates/flux-app/examples"),
        repo.join("crates/flux-flow/assets"),
    ];
    let mut scanned = Vec::new();
    let mut accepted = Vec::new();
    let mut post_cutover = Vec::new();
    for dir in dirs {
        for entry in std::fs::read_dir(&dir).expect("Flux fixture directory") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("flux") {
                continue;
            }
            let relative = path
                .strip_prefix(&repo)
                .expect("fixture under repository")
                .to_string_lossy()
                .replace('\\', "/");
            scanned.push(relative.clone());
            let src = std::fs::read_to_string(&path).expect("read fixture");
            // A few historical `.flux` fixtures intentionally store the JSON DraftAst wire shape;
            // they never entered the text parser and are outside this CST corpus.
            if src.trim_start().starts_with('{') {
                continue;
            }
            // This design example intentionally demonstrates aspirational `type` declarations and
            // multi-line call arguments that `docs/syntax.md` marks as unshipped syntax.
            if path.file_name().and_then(|name| name.to_str()) == Some("call-routing.flux") {
                continue;
            }
            // Fixtures written after the CST cutover have no legacy `parse_program` to freeze
            // evidence from, so they carry no `ast_sha256`. They still get the losslessness,
            // ERROR-free, range and format→parse contracts below via `assert_agreement(_, None)`.
            if POST_CUTOVER_FIXTURES.contains(&relative.as_str()) {
                post_cutover.push(relative);
                continue;
            }
            accepted.push(relative);
        }
    }
    scanned.sort();
    accepted.sort();
    post_cutover.sort();
    assert_eq!(scanned.len(), 17, "the shipped Flux fixture census changed");
    assert_eq!(
        post_cutover.len(),
        POST_CUTOVER_FIXTURES.len(),
        "a post-cutover fixture in the list is missing from the scanned directories"
    );
    let mut frozen_paths: Vec<_> = LEGACY_FIXTURES
        .iter()
        .map(|fixture| fixture.path.to_string())
        .collect();
    frozen_paths.sort();
    assert_eq!(
        accepted, frozen_paths,
        "the legacy-accepted fixture set changed; capture independent legacy evidence before updating it"
    );
    for fixture in LEGACY_FIXTURES {
        let src = std::fs::read_to_string(repo.join(fixture.path)).expect("read frozen fixture");
        assert_agreement(&src, fixture.path, Some(fixture));
    }
    for path in POST_CUTOVER_FIXTURES {
        let src = std::fs::read_to_string(repo.join(path)).expect("read post-cutover fixture");
        assert_agreement(&src, path, None);
    }
}

#[test]
fn native_spelling_battery_agreement() {
    // One snippet per construct family — the native spellings shipped through L-60..L-63.
    let snippets: &[&str] = &[
        // durability / cross-turn state
        "flow f\n  memo $x = read(\"a.txt\")\n  return $x\n",
        "flow f\n  once \"send\" -> $r\n    notify(\"hi\")\n  return $r\n",
        "flow f\n  checkpoint \"phase-1\"\n  return \"ok\"\n",
        "flow f\n  await $reply: String = \"reply\"\n  return $reply\n",
        "flow f\n  await \"reply\"\n  return \"ok\"\n",
        // guard rails
        "flow f\n  confirm \"Proceed?\" risk high\n    bash(\"rm -rf tmp/\")\n  return \"ok\"\n",
        "flow f\n  confirm \"Proceed?\"\n  return \"ok\"\n",
        "flow f\n  throttle \"api\" 5 per 1000\n    fetch(\"u\")\n  return \"ok\"\n",
        "flow f\n  debounce \"save\" 300\n    write(\"f\", \"x\")\n  return \"ok\"\n",
        "flow f\n  verify bash(\"cargo test\") contains \"ok\": \"tests failed\"\n  return \"ok\"\n",
        "flow f\n  verify bash(\"echo hi\") contains \"hi\"\n  return \"ok\"\n",
        // expression sugar
        "flow f(v: String) -> Number\n  $n = parse($v, as: \"f64\")\n  return $n\n",
        "flow f\n  $x = read(\"a\")\n  peek $x\n  return $x\n",
        // control flow
        "flow f\n  try\n    risky()\n  catch $e\n    log($e)\n  return \"ok\"\n",
        "flow f\n  race 5000 -> $w\n    branch $a\n      slow()\n    branch $b\n      fast()\n  return $w\n",
        "flow f\n  scope $conn = acquire_db()\n    query($conn)\n  finally\n    close($conn)\n  return \"ok\"\n",
        "flow f\n  saga\n    step\n      charge()\n    undo\n      refund()\n    step\n      ship()\n  return \"ok\"\n",
        "flow f\n  pipe -> $out\n    fetch(\"u\")\n    clean()\n    summarize()\n  return $out\n",
        // things
        "flow f\n  $t = thing person name \"john\"\n  return $t\n",
        // pre-existing kinds that ride the same grammar
        "flow f(count: Number) -> String\n  when $count > 3\n    return \"big\"\n  return \"ok\"\n",
        "flow f\n  $ok = @json {\"kind\":\"lit\",\"value\":true}\n  return $ok\n",
        "flow f\n  @effect(read)\n  $x = read(\"a.txt\")\n  return $x\n",
        "flow f\n  each $it in read_dir(\".\") -> $all\n    stat($it)\n  return $all\n",
        "flow f\n  repeat 3 -> $acc\n    until $acc\n    probe()\n  return $acc\n",
        "flow f\n  match read(\"a\")\n    case \"x\"\n      handle_x()\n    default\n      handle_other()\n  return \"ok\"\n",
        "flow f\n  ctx $pack\n    purpose \"review\"\n    budget 24000\n    include $a, $b\n  $pack += $c\n  return \"ok\"\n",
        "flow f\n  parallel\n    branch $a\n      one()\n    branch $b\n      two()\n  return $a\n",
        "flow f\n  fallback -> $r\n    branch\n      first()\n    branch\n      second()\n  return $r\n",
        "flow f\n  retry 2 backoff exponential\n    timeout 30000\n      slow()\n  return \"ok\"\n",
        "flow f\n  $s = fmt(\"hello {name}\")\n  return $s\n",
        "flow f\n  $o = { path: $p, content: \"x\" }\n  $l = [ $o, \"lit\" ]\n  return $l\n",
        // module declarations must use the same accepting grammar as flows
        "agent_loop support\n  $intent = detect_intent()\n  return $intent.intent\n\nagent guide\n  loop \"support\"\n",
        // declaration names use the full documented alphanumeric/underscore/kebab character set
        "goal \"numeric declarations\"\nflow 9lives(9arg: 9Type) -> 9Type\n  return $9arg\n",
        "op 9op(9arg: 9Type) -> 9Type\n  return $9arg\n",
    ];
    for (i, src) in snippets.iter().enumerate() {
        assert_agreement(src, &format!("battery[{i}]"), None);
    }
}
