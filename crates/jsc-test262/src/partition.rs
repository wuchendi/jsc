//! Deterministic sharding of a run (`--partition K/N`).
//!
//! Assignment is round-robin over the sorted list of collected test paths
//! (the list the unpartitioned run would consider, skipped tests included):
//! the test at index `i` belongs to shard `i % N + 1`. Neighbouring tests
//! share a directory and cost, so round-robin spreads every directory evenly
//! across the shards. A path absent from the list (a baseline entry whose test
//! file was removed) is assigned by its sorted insertion index, so every path
//! belongs to exactly one shard.

use std::fmt;

/// Shard `k` of `n` (1-based, `1 <= k <= n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    k: usize,
    n: usize,
}

impl Partition {
    /// Parses `K/N`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let malformed = || format!("--partition expects K/N with 1 <= K <= N, got `{text}`");
        let (k, n) = text.split_once('/').ok_or_else(malformed)?;
        let number = |s: &str| {
            s.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| s.parse::<usize>().ok())
                .flatten()
                .ok_or_else(malformed)
        };
        let (k, n) = (number(k)?, number(n)?);
        if n == 0 {
            return Err(format!("--partition `{text}`: N must be at least 1"));
        }
        if k == 0 || k > n {
            return Err(format!("--partition `{text}`: K must be between 1 and {n}"));
        }
        Ok(Self { k, n })
    }

    /// Whether `path` belongs to this shard; `sorted` is the full collected
    /// path list in sorted order.
    pub fn owns(self, sorted: &[String], path: &str) -> bool {
        let index = sorted
            .binary_search_by(|p| p.as_str().cmp(path))
            .unwrap_or_else(|insert| insert);
        index % self.n == self.k - 1
    }

    /// The paths of `sorted` that belong to this shard, in order.
    pub fn select(self, sorted: &[String]) -> Vec<String> {
        sorted
            .iter()
            .enumerate()
            .filter(|(i, _)| i % self.n == self.k - 1)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl fmt::Display for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.k, self.n)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn paths(n: usize) -> Vec<String> {
        let mut v: Vec<String> = (0..n).map(|i| format!("dir{}/t{i:03}.js", i % 3)).collect();
        v.sort();
        v
    }

    #[test]
    fn parses() -> Result<(), String> {
        assert_eq!(Partition::parse("1/1")?, Partition { k: 1, n: 1 });
        assert_eq!(Partition::parse("3/4")?, Partition { k: 3, n: 4 });
        assert_eq!(Partition::parse("4/4")?.to_string(), "4/4");
        for bad in [
            "", "1", "/", "1/", "/4", "0/4", "5/4", "1/0", "0/0", "a/4", "1/b", "-1/4", "+1/4",
            "1/4/2", " 1/4", "1 /4", "1.0/4",
        ] {
            assert!(Partition::parse(bad).is_err(), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn union_is_whole_and_disjoint() -> Result<(), String> {
        for total in [0, 1, 3, 4, 5, 17, 100] {
            let all = paths(total);
            for n in 1..=5 {
                let mut seen = BTreeSet::new();
                let mut count = 0;
                for k in 1..=n {
                    let part = Partition::parse(&format!("{k}/{n}"))?;
                    let shard = part.select(&all);
                    // Balanced: shard sizes differ by at most one.
                    assert!(shard.len() == total / n || shard.len() == total / n + 1);
                    for p in &shard {
                        assert!(part.owns(&all, p));
                        assert!(seen.insert(p.clone()), "{p} in two shards");
                    }
                    count += shard.len();
                }
                assert_eq!(count, total);
                assert_eq!(seen.into_iter().collect::<Vec<_>>(), all);
            }
        }
        Ok(())
    }

    #[test]
    fn deterministic_round_robin() -> Result<(), String> {
        let all = paths(10);
        let part = Partition::parse("2/4")?;
        assert_eq!(part.select(&all), part.select(&all));
        let expected: Vec<String> = [1, 5, 9].iter().map(|&i| all[i].clone()).collect();
        assert_eq!(part.select(&all), expected);
        Ok(())
    }

    #[test]
    fn absent_paths_have_exactly_one_owner() -> Result<(), String> {
        let all = paths(10);
        for absent in ["", "dir0/t000.jsx", "dir1/zzz.js", "zzz.js"] {
            let owners = (1..=4)
                .map(|k| Partition::parse(&format!("{k}/4")))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|p| p.owns(&all, absent))
                .count();
            assert_eq!(owners, 1, "{absent}");
        }
        Ok(())
    }
}
