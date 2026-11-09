//! Case mapping tables, from Rust std. Every table is checked against std
//! over all scalars before it is written.

use crate::Table;

/// `(start: u24, len: u8, delta: i32, stride: u8)`.
const RUN_SIZE: usize = 9;
/// `(cp: u24, len: u8, units: [u16; 3])`, zero-padded so the table is binary
/// searchable.
const SPECIAL_SIZE: usize = 10;
/// The most UTF-16 units a special mapping may have.
const SPECIAL_UNITS: usize = 3;

/// A direction of the full case mapping.
type Mapping = fn(char) -> Vec<char>;

fn upper(c: char) -> Vec<char> {
    c.to_uppercase().collect()
}

fn lower(c: char) -> Vec<char> {
    c.to_lowercase().collect()
}

/// Every Unicode scalar value.
pub(crate) fn scalars() -> impl Iterator<Item = char> {
    (0..=u32::from(char::MAX)).filter_map(char::from_u32)
}

/// A maximal run of `len` mapped code points `stride` apart sharing `delta`.
/// Built in code point order over the mapped scalars only, so two runs never
/// interleave.
struct Run {
    start: u32,
    len: u32,
    delta: i64,
    stride: u32,
}

impl Run {
    /// Absorb the mapped code point `cp`, if it continues this run.
    fn extend(&mut self, cp: u32, delta: i64) -> bool {
        let gap = cp - (self.start + (self.len - 1) * self.stride);
        let fits = delta == self.delta
            && self.len < u32::from(u8::MAX)
            && (gap == self.stride || (self.len == 1 && gap <= 2));
        if fits {
            self.stride = gap;
            self.len += 1;
        }
        fits
    }

    /// The image of `cp` when this run maps it.
    fn map(&self, cp: u32) -> Option<u32> {
        let offset = cp.checked_sub(self.start)?;
        let mapped = offset <= (self.len - 1) * self.stride && offset % self.stride == 0;
        mapped.then(|| u32::try_from(i64::from(cp) + self.delta).unwrap_or(0))
    }
}

/// The single-scalar mappings of `map`, as runs.
fn runs(map: Mapping) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for c in scalars() {
        let image = map(c);
        let &[target] = image.as_slice() else {
            continue;
        };
        if target == c {
            continue;
        }
        let (cp, delta) = (
            u32::from(c),
            i64::from(u32::from(target)) - i64::from(u32::from(c)),
        );
        if !runs.last_mut().is_some_and(|run| run.extend(cp, delta)) {
            runs.push(Run {
                start: cp,
                len: 1,
                delta,
                stride: 1,
            });
        }
    }
    runs
}

/// The little-endian `u24` of `value`.
pub(crate) fn u24(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

fn encode_runs(runs: &[Run]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(runs.len() * RUN_SIZE);
    for run in runs {
        let delta = i32::try_from(run.delta).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&u24(run.start));
        bytes.push(u8::try_from(run.len).map_err(|e| e.to_string())?);
        bytes.extend_from_slice(&delta.to_le_bytes());
        bytes.push(u8::try_from(run.stride).map_err(|e| e.to_string())?);
    }
    Ok(bytes)
}

/// The multi-scalar mappings of `map`, as padded records.
fn special(map: Mapping) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for c in scalars() {
        let image = map(c);
        if image.len() < 2 {
            continue;
        }
        let text: String = image.iter().collect();
        let units: Vec<u16> = text.encode_utf16().collect();
        if units.len() > SPECIAL_UNITS {
            return Err(format!("{c:?} maps to {} units", units.len()));
        }
        bytes.extend_from_slice(&u24(u32::from(c)));
        bytes.push(u8::try_from(units.len()).map_err(|e| e.to_string())?);
        for slot in 0..SPECIAL_UNITS {
            let unit = units.get(slot).copied().unwrap_or(0);
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    Ok(bytes)
}

/// `map` reconstructed from its encoded tables must equal std on every scalar.
fn verify(map: Mapping, runs: &[Run], special: &[u8]) -> Result<(), String> {
    for c in scalars() {
        let cp = u32::from(c);
        let at = runs.partition_point(|run| run.start <= cp);
        let single = at
            .checked_sub(1)
            .and_then(|last| runs[last].map(cp))
            .unwrap_or(cp);
        let record = special
            .as_chunks::<SPECIAL_SIZE>()
            .0
            .iter()
            .find(|record| record[..3] == u24(cp));
        let units: Vec<u16> = match record {
            Some(record) => (0..usize::from(record[3]))
                .map(|i| u16::from_le_bytes([record[4 + 2 * i], record[5 + 2 * i]]))
                .collect(),
            None => char::from_u32(single)
                .ok_or_else(|| format!("{c:?}: bad image {single:#x}"))?
                .encode_utf16(&mut [0; 2])
                .to_vec(),
        };
        let expected: Vec<u16> = map(c).iter().collect::<String>().encode_utf16().collect();
        if units != expected {
            return Err(format!("{c:?}: table {units:?}, std {expected:?}"));
        }
    }
    Ok(())
}

/// The sorted inclusive ranges of the scalars satisfying `set`, as
/// `(start: u24, end: u24)`.
fn ranges(set: impl Fn(char) -> bool) -> Vec<u8> {
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for c in scalars().filter(|&c| set(c)) {
        let cp = u32::from(c);
        match spans.last_mut() {
            Some((_, end)) if *end + 1 == cp => *end = cp,
            _ => spans.push((cp, cp)),
        }
    }
    spans
        .iter()
        .flat_map(|&(start, end)| [u24(start), u24(end)])
        .flatten()
        .collect()
}

/// Whether `text`, which ends in `Σ`, lowercases to a final `ς`.
fn ends_final(text: &str) -> bool {
    text.to_lowercase().ends_with('ς')
}

/// Whether the `Σ` after the leading `A` of `text` lowercases to final `ς`.
fn second_is_final(text: &str) -> bool {
    text.to_lowercase().chars().nth(1) == Some('ς')
}

/// `(Cased, Case_Ignorable)` of `c`, as observed through `Final_Sigma`.
///
/// std scans past `Case_Ignorable` scalars and tests `Cased` on the first
/// other one, so `Cased` of a `Case_Ignorable` scalar has no effect on any
/// outcome and cannot be probed: such a scalar reports `(false, true)`.
fn probe(c: char) -> (bool, bool) {
    let cased = ends_final(&format!("{c}Σ"));
    let either = ends_final(&format!("A{c}Σ"));
    (cased, either && !cased)
}

/// The probed sets must also explain the context after `Σ`, the other half of
/// `Final_Sigma`.
fn verify_probe(c: char) -> Result<(), String> {
    let (cased, ignorable) = probe(c);
    let alone = second_is_final(&format!("AΣ{c}"));
    let then_cased = second_is_final(&format!("AΣ{c}A"));
    if alone != (ignorable || !cased) || then_cased != (!ignorable && !cased) {
        return Err(format!("{c:?}: probe does not explain Final_Sigma"));
    }
    Ok(())
}

/// The case mapping tables.
pub(crate) fn tables() -> Result<Vec<Table>, String> {
    scalars().try_for_each(verify_probe)?;
    let upper_runs = runs(upper);
    let lower_runs = runs(lower);
    let special_upper = special(upper)?;
    let special_lower = special(lower)?;
    verify(upper, &upper_runs, &special_upper)?;
    verify(lower, &lower_runs, &special_lower)?;
    Ok(vec![
        Table {
            name: "CASE_UPPER",
            doc: "Single-scalar uppercase runs `(start: u24, len: u8, delta: i32, stride: u8)`.",
            bytes: encode_runs(&upper_runs)?,
        },
        Table {
            name: "CASE_LOWER",
            doc: "Single-scalar lowercase runs `(start: u24, len: u8, delta: i32, stride: u8)`.",
            bytes: encode_runs(&lower_runs)?,
        },
        Table {
            name: "SPECIAL_UPPER",
            doc: "Multi-unit uppercase mappings `(cp: u24, len: u8, units: [u16; 3])`.",
            bytes: special_upper,
        },
        Table {
            name: "SPECIAL_LOWER",
            doc: "Multi-unit lowercase mappings `(cp: u24, len: u8, units: [u16; 3])`.",
            bytes: special_lower,
        },
        Table {
            name: "CASED",
            doc: "`Cased` scalars that are not `Case_Ignorable`, as `(start: u24, end: u24)`.",
            bytes: ranges(|c| probe(c).0),
        },
        Table {
            name: "CASE_IGNORABLE",
            doc: "`Case_Ignorable` scalars, as `(start: u24, end: u24)`.",
            bytes: ranges(|c| probe(c).1),
        },
        Table {
            name: "ALPHANUMERIC",
            doc: "`char::is_alphanumeric` scalars, as `(start: u24, end: u24)`.",
            bytes: ranges(char::is_alphanumeric),
        },
    ])
}
