//! Live delivery: every post reaches who it is for while they work, run by `tower
//! watch` on its own thread every `DELIVER_EVERY`. The one path for posts from the CLI, the
//! watch and the dashboard alike: the board keeps a cursor, the last message id every
//! recipient has been offered, and how far each recipient was sent beyond it, recorded
//! before each send (`Store::mark_sent`), so a post reaches each recipient at most once.
//! A round sends to workers within `WORKER_BUDGET` and checks for a stop before each send;
//! what it did not get to (out of time, stopped, the terminal not answering its agent list)
//! stays after the cursor and goes next round, so a stalled terminal never holds a stop or
//! an upgrade for long and loses nothing. The terminal is reached only through the
//! terminal module (`terminal`): its agents, and its prompt to one by name.
//!
//! Who gets a post (`plan`): never a lead, who hears of its board's posts through its own
//! board waiter and Stop hook (`listen`), but:
//! - Each worker a post @mentions (`board::mentioned`), and the task's assignee when the
//!   lead posts in the thread of a task not done (a done task's worker has finished,
//!   and the lead closes it, often seconds after its sign-off; a post meant for it @mentions
//!   it), through `herdr agent prompt` to an agent `herdr agent
//!   list` shows by exactly that name (never a pane id or index), the text stripped of
//!   control characters first (`sanitise`), so a post cannot escape bracketed paste. Never
//!   the author, a lead or the owner, and never for a `system` post: the watch prompts
//!   workers itself.
//!
//! The posts for one recipient in one drain are one send, so a burst is one message and no
//! session's burst or repeat limits drop it.
//!
//! A name no herdr agent has and no worker on the board ever had (no task's assignee, no
//! post's author: `Pending::known`) is someone the post only seemed to name, like `@name` or
//! `@media` quoted without backticks, or a worker's name misspelt. Its failure is
//! no item for the owner: tower replies on the post's thread to its author instead
//! (`UNKNOWN_NAME`), and that reply goes to the author like any post, once. When that reply
//! cannot reach its author (closed since its post, say), the failure is only logged: no
//! item for the owner, no reply to a reply.
//!
//! Any other send that fails (herdr's `agent_blocked`, a worker the board knew that herdr no
//! longer has) opens a failure episode for its recipient, which posts one `@manager` item for
//! the owner's awareness
//! (`Store::delivery_failed`); while it is open further failures are only logged, and a send
//! that gets through ends it; one open longer than `EPISODE_EXPIRES` is told again. The item
//! is a `system` post, so it goes to no worker: it cannot start a storm. herdr stopping a
//! prompt that may have arrived (a timeout, a stall) is logged only, never resent and no
//! item.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::board::{
    DELIVERY_FAILURE, Lead, MAX_MESSAGE_CHARS, OWNER_AUTHOR, RepoId, SYSTEM_AUTHOR, TaskId,
    ThreadId, closed_worker_hint, hidden, mentioned,
};
use crate::store::Store;
use crate::terminal::{Sent, Terminal};

/// How often the watch delivers what was posted since it last did.
pub const DELIVER_EVERY: Duration = Duration::from_secs(5);
/// The most characters one send to a worker carries; the rest is named, not typed.
pub const WORKER_CAP: usize = 8000;
/// How long one round may spend sending to workers; the rest goes next round.
pub const WORKER_BUDGET: Duration = Duration::from_secs(10);
/// How long a failure episode stays told: a recipient still failing after this is told
/// again, with a new item.
pub const EPISODE_EXPIRES: i64 = 60 * 60;
/// The most characters of a failure's reason a failure item quotes.
const WHY_CHARS: usize = 300;
/// How tower's reply to a post's author starts after "@<author> ", when the post named
/// someone no worker on the board is: the reply goes to that author, and only to it.
pub const UNKNOWN_NAME: &str = "no worker is named ";
/// The most names one such reply lists.
const UNKNOWN_NAMES_MAX: usize = 10;

/// A post to deliver, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub id: i64,
    pub thread: ThreadId,
    /// The repo whose board it is on.
    pub repo: RepoId,
    /// The task its thread is linked to, if any.
    pub task: Option<TaskId>,
    pub author: String,
    pub text: String,
}

/// A lead's board: its repo and its lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadBoard {
    pub repo: RepoId,
    pub lead: Lead,
}

impl LeadBoard {
    fn is_lead(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case(&self.lead.session) || name.eq_ignore_ascii_case(&self.lead.name)
    }
}

/// What one drain delivers: the posts after the cursor, every lead's board, the tasks not
/// done with their assignees by repo, and how far each recipient (`Recipient::key`) was
/// sent beyond the cursor already.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub posts: Vec<Post>,
    pub leads: Vec<LeadBoard>,
    /// Each task not done that has an assignee: its id, its repo (`None` for a task in no
    /// repo) and its assignee.
    pub open: Vec<(TaskId, Option<RepoId>, String)>,
    pub sent: HashMap<String, i64>,
    /// Every name a worker has had on the board, lower case: each task's assignee and each
    /// author of a post but tower and the owner, never a lead taken off the board.
    pub known: HashSet<String>,
}

impl Pending {
    /// The assignee of `task` while it is not done: who a lead's post in its thread goes to.
    fn assignee(&self, task: TaskId) -> Option<&str> {
        (self.open.iter())
            .find(|(t, _, _)| *t == task)
            .map(|(_, _, a)| a.as_str())
    }

    /// Whether a worker on the board was ever named `name`, ignoring case.
    fn knows(&self, name: &str) -> bool {
        self.known.contains(&name.to_ascii_lowercase())
    }

    /// The tasks on `repo`'s board, not done and in id order, whose assignee is the worker
    /// `name` (ignoring case): the ones a failure to reach it there says how to clear.
    fn assigned_to(&self, name: &str, repo: RepoId) -> Vec<TaskId> {
        let mut tasks: Vec<TaskId> = (self.open.iter())
            .filter(|(_, r, a)| *r == Some(repo) && a.eq_ignore_ascii_case(name))
            .map(|(t, _, _)| *t)
            .collect();
        tasks.sort_by_key(|t| t.0);
        tasks
    }
}

/// Who a send is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// A worker, by its herdr agent name: lower case, as herdr requires.
    Worker(String),
}

impl Recipient {
    /// Its key in the board's failure episodes: a worker's name ignoring case.
    pub fn key(&self) -> String {
        match self {
            Recipient::Worker(name) => name.to_ascii_lowercase(),
        }
    }
}

/// One send: a recipient and the posts for it, as indexes into `Pending::posts`, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub to: Recipient,
    pub posts: Vec<usize>,
}

/// Who each pending post goes to, as one batch per worker, in the order a post first
/// named them. A recipient is not sent a post it was sent already (`Pending::sent`).
pub fn plan(pending: &Pending) -> Vec<Batch> {
    let mut workers: Vec<Batch> = Vec::new();
    let is_any_lead = |name: &str| pending.leads.iter().any(|l| l.is_lead(name));
    for (i, post) in pending.posts.iter().enumerate() {
        let Some(board) = pending.leads.iter().find(|l| l.repo == post.repo) else {
            continue;
        };
        let mut names: Vec<&str> = if post.author.eq_ignore_ascii_case(SYSTEM_AUTHOR) {
            // Tower's own posts reach no worker, but its reply to an author reaches that one.
            match replied_to(&post.text) {
                Some(author) => vec![author],
                None => continue,
            }
        } else {
            mentioned(&post.text)
        };
        if board.is_lead(&post.author)
            && let Some(assignee) = post.task.and_then(|t| pending.assignee(t))
        {
            names.push(assignee);
        }
        let mut named: Vec<&str> = Vec::new();
        for name in names {
            // A name must start with a letter or digit, so no post puts an option ("@--help")
            // in herdr's arguments.
            let skip = !name.starts_with(|c: char| c.is_ascii_alphanumeric())
                || name.eq_ignore_ascii_case(&post.author)
                || name.eq_ignore_ascii_case(OWNER_AUTHOR)
                || name.eq_ignore_ascii_case(SYSTEM_AUTHOR)
                || is_any_lead(name)
                || named.iter().any(|n| n.eq_ignore_ascii_case(name));
            if !skip {
                named.push(name);
            }
        }
        for name in named {
            let found = (workers.iter_mut())
                .find(|b| matches!(&b.to, Recipient::Worker(w) if w.eq_ignore_ascii_case(name)));
            match found {
                Some(b) => b.posts.push(i),
                // herdr names are lower case and matched exactly: `@Senior` is senior.
                None => workers.push(Batch {
                    to: Recipient::Worker(name.to_ascii_lowercase()),
                    posts: vec![i],
                }),
            }
        }
    }
    for b in &mut workers {
        let done = pending.sent.get(&b.to.key()).copied().unwrap_or(i64::MIN);
        b.posts.retain(|&i| pending.posts[i].id > done);
    }
    workers.retain(|b| !b.posts.is_empty());
    workers
}

/// The author tower's reply `text` is for, when it is one (`UNKNOWN_NAME`).
fn replied_to(text: &str) -> Option<&str> {
    let (to, rest) = text.strip_prefix('@')?.split_once(' ')?;
    rest.starts_with(UNKNOWN_NAME).then_some(to)
}

/// Tower's reply to `author` for a post that named `names`, none of them a worker on the
/// board: addressed with a mention when the author's name is one (a lead's name with
/// spaces is not; its board waiter brings it the reply anyway).
fn unknown_text(author: &str, names: &[String], terminal: &str) -> String {
    let to = if mentioned(&format!("@{author}")) == [author] {
        format!("@{author}")
    } else {
        format!("{author}:")
    };
    let mut quoted: Vec<String> = (names.iter().take(UNKNOWN_NAMES_MAX))
        .map(|n| format!("`@{n}`"))
        .collect();
    if names.len() > UNKNOWN_NAMES_MAX {
        quoted.push(format!("{} more", names.len() - UNKNOWN_NAMES_MAX));
    }
    let list = match quoted.as_slice() {
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
        [] => String::new(),
    };
    format!(
        "{to} {UNKNOWN_NAME}{list}, so your post reached no one by that name: {terminal} has no \
         such agent and no worker on this board has had the name. If you meant a worker, check \
         the name; if you were quoting, put it in backticks."
    )
}

/// `text` as it is safe to type into a pane: every control character but newline dropped
/// (tab made a space), so no escape sequence can end bracketed paste or drive the
/// terminal, and every character `board::hidden` names (format, line and paragraph
/// separators) dropped too.
pub fn sanitise(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if hidden(&c) => None,
            c => Some(c),
        })
        .collect()
}

/// The text of one send: `head`, then each post as "[T3 #12] dev-2: text", as many as fit
/// in `cap` characters, then how many more there are and the threads they are in.
fn compose(head: &str, posts: &[&Post], cap: usize) -> String {
    let mut text = head.to_string();
    let mut chars = text.chars().count();
    for (i, p) in posts.iter().enumerate() {
        let about = p.task.map(|t| format!(" {t}")).unwrap_or_default();
        let one = format!("\n\n[{}{about}] {}: {}", p.thread, p.author, p.text);
        let rest = &posts[i..];
        let mut threads: Vec<String> = Vec::new();
        for t in rest.iter().map(|p| p.thread.to_string()) {
            if !threads.contains(&t) {
                threads.push(t);
            }
        }
        // Room is kept for the line saying how many more there are.
        let tail = format!(
            "\n\nand {} more on the board, in {} (tower thread show T<n>).",
            rest.len(),
            threads.join(", ")
        );
        let len = one.chars().count();
        if chars + len + tail.chars().count() > cap && i > 0 {
            text.push_str(&tail);
            return text;
        }
        text.push_str(&one);
        chars += len;
    }
    text
}

/// What a worker receives, sanitised: the posts for it, and how to answer.
pub fn worker_text(posts: &[&Post]) -> String {
    let n = posts.len();
    let s = if n == 1 { "" } else { "s" };
    let head = format!("control-tower: {n} post{s} for you; answer on the board with tower post.");
    sanitise(&compose(&head, posts, WORKER_CAP))
}

#[derive(serde::Deserialize)]
struct Session {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    name: Option<String>,
}

/// Whether `claude agents --json`'s list (`json`) has the Claude session `session`: a
/// session it lists is running.
pub fn listed(json: &str, session: &str) -> Result<bool> {
    let sessions: Vec<Session> = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("claude agents --json said something unexpected: {e}"))?;
    Ok((sessions.iter()).any(|s| s.session_id.as_deref() == Some(session)))
}

/// Checks that `session`, the Claude session asking to attach, is the lead `lead` (its
/// registered session name), by `claude agents --json`'s list (`json`): the session must
/// be listed under that name, ignoring case. So no other session can take the lead's
/// posts.
pub fn check_asker(json: &str, session: &str, lead: &Lead, repo: RepoId) -> Result<()> {
    let sessions: Vec<Session> = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("claude agents --json said something unexpected: {e}"))?;
    let Some(found) = (sessions.iter()).find(|s| s.session_id.as_deref() == Some(session)) else {
        anyhow::bail!(
            "claude agents --json lists no session {session}; run tower lead attach from the \
             lead's own Claude session"
        );
    };
    match found.name.as_deref() {
        Some(name) if name.eq_ignore_ascii_case(&lead.session) => Ok(()),
        name => anyhow::bail!(
            "this Claude session is named {}, not {}, the session the lead of {repo} \
             registered; only the lead attaches its session",
            name.map_or("nothing".into(), |n| format!("\"{n}\"")),
            format_args!("\"{}\"", lead.session)
        ),
    }
}

/// One recipient a drain could not reach, as the failure item names it: who and why, as
/// "dev-1: <why>", and how to clear it when it is gone from herdr and the assignee of
/// `tasks` (not done, on the item's board).
fn failure_line(to: &Recipient, why: &str, tasks: &[TaskId]) -> String {
    let why: String = why.chars().take(WHY_CHARS).collect();
    let hint = match tasks {
        [] => String::new(),
        tasks => format!("; {}", closed_worker_hint(tasks)),
    };
    match to {
        Recipient::Worker(name) => format!("{name}: {why}{hint}"),
    }
}

/// The one delivery failure item of a drain (for the owner's awareness, as it starts with
/// `DELIVERY_FAILURE`), naming the recipients whose failure episodes it
/// opened (`lines`), as many as fit in a board message.
fn failure_text(lines: &[&str]) -> String {
    if let [line] = lines {
        return format!("{DELIVERY_FAILURE}{line}. Its posts stay on the board.");
    }
    let mut text = format!(
        "{DELIVERY_FAILURE}{} recipients; their posts stay on the board:",
        lines.len()
    );
    for (i, line) in lines.iter().enumerate() {
        let one = format!("\n- {line}");
        // Room for the last line; the watch log names every one.
        if text.chars().count() + one.chars().count() > MAX_MESSAGE_CHARS - 100 {
            text.push_str(&format!(
                "\n- and {} more, in the watch's log",
                lines.len() - i
            ));
            break;
        }
        text.push_str(&one);
    }
    text
}

/// The delivery side of the watch: the board it drains and the terminal it prompts through.
pub struct Deliver {
    db: PathBuf,
    terminal: Arc<dyn Terminal>,
    /// Set by SIGTERM or Ctrl-C: checked between drains.
    stop: Arc<AtomicBool>,
    /// Held through each drain, from taking the posts to the last send: the watch takes it
    /// before it restarts itself (an exec ends every thread), so an upgrade never drops
    /// posts already taken.
    gate: Arc<Mutex<()>>,
    /// How long a round may spend on workers (`WORKER_BUDGET`; shorter in tests).
    budget: Duration,
    /// Why the terminal's agent list last failed, said once until it answers or the reason
    /// changes.
    unread_said: Mutex<Option<String>>,
}

impl Deliver {
    pub fn new(db: &Path, terminal: Arc<dyn Terminal>, stop: Arc<AtomicBool>) -> Deliver {
        Deliver {
            db: db.to_path_buf(),
            terminal,
            stop,
            gate: Arc::default(),
            budget: WORKER_BUDGET,
            unread_said: Mutex::default(),
        }
    }

    /// The gate a drain holds: whoever holds it knows no drain is between taking posts
    /// and sending them.
    pub fn gate(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.gate)
    }

    /// Delivers every post since the last drain, at `now`, passing `log` a line per send,
    /// all with the gate held (from taking the posts to the last line logged), so a
    /// restart never execs mid-drain. Fails, sending nothing, when the board cannot be
    /// read; a single send or record that fails is logged and the rest still go.
    pub fn drain(&self, now: i64, log: &dyn Fn(&str)) -> Result<()> {
        // A drain that panicked left nothing half done on the board: carry on.
        let _held = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        self.send_round(now)?.iter().for_each(|l| log(l));
        Ok(())
    }

    /// One round of `drain`, its gate held: the lines to log.
    fn send_round(&self, now: i64) -> Result<Vec<String>> {
        let mut store = Store::open(&self.db)?;
        let pending = store.take_pending()?;
        let mut log = Vec::new();
        let deadline = Instant::now() + self.budget;
        // The first post (index) some recipient was not offered this round: the cursor
        // stops before it.
        let mut left: Option<usize> = None;
        // The terminal's agent names, asked once, before the first worker send.
        let mut agents: Option<Result<Vec<String>, String>> = None;
        // The recipients not reached, by board: the thread of the board's first failure,
        // then (key, line for the item, log line) for each, told in one item per board.
        type Failed = (RepoId, ThreadId, Vec<(String, String, String)>);
        let mut failed: Vec<Failed> = Vec::new();
        // The names no worker on the board has had, by post (index): its author is told.
        let mut unknown: Vec<(usize, Vec<String>)> = Vec::new();
        for batch in plan(&pending) {
            let posts: Vec<&Post> = batch.posts.iter().map(|&i| &pending.posts[i]).collect();
            let remaining = deadline.saturating_duration_since(Instant::now());
            let waits = self.stop.load(Ordering::Relaxed) || remaining.is_zero();
            if !waits && agents.is_none() {
                let timeout = remaining.min(crate::watch::terminal_timeout());
                let listed = self.terminal.agents(&self.db, timeout);
                agents = Some(
                    (listed.map(|a| a.into_iter().map(|a| a.name).collect()))
                        .map_err(|e| e.to_string()),
                );
                self.say_unread(agents.as_ref(), &mut log);
            }
            let unread = matches!(agents, Some(Err(_)));
            if waits || unread {
                left = Some(left.map_or(batch.posts[0], |l| l.min(batch.posts[0])));
                continue;
            }
            let n = posts.len();
            let key = batch.to.key();
            let last = posts.last().map_or(0, |p| p.id);
            match store.mark_sent(&key, last) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    log.push(format!("did not send {n} post(s) to {}: {e:#}", key));
                    left = Some(left.map_or(batch.posts[0], |l| l.min(batch.posts[0])));
                    continue;
                }
            }
            // Whether the terminal has no agent by the name: only then can it have been closed.
            let (who, sent, gone) = match &batch.to {
                Recipient::Worker(name) => {
                    let listed = matches!(&agents, Some(Ok(a)) if a.iter().any(|a| a == name));
                    let sent = if listed {
                        let timeout = deadline
                            .saturating_duration_since(Instant::now())
                            .min(crate::watch::terminal_timeout())
                            .max(Duration::from_millis(100));
                        let text = worker_text(&posts);
                        self.terminal.prompt(&self.db, name, &text, timeout)
                    } else {
                        let terminal = self.terminal.name();
                        Sent::Failed(format!("{terminal} has no agent named {name}"))
                    };
                    (name.clone(), sent, !listed)
                }
            };
            log.push(match sent {
                Sent::Delivered => match store.delivered(&key) {
                    Ok(true) => format!("delivered {n} post(s) to {who}, reachable again"),
                    Ok(false) => format!("delivered {n} post(s) to {who}"),
                    Err(e) => format!("delivered {n} post(s) to {who}; failed to record it: {e:#}"),
                },
                Sent::Unsure(why) => {
                    format!("sent {n} post(s) to {who}, not confirmed ({why}); not resent")
                }
                // Tower's reply to an author that is gone by now (closed after its post):
                // advice no one else needs, so it is logged, never the owner's item.
                Sent::Failed(why)
                    if posts
                        .iter()
                        .all(|p| p.author.eq_ignore_ascii_case(SYSTEM_AUTHOR)) =>
                {
                    format!("could not deliver tower's reply to {who}: {why}; logged only")
                }
                Sent::Failed(why) if gone && !pending.knows(&who) => {
                    for &i in &batch.posts {
                        match unknown.iter_mut().find(|(p, _)| *p == i) {
                            Some((_, names)) => names.push(who.clone()),
                            None => unknown.push((i, vec![who.clone()])),
                        }
                    }
                    format!(
                        "could not deliver {n} post(s) to {who}: {why}, a name no worker on \
                         the board has had; no item for the owner"
                    )
                }
                Sent::Failed(why) => {
                    let (m, thread) = (posts[0].repo, posts[0].thread);
                    let at = match failed.iter().position(|(b, _, _)| *b == m) {
                        Some(i) => i,
                        None => {
                            failed.push((m, thread, Vec::new()));
                            failed.len() - 1
                        }
                    };
                    let tasks = if gone {
                        pending.assigned_to(&who, m)
                    } else {
                        Vec::new()
                    };
                    let line = failure_line(&batch.to, &why, &tasks);
                    let said = format!("could not deliver {n} post(s) to {who}: {why}");
                    failed[at].2.push((key, line, said));
                    continue;
                }
            });
        }
        let upto = match left {
            Some(i) => pending.posts[i].id - 1,
            None => pending.posts.last().map_or(0, |p| p.id),
        };
        if let Err(e) = store.advance_delivery(upto) {
            log.push(format!("could not move the delivery cursor: {e:#}"));
        }
        if let Some(i) = left {
            let n = pending.posts.len() - i;
            log.push(format!("{n} post(s) wait for the next round"));
        }
        unknown.sort_by_key(|(i, _)| *i);
        for (i, names) in &unknown {
            let post = &pending.posts[*i];
            if post.author.eq_ignore_ascii_case(SYSTEM_AUTHOR) {
                log.push(format!(
                    "tower's post {} named no worker; not told",
                    post.id
                ));
                continue;
            }
            let text = unknown_text(&post.author, names, self.terminal.name());
            log.push(match store.tower_reply(post.thread, &text, now) {
                Ok(true) => format!(
                    "told {} in {} of the name(s) no worker has had",
                    post.author, post.thread
                ),
                Ok(false) => format!(
                    "did not tell {} of the name(s) no worker has had: {} is resolved",
                    post.author, post.thread
                ),
                Err(e) => format!("failed to tell {} in {}: {e:#}", post.author, post.thread),
            });
        }
        for (_, thread, failed) in &failed {
            let thread = *thread;
            let keys: Vec<&str> = failed.iter().map(|(k, _, _)| k.as_str()).collect();
            let text = |opened: &[usize]| {
                let lines: Vec<&str> = opened.iter().map(|&i| failed[i].1.as_str()).collect();
                failure_text(&lines)
            };
            match store.delivery_failed(&keys, thread, now, text) {
                Ok(told) => {
                    for (i, (_, _, said)) in failed.iter().enumerate() {
                        log.push(match &told {
                            Some((t, opened)) if opened.contains(&i) => {
                                format!("{said}; told the owner in {t}")
                            }
                            _ => format!("{said} (the owner was told already)"),
                        });
                    }
                }
                Err(e) => (failed.iter()).for_each(|(_, _, said)| {
                    log.push(format!("{said}; failed to tell the owner: {e:#}"))
                }),
            }
        }
        Ok(log)
    }

    /// Logs why the terminal's agent list failed, once until it answers or the reason
    /// changes.
    fn say_unread(&self, agents: Option<&Result<Vec<String>, String>>, log: &mut Vec<String>) {
        let mut said = self
            .unread_said
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match agents {
            Some(Err(why)) if said.as_ref() != Some(why) => {
                let terminal = self.terminal.name();
                log.push(format!(
                    "{why}; posts for workers wait until {terminal} answers"
                ));
                *said = Some(why.clone());
            }
            Some(Ok(_)) => *said = None,
            _ => {}
        }
    }

    /// Drains every `DELIVER_EVERY` until stopped, passing each line to `log`.
    /// A board that cannot be read is said once, until it can be or the reason changes.
    pub fn run(&self, log: impl Fn(&str)) {
        let mut said: Option<String> = None;
        while !self.stop.load(Ordering::Relaxed) {
            match self.drain(chrono::Utc::now().timestamp(), &log) {
                Ok(()) => said = None,
                Err(e) => {
                    let why = format!("skipped a delivery: {e:#}");
                    if said.as_ref() != Some(&why) {
                        log(&why);
                        said = Some(why);
                    }
                }
            }
            let until = std::time::Instant::now() + DELIVER_EVERY;
            while !self.stop.load(Ordering::Relaxed) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOf, Stage};
    use crate::store::TaskFields;
    use crate::watch::{TEST_WAIT, hold_until_released};

    fn lead(repo: i64, name: &str, session: &str) -> LeadBoard {
        LeadBoard {
            repo: RepoId(repo),
            lead: Lead {
                name: name.into(),
                session: session.into(),
                colour: None,
            },
        }
    }

    fn post(id: i64, task: Option<i64>, author: &str, text: &str) -> Post {
        Post {
            id,
            thread: ThreadId(1),
            repo: RepoId(1),
            task: task.map(TaskId),
            author: author.into(),
            text: text.into(),
        }
    }

    fn pending(posts: Vec<Post>) -> Pending {
        Pending {
            posts,
            leads: vec![
                lead(1, "Catalogue", "cat-lead"),
                lead(2, "Other", "other-lead"),
            ],
            open: vec![(TaskId(1), Some(RepoId(1)), "dev-1".to_string())],
            sent: HashMap::new(),
            known: HashSet::new(),
        }
    }

    fn batch(to: Recipient, posts: &[usize]) -> Batch {
        Batch {
            to,
            posts: posts.to_vec(),
        }
    }

    fn worker(name: &str) -> Recipient {
        Recipient::Worker(name.into())
    }

    #[test]
    fn no_post_is_planned_for_a_lead() {
        let p = pending(vec![
            post(1, Some(1), "dev-1", "status: on it"),
            post(2, Some(1), "Cat-Lead", "go on"),
            post(3, None, "system", "@manager dev-1 is waiting on a prompt"),
            post(4, None, "manager", "approved"),
            post(5, None, "Catalogue", "noted"),
        ]);
        assert_eq!(plan(&p), [batch(worker("dev-1"), &[1])]);
    }

    #[test]
    fn mentions_and_the_leads_task_posts_reach_workers_never_the_author_leads_or_owner() {
        let mut p = pending(vec![
            // The lead in task #1's thread: its assignee, and who it mentions, once each.
            post(
                1,
                Some(1),
                "cat-lead",
                "@qa-one and @Dev-One please, @dev-one",
            ),
            // A worker naming itself, the owner, system, a lead of another board.
            post(
                2,
                Some(1),
                "dev-2",
                "@dev-2 @manager @system @other-lead @qa-one",
            ),
            // The lead in a thread linked to no task, and to a task with no assignee.
            post(3, None, "cat-lead", "all: carry on"),
            post(4, Some(9), "cat-lead", "noted"),
            // A quoted name is no mention.
            post(5, None, "dev-3", "`@dev-9` is an example"),
            // A name that starts like an option never reaches herdr's arguments.
            post(6, None, "dev-3", "@--help x @-x @_y"),
            // Names in any case are one worker, by its lower-case herdr name.
            post(7, None, "dev-3", "@Qa-One and @Dev-One again"),
            // Right after kana, with no space, as Japanese is written; an e-mail address
            // names no one.
            post(
                8,
                None,
                "dev-3",
                "確認後@qa-oneに渡します (x@dev-one.example)",
            ),
            // A name before `.` and kana is a mention; before `.` and ASCII it is code.
            post(9, None, "dev-3", "確認後@qa-one.よろしく @dev-one.run()"),
        ]);
        p.open = vec![(TaskId(1), Some(RepoId(1)), "dev-one".to_string())];
        assert_eq!(
            plan(&p),
            [
                batch(worker("qa-one"), &[0, 1, 6, 7, 8]),
                batch(worker("dev-one"), &[0, 6]),
            ]
        );
    }

    #[test]
    fn system_posts_reach_no_worker() {
        let p = pending(vec![post(1, Some(1), "system", "@dev-1 nudged")]);
        assert_eq!(plan(&p), []);
    }

    /// Tower's reply to a post's author reaches that author, and no name it quotes;
    /// a lead hears it through its board waiter and the owner in the inbox, so neither is sent it.
    #[test]
    fn a_towers_reply_to_an_author_reaches_that_author_only() {
        let reply = |to: &str| format!("{to} {UNKNOWN_NAME}`@name` or `@qa-9`, so ... @dev-3");
        let p = pending(vec![
            post(1, None, "system", &reply("@dev-2")),
            post(2, None, "system", &reply("@cat-lead")),
            post(3, None, "system", &reply("@manager")),
            post(4, None, "system", &reply("Catalogue Lead:")),
            post(5, None, "system", "@dev-2 was nudged: no worker is named"),
        ]);
        assert_eq!(plan(&p), [batch(worker("dev-2"), &[0])]);
    }

    #[test]
    fn sanitising_drops_every_control_character_but_newline() {
        let evil = "a\u{1b}[201~b\rc\u{7f}d\u{9b}e\u{0}f\u{200b}g\u{2028}h\ti\nj";
        assert_eq!(sanitise(evil), "a[201~bcdefgh i\nj");
        assert!(!sanitise(evil).chars().any(|c| c.is_control() && c != '\n'));
    }

    #[test]
    fn what_a_worker_is_sent_is_sanitised() {
        let evil = post(1, Some(1), "cat-lead", "a\u{1b}[201~b\u{9b}c\u{7f}d\re\nf");
        let text = worker_text(&[&evil]);
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n'),
            "{text:?}"
        );
        assert!(!text.contains("\u{1b}[201~"), "{text:?}");
        assert!(
            text.ends_with("[T1 #1] cat-lead: a[201~bcde\nf"),
            "{text:?}"
        );
    }

    #[test]
    fn a_long_batch_is_capped_and_says_how_many_more() {
        let long = "x".repeat(3000);
        let posts: Vec<Post> = (1..=3).map(|i| post(i, None, "dev-1", &long)).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        let text = worker_text(&refs);
        assert!(text.chars().count() <= WORKER_CAP, "{}", text.len());
        assert_eq!(text.matches(&long).count(), 2);
        assert!(
            text.ends_with("and 1 more on the board, in T1 (tower thread show T<n>)."),
            "{text}"
        );
        assert!(
            text.starts_with("control-tower: 3 posts for you;"),
            "{text}"
        );
        let one = worker_text(&refs[..1]);
        assert!(one.contains("[T1] dev-1: x"), "{one}");
    }

    /// A temp board with M1 led by Catalogue (session cat-lead), task #1 assigned to dev-1 and
    /// In progress with thread T1, and a fake herdr:
    /// `agent prompt` appends "name|text" (newlines as `\n`, and one more after the text)
    /// to `prompts.log`, or, when
    /// `fail-<name>` exists, prints it to stderr and exits 1; with `slow` (or `hang`), it
    /// touches `started` and sleeps a second (or 30) first. `agent list` lists the names in
    /// `agents`, or fails when `list-fails` exists.
    struct Fixture {
        dir: tempfile::TempDir,
        db: PathBuf,
        deliver: Deliver,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("board.db");
            let herdr = dir.path().join("herdr");
            crate::test_exe::executable(&herdr, "#!/bin/sh\nd=$(dirname \"$0\")\n\
                 if [ \"$1 $2\" = \"agent list\" ]; then\n\
                 [ -f \"$d/list-fails\" ] && { echo down >&2; exit 1; }\n\
                 printf '{\"result\":{\"agents\":['; sep=\n\
                 for n in $(cat \"$d/agents\"); do\n\
                 printf '%s{\"name\":\"%s\",\"agent_status\":\"idle\"}' \"$sep\" \"$n\"; sep=,; done\n\
                 printf ']}}\\n'; exit 0; fi\n\
                 [ \"$1 $2\" = \"agent prompt\" ] || exit 2\n\
                 if [ -f \"$d/fail-$3\" ]; then cat \"$d/fail-$3\" >&2; exit 1; fi\n\
                 if [ -f \"$d/hold\" ]; then : > \"$d/started\"; HOLD; : > \"$d/ended\"; fi\n\
                 printf '%s|%s\\n' \"$3\" \"$(printf '%s' \"$4\" | awk 1 ORS='\\\\n')\" \
                 >> \"$d/prompts.log\"\n"
                    .replace("HOLD", &hold_until_released()));
            // herdr's live agents; a name not listed is not sent to.
            std::fs::write(dir.path().join("agents"), "dev-1 dev-2 qa-1 qa-2 qa-3").unwrap();
            let mut store = Store::open(&db).unwrap();
            let m = store.add_milestone("MVP", "goal", None).unwrap();
            store
                .lead_milestone(m, "Catalogue", "cat-lead", None)
                .unwrap();
            let t = store.add_task(m, "Bookmarks").unwrap();
            let fields = TaskFields {
                assignee: Some("dev-1"),
                ..Default::default()
            };
            store.set_task(t, fields).unwrap();
            store.set_task_stage(t, Stage::InProgress).unwrap();
            store
                .open_thread(&BoardOf::Milestone(m), "cat-lead", Some(t), "#1", "go")
                .unwrap();
            // A round waits for the fake as long as a test waits for anything.
            let deliver = Deliver {
                budget: TEST_WAIT,
                ..Deliver::new(&db, crate::terminal::module(herdr), Arc::default())
            };
            let f = Fixture { deliver, dir, db };
            // The lead's opening post goes to the assignee; tests start after it.
            assert_eq!(f.drain(), ["delivered 1 post(s) to dev-1"]);
            std::fs::remove_file(f.dir.path().join("prompts.log")).unwrap();
            f
        }

        fn store(&self) -> Store {
            Store::open(&self.db).unwrap()
        }

        fn post(&self, who: &str, text: &str) {
            self.store().post(ThreadId(1), who, text).unwrap();
        }

        fn drain(&self) -> Vec<String> {
            self.drain_at(1_000)
        }

        fn drain_at(&self, now: i64) -> Vec<String> {
            let lines = Mutex::new(Vec::new());
            let log = |l: &str| lines.lock().unwrap().push(l.to_string());
            self.deliver.drain(now, &log).unwrap();
            lines.into_inner().unwrap()
        }

        fn prompts(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("prompts.log"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }

        fn fail(&self, name: &str, said: &str) {
            std::fs::write(self.dir.path().join(format!("fail-{name}")), said).unwrap();
        }

        /// The delivery failure items on the board: for the owner's awareness, never
        /// need-you.
        fn items(&self) -> Vec<String> {
            let board = self.store().load().unwrap();
            assert!(
                board.inbox.need_you.is_empty(),
                "{:?}",
                board.inbox.need_you
            );
            board
                .inbox
                .awareness
                .iter()
                .map(|a| a.message.text.clone())
                .collect()
        }
    }

    #[test]
    fn a_drained_post_is_never_delivered_again() {
        let f = Fixture::new();
        assert!(f.drain().is_empty());
        assert!(f.prompts().is_empty());
    }

    #[test]
    fn posts_naming_no_worker_are_sent_to_no_one_not_even_the_lead() {
        let f = Fixture::new();
        f.post("dev-1", "status: one");
        f.post("manager", "two");
        assert!(f.drain().is_empty());
        assert!(f.prompts().is_empty() && f.items().is_empty());
    }

    #[test]
    fn the_leads_task_post_reaches_the_assignee_and_the_names_it_mentions() {
        let f = Fixture::new();
        f.post("cat-lead", "go on\nnow, and @qa-1 test it");
        let log = f.drain();
        assert_eq!(
            log,
            [
                "delivered 1 post(s) to qa-1",
                "delivered 1 post(s) to dev-1"
            ]
        );
        let prompts = f.prompts();
        assert_eq!(prompts.len(), 2, "{prompts:?}");
        let want = "control-tower: 1 post for you; answer on the board with tower post.\\n\\n\
                    [T1 #1] cat-lead: go on\\nnow, and @qa-1 test it";
        assert_eq!(prompts[0], format!("qa-1|{want}\\n"));
        assert_eq!(prompts[1], format!("dev-1|{want}\\n"));
    }

    #[test]
    fn a_closed_workers_task_once_unassigned_draws_no_delivery_or_failure() {
        let f = Fixture::new();
        // dev-1 finished its step and was closed, but is still the assignee: the lead's next
        // post in the task's thread fails and is an awareness item.
        std::fs::write(f.dir.path().join("agents"), "dev-2 qa-1").unwrap();
        f.post("cat-lead", "thanks, closing you");
        let log = f.drain();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].contains("could not deliver 1 post(s) to dev-1"),
            "{log:?}"
        );
        let before = f.items();
        // It names dev-1 as #1's assignee, so it says how to clear it.
        assert_eq!(
            before,
            [
                "@manager could not deliver to dev-1: herdr has no agent named dev-1; if it was \
                 closed: tower task unassign 1, or set the next worker. Its posts stay on the \
                 board."
            ]
        );
        // That item is a system post: it goes to no one.
        assert!(f.drain().is_empty());
        // Once the lead unassigns the task, its posts there go to no one: nothing is sent,
        // nothing fails, and the card says unassigned.
        f.store().unassign(TaskId(1)).unwrap();
        f.post("cat-lead", "QA joins next");
        let log = f.drain();
        assert!(log.is_empty(), "{log:?}");
        assert!(f.prompts().is_empty(), "{:?}", f.prompts());
        assert_eq!(f.items(), before);
        let board = f.store().load().unwrap();
        assert_eq!(board.milestones[0].tasks[0].details.assignee, None);
    }

    #[test]
    fn a_leads_post_on_a_done_tasks_thread_goes_only_to_whom_it_mentions() {
        let f = Fixture::new();
        // #1 is Done, dev-1 still its assignee and still running: the lead's sign-off in its
        // thread goes to no one, so closing dev-1 right after cannot make it fail.
        f.store().set_task_stage(TaskId(1), Stage::Done).unwrap();
        f.post("cat-lead", "merged, thanks; closing you");
        assert!(f.drain().is_empty());
        assert!(f.prompts().is_empty(), "{:?}", f.prompts());
        // The same once it is closed: nothing sent, nothing fails, no item.
        std::fs::write(f.dir.path().join("agents"), "dev-2 qa-1").unwrap();
        f.post("cat-lead", "recorded on the issue");
        assert!(f.drain().is_empty());
        assert!(f.items().is_empty(), "{:?}", f.items());
        // A post that names it still goes to it, and failing to reach it stays loud.
        f.post("cat-lead", "@dev-1 one more thing");
        let log = f.drain();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].starts_with("could not deliver 1 post(s) to dev-1: herdr has no agent"),
            "{log:?}"
        );
        assert_eq!(f.items().len(), 1, "{:?}", f.items());
        // Back In progress, the lead's posts there reach its assignee again (and a closed
        // assignee of a task not done still fails loudly, as the test above shows).
        f.store()
            .set_task_stage(TaskId(1), Stage::InProgress)
            .unwrap();
        std::fs::write(f.dir.path().join("agents"), "dev-1 dev-2 qa-1").unwrap();
        f.post("cat-lead", "reopened: the fix broke CI");
        let log = f.drain();
        assert_eq!(log, ["delivered 1 post(s) to dev-1, reachable again"]);
    }

    #[test]
    fn failed_workers_are_one_item_per_drain_naming_each_and_a_stall_is_logged_only() {
        let f = Fixture::new();
        f.fail(
            "qa-1",
            r#"{"error":{"code":"agent_blocked","message":"blocked"}}"#,
        );
        // ghost-2 and ghost-3 worked on this board (they posted) and are gone from herdr's
        // agent list: never prompted, and the owner is told.
        f.post("ghost-2", "status: done here");
        f.post("ghost-3", "status: done here");
        f.drain();
        f.fail(
            "qa-3",
            r#"{"error":{"code":"agent_prompt_stalled","message":"stalled"}}"#,
        );
        f.post("dev-1", "@qa-1 @ghost-2 @ghost-3 @qa-3 please look");
        let log = f.drain();
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(
            log[0].starts_with("sent 1 post(s) to qa-3, not confirmed"),
            "{log:?}"
        );
        assert!(log[0].ends_with("; not resent"), "{log:?}");
        let whys = [
            ("qa-1", "herdr agent prompt exited 1"),
            ("ghost-2", "herdr has no agent named ghost-2"),
            ("ghost-3", "herdr has no agent named ghost-3"),
        ];
        for (line, (who, why)) in log[1..].iter().zip(whys) {
            let want = format!("could not deliver 1 post(s) to {who}: {why}");
            assert!(line.starts_with(&want), "{log:?}");
            assert!(line.ends_with("; told the owner in T1"), "{log:?}");
        }
        let items = f.items();
        assert_eq!(items.len(), 1, "{items:?}");
        let lines: Vec<&str> = items[0].lines().collect();
        assert_eq!(
            lines[0],
            "@manager could not deliver to 3 recipients; their posts stay on the board:"
        );
        assert!(
            lines[1].starts_with(
                "- qa-1: herdr agent prompt exited 1: {\"error\":{\"code\":\"agent_blocked\""
            ),
            "{lines:?}"
        );
        assert!(
            lines[2] == "- ghost-2: herdr has no agent named ghost-2",
            "{lines:?}"
        );
        assert!(lines[3].starts_with("- ghost-3: "), "{lines:?}");
        assert_eq!(lines.len(), 4, "{lines:?}");
        // Again: logged, no second item; the item went to no worker.
        f.post("dev-1", "@qa-1 @ghost-2 @ghost-3 again");
        let log = f.drain();
        assert_eq!(log.len(), 3, "{log:?}");
        assert!(
            log.iter()
                .all(|l| l.ends_with("(the owner was told already)")),
            "{log:?}"
        );
        assert_eq!(f.items().len(), 1);
        assert!(f.prompts().is_empty(), "{:?}", f.prompts());
    }

    #[test]
    fn a_failure_says_how_to_clear_a_gone_worker_only_for_its_tasks_not_done_on_that_board() {
        let f = Fixture::new();
        let mut store = f.store();
        let m = store.load().unwrap().milestones[0].id;
        let other = store.add_milestone("Other", "goal", None).unwrap();
        store
            .lead_milestone(other, "Other", "other-lead", None)
            .unwrap();
        // On this board: ghost-two on #2 (in progress) and, in another case, #3 (waiting);
        // ghost-3 on #4 (done) and #5 (in progress). On the other board: ghost-two on #6. qa-1,
        // live in herdr, on #7, refuses the prompt. ghost-9 is nobody's assignee.
        for (on, name, stage) in [
            (m, "ghost-two", Stage::InProgress),
            (m, "Ghost-Two", Stage::Waiting),
            (m, "ghost-3", Stage::Done),
            (m, "ghost-3", Stage::InProgress),
            (other, "ghost-two", Stage::Waiting),
            (m, "qa-1", Stage::InProgress),
        ] {
            let t = store.add_task(on, "more").unwrap();
            let fields = TaskFields {
                assignee: Some(name),
                ..Default::default()
            };
            store.set_task(t, fields).unwrap();
            if stage != Stage::Waiting {
                store.set_task_stage(t, stage).unwrap();
            }
        }
        f.fail("qa-1", "blocked");
        f.post("dev-1", "@qa-1 @ghost-two @ghost-3 @ghost-9 look");
        f.drain();
        let items = f.items();
        assert_eq!(items.len(), 1, "{items:?}");
        let lines: Vec<&str> = items[0].lines().collect();
        assert_eq!(
            lines[1..],
            [
                "- qa-1: herdr agent prompt exited 1: blocked",
                "- ghost-two: herdr has no agent named ghost-two; if it was closed: tower task \
                 unassign 2 and tower task unassign 3, or set the next worker",
                "- ghost-3: herdr has no agent named ghost-3; if it was closed: tower task \
                 unassign 5, or set the next worker",
            ]
        );
        // ghost-9 never worked on the board: dev-1 is told, in the post's thread.
        let board = f.store().load().unwrap();
        let told = (board.milestones[0].threads[0].messages.iter())
            .find(|m| m.author == "system" && m.text.starts_with("@dev-1 "))
            .map(|m| m.text.clone());
        assert!(
            told.as_deref()
                .is_some_and(|t| t.starts_with("@dev-1 no worker is named `@ghost-9`, so")),
            "{told:?}"
        );
    }

    /// Names no worker on the board has had (an example, a misspelt
    /// worker) are no item for the owner; the post's author is told on its thread, once, and
    /// that reply reaches it like any post.
    #[test]
    fn a_name_no_worker_has_had_tells_the_author_not_the_owner() {
        let f = Fixture::new();
        f.post(
            "dev-2",
            "write @name for a worker, as in @media; @qa-1 please check",
        );
        let log = f.drain();
        assert_eq!(
            log,
            [
                "could not deliver 1 post(s) to name: herdr has no agent named name, a name no \
                 worker on the board has had; no item for the owner",
                "could not deliver 1 post(s) to media: herdr has no agent named media, a \
                 name no worker on the board has had; no item for the owner",
                "delivered 1 post(s) to qa-1",
                "told dev-2 in T1 of the name(s) no worker has had",
            ]
        );
        assert!(f.items().is_empty(), "{:?}", f.items());
        let reply = "@dev-2 no worker is named `@name` or `@media`, so your post reached no \
                     one by that name: herdr has no such agent and no worker on this board has \
                     had the name. If you meant a worker, check the name; if you were quoting, \
                     put it in backticks.";
        let board = f.store().load().unwrap();
        let last = board.milestones[0].threads[0]
            .messages
            .last()
            .unwrap()
            .clone();
        assert_eq!(
            (last.author.as_str(), last.text.as_str()),
            ("system", reply)
        );
        // The reply reaches dev-2 next round, once, and starts nothing more.
        assert_eq!(f.drain(), ["delivered 1 post(s) to dev-2"]);
        let prompts = f.prompts();
        assert!(prompts.last().unwrap().starts_with("dev-2|"), "{prompts:?}");
        assert!(prompts.last().unwrap().contains(reply), "{prompts:?}");
        assert!(f.drain().is_empty());
        assert!(f.items().is_empty(), "{:?}", f.items());
    }

    /// A lead misspelling a worker is told on the board, where its stream reads it;
    /// the owner writing a name no worker has had is told in the inbox, for awareness.
    #[test]
    fn a_lead_or_the_owner_naming_no_worker_is_told_on_the_thread() {
        let f = Fixture::new();
        f.post("cat-lead", "@qa-11 please test");
        let log = f.drain();
        assert!(
            log.contains(&"delivered 1 post(s) to dev-1".to_string()),
            "{log:?}"
        );
        assert!(
            log.contains(&"told cat-lead in T1 of the name(s) no worker has had".to_string()),
            "{log:?}"
        );
        assert!(f.items().is_empty(), "{:?}", f.items());
        // The lead is sent nothing: its board waiter brings it the reply.
        assert!(f.drain().is_empty());
        assert!(
            !f.prompts().iter().any(|p| p.starts_with("cat-lead|")),
            "{:?}",
            f.prompts()
        );
        f.post("manager", "@nobdy is this right?");
        f.drain();
        let items = f.items();
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(
            items[0].starts_with("@manager no worker is named `@nobdy`, so"),
            "{items:?}"
        );
    }

    /// The reply quotes one name, two with "or", and at most `UNKNOWN_NAMES_MAX`,
    /// then how many more, so it always fits a board message.
    #[test]
    fn a_reply_quotes_its_names_and_caps_the_list() {
        let one = unknown_text("dev-2", &["name".to_string()], "herdr");
        assert!(
            one.starts_with("@dev-2 no worker is named `@name`, so"),
            "{one}"
        );
        let names: Vec<String> = (1..=12).map(|i| format!("n{i}")).collect();
        let many = unknown_text("dev-2", &names, "herdr");
        let quoted = many.matches("`@n").count();
        assert_eq!(quoted, UNKNOWN_NAMES_MAX, "{many}");
        assert!(many.contains("`@n9`, `@n10` or 2 more, so"), "{many}");
        assert!(!many.contains("`@n11`"), "{many}");
        let spaced = unknown_text("Catalogue Lead", &["x".to_string()], "herdr");
        assert!(
            spaced.starts_with("Catalogue Lead: no worker is named"),
            "{spaced}"
        );
    }

    /// The reply is advice to its author; when the author is closed before
    /// it goes, the failure is logged and raises no item for the owner.
    #[test]
    fn a_reply_to_an_author_gone_meanwhile_is_logged_not_an_item() {
        let f = Fixture::new();
        f.post("dev-2", "write @name for a worker");
        f.drain();
        assert!(f.items().is_empty(), "{:?}", f.items());
        // dev-2 is closed before the next round carries the reply to it.
        std::fs::write(f.dir.path().join("agents"), "dev-1 qa-1").unwrap();
        let log = f.drain();
        assert_eq!(
            log,
            [
                "could not deliver tower's reply to dev-2: herdr has no agent named dev-2; \
                 logged only"
            ]
        );
        assert!(f.items().is_empty(), "{:?}", f.items());
        // dev-2's own posts still raise the owner's item when it is gone: it was a worker.
        f.post("cat-lead", "@dev-2 one more thing");
        let log = f.drain();
        assert!(
            log.iter()
                .any(|l| l.contains("to dev-2") && l.ends_with("told the owner in T1")),
            "{log:?}"
        );
        assert_eq!(f.items().len(), 1);
    }

    /// A lead taken off the board (`tower lead remove`) is no worker the board knew,
    /// though it posted there: a mention of it is its author's to fix, not the owner's.
    #[test]
    fn a_mention_of_a_removed_lead_tells_the_author_not_the_owner() {
        let f = Fixture::new();
        // The lead posted, so its session is a post's author on the board.
        f.post("cat-lead", "status: handing over");
        f.drain();
        let repo = f.store().load().unwrap().repos[0].id;
        let mut store = f.store();
        store.remove_lead(repo, "done: retired").unwrap();
        // The repo's next lead: posts on its board are delivered again.
        (store.add_lead(crate::board::RepoOf::Repo(repo), "Next", "next-lead", None)).unwrap();
        f.post("dev-2", "@cat-lead are you still there?");
        let log = f.drain();
        assert!(f.items().is_empty(), "{:?}", f.items());
        assert!(
            log.iter()
                .any(|l| l == "told dev-2 in T1 of the name(s) no worker has had"),
            "{log:?}"
        );
        // A worker the board knew that is gone still raises the owner's item.
        std::fs::write(f.dir.path().join("agents"), "dev-2 qa-1").unwrap();
        f.post("dev-2", "@dev-1 one more thing");
        f.drain();
        assert_eq!(f.items().len(), 1, "{:?}", f.items());
    }

    #[test]
    fn tower_replies_only_on_an_open_thread() {
        let f = Fixture::new();
        let mut store = f.store();
        assert!(store.tower_reply(ThreadId(1), "@dev-1 x", 5).unwrap());
        store
            .resolve_thread(ThreadId(1), "cat-lead", "done: shipped")
            .unwrap();
        assert!(!store.tower_reply(ThreadId(1), "@dev-1 y", 6).unwrap());
        assert!(!store.tower_reply(ThreadId(99), "@dev-1 z", 7).unwrap());
    }

    #[test]
    fn a_hung_herdr_costs_one_budget_and_the_rest_goes_next_round_once() {
        let mut f = Fixture::new();
        // herdr hangs on the first prompt until released, which never comes in this round:
        // the round stops it when its own 1 s budget ends, long before the hold (TEST_WAIT)
        // or the herdr timeout (TEST_WAIT too) would. The fixture's first drain ran the fake
        // already, so no first-run stall is in this round: half of TEST_WAIT is room for
        // any load, and a send bounded by the herdr timeout instead of the budget fails it.
        f.deliver.budget = Duration::from_secs(1);
        std::fs::write(f.dir.path().join("hold"), "").unwrap();
        f.post("dev-1", "@qa-1 @qa-2 look");
        let began = Instant::now();
        let log = f.drain();
        assert!(
            began.elapsed() < TEST_WAIT / 2,
            "the round outlived its budget: {:?}",
            began.elapsed()
        );
        assert!(
            !f.dir.path().join("ended").exists(),
            "the round waited for the hung herdr"
        );
        assert!(
            log[0].starts_with("sent 1 post(s) to qa-1, not confirmed"),
            "{log:?}"
        );
        assert_eq!(log[1], "1 post(s) wait for the next round");
        assert_eq!(log.len(), 2, "{log:?}");
        // herdr answers again: qa-2 gets it, and qa-1 not a second time, even with a new
        // post in the same round.
        std::fs::remove_file(f.dir.path().join("hold")).unwrap();
        f.post("dev-1", "status: new");
        let log = f.drain();
        assert_eq!(log, ["delivered 1 post(s) to qa-2"]);
        assert!(
            f.prompts().iter().all(|p| p.starts_with("qa-2|")),
            "{:?}",
            f.prompts()
        );
        assert!(f.drain().is_empty());
    }

    #[test]
    fn a_stop_sends_nothing_more_and_loses_nothing() {
        let f = Fixture::new();
        f.post("dev-1", "@qa-1 look");
        f.deliver.stop.store(true, Ordering::Relaxed);
        assert_eq!(f.drain(), ["1 post(s) wait for the next round"]);
        assert!(f.prompts().is_empty());
        // The restarted watch delivers it.
        f.deliver.stop.store(false, Ordering::Relaxed);
        assert_eq!(f.drain(), ["delivered 1 post(s) to qa-1"]);
    }

    #[test]
    fn workers_wait_while_herdr_cannot_list_its_agents_said_once() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("list-fails"), "").unwrap();
        f.post("dev-1", "@qa-1 look");
        let log = f.drain();
        assert!(
            log[0].starts_with("herdr agent list failed: down;"),
            "{log:?}"
        );
        assert_eq!(log[1], "1 post(s) wait for the next round");
        assert_eq!(
            f.drain(),
            ["1 post(s) wait for the next round"],
            "said twice"
        );
        assert!(f.items().is_empty(), "herdr down is no failed send");
        std::fs::remove_file(f.dir.path().join("list-fails")).unwrap();
        assert_eq!(f.drain(), ["delivered 1 post(s) to qa-1"]);
    }

    #[test]
    fn an_episode_older_than_an_hour_is_told_again() {
        let f = Fixture::new();
        // ghost-1 worked on this board and is gone from herdr (the owner is told).
        f.post("ghost-1", "status: done here");
        f.drain_at(500);
        f.post("dev-1", "@ghost-1 one");
        assert!(f.drain_at(1_000)[0].ends_with("told the owner in T1"));
        f.post("dev-1", "@ghost-1 two");
        let log = f.drain_at(1_000 + EPISODE_EXPIRES - 1);
        assert!(log[0].ends_with("(the owner was told already)"), "{log:?}");
        f.post("dev-1", "@ghost-1 three");
        assert!(f.drain_at(1_000 + EPISODE_EXPIRES)[0].ends_with("told the owner in T1"));
        assert_eq!(f.items().len(), 2);
    }

    #[test]
    fn a_round_failing_on_two_boards_posts_each_item_on_its_own_board() {
        let f = Fixture::new();
        // Both worked on the board and are gone from herdr (the owner is told).
        f.post("ghost-1", "status: done here");
        f.post("ghost-2", "status: done here");
        f.drain();
        let mut store = f.store();
        let m2 = store.add_milestone("Other", "goal", None).unwrap();
        store
            .lead_milestone(m2, "Other", "other-lead", None)
            .unwrap();
        let (t2, _) =
            (store.open_thread(&BoardOf::Milestone(m2), "dev-2", None, "q", "@ghost-2 hi"))
                .unwrap();
        f.post("dev-1", "@ghost-1 look");
        let log = f.drain();
        assert!(
            log.iter()
                .any(|l| l.contains("to ghost-1") && l.ends_with("told the owner in T1")),
            "{log:?}"
        );
        assert!(
            log.iter().any(
                |l| l.contains("to ghost-2") && l.ends_with(&format!("told the owner in {t2}"))
            ),
            "{log:?}"
        );
        let board = f.store().load().unwrap();
        let items: Vec<(ThreadId, &str)> = (board.inbox.awareness.iter())
            .map(|a| (a.thread, a.message.text.as_str()))
            .collect();
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(
            items
                .iter()
                .any(|(t, i)| *t == ThreadId(1) && i.contains("ghost-1") && !i.contains("ghost-2")),
            "{items:?}"
        );
        assert!(
            items
                .iter()
                .any(|(t, i)| *t == t2 && i.contains("ghost-2") && !i.contains("ghost-1")),
            "{items:?}"
        );
    }

    #[test]
    fn one_item_names_only_the_recipients_newly_failing_and_fits_a_message() {
        let lines: Vec<String> = (0..40)
            .map(|i| format!("ghost-{i}: {}", "w".repeat(200)))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let text = failure_text(&refs);
        assert!(text.chars().count() <= MAX_MESSAGE_CHARS, "{}", text.len());
        assert!(
            text.starts_with("@manager could not deliver to 40 recipients;"),
            "{text}"
        );
        assert!(text.ends_with(" more, in the watch's log"), "{text}");
        assert!(crate::board::check_message("message", &text).is_ok());
    }

    #[test]
    fn the_watchs_delivery_loop_holds_the_gate_through_each_round() {
        let f = Fixture::new();
        // The fake herdr marks that the send began, then holds it until released.
        std::fs::write(f.dir.path().join("hold"), "").unwrap();
        f.post("dev-1", "@qa-1 look");
        let gate = f.deliver.gate();
        let lines = Mutex::new(Vec::new());
        /// Stops the loop when dropped, so a failed assertion ends the test, not hangs it.
        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&f.deliver.stop);
            let running = s.spawn(|| f.deliver.run(|l| lines.lock().unwrap().push(l.to_string())));
            let wait = |what: &str, done: &dyn Fn() -> bool| {
                let until = std::time::Instant::now() + TEST_WAIT;
                while !done() {
                    assert!(std::time::Instant::now() < until, "{what} never happened");
                    std::thread::sleep(Duration::from_millis(20));
                }
            };
            wait("the send", &|| f.dir.path().join("started").exists());
            assert!(gate.try_lock().is_err(), "a restart could exec mid-round");
            std::fs::write(f.dir.path().join("release"), "").unwrap();
            wait("the round's lines", &|| lines.lock().unwrap().len() == 1);
            drop(_stop);
            running.join().unwrap();
        });
        assert_eq!(lines.into_inner().unwrap(), ["delivered 1 post(s) to qa-1"]);
        assert!(gate.try_lock().is_ok());
    }

    #[test]
    fn a_drain_holds_the_gate_from_taking_posts_to_its_last_send() {
        let f = Fixture::new();
        // The fake herdr marks that the send began, then holds it until released.
        std::fs::write(f.dir.path().join("hold"), "").unwrap();
        f.post("dev-1", "@qa-1 look");
        let gate = f.deliver.gate();
        std::thread::scope(|s| {
            let drain = s.spawn(|| f.drain());
            let started = f.dir.path().join("started");
            let until = std::time::Instant::now() + TEST_WAIT;
            while !started.exists() {
                assert!(std::time::Instant::now() < until, "the send never started");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(gate.try_lock().is_err(), "a restart could exec mid-drain");
            std::fs::write(f.dir.path().join("release"), "").unwrap();
            let log = drain.join().unwrap();
            assert_eq!(log, ["delivered 1 post(s) to qa-1"]);
        });
        assert!(gate.try_lock().is_ok());
    }

    #[test]
    fn a_failure_on_a_resolved_thread_is_posted_in_a_new_thread_on_the_same_board() {
        let f = Fixture::new();
        // ghost-1 worked on the board and is gone from herdr (the owner is told).
        f.post("ghost-1", "status: done here");
        f.drain();
        f.store()
            .resolve_thread(ThreadId(1), "dev-1", "done: merged, @ghost-1 FYI")
            .unwrap();
        assert!(f.drain()[0].contains("told the owner in T2"));
        let t = f.store().thread(ThreadId(2)).unwrap();
        assert_eq!(
            (t.title.as_str(), t.task),
            ("Delivery failed", Some(TaskId(1)))
        );
        assert_eq!(f.items().len(), 1);
    }
}
