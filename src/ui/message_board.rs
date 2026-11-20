//! The message board summary under an overview, as UI v0.1.7 draws it: a box per open
//! thread, each a compact chat preview (`chat`): its last bubbles, each with its last two
//! messages folded to two lines, after a line counting the messages before them. In its own
//! space an open thread shows three bubbles; in herdr's popup two and three open threads at
//! most, then "… N more open threads, open the full board ▶" (`Limits`). Resolved threads
//! fold into one row, "RESOLVED 3 ▸", as on the full board (UI v0.1.13): a click
//! unfolds it to one line per thread (✓, name, age), and a click on a line opens that thread
//! on the full board. With a task selected it shows only that task's threads.
//!
//! `plan` says what each line is without styling it, so the overview's height (for
//! scrolling) and the lines drawn come from the same rows; `lines` styles them. The plan
//! never depends on the time, so the height does not change as ages grow.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use icu_properties::CodePointMapData;
use icu_properties::props::{EastAsianWidth, GeneralCategory, LineBreak};
use icu_segmenter::LineSegmenter;
use icu_segmenter::LineSegmenterBorrowed;
use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::Mode;
use super::chat;
use super::control::{Control, ControlAt, with_control};
use super::full_board;
use super::theme::{Depth, accent, bad, bold, dim, fit, info, ok, warn};
use super::threads;
use crate::board::{
    Lead, Message, Role, Talk, TaskId, Thread, ThreadId, ThreadStatus, owner_mentions,
};
use crate::inbox::Alert;

/// How much of the board the summary shows: how many open threads (all with `None`) and
/// how many of each one's latest bubbles. The board loads the latest messages they come from
/// (`ThreadStatus::shown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits<'a> {
    pub threads: Option<usize>,
    pub open: usize,
    pub shown: Shown<'a>,
}

/// How the owner left a summary: the messages opened in full ("read more"), by id, and
/// whether its RESOLVED row is unfolded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown<'a> {
    pub opened: &'a BTreeSet<i64>,
    pub unfolded: bool,
}

impl<'a> Limits<'a> {
    /// Its own space: every open thread with its last three bubbles. The popup: three open
    /// threads, their last two bubbles and the evidence.
    pub fn of(mode: Mode, shown: Shown<'a>) -> Limits<'a> {
        match mode {
            Mode::Space => Limits {
                threads: None,
                open: 3,
                shown,
            },
            Mode::Popup => Limits {
                threads: Some(3),
                open: 2,
                shown,
            },
        }
    }
}

/// One line of the summary, unstyled.
enum Row<'a> {
    Blank,
    Title,
    /// A dim line saying why there is nothing to show.
    Empty(&'static str),
    Top(ThreadStatus),
    Bottom(ThreadStatus),
    /// A line of the "Needs you" strip pinned under the title.
    Strip(usize),
    /// A line of a thread's header in words (`threads::head`).
    Head(&'a Thread, usize),
    /// A line of the thread's chat preview.
    Chat(&'a Thread, chat::Row),
    /// "open chat ▶" at the right, under a thread's preview.
    OpenChat(ThreadStatus),
    /// "… N more open threads, open the full board ▶", for the open threads past
    /// `Limits::threads`.
    More(usize),
    /// "RESOLVED 3 ▸", unfolded ("▾") or not.
    Resolved(usize),
    /// A resolved thread's line under the unfolded RESOLVED row: ✓, its name, its age.
    Line(&'a Thread),
}

/// The summary's rows.
struct Plan<'a> {
    rows: Vec<Row<'a>>,
}

/// How many messages of a preview's bubble it shows, its latest.
const PREVIEW_SEGS: usize = 2;
/// How many lines of a message a preview shows before "read more".
const PREVIEW_CLIP: usize = 2;

pub(crate) fn pill(t: &Thread) -> String {
    format!("[{}]", t.status().name())
}

/// The summary's rows for a pane `w` wide. Each box sits one column in from each side, its
/// contents one column in from its borders: `w - 6` columns of content.
fn plan<'a>(talk: &Talk<'a>, task: Option<TaskId>, w: usize, limits: Limits) -> Plan<'a> {
    let empty = |why| Plan {
        rows: vec![Row::Blank, Row::Title, Row::Empty(why)],
    };
    if talk.lead.is_none() {
        return empty(" no lead yet, so no message board");
    }
    let (mut shown, resolved): (Vec<&Thread>, Vec<&Thread>) =
        (talk.ordered(task).into_iter()).partition(|t| t.status() == ThreadStatus::Open);
    let more = limits.threads.map_or(0, |n| shown.len().saturating_sub(n));
    shown.truncate(shown.len() - more);
    // Every board with a lead has its "Needs you" strip, even with no thread yet; the pane
    // scrolls, so it lists every question.
    let mut rows = vec![Row::Blank, Row::Title];
    let strip = threads::strip_height(needs_of(talk, task).len(), usize::MAX);
    rows.extend((0..strip).map(Row::Strip));
    if shown.is_empty() && resolved.is_empty() {
        let why = match task {
            Some(_) => " no thread for this task yet",
            None => " no threads yet",
        };
        rows.push(Row::Empty(why));
        return Plan { rows };
    }
    let content = w.saturating_sub(6);
    for t in shown {
        let status = t.status();
        rows.push(Row::Top(status));
        let head = threads::head(talk, t, content + 1, None, true, Depth::Ansi256).len();
        rows.extend((0..head).map(|k| Row::Head(t, k)));
        let shape = chat::Shape {
            clip: PREVIEW_CLIP,
            open: limits.shown.opened,
            now: None,
            preview: Some((limits.open, PREVIEW_SEGS)),
            people: talk.people(),
            full: false,
        };
        // The board loads only the latest messages: the rest are counted from the total.
        let missing = talk.missing(t);
        for r in chat::plan(&talk.conversation(t), missing, content, shape) {
            rows.push(Row::Chat(t, r));
        }
        rows.push(Row::OpenChat(status));
        rows.push(Row::Bottom(status));
    }
    if more > 0 {
        rows.push(Row::More(more));
    }
    if !resolved.is_empty() {
        rows.push(Row::Resolved(resolved.len()));
        if limits.shown.unfolded {
            rows.extend(resolved.into_iter().map(Row::Line));
        }
    }
    Plan { rows }
}

/// The open questions for the owner the summary's strip lists: all of the board's, or
/// with a task selected, those in its threads' conversations.
fn needs_of<'a>(talk: &Talk<'a>, task: Option<TaskId>) -> Vec<&'a Alert> {
    let in_task = |a: &&Alert| {
        let home = talk.home_of(a.thread);
        task.is_none() || talk.threads.iter().any(|t| t.id == home && t.task == task)
    };
    talk.needs.iter().copied().filter(in_task).collect()
}

/// `line`'s spans less its first column, a space.
fn without_lead_space(line: &Line<'static>) -> Vec<Span<'static>> {
    let mut spans = line.spans.clone();
    if let Some(first) = spans.first_mut()
        && let Some(rest) = first.content.strip_prefix(' ')
    {
        first.content = rest.to_string().into();
    }
    spans
}

/// The thread boxes alone (or the line saying why there are none), without the summary's
/// blank line and title: for a detail view, under its own heading.
pub fn boxes(
    talk: &Talk,
    task: Option<TaskId>,
    w: usize,
    limits: Limits,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    let mut all = lines(talk, task, w, limits, now, depth);
    // Every plan starts with the blank line and the title.
    all.drain(..2);
    all
}

/// Where a thread's box is in the summary: `height` lines from `line`, counted from the
/// summary's blank first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadRow {
    pub thread: ThreadId,
    pub line: usize,
    pub height: usize,
    /// On a "read more" or "show less" line, the message a click there opens or folds in
    /// place, rather than opening the thread.
    pub toggle: Option<i64>,
    /// On a line of the "Needs you" strip, the question it is: a click opens the full
    /// board on it, replying.
    pub message: Option<i64>,
}

/// Where each thread's box is in the summary in a pane `w` wide, for clicks: a click
/// anywhere in a box, its "… N earlier" line included, opens that thread.
pub fn thread_rows(talk: &Talk, task: Option<TaskId>, w: usize, limits: Limits) -> Vec<ThreadRow> {
    let mut out: Vec<ThreadRow> = Vec::new();
    let mut start = None;
    // The box being laid out, by its index in `out`; its toggles follow it.
    let mut open_box = None;
    let needs = needs_of(talk, task);
    for (i, row) in plan(talk, task, w, limits).rows.iter().enumerate() {
        match row {
            // The strip's heading is line 0; every question follows.
            Row::Strip(k) if *k >= 1 => {
                if let Some(a) = needs.get(k - 1) {
                    out.push(ThreadRow {
                        thread: talk.home_of(a.thread),
                        line: i,
                        height: 1,
                        toggle: None,
                        message: Some(a.message.id),
                    });
                }
            }
            Row::Top(_) => start = Some(i),
            Row::Head(t, _) => {
                if let Some(line) = start.take() {
                    open_box = Some(out.len());
                    out.push(ThreadRow {
                        thread: t.id,
                        line,
                        height: 0,
                        toggle: None,
                        message: None,
                    });
                }
            }
            Row::Chat(t, r) => {
                if let Some(msg) = r.toggles() {
                    // A preview's rows index its conversation, folded threads and all.
                    out.push(ThreadRow {
                        thread: t.id,
                        line: i,
                        height: 1,
                        toggle: Some(talk.conversation(t)[msg].id),
                        message: None,
                    });
                }
            }
            Row::Bottom(_) => {
                if let Some(r) = open_box.take().and_then(|b| out.get_mut(b)) {
                    r.height = i + 1 - r.line;
                }
            }
            // A resolved thread's line opens it on the full board, as a box does.
            Row::Line(t) => out.push(ThreadRow {
                thread: t.id,
                line: i,
                height: 1,
                toggle: None,
                message: None,
            }),
            _ => {}
        }
    }
    out
}

/// Where the RESOLVED row is, counted from the summary's blank first line, while the board
/// has resolved threads: a click there folds or unfolds them.
pub fn resolved_row(talk: &Talk, task: Option<TaskId>, w: usize, limits: Limits) -> Option<usize> {
    (plan(talk, task, w, limits).rows.iter()).position(|r| matches!(r, Row::Resolved(_)))
}

/// How many lines the summary takes in a pane `w` wide, its blank first line included.
pub fn height(talk: &Talk, task: Option<TaskId>, w: usize, limits: Limits) -> usize {
    plan(talk, task, w, limits).rows.len()
}

/// The URL each line `lines` draws for the same arguments shows a piece of, if any.
pub fn links(talk: &Talk, task: Option<TaskId>, w: usize, limits: Limits) -> Vec<Option<String>> {
    let Plan { rows } = plan(talk, task, w, limits);
    (rows.iter())
        .map(|r| match r {
            Row::Chat(_, r) => chat::row_link(r),
            _ => None,
        })
        .collect()
}

/// The summary's lines in a pane `w` wide, each exactly `w` columns, with ages as of `now`
/// (seconds since the Unix epoch).
pub fn lines(
    talk: &Talk,
    task: Option<TaskId>,
    w: usize,
    limits: Limits,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    let Plan { rows } = plan(talk, task, w, limits);
    let content = w.saturating_sub(6);
    let needs = needs_of(talk, task);
    let strip = threads::strip(talk, &needs, w, usize::MAX, now, depth);
    let mut convs: HashMap<i64, std::borrow::Cow<[Message]>> = HashMap::new();
    rows.iter()
        .map(|row| match row {
            Row::Blank => fit(Vec::new(), w),
            Row::Title => title(talk, task, w, depth).0,
            Row::Empty(text) => fit(vec![Span::styled(*text, dim(depth))], w),
            Row::Top(st) => edge(*st, "┌", "┐", w, depth),
            Row::Bottom(st) => edge(*st, "└", "┘", w, depth),
            Row::Strip(k) => strip[*k].clone(),
            Row::Head(t, k) => {
                // The box's own margin stands in for the header's leading space.
                let head = threads::head(talk, t, content + 1, Some(now), true, depth);
                boxed(t.status(), without_lead_space(&head[*k]), content, w, depth)
            }
            Row::OpenChat(st) => {
                let text = "open chat ▶";
                let pad = content.saturating_sub(text.width());
                let spans = vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(text, Style::new().fg(accent(depth))),
                ];
                boxed(*st, spans, content, w, depth)
            }
            Row::Chat(t, r) => {
                let msgs = convs.entry(t.id.0).or_insert_with(|| talk.conversation(t));
                let ctx = chat::Ctx {
                    msgs,
                    people: talk.people(),
                    now,
                    depth,
                    selected: None,
                };
                boxed(
                    t.status(),
                    chat::line(r, content, &ctx).spans,
                    content,
                    w,
                    depth,
                )
            }
            Row::More(n) => fit(
                vec![Span::styled(more_text(*n), Style::new().fg(accent(depth)))],
                w,
            ),
            Row::Resolved(n) => {
                let mark = if limits.shown.unfolded { "▾" } else { "▸" };
                let spans = vec![
                    Span::styled(format!(" RESOLVED {n} "), bold()),
                    Span::styled(mark, dim(depth)),
                ];
                fit(spans, w)
            }
            Row::Line(t) => full_board::title_line(talk, t, w, now, depth),
        })
        .collect()
}

/// " … 2 more open threads, open the full board ▶".
fn more_text(n: usize) -> String {
    let threads = if n == 1 { "thread" } else { "threads" };
    format!(" … {n} more open {threads}, open the full board ▶")
}

/// Where "… N more open threads, open the full board ▶" is, counted from the summary's
/// blank first line, while open threads are left out: it opens the full board, as `b` does.
pub fn more_control(
    talk: &Talk,
    task: Option<TaskId>,
    w: usize,
    limits: Limits,
) -> Option<ControlAt> {
    let rows = plan(talk, task, w, limits).rows;
    let (line, n) = (rows.iter().enumerate()).find_map(|(i, r)| match r {
        Row::More(n) => Some((i, *n)),
        _ => None,
    })?;
    Some(ControlAt {
        control: Control::FullBoard,
        line,
        from: 1,
        to: more_text(n).width().min(w),
    })
}

/// " MESSAGE BOARD · whole repo Catalogue Lead": the board's scope (`Talk::scope`, or
/// "#13" with a task selected) and its lead in the accent colour, as UI v0.1.7 heads it.
/// With a lead, "open full board ▶" at the right, which `b` does too; returns where it is.
fn title(
    talk: &Talk,
    task: Option<TaskId>,
    w: usize,
    depth: Depth,
) -> (Line<'static>, Option<(usize, usize)>) {
    let scope = match task {
        Some(t) => t.to_string(),
        None => talk.scope.clone(),
    };
    let mut spans = vec![
        Span::styled(" MESSAGE BOARD", bold().patch(dim(depth))),
        Span::styled(format!(" · {scope}"), dim(depth)),
    ];
    let Some(l) = talk.lead else {
        return (fit(spans, w), None);
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(l.name.clone(), Style::new().fg(accent(depth))));
    let control = Style::new().fg(accent(depth));
    with_control(
        spans,
        " MESSAGE BOARD".len(),
        Control::FullBoard,
        control,
        w,
    )
}

/// Where the summary's "open full board ▶" is, counted from its blank first line, in a pane
/// `w` wide: on its title, while the milestone has a lead and there is room.
pub fn board_control(talk: &Talk, task: Option<TaskId>, w: usize) -> Option<ControlAt> {
    // Colours never move the control, so any depth will do.
    let (_, at) = title(talk, task, w, Depth::Ansi256);
    at.map(|(from, to)| ControlAt {
        control: Control::FullBoard,
        line: 1,
        from,
        to,
    })
}

/// A box border: solid for an open thread, dashed for a resolved one.
fn edge(status: ThreadStatus, l: &str, r: &str, w: usize, depth: Depth) -> Line<'static> {
    let (h, _) = strokes(status);
    let s = format!("{l}{}{r}", h.repeat(w.saturating_sub(4)));
    fit(vec![Span::raw(" "), Span::styled(s, dim(depth))], w)
}

fn strokes(status: ThreadStatus) -> (&'static str, &'static str) {
    match status {
        ThreadStatus::Open => ("─", "│"),
        ThreadStatus::Resolved => ("╌", "╎"),
    }
}

/// A row of contents between the box's borders, cut or padded to `content` columns.
fn boxed(
    status: ThreadStatus,
    spans: Vec<Span<'static>>,
    content: usize,
    w: usize,
    depth: Depth,
) -> Line<'static> {
    let (_, v) = strokes(status);
    let mut row = vec![Span::raw(" "), Span::styled(v, dim(depth)), Span::raw(" ")];
    row.extend(fit(spans, content).spans);
    row.push(Span::raw(" "));
    row.push(Span::styled(v, dim(depth)));
    fit(row, w)
}

/// `text` with the golden UI's highlights: when `start` (the text starts a message or a
/// doing line), a leading `blocked:` in bold amber, or `decision:` in bold cyan when the
/// message is for the owner's awareness (`Tone::Awareness`; a delivery failure never starts
/// with `decision:`); and every mention of the owner in bold red, as `board::mentioned`
/// reads one: in any case, and never one a post quotes.
pub(crate) fn highlight(
    text: &str,
    start: bool,
    awareness: bool,
    depth: Depth,
) -> Vec<Span<'static>> {
    highlight_marked(text, &owner_mentions(text), start, awareness, depth)
}

/// `highlight` with the owner's mentions already found: `marks`, byte ranges of `text`,
/// in order. A piece of a wrapped message is marked from the whole message, where a quote
/// or code fence that began on an earlier line is known.
pub(crate) fn highlight_marked(
    text: &str,
    marks: &[Range<usize>],
    start: bool,
    awareness: bool,
    depth: Depth,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut at = 0;
    if start {
        let decision = awareness.then_some(("decision:", info(depth)));
        for (prefix, colour) in [("blocked:", warn(depth))].into_iter().chain(decision) {
            if text.starts_with(prefix) {
                spans.push(Span::styled(prefix, bold().fg(colour)));
                at = prefix.len();
                break;
            }
        }
    }
    for r in marks {
        if r.start < at || r.end > text.len() {
            continue;
        }
        if r.start > at {
            spans.push(Span::raw(text[at..r.start].to_string()));
        }
        spans.push(Span::styled(
            text[r.clone()].to_string(),
            bold().fg(bad(depth)),
        ));
        at = r.end;
    }
    if at < text.len() || spans.is_empty() {
        spans.push(Span::raw(text[at..].to_string()));
    }
    spans
}

/// The status pill's colour, as in the golden UI: open in its warn amber, resolved green.
pub(crate) fn status_style(status: ThreadStatus, depth: Depth) -> Style {
    match status {
        ThreadStatus::Open => Style::new().fg(warn(depth)),
        ThreadStatus::Resolved => Style::new().fg(ok(depth)),
    }
}

/// An author's colour by role (see `Role::of`), as UI v0.1.7 colours names: the lead in the
/// accent (no colour per lead), devs teal (#8fd6c4), seniors lilac (#c3a6ff), qa pink (#f2a7c3), anyone else the
/// default. Tower's own `system` messages (the stuck-worker watch) have a slate grey
/// (#9aa4b2) of their own, unlike any role's or lead's colour.
pub fn role_colour(author: &str, lead: Option<&Lead>, depth: Depth) -> Color {
    let rgb = |r, g, b, index| match depth {
        Depth::TrueColor => Color::Rgb(r, g, b),
        Depth::Ansi256 => Color::Indexed(index),
    };
    match Role::of(author, lead) {
        Role::System => rgb(0x9a, 0xa4, 0xb2, 248),
        Role::Lead => accent(depth),
        Role::Dev => rgb(0x8f, 0xd6, 0xc4, 116),
        Role::Senior => rgb(0xc3, 0xa6, 0xff, 147),
        Role::Qa => rgb(0xf2, 0xa7, 0xc3, 218),
        Role::Other => Color::Reset,
    }
}

/// `text` in pieces at most `w` columns wide, a piece per line of it (a blank line stays
/// blank), each wrapped as the copy's delivery-failure details are (`Wrap::Plain`).
pub(crate) fn wrap(text: &str, w: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line_by(line, w, Wrap::Plain))
        .collect()
}

/// `text` wrapped to `w` columns on at most `max` lines: past that, the last line carries
/// the rest, for `fit` to cut with "…". The rest is the text as written from where the lines
/// before it end, so a word broken across lines stays whole.
pub(crate) fn wrap_at_most(text: &str, w: usize, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut pieces = wrap_line(text, w);
    if pieces.len() > max {
        pieces.truncate(max - 1);
        // The lines kept are the text's first words, what wrapping drops aside (`dropped`):
        // skip as many characters.
        let mut left: usize = (pieces.iter())
            .map(|p| p.chars().filter(|c| !dropped(*c)).count())
            .sum();
        let at = (text.char_indices())
            .find(|(_, c)| {
                let past = left == 0;
                if !dropped(*c) {
                    left = left.saturating_sub(1);
                }
                past
            })
            .map_or(text.len(), |(i, _)| i);
        pieces.push(text[at..].trim_start().to_string());
    }
    pieces
}

/// `text` on lines at most `w` wide, its spaces folded (`fold_spaces`), as the copy's
/// `.clamp2` text (titles, goals, thread names, inbox rows) breaks it (`Wrap::Plain`).
pub(crate) fn wrap_line(text: &str, w: usize) -> Vec<String> {
    wrap_line_by(&fold_spaces(text), w, Wrap::Plain)
}

/// `text` on lines at most `w` wide, its spaces folded (`fold_spaces`), as the copy's
/// events and acks break it (`Wrap::Event`).
pub(crate) fn wrap_event_line(text: &str, w: usize) -> Vec<String> {
    wrap_line_by(&fold_spaces(text), w, Wrap::Event)
}

/// `text` with its spaces folded as the copy's normal and `nowrap` text folds them: its
/// leading and trailing ones dropped and each run between words one.
pub(crate) fn fold_spaces(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').filter(|s| !s.is_empty()).collect();
    words.join(" ")
}

/// Whether wrapping may leave `c` out where a line breaks: a space, or an ideographic space
/// (U+3000), which hangs past the line's end (`hang`). The wrapped lines hold every other
/// character of the text, in order.
pub(crate) fn dropped(c: char) -> bool {
    c == ' ' || c == IDEOGRAPHIC_SPACE
}

const IDEOGRAPHIC_SPACE: char = '\u{3000}';

/// `piece` as the part that must fit on a line and the run of ideographic spaces ending it,
/// which the copy hangs past the line's end, as `pre-wrap` hangs a run of spaces, so the
/// text before it is not broken for it. Mid-line, the run keeps its width. A piece that is
/// only such a run, which starts a word, hangs nothing: the copy's folded text carries it to
/// the next line with what follows.
fn hang(piece: &str) -> (&str, &str) {
    match piece.trim_end_matches(IDEOGRAPHIC_SPACE) {
        "" => (piece, ""),
        fits => (fits, &piece[fits.len()..]),
    }
}

/// `hung`, the run of ideographic spaces after the text just put on `line`, onto it when it
/// fits; otherwise it hangs past the line's end, left out as a run of spaces is at a break,
/// and the line ends there, so the next line starts with what follows it.
fn put_hung(line: &mut String, out: &mut Vec<String>, hung: &str, w: usize) {
    if hung.is_empty() {
        return;
    }
    if line.width() + hung.width() <= w {
        line.push_str(hung);
    } else {
        out.push(std::mem::take(line));
    }
}

/// `text` on lines at most `w` wide as the copy's message body breaks it (`Wrap::Body`).
pub(crate) fn wrap_line_paths(text: &str, w: usize) -> Vec<String> {
    wrap_line_by(text, w, Wrap::Body)
}

/// How `wrap_line_by` breaks a line, after the copy's element for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrap {
    /// Text the copy breaks with `overflow-wrap: anywhere` and marks no paths in, so a path
    /// is a word like any other: a delivery failure's opened details (`.evtdetail`) and,
    /// their spaces folded (`fold_spaces`), titles, goals, thread names and inbox rows
    /// (`.clamp2`). The details' `white-space: pre-wrap` keeps a line's leading spaces on
    /// its first line and each run between words as written; `overflow-wrap: anywhere`
    /// breaks at the space before a word with no break of its own, such as a long hash, too
    /// wide for a line, so it starts on a line of its own; a word breaks inside where Chrome
    /// does (`break_units`), such as after a `-` or between two ideographs.
    Plain,
    /// Events, a delivery failure's line and acks (`.evt`, `.ack`), their spaces folded: as
    /// `Plain`, but the copy leaves `overflow-wrap` normal there, so a piece of a word too
    /// wide for any line (`own_breaks`) overflows its line in Chrome, and what follows it
    /// starts the next. Here it breaks where its lines end, and its last line ends with it.
    Event,
    /// A message body (`.seg .body`): as `Plain`, and a path the copy marks (`paths`),
    /// alone or joined to other text, breaks only after each `/`, as the copy's `<wbr>`
    /// breaks one; the text joined to it breaks as any other.
    Body,
}

/// `word` in the pieces `how` may break it between, short of breaking it anywhere, or
/// `None` when it has no such break: where Chrome does (`break_units`), a path in a `Body`
/// only after each `/`.
fn own_breaks(word: &str, how: Wrap) -> Option<Vec<&str>> {
    let pieces = match how {
        Wrap::Plain | Wrap::Event => break_units(word, &[]),
        Wrap::Body => break_units(word, &paths(word)),
    };
    (pieces.len() > 1).then_some(pieces)
}

/// `word` in the pieces Chrome breaks it between, as the copy draws it (`breaks_between`):
/// inside one of `paths`, byte ranges of `word`, only after each `/`, where the copy puts a
/// `<wbr>`, and the last `/` of a path breaks from what follows it too.
fn break_units<'a>(word: &'a str, paths: &[Range<usize>]) -> Vec<&'a str> {
    static SEGMENTER: LineSegmenterBorrowed<'static> = {
        let mut options = LineBreakOptions::default();
        options.strictness = Some(LineBreakStrictness::Normal);
        LineSegmenter::new_for_non_complex_scripts(options)
    };
    let found: Vec<usize> = if word.is_ascii() {
        Vec::new()
    } else {
        SEGMENTER.segment_str(word).collect()
    };
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        let (at, next) = chars[i];
        let before = i.checked_sub(2).map(|j| chars[j].1);
        let after = chars.get(i + 1).map(|&(_, c)| c);
        let c = chars[i - 1].1;
        let after_path = paths.iter().any(|r| r.start < at && at <= r.end);
        let in_path = paths.iter().any(|r| r.start < at && at < r.end);
        let icu = found.binary_search(&at).is_ok();
        let breaks = if after_path && c == '/' {
            true
        } else if in_path {
            false
        } else {
            breaks_between(before, c, next, after, icu)
        };
        if breaks {
            out.push(&word[start..at]);
            start = at;
        }
    }
    out.push(&word[start..]);
    out
}

/// Whether Chrome breaks a line between `c` and `next` inside a word, as the copy draws it,
/// `before` and `after` being the characters around them, and `icu` whether `icu_segmenter`
/// breaks there. Chrome (Blink) decides a break between two ASCII characters from a table
/// of its own and hands any other to ICU, the Unicode line-breaking rules (UAX #14), here
/// `icu_segmenter` at CSS `line-break: auto`'s strictness (`Normal`, so a small kana or a
/// `ー` may start a line). So:
/// - between two ASCII characters, as Blink's table does, a break only after a `-` or a
///   `?`: after a `-` unless before a character a line cannot start with
///   (`. , ; : ! ? ) ] } / $`) or before a digit after a minus sign (a `-` with no ASCII
///   letter or digit before it), each `-` of a run breaking too; after a `?` unless before
///   one of `! " ' ) , . / : ; ? ] }`, so a run of `?` keeps together and breaks after its
///   last;
/// - between any other two, a break where `icu_segmenter` finds one: between two
///   ideographs, kana or hangul, between one and a letter or digit, but not before a
///   closing mark such as `。` `、` `」` nor after an opening one such as `「`. Two rules
///   Chrome's ICU has and `icu_segmenter` (2.4) does not apply are added: no break after a
///   hyphen that starts the word (nothing or a no-break space before it) before a letter
///   (UAX #14's LB20a), and a break before an opening `“` or `‘` between two East Asian
///   characters, and after a closing `”` or `’` between two, as in `中“文”中` (LB19a).
fn breaks_between(
    before: Option<char>,
    c: char,
    next: char,
    after: Option<char>,
    icu: bool,
) -> bool {
    if c.is_ascii() && next.is_ascii() {
        let minus = next.is_ascii_digit() && !before.is_some_and(|c| c.is_ascii_alphanumeric());
        return match c {
            '-' => !minus && !".,;:!?)]}/$".contains(next),
            '?' => !"!\"'),./:;?]}".contains(next),
            _ => false,
        };
    }
    let class = |c: char| CodePointMapData::<LineBreak>::new().get(c);
    let category = |c: char| CodePointMapData::<GeneralCategory>::new().get(c);
    let east_asian = |c: Option<char>| {
        c.is_some_and(|c| {
            matches!(
                CodePointMapData::<EastAsianWidth>::new().get(c),
                EastAsianWidth::F | EastAsianWidth::W | EastAsianWidth::H
            )
        })
    };
    // LB20a: a letter is what UAX #14 takes as AL (AI and XX resolve to it) or HL.
    if matches!(class(c), LineBreak::HY | LineBreak::HH)
        && before.is_none_or(|c| c == '\u{a0}')
        && matches!(
            class(next),
            LineBreak::AL | LineBreak::HL | LineBreak::AI | LineBreak::XX
        )
    {
        return false;
    }
    // LB19a, short of the rules before it that keep the two together.
    if class(next) == LineBreak::QU && category(next) == GeneralCategory::InitialPunctuation {
        return east_asian(Some(c))
            && east_asian(after)
            && !matches!(
                class(c),
                LineBreak::OP | LineBreak::GL | LineBreak::WJ | LineBreak::ZWJ
            );
    }
    if class(c) == LineBreak::QU && category(c) == GeneralCategory::FinalPunctuation {
        let kept = [
            LineBreak::CM,
            LineBreak::ZWJ,
            LineBreak::WJ,
            LineBreak::GL,
            LineBreak::CL,
            LineBreak::CP,
            LineBreak::EX,
            LineBreak::SY,
            LineBreak::IS,
            LineBreak::BA,
            LineBreak::HY,
            LineBreak::NS,
            LineBreak::IN,
        ];
        return east_asian(before) && east_asian(Some(next)) && !kept.contains(&class(next));
    }
    icu
}

/// The byte ranges of the paths in `word` as the copy marks them in a message: the matches
/// of its `PATH_RE`, `(?:~|\b[\w.-]+)?\/[\w.@+-]+(?:\/[\w.@+-]+)+\/?` (`\w` and `\b` are
/// ASCII), found from the left, each search going on after the last match; a match shorter
/// than 16 characters is not marked. So a path joined to other text, as CJK text often is,
/// is found inside it. A URL is laid out before this (`chat::wrap_message_line`).
fn paths(word: &str) -> Vec<Range<usize>> {
    let b = word.as_bytes();
    let w = |i: usize| {
        b.get(i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    };
    // The end of the run from `i` of `\w` and the bytes of `more`.
    let run = |i: usize, more: &[u8]| {
        (i..b.len())
            .find(|&j| !(w(j) || more.contains(&b[j])))
            .unwrap_or(b.len())
    };
    // The end of `\/[\w.@+-]+(?:\/[\w.@+-]+)+\/?` matched from `i`. A run stops only at
    // what it cannot take, so taking each whole is how the pattern matches.
    let rest = |i: usize| {
        let (mut at, mut runs) = (i, 0);
        while b.get(at) == Some(&b'/') {
            let end = run(at + 1, b".@+-");
            if end == at + 1 {
                break;
            }
            (at, runs) = (end, runs + 1);
        }
        let slash = usize::from(b.get(at) == Some(&b'/'));
        (runs >= 2).then_some(at + slash)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // `\b[\w.-]+`: a run from a word's edge, else none.
        let edge = w(i) != (i > 0 && w(i - 1));
        let prefix = if edge { run(i, b".-") } else { i };
        let found = (b[i] == b'~')
            .then(|| rest(i + 1))
            .flatten()
            .or_else(|| (prefix > i).then(|| rest(prefix)).flatten())
            .or_else(|| rest(i));
        match found {
            Some(end) => {
                if end - i >= 16 {
                    out.push(i..end);
                }
                i = end;
            }
            // No match starts inside a run the prefix took: from any byte of it, the run ends
            // where it does here, and no `~` or `/` is in it. So the search goes on after it,
            // and each byte is looked at a bounded number of times.
            None => i = prefix.max(i + 1),
        }
    }
    out
}

/// `word` in the pieces it may break between: after each of `after`'s characters.
pub(crate) fn units<'a>(word: &'a str, after: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in word.char_indices() {
        if after.contains(&c) {
            let end = i + c.len_utf8();
            out.push(&word[start..end]);
            start = end;
        }
    }
    if start < word.len() {
        out.push(&word[start..]);
    }
    out
}

/// Words of `text` on lines at most `w` wide. A word too wide for any line starts on a line
/// of its own and breaks where a line ends, unless it has breaks of its own (`own_breaks`):
/// such a word starts where the line is when its first piece fits there, and breaks only
/// between its pieces, or inside a piece too wide for a line of its own, after which, in
/// `Wrap::Event` text, its line ends.
/// The text's leading spaces, when narrower than a line, start its first line, as the
/// copy's `pre-wrap` body keeps them: a first word that does not fit after them goes to the
/// next line as any other word does, leaving them alone on the first. Each run of spaces
/// between words stays as written, and counts toward the line's width; a line that breaks
/// at a run drops it, as `pre-wrap` hangs it, so the next line starts with the word. A run of
/// ideographic spaces ending a word or a piece of one hangs the same way (`hang`).
fn wrap_line_by(text: &str, w: usize, how: Wrap) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    let body = text.trim_start_matches(' ');
    let indent = text.len() - body.len();
    let mut line = if body.is_empty() || indent >= w {
        String::new()
    } else {
        " ".repeat(indent)
    };
    // The run of spaces before the next word: the one after the last word, and one more for
    // each empty piece `split` leaves after it.
    let mut run = 0;
    for word in body.split(' ') {
        if word.is_empty() {
            run += 1;
            continue;
        }
        // No space between the indent and the first word, or at the start of a line.
        let gap = if line.bytes().any(|b| b != b' ') {
            run
        } else {
            0
        };
        run = 1;
        let (fits, hung) = hang(word);
        if line.width() + gap + fits.width() <= w {
            line.push_str(&" ".repeat(gap));
            line.push_str(fits);
            put_hung(&mut line, &mut out, hung, w);
            continue;
        }
        // A word with breaks of its own starts where the line is, rather than leaving it
        // short, when its first piece fits there (so an indent the piece does not fit after
        // keeps its line, as the copy's does); any other goes on the next, the space before
        // it hanging at the end of this one.
        let own = own_breaks(word, how);
        let here = own.is_some();
        let pieces = own.unwrap_or_else(|| vec![word]);
        if here && !line.is_empty() && line.width() + gap + hang(pieces[0]).0.width() <= w {
            line.push_str(&" ".repeat(gap));
        } else if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        for unit in pieces {
            let (unit, hung) = hang(unit);
            if !line.is_empty() && line.width() + unit.width() > w {
                out.push(std::mem::take(&mut line));
            }
            let wide = unit.width() > w;
            // Whether a run of ideographic spaces the line broke at hangs past its end.
            let mut hanging = false;
            for c in unit.chars() {
                if hanging && c == IDEOGRAPHIC_SPACE {
                    continue;
                }
                hanging = false;
                if !line.is_empty() && line.width() + c.width().unwrap_or(0) > w {
                    out.push(std::mem::take(&mut line));
                    if c == IDEOGRAPHIC_SPACE {
                        hanging = true;
                        continue;
                    }
                }
                line.push(c);
            }
            if wide && how == Wrap::Event {
                // Its last line ends with it, any run after it hanging there. A piece that
                // is only a run hung past the line before, which leaves no line to end.
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                continue;
            }
            put_hung(&mut line, &mut out, hung, w);
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Colour, Message, Milestone, MilestoneId, Resolution, ThreadId};
    use crate::ui::theme::lead_colour;
    use ratatui::style::Modifier;

    const NOW: i64 = 1_800_000_000;
    static NONE: BTreeSet<i64> = BTreeSet::new();
    const FOLDED: Shown = Shown {
        opened: &NONE,
        unfolded: false,
    };
    const SPACE: Limits = Limits {
        threads: None,
        open: 3,
        shown: FOLDED,
    };
    const MIN: i64 = 60;

    /// A thread whose messages are (author, text, minutes ago), in the order posted.
    fn thread(
        id: i64,
        status: ThreadStatus,
        task: Option<i64>,
        msgs: &[(&str, &str, i64)],
    ) -> Thread {
        let messages: Vec<Message> = (msgs.iter().enumerate())
            .map(|(i, (author, text, ago))| Message {
                id: id * 100 + i as i64,
                author: (*author).into(),
                text: (*text).into(),
                at: NOW - ago * MIN,
            })
            .collect();
        let resolved = (status == ThreadStatus::Resolved).then(|| Resolution {
            by: messages
                .last()
                .map(|m| m.author.clone())
                .unwrap_or_default(),
            at: messages.last().map_or(NOW, |m| m.at),
        });
        Thread::whole(
            ThreadId(id),
            format!("thread {id}"),
            task.map(TaskId),
            resolved,
            messages,
        )
    }

    /// The whole board a milestone carries, unnarrowed, as the summary takes it.
    fn talk_of(m: &Milestone) -> Talk<'_> {
        Talk {
            lead: m.lead.as_ref(),
            threads: m.threads.iter().collect(),
            scope: format!("{} and its tasks", m.id),
            others: Vec::new(),
            tasks: Vec::new(),
            needs: Vec::new(),
            folded: Vec::new(),
            homes: Vec::new(),
        }
    }

    fn milestone(threads: Vec<Thread>) -> Milestone {
        Milestone {
            id: MilestoneId(1),
            title: "First shop pages".into(),
            goal: "g".into(),
            repo: None,
            lead: Some(Lead {
                name: "Catalogue Lead".into(),
                session: "cat-lead".into(),
                colour: Some(Colour::Blue),
            }),
            tasks: Vec::new(),
            marked_done: false,
            done_at: None,
            folded: false,
            threads,
            details: Default::default(),
        }
    }

    fn golden() -> Milestone {
        use ThreadStatus::*;
        milestone(vec![
            thread(
                2,
                Resolved,
                None,
                &[("senior", "resolved: the ribbon sits under the cover", 120)],
            ),
            thread(
                3,
                Open,
                Some(13),
                &[
                    ("cat-lead", "Picked up #13. Opening this thread.", 58),
                    ("dev-2", "copy, reading the spec", 55),
                    ("senior", "copy, I'll review", 54),
                    ("qa", "copy, preparing the shelf at three sizes", 52),
                    ("dev-2", "I have done the shelf grid.", 12),
                    ("senior", "copy, reviewing the layout", 10),
                    ("qa", "copy, trying the shelf on a phone", 8),
                ],
            ),
            thread(
                4,
                Open,
                Some(14),
                &[
                    ("dev-1", "Starting the reading reminders.", 45),
                    ("lead", "copy, called the printer again.", 15),
                ],
            ),
            thread(
                5,
                Resolved,
                Some(13),
                &[("qa", "cases pass", 30), ("qa", "4 of 4 pass", 5)],
            ),
        ])
    }

    fn text(m: &Milestone, task: Option<TaskId>, w: usize) -> Vec<String> {
        lines(&talk_of(m), task, w, SPACE, NOW, Depth::TrueColor)
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn open_threads_come_first_newest_activity_first_then_resolved() {
        let m = golden();
        let ids: Vec<i64> = m.ordered_threads(None).iter().map(|t| t.id.0).collect();
        // T3's latest is 8m ago, T4's 15m; resolved T5 (5m) and T2 (2h) after them.
        assert_eq!(ids, vec![3, 4, 5, 2]);
    }

    #[test]
    fn threads_with_the_same_latest_message_time_go_newer_id_first() {
        use ThreadStatus::{Open, Resolved};
        let m = milestone(vec![
            thread(6, Open, Some(13), &[("dev-2", "a", 5)]),
            thread(7, Open, Some(13), &[("dev-2", "b", 5)]),
            thread(8, Resolved, Some(13), &[("qa", "c", 5)]),
            thread(9, Resolved, Some(13), &[("qa", "d", 5)]),
        ]);
        let ids: Vec<i64> = (m.ordered_threads(Some(TaskId(13))).iter())
            .map(|t| t.id.0)
            .collect();
        assert_eq!(ids, vec![7, 6, 9, 8]);
    }

    #[test]
    fn a_task_narrows_the_board_to_its_linked_threads() {
        let m = golden();
        let ids = |task| -> Vec<i64> {
            m.ordered_threads(Some(TaskId(task)))
                .iter()
                .map(|t| t.id.0)
                .collect()
        };
        assert_eq!(ids(13), vec![3, 5]);
        assert_eq!(ids(14), vec![4]);
        assert!(ids(12).is_empty());
        let rows = text(&m, Some(TaskId(12)), 80);
        assert_eq!(
            rows[1],
            format!(
                "{:<58}{}",
                " MESSAGE BOARD · #12 Catalogue Lead", "open full board (b) ▶ "
            )
        );
        // Its strip, then why there is no thread.
        assert_eq!(rows[2].trim_end(), " nothing on this board needs you");
        assert_eq!(rows[3].trim_end(), " no thread for this task yet");
        assert_eq!(rows.len(), 4);
        // #13's: T3 in its box, resolved T5 in the folded RESOLVED row.
        let rows = text(&m, Some(TaskId(13)), 80).join("\n");
        assert!(
            rows.contains("T3") && rows.contains(" RESOLVED 1 ▸"),
            "{rows}"
        );
        assert!(!rows.contains("T4") && !rows.contains("T2"), "{rows}");
    }

    #[test]
    fn in_its_space_an_open_thread_previews_its_last_three_bubbles_and_resolved_ones_fold() {
        let rows = text(&golden(), None, 80);
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} missing:\n{}", rows.join("\n")))
        };
        // T3: the lead's opening with three acks under it, then dev-2's with two: every
        // message shows, in two bubbles, so nothing is counted as earlier.
        // Each header: status, name, ids and size at the right; what it is on and who
        // started it; where it stands. `head` is its last line.
        let name = at("[open] thread 3");
        assert!(
            rows[name].contains("  T3 · 7 messages ·   8m"),
            "{}",
            rows[name]
        );
        assert!(
            rows[name].starts_with(" │ [open] thread 3"),
            "{}",
            rows[name]
        );
        assert!(
            rows[name + 1].contains("on #13 · started by cat-lead"),
            "{}",
            rows[name + 1]
        );
        assert!(
            rows[name + 2].contains("where it stands dev-2: I have done the shelf grid."),
            "{}",
            rows[name + 2]
        );
        let head = name + 2;
        assert!(!rows.iter().any(|r| r.contains("earlier")));
        // Each message's age is on its name line, not in its text.
        assert!(
            rows[head + 2].contains("│   cat-lead · lead · 58m "),
            "{}",
            rows[head + 2]
        );
        assert!(rows[head + 3].contains("│  ▌ Picked up #13. Opening this thread."));
        assert!(!rows[head + 3].contains("58m"), "{}", rows[head + 3]);
        assert!(rows[head + 4].contains("│    ↳ dev-2: copy, reading the spec · 55m"));
        assert!(rows[head + 6].contains("↳ qa: copy, preparing the shelf at three sizes · 52m"));
        assert!(rows[head + 8].contains("│   dev-2 · developer"));
        assert!(rows[head + 9].contains("│  ▌ I have done the shelf grid."));
        assert!(rows[head + 11].contains("↳ qa: copy, trying the shelf on a phone · 8m"));
        assert!(rows[head + 12].trim_end().ends_with("open chat ▶ │"));
        assert!(rows[head + 13].starts_with(" └─"));
        // T4: the lead's ack answers dev-1's bubble.
        let head = at("[open] thread 4") + 2;
        assert!(
            rows[head - 2].contains("  T4 · 2 messages ·  15m"),
            "{}",
            rows[head - 2]
        );
        assert!(rows[head + 3].contains("▌ Starting the reading reminders."));
        assert!(rows[head + 4].contains("↳ lead: copy, called the printer again. · 15m"));
        // T5 and T2 resolved: no box of their own, one folded row under the open ones
        //; every open thread shows, so nothing more is linked.
        let row = at(" RESOLVED");
        assert_eq!(rows[row].trim_end(), " RESOLVED 2 ▸");
        assert!(rows[row - 1].starts_with(" └─"), "{}", rows[row - 1]);
        assert_eq!(row, rows.len() - 1);
        assert!(
            !rows
                .iter()
                .any(|r| r.contains('╌') || r.contains("thread 5"))
        );
        assert!(!rows.iter().any(|r| r.contains("more open")));
        assert_eq!(more_control(&talk_of(&golden()), None, 80, SPACE), None);
    }

    #[test]
    fn the_popup_previews_three_open_threads_with_their_last_two_bubbles_and_links_the_rest() {
        let popup = Limits::of(Mode::Popup, FOLDED);
        let m = golden();
        let rows: Vec<String> = (lines(&talk_of(&m), None, 80, popup, NOW, Depth::TrueColor)
            .iter())
        .map(|l| l.to_string())
        .collect();
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        // T3 has two bubbles, both shown, so nothing is counted as earlier.
        let head = at("[open] thread 3").unwrap() + 2;
        assert!(rows[head + 2].contains("cat-lead · lead"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("earlier")), "{rows:#?}");
        // Resolved threads fold into the RESOLVED row in the popup too, and do not count
        // against its three: both open threads show, and nothing more is linked.
        assert_eq!(rows.last().unwrap().trim_end(), " RESOLVED 2 ▸");
        assert!(!rows.iter().any(|r| r.contains("more open")), "{rows:#?}");
        assert_eq!(more_control(&talk_of(&m), None, 80, popup), None);
        assert_eq!(rows.len(), height(&talk_of(&m), None, 80, popup));
        // Five open threads: two are left out, and the line linking to the full board
        // comes before the RESOLVED row.
        let mut five = milestone(
            (1..=5)
                .map(|i| thread(i, ThreadStatus::Open, None, &[("dev-1", "hi", i)]))
                .collect(),
        );
        (five.threads).push(thread(6, ThreadStatus::Resolved, None, &[("qa", "ok", 9)]));
        let rows: Vec<String> = (lines(&talk_of(&five), None, 80, popup, NOW, Depth::Ansi256)
            .iter())
        .map(|l| l.to_string())
        .collect();
        let n = rows.len();
        assert_eq!(
            rows[n - 2].trim_end(),
            " … 2 more open threads, open the full board ▶"
        );
        assert_eq!(rows[n - 1].trim_end(), " RESOLVED 1 ▸");
        let c = more_control(&talk_of(&five), None, 80, popup).unwrap();
        assert_eq!((c.control, c.line), (Control::FullBoard, n - 2));
        // Its own space has no such line.
        assert_eq!(more_control(&talk_of(&five), None, 80, SPACE), None);
        assert_eq!(thread_rows(&talk_of(&five), None, 80, popup).len(), 3);
        assert_eq!(thread_rows(&talk_of(&five), None, 80, SPACE).len(), 5);
    }

    /// Unfolded, the RESOLVED row has one line per resolved thread, newest first: its
    /// ✓, its name and its age, each opening that thread (on the full board) when clicked.
    #[test]
    fn unfolded_resolved_threads_are_a_line_each_that_opens_the_thread() {
        let m = golden();
        let talk = talk_of(&m);
        let unfolded = Limits {
            shown: Shown {
                opened: &NONE,
                unfolded: true,
            },
            ..SPACE
        };
        let all = lines(&talk, None, 80, unfolded, NOW, Depth::TrueColor);
        let rows: Vec<String> = all.iter().map(|l| l.to_string()).collect();
        let row = rows
            .iter()
            .position(|r| r.starts_with(" RESOLVED"))
            .unwrap();
        assert_eq!(rows[row].trim_end(), " RESOLVED 2 ▾");
        assert!(
            rows[row + 1].starts_with(" ✓ thread 5"),
            "{}",
            rows[row + 1]
        );
        assert!(
            rows[row + 1].trim_end().ends_with(" 5m"),
            "{}",
            rows[row + 1]
        );
        assert!(
            rows[row + 2].starts_with(" ✓ thread 2"),
            "{}",
            rows[row + 2]
        );
        assert!(
            rows[row + 2].trim_end().ends_with(" 2h"),
            "{}",
            rows[row + 2]
        );
        assert_eq!(rows.len(), row + 3);
        assert!(rows.iter().all(|r| r.width() == 80));
        assert_eq!(all[row + 1].spans[1].style.fg, Some(ok(Depth::TrueColor)));
        assert_eq!(height(&talk, None, 80, unfolded), rows.len());
        // The row stays where it is, folded or not; a click there folds or unfolds it.
        assert_eq!(resolved_row(&talk, None, 80, unfolded), Some(row));
        assert_eq!(resolved_row(&talk, None, 80, SPACE), Some(row));
        let lines_at = |limits| -> Vec<(i64, usize, usize)> {
            (thread_rows(&talk, None, 80, limits).iter())
                .filter(|r| r.line > row)
                .map(|r| (r.thread.0, r.line, r.height))
                .collect()
        };
        assert_eq!(lines_at(unfolded), vec![(5, row + 1, 1), (2, row + 2, 1)]);
        assert!(lines_at(SPACE).is_empty());
        // No resolved thread, no row; only resolved threads, only the row.
        use ThreadStatus::{Open, Resolved};
        let open = milestone(vec![thread(1, Open, None, &[("dev-1", "hi", 1)])]);
        assert_eq!(resolved_row(&talk_of(&open), None, 80, SPACE), None);
        let done = milestone(vec![thread(1, Resolved, None, &[("dev-1", "hi", 1)])]);
        let rows = text(&done, None, 80);
        assert_eq!(rows.last().unwrap().trim_end(), " RESOLVED 1 ▸");
        assert!(
            !rows.iter().any(|r| r.contains("no threads yet")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_loaded_summary_draws_the_same_as_the_whole_threads() {
        // The board loads only the shown messages and the total; the summary is the same.
        let mut whole = golden();
        let long: Vec<(String, String, i64)> = (0..30)
            .map(|i| {
                let who = ["dev-2", "qa", "senior", "cat-lead"][i % 4];
                (
                    who.to_string(),
                    format!("message {i}, with some words"),
                    90 - i as i64,
                )
            })
            .collect();
        let long: Vec<(&str, &str, i64)> = (long.iter())
            .map(|(a, t, m)| (a.as_str(), t.as_str(), *m))
            .collect();
        whole
            .threads
            .push(thread(6, ThreadStatus::Open, None, &long));
        let mut loaded = whole.clone();
        for t in &mut loaded.threads {
            let keep = t.status().shown();
            t.messages.drain(..t.messages.len().saturating_sub(keep));
        }
        assert_eq!(loaded.threads[4].messages.len(), 12);
        assert_eq!(text(&loaded, None, 80), text(&whole, None, 80));
        let popup = Limits::of(Mode::Popup, FOLDED);
        assert_eq!(
            lines(&talk_of(&loaded), None, 80, popup, NOW, Depth::Ansi256),
            lines(&talk_of(&whole), None, 80, popup, NOW, Depth::Ansi256)
        );
    }

    #[test]
    fn each_author_shows_in_their_role_colour_and_the_pill_in_its_status_colour() {
        // The summary draws only open threads' bubbles: senior's (T2) and qa's (T5)
        // are open here; in T3 they only ack.
        let mut m = golden();
        m.threads[0] = thread(
            2,
            ThreadStatus::Open,
            None,
            &[("senior", "resolved: the ribbon sits under the cover", 120)],
        );
        m.threads[3] = thread(
            5,
            ThreadStatus::Open,
            Some(13),
            &[("qa", "cases pass", 30), ("qa", "4 of 4 pass", 5)],
        );
        let all = lines(&talk_of(&m), None, 80, SPACE, NOW, Depth::TrueColor);
        let colour_of = |name: &str| {
            all.iter()
                .flat_map(|l| &l.spans)
                .find(|s| s.content == name)
                .and_then(|s| s.style.fg)
                .unwrap_or_else(|| panic!("no span {name:?}"))
        };
        assert_eq!(colour_of("dev-2"), Color::Rgb(0x8f, 0xd6, 0xc4));
        assert_eq!(colour_of("senior"), Color::Rgb(0xc3, 0xa6, 0xff));
        assert_eq!(colour_of("qa"), Color::Rgb(0xf2, 0xa7, 0xc3));
        assert_eq!(colour_of("[open]"), warn(Depth::TrueColor));
        let lead_msg = milestone(vec![thread(
            1,
            ThreadStatus::Open,
            None,
            &[("cat-lead", "go", 1)],
        )]);
        let all = lines(&talk_of(&lead_msg), None, 80, SPACE, NOW, Depth::TrueColor);
        let lead = all
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "cat-lead")
            .unwrap();
        assert_eq!(
            lead.style.fg,
            Some(lead_colour(Colour::Blue, Depth::TrueColor))
        );
        for other in ["manager", "devon", "qatar", "device-x"] {
            assert_eq!(
                role_colour(other, lead_msg.lead.as_ref(), Depth::Ansi256),
                Color::Reset,
                "{other}"
            );
        }
        assert_eq!(
            role_colour("senior-1", None, Depth::Ansi256),
            Color::Indexed(147)
        );
    }

    #[test]
    fn tower_messages_are_quiet_centred_lines_with_no_bubble_and_keep_a_needs_you_tag() {
        let m = milestone(vec![thread(
            1,
            ThreadStatus::Open,
            Some(13),
            &[
                ("cat-lead", "go", 9),
                ("system", "nudged dev-1: no post for 20 min", 3),
                ("system", "nudged dev-1: no post for 20 min", 2),
                (
                    "system",
                    "question: @manager dev-1 is waiting on a prompt",
                    1,
                ),
            ],
        )]);
        let talk = talk_of(&m);
        let t = talk.threads[0];
        let none = BTreeSet::new();
        let shape = chat::Shape {
            clip: 4,
            open: &none,
            now: Some(NOW),
            preview: None,
            people: talk.people(),
            full: false,
        };
        let rows = chat::plan(&t.messages, 0, 60, shape);
        let ctx = chat::Ctx {
            msgs: &t.messages,
            people: talk.people(),
            now: NOW,
            depth: Depth::TrueColor,
            selected: None,
        };
        let drawn: Vec<Line> = chat::lines(&rows, 60, &ctx);
        let text: Vec<String> = drawn.iter().map(|l| l.to_string()).collect();
        // Two nudges of one worker are one line; neither has a name or a bubble.
        let nudged = text
            .iter()
            .position(|r| r.contains("dev-1 was nudged 2 times"))
            .unwrap();
        let line = &text[nudged];
        let left = line.len() - line.trim_start().len();
        let right = line.len() - line.trim_end().len();
        assert!(left.abs_diff(right) <= 1, "{line:?}");
        assert!(!text.iter().any(|r| r.contains("system")), "{text:#?}");
        // Tower's question to the owner keeps its red tag on its quiet line.
        let q = text
            .iter()
            .position(|r| r.contains("needs you @manager dev-1 is waiting"))
            .unwrap();
        let tag = drawn[q]
            .spans
            .iter()
            .find(|s| s.content == "needs you")
            .unwrap();
        assert_eq!(tag.style.fg, Some(bad(Depth::TrueColor)));
        let rest = drawn[q]
            .spans
            .iter()
            .find(|s| s.content.contains("waiting"))
            .unwrap();
        assert_eq!(rest.style.fg, dim(Depth::TrueColor).fg);
    }

    #[test]
    fn needs_you_decision_and_blocked_messages_are_tagged_and_only_a_question_is_red() {
        use ThreadStatus::Open;
        let d = Depth::TrueColor;
        for (text, tag, colour) in [
            (
                "blocked: the printer has not sent the covers",
                Some("blocked"),
                warn(d),
            ),
            (
                "question: @manager can you ring the printer this week?",
                Some("needs you"),
                bad(d),
            ),
            (
                "decision: the shelf shows 12 books, not 10. @manager for your awareness",
                Some("decision"),
                info(d),
            ),
            ("copy, still blocked: no", None, warn(d)),
            ("decision: x @Manager approve?", Some("decision"), info(d)),
        ] {
            let one = milestone(vec![thread(1, Open, None, &[("dev-1", text, 1)])]);
            for w in [80, 30] {
                let all = lines(&talk_of(&one), None, w, SPACE, NOW, d);
                let tags: Vec<&Span> = (all.iter().flat_map(|l| &l.spans))
                    .filter(|s| ["needs you", "decision", "blocked"].contains(&&*s.content))
                    .collect();
                match tag {
                    Some(tag) => {
                        assert_eq!(tags.len(), 1, "{text} w={w}");
                        assert_eq!(tags[0].content, tag, "{text}");
                        assert_eq!(tags[0].style.fg, Some(colour), "{text}");
                        assert!(tags[0].style.add_modifier.contains(Modifier::BOLD));
                    }
                    None => assert!(tags.is_empty(), "{text} w={w}"),
                }
                // Only a question to the owner has the red edge and tint.
                let edges: Vec<&Span> = (all.iter().flat_map(|l| &l.spans))
                    .filter(|s| s.content == "▌")
                    .collect();
                assert!(!edges.is_empty(), "{text} w={w}");
                let question = tag == Some("needs you");
                for e in edges {
                    assert_eq!(e.style.fg == Some(bad(d)), question, "{text}");
                    let tint = Some(Color::Rgb(0x2a, 0x19, 0x1d));
                    assert_eq!(e.style.bg == tint, question, "{text}");
                }
                for l in &all {
                    assert_eq!(l.width(), w);
                }
            }
        }
    }

    #[test]
    fn the_owner_is_highlighted_as_board_mentioned_reads_a_mention() {
        // Any case is a mention; a quoted or code one is not.
        let d = Depth::TrueColor;
        let red = |text: &str| -> Vec<String> {
            let one = milestone(vec![thread(
                1,
                ThreadStatus::Open,
                None,
                &[("dev-1", text, 1)],
            )]);
            // In the bubble only: the header's where-it-stands line repeats the text.
            (lines(&talk_of(&one), None, 80, SPACE, NOW, d).iter())
                .filter(|l| l.to_string().contains('▌'))
                .flat_map(|l| l.spans.clone())
                .filter(|s| s.style.fg == Some(bad(d)) && s.content.starts_with('@'))
                .map(|s| s.content.to_string())
                .collect()
        };
        assert_eq!(
            red("ping @manager and @Manager, @MANAGER"),
            ["@manager", "@Manager", "@MANAGER"]
        );
        assert!(red("> @manager said so").is_empty());
        assert!(red("see `@manager` in the docs").is_empty());
        assert!(red("```\n@manager\n```").is_empty());
        assert!(red("mail me at x@manager.example").is_empty());
        // Right after kana, with no space: marked, as the signed copy's hl marks it.
        assert_eq!(
            red("notes.mdを見てから@managerに返事します。"),
            ["@manager"]
        );
        // Before `.` or `(` and a CJK letter: marked; before `.` or `(` and ASCII: code.
        assert_eq!(
            red("请@manager.谢谢 请@manager(明天)看 @manager.x() @manager(1)"),
            ["@manager", "@manager"]
        );
        // A mention past the clip is marked from the whole text, on its own line.
        assert_eq!(red("a\n> b\n@manager"), ["@manager"]);
        // Across a wrapped line, the mention stays whole and the quote's continuation stays
        // unmarked.
        let long = format!("> {} @manager", "word ".repeat(30));
        assert!(red(&long).is_empty());
    }

    #[test]
    fn says_why_the_board_is_empty() {
        let mut m = milestone(Vec::new());
        let rows = text(&m, None, 40);
        assert_eq!(rows[2].trim_end(), " nothing on this board needs you");
        assert_eq!(rows[3].trim_end(), " no threads yet");
        m.lead = None;
        let rows = text(&m, None, 40);
        // The heading still names the part of the board it would be.
        assert_eq!(rows[1].trim_end(), " MESSAGE BOARD · M1 and its tasks");
        assert_eq!(rows[2].trim_end(), " no lead yet, so no message board");
    }

    #[test]
    fn a_path_stays_in_its_line_and_breaks_only_after_a_slash() {
        let text =
            "Icons live in assets/icons/bookmarks/outline/bookmark-ribbon.svg, one file an icon.";
        assert_eq!(
            wrap_line_paths(text, 40),
            [
                "Icons live in assets/icons/bookmarks/",
                "outline/bookmark-ribbon.svg, one file an",
                "icon."
            ]
        );
        // A title marks no paths, so there one too wide for a line starts on a line of its
        // own and breaks where the line ends; a message breaks it after a `/`.
        assert_eq!(
            wrap_line(text, 30),
            [
                "Icons live in",
                "assets/icons/bookmarks/outline",
                "/bookmark-ribbon.svg, one file",
                "an icon."
            ]
        );
        assert_eq!(wrap_line_paths(text, 30)[0], "Icons live in assets/icons/");
        // Under 16 characters it is a word like any other.
        assert_eq!(
            wrap_line_paths("see a/b/c.rs and more", 10),
            ["see", "a/b/c.rs", "and more"]
        );
        // Any other word too wide for a line still breaks where the line ends.
        assert_eq!(wrap_line_paths("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn a_bubble_takes_at_most_85_percent_and_a_long_message_folds_to_two_lines() {
        let long = "Plan: the shelf grid, then the ribbon, then the phone layout. \
                    Tests for each, then docs, then the release notes and a demo for \
                    the owner in a throwaway herdr, then the tag.";
        let m = milestone(vec![thread(
            1,
            ThreadStatus::Open,
            None,
            &[("dev-2", long, 30)],
        )]);
        // In a 200-column board its chat is 194 wide: 85% would be 164, but a bubble holds
        // at most 100 columns of text, so it is 103 with its edge and spaces.
        let rows = text(&m, None, 200);
        let edge = rows.iter().filter(|r| r.contains('▌'));
        for r in edge {
            let from = r.find('▌').unwrap();
            let bubble = r[from..].trim_end_matches([' ', '│']);
            assert!(bubble.chars().count() <= 103, "{r:?}");
        }
        // At 60 its chat is 54 wide, and a bubble 85% of it, 45 columns: 42 of text.
        let rows = text(&m, None, 60);
        // The age is on the name line, so the first line is as wide as the rest.
        assert_eq!(
            rows[8],
            " │   dev-2 · developer · 30m                              │ "
        );
        assert_eq!(
            rows[9],
            " │  ▌ Plan: the shelf grid, then the ribbon,              │ "
        );
        assert_eq!(
            rows[10],
            " │  ▌ then the phone layout. Tests for each,              │ "
        );
        assert_eq!(
            rows[11],
            " │  ▌ read more                                           │ "
        );
        // One line past the clip shows in full: no "read more" to hide a single line.
        let three = "one two three four five six seven eight nine ten eleven twelve thirteen";
        let m = milestone(vec![thread(
            1,
            ThreadStatus::Open,
            None,
            &[("dev-2", three, 30)],
        )]);
        let rows = text(&m, None, 40);
        assert!(!rows.iter().any(|r| r.contains("read more")), "{rows:#?}");
    }

    #[test]
    fn a_message_with_line_breaks_keeps_them_in_its_bubble() {
        let plan = "Plan:\n1. shelf grid";
        let m = milestone(vec![thread(
            1,
            ThreadStatus::Open,
            None,
            &[("qa-2", plan, 3)],
        )]);
        let rows = text(&m, None, 60);
        // Under the strip and the thread's three-line header.
        assert!(rows[9].contains("▌ Plan:"), "{}", rows[9]);
        assert!(rows[10].contains("▌ 1. shelf grid"), "{}", rows[10]);
        let drawn = lines(&talk_of(&m), None, 60, SPACE, NOW, Depth::TrueColor);
        let author = (drawn[8].spans.iter())
            .find(|s| s.content == "qa-2")
            .unwrap();
        assert_eq!(author.style.fg, Some(Color::Rgb(0xf2, 0xa7, 0xc3)));
    }

    #[test]
    fn every_line_is_exactly_the_pane_width_and_the_height_matches() {
        let mut long = golden();
        long.threads[0].title = "日本語のスレッド名と長い題名".repeat(4);
        long.threads[1].messages[0].author = "a-very-long-author-name".into();
        long.threads[1].messages[0].text =
            format!("{} 日本語\n\n{}", "x".repeat(150), "word ".repeat(700));
        for m in [golden(), long, milestone(Vec::new())] {
            for task in [None, Some(TaskId(13)), Some(TaskId(99))] {
                for w in 0..=130 {
                    let drawn = lines(&talk_of(&m), task, w, SPACE, NOW, Depth::Ansi256);
                    assert_eq!(drawn.len(), height(&talk_of(&m), task, w, SPACE), "w={w}");
                    for line in drawn {
                        assert_eq!(line.width(), w, "w={w} {line:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_height_does_not_change_as_ages_grow() {
        let m = golden();
        for later in [0, 59, 3600 * 30, 86_400 * 500] {
            assert_eq!(
                lines(&talk_of(&m), None, 60, SPACE, NOW + later, Depth::Ansi256).len(),
                height(&talk_of(&m), None, 60, SPACE)
            );
        }
    }

    #[test]
    fn a_failures_details_wrap_as_the_copys_details_do() {
        // As a message body does: a word with no break of its own too wide for a line starts
        // on a line of its own, and a word breaks after a `-`.
        assert_eq!(wrap("ab abcdefghij", 8), ["ab", "abcdefgh", "ij"]);
        assert_eq!(wrap("ab abc-def", 8), ["ab abc-", "def"]);
        // But a path is a word like any other, as the details mark none, where a message
        // breaks it after each `/`.
        let path = "ab assets/icons/bookmarks/outline";
        assert_eq!(
            wrap(path, 12),
            ["ab", "assets/icons", "/bookmarks/o", "utline"]
        );
        assert_eq!(
            wrap_line_paths(path, 12),
            ["ab assets/", "icons/", "bookmarks/", "outline"]
        );
        assert_eq!(
            wrap("see /opt/tower/var/log/control-tower.log now", 20),
            ["see", "/opt/tower/var/log/c", "ontrol-tower.log now"]
        );
    }

    #[test]
    fn folded_text_breaks_as_the_copys_clamp_and_event_box_do() {
        // As headless Chrome draws the copy's `.clamp2` (titles, goals, thread names and inbox
        // rows, with `overflow-wrap: anywhere`): a word breaks after a `-`, and a word too
        // wide for a line starts on a line of its own and breaks where the line ends.
        let title = "Bookmarks page for the reading-list and the shelf-grid layout";
        assert_eq!(
            wrap_line(title, 27),
            [
                "Bookmarks page for the",
                "reading-list and the shelf-",
                "grid layout"
            ]
        );
        assert_eq!(
            wrap_line("ab abcdefghij cd", 8),
            ["ab", "abcdefgh", "ij cd"]
        );
        assert_eq!(
            wrap_line("ab abcdefghij-k cd", 8),
            ["ab", "abcdefgh", "ij-k cd"]
        );
        // The copy's `.evt` and `.ack` leave `overflow-wrap` normal, so Chrome lets a piece too
        // wide for a line overflow it and starts what follows on the next line: here the
        // piece breaks where its lines end, and its last line ends with it.
        assert_eq!(
            wrap_event_line("ab abcdefghij cd", 8),
            ["ab", "abcdefgh", "ij", "cd"]
        );
        assert_eq!(
            wrap_event_line("ab abcdefghij-k cd", 8),
            ["ab", "abcdefgh", "ij-", "k cd"]
        );
        assert_eq!(wrap_event_line(title, 27), wrap_line(title, 27));
    }

    #[test]
    fn folded_text_breaks_after_a_question_mark_where_the_copy_does() {
        // As headless Chrome draws the copy's `.evt` and `.clamp2`: the `?` rule a message's
        // word breaks by (`breaks_between`) holds for events, acks and titles too.
        assert_eq!(
            wrap_event_line("sent what?why to qa", 10),
            ["sent what?", "why to qa"]
        );
        assert_eq!(
            wrap_line("sent what?why to qa", 10),
            ["sent what?", "why to qa"]
        );
    }

    #[test]
    fn an_ideographic_space_at_a_line_end_hangs_as_the_copys_does() {
        // As headless Chrome draws the copy's message body, delivery-failure details,
        // `.clamp2` and `.evt`: a run of U+3000 ending a line hangs past it, so the word before
        // it is not broken for it and the run does not start the next line.
        let all = |text: &str, w: usize| {
            let lines = wrap_line_paths(text, w);
            assert_eq!(wrap(text, w), lines, "details at {w}");
            assert_eq!(wrap_line(text, w), lines, "folded at {w}");
            assert_eq!(wrap_event_line(text, w), lines, "event at {w}");
            lines
        };
        // The issue's text, in a full board's bubble (40) and a narrow one (25).
        let text = "全角　スペース　を　含む　文章　です　全角　スペース　を　含む　文章　です　全角　スペース";
        assert_eq!(
            all(text, 40),
            [
                "全角　スペース　を　含む　文章　です　全",
                "角　スペース　を　含む　文章　です　全角",
                "スペース"
            ]
        );
        assert_eq!(
            all(text, 25),
            [
                "全角　スペース　を　含む",
                "文章　です　全角　スペー",
                "ス　を　含む　文章　です",
                "全角　スペース"
            ]
        );
        assert_eq!(all("中　中中", 3), ["中", "中", "中"]);
        // Where it fits it keeps its width, at a line's end and mid-line.
        assert_eq!(all("中　中中", 4), ["中　", "中中"]);
        assert_eq!(all("中　中中", 6), ["中　中", "中"]);
        // A run hangs whole, inside a piece broken where the line ends too.
        let text = "abcd　　、ef";
        for w in [3, 4] {
            let lines = wrap_line_paths(text, w);
            assert_eq!(wrap(text, w), lines, "details at {w}");
            assert_eq!(wrap_line(text, w), lines, "folded at {w}");
        }
        assert_eq!(wrap_line_paths(text, 3), ["abc", "d　", "、", "ef"]);
        assert_eq!(wrap_line_paths(text, 4), ["abcd", "、ef"]);
        // In an event, a piece too wide for any line ends its last line, and a run after it
        // hangs there, so the next line starts with the word after it; a run that is the
        // whole piece leaves no empty line behind.
        assert_eq!(wrap_event_line("abcdef　x", 4), ["abcd", "ef", "x"]);
        assert_eq!(wrap_event_line("　　　　中", 3), ["　", "中"]);
        // The rest past the last line starts after a run hung on the lines before it.
        assert_eq!(
            wrap_at_most("確認です　確認です　確認です　確認です", 8, 3),
            ["確認です", "確認です", "確認です　確認です"]
        );
    }

    #[test]
    fn wrap_breaks_at_spaces_and_splits_only_words_too_wide() {
        assert_eq!(wrap("copy, go", 20), vec!["copy, go"]);
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("日本語です", 4), vec!["日本", "語で", "す"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(
            wrap("Plan:\n1. layout and colours\n\n2. tests", 12),
            vec!["Plan:", "1. layout", "and colours", "", "2. tests"]
        );
        for w in 1..12 {
            for piece in wrap("Plan: the shelf grid, then the ribbon 日本", w) {
                assert!(piece.width() <= w.max(2), "w={w} {piece:?}");
            }
        }
    }

    #[test]
    fn a_delivery_failures_details_break_cjk_text_where_the_copy_does() {
        // Between two ideographs, as Chrome breaks the copy's `.evtdetail`, so a run too wide
        // for any line fills the line it starts on; not before a closing mark.
        assert_eq!(wrap("ab  中文字", 6), ["ab  中", "文字"]);
        let run = "中".repeat(30);
        assert_eq!(
            wrap(&format!("pane 中 {run}"), 20),
            [
                format!("pane 中 {}", "中".repeat(6)),
                "中".repeat(10),
                "中".repeat(10),
                "中".repeat(4)
            ]
        );
        assert_eq!(wrap("中文。字字", 4), ["中", "文。", "字字"]);
    }

    #[test]
    fn a_delivery_failures_details_break_after_a_question_mark_where_the_copy_does() {
        // As Chrome breaks the copy's `.evtdetail`: after a `?`, after the last of a run, and
        // not before a character a line cannot start with.
        assert_eq!(wrap("ab abc?defg", 8), ["ab abc?", "defg"]);
        assert_eq!(wrap("ab abc??defg", 8), ["ab abc??", "defg"]);
        assert_eq!(wrap("ab abc?!defg", 8), ["ab", "abc?!def", "g"]);
    }

    #[test]
    fn paths_are_what_the_copys_path_pattern_finds_in_a_word() {
        // Each as the copy's PATH_RE finds it with `matchAll`, keeping matches of 16 or more.
        let found = |word: &str| -> Vec<String> {
            (paths(word).into_iter())
                .map(|r| word[r].to_string())
                .collect()
        };
        // A `~` starts one, after text too; a letter before the `~` is not part of it.
        let home = "~/projects/bakery/rye/notes.md";
        assert_eq!(found(home), [home]);
        assert_eq!(found(&format!("x{home}")), [home]);
        assert_eq!(
            found("x~y/var/log/tower/output.log"),
            ["y/var/log/tower/output.log"]
        );
        // A trailing `/` is the path's, and CJK text either side is not.
        assert_eq!(
            found("設定は~/.config/tower/plugins/。確認"),
            ["~/.config/tower/plugins/"]
        );
        assert_eq!(
            found("ログは/var/log/tower/output.logに書いて"),
            ["/var/log/tower/output.log"]
        );
        // A match under 16 characters is skipped and the search goes on after it.
        assert_eq!(found("src/ui/chat.rs"), Vec::<String>::new());
        assert_eq!(
            found("a/b/c:/var/log/tower/output.log"),
            ["/var/log/tower/output.log"]
        );
        assert_eq!(
            found("ab/cd/ef//var/log/tower/output.log"),
            ["/var/log/tower/output.log"]
        );
        // Two in one word.
        assert_eq!(
            found("assets/icons/bookmarks,docs/notes/plans/q4.md"),
            ["assets/icons/bookmarks", "docs/notes/plans/q4.md"]
        );
        // After a scheme other than http, the path starts at the host's `/`.
        assert_eq!(
            found("ftp://host.example/a/b/c/d"),
            ["/host.example/a/b/c/d"]
        );
        // The prefix takes letters, digits, `_`, `.` and `-` from a word's edge; not `@`,
        // and not `.-` with no word character before it.
        assert_eq!(found("a.b-c/dddd/eeee/ffff"), ["a.b-c/dddd/eeee/ffff"]);
        assert_eq!(
            found("@dev/assets/icons/bookmarks"),
            ["dev/assets/icons/bookmarks"]
        );
        assert_eq!(found(".-/var/log/tower/x.log"), ["/var/log/tower/x.log"]);
    }

    #[test]
    fn a_2000_character_word_is_searched_for_paths_as_any_other() {
        let a = "a".repeat(2000);
        let dots = ".".repeat(2000);
        // No path in a run of word characters, of dots, or the two together.
        for word in [a.clone(), dots.clone(), format!("{dots}{a}")] {
            assert!(paths(&word).is_empty(), "{}", word.len());
        }
        // A path after such a run is found where the copy's pattern starts it: (start, end).
        let found = |word: &str| -> Vec<(usize, usize)> {
            (paths(word).into_iter())
                .map(|r| (r.start, r.end))
                .collect()
        };
        let word = format!("{}@{}/cc/dd/ee", "a".repeat(1000), "b".repeat(980));
        assert_eq!(found(&word), [(1001, word.len())]);
        let word = format!("{dots}/var/log/tower/output.log");
        assert_eq!(found(&word), [(2000, word.len())]);
        let word = format!("{}a/bb/ccc/dddd/eeeee", ".-".repeat(1000));
        assert_eq!(found(&word), [(2000, word.len())]);
        // A bubble draws such a word on lines of its own, broken where each line ends.
        let word = format!("{dots}{a}");
        let mut want = vec!["ab".to_string()];
        want.extend((0..100).map(|k| word[k * 40..(k + 1) * 40].to_string()));
        assert_eq!(wrap_line_paths(&format!("ab {word}"), 40), want);
    }

    #[test]
    fn wrap_at_most_carries_the_rest_as_written_on_its_last_line() {
        assert_eq!(
            wrap_at_most("one two three", 7, 2),
            vec!["one two", "three"]
        );
        assert_eq!(
            wrap_at_most("one two three four", 7, 2),
            vec!["one two", "three four"]
        );
        // A word too wide for a line starts on a line of its own, as the copy's `.clamp2`
        // starts it, and the rest keeps its spacing as written.
        assert_eq!(
            wrap_at_most("ab bookshelf  now", 4, 2),
            vec!["ab", "bookshelf  now"]
        );
        // A word broken across the lines past the last stays whole.
        assert_eq!(wrap_at_most("abcdefghij", 4, 2), vec!["abcd", "efghij"]);
        assert_eq!(wrap_at_most("abcdefghij", 4, 1), vec!["abcdefghij"]);
        assert_eq!(wrap_at_most("日本語です", 4, 2), vec!["日本", "語です"]);
    }
}
