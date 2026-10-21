//! `jsc-test262` command-line entry point. Run from the repository root.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jsc_test262::filter::{self, Denylist, Mode, SkipReason};
use jsc_test262::outcome::{self, Outcome};
use jsc_test262::report::{self, Report};
use jsc_test262::{assemble, baseline, exec, frontmatter, walk};

const USAGE: &str = "usage: jsc-test262 [--test262 DIR] [--jsc PATH] [--jobs N] \
[--timeout SECS] [--check] [--update-baseline] [--history] [PATH_PREFIX...]";
/// Harness includes kept out of M1 (`design/object-model.md` section 9):
/// `doneprintHandle.js` needs a host `print` and an async runner,
/// `propertyHelper.js` needs phase-3 reflection.
const EXCLUDED_INCLUDES: [&str; 2] = ["doneprintHandle.js", "propertyHelper.js"];
const DENYLIST: &str = "tests/test262/unsupported-features.txt";
const BASELINE: &str = "tests/test262/baseline.txt";
const HISTORY: &str = "tests/test262/history.tsv";
const HISTORY_HEADER: &str = "commit\tpass\teligible\ttotal\n";
const RESULTS: &str = ".tmp/test262-results.tsv";
const SCRATCH: &str = ".tmp/test262-run";
const TOP_SKIPS: usize = 10;
const MAX_LISTED_NEW: usize = 20;
const MAX_DETAIL: usize = 300;

#[derive(Debug, PartialEq, Eq)]
struct Args {
    test262: PathBuf,
    jsc: PathBuf,
    jobs: usize,
    timeout: Duration,
    check: bool,
    update_baseline: bool,
    history: bool,
    filters: Vec<String>,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        test262: PathBuf::from(".tmp/test262"),
        jsc: PathBuf::from("target/release/jsc"),
        jobs: std::thread::available_parallelism().map_or(1, NonZero::get),
        timeout: Duration::from_secs(10),
        check: false,
        update_baseline: false,
        history: false,
        filters: Vec::new(),
    };
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or(format!("{flag} needs a value"));
        let positive = |flag: &str, v: String| match v.parse::<u64>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("{flag} expects a positive integer, got `{v}`")),
        };
        match arg.as_str() {
            "--test262" => args.test262 = PathBuf::from(value(&arg)?),
            "--jsc" => args.jsc = PathBuf::from(value(&arg)?),
            "--jobs" => {
                args.jobs =
                    usize::try_from(positive(&arg, value(&arg)?)?).map_err(|e| e.to_string())?;
            }
            "--timeout" => args.timeout = Duration::from_secs(positive(&arg, value(&arg)?)?),
            "--check" => args.check = true,
            "--update-baseline" => args.update_baseline = true,
            "--history" => args.history = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option `{flag}`")),
            _ => args.filters.push(arg),
        }
    }
    if args.history && !args.filters.is_empty() {
        return Err("--history records full runs only; drop the PATH_PREFIX filters".to_owned());
    }
    Ok(args)
}

/// One test's result plus the first stderr line (for failure clustering).
struct Record {
    result: Result<Outcome, SkipReason>,
    detail: String,
}

/// Inputs shared by every test run.
struct Env {
    args: Args,
    test_root: PathBuf,
    scratch: PathBuf,
    sta: String,
    assert: String,
    includes: BTreeMap<String, String>,
    compiling: BTreeSet<String>,
    deny: Denylist,
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn load(args: Args) -> Result<Env, String> {
    if !args.jsc.is_file() {
        return Err(format!(
            "jsc binary not found at {}; run `cargo build --release -p jsc` or pass --jsc",
            args.jsc.display()
        ));
    }
    let test_root = args.test262.join("test");
    if !test_root.is_dir() {
        return Err(format!(
            "test262 checkout not found at {}; run `bash scripts/test262-fetch.sh` or pass --test262",
            test_root.display()
        ));
    }
    let harness = args.test262.join("harness");
    if !harness.join("assert.js").is_file() {
        return Err(format!(
            "official test262 harness not found at {}; run `bash scripts/test262-fetch.sh` or pass --test262",
            harness.display()
        ));
    }
    let mut includes = BTreeMap::new();
    let entries =
        fs::read_dir(&harness).map_err(|e| format!("cannot read {}: {e}", harness.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|e| format!("cannot read {}: {e}", harness.display()))?
            .path();
        let is_js = path.extension().is_some_and(|e| e == "js") && path.is_file();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()).filter(|_| is_js) {
            includes.insert(name.to_owned(), read(&path)?);
        }
    }
    let mut take = |name: &str| {
        includes
            .remove(name)
            .ok_or(format!("{}/{name} is missing", harness.display()))
    };
    let (sta, assert) = (take("sta.js")?, take("assert.js")?);
    let scratch = PathBuf::from(SCRATCH);
    fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {SCRATCH}: {e}"))?;
    let mut env = Env {
        test_root,
        scratch,
        sta,
        assert,
        includes,
        compiling: BTreeSet::new(),
        deny: Denylist::parse(&read(Path::new(DENYLIST))?),
        args,
    };
    if let Err(detail) = probe(&env, 0, &[])? {
        return Err(format!(
            "official sta.js + assert.js do not compile with {}: {detail}",
            env.args.jsc.display()
        ));
    }
    env.includes
        .retain(|name, _| !EXCLUDED_INCLUDES.contains(&name.as_str()));
    let names: Vec<&String> = env.includes.keys().collect();
    let probed = exec::pool(&names, env.args.jobs, |i, name| {
        probe(&env, i + 1, &[env.includes[*name].as_str()])
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    env.compiling = names
        .into_iter()
        .zip(probed)
        .filter(|(_, r)| r.is_ok())
        .map(|(n, _)| n.clone())
        .collect();
    env.includes.retain(|name, _| env.compiling.contains(name));
    Ok(env)
}

/// Compiles and runs `sta.js` + `assert.js` + `includes` once. The inner
/// result is `Err(first stderr line)` when jsc rejects the program at compile
/// time or it times out; a runtime throw still counts as compiling.
fn probe(env: &Env, index: usize, includes: &[&str]) -> Result<Result<(), String>, String> {
    let program = assemble::program(Mode::Strict, "", &env.sta, &env.assert, includes);
    let name = format!("{}-probe-{index}", std::process::id());
    let (status, stderr) = exec::run(
        &env.args.jsc,
        &env.scratch,
        &name,
        &program,
        env.args.timeout,
    )
    .map_err(|e| format!("cannot run {}: {e}", env.args.jsc.display()))?;
    Ok(match outcome::classify(status, &stderr, None) {
        Outcome::Pass | Outcome::Fail => Ok(()),
        Outcome::CompileError | Outcome::Timeout => Err(first_line(&stderr)),
    })
}

/// First non-empty stderr line, tab-free and bounded.
fn first_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .replace('\t', " ")
        .chars()
        .take(MAX_DETAIL)
        .collect()
}

fn run_test(env: &Env, index: usize, path: &str) -> Result<Record, String> {
    let bytes =
        fs::read(env.test_root.join(path)).map_err(|e| format!("cannot read {path}: {e}"))?;
    let source = String::from_utf8_lossy(&bytes);
    let meta = frontmatter::parse(&source);
    let mode = match filter::classify(&meta, &env.deny, &env.compiling) {
        Ok(mode) => mode,
        Err(skip) => {
            return Ok(Record {
                result: Err(skip),
                detail: String::new(),
            });
        }
    };
    let includes: Vec<&str> = match mode {
        Mode::Raw => Vec::new(),
        Mode::Strict => meta
            .includes
            .iter()
            .filter_map(|i| env.includes.get(i).map(String::as_str))
            .collect(),
    };
    let program = assemble::program(mode, &source, &env.sta, &env.assert, &includes);
    let name = format!("{}-{index}", std::process::id());
    let (status, stderr) = exec::run(
        &env.args.jsc,
        &env.scratch,
        &name,
        &program,
        env.args.timeout,
    )
    .map_err(|e| format!("cannot run {} on {path}: {e}", env.args.jsc.display()))?;
    Ok(Record {
        result: Ok(outcome::classify(status, &stderr, meta.negative.as_ref())),
        detail: first_line(&stderr),
    })
}

fn summary(report: &Report) -> String {
    let mut out = report::render_table(report);
    let outcomes: Vec<String> = report
        .outcomes
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let _ = writeln!(out, "outcomes: {}", outcomes.join(" "));
    let mut skips: Vec<(&String, &usize)> = report.skips.iter().collect();
    skips.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let _ = writeln!(
        out,
        "skipped: {} (top reasons below)",
        report.overall.total - report.overall.eligible
    );
    for (label, n) in skips.into_iter().take(TOP_SKIPS) {
        let _ = writeln!(out, "  {n:>7} {label}");
    }
    out
}

fn results_tsv(paths: &[String], records: &[Record]) -> String {
    let mut out = String::new();
    for (path, r) in paths.iter().zip(records) {
        let label = match &r.result {
            Ok(o) => o.label().to_owned(),
            Err(s) => s.label(),
        };
        let _ = writeln!(out, "{path}\t{label}\t{}", r.detail);
    }
    out
}

fn check(old: &BTreeSet<String>, passing: &BTreeSet<String>, filters: &[String]) -> bool {
    let diff = baseline::diff(old, passing, filters);
    println!("regressed: {}", diff.regressed.len());
    for p in &diff.regressed {
        println!("  {p}");
    }
    if !diff.newly_passing.is_empty() {
        println!(
            "newly passing (not in baseline; run --update-baseline): {}",
            diff.newly_passing.len()
        );
        for p in diff.newly_passing.iter().take(MAX_LISTED_NEW) {
            println!("  {p}");
        }
        if diff.newly_passing.len() > MAX_LISTED_NEW {
            println!("  ... {} more", diff.newly_passing.len() - MAX_LISTED_NEW);
        }
    }
    diff.regressed.is_empty()
}

fn append_history(report: &Report) -> Result<(), String> {
    let git = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !git.status.success() {
        return Err("`git rev-parse --short=7 HEAD` failed".to_owned());
    }
    let commit = String::from_utf8_lossy(&git.stdout).trim().to_owned();
    let path = Path::new(HISTORY);
    let mut text = if path.exists() {
        read(path)?
    } else {
        HISTORY_HEADER.to_owned()
    };
    let o = &report.overall;
    let _ = writeln!(text, "{commit}\t{}\t{}\t{}", o.pass, o.eligible, o.total);
    write(path, &text)
}

fn real_main() -> Result<ExitCode, String> {
    let args = parse_args(std::env::args().skip(1)).map_err(|e| format!("{e}\n{USAGE}"))?;
    let env = load(args)?;
    let compiling: Vec<&str> = env.compiling.iter().map(String::as_str).collect();
    println!("harness includes that compile: {}", compiling.join(", "));
    let started = Instant::now();
    let paths = walk::collect(&env.test_root, &env.args.filters)
        .map_err(|e| format!("cannot walk {}: {e}", env.test_root.display()))?;
    let done = AtomicUsize::new(0);
    let records = exec::pool(&paths, env.args.jobs, |i, path| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(5000) {
            eprintln!("  {n}/{} tests", paths.len());
        }
        run_test(&env, i, path)
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let elapsed = started.elapsed();

    let pairs: Vec<(String, Result<Outcome, SkipReason>)> = paths
        .iter()
        .zip(&records)
        .map(|(p, r)| (p.clone(), r.result.clone()))
        .collect();
    let report = Report::new(&pairs);
    print!("{}", summary(&report));
    println!(
        "wall-clock: {:.1}s ({} jobs)",
        elapsed.as_secs_f64(),
        env.args.jobs
    );
    write(Path::new(RESULTS), &results_tsv(&paths, &records))?;
    println!("per-test results: {RESULTS}");

    let passing: BTreeSet<String> = pairs
        .into_iter()
        .filter(|(_, r)| *r == Ok(Outcome::Pass))
        .map(|(p, _)| p)
        .collect();
    let base_path = Path::new(BASELINE);
    let mut ok = true;
    if env.args.check {
        let text = read(base_path).map_err(|e| format!("{e}; create it with --update-baseline"))?;
        ok = check(&baseline::parse(&text), &passing, &env.args.filters);
    }
    if env.args.update_baseline {
        let old = if base_path.exists() {
            baseline::parse(&read(base_path)?)
        } else {
            BTreeSet::new()
        };
        let new = baseline::merge(&old, &passing, &env.args.filters);
        write(base_path, &baseline::render(&new))?;
        println!(
            "baseline: {} passing tests written to {BASELINE}",
            new.len()
        );
    }
    if env.args.history {
        append_history(&report)?;
        println!("history: appended to {HISTORY}");
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    real_main().unwrap_or_else(|e| {
        eprintln!("jsc-test262: {e}");
        ExitCode::from(2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults() -> Result<(), String> {
        let a = parse(&[])?;
        assert_eq!(a.test262, PathBuf::from(".tmp/test262"));
        assert_eq!(a.jsc, PathBuf::from("target/release/jsc"));
        assert!(a.jobs >= 1);
        assert_eq!(a.timeout, Duration::from_secs(10));
        assert!(!a.check && !a.update_baseline && !a.history && a.filters.is_empty());
        Ok(())
    }

    #[test]
    fn all_options() -> Result<(), String> {
        let a = parse(&[
            "--test262",
            "t",
            "--jsc",
            "j",
            "--jobs",
            "3",
            "--timeout",
            "2",
            "--check",
            "--update-baseline",
            "built-ins/Array/",
            "language/",
        ])?;
        assert_eq!(
            a,
            Args {
                test262: PathBuf::from("t"),
                jsc: PathBuf::from("j"),
                jobs: 3,
                timeout: Duration::from_secs(2),
                check: true,
                update_baseline: true,
                history: false,
                filters: vec!["built-ins/Array/".to_owned(), "language/".to_owned()],
            }
        );
        assert!(parse(&["--history"])?.history);
        Ok(())
    }

    #[test]
    fn rejects() {
        for bad in [
            &["--jobs"][..],
            &["--jobs", "0"],
            &["--timeout", "x"],
            &["--bogus"],
            &["--history", "language/"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn first_line_cases() {
        assert_eq!(first_line(""), "");
        assert_eq!(
            first_line("\n  Error: Uncaught\tx  \nmore\n"),
            "Error: Uncaught x"
        );
        assert_eq!(first_line(&"a".repeat(1000)).len(), MAX_DETAIL);
    }
}
