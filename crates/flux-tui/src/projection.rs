//! Durable event/history projection into UI-owned state.

use super::{halt_line, Entry, IntentEntry, Sev};

/// Max persisted history entries.
const HISTORY_CAP: usize = 500;

pub(super) fn historical_observation_entry(
    observation: &flux_evidence::Observation,
) -> Option<Entry> {
    match observation.kind.as_str() {
        flux_evidence::KIND_TURN_INTENT => {
            staged_intent_entry(&observation.data).map(Entry::Intent)
        }
        flux_evidence::KIND_DESTRUCTIVE => Some(Entry::Notice {
            text: "⚠ destructive operation flagged".into(),
            sev: Sev::Warn,
        }),
        "skill.activated" => observation
            .data
            .get("skill")
            .and_then(|v| v.as_str())
            .map(|name| Entry::Notice {
                text: format!("✦ skill activated: {name}"),
                sev: Sev::Info,
            }),
        "flow.halt" => Some(Entry::Notice {
            text: halt_line(&observation.data),
            sev: Sev::Err,
        }),
        "flow.brief" => Some(Entry::Brief {
            goal: observation
                .data
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            needs: observation
                .data
                .get("needs")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        }),
        _ => None,
    }
}

pub(super) fn staged_intent_entry(data: &serde_json::Value) -> Option<IntentEntry> {
    let raw_intent = data.get("intent").and_then(|v| v.as_str())?;
    let sanitized: String = raw_intent
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let intent = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    let strings = |key: &str| {
        data.get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    };
    Some(IntentEntry {
        intent,
        families: strings("families"),
        operations: strings("operations"),
    })
}

/// Derive recall history from the same durable event store as sessions. This avoids a parallel
/// `~/.flux/history` file and keeps all filesystem ownership inside `flux-events`.
pub(super) fn load_history(events: &flux_events::EventStore) -> Vec<String> {
    let mut history = Vec::new();
    let Ok(mut sessions) = events.list(50) else {
        return history;
    };
    sessions.reverse();
    for session in sessions {
        let Ok(stored) = events.load_by_kind(&session.id, "message") else {
            continue;
        };
        for event in stored {
            if let flux_events::EventKind::Message(message) = event.kind {
                if message.role == flux_core::Role::User {
                    let text = message.text();
                    if !text.trim().is_empty() && history.last() != Some(&text) {
                        history.push(text);
                    }
                }
            }
        }
    }
    if history.len() > HISTORY_CAP {
        history.drain(0..history.len() - HISTORY_CAP);
    }
    history
}
