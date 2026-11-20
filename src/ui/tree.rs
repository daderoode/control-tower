//! The tree, as UI v0.1.7 draws it: each repo heads its own part, under a rule from the
//! one above. A repo's row is its fold mark and name, with its lead's badges at the right,
//! "⚠1" need-you items in red and "ⓘ1" unread awareness items, and "N blocked" in amber
//! while any of its tasks is; its lead's name sits under it in the accent colour (or "no
//! lead yet"). Then the repo's milestones, each with its tasks, then its standalone tasks
//! under a dim "standalone · no milestone" label. Task rows are quiet: a dim title, only an
//! In progress one in normal text, its stage mark, an amber "!" while blocked and its id at
//! the right (a standalone task adds its assignee and done subtasks). A blank line gives
//! each milestone and the standalone label air.
//!
//! A repo or milestone folds: ▾ while its contents show, ▸ while the owner folded them away.
//! A folded repo keeps its row and lead line and shows one summary line, "2 milestones · 3
//! standalone !1"; a folded milestone shows "4 tasks ✓2 ●1 ○1 !1". Nothing urgent hides.
//!
//! Milestones and standalone tasks in no repo on the board stay visible in a last group
//! under a rule: one "No repo yet" label and a dim line saying how to give each its repo,
//! then its milestones with their tasks and its standalone tasks.
//!
//! A repo's milestones with the lead's Done mark are not among them: they sit in its Done
//! group, one row at the bottom of the repo, "▸ Done  2 milestones" with the badges of the
//! need-you and unread awareness items on their threads at the right. It is folded until
//! the owner unfolds it; unfolded, a line per milestone, most recently done first: "✓ M3
//! title" with the date it was marked Done at the right. A folded repo's summary line
//! counts its Done milestones too, "✓2 Done".
//!
//! A milestone's or task's title is shown whole, wrapping to an indented second line; it is
//! cut with "…" only past two lines.
//!
//! `rows` is the tree's structure; the layout reads the item each row selects from it and
//! the drawing reads each row's lines, so the two cannot disagree.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::dash::Item;
use super::message_board::{IDEOGRAPHIC_SPACE, clamp, wrap_at_most};
use super::theme::{Depth, accent, bad, bar, bold, dim, fit, ok, stage_mark, warn};
use crate::board::{Board, Milestone, MilestoneId, Repo, RepoId, Stage, Task, TaskId};

/// Cells in a milestone's progress bar in the tree, as in the golden UI.
const BAR_CELLS: usize = 8;
/// Columns a repo's name keeps, " ▾ c…" at least, before its badges are dropped.
const MIN_LEFT: usize = 5;
/// Where a milestone's line starts under its repo: " ▾ " for the repo, two more columns.
const MILESTONE_AT: usize = 3;
/// A task's stage mark starts this far in, under its milestone's title...
const TASK_AT: usize = 5;
/// ...and its title, and its second line, this far, after "○ ".
const TASK_INDENT: usize = TASK_AT + 2;
/// A click on a repo's first line within this many columns, " ▾ ", is on its fold mark...
pub const REPO_FOLD_COLUMNS: u16 = 3;
/// ...and on a milestone's within these, "   ▾ ".
pub const FOLD_COLUMNS: u16 = (MILESTONE_AT + 2) as u16;

/// What a fold folds: a repo, a milestone or a repo's Done group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Repo(RepoId),
    Milestone(MilestoneId),
    Done(RepoId),
}

/// One row of the tree.
enum Row<'a> {
    /// The hint on an empty board.
    Empty,
    Blank,
    /// The rule above each repo but the first, and above the "No repo yet" group.
    Rule,
    /// A repo's first line, with its badges and blocked count.
    Repo(&'a Repo, (usize, usize), usize),
    /// A repo's second line: its lead, or that it has none.
    RepoLead(&'a Repo),
    /// A folded repo's contents, in one line: its milestones, standalone tasks and blocked.
    RepoFolded(&'a Board, RepoId),
    /// A repo with no milestone and no standalone task.
    NoWork,
    Milestone(&'a Milestone),
    /// A folded milestone's tasks, in one line.
    Folded(&'a Milestone),
    NoTasks,
    Task(&'a Task),
    /// The dim label over a repo's standalone tasks (`None`: those in no repo), with their
    /// done count.
    Standalone(&'a Board, Option<RepoId>),
    StandaloneTask(&'a Task),
    /// A repo's Done group: the repo, its badges and how many milestones it holds.
    DoneGroup(&'a Repo, (usize, usize), usize),
    /// A milestone in an unfolded Done group.
    DoneLine(&'a Milestone),
    /// The head of the group of what is in no repo on the board.
    Unplaced,
    /// The line under it saying how to give each item its repo.
    UnplacedHint,
}

fn rows(board: &Board) -> Vec<Row<'_>> {
    let inbox = &board.inbox;
    let mut rows = Vec::new();
    if board.repos.is_empty() && !board.has_unplaced() {
        rows.push(Row::Empty);
    }
    for (i, r) in board.repos.iter().enumerate() {
        // More air between repos: a rule, then a blank line.
        if i > 0 {
            rows.extend([Row::Rule, Row::Blank]);
        }
        rows.push(Row::Repo(
            r,
            inbox.badges(r.id),
            board.blocked_in(Some(r.id)),
        ));
        rows.push(Row::RepoLead(r));
        if r.folded {
            rows.push(Row::RepoFolded(board, r.id));
            continue;
        }
        let mut under_lead = true;
        for m in board.active_of(Some(r.id)) {
            if !std::mem::take(&mut under_lead) {
                rows.push(Row::Blank);
            }
            milestone_rows(&mut rows, m);
        }
        if board.standalone_of(Some(r.id)).next().is_some() {
            if !std::mem::take(&mut under_lead) {
                rows.push(Row::Blank);
            }
            rows.push(Row::Standalone(board, Some(r.id)));
            rows.extend(board.standalone_of(Some(r.id)).map(Row::StandaloneTask));
        }
        let done = board.done_of(r.id);
        if !done.is_empty() {
            if !std::mem::take(&mut under_lead) {
                rows.push(Row::Blank);
            }
            rows.push(Row::DoneGroup(r, board.done_badges(r.id), done.len()));
            if !r.done_folded {
                rows.extend(done.into_iter().map(Row::DoneLine));
            }
        }
        if under_lead {
            rows.push(Row::NoWork);
        }
    }
    if board.has_unplaced() {
        if !board.repos.is_empty() {
            rows.extend([Row::Rule, Row::Blank]);
        }
        rows.push(Row::Unplaced);
        rows.push(Row::UnplacedHint);
        for m in board.milestones_of(None) {
            rows.push(Row::Blank);
            milestone_rows(&mut rows, m);
        }
        if board.standalone_of(None).next().is_some() {
            rows.push(Row::Blank);
            rows.extend(board.standalone_of(None).map(Row::StandaloneTask));
        }
    }
    rows
}

/// A milestone's line and, unless folded, its tasks.
fn milestone_rows<'a>(rows: &mut Vec<Row<'a>>, m: &'a Milestone) {
    rows.push(Row::Milestone(m));
    if m.folded {
        rows.push(Row::Folded(m));
        return;
    }
    if m.tasks.is_empty() {
        rows.push(Row::NoTasks);
    }
    rows.extend(m.tasks.iter().map(Row::Task));
}

impl Row<'_> {
    /// What a click on the row selects: a repo's two lines select the repo, a milestone's
    /// line and the line above it in no repo select the milestone.
    fn item(&self) -> Option<Item> {
        match self {
            Row::Repo(r, ..) | Row::RepoLead(r) => Some(Item::Repo(r.id)),
            Row::Milestone(m) => Some(Item::Milestone(m.id)),
            Row::Task(t) | Row::StandaloneTask(t) => Some(Item::Task(t.id)),
            Row::DoneGroup(r, ..) => Some(Item::Done(r.id)),
            Row::DoneLine(m) => Some(Item::Milestone(m.id)),
            Row::Empty
            | Row::Blank
            | Row::Rule
            | Row::RepoFolded(..)
            | Row::NoWork
            | Row::Folded(_)
            | Row::NoTasks
            | Row::Standalone(..)
            | Row::Unplaced
            | Row::UnplacedHint => None,
        }
    }

    /// What a click on the row folds or unfolds: a repo's or milestone's fold mark, or a
    /// folded repo's or milestone's summary line, which unfolds it.
    fn fold(&self) -> Option<FoldClick> {
        match self {
            Row::Repo(r, ..) => Some(FoldClick::Mark(Fold::Repo(r.id), REPO_FOLD_COLUMNS)),
            Row::RepoFolded(_, r) => Some(FoldClick::Line(Fold::Repo(*r))),
            Row::Milestone(m) => Some(FoldClick::Mark(Fold::Milestone(m.id), FOLD_COLUMNS)),
            Row::Folded(m) => Some(FoldClick::Line(Fold::Milestone(m.id))),
            Row::DoneGroup(r, ..) => Some(FoldClick::Mark(Fold::Done(r.id), FOLD_COLUMNS)),
            _ => None,
        }
    }

    /// The row's lines, each exactly `w` columns: one, or two for a title that wraps.
    fn lines(&self, w: usize, depth: Depth, selected: bool) -> Vec<Line<'static>> {
        match self {
            Row::Milestone(m) => milestone_lines(m, w, depth),
            Row::Task(t) => {
                let mut right = blocked_mark(t, depth);
                right.extend([Span::raw(" "), ticket_span(t, depth), Span::raw(" ")]);
                task_lines(t, right, w, depth, selected)
            }
            Row::StandaloneTask(t) => standalone_lines(t, w, depth, selected),
            Row::DoneLine(m) => done_lines(m, w, depth),
            _ => vec![self.line(w, depth)],
        }
    }

    fn line(&self, w: usize, depth: Depth) -> Line<'static> {
        match self {
            Row::Empty => fit(
                vec![Span::styled(
                    " no repos yet: add one with tower repo add",
                    dim(depth),
                )],
                w,
            ),
            Row::Blank => fit(Vec::new(), w),
            Row::Rule => Line::styled("─".repeat(w), dim(depth)),
            Row::Repo(r, badges, blocked) => {
                let mark = if r.folded { " ▸ " } else { " ▾ " };
                let left = vec![
                    Span::styled(mark, dim(depth)),
                    Span::styled(r.name(), bold()),
                ];
                let mut right = badge_spans(*badges, depth);
                if *blocked > 0 {
                    right.push(Span::styled(
                        format!(" {blocked} blocked"),
                        Style::new().fg(warn(depth)),
                    ));
                }
                if !right.is_empty() {
                    right.push(Span::raw(" "));
                }
                split_line(left, right, MIN_LEFT, w)
            }
            Row::RepoLead(r) => match &r.lead {
                Some(l) => fit(
                    vec![
                        Span::raw("   "),
                        Span::styled(l.name.clone(), Style::new().fg(accent(depth))),
                    ],
                    w,
                ),
                None => fit(
                    vec![Span::styled("   no lead yet: n starts one", dim(depth))],
                    w,
                ),
            },
            Row::RepoFolded(board, r) => repo_folded_line(board, *r, w, depth),
            Row::NoWork => fit(
                vec![Span::styled("   no milestones or tasks yet", dim(depth))],
                w,
            ),
            Row::NoTasks => fit(
                vec![Span::styled(
                    format!("{}no tasks yet", " ".repeat(TASK_AT)),
                    dim(depth),
                )],
                w,
            ),
            Row::Folded(m) => folded_line(m, w, depth),
            Row::Standalone(board, repo) => {
                let tasks: Vec<&Task> = board.standalone_of(*repo).collect();
                let done = tasks.iter().filter(|t| t.stage == Stage::Done).count();
                let left = vec![Span::styled(
                    format!("{}standalone · no milestone", " ".repeat(MILESTONE_AT)),
                    dim(depth),
                )];
                let right = vec![Span::styled(
                    format!(" {done}/{} done ", tasks.len()),
                    dim(depth),
                )];
                split_line(left, right, MILESTONE_AT + "standalone".len(), w)
            }
            Row::DoneGroup(r, badges, n) => {
                let mark = if r.done_folded { "▸ " } else { "▾ " };
                let left = vec![
                    Span::raw(" ".repeat(MILESTONE_AT)),
                    Span::styled(mark, dim(depth)),
                    Span::styled("Done", bold().fg(ok(depth))),
                    Span::styled(format!("  {}", milestones(*n)), dim(depth)),
                ];
                let mut right = badge_spans(*badges, depth);
                if !right.is_empty() {
                    right.push(Span::raw(" "));
                }
                split_line(left, right, usize::from(FOLD_COLUMNS) + "Done".len(), w)
            }
            Row::Unplaced => fit(vec![Span::styled(" No repo yet", bold())], w),
            Row::UnplacedHint => fit(
                vec![Span::styled(
                    "   tower milestone set / task set --repo R<n>",
                    dim(depth),
                )],
                w,
            ),
            Row::Milestone(_) | Row::Task(_) | Row::StandaloneTask(_) | Row::DoneLine(_) => {
                unreachable!("drawn by Row::lines")
            }
        }
    }
}

/// " ⚠1" need-you items in red and " ⓘ1" unread awareness items, each only when there
/// are any.
pub(crate) fn badge_spans((need, aware): (usize, usize), depth: Depth) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if need > 0 {
        spans.push(Span::styled(format!(" ⚠{need}"), bold().fg(bad(depth))));
    }
    if aware > 0 {
        spans.push(Span::styled(format!(" ⓘ{aware}"), bold()));
    }
    spans
}

/// "1 milestone" or "N milestones".
pub(crate) fn milestones(n: usize) -> String {
    format!("{n} {}", if n == 1 { "milestone" } else { "milestones" })
}

/// The day `at` (seconds since the Unix epoch) falls on where the dashboard runs, as
/// DD/MM/YYYY; "date not recorded" for a Done mark set before the board kept its date.
pub(crate) fn done_day(at: Option<i64>) -> String {
    use chrono::TimeZone;
    match at.and_then(|at| chrono::Local.timestamp_opt(at, 0).single()) {
        Some(day) => day.format("%d/%m/%Y").to_string(),
        None => "date not recorded".into(),
    }
}

/// What a Done milestone's stage line says about its mark: "marked Done 14/01/2027", or
/// "marked Done, date not recorded".
pub(crate) fn marked_done(at: Option<i64>) -> String {
    match at {
        Some(_) => format!("marked Done {}", done_day(at)),
        None => "marked Done, date not recorded".into(),
    }
}

/// A Done milestone's tally in the repo and Done group lists, as UI v0.1.7's milestone
/// line: its tasks, all Done, and the day it was marked, "2/2 · 14/01/2027".
pub(crate) fn done_tally(m: &Milestone) -> String {
    let n = m.tasks.len();
    format!("{n}/{n} · {}", done_day(m.done_at))
}

/// A Done line: "     ✓ M3 title" dim, wrapping to an indented second line, with the
/// date it was marked Done at the right, dropped when the pane is too narrow for it.
fn done_lines(m: &Milestone, w: usize, depth: Depth) -> Vec<Line<'static>> {
    let right = vec![Span::styled(
        format!(" {} ", done_day(m.done_at)),
        dim(depth),
    )];
    let min_left = TASK_INDENT + 6;
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    let (right, right_w) = if w < min_left + right_w {
        (Vec::new(), 0)
    } else {
        (right, right_w)
    };
    let (head, rest) = two_lines(
        &format!("{} {}", m.id, m.title),
        w.saturating_sub(TASK_INDENT + right_w),
        w.saturating_sub(TASK_INDENT),
    );
    let left = vec![
        Span::raw(" ".repeat(TASK_AT)),
        stage_mark(Stage::Done, depth),
        Span::raw(" "),
        Span::styled(head, dim(depth)),
    ];
    let mut lines = vec![split_line(left, right, min_left, w)];
    if let Some(rest) = rest {
        lines.push(fit(
            vec![
                Span::raw(" ".repeat(TASK_INDENT)),
                Span::styled(rest, dim(depth)),
            ],
            w,
        ));
    }
    lines
}

/// An amber " !" while `t` is blocked, else nothing.
fn blocked_mark(t: &Task, depth: Depth) -> Vec<Span<'static>> {
    match t.details.blocked {
        Some(_) => vec![Span::styled(" !", bold().fg(warn(depth)))],
        None => Vec::new(),
    }
}

/// `text` on one line of `first` columns, or two: the first takes whole words while they
/// fit (a word longer than the line is broken only when it is the first), and the rest
/// goes on a second line of `second` columns, clamped there as the copy's `.clamp2` ends
/// its last line (`clamp`).
fn two_lines(text: &str, first: usize, second: usize) -> (String, Option<String>) {
    if text.width() <= first {
        return (text.to_string(), None);
    }
    let mut end = 0;
    for (at, _) in text.match_indices(' ').chain([(text.len(), "")]) {
        if text[..at].trim_end().width() > first {
            break;
        }
        if !ends_carried(&text[..at]) {
            end = at;
        }
    }
    if text[..end].trim().is_empty() {
        let mut lines = wrap_at_most(text, first.max(1), 2).into_iter();
        let head = lines.next().unwrap_or_default();
        return (head, lines.next().map(|rest| clamp_line(&rest, second)));
    }
    // A run of ideographic spaces starting the rest goes to the second line with it, as the
    // copy's folded text carries it, unless the head ends with one, which it hangs with.
    let head = text[..end].trim_end_matches(' ');
    let rest = text[end..].trim_start_matches(' ');
    let rest = if head.ends_with(IDEOGRAPHIC_SPACE) {
        rest.trim_start()
    } else {
        rest
    };
    let head = head.trim_end().to_string();
    (head, (!rest.is_empty()).then(|| clamp_line(rest, second)))
}

/// Whether `head` ends with words that are only ideographic spaces, one or more, after a
/// word that does not end with one, which the copy's folded text does not end a line with:
/// it carries them to the next line with what follows them (`message_board::wrap_line`).
fn ends_carried(head: &str) -> bool {
    let mut head = head.trim_end_matches(' ');
    let mut runs = 0;
    loop {
        let (before, word) = head.rsplit_once(' ').unwrap_or(("", head));
        if word.is_empty() || !word.chars().all(|c| c == IDEOGRAPHIC_SPACE) {
            return runs > 0 && !word.is_empty() && !word.ends_with(IDEOGRAPHIC_SPACE);
        }
        runs += 1;
        head = before.trim_end_matches(' ');
    }
}

/// `rest` on one line `w` wide, as the copy's `.clamp2` ends its last line (`clamp`).
fn clamp_line(rest: &str, w: usize) -> String {
    clamp(rest, w.max(1), 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// "     ○ title" with `right` at the right edge (dropped when the pane is too narrow to
/// show it beside the start of the title), the title wrapping to a second line indented
/// under it. The title is dim, as the whole row is quiet, except while In progress.
fn task_lines(
    t: &Task,
    right: Vec<Span<'static>>,
    w: usize,
    depth: Depth,
    selected: bool,
) -> Vec<Line<'static>> {
    // The title keeps a few columns beside `right`, or `right` goes.
    let min_left = TASK_INDENT + 6;
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    let right = if w < min_left + right_w {
        Vec::new()
    } else {
        right
    };
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    let room = w.saturating_sub(TASK_INDENT + right_w);
    let (head, rest) = two_lines(&t.title, room, w.saturating_sub(TASK_INDENT));
    // Quiet: only an in-progress task's title, or the selected one, in normal text.
    let title = match t.stage {
        _ if selected => Style::new(),
        Stage::InProgress => Style::new(),
        Stage::Waiting | Stage::Done => dim(depth),
    };
    let left = vec![
        Span::raw(" ".repeat(TASK_AT)),
        stage_mark(t.stage, depth),
        Span::raw(" "),
        Span::styled(head, title),
    ];
    let mut lines = vec![split_line(left, right, min_left, w)];
    if let Some(rest) = rest {
        lines.push(fit(
            vec![
                Span::raw(" ".repeat(TASK_INDENT)),
                Span::styled(rest, title),
            ],
            w,
        ));
    }
    lines
}

/// A standalone task's lines, as `task_lines`, with "!" while blocked, then its id, its
/// assignee (or "unassigned") and its done subtasks on the right, " #13 · dev-2 · 1/3 ".
/// Short of room the assignee and subtasks go first, then the id.
fn standalone_lines(t: &Task, w: usize, depth: Depth, selected: bool) -> Vec<Line<'static>> {
    let who = t.details.assignee.as_deref().unwrap_or("unassigned");
    let subs = match t.subtasks.len() {
        0 => String::new(),
        all => format!(" · {}/{all}", t.done_subtasks()),
    };
    // " #64 · dev-3 · 1/3 ": the ticket, then who and the subtasks, dim.
    let with = |rest: String| {
        let mut right = blocked_mark(t, depth);
        right.extend([Span::raw(" "), ticket_span(t, depth)]);
        right.push(Span::styled(rest, dim(depth)));
        right
    };
    let full = with(format!(" · {who}{subs} "));
    let full_w: usize = full.iter().map(|s| s.width()).sum();
    let right = if w >= TASK_INDENT + 6 + full_w {
        full
    } else {
        with(" ".into())
    };
    task_lines(t, right, w, depth, selected)
}

/// A task's ticket at the right of its row: the number its tracker link names, in
/// the accent colour, for `o` or a click to open; "no ticket", dim, for a task with no link,
/// so a missing ticket shows rather than being inferred. Tower's own task id is not on the
/// row: it stays in the task's detail and the CLI.
fn ticket_span(t: &Task, depth: Depth) -> Span<'static> {
    match t.ticket() {
        Some(k) => Span::styled(k.to_string(), Style::new().fg(accent(depth))),
        None => Span::styled("no ticket", dim(depth)),
    }
}

/// Where a click opens a task's ticket on each line of a tree `w` wide: on the first
/// line of a task with a tracker link, the columns its ticket takes, as drawn (none when
/// the row is too narrow to show it).
pub fn tree_tickets(board: &Board, w: usize) -> Vec<Option<(TaskId, std::ops::Range<u16>)>> {
    (rows(board).iter())
        .flat_map(|r| {
            let lines = r.lines(w, Depth::Ansi256, false);
            let n = lines.len();
            let first = match r {
                Row::Task(t) | Row::StandaloneTask(t) => t.ticket().and_then(|k| {
                    let text = k.to_string();
                    let mut x = 0u16;
                    for span in &lines[0].spans {
                        let sw = span.width() as u16;
                        if span.content == text && span.style.fg.is_some() {
                            return Some((t.id, x..x + sw));
                        }
                        x += sw;
                    }
                    None
                }),
                _ => None,
            };
            std::iter::once(first).chain(std::iter::repeat_n(None, n - 1))
        })
        .collect()
}

/// `left` cut to leave room for `right` at the right edge; `right` is dropped when fewer
/// than `min_left` columns would be left for `left`.
pub(crate) fn split_line(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    min_left: usize,
    w: usize,
) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    if w < min_left + right_w {
        return fit(left, w);
    }
    let mut spans = fit(left, w - right_w).spans;
    spans.extend(right);
    Line::from(spans)
}

/// What a click on a tree line folds or unfolds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldClick {
    /// A repo's or milestone's first line: its first columns, its fold mark, fold or
    /// unfold it.
    Mark(Fold, u16),
    /// A folded repo's or milestone's summary line: anywhere on it unfolds it.
    Line(Fold),
}

/// What a click on each line of a tree `w` wide folds or unfolds, line by line: only the
/// first line of a milestone whose title wraps has its mark.
pub fn tree_folds(board: &Board, w: usize) -> Vec<Option<FoldClick>> {
    (rows(board).iter())
        .flat_map(|r| {
            let n = r.lines(w, Depth::Ansi256, false).len();
            let rest = match r.fold() {
                Some(FoldClick::Mark(..)) => None,
                other => other,
            };
            std::iter::once(r.fold()).chain(std::iter::repeat_n(rest, n - 1))
        })
        .collect()
}

/// A folded repo's summary line: "   2 milestones · 3 standalone ✓1 Done !1", its active
/// milestones, standalone tasks, Done milestones and blocked tasks, leaving out a zero
/// count of all but the active milestones.
fn repo_folded_line(board: &Board, r: RepoId, w: usize, depth: Depth) -> Line<'static> {
    let n = board.active_of(Some(r)).count();
    let loose = board.standalone_of(Some(r)).count();
    let mut text = format!("   {}", milestones(n));
    if loose > 0 {
        text.push_str(&format!(" · {loose} standalone"));
    }
    let mut spans = vec![Span::styled(text, dim(depth))];
    if let n @ 1.. = board.done_of(r).len() {
        spans.push(Span::styled(
            format!(" ✓{n} Done"),
            Style::new().fg(ok(depth)),
        ));
    }
    if let n @ 1.. = board.blocked_in(Some(r)) {
        spans.push(Span::styled(format!(" !{n}"), bold().fg(warn(depth))));
    }
    fit(spans, w)
}

/// A folded milestone's summary line: "      4 tasks ✓2 ●1 ○1 !1", leaving out a zero
/// count.
fn folded_line(m: &Milestone, w: usize, depth: Depth) -> Line<'static> {
    let n = m.tasks.len();
    let mut spans = vec![Span::styled(
        format!(
            "{}{n} {}",
            " ".repeat(TASK_AT),
            if n == 1 { "task" } else { "tasks" }
        ),
        dim(depth),
    )];
    for stage in [Stage::Done, Stage::InProgress, Stage::Waiting] {
        let count = m.tasks.iter().filter(|t| t.stage == stage).count();
        if count > 0 {
            let mark = stage_mark(stage, depth);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!("{}{count}", mark.content), mark.style));
        }
    }
    if let n @ 1.. = m.blocked_tasks() {
        spans.push(Span::styled(format!(" !{n}"), bold().fg(warn(depth))));
    }
    fit(spans, w)
}

/// The item each line of a tree `w` wide selects, line by line: both lines of a wrapped
/// title select its item.
pub fn tree_items(board: &Board, w: usize) -> Vec<Option<Item>> {
    (rows(board).iter())
        .flat_map(|r| vec![r.item(); r.lines(w, Depth::Ansi256, false).len()])
        .collect()
}

/// The tree's lines, each exactly `w` columns.
pub fn tree_lines(board: &Board, w: usize, depth: Depth) -> Vec<Line<'static>> {
    rows(board)
        .iter()
        .flat_map(|r| r.lines(w, depth, false))
        .collect()
}

/// The tree's lines with the rows of `selected` on the selected background.
pub(crate) fn tree_lines_selected(
    board: &Board,
    w: usize,
    depth: Depth,
    selected: Option<Item>,
) -> Vec<Line<'static>> {
    (rows(board).iter())
        .flat_map(|r| {
            let on = r.item().is_some() && r.item() == selected;
            let mut lines = r.lines(w, depth, on);
            if on {
                for line in &mut lines {
                    line.style = Style::new().bg(super::theme::selected_bg(depth));
                }
            }
            lines
        })
        .collect()
}

/// Bold in the milestone's lead colour; just bold with no lead or an unknown colour.
pub(crate) fn lead_style(m: &Milestone, depth: Depth) -> Style {
    lead_style_of(m.lead.as_ref(), depth)
}

/// Bold in `lead`'s colour; just bold with no lead or an unknown colour.
pub(crate) fn lead_style_of(lead: Option<&crate::board::Lead>, depth: Depth) -> Style {
    match lead.and_then(|l| l.colour) {
        Some(c) => bold().fg(super::theme::lead_colour(c, depth)),
        None => bold(),
    }
}

/// "   ▾ M1 title" ("   ▸ " while folded) on the left, wrapping to an indented second line,
/// and the progress bar and percentage on the right, " ██░░░░░░  33% ", so a title still
/// keeps a space before the bar. The bar and percentage are dropped when the pane is too
/// narrow to show them beside the milestone id.
fn milestone_lines(m: &Milestone, w: usize, depth: Depth) -> Vec<Line<'static>> {
    let percent = m.percent();
    let mut right = vec![Span::raw(" ")];
    right.extend(bar(percent, BAR_CELLS, depth));
    right.push(Span::styled(format!(" {percent:>3}%"), bold()));
    right.push(Span::raw(" "));
    let right_w: usize = right.iter().map(|s| s.width()).sum();
    let at = usize::from(FOLD_COLUMNS);
    // Room for "   ▾ M1…" at least: the mark, the id whole, and the ellipsis that says the
    // title was cut.
    let min_left = at + format!("{}…", m.id).chars().count();
    let right_w = if w < min_left + right_w { 0 } else { right_w };
    let (head, rest) = two_lines(
        &format!("{} {}", m.id, m.title),
        w.saturating_sub(at + right_w),
        w.saturating_sub(at),
    );
    let mark = if m.folded { "▸ " } else { "▾ " };
    let left = vec![
        Span::raw(" ".repeat(MILESTONE_AT)),
        Span::styled(mark, dim(depth)),
        Span::raw(head),
    ];
    let mut first = fit(left, w - right_w).spans;
    if right_w > 0 {
        first.extend(right);
    }
    let mut lines = vec![Line::from(first)];
    if let Some(rest) = rest {
        lines.push(fit(vec![Span::raw(" ".repeat(at)), Span::raw(rest)], w));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_s_second_line_starts_with_the_run_of_ideographic_spaces_it_carries() {
        // As headless Chrome draws the copy's `.clamp2` title: a run of U+3000 starting a
        // word after a space goes to the next line with it, and a word that is only a run
        // goes with the word after it; after a word ending with a run, it hangs.
        let text = "表示確認 　全角空白 　　行末 　折り返し 测试 　中文";
        assert_eq!(
            two_lines(text, 30, 30),
            (
                "表示確認 　全角空白 　　行末".to_string(),
                Some("　折り返し 测试 　中文".to_string())
            )
        );
        assert_eq!(
            two_lines("ab 　 cd", 5, 5),
            ("ab".to_string(), Some("　 cd".to_string()))
        );
        assert_eq!(
            two_lines("ab　 　cd", 5, 5),
            ("ab".to_string(), Some("cd".to_string()))
        );
        // A chain of words that are only runs goes to the second line whole. The first
        // line is Chrome's at 10 and at 30; the second has room for the whole rest, and
        // starts as Chrome's does.
        let text = "設定 　 　 次へ 　ひらがな ok 　 　 カタカナ 　 漢字 　 中文 　 结束";
        assert_eq!(
            two_lines(text, 10, 63),
            (
                "設定".to_string(),
                Some("　 　 次へ 　ひらがな ok 　 　 カタカナ 　 漢字 　 中文 　 结束".to_string())
            )
        );
        assert_eq!(
            two_lines(text, 30, 40),
            (
                "設定 　 　 次へ 　ひらがな ok".to_string(),
                Some("　 　 カタカナ 　 漢字 　 中文 　 结束".to_string())
            )
        );
    }
}
