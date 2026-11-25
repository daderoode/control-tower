//! The stuck-worker watch, `tower watch`: a long-running process that every
//! `TICK` reads the board and the terminal's live workers (the terminal module, `terminal`:
//! Rex) and flags a stuck worker by the spec's fixed rules (section 4, "While work runs").
//! It runs on its own, dashboard open or not.
//!
//! A worker is linked to its live Claude session in the terminal by name: a task's assignee
//! is the Claude session name of the worker on it, compared ignoring ASCII case. Only
//! tasks under a milestone with a lead are watched, since a flag is a `system` message on
//! that lead's board; standalone tasks and unassigned ones are not.
//!
//! A task's team is its assignee and every name its threads carry: each author of a
//! post in them and each name a post @mentions, but a lead's or the owner's. A worker on
//! several teams starts a prompt episode on one: the task it is assigned to, else the one
//! whose threads it last posted in, else the one whose threads last mentioned it (a member
//! named before its first post). The episode stays on that task while the worker waits and
//! is on its team, so one prompt is one need-you item.
//!
//! The signals, per watched task that is not Done:
//! - Waiting on a prompt: the terminal shows a member of its team `blocked`. A need-you item
//!   at once: a `system` message mentioning `@manager` in the task's thread, one per member,
//!   quoting what the prompt asks when the terminal says (`asks`).
//!   When the member leaves the prompt, a plain note says it was answered; the need-you
//!   item itself stays until the thread is resolved (spec 3a). The rest are the assignee's
//!   alone.
//! - Says it is stuck: the worker's own `blocked:` post; nothing for the watch, which also
//!   leaves a task flagged blocked out of the two nudges below.
//! - Stopped without reporting: the worker is `idle` or `done`, no post for `STOPPED_AFTER`.
//! - Silent while working: the task is In progress, the worker `working`, no post for
//!   `SILENT_AFTER`.
//!   Both nudge the worker directly (the terminal's prompt; in Rex, a message from
//!   `@control-tower` through the relay, never to a worker at a prompt) and record the nudge
//!   as a plain `system` message in the task's thread (saying so when the terminal refused
//!   delivery, or could not confirm it; a nudge sent to someone else is recorded as an item
//!   for the owner naming who got it), the relay's trace in the watch's log;
//!   if no post comes within `ANSWER_WITHIN`, a `system` message tells the lead.
//! - Orphaned: the task is In progress and the terminal's worker list, not empty, has had no
//!   live Claude session by the worker's name for `ORPHANED_AFTER` looks in a row. A
//!   `system` message to the lead. An empty list (the terminal restarting, or no team
//!   running) orphans no one; a terminal that cannot be read skips the look.
//!
//! "No post" counts every post in the task's threads but the watch's own; a task with none
//! counts from when the watch first saw it with that worker, kept on the board
//! (`watch_seen`) so a restart does not reset it. The watch sees the local Rex server.
//! Several live sessions can share a name (Claude does not keep names unique, and names
//! are compared ignoring case): they are one worker, whose status is the busiest of theirs
//! (`worker_agent`), and a prompt is raised when any of them is blocked. Each flag is
//! remembered on the board (`watch_flag`), so it is raised once per episode, never every
//! tick, and raised again only after the episode ends: the worker leaves the prompt or the
//! team, a post arrives after a nudge, the worker is back in the terminal, or the task is
//! Done or moved.
//! Reassigning a task ends the old assignee's nudge and orphan episodes; a prompt goes on
//! while the old assignee is still on the team.
//!
//! The decision (`decide`) is pure and tested with fixtures; `Watch::tick` reads the board
//! and the terminal (through the terminal module, `terminal`) and carries the actions out.
//!
//! The watch restarts itself after an upgrade: brew does not restart a service on
//! `brew upgrade`, it repoints `opt` to a new Cellar directory. The watch keeps the path it
//! was started from (the `opt` symlink, not where it resolves) and the file it resolved to;
//! between looks, when that path resolves to another file, it execs the same path with the
//! same arguments, so launchd keeps the PID. The decision (`restart_due`) is pure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::board::{
    DELIVERY_FAILURE, OWNER_AUTHOR, OWNER_HANDLE, QUESTION, Stage, TaskId, closed_worker_hint,
};
use crate::store::{FlagChange, Store};
use crate::terminal::{Agent, SEND_TIMEOUT, Sent, Status, Terminal, terminal_timeout};

/// How often the watch looks.
pub const TICK: Duration = Duration::from_secs(20);
/// No post this long from an idle or finished worker is "stopped without reporting".
pub const STOPPED_AFTER: i64 = 5 * 60;
/// No post this long from a working worker on an In progress task is "silent while working".
pub const SILENT_AFTER: i64 = 30 * 60;
/// How long a nudged worker has to post before the lead is told.
pub const ANSWER_WITHIN: i64 = 2 * 60;
/// Looks in a row a worker must be missing from a non-empty list of the terminal's workers
/// to be orphaned, so a session restarting or a name changing hands is not flagged.
pub const ORPHANED_AFTER: u32 = 3;
/// The most characters of what a prompt asks that its need-you item quotes.
pub const ASKS_CHARS: usize = 200;
/// The longest a unit test waits for something it is waiting on (a fake's answer, an event,
/// a hold released): long enough that a busy machine never fails a test that would pass,
/// short enough that a broken test still ends.
#[cfg(test)]
pub(crate) const TEST_WAIT: Duration = Duration::from_secs(60);

/// A fake's shell loop that holds until the file `$d/release` exists, at most `TEST_WAIT`,
/// so a test checks what happens while a call is in flight by the event, not a clock.
#[cfg(test)]
pub(crate) fn hold_until_released() -> String {
    format!(
        "i=0; while [ ! -e \"$d/release\" ] && [ $i -lt {} ]; do sleep 0.1; i=$((i+1)); done",
        TEST_WAIT.as_secs() * 10
    )
}
/// How often, while waiting between looks, the watch checks whether it was upgraded.
const UPGRADE_CHECK: Duration = Duration::from_secs(1);
/// How long after a failed restart the watch tries again.
const RETRY_RESTART: Duration = Duration::from_secs(60);

/// One of the spec's stuck-worker signals the watch raises (the worker's own `blocked:`
/// post is not one: the watch leaves that to the worker).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Prompt,
    Stopped,
    Silent,
    Orphaned,
}

impl Signal {
    pub const ALL: [Signal; 4] = [
        Signal::Prompt,
        Signal::Stopped,
        Signal::Silent,
        Signal::Orphaned,
    ];

    /// Its name on the board.
    pub fn name(self) -> &'static str {
        match self {
            Signal::Prompt => "prompt",
            Signal::Stopped => "stopped",
            Signal::Silent => "silent",
            Signal::Orphaned => "orphaned",
        }
    }

    /// Stopped and silent are answered by a nudge first; one episode of either at a time.
    fn nudges(self) -> bool {
        matches!(self, Signal::Stopped | Signal::Silent)
    }
}

/// A flag the watch raised on a task, remembered until its episode ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub task: TaskId,
    pub signal: Signal,
    /// The worker it was raised for: the assignee, or for a prompt any member of the team.
    pub worker: String,
    /// The task's latest post when it was raised, `None` for none: a later post ends a
    /// stopped or silent episode.
    pub quiet_since: Option<i64>,
    /// When it was raised: for stopped and silent, when the worker was nudged.
    pub raised_at: i64,
    /// When an unanswered nudge was passed to the lead.
    pub lead_told_at: Option<i64>,
}

/// A name a task's threads carry, as last written: when it last posted in them and when a
/// post last @mentioned it, `None` for never.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub posted: Option<i64>,
    pub mentioned: Option<i64>,
}

/// A task the watch looks at: assigned, not Done, under a milestone with a lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub task: TaskId,
    pub stage: Stage,
    /// Its assignee, the worker's Claude session name.
    pub worker: String,
    /// The names its threads carry: each author of a post in them and each name a post
    /// @mentions, but `system`, any lead's session and the owner. With the assignee, its
    /// team.
    pub named: Vec<Named>,
    /// Its lead's session, addressed in flags to the lead.
    pub lead: String,
    /// It carries the blocked flag: the worker said it is stuck.
    pub blocked: bool,
    /// Its latest post but the watch's own, if any.
    pub last_post: Option<i64>,
    /// When the watch first saw it with this worker (kept on the board, so a restart does
    /// not reset it): "no post" counts from here when it has none.
    pub seen_since: i64,
    /// How many looks in a row the terminal listed workers but none by the worker's name.
    pub absent: u32,
}

/// What the watch does about one flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Prompt the worker (`agent`, its name as the terminal lists it) with `text` and
    /// remember `flag`.
    Nudge {
        flag: Flag,
        agent: String,
        text: String,
    },
    /// Post `text` as `system` in the task's thread and remember `flag`.
    Raise { flag: Flag, text: String },
    /// Post `text` to the lead in the task's thread: a nudge to `worker` went unanswered.
    TellLead {
        task: TaskId,
        signal: Signal,
        worker: String,
        text: String,
    },
    /// Forget `worker`'s flag whose episode ended, posting `note` (plain) in the task's
    /// thread.
    Clear {
        task: TaskId,
        signal: Signal,
        worker: String,
        note: Option<String>,
    },
}

fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The live worker `name` (ignoring case) as every agent by that name gives it, `None` when
/// there is none: named as the first is listed, with the busiest status of them all, blocked
/// over working over unknown over idle or done. Claude does not keep session names unique,
/// so two sessions can share one: a stale twin never hides the other's prompt, nor draws a
/// stopped nudge while the other works.
fn worker_agent(agents: &[Agent], name: &str) -> Option<Agent> {
    let rank = |s: &Status| match s {
        Status::Blocked(_) => 3,
        Status::Working => 2,
        Status::Unknown => 1,
        Status::Idle | Status::Done => 0,
    };
    let mut twins = agents.iter().filter(|a| same_name(&a.name, name));
    let first = twins.next()?;
    let busiest = std::iter::once(first)
        .chain(twins)
        .max_by_key(|a| rank(&a.status))?;
    Some(Agent {
        name: first.name.clone(),
        status: busiest.status,
        msg: busiest.msg.clone(),
    })
}

/// What a prompt asks (`msg`, as the terminal gives it) as its need-you item quotes it: on
/// one line, every hidden character dropped and runs of space made one, at most
/// `ASKS_CHARS` characters (cut with "..."); `None` when nothing is left.
fn asks(msg: Option<&str>) -> Option<String> {
    let line = (msg?.split(|c: char| c.is_whitespace() || crate::board::hidden(&c)))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= ASKS_CHARS {
        return Some(line);
    }
    let cut: String = line.chars().take(ASKS_CHARS - 3).collect();
    Some(format!("{}...", cut.trim_end()))
}

/// The worker `name` as task `t`'s threads carry it, as the board writes it.
fn member<'a>(t: &'a Watched, name: &str) -> Option<&'a Named> {
    t.named.iter().find(|n| same_name(&n.name, name))
}

/// The task where a prompt episode of the worker `name` starts, with its name as the board
/// writes it: the task it is assigned to, else the one whose threads it last posted in, else
/// the one whose threads last mentioned it, so an example mention never outranks a
/// worker's own posts. `None` when it is on no watched task's team.
fn team_of<'a>(tasks: &'a [Watched], name: &str) -> Option<(&'a Watched, &'a str)> {
    if let Some(t) = tasks.iter().find(|t| same_name(&t.worker, name)) {
        return Some((t, &t.worker));
    }
    let last = |at: fn(&Named) -> Option<i64>| {
        (tasks.iter())
            .filter_map(|t| {
                let n = member(t, name)?;
                Some((t, n.name.as_str(), at(n)?))
            })
            .max_by_key(|(_, _, at)| *at)
            .map(|(t, named, _)| (t, named))
    };
    last(|n| n.posted).or_else(|| last(|n| n.mentioned))
}

/// Whether `flag`'s episode is still on, given its task as watched now (`None` when it is
/// no longer watched: Done, unassigned, moved) and its worker's agent.
fn still_on(flag: &Flag, task: Option<&Watched>, agent: Option<&Agent>) -> bool {
    let Some(t) = task else {
        return false;
    };
    let assignee = same_name(&t.worker, &flag.worker);
    match flag.signal {
        // It stays on its task while the worker waits and is on that task's team.
        Signal::Prompt => {
            agent.is_some_and(|a| a.status.is_blocked())
                && (assignee || member(t, &flag.worker).is_some())
        }
        Signal::Orphaned => assignee && agent.is_none() && t.stage == Stage::InProgress,
        // Only a post ends it: the nudge itself sets the worker working.
        Signal::Stopped | Signal::Silent => {
            assignee
                && match (t.last_post, flag.quiet_since) {
                    (Some(post), Some(since)) => post <= since,
                    (Some(_), None) => false,
                    (None, _) => true,
                }
        }
    }
}

/// What the watch does at `now`, from the watched tasks, the terminal's live agents and the
/// flags it remembers: ends finished episodes, tells the lead of unanswered nudges and raises
/// new flags, each once per episode. `terminal` is the terminal's name, for the texts.
pub fn decide(
    tasks: &[Watched],
    agents: &[Agent],
    flags: &[Flag],
    now: i64,
    terminal: &str,
) -> Vec<Action> {
    let task = |id: TaskId| tasks.iter().find(|t| t.task == id);
    let agent = |worker: &str| worker_agent(agents, worker);
    let mut actions = Vec::new();
    let mut on: Vec<&Flag> = Vec::new();
    for f in flags {
        let t = task(f.task);
        let a = agent(&f.worker);
        let a = a.as_ref();
        if !still_on(f, t, a) {
            // The owner sees the prompt was handled; the need-you item itself stays until
            // its thread is resolved (spec 3a).
            let answered = f.signal == Signal::Prompt
                && t.is_some()
                && a.is_some_and(|a| !a.status.is_blocked());
            actions.push(Action::Clear {
                task: f.task,
                signal: f.signal,
                worker: f.worker.clone(),
                note: answered.then(|| format!("{}'s prompt was answered.", f.worker)),
            });
            continue;
        }
        on.push(f);
        if let Some(t) = t
            && f.signal.nudges()
            && f.lead_told_at.is_none()
            && now - f.raised_at >= ANSWER_WITHIN
        {
            let why = match f.signal {
                Signal::Stopped => "it stopped without reporting",
                _ => "it is working but silent",
            };
            actions.push(Action::TellLead {
                task: f.task,
                signal: f.signal,
                worker: f.worker.clone(),
                text: format!(
                    "@{} {} on {} has not posted since it was nudged {} min ago ({why}).",
                    t.lead,
                    f.worker,
                    f.task,
                    (now - f.raised_at) / 60
                ),
            });
        }
    }
    for t in tasks {
        let has =
            |pred: &dyn Fn(Signal) -> bool| on.iter().any(|f| f.task == t.task && pred(f.signal));
        let flag = |signal, worker: &str| Flag {
            task: t.task,
            signal,
            worker: worker.to_string(),
            quiet_since: t.last_post,
            raised_at: now,
            lead_told_at: None,
        };
        // Each member of its team on a prompt, as the terminal lists them: the first of a
        // name that is blocked, so a twin listed before it never hides it.
        for (i, b) in agents.iter().enumerate() {
            let blocked_before = |e: &Agent| e.status.is_blocked() && same_name(&e.name, &b.name);
            if !b.status.is_blocked() || agents[..i].iter().any(blocked_before) {
                continue;
            }
            let Some((_, name)) = team_of(tasks, &b.name).filter(|(home, _)| home.task == t.task)
            else {
                continue;
            };
            // One prompt, one need-you item: an episode on any task is this prompt's.
            let flagged =
                (on.iter()).any(|f| f.signal == Signal::Prompt && same_name(&f.worker, name));
            if flagged {
                continue;
            }
            let mut text = format!(
                "{QUESTION} {OWNER_HANDLE} {name} on {} is waiting on a prompt in {terminal} \
                 that only you can answer (@{} for your information).",
                t.task, t.lead
            );
            if let Some(asks) = asks(b.msg.as_deref()) {
                text.push_str(&format!(" It asks: {asks}"));
            }
            actions.push(Action::Raise {
                flag: flag(Signal::Prompt, name),
                text,
            });
        }
        let a = agent(&t.worker);
        if a.is_none()
            && !agents.is_empty()
            && t.absent >= ORPHANED_AFTER
            && t.stage == Stage::InProgress
            && !has(&|s| s == Signal::Orphaned)
        {
            actions.push(Action::Raise {
                flag: flag(Signal::Orphaned, &t.worker),
                text: format!(
                    "@{} {} is in progress but its worker {} is gone from {terminal} (no \
                     live Claude session by that name); {}.",
                    t.lead,
                    t.task,
                    t.worker,
                    closed_worker_hint(&[t.task])
                ),
            });
        }
        let Some(a) = a else { continue };
        if t.blocked || has(&Signal::nudges) {
            continue;
        }
        let quiet = now - t.last_post.unwrap_or(t.seen_since);
        let signal = match a.status {
            Status::Idle | Status::Done if quiet >= STOPPED_AFTER => Signal::Stopped,
            Status::Working if t.stage == Stage::InProgress && quiet >= SILENT_AFTER => {
                Signal::Silent
            }
            _ => continue,
        };
        actions.push(Action::Nudge {
            flag: flag(signal, &t.worker),
            agent: a.name.clone(),
            text: format!(
                "no post on {} for {} min, post your status",
                t.task,
                quiet / 60
            ),
        });
    }
    actions
}

/// The watch: the board it reads, the terminal it asks, and how long each worker has been
/// missing from the terminal.
pub struct Watch {
    db: PathBuf,
    terminal: Arc<dyn Terminal>,
    /// Looks in a row each worker (lower case) was missing from the terminal's agents.
    absent: HashMap<String, u32>,
    /// When this watch first saw each repo's lead with no attached Claude session and no
    /// listener (`listen::check_leads`).
    unheard: crate::listen::Unheard,
    /// Set by SIGTERM or Ctrl-C: checked before each terminal call, so a stop is prompt.
    stop: Arc<AtomicBool>,
}

impl Watch {
    pub fn new(db: &Path, terminal: Arc<dyn Terminal>, stop: Arc<AtomicBool>) -> Watch {
        Watch {
            db: db.to_path_buf(),
            terminal,
            absent: HashMap::new(),
            unheard: HashMap::new(),
            stop,
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// One look at `now`: returns a line per action taken. Fails, doing nothing, when the
    /// board or the terminal cannot be read, so a terminal that is down never reads as every
    /// worker gone. A single action that fails is logged and the rest still run.
    pub fn tick(&mut self, now: i64) -> Result<Vec<String>> {
        let mut store = Store::open(&self.db)?;
        let board = store.load()?;
        let posts: HashMap<TaskId, i64> = store.latest_posts()?.into_iter().collect();
        let flags = store.flags()?;
        // No lead, of this board or another, nor the owner, is a member of a team.
        let leads: Vec<&str> = (board.milestones.iter())
            .filter_map(|m| m.lead.as_ref().map(|l| l.session.as_str()))
            .collect();
        let mut named: HashMap<TaskId, Vec<Named>> = HashMap::new();
        for (task, n) in store.task_names()? {
            if !same_name(&n.name, OWNER_AUTHOR) && !leads.iter().any(|l| same_name(l, &n.name)) {
                named.entry(task).or_default().push(n);
            }
        }
        if self.stopping() {
            return Ok(Vec::new());
        }
        let agents = (self.terminal).agents(&self.db, terminal_timeout())?;
        let mut tasks = Vec::new();
        for m in &board.milestones {
            let Some(lead) = &m.lead else { continue };
            for t in &m.tasks {
                let Some(worker) = t.details.assignee.as_deref() else {
                    continue;
                };
                if t.stage == Stage::Done {
                    continue;
                }
                tasks.push(Watched {
                    task: t.id,
                    stage: t.stage,
                    worker: worker.to_string(),
                    named: named.remove(&t.id).unwrap_or_default(),
                    lead: lead.session.clone(),
                    blocked: t.details.blocked.is_some(),
                    last_post: posts.get(&t.id).copied(),
                    seen_since: now,
                    absent: 0,
                });
            }
        }
        let pairs: Vec<(TaskId, String)> =
            (tasks.iter()).map(|t| (t.task, t.worker.clone())).collect();
        let seen = store.watch_seen(&pairs, now)?;
        // An empty list says nothing about anyone: the terminal restarting, or another
        // session.
        if !agents.is_empty() {
            let workers: Vec<String> = (tasks.iter())
                .map(|t| t.worker.to_ascii_lowercase())
                .collect();
            self.absent.retain(|w, _| workers.contains(w));
            for w in workers {
                let missing = !agents.iter().any(|a| same_name(&a.name, &w));
                let n = self.absent.entry(w).or_insert(0);
                *n = if missing {
                    (*n + 1).min(ORPHANED_AFTER)
                } else {
                    0
                };
            }
        }
        for t in &mut tasks {
            t.seen_since = seen.get(&t.task).copied().unwrap_or(now);
            t.absent = (self.absent.get(&t.worker.to_ascii_lowercase()))
                .copied()
                .unwrap_or(0);
        }
        let mut log = Vec::new();
        let name = self.terminal.name();
        for action in decide(&tasks, &agents, &flags, now, name) {
            if self.stopping() {
                break;
            }
            log.push(match self.act(&mut store, &action, now) {
                Ok(line) => line,
                Err(e) => format!("failed: {}: {e:#}", describe(&action)),
            });
        }
        Ok(log)
    }

    /// The look at whether each registered lead is listening, at `now`
    /// (`listen::check_leads`), asking `claude` (`claude agents --json`) only when a lead
    /// needs it: no terminal, so it runs whether the terminal answers or not. It runs on the
    /// caller's thread: a claude that hangs holds it, and the look after it, up to
    /// `CLAUDE_TIMEOUT` (30 s), and a stop asked meanwhile waits for that too. A line per
    /// change and per failure.
    pub fn leads(&mut self, claude: &Path, now: i64) -> Vec<String> {
        if self.stopping() {
            return Vec::new();
        }
        let mut store = match Store::open(&self.db) {
            Ok(store) => store,
            Err(e) => return vec![format!("could not check the leads are listening: {e:#}")],
        };
        let agents =
            || crate::register::claude(claude, &self.db, &self.stop, &["agents", "--json"]);
        crate::listen::check_leads(&mut store, &self.db, agents, now, &mut self.unheard)
    }

    fn act(&self, store: &mut Store, action: &Action, now: i64) -> Result<String> {
        let what = describe(action);
        match action {
            Action::Nudge { flag, agent, text } => {
                // The episode is remembered before the prompt goes, so a store failure
                // after it never sends a second one; the record follows, saying whether
                // the terminal delivered it, and the lead still hears in time either way.
                store.watch_step(flag.task, FlagChange::Raise(flag), None, now)?;
                let terminal = &self.terminal;
                let prompted = terminal.prompt(&self.db, agent, text, SEND_TIMEOUT);
                let (sent, trace) = (prompted.sent, prompted.trace);
                let quoted = |why: &str| why.chars().take(300).collect::<String>();
                let record = match &sent {
                    Sent::Delivered => format!("nudged {agent}: {text}"),
                    // It may have arrived: never said to be refused.
                    Sent::Unsure(why) => format!(
                        "nudged {agent}: {text}; not confirmed by {}: {}",
                        terminal.name(),
                        quoted(why)
                    ),
                    Sent::Failed(why) => format!(
                        "nudged {agent}: {text}; {} refused delivery: {}",
                        terminal.name(),
                        quoted(why)
                    ),
                    // Sent to someone else: the record is the owner's item, naming who.
                    Sent::Misrouted(why) => format!(
                        "{DELIVERY_FAILURE}{agent}: {} (the nudge: {text})",
                        quoted(why)
                    ),
                };
                store.watch_step(flag.task, FlagChange::None, Some(&record), now)?;
                let traced = |line: String| match &trace {
                    Some(trace) => format!("{line}; {trace}"),
                    None => line,
                };
                match sent {
                    Sent::Delivered => Ok(traced(what)),
                    Sent::Unsure(why) | Sent::Failed(why) | Sent::Misrouted(why) => {
                        Err(anyhow::anyhow!(traced(why)))
                    }
                }
            }
            Action::Raise { flag, text } => {
                let thread =
                    store.watch_step(flag.task, FlagChange::Raise(flag), Some(text), now)?;
                Ok(format!(
                    "{what} in {}",
                    thread.map_or("?".into(), |t| t.to_string())
                ))
            }
            Action::TellLead {
                task,
                signal,
                worker,
                text,
            } => {
                let change = FlagChange::LeadTold(*signal, worker);
                let thread = store.watch_step(*task, change, Some(text), now)?;
                Ok(format!(
                    "{what} in {}",
                    thread.map_or("?".into(), |t| t.to_string())
                ))
            }
            Action::Clear {
                task,
                signal,
                worker,
                note,
            } => {
                let change = FlagChange::Clear(*signal, worker);
                store.watch_step(*task, change, note.as_deref(), now)?;
                Ok(what)
            }
        }
    }
}

/// The log line of an action.
fn describe(action: &Action) -> String {
    match action {
        Action::Nudge { flag, agent, text } => format!(
            "nudged {agent} on {} ({}): {text}",
            flag.task,
            flag.signal.name()
        ),
        Action::Raise { flag, .. } => format!(
            "flagged {} on {} ({})",
            flag.worker,
            flag.task,
            flag.signal.name()
        ),
        Action::TellLead { task, signal, .. } => {
            format!(
                "told the lead of {task}'s unanswered nudge ({})",
                signal.name()
            )
        }
        Action::Clear {
            task,
            signal,
            worker,
            ..
        } => {
            format!("{task}'s {} episode for {worker} ended", signal.name())
        }
    }
}

/// The file a path resolves to: a brew upgrade repoints the path to another Cellar file,
/// a build in place changes its size or time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeId {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
}

impl ExeId {
    /// `None` when `exe` resolves to nothing, as mid-upgrade.
    pub fn of(exe: &Path) -> Option<ExeId> {
        let path = exe.canonicalize().ok()?;
        let meta = std::fs::metadata(&path).ok()?;
        Some(ExeId {
            path,
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

/// Whether the watch started as `started` restarts now: its path resolves to another file
/// (`now`; `None`, nothing there yet, waits for the next check), and no restart failed in
/// the last `RETRY_RESTART` (`since_failed`).
pub fn restart_due(started: &ExeId, now: Option<&ExeId>, since_failed: Option<Duration>) -> bool {
    now.is_some_and(|now| now != started) && since_failed.is_none_or(|d| d >= RETRY_RESTART)
}

/// Replaces this process with `exe` run with this process's arguments; returns only on
/// failure.
fn restart(exe: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec()
}

/// `tower watch`: looks every `TICK` until SIGTERM or Ctrl-C, logging one line per action
/// to stderr.
pub fn run(db: &Path) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, Arc::clone(&stop))
            .context("cannot listen for SIGTERM and Ctrl-C")?;
    }
    // The lead skill registers with Claude Code, retried less often than a look: each try
    // runs claude, and what stops it (no login, no claude) does not clear in seconds. A try
    // runs on its own thread and is polled between looks, so registering never holds a look.
    // The lead check's claude agents --json does run in the look (`Watch::leads`), bounded by
    // CLAUDE_TIMEOUT, and so does the terminal's (Rex names its Claude sessions by it).
    let claude = crate::register::claude_path();
    let terminal = crate::terminal::module(crate::terminal::exe_from_env());
    let mut watch = Watch::new(db, Arc::clone(&terminal), Arc::clone(&stop));
    let log = |line: &str| {
        let at = chrono::Local::now().format("%H:%M:%S");
        eprintln!("{at} tower watch: {line}");
    };
    log(&format!(
        "watching {} with {} every {} s",
        db.display(),
        terminal.exe().display(),
        TICK.as_secs()
    ));
    let mut skill = crate::register::Skill::new(
        claude.clone(),
        crate::register::skill_dir(),
        Arc::clone(&stop),
    );
    let mut said_missing = false;
    // The path as started, kept unresolved: the brew `opt` symlink outlives an upgrade.
    let me = std::env::current_exe()
        .and_then(std::path::absolute)
        .ok()
        .and_then(|exe| ExeId::of(&exe).map(|id| (exe, id)));
    if me.is_none() {
        log("cannot find this tower's own file; it will not restart itself after an upgrade");
    }
    let mut failed_at: Option<std::time::Instant> = None;
    // Delivery runs on its own thread, so a look waiting on the terminal never holds a post.
    let deliver = crate::deliver::Deliver::new(db, Arc::clone(&terminal), Arc::clone(&stop));
    let gate = deliver.gate();
    let delivering = std::thread::spawn(move || deliver.run(log));
    while !stop.load(Ordering::Relaxed) {
        let path = std::env::var_os("PATH");
        match terminal.missing(path.as_deref()) {
            None => said_missing = false,
            Some(line) if !said_missing => {
                log(&format!(
                    "{line} (said once, until {} is found)",
                    terminal.name()
                ));
                said_missing = true;
            }
            Some(_) => {}
        }
        let now = chrono::Utc::now().timestamp();
        watch.leads(&claude, now).iter().for_each(|l| log(l));
        match watch.tick(now) {
            Ok(lines) => lines.iter().for_each(|l| log(l)),
            Err(e) => log(&format!("skipped a look: {e:#}")),
        }
        let until = std::time::Instant::now() + TICK;
        let mut check_at = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < until {
            if let Some(line) = skill.tick(db, std::time::Instant::now()) {
                log(&line);
            }
            if let Some((exe, started)) = &me
                && std::time::Instant::now() >= check_at
            {
                check_at += UPGRADE_CHECK;
                let since_failed = failed_at.map(|t| t.elapsed());
                if restart_due(started, ExeId::of(exe).as_ref(), since_failed) {
                    log("tower was upgraded, restarting");
                    // Wait out a drain in flight: the exec would end it with posts taken
                    // and not sent. Held until the exec, or released if it fails.
                    let held = gate
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let e = restart(exe);
                    drop(held);
                    log(&format!(
                        "could not restart {}: {e}; this one keeps running, trying again in {} s",
                        exe.display(),
                        RETRY_RESTART.as_secs()
                    ));
                    failed_at = Some(std::time::Instant::now());
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if delivering.join().is_err() {
        log("the delivery thread died");
    }
    log("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOf, MilestoneId, ThreadId, Tone, tone_of};
    use crate::store::{FlagChange, TaskFields};
    use crate::terminal::rex::fake::FakeRex;
    use crate::terminal::{Handle, Prompted, Unread};

    const MIN: i64 = 60;

    /// The watch's terminal in most of these tests: Rex's look over the fake rex and claude
    /// in `rex/`, and a prompt that appends "name|text" to `prompts.log`, or refuses with the
    /// text of `refuse` (or is unsure with that of `unsure`, or sends elsewhere as `misroute`
    /// says) when that file exists, with no trace, so each test reads only the
    /// watch's own lines. Rex's own prompt, the relay, is in `a_nudge_reaches_a_rex_worker...`.
    struct Team {
        rex: crate::terminal::rex::Rex,
        dir: PathBuf,
    }

    impl Terminal for Team {
        fn name(&self) -> &'static str {
            self.rex.name()
        }
        fn exe(&self) -> &Path {
            self.rex.exe()
        }
        fn missing(&self, path: Option<&std::ffi::OsStr>) -> Option<String> {
            self.rex.missing(path)
        }
        fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread> {
            self.rex.agents(db, timeout)
        }
        fn prompt(&self, _db: &Path, name: &str, text: &str, _timeout: Duration) -> Prompted {
            if let Ok(why) = std::fs::read_to_string(self.dir.join("refuse")) {
                return Sent::Failed(why).into();
            }
            if let Ok(why) = std::fs::read_to_string(self.dir.join("unsure")) {
                return Sent::Unsure(why).into();
            }
            if let Ok(why) = std::fs::read_to_string(self.dir.join("misroute")) {
                return Sent::Misrouted(why).into();
            }
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join("prompts.log"))
                .unwrap();
            use std::io::Write;
            writeln!(log, "{name}|{text}").unwrap();
            Sent::Delivered.into()
        }
        fn start_lead(&self, _db: &Path, _lead: &crate::terminal::LeadStart) -> Result<Handle> {
            unreachable!("the watch starts no lead")
        }
        fn undo(&self, _db: &Path, _handle: &Handle) -> Result<String> {
            unreachable!("the watch starts no lead")
        }
    }

    /// The watch's terminal over the fakes in `dir`.
    fn team(dir: &Path) -> Arc<dyn Terminal> {
        Arc::new(Team {
            rex: FakeRex {
                dir: dir.join("rex"),
            }
            .module(),
            dir: dir.to_path_buf(),
        })
    }

    /// A temp board with M1 led by cat-lead, task #1 assigned to dev-1 and In progress with
    /// a thread T1 linked to it whose latest post is at `t0`, a fake rex and claude in
    /// `rex/` (`FakeRex`) and the watch's terminal over them (`team`).
    struct Fixture {
        dir: tempfile::TempDir,
        db: PathBuf,
        rex: FakeRex,
        watch: Watch,
        /// The time of #1's latest post.
        t0: i64,
        /// R1's listener lock, held: its lead's board waiter runs, so the watch's check of
        /// the leads raises nothing for it in these tests of the workers.
        _listening: std::fs::File,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("board.db");
            std::fs::create_dir(dir.path().join("rex")).unwrap();
            let rex = FakeRex::new(&dir.path().join("rex"));
            let mut store = Store::open(&db).unwrap();
            let m = store.add_milestone("MVP", "goal", None).unwrap();
            store
                .lead_milestone(m, "Catalogue", "cat-lead", None)
                .unwrap();
            let t = store.add_task(m, "Bookmarks").unwrap();
            store.set_task_stage(t, Stage::InProgress).unwrap();
            assign(&mut store, t, "dev-1");
            // Its latest post is at t0, a controlled time after the real clock's.
            let t0 = chrono::Utc::now().timestamp() + 3600;
            store
                .open_thread(&BoardOf::Milestone(m), "cat-lead", Some(t), "#1", "go")
                .unwrap();
            store.post_at(ThreadId(1), "dev-1", "plan: start with the list", t0);
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(crate::listen::lock_path(&db, crate::board::RepoId(1)))
                .unwrap();
            lock.try_lock().unwrap();
            let f = Fixture {
                watch: Watch::new(&db, team(dir.path()), Arc::default()),
                dir,
                db,
                rex,
                t0,
                _listening: lock,
            };
            f.agents(&[("dev-1", "idle")]);
            f
        }

        fn store(&self) -> Store {
            Store::open(&self.db).unwrap()
        }

        /// The live workers from now on, as (name, state): each a Claude session in its own
        /// Rex block, blocked ones at a permission prompt. Rex also has a shell running no
        /// Claude, and a Claude at a prompt that Claude does not list (its session gone).
        fn agents(&self, agents: &[(&str, &str)]) {
            let asks: Vec<(&str, &str, Option<&str>)> =
                agents.iter().map(|(n, s)| (*n, *s, None)).collect();
            self.asking(&asks);
        }

        /// As `agents`, each with what its prompt asks, if anything.
        fn asking(&self, agents: &[(&str, &str, Option<&str>)]) {
            let records: Vec<String> = (agents.iter().enumerate())
                .map(|(i, (_, state, msg))| {
                    let kind = (*state == "blocked").then_some("permission");
                    FakeRex::claude_record(100 + i as i64, state, kind, *msg)
                })
                .collect();
            let ids: Vec<String> = (0..agents.len()).map(|i| format!("block:{i}")).collect();
            let gone = FakeRex::claude_record(99, "blocked", Some("question"), None);
            let term = "com.superlogical.terminal";
            let mut blocks: Vec<(&str, &str, &str)> = (ids.iter().zip(&records))
                .map(|(id, r)| (id.as_str(), term, r.as_str()))
                .collect();
            blocks.push(("block:shell", term, ""));
            blocks.push(("block:gone", term, &gone));
            self.rex.server(&[("session:team", &blocks)]);
            let listed: Vec<(&str, i64)> = (agents.iter().enumerate())
                .map(|(i, (name, _, _))| (*name, 100 + i as i64))
                .collect();
            self.rex.claude(&listed);
        }

        /// A look `mins` minutes after `t0`.
        fn tick(&mut self, mins: i64) -> Vec<String> {
            self.watch.tick(self.t0 + mins * MIN).unwrap()
        }

        /// A post by `who` in thread `t`, `mins` minutes after `t0`.
        fn post(&self, t: i64, who: &str, mins: i64) {
            self.store()
                .post_at(ThreadId(t), who, "status: on it", self.t0 + mins * MIN);
        }

        fn prompts(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("prompts.log"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }

        /// The system messages in thread `t`, oldest first.
        fn system(&self, t: i64) -> Vec<String> {
            (self
                .store()
                .thread(ThreadId(t))
                .unwrap()
                .messages
                .into_iter())
            .filter(|m| m.author == "system")
            .map(|m| m.text)
            .collect()
        }
    }

    fn assign(store: &mut Store, task: TaskId, who: &str) {
        let fields = TaskFields {
            assignee: Some(who),
            ..Default::default()
        };
        store.set_task(task, fields).unwrap();
    }

    #[test]
    fn a_worker_on_a_prompt_is_a_need_you_item_at_once_and_once_per_episode() {
        let mut f = Fixture::new();
        f.agents(&[("dev-1", "blocked")]);
        let log = f.tick(0);
        assert_eq!(log, ["flagged dev-1 on #1 (prompt) in T1"]);
        let want = "question: @manager dev-1 on #1 is waiting on a prompt in Rex that only you can \
                    answer (@cat-lead for your information).";
        assert_eq!(f.system(1), [want]);
        // The inbox and the alert line pick it up through the one classifier.
        let board = f.store().load().unwrap();
        let m = &board.milestones[0];
        let msg = &m.threads[0].messages.last().unwrap();
        assert_eq!(tone_of(msg), Tone::NeedsYou);
        let need: Vec<_> = board
            .inbox
            .need_you
            .iter()
            .map(|a| &a.message.text)
            .collect();
        assert_eq!(need, [want]);
        // Not repeated while it waits, and no nudge: only the owner can answer.
        assert!(f.tick(1).is_empty());
        assert!(f.tick(10).is_empty());
        assert!(f.prompts().is_empty());
        // Answered, then a new prompt: a new episode, raised again.
        f.agents(&[("dev-1", "working")]);
        assert_eq!(f.tick(11), ["#1's prompt episode for dev-1 ended"]);
        // A plain note says so; the need-you item stays until the owner clears it or the
        // thread is resolved.
        assert_eq!(f.system(1)[1], "dev-1's prompt was answered.");
        let board = f.store().load().unwrap();
        assert_eq!(board.inbox.need_you.len(), 1);
        let note = board.milestones[0].threads[0]
            .messages
            .last()
            .unwrap()
            .clone();
        assert_eq!(tone_of(&note), Tone::Plain);
        f.agents(&[("dev-1", "blocked")]);
        assert_eq!(f.tick(12), ["flagged dev-1 on #1 (prompt) in T1"]);
        assert_eq!(f.system(1).len(), 3);
        // Ending for another reason (the task Done) is no answer: no note.
        f.store().set_task_stage(TaskId(1), Stage::Done).unwrap();
        assert_eq!(f.tick(13), ["#1's prompt episode for dev-1 ended"]);
        assert_eq!(f.system(1).len(), 3);
    }

    #[test]
    fn a_team_member_on_a_prompt_is_flagged_once_and_never_nudged_or_orphaned() {
        let mut f = Fixture::new();
        f.post(1, "qa", 0);
        f.agents(&[("dev-1", "working"), ("qa", "blocked")]);
        assert_eq!(f.tick(1), ["flagged qa on #1 (prompt) in T1"]);
        let want = "question: @manager qa on #1 is waiting on a prompt in Rex that only you can \
                    answer (@cat-lead for your information).";
        assert_eq!(f.system(1), [want]);
        let board = f.store().load().unwrap();
        let need: Vec<_> = (board.inbox.need_you.iter())
            .map(|a| &a.message.text)
            .collect();
        assert_eq!(need, [want]);
        assert!(f.tick(2).is_empty(), "once per episode");
        f.agents(&[("dev-1", "working"), ("qa", "working")]);
        assert_eq!(f.tick(3), ["#1's prompt episode for qa ended"]);
        assert_eq!(f.system(1)[1], "qa's prompt was answered.");
        // Gone from Rex for three looks, or idle an hour: the assignee's signals only.
        f.agents(&[("dev-1", "working")]);
        for m in 4..8 {
            assert!(f.tick(m).is_empty(), "look {m}");
        }
        f.post(1, "dev-1", 59);
        f.agents(&[("dev-1", "working"), ("qa", "idle")]);
        assert!(f.tick(60).is_empty());
        assert!(f.prompts().is_empty());
        assert_eq!(f.system(1).len(), 2);
    }

    #[test]
    fn two_members_on_prompts_at_once_are_two_episodes() {
        let mut f = Fixture::new();
        // The lead named senior, which has not posted yet.
        f.store()
            .post_at(ThreadId(1), "cat-lead", "@senior please review", f.t0);
        f.agents(&[("dev-1", "blocked"), ("SENIOR", "blocked")]);
        assert_eq!(
            f.tick(1),
            [
                "flagged dev-1 on #1 (prompt) in T1",
                "flagged senior on #1 (prompt) in T1"
            ]
        );
        f.agents(&[("dev-1", "blocked"), ("SENIOR", "working")]);
        assert_eq!(f.tick(2), ["#1's prompt episode for senior ended"]);
        assert!(f.tick(3).is_empty(), "dev-1's goes on");
        let flags = f.store().flags().unwrap();
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].worker, "dev-1");
        f.agents(&[("dev-1", "blocked"), ("SENIOR", "blocked")]);
        assert_eq!(f.tick(4), ["flagged senior on #1 (prompt) in T1"]);
        assert_eq!(f.system(1).len(), 4);
    }

    #[test]
    fn a_prompt_starts_on_the_assigned_task_else_where_it_posted_else_where_it_was_named() {
        let mut f = Fixture::new();
        let mut store = f.store();
        let t2 = store.add_task(MilestoneId(1), "Second").unwrap();
        assign(&mut store, t2, "dev-2");
        let m1 = BoardOf::Milestone(MilestoneId(1));
        store
            .open_thread(&m1, "cat-lead", Some(t2), "#2", "go")
            .unwrap();
        // dev-2 posts on #1 but is assigned #2. qa posts on #1 and is later named on #2.
        // senior has only been named, on #1 then #2.
        f.post(1, "dev-2", 0);
        f.post(1, "qa", 0);
        store.post_at(ThreadId(1), "cat-lead", "@senior review #1", f.t0 + MIN);
        store.post_at(
            ThreadId(2),
            "dev-2",
            "plan: (@qa please test X), @senior",
            f.t0 + 2 * MIN,
        );
        f.agents(&[
            ("dev-1", "working"),
            ("dev-2", "blocked"),
            ("qa", "blocked"),
            ("senior", "blocked"),
        ]);
        assert_eq!(
            f.tick(3),
            [
                "flagged qa on #1 (prompt) in T1",
                "flagged dev-2 on #2 (prompt) in T2",
                "flagged senior on #2 (prompt) in T2"
            ]
        );
        // Named on #1 again while it waits: the episode stays on #2, one need-you item.
        store.post_at(
            ThreadId(1),
            "cat-lead",
            "@senior back to #1",
            f.t0 + 4 * MIN,
        );
        assert!(f.tick(5).is_empty());
        assert_eq!(f.store().load().unwrap().inbox.need_you.len(), 3);
        // Answered, then a new prompt: it starts where senior was named last.
        f.agents(&[("dev-1", "working"), ("senior", "working")]);
        let log = f.tick(6);
        assert!(
            log.contains(&"#2's prompt episode for senior ended".to_string()),
            "{log:?}"
        );
        assert_eq!(f.system(2).last().unwrap(), "senior's prompt was answered.");
        f.agents(&[("dev-1", "working"), ("senior", "blocked")]);
        assert_eq!(f.tick(7), ["flagged senior on #1 (prompt) in T1"]);
    }

    #[test]
    fn reassigning_ends_the_old_assignees_orphan_episode_so_the_new_one_can_be_flagged() {
        let mut f = Fixture::new();
        f.agents(&[("other", "working")]);
        f.tick(1);
        f.tick(1);
        assert_eq!(f.tick(1), ["flagged dev-1 on #1 (orphaned) in T1"]);
        assign(&mut f.store(), TaskId(1), "dev-2");
        assert_eq!(f.tick(2), ["#1's orphaned episode for dev-1 ended"]);
        assert!(f.tick(3).is_empty());
        assert_eq!(f.tick(4), ["flagged dev-2 on #1 (orphaned) in T1"]);
    }

    #[test]
    fn unassigning_a_closed_workers_task_ends_its_episode_and_nothing_follows() {
        let mut f = Fixture::new();
        f.agents(&[("other", "working")]);
        f.tick(1);
        f.tick(1);
        assert_eq!(f.tick(1), ["flagged dev-1 on #1 (orphaned) in T1"]);
        // The lead closed dev-1 at the end of its step and unassigns the task.
        f.store().unassign(TaskId(1)).unwrap();
        assert_eq!(f.tick(2), ["#1's orphaned episode for dev-1 ended"]);
        // No nudge, flag or note for it after, however long it stays quiet.
        for at in [3, 4, 61, 62, 200] {
            assert!(f.tick(at).is_empty(), "at {at}");
        }
    }

    #[test]
    fn an_assignee_its_threads_never_named_is_flagged_once_and_ends_on_reassignment() {
        let mut f = Fixture::new();
        let mut store = f.store();
        let t2 = store.add_task(MilestoneId(1), "Second").unwrap();
        assign(&mut store, t2, "dev-2");
        let m1 = BoardOf::Milestone(MilestoneId(1));
        // Only the lead's "go": dev-2 has neither posted nor been named on #2.
        store
            .open_thread(&m1, "cat-lead", Some(t2), "#2", "go")
            .unwrap();
        f.agents(&[
            ("dev-1", "working"),
            ("dev-2", "blocked"),
            ("dev-3", "working"),
        ]);
        assert_eq!(f.tick(1), ["flagged dev-2 on #2 (prompt) in T2"]);
        assert!(f.tick(2).is_empty(), "not raised again each look");
        // Reassigned while it waits: off the team, so the episode ends, with no note.
        assign(&mut f.store(), t2, "dev-3");
        assert_eq!(f.tick(3), ["#2's prompt episode for dev-2 ended"]);
        assert!(f.tick(4).is_empty());
        assert_eq!(f.system(2).len(), 1);
    }

    #[test]
    fn agents_whose_names_differ_only_by_case_are_one_worker_one_flag() {
        let mut f = Fixture::new();
        f.post(1, "qa", 0);
        f.agents(&[("dev-1", "working"), ("qa", "blocked"), ("QA", "blocked")]);
        assert_eq!(f.tick(1), ["flagged qa on #1 (prompt) in T1"]);
        assert_eq!(f.system(1).len(), 1);
        assert_eq!(f.store().flags().unwrap().len(), 1);
    }

    #[test]
    fn twins_by_name_are_one_worker_so_a_stale_one_hides_no_prompt_and_draws_no_nudge() {
        let mut f = Fixture::new();
        // A stale dev-1, idle, listed before the live one, which works.
        f.agents(&[("dev-1", "idle"), ("dev-1", "working")]);
        assert!(f.tick(6).is_empty(), "no stopped nudge while a twin works");
        // The live one at a prompt: raised, quoting it, and kept while it waits.
        f.asking(&[
            ("dev-1", "idle", None),
            ("dev-1", "blocked", Some("approve Bash: ls")),
        ]);
        assert_eq!(f.tick(7), ["flagged dev-1 on #1 (prompt) in T1"]);
        assert!(
            f.system(1)[0].ends_with("It asks: approve Bash: ls"),
            "{:?}",
            f.system(1)
        );
        assert!(
            f.tick(8).is_empty(),
            "the stale twin does not end the episode"
        );
        f.agents(&[("dev-1", "idle"), ("dev-1", "working")]);
        assert_eq!(f.tick(9), ["#1's prompt episode for dev-1 ended"]);
        assert_eq!(f.system(1)[1], "dev-1's prompt was answered.");
        // Both idle: stopped, as one worker, with one nudge.
        f.agents(&[("dev-1", "idle"), ("dev-1", "idle")]);
        assert_eq!(
            f.tick(10),
            ["nudged dev-1 on #1 (stopped): no post on #1 for 10 min, post your status"]
        );
        assert_eq!(f.prompts().len(), 1);
    }

    #[test]
    fn a_task_done_as_its_worker_leaves_the_prompt_ends_the_episode_with_no_note() {
        let mut f = Fixture::new();
        f.agents(&[("dev-1", "blocked")]);
        assert_eq!(f.tick(0), ["flagged dev-1 on #1 (prompt) in T1"]);
        f.store().set_task_stage(TaskId(1), Stage::Done).unwrap();
        f.agents(&[("dev-1", "working")]);
        assert_eq!(f.tick(1), ["#1's prompt episode for dev-1 ended"]);
        assert_eq!(f.system(1).len(), 1, "no note: the task is Done");
    }

    #[test]
    fn the_lead_the_owner_and_names_on_no_team_are_never_flagged() {
        let mut f = Fixture::new();
        let mut store = f.store();
        store.post_at(ThreadId(1), "manager", "@cat-lead fine by me", f.t0);
        // Another milestone's lead, named on #1's thread, is no member either.
        let m2 = store.add_milestone("Checkout", "goal", None).unwrap();
        store
            .lead_milestone(m2, "Checkout Lead", "checkout-lead", None)
            .unwrap();
        store.post_at(ThreadId(1), "cat-lead", "@checkout-lead see this", f.t0);
        // The watch's own messages name no one.
        store.post_at(ThreadId(1), "system", "@ghost is gone", f.t0);
        f.agents(&[
            ("dev-1", "working"),
            ("cat-lead", "blocked"),
            ("checkout-lead", "blocked"),
            ("manager", "blocked"),
            ("ghost", "blocked"),
            ("stranger", "blocked"),
        ]);
        assert!(f.tick(1).is_empty());
        assert!(f.store().flags().unwrap().is_empty());
    }

    #[test]
    fn a_nudge_reaches_a_rex_worker_through_the_relay_and_logs_its_trace() {
        let mut f = Fixture::new();
        f.watch = Watch::new(&f.db, Arc::new(f.rex.module()), Arc::default());
        let lines = f.tick(6);
        assert_eq!(lines.len(), 1, "{lines:?}");
        let text = "no post on #1 for 6 min, post your status";
        let (nudged, trace) = lines[0].split_once("; relay to dev-1: exit 0, ").unwrap();
        assert_eq!(nudged, format!("nudged dev-1 on #1 (stopped): {text}"));
        assert!(trace.ends_with(&format!("; text: {text:?}")), "{trace}");
        let sent = std::fs::read_to_string(f.dir.path().join("rex/prompts.log")).unwrap();
        assert_eq!(sent, format!("dev-1|{text}\n"));
        assert_eq!(f.system(1), [format!("nudged dev-1: {text}")]);
    }

    #[test]
    fn a_stopped_worker_is_nudged_then_the_lead_told_after_two_minutes_once() {
        let mut f = Fixture::new();
        assert!(f.tick(4).is_empty(), "under 5 min is not stopped");
        assert_eq!(
            f.tick(6),
            ["nudged dev-1 on #1 (stopped): no post on #1 for 6 min, post your status"]
        );
        assert_eq!(
            f.prompts(),
            ["dev-1|no post on #1 for 6 min, post your status"]
        );
        // The nudge is on the record too: plain, so not a need-you item.
        assert_eq!(
            f.system(1),
            ["nudged dev-1: no post on #1 for 6 min, post your status"]
        );
        assert!(f.store().load().unwrap().inbox.need_you.is_empty());
        // The nudge sets the worker working; that alone does not end the episode.
        f.agents(&[("dev-1", "working")]);
        assert!(f.tick(7).is_empty());
        assert_eq!(
            f.tick(8),
            ["told the lead of #1's unanswered nudge (stopped) in T1"]
        );
        let want = "@cat-lead dev-1 on #1 has not posted since it was nudged 2 min ago (it \
                    stopped without reporting).";
        assert_eq!(f.system(1)[1..], [want]);
        // To the lead only: not a need-you item.
        assert!(f.store().load().unwrap().inbox.need_you.is_empty());
        f.agents(&[("dev-1", "idle")]);
        assert!(f.tick(9).is_empty());
        assert!(f.tick(30).is_empty());
        assert_eq!(f.prompts().len(), 1);
        // A post ends the episode; 5 quiet minutes after it start a new one.
        f.post(1, "dev-1", 31);
        assert_eq!(f.tick(32), ["#1's stopped episode for dev-1 ended"]);
        assert!(f.tick(35).is_empty(), "under 5 min after the post");
        assert_eq!(
            f.tick(37),
            ["nudged dev-1 on #1 (stopped): no post on #1 for 6 min, post your status"]
        );
        // The watch's own messages are no post.
        assert!(f.tick(38).is_empty());
    }

    #[test]
    fn an_answered_nudge_does_not_reach_the_lead() {
        let mut f = Fixture::new();
        f.tick(6);
        f.post(1, "dev-1", 7);
        assert_eq!(f.tick(7), ["#1's stopped episode for dev-1 ended"]);
        assert!(f.tick(9).is_empty());
        // Only the nudge's record: the lead is never told.
        assert_eq!(f.system(1).len(), 1);
    }

    #[test]
    fn a_nudge_the_terminal_refuses_is_logged_and_the_lead_still_told_in_time() {
        let mut f = Fixture::new();
        std::fs::write(
            f.dir.path().join("refuse"),
            "dev-1 is at a permission prompt",
        )
        .unwrap();
        assert_eq!(
            f.tick(6),
            [
                "failed: nudged dev-1 on #1 (stopped): no post on #1 for 6 min, post your \
                 status: dev-1 is at a permission prompt"
            ]
        );
        assert!(f.tick(7).is_empty(), "not retried every tick");
        assert_eq!(f.tick(8).len(), 1);
        let system = f.system(1);
        assert_eq!(
            system[0],
            "nudged dev-1: no post on #1 for 6 min, post your status; Rex refused delivery: dev-1 \
             is at a permission prompt"
        );
        assert!(system[1].starts_with("@cat-lead dev-1 on #1 has not posted"));
        assert_eq!(system.len(), 2);
    }

    #[test]
    fn a_nudge_that_may_have_arrived_is_recorded_as_not_confirmed_never_refused() {
        let mut f = Fixture::new();
        let why = "the relay did not finish in 60 s and was stopped";
        std::fs::write(f.dir.path().join("unsure"), why).unwrap();
        assert_eq!(f.tick(6).len(), 1);
        assert!(f.tick(7).is_empty(), "not retried every tick");
        assert_eq!(f.tick(8).len(), 1);
        let system = f.system(1);
        assert_eq!(
            system[0],
            format!(
                "nudged dev-1: no post on #1 for 6 min, post your status; not confirmed by Rex: \
                 {why}"
            )
        );
        assert!(system[1].starts_with("@cat-lead dev-1 on #1 has not posted"));
        assert_eq!(system.len(), 2);
    }

    #[test]
    fn a_nudge_sent_to_someone_else_is_an_item_for_the_owner_naming_who_got_it() {
        let mut f = Fixture::new();
        let why = "the relay sent it to \"qa-9\", not to dev-1";
        std::fs::write(f.dir.path().join("misroute"), why).unwrap();
        assert_eq!(f.tick(6).len(), 1);
        let item = format!(
            "@manager could not deliver to dev-1: {why} (the nudge: no post on #1 for 6 min, \
             post your status)"
        );
        assert_eq!(f.system(1), [item.as_str()]);
        let board = f.store().load().unwrap();
        let aware: Vec<&str> = (board.inbox.awareness.iter())
            .map(|a| a.message.text.as_str())
            .collect();
        assert_eq!(aware, [item.as_str()]);
        assert!(board.inbox.need_you.is_empty());
    }

    #[test]
    fn a_silent_working_worker_is_nudged_after_30_min_then_the_lead_told() {
        let mut f = Fixture::new();
        f.agents(&[("dev-1", "working")]);
        assert!(f.tick(29).is_empty());
        assert_eq!(
            f.tick(31),
            ["nudged dev-1 on #1 (silent): no post on #1 for 31 min, post your status"]
        );
        assert_eq!(
            f.prompts(),
            ["dev-1|no post on #1 for 31 min, post your status"]
        );
        assert!(f.tick(32).is_empty());
        assert_eq!(
            f.tick(33),
            ["told the lead of #1's unanswered nudge (silent) in T1"]
        );
        let system = f.system(1);
        assert_eq!(
            system[0],
            "nudged dev-1: no post on #1 for 31 min, post your status"
        );
        assert!(system[1].ends_with("(it is working but silent)."));
        assert!(f.tick(40).is_empty());
    }

    #[test]
    fn silent_needs_in_progress_and_stopped_any_stage_but_done() {
        let mut f = Fixture::new();
        f.store().set_task_stage(TaskId(1), Stage::Waiting).unwrap();
        f.agents(&[("dev-1", "working")]);
        assert!(
            f.tick(60).is_empty(),
            "a waiting task's busy worker is not silent"
        );
        f.agents(&[("dev-1", "done")]);
        assert_eq!(
            f.tick(61).len(),
            1,
            "a finished worker on a waiting task stopped"
        );
        f.store().set_task_stage(TaskId(1), Stage::Done).unwrap();
        assert_eq!(f.tick(62), ["#1's stopped episode for dev-1 ended"]);
        assert!(f.tick(90).is_empty(), "a Done task is not watched");
    }

    #[test]
    fn an_orphaned_task_is_flagged_to_the_lead_after_three_looks_once_per_episode() {
        let mut f = Fixture::new();
        f.agents(&[("dev-2", "working")]);
        assert!(f.tick(0).is_empty(), "missing once");
        assert!(f.tick(0).is_empty(), "missing twice");
        // Back for one look: the count starts again.
        f.agents(&[("dev-1", "working"), ("dev-2", "working")]);
        assert!(f.tick(1).is_empty());
        f.agents(&[("dev-2", "working")]);
        assert!(f.tick(1).is_empty());
        assert!(f.tick(1).is_empty());
        assert_eq!(f.tick(2), ["flagged dev-1 on #1 (orphaned) in T1"]);
        assert_eq!(
            f.system(1),
            [
                "@cat-lead #1 is in progress but its worker dev-1 is gone from Rex (no live \
                 Claude session by that name); if it was closed: tower task unassign 1, or set \
                 the next worker."
            ]
        );
        assert!(f.store().load().unwrap().inbox.need_you.is_empty());
        assert!(f.tick(3).is_empty());
        assert!(
            f.tick(60).is_empty(),
            "never nudged: there is no one to nudge"
        );
        assert!(f.prompts().is_empty());
        f.post(1, "dev-1", 60);
        f.agents(&[("dev-1", "working"), ("dev-2", "working")]);
        assert_eq!(f.tick(61), ["#1's orphaned episode for dev-1 ended"]);
        assert_eq!(f.system(1).len(), 1, "no note: only a prompt's end has one");
        f.agents(&[("dev-2", "working")]);
        f.tick(62);
        f.tick(62);
        assert_eq!(f.tick(62), ["flagged dev-1 on #1 (orphaned) in T1"]);
        // A waiting task's worker not started yet is not orphaned.
        f.store().set_task_stage(TaskId(1), Stage::Waiting).unwrap();
        assert_eq!(f.tick(63), ["#1's orphaned episode for dev-1 ended"]);
        assert!(f.tick(64).is_empty());
    }

    #[test]
    fn an_empty_worker_list_orphans_no_one() {
        let mut f = Fixture::new();
        // Rex with no Claude in it, then with only a Claude whose session Claude no longer
        // lists: no workers either way.
        f.rex.server(&[(
            "session:team",
            &[("block:0", "com.superlogical.terminal", "")],
        )]);
        for m in 0..5 {
            assert!(f.tick(m).is_empty(), "look {m}");
        }
        f.agents(&[]);
        for m in 5..10 {
            assert!(f.tick(m).is_empty(), "look {m}");
        }
        assert!(f.store().flags().unwrap().is_empty());
    }

    #[test]
    fn the_lead_check_asks_no_terminal_and_claude_only_for_a_lead_with_no_waiter() {
        let mut f = Fixture::new();
        // No rex answers: the workers' look is skipped, the lead check is not.
        let none = crate::terminal::module(f.dir.path().join("no-rex"));
        f.watch = Watch::new(&f.db, none, Arc::default());
        assert!(f.watch.tick(f.t0).is_err());
        f.store()
            .attach_lead(crate::board::RepoId(1), "sess-1")
            .unwrap();
        // A fake claude: notes each ask, lists only another session.
        let claude = f.dir.path().join("claude");
        crate::test_exe::executable(
            &claude,
            "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$*\" >> \"$d/claude.log\"\n\
             echo '[{\"sessionId\":\"sess-x\",\"kind\":\"interactive\",\"status\":\"idle\"}]'\n",
        );
        let asked = || std::fs::read_to_string(f.dir.path().join("claude.log")).unwrap_or_default();
        // Its waiter runs: nothing, and claude is not asked.
        assert!(f.watch.leads(&claude, f.t0).is_empty());
        assert_eq!(asked(), "");
        // Its waiter ended and Claude no longer lists its session: not listening.
        f._listening.unlock().unwrap();
        assert_eq!(
            f.watch.leads(&claude, f.t0),
            [
                "Catalogue, the lead of R1, is not listening (its Claude session is gone); told \
                 the owner in T2"
            ]
        );
        assert_eq!(asked(), "agents --json\n");
    }

    #[test]
    fn a_restart_keeps_the_quiet_clock_of_a_task_with_no_post() {
        let mut f = Fixture::new();
        let mut store = f.store();
        let t = store.add_task(MilestoneId(1), "Silent one").unwrap();
        assign(&mut store, t, "qa");
        f.agents(&[("dev-1", "working"), ("qa", "idle")]);
        assert!(f.tick(10).is_empty(), "first seen at 10");
        // A new watch on the same board, as after a restart, 6 min later.
        f.watch = Watch::new(&f.db, team(f.dir.path()), Arc::default());
        assert_eq!(
            f.tick(16),
            ["nudged qa on #2 (stopped): no post on #2 for 6 min, post your status"]
        );
    }

    #[test]
    fn a_store_failure_never_repeats_a_post_or_a_prompt() {
        let mut f = Fixture::new();
        let store = f.store();
        // Flags cannot be recorded: the orphan flag is not posted, nor the nudge sent.
        store.exec_sql(
            "CREATE TRIGGER no_flags BEFORE INSERT ON watch_flag \
             BEGIN SELECT RAISE(ABORT, 'broken on purpose'); END;",
        );
        f.agents(&[("dev-1", "idle")]);
        let log = f.tick(6);
        assert_eq!(log.len(), 1);
        assert!(log[0].starts_with("failed: nudged dev-1"), "{log:?}");
        assert!(log[0].contains("broken on purpose"), "{log:?}");
        assert!(f.prompts().is_empty() && f.system(1).is_empty());
        store.exec_sql("DROP TRIGGER no_flags;");
        // The nudge's record cannot be posted after the prompt went: no second prompt.
        store.exec_sql(
            "CREATE TRIGGER no_posts BEFORE INSERT ON message \
             BEGIN SELECT RAISE(ABORT, 'broken on purpose'); END;",
        );
        assert!(f.tick(7)[0].starts_with("failed: nudged dev-1"));
        assert!(f.tick(8).is_empty(), "no second prompt");
        // Telling the lead fails whole: it is tried again, and posted once.
        assert!(f.tick(9)[0].starts_with("failed: told the lead"));
        store.exec_sql("DROP TRIGGER no_posts;");
        assert_eq!(f.prompts().len(), 1);
        assert_eq!(
            f.tick(10),
            ["told the lead of #1's unanswered nudge (stopped) in T1"]
        );
        assert!(f.tick(11).is_empty());
        assert_eq!(f.prompts().len(), 1);
        assert_eq!(
            f.system(1).len(),
            1,
            "the lead once; the lost record stays lost"
        );
    }

    #[test]
    fn a_stop_takes_effect_before_the_next_terminal_call() {
        let mut f = Fixture::new();
        let stop: Arc<AtomicBool> = Arc::default();
        f.watch = Watch::new(&f.db, team(f.dir.path()), Arc::clone(&stop));
        stop.store(true, Ordering::Relaxed);
        assert!(f.tick(60).is_empty());
        assert!(f.prompts().is_empty() && f.store().flags().unwrap().is_empty());
        assert!(f.rex.calls().is_empty(), "{:?}", f.rex.calls());
    }

    #[test]
    fn a_rex_that_cannot_be_read_skips_the_look_rather_than_orphaning_everyone() {
        let mut f = Fixture::new();
        f.agents(&[("dev-2", "working")]);
        f.rex
            .write("fail-session.list", "Error: the server is not running");
        for _ in 0..ORPHANED_AFTER + 1 {
            let err = f.watch.tick(f.t0).unwrap_err();
            assert_eq!(
                format!("{err:#}"),
                "rex api call session.list exited 1: Error: the server is not running"
            );
        }
        // One block that cannot be read is the whole look: never a list without it.
        std::fs::remove_file(f.rex.dir.join("fail-session.list")).unwrap();
        f.rex.write("fail-block:0", "Error: no block");
        assert!(f.watch.tick(f.t0).is_err());
        std::fs::remove_file(f.rex.dir.join("fail-block:0")).unwrap();
        f.rex.write("claude.json", "not json");
        let err = f.watch.tick(f.t0).unwrap_err();
        assert!(
            format!("{err:#}").contains("claude agents --json said something unexpected"),
            "{err:#}"
        );
        std::fs::remove_file(f.rex.dir.join("rex")).unwrap();
        let err = f.watch.tick(f.t0).unwrap_err();
        assert!(format!("{err:#}").contains("cannot run"), "{err:#}");
        assert!(f.system(1).is_empty());
        assert!(f.store().flags().unwrap().is_empty());
    }

    #[test]
    fn a_prompts_need_you_item_quotes_what_it_asks_on_one_line_capped() {
        let mut f = Fixture::new();
        f.asking(&[(
            "dev-1",
            "blocked",
            Some("approve Bash:\u{1b}[31m touch\n  notes.txt\u{200b}"),
        )]);
        assert_eq!(f.tick(0), ["flagged dev-1 on #1 (prompt) in T1"]);
        assert_eq!(
            f.system(1),
            [
                "question: @manager dev-1 on #1 is waiting on a prompt in Rex that only you can \
                 answer (@cat-lead for your information). It asks: approve Bash: [31m touch \
                 notes.txt"
            ]
        );
        assert_eq!(
            asks(Some(&"x".repeat(ASKS_CHARS))),
            Some("x".repeat(ASKS_CHARS))
        );
        let long = format!("{} tail", "word ".repeat(60));
        let cut = asks(Some(&long)).unwrap();
        assert_eq!(cut.chars().count(), ASKS_CHARS, "{cut}");
        assert!(
            cut.starts_with("word word") && cut.ends_with("..."),
            "{cut}"
        );
        assert!(!cut.contains("tail"), "{cut}");
        assert_eq!(asks(Some(" \u{7}\n ")), None);
        assert_eq!(asks(None), None);
    }

    #[test]
    fn an_unknown_state_is_never_idle_so_never_stopped() {
        let mut f = Fixture::new();
        f.agents(&[("dev-1", "error")]);
        for m in [6, 31, 120] {
            assert!(f.tick(m).is_empty(), "at {m}");
        }
        assert!(f.prompts().is_empty());
        // Still live: not orphaned either.
        assert!(f.store().flags().unwrap().is_empty());
    }

    #[test]
    fn a_task_with_no_post_counts_from_when_the_watch_first_saw_it() {
        let mut f = Fixture::new();
        let mut store = f.store();
        let t = store.add_task(MilestoneId(1), "Silent one").unwrap();
        assign(&mut store, t, "qa");
        f.agents(&[("dev-1", "working"), ("qa", "idle")]);
        assert!(f.tick(10).is_empty(), "first seen now");
        assert!(f.tick(14).is_empty());
        assert_eq!(
            f.tick(15),
            ["nudged qa on #2 (stopped): no post on #2 for 5 min, post your status"]
        );
        // With no thread linked yet, the nudge's record opens one on the lead's board,
        // and the lead's flag follows in it.
        assert_eq!(
            f.tick(17),
            ["told the lead of #2's unanswered nudge (stopped) in T2"]
        );
        assert_eq!(f.system(2).len(), 2);
        let thread = f.store().thread(ThreadId(2)).unwrap();
        assert_eq!((thread.task, thread.by.as_str()), (Some(t), "system"));
        assert_eq!(thread.title, "Watch on #2");
    }

    #[test]
    fn a_blocked_task_is_not_nudged_and_reassigning_ends_an_episode() {
        let mut f = Fixture::new();
        f.store()
            .block_task(TaskId(1), "printer has not sent the covers")
            .unwrap();
        assert!(f.tick(60).is_empty(), "it said it is stuck: the lead knows");
        assert!(f.prompts().is_empty() && f.system(1).is_empty());
        f.store().unblock_task(TaskId(1)).unwrap();
        assert_eq!(f.tick(61).len(), 1);
        assign(&mut f.store(), TaskId(1), "dev-2");
        f.agents(&[("dev-2", "idle")]);
        let log = f.tick(62);
        assert_eq!(log[0], "#1's stopped episode for dev-1 ended");
        assert_eq!(
            &log[1..],
            ["nudged dev-2 on #1 (stopped): no post on #1 for 62 min, post your status"]
        );
    }

    #[test]
    fn standalone_and_unassigned_tasks_are_not_watched() {
        let mut f = Fixture::new();
        let mut store = f.store();
        let loose = store.add_standalone_task("Loose", None).unwrap();
        store.set_task_stage(loose, Stage::InProgress).unwrap();
        assign(&mut store, loose, "ghost");
        // Between steps: In progress, unassigned, blocked with its reason.
        let t = store.add_task(MilestoneId(1), "Nobody's").unwrap();
        store.set_task_stage(t, Stage::InProgress).unwrap();
        store.block_task(t, "pipeline running").unwrap();
        f.agents(&[("dev-1", "working")]);
        assert!(f.tick(10).is_empty());
        let err = (store.watch_step(loose, FlagChange::None, Some("x"), f.t0)).unwrap_err();
        assert!(err.to_string().contains("on no lead's board"), "{err}");
    }

    #[test]
    fn restarts_when_its_path_resolves_to_another_file_once_there_and_not_retrying_at_once() {
        let id = |path: &str, len| ExeId {
            path: path.into(),
            modified: None,
            len,
        };
        let old = id("/opt/homebrew/Cellar/control-tower/0.1.1/bin/tower", 10);
        let new = id("/opt/homebrew/Cellar/control-tower/0.1.2/bin/tower", 10);
        assert!(!restart_due(&old, Some(&old), None), "unchanged");
        assert!(
            restart_due(&old, Some(&new), None),
            "repointed to a new Cellar"
        );
        let rebuilt = id("/opt/homebrew/Cellar/control-tower/0.1.1/bin/tower", 11);
        assert!(
            restart_due(&old, Some(&rebuilt), None),
            "same file, new contents"
        );
        assert!(
            !restart_due(&old, None, None),
            "mid-upgrade: wait for the next check"
        );
        let secs = Duration::from_secs;
        assert!(
            !restart_due(&old, Some(&new), Some(secs(59))),
            "failed under a minute ago"
        );
        assert!(
            restart_due(&old, Some(&new), Some(secs(60))),
            "retried after a minute"
        );
    }

    #[test]
    fn an_exe_id_follows_the_symlink_and_sees_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, link) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("l"),
        );
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "1").unwrap();
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let first = ExeId::of(&link).unwrap();
        assert_eq!(first, ExeId::of(&a).unwrap());
        std::fs::remove_file(&link).unwrap();
        assert_eq!(ExeId::of(&link), None);
        std::os::unix::fs::symlink(&b, &link).unwrap();
        assert_ne!(ExeId::of(&link).unwrap(), first);
    }
}
