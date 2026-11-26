//! The machine's board in SQLite. The only code that touches the database file.
//!
//! Several agents write at once, so the file runs in WAL mode with a busy timeout and every
//! write is an IMMEDIATE transaction: it takes the write lock before reading, so a writer
//! waits its turn instead of failing with "database is locked" when it tries to upgrade.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::inbox::Inbox;
use anyhow::{Context, Result, bail};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::board::{
    Board, BoardOf, Colour, Criterion, FORGE_HOSTS, ForgeProject, Lead, Mention, Message,
    Milestone, MilestoneDetails, MilestoneId, OWNER_AUTHOR, OWNER_HANDLE, Repo, RepoId, RepoOf,
    Resolution, SYSTEM_AUTHOR, Stage, Subtask, Task, TaskDetails, TaskId, Thread, ThreadId,
    ThreadStatus, Tone, check_author, check_line, check_link, check_message, check_session,
    check_text, mentioned, no_lead_hint, tone_of,
};
use crate::deliver::{LeadBoard, Pending, Post};
use crate::watch::{Flag, Named, Signal};

/// The schema this build reads and writes, kept in SQLite's `user_version`.
pub const SCHEMA_VERSION: i32 = 19;

/// `MIGRATIONS[n]` takes the schema from version `n` to `n + 1`.
const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [
    "
    CREATE TABLE milestone (
        id    INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL,
        goal  TEXT NOT NULL
    );
    CREATE TABLE task (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        milestone_id INTEGER REFERENCES milestone(id),
        title        TEXT NOT NULL
    );
    CREATE INDEX task_milestone ON task(milestone_id);
",
    // Leads: one lead per milestone, and a lead (by name or session) on one milestone only.
    "
    CREATE TABLE lead (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        milestone_id INTEGER NOT NULL UNIQUE REFERENCES milestone(id),
        name         TEXT NOT NULL UNIQUE COLLATE NOCASE,
        session      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        colour       TEXT NOT NULL
    );
",
    // Stages: every task starts Waiting; a milestone's `done` is the lead's Done mark.
    // RULE: every task mutation that can change a milestone's task set or task stages (add,
    // stage move, and any future delete, reparent or subtask change) must clear that
    // milestone's `done` in the same transaction, so a Done mark never outlives the state the
    // lead confirmed. `Milestone::stage` also ignores a mark unless every task is Done.
    "
    ALTER TABLE task ADD COLUMN stage TEXT NOT NULL DEFAULT 'waiting'
        CHECK (stage IN ('waiting', 'in-progress', 'done'));
    ALTER TABLE milestone ADD COLUMN done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1));
",
    // Subtasks: a task row with `parent_id` set, one level only. A subtask's milestone is its
    // parent's, so its own `milestone_id` stays NULL; every read of a milestone's tasks or of
    // standalone tasks also filters on `parent_id IS NULL`, so subtasks never count towards a
    // milestone's percentage or its Done mark. A task is Done only once all its subtasks are,
    // and a subtask leaving Done takes a Done parent back to In progress, which changes its
    // milestone's task stages, so under the rule at MIGRATIONS[2] that clears the mark too.
    // Adding a subtask under a Done task does the same. A future subtask change that alters
    // a task set or task stages (deleting a parent, promoting a subtask) must clear it.
    "
    ALTER TABLE task ADD COLUMN parent_id INTEGER REFERENCES task(id)
        CHECK (parent_id IS NULL OR milestone_id IS NULL);
    CREATE INDEX task_parent ON task(parent_id);
",
    // Item details: what the detail views show. None of it changes a milestone's task set or
    // task stages, so none of it touches the Done mark. Criteria are numbered by id order
    // within their task; a task is blocked by other tasks, never by itself. Blocked by is a
    // dependency, not the blocked flag a worker sets when stuck: see GLOSSARY.md.
    "
    ALTER TABLE task ADD COLUMN description TEXT;
    ALTER TABLE task ADD COLUMN context TEXT;
    ALTER TABLE task ADD COLUMN link TEXT;
    ALTER TABLE task ADD COLUMN mr TEXT;
    ALTER TABLE task ADD COLUMN assignee TEXT;
    ALTER TABLE milestone ADD COLUMN context TEXT;
    ALTER TABLE milestone ADD COLUMN spec TEXT;
    ALTER TABLE milestone ADD COLUMN link TEXT;
    CREATE TABLE criterion (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id INTEGER NOT NULL REFERENCES task(id),
        text    TEXT NOT NULL,
        done    INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
        UNIQUE (task_id, text)
    );
    CREATE TABLE blocker (
        task_id    INTEGER NOT NULL REFERENCES task(id),
        blocker_id INTEGER NOT NULL REFERENCES task(id),
        PRIMARY KEY (task_id, blocker_id),
        CHECK (task_id != blocker_id)
    );
    CREATE INDEX blocker_of ON blocker(blocker_id);
",
    // Message boards: a lead's threads, each optionally linked to a task of the lead's
    // milestone (a stored link, never a title match), and their messages, ordered by
    // (posted_at, id). The first message opens the thread and its author is who started it.
    // Resolving posts the evidence as the last message and records who and when; a thread is
    // open while `resolved_at` is NULL, and nothing more is posted once it is not. RULE: a future change that moves a task to another
    // milestone, or makes it a subtask, must refuse while a thread links it, or move the
    // link, so a thread never points outside its lead's milestone.
    "
    CREATE TABLE thread (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        lead_id    INTEGER NOT NULL REFERENCES lead(id),
        task_id    INTEGER REFERENCES task(id),
        title       TEXT NOT NULL,
        resolved_by TEXT,
        resolved_at INTEGER,
        CHECK ((resolved_by IS NULL) = (resolved_at IS NULL))
    );
    CREATE INDEX thread_lead ON thread(lead_id);
    CREATE TABLE message (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id INTEGER NOT NULL REFERENCES thread(id),
        author    TEXT NOT NULL,
        text      TEXT NOT NULL,
        posted_at INTEGER NOT NULL
    );
    CREATE INDEX message_thread ON message(thread_id, posted_at, id);
",
    // The blocked flag: why a task is stuck, NULL while it is not. A flag on tasks,
    // never subtasks (a subtask's `parent_id` is set when it is added and never changes),
    // cleared when the task moves to Done. Not the `blocker` table, which records what a
    // task is blocked by. A progress post (`tower post --progress`) is a message marked
    // `progress`, only in a thread linked to a task; a task's doing line is its latest one.
    "
    ALTER TABLE task ADD COLUMN blocked TEXT
        CHECK (blocked IS NULL OR (blocked != '' AND parent_id IS NULL));
    ALTER TABLE message ADD COLUMN progress INTEGER NOT NULL DEFAULT 0
        CHECK (progress IN (0, 1));
    CREATE INDEX thread_task ON thread(task_id);
",
    // The owner's read mark: when the owner read a message that mentions @manager (an
    // awareness item read, or a need-you item cleared), NULL until then. Set only by
    // `Store::mark_read` (from the dashboard), which refuses any other message.
    "
    ALTER TABLE message ADD COLUMN read_at INTEGER;
",
    // The stuck-worker watch's memory: each flag it raised on a task and not yet seen
    // end, so it raises a flag once per episode instead of every tick. `worker` is who it
    // was raised for (the assignee; for a prompt, any member of the team);
    // `quiet_since` the task's latest post when it was raised
    // (NULL for none), so a later post ends a stopped or silent episode; `raised_at` when
    // it was raised (the nudge, for stopped and silent); `lead_told_at` when an unanswered
    // nudge was passed to the lead. `watch_seen` is when the watch first saw each task with
    // its assignee (lower case), so "no post" for a task with none survives a restart.
    // Written only by `tower watch`.
    "
    CREATE TABLE watch_flag (
        task_id      INTEGER NOT NULL REFERENCES task(id),
        signal       TEXT NOT NULL CHECK (signal IN ('prompt', 'stopped', 'silent', 'orphaned')),
        worker       TEXT NOT NULL,
        quiet_since  INTEGER,
        raised_at    INTEGER NOT NULL,
        lead_told_at INTEGER,
        PRIMARY KEY (task_id, signal)
    );
    CREATE TABLE watch_seen (
        task_id INTEGER NOT NULL REFERENCES task(id),
        worker  TEXT NOT NULL,
        since   INTEGER NOT NULL,
        PRIMARY KEY (task_id, worker)
    );
",
    // Who moved a task to Done, as `tower task stage N done --by <name>` names them;
    // NULL when nobody was named, and cleared whenever the task leaves Done. A done card with
    // no assignee says "done by lead" when it is the milestone's lead.
    "
    ALTER TABLE task ADD COLUMN done_by TEXT;
",
    // Whether the owner folded the milestone's task list in the dashboard's tree,
    // kept on the board so every dashboard shows the same folds; 0, unfolded, until then.
    "
    ALTER TABLE milestone ADD COLUMN folded INTEGER NOT NULL DEFAULT 0 CHECK (folded IN (0, 1));
",
    // The watch flags every member of a task's team waiting on a prompt, not only its
    // assignee, so a task holds a flag per signal and worker: two members can wait at
    // once. The worker compares ignoring case, as herdr names do. The flags are kept.
    "
    CREATE TABLE watch_flag_new (
        task_id      INTEGER NOT NULL REFERENCES task(id),
        signal       TEXT NOT NULL CHECK (signal IN ('prompt', 'stopped', 'silent', 'orphaned')),
        worker       TEXT NOT NULL COLLATE NOCASE,
        quiet_since  INTEGER,
        raised_at    INTEGER NOT NULL,
        lead_told_at INTEGER,
        PRIMARY KEY (task_id, signal, worker)
    );
    INSERT INTO watch_flag_new
        SELECT task_id, signal, worker, quiet_since, raised_at, lead_told_at FROM watch_flag;
    DROP TABLE watch_flag;
    ALTER TABLE watch_flag_new RENAME TO watch_flag;
",
    // Live delivery. `socket` and `claude_session` are the lead's Claude inbox socket
    // and session id, recorded by `tower lead attach`, NULL until then. `delivery.upto` is
    // the last message id every recipient has been offered (one row, starting at the newest
    // message when this migration runs, so no history is delivered). `delivery_sent` is how
    // far each recipient (`deliver::Recipient::key`) was sent beyond it, recorded before the
    // send, so a post reaches each recipient at most once even when a round stops part
    // way; rows at or below the cursor are dropped. `delivery_fail` holds one open failure
    // episode per recipient: its one item for the owner's awareness is posted when it opens,
    // and a send that gets through ends it.
    "
    ALTER TABLE lead ADD COLUMN socket TEXT;
    ALTER TABLE lead ADD COLUMN claude_session TEXT;
    CREATE TABLE delivery (upto INTEGER NOT NULL);
    INSERT INTO delivery (upto) SELECT coalesce(max(id), 0) FROM message;
    CREATE TABLE delivery_sent (
        recipient TEXT PRIMARY KEY COLLATE NOCASE,
        upto      INTEGER NOT NULL
    );
    CREATE TABLE delivery_fail (
        recipient TEXT PRIMARY KEY COLLATE NOCASE,
        since     INTEGER NOT NULL
    );
",
    // One lead per repo. A repo is a git repository, named by its main checkout's
    // absolute `path` and carrying its forge `link`; `path` is NULL only on a repo this
    // migration made, until `tower repo set` fills it. Every milestone and standalone task
    // belongs to one repo (NULL: not assigned yet); a task under a milestone and a subtask
    // never carry one, since their repo is their milestone's. A lead registers on a repo,
    // one lead per repo, and the message board belongs to the repo, so a thread outlives
    // its lead. A thread is about exactly one of a task (never a subtask), a milestone or
    // the whole repo, and stores only that: its repo is read through it, in one place, the
    // `thread_board` view, so a milestone or task moving repo takes its threads along. A
    // task a thread links to is in the thread's repo (checked by `refuse_foreign_task`).
    // RULE: a milestone with threads moves only to a repo with a lead, so no thread lands
    // on a board nobody reads (`Store::set_milestone`).
    // A milestone or standalone task with no repo is a transitional state, left by this
    // migration until the owner gives it one with `--repo`; nothing new is made that way
    // once a repo exists to name.
    // The migration: each lead gets a repo with the lead's own id and no path; its
    // milestone joins that repo; a thread linked to no task is about that milestone, so
    // each milestone keeps the board it had. A task a thread links to that would have no
    // repo (standalone, or under a lead-less milestone) gets the repo of the lead whose
    // board the thread was on (its milestone does, for a milestone task; the lowest lead
    // when several), so no thread loses its link or its board; `run_migrations` then
    // refuses, naming them, any thread still on no repo's board. Other milestones with no
    // lead and standalone tasks are left unassigned. Thread and lead ids are kept, so messages, read marks and watch
    // flags are untouched; delivery keys move from "lead:M<n>" to "lead:R<n>". Inserting
    // the kept ids sets each rebuilt table's sequence (nothing deletes leads or threads,
    // so no sequence is above its table's largest id). Run with foreign keys off (see
    // `migrate`), as SQLite's table rebuild requires, and checked after.
    "
    CREATE TABLE repo (
        id   INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT UNIQUE,
        link TEXT
    );
    INSERT INTO repo (id) SELECT id FROM lead ORDER BY id;
    ALTER TABLE milestone ADD COLUMN repo_id INTEGER REFERENCES repo(id);
    UPDATE milestone SET repo_id = (SELECT id FROM lead WHERE lead.milestone_id = milestone.id);
    ALTER TABLE task ADD COLUMN repo_id INTEGER REFERENCES repo(id)
        CHECK (repo_id IS NULL OR (milestone_id IS NULL AND parent_id IS NULL));
    UPDATE milestone SET repo_id = (SELECT min(t.lead_id) FROM thread t
            JOIN task k ON k.id = t.task_id WHERE k.milestone_id = milestone.id)
        WHERE repo_id IS NULL AND id IN (SELECT k.milestone_id FROM thread t
            JOIN task k ON k.id = t.task_id);
    UPDATE task SET repo_id = (SELECT min(t.lead_id) FROM thread t WHERE t.task_id = task.id)
        WHERE milestone_id IS NULL AND parent_id IS NULL
        AND id IN (SELECT task_id FROM thread);
    CREATE INDEX milestone_repo ON milestone(repo_id);
    CREATE INDEX task_repo ON task(repo_id);
    CREATE TABLE lead_new (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        repo_id        INTEGER NOT NULL UNIQUE REFERENCES repo(id),
        name           TEXT NOT NULL UNIQUE COLLATE NOCASE,
        session        TEXT NOT NULL UNIQUE COLLATE NOCASE,
        colour         TEXT NOT NULL,
        socket         TEXT,
        claude_session TEXT
    );
    INSERT INTO lead_new (id, repo_id, name, session, colour, socket, claude_session)
        SELECT id, id, name, session, colour, socket, claude_session FROM lead;
    CREATE TABLE thread_new (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        repo_id      INTEGER REFERENCES repo(id),
        milestone_id INTEGER REFERENCES milestone(id),
        task_id      INTEGER REFERENCES task(id),
        title        TEXT NOT NULL,
        resolved_by  TEXT,
        resolved_at  INTEGER,
        CHECK ((resolved_by IS NULL) = (resolved_at IS NULL)),
        CHECK ((repo_id IS NOT NULL) + (milestone_id IS NOT NULL) + (task_id IS NOT NULL) = 1)
    );
    INSERT INTO thread_new (id, milestone_id, task_id, title, resolved_by, resolved_at)
        SELECT t.id, CASE WHEN t.task_id IS NULL THEN l.milestone_id END,
               t.task_id, t.title, t.resolved_by, t.resolved_at
        FROM thread t JOIN lead l ON l.id = t.lead_id;
    UPDATE delivery_sent SET recipient = 'lead:R' ||
        (SELECT id FROM lead WHERE 'lead:M' || lead.milestone_id = delivery_sent.recipient)
        WHERE recipient IN (SELECT 'lead:M' || milestone_id FROM lead);
    UPDATE delivery_fail SET recipient = 'lead:R' ||
        (SELECT id FROM lead WHERE 'lead:M' || lead.milestone_id = delivery_fail.recipient)
        WHERE recipient IN (SELECT 'lead:M' || milestone_id FROM lead);
    DROP TABLE thread;
    DROP TABLE lead;
    ALTER TABLE lead_new RENAME TO lead;
    ALTER TABLE thread_new RENAME TO thread;
    CREATE INDEX thread_repo ON thread(repo_id);
    CREATE INDEX thread_milestone ON thread(milestone_id);
    CREATE INDEX thread_task ON thread(task_id);
    CREATE TRIGGER thread_not_on_a_subtask BEFORE INSERT ON thread
        WHEN NEW.task_id IN (SELECT id FROM task WHERE parent_id IS NOT NULL)
        BEGIN SELECT RAISE(ABORT, 'a thread links to a task, never a subtask'); END;
    CREATE TRIGGER thread_stays_off_subtasks BEFORE UPDATE OF task_id ON thread
        WHEN NEW.task_id IN (SELECT id FROM task WHERE parent_id IS NOT NULL)
        BEGIN SELECT RAISE(ABORT, 'a thread links to a task, never a subtask'); END;
    CREATE VIEW thread_board AS
        SELECT t.id, coalesce(t.repo_id, m.repo_id, k.repo_id, km.repo_id) AS repo_id
        FROM thread t
        LEFT JOIN milestone m ON m.id = t.milestone_id
        LEFT JOIN task k ON k.id = t.task_id
        LEFT JOIN milestone km ON km.id = k.milestone_id;
",
    // Whether the owner folded the repo in the dashboard's tree, kept on the board
    // like a milestone's fold, so every dashboard shows the same folds; 0, unfolded, until
    // then.
    "
    ALTER TABLE repo ADD COLUMN folded INTEGER NOT NULL DEFAULT 0 CHECK (folded IN (0, 1));
",
    // The Done group. `milestone.done_at` is when the lead set the Done mark, in
    // seconds since the Unix epoch: NULL while the mark is not set, and NULL on a mark set
    // before this version, whose date was never recorded. A trigger clears it whenever the
    // mark clears, so every write that clears `done` (the rule at MIGRATIONS[2]) clears the
    // date too. `repo.done_folded` is the fold of the repo's Done group, kept on the board
    // like a milestone's fold; 1, folded, by default.
    "
    ALTER TABLE milestone ADD COLUMN done_at INTEGER;
    CREATE TRIGGER milestone_done_at_clears AFTER UPDATE OF done ON milestone
        WHEN NEW.done = 0 AND NEW.done_at IS NOT NULL
        BEGIN UPDATE milestone SET done_at = NULL WHERE id = NEW.id; END;
    ALTER TABLE repo ADD COLUMN done_folded INTEGER NOT NULL DEFAULT 1
        CHECK (done_folded IN (0, 1));
",
    // Posts reach a lead through its board stream, not its Claude inbox socket, so
    // `socket` and the leads' delivery records go. `claude_session` stays: it is how the
    // lead's Stop hook and the watch know the lead's session. `told_upto` is the last
    // message id the lead has been told of, by its stream or its Stop hook (one cursor for
    // both), starting at the newest message, so no history floods a lead. `deaf_thread` is
    // the open "Lead not listening" episode's thread, NULL while none is open. `quiet_seq` is
    // herdr's state_change_seq of the lead's session when the watch first saw it, idle or
    // busy, with no stream running, NULL while it has not: the watch raises only once the session has
    // finished a turn since (the seq moved), so the gap between a Monitor's expiry and the
    // lead re-arming never raises.
    "
    ALTER TABLE lead DROP COLUMN socket;
    ALTER TABLE lead ADD COLUMN told_upto INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE lead ADD COLUMN deaf_thread INTEGER REFERENCES thread(id);
    ALTER TABLE lead ADD COLUMN quiet_seq INTEGER;
    UPDATE lead SET told_upto = (SELECT coalesce(max(id), 0) FROM message);
    DELETE FROM delivery_sent WHERE recipient LIKE 'lead:%';
    DELETE FROM delivery_fail WHERE recipient LIKE 'lead:%';
",
    // Leads taken off the board with `tower lead remove`, by name and by session, in
    // lower case: someone who once posted as a lead is no worker the board knew, so a failed
    // mention of one is its author's to fix, not the owner's (`deliver::Pending::known`).
    // Leads removed before this version are found from the watch's flags, which open with
    // "@<lead's session>" (watch.rs): a name a `system` post opens with, that someone posted
    // under and that no lead has now. Neither "@manager" nor tower's reply to an author
    // ("@<author> no worker is named ...") is a flag.
    "
    CREATE TABLE retired_lead (name TEXT PRIMARY KEY);
    INSERT OR IGNORE INTO retired_lead (name)
        SELECT n FROM (
            SELECT lower(substr(text, 2, instr(substr(text, 2) || ' ', ' ') - 1)) AS n
            FROM message
            WHERE author = 'system' AND text LIKE '@%'
                AND text NOT LIKE '@% no worker is named %'
        )
        WHERE n NOT IN ('', 'manager')
            AND n IN (SELECT lower(author) FROM message WHERE author != 'system')
            AND n NOT IN (SELECT lower(name) FROM lead UNION SELECT lower(session) FROM lead);
",
    // The lead check needs no terminal. `turns` is how many turns the lead's attached
    // session has ended with no waiter running, counted by its Stop hook; `quiet_seq`,
    // herdr's state_change_seq, becomes `quiet_turns`, that count when the watch last saw
    // the lead's waiter running, or first saw it, idle or busy, with none. A herdr seq
    // means nothing to the count, so an open record starts again at the count's 0: it stays
    // recorded (no fresh first look), and only a turn the hook counts from now on raises.
    // The open episodes (`deaf_thread`) are kept as they are.
    "
    ALTER TABLE lead RENAME COLUMN quiet_seq TO quiet_turns;
    ALTER TABLE lead ADD COLUMN turns INTEGER NOT NULL DEFAULT 0;
    UPDATE lead SET quiet_turns = 0 WHERE quiet_turns IS NOT NULL;
",
];

/// What `Store::add_message` posts: a plain message, a progress post, or the evidence that
/// resolves its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Progress,
    Evidence,
}

/// The mentions of the owner that may be items, of `message m`, with `?1` the owner's
/// handle: any message with the handle in it, in any ASCII case (as `Tone::of` reads it),
/// unread, on an open thread, which is all the inbox lists. `Inbox::of` and
/// `inbox::listed` classify each with `Tone::of`; this only narrows what is read.
const MENTIONS_WHERE: &str = "JOIN thread t ON t.id = m.thread_id \
     WHERE instr(lower(m.text), ?1) > 0 \
     AND m.read_at IS NULL AND t.resolved_at IS NULL";

/// How long a writer waits for another writer before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the dashboard waits for a lock before showing it as a problem. Readers in WAL
/// mode are not blocked by writers, so only something unusual holds them up this long.
const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

/// The board's path: `TOWER_DB` if set, else `~/.local/state/control-tower/board.db`, made
/// absolute against the working directory. Every process tower starts is handed this
/// path, some in another directory (a lead's Rex session opens on its repo's checkout), so a
/// relative one would name another file there.
pub fn board_path() -> Result<PathBuf> {
    let path = match std::env::var_os("TOWER_DB") {
        Some(p) if p.is_empty() => bail!("TOWER_DB is set but empty; unset it or give a file path"),
        Some(p) => PathBuf::from(p),
        None => default_board()?,
    };
    std::path::absolute(&path).with_context(|| {
        format!(
            "the board's path {} is relative and the working directory is unknown; give \
             TOWER_DB an absolute path",
            path.display()
        )
    })
}

/// Where the default board sits in a home directory.
const DEFAULT_BOARD: &str = ".local/state/control-tower/board.db";

/// The default board, `~/.local/state/control-tower/board.db`: the installed tower's.
fn default_board() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .context("HOME is not set, so the board's default path is unknown; set TOWER_DB")?;
    Ok(PathBuf::from(home).join(DEFAULT_BOARD))
}

/// Whether `path` is the default board of `$HOME` or of the account's own home from the
/// password database, so a run with a temp HOME and `TOWER_DB` naming the default board
/// is caught too.
fn is_default_board(path: &Path) -> bool {
    is_board_in(path, &homes())
}

/// `$HOME`, when set, and the account's home.
fn homes() -> Vec<PathBuf> {
    (std::env::var_os("HOME").filter(|h| !h.is_empty()))
        .map(PathBuf::from)
        .into_iter()
        .chain(account_home())
        .collect()
}

/// Whether `path` is the default board of any of `homes`: both are made absolute and
/// compared with their deepest existing ancestor resolved, so it is matched by any path
/// once it exists, and before that through a link or a relative path, but not through `..`
/// or another letter case in folders not created yet.
fn is_board_in(path: &Path, homes: &[PathBuf]) -> bool {
    let resolved = |p: &Path| {
        // absolute() fails only without a working directory; the path is then compared as
        // given, which still matches an absolute one.
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        (p.ancestors())
            .find_map(|a| Some(a.canonicalize().ok()?.join(p.strip_prefix(a).ok()?)))
            .unwrap_or(p)
    };
    let path = resolved(path);
    homes
        .iter()
        .any(|h| resolved(&h.join(DEFAULT_BOARD)) == path)
}

/// The account's home from the password database, whatever `HOME` says.
fn account_home() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current()).ok()??;
    Some(user.dir)
}

/// Refuses a tower that is not the brew install creating or upgrading the default board:
/// a newer dev build that did so would lock out the installed tower, the watch and every
/// lead. `found` is the board's schema version, `None` for no file.
fn refuse_dev_upgrade(path: &Path, found: Option<i32>) -> anyhow::Error {
    let exe = std::env::current_exe().map_or_else(|_| "?".into(), |e| e.display().to_string());
    let what = match found {
        None => "would create it".to_string(),
        Some(v) => format!(
            "would upgrade it from schema version {v} to {SCHEMA_VERSION}, \
             locking out the installed tower"
        ),
    };
    anyhow::anyhow!(
        "board {} is the installed tower's board; this tower ({exe}) is not the brew install \
         and {what}. Set TOWER_DB to a test board, for example TOWER_DB=/tmp/try.db",
        path.display()
    )
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

/// Puts the file in WAL mode. WAL is stored in the file, so this only switches once, on a
/// new board. Several processes opening a new board at once race for that switch, and
/// SQLite can answer the losers with SQLITE_BUSY without waiting on the busy timeout, so a
/// busy answer is retried until the timeout runs out.
fn use_wal(conn: &Connection) -> Result<()> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        if mode.eq_ignore_ascii_case("wal") {
            return Ok(());
        }
        match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
            Ok(m) if m.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(m) => bail!("it stays in {m} journal mode"),
            Err(e) if is_busy(&e) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The board's schema version, refusing one this build cannot read: newer than
/// `SCHEMA_VERSION`, or negative.
fn read_version(conn: &Connection) -> Result<i32> {
    let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        bail!(
            "it has schema version {v}, newer than this tower understands \
             ({SCHEMA_VERSION}); update tower"
        );
    }
    if v < 0 {
        bail!("it has schema version {v}, which no tower ever wrote");
    }
    Ok(v)
}

fn is_busy(e: &rusqlite::Error) -> bool {
    e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}

/// Refuses a milestone that is not on the board. The outer result is a database error, the
/// inner one the refusal, so a caller can name the board on the first only.
fn refuse_missing(conn: &Connection, milestone: MilestoneId) -> rusqlite::Result<Result<()>> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM milestone WHERE id = ?1",
            params![milestone.0],
            |_| Ok(()),
        )
        .optional()?;
    Ok(match exists {
        Some(()) => Ok(()),
        None => Err(anyhow::anyhow!(
            "there is no milestone {milestone} on the board"
        )),
    })
}

/// A watch signal is stored by its name; the schema's CHECK refuses any other text.
impl FromSql for Signal {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let name = value.as_str()?;
        Signal::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| FromSqlError::Other(format!("no watch signal {name:?}").into()))
    }
}

/// A stage is stored by its name; the schema's CHECK refuses any other text.
impl FromSql for Stage {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e: anyhow::Error| FromSqlError::Other(e.into()))
    }
}

/// `Store::mark_read`'s work in `tx`, at `at`: `None` when `message` is not on the board,
/// else whether it was unread and is read now. Refuses one that does not mention the owner.
fn mark_read_in(
    tx: &Connection,
    message: i64,
    at: i64,
    cannot_write: &str,
) -> Result<Option<bool>> {
    let found = tx
        .query_row(
            "SELECT m.author, m.text, m.read_at IS NOT NULL \
             FROM message m JOIN thread t ON t.id = m.thread_id \
             JOIN thread_board b ON b.id = t.id WHERE m.id = ?1",
            params![message],
            |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, bool>(2)?)),
        )
        .optional()
        .context(cannot_write.to_string())?;
    let Some((author, text, read)) = found else {
        return Ok(None);
    };
    let m = Message {
        id: message,
        author,
        text,
        at,
    };
    if !matches!(tone_of(&m), Tone::NeedsYou | Tone::Awareness) {
        bail!("message {message} is not for the owner: it does not mention {OWNER_HANDLE}");
    }
    if read {
        return Ok(Some(false));
    }
    tx.execute(
        "UPDATE message SET read_at = ?1 WHERE id = ?2",
        params![at, message],
    )
    .context(cannot_write.to_string())?;
    Ok(Some(true))
}

/// Seconds since the Unix epoch, the time a message is stamped with.
fn now() -> Result<i64> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("the system clock is before 1970, so a message cannot be stamped")?;
    Ok(since.as_secs() as i64)
}

/// The board `of` names: its repo, and the milestone a new thread there is about (none for
/// a lead's session, whose thread is about the whole repo). Refuses a repo with no lead,
/// which has no board yet. Outer and inner results as in `refuse_missing`.
fn board_of(conn: &Connection, of: &BoardOf) -> rusqlite::Result<Result<(RepoId, Option<i64>)>> {
    let found = match of {
        BoardOf::Milestone(m) => {
            if let Err(e) = refuse_missing(conn, *m)? {
                return Ok(Err(e));
            }
            let (repo, lead): (Option<i64>, Option<i64>) = conn.query_row(
                "SELECT m.repo_id, l.repo_id FROM milestone m \
                 LEFT JOIN lead l ON l.repo_id = m.repo_id WHERE m.id = ?1",
                params![m.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            lead.map(|r| (RepoId(r), Some(m.0))).ok_or_else(|| {
                anyhow::anyhow!(
                    "{m} has no lead yet, so it has no message board; {}",
                    no_lead_hint(*m, repo.is_some())
                )
            })
        }
        BoardOf::Session(session) => conn
            .query_row(
                "SELECT repo_id FROM lead WHERE session = ?1",
                params![session],
                |r| Ok((RepoId(r.get(0)?), None)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("no lead has the session {session}")),
    };
    Ok(found)
}

/// The SQL for the repo of the task `?1`: its own if standalone, else its milestone's. NULL
/// when it has none, and for a subtask, which has no board of its own.
const TASK_REPO: &str = "SELECT coalesce(t.repo_id, m.repo_id) \
     FROM task t LEFT JOIN milestone m ON m.id = t.milestone_id WHERE t.id = ?1";

/// Refuses linking a thread on `repo`'s board to `task` unless the task is a task (not a
/// subtask) in that repo. Outer and inner results as in `refuse_missing`.
fn refuse_foreign_task(
    conn: &Connection,
    repo: RepoId,
    task: TaskId,
) -> rusqlite::Result<Result<()>> {
    let parent: Option<Option<i64>> = conn
        .query_row(
            "SELECT parent_id FROM task WHERE id = ?1",
            params![task.0],
            |r| r.get(0),
        )
        .optional()?;
    let why = match parent {
        None => format!("there is no task {task} on the board"),
        Some(Some(parent)) => format!(
            "{task} is a subtask of {}; link the thread to {} instead",
            TaskId(parent),
            TaskId(parent)
        ),
        Some(None) => {
            let in_repo: Option<i64> = conn.query_row(TASK_REPO, params![task.0], |r| r.get(0))?;
            match in_repo {
                Some(r) if r == repo.0 => return Ok(Ok(())),
                Some(r) => format!(
                    "{task} is in {}, not {repo}; a thread links to a task of its own repo",
                    RepoId(r)
                ),
                None => format!("{task} is in no repo; a thread links to a task of its own repo"),
            }
        }
    };
    Ok(Err(anyhow::anyhow!(why)))
}

/// Refuses the tracker link of `task` when it is another repo's forge ticket, by
/// `refuse_link_outside`, naming the task. A task with no link, or in no repo, passes: it is
/// checked when it gets one. Outer and inner results as in `refuse_missing`.
fn refuse_foreign_link(conn: &Connection, task: TaskId) -> rusqlite::Result<Result<()>> {
    let (link, repo): (Option<String>, Option<i64>) = conn.query_row(
        "SELECT t.link, coalesce(t.repo_id, m.repo_id) \
         FROM task t LEFT JOIN milestone m ON m.id = t.milestone_id WHERE t.id = ?1",
        params![task.0],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    match (link, repo) {
        (Some(link), Some(repo)) => {
            refuse_link_outside(conn, &link, RepoId(repo), &task.to_string())
        }
        _ => Ok(Ok(())),
    }
}

/// Refuses `link` as the tracker link of a task in `repo` (`of` names what the task is or
/// will be in, for the message: its id, or the milestone a new task goes under) when it is
/// an issue, work item, or merge or pull request of a forge project that is not `repo`'s,
/// so one repo's work is never filed under another's. A link is read as a forge's only on
/// a forge host the board knows (`FORGE_HOSTS`, or the host of any repo's forge link), as
/// the path alone cannot tell a forge from another tracker; any other link passes (another
/// tracker, a document). A repo with no forge link cannot tell whose a forge ticket is, so
/// it refuses one, and so does a repo whose link goes on past the ticket's project, which is
/// then no project's address. Outer and inner results as in `refuse_missing`.
fn refuse_link_outside(
    conn: &Connection,
    link: &str,
    repo: RepoId,
    of: &str,
) -> rusqlite::Result<Result<()>> {
    let Some(project) = ForgeProject::of_ticket(link) else {
        return Ok(Ok(()));
    };
    let repos = conn
        .prepare("SELECT id, link FROM repo WHERE link IS NOT NULL ORDER BY id")?
        .query_map([], |r| Ok((RepoId(r.get(0)?), r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let known = FORGE_HOSTS.contains(&project.host())
        || (repos.iter())
            .any(|(_, l)| ForgeProject::of_repo(l).is_some_and(|p| p.host() == project.host()));
    if !known {
        return Ok(Ok(()));
    }
    let Some((_, own)) = repos.iter().find(|(r, _)| *r == repo) else {
        return Ok(Err(anyhow::anyhow!(
            "{repo}, the repo of {of}, has no forge link, so tower cannot tell whose {link} \
             is; set it first: tower repo set {repo} --link <forge URL>"
        )));
    };
    let own_project = ForgeProject::of_repo(own);
    if own_project.as_ref() == Some(&project) {
        return Ok(Ok(()));
    }
    let owner = (repos.iter()).find(|(_, l)| ForgeProject::of_repo(l).as_ref() == Some(&project));
    let why = match owner {
        Some((other, other_link)) => format!(
            "{link} belongs to {other} ({other_link}), not to {repo} ({own}), the repo of \
             {of}; file it in {other}"
        ),
        None if own_project.is_some_and(|p| p.goes_past(&project)) => format!(
            "{repo}'s link ({own}) is not a project's address; set it to the project: tower \
             repo set {repo} --link <project URL>"
        ),
        None => format!(
            "{link} belongs to {project}, which is no repo on the board, not to {repo} \
             ({own}), the repo of {of}"
        ),
    };
    Ok(Err(anyhow::anyhow!(why)))
}

/// The task columns `task_from_row` reads.
const TASK_COLUMNS: &str =
    "id, title, stage, description, context, link, mr, assignee, blocked, done_by, repo_id";

/// A task without its subtasks, criteria, what it is blocked by and its doing post: `load`
/// and `task` add them.
fn task_from_row(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: TaskId(r.get("id")?),
        title: r.get("title")?,
        stage: r.get("stage")?,
        repo: r.get::<_, Option<i64>>("repo_id")?.map(RepoId),
        subtasks: Vec::new(),
        details: TaskDetails {
            description: r.get("description")?,
            context: r.get("context")?,
            link: r.get("link")?,
            mr: r.get("mr")?,
            assignee: r.get("assignee")?,
            criteria: Vec::new(),
            blocked_by: Vec::new(),
            blocked: r.get("blocked")?,
            doing: None,
            done_by: r.get("done_by")?,
            repo: r.get::<_, Option<i64>>("repo_id")?.map(RepoId),
        },
    })
}

/// Each task's doing post (see `TaskDetails::doing`), as (task id, message), for `task`
/// alone or for every task with `None`: its latest progress post in a thread linked to it.
fn doing_posts(conn: &Connection, task: Option<TaskId>) -> rusqlite::Result<Vec<(i64, Message)>> {
    conn.prepare(
        "SELECT task_id, id, author, text, posted_at FROM ( \
           SELECT t.task_id, m.id, m.author, m.text, m.posted_at, ROW_NUMBER() OVER ( \
             PARTITION BY t.task_id ORDER BY m.posted_at DESC, m.id DESC) AS back \
           FROM message m JOIN thread t ON t.id = m.thread_id \
           WHERE m.progress AND t.task_id IS NOT NULL AND (?1 IS NULL OR t.task_id = ?1)) \
         WHERE back = 1",
    )?
    .query_map(params![task.map(|t| t.0)], |r| {
        Ok((
            r.get(0)?,
            Message {
                id: r.get(1)?,
                author: r.get(2)?,
                text: r.get(3)?,
                at: r.get(4)?,
            },
        ))
    })?
    .collect()
}

/// The blocked flag of `task`, refusing a task not on the board and a subtask: blocked is a
/// flag on tasks. Outer and inner results as in `refuse_missing`.
fn blocked_flag(conn: &Connection, task: TaskId) -> rusqlite::Result<Result<Option<String>>> {
    let found: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT parent_id, blocked FROM task WHERE id = ?1",
            params![task.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match found {
        None => Err(anyhow::anyhow!("there is no task {task} on the board")),
        Some((Some(parent), _)) => Err(anyhow::anyhow!(
            "{task} is a subtask of {}; blocked is a flag on tasks, so block {} instead",
            TaskId(parent),
            TaskId(parent)
        )),
        Some((None, blocked)) => Ok(blocked),
    })
}

/// The criteria of `task`, in the order they were added.
fn criteria_of(conn: &Connection, task: TaskId) -> rusqlite::Result<Vec<Criterion>> {
    conn.prepare("SELECT text, done FROM criterion WHERE task_id = ?1 ORDER BY id")?
        .query_map(params![task.0], |r| {
            Ok(Criterion {
                text: r.get(0)?,
                done: r.get(1)?,
            })
        })?
        .collect()
}

/// The tasks `task` is blocked by, in id order.
fn blockers_of(conn: &Connection, task: TaskId) -> rusqlite::Result<Vec<TaskId>> {
    conn.prepare("SELECT blocker_id FROM blocker WHERE task_id = ?1 ORDER BY blocker_id")?
        .query_map(params![task.0], |r| Ok(TaskId(r.get(0)?)))?
        .collect()
}

/// Refuses anything but a task on the board: details belong to tasks, not to subtasks.
/// Outer and inner results as in `refuse_missing`.
fn refuse_not_a_task(conn: &Connection, task: TaskId) -> rusqlite::Result<Result<()>> {
    let found: Option<Option<i64>> = conn
        .query_row(
            "SELECT parent_id FROM task WHERE id = ?1",
            params![task.0],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match found {
        None => Err(anyhow::anyhow!("there is no task {task} on the board")),
        Some(Some(parent)) => Err(anyhow::anyhow!(
            "{task} is a subtask of {}; details belong to tasks",
            TaskId(parent)
        )),
        Some(None) => Ok(()),
    })
}

/// What `Store::set_task` changes: each field given replaces the stored one.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskFields<'a> {
    pub description: Option<&'a str>,
    pub context: Option<&'a str>,
    pub link: Option<&'a str>,
    pub mr: Option<&'a str>,
    pub assignee: Option<&'a str>,
    /// For a standalone task: the repo it belongs to.
    pub repo: Option<RepoId>,
}

/// What `Store::set_milestone` changes: each field given replaces the stored one.
#[derive(Debug, Clone, Copy, Default)]
pub struct MilestoneFields<'a> {
    pub title: Option<&'a str>,
    pub goal: Option<&'a str>,
    pub context: Option<&'a str>,
    pub spec: Option<&'a str>,
    pub link: Option<&'a str>,
    /// The repo it moves to, taking the threads about it and its tasks along.
    pub repo: Option<RepoId>,
}

/// Where `Store::set_milestone` put a milestone, or `Store::set_task` a standalone task,
/// given a repo: the repo, how many threads (about it or, for a milestone, its tasks) went
/// with it, and its lead there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub repo: RepoId,
    pub threads: usize,
    pub lead: Option<Lead>,
}

/// What `Store::remove_repo` removed with a repo: its lead, if it had one, its threads, and
/// their messages, of which `inbox` were items the owner's inbox listed (need you and
/// unread awareness; read ones are not listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub lead: Option<Lead>,
    pub threads: usize,
    pub messages: usize,
    pub inbox: usize,
}

/// What `Store::remove_task` removed with a standalone task: its title, and how many
/// subtasks, criteria (its own and its subtasks'), threads, messages and items the owner's
/// inbox listed went with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRemoved {
    pub title: String,
    pub subtasks: usize,
    pub criteria: usize,
    pub threads: usize,
    pub messages: usize,
    pub inbox: usize,
}

/// A repo as `tower repo show` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDetails {
    pub id: RepoId,
    /// Its main checkout; `None` on a repo the schema 14 migration made, until it is set.
    pub path: Option<String>,
    /// Its forge link.
    pub link: Option<String>,
    pub lead: Option<Lead>,
    pub milestones: Vec<(MilestoneId, String)>,
    /// Each standalone task: its id, title and tracker link.
    pub standalone: Vec<(TaskId, String, Option<String>)>,
    /// Its whole board, in thread id order, each thread with its latest messages.
    pub threads: Vec<Thread>,
}

/// The main checkout at `path`, as stored: absolute, a directory whose `.git` is a
/// directory (a linked worktree's `.git` is a file), with links resolved, so one checkout
/// is one path.
fn check_checkout(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("the repo path {} is not absolute", path.display());
    }
    let git = path.join(".git");
    if git.is_file() {
        bail!(
            "{} is a linked worktree; give the repo's main checkout",
            path.display()
        );
    }
    if !git.is_dir() {
        bail!(
            "{} is not a git main checkout: it has no .git directory",
            path.display()
        );
    }
    let real = path
        .canonicalize()
        .with_context(|| format!("cannot resolve the repo path {}", path.display()))?;
    let real = real
        .to_str()
        .with_context(|| format!("the repo path {} is not UTF-8", real.display()))?;
    check_line("repo path", real)
}

/// Refuses a path another repo has. Outer and inner results as in `refuse_missing`.
fn refuse_path_taken(conn: &Connection, path: &str) -> rusqlite::Result<Result<()>> {
    let holder: Option<i64> = conn
        .query_row("SELECT id FROM repo WHERE path = ?1", params![path], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match holder {
        Some(r) => Err(anyhow::anyhow!("{path} is already {}", RepoId(r))),
        None => Ok(()),
    })
}

fn subtask_from_row(r: &rusqlite::Row) -> rusqlite::Result<Subtask> {
    Ok(Subtask {
        id: TaskId(r.get("id")?),
        title: r.get("title")?,
        stage: r.get("stage")?,
    })
}

/// Where `load` put task `id`, as `at` records it: (milestone index, or `None` for a
/// standalone task, task index).
fn task_at<'a>(
    milestones: &'a mut [Milestone],
    standalone: &'a mut [Task],
    at: &HashMap<i64, (Option<usize>, usize)>,
    id: i64,
) -> Option<&'a mut Task> {
    match *at.get(&id)? {
        (Some(i), j) => milestones[i].tasks.get_mut(j),
        (None, j) => standalone.get_mut(j),
    }
}

/// The tasks of `milestone` in id order, without subtasks.
fn tasks_of(conn: &Connection, milestone: MilestoneId) -> rusqlite::Result<Vec<Task>> {
    conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM task WHERE milestone_id = ?1 AND parent_id IS NULL \
         ORDER BY id"
    ))?
    .query_map(params![milestone.0], task_from_row)?
    .collect()
}

/// A subtask is open under `parent` (new, or moved out of Done): a Done parent goes back to
/// In progress, which changes its milestone's task stages, so that milestone's Done mark goes
/// too, per the rule at MIGRATIONS[2].
fn reopen_parent(conn: &Connection, parent: TaskId) -> rusqlite::Result<()> {
    let reopened = conn.execute(
        "UPDATE task SET stage = 'in-progress', done_by = NULL WHERE id = ?1 AND stage = 'done'",
        params![parent.0],
    )?;
    if reopened > 0 {
        conn.execute(
            "UPDATE milestone SET done = 0 \
             WHERE id = (SELECT milestone_id FROM task WHERE id = ?1)",
            params![parent.0],
        )?;
    }
    Ok(())
}

/// The subtasks of `task` in id order.
fn subtasks_of(conn: &Connection, task: TaskId) -> rusqlite::Result<Vec<Subtask>> {
    conn.prepare("SELECT id, title, stage FROM task WHERE parent_id = ?1 ORDER BY id")?
        .query_map(params![task.0], subtask_from_row)?
        .collect()
}

/// A lead as stored. A colour this build does not know (a hand-edited file, or a palette
/// change) loads as `None` rather than failing the whole board.
fn lead_from_row(r: &rusqlite::Row) -> rusqlite::Result<Lead> {
    Ok(Lead {
        name: r.get("name")?,
        session: r.get("session")?,
        colour: r.get::<_, String>("colour")?.parse().ok(),
    })
}

/// Registers `name` with `session` as `repo`'s lead in `tx`, in `colour` or the next free
/// one, after the caller's checks (no lead yet, name and session free).
fn insert_lead(
    tx: &Connection,
    repo: RepoId,
    name: String,
    session: String,
    colour: Option<Colour>,
) -> rusqlite::Result<Lead> {
    let colour = match colour {
        Some(c) => c,
        None => {
            let taken: Vec<Colour> = tx
                .prepare("SELECT colour FROM lead")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .filter_map(|c| c.parse().ok())
                .collect();
            Colour::next(&taken)
        }
    };
    tx.execute(
        // A new lead is told of what is posted from now on: it reads the board at start.
        "INSERT INTO lead (repo_id, name, session, colour, told_upto) \
         VALUES (?1, ?2, ?3, ?4, (SELECT coalesce(max(id), 0) FROM message))",
        params![repo.0, name, session, colour.name()],
    )?;
    Ok(Lead {
        name,
        session,
        colour: Some(colour),
    })
}

/// The lead of `milestone`: its repo's, if it has a repo with a lead.
fn lead_of(conn: &Connection, milestone: MilestoneId) -> rusqlite::Result<Option<Lead>> {
    conn.query_row(
        "SELECT l.name, l.session, l.colour FROM lead l JOIN milestone m ON m.repo_id = l.repo_id \
         WHERE m.id = ?1",
        params![milestone.0],
        lead_from_row,
    )
    .optional()
}

/// The lead of `repo`, if it has one.
fn lead_of_repo(conn: &Connection, repo: RepoId) -> rusqlite::Result<Option<Lead>> {
    conn.query_row(
        "SELECT name, session, colour FROM lead WHERE repo_id = ?1",
        params![repo.0],
        lead_from_row,
    )
    .optional()
}

/// Refuses a repo that is not on the board. Outer and inner results as in `refuse_missing`.
fn refuse_missing_repo(conn: &Connection, repo: RepoId) -> rusqlite::Result<Result<()>> {
    let exists = conn
        .query_row("SELECT 1 FROM repo WHERE id = ?1", params![repo.0], |_| {
            Ok(())
        })
        .optional()?;
    Ok(match exists {
        Some(()) => Ok(()),
        None => Err(anyhow::anyhow!("there is no repo {repo} on the board")),
    })
}

/// The repo `of` names: the repo itself, or the repo a milestone belongs to. Refuses one
/// not on the board and a milestone with no repo yet. Outer and inner results as in
/// `refuse_missing`.
fn repo_of(conn: &Connection, of: RepoOf) -> rusqlite::Result<Result<RepoId>> {
    match of {
        RepoOf::Repo(r) => Ok(refuse_missing_repo(conn, r)?.map(|()| r)),
        RepoOf::Milestone(m) => {
            if let Err(e) = refuse_missing(conn, m)? {
                return Ok(Err(e));
            }
            let repo: Option<i64> = conn.query_row(
                "SELECT repo_id FROM milestone WHERE id = ?1",
                params![m.0],
                |r| r.get(0),
            )?;
            Ok(repo.map(RepoId).ok_or_else(|| {
                anyhow::anyhow!(
                    "{m} is in no repo yet; set its repo first: tower milestone set {m} --repo R<n>"
                )
            }))
        }
    }
}

/// Every lead's board, in repo order.
fn lead_boards(conn: &Connection) -> rusqlite::Result<Vec<LeadBoard>> {
    conn.prepare("SELECT repo_id, name, session, colour FROM lead ORDER BY repo_id")?
        .query_map([], |r| {
            Ok(LeadBoard {
                repo: RepoId(r.get(0)?),
                lead: lead_from_row(r)?,
            })
        })?
        .collect()
}

/// The posts on `repo`'s board its lead has not been told of: after its cursor
/// (`told_upto`), not written by the lead (its name or session, ignoring ASCII case), and
/// not the watch's own posts in a "Lead not listening" thread, which are about the lead
/// for the owner, in id order. None for a repo with no lead.
fn unread(conn: &Connection, repo: RepoId) -> rusqlite::Result<Vec<Post>> {
    conn.prepare(
        "SELECT m.id, m.thread_id, b.repo_id, t.task_id, m.author, m.text, m.posted_at \
         FROM lead l JOIN thread_board b ON b.repo_id = l.repo_id \
         JOIN thread t ON t.id = b.id JOIN message m ON m.thread_id = t.id \
         WHERE l.repo_id = ?1 AND m.id > l.told_upto \
         AND lower(m.author) NOT IN (lower(l.name), lower(l.session)) \
         AND NOT (m.author = ?2 AND t.repo_id IS NOT NULL AND t.title = ?3) ORDER BY m.id",
    )?
    .query_map(params![repo.0, SYSTEM_AUTHOR, NOT_LISTENING], |r| {
        Ok(Post {
            id: r.get(0)?,
            thread: ThreadId(r.get(1)?),
            repo: board_repo(r, 2, r.get(1)?)?,
            task: r.get::<_, Option<i64>>(3)?.map(TaskId),
            author: r.get(4)?,
            text: r.get(5)?,
            posted_at: r.get(6)?,
        })
    })?
    .collect()
}

/// The title of the thread the watch opens when a lead is not listening.
pub const NOT_LISTENING: &str = "Lead not listening";

/// A registered lead: its repo, itself, the Claude session it attached (`None` while it has
/// not, or since `tower lead set --session` gave it a new one), the thread of its open
/// "lead not listening" episode, if any, how many turns its session has ended with no
/// waiter running, and that count when the watch last saw its waiter or first saw it quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub repo: RepoId,
    pub lead: Lead,
    pub claude_session: Option<String>,
    pub deaf: Option<ThreadId>,
    /// How many turns its attached session has ended with no waiter running, by its Stop
    /// hook (`lead_turn_ended`).
    pub turns: i64,
    /// `turns` when the watch last saw its waiter running, or a waiter last took its lock,
    /// or the watch first saw it, idle or busy, with none; `None` while none of these has
    /// happened since the session attached.
    pub quiet_turns: Option<i64>,
}

/// How many messages in the threads `threads` selects (a query on `?2`, given `of`) the
/// owner's inbox lists, by its own rule (`inbox::listed`), not every message
/// `MENTIONS_WHERE` narrows to: what a removal says it takes from the inbox.
fn inbox_items(conn: &Connection, threads: &str, of: i64) -> rusqlite::Result<usize> {
    let mentions: Vec<(Message, bool, bool)> = conn
        .prepare(&format!(
            "SELECT m.id, m.author, m.text, m.posted_at, m.read_at IS NOT NULL, \
             t.resolved_at IS NULL FROM message m {MENTIONS_WHERE} \
             AND m.thread_id IN ({threads})"
        ))?
        .query_map(params![OWNER_HANDLE, of], |r| {
            Ok((
                Message {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    text: r.get(2)?,
                    at: r.get(3)?,
                },
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((mentions.iter())
        .filter(|(m, read, open)| crate::inbox::listed(m, *read, *open))
        .count())
}

/// Refuses a name or session (each ignoring ASCII case, by the columns' collation) that
/// already belongs to the lead of a repo other than `repo` (of any repo, with `None`): a
/// lead has one repo. Refuses too a session that is any task's assignee (ignoring ASCII
/// case, as `set_task_details` does the other way round), Done or not, naming the tasks: a
/// lead is never a task's assignee. Outer and inner results as in
/// `refuse_missing`.
fn refuse_taken(
    conn: &Connection,
    repo: Option<RepoId>,
    name: Option<&str>,
    session: Option<&str>,
) -> rusqlite::Result<Result<()>> {
    let holder = |column: &str, value: &str| {
        conn.query_row(
            &format!("SELECT repo_id, name FROM lead WHERE {column} = ?1 AND repo_id IS NOT ?2"),
            params![value, repo.map(|r| r.0)],
            |r| Ok((RepoId(r.get(0)?), r.get::<_, String>(1)?)),
        )
        .optional()
    };
    if let Some(name) = name
        && let Some((r, _)) = holder("name", name)?
    {
        return Ok(Err(anyhow::anyhow!(
            "{name} already leads {r}; a lead has one repo"
        )));
    }
    if let Some(session) = session
        && let Some((r, current)) = holder("session", session)?
    {
        return Ok(Err(anyhow::anyhow!(
            "the session {session} is already {current}, the lead of {r}; \
             a lead has one repo"
        )));
    }
    if let Some(session) = session {
        let tasks: Vec<TaskId> = conn
            .prepare("SELECT id FROM task WHERE assignee = ?1 COLLATE NOCASE ORDER BY id")?
            .query_map(params![session], |r| Ok(TaskId(r.get(0)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if !tasks.is_empty() {
            let (named, unassign): (Vec<String>, Vec<String>) = (tasks.iter())
                .map(|t| (t.to_string(), format!("tower task unassign {}", t.0)))
                .unzip();
            let (of, it) = match tasks.len() {
                1 => ("task", "it"),
                _ => ("tasks", "them"),
            };
            return Ok(Err(anyhow::anyhow!(
                "the session {session} is the assignee of {of} {}, and a lead is never a \
                 task's assignee: unassign {it} first with {}",
                named.join(", "),
                unassign.join(" and ")
            )));
        }
    }
    Ok(Ok(()))
}

/// Adds the milestone `title` with `goal` in `tx`, in `repo` if given (else in no repo yet,
/// until `set_milestone` gives it one), and returns its id. Checks both as one line each,
/// then refuses a repo not on the board. Every milestone insert goes through it
/// (`add_milestone` and the dashboard's starts).
fn insert_milestone_in(
    tx: &Connection,
    title: &str,
    goal: &str,
    repo: Option<RepoId>,
    cannot_write: &str,
) -> Result<MilestoneId> {
    let title = check_line("milestone title", title)?;
    let goal = check_line("milestone goal", goal)?;
    if let Some(repo) = repo {
        refuse_missing_repo(tx, repo).with_context(|| cannot_write.to_string())??;
    }
    tx.execute(
        "INSERT INTO milestone (title, goal, repo_id) VALUES (?1, ?2, ?3)",
        params![title, goal, repo.map(|r| r.0)],
    )
    .with_context(|| cannot_write.to_string())?;
    Ok(MilestoneId(tx.last_insert_rowid()))
}

/// The name, used as both name and session, of a lead the dashboard starts for milestone
/// `m` on `repo` (in no repo yet, with `None`): `name`, or while `refuse_taken` refuses it
/// (another lead has it, or a task's assignee is it), `name` then "-" and `m` in lowercase
/// (`checkpoint-lead-m4`), refused in turn if that is taken too. Outer and inner results as
/// in `refuse_missing`.
fn free_lead_name(
    conn: &Connection,
    repo: Option<RepoId>,
    name: &str,
    m: MilestoneId,
) -> rusqlite::Result<Result<String>> {
    let name = match check_session(name) {
        Ok(name) => name,
        Err(e) => return Ok(Err(e)),
    };
    if refuse_taken(conn, repo, Some(&name), Some(&name))?.is_ok() {
        return Ok(Ok(name));
    }
    let name = match check_session(&format!("{name}-{}", m.to_string().to_ascii_lowercase())) {
        Ok(name) => name,
        Err(e) => return Ok(Err(e)),
    };
    Ok(refuse_taken(conn, repo, Some(&name), Some(&name))?.map(|()| name))
}

/// The repo whose board thread `thread` is on, from `thread_board`'s `repo_id` in column
/// `at` of `r`, refusing none with a message naming the thread: a thread whose milestone or
/// task is in no repo (only a board migrated before the check in `run_migrations` could
/// have one) instead of a column type error.
fn board_repo(r: &rusqlite::Row, at: usize, thread: i64) -> rusqlite::Result<RepoId> {
    match r.get::<_, Option<i64>>(at)? {
        Some(repo) => Ok(RepoId(repo)),
        None => Err(rusqlite::Error::FromSqlConversionFailure(
            at,
            rusqlite::types::Type::Null,
            format!(
                "{} is on no repo's board: it links to a task or milestone in no repo; give \
                 that milestone or task a repo with --repo",
                ThreadId(thread)
            )
            .into(),
        )),
    }
}

/// A repo with its lead, milestones, standalone tasks and threads, read on `conn` (inside
/// the caller's transaction), or `None` when it is not on the board.
fn repo_details(conn: &Connection, repo: RepoId) -> rusqlite::Result<Option<RepoDetails>> {
    let found: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT path, link FROM repo WHERE id = ?1",
            params![repo.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((path, link)) = found else {
        return Ok(None);
    };
    let lead = lead_of_repo(conn, repo)?;
    let milestones = conn
        .prepare("SELECT id, title FROM milestone WHERE repo_id = ?1 ORDER BY id")?
        .query_map(params![repo.0], |r| Ok((MilestoneId(r.get(0)?), r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let standalone = conn
        .prepare("SELECT id, title, link FROM task WHERE repo_id = ?1 ORDER BY id")?
        .query_map(params![repo.0], |r| {
            Ok((TaskId(r.get(0)?), r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let threads = threads_where(
        conn,
        "t.id IN (SELECT id FROM thread_board WHERE repo_id = ?1)",
        repo.0,
        true,
    )?
    .into_iter()
    .map(|(_, t)| t)
    .collect();
    Ok(Some(RepoDetails {
        id: repo,
        path,
        link,
        lead,
        milestones,
        standalone,
        threads,
    }))
}

/// The threads matching `filter` (SQL over `thread t`, with one parameter, `?1`), in id
/// order, each with the repo whose board it is on, who started it and its
/// message count. With `summary`, each carries only the latest messages the board summary
/// shows (`ThreadStatus::shown`), so a long thread costs no more to load than a short one;
/// without, its whole history. Messages are in the order posted, by time and then id.
fn threads_where(
    conn: &Connection,
    filter: &str,
    param: i64,
    summary: bool,
) -> rusqlite::Result<Vec<(RepoId, Thread)>> {
    let mut threads: Vec<(RepoId, Thread)> = conn
        .prepare(&format!(
            "SELECT b.repo_id, t.id, t.title, t.task_id, t.resolved_by, t.resolved_at, \
             (SELECT author FROM message WHERE thread_id = t.id \
              ORDER BY posted_at, id LIMIT 1), \
             (SELECT count(*) FROM message WHERE thread_id = t.id), t.milestone_id \
             FROM thread t JOIN thread_board b ON b.id = t.id WHERE {filter} ORDER BY t.id"
        ))?
        .query_map(params![param], |r| {
            let resolved = match (r.get::<_, Option<String>>(4)?, r.get(5)?) {
                (Some(by), Some(at)) => Some(Resolution { by, at }),
                _ => None,
            };
            Ok((
                board_repo(r, 0, r.get(1)?)?,
                Thread {
                    id: ThreadId(r.get(1)?),
                    title: r.get(2)?,
                    task: r.get::<_, Option<i64>>(3)?.map(TaskId),
                    milestone: r.get::<_, Option<i64>>(8)?.map(MilestoneId),
                    resolved,
                    by: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    total: r.get::<_, i64>(7)? as usize,
                    authors: Vec::new(),
                    messages: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // Everyone who posted in each thread, from its whole history, in the order they first
    // posted, so a loaded board's team does not depend on which messages it carries.
    let mut stmt = conn.prepare(&format!(
        "SELECT m.thread_id, m.author FROM message m JOIN thread t ON t.id = m.thread_id \
         WHERE {filter} GROUP BY m.thread_id, m.author \
         ORDER BY m.thread_id, min(m.posted_at), min(m.id)"
    ))?;
    let mut rows = stmt.query(params![param])?;
    while let Some(r) = rows.next()? {
        let thread: i64 = r.get(0)?;
        if let Ok(i) = threads.binary_search_by_key(&thread, |(_, t)| t.id.0) {
            threads[i].1.authors.push(r.get(1)?);
        }
    }
    // `back` counts each thread's messages from its latest, 1 first.
    let mut stmt = conn.prepare(&format!(
        "SELECT thread_id, id, author, text, posted_at FROM ( \
           SELECT m.*, t.resolved_at IS NULL AS open, ROW_NUMBER() OVER ( \
             PARTITION BY m.thread_id ORDER BY m.posted_at DESC, m.id DESC) AS back \
           FROM message m JOIN thread t ON t.id = m.thread_id WHERE {filter}) \
         WHERE NOT ?2 OR back <= CASE WHEN open THEN ?3 ELSE ?4 END \
         ORDER BY thread_id, posted_at, id"
    ))?;
    let mut rows = stmt.query(params![
        param,
        summary,
        ThreadStatus::Open.shown() as i64,
        ThreadStatus::Resolved.shown() as i64
    ])?;
    while let Some(r) = rows.next()? {
        let thread: i64 = r.get(0)?;
        // Threads are sorted by id, and every message's thread matched the same filter.
        if let Ok(i) = threads.binary_search_by_key(&thread, |(_, t)| t.id.0) {
            threads[i].1.messages.push(Message {
                id: r.get(1)?,
                author: r.get(2)?,
                text: r.get(3)?,
                at: r.get(4)?,
            });
        }
    }
    Ok(threads)
}

/// What a watch step (`Store::watch_step`) changes in the flags the watch remembers.
#[derive(Debug, Clone, Copy)]
pub enum FlagChange<'a> {
    None,
    /// Remember a flag, replacing one of the same task, signal and worker.
    Raise(&'a Flag),
    /// An unanswered nudge to the worker was passed to the lead, at the step's time.
    LeadTold(Signal, &'a str),
    /// Forget the worker's flag whose episode ended.
    Clear(Signal, &'a str),
}
/// Posts the delivery item `text` as `system` in `thread` at `at`, or, when that thread is
/// resolved, in a new "Delivery failed" thread on the same board linked to the same task.
/// Returns the thread posted in.
fn failure_item(
    tx: &Connection,
    thread: ThreadId,
    text: &str,
    at: i64,
    cannot_write: &str,
) -> Result<ThreadId> {
    let (repo, milestone, task, resolved): (RepoId, Option<i64>, Option<i64>, bool) = tx
        .query_row(
            "SELECT b.repo_id, t.milestone_id, t.task_id, t.resolved_at IS NOT NULL \
             FROM thread t JOIN thread_board b ON b.id = t.id WHERE t.id = ?1",
            params![thread.0],
            |r| Ok((board_repo(r, 0, thread.0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .context(cannot_write.to_string())?
        .with_context(|| format!("there is no thread {thread} on the board"))?;
    let thread = if resolved {
        let task = task.map(TaskId);
        new_thread(tx, repo, milestone, task, "Delivery failed", cannot_write)?
    } else {
        thread
    };
    append(
        tx,
        thread,
        SYSTEM_AUTHOR,
        text,
        Kind::Plain,
        at,
        cannot_write,
    )?;
    Ok(thread)
}

/// Opens a thread titled `title` on `repo`'s board, about `milestone` or linked to `task`
/// (not both) or about the whole repo, with no message yet: the checks `tower thread new`
/// makes, for it, the watch and delivery. Refuses a repo with no lead (it has no board
/// yet) and a task not in the repo.
fn new_thread(
    tx: &Connection,
    repo: RepoId,
    milestone: Option<i64>,
    task: Option<TaskId>,
    title: &str,
    cannot_write: &str,
) -> Result<ThreadId> {
    let title = check_line("thread title", title)?;
    if lead_of_repo(tx, repo)
        .context(cannot_write.to_string())?
        .is_none()
    {
        bail!(
            "{repo} has no lead yet, so it has no message board; add one with tower lead add {repo}"
        );
    }
    if let Some(task) = task {
        refuse_foreign_task(tx, repo, task).context(cannot_write.to_string())??;
    }
    // Only a thread about the whole repo stores the repo: the others reach it through
    // their milestone or task (`thread_board`).
    let milestone = milestone.filter(|_| task.is_none());
    let repo_wide = (milestone.is_none() && task.is_none()).then_some(repo.0);
    tx.execute(
        "INSERT INTO thread (repo_id, milestone_id, task_id, title) VALUES (?1, ?2, ?3, ?4)",
        params![repo_wide, milestone, task.map(|t| t.0), title],
    )
    .context(cannot_write.to_string())?;
    Ok(ThreadId(tx.last_insert_rowid()))
}

/// The thread tower posts about `task` in: its open linked thread with the newest message,
/// else a new one titled `title` on its lead's board linked to it, opened by `new_thread`.
/// Refuses a task with no lead's board (a subtask, a task in no repo, a repo with no lead).
fn task_thread(tx: &Connection, task: TaskId, title: &str, cannot_write: &str) -> Result<ThreadId> {
    let open: Option<i64> = tx
        .query_row(
            "SELECT t.id FROM thread t JOIN message m ON m.thread_id = t.id \
             WHERE t.task_id = ?1 AND t.resolved_at IS NULL \
             GROUP BY t.id ORDER BY max(m.posted_at) DESC, t.id DESC LIMIT 1",
            params![task.0],
            |r| r.get(0),
        )
        .optional()
        .context(cannot_write.to_string())?;
    if let Some(id) = open {
        return Ok(ThreadId(id));
    }
    let repo: Option<Option<i64>> = tx
        .query_row(TASK_REPO, params![task.0], |r| r.get(0))
        .optional()
        .context(cannot_write.to_string())?;
    let Some(Some(repo)) = repo else {
        bail!("{task} is on no lead's board, so there is nowhere to post about it");
    };
    new_thread(tx, RepoId(repo), None, Some(task), title, cannot_write)
}

/// Posts by `system` that `blocker` is done, in the thread (`task_thread`) of each task it
/// blocks in another repo, not Done, whose repo has a lead: that lead hears only its own
/// board, so this is what wakes it (#144). A task it blocks in its own repo is left to its
/// own lead, who sees it done.
fn tell_blocked_boards(tx: &Connection, blocker: TaskId, cannot_write: &str) -> Result<()> {
    let repo_of_task = |task: i64| -> rusqlite::Result<Option<i64>> {
        let repo: Option<Option<i64>> = tx
            .query_row(TASK_REPO, params![task], |r| r.get(0))
            .optional()?;
        Ok(repo.flatten())
    };
    let from = repo_of_task(blocker.0).context(cannot_write.to_string())?;
    let blocked: Vec<i64> = tx
        .prepare(
            "SELECT b.task_id FROM blocker b JOIN task t ON t.id = b.task_id \
             WHERE b.blocker_id = ?1 AND t.stage != ?2 ORDER BY b.task_id",
        )
        .and_then(|mut s| {
            s.query_map(params![blocker.0, Stage::Done.name()], |r| r.get(0))?
                .collect()
        })
        .context(cannot_write.to_string())?;
    let at = now()?;
    for task in blocked {
        let Some(repo) = repo_of_task(task).context(cannot_write.to_string())? else {
            continue;
        };
        let has_lead = lead_of_repo(tx, RepoId(repo))
            .context(cannot_write.to_string())?
            .is_some();
        if Some(repo) == from || !has_lead {
            continue;
        }
        let task = TaskId(task);
        let title = format!("Blocked by {blocker}");
        let thread = task_thread(tx, task, &title, cannot_write)?;
        // `blocker` is Done already in this transaction, so these are the others.
        let open: Vec<String> = tx
            .prepare(
                "SELECT b.blocker_id FROM blocker b JOIN task t ON t.id = b.blocker_id \
                 WHERE b.task_id = ?1 AND t.stage != ?2 ORDER BY b.blocker_id",
            )
            .and_then(|mut s| {
                s.query_map(params![task.0, Stage::Done.name()], |r| {
                    Ok(TaskId(r.get(0)?).to_string())
                })?
                .collect()
            })
            .context(cannot_write.to_string())?;
        let done = match from {
            Some(from) => format!("{blocker} in {} is done", RepoId(from)),
            None => format!("{blocker} is done"),
        };
        let text = if open.is_empty() {
            format!("{done}; {task}, which it blocked, can go on")
        } else {
            format!(
                "{done}; {task}, which it blocked, is still blocked by {}",
                open.join(", ")
            )
        };
        append(
            tx,
            thread,
            SYSTEM_AUTHOR,
            &text,
            Kind::Plain,
            at,
            cannot_write,
        )?;
    }
    Ok(())
}

/// Adds a checked message by a checked author to an open thread at `at`, or after its
/// latest message, resolving the thread by it when it is evidence. Refuses a thread that
/// is not on the board, a resolved one, and a progress post in a thread linked to no task.
fn append(
    tx: &Connection,
    thread: ThreadId,
    author: &str,
    text: &str,
    kind: Kind,
    at: i64,
    cannot_write: &str,
) -> Result<()> {
    let found: Option<(bool, bool)> = tx
        .query_row(
            "SELECT resolved_at IS NOT NULL, task_id IS NOT NULL FROM thread WHERE id = ?1",
            params![thread.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context(cannot_write.to_string())?;
    match found {
        None => bail!("there is no thread {thread} on the board"),
        Some((true, _)) => bail!("{thread} is resolved; resolved threads are read-only"),
        Some((false, false)) if kind == Kind::Progress => bail!(
            "{thread} is linked to no task, so a progress post there would be no task's \
             doing line; post it in the task's thread"
        ),
        Some(_) => {}
    }
    // Never stamped before the thread's latest message (a clock stepped back), so the
    // order by time is the order posted and a resolved thread's evidence stays last.
    let latest: Option<i64> = tx
        .query_row(
            "SELECT max(posted_at) FROM message WHERE thread_id = ?1",
            params![thread.0],
            |r| r.get(0),
        )
        .context(cannot_write.to_string())?;
    let at = latest.map_or(at, |l| at.max(l));
    tx.execute(
        "INSERT INTO message (thread_id, author, text, posted_at, progress) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![thread.0, author, text, at, kind == Kind::Progress],
    )
    .context(cannot_write.to_string())?;
    if kind == Kind::Evidence {
        tx.execute(
            "UPDATE thread SET resolved_by = ?1, resolved_at = ?2 WHERE id = ?3",
            params![author, at, thread.0],
        )
        .context(cannot_write.to_string())?;
    }
    Ok(())
}

impl Store {
    /// Opens the board at `path`, creating it (and its directory) on first use and bringing
    /// its schema up to date. Refuses a board written by a newer `tower`, and a tower that
    /// is not the brew install creating or upgrading the default board.
    pub fn open(path: &Path) -> Result<Store> {
        let at = || format!("board {}", path.display());
        let guarded = is_default_board(path) && !crate::register::installed();
        if guarded && !path.exists() {
            return Err(refuse_dev_upgrade(path, None));
        }
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create the directory {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("cannot open the {}", at()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // Refuse a newer board, or a dev build's upgrade, before anything below changes the file.
        let found = read_version(&conn).with_context(at)?;
        if guarded && found < SCHEMA_VERSION {
            return Err(refuse_dev_upgrade(path, Some(found)));
        }
        use_wal(&conn).with_context(|| format!("cannot switch the {} to WAL", at()))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut store = Store {
            conn,
            path: path.to_path_buf(),
        };
        store.migrate().with_context(at)?;
        Ok(store)
    }

    /// Opens the board for reading only, as the dashboard does: never creates, migrates or
    /// changes the file. Refuses a board this build cannot read, and says so when there is
    /// no board yet (no file, or a file no `tower` has set up).
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let at = || format!("board {}", path.display());
        let no_board = || {
            anyhow::anyhow!(
                "no board at {} yet; add a milestone with tower milestone add",
                path.display()
            )
        };
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(no_board()),
            Err(e) => return Err(e).with_context(|| format!("cannot read the {}", at())),
            Ok(_) => {}
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)
            .with_context(|| format!("cannot open the {}", at()))?;
        conn.busy_timeout(READ_BUSY_TIMEOUT)?;
        match read_version(&conn).with_context(at)? {
            0 => return Err(no_board()),
            v if v < SCHEMA_VERSION => bail!(
                "board {} is schema version {v}; run tower upgrade",
                path.display()
            ),
            _ => {}
        }
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    fn cannot_write(&self) -> String {
        format!("cannot write to the board {}", self.path.display())
    }

    fn migrate(&mut self) -> Result<()> {
        if read_version(&self.conn)? == SCHEMA_VERSION {
            return Ok(());
        }
        // A migration that rebuilds a table drops one other tables refer to, which SQLite
        // allows only with foreign keys off; they can change only outside a transaction,
        // so they are off for the migration, checked before it commits, and on again after.
        self.conn.pragma_update(None, "foreign_keys", false)?;
        let migrated = self.run_migrations();
        self.conn.pragma_update(None, "foreign_keys", true)?;
        migrated
    }

    fn run_migrations(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Read again under the write lock: another process may have migrated meanwhile.
        let found = read_version(&tx)?;
        for (from, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
            tx.execute_batch(sql)
                .with_context(|| format!("migration from schema version {from} failed"))?;
        }
        // Every thread is on a repo's board (MIGRATIONS[13]).
        let lost: Option<String> = tx.query_row(
            "SELECT group_concat('T' || id, ', ') FROM thread_board WHERE repo_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        if let Some(lost) = lost {
            bail!(
                "migration to schema version {SCHEMA_VERSION} left threads on no repo's board: \
                 {lost}; a thread links to a task or milestone in no repo"
            );
        }
        let broken: Option<String> = tx
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()?;
        if let Some(table) = broken {
            bail!(
                "migration to schema version {SCHEMA_VERSION} left a broken reference in {table}"
            );
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    /// Adds the repo whose main checkout is at `path`, with its forge `link` if given.
    /// Refuses a path that is not a main checkout (see `check_checkout`) or is already a
    /// repo, and a link that is not an http or https URL.
    pub fn add_repo(&mut self, path: &Path, link: Option<&str>) -> Result<RepoId> {
        let path = check_checkout(path)?;
        let link = link.map(|l| check_link("forge link", l)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_path_taken(&tx, &path).context(cannot_write.clone())??;
        tx.execute(
            "INSERT INTO repo (path, link) VALUES (?1, ?2)",
            params![path, link],
        )
        .context(cannot_write.clone())?;
        let id = RepoId(tx.last_insert_rowid());
        tx.commit().context(cannot_write)?;
        Ok(id)
    }

    /// Sets a repo's main checkout and forge link, whichever are given, by the rules of
    /// `add_repo`: for a repo the migration made, whose path is not known. Refuses a repo
    /// not on the board and a call that changes nothing.
    pub fn set_repo(
        &mut self,
        repo: RepoId,
        path: Option<&Path>,
        link: Option<&str>,
    ) -> Result<()> {
        if path.is_none() && link.is_none() {
            bail!("nothing to change: give --path or --link");
        }
        let path = path.map(check_checkout).transpose()?;
        let link = link.map(|l| check_link("forge link", l)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
        if let Some(path) = &path {
            let holder: Option<i64> = tx
                .query_row(
                    "SELECT id FROM repo WHERE path = ?1 AND id != ?2",
                    params![path, repo.0],
                    |r| r.get(0),
                )
                .optional()
                .context(cannot_write.clone())?;
            if let Some(r) = holder {
                bail!("{path} is already {}", RepoId(r));
            }
        }
        tx.execute(
            "UPDATE repo SET path = COALESCE(?1, path), link = COALESCE(?2, link) WHERE id = ?3",
            params![path, link, repo.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Whether `repo` is on the board.
    pub fn has_repo(&self, repo: RepoId) -> Result<bool> {
        Ok(refuse_missing_repo(&self.conn, repo)?.is_ok())
    }

    /// Removes `repo` with its lead and every thread on its board, with their messages, in
    /// one transaction, and says what went. Refuses a repo not on the board and one that
    /// still holds a milestone or a standalone task, naming them. With neither, its board is
    /// its repo-wide threads only (`thread_board`). Nothing else points at what goes: the
    /// lead's cursor (`told_upto`) and episode (`deaf_thread`) are on its row, a message's
    /// read mark on the message, and the delivery cursors are message ids, never reused.
    pub fn remove_repo(&mut self, repo: RepoId) -> Result<Removed> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
        let held: Vec<String> = tx
            .prepare(
                "SELECT 'M' || id FROM milestone WHERE repo_id = ?1 \
                 UNION ALL SELECT '#' || id FROM task WHERE repo_id = ?1",
            )?
            .query_map(params![repo.0], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !held.is_empty() {
            bail!(
                "{repo} still holds {}; move each to another repo first with tower milestone \
                 set M<n> --repo R<n> or tower task set <n> --repo R<n>, or remove an \
                 abandoned standalone task with tower task remove <n>",
                held.join(", ")
            );
        }
        let lead = lead_of_repo(&tx, repo)?;
        let count = |sql: &str| -> rusqlite::Result<usize> {
            tx.query_row(sql, params![repo.0], |r| r.get::<_, i64>(0))
                .map(|n| n as usize)
        };
        let threads = count("SELECT count(*) FROM thread_board WHERE repo_id = ?1")?;
        let messages = count(
            "SELECT count(*) FROM message \
             WHERE thread_id IN (SELECT id FROM thread_board WHERE repo_id = ?1)",
        )?;
        let inbox = inbox_items(
            &tx,
            "SELECT id FROM thread_board WHERE repo_id = ?2",
            repo.0,
        )?;
        for sql in [
            "DELETE FROM lead WHERE repo_id = ?1",
            "DELETE FROM message \
             WHERE thread_id IN (SELECT id FROM thread_board WHERE repo_id = ?1)",
            "DELETE FROM thread WHERE id IN (SELECT id FROM thread_board WHERE repo_id = ?1)",
            "DELETE FROM repo WHERE id = ?1",
        ] {
            tx.execute(sql, params![repo.0])
                .context(cannot_write.clone())?;
        }
        tx.commit().context(cannot_write)?;
        Ok(Removed {
            lead,
            threads,
            messages,
            inbox,
        })
    }

    /// Removes an abandoned standalone task with its subtasks, criteria, blocked-by
    /// rows, the watch's rows and its threads with their messages, in one transaction, and
    /// says what went, so its repo can be retired with `remove_repo`. A thread linked to it
    /// takes its board from it (`thread_board`), so it goes with it rather than staying on
    /// no board or moving to a board it is not about. Refuses a task not on the board, a
    /// subtask, a milestone's task (its milestone pins the repo, not the task, and its Done
    /// mark would need taking back), a task with an assignee (a worker may be on it) and
    /// one another task is blocked by, naming each. A Done task goes by the same rules.
    pub fn remove_task(&mut self, task: TaskId) -> Result<TaskRemoved> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        // Its milestone, parent, assignee and title.
        type Found = (Option<i64>, Option<i64>, Option<String>, String);
        let found: Option<Found> = tx
            .query_row(
                "SELECT milestone_id, parent_id, assignee, title FROM task WHERE id = ?1",
                params![task.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .context(cannot_write.clone())?;
        let title = match found {
            None => bail!("there is no task {task} on the board"),
            Some((_, Some(parent), _, _)) => bail!(
                "{task} is a subtask of {}; only a standalone task is removed, with its subtasks",
                TaskId(parent)
            ),
            Some((Some(under), None, _, _)) => bail!(
                "{task} is under {}; only a standalone task is removed",
                MilestoneId(under)
            ),
            Some((None, None, Some(worker), _)) => bail!(
                "{task} is assigned to {worker}; if nobody is on it any more, unassign it \
                 first with tower task unassign {}",
                task.0
            ),
            Some((None, None, None, title)) => title,
        };
        // What waits for the task or one of its subtasks, from outside them.
        let waiting: Vec<(TaskId, TaskId)> = tx
            .prepare(
                "SELECT task_id, blocker_id FROM blocker \
                 WHERE blocker_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1) \
                 AND task_id NOT IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1) \
                 ORDER BY task_id, blocker_id",
            )?
            .query_map(params![task.0], |r| {
                Ok((TaskId(r.get(0)?), TaskId(r.get(1)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !waiting.is_empty() {
            let (named, undo): (Vec<String>, Vec<String>) = (waiting.iter())
                .map(|(t, b)| {
                    (
                        format!("{t} is blocked by {b}"),
                        format!("tower task blocked-by remove {} {}", t.0, b.0),
                    )
                })
                .unzip();
            bail!(
                "{task} cannot go while other tasks wait for it: {}; take each away first \
                 with {}",
                named.join(", "),
                undo.join(" and ")
            );
        }
        let count = |sql: &str| -> rusqlite::Result<usize> {
            tx.query_row(sql, params![task.0], |r| r.get::<_, i64>(0))
                .map(|n| n as usize)
        };
        let subtasks = count("SELECT count(*) FROM task WHERE parent_id = ?1")?;
        let criteria = count(
            "SELECT count(*) FROM criterion \
             WHERE task_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1)",
        )?;
        // A thread links to a task, never a subtask (the `thread_not_on_a_subtask` trigger).
        let threads = count("SELECT count(*) FROM thread WHERE task_id = ?1")?;
        let messages = count(
            "SELECT count(*) FROM message \
             WHERE thread_id IN (SELECT id FROM thread WHERE task_id = ?1)",
        )?;
        let inbox = inbox_items(&tx, "SELECT id FROM thread WHERE task_id = ?2", task.0)?;
        // Every row that points at the task or a subtask goes before them. The lead's
        // cursor and episode, read marks and delivery cursors point at none of it (see
        // `remove_repo`); the "not listening" thread is the repo's, never a task's.
        for sql in [
            "DELETE FROM message WHERE thread_id IN (SELECT id FROM thread WHERE task_id = ?1)",
            "DELETE FROM thread WHERE task_id = ?1",
            "DELETE FROM watch_flag \
             WHERE task_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1)",
            "DELETE FROM watch_seen \
             WHERE task_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1)",
            "DELETE FROM criterion \
             WHERE task_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1)",
            "DELETE FROM blocker \
             WHERE task_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1) \
             OR blocker_id IN (SELECT id FROM task WHERE id = ?1 OR parent_id = ?1)",
            "DELETE FROM task WHERE parent_id = ?1",
            "DELETE FROM task WHERE id = ?1",
        ] {
            tx.execute(sql, params![task.0])
                .context(cannot_write.clone())?;
        }
        tx.commit().context(cannot_write)?;
        Ok(TaskRemoved {
            title,
            subtasks,
            criteria,
            threads,
            messages,
            inbox,
        })
    }

    /// Every repo on the board, in id order, as `repo` reads each.
    /// One read transaction, so a repo removed meanwhile is either all there or not at all.
    pub fn repos(&mut self) -> Result<Vec<RepoDetails>> {
        let tx = self.conn.transaction()?;
        let ids: Vec<i64> = tx
            .prepare("SELECT id FROM repo ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut repos = Vec::with_capacity(ids.len());
        for id in ids {
            let repo = RepoId(id);
            repos.push(repo_details(&tx, repo)?.with_context(|| {
                format!("{repo} vanished from the board while it was read in one transaction")
            })?);
        }
        tx.commit()?;
        Ok(repos)
    }

    /// The milestones and standalone tasks in no repo yet, in id order: what the schema 14
    /// upgrade left unassigned (MIGRATIONS[13]), for `tower repo list` to name.
    pub fn without_repo(&mut self) -> Result<(Vec<MilestoneId>, Vec<TaskId>)> {
        let milestones = self
            .conn
            .prepare("SELECT id FROM milestone WHERE repo_id IS NULL ORDER BY id")?
            .query_map([], |r| Ok(MilestoneId(r.get(0)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let tasks = self
            .conn
            .prepare(
                "SELECT id FROM task WHERE repo_id IS NULL AND milestone_id IS NULL \
                 AND parent_id IS NULL ORDER BY id",
            )?
            .query_map([], |r| Ok(TaskId(r.get(0)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok((milestones, tasks))
    }

    /// A repo with its lead, milestones, standalone tasks and threads. Refuses a repo not on
    /// the board.
    pub fn repo(&mut self, repo: RepoId) -> Result<RepoDetails> {
        let tx = self.conn.transaction()?;
        let found = repo_details(&tx, repo)?;
        tx.commit()?;
        found.with_context(|| format!("there is no repo {repo} on the board"))
    }

    /// Adds a milestone, in `repo` if given, else unassigned until `set_milestone` gives it
    /// one. Refuses a repo not on the board.
    pub fn add_milestone(
        &mut self,
        title: &str,
        goal: &str,
        repo: Option<RepoId>,
    ) -> Result<MilestoneId> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let id = insert_milestone_in(&tx, title, goal, repo, &cannot_write)?;
        tx.commit().context(cannot_write)?;
        Ok(id)
    }

    pub fn add_task(&mut self, milestone: MilestoneId, title: &str) -> Result<TaskId> {
        self.insert_task(Some(milestone), None, title, None)
    }

    /// Adds a standalone task, under no milestone, Waiting, in `repo` if given. A lead takes
    /// it on later with `hand_task`. Refuses a repo not on the board.
    pub fn add_standalone_task(&mut self, title: &str, repo: Option<RepoId>) -> Result<TaskId> {
        self.insert_task(None, repo, title, None)
    }

    /// Adds a task under `milestone`, or standalone in `repo` (or in no repo), as `add_task`
    /// and `add_standalone_task` do, with its tracker `link` if given. A link that is
    /// another repo's forge ticket is refused before the task exists (`refuse_link_outside`),
    /// so a refusal leaves no task.
    pub fn insert_task(
        &mut self,
        milestone: Option<MilestoneId>,
        repo: Option<RepoId>,
        title: &str,
        link: Option<&str>,
    ) -> Result<TaskId> {
        let title = check_line("task title", title)?;
        let link = link.map(|v| check_link("tracker link", v)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        if let Some(milestone) = milestone {
            refuse_missing(&tx, milestone).context(cannot_write.clone())??;
        }
        if let Some(repo) = repo {
            refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
        }
        if let Some(link) = &link {
            let (in_repo, of) = match milestone {
                Some(m) => {
                    let r: Option<i64> = tx
                        .query_row(
                            "SELECT repo_id FROM milestone WHERE id = ?1",
                            params![m.0],
                            |r| r.get(0),
                        )
                        .context(cannot_write.clone())?;
                    (r.map(RepoId), m.to_string())
                }
                None => (repo, "the new task".to_string()),
            };
            if let Some(in_repo) = in_repo {
                refuse_link_outside(&tx, link, in_repo, &of).context(cannot_write.clone())??;
            }
        }
        let milestone = milestone.map(|m| m.0);
        tx.execute(
            "INSERT INTO task (milestone_id, repo_id, title, link) VALUES (?1, ?2, ?3, ?4)",
            params![milestone, repo.map(|r| r.0), title, link],
        )
        .context(cannot_write.clone())?;
        let id = TaskId(tx.last_insert_rowid());
        // A new task is not Done, so a Done milestone is open again until its lead says so.
        // Every task mutation must clear the mark like this: see the rule at MIGRATIONS[2].
        tx.execute(
            "UPDATE milestone SET done = 0 WHERE id = ?1",
            params![milestone],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(id)
    }

    /// Hands a standalone task, with its subtasks, to the lead of `milestone`: the task moves
    /// under it, and into its repo. Refuses a task not on the board, a subtask, a task
    /// already under a milestone, a milestone not on the board, one without a lead, and a
    /// task in another repo (its threads stay on its repo's board, per the rule at
    /// MIGRATIONS[13]). The milestone's task set changes, so its Done mark goes, per the rule
    /// at MIGRATIONS[2]. A task in no repo whose link is another repo's forge ticket is
    /// refused (`refuse_foreign_link`). Returns the lead it went to.
    pub fn hand_task(&mut self, task: TaskId, milestone: MilestoneId) -> Result<Lead> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let found: Option<(Option<i64>, Option<i64>, Option<i64>)> = tx
            .query_row(
                "SELECT milestone_id, parent_id, repo_id FROM task WHERE id = ?1",
                params![task.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context(cannot_write.clone())?;
        let in_repo = match found {
            None => bail!("there is no task {task} on the board"),
            Some((_, Some(parent), _)) => bail!(
                "{task} is a subtask of {}; hand its task to a lead instead",
                TaskId(parent)
            ),
            Some((Some(under), None, _)) => bail!(
                "{task} is already under {}; only a standalone task is handed to a lead",
                MilestoneId(under)
            ),
            Some((None, None, repo)) => repo,
        };
        refuse_missing(&tx, milestone).context(cannot_write.clone())??;
        let to_repo: Option<i64> = tx
            .query_row(
                "SELECT repo_id FROM milestone WHERE id = ?1",
                params![milestone.0],
                |r| r.get(0),
            )
            .context(cannot_write.clone())?;
        let Some(lead) = lead_of(&tx, milestone).context(cannot_write.clone())? else {
            bail!(
                "{milestone} has no lead yet, so there is nobody to hand {task} to; {}",
                no_lead_hint(milestone, to_repo.is_some())
            );
        };
        if let Some(from) = in_repo
            && Some(from) != to_repo
        {
            bail!(
                "{task} is in {}, and {milestone} is not; hand it to a milestone of its own repo",
                RepoId(from)
            );
        }
        // Its subtasks follow it by their parent_id; their own milestone_id stays NULL. Its
        // repo is now its milestone's.
        tx.execute(
            "UPDATE task SET milestone_id = ?1, repo_id = NULL WHERE id = ?2",
            params![milestone.0, task.0],
        )
        .context(cannot_write.clone())?;
        // A task in no repo gets the milestone's here, so its link is checked against it.
        if in_repo.is_none() {
            refuse_foreign_link(&tx, task).context(cannot_write.clone())??;
        }
        // The milestone's task set changed: clear its Done mark, per the rule at MIGRATIONS[2].
        tx.execute(
            "UPDATE milestone SET done = 0 WHERE id = ?1",
            params![milestone.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(lead)
    }

    /// Adds a subtask under `parent`, Waiting. One level only: refuses a parent that is
    /// itself a subtask, and a parent that is not on the board. A new subtask is not Done, so a
    /// Done parent goes back to In progress, taking its milestone's Done mark with it.
    pub fn add_subtask(&mut self, parent: TaskId, title: &str) -> Result<TaskId> {
        let title = check_line("subtask title", title)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let found: Option<Option<i64>> = tx
            .query_row(
                "SELECT parent_id FROM task WHERE id = ?1",
                params![parent.0],
                |r| r.get(0),
            )
            .optional()
            .context(cannot_write.clone())?;
        match found {
            None => bail!("there is no task {parent} on the board"),
            Some(Some(grandparent)) => bail!(
                "{parent} is a subtask of {}; a subtask has no subtasks of its own: \
                 ask the lead for a new task",
                TaskId(grandparent)
            ),
            Some(None) => {}
        }
        tx.execute(
            "INSERT INTO task (parent_id, title) VALUES (?1, ?2)",
            params![parent.0, title],
        )
        .context(cannot_write.clone())?;
        let id = TaskId(tx.last_insert_rowid());
        reopen_parent(&tx, parent).context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(id)
    }

    /// A task or subtask's parent, if it is a subtask.
    pub fn parent_of(&mut self, task: TaskId) -> Result<Option<TaskId>> {
        let found: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT parent_id FROM task WHERE id = ?1",
                params![task.0],
                |r| r.get(0),
            )
            .optional()?;
        match found {
            None => bail!("there is no task {task} on the board"),
            Some(parent) => Ok(parent.map(TaskId)),
        }
    }

    /// One task with its subtasks, standalone or under a milestone. Refuses a task that is not
    /// on the board, and a subtask.
    pub fn task(&mut self, task: TaskId) -> Result<Task> {
        let tx = self.conn.transaction()?;
        let found = tx
            .query_row(
                &format!("SELECT {TASK_COLUMNS} FROM task WHERE id = ?1 AND parent_id IS NULL"),
                params![task.0],
                task_from_row,
            )
            .optional()?;
        let Some(mut found) = found else {
            bail!("there is no task {task} on the board");
        };
        found.subtasks = subtasks_of(&tx, task)?;
        found.details.criteria = criteria_of(&tx, task)?;
        found.details.blocked_by = blockers_of(&tx, task)?;
        found.details.doing = doing_posts(&tx, Some(task))?.pop().map(|(_, post)| post);
        tx.commit()?;
        Ok(found)
    }

    /// Registers the lead of the repo `of` names, with the colour given or else the next
    /// free one from the palette, and returns it with the repo. Refuses a repo that already
    /// has a lead, and a name or session (both ignoring ASCII case) that already leads
    /// another repo.
    pub fn add_lead(
        &mut self,
        of: RepoOf,
        name: &str,
        session: &str,
        colour: Option<Colour>,
    ) -> Result<(Lead, RepoId)> {
        let name = check_line("lead name", name)?;
        let session = check_session(session)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let repo = repo_of(&tx, of).context(cannot_write.clone())??;
        if let Some(current) = lead_of_repo(&tx, repo).context(cannot_write.clone())? {
            bail!(
                "{repo} already has a lead, {}; a repo has one lead",
                current.name
            );
        }
        refuse_taken(&tx, Some(repo), Some(&name), Some(&session))
            .context(cannot_write.clone())??;
        let lead = insert_lead(&tx, repo, name, session, colour).context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok((lead, repo))
    }

    /// Starts a lead in no repo yet from the dashboard, in one transaction: adds the
    /// milestone (`title`, `goal`) in no repo and returns it with the name its lead takes,
    /// `name_of` the milestone, or with the milestone id after it when taken
    /// (`free_lead_name`), against every lead on the board and every task's assignee. The
    /// lead registers itself under that name once it has its repo (`add_lead`).
    pub fn start_lead_in_no_repo(
        &mut self,
        title: &str,
        goal: &str,
        name_of: impl Fn(MilestoneId) -> String,
    ) -> Result<(MilestoneId, String)> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let m = insert_milestone_in(&tx, title, goal, None, &cannot_write)?;
        let name = free_lead_name(&tx, None, &name_of(m), m).context(cannot_write.clone())??;
        tx.commit().context(cannot_write)?;
        Ok((m, name))
    }

    /// Starts a lead on `repo` from the dashboard, in one transaction: adds the milestone
    /// (`title`, `goal`) in the repo and registers its lead, named `name_of` the new
    /// milestone, as both its name and its session (the name the dashboard passes to
    /// `claude -n`); a taken name gets the milestone id after it (`free_lead_name`). Refuses
    /// a repo not on the board or one that has its lead, writing nothing, so a second start
    /// on the same repo is refused.
    pub fn start_repo_lead(
        &mut self,
        title: &str,
        goal: &str,
        repo: RepoId,
        name_of: impl Fn(MilestoneId) -> String,
    ) -> Result<(MilestoneId, Lead)> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        // Checked in the same order as before the insert; a refusal after it writes nothing,
        // the transaction being dropped uncommitted.
        let m = insert_milestone_in(&tx, title, goal, Some(repo), &cannot_write)?;
        if let Some(l) = lead_of_repo(&tx, repo).context(cannot_write.clone())? {
            bail!("{repo} already has its lead, {}", l.name);
        }
        let name =
            free_lead_name(&tx, Some(repo), &name_of(m), m).context(cannot_write.clone())??;
        let lead =
            insert_lead(&tx, repo, name.clone(), name, None).context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok((m, lead))
    }

    /// Undoes a dashboard start whose Claude did not start, in one transaction, so `n` can
    /// be tried again: removes the lead `session` registered on `lead`'s repo by
    /// `start_repo_lead`, only while it is still that lead and never attached (a lead that
    /// has attached is a real session and stays), and milestone `m` while nothing uses it
    /// (no task, no thread). Returns what it removed, like ["M3", "Comics Lead"].
    pub fn undo_start(
        &mut self,
        m: MilestoneId,
        lead: Option<(RepoId, &str)>,
    ) -> Result<Vec<String>> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let mut removed = Vec::new();
        let n = tx
            .execute(
                "DELETE FROM milestone WHERE id = ?1 \
                 AND NOT EXISTS (SELECT 1 FROM task WHERE milestone_id = ?1) \
                 AND NOT EXISTS (SELECT 1 FROM thread WHERE milestone_id = ?1)",
                params![m.0],
            )
            .context(cannot_write.clone())?;
        if n > 0 {
            removed.push(m.to_string());
        }
        if let Some((repo, session)) = lead {
            let n = tx
                .execute(
                    "DELETE FROM lead WHERE repo_id = ?1 AND session = ?2 AND claude_session IS NULL",
                    params![repo.0, session],
                )
                .context(cannot_write.clone())?;
            if n > 0 {
                removed.push(session.to_string());
            }
        }
        tx.commit().context(cannot_write)?;
        Ok(removed)
    }

    /// Changes the lead of the repo `of` names: whichever of its name, session and colour
    /// are given, by the same rules as `add_lead`. For example, the lead's Claude session was
    /// restarted under a new name, or another session takes over the repo: the board is the
    /// repo's, so its threads stay. Refuses a repo without a lead, and a call that changes
    /// nothing. Returns the lead with the repo.
    pub fn set_lead(
        &mut self,
        of: RepoOf,
        name: Option<&str>,
        session: Option<&str>,
        colour: Option<Colour>,
    ) -> Result<(Lead, RepoId)> {
        if name.is_none() && session.is_none() && colour.is_none() {
            bail!("nothing to change: give --name, --session or --colour");
        }
        let name = name.map(|n| check_line("lead name", n)).transpose()?;
        let session = session.map(check_session).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let repo = repo_of(&tx, of).context(cannot_write.clone())??;
        if lead_of_repo(&tx, repo)
            .context(cannot_write.clone())?
            .is_none()
        {
            bail!("{repo} has no lead yet; add one with tower lead add {repo}");
        }
        refuse_taken(&tx, Some(repo), name.as_deref(), session.as_deref())
            .context(cannot_write.clone())??;
        tx.execute(
            // A new session is a new Claude session: the old one is forgotten until the new
            // one attaches.
            "UPDATE lead SET name = COALESCE(?1, name), session = COALESCE(?2, session), \
             colour = COALESCE(?3, colour), \
             claude_session = CASE WHEN ?2 IS NOT NULL AND ?2 != session \
               THEN NULL ELSE claude_session END \
             WHERE repo_id = ?4",
            params![name, session, colour.map(Colour::name), repo.0],
        )
        .context(cannot_write.clone())?;
        let lead = lead_of_repo(&tx, repo)
            .context(cannot_write.clone())?
            .context("the lead vanished while it was changed")?;
        tx.commit().context(cannot_write)?;
        Ok((lead, repo))
    }

    /// Moves a task or subtask to `stage` and returns the milestone it is under, if any (never
    /// for a subtask, whose milestone is its parent's). Moving a task out of Done takes back
    /// its milestone's Done mark: the lead marks it Done again once every task is. Refuses a
    /// task that is not on the board or already at `stage`, and a task moved to Done while
    /// any of its subtasks is not (naming them). A subtask moved out of Done takes a Done
    /// parent back to In progress, and with it the parent's milestone's Done mark.
    pub fn set_task_stage(&mut self, task: TaskId, stage: Stage) -> Result<Option<MilestoneId>> {
        self.set_task_stage_by(task, stage, None)
    }

    /// As `set_task_stage`, recording `by` (an author's name, checked as one) as who moved
    /// it to Done. Refuses `by` for any other stage. Leaving Done clears who did it.
    pub fn set_task_stage_by(
        &mut self,
        task: TaskId,
        stage: Stage,
        by: Option<&str>,
    ) -> Result<Option<MilestoneId>> {
        if by.is_some() && stage != Stage::Done {
            bail!("--by names who moved a task to done; give it only with done");
        }
        let by = by.map(check_author).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        type Found = (Option<i64>, Option<i64>, Stage);
        let found: Option<Found> = tx
            .query_row(
                "SELECT milestone_id, parent_id, stage FROM task WHERE id = ?1",
                params![task.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context(cannot_write.clone())?;
        let Some((milestone, parent, current)) = found else {
            bail!("there is no task {task} on the board");
        };
        if current == stage {
            bail!("task {task} is already {}", stage.label());
        }
        if stage == Stage::Done && parent.is_none() {
            // The merge can be a subtask, so a task is not Done while any subtask is open.
            let open: Vec<String> = subtasks_of(&tx, task)
                .context(cannot_write.clone())?
                .iter()
                .filter(|s| s.stage != Stage::Done)
                .map(|s| format!("{} {}", s.id, s.stage.label()))
                .collect();
            if !open.is_empty() {
                bail!(
                    "task {task} is not done until every subtask is: {}",
                    open.join(", ")
                );
            }
        }
        tx.execute(
            // A Done task is not stuck, so Done clears the blocked flag.
            "UPDATE task SET stage = ?1, done_by = ?3, \
             blocked = CASE WHEN ?1 = 'done' THEN NULL ELSE blocked END WHERE id = ?2",
            params![stage.name(), task.0, by],
        )
        .context(cannot_write.clone())?;
        // Clears the Done mark in this transaction, per the rule at MIGRATIONS[2].
        if stage != Stage::Done {
            tx.execute(
                "UPDATE milestone SET done = 0 WHERE id = ?1",
                params![milestone],
            )
            .context(cannot_write.clone())?;
        }
        if let Some(parent) = parent
            && stage != Stage::Done
        {
            reopen_parent(&tx, TaskId(parent)).context(cannot_write.clone())?;
        }
        if stage == Stage::Done && parent.is_none() {
            tell_blocked_boards(&tx, task, &cannot_write)?;
        }
        tx.commit().context(cannot_write)?;
        Ok(milestone.map(MilestoneId))
    }

    /// The lead's Done mark on `milestone`. Refuses a milestone with no tasks, one with a
    /// task not Done (naming them), and one already Done.
    pub fn mark_milestone_done(&mut self, milestone: MilestoneId) -> Result<()> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing(&tx, milestone).context(cannot_write.clone())??;
        let tasks = tasks_of(&tx, milestone).context(cannot_write.clone())?;
        if tasks.is_empty() {
            bail!("{milestone} has no tasks, so it cannot be done");
        }
        let open: Vec<String> = tasks
            .iter()
            .filter(|t| t.stage != Stage::Done)
            .map(|t| format!("{} {}", t.id, t.stage.label()))
            .collect();
        if !open.is_empty() {
            bail!(
                "{milestone} is not done until every task is: {}",
                open.join(", ")
            );
        }
        let changed = tx
            .execute(
                "UPDATE milestone SET done = 1, done_at = ?2 WHERE id = ?1 AND done = 0",
                params![milestone.0, now()?],
            )
            .context(cannot_write.clone())?;
        if changed == 0 {
            bail!("{milestone} is already done");
        }
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Sets whichever of a task's description, standing context, tracker link, MR line,
    /// assignee and repo are given, each replacing the stored one. Refuses a call that
    /// changes nothing, a subtask, and a task not on the board. The repo is for a standalone
    /// task only, as a milestone's task is in its milestone's repo; it is how a task the
    /// schema 14 upgrade left in no repo gets one (MIGRATIONS[13]). A task's threads follow
    /// it through `thread_board`, so a task with threads moves only to a repo with a lead,
    /// as `set_milestone` rules for a milestone. A link, or a repo for a linked task, is
    /// refused when the link is another repo's forge ticket (`refuse_foreign_link`).
    /// Returns where it went when given a repo.
    pub fn set_task(&mut self, task: TaskId, fields: TaskFields) -> Result<Option<Moved>> {
        let TaskFields {
            description,
            context,
            link,
            mr,
            assignee,
            repo,
        } = fields;
        if [description, context, link, mr, assignee]
            .iter()
            .all(Option::is_none)
            && repo.is_none()
        {
            bail!(
                "nothing to change: give --description, --context, --link, --mr, --assignee \
                 or --repo"
            );
        }
        let description = description
            .map(|v| check_text("description", v))
            .transpose()?;
        let context = context
            .map(|v| check_text("standing context", v))
            .transpose()?;
        let link = link.map(|v| check_link("tracker link", v)).transpose()?;
        let mr = mr.map(|v| check_line("MR", v)).transpose()?;
        if assignee.is_some_and(|v| v.trim().is_empty()) {
            bail!(
                "the assignee is empty: to clear it, tower task unassign {}",
                task.0
            );
        }
        let assignee = assignee.map(|v| check_line("assignee", v)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        // A lead is never a task's assignee: the assignee is the worker doing the
        // current step, and a lead has none. `lead.session` is NOCASE, so ASCII case is
        // ignored as the watch compares names.
        if let Some(name) = &assignee {
            let lead: Option<(String, String)> = tx
                .query_row(
                    "SELECT name, session FROM lead WHERE session = ?1",
                    params![name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .context(cannot_write.clone())?;
            if let Some((lead, session)) = lead {
                bail!(
                    "{session} is the session of the lead {lead}, and a lead is never a task's \
                     assignee: assign the worker doing the step, or leave the task \
                     unassigned between steps with tower task unassign {}",
                    task.0
                );
            }
        }
        let mut moved = None;
        if let Some(repo) = repo {
            let under: Option<i64> = tx
                .query_row(
                    "SELECT milestone_id FROM task WHERE id = ?1",
                    params![task.0],
                    |r| r.get(0),
                )
                .context(cannot_write.clone())?;
            if let Some(m) = under {
                let m = MilestoneId(m);
                bail!(
                    "{task} is under {m}, and its repo is {m}'s: only a standalone task takes --repo"
                );
            }
            refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
            let threads: i64 = tx
                .query_row(
                    "SELECT count(*) FROM thread WHERE task_id = ?1",
                    params![task.0],
                    |r| r.get(0),
                )
                .context(cannot_write.clone())?;
            let lead = lead_of_repo(&tx, repo).context(cannot_write.clone())?;
            if threads > 0 && lead.is_none() {
                bail!(
                    "{task} has {threads} thread(s), and {repo} has no lead to read them \
                     there; add its lead first: tower lead add {repo}"
                );
            }
            tx.execute(
                "UPDATE task SET repo_id = ?1 WHERE id = ?2",
                params![repo.0, task.0],
            )
            .context(cannot_write.clone())?;
            moved = Some(Moved {
                repo,
                threads: threads as usize,
                lead,
            });
        }
        tx.execute(
            "UPDATE task SET description = COALESCE(?1, description), \
             context = COALESCE(?2, context), link = COALESCE(?3, link), \
             mr = COALESCE(?4, mr), assignee = COALESCE(?5, assignee) WHERE id = ?6",
            params![description, context, link, mr, assignee, task.0],
        )
        .context(cannot_write.clone())?;
        // A new link, or a new repo for a linked task, must not file another repo's forge
        // ticket here; a refusal drops the transaction, so nothing is written.
        if link.is_some() || repo.is_some() {
            refuse_foreign_link(&tx, task).context(cannot_write.clone())??;
        }
        tx.commit().context(cannot_write)?;
        Ok(moved)
    }

    /// Adds an acceptance criterion to a task, not met, and returns its number (from 1, in
    /// the order added). Refuses a criterion the task already has, a subtask, and a task not
    /// on the board.
    pub fn add_criterion(&mut self, task: TaskId, text: &str) -> Result<usize> {
        let text = check_line("criterion", text)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        let criteria = criteria_of(&tx, task).context(cannot_write.clone())?;
        if let Some(n) = criteria.iter().position(|c| c.text == text) {
            bail!(
                "{task} already has the criterion {text:?} (number {})",
                n + 1
            );
        }
        tx.execute(
            "INSERT INTO criterion (task_id, text) VALUES (?1, ?2)",
            params![task.0, text],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(criteria.len() + 1)
    }

    /// Marks a task's criterion number `n` (from 1) met or not, and returns its text. Refuses
    /// a number the task has no criterion for, and a criterion already so.
    pub fn check_criterion(&mut self, task: TaskId, n: usize, done: bool) -> Result<String> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        let ids: Vec<(i64, String, bool)> = tx
            .prepare("SELECT id, text, done FROM criterion WHERE task_id = ?1 ORDER BY id")
            .and_then(|mut stmt| {
                stmt.query_map(params![task.0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .context(cannot_write.clone())?;
        let Some((id, text, was)) = n.checked_sub(1).and_then(|i| ids.get(i)).cloned() else {
            bail!(
                "{task} has no criterion {n}; it has {} (numbered from 1)",
                ids.len()
            );
        };
        if was == done {
            let state = if done { "met" } else { "not met" };
            bail!("criterion {n} of {task} is already {state}");
        }
        tx.execute(
            "UPDATE criterion SET done = ?1 WHERE id = ?2",
            params![done, id],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(text)
    }

    /// Removes a task's criterion number `n` (from 1) and returns its text; the criteria
    /// after it move up a number. Refuses a number the task has no criterion for.
    pub fn remove_criterion(&mut self, task: TaskId, n: usize) -> Result<String> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        let ids: Vec<(i64, String)> = tx
            .prepare("SELECT id, text FROM criterion WHERE task_id = ?1 ORDER BY id")
            .and_then(|mut stmt| {
                stmt.query_map(params![task.0], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .context(cannot_write.clone())?;
        let Some((id, text)) = n.checked_sub(1).and_then(|i| ids.get(i)).cloned() else {
            bail!(
                "{task} has no criterion {n}; it has {} (numbered from 1)",
                ids.len()
            );
        };
        tx.execute("DELETE FROM criterion WHERE id = ?1", params![id])
            .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(text)
    }

    /// Takes a task's assignee away, so its card says "unassigned" again. Refuses a task
    /// nobody is assigned to.
    pub fn unassign(&mut self, task: TaskId) -> Result<()> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        let changed = tx
            .execute(
                "UPDATE task SET assignee = NULL WHERE id = ?1 AND assignee IS NOT NULL",
                params![task.0],
            )
            .context(cannot_write.clone())?;
        if changed == 0 {
            bail!("{task} is not assigned to anyone");
        }
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Records that `task` is blocked by `other`. Both must be tasks on the board; refuses
    /// the task itself, a task it is already blocked by, and one that would close a loop (`other`
    /// already waits, directly or not, for `task`).
    pub fn add_blocker(&mut self, task: TaskId, other: TaskId) -> Result<()> {
        if task == other {
            bail!("{task} cannot be blocked by itself");
        }
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        refuse_not_a_task(&tx, other).context(cannot_write.clone())??;
        if blockers_of(&tx, task)
            .context(cannot_write.clone())?
            .contains(&other)
        {
            bail!("{task} is already blocked by {other}");
        }
        let loops = tx
            .query_row(
                "WITH RECURSIVE waits(id) AS ( \
                     SELECT blocker_id FROM blocker WHERE task_id = ?1 \
                     UNION SELECT b.blocker_id FROM blocker b JOIN waits w ON b.task_id = w.id) \
                 SELECT 1 FROM waits WHERE id = ?2",
                params![other.0, task.0],
                |_| Ok(()),
            )
            .optional()
            .context(cannot_write.clone())?;
        if loops.is_some() {
            bail!("{other} already waits for {task}, so {task} cannot wait for {other}");
        }
        tx.execute(
            "INSERT INTO blocker (task_id, blocker_id) VALUES (?1, ?2)",
            params![task.0, other.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Sets the blocked flag on `task` with why, and returns the reason it had if it was
    /// already blocked (the new one replaces it). Refuses an empty or multi-line reason, a
    /// Done task, a subtask, and a task not on the board. Changes no stage, so no Done mark.
    pub fn block_task(&mut self, task: TaskId, reason: &str) -> Result<Option<String>> {
        let reason = check_line("blocked reason", reason)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let was = blocked_flag(&tx, task).context(cannot_write.clone())??;
        let stage: Stage = tx
            .query_row(
                "SELECT stage FROM task WHERE id = ?1",
                params![task.0],
                |r| r.get(0),
            )
            .context(cannot_write.clone())?;
        if stage == Stage::Done {
            bail!("{task} is done, and a done task is not stuck; move it back to block it");
        }
        tx.execute(
            "UPDATE task SET blocked = ?1 WHERE id = ?2",
            params![reason, task.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(was)
    }

    /// Clears the blocked flag on `task` and returns the reason it had. Refuses a task that
    /// is not blocked, a subtask, and a task not on the board.
    pub fn unblock_task(&mut self, task: TaskId) -> Result<String> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let Some(was) = blocked_flag(&tx, task).context(cannot_write.clone())?? else {
            bail!("{task} is not blocked");
        };
        tx.execute(
            "UPDATE task SET blocked = NULL WHERE id = ?1",
            params![task.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(was)
    }

    /// Records that `task` no longer waits for `other`. Refuses a task it is not blocked by.
    pub fn remove_blocker(&mut self, task: TaskId, other: TaskId) -> Result<()> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_not_a_task(&tx, task).context(cannot_write.clone())??;
        let removed = tx
            .execute(
                "DELETE FROM blocker WHERE task_id = ?1 AND blocker_id = ?2",
                params![task.0, other.0],
            )
            .context(cannot_write.clone())?;
        if removed == 0 {
            bail!("{task} is not blocked by {other}");
        }
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Sets whichever of a milestone's title, goal, standing context, spec, tracker link
    /// and repo are given, each replacing the stored one. A milestone given a repo takes
    /// the threads about it and its tasks along, and says so in what it returns. Refuses a
    /// call that changes nothing, a milestone not on the board, a repo not on the board, and
    /// a milestone with threads going to a repo with no lead (the rule at MIGRATIONS[13]),
    /// and a repo that is not the link's of one of its tasks (`refuse_foreign_link`).
    pub fn set_milestone(
        &mut self,
        milestone: MilestoneId,
        fields: MilestoneFields,
    ) -> Result<Option<Moved>> {
        let MilestoneFields {
            title,
            goal,
            context,
            spec,
            link,
            repo,
        } = fields;
        if [title, goal, context, spec, link]
            .iter()
            .all(Option::is_none)
            && repo.is_none()
        {
            bail!("nothing to change: give --title, --goal, --context, --spec, --link or --repo");
        }
        let title = title
            .map(|v| check_line("milestone title", v))
            .transpose()?;
        let goal = goal.map(|v| check_line("milestone goal", v)).transpose()?;
        let context = context
            .map(|v| check_text("standing context", v))
            .transpose()?;
        let spec = spec.map(|v| check_line("spec", v)).transpose()?;
        let link = link.map(|v| check_link("tracker link", v)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing(&tx, milestone).context(cannot_write.clone())??;
        tx.execute(
            "UPDATE milestone SET context = COALESCE(?1, context), spec = COALESCE(?2, spec), \
             link = COALESCE(?3, link), title = COALESCE(?4, title), goal = COALESCE(?5, goal) \
             WHERE id = ?6",
            params![context, spec, link, title, goal, milestone.0],
        )
        .context(cannot_write.clone())?;
        let mut moved = None;
        if let Some(repo) = repo {
            refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
            // Its threads, about it or its tasks, follow it through `thread_board`.
            let threads: i64 = tx
                .query_row(
                    "SELECT count(*) FROM thread WHERE milestone_id = ?1 \
                     OR task_id IN (SELECT id FROM task WHERE milestone_id = ?1)",
                    params![milestone.0],
                    |r| r.get(0),
                )
                .context(cannot_write.clone())?;
            let lead = lead_of_repo(&tx, repo).context(cannot_write.clone())?;
            if threads > 0 && lead.is_none() {
                bail!(
                    "{milestone} has {threads} thread(s), and {repo} has no lead to read them \
                     there; add its lead first: tower lead add {repo}"
                );
            }
            tx.execute(
                "UPDATE milestone SET repo_id = ?1 WHERE id = ?2",
                params![repo.0, milestone.0],
            )
            .context(cannot_write.clone())?;
            // Its tasks are now in the new repo, so each one's link is checked against it;
            // the first refusal drops the transaction, so nothing moves.
            let linked = tx
                .prepare(
                    "SELECT id FROM task WHERE milestone_id = ?1 AND link IS NOT NULL ORDER BY id",
                )
                .and_then(|mut q| {
                    q.query_map(params![milestone.0], |r| Ok(TaskId(r.get(0)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .context(cannot_write.clone())?;
            for task in linked {
                refuse_foreign_link(&tx, task).context(cannot_write.clone())??;
            }
            moved = Some(Moved {
                repo,
                threads: threads as usize,
                lead,
            });
        }
        tx.commit().context(cannot_write)?;
        Ok(moved)
    }

    /// Opens a thread on the board `of` names, started by `by` with its opening message
    /// `text`: linked to `task` if given, else about the milestone `of` names, else about
    /// the whole repo. Returns it with the repo whose board it is on. Refuses a board that is
    /// not there (an unknown milestone or session, a milestone whose repo has no lead) and a
    /// task that is not a task of that repo.
    pub fn open_thread(
        &mut self,
        of: &BoardOf,
        by: &str,
        task: Option<TaskId>,
        title: &str,
        text: &str,
    ) -> Result<(ThreadId, RepoId)> {
        let by = check_author(by)?;
        let text = check_message("message", text)?;
        let at = now()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let (repo, milestone) = board_of(&tx, of).context(cannot_write.clone())??;
        let id = new_thread(&tx, repo, milestone, task, title, &cannot_write)?;
        append(&tx, id, &by, &text, Kind::Plain, at, &cannot_write)?;
        tx.commit().context(cannot_write)?;
        Ok((id, repo))
    }

    /// Posts `text` by `author` to an open thread and returns the thread as it is now.
    /// Refuses a thread that is not on the board, and a resolved one.
    pub fn post(&mut self, thread: ThreadId, author: &str, text: &str) -> Result<Thread> {
        self.add_message(thread, author, "message", text, Kind::Plain)
    }

    /// Posts `text` by `author` as a progress post: the doing line of the task the thread is
    /// linked to, until a later one. Refuses as `post` does, and a thread linked to no task.
    pub fn post_progress(&mut self, thread: ThreadId, author: &str, text: &str) -> Result<Thread> {
        self.add_message(thread, author, "message", text, Kind::Progress)
    }

    /// Resolves an open thread, posting `evidence` by `author` as its last message and
    /// recording who resolved it and when, and returns it. Refuses empty evidence, a thread that is not on the board, and one that is
    /// already resolved.
    pub fn resolve_thread(
        &mut self,
        thread: ThreadId,
        author: &str,
        evidence: &str,
    ) -> Result<Thread> {
        self.add_message(thread, author, "evidence", evidence, Kind::Evidence)
    }

    /// Posts a message (`what` names its text in errors) to an open thread, resolving it by
    /// that message when it is evidence, and returns the thread with its whole history.
    fn add_message(
        &mut self,
        thread: ThreadId,
        author: &str,
        what: &str,
        text: &str,
        kind: Kind,
    ) -> Result<Thread> {
        let author = check_author(author)?;
        let text = check_message(what, text)?;
        let at = now()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        append(&tx, thread, &author, &text, kind, at, &cannot_write)?;
        let found = threads_where(&tx, "t.id = ?1", thread.0, false)
            .context(cannot_write.clone())?
            .pop()
            .map(|(_, t)| t)
            .context("the thread vanished while it was posted to")?;
        tx.commit().context(cannot_write)?;
        Ok(found)
    }

    /// When each task last had a post of any kind, by anyone but `system`, in a thread
    /// linked to it, as (task, seconds since the Unix epoch), in task id order: for spotting
    /// a silent worker, whose own flags must not count as the worker posting. Tasks
    /// with no such post are left out.
    pub fn latest_posts(&mut self) -> Result<Vec<(TaskId, i64)>> {
        let found = self
            .conn
            .prepare(
                "SELECT t.task_id, max(m.posted_at) FROM thread t \
                 JOIN message m ON m.thread_id = t.id \
                 WHERE t.task_id IS NOT NULL AND m.author != ?1 \
                 GROUP BY t.task_id ORDER BY t.task_id",
            )?
            .query_map(params![SYSTEM_AUTHOR], |r| {
                Ok((TaskId(r.get(0)?), r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(found)
    }

    /// Who the threads of each task not Done name, for the watch to know the task's team
    ///: every author of a post in a thread linked to it and every name such a post
    /// @mentions (`mentioned`), each with when it last posted there and when it was last
    /// mentioned, by task and then name ignoring ASCII case. `system`'s posts name no one.
    pub fn task_names(&mut self) -> Result<Vec<(TaskId, Named)>> {
        // Read every look: each author's latest post per task from the index, and the text
        // only of posts that can mention anyone (those with an `@`).
        const THREADS: &str = "FROM thread t JOIN message m ON m.thread_id = t.id \
             JOIN task k ON k.id = t.task_id WHERE k.stage != 'done' AND m.author != ?1";
        let authors: Vec<(i64, String, i64)> = self
            .conn
            .prepare(&format!(
                "SELECT t.task_id, m.author, max(m.posted_at) {THREADS} \
                 GROUP BY t.task_id, m.author"
            ))?
            .query_map(params![SYSTEM_AUTHOR], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mentions: Vec<(i64, String, i64)> = self
            .conn
            .prepare(&format!(
                "SELECT t.task_id, m.text, m.posted_at {THREADS} AND instr(m.text, '@') > 0"
            ))?
            .query_map(params![SYSTEM_AUTHOR], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut named: BTreeMap<(i64, String), Named> = BTreeMap::new();
        let mut note = |task: i64, name: &str, at: i64, posted: bool| {
            let n = (named.entry((task, name.to_ascii_lowercase()))).or_insert_with(|| Named {
                name: name.to_string(),
                posted: None,
                mentioned: None,
            });
            // The name as last written, by a post or a mention.
            if n.posted.max(n.mentioned).is_none_or(|last| at >= last) {
                n.name = name.to_string();
            }
            let time = if posted {
                &mut n.posted
            } else {
                &mut n.mentioned
            };
            *time = (*time).max(Some(at));
        };
        for (task, author, at) in &authors {
            note(*task, author, *at, true);
        }
        for (task, text, at) in &mentions {
            for name in mentioned(text) {
                note(*task, name, *at, false);
            }
        }
        Ok((named.into_iter())
            .map(|((task, _), n)| (TaskId(task), n))
            .collect())
    }

    /// One step of the stuck-worker watch on `task`, in one transaction: `change` to the
    /// flags it remembers and, with `post`, a message by `system`, tower's own author, in
    /// the task's thread at `at` (or after the thread's latest message, as any post): its
    /// open linked thread with the newest message, else a new thread on its lead's board
    /// linked to it, opened by the same rules as `open_thread`. Either both happen or
    /// neither, so a failure never leaves a flag unposted or a post unremembered. Returns
    /// the thread posted in. Only the watch posts as `system`; `check_author` refuses it
    /// everywhere else. Refuses a post about a task with no lead's board (a standalone
    /// task, a subtask, a milestone with no lead).
    pub fn watch_step(
        &mut self,
        task: TaskId,
        change: FlagChange,
        post: Option<&str>,
        at: i64,
    ) -> Result<Option<ThreadId>> {
        let post = post.map(|p| check_message("message", p)).transpose()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        match change {
            FlagChange::None => {}
            FlagChange::Raise(flag) => {
                tx.execute(
                    "INSERT OR REPLACE INTO watch_flag \
                     (task_id, signal, worker, quiet_since, raised_at, lead_told_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        task.0,
                        flag.signal.name(),
                        flag.worker,
                        flag.quiet_since,
                        flag.raised_at,
                        flag.lead_told_at
                    ],
                )
                .context(cannot_write.clone())?;
            }
            FlagChange::LeadTold(signal, worker) => {
                tx.execute(
                    "UPDATE watch_flag SET lead_told_at = ?1 \
                     WHERE task_id = ?2 AND signal = ?3 AND worker = ?4",
                    params![at, task.0, signal.name(), worker],
                )
                .context(cannot_write.clone())?;
            }
            FlagChange::Clear(signal, worker) => {
                tx.execute(
                    "DELETE FROM watch_flag WHERE task_id = ?1 AND signal = ?2 AND worker = ?3",
                    params![task.0, signal.name(), worker],
                )
                .context(cannot_write.clone())?;
            }
        }
        let mut posted = None;
        if let Some(text) = post {
            let title = format!("Watch on {task}");
            let thread = task_thread(&tx, task, &title, &cannot_write)?;
            append(
                &tx,
                thread,
                SYSTEM_AUTHOR,
                &text,
                Kind::Plain,
                at,
                &cannot_write,
            )?;
            posted = Some(thread);
        }
        tx.commit().context(cannot_write)?;
        Ok(posted)
    }

    /// The time the watch first saw each of `watched` (task, assignee) at the latest, kept
    /// on the board so a restart does not reset a quiet clock: a pair seen for the first
    /// time is stamped `now`, and pairs no longer watched are forgotten (a reassigned task
    /// starts afresh). Assignees compare ignoring ASCII case.
    pub fn watch_seen(
        &mut self,
        watched: &[(TaskId, String)],
        now: i64,
    ) -> Result<HashMap<TaskId, i64>> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let old: Vec<(i64, String)> = tx
            .prepare("SELECT task_id, worker FROM watch_seen")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (task, worker) in old {
            if !watched
                .iter()
                .any(|(t, w)| t.0 == task && w.eq_ignore_ascii_case(&worker))
            {
                tx.execute(
                    "DELETE FROM watch_seen WHERE task_id = ?1 AND worker = ?2",
                    params![task, worker],
                )
                .context(cannot_write.clone())?;
            }
        }
        let mut seen = HashMap::new();
        for (task, worker) in watched {
            let worker = worker.to_ascii_lowercase();
            tx.execute(
                "INSERT OR IGNORE INTO watch_seen (task_id, worker, since) VALUES (?1, ?2, ?3)",
                params![task.0, worker, now],
            )
            .context(cannot_write.clone())?;
            let since: i64 = tx.query_row(
                "SELECT since FROM watch_seen WHERE task_id = ?1 AND worker = ?2",
                params![task.0, worker],
                |r| r.get(0),
            )?;
            seen.insert(*task, since);
        }
        tx.commit().context(cannot_write)?;
        Ok(seen)
    }

    /// Records the Claude session id of the lead of `repo`, as `tower lead attach` reads it
    /// from the lead's own Bash, replacing any earlier one (a restarted session has a new
    /// one): the session the lead's Stop hook acts for and the watch checks is listening.
    /// Refuses a repo that is not on the board or has no lead.
    pub fn attach_lead(&mut self, repo: RepoId, session: &str) -> Result<Lead> {
        let session = check_line("Claude session id", session)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
        let Some(lead) = lead_of_repo(&tx, repo).context(cannot_write.clone())? else {
            bail!("{repo} has no lead yet; add one with tower lead add {repo}");
        };
        tx.execute(
            // A new session: the watch looks at it afresh.
            "UPDATE lead SET claude_session = ?1, quiet_turns = NULL WHERE repo_id = ?2",
            params![session, repo.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(lead)
    }

    /// The lead registered under the session name `name` (ignoring ASCII case, by the
    /// column's collation), with its repo and the Claude session it attached last, if any:
    /// how a starting Claude session knows it is a lead's.
    pub fn lead_named(&mut self, name: &str) -> Result<Option<(RepoId, Lead, Option<String>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT repo_id, name, session, colour, claude_session FROM lead \
                 WHERE session = ?1",
                params![name],
                |r| {
                    Ok((
                        RepoId(r.get("repo_id")?),
                        lead_from_row(r)?,
                        r.get("claude_session")?,
                    ))
                },
            )
            .optional()?)
    }

    /// The repo whose lead attached the Claude session `session`, if any: how the Stop hook
    /// knows its session is a lead.
    pub fn lead_of_session(&mut self, session: &str) -> Result<Option<RepoId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT repo_id FROM lead WHERE claude_session = ?1",
                params![session],
                |r| Ok(RepoId(r.get(0)?)),
            )
            .optional()?)
    }

    /// Whether the lead of `repo` has posts it has not been told of: a cheap read, so a
    /// stream looking every second writes only when there is something to tell.
    pub fn has_unread(&mut self, repo: RepoId) -> Result<bool> {
        Ok(!unread(&self.conn, repo)?.is_empty())
    }

    /// How many posts the lead of `repo` has not been told of, the ones `tell_lead` would
    /// tell, read without telling them: what `tower lead wait` waits for.
    pub fn unread_count(&mut self, repo: RepoId) -> Result<usize> {
        Ok(unread(&self.conn, repo)?.len())
    }

    /// Tells the lead of `repo` of its unread posts: the posts on its board after its
    /// cursor (`lead.told_upto`) not written by the lead (its name or session, ignoring
    /// ASCII case), in id order, read in one snapshot, are passed to `tell`, and only if
    /// `tell` succeeds does the cursor move, to the newest message of that snapshot. No
    /// transaction is held while `tell` runs, so a reader that stops reading never holds up
    /// the board's writers. The cursor moves only from where it was read: if another teller
    /// moved it meanwhile (the Stop hook as a stream prints), this one leaves it, so a post
    /// may at worst be told twice, never skipped. A failed `tell` (a stream whose reader is
    /// gone) moves nothing. Returns how many posts were told; `tell` is not called when
    /// there are none. Refuses a repo with no lead.
    pub fn tell_lead(
        &mut self,
        repo: RepoId,
        tell: impl FnOnce(&[Post]) -> Result<()>,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let from: Option<i64> = tx
            .query_row(
                "SELECT told_upto FROM lead WHERE repo_id = ?1",
                params![repo.0],
                |r| r.get(0),
            )
            .optional()?;
        let Some(from) = from else {
            bail!("{repo} has no lead yet; add one with tower lead add {repo}");
        };
        let posts = unread(&tx, repo)?;
        let upto: i64 =
            tx.query_row("SELECT coalesce(max(id), 0) FROM message", [], |r| r.get(0))?;
        tx.commit()?;
        if posts.is_empty() {
            return Ok(0);
        }
        tell(&posts)?;
        self.conn
            .execute(
                "UPDATE lead SET told_upto = ?1 WHERE repo_id = ?2 AND told_upto = ?3",
                params![upto, repo.0, from],
            )
            .context(self.cannot_write())?;
        Ok(posts.len())
    }

    /// Every registered lead, in repo order: the leads the watch checks are listening
    /// (attached or not).
    pub fn leads(&mut self) -> Result<Vec<Registered>> {
        let found = self
            .conn
            .prepare(
                "SELECT repo_id, name, session, colour, claude_session, deaf_thread, turns, \
                 quiet_turns FROM lead ORDER BY repo_id",
            )?
            .query_map([], |r| {
                Ok(Registered {
                    repo: RepoId(r.get("repo_id")?),
                    lead: lead_from_row(r)?,
                    claude_session: r.get("claude_session")?,
                    deaf: r.get::<_, Option<i64>>("deaf_thread")?.map(ThreadId),
                    turns: r.get("turns")?,
                    quiet_turns: r.get("quiet_turns")?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(found)
    }

    /// Every lead that attached its Claude session, in repo order.
    pub fn attached_leads(&mut self) -> Result<Vec<Registered>> {
        let mut leads = self.leads()?;
        leads.retain(|l| l.claude_session.is_some());
        Ok(leads)
    }

    /// Opens a "lead not listening" episode for the lead of `repo` at `at`, unless one is
    /// open: a new thread about the whole repo, "Lead not listening", with `text` posted
    /// by `system`. Returns the thread, or `None`, posting nothing, when one was open.
    pub fn lead_deaf(&mut self, repo: RepoId, text: &str, at: i64) -> Result<Option<ThreadId>> {
        let text = check_message("message", text)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let open: Option<Option<i64>> = tx
            .query_row(
                "SELECT deaf_thread FROM lead WHERE repo_id = ?1",
                params![repo.0],
                |r| r.get(0),
            )
            .optional()
            .context(cannot_write.clone())?;
        match open {
            None => bail!("{repo} has no lead"),
            Some(Some(_)) => return Ok(None),
            Some(None) => {}
        }
        let thread = new_thread(&tx, repo, None, None, NOT_LISTENING, &cannot_write)?;
        append(
            &tx,
            thread,
            SYSTEM_AUTHOR,
            &text,
            Kind::Plain,
            at,
            &cannot_write,
        )?;
        tx.execute(
            "UPDATE lead SET deaf_thread = ?1 WHERE repo_id = ?2",
            params![thread.0, repo.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(Some(thread))
    }

    /// Records `turns`, the turns the lead of `repo`'s session has ended, as when the watch
    /// first saw it, idle or busy, with no waiter, unless one is recorded already.
    pub fn lead_quiet(&mut self, repo: RepoId, turns: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE lead SET quiet_turns = ?1 WHERE repo_id = ?2 AND quiet_turns IS NULL",
                params![turns, repo.0],
            )
            .context(self.cannot_write())?;
        Ok(())
    }

    /// A waiter for `repo` has taken its lock: the turns its lead has ended are the watch's
    /// baseline (`quiet_turns`), so a turn the Stop hook counted just before the waiter took
    /// the lock is no turn ended without one.
    pub fn lead_waiter_armed(&mut self, repo: RepoId) -> Result<()> {
        self.conn
            .execute(
                "UPDATE lead SET quiet_turns = turns WHERE repo_id = ?1",
                params![repo.0],
            )
            .context(self.cannot_write())?;
        Ok(())
    }

    /// The Claude session `session` ended a turn: one more turn ended with no waiter running
    /// for the lead of `repo`, if `session` is still the one it attached and `waiting` (is a
    /// waiter running?) says no (the Stop hook's count, which the watch reads to know a lead
    /// finished a turn without re-arming). `waiting` is asked inside this write's own
    /// transaction, so a waiter that took its lock first is seen, and one that takes it
    /// later writes its baseline (`lead_waiter_armed`) after this commits, absorbing the
    /// count. Returns whether it counted.
    pub fn lead_turn_ended(
        &mut self,
        repo: RepoId,
        session: &str,
        waiting: impl FnOnce() -> Result<bool>,
    ) -> Result<bool> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        if waiting()? {
            return Ok(false);
        }
        let n = tx
            .execute(
                "UPDATE lead SET turns = turns + 1 WHERE repo_id = ?1 AND claude_session = ?2",
                params![repo.0, session],
            )
            .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(n > 0)
    }

    /// The lead of `repo` is listening: ends its "lead not listening" episode, if one is
    /// open, resolving its thread with `evidence` by `system` at `at` unless someone resolved
    /// it already, and takes its count of turns ended now as the baseline (`quiet_turns`), so
    /// a turn it ends without re-arming from here on is seen. Returns the episode's thread,
    /// or `None` when none was open.
    pub fn lead_listening(
        &mut self,
        repo: RepoId,
        evidence: &str,
        at: i64,
    ) -> Result<Option<ThreadId>> {
        let evidence = check_message("evidence", evidence)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let open: Option<(i64, bool)> = tx
            .query_row(
                "SELECT l.deaf_thread, t.resolved_at IS NULL FROM lead l \
                 JOIN thread t ON t.id = l.deaf_thread WHERE l.repo_id = ?1",
                params![repo.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context(cannot_write.clone())?;
        if let Some((thread, true)) = open {
            append(
                &tx,
                ThreadId(thread),
                SYSTEM_AUTHOR,
                &evidence,
                Kind::Evidence,
                at,
                &cannot_write,
            )?;
        }
        tx.execute(
            "UPDATE lead SET deaf_thread = NULL, quiet_turns = turns WHERE repo_id = ?1",
            params![repo.0],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(open.map(|(thread, _)| ThreadId(thread)))
    }

    /// Takes the lead of `repo` off the board, in one transaction: its row goes, so
    /// no session is its lead any more and the watch stops checking it, while the repo, its
    /// milestones, tasks and threads stay for the next lead (`tower lead add`). Its open
    /// "lead not listening" episode, if any, is resolved with `evidence` by `system` at `at`
    /// (unless someone resolved it already), so its item leaves the alert line. Its name and
    /// session are kept as a retired lead's, so a mention of it is no worker's. Nothing
    /// else points at a lead: threads belong to the repo, and its cursor (`told_upto`),
    /// episode and seq are on its own row. Refuses a repo not on the board or with no lead.
    /// Returns the lead and its episode's thread, if one was open.
    pub fn remove_lead(
        &mut self,
        repo: RepoId,
        evidence: &str,
    ) -> Result<(Lead, Option<ThreadId>)> {
        let evidence = check_message("evidence", evidence)?;
        let at = now()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        refuse_missing_repo(&tx, repo).context(cannot_write.clone())??;
        let Some(lead) = lead_of_repo(&tx, repo).context(cannot_write.clone())? else {
            bail!("{repo} has no lead to remove");
        };
        let open: Option<(i64, bool)> = tx
            .query_row(
                "SELECT l.deaf_thread, t.resolved_at IS NULL FROM lead l \
                 JOIN thread t ON t.id = l.deaf_thread WHERE l.repo_id = ?1",
                params![repo.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context(cannot_write.clone())?;
        if let Some((thread, true)) = open {
            append(
                &tx,
                ThreadId(thread),
                SYSTEM_AUTHOR,
                &evidence,
                Kind::Evidence,
                at,
                &cannot_write,
            )?;
        }
        tx.execute("DELETE FROM lead WHERE repo_id = ?1", params![repo.0])
            .context(cannot_write.clone())?;
        // Its names are no worker's from now on: a mention of it tells its author.
        tx.execute(
            "INSERT OR IGNORE INTO retired_lead (name) VALUES (lower(?1)), (lower(?2))",
            params![lead.name, lead.session],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok((lead, open.map(|(thread, _)| ThreadId(thread))))
    }

    /// Whether `repo` has a lead: `false` for a repo with none, or none any more, and an
    /// error only when the board cannot be read, so a caller can tell the two apart.
    pub fn has_lead(&mut self, repo: RepoId) -> Result<bool> {
        Ok(lead_of_repo(&self.conn, repo)?.is_some())
    }

    /// The lead of the repo `of` names, with the repo. Refuses a repo that is not on the
    /// board or has no lead, and a milestone in no repo.
    pub fn lead(&mut self, of: RepoOf) -> Result<(Lead, RepoId)> {
        let repo = repo_of(&self.conn, of)??;
        let lead = lead_of_repo(&self.conn, repo)?.with_context(|| {
            format!("{repo} has no lead yet; add one with tower lead add {repo}")
        })?;
        Ok((lead, repo))
    }

    /// Every lead with its board and the Claude inbox it attached, if any, in milestone
    /// order.
    pub fn lead_boards(&mut self) -> Result<Vec<LeadBoard>> {
        Ok(lead_boards(&self.conn)?)
    }

    /// The posts after the delivery cursor, in id order, with every lead's board, the
    /// assignees of the tasks not done and how far each recipient was sent beyond the
    /// cursor, read in one snapshot. Moves nothing: a round records each send (`mark_sent`)
    /// and then the cursor (`advance_delivery`).
    pub fn take_pending(&mut self) -> Result<Pending> {
        let tx = self.conn.transaction()?;
        let upto: i64 = tx
            .query_row("SELECT upto FROM delivery", [], |r| r.get(0))
            .context("the board has no delivery cursor")?;
        let posts: Vec<Post> = tx
            .prepare(
                "SELECT m.id, m.thread_id, b.repo_id, t.task_id, m.author, m.text, m.posted_at \
                 FROM message m JOIN thread t ON t.id = m.thread_id \
                 JOIN thread_board b ON b.id = t.id WHERE m.id > ?1 ORDER BY m.id",
            )?
            .query_map(params![upto], |r| {
                Ok(Post {
                    id: r.get(0)?,
                    thread: ThreadId(r.get(1)?),
                    repo: board_repo(r, 2, r.get(1)?)?,
                    task: r.get::<_, Option<i64>>(3)?.map(TaskId),
                    author: r.get(4)?,
                    text: r.get(5)?,
                    posted_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let leads = lead_boards(&tx)?;
        let open = tx
            .prepare(
                "SELECT k.id, coalesce(k.repo_id, m.repo_id), k.assignee \
                 FROM task k LEFT JOIN milestone m ON m.id = k.milestone_id \
                 WHERE k.assignee IS NOT NULL AND k.stage != 'done'",
            )?
            .query_map([], |r| {
                Ok((
                    TaskId(r.get(0)?),
                    r.get::<_, Option<i64>>(1)?.map(RepoId),
                    r.get(2)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let sent = tx
            .prepare("SELECT recipient, upto FROM delivery_sent")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        // Every name a worker has had on the board: each task's assignee, and each
        // author of a post but tower and the owner, lower case; never a lead taken off the
        // board, who posted there as a lead, not as a worker.
        let known = tx
            .prepare(
                "SELECT lower(assignee) FROM task WHERE assignee IS NOT NULL \
                 UNION SELECT lower(author) FROM message WHERE lower(author) NOT IN (?1, ?2) \
                 EXCEPT SELECT name FROM retired_lead",
            )?
            .query_map(params![SYSTEM_AUTHOR, OWNER_AUTHOR], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(Pending {
            posts,
            leads,
            open,
            sent,
            known,
        })
    }

    /// Records, before the send, that `recipient` is sent the posts up to id `upto`.
    /// Returns false, recording nothing, when it was sent that far already (another watch
    /// on the same board took the send), so a post never goes to a recipient twice.
    pub fn mark_sent(&mut self, recipient: &str, upto: i64) -> Result<bool> {
        let cannot_write = self.cannot_write();
        let marked = self
            .conn
            .execute(
                "INSERT INTO delivery_sent (recipient, upto) VALUES (?1, ?2) \
                 ON CONFLICT (recipient) DO UPDATE SET upto = excluded.upto \
                 WHERE upto < excluded.upto",
                params![recipient, upto],
            )
            .context(cannot_write)?;
        Ok(marked > 0)
    }

    /// Moves the delivery cursor to `upto` (never back): every recipient has been offered
    /// every post up to it. Forgets the sends recorded at or below it.
    pub fn advance_delivery(&mut self, upto: i64) -> Result<()> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        tx.execute("UPDATE delivery SET upto = max(upto, ?1)", params![upto])
            .context(cannot_write.clone())?;
        tx.execute(
            "DELETE FROM delivery_sent WHERE upto <= (SELECT upto FROM delivery)",
            [],
        )
        .context(cannot_write.clone())?;
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Opens a delivery failure episode, at `at`, for each of `recipients`
    /// (`Recipient::key`) that has none open, or one older than
    /// `deliver::EPISODE_EXPIRES` (a recipient unreachable for hours is told again), and
    /// when it opened any, posts one item for the owner's awareness
    /// (`board::DELIVERY_FAILURE`) as `system`: `text` of the indexes of those it opened, in
    /// `thread`, or, when that thread is resolved, in a new thread on the same board linked to
    /// the same task.
    /// Returns the thread posted in with those indexes, or `None`, posting nothing, when
    /// every one had an episode open: one item per drain and per episode, so repeated or
    /// many failures never flood the board.
    pub fn delivery_failed(
        &mut self,
        recipients: &[&str],
        thread: ThreadId,
        at: i64,
        text: impl FnOnce(&[usize]) -> String,
    ) -> Result<Option<(ThreadId, Vec<usize>)>> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let mut opened = Vec::new();
        for (i, recipient) in recipients.iter().enumerate() {
            let added = tx
                .execute(
                    "INSERT INTO delivery_fail (recipient, since) VALUES (?1, ?2) \
                     ON CONFLICT (recipient) DO UPDATE SET since = excluded.since \
                     WHERE since <= excluded.since - ?3",
                    params![recipient, at, crate::deliver::EPISODE_EXPIRES],
                )
                .context(cannot_write.clone())?;
            if added > 0 {
                opened.push(i);
            }
        }
        if opened.is_empty() {
            return Ok(None);
        }
        let text = check_message("message", &text(&opened))?;
        let thread = failure_item(&tx, thread, &text, at, &cannot_write)?;
        tx.commit().context(cannot_write)?;
        Ok(Some((thread, opened)))
    }

    /// Posts `text`, an item for the owner's awareness about a send that went to someone
    /// else (`board::DELIVERY_FAILURE`), as `system` in `thread` at `at`, or, when that thread
    /// is resolved, in a new thread on the same board linked to the same task, as
    /// `delivery_failed` does but always: no failure episode can hold it back, since a
    /// misroute is worse than a lost send. Returns the thread posted in.
    pub fn misrouted(&mut self, thread: ThreadId, at: i64, text: &str) -> Result<ThreadId> {
        let text = check_message("message", text)?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let thread = failure_item(&tx, thread, &text, at, &cannot_write)?;
        tx.commit().context(cannot_write)?;
        Ok(thread)
    }

    /// Posts tower's reply `text` as `system` in `thread`, at `at`: for a post's author, when
    /// it named someone no worker on the board is. Returns false, posting nothing,
    /// when the thread is resolved by now or not on the board.
    pub fn tower_reply(&mut self, thread: ThreadId, text: &str, at: i64) -> Result<bool> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let open: Option<bool> = tx
            .query_row(
                "SELECT resolved_at IS NULL FROM thread WHERE id = ?1",
                params![thread.0],
                |r| r.get(0),
            )
            .optional()
            .context(cannot_write.clone())?;
        if open != Some(true) {
            return Ok(false);
        }
        let text = check_message("message", text)?;
        append(
            &tx,
            thread,
            SYSTEM_AUTHOR,
            &text,
            Kind::Plain,
            at,
            &cannot_write,
        )?;
        tx.commit().context(cannot_write)?;
        Ok(true)
    }

    /// Ends `recipient`'s delivery failure episode, if one is open: a send got through.
    /// Returns whether one was.
    pub fn delivered(&mut self, recipient: &str) -> Result<bool> {
        let cannot_write = self.cannot_write();
        let ended = self
            .conn
            .execute(
                "DELETE FROM delivery_fail WHERE recipient = ?1",
                params![recipient],
            )
            .context(cannot_write)?;
        Ok(ended > 0)
    }

    /// For tests elsewhere in the crate: a plain post stamped `at`, a controlled time.
    #[cfg(test)]
    pub(crate) fn post_at(&mut self, thread: ThreadId, author: &str, text: &str, at: i64) {
        self.conn
            .execute(
                "INSERT INTO message (thread_id, author, text, posted_at) VALUES (?1, ?2, ?3, ?4)",
                params![thread.0, author, text, at],
            )
            .unwrap();
    }

    /// For tests: registers a lead on `milestone`'s repo, first putting the milestone in a
    /// new repo with no path when it has none, so a test reads as one lead per milestone.
    #[cfg(test)]
    pub(crate) fn lead_milestone(
        &mut self,
        milestone: MilestoneId,
        name: &str,
        session: &str,
        colour: Option<Colour>,
    ) -> Result<Lead> {
        let has_repo: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT repo_id FROM milestone WHERE id = ?1",
                params![milestone.0],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(None) = has_repo {
            self.conn
                .execute("INSERT INTO repo (path) VALUES (NULL)", [])?;
            self.conn.execute(
                "UPDATE milestone SET repo_id = last_insert_rowid() WHERE id = ?1",
                params![milestone.0],
            )?;
        }
        let of = RepoOf::Milestone(milestone);
        Ok(self.add_lead(of, name, session, colour)?.0)
    }

    /// For tests elsewhere in the crate: runs `sql` on the board, to break it on purpose.
    #[cfg(test)]
    pub(crate) fn exec_sql(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    /// Every flag the watch raised and has not seen end, by task, signal and worker.
    pub fn flags(&mut self) -> Result<Vec<Flag>> {
        let found = self
            .conn
            .prepare(
                "SELECT task_id, signal, worker, quiet_since, raised_at, lead_told_at \
                 FROM watch_flag ORDER BY task_id, signal, worker",
            )?
            .query_map([], |r| {
                Ok(Flag {
                    task: TaskId(r.get(0)?),
                    signal: r.get(1)?,
                    worker: r.get(2)?,
                    quiet_since: r.get(3)?,
                    raised_at: r.get(4)?,
                    lead_told_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(found)
    }

    /// Folds the task lists of `milestones` in the dashboard's tree, or unfolds them, all in
    /// one write. Refuses a milestone that is not on the board, writing none.
    pub fn set_folded(&mut self, milestones: &[MilestoneId], folded: bool) -> Result<()> {
        self.set_folds(&[], milestones, &[], folded)
    }

    /// Folds `repos`, the task lists of `milestones` and the Done groups of `done_groups`
    /// (repos) in the dashboard's tree, or unfolds them, all in one transaction, so a
    /// dashboard never reads one changed without the other. Refuses a repo or milestone
    /// that is not on the board, writing none.
    pub fn set_folds(
        &mut self,
        repos: &[RepoId],
        milestones: &[MilestoneId],
        done_groups: &[RepoId],
        folded: bool,
    ) -> Result<()> {
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        for r in repos {
            refuse_missing_repo(&tx, *r).context(cannot_write.clone())??;
            tx.execute(
                "UPDATE repo SET folded = ?1 WHERE id = ?2",
                params![folded, r.0],
            )
            .context(cannot_write.clone())?;
        }
        for m in milestones {
            refuse_missing(&tx, *m).context(cannot_write.clone())??;
            tx.execute(
                "UPDATE milestone SET folded = ?1 WHERE id = ?2",
                params![folded, m.0],
            )
            .context(cannot_write.clone())?;
        }
        for r in done_groups {
            refuse_missing_repo(&tx, *r).context(cannot_write.clone())??;
            tx.execute(
                "UPDATE repo SET done_folded = ?1 WHERE id = ?2",
                params![folded, r.0],
            )
            .context(cannot_write.clone())?;
        }
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// Marks a message for the owner read: a need-you item the owner cleared, or an
    /// awareness item read (`Tone::of`). It leaves the alert line and the inbox, and stays
    /// readable in its thread. Returns `Some(false)` when it was read already, and `None`
    /// when it is not on the board (its task or repo removed since the owner saw it), so the
    /// dashboard can say so in words. Refuses one that does not mention the owner.
    pub fn mark_read(&mut self, message: i64) -> Result<Option<bool>> {
        let at = now()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        let marked = mark_read_in(&tx, message, at, &cannot_write)?;
        tx.commit().context(cannot_write)?;
        Ok(marked)
    }

    /// Marks read the awareness items of a thread the owner opened, `messages`, in one go
    ///: each as `mark_read` does, except that one no longer on the board (its task or
    /// repo removed since) is skipped, not a failure, so a read tried again never sticks on
    /// it and the rest are still read.
    pub fn mark_seen(&mut self, messages: &[i64]) -> Result<()> {
        let at = now()?;
        let cannot_write = self.cannot_write();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context(cannot_write.clone())?;
        for &message in messages {
            mark_read_in(&tx, message, at, &cannot_write)?;
        }
        tx.commit().context(cannot_write)?;
        Ok(())
    }

    /// One thread with its whole history, for reading it in full. Refuses a thread that is
    /// not on the board.
    pub fn thread(&mut self, thread: ThreadId) -> Result<Thread> {
        let tx = self.conn.transaction()?;
        let found = threads_where(&tx, "t.id = ?1", thread.0, false)?.pop();
        tx.commit()?;
        found
            .map(|(_, t)| t)
            .with_context(|| format!("there is no thread {thread} on the board"))
    }

    /// Where a thread is: the repo whose board it is on, and the milestone it is about, if
    /// it is about one. Refuses a thread that is not on the board.
    pub fn thread_place(&mut self, thread: ThreadId) -> Result<(RepoId, Option<MilestoneId>)> {
        self.conn
            .query_row(
                "SELECT b.repo_id, t.milestone_id FROM thread t \
                 JOIN thread_board b ON b.id = t.id WHERE t.id = ?1",
                params![thread.0],
                |r| {
                    Ok((
                        board_repo(r, 0, thread.0)?,
                        r.get::<_, Option<i64>>(1)?.map(MilestoneId),
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("there is no thread {thread} on the board"))
    }

    /// Reads the whole board in one transaction, so it is a consistent snapshot.
    pub fn load(&mut self) -> Result<Board> {
        let tx = self.conn.transaction()?;
        let mut milestones: Vec<Milestone> = tx
            .prepare(
                "SELECT id, title, goal, done, context, spec, link, folded, repo_id, done_at \
                 FROM milestone ORDER BY id",
            )?
            .query_map([], |r| {
                Ok(Milestone {
                    id: MilestoneId(r.get(0)?),
                    title: r.get(1)?,
                    goal: r.get(2)?,
                    repo: r.get::<_, Option<i64>>(8)?.map(RepoId),
                    lead: None,
                    tasks: Vec::new(),
                    marked_done: r.get(3)?,
                    done_at: r.get(9)?,
                    folded: r.get(7)?,
                    threads: Vec::new(),
                    details: MilestoneDetails {
                        context: r.get(4)?,
                        spec: r.get(5)?,
                        link: r.get(6)?,
                    },
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        // Each milestone's repo, and its lead, the repo's, by milestone index.
        let mut repo_of: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut stmt = tx.prepare(
            "SELECT m.id, m.repo_id, l.name, l.session, l.colour FROM milestone m \
             LEFT JOIN lead l ON l.repo_id = m.repo_id WHERE m.repo_id IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let owner = MilestoneId(r.get(0)?);
            let i = milestones
                .binary_search_by_key(&owner.0, |m| m.id.0)
                .map_err(|_| anyhow::anyhow!("{owner} vanished while the board was read"))?;
            repo_of.entry(r.get(1)?).or_default().push(i);
            if r.get::<_, Option<String>>(2)?.is_some() {
                milestones[i].lead = Some(lead_from_row(r)?);
            }
        }
        drop(rows);
        drop(stmt);
        let mut stmt = tx.prepare(&format!(
            "SELECT milestone_id, {TASK_COLUMNS} FROM task WHERE parent_id IS NULL ORDER BY id"
        ))?;
        let mut rows = stmt.query([])?;
        let mut standalone: Vec<Task> = Vec::new();
        // Where each task sits, as (milestone index or None when standalone, task index), for
        // its subtasks, criteria and blockers below.
        let mut at: HashMap<i64, (Option<usize>, usize)> = HashMap::new();
        while let Some(r) = rows.next()? {
            let task = task_from_row(r)?;
            let Some(owner) = r.get::<_, Option<i64>>(0)?.map(MilestoneId) else {
                at.insert(task.id.0, (None, standalone.len()));
                standalone.push(task);
                continue;
            };
            // Milestones are sorted by id, and the foreign key guarantees the owner exists.
            let i = milestones
                .binary_search_by_key(&owner.0, |m| m.id.0)
                .map_err(|_| anyhow::anyhow!("task {} belongs to missing {owner}", task.id))?;
            at.insert(task.id.0, (Some(i), milestones[i].tasks.len()));
            milestones[i].tasks.push(task);
        }
        drop(rows);
        drop(stmt);
        let mut stmt = tx.prepare(
            "SELECT parent_id, id, title, stage FROM task WHERE parent_id IS NOT NULL ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let Some(t) = task_at(&mut milestones, &mut standalone, &at, r.get(0)?) {
                t.subtasks.push(subtask_from_row(r)?);
            }
        }
        drop(rows);
        drop(stmt);
        let mut stmt = tx.prepare("SELECT task_id, text, done FROM criterion ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let Some(t) = task_at(&mut milestones, &mut standalone, &at, r.get(0)?) {
                t.details.criteria.push(Criterion {
                    text: r.get(1)?,
                    done: r.get(2)?,
                });
            }
        }
        drop(rows);
        drop(stmt);
        let mut stmt = tx.prepare("SELECT task_id, blocker_id FROM blocker ORDER BY blocker_id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let Some(t) = task_at(&mut milestones, &mut standalone, &at, r.get(0)?) {
                t.details.blocked_by.push(TaskId(r.get(1)?));
            }
        }
        drop(rows);
        drop(stmt);
        for (task, post) in doing_posts(&tx, None)? {
            if let Some(t) = task_at(&mut milestones, &mut standalone, &at, task) {
                t.details.doing = Some(post);
            }
        }
        let mut repos: Vec<Repo> = tx
            .prepare(
                "SELECT r.id, r.path, r.link, r.folded, l.name, l.session, l.colour, \
                 r.done_folded FROM repo r LEFT JOIN lead l ON l.repo_id = r.id ORDER BY r.id",
            )?
            .query_map([], |r| {
                let lead = match r.get::<_, Option<String>>(4)? {
                    Some(name) => Some(Lead {
                        name,
                        session: r.get(5)?,
                        colour: r.get::<_, String>(6)?.parse().ok(),
                    }),
                    None => None,
                };
                Ok(Repo {
                    id: RepoId(r.get(0)?),
                    path: r.get(1)?,
                    link: r.get(2)?,
                    lead,
                    folded: r.get(3)?,
                    done_folded: r.get(7)?,
                    threads: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        // The board is the repo's: every milestone of a repo carries all of its threads.
        for (repo, thread) in threads_where(&tx, "1 = ?1", 1, true)? {
            for &i in repo_of.get(&repo.0).into_iter().flatten() {
                milestones[i].threads.push(thread.clone());
            }
            if let Ok(i) = repos.binary_search_by_key(&repo.0, |r| r.id.0) {
                repos[i].threads.push(thread);
            }
        }
        let mentions = tx
            .prepare(&format!(
                "SELECT m.thread_id, m.id, m.author, m.text, m.posted_at, \
                 m.read_at IS NOT NULL FROM message m {MENTIONS_WHERE} \
                 ORDER BY m.posted_at, m.id"
            ))?
            .query_map(params![OWNER_HANDLE], |r| {
                Ok(Mention {
                    thread: ThreadId(r.get(0)?),
                    message: Message {
                        id: r.get(1)?,
                        author: r.get(2)?,
                        text: r.get(3)?,
                        at: r.get(4)?,
                    },
                    read: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(Board {
            inbox: Inbox::of(&repos, mentions),
            repos,
            milestones,
            standalone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_board_of_any_home_is_matched_and_nothing_else() {
        // As a sandbox run sees it: HOME a temp dir, the account's own home another.
        let (temp, account) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let homes = [temp.path().to_path_buf(), account.path().to_path_buf()];
        let board = account.path().join(DEFAULT_BOARD);
        // A link to the account's home, as macOS's /var is to /private/var, made here so
        // every system has one: the board through it is the same board, from either side.
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(account.path(), &link).unwrap();
        let through = |what: &str| {
            let linked = link.join(DEFAULT_BOARD);
            assert!(is_board_in(&linked, &homes), "{what}, through the link");
            assert!(
                is_board_in(&board, std::slice::from_ref(&link)),
                "{what}, its home given as the link"
            );
        };
        assert!(
            is_board_in(&board, &homes),
            "the account's board, not created yet"
        );
        through("the account's board, not created yet");
        std::fs::create_dir_all(board.parent().unwrap()).unwrap();
        std::fs::write(&board, b"").unwrap();
        assert!(is_board_in(&board, &homes), "the account's board");
        through("the account's board");
        assert!(
            is_board_in(&temp.path().join(DEFAULT_BOARD), &homes),
            "HOME's board"
        );
        assert!(
            !is_board_in(&board, &homes[..1]),
            "matched a home not given"
        );
        assert!(!is_board_in(&account.path().join("board.db"), &homes));
        assert!(!is_board_in(&board, &[]));
    }

    #[test]
    fn a_temp_home_still_guards_the_account_home() {
        // Runs itself again under a temp HOME, since the tests share one environment.
        let account = account_home().unwrap();
        if let Some(temp) = std::env::var_os("TOWER_TEST_TEMP_HOME") {
            assert_eq!(homes(), [PathBuf::from(temp), account]);
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        assert_ne!(temp.path(), account);
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::tests::a_temp_home_still_guards_the_account_home",
            ])
            .env("HOME", temp.path())
            .env("TOWER_TEST_TEMP_HOME", temp.path())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && said.contains("1 passed"), "{said}");
    }

    #[test]
    fn the_account_home_comes_from_the_password_database() {
        // HOME is not consulted, so this holds under a temp HOME too.
        let home = account_home().expect("no password database entry for this account");
        assert!(home.is_absolute() && home.is_dir(), "{}", home.display());
    }
    use std::sync::{Arc, Barrier};

    fn temp_board() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/board.db");
        (dir, path)
    }

    #[test]
    fn first_open_creates_the_board_at_the_current_version() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        assert!(path.exists());
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load().unwrap(), Board::default());
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn adds_milestones_and_tasks_and_loads_them_in_order() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store
            .add_milestone(" First shop pages ", "Open to every reader.", None)
            .unwrap();
        let m2 = store
            .add_milestone("Checkout", "A reader can buy a book.", None)
            .unwrap();
        assert_eq!((m1, m2), (MilestoneId(1), MilestoneId(2)));
        let t1 = store.add_task(m2, "Shelf layout").unwrap();
        let t2 = store.add_task(m1, "Bookmarks page").unwrap();
        let t3 = store.add_task(m2, "Empty state").unwrap();

        // A second connection sees the same board: it is the file, not the session.
        let board = Store::open(&path).unwrap().load().unwrap();
        let shape: Vec<(MilestoneId, &str, Vec<TaskId>)> = board
            .milestones
            .iter()
            .map(|m| {
                (
                    m.id,
                    m.title.as_str(),
                    m.tasks.iter().map(|t| t.id).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (m1, "First shop pages", vec![t2]),
                (m2, "Checkout", vec![t1, t3]),
            ]
        );
    }

    #[test]
    fn refuses_bad_input_and_writes_nothing() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("M", "goal", None).unwrap();

        let err = store
            .add_milestone("", "goal", None)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "the milestone title is empty");
        let err = store
            .add_milestone("ok", "two\nlines", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("the milestone goal contains a control character"),
            "{err}"
        );
        let err = store.add_task(m, &"x".repeat(201)).unwrap_err().to_string();
        assert_eq!(
            err,
            "the task title is 201 characters long; the most is 200"
        );
        let err = store
            .add_task(MilestoneId(9), "orphan")
            .unwrap_err()
            .to_string();
        assert_eq!(err, "there is no milestone M9 on the board");

        let board = store.load().unwrap();
        assert_eq!(board.milestones.len(), 1);
        assert!(board.milestones[0].tasks.is_empty());
    }

    #[test]
    fn a_task_without_a_milestone_is_standalone_and_loads_with_its_subtasks() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("M", "goal", None).unwrap();
        let led = store.add_task(m, "led").unwrap();
        let a = store
            .add_standalone_task(" Sort the shelves ", None)
            .unwrap();
        let b = store
            .add_standalone_task("Repaint the shop sign", None)
            .unwrap();
        let s1 = store.add_subtask(a, "sand the shelf").unwrap();
        store.add_subtask(a, "label the shelf").unwrap();
        store.set_task_stage(s1, Stage::Done).unwrap();
        store.add_criterion(a, "Every shelf labelled").unwrap();
        store.add_blocker(a, led).unwrap();
        // The same checks as a task under a milestone.
        let err = store.add_standalone_task("two\nlines", None).unwrap_err();
        assert!(
            err.to_string().starts_with("the task title contains"),
            "{err}"
        );
        assert_eq!(
            store
                .add_standalone_task(" ", None)
                .unwrap_err()
                .to_string(),
            "the task title is empty"
        );

        let board = store.load().unwrap();
        assert_eq!(board.milestones[0].tasks.len(), 1);
        let ids: Vec<TaskId> = board.standalone.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a, b]);
        let t = &board.standalone[0];
        assert_eq!(t.title, "Sort the shelves");
        assert_eq!((t.done_subtasks(), t.subtasks.len()), (1, 2));
        assert_eq!(t.details.criteria.len(), 1);
        assert_eq!(t.details.blocked_by, vec![led]);
        assert!(board.standalone[1].subtasks.is_empty());
        // A standalone task is Done only once its subtasks are, as any task.
        assert!(store.set_task_stage(a, Stage::Done).is_err());
        assert_eq!(store.set_task_stage(b, Stage::Done).unwrap(), None);
    }

    #[test]
    fn handing_a_standalone_task_moves_it_and_its_subtasks_under_a_led_milestone() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let led = store.add_milestone("Led", "goal", None).unwrap();
        let unled = store.add_milestone("Unled", "goal", None).unwrap();
        store
            .lead_milestone(led, "Kids Lead", "kids-lead", None)
            .unwrap();
        let done = store.add_task(led, "done one").unwrap();
        store.set_task_stage(done, Stage::Done).unwrap();
        store.mark_milestone_done(led).unwrap();
        let loose = store.add_standalone_task("loose", None).unwrap();
        let sub = store.add_subtask(loose, "step").unwrap();
        store.set_task_stage(sub, Stage::Done).unwrap();
        store.set_task_stage(loose, Stage::Done).unwrap();

        let cases = [
            (TaskId(99), led, "there is no task #99 on the board"),
            (
                sub,
                led,
                "#3 is a subtask of #2; hand its task to a lead instead",
            ),
            (
                done,
                led,
                "#1 is already under M1; only a standalone task is handed to a lead",
            ),
            (
                loose,
                MilestoneId(9),
                "there is no milestone M9 on the board",
            ),
            (
                loose,
                unled,
                "M2 has no lead yet, so there is nobody to hand #2 to; it is in no repo yet: \
                 tower milestone set M2 --repo R<n> first, then tower lead add M2",
            ),
        ];
        for (task, m, want) in cases {
            let err = store.hand_task(task, m).unwrap_err();
            assert_eq!(err.to_string(), want);
        }
        // Nothing moved, and the receiver kept its Done mark.
        let board = store.load().unwrap();
        assert_eq!(board.standalone.len(), 1);
        assert_eq!(board.milestones[0].stage(), Stage::Done);

        assert_eq!(store.hand_task(loose, led).unwrap().name, "Kids Lead");
        let board = store.load().unwrap();
        assert!(board.standalone.is_empty());
        let m = &board.milestones[0];
        let ids: Vec<TaskId> = m.tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![done, loose]);
        assert_eq!(m.tasks[1].subtasks.len(), 1);
        // The task set changed, so the Done mark is gone though every task is Done.
        assert!(!m.marked_done);
        assert_eq!(m.stage(), Stage::InProgress);
        // A subtask's own milestone stays empty, as the schema requires.
        assert_eq!(store.parent_of(sub).unwrap(), Some(loose));
        let err = store.hand_task(loose, led).unwrap_err();
        assert_eq!(
            err.to_string(),
            "#2 is already under M1; only a standalone task is handed to a lead"
        );
    }

    #[test]
    fn concurrent_writers_on_a_file_all_succeed() {
        const WRITERS: usize = 8;
        const EACH: usize = 25;
        let (_dir, path) = temp_board();
        let path = Arc::new(path);
        let start = Arc::new(Barrier::new(WRITERS));
        // Each thread opens its own connection, like a separate agent process, on a board
        // that does not exist yet, so first-use creation races too.
        let handles: Vec<_> = (0..WRITERS)
            .map(|w| {
                let (path, start) = (Arc::clone(&path), Arc::clone(&start));
                std::thread::spawn(move || -> Result<()> {
                    start.wait();
                    let mut store = Store::open(&path)?;
                    let m = store.add_milestone(&format!("writer {w}"), "goal", None)?;
                    for i in 0..EACH {
                        store.add_task(m, &format!("task {i}"))?;
                        store.load()?;
                    }
                    Ok(())
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let board = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(board.milestones.len(), WRITERS);
        assert!(board.milestones.iter().all(|m| m.tasks.len() == EACH));
    }

    #[test]
    fn refuses_a_board_from_a_newer_tower() {
        let (_dir, path) = temp_board();
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(conn);
        let err = format!("{:#}", Store::open(&path).err().unwrap());
        let newer = SCHEMA_VERSION + 1;
        assert!(
            err.contains(&format!(
                "schema version {newer}, newer than this tower understands ({SCHEMA_VERSION})"
            )),
            "{err}"
        );
    }

    #[test]
    fn a_newer_board_is_refused_before_its_journal_mode_is_touched() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        let newer = SCHEMA_VERSION + 1;
        conn.execute_batch(&format!(
            "CREATE TABLE future (x); PRAGMA user_version = {newer};"
        ))
        .unwrap();
        drop(conn);
        let err = format!("{:#}", Store::open(&path).err().unwrap());
        assert!(
            err.contains(&format!("schema version {newer}, newer than this tower")),
            "{err}"
        );
        let conn = Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, newer);
    }

    /// A board at schema version 13: two leads, each on its milestone (lead 1 on M2, lead 2
    /// on M1, so a lead's id is never its milestone's) with threads with
    /// and without a task, one resolved; a milestone with no lead, with a task; a
    /// standalone task with a subtask; delivery records keyed by milestone; a watch flag
    /// and a read mark.
    fn version_13_board(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        for sql in &MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('MVP', 'g'), ('Checkout', 'g'), ('Later', 'g');
             INSERT INTO lead (milestone_id, name, session, colour, socket, claude_session)
                 VALUES (2, 'Checkout Lead', 'checkout-lead', 'teal', NULL, NULL),
                        (1, 'Catalogue', 'cat-lead', 'blue', '/tmp/s.sock', 'sess-1');
             INSERT INTO task (milestone_id, title, assignee) VALUES (1, 'Reviews', 'dev-1');
             INSERT INTO task (milestone_id, title) VALUES (2, 'Basket'), (3, 'Someday');
             INSERT INTO task (milestone_id, title) VALUES (NULL, 'Loose');
             INSERT INTO task (parent_id, title) VALUES (4, 'Loose part');
             INSERT INTO thread (lead_id, task_id, title) VALUES (2, 1, 'Reviews team');
             INSERT INTO thread (lead_id, task_id, title) VALUES (2, NULL, 'Cover images');
             INSERT INTO thread (lead_id, task_id, title, resolved_by, resolved_at)
                 VALUES (1, 2, 'Basket team', 'qa', 9);
             INSERT INTO thread (lead_id, task_id, title) VALUES (1, NULL, 'Colours');
             INSERT INTO message (thread_id, author, text, posted_at, progress)
                 VALUES (1, 'dev-1', 'building', 1, 1), (1, 'cat-lead', 'copy', 2, 0),
                        (2, 'dev-1', 'question: @manager need the covers', 3, 0),
                        (3, 'dev-2', 'done', 4, 0), (3, 'qa', 'fine', 9, 0),
                        (4, 'checkout-lead', 'decision: teal @manager', 5, 0);
             UPDATE message SET read_at = 6 WHERE id = 6;
             UPDATE delivery SET upto = 4;
             INSERT INTO delivery_sent (recipient, upto) VALUES ('lead:M1', 5), ('dev-1', 5);
             INSERT INTO delivery_fail (recipient, since) VALUES ('lead:M2', 7), ('dev-9', 7);
             INSERT INTO watch_flag (task_id, signal, worker, raised_at) VALUES (1, 'silent', 'dev-1', 8);
             PRAGMA user_version = 13;",
        )
        .unwrap();
    }

    #[test]
    fn migrates_a_version_13_board_to_one_lead_per_repo_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        version_13_board(&path);
        let mut store = Store::open(&path).unwrap();
        let raw = Connection::open(&path).unwrap();
        let rows = |sql: &str| -> Vec<Vec<Option<String>>> {
            let mut stmt = raw.prepare(sql).unwrap();
            let n = stmt.column_count();
            stmt.query_map([], |r| {
                (0..n)
                    .map(|i| {
                        r.get::<_, rusqlite::types::Value>(i).map(|v| match v {
                            rusqlite::types::Value::Null => None,
                            rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                            rusqlite::types::Value::Text(t) => Some(t),
                            other => Some(format!("{other:?}")),
                        })
                    })
                    .collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
        };
        let s = |v: &[Option<&str>]| -> Vec<Option<String>> {
            v.iter().map(|x| x.map(str::to_string)).collect()
        };
        // A repo per lead, by the lead's id, with no path or link yet.
        assert_eq!(
            rows("SELECT id, path, link FROM repo ORDER BY id"),
            [s(&[Some("1"), None, None]), s(&[Some("2"), None, None])]
        );
        // Each lead's milestone joins its repo; the lead-less one stays unassigned.
        assert_eq!(
            rows("SELECT id, repo_id FROM milestone ORDER BY id"),
            [
                s(&[Some("1"), Some("2")]),
                s(&[Some("2"), Some("1")]),
                s(&[Some("3"), None])
            ]
        );
        // Leads keep their ids, names, sessions, colours and Claude sessions, now on a repo;
        // their inbox sockets go, and each is told of posts from the newest on.
        assert_eq!(
            rows(
                "SELECT id, repo_id, name, session, colour, claude_session, told_upto, \
                 deaf_thread FROM lead"
            ),
            [
                s(&[
                    Some("1"),
                    Some("1"),
                    Some("Checkout Lead"),
                    Some("checkout-lead"),
                    Some("teal"),
                    None,
                    Some("6"),
                    None
                ]),
                s(&[
                    Some("2"),
                    Some("2"),
                    Some("Catalogue"),
                    Some("cat-lead"),
                    Some("blue"),
                    Some("sess-1"),
                    Some("6"),
                    None
                ]),
            ]
        );
        // Threads keep their ids, titles, tasks and resolutions on their lead's repo; one
        // with no task is about the lead's milestone. None stores a repo: each reads it
        // through its milestone or task.
        assert_eq!(
            rows("SELECT count(*) FROM thread WHERE repo_id IS NOT NULL"),
            [s(&[Some("0")])]
        );
        assert_eq!(
            rows(
                "SELECT t.id, b.repo_id, t.milestone_id, t.task_id, t.title, t.resolved_by, \
                 t.resolved_at FROM thread t JOIN thread_board b ON b.id = t.id ORDER BY t.id"
            ),
            [
                s(&[
                    Some("1"),
                    Some("2"),
                    None,
                    Some("1"),
                    Some("Reviews team"),
                    None,
                    None
                ]),
                s(&[
                    Some("2"),
                    Some("2"),
                    Some("1"),
                    None,
                    Some("Cover images"),
                    None,
                    None
                ]),
                s(&[
                    Some("3"),
                    Some("1"),
                    None,
                    Some("2"),
                    Some("Basket team"),
                    Some("qa"),
                    Some("9")
                ]),
                s(&[
                    Some("4"),
                    Some("1"),
                    Some("2"),
                    None,
                    Some("Colours"),
                    None,
                    None
                ]),
            ]
        );
        // No task carries a repo: milestone tasks and the subtask take theirs from their
        // milestone, and the standalone task is unassigned.
        assert_eq!(
            rows("SELECT count(*) FROM task WHERE repo_id IS NOT NULL"),
            [s(&[Some("0")])]
        );
        assert_eq!(rows("SELECT count(*) FROM task"), [s(&[Some("5")])]);
        // Messages, read marks, the cursor and watch flags are untouched; workers' delivery
        // records stay, and leads' go with lead delivery.
        assert_eq!(
            rows("SELECT count(*), sum(progress), max(read_at) FROM message"),
            [s(&[Some("6"), Some("1"), Some("6")])]
        );
        assert_eq!(rows("SELECT upto FROM delivery"), [s(&[Some("4")])]);
        assert_eq!(
            rows("SELECT recipient, upto FROM delivery_sent ORDER BY recipient"),
            [s(&[Some("dev-1"), Some("5")])]
        );
        assert_eq!(
            rows("SELECT recipient, since FROM delivery_fail ORDER BY recipient"),
            [s(&[Some("dev-9"), Some("7")])]
        );
        assert_eq!(rows("SELECT count(*) FROM watch_flag"), [s(&[Some("1")])]);
        assert_eq!(
            rows("PRAGMA foreign_key_check"),
            Vec::<Vec<Option<String>>>::new()
        );
        // The store turns foreign keys back on once it has migrated.
        let on: bool = (store.conn)
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert!(on, "foreign keys left off after the migration");

        // The board reads as it did: each milestone with its lead and board, the lead-less
        // one with neither, the inbox the same.
        let board = store.load().unwrap();
        let shape: Vec<(i64, Option<&str>, Vec<i64>)> = (board.milestones.iter())
            .map(|m| {
                let lead = m.lead.as_ref().map(|l| l.session.as_str());
                (m.id.0, lead, m.threads.iter().map(|t| t.id.0).collect())
            })
            .collect();
        assert_eq!(
            shape,
            [
                (1, Some("cat-lead"), vec![1, 2]),
                (2, Some("checkout-lead"), vec![3, 4]),
                (3, None, vec![])
            ]
        );
        assert_eq!(board.standalone.len(), 1);
        assert_eq!(board.standalone[0].subtasks.len(), 1);
        assert_eq!(
            board.milestones[0].tasks[0]
                .details
                .doing
                .as_ref()
                .unwrap()
                .text,
            "building"
        );
        assert_eq!(board.inbox.need_you.len(), 1);
        // The lead attached before stays attached, on its repo.
        let leads = store.attached_leads().unwrap();
        assert_eq!(
            (leads.iter())
                .map(|l| (
                    l.repo,
                    l.lead.session.as_str(),
                    l.claude_session.as_deref().unwrap()
                ))
                .collect::<Vec<_>>(),
            [(RepoId(2), "cat-lead", "sess-1")]
        );
        store.post(ThreadId(2), "dev-1", "after").unwrap();
        let pending = store.take_pending().unwrap();
        assert_eq!(
            (pending.posts.iter())
                .map(|p| (p.id, p.repo))
                .collect::<Vec<_>>(),
            [(5, RepoId(1)), (6, RepoId(1)), (7, RepoId(2))]
        );
        // New ids follow on from the old ones.
        let (t, repo) = store
            .open_thread(
                &BoardOf::Session("checkout-lead".into()),
                "checkout-lead",
                None,
                "Next",
                "go",
            )
            .unwrap();
        assert_eq!((t, repo), (ThreadId(5), RepoId(1)));
        // The lead-less milestone joins a repo with tower milestone set --repo, and then has
        // that repo's lead.
        let fields = MilestoneFields {
            repo: Some(RepoId(1)),
            ..Default::default()
        };
        store.set_milestone(MilestoneId(3), fields).unwrap();
        assert_eq!(
            lead_of_board(&store.load().unwrap(), MilestoneId(3))
                .unwrap()
                .session,
            "checkout-lead"
        );
        // Migrating again does nothing.
        drop(store);
        let again = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(again.milestones[0].threads.len(), 2);
    }

    /// A schema 13 board whose threads link tasks that would land in no repo: lead 1 (on
    /// M2) has a thread on a standalone task, lead 2 (on M1) a thread on a task under the
    /// lead-less M3; `extra` runs after, before the version is set.
    fn version_13_board_with_stray_links(path: &Path, extra: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        for sql in &MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(&format!(
            "INSERT INTO milestone (title, goal) VALUES ('MVP', 'g'), ('Checkout', 'g'), ('Later', 'g');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (2, 'Checkout Lead', 'checkout-lead', 'teal'), (1, 'Catalogue', 'cat-lead', 'blue');
             INSERT INTO task (milestone_id, title) VALUES (NULL, 'Loose'), (3, 'Someday');
             INSERT INTO thread (lead_id, task_id, title) VALUES (1, 1, 'Loose team');
             INSERT INTO thread (lead_id, task_id, title) VALUES (2, 2, 'Someday team');
             INSERT INTO message (thread_id, author, text, posted_at)
                 VALUES (1, 'dev-1', 'question: @manager loose', 1), (2, 'dev-2', 'someday', 2);
             {extra}
             PRAGMA user_version = 13;"
        ))
        .unwrap();
    }

    #[test]
    fn the_migration_gives_a_linked_task_with_no_repo_the_repo_of_its_threads_lead() {
        let (_dir, path) = temp_board();
        version_13_board_with_stray_links(&path, "");
        let mut store = Store::open(&path).unwrap();
        // Lead 1 is on M2, so its repo is R1; lead 2's is R2. The standalone task joins R1;
        // the lead-less M3 joins R2, with its task.
        let raw = Connection::open(&path).unwrap();
        let task_repo: Option<i64> = raw
            .query_row("SELECT repo_id FROM task WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        let m3_repo: Option<i64> = raw
            .query_row("SELECT repo_id FROM milestone WHERE id = 3", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!((task_repo, m3_repo), (Some(1), Some(2)));
        assert_eq!(store.thread_place(ThreadId(1)).unwrap(), (RepoId(1), None));
        assert_eq!(store.thread_place(ThreadId(2)).unwrap(), (RepoId(2), None));
        let board = store.load().unwrap();
        assert_eq!(
            board.milestones[2].lead.as_ref().unwrap().session,
            "cat-lead"
        );
        assert_eq!(board.inbox.need_you.len(), 1);
        assert_eq!(store.take_pending().unwrap().posts.len(), 2);
        // A board broken by hand after the migration is refused with the thread named.
        store.exec_sql("UPDATE task SET repo_id = NULL WHERE id = 1");
        let err = format!("{:#}", store.load().unwrap_err());
        assert!(err.contains("T1 is on no repo's board"), "{err}");
        let err = format!("{:#}", store.take_pending().unwrap_err());
        assert!(err.contains("T1 is on no repo's board"), "{err}");
    }

    #[test]
    fn the_migration_refuses_naming_them_threads_it_cannot_put_on_a_board() {
        let (_dir, path) = temp_board();
        // A subtask link, which no tower allowed, has no repo to give.
        version_13_board_with_stray_links(
            &path,
            "INSERT INTO task (parent_id, title) VALUES (1, 'part');
             INSERT INTO thread (lead_id, task_id, title) VALUES (1, 3, 'Part team');",
        );
        let err = format!("{:#}", Store::open(&path).err().unwrap());
        assert!(
            err.contains("left threads on no repo's board: T3; a thread links to a task"),
            "{err}"
        );
        // Nothing changed: the board is still at schema 13.
        let version: i32 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 13);
    }

    #[test]
    fn a_repo_has_one_lead_and_a_replaced_lead_keeps_the_repos_threads() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store.add_milestone("a", "g", None).unwrap();
        store.lead_milestone(m1, "First", "first", None).unwrap();
        let m2 = store.add_milestone("b", "g", Some(RepoId(1))).unwrap();
        // A second lead on the repo is refused, named by the repo or by either milestone.
        for of in [
            RepoOf::Repo(RepoId(1)),
            RepoOf::Milestone(m1),
            RepoOf::Milestone(m2),
        ] {
            let err = store.add_lead(of, "Second", "second", None).unwrap_err();
            assert_eq!(
                err.to_string(),
                "R1 already has a lead, First; a repo has one lead"
            );
        }
        let (t, _) = store
            .open_thread(&BoardOf::Milestone(m1), "first", None, "Plan", "go")
            .unwrap();
        store.add_task(m2, "x").unwrap();
        let (t2, _) = store
            .open_thread(
                &BoardOf::Milestone(m2),
                "first",
                Some(TaskId(1)),
                "x team",
                "go",
            )
            .unwrap();
        // Another session takes over the repo: the board stays, and reaches the new lead.
        // A regression check only: set_lead changes the same lead row, so this would pass
        // with threads keyed by lead too; the schema (threads hold no lead) is the evidence.
        store
            .set_lead(
                RepoOf::Repo(RepoId(1)),
                Some("Second"),
                Some("second"),
                None,
            )
            .unwrap();
        let board = store.load().unwrap();
        for m in &board.milestones {
            assert_eq!(m.lead.as_ref().unwrap().session, "second");
            assert_eq!(m.threads.iter().map(|t| t.id).collect::<Vec<_>>(), [t, t2]);
        }
        store.post(t, "dev-1", "still here").unwrap();
        let (thread, _) = store
            .open_thread(
                &BoardOf::Session("second".into()),
                "second",
                None,
                "Triage",
                "go",
            )
            .unwrap();
        assert_eq!(store.thread(thread).unwrap().task, None);
        assert_eq!(store.load().unwrap().milestones[0].threads.len(), 3);
        assert_eq!(store.repo(RepoId(1)).unwrap().threads.len(), 3);
    }

    #[test]
    fn the_schema_links_a_thread_to_exactly_one_of_repo_milestone_or_task_never_a_subtask() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "g", None).unwrap();
        store.lead_milestone(m, "One", "one", None).unwrap();
        let task = store.add_task(m, "t").unwrap();
        store.add_subtask(task, "s").unwrap();
        let raw = Connection::open(&path).unwrap();
        let insert = |repo: Option<i64>, milestone: Option<i64>, task: Option<i64>| {
            raw.execute(
                "INSERT INTO thread (repo_id, milestone_id, task_id, title) VALUES (?1, ?2, ?3, 't')",
                params![repo, milestone, task],
            )
            .map_err(|e| e.to_string())
        };
        // Two links, as in a thread on R1 about a milestone of another repo, or none.
        for (r, m, t) in [
            (Some(1), Some(1), None),
            (Some(1), None, Some(1)),
            (None, Some(1), Some(1)),
            (None, None, None),
        ] {
            let err = insert(r, m, t).unwrap_err();
            assert!(err.contains("CHECK constraint failed"), "{err}");
        }
        let err = insert(None, None, Some(2)).unwrap_err();
        assert!(
            err.contains("a thread links to a task, never a subtask"),
            "{err}"
        );
        insert(None, None, Some(1)).unwrap();
        let err = raw
            .execute("UPDATE thread SET task_id = 2 WHERE id = 1", [])
            .unwrap_err()
            .to_string();
        assert!(err.contains("never a subtask"), "{err}");
        // Each of the three ways reads its repo through the view.
        insert(Some(1), None, None).unwrap();
        insert(None, Some(1), None).unwrap();
        let repos: Vec<i64> = raw
            .prepare("SELECT repo_id FROM thread_board ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(repos, [1, 1, 1]);
    }

    #[test]
    fn each_owner_item_counts_once_on_its_repo_not_on_every_copy_of_the_repo_board() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store.add_milestone("one", "g", None).unwrap();
        store.lead_milestone(m1, "Lead", "lead", None).unwrap();
        let [m2, m3] =
            ["two", "three"].map(|t| store.add_milestone(t, "g", Some(RepoId(1))).unwrap());
        let in_m3 = store.add_task(m3, "t").unwrap();
        let loose = store.add_standalone_task("loose", Some(RepoId(1))).unwrap();
        let ask = |store: &mut Store, of: BoardOf, task| {
            store
                .open_thread(&of, "dev-1", task, "q", "question: @manager which way?")
                .unwrap()
                .0
        };
        let about_m2 = ask(&mut store, BoardOf::Milestone(m2), None);
        let about_task = ask(&mut store, BoardOf::Milestone(m1), Some(in_m3));
        let repo_wide = ask(&mut store, BoardOf::Session("lead".into()), None);
        let standalone = ask(&mut store, BoardOf::Session("lead".into()), Some(loose));
        let board = store.load().unwrap();
        // Every milestone carries the whole board, yet each item counts once, on its repo:
        // the badges are the repo row's.
        assert!(board.milestones.iter().all(|m| m.threads.len() == 4));
        let items: Vec<(ThreadId, RepoId)> = (board.inbox.need_you.iter())
            .map(|a| (a.thread, a.repo))
            .rev()
            .collect();
        let r1 = RepoId(1);
        assert_eq!(
            items,
            [
                (about_m2, r1),
                (about_task, r1),
                (repo_wide, r1),
                (standalone, r1)
            ]
        );
        assert_eq!(board.inbox.badges(r1), (4, 0));
        assert_eq!(board.inbox.badges(RepoId(2)), (0, 0));
    }

    #[test]
    fn a_milestone_moving_repo_takes_its_threads_and_tasks_threads_along() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2, m3] = ["a", "b", "c"].map(|t| store.add_milestone(t, "g", None).unwrap());
        store.lead_milestone(m1, "One", "one", None).unwrap();
        store.lead_milestone(m2, "Two", "two", None).unwrap();
        store.exec_sql("UPDATE milestone SET repo_id = 1 WHERE id = 3");
        let task = store.add_task(m3, "t").unwrap();
        let open = |store: &mut Store, of: BoardOf, task| {
            store.open_thread(&of, "one", task, "t", "m").unwrap().0
        };
        let about_m3 = open(&mut store, BoardOf::Milestone(m3), None);
        let about_task = open(&mut store, BoardOf::Milestone(m1), Some(task));
        let about_m1 = open(&mut store, BoardOf::Milestone(m1), None);
        let repo_wide = open(&mut store, BoardOf::Session("one".into()), None);
        let fields = MilestoneFields {
            repo: Some(RepoId(2)),
            ..Default::default()
        };
        store.set_milestone(m3, fields).unwrap();
        let board = store.load().unwrap();
        let ids = |m: usize| {
            board.milestones[m]
                .threads
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(0), [about_m1, repo_wide]);
        assert_eq!(ids(1), [about_m3, about_task]);
        assert_eq!(ids(2), [about_m3, about_task]);
        assert_eq!(board.milestones[2].lead.as_ref().unwrap().session, "two");
        // A repo not on the board is refused, changing nothing.
        let fields = MilestoneFields {
            repo: Some(RepoId(99)),
            ..Default::default()
        };
        let err = store.set_milestone(m3, fields).unwrap_err();
        assert_eq!(err.to_string(), "there is no repo R99 on the board");
    }

    #[test]
    fn a_repo_is_a_main_checkout_named_once() {
        let (dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let checkout = dir.path().join("repo");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere").unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let link = Some("https://github.com/o/r");
        let r = store.add_repo(&checkout, link).unwrap();
        assert_eq!(r, RepoId(1));
        let real = checkout.canonicalize().unwrap();
        let shown = store.repo(r).unwrap();
        assert_eq!(shown.path.as_deref(), real.to_str());
        assert_eq!(shown.link.as_deref(), link);
        assert_eq!((shown.lead, shown.threads), (None, vec![]));
        let cases: [(&Path, Option<&str>, String); 5] = [
            (
                Path::new("rel/repo"),
                None,
                "the repo path rel/repo is not absolute".into(),
            ),
            (
                &worktree,
                None,
                format!(
                    "{} is a linked worktree; give the repo's main checkout",
                    worktree.display()
                ),
            ),
            (
                &plain,
                None,
                format!(
                    "{} is not a git main checkout: it has no .git directory",
                    plain.display()
                ),
            ),
            (&checkout, None, format!("{} is already R1", real.display())),
            (&plain, Some("ftp://x"), String::new()),
        ];
        for (p, l, want) in cases {
            let err = store.add_repo(p, l).unwrap_err().to_string();
            assert!(err.starts_with(&want), "{err}");
        }
        // Another path for the same checkout, through a link, is the same repo.
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&checkout, &alias).unwrap();
        let err = store.add_repo(&alias, None).unwrap_err().to_string();
        assert_eq!(err, format!("{} is already R1", real.display()));
        // A migrated repo gets its path; one taken by another repo is refused.
        store.exec_sql("INSERT INTO repo (path) VALUES (NULL)");
        let err = store
            .set_repo(RepoId(2), Some(&checkout), None)
            .unwrap_err();
        assert_eq!(err.to_string(), format!("{} is already R1", real.display()));
        let other = dir.path().join("other");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        store.set_repo(RepoId(2), Some(&other), None).unwrap();
        store.set_repo(RepoId(1), Some(&checkout), None).unwrap();
        assert_eq!(
            store.repo(RepoId(2)).unwrap().path.as_deref(),
            other.canonicalize().unwrap().to_str()
        );
        let err = store.set_repo(RepoId(2), None, None).unwrap_err();
        assert_eq!(err.to_string(), "nothing to change: give --path or --link");
        let err = store.set_repo(RepoId(7), None, link).unwrap_err();
        assert_eq!(err.to_string(), "there is no repo R7 on the board");
    }

    /// A task's forge ticket is its own repo's, so one repo's work is never filed under
    /// another's (#142); any other link goes anywhere.
    #[test]
    fn a_tasks_forge_link_is_refused_outside_its_repo() {
        let (dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let repo = |store: &mut Store, name: &str, link: Option<&str>| {
            let checkout = dir.path().join(name);
            std::fs::create_dir_all(checkout.join(".git")).unwrap();
            store.add_repo(&checkout, link).unwrap()
        };
        let shop_link = "https://github.com/example/bookshop.git";
        let web_link = "https://gitlab.example.com/acme/shop/web";
        let shop = repo(&mut store, "shop", Some(shop_link));
        let web = repo(&mut store, "web", Some(web_link));
        let bare = repo(&mut store, "bare", None);
        let m1 = store.add_milestone("Shelves", "g", Some(shop)).unwrap();
        store.lead_milestone(m1, "One", "one", None).unwrap();
        let on_m1 = store.add_task(m1, "Shelf").unwrap();
        let on_web = store.add_standalone_task("Web", Some(web)).unwrap();
        let on_bare = store.add_standalone_task("Bare", Some(bare)).unwrap();
        let loose = store.add_standalone_task("Loose", None).unwrap();
        let set = |store: &mut Store, task: TaskId, link: Option<&str>, repo: Option<RepoId>| {
            let fields = TaskFields {
                description: Some("set with it"),
                link,
                repo,
                ..TaskFields::default()
            };
            store
                .set_task(task, fields)
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        let link_of = |store: &mut Store, task: TaskId| store.task(task).unwrap().details.link;
        let web_mr = "https://gitlab.example.com/acme/shop/web/-/merge_requests/27";
        let jira = "https://acme.atlassian.net/browse/SHOP-12";
        // The repo's own issue and pull request, in any case, and another tracker's link,
        // even one shaped like a forge's path: a link is a forge's only on a forge host the
        // board knows, so a self-hosted forge no repo links to is another tracker too.
        let sentry = "https://sentry.io/organizations/acme/issues/4412873/";
        let elsewhere = "https://git.acme.example/acme/shop/-/issues/9";
        for (task, link) in [
            (on_m1, "https://github.com/Example/Bookshop/issues/42"),
            (on_m1, "https://github.com/example/bookshop/pull/58"),
            (
                on_web,
                "https://gitlab.example.com/acme/shop/web/-/issues/5",
            ),
            (on_m1, sentry),
            (on_bare, sentry),
            (on_m1, elsewhere),
            // A GitLab group's work item is in no project, so it is another tracker's too.
            (
                on_web,
                "https://gitlab.example.com/groups/acme/-/work_items/7",
            ),
            (on_m1, jira),
            (on_bare, jira),
            (loose, web_mr),
        ] {
            assert_eq!(set(&mut store, task, Some(link), None), Ok(()), "{link}");
        }
        // Another repo's ticket is refused naming that repo; one of no repo on the board,
        // naming its project, on a repo's host or on github.com or gitlab.com with no repo
        // there; the parent group's ticket, which makes the parent a project, so
        // the subgroup's link no project's address; one in a repo with no forge link, saying
        // to give it one. Nothing is written, the description given with it included.
        for (task, link, want) in [
            (
                on_m1,
                web_mr,
                format!(
                    "{web_mr} belongs to R2 ({web_link}), not to R1 ({shop_link}), the repo \
                     of #1; file it in R2"
                ),
            ),
            (
                on_web,
                "https://gitlab.example.com/acme/catalogue/-/issues/5",
                format!(
                    "https://gitlab.example.com/acme/catalogue/-/issues/5 belongs to \
                     gitlab.example.com/acme/catalogue, which is no repo on the board, not to \
                     R2 ({web_link}), the repo of #2"
                ),
            ),
            (
                on_m1,
                "https://gitlab.com/acme/other/-/issues/3",
                format!(
                    "https://gitlab.com/acme/other/-/issues/3 belongs to gitlab.com/acme/other, \
                     which is no repo on the board, not to R1 ({shop_link}), the repo of #1"
                ),
            ),
            (
                on_web,
                "https://gitlab.example.com/acme/shop/-/issues/5",
                format!(
                    "R2's link ({web_link}) is not a project's address; set it to the \
                     project: tower repo set R2 --link <project URL>"
                ),
            ),
            (
                on_bare,
                "https://github.com/example/bookshop/issues/42",
                "R3, the repo of #3, has no forge link, so tower cannot tell whose \
                 https://github.com/example/bookshop/issues/42 is; set it first: tower repo \
                 set R3 --link <forge URL>"
                    .to_string(),
            ),
        ] {
            let before = store.task(task).unwrap().details;
            assert_eq!(set(&mut store, task, Some(link), None), Err(want), "{link}");
            assert_eq!(store.task(task).unwrap().details, before, "{link}");
        }
        // A task in no repo keeps its link until it gets a repo, which must be the link's.
        let err = set(&mut store, loose, None, Some(shop)).unwrap_err();
        assert!(err.starts_with(&format!("{web_mr} belongs to R2")), "{err}");
        assert_eq!(store.task(loose).unwrap().repo, None);
        let err = store.hand_task(loose, m1).unwrap_err().to_string();
        assert!(err.starts_with(&format!("{web_mr} belongs to R2")), "{err}");
        assert_eq!(store.task(loose).unwrap().repo, None);
        assert_eq!(milestone_of(&mut store, m1).tasks.len(), 1);
        assert_eq!(set(&mut store, loose, None, Some(web)), Ok(()));
        assert_eq!(store.task(loose).unwrap().repo, Some(web));
        assert_eq!(link_of(&mut store, loose).as_deref(), Some(web_mr));
        // A repo link past its project's address says so, not that the ticket is no repo's.
        let docs = repo(
            &mut store,
            "docs",
            Some("https://github.com/example/docs/tree/main"),
        );
        let on_docs = store.add_standalone_task("Docs", Some(docs)).unwrap();
        let docs_issue = "https://github.com/example/docs/issues/4";
        assert_eq!(
            set(&mut store, on_docs, Some(docs_issue), None),
            Err(
                "R4's link (https://github.com/example/docs/tree/main) is not a project's \
                 address; set it to the project: tower repo set R4 --link <project URL>"
                    .to_string()
            )
        );
        // A task added with another repo's ticket is never added, so it cannot sit under the
        // wrong milestone; with its own repo's, it is added with it.
        let tasks = milestone_of(&mut store, m1).tasks.len();
        let err = (store.insert_task(Some(m1), None, "Misfiled", Some(web_mr)))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "{web_mr} belongs to R2 ({web_link}), not to R1 ({shop_link}), the repo of M1; \
                 file it in R2"
            )
        );
        assert_eq!(milestone_of(&mut store, m1).tasks.len(), tasks);
        let shop_issue = "https://github.com/example/bookshop/issues/7";
        let err = (store.insert_task(None, Some(bare), "Bare", Some(shop_issue)))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("R3, the repo of the new task, has no forge link"),
            "{err}"
        );
        assert_eq!(store.repo(bare).unwrap().standalone.len(), 1);
        let filed = (store.insert_task(Some(m1), None, "Filed", Some(shop_issue))).unwrap();
        let crash = (store.insert_task(Some(m1), None, "Crash", Some(sentry))).unwrap();
        assert_eq!(milestone_of(&mut store, m1).tasks.len(), tasks + 2);
        assert_eq!(link_of(&mut store, crash).as_deref(), Some(sentry));
        assert_eq!(link_of(&mut store, filed).as_deref(), Some(shop_issue));
        // A milestone given a repo takes its tasks along, so each linked task's link is
        // checked against it: a refusal moves nothing, the title given with it included.
        let m2 = store.add_milestone("Loose shelves", "g", None).unwrap();
        let on_m2 = (store.insert_task(Some(m2), None, "Web work", Some(web_mr))).unwrap();
        let move_to = |store: &mut Store, repo: RepoId| {
            let fields = MilestoneFields {
                title: Some("Moved"),
                repo: Some(repo),
                ..MilestoneFields::default()
            };
            store
                .set_milestone(m2, fields)
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            move_to(&mut store, shop),
            Err(format!(
                "{web_mr} belongs to R2 ({web_link}), not to R1 ({shop_link}), the repo of \
                 {on_m2}; file it in R2"
            ))
        );
        let m = milestone_of(&mut store, m2);
        assert_eq!((m.repo, m.title.as_str()), (None, "Loose shelves"));
        assert_eq!(move_to(&mut store, web), Ok(()));
        let m = milestone_of(&mut store, m2);
        assert_eq!((m.repo, m.title.as_str()), (Some(web), "Moved"));
        // A change that sets neither link nor repo leaves a stored link alone.
        store.exec_sql(&format!(
            "UPDATE task SET link = '{web_mr}' WHERE id = {}",
            on_m1.0
        ));
        let fields = TaskFields {
            assignee: Some("dev-1"),
            ..TaskFields::default()
        };
        store.set_task(on_m1, fields).unwrap();
    }

    #[test]
    fn standalone_tasks_belong_to_a_repo_and_are_handed_only_within_it() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2] = ["a", "b"].map(|t| store.add_milestone(t, "g", None).unwrap());
        store.lead_milestone(m1, "One", "one", None).unwrap();
        store.lead_milestone(m2, "Two", "two", None).unwrap();
        let in_r1 = store
            .add_standalone_task("r1 task", Some(RepoId(1)))
            .unwrap();
        let loose = store.add_standalone_task("loose", None).unwrap();
        let err = store
            .add_standalone_task("x", Some(RepoId(99)))
            .unwrap_err();
        assert_eq!(err.to_string(), "there is no repo R99 on the board");
        // A thread on its repo's board can link to it; another repo's cannot.
        let (t, _) = store
            .open_thread(
                &BoardOf::Session("one".into()),
                "one",
                Some(in_r1),
                "t",
                "m",
            )
            .unwrap();
        let err = store
            .open_thread(
                &BoardOf::Session("two".into()),
                "two",
                Some(in_r1),
                "t",
                "m",
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "#1 is in R1, not R2; a thread links to a task of its own repo"
        );
        let err = store.hand_task(in_r1, m2).unwrap_err();
        assert_eq!(
            err.to_string(),
            "#1 is in R1, and M2 is not; hand it to a milestone of its own repo"
        );
        store.hand_task(in_r1, m1).unwrap();
        store.hand_task(loose, m2).unwrap();
        assert_eq!(store.repo(RepoId(1)).unwrap().standalone, []);
        let board = store.load().unwrap();
        assert_eq!(board.milestones[0].tasks[0].id, in_r1);
        assert_eq!(board.milestones[0].threads[0].id, t);
    }

    #[test]
    fn the_delivery_migration_starts_after_the_newest_message() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..12] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('m', 'g');
             INSERT INTO lead (milestone_id, name, session, colour) VALUES (1, 'L', 'l', 'blue');
             INSERT INTO thread (lead_id, title) VALUES (1, 't');
             INSERT INTO message (thread_id, author, text, posted_at) VALUES (1, 'dev', 'a', 1);
             INSERT INTO message (thread_id, author, text, posted_at) VALUES (1, 'dev', 'b', 2);
             PRAGMA user_version = 12;",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&path).unwrap();
        assert!(
            store.take_pending().unwrap().posts.is_empty(),
            "history replayed"
        );
        let leads = store.lead_boards().unwrap();
        assert_eq!(leads.len(), 1);
        assert!(store.attached_leads().unwrap().is_empty());
        store.post(ThreadId(1), "dev", "c").unwrap();
        let pending = store.take_pending().unwrap();
        assert_eq!(pending.posts.len(), 1);
        assert_eq!(
            (pending.posts[0].id, pending.posts[0].text.as_str()),
            (3, "c")
        );
        // Taking moves nothing; the round moves the cursor once it has offered them.
        assert_eq!(store.take_pending().unwrap().posts.len(), 1);
        assert!(store.mark_sent("lead:M1", 3).unwrap());
        assert!(!store.mark_sent("LEAD:m1", 3).unwrap(), "sent twice");
        assert_eq!(store.take_pending().unwrap().sent.get("lead:M1"), Some(&3));
        store.advance_delivery(3).unwrap();
        store.advance_delivery(1).unwrap();
        let after = store.take_pending().unwrap();
        assert!(after.sent.is_empty(), "marks kept at or below the cursor");
        assert!(
            after.posts.is_empty(),
            "the cursor moved back or did not move"
        );
    }

    #[test]
    fn a_new_board_starts_its_delivery_cursor_at_zero_and_episodes_open_once() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("m", "g", None).unwrap();
        store.lead_milestone(m, "L", "l", None).unwrap();
        let (t, _) = store
            .open_thread(&BoardOf::Milestone(m), "dev", None, "t", "first")
            .unwrap();
        assert_eq!(store.take_pending().unwrap().posts.len(), 1);
        let say = |text: &'static str| move |_: &[usize]| text.to_string();
        let failed = store.delivery_failed(&["Dev-One", "qa-one"], t, 5, say("@manager x"));
        assert_eq!(failed.unwrap(), Some((t, vec![0, 1])));
        let failed = store.delivery_failed(&["dev-one", "Qa-One"], t, 6, say("@manager y"));
        assert_eq!(failed.unwrap(), None);
        assert!(store.delivered("Dev-One").unwrap());
        assert!(!store.delivered("dev-one").unwrap());
        let failed = store.delivery_failed(&["qa-one", "dev-one"], t, 7, say("@manager z"));
        assert_eq!(failed.unwrap(), Some((t, vec![1])));
        let texts: Vec<String> = (store.thread(t).unwrap().messages.into_iter())
            .filter(|m| m.author == SYSTEM_AUTHOR)
            .map(|m| m.text)
            .collect();
        assert_eq!(texts, ["@manager x", "@manager z"]);
    }

    fn seeded_board(path: &Path) {
        let mut store = Store::open(path).unwrap();
        let m = store.add_milestone("seeded", "goal", None).unwrap();
        store.add_task(m, "a task").unwrap();
    }

    #[test]
    fn read_only_open_leaves_the_file_bytes_and_mtime_unchanged() {
        let (_dir, path) = temp_board();
        seeded_board(&path);
        let bytes = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        for _ in 0..3 {
            let board = Store::open_read_only(&path).unwrap().load().unwrap();
            assert_eq!(board.milestones[0].tasks.len(), 1);
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }

    #[test]
    fn read_only_open_of_a_missing_board_creates_nothing() {
        let (dir, path) = temp_board();
        let err = format!("{:#}", Store::open_read_only(&path).err().unwrap());
        assert_eq!(
            err,
            format!(
                "no board at {} yet; add a milestone with tower milestone add",
                path.display()
            )
        );
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn read_only_open_of_a_version_zero_file_says_there_is_no_board_yet() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        let err = format!("{:#}", Store::open_read_only(&path).err().unwrap());
        assert!(err.starts_with("no board at "), "{err}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn read_only_open_refuses_newer_and_negative_schemas() {
        for (version, message) in [
            (
                SCHEMA_VERSION + 1,
                format!(
                    "schema version {}, newer than this tower understands ({SCHEMA_VERSION})",
                    SCHEMA_VERSION + 1
                ),
            ),
            (
                -1,
                "schema version -1, which no tower ever wrote".to_string(),
            ),
        ] {
            let (_dir, path) = temp_board();
            seeded_board(&path);
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            drop(conn);
            let err = format!("{:#}", Store::open_read_only(&path).err().unwrap());
            assert!(err.contains(&message), "{err}");
        }
    }

    #[test]
    fn a_long_lock_fails_a_read_instead_of_waiting_forever() {
        let (_dir, path) = temp_board();
        seeded_board(&path);
        let holder = Connection::open(&path).unwrap();
        holder
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; \
                 INSERT INTO milestone (title, goal) VALUES ('held', 'g');",
            )
            .unwrap();
        let started = std::time::Instant::now();
        let result = Store::open_read_only(&path).and_then(|mut s| s.load());
        let err = format!("{:#}", result.err().unwrap());
        assert!(err.contains("database is locked"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn write_errors_name_the_board() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("kept", "goal", None).unwrap();
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("DROP TABLE task").unwrap();
        let want = format!("cannot write to the board {}", path.display());
        let err = format!("{:#}", store.add_task(m, "t").unwrap_err());
        assert!(err.starts_with(&want), "{err}");
        raw.execute_batch("DROP TABLE milestone").unwrap();
        let err = format!("{:#}", store.add_milestone("t", "g", None).unwrap_err());
        assert!(err.starts_with(&want), "{err}");
    }

    fn lead_of_board(board: &Board, m: MilestoneId) -> Option<Lead> {
        board.milestones.iter().find(|x| x.id == m)?.lead.clone()
    }

    #[test]
    fn adds_leads_with_the_next_free_colour_unless_one_is_given() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2, m3] = ["a", "b", "c"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        let cat = store
            .lead_milestone(m1, " Catalogue Lead ", " cat-lead ", None)
            .unwrap();
        assert_eq!(
            cat,
            Lead {
                name: "Catalogue Lead".into(),
                session: "cat-lead".into(),
                colour: Some(Colour::Blue),
            }
        );
        let home = store
            .lead_milestone(m2, "Checkout Lead", "checkout-lead", Some(Colour::Orange))
            .unwrap();
        assert_eq!(home.colour, Some(Colour::Orange));
        let lab = store
            .lead_milestone(m3, "Homelab Lead", "lab", None)
            .unwrap();
        assert_eq!(lab.colour, Some(Colour::Purple));

        let board = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(lead_of_board(&board, m1), Some(cat));
        assert_eq!(lead_of_board(&board, m2), Some(home));
        assert_eq!(lead_of_board(&board, m3), Some(lab));
    }

    #[test]
    fn a_repo_has_one_lead_and_a_lead_one_repo() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store.add_milestone("a", "goal", None).unwrap();
        let m2 = store.add_milestone("b", "goal", None).unwrap();
        store
            .lead_milestone(m1, "Catalogue Lead", "cat-lead", None)
            .unwrap();

        let cases = [
            (
                m1,
                "Other Lead",
                "other",
                "R1 already has a lead, Catalogue Lead; a repo has one lead",
            ),
            (
                m2,
                "Catalogue Lead",
                "other",
                "Catalogue Lead already leads R1; a lead has one repo",
            ),
            (
                m2,
                "catalogue LEAD",
                "other",
                "catalogue LEAD already leads R1; a lead has one repo",
            ),
            (
                m2,
                "Other Lead",
                "cat-lead",
                "the session cat-lead is already Catalogue Lead, the lead of R1; \
                 a lead has one repo",
            ),
        ];
        for (m, name, session, want) in cases {
            let err = store.lead_milestone(m, name, session, None).unwrap_err();
            assert_eq!(err.to_string(), want);
        }
        let board = store.load().unwrap();
        assert_eq!(lead_of_board(&board, m1).unwrap().session, "cat-lead");
        assert_eq!(lead_of_board(&board, m2), None);
    }

    #[test]
    fn the_schema_itself_refuses_a_second_lead_either_way() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store.add_milestone("a", "goal", None).unwrap();
        let m2 = store.add_milestone("b", "goal", None).unwrap();
        store.lead_milestone(m1, "Lead", "s", None).unwrap();
        let raw = Connection::open(&path).unwrap();
        let insert = |m: i64, name: &str, session: &str| {
            raw.execute(
                "INSERT INTO lead (repo_id, name, session, colour) \
                 VALUES (?1, ?2, ?3, 'blue')",
                params![m, name, session],
            )
            .unwrap_err()
            .to_string()
        };
        assert!(insert(m1.0, "Other", "o").contains("UNIQUE constraint failed: lead.repo_id"));
        assert!(insert(m2.0, "LEAD", "o").contains("UNIQUE constraint failed: lead.name"));
        assert!(insert(m2.0, "Other", "S").contains("UNIQUE constraint failed: lead.session"));
    }

    #[test]
    fn refuses_bad_lead_input_and_writes_nothing() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let long = "x".repeat(201);
        let cases = [
            (m, "  ", "s", "the lead name is empty".to_string()),
            (m, "Lead", "", "the lead session name is empty".to_string()),
            (
                m,
                "Lead\u{202E}",
                "s",
                "the lead name contains a control character (U+202E); \
                 it must be one line of plain text"
                    .to_string(),
            ),
            (
                m,
                "Lead",
                "a\nb",
                "the lead session name contains a control character (U+000A); \
                 it must be one line of plain text"
                    .to_string(),
            ),
            (
                m,
                &long,
                "s",
                "the lead name is 201 characters long; the most is 200".to_string(),
            ),
            (
                MilestoneId(9),
                "Lead",
                "s",
                "there is no milestone M9 on the board".to_string(),
            ),
        ];
        for (m, name, session, want) in cases {
            let err = store.lead_milestone(m, name, session, None).unwrap_err();
            assert_eq!(err.to_string(), want);
        }
        assert_eq!(store.load().unwrap().milestones[0].lead, None);
    }

    #[test]
    fn concurrent_leads_for_one_repo_one_wins() {
        const RIVALS: usize = 6;
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        store.add_milestone("a", "goal", None).unwrap();
        store.exec_sql("INSERT INTO repo (path) VALUES (NULL); UPDATE milestone SET repo_id = 1;");
        drop(store);
        let path = Arc::new(path);
        let start = Arc::new(Barrier::new(RIVALS));
        let handles: Vec<_> = (0..RIVALS)
            .map(|i| {
                let (path, start) = (Arc::clone(&path), Arc::clone(&start));
                std::thread::spawn(move || {
                    let mut store = Store::open(&path).unwrap();
                    start.wait();
                    store.add_lead(
                        RepoOf::Milestone(MilestoneId(1)),
                        &format!("Lead {i}"),
                        &format!("s{i}"),
                        None,
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        for r in results.iter().filter_map(|r| r.as_ref().err()) {
            assert!(r.to_string().starts_with("R1 already has a lead"), "{r}");
        }
    }

    #[test]
    fn an_unknown_stored_colour_loads_without_colour_and_is_not_handed_out_again() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2] = ["a", "b"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        store.lead_milestone(m1, "Lead", "s", None).unwrap();
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("UPDATE lead SET colour = 'mauve'")
            .unwrap();
        let board = store.load().unwrap();
        assert_eq!(lead_of_board(&board, m1).unwrap().colour, None);
        // The unknown colour counts as no colour taken, and a rename keeps it as stored.
        let next = store.lead_milestone(m2, "Other", "o", None).unwrap();
        assert_eq!(next.colour, Some(Colour::Blue));
        store
            .set_lead(RepoOf::Milestone(m1), Some("Renamed"), None, None)
            .unwrap();
        let colour: String = raw
            .query_row("SELECT colour FROM lead WHERE repo_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(colour, "mauve");
    }

    #[test]
    fn session_names_take_single_spaces_and_are_unique_ignoring_case() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2] = ["a", "b"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        for (bad, want) in [
            (
                "cat  lead",
                "the lead session name \"cat  lead\" has two spaces in a row",
            ),
            (
                "cat\u{a0}lead",
                "the lead session name contains '\\u{a0}' (U+00A0)",
            ),
        ] {
            let err = store.lead_milestone(m1, "Lead", bad, None).unwrap_err();
            assert!(err.to_string().starts_with(want), "{err}");
        }
        store
            .lead_milestone(m1, "Lead", " Cat-Lead ", None)
            .unwrap();
        let err = store
            .lead_milestone(m2, "Other", "cat-lead", None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the session cat-lead is already Lead, the lead of R1; a lead has one repo"
        );
    }

    #[test]
    fn sets_a_lead_by_the_same_rules_as_add() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m1, m2, m3] = ["a", "b", "c"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        store
            .lead_milestone(m1, "Catalogue Lead", "cat-lead", None)
            .unwrap();
        store
            .lead_milestone(m2, "Checkout Lead", "checkout-lead", None)
            .unwrap();

        // The session died and came back under a new name: only the session changes.
        let (lead, _) = store
            .set_lead(RepoOf::Milestone(m1), None, Some(" cat-lead-two "), None)
            .unwrap();
        assert_eq!(
            lead,
            Lead {
                name: "Catalogue Lead".into(),
                session: "cat-lead-two".into(),
                colour: Some(Colour::Blue),
            }
        );
        let (lead, _) = store
            .set_lead(
                RepoOf::Milestone(m1),
                Some("Cat Lead"),
                None,
                Some(Colour::Green),
            )
            .unwrap();
        assert_eq!(
            (lead.name.as_str(), lead.colour),
            ("Cat Lead", Some(Colour::Green))
        );
        // Its own name or session again, in another case, is not a clash with itself.
        store
            .set_lead(
                RepoOf::Milestone(m1),
                Some("cat lead"),
                Some("Cat-Lead-Two"),
                None,
            )
            .unwrap();
        assert_eq!(
            lead_of_board(&store.load().unwrap(), m1).unwrap(),
            Lead {
                name: "cat lead".into(),
                session: "Cat-Lead-Two".into(),
                colour: Some(Colour::Green),
            }
        );

        let cases: [(MilestoneId, Option<&str>, Option<&str>, &str); 7] = [
            (
                m1,
                None,
                None,
                "nothing to change: give --name, --session or --colour",
            ),
            (
                m3,
                Some("x"),
                None,
                "M3 is in no repo yet; set its repo first: tower milestone set M3 --repo R<n>",
            ),
            (
                MilestoneId(9),
                Some("x"),
                None,
                "there is no milestone M9 on the board",
            ),
            (
                m1,
                Some("CHECKOUT LEAD"),
                None,
                "CHECKOUT LEAD already leads R2; a lead has one repo",
            ),
            (
                m1,
                None,
                Some("Checkout-Lead"),
                "the session Checkout-Lead is already Checkout Lead, the lead of R2; a lead has one repo",
            ),
            (m1, Some(" "), None, "the lead name is empty"),
            (
                m1,
                None,
                Some("a  b"),
                "the lead session name \"a  b\" has two spaces in a row; use single spaces \
                 between words",
            ),
        ];
        for (m, name, session, want) in cases {
            let err = store
                .set_lead(RepoOf::Milestone(m), name, session, None)
                .unwrap_err();
            assert_eq!(err.to_string(), want);
        }
        let board = store.load().unwrap();
        assert_eq!(lead_of_board(&board, m1).unwrap().name, "cat lead");
        assert_eq!(lead_of_board(&board, m2).unwrap().session, "checkout-lead");
        assert_eq!(lead_of_board(&board, m3), None);
    }

    #[test]
    fn read_only_open_of_an_older_schema_says_how_to_upgrade_it() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('old', 'g'); PRAGMA user_version = 1;",
        )
        .unwrap();
        drop(conn);
        let bytes = std::fs::read(&path).unwrap();
        let err = format!("{:#}", Store::open_read_only(&path).err().unwrap());
        assert_eq!(
            err,
            format!(
                "board {} is schema version 1; run tower upgrade",
                path.display()
            )
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "read-only open changed the file"
        );
        // Every version older than this build's gets the same message.
        for v in 1..SCHEMA_VERSION {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", v).unwrap();
            drop(conn);
            let err = format!("{:#}", Store::open_read_only(&path).err().unwrap());
            assert!(
                err.ends_with(&format!("is schema version {v}; run tower upgrade")),
                "{err}"
            );
        }
    }

    #[test]
    fn migrates_a_version_one_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('First shop pages', 'g'), ('Checkout', 'g');
             INSERT INTO task (milestone_id, title) VALUES (2, 'Basket'), (1, 'Reviews'), (NULL, 'Loose');
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 1;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let board = store.load().unwrap();
        let shape: Vec<(&str, Vec<&str>, bool)> = board
            .milestones
            .iter()
            .map(|m| {
                (
                    m.title.as_str(),
                    m.tasks.iter().map(|t| t.title.as_str()).collect(),
                    m.lead.is_some(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("First shop pages", vec!["Reviews"], false),
                ("Checkout", vec!["Basket"], false)
            ]
        );
        // New ids carry on from the old ones, and leads can be added.
        assert_eq!(store.add_milestone("c", "g", None).unwrap(), MilestoneId(3));
        assert_eq!(store.add_task(MilestoneId(1), "t").unwrap(), TaskId(4));
        store
            .lead_milestone(MilestoneId(1), "Lead", "s", None)
            .unwrap();
        assert!(lead_of_board(&store.load().unwrap(), MilestoneId(1)).is_some());
    }

    #[test]
    fn load_reads_each_milestone_goal() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store
            .add_milestone("a", "  Ship the bookmarks page ", None)
            .unwrap();
        assert_eq!(milestone_of(&mut store, m).goal, "Ship the bookmarks page");
    }

    #[test]
    fn a_repo_folds_and_loads_with_its_name_lead_and_board_and_a_schema_14_board_upgrades() {
        let (dir, path) = temp_board();
        let checkout = dir.path().join("storefront");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(&checkout, None).unwrap();
        let m = store.add_milestone("one", "g", Some(r)).unwrap();
        store
            .add_lead(RepoOf::Repo(r), "Lead", "lead", None)
            .unwrap();
        let loose = store.add_standalone_task("loose", Some(r)).unwrap();
        let (t, _) = store
            .open_thread(
                &BoardOf::Session("lead".into()),
                "lead",
                None,
                "triage",
                "go",
            )
            .unwrap();
        let board = store.load().unwrap();
        let repo = board.repo(r).unwrap();
        assert_eq!(repo.name(), "storefront");
        assert_eq!(repo.lead.as_ref().unwrap().name, "Lead");
        assert_eq!(repo.threads.iter().map(|t| t.id).collect::<Vec<_>>(), [t]);
        assert!(!repo.folded);
        assert_eq!(board.standalone[0].repo, Some(r));
        assert_eq!(
            board
                .standalone_of(Some(r))
                .map(|t| t.id)
                .collect::<Vec<_>>(),
            [loose]
        );
        // A repo-wide thread is on the repo's board, not the milestone's part of it.
        assert!(!board.milestones[0].links(&repo.threads[0]));
        assert_eq!(board.milestones[0].id, m);
        store.set_folds(&[r], &[], &[], true).unwrap();
        assert!(store.load().unwrap().repos[0].folded);
        let err = store
            .set_folds(&[r, RepoId(99)], &[], &[], false)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("there is no repo R99 on the board"),
            "{err:#}"
        );
        assert!(store.load().unwrap().repos[0].folded, "nothing written");
        // A repo and a milestone fold in one write, or neither does.
        let err = store
            .set_folds(&[r], &[m, MilestoneId(9)], &[], false)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("there is no milestone M9"),
            "{err:#}"
        );
        assert!(store.load().unwrap().repos[0].folded, "nothing written");
        store.set_folds(&[r], &[m], &[], false).unwrap();
        let board = store.load().unwrap();
        assert!(!board.repos[0].folded && !board.milestones[0].folded);
        store.set_folds(&[r], &[m], &[], true).unwrap();
        let board = store.load().unwrap();
        assert!(board.repos[0].folded && board.milestones[0].folded);
        // Starting a lead on a repo that has its lead is refused, writing none.
        let err = store
            .start_repo_lead("two", "g", r, |m| format!("Two {m}"))
            .unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!("{r} already has its lead, Lead")
        );
        assert_eq!(store.load().unwrap().milestones.len(), 1);
        // A board at schema 14 upgrades with every repo unfolded, keeping its repos.
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..14] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch("INSERT INTO repo (path) VALUES (NULL); PRAGMA user_version = 14;")
            .unwrap();
        drop(conn);
        let board = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(board.repos.len(), 1);
        assert!(!board.repos[0].folded);
        // A repo with no known checkout is named by its id.
        assert_eq!(board.repos[0].name(), "R1");
    }

    #[test]
    fn undoing_a_start_keeps_a_lead_that_attached_and_a_milestone_in_use() {
        let (dir, path) = temp_board();
        let checkout = dir.path().join("storefront");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(&checkout, None).unwrap();
        let (m, lead) = store
            .start_repo_lead("one", "g", r, |m| format!("One {m}"))
            .unwrap();
        // Nothing else uses them: both go, in one write.
        let removed = store.undo_start(m, Some((r, &lead.session))).unwrap();
        assert_eq!(removed, [m.to_string(), lead.session.clone()]);
        let board = store.load().unwrap();
        assert!(board.milestones.is_empty() && board.repos[0].lead.is_none());
        // A lead that has attached and a milestone with a task stay.
        let (m, lead) = store
            .start_repo_lead("two", "g", r, |m| format!("Two {m}"))
            .unwrap();
        store.add_task(m, "t").unwrap();
        store.exec_sql("UPDATE lead SET claude_session = 'sess-1'");
        assert!(
            store
                .undo_start(m, Some((r, &lead.session)))
                .unwrap()
                .is_empty()
        );
        let board = store.load().unwrap();
        assert_eq!(board.milestones.len(), 1);
        assert!(board.repos[0].lead.is_some());
    }

    #[test]
    fn a_dashboard_start_named_as_a_tasks_assignee_gets_the_milestone_id() {
        let (dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let mut repo = |name: &str| {
            let checkout = dir.path().join(name);
            std::fs::create_dir_all(checkout.join(".git")).unwrap();
            store.add_repo(&checkout, None).unwrap()
        };
        let (r1, r2) = (repo("one"), repo("two"));
        let m = store.add_milestone("a", "g", Some(r1)).unwrap();
        store.add_task(m, "t").unwrap();
        // A lead is never a task's assignee, so the name is taken, ignoring case.
        store.exec_sql("UPDATE task SET assignee = 'Comics-Lead'");
        let (m2, lead) = store
            .start_repo_lead("Comics", "g", r2, |_| "comics-lead".into())
            .unwrap();
        assert_eq!(m2, MilestoneId(2));
        assert_eq!(lead.session, "comics-lead-m2");
        assert_eq!(lead.name, lead.session);
        // In no repo, by the same check: against an assignee, and against any repo's lead.
        let (m3, name) = store
            .start_lead_in_no_repo("Atlases", "g", |_| "COMICS-LEAD".into())
            .unwrap();
        assert_eq!((m3, name.as_str()), (MilestoneId(3), "COMICS-LEAD-m3"));
        let (m4, name) = store
            .start_lead_in_no_repo("Atlases", "g", |_| "comics-lead-m2".into())
            .unwrap();
        assert_eq!((m4, name.as_str()), (MilestoneId(4), "comics-lead-m2-m4"));
        // A free name is kept as given, and so is the milestone, in no repo.
        let (m5, name) = store
            .start_lead_in_no_repo("Atlases", "g", |_| "atlases-lead".into())
            .unwrap();
        assert_eq!(name, "atlases-lead");
        let board = store.load().unwrap();
        let added = board.milestones.iter().find(|x| x.id == m5).unwrap();
        assert_eq!((added.title.as_str(), added.repo), ("Atlases", None));
    }

    #[test]
    fn set_folded_folds_and_unfolds_and_refuses_a_missing_milestone_writing_none() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m1 = store.add_milestone("a", "g", None).unwrap();
        let m2 = store.add_milestone("b", "g", None).unwrap();
        let folds = |store: &mut Store| -> Vec<bool> {
            store
                .load()
                .unwrap()
                .milestones
                .iter()
                .map(|m| m.folded)
                .collect()
        };
        assert_eq!(folds(&mut store), [false, false]);
        store.set_folded(&[m1, m2], true).unwrap();
        assert_eq!(folds(&mut store), [true, true]);
        store.set_folded(&[m2], false).unwrap();
        assert_eq!(folds(&mut store), [true, false]);
        let err = store.set_folded(&[m2, MilestoneId(9)], true).unwrap_err();
        assert!(
            format!("{err:#}").contains("there is no milestone M9 on the board"),
            "{err:#}"
        );
        assert_eq!(folds(&mut store), [true, false], "nothing written");
        // A board from before folds upgrades with every milestone unfolded.
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..10] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('old', 'g'); PRAGMA user_version = 10;",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&path).unwrap();
        assert_eq!(folds(&mut store), [false]);
    }

    fn milestone_of(store: &mut Store, m: MilestoneId) -> Milestone {
        store
            .load()
            .unwrap()
            .milestones
            .into_iter()
            .find(|x| x.id == m)
            .unwrap()
    }

    #[test]
    fn tasks_move_between_stages_and_the_milestone_follows() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let [t1, t2] = ["one", "two"].map(|t| store.add_task(m, t).unwrap());
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::Waiting);

        assert_eq!(
            store.set_task_stage(t1, Stage::InProgress).unwrap(),
            Some(m)
        );
        let now = milestone_of(&mut store, m);
        assert_eq!(now.tasks[0].stage, Stage::InProgress);
        assert_eq!(now.tasks[1].stage, Stage::Waiting);
        assert_eq!((now.stage(), now.percent()), (Stage::InProgress, 0));

        store.set_task_stage(t1, Stage::Done).unwrap();
        // Straight from Waiting to Done is allowed.
        store.set_task_stage(t2, Stage::Done).unwrap();
        let now = milestone_of(&mut store, m);
        assert_eq!((now.stage(), now.percent()), (Stage::InProgress, 100));
        // And back to Waiting.
        store.set_task_stage(t2, Stage::Waiting).unwrap();
        assert_eq!(milestone_of(&mut store, m).percent(), 50);
        // A task back at Waiting with the others not started leaves the milestone Waiting.
        store.set_task_stage(t1, Stage::Waiting).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::Waiting);

        let err = store.set_task_stage(t1, Stage::Waiting).unwrap_err();
        assert_eq!(err.to_string(), "task #1 is already waiting");
        let err = store.set_task_stage(TaskId(9), Stage::Done).unwrap_err();
        assert_eq!(err.to_string(), "there is no task #9 on the board");
    }

    #[test]
    fn a_milestone_is_done_only_by_its_lead_and_only_when_every_task_is() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m, empty] = ["a", "b"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        let [t1, t2, t3] = ["one", "two", "three"].map(|t| store.add_task(m, t).unwrap());
        store.set_task_stage(t1, Stage::Done).unwrap();
        store.set_task_stage(t2, Stage::InProgress).unwrap();

        let err = store.mark_milestone_done(m).unwrap_err();
        assert_eq!(
            err.to_string(),
            "M1 is not done until every task is: #2 in progress, #3 waiting"
        );
        let err = store.mark_milestone_done(empty).unwrap_err();
        assert_eq!(err.to_string(), "M2 has no tasks, so it cannot be done");
        let err = store.mark_milestone_done(MilestoneId(9)).unwrap_err();
        assert_eq!(err.to_string(), "there is no milestone M9 on the board");
        assert!(!milestone_of(&mut store, m).marked_done);

        store.set_task_stage(t2, Stage::Done).unwrap();
        store.set_task_stage(t3, Stage::Done).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::InProgress);
        store.mark_milestone_done(m).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::Done);
        let err = store.mark_milestone_done(m).unwrap_err();
        assert_eq!(err.to_string(), "M1 is already done");
    }

    #[test]
    fn a_task_leaving_done_or_a_new_task_reopens_a_done_milestone_until_the_lead_says_again() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let t = store.add_task(m, "one").unwrap();
        store.set_task_stage(t, Stage::Done).unwrap();
        store.mark_milestone_done(m).unwrap();

        store.set_task_stage(t, Stage::InProgress).unwrap();
        let now = milestone_of(&mut store, m);
        assert_eq!((now.stage(), now.marked_done), (Stage::InProgress, false));
        // Done again by the task alone is not Done: the lead marks it.
        store.set_task_stage(t, Stage::Done).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::InProgress);
        store.mark_milestone_done(m).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::Done);

        let t2 = store.add_task(m, "late").unwrap();
        let now = milestone_of(&mut store, m);
        assert_eq!((now.stage(), now.marked_done), (Stage::InProgress, false));
        store.set_task_stage(t2, Stage::Done).unwrap();
        assert_eq!(milestone_of(&mut store, m).stage(), Stage::InProgress);
    }

    #[test]
    fn a_done_mark_keeps_its_date_until_any_task_change_clears_both() {
        let (dir, path) = temp_board();
        let checkout = dir.path().join("storefront");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(&checkout, None).unwrap();
        let m = store.add_milestone("a", "goal", Some(r)).unwrap();
        let t = store.add_task(m, "one").unwrap();
        store.set_task_stage(t, Stage::Done).unwrap();
        assert_eq!(milestone_of(&mut store, m).done_at, None);
        let before = now().unwrap();
        store.mark_milestone_done(m).unwrap();
        let at = milestone_of(&mut store, m).done_at.unwrap();
        assert!((before..=now().unwrap()).contains(&at), "{at}");
        assert_eq!(store.load().unwrap().done_of(r)[0].id, m);
        // Every way a task change clears the mark clears its date: a stage move, a new task
        // and a new subtask under a Done task.
        let clears: [&dyn Fn(&mut Store); 3] = [
            &|s| {
                s.set_task_stage(t, Stage::InProgress).unwrap();
            },
            &|s| {
                s.add_task(m, "late").unwrap();
            },
            &|s| {
                s.add_subtask(t, "sub").unwrap();
            },
        ];
        for clear in clears {
            let board = store.load().unwrap();
            for task in &board.milestones[0].tasks {
                for sub in task.subtasks.iter().filter(|x| x.stage != Stage::Done) {
                    store.set_task_stage(sub.id, Stage::Done).unwrap();
                }
                if task.stage != Stage::Done {
                    store.set_task_stage(task.id, Stage::Done).unwrap();
                }
            }
            if !milestone_of(&mut store, m).marked_done {
                store.mark_milestone_done(m).unwrap();
            }
            assert!(milestone_of(&mut store, m).done_at.is_some());
            clear(&mut store);
            let now = milestone_of(&mut store, m);
            assert_eq!((now.marked_done, now.done_at), (false, None));
            assert!(store.load().unwrap().done_of(r).is_empty());
        }
        // The Done group starts folded and folds through set_folds, refusing a missing repo
        // and writing nothing then.
        assert!(store.load().unwrap().repos[0].done_folded);
        store.set_folds(&[], &[], &[r], false).unwrap();
        assert!(!store.load().unwrap().repos[0].done_folded);
        let err = store
            .set_folds(&[], &[], &[r, RepoId(99)], true)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("there is no repo R99"),
            "{err:#}"
        );
        assert!(
            !store.load().unwrap().repos[0].done_folded,
            "nothing written"
        );
        store.set_folds(&[], &[], &[r], true).unwrap();
        let board = store.load().unwrap();
        assert!(board.repos[0].done_folded && !board.repos[0].folded);
    }

    /// The GLOSSARY's rule, "any change to its tasks' set or stages takes it
    /// back", for a task leaving a Done milestone. No command lets a task leave its milestone:
    /// handing one elsewhere and removing it are both refused, so the source's mark stands
    /// with its task set unchanged. A command that ever lets a task move out must take the
    /// source's mark back too, and this test then says so.
    #[test]
    fn a_task_never_leaves_a_done_milestone_so_its_mark_stands() {
        let (dir, path) = temp_board();
        let checkout = dir.path().join("storefront");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(&checkout, None).unwrap();
        let [done, other] = ["a", "b"].map(|x| store.add_milestone(x, "goal", Some(r)).unwrap());
        store.lead_milestone(other, "Other", "other", None).unwrap();
        let t = store.add_task(done, "one").unwrap();
        store.set_task_stage(t, Stage::Done).unwrap();
        store.mark_milestone_done(done).unwrap();
        let err = store.hand_task(t, other).unwrap_err().to_string();
        assert!(err.contains("already under M1"), "{err}");
        let err = store.remove_task(t).unwrap_err().to_string();
        assert!(err.contains("is under M1"), "{err}");
        let now = milestone_of(&mut store, done);
        assert!(now.marked_done && now.done_at.is_some());
        assert_eq!(now.tasks.iter().map(|k| k.id).collect::<Vec<_>>(), [t]);
    }

    #[test]
    fn a_schema_15_board_upgrades_with_its_done_marks_undated_and_done_groups_folded() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..15] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO repo (path, folded) VALUES (NULL, 0);
             INSERT INTO milestone (title, goal, repo_id, done) VALUES ('old', 'g', 1, 1);
             INSERT INTO task (title, milestone_id, stage) VALUES ('t', 1, 'done');
             INSERT INTO milestone (title, goal, repo_id) VALUES ('open', 'g', 1);
             PRAGMA user_version = 15;",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&path).unwrap();
        let board = store.load().unwrap();
        let old = &board.milestones[0];
        assert_eq!((old.marked_done, old.done_at), (true, None));
        assert_eq!(old.stage(), Stage::Done);
        assert!(board.repos[0].done_folded && !board.repos[0].folded);
        let ids = |ms: Vec<&Milestone>| ms.iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(board.done_of(RepoId(1))), [MilestoneId(1)]);
        assert_eq!(
            ids(board.active_of(Some(RepoId(1))).collect()),
            [MilestoneId(2)]
        );
        // Its undated mark still clears with a task change, and a new mark is dated.
        let t = store.add_task(MilestoneId(1), "late").unwrap();
        assert!(!milestone_of(&mut store, MilestoneId(1)).marked_done);
        store.set_task_stage(t, Stage::Done).unwrap();
        store.mark_milestone_done(MilestoneId(1)).unwrap();
        assert!(milestone_of(&mut store, MilestoneId(1)).done_at.is_some());
    }

    /// A schema 17 board upgrades knowing the leads removed before it: each name a
    /// watch flag opened with, that someone posted under and no lead has now. Neither a
    /// current lead, nor "@manager", nor tower's reply to an author counts.
    #[test]
    fn a_schema_17_board_upgrades_knowing_its_removed_leads_from_the_watchs_flags() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..17] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO repo (path) VALUES (NULL);
             INSERT INTO lead (repo_id, name, session, colour) VALUES (1, 'Now', 'now-lead', 'blue');
             INSERT INTO thread (repo_id, title) VALUES (1, 't');
             INSERT INTO message (thread_id, author, text, posted_at) VALUES
               (1, 'old-lead', 'status: handing over', 1),
               (1, 'system', '@old-lead dev-1 on #4 has not posted since it was nudged', 2),
               (1, 'now-lead', 'go on', 3),
               (1, 'system', '@now-lead dev-1 on #5 is gone from herdr', 4),
               (1, 'dev-2', 'use @name', 5),
               (1, 'system', '@dev-2 no worker is named `@name`, so ...', 6),
               (1, 'system', '@manager could not deliver to ghost: gone', 7),
               (1, 'system', '@never-posted dev-1 on #6 is gone', 8);
             PRAGMA user_version = 17;",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&path).unwrap();
        let known = store.take_pending().unwrap().known;
        // old-lead's flag and post make it a removed lead: no longer known.
        assert!(!known.contains("old-lead"), "{known:?}");
        // The lead now, and dev-2 whom tower replied to, stay known.
        assert!(
            known.contains("now-lead") && known.contains("dev-2"),
            "{known:?}"
        );
        let conn = Connection::open(&path).unwrap();
        let retired: Vec<String> = conn
            .prepare("SELECT name FROM retired_lead ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(retired, ["old-lead"]);
    }

    /// A schema 18 board, its lead check fed by herdr's state_change_seq, upgrades keeping
    /// its "Lead not listening" episodes and first-seen records, and the next look raises
    /// nothing new: a herdr seq is never compared with the Stop hook's count.
    #[test]
    fn a_schema_18_board_upgrades_keeping_its_lead_episodes_and_raising_nothing() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..18] {
            conn.execute_batch(sql).unwrap();
        }
        // R1's lead in an open episode (T1); R2's seen quiet by herdr at seq 57; R3's not.
        conn.execute_batch(
            "INSERT INTO repo (path) VALUES (NULL), (NULL), (NULL);
             INSERT INTO thread (repo_id, title) VALUES (1, 'Lead not listening');
             INSERT INTO lead (repo_id, name, session, colour, claude_session, deaf_thread,
                 quiet_seq) VALUES
               (1, 'One', 'one-lead', 'blue', 'sess-1', 1, 12),
               (2, 'Two', 'two-lead', 'blue', 'sess-2', NULL, 57),
               (3, 'Three', 'three-lead', 'blue', 'sess-3', NULL, NULL);
             PRAGMA user_version = 18;",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&path).unwrap();
        let leads = store.leads().unwrap();
        let kept: Vec<_> = (leads.iter())
            .map(|l| (l.deaf, l.turns, l.quiet_turns))
            .collect();
        assert_eq!(
            kept,
            [
                (Some(ThreadId(1)), 0, Some(0)),
                (None, 0, Some(0)),
                (None, 0, None)
            ]
        );
        // Each lead idle with no waiter: nothing raised, no thread opened.
        let db = path.clone();
        let idle = r#"[{"sessionId":"sess-1","status":"idle"},{"sessionId":"sess-2","status":"idle"},
            {"sessionId":"sess-3","status":"idle"}]"#;
        let log = crate::listen::check_leads(
            &mut store,
            &db,
            || Ok(idle.to_string()),
            10,
            &mut std::collections::HashMap::new(),
        );
        assert!(log.is_empty(), "{log:?}");
        let threads: i64 = store
            .conn
            .query_row("SELECT count(*) FROM thread", [], |r| r.get(0))
            .unwrap();
        assert_eq!(threads, 1);
    }

    /// `tower lead remove` keeps the lead's name and session as a retired lead's.
    #[test]
    fn removing_a_lead_keeps_its_name_and_session_as_a_retired_leads() {
        let (dir, path) = temp_board();
        let checkout = dir.path().join("storefront");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(&checkout, None).unwrap();
        store
            .add_lead(RepoOf::Repo(r), "Kids-Lead", "Kids-Corner", None)
            .unwrap();
        let (t, _) = store
            .open_thread(
                &BoardOf::Session("Kids-Corner".into()),
                "Kids-Corner",
                None,
                "t",
                "hello",
            )
            .unwrap();
        store.post(t, "dev-2", "status: on it").unwrap();
        assert!(store.take_pending().unwrap().known.contains("kids-corner"));
        store.remove_lead(r, "done: retired").unwrap();
        let known = store.take_pending().unwrap().known;
        assert!(
            !known.contains("kids-corner") && known.contains("dev-2"),
            "{known:?}"
        );
    }

    #[test]
    fn the_schema_itself_refuses_an_unknown_stage() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        store.add_task(m, "one").unwrap();
        let raw = Connection::open(&path).unwrap();
        for sql in [
            "UPDATE task SET stage = 'blocked'",
            "UPDATE task SET stage = 'in progress'",
            "UPDATE milestone SET done = 2",
        ] {
            let err = raw.execute_batch(sql).unwrap_err().to_string();
            assert!(err.contains("CHECK constraint failed"), "{sql}: {err}");
        }
    }

    #[test]
    fn migrates_a_version_two_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('First shop pages', 'g'), ('Checkout', 'g');
             INSERT INTO task (milestone_id, title) VALUES (2, 'Basket'), (1, 'Reviews'), (NULL, 'Loose');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 2;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let board = store.load().unwrap();
        // Title, tasks with stages, the Done mark, the lead's session.
        type Shape<'a> = (&'a str, Vec<(&'a str, Stage)>, bool, Option<&'a str>);
        let shape: Vec<Shape> = board
            .milestones
            .iter()
            .map(|m| {
                (
                    m.title.as_str(),
                    m.tasks
                        .iter()
                        .map(|t| (t.title.as_str(), t.stage))
                        .collect(),
                    m.marked_done,
                    m.lead.as_ref().map(|l| l.session.as_str()),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "First shop pages",
                    vec![("Reviews", Stage::Waiting)],
                    false,
                    Some("cat-lead")
                ),
                ("Checkout", vec![("Basket", Stage::Waiting)], false, None),
            ]
        );
        // The old tasks move through stages like new ones, the standalone one included.
        store.set_task_stage(TaskId(2), Stage::Done).unwrap();
        assert_eq!(store.set_task_stage(TaskId(3), Stage::Done).unwrap(), None);
        store.mark_milestone_done(MilestoneId(1)).unwrap();
        assert_eq!(
            milestone_of(&mut store, MilestoneId(1)).stage(),
            Stage::Done
        );
        assert_eq!(store.add_task(MilestoneId(2), "t").unwrap(), TaskId(4));
    }

    /// (id, title, stage) of each subtask of the task at `task` in milestone `m`.
    fn subtasks_in(store: &mut Store, m: MilestoneId, task: TaskId) -> Vec<(TaskId, Stage)> {
        milestone_of(store, m)
            .tasks
            .into_iter()
            .find(|t| t.id == task)
            .unwrap()
            .subtasks
            .iter()
            .map(|s| (s.id, s.stage))
            .collect()
    }

    #[test]
    fn subtasks_go_one_level_under_a_task_that_exists() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let t = store.add_task(m, "parent").unwrap();
        let s1 = store.add_subtask(t, " write tests ").unwrap();
        let s2 = store.add_subtask(t, "merge").unwrap();
        assert_eq!((s1, s2), (TaskId(2), TaskId(3)));
        // Subtasks hang under their task, not beside it in the milestone.
        let now = milestone_of(&mut store, m);
        assert_eq!(now.tasks.len(), 1);
        assert_eq!(now.tasks[0].subtasks[0].title, "write tests");

        let cases = [
            (
                s1,
                "x",
                "#2 is a subtask of #1; a subtask has no subtasks of its own: \
                 ask the lead for a new task",
            ),
            (TaskId(9), "x", "there is no task #9 on the board"),
            (t, " ", "the subtask title is empty"),
        ];
        for (parent, title, want) in cases {
            let err = store.add_subtask(parent, title).unwrap_err();
            assert_eq!(err.to_string(), want);
        }
        assert_eq!(
            subtasks_in(&mut store, m, t),
            vec![(s1, Stage::Waiting), (s2, Stage::Waiting)]
        );
        // A standalone task takes subtasks too.
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("INSERT INTO task (milestone_id, title) VALUES (NULL, 'loose')")
            .unwrap();
        let loose = TaskId(raw.last_insert_rowid());
        let s3 = store.add_subtask(loose, "step").unwrap();
        assert_eq!(store.task(loose).unwrap().subtasks.len(), 1);
        assert_eq!(store.parent_of(s3).unwrap(), Some(loose));
        assert_eq!(store.parent_of(loose).unwrap(), None);
        assert_eq!(
            store.task(s3).unwrap_err().to_string(),
            "there is no task #5 on the board"
        );
    }

    #[test]
    fn the_schema_itself_refuses_a_subtask_with_its_own_milestone() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let t = store.add_task(m, "parent").unwrap();
        let raw = Connection::open(&path).unwrap();
        let err = raw
            .execute(
                "INSERT INTO task (milestone_id, parent_id, title) VALUES (?1, ?2, 's')",
                params![m.0, t.0],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("CHECK constraint failed"), "{err}");
    }

    #[test]
    fn subtasks_move_through_stages_and_the_task_counts_them() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let t = store.add_task(m, "parent").unwrap();
        let [s1, s2, s3] = ["one", "two", "three"].map(|x| store.add_subtask(t, x).unwrap());
        // A subtask is under no milestone of its own.
        assert_eq!(store.set_task_stage(s1, Stage::Done).unwrap(), None);
        store.set_task_stage(s2, Stage::InProgress).unwrap();
        assert_eq!(
            subtasks_in(&mut store, m, t),
            vec![
                (s1, Stage::Done),
                (s2, Stage::InProgress),
                (s3, Stage::Waiting)
            ]
        );
        let task = store.task(t).unwrap();
        assert_eq!((task.done_subtasks(), task.subtasks.len()), (1, 3));
        let err = store.set_task_stage(s1, Stage::Done).unwrap_err();
        assert_eq!(err.to_string(), "task #2 is already done");
    }

    #[test]
    fn subtasks_never_count_towards_the_percentage_or_the_done_guard() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let [t1, t2] = ["one", "two"].map(|x| store.add_task(m, x).unwrap());
        let s = store.add_subtask(t1, "step").unwrap();
        store.set_task_stage(s, Stage::Done).unwrap();
        // A Done subtask is not a Done task: 0 of 2, and the guard names only the tasks.
        let now = milestone_of(&mut store, m);
        assert_eq!((now.percent(), now.tasks.len()), (0, 2));
        let err = store.mark_milestone_done(m).unwrap_err();
        assert_eq!(
            err.to_string(),
            "M1 is not done until every task is: #1 waiting, #2 waiting"
        );
        store.set_task_stage(t1, Stage::Done).unwrap();
        store.set_task_stage(t2, Stage::Done).unwrap();
        assert_eq!(milestone_of(&mut store, m).percent(), 100);
        store.mark_milestone_done(m).unwrap();
    }

    #[test]
    fn a_subtask_added_under_a_done_task_reopens_it_and_the_milestone() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m, other] = ["a", "b"].map(|x| store.add_milestone(x, "goal", None).unwrap());
        let [t1, t2] = ["one", "two"].map(|x| store.add_task(m, x).unwrap());
        let t3 = store.add_task(other, "three").unwrap();
        for id in [t1, t2, t3] {
            store.set_task_stage(id, Stage::Done).unwrap();
        }
        store.mark_milestone_done(m).unwrap();
        store.mark_milestone_done(other).unwrap();

        store.add_subtask(t2, "merge").unwrap();
        let now = milestone_of(&mut store, m);
        let stages: Vec<Stage> = now.tasks.iter().map(|t| t.stage).collect();
        assert_eq!(stages, vec![Stage::Done, Stage::InProgress]);
        assert_eq!((now.marked_done, now.stage()), (false, Stage::InProgress));
        assert!(milestone_of(&mut store, other).marked_done);

        // Under a task not Done, the task stays where it is.
        store.set_task_stage(t1, Stage::Waiting).unwrap();
        store.add_subtask(t1, "step").unwrap();
        assert_eq!(store.task(t1).unwrap().stage, Stage::Waiting);
    }

    #[test]
    fn a_task_is_not_done_while_any_subtask_is_open() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let t = store.add_task(m, "parent").unwrap();
        let [s1, s2, s3] = ["one", "two", "merge"].map(|x| store.add_subtask(t, x).unwrap());
        store.set_task_stage(s1, Stage::Done).unwrap();
        store.set_task_stage(s2, Stage::InProgress).unwrap();
        let err = store.set_task_stage(t, Stage::Done).unwrap_err();
        assert_eq!(
            err.to_string(),
            "task #1 is not done until every subtask is: #3 in progress, #4 waiting"
        );
        assert_eq!(store.task(t).unwrap().stage, Stage::Waiting);
        // Other stages are not guarded.
        store.set_task_stage(t, Stage::InProgress).unwrap();
        store.set_task_stage(s2, Stage::Done).unwrap();
        store.set_task_stage(s3, Stage::Done).unwrap();
        store.set_task_stage(t, Stage::Done).unwrap();
        assert_eq!(store.task(t).unwrap().stage, Stage::Done);
        // A task without subtasks moves to Done as before.
        let plain = store.add_task(m, "plain").unwrap();
        store.set_task_stage(plain, Stage::Done).unwrap();
    }

    #[test]
    fn a_subtask_leaving_done_reopens_its_done_parent_and_the_milestone() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let [m, other] = ["a", "b"].map(|x| store.add_milestone(x, "goal", None).unwrap());
        let t = store.add_task(m, "parent").unwrap();
        let [s1, s2] = ["one", "merge"].map(|x| store.add_subtask(t, x).unwrap());
        let t2 = store.add_task(other, "other").unwrap();
        for id in [s1, s2, t, t2] {
            store.set_task_stage(id, Stage::Done).unwrap();
        }
        store.mark_milestone_done(m).unwrap();
        store.mark_milestone_done(other).unwrap();

        store.set_task_stage(s2, Stage::Waiting).unwrap();
        let now = milestone_of(&mut store, m);
        assert_eq!(now.tasks[0].stage, Stage::InProgress);
        assert_eq!((now.marked_done, now.stage()), (false, Stage::InProgress));
        // Only the parent's milestone reopens.
        assert!(milestone_of(&mut store, other).marked_done);

        // A parent not Done stays where it is when a subtask leaves Done.
        store.set_task_stage(t, Stage::Waiting).unwrap();
        store.set_task_stage(s1, Stage::InProgress).unwrap();
        assert_eq!(store.task(t).unwrap().stage, Stage::Waiting);

        // A subtask under a standalone Done task reopens it too, with no milestone to clear.
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("INSERT INTO task (milestone_id, title) VALUES (NULL, 'loose')")
            .unwrap();
        let loose = TaskId(raw.last_insert_rowid());
        let s3 = store.add_subtask(loose, "step").unwrap();
        store.set_task_stage(s3, Stage::Done).unwrap();
        store.set_task_stage(loose, Stage::Done).unwrap();
        store.set_task_stage(s3, Stage::InProgress).unwrap();
        assert_eq!(store.task(loose).unwrap().stage, Stage::InProgress);
        assert!(milestone_of(&mut store, other).marked_done);
    }

    #[test]
    fn migrates_a_version_three_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal, done) VALUES ('First shop pages', 'g', 1), ('Checkout', 'g', 0);
             INSERT INTO task (milestone_id, title, stage)
                 VALUES (1, 'Reviews', 'done'), (2, 'Basket', 'in-progress'), (NULL, 'Loose', 'waiting');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 3;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let board = store.load().unwrap();
        type Shape<'a> = (&'a str, Vec<(&'a str, Stage, usize)>, bool, Option<&'a str>);
        let shape: Vec<Shape> = board
            .milestones
            .iter()
            .map(|m| {
                (
                    m.title.as_str(),
                    m.tasks
                        .iter()
                        .map(|t| (t.title.as_str(), t.stage, t.subtasks.len()))
                        .collect(),
                    m.marked_done,
                    m.lead.as_ref().map(|l| l.session.as_str()),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "First shop pages",
                    vec![("Reviews", Stage::Done, 0)],
                    true,
                    Some("cat-lead")
                ),
                (
                    "Checkout",
                    vec![("Basket", Stage::InProgress, 0)],
                    false,
                    None
                ),
            ]
        );
        assert_eq!(
            milestone_of(&mut store, MilestoneId(1)).stage(),
            Stage::Done
        );
        // Old tasks, standalone included, take subtasks and ids carry on; one under the old
        // Done task reopens it and its milestone.
        assert_eq!(store.add_subtask(TaskId(1), "s").unwrap(), TaskId(4));
        assert_eq!(store.add_subtask(TaskId(3), "s").unwrap(), TaskId(5));
        let now = milestone_of(&mut store, MilestoneId(1));
        assert_eq!(
            (
                now.tasks[0].subtasks.len(),
                now.tasks[0].stage,
                now.marked_done
            ),
            (1, Stage::InProgress, false)
        );
    }

    #[test]
    fn migrates_a_version_four_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal, done) VALUES ('First shop pages', 'g', 1);
             INSERT INTO task (milestone_id, title, stage) VALUES (1, 'Reviews', 'done');
             INSERT INTO task (parent_id, title, stage) VALUES (1, 'Search API', 'done');
             INSERT INTO task (milestone_id, title, stage) VALUES (NULL, 'Loose', 'waiting');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 4;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let m = milestone_of(&mut store, MilestoneId(1));
        // Everything kept, the Done mark included, and no details yet.
        assert_eq!(
            (
                m.title.as_str(),
                m.stage(),
                m.lead.unwrap().session.as_str()
            ),
            ("First shop pages", Stage::Done, "cat-lead")
        );
        assert_eq!(m.details, MilestoneDetails::default());
        let t = &m.tasks[0];
        assert_eq!(
            (t.title.as_str(), t.stage, t.subtasks.len()),
            ("Reviews", Stage::Done, 1)
        );
        assert_eq!(t.details, TaskDetails::default());

        // Old tasks and milestones take details; none of it takes back the Done mark.
        store
            .set_task(
                TaskId(1),
                TaskFields {
                    description: Some("What and why"),
                    assignee: Some("dev-1"),
                    link: Some("https://example.com/t/1"),
                    ..TaskFields::default()
                },
            )
            .unwrap();
        store.add_criterion(TaskId(1), "Loads fast").unwrap();
        store.check_criterion(TaskId(1), 1, true).unwrap();
        store.add_blocker(TaskId(1), TaskId(3)).unwrap();
        store
            .set_milestone(
                MilestoneId(1),
                MilestoneFields {
                    spec: Some("docs/spec.md"),
                    ..MilestoneFields::default()
                },
            )
            .unwrap();
        let m = milestone_of(&mut store, MilestoneId(1));
        assert_eq!(m.stage(), Stage::Done);
        assert_eq!(m.details.spec.as_deref(), Some("docs/spec.md"));
        assert_eq!(
            m.tasks[0].details,
            TaskDetails {
                description: Some("What and why".into()),
                context: None,
                link: Some("https://example.com/t/1".into()),
                mr: None,
                assignee: Some("dev-1".into()),
                criteria: vec![Criterion {
                    text: "Loads fast".into(),
                    done: true
                }],
                blocked_by: vec![TaskId(3)],
                blocked: None,
                doing: None,
                done_by: None,
                repo: None,
            }
        );
        // `task` reads the same details as `load`.
        assert_eq!(store.task(TaskId(1)).unwrap().details, m.tasks[0].details);
    }

    #[test]
    fn the_schema_itself_refuses_a_task_blocked_by_itself_or_twice() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let [t1, t2] = ["one", "two"].map(|t| store.add_task(m, t).unwrap());
        store.add_blocker(t1, t2).unwrap();
        let raw = Connection::open(&path).unwrap();
        let insert = |a: TaskId, b: TaskId| {
            raw.execute(
                "INSERT INTO blocker (task_id, blocker_id) VALUES (?1, ?2)",
                params![a.0, b.0],
            )
            .unwrap_err()
            .to_string()
        };
        assert!(insert(t1, t1).contains("CHECK constraint failed"));
        assert!(insert(t1, t2).contains("UNIQUE constraint failed"));
    }

    #[test]
    fn a_loop_of_blockers_is_refused_however_long() {
        let (_dir, path) = temp_board();
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("a", "goal", None).unwrap();
        let [a, b, c] = ["a", "b", "c"].map(|t| store.add_task(m, t).unwrap());
        store.add_blocker(a, b).unwrap();
        store.add_blocker(b, c).unwrap();
        let err = store.add_blocker(c, a).unwrap_err();
        assert_eq!(
            err.to_string(),
            "#1 already waits for #3, so #3 cannot wait for #1"
        );
        // Two tasks waiting for the same one is not a loop.
        store.add_blocker(a, c).unwrap();
    }

    /// The unread posts on `repo`'s board as (thread, author, text), then counted as told.
    fn take_unread(store: &mut Store, repo: RepoId) -> Vec<(ThreadId, String, String)> {
        let posts = unread(&store.conn, repo).unwrap();
        store.tell_lead(repo, |_| Ok(())).unwrap();
        (posts.into_iter())
            .map(|p| (p.thread, p.author, p.text))
            .collect()
    }

    #[test]
    fn a_blocker_done_in_another_repo_posts_in_the_blocked_tasks_thread_for_its_lead() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        // #4 in R2 waits for #1 and #2, both in R1.
        store.add_blocker(TaskId(4), TaskId(1)).unwrap();
        store.add_blocker(TaskId(4), TaskId(2)).unwrap();
        let m2 = BoardOf::Milestone(MilestoneId(2));
        let (t, _) = store
            .open_thread(
                &m2,
                "dev-4",
                Some(TaskId(4)),
                "Task #4 team",
                "waiting on R1",
            )
            .unwrap();
        take_unread(&mut store, RepoId(2));
        store.set_task_stage(TaskId(3), Stage::Done).unwrap();
        store.set_task_stage(TaskId(1), Stage::Done).unwrap();
        let line = |text: &str| vec![(t, "system".to_string(), text.to_string())];
        assert_eq!(
            take_unread(&mut store, RepoId(2)),
            line("#1 in R1 is done; #4, which it blocked, is still blocked by #2")
        );
        store.set_task_stage(TaskId(2), Stage::Done).unwrap();
        assert_eq!(
            take_unread(&mut store, RepoId(2)),
            line("#2 in R1 is done; #4, which it blocked, can go on")
        );
        // The blocker's own board is told nothing: its lead moved it.
        assert!(take_unread(&mut store, RepoId(1)).is_empty());
    }

    #[test]
    fn a_blocker_done_opens_a_thread_for_a_blocked_task_with_none_and_no_other_board_hears() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let done = store.add_task(MilestoneId(2), "done already").unwrap();
        store.set_task_stage(done, Stage::Done).unwrap();
        store.exec_sql("INSERT INTO repo (path) VALUES (NULL)");
        let leadless = store
            .add_standalone_task("in a repo with no lead", Some(RepoId(3)))
            .unwrap();
        // #1 in R1 blocks: #2 in its own repo, #4 in R2 with no thread, a Done task in
        // R2, a task in a repo with no lead and #5 in no repo.
        for task in [TaskId(2), TaskId(4), done, leadless, TaskId(5)] {
            store.add_blocker(task, TaskId(1)).unwrap();
        }
        store.set_task_stage(TaskId(3), Stage::Done).unwrap();
        store.set_task_stage(TaskId(1), Stage::Done).unwrap();
        let threads = milestone_of(&mut store, MilestoneId(2)).threads;
        assert_eq!(threads.len(), 1);
        let thread = store.thread(threads[0].id).unwrap();
        assert_eq!(
            (thread.task, thread.by.as_str(), thread.title.as_str()),
            (Some(TaskId(4)), "system", "Blocked by #1")
        );
        let texts: Vec<&str> = thread.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["#1 in R1 is done; #4, which it blocked, can go on"]);
        // That line is the only message on the board.
        let messages: i64 = (store.conn)
            .query_row("SELECT count(*) FROM message", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 1);
    }

    #[test]
    fn migrates_an_empty_file_from_version_zero() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        drop(Connection::open(&path).unwrap());
        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        store
            .add_milestone("after migration", "goal", None)
            .unwrap();
    }

    /// A board with M1 led by cat-lead (tasks #1, #2 and subtask #3 of #1), M2 led by
    /// checkout-lead (task #4), M3 with no lead, and standalone task #5.
    fn board_with_leads(path: &Path) -> Store {
        let mut store = Store::open(path).unwrap();
        let [m1, m2, _m3] = ["a", "b", "c"].map(|t| store.add_milestone(t, "goal", None).unwrap());
        store
            .lead_milestone(m1, "Catalogue Lead", "cat-lead", None)
            .unwrap();
        store
            .lead_milestone(m2, "Checkout Lead", "checkout-lead", None)
            .unwrap();
        store.add_task(m1, "one").unwrap();
        store.add_task(m1, "two").unwrap();
        store.add_subtask(TaskId(1), "sub").unwrap();
        store.add_task(m2, "four").unwrap();
        store
            .conn
            .execute("INSERT INTO task (title) VALUES ('loose')", [])
            .unwrap();
        store
    }

    fn threads_of(store: &mut Store, m: MilestoneId) -> Vec<Thread> {
        milestone_of(store, m).threads
    }

    #[test]
    fn opens_threads_by_milestone_or_session_posts_and_resolves_with_evidence() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let before = now().unwrap();
        let (t1, m) = store
            .open_thread(
                &BoardOf::Milestone(MilestoneId(1)),
                " cat-lead ",
                Some(TaskId(2)),
                " Task #2 team ",
                "Opening this thread for its team.",
            )
            .unwrap();
        assert_eq!((t1, m), (ThreadId(1), RepoId(1)));
        let (t2, m) = store
            .open_thread(
                &BoardOf::Session("Cat-Lead".into()),
                "dev-1",
                None,
                "Cover images",
                "blocked: the covers are missing",
            )
            .unwrap();
        assert_eq!((t2, m), (ThreadId(2), RepoId(1)));
        let after_post = store.post(t1, "dev-2", "copy, reading the spec").unwrap();
        assert_eq!(after_post.replies(), 1);
        let resolved = store
            .resolve_thread(t2, "lead", "covers in for the whole shelf")
            .unwrap();
        assert_eq!(resolved.status(), ThreadStatus::Resolved);
        let after = now().unwrap();

        let threads = threads_of(&mut Store::open(&path).unwrap(), MilestoneId(1));
        let shape: Vec<_> = threads
            .iter()
            .map(|t| {
                let msgs: Vec<(&str, &str)> = t
                    .messages
                    .iter()
                    .map(|m| (m.author.as_str(), m.text.as_str()))
                    .collect();
                (
                    t.id,
                    t.title.as_str(),
                    t.by.as_str(),
                    t.task,
                    t.status(),
                    msgs,
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    t1,
                    "Task #2 team",
                    "cat-lead",
                    Some(TaskId(2)),
                    ThreadStatus::Open,
                    vec![
                        ("cat-lead", "Opening this thread for its team."),
                        ("dev-2", "copy, reading the spec"),
                    ]
                ),
                (
                    t2,
                    "Cover images",
                    "dev-1",
                    None,
                    ThreadStatus::Resolved,
                    // The board loads a resolved thread's last two: its evidence last.
                    vec![
                        ("dev-1", "blocked: the covers are missing"),
                        ("lead", "covers in for the whole shelf")
                    ]
                ),
            ]
        );
        assert!(
            threads
                .iter()
                .flat_map(|t| &t.messages)
                .all(|m| (before..=after).contains(&m.at))
        );
        assert!(threads_of(&mut store, MilestoneId(2)).is_empty());
    }

    #[test]
    fn a_thread_links_only_to_a_task_of_its_own_repo() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        for (task, want) in [
            (
                4,
                "#4 is in R2, not R1; a thread links to a task of its own repo",
            ),
            (
                5,
                "#5 is in no repo; a thread links to a task of its own repo",
            ),
            (3, "#3 is a subtask of #1; link the thread to #1 instead"),
            (9, "there is no task #9 on the board"),
        ] {
            let err = store
                .open_thread(&m1, "lead", Some(TaskId(task)), "t", "m")
                .unwrap_err();
            assert_eq!(err.to_string(), want, "#{task}");
        }
        assert!(threads_of(&mut store, MilestoneId(1)).is_empty());
    }

    #[test]
    fn refuses_a_board_that_is_not_there_and_bad_text_and_writes_nothing() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let cases = [
            (
                BoardOf::Milestone(MilestoneId(3)),
                "lead",
                "t",
                "m",
                "M3 has no lead yet, so it has no message board; it is in no repo yet: \
                 tower milestone set M3 --repo R<n> first, then tower lead add M3",
            ),
            (
                BoardOf::Milestone(MilestoneId(9)),
                "lead",
                "t",
                "m",
                "there is no milestone M9 on the board",
            ),
            (
                BoardOf::Session("nobody".into()),
                "lead",
                "t",
                "m",
                "no lead has the session nobody",
            ),
            (
                m1.clone(),
                "two  spaces",
                "t",
                "m",
                "the author \"two  spaces\" has two spaces in a row",
            ),
            (m1.clone(), "system", "t", "m", "the author system is kept"),
            (m1.clone(), "lead", " ", "m", "the thread title is empty"),
            (m1.clone(), "lead", "t", "", "the message is empty"),
            (
                m1.clone(),
                "lead",
                "t",
                "a\u{7}b",
                "the message contains a control character",
            ),
        ];
        for (of, by, title, text, want) in cases {
            let err = store.open_thread(&of, by, None, title, text).unwrap_err();
            assert!(err.to_string().starts_with(want), "{want}: {err}");
        }
        let count: i64 = store
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM thread) + (SELECT count(*) FROM message)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn resolving_needs_evidence_and_a_resolved_thread_takes_no_more_messages() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let (t, _) = store
            .open_thread(&BoardOf::Milestone(MilestoneId(1)), "lead", None, "t", "m")
            .unwrap();
        let err = store.resolve_thread(t, "lead", "  ").unwrap_err();
        assert_eq!(err.to_string(), "the evidence is empty");
        assert_eq!(
            threads_of(&mut store, MilestoneId(1))[0].status(),
            ThreadStatus::Open
        );
        store
            .resolve_thread(t, "senior", "12 books on all three sizes")
            .unwrap();
        for err in [
            store.post(t, "dev-1", "one more thing").unwrap_err(),
            store.resolve_thread(t, "lead", "again").unwrap_err(),
        ] {
            assert_eq!(
                err.to_string(),
                "T1 is resolved; resolved threads are read-only"
            );
        }
        let err = store.post(ThreadId(7), "dev-1", "hi").unwrap_err();
        assert_eq!(err.to_string(), "there is no thread T7 on the board");
        let thread = &store.thread(t).unwrap();
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.messages[1].text, "12 books on all three sizes");
    }

    #[test]
    fn the_schema_itself_refuses_half_a_resolution() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        store
            .open_thread(&BoardOf::Milestone(MilestoneId(1)), "lead", None, "t", "m")
            .unwrap();
        for sql in [
            "UPDATE thread SET resolved_by = 'qa'",
            "UPDATE thread SET resolved_at = 5",
        ] {
            let err = store.conn.execute(sql, []).unwrap_err();
            assert!(err.to_string().contains("CHECK constraint failed"), "{err}");
        }
    }

    #[test]
    fn resolving_records_who_and_when_and_the_starter_is_the_first_author() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let (t, _) = store
            .open_thread(&BoardOf::Milestone(MilestoneId(1)), "dev-1", None, "t", "m")
            .unwrap();
        store.post(t, "qa", "copy").unwrap();
        let before = now().unwrap();
        store.resolve_thread(t, "Senior", "4 of 4 pass").unwrap();
        let after = now().unwrap();
        let thread = store.thread(t).unwrap();
        let r = thread.resolved.clone().unwrap();
        assert_eq!((thread.by.as_str(), r.by.as_str()), ("dev-1", "Senior"));
        assert!((before..=after).contains(&r.at));
        assert_eq!(thread.messages.last().unwrap().text, "4 of 4 pass");
        let err = store.thread(ThreadId(9)).unwrap_err();
        assert_eq!(err.to_string(), "there is no thread T9 on the board");
    }

    /// Adds a message to `thread` straight into the file, at `at`.
    fn raw_message(store: &Store, thread: ThreadId, author: &str, text: &str, at: i64) {
        store
            .conn
            .execute(
                "INSERT INTO message (thread_id, author, text, posted_at) VALUES (?1, ?2, ?3, ?4)",
                params![thread.0, author, text, at],
            )
            .unwrap();
    }

    #[test]
    fn names_written_before_the_character_rule_still_load_and_take_replies() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t, _) = store
            .open_thread(&m1, "cat-lead", None, "t", "Open.")
            .unwrap();
        // An author the rule now refuses, already on the board.
        raw_message(&store, t, "dév-2", "copy", 4_000_000_000);
        let board = store.load().unwrap();
        assert_eq!(
            board.milestones[0].threads[0].authors,
            ["cat-lead", "dév-2"]
        );
        // The thread still takes posts; a new post by that name is refused.
        store.post(t, "dev-2", "copy again").unwrap();
        let err = store.post(t, "dév-2", "x").unwrap_err().to_string();
        assert!(err.contains("use only ASCII letters"), "{err}");
    }

    #[test]
    fn messages_are_ordered_by_time_then_id_so_same_second_posts_keep_their_order() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let (t, _) = store
            .open_thread(
                &BoardOf::Milestone(MilestoneId(1)),
                "lead",
                None,
                "t",
                "open",
            )
            .unwrap();
        store
            .conn
            .execute("UPDATE message SET posted_at = 100", [])
            .unwrap();
        raw_message(&store, t, "dev-1", "b", 200);
        raw_message(&store, t, "dev-2", "c", 200);
        raw_message(&store, t, "qa", "d", 200);
        raw_message(&store, t, "qa", "e", 200);
        raw_message(&store, t, "qa", "f", 200);
        raw_message(&store, t, "qa", "g", 200);
        for text in ["h", "i", "j", "k", "l", "m"] {
            raw_message(&store, t, "qa", text, 200);
        }
        // Posted later by id, but stamped earlier (a clock step back): it sorts by time.
        raw_message(&store, t, "senior", "a", 150);
        let texts = |msgs: &[Message]| msgs.iter().map(|m| m.text.clone()).collect::<Vec<_>>();
        assert_eq!(
            texts(&store.thread(t).unwrap().messages),
            [
                "open", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m"
            ]
        );
        let summary = &milestone_of(&mut store, MilestoneId(1)).threads[0];
        assert_eq!(
            texts(&summary.messages),
            ["b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m"]
        );
        assert_eq!(summary.by, "lead");
        let ids: Vec<i64> = summary.messages.iter().map(|m| m.id).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    }

    #[test]
    fn the_board_loads_only_what_the_summary_shows_of_a_long_thread() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (open, _) = store.open_thread(&m1, "dev-1", None, "long", "m0").unwrap();
        let (done, _) = store.open_thread(&m1, "qa", None, "done", "r0").unwrap();
        for i in 1..500 {
            raw_message(&store, open, "dev-2", &format!("m{i}"), 2_000_000_000 + i);
        }
        for i in 1..50 {
            raw_message(&store, done, "qa", &format!("r{i}"), 2_000_000_000 + i);
        }
        store.resolve_thread(done, "senior", "evidence").unwrap();
        let threads = threads_of(&mut store, MilestoneId(1));
        let shape: Vec<_> = threads
            .iter()
            .map(|t| {
                let texts: Vec<&str> = t.messages.iter().map(|m| m.text.as_str()).collect();
                (t.by.as_str(), t.total, t.replies(), texts)
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "dev-1",
                    500,
                    499,
                    vec![
                        "m488", "m489", "m490", "m491", "m492", "m493", "m494", "m495", "m496",
                        "m497", "m498", "m499"
                    ]
                ),
                // The far-future posts are later than the clock: the evidence is stamped
                // with the thread's latest time, so it stays last.
                ("qa", 51, 50, vec!["r47", "r48", "r49", "evidence"]),
            ]
        );
        assert_eq!(store.thread(open).unwrap().messages.len(), 500);
    }

    #[test]
    fn migrates_a_version_five_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal, done) VALUES ('First shop pages', 'g', 0);
             INSERT INTO task (milestone_id, title, stage, description, assignee)
                 VALUES (1, 'Reviews', 'in-progress', 'Reviews under each book.', 'dev-1');
             INSERT INTO criterion (task_id, text) VALUES (1, 'ordered');
             INSERT INTO task (parent_id, title, stage) VALUES (1, 'Sub', 'done');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 5;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let m = milestone_of(&mut store, MilestoneId(1));
        assert_eq!(
            (
                m.tasks.len(),
                m.tasks[0].subtasks.len(),
                m.tasks[0].details.description.as_deref(),
                m.tasks[0].details.criteria.len(),
                m.lead.map(|l| l.session),
                m.threads.len()
            ),
            (
                1,
                1,
                Some("Reviews under each book."),
                1,
                Some("cat-lead".to_string()),
                0
            )
        );
        // The old lead gets a board, and the old task takes a thread.
        let (t, _) = store
            .open_thread(
                &BoardOf::Session("cat-lead".into()),
                "lead",
                Some(TaskId(1)),
                "Reviews team",
                "go",
            )
            .unwrap();
        assert_eq!(t, ThreadId(1));
        assert_eq!(
            milestone_of(&mut store, MilestoneId(1)).threads[0].task,
            Some(TaskId(1))
        );
    }

    fn blocked_in(store: &mut Store, m: MilestoneId) -> Vec<Option<String>> {
        let m = milestone_of(store, m);
        m.tasks.iter().map(|t| t.details.blocked.clone()).collect()
    }

    #[test]
    fn blocks_and_unblocks_a_task_with_a_reason_and_refuses_subtasks() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        assert_eq!(blocked_in(&mut store, MilestoneId(1)), [None, None]);
        assert_eq!(
            store.block_task(TaskId(2), "  no covers yet  ").unwrap(),
            None
        );
        let m = milestone_of(&mut store, MilestoneId(1));
        assert_eq!(m.blocked_tasks(), 1);
        assert_eq!(m.tasks[1].details.blocked.as_deref(), Some("no covers yet"));
        // Blocking again replaces the reason and says what it was.
        assert_eq!(
            store
                .block_task(TaskId(2), "still no covers")
                .unwrap()
                .as_deref(),
            Some("no covers yet")
        );
        // Standalone tasks take the flag too; it shows on `task`.
        store.block_task(TaskId(5), "no lead").unwrap();
        let loose = store.task(TaskId(5)).unwrap();
        assert_eq!(loose.details.blocked.as_deref(), Some("no lead"));
        // Nothing else changes: no stage, no blocked by, no Done mark.
        let m = milestone_of(&mut store, MilestoneId(1));
        assert!(m.tasks.iter().all(|t| t.stage == Stage::Waiting));
        assert!(m.tasks[1].details.blocked_by.is_empty());

        for (result, want) in [
            (
                store.block_task(TaskId(3), "r").unwrap_err(),
                "#3 is a subtask of #1; blocked is a flag on tasks, so block #1 instead",
            ),
            (
                store.block_task(TaskId(9), "r").unwrap_err(),
                "there is no task #9 on the board",
            ),
            (
                store.block_task(TaskId(1), " ").unwrap_err(),
                "the blocked reason is empty",
            ),
            (
                store.block_task(TaskId(1), "a\nb").unwrap_err(),
                "the blocked reason contains a control character (U+000A)",
            ),
            (
                store.unblock_task(TaskId(1)).unwrap_err(),
                "#1 is not blocked",
            ),
            (
                store.unblock_task(TaskId(3)).unwrap_err(),
                "#3 is a subtask of #1",
            ),
        ] {
            assert!(format!("{result:#}").contains(want), "{result:#}");
        }
        assert_eq!(
            blocked_in(&mut store, MilestoneId(1)),
            [None, Some("still no covers".to_string())]
        );
        assert_eq!(store.unblock_task(TaskId(2)).unwrap(), "still no covers");
        assert_eq!(blocked_in(&mut store, MilestoneId(1)), [None, None]);
        assert_eq!(milestone_of(&mut store, MilestoneId(1)).blocked_tasks(), 0);
    }

    #[test]
    fn the_schema_itself_refuses_a_blocked_subtask_or_an_empty_reason() {
        let (_dir, path) = temp_board();
        let store = board_with_leads(&path);
        let set = |id: i64, reason: &str| {
            store
                .conn
                .execute(
                    "UPDATE task SET blocked = ?1 WHERE id = ?2",
                    params![reason, id],
                )
                .unwrap_err()
                .to_string()
        };
        assert!(set(3, "r").contains("CHECK constraint failed"));
        assert!(set(1, "").contains("CHECK constraint failed"));
    }

    /// Adds a progress post to `thread` straight into the file, at `at`.
    fn raw_progress(store: &Store, thread: ThreadId, author: &str, text: &str, at: i64) {
        store
            .conn
            .execute(
                "INSERT INTO message (thread_id, author, text, posted_at, progress) \
                 VALUES (?1, ?2, ?3, ?4, 1)",
                params![thread.0, author, text, at],
            )
            .unwrap();
    }

    #[test]
    fn the_doing_post_is_the_latest_progress_post_in_the_tasks_threads() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let doing = |store: &mut Store| -> Vec<Option<String>> {
            let m = milestone_of(store, MilestoneId(1));
            let from_load: Vec<_> = (m.tasks.iter())
                .map(|t| t.details.doing.as_ref().map(|p| p.text.clone()))
                .collect();
            // `task` reads it the same way.
            for t in &m.tasks {
                assert_eq!(store.task(t.id).unwrap().details.doing, t.details.doing);
            }
            from_load
        };
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t1, _) = store
            .open_thread(&m1, "dev-2", Some(TaskId(1)), "team", "plan: A then B")
            .unwrap();
        let (other, _) = store.open_thread(&m1, "dev-2", None, "chat", "hi").unwrap();
        // Plain posts, the opening message included, are never the doing line.
        store.post(t1, "dev-2", "copy").unwrap();
        assert_eq!(doing(&mut store), [None, None]);
        // Checked as any post: a leading blank line and the end go, the indent stays (#16).
        let posted = store
            .post_progress(t1, "dev-2", "\n step 1 done \n")
            .unwrap();
        assert_eq!(posted.messages.last().unwrap().text, " step 1 done");
        assert_eq!(doing(&mut store), [Some(" step 1 done".into()), None]);
        // Anyone on the team, assigned or not; a later plain post leaves it.
        store
            .post_progress(t1, "qa", "testing: 3 of 4 pass")
            .unwrap();
        store.post(t1, "senior", "copy, reviewing").unwrap();
        assert_eq!(
            doing(&mut store),
            [Some("testing: 3 of 4 pass".into()), None]
        );
        // Latest by time then id: a same-second post later by id wins, an earlier stamp loses.
        raw_progress(&store, t1, "dev-2", "step 2a", 2_000_000_150);
        raw_progress(&store, t1, "dev-2", "step 2b", 2_000_000_150);
        raw_progress(&store, t1, "dev-2", "stamped early", 1_000);
        assert_eq!(doing(&mut store), [Some("step 2b".into()), None]);
        // A second linked thread counts too, and a resolved one still holds its posts.
        let (t2, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "review", "go")
            .unwrap();
        raw_progress(&store, t2, "dev-2", "fixing review notes", 2_000_000_400);
        store.resolve_thread(t2, "senior", "approved").unwrap();
        assert_eq!(
            doing(&mut store),
            [Some("fixing review notes".into()), None]
        );
        // A thread linked to no task takes no progress post; nothing is written.
        let err = store.post_progress(other, "dev-2", "x").unwrap_err();
        assert!(err.to_string().contains("T2 is linked to no task"), "{err}");
        assert_eq!(store.thread(other).unwrap().total, 1);
        let err = store.post_progress(t2, "dev-2", "x").unwrap_err();
        assert!(err.to_string().contains("resolved"), "{err}");
        // The blocked flag leaves the post alone: the doing line picks which to show.
        store.block_task(TaskId(1), "no covers").unwrap();
        let m = milestone_of(&mut store, MilestoneId(1));
        let post = m.tasks[0].details.doing.clone().unwrap();
        assert_eq!(
            (post.text.as_str(), post.author.as_str(), post.at),
            ("fixing review notes", "dev-2", 2_000_000_400)
        );
        assert_eq!(
            m.tasks[0].doing_line(),
            Some(crate::board::DoingLine::Blocked("no covers"))
        );
    }

    #[test]
    fn moving_a_task_to_done_clears_its_blocked_flag_and_no_other_stage_does() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        store.block_task(TaskId(2), "no covers").unwrap();
        for stage in [Stage::InProgress, Stage::Waiting] {
            store.set_task_stage(TaskId(2), stage).unwrap();
            assert_eq!(
                blocked_in(&mut store, MilestoneId(1))[1].as_deref(),
                Some("no covers")
            );
        }
        // A refused move to Done (an open subtask) keeps the flag.
        store.block_task(TaskId(1), "waiting").unwrap();
        assert!(store.set_task_stage(TaskId(1), Stage::Done).is_err());
        store.set_task_stage(TaskId(2), Stage::Done).unwrap();
        assert_eq!(
            blocked_in(&mut store, MilestoneId(1)),
            [Some("waiting".to_string()), None]
        );
        // ...and a Done task cannot be blocked again until it moves back.
        let err = store.block_task(TaskId(2), "again").unwrap_err();
        assert!(err.to_string().contains("#2 is done"), "{err}");
        // Standalone tasks too.
        store.block_task(TaskId(5), "no lead").unwrap();
        store.set_task_stage(TaskId(5), Stage::Done).unwrap();
        assert_eq!(store.task(TaskId(5)).unwrap().details.blocked, None);
    }

    #[test]
    fn migrates_a_version_six_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal, done) VALUES ('First shop pages', 'g', 1);
             INSERT INTO task (milestone_id, title, stage, assignee)
                 VALUES (1, 'Reviews', 'done', 'dev-1');
             INSERT INTO task (parent_id, title, stage) VALUES (1, 'Sub', 'done');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             INSERT INTO thread (lead_id, task_id, title) VALUES (1, 1, 'Reviews team');
             INSERT INTO message (thread_id, author, text, posted_at)
                 VALUES (1, 'cat-lead', 'go', 100), (1, 'dev-1', 'tests pass', 200);
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 6;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let m = milestone_of(&mut store, MilestoneId(1));
        assert_eq!(m.stage(), Stage::Done);
        assert_eq!(m.blocked_tasks(), 0);
        let t = &m.tasks[0];
        assert_eq!(
            (
                t.subtasks.len(),
                t.details.blocked.as_deref(),
                t.details.doing.as_ref().map(|p| p.text.as_str()),
                m.threads[0].total
            ),
            // Old messages were never marked as progress, so no doing line yet.
            (1, None, None, 2)
        );
        store
            .post_progress(ThreadId(1), "dev-1", "reworking after QA")
            .unwrap();
        assert_eq!(
            store
                .task(TaskId(1))
                .unwrap()
                .details
                .doing
                .map(|p| (p.author, p.text)),
            Some(("dev-1".to_string(), "reworking after QA".to_string()))
        );
        // The old task takes the flag once it is not Done, the old subtask still does not.
        let err = store.block_task(TaskId(1), "r").unwrap_err();
        assert!(err.to_string().contains("#1 is done"), "{err}");
        store.set_task_stage(TaskId(1), Stage::InProgress).unwrap();
        store.block_task(TaskId(1), "reopened by QA").unwrap();
        assert!(store.block_task(TaskId(2), "r").is_err());
        assert_eq!(milestone_of(&mut store, MilestoneId(1)).blocked_tasks(), 1);
    }

    #[test]
    fn latest_posts_gives_each_tasks_last_post_of_any_kind_in_its_threads() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        assert!(store.latest_posts().unwrap().is_empty());
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t1, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "one", "go")
            .unwrap();
        let (t2, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "review", "go")
            .unwrap();
        let (t3, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(2)), "two", "go")
            .unwrap();
        let (chat, _) = store.open_thread(&m1, "qa", None, "chat", "hi").unwrap();
        store
            .conn
            .execute("UPDATE message SET posted_at = 100", [])
            .unwrap();
        // Plain and progress posts both count, by anyone, across the task's threads.
        raw_message(&store, t1, "qa", "copy", 300);
        raw_progress(&store, t2, "dev-2", "step", 500);
        raw_message(&store, t1, "senior", "older", 400);
        raw_message(&store, t3, "dev-1", "done", 250);
        raw_message(&store, chat, "qa", "not a task's", 900);
        // The watch's own flags are not the worker posting.
        raw_message(&store, t3, "system", "@cat-lead #2 is gone", 950);
        assert_eq!(
            store.latest_posts().unwrap(),
            [(TaskId(1), 500), (TaskId(2), 250)]
        );
        // The index on a thread's task is there for it.
        let plan: String = store
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT 1 FROM thread WHERE task_id = 1",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("thread_task"), "{plan}");
    }

    #[test]
    fn load_brings_every_mention_whole_with_its_read_mark() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t, _) = store
            .open_thread(&m1, "dev-2", None, "covers", "hi")
            .unwrap();
        // Five asks and a reply, fewer than the summary's twelve latest: all are loaded.
        for i in 0..5 {
            store
                .post(t, "dev-2", &format!("question: @manager ask {i}"))
                .unwrap();
        }
        store.post(t, "qa", "copy").unwrap();
        let board = store.load().unwrap();
        assert_eq!(board.milestones[0].threads[0].messages.len(), 7);
        // Its authors come from the whole history, not the twelve loaded messages.
        assert_eq!(board.milestones[0].threads[0].authors, ["dev-2", "qa"]);
        for i in 0..10 {
            store.post(t, "qa", &format!("and again {i}")).unwrap();
        }
        store.post(t, "senior", "lgtm").unwrap();
        let board = store.load().unwrap();
        let t1 = &board.milestones[0].threads[0];
        assert!(t1.messages.iter().all(|m| m.author != "dev-2"));
        assert_eq!(t1.authors, ["dev-2", "qa", "senior"]);
        let texts: Vec<&str> = (board.inbox.need_you.iter())
            .map(|a| a.message.text.as_str())
            .collect();
        // Newest first.
        assert_eq!(
            texts,
            [
                "question: @manager ask 4",
                "question: @manager ask 3",
                "question: @manager ask 2",
                "question: @manager ask 1",
                "question: @manager ask 0"
            ]
        );
        assert!(board.inbox.need_you.iter().all(|a| a.thread == t));
        // Resolved: the asks are gone, and so is a decision.
        store
            .post(
                t,
                "cat-lead",
                "decision: a second printer. @manager for your awareness",
            )
            .unwrap();
        store.resolve_thread(t, "cat-lead", "covers in").unwrap();
        let inbox = store.load().unwrap().inbox;
        assert!(inbox.need_you.is_empty() && inbox.awareness.is_empty());
    }

    #[test]
    fn the_mentions_query_reads_only_unread_mentions_on_open_threads() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        // A resolved thread: an unanswered question and an unread decision.
        let (gone, _) = store
            .open_thread(&m1, "dev-2", None, "k", "question: @manager a")
            .unwrap();
        store
            .post(gone, "cat-lead", "decision: b. @manager")
            .unwrap();
        store.resolve_thread(gone, "cat-lead", "done").unwrap();
        // An open thread: an unread decision, a read one, and a message with no mention.
        let (open, _) = store
            .open_thread(&m1, "cat-lead", None, "c", "decision: c. @manager")
            .unwrap();
        let unread = store.thread(open).unwrap().messages[0].id;
        let posted = store.post(open, "cat-lead", "decision: d. @manager");
        let read = posted.unwrap().messages.last().unwrap().id;
        assert_eq!(store.mark_read(read).unwrap(), Some(true));
        store.post(open, "dev-1", "no mention").unwrap();
        // The query itself, which the load and tower repo remove's count both use, reads
        // only the unread mention on the open thread: all the inbox lists.
        let ids: Vec<i64> = (store.conn)
            .prepare(&format!("SELECT m.id FROM message m {MENTIONS_WHERE}"))
            .unwrap()
            .query_map(params![OWNER_HANDLE], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, [unread]);
        let inbox = store.load().unwrap().inbox;
        let listed: Vec<i64> = inbox.all().map(|a| a.message.id).collect();
        assert_eq!(listed, [unread]);
    }

    #[test]
    fn a_delivery_failure_is_for_the_owners_awareness_and_a_question_needs_them() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t, _) = store
            .open_thread(
                &m1,
                "dev-2",
                None,
                "covers",
                "question: @manager can you approve?",
            )
            .unwrap();
        // Posted the way the watch's drain posts one.
        let (at, _) = store
            .delivery_failed(&["lead:M1"], t, 500, |_| {
                format!(
                    "{}Catalogue, the lead of M1: it has not attached its Claude inbox. \
                     Its posts stay on the board.",
                    crate::board::DELIVERY_FAILURE
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(at, t);
        let board = store.load().unwrap();
        let texts = |s: &[crate::inbox::Alert]| {
            s.iter().map(|a| a.message.text.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            texts(&board.inbox.need_you),
            ["question: @manager can you approve?"]
        );
        let awareness = texts(&board.inbox.awareness);
        assert_eq!(awareness.len(), 1);
        assert!(awareness[0].starts_with("@manager could not deliver to Catalogue"));
        // The lead's badges and the alert line count it the same way.
        assert_eq!(board.inbox.badges(RepoId(1)), (1, 1));
        assert_eq!(
            board.inbox.newest().map(|a| a.message.text.as_str()),
            Some("question: @manager can you approve?")
        );
        // The owner can mark it read, like a decision: it leaves the inbox, the question
        // stays.
        let failure = board.inbox.awareness[0].message.id;
        assert_eq!(store.mark_read(failure).unwrap(), Some(true));
        let inbox = store.load().unwrap().inbox;
        assert!(inbox.awareness.is_empty(), "{inbox:?}");
        assert_eq!(inbox.need_you.len(), 1);
    }

    #[test]
    fn the_owner_marks_need_you_and_awareness_items_read_once_and_nothing_else() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t, _) = store
            .open_thread(&m1, "cat-lead", None, "colours", "go")
            .unwrap();
        let post = |s: &mut Store, by: &str, text: &str| {
            s.post(t, by, text).unwrap().messages.last().unwrap().id
        };
        let reply = post(&mut store, "cat-lead", "@manager done: merged, it works");
        let ask = post(&mut store, "cat-lead", "question: @manager which order?");
        let other = post(&mut store, "dev-2", "question: @manager approve?");
        let plain = post(&mut store, "cat-lead", "decision: no mention");
        let blocked = post(&mut store, "dev-2", "blocked: the covers are missing");
        let quoted = post(&mut store, "dev-2", "question: is `@manager` red?");
        let ids = |s: &[crate::inbox::Alert]| s.iter().map(|a| a.message.id).collect::<Vec<_>>();
        let inbox = store.load().unwrap().inbox;
        assert_eq!(
            (ids(&inbox.need_you), ids(&inbox.awareness)),
            (vec![other, ask], vec![reply])
        );
        assert_eq!(store.mark_read(reply).unwrap(), Some(true));
        // The owner clears a need-you item: it leaves the alert line and the inbox.
        assert_eq!(store.mark_read(ask).unwrap(), Some(true));
        // Again: nothing changes, and it says so.
        assert_eq!(store.mark_read(ask).unwrap(), Some(false));
        let inbox = store.load().unwrap().inbox;
        assert_eq!(
            (ids(&inbox.need_you), ids(&inbox.awareness)),
            (vec![other], vec![])
        );
        assert_eq!(inbox.newest().map(|a| a.message.id), Some(other));
        for refused in [plain, blocked, quoted] {
            let err = store.mark_read(refused).unwrap_err().to_string();
            assert!(err.contains("is not for the owner"), "{err}");
        }
        // One not on the board (removed since) is no error: it says so, writing nothing.
        assert_eq!(store.mark_read(9999).unwrap(), None);
        // A refused mark writes nothing.
        let marked: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM message WHERE read_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marked, 2);
        // Resolved: the other question goes too.
        store.resolve_thread(t, "cat-lead", "ordered").unwrap();
        let inbox = store.load().unwrap().inbox;
        assert_eq!(inbox, Inbox::default());
    }

    #[test]
    fn the_owner_is_mentioned_in_any_case_as_delivery_reads_it() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t, _) = store
            .open_thread(
                &m1,
                "cat-lead",
                None,
                "order",
                "question: @Manager which order?",
            )
            .unwrap();
        store.post(t, "cat-lead", "@MANAGER done: merged").unwrap();
        let texts = |s: &[crate::inbox::Alert]| {
            s.iter().map(|a| a.message.text.clone()).collect::<Vec<_>>()
        };
        let inbox = store.load().unwrap().inbox;
        assert_eq!(texts(&inbox.need_you), ["question: @Manager which order?"]);
        assert_eq!(texts(&inbox.awareness), ["@MANAGER done: merged"]);
        // Resolved: both go.
        store.resolve_thread(t, "cat-lead", "ordered").unwrap();
        let inbox = store.load().unwrap().inbox;
        assert!(inbox.need_you.is_empty() && inbox.awareness.is_empty());
    }

    #[test]
    fn migrates_a_version_seven_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('First shop pages', 'g');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             INSERT INTO thread (lead_id, title) VALUES (1, 'Colours');
             INSERT INTO message (thread_id, author, text, posted_at) VALUES
                 (1, 'cat-lead', 'decision: covers first. @manager for your awareness', 100),
                 (1, 'dev-1', 'question: @manager approve?', 200),
                 (1, 'qa', 'copy', 300);
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 7;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let board = store.load().unwrap();
        assert_eq!(board.milestones[0].threads[0].total, 3);
        // Old messages start unread.
        let ids = |s: &[crate::inbox::Alert]| s.iter().map(|a| a.message.id).collect::<Vec<_>>();
        assert_eq!(
            (ids(&board.inbox.awareness), ids(&board.inbox.need_you)),
            (vec![1], vec![2])
        );
        assert_eq!(store.mark_read(1).unwrap(), Some(true));
        assert!(store.load().unwrap().inbox.awareness.is_empty());
    }

    #[test]
    fn system_posts_go_to_the_tasks_newest_open_thread_or_a_new_one() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (old, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "old", "go")
            .unwrap();
        let (newer, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "newer", "go")
            .unwrap();
        let (gone, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "resolved", "go")
            .unwrap();
        store
            .conn
            .execute("UPDATE message SET posted_at = 100", [])
            .unwrap();
        raw_message(&store, newer, "dev-1", "latest", 500);
        raw_message(&store, old, "dev-1", "older", 400);
        raw_message(&store, gone, "dev-1", "newest but resolved", 600);
        store.resolve_thread(gone, "qa", "fine").unwrap();
        // Stamped no earlier than the thread's latest message, as any post.
        assert_eq!(
            store
                .watch_step(TaskId(1), FlagChange::None, Some("@manager x"), 450)
                .unwrap(),
            Some(newer)
        );
        let last = store.thread(newer).unwrap().messages.pop().unwrap();
        assert_eq!((last.author.as_str(), last.at), ("system", 500));
        // #2 has no thread: one opens on its lead's board, linked to it, by system.
        let t = (store.watch_step(TaskId(2), FlagChange::None, Some("@cat-lead y"), 700))
            .unwrap()
            .unwrap();
        let thread = store.thread(t).unwrap();
        assert_eq!(
            (thread.task, thread.by.as_str(), thread.title.as_str()),
            (Some(TaskId(2)), "system", "Watch on #2")
        );
        assert_eq!(milestone_of(&mut store, MilestoneId(1)).threads.len(), 4);
        // A subtask, a standalone task and a task with no lead have no board to post on.
        for task in [TaskId(3), TaskId(5)] {
            let err = (store.watch_step(task, FlagChange::None, Some("z"), 1))
                .unwrap_err()
                .to_string();
            assert!(err.contains("on no lead's board"), "{task}: {err}");
        }
        // A task whose milestone is in no repo has no board; one in a repo with no lead
        // neither.
        let m3_task = store.add_task(MilestoneId(3), "no lead").unwrap();
        let err = (store.watch_step(m3_task, FlagChange::None, Some("z"), 1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("on no lead's board"), "{err}");
        store.exec_sql("INSERT INTO repo (path) VALUES (NULL)");
        let leadless = RepoId(store.conn.last_insert_rowid());
        store.exec_sql(&format!(
            "UPDATE milestone SET repo_id = {} WHERE id = 3",
            leadless.0
        ));
        let err = (store.watch_step(m3_task, FlagChange::None, Some("z"), 1))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("has no lead yet, so it has no message board"),
            "{err}"
        );
        // The CLI's way in still refuses system.
        assert!(store.post(newer, "system", "spoof").is_err());
        assert!(store.open_thread(&m1, "System", None, "t", "m").is_err());
    }

    #[test]
    fn a_watch_step_flags_and_posts_together_or_not_at_all() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let flag = |task| Flag {
            task,
            signal: Signal::Orphaned,
            worker: "dev-1".into(),
            quiet_since: None,
            raised_at: 100,
            lead_told_at: None,
        };
        // The post is refused (a standalone task has no board): no flag either.
        let loose = flag(TaskId(5));
        let err = store.watch_step(TaskId(5), FlagChange::Raise(&loose), Some("x"), 100);
        assert!(err.is_err());
        assert!(store.flags().unwrap().is_empty());
        // The flag is refused: no post either.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER no_flags BEFORE INSERT ON watch_flag \
                 BEGIN SELECT RAISE(ABORT, 'broken on purpose'); END;",
            )
            .unwrap();
        let ok = flag(TaskId(1));
        let err = store.watch_step(TaskId(1), FlagChange::Raise(&ok), Some("x"), 100);
        assert!(format!("{:#}", err.unwrap_err()).contains("broken on purpose"));
        assert!(milestone_of(&mut store, MilestoneId(1)).threads.is_empty());
        store.conn.execute_batch("DROP TRIGGER no_flags").unwrap();
        let t = store.watch_step(TaskId(1), FlagChange::Raise(&ok), Some("x"), 100);
        assert_eq!(t.unwrap(), Some(ThreadId(1)));
        assert_eq!(store.flags().unwrap(), [ok]);
        assert_eq!(store.thread(ThreadId(1)).unwrap().total, 1);
    }

    #[test]
    fn the_watch_keeps_when_it_first_saw_each_task_with_its_worker() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let pair = |t, w: &str| (TaskId(t), w.to_string());
        let seen = store
            .watch_seen(&[pair(1, "dev-one"), pair(2, "qa")], 100)
            .unwrap();
        assert_eq!((seen[&TaskId(1)], seen[&TaskId(2)]), (100, 100));
        // Kept across opens (a restart), ignoring the name's case.
        drop(store);
        let mut store = Store::open(&path).unwrap();
        let seen = store
            .watch_seen(&[pair(1, "Dev-One"), pair(2, "qa")], 500)
            .unwrap();
        assert_eq!((seen[&TaskId(1)], seen[&TaskId(2)]), (100, 100));
        // Reassigned: afresh. No longer watched: forgotten, so afresh when back.
        let seen = store.watch_seen(&[pair(1, "dev-2")], 600).unwrap();
        assert_eq!((seen[&TaskId(1)], seen.len()), (600, 1));
        let seen = store
            .watch_seen(&[pair(1, "dev-one"), pair(2, "qa")], 700)
            .unwrap();
        assert_eq!((seen[&TaskId(1)], seen[&TaskId(2)]), (700, 700));
    }

    #[test]
    fn the_watch_remembers_flags_until_they_are_cleared() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        assert!(store.flags().unwrap().is_empty());
        let flag = Flag {
            task: TaskId(1),
            signal: Signal::Stopped,
            worker: "dev-1".into(),
            quiet_since: None,
            raised_at: 100,
            lead_told_at: None,
        };
        let orphan = Flag {
            task: TaskId(2),
            signal: Signal::Orphaned,
            quiet_since: Some(50),
            ..flag.clone()
        };
        store
            .watch_step(TaskId(2), FlagChange::Raise(&orphan), None, 0)
            .unwrap();
        store
            .watch_step(TaskId(1), FlagChange::Raise(&flag), None, 0)
            .unwrap();
        (store.watch_step(
            TaskId(1),
            FlagChange::LeadTold(Signal::Stopped, "dev-1"),
            None,
            220,
        ))
        .unwrap();
        let told = Flag {
            lead_told_at: Some(220),
            ..flag.clone()
        };
        assert_eq!(store.flags().unwrap(), [told, orphan.clone()]);
        (store.watch_step(
            TaskId(1),
            FlagChange::Clear(Signal::Stopped, "dev-1"),
            None,
            0,
        ))
        .unwrap();
        assert_eq!(store.flags().unwrap(), [orphan]);
        for s in Signal::ALL {
            let f = Flag {
                signal: s,
                ..flag.clone()
            };
            store
                .watch_step(f.task, FlagChange::Raise(&f), None, 0)
                .unwrap();
        }
        assert_eq!(store.flags().unwrap().len(), 5);
    }

    #[test]
    fn a_task_holds_a_prompt_flag_per_worker_ignoring_case() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let prompt = |worker: &str| Flag {
            task: TaskId(1),
            signal: Signal::Prompt,
            worker: worker.into(),
            quiet_since: None,
            raised_at: 100,
            lead_told_at: None,
        };
        for f in [prompt("dev-1"), prompt("qa"), prompt("QA")] {
            (store.watch_step(f.task, FlagChange::Raise(&f), None, 0)).unwrap();
        }
        // QA replaced qa: the same worker.
        assert_eq!(store.flags().unwrap(), [prompt("dev-1"), prompt("QA")]);
        (store.watch_step(TaskId(1), FlagChange::Clear(Signal::Prompt, "qa"), None, 0)).unwrap();
        assert_eq!(store.flags().unwrap(), [prompt("dev-1")]);
    }

    #[test]
    fn task_names_gives_who_each_tasks_threads_named_last_by_post_or_mention() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let m1 = BoardOf::Milestone(MilestoneId(1));
        let (t1, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(1)), "one", "go @dev-1")
            .unwrap();
        let (t2, _) = store
            .open_thread(&m1, "cat-lead", Some(TaskId(2)), "two", "go")
            .unwrap();
        let (chat, _) = store.open_thread(&m1, "qa", None, "chat", "hi").unwrap();
        store
            .conn
            .execute("UPDATE message SET posted_at = 100", [])
            .unwrap();
        raw_message(&store, t1, "dev-1", "plan: list", 200);
        raw_message(
            &store,
            t1,
            "cat-lead",
            "@Qa-One please test, @senior review",
            300,
        );
        raw_message(&store, t1, "qa-one", "copy", 250);
        raw_message(&store, chat, "senior", "not a task's", 900);
        // The watch's own messages name no one, though they name workers.
        raw_message(&store, t2, "system", "@cat-lead dev-9 is gone @dev-9", 950);
        let named = store.task_names().unwrap();
        let want = |t, name: &str, posted, mentioned| {
            let name = name.to_string();
            (
                TaskId(t),
                Named {
                    name,
                    posted,
                    mentioned,
                },
            )
        };
        // A post and a mention are told apart; the name is as last written.
        assert_eq!(
            named,
            [
                want(1, "cat-lead", Some(300), None),
                want(1, "dev-1", Some(200), Some(100)),
                want(1, "Qa-One", Some(250), Some(300)),
                want(1, "senior", None, Some(300)),
                want(2, "cat-lead", Some(100), None),
            ]
        );
        // A Done task's threads name no one.
        store.set_task_stage(TaskId(2), Stage::Done).unwrap();
        assert_eq!(store.task_names().unwrap(), named[..4]);
    }

    #[test]
    fn done_by_is_kept_while_the_task_is_done_and_cleared_when_it_leaves() {
        let (_dir, path) = temp_board();
        let mut store = board_with_leads(&path);
        let done_by = |store: &mut Store, t: i64| store.task(TaskId(t)).unwrap().details.done_by;
        store
            .set_task_stage_by(TaskId(2), Stage::Done, Some(" cat-lead "))
            .unwrap();
        assert_eq!(done_by(&mut store, 2).as_deref(), Some("cat-lead"));
        let loaded = store.load().unwrap();
        let t = (loaded.milestones.iter().flat_map(|m| &m.tasks))
            .find(|t| t.id == TaskId(2))
            .unwrap();
        assert_eq!(t.details.done_by.as_deref(), Some("cat-lead"));
        store.set_task_stage(TaskId(2), Stage::InProgress).unwrap();
        assert_eq!(done_by(&mut store, 2), None);
        // Done without --by records nobody.
        store.set_task_stage(TaskId(2), Stage::Done).unwrap();
        assert_eq!(done_by(&mut store, 2), None);
        // --by only with done, and never as system.
        let e = store
            .set_task_stage_by(TaskId(2), Stage::Waiting, Some("cat-lead"))
            .unwrap_err();
        assert!(e.to_string().contains("only with done"), "{e}");
        store.set_task_stage(TaskId(2), Stage::Waiting).unwrap();
        let e = store
            .set_task_stage_by(TaskId(2), Stage::Done, Some("system"))
            .unwrap_err();
        assert!(e.to_string().contains("kept for tower"), "{e}");
        assert_eq!(store.task(TaskId(2)).unwrap().stage, Stage::Waiting);
    }

    #[test]
    fn migrates_a_version_eight_board_and_keeps_its_data() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('First shop pages', 'g');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             INSERT INTO task (milestone_id, title, stage, assignee)
                 VALUES (1, 'Bookmarks', 'in-progress', 'dev-1');
             INSERT INTO thread (lead_id, task_id, title) VALUES (1, 1, '#1');
             INSERT INTO message (thread_id, author, text, posted_at, progress, read_at) VALUES
                 (1, 'cat-lead', 'decision: covers first. @manager', 100, 0, 150),
                 (1, 'dev-1', 'testing', 200, 1, NULL);
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 8;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let board = store.load().unwrap();
        let task = &board.milestones[0].tasks[0];
        assert_eq!(task.details.assignee.as_deref(), Some("dev-1"));
        assert_eq!(task.details.doing.as_ref().unwrap().text, "testing");
        // The decision read before the upgrade stays read: no item.
        assert_eq!(board.inbox, Inbox::default());
        assert_eq!(store.latest_posts().unwrap(), [(TaskId(1), 200)]);
        // The watch starts with nothing remembered, and its table takes flags.
        assert!(store.flags().unwrap().is_empty());
        let flag = Flag {
            task: TaskId(1),
            signal: Signal::Silent,
            worker: "dev-1".into(),
            quiet_since: Some(200),
            raised_at: 2000,
            lead_told_at: None,
        };
        store
            .watch_step(TaskId(1), FlagChange::Raise(&flag), None, 0)
            .unwrap();
        assert_eq!(store.flags().unwrap(), [flag]);
        let bad = store.conn.execute(
            "INSERT INTO watch_flag (task_id, signal, worker, raised_at) VALUES (1, 'loud', 'x', 1)",
            [],
        );
        assert!(bad.is_err(), "a signal the watch does not know is refused");
    }

    #[test]
    fn migrates_a_version_eleven_board_and_keeps_the_watchs_flags() {
        let (_dir, path) = temp_board();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..11] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO milestone (title, goal) VALUES ('MVP', 'g');
             INSERT INTO lead (milestone_id, name, session, colour)
                 VALUES (1, 'Catalogue Lead', 'cat-lead', 'blue');
             INSERT INTO task (milestone_id, title, stage, assignee)
                 VALUES (1, 'Bookmarks', 'in-progress', 'dev-1');
             INSERT INTO watch_flag (task_id, signal, worker, quiet_since, raised_at, lead_told_at)
                 VALUES (1, 'stopped', 'dev-1', 200, 300, 420), (1, 'prompt', 'dev-1', NULL, 500, NULL);
             PRAGMA journal_mode = WAL;
             PRAGMA user_version = 11;",
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(read_version(&store.conn).unwrap(), SCHEMA_VERSION);
        let flag = |signal, quiet_since, raised_at, lead_told_at| Flag {
            task: TaskId(1),
            signal,
            worker: "dev-1".into(),
            quiet_since,
            raised_at,
            lead_told_at,
        };
        assert_eq!(
            store.flags().unwrap(),
            [
                flag(Signal::Prompt, None, 500, None),
                flag(Signal::Stopped, Some(200), 300, Some(420)),
            ]
        );
        // A second worker's prompt flag on the same task now fits beside the first.
        let qa = Flag {
            worker: "qa".into(),
            ..flag(Signal::Prompt, None, 600, None)
        };
        (store.watch_step(TaskId(1), FlagChange::Raise(&qa), None, 0)).unwrap();
        assert_eq!(store.flags().unwrap().len(), 3);
        let bad = store.conn.execute(
            "INSERT INTO watch_flag (task_id, signal, worker, raised_at) VALUES (1, 'loud', 'x', 1)",
            [],
        );
        assert!(
            bad.is_err(),
            "a signal the watch does not know is still refused"
        );
    }
}
