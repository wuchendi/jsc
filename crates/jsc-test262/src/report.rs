//! Aggregation of per-test results into a per-directory summary.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::filter::SkipReason;
use crate::outcome::Outcome;

/// Counts for one directory (first two path segments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    /// Directory key, e.g. `built-ins/Array`.
    pub dir: String,
    /// Passing tests.
    pub pass: usize,
    /// Tests that were run (not skipped).
    pub eligible: usize,
    /// All collected tests.
    pub total: usize,
}

/// Aggregated results.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Per-directory rows, sorted by directory.
    pub rows: Vec<Row>,
    /// Totals over all directories (`dir` is `OVERALL`).
    pub overall: Row,
    /// Count per outcome label.
    pub outcomes: BTreeMap<&'static str, usize>,
    /// Count per skip-reason label.
    pub skips: BTreeMap<String, usize>,
}

impl Report {
    /// Builds a report from `(path, result)` records.
    pub fn new(records: &[(String, Result<Outcome, SkipReason>)]) -> Self {
        let mut dirs: BTreeMap<String, Row> = BTreeMap::new();
        let mut report = Self {
            overall: Row {
                dir: "OVERALL".to_owned(),
                ..Row::default()
            },
            ..Self::default()
        };
        for (path, result) in records {
            let segs: Vec<&str> = path.split('/').collect();
            let dir = segs[..segs.len().saturating_sub(1).min(2)].join("/");
            let row = dirs.entry(dir.clone()).or_insert_with(|| Row {
                dir,
                ..Row::default()
            });
            for r in [&mut *row, &mut report.overall] {
                r.total += 1;
                r.eligible += usize::from(result.is_ok());
                r.pass += usize::from(*result == Ok(Outcome::Pass));
            }
            match result {
                Ok(o) => *report.outcomes.entry(o.label()).or_default() += 1,
                Err(s) => *report.skips.entry(s.label()).or_default() += 1,
            }
        }
        report.rows = dirs.into_values().collect();
        report
    }
}

#[expect(clippy::cast_precision_loss, reason = "test counts are far below 2^52")]
fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 * 100.0 / d as f64
    }
}

/// Renders the report as a plain-text table followed by an `OVERALL` line.
pub fn render_table(report: &Report) -> String {
    let width = report
        .rows
        .iter()
        .map(|r| r.dir.len())
        .max()
        .unwrap_or(0)
        .max(9);
    let mut out = format!(
        "{:<width$} {:>7} {:>8} {:>7} {:>7}\n",
        "directory", "pass", "eligible", "total", "pass%"
    );
    for r in &report.rows {
        let _ = writeln!(
            out,
            "{:<width$} {:>7} {:>8} {:>7} {:>6.1}%",
            r.dir,
            r.pass,
            r.eligible,
            r.total,
            pct(r.pass, r.eligible)
        );
    }
    let o = &report.overall;
    let _ = writeln!(
        out,
        "OVERALL: {} pass / {} eligible ({:.1}%) / {} total ({:.1}%)",
        o.pass,
        o.eligible,
        pct(o.pass, o.eligible),
        o.total,
        pct(o.pass, o.total)
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_table() {
        let records = vec![
            ("built-ins/Array/a.js".to_owned(), Ok(Outcome::Pass)),
            ("built-ins/Array/sub/b.js".to_owned(), Ok(Outcome::Fail)),
            ("built-ins/Array/c.js".to_owned(), Err(SkipReason::NoStrict)),
            ("language/expressions/x/d.js".to_owned(), Ok(Outcome::Pass)),
            (
                "language/e.js".to_owned(),
                Err(SkipReason::Feature("Symbol".into())),
            ),
        ];
        let report = Report::new(&records);
        let rows: Vec<_> = report
            .rows
            .iter()
            .map(|r| (r.dir.as_str(), r.pass, r.eligible, r.total))
            .collect();
        assert_eq!(
            rows,
            [
                ("built-ins/Array", 1, 2, 3),
                ("language", 0, 0, 1),
                ("language/expressions", 1, 1, 1)
            ]
        );
        assert_eq!(
            (
                report.overall.pass,
                report.overall.eligible,
                report.overall.total
            ),
            (2, 3, 5)
        );
        assert_eq!(report.outcomes, BTreeMap::from([("fail", 1), ("pass", 2)]));
        assert_eq!(
            report.skips,
            BTreeMap::from([("feature:Symbol".to_owned(), 1), ("noStrict".to_owned(), 1)])
        );
        let table = render_table(&report);
        assert!(
            table.contains("built-ins/Array            1        2       3   50.0%\n"),
            "{table}"
        );
        assert!(
            table.ends_with("OVERALL: 2 pass / 3 eligible (66.7%) / 5 total (40.0%)\n"),
            "{table}"
        );
    }
}
