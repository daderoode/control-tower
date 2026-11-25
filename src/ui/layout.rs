//! Where each pane goes and what a click lands on, worked out without styling anything.

use ratatui::layout::Rect;

use super::control::{Control, ControlAt, control_at};
use super::dash::{Dash, Item, ViewKind, board_of, milestone_of, pane_of, repo_of};
use super::detail::{DetailLayout, detail_layout};
use super::full_board::{BoardLayout, board_layout, shown};
use super::inbox::{InboxLayout, alert_parts, inbox_layout};
use super::message_board::{self, Limits, ThreadRow};
use super::overview::{OverviewLayout, overview_layout, repo_overview_layout};
use super::theme::Depth;
use super::tree::{FoldClick, tree_folds, tree_items, tree_tickets};
use super::{Mode, View};
use crate::board::{Board, MilestoneId, Talk, TaskId};

/// The tree takes this share of the width, as in the signed design...
const TREE_PERCENT: usize = 34;
/// ...but never fewer columns than this, so a milestone row keeps its bar.
const TREE_MIN: usize = 30;
/// In the popup the tree is narrower, as in the reference's compact profile, leaving the
/// overview more of the smaller popup...
const POPUP_TREE_PERCENT: usize = 27;
/// ...but never fewer columns than this, still room for a milestone's bar.
const POPUP_TREE_MIN: usize = 24;
/// The overview shows beside the tree only when it gets at least this many columns; below
/// that the tree takes the whole width and Enter opens the overview in its place.
const RIGHT_MIN: usize = 30;
/// The clock at the end of the first line, " 14:32 ".
const CLOCK_COLUMNS: u16 = 7;
/// The clock shows only in a pane at least this wide; narrower, the line keeps its room.
const CLOCK_MIN: u16 = 40;

/// The panes of one frame's body: the tree, the overview of the selection, or both side by
/// side. Each pane's rows: the item each tree row selects, the overview's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub tree: Option<Rect>,
    pub tree_items: Vec<Option<Item>>,
    pub right: Option<Rect>,
    /// The overview's layout, while the right pane shows the overview, its `lines`
    /// including the message board summary under it.
    pub overview: Option<OverviewLayout>,
    /// The detail view's layout, while the right pane shows a detail view.
    pub detail: Option<DetailLayout>,
    /// The tree takes the whole width and hides an overview (or a standalone task's
    /// detail) that Enter would open.
    pub overview_hidden: bool,
    /// The thread boxes in the right pane's view, by line, for opening the full board.
    pub threads: Vec<ThreadRow>,
    /// The RESOLVED row of the right pane's board, by line: a click folds or unfolds it.
    pub resolved: Option<usize>,
    /// The full message board's layout, while it is open across the body.
    pub board: Option<BoardLayout>,
    /// The selection's repo has a message board (a lead, or threads) that `b` opens.
    pub has_board: bool,
    /// The alert line under the header, while the tree's view shows it; a click opens the
    /// inbox.
    pub alert: Option<Rect>,
    /// "new lead (n) ▶" on the alert line, while it shows there.
    pub new_lead: Option<Rect>,
    /// The inbox's layout, while it is open across the body.
    pub inbox: Option<InboxLayout>,
    /// The clock at the right end of the first line: the alert line's, or the top line of
    /// the inbox or the full board.
    pub clock: Option<Rect>,
}

/// The body area of a frame: under the alert line, above the rule and keys line, or the
/// whole frame in a pane too short for them.
pub(crate) fn body_area(area: Rect) -> Rect {
    if area.height < 5 {
        area
    } else {
        Rect::new(area.x, area.y + 1, area.width, area.height - 3)
    }
}

/// The body of a view across the frame with a top line of its own (the inbox, the full
/// board): from the first line, where the alert line would be, down to the rule.
fn top_body(area: Rect) -> Rect {
    if area.height < 5 {
        area
    } else {
        Rect::new(area.x, area.y, area.width, area.height - 2)
    }
}

/// Where the clock goes: the right end of the first line, in a pane tall enough for the
/// rule and keys line and wide enough to keep room for the line it ends.
pub(crate) fn clock_area(area: Rect) -> Option<Rect> {
    (area.height >= 5 && area.width >= CLOCK_MIN).then(|| {
        Rect::new(
            area.x + area.width - CLOCK_COLUMNS,
            area.y,
            CLOCK_COLUMNS,
            1,
        )
    })
}

/// `top`, a first line, short of the clock when it shows.
fn before_clock(top: Rect, clock: Option<Rect>) -> Rect {
    match clock {
        Some(c) if top.y == c.y => Rect {
            width: top.width.saturating_sub(c.width),
            ..top
        },
        _ => top,
    }
}

/// Lays out a frame of size `area`. With something selected, the overview shows beside a
/// tree of `TREE_PERCENT` (at least `TREE_MIN`) when it gets `RIGHT_MIN` columns. In a
/// narrower pane the tree takes the whole width, until a view is opened on top of it, which
/// then takes the whole width instead. A problem reading the board leaves no panes: it is
/// drawn across the body.
pub fn layout(view: &View, dash: &Dash, area: Rect) -> Layout {
    let body = body_area(area);
    let clock = clock_area(area);
    let Ok(board) = view else {
        return Layout {
            tree: None,
            tree_items: Vec::new(),
            right: None,
            overview: None,
            detail: None,
            overview_hidden: false,
            threads: Vec::new(),
            resolved: None,
            board: None,
            has_board: false,
            alert: None,
            new_lead: None,
            inbox: None,
            clock,
        };
    };
    let sel = dash.selection(board);
    let pane = sel.and_then(|s| pane_of(board, s));
    let talk = sel.and_then(|s| board_of(board, s));
    let has_board = (talk.as_ref()).is_some_and(|t| t.lead.is_some() || !t.threads.is_empty());
    // The full board takes the whole body, over the tree and the right pane.
    if let (ViewKind::Board, Some(talk)) = (dash.view(), &talk) {
        return Layout {
            tree: None,
            tree_items: tree_items(board, usize::from(body.width)),
            right: None,
            overview: None,
            detail: None,
            overview_hidden: false,
            threads: Vec::new(),
            resolved: None,
            board: Some(board_layout(
                talk,
                dash.board_thread_wanted(),
                dash.board_fold(),
                dash.open_messages(),
                dash.replying().is_some(),
                shown(talk, dash.board_thread_wanted()).map_or("", |t| dash.draft(t.id)),
                dash.minute(),
                top_body(area),
            ))
            .map(|b| BoardLayout {
                top: before_clock(b.top, clock),
                ..b
            }),
            has_board,
            alert: None,
            new_lead: None,
            inbox: None,
            clock,
        };
    }
    if dash.view() == ViewKind::Inbox {
        return Layout {
            tree: None,
            tree_items: tree_items(board, usize::from(body.width)),
            right: None,
            overview: None,
            detail: None,
            overview_hidden: false,
            threads: Vec::new(),
            resolved: None,
            board: None,
            has_board,
            alert: None,
            new_lead: None,
            inbox: Some(inbox_layout(board, top_body(area))).map(|ib| InboxLayout {
                top: before_clock(ib.top, clock),
                ..ib
            }),
            clock,
        };
    }
    let w = usize::from(body.width);
    let (percent, min) = match dash.mode {
        Mode::Space => (TREE_PERCENT, TREE_MIN),
        Mode::Popup => (POPUP_TREE_PERCENT, POPUP_TREE_MIN),
    };
    let tree_w = (w * percent / 100).max(min);
    let (tree, right) = if pane.is_some() && w >= tree_w + 1 + RIGHT_MIN {
        let tw = tree_w as u16;
        (
            Some(Rect::new(body.x, body.y, tw, body.height)),
            Some(Rect::new(
                body.x + tw + 1,
                body.y,
                body.width - tw - 1,
                body.height,
            )),
        )
    } else if pane.is_some() && (dash.across() || dash.views() > 1) {
        (None, Some(body))
    } else {
        (Some(body), None)
    };
    // The right pane's layout follows the view it shows: the overview or a detail view.
    let (mut overview, mut detail, mut threads, mut resolved) = (None, None, Vec::new(), None);
    if let (Some(r), Some(sel)) = (right, sel) {
        let rw = usize::from(r.width);
        match (dash.shown(board), overview_of(board, sel)) {
            (ViewKind::Overview, Some((ov, talk, task))) => {
                let limits = Limits::of(dash.mode, dash.board_shown(sel), dash.minute());
                let mut o = match ov {
                    Over::Repo(r) => repo_overview_layout(board, r, rw),
                    Over::Done(r) => super::overview::done_overview_layout(board, r, rw),
                    Over::Milestone(m) => overview_layout(m, task, rw),
                };
                let m = &talk;
                threads = (message_board::thread_rows(m, task, rw, limits).into_iter())
                    .map(|t| ThreadRow {
                        line: t.line + o.lines,
                        ..t
                    })
                    .collect();
                let controls = [
                    message_board::board_control(m, task, rw),
                    message_board::more_control(m, task, rw, limits),
                ];
                for c in controls.into_iter().flatten() {
                    o.controls.push(ControlAt {
                        line: c.line + o.lines,
                        ..c
                    });
                }
                resolved = message_board::resolved_row(m, task, rw, limits).map(|l| l + o.lines);
                o.lines += message_board::height(m, task, rw, limits);
                overview = Some(o);
            }
            (ViewKind::Overview, None) => {}
            (kind, _) => {
                detail = detail_layout(board, kind, sel, rw, dash.board_shown(sel), dash.minute());
                threads = (detail.as_ref())
                    .map(|d| d.threads.clone())
                    .unwrap_or_default();
                resolved = detail.as_ref().and_then(|d| d.resolved);
            }
        }
    }
    Layout {
        tree,
        tree_items: tree_items(board, usize::from(tree.map_or(body.width, |t| t.width))),
        right,
        overview,
        detail,
        overview_hidden: pane.is_some() && right.is_none(),
        threads,
        resolved,
        board: None,
        has_board,
        alert: alert_area(area),
        new_lead: new_lead_area(board, area),
        inbox: None,
        clock,
    }
}

/// What an overview is of: a repo, its Done group, or a milestone.
pub(crate) enum Over<'a> {
    Repo(&'a crate::board::Repo),
    Done(&'a crate::board::Repo),
    Milestone(&'a crate::board::Milestone),
}

/// The overview for `sel`, the board under it and the task it narrows to: a repo's with
/// its whole board, a Done group's with the threads of its milestones, or a milestone's (for it or one of its tasks) with its part of the
/// board. `None` for a standalone task, which has no overview.
pub(crate) fn overview_of(
    board: &Board,
    sel: Item,
) -> Option<(Over<'_>, Talk<'_>, Option<crate::board::TaskId>)> {
    match sel {
        Item::Repo(_) => {
            let r = repo_of(board, sel)?;
            Some((Over::Repo(r), Talk::of_repo(board, r), None))
        }
        Item::Done(_) => {
            let r = repo_of(board, sel)?;
            Some((Over::Done(r), Talk::of_done(board, r), None))
        }
        Item::Milestone(_) | Item::Task(_) => {
            let m = milestone_of(board, sel)?;
            let task = match sel {
                Item::Task(id) => Some(id),
                _ => None,
            };
            Some((Over::Milestone(m), Talk::of_milestone(board, m), task))
        }
    }
}

/// The alert line's row: the first, up to the clock, in a pane tall enough for the rule
/// and keys line.
pub(crate) fn alert_area(area: Rect) -> Option<Rect> {
    let a = (area.height >= 5).then(|| Rect::new(area.x, area.y, area.width, 1))?;
    Some(before_clock(a, clock_area(area)))
}

/// Where "new lead (n) ▶" is on the alert line, if it shows. Colours do not move it, so
/// any depth lays it out.
fn new_lead_area(board: &crate::board::Board, area: Rect) -> Option<Rect> {
    let a = alert_area(area)?;
    // Where the controls are never depends on the newest item's age: it is reserved at its
    // widest either way.
    let (_, at) = alert_parts(board, usize::from(a.width), None, Depth::Ansi256);
    let (from, to) = at?;
    Some(Rect::new(a.x + from as u16, a.y, (to - from) as u16, 1))
}

impl Layout {
    /// How many lines the view in the right pane has, for scrolling it.
    pub fn right_lines(&self) -> Option<usize> {
        (self.overview.as_ref().map(|o| o.lines)).or(self.detail.as_ref().map(|d| d.lines))
    }

    /// The task whose row in the milestone detail is under a click at (`x`, `y`).
    pub fn detail_row(&self, dash: &Dash, x: u16, y: u16) -> Option<crate::board::TaskId> {
        let r = self.right.filter(|r| contains(*r, x, y))?;
        let d = self.detail.as_ref()?;
        let scroll = clamp_scroll(dash.right_scroll(), d.lines, r.height);
        d.task_at(usize::from(y - r.y) + scroll)
    }

    /// The milestone whose line in the repo detail is under a click at (`x`, `y`).
    pub fn detail_milestone(&self, dash: &Dash, x: u16, y: u16) -> Option<MilestoneId> {
        let r = self.right.filter(|r| contains(*r, x, y))?;
        let d = self.detail.as_ref()?;
        let scroll = clamp_scroll(dash.right_scroll(), d.lines, r.height);
        d.milestone_at(usize::from(y - r.y) + scroll)
    }

    /// What a click at (`x`, `y`) in the right pane's view lands on in its board: a "read
    /// more" or "show less" line (its `toggle`) before the thread box around it, or a
    /// question of the "Needs you" strip (its `message`).
    pub fn thread_at(&self, dash: &Dash, x: u16, y: u16) -> Option<ThreadRow> {
        let r = self.right.filter(|r| contains(*r, x, y))?;
        let lines = self.right_lines()?;
        let line = usize::from(y - r.y) + clamp_scroll(dash.right_scroll(), lines, r.height);
        let under = |t: &&ThreadRow| (t.line..t.line + t.height).contains(&line);
        (self
            .threads
            .iter()
            .filter(|t| t.toggle.is_some())
            .find(under))
        .or_else(|| self.threads.iter().find(under))
        .copied()
    }

    /// Whether a click at (`x`, `y`) lands on the RESOLVED row of the right pane's board.
    pub fn resolved_at(&self, dash: &Dash, x: u16, y: u16) -> bool {
        let Some(r) = self.right.filter(|r| contains(*r, x, y)) else {
            return false;
        };
        let Some(lines) = self.right_lines() else {
            return false;
        };
        let line = usize::from(y - r.y) + clamp_scroll(dash.right_scroll(), lines, r.height);
        self.resolved == Some(line)
    }

    /// The control in the right pane's view under a click at (`x`, `y`): on the overview,
    /// "open milestone ▶" (or "open task ▶") and "open full board ▶"; on a detail view,
    /// "◀ close".
    pub fn control_at(&self, dash: &Dash, x: u16, y: u16) -> Option<Control> {
        let r = self.right.filter(|r| contains(*r, x, y))?;
        let lines = self.right_lines()?;
        let controls = (self.overview.as_ref().map(|o| &o.controls))
            .or(self.detail.as_ref().map(|d| &d.controls))?;
        let line = usize::from(y - r.y) + clamp_scroll(dash.right_scroll(), lines, r.height);
        control_at(controls, usize::from(x - r.x), line)
    }

    /// What a click at (`x`, `y`) on the tree of `board` folds or unfolds: a milestone's
    /// fold mark, or a folded milestone's summary line.
    pub fn fold_at(&self, board: &Board, dash: &Dash, x: u16, y: u16) -> Option<FoldClick> {
        let t = self.tree.filter(|t| contains(*t, x, y))?;
        let scroll = clamp_scroll(dash.tree_scroll, self.tree_items.len(), t.height);
        let folds = tree_folds(board, usize::from(t.width));
        match folds.get(usize::from(y - t.y) + scroll).copied()? {
            Some(FoldClick::Mark(_, columns)) if x >= t.x + columns => None,
            fold => fold,
        }
    }

    /// The task whose ticket a click at (`x`, `y`) is on, in the tree.
    pub fn ticket_at(&self, board: &Board, dash: &Dash, x: u16, y: u16) -> Option<TaskId> {
        let t = self.tree.filter(|t| contains(*t, x, y))?;
        let scroll = clamp_scroll(dash.tree_scroll, self.tree_items.len(), t.height);
        let tickets = tree_tickets(board, usize::from(t.width));
        let (id, columns) = tickets.get(usize::from(y - t.y) + scroll).cloned()??;
        columns.contains(&(x - t.x)).then_some(id)
    }

    /// The item under a click at (`x`, `y`): a tree row, or a card in the overview.
    pub fn hit(&self, dash: &Dash, x: u16, y: u16) -> Option<Item> {
        if let Some(t) = self.tree.filter(|t| contains(*t, x, y)) {
            let scroll = clamp_scroll(dash.tree_scroll, self.tree_items.len(), t.height);
            return self
                .tree_items
                .get(usize::from(y - t.y) + scroll)
                .copied()?;
        }
        let r = self.right.filter(|r| contains(*r, x, y))?;
        let o = self.overview.as_ref()?;
        let scroll = clamp_scroll(dash.right_scroll(), o.lines, r.height);
        let line = usize::from(y - r.y) + scroll;
        (o.card_at(usize::from(x - r.x), line).map(Item::Task))
            .or(o.milestone_at(line).map(Item::Milestone))
    }
}

pub(crate) fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Keeps a scroll within the content: never past the point where the last line is at the
/// bottom of the pane.
pub(crate) fn clamp_scroll(scroll: usize, len: usize, height: u16) -> usize {
    scroll.min(len.saturating_sub(usize::from(height)))
}
