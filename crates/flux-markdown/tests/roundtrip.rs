//! Round-trip stability (L-02): `parse(&to_markdown(&parse(src))) == parse(src)` over a corpus of
//! real, committed flux docs/skills (snapshotted under `tests/corpus/`) plus a synthetic
//! kitchen-sink document. The corpus files carry the constructs the engine supports; the parser
//! docs list what is deliberately NOT parsed.

use flux_markdown::parser::parse;
use flux_markdown::writer::to_markdown;

fn corpus() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "md") {
            let raw = std::fs::read_to_string(&path).unwrap();
            // Feed the markdown body; frontmatter is the other half of this crate.
            let (_, body) = flux_markdown::split_frontmatter(&raw);
            out.push((path.display().to_string(), body.to_string()));
        }
    }
    assert!(out.len() >= 5, "corpus present: {out:?}");
    out
}

#[test]
fn corpus_round_trips_through_the_writer() {
    for (name, src) in corpus() {
        let a = parse(&src);
        let written = to_markdown(&a);
        let b = parse(&written);
        assert_eq!(a, b, "round-trip drift in {name}");
    }
}

#[test]
fn kitchen_sink_round_trips() {
    let src = "\
# Top *heading* with `code`

A paragraph with **strong**, *emphasis*, ~~strike~~, [a link](https://e.com/x \"Title\"),
an ![image](img.png), an autolink <https://auto.link>, and a hard break.\\
Second line after `` a ` tricky `` span.

## Lists

- tight one
- tight two
  - nested child
  - another `child`
- tight three

1. loose first

2. loose second

   with a follow-up paragraph

> A quote with **style**
>
> - and a quoted list
> - of two items

```rust
fn main() { println!(\"| not a table |\"); }
```

| Col A | Col B | Col C |
|:------|:-----:|------:|
| left  | mid   | right |
| *em*  | `c`   | plain |

---

Literal specials: a\\*b a\\_b \\[x\\] \\`t\\` & ampersand < angle.
";
    let a = parse(src);
    let written = to_markdown(&a);
    let b = parse(&written);
    assert_eq!(a, b, "kitchen sink drift via:\n{written}");
}

/// Double round-trip: the writer's own output is a fixed point (canonical form).
#[test]
fn writer_output_is_a_fixed_point() {
    for (name, src) in corpus() {
        let once = to_markdown(&parse(&src));
        let twice = to_markdown(&parse(&once));
        assert_eq!(once, twice, "writer not canonical for {name}");
    }
}
