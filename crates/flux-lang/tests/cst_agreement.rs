//! L-59 acceptance-agreement guard: every source the legacy strict parser accepts must produce an
//! ERROR-free CST from the tolerant parser. The CST front-end (`lower_cst`) treats the legacy
//! line machinery as the semantic authority, so the only way the CST can silently rot is by
//! rejecting (ERROR-noding) text the language accepts — this test makes that a hard failure.
//!
//! Two sources of truth are swept: the repo's `examples/*.flux` corpus and an inline battery
//! covering every node kind's native spelling (mirrors `docs/syntax.md`).

use flux_lang::parser::parse_cst;

/// Assert: if the legacy parser accepts `src`, the CST parse has no errors and no ERROR nodes.
fn assert_agreement(src: &str, what: &str) {
    let legacy_flow = flux_lang::parse::parse(src);
    let legacy_program = flux_lang::parse::parse_program(src);
    if legacy_flow.is_err() && legacy_program.is_err() {
        return; // legacy rejects — the CST may do anything (tolerant by design)
    }
    let parsed = parse_cst(src);
    let errors = &parsed.errors;
    assert!(
        errors.is_empty(),
        "{what}: legacy accepts but the CST parser reported {} error(s): {:?}\nsource:\n{src}",
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
        "{what}: legacy accepts but the CST contains ERROR node(s): {error_nodes:?}\nsource:\n{src}"
    );
}

#[test]
fn examples_corpus_agreement() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("examples dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("flux") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read example");
        assert_agreement(&src, &format!("examples/{:?}", path.file_name().unwrap()));
        checked += 1;
    }
    assert!(checked >= 3, "expected to sweep the examples corpus");
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
    ];
    for (i, src) in snippets.iter().enumerate() {
        assert_agreement(src, &format!("battery[{i}]"));
    }
}
