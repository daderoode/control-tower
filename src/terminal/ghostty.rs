//! Ghostty, where the core starts and closes workers: the core's own version of
//! `Terminal::starts_workers`, `start_worker` and `close_worker`, used by the trait's
//! defaults when the lead does not run in Rex. Ghostty is driven through its AppleScript
//! dictionary by `osascript`; nothing here is asked of it for the watch, delivery or the
//! dashboard, so those stay testable without it.
//!
//! Each call is one constant script (`START`, `CLOSE`) given to `osascript -e`, and every
//! value (a tty, a folder, a command, a terminal's id) is one of its arguments, never text
//! pasted into a script. `osascript` runs with nothing of the lead's session in its
//! environment but what an app opened from the Finder has (`kept`): a Ghostty it launches
//! takes its environment into every terminal it opens from then on, the owner's own tabs
//! included. Ghostty is named by its bundle id (`APP`).
//!
//! - **Installed** (`installed`): `id of application id`, which sends Ghostty no event and
//!   launches nothing. A start or close with no Ghostty says workers need Ghostty or Rex.
//! - **A start** (`start`): an implementer or standing worker is a new tab in the lead's
//!   Ghostty window, labelled, the window being the one holding the terminal whose tty is
//!   the lead's (`lead_tty`: the first process with a terminal from `tower` upwards, since a
//!   `tower` run by Claude Code has none of its own); with no such terminal (the lead is not
//!   in Ghostty) it is a new Ghostty window, which takes focus. A tester or reviewer is a
//!   split to the right of another worker's terminal, by its id. Ghostty has no way to
//!   start one without focus, so the lead's tab is selected again afterwards; when that
//!   fails the start still stands, with a note saying so.
//! - **The command** (`command_line`): one shell line, each word quoted, run by
//!   `/usr/bin/env` with the same variables removed, and with the lead's `PATH` and the
//!   board (`TOWER_DB`, absolute) set: a Ghostty command gets Ghostty's environment and no
//!   shell profile, so neither `claude` nor the hooks' `tower` would be found, and a lead on
//!   another board would get workers on the default one.
//! - **The handle** is the new terminal's id, which Ghostty never reuses (a tab's id it
//!   does): `ghostty-tab,<id>` or `ghostty-split,<id>`. It is always an id Ghostty did not
//!   have before the start: the script reads every terminal's id first, and a start whose
//!   new terminal reads back as one of those fails, labelling nothing. macOS can make a new
//!   window a tab of another, and what is focused there is read a moment later, so the
//!   terminal read back could be the owner's, which a close would then close.
//! - **A close** (`close`): a split's terminal by its id; a tab by finding the tab that
//!   holds that terminal now and closing it with every pane in it. An id Ghostty no longer
//!   has (its error -1728) is gone, not a failure, and so is any id while Ghostty is not
//!   running, which is not launched to say so. Only an id from a handle is ever closed or
//!   split, and the lead's tab is only ever selected.
//! - **A closed split's worker** (`end`): Ghostty ends a tab's processes when it closes
//!   the tab, but leaves a closed split's running, so its Claude session would stay listed
//!   with its name taken. The close reads the terminal's pid (its `/usr/bin/login` wrapper)
//!   and tty first; afterwards, what still runs on that tty under that wrapper
//!   (`on_terminal`) gets a moment to end (`ENDS_WITHIN`), then a hangup (SIGHUP), as a
//!   terminal closing sends, and the close fails, naming them, when they outlive that too
//!   (`HUNG_UP_WITHIN`). Nothing else is ever signalled.
//! - **Time**: each script runs under `with timeout` (`EVENT_SECS`) and `osascript` within
//!   `terminal_timeout`. Ghostty not answering (-1712), or macOS refusing the event
//!   (-1743), is what an Automation permission not yet given looks like, so both say to
//!   approve Automation for Ghostty.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::{Closed, Handle, WorkerStart, is_id, terminal_timeout};
use crate::ui::runner::{Outcome, Run, run_now};

/// Ghostty's bundle id.
const APP: &str = "com.mitchellh.ghostty";

/// What a lead's session sets that must not reach a worker, removed from its command's
/// environment: Claude Code's and Rex's own, naming the lead's session, pane and sockets. A
/// fixed list, not every `CLAUDE*`: a setting the owner exports on purpose (a config folder,
/// a provider) stays.
const UNSET: [&str; 14] = [
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "REX_SESSION",
    "REX_BLOCK",
    "REX_SERVER",
    "REX_BIN_DIR",
];

/// What `osascript` keeps of this process's environment, with every `LC_` name: what an
/// app opened from the Finder has, and nothing a session adds (`kept`).
const KEPT: [&str; 8] = [
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "SSH_AUTH_SOCK",
    "__CF_USER_TEXT_ENCODING",
];

/// The `PATH` `osascript` runs with, as macOS gives an app opened from the Finder: never
/// the lead's.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The environment `osascript` runs with, of `vars`: the `KEPT` names and each `LC_` one,
/// as they are, and `SYSTEM_PATH`. Nothing else, so a Ghostty that `osascript` launches
/// starts as one opened from the Finder would and carries none of the lead's session (its
/// Claude and Rex variables, its board, `GIT_EDITOR`) into the owner's own tabs.
fn kept(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vars
        .filter(|(name, _)| KEPT.contains(&name.as_str()) || name.starts_with("LC_"))
        .collect();
    env.push(("PATH".into(), SYSTEM_PATH.into()));
    env
}

/// How long Ghostty has to answer one event of a script, in seconds, as the scripts say it
/// (`with timeout`).
const EVENT_SECS: u64 = 10;

/// Starts a worker. Arguments: the lead's tty ("" for none), the folder, the command, the
/// tab's label, the terminal id to split beside ("" for a tab). Prints `id <terminal id>`,
/// then a `note <text>` line for each thing that failed after the terminal was made; or
/// `gone` when there is no terminal to split beside; or `reused <terminal id>` when the
/// new terminal reads back as one Ghostty had before the start, which is then not touched.
const START: &str = r#"on run argv
	set leadTty to item 1 of argv
	set besideId to item 5 of argv
	with timeout of 10 seconds
		tell application id "com.mitchellh.ghostty"
			set hadIds to id of every terminal
			set cfg to new surface configuration
			set initial working directory of cfg to item 2 of argv
			set command of cfg to item 3 of argv
			set leadWindow to ""
			set leadTab to ""
			if leadTty is not "" then
				repeat with w in windows
					repeat with t in tabs of w
						repeat with term in terminals of t
							if (tty of term) is leadTty then
								set leadWindow to id of w
								set leadTab to id of t
							end if
						end repeat
					end repeat
				end repeat
			end if
			set newTab to missing value
			if besideId is not "" then
				try
					set made to split terminal id besideId direction right with configuration cfg
				on error number -1728
					return "gone"
				end try
			else
				if leadWindow is "" then
					set newWindow to new window with configuration cfg
					set newTab to selected tab of newWindow
				else
					set newTab to new tab in window id leadWindow with configuration cfg
				end if
				set made to focused terminal of newTab
			end if
			set newId to id of made
			if hadIds contains newId then return "reused " & newId
			set said to "id " & newId
			if newTab is not missing value then
				try
					set name override of newTab to item 4 of argv
				on error why
					set said to said & linefeed & "note its tab was not labelled: " & why
				end try
			end if
			if leadTab is not "" then
				try
					select tab (tab id leadTab of window id leadWindow)
				on error why
					set said to said & linefeed & "note the lead's tab was not selected again, so select it yourself: " & why
				end try
			end if
			return said
		end tell
	end timeout
end run"#;

/// Closes what a handle names. Arguments: `tab` or `split`, the terminal's id. Prints
/// `closed` for a tab and `closed <pid> <tty>` for a split, the terminal's as they were
/// before the close; or `gone` when Ghostty has no such terminal, or is not running (it is
/// not launched to be asked).
const CLOSE: &str = r#"on run argv
	set theId to item 2 of argv
	if application id "com.mitchellh.ghostty" is not running then return "gone"
	with timeout of 10 seconds
		tell application id "com.mitchellh.ghostty"
			if item 1 of argv is "split" then
				try
					set thePid to pid of terminal id theId
					set theTty to tty of terminal id theId
					close terminal id theId
				on error number -1728
					return "gone"
				end try
				return "closed " & thePid & " " & theTty
			end if
			repeat with w in windows
				repeat with t in tabs of w
					repeat with term in terminals of t
						if (id of term) is theId then
							set windowId to id of w
							set tabId to id of t
							try
								close tab (tab id tabId of window id windowId)
							on error number -1728
								return "gone"
							end try
							return "closed"
						end if
					end repeat
				end repeat
			end repeat
			return "gone"
		end tell
	end timeout
end run"#;

/// What a worker's handle names: the terminal's id, and whether its whole tab is the
/// worker's.
#[derive(Debug, PartialEq, Eq)]
enum Made<'a> {
    Tab(&'a str),
    Split(&'a str),
}

impl<'a> Made<'a> {
    /// What `handle` names, when it is one `start` wrote.
    fn of(handle: &'a Handle) -> Result<Made<'a>> {
        match handle.0.split(',').collect::<Vec<_>>()[..] {
            ["ghostty-tab", id] if is_id(id) => Ok(Made::Tab(id)),
            ["ghostty-split", id] if is_id(id) => Ok(Made::Split(id)),
            _ => bail!(
                "{:?} is not a handle tower worker start printed in Ghostty",
                handle.0
            ),
        }
    }

    /// The worker's terminal, and the script's word for which it is.
    fn terminal(&self) -> (&'static str, &'a str) {
        match *self {
            Made::Tab(id) => ("tab", id),
            Made::Split(id) => ("split", id),
        }
    }
}

/// A started worker: its handle, and what failed after its terminal was made (its label,
/// selecting the lead's tab again), for the lead to read beside the handle.
#[derive(Debug, PartialEq, Eq)]
pub struct Started {
    pub handle: Handle,
    pub notes: Vec<String>,
}

/// How long what runs in a closed split has to end by itself before it is hung up.
const ENDS_WITHIN: Duration = Duration::from_secs(1);
/// How long it then has to end: a Claude session takes some seconds to leave.
const HUNG_UP_WITHIN: Duration = Duration::from_secs(15);
/// How often `ps` is asked while waiting for either.
const ASK_EVERY: Duration = Duration::from_millis(250);

/// Ghostty as the core reaches it: `osascript` and the whole environment it runs with
/// (`kept`), `ps` to find the lead's terminal and what a closed split left running, the
/// lead's `PATH` for its workers, and how long a closed split's worker has to end by itself
/// and then after a hangup.
#[derive(Debug, Clone)]
pub struct Ghostty {
    osascript: PathBuf,
    env: Vec<(String, String)>,
    ps: PathBuf,
    path: Option<String>,
    ends_within: Duration,
    hung_up_within: Duration,
}

/// `word` as one word of a shell line: in single quotes, each one of its own written as
/// `'\''`.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// What `ps -A -o pid=,ppid=,tty=` printed (`table`), as (pid, parent's pid, tty), each pid
/// as a signal takes one. A line that is not such a row is no process, and neither is one
/// whose pid is not above 1: the first process is never a worker's, and 0 or a negative
/// one would, as a signal's target, name whole groups of processes or every process.
fn rows(table: &str) -> Vec<(i32, i32, &str)> {
    (table.lines())
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let row: (i32, i32, &str) = (
                words.next()?.parse().ok()?,
                words.next()?.parse().ok()?,
                words.next()?,
            );
            (row.0 > 1).then_some(row)
        })
        .collect()
}

/// The processes of the terminal whose wrapper is `wrapper` and whose tty is `tty` (as
/// Ghostty names it, `/dev/ttys010`), in what `ps` printed (`table`): the wrapper, and
/// each process under it, that is on that tty. None while the wrapper is not on that tty
/// (it ended, and its pid may be another process's by now), and never the first process.
fn on_terminal(table: &str, wrapper: i32, tty: &str) -> Vec<i32> {
    let rows = rows(table);
    let on_tty = |row_tty: &str| format!("/dev/{row_tty}") == tty;
    let wrapper_there = (rows.iter()).any(|(pid, _, row_tty)| *pid == wrapper && on_tty(row_tty));
    if wrapper <= 1 || !wrapper_there {
        return Vec::new();
    }
    let under = |pid: i32| {
        let mut at = pid;
        // No more steps than there are processes: a table that loops ends.
        for _ in 0..rows.len() {
            if at == wrapper {
                return true;
            }
            match rows.iter().find(|(p, _, _)| *p == at) {
                Some((_, parent, _)) => at = *parent,
                None => return false,
            }
        }
        false
    };
    (rows.iter())
        .filter(|(pid, _, row_tty)| on_tty(row_tty) && under(*pid))
        .map(|(pid, _, _)| *pid)
        .collect()
}

/// The tty of the first process with one from `pid` upwards, as Ghostty names it
/// (`/dev/ttys010`), in what `ps -A -o pid=,ppid=,tty=` printed (`table`): `None` when no
/// process up to the first has a terminal. A process with none shows `??` (macOS) or `?`.
fn tty_of(table: &str, pid: i32) -> Option<String> {
    let rows = rows(table);
    let mut at = pid;
    // No more steps than there are processes: a table that loops ends.
    for _ in 0..rows.len() {
        let (_, parent, tty) = rows.iter().find(|(p, _, _)| *p == at)?;
        if !tty.starts_with('?') {
            return Some(format!("/dev/{tty}"));
        }
        at = *parent;
    }
    None
}

impl Ghostty {
    /// Ghostty by this process's environment: `osascript` as `TOWER_OSASCRIPT` names it,
    /// else the system's, run with `kept` of this environment (a variable that is not text
    /// is not kept); the system's `ps`; and `PATH` for the workers (one that is not text is
    /// not passed on).
    pub fn from_env() -> Ghostty {
        let osascript = std::env::var_os("TOWER_OSASCRIPT")
            .filter(|p| !p.is_empty())
            .map_or_else(|| PathBuf::from("/usr/bin/osascript"), PathBuf::from);
        let vars = std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        });
        Ghostty {
            osascript,
            env: kept(vars),
            ps: PathBuf::from("/bin/ps"),
            path: std::env::var("PATH").ok(),
            ends_within: ENDS_WITHIN,
            hung_up_within: HUNG_UP_WITHIN,
        }
    }

    /// Runs `osascript -e <script> <args>` with `env` as its whole environment (`env -i`),
    /// bounded by `terminal_timeout`.
    fn run(&self, script: &str, args: &[&str]) -> Outcome {
        let mut words: Vec<String> = vec!["-i".to_string()];
        words.extend(
            self.env
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
        words.push(self.osascript.to_string_lossy().into_owned());
        words.extend(["-e".to_string(), script.to_string()]);
        words.extend(args.iter().map(|a| a.to_string()));
        run_now(&Run {
            exe: PathBuf::from("/usr/bin/env"),
            args: words,
            // No board is asked: `TOWER_DB` goes with everything `env -i` drops.
            db: PathBuf::new(),
            env: Vec::new(),
            stdin: None,
            timeout: terminal_timeout(),
        })
    }

    /// `Ok` when Ghostty is installed; else why workers cannot start in it: it is not
    /// installed, or `osascript` could not say.
    pub fn installed(&self) -> Result<(), String> {
        let script = format!("id of application id \"{APP}\"");
        match self.run(&script, &[]) {
            Outcome::Exited { code: Some(0), .. } => Ok(()),
            Outcome::Exited { stderr, .. } if stderr.contains("(-1728)") => {
                Err("Ghostty is not installed".into())
            }
            other => Err(format!(
                "whether Ghostty is installed is not known ({})",
                failed(other)
            )),
        }
    }

    /// The lead's tty (`tty_of`, from this process upwards), `None` when it has none.
    fn lead_tty(&self) -> Result<Option<String>> {
        match self.processes() {
            // A pid of this process's that no signal could take is no row of ps's either.
            Ok(table) => Ok(i32::try_from(std::process::id())
                .ok()
                .and_then(|me| tty_of(&table, me))),
            Err(why) => bail!("cannot find the lead's terminal, so nothing was started: {why}"),
        }
    }

    /// Every process as `ps -A -o pid=,ppid=,tty=` prints it, or how `ps` failed.
    fn processes(&self) -> Result<String, String> {
        let outcome = run_now(&Run {
            exe: self.ps.clone(),
            args: ["-A", "-o", "pid=,ppid=,tty="].map(String::from).to_vec(),
            db: PathBuf::new(),
            env: Vec::new(),
            stdin: None,
            timeout: terminal_timeout(),
        });
        match outcome {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => Ok(stdout),
            other => Err(format!("ps {}", failed(other))),
        }
    }

    /// What still runs of a closed terminal (`on_terminal`) once it has had `within` to
    /// end, asked every `ASK_EVERY`: empty as soon as nothing does.
    fn left(&self, wrapper: i32, tty: &str, within: Duration) -> Result<Vec<i32>> {
        let deadline = Instant::now() + within;
        loop {
            let table = (self.processes()).map_err(|why| {
                anyhow::anyhow!(
                    "its pane is closed, but whether its worker ended is not known, on {tty} \
                     under pid {wrapper}: {why}"
                )
            })?;
            let left = on_terminal(&table, wrapper, tty);
            let time = deadline.saturating_duration_since(Instant::now());
            if left.is_empty() || time.is_zero() {
                return Ok(left);
            }
            std::thread::sleep(ASK_EVERY.min(time));
        }
    }

    /// Ends what a closed split left running: the processes on `tty` under `wrapper`, the
    /// terminal's as they were before its close. Given `ends_within` to end by themselves,
    /// they are then hung up, each by its own pid, and fail the close when they still run
    /// `hung_up_within` later.
    fn end(&self, wrapper: i32, tty: &str) -> Result<()> {
        let left = self.left(wrapper, tty, self.ends_within)?;
        if left.is_empty() {
            return Ok(());
        }
        for pid in &left {
            // One that ended meanwhile is no failure.
            let _ = kill(Pid::from_raw(*pid), Signal::SIGHUP);
        }
        let left = self.left(wrapper, tty, self.hung_up_within)?;
        if left.is_empty() {
            return Ok(());
        }
        let pids: Vec<String> = left.iter().map(i32::to_string).collect();
        bail!(
            "its pane is closed, but its worker still runs {} s after a hangup, on {tty}: end \
             it with: kill {}",
            self.hung_up_within.as_secs(),
            pids.join(" ")
        )
    }

    /// The one shell line a worker's terminal runs: `worker`'s command, each word quoted,
    /// with `UNSET` removed, and the lead's `PATH` and the board `db` (absolute) set.
    fn command_line(&self, db: &Path, worker: &WorkerStart) -> Result<String> {
        let board = std::path::absolute(db)
            .with_context(|| format!("cannot make {} an absolute path", db.display()))?;
        let Some(board) = board.to_str() else {
            bail!(
                "the board's path {} is not text, so a worker cannot be given it",
                board.display()
            );
        };
        let mut line = String::from("/usr/bin/env");
        for name in UNSET {
            line.push_str(&format!(" -u {name}"));
        }
        if let Some(path) = &self.path {
            line.push_str(&format!(" PATH={}", quoted(path)));
        }
        line.push_str(&format!(" TOWER_DB={}", quoted(board)));
        for word in worker.command {
            line.push(' ');
            line.push_str(&quoted(word));
        }
        Ok(line)
    }

    /// Starts `worker` (`Terminal::start_worker`) for the board `db`.
    pub fn start(&self, db: &Path, worker: &WorkerStart) -> Result<Started> {
        let beside = worker.beside.map(Made::of).transpose()?;
        let beside_id = beside.as_ref().map_or("", |made| made.terminal().1);
        let command = self.command_line(db, worker)?;
        let tty = self.lead_tty()?.unwrap_or_default();
        let args = [
            tty.as_str(),
            worker.dir,
            command.as_str(),
            worker.label,
            beside_id,
        ];
        // Ghostty may have made it before osascript stopped, or before a reply this cannot
        // read; with no id it cannot be closed here.
        let left = match beside {
            None => format!(
                "a tab labelled {:?} may be left in Ghostty: close it there",
                worker.label
            ),
            Some(_) => {
                format!("a pane right of {beside_id} may be left in Ghostty: close it there")
            }
        };
        let reply = said("start a worker", self.run(START, &args)).context(left.clone())?;
        let mut lines = reply.lines();
        let first = lines.next().unwrap_or_default();
        if first == "gone" {
            bail!(
                "Ghostty has no terminal {beside_id} to start a worker beside: it is closed, \
                 or the handle was never a start's; nothing was started"
            );
        }
        if let Some(id) = first.strip_prefix("reused ") {
            return Err(anyhow::anyhow!(
                "Ghostty named terminal {id} as the worker's, but had it before the start, so \
                 it is not the worker's: it was not labelled and is no handle"
            )
            .context(left));
        }
        let id = (first.strip_prefix("id ").filter(|id| is_id(id)))
            .with_context(|| format!("osascript said something unexpected: {}", reply.trim()))
            .context(left)?;
        let kind = match beside {
            None => "ghostty-tab",
            Some(_) => "ghostty-split",
        };
        Ok(Started {
            handle: Handle(format!("{kind},{id}")),
            notes: (lines.filter_map(|l| l.strip_prefix("note ")))
                .map(String::from)
                .collect(),
        })
    }

    /// Closes what `handle` names (`Terminal::close_worker`), and ends what a split's close
    /// left running (`end`).
    pub fn close(&self, handle: &Handle) -> Result<Closed> {
        let (kind, id) = Made::of(handle)?.terminal();
        let reply = said("close a worker", self.run(CLOSE, &[kind, id]))?;
        let unexpected = || format!("osascript said something unexpected: {}", reply.trim());
        match (kind, &reply.split_whitespace().collect::<Vec<_>>()[..]) {
            (_, ["gone"]) => Ok(Closed::Gone(format!("no such terminal in Ghostty ({id})"))),
            ("tab", ["closed"]) => Ok(Closed::Now),
            ("split", ["closed", pid, tty]) => {
                let wrapper: i32 = pid.parse().ok().with_context(unexpected)?;
                self.end(wrapper, tty)?;
                Ok(Closed::Now)
            }
            _ => bail!(unexpected()),
        }
    }
}

/// How a run that did not exit 0 went, in words: its exit and what it said, or that it was
/// stopped.
fn failed(outcome: Outcome) -> String {
    match outcome {
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            let how = code.map_or("was ended by a signal".into(), |c| format!("exited {c}"));
            let said = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            format!("{how}: {}", said.trim())
        }
        Outcome::TimedOut { after, .. } => {
            format!("did not finish in {} s and was stopped", after.as_secs())
        }
        Outcome::Failed(why) => why,
    }
}

/// What a script printed, asked `to` do something. One Ghostty did not answer (-1712, or
/// `osascript` stopped at its time limit), or was not allowed to be asked (-1743), fails
/// saying to approve Automation for Ghostty.
fn said(to: &str, outcome: Outcome) -> Result<String> {
    let unanswered = match &outcome {
        Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => return Ok(stdout.clone()),
        Outcome::Exited { stderr, .. } => stderr.contains("(-1712)") || stderr.contains("(-1743)"),
        Outcome::TimedOut { .. } => true,
        Outcome::Failed(_) => false,
    };
    let how = failed(outcome);
    if unanswered {
        bail!(
            "Ghostty did not answer when asked to {to} (each event has {EVENT_SECS} s): \
             approve Automation for Ghostty (System Settings, Privacy & Security, Automation: \
             let the app this session runs in control Ghostty), then run this again. osascript \
             {how}"
        );
    }
    bail!("osascript, asked to {to}, {how}")
}

/// Test only: a fake `osascript` and `ps` in a temp folder, and Ghostty over them. Each
/// osascript call writes the names in its environment to `osascript.env` and goes to
/// `osascript.log`, one per line: which script it was (`installed`,
/// `start`, `close`) and its arguments, joined by `|`, with a start's command (its third
/// argument) written to the file `command` and logged as `<command>`. For `<it>` one of
/// those three: `fail-<it>` exits 1 saying that file's text, and `reply-<it>` is printed in
/// place of the answer. A start answers `id T1` (a tab) or `id T2` (a split), and first
/// makes `claude-started.json`, when there is one, Claude's list (`rex::fake`'s claude
/// reads it); a close answers `closed` for a tab and, for a split, `closed 999999
/// /dev/ttys999`, a terminal with no process left, and first kills (-9) the pids in
/// `kill-on-close`, when there is one. `ps` prints the file `ps.txt`, then each row of
/// `live.txt` (invented parents and ttys) whose pid is a process that still runs.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::Ghostty;

    pub struct FakeGhostty {
        pub dir: PathBuf,
    }

    const OSASCRIPT: &str = r#"#!/bin/sh
d=$(dirname "$0")
[ "$1" = "-e" ] || { echo "unexpected: $*" >&2; exit 2; }
script=$2; shift 2
case "$script" in
'id of application id "com.mitchellh.ghostty"') w=installed ;;
*"new surface configuration"*) w=start ;;
*"close terminal id"*) w=close ;;
*) echo "unexpected script: $script" >&2; exit 2 ;;
esac
env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort > "$d/osascript.env"
if [ "$w" = start ]; then
  printf '%s' "$3" > "$d/command"
  echo "start|$1|$2|<command>|$4|$5" >> "$d/osascript.log"
else
  (IFS='|'; echo "$w|$*") >> "$d/osascript.log"
fi
[ -e "$d/fail-$w" ] && { cat "$d/fail-$w" >&2; exit 1; }
if [ "$w" = close ] && [ -e "$d/kill-on-close" ]; then kill -9 $(cat "$d/kill-on-close"); fi
if [ "$w" = start ] && [ -e "$d/claude-started.json" ]; then cp "$d/claude-started.json" "$d/claude.json"; fi
if [ -e "$d/reply-$w" ]; then cat "$d/reply-$w"; exit 0; fi
case "$w" in
installed) echo com.mitchellh.ghostty ;;
start) if [ -n "$5" ]; then echo "id T2"; else echo "id T1"; fi ;;
close) if [ "$1" = split ]; then echo "closed 999999 /dev/ttys999"; else echo closed; fi ;;
esac
"#;

    /// The fake ps: `ps.txt`, then each row of `live.txt` whose process still runs (one
    /// that ended and waits to be reaped does not).
    const PS: &str = r#"#!/bin/sh
d=$(dirname "$0")
cat "$d/ps.txt" || exit 1
if [ -e "$d/live.txt" ]; then
  while read -r pid rest; do
    case "$(/bin/ps -o stat= -p "$pid" 2>/dev/null)" in ""|Z*) ;; *) echo "$pid $rest" ;; esac
  done < "$d/live.txt"
fi
exit 0
"#;

    impl FakeGhostty {
        /// The fakes in `dir`: Ghostty installed, and this process with no terminal.
        pub fn new(dir: &Path) -> FakeGhostty {
            crate::test_exe::executable(&dir.join("osascript"), OSASCRIPT);
            crate::test_exe::executable(&dir.join("ps"), PS);
            let f = FakeGhostty {
                dir: dir.to_path_buf(),
            };
            f.lead_tty(None);
            f
        }

        /// Ghostty over the fakes, with `/usr/bin:/bin` as the lead's PATH.
        pub fn ghostty(&self) -> Ghostty {
            Ghostty {
                osascript: self.dir.join("osascript"),
                env: super::kept(
                    [("HOME", "/Users/someone"), ("LC_ALL", "C")]
                        .map(|(name, value)| (name.to_string(), value.to_string()))
                        .into_iter(),
                ),
                ps: self.dir.join("ps"),
                path: Some("/usr/bin:/bin".into()),
                ends_within: Duration::from_millis(300),
                hung_up_within: Duration::from_secs(20),
            }
        }

        pub fn write(&self, file: &str, contents: &str) {
            std::fs::write(self.dir.join(file), contents).unwrap();
        }

        /// What `ps` prints from now on: this process with no terminal, under a parent
        /// with `tty` (none when `None`).
        pub fn lead_tty(&self, tty: Option<&str>) {
            let me = std::process::id();
            let parent = i32::MAX - 1;
            let tty = tty.unwrap_or("??");
            self.write(
                "ps.txt",
                &format!("    1     0 ??\n{parent}     1 {tty}\n{me} {parent} ??\n"),
            );
        }

        /// The osascript calls made so far, oldest first.
        pub fn calls(&self) -> Vec<String> {
            (std::fs::read_to_string(self.dir.join("osascript.log")).unwrap_or_default())
                .lines()
                .map(String::from)
                .collect()
        }

        /// The names in the last osascript call's environment, but those a shell sets for
        /// itself.
        pub fn env(&self) -> Vec<String> {
            (std::fs::read_to_string(self.dir.join("osascript.env")).unwrap())
                .lines()
                .filter(|name| !["PWD", "OLDPWD", "SHLVL", "_"].contains(name))
                // macOS may add this one to a process by itself; it is one of the kept.
                .filter(|name| *name != "__CF_USER_TEXT_ENCODING")
                .map(String::from)
                .collect()
        }

        /// The command the last start gave Ghostty.
        pub fn command(&self) -> String {
            std::fs::read_to_string(self.dir.join("command")).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGhostty;
    use super::*;

    /// The fakes in a temp folder, and its board's path.
    fn fakes() -> (tempfile::TempDir, FakeGhostty, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeGhostty::new(dir.path());
        let db = dir.path().join("board.db");
        (dir, f, db)
    }

    fn worker<'a>(
        label: &'a str,
        command: &'a [String],
        beside: Option<&'a Handle>,
    ) -> WorkerStart<'a> {
        WorkerStart {
            label,
            dir: "/srv/shop",
            command,
            beside,
        }
    }

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// The `-u` words that remove `UNSET`.
    fn unset() -> String {
        UNSET.map(|name| format!(" -u {name}")).concat()
    }

    fn exited(stderr: &str) -> Outcome {
        Outcome::Exited {
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn an_implementer_is_a_tab_in_the_leads_window_and_a_tester_a_split_beside_it() {
        let (_dir, f, db) = fakes();
        f.lead_tty(Some("ttys010"));
        let dev = words(&["claude", "-n", "dev-1", "Brief on T3."]);
        let started = f
            .ghostty()
            .start(&db, &worker("#12 cart", &dev, None))
            .unwrap();
        let tab = Handle("ghostty-tab,T1".into());
        assert_eq!(
            started,
            Started {
                handle: tab.clone(),
                notes: vec![],
            }
        );
        // The lead's tty, the folder, the label, and no terminal to split.
        assert_eq!(
            f.calls(),
            ["start|/dev/ttys010|/srv/shop|<command>|#12 cart|"]
        );
        // The command: the lead's variables removed, its PATH and board set, each word
        // quoted.
        assert_eq!(
            f.command(),
            format!(
                "/usr/bin/env{} PATH='/usr/bin:/bin' TOWER_DB='{}' 'claude' '-n' 'dev-1' \
                 'Brief on T3.'",
                unset(),
                db.display()
            )
        );
        // A tester beside it, by the tab's handle; a reviewer beside the tester.
        let qa = words(&["claude", "-n", "qa-1", "Test it."]);
        let started = f
            .ghostty()
            .start(&db, &worker("unused", &qa, Some(&tab)))
            .unwrap();
        let split = Handle("ghostty-split,T2".into());
        assert_eq!(started.handle, split);
        (f.ghostty())
            .start(&db, &worker("unused", &qa, Some(&split)))
            .unwrap();
        assert_eq!(
            f.calls()[1..],
            [
                "start|/dev/ttys010|/srv/shop|<command>|unused|T1",
                "start|/dev/ttys010|/srv/shop|<command>|unused|T2",
            ]
        );
    }

    #[test]
    fn a_lead_with_no_terminal_gives_no_tty_and_a_ps_that_fails_starts_nothing() {
        let (dir, f, db) = fakes();
        let dev = words(&["claude", "-n", "dev-1", "Brief."]);
        // No process from this one upwards has a terminal: no tty, so a new window.
        (f.ghostty().start(&db, &worker("dev-1", &dev, None))).unwrap();
        assert_eq!(f.calls(), ["start||/srv/shop|<command>|dev-1|"]);
        std::fs::remove_file(dir.path().join("ps.txt")).unwrap();
        let went = f.ghostty().start(&db, &worker("dev-1", &dev, None));
        let why = format!("{:#}", went.unwrap_err());
        assert!(
            why.starts_with(
                "cannot find the lead's terminal, so nothing was started: ps exited 1: cat: "
            ),
            "{why}"
        );
        assert_eq!(f.calls().len(), 1);
    }

    #[test]
    fn the_leads_tty_is_the_first_from_its_process_upwards() {
        let table = "    1     0 ??      \n  400     1 ttys003 \n  410   400 ttys010 \n  \
                     420   410 ??      \n  430   420 ??      \n  500     1 ttys001 \n";
        // Its own, when it has one; else the nearest above it.
        assert_eq!(tty_of(table, 410), Some("/dev/ttys010".into()));
        assert_eq!(tty_of(table, 430), Some("/dev/ttys010".into()));
        assert_eq!(tty_of(table, 400), Some("/dev/ttys003".into()));
        // None up to the first process, a process ps does not list, and Linux's `?`.
        assert_eq!(tty_of("    1     0 ??\n   20     1 ??\n", 20), None);
        assert_eq!(tty_of(table, 999), None);
        assert_eq!(
            tty_of("1 0 ?\n20 1 ?\n30 20 pts/4\n", 30),
            Some("/dev/pts/4".into())
        );
        assert_eq!(tty_of("1 0 ?\n20 1 ?\n", 20), None);
        // A table that loops, and lines that are not rows, end with none.
        assert_eq!(tty_of("20 30 ??\n30 20 ??\n", 20), None);
        assert_eq!(tty_of("PID PPID TTY\n\n20 x ??\n", 20), None);
    }

    #[test]
    fn a_brief_reaches_claude_as_one_word_whatever_it_holds_with_the_leads_variables_gone() {
        let (dir, f, db) = fakes();
        // A fake claude on the worker's PATH writes each of its words, and what it sees of
        // two variables, to files.
        let bin = dir.path().join("my bin");
        std::fs::create_dir(&bin).unwrap();
        crate::test_exe::executable(
            &bin.join("claude"),
            "#!/bin/sh\nd=$(dirname \"$0\")\nn=0\nfor w in \"$@\"; do n=$((n+1)); printf '%s' \
             \"$w\" > \"$d/word$n\"; done\nprintf '%s|%s|%s' \"$n\" \"${CLAUDECODE-unset}\" \
             \"$TOWER_DB\" > \"$d/saw\"\n",
        );
        let brief = "It's \"T3\": run `tower thread show T3`; $HOME \\ and\na second line";
        let command = words(&["claude", "-n", "dev-1", brief]);
        let ghostty = Ghostty {
            path: Some(format!("{}:/usr/bin:/bin", bin.display())),
            ..f.ghostty()
        };
        let line = ghostty
            .command_line(&db, &worker("dev-1", &command, None))
            .unwrap();
        // As Ghostty runs it: through a shell, in an environment holding a lead's variable.
        let ran = std::process::Command::new("/bin/sh")
            .args(["-c", &line])
            .env("CLAUDECODE", "1")
            .env("TOWER_DB", "/another/board.db")
            .status()
            .unwrap();
        assert!(ran.success());
        let read = |file: &str| std::fs::read_to_string(bin.join(file)).unwrap();
        assert_eq!(read("saw"), format!("3|unset|{}", db.display()));
        assert_eq!(read("word1"), "-n");
        assert_eq!(read("word2"), "dev-1");
        assert_eq!(read("word3"), brief);
        // A relative board is the worker's as an absolute path; no PATH is none set.
        let bare = Ghostty {
            path: None,
            ..f.ghostty()
        };
        let line = bare
            .command_line(Path::new("board.db"), &worker("dev-1", &command[..1], None))
            .unwrap();
        let board = std::env::current_dir().unwrap().join("board.db");
        assert_eq!(
            line,
            format!(
                "/usr/bin/env{} TOWER_DB='{}' 'claude'",
                unset(),
                board.display()
            )
        );
    }

    #[test]
    fn a_start_keeps_the_notes_and_fails_on_a_gone_beside_or_a_reply_it_cannot_read() {
        let (_dir, f, db) = fakes();
        let dev = words(&["claude", "-n", "dev-1", "Brief."]);
        let start = |beside: Option<&Handle>| f.ghostty().start(&db, &worker("#1 x", &dev, beside));
        // What failed after the terminal was made comes back beside the handle.
        f.write(
            "reply-start",
            "id 8A1F-22\nnote its tab was not labelled: no\nnote the lead's tab was not \
             selected again, so select it yourself: gone\n",
        );
        assert_eq!(
            start(None).unwrap(),
            Started {
                handle: Handle("ghostty-tab,8A1F-22".into()),
                notes: vec![
                    "its tab was not labelled: no".into(),
                    "the lead's tab was not selected again, so select it yourself: gone".into(),
                ],
            }
        );
        // The new terminal reads back as one Ghostty had before the start (a window macOS
        // made a tab of the owner's): no handle, and what may be left.
        f.write("reply-start", "reused 8A1F-22\n");
        assert_eq!(
            format!("{:#}", start(None).unwrap_err()),
            "a tab labelled \"#1 x\" may be left in Ghostty: close it there: Ghostty named \
             terminal 8A1F-22 as the worker's, but had it before the start, so it is not the \
             worker's: it was not labelled and is no handle"
        );
        // The terminal to split beside is gone: nothing started, nothing left.
        f.write("reply-start", "gone\n");
        let tab = Handle("ghostty-tab,T1".into());
        assert_eq!(
            format!("{:#}", start(Some(&tab)).unwrap_err()),
            "Ghostty has no terminal T1 to start a worker beside: it is closed, or the handle \
             was never a start's; nothing was started"
        );
        // No id, or one a handle cannot keep: what may be left, and what was said.
        for (reply, said) in [
            ("", ""),
            ("id \n", "id"),
            ("id a,b\n", "id a,b"),
            ("T1\n", "T1"),
        ] {
            f.write("reply-start", reply);
            assert_eq!(
                format!("{:#}", start(None).unwrap_err()),
                format!(
                    "a tab labelled \"#1 x\" may be left in Ghostty: close it there: osascript \
                     said something unexpected: {said}"
                )
            );
        }
        f.write("reply-start", "nothing\n");
        assert_eq!(
            format!("{:#}", start(Some(&tab)).unwrap_err()),
            "a pane right of T1 may be left in Ghostty: close it there: osascript said \
             something unexpected: nothing"
        );
        // Any other failure of the script, as osascript said it.
        std::fs::remove_file(f.dir.join("reply-start")).unwrap();
        f.write(
            "fail-start",
            "execution error: Ghostty got an error: no (-50)\n",
        );
        assert_eq!(
            format!("{:#}", start(None).unwrap_err()),
            "a tab labelled \"#1 x\" may be left in Ghostty: close it there: osascript, asked \
             to start a worker, exited 1: execution error: Ghostty got an error: no (-50)"
        );
    }

    #[test]
    fn a_close_is_by_the_handles_terminal_and_one_ghostty_no_longer_has_is_gone() {
        let (_dir, f, _db) = fakes();
        let (tab, split) = (
            Handle("ghostty-tab,T1".into()),
            Handle("ghostty-split,T2".into()),
        );
        assert_eq!(f.ghostty().close(&split).unwrap(), Closed::Now);
        assert_eq!(f.ghostty().close(&tab).unwrap(), Closed::Now);
        assert_eq!(f.calls(), ["close|split|T2", "close|tab|T1"]);
        // Ghostty has no such terminal: gone, and no failure.
        f.write("reply-close", "gone\n");
        assert_eq!(
            f.ghostty().close(&split).unwrap(),
            Closed::Gone("no such terminal in Ghostty (T2)".into())
        );
        assert_eq!(
            f.ghostty().close(&tab).unwrap(),
            Closed::Gone("no such terminal in Ghostty (T1)".into())
        );
        // A reply that is not its kind's: a split's with no pid and tty, or a pid that is
        // not one, and a tab's with them.
        for (handle, reply) in [
            (&tab, "what"),
            (&split, "closed"),
            (&split, "closed login /dev/ttys020"),
            (&split, "closed 41 /dev/ttys020 more"),
            (&tab, "closed 41 /dev/ttys020"),
        ] {
            f.write("reply-close", reply);
            assert_eq!(
                format!("{:#}", f.ghostty().close(handle).unwrap_err()),
                format!("osascript said something unexpected: {reply}")
            );
        }
    }

    #[test]
    fn osascript_runs_with_what_a_finder_launch_has_and_nothing_of_the_leads_session() {
        let pairs = |vars: &[(&str, &str)]| -> Vec<(String, String)> {
            (vars.iter())
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect()
        };
        let lead = pairs(&[
            ("HOME", "/Users/someone"),
            ("PATH", "/opt/homebrew/bin:/usr/bin"),
            ("CLAUDECODE", "1"),
            ("GIT_EDITOR", "true"),
            ("REX_SESSION", "session:7"),
            ("TOWER_DB", "/b/board.db"),
            ("LC_ALL", "en_US.UTF-8"),
            ("LCX", "no"),
            ("USER", "someone"),
            ("LOGNAME", "someone"),
            ("SHELL", "/bin/zsh"),
            ("TMPDIR", "/var/tmp/"),
            ("LANG", "en_US.UTF-8"),
            ("SSH_AUTH_SOCK", "/private/tmp/agent"),
            ("__CF_USER_TEXT_ENCODING", "0x1F5:0x0:0x0"),
            ("TERM", "xterm"),
        ]);
        // Each kept name as it was, and the system's PATH, never the lead's.
        assert_eq!(
            kept(lead.into_iter()),
            pairs(&[
                ("HOME", "/Users/someone"),
                ("LC_ALL", "en_US.UTF-8"),
                ("USER", "someone"),
                ("LOGNAME", "someone"),
                ("SHELL", "/bin/zsh"),
                ("TMPDIR", "/var/tmp/"),
                ("LANG", "en_US.UTF-8"),
                ("SSH_AUTH_SOCK", "/private/tmp/agent"),
                ("__CF_USER_TEXT_ENCODING", "0x1F5:0x0:0x0"),
                ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ])
        );
        assert_eq!(kept(std::iter::empty()), pairs(&[("PATH", SYSTEM_PATH)]));
        // What reaches osascript is that and no more, whatever this process holds (the
        // board `run_now` sets included): for the installed check, a start and a close.
        let (_dir, f, db) = fakes();
        let dev = words(&["claude", "-n", "dev-1", "Brief."]);
        let reached = ["HOME", "LC_ALL", "PATH"];
        f.ghostty().installed().unwrap();
        assert_eq!(f.env(), reached);
        (f.ghostty().start(&db, &worker("dev-1", &dev, None))).unwrap();
        assert_eq!(f.env(), reached);
        (f.ghostty().close(&Handle("ghostty-tab,T1".into()))).unwrap();
        assert_eq!(f.env(), reached);
    }

    /// A process of its own, with no parent here to wait for it, running `script` in a
    /// shell (30 s at most, as the scripts sleep); killed when dropped.
    struct Orphan(i32);

    impl Orphan {
        fn new(script: &str) -> Orphan {
            let out = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("({script}) </dev/null >/dev/null 2>&1 & echo $!"),
                ])
                .output()
                .unwrap();
            Orphan(
                String::from_utf8(out.stdout)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap(),
            )
        }

        /// Whether it still runs: one that ended and waits to be reaped does not.
        fn runs(&self) -> bool {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &self.0.to_string()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout);
            !stat.trim().is_empty() && !stat.trim().starts_with('Z')
        }
    }

    impl Drop for Orphan {
        fn drop(&mut self) {
            let _ = kill(Pid::from_raw(self.0), Signal::SIGKILL);
        }
    }

    const SLEEPS: &str = "exec sleep 30";
    const SPLIT: &str = "ghostty-split,T2";

    #[test]
    fn a_closed_splits_worker_that_ended_with_its_pane_is_closed_and_nothing_is_hung_up() {
        let (_dir, f, _db) = fakes();
        // The pane's wrapper ends as Ghostty closes it; another process is on its tty.
        let (wrapper, other) = (Orphan::new(SLEEPS), Orphan::new(SLEEPS));
        f.write(
            "live.txt",
            &format!("{} 1 ttys020\n{} 1 ttys020\n", wrapper.0, other.0),
        );
        f.write("kill-on-close", &wrapper.0.to_string());
        f.write("reply-close", &format!("closed {} /dev/ttys020", wrapper.0));
        let closed = f.ghostty().close(&Handle(SPLIT.into())).unwrap();
        assert_eq!(closed, Closed::Now);
        assert!(!wrapper.runs());
        assert!(other.runs());
    }

    #[test]
    fn a_closed_splits_worker_that_lingers_is_hung_up_and_no_other_process_is() {
        let (_dir, f, _db) = fakes();
        // The wrapper and the worker under it outlive the close, as Ghostty leaves a
        // split's. Beside them: a process under the worker on another tty, and one on the
        // pane's tty that is not under the wrapper.
        let (wrapper, worker) = (Orphan::new(SLEEPS), Orphan::new(SLEEPS));
        let (elsewhere, stranger) = (Orphan::new(SLEEPS), Orphan::new(SLEEPS));
        f.write(
            "live.txt",
            &format!(
                "{w} 1 ttys020\n{c} {w} ttys020\n{e} {c} ttys077\n{s} 1 ttys020\n",
                w = wrapper.0,
                c = worker.0,
                e = elsewhere.0,
                s = stranger.0
            ),
        );
        f.write("reply-close", &format!("closed {} /dev/ttys020", wrapper.0));
        let closed = f.ghostty().close(&Handle(SPLIT.into())).unwrap();
        assert_eq!(closed, Closed::Now);
        assert!(!wrapper.runs() && !worker.runs());
        assert!(elsewhere.runs() && stranger.runs());
    }

    #[test]
    fn a_wrapper_pid_that_is_not_on_the_panes_tty_is_never_signalled_nor_what_is_under_it() {
        let (_dir, f, _db) = fakes();
        // The pid Ghostty gave is a process on another tty by now, with one under it on
        // the pane's tty.
        let (reused, under) = (Orphan::new(SLEEPS), Orphan::new(SLEEPS));
        f.write(
            "live.txt",
            &format!(
                "{p} 1 ttys099\n{u} {p} ttys020\n",
                p = reused.0,
                u = under.0
            ),
        );
        f.write("reply-close", &format!("closed {} /dev/ttys020", reused.0));
        let closed = f.ghostty().close(&Handle(SPLIT.into())).unwrap();
        assert_eq!(closed, Closed::Now);
        assert!(reused.runs() && under.runs());
    }

    #[test]
    fn a_worker_that_outlives_the_hangup_fails_the_close_naming_it_and_so_does_no_ps() {
        let (dir, f, _db) = fakes();
        let deaf = Orphan::new("trap '' HUP; exec sleep 30");
        f.write("live.txt", &format!("{} 1 ttys020\n", deaf.0));
        f.write("reply-close", &format!("closed {} /dev/ttys020", deaf.0));
        let ghostty = Ghostty {
            hung_up_within: Duration::from_secs(1),
            ..f.ghostty()
        };
        assert_eq!(
            format!("{:#}", ghostty.close(&Handle(SPLIT.into())).unwrap_err()),
            format!(
                "its pane is closed, but its worker still runs 1 s after a hangup, on \
                 /dev/ttys020: end it with: kill {}",
                deaf.0
            )
        );
        assert!(deaf.runs());
        // Whether it ended cannot be told without ps.
        std::fs::remove_file(dir.path().join("ps.txt")).unwrap();
        let why = format!("{:#}", ghostty.close(&Handle(SPLIT.into())).unwrap_err());
        assert!(
            why.starts_with(&format!(
                "its pane is closed, but whether its worker ended is not known, on \
                     /dev/ttys020 under pid {}: ps exited 1: ",
                deaf.0
            )),
            "{why}"
        );
    }

    #[test]
    fn a_terminals_processes_are_its_wrapper_and_what_is_under_it_on_its_tty() {
        let table = "    1     0 ??\n   50     1 ttys020\n   60    50 ttys020\n   70    60 \
                     ttys020\n   80    60 ttys077\n   85    80 ttys020\n   90     1 \
                     ttys020\n   95    90 ttys020\n";
        let tty = "/dev/ttys020";
        // The wrapper, the worker under it and what the worker runs there, whatever is
        // between them; not what is on another tty, nor another terminal's on this one.
        assert_eq!(on_terminal(table, 50, tty), [50, 60, 70, 85]);
        assert_eq!(on_terminal(table, 90, tty), [90, 95]);
        // A wrapper on another tty, or one ps does not list: none, even what is under it.
        assert_eq!(on_terminal(table, 80, tty), [] as [i32; 0]);
        assert_eq!(on_terminal(table, 999, tty), [] as [i32; 0]);
        assert_eq!(on_terminal(table, 50, "/dev/ttys077"), [] as [i32; 0]);
        // Never the first process, nor none.
        let first = "    1     0 ttys020\n   50     1 ttys020\n    0     0 ttys020\n";
        assert_eq!(on_terminal(first, 1, tty), [] as [i32; 0]);
        assert_eq!(on_terminal(first, 0, tty), [] as [i32; 0]);
        // A row whose pid no signal could take, or that would name more than one process
        // (as a u32 the first would read as -1, every process), is no process, though it is
        // on the tty and under the wrapper; nor is what is under it.
        let odd = "50 1 ttys020\n4294967295 50 ttys020\n2147483648 50 ttys020\n-1 50 \
                   ttys020\n-60 50 ttys020\n0 50 ttys020\n1 50 ttys020\n70 -1 ttys020\n60 50 \
                   ttys020\n";
        assert_eq!(on_terminal(odd, 50, tty), [50, 60]);
        assert_eq!(on_terminal(odd, -1, tty), [] as [i32; 0]);
        assert_eq!(tty_of("4294967295 1 ttys020\n-1 1 ttys020\n", -1), None);
        // A table that loops ends.
        let loops = "50 60 ttys020\n60 50 ttys020\n70 71 ttys020\n71 70 ttys020\n";
        assert_eq!(on_terminal(loops, 50, tty), [50, 60]);
    }

    #[test]
    fn a_handle_no_start_in_ghostty_printed_is_refused_and_osascript_never_run() {
        let (_dir, f, db) = fakes();
        let dev = words(&["claude", "-n", "dev-1", "Brief."]);
        for bad in [
            "",
            "T1",
            "ghostty-tab",
            "ghostty-tab,",
            "ghostty-tab,-e",
            "ghostty-tab,T1,T2",
            "ghostty-pane,T1",
            "rex-tab,session:7,window:w1,block:b1",
        ] {
            let bad = Handle(bad.into());
            let said = format!(
                "{:?} is not a handle tower worker start printed in Ghostty",
                bad.0
            );
            assert_eq!(format!("{:#}", f.ghostty().close(&bad).unwrap_err()), said);
            let went = f.ghostty().start(&db, &worker("x", &dev, Some(&bad)));
            assert_eq!(format!("{:#}", went.unwrap_err()), said);
        }
        assert_eq!(f.calls(), [] as [String; 0]);
    }

    #[test]
    fn ghostty_not_answering_says_to_approve_automation_for_ghostty() {
        let approve = "approve Automation for Ghostty (System Settings, Privacy & Security, \
                       Automation: let the app this session runs in control Ghostty), then run \
                       this again.";
        let timed_out = "execution error: Ghostty got an error: AppleEvent timed out. (-1712)";
        let refused = "execution error: Not authorized to send Apple events to Ghostty. (-1743)";
        for stderr in [timed_out, refused] {
            assert_eq!(
                format!("{:#}", said("close a worker", exited(stderr)).unwrap_err()),
                format!(
                    "Ghostty did not answer when asked to close a worker (each event has 10 s): \
                     {approve} osascript exited 1: {stderr}"
                )
            );
        }
        // The scripts give each event the time the message names.
        for script in [START, CLOSE] {
            assert!(script.contains(&format!("with timeout of {EVENT_SECS} seconds")));
        }
        // osascript itself stopped at its time limit.
        let stopped = Outcome::TimedOut {
            after: Duration::from_secs(15),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert_eq!(
            format!("{:#}", said("start a worker", stopped).unwrap_err()),
            format!(
                "Ghostty did not answer when asked to start a worker (each event has 10 s): \
                 {approve} osascript did not finish in 15 s and was stopped"
            )
        );
        // Through a start and a close, with what a start may have left.
        let (_dir, f, db) = fakes();
        let dev = words(&["claude", "-n", "dev-1", "Brief."]);
        f.write("fail-start", timed_out);
        f.write("fail-close", timed_out);
        let why = format!(
            "{:#}",
            (f.ghostty().start(&db, &worker("dev-1", &dev, None))).unwrap_err()
        );
        assert!(
            why.starts_with(
                "a tab labelled \"dev-1\" may be left in Ghostty: close it there: Ghostty did \
                 not answer when asked to start a worker"
            ) && why.contains(approve),
            "{why}"
        );
        let why = format!(
            "{:#}",
            (f.ghostty().close(&Handle("ghostty-tab,T1".into()))).unwrap_err()
        );
        assert!(why.contains(approve), "{why}");
        // Any other error is not Automation's, and neither is an osascript that cannot run.
        let other = said("close a worker", exited("execution error: no (-50)")).unwrap_err();
        assert_eq!(
            other.to_string(),
            "osascript, asked to close a worker, exited 1: execution error: no (-50)"
        );
        let cannot = said("close a worker", Outcome::Failed("cannot run it".into())).unwrap_err();
        assert_eq!(
            cannot.to_string(),
            "osascript, asked to close a worker, cannot run it"
        );
    }

    #[test]
    fn ghostty_is_installed_when_osascript_knows_its_bundle_id() {
        let (dir, f, _db) = fakes();
        assert_eq!(f.ghostty().installed(), Ok(()));
        assert_eq!(f.calls(), ["installed|"]);
        // As osascript says it of an application macOS does not have.
        f.write(
            "fail-installed",
            "0:44: syntax error: Can\u{2019}t get application id \"com.mitchellh.ghostty\". \
             (-1728)\n",
        );
        assert_eq!(
            f.ghostty().installed(),
            Err("Ghostty is not installed".into())
        );
        // Anything else is not known, saying what osascript said; so is no osascript.
        f.write("fail-installed", "no user interaction allowed (-1713)\n");
        assert_eq!(
            f.ghostty().installed(),
            Err(
                "whether Ghostty is installed is not known (exited 1: no user interaction \
                 allowed (-1713))"
                    .into()
            )
        );
        let none = Ghostty {
            osascript: dir.path().join("no-osascript"),
            ..f.ghostty()
        };
        let why = none.installed().unwrap_err();
        assert!(
            why.starts_with("whether Ghostty is installed is not known (exited 127: "),
            "{why}"
        );
    }
}
