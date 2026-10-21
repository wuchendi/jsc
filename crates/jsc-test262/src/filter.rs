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
}

/// Why a test was not run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// Unsupported flag (`module`, `async`, `CanBlockIs*`).
    Flag(String),
    /// Sloppy-mode-only test.
    NoStrict,
    /// Denied feature.
    Feature(String),
    /// Harness include that does not compile with jsc, or is kept out of M1.
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
/// listed).
pub fn classify(
    meta: &Meta,
    deny: &Denylist,
    compiling: &BTreeSet<String>,
) -> Result<Mode, SkipReason> {
    let has = |flag: &str| meta.flags.iter().any(|f| f == flag);
    for flag in ["module", "async", "CanBlockIsFalse", "CanBlockIsTrue"] {
        if has(flag) {
            return Err(SkipReason::Flag(flag.to_owned()));
        }
    }
    if meta
        .negative
        .as_ref()
        .is_some_and(|n| n.phase == "resolution")
    {
        return Err(SkipReason::Flag("module".to_owned()));
    }
    let raw = has("raw");
    if !raw && has("noStrict") {
        return Err(SkipReason::NoStrict);
    }
    if let Some(f) = meta.features.iter().find(|f| deny.matches(f)) {
        return Err(SkipReason::Feature(f.clone()));
    }
    if raw {
        return Ok(Mode::Raw);
    }
    if let Some(i) = meta
        .includes
        .iter()
        .find(|i| !compiling.contains(i.as_str()))
    {
        return Err(SkipReason::Include(i.clone()));
    }
    Ok(Mode::Strict)
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
        let compiling = BTreeSet::from(["compareArray.js".to_owned()]);
        classify(m, &deny, &compiling)
    }

    #[test]
    fn flag_rules() {
        for f in ["module", "async", "CanBlockIsFalse", "CanBlockIsTrue"] {
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
            run(&meta(&[], &[], &["compareArray.js", "propertyHelper.js"])).map_err(|r| r.label()),
            Err("include:propertyHelper.js".into())
        );
    }
}
