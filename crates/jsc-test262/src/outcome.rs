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
//! errors as `Error: Uncaught TypeError: msg`, so builtin names match. An
//! uncaught user class instance prints as an object literal
//! (`Error: Uncaught { @@prototype: ..., message: x }`) without its class
//! name, so runtime-negative tests expecting `Test262Error` cannot match and
//! count as failures until jsc names class instances. Harness assertion
//! failures throw `new Error("Test262Error: ...")` and therefore report the
//! name `Error`.

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

#[cfg(test)]
mod tests {
    use super::*;

    fn neg(phase: &str, kind: &str) -> Negative {
        Negative {
            phase: phase.into(),
            kind: kind.into(),
        }
    }

    const UNCAUGHT: &str = "Error: Uncaught TypeError: x is not a function\n";

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
        let class_instance =
            "Error: Uncaught { @@prototype: { toString: [Function (anonymous)] }, message: top }\n";
        assert_eq!(uncaught_name(class_instance), Some("{"));
        assert_eq!(
            classify(Status::Exited(1), class_instance, Some(&t262)),
            Outcome::Fail
        );
        let harness = "Error: Uncaught Error: Test262Error: Expected SameValue\n";
        assert_eq!(uncaught_name(harness), Some("Error"));
        assert_eq!(
            classify(Status::Exited(1), harness, Some(&neg("runtime", "Error"))),
            Outcome::Pass
        );
        assert_eq!(uncaught_name("Error: Uncaught str\n"), Some("str"));
    }
}
