//! `flux preset` — make the `flux_sdk::recipes` cookbook reachable from the binary.
//!
//! A *preset* is a named, parameterized flow drawn from the recipe cookbook: name it, fill its op-name
//! slots + input with `key=value` arguments, and the command either **scaffolds** the resulting
//! Flux-Lang flow (default — print it as a tree or JSON) or **runs** it (`--run`) through the very same
//! envelope as `flux flow run`. Recipes are op-agnostic templates, so a preset runs offline whenever the
//! ops you give it resolve in the live registry (the built-ins: `read`/`grep`/`glob`/`write`/…); the
//! model-flavored presets (`route_intent`, `answer_with_fallback`) need a provider and are
//! scaffold-by-default.
//!
//! Argument convention (uniform `key=value`, order-independent):
//! - scalars: `max=3 backoff=exponential delay_ms=200 timeout_ms=2000 item=f collect=out bind=r`
//! - op-name slots: `op=read classify_op=ai.reason synth_op=synth`
//! - lists: repeat the key — `op=read op=glob` (also `op=read,glob`); arms as `arm=label:handler`
//! - the `input`/`source`/`question`/`until` nodes: a JSON literal (`input='"hi"'`,
//!   `source='["a.txt","b.txt"]'`), a `$name` variable reference, or a bare string.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use flux_sdk::dsl::{lit, var, DraftAst, Node};
use flux_sdk::recipes::{batch, compose, dispatch, fanout, lookup, resilience, routing};

/// One catalog row — drives `flux preset list` and `flux preset help <name>`. The actual builder lives
/// in [`build_flow`].
struct PresetInfo {
    name: &'static str,
    category: &'static str,
    /// `true` ⇒ runs offline when pointed at built-in ops; `false` ⇒ needs a model op (scaffold-first).
    deterministic: bool,
    /// Human usage hint — the keys this preset consumes.
    keys: &'static str,
    blurb: &'static str,
}

const CATALOG: &[PresetInfo] = &[
    PresetInfo {
        name: "map_each",
        category: "batch",
        deterministic: true,
        keys: "item= source= op= collect=",
        blurb: "map an op over each element of a list",
    },
    PresetInfo {
        name: "repeat_until",
        category: "batch",
        deterministic: true,
        keys: "max= op= input= bind= until=",
        blurb: "retry an op until a condition holds",
    },
    PresetInfo {
        name: "poll_for",
        category: "batch",
        deterministic: true,
        keys: "for_ms= every_ms= op= input=",
        blurb: "poll an op on an interval for a duration",
    },
    PresetInfo {
        name: "race_first",
        category: "batch",
        deterministic: true,
        keys: "timeout_ms= op= op=… input= bind=",
        blurb: "race ops, take the first to finish",
    },
    PresetInfo {
        name: "retry_with_backoff",
        category: "resilience",
        deterministic: true,
        keys: "max= backoff= delay_ms= op= input= bind=",
        blurb: "retry on error with backoff",
    },
    PresetInfo {
        name: "with_timeout",
        category: "resilience",
        deterministic: true,
        keys: "ms= op= input= bind=",
        blurb: "bound an op by a deadline",
    },
    PresetInfo {
        name: "with_budget",
        category: "resilience",
        deterministic: true,
        keys: "limit= op= input= bind=",
        blurb: "cap the op dispatches an op may make",
    },
    PresetInfo {
        name: "try_catch",
        category: "resilience",
        deterministic: true,
        keys: "op= input= catch= handler=",
        blurb: "run an op, recover via a handler on error",
    },
    PresetInfo {
        name: "parallel_all",
        category: "fanout",
        deterministic: true,
        keys: "op= op=… input=",
        blurb: "run ops concurrently, replay in order",
    },
    PresetInfo {
        name: "match_value",
        category: "dispatch",
        deterministic: true,
        keys: "subject_op= input= arm=value:handler… default=",
        blurb: "dispatch on a computed value",
    },
    PresetInfo {
        name: "route_intent",
        category: "model",
        deterministic: false,
        keys: "classify_op= input= arm=label:handler… default=",
        blurb: "classify once, then route",
    },
    PresetInfo {
        name: "answer_with_fallback",
        category: "model",
        deterministic: false,
        keys: "primary_op= escalate_op= synth_op= question=",
        blurb: "degrade gracefully into an answer",
    },
    PresetInfo {
        name: "resilient_call",
        category: "compose",
        deterministic: false,
        keys: "max= backoff= delay_ms= timeout_ms= primary= backup= input= bind=",
        blurb: "retry { timeout { fallback {…} } }, nested",
    },
];

fn find(name: &str) -> Option<&'static PresetInfo> {
    CATALOG.iter().find(|p| p.name == name)
}

/// Parse a `Node` spec: `$name` ⇒ a variable, valid JSON ⇒ a literal, anything else ⇒ a bare string.
fn parse_node(spec: &str) -> Node {
    if let Some(name) = spec.strip_prefix('$') {
        return var(name);
    }
    match serde_json::from_str::<serde_json::Value>(spec) {
        Ok(value) => lit(value),
        Err(_) => lit(spec),
    }
}

/// The parsed `key=value` arguments — a multimap, since list params (`op=`, `arm=`) repeat their key.
struct ArgMap {
    map: BTreeMap<String, Vec<String>>,
}

impl ArgMap {
    fn parse(pairs: &[String]) -> Result<Self> {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in pairs {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("expected key=value, got `{p}`"))?;
            map.entry(k.to_string()).or_default().push(v.to_string());
        }
        Ok(Self { map })
    }

    /// The last value for a required scalar key.
    fn req(&self, k: &str) -> Result<&str> {
        self.map
            .get(k)
            .and_then(|v| v.last())
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required `{k}=`"))
    }

    fn req_u32(&self, k: &str) -> Result<u32> {
        self.req(k)?
            .parse()
            .with_context(|| format!("`{k}=` must be a non-negative integer"))
    }

    fn req_u64(&self, k: &str) -> Result<u64> {
        self.req(k)?
            .parse()
            .with_context(|| format!("`{k}=` must be a non-negative integer"))
    }

    /// A required `Node` argument.
    fn node(&self, k: &str) -> Result<Node> {
        Ok(parse_node(self.req(k)?))
    }

    /// A required list of op names: repeated `op=` keys, each optionally comma-joined.
    fn ops(&self, k: &str) -> Result<Vec<String>> {
        let raw = self
            .map
            .get(k)
            .ok_or_else(|| anyhow::anyhow!("missing required `{k}=`"))?;
        let out: Vec<String> = raw
            .iter()
            .flat_map(|e| e.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if out.is_empty() {
            bail!("`{k}=` needs at least one op");
        }
        Ok(out)
    }

    /// The `(value/label, handler)` arms: repeated `arm=key:handler` keys.
    fn arms(&self) -> Result<Vec<(String, String)>> {
        let raw = self.map.get("arm").map(Vec::as_slice).unwrap_or(&[]);
        raw.iter()
            .map(|e| {
                e.split_once(':')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .ok_or_else(|| anyhow::anyhow!("`arm=` must be key:handler, got `{e}`"))
            })
            .collect()
    }
}

/// Dispatch a preset name + its parsed args to the matching `flux_sdk::recipes` builder.
fn build_flow(name: &str, kv: &ArgMap) -> Result<DraftAst> {
    Ok(match name {
        "map_each" => batch::map_each(
            kv.req("item")?,
            kv.node("source")?,
            kv.req("op")?,
            kv.req("collect")?,
        ),
        "repeat_until" => batch::repeat_until(
            kv.req_u32("max")?,
            kv.req("op")?,
            kv.node("input")?,
            kv.req("bind")?,
            kv.node("until")?,
        ),
        "poll_for" => batch::poll_for(
            kv.req_u64("for_ms")?,
            kv.req_u64("every_ms")?,
            kv.req("op")?,
            kv.node("input")?,
        ),
        "race_first" => {
            let owned = kv.ops("op")?;
            let ops: Vec<&str> = owned.iter().map(String::as_str).collect();
            batch::race_first(
                kv.req_u64("timeout_ms")?,
                &ops,
                kv.node("input")?,
                kv.req("bind")?,
            )
        }
        "retry_with_backoff" => resilience::retry_with_backoff(
            kv.req_u32("max")?,
            kv.req("backoff")?,
            kv.req_u64("delay_ms")?,
            kv.req("op")?,
            kv.node("input")?,
            kv.req("bind")?,
        ),
        "with_timeout" => resilience::with_timeout(
            kv.req_u64("ms")?,
            kv.req("op")?,
            kv.node("input")?,
            kv.req("bind")?,
        ),
        "with_budget" => resilience::with_budget(
            kv.req_u32("limit")?,
            kv.req("op")?,
            kv.node("input")?,
            kv.req("bind")?,
        ),
        "try_catch" => resilience::try_catch(
            kv.req("op")?,
            kv.node("input")?,
            kv.req("catch")?,
            kv.req("handler")?,
        ),
        "parallel_all" => {
            let owned = kv.ops("op")?;
            let ops: Vec<&str> = owned.iter().map(String::as_str).collect();
            fanout::parallel_all(&ops, kv.node("input")?)
        }
        "match_value" => {
            let owned = kv.arms()?;
            let arms: Vec<(&str, &str)> = owned
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            dispatch::match_value(
                kv.req("subject_op")?,
                kv.node("input")?,
                &arms,
                kv.req("default")?,
            )
        }
        "route_intent" => {
            let owned = kv.arms()?;
            let arms: Vec<(&str, &str)> = owned
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            routing::route_intent(
                kv.req("classify_op")?,
                kv.node("input")?,
                &arms,
                kv.req("default")?,
            )
        }
        "answer_with_fallback" => lookup::answer_with_fallback(
            kv.req("primary_op")?,
            kv.req("escalate_op")?,
            kv.req("synth_op")?,
            kv.node("question")?,
        ),
        "resilient_call" => compose::resilient_call(
            kv.req_u32("max")?,
            kv.req("backoff")?,
            kv.req_u64("delay_ms")?,
            kv.req_u64("timeout_ms")?,
            kv.req("primary")?,
            kv.req("backup")?,
            kv.node("input")?,
            kv.req("bind")?,
        ),
        other => bail!("unknown preset `{other}` — try `flux preset list`"),
    })
}

/// Entry point: `argv` after `flux preset`.
pub async fn run_preset(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("list") => return print_list(),
        Some("help") => return print_help(args.get(1).map(String::as_str)),
        _ => {}
    }
    let name = args[0].as_str();

    let mut pairs: Vec<String> = Vec::new();
    let mut run = false;
    let mut yes = false;
    let mut output = String::from("pretty");
    let mut model: Option<String> = None;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--run" => run = true,
            "--yes" | "-y" => yes = true,
            "-o" | "--output" => {
                output = it
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("`-o` needs a format: pretty|json"))?
            }
            "-m" | "--model" => {
                model = Some(
                    it.next()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("`-m` needs a provider/model spec"))?,
                )
            }
            s if s.contains('=') => pairs.push(s.to_string()),
            other => bail!(
                "preset {name}: unexpected argument `{other}` (try `flux preset help {name}`)"
            ),
        }
    }

    let kv = ArgMap::parse(&pairs)?;
    let ast = build_flow(name, &kv).with_context(|| format!("build preset `{name}`"))?;

    if run {
        // Build the agent flags from the recipe's flags and reuse the shared execute core
        // (build_agent → analyze → risk → approver → execute_flow), exactly as `flux flow run` does.
        let flags = crate::AgentFlags::from_model_yes(model.as_deref(), yes);
        crate::run_draft_ast(&flags, &ast).await
    } else {
        print_scaffold(&ast, &output)
    }
}

fn print_scaffold(ast: &DraftAst, output: &str) -> Result<()> {
    match output {
        "pretty" | "" => println!("{}", flux_flow::render::render_pretty(ast)),
        "json" => println!("{}", serde_json::to_string_pretty(ast)?),
        other => bail!("unknown -o `{other}` (use pretty|json)"),
    }
    Ok(())
}

fn print_list() -> Result<()> {
    println!(
        "{}",
        crate::style::bold("flux preset — the recipe cookbook")
    );
    println!();
    let width = CATALOG.iter().map(|p| p.name.len()).max().unwrap_or(0);
    for p in CATALOG {
        let tag = if p.deterministic {
            "[runs offline]"
        } else {
            "[needs a model]"
        };
        // Pad the plain name first, then colorize the padded string so columns stay aligned.
        let name = format!("{:<width$}", p.name);
        println!(
            "  {}  {:<10}  {:<15}  {}",
            crate::style::cyan(&name),
            p.category,
            crate::style::dim(tag),
            p.blurb,
        );
    }
    println!();
    println!(
        "  {}",
        crate::style::dim(
            "flux preset help <name>   ·   flux preset <name> key=value … [--run] [-o pretty|json]"
        )
    );
    Ok(())
}

fn print_help(name: Option<&str>) -> Result<()> {
    let Some(name) = name else {
        return print_list();
    };
    let Some(p) = find(name) else {
        bail!("unknown preset `{name}` — try `flux preset list`");
    };
    println!(
        "{}  {}",
        crate::style::bold(p.name),
        crate::style::dim(&format!("({})", p.category))
    );
    println!("  {}", p.blurb);
    println!();
    println!("  keys:   {}", p.keys);
    println!(
        "  runs:   {}",
        if p.deterministic {
            "offline, when every op resolves in the registry (built-ins like read/grep/glob/write)"
        } else {
            "needs a model op (scaffold-only without `-m provider/model`)"
        }
    );
    println!();
    println!(
        "  {}",
        crate::style::dim(
            "scaffold:  flux preset <name> … (-o pretty|json)        run:  add --run [--yes]"
        )
    );
    println!(
        "  {}",
        crate::style::dim("note: `-o json` is the form `flux flow run <file>` ingests")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argmap(pairs: &[&str]) -> ArgMap {
        ArgMap::parse(&pairs.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parse_node_handles_var_json_and_bare() {
        // $name → variable; JSON → literal; bare → string literal.
        assert!(matches!(parse_node("$x"), Node::Var { .. }));
        assert!(matches!(parse_node("\"hi\""), Node::Lit { .. }));
        assert!(matches!(parse_node("[\"a\",\"b\"]"), Node::Lit { .. }));
        assert!(matches!(parse_node("plain"), Node::Lit { .. }));
    }

    #[test]
    fn catalog_dispatches_every_preset() {
        // Every advertised preset must build with a representative arg set — guards the catalog ⇄
        // dispatch table from drifting apart.
        let cases: &[(&str, &[&str])] = &[
            (
                "map_each",
                &["item=f", "source=[\"a.txt\"]", "op=read", "collect=out"],
            ),
            (
                "repeat_until",
                &["max=3", "op=read", "input=\"a\"", "bind=r", "until=$r"],
            ),
            (
                "poll_for",
                &["for_ms=10", "every_ms=2", "op=read", "input=\"a\""],
            ),
            (
                "race_first",
                &[
                    "timeout_ms=50",
                    "op=read",
                    "op=glob",
                    "input=\"a\"",
                    "bind=r",
                ],
            ),
            (
                "retry_with_backoff",
                &[
                    "max=3",
                    "backoff=exponential",
                    "delay_ms=10",
                    "op=read",
                    "input=\"a\"",
                    "bind=r",
                ],
            ),
            (
                "with_timeout",
                &["ms=50", "op=read", "input=\"a\"", "bind=r"],
            ),
            (
                "with_budget",
                &["limit=2", "op=read", "input=\"a\"", "bind=r"],
            ),
            (
                "try_catch",
                &["op=read", "input=\"a\"", "catch=err", "handler=write"],
            ),
            ("parallel_all", &["op=read", "op=glob", "input=\"a\""]),
            (
                "match_value",
                &[
                    "subject_op=read",
                    "input=\"a\"",
                    "arm=hi:write",
                    "default=glob",
                ],
            ),
            (
                "route_intent",
                &[
                    "classify_op=ai.reason",
                    "input=\"a\"",
                    "arm=book:write",
                    "default=read",
                ],
            ),
            (
                "answer_with_fallback",
                &[
                    "primary_op=read",
                    "escalate_op=glob",
                    "synth_op=synth",
                    "question=\"q\"",
                ],
            ),
            (
                "resilient_call",
                &[
                    "max=2",
                    "backoff=linear",
                    "delay_ms=10",
                    "timeout_ms=50",
                    "primary=read",
                    "backup=glob",
                    "input=\"a\"",
                    "bind=r",
                ],
            ),
        ];
        assert_eq!(
            cases.len(),
            CATALOG.len(),
            "a preset is in the catalog but untested (or vice versa)"
        );
        for (name, args) in cases {
            assert!(find(name).is_some(), "`{name}` tested but not in CATALOG");
            build_flow(name, &argmap(args)).unwrap_or_else(|e| panic!("build `{name}`: {e:#}"));
        }
    }

    #[test]
    fn map_each_scaffolds_expected_flow() {
        let ast = build_flow(
            "map_each",
            &argmap(&["item=f", "source=[\"a.txt\"]", "op=read", "collect=out"]),
        )
        .unwrap();
        // The rendered tree mentions the op; the JSON form (what `flux flow run` ingests) too.
        let text = flux_flow::render::render_pretty(&ast);
        assert!(
            text.contains("read"),
            "render should mention the op: {text}"
        );
        let json = serde_json::to_string(&ast).unwrap();
        assert!(
            json.contains("\"op\":\"read\""),
            "json should carry the read call: {json}"
        );
    }

    #[tokio::test]
    async fn map_each_runs_over_read_in_a_temp_workspace() {
        use std::sync::Arc;

        use flux_agent::AgentSink;
        use flux_runtime::{AllowApprover, Executor, PermissionManager, ToolContext, ToolRegistry};
        use flux_system::{System, Workspace};

        // A no-op sink (every AgentSink method has a default).
        #[derive(Default)]
        struct NullSink;
        impl AgentSink for NullSink {}

        let root = std::env::temp_dir().join(format!(
            "flux-cli-preset-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello preset").unwrap();

        let ast = build_flow(
            "map_each",
            &argmap(&["item=f", "source=[\"a.txt\"]", "op=read", "collect=out"]),
        )
        .unwrap();

        // Build an executor scoped to the temp workspace (mirrors flux-sdk/src/flow.rs).
        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        let exec = Executor::new(
            registry,
            PermissionManager::from_rules(&["read".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );

        // analyze must accept it (every op resolves), then execute over an in-memory store.
        let oreg = flux_flow::registry::OpRegistry::new(exec.registry());
        flux_flow::analyze::analyze_flow(&ast, &oreg).expect("read flow analyzes");

        let store = flux_flow::state::FlowStore::in_memory().unwrap();
        let mut sink = NullSink;
        let outcome =
            flux_flow::runtime::execute_flow(&store, &exec, "preset-test", &ast, &mut sink)
                .await
                .expect("execute");
        assert!(
            outcome.result.contains("hello preset"),
            "result should carry the file content, got: {}",
            outcome.result
        );
    }
}
