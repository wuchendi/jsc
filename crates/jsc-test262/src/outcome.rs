//! Maps a finished jsc process to a test outcome.
//!
//! Both exit-code conventions are accepted: compile errors exit 2 (or exit
//! nonzero with stderr starting `error:`), uncaught exceptions exit 1 with
//! `Uncaught` on stderr.
//!
//! # Uncaught error names
//!
//! For `negative: { phase: runtime }` tests the expected constructor name is
//! matched against the token after `Uncaught ` on stderr. jsc prints builtin
//! errors as `Uncaught TypeError: msg`, so builtin names match, and any other
//! uncaught object Node-style with its constructor name. The official harness
//! throws `new Test262Error(message)` from failed assertions and
//! `Test262Error.thrower`, which renders as `Uncaught Test262Error { message:
//! x }` and so reports the name `Test262Error`. `$DONOTEVALUATE()` throws a
//! string (`Uncaught Test262: This statement ...`), which never matches.
//!
//! # Async tests
//!
//! A non-negative `flag:async` test that exits 0 is judged by the `$DONE`
//! report on stdout ([`classify_async`]); every other async result follows
//! the rules above.

use crate::frontmatter::Negative;

/// How the jsc process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Exited with this code.
    Exited(i32),
    /// Killed by a signal.
    Signaled,
    /// Killed by the runner's timeout.
    TimedOut,
}

/// Final classification of a run test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Behaved as expected.
    Pass,
    /// Behaved differently from expected.
    Fail,
    /// jsc rejected a program that should compile.
    CompileError,
    /// Exceeded the time limit.
    Timeout,
}

impl Outcome {
    /// Stable short label for reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::CompileError => "compile-error",
            Self::Timeout => "timeout",
        }
    }
}

enum Verdict {
    Ok,
    CompileError,
    Uncaught,
    Timeout,
    Other,
}

fn verdict(status: Status, stderr: &str) -> Verdict {
    match status {
        Status::TimedOut => Verdict::Timeout,
        Status::Exited(0) => Verdict::Ok,
        Status::Exited(code) if code == 2 || stderr.trim_start().starts_with("error:") => {
            Verdict::CompileError
        }
        Status::Exited(1) if stderr.contains("Uncaught") => Verdict::Uncaught,
        Status::Signaled | Status::Exited(_) => Verdict::Other,
    }
}

/// Name of the uncaught error: the identifier right after the first
/// `Uncaught ` in stderr.
fn uncaught_name(stderr: &str) -> Option<&str> {
    let rest = &stderr[stderr.find("Uncaught ")? + "Uncaught ".len()..];
    let end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Classifies a process result against the test's `negative` expectation.
pub fn classify(status: Status, stderr: &str, negative: Option<&Negative>) -> Outcome {
    let v = verdict(status, stderr);
    if let Verdict::Timeout = v {
        return Outcome::Timeout;
    }
    match negative {
        None => match v {
            Verdict::Ok => Outcome::Pass,
            Verdict::CompileError => Outcome::CompileError,
            _ => Outcome::Fail,
        },
        Some(n) if n.phase == "runtime" => match v {
            Verdict::Uncaught if uncaught_name(stderr) == Some(n.kind.as_str()) => Outcome::Pass,
            Verdict::CompileError => Outcome::CompileError,
            _ => Outcome::Fail,
        },
        Some(_) => match v {
            Verdict::CompileError => Outcome::Pass,
            _ => Outcome::Fail,
        },
    }
}

/// Line `$DONE()` prints on success.
pub const ASYNC_COMPLETE: &str = "Test262:AsyncTestComplete";
/// Prefix of the line `$DONE(error)` prints.
pub const ASYNC_FAILURE: &str = "Test262:AsyncTestFailure";
/// Failure detail when an async test exits 0 without reporting completion.
pub const MISSING_COMPLETE: &str = "missing Test262:AsyncTestComplete";

/// Classifies an async test. A non-negative test that exits 0 passes iff
/// stdout has a line exactly [`ASYNC_COMPLETE`] and none starting with
/// [`ASYNC_FAILURE`]; otherwise it fails with the failure line (or
/// [`MISSING_COMPLETE`]) as the detail. Other cases defer to [`classify`]
/// with no detail.
pub fn classify_async(
    status: Status,
    stdout: &str,
    stderr: &str,
    negative: Option<&Negative>,
) -> (Outcome, Option<String>) {
    if negative.is_some() || status != Status::Exited(0) {
        return (classify(status, stderr, negative), None);
    }
    if let Some(line) = stdout.lines().find(|l| l.starts_with(ASYNC_FAILURE)) {
        return (Outcome::Fail, Some(line.to_owned()));
    }
    if stdout.lines().any(|l| l == ASYNC_COMPLETE) {
        (Outcome::Pass, None)
    } else {
        (Outcome::Fail, Some(MISSING_COMPLETE.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neg(phase: &str, kind: &str) -> Negative {
        Negative {
            phase: phase.into(),
            kind: kind.into(),
        }
    }

    const UNCAUGHT: &str = "Uncaught TypeError: x is not a function\n";

    #[test]
    fn positive() {
        assert_eq!(classify(Status::Exited(0), "", None), Outcome::Pass);
        assert_eq!(
            classify(Status::Exited(1), "error: bad\n", None),
            Outcome::CompileError
        );
        assert_eq!(
            classify(Status::Exited(2), "whatever", None),
            Outcome::CompileError
        );
        assert_eq!(classify(Status::Exited(1), UNCAUGHT, None), Outcome::Fail);
        assert_eq!(classify(Status::Exited(101), "", None), Outcome::Fail);
        assert_eq!(classify(Status::Signaled, "", None), Outcome::Fail);
        assert_eq!(classify(Status::TimedOut, "", None), Outcome::Timeout);
        assert_eq!(Outcome::CompileError.label(), "compile-error");
    }

    #[test]
    fn negative_parse() {
        for phase in ["parse", "early"] {
            let n = neg(phase, "SyntaxError");
            assert_eq!(
                classify(Status::Exited(1), "error: x", Some(&n)),
                Outcome::Pass
            );
            assert_eq!(
                classify(Status::Exited(2), "error: x", Some(&n)),
                Outcome::Pass
            );
            assert_eq!(classify(Status::Exited(0), "", Some(&n)), Outcome::Fail);
            assert_eq!(
                classify(Status::Exited(1), UNCAUGHT, Some(&n)),
                Outcome::Fail
            );
            assert_eq!(classify(Status::TimedOut, "", Some(&n)), Outcome::Timeout);
        }
    }

    #[test]
    fn negative_runtime() {
        let n = neg("runtime", "TypeError");
        assert_eq!(
            classify(Status::Exited(1), UNCAUGHT, Some(&n)),
            Outcome::Pass
        );
        assert_eq!(
            classify(Status::Exited(1), "Uncaught TypeError", Some(&n)),
            Outcome::Pass
        );
        let mismatch = neg("runtime", "RangeError");
        assert_eq!(
            classify(Status::Exited(1), UNCAUGHT, Some(&mismatch)),
            Outcome::Fail
        );
        assert_eq!(
            classify(Status::Exited(1), "error: x", Some(&n)),
            Outcome::CompileError
        );
        assert_eq!(
            classify(Status::Exited(2), "", Some(&n)),
            Outcome::CompileError
        );
        assert_eq!(classify(Status::Exited(0), "", Some(&n)), Outcome::Fail);
        assert_eq!(classify(Status::TimedOut, "", Some(&n)), Outcome::Timeout);
    }

    #[test]
    fn observed_jsc_stderr() {
        let t262 = neg("runtime", "Test262Error");
        let class_instance = "Uncaught Test262Error { message: top }\n";
        assert_eq!(uncaught_name(class_instance), Some("Test262Error"));
        assert_eq!(
            classify(Status::Exited(1), class_instance, Some(&t262)),
            Outcome::Pass
        );
        let empty = "Uncaught Test262Error { message:  }\n";
        assert_eq!(
            classify(Status::Exited(1), empty, Some(&t262)),
            Outcome::Pass
        );
        assert_eq!(
            classify(
                Status::Exited(1),
                class_instance,
                Some(&neg("runtime", "Error"))
            ),
            Outcome::Fail
        );
        let not_evaluated = "Uncaught Test262: This statement should not be evaluated.\n";
        assert_eq!(uncaught_name(not_evaluated), Some("Test262"));
        assert_eq!(
            classify(Status::Exited(1), not_evaluated, Some(&t262)),
            Outcome::Fail
        );
    }

    #[test]
    fn async_positive() {
        let done = "Test262:AsyncTestComplete\n";
        let failure = "Test262:AsyncTestFailure:Test262Error: boom";
        let ok = Status::Exited(0);
        assert_eq!(classify_async(ok, done, "", None), (Outcome::Pass, None));
        assert_eq!(
            classify_async(ok, &format!("log\n{done}more\n"), "", None),
            (Outcome::Pass, None)
        );
        assert_eq!(
            classify_async(ok, &format!("{failure}\n"), "", None),
            (Outcome::Fail, Some(failure.to_owned()))
        );
        assert_eq!(
            classify_async(ok, &format!("{done}{failure}\n"), "", None),
            (Outcome::Fail, Some(failure.to_owned()))
        );
        for stdout in [
            "",
            "x\n",
            " Test262:AsyncTestComplete\n",
            "Test262:AsyncTestComplete!\n",
        ] {
            assert_eq!(
                classify_async(ok, stdout, "", None),
                (Outcome::Fail, Some(MISSING_COMPLETE.to_owned())),
                "{stdout:?}"
            );
        }
        assert_eq!(
            classify_async(Status::Exited(2), done, "error: x", None),
            (Outcome::CompileError, None)
        );
        assert_eq!(
            classify_async(Status::TimedOut, done, "", None),
            (Outcome::Timeout, None)
        );
        assert_eq!(
            classify_async(Status::Exited(1), done, UNCAUGHT, None),
            (Outcome::Fail, None)
        );
    }

    #[test]
    fn async_negative_follows_negative_rules() {
        let parse = neg("parse", "SyntaxError");
        assert_eq!(
            classify_async(Status::Exited(2), "", "error: x", Some(&parse)),
            (Outcome::Pass, None)
        );
        let done = "Test262:AsyncTestComplete\n";
        assert_eq!(
            classify_async(Status::Exited(0), done, "", Some(&parse)),
            (Outcome::Fail, None)
        );
        let runtime = neg("runtime", "TypeError");
        assert_eq!(
            classify_async(Status::Exited(1), "", UNCAUGHT, Some(&runtime)),
            (Outcome::Pass, None)
        );
        assert_eq!(
            classify_async(Status::Exited(0), done, "", Some(&runtime)),
            (Outcome::Fail, None)
        );
    }
}
