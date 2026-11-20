//! The milestone overview, as in the golden UI: the milestone in its lead's colour with the
//! lead's name and its tracker link, and "open milestone ▶" (or "open task ▶") at the right,
//! the goal (wrapping to two lines), the stage, bar and done count, then one column of task
//! cards per stage, side by side when the pane is `COLUMNS_MIN` wide and stacked when
//! narrower. A card is one line, as UI v0.1.7 has it: the task's number, who is on it,
//! its subtasks and a "!" while it is blocked. Its title is in the tree, and what it is
//! doing in its thread's header.
//!
//! `overview_layout` says where everything goes without styling it, for clicks and
//! scrolling; `overview` draws the lines from that same layout.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::control::{Control, ControlAt, with_control};
use super::message_board::clamp;
use super::theme::{
    Depth, accent, bar, bold, dim, fit, goal, selected_bg, stage_glyph, stage_style, warn,
};
use super::tree::{badge_spans, done_tally, lead_style, marked_done, milestones};
use crate::board::{Board, Milestone, MilestoneId, Repo, Stage, Task, TaskId};

/// Cells in the progress bar of the milestone overview, as in the golden UI.
const OVERVIEW_BAR_CELLS: usize = 24;
/// The stage columns sit side by side from this overview width; below it they stack.
pub(crate) const COLUMNS_MIN: usize = 60;
/// The goal wraps to at most this many lines.
const GOAL_LINES: usize = 2;
/// " goal ", before the goal's text; its second line is indented as far.
const GOAL_LABEL: &str = " goal ";
/// A box's top border, stage header and the rule under it.
const BOX_HEAD_LINES: usize = 3;
/// The columns of the milestone's title the header keeps beside its control.
const HEADER_TITLE_MIN: usize = 8;

/// Where everything in one milestone's overview goes, `lines` tall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewLayout {
    pub lines: usize,
    pub cards: Vec<Card>,
    /// The controls a click presses the key of: "open milestone ▶" or "open task ▶", and
    /// "open full board ▶" once the message board is laid out under it.
    pub controls: Vec<ControlAt>,
    /// The milestone lines of a repo overview, by line: a click selects the milestone.
    pub milestones: Vec<(usize, MilestoneId)>,
    boxes: Vec<StageBox>,
}

/// One bordered box of stage columns: its first line, its columns, and how many lines its
/// cards take under the header.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StageBox {
    top: usize,
    columns: Vec<(Stage, usize)>,
    body: usize,
}

/// Where a task's card is in the overview: `height` lines from `line`, and `width` columns
/// from `x`, counted from the overview's top left. A card is one line since UI v0.1.7, so
/// `height` is 1; it stays so clicks and scrolling treat a card as an area, as they do
/// every other row of the overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    pub task: TaskId,
    pub line: usize,
    pub x: usize,
    pub width: usize,
    pub height: usize,
}

impl OverviewLayout {
    /// The task whose card covers column `x` of overview line `line`.
    pub fn card_at(&self, x: usize, line: usize) -> Option<TaskId> {
        self.cards
            .iter()
            .find(|c| {
                (c.line..c.line + c.height).contains(&line) && (c.x..c.x + c.width).contains(&x)
            })
            .map(|c| c.task)
    }

    /// The milestone whose line in a repo overview is line `line`.
    pub fn milestone_at(&self, line: usize) -> Option<MilestoneId> {
        (self.milestones.iter())
            .find(|(l, _)| *l == line)
            .map(|(_, m)| *m)
    }
}

fn tasks_at(m: &Milestone, stage: Stage) -> impl Iterator<Item = &Task> {
    m.tasks.iter().filter(move |t| t.stage == stage)
}

/// Where everything in `m`'s overview goes in a pane `w` wide, with `selected` (one of its
/// tasks) or the milestone itself selected. Each box sits one column in from each side of
/// the pane; its columns share the room between its borders.
pub fn overview_layout(m: &Milestone, selected: Option<TaskId>, w: usize) -> OverviewLayout {
    let groups: Vec<Vec<Stage>> = if w >= COLUMNS_MIN {
        vec![Stage::ALL.to_vec()]
    } else {
        Stage::ALL.iter().map(|s| vec![*s]).collect()
    };
    // The milestone line, the goal's lines and the progress line.
    let mut lines = 2 + goal_pieces(m, w).len();
    let (mut cards, mut boxes) = (Vec::new(), Vec::new());
    for stages in groups {
        let n = stages.len();
        let inner = w.saturating_sub(2).saturating_sub(n + 1);
        let top = lines;
        let mut x = 2;
        let mut body = 0;
        let mut columns = Vec::new();
        for (i, stage) in stages.into_iter().enumerate() {
            let cw = inner / n + if i == n - 1 { inner % n } else { 0 };
            let mut y = top + BOX_HEAD_LINES;
            for t in tasks_at(m, stage) {
                let height = 1;
                cards.push(Card {
                    task: t.id,
                    line: y,
                    x,
                    width: cw,
                    height,
                });
                y += height;
            }
            // An empty column says "none" on one line.
            body = body.max((y - top - BOX_HEAD_LINES).max(1));
            columns.push((stage, cw));
            x += cw + 1;
        }
        lines = top + BOX_HEAD_LINES + body + 1;
        boxes.push(StageBox { top, columns, body });
    }
    // Colours never move the control, so any depth will do.
    let controls = header_line(m, selected, w, Depth::Ansi256)
        .1
        .into_iter()
        .collect();
    OverviewLayout {
        lines,
        cards,
        controls,
        milestones: Vec::new(),
        boxes,
    }
}

/// Cells in a milestone's bar in the repo overview.
const REPO_BAR_CELLS: usize = 10;

/// A repo's overview, as UI v0.1.7 draws it: " storefront · its lead · its forge link" with
/// "open repo ▶" at the right, its main checkout, then a line per milestone (its stage
/// mark, id, title, bar and percentage) and a line counting its standalone tasks by stage,
/// with "!N" for the blocked ones. Returns the lines, each `w` wide, and the layout.
fn repo_parts(
    board: &Board,
    r: &Repo,
    w: usize,
    depth: Depth,
) -> (Vec<Line<'static>>, OverviewLayout) {
    let mut about = match &r.lead {
        Some(l) => vec![
            Span::styled(" · ", dim(depth)),
            Span::styled(l.name.clone(), Style::new().fg(accent(depth))),
        ],
        None => vec![Span::styled(" · no lead yet: n starts one", dim(depth))],
    };
    if let Some(link) = &r.link {
        about.push(Span::styled(format!(" · {link}"), dim(depth)));
    }
    let mut left = vec![Span::raw(" "), Span::styled(r.name(), bold())];
    left.extend(about);
    let style = Style::new().fg(accent(depth));
    let min_left = 1 + r.name().width().min(HEADER_TITLE_MIN);
    let (head, at) = with_control(left, min_left, Control::OpenRepo, style, w);
    let mut lines = vec![head];
    let checkout = match &r.path {
        Some(p) => format!(" checkout {p}"),
        None => format!(" checkout not known yet: tower repo set {} --path", r.id),
    };
    lines.push(fit(vec![Span::styled(checkout, dim(depth))], w));
    let mut milestones = Vec::new();
    for m in board.active_of(Some(r.id)) {
        milestones.push((lines.len(), m.id));
        lines.push(milestone_line(m, w, depth));
    }
    if milestones.is_empty() {
        lines.push(fit(
            vec![Span::styled("  no milestones yet", dim(depth))],
            w,
        ));
    }
    let loose: Vec<&Task> = board.standalone_of(Some(r.id)).collect();
    let mut counts = vec![Span::styled(
        format!("  standalone {}", loose.len()),
        dim(depth),
    )];
    for stage in [Stage::Done, Stage::InProgress, Stage::Waiting] {
        let n = loose.iter().filter(|t| t.stage == stage).count();
        if n > 0 {
            counts.push(Span::raw(" "));
            counts.push(Span::styled(
                format!("{}{n}", stage_glyph(stage)),
                stage_style(stage, depth),
            ));
        }
    }
    let blocked = loose.iter().filter(|t| t.details.blocked.is_some()).count();
    if blocked > 0 {
        counts.push(Span::styled(
            format!(" !{blocked}"),
            bold().fg(super::theme::warn(depth)),
        ));
    }
    counts.extend([
        Span::styled(" · ", dim(depth)),
        Span::styled("Done", stage_style(Stage::Done, depth)),
        Span::styled(
            format!(" {}", super::tree::milestones(board.done_of(r.id).len())),
            dim(depth),
        ),
    ]);
    lines.push(fit(counts, w));
    let layout = OverviewLayout {
        lines: lines.len(),
        cards: Vec::new(),
        controls: (at.map(|(from, to)| ControlAt {
            control: Control::OpenRepo,
            line: 0,
            from,
            to,
        }))
        .into_iter()
        .collect(),
        milestones,
        boxes: Vec::new(),
    };
    (lines, layout)
}

/// A milestone's line in a repo or Done group overview: "  ● M2 title" with its bar and
/// percentage at the right, or, for a Done milestone, "  ✓ M3 title" with the date it was
/// marked Done.
fn milestone_line(m: &Milestone, w: usize, depth: Depth) -> Line<'static> {
    let right = if m.stage() == Stage::Done {
        vec![Span::styled(format!(" {} ", done_tally(m)), dim(depth))]
    } else {
        let percent = m.percent();
        let mut right = vec![Span::raw(" ")];
        right.extend(bar(percent, REPO_BAR_CELLS, depth));
        right.push(Span::styled(format!(" {percent:>3}% "), bold()));
        right
    };
    let left = vec![
        Span::raw("  "),
        Span::styled(stage_glyph(m.stage()), stage_style(m.stage(), depth)),
        Span::raw(" "),
        Span::styled(m.id.to_string(), dim(depth)),
        Span::raw(format!(" {}", m.title)),
    ];
    super::tree::split_line(left, right, 8, w)
}

/// A repo's Done group overview, as UI v0.1.7 draws it: " Done  2 milestones · storefront
/// · its lead" with the group's badges at the right, a line saying what the group is, then
/// a line per milestone, most recently done first, which a click selects. Returns the
/// lines, each `w` wide, and the layout.
fn done_parts(
    board: &Board,
    r: &Repo,
    w: usize,
    depth: Depth,
) -> (Vec<Line<'static>>, OverviewLayout) {
    let done = board.done_of(r.id);
    let mut about = format!("  {} · {}", milestones(done.len()), r.name());
    if let Some(l) = &r.lead {
        about.push_str(&format!(" · {}", l.name));
    }
    let left = vec![
        Span::raw(" "),
        Span::styled(
            "Done",
            stage_style(Stage::Done, depth).add_modifier(Modifier::BOLD),
        ),
        Span::styled(about, dim(depth)),
    ];
    let mut right = badge_spans(board.done_badges(r.id), depth);
    if !right.is_empty() {
        right.push(Span::raw(" "));
    }
    let mut lines = vec![super::tree::split_line(left, right, 5, w)];
    lines.push(fit(
        vec![Span::styled(
            " Milestones with the lead's Done mark, most recently done first.",
            Style::new().fg(goal(depth)),
        )],
        w,
    ));
    let mut ids = Vec::new();
    for m in done {
        ids.push((lines.len(), m.id));
        lines.push(milestone_line(m, w, depth));
    }
    let layout = OverviewLayout {
        lines: lines.len(),
        cards: Vec::new(),
        controls: Vec::new(),
        milestones: ids,
        boxes: Vec::new(),
    };
    (lines, layout)
}

/// Where everything in repo `r`'s Done group overview goes in a pane `w` wide.
pub fn done_overview_layout(board: &Board, r: &Repo, w: usize) -> OverviewLayout {
    // Colours never move anything, so any depth will do.
    done_parts(board, r, w, Depth::Ansi256).1
}

/// Repo `r`'s Done group overview in a pane `w` wide.
pub fn done_overview(board: &Board, r: &Repo, w: usize, depth: Depth) -> Overview {
    let (lines, layout) = done_parts(board, r, w, depth);
    Overview {
        lines,
        cards: layout.cards,
    }
}

/// What the milestone's stage line adds about the lead's Done mark: "marked Done
/// 14/01/2027" in the Done colour once set, "Done mark not set" dim while every task is
/// Done without it, else nothing.
pub(crate) fn mark_note(m: &Milestone, depth: Depth) -> Option<Span<'static>> {
    if m.stage() == Stage::Done {
        Some(Span::styled(
            marked_done(m.done_at),
            stage_style(Stage::Done, depth),
        ))
    } else if m.percent() == 100 {
        Some(Span::styled("Done mark not set", dim(depth)))
    } else {
        None
    }
}

/// Where everything in repo `r`'s overview goes in a pane `w` wide.
pub fn repo_overview_layout(board: &Board, r: &Repo, w: usize) -> OverviewLayout {
    // Colours never move anything, so any depth will do.
    repo_parts(board, r, w, Depth::Ansi256).1
}

/// Repo `r`'s overview in a pane `w` wide.
pub fn repo_overview(board: &Board, r: &Repo, w: usize, depth: Depth) -> Overview {
    let (lines, layout) = repo_parts(board, r, w, depth);
    Overview {
        lines,
        cards: layout.cards,
    }
}

/// The overview's first line: " M1  title · lead · link", the milestone in its lead's
/// colour, cut to fit beside "open milestone ▶" (or "open task ▶" with `selected`) at the
/// right, which Enter does too. Returns where the control is.
fn header_line(
    m: &Milestone,
    selected: Option<TaskId>,
    w: usize,
    depth: Depth,
) -> (Line<'static>, Option<ControlAt>) {
    let mut about = match &m.lead {
        Some(l) => format!(" · {}", l.name),
        None => " · no lead yet".to_string(),
    };
    if let Some(link) = &m.details.link {
        about.push_str(&format!(" · {link}"));
    }
    let left = vec![
        Span::raw(" "),
        Span::styled(format!("{}  {}", m.id, m.title), lead_style(m, depth)),
        Span::styled(about, dim(depth)),
    ];
    let control = match selected {
        Some(_) => Control::OpenTask,
        None => Control::OpenMilestone,
    };
    // The id and a few columns of the title stay beside the control.
    let min_left = format!(" {}  ", m.id).width() + HEADER_TITLE_MIN;
    let style = Style::new().fg(accent(depth));
    let (line, at) = with_control(left, min_left, control, style, w);
    let at = at.map(|(from, to)| ControlAt {
        control,
        line: 0,
        from,
        to,
    });
    (line, at)
}

/// The overview of one milestone: its lines, each exactly the pane's width, and its
/// layout, for clicks.
pub struct Overview {
    pub lines: Vec<Line<'static>>,
    pub cards: Vec<Card>,
}

/// The overview's lines in a pane `w` wide, with the card of `selected` highlighted.
pub fn overview(m: &Milestone, selected: Option<TaskId>, w: usize, depth: Depth) -> Overview {
    let layout = overview_layout(m, selected, w);
    let mut lines = vec![header_line(m, selected, w, depth).0];
    let indent = " ".repeat(GOAL_LABEL.width());
    for (i, piece) in goal_pieces(m, w).into_iter().enumerate() {
        let label = if i == 0 {
            Span::styled(GOAL_LABEL, dim(depth))
        } else {
            Span::raw(indent.clone())
        };
        let text = Span::styled(piece, Style::new().fg(goal(depth)));
        lines.push(fit(vec![label, text], w));
    }
    lines.push(progress_line(m, w, depth));
    for b in &layout.boxes {
        debug_assert_eq!(lines.len(), b.top);
        stage_box(m, b, selected, w, depth, &mut lines);
    }
    debug_assert_eq!(lines.len(), layout.lines);
    Overview {
        lines,
        cards: layout.cards,
    }
}

/// The goal's text in pieces beside " goal ", on at most `GOAL_LINES` lines.
fn goal_pieces(m: &Milestone, w: usize) -> Vec<String> {
    let room = w.saturating_sub(GOAL_LABEL.width()).max(1);
    clamp(&m.goal, room, GOAL_LINES)
}

/// " [in progress] ████████░░░░  33% 1/3 tasks done": the stage as a pill in its colour, the
/// 24-cell bar, the percentage and the done count. The bar goes first when it does not fit.
fn progress_line(m: &Milestone, w: usize, depth: Depth) -> Line<'static> {
    let stage = m.stage();
    let pill = Span::styled(format!("[{}]", stage.label()), stage_style(stage, depth));
    let percent = m.percent();
    let mut tail = vec![
        Span::styled(format!("{percent}%"), bold()),
        Span::raw(" "),
        Span::styled(
            format!("{}/{} tasks done", m.done_tasks(), m.tasks.len()),
            dim(depth),
        ),
    ];
    if let Some(note) = mark_note(m, depth) {
        tail.extend([Span::raw(" "), note]);
    }
    let mut spans = vec![Span::raw(" "), pill, Span::raw(" ")];
    let without_bar: usize = spans.iter().chain(&tail).map(|s| s.width()).sum();
    if without_bar + OVERVIEW_BAR_CELLS < w {
        spans.extend(bar(percent, OVERVIEW_BAR_CELLS, depth));
        spans.push(Span::raw(" "));
    }
    spans.extend(tail);
    fit(spans, w)
}

/// Draws one box of stage columns at its layout: a header naming each stage and its count,
/// then a card per task, or "none".
fn stage_box(
    m: &Milestone,
    b: &StageBox,
    selected: Option<TaskId>,
    w: usize,
    depth: Depth,
    lines: &mut Vec<Line<'static>>,
) {
    let rule = |l: &str, mid: &str, r: &str| {
        let mut s = String::from(l);
        for (i, (_, cw)) in b.columns.iter().enumerate() {
            if i > 0 {
                s.push_str(mid);
            }
            s.push_str(&"─".repeat(*cw));
        }
        s.push_str(r);
        fit(vec![Span::raw(" "), Span::styled(s, dim(depth))], w)
    };
    // A row of cells, each already exactly its column's width, between the box's borders.
    let row = |cells: Vec<Vec<Span<'static>>>| {
        let mut spans = vec![Span::raw(" ")];
        for cell in cells {
            spans.push(Span::styled("│", dim(depth)));
            spans.extend(cell);
        }
        spans.push(Span::styled("│", dim(depth)));
        fit(spans, w)
    };
    lines.push(rule("┌", "┬", "┐"));
    let heads = (b.columns.iter())
        .map(|(st, cw)| {
            let name = Span::styled(
                format!(" {}", st.label().to_uppercase()),
                stage_style(*st, depth).add_modifier(Modifier::BOLD),
            );
            let count = Span::styled(format!(" {}", tasks_at(m, *st).count()), dim(depth));
            fit(vec![name, count], *cw).spans
        })
        .collect();
    lines.push(row(heads));
    lines.push(rule("├", "┼", "┤"));
    let bodies: Vec<Vec<Vec<Span<'static>>>> = (b.columns.iter())
        .map(|(st, cw)| {
            let body: Vec<_> = tasks_at(m, *st)
                .flat_map(|t| card(t, m, selected == Some(t.id), *cw, depth))
                .collect();
            if body.is_empty() {
                vec![fit(vec![Span::styled(" none", dim(depth))], *cw).spans]
            } else {
                body
            }
        })
        .collect();
    for y in 0..b.body {
        let cells = (bodies.iter().zip(&b.columns))
            .map(|(body, (_, cw))| match body.get(y) {
                Some(cell) => cell.clone(),
                None => vec![Span::raw(" ".repeat(*cw))],
            })
            .collect();
        lines.push(row(cells));
    }
    lines.push(rule("└", "┴", "┘"));
}

/// A task's card `w` wide, one line: " #13 dev-2 · 1/3 !", its number dim, who is on it,
/// its subtasks done of all (dim) and a bold amber "!" while it is blocked; the selected
/// card on the selected background.
fn card(
    t: &Task,
    m: &Milestone,
    selected: bool,
    w: usize,
    depth: Depth,
) -> Vec<Vec<Span<'static>>> {
    // The "!" is the only blocked signal on the board's columns, so it never goes: short
    // of room the assignee is cut first, then the subtasks go.
    // Its ticket, from the tracker link, where one is: tower's id is not on a card.
    let ticket = t.ticket().map(|k| k.to_string());
    let id = ticket
        .as_ref()
        .map_or(" ".to_string(), |k| format!(" {k} "));
    let bang = if t.details.blocked.is_some() { 2 } else { 0 };
    let mut subs = match t.subtasks.len() {
        0 => String::new(),
        n => format!(" · {}/{n}", t.done_subtasks()),
    };
    let who = t.who(m.lead.as_ref()).to_string();
    let room = |subs: &str| w.saturating_sub(id.width() + subs.width() + bang);
    if room(&subs) < who.width().min(4) {
        subs.clear();
    }
    let who_room = room(&subs).min(who.width());
    let mut s = vec![Span::raw(" ")];
    if let Some(k) = ticket {
        s.push(Span::styled(k, Style::new().fg(accent(depth))));
        s.push(Span::raw(" "));
    }
    s.extend(fit(vec![Span::raw(who)], who_room).spans);
    if !subs.is_empty() {
        s.push(Span::styled(subs, dim(depth)));
    }
    if bang > 0 {
        s.push(Span::raw(" "));
        s.push(Span::styled("!", bold().fg(warn(depth))));
    }
    let mut line = fit(s, w);
    if selected {
        // On the selected background, not bold, as the copy has it.
        let bg = Style::new().bg(selected_bg(depth));
        for span in &mut line.spans {
            span.style = bg.patch(span.style);
        }
    }
    vec![line.spans]
}
