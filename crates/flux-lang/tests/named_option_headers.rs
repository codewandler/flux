//! L-96: canonical control headers use call-like named options.
//!
//! Parameterized control headers spell their non-primary values with the same `name: value`
//! vocabulary as a named call input (`confirm "Open issue?", risk: medium`). The primary operand
//! and the `-> bind` result target stay positional, and the *structural* control-flow words
//! (`parallel`/`branch`, `match`/`case`, `try`/`catch`, `scope`/`finally`, `saga`/`step`/`undo`)
//! keep their word-and-indentation forms.
//!
//! The change is **additive**: every fixed-order space-keyword header that parsed before still
//! parses and lowers to the identical `DraftAst`. This file is the equivalence oracle for that
//! claim — each pair below must produce byte-identical ASTs — plus the proof that the formatter
//! now *emits* the canonical spelling.

use flux_lang::format::format;
use flux_lang::highlight::header_option_labels;
use flux_lang::parse::parse;
use std::collections::BTreeSet;

/// `(construct, canonical source, older spellings that must still be accepted)`.
///
/// The canonical member is what the formatter emits and a formatter fixed point; every accepted
/// spelling must lower to the identical `DraftAst`.
///
/// The rule the table encodes: the **first** operand of a header stays positional (with its
/// structural connector word — `for`, `in`), everything after it is a `name: value` option, and the
/// result target stays `-> name`. `race` therefore has nothing to name — its timeout *is* the
/// primary operand — even though `race timeout: …` is accepted, and `each` spells its only optional
/// field through the arrow it already had.
#[allow(clippy::type_complexity)]
const HEADERS: &[(&str, &str, &[&str])] = &[
    (
        "confirm",
        "flow f\n  confirm \"Proceed?\", risk: high\n    bash(\"rm -rf tmp/\")\n  return \"ok\"\n",
        &["flow f\n  confirm \"Proceed?\" risk high\n    bash(\"rm -rf tmp/\")\n  return \"ok\"\n"],
    ),
    (
        "retry",
        "flow f\n  retry 3, backoff: exponential, delay: 500ms -> out\n    fetch(\"u\")\n  return out\n",
        &["flow f\n  retry 3 backoff exponential delay 500ms -> out\n    fetch(\"u\")\n  return out\n"],
    ),
    (
        "loop",
        "flow f\n  loop for 10s, every: 1s, until: done -> last\n    poll()\n  return last\n",
        &[
            "flow f\n  loop for 10s every 1s -> last\n    until done\n    poll()\n  return last\n",
            "flow f\n  loop for 10s, every: 1s -> last\n    until done\n    poll()\n  return last\n",
        ],
    ),
    (
        "race",
        "flow f\n  race 5s -> w\n    branch a\n      slow()\n    branch b\n      fast()\n  return w\n",
        &["flow f\n  race timeout: 5s -> w\n    branch a\n      slow()\n    branch b\n      fast()\n  return w\n"],
    ),
    (
        "throttle",
        "flow f\n  throttle \"api\", max: 5, per: 1m\n    fetch(\"u\")\n  return \"ok\"\n",
        &["flow f\n  throttle \"api\" 5 per 1m\n    fetch(\"u\")\n  return \"ok\"\n"],
    ),
    (
        "debounce",
        "flow f\n  debounce \"save\", wait: 300ms\n    write(\"f\", \"x\")\n  return \"ok\"\n",
        &["flow f\n  debounce \"save\" 300ms\n    write(\"f\", \"x\")\n  return \"ok\"\n"],
    ),
    (
        "await",
        "flow f\n  await reply: String = \"reply\", when: ready\n  return reply\n",
        &["flow f\n  await reply: String = \"reply\" when ready\n  return reply\n"],
    ),
    (
        "repeat",
        "flow f\n  repeat 3, until: acc -> acc\n    probe()\n  return acc\n",
        &["flow f\n  repeat 3 -> acc\n    until acc\n    probe()\n  return acc\n"],
    ),
    (
        "each",
        "flow f\n  each x in items -> flat all\n    stat(x)\n  return all\n",
        &["flow f\n  each $x in $items -> flat $all\n    do stat $x\n  return $all\n"],
    ),
];

#[test]
fn canonical_named_option_headers_lower_to_the_legacy_ast() {
    for (construct, canonical, accepted) in HEADERS {
        let new =
            parse(canonical).unwrap_or_else(|e| panic!("`{construct}` canonical source: {e}"));
        for legacy in *accepted {
            let old = parse(legacy).unwrap_or_else(|e| panic!("`{construct}` legacy source: {e}"));
            assert_eq!(
                old, new,
                "`{construct}`: {legacy:?} must lower to the canonical AST"
            );
        }
    }
}

#[test]
fn the_formatter_emits_the_canonical_named_option_header() {
    for (construct, canonical, accepted) in HEADERS {
        // The canonical source is what the formatter must produce, header line for header line.
        let header = canonical
            .lines()
            .nth(1)
            .expect("the canonical fixture has a header on line 2")
            .trim();
        for legacy in *accepted {
            let ast = parse(legacy).unwrap_or_else(|e| panic!("`{construct}` legacy source: {e}"));
            let printed = format(&ast);
            assert!(
                printed.contains(header),
                "`{construct}`: formatter should emit `{header}`, got:\n{printed}"
            );
            assert!(
                !printed.contains("@json"),
                "`{construct}`: must stay natively spellable:\n{printed}"
            );
        }
    }
}

#[test]
fn canonical_headers_are_format_stable() {
    for (construct, canonical, _) in HEADERS {
        let ast =
            parse(canonical).unwrap_or_else(|e| panic!("`{construct}` canonical source: {e}"));
        assert_eq!(
            format(&ast),
            *canonical,
            "`{construct}`: the canonical spelling must be a formatter fixed point"
        );
    }
}

/// The shared golden fixture of `docs/designs/flux-notation-workbench.md`, verbatim — blank lines
/// and all. The comment- and order-preserving CST formatter returns `None` for a buffer that is
/// already canonical, so this is an exact round-trip, not a normalized one.
const TRIAGE: &str = r#"flow triage(ticket: Ticket) -> Answer
  kind = classify(ticket)

  parallel
    branch docs
      search(query: ticket)
    branch hits
      grep(pattern: ticket.title)

  match kind
    case "bug"
      confirm "Open issue?", risk: medium
        issue = create_issue({ ticket, hits })
        return issue
    default
      return docs
"#;

#[test]
fn the_design_triage_fixture_round_trips_exactly() {
    let ast = parse(TRIAGE).expect("the design triage fixture parses");
    assert_eq!(
        flux_lang::format_cst::format_source(TRIAGE),
        None,
        "the design triage fixture is already canonical — the CST formatter has nothing to change"
    );
    // …and the semantic projection is a fixed point on the AST: format → parse is the identity.
    let printed = format(&ast);
    assert!(
        printed.contains("      confirm \"Open issue?\", risk: medium\n"),
        "the formatter emits the canonical confirm header:\n{printed}"
    );
    assert_eq!(
        parse(&printed).expect("the formatted triage fixture parses"),
        ast,
        "format → parse must be the identity on the triage fixture:\n{printed}"
    );
    assert_eq!(format(&ast), printed, "formatting is idempotent");
}

#[test]
fn structural_headers_keep_their_word_and_indentation_forms() {
    // `parallel`/`branch`, `match`/`case`, `try`/`catch`, `scope`/`finally`, `saga`/`step`/`undo`
    // carry control flow, not options — they never grow a `name: value` tail.
    let structural = "flow f\n  parallel\n    branch a\n      one()\n    branch b\n      two()\n  try\n    risky()\n  catch e\n    log(e)\n  scope conn = acquire_db()\n    query(conn)\n  finally\n    close(conn)\n  saga\n    step\n      charge()\n    undo\n      refund()\n  match a\n    case \"x\"\n      handle_x()\n    default\n      handle_other()\n  return \"ok\"\n";
    let ast = parse(structural).expect("structural forms parse");
    assert_eq!(
        format(&ast),
        structural,
        "structural headers must stay word-and-indentation forms"
    );
}

#[test]
fn an_option_tail_on_a_header_that_has_no_options_is_rejected() {
    // `each` binds its result through `-> [flat] name`; it has nothing left to name, so an option
    // tail must fail closed rather than be silently ignored.
    for src in [
        "flow f\n  each x in items, flat: true -> all\n    stat(x)\n  return all\n",
        "flow f\n  confirm \"Proceed?\", danger: high\n  return \"ok\"\n",
        "flow f\n  retry 2, jitter: 5ms\n    flaky()\n  return \"ok\"\n",
    ] {
        assert!(
            parse(src).is_err(),
            "an unknown option tail must be a parse error: {src:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The editor-tooling mirror (C-300)
// ---------------------------------------------------------------------------

/// The website's hand-written Prism grammar, relative to the repo root.
const PRISM_GRAMMAR: &str = "website/src/theme/prism-include-languages.js";

/// The alternatives of the `keyword:` pattern in the website's Prism grammar, as a set.
///
/// Parsed structurally rather than by substring search on the whole file: `in` is a substring of
/// `include`, so `contains("in")` would pass for a keyword that is not listed. Splitting the
/// `\b(?:a|b|c)\b` alternation on `|` is the only check that distinguishes them.
fn prism_keywords() -> BTreeSet<String> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(PRISM_GRAMMAR);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let alternation = src
        .split_once("keyword:")
        .and_then(|(_, rest)| rest.split_once("\\b(?:"))
        .and_then(|(_, rest)| rest.split_once(")\\b"))
        .map(|(alts, _)| alts)
        .unwrap_or_else(|| {
            panic!(
                "{PRISM_GRAMMAR} no longer has a `keyword: /\\b(?:…)\\b/` pattern — this guard \
                 reads that shape and must be taught the new one"
            )
        });
    alternation.split('|').map(str::to_string).collect()
}

/// ⚠ **Every option label the highlighter classifies must be listed in the website's Prism
/// grammar** — and read the coverage limits below before trusting a green run.
///
/// `AGENTS.md` requires new language vocabulary to be mirrored by hand into **four** editor
/// grammars. This test reaches exactly **one** of them, the in-repo website Prism grammar. The
/// other three are separate repositories that no test in this workspace can see:
///
/// - [`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) — Helix,
///   Neovim, Zed, plus its `.helix/languages.toml`
/// - the TextMate grammar in [`codewandler/flux-editors`](https://github.com/codewandler/flux-editors)
/// - the IntelliJ grammar in the same repository
///
/// So a green run here means *"the website mirror is current"*, **not** *"the editors are
/// current"*. Those three stay a manual step with no drift guard, exactly as `AGENTS.md` warns.
///
/// The second limit: this is only as exhaustive as `HEADERS` above. A construct that grows an
/// option label without gaining a row in that table is invisible here — but such a label is also
/// missing from L-96's equivalence oracle, which is the more serious omission of the two.
#[test]
fn every_option_label_is_mirrored_in_the_website_prism_grammar() {
    let listed = prism_keywords();
    let mut missing: Vec<(&str, String)> = Vec::new();
    for (construct, canonical, _) in HEADERS {
        for label in header_option_labels(canonical) {
            if !listed.contains(&label) {
                missing.push((construct, label));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{PRISM_GRAMMAR} does not list these canonical option labels: {missing:?}\n\
         Add them to the `keyword:` alternation — and mirror them by hand into \
         flux-tree-sitter and the TextMate/IntelliJ grammars, which this test cannot reach."
    );
}

/// The guard above is worthless if `header_option_labels` returns nothing — an empty vocabulary
/// satisfies "every label is listed" vacuously. Pin the labels the corpus actually spells, so a
/// classifier change that stops recognising them fails here instead of going quiet.
#[test]
fn the_canonical_corpus_spells_the_option_labels_we_expect() {
    let found: BTreeSet<String> = HEADERS
        .iter()
        .flat_map(|(_, canonical, _)| header_option_labels(canonical))
        .collect();
    // Nine labels across the seven constructs that have options. `timeout` is absent on purpose:
    // `race timeout: 5s` is *accepted* but never emitted, because a race's timeout is its primary
    // operand (L-96). `each`'s `flat` is spelled through the arrow, not as an option.
    let expected: BTreeSet<String> = [
        "backoff", "delay", "every", "max", "per", "risk", "until", "wait", "when",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        found, expected,
        "the canonical corpus's option-label vocabulary changed; if that is intended, mirror the \
         new labels into the editor grammars and update this expectation"
    );
}

#[test]
fn a_header_until_and_a_body_until_cannot_both_be_given() {
    for src in [
        "flow f\n  repeat 3, until: a\n    until b\n    probe()\n  return \"ok\"\n",
        "flow f\n  loop for 10s, every: 1s, until: a\n    until b\n    poll()\n  return \"ok\"\n",
    ] {
        assert!(
            parse(src).is_err(),
            "`until` may be given once, in the header or the body: {src:?}"
        );
    }
}
