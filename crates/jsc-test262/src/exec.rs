//! Process execution: runs one program under jsc with a timeout, and fans a
//! batch of work out over a std-only worker pool.
//!
//! Each program is written to its own file under a scratch directory; jsc's
//! stdout is discarded and its stderr goes to a sibling file (not a pipe, so a
//! chatty child can never deadlock the runner). Both files are removed after
//! the run.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::outcome::Status;

/// Upper bound of the `try_wait` polling interval.
const MAX_POLL: Duration = Duration::from_millis(20);

/// Runs `jsc <dir>/<name>.js` on `program` and returns how it ended plus its
/// stderr (lossily decoded). The child is killed and reaped after `timeout`.
pub fn run(
    jsc: &Path,
    dir: &Path,
    name: &str,
    program: &str,
    timeout: Duration,
) -> io::Result<(Status, String)> {
    let src = dir.join(format!("{name}.js"));
    let err = dir.join(format!("{name}.stderr"));
    fs::write(&src, program)?;
    let result = spawn_and_wait(jsc, &src, &err, timeout);
    let stderr = fs::read(&err).map(|b| String::from_utf8_lossy(&b).into_owned());
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&err);
    Ok((result?, stderr?))
}

fn spawn_and_wait(jsc: &Path, src: &Path, err: &Path, timeout: Duration) -> io::Result<Status> {
    let mut child = Command::new(jsc)
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(err)?)
        .spawn()?;
    let start = Instant::now();
    let mut poll = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().map_or(Status::Signaled, Status::Exited));
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            child.wait()?;
            return Ok(Status::TimedOut);
        }
        thread::sleep(poll);
        poll = (poll * 2).min(MAX_POLL);
    }
}

/// Applies `f(index, item)` to every item on `jobs` worker threads (at least
/// one) and returns the results in input order.
pub fn pool<T, R, F>(items: &[T], jobs: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    let next = AtomicUsize::new(0);
    let mut indexed: Vec<(usize, R)> = thread::scope(|s| {
        let workers: Vec<_> = (0..jobs.clamp(1, items.len().max(1)))
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        done.push((i, f(i, item)));
                    }
                    done
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> io::Result<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("jsc-test262-exec-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[test]
    fn exit_codes_and_stderr() -> io::Result<()> {
        let dir = scratch("codes")?;
        let sh = Path::new("/bin/sh");
        let long = Duration::from_secs(10);
        let ok = run(sh, &dir, "a", "exit 0", long)?;
        let fail = run(sh, &dir, "b", "echo 'Error: Uncaught x' >&2; exit 3", long)?;
        let sig = run(sh, &dir, "c", "kill -9 $$", long)?;
        let left = fs::read_dir(&dir)?.count();
        fs::remove_dir_all(&dir)?;
        assert_eq!(ok, (Status::Exited(0), String::new()));
        assert_eq!(fail, (Status::Exited(3), "Error: Uncaught x\n".to_owned()));
        assert_eq!(sig.0, Status::Signaled);
        assert_eq!(left, 0, "scratch files must be removed");
        Ok(())
    }

    #[test]
    fn timeout_kills() -> io::Result<()> {
        let dir = scratch("timeout")?;
        let start = Instant::now();
        let r = run(
            Path::new("/bin/sh"),
            &dir,
            "t",
            "sleep 5",
            Duration::from_millis(100),
        );
        fs::remove_dir_all(&dir)?;
        assert_eq!(r?.0, Status::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(4));
        Ok(())
    }

    #[test]
    fn missing_binary_is_error() -> io::Result<()> {
        let dir = scratch("missing")?;
        let r = run(
            Path::new("/nonexistent/jsc"),
            &dir,
            "m",
            "",
            Duration::from_secs(1),
        );
        let left = fs::read_dir(&dir)?.count();
        fs::remove_dir_all(&dir)?;
        assert!(r.is_err());
        assert_eq!(left, 0);
        Ok(())
    }

    #[test]
    fn pool_preserves_order() {
        let items: Vec<usize> = (0..100).collect();
        for jobs in [0, 1, 7, 500] {
            let out = pool(&items, jobs, |i, x| (i, x * 2));
            assert_eq!(out, items.iter().map(|x| (*x, x * 2)).collect::<Vec<_>>());
        }
        assert!(pool(&Vec::<u8>::new(), 4, |_, x| *x).is_empty());
    }
}
