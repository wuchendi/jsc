//! Hand-written parser for the subset of YAML used by test262 frontmatter
//! (`/*--- ... ---*/`). Only `features`, `flags`, `includes` and `negative`
//! are extracted; every other key (including `|` / `>` multi-line scalars) is
//! skipped.

/// Expected failure declared by a test's `negative:` key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Negative {
    /// `parse`, `early`, `resolution` or `runtime`.
    pub phase: String,
    /// Expected error constructor name (YAML key `type`).
    pub kind: String,
}

/// Metadata relevant to running a test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// `features:` entries.
    pub features: Vec<String>,
    /// `flags:` entries.
    pub flags: Vec<String>,
    /// `includes:` harness file names.
    pub includes: Vec<String>,
    /// `negative:` expectation, if any.
    pub negative: Option<Negative>,
}

/// Parses the frontmatter of a test source; returns `Meta::default()` when
/// there is none.
pub fn parse(source: &str) -> Meta {
    let mut meta = Meta::default();
    let Some(start) = source.find("/*---") else {
        return meta;
    };
    let rest = &source[start + 5..];
    let Some(end) = rest.find("---*/") else {
        return meta;
    };
    let mut key = String::new();
    for line in rest[..end].lines() {
        let line = line.trim_end_matches('\r');
        let text = strip_comment(line.trim());
        if text.is_empty() {
            continue;
        }
        let top_level = !line.starts_with([' ', '\t']);
        if top_level && !text.starts_with('-') {
            let (k, value) = text.split_once(':').unwrap_or((text, ""));
            k.trim().clone_into(&mut key);
            let value = value.trim();
            if key == "negative" {
                meta.negative = Some(Negative::default());
            } else if let (Some(list), Some(inner)) = (
                list_mut(&mut meta, &key),
                value.strip_prefix('[').and_then(|v| v.strip_suffix(']')),
            ) {
                list.extend(
                    inner
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                );
            }
        } else if let Some(item) = text.strip_prefix('-') {
            if let Some(list) = list_mut(&mut meta, &key) {
                list.push(item.trim().to_owned());
            }
        } else if let (Some(neg), Some((k, v))) = (meta.negative.as_mut(), text.split_once(':'))
            && key == "negative"
        {
            match k.trim() {
                "phase" => v.trim().clone_into(&mut neg.phase),
                "type" => v.trim().clone_into(&mut neg.kind),
                _ => {}
            }
        }
    }
    meta
}

fn list_mut<'a>(meta: &'a mut Meta, key: &str) -> Option<&'a mut Vec<String>> {
    match key {
        "features" => Some(&mut meta.features),
        "flags" => Some(&mut meta.flags),
        "includes" => Some(&mut meta.includes),
        _ => None,
    }
}

/// Drops a `#` comment that starts the text or follows whitespace.
fn strip_comment(text: &str) -> &str {
    let mut prev_space = true;
    for (i, c) in text.char_indices() {
        if c == '#' && prev_space {
            return text[..i].trim_end();
        }
        prev_space = c.is_whitespace();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_lists() {
        let m = parse("/*---\nfeatures: [Symbol, Proxy]  # note\nflags: [onlyStrict]\n---*/\n");
        assert_eq!(m.features, ["Symbol", "Proxy"]);
        assert_eq!(m.flags, ["onlyStrict"]);
    }

    #[test]
    fn block_list_and_crlf() {
        let m = parse(
            "/*---\r\nincludes:\r\n  - compareArray.js\r\n  - propertyHelper.js # c\r\nflags: [raw]\r\n---*/",
        );
        assert_eq!(m.includes, ["compareArray.js", "propertyHelper.js"]);
        assert_eq!(m.flags, ["raw"]);
    }

    #[test]
    fn negative_map() {
        let m = parse("/*---\nnegative:\n  phase: parse\n  type: SyntaxError\nflags: [x]\n---*/");
        assert_eq!(
            m.negative,
            Some(Negative {
                phase: "parse".into(),
                kind: "SyntaxError".into()
            })
        );
        assert_eq!(m.flags, ["x"]);
    }

    #[test]
    fn multi_line_scalars_skipped() {
        let src = "/*---\ndescription: |\n  - not an item\n  phase: no\ninfo: >\n    features: [Nope]\nfeatures: [Yes]\n---*/";
        let m = parse(src);
        assert_eq!(m.features, ["Yes"]);
        assert!(m.includes.is_empty() && m.negative.is_none());
    }

    #[test]
    fn missing_frontmatter() {
        assert_eq!(parse("assert(true);"), Meta::default());
        assert_eq!(parse("/*--- features: [a]"), Meta::default());
    }
}
