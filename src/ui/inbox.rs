//! The owner's alerts on screen, as in the golden UI's layout E: the alert line under the
//! header (one line, never more), and the inbox it opens, across the body: a "◀ back  INBOX"
//! line over the NEED YOU section, then FOR YOUR AWARENESS (no read section: read items stay
//! readable in their threads), each item its author
//! in their role's colour, its message on at most two lines and, dim at the right, its
//! repo, its thread in words and its age (UI v0.1.7; the two lines, UI v0.1.13).
//!
//! The inbox is planned once, unstyled (`plan`), and both its layout (which line opens
//! which item) and its drawing read the plan, so they cannot disagree. What is in each
//! section comes from `crate::inbox`, never from here.

use std::ops::Range;

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::chat::{failed_to, marked};
use super::control::Control;
use super::message_board::{clamp_end, highlight_marked, wrap_line};
use super::theme::{Depth, accent, bad, bold, dim, fit, info, ok, selected_bg};
use super::tree::split_line;
use crate::board::{
    Board, DELIVERY_FAILURE, OWNER_HANDLE, SYSTEM_AUTHOR, Talk, age, owner_mentions,
};
use crate::inbox::{Alert, Inbox, Kind};

/// Columns "◀ back" takes on the inbox's top line, for clicking it.
pub const BACK_COLUMNS: u16 = 7;
/// The author column grows to the longest author listed, up to this.
const LEAD_MAX: usize = 22;
/// The fewest columns an inbox item's message keeps before its repo and thread go.
const MESSAGE_MIN: usize = 12;
/// The widest an age gets on the alert line ("999d"), reserved so the layout, which has no
/// time, lays the line out as the drawing does.
const AGE_COLUMNS: usize = 4;
/// The most lines an inbox item's message takes, as the copy's `.txt clamp2` clamps it.
const MESSAGE_LINES: usize = 2;

/// The alert line's background, the golden UI's #1d1517, on a 24-bit terminal; none on the
/// 256-colour palette, which has no tint this dark.
pub(crate) fn alert_bg(depth: Depth) -> Style {
    match depth {
        Depth::TrueColor => Style::new().bg(Color::Rgb(0x1d, 0x15, 0x17)),
        Depth::Ansi256 => Style::new(),
    }
}

/// Each repo's board as the inbox names threads and colours authors on it, built once for
/// all the alerts drawn.
fn talks(board: &Board) -> Vec<Talk<'_>> {
    board
        .repos
        .iter()
        .map(|r| Talk::of_repo(board, r))
        .collect()
}

/// Where an alert is, in words: its repo's name and its thread's (`Talk::name`, the thread
/// a folded one joins), and its author's colour on that board; `talks` from `talks`.
fn whereabouts(board: &Board, talks: &[Talk], a: &Alert, depth: Depth) -> (String, String, Color) {
    let (Some(r), Some(talk)) = (
        board.repo(a.repo),
        (board.repos.iter().position(|r| r.id == a.repo)).and_then(|i| talks.get(i)),
    ) else {
        return (a.repo.to_string(), a.thread.to_string(), Color::Reset);
    };
    let home = talk.home_of(a.thread);
    let thread = (talk.threads.iter())
        .find(|t| t.id == home)
        .map_or_else(|| home.to_string(), |t| talk.name(t));
    (
        r.name(),
        thread,
        talk.people().colour(&a.message.author, depth),
    )
}

/// An alert in words, as the alert line names it, for a notice about it: "<author> in
/// ‘<thread>’ · <repo>", the thread in words (`Talk::name`), never a message id.
pub(crate) fn alert_name(board: &Board, a: &Alert) -> String {
    let (repo, thread, _) = whereabouts(board, &talks(board), a, Depth::TrueColor);
    format!("{} in ‘{thread}’ · {repo}", a.message.author)
}

/// What an alert says, unstyled, as its inbox row shows it: its message on at most
/// `MESSAGE_LINES` lines, each with the byte ranges in it to mark in the row's red, and
/// whether the message goes on past the last, and whether its first line may tone a leading
/// tag.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    lines: Vec<(String, Vec<Range<usize>>)>,
    more: bool,
    tagged: bool,
}

impl Said {
    /// What `a` says on lines `room` columns wide: its message, its marks the owner's
    /// mentions, or for a delivery failure its short form, "@manager delivery failed to X"
    /// (its full text is in its thread), its mark the "@manager". As the copy's row (`.txt
    /// clamp2`, `white-space: normal`) draws either: its line breaks and tabs are spaces,
    /// each run of spaces one, wrapped as the copy's `.clamp2` wraps it (`wrap_line`) and
    /// clamped to `MESSAGE_LINES` lines.
    fn of(a: &Alert, room: usize) -> Said {
        let text = &a.message.text;
        let failure = a.message.author.eq_ignore_ascii_case(SYSTEM_AUTHOR)
            && text.starts_with(DELIVERY_FAILURE);
        let (text, marks) = if failure {
            let short = format!("{OWNER_HANDLE} delivery failed to {}", failed_to(text));
            (short, std::iter::once(0..OWNER_HANDLE.len()).collect())
        } else {
            (text.clone(), owner_mentions(text))
        };
        // The same bytes, so the marks found in `text` are where they were.
        let flat: String = (text.chars())
            .map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let mut lines = wrap_line(&flat, room);
        let more = lines.len() > MESSAGE_LINES;
        lines.truncate(MESSAGE_LINES);
        let found = marked(&flat, 0, &marks, lines.iter().map(String::as_str));
        Said {
            lines: lines.into_iter().zip(found).collect(),
            more,
            // The copy tones the raw text, so an indented first line, folded here, stays
            // plain.
            tagged: !flat.starts_with(' '),
        }
    }

    /// How many list lines it takes.
    fn height(&self) -> usize {
        self.lines.len()
    }

    /// Its lines, styled, each at most `room` columns: the last ends in "…" where the
    /// message goes on, right after its last word, or in place of its last character when
    /// the line is full, as the copy's clamp ends it.
    fn lines(&self, awareness: bool, room: usize, depth: Depth) -> Vec<Vec<Span<'static>>> {
        let n = self.lines.len();
        (self.lines.iter().enumerate())
            .map(|(k, (piece, marks))| {
                let head = k == 0 && self.tagged;
                if !(self.more && k + 1 == n) {
                    return highlight_marked(piece, marks, head, awareness, depth);
                }
                // The "…" in the row's text style, never a mark's: the piece is cut first, to
                // leave it a column, and then styled.
                let end = clamp_end(piece, room);
                let marks: Vec<Range<usize>> = (marks.iter())
                    .filter(|r| r.start < end)
                    .map(|r| r.start..r.end.min(end))
                    .collect();
                let mut spans = highlight_marked(&piece[..end], &marks, head, awareness, depth);
                spans.push(Span::raw("…"));
                spans
            })
            .collect()
    }
}

/// The alert line, exactly `w` columns: "⚠ N need you · ⓘ N for your awareness · newest:
/// <author> in '<thread>' · <repo> · <age>" (its text is in the inbox) with "new lead (n) ▶
/// inbox (i) ▶" at the right; with nothing needing the owner, "nothing needs you", and
/// "· ⓘ N for your awareness" while there is one. Short of width, where the newest item is
/// goes first: its thread's name is cut, then left out, then its repo's; its author and age
/// stay together. Then "new lead (n) ▶" goes, then "inbox (i) ▶" (a click anywhere on the
/// line opens the inbox), and when not even its author and age fit, the item goes whole
/// and the controls come back while they fit. The counts stay whole until they alone are
/// wider than the pane.
///
/// `now` gives the newest item's age; `None` leaves it out, for a layout: an age is
/// reserved at its widest, so where the controls go is the same either way.
pub fn alert_line(board: &Board, w: usize, now: Option<i64>, depth: Depth) -> Line<'static> {
    alert_parts(board, w, now, depth).0
}

/// The alert line, and the columns "new lead (n) ▶" takes on it while it shows.
pub(crate) fn alert_parts(
    board: &Board,
    w: usize,
    now: Option<i64>,
    depth: Depth,
) -> (Line<'static>, Option<(usize, usize)>) {
    let inbox = &board.inbox;
    let sep = || Span::styled(" · ", dim(depth));
    let aware = Span::styled(
        format!("ⓘ {} for your awareness", inbox.awareness.len()),
        bold().fg(info(depth)),
    );
    let mut counts = vec![Span::raw(" ")];
    // The newest question's author, colour, thread and repo, and its age: laid out once the
    // room for it is known, so its thread's name is what gives way.
    let mut top_parts = None;
    match inbox.newest() {
        None => {
            counts.push(Span::styled("nothing needs you", bold().fg(ok(depth))));
            if !inbox.awareness.is_empty() {
                counts.extend([sep(), aware]);
            }
        }
        Some(top) => {
            counts.extend([
                Span::styled(
                    format!("⚠ {} need you", inbox.need_you.len()),
                    bold().fg(bad(depth)),
                ),
                sep(),
                aware,
            ]);
            // Laid out (no `now`), only where the controls go matters: no names needed.
            let (repo, thread, colour) = match now {
                Some(_) => whereabouts(board, &talks(board), top, depth),
                None => (String::new(), String::new(), Color::Reset),
            };
            let when = now.map(|now| format!(" · {}", age(top.message.at, now)));
            top_parts = Some((top.message.author.clone(), colour, thread, repo, when));
        }
    }
    let new_lead = Control::NewLead.texts()[0];
    let inbox_ctl = || {
        vec![
            Span::styled(" inbox (i) ▶", bold().fg(accent(depth))),
            Span::raw(" "),
        ]
    };
    let both_ctl = || {
        let mut c = vec![
            Span::raw(" "),
            Span::styled(new_lead, bold().fg(accent(depth))),
            Span::raw(" "),
        ];
        c.extend(inbox_ctl());
        c
    };
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let counts_w = width(&counts);
    // What the newest item needs: its author and an age at its widest, so the layout (no
    // `now`) and the drawing choose the same way.
    let base = top_parts
        .as_ref()
        .map(|(author, ..)| format!(" · newest: {author} · ").width() + AGE_COLUMNS);
    let fits = |extra: usize| counts_w + extra <= w;
    // The item beside both controls, else beside "inbox (i) ▶", else alone; with no room
    // for it, the controls while they fit. "new lead (n) ▶" only where it is drawn.
    let (with_item, right) = match base {
        Some(b) if fits(b + width(&both_ctl())) => (true, both_ctl()),
        Some(b) if fits(b + width(&inbox_ctl())) => (true, inbox_ctl()),
        Some(b) if fits(b) => (true, Vec::new()),
        _ if fits(width(&both_ctl())) => (false, both_ctl()),
        _ if fits(width(&inbox_ctl())) => (false, inbox_ctl()),
        _ => (false, Vec::new()),
    };
    let right_w = width(&right);
    let drawn = right.iter().any(|s| s.content == new_lead);
    let at = drawn.then(|| (w + 1 - right_w, w + 1 - right_w + new_lead.width()));
    let room = w.saturating_sub(counts_w + right_w);
    // The newest item: "· newest: <author>", then where it is, then its age. Its author and
    // age always go together; short of room its thread's name is cut, then left out, then
    // its repo's.
    let newest = |room: usize| -> Vec<Span<'static>> {
        let Some((author, colour, thread, repo, when)) = top_parts.as_ref() else {
            return Vec::new();
        };
        let when = when.clone().unwrap_or_default();
        let mut extra = room.saturating_sub(format!(" · newest: {author}{when}").width());
        // " · <repo>": cut, then dropped once fewer than four columns of it fit.
        let repo_room = extra.saturating_sub(3).min(repo.width());
        let repo = (extra >= 3 + repo_room && repo_room >= repo.width().min(4)).then(|| {
            extra -= 3 + repo_room;
            fit(vec![Span::raw(repo.clone())], repo_room).to_string()
        });
        // " in ‘<thread>’": only with its repo, and cut first.
        let thread_room = extra.saturating_sub(6).min(thread.width());
        let place = match (&repo, thread_room >= thread.width().min(2)) {
            (Some(_), true) => {
                let thread = fit(vec![Span::raw(thread.clone())], thread_room).to_string();
                format!(" in ‘{thread}’")
            }
            _ => String::new(),
        };
        let repo = repo.map(|r| format!(" · {r}")).unwrap_or_default();
        vec![
            Span::styled(" · newest: ", dim(depth)),
            Span::styled(author.clone(), Style::new().fg(*colour)),
            Span::styled(format!("{place}{repo}{when}"), dim(depth)),
        ]
    };
    let mut line = if with_item || right_w > 0 {
        let mut spans = counts;
        let middle = if with_item { newest(room) } else { Vec::new() };
        spans.extend(fit(middle, room).spans);
        spans.extend(right);
        Line::from(spans)
    } else {
        fit(counts, w)
    };
    line.style = alert_bg(depth);
    (line, at)
}

/// One row of the inbox's list: an item's takes as many lines as its message (`Said`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row<'a> {
    Blank,
    Head(Section, usize),
    Nothing,
    Item(&'a Alert, Said),
}

impl Row<'_> {
    /// How many list lines it takes.
    fn height(&self) -> usize {
        match self {
            Row::Item(_, said) => said.height(),
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    NeedYou,
    Awareness,
}

/// The inbox's list, row by row, for a list `w` wide: each section's head and its items, or
/// "nothing"; `talks` from `talks`.
fn plan<'a>(board: &'a Board, talks: &[Talk], w: usize) -> Vec<Row<'a>> {
    let inbox = &board.inbox;
    let author_w = author_width(inbox, w);
    let mut rows = Vec::new();
    for (section, items) in [
        (Section::NeedYou, &inbox.need_you),
        (Section::Awareness, &inbox.awareness),
    ] {
        if !rows.is_empty() {
            rows.push(Row::Blank);
        }
        rows.push(Row::Head(section, items.len()));
        if items.is_empty() {
            rows.push(Row::Nothing);
        }
        rows.extend(items.iter().map(|a| {
            let (repo, thread, _) = whereabouts(board, talks, a, Depth::Ansi256);
            Row::Item(a, Said::of(a, message_room(&repo, &thread, author_w, w)))
        }));
    }
    rows
}

/// Where the inbox goes in the body and which list line opens which item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxLayout {
    /// The "◀ back  INBOX" line.
    pub top: Rect,
    pub list: Rect,
    /// The message id of the item on each list line, an item's on each of its lines; `None`
    /// for heads and gaps.
    pub rows: Vec<Option<i64>>,
}

/// Lays the inbox of `board` out across `body`: the top line, then the list.
pub fn inbox_layout(board: &Board, body: Rect) -> InboxLayout {
    let top_h = body.height.min(1);
    InboxLayout {
        top: Rect::new(body.x, body.y, body.width, top_h),
        list: Rect::new(body.x, body.y + top_h, body.width, body.height - top_h),
        rows: plan(board, &talks(board), usize::from(body.width))
            .iter()
            .flat_map(|r| {
                let id = match r {
                    Row::Item(a, _) => Some(a.message.id),
                    _ => None,
                };
                std::iter::repeat_n(id, r.height())
            })
            .collect(),
    }
}

/// "◀ back  INBOX", exactly `w` columns.
pub fn top_line(w: usize, depth: Depth) -> Line<'static> {
    fit(
        vec![
            Span::styled(" ◀ back", dim(depth)),
            Span::styled("  INBOX", bold()),
        ],
        w,
    )
}

/// The inbox's list lines, each exactly `w` columns, with `selected`'s lines on the
/// selected background and ages as of `now`.
pub fn list_lines(
    board: &Board,
    selected: Option<i64>,
    w: usize,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    let talks = talks(board);
    let author_w = author_width(&board.inbox, w);
    plan(board, &talks, w)
        .iter()
        .flat_map(|row| match row {
            Row::Blank => vec![fit(Vec::new(), w)],
            Row::Nothing => vec![fit(vec![Span::styled("   nothing", dim(depth))], w)],
            Row::Head(section, n) => {
                let (text, style) = match section {
                    Section::NeedYou => (format!(" ⚠ {n} NEED YOU"), bold().fg(bad(depth))),
                    Section::Awareness => {
                        (format!(" ⓘ {n} FOR YOUR AWARENESS"), bold().fg(info(depth)))
                    }
                };
                vec![fit(vec![Span::styled(text, style)], w)]
            }
            Row::Item(a, said) => {
                let mut lines = item_lines(board, &talks, a, said, author_w, w, now, depth);
                if Some(a.message.id) == selected {
                    for line in &mut lines {
                        line.style = Style::new().bg(selected_bg(depth));
                    }
                }
                lines
            }
        })
        .collect()
}

/// The author column's width in a list `w` wide: the longest author listed, up to
/// `LEAD_MAX` and a third of the line.
fn author_width(inbox: &Inbox, w: usize) -> usize {
    (inbox.all())
        .map(|a| a.message.author.width())
        .max()
        .unwrap_or(0)
        .min(LEAD_MAX)
        .min(w / 3)
}

/// The columns the right block may take in a list `w` wide, `author_w` its author column:
/// at most a third of the line, and what leaves the message `MESSAGE_MIN`.
fn right_budget(author_w: usize, w: usize) -> usize {
    let min_left = 3 + author_w + 2 + MESSAGE_MIN;
    (w / 3).min(w.saturating_sub(min_left + 1))
}

/// The dim block at an item's right, " <repo> · <thread> · <age> ", in at most `budget`
/// columns: the thread's name gives way first, then the repo's, and the block goes whole
/// (`None`) before either is cut below two columns (one that short whole still shows).
fn right_text(repo: &str, thread: &str, when: &str, budget: usize) -> Option<String> {
    // " <repo> · <thread> · <age> " less the repo and thread.
    let fixed = format!("  ·  · {when} ").width();
    let repo_room = budget.saturating_sub(fixed + 4).min(repo.width());
    let thread_room = budget.saturating_sub(fixed + repo_room).min(thread.width());
    if repo_room < repo.width().min(2) || thread_room < thread.width().min(2) {
        return None;
    }
    let repo = fit(vec![Span::raw(repo.to_string())], repo_room).to_string();
    let thread = fit(vec![Span::raw(thread.to_string())], thread_room).to_string();
    Some(format!(" {repo} · {thread} · {when} "))
}

/// The columns an item's message has on each of its lines, in a list `w` wide: what its
/// author column and its right block leave. The layout has no clock, so the block is taken
/// at its widest for any age ("9m" to "999d"), and the message breaks where it does in the
/// layout whatever its age.
fn message_room(repo: &str, thread: &str, author_w: usize, w: usize) -> usize {
    let budget = right_budget(author_w, w);
    let right = (2..=AGE_COLUMNS)
        .filter_map(|n| right_text(repo, thread, &"9".repeat(n), budget))
        .map(|r| r.width())
        .max()
        .unwrap_or(0);
    w.saturating_sub(3 + author_w + 2 + right).max(1)
}

/// "   <author>  <message>", the author in their role's colour, and dim at the right
/// " <repo> · <thread in words> · 15m ", at most a third of the line and dropped when the
/// pane is too narrow to show it beside the author; the message's second line, if any,
/// under its first.
#[allow(clippy::too_many_arguments)]
fn item_lines(
    board: &Board,
    talks: &[Talk],
    a: &Alert,
    said: &Said,
    author_w: usize,
    w: usize,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    let (repo, thread, colour) = whereabouts(board, talks, a, depth);
    let room = message_room(&repo, &thread, author_w, w);
    let mut texts = (said.lines(a.kind == Kind::Awareness, room, depth)).into_iter();
    let mut left = vec![Span::raw("   ")];
    let author = Span::styled(a.message.author.clone(), bold().fg(colour));
    left.extend(fit(vec![author], author_w).spans);
    left.push(Span::raw("  "));
    left.extend(texts.next().unwrap_or_default());
    // The message keeps at least `MESSAGE_MIN` columns. The right block takes at most a
    // third of the line and what the message leaves, and goes whole before the message
    // would.
    let min_left = 3 + author_w + 2 + MESSAGE_MIN;
    let when = age(a.message.at, now);
    let right = right_text(&repo, &thread, &when, right_budget(author_w, w))
        .map(|r| vec![Span::styled(r, dim(depth))])
        .unwrap_or_default();
    let mut lines = vec![split_line(left, right, min_left, w)];
    lines.extend(texts.map(|text| {
        let mut spans = vec![Span::raw(" ".repeat(3 + author_w + 2))];
        spans.extend(text);
        fit(spans, w)
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Message, RepoId, ThreadId};

    /// The inbox row's message lines for `author`'s `text`, `room` columns wide, styled.
    fn spans(author: &str, text: &str, room: usize) -> Vec<Vec<Span<'static>>> {
        let a = Alert {
            kind: Kind::NeedsYou,
            repo: RepoId(1),
            thread: ThreadId(1),
            message: Message {
                id: 1,
                author: author.into(),
                text: text.into(),
                at: 0,
            },
        };
        Said::of(&a, room).lines(false, room, Depth::TrueColor)
    }

    /// Each line's text and its spans' styles.
    fn styled(lines: Vec<Vec<Span<'static>>>) -> Vec<Vec<(String, Style)>> {
        (lines.into_iter())
            .map(|l| l.into_iter().map(|s| (s.content.into(), s.style)).collect())
            .collect()
    }

    /// The inbox row's message lines for `text`, `room` columns wide, unstyled.
    fn row(text: &str, room: usize) -> Vec<String> {
        (spans("cat-lead", text, room).iter())
            .map(|spans| spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn a_delivery_failures_row_wraps_onto_two_lines_as_the_copys_clamp_does() {
        // As headless Chrome draws the copy's failure row (`.txt clamp2`, #51): its short
        // form wraps at its words onto a second line, clamped there like any row's, where
        // it had been one line cut at the row's edge.
        let text = "@manager could not deliver to Garden Shed Lead, the lead's pane is gone";
        let red = bold().fg(bad(Depth::TrueColor));
        let plain = Style::new();
        let s = |t: &str, style| (t.to_string(), style);
        assert_eq!(
            styled(spans(SYSTEM_AUTHOR, text, 20)),
            [
                vec![s("@manager", red), s(" delivery", plain)],
                vec![s("failed to Garden", plain), s("…", plain)],
            ]
        );
        assert_eq!(
            styled(spans(SYSTEM_AUTHOR, text, 30)),
            [
                vec![s("@manager", red), s(" delivery failed to", plain)],
                vec![s("Garden Shed Lead", plain)],
            ]
        );
        assert_eq!(
            styled(spans(SYSTEM_AUTHOR, text, 12)),
            [
                vec![s("@manager", red)],
                vec![s("delivery", plain), s("…", plain)],
            ]
        );
        // Where "@manager" itself breaks across the lines, both its parts stay red.
        assert_eq!(
            styled(spans(SYSTEM_AUTHOR, text, 4)),
            [vec![s("@man", red)], vec![s("age", red), s("…", plain)]]
        );
    }

    #[test]
    fn an_inbox_rows_words_break_where_the_copys_clamp_breaks_them() {
        // As headless Chrome draws the copy's `.txt clamp2` (`overflow-wrap: anywhere`): a
        // word too wide for a line starts on a line of its own, and a word breaks after a `-`.
        let link = "decision: @manager see https://example.invalid/shed/plans/roof/very/long/\
                    path/that/never/ends/and/keeps/going for the plan";
        assert_eq!(
            row(link, 27),
            ["decision: @manager see", "https://example.invalid/sh…"]
        );
        assert_eq!(
            row(link, 47),
            [
                "decision: @manager see",
                "https://example.invalid/shed/plans/roof/very/l…"
            ]
        );
        assert_eq!(
            row(
                "question: @manager can we re-run the long-running check on the \
                 cross-platform build before Friday?",
                47
            ),
            [
                "question: @manager can we re-run the long-",
                "running check on the cross-platform build…"
            ]
        );
        assert_eq!(
            row(
                "decision: the garden-shed-lead-senior-reviewer keeps the plan @manager",
                27
            ),
            ["decision: the garden-shed-", "lead-senior-reviewer keeps…"]
        );
    }
}
