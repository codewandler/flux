//! Transition discovery, selection, execution, and partial-failure reporting.

use super::*;

pub(crate) fn transition_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let current = issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(Value::Null);
    let tl = jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?;
    let transitions = tl.get("transitions").cloned().unwrap_or(json!([]));
    Ok(json!({"issue_key": key, "current_status": current, "transitions": transitions}))
}

/// A stable per-transition key (id + target id + target name), matching the reference `transitionKey`
/// — distinguishes transitions even when ids repeat across statuses, so the walk never loops.
pub(crate) fn transition_key(t: &Value) -> String {
    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let to = t.get("to");
    let to_id = to
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let to_name = to
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    format!("{id}\u{0}{to_id}\u{0}{to_name}")
}

/// Clamp the auto_transition step budget (default 5, max 20) — the reference `boundedTransitionSteps`.
pub(crate) fn bounded_transition_steps(value: i64) -> i64 {
    if value <= 0 {
        5
    } else if value > 20 {
        20
    } else {
        value
    }
}

/// Score an intermediate transition (lower is more progress-y) — the reference
/// `intermediateTransitionScore`. Terminal/blocking transitions are heavily penalized; clear
/// forward-motion transitions score 0; done/resolved 50; everything else 10.
pub(crate) fn intermediate_transition_score(t: &Value) -> i64 {
    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    let to = t
        .get("to")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let text = format!("{name} {to}").to_lowercase();
    for term in [
        "blocked",
        "block",
        "hold",
        "abandoned",
        "closed",
        "cancel",
        "rejected",
    ] {
        if text.contains(term) {
            return 100;
        }
    }
    for term in [
        "progress",
        "prepare",
        "preparation",
        "selected",
        "todo",
        "to do",
        "review",
        "test",
        "qa",
    ] {
        if text.contains(term) {
            return 0;
        }
    }
    if text.contains("done") || text.contains("resolved") {
        return 50;
    }
    10
}

/// Pick the best untried, non-self transition by score — the reference `bestIntermediateTransition`.
pub(crate) fn best_intermediate_transition(
    transitions: &[Value],
    current: &Value,
    tried: &[String],
) -> Option<Value> {
    let current_name = current.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let current_id = current.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut best: Option<Value> = None;
    let mut best_score = 1000;
    for t in transitions {
        if tried.iter().any(|x| x == &transition_key(t)) {
            continue;
        }
        let to = t.get("to").unwrap_or(&Value::Null);
        if status_matches(to, current_name) || status_matches(to, current_id) {
            continue;
        }
        let score = intermediate_transition_score(t);
        if score < best_score {
            best = Some(t.clone());
            best_score = score;
        }
    }
    if best_score < 1000 {
        best
    } else {
        None
    }
}

/// Select a transition by id, name, target-status (matching `to`), or — when intermediate steps are
/// allowed — the best-scoring untried transition. Ported from the reference `selectTransition`.
pub(crate) fn select_transition(
    transitions: &[Value],
    current: &Value,
    id: &str,
    name: &str,
    target: &str,
    allow_intermediate: bool,
    tried: &[String],
) -> Option<Value> {
    let untried = |t: &Value| -> bool {
        let k = transition_key(t);
        !tried.iter().any(|x| x == &k)
    };
    if !id.is_empty() {
        return transitions
            .iter()
            .find(|t| {
                untried(t)
                    && t.get("id")
                        .and_then(|v| v.as_str())
                        .map(|x| x.trim().eq_ignore_ascii_case(id))
                        .unwrap_or(false)
            })
            .cloned();
    }
    if !name.is_empty() {
        return transitions
            .iter()
            .find(|t| {
                untried(t)
                    && t.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| n.trim().eq_ignore_ascii_case(name))
                        .unwrap_or(false)
            })
            .cloned();
    }
    if !target.is_empty() {
        if let Some(t) = transitions
            .iter()
            .find(|t| untried(t) && status_matches(t.get("to").unwrap_or(&Value::Null), target))
        {
            return Some(t.clone());
        }
        if !allow_intermediate {
            return None;
        }
        return best_intermediate_transition(transitions, current, tried);
    }
    if allow_intermediate && !transitions.is_empty() {
        return best_intermediate_transition(transitions, current, tried);
    }
    None
}

pub(crate) fn transition_summary(transitions: &[Value]) -> String {
    if transitions.is_empty() {
        return "none".into();
    }
    transitions
        .iter()
        .map(|t| {
            let name = {
                let n = t.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
                if n.is_empty() {
                    t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim()
                } else {
                    n
                }
            };
            let to = t
                .get("to")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if to.is_empty() {
                name.to_string()
            } else {
                format!("{name} -> {to}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn transition_run(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let id = opt_str(&input, "transition_id").trim();
    let name = opt_str(&input, "transition_name").trim();
    let target = opt_str(&input, "target_status").trim();
    if id.is_empty() && name.is_empty() && target.is_empty() {
        return Err("transition_id, transition_name, or target_status is required".into());
    }
    let auto = input
        .get("auto_transition")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_steps =
        bounded_transition_steps(input.get("max_steps").and_then(|v| v.as_i64()).unwrap_or(0));

    // Read the initial status + currently available transitions (the reference ListTransitions).
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let initial_status = issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut current_status = initial_status.clone();
    let mut transitions: Vec<Value> =
        jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?
            .get("transitions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

    let mut applied: Vec<Value> = Vec::new();
    let mut tried: Vec<String> = Vec::new();
    let mut steps: i64 = 0;

    // Already at the target? Re-read the issue and return without mutating.
    if !target.is_empty() && status_matches(&current_status, target) {
        let final_issue = jget(
            host,
            &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
        )?;
        return Ok(transition_result(
            &key,
            &initial_status,
            &current_status,
            target,
            &applied,
            steps,
            &final_issue,
            &transitions,
        ));
    }

    while steps < max_steps {
        let Some(transition) = select_transition(
            &transitions,
            &current_status,
            id,
            name,
            target,
            steps > 0 || auto,
            &tried,
        ) else {
            if !applied.is_empty() {
                // Already mutated — surface what happened (the reference transitionRunFailure).
                return Err(transition_run_failure(
                    &key,
                    &initial_status,
                    &current_status,
                    &applied,
                    &format!(
                        "no further transition matches the request; available: {}",
                        transition_summary(&transitions)
                    ),
                ));
            }
            return Err(format!(
                "no available transition matches the request; available: {}",
                transition_summary(&transitions)
            ));
        };
        let tkey = transition_key(&transition);
        if tried.iter().any(|x| x == &tkey) {
            return Err(transition_run_failure(
                &key,
                &initial_status,
                &current_status,
                &applied,
                &format!(
                    "transition walk repeated {:?} before reaching target status {target:?}",
                    transition
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
            ));
        }
        tried.push(tkey);
        let tid = transition
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        jsend_noresp(
            host,
            "POST",
            &format!("/issue/{}/transitions", urlencode(&key)),
            Some(&json!({"transition": {"id": tid}})),
        )?;
        applied.push(transition);
        steps += 1;

        if target.is_empty() {
            break; // an explicit single transition by id/name
        }
        // Re-read state (the reference re-calls ListTransitions each loop).
        let issue = jget(
            host,
            &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
        )?;
        current_status = issue
            .get("fields")
            .and_then(|f| f.get("status"))
            .cloned()
            .unwrap_or(Value::Null);
        transitions = jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?
            .get("transitions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if status_matches(&current_status, target) {
            break;
        }
        if !auto {
            break;
        }
    }

    if !target.is_empty() && !status_matches(&current_status, target) && steps >= max_steps {
        return Err(transition_run_failure(
            &key,
            &initial_status,
            &current_status,
            &applied,
            &format!(
                "target status {target:?} was not reached within max_steps={max_steps}; current status is {:?}",
                current_status.get("name").and_then(|v| v.as_str()).unwrap_or("")
            ),
        ));
    }

    let final_issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    current_status = final_issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(current_status);
    Ok(transition_result(
        &key,
        &initial_status,
        &current_status,
        target,
        &applied,
        steps,
        &final_issue,
        &transitions,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn transition_result(
    key: &str,
    initial_status: &Value,
    current_status: &Value,
    target: &str,
    applied: &[Value],
    steps: i64,
    final_issue: &Value,
    available: &[Value],
) -> Value {
    json!({
        "ok": true,
        "issue_key": key,
        "initial_status": initial_status,
        "current_status": current_status,
        "target_status": target,
        "applied_transitions": applied,
        "available_transitions": available,
        "steps": steps,
        "issue": final_issue,
    })
}

/// Build a walker-failure error that does NOT hide the transitions already applied (the reference
/// `transitionRunFailure`): it names every applied transition and the issue's current status.
pub(crate) fn transition_run_failure(
    key: &str,
    initial_status: &Value,
    current_status: &Value,
    applied: &[Value],
    message: &str,
) -> String {
    if applied.is_empty() {
        return message.to_string();
    }
    let names: Vec<String> = applied
        .iter()
        .map(|t| {
            t.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    let was = initial_status
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let now = current_status
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!(
        "{message}; issue {key} WAS mutated before the failure: applied {} transition(s): {}; status is now {now:?} (was {was:?})",
        names.len(),
        names.join(" → ")
    )
}
