//! Test eligibility: decides whether a test runs (and how) or is skipped.

use std::collections::BTreeSet;

use crate::frontmatter::Meta;

/// Features jsc does not support. Entries ending in `*` match by prefix.
#[derive(Debug, Clone, Default)]
pub struct Denylist {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
}

impl Denylist {
    /// Parses one feature per line; `#` comments and blank lines are ignored.
    pub fn parse(text: &str) -> Self {
        let mut deny = Self::default();
        for line in text.lines() {
            let entry = line.split('#').next().unwrap_or("").trim();
            if let Some(prefix) = entry.strip_suffix('*') {
                deny.prefixes.push(prefix.to_owned());
            } else if !entry.is_empty() {
                deny.exact.insert(entry.to_owned());
            }
        }
        deny
    }

    /// Whether `feature` is denied.
    pub fn matches(&self, feature: &str) -> bool {
        self.exact.contains(feature)
            || self
                .prefixes
                .iter()
                .any(|p| feature.starts_with(p.as_str()))
    }
}

/// How an eligible test is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Strict-mode directive plus the official harness.
    Strict,
    /// Source run unmodified (`raw` flag).
    Raw,
    /// `async` flag: like [`Mode::Strict`] plus a host `print` prelude and
    /// `doneprintHandle.js`; judged by the `$DONE` completion line on stdout.
    Async,
}

/// Harness file defining `$DONE`, prepended to every [`Mode::Async`] test.
pub const DONE_PRINT: &str = "doneprintHandle.js";

/// Why a test was not run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// Unsupported flag (`module`, `CanBlockIs*`, or `async` together with
    /// `raw`, which leaves no place to prepend `doneprintHandle.js`).
    Flag(String),
    /// Sloppy-mode-only test.
    NoStrict,
    /// Denied feature.
    Feature(String),
    /// Harness include that does not compile with jsc or is excluded.
    Include(String),
}

impl SkipReason {
    /// Stable short label for reports, e.g. `feature:Symbol`.
    pub fn label(&self) -> String {
        match self {
            Self::Flag(f) => format!("flag:{f}"),
            Self::NoStrict => "noStrict".to_owned(),
            Self::Feature(f) => format!("feature:{f}"),
            Self::Include(i) => format!("include:{i}"),
        }
    }
}

/// Classifies a test by its metadata. `compiling` holds the harness include
/// names that compile with jsc (`assert.js` / `sta.js` are implicit and never
/// listed); async tests also need [`DONE_PRINT`] in it.
pub fn classify(
    meta: &Meta,
    deny: &Denylist,
    compiling: &BTreeSet<String>,
) -> Result<Mode, SkipReason> {
    let has = |flag: &str| meta.flags.iter().any(|f| f == flag);
    for flag in ["module", "CanBlockIsFalse", "CanBlockIsTrue"] {
        if has(flag) {
            return Err(SkipReason::Flag(flag.to_owned()));
        }
    }
    let raw = has("raw");
    let is_async = has("async");
    if raw && is_async {
        return Err(SkipReason::Flag("async".to_owned()));
    }
    if meta
        .negative
        .as_ref()
        .is_some_and(|n| n.phase == "resolution")
    {
        return Err(SkipReason::Flag("module".to_owned()));
    }
    if !raw && has("noStrict") {
        return Err(SkipReason::NoStrict);
    }
    if let Some(f) = meta.features.iter().find(|f| deny.matches(f)) {
        return Err(SkipReason::Feature(f.clone()));
    }
    if raw {
        return Ok(Mode::Raw);
    }
    let implicit = is_async.then_some(DONE_PRINT);
    if let Some(i) = implicit
        .into_iter()
        .chain(meta.includes.iter().map(String::as_str))
        .find(|i| !compiling.contains(*i))
    {
        return Err(SkipReason::Include(i.to_owned()));
    }
    Ok(if is_async { Mode::Async } else { Mode::Strict })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::Negative;

    fn meta(flags: &[&str], features: &[&str], includes: &[&str]) -> Meta {
        let v = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect();
        Meta {
            flags: v(flags),
            features: v(features),
            includes: v(includes),
            negative: None,
        }
    }

    fn run(m: &Meta) -> Result<Mode, SkipReason> {
        let deny = Denylist::parse("# unsupported\nSymbol*\n\nProxy  # exact\n");
        let compiling = BTreeSet::from(["compareArray.js".to_owned(), DONE_PRINT.to_owned()]);
        classify(m, &deny, &compiling)
    }

    #[test]
    fn flag_rules() {
        for f in ["module", "CanBlockIsFalse", "CanBlockIsTrue"] {
            assert_eq!(run(&meta(&[f], &[], &[])), Err(SkipReason::Flag(f.into())));
        }
        let mut m = meta(&[], &[], &[]);
        m.negative = Some(Negative {
            phase: "resolution".into(),
            kind: "SyntaxError".into(),
        });
        assert_eq!(run(&m).map_err(|r| r.label()), Err("flag:module".into()));
        assert_eq!(
            run(&meta(&["noStrict"], &[], &[])),
            Err(SkipReason::NoStrict)
        );
    }

    #[test]
    fn modes() {
        assert_eq!(run(&meta(&["onlyStrict"], &[], &[])), Ok(Mode::Strict));
        assert_eq!(run(&meta(&[], &[], &["compareArray.js"])), Ok(Mode::Strict));
        assert_eq!(run(&meta(&["raw", "noStrict"], &[], &[])), Ok(Mode::Raw));
        assert_eq!(
            run(&meta(&["raw"], &["Symbol"], &[])),
            Err(SkipReason::Feature("Symbol".into()))
        );
    }

    #[test]
    fn async_tests() {
        assert_eq!(run(&meta(&["async"], &[], &[])), Ok(Mode::Async));
        assert_eq!(
            run(&meta(&["async"], &[], &["compareArray.js", DONE_PRINT])),
            Ok(Mode::Async)
        );
        assert_eq!(
            run(&meta(&["async", "raw"], &[], &[])),
            Err(SkipReason::Flag("async".into()))
        );
        assert_eq!(
            run(&meta(&["async", "module"], &[], &[])),
            Err(SkipReason::Flag("module".into()))
        );
        assert_eq!(
            run(&meta(&["async"], &[], &["asyncHelpers.js"])).map_err(|r| r.label()),
            Err("include:asyncHelpers.js".into())
        );
        let deny = Denylist::default();
        let no_done = BTreeSet::from(["compareArray.js".to_owned()]);
        assert_eq!(
            classify(&meta(&["async"], &[], &[]), &deny, &no_done).map_err(|r| r.label()),
            Err("include:doneprintHandle.js".into())
        );
        assert_eq!(
            classify(&meta(&[], &[], &[]), &deny, &no_done),
            Ok(Mode::Strict)
        );
    }

    #[test]
    fn denylist_and_includes() {
        assert_eq!(
            run(&meta(&[], &["let", "Symbol.iterator"], &[])).map_err(|r| r.label()),
            Err("feature:Symbol.iterator".into())
        );
        assert_eq!(
            run(&meta(&[], &["Proxy"], &[])),
            Err(SkipReason::Feature("Proxy".into()))
        );
        assert_eq!(run(&meta(&[], &["Proxy.revocable"], &[])), Ok(Mode::Strict));
        assert_eq!(
            run(&meta(&[], &[], &["compareArray.js", "nonCompiling.js"])).map_err(|r| r.label()),
            Err("include:nonCompiling.js".into())
        );
    }
}
