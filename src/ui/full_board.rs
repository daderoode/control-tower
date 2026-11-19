//! The lead's full message board, as in the golden UI's layout E: a "◀ back" line naming the
//! board, a channel list of the lead's threads grouped OPEN and RESOLVED, and the selected
//! thread's header (status pill, id, title, who started it, its linked task, how many
//! messages) over its whole conversation as a group chat (`chat`), which opens at the
//! latest message and scrolls back.
//!
//! An open thread takes two lines: its status mark, name and age, then where it stands
//! (what it is on is in its header, UI v0.1.13). A blank line parts two open threads
//! and comes before the RESOLVED row, a blank row is under the "Needs you" strip, and one
//! between the thread's header and its conversation. RESOLVED is one row, "RESOLVED 3 ▸", folded by default each
//! time the board opens; unfolded ("▾") each resolved thread is
//! one line, ✓ name then age. The row is a stop for up and down; right or a click on it
//! unfolds the group, left or a click folds it, and left on a resolved thread folds it
//! with the row selected. A resolved thread shown off the row (opened from elsewhere, or
//! the first on a board with no open thread) unfolds the group, so the thread shown is
//! always in the list.
//!
//! Keys, as UI v0.1.7 has them: up and down move between threads; right moves into the
//! chat and selects its newest message, then up and down move between messages (the
//! selected one outlined), right opens or folds it (read more, a delivery failure's
//! details; Enter too on a resolved thread, as Enter posts on an open one) and left returns
//! to the thread list. A click on "read more", "show less" or "details" opens or folds that
//! message.
//!
//! Wide, the list sits left of the conversation; in a pane narrower than `LIST_MIN + 1 +
//! CONV_MIN` it sits above it, at most a third of the height, so both stay in sight without a
//! toggle. The list and the conversation are each planned once, unstyled (`list_plan`,
//! `conv_plan`), and both the layout and the drawing read the plan, so they cannot disagree;
//! a plan never depends on the colour depth, and depends on the time only through the
//! conversation's separators, reckoned at the dashboard's clock to the minute
//! (`Dash::minute`). The conversation's plan, the wrapping of every message, is kept for
//! the last conversation, width, open messages and minute asked for, since every frame and
//! input asks for it again.
//!
//! Scrolled back, the conversation is anchored on the message being read, so new messages
//! do not move it; at the latest (no anchor) it follows new messages.
//!
//! Under the conversation is the composer: a rule, then "manager", the input and "Post",
//! where the owner writes to an open thread as `manager`. On an open thread the keys always
//! write (UI v0.1.13), so the composer always shows it: the label in the accent, an
//! accent bar down the input's left edge and the terminal's caret at the cursor. The input
//! grows from one line to `INPUT_MAX` as the draft wraps, the conversation giving up the
//! rows. A resolved thread shows "resolved threads are read-only" there instead.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use std::collections::BTreeSet;

use unicode_width::UnicodeWidthStr;

use super::chat::{self, Row};
use super::message_board;
use super::theme::{Depth, accent, bold, dim, fit, selected_bg};
use super::threads;
use crate::board::{Talk, Thread, ThreadId, ThreadStatus, age};

/// The list takes this share of a wide pane...
const LIST_PERCENT: usize = 34;
/// ...but never fewer columns than this.
const LIST_MIN: usize = 28;
/// The list sits beside the conversation only when the conversation gets this many columns.
const CONV_MIN: usize = 40;
/// A list item's age column: "now" up to "999d".
const AGE_COLUMNS: usize = 4;
/// The columns a thread's name keeps in a list line before its own title is cut.
const NAME_KEEPS: usize = 12;
/// How many lines of a long message the chat shows before "read more".
const CLIP: usize = 4;
/// Columns of the "◀ back" a click on the top line goes back from.
pub const BACK_COLUMNS: u16 = 8;
/// The most lines the composer's input grows to as the draft wraps; a longer draft shows
/// the lines around the cursor.
const INPUT_MAX: usize = 4;
/// The composer's label, before the input; left out when the input would get fewer than
/// `INPUT_MIN` columns with it.
const LABEL: &str = " manager ";
/// The composer's button, after the input and a space.
const POST: &str = " Post ";
const INPUT_MIN: usize = 12;
/// What an empty input says.
const PLACEHOLDER: &str = "Type to write to the team, e.g. copy, approved";
/// The input's left edge: in the accent, it shows the keys go to the draft.
const BAR: &str = "▌";
/// What the composer of a resolved thread says.
const READ_ONLY: &str = " resolved threads are read-only";

/// Where the conversation is scrolled back to: `offset` lines from the first line of the
/// message with id `message` (negative above it). No anchor means at the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub message: i64,
    pub offset: isize,
}

/// A stop up and down move between in the thread list, and what a click on it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Thread(ThreadId),
    /// The "RESOLVED N" row, which folds and unfolds the resolved threads.
    Resolved,
}

/// The RESOLVED group's fold: whether it is unfolded, and whether the selection is on its
/// row (the conversation then keeps the thread shown last). Folded, off the row, by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolvedFold {
    pub unfolded: bool,
    pub on_row: bool,
}

/// Where everything in the full board goes in the body of a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardLayout {
    /// The "◀ back  MESSAGE BOARD · lead · milestone" line.
    pub top: Rect,
    /// The "Needs you" strip under it, across the board, and the lines it may take.
    pub strip: Rect,
    pub strip_room: usize,
    /// The question each strip line opens, by its thread (the one whose conversation shows
    /// it) and message id; `None` for its heading and other lines.
    pub strip_rows: Vec<Option<(ThreadId, i64)>>,
    pub list: Rect,
    /// The stop each list line selects, `None` for the OPEN heading (and RESOLVED with no
    /// resolved thread).
    pub list_rows: Vec<Option<Stop>>,
    /// The RESOLVED group's fold as the list shows it: unfolded when the thread shown is
    /// resolved and the selection is not on the row.
    pub fold: ResolvedFold,
    /// Whether the list sits beside the conversation (else above it).
    pub beside: bool,
    /// The selected thread's header, its rule under it included.
    pub head: Rect,
    /// The conversation under the header.
    pub conv: Rect,
    /// The composer under the conversation, its rule included; empty with no thread.
    pub composer: Rect,
    /// The composer's input, its last one to `INPUT_MAX` lines, and its "Post" button, on
    /// the input's first line; empty on a resolved thread, which takes no messages.
    pub input: Rect,
    pub post: Rect,
    /// How many lines the conversation has, for scrolling it.
    pub conv_lines: usize,
    /// Each message the keys step through (a bubble's message, a delivery failure): its id
    /// and the conversation line it starts on, in order.
    pub starts: Vec<(i64, usize)>,
    /// Each conversation line a click opens or folds a message on, with that message's id.
    pub toggles: Vec<(usize, i64)>,
    /// The thread shown, `None` on a board with no threads.
    pub thread: Option<ThreadId>,
}

impl BoardLayout {
    /// The thread the keys write to: the open thread shown, while its composer's input is
    /// on screen and the selection is not on the RESOLVED row, where the keys work
    /// the fold, as the keys line says. A resolved thread, or none, takes no writing.
    pub fn writes_to(&self) -> Option<ThreadId> {
        self.thread
            .filter(|_| self.input.width > 0 && self.input.height > 0 && !self.fold.on_row)
    }

    /// The stop selected in the list: the RESOLVED row, or the thread shown.
    pub fn current(&self) -> Option<Stop> {
        if self.fold.on_row {
            Some(Stop::Resolved)
        } else {
            self.thread.map(Stop::Thread)
        }
    }

    /// Whether the selection is on a resolved thread's line, under the RESOLVED row.
    pub fn on_resolved_thread(&self) -> bool {
        let stops = self.stops();
        let current = self.current();
        (stops.iter())
            .skip_while(|s| **s != Stop::Resolved)
            .skip(1)
            .any(|s| Some(*s) == current)
    }

    /// The list's stops in order, each once.
    pub fn stops(&self) -> Vec<Stop> {
        let mut out: Vec<Stop> = Vec::new();
        for s in self.list_rows.iter().flatten() {
            if out.last() != Some(s) {
                out.push(*s);
            }
        }
        out
    }

    /// The first conversation line shown at the latest: the last page.
    pub fn max_top(&self) -> usize {
        self.conv_lines
            .saturating_sub(usize::from(self.conv.height))
    }

    /// The first conversation line shown with `anchor`: the anchored line, kept within the
    /// content; the latest page without one, or when its message is gone.
    pub fn conv_top(&self, anchor: Option<Anchor>) -> usize {
        let max = self.max_top();
        let Some(a) = anchor else { return max };
        match self.starts.iter().find(|(id, _)| *id == a.message) {
            Some((_, at)) => at.saturating_add_signed(a.offset).min(max),
            None => max,
        }
    }

    /// The anchor that shows line `line` first: on the message that line belongs to (the
    /// first message for the lines above it), or none at the latest page.
    pub fn anchor_at(&self, line: usize) -> Option<Anchor> {
        if line >= self.max_top() {
            return None;
        }
        let (message, at) = (self.starts.iter())
            .rev()
            .find(|(_, at)| *at <= line)
            .or(self.starts.first())
            .copied()?;
        Some(Anchor {
            message,
            offset: line as isize - at as isize,
        })
    }
}

/// The thread the board shows: `wanted` while it is still on the board (the thread it is
/// folded into, for one folded into another), else the first.
pub fn shown<'a>(talk: &Talk<'a>, wanted: Option<ThreadId>) -> Option<&'a Thread> {
    let all = talk.ordered(None);
    let wanted = wanted.map(|id| talk.home_of(id));
    (all.iter().find(|t| Some(t.id) == wanted))
        .or(all.first())
        .copied()
}

/// One line of the thread list, unstyled.
enum ListRow<'a> {
    /// "OPEN 2".
    Open(usize),
    /// "RESOLVED 3 ▸", unfolded or not.
    Resolved(usize, bool),
    /// A thread's mark, its name in words and its age.
    Title(&'a Thread),
    /// Line `k` (0 or 1) of an open thread's name wrapped to two lines, its own title
    /// with it, its mark and age on the first.
    Name(&'a Thread, usize),
    /// Where it stands.
    Stands(&'a Thread),
    /// A blank line between two open threads, and before the RESOLVED row.
    Gap,
}

/// The thread list, read like a chat app's list of conversations: OPEN and two lines per
/// open thread, a blank line between them, then after a blank line the RESOLVED row and,
/// `unfolded`, one line per resolved thread, each group in list order. Beside the
/// conversation (`beside`, the list's width) a long name wraps to a second line.
/// Above it (`None`, a narrow pane) an open thread is one line too, its name and age, with
/// no blank between them.
fn list_plan<'a>(talk: &Talk<'a>, unfolded: bool, beside: Option<usize>) -> Vec<ListRow<'a>> {
    let (open, resolved): (Vec<&Thread>, Vec<&Thread>) =
        (talk.ordered(None).into_iter()).partition(|t| t.status() == ThreadStatus::Open);
    let mut rows = vec![ListRow::Open(open.len())];
    for (i, t) in open.into_iter().enumerate() {
        let Some(w) = beside else {
            rows.push(ListRow::Title(t));
            continue;
        };
        if i > 0 {
            rows.push(ListRow::Gap);
        }
        // Colours never change how many lines a name takes.
        match name_rows(talk, t, w, Depth::Ansi256).len() {
            1 => rows.push(ListRow::Title(t)),
            _ => rows.extend([ListRow::Name(t, 0), ListRow::Name(t, 1)]),
        }
        rows.push(ListRow::Stands(t));
    }
    rows.push(ListRow::Gap);
    rows.push(ListRow::Resolved(resolved.len(), unfolded));
    if unfolded {
        rows.extend(resolved.into_iter().map(ListRow::Title));
    }
    rows
}

/// The stop each line of the thread list selects (`list_plan`), `None` for the OPEN heading,
/// a blank line and RESOLVED with no resolved thread.
pub fn list_stops(talk: &Talk, unfolded: bool, beside: Option<usize>) -> Vec<Option<Stop>> {
    (list_plan(talk, unfolded, beside).iter())
        .map(|r| match r {
            ListRow::Open(_) | ListRow::Resolved(0, _) | ListRow::Gap => None,
            ListRow::Resolved(..) => Some(Stop::Resolved),
            ListRow::Title(t) | ListRow::Name(t, _) | ListRow::Stands(t) => {
                Some(Stop::Thread(t.id))
            }
        })
        .collect()
}

/// The fold the list shows for `fold` with thread `t` shown: the selection is on the row
/// only while there is a resolved thread, and a resolved thread shown off the row unfolds
/// the group, so a thread shown is never hidden.
pub fn shown_fold(talk: &Talk, t: Option<&Thread>, fold: ResolvedFold) -> ResolvedFold {
    let any = talk
        .threads
        .iter()
        .any(|t| t.status() == ThreadStatus::Resolved);
    let on_row = fold.on_row && any;
    let resolved = t.is_some_and(|t| t.status() == ThreadStatus::Resolved);
    ResolvedFold {
        unfolded: fold.unfolded || (resolved && !on_row),
        on_row,
    }
}

/// The thread list's width in a board `w` wide while it sits beside the conversation;
/// `None` in a narrow pane, where it sits above it, as wide as the board.
pub fn list_width(w: usize) -> Option<usize> {
    let lw = (w * LIST_PERCENT / 100).max(LIST_MIN);
    (w >= lw + 1 + CONV_MIN).then_some(lw)
}

/// What a conversation plan was made for: the thread, its loaded messages, the width,
/// whether bubbles take all of it, which of its messages are open, the minute its separators were reckoned at and the
/// lead (whose name takes a role word).
type PlanKey = (
    ThreadId,
    usize,
    usize,
    Option<i64>,
    usize,
    bool,
    Vec<i64>,
    i64,
    String,
);

thread_local! {
    /// The last conversation plan made, on this thread.
    static PLAN: RefCell<Option<(PlanKey, Rc<Vec<Row>>)>> = const { RefCell::new(None) };
}

/// The board's leads as the plan cache keys them: each one's repo (none for the board's
/// own), session and name.
fn leads_key(talk: &Talk) -> String {
    let own = talk.lead.map(|l| (String::new(), l));
    (own.into_iter()
        .chain(talk.others.iter().map(|(r, l)| (r.clone(), *l))))
    .map(|(repo, l)| format!("{repo}\t{}\t{}", l.session, l.name))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Thread `t`'s conversation `w` wide as a chat (`Talk::conversation`: with the threads
/// folded into it), oldest first, `open` messages (by id) in full and separators as of
/// `now`; a first line says how many earlier messages are not loaded, if any. Kept for the
/// last conversation, width, open messages, minute and who posts asked for.
fn conv_plan(
    talk: &Talk,
    t: &Thread,
    w: usize,
    full: bool,
    open: &BTreeSet<i64>,
    now: i64,
) -> Rc<Vec<Row>> {
    let msgs = talk.conversation(t);
    let opened: Vec<i64> = (msgs.iter())
        .map(|m| m.id)
        .filter(|id| open.contains(id))
        .collect();
    let key = (
        t.id,
        talk.total(t),
        msgs.len(),
        msgs.last().map(|m| m.id),
        w,
        full,
        opened,
        now,
        // Who is a lead, by session and name, and another lead's repo decide role words,
        // so bubble widths.
        leads_key(talk),
    );
    if let Some(plan) = PLAN.with_borrow(|p| {
        p.as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, plan)| plan.clone())
    }) {
        return plan;
    }
    let missing = talk.missing(t);
    let shape = chat::Shape {
        clip: CLIP,
        open,
        now: Some(now),
        preview: None,
        people: talk.people(),
        full,
    };
    let plan = Rc::new(chat::plan(&msgs, missing, w, shape));
    PLAN.set(Some((key, plan.clone())));
    plan
}

/// Where everything goes for the board `talk` showing `wanted` in `body`, its RESOLVED
/// group as `fold` asks, with `draft` in the composer of the thread shown.
#[allow(clippy::too_many_arguments)]
pub fn board_layout(
    talk: &Talk,
    wanted: Option<ThreadId>,
    fold: ResolvedFold,
    open: &BTreeSet<i64>,
    replying: bool,
    draft: &str,
    now: i64,
    body: Rect,
) -> BoardLayout {
    let t = shown(talk, wanted);
    let fold = shown_fold(talk, t, fold);
    let w = usize::from(body.width);
    let top = Rect::new(body.x, body.y, body.width, body.height.min(1));
    let below = body.height - top.height;
    // The strip takes at most a third of what is under the top line, two lines at least.
    let room = usize::from(below / 3).max(2);
    // A blank row under it, so the list does not run into it, while the list keeps a
    // line of its own: in a pane too short for both, the list's line wins.
    let strip_h = threads::strip_height(talk.needs.len(), room) as u16;
    let blank = u16::from(below > strip_h + 1);
    let sh = (strip_h + blank).min(below);
    let strip = Rect::new(body.x, body.y + top.height, body.width, sh);
    let listed = threads::strip_listed(talk.needs.len(), room);
    let mut strip_rows = vec![None; usize::from(sh)];
    for (i, a) in talk.needs.iter().take(listed).enumerate() {
        if let Some(r) = strip_rows.get_mut(i + 1) {
            *r = Some((talk.home_of(a.thread), a.message.id));
        }
    }
    let rest = Rect::new(body.x, strip.y + sh, body.width, below - sh);
    let lw = list_width(w);
    let beside = lw.is_some();
    let list_rows = list_stops(talk, fold.unfolded, lw);
    let lw = lw.unwrap_or(w);
    let (list, right) = if beside {
        let lw = lw as u16;
        (
            Rect::new(rest.x, rest.y, lw, rest.height),
            Rect::new(rest.x + lw + 1, rest.y, rest.width - lw - 1, rest.height),
        )
    } else {
        // Above, its content's height up to half the pane but at least three lines, then a
        // rule: one line a thread, so every open thread and the RESOLVED row show.
        let half = (rest.height / 2).max(3);
        let lh = (list_rows.len() as u16).min(half).min(rest.height);
        let below = (rest.height - lh).saturating_sub(1);
        (
            Rect::new(rest.x, rest.y, rest.width, lh),
            Rect::new(rest.x, rest.y + rest.height - below, rest.width, below),
        )
    };
    let rw = usize::from(right.width);
    let hh = (head_lines(talk, t, rw, beside, None, Depth::Ansi256).len() as u16).min(right.height);
    let writable = t.is_some_and(|t| t.status() == ThreadStatus::Open);
    let (label, iw, rest) = composer_columns(rw);
    // An open thread's input takes a line for each its draft wraps to, up to INPUT_MAX; a
    // resolved thread's line says it is read-only.
    let rows = if writable {
        wrap(draft, text_columns(iw)).len().min(INPUT_MAX) as u16
    } else {
        1
    };
    let reply = u16::from(replying && writable);
    let ch = if t.is_some() {
        (1 + reply + rows).min(right.height - hh)
    } else {
        0
    };
    let composer = Rect::new(right.x, right.y + right.height - ch, right.width, ch);
    let (input, post) = if writable && ch >= 2 {
        // Under the rule, never over it, at least one line: the reply line keeps its
        // row only while the input still has one. Its last lines when the pane is too short
        // for them all; "Post" is on the first, so only while that one shows. With no row
        // under the rule there is no input, so no writing.
        let kept_reply = reply.min(ch - 2);
        let ih = rows.min(ch - 1 - kept_reply);
        let y = composer.y + 1 + kept_reply;
        let post = if rest > 0 && ih == rows {
            Rect::new(right.x + label + iw + 1, y, POST.len() as u16, 1)
        } else {
            Rect::default()
        };
        (Rect::new(right.x + label, y, iw, ih), post)
    } else {
        (Rect::default(), Rect::default())
    };
    let (mut conv_lines, mut starts, mut toggles) = (0, Vec::new(), Vec::new());
    if let Some(t) = t {
        let plan = conv_plan(talk, t, rw, !beside, open, now);
        let msgs = talk.conversation(t);
        conv_lines = plan.len();
        for (line, row) in plan.iter().enumerate() {
            if let Some(i) = row.starts() {
                starts.push((msgs[i].id, line));
            }
            if let Some(i) = row.toggles() {
                toggles.push((line, msgs[i].id));
            }
        }
    }
    BoardLayout {
        top,
        strip,
        strip_room: room,
        strip_rows,
        list,
        list_rows,
        fold,
        beside,
        head: Rect::new(right.x, right.y, right.width, hh),
        conv: Rect::new(right.x, right.y + hh, right.width, right.height - hh - ch),
        composer,
        input,
        post,
        conv_lines,
        starts,
        toggles,
        thread: t.map(|t| t.id),
    }
}

/// " ◀ back  MESSAGE BOARD · catalogue · Catalogue Lead · whole repo", the repo
/// `name` in bold and the lead in the accent colour.
pub fn top_line(name: &str, talk: &Talk, w: usize, depth: Depth) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" ◀ back", dim(depth)),
        Span::styled("  MESSAGE BOARD", bold()),
        Span::styled(" · ", dim(depth)),
        Span::styled(name.to_string(), bold()),
    ];
    if let Some(l) = talk.lead {
        spans.push(Span::styled(" · ", dim(depth)));
        spans.push(Span::styled(l.name.clone(), Style::new().fg(accent(depth))));
    }
    spans.push(Span::styled(" · whole repo", dim(depth)));
    fit(spans, w)
}

/// The channel list `w` wide as `b` lays it out, its selected stop on the selected
/// background, ages as of `now`.
pub fn list_lines(
    talk: &Talk,
    b: &BoardLayout,
    w: usize,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    let bg = Style::new().bg(selected_bg(depth));
    let current = b.current();
    (list_plan(talk, b.fold.unfolded, b.beside.then_some(w)).iter())
        .map(|row| {
            let (line, stop) = match row {
                ListRow::Open(n) => {
                    return fit(vec![Span::styled(format!(" OPEN {n}"), bold())], w);
                }
                ListRow::Resolved(0, _) => {
                    return fit(vec![Span::styled(" RESOLVED 0", bold())], w);
                }
                ListRow::Resolved(n, unfolded) => {
                    let mark = if *unfolded { "▾" } else { "▸" };
                    let spans = vec![
                        Span::styled(format!(" RESOLVED {n} "), bold()),
                        Span::styled(mark, dim(depth)),
                    ];
                    (fit(spans, w), Stop::Resolved)
                }
                ListRow::Title(t) => (title_line(talk, t, w, now, depth), Stop::Thread(t.id)),
                ListRow::Name(t, k) => (name_line(talk, t, *k, w, now, depth), Stop::Thread(t.id)),
                ListRow::Gap => return fit(Vec::new(), w),
                ListRow::Stands(t) => {
                    let line = threads::stands(talk, t, "   ", w, depth);
                    let line = line.unwrap_or_else(|| fit(Vec::new(), w));
                    (line, Stop::Thread(t.id))
                }
            };
            if Some(stop) != current {
                return line;
            }
            Line::from(
                (line.spans.into_iter())
                    .map(|s| Span::styled(s.content, bg.patch(s.style)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// " ● New book shelf    8m": its mark, its name in words, its age; on a task's thread that
/// would read like another, its own title after the name, dim (" · Delivery failed").
pub(crate) fn title_line(
    talk: &Talk,
    t: &Thread,
    w: usize,
    now: i64,
    depth: Depth,
) -> Line<'static> {
    let when = talk.latest(t).map(|at| age(at, now)).unwrap_or_default();
    let right = Span::styled(format!(" {when:>AGE_COLUMNS$} "), dim(depth));
    let room = if w > right.width() + 6 {
        w - right.width()
    } else {
        w
    };
    let mut left = vec![
        Span::raw(" "),
        threads::mark(talk, t, depth),
        Span::raw(" "),
    ];
    let name = talk.name(t);
    match talk.also(t) {
        None => left.push(Span::styled(name, bold())),
        Some(own) => {
            // Its own title, dim. Short of room the name is cut first: the title
            // keeps all but NAME_KEEPS columns, and takes any the name leaves.
            let own = format!(" · {own}");
            let content = room.saturating_sub(3);
            let sw = own.width().min(content.saturating_sub(NAME_KEEPS));
            let nw = name.width().min(content - sw);
            let sw = own.width().min(content - nw);
            left.extend(fit(vec![Span::styled(name, bold())], nw).spans);
            if sw > 0 {
                left.extend(fit(vec![Span::styled(own, dim(depth))], sw).spans);
            }
        }
    }
    if room < w {
        let mut s = fit(left, room).spans;
        s.push(right);
        Line::from(s)
    } else {
        fit(left, w)
    }
}

/// The room for a thread's name in a list line `w` wide: between " ● " and its age.
fn name_room(w: usize) -> usize {
    let age = AGE_COLUMNS + 2;
    if w > age + 6 { w - age } else { w }.saturating_sub(3)
}

/// Thread `t`'s name, and its own title, on at most two list lines `w` wide, styled
/// (the name bold, its own title dim), without the mark and age. It wraps at word
/// boundaries; past two lines the second is cut with "…", but never into the own title:
/// then, as in a one-line row, the name is cut first and its own title keeps all but
/// `NAME_KEEPS` columns of the second line.
fn name_rows(talk: &Talk, t: &Thread, w: usize, depth: Depth) -> Vec<Vec<Span<'static>>> {
    let room = name_room(w).max(1);
    let name = talk.name(t);
    let own = talk.also(t).map(|o| format!(" · {o}"));
    let text = format!("{name}{}", own.as_deref().unwrap_or_default());
    let pieces = message_board::wrap_at_most(&text, room, 2);
    let fits = pieces.last().is_none_or(|p| p.width() <= room);
    if let (Some(own), false) = (&own, fits) {
        let first = message_board::wrap_at_most(&name, room, 2);
        let rest = first.get(1).cloned().unwrap_or_default();
        // A name that fits its first line leaves the second to its own title.
        let own = if rest.is_empty() {
            own.trim_start().to_string()
        } else {
            own.clone()
        };
        let keeps = if rest.is_empty() { 0 } else { NAME_KEEPS };
        let sw = own.width().min(room.saturating_sub(keeps));
        let nw = rest.width().min(room - sw);
        let sw = own.width().min(room - nw);
        let mut second = Vec::new();
        if nw > 0 {
            second.extend(fit(vec![Span::styled(rest, bold())], nw).spans);
        }
        if sw > 0 {
            second.extend(fit(vec![Span::styled(own, dim(depth))], sw).spans);
        }
        return vec![vec![Span::styled(first[0].clone(), bold())], second];
    }
    // Where each piece starts in the text, so the name and its own title keep their styles
    // across the break.
    let mut at = 0;
    (pieces.iter())
        .map(|p| {
            let from = text[at..].find(p.as_str()).map_or(at, |i| at + i);
            at = from + p.len();
            let split = name.len().saturating_sub(from).min(p.len());
            let split = (0..=split)
                .rev()
                .find(|i| p.is_char_boundary(*i))
                .unwrap_or(0);
            let mut spans = Vec::new();
            if split > 0 {
                spans.push(Span::styled(p[..split].to_string(), bold()));
            }
            if split < p.len() {
                spans.push(Span::styled(p[split..].to_string(), dim(depth)));
            }
            spans
        })
        .collect()
}

/// Line `k` of thread `t`'s name wrapped to two lines `w` wide (`name_rows`): the first
/// with its mark and age, the second under the name.
fn name_line(talk: &Talk, t: &Thread, k: usize, w: usize, now: i64, depth: Depth) -> Line<'static> {
    let spans = name_rows(talk, t, w, depth).swap_remove(k);
    let room = name_room(w);
    let lead = if k == 0 {
        vec![
            Span::raw(" "),
            threads::mark(talk, t, depth),
            Span::raw(" "),
        ]
    } else {
        vec![Span::raw("   ")]
    };
    let mut line = lead;
    line.extend(fit(spans, room).spans);
    if k == 0 && w > room + 3 {
        let when = talk.latest(t).map(|at| age(at, now)).unwrap_or_default();
        line.push(Span::styled(format!(" {when:>AGE_COLUMNS$} "), dim(depth)));
    }
    fit(line, w)
}

/// The thread's header `w` wide (`threads::head`), ages as of `now` (`None` to count its
/// lines), then a rule and a blank line. With the list above it (`beside` false), without
/// its "where it stands" line, to keep rows for the conversation. On a board with no
/// threads, why.
pub fn head_lines(
    talk: &Talk,
    t: Option<&Thread>,
    w: usize,
    beside: bool,
    now: Option<i64>,
    depth: Depth,
) -> Vec<Line<'static>> {
    let rule = Line::styled("─".repeat(w), dim(depth));
    let Some(t) = t else {
        let why = if talk.lead.is_none() {
            " no lead yet, so no message board"
        } else {
            " no threads yet"
        };
        return vec![fit(vec![Span::styled(why, dim(depth))], w), rule];
    };
    // Above the list (a narrow pane), without its "where it stands" line.
    let mut out = threads::head(talk, t, w, now, beside, depth);
    out.push(rule);
    // A blank line between the header and the conversation.
    out.push(fit(Vec::new(), w));
    out
}

/// The conversation's lines `w` wide in `rows` only (cut to the plan), as `conv_plan` plans
/// them, `open` messages in full and `selected` (by id) outlined, separators and ages as of
/// `now`: a frame styles the lines it shows, not a long thread's whole history.
#[allow(clippy::too_many_arguments)]
pub fn conv_window(
    talk: &Talk,
    t: &Thread,
    w: usize,
    full: bool,
    open: &BTreeSet<i64>,
    selected: Option<i64>,
    now: i64,
    depth: Depth,
    rows: std::ops::Range<usize>,
) -> Vec<Line<'static>> {
    let msgs = talk.conversation(t);
    let ctx = chat::Ctx {
        msgs: &msgs,
        people: talk.people(),
        now,
        depth,
        selected: selected.and_then(|id| msgs.iter().position(|m| m.id == id)),
    };
    let plan = conv_plan(talk, t, w, full, open, now);
    let end = rows.end.min(plan.len());
    let start = rows.start.min(end);
    chat::lines(&plan[start..end], w, &ctx)
}

/// The URL each line `conv_window` draws for the same arguments shows a piece of, if any.
pub fn conv_links(
    talk: &Talk,
    t: &Thread,
    w: usize,
    full: bool,
    open: &BTreeSet<i64>,
    now: i64,
    rows: std::ops::Range<usize>,
) -> Vec<Option<String>> {
    let plan = conv_plan(talk, t, w, full, open, now);
    let end = rows.end.min(plan.len());
    (plan[rows.start.min(end)..end].iter())
        .map(chat::row_link)
        .collect()
}

/// The composer line's columns `w` wide: the label's, the input's and what is left after it
/// (a space, "Post" and a space). The label goes first when the input would be too narrow,
/// then the button.
fn composer_columns(w: usize) -> (u16, u16, u16) {
    let button = 1 + POST.len() + 1;
    let (label, rest) = if w >= LABEL.len() + INPUT_MIN + button {
        (LABEL.len(), button)
    } else if w >= INPUT_MIN + button {
        (0, button)
    } else {
        (0, 0)
    };
    let input = w.saturating_sub(label + rest);
    (label as u16, input as u16, (w - label - input) as u16)
}

/// The columns of text the input's lines take in an input `w` wide: all but its bar and a
/// column for the caret after a full line.
fn text_columns(w: u16) -> usize {
    usize::from(w).saturating_sub(2)
}

/// The draft's lines as the input shows them, `w` columns of text each: split at its line
/// breaks, and wrapped after the last space that fits, or within a word wider than the line,
/// whole grapheme clusters only. Each is the byte range of the draft it shows; an empty
/// draft is one empty line.
fn wrap(draft: &str, w: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut used, mut space) = (0, 0, None);
    for (i, g) in draft.grapheme_indices(true) {
        if g == "\n" {
            out.push(start..i);
            (start, used, space) = (i + 1, 0, None);
            continue;
        }
        let gw = g.width();
        // A line holds at least one cluster, so a line narrower than one still moves on.
        while used > 0 && used + gw > w {
            let at = space.take().unwrap_or(i);
            out.push(start..at);
            start = at;
            used = draft[start..i].width();
        }
        used += gw;
        if g == " " {
            space = Some(i + 1);
        }
    }
    out.push(start..draft.len());
    out
}

/// The input `w` wide and `rows` tall (one to `INPUT_MAX`) showing `draft` with its cursor
/// at byte `at`: the draft's lines it shows (`wrap`), the `rows` around the cursor's, and the
/// caret's line among them and column of text. The one window both the drawing and the caret
/// take, so the caret is always on a line the input shows.
fn input_view(draft: &str, at: usize, w: u16, rows: usize) -> (Vec<Range<usize>>, (usize, usize)) {
    let rows = rows.clamp(1, INPUT_MAX);
    let lines = wrap(draft, text_columns(w));
    // At a wrap the cursor goes to the start of the next line.
    let line = lines.iter().rposition(|r| r.start <= at).unwrap_or(0);
    let r = &lines[line];
    let col = draft[r.start..at.clamp(r.start, r.end)].width();
    let first = (line + 1)
        .saturating_sub(rows)
        .min(lines.len().saturating_sub(rows));
    let shown = lines.into_iter().skip(first).take(rows).collect();
    (shown, (line - first, col))
}

/// What the composer's input shows: the thread's draft, its cursor (a byte in it), whether
/// the keys write to it now, and how many rows the input has (0 when none fits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Writing<'a> {
    pub draft: &'a str,
    pub at: usize,
    pub keys: bool,
    pub rows: u16,
}

/// The composer `w` wide on thread `t`: its rule, then, on an open thread, "manager", the
/// input on its own background and "Post"; on a resolved one, why it takes no messages.
/// While the keys write to it (`writing.keys`), its composer shows it: the label in the
/// accent and the accent bar down the input's left edge; otherwise both are dim. The input
/// shows the draft (its cursor at byte `writing.at`) as `input_view` lays it out, or what to
/// write while it is empty. Answering a question, a line under the rule names it: "replying
/// to" and its author stay whole, the question is cut.
pub fn composer_lines(
    talk: &Talk,
    t: &Thread,
    writing: Writing,
    reply: Option<&crate::board::Message>,
    w: usize,
    depth: Depth,
) -> Vec<Line<'static>> {
    let Writing {
        draft,
        at,
        keys,
        rows,
    } = writing;
    let rule = Line::styled("─".repeat(w), dim(depth));
    let focus = if keys {
        bold().fg(accent(depth))
    } else {
        dim(depth)
    };
    if t.status() == ThreadStatus::Resolved {
        return vec![rule, fit(vec![Span::styled(READ_ONLY, dim(depth))], w)];
    }
    // The question the owner is answering, from the "Needs you" strip.
    let replying = reply.map(|m| {
        let colour = talk.people().colour(&m.author, depth);
        // Its spaces folded, as the copy's `nowrap` quote draws them.
        let first = message_board::fold_spaces(m.text.lines().next().unwrap_or_default());
        let mut head = vec![
            Span::styled(" replying to ", dim(depth)),
            Span::styled(m.author.clone(), bold().fg(colour)),
        ];
        let room = w.saturating_sub(head.iter().map(Span::width).sum());
        if room == 0 {
            return fit(head, w);
        }
        let quote = vec![Span::styled(format!(": {first}"), dim(depth))];
        head.extend(fit(quote, room).spans);
        Line::from(head)
    });
    let (label, input, rest) = composer_columns(w);
    let field = Style::new().bg(selected_bg(depth));
    // The input's rows of the draft, around the cursor; none when no row fits under the rule.
    let lines = if rows > 0 {
        input_view(draft, at, input, usize::from(rows)).0
    } else {
        Vec::new()
    };
    let mut out = vec![rule];
    out.extend(replying);
    for (i, r) in lines.into_iter().enumerate() {
        let mut spans = Vec::new();
        if label > 0 {
            spans.push(if i == 0 {
                Span::styled(LABEL, focus)
            } else {
                Span::raw(" ".repeat(LABEL.len()))
            });
        }
        let text = if draft.is_empty() {
            Span::styled(PLACEHOLDER, dim(depth))
        } else {
            Span::raw(draft[r].to_string())
        };
        let bar = if keys {
            Style::new().fg(accent(depth))
        } else {
            dim(depth)
        };
        let cells = vec![Span::styled(BAR, bar), text];
        spans.extend(
            (fit(cells, usize::from(input)).spans.into_iter())
                .map(|s| Span::styled(s.content, field.patch(s.style))),
        );
        if rest > 0 && i == 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                POST,
                bold().fg(selected_bg(depth)).bg(accent(depth)),
            ));
        }
        out.push(fit(spans, w));
    }
    out
}

/// Where the caret sits in the composer's `input` showing `draft` with its cursor at byte
/// `at`: on its line, after the text before it, in the window the input draws
/// (`input_view` for the input's height).
pub fn cursor(input: Rect, draft: &str, at: usize) -> (u16, u16) {
    let (_, (line, col)) = input_view(draft, at, input.width, usize::from(input.height));
    (input.x + 1 + col as u16, input.y + line as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Lead, Message};
    use crate::ui::theme::Depth;

    fn thread(n: usize) -> Thread {
        let messages = (0..n as i64)
            .map(|i| Message {
                id: i,
                author: "dev-1".into(),
                text: format!("message {i} with a few words to wrap"),
                at: i,
            })
            .collect();
        Thread::whole(ThreadId(1), "t".into(), None, None, messages)
    }

    fn talk_of(t: &Thread) -> Talk<'_> {
        Talk {
            lead: None,
            threads: vec![t],
            scope: "whole repo".into(),
            others: Vec::new(),
            tasks: Vec::new(),
            needs: Vec::new(),
            folded: Vec::new(),
            homes: Vec::new(),
        }
    }

    #[test]
    fn the_conversation_plan_is_kept_for_the_same_thread_width_open_messages_and_minute() {
        let none = BTreeSet::new();
        let t = thread(3);
        let talk = talk_of(&t);
        let first = conv_plan(&talk, &t, 40, false, &none, 0);
        assert!(Rc::ptr_eq(
            &first,
            &conv_plan(&talk, &t, 40, false, &none, 0)
        ));
        assert!(!Rc::ptr_eq(
            &first,
            &conv_plan(&talk, &t, 20, false, &none, 0)
        ));
        let more = thread(4);
        let grown = conv_plan(&talk_of(&more), &more, 20, false, &none, 0);
        assert_ne!(grown.len(), conv_plan(&talk, &t, 20, false, &none, 0).len());
        // Opening a message of another thread keeps the plan; one of this thread does not.
        let other = BTreeSet::from([999]);
        let kept = conv_plan(&talk, &t, 40, false, &none, 0);
        assert!(Rc::ptr_eq(
            &kept,
            &conv_plan(&talk, &t, 40, false, &other, 0)
        ));
        let opened = BTreeSet::from([1]);
        assert!(!Rc::ptr_eq(
            &kept,
            &conv_plan(&talk, &t, 40, false, &opened, 0)
        ));
        // Another name for the lead plans again: role words, so widths, may change.
        let renamed = Lead {
            name: "Kids".into(),
            session: "kids".into(),
            colour: None,
        };
        let mut led = talk_of(&t);
        led.lead = Some(&renamed);
        let kept = conv_plan(&talk, &t, 40, false, &none, 0);
        assert!(!Rc::ptr_eq(
            &kept,
            &conv_plan(&led, &t, 40, false, &none, 0)
        ));
        // A window is the same lines as the whole conversation's.
        let all = conv_window(
            &talk,
            &t,
            40,
            false,
            &none,
            None,
            0,
            Depth::Ansi256,
            0..usize::MAX,
        );
        let part = conv_window(&talk, &t, 40, false, &none, None, 0, Depth::Ansi256, 2..5);
        assert_eq!(part, all[2..5].to_vec());
        let past = conv_window(
            &talk,
            &t,
            40,
            false,
            &none,
            None,
            0,
            Depth::Ansi256,
            900..999,
        );
        assert!(past.is_empty());
        // A new minute plans again: its separators may have moved.
        let kept = conv_plan(&talk, &t, 40, false, &none, 0);
        let later = conv_plan(&talk, &t, 40, false, &none, 7200);
        assert!(!Rc::ptr_eq(&kept, &later));
        let hours = |p: &[Row]| p.iter().filter(|r| matches!(r, Row::Hour(_))).count();
        assert_eq!(hours(&kept), 1);
        assert!(later.iter().any(|r| *r == Row::Hour("2h ago".into())));
    }

    #[test]
    fn the_input_wraps_after_spaces_and_shows_four_lines_around_the_cursor() {
        let empty = wrap("", 10);
        assert_eq!((empty.len(), empty[0].clone()), (1, 0..0));
        // After the last space that fits, the space staying at the line's end: at 6
        // columns "bbb ccc" (7) does not fit, so it breaks after "bbb ".
        assert_eq!(wrap("aaa bbb ccc", 6), [0..4, 4..8, 8..11]);
        // A line that fits exactly is not broken: the caret's column is outside it
        // (`text_columns`).
        assert_eq!(wrap("aaa bbb ccc", 7), [0..4, 4..11]);
        // A word wider than the line is cut within it.
        assert_eq!(wrap("abcdefghij", 4), [0..4, 4..8, 8..10]);
        // A line break starts a line, and a blank line is one.
        assert_eq!(wrap("ab\n\ncd", 10), [0..2, 3..3, 4..6]);
        // A wide character never straddles the edge.
        assert_eq!(wrap("界界界", 5), [0..6, 6..9]);
        // Too narrow for anything: one cluster a line, never stuck.
        assert_eq!(wrap("ab", 0), [0..1, 1..2]);
        // The caret: at a wrap it starts the next line; at the end, after the text.
        let w = 6 + 2;
        assert_eq!(input_view("aaa bbb ccc", 4, w, INPUT_MAX).1, (1, 0));
        assert_eq!(input_view("aaa bbb ccc", 11, w, INPUT_MAX).1, (2, 3));
        let (shown, caret) = input_view("", 0, w, INPUT_MAX);
        assert_eq!((shown.len(), shown[0].clone(), caret), (1, 0..0, (0, 0)));
        // Six lines: four show, around the cursor's line.
        let six = "a\nb\nc\nd\ne\nf";
        let (shown, caret) = input_view(six, six.len(), w, INPUT_MAX);
        assert_eq!((shown, caret), (vec![4..5, 6..7, 8..9, 10..11], (3, 1)));
        let (shown, caret) = input_view(six, 0, w, INPUT_MAX);
        assert_eq!((shown, caret), (vec![0..1, 2..3, 4..5, 6..7], (0, 0)));
        // The caret in an input shorter than its lines (a short pane) is on its last.
        let input = Rect::new(10, 20, w, 2);
        assert_eq!(cursor(input, six, six.len()), (12, 21));
        assert_eq!(cursor(Rect::new(10, 20, w, 4), six, 0), (11, 20));
        // A two-row input shows the two lines around the caret, and the caret is on one of
        // them, wherever it is: at the start, in the middle, at the end.
        assert_eq!(input_view(six, 0, w, 2), (vec![0..1, 2..3], (0, 0)));
        assert_eq!(cursor(Rect::new(10, 20, w, 2), six, 0), (11, 20));
        assert_eq!(input_view(six, 4, w, 2), (vec![2..3, 4..5], (1, 0)));
        assert_eq!(cursor(Rect::new(10, 20, w, 2), six, 4), (11, 21));
        assert_eq!(
            input_view(six, six.len(), w, 2),
            (vec![8..9, 10..11], (1, 1))
        );
    }

    #[test]
    fn the_reply_line_keeps_replying_to_and_its_author_whole_and_cuts_the_question() {
        let t = thread(1);
        let talk = talk_of(&t);
        let m = Message {
            id: 7,
            author: "dev-1".into(),
            text: "question: ship now, or wait for the proofs?\nmore".into(),
            at: 0,
        };
        let line = |w: usize| {
            let writing = Writing {
                draft: "",
                at: 0,
                keys: true,
                rows: 1,
            };
            let lines = composer_lines(&talk, &t, writing, Some(&m), w, Depth::Ansi256);
            assert!(lines.iter().all(|l| l.width() == w), "{lines:?}");
            lines[1].to_string()
        };
        // Just wide enough for them: whole, with nothing of the question.
        assert_eq!(line(18), " replying to dev-1");
        // The question takes what is left, cut.
        assert_eq!(line(30), " replying to dev-1: question:…");
        assert_eq!(line(19), " replying to dev-1…");
        // Wide, all of its first line.
        let wide = line(80);
        assert!(
            wide.starts_with(" replying to dev-1: question: ship now, or wait for the proofs?")
        );
        assert!(!wide.contains("more"));
    }

    #[test]
    fn a_long_message_folds_behind_read_more_and_opens_in_place() {
        let words = (0..160)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let messages = vec![
            Message {
                id: 1,
                author: "dev-1".into(),
                text: words.clone(),
                at: 0,
            },
            Message {
                id: 2,
                author: "dev-1".into(),
                text: "short".into(),
                at: 60,
            },
            Message {
                id: 3,
                author: "system".into(),
                text: "@manager could not deliver to dev-1: herdr refused".into(),
                at: 120,
            },
        ];
        let t = Thread::whole(ThreadId(8), "t".into(), None, None, messages);
        let talk = talk_of(&t);
        let body = Rect::new(0, 0, 120, 40);
        let fold = ResolvedFold::default();
        let closed = board_layout(
            &talk,
            Some(ThreadId(8)),
            fold,
            &BTreeSet::new(),
            false,
            "",
            0,
            body,
        );
        // The keys step through both messages of the run and the delivery failure.
        let ids: Vec<i64> = closed.starts.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [1, 2, 3]);
        // "read more" and the failure's "details" toggle.
        let toggled: Vec<i64> = closed.toggles.iter().map(|(_, id)| *id).collect();
        assert_eq!(toggled, [1, 3]);
        let w = usize::from(closed.conv.width);
        let text = |open: &BTreeSet<i64>| -> Vec<String> {
            (conv_window(
                &talk,
                &t,
                w,
                false,
                open,
                None,
                0,
                Depth::Ansi256,
                0..usize::MAX,
            )
            .iter())
            .map(|l| l.to_string())
            .collect()
        };
        let shut = text(&BTreeSet::new());
        let pieces = shut.iter().filter(|r| r.contains("▌ w")).count();
        assert_eq!(pieces, CLIP, "{shut:#?}");
        assert!(shut.iter().any(|r| r.contains("▌ read more")));
        assert!(
            shut.iter()
                .any(|r| r.contains("delivery failed to dev-1 · details"))
        );
        let open = BTreeSet::from([1, 3]);
        let opened = board_layout(&talk, Some(ThreadId(8)), fold, &open, false, "", 0, body);
        assert!(opened.conv_lines > closed.conv_lines);
        let all = text(&open);
        assert!(all.iter().any(|r| r.contains("w159")), "{all:#?}");
        assert!(all.iter().any(|r| r.contains("▌ show less")));
        assert!(
            all.iter()
                .any(|r| r.contains("delivery failed to dev-1 · hide details"))
        );
        assert!(all.iter().any(|r| r.contains("herdr refused")));
        let d = Depth::TrueColor;
        let drawn = conv_window(&talk, &t, w, false, &open, Some(2), 0, d, 0..usize::MAX);
        for l in &drawn {
            assert_eq!(l.width(), w);
        }
        // The selected message is outlined: its edge in the accent, on the selected
        // background; the message before it keeps its author's colour.
        let edge_of = |needle: &str| {
            let l = drawn
                .iter()
                .find(|l| l.to_string().contains(needle))
                .unwrap();
            l.spans.iter().find(|s| s.content == "▌").unwrap().style
        };
        let short = edge_of("▌ short");
        assert_eq!(
            (short.fg, short.bg),
            (Some(accent(d)), Some(selected_bg(d)))
        );
        assert_ne!(edge_of("▌ w0 ").fg, Some(accent(d)));
    }
}
