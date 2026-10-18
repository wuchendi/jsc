//! Baseline of passing tests: one path (relative to `test/`) per line.

use std::collections::BTreeSet;

/// Difference between a baseline and the current passing set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// Baseline tests (within the filter) that no longer pass.
    pub regressed: Vec<String>,
    /// Passing tests absent from the baseline.
    pub newly_passing: Vec<String>,
}

/// Parses baseline text; blank lines are ignored.
pub fn parse(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Renders a baseline: sorted, one path per `\n`-terminated line.
pub fn render(set: &BTreeSet<String>) -> String {
    set.iter().flat_map(|p| [p.as_str(), "\n"]).collect()
}

/// Compares `passing` against `baseline`. Only baseline entries starting with
/// one of the `filter` prefixes (all, if empty) can regress.
pub fn diff(baseline: &BTreeSet<String>, passing: &BTreeSet<String>, filter: &[String]) -> Diff {
    let in_scope =
        |p: &String| filter.is_empty() || filter.iter().any(|f| p.starts_with(f.as_str()));
    Diff {
        regressed: baseline
            .iter()
            .filter(|p| in_scope(p) && !passing.contains(*p))
            .cloned()
            .collect(),
        newly_passing: passing.difference(baseline).cloned().collect(),
    }
}

/// New baseline after a run: with no `filter`, exactly `passing`; otherwise
/// the `baseline` entries outside the filter prefixes plus `passing`.
pub fn merge(
    baseline: &BTreeSet<String>,
    passing: &BTreeSet<String>,
    filter: &[String],
) -> BTreeSet<String> {
    baseline
        .iter()
        .filter(|p| !filter.is_empty() && !filter.iter().any(|f| p.starts_with(f.as_str())))
        .chain(passing)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let set = parse("b.js\n\n a.js \r\n");
        assert_eq!(render(&set), "a.js\nb.js\n");
        assert_eq!(render(&BTreeSet::new()), "");
    }

    #[test]
    fn diff_scoped() {
        let base = parse("x/a.js\nx/b.js\ny/c.js\n");
        let pass = parse("x/a.js\nx/new.js\n");
        let all = diff(&base, &pass, &[]);
        assert_eq!(all.regressed, ["x/b.js", "y/c.js"]);
        assert_eq!(all.newly_passing, ["x/new.js"]);
        assert_eq!(diff(&base, &pass, &["x/".to_owned()]).regressed, ["x/b.js"]);
    }

    #[test]
    fn merge_scoped() {
        let base = parse("x/a.js\nx/b.js\ny/c.js\n");
        let pass = parse("x/a.js\nx/new.js\n");
        assert_eq!(merge(&base, &pass, &[]), pass);
        assert_eq!(
            render(&merge(&base, &pass, &["x/".to_owned()])),
            "x/a.js\nx/new.js\ny/c.js\n"
        );
    }
}
