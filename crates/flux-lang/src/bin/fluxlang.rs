//! `fluxlang` — the Flux-Lang command-line surface.
//!
//! Inspect the language without the engine: print its skill, its JSON Schema, render a JSON AST as a
//! human-readable tree, or `compile` the round-trippable text syntax into a JSON AST (over
//! `flux_lang::parse`). Note `render` is intentionally one-way (a lossy display tree), distinct from
//! `compile`/`format`/`parse`.
//!
//! Built only with `--features cli` (keeps `clap` off the library's dependency graph).

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use flux_core::{Error, Result};
use flux_lang::ast::DraftAst;
use flux_lang::render::Palette;

#[derive(Parser)]
#[command(
    name = "fluxlang",
    about = "Flux-Lang — the typed execution-graph language for LLMs",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the Flux-Lang language skill (markdown).
    Skill,
    /// Print the JSON Schema of the Flux-Lang AST.
    Schema {
        /// Print a compact merged schema — one `Node` object (`kind` enum + the union of every
        /// kind's properties) — instead of the strict per-kind union. Useful for language tooling
        /// and compatibility analysis; agent models do not generate executable ASTs.
        #[arg(long)]
        merged: bool,
    },
    /// Render a JSON AST (from FILE, or stdin when omitted) as a human-readable tree.
    Render {
        /// Path to a JSON AST file; reads stdin when omitted.
        file: Option<PathBuf>,
    },
    /// Compile Flux-Lang text (from FILE, or stdin when omitted) into a JSON AST.
    Compile {
        /// Path to a Flux-Lang text file; reads stdin when omitted.
        file: Option<PathBuf>,
    },
    /// Render Flux-Lang text (from FILE, or stdin when omitted) as a Railflux ASCII dataflow diagram.
    Rail {
        /// Path to a Flux-Lang text file; reads stdin when omitted.
        file: Option<PathBuf>,
    },
    /// Project canonical Flux-Lang text (from FILE, or stdin when omitted) as Flux Glyph.
    Glyph {
        /// Path to a Flux-Lang text file; reads stdin when omitted.
        file: Option<PathBuf>,
    },
    /// Expand Flux **Glyph** (from FILE, or stdin when omitted) back to canonical Flux-Lang text.
    Unglyph {
        /// Path to a Flux Glyph file; reads stdin when omitted.
        file: Option<PathBuf>,
    },
    /// Rewrite Flux-Lang source in the canonical dialect, comments and declaration order intact.
    ///
    /// Rewrites each FILE in place; with no FILE, reads stdin and writes stdout.
    Fmt {
        /// Files to rewrite in place; reads stdin and writes stdout when omitted.
        files: Vec<PathBuf>,
        /// Report what would change instead of writing it, and exit non-zero if anything would.
        #[arg(long)]
        check: bool,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fluxlang: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let out = match Cli::parse().command {
        Command::Skill => flux_lang::skill::render(),
        Command::Schema { merged } => {
            let schema = if merged {
                flux_lang::schema::model_schema()
            } else {
                flux_lang::schema::ast_schema()
            };
            serde_json::to_string_pretty(&schema).map_err(|e| Error::Other(e.to_string()))?
        }
        Command::Render { file } => render_ast(file)?,
        Command::Compile { file } => compile_text(file)?,
        Command::Rail { file } => rail_text(file)?,
        Command::Glyph { file } => glyph_src(&read_source(file)?)?,
        Command::Unglyph { file } => unglyph_src(&read_source(file)?)?,
        // `fmt` writes files and owns its own exit code, so it does not join the print-one-string
        // path the projections share.
        Command::Fmt { files, check } => return fmt(&files, check),
    };
    let mut stdout = std::io::stdout();
    stdout
        .write_all(out.as_bytes())
        .map_err(|e| Error::Other(e.to_string()))?;
    if !out.ends_with('\n') {
        let _ = stdout.write_all(b"\n");
    }
    Ok(())
}

/// Read the contents of `file`, or stdin when omitted.
fn read_source(file: Option<PathBuf>) -> Result<String> {
    match file {
        Some(p) => std::fs::read_to_string(&p)
            .map_err(|e| Error::Other(format!("read {}: {e}", p.display()))),
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| Error::Other(e.to_string()))?;
            Ok(s)
        }
    }
}

/// Read a JSON `DraftAst` from `file` (or stdin) and render it as a tree — colored on a TTY, plain
/// otherwise (so piped output stays clean).
fn render_ast(file: Option<PathBuf>) -> Result<String> {
    let raw = read_source(file)?;
    let ast: DraftAst =
        serde_json::from_str(&raw).map_err(|e| Error::Other(format!("invalid AST JSON: {e}")))?;
    Ok(if std::io::stdout().is_terminal() {
        flux_lang::render::render_styled(&ast, &ANSI)
    } else {
        flux_lang::render::render_pretty(&ast)
    })
}

/// Compile Flux-Lang text to a pretty-JSON AST. Parses via the **same module entry** `flux flow run`
/// uses (`Module::parse_str` → `parse_program`), so a module whose first declaration is an `op` (or
/// any program declaration) compiles here too — not only in the runner (F-013). A bare flow
/// serializes to its `DraftAst` (byte-identical to the old flow-only path, preserving the
/// `compile(format(ast))` round-trip); a program serializes to its `Program`.
fn compile_src(src: &str) -> Result<String> {
    match flux_lang::program::Module::parse_str(src)
        .map_err(|e| Error::Other(format!("parse error: {e}")))?
    {
        flux_lang::program::Module::Flow(ast) => {
            serde_json::to_string_pretty(&ast).map_err(|e| Error::Other(e.to_string()))
        }
        flux_lang::program::Module::Program(prog) => {
            serde_json::to_string_pretty(&prog).map_err(|e| Error::Other(e.to_string()))
        }
    }
}

/// Read Flux-Lang text from `file` (or stdin), parse it, and emit the AST as pretty JSON. The
/// inverse of `format` — `compile(format(ast))` round-trips back to the same AST.
fn compile_text(file: Option<PathBuf>) -> Result<String> {
    let src = read_source(file)?;
    compile_src(&src)
}

/// Project canonical Flux source as **Railflux** — the 7-bit ASCII dataflow diagram (L-95). Shares
/// `compile`'s module parse entry, so malformed source reports exactly the same parser diagnostic
/// here as it does there; this subcommand is output-only and never reads Railflux back.
///
/// A module that is a program has no single flow, so every top-level flow and journey flow it
/// declares is rendered in declaration order, blank-line separated. Composite `op` declarations are
/// operations rather than flows and are not projected.
fn rail_src(src: &str) -> Result<String> {
    let diagrams: Vec<String> = match flux_lang::program::Module::parse_str(src)
        .map_err(|e| Error::Other(format!("parse error: {e}")))?
    {
        flux_lang::program::Module::Flow(ast) => vec![rail_one(&ast)],
        flux_lang::program::Module::Program(prog) => prog
            .flows
            .iter()
            .chain(prog.journeys.iter().map(|j| &j.flow))
            .map(rail_one)
            .collect(),
    };
    if diagrams.is_empty() {
        return Err(Error::Other(
            "no flow to render: the module declares no top-level flow or journey".to_string(),
        ));
    }
    Ok(diagrams.join("\n"))
}

/// One flow's diagram — colored on a TTY, plain otherwise (so piped output stays byte-canonical).
fn rail_one(ast: &DraftAst) -> String {
    if std::io::stdout().is_terminal() {
        flux_lang::render::render_rail_styled(ast, &ANSI)
    } else {
        flux_lang::render::render_rail(ast)
    }
}

fn rail_text(file: Option<PathBuf>) -> Result<String> {
    let src = read_source(file)?;
    rail_src(&src)
}

/// Project canonical Flux source as **Flux Glyph** (L-97) — the compact indented opcode notation.
/// Shares `compile`'s parse entry, so malformed source reports the same diagnostic here.
///
/// Glyph is a flow projection, so a program module is projected one flow per document, blank-line
/// separated, exactly as `rail` does.
fn glyph_src(src: &str) -> Result<String> {
    let documents: Vec<String> = match flux_lang::program::Module::parse_str(src)
        .map_err(|e| Error::Other(format!("parse error: {e}")))?
    {
        flux_lang::program::Module::Flow(ast) => vec![flux_lang::glyph::format_glyph(&ast)],
        flux_lang::program::Module::Program(prog) => prog
            .flows
            .iter()
            .chain(prog.journeys.iter().map(|j| &j.flow))
            .map(flux_lang::glyph::format_glyph)
            .collect(),
    };
    if documents.is_empty() {
        return Err(Error::Other(
            "no flow to project: the module declares no top-level flow or journey".to_string(),
        ));
    }
    Ok(documents.join("\n"))
}

/// Expand a **Glyph** document back to canonical Flux source. The notation is never sniffed: this
/// subcommand *is* the explicit declaration that the input is Glyph, and it goes through the AST —
/// so what it prints is canonical `format` output, not a textual rewrite.
fn unglyph_src(src: &str) -> Result<String> {
    let ast = flux_lang::glyph::parse_glyph(src)
        .map_err(|e| Error::Other(format!("glyph parse error: {e}")))?;
    Ok(flux_lang::format::format(&ast))
}

/// `fmt` — canonicalize `.flux` source (L-103).
///
/// With no `files`, this is a filter: stdin in, canonical source out. With files, each is rewritten
/// **in place**, and only when it actually changes (so an already-canonical tree keeps its mtimes
/// and a `fmt` run is safe to put in a pre-commit hook).
///
/// `--check` writes nothing and exits non-zero if any input is not already canonical, printing a
/// per-file diff summary — the CI shape.
///
/// A file that does not parse, or whose rewrite the equivalence guard refuses, is an error in both
/// modes rather than a silent skip: the whole point of the command is that its output is trustworthy.
///
/// One bad file does **not** end the run. `fmt` is meant to be pointed at a whole tree, and a batch
/// that stops at the first problem hides every file behind it — the operator fixes one thing, re-runs,
/// and discovers the next. Each file is reported and the run continues; the exit code is non-zero if
/// anything failed or, under `--check`, if anything was stale.
fn fmt(files: &[PathBuf], check: bool) -> Result<()> {
    if files.is_empty() {
        return fmt_stdin(check);
    }
    let (mut stale, mut failed) = (Vec::new(), Vec::new());
    for path in files {
        let label = path.display().to_string();
        let outcome = std::fs::read_to_string(path)
            .map_err(|e| Error::Other(format!("read {label}: {e}")))
            .and_then(|src| {
                let canonical = canonical_text(&src, &label)?;
                if canonical == src {
                    return Ok(());
                }
                if check {
                    eprintln!("{label}: not canonical\n{}", diff_summary(&src, &canonical));
                    stale.push(label.clone());
                    return Ok(());
                }
                std::fs::write(path, &canonical)
                    .map_err(|e| Error::Other(format!("write {label}: {e}")))
            });
        if let Err(e) = outcome {
            eprintln!("fluxlang: {e}");
            failed.push(label);
        }
    }
    match (failed.is_empty(), stale.is_empty()) {
        (true, true) => Ok(()),
        (false, _) => Err(Error::Other(format!(
            "{} file(s) could not be formatted: {}",
            failed.len(),
            failed.join(", ")
        ))),
        (true, false) => Err(Error::Other(format!(
            "{} file(s) are not canonical: {}",
            stale.len(),
            stale.join(", ")
        ))),
    }
}

/// The filter form: stdin in, canonical source on stdout. Under `--check` nothing is written and a
/// non-canonical input is the error.
fn fmt_stdin(check: bool) -> Result<()> {
    let src = read_source(None)?;
    let canonical = canonical_text(&src, "<stdin>")?;
    if check {
        if canonical == src {
            return Ok(());
        }
        eprintln!("<stdin>: not canonical\n{}", diff_summary(&src, &canonical));
        return Err(Error::Other("input is not canonical".to_string()));
    }
    std::io::stdout()
        .write_all(canonical.as_bytes())
        .map_err(|e| Error::Other(e.to_string()))
}

/// The canonical form of `src`, or the reason there isn't one.
///
/// [`Canonical::Rejected`] is deliberately loud. It means the source parsed but the rewrite could
/// not be proven to lower to the same module and carry the same comments — a defect in the
/// canonicalizer, not in the file, and the last thing that should happen is for `fmt` to shrug and
/// report success.
fn canonical_text(src: &str, label: &str) -> Result<String> {
    use flux_lang::canonicalize::Canonical;
    match flux_lang::canonicalize::canonicalize_source(src) {
        Canonical::Unchanged => Ok(src.to_string()),
        Canonical::Rewritten(out) => Ok(out),
        Canonical::Unparsed => Err(Error::Other(format!(
            "{label}: parse error: not valid Flux-Lang source"
        ))),
        Canonical::Rejected => Err(Error::Other(format!(
            "{label}: the canonical rewrite failed its equivalence guard; the file was left alone"
        ))),
    }
}

/// A compact line diff for `--check`: the changed region only, `-` for the file, `+` for canonical.
///
/// Common leading and trailing lines are trimmed so the summary points at the edit rather than
/// reprinting the flow. Each side gets its **own** budget: a single shared cap would be spent
/// entirely on removals for any change bigger than a few lines, and a diff that never reaches the
/// `+` side does not tell the reader what to do about it.
fn diff_summary(before: &str, after: &str) -> String {
    const MAX_LINES_PER_SIDE: usize = 8;
    let (old, new): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = String::new();
    for (sign, lines) in [
        ('-', &old[head..old.len() - tail]),
        ('+', &new[head..new.len() - tail]),
    ] {
        for line in lines.iter().take(MAX_LINES_PER_SIDE) {
            out.push_str(&format!("  {sign}{line}\n"));
        }
        if lines.len() > MAX_LINES_PER_SIDE {
            out.push_str(&format!(
                "  {sign}… and {} more line(s)\n",
                lines.len() - MAX_LINES_PER_SIDE
            ));
        }
    }
    out
}

/// A small ANSI palette for terminal rendering.
const ANSI: Palette = Palette {
    keyword: ("\x1b[1;35m", "\x1b[0m"),
    op: ("\x1b[1;36m", "\x1b[0m"),
    symbol: ("\x1b[33m", "\x1b[0m"),
    string: ("\x1b[32m", "\x1b[0m"),
    lit: ("\x1b[32m", "\x1b[0m"),
    effect: ("\x1b[90m", "\x1b[0m"),
    connector: ("\x1b[90m", "\x1b[0m"),
    thing: ("\x1b[34m", "\x1b[0m"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_json_ast() {
        let json =
            r#"{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"README.md"}]}]}"#;
        let tree = render_ast_str(json).unwrap();
        assert!(tree.contains("read"));
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(render_ast_str("{ not json").is_err());
    }

    #[test]
    fn compiles_text_back_to_a_json_ast() {
        // Build an AST, format it to text, then compile that text back to JSON: the op survives the
        // round-trip (the deep `parse(format(ast)) == ast` guarantee is tested in flux-lang itself).
        let json_in =
            r#"{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"README.md"}]}]}"#;
        let ast: DraftAst = serde_json::from_str(json_in).unwrap();
        let text = flux_lang::format::format(&ast);
        let json_out = compile_str(&text).unwrap();
        assert!(json_out.contains("read"));
        assert!(json_out.contains("README.md"));
    }

    #[test]
    fn rejects_unparseable_text() {
        assert!(compile_str("= = = not flux = = =").is_err());
    }

    #[test]
    fn compiles_a_module_with_a_leading_op() {
        // A module whose first declaration is `op` compiles here just as `flux flow run` executes it.
        // The dev CLI used to reject it because `compile` parsed flow-only text (F-013); it now shares
        // the module parse entry with the runner.
        let src = "op noop() -> string\n  return \"ok\"\n";
        let json = compile_str(src).unwrap();
        assert!(json.contains("noop"), "op survived the compile: {json}");
    }

    #[test]
    fn rails_canonical_flux_source() {
        // L-95: `rail` takes *source*, not a JSON AST, and projects it as the dataflow diagram.
        let src = "flow triage(ticket: Ticket)\n  kind = classify(ticket)\n  return kind\n";
        assert_eq!(
            rail_src(src).unwrap(),
            "[flow triage (ticket: Ticket)]\n  ticket --> classify(.) --> kind\n  kind --> RETURN\n"
        );
    }

    #[test]
    fn rail_reports_the_existing_parser_diagnostics() {
        // Malformed source must fail with the same diagnostic `compile` reports — one parse entry,
        // one error vocabulary.
        //
        // The fixture is malformed **lexically** (an unterminated string literal), deliberately: the
        // previous one spelled a statement the parser rejected at the time (`confirm "y", risk:
        // high`), and L-96 then made that exact spelling canonical — so the fixture quietly became
        // valid and this test's `expect_err` started panicking (C-308). A vocabulary the language
        // can never grow into keeps the fixture malformed across syntax work.
        let bad = "flow x\n  confirm \"y\n";
        let rail = rail_src(bad)
            .expect_err("malformed flux must not render")
            .to_string();
        let compile = compile_src(bad)
            .expect_err("malformed flux must not compile")
            .to_string();
        assert_eq!(rail, compile);
        assert!(rail.contains("parse error"), "got: {rail}");
    }

    #[test]
    fn rails_every_flow_of_a_program_module() {
        // A program has no single flow; each top-level flow and journey flow is projected in
        // declaration order rather than the command refusing the module outright.
        let src =
            "agent helper\n  model \"mock\"\n\nflow first\n  return 1\n\nflow second\n  return 2\n";
        let out = rail_src(src).unwrap();
        assert_eq!(
            out,
            "[flow first]\n  --> [1] --> RETURN\n\n[flow second]\n  --> [2] --> RETURN\n"
        );
    }

    #[test]
    fn projects_canonical_flux_as_glyph_and_back() {
        // L-97: the two directions are separate, explicitly-named subcommands — nothing sniffs the
        // notation, and a Glyph document expands back to exactly the canonical source it came from.
        let src = "flow triage(ticket: Ticket)\n  kind = classify(ticket)\n  return kind\n";
        let glyph = glyph_src(src).unwrap();
        assert_eq!(
            glyph,
            "F triage(ticket:Ticket)\n= kind classify(ticket)\n^ kind\n"
        );
        assert_eq!(unglyph_src(&glyph).unwrap(), src);
    }

    #[test]
    fn glyph_reports_the_existing_parser_diagnostics() {
        // Canonical source in, canonical diagnostic out — one parse entry, one error vocabulary.
        let bad = "= = = not flux = = =";
        let glyph = glyph_src(bad)
            .expect_err("malformed flux must not project")
            .to_string();
        assert_eq!(glyph, compile_src(bad).expect_err("also fails").to_string());
    }

    #[test]
    fn unglyph_refuses_canonical_flux() {
        // The Glyph reader is not a second canonical parser: feeding it `.flux` is an error, not a
        // silent pass-through.
        let err = unglyph_src("flow x\n  return 1\n")
            .expect_err("canonical Flux is not a Glyph document")
            .to_string();
        assert!(err.contains("glyph parse error"), "got: {err}");
    }

    /// Render from an in-memory string (test helper mirroring `render_ast`'s parse+render).
    fn render_ast_str(raw: &str) -> Result<String> {
        let ast: DraftAst = serde_json::from_str(raw)
            .map_err(|e| Error::Other(format!("invalid AST JSON: {e}")))?;
        Ok(flux_lang::render::render_pretty(&ast))
    }

    /// Compile from an in-memory string (test helper over the shared `compile_src` path).
    fn compile_str(src: &str) -> Result<String> {
        compile_src(src)
    }
}
