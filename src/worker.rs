//! `tower worker start` and `tower worker close`: how a lead starts and closes its workers
//! (implementers, testers, reviewers, standing workers), through the terminal module
//! (`terminal::for_workers_from_env`), so the lead skill's steps are the same in any terminal.
//!
//! A start (`start`) does what the lead skill did by hand around the terminal's own start:
//! - first, a terminal that starts no workers says so (`Terminal::starts_workers`), before
//!   anything is asked;
//! - then, one live worker per name: Claude does not keep session names unique, so a name
//!   Claude's session list already has, ignoring case, is refused, and so is a list that
//!   cannot be read. Nothing is started.
//! - it starts the worker (`Terminal::start_worker`) and prints its handle at once, one
//!   line of JSON (`{"handle":"..."}`), so the caller holds it whatever follows.
//! - then it waits for Claude's list to name the worker (`LISTED_WITHIN`, asking every
//!   `ASK_EVERY`). One never listed fails, saying so, the usual cause (Claude's folder-trust
//!   dialog, where a session is not listed), what its pane shows where the terminal can read
//!   it (`Terminal::shows`), and how to close it. Nothing is closed: the lead decides.
//!
//! A close (`close`) closes what a handle names and says which it was: closed now, or
//! nothing the terminal has by that handle (closed already, or never a start's handle, so a
//! mistyped one is not read as a clean close), which is no failure.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::terminal::{Closed, Handle, Terminal, WorkerStart, listed, terminal_timeout};

/// How long a started worker has to show in Claude's session list.
pub const LISTED_WITHIN: Duration = Duration::from_secs(30);
/// How often Claude's list is asked while waiting for it.
pub const ASK_EVERY: Duration = Duration::from_secs(2);

/// Whether Claude's session list has a session named `name`, ignoring case.
fn is_listed(terminal: &dyn Terminal, db: &Path, name: &str) -> Result<bool> {
    let sessions = listed(terminal.claude(), db, terminal_timeout())?;
    Ok((sessions.iter())
        .filter_map(|s| s.name.as_deref())
        .any(|n| n.eq_ignore_ascii_case(name)))
}

/// Starts the worker `name` as `worker` says and writes its handle to `out`, then waits up
/// to `within` for Claude's list to name it, asking every `every`.
pub fn start(
    terminal: &dyn Terminal,
    db: &Path,
    name: &str,
    worker: &WorkerStart,
    (within, every): (Duration, Duration),
    out: &mut dyn Write,
) -> Result<()> {
    terminal.starts_workers()?;
    let taken = is_listed(terminal, db, name).with_context(|| {
        format!("cannot tell whether the name {name} is free, so nothing was started")
    })?;
    if taken {
        bail!(
            "a live Claude session is already named {name} (ignoring case): pick another \
             name; nothing was started"
        );
    }
    let handle = terminal.start_worker(db, worker)?;
    writeln!(out, "{}", serde_json::json!({ "handle": handle.0 }))
        .and_then(|()| out.flush())
        .with_context(|| format!("started {name} as {}, but cannot print it", handle.0))?;
    let deadline = Instant::now() + within;
    // Why the last ask could not tell, when it could not.
    let unread = loop {
        let unread = match is_listed(terminal, db, name) {
            Ok(true) => return Ok(()),
            Ok(false) => None,
            Err(e) => Some(format!("{e:#}")),
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break unread;
        }
        std::thread::sleep(every.min(left));
    };
    let unread = unread.map_or(String::new(), |e| format!(" (the last ask failed: {e})"));
    let shows = (terminal.shows(db, &handle))
        .map_or(" ".to_string(), |s| format!("\nIts pane shows:\n{s}\n"));
    bail!(
        "{name} was started but is not in Claude Code's session list {} s later{unread}. A \
         session at Claude's folder-trust dialog is never listed: start a worker only in a \
         folder Claude trusts.{shows}Close it with: tower worker close '{}'",
        within.as_secs(),
        handle.0
    )
}

/// Closes what `handle` names; what to say of it.
pub fn close(terminal: &dyn Terminal, db: &Path, handle: &Handle) -> Result<String> {
    Ok(match terminal.close_worker(db, handle)? {
        Closed::Now => format!("closed {}", handle.0),
        Closed::Gone(what) => format!("{what}: already closed, or never this start's"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::rex::fake::FakeRex;

    /// A wait short enough for a test, asked often.
    const SHORT: (Duration, Duration) = (Duration::from_millis(300), Duration::from_millis(20));

    fn command(name: &str) -> Vec<String> {
        ["claude", "-n", name, "the brief"]
            .map(String::from)
            .to_vec()
    }

    /// `start` of a tab for `name` in the fake's Rex session `session:7`: how it went, and
    /// what it printed.
    fn start_tab(f: &FakeRex, name: &str) -> (Result<()>, String) {
        let command = command(name);
        let worker = WorkerStart {
            label: name,
            dir: "/srv/shop",
            command: &command,
            beside: None,
        };
        let mut out = Vec::new();
        let rex = f.module_in("session:7");
        let went = start(
            &rex,
            &f.dir.join("board.db"),
            name,
            &worker,
            SHORT,
            &mut out,
        );
        (went, String::from_utf8(out).unwrap())
    }

    /// The calls to rex that start or close something.
    fn rex_acts(f: &FakeRex) -> Vec<String> {
        (f.calls().into_iter())
            .filter(|c| !c.starts_with("claude|") && !c.contains("|api|call|"))
            .collect()
    }

    const HANDLE: &str = "{\"handle\":\"rex-tab,session:7,window:w1,block:b1\"}\n";

    #[test]
    fn a_worker_claude_lists_after_its_start_is_started_and_its_handle_printed() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        // Another worker is live; dev-1 is listed once Rex has started it.
        f.claude(&[("qa-1", 40), ("dev-1", 41)]);
        let listed = std::fs::read_to_string(dir.path().join("claude.json")).unwrap();
        f.write("claude-started.json", &listed);
        f.claude(&[("qa-1", 40)]);
        let (went, printed) = start_tab(&f, "dev-1");
        went.unwrap();
        assert_eq!(printed, HANDLE);
        assert_eq!(rex_acts(&f).len(), 1);
    }

    #[test]
    fn a_name_claude_already_lists_or_a_list_it_cannot_read_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        f.claude(&[("Dev-1", 41)]);
        let (went, printed) = start_tab(&f, "dev-1");
        assert_eq!(
            format!("{:#}", went.unwrap_err()),
            "a live Claude session is already named dev-1 (ignoring case): pick another \
             name; nothing was started"
        );
        assert_eq!(printed, "");
        f.write("claude.json", "not json");
        let (went, printed) = start_tab(&f, "dev-2");
        let why = format!("{:#}", went.unwrap_err());
        assert!(
            why.starts_with(
                "cannot tell whether the name dev-2 is free, so nothing was started: claude \
                 agents --json said something unexpected"
            ),
            "{why}"
        );
        assert_eq!(printed, "");
        // Rex was never asked to start anything.
        assert_eq!(rex_acts(&f), [] as [String; 0]);
    }

    #[test]
    fn a_worker_claude_never_lists_fails_with_its_handle_printed_and_what_its_pane_shows() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        f.write("screen", "Do you trust the files in this folder?\n");
        let (went, printed) = start_tab(&f, "dev-1");
        assert_eq!(printed, HANDLE);
        assert_eq!(
            format!("{:#}", went.unwrap_err()),
            "dev-1 was started but is not in Claude Code's session list 0 s later. A session \
             at Claude's folder-trust dialog is never listed: start a worker only in a folder \
             Claude trusts.\nIts pane shows:\nDo you trust the files in this folder?\nClose it \
             with: tower worker close 'rex-tab,session:7,window:w1,block:b1'"
        );
        // Started once, read once, and nothing closed: the lead decides.
        let acts = rex_acts(&f);
        assert_eq!(acts.len(), 2, "{acts:?}");
        assert!(acts[0].starts_with("--autostart=false|window|new|"));
        assert!(acts[1].starts_with("--autostart=false|capture|"));
        // A pane the terminal cannot read, and a list that cannot be read while waiting.
        f.write("fail-capture", "no such block");
        f.write(
            "claude-started.json",
            "not json, once the worker is started",
        );
        let (went, printed) = start_tab(&f, "dev-1");
        assert_eq!(printed, HANDLE);
        let why = format!("{:#}", went.unwrap_err());
        assert!(
            why.starts_with(
                "dev-1 was started but is not in Claude Code's session list 0 s later (the \
                 last ask failed: claude agents --json said something unexpected"
            ),
            "{why}"
        );
        assert!(
            why.ends_with(
                "start a worker only in a folder Claude trusts. Close it with: tower worker \
                 close 'rex-tab,session:7,window:w1,block:b1'"
            ),
            "{why}"
        );
    }

    #[test]
    fn a_close_says_whether_it_closed_it_or_it_was_already_closed() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let db = dir.path().join("board.db");
        f.tabs(&[("session:7", &[("block:b1", "window:w1", "")])]);
        let handle = Handle("rex-tab,session:7,window:w1,block:b1".into());
        let rex = f.module_in("session:7");
        assert_eq!(
            close(&rex, &db, &handle).unwrap(),
            "closed rex-tab,session:7,window:w1,block:b1"
        );
        f.tabs(&[("session:7", &[])]);
        assert_eq!(
            close(&rex, &db, &handle).unwrap(),
            "no such tab or pane in session:7: already closed, or never this start's"
        );
        f.tabs(&[("session:other", &[])]);
        assert_eq!(
            close(&rex, &db, &handle).unwrap(),
            "no such Rex session session:7: already closed, or never this start's"
        );
        assert_eq!(rex_acts(&f).len(), 1);
    }

    #[test]
    fn a_terminal_that_starts_no_workers_says_so_before_claudes_list_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        // The name is taken, and the core still says what it needs first.
        f.claude(&[("dev-1", 41)]);
        let core = crate::terminal::Core::new(dir.path().join("claude"));
        let command = command("dev-1");
        let worker = WorkerStart {
            label: "dev-1",
            dir: "/srv/shop",
            command: &command,
            beside: None,
        };
        let mut out = Vec::new();
        let went = start(
            &core,
            &dir.path().join("board.db"),
            "dev-1",
            &worker,
            SHORT,
            &mut out,
        );
        assert_eq!(
            format!("{:#}", went.unwrap_err()),
            "starting a worker needs Rex, and this session is not in Rex (REX_SESSION is not \
             set)"
        );
        assert_eq!(out, b"");
        // Neither claude nor rex was run.
        assert_eq!(f.calls(), [] as [String; 0]);
    }
}
