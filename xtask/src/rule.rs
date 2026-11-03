//! The rule a candidate policy text is judged by (model-prompts §8.1). Both
//! evaluation runners judge through this module.
//!
//! One comparison is the candidate arm's loss against the current arm on one
//! rate: a regression when the loss exceeds δ and a one-sided mid-p Fisher
//! exact test on the two arms' counts rejects "no loss". Each evaluation has
//! one family of comparisons, fixed before any data and adjusted with Holm at
//! a family-wise α. The verdict is `fail` when any comparison is a regression,
//! `inconclusive` when none is but the floor is unmet or a comparison has no
//! counted run to compare, and `pass` otherwise. There is no absolute pass
//! line.

/// The smallest loss that is tested: a comparison whose loss is at most δ
/// enters the family with p = 1 and never rejects.
pub(crate) const DELTA: f64 = 0.05;
/// The family-wise significance level the Holm adjustment holds.
pub(crate) const ALPHA: f64 = 0.10;
/// The fewest judged runs every arm needs on every case before the verdict
/// can be `pass`. Failed and unjudged runs are compared, in their own rows,
/// but they are not judged runs, so they do not count here: only judged runs
/// enter the probe rates this floor is for.
pub(crate) const FLOOR: u64 = 20;

/// One arm's counts on one rate: how many of `total` counted runs showed the
/// outcome the comparison measures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub outcome: u64,
    pub total: u64,
}
impl Counts {
    pub(crate) fn new(outcome: u64, total: u64) -> Self {
        debug_assert!(outcome <= total, "{outcome} of {total}");
        Self { outcome, total }
    }
    pub(crate) fn rate(&self) -> Option<f64> {
        (self.total > 0).then(|| self.outcome as f64 / self.total as f64)
    }
}

/// Which way a rate loses: a pass rate falls, a violation or failure rate
/// rises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Worse {
    Lower,
    Higher,
}

/// One comparison of an evaluation's family, before any data is read: the
/// report's name for it, the direction its rate loses in, and both arms'
/// counts.
#[derive(Clone, Debug)]
pub(crate) struct Comparison {
    pub name: String,
    pub worse: Worse,
    pub current: Counts,
    pub candidate: Counts,
}

/// One comparison with its result: both arms' counts and rates, the loss, the
/// test's p-value, its Holm threshold and whether it rejected.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub name: String,
    pub worse: Worse,
    pub current: Counts,
    pub candidate: Counts,
    /// The current rate less the candidate's in the losing direction; `None`
    /// when an arm counted no runs.
    pub loss: Option<f64>,
    pub p: f64,
    pub threshold: f64,
    pub rejected: bool,
}
impl Row {
    fn of(comparison: &Comparison) -> Self {
        let (current, candidate) = (comparison.current, comparison.candidate);
        let loss = match (current.rate(), candidate.rate()) {
            (Some(current), Some(candidate)) => Some(match comparison.worse {
                Worse::Lower => current - candidate,
                Worse::Higher => candidate - current,
            }),
            _ => None,
        };
        let p = if loss.is_some() && exceeds_delta(comparison) {
            match comparison.worse {
                Worse::Lower => mid_p(candidate, current),
                Worse::Higher => mid_p(current, candidate),
            }
        } else {
            1.
        };
        Self {
            name: comparison.name.clone(),
            worse: comparison.worse,
            current,
            candidate,
            loss,
            p,
            threshold: 0.,
            rejected: false,
        }
    }
    /// The row's line in a comparison table.
    pub(crate) fn line(&self) -> String {
        let cell = |counts: Counts| match counts.rate() {
            Some(rate) => format!("{}/{} ({rate:.3})", counts.outcome, counts.total),
            None => "–".to_owned(),
        };
        let loss = self
            .loss
            .map_or_else(|| "–".to_owned(), |loss| format!("{loss:.3}"));
        format!(
            "| {} | {} | {} | {loss} | {} | {:.4} | {} |",
            self.name,
            cell(self.current),
            cell(self.candidate),
            probability(self.p),
            self.threshold,
            if self.rejected { "yes" } else { "no" },
        )
    }
}

/// The comparison table's header, one row per comparison.
pub(crate) const COMPARISON_HEADER: &str = "| comparison | current | candidate | loss | p | Holm threshold | rejected |\n| --- | --- | --- | --- | --- | --- | --- |\n";

/// The three-valued verdict (`fail`, `pass` or `inconclusive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

/// One evaluation's result: the family's rows and the verdict over them.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub rows: Vec<Row>,
    pub verdict: Verdict,
}

/// Judges a family of comparisons fixed before any data. `min_judged` is the
/// fewest judged runs any arm ran any case. A comparison an arm counted no run
/// for cannot be made, and a family with one cannot pass. `fail` comes first,
/// then `inconclusive`, then `pass` (model-prompts §8.1).
pub(crate) fn judge(comparisons: &[Comparison], min_judged: u64) -> Rule {
    let mut rows: Vec<Row> = comparisons.iter().map(Row::of).collect();
    holm(&mut rows);
    let incomplete = rows.iter().any(|row| row.loss.is_none());
    let verdict = if rows.iter().any(|row| row.rejected) {
        Verdict::Fail
    } else if min_judged >= FLOOR && !incomplete {
        Verdict::Pass
    } else {
        Verdict::Inconclusive
    };
    Rule { rows, verdict }
}

/// The verdict as a report line: the comparisons that rejected, or why the
/// evaluation cannot pass. `case` names what the floor counts, and `subject`
/// whose verdict it is, such as `candidate`.
pub(crate) fn verdict_line(rule: &Rule, min_judged: u64, case: &str, subject: &str) -> String {
    match rule.verdict {
        Verdict::Fail => {
            let rejected: Vec<String> = rule
                .rows
                .iter()
                .filter(|row| row.rejected)
                .map(|row| {
                    format!(
                        "{} {} from {:.3} to {:.3} (loss {}, p {}, Holm threshold {:.4})",
                        row.name,
                        match row.worse {
                            Worse::Lower => "falls",
                            Worse::Higher => "rises",
                        },
                        row.current.rate().unwrap_or(0.),
                        row.candidate.rate().unwrap_or(0.),
                        row.loss
                            .map_or_else(|| "–".to_owned(), |loss| format!("{loss:.3}")),
                        probability(row.p),
                        row.threshold,
                    )
                })
                .collect();
            format!("{subject} fails: {}.", rejected.join("; "))
        }
        Verdict::Pass => format!(
            "{subject} passes: no comparison is a regression, and every arm has at least {FLOOR} judged runs of every {case}."
        ),
        Verdict::Inconclusive => {
            let mut reasons: Vec<String> = rule
                .rows
                .iter()
                .filter(|row| row.loss.is_none())
                .map(|row| format!("{} has no counted run in an arm", row.name))
                .collect();
            if min_judged < FLOOR {
                reasons.push(format!(
                    "the fewest judged runs on a {case} are {min_judged}, under the floor of {FLOOR}"
                ));
            }
            format!(
                "{subject} is inconclusive: no comparison is a regression, but {}.",
                reasons.join(", and ")
            )
        }
    }
}

/// Whether the comparison's loss exceeds δ, decided in integers: `f64` rates
/// put some run counts that sit exactly on `current − δ` on either side of it.
fn exceeds_delta(comparison: &Comparison) -> bool {
    let (a, an, b, bn) = match comparison.worse {
        Worse::Lower => (
            comparison.current.outcome,
            comparison.current.total,
            comparison.candidate.outcome,
            comparison.candidate.total,
        ),
        Worse::Higher => (
            comparison.candidate.outcome,
            comparison.candidate.total,
            comparison.current.outcome,
            comparison.current.total,
        ),
    };
    // a/an − b/bn > 1/20  ⇔  20·(a·bn − b·an) > an·bn
    20 * a * bn > 20 * b * an + an * bn
}

/// Holm's step-down adjustment at [`ALPHA`]: the row at rank `i` of the
/// ascending p-values has the threshold `α / (m − i)`, and no row rejects
/// after one fails.
fn holm(rows: &mut [Row]) {
    let m = rows.len();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| rows[a].p.total_cmp(&rows[b].p));
    let mut stopped = false;
    for (rank, index) in order.into_iter().enumerate() {
        rows[index].threshold = ALPHA / (m - rank) as f64;
        rows[index].rejected = !stopped && rows[index].p <= rows[index].threshold;
        stopped |= !rows[index].rejected;
    }
}

/// The one-sided mid-p Fisher exact test on a 2×2 table: the lower tail of
/// `low`'s count, P(X < observed) + P(X = observed) / 2, under "both arms
/// share one rate". The hypergeometric probabilities come from `f64`
/// log-factorials, so counts in the hundreds stay accurate enough; no binomial
/// coefficients are formed.
fn mid_p(low: Counts, high: Counts) -> f64 {
    let draws = low.total;
    let population = low.total + high.total;
    let successes = low.outcome + high.outcome;
    let observed = low.outcome;
    let log_factorial = log_factorials(population);
    let choose = |n: u64, k: u64| {
        log_factorial[n as usize] - log_factorial[k as usize] - log_factorial[(n - k) as usize]
    };
    let probability = |x: u64| {
        let log = choose(successes, x) + choose(population - successes, draws - x)
            - choose(population, draws);
        log.exp()
    };
    let lowest = draws.saturating_sub(population - successes);
    let mut p = 0.5 * probability(observed);
    for x in lowest..observed {
        p += probability(x);
    }
    p.min(1.)
}

/// `ln(n!)` for every `n` up to `n`, in `f64`.
fn log_factorials(n: u64) -> Vec<f64> {
    let mut table = Vec::with_capacity(n as usize + 1);
    table.push(0.);
    for i in 1..=n {
        table.push(table[(i - 1) as usize] + (i as f64).ln());
    }
    table
}

/// A probability as a report prints it: `1` for the gate's p, four decimals
/// down to the Holm thresholds' scale, and scientific notation below.
pub(crate) fn probability(p: f64) -> String {
    if p >= 1. {
        "1".to_owned()
    } else if p >= 0.0001 {
        format!("{p:.4}")
    } else {
        format!("{p:.2e}")
    }
}

#[cfg(all(test, feature = "eval-tests"))]
mod tests {
    use super::*;

    /// One comparison with both arms' counted outcome and runs.
    fn comparison(
        name: &str,
        worse: Worse,
        current: (u64, u64),
        candidate: (u64, u64),
    ) -> Comparison {
        Comparison {
            name: name.into(),
            worse,
            current: Counts::new(current.0, current.1),
            candidate: Counts::new(candidate.0, candidate.1),
        }
    }
    /// One row for the Holm tests.
    fn row(name: &str, p: f64) -> Row {
        Row {
            name: name.into(),
            worse: Worse::Lower,
            current: Counts::new(1, 2),
            candidate: Counts::new(1, 2),
            loss: Some(0.),
            p,
            threshold: 0.,
            rejected: false,
        }
    }
    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-9
    }

    /// The mid-p values against references computed independently with exact
    /// rational arithmetic and stated here as the fractions they are: small
    /// counts, and counts in the hundreds as the pooled rates are.
    #[test]
    fn the_mid_p_values_match_the_exact_references() {
        for (low, high, numerator, denominator) in [
            // 4/10 against 9/10.
            (Counts::new(4, 10), Counts::new(9, 10), 39., 2584.),
            // 24/40 against 30/40.
            (
                Counts::new(24, 40),
                Counts::new(30, 40),
                56364741159.,
                691135163467.,
            ),
            // 8/20 against 16/20.
            (Counts::new(8, 20), Counts::new(16, 20), 637., 99789.),
            // 2/100 against 12/100.
            (
                Counts::new(2, 100),
                Counts::new(12, 100),
                1676724311.,
                589617197112.,
            ),
            // Counts in the hundreds as the pooled rates are: 1/150 against
            // 10/150.
            (
                Counts::new(1, 150),
                Counts::new(10, 150),
                644041.,
                231403782.,
            ),
            // The far tail of a larger pool: 2/200 against 14/200.
            (
                Counts::new(2, 200),
                Counts::new(14, 200),
                10835633988.,
                11064526330991.,
            ),
        ] {
            let expected = numerator / denominator;
            let p = mid_p(low, high);
            assert!(
                close(p, expected),
                "mid_p({}/{} against {}/{}) is {p}, not {expected} ({numerator}/{denominator})",
                low.outcome,
                low.total,
                high.outcome,
                high.total,
            );
        }
    }

    /// Holm applies the ordered thresholds at a family-wise α and stops at the
    /// first comparison it cannot reject.
    #[test]
    fn holm_applies_the_ordered_thresholds() {
        let mut rows = [row("a", 0.001), row("b", 0.06), row("c", 0.07)];
        holm(&mut rows);
        // 0.10/3, 0.10/2, 0.10/1 in the ascending p order.
        assert_eq!(rows[0].threshold, 0.1 / 3.);
        assert_eq!(rows[1].threshold, 0.1 / 2.);
        assert_eq!(rows[2].threshold, 0.1 / 1.);
        assert!(rows[0].rejected, "0.001 is under 0.10/3");
        assert!(!rows[1].rejected, "0.06 is over 0.10/2");
        assert!(
            !rows[2].rejected,
            "0.07 is under 0.10, but the step-down already stopped"
        );
        let mut rows = [row("a", 0.001), row("b", 0.02)];
        holm(&mut rows);
        assert!(rows.iter().all(|row| row.rejected), "both clear their rank");
    }

    /// A loss at or under δ enters the family with p = 1 and never rejects,
    /// even where the test alone would reject.
    #[test]
    fn a_loss_at_or_under_delta_never_rejects() {
        for (current, candidate) in [(20, 19), (40, 38), (20, 20)] {
            let rule = judge(
                &[comparison(
                    "probe",
                    Worse::Lower,
                    (current, current),
                    (candidate, current),
                )],
                20,
            );
            let row = &rule.rows[0];
            assert_eq!(row.p, 1., "{candidate}/{current}");
            assert!(!row.rejected, "{candidate}/{current}");
            assert_eq!(rule.verdict, Verdict::Pass, "{candidate}/{current}");
        }
        // Just past δ the test runs instead of the gate: 20/20 against 18/20
        // is a loss of 0.10 and a p-value of about 0.12.
        let rule = judge(&[comparison("probe", Worse::Lower, (20, 20), (18, 20))], 20);
        assert!(rule.rows[0].p < 0.2, "{}", rule.rows[0].p);
        assert_eq!(rule.verdict, Verdict::Pass, "the loss is δ plus noise");
        let rule = judge(&[comparison("probe", Worse::Lower, (20, 20), (12, 20))], 20);
        assert!(rule.rows[0].p < 0.01, "{}", rule.rows[0].p);
        assert_eq!(rule.verdict, Verdict::Fail);
    }

    /// The verdict has three values: identical counts pass at the floor,
    /// fall to inconclusive below it, and a planted loss fails at any n and
    /// names its comparison.
    #[test]
    fn the_verdict_is_three_valued() {
        let identical = [comparison("probe", Worse::Lower, (20, 20), (20, 20))];
        assert_eq!(judge(&identical, 20).verdict, Verdict::Pass);
        assert_eq!(judge(&identical, 19).verdict, Verdict::Inconclusive);
        let planted = [comparison("probe", Worse::Lower, (20, 20), (10, 20))];
        let rule = judge(&planted, 10);
        assert_eq!(rule.verdict, Verdict::Fail);
        assert!(rule.rows[0].rejected);
        let line = verdict_line(&rule, 10, "scenario", "candidate");
        assert!(
            line.starts_with("candidate fails: probe falls from 1.000 to 0.500"),
            "{line}"
        );
        let line = verdict_line(&judge(&identical, 19), 19, "scenario", "candidate");
        assert!(
            line.contains("the fewest judged runs on a scenario are 19"),
            "{line}"
        );
    }

    /// A higher rate that loses is tested on the arm with the lower count,
    /// and its loss reads in the direction the rate rises.
    #[test]
    fn a_higher_rate_that_rises_rejects() {
        let rule = judge(
            &[comparison("violations", Worse::Higher, (1, 40), (12, 40))],
            40,
        );
        let row = &rule.rows[0];
        assert!(close(row.loss.unwrap(), 11. / 40.), "{:?}", row.loss);
        assert!(row.p < 0.01, "{}", row.p);
        assert!(row.rejected);
        let line = verdict_line(&rule, 40, "scenario", "candidate");
        assert!(
            line.contains("violations rises from 0.025 to 0.300"),
            "{line}"
        );
        // A candidate that lowers the rate is no regression.
        let rule = judge(
            &[comparison("violations", Worse::Higher, (12, 40), (1, 40))],
            40,
        );
        assert_eq!(rule.rows[0].p, 1.);
        assert_eq!(rule.verdict, Verdict::Pass);
    }

    /// A comparison with no counted run in an arm cannot be made: it enters
    /// with p = 1, and the verdict is `inconclusive` even at the floor, so no
    /// `pass` can rest on data that was never judged (model-prompts §8.1).
    #[test]
    fn a_comparison_without_counts_makes_the_verdict_inconclusive() {
        let rule = judge(&[comparison("empty", Worse::Lower, (0, 0), (0, 0))], 20);
        assert_eq!(rule.rows[0].loss, None);
        assert_eq!(rule.rows[0].p, 1.);
        assert_eq!(rule.verdict, Verdict::Inconclusive);
        let line = verdict_line(&rule, 20, "scenario", "candidate");
        assert!(
            line.contains("empty has no counted run in an arm"),
            "{line}"
        );
        let rule = judge(&[comparison("empty", Worse::Lower, (0, 0), (0, 0))], 0);
        assert!(
            verdict_line(&rule, 0, "scenario", "candidate")
                .contains("empty has no counted run in an arm, and the fewest judged runs"),
            "both reasons are named"
        );
        let rule = judge(
            &[comparison("one-sided", Worse::Lower, (20, 20), (0, 0))],
            20,
        );
        assert_eq!(rule.rows[0].loss, None);
        assert_eq!(rule.verdict, Verdict::Inconclusive);
    }
}
