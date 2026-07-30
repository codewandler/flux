//! `highlight` — CST-classified highlight spans (the shared syntax-highlighting substrate).
//!
//! [`highlight`] walks the lossless CST from [`crate::parser::parse_cst`] and classifies every
//! non-whitespace token into a [`HighlightClass`] span, keyed on the token kind **and its parent
//! node's kind**: the leading `IDENT` of a [`SyntaxKind::WHEN_STMT`] is a [`HighlightClass::Keyword`],
//! the `NAME` of a `CALL_STMT` is an [`HighlightClass::Op`], the `NAME` of a `PARAM` is a
//! [`HighlightClass::Type`]. That is strictly more accurate than keyword-list string matching (the
//! lexer deliberately does not classify keywords — keywords are contextual), and it is **total**:
//! the parser is error-recovering, so invalid or incomplete source still yields spans.
//!
//! Consumers: the `flow_render` SVG source view (L-76) and, later, flux-lsp semantic tokens (L-69,
//! a thin LSP adapter over this walk). Multi-line tokens (a `"""…"""` string) come back as **one**
//! span covering all their lines — splitting at line boundaries is the consumer's job.

use crate::parser::parse_cst;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::TextRange;
use std::collections::BTreeSet;

/// The visual class of one source token, named for what the token *is*, not for a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightClass {
    /// A contextual keyword: the leading ident of a statement/clause/arm (`flow`, `when`, `do`,
    /// `case`, …) and the fixed interior words of a form (`in`, `until`, `contains`, …).
    Keyword,
    /// An operation or callable name: op names after `do`, call names (`fmt(…)`), flow names.
    Op,
    /// A `$symbol` reference (and the binder positions that become symbols: parameter names,
    /// field-access path segments).
    Var,
    /// An `@annotation` (`@effect`, `@json`) and the tag inside `@effect(tag)`.
    Annotation,
    /// A `"…"` or `"""…"""` string literal (one span, even across lines).
    String,
    /// A numeric literal — and the literal idents `true`/`false`/`null` (the `lit` role).
    Number,
    /// A `# …` line comment.
    Comment,
    /// Punctuation, operators, and any ident with no more specific classification.
    Punct,
    /// A type name: parameter/bind/return-type annotations and a `thing`-selector kind.
    Type,
    /// A token the lexer could not classify or the parser wrapped in an `ERROR` node.
    Error,
}

/// Classify every token of `src` into `(range, class)` spans, in source order.
///
/// Total: never panics and never fails — malformed source still parses to a lossless CST, so every
/// non-whitespace token still gets a span. Whitespace and line breaks are skipped (they have no
/// class); everything else is covered.
pub fn highlight(src: &str) -> Vec<(TextRange, HighlightClass)> {
    parse_cst(src)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter_map(|tok| classify(&tok).map(|class| (tok.text_range(), class)))
        .collect()
}

/// The class of one token, or `None` for whitespace/line breaks.
fn classify(tok: &SyntaxToken) -> Option<HighlightClass> {
    use HighlightClass as C;
    use SyntaxKind as K;
    Some(match tok.kind() {
        K::WHITESPACE | K::NEWLINE => return None,
        K::COMMENT => C::Comment,
        K::STRING => C::String,
        K::NUMBER => C::Number,
        K::VAR => C::Var,
        K::ANNOTATION => C::Annotation,
        K::ERROR => C::Error,
        K::IDENT => classify_ident(tok),
        // Everything else a token can be is punctuation or an operator.
        _ => C::Punct,
    })
}

/// An `IDENT` is the context-dependent case: keywords, op names, type names, and literals are all
/// lexed as plain idents, so the parent node kind (and the token's position in it) decides.
fn classify_ident(tok: &SyntaxToken) -> HighlightClass {
    use HighlightClass as C;
    use SyntaxKind as K;
    let Some(parent) = tok.parent() else {
        return C::Punct;
    };
    let pk = parent.kind();
    if pk == K::ERROR {
        return C::Error;
    }
    if keyword_leads(pk) && is_leading(tok, &parent) {
        return C::Keyword;
    }
    if matches!(tok.text(), "true" | "false" | "null") {
        return C::Number; // literal idents (the `lit` role)
    }
    match pk {
        K::NAME => name_class(&parent),
        K::PARAM => C::Var,               // the `name` in `name: Type`
        K::FLOW_HEADER => C::Op,          // the flow's own name (kebab-case segments included)
        K::FIELD_EXPR => C::Var,          // `$sym.path` — the path reads as part of the variable
        K::EFFECT_ANNOT => C::Annotation, // the tag in `@effect(tag)`
        K::THING_EXPR => thing_class(tok, &parent),
        // `, risk: medium` — the label of a canonical header option reads as a keyword of the
        // form, exactly like the space-keyword spelling it replaces (L-96).
        K::HEADER_OPTION => option_class(tok, &parent),
        K::EACH_STMT if matches!(tok.text(), "in" | "flat") => C::Keyword,
        K::LOOP_STMT if matches!(tok.text(), "for" | "every") => C::Keyword,
        K::RETRY_STMT if tok.text() == "backoff" => C::Keyword,
        K::CONFIRM_STMT if tok.text() == "risk" => C::Keyword,
        K::THROTTLE_STMT if tok.text() == "per" => C::Keyword,
        K::SCOPE_STMT if tok.text() == "acquire" => C::Keyword,
        K::VERIFY_STMT if tok.text() == "contains" => C::Keyword,
        // `verify <cmd> …` names an op; `with_tools read, write` names tools.
        K::VERIFY_STMT | K::WITH_TOOLS_STMT => C::Op,
        _ => C::Punct,
    }
}

/// The class of an ident inside a `NAME` node, decided by where that `NAME` sits.
fn name_class(name: &SyntaxNode) -> HighlightClass {
    use HighlightClass as C;
    use SyntaxKind as K;
    let Some(parent) = name.parent() else {
        return C::Op;
    };
    match parent.kind() {
        // `type_ref` positions: a parameter's type, a bind/memo annotation, the return type after
        // `->`, and the nested argument of `List<T>` (a NAME directly inside a NAME).
        K::PARAM | K::FLOW_HEADER | K::BIND_STMT | K::MEMO_STMT | K::NAME => C::Type,
        // Labels: object keys and named-argument keys (`as:` in `parse(v, as: "f64")`). A bare
        // expression name in an arg list is a reference, not a label.
        K::OBJ_FIELD | K::NAMED_ARG => C::Punct,
        K::ARG_LIST if followed_by_colon(name) => C::Punct,
        // Op names: `do <op>`, `op(args)`, `fmt(…)`/`parse(…)`, and bare references.
        _ => C::Op,
    }
}

/// Inside a `HEADER_OPTION` the *label* is the form's keyword (`risk`, `backoff`, `until`); a bare
/// ident in value position (`backoff: exponential`) is ordinary punctuation, as it was in the
/// space-keyword spelling.
fn option_class(tok: &SyntaxToken, parent: &SyntaxNode) -> HighlightClass {
    let label = parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT);
    if label.as_ref() == Some(tok) {
        HighlightClass::Keyword
    } else {
        HighlightClass::Punct
    }
}

/// `thing <kind> [custom-kind-str] <selector> "<value>"`: the kind ident (the second one) is the
/// Type; the leading `thing` and the selector words are keywords of the form.
fn thing_class(tok: &SyntaxToken, parent: &SyntaxNode) -> HighlightClass {
    let idx = parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT)
        .position(|t| &t == tok);
    match idx {
        Some(1) => HighlightClass::Type,
        _ => HighlightClass::Keyword,
    }
}

/// Is `tok` the first non-trivia child of `parent`? (The leading position is where statement
/// keywords live; a wrapped expression statement leads with a *node*, so its tokens never match.)
fn is_leading(tok: &SyntaxToken, parent: &SyntaxNode) -> bool {
    parent
        .children_with_tokens()
        .find(|el| !el.kind().is_trivia())
        .is_some_and(|el| el.as_token() == Some(tok))
}

/// Node kinds whose *leading* `IDENT` is a contextual keyword (`flow`, `when`, `do`, `case`,
/// `purpose`, …). Expression wrappers and `NAME` are deliberately absent.
fn keyword_leads(kind: SyntaxKind) -> bool {
    use SyntaxKind as K;
    matches!(
        kind,
        K::FLOW_HEADER
            | K::DECL
            | K::CTX_ENTRY
            | K::CALL_STMT
            | K::WHEN_STMT
            | K::ELSE_CLAUSE
            | K::UNLESS_STMT
            | K::EACH_STMT
            | K::REPEAT_STMT
            | K::UNTIL_CLAUSE
            | K::MATCH_STMT
            | K::CASE_ARM
            | K::DEFAULT_ARM
            | K::ROUTE_STMT
            | K::FALLBACK_STMT
            | K::BRANCH_ARM
            | K::PARALLEL_STMT
            | K::LOOP_STMT
            | K::TIMEOUT_STMT
            | K::BUDGET_STMT
            | K::WITH_TOOLS_STMT
            | K::RETRY_STMT
            | K::SEQ_STMT
            | K::CTX_STMT
            | K::RETURN_STMT
            | K::ASSERT_STMT
            | K::MEMO_STMT
            | K::ONCE_STMT
            | K::CHECKPOINT_STMT
            | K::AWAIT_STMT
            | K::CONFIRM_STMT
            | K::THROTTLE_STMT
            | K::DEBOUNCE_STMT
            | K::VERIFY_STMT
            | K::TRY_STMT
            | K::CATCH_CLAUSE
            | K::RACE_STMT
            | K::SCOPE_STMT
            | K::FINALLY_CLAUSE
            | K::SAGA_STMT
            | K::STEP_ARM
            | K::UNDO_CLAUSE
            | K::PIPE_STMT
            | K::PEEK_EXPR
    )
}

/// Every canonical header-option label (L-96) that `src` contains, as **the highlighter itself**
/// classifies it: a token inside a [`SyntaxKind::HEADER_OPTION`] that [`option_class`] calls a
/// [`HighlightClass::Keyword`].
///
/// This is the option-label vocabulary the editor grammars owe a mirror, and it is derived by
/// running the classifier rather than by restating its rule — a second copy of "the label is the
/// first `IDENT`" would agree with itself, not with the highlighter. Consumer:
/// `tests/named_option_headers.rs`, which asserts the website's Prism grammar lists every label
/// the canonical corpus spells.
pub fn header_option_labels(src: &str) -> BTreeSet<String> {
    let mut labels = BTreeSet::new();
    for node in parse_cst(src).syntax().descendants() {
        if node.kind() != SyntaxKind::HEADER_OPTION {
            continue;
        }
        for tok in node.children_with_tokens().filter_map(|el| el.into_token()) {
            if option_class(&tok, &node) == HighlightClass::Keyword {
                labels.insert(tok.text().to_string());
            }
        }
    }
    labels
}

/// Is the next non-trivia sibling of `name` a `:`? (Distinguishes a named-arg label from a bare
/// expression name inside an `ARG_LIST`.)
fn followed_by_colon(name: &SyntaxNode) -> bool {
    let mut sib = name.next_sibling_or_token();
    while let Some(el) = sib {
        if el.kind().is_trivia() {
            sib = el.next_sibling_or_token();
        } else {
            return el.kind() == SyntaxKind::COLON;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spans as `(text, class)` pairs, for readable assertions.
    fn classes(src: &str) -> Vec<(String, HighlightClass)> {
        highlight(src)
            .into_iter()
            .map(|(r, c)| (src[r].to_string(), c))
            .collect()
    }

    /// The class of the first span whose text equals `text`.
    fn class_of(src: &str, text: &str) -> HighlightClass {
        classes(src)
            .into_iter()
            .find(|(t, _)| t == text)
            .map(|(_, c)| c)
            .unwrap_or_else(|| panic!("no span with text {text:?}"))
    }

    /// Every non-whitespace byte of `src` is covered by exactly one span, in ascending order.
    fn assert_total(src: &str) {
        let spans = highlight(src);
        for w in spans.windows(2) {
            assert!(
                w[0].0.end() <= w[1].0.start(),
                "spans must be ordered and non-overlapping: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
        let mut covered = vec![false; src.len()];
        for (r, _) in &spans {
            covered[usize::from(r.start())..usize::from(r.end())].fill(true);
        }
        for (i, b) in src.bytes().enumerate() {
            if !covered[i] {
                assert!(
                    matches!(b, b' ' | b'\t' | b'\n' | b'\r'),
                    "uncovered non-whitespace byte at {i}: {:?}",
                    b as char
                );
            }
        }
    }

    #[test]
    fn keywords_ops_vars_annotations_classify_by_context() {
        let src = "flow greet(name: String) -> String\n  @effect(net)\n  $g = fmt(\"hi {name}\")\n  when $g != \"\"\n    do notify \"hey\"\n  return $g\n";
        assert_eq!(class_of(src, "flow"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "when"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "do"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "return"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "greet"), HighlightClass::Op);
        assert_eq!(class_of(src, "fmt"), HighlightClass::Op);
        assert_eq!(class_of(src, "notify"), HighlightClass::Op);
        assert_eq!(class_of(src, "$g"), HighlightClass::Var);
        assert_eq!(class_of(src, "name"), HighlightClass::Var);
        assert_eq!(class_of(src, "@effect"), HighlightClass::Annotation);
        assert_eq!(class_of(src, "net"), HighlightClass::Annotation);
        assert_eq!(class_of(src, "String"), HighlightClass::Type);
        assert_eq!(class_of(src, "\"hey\""), HighlightClass::String);
        assert_eq!(class_of(src, "->"), HighlightClass::Punct);
        assert_total(src);
    }

    #[test]
    fn literals_and_comments_classify_as_themselves() {
        let src = "flow f\n  # a comment\n  $n = 42\n  $b = true\n  return $n\n";
        assert_eq!(class_of(src, "# a comment"), HighlightClass::Comment);
        assert_eq!(class_of(src, "42"), HighlightClass::Number);
        assert_eq!(class_of(src, "true"), HighlightClass::Number);
        assert_total(src);
    }

    #[test]
    fn canonical_header_option_labels_are_keywords() {
        // L-96: `, risk: medium` must read exactly like the `risk medium` it replaces — the label
        // is a keyword of the form, the value is not.
        let src = "flow f\n  confirm \"go?\", risk: high\n    retry 2, backoff: exponential\n      flaky()\n  return \"ok\"\n";
        assert_eq!(class_of(src, "confirm"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "risk"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "backoff"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "exponential"), HighlightClass::Punct);
        assert_total(src);
    }

    #[test]
    fn thing_selector_kind_is_a_type() {
        let src = "flow f\n  $x = thing person name \"john\"\n  return $x\n";
        assert_eq!(class_of(src, "thing"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "person"), HighlightClass::Type);
        assert_eq!(class_of(src, "name"), HighlightClass::Keyword);
        assert_total(src);
    }

    #[test]
    fn total_on_malformed_source() {
        // A torn bind, an unclosed call, an unlexable char, and a truncated flow: the walk must
        // still produce spans (never panic), and the junk classifies as Error.
        let src = "flow f\n  $a =\n  do read(\n  € oops\n  $b = 2";
        let spans = highlight(src);
        assert!(!spans.is_empty(), "malformed source must still yield spans");
        assert_eq!(class_of(src, "€"), HighlightClass::Error);
        assert_total(src);
    }

    #[test]
    fn triple_string_is_one_span_across_lines() {
        let src = "flow f\n  $x = \"\"\"line one\nline two\"\"\"\n  return $x\n";
        let (text, class) = classes(src)
            .into_iter()
            .find(|(t, _)| t.starts_with("\"\"\""))
            .expect("triple string span");
        assert_eq!(class, HighlightClass::String);
        assert!(
            text.contains('\n'),
            "one span must cover all lines of a multi-line string, got {text:?}"
        );
        assert_total(src);
    }

    #[test]
    fn types_in_binds_and_generics() {
        let src = "flow f(xs: List<Number>) -> Number\n  $t: Number = 1\n  memo $m: String = \"a\"\n  return $t\n";
        assert_eq!(class_of(src, "List"), HighlightClass::Type);
        assert_eq!(class_of(src, "Number"), HighlightClass::Type);
        assert_eq!(class_of(src, "String"), HighlightClass::Type);
        assert_eq!(class_of(src, "memo"), HighlightClass::Keyword);
        assert_total(src);
    }

    #[test]
    fn interior_form_keywords_and_labels() {
        let src = "flow f\n  each $it in $items -> flat $all\n    do process $it\n  $v = parse($all, as: \"f64\")\n  $o = { k: $v }\n  return $o\n";
        assert_eq!(class_of(src, "each"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "in"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "flat"), HighlightClass::Keyword);
        assert_eq!(class_of(src, "parse"), HighlightClass::Op);
        assert_eq!(class_of(src, "as"), HighlightClass::Punct); // named-arg label
        assert_eq!(class_of(src, "k"), HighlightClass::Punct); // object key
        assert_total(src);
    }
}
