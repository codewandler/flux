//! Suite scoring and the keep/revert comparison.
//!
//! The primary signal is the **weighted pass-rate**. Cost (tool-errors, then iterations) is a strict
//! *lexicographic* tie-breaker, never blended in — a faster-but-wronger agent must not outscore a
//! slower-but-correct one. The improvement loop adopts a candidate only when [`SuiteScore::is_better`]
//! holds (and, separately, the dev-gate is green).

use crate::metrics::RunResult;

/// An aggregate score over a suite run.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct SuiteScore {
    /// Σ(weightᵢ·passedᵢ) / Σ(weightᵢ), in `[0,1]`.
    pub pass_rate: f64,
    pub total: u32,
    pub passed: u32,
    pub total_weight: f64,
    pub mean_tool_errors: f64,
    pub mean_iterations: f64,
    pub mean_wall_ms: f64,
}

impl SuiteScore {
    /// Aggregate per-task results. `weight_of(task_id)` supplies each task's weight (default 1.0 for
    /// unknown ids).
    pub fn from_results(results: &[RunResult], weight_of: impl Fn(&str) -> f64) -> Self {
        let n = results.len() as f64;
        let mut total_weight = 0.0;
        let mut weighted_pass = 0.0;
        let mut sum_tool_errors = 0u64;
        let mut sum_iterations = 0u64;
        let mut sum_wall_ms = 0u64;
        let mut passed = 0u32;
        for r in results {
            let w = weight_of(&r.task_id);
            total_weight += w;
            if r.passed {
                weighted_pass += w;
                passed += 1;
            }
            sum_tool_errors += r.tool_errors as u64;
            sum_iterations += r.iterations as u64;
            sum_wall_ms += r.wall_ms;
        }
        SuiteScore {
            pass_rate: if total_weight > 0.0 {
                weighted_pass / total_weight
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
            mean_wall_ms: if n > 0.0 { sum_wall_ms as f64 / n } else { 0.0 },
        }
    }

    /// A single committable scalar for tags/reporting: `round(pass_rate * 1000)` (e.g. 0.857 → 857).
    pub fn scalar(&self) -> u32 {
        (self.pass_rate * 1000.0).round() as u32
    }

    /// Is `self` strictly better than `baseline`? Lexicographic: higher pass-rate wins; on a tie,
    /// fewer mean tool-errors; on a further tie, fewer mean iterations. A small epsilon absorbs
    /// float noise on the rate comparison.
    pub fn is_better(&self, baseline: &SuiteScore) -> bool {
        const EPS: f64 = 1e-9;
        if self.pass_rate > baseline.pass_rate + EPS {
            return true;
        }
        if self.pass_rate + EPS < baseline.pass_rate {
            return false;
        }
        // equal pass-rate
        if self.mean_tool_errors + EPS < baseline.mean_tool_errors {
            return true;
        }
        if self.mean_tool_errors > baseline.mean_tool_errors + EPS {
            return false;
        }
        // equal tool-errors too
        self.mean_iterations + EPS < baseline.mean_iterations
    }
}

/// Compare two `eval_run` reports (JSON objects carrying `pass_rate` / `mean_tool_errors` /
/// `mean_iterations`): is `candidate` strictly better than `baseline`? Same lexicographic order as
/// [`SuiteScore::is_better`] — this is what the improve loop's `score_compare` op uses on the
/// report objects the flow passes around.
pub fn report_is_better(candidate: &serde_json::Value, baseline: &serde_json::Value) -> bool {
    fn f(v: &serde_json::Value, k: &str) -> f64 {
        v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
    }
    const EPS: f64 = 1e-9;
    let (cp, bp) = (f(candidate, "pass_rate"), f(baseline, "pass_rate"));
    if cp > bp + EPS {
        return true;
    }
    if cp + EPS < bp {
        return false;
    }
    let (ce, be) = (
        f(candidate, "mean_tool_errors"),
        f(baseline, "mean_tool_errors"),
    );
    if ce + EPS < be {
        return true;
    }
    if ce > be + EPS {
        return false;
    }
    f(candidate, "mean_iterations") + EPS < f(baseline, "mean_iterations")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, passed: bool, tool_errors: u32, iterations: u32) -> RunResult {
        RunResult {
            task_id: id.into(),
            passed,
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
        }
    }

    #[test]
    fn pass_rate_is_weighted() {
        let results = vec![result("a", true, 0, 1), result("b", false, 0, 1)];
        let weights = |id: &str| if id == "a" { 3.0 } else { 1.0 };
        let s = SuiteScore::from_results(&results, weights);
        assert_eq!(s.total, 2);
        assert_eq!(s.passed, 1);
        assert!((s.pass_rate - 0.75).abs() < 1e-9); // 3/(3+1)
        assert_eq!(s.scalar(), 750);
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
    fn report_is_better_compares_report_json() {
        let base =
            serde_json::json!({"pass_rate": 0.5, "mean_tool_errors": 2.0, "mean_iterations": 4.0});
        let higher =
            serde_json::json!({"pass_rate": 0.6, "mean_tool_errors": 9.0, "mean_iterations": 9.0});
        let tie_fewer_errors =
            serde_json::json!({"pass_rate": 0.5, "mean_tool_errors": 1.0, "mean_iterations": 4.0});
        let worse =
            serde_json::json!({"pass_rate": 0.4, "mean_tool_errors": 0.0, "mean_iterations": 1.0});
        assert!(report_is_better(&higher, &base));
        assert!(report_is_better(&tie_fewer_errors, &base));
        assert!(!report_is_better(&worse, &base));
        assert!(!report_is_better(&base, &base));
    }
}
