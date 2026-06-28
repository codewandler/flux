# Flux-Lang IntelliJ plugin

A full authoring experience for `.flux` files in any JetBrains IDE (IntelliJ IDEA, RustRover, CLion, …).
The editor smarts are split the way the language is: a thin Kotlin layer hosts the IDE features, and the
real semantics are **reused from the Rust `fluxlang` toolchain** (validation and plan preview shell out to
it), so they never drift from the language.

## Features

- **File type** — `.flux` registered as **Flux-Lang**, with file + plugin icons.
- **Syntax highlighting** via a hand-written lexer (`lang/FluxLexer.kt`): keywords, `$symbols`, single &
  triple-quoted strings, numbers, `true|false|null`, `@effect`/`@thing` annotations, built-in + named
  types, `#` comments, operators, matched brackets.
- **Live validation** (`editor/FluxExternalAnnotator.kt`) — pipes the buffer to `fluxlang compile` (text)
  or `fluxlang render` (JSON wire form) and shows parse errors as red squiggles. Positions best-effort
  (errors carry no spans; it matches the line named in the message).
- **Completion** (`editor/FluxCompletionContributor.kt`) — keywords, types, built-in ops, `@annotations`,
  and the `$symbols` already bound in the file.
- **Live templates** — `flow`, `flowp`, `when`, `whenelse`, `unless`, `repeat`, `repeatu`, `each`, `eachc`,
  `match`, `tryc`, `retry`, `confirm`, `parallel`, `ctx` (type the name + Tab).
- **Quick docs** (Ctrl-Q) — hover any keyword/node for its meaning.
- **Folding** — collapse indentation blocks (`when`/`repeat`/`each`/`match`/`ctx`/…).
- **Structure view** (Alt-7) — navigable outline of `flow` and `type` declarations.
- **Preview Plan / Show AST** — right-click ▸ *Flux-Lang* (or the Tools menu): compiles the file with
  `fluxlang` and shows the rendered plan tree or the JSON AST.
- **Line commenting** (`#`, Ctrl-/), **brace matching** (`()`/`[]`/`{}`), and a **color settings page**
  (Settings ▸ Editor ▸ Color Scheme ▸ Flux-Lang).

The validator/preview look for the `fluxlang` binary in `$FLUXLANG_BIN`, then `<project>/target/{debug,
release}/fluxlang`, then `$PATH`. Build it with `cargo build -p flux-lang --features cli --bin fluxlang`.

## Build & run

Requires **JDK 21** (IntelliJ 2024.2+ builds against JBR 21).

```bash
cd editors/intellij

# First time only — generate the Gradle wrapper if you don't have one
#   (or just open this folder in IntelliJ and let it sync Gradle).
gradle wrapper --gradle-version 8.10

# Launch a sandbox IDE with the plugin loaded:
./gradlew runIde

# Build an installable zip (Settings ▸ Plugins ▸ ⚙ ▸ Install Plugin from Disk):
./gradlew buildPlugin   # → build/distributions/Flux-Lang-0.1.0.zip
```

Set the IDE you target in [`gradle.properties`](gradle.properties) (`platformType` / `platformVersion`).
`IC` (Community) is the default and its plugins load in every JetBrains IDE.

## Verify

Open these and confirm coloring, the file icon, Ctrl-/ commenting, and brace matching:

- `../../crates/flux-flow/assets/agent-loop.flux`
- `../../crates/flux-lang/examples/call-routing.flux`

Every `*.flux` text file under `crates/flux-lang/examples/`, `crates/flux-flow/assets/`, and `examples/`
should highlight without the lexer choking.

## Layout

```
src/main/kotlin/dev/flux/intellij/
  FluxLanguage.kt              the Language
  FluxFileType.kt              .flux ↔ Flux-Lang
  FluxIcons.kt                 file icon loader
  FluxCommenter.kt             # line comments
  FluxBraceMatcher.kt          () [] {} pairing
  lang/FluxTokenType.kt        IElementType for tokens
  lang/FluxTokens.kt           the token set
  lang/FluxLexer.kt            the lexer (the real work)
  highlight/FluxSyntaxHighlighter.kt         token → color mapping
  highlight/FluxSyntaxHighlighterFactory.kt  registration
  highlight/FluxColorSettingsPage.kt         recolor + preview
src/main/resources/
  META-INF/plugin.xml          extension registrations
  icons/flux.svg               file icon
```

## Scope & next tiers

No validation, completion, or navigation yet. The natural next step (Tier 2) is an `ExternalAnnotator` that
shells out to `fluxlang compile` for live parse-error checking — note that `flux-lang` errors carry no
source spans today, so precise squiggles want a small upstream change first. See the project plan for the
full ladder.
