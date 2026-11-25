//! The dashboard's state: what is selected, the views open over the tree, how far each pane
//! is scrolled and what the owner is writing, and how a key or mouse input changes it. No
//! terminal and no styling.

use std::collections::{BTreeMap, BTreeSet};

use ratatui::layout::Rect;

use super::full_board::{self, Anchor, BACK_COLUMNS, BoardLayout, ResolvedFold, Stop};
use super::inbox::{BACK_COLUMNS as INBOX_BACK, InboxLayout};
use super::layout::{Layout, clamp_scroll, contains, layout};
use super::tree::{Fold, FoldClick};
use super::{Mode, Shown, View};
use unicode_segmentation::UnicodeSegmentation;

use super::control::Control;
use crate::board::{
    Board, MAX_LINE_CHARS, Milestone, MilestoneId, Repo, RepoId, Talk, Task, TaskId, ThreadId,
    hidden,
};
use crate::inbox::Inbox;

/// Lines a mouse wheel notch scrolls.
const WHEEL_LINES: usize = 3;

/// One selectable entry in the tree: a repo (its two lines), a milestone or a task, under
/// a milestone or standalone, or a repo's Done group (a milestone in it is selected as a
/// milestone, by its Done line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Repo(RepoId),
    Milestone(MilestoneId),
    Task(TaskId),
    Done(RepoId),
}

/// Every item in tree order, `shown` only (the tree's rows: none under a folded repo,
/// milestone or Done group) or all: each repo with its active milestones and their tasks,
/// then its standalone tasks, then its Done group and the milestones in it (never their
/// tasks: a Done milestone is shown read-only); then the milestones and standalone tasks in
/// no repo.
fn tree_order(board: &Board, shown: bool) -> Vec<Item> {
    let mut out = Vec::new();
    let group = |out: &mut Vec<Item>, repo: Option<RepoId>| {
        for m in board.active_of(repo) {
            out.push(Item::Milestone(m.id));
            if !(shown && m.folded) {
                out.extend(m.tasks.iter().map(|t| Item::Task(t.id)));
            }
        }
        out.extend(board.standalone_of(repo).map(|t| Item::Task(t.id)));
    };
    for r in &board.repos {
        out.push(Item::Repo(r.id));
        if shown && r.folded {
            continue;
        }
        group(&mut out, Some(r.id));
        let done = board.done_of(r.id);
        if !done.is_empty() {
            out.push(Item::Done(r.id));
            if !(shown && r.done_folded) {
                out.extend(done.iter().map(|m| Item::Milestone(m.id)));
            }
        }
    }
    group(&mut out, None);
    out
}

/// The milestone an item is when it is in its repo's Done group, with that repo.
fn done_milestone(board: &Board, item: Item) -> Option<(&Repo, &Milestone)> {
    let m = milestone_of(board, item).filter(|_| matches!(item, Item::Milestone(_)))?;
    let r = board.repo(m.repo?)?;
    board
        .done_of(r.id)
        .iter()
        .any(|d| d.id == m.id)
        .then_some((r, m))
}

/// The active milestones, those the tree shows with their tasks: every milestone but those
/// in a Done group.
fn active_milestones(board: &Board) -> Vec<&Milestone> {
    (board.repos.iter().map(|r| Some(r.id)))
        .chain([None])
        .flat_map(|r| board.active_of(r))
        .collect()
}

/// Every item in tree order.
fn items(board: &Board) -> Vec<Item> {
    tree_order(board, false)
}

/// The items the tree shows, in its order: every item but those under a folded repo or
/// milestone. Up and down move over these.
fn shown_items(board: &Board) -> Vec<Item> {
    tree_order(board, true)
}

/// The repo an item is, or belongs to, if it is on the board: a milestone's, or a task's
/// milestone's, or a standalone task's own.
pub(crate) fn repo_of(board: &Board, item: Item) -> Option<&Repo> {
    let id = match item {
        Item::Repo(r) | Item::Done(r) => Some(r),
        Item::Milestone(_) => milestone_of(board, item).and_then(|m| m.repo),
        Item::Task(_) => match milestone_of(board, item) {
            Some(m) => m.repo,
            None => standalone_of(board, item).and_then(|t| t.repo),
        },
    };
    board.repo(id?)
}

/// The folded row an item is hidden under, if it is: its repo when folded, else a task's
/// milestone when folded, or a Done milestone's Done group when folded.
fn folded_under(board: &Board, item: Item) -> Option<Item> {
    if !matches!(item, Item::Repo(_))
        && let Some(r) = repo_of(board, item).filter(|r| r.folded)
    {
        return Some(Item::Repo(r.id));
    }
    match item {
        Item::Task(_) => milestone_of(board, item)
            .filter(|m| m.folded)
            .map(|m| Item::Milestone(m.id)),
        Item::Milestone(_) => done_milestone(board, item)
            .filter(|(r, _)| r.done_folded)
            .map(|(r, _)| Item::Done(r.id)),
        Item::Repo(_) | Item::Done(_) => None,
    }
}

/// The standalone task an item is, if it is one.
pub(crate) fn standalone_of(board: &Board, item: Item) -> Option<&Task> {
    match item {
        Item::Task(id) => board.standalone.iter().find(|t| t.id == id),
        Item::Repo(_) | Item::Milestone(_) | Item::Done(_) => None,
    }
}

/// What the right pane is about for `item`: the repo, its Done group, its milestone, or a
/// standalone task itself. `None` for an item no longer on the board.
pub(crate) fn pane_of(board: &Board, item: Item) -> Option<Item> {
    match item {
        Item::Repo(r) => return board.repo(r).map(|r| Item::Repo(r.id)),
        Item::Done(r) => return board.repo(r).map(|r| Item::Done(r.id)),
        Item::Milestone(_) | Item::Task(_) => {}
    }
    match milestone_of(board, item) {
        Some(m) => Some(Item::Milestone(m.id)),
        None => standalone_of(board, item).map(|t| Item::Task(t.id)),
    }
}

/// The message board the full board shows for `item`: its repo's whole board.
pub(crate) fn board_of(board: &Board, item: Item) -> Option<Talk<'_>> {
    repo_of(board, item).map(|r| Talk::of_repo(board, r))
}

/// What the owner asked for that the dashboard cannot do itself, since it never writes the
/// board; the edge runs it as a `tower` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Post `text` to `thread` as `manager`, which the dashboard writes itself through the
    /// store (see `writer`), since the CLI refuses `manager`.
    Post { thread: ThreadId, text: String },
    /// Mark the awareness message `id` read, also through the store; `item` names it in
    /// words for the notice (`inbox::alert_name`).
    MarkRead { id: i64, item: String },
    /// Clear the need-you message `id`, the owner's `r` on it: marked read through the store
    /// as `MarkRead` is, and said as cleared, naming `item`.
    Clear { id: i64, item: String },
    /// Fold `repos`, the task lists of `milestones` and the Done groups of `done_groups`
    /// in the tree, or unfold them, also through the store, so every dashboard shows the
    /// same folds.
    Fold {
        repos: Vec<RepoId>,
        milestones: Vec<MilestoneId>,
        done_groups: Vec<RepoId>,
        folded: bool,
    },
    /// Start a lead on the owner's goal, one line: a milestone (in `repo`, when given) and
    /// Claude leading it in a new place the terminal module opens, through the store and
    /// that module (see `writer`).
    StartLead { line: String, repo: Option<RepoId> },
    /// Close the dashboard: `q` outside the composer and the new lead's input, or, in the
    /// popup, Esc with nothing to go back from.
    Quit,
    /// Open a task's ticket, its tracker link, in the browser: `o` on the selected
    /// task, or a click on its ticket in the tree.
    Open(String),
}

/// The milestone an item is, or the one its task sits under.
pub(crate) fn milestone_of(board: &Board, item: Item) -> Option<&Milestone> {
    board.milestones.iter().find(|m| match item {
        Item::Repo(_) | Item::Done(_) => false,
        Item::Milestone(id) => m.id == id,
        Item::Task(id) => m.tasks.iter().any(|t| t.id == id),
    })
}

/// What a view in the right pane shows. Each shows the selection, never its own copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Overview,
    /// The selected repo's detail.
    RepoDetail,
    /// The selected milestone's detail, or the selected task's milestone's.
    MilestoneDetail,
    /// The selected task's detail.
    TaskDetail,
    /// The full message board of the selection's repo, across the body.
    Board,
    /// The owner's inbox of `@manager` messages, across the body.
    Inbox,
}

/// A view open in the right pane, with its own scroll, and the selection to go back to
/// when it closes, if opening it changed the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenView {
    kind: ViewKind,
    scroll: usize,
    restore: Option<Item>,
    board: BoardView,
    /// The inbox item selected, by message id and its place among the items, while this
    /// is the inbox.
    inbox: Option<(i64, usize)>,
}

/// The full board's own state: the thread it shows, where its conversation is scrolled back
/// to (no anchor: at the latest), how far its channel list is scrolled, and the message
/// selected in the chat (by id) while the keys are in it, not in the thread list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct BoardView {
    thread: Option<ThreadId>,
    /// The RESOLVED group's fold and whether the selection is on its row.
    fold: ResolvedFold,
    /// The stop selected in the list and its first and last lines there at the last
    /// refresh, to see the list move under it.
    at: Option<(Stop, usize, usize)>,
    anchor: Option<Anchor>,
    list_scroll: usize,
    message: Option<i64>,
    /// The question the owner is answering, picked from the "Needs you" strip: the
    /// composer says so until a post goes through, the board closes (Esc), or another
    /// thread shows. A click elsewhere or a refused post keeps it.
    reply: Option<i64>,
}

impl OpenView {
    fn new(kind: ViewKind) -> OpenView {
        OpenView {
            kind,
            scroll: 0,
            restore: None,
            board: BoardView::default(),
            inbox: None,
        }
    }
}

/// What the owner has selected, the views open in the right pane (the top one shows; Esc
/// closes it, back to the view and selection it was opened from, down to the overview,
/// which always stays) and how far the tree is scrolled. The overview is at the bottom of
/// the stack, once, and detail views are above it. In a pane too narrow for the tree and
/// the overview side by side, the overview opens across the pane in the tree's place; that
/// is a flag, not a second overview, so every Esc closes something that shows. It outlives
/// refreshes: the selection is kept by id, so new rows above it do not move it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dash {
    pub selected: Option<Item>,
    pub tree_scroll: usize,
    views: Vec<OpenView>,
    across: bool,
    /// How the last `tower` command the dashboard ran went, shown in the keys line until the
    /// next input: its message, and whether it failed.
    pub notice: Option<(String, bool)>,
    /// "starting a lead…", "clearing…" or "marking read…" while a request runs; the edge
    /// clears it when the request returns. No second request starts meanwhile.
    pub handing: Option<String>,
    /// What the owner wrote to each thread and has not posted, kept across Esc. The keys
    /// write to the open thread the full board shows (`BoardLayout::writes_to`).
    drafts: BTreeMap<ThreadId, Draft>,
    /// The thread a post is running to, until `posted`. The draft stays as it is meanwhile
    /// and no second post starts.
    pub posting: Option<ThreadId>,
    /// The goal the owner is typing for a new lead, while that input is open (`n`): it
    /// takes every input but Ctrl-C until Enter starts the lead or Esc cancels.
    new_lead: Option<String>,
    /// The repo the new lead starts on, picked when its input opened: the selection's
    /// repo (which has no lead, or `n` was refused); `None` starts it in no repo yet, for a
    /// selection in no repo or none.
    new_lead_repo: Option<RepoId>,
    /// Where it runs: a pane of its own or a popup.
    pub mode: Mode,
    /// The repo or milestone being unfolded because the owner picked something folded in it
    /// (by its card or a line in a view), until a refresh shows it unfolded or the edge
    /// says the write was refused: meanwhile a refresh still showing it folded leaves the
    /// pick selected. Should the other dashboard fold it again before that refresh, this
    /// stays set until it next shows unfolded, and that fold alone does not move the
    /// selection.
    pub unfolding: Option<Fold>,
    /// The repos and milestones folded at the last refresh, to see when the folds change.
    folds_seen: Vec<Fold>,
    /// The tree changed under the selection, or a fold moved it: the next `fit`, which
    /// knows the pane's size, scrolls the selection into view.
    reveal_due: bool,
    /// The thread the full board showed when `opened` last looked: its items are read
    /// once, as it opens, never as later ones arrive.
    opened_thread: Option<ThreadId>,
    /// The messages the owner opened in full in the chat ("read more", a delivery
    /// failure's details), by id, until folded again or their thread leaves the board.
    open_messages: BTreeSet<i64>,
    /// The selections whose overview and detail boards show their resolved threads: their
    /// RESOLVED row unfolded. Every board's is folded until a click unfolds it.
    unfolded_boards: Vec<Item>,
    /// The thread each open message is in, so it is forgotten only when that thread is
    /// gone: a refresh carries only each thread's latest messages, so a message missing
    /// from it is no sign it is gone.
    open_in: BTreeMap<i64, ThreadId>,
    /// A question picked from a "Needs you" strip: the next `fit`, which knows the full
    /// board's layout, selects it in view.
    reply_due: bool,
    /// The list under the selection changed (the full board's RESOLVED group folded or
    /// unfolded, an inbox item left, the inbox shows again): the next `fit`, which knows the
    /// new list, scrolls the selection into view in it.
    list_due: bool,
    /// The full board's selection moved in its list (the thread shown resolved, or one
    /// above it resolved or opened), from these first and last lines: the next `fit`
    /// keeps it in view if any of them was in view there, and leaves a list scrolled away
    /// from it as it was.
    list_moved: Option<(usize, usize)>,
    /// A message just opened in full in the full board: the next `fit`, which knows its new
    /// layout, scrolls what opened into view.
    opened_due: Option<i64>,
    /// The clock to the minute (seconds since the epoch), which the full board's hour
    /// separators are reckoned at; the edge sets it before each frame and input.
    minute: Option<i64>,
}

/// What the owner wrote to a thread and has not posted, and where its cursor is: a byte
/// offset into it, on the edge of a grapheme cluster.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Draft {
    text: String,
    at: usize,
}

impl Draft {
    /// Where each grapheme cluster starts, then the end.
    fn edges(&self) -> impl Iterator<Item = usize> + '_ {
        (self.text.grapheme_indices(true).map(|(i, _)| i)).chain([self.text.len()])
    }

    /// The edge before the cursor, or the start.
    fn before(&self) -> usize {
        self.edges()
            .take_while(|i| *i < self.at)
            .last()
            .unwrap_or(0)
    }

    /// Puts `s` in at the cursor, the cursor after it. What went in may join the cluster
    /// after it (a combining mark, half of a flag): the cursor then goes past that cluster.
    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.at, s);
        let after = self.at + s.len();
        let at = self.edges().find(|i| *i >= after);
        self.at = at.unwrap_or(self.text.len());
    }

    /// Takes the whole grapheme cluster before the cursor away: an accented letter or an
    /// emoji at once.
    fn back(&mut self) {
        let from = self.before();
        self.text.replace_range(from..self.at, "");
        self.at = from;
    }

    /// Moves the cursor one grapheme cluster right, or left.
    fn step(&mut self, right: bool) {
        let at = if right {
            self.edges().find(|i| *i > self.at)
        } else {
            Some(self.before())
        };
        self.at = at.unwrap_or(self.at);
    }
}

/// What typing `c` puts into a draft: only what tower takes. A line break stays, a tab
/// becomes four spaces as tower makes it, and any other hidden character is dropped.
fn typed(c: char) -> Option<String> {
    match c {
        '\n' => Some("\n".to_string()),
        '\t' => Some("    ".to_string()),
        c if hidden(&c) => None,
        c => Some(c.to_string()),
    }
}

impl Default for Dash {
    fn default() -> Self {
        Dash::new(None)
    }
}

/// One input the dashboard acts on, in terminal cells for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Up,
    Down,
    /// Folds the selected milestone, or the selected task's, or unfolds it when folded.
    Left,
    /// The same as `Left`.
    Right,
    PageUp,
    PageDown,
    Enter,
    /// Alt+Enter: a line break in the draft while writing; anywhere else, Enter.
    NewLine,
    Esc,
    /// A character key other than the ones that quit: `b` opens the full message board,
    /// `i` the inbox, `n` the new lead's input, `r` marks an inbox item read, `z` folds or
    /// unfolds every milestone. While writing, a character of the draft, `q` and `c`
    /// included, and each character of a paste.
    Char(char),
    /// Takes the grapheme cluster before the cursor away while writing.
    Backspace,
    Click {
        x: u16,
        y: u16,
    },
    Wheel {
        x: u16,
        y: u16,
        down: bool,
    },
}

impl Dash {
    /// A dashboard showing the overview, with `selected` selected.
    pub fn new(selected: Option<Item>) -> Dash {
        Dash {
            selected,
            tree_scroll: 0,
            views: vec![OpenView::new(ViewKind::Overview)],
            across: false,
            notice: None,
            handing: None,
            drafts: BTreeMap::new(),
            posting: None,
            new_lead: None,
            new_lead_repo: None,
            mode: Mode::Space,
            unfolding: None,
            folds_seen: Vec::new(),
            reveal_due: false,
            opened_thread: None,
            open_messages: BTreeSet::new(),
            unfolded_boards: Vec::new(),
            open_in: BTreeMap::new(),
            reply_due: false,
            list_due: false,
            list_moved: None,
            opened_due: None,
            minute: None,
        }
    }

    /// The goal typed so far for a new lead, while its input is open.
    pub fn new_lead(&self) -> Option<&str> {
        self.new_lead.as_deref()
    }

    /// The repo the new lead starts on, while its input is open; `None` for no repo yet.
    pub fn new_lead_repo(&self) -> Option<RepoId> {
        self.new_lead_repo
    }

    /// Whether a key is text now in a terminal of size `area` showing `view` (the composer
    /// or the new lead's input), so `q` is typed, not quitting.
    pub fn typing(&self, view: &View, area: Rect) -> bool {
        self.new_lead.is_some() || self.writing(view, area)
    }

    /// Whether the owner is writing in the composer, in a terminal of size `area` showing
    /// `view`: whenever the full board shows an open thread. Every key that is text
    /// then goes into its draft, `q` included.
    pub fn writing(&self, view: &View, area: Rect) -> bool {
        (layout(view, self, area).board).is_some_and(|b| b.writes_to().is_some())
    }

    /// What the owner wrote to `thread` and has not posted yet.
    pub fn draft(&self, thread: ThreadId) -> &str {
        self.drafts.get(&thread).map_or("", |d| d.text.as_str())
    }

    /// Where the cursor is in `thread`'s draft: a byte offset into it.
    pub fn draft_at(&self, thread: ThreadId) -> usize {
        self.drafts.get(&thread).map_or(0, |d| d.at)
    }

    /// Takes in a paste while writing: its text goes into the draft at the cursor as if
    /// typed, its line ends made plain, in one go (not a key at a time, which would lay the
    /// board out again for each). While a post runs the draft cannot change, so a notice
    /// says so rather than the paste vanishing.
    pub fn paste(&mut self, view: &View, area: Rect, text: &str) {
        if self.new_lead.is_some() {
            // One line: line breaks become spaces.
            for c in text.chars() {
                let c = if matches!(c, '\r' | '\n') { ' ' } else { c };
                self.handle(view, area, Input::Char(c));
            }
            return;
        }
        self.notice = None;
        self.fit(view, area);
        let l = layout(view, self, area);
        let Some(thread) = (l.board.as_ref())
            .and_then(BoardLayout::writes_to)
            .filter(|_| view.is_ok())
        else {
            return;
        };
        if self.posting.is_some() {
            self.notice = Some((
                "a post is running: paste again once it is done".into(),
                true,
            ));
            return;
        }
        let text: String = (text.replace("\r\n", "\n").replace('\r', "\n").chars())
            .filter_map(typed)
            .collect();
        if !text.is_empty() {
            self.drafts.entry(thread).or_default().insert(&text);
        }
    }

    /// Takes in how the post that was running went, `said` as `Outcome::said` puts it: on
    /// success its draft is cleared; on a failure it stays, to try again.
    pub fn posted(&mut self, said: (String, bool)) {
        if let (Some(thread), false) = (self.posting.take(), said.1) {
            self.drafts.remove(&thread);
            self.views.iter_mut().for_each(|v| v.board.reply = None);
        }
        self.notice = Some(said);
    }

    /// Whether the overview is open across a narrow pane, in the tree's place.
    pub fn across(&self) -> bool {
        self.across
    }

    /// Whether Esc closes something that shows in `l`: a detail view, or the overview open
    /// across a narrow pane.
    pub fn can_go_back(&self, l: &Layout) -> bool {
        self.views.len() > 1 || (self.across && l.tree.is_none())
    }

    /// The view on top of the stack.
    pub fn view(&self) -> ViewKind {
        self.top().kind
    }

    /// The view the right pane shows on `board`: the view on top, except that a standalone
    /// task has no overview above it, so it shows its task detail straight away.
    pub fn shown(&self, board: &Board) -> ViewKind {
        match self.view() {
            ViewKind::Overview
                if self
                    .selection(board)
                    .is_some_and(|s| standalone_of(board, s).is_some()) =>
            {
                ViewKind::TaskDetail
            }
            kind => kind,
        }
    }

    /// How many views are in the stack: the overview and the detail views over it.
    pub fn views(&self) -> usize {
        self.views.len()
    }

    /// How far the view showing in the right pane is scrolled.
    pub fn right_scroll(&self) -> usize {
        self.top().scroll
    }

    /// The thread the full board was opened on or moved to, while it shows; the board
    /// shows the first thread when this is `None` or gone.
    pub fn board_thread_wanted(&self) -> Option<ThreadId> {
        (self.view() == ViewKind::Board)
            .then_some(self.top().board.thread)
            .flatten()
    }

    /// The thread the full board shows on `view`, while it shows: the one whose whole
    /// history the refresh reads in.
    pub fn board_thread(&self, view: &View) -> Option<ThreadId> {
        let board = view.as_ref().ok()?;
        if self.view() != ViewKind::Board {
            return None;
        }
        let talk = board_of(board, self.selection(board)?)?;
        full_board::shown(&talk, self.top().board.thread).map(|t| t.id)
    }

    /// The awareness items the owner reads by opening a thread (spec 3a): when the thread
    /// the full board shows on `view` is not the one it showed at the last call, that
    /// thread's unread awareness items; otherwise none, so an item posted while the thread
    /// stays open stays unread. Its need-you items stay until the owner clears them. A
    /// view that could not be read changes nothing: a failed refresh closes no thread.
    pub fn opened(&mut self, view: &View) -> Vec<i64> {
        let Ok(board) = view else {
            return Vec::new();
        };
        let shown = self.board_thread(view);
        if shown == self.opened_thread {
            return Vec::new();
        }
        self.opened_thread = shown;
        // Every thread its conversation shows, those folded into it too.
        let Some(home) = shown else {
            return Vec::new();
        };
        let talk = self.selection(board).and_then(|s| board_of(board, s));
        let threads = talk.map_or_else(|| vec![home], |t| t.joined(home));
        (threads.into_iter())
            .flat_map(|t| board.inbox.unread_on(t))
            .collect()
    }

    /// Asks the edge to open task `id`'s ticket in the browser, saying which; a task
    /// with no tracker link says so instead.
    fn open_ticket(&mut self, board: &Board, id: TaskId) -> Option<Request> {
        let task = board.task(id)?;
        match (task.ticket(), task.details.link.clone()) {
            (Some(k), Some(link)) => {
                self.notice = Some((format!("opening {k} in the browser"), false));
                Some(Request::Open(link))
            }
            _ => {
                self.notice = Some((
                    format!(
                        "this task has no ticket (tower task set {} --link <url>)",
                        id.0
                    ),
                    false,
                ));
                None
            }
        }
    }

    /// The full board's RESOLVED group as the owner left it: folded by default.
    pub fn board_fold(&self) -> ResolvedFold {
        self.top().board.fold
    }

    /// Where the full board's conversation is scrolled back to; `None` at the latest.
    pub fn board_anchor(&self) -> Option<Anchor> {
        self.top().board.anchor
    }

    /// Sets the clock the full board's chat, the message board and a detail view's boards
    /// are reckoned at, to the minute: their separators and ages both, so they agree, and
    /// their layout holds for the whole minute. The edge sets it before each frame and input.
    pub fn set_now(&mut self, now: i64) {
        self.minute = Some(now - now.rem_euclid(60));
    }

    /// The clock the full board's chat, the message board and a detail view's boards are
    /// reckoned at. The edge sets it before anything is laid out; without it every message
    /// would read as "now", so it panics instead.
    pub fn minute(&self) -> i64 {
        self.minute
            .expect("Dash::set_now runs before the full board, the message board or a detail view is laid out")
    }

    /// The messages open in full in the chat, by id.
    pub fn open_messages(&self) -> &BTreeSet<i64> {
        &self.open_messages
    }

    /// How the owner left the overview and detail boards of `sel`: the messages open in
    /// full, and whether its RESOLVED row is unfolded.
    pub fn board_shown(&self, sel: Item) -> Shown<'_> {
        Shown {
            opened: &self.open_messages,
            unfolded: self.unfolded_boards.contains(&sel),
        }
    }

    /// The question the owner is answering in the composer, by message id.
    pub fn replying(&self) -> Option<i64> {
        self.top().board.reply
    }

    /// The message selected in the full board's chat, by id, while the keys are in it.
    pub fn board_message(&self) -> Option<i64> {
        self.top().board.message
    }

    /// How far the full board's channel list is scrolled.
    pub fn list_scroll(&self) -> usize {
        self.top().board.list_scroll
    }

    /// The inbox item selected on `board`, while the inbox shows: the one picked while it
    /// is still in the inbox; once it leaves (read, cleared, or its thread resolved), the
    /// item now in its place, or the last; with none picked, the first.
    pub fn inbox_selected(&self, board: &Board) -> Option<i64> {
        if self.view() != ViewKind::Inbox {
            return None;
        }
        let ids: Vec<i64> = board.inbox.all().map(|a| a.message.id).collect();
        match self.top().inbox {
            Some((id, _)) if ids.contains(&id) => Some(id),
            Some((_, at)) => ids.get(at.min(ids.len().saturating_sub(1))).copied(),
            None => ids.first().copied(),
        }
    }

    /// The inbox item Enter and `r` act on, laid out as `ib`: the selected one, while its
    /// first line is in view. `None` while it is not (the wheel can scroll every item's
    /// first line away from a short list), so nothing is highlighted and those keys do
    /// nothing until the arrows or the wheel bring an item back.
    pub fn inbox_target(&self, board: &Board, ib: &InboxLayout) -> Option<i64> {
        let id = self.inbox_selected(board)?;
        let first = ib.rows.iter().position(|r| *r == Some(id))?;
        let scroll = clamp_scroll(self.inbox_scroll(), ib.rows.len(), ib.list.height);
        (scroll..scroll + usize::from(ib.list.height))
            .contains(&first)
            .then_some(id)
    }

    /// How far the inbox's list is scrolled.
    pub fn inbox_scroll(&self) -> usize {
        self.top().scroll
    }

    fn top(&self) -> &OpenView {
        self.views.last().expect("the overview always stays open")
    }

    fn top_mut(&mut self) -> &mut OpenView {
        self.views
            .last_mut()
            .expect("the overview always stays open")
    }

    /// The selection as the board is now: the stored item while it is still on the board;
    /// a Done group whose last milestone reopened, its own repo; otherwise the first
    /// row the tree shows, the first repo as UI v0.1.7 opens on it (none on an empty board).
    pub fn selection(&self, board: &Board) -> Option<Item> {
        let all = items(board);
        match self.selected {
            Some(s) if all.contains(&s) => Some(s),
            Some(Item::Done(r)) if all.contains(&Item::Repo(r)) => Some(Item::Repo(r)),
            _ => shown_items(board).first().copied(),
        }
    }

    /// The selection as the tree shows it: something folded away shows as the folded row
    /// it is under.
    pub fn tree_selection(&self, board: &Board) -> Option<Item> {
        let sel = self.selection(board)?;
        Some(folded_under(board, sel).unwrap_or(sel))
    }

    /// Selects the folded row `item` in place of what it hides, and closes the detail views
    /// over it, which showed what the tree no longer shows.
    fn select_folded(&mut self, board: &Board, item: Item) {
        self.select(board, item);
        self.close_details();
        self.reveal_due = true;
    }

    /// Folds `folds` (or unfolds them), asking the edge to write the ones that change.
    /// Folding what the selection is under selects the folded row.
    fn fold(&mut self, board: &Board, folds: &[Fold], folded: bool) -> Option<Request> {
        let repos: Vec<RepoId> = (board.repos.iter())
            .filter(|r| folds.contains(&Fold::Repo(r.id)) && r.folded != folded)
            .map(|r| r.id)
            .collect();
        let milestones: Vec<MilestoneId> = (board.milestones.iter())
            .filter(|m| folds.contains(&Fold::Milestone(m.id)) && m.folded != folded)
            .map(|m| m.id)
            .collect();
        let done_groups: Vec<RepoId> = (board.repos.iter())
            .filter(|r| folds.contains(&Fold::Done(r.id)) && r.done_folded != folded)
            .map(|r| r.id)
            .collect();
        if folded && let Some(s) = self.selection(board) {
            let repo = repo_of(board, s).filter(|r| repos.contains(&r.id));
            let milestone = milestone_of(board, s).filter(|m| milestones.contains(&m.id));
            let group = done_milestone(board, s).filter(|(r, _)| done_groups.contains(&r.id));
            match (s, repo, milestone, group) {
                (Item::Repo(_), ..) => {}
                (_, Some(r), ..) => self.select_folded(board, Item::Repo(r.id)),
                (Item::Task(_), None, Some(m), _) => {
                    self.select_folded(board, Item::Milestone(m.id))
                }
                (_, None, _, Some((r, _))) => self.select_folded(board, Item::Done(r.id)),
                _ => {}
            }
        }
        let any = !repos.is_empty() || !milestones.is_empty() || !done_groups.is_empty();
        any.then_some(Request::Fold {
            repos,
            milestones,
            done_groups,
            folded,
        })
    }

    /// Unfolds what hides the selection, its repo and then its milestone or Done group, as
    /// picking a folded task by its card does, so the tree shows what is selected.
    fn unfold_selection(&mut self, board: &Board) -> Option<Request> {
        let sel = self.selection(board)?;
        let mut folds = Vec::new();
        if !matches!(sel, Item::Repo(_))
            && let Some(r) = repo_of(board, sel).filter(|r| r.folded)
        {
            folds.push(Fold::Repo(r.id));
        }
        if let (Item::Task(_), Some(m)) = (sel, milestone_of(board, sel))
            && m.folded
        {
            folds.push(Fold::Milestone(m.id));
        }
        if let Some((r, _)) = done_milestone(board, sel).filter(|(r, _)| r.done_folded) {
            folds.push(Fold::Done(r.id));
        }
        self.unfolding = folds.first().copied();
        self.fold(board, &folds, false)
    }

    /// Takes in a new refresh. When the selected item is gone from the board, the selection
    /// falls back as `selection` says, the detail views close (they showed what is gone)
    /// and the overviews start at their top, since they now show something else.
    pub fn refreshed(&mut self, view: &View, area: Rect) {
        let Ok(board) = view else { return };
        // A selected task whose milestone got the Done mark is read-only in its Done group
        // now: its milestone takes the selection (the Done line, or the folded Done row
        // below), and the detail closes, as UI v0.1.7 walks up to the row's parents.
        if let Some(s @ Item::Task(_)) = self.selected
            && let Some(m) = milestone_of(board, s)
            && done_milestone(board, Item::Milestone(m.id)).is_some()
        {
            self.select_folded(board, Item::Milestone(m.id));
        }
        // Messages opened in full stay so while their threads are on the board.
        let threads: BTreeSet<ThreadId> = (board.repos.iter())
            .flat_map(|r| r.threads.iter().map(|t| t.id))
            .collect();
        self.open_in.retain(|_, t| threads.contains(t));
        let open_in = &self.open_in;
        self.open_messages.retain(|id| open_in.contains_key(id));
        if let Some(s) = self.selected
            && !items(board).contains(&s)
        {
            self.selected = self.selection(board);
            self.close_details();
            self.views.iter_mut().for_each(|v| v.scroll = 0);
        }
        let folds = folds_of(board);
        if let Some(f) = self.unfolding
            && !folds.contains(&f)
        {
            self.unfolding = None;
        }
        // The selection was folded away (by the other dashboard): the folded row becomes
        // the selection, unless the owner just picked it and its unfold is running.
        if let Some(s) = self.selected
            && let Some(row) = folded_under(board, s)
            && self.unfolding != fold_of(row)
        {
            self.select_folded(board, row);
        }
        // The thread the full board showed is gone: it falls back to the first, at its latest.
        if let Some(wanted) = self.board_thread_wanted()
            && self.board_thread(view) != Some(wanted)
        {
            let shown = self.board_thread(view);
            let b = &mut self.top_mut().board;
            b.thread = shown;
            b.anchor = None;
        }
        // The full board's list changed under its selection. A fold change: the next fit
        // scrolls the selection into view. Its line moved (the thread shown resolved, or a
        // thread above it resolved or opened): the next fit keeps it in view if it was.
        // The fold is kept as the list shows it.
        if let Some(talk) = (self.view() == ViewKind::Board)
            .then(|| self.selection(board).and_then(|s| board_of(board, s)))
            .flatten()
        {
            let v = &self.top().board;
            let t = full_board::shown(&talk, v.thread);
            let fold = full_board::shown_fold(&talk, t, v.fold);
            let stop = if fold.on_row {
                Some(Stop::Resolved)
            } else {
                t.map(|t| Stop::Thread(t.id))
            };
            let at = stop.and_then(|s| {
                // The list's lines as a frame `area` wide draws them: beside the
                // conversation or above it, long names wrapped to its width.
                let w = full_board::list_width(usize::from(area.width));
                let stops = full_board::list_stops(&talk, fold.unfolded, w);
                let first = stops.iter().position(|r| *r == Some(s))?;
                let last = stops.iter().rposition(|r| *r == Some(s))?;
                Some((s, first, last))
            });
            if fold != v.fold {
                self.list_due = true;
            } else if let (Some((a, first, last)), Some((b, to, _))) = (v.at, at)
                && a == b
                && first != to
            {
                self.list_moved = Some((first, last));
            }
            let v = &mut self.top_mut().board;
            v.fold = fold;
            v.at = at;
        }
        // An inbox item left (read, or its thread resolved): the next one takes its place.
        if self.view() == ViewKind::Inbox
            && let Some((id, _)) = self.top().inbox
            && !board.inbox.all().any(|a| a.message.id == id)
        {
            self.list_due = true;
        }
        // A fold changes which rows the tree has above the selection: show it again.
        if self.folds_seen != folds {
            self.reveal_due = true;
            self.folds_seen = folds;
        }
    }

    /// Closes every detail view, leaving the overview under them.
    fn close_details(&mut self) {
        self.views.truncate(1);
    }

    /// Opens the selection, as Enter or a second click does: the overview across the pane
    /// when the tree hides it, otherwise the selection's detail over the overview.
    fn open(&mut self, board: &Board, l: &Layout) {
        let Some(sel) = self.selection(board) else {
            return;
        };
        if l.overview_hidden {
            self.across = true;
        } else if l.right.is_some() && self.shown(board) == ViewKind::Overview {
            self.views.push(OpenView::new(match sel {
                Item::Repo(_) => ViewKind::RepoDetail,
                Item::Milestone(_) => ViewKind::MilestoneDetail,
                Item::Task(_) => ViewKind::TaskDetail,
                // The Done group has no detail: its overview lists all it holds.
                Item::Done(_) => return,
            }));
        }
    }

    /// Takes in the terminal's size, before each frame and each input: once `area` is wide
    /// enough for the tree and the overview side by side, the overview open across the
    /// narrow pane closes, so no Esc is spent closing something that no longer shows. After
    /// a fold, the selection is scrolled into view.
    pub fn fit(&mut self, view: &View, area: Rect) {
        if self.across {
            let l = layout(view, self, area);
            if l.tree.is_some() && l.right.is_some() {
                self.across = false;
            }
        }
        if std::mem::take(&mut self.reveal_due) {
            self.reveal(view, area);
        }
        // The question is selected and scrolled to once the chat holds it (one older than
        // the loaded window comes with the whole history). Waiting ends when the reply is
        // dropped (a post, the board closing) or the board is left.
        if self.reply_due {
            let reply = self.top().board.reply;
            match (layout(view, self, area).board, reply) {
                (Some(b), Some(id)) => {
                    if b.starts.iter().any(|(m, _)| *m == id) {
                        self.select_message(&b, id);
                        self.reply_due = false;
                    }
                }
                _ => self.reply_due = false,
            }
        }
        if let Some(id) = self.opened_due.take()
            && let Some(b) = layout(view, self, area).board
        {
            self.select_message(&b, id);
        }
        if let Some((first, last)) = self.list_moved.take()
            && let Some(b) = layout(view, self, area).board
            && let Some(stop) = b.current()
        {
            // In view if any of its lines was: its title may already be above the window.
            let top = self.top().board.list_scroll;
            if first < top + usize::from(b.list.height).max(1) && last >= top {
                self.list_into_view(&b, stop);
            }
        }
        if std::mem::take(&mut self.list_due) {
            let l = layout(view, self, area);
            if let Some(b) = &l.board
                && let Some(stop) = b.current()
            {
                self.list_into_view(b, stop);
            } else if let (Some(ib), Ok(board)) = (&l.inbox, view)
                && let Some(id) = self.inbox_selected(board)
            {
                self.inbox_pick(ib, id);
            }
        }
    }

    /// Applies one input to a terminal of size `area` showing `view`, and returns what it
    /// asked the edge to run, if anything. While the board cannot be read only Esc does
    /// anything. While the owner is writing, every input but Esc, which goes back as anywhere
    /// else, the draft kept.
    pub fn handle(&mut self, view: &View, area: Rect, input: Input) -> Option<Request> {
        self.notice = None;
        self.fit(view, area);
        let l = layout(view, self, area);
        let writing = (l.board.as_ref())
            .and_then(BoardLayout::writes_to)
            .filter(|_| view.is_ok());
        // Alt+Enter is a line break only in a draft.
        let input = match input {
            Input::NewLine if writing.is_none() => Input::Enter,
            input => input,
        };
        if input == Input::Char('q') && self.new_lead.is_none() && writing.is_none() {
            return Some(Request::Quit);
        }
        if self.new_lead.is_some() {
            return self.type_lead(input);
        }
        if input == Input::Esc {
            if self.mode == Mode::Popup && !self.can_go_back(&l) {
                return Some(Request::Quit);
            }
            self.back(&l);
            return None;
        }
        if let (Some(thread), Some(b)) = (writing, &l.board) {
            return self.write(thread, b, &l, input);
        }
        let Ok(board) = view else { return None };
        if let Some(b) = &l.board {
            return self.board_input(b, &l, input);
        }
        if let Some(ib) = &l.inbox {
            return self.inbox_input(view, area, board, ib, &l, input);
        }
        match input {
            Input::Up | Input::Down => {
                let all = shown_items(board);
                let cur = self.tree_selection(board)?;
                // Moving away from what a detail view shows closes it, as in the signed design.
                self.close_details();
                let i = all.iter().position(|x| *x == cur).unwrap_or(0);
                let next = if input == Input::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(all.len() - 1)
                };
                self.select(board, all[next]);
                self.reveal(view, area);
            }
            Input::PageUp | Input::PageDown => {
                let down = input == Input::PageDown;
                match (&l.right, l.right_lines(), l.tree) {
                    (Some(r), Some(lines), _) => {
                        let page = page(r.height);
                        let v = self.top_mut();
                        v.scroll = scroll_by(v.scroll, page, down, lines, r.height);
                    }
                    (_, _, Some(t)) => {
                        let page = page(t.height);
                        let len = l.tree_items.len();
                        self.tree_scroll = scroll_by(self.tree_scroll, page, down, len, t.height);
                    }
                    _ => {}
                }
            }
            Input::Enter => self.open(board, &l),
            Input::Left | Input::Right if l.tree.is_some() => {
                // The row folds itself (a repo, a milestone, a Done group), a task its
                // milestone, a standalone task its repo, and a Done line its Done group.
                let sel = self.selection(board)?;
                let group = match sel {
                    Item::Done(r) => board.repo(r),
                    _ => done_milestone(board, sel).map(|(r, _)| r),
                };
                let (fold, folded) = match (sel, milestone_of(board, sel)) {
                    _ if group.is_some() => {
                        let r = group?;
                        (Fold::Done(r.id), r.done_folded)
                    }
                    (Item::Repo(_), _) | (Item::Task(_), None) => {
                        let r = repo_of(board, sel)?;
                        (Fold::Repo(r.id), r.folded)
                    }
                    (_, Some(m)) => (Fold::Milestone(m.id), m.folded),
                    (Item::Milestone(_) | Item::Done(_), None) => return None,
                };
                return self.fold(board, &[fold], !folded);
            }
            Input::Char('z') if l.tree.is_some() => {
                // Every active milestone; repos and Done groups keep their folds.
                let active = active_milestones(board);
                let all: Vec<Fold> = active.iter().map(|m| Fold::Milestone(m.id)).collect();
                let folded = !active.iter().all(|m| m.folded);
                return self.fold(board, &all, folded);
            }
            Input::Char('n') if self.handing.is_none() => {
                // On the selection's repo, refused while it has its lead (one lead per
                // repo); in no repo yet only for a selection in no repo, or none.
                let repo = self.selection(board).and_then(|s| repo_of(board, s));
                if let Some((r, l)) = repo.and_then(|r| Some((r, r.lead.as_ref()?))) {
                    let why = format!(
                        "{} has its lead, {}: select a repo without one, or something in no repo",
                        r.name(),
                        l.name
                    );
                    self.notice = Some((why, true));
                    return None;
                }
                // Its lead's place opens on the repo's main checkout, so it must be known.
                if let Some(r) = repo.filter(|r| r.path.is_none()) {
                    self.notice = Some((super::writer::no_checkout(r), true));
                    return None;
                }
                self.new_lead_repo = repo.map(|r| r.id);
                self.new_lead = Some(String::new());
            }
            Input::Char('i') => self.open_inbox(view, area),
            // The full board and the inbox take their keys above, so `o` acts only where the
            // tree's selection is on screen.
            Input::Char('o') => {
                if let Some(Item::Task(id)) = self.selection(board) {
                    return self.open_ticket(board, id);
                }
            }
            Input::Char('b') if l.has_board => {
                // On the selected task's thread, else on the board's first.
                let thread = match self.selection(board) {
                    Some(s @ Item::Task(id)) => {
                        (board_of(board, s)).and_then(|b| b.ordered(Some(id)).first().map(|t| t.id))
                    }
                    _ => None,
                };
                self.open_board(view, area, thread);
            }
            Input::Click { x, y } => {
                if l.new_lead.is_some_and(|a| contains(a, x, y)) {
                    return self.handle(view, area, Control::NewLead.key());
                } else if l.alert.is_some_and(|a| contains(a, x, y)) {
                    self.open_inbox(view, area);
                } else if let Some(id) = l.ticket_at(board, self, x, y) {
                    self.select(board, Item::Task(id));
                    return self.open_ticket(board, id);
                } else if let Some(fold) = l.fold_at(board, self, x, y) {
                    return match fold {
                        FoldClick::Mark(f, _) => {
                            // From the board as last read: a second click before the
                            // refresh that shows the first one repeats it.
                            let folded = folds_of(board).contains(&f);
                            self.fold(board, &[f], !folded)
                        }
                        FoldClick::Line(f) => self.fold(board, &[f], false),
                    };
                } else if l.resolved_at(self, x, y) {
                    // The RESOLVED row of the selection's board folds or unfolds.
                    if let Some(sel) = self.selection(board) {
                        if let Some(at) = self.unfolded_boards.iter().position(|i| *i == sel) {
                            self.unfolded_boards.remove(at);
                        } else {
                            self.unfolded_boards.push(sel);
                        }
                    }
                } else if let Some(row) = l.thread_at(self, x, y) {
                    match (row.toggle, row.message) {
                        (Some(id), _) => self.toggle_message(row.thread, id),
                        (None, None) => self.open_board(view, area, Some(row.thread)),
                        (None, Some(id)) => {
                            self.open_board(view, area, Some(row.thread));
                            self.reply_to(row.thread, id);
                            self.fit(view, area);
                        }
                    }
                } else if let Some(c) = l.control_at(self, x, y) {
                    // A click on a control is its key, so the two never differ.
                    return self.handle(view, area, c.key());
                } else if let Some(m) = l.detail_milestone(self, x, y) {
                    // A milestone line in the repo detail opens its detail; Esc comes back.
                    let back = self.selection(board);
                    self.select(board, Item::Milestone(m));
                    self.views.push(OpenView {
                        restore: back,
                        ..OpenView::new(ViewKind::MilestoneDetail)
                    });
                    return self.unfold_selection(board);
                } else if let Some(task) =
                    (l.detail_row(self, x, y)).filter(|t| items(board).contains(&Item::Task(*t)))
                {
                    // A task row in the milestone detail opens its task; Esc comes back.
                    let back = self.selection(board);
                    self.select(board, Item::Task(task));
                    self.views.push(OpenView {
                        restore: back,
                        ..OpenView::new(ViewKind::TaskDetail)
                    });
                    return self.unfold_selection(board);
                } else if let Some(item) = (l.hit(self, x, y)).filter(|i| items(board).contains(i))
                {
                    if self.selection(board) == Some(item) {
                        self.open(board, &l);
                    } else {
                        self.close_details();
                        self.select(board, item);
                        self.reveal(view, area);
                        return self.unfold_selection(board);
                    }
                }
            }
            Input::Wheel { x, y, down } => self.wheel(&l, x, y, down),
            // Alt+Enter is Enter here (`handle`).
            Input::NewLine => {}
            Input::Esc | Input::Left | Input::Right | Input::Char(_) | Input::Backspace => {}
        }
        None
    }

    /// An input while the keys write to `thread`, the open thread the full board shows as
    /// `b`: a character goes into its draft at the cursor, Alt+Enter a line break,
    /// Backspace takes the cluster before the cursor away and, while there is a draft, left
    /// and right move the cursor. Enter or a click on "Post" posts it. The rest is the
    /// board's (`board_input`), the keys staying here: up and down move between threads or
    /// messages, the conversation scrolls, and a click acts where it lands. While a post
    /// runs the draft does not change.
    fn write(
        &mut self,
        thread: ThreadId,
        b: &BoardLayout,
        l: &Layout,
        input: Input,
    ) -> Option<Request> {
        let idle = self.posting.is_none();
        match input {
            Input::Enter => return self.post(thread),
            Input::Click { x, y } if contains(b.post, x, y) => return self.post(thread),
            Input::Click { x, y } if contains(b.input, x, y) => {}
            Input::Char(c) if idle => self.type_in(thread, c),
            Input::NewLine if idle => self.type_in(thread, '\n'),
            Input::Backspace if idle => {
                if let Some(d) = self.drafts.get_mut(&thread) {
                    d.back();
                    if d.text.is_empty() {
                        self.drafts.remove(&thread);
                    }
                }
            }
            Input::Left | Input::Right if self.drafts.contains_key(&thread) => {
                if let Some(d) = self.drafts.get_mut(&thread) {
                    d.step(input == Input::Right);
                }
            }
            Input::Char(_) | Input::NewLine | Input::Backspace => {}
            Input::Esc
            | Input::Up
            | Input::Down
            | Input::Left
            | Input::Right
            | Input::PageUp
            | Input::PageDown
            | Input::Click { .. }
            | Input::Wheel { .. } => return self.board_input(b, l, input),
        }
        None
    }

    /// Types `c` into `thread`'s draft at its cursor (`typed`).
    fn type_in(&mut self, thread: ThreadId, c: char) {
        if let Some(text) = typed(c) {
            self.drafts.entry(thread).or_default().insert(&text);
        }
    }

    /// An input while the new lead's input is open: characters go into the goal (only what
    /// one line on the board takes, up to `MAX_LINE_CHARS`), Backspace takes one away, Enter
    /// starts the lead, refusing an empty goal, and Esc cancels.
    fn type_lead(&mut self, input: Input) -> Option<Request> {
        let line = self.new_lead.as_mut()?;
        match input {
            Input::Esc => {
                self.new_lead = None;
                self.new_lead_repo = None;
            }
            Input::Enter if line.trim().is_empty() => {
                self.notice = Some(("type the goal first: it is empty".into(), true));
            }
            Input::Enter => {
                let line = self.new_lead.take()?.trim().to_string();
                self.handing = Some("starting a lead…".into());
                let repo = self.new_lead_repo.take();
                return Some(Request::StartLead { line, repo });
            }
            Input::Char(c) if hidden(&c) => {}
            Input::Char(_) if line.chars().count() >= MAX_LINE_CHARS => {
                let most = format!("a goal is one line of at most {MAX_LINE_CHARS} characters");
                self.notice = Some((most, true));
            }
            Input::Char(c) => line.push(c),
            Input::Backspace => {
                let last = line
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                line.truncate(last);
            }
            _ => {}
        }
        None
    }

    /// An input on the inbox laid out as `ib`: up/down move over its items, Enter or a click
    /// opens the item's thread on the full board (Esc there comes back here), `r` asks to
    /// mark an unread awareness item read, `i` or a click on "◀ back" goes back. Enter and
    /// `r` act on `inbox_target`, never on a selection out of view.
    fn inbox_input(
        &mut self,
        view: &View,
        area: Rect,
        board: &Board,
        ib: &InboxLayout,
        l: &Layout,
        input: Input,
    ) -> Option<Request> {
        let inbox = &board.inbox;
        let ids: Vec<i64> = inbox.all().map(|a| a.message.id).collect();
        let at = self.inbox_selected(board);
        let target = self.inbox_target(board, ib);
        let i = ids.iter().position(|id| Some(*id) == at);
        match input {
            Input::Up | Input::Down => {
                let i = i?;
                let next = if input == Input::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(ids.len() - 1)
                };
                self.inbox_pick(ib, ids[next]);
            }
            Input::Enter => self.open_from_inbox(view, area, inbox, target?),
            Input::Char('r') => {
                // Every item listed can be marked: the store refuses one read meanwhile.
                let id = target?;
                if self.handing.is_some() {
                    return None;
                }
                let a = inbox.all().find(|a| a.message.id == id)?;
                let item = super::inbox::alert_name(board, a);
                if inbox.need_you.iter().any(|a| a.message.id == id) {
                    self.handing = Some("clearing…".into());
                    return Some(Request::Clear { id, item });
                }
                self.handing = Some("marking read…".into());
                return Some(Request::MarkRead { id, item });
            }
            Input::Char('i') => self.back(l),
            Input::PageUp | Input::PageDown => {
                let page = page(ib.list.height);
                let v = self.top_mut();
                let down = input == Input::PageDown;
                let scroll = scroll_by(v.scroll, page, down, ib.rows.len(), ib.list.height);
                v.scroll = page_to_item(scroll, down, ib);
                self.inbox_follow_view(ib, at);
            }
            Input::Click { x, y } => {
                if contains(ib.top, x, y) && x < ib.top.x + INBOX_BACK {
                    self.back(l);
                } else if contains(ib.list, x, y) {
                    let scroll = clamp_scroll(self.inbox_scroll(), ib.rows.len(), ib.list.height);
                    if let Some(Some(id)) = ib.rows.get(usize::from(y - ib.list.y) + scroll) {
                        let at = ids.iter().position(|i| i == id).unwrap_or(0);
                        self.top_mut().inbox = Some((*id, at));
                        self.open_from_inbox(view, area, inbox, *id);
                    }
                }
            }
            Input::Wheel { x, y, down } => {
                if contains(ib.list, x, y) {
                    let v = self.top_mut();
                    v.scroll =
                        scroll_by(v.scroll, WHEEL_LINES, down, ib.rows.len(), ib.list.height);
                    self.inbox_follow_view(ib, at);
                }
            }
            // Alt+Enter is Enter here (`handle`).
            Input::NewLine => {}
            Input::Esc | Input::Left | Input::Right | Input::Char(_) | Input::Backspace => {}
        }
        None
    }

    /// Posts `thread`'s draft, unless a post is running already; a blank draft gets a
    /// notice instead.
    fn post(&mut self, thread: ThreadId) -> Option<Request> {
        if self.posting.is_some() {
            return None;
        }
        let Some(d) = (self.drafts.get(&thread)).filter(|d| !d.text.trim().is_empty()) else {
            self.notice = Some(("nothing to post".into(), false));
            return None;
        };
        let text = d.text.clone();
        self.posting = Some(thread);
        Some(Request::Post { thread, text })
    }

    /// Selects the inbox item with message id `id`, its lines in view.
    fn inbox_pick(&mut self, ib: &InboxLayout, id: i64) {
        // Its place among the items: the rows list them in order, between heads and gaps,
        // an item on each of its lines.
        let mut ids = (ib.rows.iter().flatten()).collect::<Vec<_>>();
        ids.dedup();
        let ids_at = ids.iter().position(|i| **i == id).unwrap_or(0);
        let v = self.top_mut();
        v.inbox = Some((id, ids_at));
        let first = ib.rows.iter().position(|r| *r == Some(id));
        let last = ib.rows.iter().rposition(|r| *r == Some(id));
        if let (Some(first), Some(last)) = (first, last) {
            let at = into_view(v.scroll, first, last, ib.list.height);
            v.scroll = clamp_scroll(at, ib.rows.len(), ib.list.height);
        }
    }

    /// After the inbox list scrolled, with `at` selected: an item no longer wholly in view
    /// hands the selection to the nearest item that is (or, when none is, the nearest whose
    /// first line is in view). When no item's first line is in view the selection stays,
    /// out of sight, and `inbox_target` keeps Enter and `r` off it.
    fn inbox_follow_view(&mut self, ib: &InboxLayout, at: Option<i64>) {
        let Some(at) = at else {
            return;
        };
        let scroll = clamp_scroll(self.inbox_scroll(), ib.rows.len(), ib.list.height);
        let shown = scroll..scroll + usize::from(ib.list.height);
        let items = inbox_items(&ib.rows);
        let Some(&(_, from, to)) = items.iter().find(|(id, ..)| *id == at) else {
            return;
        };
        let whole = |(_, a, b): &&(i64, usize, usize)| shown.contains(a) && shown.contains(b);
        let first = |(_, a, _): &&(i64, usize, usize)| shown.contains(a);
        if items.iter().filter(whole).any(|(id, ..)| *id == at) {
            return;
        }
        let distance = |(_, a, b): &&(i64, usize, usize)| {
            if *b < from {
                from - b
            } else {
                a.saturating_sub(to)
            }
        };
        let near = (items.iter().filter(whole).min_by_key(distance))
            .or_else(|| items.iter().filter(first).min_by_key(distance));
        if let Some(&(id, ..)) = near {
            let index = items.iter().position(|(i, ..)| *i == id).unwrap_or(0);
            self.top_mut().inbox = Some((id, index));
        }
    }

    /// Opens the inbox over whatever shows. Its first item can sit below a short list's top
    /// (an empty NEED YOU section comes first), so the selection comes into view as
    /// selecting brings it when its first line would not show.
    fn open_inbox(&mut self, view: &View, area: Rect) {
        self.views.push(OpenView::new(ViewKind::Inbox));
        if let (Some(ib), Ok(board)) = (layout(view, self, area).inbox, view)
            && let Some(id) = self.inbox_selected(board)
            && self.inbox_target(board, &ib).is_none()
        {
            self.inbox_pick(&ib, id);
        }
    }

    /// Opens the thread of inbox item `id` on the full board, its lead's milestone selected
    /// until Esc comes back to the inbox.
    fn open_from_inbox(&mut self, view: &View, area: Rect, inbox: &Inbox, id: i64) {
        let Some(a) = inbox.all().find(|a| a.message.id == id) else {
            return;
        };
        let (repo, thread) = (a.repo, a.thread);
        let back = view.as_ref().ok().and_then(|b| self.selection(b));
        self.selected = Some(Item::Repo(repo));
        self.open_board(view, area, Some(thread));
        self.top_mut().restore = back;
    }

    /// Esc: closes the view on top, back to the selection it was opened from, or the
    /// overview open across a narrow pane.
    fn back(&mut self, l: &Layout) {
        if self.views.len() > 1 {
            if let Some(back) = self.views.pop().and_then(|v| v.restore) {
                self.selected = Some(back);
            }
            // Items may have left the inbox while a view covered it (opening an item's
            // thread reads it): the next fit picks its selection again, in view.
            if self.view() == ViewKind::Inbox {
                self.list_due = true;
            }
        } else if self.can_go_back(l) {
            self.across = false;
        }
    }

    /// Opens the full board over whatever shows, on `thread` (its first when `None`), at
    /// the latest message, with the thread's list item in view.
    fn open_board(&mut self, view: &View, area: Rect, thread: Option<ThreadId>) {
        self.views.push(OpenView {
            board: BoardView {
                thread,
                ..BoardView::default()
            },
            ..OpenView::new(ViewKind::Board)
        });
        if let Some(b) = layout(view, self, area).board
            && let Some(shown) = b.thread
        {
            self.show_thread(&b, shown);
        }
    }

    /// An input on the full board laid out as `b`: up/down move between threads, the
    /// conversation and the channel list scroll on their own, a click on a thread shows it
    /// and a click on "◀ back" goes back. On an open thread the keys write (`write`), which
    /// leaves the rest to this.
    fn board_input(&mut self, b: &BoardLayout, l: &Layout, input: Input) -> Option<Request> {
        let message = self.top().board.message;
        // The fold as the list shows it, so moving off a resolved thread shown from
        // elsewhere keeps its group open.
        self.top_mut().board.fold = b.fold;
        match input {
            // A message selected (by a click on its "read more" with the row selected) takes
            // the keys first, as in the copy and as the keys line says.
            Input::Right if b.fold.on_row && message.is_none() => self.set_unfolded(true),
            Input::Left if b.fold.on_row && message.is_none() => self.set_unfolded(false),
            Input::Left if message.is_none() && b.on_resolved_thread() => self.set_unfolded(false),
            Input::Up | Input::Down if message.is_some() => {
                let ids: Vec<i64> = b.starts.iter().map(|(id, _)| *id).collect();
                // A selected message gone from the chat leaves the newest selected.
                let Some(i) = ids.iter().position(|id| Some(*id) == message) else {
                    if let Some(id) = ids.last() {
                        self.select_message(b, *id);
                    }
                    return None;
                };
                let next = if input == Input::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(ids.len().saturating_sub(1))
                };
                if let Some(id) = ids.get(next) {
                    self.select_message(b, *id);
                }
            }
            Input::Right if message.is_none() => {
                if let Some((id, _)) = b.starts.last() {
                    self.select_message(b, *id);
                }
            }
            Input::Right | Input::Enter => {
                if let Some(id) = message.filter(|id| b.toggles.iter().any(|(_, t)| t == id)) {
                    self.toggle_message(b.thread?, id);
                }
            }
            Input::Left => self.top_mut().board.message = None,
            Input::Up | Input::Down => {
                let stops = b.stops();
                let i = stops.iter().position(|s| Some(*s) == b.current())?;
                let next = if input == Input::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(stops.len() - 1)
                };
                self.show_stop(b, stops[next]);
            }
            Input::PageUp | Input::PageDown => {
                let page = page(b.conv.height);
                self.scroll_conv(b, page, input == Input::PageUp);
            }
            Input::Click { x, y } if contains(b.strip, x, y) => {
                if let Some(Some((thread, id))) = b.strip_rows.get(usize::from(y - b.strip.y)) {
                    self.reply_to(*thread, *id);
                }
            }
            Input::Click { x, y } => {
                if contains(b.top, x, y) && x < b.top.x + BACK_COLUMNS {
                    self.back(l);
                } else if contains(b.list, x, y) {
                    let len = b.list_rows.len();
                    let scroll = clamp_scroll(self.list_scroll(), len, b.list.height);
                    match b.list_rows.get(usize::from(y - b.list.y) + scroll) {
                        Some(Some(Stop::Thread(id))) => self.show_thread(b, *id),
                        Some(Some(Stop::Resolved)) => self.set_unfolded(!b.fold.unfolded),
                        _ => {}
                    }
                } else if contains(b.conv, x, y) {
                    let line = b.conv_top(self.board_anchor()) + usize::from(y - b.conv.y);
                    if let Some((_, id)) = b.toggles.iter().find(|(l, _)| *l == line) {
                        let id = *id;
                        self.toggle_message(b.thread?, id);
                        self.top_mut().board.message = Some(id);
                    }
                }
            }
            Input::Wheel { x, y, down } => {
                if contains(b.list, x, y) {
                    let v = &mut self.top_mut().board;
                    let len = b.list_rows.len();
                    v.list_scroll = scroll_by(v.list_scroll, WHEEL_LINES, down, len, b.list.height);
                } else if contains(b.conv, x, y) || contains(b.head, x, y) {
                    self.scroll_conv(b, WHEEL_LINES, !down);
                }
            }
            // Alt+Enter is Enter here (`handle`).
            Input::Esc | Input::NewLine => {}
            Input::Char(_) | Input::Backspace => {}
        }
        None
    }

    /// Shows `thread` on the full board with question `id` from a "Needs you" strip
    /// selected, replying (the keys write to it, as to any open thread); the next `fit`
    /// scrolls it in view.
    fn reply_to(&mut self, thread: ThreadId, id: i64) {
        let v = &mut self.top_mut().board;
        v.thread = Some(thread);
        v.fold.on_row = false;
        v.anchor = None;
        v.message = Some(id);
        v.reply = Some(id);
        self.reply_due = true;
    }

    /// Selects message `id` in the full board's chat and scrolls it into view.
    fn select_message(&mut self, b: &BoardLayout, id: i64) {
        let Some(i) = b.starts.iter().position(|(m, _)| *m == id) else {
            return;
        };
        let first = b.starts[i].1;
        let last = b
            .starts
            .get(i + 1)
            .map_or(b.conv_lines, |(_, l)| *l)
            .max(first + 1)
            - 1;
        let v = &mut self.top_mut().board;
        v.message = Some(id);
        let top = b.conv_top(v.anchor);
        let at = into_view(top, first, last, b.conv.height);
        v.anchor = b.anchor_at(at.min(b.max_top()));
    }

    /// Opens message `id` of `thread` in full in the chat, or folds it when open.
    fn toggle_message(&mut self, thread: ThreadId, id: i64) {
        if self.open_messages.remove(&id) {
            self.open_in.remove(&id);
        } else {
            self.open_messages.insert(id);
            self.open_in.insert(id, thread);
            self.opened_due = Some(id);
        }
    }

    /// Selects `stop` in the full board's list: a thread shows, the RESOLVED row takes the
    /// selection with the conversation left on the thread shown last.
    fn show_stop(&mut self, b: &BoardLayout, stop: Stop) {
        match stop {
            Stop::Thread(id) => self.show_thread(b, id),
            Stop::Resolved => {
                let v = &mut self.top_mut().board;
                v.fold.on_row = true;
                v.message = None;
                self.list_into_view(b, Stop::Resolved);
            }
        }
    }

    /// Unfolds the RESOLVED group (`unfolded`) or folds it, with the selection on its row,
    /// so it never stays on a thread the fold hides; the next `fit` scrolls the row into
    /// view in the list as it is then.
    fn set_unfolded(&mut self, unfolded: bool) {
        let v = &mut self.top_mut().board;
        v.fold = ResolvedFold {
            unfolded,
            on_row: true,
        };
        v.message = None;
        self.list_due = true;
    }

    /// Shows `thread` on the full board at its latest message, its list item in view.
    fn show_thread(&mut self, b: &BoardLayout, thread: ThreadId) {
        self.reply_due = false;
        let v = &mut self.top_mut().board;
        v.thread = Some(thread);
        v.fold.on_row = false;
        v.anchor = None;
        v.message = None;
        v.reply = None;
        self.list_into_view(b, Stop::Thread(thread));
    }

    /// Scrolls the full board's list so `stop`'s lines show, as `b` lays them out.
    fn list_into_view(&mut self, b: &BoardLayout, stop: Stop) {
        let rows: Vec<usize> = (b.list_rows.iter().enumerate())
            .filter(|(_, r)| **r == Some(stop))
            .map(|(i, _)| i)
            .collect();
        let v = &mut self.top_mut().board;
        if let (Some(&first), Some(&last)) = (rows.first(), rows.last()) {
            let len = b.list_rows.len();
            let at = into_view(v.list_scroll, first, last, b.list.height);
            v.list_scroll = clamp_scroll(at, len, b.list.height);
        }
    }

    /// Scrolls the conversation `by` lines back towards its start (`up`) or on towards the
    /// latest; back at the latest page it follows new messages again.
    fn scroll_conv(&mut self, b: &BoardLayout, by: usize, up: bool) {
        let v = &mut self.top_mut().board;
        let top = b.conv_top(v.anchor);
        let to = if up {
            top.saturating_sub(by)
        } else {
            top.saturating_add(by).min(b.max_top())
        };
        v.anchor = b.anchor_at(to);
    }

    fn wheel(&mut self, l: &Layout, x: u16, y: u16, down: bool) {
        if let Some(t) = l.tree.filter(|t| contains(*t, x, y)) {
            let len = l.tree_items.len();
            self.tree_scroll = scroll_by(self.tree_scroll, WHEEL_LINES, down, len, t.height);
        } else if let (Some(r), Some(lines)) =
            (l.right.filter(|r| contains(*r, x, y)), l.right_lines())
        {
            let v = self.top_mut();
            v.scroll = scroll_by(v.scroll, WHEEL_LINES, down, lines, r.height);
        }
    }

    /// Selects `item`; the views start at their top when what the right pane is about
    /// changes (another milestone, or another standalone task).
    fn select(&mut self, board: &Board, item: Item) {
        let was = self.selection(board).and_then(|s| pane_of(board, s));
        if pane_of(board, item) != was {
            self.views.iter_mut().for_each(|v| v.scroll = 0);
        }
        self.selected = Some(item);
    }

    /// Scrolls the tree so the selected rows show, and the overview so a selected task's
    /// card shows.
    fn reveal(&mut self, view: &View, area: Rect) {
        let l = layout(view, self, area);
        let Some(sel) = view.as_ref().ok().and_then(|b| self.tree_selection(b)) else {
            return;
        };
        let rows: Vec<usize> = (l.tree_items.iter().enumerate())
            .filter(|(_, it)| **it == Some(sel))
            .map(|(i, _)| i)
            .collect();
        if let (Some(t), Some(&first), Some(&last)) = (l.tree, rows.first(), rows.last()) {
            self.tree_scroll = into_view(self.tree_scroll, first, last, t.height);
        }
        // The card of the task itself, even while the tree shows it as its milestone.
        if let (Some(Item::Task(id)), Some(r), Some(o)) = (self.selected, l.right, &l.overview)
            && let Some(card) = o.cards.iter().find(|c| c.task == id)
        {
            let last = card.line + card.height - 1;
            let v = self.top_mut();
            v.scroll = into_view(v.scroll, card.line, last, r.height);
        }
    }
}

/// Each inbox item with its first and last list line, in list order, from a layout's
/// `rows`.
fn inbox_items(rows: &[Option<i64>]) -> Vec<(i64, usize, usize)> {
    let mut items: Vec<(i64, usize, usize)> = Vec::new();
    for (line, row) in rows.iter().enumerate() {
        match (row, items.last_mut()) {
            (Some(id), Some((last, _, end))) if last == id => *end = line,
            (Some(id), _) => items.push((*id, line, line)),
            (None, _) => {}
        }
    }
    items
}

/// The inbox list paged `down` or up to `scroll`, moved on the way it went (or, at its end,
/// back) to the nearest item's first line where no item's first line is in view, so the
/// item the selection follows to always shows its first line. Items take at most two lines
/// and at most four lines (heads, gaps, "nothing") come before an item's first, so a list
/// of five lines or more always shows one and only a shorter list moves.
fn page_to_item(scroll: usize, down: bool, ib: &InboxLayout) -> usize {
    let shown = scroll..scroll + usize::from(ib.list.height);
    let items = inbox_items(&ib.rows);
    if items.iter().any(|(_, a, _)| shown.contains(a)) {
        return scroll;
    }
    let below = items.iter().find(|(_, a, _)| *a >= scroll);
    let above = items.iter().rev().find(|(_, a, _)| *a < scroll);
    let next = if down {
        below.or(above)
    } else {
        above.or(below)
    };
    next.map_or(scroll, |&(_, a, _)| {
        clamp_scroll(
            into_view(scroll, a, a, ib.list.height),
            ib.rows.len(),
            ib.list.height,
        )
    })
}

/// A page: the pane's height less one line, so a line of context stays.
fn page(height: u16) -> usize {
    usize::from(height).saturating_sub(1).max(1)
}

/// The scroll that shows lines `first..=last` in a pane `height` tall, moving as little as
/// it can from `scroll`; the top line wins when they do not all fit.
fn into_view(scroll: usize, first: usize, last: usize, height: u16) -> usize {
    let h = usize::from(height).max(1);
    if first < scroll || last - first >= h {
        first
    } else if last >= scroll + h {
        last + 1 - h
    } else {
        scroll
    }
}

/// `scroll` moved by `by` lines, kept between the top and the last full page.
fn scroll_by(scroll: usize, by: usize, down: bool, len: usize, height: u16) -> usize {
    let moved = if down {
        scroll.saturating_add(by)
    } else {
        scroll.saturating_sub(by)
    };
    clamp_scroll(moved, len, height)
}

/// The repos, milestones and Done groups (those with a milestone in them) folded on
/// `board`.
fn folds_of(board: &Board) -> Vec<Fold> {
    (board.repos.iter())
        .filter(|r| r.folded)
        .map(|r| Fold::Repo(r.id))
        .chain(
            (board.milestones.iter())
                .filter(|m| m.folded)
                .map(|m| Fold::Milestone(m.id)),
        )
        .chain(
            (board.repos.iter())
                .filter(|r| r.done_folded && !board.done_of(r.id).is_empty())
                .map(|r| Fold::Done(r.id)),
        )
        .collect()
}

/// The fold a folded row is.
fn fold_of(row: Item) -> Option<Fold> {
    match row {
        Item::Repo(r) => Some(Fold::Repo(r)),
        Item::Milestone(m) => Some(Fold::Milestone(m)),
        Item::Done(r) => Some(Fold::Done(r)),
        Item::Task(_) => None,
    }
}
