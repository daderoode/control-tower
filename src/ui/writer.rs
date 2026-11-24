//! The owner's own writes from the dashboard: posting as `manager`, marking an inbox item
//! read, folding repos and milestones in the tree and starting a lead (a new milestone, in
//! a repo when the owner started it on one, and Claude leading it in a new place the
//! terminal module opens on that repo's main checkout). The CLI refuses the first two to
//! agents, so the dashboard makes them itself, through the store's checked writes (the same
//! validation and transactions `tower` uses), each on a thread of its own so the UI never
//! waits. The writing connection is opened on that thread and closed with it; the dashboard
//! keeps reading through its read-only connection. The store's busy timeout bounds a write
//! waiting on another writer.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::{Context, Result};

use crate::board::{MAX_LINE_CHARS, MilestoneId, OWNER_AUTHOR, RepoId, ThreadId};
use crate::store::Store;
use crate::terminal::{LeadStart, Terminal};

/// A write the owner asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Post `text` to `thread` as `manager`.
    Post { thread: ThreadId, text: String },
    /// Mark the awareness message `id` read: the owner's `r`. `item` names it in words, as
    /// the inbox does, for the notice.
    MarkRead { id: i64, item: String },
    /// Clear the need-you message `id` (the owner's `r`): marked read, said as cleared,
    /// naming `item`.
    Clear { id: i64, item: String },
    /// Mark these awareness messages read, as the owner opened their thread: quiet when
    /// it works.
    Seen(Vec<i64>),
    /// Fold `repos` and the task lists of `milestones` in the tree, or unfold them.
    Fold {
        repos: Vec<RepoId>,
        milestones: Vec<MilestoneId>,
        done_groups: Vec<RepoId>,
        folded: bool,
    },
    /// Start a lead on `line`, the owner's goal in one line, on `repo` when given, with the
    /// terminal module's executable at `terminal`, from `home` (`HOME`, `None` when it is
    /// not set). The dashboard reads `home` from its environment
    /// (`Write::start_lead_from_env`), so the write itself never does. The lead gets the
    /// board this write is made on.
    StartLead {
        line: String,
        repo: Option<RepoId>,
        terminal: PathBuf,
        home: Option<String>,
    },
}

impl Write {
    /// The start of a lead on `line`, on `repo` when given, with the terminal module's
    /// executable at `terminal`, from `HOME` as `env` has it (the dashboard's:
    /// `std::env::var_os`). A `HOME` that is not UTF-8 counts as not set.
    pub fn start_lead_from_env(
        line: String,
        repo: Option<RepoId>,
        terminal: PathBuf,
        env: impl Fn(&'static str) -> Option<OsString>,
    ) -> Write {
        Write::StartLead {
            line,
            repo,
            terminal,
            home: env("HOME").and_then(|h| h.into_string().ok()),
        }
    }
}

/// Makes `write` on the board at `path` now, on the calling thread, and says how it went in
/// one line, with whether it failed.
pub fn write_now(path: &Path, write: &Write) -> (String, bool) {
    match make(path, write) {
        Ok(said) => (said, false),
        Err(e) => (format!("{e:#}"), true),
    }
}

fn make(path: &Path, write: &Write) -> Result<String> {
    let mut store = Store::open(path)?;
    Ok(match write {
        Write::Post { thread, text } => {
            store.post(*thread, OWNER_AUTHOR, text)?;
            format!("posted to {thread}")
        }
        Write::MarkRead { id, item } => {
            match (store.mark_read(*id)).with_context(|| format!("cannot mark {item} read"))? {
                Some(true) => format!("marked read: {item}"),
                Some(false) => format!("{item} was already read"),
                None => format!("{item} is no longer on the board"),
            }
        }
        Write::Clear { id, item } => {
            match (store.mark_read(*id)).with_context(|| format!("cannot clear {item}"))? {
                Some(true) => format!("cleared: {item}"),
                Some(false) => format!("{item} was already cleared"),
                None => format!("{item} is no longer on the board"),
            }
        }
        Write::Seen(ids) => {
            store.mark_seen(ids)?;
            String::new()
        }
        Write::Fold {
            repos,
            milestones,
            done_groups,
            folded,
        } => {
            store.set_folds(repos, milestones, done_groups, *folded)?;
            let ids: Vec<String> = (repos.iter().map(|r| r.to_string()))
                .chain(milestones.iter().map(|m| m.to_string()))
                .chain(done_groups.iter().map(|r| format!("{r}'s Done group")))
                .collect();
            let how = if *folded { "folded" } else { "unfolded" };
            format!("{how} {}", ids.join(", "))
        }
        Write::StartLead {
            line,
            repo,
            terminal,
            home,
        } => {
            let home = (home.as_deref()).context("cannot start a lead: HOME is not set")?;
            let at = Start { repo: *repo, home };
            let terminal = crate::terminal::lead_module(terminal.clone());
            start_lead(&mut store, path, &*terminal, line, at)?
        }
    })
}

/// Words of the title a lead's name takes at most, before " Lead".
const NAME_WORDS: usize = 3;
/// The longest lead name made, " Lead" included.
const NAME_MAX: usize = 40;

/// The lead's name for milestone `m` titled `title`: its first words plus " Lead", made
/// of the board's name characters only (other characters dropped, any run of spaces one),
/// starting with a letter or digit, so no command takes it for an option, at most
/// `NAME_MAX` long; "<m> Lead" when no word is left. It always passes the board's name
/// rule, and it is the only text from the owner that reaches the terminal.
pub fn lead_name(title: &str, m: MilestoneId) -> String {
    let kept: String = (title.chars())
        .filter(|c| c.is_ascii_alphanumeric() || " -_.".contains(*c) || c.is_whitespace())
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .collect();
    let mut words: Vec<&str> = kept.split_whitespace().take(NAME_WORDS).collect();
    // "Audiobooks Lead" names a lead "Audiobooks Lead", not "Audiobooks Lead Lead".
    if words.last().is_some_and(|w| w.eq_ignore_ascii_case("lead")) {
        words.pop();
    }
    let room = NAME_MAX - " Lead".len();
    let mut name = String::new();
    for w in words {
        let sep = usize::from(!name.is_empty());
        if name.len() + sep + w.len() <= room {
            if sep == 1 {
                name.push(' ');
            }
            name.push_str(w);
        } else {
            if name.is_empty() {
                name.push_str(&w[..room]);
            }
            break;
        }
    }
    if name.is_empty() {
        return format!("{m} Lead");
    }
    format!("{name} Lead")
}

/// `text` in single quotes for a POSIX shell, each `'` in it written as `'\''`, so the
/// shell reads it as one word, exactly as given.
pub fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The command a lead runs in its new place, one word per argument: Claude named `name`,
/// leading `m`.
pub fn lead_command(name: &str, m: MilestoneId) -> Vec<String> {
    let lead = format!("/control-tower:lead {m}");
    ["claude", "-n", name, &lead].map(String::from).to_vec()
}

/// Where a lead starts: on `repo`, when given, in its main checkout, else in no repo yet,
/// in `home`.
pub struct Start<'a> {
    pub repo: Option<RepoId>,
    pub home: &'a str,
}

/// Starts a lead on the owner's `line`. On the repo `at` names: adds the milestone (title
/// the line cut to `MAX_LINE_CHARS`, goal the line) and registers its lead on the repo in
/// one write (`Store::start_repo_lead`), so the tree shows the lead at once and a second
/// start on the repo is refused; then the terminal module opens a place named after the
/// lead on the repo's main checkout, out of focus, with Claude in it under the registered
/// name, running the lead skill, which finds itself registered and attaches. A repo that is
/// gone or already has a lead, or a directory to start in that is not one, is refused
/// before anything is written. In no repo (`at.repo` `None`): only the milestone is added,
/// in `home`, and the skill registers the lead once it has its repo. A start the terminal
/// fails is undone on the board (`undone`), or, where it cannot be, says so; the module
/// leaves no place of its own behind, or says it may have. The board `db`, made absolute,
/// is set as `TOWER_DB` in the new place, so the lead uses the same board. A name a lead on
/// the board has already (as its name or session, ignoring case), or a task's assignee,
/// gets the milestone id after it (`Store::start_repo_lead`, `Store::start_lead_in_no_repo`).
pub fn start_lead(
    store: &mut Store,
    db: &Path,
    terminal: &dyn Terminal,
    line: &str,
    at: Start,
) -> Result<String> {
    let line = line.trim();
    let title: String = line.chars().take(MAX_LINE_CHARS).collect();
    let board = (std::path::absolute(db))
        .context("cannot start a lead: the board's path cannot be made absolute")?;
    let board = (board.to_str()).context("cannot start a lead: the board's path is not UTF-8")?;
    let mut cwd = at.home.to_string();
    let mut place = String::new();
    let (m, name) = match at.repo {
        Some(id) => {
            let board = store.load()?;
            let Some(r) = board.repo(id) else {
                anyhow::bail!("cannot start a lead on {id}: it is no longer on the board");
            };
            if let Some(l) = &r.lead {
                anyhow::bail!("{} already has its lead, {}", r.name(), l.name);
            }
            let Some(p) = &r.path else {
                anyhow::bail!("{}", no_checkout(r));
            };
            if !Path::new(p).is_dir() {
                anyhow::bail!(
                    "cannot start a lead on {}: its main checkout {p} is not a directory; \
                     tower repo set {} --path <checkout>, then n",
                    r.name(),
                    r.id
                );
            }
            cwd = p.clone();
            place = format!(" on {}", r.name());
            // The "no lead" check runs again in the write's own transaction.
            let (m, lead) = store.start_repo_lead(&title, line, id, |m| lead_name(&title, m))?;
            (m, lead.name)
        }
        None => {
            if !Path::new(at.home).is_dir() {
                anyhow::bail!(
                    "cannot start a lead in your home directory {}: it is not a directory",
                    at.home
                );
            }
            store.start_lead_in_no_repo(&title, line, |m| lead_name(&title, m))?
        }
    };
    let command = lead_command(&name, m);
    let lead = LeadStart {
        name: &name,
        dir: &cwd,
        board,
        command: &command,
    };
    // On a failure, what this start wrote is undone, so `n` can be tried again.
    if let Err(e) = terminal.start_lead(db, &lead) {
        let registered = at.repo.map(|r| (r, name.as_str()));
        return Err(undone(store, m, registered, e));
    }
    Ok(format!(
        "started {name} for {m}{place} in {}",
        terminal.name()
    ))
}

/// Why a lead cannot start on `r`: its main checkout is not on the board, so its place
/// would open nowhere it can work from.
pub fn no_checkout(r: &crate::board::Repo) -> String {
    format!(
        "{} has no main checkout on the board: tower repo set {} --path <checkout>, then n",
        r.name(),
        r.id
    )
}

/// `e`, a start that failed after milestone `m` (and, on a repo, its lead) was written,
/// with what undoing it did: removed again, so `n` can be tried again, or kept, saying how
/// to start the lead by hand.
fn undone(
    store: &mut Store,
    m: MilestoneId,
    lead: Option<(RepoId, &str)>,
    e: anyhow::Error,
) -> anyhow::Error {
    let name = lead.map_or(String::new(), |(_, n)| format!(" {}", sh_quote(n)));
    match store.undo_start(m, lead) {
        Ok(removed) if !removed.is_empty() => e.context(format!(
            "could not start the lead, so {} {} removed again; press n to try again",
            removed.join(" and "),
            if removed.len() == 1 { "was" } else { "were" }
        )),
        Ok(_) => e.context(format!(
            "could not start the lead for {m}; {m} is in use and stays on the board, start it \
             from a session with claude -n{name} /control-tower:lead {m}"
        )),
        Err(u) => e.context(format!(
            "could not start the lead for {m}, and could not undo it either ({u:#}); {m} \
             stays on the board, start it from a session with claude -n{name} \
             /control-tower:lead {m}"
        )),
    }
}

/// Starts `write` on its own thread; what `write_now` says arrives on the returned channel.
pub fn spawn(path: &Path, write: Write) -> Receiver<(String, bool)> {
    let (tx, rx) = mpsc::channel();
    let path: PathBuf = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(write_now(&path, &write));
    });
    rx
}

/// What a write in flight on `rx` says now: `None` while it runs, then its outcome, or a
/// failure when its thread died without one, so the caller never waits forever.
pub fn poll(rx: &Receiver<(String, bool)>) -> Option<(String, bool)> {
    match rx.try_recv() {
        Ok(said) => Some(said),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            Some(("the write stopped without an answer".into(), true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOf, MilestoneId, check_session};

    /// A board with a lead, an open thread T1 holding a decision for the owner's awareness
    /// (message 1) and a need-you ask (message 2), and a resolved thread T2.
    fn board() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let mut s = Store::open(&path).unwrap();
        let m = s.add_milestone("M", "g", None).unwrap();
        s.lead_milestone(m, "Catalogue", "cat-lead", None).unwrap();
        let of = BoardOf::Milestone(MilestoneId(1));
        let decision = "decision: the shelf shows 12 books, not 10. @manager for your awareness";
        let (t1, _) = s
            .open_thread(&of, "cat-lead", None, "Colours", decision)
            .unwrap();
        s.post(t1, "dev-2", "question: @manager can you approve?")
            .unwrap();
        let (t2, _) = s
            .open_thread(&of, "cat-lead", None, "Old", "Open.")
            .unwrap();
        s.resolve_thread(t2, "cat-lead", "done").unwrap();
        (dir, path)
    }

    #[test]
    fn a_post_goes_through_the_store_as_manager_with_its_checks() {
        let (_dir, path) = board();
        let post = |thread: i64, text: &str| Write::Post {
            thread: ThreadId(thread),
            text: text.into(),
        };
        let said = write_now(&path, &post(1, "-x copy\napproved"));
        assert_eq!(said, ("posted to T1".into(), false));
        let t = Store::open(&path).unwrap().thread(ThreadId(1)).unwrap();
        let last = t.messages.last().unwrap();
        assert_eq!(
            (last.author.as_str(), last.text.as_str()),
            ("manager", "-x copy\napproved")
        );
        // Refused as `tower post` refuses, saying why, and nothing is written.
        for (thread, text, why) in [
            (2, "hi", "T2 is resolved; resolved threads are read-only"),
            (1, " \n", "the message is empty"),
            (1, "a\u{1b}b", "control character"),
            (9, "hi", "T9"),
        ] {
            let (msg, failed) = write_now(&path, &post(thread, text));
            assert!(failed && msg.contains(why), "{msg:?} lacks {why:?}");
        }
        let t = Store::open(&path).unwrap().thread(ThreadId(1)).unwrap();
        assert_eq!(t.total, 3);
    }

    #[test]
    fn mark_read_goes_through_the_store_and_clears_a_need_you_item_too() {
        let (_dir, path) = board();
        // Each notice names the item as the dashboard gave it, never by its message id.
        let mark = |id| Write::MarkRead {
            id,
            item: "qa in ‘New-book ribbon’ · storefront".into(),
        };
        let clear = |id| Write::Clear {
            id,
            item: "cat-lead in ‘Task #13 team’ · storefront".into(),
        };
        assert_eq!(
            write_now(&path, &mark(1)),
            (
                "marked read: qa in ‘New-book ribbon’ · storefront".into(),
                false
            )
        );
        assert_eq!(
            write_now(&path, &mark(1)),
            (
                "qa in ‘New-book ribbon’ · storefront was already read".into(),
                false
            )
        );
        // The owner clears a question explicitly, and it says so; a message not for the
        // owner is refused, naming the item first.
        assert_eq!(
            write_now(&path, &clear(2)),
            (
                "cleared: cat-lead in ‘Task #13 team’ · storefront".into(),
                false
            )
        );
        assert_eq!(
            write_now(&path, &clear(2)),
            (
                "cat-lead in ‘Task #13 team’ · storefront was already cleared".into(),
                false
            )
        );
        let (msg, failed) = write_now(&path, &mark(3));
        assert!(
            failed && msg.starts_with("cannot mark qa in ‘New-book ribbon’"),
            "{msg}"
        );
        assert!(msg.contains("is not for the owner"), "{msg}");
        // One removed since the inbox showed it (its task or repo removed) says so, by the
        // item, never by its message id, and is no failure: there is nothing left to clear.
        assert_eq!(
            write_now(&path, &clear(77)),
            (
                "cat-lead in ‘Task #13 team’ · storefront is no longer on the board".into(),
                false
            )
        );
        assert_eq!(
            write_now(&path, &mark(77)),
            (
                "qa in ‘New-book ribbon’ · storefront is no longer on the board".into(),
                false
            )
        );
    }

    #[test]
    fn seen_marks_an_opened_threads_awareness_items_read_quietly() {
        let (_dir, path) = board();
        // Nothing to say when it works: opening a thread is not an action to report.
        assert_eq!(
            write_now(&path, &Write::Seen(vec![1])),
            (String::new(), false)
        );
        // The awareness item leaves the inbox; the question on the same thread stays.
        let inbox = Store::open(&path).unwrap().load().unwrap().inbox;
        assert!(inbox.awareness.is_empty(), "{inbox:?}");
        let asks: Vec<i64> = inbox.need_you.iter().map(|a| a.message.id).collect();
        assert_eq!(asks, [2]);
        // A failure says so.
        let (msg, failed) = write_now(&path, &Write::Seen(vec![3]));
        assert!(failed && msg.contains("is not for the owner"), "{msg}");
    }

    /// A message gone from the board since its read failed (its task or
    /// repo removed) is skipped, so the read tried again still reads the rest and is done.
    #[test]
    fn seen_skips_a_message_no_longer_on_the_board_and_reads_the_rest() {
        let (_dir, path) = board();
        let mut store = Store::open(&path).unwrap();
        let fyi = store
            .post(ThreadId(1), "dev-2", "@manager fyi: done")
            .unwrap();
        let c = fyi.messages.last().unwrap().id;
        assert_eq!(
            write_now(&path, &Write::Seen(vec![1, 77, c])),
            (String::new(), false)
        );
        let inbox = Store::open(&path).unwrap().load().unwrap().inbox;
        assert!(inbox.awareness.is_empty(), "{inbox:?}");
        let asks: Vec<i64> = inbox.need_you.iter().map(|a| a.message.id).collect();
        assert_eq!(asks, [2]);
    }

    #[test]
    fn a_lead_name_is_the_first_words_plus_lead_in_the_board_name_rule() {
        let m = MilestoneId(3);
        for (title, want) in [
            (
                "Reprint the poetry list for the book fair",
                "Reprint the poetry Lead",
            ),
            ("  sort   the\tshelf ", "sort the shelf Lead"),
            ("", "M3 Lead"),
            ("🚀🔥 ✨", "M3 Lead"),
            ("🚀 Launch day", "Launch day Lead"),
            ("!@#$%^&*()+=[]{};:'\",<>/?`~|\\", "M3 Lead"),
            ("don't $(rm -rf ~) `x`", "dont rm -rf Lead"),
            ("manager", "manager Lead"),
            ("system", "system Lead"),
            ("Audiobooks Lead", "Audiobooks Lead"),
            ("Ünïcödé café", "ncd caf Lead"),
            ("--force the shelf", "force the shelf Lead"),
            ("- . _", "M3 Lead"),
        ] {
            let name = lead_name(title, m);
            assert_eq!(name, want, "{title:?}");
            assert_eq!(check_session(&name).unwrap(), name, "{title:?}");
        }
        let long = lead_name(&format!("{} b c", "a".repeat(300)), m);
        assert_eq!(long, format!("{} Lead", "a".repeat(35)));
        let words = lead_name("Supercalifragilistic expialidociously extraordinary", m);
        assert_eq!(words, "Supercalifragilistic Lead");
        for name in [long, words] {
            assert!(
                name.len() <= NAME_MAX && check_session(&name).is_ok(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_claude_line_is_one_shell_word_per_argument_whatever_the_text() {
        assert_eq!(
            crate::terminal::herdr::shell_line(&lead_command("Audiobooks Lead", MilestoneId(3))),
            "claude -n 'Audiobooks Lead' '/control-tower:lead M3'"
        );
        // The shell reads the line back as its words, exactly as given, and runs nothing in
        // them: a hostile text is quoted, and a plain word stays one word.
        let dir = tempfile::tempdir().unwrap();
        let pwned = dir.path().join("pwned");
        let hostile = format!(
            "a'; touch {p}; echo '$(touch {p}) `touch {p}` \"$HOME\" \\ \n x",
            p = pwned.display()
        );
        // Each a word with no space, so only the plain characters decide its quoting. A
        // file in the shell's directory, so a bare `*` would expand to it.
        std::fs::write(dir.path().join("f"), "").unwrap();
        let one_word = [
            "it's", "$HOME", "a;b", "`id`", "*", "~", "a|b", "a&b", "a>b", "a\\b", "#c",
        ];
        let plain = ["-n", "/control-tower:lead", "x@y%z+1,2"];
        let mut words = vec!["printf".to_string(), "%s|".to_string(), hostile];
        words.extend(one_word.iter().chain(&plain).map(|w| w.to_string()));
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(crate::terminal::herdr::shell_line(&words))
            .current_dir(dir.path())
            .output()
            .unwrap();
        let want: String = words[2..].iter().map(|w| format!("{w}|")).collect();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), want);
        assert!(!pwned.exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "it wrote a file"
        );
    }

    /// A fake rex in a temp dir: each call appends its arguments, joined by "|", to
    /// `calls.log`; `new` prints Rex's reply (session "session:7"), or the file `reply-new`
    /// when there is one, and a call fails as Rex does when its `fail-<command>` file exists.
    fn fake_rex() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rex");
        let created = r#"{"session_id":"session:7","initial_windows":[{"window_id":"window:8","block_ids":["block:9"],"revision":1}]}"#;
        crate::test_exe::executable(
            &exe,
            format!(
                "#!/bin/sh\nd=$(dirname \"$0\")\n(IFS='|'; echo \"$*\") >> \"$d/calls.log\"\n\
                 [ -e \"$d/fail-$1\" ] && {{ echo 'Error: no server' >&2; exit 3; }}\n\
                 if [ \"$1\" = new ]; then\n\
                 if [ -e \"$d/reply-new\" ]; then cat \"$d/reply-new\"; else echo '{created}'; fi\n\
                 fi\nexit 0\n"
            ),
        );
        (dir, exe)
    }

    /// The module `n` starts a lead with, over the fake rex at `exe`.
    fn rex(exe: &Path) -> std::sync::Arc<dyn Terminal> {
        crate::terminal::lead_module(exe.to_path_buf())
    }

    fn calls(dir: &Path) -> Vec<String> {
        let log = std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default();
        log.lines().map(String::from).collect()
    }

    /// The `rex new` call that starts the lead `name` for `m` in `dir` on the board `db`,
    /// as the fake logs it: one word per argument, joined by "|".
    fn new_call(name: &str, dir: &Path, db: &Path, m: &str) -> String {
        format!(
            "new|{name}|-c|{}|--json|--|env|TOWER_DB={}|claude|-n|{name}|/control-tower:lead {m}",
            dir.display(),
            db.display()
        )
    }

    /// The home a test start runs from: the board's own folder, a directory that exists.
    fn home(path: &Path) -> &str {
        path.parent().unwrap().to_str().unwrap()
    }

    /// A start of a lead on `line`, in no repo, from the board's folder as home.
    fn start(path: &Path, rex: &Path, line: &str) -> (String, bool) {
        let write = Write::StartLead {
            line: line.into(),
            repo: None,
            terminal: rex.to_path_buf(),
            home: Some(home(path).into()),
        };
        write_now(path, &write)
    }

    fn titles(path: &Path) -> Vec<(String, String)> {
        let board = Store::open(path).unwrap().load().unwrap();
        (board.milestones.iter())
            .map(|m| (format!("{} {}", m.id, m.title), m.goal.clone()))
            .collect()
    }

    #[test]
    fn starting_a_lead_adds_the_milestone_then_a_named_claude_in_a_new_rex_session() {
        let (dir, path) = board();
        let (fake, exe) = fake_rex();
        let goal = "Audiobook samples play on every book page; don't `rm` $(anything) 'x'";
        assert_eq!(
            start(&path, &exe, &format!("  {goal} ")),
            (
                "started Audiobook samples play Lead for M2 in Rex".into(),
                false
            )
        );
        assert_eq!(titles(&path)[1], (format!("M2 {goal}"), goal.to_string()));
        // One word per argument, the board as TOWER_DB, and of the goal only the name.
        assert_eq!(
            calls(fake.path()),
            [new_call(
                "Audiobook samples play Lead",
                dir.path(),
                &path,
                "M2"
            )]
        );
        // A goal past a title's length: the title is cut, the goal kept whole.
        let long = format!("Long {}", "y".repeat(195));
        assert!(!start(&path, &exe, &long).1);
        assert_eq!(titles(&path)[2].1, long);
    }

    #[test]
    fn a_goal_that_starts_like_an_option_reaches_rex_as_its_label() {
        let (dir, path) = board();
        let (fake, exe) = fake_rex();
        assert!(!start(&path, &exe, "--force -c /tmp it's $(touch x)").1);
        assert_eq!(
            calls(fake.path()),
            [new_call("force -c tmp Lead", dir.path(), &path, "M2")]
        );
    }

    #[test]
    fn a_start_takes_its_home_from_the_write_and_always_gives_the_lead_its_board() {
        let (dir, path) = board();
        let (fake, exe) = fake_rex();
        let write = |home: Option<&str>| Write::StartLead {
            line: "Comics".into(),
            repo: None,
            terminal: exe.clone(),
            home: home.map(String::from),
        };
        // No home: refused before anything is written or Rex is asked.
        let (msg, failed) = write_now(&path, &write(None));
        assert!(failed && msg.contains("HOME is not set"), "{msg}");
        // A home that is not a directory: refused the same way, naming it.
        let gone = dir.path().join("gone");
        let (msg, failed) = write_now(&path, &write(gone.to_str()));
        assert!(failed, "{msg}");
        assert_eq!(
            msg,
            format!(
                "cannot start a lead in your home directory {}: it is not a directory",
                gone.display()
            )
        );
        assert_eq!(titles(&path).len(), 1);
        assert!(calls(fake.path()).is_empty());
        // The lead's session opens in that home, with this board, whatever TOWER_DB says
        // here.
        let other = tempfile::tempdir().unwrap();
        assert!(!write_now(&path, &write(other.path().to_str())).1);
        assert_eq!(
            calls(fake.path()),
            [new_call("Comics Lead", other.path(), &path, "M2")]
        );
        // A board named by a relative path reaches the lead made absolute.
        let mut store = Store::open(&path).unwrap();
        let at = Start {
            repo: None,
            home: home(&path),
        };
        let relative = Path::new("rel/board.db");
        start_lead(&mut store, relative, &*rex(&exe), "Atlases", at).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            calls(fake.path())[1],
            new_call("Atlases Lead", dir.path(), &cwd.join(relative), "M3")
        );
    }

    #[test]
    fn the_dashboards_home_decides_the_starts_home() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // A start in an environment holding just `vars`.
        let start = |vars: &[(&'static str, &OsStr)]| {
            let vars: Vec<(&str, OsString)> =
                (vars.iter()).map(|&(k, v)| (k, v.to_os_string())).collect();
            let env = move |name| (vars.iter().find(|(k, _)| *k == name)).map(|(_, v)| v.clone());
            Write::start_lead_from_env("Comics".into(), Some(RepoId(2)), "/bin/rex".into(), env)
        };
        let want = |home: Option<&str>| Write::StartLead {
            line: "Comics".into(),
            repo: Some(RepoId(2)),
            terminal: "/bin/rex".into(),
            home: home.map(String::from),
        };
        let home = ("HOME", OsStr::new("/Users/o"));
        assert_eq!(start(&[home]), want(Some("/Users/o")));
        // HOME unset, or not UTF-8: no home, which refuses the start.
        assert_eq!(start(&[]), want(None));
        let bad = ("HOME", OsStr::from_bytes(b"/Users/\xff"));
        assert_eq!(start(&[bad]), want(None));
    }

    #[test]
    fn a_taken_name_gets_the_milestone_id() {
        let (dir, path) = board();
        let (fake, exe) = fake_rex();
        let mut store = Store::open(&path).unwrap();
        let at = || Start {
            repo: None,
            home: home(&path),
        };
        let said = start_lead(&mut store, &path, &*rex(&exe), "Comics", at()).unwrap();
        assert_eq!(said, "started Comics Lead for M2 in Rex");
        let m = store.add_milestone("x", "g", None).unwrap();
        store
            .lead_milestone(m, "Shown", "taken lead", None)
            .unwrap();
        let said = start_lead(&mut store, &path, &*rex(&exe), "TAKEN", at()).unwrap();
        assert_eq!(said, "started TAKEN Lead M4 for M4 in Rex");
        assert_eq!(
            calls(fake.path()),
            [
                new_call("Comics Lead", dir.path(), &path, "M2"),
                new_call("TAKEN Lead M4", dir.path(), &path, "M4"),
            ]
        );
    }

    #[test]
    fn a_lead_started_on_a_repo_is_registered_opens_on_its_checkout_and_a_second_start_is_refused()
    {
        let (dir, path) = board();
        let (fake, exe) = fake_rex();
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir(checkout.path().join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let free = store.add_repo(checkout.path(), None).unwrap();
        let at = |repo| Start {
            repo: Some(repo),
            home: home(&path),
        };
        let said = start_lead(&mut store, &path, &*rex(&exe), "Comics", at(free)).unwrap();
        let name = checkout.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(said, format!("started Comics Lead for M2 on {name} in Rex"));
        let board = store.load().unwrap();
        assert_eq!(board.milestones[1].repo, Some(free));
        // The lead is registered at start, under the name Claude runs as.
        let lead = board.repo(free).unwrap().lead.clone().unwrap();
        assert_eq!(
            (lead.name.as_str(), lead.session.as_str()),
            ("Comics Lead", "Comics Lead")
        );
        let real = checkout.path().canonicalize().unwrap();
        assert_eq!(
            calls(fake.path()),
            [new_call("Comics Lead", &real, &path, "M2")]
        );
        // A second start on the same repo is refused before anything is written.
        let err = start_lead(&mut store, &path, &*rex(&exe), "Comics again", at(free)).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!("{name} already has its lead, Comics Lead")
        );
        // ...and in the write itself, should a load have missed the lead.
        let err = store
            .start_repo_lead("again", "g", free, |m| lead_name("again", m))
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("already has its lead, Comics Lead"),
            "{err:#}"
        );
        // R1 has its lead (the fixture's): refused too.
        let led = board.milestones[0].repo.unwrap();
        let err = start_lead(&mut store, &path, &*rex(&exe), "Atlases", at(led)).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!("{led} already has its lead, Catalogue")
        );
        assert_eq!(store.load().unwrap().milestones.len(), 2);
        assert_eq!(calls(fake.path()).len(), 1);
        // A name another repo's lead has (even one with no milestone) gets the milestone id,
        // on a repo and in no repo alike, so no registration is refused.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(other.path().join(".git")).unwrap();
        let r = store.add_repo(other.path(), None).unwrap();
        let said = start_lead(&mut store, &path, &*rex(&exe), "Comics", at(r)).unwrap();
        assert!(
            said.starts_with("started Comics Lead M3 for M3 on "),
            "{said}"
        );
        let board = store.load().unwrap();
        assert_eq!(
            board.repo(r).unwrap().lead.as_ref().unwrap().name,
            "Comics Lead M3"
        );
        let at_home = || Start {
            repo: None,
            home: home(&path),
        };
        let said = start_lead(&mut store, &path, &*rex(&exe), "Comics", at_home()).unwrap();
        assert_eq!(said, "started Comics Lead M4 for M4 in Rex");
        // A task's assignee is taken too, in no repo as on one: a lead is never a
        // task's assignee, so its own registration would be refused under that name.
        let task = store.add_task(MilestoneId(1), "t").unwrap();
        store.exec_sql(&format!(
            "UPDATE task SET assignee = 'atlases lead' WHERE id = {}",
            task.0
        ));
        let said = start_lead(&mut store, &path, &*rex(&exe), "Atlases", at_home()).unwrap();
        assert_eq!(said, "started Atlases Lead M5 for M5 in Rex");
        drop(dir);
    }

    #[test]
    fn a_rex_failure_undoes_the_start_and_says_why() {
        let (_dir, path) = board();
        let (fake, exe) = fake_rex();
        std::fs::write(fake.path().join("fail-new"), "").unwrap();
        let (msg, failed) = start(&path, &exe, "Comics");
        assert!(failed, "{msg}");
        assert_eq!(
            msg,
            "could not start the lead, so M2 was removed again; press n to try again: rex new \
             exited 3: Error: no server"
        );
        assert_eq!(calls(fake.path()).len(), 1);
        assert_eq!(titles(&path).len(), 1, "the milestone is removed again");

        // A reply that names no session: undone too, saying a session may be left.
        std::fs::remove_file(fake.path().join("fail-new")).unwrap();
        std::fs::write(fake.path().join("reply-new"), r#"{"sessions":[]}"#).unwrap();
        let (msg, failed) = start(&path, &exe, "Atlases");
        assert!(failed && msg.contains("M3 was removed again"), "{msg}");
        assert!(
            msg.contains(
                r#"a Rex session labelled "Atlases Lead" may be left: rex ls, then rex kill its id: rex new said something unexpected: {"sessions":[]}"#
            ),
            "{msg}"
        );

        // rex missing: undone, saying where it was looked for.
        let (msg, failed) = start(&path, &fake.path().join("none"), "Wishlists");
        assert!(failed && msg.contains("M4 was removed again"), "{msg}");
        assert!(msg.contains("rex not found: tried "), "{msg}");
        // An empty line adds nothing, as `tower milestone add` refuses it.
        let (msg, failed) = start(&path, &exe, " ");
        assert!(
            failed && msg.contains("the milestone title is empty"),
            "{msg}"
        );
        assert_eq!(titles(&path).len(), 1);
    }

    #[test]
    fn a_failed_start_on_a_repo_removes_its_lead_too_so_n_works_again() {
        let (_dir, path) = board();
        let (fake, exe) = fake_rex();
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir(checkout.path().join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(checkout.path(), None).unwrap();
        let at = || Start {
            repo: Some(r),
            home: home(&path),
        };
        std::fs::write(fake.path().join("fail-new"), "").unwrap();
        let err = start_lead(&mut store, &path, &*rex(&exe), "Comics", at()).unwrap_err();
        assert!(
            format!("{err:#}")
                .starts_with("could not start the lead, so M2 and Comics Lead were removed again"),
            "{err:#}"
        );
        let board = store.load().unwrap();
        assert!(board.repo(r).unwrap().lead.is_none());
        assert_eq!(board.milestones.len(), 1);
        // The retry starts it.
        std::fs::remove_file(fake.path().join("fail-new")).unwrap();
        let said = start_lead(&mut store, &path, &*rex(&exe), "Comics", at()).unwrap();
        assert!(said.starts_with("started Comics Lead for M3 on "), "{said}");
        let lead = store.load().unwrap().repo(r).unwrap().lead.clone().unwrap();
        assert_eq!(lead.name, "Comics Lead");
    }

    #[test]
    fn a_repo_with_no_main_checkout_is_refused_writing_nothing() {
        let (_dir, path) = board();
        let (fake, exe) = fake_rex();
        let mut store = Store::open(&path).unwrap();
        // The fixture's R1 came from the schema 14 path-less shape: give it no lead.
        store.exec_sql("DELETE FROM lead; UPDATE repo SET path = NULL");
        let r = store.load().unwrap().repos[0].id;
        let at = Start {
            repo: Some(r),
            home: home(&path),
        };
        let err = start_lead(&mut store, &path, &*rex(&exe), "Comics", at).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!(
                "{r} has no main checkout on the board: tower repo set {r} --path <checkout>, then n"
            )
        );
        assert_eq!(store.load().unwrap().milestones.len(), 1);
        assert!(calls(fake.path()).is_empty());
    }

    #[test]
    fn a_repo_whose_checkout_is_not_a_directory_is_refused_writing_nothing() {
        let (_dir, path) = board();
        let (fake, exe) = fake_rex();
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir(checkout.path().join(".git")).unwrap();
        let mut store = Store::open(&path).unwrap();
        let r = store.add_repo(checkout.path(), None).unwrap();
        let real = checkout.path().canonicalize().unwrap();
        let name = real.file_name().unwrap().to_str().unwrap().to_string();
        drop(checkout);
        let at = Start {
            repo: Some(r),
            home: home(&path),
        };
        let err = start_lead(&mut store, &path, &*rex(&exe), "Comics", at).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!(
                "cannot start a lead on {name}: its main checkout {} is not a directory; tower \
                 repo set {r} --path <checkout>, then n",
                real.display()
            )
        );
        let board = store.load().unwrap();
        assert_eq!(board.milestones.len(), 1);
        assert!(board.repo(r).unwrap().lead.is_none());
        assert!(calls(fake.path()).is_empty());
    }

    #[test]
    fn a_spawned_write_answers_on_its_channel() {
        let (_dir, path) = board();
        let item = "qa in ‘New-book ribbon’ · storefront".to_string();
        let rx = spawn(&path, Write::MarkRead { id: 1, item });
        let wait = std::time::Duration::from_secs(20);
        let said = rx.recv_timeout(wait).unwrap();
        assert_eq!(
            said,
            (
                "marked read: qa in ‘New-book ribbon’ · storefront".into(),
                false
            )
        );
        // The thread sends its answer and only then ends, dropping its sender: wait
        // until it has, so the poll below always sees it gone rather than racing its end.
        assert_eq!(
            rx.recv_timeout(wait),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        );
        // The thread is gone after its answer: polling again says so rather than hanging.
        assert!(poll(&rx).is_some_and(|(_, failed)| failed));
    }
}
