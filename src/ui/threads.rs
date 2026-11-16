//! Threads in words, as UI v0.1.7 names them on every board: a thread linked to a task
//! takes the task's title, with its stage and who is on it, and its ids dim at the right;
//! under it, where it stands (`Talk::stands`). The same header heads a thread in the full
//! board and in the inline previews, and the full board's thread list reads the same way.
//!
//! Also the "Needs you" strip pinned at the top of every board: the open questions for the
//! owner in that board's threads (`Talk::needs`), each its author, its first line and the
//! thread it is in.
//!
//! The number of lines never depends on the time: an age is planned at its widest.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::chat::event_text;
use super::message_board::{highlight, pill, status_style};
use super::theme::{Depth, bad, bold, dim, fit, ok};
use crate::board::{Message, Talk, Thread, ThreadStatus, age, is_tower};
use crate::inbox::Alert;

/// The widest an age gets: "999d".
const AGE_COLUMNS: usize = 4;

/// An age as of `now`, right-aligned to `AGE_COLUMNS`; blank when planning (`None`).
fn when(at: Option<i64>, now: Option<i64>) -> String {
    match (at, now) {
        (Some(at), Some(now)) => format!("{:>AGE_COLUMNS$}", age(at, now)),
        _ => " ".repeat(AGE_COLUMNS),
    }
}

/// "T3 · 40 messages · 5m".
fn meta(talk: &Talk, t: &Thread, now: Option<i64>) -> String {
    let n = match talk.total(t) {
        1 => "1 message".to_string(),
        n => format!("{n} messages"),
    };
    format!("{} · {n} · {}", talk.ids(t), when(talk.latest(t), now))
}

/// Thread `t`'s header `w` wide, ages as of `now` (`None` to count its lines): its status
/// and name in words with its ids, size and age at the right (on the next line when they do
/// not fit), then dim what it is on and who started it, then where it stands unless
/// `stands_line` is false (the full board in a narrow pane leaves it out).
pub fn head(
    talk: &Talk,
    t: &Thread,
    w: usize,
    now: Option<i64>,
    stands_line: bool,
    depth: Depth,
) -> Vec<Line<'static>> {
    let left = vec![
        Span::raw(" "),
        Span::styled(pill(t), status_style(t.status(), depth)),
        Span::raw(" "),
        Span::styled(talk.name(t), bold()),
    ];
    let right = format!("  {} ", meta(talk, t, now));
    let used: usize = left.iter().map(Span::width).sum();
    let mut out = Vec::new();
    if used + right.width() <= w {
        let mut s = left;
        s.push(Span::raw(" ".repeat(w - used - right.width())));
        s.push(Span::styled(right, dim(depth)));
        out.push(Line::from(s));
    } else {
        out.push(fit(left, w));
        let meta = format!(" {}", meta(talk, t, now));
        out.push(fit(vec![Span::styled(meta, dim(depth))], w));
    }
    let state = format!(" {} · started by {}", talk.state(t), t.by);
    out.push(fit(vec![Span::styled(state, dim(depth))], w));
    if stands_line && let Some(line) = stands(talk, t, " where it stands ", w, depth) {
        out.push(line);
    }
    out
}

/// Where thread `t` stands on one line `w` wide after `lead_in` (dim): its author in their
/// colour, then the line.
pub fn stands(
    talk: &Talk,
    t: &Thread,
    lead_in: &str,
    w: usize,
    depth: Depth,
) -> Option<Line<'static>> {
    let (who, text) = talk.stands(t)?;
    let mut s = vec![Span::styled(lead_in.to_string(), dim(depth))];
    // Tower's own word reads as the chat's quiet line does, with no "system:" label.
    if is_tower(&who) {
        let m = Message {
            id: 0,
            author: who,
            text,
            at: 0,
        };
        let said = event_text(&m, talk.lead);
        let first = said.lines().next().unwrap_or_default().to_string();
        s.push(Span::styled(first, dim(depth)));
        return Some(fit(s, w));
    }
    if !who.is_empty() {
        let colour = talk.people().colour(&who, depth);
        s.push(Span::styled(who, Style::new().fg(colour)));
        s.push(Span::raw(": "));
    }
    s.extend(highlight(&text, true, false, depth));
    Some(fit(s, w))
}

/// Thread `t`'s mark in the thread list: a red ● while a question for the owner waits in
/// it, ○ while it is open, a green ✓ once resolved.
pub fn mark(talk: &Talk, t: &Thread, depth: Depth) -> Span<'static> {
    if talk.needs_you(t) {
        Span::styled("●", Style::new().fg(bad(depth)))
    } else if t.status() == ThreadStatus::Open {
        Span::styled("○", dim(depth))
    } else {
        Span::styled("✓", Style::new().fg(ok(depth)))
    }
}

/// How many of `n` questions the strip lists in about `room` lines: all of them while they
/// fit under its heading, else as many as fit above a line counting the rest, and always
/// one, so a strip never hides every question (it then takes three lines, past `room`).
pub fn strip_listed(n: usize, room: usize) -> usize {
    if n == 0 || n < room {
        n
    } else {
        room.saturating_sub(2).clamp(1, n)
    }
}

/// How many lines the strip takes for `n` questions in at most `room` lines: one saying
/// there are none; else its heading, the questions it lists, and a line counting the rest
/// when not all fit.
pub fn strip_height(n: usize, room: usize) -> usize {
    match n {
        0 => 1,
        n => {
            let listed = strip_listed(n, room);
            1 + listed + usize::from(listed < n)
        }
    }
}

/// The "Needs you" strip `w` wide for `needs`, newest first, ages as of `now`, in at most
/// `room` lines (`usize::MAX` where the pane scrolls).
pub fn strip(
    talk: &Talk,
    needs: &[&Alert],
    w: usize,
    room: usize,
    now: i64,
    depth: Depth,
) -> Vec<Line<'static>> {
    if needs.is_empty() {
        let text = " nothing on this board needs you";
        return vec![fit(vec![Span::styled(text, dim(depth))], w)];
    }
    let head = format!(" ⚠ Needs you · {}", needs.len());
    let mut out = vec![fit(vec![Span::styled(head, bold().fg(bad(depth)))], w)];
    let listed = strip_listed(needs.len(), room);
    for a in &needs[..listed] {
        out.push(need_line(talk, a, w, now, depth));
    }
    if listed < needs.len() {
        let more = format!("   … {} more, in the inbox (i)", needs.len() - listed);
        out.push(fit(vec![Span::styled(more, dim(depth))], w));
    }
    out
}

/// One question in the strip: its author in their colour, its first line, and at the
/// right the thread it is in, by name, and its age.
fn need_line(talk: &Talk, a: &Alert, w: usize, now: i64, depth: Depth) -> Line<'static> {
    let m = &a.message;
    let home = talk.home_of(a.thread);
    let name = (talk.threads.iter())
        .find(|t| t.id == home)
        .map_or_else(|| home.to_string(), |t| talk.name(t));
    let right = format!("  in {name} · {} ", age(m.at, now));
    let colour = talk.people().colour(&m.author, depth);
    let mut left = vec![
        Span::raw("   "),
        Span::styled(m.author.clone(), bold().fg(colour)),
        Span::raw(" "),
    ];
    let first = m.text.lines().next().unwrap_or_default();
    left.extend(highlight(first, true, false, depth));
    // The thread's name gives way before the question does, down to a third of the line.
    let right = fit(
        vec![Span::styled(right.clone(), dim(depth))],
        right.width().min(w / 3),
    );
    let room = w.saturating_sub(right.width());
    let mut s = fit(left, room).spans;
    s.extend(right.spans);
    fit(s, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_always_lists_one_question_and_counts_the_rest() {
        // Room for all: every question under the heading.
        assert_eq!((strip_listed(3, 10), strip_height(3, 10)), (3, 4));
        assert_eq!((strip_listed(3, 4), strip_height(3, 4)), (3, 4));
        // One short: as many as fit above the count line.
        assert_eq!((strip_listed(3, 3), strip_height(3, 3)), (1, 3));
        assert_eq!((strip_listed(6, 5), strip_height(6, 5)), (3, 5));
        // Two lines: still one question, so three lines.
        assert_eq!((strip_listed(3, 2), strip_height(3, 2)), (1, 3));
        assert_eq!((strip_listed(1, 2), strip_height(1, 2)), (1, 2));
        // None: one line says so.
        assert_eq!((strip_listed(0, 2), strip_height(0, 2)), (0, 1));
    }
}
