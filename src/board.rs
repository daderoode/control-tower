//! The board as the dashboard sees it, and the checks every write goes through.
//!
//! Pure: no SQLite, no terminal. `store` loads a `Board` and calls `check_line` before it writes.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// The longest title or goal the board accepts, in characters. Both are one line on the
/// dashboard; anything longer is a description, which comes with item details.
pub const MAX_LINE_CHARS: usize = 200;

/// A milestone's id, shown and typed as `M<n>` (for example `M2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MilestoneId(pub i64);

/// A repo's id, shown and typed as `R<n>` (for example `R2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepoId(pub i64);

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "R{}", self.0)
    }
}

impl FromStr for RepoId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let digits = s.strip_prefix('R').unwrap_or("");
        match digits.parse::<i64>() {
            Ok(n) if n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(RepoId(n)),
            _ => bail!("{s:?} is not a repo id: expected R followed by a number, like R2"),
        }
    }
}

/// Which repo a lead command means: the repo (`R2`), or a milestone (`M2`) standing for
/// the repo it belongs to, as the lead skill names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoOf {
    Repo(RepoId),
    Milestone(MilestoneId),
}

impl fmt::Display for RepoOf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoOf::Repo(r) => r.fmt(f),
            RepoOf::Milestone(m) => m.fmt(f),
        }
    }
}

impl FromStr for RepoOf {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if let Ok(r) = s.parse() {
            return Ok(RepoOf::Repo(r));
        }
        s.parse().map(RepoOf::Milestone).map_err(|_| {
            anyhow::anyhow!("{s:?} is neither a repo id (like R2) nor a milestone id (like M2)")
        })
    }
}

/// A task's id, shown as `#<n>` (for example `#12`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub i64);

impl fmt::Display for MilestoneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "M{}", self.0)
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl FromStr for MilestoneId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let digits = s.strip_prefix('M').unwrap_or("");
        match digits.parse::<i64>() {
            Ok(n) if n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(MilestoneId(n)),
            _ => bail!("{s:?} is not a milestone id: expected M followed by a number, like M2"),
        }
    }
}

impl FromStr for TaskId {
    type Err = anyhow::Error;

    /// A number, with or without the `#` it is shown with (`12` or `#12`); unquoted, a shell
    /// takes `#12` for a comment, so the bare number is the one to type.
    fn from_str(s: &str) -> Result<Self> {
        let digits = s.strip_prefix('#').unwrap_or(s);
        match digits.parse::<i64>() {
            Ok(n) if n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(TaskId(n)),
            _ => bail!("{s:?} is not a task id: expected a number, like 12"),
        }
    }
}

/// Where a task is, and where a milestone is as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Waiting,
    InProgress,
    Done,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Waiting, Stage::InProgress, Stage::Done];

    /// The name typed in commands and stored on the board.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Waiting => "waiting",
            Stage::InProgress => "in-progress",
            Stage::Done => "done",
        }
    }

    /// The name in a sentence, like "in progress".
    pub fn label(self) -> &'static str {
        match self {
            Stage::InProgress => "in progress",
            other => other.name(),
        }
    }
}

impl FromStr for Stage {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match Stage::ALL.into_iter().find(|st| st.name() == s) {
            Some(st) => Ok(st),
            None => bail!("{s:?} is not a stage: pick one of waiting, in-progress, done"),
        }
    }
}

/// Checks a one-line text field (`what` names it in the error) and returns it trimmed.
/// Refuses empty text, text over `MAX_LINE_CHARS`, any control character, and any Unicode
/// format character (Cf, such as a bidi override or a zero-width space) or line or paragraph
/// separator (Zl, Zp), so nothing stored can break or disguise a dashboard row.
pub fn check_line(what: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("the {what} is empty");
    }
    if let Some(c) = trimmed.chars().find(hidden) {
        bail!(
            "the {what} contains a control character (U+{:04X}); it must be one line of plain text",
            c as u32
        );
    }
    refuse_longer(what, trimmed, MAX_LINE_CHARS)?;
    Ok(trimmed.to_string())
}

/// A character that would break or disguise a dashboard row: a control character, a Unicode
/// format character (Cf) or a line or paragraph separator (Zl, Zp).
pub fn hidden(c: &char) -> bool {
    c.is_control()
        || matches!(
            c.general_category(),
            GeneralCategory::Format
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        )
}

fn refuse_longer(what: &str, value: &str, most: usize) -> Result<()> {
    let chars = value.chars().count();
    if chars > most {
        bail!("the {what} is {chars} characters long; the most is {most}");
    }
    Ok(())
}

/// The longest description or standing context the board accepts, in characters.
pub const MAX_TEXT_CHARS: usize = 2000;

/// Checks a text of one or more lines, such as a description (`what` names it), and returns
/// it trimmed, with Windows line ends made plain. As `check_line`, but a line break is
/// allowed and the limit is `MAX_TEXT_CHARS`.
pub fn check_text(what: &str, value: &str) -> Result<String> {
    check_lines(what, value.replace("\r\n", "\n").trim(), MAX_TEXT_CHARS)
}

/// The longest message or evidence on a message board, in characters: room for a plan or a
/// test report.
pub const MAX_MESSAGE_CHARS: usize = 4000;

/// Checks a message or evidence (`what` names it) as `check_text` does, but up to
/// `MAX_MESSAGE_CHARS`. Pasted text works: CRLF and CR become LF and a tab four spaces
/// first. Returns it so, as the signed design's composer submits it: its leading blank lines
/// dropped and its end trimmed, its first line's indent kept, so a list reads in line. A
/// hidden whitespace character before the text (a form feed from a pager) goes too, as it
/// always did, since the board can store none.
pub fn check_message(what: &str, value: &str) -> Result<String> {
    let normal = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ");
    // The signed design's (UI v0.1.14) `replace(/^\s*\n/, "")`: the leading whitespace up
    // to its last line break, or up to its last hidden character (`hidden` takes in a line
    // break), so what is kept starts with plain spaces or the text.
    let lead = normal.len() - normal.trim_start().len();
    let start = (normal[..lead].char_indices())
        .rfind(|(_, c)| hidden(c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    check_lines(what, normal[start..].trim_end(), MAX_MESSAGE_CHARS)
}

/// Checks text already cut as it is stored, its line ends plain: not empty, no hidden
/// character but line breaks, at most `most` characters. Returns it.
fn check_lines(what: &str, trimmed: &str, most: usize) -> Result<String> {
    if trimmed.is_empty() {
        bail!("the {what} is empty");
    }
    if let Some(c) = trimmed.chars().find(|c| *c != '\n' && hidden(c)) {
        bail!(
            "the {what} contains a control character (U+{:04X}); it must be plain text",
            c as u32
        );
    }
    refuse_longer(what, trimmed, most)?;
    Ok(trimmed.to_string())
}

/// Checks a link, such as a tracker link (`what` names it): one plain `http` or `https` URL
/// with a host, read by a URL parser. Returns it as the parser writes it.
pub fn check_link(what: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("the {what} is empty");
    }
    // The parser drops tabs and line breaks silently, so refuse them before it sees them.
    if let Some(c) = trimmed.chars().find(|c| hidden(c) || c.is_whitespace()) {
        bail!(
            "the {what} contains a space or control character (U+{:04X}); it must be one URL",
            c as u32
        );
    }
    refuse_longer(what, trimmed, MAX_TEXT_CHARS)?;
    let url = url::Url::parse(trimmed)
        .map_err(|e| anyhow::anyhow!("the {what} {trimmed:?} is not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        bail!("the {what} {trimmed:?} is not an http or https link");
    }
    Ok(url.to_string())
}

/// A lead's colour, from a fixed palette that starts with the signed design's lead colours.
/// Stored and typed by name; the dashboard turns it into a terminal colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Blue,
    Purple,
    Orange,
    Teal,
    Pink,
    Green,
}

impl Colour {
    /// The palette in the order colours are handed out.
    pub const ALL: [Colour; 6] = [
        Colour::Blue,
        Colour::Purple,
        Colour::Orange,
        Colour::Teal,
        Colour::Pink,
        Colour::Green,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Colour::Blue => "blue",
            Colour::Purple => "purple",
            Colour::Orange => "orange",
            Colour::Teal => "teal",
            Colour::Pink => "pink",
            Colour::Green => "green",
        }
    }

    /// The colour for a new lead when none is given: the first in the palette no lead has
    /// yet, or once every colour is taken, the least used (the earliest on a tie).
    pub fn next(taken: &[Colour]) -> Colour {
        let uses = |c: &Colour| taken.iter().filter(|t| *t == c).count();
        Colour::ALL
            .into_iter()
            .min_by_key(uses)
            .expect("the palette is not empty")
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Colour {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match Colour::ALL.into_iter().find(|c| c.name() == s) {
            Some(c) => Ok(c),
            None => {
                let names: Vec<&str> = Colour::ALL.iter().map(|c| c.name()).collect();
                bail!(
                    "{s:?} is not a lead colour: pick one of {}",
                    names.join(", ")
                )
            }
        }
    }
}

/// Checks a lead's Claude session name as it is written to the board, as `check_name`
/// does. Returns it trimmed.
pub fn check_session(value: &str) -> Result<String> {
    check_name("lead session name", value)
}

/// What a name written to the board (an author, a lead's session name) may be made of.
pub const NAME_CHARS: &str = "ASCII letters, digits, single spaces, \"-\", \"_\" and \".\"";

/// Checks a name as it is written to the board: one line (`check_line`, which trims it) of
/// `NAME_CHARS` only, with single spaces between words, as Claude session names like
/// "Catalogue Lead" have. A lookalike (a fullwidth or Cyrillic "manager") never passes
/// for a reserved or known name. Checked on writes only: names already on the board are read
/// and found as they are.
fn check_name(what: &str, value: &str) -> Result<String> {
    let name = check_line(what, value)?;
    let allowed = |c: char| c.is_ascii_alphanumeric() || " -_.".contains(c);
    if let Some(c) = name.chars().find(|c| !allowed(*c)) {
        bail!(
            "the {what} contains {c:?} (U+{:04X}); use only {NAME_CHARS}",
            c as u32
        );
    }
    if name.contains("  ") {
        bail!("the {what} {name:?} has two spaces in a row; use single spaces between words");
    }
    Ok(name)
}

/// Whether `name` is `reserved` ignoring ASCII case and spaces, so "Man ager" or " SYSTEM "
/// cannot stand in for it.
fn is_reserved(name: &str, reserved: &str) -> bool {
    name.replace(' ', "").eq_ignore_ascii_case(reserved)
}

/// What the dashboard shows: every repo in id order, every milestone in id order, each with
/// its lead (if it has one yet) and its tasks in id order, and the standalone tasks, under
/// no milestone, in id order (`Task::repo` says whose they are).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Board {
    pub repos: Vec<Repo>,
    pub milestones: Vec<Milestone>,
    pub standalone: Vec<Task>,
    /// The owner's alerts, worked out once from every unread `@manager` message on an open
    /// thread when the board is read (`crate::inbox`): whole, unlike a thread's messages on
    /// a loaded board, which are only the latest few.
    pub inbox: crate::inbox::Inbox,
}

/// A git repository on the board, named by its main checkout: it holds milestones and
/// standalone tasks, has one lead and owns the message board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub id: RepoId,
    /// Its main checkout; `None` on a repo the schema 14 migration made, until it is set.
    pub path: Option<String>,
    /// Its forge link.
    pub link: Option<String>,
    pub lead: Option<Lead>,
    /// The owner folded it in the dashboard's tree.
    pub folded: bool,
    /// Its Done group is folded in the dashboard's tree, as it is until the owner unfolds it.
    pub done_folded: bool,
    /// Its whole message board, every thread in id order.
    pub threads: Vec<Thread>,
}

impl Repo {
    /// Its name: the last part of its main checkout's path, or its id while that is not
    /// known.
    pub fn name(&self) -> String {
        (self.path.as_deref())
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .map_or_else(|| self.id.to_string(), str::to_string)
    }
}

impl Board {
    /// Every repo's lead, in repo order.
    pub fn leads(&self) -> impl Iterator<Item = &Lead> {
        self.repos.iter().filter_map(|r| r.lead.as_ref())
    }

    /// The task `id`, under a milestone or standalone, if it is on the board.
    pub fn task(&self, id: TaskId) -> Option<&Task> {
        (self.milestones.iter().flat_map(|m| &m.tasks))
            .chain(&self.standalone)
            .find(|t| t.id == id)
    }

    /// The repo `id`, if it is on the board.
    pub fn repo(&self, id: RepoId) -> Option<&Repo> {
        self.repos.iter().find(|r| r.id == id)
    }

    /// The milestones of `repo` (`None`: those in no repo on the board), in id order.
    pub fn milestones_of(&self, repo: Option<RepoId>) -> impl Iterator<Item = &Milestone> {
        let known = |r: Option<RepoId>| r.is_some_and(|r| self.repo(r).is_some());
        (self.milestones.iter()).filter(move |m| match repo {
            Some(_) => m.repo == repo,
            None => !known(m.repo),
        })
    }

    /// The standalone tasks of `repo` (`None`: those in no repo on the board), in id order.
    pub fn standalone_of(&self, repo: Option<RepoId>) -> impl Iterator<Item = &Task> {
        let known = |r: Option<RepoId>| r.is_some_and(|r| self.repo(r).is_some());
        (self.standalone.iter()).filter(move |t| match repo {
            Some(_) => t.repo == repo,
            None => !known(t.repo),
        })
    }

    /// The active milestones of `repo`, in id order: all but those in its Done group. In no
    /// repo (`None`) every milestone is active, as only a repo has a Done group.
    pub fn active_of(&self, repo: Option<RepoId>) -> impl Iterator<Item = &Milestone> {
        (self.milestones_of(repo)).filter(move |m| repo.is_none() || m.stage() != Stage::Done)
    }

    /// The Done group of `repo`: its milestones with the lead's Done mark (see
    /// `Milestone::stage`), most recently marked first, those with no recorded date last,
    /// newest id first among equals.
    pub fn done_of(&self, repo: RepoId) -> Vec<&Milestone> {
        let mut done: Vec<&Milestone> = (self.milestones_of(Some(repo)))
            .filter(|m| m.stage() == Stage::Done)
            .collect();
        done.sort_by_key(|m| std::cmp::Reverse((m.done_at, m.id.0)));
        done
    }

    /// The Done group's badges: the need-you and unread awareness items of `repo` on
    /// threads linked to one of its Done milestones or their tasks.
    pub fn done_badges(&self, repo: RepoId) -> (usize, usize) {
        let done = self.done_of(repo);
        let linked = |a: &&crate::inbox::Alert| {
            a.repo == repo
                && done
                    .iter()
                    .any(|m| (m.threads.iter()).any(|t| t.id == a.thread && m.links(t)))
        };
        let inbox = &self.inbox;
        (
            inbox.need_you.iter().filter(linked).count(),
            inbox.awareness.iter().filter(linked).count(),
        )
    }

    /// Whether anything is in no repo on the board: the tree's last group.
    pub fn has_unplaced(&self) -> bool {
        self.milestones_of(None).next().is_some() || self.standalone_of(None).next().is_some()
    }

    /// How many tasks of `repo` carry the blocked flag, in its milestones and standalone:
    /// the repo row's "N blocked".
    pub fn blocked_in(&self, repo: Option<RepoId>) -> usize {
        self.milestones_of(repo)
            .map(Milestone::blocked_tasks)
            .sum::<usize>()
            + (self.standalone_of(repo))
                .filter(|t| t.details.blocked.is_some())
                .count()
    }
}

/// A message board as a view shows it: a lead (for author colours), the threads it shows,
/// narrowed to a milestone or task, or a repo's whole board, the tasks those threads can be
/// linked to (a thread about a task is named by it), the open questions for the owner in
/// those threads (the inbox's need-you items, so a cleared one is gone), and the other
/// repos' leads, each with its repo's name, who may post on it lead to lead (spec 3a).
///
/// A thread with only tower's messages, linked to a task that has a thread with people in
/// it, is no thread of its own here: it joins that thread's conversation (`folded`), so a
/// task has one conversation (UI v0.1.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Talk<'a> {
    pub lead: Option<&'a Lead>,
    pub threads: Vec<&'a Thread>,
    /// What part of the board it is, as its heading says: "whole repo", or "M2 and its
    /// tasks".
    pub scope: String,
    pub others: Vec<(String, &'a Lead)>,
    pub tasks: Vec<&'a Task>,
    /// The open questions for the owner in its threads, newest first.
    pub needs: Vec<&'a crate::inbox::Alert>,
    /// Each thread folded into another, with the thread it joins.
    pub folded: Vec<(ThreadId, &'a Thread)>,
    /// Each task's home thread: its first open thread a person has posted in (by id).
    pub homes: Vec<(TaskId, ThreadId)>,
}

impl<'a> Talk<'a> {
    /// The leads of `board`'s repos other than `repo`, each with its repo's name.
    fn others(board: &'a Board, repo: Option<RepoId>) -> Vec<(String, &'a Lead)> {
        (board.repos.iter())
            .filter(|r| Some(r.id) != repo)
            .filter_map(|r| r.lead.as_ref().map(|l| (r.name(), l)))
            .collect()
    }

    /// A board of `threads` (the repo's whole board narrowed by `keep`), its tasks from
    /// `tasks`, with the board's lead and need-you items.
    fn new(
        board: &'a Board,
        repo: Option<&'a Repo>,
        lead: Option<&'a Lead>,
        all: &'a [Thread],
        keep: impl Fn(&Thread) -> bool,
        tasks: Vec<&'a Task>,
        scope: String,
    ) -> Talk<'a> {
        // Each task's home thread (`Talk::home_of_task`), worked out once.
        let mut homes: Vec<(TaskId, ThreadId)> = Vec::new();
        for h in all
            .iter()
            .filter(|h| h.status() == ThreadStatus::Open && !tower_only(h))
        {
            if let Some(task) = h.task
                && !homes.iter().any(|(k, _)| *k == task)
            {
                homes.push((task, h.id));
            }
        }
        // A thread only tower has posted in joins its task's home thread. Never a resolved
        // one: an open question there could not be answered, so the tower thread stays in
        // the list. A resolved tower thread does join an open home (its lines are history).
        let home = |t: &Thread| -> Option<ThreadId> {
            let task = t.task.filter(|_| tower_only(t))?;
            (homes.iter())
                .find(|(k, h)| *k == task && *h != t.id)
                .map(|(_, h)| *h)
        };
        let mut threads = Vec::new();
        let mut folded = Vec::new();
        for t in all.iter().filter(|t| keep(t)) {
            match home(t) {
                Some(h) => folded.push((h, t)),
                None => threads.push(t),
            }
        }
        let shown = |id: ThreadId| {
            threads.iter().any(|t: &&Thread| t.id == id) || folded.iter().any(|(_, f)| f.id == id)
        };
        let needs = (board.inbox.need_you.iter())
            .filter(|a| repo.is_none_or(|r| a.repo == r.id) && shown(a.thread))
            .collect();
        Talk {
            lead,
            threads,
            scope,
            others: Talk::others(board, repo.map(|r| r.id)),
            tasks,
            needs,
            folded,
            homes,
        }
    }

    /// A repo's whole board.
    pub fn of_repo(board: &'a Board, r: &'a Repo) -> Talk<'a> {
        let tasks = (board.milestones_of(Some(r.id)))
            .flat_map(|m| m.tasks.iter())
            .chain(board.standalone_of(Some(r.id)))
            .collect();
        let lead = r.lead.as_ref();
        Talk::new(
            board,
            Some(r),
            lead,
            &r.threads,
            |_| true,
            tasks,
            "whole repo".into(),
        )
    }

    /// A milestone's part of its repo's board: the threads linked to it or to one of its
    /// tasks.
    pub fn of_milestone(board: &'a Board, m: &'a Milestone) -> Talk<'a> {
        let repo = m.repo.and_then(|r| board.repo(r));
        let scope = format!("{} and its tasks", m.id);
        let tasks = m.tasks.iter().collect();
        Talk::new(
            board,
            repo,
            m.lead.as_ref(),
            &m.threads,
            |t| m.links(t),
            tasks,
            scope,
        )
    }

    /// The part of repo `r`'s board about its Done group: the threads linked to one of its
    /// Done milestones or their tasks.
    pub fn of_done(board: &'a Board, r: &'a Repo) -> Talk<'a> {
        let done = board.done_of(r.id);
        let tasks = done.iter().flat_map(|m| m.tasks.iter()).collect();
        let keep = |t: &Thread| done.iter().any(|m| m.links(t));
        let scope = "Done milestones".into();
        Talk::new(
            board,
            Some(r),
            r.lead.as_ref(),
            &r.threads,
            keep,
            tasks,
            scope,
        )
    }

    /// Who posts on it, for the chat's colours and role words.
    pub fn people(&self) -> crate::ui::People<'_> {
        crate::ui::People {
            lead: self.lead,
            others: &self.others,
        }
    }

    /// The threads it shows, only those linked to `task` when given: open ones first, then
    /// resolved, each newest activity first.
    pub fn ordered(&self, task: Option<TaskId>) -> Vec<&'a Thread> {
        let mut shown: Vec<&Thread> = (self.threads.iter().copied())
            .filter(|t| task.is_none() || t.task == task)
            .collect();
        // Open ones first, then resolved, each by its conversation's newest message: the
        // rule `order_key` keeps for the CLI, which does not fold; keep the two the same.
        shown.sort_by_key(|t| {
            let resolved = t.status() == ThreadStatus::Resolved;
            (resolved, std::cmp::Reverse((self.latest(t), t.id.0)))
        });
        shown
    }

    /// The task thread `t` is linked to, when it is on this board.
    pub fn task_of(&self, t: &Thread) -> Option<&'a Task> {
        let id = t.task?;
        self.tasks.iter().copied().find(|k| k.id == id)
    }

    /// Thread `t`'s conversation: its messages with those of the threads folded into it,
    /// in time order, then by id.
    pub fn conversation<'t>(&self, t: &'t Thread) -> std::borrow::Cow<'t, [Message]> {
        let joining: Vec<&Thread> = (self.folded.iter())
            .filter(|(h, _)| *h == t.id)
            .map(|(_, f)| *f)
            .collect();
        if joining.is_empty() {
            return std::borrow::Cow::Borrowed(&t.messages);
        }
        let mut all: Vec<Message> = t.messages.clone();
        all.extend(joining.iter().flat_map(|f| f.messages.iter().cloned()));
        all.sort_by_key(|m| (m.at, m.id));
        std::borrow::Cow::Owned(all)
    }

    /// When thread `t`'s conversation last had a message, the threads folded into it with
    /// it.
    pub fn latest(&self, t: &Thread) -> Option<i64> {
        let folded = (self.folded.iter())
            .filter(|(h, _)| *h == t.id)
            .map(|(_, f)| f.latest());
        std::iter::once(t.latest()).chain(folded).flatten().max()
    }

    /// How many of thread `t`'s conversation's messages are not loaded: of it and of each
    /// thread folded into it.
    pub fn missing(&self, t: &Thread) -> usize {
        let folded = self
            .folded
            .iter()
            .filter(|(h, _)| *h == t.id)
            .map(|(_, f)| *f);
        (std::iter::once(t).chain(folded))
            .map(|x| x.total.saturating_sub(x.messages.len()))
            .sum()
    }

    /// How many messages thread `t`'s conversation has in all, the threads folded into it
    /// with it.
    pub fn total(&self, t: &Thread) -> usize {
        let folded = (self.folded.iter())
            .filter(|(h, _)| *h == t.id)
            .map(|(_, f)| f.total);
        t.total + folded.sum::<usize>()
    }

    /// The threads whose messages thread `home`'s conversation shows: it, then those
    /// folded into it.
    pub fn joined(&self, home: ThreadId) -> Vec<ThreadId> {
        let folded = self
            .folded
            .iter()
            .filter(|(h, _)| *h == home)
            .map(|(_, f)| f.id);
        std::iter::once(home).chain(folded).collect()
    }

    /// The thread whose conversation shows message `id`: its own, or the one it is folded
    /// into.
    pub fn home_of(&self, thread: ThreadId) -> ThreadId {
        (self.folded.iter())
            .find(|(_, f)| f.id == thread)
            .map_or(thread, |(h, _)| *h)
    }

    /// Whether a question for the owner waits in thread `t`'s conversation.
    pub fn needs_you(&self, t: &Thread) -> bool {
        self.needs.iter().any(|a| self.home_of(a.thread) == t.id)
    }

    /// Thread `t` in words: its task's title when it is linked to one, else its own.
    pub fn name(&self, t: &Thread) -> String {
        self.task_of(t)
            .map_or_else(|| t.title.clone(), |k| k.title.clone())
    }

    /// Thread `t`'s own title, to show dim after its name: when it is on a task with
    /// more than one thread listed here and its title is not the task's, which would make
    /// the task's threads read alike.
    pub fn also<'t>(&self, t: &'t Thread) -> Option<&'t str> {
        let k = self.task_of(t)?;
        let listed = (self.threads.iter())
            .filter(|x| x.task == Some(k.id))
            .count();
        (listed > 1 && t.title != k.title).then_some(t.title.as_str())
    }

    /// What thread `t` is on, dim beside its name: its task's stage and who is on it (or
    /// the task's id when this board does not carry it), or the milestone it is about, or
    /// "repo-wide".
    pub fn state(&self, t: &Thread) -> String {
        match (self.task_of(t), t.task, t.milestone) {
            (Some(k), _, _) => format!("{} · {}", k.stage.label(), k.who(self.lead)),
            // Linked to a task this board does not carry: name it all the same.
            (None, Some(k), _) => format!("on {k}"),
            (None, None, Some(m)) => format!("on {m}"),
            (None, None, None) => "repo-wide".into(),
        }
    }

    /// Thread `t`'s id, dim at the right: "T3". Its task's id is not shown: the
    /// owner reads a thread by its name, and a task's number reads as a ticket's.
    pub fn ids(&self, t: &Thread) -> String {
        t.id.to_string()
    }

    /// Where thread `t` stands, by whom, and its tone: on its task's first open thread, the
    /// task's doing line (the blocked reason first, as its detail has it), else the first
    /// line of its newest message by a person that is not an ack, else of its newest
    /// message; either way without a "Status:" prefix. The tone is the whole message's, as
    /// its bubble takes it; a blocked reason, which has no message, takes its
    /// "blocked: <why>" line's.
    pub fn stands(&self, t: &Thread) -> Option<(String, String, Tone)> {
        // The doing line is the task's, so it heads the task's home thread only (or, while
        // no person has posted on the task, its first open thread); any other thread on the
        // task says where it stands in its own words.
        let heads = |k: &Task| match self.home_of_task(k.id) {
            Some(home) => home == t.id,
            None => {
                let open = (self.threads.iter())
                    .filter(|h| h.task == Some(k.id) && h.status() == ThreadStatus::Open);
                open.map(|h| h.id).min() == Some(t.id)
            }
        };
        if let Some(k) = self.task_of(t).filter(|k| heads(k)) {
            match k.doing_line() {
                Some(DoingLine::Blocked(why)) => {
                    let who = k.details.assignee.clone().unwrap_or_default();
                    let text = format!("blocked: {why}");
                    let tone = Tone::of(&text);
                    return Some((who, text, tone));
                }
                Some(DoingLine::Post(line)) => {
                    let doing = k.details.doing.as_ref();
                    let who = doing.map_or_else(String::new, |m| m.author.clone());
                    let tone = doing.map_or(Tone::Plain, tone_of);
                    return Some((who, without_status(line).to_string(), tone));
                }
                None => {}
            }
        }
        // The newest message across the conversation's threads, without copying it.
        let joined: Vec<&Thread> = std::iter::once(t)
            .chain(
                self.folded
                    .iter()
                    .filter(|(h, _)| *h == t.id)
                    .map(|(_, f)| *f),
            )
            .collect();
        let newest = |keep: &dyn Fn(&Message) -> bool| {
            (joined.iter().flat_map(|x| x.messages.iter()))
                .filter(|m| keep(m))
                .max_by_key(|m| (m.at, m.id))
        };
        let m = newest(&|m| !is_tower(&m.author) && !is_ack(m)).or_else(|| newest(&|_| true))?;
        let first = m.text.lines().next().unwrap_or_default();
        Some((
            m.author.clone(),
            without_status(first).to_string(),
            tone_of(m),
        ))
    }

    /// Task `task`'s home thread on this board's whole repo: its first open thread a person
    /// has posted in, which takes its tower threads and heads its doing line.
    pub fn home_of_task(&self, task: TaskId) -> Option<ThreadId> {
        self.homes.iter().find(|(k, _)| *k == task).map(|(_, h)| *h)
    }
}

/// Whether `author` is tower itself (`SYSTEM_AUTHOR`, in any case or spacing, as the board
/// reserves it).
pub fn is_tower(author: &str) -> bool {
    is_reserved(author, SYSTEM_AUTHOR)
}

/// Whether only tower has posted in thread `t`, by its whole history (`Thread::authors`),
/// not the latest messages a loaded board carries.
fn tower_only(t: &Thread) -> bool {
    !t.authors.is_empty() && t.authors.iter().all(|a| is_tower(a))
}

/// `line` without a leading "Status:" or "Status <word>:" and the spaces after it.
fn without_status(line: &str) -> &str {
    // As the signed design (UI v0.1.13) reads it: "status", then at most one more word,
    // then ":" and spaces, in any case ("Status:", "status update:"; not "Statuses:" or
    // "Status of the build:").
    let Some(head) = line.get(..6).filter(|h| h.eq_ignore_ascii_case("status")) else {
        return line;
    };
    let rest = &line[head.len()..];
    let after = match rest.strip_prefix(':') {
        Some(after) => after,
        None => match rest.strip_prefix(' ').and_then(|r| r.split_once(':')) {
            Some((word, after)) if !word.is_empty() && !word.contains(char::is_whitespace) => after,
            _ => return line,
        },
    };
    after.trim_start()
}

/// A message that mentions `@manager`, with the thread it is in and whether the owner
/// marked it read (an awareness item read, or a need-you item cleared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub thread: ThreadId,
    pub message: Message,
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    pub id: MilestoneId,
    pub title: String,
    /// The one-line goal, shown in the milestone overview.
    pub goal: String,
    /// The repo it belongs to; `None` until `tower milestone set --repo` gives it one.
    pub repo: Option<RepoId>,
    /// Its repo's lead.
    pub lead: Option<Lead>,
    pub tasks: Vec<Task>,
    /// The lead marked it Done. It counts only while every task is Done: see `stage`.
    pub marked_done: bool,
    /// When the lead marked it Done, in seconds since the Unix epoch; `None` while it is
    /// not marked, or for a mark set before the board recorded the date.
    pub done_at: Option<i64>,
    /// The owner folded its task list in the dashboard's tree.
    pub folded: bool,
    /// Its repo's message board, every thread of the repo in id order, so milestones of
    /// one repo show the same board: empty while it has no repo.
    pub threads: Vec<Thread>,
    pub details: MilestoneDetails,
}

/// What a milestone's detail view shows beyond its overview, each set with `tower milestone
/// set`; `None` until then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MilestoneDetails {
    /// Standing context: "when picking this up, do ABC", for this milestone only.
    pub context: Option<String>,
    /// The spec it came from, like "docs/specs/first-shop-pages.md".
    pub spec: Option<String>,
    /// Its ticket in whichever tracker it came from: an http or https URL.
    pub link: Option<String>,
}

impl Milestone {
    /// Its team as its detail shows it: the assignees of its tasks, in task order, then the
    /// authors of its threads (those linked to it or its tasks), in thread order, as
    /// `team` says, leaving out its lead and every lead on `board`.
    pub fn team(&self, board: &Board) -> Vec<&str> {
        let leads: Vec<&Lead> = self.lead.iter().chain(board.leads()).collect();
        team(
            &leads,
            self.tasks.iter(),
            self.threads.iter().filter(|t| self.links(t)),
        )
    }

    pub fn done_tasks(&self) -> usize {
        self.tasks.iter().filter(|t| t.stage == Stage::Done).count()
    }

    /// Done tasks out of all tasks, as a whole percentage rounded down: 100 only when every
    /// task is Done, and 0 with no tasks.
    pub fn percent(&self) -> u8 {
        match self.tasks.len() {
            0 => 0,
            all => (self.done_tasks() * 100 / all) as u8,
        }
    }

    /// Done when the lead marked it and every one of its tasks (at least one) is Done; In
    /// progress once any task has left Waiting; Waiting otherwise.
    pub fn stage(&self) -> Stage {
        if self.marked_done && self.can_be_done() {
            Stage::Done
        } else if self.tasks.iter().any(|t| t.stage != Stage::Waiting) {
            Stage::InProgress
        } else {
            Stage::Waiting
        }
    }

    /// Whether the lead may mark it Done: it has tasks and all of them are Done.
    pub fn can_be_done(&self) -> bool {
        !self.tasks.is_empty() && self.done_tasks() == self.tasks.len()
    }

    /// How many of its tasks carry the blocked flag: the lead's "N blocked".
    pub fn blocked_tasks(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.details.blocked.is_some())
            .count()
    }

    /// Its threads in the order the board shows them: open ones first, then resolved, each
    /// group by its latest message, newest first (then by id, newest first). With `task`,
    /// only the threads linked to it.
    pub fn ordered_threads(&self, task: Option<TaskId>) -> Vec<&Thread> {
        ordered_threads(&self.threads, task)
    }

    /// Whether `t` is about this milestone: linked to it, or to one of its tasks.
    pub fn links(&self, t: &Thread) -> bool {
        t.milestone == Some(self.id) || t.task.is_some_and(|k| self.tasks.iter().any(|x| x.id == k))
    }
}

/// A team: the assignees of `tasks`, in order, then the authors of `threads`, in order,
/// each once ignoring ASCII case. `leads` (the board's own and other repos' leads, who
/// post lead to lead, by session or name), the owner (`manager`), tower's own `system` and
/// "you" are not on it.
pub fn team<'a>(
    leads: &[&Lead],
    tasks: impl Iterator<Item = &'a Task>,
    threads: impl Iterator<Item = &'a Thread>,
) -> Vec<&'a str> {
    let assignees = tasks.filter_map(|t| t.details.assignee.as_deref());
    let authors = threads.flat_map(|t| &t.authors).map(String::as_str);
    let mut team: Vec<&str> = Vec::new();
    for name in assignees.chain(authors) {
        let lead = (leads.iter()).any(|l| matches!(Role::of(name, Some(l)), Role::Lead));
        let left_out = lead
            || Role::of(name, None) == Role::System
            || ["manager", "you"]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n));
        if !left_out && !team.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            team.push(name);
        }
    }
    team
}

/// The board's thread order: open ones first, then resolved, each newest activity first.
/// `Talk::ordered` keeps the same rule by hand for the dashboard, with folded threads'
/// messages counted (the CLI does not fold), so change both together.
fn order_key(t: &Thread) -> (bool, std::cmp::Reverse<(Option<i64>, i64)>) {
    (
        t.status() == ThreadStatus::Resolved,
        std::cmp::Reverse((t.latest(), t.id.0)),
    )
}

/// `threads` in the dashboard's order, only those linked to `task` when given: open ones
/// first, then resolved, each newest activity first.
pub fn ordered_threads(threads: &[Thread], task: Option<TaskId>) -> Vec<&Thread> {
    let mut shown: Vec<&Thread> = (threads.iter())
        .filter(|t| task.is_none() || t.task == task)
        .collect();
    shown.sort_by_key(|t| order_key(t));
    shown
}

/// How many of a progress bar's `cells` are filled for `percent`, rounded down like the
/// percentage itself, so the bar never shows more than the number beside it.
pub fn filled_cells(percent: u8, cells: usize) -> usize {
    usize::from(percent.min(100)) * cells / 100
}

/// The lead of one repo. One lead, one repo: the board refuses a second lead on a repo and
/// a lead (by name or session) on a second repo. A milestone's lead is its repo's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lead {
    /// Shown in the tree, like "Catalogue Lead".
    pub name: String,
    /// The Claude session name workers message the lead by.
    pub session: String,
    /// `None` when the stored colour is not in this build's palette: shown without colour.
    pub colour: Option<Colour>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub stage: Stage,
    /// A standalone task's repo; `None` for a task under a milestone (its milestone's repo
    /// is its) and for a standalone task in no repo yet.
    pub repo: Option<RepoId>,
    /// Its subtasks in id order. One level only, and they never count towards the
    /// milestone's percentage or its Done mark: the task shows its own done/total instead.
    pub subtasks: Vec<Subtask>,
    pub details: TaskDetails,
}

/// What a task's detail view shows beyond its card, each set through `tower task`; `None`
/// or empty until then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetails {
    /// What and why, one or more lines.
    pub description: Option<String>,
    /// Standing context: "when picking this up, do ABC", for this task only.
    pub context: Option<String>,
    /// Its ticket in whichever tracker it came from: an http or https URL.
    pub link: Option<String>,
    /// Its merge or pull request and pipeline, one line, like "!44 draft into feat/bookmarks ·
    /// pipeline running".
    pub mr: Option<String>,
    /// Who is on it, by name; cards say "unassigned" without one.
    pub assignee: Option<String>,
    /// In the order they were added; `tower` numbers them from 1.
    pub criteria: Vec<Criterion>,
    /// The other tasks it waits for, in id order.
    pub blocked_by: Vec<TaskId>,
    /// Why it is blocked, set with `tower task block` and cleared with `unblock`: the flag
    /// behind the tree's and the card's "!", the lead's "N blocked", and the reason where its
    /// thread stands. `None` while it is not blocked. Not the same as `blocked_by`, a
    /// dependency.
    pub blocked: Option<String>,
    /// Its latest progress post (`tower post --progress`, by anyone) in a thread linked to
    /// it, by time then id, with its author and time: its doing line. Read from the
    /// messages when the board is read; `None` until one is posted.
    pub doing: Option<Message>,
    /// Who moved it to Done, when they said (`tower task stage N done --by`); `None`
    /// otherwise, and once it leaves Done.
    pub done_by: Option<String>,
    /// A standalone task's repo; `None` for a milestone's task, whose repo is its
    /// milestone's, and for a standalone task in no repo yet.
    pub repo: Option<RepoId>,
}

/// One acceptance criterion of a task, met or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub text: String,
    pub done: bool,
}

/// A task's ticket, read from its tracker link: the number of the issue or merge
/// request the link names on a forge, or only that the task links somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ticket {
    /// A GitLab issue or work item, or a GitHub issue or pull request: "#N".
    Issue(u64),
    /// A GitLab merge request: "!N", as GitLab writes it.
    MergeRequest(u64),
    /// Any other link (another tracker, a document): "link".
    Link,
}

impl Ticket {
    /// The ticket `link` names: GitLab's `/-/issues/N`, `/-/work_items/N` and
    /// `/-/merge_requests/N`, GitHub's `/<owner>/<repo>/issues/N` and `/pull/N` (on any host,
    /// so `www.github.com` and GitHub Enterprise too), whatever follows the number (a
    /// merge request's `/diffs`, a pull request's `/files`, a comment's `#...`); any other
    /// link, or one that is no URL, is `Link`.
    pub fn of(link: &str) -> Ticket {
        forge_item(link).map_or(Ticket::Link, |(ticket, _)| ticket)
    }
}

/// The forge ticket `link` names, as `Ticket::of` reads it, with the project it is in:
/// GitLab's is the path before its "-" segment, GitHub's the first two segments. A GitLab
/// group's work item (`/groups/<group>/-/work_items/N`; `groups` is a path GitLab keeps for
/// itself, never a namespace) is in no project. `None` for any other link.
fn forge_item(link: &str) -> Option<(Ticket, Option<ForgeProject>)> {
    let url = url::Url::parse(link.trim()).ok()?;
    let parts: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let number = |s: &str| s.parse::<u64>().ok();
    // GitLab puts a project's pages after a "-" segment; GitHub's are the third.
    let (project, after) = match parts.iter().position(|s| *s == "-") {
        Some(dash) => (&parts[..dash], &parts[dash + 1..]),
        None if parts.len() >= 2 => parts.split_at(2),
        None => return None,
    };
    let gitlab = parts.contains(&"-");
    let ticket = match after {
        ["issues" | "work_items", n, ..] if gitlab => Ticket::Issue(number(n)?),
        ["merge_requests", n, ..] if gitlab => Ticket::MergeRequest(number(n)?),
        ["issues" | "pull", n, ..] if !gitlab => Ticket::Issue(number(n)?),
        _ => return None,
    };
    let of_group = gitlab && project.first() == Some(&"groups");
    Some((
        ticket,
        (!of_group).then(|| ForgeProject::new(&url, project)),
    ))
}

/// The forge hosts a link is read as a forge ticket on even when no repo on the board links
/// there; the board also knows the host of each repo's forge link.
pub const FORGE_HOSTS: [&str; 2] = ["github.com", "gitlab.com"];

/// A project on a forge, read from a link: its host, with any port, and its path, in lower
/// case, as GitHub and GitLab read both whatever their case. Written `host/path`, like
/// `github.com/acme/bookshop`. The scheme is not part of it: http and https are one forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeProject(String);

impl ForgeProject {
    fn new(url: &url::Url, path: &[&str]) -> ForgeProject {
        let mut at = url.host_str().unwrap_or_default().to_string();
        if let Some(port) = url.port() {
            at.push_str(&format!(":{port}"));
        }
        for segment in path {
            at.push('/');
            at.push_str(segment);
        }
        ForgeProject(at.to_ascii_lowercase())
    }

    /// The project a forge issue, work item, or merge or pull request `link` is in, read as
    /// `Ticket::of` reads it; `None` for a GitLab group's work item, in no project, and for
    /// any other link, such as another tracker's.
    pub fn of_ticket(link: &str) -> Option<ForgeProject> {
        forge_item(link).and_then(|(_, project)| project)
    }

    /// Its host, with any port, like `github.com` or `gitlab.example.com:8443`.
    pub fn host(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(host, _)| host)
    }

    /// Whether this address goes on past `project`'s, as a link to a page of that project
    /// does (`.../docs/tree/main` past `.../docs`, never `.../docs2`). A repo link that does
    /// is no project's address: a GitHub project is two segments, and a GitLab project
    /// holds no project under it.
    pub fn goes_past(&self, project: &ForgeProject) -> bool {
        (self.0.strip_prefix(&project.0)).is_some_and(|rest| rest.starts_with('/'))
    }

    /// The project a repo's forge `link` names: its whole path, without a trailing `/` or
    /// `.git`. `None` for a link that is no URL.
    pub fn of_repo(link: &str) -> Option<ForgeProject> {
        let url = url::Url::parse(link.trim()).ok()?;
        let mut parts: Vec<&str> = url
            .path_segments()
            .map(|p| p.filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        if let Some(last) = parts.pop() {
            parts.push(last.strip_suffix(".git").unwrap_or(last));
        }
        Some(ForgeProject::new(&url, &parts))
    }
}

impl fmt::Display for ForgeProject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ticket::Issue(n) => write!(f, "#{n}"),
            Ticket::MergeRequest(n) => write!(f, "!{n}"),
            Ticket::Link => f.write_str("link"),
        }
    }
}

impl Task {
    /// Its ticket, from its tracker link; `None` for a task with no link.
    pub fn ticket(&self) -> Option<Ticket> {
        self.details.link.as_deref().map(Ticket::of)
    }

    /// Who its card names: its assignee; for a Done task with none that its milestone's
    /// `lead` moved to Done, "done by lead"; else "unassigned".
    pub fn who(&self, lead: Option<&Lead>) -> &str {
        match (&self.details.assignee, &self.details.done_by) {
            (Some(a), _) => a,
            (None, Some(by)) if self.stage == Stage::Done && Role::of(by, lead) == Role::Lead => {
                "done by lead"
            }
            _ => "unassigned",
        }
    }

    pub fn done_subtasks(&self) -> usize {
        self.subtasks
            .iter()
            .filter(|s| s.stage == Stage::Done)
            .count()
    }

    /// Its doing line, where its home thread stands: why it is blocked while it is, else the
    /// first line of the worker's latest progress post; `None` with neither.
    pub fn doing_line(&self) -> Option<DoingLine<'_>> {
        if let Some(reason) = &self.details.blocked {
            return Some(DoingLine::Blocked(reason));
        }
        let post = self.details.doing.as_ref()?.text.as_str();
        Some(DoingLine::Post(post.lines().next().unwrap_or(post)))
    }
}

/// What a task's doing line says: the blocked flag's reason wins over the latest post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoingLine<'a> {
    Blocked(&'a str),
    Post(&'a str),
}

/// A step of a task, with the same three stages. Its id comes from the same sequence as the
/// tasks' (`#<n>`), so `tower task stage` moves either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtask {
    pub id: TaskId,
    pub title: String,
    pub stage: Stage,
}

/// A thread's id on the message board, shown and typed as `T<n>` (for example `T3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadId(pub i64);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.0)
    }
}

impl FromStr for ThreadId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let digits = s.strip_prefix('T').unwrap_or("");
        match digits.parse::<i64>() {
            Ok(n) if n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(ThreadId(n)),
            _ => bail!("{s:?} is not a thread id: expected T followed by a number, like T3"),
        }
    }
}

/// What to do about `milestone` having no lead: register one on its repo, or first give it
/// a repo when it has none (`has_repo`).
pub fn no_lead_hint(milestone: MilestoneId, has_repo: bool) -> String {
    if has_repo {
        format!("add one with tower lead add {milestone}")
    } else {
        format!(
            "it is in no repo yet: tower milestone set {milestone} --repo R<n> first, then \
             tower lead add {milestone}"
        )
    }
}

/// The dashboard's line for a milestone with no lead, as short as the column allows: what
/// to type next, which is giving it a repo when it has none.
pub fn no_lead_line(milestone: MilestoneId, has_repo: bool) -> String {
    if has_repo {
        format!("no lead yet: add one with tower lead add {milestone}")
    } else {
        format!("no repo: tower milestone set {milestone} --repo R<n>")
    }
}

/// Whose message board a command means: the board of a milestone's repo (`M2`), and the
/// thread is about that milestone, or a lead's repo's board by the lead's session name, and
/// the thread is about the whole repo. A word that reads as a milestone id is taken as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardOf {
    Milestone(MilestoneId),
    Session(String),
}

impl FromStr for BoardOf {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if let Ok(m) = s.parse() {
            return Ok(BoardOf::Milestone(m));
        }
        // A lead already on the board is found by its session name as stored, spaces and
        // all ("Catalogue Lead").
        check_line("lead session name", s)
            .map(BoardOf::Session)
            .map_err(|_| {
                anyhow::anyhow!(
                    "{s:?} is neither a milestone id (like M2) nor a lead's session name"
                )
            })
    }
}

/// Open while the team talks; Resolved, with evidence as its last message, after which it
/// takes no more messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Open,
    Resolved,
}

impl ThreadStatus {
    /// The name shown in the thread's pill.
    pub fn name(self) -> &'static str {
        match self {
            ThreadStatus::Open => "open",
            ThreadStatus::Resolved => "resolved",
        }
    }

    /// How many of its latest messages the board loads for the summary's chat previews: an
    /// open thread's last twelve, enough for the bubbles a preview shows (three of two
    /// messages each) with the acks and tower's lines between them; a resolved thread's last
    /// four (the summary folds it into its RESOLVED row). The whole thread loads only
    /// when the full board shows it.
    pub fn shown(self) -> usize {
        match self {
            ThreadStatus::Open => 12,
            ThreadStatus::Resolved => 4,
        }
    }
}

/// The author tower keeps for its own messages (stuck-worker flags and nudges).
pub const SYSTEM_AUTHOR: &str = "system";

/// How the watch's delivery failure starts (`deliver::failure_text`), posted as
/// `SYSTEM_AUTHOR`: for the owner's awareness, not a question for them (spec 3a).
pub const DELIVERY_FAILURE: &str = "@manager could not deliver to ";

/// How to clear a worker that is the assignee of `tasks` (one or more, not done, in id
/// order) once it is gone, for a delivery failure or an orphan flag naming it: "if it
/// was closed: tower task unassign 1, or set the next worker".
pub fn closed_worker_hint(tasks: &[TaskId]) -> String {
    let unassign: Vec<String> = (tasks.iter())
        .map(|t| format!("tower task unassign {}", t.0))
        .collect();
    format!(
        "if it was closed: {}, or set the next worker",
        unassign.join(" and ")
    )
}

/// Checks the name a message is posted by, as `check_name` does: like dev-2, or a lead's
/// session name such as "Catalogue Lead". `system` (in any case or spacing) is kept for
/// tower's own messages. Returns it trimmed.
pub fn check_author(value: &str) -> Result<String> {
    let author = check_name("author", value)?;
    if is_reserved(&author, SYSTEM_AUTHOR) {
        bail!("the author {SYSTEM_AUTHOR} is kept for tower's own messages");
    }
    Ok(author)
}

/// A conversation on a lead's message board: a problem, a question, a decision, or a task
/// team's own thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: ThreadId,
    pub title: String,
    /// The task it belongs to, by a stored link (never by its title).
    pub task: Option<TaskId>,
    /// The milestone it is about, by a stored link; `None` for a task's thread and a
    /// repo-wide one.
    pub milestone: Option<MilestoneId>,
    /// Who resolved it and when; `None` while it is open.
    pub resolved: Option<Resolution>,
    /// Who started it: the author of its opening message.
    pub by: String,
    /// How many messages it has in all, the opening one included.
    pub total: usize,
    /// Everyone who posted in it, each once (as stored), in the order they first posted:
    /// from its whole history, even on a loaded board that carries only its latest messages.
    pub authors: Vec<String>,
    /// Its latest messages in the order posted (by time, then id): every one from
    /// `Store::thread`, only the ones the summary shows (`ThreadStatus::shown`) on a loaded
    /// board. A resolved thread's last is its evidence.
    pub messages: Vec<Message>,
}

impl Thread {
    /// A thread with its whole history: who started it and its total come from `messages`.
    pub fn whole(
        id: ThreadId,
        title: String,
        task: Option<TaskId>,
        resolved: Option<Resolution>,
        messages: Vec<Message>,
    ) -> Thread {
        Thread {
            id,
            title,
            task,
            milestone: None,
            resolved,
            by: messages
                .first()
                .map(|m| m.author.clone())
                .unwrap_or_default(),
            total: messages.len(),
            authors: messages.iter().fold(Vec::new(), |mut all, m| {
                if !all.contains(&m.author) {
                    all.push(m.author.clone());
                }
                all
            }),
            messages,
        }
    }

    pub fn status(&self) -> ThreadStatus {
        match self.resolved {
            None => ThreadStatus::Open,
            Some(_) => ThreadStatus::Resolved,
        }
    }

    /// Its replies: every message but the opening one.
    pub fn replies(&self) -> usize {
        self.total.saturating_sub(1)
    }

    /// When its latest message was posted, which is the thread's age.
    pub fn latest(&self) -> Option<i64> {
        self.messages.last().map(|m| m.at)
    }
}

/// Who resolved a thread, and when, in seconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub by: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Its id on the board, which orders messages posted in the same second.
    pub id: i64,
    pub author: String,
    pub text: String,
    /// When it was posted, in seconds since the Unix epoch.
    pub at: i64,
}

/// The owner's handle in messages.
pub const OWNER_HANDLE: &str = "@manager";

/// The author the owner posts as, only from the dashboard: the CLI refuses it (see
/// `refuse_owner`), so no agent can speak as the owner through `tower`.
pub const OWNER_AUTHOR: &str = "manager";

/// Refuses `author` when it is the owner's, `manager` in any ASCII case or spacing: for the
/// CLI, which agents run. The dashboard posts as the owner through the store instead.
pub fn refuse_owner(author: &str) -> Result<()> {
    if is_reserved(author, OWNER_AUTHOR) {
        bail!(
            "only the owner posts as {OWNER_AUTHOR}, from the dashboard; post under your own \
             name, like dev-2"
        );
    }
    Ok(())
}

/// The names `text` @mentions, like `@qa-2`, in order: a `@` not right after an ASCII
/// letter or digit (so not an ASCII email address; after any other letter, such as kana, an
/// ideograph or é, it is), then letters, digits, `-` and `_`, less any trailing `-` or `_`.
/// A name with spaces, such as an older lead's session name, cannot be mentioned. What a post
/// quotes is no mention: a fenced block (between lines starting with three backticks), a
/// code span (between backticks on one line; an unmatched one ends with its line) and a
/// line starting with `>`. Nor is code written without backticks: a name followed straight
/// by `(` and an ASCII character or nothing, as in `@cart(x)`, or by `.` and an ASCII
/// letter, digit or `_`, as in `@cart.isEmpty()`. A sentence that ends on a name, `@qa.`,
/// still mentions it, and so does a name before `.` or `(` and a character that is not
/// ASCII, as Chinese or Japanese is written: `请@dev-2.谢谢`, `请@dev-2(明天)看一下`.
pub fn mentioned(text: &str) -> Vec<&str> {
    mentions(text).into_iter().map(|(_, name)| name).collect()
}

/// The mentions `mentioned` reads, each with the byte offset of its `@` in `text`, so a
/// view can mark exactly what counts as a mention and nothing a post quotes.
pub fn mentions(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut fenced = false;
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let at = offset;
        offset += raw.len();
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let start = line.trim_start();
        if start.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || start.starts_with('>') {
            continue;
        }
        let mut code = false;
        let mut prev: Option<char> = None;
        for (i, c) in line.char_indices() {
            if c == '`' {
                code = !code;
            } else if c == '@' && !code && !prev.is_some_and(|p| p.is_ascii_alphanumeric()) {
                let rest = &line[i + 1..];
                let end = (rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')))
                .unwrap_or(rest.len());
                let name = rest[..end].trim_end_matches(['-', '_']);
                // Code, not a mention: `@cart.isEmpty()`, `@cart(x)`. Not `@qa.谢谢`.
                let mut after = rest[end..].chars();
                let code_like = match (after.next(), after.next()) {
                    (Some('('), n) => n.is_none_or(|n| n.is_ascii()),
                    (Some('.'), Some(n)) => n.is_ascii_alphanumeric() || n == '_',
                    _ => false,
                };
                if !name.is_empty() && !code_like {
                    found.push((at + i, name));
                }
            }
            prev = Some(c);
        }
    }
    found
}

/// Where `text` mentions the owner, as `mentioned` reads it and in any ASCII case: the
/// byte range of each `@manager`.
pub fn owner_mentions(text: &str) -> Vec<std::ops::Range<usize>> {
    let owner = &OWNER_HANDLE[1..];
    (mentions(text).into_iter())
        .filter(|(_, name)| name.eq_ignore_ascii_case(owner))
        .map(|(at, name)| at..at + 1 + name.len())
        .collect()
}

/// What a message is, as the spec (3a) classifies it, from its text alone. The one
/// classifier: the board's styles and the owner's alerts both use it. A mention is what
/// `mentioned` reads, so a quoted `@manager` (code span, fenced block, `>` line) is none.
/// - Needs you: a message that starts with `question:` and mentions `@manager`, the lead's
///   tag for what waits on the owner. Tower's own post
///   about a worker on a prompt in the terminal is one.
/// - Awareness ("for your awareness"): any other message that mentions `@manager`:
///   answers, reports, decisions, delivery failures (`DELIVERY_FAILURE`).
/// - Blocked: a message that starts with `blocked:`, a worker saying it is stuck.
/// - Plain: anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Blocked,
    NeedsYou,
    Awareness,
}

/// How a message to the owner that waits on them starts (`Tone::NeedsYou`).
pub const QUESTION: &str = "question:";

/// The longest ack, in characters.
const ACK_MAX: usize = 40;
/// How an ack starts, in any case, followed by the end or a character that is no letter.
const ACK_WORDS: [&str; 6] = [
    "acknowledged",
    "plan approved",
    "copy",
    "ack",
    "ok",
    "thanks",
];

/// A short ack, which the board shows as a reply under the message it answers: a person's
/// message of at most 40 characters that starts with copy, ack, acknowledged, plan
/// approved, ok or thanks, and never one that mentions the owner (UI v0.1.7).
pub fn is_ack(m: &Message) -> bool {
    let text = m.text.trim();
    if m.author.eq_ignore_ascii_case(SYSTEM_AUTHOR)
        || text.chars().count() > ACK_MAX
        || !owner_mentions(text).is_empty()
    {
        return false;
    }
    ACK_WORDS.iter().any(|word| {
        text.get(..word.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(word))
            && !(text[word.len()..].chars().next()).is_some_and(char::is_alphanumeric)
    })
}

impl Tone {
    pub fn of(text: &str) -> Tone {
        // The owner's name in any ASCII case, as delivery reads it.
        let owner = &OWNER_HANDLE[1..];
        let mentions = (mentioned(text).iter()).any(|n| n.eq_ignore_ascii_case(owner));
        if mentions && text.starts_with(QUESTION) {
            Tone::NeedsYou
        } else if mentions {
            Tone::Awareness
        } else if text.starts_with("blocked:") {
            Tone::Blocked
        } else {
            Tone::Plain
        }
    }
}

/// A message's tone (`Tone::of`): the one place the board's styles, the alerts and the
/// mark-read check classify a message.
pub fn tone_of(message: &Message) -> Tone {
    Tone::of(&message.text)
}

/// What an author is on a team, for the colour of their name: the lead (by its session or
/// display name), else dev, senior (reviewer) or qa (tester) by the name being the role or
/// the role then "-" or a digit (qa, qa-2, qa2); or anyone else (the owner as manager, helpers). Names compare ignoring ASCII case.
/// `system`, tower's own author (the stuck-worker watch), is a role of its own, ahead
/// of the lead's names, since no one else can post as it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    Lead,
    Dev,
    Senior,
    Qa,
    Other,
}

impl Role {
    pub fn of(author: &str, lead: Option<&Lead>) -> Role {
        let is = |name: &str| author.eq_ignore_ascii_case(name);
        // The role itself, or the role then "-" or a digit: dev, dev-2, dev2; not devon.
        let starts = |role: &str| {
            author
                .get(..role.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(role))
                && author[role.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == '-' || c.is_ascii_digit())
        };
        match lead {
            _ if is(SYSTEM_AUTHOR) => Role::System,
            Some(l) if is(&l.session) || is(&l.name) => Role::Lead,
            _ if starts("dev") => Role::Dev,
            _ if starts("senior") => Role::Senior,
            _ if starts("qa") => Role::Qa,
            _ => Role::Other,
        }
    }
}

/// How long ago `at` was at `now`, as the board shows a thread's age: "now" under a minute
/// (or for a time ahead of the clock), then whole minutes, hours or days, rounded down.
pub fn age(at: i64, now: i64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// When a message was posted, as the full board and `tower thread show` show it: "now" under
/// a minute, else its `age` and "ago", like "8m ago".
pub fn ago(at: i64, now: i64) -> String {
    match age(at, now).as_str() {
        "now" => "now".into(),
        a => format!("{a} ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn milestone(stages: &[Stage], marked_done: bool) -> Milestone {
        Milestone {
            id: MilestoneId(1),
            title: "m".into(),
            goal: "g".into(),
            repo: None,
            lead: None,
            tasks: stages
                .iter()
                .enumerate()
                .map(|(i, &stage)| Task {
                    id: TaskId(i as i64 + 1),
                    title: "t".into(),
                    stage,
                    repo: None,
                    subtasks: Vec::new(),
                    details: TaskDetails::default(),
                })
                .collect(),
            marked_done,
            done_at: None,
            folded: false,
            threads: Vec::new(),
            details: MilestoneDetails::default(),
        }
    }

    /// `done` Done tasks and the rest Waiting, `all` in total.
    fn progress(done: usize, all: usize) -> Milestone {
        let mut stages = vec![Stage::Done; done];
        stages.resize(all, Stage::Waiting);
        milestone(&stages, false)
    }

    #[test]
    fn percent_rounds_down() {
        for (done, all, want) in [
            (0, 0, 0),
            (0, 3, 0),
            (1, 5, 20),
            (1, 3, 33),
            (2, 3, 66),
            (1, 8, 12),
            (199, 200, 99),
            (200, 200, 100),
        ] {
            assert_eq!(progress(done, all).percent(), want, "{done}/{all}");
        }
    }

    #[test]
    fn the_bar_rounds_down_from_the_percentage() {
        for (percent, want) in [
            (0, 0),
            (12, 0),
            (13, 1),
            (20, 1),
            (49, 3),
            (50, 4),
            (99, 7),
            (100, 8),
        ] {
            assert_eq!(filled_cells(percent, 8), want, "{percent}%");
        }
        // 1 of 8 tasks is 12%, so the bar stays empty rather than showing 1/8 = 12.5%.
        assert_eq!(filled_cells(progress(1, 8).percent(), 8), 0);
    }

    #[test]
    fn a_milestone_is_in_progress_once_any_task_leaves_waiting() {
        use Stage::*;
        assert_eq!(milestone(&[], false).stage(), Waiting);
        assert_eq!(milestone(&[Waiting, Waiting], false).stage(), Waiting);
        assert_eq!(milestone(&[Waiting, InProgress], false).stage(), InProgress);
        assert_eq!(milestone(&[Done, Waiting], false).stage(), InProgress);
        // Every task Done is still In progress until the lead marks it Done.
        assert_eq!(milestone(&[Done, Done], false).stage(), InProgress);
        assert_eq!(milestone(&[Done, Done], true).stage(), Done);
    }

    #[test]
    fn a_done_mark_counts_only_while_every_task_is_done() {
        use Stage::*;
        // A task moved back from Done reopens the milestone.
        assert_eq!(milestone(&[Done, InProgress], true).stage(), InProgress);
        assert_eq!(milestone(&[Waiting], true).stage(), Waiting);
        // No tasks is never Done, marked or not.
        assert_eq!(milestone(&[], true).stage(), Waiting);
        assert!(!milestone(&[], false).can_be_done());
        assert!(!milestone(&[Done, InProgress], false).can_be_done());
        assert!(milestone(&[Done], false).can_be_done());
    }

    /// Gives every task of `m` subtasks at `stages`.
    fn with_subtasks(mut m: Milestone, stages: &[Stage]) -> Milestone {
        for t in &mut m.tasks {
            t.subtasks = stages
                .iter()
                .enumerate()
                .map(|(i, &stage)| Subtask {
                    id: TaskId(100 + i as i64),
                    title: "s".into(),
                    stage,
                })
                .collect();
        }
        m
    }

    #[test]
    fn subtasks_never_count_towards_the_percentage_or_the_done_mark() {
        use Stage::*;
        // Open subtasks under Done tasks: still 100%, and the lead may mark it Done.
        let m = with_subtasks(milestone(&[Done, Done], true), &[Waiting, InProgress]);
        assert_eq!((m.percent(), m.can_be_done(), m.stage()), (100, true, Done));
        // Done subtasks under open tasks: still 0% and Waiting, and not markable.
        let m = with_subtasks(milestone(&[Waiting, Waiting], true), &[Done, Done]);
        assert_eq!(
            (m.percent(), m.can_be_done(), m.stage()),
            (0, false, Waiting)
        );
        assert_eq!(m.done_tasks(), 0);
    }

    #[test]
    fn a_task_counts_its_done_subtasks() {
        use Stage::*;
        let m = with_subtasks(
            milestone(&[Waiting], false),
            &[Done, InProgress, Done, Waiting, Done],
        );
        assert_eq!(
            (m.tasks[0].done_subtasks(), m.tasks[0].subtasks.len()),
            (3, 5)
        );
        let none = milestone(&[Done], false);
        assert_eq!(
            (none.tasks[0].done_subtasks(), none.tasks[0].subtasks.len()),
            (0, 0)
        );
    }

    #[test]
    fn stage_names_round_trip_and_anything_else_is_refused() {
        for st in Stage::ALL {
            assert_eq!(st.name().parse::<Stage>().unwrap(), st);
        }
        assert_eq!(Stage::InProgress.label(), "in progress");
        for bad in ["", "Done", "in progress", "in_progress", "blocked"] {
            let err = bad.parse::<Stage>().unwrap_err().to_string();
            assert_eq!(
                err,
                format!("{bad:?} is not a stage: pick one of waiting, in-progress, done")
            );
        }
    }

    #[test]
    fn task_id_takes_a_number_with_or_without_the_hash() {
        assert_eq!("12".parse::<TaskId>().unwrap(), TaskId(12));
        assert_eq!("#12".parse::<TaskId>().unwrap(), TaskId(12));
        for bad in ["", "#", "0", "#0", "-1", "+1", "12x", "M12", "##1", " 1"] {
            let err = bad.parse::<TaskId>().unwrap_err().to_string();
            assert!(err.contains("not a task id"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn milestone_id_round_trips() {
        let id: MilestoneId = "M12".parse().unwrap();
        assert_eq!(id, MilestoneId(12));
        assert_eq!(id.to_string(), "M12");
    }

    #[test]
    fn milestone_id_refuses_anything_else() {
        for bad in [
            "",
            "M",
            "12",
            "m1",
            "M0",
            "M-1",
            "M+1",
            "M1x",
            " M1",
            "M99999999999999999999",
        ] {
            let err = bad.parse::<MilestoneId>().unwrap_err().to_string();
            assert!(err.contains("not a milestone id"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn colour_names_round_trip_and_anything_else_is_refused() {
        for c in Colour::ALL {
            assert_eq!(c.name().parse::<Colour>().unwrap(), c);
        }
        for bad in ["", "Blue", "red", " blue", "#3d8fe0"] {
            let err = bad.parse::<Colour>().unwrap_err().to_string();
            assert_eq!(
                err,
                format!(
                    "{bad:?} is not a lead colour: \
                     pick one of blue, purple, orange, teal, pink, green"
                )
            );
        }
    }

    #[test]
    fn next_colour_is_the_first_unused_then_the_least_used() {
        use Colour::*;
        assert_eq!(Colour::next(&[]), Blue);
        assert_eq!(Colour::next(&[Blue]), Purple);
        // A gap left by an explicit choice is filled first.
        assert_eq!(Colour::next(&[Blue, Orange]), Purple);
        assert_eq!(Colour::next(&[Purple, Blue, Teal]), Orange);
        assert_eq!(Colour::next(&Colour::ALL), Blue);
        assert_eq!(
            Colour::next(&[Blue, Purple, Orange, Teal, Pink, Green, Blue]),
            Purple
        );
        assert_eq!(Colour::next(&[Green, Green]), Blue);
    }

    #[test]
    fn check_line_trims_and_accepts_plain_text() {
        assert_eq!(
            check_line("title", "  Bookmarks page ").unwrap(),
            "Bookmarks page"
        );
        assert_eq!(check_line("title", "Café 日本").unwrap(), "Café 日本");
    }

    #[test]
    fn check_line_refuses_empty() {
        for bad in ["", "   ", "\u{3000}"] {
            let err = check_line("title", bad).unwrap_err().to_string();
            assert_eq!(err, "the title is empty", "{bad:?}");
        }
    }

    #[test]
    fn check_line_refuses_control_characters() {
        for bad in [
            "a\nb",
            "a\tb",
            "a\u{1b}[31mb",
            "a\u{7f}b",
            "a\u{85}b",
            "a\0b",
        ] {
            let err = check_line("goal", bad).unwrap_err().to_string();
            assert!(
                err.starts_with("the goal contains a control character"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn check_line_refuses_format_and_separator_characters() {
        for (bad, code) in [
            ("abc\u{202E}def", "U+202E"),
            ("a\u{200B}b", "U+200B"),
            ("one\u{2028}two", "U+2028"),
            ("one\u{2029}two", "U+2029"),
        ] {
            let err = check_line("title", bad).unwrap_err().to_string();
            assert_eq!(
                err,
                format!(
                    "the title contains a control character ({code}); \
                     it must be one line of plain text"
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn check_message_takes_line_breaks_and_up_to_4000_characters() {
        // As the signed design's composer: leading blank lines dropped, the end trimmed, the first
        // line's indent kept, so a list reads in line.
        assert_eq!(
            check_message("message", " Plan:\n1. layout\n\n2. tests \n").unwrap(),
            " Plan:\n1. layout\n\n2. tests"
        );
        assert_eq!(
            check_message(
                "message",
                "\n  \n\n  - preheat the left oven\n  - proof the rye  \n\n"
            )
            .unwrap(),
            "  - preheat the left oven\n  - proof the rye"
        );
        // A tab is four spaces first, so an indent made with one is kept as four.
        assert_eq!(
            check_message("message", "\r\n\tblocked: the rye\r\n").unwrap(),
            "    blocked: the rye"
        );
        // A hidden whitespace character before the text goes, as it always did (the board
        // stores none), with the leading run up to it; an indent after it stays.
        assert_eq!(
            check_message("message", "\u{c}the glue").unwrap(),
            "the glue"
        );
        assert_eq!(
            check_message("message", "\n\u{2028}  the glue").unwrap(),
            "  the glue"
        );
        // A description still trims both ends.
        assert_eq!(
            check_text("description", "\n  - sift the flour\n  - weigh it\n").unwrap(),
            "- sift the flour\n  - weigh it"
        );
        assert!(check_message("message", &"é".repeat(MAX_MESSAGE_CHARS)).is_ok());
        let err = check_message("message", &"é".repeat(MAX_MESSAGE_CHARS + 1)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the message is 4001 characters long; the most is 4000"
        );
        // Pasted line ends and tabs are normalised, not refused.
        assert_eq!(
            check_message("message", "one\r\ntwo\rthree\n\tfour").unwrap(),
            "one\ntwo\nthree\n    four"
        );
        for (bad, code) in [
            ("a\u{7}b", "U+0007"),
            ("a\u{0}b", "U+0000"),
            ("a\u{85}b", "U+0085"),
            ("a\u{1b}[31mb", "U+001B"),
            ("a\u{202E}b", "U+202E"),
            ("a\u{2028}b", "U+2028"),
        ] {
            let err = check_message("evidence", bad).unwrap_err().to_string();
            assert_eq!(
                err,
                format!(
                    "the evidence contains a control character ({code}); it must be plain text"
                ),
                "{bad:?}"
            );
        }
        assert_eq!(
            check_message("message", "\n \n").unwrap_err().to_string(),
            "the message is empty"
        );
        // A title is still one line.
        assert!(check_line("thread title", "a\nb").is_err());
    }

    #[test]
    fn check_line_length_limit_is_in_characters() {
        let at_limit = "é".repeat(MAX_LINE_CHARS);
        assert!(check_line("title", &at_limit).is_ok());
        let over = "é".repeat(MAX_LINE_CHARS + 1);
        let err = check_line("title", &over).unwrap_err().to_string();
        assert_eq!(err, "the title is 201 characters long; the most is 200");
    }

    #[test]
    fn thread_id_round_trips_and_anything_else_is_refused() {
        let id: ThreadId = "T3".parse().unwrap();
        assert_eq!((id, id.to_string()), (ThreadId(3), "T3".to_string()));
        for bad in ["", "T", "3", "t3", "T0", "T-1", "T3x", " T3", "#3"] {
            let err = bad.parse::<ThreadId>().unwrap_err().to_string();
            assert!(err.contains("not a thread id"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_board_is_named_by_milestone_or_by_lead_session() {
        assert_eq!(
            "M2".parse::<BoardOf>().unwrap(),
            BoardOf::Milestone(MilestoneId(2))
        );
        assert_eq!(
            " cat-lead ".parse::<BoardOf>().unwrap(),
            BoardOf::Session("cat-lead".into())
        );
        // Not a milestone id, so a session name.
        assert_eq!(
            "M2x".parse::<BoardOf>().unwrap(),
            BoardOf::Session("M2x".into())
        );
        // A session name with spaces, as Claude's can be.
        assert_eq!(
            "Catalogue Lead".parse::<BoardOf>().unwrap(),
            BoardOf::Session("Catalogue Lead".into())
        );
        for bad in ["", "a\u{7}b"] {
            let err = bad.parse::<BoardOf>().unwrap_err().to_string();
            assert!(err.contains("neither a milestone id"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn authors_are_one_word_and_system_is_kept_for_tower() {
        assert_eq!(check_author(" dev-2 ").unwrap(), "dev-2");
        assert_eq!(check_author("manager").unwrap(), "manager");
        for (bad, want) in [
            ("", "the author is empty"),
            (
                "Catalogue  Desk",
                "the author \"Catalogue  Desk\" has two spaces in a row",
            ),
            (
                "dev\u{a0}2",
                "the author contains '\\u{a0}' (U+00A0); use only ASCII",
            ),
            ("dev\t2", "the author contains"),
            (" Sys tem ", "the author system is kept"),
            (
                "a\u{202E}b",
                "the author contains a control character (U+202E)",
            ),
            (
                "system",
                "the author system is kept for tower's own messages",
            ),
            (
                "SYSTEM",
                "the author system is kept for tower's own messages",
            ),
            // Lookalikes of manager: fullwidth, Cyrillic "а", and a combining mark.
            (
                "ｍａｎａｇｅｒ",
                "the author contains 'ｍ' (U+FF4D); use only ASCII",
            ),
            (
                "m\u{430}nager",
                "the author contains 'а' (U+0430); use only ASCII",
            ),
            (
                "manage\u{301}r",
                "the author contains '\\u{301}' (U+0301); use only ASCII",
            ),
            ("dev/2", "the author contains '/' (U+002F); use only ASCII"),
        ] {
            let err = check_author(bad).unwrap_err().to_string();
            assert!(err.starts_with(want), "{bad:?}: {err}");
        }
        for good in ["dev-2", "qa_1", "cat.lead", "Senior9", "Catalogue Lead"] {
            assert_eq!(check_author(good).unwrap(), good);
        }
        // Trimmed; the owner's name is refused (on the CLI) in any case and spacing.
        assert_eq!(check_author("  Catalogue Lead ").unwrap(), "Catalogue Lead");
        for owner in ["manager", " manager ", "MANAGER", "Man ager"] {
            assert!(refuse_owner(owner).is_err(), "{owner:?}");
        }
        assert!(refuse_owner("managers").is_ok());
    }

    #[test]
    fn session_names_take_the_same_characters_on_write_but_are_found_as_stored() {
        assert_eq!(check_session("cat-lead.2").unwrap(), "cat-lead.2");
        for bad in ["ｃａｔ-lead", "cat-l\u{435}ad", "cat:lead"] {
            let err = check_session(bad).unwrap_err().to_string();
            assert!(err.contains("use only ASCII letters"), "{bad:?}: {err}");
        }
        // A lead stored before the rule is still found by its session name.
        assert_eq!(
            "chef-é".parse::<BoardOf>().unwrap(),
            BoardOf::Session("chef-é".into())
        );
    }

    fn thread(texts: &[(&str, i64)]) -> Thread {
        let messages = (texts.iter().enumerate())
            .map(|(i, (text, at))| Message {
                id: i as i64 + 1,
                author: format!("dev-{i}"),
                text: (*text).into(),
                at: *at,
            })
            .collect();
        Thread::whole(ThreadId(1), "t".into(), None, None, messages)
    }

    #[test]
    fn replies_leave_out_the_opening_message_and_age_is_the_latest() {
        let t = thread(&[("open", 100), ("copy", 200), ("go", 300)]);
        assert_eq!((t.replies(), t.latest()), (2, Some(300)));
        assert_eq!((t.by.as_str(), t.status()), ("dev-0", ThreadStatus::Open));
        // On a loaded board only the shown messages are there; the counts still hold.
        let summary = Thread {
            messages: t.messages[2..].to_vec(),
            ..t.clone()
        };
        assert_eq!((summary.replies(), summary.latest()), (2, Some(300)));
        let t = thread(&[("open", 100)]);
        assert_eq!((t.replies(), t.latest()), (0, Some(100)));
        let t = thread(&[]);
        assert_eq!((t.replies(), t.latest()), (0, None));
    }

    #[test]
    fn whole_threads_list_each_author_once_in_order_of_first_post() {
        let mut t = thread(&[("a", 1), ("b", 2), ("c", 3)]);
        t = Thread::whole(t.id, t.title, None, None, {
            let mut m = t.messages;
            m[2].author = "dev-0".into();
            m
        });
        assert_eq!(t.authors, ["dev-0", "dev-1"]);
    }

    #[test]
    fn the_team_is_assignees_then_thread_authors_without_lead_owner_system_or_you() {
        let mut m = milestone(&[Stage::Waiting; 3], false);
        m.lead = Some(Lead {
            name: "Catalogue Lead".into(),
            session: "cat-lead".into(),
            colour: None,
        });
        m.tasks[0].details.assignee = Some("dev-two".into());
        m.tasks[2].details.assignee = Some("dev-1".into());
        let board = Board::default();
        assert_eq!(m.team(&board), ["dev-two", "dev-1"]);
        let mut a = thread(&[]);
        a.authors = ["Cat-Lead", "senior", "manager", "System", "Dev-Two", "qa"]
            .map(String::from)
            .to_vec();
        a.milestone = Some(MilestoneId(1));
        let mut b = thread(&[]);
        b.authors = ["Catalogue Lead", "you", "devon", "Senior"]
            .map(String::from)
            .to_vec();
        b.task = Some(TaskId(2));
        // A repo-wide thread on the same board is not this milestone's.
        let mut c = thread(&[]);
        c.authors = vec!["triage-bot".into()];
        m.threads = vec![a, b, c];
        // A name is on it once, as it first appears; the lead by session or name is not.
        assert_eq!(
            m.team(&board),
            ["dev-two", "dev-1", "senior", "qa", "devon"]
        );
        // No assignees and no threads: nobody.
        assert!(milestone(&[], false).team(&board).is_empty());
    }

    #[test]
    fn another_repos_lead_posting_here_is_not_on_the_team_even_named_like_a_role() {
        let mut m = milestone(&[Stage::Waiting], false);
        m.lead = Some(Lead {
            name: "kids-lead".into(),
            session: "kids-lead".into(),
            colour: None,
        });
        let mut t = thread(&[]);
        t.authors = ["QA-Tools-Lead", "qa-12", "dev-portal-lead"]
            .map(String::from)
            .to_vec();
        t.milestone = Some(MilestoneId(1));
        m.threads = vec![t];
        let repo = |id: i64, name: &str| Repo {
            id: RepoId(id),
            path: Some(format!("/src/{name}")),
            link: None,
            lead: Some(Lead {
                name: format!("{name}-lead"),
                session: format!("{name}-lead"),
                colour: None,
            }),
            folded: false,
            done_folded: true,
            threads: Vec::new(),
        };
        let board = Board {
            repos: vec![repo(1, "kids"), repo(2, "qa-tools"), repo(3, "dev-portal")],
            ..Board::default()
        };
        // By session or name, ignoring case: the leads are off; the tester stays.
        assert_eq!(m.team(&board), ["qa-12"]);
        // Without the board's other leads, their names read as roles and join the team.
        assert_eq!(
            m.team(&Board::default()),
            ["QA-Tools-Lead", "qa-12", "dev-portal-lead"]
        );
    }

    #[test]
    fn age_rounds_down_to_minutes_hours_and_days() {
        for (secs, want) in [
            (-30, "now"),
            (0, "now"),
            (59, "now"),
            (60, "1m"),
            (3599, "59m"),
            (3600, "1h"),
            (86_399, "23h"),
            (86_400, "1d"),
            (10 * 86_400 + 5, "10d"),
        ] {
            assert_eq!(age(1_000_000, 1_000_000 + secs), want, "{secs}s");
        }
    }

    #[test]
    fn roles_come_from_the_name() {
        // A spaced session name is the lead's, in any case, so it shows in the lead's colour.
        let spaced = Lead {
            name: "Catalogue Lead".into(),
            session: "Catalogue Lead".into(),
            colour: None,
        };
        assert_eq!(Role::of("catalogue lead", Some(&spaced)), Role::Lead);
        assert_eq!(Role::of("Catalogue", Some(&spaced)), Role::Other);
        let lead = Lead {
            name: "Catalogue".into(),
            session: "cat-lead".into(),
            colour: None,
        };
        for (author, want) in [
            ("cat-lead", Role::Lead),
            ("Cat-Lead", Role::Lead),
            ("catalogue", Role::Lead),
            ("dev-2", Role::Dev),
            ("Dev", Role::Dev),
            ("dev2", Role::Dev),
            ("Dev-x", Role::Dev),
            ("senior", Role::Senior),
            ("Senior-One", Role::Senior),
            ("QA", Role::Qa),
            ("qa-2", Role::Qa),
            ("qa1", Role::Qa),
            // A worker's name, <role>-<task>-<area> (#92), reads its role from the front.
            ("dev-119-db", Role::Dev),
            ("qa-119-help", Role::Qa),
            ("senior-119-api", Role::Senior),
            ("db-119", Role::Other),
            ("developer", Role::Other),
            ("devon", Role::Other),
            ("device-x", Role::Other),
            ("qatar", Role::Other),
            ("seniority", Role::Other),
            ("q", Role::Other),
            ("manager", Role::Other),
            ("de", Role::Other),
            ("dé", Role::Other),
        ] {
            assert_eq!(Role::of(author, Some(&lead)), want, "{author}");
        }
        assert_eq!(Role::of("cat-lead", None), Role::Other);
        // A lead named like a dev is still the lead.
        let devlead = Lead {
            session: "dev-lead".into(),
            ..lead
        };
        assert_eq!(Role::of("dev-lead", Some(&devlead)), Role::Lead);
        // system is tower's own, even on a board whose lead goes by that name.
        let named_system = Lead {
            session: "System".into(),
            ..devlead
        };
        for lead in [None, Some(&named_system)] {
            assert_eq!(Role::of("system", lead), Role::System);
            assert_eq!(Role::of("SYSTEM", lead), Role::System);
        }
        assert_eq!(Role::of("systems", None), Role::Other);
    }

    #[test]
    fn the_blocked_flag_counts_and_its_reason_wins_over_the_latest_post() {
        let mut m = milestone(&[Stage::Waiting, Stage::InProgress, Stage::Done], false);
        assert_eq!(m.blocked_tasks(), 0);
        assert_eq!(m.tasks[0].doing_line(), None);
        // The doing line is the post's first line only.
        m.tasks[1].details.doing = Some(Message {
            id: 1,
            author: "dev-2".into(),
            text: "testing the shelf grid\nthen the ribbon".into(),
            at: 0,
        });
        assert_eq!(
            m.tasks[1].doing_line(),
            Some(DoingLine::Post("testing the shelf grid"))
        );
        m.tasks[1].details.blocked = Some("printer has not sent the covers".into());
        m.tasks[2].details.blocked = Some("waiting on review".into());
        assert_eq!(m.blocked_tasks(), 2);
        assert_eq!(
            m.tasks[1].doing_line(),
            Some(DoingLine::Blocked("printer has not sent the covers"))
        );
        // Blocked by is a dependency and never counts as blocked.
        m.tasks[2].details.blocked = None;
        m.tasks[0].details.blocked_by = vec![TaskId(2)];
        assert_eq!(m.blocked_tasks(), 1);
    }

    #[test]
    fn need_you_is_only_a_question_to_the_owner_other_mentions_are_awareness() {
        for (text, want) in [
            // A question to the owner, from anyone.
            (
                "question: @manager can you approve the merge order?",
                Tone::NeedsYou,
            ),
            ("question: which order, @manager?", Tone::NeedsYou),
            // Tower's own herdr-prompt post (`watch`) is one.
            (
                "question: @manager dev-1 on #13 is waiting on a prompt in herdr that only \
                 you can answer (@cat-lead for your information).",
                Tone::NeedsYou,
            ),
            // Every other mention is for the owner's awareness: replies, reports, decisions,
            // delivery failures, a worker's "decision:" or "blocked:".
            ("@manager can you approve?", Tone::Awareness),
            ("@manager done: merged, the covers show.", Tone::Awareness),
            (
                "decision: the shelf shows 12 books, not 10. @manager for your awareness",
                Tone::Awareness,
            ),
            ("blocked: no covers, @manager please", Tone::Awareness),
            ("@manager question: not at the start", Tone::Awareness),
            // Right after kana, with no space, as Japanese is written.
            ("notes.mdを見てから@managerに返事します。", Tone::Awareness),
            ("question: 確認後@managerの判断を待ちます", Tone::NeedsYou),
            ("Question: @manager capitalised", Tone::Awareness),
            // The owner's name in any case is the owner, as in delivery.
            ("question: @Manager which order?", Tone::NeedsYou),
            ("@MANAGER done: merged", Tone::Awareness),
            (
                "@manager could not deliver to dev-3: herdr has no agent named dev-3. Its \
                 posts stay on the board.",
                Tone::Awareness,
            ),
            // What is quoted is no mention (`mentioned`): a code span, a fenced block or a
            // `>` line, so the inbox and delivery agree.
            ("question: is `@manager red` the colour?", Tone::Plain),
            ("the colours: `@manager` red, `@lead` blue", Tone::Plain),
            ("question: see\n```\n@manager x\n```", Tone::Plain),
            ("> @manager dev-1 is waiting\nok", Tone::Plain),
            ("blocked: `@manager` quoted", Tone::Blocked),
            // A name that only starts like the owner's is someone else's.
            ("question: @managers all", Tone::Plain),
            ("question: no one is asked", Tone::Plain),
            ("copy, testing now", Tone::Plain),
            ("blocked: the covers are missing", Tone::Blocked),
            ("still blocked: no", Tone::Plain),
            ("Blocked: capitalised", Tone::Plain),
            (
                "@cat-lead #13 is in progress but dev-9 is gone",
                Tone::Plain,
            ),
        ] {
            assert_eq!(Tone::of(text), want, "{text}");
        }
    }

    /// A task's ticket comes from its tracker link, never its title.
    #[test]
    fn a_ticket_is_read_from_the_tracker_link() {
        for (link, want) in [
            (
                "https://gitlab.example.com/acme/bookshop/-/issues/15",
                "#15",
            ),
            (
                "https://gitlab.example.com/acme/bookshop/-/work_items/64",
                "#64",
            ),
            (
                "https://gitlab.example.com/acme/bookshop/-/merge_requests/19",
                "!19",
            ),
            (
                "https://gitlab.com/group/sub/repo/-/issues/12?foo=1#note_3",
                "#12",
            ),
            ("https://github.com/example/bookshop/issues/42", "#42"),
            ("https://github.com/example/bookshop/pull/58/", "#58"),
            // Deep links: what follows the number does not matter.
            ("https://github.com/example/bookshop/pull/58/files", "#58"),
            (
                "https://github.com/example/bookshop/pull/58/commits/abc123",
                "#58",
            ),
            (
                "https://github.com/example/bookshop/issues/42#issuecomment-9",
                "#42",
            ),
            ("https://www.github.com/example/bookshop/issues/12", "#12"),
            // A GitHub Enterprise host: GitHub's path, on a host of its own.
            ("https://github.acme.example/team/bookshop/pull/5", "#5"),
            (
                "https://gitlab.com/group/sub/repo/-/merge_requests/44/diffs?commit_id=1",
                "!44",
            ),
            ("https://gitlab.com/group/repo/-/work_items/3/designs", "#3"),
            // Another tracker, a forge page that is no ticket, a broken number, no URL.
            ("https://example.com/notes/7", "link"),
            ("https://github.com/example/bookshop/issues", "link"),
            ("https://github.com/example/bookshop/pulls/3", "link"),
            (
                "https://github.com/example/bookshop/tree/main/issues/9",
                "link",
            ),
            ("https://gitlab.com/group/repo/-/pipelines/9", "link"),
            ("https://gitlab.com/group/repo/-/tree/main/issues/9", "link"),
            ("https://gitlab.com/group/repo/-/pull/9", "link"),
            ("https://gitlab.example.com/x/-/issues/abc", "link"),
            ("not a url", "link"),
        ] {
            assert_eq!(Ticket::of(link).to_string(), want, "{link}");
        }
    }

    #[test]
    fn a_forge_ticket_is_in_the_project_its_repo_link_names_exactly() {
        for (ticket, repo) in [
            (
                "https://github.com/example/bookshop/issues/42",
                "https://github.com/example/bookshop",
            ),
            // Case, scheme, a trailing slash and `.git` on the repo link are the same project.
            (
                "https://github.com/Example/BookShop/pull/58/files",
                "http://github.com/example/bookshop.git/",
            ),
            (
                "https://gitlab.example.com/acme/shop/web/-/merge_requests/27",
                "https://gitlab.example.com/acme/shop/web",
            ),
            (
                "https://gitlab.example.com:8443/acme/web/-/work_items/3#note_1",
                "https://gitlab.example.com:8443/acme/web",
            ),
        ] {
            let project = ForgeProject::of_ticket(ticket);
            assert!(project.is_some(), "{ticket} is a forge ticket");
            assert_eq!(project, ForgeProject::of_repo(repo), "{ticket} in {repo}");
        }
        // Another project, host or port; a subgroup's project is not its parent group's.
        for (ticket, repo) in [
            (
                "https://github.com/example/bookshop/issues/42",
                "https://github.com/example/bookshop-web",
            ),
            (
                "https://github.com/example/bookshop/issues/42",
                "https://github.acme.example/example/bookshop",
            ),
            (
                "https://gitlab.example.com/acme/shop/web/-/issues/1",
                "https://gitlab.example.com/acme/shop",
            ),
            (
                "https://gitlab.example.com/acme/shop/-/issues/1",
                "https://gitlab.example.com/acme/shop/web",
            ),
            (
                "https://gitlab.example.com:8443/acme/web/-/issues/3",
                "https://gitlab.example.com/acme/web",
            ),
        ] {
            let project = ForgeProject::of_ticket(ticket);
            assert!(project.is_some(), "{ticket} is a forge ticket");
            assert_ne!(project, ForgeProject::of_repo(repo), "{ticket} in {repo}");
        }
        // No forge ticket, so in no project: another tracker, a repo's own page, a ticket
        // with no number, a GitLab group's work item.
        for link in [
            "https://acme.atlassian.net/browse/SHOP-12",
            "https://github.com/example/bookshop",
            "https://github.com/example/bookshop/tree/main/issues/9",
            "https://gitlab.example.com/x/-/issues/abc",
            "https://gitlab.example.com/groups/acme/-/work_items/7",
            "not a url",
        ] {
            assert_eq!(ForgeProject::of_ticket(link), None, "{link}");
        }
        // A group's work item is still a GitLab work item on the board, but in no project.
        let group_item = "https://gitlab.example.com/groups/acme/shop/-/work_items/7";
        assert_eq!(Ticket::of(group_item), Ticket::Issue(7));
        let project = ForgeProject::of_ticket("https://GitHub.com/Example/Bookshop/issues/42");
        assert_eq!(project.unwrap().to_string(), "github.com/example/bookshop");
        // A repo link to a page of the project goes on past its address; the project
        // itself, and another whose name only starts the same, do not.
        let docs = ForgeProject::of_ticket("https://github.com/example/docs/issues/4").unwrap();
        for (repo, past) in [
            ("https://github.com/example/docs/tree/main", true),
            ("https://github.com/example/docs", false),
            ("https://github.com/example/docs2", false),
            ("https://github.com/example", false),
        ] {
            let repo_project = ForgeProject::of_repo(repo).unwrap();
            assert_eq!(repo_project.goes_past(&docs), past, "{repo}");
        }
    }

    #[test]
    fn mentioned_reads_names_after_an_at_sign_but_not_email_addresses() {
        for (text, want) in [
            (
                "@qa-2 please test, then @Senior_2.",
                vec!["qa-2", "Senior_2"],
            ),
            (
                "dev-2's turn (@dev-2's), @manager",
                vec!["dev-2", "manager"],
            ),
            (
                "mail a@b.example or @@x or @ alone, @trail-",
                vec!["x", "trail"],
            ),
            ("no mention here", vec![]),
            ("@", vec![]),
            // Only an ASCII letter or digit makes an e-mail address; after any other
            // letter, such as kana, an ideograph or hangul, where words have no spaces
            // between them, it is a mention, as the signed design marks every `@manager`.
            ("2@x or Z@x but (@y", vec!["y"]),
            ("a@x", vec![]),
            ("notes.mdを見てから@managerに返事します。", vec!["manager"]),
            ("请@dev-2看一下，然后@qa。", vec!["dev-2", "qa"]),
            ("검토@senior 부탁", vec!["senior"]),
            ("é@x", vec!["x"]),
            // Quoted: code spans, fenced blocks and quoted lines.
            ("run `herdr agent prompt @dev-2` then @qa", vec!["qa"]),
            ("```\n@dev-1 x\n```\n@senior", vec!["senior"]),
            ("```sh\nherdr agent prompt `x` @dev-1\n```\n@qa", vec!["qa"]),
            // An unmatched backtick quotes only the rest of its line.
            ("it's `odd @dev-1\nso @senior please review", vec!["senior"]),
            (
                "> @manager dev-1 is waiting\n  > @dev-3\nok @dev-1",
                vec!["dev-1"],
            ),
        ] {
            assert_eq!(mentioned(text), want, "{text}");
        }
    }

    /// Code written without backticks names no one. A name followed straight by `(` and an
    /// ASCII character or nothing, or by `.` and an ASCII letter, digit or `_`, is code; a
    /// sentence ending on a name still counts, and so does a name before `.` or `(` and a
    /// character that is not ASCII.
    #[test]
    fn a_name_followed_by_a_member_or_a_call_is_code_not_a_mention() {
        let none: Vec<&str> = Vec::new();
        for (text, want) in [
            // Code in a post.
            ("- Empty: @cart.isEmpty() for a new reader.", none.clone()),
            ("@cart(\"x\") or @cart_check(1)", none.clone()),
            ("@step-1.2 runs first", none.clone()),
            // The name is checked where its run ends, before a trailing `-` or `_` is trimmed,
            // and `_` counts as a word character after the `.`.
            ("@qa_(x) and @qa-.foo", none.clone()),
            ("@obj._private", none.clone()),
            // A sentence that ends on a name, however it goes on.
            ("thanks @qa.", vec!["qa"]),
            (
                "thanks @qa. Next, @dev-2.\nThen @senior",
                vec!["qa", "dev-2", "senior"],
            ),
            ("ask @qa... or @dev-2.)", vec!["qa", "dev-2"]),
            ("@qa (see above)", vec!["qa"]),
            // Only ASCII after the `.` or `(` is code: Chinese and Japanese go straight on
            // after a name, as the signed design marks every `@manager`.
            ("请@dev-zh.谢谢", vec!["dev-zh"]),
            ("確認後@dev-ja.よろしく", vec!["dev-ja"]),
            ("请@dev-zh(明天)看一下", vec!["dev-zh"]),
            // A call with no argument, or one cut at the end of its line, is still code.
            ("@cart() then @cart(\n  x)", none.clone()),
        ] {
            assert_eq!(mentioned(text), want, "{text}");
        }
        // The inbox reads the same parser: code naming the owner raises nothing.
        assert_eq!(
            Tone::of("question: is @manager.isOwner() right?"),
            Tone::Plain
        );
        assert_eq!(Tone::of("question: is @manager(1) right?"), Tone::Plain);
        assert_eq!(Tone::of("question: 请@manager(明天)回复"), Tone::NeedsYou);
        assert_eq!(
            Tone::of("question: @manager. Is this right?"),
            Tone::NeedsYou
        );
    }
}

#[cfg(test)]
mod talk_tests {
    //! How a board names its threads, says where each stands, folds tower's own threads
    //! into their task's conversation and finds the questions waiting on the owner.
    use super::*;
    use crate::inbox::Inbox;

    fn msg(id: i64, author: &str, text: &str, at: i64) -> Message {
        Message {
            id,
            author: author.into(),
            text: text.into(),
            at,
        }
    }

    fn task(id: i64, title: &str, stage: Stage) -> Task {
        Task {
            id: TaskId(id),
            title: title.into(),
            stage,
            repo: None,
            subtasks: Vec::new(),
            details: TaskDetails::default(),
        }
    }

    /// A repo whose board has: T1 about task #1 (people), T2 about #1 (tower only), T3
    /// repo-wide, T4 about milestone M1, T5 about #2 (tower only, no home: #2 has no other
    /// thread). One question for the owner in T2, one in T3.
    fn board() -> Board {
        let lead = Lead {
            name: "Kids Lead".into(),
            session: "kids-lead".into(),
            colour: None,
        };
        let threads = vec![
            Thread::whole(
                ThreadId(1),
                "Task #1 team".into(),
                Some(TaskId(1)),
                None,
                vec![
                    msg(10, "kids-lead", "Picked up #1.", 100),
                    msg(
                        11,
                        "dev-1",
                        "Status: building the picture-book shelf\nmore detail",
                        300,
                    ),
                    msg(12, "qa", "copy", 400),
                ],
            ),
            Thread::whole(
                ThreadId(2),
                "#1 watch".into(),
                Some(TaskId(1)),
                None,
                vec![
                    msg(20, "system", "nudged dev-1: no post for 20 min", 200),
                    msg(21, "system", "question: @manager dev-1 is on a prompt", 350),
                ],
            ),
            Thread::whole(
                ThreadId(3),
                "Merge order".into(),
                None,
                None,
                vec![msg(
                    30,
                    "kids-lead",
                    "question: @manager merge !1 first?",
                    500,
                )],
            ),
            Thread::whole(
                ThreadId(4),
                "Phase 1".into(),
                None,
                None,
                vec![msg(40, "kids-lead", "Phase 1 plan", 50)],
            ),
            Thread::whole(
                ThreadId(5),
                "#2 watch".into(),
                Some(TaskId(2)),
                None,
                vec![msg(50, "system", "nudged dev-2: no post", 60)],
            ),
        ];
        let mut threads = threads;
        threads[3].milestone = Some(MilestoneId(1));
        let mut t1 = task(1, "New book shelf", Stage::InProgress);
        t1.details.assignee = Some("dev-1".into());
        let milestone = Milestone {
            id: MilestoneId(1),
            title: "MVP".into(),
            goal: "g".into(),
            repo: Some(RepoId(1)),
            lead: Some(lead.clone()),
            tasks: vec![t1, task(2, "Reviews", Stage::Waiting)],
            marked_done: false,
            done_at: None,
            folded: false,
            threads: threads.clone(),
            details: Default::default(),
        };
        let repo = Repo {
            id: RepoId(1),
            path: Some("/src/kids".into()),
            link: None,
            lead: Some(lead),
            folded: false,
            done_folded: true,
            threads,
        };
        let mentions: Vec<Mention> = (repo.threads.iter())
            .flat_map(|t| {
                (t.messages.iter())
                    .filter(|m| !mentioned(&m.text).is_empty())
                    .map(|m| Mention {
                        thread: t.id,
                        message: m.clone(),
                        read: false,
                    })
            })
            .collect();
        let inbox = Inbox::of(std::slice::from_ref(&repo), mentions);
        Board {
            repos: vec![repo],
            milestones: vec![milestone],
            standalone: Vec::new(),
            inbox,
        }
    }

    #[test]
    fn a_tower_only_thread_joins_its_tasks_conversation_and_leaves_the_list() {
        let b = board();
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let ids: Vec<i64> = talk.ordered(None).iter().map(|t| t.id.0).collect();
        // T2 folds into T1; T5 has no thread with people for its task, so it stays.
        assert_eq!(ids.len(), 4, "{ids:?}");
        assert!(!ids.contains(&2));
        assert!(ids.contains(&5));
        assert_eq!(talk.home_of(ThreadId(2)), ThreadId(1));
        assert_eq!(talk.home_of(ThreadId(3)), ThreadId(3));
        let t1 = talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
        let conv: Vec<i64> = talk.conversation(t1).iter().map(|m| m.id).collect();
        // In time order, the folded messages between T1's own.
        assert_eq!(conv, [10, 20, 11, 21, 12]);
        // A thread with nothing folded into it is its own messages, unchanged.
        let t3 = talk.threads.iter().find(|t| t.id == ThreadId(3)).unwrap();
        assert!(matches!(
            talk.conversation(t3),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn questions_for_the_owner_are_the_boards_and_mark_the_thread_that_shows_them() {
        let b = board();
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let needs: Vec<i64> = talk.needs.iter().map(|a| a.message.id).collect();
        // Newest first: T3's, then the one folded from T2 into T1.
        assert_eq!(needs, [30, 21]);
        let t = |id| *talk.threads.iter().find(|t| t.id == ThreadId(id)).unwrap();
        assert!(
            talk.needs_you(t(1)),
            "a folded question marks its home thread"
        );
        assert!(talk.needs_you(t(3)));
        assert!(!talk.needs_you(t(4)));
        // A milestone's part of the board carries only its threads' questions.
        let m = Talk::of_milestone(&b, &b.milestones[0]);
        let needs: Vec<i64> = m.needs.iter().map(|a| a.message.id).collect();
        assert_eq!(needs, [21]);
    }

    #[test]
    fn a_thread_is_named_by_its_task_with_its_stage_and_who_and_its_ids() {
        let b = board();
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let t = |id| *talk.threads.iter().find(|t| t.id == ThreadId(id)).unwrap();
        assert_eq!(talk.name(t(1)), "New book shelf");
        assert_eq!(talk.state(t(1)), "in progress · dev-1");
        assert_eq!(talk.ids(t(1)), "T1");
        // No task: its own title, what it is about, its id.
        assert_eq!(talk.name(t(3)), "Merge order");
        assert_eq!(talk.state(t(3)), "repo-wide");
        assert_eq!(talk.ids(t(3)), "T3");
        assert_eq!(talk.state(t(4)), "on M1");
        // A task the board does not carry is still named by its id.
        let mut gone = t(5).clone();
        gone.task = Some(TaskId(99));
        assert_eq!(talk.name(&gone), "#2 watch");
        assert_eq!(talk.state(&gone), "on #99");
        assert_eq!(talk.ids(&gone), "T5");
    }

    #[test]
    fn where_a_thread_stands_is_its_tasks_doing_line_else_its_latest_word() {
        let mut b = board();
        // No progress post yet: the newest message by a person that is no ack, its first
        // line, without "Status:".
        {
            let talk = Talk::of_repo(&b, &b.repos[0]);
            let t1 = *talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
            let want = (
                "dev-1".to_string(),
                "building the picture-book shelf".to_string(),
                Tone::Plain,
            );
            assert_eq!(talk.stands(t1), Some(want));
            // Only tower's messages: the newest of them.
            let t5 = *talk.threads.iter().find(|t| t.id == ThreadId(5)).unwrap();
            let want = (
                "system".to_string(),
                "nudged dev-2: no post".to_string(),
                Tone::Plain,
            );
            assert_eq!(talk.stands(t5), Some(want));
        }
        // The task's doing line once there is one (one source with its detail).
        b.milestones[0].tasks[0].details.doing =
            Some(msg(99, "dev-1", "Status: picture-book shelf done\nx", 600));
        {
            let talk = Talk::of_repo(&b, &b.repos[0]);
            let t1 = *talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
            let want = (
                "dev-1".to_string(),
                "picture-book shelf done".to_string(),
                Tone::Plain,
            );
            assert_eq!(talk.stands(t1), Some(want));
        }
        // Its tone is the whole message's: a mention of the owner on a later line counts,
        // as in its bubble.
        b.milestones[0].tasks[0].details.doing = Some(msg(
            98,
            "dev-1",
            "decision: the shelf takes 12 books\n@manager for your awareness",
            700,
        ));
        {
            let talk = Talk::of_repo(&b, &b.repos[0]);
            let t1 = *talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
            let want = (
                "dev-1".to_string(),
                "decision: the shelf takes 12 books".to_string(),
                Tone::Awareness,
            );
            assert_eq!(talk.stands(t1), Some(want));
        }
        // The blocked reason wins, as in its detail.
        b.milestones[0].tasks[0].details.blocked = Some("no cover images".into());
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let t1 = *talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
        let want = (
            "dev-1".to_string(),
            "blocked: no cover images".to_string(),
            Tone::Blocked,
        );
        assert_eq!(talk.stands(t1), Some(want));
    }

    #[test]
    fn a_status_prefix_goes_but_a_word_starting_status_stays() {
        assert_eq!(without_status("Status: done"), "done");
        assert_eq!(without_status("status update: done"), "done");
        assert_eq!(without_status("Statuses are fine"), "Statuses are fine");
        assert_eq!(without_status("Statuses: green"), "Statuses: green");
        assert_eq!(without_status("STATUS:   done"), "done");
        // Only the prefix: nothing left to say.
        assert_eq!(without_status("Status: "), "");
        assert_eq!(
            without_status("Status of the shelf is: half done"),
            "Status of the shelf is: half done"
        );
    }
}

#[cfg(test)]
mod fold_tests {
    //! When a thread of tower's own joins its task's conversation, and when it stays apart.
    use super::*;
    use crate::inbox::Inbox;

    fn msg(id: i64, author: &str, text: &str, at: i64) -> Message {
        Message {
            id,
            author: author.into(),
            text: text.into(),
            at,
        }
    }

    fn task(id: i64) -> Task {
        Task {
            id: TaskId(id),
            title: format!("task {id}"),
            stage: Stage::InProgress,
            repo: None,
            subtasks: Vec::new(),
            details: TaskDetails::default(),
        }
    }

    /// A board of one repo (`repo` false: in no repo) holding `threads`, all about
    /// milestone M1's task #1 unless set otherwise, with its inbox read from them.
    fn board_of(threads: Vec<Thread>, repo: bool) -> Board {
        let lead = Lead {
            name: "Kids Lead".into(),
            session: "kids-lead".into(),
            colour: None,
        };
        let r = repo.then_some(RepoId(1));
        let milestone = Milestone {
            id: MilestoneId(1),
            title: "MVP".into(),
            goal: "g".into(),
            repo: r,
            lead: Some(lead.clone()),
            tasks: vec![task(1)],
            marked_done: false,
            done_at: None,
            folded: false,
            threads: threads.clone(),
            details: Default::default(),
        };
        let repos = if repo {
            vec![Repo {
                id: RepoId(1),
                path: Some("/src/kids".into()),
                link: None,
                lead: Some(lead),
                folded: false,
                done_folded: true,
                threads: threads.clone(),
            }]
        } else {
            Vec::new()
        };
        let mentions: Vec<Mention> = (threads.iter())
            .flat_map(|t| {
                (t.messages.iter())
                    .filter(|m| !mentioned(&m.text).is_empty())
                    .map(|m| Mention {
                        thread: t.id,
                        message: m.clone(),
                        read: false,
                    })
            })
            .collect();
        let inbox = Inbox::of(&repos, mentions);
        Board {
            repos,
            milestones: vec![milestone],
            standalone: Vec::new(),
            inbox,
        }
    }

    fn thread(id: i64, msgs: Vec<Message>) -> Thread {
        Thread::whole(ThreadId(id), format!("T{id}"), Some(TaskId(1)), None, msgs)
    }

    fn ids(talk: &Talk) -> Vec<i64> {
        talk.ordered(None).iter().map(|t| t.id.0).collect()
    }

    #[test]
    fn a_people_thread_whose_loaded_window_is_all_nudges_stays_a_thread() {
        let people = thread(
            1,
            vec![
                msg(10, "dev-1", "on it", 100),
                msg(11, "system", "nudged dev-1: no post", 200),
            ],
        );
        let tower = thread(2, vec![msg(20, "system", "nudged dev-1: no post", 300)]);
        // The board loads only T1's latest message, a nudge; its authors are its whole
        // history, so it is still a people thread, and T2 still folds into it.
        let mut loaded = people.clone();
        loaded.messages.drain(..1);
        assert!(loaded.messages.iter().all(|m| m.author == "system"));
        let b = board_of(vec![loaded, tower], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        assert_eq!(ids(&talk), [1]);
        assert_eq!(talk.home_of(ThreadId(2)), ThreadId(1));
        // A loaded window of T2 that shows no person is judged by its authors too: "System"
        // or "sys tem" are tower, as the board reserves them.
        let spaced = thread(3, vec![msg(30, "Sys tem", "nudged dev-1", 400)]);
        let b = board_of(vec![people.clone(), spaced], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        assert_eq!(talk.home_of(ThreadId(3)), ThreadId(1));
    }

    #[test]
    fn a_tower_thread_never_folds_into_a_resolved_one_and_its_question_stays_answerable() {
        let mut done = thread(1, vec![msg(10, "dev-1", "done", 100)]);
        done.resolved = Some(Resolution {
            by: "kids-lead".into(),
            at: 150,
        });
        let tower = thread(
            2,
            vec![msg(
                20,
                "system",
                "question: @manager dev-1 is on a prompt",
                200,
            )],
        );
        let b = board_of(vec![done, tower], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        // T2 stays open in the list, first, with its question; T1 has none.
        assert_eq!(ids(&talk), [2, 1]);
        assert_eq!(talk.home_of(ThreadId(2)), ThreadId(2));
        let t = |id| *talk.threads.iter().find(|t| t.id == ThreadId(id)).unwrap();
        assert!(talk.needs_you(t(2)));
        assert!(!talk.needs_you(t(1)));
    }

    #[test]
    fn two_people_threads_on_one_task_take_the_tower_thread_into_the_first_open_one() {
        let first = thread(1, vec![msg(10, "dev-1", "a", 100)]);
        let second = thread(2, vec![msg(20, "qa", "b", 200)]);
        let tower = thread(3, vec![msg(30, "system", "nudged dev-1", 300)]);
        let b = board_of(vec![first, second, tower], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        assert_eq!(talk.home_of(ThreadId(3)), ThreadId(1));
        assert_eq!(talk.joined(ThreadId(1)), [ThreadId(1), ThreadId(3)]);
        assert_eq!(talk.joined(ThreadId(2)), [ThreadId(2)]);
    }

    #[test]
    fn with_the_first_people_thread_resolved_the_tower_thread_folds_into_the_second() {
        let mut first = thread(1, vec![msg(10, "dev-1", "a", 100)]);
        first.resolved = Some(Resolution {
            by: "kids-lead".into(),
            at: 120,
        });
        let second = thread(2, vec![msg(20, "qa", "b", 200)]);
        let tower = thread(3, vec![msg(30, "system", "nudged dev-1", 300)]);
        let b = board_of(vec![first, second, tower], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        assert_eq!(talk.home_of(ThreadId(3)), ThreadId(2));
        assert_eq!(talk.joined(ThreadId(1)), [ThreadId(1)]);
    }

    #[test]
    fn a_folded_thread_counts_ages_and_orders_its_home() {
        let old = thread(1, vec![msg(10, "dev-1", "a", 100)]);
        let other = Thread::whole(
            ThreadId(2),
            "repo-wide".into(),
            None,
            None,
            vec![msg(20, "kids-lead", "b", 200)],
        );
        let tower = thread(3, vec![msg(30, "system", "nudged dev-1", 300)]);
        let b = board_of(vec![old, other, tower], true);
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let t1 = *talk.threads.iter().find(|t| t.id == ThreadId(1)).unwrap();
        assert_eq!(talk.latest(t1), Some(300));
        assert_eq!(talk.total(t1), 2);
        // The newer folded message puts T1 ahead of T2.
        assert_eq!(ids(&talk), [1, 2]);
    }

    #[test]
    fn the_doing_line_heads_the_tasks_first_open_thread_and_the_others_speak_for_themselves() {
        let first = thread(
            1,
            vec![msg(10, "dev-1", "Status: on the picture-book shelf", 100)],
        );
        let second = thread(
            2,
            vec![msg(
                20,
                "kids-lead",
                "decision: the shelf shows 12 books, not 10. @manager for your awareness",
                200,
            )],
        );
        let mut b = board_of(vec![first, second], true);
        b.milestones[0].tasks[0].details.doing =
            Some(msg(99, "dev-1", "picture-book shelf done", 300));
        let talk = Talk::of_repo(&b, &b.repos[0]);
        let t = |id| *talk.threads.iter().find(|t| t.id == ThreadId(id)).unwrap();
        assert_eq!(
            talk.stands(t(1)),
            Some((
                "dev-1".into(),
                "picture-book shelf done".into(),
                Tone::Plain
            ))
        );
        let own =
            "decision: the shelf shows 12 books, not 10. @manager for your awareness".to_string();
        assert_eq!(
            talk.stands(t(2)),
            Some(("kids-lead".into(), own, Tone::Awareness))
        );
        // With the first resolved, the second open one is the task's first open thread.
        let mut done = t(1).clone();
        done.resolved = Some(Resolution {
            by: "kids-lead".into(),
            at: 150,
        });
        let mut b2 = b.clone();
        b2.repos[0].threads[0] = done;
        let talk = Talk::of_repo(&b2, &b2.repos[0]);
        let t2 = *talk.threads.iter().find(|t| t.id == ThreadId(2)).unwrap();
        assert_eq!(
            talk.stands(t2),
            Some((
                "dev-1".into(),
                "picture-book shelf done".into(),
                Tone::Plain
            ))
        );
    }

    #[test]
    fn a_milestones_part_of_the_board_and_a_milestone_in_no_repo_fold_the_same_way() {
        let people = thread(1, vec![msg(10, "dev-1", "a", 100)]);
        let tower = thread(
            2,
            vec![msg(
                20,
                "system",
                "question: @manager dev-1 is on a prompt",
                200,
            )],
        );
        let b = board_of(vec![people.clone(), tower.clone()], true);
        let m = Talk::of_milestone(&b, &b.milestones[0]);
        assert_eq!(m.home_of(ThreadId(2)), ThreadId(1));
        assert_eq!(m.needs.len(), 1);
        // In no repo: the fold holds; there are no inbox items for a board in no repo.
        let b = board_of(vec![people, tower], false);
        let m = Talk::of_milestone(&b, &b.milestones[0]);
        assert_eq!(m.home_of(ThreadId(2)), ThreadId(1));
        assert!(m.others.is_empty());
    }
}
