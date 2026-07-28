use super::*;

/// A coarse relative time string from a millisecond epoch timestamp, for `flux wakeups list`.
/// Positive (future) durations render as "in Xs/Xm/Xh/Xd"; non-positive ones as "overdue".
fn fmt_relative(target_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(target_ms);
    let secs = (target_ms - now) / 1000;
    if secs <= 0 {
        return "overdue".to_string();
    }
    match secs {
        s if s < 60 => format!("in {s}s"),
        s if s < 3_600 => format!("in {}m", s / 60),
        s if s < 86_400 => format!("in {}h", s / 3_600),
        s => format!("in {}d", s / 86_400),
    }
}

/// Resolve a `flux wakeups` session argument (`last`, or an explicit id) against `store`.
fn resolve_wakeup_session(store: &EventStore, session_arg: &str) -> Result<String> {
    if session_arg == "last" {
        store
            .latest_session()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .context("no recorded sessions in ~/.flux/events.db")
    } else {
        store
            .info(session_arg)
            .with_context(|| format!("unknown session `{session_arg}`"))?;
        Ok(session_arg.to_string())
    }
}

/// `flux wakeups [list <session>]` — list pending wake-ups for a session, newest-registered last
/// (A-98). Reads the same durable `EventStore::pending_wakeups` projection the op and the firing
/// path use — no live engine is needed, mirroring `flux sessions`/`flux replay`.
///
/// `flux wakeups cancel <session> <wakeup_id>` — cancel a pending wake-up before it fires.
pub(super) fn run_wakeups(action: Option<WakeupAction>) -> Result<()> {
    let store = open_event_store()?;
    match action.unwrap_or(WakeupAction::List {
        session: "last".to_string(),
    }) {
        WakeupAction::List { session } => {
            let sid = resolve_wakeup_session(&store, &session)?;
            let pending = store
                .pending_wakeups(&sid)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if pending.is_empty() {
                eprintln!("no pending wake-ups on session {sid}");
                return Ok(());
            }
            for w in &pending {
                let prompt = w.prompt.replace('\n', " ");
                let prompt: String = prompt.chars().take(60).collect();
                println!(
                    "{}  {:<10} {}{}",
                    w.wakeup_id,
                    fmt_relative(w.fire_at_ms),
                    prompt,
                    if w.context.is_some() {
                        " (+context)"
                    } else {
                        ""
                    }
                );
            }
            eprintln!(
                "{}",
                style::dim(&format!(
                    "{} pending wake-up(s) on session {sid} — cancel with `flux wakeups cancel {sid} <id>`",
                    pending.len()
                ))
            );
            Ok(())
        }
        WakeupAction::Cancel { session, wakeup_id } => {
            let sid = resolve_wakeup_session(&store, &session)?;
            let cancelled = store
                .cancel_wakeup(&sid, &wakeup_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if cancelled {
                println!("cancelled wake-up {wakeup_id} on session {sid}");
            } else {
                bail!(
                    "wake-up `{wakeup_id}` is not pending on session {sid} (unknown, already fired, or already cancelled)"
                );
            }
            Ok(())
        }
    }
}
