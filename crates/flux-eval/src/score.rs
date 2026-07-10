//! Suite scoring and the keep/revert comparison.
//!
//! The primary signal is the **weighted pass-rate**. Cost (tool-errors, then iterations) is a strict
//! *lexicographic* tie-breaker, never blended in — a faster-but-wronger agent must not outscore a
//! slower-but-correct one. The improvement loop adopts a candidate only when [`SuiteScore::is_better`]
//! holds (and, separately, the dev-gate is green).

use crate::metrics::{CaseOutcome, RunResult};

/// An aggregate score over a suite run.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct SuiteScore {
    /// False when any requested trial failed to produce a complete benchmark observation.
    pub valid: bool,
    pub invalid_trials: u32,
    /// Weighted mean of per-task pass-rates, in `[0,1]`.
    pub pass_rate: f64,
    /// Weighted mean of per-task sub-check pass-rates (partial credit), in `[0,1]`. Equals
    /// `pass_rate` for binary-only adapters; finer-grained for adapters that report sub-checks.
    pub mean_check_pass_rate: f64,
    pub total: u32,
    /// Tasks that passed every trial.
    pub passed: u32,
    pub total_weight: f64,
    pub mean_tool_errors: f64,
    pub mean_iterations: f64,
    /// Mean tokens only when every valid case reported usage; unavailable is not zero cost.
    pub mean_tokens: Option<f64>,
    pub mean_wall_ms: f64,
}

impl SuiteScore {
    /// Aggregate per-task results. `weight_of(task_id)` supplies each task's weight (default 1.0 for
    /// unknown ids).
    pub fn from_results(results: &[RunResult], weight_of: impl Fn(&str) -> f64) -> Self {
        let n = results.len() as f64;
        let mut total_weight = 0.0;
        let mut weighted_pass = 0.0;
        let mut weighted_check = 0.0;
        let mut sum_tool_errors = 0u64;
        let mut sum_iterations = 0u64;
        let mut sum_wall_ms = 0u64;
        let mut sum_tokens = 0.0;
        let mut token_samples = 0usize;
        let mut invalid_trials = 0u32;
        let mut passed = 0u32;
        for r in results {
            if !r.valid {
                invalid_trials += 1;
            }
            let w = weight_of(&r.task_id);
            total_weight += w;
            if r.passed {
                weighted_pass += w;
                passed += 1;
            }
            let cr = if r.checks_total > 0 {
                r.checks_passed as f64 / r.checks_total as f64
            } else if r.passed {
                1.0
            } else {
                0.0
            };
            weighted_check += w * cr;
            sum_tool_errors += r.tool_errors as u64;
            sum_iterations += r.iterations as u64;
            sum_wall_ms += r.wall_ms;
            if let Some(tokens) = &r.tokens {
                sum_tokens += tokens.total() as f64;
                token_samples += 1;
            }
        }
        SuiteScore {
            valid: !results.is_empty() && invalid_trials == 0,
            invalid_trials,
            pass_rate: if total_weight > 0.0 {
                weighted_pass / total_weight
            } else {
                0.0
            },
            mean_check_pass_rate: if total_weight > 0.0 {
                weighted_check / total_weight
            } else {
                0.0
            },
            total: results.len() as u32,
            passed,
            total_weight,
            mean_tool_errors: if n > 0.0 {
                sum_tool_errors as f64 / n
            } else {
                0.0
            },
            mean_iterations: if n > 0.0 {
                sum_iterations as f64 / n
            } else {
                0.0
            },
            mean_tokens: (n > 0.0 && token_samples == results.len()).then_some(sum_tokens / n),
            mean_wall_ms: if n > 0.0 { sum_wall_ms as f64 / n } else { 0.0 },
        }
    }

    /// Aggregate per-task [`CaseOutcome`]s (multi-trial). `pass_rate` is the weighted mean of per-task
    /// pass-rates; `passed` counts tasks that passed every trial; means are over tasks.
    pub fn from_cases(cases: &[CaseOutcome], weight_of: impl Fn(&str) -> f64) -> Self {
        let n = cases.len() as f64;
        let mut total_weight = 0.0;
        let mut weighted_pass = 0.0;
        let mut weighted_check = 0.0;
        let (mut e, mut it, mut tok, mut wall) = (0.0, 0.0, 0.0, 0.0);
        let mut token_cases = 0usize;
        let mut invalid_trials = 0u32;
        let mut passed = 0u32;
        for c in cases {
            invalid_trials = invalid_trials.saturating_add(c.invalid_trials);
            let w = weight_of(&c.task_id);
            total_weight += w;
            weighted_pass += w * c.pass_rate;
            weighted_check += w * c.mean_check_pass_rate;
            e += c.mean_tool_errors;
            it += c.mean_iterations;
            if let Some(tokens) = c.mean_tokens {
                tok += tokens;
                token_cases += 1;
            }
            wall += c.mean_wall_ms;
            if c.pass_rate >= 1.0 - 1e-9 {
                passed += 1;
            }
        }
        SuiteScore {
            valid: !cases.is_empty() && invalid_trials == 0 && cases.iter().all(|case| case.valid),
            invalid_trials,
            pass_rate: if total_weight > 0.0 {
                weighted_pass / total_weight
            } else {
                0.0
            },
            mean_check_pass_rate: if total_weight > 0.0 {
                weighted_check / total_weight
            } else {
                0.0
            },
            total: cases.len() as u32,
            passed,
            total_weight,
            mean_tool_errors: if n > 0.0 { e / n } else { 0.0 },
            mean_iterations: if n > 0.0 { it / n } else { 0.0 },
            mean_tokens: (n > 0.0 && token_cases == cases.len()).then_some(tok / n),
            mean_wall_ms: if n > 0.0 { wall / n } else { 0.0 },
        }
    }

    /// A single committable scalar for tags/reporting: `round(mean_check_pass_rate * 1000)` (e.g.
    /// 0.833 → 833). Tracks the **sub-check** pass-rate (partial credit), not the full-pass-rate, so a
    /// candidate that improves only on sub-checks without flipping a full pass tags meaningfully
    /// (`improve-tbench-833`) instead of `improve-tbench-0`. `mean_check_pass_rate >= pass_rate`
    /// always, and equals it for binary adapters (e.g. the synthetic suite), so the scalar is
    /// unchanged there.
    pub fn scalar(&self) -> u32 {
        (self.mean_check_pass_rate * 1000.0).round() as u32
    }

    /// Is `self` strictly better than `baseline`? Lexicographic: higher full-pass-rate wins; on a tie,
    /// higher sub-check pass-rate (partial progress); then fewer mean tool-errors; then fewer mean
    /// iterations; then fewer tokens. A small epsilon absorbs float noise on the rate comparisons.
    pub fn is_better(&self, baseline: &SuiteScore) -> bool {
        const EPS: f64 = 1e-9;
        if !self.valid || !baseline.valid {
            return false;
        }
        if self.pass_rate > baseline.pass_rate + EPS {
            return true;
        }
        if self.pass_rate + EPS < baseline.pass_rate {
            return false;
        }
        // equal full-pass-rate → more sub-checks passing (partial progress toward a full pass)
        if self.mean_check_pass_rate > baseline.mean_check_pass_rate + EPS {
            return true;
        }
        if self.mean_check_pass_rate + EPS < baseline.mean_check_pass_rate {
            return false;
        }
        // equal partial credit → fewer tool-errors
        if self.mean_tool_errors + EPS < baseline.mean_tool_errors {
            return true;
        }
        if self.mean_tool_errors > baseline.mean_tool_errors + EPS {
            return false;
        }
        // equal tool-errors → fewer iterations
        if self.mean_iterations + EPS < baseline.mean_iterations {
            return true;
        }
        if self.mean_iterations > baseline.mean_iterations + EPS {
            return false;
        }
        // equal iterations too → fewer tokens (cost)
        match (self.mean_tokens, baseline.mean_tokens) {
            (Some(candidate), Some(base)) => candidate + EPS < base,
            _ => false,
        }
    }
}

/// Compare two `eval_run` reports (JSON objects carrying `pass_rate` / `mean_tool_errors` /
/// `mean_iterations`): is `candidate` strictly better than `baseline`? Same lexicographic order as
/// [`SuiteScore::is_better`] — this is what the improve loop's `score_compare` op uses on the
/// report objects the flow passes around.
pub fn report_is_better(candidate: &serde_json::Value, baseline: &serde_json::Value) -> bool {
    fn metrics(v: &serde_json::Value) -> Option<[f64; 4]> {
        if v.get("valid").and_then(|x| x.as_bool()) != Some(true)
            || !v
                .get("mean_tokens")
                .is_some_and(|x| x.is_null() || x.is_number())
        {
            return None;
        }
        Some([
            v.get("pass_rate")?.as_f64()?,
            v.get("mean_check_pass_rate")?.as_f64()?,
            v.get("mean_tool_errors")?.as_f64()?,
            v.get("mean_iterations")?.as_f64()?,
        ])
    }
    const EPS: f64 = 1e-9;
    let (Some([cp, cc, ce, ci]), Some([bp, bc, be, bi])) = (metrics(candidate), metrics(baseline))
    else {
        return false;
    };
    if cp > bp + EPS {
        return true;
    }
    if cp + EPS < bp {
        return false;
    }
    // equal full-pass-rate → more sub-checks passing (partial progress)
    if cc > bc + EPS {
        return true;
    }
    if cc + EPS < bc {
        return false;
    }
    if ce + EPS < be {
        return true;
    }
    if ce > be + EPS {
        return false;
    }
    if ci + EPS < bi {
        return true;
    }
    if ci > bi + EPS {
        return false;
    }
    // equal iterations → fewer tokens (cost)
    match (
        candidate.get("mean_tokens").and_then(|v| v.as_f64()),
        baseline.get("mean_tokens").and_then(|v| v.as_f64()),
    ) {
        (Some(candidate), Some(base)) => candidate + EPS < base,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(pass_rate: f64, check_rate: f64, errors: f64, iters: f64) -> serde_json::Value {
        serde_json::json!({
            "valid": true,
            "pass_rate": pass_rate,
            "mean_check_pass_rate": check_rate,
            "mean_tool_errors": errors,
            "mean_iterations": iters,
            "mean_tokens": null,
        })
    }

    fn result(id: &str, passed: bool, tool_errors: u32, iterations: u32) -> RunResult {
        RunResult {
            task_id: id.into(),
            valid: true,
            passed,
            checks_passed: 0,
            checks_total: 0,
            failed_checks: Vec::new(),
            iterations,
            tool_calls: iterations,
            tool_errors,
            tokens: None,
            wall_ms: 100,
            session_id: None,
            session_db: None,
            flow_db: None,
            timed_out: false,
            note: None,
            transcript: None,
        }
    }

    #[test]
    fn pass_rate_is_weighted() {
        let results = vec![result("a", true, 0, 1), result("b", false, 0, 1)];
        let weights = |id: &str| if id == "a" { 3.0 } else { 1.0 };
        let s = SuiteScore::from_results(&results, weights);
        assert_eq!(s.total, 2);
        assert_eq!(s.passed, 1);
        // For a binary suite the check-rate equals the pass-rate, so the partial-credit scalar fix
        // leaves this case unchanged.
        assert!((s.mean_check_pass_rate - s.pass_rate).abs() < 1e-9);
        assert!((s.pass_rate - 0.75).abs() < 1e-9); // 3/(3+1)
        assert_eq!(s.scalar(), 750);
    }

    #[test]
    fn scalar_tracks_partial_credit() {
        // A candidate that passes no task fully but clears 5/6 sub-checks must tag as 833, not 0 —
        // the scalar tracks mean_check_pass_rate, so partial-credit-only gains read meaningfully.
        let mut r = result("a", false, 0, 1);
        r.checks_passed = 5;
        r.checks_total = 6;
        let s = SuiteScore::from_results(&[r], |_| 1.0);
        assert!((s.pass_rate - 0.0).abs() < 1e-9);
        assert!((s.mean_check_pass_rate - 5.0 / 6.0).abs() < 1e-9);
        assert_eq!(s.scalar(), 833); // round(0.8333 * 1000)
    }

    #[test]
    fn higher_pass_rate_beats_lower() {
        let better = SuiteScore::from_results(&[result("a", true, 5, 9)], |_| 1.0);
        let worse = SuiteScore::from_results(&[result("a", false, 0, 1)], |_| 1.0);
        assert!(better.is_better(&worse));
        assert!(!worse.is_better(&better));
    }

    #[test]
    fn tie_broken_by_fewer_tool_errors_then_iterations() {
        let base = SuiteScore::from_results(&[result("a", true, 4, 5)], |_| 1.0);
        let fewer_errors = SuiteScore::from_results(&[result("a", true, 1, 5)], |_| 1.0);
        assert!(fewer_errors.is_better(&base));

        let same_errors_fewer_iters = SuiteScore::from_results(&[result("a", true, 4, 2)], |_| 1.0);
        assert!(same_errors_fewer_iters.is_better(&base));

        // identical → not better
        let same = SuiteScore::from_results(&[result("a", true, 4, 5)], |_| 1.0);
        assert!(!same.is_better(&base));
    }

    #[test]
    fn partial_credit_breaks_a_full_pass_tie() {
        // Same full-pass-rate (both 0), but the candidate passes more sub-checks → strictly better.
        let base = report(0.0, 0.50, 0.0, 1.0);
        let partial = report(0.0, 0.83, 0.0, 1.0);
        assert!(report_is_better(&partial, &base));
        assert!(!report_is_better(&base, &partial));
        // but a real full-pass regression is never masked by better partial credit.
        let regressed = report(0.0, 1.0, 0.0, 1.0);
        let full = report(1.0, 1.0, 0.0, 1.0);
        assert!(!report_is_better(&regressed, &full));
    }

    #[test]
    fn report_is_better_compares_report_json() {
        let base = report(0.5, 0.5, 2.0, 4.0);
        let higher = report(0.6, 0.6, 9.0, 9.0);
        let tie_fewer_errors = report(0.5, 0.5, 1.0, 4.0);
        let worse = report(0.4, 0.4, 0.0, 1.0);
        assert!(report_is_better(&higher, &base));
        assert!(report_is_better(&tie_fewer_errors, &base));
        assert!(!report_is_better(&worse, &base));
        assert!(!report_is_better(&base, &base));
    }

    #[test]
    fn failed_candidate_does_not_win_on_zeroed_telemetry() {
        let baseline = serde_json::json!({
            "valid": true,
            "pass_rate": 0.0,
            "mean_check_pass_rate": 0.0,
            "mean_tool_errors": 2.0,
            "mean_iterations": 4.0,
            "mean_tokens": 100.0,
        });
        // This is the old failure shape: adapter errors became an all-zero result, which won the
        // efficiency tiebreakers despite producing no valid benchmark observation.
        let crashed = serde_json::json!({
            "valid": false,
            "pass_rate": 0.0,
            "mean_check_pass_rate": 0.0,
            "mean_tool_errors": 0.0,
            "mean_iterations": 0.0,
            "mean_tokens": 0.0,
        });
        assert!(!report_is_better(&crashed, &baseline));
    }

    #[test]
    fn malformed_report_missing_metrics_is_invalid() {
        let baseline = serde_json::json!({
            "valid": true,
            "pass_rate": 0.5,
            "mean_check_pass_rate": 0.5,
            "mean_tool_errors": 2.0,
            "mean_iterations": 4.0,
            "mean_tokens": 100.0,
        });
        let missing_cost_metrics = serde_json::json!({
            "valid": true,
            "pass_rate": 0.5,
            "mean_check_pass_rate": 0.5,
        });
        assert!(!report_is_better(&missing_cost_metrics, &baseline));
        assert!(!report_is_better(&baseline, &missing_cost_metrics));
    }

    #[test]
    fn from_cases_uses_per_task_pass_rate_and_tokens_tiebreak() {
        use crate::metrics::CaseOutcome;
        fn case(id: &str, passes: u32, trials: u32) -> CaseOutcome {
            CaseOutcome {
                task_id: id.into(),
                valid: true,
                trials,
                valid_trials: trials,
                invalid_trials: 0,
                passes,
                pass_rate: passes as f64 / trials as f64,
                mean_check_pass_rate: passes as f64 / trials as f64,
                failed_checks: Vec::new(),
                mean_tool_errors: 0.0,
                mean_iterations: 1.0,
                mean_tokens: Some(0.0),
                mean_wall_ms: 1.0,
                timed_out_any: false,
                sessions: vec![],
                note: None,
                transcript: None,
            }
        }
        // a passes 2/3, b passes 3/3 → weighted pass_rate = (0.667 + 1.0)/2 ≈ 0.833.
        let s = SuiteScore::from_cases(&[case("a", 2, 3), case("b", 3, 3)], |_| 1.0);
        assert!((s.pass_rate - 0.8333).abs() < 1e-3, "{}", s.pass_rate);
        assert_eq!(s.total, 2);
        assert_eq!(s.passed, 1); // only b passed all trials

        // tokens break a full tie (same pass-rate / errors / iters).
        let cheap = serde_json::json!({"valid":true,"pass_rate":1.0,"mean_check_pass_rate":1.0,"mean_tool_errors":0.0,"mean_iterations":1.0,"mean_tokens":100.0});
        let dear = serde_json::json!({"valid":true,"pass_rate":1.0,"mean_check_pass_rate":1.0,"mean_tool_errors":0.0,"mean_iterations":1.0,"mean_tokens":200.0});
        assert!(report_is_better(&cheap, &dear));
        assert!(!report_is_better(&dear, &cheap));
    }
}
