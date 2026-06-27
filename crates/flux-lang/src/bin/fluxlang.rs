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
    Schema,
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
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fluxlang: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let out = match Cli::parse().command {
        Command::Skill => flux_lang::skill::render(),
        Command::Schema => serde_json::to_string_pretty(&flux_lang::schema::ast_schema())
            .map_err(|e| e.to_string())?,
        Command::Render { file } => render_ast(file)?,
        Command::Compile { file } => compile_text(file)?,
    };
    let mut stdout = std::io::stdout();
    stdout
        .write_all(out.as_bytes())
        .map_err(|e| e.to_string())?;
    if !out.ends_with('\n') {
        let _ = stdout.write_all(b"\n");
    }
    Ok(())
}

/// Read the contents of `file`, or stdin when omitted.
fn read_source(file: Option<PathBuf>) -> Result<String, String> {
    match file {
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display())),
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            Ok(s)
        }
    }
}

/// Read a JSON `DraftAst` from `file` (or stdin) and render it as a tree — colored on a TTY, plain
/// otherwise (so piped output stays clean).
fn render_ast(file: Option<PathBuf>) -> Result<String, String> {
    let raw = read_source(file)?;
    let ast: DraftAst = serde_json::from_str(&raw).map_err(|e| format!("invalid AST JSON: {e}"))?;
    Ok(if std::io::stdout().is_terminal() {
        flux_lang::render::render_styled(&ast, &ANSI)
    } else {
        flux_lang::render::render_pretty(&ast)
    })
}

/// Read Flux-Lang text from `file` (or stdin), parse it, and emit the `DraftAst` as pretty JSON. The
/// inverse of `format` — `compile(format(ast))` round-trips back to the same AST.
fn compile_text(file: Option<PathBuf>) -> Result<String, String> {
    let src = read_source(file)?;
    let ast = flux_lang::parse::parse(&src).map_err(|e| format!("parse error: {e}"))?;
    serde_json::to_string_pretty(&ast).map_err(|e| e.to_string())
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

    /// Render from an in-memory string (test helper mirroring `render_ast`'s parse+render).
    fn render_ast_str(raw: &str) -> Result<String, String> {
        let ast: DraftAst =
            serde_json::from_str(raw).map_err(|e| format!("invalid AST JSON: {e}"))?;
        Ok(flux_lang::render::render_pretty(&ast))
    }

    /// Compile from an in-memory string (test helper mirroring `compile_text`'s parse+serialize).
    fn compile_str(src: &str) -> Result<String, String> {
        let ast = flux_lang::parse::parse(src).map_err(|e| format!("parse error: {e}"))?;
        serde_json::to_string_pretty(&ast).map_err(|e| e.to_string())
    }
}
