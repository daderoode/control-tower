//! How posts reach a lead (spec section 4, "How posts reach a lead"): not as turns in
//! its pane for every post but through a board waiter the lead runs as a background
//! Bash command, which ends when a post is unread and so wakes the lead only then, with its
//! Stop hook as the backstop and the watch saying when it is not listening.
//!
//! One cursor, one source of truth: `lead.told_upto` on the board, the last message the lead
//! has been told of. A post is unread while it is on the lead's repo board, after that
//! cursor and not the lead's own (`Store::tell_lead`). `unread` and the Stop hook tell
//! and move it, after the telling and only from where they read it, so a telling
//! that fails moves nothing and a post is never skipped. The waiter never moves it: telling
//! happens where the lead sees the text, and Claude Code's notice of a background command's
//! end does not carry its output.
//!
//! - `wait`: `tower lead wait R<n>` holds the board's listener lock (below) until a post is
//!   unread, then prints one line saying how many and ends. With posts already unread it
//!   ends at once, so a post landing between one waiter's end and the next is never
//!   skipped. Claude Code puts no time limit on a background command in a terminal session,
//!   so an idle lead is never woken for nothing. Once it holds the lock it takes the lead's
//!   count of turns as the watch's baseline.
//! - `unread`: `tower lead unread R<n>` prints one line per unread post (`line`) and tells
//!   them; then, if a listener for the repo already runs, one line saying so, so a lead
//!   whose waiter was stopped by a newer one does not arm a third.
//! - The listener lock: a waiter holds an exclusive lock on a file beside the
//!   board (`lock_path`) while it runs, which the kernel drops when it ends however it
//!   ends, with its process id in the file. The newest wins: one starting while another
//!   holds the lock stops that one first, so the one the lead armed last always has it, and
//!   one orphaned by a Claude Code that went away never blocks a re-arm or reads as
//!   listening for long. A held lock is what "listening" means.
//! - `hook`: `tower lead hook`, the plugin's Stop hook. For the Claude session a lead
//!   attached (`tower lead attach`), it blocks the end of the turn while posts are unread,
//!   naming them and so telling them, and counts a turn it lets end with no listener running
//!   (`lead.turns`); any other session passes. For a turn that ends on an API error, Claude
//!   Code runs StopFailure instead, which only counts (`turn_failed`).
//! - `check_leads`: the watch, each look, for every registered lead: no listener
//!   running while it has no attached Claude session for `NO_SESSION_GRACE`, or its Claude
//!   session gone from `claude agents --json`, or idle having finished a turn with no
//!   listener since the watch last saw one running or first saw it with none (the Stop
//!   hook's count of such turns moved), opens
//!   one "lead not listening" episode (one awareness item for the owner); a listener running
//!   again resolves it. A lead woken by its waiter's end re-arms in that turn, so the gap
//!   between the end and the re-arm never raises one. No terminal is asked.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use crate::board::{RepoId, RepoOf};
use crate::deliver::{Post, sanitise};
use crate::store::Store;

/// How often a stream looks at the board for new posts.
pub const LOOK_EVERY: Duration = Duration::from_secs(1);
/// The most characters of a post's text a line carries: its first words, never its body.
pub const FIRST_WORDS: usize = 80;
/// How long a starting stream waits for its lock before taking it as a stream's, so a
/// watch probing it at that moment does not count as one.
const PROBE_WAIT: Duration = Duration::from_millis(500);
/// How long a starting stream waits for the stream it stopped to let go of the lock.
const STOP_WAIT: Duration = Duration::from_secs(3);
/// The most posts a Stop hook names; the rest are counted.
const HOOK_NAMES: usize = 50;

/// How long the watch sees a registered lead with no attached Claude session and no
/// listener before it says the lead is not listening: long enough for a lead the
/// dashboard started to boot Claude and arm its waiter.
pub const NO_SESSION_GRACE: i64 = 5 * 60;

/// The watch's memory, for `check_leads`, of when it first saw each lead with no attached
/// Claude session and no listener, by its repo and registered session name (lower case),
/// so a new lead on the same repo is never taken for one it replaced.
pub type Unheard = HashMap<(RepoId, String), i64>;

/// One post as a line: "[T3 #12] dev-2: plan: read the bookmarks spec...", its text cut
/// to its first words, on one line, with nothing that could drive a terminal.
pub fn line(post: &Post) -> String {
    let about = post.task.map(|t| format!(" {t}")).unwrap_or_default();
    let author: String = sanitise(&post.author).split_whitespace().collect();
    format!(
        "[{}{about}] {author}: {}",
        post.thread,
        first_words(&post.text)
    )
}

/// `text` on one line, cut at a word to `FIRST_WORDS` characters with "..." when longer.
fn first_words(text: &str) -> String {
    let flat = sanitise(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= FIRST_WORDS {
        return flat;
    }
    let cut: String = flat.chars().take(FIRST_WORDS).collect();
    let cut = match cut.rfind(' ') {
        Some(at) if at > 0 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{cut}...")
}

/// The lock file a stream for `repo` holds, beside the board: "board.db.R1.stream".
pub fn lock_path(db: &Path, repo: RepoId) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{repo}.stream"));
    db.with_file_name(name)
}

fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open the stream lock {}", path.display()))
}

/// Whether a stream for `repo` runs: whether its lock is held (`look`). Never makes the lock
/// file: with none, no stream runs, and a look racing `tower repo remove` leaves nothing
/// behind for the removed repo.
pub fn listening(db: &Path, repo: RepoId) -> Result<bool> {
    let path = lock_path(db, repo);
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot open the stream lock {}", path.display()));
        }
    };
    look(&file, &path)
}

/// Whether the stream lock open as `file` (the one at `path`) is held. Takes the lock for no
/// longer than it takes to look, and lets go explicitly as `Held` does: a child forked
/// meanwhile shares the open file, and closing it would leave the lock held.
fn look(file: &File, path: &Path) -> Result<bool> {
    match file.try_lock() {
        Ok(()) => (file.unlock().map(|()| false))
            .with_context(|| format!("cannot let go of the stream lock {}", path.display())),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot look at the stream lock {}", path.display()))
        }
    }
}

/// Deletes the lock file at `path`, if there is one.
fn delete_lock(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("cannot delete the stream lock {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// For `repo`, which is not on the board or has no lead any more: deletes its stream lock
/// file unless a listener holds it, which deletes it itself on its next look. Run by `tower
/// repo remove` (also when it refuses a repo not on the board), by `tower lead remove`, and
/// by a `tower lead wait` refused one, so a lock left by a listener killed before that
/// look is cleared.
pub fn forget_stream(db: &Path, repo: RepoId) -> Result<()> {
    let path = lock_path(db, repo);
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot open the stream lock {}", path.display()));
        }
    };
    match file.try_lock() {
        Ok(()) => delete_lock(&path),
        Err(TryLockError::WouldBlock) => Ok(()),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot look at the stream lock {}", path.display()))
        }
    }
}

/// Waits up to `wait` for `lock`: whether it was taken.
fn take_within(lock: &File, path: &Path, wait: Duration) -> Result<bool> {
    let until = Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("cannot lock {}", path.display()));
            }
        }
    }
}

/// The repo argument of a listener's command line, from `ps`'s `comm` (argv0 in full) and
/// `args` (every argument after it, joined by spaces): `Some` only when argv0's file name is
/// `tower` and the arguments are exactly `lead wait <one word>`, or `lead stream <one word>`.
/// That command is gone, but a stream started by an older tower can outlive the
/// upgrade, holding the lock (one orphaned by a Claude Code that went away lingers until
/// its next post): a new waiter must still stop it, not refuse the lock and leave the lead
/// deaf.
fn listener_argument<'a>(comm: &str, args: &'a str) -> Option<&'a str> {
    let comm = comm.trim_end();
    if Path::new(comm).file_name()? != "tower" {
        return None;
    }
    match args
        .trim_end()
        .strip_prefix(comm)?
        .split_whitespace()
        .collect::<Vec<_>>()[..]
    {
        ["lead", "wait" | "stream", repo] => Some(repo),
        _ => None,
    }
}

/// One `ps -o <field>= -p <pid>` value, if the process exists.
fn ps(field: &str, pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether process `pid` is a listener (a `tower lead wait`, or a stream left by an older
/// tower) for `repo` on this board, by its command line (`listener_argument`), its repo
/// argument (`R<n>` or a milestone of it) read on the board, so a process id left in the
/// lock file by a listener long gone, and reused since, is never signalled.
fn is_listener(pid: i32, db: &Path, repo: RepoId) -> bool {
    let (Some(comm), Some(args)) = (ps("comm", pid), ps("args", pid)) else {
        return false;
    };
    let Some(of) = listener_argument(&comm, &args).and_then(|a| a.parse::<RepoOf>().ok()) else {
        return false;
    };
    Store::open_read_only(db)
        .and_then(|mut store| store.lead(of))
        .is_ok_and(|(_, r)| r == repo)
}

/// A stream's lock: the lock file held, its process id in it. Dropping it empties the file
/// and then lets go of the lock, so a stream that ends leaves no process id behind. It lets
/// go explicitly, not by closing the file: a child forked meanwhile by any thread shares
/// the open file, and so the lock, until it execs, and a close leaves the lock held until
/// that child's copy closes too.
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
        // Should it fail, closing the file still lets go, as it always has.
        let _ = self.0.unlock();
    }
}

/// Takes `repo`'s listener lock, newest wins: when another listener (a waiter, or a stream
/// left by an older tower) for `repo` holds it, stops that one (SIGTERM, then SIGKILL if it has not let go within
/// `STOP_WAIT`, by the process id in the lock file) and takes it. Writes this process's id
/// in the file. Refuses a lock held by anything that is not a listener for `repo` on this
/// board (`is_listener`).
fn take_lock(db: &Path, repo: RepoId) -> Result<Held> {
    let path = lock_path(db, repo);
    let mut lock = open_lock(&path)?;
    if !take_within(&lock, &path, PROBE_WAIT)? {
        let mut held = String::new();
        std::io::Read::read_to_string(&mut lock, &mut held)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let pid = (held.trim().parse::<i32>().ok()).filter(|&p| p > 0 && is_listener(p, db, repo));
        let Some(pid) = pid else {
            bail!(
                "{} is locked by something that is not a board listener for {repo} (it names \
                 process {:?})",
                path.display(),
                held.trim()
            );
        };
        use nix::sys::signal::{Signal, kill};
        let pid_of = nix::unistd::Pid::from_raw(pid);
        // Gone between the look and the signal is as good as stopped.
        let _ = kill(pid_of, Signal::SIGTERM);
        if !take_within(&lock, &path, STOP_WAIT)? {
            // An old stream stuck writing to a reader that stopped reading cannot see the
            // request.
            let _ = kill(pid_of, Signal::SIGKILL);
            if !take_within(&lock, &path, STOP_WAIT)? {
                bail!("the board listener for {repo} (process {pid}) did not stop when asked");
            }
        }
        eprintln!("stopped the earlier board listener for {repo} (process {pid})");
    }
    // Taken: from here every way out lets go through `Held`, emptied and then unlocked.
    let mut taken = Held(lock);
    (taken.0.set_len(0))
        .and_then(|()| std::io::Seek::rewind(&mut taken.0))
        .and_then(|()| writeln!(taken.0, "{}", std::process::id()))
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(taken)
}

/// How a board waiter ends (`wait`), each with the one line it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// Posts are unread: how many.
    Unread(usize),
    /// Its repo was removed from the board.
    Removed,
    /// Its repo's lead was taken off the board (`tower lead remove`).
    NoLead,
    /// It was asked to stop: a newer waiter or stream for the repo took the lock, or a
    /// signal.
    Stopped,
}

impl Waited {
    /// The line the waiter prints as it ends, for the lead to act on.
    pub fn line(&self, repo: RepoId) -> String {
        match self {
            Waited::Unread(n) => {
                let (s, them) = if *n == 1 { ("", "it") } else { ("s", "them") };
                format!(
                    "control-tower: {n} unread post{s} on the board of {repo}: read {them} with \
                     tower lead unread {repo}, then arm this waiter again."
                )
            }
            Waited::Removed => format!(
                "control-tower: {repo} was removed from the board (tower repo remove); this \
                 waiter ends, so do not arm it again."
            ),
            Waited::NoLead => format!(
                "control-tower: {repo} has no lead any more (tower lead remove); this waiter \
                 ends, so do not arm it again."
            ),
            Waited::Stopped => format!(
                "control-tower: this board waiter for {repo} was stopped (a newer one took \
                 over, or it was asked to stop); tower lead unread {repo} says whether one runs."
            ),
        }
    }
}

/// `tower lead wait`: takes `repo`'s listener lock (`take_lock`, stopping any earlier waiter
/// or stream) and holds it until a post on its board is unread (`Store::unread_count`),
/// looking every `LOOK_EVERY`, then lets go, prints one line (`Waited::line`) and ends. It
/// tells nothing and moves no cursor: the lead reads with `tower lead unread`. Posts already
/// unread end it at once, so a post that lands while no waiter runs is never skipped. Asked
/// to stop (SIGTERM, SIGINT, SIGHUP), it lets go and ends; its repo removed from the board,
/// or its lead taken off it, it deletes its lock file and ends. Every end is exit 0 with its line: Claude Code wakes
/// the lead on any end, and the lead's next step is the same, `tower lead unread`.
pub fn wait(db: &Path, repo: RepoId) -> Result<Waited> {
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [SIGTERM, SIGINT, SIGHUP] {
        signal_hook::flag::register(sig, Arc::clone(&stop))
            .context("cannot listen for a request to stop")?;
    }
    let held = take_lock(db, repo)?;
    let mut store = Store::open(db)?;
    // Armed: the turns its lead ended so far are the watch's baseline, so a turn the Stop
    // hook counted just before this waiter took its lock never raises.
    store.lead_waiter_armed(repo)?;
    let ended = loop {
        if !store.has_repo(repo)? {
            // Held, so no other listener for it runs: its lock file goes with it.
            delete_lock(&lock_path(db, repo))?;
            break Waited::Removed;
        }
        // Only a lead that is not there ends it so: a board it cannot read is an error,
        // and the waiter ends with it (a non-zero exit, whose output the lead reads).
        if !store.has_lead(repo)? {
            delete_lock(&lock_path(db, repo))?;
            break Waited::NoLead;
        }
        let n = store.unread_count(repo)?;
        if n > 0 {
            break Waited::Unread(n);
        }
        let until = Instant::now() + LOOK_EVERY;
        while !stop.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        if stop.load(Ordering::Relaxed) {
            break Waited::Stopped;
        }
    };
    // Let go before the line, so a lead that reads it and re-arms at once finds no lock held
    // by a waiter that is ending.
    drop(held);
    println!("{}", ended.line(repo));
    Ok(ended)
}

/// `tower lead unread`: prints one line per unread post on `repo`'s board (`line`) to
/// `out` and tells them (`Store::tell_lead`: the cursor moves only once they are written).
/// Then, when a listener for the repo already runs (`listening`), one line saying so, so a
/// lead woken by a waiter that a newer one stopped does not arm another. Prints nothing when
/// nothing is unread and no listener runs: the lead's next step is to arm its waiter.
pub fn unread(store: &mut Store, db: &Path, repo: RepoId, out: &mut impl Write) -> Result<()> {
    store.tell_lead(repo, |posts| {
        for post in posts {
            writeln!(out, "{}", line(post))?;
        }
        out.flush()?;
        Ok(())
    })?;
    if listening(db, repo)? {
        writeln!(
            out,
            "control-tower: a board waiter for {repo} is already running, so do not arm another."
        )?;
    }
    Ok(())
}

/// What Claude Code passes a Stop hook on stdin, as far as this reads it.
#[derive(Deserialize)]
struct HookInput {
    session_id: String,
}

/// `tower lead hook`: given the Stop hook's JSON `input`, the hook's reply to print, or
/// `None` to let the turn end. Only the Claude session a lead attached is held, and only
/// while posts on its board are unread: the reply blocks the stop, naming them, and they
/// count as told, so it blocks once per batch and never loops. A stop of that session that
/// ends its turn (nothing to hold it for) with no waiter running counts as a turn ended
/// without one (`Store::lead_turn_ended`), for the watch; a turn ended while the waiter
/// runs, and a stop held for posts, which goes on, are not counted. The session is looked up on
/// the board opened read only, so a stop never waits long on a writer and the board is
/// opened to write only for a lead. A board that does not exist yet holds no lead, so it
/// passes without making one.
pub fn hook(db: &Path, input: &str) -> Result<Option<String>> {
    let input: HookInput = serde_json::from_str(input)
        .context("the Stop hook's input is not what Claude Code sends")?;
    if !db.exists() {
        return Ok(None);
    }
    // Every session's every stop looks here: read only (never waiting long on a writer),
    // and the board opened to write only for a lead's session.
    let Some(repo) = Store::open_read_only(db)?.lead_of_session(&input.session_id)? else {
        return Ok(None);
    };
    let mut store = Store::open(db)?;
    let waiting = listening(db, repo)?;
    let mut reason = String::new();
    store.tell_lead(repo, |posts| {
        reason = hook_reason(repo, posts, waiting);
        Ok(())
    })?;
    if reason.is_empty() {
        // The turn ends, with nothing told that a failed count could lose.
        // Asked again inside the count's own write: a waiter whose lock lands meanwhile is
        // seen, or writes its baseline after the count (`Store::lead_turn_ended`).
        if !waiting {
            store.lead_turn_ended(repo, &input.session_id, || listening(db, repo))?;
        }
        return Ok(None);
    }
    Ok(Some(
        serde_json::json!({"decision": "block", "reason": reason}).to_string(),
    ))
}

/// `tower lead hook` for a StopFailure: given the hook's JSON `input`, a turn that ended
/// on an API error (a rate limit, an overload, the output limit), for which Claude Code runs
/// no Stop hook. For the Claude session a lead attached, with no waiter running, it counts as
/// a turn ended without one (`Store::lead_turn_ended`), as a Stop that lets the turn end
/// does. It never tells: Claude Code ignores this hook's output, so a telling would move the
/// cursor past posts nobody saw. Any other session, and a board that does not exist, passes.
pub fn turn_failed(db: &Path, input: &str) -> Result<()> {
    let input: HookInput = serde_json::from_str(input)
        .context("the StopFailure hook's input is not what Claude Code sends")?;
    if !db.exists() {
        return Ok(());
    }
    let Some(repo) = Store::open_read_only(db)?.lead_of_session(&input.session_id)? else {
        return Ok(());
    };
    if !listening(db, repo)? {
        Store::open(db)?.lead_turn_ended(repo, &input.session_id, || listening(db, repo))?;
    }
    Ok(())
}

/// What Claude Code passes a SessionStart hook on stdin, as far as this reads it: the
/// session's id, why it started (`startup`, `resume`, `clear`, `compact` or `fork`) and its
/// name (`session_title`, absent for a session with none).
#[derive(Deserialize)]
struct StartInput {
    session_id: String,
    source: Option<String>,
    session_title: Option<String>,
}

/// How many times a SessionStart asks `claude agents --json` for a session it does not
/// list yet, `LISTED_WAIT` apart: a starting session is usually, not always, listed by
/// the time its hook runs. Each ask is bounded by
/// `LISTED_ASK`, so every ask and wait together stay under the hook's 30 s (hooks.json).
const LISTED_TRIES: u32 = 4;
const LISTED_WAIT: Duration = Duration::from_millis(500);
pub const LISTED_ASK: Duration = Duration::from_secs(5);

/// `tower lead hook` for a SessionStart: given the hook's JSON `input`, the reply to
/// print (context for the session), or `None` to say nothing. A session named as a lead's
/// registered session (ignoring case) is attached, as `tower lead attach` would: `agents`
/// gives `claude agents --json`, asked again up to `LISTED_TRIES` times while it does not
/// list the session yet, and the session must be listed under that name. It is then told,
/// by `source`, how to resume leading. The lead skill cannot be invoked by the model
/// (`disable-model-invocation`: the owner starts it), so a session with no conversation is
/// told to arm its board waiter itself, so it is never deaf, and to ask the owner for the
/// skill:
/// - `startup`: to arm its board waiter, and ask the owner to run `/control-tower:lead <R>`
///   (which arms its forge waiter too);
/// - `clear` (no conversation, but background commands outlive it, as they do a
///   compaction): while its board waiter runs, that its waiters still run, not to arm them
///   again, and to ask the owner for the skill if it needs it again; else as `startup`;
/// - `compact`: nothing while its board waiter runs, else as `resume`;
/// - `resume`, `fork` and anything else: to read and arm its board waiter and its forge
///   waiter, which ended with the old session (its conversation still holds the skill).
///
/// A session with no name, or one no lead is registered under, passes before `claude` is
/// asked or the board opened to write, so every other session's start is cheap. So does a
/// board that does not exist. On a `startup`, `resume`, `fork` or another source, a lead's
/// attached session that is still running keeps its attachment: a second session under
/// the same name (a fork, or a second `claude -n`, a separate process `claude agents
/// --json` lists beside it) is told it is not the attached one, so two sessions never take
/// turns as the lead. A `/clear` and a compaction are the same process going on, so they
/// attach whatever the list says of the old id. Errors (a board
/// needing `tower upgrade`, `claude` failing) are the caller's to keep quiet: a hook can
/// never stop a session from starting.
pub fn session_start(
    db: &Path,
    input: &str,
    mut agents: impl FnMut() -> Result<String>,
) -> Result<Option<String>> {
    let input: StartInput = serde_json::from_str(input)
        .context("the SessionStart hook's input is not what Claude Code sends")?;
    let Some(title) = (input.session_title.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return Ok(None);
    };
    if !db.exists() {
        return Ok(None);
    }
    let Some((repo, lead, attached)) = Store::open_read_only(db)?.lead_named(title)? else {
        return Ok(None);
    };
    let mut json = agents()?;
    for _ in 1..LISTED_TRIES {
        if crate::deliver::listed(&json, &input.session_id)? {
            break;
        }
        std::thread::sleep(LISTED_WAIT);
        json = agents()?;
    }
    crate::deliver::check_asker(&json, &input.session_id, &lead, repo)?;
    let source = input.source.as_deref();
    let same_process = matches!(source, Some("clear" | "compact"));
    if !same_process
        && let Some(old) = attached.filter(|old| *old != input.session_id)
        && crate::deliver::listed(&json, &old)?
    {
        return Ok(Some(start_context(&format!(
            "control-tower: this session is named {}, the session of {}, the lead of {repo}, \
             but the lead's attached Claude session {old} is still running, so this one is \
             not attached: do not arm the lead's board waiter here.",
            lead.session, lead.name
        ))));
    }
    Store::open(db)?.attach_lead(repo, &input.session_id)?;
    let resume = format!(
        "control-tower: re-attached as {}, the lead of {repo}; your waiters ended with the \
         old session. Run tower lead unread {repo}, then tower lead wait {repo} with Bash \
         run_in_background, and arm your forge waiter (lead skill, New issues and idle).",
        lead.name
    );
    let fresh = format!(
        "control-tower: this session is {}, the lead of {repo}, and is attached. Arm your \
         board waiter now: run tower lead unread {repo}, then tower lead wait {repo} with Bash \
         run_in_background. The lead skill is started by the owner: ask them to run \
         /control-tower:lead {repo} to resume leading {repo} (it arms your forge waiter).",
        lead.name
    );
    // Background commands outlive a compaction and a /clear: its waiters may still run.
    let waiting = same_process && listening(db, repo)?;
    let text = match (source, waiting) {
        (Some("compact"), true) => return Ok(None),
        (Some("clear"), true) => format!(
            "control-tower: this session is {}, the lead of {repo}, and is attached again \
             after /clear. Your board waiter and forge waiter still run (background commands \
             outlive /clear), so do not arm them again. If you need the lead skill again, ask \
             the owner to run /control-tower:lead {repo}.",
            lead.name
        ),
        (Some("startup" | "clear"), _) => fresh,
        _ => resume,
    };
    Ok(Some(start_context(&text)))
}

/// A SessionStart hook's reply adding `text` to the session's context.
fn start_context(text: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}
    })
    .to_string()
}

/// Why a Stop hook holds the lead: its unread posts, one line each, up to `HOOK_NAMES`, and,
/// when no listener runs for it, how to arm its board waiter.
fn hook_reason(repo: RepoId, posts: &[Post], waiting: bool) -> String {
    let n = posts.len();
    let s = if n == 1 { "" } else { "s" };
    let mut text = format!(
        "control-tower: {n} unread post{s} on the board of {repo}. Read a thread with tower \
         thread show T<n>, answer on the board, then end the turn with one short line."
    );
    for post in posts.iter().take(HOOK_NAMES) {
        text.push('\n');
        text.push_str(&line(post));
    }
    if n > HOOK_NAMES {
        text.push_str(&format!("\nand {} more on the board.", n - HOOK_NAMES));
    }
    if !waiting {
        text.push_str(&format!(
            "\nYour board waiter is not running: arm it with tower lead wait {repo}, run with \
             Bash run_in_background."
        ));
    }
    text
}

/// A Claude session as `claude agents --json` lists it, as far as the lead check reads it.
#[derive(Deserialize)]
struct Listed {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    status: Option<String>,
}

/// What the lead check makes of a listed session's `status`.
enum Status {
    Working,
    Idle,
    Unknown,
}

impl Status {
    fn of(listed: &Listed) -> Status {
        match listed.status.as_deref() {
            Some("busy") => Status::Working,
            Some("idle") => Status::Idle,
            _ => Status::Unknown,
        }
    }
}

/// `claude agents --json` (`json`) as the lead check reads it.
fn read_listed(json: Result<String>) -> Result<Vec<Listed>> {
    serde_json::from_str(&json?).context("claude agents --json said something unexpected")
}

/// The watch's look at whether each registered lead is listening, at `now`: returns a line
/// per change and per failure. `agents` gives `claude agents --json`, asked at most once
/// and only when an attached lead has no waiter running, so a look where every lead
/// listens never runs claude. A list that cannot be read, or reads as nothing,
/// skips those leads for this look (one line for one that cannot be read).
/// `unheard` is the watch's own memory of when it first saw each lead with no attached
/// session and no listener (the grace before it says so, `NO_SESSION_GRACE`); a restart
/// of the watch starts the grace again, so it can only delay an item, never lose one.
pub fn check_leads(
    store: &mut Store,
    db: &Path,
    mut agents: impl FnMut() -> Result<String>,
    now: i64,
    unheard: &mut Unheard,
) -> Vec<String> {
    let mut listed: Option<Result<Vec<Listed>, String>> = None;
    let mut log = match check(store, db, &mut agents, &mut listed, now, unheard) {
        Ok(log) => log,
        Err(e) => vec![format!("could not check the leads are listening: {e:#}")],
    };
    if let Some(Err(e)) = listed {
        log.push(format!("could not check the leads are listening: {e}"));
    }
    log
}

fn check(
    store: &mut Store,
    db: &Path,
    agents: &mut impl FnMut() -> Result<String>,
    listed: &mut Option<Result<Vec<Listed>, String>>,
    now: i64,
    unheard: &mut Unheard,
) -> Result<Vec<String>> {
    let mut log = Vec::new();
    // The leads seen with no attached session and no listener in this look: every other
    // memory of one goes after it (a lead listening, attached, changed or taken off), so a
    // lead registered later starts its grace afresh.
    let mut seen: Vec<(RepoId, String)> = Vec::new();
    for lead in store.leads()? {
        let repo = lead.repo;
        let name = &lead.lead.name;
        // Opens the lead's "not listening" episode, saying `why`, unless one is open.
        let deaf = |store: &mut Store, why: &str| -> Result<Option<String>> {
            let text = format!(
                "@manager {name}, the lead of {repo}, is not listening: no board waiter runs \
                 and {why}, so posts on {repo} wait until it next looks. If a session leads \
                 {repo}, it arms one with tower lead wait {repo}, run with Bash \
                 run_in_background; if nobody leads {repo} any more, take the lead off with \
                 tower lead remove {repo}."
            );
            let opened = store.lead_deaf(repo, &text, now)?;
            Ok(opened.map(|t| {
                format!(
                    "{name}, the lead of {repo}, is not listening ({why}); told the owner in {t}"
                )
            }))
        };
        let step = (|| -> Result<Option<String>> {
            if listening(db, repo)? {
                // The count as the waiter runs is the baseline: a turn that ends after the
                // waiter ends, without re-arming, moves it, however soon after this look.
                if lead.deaf.is_none() && lead.quiet_turns == Some(lead.turns) {
                    return Ok(None);
                }
                let evidence =
                    format!("{name} is listening again: its board waiter for {repo} runs.");
                let ended = store.lead_listening(repo, &evidence, now)?;
                return Ok(ended
                    .map(|t| format!("{name}, the lead of {repo}, listens again; resolved {t}")));
            }
            // A lead with no attached Claude session has no session to look for: once the
            // watch has seen it with no listener for `NO_SESSION_GRACE`, it is not
            // listening. The grace covers a lead between being registered (by the lead skill
            // or a dashboard start) and arming its waiter.
            let Some(session) = lead.claude_session.as_deref() else {
                let key = (repo, lead.lead.session.to_ascii_lowercase());
                let first = *unheard.entry(key.clone()).or_insert(now);
                seen.push(key);
                if now - first < NO_SESSION_GRACE {
                    return Ok(None);
                }
                return deaf(
                    store,
                    "no Claude session is attached to it (tower lead attach)",
                );
            };
            let sessions =
                listed.get_or_insert_with(|| read_listed(agents()).map_err(|e| format!("{e:#}")));
            // Unreadable: logged once by `check_leads`. An empty list is likelier a glitch
            // than every Claude session on the machine gone, so it says nothing either.
            let Ok(sessions) = sessions else {
                return Ok(None);
            };
            if sessions.is_empty() {
                return Ok(None);
            }
            let found = (sessions.iter()).find(|s| s.session_id.as_deref() == Some(session));
            let why = match found.map(Status::of) {
                None => "its Claude session is gone",
                Some(Status::Idle) => {
                    // Idle with no listener is the gap between a waiter's end and the lead's
                    // re-arm until the lead has finished a turn without a waiter since the
                    // watch last saw the waiter running (or a waiter took its lock), or first
                    // saw it with none (idle or busy): a lead woken by its waiter's end
                    // re-arms in that turn, which the Stop hook then does not count. The hook
                    // counts every such turn, so one that starts and ends between two looks
                    // is counted too.
                    match lead.quiet_turns {
                        None => {
                            store.lead_quiet(repo, lead.turns)?;
                            return Ok(None);
                        }
                        Some(first) if first == lead.turns => return Ok(None),
                        Some(_) => "its Claude session finished a turn without one",
                    }
                }
                Some(Status::Working) => {
                    // Busy with no listener: recorded, so its next idle look, with the count
                    // moved, is a turn that finished without re-arming.
                    if lead.quiet_turns.is_none() {
                        store.lead_quiet(repo, lead.turns)?;
                    }
                    return Ok(None);
                }
                // Nothing to judge by.
                Some(Status::Unknown) => return Ok(None),
            };
            deaf(store, why)
        })();
        match step {
            Ok(Some(said)) => log.push(said),
            Ok(None) => {}
            Err(e) => log.push(format!(
                "could not check {name}, the lead of {repo}, is listening: {e:#}"
            )),
        }
    }
    unheard.retain(|key, _| seen.contains(key));
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOf, ThreadId};

    /// A temp board: M1 in R1 led by Catalogue (session cat-lead) with thread T1, M2 in R2 led
    /// by Other (session other-lead) with thread T2.
    fn board() -> (tempfile::TempDir, PathBuf, Store) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("board.db");
        let mut store = Store::open(&db).unwrap();
        for (title, name, session) in [
            ("MVP", "Catalogue", "cat-lead"),
            ("Two", "Other", "other-lead"),
        ] {
            let m = store.add_milestone(title, "g", None).unwrap();
            store.lead_milestone(m, name, session, None).unwrap();
            store
                .open_thread(&BoardOf::Milestone(m), session, None, title, "opened")
                .unwrap();
        }
        (dir, db, store)
    }

    fn told(store: &mut Store, repo: i64) -> Vec<String> {
        let mut lines = Vec::new();
        store
            .tell_lead(RepoId(repo), |posts| {
                lines = posts.iter().map(line).collect();
                Ok(())
            })
            .unwrap();
        lines
    }

    fn post(thread: i64, author: &str, text: &str) -> Post {
        Post {
            id: 1,
            thread: ThreadId(thread),
            repo: RepoId(1),
            task: None,
            author: author.into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_line_is_thread_task_author_and_first_words_on_one_line() {
        let mut p = post(3, "dev-2", "plan: short");
        p.task = Some(crate::board::TaskId(12));
        assert_eq!(line(&p), "[T3 #12] dev-2: plan: short");
        let long = "word ".repeat(40) + "\nbody that never shows";
        let got = line(&post(3, "dev-1", &long));
        assert_eq!(got, format!("[T3] dev-1: {}...", ["word"; 16].join(" ")));
        assert!(got.chars().count() <= FIRST_WORDS + 20, "{got}");
        // Nothing that drives a terminal, and no line break, from the text or the author.
        let evil = line(&post(1, "dev\n-1", "a\u{1b}[2Jb\r\nc\td"));
        assert_eq!(evil, "[T1] dev-1: a[2Jb c d");
        // One word longer than the cut is cut inside it.
        let one = line(&post(1, "x", &"y".repeat(200)));
        assert_eq!(one, format!("[T1] x: {}...", "y".repeat(FIRST_WORDS)));
    }

    #[test]
    fn a_lead_is_told_each_post_on_its_board_once_never_its_own_or_another_boards() {
        let (_dir, _db, mut store) = board();
        // The openers came before nothing: each lead's own, so nothing is unread.
        assert!(!store.has_unread(RepoId(1)).unwrap());
        assert!(told(&mut store, 1).is_empty());
        store.post(ThreadId(1), "dev-1", "status: one").unwrap();
        store.post(ThreadId(1), "Cat-Lead", "copy").unwrap();
        store.post(ThreadId(1), "catalogue", "noted").unwrap();
        store.post(ThreadId(2), "dev-2", "on R2").unwrap();
        store.post(ThreadId(1), "manager", "approved").unwrap();
        assert!(store.has_unread(RepoId(1)).unwrap());
        assert_eq!(
            told(&mut store, 1),
            ["[T1] dev-1: status: one", "[T1] manager: approved"]
        );
        assert!(!store.has_unread(RepoId(1)).unwrap());
        assert!(told(&mut store, 1).is_empty(), "told twice");
        // R2's lead has its own cursor.
        assert_eq!(told(&mut store, 2), ["[T2] dev-2: on R2"]);
        store.post(ThreadId(1), "dev-1", "status: two").unwrap();
        assert_eq!(told(&mut store, 1), ["[T1] dev-1: status: two"]);
    }

    #[test]
    fn a_telling_that_fails_moves_nothing() {
        let (_dir, _db, mut store) = board();
        store.post(ThreadId(1), "dev-1", "status: one").unwrap();
        let err = store
            .tell_lead(RepoId(1), |_| anyhow::bail!("stdout is gone"))
            .unwrap_err();
        assert_eq!(err.to_string(), "stdout is gone");
        assert_eq!(told(&mut store, 1), ["[T1] dev-1: status: one"]);
        let err = store.tell_lead(RepoId(99), |_| Ok(())).unwrap_err();
        assert!(err.to_string().contains("R99 has no lead"), "{err}");
    }

    #[test]
    fn a_teller_never_moves_the_cursor_back_over_another_tellers_post() {
        let (_dir, db, mut store) = board();
        store.post(ThreadId(1), "dev-1", "one").unwrap();
        let mut first = Vec::new();
        store
            .tell_lead(RepoId(1), |posts| {
                first = posts.iter().map(line).collect();
                // While this one writes, a post lands and the Stop hook tells both.
                let mut hook = Store::open(&db).unwrap();
                hook.post(ThreadId(1), "dev-1", "two").unwrap();
                assert_eq!(told(&mut hook, 1).len(), 2);
                Ok(())
            })
            .unwrap();
        assert_eq!(first, ["[T1] dev-1: one"]);
        assert!(told(&mut store, 1).is_empty(), "two was told again");
    }

    #[test]
    fn only_a_tower_lead_wait_or_an_old_stream_command_line_names_a_repo() {
        let tower = "/opt/homebrew/bin/tower";
        fn ok<'a>(comm: &str, args: &'a str) -> Option<&'a str> {
            listener_argument(comm, args)
        }
        // A stream left running by an older tower, though the command is gone: a
        // listener the newest waiter must stop, never a lock it refuses.
        assert_eq!(
            ok(tower, "/opt/homebrew/bin/tower lead stream R1\n"),
            Some("R1")
        );
        assert_eq!(ok("tower", "tower lead stream M3"), Some("M3"));
        // A waiter holds the listener lock.
        assert_eq!(
            ok(tower, "/opt/homebrew/bin/tower lead wait R4\n"),
            Some("R4")
        );
        assert_eq!(ok("tower", "tower lead unread R1"), None);
        // A path with spaces, as a checkout under "Application Support" would have.
        let spaced = "/Users/x/My Tools/tower";
        assert_eq!(ok(spaced, &format!("{spaced} lead stream R2")), Some("R2"));
        // argv0 whose file name is not tower, though the path holds "tower".
        assert_eq!(
            ok(
                "/w/control-tower/towerd",
                "/w/control-tower/towerd lead stream R1"
            ),
            None
        );
        assert_eq!(ok("/w/tower/perl", "/w/tower/perl lead stream R1"), None);
        // Anything but exactly `lead wait <repo>` or `lead stream <repo>`.
        for args in [
            "tower lead wait",
            "tower lead wait R1 R2",
            "tower lead stream",
            "tower lead stream R1 R2",
            "tower lead hook",
            "tower x lead stream R1",
            "tower watch",
        ] {
            assert_eq!(ok("tower", args), None, "{args}");
        }
        // args that do not start with argv0 (ps cut it short, say).
        assert_eq!(ok(tower, "tower lead stream R1"), None);
    }

    #[test]
    fn listening_is_whether_a_waiter_holds_the_lock() {
        let (_dir, db, _store) = board();
        assert_eq!(
            lock_path(&db, RepoId(1)),
            db.with_file_name("board.db.R1.stream")
        );
        assert!(!listening(&db, RepoId(1)).unwrap());
        // Looking makes no lock file, so a look racing tower repo remove leaves none.
        assert!(!lock_path(&db, RepoId(1)).exists());
        // Looking does not hold it.
        assert!(!listening(&db, RepoId(1)).unwrap());
        let held = waiter_runs(&db, 1);
        assert!(listening(&db, RepoId(1)).unwrap());
        assert!(!listening(&db, RepoId(2)).unwrap(), "one lock per repo");
        assert!(!lock_path(&db, RepoId(2)).exists());
        drop(held);
        assert!(!listening(&db, RepoId(1)).unwrap());
    }

    /// A child process, killed and waited for when dropped, so a failing test leaves none.
    struct Kill(std::process::Child);

    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_waiter_lets_go_of_its_lock_even_while_a_child_shares_it() {
        // A child forked while the lock is held shares the lock file's open file, and with it
        // the lock, until it execs. This one shares it for its whole life, as its
        // stdin: letting go frees the lock at once all the same.
        let (_dir, db, _store) = board();
        let held = waiter_runs(&db, 1);
        let child = Kill(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(held.0.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        assert!(listening(&db, RepoId(1)).unwrap());
        drop(held);
        assert!(!listening(&db, RepoId(1)).unwrap());
        drop(child);
    }

    #[test]
    fn a_look_lets_go_of_the_lock_it_took_even_while_a_child_shares_it() {
        // The same for a look, which takes the lock to see it free: a child sharing the
        // look's open file (as its stdin) does not keep the lock once the look is done.
        let (_dir, db, _store) = board();
        let path = lock_path(&db, RepoId(1));
        drop(open_lock(&path).unwrap());
        let file = File::open(&path).unwrap();
        let child = Kill(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(file.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        assert!(!look(&file, &path).unwrap());
        drop(file);
        assert!(!listening(&db, RepoId(1)).unwrap());
        drop(child);
    }

    fn input(session: &str) -> String {
        serde_json::json!({
            "session_id": session,
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
    }

    #[test]
    fn the_hook_holds_only_an_attached_lead_and_once_per_batch() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        store.post(ThreadId(1), "dev-1", "status: one").unwrap();
        store
            .post(ThreadId(1), "dev-1", "@manager status: two")
            .unwrap();
        // Any other session passes: a worker, a lead that never attached, a stranger.
        assert_eq!(hook(&db, &input("sess-9")).unwrap(), None);
        let reply = hook(&db, &input("sess-1")).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["decision"], "block");
        assert_eq!(
            v["reason"],
            "control-tower: 2 unread posts on the board of R1. Read a thread with tower thread \
             show T<n>, answer on the board, then end the turn with one short line.\n\
             [T1] dev-1: status: one\n\
             [T1] dev-1: @manager status: two\n\
             Your board waiter is not running: arm it with tower lead wait R1, run with Bash \
             run_in_background."
        );
        // Told: the next stop passes, and a stream would not repeat them.
        assert_eq!(hook(&db, &input("sess-1")).unwrap(), None);
        assert!(!store.has_unread(RepoId(1)).unwrap());
        // With its stream running, the hook does not ask for one.
        let _held = waiter_runs(&db, 1);
        store.post(ThreadId(1), "dev-1", "status: three").unwrap();
        let reply = hook(&db, &input("sess-1")).unwrap().unwrap();
        assert!(!reply.contains("not running"), "{reply}");
        assert!(
            reply.contains("1 unread post on the board of R1"),
            "{reply}"
        );
    }

    #[test]
    fn the_hook_refuses_input_it_cannot_read_and_makes_no_board() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("none.db");
        assert_eq!(hook(&db, &input("sess-1")).unwrap(), None);
        assert!(!db.exists(), "the hook made a board");
        let err = hook(&db, "not json").unwrap_err();
        assert!(
            format!("{err:#}").contains("not what Claude Code sends"),
            "{err:#}"
        );
    }

    #[test]
    fn a_long_backlog_is_named_up_to_a_cap_and_counted() {
        let posts: Vec<Post> = (0..HOOK_NAMES + 3).map(|_| post(1, "d", "x")).collect();
        let text = hook_reason(RepoId(1), &posts, true);
        assert_eq!(text.lines().count(), 1 + HOOK_NAMES + 1, "{text}");
        assert!(text.ends_with("\nand 3 more on the board."), "{text}");
    }

    /// `claude agents --json` listing `sessions`, each (session id, status), shaped as
    /// Claude Code prints an interactive session. An invented example entry in that shape:
    ///
    /// {"id":"<id>","cwd":"/Users/me/src/<repo>","kind":"interactive","startedAt":1,
    /// "sessionId":"00000000-xxxx-xxxx-xxxx-xxxxxxxxxxxx","name":"<name>","pid":4242,
    /// "status":"idle"}
    fn agents(sessions: &[(&str, &str)]) -> String {
        let list: Vec<serde_json::Value> = (sessions.iter())
            .map(|(session, status)| {
                serde_json::json!({
                    "id": "a1", "cwd": "/somewhere", "kind": "interactive", "startedAt": 1,
                    "sessionId": session, "name": "cat-lead", "pid": 4242, "status": status,
                })
            })
            .collect();
        serde_json::Value::Array(list).to_string()
    }

    /// A look with `list` as `claude agents --json`, at `now`, with a fresh grace memory.
    fn look_with(store: &mut Store, db: &Path, list: &str, now: i64) -> Vec<String> {
        check_leads(store, db, || Ok(list.to_string()), now, &mut HashMap::new())
    }

    /// `session` ends a turn: its Stop hook runs, with nothing unread to hold it for.
    fn ends_turn(db: &Path, session: &str) {
        assert_eq!(hook(db, &input(session)).unwrap(), None);
    }

    /// How many turns the lead of `repo` has ended, by its Stop hook.
    fn turns(store: &mut Store, repo: i64) -> i64 {
        let leads = store.leads().unwrap();
        leads.iter().find(|l| l.repo == RepoId(repo)).unwrap().turns
    }

    /// The owner's awareness items still on the alert line, oldest first: "not listening"
    /// is never a question, so never need-you.
    fn items(store: &mut Store) -> Vec<String> {
        let board = store.load().unwrap();
        assert!(
            board.inbox.need_you.is_empty(),
            "{:?}",
            board.inbox.need_you
        );
        let mut texts: Vec<String> = (board.inbox.awareness.iter())
            .map(|a| a.message.text.clone())
            .collect();
        texts.reverse();
        texts
    }

    /// Holds `repo`'s listener lock as a running waiter would, until dropped, when it lets go
    /// as a waiter does (`Held`).
    fn waiter_runs(db: &Path, repo: i64) -> Held {
        let held = open_lock(&lock_path(db, RepoId(repo))).unwrap();
        held.try_lock().unwrap();
        Held(held)
    }

    #[test]
    fn the_hook_counts_a_turn_the_attached_session_ends_with_no_waiter_only() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        ends_turn(&db, "sess-1");
        ends_turn(&db, "sess-1");
        // A worker, a stranger, the other lead's never-attached session: not counted.
        for other in ["sess-9", "other-lead"] {
            ends_turn(&db, other);
        }
        assert_eq!((turns(&mut store, 1), turns(&mut store, 2)), (2, 0));
        // A stop held for unread posts goes on: no turn ended, not counted; the stop that
        // then ends it is.
        store.post(ThreadId(1), "dev-1", "status: one").unwrap();
        assert!(hook(&db, &input("sess-1")).unwrap().is_some());
        assert_eq!(turns(&mut store, 1), 2);
        ends_turn(&db, "sess-1");
        assert_eq!(turns(&mut store, 1), 3);
        // A turn ended with its waiter running is no turn without one: not counted.
        let held = waiter_runs(&db, 1);
        ends_turn(&db, "sess-1");
        assert_eq!(turns(&mut store, 1), 3);
        drop(held);
        // Re-attached to a new session: the old one's stops no longer count.
        store.attach_lead(RepoId(1), "sess-2").unwrap();
        ends_turn(&db, "sess-1");
        assert_eq!(turns(&mut store, 1), 3);
        ends_turn(&db, "sess-2");
        assert_eq!(turns(&mut store, 1), 4);
    }

    #[test]
    fn a_short_turn_right_after_a_look_that_saw_the_waiter_is_counted() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let idle = agents(&[("sess-1", "idle")]);
        // A look sees its waiter running. Before the next, the waiter ends on a post, and
        // the lead reads it and ends a short turn without re-arming, then sits idle: the
        // next look raises it.
        let held = waiter_runs(&db, 1);
        assert!(look_with(&mut store, &db, &idle, 10).is_empty());
        drop(held);
        ends_turn(&db, "sess-1");
        assert_eq!(
            look_with(&mut store, &db, &idle, 30),
            [
                "Catalogue, the lead of R1, is not listening (its Claude session finished a turn \
                 without one); told the owner in T3"
            ]
        );
        // Resolved by its waiter, then the same again: raised again, as a new episode.
        let held = waiter_runs(&db, 1);
        assert_eq!(look_with(&mut store, &db, &idle, 40).len(), 1);
        drop(held);
        ends_turn(&db, "sess-1");
        assert_eq!(look_with(&mut store, &db, &idle, 50).len(), 1);
        // Re-armed in that turn instead: the turn is not counted, nothing raised.
        let held = waiter_runs(&db, 1);
        assert_eq!(look_with(&mut store, &db, &idle, 60).len(), 1);
        drop(held);
        let held = waiter_runs(&db, 1);
        ends_turn(&db, "sess-1");
        drop(held);
        assert!(look_with(&mut store, &db, &idle, 70).is_empty());
        assert_eq!(items(&mut store).len(), 0);
    }

    #[test]
    fn the_gap_between_a_waiters_end_and_its_re_arm_never_raises() {
        let (_dir, db, mut store) = board();
        // R2's lead never attached: never checked by its session.
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let other = ("sess-x", "idle");
        let look = |store: &mut Store, status: &str| {
            look_with(store, &db, &agents(&[("sess-1", status), other]), 10)
        };
        // An empty list says nothing.
        assert!(look_with(&mut store, &db, &agents(&[]), 10).is_empty());
        // With its waiter running, claude is not even asked, and nothing is recorded.
        let held = waiter_runs(&db, 1);
        let never = || -> Result<String> { panic!("claude was asked") };
        assert!(check_leads(&mut store, &db, never, 10, &mut HashMap::new()).is_empty());
        drop(held);
        // The waiter ended on a post while the lead was idle: no listener, idle, seen first
        // before its end notice has started a turn.
        assert!(look(&mut store, "idle").is_empty());
        assert!(
            look(&mut store, "idle").is_empty(),
            "idle since: still the gap"
        );
        // The end notice wakes it; it reads, re-arms in that turn and ends it.
        assert!(look(&mut store, "busy").is_empty());
        let held = waiter_runs(&db, 1);
        ends_turn(&db, "sess-1");
        assert!(look(&mut store, "idle").is_empty());
        drop(held);
        // The next end: seen first again now, not compared with the last episode's count.
        assert!(look(&mut store, "idle").is_empty());
        assert!(look(&mut store, "idle").is_empty());
        assert!(items(&mut store).is_empty());
        // It finished a turn and did not re-arm: now it is not listening.
        assert!(look(&mut store, "busy").is_empty());
        ends_turn(&db, "sess-1");
        assert_eq!(
            look(&mut store, "idle"),
            [
                "Catalogue, the lead of R1, is not listening (its Claude session finished a turn \
                 without one); told the owner in T3"
            ]
        );
        assert_eq!(
            items(&mut store),
            [
                "@manager Catalogue, the lead of R1, is not listening: no board waiter runs and \
                 its Claude session finished a turn without one, so posts on R1 wait until it \
                 next looks. If a session leads R1, it arms one with tower lead wait R1, run \
                 with Bash run_in_background; if nobody leads R1 any more, take the lead off \
                 with tower lead remove R1."
            ]
        );
        let thread = store.thread(ThreadId(3)).unwrap();
        assert_eq!(
            (thread.title.as_str(), thread.task),
            (crate::store::NOT_LISTENING, None)
        );
        // Once per episode, another turn or gone alike.
        ends_turn(&db, "sess-1");
        assert!(look(&mut store, "idle").is_empty());
        assert!(look_with(&mut store, &db, &agents(&[other]), 30).is_empty());
        assert_eq!(items(&mut store).len(), 1);
        // A waiter runs: the episode ends, its thread resolved by system, off the alert line.
        let held = waiter_runs(&db, 1);
        assert_eq!(
            look(&mut store, "idle"),
            ["Catalogue, the lead of R1, listens again; resolved T3"]
        );
        let thread = store.thread(ThreadId(3)).unwrap();
        assert_eq!(
            thread.resolved.as_ref().map(|r| r.by.as_str()),
            Some("system")
        );
        assert!(items(&mut store).is_empty());
        assert!(look(&mut store, "idle").is_empty());
        // Its session gone from Claude's list and no waiter: at once a new episode, a new item.
        drop(held);
        assert_eq!(
            look_with(&mut store, &db, &agents(&[other]), 60),
            [
                "Catalogue, the lead of R1, is not listening (its Claude session is gone); told \
                 the owner in T4"
            ]
        );
    }

    #[test]
    fn a_turn_seen_busy_with_no_waiter_and_then_idle_is_not_listening() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let look = |store: &mut Store, status: &str| {
            look_with(store, &db, &agents(&[("sess-1", status)]), 10)
        };
        // The waiter ended while the lead worked; it went on working and ended the turn
        // without re-arming, then sat idle: raised at its first idle look.
        assert!(look(&mut store, "busy").is_empty());
        assert!(look(&mut store, "busy").is_empty());
        ends_turn(&db, "sess-1");
        assert_eq!(
            look(&mut store, "idle"),
            [
                "Catalogue, the lead of R1, is not listening (its Claude session finished a turn \
                 without one); told the owner in T3"
            ]
        );
        assert!(look(&mut store, "idle").is_empty(), "raised twice");
        // A status that is neither busy nor idle (or none) says nothing about a turn: not
        // recorded, so a turn ended after it does not raise at the first idle look.
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let look = |store: &mut Store, list: &str| look_with(store, &db, list, 10);
        let unknown = agents(&[("sess-1", "waiting")]);
        let bare = r#"[{"sessionId":"sess-1","kind":"background","state":"blocked"}]"#;
        for list in [unknown.as_str(), bare] {
            assert!(look(&mut store, list).is_empty());
        }
        ends_turn(&db, "sess-1");
        assert!(look(&mut store, &agents(&[("sess-1", "idle")])).is_empty());
        // Busy and re-armed before the turn ended: that turn is not counted, nothing raised.
        let busy = agents(&[("sess-1", "busy")]);
        assert!(look(&mut store, &busy).is_empty());
        let held = waiter_runs(&db, 1);
        assert!(look(&mut store, &busy).is_empty());
        ends_turn(&db, "sess-1");
        drop(held);
        let idle = agents(&[("sess-1", "idle")]);
        assert!(look(&mut store, &idle).is_empty());
        assert!(look(&mut store, &idle).is_empty());
        assert!(items(&mut store).is_empty());
    }

    #[test]
    fn a_short_turn_between_two_looks_is_counted() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let idle = agents(&[("sess-1", "idle")]);
        // Seen idle with no waiter; a whole turn starts and ends before the next look, which
        // sees it idle again: the hook counted the turn, so it is not listening.
        assert!(look_with(&mut store, &db, &idle, 10).is_empty());
        ends_turn(&db, "sess-1");
        assert_eq!(look_with(&mut store, &db, &idle, 30).len(), 1);
    }

    #[test]
    fn a_new_session_starts_the_look_afresh() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let look = |store: &mut Store, session: &str| {
            look_with(store, &db, &agents(&[(session, "idle")]), 10)
        };
        assert!(look(&mut store, "sess-1").is_empty());
        // The lead restarted and attached again: what the watch saw of sess-1 goes.
        ends_turn(&db, "sess-1");
        store.attach_lead(RepoId(1), "sess-2").unwrap();
        assert!(look(&mut store, "sess-2").is_empty());
        assert!(look(&mut store, "sess-2").is_empty());
        ends_turn(&db, "sess-2");
        assert_eq!(look(&mut store, "sess-2").len(), 1);
    }

    #[test]
    fn an_episode_its_thread_resolved_by_hand_ends_quietly_when_a_waiter_runs() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let idle = agents(&[("sess-1", "idle")]);
        let look = |store: &mut Store| {
            ends_turn(&db, "sess-1");
            look_with(store, &db, &idle, 10)
        };
        assert!(look(&mut store).is_empty());
        assert_eq!(look(&mut store).len(), 1);
        store
            .resolve_thread(ThreadId(3), "cat-lead", "re-armed")
            .unwrap();
        // Still deaf: no second item while the episode is open.
        assert!(look(&mut store).is_empty());
        let _held = waiter_runs(&db, 1);
        assert_eq!(
            look(&mut store),
            ["Catalogue, the lead of R1, listens again; resolved T3"]
        );
        let thread = store.thread(ThreadId(3)).unwrap();
        assert_eq!(thread.messages.last().unwrap().text, "re-armed");
    }

    #[test]
    fn the_watchs_own_not_listening_posts_are_never_the_leads_unread() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let gone = agents(&[("sess-x", "idle")]);
        assert_eq!(look_with(&mut store, &db, &gone, 10).len(), 1);
        // The owner answering there is for the lead; the watch's own posts are not.
        store.post(ThreadId(3), "manager", "please re-arm").unwrap();
        let _held = waiter_runs(&db, 1);
        assert_eq!(look_with(&mut store, &db, &gone, 20).len(), 1);
        assert_eq!(told(&mut store, 1), ["[T3] manager: please re-arm"]);
        // A system post anywhere else (a flag in a task's thread) still is.
        store.exec_sql(
            "INSERT INTO message (thread_id, author, text, posted_at) VALUES (1, 'system', 'flag', 30)",
        );
        assert_eq!(told(&mut store, 1), ["[T1] system: flag"]);
    }

    #[test]
    fn a_claude_list_failing_or_garbled_is_one_logged_line_and_changes_nothing() {
        let (_dir, db, mut store) = board();
        // Both leads attached, neither listening, the lead of R1 seen idle once.
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        store.attach_lead(RepoId(2), "sess-2").unwrap();
        let idle = agents(&[("sess-1", "idle"), ("sess-2", "busy")]);
        assert!(look_with(&mut store, &db, &idle, 10).is_empty());
        ends_turn(&db, "sess-1");
        let before = store.leads().unwrap();
        // Garbled, or claude failing: asked once for both leads, one line, nothing raised.
        let asked = std::cell::Cell::new(0);
        let garbled = || {
            asked.set(asked.get() + 1);
            Ok("nonsense".to_string())
        };
        let log = check_leads(&mut store, &db, garbled, 20, &mut HashMap::new());
        assert_eq!(asked.get(), 1);
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].starts_with(
                "could not check the leads are listening: claude agents --json said something \
                 unexpected"
            ),
            "{log:?}"
        );
        let failing = || -> Result<String> { anyhow::bail!("claude agents --json exited 1: no") };
        assert_eq!(
            check_leads(&mut store, &db, failing, 20, &mut HashMap::new()),
            ["could not check the leads are listening: claude agents --json exited 1: no"]
        );
        assert_eq!(store.leads().unwrap(), before);
        assert!(items(&mut store).is_empty());
        // Read again, the turn it ended is still there to raise.
        assert_eq!(look_with(&mut store, &db, &idle, 30).len(), 1);
    }

    fn start(session: &str, source: &str, title: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "SessionStart",
            "source": source,
            "cwd": "/somewhere",
        });
        if let Some(t) = title {
            v["session_title"] = t.into();
        }
        v.to_string()
    }

    /// `claude agents --json` listing `sessions`, each (id, name).
    fn claude_lists(sessions: &[(&str, &str)]) -> String {
        let list: Vec<serde_json::Value> = (sessions.iter())
            .map(|(id, name)| {
                serde_json::json!({"sessionId": id, "name": name, "kind": "interactive"})
            })
            .collect();
        serde_json::Value::Array(list).to_string()
    }

    /// The context a SessionStart reply adds, or `None` for no reply.
    fn context(reply: Option<String>) -> Option<String> {
        reply.map(|r| {
            let v: serde_json::Value = serde_json::from_str(&r).unwrap();
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string()
        })
    }

    fn attached(store: &mut Store, repo: i64) -> Option<String> {
        (store.attached_leads().unwrap().into_iter())
            .find(|a| a.repo == RepoId(repo))
            .and_then(|a| a.claude_session)
    }

    #[test]
    fn a_session_named_as_no_lead_passes_without_asking_claude_or_writing() {
        let (dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let never = || -> Result<String> { panic!("claude was asked") };
        // No name, a blank one, a name no lead has, and a lead's display name (not its
        // session): all pass at once.
        for title in [None, Some("  "), Some("dev-4"), Some("Catalogue")] {
            let reply = session_start(&db, &start("sess-9", "startup", title), never).unwrap();
            assert_eq!(reply, None, "{title:?}");
        }
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-1"));
        // A board that does not exist is not made.
        let none = dir.path().join("none.db");
        let reply = session_start(&none, &start("s", "startup", Some("cat-lead")), never);
        assert_eq!(reply.unwrap(), None);
        assert!(!none.exists());
        // Input that is not a SessionStart's is an error for the caller to keep quiet.
        assert!(session_start(&db, "{}", never).is_err());
    }

    #[test]
    fn a_restarted_lead_is_attached_and_told_how_to_resume_by_source() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        // The lead skill cannot be invoked by the model (disable-model-invocation): a
        // session with no conversation arms its board waiter and asks the owner for it.
        let startup = "control-tower: this session is Catalogue, the lead of R1, and is \
                       attached. Arm your board waiter now: run tower lead unread R1, then \
                       tower lead wait R1 with Bash run_in_background. The lead skill is \
                       started by the owner: ask them to run /control-tower:lead R1 to resume \
                       leading R1 (it arms your forge waiter).";
        let resume = "control-tower: re-attached as Catalogue, the lead of R1; your waiters \
                      ended with the old session. Run tower lead unread R1, then tower lead \
                      wait R1 with Bash run_in_background, and arm your forge waiter (lead \
                      skill, New issues and idle).";
        // sess-1 is gone; each new session under the lead's name (any case) takes over. After
        // a /clear only the new id is listed: the old one has left.
        for (n, source, want) in [
            (2, "startup", startup),
            (3, "clear", startup),
            (4, "resume", resume),
            (5, "fork", resume),
            (6, "compact", resume),
        ] {
            let id = format!("sess-{n}");
            let lists = claude_lists(&[(&id, "Cat-Lead"), ("other", "dev-1")]);
            let reply = session_start(&db, &start(&id, source, Some("Cat-Lead")), || {
                Ok(lists.clone())
            });
            assert_eq!(context(reply.unwrap()).as_deref(), Some(want), "{source}");
            assert_eq!(attached(&mut store, 1), Some(id.clone()), "{source}");
        }
        // Attached, the Stop hook holds it while a post is unread.
        store
            .post(ThreadId(1), "dev-1", "status: while it restarted")
            .unwrap();
        let held = hook(&db, &input("sess-6")).unwrap().unwrap();
        assert!(
            held.contains("[T1] dev-1: status: while it restarted"),
            "{held}"
        );
        // A compaction with its board waiter still running: nothing to say.
        let _held = waiter_runs(&db, 1);
        let lists = claude_lists(&[("sess-6", "cat-lead")]);
        let reply = session_start(&db, &start("sess-6", "compact", Some("cat-lead")), || {
            Ok(lists.clone())
        });
        assert_eq!(reply.unwrap(), None);
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-6"));
        // A /clear with its waiters still running (they outlive it): attached again, and
        // told not to arm them again.
        let lists = claude_lists(&[("sess-7", "cat-lead")]);
        let reply = session_start(&db, &start("sess-7", "clear", Some("cat-lead")), || {
            Ok(lists.clone())
        });
        assert_eq!(
            context(reply.unwrap()).as_deref(),
            Some(
                "control-tower: this session is Catalogue, the lead of R1, and is attached again \
                 after /clear. Your board waiter and forge waiter still run (background \
                 commands outlive /clear), so do not arm them again. If you need the lead \
                 skill again, ask the owner to run /control-tower:lead R1."
            )
        );
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-7"));
        // A /clear or a compaction is the same process going on: should claude still list the
        // id it had before, the session is attached all the same, never taken for a fork.
        for (n, source) in [(8, "clear"), (9, "compact")] {
            let (old, id) = (format!("sess-{}", n - 1), format!("sess-{n}"));
            let lists = claude_lists(&[(&old, "cat-lead"), (&id, "cat-lead")]);
            let reply = session_start(&db, &start(&id, source, Some("cat-lead")), || {
                Ok(lists.clone())
            });
            assert!(
                !context(reply.unwrap())
                    .unwrap_or_default()
                    .contains("is not attached"),
                "{source}"
            );
            assert_eq!(attached(&mut store, 1), Some(id.clone()), "{source}");
        }
        // R2's lead was never touched.
        assert_eq!(attached(&mut store, 2), None);
    }

    #[test]
    fn a_starting_session_claude_does_not_list_yet_is_asked_for_again() {
        let (_dir, db, mut store) = board();
        // Listed only from the third ask: still attached, after two waits.
        let asked = std::cell::Cell::new(0);
        let lists = claude_lists(&[("sess-2", "cat-lead")]);
        let agents = || {
            asked.set(asked.get() + 1);
            Ok(if asked.get() < 3 {
                "[]".to_string()
            } else {
                lists.clone()
            })
        };
        let reply = session_start(&db, &start("sess-2", "startup", Some("cat-lead")), agents);
        assert!(context(reply.unwrap()).unwrap().ends_with(
            "ask them to run /control-tower:lead R1 to resume leading R1 (it arms your forge \
             waiter)."
        ));
        assert_eq!(asked.get(), 3);
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-2"));
        // Never listed: asked LISTED_TRIES times, then refused.
        asked.set(0);
        let never = || {
            asked.set(asked.get() + 1);
            Ok("[]".to_string())
        };
        let err = session_start(&db, &start("sess-3", "startup", Some("cat-lead")), never);
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("lists no session sess-3")
        );
        assert_eq!(asked.get(), LISTED_TRIES);
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-2"));
    }

    #[test]
    fn a_session_claude_does_not_list_under_the_name_or_a_second_live_one_is_not_attached() {
        let (_dir, db, mut store) = board();
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        // Claude lists this session under another name: refused, as tower lead attach is.
        let lists = claude_lists(&[("sess-2", "something else")]);
        let err = session_start(&db, &start("sess-2", "startup", Some("cat-lead")), || {
            Ok(lists.clone())
        })
        .unwrap_err();
        assert!(err.to_string().contains("not \"cat-lead\""), "{err}");
        // Not listed at all: refused too. Claude failing: its error, for the caller.
        let err = session_start(&db, &start("sess-2", "startup", Some("cat-lead")), || {
            Ok("[]".into())
        })
        .unwrap_err();
        assert!(err.to_string().contains("lists no session sess-2"), "{err}");
        let err = session_start(&db, &start("sess-2", "startup", Some("cat-lead")), || {
            anyhow::bail!("claude agents failed")
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "claude agents failed");
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-1"));
        // The attached session still runs and a second one starts under its name (a fork):
        // the lead keeps its session, and the second is told it is not the attached one.
        let lists = claude_lists(&[("sess-1", "cat-lead"), ("sess-2", "cat-lead")]);
        let reply = session_start(&db, &start("sess-2", "fork", Some("cat-lead")), || {
            Ok(lists.clone())
        });
        assert_eq!(
            context(reply.unwrap()).as_deref(),
            Some(
                "control-tower: this session is named cat-lead, the session of Catalogue, the \
                 lead of R1, but the lead's attached Claude session sess-1 is still running, \
                 so this one is not attached: do not arm the lead's board waiter here."
            )
        );
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-1"));
        // Once sess-1 has ended, the same start takes over.
        let lists = claude_lists(&[("sess-2", "cat-lead")]);
        let reply = session_start(&db, &start("sess-2", "fork", Some("cat-lead")), || {
            Ok(lists.clone())
        });
        assert!(
            context(reply.unwrap())
                .unwrap()
                .starts_with("control-tower: re-attached")
        );
        assert_eq!(attached(&mut store, 1).as_deref(), Some("sess-2"));
    }

    #[test]
    fn a_lead_with_no_attached_session_is_not_listening_after_a_grace() {
        let (_dir, db, mut store) = board();
        // Neither lead attached; R2's has its waiter running.
        let r2 = waiter_runs(&db, 2);
        let mut unheard = HashMap::new();
        let list = agents(&[("sess-x", "idle")]);
        let mut look = |store: &mut Store, now: i64| {
            check_leads(store, &db, || Ok(list.clone()), now, &mut unheard)
        };
        // First seen at 100, with no listener: within the grace, nothing.
        assert!(look(&mut store, 100).is_empty());
        assert!(look(&mut store, 100 + NO_SESSION_GRACE - 1).is_empty());
        assert!(items(&mut store).is_empty());
        // Past it: R1's lead is not listening, once; R2's, with its waiter, never.
        assert_eq!(
            look(&mut store, 100 + NO_SESSION_GRACE),
            [
                "Catalogue, the lead of R1, is not listening (no Claude session is attached to \
                 it (tower lead attach)); told the owner in T3"
            ]
        );
        assert_eq!(
            items(&mut store),
            [
                "@manager Catalogue, the lead of R1, is not listening: no board waiter runs and \
                 no Claude session is attached to it (tower lead attach), so posts on R1 wait \
                 until it next looks. If a session leads R1, it arms one with tower lead wait \
                 R1, run with Bash run_in_background; if nobody leads R1 any more, take the \
                 lead off with tower lead remove R1."
            ]
        );
        assert!(look(&mut store, 100 + 2 * NO_SESSION_GRACE).is_empty());
        // Its waiter runs: the episode ends, and the grace starts afresh after.
        let r1 = waiter_runs(&db, 1);
        assert_eq!(
            look(&mut store, 2000),
            ["Catalogue, the lead of R1, listens again; resolved T3"]
        );
        drop(r1);
        assert!(look(&mut store, 2001).is_empty());
        assert!(look(&mut store, 2001 + NO_SESSION_GRACE - 1).is_empty());
        // Attached, it is checked by its Claude session instead, and its grace is forgotten:
        // detached again later, the grace starts from then.
        store.attach_lead(RepoId(1), "sess-1").unwrap();
        let list = agents(&[("sess-1", "busy")]);
        assert!(check_leads(&mut store, &db, || Ok(list.clone()), 3000, &mut unheard).is_empty());
        assert!(unheard.is_empty());
        drop(r2);
    }

    #[test]
    fn a_new_lead_on_a_repo_gets_its_own_grace() {
        let (_dir, db, mut store) = board();
        let _r2 = waiter_runs(&db, 2);
        let mut unheard = HashMap::new();
        let list = agents(&[("sess-x", "idle")]);
        let mut look = |store: &mut Store, now: i64| {
            check_leads(store, &db, || Ok(list.clone()), now, &mut unheard)
        };
        let raised = "Next, the lead of R1, is not listening (no Claude session is attached to it \
                      (tower lead attach)); told the owner in T3";
        // R1's lead, first seen at 100, is taken off inside its grace, and a new lead is
        // registered on R1 before the next look: the new one's grace starts at that look.
        assert!(look(&mut store, 100).is_empty());
        store.remove_lead(RepoId(1), "gone").unwrap();
        store
            .add_lead(RepoOf::Repo(RepoId(1)), "Next", "next-lead", None)
            .unwrap();
        assert!(look(&mut store, 100 + NO_SESSION_GRACE).is_empty());
        assert!(look(&mut store, 100 + 2 * NO_SESSION_GRACE - 1).is_empty());
        assert_eq!(look(&mut store, 100 + 2 * NO_SESSION_GRACE), [raised]);
        // Taken off, a look with no lead on R1, then the same session registered again: a
        // fresh grace too, from its first look.
        store.remove_lead(RepoId(1), "gone").unwrap();
        assert!(look(&mut store, 5000).is_empty());
        store
            .add_lead(RepoOf::Repo(RepoId(1)), "Next", "next-lead", None)
            .unwrap();
        assert!(look(&mut store, 5001).is_empty());
        assert!(look(&mut store, 5001 + NO_SESSION_GRACE - 1).is_empty());
        assert_eq!(look(&mut store, 5001 + NO_SESSION_GRACE).len(), 1);
    }
}
