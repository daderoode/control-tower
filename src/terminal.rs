//! The terminal module: the one place the core (the watch's look, the delivery round, the
//! dashboard's `n`) reaches the terminal its workers run in. The core asks it for the live
//! workers (`agents`), has it deliver a text to one by name (`prompt`), and has it open a
//! new place for a lead (`start_lead`, undone by `undo`), and serves the dashboard's keys
//! (`space`, `popup`); its display name names the terminal in the core's texts and logs. A
//! module is built in: `module` gives the one this build has, Rex (`rex`), so another slots
//! in here without touching the core. Rex's prompt reaches a worker's Claude session
//! through the relay (`relay`).
//!
//! Status words are the Program Status Protocol's (idle, working, done, blocked, and the
//! blocked kind where the terminal gives it). A value the core does not know is
//! `Status::Unknown`: present, with nothing to judge by.

pub mod relay;
pub mod rex;

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

/// What a worker waits on while blocked, where the terminal says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    /// The kind a terminal names, `None` for one this build does not know.
    pub fn of(s: &str) -> Option<Kind> {
        match s {
            "permission" => Some(Kind::Permission),
            "question" => Some(Kind::Question),
            "auth" => Some(Kind::Auth),
            _ => None,
        }
    }
}

/// A worker's state, in the Program Status words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Working,
    /// Waiting on a prompt, of this kind when the terminal gives it.
    Blocked(Option<Kind>),
    Done,
    /// `unknown`, or a state this build does not know: present, but nothing to judge by.
    Unknown,
}

impl Status {
    /// The status a terminal names as `state`, blocked on `kind` when given.
    pub fn of(state: &str, kind: Option<&str>) -> Status {
        match state {
            "idle" => Status::Idle,
            "working" => Status::Working,
            "blocked" => Status::Blocked(kind.and_then(Kind::of)),
            "done" => Status::Done,
            _ => Status::Unknown,
        }
    }

    pub fn is_blocked(self) -> bool {
        matches!(self, Status::Blocked(_))
    }
}

/// A live, named worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub status: Status,
    /// The one line the terminal shows with its status, where it gives one: for a blocked
    /// worker, what it is asked. Text from the worker's program, shown as plain text.
    pub msg: Option<String>,
}

/// Why the terminal's workers could not be listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// The terminal is not running: its own answer, so a look is skipped and says so.
    NotRunning(String),
    /// It could not be read, for this reason.
    Failed(String),
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unread::NotRunning(why) | Unread::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Unread {}

/// How a send went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    Delivered,
    /// It may have arrived (the terminal timed out or saw no activity): logged, never
    /// resent.
    Unsure(String),
    /// It did not arrive, for this reason.
    Failed(String),
    /// It went to someone else, as this says, naming who got it: worse than a lost send, so
    /// always told to the owner, and never resent.
    Misrouted(String),
}

/// What a prompt did: how the send went, and the one line the module has on how it made it
/// (for the relay: who, the text, its exit, duration, cost and answer), for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompted {
    pub sent: Sent,
    pub trace: Option<String>,
}

impl From<Sent> for Prompted {
    /// A send with nothing more to log.
    fn from(sent: Sent) -> Prompted {
        Prompted { sent, trace: None }
    }
}

/// How long one prompt may take, the module's look for the worker and its send together. A
/// relay send takes about 6 to 10 s; a round of delivery starts no send after its budget,
/// but never cuts one short (`deliver::WORKER_BUDGET`).
pub const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// A lead to start: its place named `name`, opened in `dir`, with the board's absolute path
/// `board` set as `TOWER_DB` there, running `command` (one word per argument, never a
/// shell string).
#[derive(Debug, Clone, Copy)]
pub struct LeadStart<'a> {
    pub name: &'a str,
    pub dir: &'a str,
    pub board: &'a str,
    pub command: &'a [String],
}

/// A started lead's place, as its module names it: what `Terminal::undo` closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle(pub String);

/// Where a key was pressed: its session and block, as the terminal names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    pub session: String,
    pub block: String,
}

/// A place the terminal can show: a session and one of its windows (a tab). It is also
/// what the back file beside the board holds, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub session_id: String,
    pub window_id: String,
}

/// What the dashboard's key is to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// This place.
    Go(Place),
    /// Nothing moves, for this reason.
    Stay(String),
}

/// What the dashboard's key found: what to show, and each block or session it passed over
/// because the terminal could not read it (one closing as it looked), saying why, for the
/// log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub shown: Shown,
    pub passed_over: Vec<String>,
}

/// The terminal module interface. `db` is the board, passed to what the module runs as
/// `TOWER_DB`.
pub trait Terminal: Send + Sync {
    /// The terminal's name in texts and logs.
    fn name(&self) -> &'static str;
    /// Its executable, as the module was given it.
    fn exe(&self) -> &Path;
    /// The one line saying the executable cannot be found on `path` (a PATH value),
    /// naming what was tried and how to name it; `None` when it is found. Whoever says it
    /// adds how often (the watch: once, until it is found).
    fn missing(&self, path: Option<&OsStr>) -> Option<String>;
    /// The live named workers, within `timeout`.
    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread>;
    /// Delivers `text` to the worker `name` (ignoring case), within `timeout`. A worker at a
    /// prompt is sent nothing.
    fn prompt(&self, db: &Path, name: &str, text: &str, timeout: Duration) -> Prompted;
    /// Opens a new place for a lead without taking focus and starts it there.
    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle>;
    /// Undoes a start: closes the place `handle` names. Says what it did.
    fn undo(&self, db: &Path, handle: &Handle) -> Result<String>;
    /// The dashboard's key, pressed `here`: from the dashboard's tab, the place the key was
    /// last pressed outside it, as `back` (a file) holds it; from anywhere else, the
    /// dashboard's tab, opening one running `dashboard` (one word per argument) when there
    /// is none, with `here` written to `back` (its folder made if need be). Another block or
    /// session that cannot be read is passed over; the key fails when `here` cannot be.
    fn space(&self, db: &Path, here: &Here, back: &Path, dashboard: &[String]) -> Result<Answer>;
    /// The popup's key, pressed `here`: opens `dashboard` in a popup over `here`'s tab.
    fn popup(&self, db: &Path, here: &Here, dashboard: &[String]) -> Result<()>;
}

/// The built-in terminal module, Rex, its executable at `exe`, asking the `claude` the
/// environment names (`register::claude_path`) for the names of the Claude sessions it finds.
pub fn module(exe: PathBuf) -> Arc<dyn Terminal> {
    Arc::new(rex::Rex::new(exe, crate::register::claude_path()))
}

/// The built-in module's executable as the environment names it.
pub fn exe_from_env() -> PathBuf {
    rex::path()
}

/// What `tower lead attach` and the SessionStart hook tell a lead whose session runs outside
/// the built-in module's terminal, by this process's environment; `None` inside it.
pub fn outside_from_env() -> Option<String> {
    rex::outside(|name| std::env::var_os(name))
}

/// What the dashboard in a tab of its own writes to its terminal, by this process's
/// environment, so the dashboard's key can find it: (on start, on exit), or `None` where
/// the built-in module's terminal is not the one it runs in.
pub fn dashboard_status_from_env() -> Option<(&'static str, &'static str)> {
    rex::dashboard_status(|name| std::env::var_os(name))
}

/// How long one terminal command may take before it is stopped, unless a caller has its
/// own limit (`terminal_timeout`).
#[cfg(not(test))]
const TERMINAL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one terminal command may take before it is stopped: `TERMINAL_TIMEOUT`, but in
/// unit tests `TEST_WAIT`. A fake terminal answers in milliseconds, but its first run can
/// stall for seconds on a busy machine, and those tests check what it answered, not how
/// fast.
#[cfg(test)]
pub(crate) fn terminal_timeout() -> Duration {
    crate::watch::TEST_WAIT
}

/// How long one terminal command may take before it is stopped: `TERMINAL_TIMEOUT`.
///
/// Test knob, debug builds only: `TOWER_TERMINAL_TIMEOUT`, in whole seconds, replaces it,
/// so the integration tests, which run this binary against a fake terminal whose first run
/// can stall on a busy machine, wait longer; the repo's `.cargo/config.toml` sets it. A
/// value that is not whole seconds panics, naming it. A release build (brew) never reads it.
#[cfg(not(test))]
pub(crate) fn terminal_timeout() -> Duration {
    if cfg!(debug_assertions)
        && let Some(v) = std::env::var_os("TOWER_TERMINAL_TIMEOUT")
    {
        let secs = (v.to_str().and_then(|v| v.parse().ok()))
            .unwrap_or_else(|| panic!("TOWER_TERMINAL_TIMEOUT must be whole seconds, not {v:?}"));
        return Duration::from_secs(secs);
    }
    TERMINAL_TIMEOUT
}

/// Where an executable (rex, claude) runs from: a name with a `/` as it is, else the
/// first executable file by that name in `path` (a PATH value). `None` when there is none.
pub fn find_exe(exe: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if exe.components().count() > 1 {
        return runnable(exe).then(|| exe.to_path_buf());
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(exe))
        .find(|p| runnable(p))
}

/// "`name` not found: tried ...", naming exactly what `find_exe` tried for `exe`.
pub(crate) fn not_found(name: &str, exe: &Path, path: Option<&OsStr>) -> String {
    let tried = if exe.components().count() > 1 {
        exe.display().to_string()
    } else {
        let path = path.map_or("(no PATH)".into(), |p| p.to_string_lossy().into_owned());
        format!("{} on PATH {path}", exe.display())
    };
    format!("{name} not found: tried {tried}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn finds_an_executable_by_path_or_on_path_in_order_and_says_what_it_tried() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let exe = first.join("rex");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        let later = second.join("rex");
        crate::test_exe::executable(&later, "#!/bin/sh\n");
        let path = std::env::join_paths([
            "/nowhere",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
        ])
        .unwrap();
        // The first is not executable yet: skipped for the next on the PATH.
        assert_eq!(find_exe(Path::new("rex"), Some(&path)), Some(later.clone()));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Both are: the PATH's order decides.
        assert_eq!(find_exe(Path::new("rex"), Some(&path)), Some(exe.clone()));
        // A path with a / as it is, PATH or not.
        assert_eq!(find_exe(&exe, None), Some(exe.clone()));
        // A bare name and no PATH: none.
        assert_eq!(find_exe(Path::new("rex"), None), None);
        // A missing path with a /: none, even with that name on the PATH.
        let gone = dir.path().join("gone/rex");
        assert_eq!(find_exe(&gone, Some(&path)), None);
        // What was tried, as the not-found lines say it.
        assert_eq!(
            not_found("rex", Path::new("rex"), Some(OsStr::new("/a:/b"))),
            "rex not found: tried rex on PATH /a:/b"
        );
        assert_eq!(
            not_found("rex", Path::new("rex"), None),
            "rex not found: tried rex on PATH (no PATH)"
        );
        assert_eq!(
            not_found("rex", &gone, None),
            format!("rex not found: tried {}", gone.display())
        );
    }
}
