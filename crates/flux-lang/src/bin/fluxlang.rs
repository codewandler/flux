//! `fluxlang` — the Flux-Lang command-line surface.
//!
//! Inspect the language without the engine: print its skill, its JSON Schema, or render a JSON AST as
//! a human-readable tree. The round-trippable text syntax exists in the library (`flux_lang::parse` /
//! `flux_lang::format`); wiring a `fluxlang compile` subcommand onto it is the one remaining step. Note
//! `render` is intentionally one-way (a lossy display tree), distinct from `format`/`parse`.
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

/// Read a JSON `DraftAst` from `file` (or stdin) and render it as a tree — colored on a TTY, plain
/// otherwise (so piped output stays clean).
fn render_ast(file: Option<PathBuf>) -> Result<String, String> {
    let raw = match file {
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            s
        }
    };
    let ast: DraftAst = serde_json::from_str(&raw).map_err(|e| format!("invalid AST JSON: {e}"))?;
    Ok(if std::io::stdout().is_terminal() {
        flux_lang::render::render_styled(&ast, &ANSI)
    } else {
        flux_lang::render::render_pretty(&ast)
    })
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

    /// Render from an in-memory string (test helper mirroring `render_ast`'s parse+render).
    fn render_ast_str(raw: &str) -> Result<String, String> {
        let ast: DraftAst =
            serde_json::from_str(raw).map_err(|e| format!("invalid AST JSON: {e}"))?;
        Ok(flux_lang::render::render_pretty(&ast))
    }
}
