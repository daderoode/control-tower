//! The Rex module: the terminal module interface over the `rex` command line (`path`), on
//! the local Rex server only, never starting one (`--autostart=false`).
//!
//! Rex overrides the core's live workers (`agents`) only: it adds what a worker's prompt
//! asks. The workers are the Claude sessions running in Rex's terminal blocks: every
//! session's blocks, each terminal block's `program_status` root record (the one without an
//! id; child records are a session's background shells and subagents), kept when its
//! `effective_app` is `claude-code` with an owner pid, joined by that pid to Claude's list
//! (`super::listed`). A worker's name is its Claude session's name, its status the root
//! record's state and kind, its `msg` the record's. A block with no Claude record, or whose
//! pid Claude does not list, is no one. One look is one call per session and per terminal
//! block, all within the look's time limit; any call that fails fails the whole look, so a
//! Rex half read never reads as workers gone.
//!
//! A text reaches a worker by the core's `prompt`, over Rex's look: a worker at a prompt is
//! sent nothing, the send failing with what the prompt asks.
//!
//! A lead starts in a new Rex session labelled with its name (`rex new`), which Rex does not
//! show in the app, so focus stays where it is; the session runs the lead's command directly,
//! each argument its own word, never through a shell string. The handle is the session's id,
//! and undo kills it (`rex kill`).
//!
//! The dashboard's key (`space`, run by `rex/control-tower.lua`'s `tower_space` action with
//! the session and block the key was pressed in) finds the dashboard's tab by asking Rex, not
//! by its label: the dashboard in a tab of its own names itself and its board in its root
//! `program_status` record (`dashboard_status`, `dashboard_app`), and a block is a
//! dashboard of the key's board while that record names the key's board and its owner is
//! the block's foreground process, by pid and name (a record outlives its program). The
//! key's session is looked in first, then the others in Rex's order; another session or
//! block Rex cannot read is passed over, the key's own fails the key. The key answers with
//! the place for the snippet to show (`session.select`, which only the app does): from a
//! tab holding a dashboard, the place in the back file, if Rex still has it; from anywhere
//! else, the first dashboard's tab, after writing where the key was pressed to the back
//! file. With no
//! dashboard, it opens a tab running it in the key's session first. The popup (`popup`)
//! is a floating layer over the key's tab, 85% each way, focused; it goes when its block
//! exits.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{
    Agent, Answer, Handle, Here, LeadStart, Listed, Place, Shown, Status, Terminal, Unread,
    find_exe, listed, not_found, terminal_timeout,
};
use crate::ui::runner::{Outcome, Run, run_now};

/// The creator of Rex's terminal blocks: only they have a `program_status`.
const TERMINAL_CREATOR: &str = "com.superlogical.terminal";
/// The terminal block method listing what its programs reported.
const PROGRAM_STATUS: &str = "com.superlogical.terminal.program_status";
/// The app Claude Code reports itself as.
const CLAUDE_CODE: &str = "claude-code";
/// What the app the dashboard in a tab of its own reports itself as starts with, before its
/// board's hash (`dashboard_app`).
const DASHBOARD_APP: &str = "control-tower";
/// The terminal block method naming a terminal's processes.
const PROCESS: &str = "com.superlogical.terminal.process";
/// The flavor of a terminal block, which runs its command through the user's shell.
const SHELL: &str = "com.superlogical.terminal.shell";
/// The label of the dashboard's tab and popup block, for the owner's eyes only: the key
/// never finds the dashboard by it.
const DASHBOARD_LABEL: &str = "control-tower";
/// The popup's place in its tab, as [x, y, w, h] fractions: 85% each way, centred.
const POPUP_BOUNDS: [f64; 4] = [0.075, 0.075, 0.85, 0.85];
/// How long the key waits after opening the dashboard's tab before answering: the app can
/// drop a `session.select` of a window made just before it (a known Rex bug).
const NEW_TAB_SETTLE: Duration = Duration::from_millis(300);

/// The rex executable as this process's environment names it (`path_in`).
pub fn path() -> PathBuf {
    path_in(|name| std::env::var_os(name))
}

/// The rex executable as `env` names it: `TOWER_REX` if set; else `rex` in `REX_BIN_DIR`,
/// where Rex keeps it and which Rex sets in its own tabs, when one is there; else `rex` on
/// the PATH (Rex does not put it there).
pub fn path_in(env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(p) = env("TOWER_REX").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let in_bin_dir = env("REX_BIN_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("rex"))
        .filter(|p| p.is_absolute() && find_exe(p, None).is_some());
    in_bin_dir.unwrap_or_else(|| PathBuf::from("rex"))
}

/// The app the dashboard on `board` reports itself as: `control-tower-` and the first 8 hex
/// digits of the SHA-256 of the board's canonical path (its path as given when it cannot be
/// resolved, as before the board file exists), so the key finds its own board's dashboard
/// only. 22 bytes: Rex ignores a report whose app is longer than 32.
pub fn dashboard_app(board: &Path) -> String {
    use sha2::Digest;
    use std::os::unix::ffi::OsStrExt;
    let canonical = std::fs::canonicalize(board).unwrap_or_else(|_| board.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.as_os_str().as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{DASHBOARD_APP}-{hex}")
}

/// What the dashboard on `board` in a tab of its own writes to its terminal, by `env`: on
/// start its root program status record, idle, as `dashboard_app(board)`; on exit, the
/// record cleared. `None` outside Rex, where Rex sets `REX_BLOCK` (an empty one counts as
/// unset), so another terminal is never sent Rex's sequence.
pub fn dashboard_status(
    env: impl Fn(&str) -> Option<OsString>,
    board: &Path,
) -> Option<(String, &'static str)> {
    env("REX_BLOCK").filter(|b| !b.is_empty())?;
    Some((
        format!("\x1b]7501;state=idle:app={}\x1b\\", dashboard_app(board)),
        "\x1b]7501;state=clear\x1b\\",
    ))
}

/// What a lead running outside Rex is told, by `env`: `None` inside Rex, where Rex sets
/// `REX_SESSION` (an empty one counts as unset).
pub fn outside(env: impl Fn(&str) -> Option<OsString>) -> Option<String> {
    if env("REX_SESSION").is_some_and(|s| !s.is_empty()) {
        return None;
    }
    Some(
        "control-tower: this session runs outside Rex (REX_SESSION is not set), but the \
         board's team runs in Rex. Outside Rex the board does not see the workers you start: \
         not their prompts, and posts and nudges are not delivered to them. Raise it with the \
         owner in a question: post to @manager."
            .into(),
    )
}

/// Rex as the terminal module, run from `exe`, asking the `claude` at `claude` for the
/// names of the Claude sessions it finds.
pub struct Rex {
    exe: PathBuf,
    claude: PathBuf,
}

impl Rex {
    pub fn new(exe: PathBuf, claude: PathBuf) -> Rex {
        Rex { exe, claude }
    }

    /// Runs `rex <args>`, bounded by `terminal_timeout`.
    fn run(&self, db: &Path, args: &[&str]) -> Outcome {
        run_now(&Run {
            exe: self.exe.clone(),
            args: args.iter().map(|a| a.to_string()).collect(),
            db: db.to_path_buf(),
            env: Vec::new(),
            stdin: None,
            timeout: terminal_timeout(),
        })
    }

    /// `rex api call <method> <input>` on the local server, bounded by what is left of the
    /// look before `deadline` (`limit`, for its message): what it printed, read as `T`.
    fn call<T: DeserializeOwned>(
        &self,
        db: &Path,
        method: &str,
        input: serde_json::Value,
        deadline: Instant,
        limit: Duration,
    ) -> Result<T, Unread> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(out_of_time(limit));
        }
        let outcome = run_now(&Run {
            exe: self.exe.clone(),
            args: [
                "--autostart=false",
                "api",
                "call",
                method,
                &input.to_string(),
            ]
            .map(String::from)
            .to_vec(),
            db: db.to_path_buf(),
            env: Vec::new(),
            stdin: None,
            timeout: left,
        });
        let command = format!("rex api call {method}");
        let stdout = match outcome {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => stdout,
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
                return Err(Unread::Failed(format!("{command} {how}: {}", said.trim())));
            }
            Outcome::TimedOut { .. } => return Err(out_of_time(limit)),
            Outcome::Failed(why) => return Err(Unread::Failed(why)),
        };
        serde_json::from_str(&stdout).map_err(|e| {
            Unread::Failed(format!(
                "{command} said something unexpected ({e}): {}",
                stdout.trim()
            ))
        })
    }
}

impl Rex {
    /// `call` for the dashboard's key, before `deadline`, the end of a look of `limit`.
    fn ask<T: DeserializeOwned>(
        &self,
        db: &Path,
        method: &str,
        input: serde_json::Value,
        (deadline, limit): (Instant, Duration),
    ) -> Result<T> {
        Ok(self.call(db, method, input, deadline, limit)?)
    }

    /// Whether the terminal block `block` in `session` is a dashboard of the board whose
    /// dashboard reports itself as `app` (`dashboard_app`): its root record is that app's
    /// and its owner is the block's foreground process, by pid, and by name when the record
    /// names one.
    fn is_dashboard(
        &self,
        db: &Path,
        (session, block): (&str, &str),
        app: &str,
        time: (Instant, Duration),
    ) -> Result<bool> {
        let input = serde_json::json!({ "session_id": session, "block_id": block, "args": {} });
        let records: Records = self.ask(db, PROGRAM_STATUS, input.clone(), time)?;
        let Some(owner) = dashboard_in(records.records, app) else {
            return Ok(false);
        };
        let processes: Processes = self.ask(db, PROCESS, input, time)?;
        Ok(processes.foreground.is_some_and(|f| {
            f.pid == owner.pid && owner.name.as_ref().is_none_or(|n| *n == f.name)
        }))
    }

    /// The key's answer (`Terminal::space`) within `limit`, with each block or session it
    /// passed over in `passed_over`.
    fn shown(
        &self,
        db: &Path,
        (here, back, dashboard): (&Here, &Path, &[String]),
        limit: Duration,
        passed_over: &mut Vec<String>,
    ) -> Result<Shown> {
        let time = (Instant::now() + limit, limit);
        let app = dashboard_app(db);
        let none = serde_json::json!({});
        let sessions: Sessions = self.ask(db, "session.list", none, time)?;
        let mut ids: Vec<String> = (sessions.sessions.unwrap_or_default().into_iter())
            .map(|s| s.session_id)
            .collect();
        let Some(at) = ids.iter().position(|s| *s == here.session) else {
            bail!("Rex has no session {}", here.session);
        };
        let key_session = ids.remove(at);
        ids.insert(0, key_session);
        // Every block in a tab, as (session, block, window); the sessions passed over; the
        // first dashboard found; whether a dashboard is in the key's own tab.
        let mut placed: Vec<(String, String, String)> = Vec::new();
        let mut unread: Vec<String> = Vec::new();
        let mut found: Option<Place> = None;
        let mut in_dashboard = false;
        // Another session or block Rex cannot read (one closing as the key looks) is passed
        // over; the key's own, or a look out of time, fails the key.
        let pass_over = |err: anyhow::Error, own: bool| {
            if own || Instant::now() >= time.0 {
                Err(err)
            } else {
                Ok(format!("{err:#}"))
            }
        };
        let mut pressed: Option<Place> = None;
        for s in &ids {
            let input = serde_json::json!({ "session_id": s });
            let blocks: Blocks = match self.ask(db, "session.list_blocks", input, time) {
                Ok(blocks) => blocks,
                Err(e) => {
                    let why = pass_over(e, *s == here.session)?;
                    passed_over.push(format!(
                        "passed over session {s}, which Rex could not list: {why}"
                    ));
                    unread.push(s.clone());
                    continue;
                }
            };
            let tabbed: Vec<(String, String, bool)> = (blocks.blocks.unwrap_or_default())
                .into_iter()
                .filter(|b| b.placement.as_deref() == Some("placed"))
                .filter_map(|b| {
                    let terminal = b.creator_name.as_deref() == Some(TERMINAL_CREATOR);
                    Some((b.block_id?, b.window_id?, terminal))
                })
                .collect();
            // The key's session comes first: the tab it was pressed in.
            if *s == here.session {
                let Some((_, window, _)) = tabbed.iter().find(|(b, _, _)| *b == here.block) else {
                    bail!(
                        "Rex has no block {} in a tab of session {}",
                        here.block,
                        here.session
                    );
                };
                pressed = Some(Place {
                    session_id: s.clone(),
                    window_id: window.clone(),
                });
            }
            for (block, window, terminal) in tabbed {
                let in_key_tab =
                    *s == here.session && pressed.as_ref().is_some_and(|p| p.window_id == window);
                if terminal && (found.is_none() || (in_key_tab && !in_dashboard)) {
                    let own = *s == here.session && block == here.block;
                    match self.is_dashboard(db, (s, &block), &app, time) {
                        Ok(true) => {
                            in_dashboard |= in_key_tab;
                            found.get_or_insert_with(|| Place {
                                session_id: s.clone(),
                                window_id: window.clone(),
                            });
                        }
                        Ok(false) => {}
                        Err(e) => {
                            let why = pass_over(e, own)?;
                            passed_over.push(format!(
                                "passed over block {block} in session {s}, which Rex could not \
                                 read: {why}"
                            ));
                        }
                    }
                }
                placed.push((s.clone(), block, window));
            }
        }
        let Some(pressed) = pressed else {
            bail!("Rex has no session {}", here.session);
        };
        if in_dashboard {
            let to = match read_back(back) {
                Ok(Some(to)) => to,
                Ok(None) => {
                    return Ok(Shown::Stay(
                        "no place to go back to: the key has not been pressed outside the \
                         dashboard"
                            .into(),
                    ));
                }
                Err(why) => return Ok(Shown::Stay(why)),
            };
            let last = format!(
                "the place the key was last pressed outside the dashboard (window {} in \
                 session {})",
                to.window_id, to.session_id
            );
            if unread.contains(&to.session_id) {
                return Ok(Shown::Stay(format!(
                    "{last} cannot be read: Rex could not list its session"
                )));
            }
            let there = (placed.iter()).any(|(s, _, w)| *s == to.session_id && *w == to.window_id);
            return Ok(if there {
                Shown::Go(to)
            } else {
                Shown::Stay(format!("{last} is gone"))
            });
        }
        let json = serde_json::to_string(&pressed)?;
        if let Some(folder) = back.parent() {
            std::fs::create_dir_all(folder).with_context(|| {
                format!("cannot make the back file's folder {}", folder.display())
            })?;
        }
        std::fs::write(back, format!("{json}\n"))
            .with_context(|| format!("cannot write the back file {}", back.display()))?;
        if let Some(dashboard) = found {
            return Ok(Shown::Go(dashboard));
        }
        let input = serde_json::json!({
            "session_id": here.session,
            "window_label": DASHBOARD_LABEL,
            "layout": dashboard_block(dashboard),
        });
        let made: NewWindow = self.ask(db, "session.new_window", input, time)?;
        std::thread::sleep(NEW_TAB_SETTLE);
        Ok(Shown::Go(Place {
            session_id: here.session.clone(),
            window_id: made.window_id,
        }))
    }
}

/// The layout node of the dashboard's block: a terminal block labelled for the dashboard,
/// running `command` (one word per argument).
fn dashboard_block(command: &[String]) -> serde_json::Value {
    serde_json::json!({
        "block": { "flavor": SHELL, "label": DASHBOARD_LABEL, "options": { "command": command } }
    })
}

/// The place in the back file at `back`: `Ok(None)` when there is none, `Err` saying why
/// when it cannot be read as one.
fn read_back(back: &Path) -> Result<Option<Place>, String> {
    let text = match std::fs::read_to_string(back) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "the back file {} cannot be read: {e}",
                back.display()
            ));
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("the back file {} is not a place ({e})", back.display()))
}

/// A look that ran past its limit.
fn out_of_time(limit: Duration) -> Unread {
    Unread::Failed(format!(
        "Rex did not answer within {} s and the look was stopped",
        limit.as_secs()
    ))
}

/// What `rex <command>` ending in `outcome` gave: its stdout when it exited 0, else what
/// went wrong.
fn stdout(command: &str, outcome: Outcome) -> Result<String> {
    match outcome {
        Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(stdout),
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
            bail!("rex {command} {how}: {}", said.trim())
        }
        Outcome::TimedOut { after, .. } => bail!(
            "rex {command} did not finish in {} s and was stopped",
            after.as_secs()
        ),
        Outcome::Failed(why) => bail!("{why}"),
    }
}

#[derive(Deserialize)]
struct Created {
    session_id: String,
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Option<Vec<Session>>,
}

#[derive(Deserialize)]
struct Session {
    session_id: String,
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Option<Vec<Block>>,
}

#[derive(Deserialize)]
struct Block {
    block_id: Option<String>,
    creator_name: Option<String>,
    placement: Option<String>,
    window_id: Option<String>,
}

#[derive(Deserialize)]
struct NewWindow {
    window_id: String,
}

/// What `com.superlogical.terminal.process` says, as far as the key reads it.
#[derive(Deserialize)]
struct Processes {
    foreground: Option<Process>,
}

#[derive(Deserialize)]
struct Process {
    pid: i64,
    name: String,
}

#[derive(Deserialize)]
struct Records {
    records: Vec<Record>,
}

/// One `program_status` record, as far as the look reads it.
#[derive(Deserialize)]
struct Record {
    id: Option<String>,
    state: String,
    kind: Option<String>,
    msg: Option<String>,
    effective_app: Option<String>,
    owner: Option<Owner>,
}

#[derive(Deserialize)]
struct Owner {
    pid: i64,
    name: Option<String>,
}

/// A block's Claude: the pid its root record names, with the record's status and msg, when
/// that record is Claude Code's. `None` for a block with no records, or whose root record
/// is another program's or names no owner. Child records (with an id) are never read.
fn claude_in(records: Vec<Record>) -> Option<(i64, Status, Option<String>)> {
    let root = records.into_iter().find(|r| r.id.is_none())?;
    if root.effective_app.as_deref() != Some(CLAUDE_CODE) {
        return None;
    }
    let pid = root.owner?.pid;
    Some((pid, Status::of(&root.state, root.kind.as_deref()), root.msg))
}

/// The owner of a block's root record when that record is `app`'s (`dashboard_app`);
/// `None` for any other, or one that names no owner.
fn dashboard_in(records: Vec<Record>, app: &str) -> Option<Owner> {
    let root = records.into_iter().find(|r| r.id.is_none())?;
    if root.effective_app.as_deref() != Some(app) {
        return None;
    }
    root.owner
}

/// The workers: each block's Claude (`found`, in Rex's order) that `claude agents --json`
/// (`listed`) lists by its pid, named as Claude names it. A pid Claude does not list, or
/// lists with no name, is no one.
fn join(found: Vec<(i64, Status, Option<String>)>, listed: &[Listed]) -> Vec<Agent> {
    (found.into_iter())
        .filter_map(|(pid, status, msg)| {
            let name = (listed.iter()).find(|l| l.pid == Some(pid))?.name.clone()?;
            Some(Agent { name, status, msg })
        })
        .collect()
}

impl Terminal for Rex {
    fn name(&self) -> &'static str {
        "Rex"
    }

    fn exe(&self) -> &Path {
        &self.exe
    }

    fn claude(&self) -> &Path {
        &self.claude
    }

    fn missing(&self, path: Option<&OsStr>) -> Option<String> {
        match find_exe(&self.exe, path) {
            Some(_) => None,
            None => Some(format!(
                "{}; Rex keeps it in REX_BIN_DIR, which Rex sets in its own tabs: link it into \
                 Homebrew's bin (ln -s \"$REX_BIN_DIR/rex\" \"$(brew --prefix)/bin/rex\"), or \
                 set TOWER_REX to its full path",
                not_found("rex", &self.exe, path)
            )),
        }
    }

    fn adds(&self) -> Option<&'static str> {
        Some(
            "the text of the prompt a worker waits at in its owner item, the dashboard's n and \
             its keys, and starting workers as visible tabs",
        )
    }

    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread> {
        let deadline = Instant::now() + timeout;
        let none = serde_json::json!({});
        let sessions: Sessions = self.call(db, "session.list", none, deadline, timeout)?;
        let mut found = Vec::new();
        for s in sessions.sessions.unwrap_or_default() {
            let input = serde_json::json!({ "session_id": s.session_id });
            let blocks: Blocks = self.call(db, "session.list_blocks", input, deadline, timeout)?;
            for b in blocks.blocks.unwrap_or_default() {
                let Some(block) = b.block_id else { continue };
                if b.creator_name.as_deref() != Some(TERMINAL_CREATOR) {
                    continue;
                }
                let input = serde_json::json!({
                    "session_id": s.session_id, "block_id": block, "args": {},
                });
                let records: Records = self.call(db, PROGRAM_STATUS, input, deadline, timeout)?;
                found.extend(claude_in(records.records));
            }
        }
        // No Claude in Rex: nothing to name, and claude is not asked.
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(out_of_time(timeout));
        }
        Ok(join(found, &listed(&self.claude, db, left)?))
    }

    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle> {
        let env = format!("TOWER_DB={}", lead.board);
        let mut args = vec![
            "new", lead.name, "-c", lead.dir, "--json", "--", "env", &env,
        ];
        args.extend(lead.command.iter().map(String::as_str));
        // Rex may have made the session before it stopped, or before a reply this cannot
        // read; with no id it cannot be killed here.
        let left = || {
            format!(
                "a Rex session labelled {:?} may be left: rex ls, then rex kill its id",
                lead.name
            )
        };
        let outcome = self.run(db, &args);
        let timed_out = matches!(outcome, Outcome::TimedOut { .. });
        let reply =
            stdout("new", outcome).map_err(|e| if timed_out { e.context(left()) } else { e })?;
        let created: Created = serde_json::from_str(&reply)
            .with_context(|| format!("rex new said something unexpected: {}", reply.trim()))
            .with_context(left)?;
        Ok(Handle(created.session_id))
    }

    fn undo(&self, db: &Path, handle: &Handle) -> Result<String> {
        stdout("kill", self.run(db, &["kill", &handle.0]))?;
        Ok(format!("killed Rex session {}", handle.0))
    }

    fn space(&self, db: &Path, here: &Here, back: &Path, dashboard: &[String]) -> Result<Answer> {
        let mut passed_over = Vec::new();
        let asked = (here, back, dashboard);
        let shown = self.shown(db, asked, terminal_timeout(), &mut passed_over)?;
        Ok(Answer { shown, passed_over })
    }

    fn popup(&self, db: &Path, here: &Here, dashboard: &[String]) -> Result<()> {
        let time = (Instant::now() + terminal_timeout(), terminal_timeout());
        let input = serde_json::json!({ "session_id": here.session });
        let blocks: Blocks = self.ask(db, "session.list_blocks", input, time)?;
        let window = (blocks.blocks.unwrap_or_default().into_iter())
            .find(|b| b.block_id.as_deref() == Some(here.block.as_str()))
            .and_then(|b| b.window_id);
        let Some(window) = window else {
            bail!(
                "Rex has no block {} in a tab of session {}",
                here.block,
                here.session
            );
        };
        let input = serde_json::json!({
            "session_id": here.session,
            "window_id": window,
            "bounds": POPUP_BOUNDS,
            "focus": true,
            "layout": dashboard_block(dashboard),
        });
        let _: serde_json::Value = self.ask(db, "session.new_layer", input, time)?;
        Ok(())
    }
}

/// Test only: a fake `rex` and `claude` in a temp folder, and the Rex module over them.
/// Rex's server is files: `sessions` lists one session id per line, `<session>.blocks` one
/// block per line as "<block id> <creator>", `<block>.window` the window it is in (none
/// when it is missing), `<block>.label` its label (`shell` when missing), `<block>.json`
/// that block's `program_status` records (none when it is missing) and `<block>.process`
/// its `process` answer (no processes when missing); a call about a block with a file
/// `slow-<block>` first sleeps that file's seconds. `session.new_window` makes `window:new`
/// and `session.new_layer` `layer:pop`. `claude.json` is what `claude
/// agents --json` prints. Each call's words go to `calls.log`, one call per line joined by
/// `|`; a call whose method or session or block has a file `fail-<it>` exits 1 saying so.
///
/// The fake claude is the relay too (`claude -p`): it reads the instruction on stdin
/// and sends the text between its two marker lines (the first line shaped as one, and
/// its closing twin) to the name it gives, printing the stream a relay prints; it
/// writes the relay's `TOWER_RELAY` to `relay.env`, and appends each send it makes as
/// "name|text" (newlines as `\n`) to `prompts.log`. For a recipient `<to>`:
/// `relay-refuse-<to>` has its SendMessage refused with that file's text;
/// `relay-quiet-<to>` makes the send, then crashes before its answer; `relay-none-<to>`
/// sends nothing and says that file's text; `relay-error-<to>` ends on an error, as a
/// provider outage does, sending nothing and saying that file's text, exit 1;
/// `relay-to-<to>` sends to the name in it
/// instead; `relay-changed-<to>` sends its text instead; `relay-hang-<to>` touches
/// `started` once the send is answered, then holds until `release` (or `TEST_WAIT`) and
/// touches `ended`. With `hold`, it does the same before it sends; with `slow`, it
/// sleeps that file's seconds first.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::{Path, PathBuf};

    use super::Rex;

    /// A session's blocks, each (block id, creator, its `program_status` records).
    pub type Blocks<'a> = [(&'a str, &'a str, &'a str)];

    /// A session's blocks in tabs, each (block id, window, its `program_status` records).
    pub type Tabs<'a> = [(&'a str, &'a str, &'a str)];

    pub struct FakeRex {
        pub dir: PathBuf,
    }

    /// The fake rex: answers `--autostart=false api call <method> <json>` from the files.
    const REX: &str = r#"#!/bin/sh
d=$(dirname "$0")
(IFS='|'; echo "$*") >> "$d/calls.log"
[ "$1" = "--autostart=false" ] && [ "$2 $3" = "api call" ] || { echo "unexpected: $*" >&2; exit 2; }
m=$4
field() { printf '%s' "$2" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
s=$(field session_id "$5"); b=$(field block_id "$5")
for it in "$m" "$s" "$b"; do
  [ -n "$it" ] && [ -e "$d/fail-$it" ] && { cat "$d/fail-$it" >&2; exit 1; }
done
[ -n "$b" ] && [ -e "$d/slow-$b" ] && sleep "$(cat "$d/slow-$b")" </dev/null >/dev/null 2>&1
case "$m" in
session.list)
  printf '{"sessions":['; sep=
  while read -r id; do printf '%s{"session_id":"%s","label":"work"}' "$sep" "$id"; sep=,; done < "$d/sessions"
  echo ']}' ;;
session.list_blocks)
  printf '{"blocks":['; sep=
  [ -e "$d/$s.blocks" ] && while read -r id creator; do
    label=shell; [ -e "$d/$id.label" ] && label=$(cat "$d/$id.label")
    window=; [ -e "$d/$id.window" ] && window=$(printf ',"window_id":"%s"' "$(cat "$d/$id.window")")
    printf '%s{"block_id":"%s","label":"%s","creator_name":"%s","placement":"placed"%s}' "$sep" "$id" "$label" "$creator" "$window"; sep=,
  done < "$d/$s.blocks"
  echo ']}' ;;
com.superlogical.terminal.program_status)
  if [ -e "$d/$b.json" ]; then cat "$d/$b.json"; else echo '{"records":[]}'; fi ;;
com.superlogical.terminal.process)
  if [ -e "$d/$b.process" ]; then cat "$d/$b.process"; else echo '{"home":"/somewhere"}'; fi ;;
session.new_window) echo '{"window_id":"window:new","block_ids":["block:new"],"revision":7}' ;;
session.new_layer) echo '{"layer_id":"layer:pop","block_ids":["block:pop"],"revision":8}' ;;
*) echo "unexpected method: $m" >&2; exit 2 ;;
esac
"#;

    /// The fake claude: `agents --json` prints `claude.json`; `-p` is the relay.
    const CLAUDE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "claude|$*" >> "$d/calls.log"
if [ "$*" = "agents --json" ]; then cat "$d/claude.json"; exit 0; fi
[ "$1" = "-p" ] || { echo "unexpected: $*" >&2; exit 2; }
in=$(cat)
to=$(printf '%s\n' "$in" | sed -n 's/.*"to" set to "\([^"]*\)".*/\1/p' | head -n 1)
begin=$(printf '%s\n' "$in" | grep -m 1 '^<control-tower-text-[0-9a-f]*>$')
text=$(printf '%s\n' "$in" | awk -v b="$begin" -v e="</${begin#<}" '$0 == e {p = 0} p {print} $0 == b {p = 1}')
printf '%s\n' "${TOWER_RELAY-unset}" > "$d/relay.env"
[ -e "$d/slow" ] && sleep "$(cat "$d/slow")"
if [ -e "$d/hold" ]; then : > "$d/started"; HOLD; : > "$d/ended"; fi
json() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{ORS=""} {if (NR>1) print "\\n"; print}'; }
echo '{"type":"system","subtype":"init","tools":["ListAgents","SendMessage","ToolSearch"]}'
if [ -e "$d/relay-none-$to" ]; then
  printf '{"type":"result","is_error":false,"num_turns":1,"total_cost_usd":0.001,"result":"%s"}\n' "$(json "$(cat "$d/relay-none-$to")")"
  exit 0
fi
if [ -e "$d/relay-error-$to" ]; then
  printf '{"type":"result","is_error":true,"num_turns":1,"total_cost_usd":0,"result":"%s"}\n' "$(json "$(cat "$d/relay-error-$to")")"
  exit 1
fi
sent_to=$to; [ -e "$d/relay-to-$to" ] && sent_to=$(cat "$d/relay-to-$to")
message=$text; [ -e "$d/relay-changed-$to" ] && message=$(cat "$d/relay-changed-$to")
printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"SendMessage","input":{"to":"%s","message":"%s"}}]}}\n' "$(json "$sent_to")" "$(json "$message")"
[ -e "$d/relay-quiet-$to" ] && { echo "relay crashed" >&2; exit 1; }
if [ -e "$d/relay-refuse-$to" ]; then
  ok=false; said=$(cat "$d/relay-refuse-$to")
else
  ok=true; said="in that session's inbox"
  printf '%s|%s\n' "$sent_to" "$(printf '%s' "$message" | awk 'BEGIN{ORS=""} {if (NR>1) print "\\n"; print}')" >> "$d/prompts.log"
fi
r=$(printf '{"success":%s,"message":"%s"}' "$ok" "$(json "$said")")
printf '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"%s"}]}]}}\n' "$(json "$r")"
[ -e "$d/relay-hang-$to" ] && { : > "$d/started"; HOLD; : > "$d/ended"; }
echo '{"type":"result","is_error":false,"num_turns":3,"total_cost_usd":0.00593,"result":"Sent it once.","permission_denials":[]}'
"#;

    impl FakeRex {
        /// The fakes in `dir`, with an empty Rex and an empty Claude list.
        pub fn new(dir: &Path) -> FakeRex {
            crate::test_exe::executable(&dir.join("rex"), REX);
            let claude = CLAUDE.replace("HOLD", &crate::watch::hold_until_released());
            crate::test_exe::executable(&dir.join("claude"), &claude);
            let f = FakeRex {
                dir: dir.to_path_buf(),
            };
            f.write("sessions", "");
            f.write("claude.json", "[]");
            f
        }

        pub fn module(&self) -> Rex {
            Rex::new(self.dir.join("rex"), self.dir.join("claude"))
        }

        pub fn write(&self, file: &str, contents: &str) {
            std::fs::write(self.dir.join(file), contents).unwrap();
        }

        /// The calls made so far, oldest first.
        pub fn calls(&self) -> Vec<String> {
            (std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default())
                .lines()
                .map(String::from)
                .collect()
        }

        /// One root record of Claude Code's, owned by `pid`, in `state` (and `kind`, `msg`
        /// when given), as Claude Code reports itself.
        pub fn claude_record(
            pid: i64,
            state: &str,
            kind: Option<&str>,
            msg: Option<&str>,
        ) -> String {
            let mut r = serde_json::json!({
                "app": "claude-code", "effective_app": "claude-code", "state": state,
                "owner": {"pid": pid, "name": "claude"},
                "updated_at": "2026-01-02T03:04:05Z",
            });
            if let Some(kind) = kind {
                r["kind"] = kind.into();
            }
            if let Some(msg) = msg {
                r["msg"] = msg.into();
            }
            r.to_string()
        }

        /// Rex's whole server from now on: `sessions`, each with its blocks as
        /// (block id, creator, `program_status` records joined as a JSON array's items).
        pub fn server(&self, sessions: &[(&str, &Blocks)]) {
            let ids: Vec<&str> = sessions.iter().map(|(s, _)| *s).collect();
            self.write("sessions", &lines(&ids));
            for (s, blocks) in sessions {
                let rows: Vec<String> = (blocks.iter())
                    .map(|(b, creator, _)| format!("{b} {creator}"))
                    .collect();
                self.write(&format!("{s}.blocks"), &lines(&rows));
                for (b, _, records) in blocks.iter() {
                    self.write(
                        &format!("{b}.json"),
                        &format!(r#"{{"records":[{records}]}}"#),
                    );
                }
            }
        }

        /// Rex's server from now on: `sessions`, each with its blocks in tabs as (block id,
        /// window, `program_status` records joined as a JSON array's items), all terminal
        /// blocks.
        pub fn tabs(&self, sessions: &[(&str, &Tabs)]) {
            let ids: Vec<&str> = sessions.iter().map(|(s, _)| *s).collect();
            self.write("sessions", &lines(&ids));
            for (s, blocks) in sessions {
                let rows: Vec<String> = (blocks.iter())
                    .map(|(b, _, _)| format!("{b} {}", super::TERMINAL_CREATOR))
                    .collect();
                self.write(&format!("{s}.blocks"), &lines(&rows));
                for (b, window, records) in blocks.iter() {
                    self.write(&format!("{b}.window"), window);
                    self.write(
                        &format!("{b}.json"),
                        &format!(r#"{{"records":[{records}]}}"#),
                    );
                }
            }
        }

        /// `block`'s foreground process from now on: `pid`, named `name`.
        pub fn foreground(&self, block: &str, pid: i64, name: &str) {
            let p = serde_json::json!({ "pid": pid, "user_id": 501, "name": name });
            let answer = serde_json::json!({ "foreground": p, "child": p });
            self.write(&format!("{block}.process"), &answer.to_string());
        }

        /// A root record as the dashboard on the fake's board (`board.db` in its folder,
        /// the key's board in these tests) reports itself, written by `pid` named `name`.
        pub fn dashboard_record(&self, pid: i64, name: &str) -> String {
            let app = super::dashboard_app(&self.dir.join("board.db"));
            Self::app_record(&app, serde_json::json!({"pid": pid, "name": name}))
        }

        /// A root record, idle, as `app`, owned by `owner`.
        pub fn app_record(app: &str, owner: serde_json::Value) -> String {
            serde_json::json!({
                "state": "idle", "app": app, "effective_app": app, "owner": owner,
                "updated_at": "2026-01-02T03:04:05Z",
            })
            .to_string()
        }

        /// `claude agents --json` from now on: each (name, pid) an interactive session.
        pub fn claude(&self, sessions: &[(&str, i64)]) {
            let list: Vec<serde_json::Value> = (sessions.iter())
                .map(|(name, pid)| {
                    serde_json::json!({
                        "id": format!("a{pid}"), "cwd": "/somewhere", "kind": "interactive",
                        "startedAt": 1, "sessionId": format!("sess-{pid}"), "name": name,
                        "pid": pid, "status": "idle",
                    })
                })
                .collect();
            self.write("claude.json", &serde_json::Value::Array(list).to_string());
        }
    }

    fn lines(rows: &[impl AsRef<str>]) -> String {
        rows.iter().map(|r| format!("{}\n", r.as_ref())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRex;
    use super::*;
    use crate::terminal::{Kind, Prompted, Sent};
    use crate::test_exe::executable;

    const TERM: &str = TERMINAL_CREATOR;

    fn agents(f: &FakeRex) -> Result<Vec<Agent>, Unread> {
        f.module()
            .agents(&f.dir.join("board.db"), crate::watch::TEST_WAIT)
    }

    fn agent(name: &str, status: Status, msg: Option<&str>) -> Agent {
        Agent {
            name: name.into(),
            status,
            msg: msg.map(String::from),
        }
    }

    /// `path_in` over an environment holding just `vars`.
    fn path_with(vars: &[(&str, &OsStr)]) -> PathBuf {
        path_in(|name| (vars.iter().find(|(k, _)| *k == name)).map(|(_, v)| v.to_os_string()))
    }

    #[test]
    fn a_claude_entry_whose_fields_are_not_the_types_listed_fails_no_other_worker() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let asks = Some("approve Bash: touch notes.txt");
        let blocked = FakeRex::claude_record(41, "blocked", Some("permission"), asks);
        let working = FakeRex::claude_record(42, "working", None, None);
        f.server(&[(
            "session:a",
            &[("block:1", TERM, &blocked), ("block:2", TERM, &working)],
        )]);
        let list = serde_json::json!([
            {"pid": 41, "kind": "interactive", "sessionId": "s1", "name": "dev-1",
             "status": 1, "waitingFor": {"x": 1}},
            // A name that is not a string: no one. A pid that is not an integer: joins no
            // record of Rex's.
            {"pid": 42, "kind": "interactive", "sessionId": "s2", "name": 7, "status": "busy"},
            {"pid": "41", "kind": "interactive", "sessionId": 3, "name": "dev-3"},
        ]);
        f.write("claude.json", &list.to_string());
        // Rex's record gives the state; Claude's odd fields fail nothing.
        assert_eq!(
            agents(&f).unwrap(),
            [agent(
                "dev-1",
                Status::Blocked(Some(Kind::Permission)),
                asks
            )]
        );
    }

    #[test]
    fn the_workers_are_claude_sessions_in_rex_by_their_root_record_named_by_claude() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let blocked = FakeRex::claude_record(
            41,
            "blocked",
            Some("permission"),
            Some("approve Bash: touch notes.txt"),
        );
        // dev-1 waits on a background shell: its root is working, its child blocked.
        let busy = format!(
            "{},{}",
            FakeRex::claude_record(42, "working", None, None),
            r#"{"id":"shell-1","app":"zsh","effective_app":"zsh","state":"blocked","kind":"question","owner":{"pid":43,"name":"zsh"},"updated_at":"2026-01-02T03:04:05Z"}"#
        );
        let other_app = r#"{"app":"cargo","effective_app":"cargo","state":"working","owner":{"pid":44,"name":"cargo"},"updated_at":"2026-01-02T03:04:05Z"}"#;
        let no_owner = r#"{"app":"claude-code","effective_app":"claude-code","state":"idle","updated_at":"2026-01-02T03:04:05Z"}"#;
        let unknown = FakeRex::claude_record(46, "error", None, None);
        let unlisted = FakeRex::claude_record(47, "blocked", Some("question"), None);
        let child_only = r#"{"id":"task","app":"claude-code","effective_app":"claude-code","state":"blocked","owner":{"pid":48,"name":"claude"},"updated_at":"2026-01-02T03:04:05Z"}"#;
        f.server(&[
            (
                "session:a",
                &[
                    ("block:1", TERM, &blocked),
                    ("block:2", TERM, &busy),
                    ("block:3", TERM, ""),
                    ("block:4", TERM, other_app),
                ],
            ),
            (
                "session:b",
                &[
                    ("block:5", TERM, no_owner),
                    ("block:6", TERM, &unknown),
                    ("block:7", TERM, &unlisted),
                    ("block:8", TERM, child_only),
                    ("block:9", "com.superlogical.viewer", "not records"),
                ],
            ),
            ("session:c", &[]),
        ]);
        f.claude(&[
            ("qa-1", 41),
            ("dev-1", 42),
            ("ghost", 44),
            ("dev-2", 46),
            ("x", 48),
        ]);
        assert_eq!(
            agents(&f).unwrap(),
            [
                agent(
                    "qa-1",
                    Status::Blocked(Some(Kind::Permission)),
                    Some("approve Bash: touch notes.txt")
                ),
                agent("dev-1", Status::Working, None),
                agent("dev-2", Status::Unknown, None),
            ]
        );
        // One call per session and per terminal block, then claude once; the viewer block
        // has no program_status and is never asked.
        let calls = f.calls();
        let methods: Vec<&str> = (calls.iter())
            .map(|c| c.split('|').nth(3).unwrap_or(c))
            .collect();
        assert_eq!(
            methods,
            [
                "session.list",
                "session.list_blocks",
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                "session.list_blocks",
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                "session.list_blocks",
                "claude|agents --json",
            ]
        );
        assert_eq!(
            calls[2],
            r#"--autostart=false|api|call|com.superlogical.terminal.program_status|{"args":{},"block_id":"block:1","session_id":"session:a"}"#
        );
    }

    /// The relay's call as the fake claude logs it: every argument fixed, no text.
    const RELAY_CALL: &str = "claude|-p -n control-tower --model haiku --tools \
                              SendMessage,ListAgents,ToolSearch --strict-mcp-config \
                              --no-session-persistence --output-format stream-json --verbose";

    fn prompt(f: &FakeRex, name: &str, text: &str) -> Prompted {
        let db = f.dir.join("board.db");
        f.module().prompt(&db, name, text, crate::watch::TEST_WAIT)
    }

    fn relays(f: &FakeRex) -> usize {
        f.calls()
            .iter()
            .filter(|c| c.starts_with("claude|-p"))
            .count()
    }

    #[test]
    fn a_text_reaches_a_worker_by_the_relay_whole_once_and_one_at_a_prompt_gets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let idle = FakeRex::claude_record(41, "idle", None, None);
        let asks = Some("approve Bash: touch notes.txt");
        let blocked = FakeRex::claude_record(42, "blocked", Some("permission"), asks);
        let twin_working = FakeRex::claude_record(43, "working", None, None);
        let twin_asked = FakeRex::claude_record(44, "blocked", Some("question"), None);
        f.server(&[(
            "session:a",
            &[
                ("block:1", TERM, &idle),
                ("block:2", TERM, &blocked),
                ("block:3", TERM, &twin_working),
                ("block:4", TERM, &twin_asked),
            ],
        )]);
        f.claude(&[("dev-1", 41), ("qa-1", 42), ("dev-2", 43), ("Dev-2", 44)]);
        let text = "control-tower: 1 post for you\n\n[T1 #1] lead: \"one\"\nand two";
        // Named in any case, the worker is reached by its own name, once, the text whole and
        // only on stdin.
        let got = prompt(&f, "DEV-1", text);
        assert_eq!(got.sent, Sent::Delivered, "{got:?}");
        let trace = got.trace.unwrap();
        assert!(
            trace.starts_with("relay to dev-1: exit 0, ") && trace.contains(", $0.0059, 3 turns;"),
            "{trace}"
        );
        assert!(trace.contains("the text verbatim"), "{trace}");
        let sent = std::fs::read_to_string(dir.path().join("prompts.log")).unwrap();
        assert_eq!(sent, format!("dev-1|{}\n", text.replace('\n', "\\n")));
        let calls = f.calls();
        assert_eq!(calls.last().unwrap(), RELAY_CALL);
        // The relay runs marked, so tower's own hooks in it do nothing.
        let env = std::fs::read_to_string(dir.path().join("relay.env")).unwrap();
        assert_eq!(env, "1\n");
        assert_eq!(relays(&f), 1);
        // At a prompt: nothing is sent, and why says what it asks. A twin at a prompt stops a
        // send to the name too.
        assert_eq!(
            prompt(&f, "qa-1", text).sent,
            Sent::Failed("at a permission prompt (approve Bash: touch notes.txt)".into())
        );
        assert_eq!(
            prompt(&f, "dev-2", text).sent,
            Sent::Failed("at a question".into())
        );
        // A name Rex has no Claude session for, and a Rex that cannot be read: nothing sent.
        assert_eq!(
            prompt(&f, "ghost", text).sent,
            Sent::Failed("Rex has no agent named ghost".into())
        );
        f.write("fail-session.list", "Error: server not running");
        assert_eq!(
            prompt(&f, "dev-1", text).sent,
            Sent::Failed("rex api call session.list exited 1: Error: server not running".into())
        );
        assert_eq!(relays(&f), 1);
    }

    #[test]
    fn a_send_the_relay_makes_elsewhere_or_not_at_all_fails_and_one_cut_off_is_unsure() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 41)]);
        f.write("relay-to-dev-1", "qa-7");
        assert_eq!(
            prompt(&f, "dev-1", "x").sent,
            Sent::Misrouted("the relay sent it to \"qa-7\", not to dev-1".into())
        );
        std::fs::remove_file(dir.path().join("relay-to-dev-1")).unwrap();
        f.write("relay-none-dev-1", "I could not find that session.");
        assert_eq!(
            prompt(&f, "dev-1", "x").sent,
            Sent::Failed(
                "the relay sent nothing; it said: \"I could not find that session.\"".into()
            )
        );
        std::fs::remove_file(dir.path().join("relay-none-dev-1")).unwrap();
        f.write("relay-refuse-dev-1", "No agent named 'dev-1' is reachable.");
        assert_eq!(
            prompt(&f, "dev-1", "x").sent,
            Sent::Failed(
                "the relay could not send it: \"No agent named 'dev-1' is reachable.\"".into()
            )
        );
        std::fs::remove_file(dir.path().join("relay-refuse-dev-1")).unwrap();
        f.write("relay-quiet-dev-1", "");
        assert_eq!(
            prompt(&f, "dev-1", "x").sent,
            Sent::Unsure("the relay's send had no answer; it said: \"relay crashed\"".into())
        );
        std::fs::remove_file(dir.path().join("relay-quiet-dev-1")).unwrap();
        // A relay past the limit is stopped: it may have sent, so Unsure.
        f.write("hold", "");
        let db = dir.path().join("board.db");
        let got = f.module().prompt(&db, "dev-1", "x", Duration::from_secs(2));
        assert!(
            matches!(&got.sent, Sent::Unsure(why) if why.starts_with("the relay did not finish")),
            "{got:?}"
        );
        assert!(!dir.path().join("ended").exists());
        // Its absence fails the send.
        std::fs::remove_file(dir.path().join("hold")).unwrap();
        std::fs::remove_file(dir.path().join("claude")).unwrap();
        let rex = Rex::new(dir.path().join("rex"), dir.path().join("claude"));
        let got = rex.prompt(&db, "dev-1", "x", crate::watch::TEST_WAIT);
        assert!(
            matches!(&got.sent, Sent::Failed(why) if why.contains("cannot run")),
            "{got:?}"
        );
    }

    #[test]
    fn a_rex_with_no_claude_is_no_workers_and_claude_is_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        assert_eq!(agents(&f).unwrap(), []);
        f.server(&[("session:a", &[("block:1", TERM, "")])]);
        assert_eq!(agents(&f).unwrap(), []);
        assert!(!f.calls().iter().any(|c| c.starts_with("claude")));
        // Claude, but Claude lists none of their pids: no one.
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 99)]);
        assert_eq!(agents(&f).unwrap(), []);
    }

    #[test]
    fn any_call_that_fails_fails_the_whole_look_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 41)]);
        assert_eq!(agents(&f).unwrap().len(), 1);
        let failed = |f: &FakeRex| match agents(f) {
            Err(Unread::Failed(why)) => why,
            other => panic!("{other:?}"),
        };
        for (it, method) in [
            ("session.list", "session.list"),
            ("session:a", "session.list_blocks"),
            ("block:1", PROGRAM_STATUS),
        ] {
            f.write(&format!("fail-{it}"), "Error: no such thing");
            assert_eq!(
                failed(&f),
                format!("rex api call {method} exited 1: Error: no such thing")
            );
            std::fs::remove_file(dir.path().join(format!("fail-{it}"))).unwrap();
        }
        f.write("block:1.json", "not json");
        assert!(
            failed(&f).starts_with(&format!(
                "rex api call {PROGRAM_STATUS} said something unexpected"
            )),
            "{}",
            failed(&f)
        );
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.write("claude.json", "not json");
        assert!(failed(&f).starts_with("claude agents --json said something unexpected"));
        std::fs::remove_file(dir.path().join("claude")).unwrap();
        assert!(failed(&f).contains("cannot run"), "{}", failed(&f));
        std::fs::remove_file(dir.path().join("rex")).unwrap();
        assert!(failed(&f).contains("cannot run"), "{}", failed(&f));
    }

    #[test]
    fn a_look_past_its_time_limit_is_stopped_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rex");
        executable(&exe, "#!/bin/sh\nexec sleep 5\n");
        let rex = Rex::new(exe, dir.path().join("claude"));
        let began = Instant::now();
        let got = rex.agents(&dir.path().join("board.db"), Duration::from_secs(1));
        assert_eq!(
            got,
            Err(Unread::Failed(
                "Rex did not answer within 1 s and the look was stopped".into()
            ))
        );
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn the_time_limit_bounds_the_whole_look_not_each_call() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 41)]);
        // Each rex call takes 0.6 s: a look is three (sessions, blocks, the block's status),
        // so each fits in 1 s but the three do not.
        let slow = dir.path().join("slow-rex");
        executable(
            &slow,
            "#!/bin/sh\nsleep 0.6\nexec \"$(dirname \"$0\")/rex\" \"$@\"\n",
        );
        let rex = Rex::new(slow, dir.path().join("claude"));
        let db = dir.path().join("board.db");
        let roomy = rex.agents(&db, crate::watch::TEST_WAIT).unwrap();
        assert_eq!(roomy, [agent("dev-1", Status::Idle, None)]);
        let began = Instant::now();
        assert_eq!(
            rex.agents(&db, Duration::from_secs(1)),
            Err(Unread::Failed(
                "Rex did not answer within 1 s and the look was stopped".into()
            ))
        );
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_session_without_rex_session_is_told_it_runs_outside_rex() {
        let with = |vars: &[(&str, &str)]| {
            outside(|name| (vars.iter().find(|(k, _)| *k == name)).map(|(_, v)| v.into()))
        };
        // Inside Rex: nothing, whatever else is set.
        assert_eq!(with(&[("REX_SESSION", "session:7")]), None);
        // Unset, or empty: told, naming what the board cannot see and whom to ask.
        let told = with(&[("REX_BIN_DIR", "/opt/rex")]).unwrap();
        assert_eq!(with(&[("REX_SESSION", "")]).as_deref(), Some(told.as_str()));
        assert!(
            told.starts_with(
                "control-tower: this session runs outside Rex (REX_SESSION is not set)"
            ) && told.contains("not their prompts, and posts and nudges are not delivered")
                && told.ends_with("question: post to @manager."),
            "{told}"
        );
    }

    #[test]
    fn rex_is_tower_rex_else_in_rex_bin_dir_else_on_the_path() {
        let bin = tempfile::tempdir().unwrap();
        let dir = bin.path().as_os_str();
        let in_bin = bin.path().join("rex");
        // No rex in REX_BIN_DIR yet: rex on the PATH.
        assert_eq!(path_with(&[("REX_BIN_DIR", dir)]), PathBuf::from("rex"));
        executable(&in_bin, "#!/bin/sh\n");
        assert_eq!(path_with(&[("REX_BIN_DIR", dir)]), in_bin);
        // TOWER_REX first; an empty one counts as unset.
        let tower_rex = ("TOWER_REX", OsStr::new("/opt/x/rex"));
        assert_eq!(
            path_with(&[tower_rex, ("REX_BIN_DIR", dir)]),
            PathBuf::from("/opt/x/rex")
        );
        let empty = ("TOWER_REX", OsStr::new(""));
        assert_eq!(path_with(&[empty, ("REX_BIN_DIR", dir)]), in_bin);
        // Neither, or an empty REX_BIN_DIR: rex on the PATH.
        assert_eq!(path_with(&[]), PathBuf::from("rex"));
        let empty_dir = ("REX_BIN_DIR", OsStr::new(""));
        assert_eq!(path_with(&[empty_dir]), PathBuf::from("rex"));
        // A relative REX_BIN_DIR is none, even one that reaches that rex from here: it would
        // name another file wherever the dashboard runs.
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative = format!(
            "{up}{}",
            bin.path().to_str().unwrap().trim_start_matches('/')
        );
        assert!(
            Path::new(&relative).join("rex").is_file(),
            "{relative} reaches the rex"
        );
        let relative_dir = ("REX_BIN_DIR", OsStr::new(&relative));
        assert_eq!(path_with(&[relative_dir]), PathBuf::from("rex"));
        // The not-found line names what was tried and how to link it.
        let none = Rex::new("rex".into(), "claude".into());
        assert_eq!(
            none.missing(Some(OsStr::new("/nowhere"))).unwrap(),
            "rex not found: tried rex on PATH /nowhere; Rex keeps it in REX_BIN_DIR, which Rex \
             sets in its own tabs: link it into Homebrew's bin (ln -s \"$REX_BIN_DIR/rex\" \
             \"$(brew --prefix)/bin/rex\"), or set TOWER_REX to its full path"
        );
        assert_eq!(Rex::new(in_bin, "claude".into()).missing(None), None);
    }

    #[test]
    fn a_started_lead_is_its_new_sessions_id_and_undo_kills_that_session() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rex");
        executable(
            &exe,
            "#!/bin/sh\nd=$(dirname \"$0\")\n(IFS='|'; echo \"$*\") >> \"$d/calls.log\"\n\
             [ -e \"$d/fail\" ] && { echo 'Error: session target \"session:7\" not found' >&2; \
             exit 3; }\n[ \"$1\" = new ] && echo '{\"session_id\":\"session:7\"}'\nexit 0\n",
        );
        let rex = Rex::new(exe, "claude".into());
        let db = dir.path().join("board.db");
        let command = ["claude", "-n", "Comics Lead", "/control-tower:lead M2"].map(String::from);
        let lead = LeadStart {
            name: "Comics Lead",
            dir: "/srv/shop",
            board: "/b/board.db",
            command: &command,
        };
        let handle = rex.start_lead(&db, &lead).unwrap();
        assert_eq!(handle, Handle("session:7".into()));
        assert_eq!(
            rex.undo(&db, &handle).unwrap(),
            "killed Rex session session:7"
        );
        let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "new|Comics Lead|-c|/srv/shop|--json|--|env|TOWER_DB=/b/board.db|claude|-n|\
                 Comics Lead|/control-tower:lead M2",
                "kill|session:7",
            ]
        );
        // A session Rex no longer has: undo says so.
        std::fs::write(dir.path().join("fail"), "").unwrap();
        assert_eq!(
            format!("{:#}", rex.undo(&db, &handle).unwrap_err()),
            "rex kill exited 3: Error: session target \"session:7\" not found"
        );
    }

    fn here(session: &str, block: &str) -> Here {
        Here {
            session: session.into(),
            block: block.into(),
        }
    }

    fn place(session: &str, window: &str) -> Place {
        Place {
            session_id: session.into(),
            window_id: window.into(),
        }
    }

    /// The dashboard's command the key is given to open.
    fn dash() -> Vec<String> {
        ["env", "TOWER_DB=/b/board.db", "/bin/tower", "ui"]
            .map(String::from)
            .to_vec()
    }

    fn back(f: &FakeRex) -> PathBuf {
        f.dir.join("board.space-back")
    }

    /// The key pressed `at`, with its back file at `back`.
    fn answer(f: &FakeRex, at: &Here, back: &Path) -> Result<Answer> {
        f.module().space(&f.dir.join("board.db"), at, back, &dash())
    }

    /// What the key pressed `at` shows, having passed nothing over.
    fn space(f: &FakeRex, at: &Here) -> Result<Shown> {
        let answer = answer(f, at, &back(f))?;
        assert_eq!(answer.passed_over, Vec::<String>::new());
        Ok(answer.shown)
    }

    /// The input of each call to `method` so far, oldest first.
    fn inputs(f: &FakeRex, method: &str) -> Vec<serde_json::Value> {
        (f.calls().iter())
            .filter_map(|c| {
                let words: Vec<&str> = c.split('|').collect();
                (words.get(3) == Some(&method)).then(|| serde_json::from_str(words[4]).unwrap())
            })
            .collect()
    }

    #[test]
    fn the_dashboard_is_the_tab_whose_live_tower_named_itself_not_one_labelled_so() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let stale = f.dashboard_record(51, "tower");
        let claude = FakeRex::claude_record(52, "idle", None, None);
        let reused = f.dashboard_record(53, "tower");
        let live = f.dashboard_record(60, "tower");
        f.tabs(&[
            (
                "session:a",
                &[
                    ("block:1", "window:1", ""),
                    ("block:2", "window:2", ""),
                    ("block:3", "window:3", &stale),
                    ("block:4", "window:4", &claude),
                    ("block:5", "window:5", &reused),
                ],
            ),
            ("session:b", &[("block:6", "window:6", &live)]),
        ]);
        // A tab labelled as the dashboard, with no record, is not it.
        f.write("block:2.label", "control-tower");
        // The record outlived its dashboard: the shell is back in the foreground.
        f.foreground("block:3", 99, "zsh");
        f.foreground("block:4", 52, "claude");
        // The dashboard's pid now names another program.
        f.foreground("block:5", 53, "vim");
        f.foreground("block:6", 60, "tower");
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:b", "window:6"))
        );
        // Where the key was pressed is kept for the way back.
        assert_eq!(
            std::fs::read_to_string(back(&f)).unwrap(),
            "{\"session_id\":\"session:a\",\"window_id\":\"window:1\"}\n"
        );
        // Only blocks with the dashboard's record are asked for their processes; nothing
        // is opened.
        let asked: Vec<serde_json::Value> = (inputs(&f, PROCESS).into_iter())
            .map(|i| i["block_id"].clone())
            .collect();
        assert_eq!(asked, ["block:3", "block:5", "block:6"]);
        assert!(inputs(&f, "session.new_window").is_empty());
    }

    #[test]
    fn the_keys_own_session_is_looked_in_first() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let one = f.dashboard_record(60, "tower");
        let two = f.dashboard_record(61, "tower");
        f.tabs(&[
            (
                "session:a",
                &[("block:1", "window:1", ""), ("block:2", "window:2", &one)],
            ),
            (
                "session:b",
                &[("block:5", "window:5", ""), ("block:6", "window:6", &two)],
            ),
        ]);
        f.foreground("block:2", 60, "tower");
        f.foreground("block:6", 61, "tower");
        assert_eq!(
            space(&f, &here("session:b", "block:5")).unwrap(),
            Shown::Go(place("session:b", "window:6"))
        );
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:a", "window:2"))
        );
    }

    #[test]
    fn from_the_dashboards_tab_the_key_goes_back_to_where_it_was_last_pressed() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let live = f.dashboard_record(60, "tower");
        let server = |first: &str, window: &str| {
            f.tabs(&[
                ("session:a", &[(first, window, "")]),
                (
                    "session:b",
                    &[("block:6", "window:6", &live), ("block:7", "window:6", "")],
                ),
            ]);
            f.foreground("block:6", 60, "tower");
        };
        server("block:1", "window:1");
        // Never pressed outside the dashboard: nothing moves.
        let Shown::Stay(why) = space(&f, &here("session:b", "block:6")).unwrap() else {
            panic!("the key moved with no place to go back to");
        };
        assert_eq!(
            why,
            "no place to go back to: the key has not been pressed outside the dashboard"
        );
        assert!(!back(&f).exists());
        let dashboard = Shown::Go(place("session:b", "window:6"));
        assert_eq!(space(&f, &here("session:a", "block:1")).unwrap(), dashboard);
        // From the dashboard, or a pane beside it in its tab, back; and again.
        let pressed = Shown::Go(place("session:a", "window:1"));
        assert_eq!(space(&f, &here("session:b", "block:6")).unwrap(), pressed);
        assert_eq!(space(&f, &here("session:b", "block:7")).unwrap(), pressed);
        // The tab it was pressed in is closed: nothing moves, saying why.
        server("block:8", "window:8");
        let Shown::Stay(why) = space(&f, &here("session:b", "block:6")).unwrap() else {
            panic!("the key went to a tab Rex no longer has");
        };
        assert_eq!(
            why,
            "the place the key was last pressed outside the dashboard (window window:1 in \
             session session:a) is gone"
        );
        // A back file that is not a place: nothing moves, naming it.
        f.write("board.space-back", "window:1");
        let Shown::Stay(why) = space(&f, &here("session:b", "block:6")).unwrap() else {
            panic!("the key read a place from a broken back file");
        };
        let named = format!("the back file {} is not a place (", back(&f).display());
        assert!(why.starts_with(&named), "{why}");
    }

    #[test]
    fn with_no_dashboard_the_key_opens_one_in_its_own_session_and_shows_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let stale = f.dashboard_record(51, "tower");
        f.tabs(&[
            ("session:a", &[("block:1", "window:1", "")]),
            ("session:b", &[("block:6", "window:6", &stale)]),
        ]);
        f.foreground("block:6", 99, "zsh");
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:a", "window:new"))
        );
        assert_eq!(
            inputs(&f, "session.new_window"),
            [serde_json::json!({
                "session_id": "session:a",
                "window_label": "control-tower",
                "layout": {"block": {
                    "flavor": "com.superlogical.terminal.shell",
                    "label": "control-tower",
                    "options": {"command": dash()},
                }},
            })]
        );
        assert_eq!(
            std::fs::read_to_string(back(&f)).unwrap(),
            "{\"session_id\":\"session:a\",\"window_id\":\"window:1\"}\n"
        );
    }

    #[test]
    fn the_key_fails_saying_why_when_rex_cannot_say_where_it_was_pressed() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let live = f.dashboard_record(60, "tower");
        f.tabs(&[(
            "session:a",
            &[("block:1", "window:1", ""), ("block:6", "window:6", &live)],
        )]);
        f.foreground("block:6", 60, "tower");
        let err = |at: &Here| format!("{:#}", space(&f, at).unwrap_err());
        assert_eq!(
            err(&here("session:z", "block:1")),
            "Rex has no session session:z"
        );
        assert_eq!(
            err(&here("session:a", "block:9")),
            "Rex has no block block:9 in a tab of session session:a"
        );
        // A call about the key's own block or session that fails fails the key with Rex's
        // reason.
        f.write("fail-block:1", "Error: block not found");
        assert_eq!(
            err(&here("session:a", "block:1")),
            format!("rex api call {PROGRAM_STATUS} exited 1: Error: block not found")
        );
        std::fs::remove_file(f.dir.join("fail-block:1")).unwrap();
        f.write("fail-session:a", "Error: session busy");
        assert_eq!(
            err(&here("session:a", "block:1")),
            "rex api call session.list_blocks exited 1: Error: session busy"
        );
        // None of these wrote a back file or opened a tab.
        assert!(!back(&f).exists());
        assert!(inputs(&f, "session.new_window").is_empty());
    }

    #[test]
    fn a_block_or_session_rex_cannot_read_as_the_key_looks_is_passed_over_saying_so() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let closing = f.dashboard_record(50, "tower");
        let live = f.dashboard_record(60, "tower");
        f.tabs(&[
            (
                "session:a",
                &[
                    ("block:1", "window:1", ""),
                    ("block:2", "window:2", &closing),
                ],
            ),
            ("session:c", &[("block:3", "window:3", "")]),
            ("session:b", &[("block:6", "window:6", &live)]),
        ]);
        f.foreground("block:6", 60, "tower");
        // A tab closing mid-look, and a session Rex cannot list.
        f.write("fail-block:2", "Error: block not found");
        f.write("fail-session:c", "Error: session gone");
        let pressed = answer(&f, &here("session:a", "block:1"), &back(&f)).unwrap();
        assert_eq!(pressed.shown, Shown::Go(place("session:b", "window:6")));
        assert_eq!(
            pressed.passed_over,
            [
                format!(
                    "passed over block block:2 in session session:a, which Rex could not \
                     read: rex api call {PROGRAM_STATUS} exited 1: Error: block not found"
                ),
                "passed over session session:c, which Rex could not list: rex api call \
                 session.list_blocks exited 1: Error: session gone"
                    .into(),
            ]
        );
        // A back place in the session passed over cannot be read; it is not gone.
        f.write(
            "board.space-back",
            r#"{"session_id":"session:c","window_id":"window:3"}"#,
        );
        let from_dashboard = answer(&f, &here("session:b", "block:6"), &back(&f)).unwrap();
        assert_eq!(
            from_dashboard.shown,
            Shown::Stay(
                "the place the key was last pressed outside the dashboard (window window:3 in \
                 session session:c) cannot be read: Rex could not list its session"
                    .into()
            )
        );
    }

    #[test]
    fn the_back_files_folder_is_made_when_there_is_none_yet() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let live = f.dashboard_record(60, "tower");
        f.tabs(&[(
            "session:a",
            &[("block:1", "window:1", ""), ("block:6", "window:6", &live)],
        )]);
        f.foreground("block:6", 60, "tower");
        let back = f.dir.join("state/control-tower/board.space-back");
        let answer = answer(&f, &here("session:a", "block:1"), &back).unwrap();
        assert_eq!(answer.shown, Shown::Go(place("session:a", "window:6")));
        assert_eq!(
            std::fs::read_to_string(&back).unwrap(),
            "{\"session_id\":\"session:a\",\"window_id\":\"window:1\"}\n"
        );
    }

    #[test]
    fn the_popup_opens_over_the_keys_tab_at_85_percent_focused() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        f.tabs(&[(
            "session:a",
            &[("block:1", "window:1", ""), ("block:2", "window:2", "")],
        )]);
        let popup = [
            "env",
            "TOWER_UI_MODE=popup",
            "TOWER_DB=/b/board.db",
            "/bin/tower",
            "ui",
        ]
        .map(String::from);
        let db = f.dir.join("board.db");
        f.module()
            .popup(&db, &here("session:a", "block:2"), &popup)
            .unwrap();
        assert_eq!(
            inputs(&f, "session.new_layer"),
            [serde_json::json!({
                "session_id": "session:a",
                "window_id": "window:2",
                "bounds": [0.075, 0.075, 0.85, 0.85],
                "focus": true,
                "layout": {"block": {
                    "flavor": "com.superlogical.terminal.shell",
                    "label": "control-tower",
                    "options": {"command": popup},
                }},
            })]
        );
        // A block Rex does not have: no popup, saying why.
        let err = f.module().popup(&db, &here("session:a", "block:9"), &popup);
        assert_eq!(
            format!("{:#}", err.unwrap_err()),
            "Rex has no block block:9 in a tab of session session:a"
        );
        assert_eq!(inputs(&f, "session.new_layer").len(), 1);
    }

    #[test]
    fn the_dashboard_names_itself_and_its_board_only_in_rex() {
        let board = Path::new("/nowhere/board.db");
        let status = |v: Option<&str>| {
            let env = |n: &str| (n == "REX_BLOCK").then(|| v.map(OsString::from)).flatten();
            dashboard_status(env, board)
        };
        // The first 8 hex digits of SHA-256("/nowhere/board.db"), a board not made yet.
        assert_eq!(
            status(Some("block:1")),
            Some((
                "\x1b]7501;state=idle:app=control-tower-fbc1594c\x1b\\".into(),
                "\x1b]7501;state=clear\x1b\\"
            ))
        );
        assert_eq!(status(Some("")), None);
        assert_eq!(status(None), None);
    }

    #[test]
    fn a_board_is_named_by_its_canonical_path_within_rexs_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("state");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("board.db"), "").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let app = dashboard_app(&real.join("board.db"));
        // Reached through a link, or with a `..` in it, it is the same board.
        assert_eq!(dashboard_app(&link.join("board.db")), app);
        assert_eq!(dashboard_app(&real.join("../state/board.db")), app);
        // Another board, another name.
        assert_ne!(dashboard_app(&real.join("other.db")), app);
        assert!(
            app.starts_with("control-tower-") && app.len() == 22,
            "{app}"
        );
        let hex = &app["control-tower-".len()..];
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{app}"
        );
    }

    #[test]
    fn a_dashboard_of_another_board_is_passed_by_and_the_key_opens_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let elsewhere = dashboard_app(&f.dir.join("try.db"));
        let other =
            FakeRex::app_record(&elsewhere, serde_json::json!({"pid": 60, "name": "tower"}));
        // A tower older than the board's name in its record.
        let older = FakeRex::app_record(
            "control-tower",
            serde_json::json!({"pid": 61, "name": "tower"}),
        );
        f.tabs(&[(
            "session:a",
            &[
                ("block:1", "window:1", ""),
                ("block:2", "window:2", &other),
                ("block:3", "window:3", &older),
            ],
        )]);
        f.foreground("block:2", 60, "tower");
        f.foreground("block:3", 61, "tower");
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:a", "window:new"))
        );
        assert_eq!(inputs(&f, "session.new_window").len(), 1);
    }

    #[test]
    fn a_record_whose_owner_has_no_name_is_taken_on_its_pid() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let app = dashboard_app(&f.dir.join("board.db"));
        let nameless = FakeRex::app_record(&app, serde_json::json!({"pid": 60}));
        f.tabs(&[(
            "session:a",
            &[
                ("block:1", "window:1", ""),
                ("block:6", "window:6", &nameless),
            ],
        )]);
        f.foreground("block:6", 60, "tower");
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:a", "window:6"))
        );
    }

    #[test]
    fn a_key_pressed_in_any_of_two_dashboards_goes_back() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let (one, two) = (
            f.dashboard_record(60, "tower"),
            f.dashboard_record(61, "tower"),
        );
        f.tabs(&[(
            "session:a",
            &[
                ("block:1", "window:1", ""),
                ("block:2", "window:2", &one),
                ("block:3", "window:3", &two),
            ],
        )]);
        f.foreground("block:2", 60, "tower");
        f.foreground("block:3", 61, "tower");
        assert_eq!(
            space(&f, &here("session:a", "block:1")).unwrap(),
            Shown::Go(place("session:a", "window:2"))
        );
        // From the second, back to where it was pressed, not on to the first; the back file
        // stays as it was.
        let pressed = Shown::Go(place("session:a", "window:1"));
        assert_eq!(space(&f, &here("session:a", "block:3")).unwrap(), pressed);
        assert_eq!(space(&f, &here("session:a", "block:2")).unwrap(), pressed);
        assert_eq!(
            std::fs::read_to_string(back(&f)).unwrap(),
            "{\"session_id\":\"session:a\",\"window_id\":\"window:1\"}\n"
        );
    }

    #[test]
    fn a_look_out_of_time_fails_the_key_instead_of_passing_over() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let live = f.dashboard_record(60, "tower");
        f.tabs(&[
            (
                "session:a",
                &[("block:1", "window:1", ""), ("block:2", "window:2", &live)],
            ),
            ("session:b", &[("block:6", "window:6", &live)]),
        ]);
        f.foreground("block:6", 60, "tower");
        // Rex answers about block:2 only after the look's second is over.
        f.write("slow-block:2", "3");
        let mut passed_over = Vec::new();
        let (key, back_file, command) = (here("session:a", "block:1"), back(&f), dash());
        let asked = (&key, back_file.as_path(), command.as_slice());
        let db = f.dir.join("board.db");
        let err = (f.module())
            .shown(&db, asked, Duration::from_secs(1), &mut passed_over)
            .unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "Rex did not answer within 1 s and the look was stopped"
        );
        // Nothing passed over as if it were one tab closing, no back file, no tab.
        assert_eq!(passed_over, Vec::<String>::new());
        assert!(!back(&f).exists());
        assert!(inputs(&f, "session.new_window").is_empty());
    }
}
