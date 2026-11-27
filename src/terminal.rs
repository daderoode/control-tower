//! The terminal module interface: the one place the core (the watch's look, the delivery
//! round, the dashboard's `n`) reaches the terminal its workers run in. The core asks it for
//! the live workers (`agents`), has it deliver a text to one by name (`prompt`), and has it
//! open a new place for a lead (`start_lead`, undone by `undo`), and serves the dashboard's
//! keys (`space`, `popup`); its display name names the terminal in the core's texts and logs.
//! `tower worker start` and `tower worker close` (`crate::worker`) reach it too: a worker
//! starts in a tab of its own or as a split beside another (`start_worker`), is closed by the
//! handle its start gave (`close_worker`), and its pane is read where the terminal can
//! (`shows`). The terminal those two commands use is `for_workers_from_env`'s.
//!
//! The core has its own version of what it can do with no terminal, as the trait's defaults:
//! the live workers are Claude Code's session list (`claude agents --json`, read by
//! `listed`), and a prompt is a fresh look for the worker by name, nothing sent to one at a
//! prompt, then the relay (`relay`) to its Claude session. The core's start and close of a
//! worker are Ghostty's (`ghostty`), a tab or a split there driven through AppleScript, from
//! whatever terminal the lead runs in; with no Ghostty they say workers need Ghostty or
//! Rex, and the core reads no pane (`shows`). A module
//! overrides any of them with its own; what it does not override runs the core's. `Core`
//! is the core with no module: every default, and what only a terminal can do (starting a
//! lead, the dashboard's keys) fails saying so. A module is built in: `module` gives the
//! one this build has, Rex (`rex`), whose `agents` adds the text of the prompt a worker
//! waits at; another slots in here without touching the core.
//!
//! Status words are the Program Status Protocol's (idle, working, done, blocked, and the
//! blocked kind where the terminal gives it). Claude's list has its own (`Status::listed`):
//! `busy` is working, `idle` idle, `waiting` blocked, its kind from `waitingFor`. A value
//! the core does not know is `Status::Unknown`: present, with nothing to judge by.

pub mod ghostty;
pub mod relay;
pub mod rex;

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

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

    /// The kind Claude's list names as `waitingFor`, `None` for one this build does not
    /// know.
    pub fn listed(waiting_for: &str) -> Option<Kind> {
        match waiting_for {
            "permission prompt" => Some(Kind::Permission),
            "input needed" => Some(Kind::Question),
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

    /// The status Claude's list names as `status`, waiting for `waiting_for` when given:
    /// `busy`, `idle`, `waiting`; anything else, or none, is `Unknown`.
    pub fn listed(status: Option<&str>, waiting_for: Option<&str>) -> Status {
        match status {
            Some("busy") => Status::Working,
            Some("idle") => Status::Idle,
            Some("waiting") => Status::Blocked(waiting_for.and_then(Kind::listed)),
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
/// `cause` is the error a send that failed shares with every other send it stops: for the
/// relay, the result its own Claude run ended on when that run ended on an error and sent
/// nothing (a provider outage, say), never a refusal or anything about the recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompted {
    pub sent: Sent,
    pub trace: Option<String>,
    pub cause: Option<String>,
}

impl From<Sent> for Prompted {
    /// A send with nothing more to log, and no cause it shares.
    fn from(sent: Sent) -> Prompted {
        Prompted {
            sent,
            trace: None,
            cause: None,
        }
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

/// A started lead's place, as its module names it: what `Terminal::undo` closes. Also a
/// started worker's: what `tower worker start` prints, `Terminal::close_worker` closes and
/// another worker is split beside. Only the module that wrote one reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle(pub String);

/// A worker to start, without taking focus, in the absolute folder `dir`, running `command`
/// (one word per argument, never a shell string): in a tab of its own labelled `label`, or,
/// with `beside`, as a split to the right of the pane that handle names (its `label` is then
/// not used).
#[derive(Debug, Clone, Copy)]
pub struct WorkerStart<'a> {
    pub label: &'a str,
    pub dir: &'a str,
    pub command: &'a [String],
    pub beside: Option<&'a Handle>,
}

/// How a close of a worker went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    /// It was there and is closed now.
    Now,
    /// The terminal has nothing by that handle, so nothing was closed: what it did not
    /// find, like "no such tab or pane in session:7". It was closed already, or the handle
    /// was never a start's (a typo), which the terminal cannot tell apart.
    Gone(String),
}

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

/// A terminal as a `Terminal` object: what `Terminal::current`'s default returns, given to
/// every terminal.
pub trait AsTerminal {
    fn as_terminal(&self) -> &dyn Terminal;
}

impl<T: Terminal> AsTerminal for T {
    fn as_terminal(&self) -> &dyn Terminal {
        self
    }
}

/// The terminal module interface. `db` is the board, passed to what the module runs as
/// `TOWER_DB`.
pub trait Terminal: AsTerminal + Send + Sync {
    /// The terminal to ask for one piece of work that must ask a single one throughout (a
    /// delivery round: its list, its texts, its sends). The core's: itself.
    fn current(&self) -> &dyn Terminal {
        self.as_terminal()
    }
    /// The terminal's name in texts and logs.
    fn name(&self) -> &'static str;
    /// Its executable, as the module was given it.
    fn exe(&self) -> &Path;
    /// The one line saying the executable cannot be found on `path` (a PATH value),
    /// naming what was tried and how to name it; `None` when it is found. Whoever says it
    /// adds how often (the watch: once, until it is found).
    fn missing(&self, path: Option<&OsStr>) -> Option<String>;
    /// What the module adds to the core, for the line the watch says when it goes missing
    /// ("the text of ..., and ..."). The core's: nothing.
    fn adds(&self) -> Option<&'static str> {
        None
    }
    /// The one line saying the terminal, found, does not answer within `timeout` (it cannot
    /// list its sessions, for the reason given); `None` when it answers. Whoever says it
    /// adds how often (the watch: once, until it answers). The core's: it always answers,
    /// since its list is asked fresh each look.
    fn silent(&self, _db: &Path, _timeout: Duration) -> Option<String> {
        None
    }
    /// The Claude Code it asks (`register::claude_path`): for the core's list and the relay.
    fn claude(&self) -> &Path;
    /// The live named workers, within `timeout`. The core's: each session Claude's list
    /// names (`listed`), with its status (`Status::listed`) and no `msg`; one with no name
    /// is no one.
    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread> {
        Ok((listed(self.claude(), db, timeout)?.into_iter())
            .filter_map(|l| {
                let status = Status::listed(l.status.as_deref(), l.waiting_for.as_deref());
                Some(Agent {
                    name: l.name?,
                    status,
                    msg: None,
                })
            })
            .collect())
    }
    /// Delivers `text` to the worker `name` (ignoring case), within `timeout`. The core's: a
    /// fresh look for it (`agents`, the module's when it has its own); one it has no agent
    /// by, or one at a prompt (any of its sessions blocked), is sent nothing and the send
    /// fails saying why; else the relay sends the text to its Claude session
    /// (`relay::send`), within what is left of the time limit.
    fn prompt(&self, db: &Path, name: &str, text: &str, timeout: Duration) -> Prompted {
        let deadline = Instant::now() + timeout;
        let agents = match self.agents(db, timeout.min(terminal_timeout())) {
            Ok(agents) => agents,
            Err(e) => return Sent::Failed(e.to_string()).into(),
        };
        let theirs: Vec<&Agent> = (agents.iter())
            .filter(|a| a.name.eq_ignore_ascii_case(name))
            .collect();
        let Some(first) = theirs.first() else {
            return Sent::Failed(format!("{} has no agent named {name}", self.name())).into();
        };
        if let Some(blocked) = theirs.iter().find(|a| a.status.is_blocked()) {
            return Sent::Failed(at_prompt(blocked)).into();
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let why = format!("no time was left to send within {} s", timeout.as_secs());
            return Sent::Failed(why).into();
        }
        relay::send(self.claude(), db, &first.name, text, left)
    }
    /// Opens a new place for a lead without taking focus and starts it there.
    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle>;
    /// Undoes a start: closes the place `handle` names. Says what it did.
    fn undo(&self, db: &Path, handle: &Handle) -> Result<String>;
    /// The Ghostty the core's start and close of a worker use. The core's: the one this
    /// process's environment names (`ghostty::Ghostty::from_env`).
    fn ghostty(&self) -> ghostty::Ghostty {
        ghostty::Ghostty::from_env()
    }
    /// Whether this terminal starts workers at all, asked before anything else a start
    /// does (`crate::worker::start`), so one that cannot says so first. The core's: it
    /// does when Ghostty is installed, and else says workers need Ghostty or Rex.
    fn starts_workers(&self) -> Result<()> {
        (self.ghostty().installed()).map_err(|why| needs_ghostty("starting a worker", &why))
    }
    /// Starts a worker (`tower worker start`): a tab of its own, or a split beside another
    /// worker's pane, without taking focus. The handle names what it made, for
    /// `close_worker`, `shows` and a later split beside it. The core's: Ghostty's
    /// (`ghostty::Ghostty::start`), a tab in the lead's Ghostty window or a split there,
    /// with the lead's tab selected again since Ghostty starts nothing without focus; what
    /// failed after the worker's terminal was made is said on stderr, beside the handle.
    fn start_worker(&self, db: &Path, worker: &WorkerStart) -> Result<Handle> {
        let started = self.ghostty().start(db, worker)?;
        for note in &started.notes {
            eprintln!("tower: {note}");
        }
        Ok(started.handle)
    }
    /// Closes what `handle` names (`tower worker close`): a tab with every pane in it, or
    /// one split. One the terminal does not have is `Closed::Gone`, not an error. The
    /// core's: Ghostty's (`ghostty::Ghostty::close`), and with no Ghostty it says workers
    /// need Ghostty or Rex.
    fn close_worker(&self, _db: &Path, handle: &Handle) -> Result<Closed> {
        let ghostty = self.ghostty();
        (ghostty.installed()).map_err(|why| needs_ghostty("closing a worker", &why))?;
        ghostty.close(handle)
    }
    /// What the pane `handle` names shows now, its last lines, where the terminal can read
    /// it: for the line saying a started worker is not in Claude's list. The core's: none.
    fn shows(&self, _db: &Path, _handle: &Handle) -> Option<String> {
        None
    }
    /// The dashboard's key, pressed `here`, for the board `db`'s dashboard only: from a tab
    /// holding one, the place the key was last pressed outside it, as `back` (a file) holds
    /// it; from anywhere else, the dashboard's tab, opening one running `dashboard` (one word
    /// per argument) when there is none, with `here` written to `back` (its folder made if
    /// need be). Another block or session that cannot be read is passed over; the key fails
    /// when `here` cannot be, or the terminal does not answer in time.
    fn space(&self, db: &Path, here: &Here, back: &Path, dashboard: &[String]) -> Result<Answer>;
    /// The popup's key, pressed `here`: opens `dashboard` in a popup over `here`'s tab.
    fn popup(&self, db: &Path, here: &Here, dashboard: &[String]) -> Result<()>;
}

/// A Claude session as `claude agents --json` lists it, as far as control-tower reads it.
/// Each entry stands alone: a field of another type is none (`text`, `integer`), so an
/// odd entry is a session with nothing to know it by (no name, no one), and a `status` or
/// `waitingFor` of another type a value the core does not know, never a list that cannot
/// be read.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Listed {
    #[serde(default, deserialize_with = "text")]
    pub name: Option<String>,
    #[serde(default, rename = "sessionId", deserialize_with = "text")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "integer")]
    pub pid: Option<i64>,
    #[serde(default, deserialize_with = "text")]
    pub status: Option<String>,
    #[serde(default, rename = "waitingFor", deserialize_with = "text")]
    pub waiting_for: Option<String>,
}

/// A JSON string as itself; any other value, null included, as none.
fn text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    use serde::Deserialize;
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
}

/// A JSON integer (within an i64) as itself; any other value, null included, as none.
fn integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    use serde::Deserialize;
    Ok(serde_json::Value::deserialize(d)?.as_i64())
}

/// Claude Code's session list: `claude agents --json`, run from `claude` within `timeout`.
pub(crate) fn listed(claude: &Path, db: &Path, timeout: Duration) -> Result<Vec<Listed>, Unread> {
    let not_stopped = AtomicBool::new(false);
    let args = ["agents", "--json"];
    let json = crate::register::claude_within(claude, db, &not_stopped, &args, timeout)
        .map_err(|e| Unread::Failed(format!("{e:#}")))?;
    parse(&json).map_err(Unread::Failed)
}

/// `claude agents --json`'s list (`json`), the one reader of it: the core's workers, Rex,
/// the lead check, a lead's attach and delivery. One odd entry stands alone (`Listed`), but
/// a field that some entry gives (not null) and none gives as the type Claude lists, a
/// string for `name` and `sessionId`, an integer for `pid`, is Claude's format changed:
/// the list cannot be read, never a list of sessions with nothing to know them by.
pub(crate) fn parse(json: &str) -> Result<Vec<Listed>, String> {
    let unexpected =
        |e: &dyn fmt::Display| format!("claude agents --json said something unexpected: {e}");
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| unexpected(&e))?;
    for (field, what) in [
        ("name", "a string"),
        ("sessionId", "a string"),
        ("pid", "an integer"),
    ] {
        let listed = |v: &serde_json::Value| match field {
            "pid" => v.is_i64(),
            _ => v.is_string(),
        };
        let given = || (entries.iter()).filter_map(|e| e.get(field).filter(|v| !v.is_null()));
        if given().next().is_some() && !given().any(listed) {
            return Err(unexpected(&format_args!("no session's {field} is {what}")));
        }
    }
    (entries.into_iter())
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|e| unexpected(&e))
}

/// Why a worker at a prompt is sent nothing: "at a permission prompt (approve Bash: ...)",
/// naming the kind and what it asks where the terminal says.
fn at_prompt(agent: &Agent) -> String {
    let what = match agent.status {
        Status::Blocked(Some(Kind::Permission)) => "a permission prompt",
        Status::Blocked(Some(Kind::Question)) => "a question",
        Status::Blocked(Some(Kind::Auth)) => "a credential prompt",
        _ => "a prompt",
    };
    let asks = (agent.msg.as_deref()).map_or(String::new(), |m| format!(" ({m})"));
    format!("at {what}{asks}")
}

/// The core with no terminal module, asking the `claude` at `claude`: the live workers and
/// prompts are the core's (the trait's defaults), and so are a worker's start and close, in
/// `ghostty`; starting a lead and the dashboard's keys need a terminal, so they fail saying
/// so. Its name in texts is Claude Code's, whose list it reads.
pub struct Core {
    claude: PathBuf,
    ghostty: ghostty::Ghostty,
}

impl Core {
    /// The core over the Ghostty this process's environment names.
    pub fn new(claude: PathBuf) -> Core {
        Core::over(claude, ghostty::Ghostty::from_env())
    }

    /// The core starting and closing its workers in `ghostty`.
    pub fn over(claude: PathBuf, ghostty: ghostty::Ghostty) -> Core {
        Core { claude, ghostty }
    }
}

/// What `Core` says for `what`, which only a terminal module can do.
fn needs_module(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what} needs a terminal module (Rex), and there is none")
}

/// What the core says for `what` (starting or closing a worker) when Ghostty cannot do it,
/// for the reason `why` (`ghostty::Ghostty::installed`): only Rex could, and the core is
/// asked only outside Rex (`for_workers_from_env`).
fn needs_ghostty(what: &str, why: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{what} needs Ghostty or Rex: {why}, and this session is not in Rex (REX_SESSION is \
         not set)"
    )
}

/// Whether `id` is a word a worker's handle keeps as one of its terminal's ids: not empty,
/// no comma (the handle's separator), and no leading dash, which a command would read as a
/// flag.
pub(crate) fn is_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(',') && !id.starts_with('-')
}

impl Terminal for Core {
    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn exe(&self) -> &Path {
        &self.claude
    }

    fn missing(&self, path: Option<&OsStr>) -> Option<String> {
        find_exe(&self.claude, path)
            .is_none()
            .then(|| not_found("claude", &self.claude, path))
    }

    fn claude(&self) -> &Path {
        &self.claude
    }

    fn ghostty(&self) -> ghostty::Ghostty {
        self.ghostty.clone()
    }

    fn start_lead(&self, _db: &Path, _lead: &LeadStart) -> Result<Handle> {
        Err(needs_module("starting a lead"))
    }

    fn undo(&self, _db: &Path, _handle: &Handle) -> Result<String> {
        Err(needs_module("closing a lead's place"))
    }

    fn space(&self, _db: &Path, _here: &Here, _back: &Path, _dash: &[String]) -> Result<Answer> {
        Err(needs_module("the dashboard's key"))
    }

    fn popup(&self, _db: &Path, _here: &Here, _dashboard: &[String]) -> Result<()> {
        Err(needs_module("the popup's key"))
    }
}

/// The built-in terminal module, Rex, its executable at `exe`, asking the `claude` the
/// environment names (`register::claude_path`) for the names of the Claude sessions it finds.
pub fn module(exe: PathBuf) -> Arc<dyn Terminal> {
    Arc::new(rex::Rex::new(exe, crate::register::claude_path()))
}

/// The terminal `tower worker start` and `tower worker close` use, by this process's
/// environment: Rex's when the caller runs in Rex (`rex::for_workers`), else the core, whose
/// workers are Ghostty's.
pub fn for_workers_from_env() -> Arc<dyn Terminal> {
    match rex::for_workers(|name| std::env::var_os(name)) {
        Some(rex) => Arc::new(rex),
        None => Arc::new(Core::new(crate::register::claude_path())),
    }
}

/// The built-in module's executable as the environment names it.
pub fn exe_from_env() -> PathBuf {
    rex::path()
}

/// What `tower lead attach` and the SessionStart hook tell a lead whose session runs outside
/// the built-in module's terminal, by this process's environment (whether that module is
/// found and answers included, asked with `db` as the board); `None` inside it.
pub fn outside_from_env(db: &Path) -> Option<String> {
    rex::outside(|name| std::env::var_os(name), db)
}

/// What the dashboard on `board` in a tab of its own writes to its terminal, by this
/// process's environment, so the dashboard's key for that board can find it: (on start, on
/// exit), or `None` where the built-in module's terminal is not the one it runs in.
pub fn dashboard_status_from_env(board: &Path) -> Option<(String, &'static str)> {
    rex::dashboard_status(|name| std::env::var_os(name), board)
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

/// How long the module is given to answer (`Terminal::silent`), by the watch before each
/// look and by a lead's notice outside it (`rex::outside`): `ANSWERS_WITHIN`, a local call,
/// so a module that takes longer is not answering, and one that hangs holds the caller this
/// long at most. In unit tests, and in a debug build given `TOWER_TERMINAL_TIMEOUT` (the
/// integration tests), it is `terminal_timeout`, since a fake's first run can stall for
/// seconds on a busy machine.
pub(crate) fn answers_within() -> Duration {
    let knob = cfg!(debug_assertions) && std::env::var_os("TOWER_TERMINAL_TIMEOUT").is_some();
    if cfg!(test) || knob {
        terminal_timeout()
    } else {
        ANSWERS_WITHIN
    }
}

/// How long a module has to answer, outside tests (`answers_within`).
const ANSWERS_WITHIN: Duration = Duration::from_secs(3);

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
    use crate::terminal::ghostty::fake::FakeGhostty;
    use crate::terminal::rex::fake::FakeRex;
    use crate::watch::TEST_WAIT;
    use std::os::unix::fs::PermissionsExt;

    /// One session as `claude agents --json` lists it: interactive unless `background`.
    fn session(
        name: Option<&str>,
        status: Option<&str>,
        waiting_for: Option<&str>,
    ) -> serde_json::Value {
        let mut s = serde_json::json!({
            "pid": 1, "cwd": "/somewhere", "kind": "interactive", "startedAt": 1,
            "sessionId": "sess-1",
        });
        for (key, value) in [
            ("name", name),
            ("status", status),
            ("waitingFor", waiting_for),
        ] {
            if let Some(value) = value {
                s[key] = value.into();
            }
        }
        s
    }

    /// The fakes in a temp folder, with Claude listing `sessions`, and the core over them.
    fn over(sessions: &[serde_json::Value]) -> (tempfile::TempDir, FakeRex, Core) {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        f.write(
            "claude.json",
            &serde_json::Value::from(sessions).to_string(),
        );
        let core = Core::new(dir.path().join("claude"));
        (dir, f, core)
    }

    fn agent(name: &str, status: Status) -> Agent {
        Agent {
            name: name.into(),
            status,
            msg: None,
        }
    }

    #[test]
    fn the_core_reads_each_worker_from_claudes_list_by_its_status_and_what_it_waits_for() {
        let mut background = session(Some("bg"), Some("waiting"), Some("input needed"));
        background["kind"] = "background".into();
        background["id"] = "b1".into();
        background["state"] = "blocked".into();
        let (dir, _f, core) = over(&[
            session(Some("busy"), Some("busy"), None),
            session(Some("idle"), Some("idle"), None),
            session(
                Some("asks-leave"),
                Some("waiting"),
                Some("permission prompt"),
            ),
            session(Some("asks-what"), Some("waiting"), Some("input needed")),
            session(Some("asks-other"), Some("waiting"), Some("sign in")),
            session(Some("asks-none"), Some("waiting"), None),
            session(Some("odd"), Some("stopping"), None),
            session(Some("silent"), None, None),
            // Not strings: a value the core does not know, never a list it cannot read.
            serde_json::json!({"name": "odd-type", "status": 1}),
            serde_json::json!({"name": "odd-wait", "status": "waiting", "waitingFor": {"x": 1}}),
            // No name, or one that is not a string: no one, whatever its status. A pid or
            // sessionId that is not the type listed fails nothing.
            session(None, Some("waiting"), Some("permission prompt")),
            serde_json::json!({"name": 7, "status": "busy"}),
            serde_json::json!({"name": "odd-ids", "pid": "301", "sessionId": 5, "status": "idle"}),
            background,
        ]);
        let got = core
            .agents(&dir.path().join("board.db"), TEST_WAIT)
            .unwrap();
        assert_eq!(
            got,
            [
                agent("busy", Status::Working),
                agent("idle", Status::Idle),
                agent("asks-leave", Status::Blocked(Some(Kind::Permission))),
                agent("asks-what", Status::Blocked(Some(Kind::Question))),
                agent("asks-other", Status::Blocked(None)),
                agent("asks-none", Status::Blocked(None)),
                agent("odd", Status::Unknown),
                agent("silent", Status::Unknown),
                agent("odd-type", Status::Unknown),
                agent("odd-wait", Status::Blocked(None)),
                agent("odd-ids", Status::Idle),
                agent("bg", Status::Blocked(Some(Kind::Question))),
            ]
        );
        // A session gone from the list is gone from the workers.
        let (dir, _f, core) = over(&[]);
        let got = core
            .agents(&dir.path().join("board.db"), TEST_WAIT)
            .unwrap();
        assert_eq!(got, []);
    }

    #[test]
    fn a_field_no_session_gives_as_the_type_claude_lists_fails_the_read() {
        let fails = |json: &str| parse(json).unwrap_err();
        let unexpected = "claude agents --json said something unexpected: ";
        // Every entry that gives it, of another type: Claude's format changed.
        let name = r#"[{"name":7,"pid":1},{"name":{"x":1},"pid":2},{"pid":3}]"#;
        assert_eq!(
            fails(name),
            format!("{unexpected}no session's name is a string")
        );
        let id = r#"[{"name":"dev-1","sessionId":5}]"#;
        assert_eq!(
            fails(id),
            format!("{unexpected}no session's sessionId is a string")
        );
        let pid = r#"[{"name":"dev-1","pid":"41"},{"name":"dev-2","pid":4.5}]"#;
        assert_eq!(
            fails(pid),
            format!("{unexpected}no session's pid is an integer")
        );
        // One odd beside one of the type listed: that entry stands alone, as none.
        let one =
            r#"[{"name":7,"sessionId":5,"pid":"41"},{"name":"dev-1","sessionId":"s1","pid":41}]"#;
        let got = parse(one).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].name.as_deref(), &got[0].session_id, got[0].pid),
            (None, &None, None)
        );
        assert_eq!(got[1].name.as_deref(), Some("dev-1"));
        assert_eq!(got[1].session_id.as_deref(), Some("s1"));
        assert_eq!(got[1].pid, Some(41));
        // Absent, or null, in every entry: read, as sessions with nothing to know them by.
        let absent = r#"[{"status":"busy"},{"name":null,"sessionId":null,"pid":null}]"#;
        let got = parse(absent).unwrap();
        assert_eq!(got.len(), 2);
        assert!(
            got.iter()
                .all(|l| l.name.is_none() && l.session_id.is_none() && l.pid.is_none())
        );
        assert!(parse("[]").unwrap().is_empty());
        // Not a list of entries: unreadable, as before.
        assert!(fails("[1]").starts_with(unexpected));
        assert!(fails("{}").starts_with(unexpected));
    }

    #[test]
    fn the_core_fails_a_look_when_claudes_list_cannot_be_read() {
        let (dir, f, core) = over(&[]);
        let db = dir.path().join("board.db");
        f.write("claude.json", "not json");
        match core.agents(&db, TEST_WAIT) {
            Err(Unread::Failed(why)) => {
                assert!(
                    why.starts_with("claude agents --json said something unexpected"),
                    "{why}"
                )
            }
            other => panic!("{other:?}"),
        }
        let gone = Core::new(dir.path().join("no-claude"));
        assert!(matches!(
            gone.agents(&db, TEST_WAIT),
            Err(Unread::Failed(_))
        ));
        // What the watch says once: claude is found, or not.
        assert_eq!(core.missing(None), None);
        assert_eq!(
            gone.missing(None),
            Some(not_found("claude", &dir.path().join("no-claude"), None))
        );
    }

    #[test]
    fn the_core_prompts_a_listed_worker_through_the_relay_and_one_at_a_prompt_never() {
        let (dir, f, core) = over(&[
            session(Some("dev-1"), Some("idle"), None),
            session(Some("qa"), Some("waiting"), Some("permission prompt")),
            session(Some("rev"), Some("waiting"), Some("input needed")),
        ]);
        let db = dir.path().join("board.db");
        let prompt = |name: &str| core.prompt(&db, name, "hello", TEST_WAIT).sent;
        assert_eq!(prompt("DEV-1"), Sent::Delivered);
        assert_eq!(prompt("qa"), Sent::Failed("at a permission prompt".into()));
        assert_eq!(prompt("rev"), Sent::Failed("at a question".into()));
        assert_eq!(
            prompt("ghost"),
            Sent::Failed("Claude Code has no agent named ghost".into())
        );
        // The relay ran once, for dev-1 alone.
        let relays = (f.calls().into_iter()).filter(|c| c.starts_with("claude|-p"));
        assert_eq!(relays.count(), 1);
        let sent = std::fs::read_to_string(dir.path().join("prompts.log")).unwrap();
        assert_eq!(sent, "dev-1|hello\n");
    }

    #[test]
    fn the_core_cannot_start_a_lead_or_serve_the_keys() {
        let (dir, _f, core) = over(&[]);
        let db = dir.path().join("board.db");
        let here = Here {
            session: "s".into(),
            block: "b".into(),
        };
        let lead = LeadStart {
            name: "lead",
            dir: "/",
            board: "/board.db",
            command: &[],
        };
        let said = [
            core.start_lead(&db, &lead).map(|_| ()),
            core.undo(&db, &Handle("h".into())).map(|_| ()),
            core.space(&db, &here, &dir.path().join("back"), &[])
                .map(|_| ()),
            core.popup(&db, &here, &[]),
        ]
        .map(|r| r.unwrap_err().to_string());
        assert_eq!(
            said,
            [
                "starting a lead needs a terminal module (Rex), and there is none",
                "closing a lead's place needs a terminal module (Rex), and there is none",
                "the dashboard's key needs a terminal module (Rex), and there is none",
                "the popup's key needs a terminal module (Rex), and there is none",
            ]
        );
    }

    #[test]
    fn the_core_starts_and_closes_a_worker_in_ghostty_and_with_none_says_what_it_needs() {
        let (dir, f, _) = over(&[]);
        let g = FakeGhostty::new(dir.path());
        let core = Core::over(dir.path().join("claude"), g.ghostty());
        let db = dir.path().join("board.db");
        let command = ["claude", "-n", "dev-1", "the brief"].map(String::from);
        let worker = WorkerStart {
            label: "dev-1",
            dir: "/srv/shop",
            command: &command,
            beside: None,
        };
        core.starts_workers().unwrap();
        let tab = core.start_worker(&db, &worker).unwrap();
        assert_eq!(tab, Handle("ghostty-tab,T1".into()));
        let beside = WorkerStart {
            beside: Some(&tab),
            ..worker
        };
        let split = core.start_worker(&db, &beside).unwrap();
        assert_eq!(split, Handle("ghostty-split,T2".into()));
        assert_eq!(core.close_worker(&db, &split).unwrap(), Closed::Now);
        // Ghostty has no way to read a pane.
        assert_eq!(core.shows(&db, &tab), None);
        assert_eq!(
            g.calls(),
            [
                "installed|",
                "start||/srv/shop|<command>|dev-1|",
                "start||/srv/shop|<command>|dev-1|T1",
                "installed|",
                "close|split|T2",
            ]
        );
        // No Ghostty: both say what workers need, and nothing is started or closed.
        g.write(
            "fail-installed",
            "syntax error: Can\u{2019}t get application id \"com.mitchellh.ghostty\". (-1728)\n",
        );
        let said = [
            core.starts_workers(),
            core.close_worker(&db, &tab).map(|_| ()),
        ]
        .map(|r| r.unwrap_err().to_string());
        let needs = "needs Ghostty or Rex: Ghostty is not installed, and this session is not in \
                     Rex (REX_SESSION is not set)";
        assert_eq!(
            said,
            [
                format!("starting a worker {needs}"),
                format!("closing a worker {needs}"),
            ]
        );
        assert_eq!(g.calls()[5..], ["installed|", "installed|"]);
        // Neither rex nor claude was run for any of it.
        assert_eq!(f.calls(), [] as [String; 0]);
    }

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
