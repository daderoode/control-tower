//! The dashboard, `tower ui`: a read-only view of the board that refreshes on its own.
//!
//! The tree is on the left; the view of whatever is selected in it is on the right: the
//! milestone overview, a detail view, or a standalone task's detail straight away. `layout` works out where each pane goes and what a click
//! lands on, from the latest `View`, the `Dash` (selection, open views, scroll) and the
//! terminal's size; `draw` paints it and `Dash::handle` applies a key or mouse input. All
//! three are pure, so they are tested with ratatui's `TestBackend`. `run` is the edge.
//!
//! - `theme`: colours, styles and the exact-width rule.
//! - `control`: the clickable controls, each a key: "open task ▶", "◀ close" and the like.
//! - `tree` and `overview`: what each pane says, and the overview's layout.
//! - `detail`: the milestone and task detail views, opened over the overview.
//! - `message_board`: the lead's message board summary under the overview.
//! - `full_board`: the lead's full message board, opened from a thread or with `b`, with
//!   the composer the owner posts from as `manager`, through `tower post` and `runner`.
//! - `inbox`: the alert line under the header and the inbox it opens (with `i`).
//! - `layout`: the panes and hit-testing; `dash`: the state and its inputs.
//! - `runner`: runs a bounded command off the calling thread; the watch runs its `herdr`
//!   commands through it.
//! - `writer`: the owner's own writes, a post as `manager` and marking an inbox item read,
//!   through the store on a thread of their own; the CLI refuses both.
//! - `term`: the terminal and the input loop.

mod chat;
mod control;
mod dash;
mod detail;
mod full_board;
mod inbox;
mod layout;
mod link;
mod message_board;
mod overview;
pub(crate) mod runner;
mod term;
mod theme;
mod threads;
mod tree;
pub mod writer;

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

pub use chat::People;
pub use control::{Control, ControlAt};
pub use dash::{Dash, Input, Item, Request, ViewKind};
pub use detail::{Detail, DetailLayout, TaskRow, detail_layout, milestone_detail, task_detail};
pub use full_board::{Anchor, BoardLayout, ResolvedFold, Stop};
pub use inbox::{InboxLayout, alert_line};
pub use layout::{Layout, layout};
pub use link::{Link, hyperlink};
pub use message_board::{Limits, Shown, role_colour};
pub use overview::{Card, Overview, OverviewLayout, overview, overview_layout};
pub use term::run;
pub use theme::{
    Depth, accent, bad, dim_colour, lead_colour, ok, selected_bg, stage_mark, stage_style, warn,
};
pub use tree::{Fold, tree_items, tree_lines};

use crate::board::{Board, Repo, Talk, Thread, ThreadId};
use crate::store::Store;
use dash::repo_of;
use layout::{body_area, clamp_scroll};
use theme::{bold, dim, fit};

/// Where the dashboard runs: in its own herdr workspace (or any pane), or in herdr's popup
/// over the work, which `tower popup` opens with `TOWER_UI_MODE=popup`. `tower space` opens
/// it with `TOWER_UI_MODE=space`, which lays it out as any pane; what `q` does there is the
/// edge's (`term::run`). In the popup the
/// message board is compact (`message_board::Limits`), Esc at the top level closes it, and
/// the keys line ends "esc back · q close".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Space,
    Popup,
}

impl Mode {
    /// The mode `TOWER_UI_MODE` names: unset, empty or `space` for its own space, `popup`
    /// for the popup. Any other value is refused, naming it.
    pub fn from_env(value: Option<&str>) -> anyhow::Result<Mode> {
        match value {
            None | Some("" | "space") => Ok(Mode::Space),
            Some("popup") => Ok(Mode::Popup),
            Some(other) => anyhow::bail!(
                "TOWER_UI_MODE is {other:?}: leave it unset, or set it to space or popup"
            ),
        }
    }
}

/// Whether `q` returns to the workspace `tower space` was pressed in, the dashboard kept
/// running, rather than closing it: only in its own space, which `TOWER_UI_MODE` (`value`)
/// names `space`. Unset or empty (any pane) and `popup` close.
pub fn q_returns(value: Option<&str>) -> bool {
    value == Some(crate::open::SPACE_MODE)
}

/// What the latest refresh found: the board, or why it could not be read.
pub type View = std::result::Result<crate::board::Board, String>;

/// Reads the board afresh, read-only. A new connection each time, so a board file that was
/// replaced or deleted since the last refresh is seen as it is now, never as stale rows.
pub fn refresh(path: &Path) -> View {
    Store::open_read_only(path)
        .and_then(|mut store| store.load())
        .map_err(|e| format!("{e:#}"))
}

/// Reads the board for the dashboard, off the UI thread: afresh each time, as `refresh`
/// does, with the whole history of the thread the full board shows filled in. That history
/// is kept and read again only when the thread's message count changes.
pub struct Reader {
    path: PathBuf,
    /// The whole histories last read: the shown thread's and those folded into it.
    whole: Vec<Thread>,
    /// How many times a thread's whole history was read, for tests.
    reads: usize,
}

impl Reader {
    pub fn new(path: &Path) -> Reader {
        Reader {
            path: path.to_path_buf(),
            whole: Vec::new(),
            reads: 0,
        }
    }

    /// How many times a thread's whole history has been read.
    pub fn reads(&self) -> usize {
        self.reads
    }

    /// The board as it is now, with the whole history of `thread`'s conversation when it is
    /// on it: the thread and those folded into it (`Talk::joined`). A thread gone between
    /// the refresh and the read leaves the refreshed view as it is.
    pub fn read(&mut self, thread: Option<ThreadId>) -> View {
        let mut view = refresh(&self.path);
        if let (Ok(board), Some(id)) = (&mut view, thread) {
            let joined = (board.repos.iter())
                .find(|r| r.threads.iter().any(|t| t.id == id))
                .map(|r| {
                    let talk = Talk::of_repo(board, r);
                    talk.joined(talk.home_of(id))
                })
                .unwrap_or_else(|| vec![id]);
            let kept = std::mem::take(&mut self.whole);
            for t in joined {
                self.fill(board, t, &kept);
            }
        }
        view
    }

    /// Fills thread `id` on `board` with its whole history: from `kept` (the last read)
    /// while its count is unchanged, else from the store.
    fn fill(&mut self, board: &mut Board, id: ThreadId, kept: &[Thread]) {
        let Some(t) = (board.repos.iter_mut())
            .flat_map(|r| r.threads.iter_mut())
            .find(|t| t.id == id)
        else {
            return;
        };
        if t.messages.len() >= t.total {
            return;
        }
        if let Some(w) = kept.iter().find(|w| w.id == id && w.total == t.total) {
            *t = w.clone();
            self.whole.push(w.clone());
            return;
        }
        // On a failure (the thread removed meanwhile) the conversation shows only its latest
        // messages, saying how many earlier ones are not loaded, and the next refresh retries.
        if let Ok(whole) = Store::open_read_only(&self.path).and_then(|mut s| s.thread(id)) {
            self.reads += 1;
            self.whole.push(whole.clone());
            *t = whole;
        }
    }
}

/// Draws one frame, with the header's `clock` and every message age as of the dashboard's
/// minute (`Dash::minute`), the clock the layout uses, so a message shows one age wherever
/// it is on the frame. When the last refresh failed, its problem takes the body's place, so
/// a broken or missing board is visible rather than shown as stale rows.
pub fn draw(frame: &mut Frame, view: &View, dash: &Dash, clock: &str, depth: Depth) -> Vec<Link> {
    let now = dash.minute();
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let w = area.width as usize;
    let row = |y: u16| Rect::new(area.x, area.y + y, area.width, 1);
    let l = layout(view, dash, area);

    if let Some(c) = l.clock {
        let mut text = Line::styled(format!(" {clock:>5} "), dim(depth));
        // On the alert line, in its tint.
        if l.alert.is_some() {
            text.style = inbox::alert_bg(depth);
        }
        frame.render_widget(Paragraph::new(text), c);
    }
    if area.height >= 5 {
        let rule = Line::styled("─".repeat(w), dim(depth));
        frame.render_widget(Paragraph::new(rule), row(area.height - 2));
        let running = (dash.handing.clone())
            .or(dash.posting.map(|t| format!("posting to {t}…")))
            .map(|r| (r, false));
        let notice = running.or(dash.notice.clone());
        let keys = match (dash.new_lead(), &notice) {
            (Some(goal), _) => {
                // Where it starts: the repo the input opened on, or no repo yet.
                let repo = (view.as_ref().ok())
                    .zip(dash.new_lead_repo())
                    .and_then(|(b, r)| b.repo(r))
                    .map(|r| r.name());
                new_lead_line(goal, repo.as_deref(), dash.notice.as_ref(), w, depth)
            }
            (None, notice) => match notice {
                Some((text, failed)) => {
                    let style = if *failed {
                        Style::new().fg(bad(depth))
                    } else {
                        Style::new()
                    };
                    fit(vec![Span::styled(format!(" {}", one_line(text)), style)], w)
                }
                None => keys_line(view, &l, dash, w, depth),
            },
        };
        frame.render_widget(Paragraph::new(keys), row(area.height - 1));
    }
    let board = match view {
        Ok(board) => board,
        Err(problem) => {
            // The problem wraps over as many lines as it needs: it names the board's path,
            // which is too useful to cut off.
            let p = Paragraph::new(Line::styled(
                format!(" {problem}"),
                Style::new().fg(bad(depth)),
            ))
            .wrap(Wrap { trim: false });
            frame.render_widget(p, body_area(area));
            return Vec::new();
        }
    };
    if let Some(a) = l.alert {
        let aw = usize::from(a.width);
        frame.render_widget(
            Paragraph::new(inbox::alert_line(board, aw, Some(now), depth)),
            a,
        );
    }
    if let Some(ib) = &l.inbox {
        let tw = usize::from(ib.top.width);
        frame.render_widget(Paragraph::new(inbox::top_line(tw, depth)), ib.top);
        let sel = dash.inbox_selected(board);
        let lines = inbox::list_lines(board, sel, usize::from(ib.list.width), now, depth);
        render_scrolled(frame, lines, dash.inbox_scroll(), ib.list);
        return Vec::new();
    }
    let selected = dash.selection(board);
    if let (Some(b), Some(r)) = (&l.board, selected.and_then(|s| repo_of(board, s))) {
        return draw_board(frame, board, r, b, dash, now, depth);
    }
    if let Some(t) = l.tree {
        let shown = dash.tree_selection(board);
        let lines = tree::tree_lines_selected(board, usize::from(t.width), depth, shown);
        render_scrolled(frame, lines, dash.tree_scroll, t);
        if l.right.is_some() {
            let sep = Rect::new(t.x + t.width, t.y, 1, t.height);
            let bar = vec![Line::styled("│", dim(depth)); usize::from(sep.height)];
            frame.render_widget(Paragraph::new(bar), sep);
        }
    }
    let mut links = Vec::new();
    if let (Some(r), Some(sel)) = (l.right, selected) {
        let rw = usize::from(r.width);
        // The URL each line shows a piece of, if any: only the message board's do.
        let mut urls = Vec::new();
        let lines = match (dash.shown(board), layout::overview_of(board, sel)) {
            (ViewKind::Overview, Some((over, talk, task))) => {
                let mut lines = match over {
                    layout::Over::Repo(r) => overview::repo_overview(board, r, rw, depth).lines,
                    layout::Over::Done(r) => overview::done_overview(board, r, rw, depth).lines,
                    layout::Over::Milestone(m) => overview(m, task, rw, depth).lines,
                };
                let limits = message_board::Limits::of(dash.mode, dash.board_shown(sel), now);
                urls = vec![None; lines.len()];
                urls.extend(message_board::links(&talk, task, rw, limits));
                lines.extend(message_board::lines(&talk, task, rw, limits, depth));
                lines
            }
            (ViewKind::Overview, None) => Vec::new(),
            (kind, _) => detail::detail(board, kind, sel, rw, dash.board_shown(sel), now, depth)
                .map(|d| d.lines)
                .unwrap_or_default(),
        };
        let scroll = clamp_scroll(dash.right_scroll(), lines.len(), r.height);
        links = link::placed(&lines, &urls, scroll, r);
        render_scrolled(frame, lines, scroll, r);
    }
    links
}

/// Draws repo `r`'s full board as `b` lays it out.
fn draw_board(
    frame: &mut Frame,
    board: &Board,
    r: &Repo,
    b: &BoardLayout,
    dash: &Dash,
    now: i64,
    depth: Depth,
) -> Vec<Link> {
    let m = &Talk::of_repo(board, r);
    let t = full_board::shown(m, b.thread);
    let top = full_board::top_line(&r.name(), m, usize::from(b.top.width), depth);
    frame.render_widget(Paragraph::new(top), b.top);
    let sw = usize::from(b.strip.width);
    let strip = threads::strip(m, &m.needs, sw, b.strip_room, now, depth);
    frame.render_widget(Paragraph::new(strip), b.strip);
    let list = full_board::list_lines(m, b, usize::from(b.list.width), now, depth);
    render_scrolled(frame, list, dash.list_scroll(), b.list);
    let (sep, bar) = if b.beside {
        let sep = Rect::new(b.list.x + b.list.width, b.list.y, 1, b.list.height);
        (
            sep,
            vec![Line::styled("│", dim(depth)); usize::from(sep.height)],
        )
    } else {
        let sep = Rect::new(b.list.x, b.list.y + b.list.height, b.list.width, 1);
        let rule = "─".repeat(usize::from(sep.width));
        (sep, vec![Line::styled(rule, dim(depth))])
    };
    if b.head.height > 0 || b.conv.height > 0 {
        frame.render_widget(Paragraph::new(bar), sep);
    }
    let rw = usize::from(b.head.width);
    let head = full_board::head_lines(m, t, rw, b.beside, Some(now), depth);
    frame.render_widget(Paragraph::new(head), b.head);
    let mut links = Vec::new();
    if let Some(t) = t {
        let (open, selected) = (dash.open_messages(), dash.board_message());
        // Only the lines that show are styled.
        let top = b.conv_top(dash.board_anchor());
        let rows = top..top + usize::from(b.conv.height);
        // Above the list (a narrow pane), bubbles take the chat's whole width.
        let full = !b.beside;
        let urls = full_board::conv_links(m, t, rw, full, open, now, rows.clone());
        let conv = full_board::conv_window(m, t, rw, full, open, selected, now, depth, rows);
        links = link::placed(&conv, &urls, 0, b.conv);
        render_scrolled(frame, conv, 0, b.conv);
        let draft = dash.draft(t.id);
        let msgs = m.conversation(t);
        // The question from the chat, or, older than the loaded window, from its inbox item.
        let reply = dash.replying().and_then(|id| {
            (msgs.iter().find(|x| x.id == id))
                .or_else(|| (m.needs.iter()).map(|a| &a.message).find(|x| x.id == id))
        });
        let at = dash.draft_at(t.id);
        let keys = b.writes_to().is_some();
        let rows = b.input.height;
        let writing = full_board::Writing {
            draft,
            at,
            keys,
            rows,
        };
        let composer = full_board::composer_lines(m, t, writing, reply, rw, depth);
        if b.input.height > 0 {
            // The rule on top, then the reply line where the layout kept it a row, then the
            // input's rows (the window around the caret) where the input is: never over the
            // rule.
            let mut lines = composer.into_iter();
            let rule = lines.next();
            let replying = if reply.is_some() { lines.next() } else { None };
            let input: Vec<Line> = lines.collect();
            let gap = usize::from(b.input.y.saturating_sub(b.composer.y + 1));
            let mut shown: Vec<Line> = rule.into_iter().collect();
            if gap > 0 {
                shown.push(replying.unwrap_or_default());
            }
            shown.extend(input);
            render_scrolled(frame, shown, 0, b.composer);
        } else if t.status() == crate::board::ThreadStatus::Open {
            // No row under the rule for an input: the rule (and reply line) from the top.
            render_scrolled(frame, composer, 0, b.composer);
        } else {
            // Its last lines, when there is no room for the rule.
            let skip = composer
                .len()
                .saturating_sub(usize::from(b.composer.height));
            render_scrolled(frame, composer, skip, b.composer);
        }
        // The caret: the keys write to an open thread.
        if keys && dash.posting.is_none() {
            frame.set_cursor_position(full_board::cursor(b.input, draft, at));
        }
    }
    links
}

/// Renders the lines of a pane from `scroll`, kept within the content.
fn render_scrolled(frame: &mut Frame, lines: Vec<Line<'static>>, scroll: usize, area: Rect) {
    let scroll = clamp_scroll(scroll, lines.len(), area.height);
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .take(usize::from(area.height))
        .collect();
    frame.render_widget(Paragraph::new(shown), area);
}

/// `text` on one line: every run of whitespace or control characters (line breaks, escape
/// codes from a command's stderr) becomes one space, so it can never break the keys row.
fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The new lead's input in the keys line's place: " new lead on <repo>, its goal: <goal>▏"
/// (" new lead in no repo yet, ..." with no `repo`), the goal's end in view when it is
/// longer than the line, then its keys (or why Enter or a key was refused, in red) at the
/// right while they fit.
fn new_lead_line(
    goal: &str,
    repo: Option<&str>,
    refused: Option<&(String, bool)>,
    w: usize,
    depth: Depth,
) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let label = match repo {
        Some(r) => format!(" new lead on {r}, its goal: "),
        None => " new lead in no repo yet, its goal: ".to_string(),
    };
    let right = match refused {
        Some((why, _)) => Span::styled(format!(" {why} "), Style::new().fg(bad(depth))),
        None => Span::styled(" enter start · esc cancel ", dim(depth)),
    };
    let right = (label.width() + 12 + right.width() <= w).then_some(right);
    let room = w
        .saturating_sub(label.width() + 1 + right.as_ref().map_or(0, Span::width))
        .max(1);
    // The end of the goal that fits, whole characters only.
    let mut shown = String::new();
    for c in goal.chars().rev() {
        if shown.width() + c.to_string().width() > room {
            break;
        }
        shown.insert(0, c);
    }
    let mut spans = vec![
        Span::styled(label, bold()),
        Span::raw(shown),
        Span::raw("▏"),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    if let Some(r) = right {
        spans.push(Span::raw(" ".repeat(w.saturating_sub(used + r.width()))));
        spans.push(r);
    }
    fit(spans, w)
}

/// The keys that work now, as many as fit, whole: "q quit" first, then moving, folding
/// (only when a tree has milestones), opening and going back, the board,
/// inbox and new lead keys (in the inbox, `r` as "r clear" on a need-you item), scrolling,
/// and clicking. They show in reading order. In the popup, "esc back · q close" at the end.
/// A key that would do nothing is not offered: an empty inbox has only the way out.
fn keys_line(view: &View, l: &Layout, dash: &Dash, w: usize, depth: Depth) -> Line<'static> {
    // (priority, hint) in reading order; the lowest priorities are left out first.
    let mut hints = vec![];
    if l.inbox.is_some() {
        // r clears a need-you item and marks any other read.
        let board = view.as_ref().ok();
        let at = board.and_then(|b| dash.inbox_selected(b));
        let need_you = (board.zip(at))
            .is_some_and(|(b, id)| b.inbox.need_you.iter().any(|a| a.message.id == id));
        if at.is_none() {
            // An empty inbox: nothing to select, open or mark, only the way out.
            hints.extend([(2, "esc back"), (0, "q quit")]);
        } else {
            hints.extend([
                (1, "↑↓ select"),
                (1, "enter open thread"),
                (2, if need_you { "r clear" } else { "r mark read" }),
                (2, "esc back"),
                (3, "PgUp PgDn scroll"),
                (4, "click an item"),
                (0, "q quit"),
            ]);
        }
    } else if let Some(b) = &l.board {
        // On an open thread the keys write: q is typed, and Ctrl-C quits, named only
        // where the copy's hints leave room.
        let writing = b.writes_to().is_some();
        // In the chat the keys move between messages; in the list, between threads.
        if dash.board_message().is_some() {
            hints.extend([
                (1, "↑↓ message"),
                // Kept with the writing hints at 120 columns, as the copy has them.
                (2, "→ read more or less"),
                (2, "← threads"),
            ]);
        } else if b.fold.on_row {
            let fold = if b.fold.unfolded {
                "← fold"
            } else {
                "→ unfold"
            };
            hints.extend([(1, "↑↓ threads"), (2, fold)]);
        } else {
            hints.push((1, "↑↓ threads"));
            // Only when the thread has a message → can select: a system line is none.
            if !b.starts.is_empty() {
                hints.push((2, "→ read its messages"));
            }
            if b.on_resolved_thread() {
                hints.push((3, "← fold resolved"));
            }
        }
        // As UI v0.1.13 names writing: not on the RESOLVED row, which the keys are on.
        if writing && !b.fold.on_row {
            hints.extend([
                (1, "type to write"),
                (1, "enter posts"),
                (2, "alt+enter new line"),
            ]);
        }
        hints.extend([
            (2, "esc back"),
            (3, "PgUp PgDn scroll"),
            (4, "click a thread"),
            if writing {
                (4, "ctrl-c quit")
            } else {
                (0, "q quit")
            },
        ]);
    } else {
        hints.push((1, "↑↓ select"));
        let milestones = view.as_ref().is_ok_and(|b| !b.milestones.is_empty());
        // As in the design's compact line, folding stays when scrolling has to go, and
        // goes before b, i and n, which showed wherever they fit before folding came in.
        if l.tree.is_some() && milestones {
            hints.extend([(4, "←→ fold"), (4, "z fold all")]);
        }
        let shown = view.as_ref().map(|b| dash.shown(b)).ok();
        let can_open =
            l.overview_hidden || (l.right.is_some() && shown == Some(ViewKind::Overview));
        if can_open {
            hints.push((2, "enter open"));
        }
        if dash.can_go_back(l) {
            hints.push((2, "esc back"));
        }
        // A task selected: `o` opens its ticket.
        let task = view
            .as_ref()
            .ok()
            .is_some_and(|b| matches!(dash.selection(b), Some(Item::Task(_))));
        if task {
            hints.push((3, "o open ticket"));
        }
        if l.has_board {
            hints.push((3, "b board"));
        }
        hints.push((3, "i inbox"));
        hints.push((3, "n new lead"));
        hints.extend([
            (5, "PgUp PgDn scroll"),
            (6, "click a row or card"),
            (0, "q quit"),
        ]);
    }
    // In the popup the line ends "esc back · q close": Esc steps back, and closes the popup
    // with nothing to step back from. While writing q is typed, so "q close" is left out.
    if dash.mode == Mode::Popup && hints.contains(&(0, "q quit")) {
        hints.retain(|(_, h)| *h != "q quit" && *h != "esc back");
        hints.extend([(0, "esc back"), (0, "q close")]);
    }
    let line = |most: usize| {
        let shown: Vec<&str> = (hints.iter())
            .filter(|(p, _)| *p <= most)
            .map(|(_, h)| *h)
            .collect();
        format!(" {}", shown.join(" · "))
    };
    let text = (0..=6)
        .rev()
        .map(line)
        .find(|t| unicode_width::UnicodeWidthStr::width(t.as_str()) <= w)
        .unwrap_or_else(|| line(0));
    fit(vec![Span::styled(text, dim(depth))], w)
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::board::{BoardOf, ThreadStatus};
    use crate::store::FlagChange;

    #[test]
    fn a_folded_thread_longer_than_its_loaded_window_is_counted_and_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let mut store = Store::open(&path).unwrap();
        let m = store.add_milestone("m", "g", None).unwrap();
        store.lead_milestone(m, "Lead", "lead", None).unwrap();
        let task = store.add_task(m, "picture-book shelf").unwrap();
        // The watch posts first: a thread only tower posts in, then 29 more nudges.
        let now = 2_000_000_000;
        let tower = (store.watch_step(task, FlagChange::None, Some("nudged dev-1: a"), now))
            .unwrap()
            .unwrap();
        for i in 1..30 {
            store.post_at(tower, "system", &format!("nudged dev-1: {i}"), now + i);
        }
        // Then the people thread on the task: the tower thread's home.
        let (home, _) = store
            .open_thread(&BoardOf::Milestone(m), "dev-1", Some(task), "team", "on it")
            .unwrap();
        let mut reader = Reader::new(&path);
        let view = reader.read(None);
        let board = view.as_ref().unwrap();
        let r = &board.repos[0];
        let talk = Talk::of_repo(board, r);
        let t = *talk.threads.iter().find(|t| t.id == home).unwrap();
        assert_eq!(t.status(), ThreadStatus::Open);
        assert_eq!(talk.home_of(tower), home);
        // The header counts 31; the summary loads 12 of the tower thread's 30: 18 earlier.
        assert_eq!(talk.total(t), 31);
        assert_eq!(talk.missing(t), 18);
        // Reading the conversation reads both threads whole: nothing earlier.
        let view = reader.read(Some(home));
        let board = view.as_ref().unwrap();
        let talk = Talk::of_repo(board, &board.repos[0]);
        let t = *talk.threads.iter().find(|t| t.id == home).unwrap();
        assert_eq!(talk.missing(t), 0);
        assert_eq!(talk.conversation(t).len(), 31);
        assert_eq!(reader.reads(), 1, "the people thread was whole already");
        // A second read keeps what it read; a new nudge reads that thread again.
        assert!(reader.read(Some(home)).is_ok());
        assert_eq!(reader.reads(), 1);
        store.post_at(tower, "system", "nudged dev-1: more", now + 100);
        assert!(reader.read(Some(home)).is_ok());
        assert_eq!(reader.reads(), 2);
    }
}
