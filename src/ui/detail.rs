//! The detail views, as in the golden UI's layout E, each with "◀ close" at the right of its
//! first line: a repo's lead and team, main checkout, forge link and work, its milestones,
//! its standalone tasks and its whole message board (UI v0.1.7); a milestone's goal, stage, lead and team, standing context, spec, tracker
//! link, its tasks with a line of description each and its message board; a task's
//! milestone and lead, who is on it and its team, what it is doing, the blocked flag's
//! reason while it is blocked, what it is blocked by, its MR, standing context and tracker
//! link (errata 1), description, acceptance criteria, subtasks and thread. A standalone
//! task's, shown straight away with nothing to close, names its repo and the repo's lead in
//! place of its milestone and shows its thread; one in no repo yet says how the CLI places
//! it (`tower task set --repo`, or `tower task hand` on a board with no repos), and has no
//! thread.
//!
//! A milestone's board is its part of the repo's: the threads linked to it or its tasks.
//!
//! Each view is built once into its lines and its layout together, so what is drawn and
//! what a click lands on cannot disagree. The message board and the thread are the
//! `message_board` summary's thread boxes, all of the lead's or the task's own.

use std::collections::BTreeSet;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::Mode;
use super::control::{Control, ControlAt, with_control};
use super::dash::{Item, ViewKind, milestone_of, repo_of, standalone_of};
use super::message_board::{self, Limits, Shown, ThreadRow, role_colour};
use super::theme::{
    Depth, accent, bar, bold, dim, fit, ok, stage_glyph, stage_mark, stage_style, warn,
};
use super::tree::{done_tally, lead_style, marked_done, split_line};
use crate::board::{
    Board, Lead, Milestone, MilestoneId, Repo, Stage, Talk, Task, TaskId, Tone, filled_cells,
    no_lead_line, team, tone_of,
};

/// Columns the field labels take, "blocked by" being the longest, before a space.
const LABEL: usize = 10;
/// Where a field's value starts: a space, the label and a space.
const VALUE_X: usize = LABEL + 2;
/// Where a task row's description line starts in the milestone detail, under its title.
const ROW_INDENT: usize = 6;
/// Cells in the milestone detail's progress bar, as in the golden UI.
const BAR_CELLS: usize = 20;

/// Where a task's row is in the milestone detail: `height` lines from `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRow {
    pub task: TaskId,
    pub line: usize,
    pub height: usize,
}

/// A detail view's geometry: `lines` tall, with the rows a click opens a task from, and its
/// controls: "◀ close".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLayout {
    pub lines: usize,
    pub rows: Vec<TaskRow>,
    /// The repo detail's milestone lines, by line: a click opens that milestone's detail.
    pub milestones: Vec<(usize, MilestoneId)>,
    pub controls: Vec<ControlAt>,
    /// The thread boxes a click opens the full message board from.
    pub threads: Vec<ThreadRow>,
    /// The board's RESOLVED row, by line: a click folds or unfolds its resolved threads.
    pub resolved: Option<usize>,
}

impl DetailLayout {
    /// The milestone whose line in the repo detail is line `line`.
    pub fn milestone_at(&self, line: usize) -> Option<MilestoneId> {
        (self.milestones.iter())
            .find(|(l, _)| *l == line)
            .map(|(_, m)| *m)
    }

    /// The task whose row covers line `line` of the view.
    pub fn task_at(&self, line: usize) -> Option<TaskId> {
        (self.rows.iter())
            .find(|r| (r.line..r.line + r.height).contains(&line))
            .map(|r| r.task)
    }
}

/// A detail view: its lines, each exactly the pane's width, and its layout.
pub struct Detail {
    pub lines: Vec<Line<'static>>,
    pub layout: DetailLayout,
}

/// The colours a detail view is drawn in: those of a terminal of some depth, or none when
/// only its layout is wanted, so the layout never depends on a colour depth.
#[derive(Debug, Clone, Copy)]
struct Pen(Option<Depth>);

impl Pen {
    fn paint(self, style: impl Fn(Depth) -> Style) -> Style {
        self.0.map_or(Style::new(), style)
    }

    fn lead(self, m: &Milestone) -> Style {
        self.paint(|d| lead_style(m, d))
    }

    fn stage(self, stage: Stage) -> Style {
        self.paint(|d| stage_style(stage, d))
    }

    fn accent(self) -> Style {
        self.paint(|d| Style::new().fg(accent(d)))
    }

    fn dim(self) -> Style {
        self.paint(dim)
    }

    fn ok(self) -> Style {
        self.paint(|d| Style::new().fg(ok(d)))
    }

    fn warn(self) -> Style {
        self.paint(|d| Style::new().fg(warn(d)))
    }

    fn role(self, name: &str, lead: Option<&Lead>) -> Style {
        self.paint(|d| Style::new().fg(role_colour(name, lead, d)))
    }

    fn mark(self, stage: Stage) -> Span<'static> {
        match self.0 {
            Some(d) => stage_mark(stage, d),
            None => Span::raw(stage_glyph(stage)),
        }
    }

    fn highlight(self, text: &str, start: bool, awareness: bool) -> Vec<Span<'static>> {
        match self.0 {
            Some(d) => message_board::highlight(text, start, awareness, d),
            None => vec![Span::raw(text.to_string())],
        }
    }

    fn bar(self, percent: u8, cells: usize) -> [Span<'static>; 2] {
        match self.0 {
            Some(d) => bar(percent, cells, d),
            None => {
                let filled = filled_cells(percent, cells);
                [
                    Span::raw("█".repeat(filled)),
                    Span::raw("░".repeat(cells - filled)),
                ]
            }
        }
    }
}

/// The detail view `kind` shows for the selection `sel`, `w` wide, in the colours of
/// `depth`, with message ages as of `now`; `None` for the overview, which is not a detail
/// view. A task detail with a milestone selected shows the milestone; any detail of a
/// standalone task shows the task.
pub fn detail(
    board: &Board,
    kind: ViewKind,
    sel: Item,
    w: usize,
    shown: Shown,
    now: i64,
    depth: Depth,
) -> Option<Detail> {
    build(board, kind, sel, w, shown, now, Pen(Some(depth)))
}

/// Where everything in the detail view `kind` goes, as `detail` draws it, without colours,
/// its boards as the owner left them (`shown`), at the clock `detail` draws it at (`now`):
/// an ack lays its age out at its width as drawn.
pub fn detail_layout(
    board: &Board,
    kind: ViewKind,
    sel: Item,
    w: usize,
    shown: Shown,
    now: i64,
) -> Option<DetailLayout> {
    build(board, kind, sel, w, shown, now, Pen(None)).map(|d| d.layout)
}

fn build(
    board: &Board,
    kind: ViewKind,
    sel: Item,
    w: usize,
    shown: Shown,
    now: i64,
    pen: Pen,
) -> Option<Detail> {
    if let Some(t) = standalone_of(board, sel) {
        return (kind != ViewKind::Overview)
            .then(|| task_lines(board, None, t, w, shown, now, pen));
    }
    if let Item::Repo(_) = sel {
        let r = repo_of(board, sel)?;
        return (kind == ViewKind::RepoDetail).then(|| repo_lines(board, r, w, shown, now, pen));
    }
    let m = milestone_of(board, sel)?;
    match (kind, sel) {
        (ViewKind::Overview | ViewKind::Board | ViewKind::Inbox | ViewKind::RepoDetail, _) => None,
        (ViewKind::TaskDetail, Item::Task(id)) => {
            let t = m.tasks.iter().find(|t| t.id == id)?;
            Some(task_lines(board, Some(m), t, w, shown, now, pen))
        }
        (ViewKind::MilestoneDetail | ViewKind::TaskDetail, _) => {
            Some(milestone_lines(board, m, w, shown, now, pen))
        }
    }
}

/// A board as it first shows: no message opened in full, RESOLVED folded.
const FOLDED: Shown = Shown {
    opened: &NO_MESSAGES,
    unfolded: false,
};
static NO_MESSAGES: BTreeSet<i64> = BTreeSet::new();

/// The milestone detail of `m` on `board` in a pane `w` wide, in the colours of `depth`,
/// with message ages as of `now`.
pub fn milestone_detail(board: &Board, m: &Milestone, w: usize, now: i64, depth: Depth) -> Detail {
    milestone_lines(board, m, w, FOLDED, now, Pen(Some(depth)))
}

/// The task detail of `t`, under `m` or standalone, in a pane `w` wide, in the colours of
/// `depth`, with message ages as of `now`. The tasks it is blocked by are looked up on
/// `board`.
pub fn task_detail(
    board: &Board,
    m: Option<&Milestone>,
    t: &Task,
    w: usize,
    now: i64,
    depth: Depth,
) -> Detail {
    task_lines(board, m, t, w, FOLDED, now, Pen(Some(depth)))
}

/// Adds the thread boxes of the board `m` for `task` (all of them with `None`), or why
/// there are none, and returns where each box is. Without colours (a layout) any depth will
/// do: the summary's height never depends on the depth.
fn board_boxes(
    lines: &mut Vec<Line<'static>>,
    m: &Talk,
    task: Option<TaskId>,
    w: usize,
    shown: Shown,
    now: i64,
    pen: Pen,
) -> (Vec<ThreadRow>, Option<usize>) {
    // `boxes` leaves out the summary's blank line and title, two lines.
    let at = lines.len();
    // A detail view shows the board as its own space does, in the popup too.
    let limits = Limits::of(Mode::Space, shown, now);
    lines.extend(message_board::boxes(
        m,
        task,
        w,
        limits,
        pen.0.unwrap_or(Depth::Ansi256),
    ));
    let threads = (message_board::thread_rows(m, task, w, limits).into_iter())
        .map(|r| ThreadRow {
            line: r.line + at - 2,
            ..r
        })
        .collect();
    let resolved = message_board::resolved_row(m, task, w, limits).map(|l| l + at - 2);
    (threads, resolved)
}

/// `text` wrapped to `width` columns at word boundaries, keeping its own line breaks.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    text.lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                vec![String::new()]
            } else {
                textwrap::wrap(line, width)
                    .into_iter()
                    .map(|c| c.into_owned())
                    .collect()
            }
        })
        .collect()
}

/// " label      value": one line, the value cut to fit.
fn field(label: &str, value: Vec<Span<'static>>, w: usize, pen: Pen) -> Line<'static> {
    let mut spans = vec![Span::styled(format!(" {label:<LABEL$} "), pen.dim())];
    spans.extend(value);
    fit(spans, w)
}

/// A field whose value is text of any length: wrapped under the value's column, or a dim
/// `none` when there is none.
fn text_field(
    label: &str,
    text: Option<&str>,
    style: Style,
    none: &str,
    w: usize,
    pen: Pen,
) -> Vec<Line<'static>> {
    let Some(text) = text else {
        return vec![field(
            label,
            vec![Span::styled(none.to_string(), pen.dim())],
            w,
            pen,
        )];
    };
    wrap(text, w.saturating_sub(VALUE_X))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let label = if i == 0 { label } else { "" };
            field(label, vec![Span::styled(line, style)], w, pen)
        })
        .collect()
}

/// A link field: the URL in the accent colour, wrapped so it is never cut.
fn link_field(
    label: &str,
    link: Option<&str>,
    none: &str,
    w: usize,
    pen: Pen,
) -> Vec<Line<'static>> {
    let style = pen.accent();
    let Some(link) = link else {
        return text_field(label, None, style, none, w, pen);
    };
    // A URL has no spaces to wrap at, so it is broken every so many columns.
    let width = w.saturating_sub(VALUE_X).max(1);
    let chars: Vec<char> = link.chars().collect();
    (chars.chunks(width).enumerate())
        .map(|(i, part)| {
            let label = if i == 0 { label } else { "" };
            field(
                label,
                vec![Span::styled(part.iter().collect::<String>(), style)],
                w,
                pen,
            )
        })
        .collect()
}

/// A section heading after a blank line: " TASKS 1/3".
fn heading(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    count: Option<(usize, usize)>,
    w: usize,
    pen: Pen,
) {
    lines.push(fit(Vec::new(), w));
    let mut spans = vec![Span::styled(format!(" {title}"), bold())];
    if let Some((done, all)) = count {
        spans.push(Span::styled(format!(" {done}/{all}"), pen.dim()));
    }
    lines.push(fit(spans, w));
}

/// A line of dim text under a heading, such as "none".
fn note(text: &str, w: usize, pen: Pen) -> Line<'static> {
    fit(vec![Span::styled(format!(" {text}"), pen.dim())], w)
}

/// A `team` (see `board::team`) of `lead`'s, "dev-2, senior, qa", each in its role's
/// colour, in lines of at most `w` columns broken between names; none when it has nobody.
fn team_lines(team: Vec<&str>, lead: Option<&Lead>, w: usize, pen: Pen) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut used = 0;
    for (i, name) in team.iter().enumerate() {
        let comma = if i + 1 < team.len() { "," } else { "" };
        let width = name.width() + comma.len();
        match lines.last_mut() {
            Some(line) if used + 1 + width <= w => {
                line.push(Span::raw(" "));
                used += 1 + width;
            }
            _ => {
                lines.push(Vec::new());
                used = width;
            }
        }
        let line = lines.last_mut().expect("a line was just pushed");
        line.push(Span::styled(name.to_string(), pen.role(name, lead)));
        line.push(Span::raw(comma));
    }
    lines
}

/// The first line of a detail view: `left`, cut to fit beside "◀ close" at the right, which
/// Esc does too, when `close`. Returns where the control is.
fn first_line(
    left: Vec<Span<'static>>,
    min_left: usize,
    close: bool,
    w: usize,
    pen: Pen,
) -> (Line<'static>, Option<ControlAt>) {
    if !close {
        return (fit(left, w), None);
    }
    let (line, at) = with_control(left, min_left, Control::Close, pen.accent(), w);
    let at = at.map(|(from, to)| ControlAt {
        control: Control::Close,
        line: 0,
        from,
        to,
    });
    (line, at)
}

fn pill(stage: Stage, pen: Pen) -> Span<'static> {
    Span::styled(format!("[{}]", stage.label()), pen.stage(stage))
}

fn milestone_lines(
    board: &Board,
    m: &Milestone,
    w: usize,
    shown: Shown,
    now: i64,
    pen: Pen,
) -> Detail {
    let d = &m.details;
    let (first, close) = first_line(
        vec![
            Span::raw(" "),
            Span::styled(format!("{}  {}", m.id, m.title), pen.lead(m)),
        ],
        format!(" {}  ", m.id).width() + TITLE_MIN,
        true,
        w,
        pen,
    );
    let mut lines = vec![first];
    lines.extend(text_field(
        "goal",
        Some(&m.goal),
        Style::new(),
        "none",
        w,
        pen,
    ));
    let percent = m.percent();
    let mut stage = vec![pill(m.stage(), pen), Span::raw(" ")];
    let mut tail = vec![
        Span::styled(format!("{percent}%"), bold()),
        Span::styled(
            format!(" {}/{} tasks done", m.done_tasks(), m.tasks.len()),
            pen.dim(),
        ),
    ];
    // As the overview's `mark_note` says it.
    if m.stage() == Stage::Done {
        tail.push(Span::styled(
            format!(" {}", marked_done(m.done_at)),
            pen.stage(Stage::Done),
        ));
    } else if percent == 100 {
        tail.push(Span::styled(" Done mark not set", pen.dim()));
    }
    let without_bar: usize = stage.iter().chain(&tail).map(|s| s.width()).sum();
    if VALUE_X + without_bar + BAR_CELLS < w {
        stage.extend(pen.bar(percent, BAR_CELLS));
        stage.push(Span::raw(" "));
    }
    stage.extend(tail);
    lines.push(field("stage", stage, w, pen));
    let lead = match &m.lead {
        Some(l) => Span::styled(l.name.clone(), pen.lead(m)),
        None => Span::styled(no_lead_line(m.id, m.repo.is_some()), pen.dim()),
    };
    lines.push(field("lead", vec![lead], w, pen));
    let team = team_lines(m.team(), m.lead.as_ref(), w.saturating_sub(VALUE_X), pen);
    if team.is_empty() {
        lines.push(field(
            "team",
            vec![Span::styled("nobody yet", pen.dim())],
            w,
            pen,
        ));
    }
    for (i, names) in team.into_iter().enumerate() {
        lines.push(field(if i == 0 { "team" } else { "" }, names, w, pen));
    }
    lines.extend(text_field(
        "context",
        d.context.as_deref(),
        Style::new(),
        "none",
        w,
        pen,
    ));
    lines.extend(text_field(
        "spec",
        d.spec.as_deref(),
        Style::new(),
        "none",
        w,
        pen,
    ));
    lines.extend(link_field("tracker", d.link.as_deref(), "none", w, pen));

    heading(
        &mut lines,
        "TASKS",
        Some((m.done_tasks(), m.tasks.len())),
        w,
        pen,
    );
    let mut rows = Vec::new();
    if m.tasks.is_empty() {
        lines.push(note("no tasks yet", w, pen));
    }
    for t in &m.tasks {
        let line = lines.len();
        lines.push(task_row(t, w, pen));
        if let Some(first) = t
            .details
            .description
            .as_deref()
            .and_then(|d| d.lines().next())
        {
            let indent = " ".repeat(ROW_INDENT);
            lines.push(fit(
                vec![Span::styled(format!("{indent}{first}"), pen.dim())],
                w,
            ));
        }
        rows.push(TaskRow {
            task: t.id,
            line,
            height: lines.len() - line,
        });
    }

    heading(&mut lines, "MESSAGE BOARD", None, w, pen);
    let (threads, resolved) = board_boxes(
        &mut lines,
        &Talk::of_milestone(board, m),
        None,
        w,
        shown,
        now,
        pen,
    );

    let layout = DetailLayout {
        lines: lines.len(),
        milestones: Vec::new(),
        rows,
        controls: close.into_iter().collect(),
        threads,
        resolved,
    };
    Detail { lines, layout }
}

/// Cells in a milestone's bar in the repo detail.
const REPO_BAR_CELLS: usize = 10;

/// Repo `r`'s detail: its name, its lead and team (everyone on its tasks and its board),
/// main checkout, forge link and how much work it holds, then its milestones (mark, id,
/// title, bar and percentage), its standalone tasks (each opening its task detail) and its
/// whole message board.
fn repo_lines(board: &Board, r: &Repo, w: usize, shown: Shown, now: i64, pen: Pen) -> Detail {
    let (first, close) = first_line(
        vec![
            Span::raw(" "),
            Span::styled(r.name(), bold()),
            Span::styled("  repo", pen.dim()),
        ],
        1 + r.name().width().min(TITLE_MIN),
        true,
        w,
        pen,
    );
    let mut lines = vec![first];
    let lead = match &r.lead {
        Some(l) => Span::styled(l.name.clone(), pen.accent()),
        None => Span::styled("no lead yet: n starts one", pen.dim()),
    };
    lines.push(field("lead", vec![lead], w, pen));
    let milestones: Vec<&Milestone> = board.milestones_of(Some(r.id)).collect();
    let active: Vec<&Milestone> = board.active_of(Some(r.id)).collect();
    let done = board.done_of(r.id);
    let loose: Vec<&Task> = board.standalone_of(Some(r.id)).collect();
    let tasks = (milestones.iter().flat_map(|m| &m.tasks)).chain(loose.iter().copied());
    let names = team(r.lead.as_ref(), tasks, r.threads.iter());
    let team = team_lines(names, r.lead.as_ref(), w.saturating_sub(VALUE_X), pen);
    if team.is_empty() {
        lines.push(field(
            "team",
            vec![Span::styled("nobody yet", pen.dim())],
            w,
            pen,
        ));
    }
    for (i, names) in team.into_iter().enumerate() {
        lines.push(field(if i == 0 { "team" } else { "" }, names, w, pen));
    }
    let unknown = format!("not known yet: tower repo set {} --path", r.id);
    lines.extend(text_field(
        "checkout",
        r.path.as_deref(),
        Style::new(),
        &unknown,
        w,
        pen,
    ));
    lines.extend(link_field("tracker", r.link.as_deref(), "none", w, pen));
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let work = format!(
        "{} · {} · {}",
        plural(active.len(), "active milestone"),
        plural(loose.len(), "standalone task"),
        plural(done.len(), "Done milestone"),
    );
    lines.push(field("work", vec![Span::raw(work)], w, pen));
    heading(&mut lines, "MILESTONES", None, w, pen);
    if active.is_empty() {
        lines.push(note("none", w, pen));
    }
    let mut lines_of = Vec::new();
    let milestone_line = |m: &Milestone| {
        let left = vec![
            Span::raw(" "),
            Span::styled(stage_glyph(m.stage()), pen.stage(m.stage())),
            Span::styled(format!(" {}", m.id), pen.dim()),
            Span::raw(format!(" {}", m.title)),
        ];
        let right = if m.stage() == Stage::Done {
            vec![Span::styled(format!(" {} ", done_tally(m)), pen.dim())]
        } else {
            let percent = m.percent();
            let mut right = vec![Span::raw(" ")];
            right.extend(pen.bar(percent, REPO_BAR_CELLS));
            right.push(Span::styled(format!(" {percent:>3}% "), bold()));
            right
        };
        split_line(left, right, format!(" ✓ {}…", m.id).width(), w)
    };
    for m in &active {
        lines_of.push((lines.len(), m.id));
        lines.push(milestone_line(m));
    }
    // The Done group under a label, most recently done first.
    if !done.is_empty() {
        lines.push(fit(
            vec![
                Span::raw(" "),
                Span::styled("Done", pen.stage(Stage::Done)),
                Span::styled(format!(" {}", plural(done.len(), "milestone")), pen.dim()),
            ],
            w,
        ));
    }
    for m in &done {
        lines_of.push((lines.len(), m.id));
        lines.push(milestone_line(m));
    }
    let done = loose.iter().filter(|t| t.stage == Stage::Done).count();
    heading(
        &mut lines,
        "STANDALONE TASKS",
        Some((done, loose.len())),
        w,
        pen,
    );
    let mut rows = Vec::new();
    if loose.is_empty() {
        lines.push(note("none", w, pen));
    }
    for t in &loose {
        rows.push(TaskRow {
            task: t.id,
            line: lines.len(),
            height: 1,
        });
        lines.push(task_row(t, w, pen));
    }
    heading(&mut lines, "MESSAGE BOARD", None, w, pen);
    let (threads, resolved) = board_boxes(
        &mut lines,
        &Talk::of_repo(board, r),
        None,
        w,
        shown,
        now,
        pen,
    );
    let layout = DetailLayout {
        lines: lines.len(),
        milestones: lines_of,
        rows,
        controls: close.into_iter().collect(),
        threads,
        resolved,
    };
    Detail { lines, layout }
}

/// " ✓ #12 Bookmarks page ...   dev-1 [done]": a task's mark, id and title, with who is on
/// it and its stage at the right edge while there is room.
fn task_row(t: &Task, w: usize, pen: Pen) -> Line<'static> {
    let left = vec![
        Span::raw(" "),
        pen.mark(t.stage),
        Span::styled(format!(" {}", t.id), pen.dim()),
        Span::raw(format!(" {}", t.title)),
    ];
    let who = t.details.assignee.as_deref().unwrap_or("unassigned");
    let right = vec![
        Span::styled(format!(" {who} "), pen.dim()),
        pill(t.stage, pen),
        Span::raw(" "),
    ];
    let min_left = format!(" ✓ {}…", t.id).width();
    split_line(left, right, min_left, w)
}

/// The columns of an item's title its detail's first line keeps beside "◀ close".
const TITLE_MIN: usize = 8;

fn task_lines(
    board: &Board,
    m: Option<&Milestone>,
    t: &Task,
    w: usize,
    shown: Shown,
    now: i64,
    pen: Pen,
) -> Detail {
    let d = &t.details;
    // A standalone task's detail shows straight away, with no view under it to go back to.
    let (first, close) = first_line(
        // "task #12": tower's id, said as the task's, so it is never taken for its ticket.
        vec![
            Span::raw(" "),
            Span::styled(format!("task {}", t.id), pen.dim()),
            Span::styled(format!(" {} ", t.title), bold()),
            pill(t.stage, pen),
        ],
        format!(" task {} ", t.id).width() + TITLE_MIN,
        m.is_some(),
        w,
        pen,
    );
    let mut lines = vec![first];
    let controls: Vec<ControlAt> = close.into_iter().collect();
    // A standalone task's repo, when it is on the board.
    let repo = t.repo.and_then(|r| board.repo(r)).filter(|_| m.is_none());
    match (m, repo) {
        (Some(m), _) => {
            let lead = match &m.lead {
                Some(l) => format!(" · {}", l.name),
                None => " · no lead yet".to_string(),
            };
            lines.push(field(
                "milestone",
                vec![
                    Span::styled(format!("{} {}", m.id, m.title), pen.lead(m)),
                    Span::styled(lead, pen.dim()),
                ],
                w,
                pen,
            ));
        }
        (None, Some(r)) => {
            let mut value = vec![Span::styled(
                format!("standalone, no milestone · {} · ", r.name()),
                pen.dim(),
            )];
            value.push(match &r.lead {
                Some(l) => Span::styled(l.name.clone(), pen.accent()),
                None => Span::styled("no lead yet", pen.dim()),
            });
            lines.push(field("milestone", value, w, pen));
        }
        // In no repo on a board with repos: it is given its repo, not handed to a lead.
        (None, None) if !board.repos.is_empty() => {
            let hint = format!(
                "standalone, in no repo yet: tower task set {} --repo R<n>",
                t.id.0
            );
            lines.push(field(
                "milestone",
                vec![Span::styled(hint, pen.dim())],
                w,
                pen,
            ));
        }
        // On a board with no repos: the CLI hands it to a milestone's lead.
        (None, None) => {
            let hint = format!("standalone, no lead yet: tower task hand {} M<n>", t.id.0);
            lines.push(field(
                "milestone",
                vec![Span::styled(hint, pen.dim())],
                w,
                pen,
            ));
        }
    }
    let mut assigned = vec![match &d.assignee {
        Some(a) => Span::raw(a.clone()),
        None => Span::styled("unassigned", pen.dim()),
    }];
    // The team on the same line, cut to fit, as in the golden UI.
    if let Some(names) =
        m.and_then(|m| team_lines(m.team(), m.lead.as_ref(), usize::MAX, pen).pop())
    {
        assigned.push(Span::styled(" · team ", pen.dim()));
        assigned.extend(names);
    }
    lines.push(field("assigned", assigned, w, pen));
    // The latest progress post in full; the blocked flag has its own row below. Its
    // leading tag is toned as in its bubble, by the whole post's tone.
    match &d.doing {
        None => lines.push(field(
            "doing",
            vec![Span::styled("nothing posted yet", pen.dim())],
            w,
            pen,
        )),
        Some(post) => {
            let awareness = tone_of(post) == Tone::Awareness;
            let pieces = wrap(&post.text, w.saturating_sub(VALUE_X));
            for (i, piece) in pieces.iter().enumerate() {
                let label = if i == 0 { "doing" } else { "" };
                let value = pen.highlight(piece, i == 0, awareness);
                lines.push(field(label, value, w, pen));
            }
        }
    }
    // The flag a worker or lead set when stuck, beside the dependencies; not the same thing.
    if let Some(reason) = &d.blocked {
        lines.extend(text_field("blocked", Some(reason), pen.warn(), "", w, pen));
    }
    if d.blocked_by.is_empty() {
        lines.push(field(
            "blocked by",
            vec![Span::styled("nothing", pen.dim())],
            w,
            pen,
        ));
    }
    for (i, id) in d.blocked_by.iter().enumerate() {
        let label = if i == 0 { "blocked by" } else { "" };
        let found = (board.milestones.iter())
            .flat_map(|m| &m.tasks)
            .chain(&board.standalone)
            .find(|x| x.id == *id);
        let value = match found {
            Some(b) => vec![
                pen.mark(b.stage),
                Span::styled(format!(" {}", b.id), pen.dim()),
                Span::raw(format!(" {}", b.title)),
            ],
            None => vec![Span::styled(id.to_string(), pen.dim())],
        };
        lines.push(field(label, value, w, pen));
    }
    lines.extend(text_field(
        "MR",
        d.mr.as_deref(),
        Style::new(),
        "none yet",
        w,
        pen,
    ));
    lines.extend(text_field(
        "context",
        d.context.as_deref(),
        Style::new(),
        "none",
        w,
        pen,
    ));
    // Its ticket, from the tracker link, then the link itself.
    let ticket = match t.ticket() {
        Some(k) => Span::styled(k.to_string(), pen.accent()),
        None => Span::styled("no ticket", pen.dim()),
    };
    lines.push(field("ticket", vec![ticket], w, pen));
    lines.extend(link_field("tracker", d.link.as_deref(), "none", w, pen));

    heading(&mut lines, "DESCRIPTION", None, w, pen);
    match d.description.as_deref() {
        Some(text) => lines.extend(
            wrap(text, w.saturating_sub(2))
                .into_iter()
                .map(|l| fit(vec![Span::raw(format!(" {l}"))], w)),
        ),
        None => lines.push(note("no description yet", w, pen)),
    }

    let met = d.criteria.iter().filter(|c| c.done).count();
    heading(
        &mut lines,
        "ACCEPTANCE CRITERIA",
        Some((met, d.criteria.len())),
        w,
        pen,
    );
    if d.criteria.is_empty() {
        lines.push(note("none", w, pen));
    }
    for c in &d.criteria {
        checklist(&mut lines, c.done, &c.text, None, w, pen);
    }

    heading(
        &mut lines,
        "SUBTASKS",
        Some((t.done_subtasks(), t.subtasks.len())),
        w,
        pen,
    );
    if t.subtasks.is_empty() {
        lines.push(note("none", w, pen));
    }
    for s in &t.subtasks {
        let text = format!("{} {}", s.id, s.title);
        let doing = (s.stage == Stage::InProgress).then_some("in progress");
        checklist(&mut lines, s.stage == Stage::Done, &text, doing, w, pen);
    }

    // A standalone task in no repo has no board, so no thread, as in the golden UI.
    let (mut threads, mut resolved) = (Vec::new(), None);
    let talk = match (m, repo) {
        (Some(m), _) => Some(Talk::of_milestone(board, m)),
        (None, Some(r)) => Some(Talk::of_repo(board, r)),
        (None, None) => None,
    };
    if let Some(talk) = talk {
        heading(&mut lines, "THREAD", None, w, pen);
        (threads, resolved) = board_boxes(&mut lines, &talk, Some(t.id), w, shown, now, pen);
    }

    let layout = DetailLayout {
        lines: lines.len(),
        milestones: Vec::new(),
        rows: Vec::new(),
        controls,
        threads,
        resolved,
    };
    Detail { lines, layout }
}

/// " [x] text" in the golden UI's checklist style, wrapped under the text, with an optional
/// dim note after it, such as "in progress".
fn checklist(
    lines: &mut Vec<Line<'static>>,
    done: bool,
    text: &str,
    after: Option<&str>,
    w: usize,
    pen: Pen,
) {
    let (mark, style) = if done {
        ("[x]", pen.ok())
    } else {
        ("[ ]", pen.dim())
    };
    let wrapped = wrap(text, w.saturating_sub(6));
    let last = wrapped.len() - 1;
    for (i, part) in wrapped.into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![Span::raw(" "), Span::styled(mark, style), Span::raw(" ")]
        } else {
            vec![Span::raw("     ")]
        };
        spans.push(Span::raw(part));
        if let (true, Some(note)) = (i == last, after) {
            spans.push(Span::styled(format!(" · {note}"), pen.dim()));
        }
        lines.push(fit(spans, w));
    }
}
