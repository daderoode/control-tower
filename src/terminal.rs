//! The terminal module: the one place the core (the watch's look, the delivery round, the
//! dashboard's `n`) reaches the terminal its workers run in. The core asks it for the live
//! workers (`agents`), has it deliver a text to one by name (`prompt`), and has it open a
//! new place for a lead (`start_lead`, undone by `undo`); its display name names the
//! terminal in the core's texts and logs. A module is built in: `module` gives the one
//! this build has, herdr (`herdr`), so another slots in here without touching the core.
//!
//! Status words are the Program Status Protocol's (idle, working, done, blocked, and the
//! blocked kind where the terminal gives it). A value the core does not know is
//! `Status::Unknown`: present, with nothing to judge by.

pub mod herdr;

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
}

/// A lead to start: its place named `name`, opened in `dir`, with the board path `board`
/// set as `TOWER_DB` there when given, running `command` (one word per argument, never a
/// shell string).
#[derive(Debug, Clone, Copy)]
pub struct LeadStart<'a> {
    pub name: &'a str,
    pub dir: &'a str,
    pub board: Option<&'a str>,
    pub command: &'a [String],
}

/// A started lead's place, as its module names it: what `Terminal::undo` closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle(pub String);

/// The terminal module interface. `db` is the board, passed to what the module runs as
/// `TOWER_DB`.
pub trait Terminal: Send + Sync {
    /// The terminal's name in texts and logs.
    fn name(&self) -> &'static str;
    /// Its executable, as the module was given it.
    fn exe(&self) -> &Path;
    /// The one line saying the executable cannot be found on `path` (a PATH value),
    /// naming what was tried; `None` when it is found.
    fn missing(&self, path: Option<&OsStr>) -> Option<String>;
    /// The live named workers, within `timeout`.
    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread>;
    /// Delivers `text` to the worker `name`, within `timeout`.
    fn prompt(&self, db: &Path, name: &str, text: &str, timeout: Duration) -> Sent;
    /// Opens a new place for a lead without taking focus and starts it there.
    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle>;
    /// Undoes a start: closes the place `handle` names. Says what it did.
    fn undo(&self, db: &Path, handle: &Handle) -> Result<String>;
}

/// The built-in terminal module, its executable at `exe`.
pub fn module(exe: PathBuf) -> Arc<dyn Terminal> {
    Arc::new(herdr::Herdr::new(exe))
}

/// The built-in module's executable as the environment names it.
pub fn exe_from_env() -> PathBuf {
    herdr::path()
}
