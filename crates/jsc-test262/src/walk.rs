//! Discovery of test files.

use std::fs;
use std::io;
use std::path::Path;

const EXCLUDED_TOP: [&str; 3] = ["intl402", "annexB", "staging"];

/// Collects test files under `test_root` (the test262 `test/` directory) as
/// sorted `/`-separated paths relative to it. Skips `intl402/`, `staging/`,
/// any `annexB` segment, `_FIXTURE` files and non-`.js` files; a non-empty
/// `filter` keeps only paths starting with one of its prefixes.
pub fn collect(test_root: &Path, filter: &[String]) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    visit(test_root, "", &mut out)?;
    out.retain(|p| filter.is_empty() || filter.iter().any(|f| p.starts_with(f.as_str())));
    out.sort();
    Ok(out)
}

fn visit(dir: &Path, rel: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if entry.file_type()?.is_dir() {
            if name == "annexB" || (rel.is_empty() && EXCLUDED_TOP.contains(&name.as_str())) {
                continue;
            }
            visit(&entry.path(), &path, out)?;
        } else if Path::new(&name).extension().is_some_and(|e| e == "js")
            && !name.contains("_FIXTURE")
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_and_filters() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("jsc-test262-walk-{}", std::process::id()));
        let files = [
            "built-ins/Array/a.js",
            "built-ins/Array/b.txt",
            "built-ins/Array/c_FIXTURE.js",
            "built-ins/RegExp/annexB/d.js",
            "language/expressions/e.js",
            "intl402/f.js",
            "annexB/g.js",
            "staging/h.js",
            "language/staging/i.js",
        ];
        for f in files {
            let p = root.join(f);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(p, "")?;
        }
        let all = collect(&root, &[]);
        let filtered = collect(&root, &["language/".to_owned()]);
        fs::remove_dir_all(&root)?;
        assert_eq!(
            all?,
            [
                "built-ins/Array/a.js",
                "language/expressions/e.js",
                "language/staging/i.js"
            ]
        );
        assert_eq!(
            filtered?,
            ["language/expressions/e.js", "language/staging/i.js"]
        );
        Ok(())
    }
}
