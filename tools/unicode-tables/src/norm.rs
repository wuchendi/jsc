//! Normalization tables, from `icu_normalizer`. The combining classes and
//! decompositions are checked against it over all scalars before they are
//! written; the compositions are its own answers.

use std::collections::HashMap;

use icu_normalizer::DecomposingNormalizerBorrowed;
use icu_normalizer::properties::{
    CanonicalCombiningClassMapBorrowed, CanonicalCompositionBorrowed,
    CanonicalDecompositionBorrowed, Decomposed,
};

use crate::Table;
use crate::case::{scalars, u24};

/// The `icu_normalizer` release the tables come from, pinned in `Cargo.toml`.
pub(crate) const ICU_VERSION: &str = "2.4.0";
/// The Unicode version of that release's data.
pub(crate) const ICU_UNICODE_VERSION: &str = "18.0.0";

/// `(cp: u24, flags: u8, off: u16, len: u8)`.
const DECOMPOSITION_SIZE: usize = 7;
/// `flags` of a record that applies to the compatibility forms only.
const COMPAT: u8 = 1;
/// `(start: u24, len: u8, ccc: u8)`.
const CCC_SIZE: usize = 5;

/// Whether `c` is a precomposed Hangul syllable, decomposed algorithmically.
fn is_hangul(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// The non-zero canonical combining classes as runs.
fn ccc() -> Result<Vec<u8>, String> {
    let map = CanonicalCombiningClassMapBorrowed::new();
    let mut runs: Vec<(u32, u32, u8)> = Vec::new();
    for c in scalars() {
        let class = map.get_u8(c);
        if class == 0 {
            continue;
        }
        let cp = u32::from(c);
        match runs.last_mut() {
            Some((start, len, last))
                if *last == class && *start + *len == cp && *len < u32::from(u8::MAX) =>
            {
                *len += 1;
            }
            _ => runs.push((cp, 1, class)),
        }
    }
    let mut bytes = Vec::with_capacity(runs.len() * CCC_SIZE);
    for (start, len, class) in runs {
        bytes.extend_from_slice(&u24(start));
        bytes.push(u8::try_from(len).map_err(|e| e.to_string())?);
        bytes.push(class);
    }
    let runs = bytes.as_chunks::<CCC_SIZE>().0;
    let start_of = |run: &[u8; CCC_SIZE]| u32::from_le_bytes([run[0], run[1], run[2], 0]);
    for c in scalars() {
        let cp = u32::from(c);
        let found = runs
            .partition_point(|run| start_of(run) <= cp)
            .checked_sub(1)
            .map(|last| &runs[last])
            .filter(|run| cp - start_of(run) < u32::from(run[3]))
            .map_or(0, |run| run[4]);
        if found != map.get_u8(c) {
            return Err(format!("{c:?}: ccc table {found}"));
        }
    }
    Ok(bytes)
}

/// One decomposition record before encoding.
struct Decomposition {
    cp: u32,
    flags: u8,
    expansion: Vec<u32>,
}

/// The full decomposition of `c` under `normalizer`, when it has one.
fn expansion(normalizer: &DecomposingNormalizerBorrowed<'_>, c: char) -> Option<Vec<u32>> {
    let text = c.to_string();
    let image: Vec<u32> = normalizer.normalize(&text).chars().map(u32::from).collect();
    (image != [u32::from(c)]).then_some(image)
}

/// Every decomposition in `(cp, flags)` order: a canonical record, then a
/// compatibility record when that form expands differently.
fn decompositions() -> Vec<Decomposition> {
    let canon_form = DecomposingNormalizerBorrowed::new_nfd();
    let compat_form = DecomposingNormalizerBorrowed::new_nfkd();
    let mut records = Vec::new();
    for c in scalars().filter(|&c| !is_hangul(c)) {
        let cp = u32::from(c);
        let canonical = expansion(&canon_form, c);
        let compat = expansion(&compat_form, c);
        if compat.is_some() && compat != canonical {
            if let Some(expansion) = canonical {
                records.push(Decomposition {
                    cp,
                    flags: 0,
                    expansion,
                });
            }
            if let Some(expansion) = compat {
                records.push(Decomposition {
                    cp,
                    flags: COMPAT,
                    expansion,
                });
            }
        } else if let Some(expansion) = canonical {
            records.push(Decomposition {
                cp,
                flags: 0,
                expansion,
            });
        }
    }
    records
}

/// A pool holding every expansion, each stored once: longest first, so a
/// shorter expansion usually lands inside an earlier one. Returns the pool
/// and the unit offset of each expansion.
fn build_pool(records: &[Decomposition]) -> (Vec<u32>, HashMap<Vec<u32>, usize>) {
    let mut expansions: Vec<&Vec<u32>> = records.iter().map(|r| &r.expansion).collect();
    expansions.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    expansions.dedup();
    let mut pool: Vec<u32> = Vec::new();
    let mut offsets: HashMap<Vec<u32>, usize> = HashMap::new();
    for expansion in expansions {
        if offsets.contains_key(expansion) {
            continue;
        }
        let at = pool.len();
        pool.extend_from_slice(expansion);
        for start in 0..expansion.len() {
            for end in start + 1..=expansion.len() {
                offsets
                    .entry(expansion[start..end].to_vec())
                    .or_insert(at + start);
            }
        }
    }
    (pool, offsets)
}

fn encode_decompositions(records: &[Decomposition]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (pool, offsets) = build_pool(records);
    let mut index = Vec::with_capacity(records.len() * DECOMPOSITION_SIZE);
    for record in records {
        let offset = offsets
            .get(&record.expansion)
            .ok_or_else(|| format!("U+{:04X}: expansion not pooled", record.cp))?;
        index.extend_from_slice(&u24(record.cp));
        index.push(record.flags);
        index.extend_from_slice(
            &u16::try_from(*offset)
                .map_err(|e| e.to_string())?
                .to_le_bytes(),
        );
        index.push(u8::try_from(record.expansion.len()).map_err(|e| e.to_string())?);
    }
    let bytes = pool.iter().flat_map(|&cp| u24(cp)).collect();
    Ok((index, bytes))
}

/// Decode the expansion of `cp` the way the guest does: the last record of
/// `cp` applies, unless it is compatibility-only and the form is canonical.
fn lookup(index: &[u8], pool: &[u8], cp: u32, compat: bool) -> Option<Vec<u32>> {
    let records = index.as_chunks::<DECOMPOSITION_SIZE>().0;
    let cp_of = |r: &[u8; DECOMPOSITION_SIZE]| u32::from_le_bytes([r[0], r[1], r[2], 0]);
    let at = records.partition_point(|r| cp_of(r) <= cp).checked_sub(1)?;
    let mut record = &records[at];
    if cp_of(record) != cp {
        return None;
    }
    if !compat && record[3] & COMPAT != 0 {
        record = records.get(at.checked_sub(1)?)?;
        if cp_of(record) != cp {
            return None;
        }
    }
    let offset = usize::from(u16::from_le_bytes([record[4], record[5]]));
    Some(
        pool[offset * 3..(offset + usize::from(record[6])) * 3]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], 0]))
            .collect(),
    )
}

fn verify_decompositions(index: &[u8], pool: &[u8]) -> Result<(), String> {
    let canon_form = DecomposingNormalizerBorrowed::new_nfd();
    let compat_form = DecomposingNormalizerBorrowed::new_nfkd();
    for c in scalars().filter(|&c| !is_hangul(c)) {
        for (normalizer, compat) in [(&canon_form, false), (&compat_form, true)] {
            let table = lookup(index, pool, u32::from(c), compat);
            let expected = expansion(normalizer, c);
            if table != expected {
                return Err(format!("{c:?}: table {table:?}, icu {expected:?}"));
            }
        }
    }
    Ok(())
}

/// The primary composites, sorted by `(first, second)`: the canonical pairs
/// that compose back to their scalar, so the composition exclusions drop out.
fn compositions() -> Vec<u8> {
    let decomposition = CanonicalDecompositionBorrowed::new();
    let composition = CanonicalCompositionBorrowed::new();
    let mut triples: Vec<(u32, u32, u32)> = Vec::new();
    for c in scalars().filter(|&c| !is_hangul(c)) {
        if let Decomposed::Expansion(first, second) = decomposition.decompose(c)
            && composition.compose(first, second) == Some(c)
        {
            triples.push((u32::from(first), u32::from(second), u32::from(c)));
        }
    }
    triples.sort_unstable();
    triples
        .iter()
        .flat_map(|&(first, second, composite)| [u24(first), u24(second), u24(composite)])
        .flatten()
        .collect()
}

/// The normalization tables.
pub(crate) fn tables() -> Result<Vec<Table>, String> {
    let (index, pool) = encode_decompositions(&decompositions())?;
    verify_decompositions(&index, &pool)?;
    Ok(vec![
        Table {
            name: "CCC",
            doc: "Non-zero canonical combining classes `(start: u24, len: u8, ccc: u8)`.",
            bytes: ccc()?,
        },
        Table {
            name: "DECOMPOSITIONS",
            doc: "Full decompositions `(cp: u24, flags: u8, off: u16, len: u8)`, sorted by \
                  `(cp, flags)`; flag 1 marks a compatibility-only record, `off` indexes \
                  `DECOMPOSITION_POOL`.",
            bytes: index,
        },
        Table {
            name: "DECOMPOSITION_POOL",
            doc: "The expansions of `DECOMPOSITIONS`, as `u24` code points.",
            bytes: pool,
        },
        Table {
            name: "COMPOSITIONS",
            doc: "Primary composites `(first: u24, second: u24, composite: u24)`, sorted.",
            bytes: compositions(),
        },
    ])
}
