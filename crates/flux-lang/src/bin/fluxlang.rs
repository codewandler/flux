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
