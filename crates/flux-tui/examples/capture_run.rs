//! A-145 — **capture a recorded session as a committable, publishable JSONL fixture.**
//!
//! ```text
//! cargo run -p flux-tui --example capture_run -- \
//!     --session s_1477 --title "docs gap audit, fix, commit, release" \
//!     > crates/flux-tui/src/loopmock/src_capture.jsonl
//! ```
//!
//! ⚠ **Committing a capture is a publishing act.** It comes off a real machine into a public
//! repository and is permanent once pushed. `flux export` redacts what the `Redactor` *was told
//! about*; a real coding run also carries what redaction was never aimed at — the operator's
//! username, absolute paths, internal hostnames, locally installed plugin names, ticket and
//! customer names. So this command is an **allow-list**, not a filter:
//!
//! - only the event kinds the projection actually reads are emitted at all — every `observation`
//!   is dropped, which alone removes `turn.identity`'s `caller`, `tool_call`'s `caller`, and the
//!   `toolchain` listing that names every locally installed plugin;
//! - the fields inside those kinds are named one by one, never spread;
//! - free text is cut to [`CAP`] characters of its first line, so a long tool output cannot smuggle
//!   anything past a reviewer in its tail;
//! - what survives is then run through [`scrub`], a deny-list of shapes redaction is not aimed at,
//!   and anything it hits is **replaced, not masked in place**, so the substitution is visible;
//! - and [`assert_clean`] re-scans the finished artifact, so the command fails rather than emits a
//!   capture that still matches one of its own patterns.
//!
//! None of that removes the need to **read the diff**. It makes reading it possible.
//!
//! ⚠ A capture is a snapshot. Re-running this produces a *different* run — nothing regenerates the
//! committed file.

use std::collections::BTreeSet;

use flux_events::{EventKind, EventStore};
use flux_flow::ast::RunEvent;
use flux_tui::loopmock::capture::{Body, CaptureEvent};

/// Characters of free text kept per field. Small enough that the whole capture stays readable
/// line-by-line, which is the only redaction review that actually works.
const CAP: usize = 100;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let session = arg("--session").ok_or("usage: --session <s_NNN> [--title <text>]")?;
    let title = arg("--title").unwrap_or_else(|| "recorded run".to_string());
    let db = arg("--db").unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.flux/events.db")
    });

    let store = EventStore::open(&db)?;
    let stored = store.load_stream(&session, None)?;

    let mut hits: BTreeSet<&'static str> = BTreeSet::new();
    let mut out = String::new();
    let command = format!(
        "cargo run -p flux-tui --example capture_run -- --session {session} --title {title:?}"
    );
    push(
        &mut out,
        &CaptureEvent {
            s: 0,
            t: 0,
            body: Body::Capture {
                session: session.clone(),
                title: title.clone(),
                command,
                scrubbed: scrub_classes(),
            },
        },
    );

    for event in &stored {
        let Some(body) = project(&event.kind, &mut hits) else {
            continue;
        };
        push(
            &mut out,
            &CaptureEvent {
                s: event.global_seq as u64,
                t: event.ts_ms,
                body,
            },
        );
    }

    assert_clean(&out);
    eprintln!(
        "captured {session}: {} of {} events; scrub hit: {}",
        out.lines().count() - 1,
        stored.len(),
        if hits.is_empty() {
            "nothing".to_string()
        } else {
            hits.into_iter().collect::<Vec<_>>().join(", ")
        },
    );
    print!("{out}");
    Ok(())
}

fn push(out: &mut String, event: &CaptureEvent) {
    out.push_str(&serde_json::to_string(&event.to_json()).expect("capture event serializes"));
    out.push('\n');
}

/// The allow-list. An **exhaustive** match on `EventKind` on purpose: a new durable fact forces a
/// decision here rather than being silently omitted from every future capture.
fn project(kind: &EventKind, hits: &mut BTreeSet<&'static str>) -> Option<Body> {
    Some(match kind {
        EventKind::SessionStarted { model } => Body::SessionStarted {
            model: model.clone(),
        },
        EventKind::TurnStarted { user_input, .. } => Body::TurnStarted {
            input: cut(user_input, 2 * CAP, hits),
        },
        EventKind::TurnEnded {
            outcome, answer, ..
        } => Body::TurnEnded {
            outcome: outcome.clone(),
            answer: cut(answer, CAP, hits),
        },
        // Only an accepted plan, and only its canonical source. `plan_text` is not persisted by
        // this loop and `error` on a rejected attempt is a raw provider body — the one place a
        // provider account id has already been seen in this store.
        EventKind::PlanAttempted {
            outcome,
            plan_source: Some(src),
            ..
        } if outcome == "accepted" => Body::Plan {
            // Two lines, not one: a `plan_source` opens with a bare `flow` header, so a one-line cut
            // would publish nothing but the word "flow" — and the capture would look clean by
            // saying nothing, which is the failure mode this whole file is written against.
            src: cut_lines(src, 2, CAP, hits),
        },
        EventKind::CallUsage { usage, .. } => Body::Usage {
            u: [
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens,
            ],
        },
        EventKind::Run(run) => project_run(run, hits)?,
        // Dropped, deliberately, every one of them:
        //   Message / Compacted     the conversation; the loop view draws steps, not transcript
        //   Observation             batch-flushed (unusable for pacing) AND the one place the
        //                           operator's username and the installed plugin list live
        //   ModelChanged            not exercised by this capture
        //   PlanAttempted (other)   rejected/compile_error carry raw provider error bodies
        //   PrivateNetAdmit / CrossPluginResolve / EndpointDiscovered
        //                           security audit records naming hosts and credential locations
        //   Wakeup* / Custom        nothing the loop view draws
        _ => return None,
    })
}

fn project_run(run: &RunEvent, hits: &mut BTreeSet<&'static str>) -> Option<Body> {
    Some(match run {
        RunEvent::StepStarted { step, op, .. } => Body::StepStarted {
            id: step.to_string(),
            op: op.clone(),
        },
        RunEvent::StepSucceeded { step, .. } => Body::StepOk {
            id: step.to_string(),
        },
        RunEvent::StepFailed { step, error } => Body::StepFailed {
            id: step.to_string(),
            err: cut(error, CAP, hits),
        },
        RunEvent::OpRecorded {
            step,
            input_view,
            content,
            is_error,
            denied,
            truncated,
            ..
        } => Body::Op {
            id: step.to_string(),
            input: cut(input_view.as_deref().unwrap_or_default(), CAP, hits),
            out: cut(content, CAP, hits),
            n: content.len() as u64,
            is_error: *is_error,
            denied: *denied,
            truncated: *truncated,
        },
        _ => return None,
    })
}

/// First line, collapsed and cut to `cap` — then scrubbed. Order matters: cutting first bounds how
/// much a reviewer has to read, scrubbing second means a pattern split by the cut cannot survive.
fn cut(s: &str, cap: usize, hits: &mut BTreeSet<&'static str>) -> String {
    cut_lines(s, 1, cap, hits)
}

/// [`cut`] over the first `lines` lines, collapsed onto one.
fn cut_lines(s: &str, lines: usize, cap: usize, hits: &mut BTreeSet<&'static str>) -> String {
    let head: String = s
        .lines()
        .take(lines)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(cap)
        .collect();
    scrub(&head, hits)
}

/// One deny-list entry: a shape the `Redactor` is not aimed at, what it becomes, and the class the
/// capture header advertises. Substitutions are **visible words** rather than `***`, so a reader of
/// the committed capture can tell a scrub from a value that happened to look like one.
struct Rule {
    pattern: &'static str,
    replacement: &'static str,
    /// What the header says was scrubbed. Deliberately a description rather than the pattern: the
    /// header would otherwise be a line of the artifact that matches the deny-list.
    class: &'static str,
    /// Only match at a word start. Credential *prefixes* need this — `sk-` is a substring of
    /// "risk-gated", and a scrub that silently mangles prose is a scrub nobody will trust.
    word_start: bool,
}

const SCRUB: &[Rule] = &[
    // The operator's home directory, and with it their username. Reaches this store only as an
    // absolute path some op was handed.
    Rule {
        pattern: "/home/",
        replacement: "/<home>/",
        class: "absolute home paths (username)",
        word_start: false,
    },
    Rule {
        pattern: "/Users/",
        replacement: "/<home>/",
        class: "absolute home paths (username)",
        word_start: false,
    },
    // The downstream consumer's name has no business in this repo, and the locally installed plugin
    // pack puts it in every toolchain listing.
    Rule {
        pattern: "babelforce",
        replacement: "<downstream>",
        class: "downstream consumer names",
        word_start: false,
    },
    Rule {
        pattern: "babeldesk",
        replacement: "<downstream>",
        class: "downstream consumer names",
        word_start: false,
    },
    // Credential shapes, in case a doc example or a shell line carried one past the Redactor.
    Rule {
        pattern: "sk-",
        replacement: "<key>",
        class: "credential shapes",
        word_start: true,
    },
    Rule {
        pattern: "ghp_",
        replacement: "<key>",
        class: "credential shapes",
        word_start: true,
    },
    Rule {
        pattern: "xoxb-",
        replacement: "<key>",
        class: "credential shapes",
        word_start: true,
    },
    Rule {
        pattern: "AKIA",
        replacement: "<key>",
        class: "credential shapes",
        word_start: true,
    },
    Rule {
        pattern: "Bearer ",
        replacement: "<key> ",
        class: "credential shapes",
        word_start: true,
    },
    Rule {
        pattern: "BEGIN PRIVATE KEY",
        replacement: "<key>",
        class: "credential shapes",
        word_start: false,
    },
    Rule {
        pattern: "BEGIN RSA",
        replacement: "<key>",
        class: "credential shapes",
        word_start: false,
    },
    // Reachable-address shapes.
    Rule {
        pattern: "127.0.0.1",
        replacement: "<loopback>",
        class: "internal addresses",
        word_start: false,
    },
    Rule {
        pattern: "192.168.",
        replacement: "<private-net>.",
        class: "internal addresses",
        word_start: false,
    },
    Rule {
        pattern: ".internal",
        replacement: ".<internal>",
        class: "internal addresses",
        word_start: false,
    },
];

/// The classes the header advertises, deduplicated and in declaration order.
fn scrub_classes() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rule in SCRUB {
        if !out.iter().any(|c| c == rule.class) {
            out.push(rule.class.to_string());
        }
    }
    out
}

fn scrub(s: &str, hits: &mut BTreeSet<&'static str>) -> String {
    let mut out = s.to_string();
    for rule in SCRUB {
        while let Some(at) = find(&out, rule) {
            hits.insert(rule.pattern);
            out.replace_range(at..at + rule.pattern.len(), rule.replacement);
        }
    }
    out
}

/// The next occurrence of `rule`'s pattern that the rule actually claims.
fn find(haystack: &str, rule: &Rule) -> Option<usize> {
    let mut from = 0usize;
    while let Some(rel) = haystack[from..].find(rule.pattern) {
        let at = from + rel;
        let boundary = !rule.word_start
            || at == 0
            || !haystack[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        if boundary {
            return Some(at);
        }
        from = at + rule.pattern.len();
    }
    None
}

/// Re-scan the finished artifact. A capture that still matches one of its own deny-list patterns is
/// a bug in the scrub, and the command must fail rather than print it — the failure mode this
/// guards is C-339's, redaction falling back to the unredacted value.
fn assert_clean(out: &str) {
    for line in out.lines() {
        for rule in SCRUB {
            assert!(
                find(line, rule).is_none(),
                "capture still contains {:?}: {line}",
                rule.pattern,
            );
        }
    }
}
