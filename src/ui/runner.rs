//! Runs a bounded command as a separate process: an argument vector, never a shell; the
//! board's path passed as `TOWER_DB`; optional text on stdin; stdout and stderr drained
//! while it runs, so a large output cannot block it; and a bounded wait, after which it is
//! killed. The watch runs its `herdr` commands through it; the owner's own writes from the
//! dashboard go through `writer` instead.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How a run went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It exited by itself: its exit code (`None` when a signal ended it) and what it wrote.
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// It ran past the wait (`after`) and was killed: what it wrote until then, as far as
    /// its readers drained it within `DRAIN_AFTER_KILL`.
    TimedOut {
        after: Duration,
        stdout: String,
        stderr: String,
    },
    /// It could not be started or waited for, or the thread running it died.
    Failed(String),
}

/// One run to make: the executable, its arguments, the board, any more environment
/// variables and optional stdin.
#[derive(Debug, Clone)]
pub struct Run {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub db: PathBuf,
    /// Set in the run's environment besides `TOWER_DB`, as (name, value).
    pub env: Vec<(&'static str, &'static str)>,
    pub stdin: Option<String>,
    pub timeout: Duration,
}

/// Runs `run` and waits for it, at most `run.timeout`.
pub fn run_now(run: &Run) -> Outcome {
    let child = Command::new(&run.exe)
        .args(&run.args)
        .env("TOWER_DB", &run.db)
        .envs(run.env.iter().copied())
        .stdin(if run.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("cannot run {}: {e}", run.exe.display())),
    };
    // Feed stdin and drain both outputs on their own threads, so none of the three pipes
    // can fill up and stall the child while this thread waits.
    if let (Some(text), Some(mut pipe)) = (run.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    match wait(&mut child, run.timeout) {
        Ok(Some(code)) => Outcome::Exited {
            code,
            stdout: joined(stdout),
            stderr: joined(stderr),
        },
        // A process the child started may hold the pipes: the readers are given a moment,
        // then left behind, with what they have.
        Ok(None) => Outcome::TimedOut {
            after: run.timeout,
            stdout: joined_within(stdout, DRAIN_AFTER_KILL),
            stderr: joined_within(stderr, DRAIN_AFTER_KILL),
        },
        Err(e) => Outcome::Failed(format!("cannot wait for {}: {e}", run.exe.display())),
    }
}

/// Waits for `child` at most `timeout`: its exit code once it exits, or `None` after
/// killing it.
fn wait(child: &mut Child, timeout: Duration) -> std::io::Result<Option<Option<i32>>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.code()));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How long a run stopped at its limit waits for its readers to reach the end of its pipes,
/// before it takes what they have.
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(1);

/// A pipe's reader on its own thread: what it has read so far, and word when it is done.
struct Reader {
    got: Arc<Mutex<Vec<u8>>>,
    done: Receiver<()>,
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Option<Reader> {
    pipe.map(|mut p| {
        let got = Arc::new(Mutex::new(Vec::new()));
        let (tell, done) = std::sync::mpsc::channel();
        let into = Arc::clone(&got);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n @ 1..) = p.read(&mut chunk) {
                into.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..n]);
            }
            let _ = tell.send(());
        });
        Reader { got, done }
    })
}

/// What `reader` read, once it reached the end of its pipe.
fn joined(reader: Option<Reader>) -> String {
    taken(reader, |done| {
        let _ = done.recv();
    })
}

/// What `reader` read by the end of its pipe or `bound`, whichever comes first.
fn joined_within(reader: Option<Reader>, bound: Duration) -> String {
    taken(reader, |done| {
        let _ = done.recv_timeout(bound);
    })
}

fn taken(reader: Option<Reader>, wait: impl FnOnce(&Receiver<()>)) -> String {
    reader.map_or_else(String::new, |r| {
        wait(&r.done);
        let got = r.got.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&got).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A fake `tower`: a shell script with `body`, in `dir`.
    fn fake(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("tower");
        crate::test_exe::executable(&path, format!("#!/bin/sh\n{body}\n"));
        path
    }

    fn run(exe: PathBuf, args: &[&str], stdin: Option<&str>, timeout: Duration) -> Outcome {
        run_now(&Run {
            exe,
            args: args.iter().map(|a| a.to_string()).collect(),
            db: PathBuf::from("/tmp/some board.db"),
            env: Vec::new(),
            stdin: stdin.map(String::from),
            timeout,
        })
    }

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn passes_arguments_as_given_with_no_shell_and_the_board_and_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), r#"printf '%s|' "$@" "$TOWER_DB"; cat"#);
        let out = run(
            exe,
            &["post", "a b; rm -rf x", "$HOME"],
            Some("hi\nthere"),
            LONG,
        );
        let want = "post|a b; rm -rf x|$HOME|/tmp/some board.db|hi\nthere";
        assert_eq!(
            out,
            Outcome::Exited {
                code: Some(0),
                stdout: want.into(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn a_slow_tower_is_killed_after_the_wait() {
        // The fake would sleep twice TEST_WAIT; the run waits 300 ms. Even with a fresh
        // fake's first-run stall on a busy machine, a run that kills it returns well
        // inside half of TEST_WAIT, and one that waits it out never does.
        let wait = crate::watch::TEST_WAIT;
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), &format!("exec sleep {}", (wait * 2).as_secs()));
        let started = Instant::now();
        let out = run(exe, &[], None, Duration::from_millis(300));
        assert!(started.elapsed() < wait / 2, "{:?}", started.elapsed());
        let stopped = Outcome::TimedOut {
            after: Duration::from_millis(300),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert_eq!(out, stopped);
    }

    #[test]
    fn a_run_stopped_at_its_limit_keeps_what_it_wrote_even_with_its_pipes_held() {
        // It writes, starts a process that holds both pipes open past the run, and sleeps.
        // The run is stopped at 300 ms and still has what was written, and its readers
        // waited at most DRAIN_AFTER_KILL for the held pipes.
        let wait = crate::watch::TEST_WAIT;
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(
            dir.path(),
            &format!(
                "printf 'one\\ntwo'; printf oops >&2; (sleep 5) & exec sleep {}",
                (wait * 2).as_secs()
            ),
        );
        let started = Instant::now();
        let out = run(exe, &[], None, Duration::from_millis(300));
        assert!(started.elapsed() < wait / 2, "{:?}", started.elapsed());
        let stopped = Outcome::TimedOut {
            after: Duration::from_millis(300),
            stdout: "one\ntwo".into(),
            stderr: "oops".into(),
        };
        assert_eq!(out, stopped);
    }

    #[test]
    fn output_past_a_pipe_buffer_on_both_streams_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(
            dir.path(),
            "head -c 200000 /dev/zero | tr '\\0' e >&2; head -c 300000 /dev/zero | tr '\\0' o",
        );
        match run(exe, &[], None, LONG) {
            Outcome::Exited {
                code: Some(0),
                stdout,
                stderr,
            } => {
                assert_eq!((stdout.len(), stderr.len()), (300_000, 200_000));
                assert!(stdout.bytes().all(|b| b == b'o'));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refusal_reports_its_status_and_both_streams() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(
            dir.path(),
            "echo ignored; echo 'tower: M9 has no lead' >&2; exit 3",
        );
        assert_eq!(
            run(exe, &[], None, LONG),
            Outcome::Exited {
                code: Some(3),
                stdout: "ignored\n".into(),
                stderr: "tower: M9 has no lead\n".into()
            }
        );
    }

    #[test]
    fn a_missing_tower_fails_with_its_path() {
        let out = run(PathBuf::from("/nonexistent/tower"), &[], None, LONG);
        assert!(
            matches!(&out, Outcome::Failed(text) if text.starts_with("cannot run /nonexistent/tower")),
            "{out:?}"
        );
    }
}
