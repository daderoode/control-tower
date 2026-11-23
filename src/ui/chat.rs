//! The message board as a group chat, as UI v0.1.7 draws it: every board view (the full
//! board's conversation and the inline boards) shows a thread's messages in bubbles. The
//! author's name sits above the bubble in its role colour with a dim role word, never cut.
//! Each message in a bubble has its age, and any tags ("needs you", "decision",
//! "blocked"), at the top right of its first line. The owner's own messages are on the
//! right as "you (manager)", everyone else's on the left.
//!
//! - One person's run of messages is one bubble, a dim rule between its messages.
//! - A long message shows its first lines (`Shape::clip`), then "read more"; open, it ends
//!   with "show less". A message that would hide one line only shows in full.
//! - A short ack (`board::is_ack`) is a small "↳ qa: copy" reply under the bubble it
//!   answers, wrapping when it is long.
//! - Tower's own messages are quiet centred lines, wrapped at `EVENT_MAX`: consecutive
//!   nudges of one worker as one ("dev-1 was nudged 3 times"), the watch's flags in a
//!   chat's words, and a delivery failure as "delivery failed to X · details", which opens
//!   its full text under it, left-aligned in a centred box as wide as its widest line.
//! - Separators where how long ago the messages were changes: "now", "within the hour",
//!   "3h ago", "yesterday".
//!
//! The terminal has one font size, so the copy's smaller type is dim colour here.
//!
//! `plan` says what each line is without styling it, so a layout (heights, where each
//! message starts, what a click toggles) and the lines drawn come from the same rows. The
//! time a plan is made for (`Shape::now`) decides its separators, which a preview has none
//! of, and an ack's age, which a plan takes at its real width as drawn: so a chat's height,
//! a preview's too, can grow by a line for an ack as its age widens (9m to 10m), as the
//! copy reflows.
//!
//! A bubble fits its text as if no width limited it, and its name line, from `BUBBLE_MIN`
//! columns up to `widest`: 85% of the chat's width, with at most `TEXT_MAX` columns of text.

use std::collections::BTreeSet;
use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::message_board::{
    dropped, fold_spaces, highlight_marked, may_tone_tag, rest_after, role_colour, units, wrap,
    wrap_event_line, wrap_line_paths,
};
use super::theme::{Depth, accent, bad, bold, dim, fit, info, selected_bg, warn};
use crate::board::{
    DELIVERY_FAILURE, Lead, Message, OWNER_AUTHOR, QUESTION, Role, SYSTEM_AUTHOR, Tone, age,
    is_ack, owner_mentions, tone_of,
};

/// A bubble holds at most this many columns of text, however wide the pane: lines much
/// longer get hard to read (UI v0.1.13).
pub const TEXT_MAX: usize = 100;
/// The share of the chat's width a bubble may take, in percent (UI v0.1.13).
pub const BUBBLE_SHARE: usize = 85;
/// The narrowest a bubble gets, so a one-word message still reads as a bubble.
pub const BUBBLE_MIN: usize = 24;
/// Tower's lines wrap at this many columns, as the copy's centred events do.
pub const EVENT_MAX: usize = 80;
/// The widest an age gets: "999d".
const AGE_COLUMNS: usize = 4;
/// What a nudge record starts with (`watch`): "nudged dev-1: ...".
const NUDGED: &str = "nudged ";

/// Whether `author` is the owner, who posts only from the dashboard.
pub fn is_owner(author: &str) -> bool {
    author.eq_ignore_ascii_case(OWNER_AUTHOR)
}

fn is_system(m: &Message) -> bool {
    m.author.eq_ignore_ascii_case(SYSTEM_AUTHOR)
}

/// The worker a nudge record is about: tower's "nudged dev-1: ..." (`watch`).
fn nudged(m: &Message) -> Option<&str> {
    let rest = m.text.strip_prefix(NUDGED).filter(|_| is_system(m))?;
    rest.split_once(':').map(|(who, _)| who)
}

fn is_failure(m: &Message) -> bool {
    is_system(m) && m.text.starts_with(DELIVERY_FAILURE)
}

/// The watch's flag that a task's worker left herdr (`watch`): its line says so in a few
/// words, and its details hold how to clear it.
fn is_orphan(m: &Message) -> bool {
    // On its first line, as `event_text` reads it.
    is_system(m) && m.text.lines().next().is_some_and(|l| l.contains(ORPHANED))
}

/// What the watch's orphan flag says, after the task and before the worker.
const ORPHANED: &str = " is in progress but its worker ";

/// Who posts on a board: its lead, and the leads of the other repos (each with its repo's
/// name), who may post there lead to lead (spec 3a). A lead's name takes the accent and
/// the role word "lead", or "<repo> lead" for another repo's.
#[derive(Debug, Clone, Copy, Default)]
pub struct People<'a> {
    pub lead: Option<&'a Lead>,
    pub others: &'a [(String, &'a Lead)],
}

impl People<'_> {
    /// The other repo whose lead `author` is, by its session or name.
    fn other_repo(&self, author: &str) -> Option<&str> {
        (self.others.iter())
            .find(|(_, l)| {
                author.eq_ignore_ascii_case(&l.session) || author.eq_ignore_ascii_case(&l.name)
            })
            .map(|(repo, _)| repo.as_str())
    }

    /// The role word beside a name: "developer", "tester", "reviewer", "lead", or
    /// "<repo> lead" for another repo's lead; none for the owner, tower and anyone else.
    fn role_word(&self, author: &str) -> Option<String> {
        if is_owner(author) {
            return None;
        }
        let word = match Role::of(author, self.lead) {
            Role::Lead => "lead",
            Role::Dev => "developer",
            Role::Qa => "tester",
            Role::Senior => "reviewer",
            Role::System => return None,
            Role::Other => return self.other_repo(author).map(|r| format!("{r} lead")),
        };
        Some(word.into())
    }

    /// An author's colour in the chat: the owner red, any lead the accent, everyone else
    /// by role.
    pub fn colour(&self, author: &str, depth: Depth) -> Color {
        if is_owner(author) {
            bad(depth)
        } else if self.other_repo(author).is_some() {
            accent(depth)
        } else {
            role_colour(author, self.lead, depth)
        }
    }
}

/// What one thread's messages are in the chat, in order; each `usize` is a message's index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// How long ago the messages after it were: "now", "within the hour", "3h ago".
    Hour(String),
    /// One person's run: its messages, then the acks that answer it.
    Bubble { segs: Vec<usize>, acks: Vec<usize> },
    /// Consecutive nudges of one worker.
    Nudged { who: String, times: usize },
    /// A delivery failure, or the watch's flag that a worker left herdr: a line, wrapped
    /// onto more in a chat narrower than it, whose details open its full text.
    Failure(usize),
    /// Any other message of tower's.
    Event(usize),
}

/// The chat's items for `msgs`, with an `Hour` separator wherever how long ago a message
/// was (`ago_bucket`, at `now`) changes, when given a `now`. A run ends at a separator, at
/// one of tower's messages, at another author and after an ack; an ack with no bubble
/// before it is a bubble of its own.
/// Only the messages from index `skip` on make items; indexes still count from the first.
pub fn items(msgs: &[Message], now: Option<i64>, skip: usize) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut hour: Option<String> = None;
    // Whether the last item can still take this message: a bubble with no separator or
    // event after it.
    let mut open = false;
    for (i, m) in msgs.iter().enumerate().skip(skip) {
        if let Some(now) = now {
            let bucket = ago_bucket(now.saturating_sub(m.at));
            if hour.as_ref() != Some(&bucket) {
                out.push(Item::Hour(bucket.clone()));
                hour = Some(bucket);
                open = false;
            }
        }
        if is_system(m) {
            open = false;
            if let Some(who) = nudged(m) {
                if let Some(Item::Nudged { who: last, times }) = out.last_mut()
                    && last.as_str() == who
                {
                    *times += 1;
                    continue;
                }
                out.push(Item::Nudged {
                    who: who.to_string(),
                    times: 1,
                });
            } else if is_failure(m) || is_orphan(m) {
                out.push(Item::Failure(i));
            } else {
                out.push(Item::Event(i));
            }
            continue;
        }
        if open && let Some(Item::Bubble { segs, acks }) = out.last_mut() {
            if is_ack(m) {
                acks.push(i);
                continue;
            }
            if acks.is_empty() && msgs[segs[0]].author.eq_ignore_ascii_case(&m.author) {
                segs.push(i);
                continue;
            }
        }
        out.push(Item::Bubble {
            segs: vec![i],
            acks: Vec::new(),
        });
        open = true;
    }
    out
}

/// What a chat shows: how many lines of a long message (`clip`), which messages are open
/// in full (by id), its acks' ages and separators by how long ago as of `now`, who posts
/// (for role words), and for a preview the last `bubbles` bubbles, each with its last
/// `segs` messages, and no separators.
#[derive(Debug, Clone, Copy)]
pub struct Shape<'a> {
    pub clip: usize,
    pub open: &'a BTreeSet<i64>,
    pub now: i64,
    pub preview: Option<(usize, usize)>,
    pub people: People<'a>,
    /// Bubbles may take the chat's whole width, less a column at each edge: the full board
    /// in a narrow pane, with the thread list above it (UI v0.1.13).
    pub full: bool,
}

/// One line of the chat, unstyled; each `msg` is a message's index in the thread, each
/// `bw` its bubble's width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Blank,
    /// "… N earlier not loaded yet" in the full board, "N earlier messages" in a preview.
    Earlier {
        n: usize,
        loaded: bool,
    },
    Hour(String),
    /// The name above a bubble, with its role word and its first message's tags and age.
    Name {
        msg: usize,
        bw: usize,
    },
    /// A later message's tags and age in a run, on a dim line of their own above its text
    /// (it replaces the dashed rule between the messages).
    Meta {
        msg: usize,
        bw: usize,
    },
    /// A piece of a message's text, the owner's mentions in it marked, `head` on its first
    /// piece; `link` the URL it is part of, when it is one.
    Text {
        msg: usize,
        bw: usize,
        piece: String,
        marks: Vec<Range<usize>>,
        head: bool,
        link: Option<String>,
    },
    /// "read more" under a clipped message, or "show less" under an open one.
    More {
        msg: usize,
        bw: usize,
        open: bool,
    },
    /// A line of an ack under the bubble it answers, on that bubble's side; the first
    /// starts "↳ author:", the last ends with its age, which its `piece` leaves out: "the
    /// thing now · ", "· " after a piece too wide for a line, or empty when the age is
    /// alone after a line ending " ·".
    Ack {
        msg: usize,
        bw: usize,
        owner: bool,
        piece: String,
        first: bool,
        last: bool,
    },
    Nudged {
        who: String,
        times: usize,
    },
    /// A line of one of tower's messages, centred; its tags on the first.
    Event {
        msg: usize,
        piece: String,
        marks: Vec<Range<usize>>,
        first: bool,
    },
    /// A line of "delivery failed to X · details" or "dev-1 left herdr while on #1 ·
    /// details", its link "hide details" while its full text shows under it, wrapped as the
    /// copy's centred event box wraps it (`failure_pieces`): `said`, the piece of what it
    /// says on this line, and `link`, the piece of its link; `first` on its first line.
    Failure {
        msg: usize,
        said: String,
        link: String,
        first: bool,
    },
    /// A line of an open `Failure`'s full text, from column `left`: the left edge of the
    /// copy's centred event box, which fits the failure's line and its text (`detail_box`).
    Detail {
        piece: String,
        left: usize,
    },
}

impl Row {
    /// The message whose rows this starts in the full board's keys: a bubble's name or a
    /// later message's tags line, or a delivery failure's first line.
    pub fn starts(&self) -> Option<usize> {
        match self {
            Row::Name { msg, .. } | Row::Meta { msg, .. } => Some(*msg),
            Row::Failure {
                msg, first: true, ..
            } => Some(*msg),
            _ => None,
        }
    }

    /// The message a click on this line opens or folds: "read more", "show less" and a
    /// delivery failure's "details", on any line of the failure's.
    pub fn toggles(&self) -> Option<usize> {
        match self {
            Row::More { msg, .. } | Row::Failure { msg, .. } => Some(*msg),
            _ => None,
        }
    }

    /// The message this line belongs to, if any, for the selected message's outline.
    fn message(&self) -> Option<usize> {
        match self {
            Row::Meta { msg, .. } | Row::Text { msg, .. } | Row::More { msg, .. } => Some(*msg),
            Row::Failure { msg, .. } => Some(*msg),
            _ => None,
        }
    }
}

/// The widest a bubble gets in a chat `w` wide (UI v0.1.13): `BUBBLE_SHARE` percent
/// of it, rounded down, with at most `TEXT_MAX` columns of text (`inner`), and one column in
/// from each edge.
fn widest(w: usize) -> usize {
    (w * BUBBLE_SHARE / 100)
        .min(TEXT_MAX + 3)
        .min(w.saturating_sub(2))
        .max(1)
}

/// The widest a bubble gets in a chat `w` wide (`widest`), or with `full` (the full board
/// in a narrow pane) the whole chat less a column at each edge.
fn widest_in(w: usize, full: bool) -> usize {
    if full {
        w.saturating_sub(2).max(1)
    } else {
        widest(w)
    }
}

/// The first column of a bubble `bw` wide in a chat `w` wide: the owner's at the right,
/// everyone else's at the left, one column in from the edge.
fn left(w: usize, owner: bool, bw: usize) -> usize {
    if owner { w.saturating_sub(1 + bw) } else { 1 }
}

/// The text columns inside a bubble `bw` wide: its edge and a space each side.
fn inner(bw: usize) -> usize {
    bw.saturating_sub(3).max(1)
}

/// The URLs in `line`, as byte ranges, as the copy finds them: `http://` or
/// `https://` and what follows up to a space or one of `<>"')]`, less trailing `.,;:!?`.
pub(crate) fn urls(line: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = (line[from..].find("http")).map(|i| from + i) {
        let rest = &line[i..];
        let Some(scheme) = ["https://", "http://"]
            .into_iter()
            .find(|s| rest.starts_with(s))
        else {
            from = i + 4;
            continue;
        };
        let stop = |c: char| c.is_whitespace() || "<>\"')]".contains(c);
        let mut end = rest.find(stop).map_or(line.len(), |j| i + j);
        while end > i + scheme.len() && line[..end].ends_with(|c: char| ".,;:!?".contains(c)) {
            end -= 1;
        }
        // At least two characters after the scheme, as the copy's pattern has.
        if line[i + scheme.len()..end].chars().nth(1).is_some() {
            out.push(i..end);
        }
        from = end.max(i + scheme.len());
    }
    out
}

/// One line of a message's text, wrapped to `w` for a bubble: each URL starts on a line of
/// its own, whole, and one wider than `w` breaks only after `/`, `?`, `&` or `-`
/// (`pack_url`), each of its pieces with the URL; the text after a URL starts a line of its
/// own too; a path in the text breaks after a `/` and where Chrome breaks the copy's, such
/// as after a `-`, and other text where Chrome breaks it, such as between two CJK characters; a word with no break of
/// its own too wide for a line starts on a line of its own (`wrap_line_paths`). The line's
/// leading spaces stay on its first line of text and its runs of spaces between words stay
/// as written; a URL's line starts at the text's column, as the copy's block URL does.
/// Spaces are all that is added, and what `dropped` names all that is dropped, so `marked`
/// can find the owner's mentions in the pieces.
pub(crate) fn wrap_message_line(raw: &str, w: usize) -> Vec<(String, Option<String>)> {
    let found = urls(raw);
    if found.is_empty() {
        return (wrap_line_paths(raw, w).into_iter())
            .map(|p| (p, None))
            .collect();
    }
    let mut out = Vec::new();
    let text = |out: &mut Vec<(String, Option<String>)>, t: &str| {
        // Spaces only: `marked` counts every other character.
        if !t.trim_start_matches(' ').is_empty() {
            out.extend(wrap_line_paths(t, w).into_iter().map(|p| (p, None)));
        }
    };
    let mut at = 0;
    for r in found {
        // Only the space next to a URL goes: the line's indent before its first stays.
        let mut before = raw[at..r.start].trim_end_matches(' ');
        if at > 0 {
            before = before.trim_start_matches(' ');
        }
        text(&mut out, before);
        let url = &raw[r.clone()];
        out.extend((pack_url(url, w).into_iter()).map(|p| (p, Some(url.to_string()))));
        at = r.end;
    }
    text(&mut out, raw[at..].trim_start_matches(' '));
    out
}

/// `url` on lines at most `w` wide, breaking only after `/`, `?`, `&` or `-`, the lines
/// after the first indented two columns; a part between two of those wider than a line
/// breaks where the line ends.
fn pack_url(url: &str, w: usize) -> Vec<String> {
    let w = w.max(3);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let room = |out: &Vec<String>| if out.is_empty() { w } else { w - 2 };
    let push = |out: &mut Vec<String>, line: &mut String| {
        let indent = if out.is_empty() { "" } else { "  " };
        out.push(format!("{indent}{}", std::mem::take(line)));
    };
    for unit in units(url, &['/', '?', '&', '-']) {
        if !line.is_empty() && line.width() + unit.width() > room(&out) {
            push(&mut out, &mut line);
        }
        for c in unit.chars() {
            if !line.is_empty() && line.width() + c.width().unwrap_or(0) > room(&out) {
                push(&mut out, &mut line);
            }
            line.push(c);
        }
    }
    if !line.is_empty() {
        push(&mut out, &mut line);
    }
    out
}

/// The width a message's `text` asks of its bubble, as the copy's `.msg` fits its content:
/// its widest line laid out with no width limit, a URL still a block of its own, a line a
/// clip hides included. Past the widest a bubble gets, the text wraps and the bubble takes
/// that widest whatever its widest wrapped line.
fn max_content(text: &str) -> usize {
    (message_pieces(text, text.width() + 1).iter())
        .map(|p| p.text.width())
        .max()
        .unwrap_or(0)
}

/// A piece of a message's text as a bubble shows it: the line, the byte ranges in it of
/// the owner's mentions, and the URL it is part of, when it is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub text: String,
    pub marks: Vec<Range<usize>>,
    pub link: Option<String>,
}

/// A message's `text` in pieces at most `w` wide for a bubble (`wrap_message_line`), each
/// with the owner's mentions marked as `board::owner_mentions` reads the whole text.
pub(crate) fn message_pieces(text: &str, w: usize) -> Vec<Piece> {
    let marks = owner_mentions(text);
    let mut out = Vec::new();
    let mut base = 0;
    for raw in text.split('\n') {
        let lines = wrap_message_line(raw, w);
        let found = marked(raw, base, &marks, (lines.iter()).map(|(p, _)| p.as_str()));
        for ((text, link), marks) in lines.into_iter().zip(found) {
            out.push(Piece { text, marks, link });
        }
        base += raw.len() + 1;
    }
    out
}

/// The owner's mentions `marks` (byte ranges of the whole text, its line `raw` starting at
/// `base`) in each of `lines`, `raw` wrapped: wrapping adds spaces or drops them (`dropped`)
/// only, so the lines' other characters are `raw`'s, in order.
pub(crate) fn marked<'a>(
    raw: &str,
    base: usize,
    marks: &[Range<usize>],
    lines: impl Iterator<Item = &'a str>,
) -> Vec<Vec<Range<usize>>> {
    let mut out = Vec::new();
    let mut chars = raw.char_indices().filter(|(_, c)| !dropped(*c));
    for piece in lines {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (pi, pc) in piece.char_indices() {
            if dropped(pc) {
                continue;
            }
            let Some((ri, _)) = chars.next() else { break };
            if !marks.iter().any(|r| r.contains(&(base + ri))) {
                continue;
            }
            let r = pi..pi + pc.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                _ => ranges.push(r),
            }
        }
        out.push(ranges);
    }
    out
}

/// `text` in pieces, its first at most `first` columns wide and the rest at most `w` (as
/// `wrap_event_line` breaks them, their spaces folded), each with the byte ranges in it of the
/// owner's mentions, as `board::owner_mentions` reads the whole text.
pub(crate) fn pieces(text: &str, first: usize, w: usize) -> Vec<(String, Vec<Range<usize>>)> {
    let marks = owner_mentions(text);
    let mut out = Vec::new();
    let mut base = 0;
    for raw in text.split('\n') {
        let mut lines = Vec::new();
        if out.is_empty() && lines.is_empty() && first < w {
            // The first piece at its own width, then the rest of that line at `w`.
            let mut head = wrap_event_line(raw, first);
            let rest = rest_after(raw, &head[..1], head.get(1).map_or("", String::as_str));
            lines.push(head.remove(0));
            if !rest.is_empty() {
                lines.extend(wrap_event_line(rest, w));
            }
        } else {
            lines = wrap_event_line(raw, w);
        }
        let found = marked(raw, base, &marks, lines.iter().map(String::as_str));
        out.extend(lines.into_iter().zip(found));
        base += raw.len() + 1;
    }
    out
}

/// The chat's rows `w` wide for `msgs`, of which `missing` earlier ones are not loaded.
/// A preview starts with how many messages come before the first it shows, never starts
/// on an ack whose message is not loaded, and shows tower's last lines when its messages
/// hold no bubble.
pub fn plan(msgs: &[Message], missing: usize, w: usize, shape: Shape) -> Vec<Row> {
    // Acks first in a preview's window that starts after the thread's start answer messages
    // not loaded: they are earlier ones, unless nothing but acks is loaded.
    let leading = msgs.iter().take_while(|m| is_ack(m)).count();
    let skip = match shape.preview {
        Some(_) if missing > 0 && leading < msgs.len() => leading,
        _ => 0,
    };
    let mut all = items(msgs, shape.preview.is_none().then_some(shape.now), skip);
    if let Some((bubbles, segs)) = shape.preview {
        if all.iter().any(|it| matches!(it, Item::Bubble { .. })) {
            all.retain(|it| matches!(it, Item::Bubble { .. }));
            all.drain(..all.len().saturating_sub(bubbles));
            for it in &mut all {
                if let Item::Bubble { segs: s, .. } = it {
                    s.drain(..s.len().saturating_sub(segs));
                }
            }
        } else {
            all.drain(..all.len().saturating_sub(segs));
        }
    }
    let mut rows = Vec::new();
    let first = match shape.preview {
        Some(_) => all.iter().find_map(|it| match it {
            Item::Bubble { segs, .. } => segs.first().copied(),
            Item::Failure(i) | Item::Event(i) => Some(*i),
            _ => None,
        }),
        None => Some(0),
    };
    let earlier = missing + first.unwrap_or(msgs.len());
    if earlier > 0 {
        rows.push(Row::Earlier {
            n: earlier,
            loaded: shape.preview.is_some(),
        });
    }
    // A preview's first bubble keeps a line between it and the thread's head.
    if shape.preview.is_some() && rows.is_empty() && !all.is_empty() {
        rows.push(Row::Blank);
    }
    let people = shape.people;
    let ew = w.saturating_sub(4).clamp(1, EVENT_MAX);
    for it in all {
        if rows.last().is_some_and(|r| *r != Row::Blank) {
            rows.push(Row::Blank);
        }
        match it {
            Item::Hour(label) => rows.push(Row::Hour(label)),
            Item::Nudged { who, times } => rows.push(Row::Nudged { who, times }),
            Item::Event(msg) => {
                let m = &msgs[msg];
                let text = event_text(m, people.lead);
                let tag_w: usize = tags(m, Depth::Ansi256).iter().map(Span::width).sum();
                for (j, (piece, marks)) in pieces(&text, ew.saturating_sub(tag_w).max(1), ew)
                    .into_iter()
                    .enumerate()
                {
                    rows.push(Row::Event {
                        msg,
                        piece,
                        marks,
                        first: j == 0,
                    });
                }
            }
            Item::Failure(msg) => {
                let open = shape.open.contains(&msgs[msg].id);
                let (said, link) = failure_line(&msgs[msg], people.lead, open);
                for (j, (said, link)) in failure_pieces(&said, link, ew).into_iter().enumerate() {
                    rows.push(Row::Failure {
                        msg,
                        said,
                        link,
                        first: j == 0,
                    });
                }
                if open {
                    let text = &msgs[msg].text;
                    // The line as drawn, its spaces folded: what it says, a space, its link.
                    let line_w = fold_spaces(&said).width() + 1 + link.width();
                    let bw = detail_box(text, line_w, ew);
                    let left = w.saturating_sub(bw) / 2;
                    (wrap(text, ew).into_iter())
                        .for_each(|piece| rows.push(Row::Detail { piece, left }));
                }
            }
            Item::Bubble { segs, acks } => {
                let owner = is_owner(&msgs[segs[0]].author);
                let widest = |w| widest_in(w, shape.full);
                let most = inner(widest(w));
                // Each message's pieces at the widest, folded unless open; the bubble then
                // fits each message's text unwrapped (`max_content`) and its name line.
                let mut texts = Vec::new();
                // The name line holds the first message's tags and age; every text line
                // takes the bubble's full width.
                let first = &msgs[segs[0]];
                let mut need =
                    (name_columns(first, people) + meta_columns(first)).saturating_sub(3);
                for (k, &msg) in segs.iter().enumerate() {
                    let m = &msgs[msg];
                    let mut ps = message_pieces(&m.text, most);
                    let long = ps.len() > shape.clip + 1;
                    let open = shape.open.contains(&m.id);
                    if long && !open {
                        ps.truncate(shape.clip.max(1));
                    }
                    need = need.max(max_content(&m.text));
                    if k > 0 {
                        need = need.max(meta_columns(m).saturating_sub(3));
                    }
                    if long {
                        need = need.max("show less".len());
                    }
                    texts.push((msg, ps, long, open));
                }
                // Its acks widen it too, as the copy's message fits all it holds: two in,
                // the ack, " · " and the age, at most the widest bubble.
                for &msg in &acks {
                    let m = &msgs[msg];
                    let text = ack_text(m);
                    need = need.max((text.width() + 2 + 3 + AGE_COLUMNS).saturating_sub(3));
                }
                let bw = (need + 3).clamp(BUBBLE_MIN.min(widest(w)), widest(w));
                for (k, (msg, ps, long, open)) in texts.into_iter().enumerate() {
                    rows.push(if k == 0 {
                        Row::Name { msg, bw }
                    } else {
                        Row::Meta { msg, bw }
                    });
                    for (j, p) in ps.into_iter().enumerate() {
                        rows.push(Row::Text {
                            msg,
                            bw,
                            piece: p.text,
                            marks: p.marks,
                            head: j == 0,
                            link: p.link,
                        });
                    }
                    if long {
                        rows.push(Row::More { msg, bw, open });
                    }
                }
                for msg in acks {
                    let m = &msgs[msg];
                    // The ack, " · " and its age as one run of words, as the copy lays its
                    // `· age` out inline: each line filled to the ack's width, two in from
                    // the bubble's edge, the age after the last word where it fits, else
                    // `·` ends that line and the age starts the next, or after a piece too
                    // wide for a line `· age` starts the next. The age as drawn at `now`.
                    let shown = age(m.at, shape.now);
                    let aw = bw.saturating_sub(2).max(shown.width());
                    let mut ps = wrap_event_line(&format!("{} · {shown}", ack_text(m)), aw);
                    // The age, a word no wider than a line, ends the last line; `line` draws
                    // it as of its own `now`.
                    let last = ps.last_mut().expect("wrapping leaves a line");
                    let kept = (last.strip_suffix(shown.as_str()))
                        .expect("an ack's age ends its last line")
                        .len();
                    last.truncate(kept);
                    let n = ps.len();
                    for (j, piece) in ps.into_iter().enumerate() {
                        rows.push(Row::Ack {
                            msg,
                            bw,
                            owner,
                            piece,
                            first: j == 0,
                            last: j + 1 == n,
                        });
                    }
                }
            }
        }
    }
    rows
}

/// An ack as its reply line says it, before its age: "↳ qa: copy, testing".
fn ack_text(m: &Message) -> String {
    format!("↳ {}: {}", m.author, m.text.trim().replace('\n', " "))
}

/// A message's tags: "needs you" for a question to the owner (`Tone::NeedsYou`), then
/// "decision" or "blocked" for a message that starts so (in any case).
fn tag_words(m: &Message) -> Vec<&'static str> {
    let starts = |p: &str| {
        m.text
            .get(..p.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(p))
    };
    let mut out = Vec::new();
    if tone_of(m) == Tone::NeedsYou {
        out.push("needs you");
    }
    if starts("decision:") {
        out.push("decision");
    }
    if starts("blocked:") {
        out.push("blocked");
    }
    out
}

/// A tag in its colour: "needs you" red, "decision" blue, "blocked" amber.
fn tag(word: &'static str, depth: Depth) -> Span<'static> {
    let colour = match word {
        "needs you" => bad(depth),
        "decision" => info(depth),
        _ => warn(depth),
    };
    Span::styled(word, bold().fg(colour))
}

/// The tags in their colours, each followed by a space.
fn tags(m: &Message, depth: Depth) -> Vec<Span<'static>> {
    (tag_words(m).into_iter())
        .flat_map(|word| [tag(word, depth), Span::raw(" ")])
        .collect()
}

/// The columns a message's tags and age take on its name line: " · ", the tags
/// a space apart and " · " after them, then the age, at most `AGE_COLUMNS`. A later
/// message's line of its own takes three fewer, having no " · " before its first.
fn meta_columns(m: &Message) -> usize {
    let words = tag_words(m);
    let tags = (words.iter()).map(|t| t.len() + 1).sum::<usize>();
    // The tags' last space is the " · " after them, less two.
    3 + if tags == 0 { 0 } else { tags + 2 } + AGE_COLUMNS
}

/// A message's tags in their colours, a space apart, and its age, each part after a dim
/// " · ", as the copy's name line shows them.
fn meta(m: &Message, now: i64, depth: Depth) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (k, word) in tag_words(m).into_iter().enumerate() {
        out.push(Span::styled(if k == 0 { " · " } else { " " }, dim(depth)));
        out.push(tag(word, depth));
    }
    out.push(Span::styled(" · ", dim(depth)));
    out.push(Span::styled(age(m.at, now), dim(depth)));
    out
}

/// The name a bubble shows: "you (manager)" for the owner, else the author.
fn shown_name(m: &Message) -> String {
    if is_owner(&m.author) {
        "you (manager)".into()
    } else {
        m.author.clone()
    }
}

/// The role word beside a name, with its separator: " · developer".
fn role_suffix(m: &Message, people: People) -> Option<String> {
    people.role_word(&m.author).map(|w| format!(" · {w}"))
}

/// The columns a bubble's name line needs: one in, its name and role word, one before the
/// bubble's edge.
fn name_columns(m: &Message, people: People) -> usize {
    let role = role_suffix(m, people).map_or(0, |r| r.width());
    1 + shown_name(m).width() + role + 1
}

/// A separator's label: how long ago a message `secs` old was, as UI v0.1.7 buckets it.
pub fn ago_bucket(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".into(),
        s if s < 3600 => "within the hour".into(),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 2 * 86_400 => "yesterday".into(),
        s => format!("{} days ago", s / 86_400),
    }
}

/// Tower's message in a chat's words, whole: the watch's flags about a worker as what
/// happened to it (the texts `watch` posts), a message to the lead as "to <lead>: ...",
/// and a question for the owner without its "question:" (its "needs you" tag says so).
pub fn event_text(m: &Message, lead: Option<&Lead>) -> String {
    let text = m.text.as_str();
    let text = match text.strip_prefix(QUESTION) {
        Some(rest) if tone_of(m) == Tone::NeedsYou => rest.trim_start(),
        _ => text,
    };
    event_words(text, lead)
}

/// Tower's `text` in a chat's words, whole, as `event_text` says it but keeping a
/// "question:", for a line with no "needs you" tag to say it.
pub fn event_words(text: &str, lead: Option<&Lead>) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let then = |line: String| {
        if rest.is_empty() {
            line
        } else {
            format!("{line}\n{rest}")
        }
    };
    if let Some((before, after)) = first.split_once(ORPHANED)
        && let (Some(task), Some(worker)) = (before.rsplit(' ').next(), after.split(' ').next())
    {
        return then(format!("{worker} left herdr while on {task}"));
    }
    if let Some((before, after)) = first.split_once(" has not posted since it was nudged ") {
        let mut words = before.rsplit(' ');
        if let (Some(task), Some("on"), Some(worker)) = (words.next(), words.next(), words.next()) {
            let when = after
                .split(" (")
                .next()
                .unwrap_or(after)
                .trim_end_matches('.');
            return then(format!(
                "{worker} has been silent on {task} since its nudge {when}"
            ));
        }
    }
    if let Some(l) = lead {
        for handle in [&l.session, &l.name] {
            if let Some(rest) = first.strip_prefix(&format!("@{handle} ")) {
                return then(format!("to {}: {rest}", l.name));
            }
        }
    }
    text.to_string()
}

/// A delivery failure's or the left-herdr flag's line, in its two parts: what it says with
/// " · " after it ("delivery failed to X · "), and its link, "details" or "hide details"
/// when `open`.
fn failure_line(m: &Message, lead: Option<&Lead>, open: bool) -> (String, &'static str) {
    let said = if is_failure(m) {
        format!("delivery failed to {}", failed_to(&m.text))
    } else {
        let text = event_text(m, lead);
        text.lines().next().unwrap_or_default().to_string()
    };
    let link = if open { "hide details" } else { "details" };
    (format!("{said} · "), link)
}

/// A failure's line (`failure_line`) in pieces at most `w` wide, as the copy's centred event
/// box wraps it: each a piece of what it `said` and a piece of its `link`, either empty. Its
/// words break as an event's do (`wrap_event_line`); the link, an `inline-block` there, ends the
/// last of them when it fits and otherwise starts a line of its own, breaking inside only
/// when wider than a line.
fn failure_pieces(said: &str, link: &str, w: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = (wrap_event_line(said, w).into_iter())
        .map(|p| (p, String::new()))
        .collect();
    match out.last_mut() {
        Some((last, room)) if last.width() + 1 + link.width() <= w => *room = link.to_string(),
        _ => out.extend(
            wrap_event_line(link, w)
                .into_iter()
                .map(|p| (String::new(), p)),
        ),
    }
    out
}

/// The width of the copy's event box (`.evt`) around a failure's line `line_w` wide and
/// its open `text`: it fits the widest of them, each line of the text with all its spaces,
/// as the copy's `pre-wrap` details size the box (trailing ones too, though a drawn line
/// drops them), at most `ew`, the events' width, past which the text wraps.
fn detail_box(text: &str, line_w: usize, ew: usize) -> usize {
    (text.split('\n'))
        .map(UnicodeWidthStr::width)
        .fold(line_w, usize::max)
        .min(ew)
}

/// Who a delivery failure is about: "Kids Lead", "dev-1" or "3 recipients".
pub(crate) fn failed_to(text: &str) -> &str {
    let rest = text.strip_prefix(DELIVERY_FAILURE).unwrap_or(text);
    let end = [", the lead", ";", ":", ". "]
        .iter()
        .filter_map(|s| rest.find(s))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// A bubble's background: the owner's a shade lighter, a question to the owner tinted red.
/// On the 256-colour palette the nearest greys, and the question only by its red edge.
fn fill(owner: bool, needs: bool, depth: Depth) -> Style {
    let c = match (depth, needs, owner) {
        (Depth::TrueColor, true, _) => Color::Rgb(0x2a, 0x19, 0x1d),
        (Depth::TrueColor, false, true) => Color::Rgb(0x22, 0x26, 0x32),
        (Depth::TrueColor, false, false) => Color::Rgb(0x1b, 0x20, 0x29),
        (Depth::Ansi256, _, true) => Color::Indexed(235),
        (Depth::Ansi256, _, false) => Color::Indexed(234),
    };
    Style::new().bg(c)
}

/// What drawing a chat needs besides its rows: the messages, who posts (for colours and
/// role words), the time and the message selected in the full board, by index.
pub struct Ctx<'a> {
    pub msgs: &'a [Message],
    pub people: People<'a>,
    pub now: i64,
    pub depth: Depth,
    pub selected: Option<usize>,
}

/// `spans`, a line of text, without the run of spaces and ideographic spaces (U+3000) ending
/// it (`dropped`): the copy hangs such a run past a line's end, an ideographic space whether
/// it fits or not, so it is blank and never counts toward where a centred or right-aligned
/// line sits.
fn unhung(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    while let Some(last) = spans.last_mut() {
        let kept = last.content.trim_end_matches(dropped);
        if kept.len() < last.content.len() {
            last.content = kept.to_string().into();
        }
        if !last.content.is_empty() {
            break;
        }
        spans.pop();
    }
    spans
}

/// `spans` centred in `w` columns, cut to fit, the run of spaces ending them left out
/// (`unhung`).
fn centred(spans: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let spans = unhung(spans);
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = w.saturating_sub(used) / 2;
    let mut s = vec![Span::raw(" ".repeat(pad))];
    s.extend(spans);
    fit(s, w)
}

/// A line of a bubble: its edge in `edge`, `content` cut to the bubble's text columns,
/// all on the bubble's `bg`.
fn bubble_line(
    w: usize,
    owner: bool,
    bw: usize,
    edge: Style,
    bg: Style,
    content: Vec<Span<'static>>,
) -> Line<'static> {
    let x = left(w, owner, bw);
    let inside: Vec<Span<'static>> = (fit(content, inner(bw)).spans.into_iter())
        .map(|s| Span::styled(s.content, bg.patch(s.style)))
        .collect();
    let mut s = vec![Span::raw(" ".repeat(x))];
    let mark = Span::styled(if owner { "▐" } else { "▌" }, bg.patch(edge));
    if owner {
        s.push(Span::styled(" ", bg));
        s.extend(inside);
        s.push(Span::styled(" ", bg));
        s.push(mark);
    } else {
        s.push(mark);
        s.push(Span::styled(" ", bg));
        s.extend(inside);
        s.push(Span::styled(" ", bg));
    }
    fit(s, w)
}

/// The URL `row` shows a piece of, if any: `link::placed` links the piece's cells.
pub fn row_link(row: &Row) -> Option<String> {
    match row {
        Row::Text { link, .. } => link.clone(),
        _ => None,
    }
}

/// The rows `w` wide, styled.
pub fn lines(rows: &[Row], w: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    rows.iter().map(|r| line(r, w, ctx)).collect()
}

/// One row `w` wide, styled.
pub fn line(row: &Row, w: usize, ctx: &Ctx) -> Line<'static> {
    let Ctx {
        msgs,
        people,
        now,
        depth,
        selected,
    } = *ctx;
    let selected_here = selected.is_some() && row.message() == selected;
    match row {
        Row::Blank => fit(Vec::new(), w),
        Row::Earlier { n, loaded } => {
            let text = match (loaded, n) {
                (true, 1) => "1 earlier message".to_string(),
                (true, n) => format!("{n} earlier messages"),
                (false, n) => format!("… {n} earlier not loaded yet"),
            };
            centred(vec![Span::styled(text, dim(depth))], w)
        }
        Row::Hour(label) => {
            let label = format!(" {label} ");
            let side = w.saturating_sub(label.width() + 2);
            let left = side / 2;
            let text = format!(" {}{label}{} ", "─".repeat(left), "─".repeat(side - left));
            fit(vec![Span::styled(text, dim(depth))], w)
        }
        Row::Name { msg, bw } => {
            let m = &msgs[*msg];
            let owner = is_owner(&m.author);
            let x = left(w, owner, *bw);
            let colour = Style::new().fg(people.colour(&m.author, depth));
            let mut name = vec![Span::styled(shown_name(m), bold().patch(colour))];
            // Then the first message's tags and age. The role word gives way before
            // them, and they before the name, which is never cut.
            let when = meta(m, now, depth);
            let when_w: usize = when.iter().map(Span::width).sum();
            if let Some(role) = role_suffix(m, people)
                && name[0].width() + role.width() + when_w + 2 <= *bw
            {
                name.push(Span::styled(role, dim(depth)));
            }
            name.extend(when);
            let name_w: usize = name.iter().map(Span::width).sum();
            let at = if owner {
                (x + bw).saturating_sub(name_w + 1)
            } else {
                x + 1
            };
            let mut s = vec![Span::raw(" ".repeat(at))];
            s.extend(name);
            fit(s, w)
        }
        Row::Meta { msg, bw } => {
            // A later message's tags and age, on a line of their own: at the left, or at the
            // right in the owner's bubbles.
            let m = &msgs[*msg];
            let owner = is_owner(&m.author);
            let (edge, bg) = edges(m, owner, selected_here, people, depth);
            let mut s: Vec<Span<'static>> = meta(m, now, depth).into_iter().skip(1).collect();
            if owner {
                let used: usize = s.iter().map(Span::width).sum();
                s.insert(0, Span::raw(" ".repeat(inner(*bw).saturating_sub(used))));
            }
            bubble_line(w, owner, *bw, edge, bg, s)
        }
        Row::Text {
            msg,
            bw,
            piece,
            marks,
            head,
            link,
        } => {
            let m = &msgs[*msg];
            let owner = is_owner(&m.author);
            let (edge, bg) = edges(m, owner, selected_here, people, depth);
            let s = if link.is_some() {
                // A URL's piece reads as a link: its text in the accent, underlined, after
                // its indent. `link::placed` makes it one OSC 8 link.
                let text = piece.trim_start();
                let indent = piece.len() - text.len();
                let style = Style::new()
                    .fg(accent(depth))
                    .add_modifier(Modifier::UNDERLINED);
                vec![
                    Span::raw(piece[..indent].to_string()),
                    Span::styled(text.to_string(), style),
                ]
            } else {
                // A leading tag toned on its first piece only as `may_tone_tag` says: the
                // piece's own start is not the message's once its indent is wrapped.
                let awareness = tone_of(m) == Tone::Awareness;
                let head = *head && may_tone_tag(&m.text);
                highlight_marked(piece, marks, head, awareness, depth)
            };
            bubble_line(w, owner, *bw, edge, bg, s)
        }
        Row::More { msg, bw, open } => {
            let m = &msgs[*msg];
            let owner = is_owner(&m.author);
            let (edge, bg) = edges(m, owner, selected_here, people, depth);
            let text = if *open { "show less" } else { "read more" };
            let s = vec![Span::styled(text, Style::new().fg(accent(depth)))];
            bubble_line(w, owner, *bw, edge, bg, s)
        }
        Row::Ack {
            msg,
            bw,
            owner,
            piece,
            first,
            last,
        } => {
            let m = &msgs[*msg];
            let colour = Style::new().fg(people.colour(&m.author, depth));
            let lead_in = format!("↳ {}", m.author);
            // Each line starts where the first does, as the copy's do.
            let mut s = match piece.strip_prefix(&lead_in).filter(|_| *first) {
                Some(rest) => vec![
                    Span::styled(lead_in.clone(), colour),
                    Span::styled(rest.to_string(), dim(depth)),
                ],
                None => vec![Span::styled(piece.clone(), dim(depth))],
            };
            if *last {
                s.push(Span::styled(age(m.at, now), dim(depth)));
            }
            // The run of spaces ending the line hangs past its end (`unhung`), so an owner's
            // ack, at the right, ends at the edge with the text before it.
            let mut s = unhung(s);
            let x = left(w, *owner, *bw);
            let used: usize = s.iter().map(Span::width).sum();
            let at = if *owner {
                (x + bw).saturating_sub(used + 1)
            } else {
                x + 2
            };
            s.insert(0, Span::raw(" ".repeat(at)));
            fit(s, w)
        }
        Row::Nudged { who, times } => {
            let text = match times {
                1 => format!("{who} was nudged"),
                n => format!("{who} was nudged {n} times"),
            };
            centred(vec![Span::styled(text, dim(depth))], w)
        }
        Row::Event {
            msg,
            piece,
            marks,
            first,
        } => {
            // Tower's question to the owner (a worker on a herdr prompt) keeps its tag.
            let m = &msgs[*msg];
            let mut spans = if *first { tags(m, depth) } else { Vec::new() };
            spans.extend(
                (highlight_marked(piece, marks, false, false, depth).into_iter())
                    .map(|s| Span::styled(s.content, dim(depth).patch(s.style))),
            );
            centred(spans, w)
        }
        Row::Failure { said, link, .. } => {
            let gap = if said.is_empty() || link.is_empty() {
                ""
            } else {
                " "
            };
            let spans = vec![
                Span::styled(format!("{said}{gap}"), dim(depth)),
                Span::styled(link.clone(), Style::new().fg(accent(depth))),
            ];
            let mut line = centred(spans, w);
            if selected_here {
                let bg = Style::new().bg(selected_bg(depth));
                for s in &mut line.spans {
                    s.style = bg.patch(s.style);
                }
            }
            line
        }
        Row::Detail { piece, left } => fit(
            vec![
                Span::raw(" ".repeat(*left)),
                Span::styled(piece.clone(), dim(depth)),
            ],
            w,
        ),
    }
}

/// A bubble line's edge and background: the edge in the author's colour, red on a question
/// to the owner (selected or not), and the accent on any other selected message, over the
/// selected background.
fn edges(m: &Message, owner: bool, selected: bool, people: People, depth: Depth) -> (Style, Style) {
    let needs = tone_of(m) == Tone::NeedsYou;
    // A question to the owner keeps its red edge when selected; the background says which.
    let colour = if needs {
        bad(depth)
    } else if selected {
        accent(depth)
    } else {
        people.colour(&m.author, depth)
    };
    let bg = if selected {
        Style::new().bg(selected_bg(depth))
    } else {
        fill(owner, needs, depth)
    };
    (Style::new().fg(colour), bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Messages (author, text, seconds since the epoch), ids from 1.
    fn msgs(all: &[(&str, &str, i64)]) -> Vec<Message> {
        (all.iter().enumerate())
            .map(|(i, (author, text, at))| Message {
                id: i as i64 + 1,
                author: (*author).into(),
                text: (*text).into(),
                at: *at,
            })
            .collect()
    }

    fn lead() -> Lead {
        Lead {
            name: "Kids Lead".into(),
            session: "kids-lead".into(),
            colour: None,
        }
    }

    fn shape<'a>(open: &'a BTreeSet<i64>, now: i64, people: People<'a>) -> Shape<'a> {
        Shape {
            clip: 4,
            open,
            now,
            preview: None,
            people,
            full: false,
        }
    }

    fn drawn(m: &[Message], w: usize, now: i64, people: People) -> Vec<String> {
        let none = BTreeSet::new();
        let rows = plan(m, 0, w, shape(&none, now, people));
        let ctx = Ctx {
            msgs: m,
            people,
            now,
            depth: Depth::Ansi256,
            selected: None,
        };
        lines(&rows, w, &ctx)
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn a_message_age_is_the_readable_dim_grey_at_both_depths() {
        let m = msgs(&[("dev-1", "the bar is drawn", 0)]);
        let none = BTreeSet::new();
        let rows = plan(&m, 0, 60, shape(&none, 120, People::default()));
        for (depth, grey) in [
            (Depth::TrueColor, Color::Rgb(0x6c, 0x72, 0x80)),
            (Depth::Ansi256, Color::Indexed(243)),
        ] {
            let ctx = Ctx {
                msgs: &m,
                people: People::default(),
                now: 120,
                depth,
                selected: None,
            };
            let drawn = lines(&rows, 60, &ctx);
            let age = (drawn.iter().flat_map(|l| l.spans.iter()))
                .find(|s| s.content.trim() == "2m")
                .unwrap_or_else(|| panic!("{drawn:#?}"));
            assert_eq!(age.style.fg, Some(grey), "{depth:?}");
        }
    }

    #[test]
    fn an_ack_is_short_starts_with_an_ack_word_and_never_mentions_the_owner() {
        let ack = |author: &str, text: &str| is_ack(&msgs(&[(author, text, 0)])[0]);
        for yes in [
            "copy",
            "Copy, thanks",
            "ack",
            "acknowledged",
            "copy, plan approved",
            "Plan approved.",
            "ok",
            "thanks!",
            "  copy, go  ",
        ] {
            assert!(ack("dev-1", yes), "{yes}");
        }
        for no in [
            "copying the files now",
            "okay then",
            "acked",
            "copy, @manager please look",
            "copy, @MANAGER",
            "copy, and here is a long report of what I did today",
            "the plan is approved",
        ] {
            assert!(!ack("dev-1", no), "{no}");
        }
        assert!(!ack("system", "ok"));
        // Exactly 40 characters is still an ack; 41 is not.
        let forty = format!("copy {}", "x".repeat(35));
        assert!(ack("qa", &forty));
        assert!(!ack("qa", &format!("{forty}x")));
    }

    #[test]
    fn a_run_is_one_bubble_until_another_author_an_ack_an_event_or_the_hour() {
        let h = 3600 * 100;
        let now = h + 3600 + 3900;
        let m = msgs(&[
            ("dev-one", "one", h),
            ("Dev-One", "two", h + 60),
            ("qa", "copy", h + 120),
            ("dev-one", "three", h + 180),
            ("system", "nudged dev-one: no post", h + 240),
            ("system", "nudged dev-one: still no post", h + 250),
            ("system", "nudged qa: no post", h + 260),
            ("dev-one", "four", h + 300),
            ("dev-one", "five", h + 3600),
            (
                "system",
                "@manager could not deliver to dev-1: gone",
                h + 3700,
            ),
            ("system", "dev-1's prompt was answered.", h + 3800),
            ("qa", "ok", h + 3900),
        ]);
        use Item::*;
        let bubble = |segs: Vec<usize>, acks: Vec<usize>| Bubble { segs, acks };
        assert_eq!(
            items(&m, Some(now), 0),
            vec![
                Hour("2h ago".into()),
                bubble(vec![0, 1], vec![2]),
                // After an ack the same author starts a new bubble.
                bubble(vec![3], vec![]),
                Nudged {
                    who: "dev-one".into(),
                    times: 2
                },
                Nudged {
                    who: "qa".into(),
                    times: 1
                },
                bubble(vec![7], vec![]),
                Hour("1h ago".into()),
                bubble(vec![8], vec![]),
                Failure(9),
                Event(10),
                // An ack with no bubble to answer is a bubble of its own.
                bubble(vec![11], vec![]),
            ]
        );
        // Without separators the run crosses the hour.
        assert!(items(&m, None, 0).contains(&bubble(vec![7, 8], vec![])));
    }

    #[test]
    fn tower_messages_read_in_a_chats_words() {
        let l = lead();
        let sys = |text: &str| msgs(&[("system", text, 0)]).remove(0);
        let gone = "@kids-lead #61 is in progress but its worker dev-7 is gone from herdr \
                    (no live agent).";
        assert_eq!(
            event_text(&sys(gone), Some(&l)),
            "dev-7 left herdr while on #61"
        );
        let silent = "@kids-lead dev-5 on #28 has not posted since it was nudged 1 min ago \
                      (it was nudged for no post).";
        assert_eq!(
            event_text(&sys(silent), Some(&l)),
            "dev-5 has been silent on #28 since its nudge 1 min ago"
        );
        // The whole text, its first line in a chat's words.
        assert_eq!(
            event_text(&sys("@Kids Lead dev-1 needs a hand\nmore"), Some(&l)),
            "to Kids Lead: dev-1 needs a hand\nmore"
        );
        assert_eq!(
            event_text(&sys("dev-1's prompt was answered."), Some(&l)),
            "dev-1's prompt was answered."
        );
        // A question for the owner drops "question:", which its tag says.
        assert_eq!(
            event_text(&sys("question: @manager dev-1 is waiting"), Some(&l)),
            "@manager dev-1 is waiting"
        );
        assert_eq!(
            failed_to(
                "@manager could not deliver to Kids Lead, the lead of M2: no pane (it attaches \
                 again with tower lead attach M2). Its posts stay on the board."
            ),
            "Kids Lead"
        );
        assert_eq!(
            failed_to("@manager could not deliver to dev-1: gone. Its posts stay on the board."),
            "dev-1"
        );
        assert_eq!(
            failed_to("@manager could not deliver to 3 recipients; their posts stay:\n- a"),
            "3 recipients"
        );
    }

    #[test]
    fn the_watchs_prompt_question_wraps_whole_with_its_tag_and_the_owner_marked() {
        // The watch's own text (watch.rs, `Signal::Prompt`), over 130 characters.
        let text = "question: @manager dev-1 on #1 is waiting on a prompt in herdr that only \
                    you can answer (@Catalogue and Search Lead for your information).";
        assert!(text.chars().count() > 130);
        let m = msgs(&[("system", text, 0)]);
        for w in [50, 79, 120] {
            let rows = drawn(&m, w, 30, People::default());
            let words: Vec<&str> = rows.iter().flat_map(|r| r.split_whitespace()).collect();
            let all = words.join(" ");
            assert!(
                all.contains("that only you can answer (@Catalogue"),
                "w={w}\n{all}"
            );
            assert!(all.ends_with("for your information)."), "w={w}\n{all}");
            assert!(!all.contains("question:"), "w={w}\n{all}");
            let first = rows.iter().find(|r| r.contains("needs you")).unwrap();
            assert!(first.contains("needs you @manager"), "w={w} {first:?}");
            for r in &rows {
                assert_eq!(r.chars().count(), w);
                // Centred, at most 80 columns of text (the separator runs across).
                assert!(
                    r.contains('─') || r.trim().chars().count() <= EVENT_MAX,
                    "{r:?}"
                );
            }
        }
        let none = BTreeSet::new();
        let rows = plan(&m, 0, 79, shape(&none, 30, People::default()));
        let ctx = Ctx {
            msgs: &m,
            people: People::default(),
            now: 30,
            depth: Depth::TrueColor,
            selected: None,
        };
        let red = (lines(&rows, 79, &ctx).iter().flat_map(|l| l.spans.clone()))
            .filter(|s| s.content == "@manager" && s.style.fg == Some(bad(Depth::TrueColor)))
            .count();
        assert_eq!(red, 1);
    }

    #[test]
    fn a_separator_says_how_long_ago_as_the_copy_buckets_it() {
        assert_eq!(ago_bucket(0), "now");
        assert_eq!(ago_bucket(59), "now");
        assert_eq!(ago_bucket(60), "within the hour");
        assert_eq!(ago_bucket(3599), "within the hour");
        assert_eq!(ago_bucket(3600), "1h ago");
        assert_eq!(ago_bucket(86_399), "23h ago");
        assert_eq!(ago_bucket(86_400), "yesterday");
        assert_eq!(ago_bucket(3 * 86_400 + 5), "3 days ago");
        // A message ahead of the clock is now.
        assert_eq!(ago_bucket(-50), "now");
    }

    #[test]
    fn pieces_mark_the_owner_where_the_whole_text_mentions_it() {
        let marked = |text: &str, first: usize, w: usize| -> Vec<(String, String)> {
            (pieces(text, first, w).into_iter())
                .flat_map(|(piece, ranges)| {
                    (ranges.into_iter()).map(move |r| (piece.clone(), piece[r].to_string()))
                })
                .collect()
        };
        let text = "hi @Manager there\n> @manager quoted\nx `@manager` y @manager";
        // The quoted line and the code span are not marked; each mention is whole, on the
        // piece it wrapped to.
        assert_eq!(
            marked(text, 8, 8),
            [("@Manager", "@Manager"), ("@manager", "@manager")]
                .map(|(p, m)| (p.to_string(), m.to_string()))
        );
        // After wide characters the offsets still land on the mention.
        assert_eq!(
            marked("日本語です @manager", 40, 40),
            [("日本語です @manager".to_string(), "@manager".to_string())]
        );
        // The first piece narrower than the rest, after wide characters: the head split
        // keeps the offsets on the mention.
        assert_eq!(
            marked("日本語 @manager です @Manager", 12, 40),
            [
                ("@manager です @Manager", "@manager"),
                ("@manager です @Manager", "@Manager"),
            ]
            .map(|(p, m)| (p.to_string(), m.to_string()))
        );
        // Right after kana, with no space, the mention is marked.
        assert_eq!(
            marked("見てから@managerに返事", 40, 40),
            [("見てから@managerに返事".to_string(), "@manager".to_string())]
        );
        let split = pieces("日本語 @manager です @Manager", 12, 40);
        assert_eq!(split[0].0, "日本語");
        // A word too wide for a line breaks inside it; the mention's halves stay marked.
        let halves = marked("@manager", 4, 4);
        assert_eq!(
            halves,
            [("@man", "@man"), ("ager", "ager")].map(|(p, m)| (p.to_string(), m.to_string()))
        );
        // The first piece takes its own width, the rest the full width.
        let p = pieces("one two three four five", 7, 14);
        let p: Vec<&str> = p.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(p, ["one two", "three four", "five"]);
    }

    #[test]
    fn a_tagged_events_second_piece_starts_with_the_run_its_first_line_carries() {
        // A tagged event's first piece is narrower than the rest (`plan`). A U+3000 run the
        // first line's break carries starts the second piece with its word, as an untagged
        // event's second line does (`wrap_event_line`); after a word ending with a run, the
        // run hangs with that one.
        let texts = |text: &str, first: usize, w: usize| -> Vec<String> {
            (pieces(text, first, w).into_iter())
                .map(|(s, _)| s)
                .collect()
        };
        assert_eq!(wrap_event_line("ab 　中文 cd", 5)[1], "　中");
        assert_eq!(texts("ab 　中文 cd", 5, 10), ["ab", "　中文 cd"]);
        assert_eq!(texts("ab　 　中文 cd", 4, 10), ["ab　", "中文 cd"]);
    }

    #[test]
    fn a_bubble_takes_85_percent_of_the_chat_with_at_most_100_columns_of_text() {
        // 85%, rounded down; one column in from each edge in a tiny chat; never none.
        for (w, want) in [
            (60, 51),
            (100, 85),
            (120, 102),
            (121, 102),
            (122, 103),
            (200, 103),
            (1000, 103),
            (10, 8),
            (2, 1),
            (0, 1),
        ] {
            assert_eq!(widest(w), want, "{w}");
        }
        assert_eq!(inner(widest(400)), TEXT_MAX);
    }

    #[test]
    fn a_long_message_wraps_at_the_bubbles_text_width() {
        // Four-letter words fill a line as far as it goes: 20 of them are 99 columns, 21
        // would be 104; 16 are 79 and 17 would be 84.
        let long = vec!["aaaa"; 60].join(" ");
        let m = msgs(&[("dev-1", long.as_str(), 0)]);
        for (w, most) in [(200, 99), (100, 79)] {
            let text = drawn(&m, w, 120, People::default());
            let widest_line = (text.iter())
                .filter_map(|r| {
                    r.split_once("▌ ")
                        .map(|(_, t)| t.trim_end().chars().count())
                })
                .max()
                .unwrap();
            assert_eq!(widest_line, most, "{w}: {text:#?}");
        }
    }

    /// In the full board's narrow pane (`full`) a long message's bubble takes the
    /// chat's whole width, less a column at each edge; elsewhere 85% of it.
    #[test]
    fn a_full_chat_gives_a_long_message_its_whole_width() {
        let long = vec!["aaaa"; 60].join(" ");
        let m = msgs(&[("dev-1", long.as_str(), 0)]);
        let none = BTreeSet::new();
        let bw = |full: bool, w: usize| {
            let s = Shape {
                full,
                ..shape(&none, 0, People::default())
            };
            (plan(&m, 0, w, s).iter())
                .find_map(|r| match r {
                    Row::Name { bw, .. } => Some(*bw),
                    _ => None,
                })
                .unwrap()
        };
        // At 60 columns the widest is 51 (85%), or 58 with `full`, which the wrapping
        // message takes.
        assert_eq!(bw(false, 60), 51);
        assert_eq!(bw(true, 60), 58);
        // A short message's bubble still fits its text.
        let short = msgs(&[("dev-1", "ok", 0)]);
        let s = Shape {
            full: true,
            ..shape(&none, 0, People::default())
        };
        let fit = (plan(&short, 0, 60, s).iter())
            .find_map(|r| match r {
                Row::Name { bw, .. } => Some(*bw),
                _ => None,
            })
            .unwrap();
        assert!(fit < 58, "{fit}");
    }

    /// The copy's `.msg` fits its content: a message that wraps takes the widest a bubble
    /// gets, on either side, and one that fits keeps its width.
    #[test]
    fn a_wrapped_message_takes_the_widest_bubble_and_one_that_fits_keeps_its_width() {
        let none = BTreeSet::new();
        let bw = |author: &str, text: &str| {
            let m = msgs(&[(author, text, 0)]);
            (plan(&m, 0, 60, shape(&none, 0, People::default())).iter())
                .find_map(|r| match r {
                    Row::Name { bw, .. } => Some(*bw),
                    _ => None,
                })
                .unwrap()
        };
        // At 60 columns the widest is 51, 48 of text.
        assert_eq!(widest(60), 51);
        let wraps = format!("{} {}", "a".repeat(20), "b".repeat(30));
        assert_eq!(bw("dev-1", &wraps), 51);
        assert_eq!(bw(OWNER_AUTHOR, &wraps), 51);
        // A U+3000 dropped where the line breaks still counts.
        assert_eq!(
            bw("dev-1", &format!("{} \u{3000}bbbbbb", "a".repeat(44))),
            51
        );
        // A line that fits, or lines that each fit, keep their width.
        let fits = "aaa bbb ccc ddd eee fff ggg hhh";
        assert_eq!(bw("dev-1", fits), 34);
        assert_eq!(bw(OWNER_AUTHOR, fits), 34);
        assert_eq!(bw("dev-1", &format!("{fits}\n{fits}")), 34);
        // Lines that each fit: the widest of them.
        assert_eq!(bw("dev-1", &format!("ab\n{}\nde", "c".repeat(40))), 43);
        // A URL is a block of its own: the text before it and the URL each fit.
        let url = "https://x.example/aaaa/bbbb/cccc/dddd/ee";
        assert_eq!(bw("dev-1", &format!("some words here {url}")), 43);
        // A line the clip hides counts, as the copy's clipped body still measures it.
        let clipped = format!("aa\nbb\ncc\ndd\nee\n{}", "d".repeat(30));
        assert_eq!(bw("dev-1", &clipped), 33);
    }

    #[test]
    fn urls_are_found_as_the_copy_finds_them() {
        let u = |s: &str| {
            (urls(s).into_iter())
                .map(|r| s[r].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(u("see https://a.b/c."), ["https://a.b/c"]);
        assert_eq!(
            u("(https://a.b/c), and http://x.y?q=1!"),
            ["https://a.b/c", "http://x.y?q=1"]
        );
        assert_eq!(
            u("\"https://a.b/c\" <https://d.e>"),
            ["https://a.b/c", "https://d.e"]
        );
        assert_eq!(
            u("https://a.b/c]d 'https://e.f/g'"),
            ["https://a.b/c", "https://e.f/g"]
        );
        // Not URLs: no scheme, another scheme, a bare scheme, one character after it.
        assert!(u("http and httpx://a.b ftp://a.b https:// https://a").is_empty());
    }

    #[test]
    fn a_url_wider_than_a_line_breaks_only_after_a_separator_the_rest_indented() {
        let url = "https://gitlab.example.com/acme/bookshop/-/merge_requests/4?tab=diffs&x=1";
        assert_eq!(
            pack_url(url, 30),
            [
                "https://gitlab.example.com/",
                "  acme/bookshop/-/",
                "  merge_requests/4?tab=diffs&",
                "  x=1"
            ]
        );
        assert_eq!(pack_url("https://a.b/c", 30), ["https://a.b/c"]);
        // A part between separators wider than a line breaks where the line ends.
        assert_eq!(
            pack_url("https://abcdefghijklmnop", 10),
            ["https://", "  abcdefgh", "  ijklmnop"]
        );
    }

    #[test]
    fn a_url_starts_a_line_of_its_own_and_so_does_the_text_after_it() {
        let url = "https://a.b/c/d";
        let pair = |t: &str, l: Option<&str>| (t.to_string(), l.map(str::to_string));
        assert_eq!(
            wrap_message_line("see  https://a.b/c/d  then more", 40),
            [
                pair("see", None),
                pair(url, Some(url)),
                pair("then more", None)
            ]
        );
        // Two URLs, nothing between them: each starts its own line.
        assert_eq!(
            wrap_message_line("https://a.b/c/d https://e.f", 40),
            [
                pair(url, Some(url)),
                pair("https://e.f", Some("https://e.f"))
            ]
        );
        // The owner's mentions stay marked around a URL, and each piece knows its URL.
        let p = message_pieces("@manager look: https://a.b/c/d @manager", 40);
        let got: Vec<_> = (p.iter())
            .map(|p| {
                let marks: Vec<(usize, usize)> = p.marks.iter().map(|r| (r.start, r.end)).collect();
                (p.text.as_str(), marks, p.link.as_deref())
            })
            .collect();
        assert_eq!(
            got,
            [
                ("@manager look:", vec![(0, 8)], None),
                (url, vec![], Some(url)),
                ("@manager", vec![(0, 8)], None)
            ]
        );
    }

    #[test]
    fn a_lines_indent_stays_on_its_first_line_but_not_on_a_url() {
        let url = "https://example.com/a/b";
        let pair = |t: &str, l: Option<&str>| (t.to_string(), l.map(str::to_string));
        assert_eq!(
            wrap_message_line("  - see https://example.com/a/b", 40),
            [pair("  - see", None), pair(url, Some(url))]
        );
        assert_eq!(
            wrap_message_line("  - plain", 40),
            [pair("  - plain", None)]
        );
        // A line that starts with a URL: the URL's line is at the text's column.
        assert_eq!(
            wrap_message_line("  https://example.com/a/b then", 40),
            [pair(url, Some(url)), pair("then", None)]
        );
        // The indent counts toward the first line's width; the lines after it start at the
        // text's column.
        assert_eq!(
            wrap_message_line("    one two three", 12),
            [pair("    one two", None), pair("three", None)]
        );
        // An indented word that fits a line of its own but not after the indent goes to the
        // next line whole, leaving the indent alone on the first, as the copy does.
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            wrap_message_line(&format!("    {hash} sha"), 40),
            [pair("    ", None), pair(hash, None), pair("sha", None)]
        );
        // An indent as wide as the line hangs on a blank line of its own, so no line is
        // wider than the bubble.
        assert_eq!(
            wrap_message_line("      ab cd", 6),
            [pair("", None), pair("ab cd", None)]
        );
        // A line of spaces alone is a blank line, as before.
        assert_eq!(wrap_message_line("   ", 40), [pair("", None)]);
        // The owner's mentions stay marked after an indent.
        let p = message_pieces("  - ask @manager", 40);
        let marks: Vec<(usize, usize)> = p[0].marks.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(
            (p[0].text.as_str(), marks),
            ("  - ask @manager", vec![(8, 16)])
        );
    }

    #[test]
    fn an_indent_as_wide_as_the_text_hangs_on_a_blank_row_before_the_words() {
        // The rows Chrome draws the copy's bubble with (`.seg .body`, `pre-wrap`): the
        // indent hangs on a blank row and the words start the next at the text's column,
        // breaking there as they would from any line's start.
        let texts = |text: &str, w: usize| -> Vec<String> {
            (message_pieces(text, w).into_iter())
                .map(|p| p.text)
                .collect()
        };
        let sp = |n: usize| " ".repeat(n);
        let words = "after a wide indent the words go on";
        assert_eq!(
            texts(&format!("x first\n{}{words}", sp(40)), 31),
            ["x first", "", "after a wide indent the words", "go on"]
        );
        assert_eq!(
            texts(&format!("{}{words}", sp(40)), 31),
            ["", "after a wide indent the words", "go on"]
        );
        for n in [31, 32] {
            assert_eq!(
                texts(&format!("{}after a wide indent", sp(n)), 31),
                ["", "after a wide indent"],
                "indent {n}"
            );
        }
        for n in [7, 9, 20] {
            assert_eq!(texts(&format!("{}ab cd", sp(n)), 7), ["", "ab cd"], "{n}");
        }
        let at7 = |rest: &str| texts(&format!("{}{rest}", sp(7)), 7);
        assert_eq!(at7("abcdefghijklmn"), ["", "abcdefg", "hijklmn"]);
        assert_eq!(at7("ab-cd-ef"), ["", "ab-cd-", "ef"]);
        // A run of ideographic spaces starting the first word hangs with the indent.
        assert_eq!(at7("\u{3000}ab"), ["", "ab"]);
        assert_eq!(at7("\u{3000} ab"), ["", "ab"]);
        assert_eq!(at7("\u{3000}"), [""]);
        // On a later line too.
        assert_eq!(
            texts(&format!("ab\n{}cd ef", sp(9)), 7),
            ["ab", "", "cd ef"]
        );
        // Narrower, the indent starts the first row, as before; a line of spaces alone is
        // one blank row.
        assert_eq!(
            texts(&format!("{}after a wide indent", sp(30)), 31),
            [sp(30).as_str(), "after a wide indent"]
        );
        assert_eq!(
            texts(&format!("{}ab cd", sp(29)), 31),
            [format!("{}ab", sp(29)).as_str(), "cd"]
        );
        assert_eq!(texts(&sp(9), 7), [""]);
        // The owner's mention after the blank row stays marked.
        let p = message_pieces(&format!("{}ask @manager", sp(12)), 12);
        let got: Vec<(&str, Vec<(usize, usize)>)> = (p.iter())
            .map(|p| {
                let marks = p.marks.iter().map(|r| (r.start, r.end)).collect();
                (p.text.as_str(), marks)
            })
            .collect();
        assert_eq!(got, [("", vec![]), ("ask @manager", vec![(4, 12)])]);
        // A delivery failure's details (`.evtdetail`, `pre-wrap`) hang it the same way.
        assert_eq!(wrap(&format!("{}ab cd", sp(7)), 7), ["", "ab cd"]);
        assert_eq!(
            wrap(&format!("ab\n{}exit 1", sp(9)), 7),
            ["ab", "", "exit 1"]
        );
        assert_eq!(wrap(&format!("{}\u{3000}ab", sp(7)), 7), ["", "ab"]);
    }

    #[test]
    fn a_lines_inner_runs_of_spaces_stay_and_a_break_drops_them() {
        let pair = |t: &str| (t.to_string(), None);
        // A line aligned in columns keeps its runs of spaces, as the copy's pre-wrap body does.
        assert_eq!(
            wrap_message_line("phone    2 rows", 40),
            [pair("phone    2 rows")]
        );
        assert_eq!(wrap_message_line("  - a    b", 40), [pair("  - a    b")]);
        // A break at a run drops the whole run; the next line starts at the text's column.
        assert_eq!(
            wrap_message_line("aa    bb    cc", 9),
            [pair("aa    bb"), pair("cc")]
        );
        // A run counts toward the line's width.
        assert_eq!(wrap_message_line("aa    bb", 7), [pair("aa"), pair("bb")]);
        // The spaces at the line's end go, as before.
        assert_eq!(wrap_message_line("a  b   ", 40), [pair("a  b")]);
        // A word too wide for any line starts on the next, whether or not the run leaves a
        // column on this one; the run hangs.
        assert_eq!(
            wrap_message_line("ab   abcdefghij", 8),
            [pair("ab"), pair("abcdefgh"), pair("ij")]
        );
        assert_eq!(
            wrap_message_line("ab      abcdefghij", 8),
            [pair("ab"), pair("abcdefgh"), pair("ij")]
        );
        // Only the spaces next to a URL go; a run in the text after it stays.
        let url = "https://a.b/c/d";
        assert_eq!(
            wrap_message_line("see    https://a.b/c/d    then  more", 40),
            [
                pair("see"),
                (url.to_string(), Some(url.to_string())),
                pair("then  more")
            ]
        );
        // The owner's mentions stay marked after a run.
        let p = message_pieces("ask    @manager", 40);
        let marks: Vec<(usize, usize)> = p[0].marks.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(
            (p[0].text.as_str(), marks),
            ("ask    @manager", vec![(7, 15)])
        );
        // In wrap_event_line (events, acks) a line's spaces still fold: its indent goes, each
        // run is one.
        assert_eq!(wrap_event_line("  a    b  ", 40), ["a b"]);
    }

    #[test]
    fn a_word_too_wide_for_any_line_starts_on_a_line_of_its_own() {
        let pair = |t: &str| (t.to_string(), None);
        // After one space, as the copy's `overflow-wrap: anywhere` body breaks there first;
        // the word then breaks where each line ends.
        assert_eq!(
            wrap_message_line("ab abcdefghij", 8),
            [pair("ab"), pair("abcdefgh"), pair("ij")]
        );
        let hash = "0123456789abcdef0123456789abcdef0123456789";
        assert_eq!(
            wrap_message_line(&format!("ab {hash}"), 40),
            [pair("ab"), pair(&hash[..40]), pair(&hash[40..])]
        );
        // After several words, and with text after it, which goes on the word's last line.
        assert_eq!(
            wrap_message_line("a ab abcdefghij cd", 8),
            [pair("a ab"), pair("abcdefgh"), pair("ij cd")]
        );
        // After a line's indent, which stays alone on the first line.
        assert_eq!(
            wrap_message_line("    abcdefghij", 8),
            [pair("    "), pair("abcdefgh"), pair("ij")]
        );
        // Alone on the line it is broken where the line ends, as before.
        assert_eq!(
            wrap_message_line("abcdefghij", 8),
            [pair("abcdefgh"), pair("ij")]
        );
        // A path still starts where the line is and breaks after a `/`, as the copy's path
        // does.
        assert_eq!(
            wrap_message_line("ab assets/icons/bookmarks/outline", 12),
            [
                pair("ab assets/"),
                pair("icons/"),
                pair("bookmarks/"),
                pair("outline")
            ]
        );
    }

    #[test]
    fn a_word_breaks_after_a_hyphen_where_the_copy_does() {
        let wrapped = |t: &str, w: usize| -> Vec<String> {
            (wrap_message_line(t, w).into_iter())
                .map(|(p, link)| {
                    assert_eq!(link, None);
                    p
                })
                .collect()
        };
        // A word with a `-` starts where the line is and breaks after it, as Chrome breaks
        // the copy's body; a piece that then does not fit goes to the next line.
        assert_eq!(wrapped("ab abc-def", 8), ["ab abc-", "def"]);
        assert_eq!(wrapped("ab abc-def", 6), ["ab", "abc-", "def"]);
        assert_eq!(
            wrapped(
                "ab a-word-too-wide-for-any-line-starts-on-a-line-of-its-own",
                40
            ),
            [
                "ab a-word-too-wide-for-any-line-starts-",
                "on-a-line-of-its-own"
            ]
        );
        assert_eq!(wrapped("ab 2026-11-19", 8), ["ab 2026-", "11-19"]);
        assert_eq!(wrapped("ab 2026-11-19", 6), ["ab", "2026-", "11-19"]);
        assert_eq!(wrapped("ab @dev-8 ok", 8), ["ab @dev-", "8 ok"]);
        assert_eq!(wrapped("ab @dev-8 ok", 6), ["ab", "@dev-8", "ok"]);
        // After each `-` of a run, and not after one that ends the word.
        assert_eq!(wrapped("ab --verbose", 8), ["ab --", "verbose"]);
        assert_eq!(wrapped("ab a--bcdefg", 6), ["ab a--", "bcdefg"]);
        assert_eq!(
            wrapped("ab cargo fmt --all --check", 14),
            ["ab cargo fmt -", "-all --check"]
        );
        assert_eq!(wrapped("ab --", 4), ["ab -", "-"]);
        assert_eq!(wrapped("ab --all", 4), ["ab -", "-all"]);
        assert_eq!(wrapped("ab --verbose", 4), ["ab -", "-", "verb", "ose"]);
        assert_eq!(wrapped("ab a--bcdefg", 5), ["ab a-", "-", "bcdef", "g"]);
        assert_eq!(wrapped("ab abc---d", 7), ["ab abc-", "--d"]);
        assert_eq!(wrapped("ab ----", 5), ["ab --", "--"]);
        assert_eq!(wrapped("ab (--x", 5), ["ab (-", "-x"]);
        assert_eq!(wrapped("ab x--", 4), ["ab", "x--"]);
        assert_eq!(wrapped("ab abcdef-", 8), ["ab", "abcdef-"]);
        // A piece too wide for a line breaks where the line ends.
        assert_eq!(
            wrapped("ab x-yzzzzzzzzzzz", 8),
            ["ab x-", "yzzzzzzz", "zzzz"]
        );
        // Not after a minus sign: a `-` with no letter or digit before it and a digit after.
        assert_eq!(wrapped("ab -5000000", 8), ["ab", "-5000000"]);
        assert_eq!(wrapped("ab -5000000", 6), ["ab", "-50000", "00"]);
        assert_eq!(wrapped("ab (-5000000", 10), ["ab", "(-5000000"]);
        assert_eq!(wrapped("ab --5000", 5), ["ab -", "-5000"]);
        assert_eq!(wrapped("ab --5000", 4), ["ab -", "-500", "0"]);
        assert_eq!(wrapped("ab x-5000000", 8), ["ab x-", "5000000"]);
        // Not before a character a line cannot start with.
        assert_eq!(wrapped("ab abc-,defg", 8), ["ab", "abc-,def", "g"]);
        assert_eq!(wrapped("ab abc-)defg", 8), ["ab", "abc-)def", "g"]);
        // Before a character other than ASCII, where Chrome breaks through ICU: not before a
        // quote or another script's digit, nor after a minus sign (no ASCII letter or digit
        // before the `-`).
        assert_eq!(
            wrapped("the loaves for the morning shift: café-12", 40),
            ["the loaves for the morning shift:", "café-12"]
        );
        assert_eq!(
            wrapped("the loaves for the morning are non-“standard” today", 40),
            ["the loaves for the morning are", "non-“standard” today"]
        );
        assert_eq!(wrapped("ab aé-5000000", 8), ["ab", "aé-50000", "00"]);
        assert_eq!(wrapped("ab abc-٣٣", 8), ["ab", "abc-٣٣"]);
        assert_eq!(wrapped("ab x-éabcdef", 8), ["ab x-", "éabcdef"]);
        assert_eq!(wrapped("ab abc-ーdefg", 8), ["ab abc-", "ーdefg"]);
        assert_eq!(wrapped("ab abc-·defg", 8), ["ab abc-", "·defg"]);
        // Nor after a `-` that starts the word, before one.
        assert_eq!(wrapped("ab -éabcdefg", 8), ["ab", "-éabcdef", "g"]);
        assert_eq!(wrapped("ab -½00000", 9), ["ab", "-½00000"]);
        // A word with breaks starts after a line's indent only when its first piece fits
        // there; else the indent keeps its line, as the copy's does, and so does a path's.
        assert_eq!(wrapped("    ab-cdefgh", 8), ["    ab-", "cdefgh"]);
        let indent = " ".repeat(38);
        assert_eq!(
            wrapped(&format!("{indent}rye-bread"), 40),
            [indent.as_str(), "rye-bread"]
        );
        let indent = " ".repeat(36);
        let path = "assets/icons/bookmarks/outline.svg";
        assert_eq!(
            wrapped(&format!("{indent}{path}"), 40),
            [indent.as_str(), path]
        );
        // The owner's mention after a `-` stays marked on its own line.
        let p = message_pieces("ab x-@manager ok", 8);
        let got: Vec<(&str, Vec<(usize, usize)>)> = (p.iter())
            .map(|p| {
                let marks = p.marks.iter().map(|r| (r.start, r.end)).collect();
                (p.text.as_str(), marks)
            })
            .collect();
        assert_eq!(
            got,
            [
                ("ab x-", vec![]),
                ("@manager", vec![(0, 8)]),
                ("ok", vec![])
            ]
        );
    }

    #[test]
    fn a_word_breaks_after_a_question_mark_where_the_copy_does() {
        let wrapped = |t: &str, w: usize| -> Vec<String> {
            (wrap_message_line(t, w).into_iter())
                .map(|(p, link)| {
                    assert_eq!(link, None);
                    p
                })
                .collect()
        };
        // A word with a `?` starts where the line is and breaks after it, as Chrome breaks
        // the copy's body; a piece that then does not fit goes to the next line.
        assert_eq!(wrapped("ab abc?defg", 8), ["ab abc?", "defg"]);
        assert_eq!(wrapped("ab abc?defg", 6), ["ab", "abc?", "defg"]);
        assert_eq!(wrapped("ab what?why", 8), ["ab what?", "why"]);
        assert_eq!(
            wrapped("see search?q=rye&page=2 for the loaves", 16),
            ["see search?", "q=rye&page=2 for", "the loaves"]
        );
        assert_eq!(
            wrapped(
                "did the proof rise?the dough doubled overnight?the oven is hot?bake it now",
                24
            ),
            [
                "did the proof rise?the",
                "dough doubled overnight?",
                "the oven is hot?bake it",
                "now"
            ]
        );
        // Before a digit, a `(`, and a `$` (which a `-` does not break before); beside a `-`;
        // and at a word's start.
        assert_eq!(wrapped("ab x?5000000", 8), ["ab x?", "5000000"]);
        assert_eq!(wrapped("ab abc?$500", 8), ["ab abc?", "$500"]);
        assert_eq!(wrapped("ab abc?(defg)", 8), ["ab abc?", "(defg)"]);
        assert_eq!(wrapped("ab abc?-defg", 7), ["ab abc?", "-defg"]);
        assert_eq!(wrapped("ab abc-?defg", 8), ["ab abc-?", "defg"]);
        assert_eq!(wrapped("ab ?defg", 4), ["ab ?", "defg"]);
        // A run of `?` keeps together and breaks after its last.
        assert_eq!(wrapped("ab abc??defg", 8), ["ab abc??", "defg"]);
        assert_eq!(wrapped("ab ???", 4), ["ab", "???"]);
        // Not before a character a line cannot start with after a `?`, nor before a quote,
        // nor after a `?` that ends the word.
        for t in [
            "ab abc?!defg",
            "ab abc?)defg",
            "ab abc?/defg",
            "ab abc?\"defg\"",
        ] {
            let (head, tail) = t[3..].split_at(8);
            assert_eq!(wrapped(t, 8), ["ab", head, tail], "{t}");
        }
        assert_eq!(wrapped("ab abc?'defg'", 8), ["ab", "abc?'def", "g'"]);
        assert_eq!(wrapped("ab abc?“defg”", 8), ["ab", "abc?“def", "g”"]);
        assert_eq!(wrapped("ab defg?", 6), ["ab", "defg?"]);
        // Beside another script as before, where Chrome breaks through ICU; and after a `?`
        // that follows an ideograph, before an ASCII letter.
        assert_eq!(wrapped("ab abc?édefg", 8), ["ab abc?", "édefg"]);
        assert_eq!(wrapped("ab abc?中文字", 8), ["ab abc?", "中文字"]);
        assert_eq!(wrapped("ab 中?abcdefg", 8), ["ab 中?", "abcdefg"]);
        // A path ends at a `?`, which breaks from what follows; an indent stays.
        assert_eq!(
            wrapped("ab assets/icons/bookmarks/outline.svg?v=2 ok", 12),
            [
                "ab assets/",
                "icons/",
                "bookmarks/",
                "outline.svg?",
                "v=2 ok"
            ]
        );
        assert_eq!(wrapped("    abc?defghij", 8), ["    abc?", "defghij"]);
        // The owner's mention after a `?` stays marked on its own line.
        let p = message_pieces("ab x?@manager ok", 8);
        let got: Vec<(&str, Vec<(usize, usize)>)> = (p.iter())
            .map(|p| {
                let marks = p.marks.iter().map(|r| (r.start, r.end)).collect();
                (p.text.as_str(), marks)
            })
            .collect();
        assert_eq!(
            got,
            [
                ("ab x?", vec![]),
                ("@manager", vec![(0, 8)]),
                ("ok", vec![])
            ]
        );
    }

    #[test]
    fn the_owners_mention_after_a_hung_ideographic_space_stays_marked() {
        // The U+3000 at the first line's end hangs past it, left off the line, and the
        // mention after it is marked where it is drawn.
        let p = message_pieces("確認です　@manager ok", 8);
        let got: Vec<(&str, Vec<(usize, usize)>)> = (p.iter())
            .map(|p| {
                let marks = p.marks.iter().map(|r| (r.start, r.end)).collect();
                (p.text.as_str(), marks)
            })
            .collect();
        assert_eq!(
            got,
            [
                ("確認です", vec![]),
                ("@manager", vec![(0, 8)]),
                ("ok", vec![])
            ]
        );
    }

    #[test]
    fn a_word_breaks_before_an_opening_bracket_after_punctuation_where_the_copy_does() {
        let wrapped = |t: &str, w: usize| -> Vec<String> {
            (wrap_message_line(t, w).into_iter())
                .map(|(p, link)| {
                    assert_eq!(link, None);
                    p
                })
                .collect()
        };
        // Before a `(`, `[`, `{` or `<` after a punctuation mark, as Chrome breaks the copy's
        // body; a piece that then does not fit goes to the next line.
        assert_eq!(wrapped("ab abc!(defg)", 8), ["ab abc!", "(defg)"]);
        assert_eq!(wrapped("ab abc.(defg)", 8), ["ab abc.", "(defg)"]);
        assert_eq!(wrapped("ab abc,[defg]", 8), ["ab abc,", "[defg]"]);
        assert_eq!(wrapped("ab abc:{defg}", 8), ["ab abc:", "{defg}"]);
        assert_eq!(wrapped("ab abc;<defg>", 8), ["ab abc;", "<defg>"]);
        assert_eq!(wrapped("ab abc)(defg)", 8), ["ab abc)", "(defg)"]);
        assert_eq!(wrapped("ab f(x),(y)", 8), ["ab f(x),", "(y)"]);
        assert_eq!(wrapped("ab 5%(defg)", 8), ["ab 5%", "(defg)"]);
        assert_eq!(wrapped("ab a=<b>", 5), ["ab a=", "<b>"]);
        // Not after a letter, a digit, an opening bracket, or one of the marks Chrome keeps
        // with it.
        assert_eq!(wrapped("ab abc(defg)", 8), ["ab", "abc(defg", ")"]);
        assert_eq!(wrapped("ab abc5(defg)", 8), ["ab", "abc5(def", "g)"]);
        assert_eq!(wrapped("ab ab((defg)", 8), ["ab", "ab((defg", ")"]);
        assert_eq!(wrapped("ab abc$(defg)", 8), ["ab", "abc$(def", "g)"]);
        assert_eq!(wrapped("ab abc'(defg)", 8), ["ab", "abc'(def", "g)"]);
        assert_eq!(wrapped("ab abc_[defg]", 8), ["ab", "abc_[def", "g]"]);
    }

    #[test]
    fn cjk_text_breaks_between_its_characters_where_the_copy_does() {
        let wrapped = |t: &str, w: usize| -> Vec<String> {
            (wrap_message_line(t, w).into_iter())
                .map(|(p, link)| {
                    assert_eq!(link, None);
                    p
                })
                .collect()
        };
        // Between two ideographs, after a run of spaces or one, as Chrome breaks the copy's
        // body.
        assert_eq!(wrapped("ab  中文字", 6), ["ab  中", "文字"]);
        assert_eq!(wrapped("ab 中文字", 6), ["ab 中", "文字"]);
        // A run too wide for any line fills the line it starts on.
        let run = "中".repeat(50);
        assert_eq!(
            wrapped(&format!("ab {run}"), 40),
            [
                format!("ab {}", "中".repeat(18)),
                "中".repeat(20),
                "中".repeat(12)
            ]
        );
        // Between an ideograph and a letter, kana and hangul too.
        assert_eq!(
            wrapped(
                "rebased the branch onto master: テストは全部通りました、問題ありません",
                40
            ),
            [
                "rebased the branch onto master: テストは",
                "全部通りました、問題ありません"
            ]
        );
        assert_eq!(
            wrapped("今日はパンを焼きました。明日はケーキを作る予定です。", 20),
            [
                "今日はパンを焼きまし",
                "た。明日はケーキを作",
                "る予定です。"
            ]
        );
        assert_eq!(
            wrapped(
                "오늘 아침에 빵을 세 개 구웠습니다. 내일은 케이크를 만들 예정입니다.",
                14
            ),
            [
                "오늘 아침에 빵",
                "을 세 개 구웠",
                "습니다. 내일은",
                "케이크를 만들",
                "예정입니다."
            ]
        );
        // Not before a closing mark nor after an opening one.
        assert_eq!(wrapped("中文。字字", 4), ["中", "文。", "字字"]);
        assert_eq!(wrapped("「中文」字", 4), ["「中", "文」", "字"]);
        assert_eq!(wrapped("中（中）中", 6), ["中", "（中）", "中"]);
        // A `ー` may start a line, as CSS `line-break: auto` lets it.
        assert_eq!(wrapped("テスト・ケース", 6), ["テス", "ト・ケ", "ース"]);
        // A curly quote breaks from an ideograph on its outer side (UAX #14's LB19a).
        assert_eq!(wrapped("ab 中“文”中", 5), ["ab 中", "“文”", "中"]);
        assert_eq!(wrapped("ab 中“文”中", 9), ["ab 中“文”", "中"]);
        // After a `-` that starts the word, before an ideograph but not before a letter,
        // nor after a `‐` before one (LB20a).
        assert_eq!(wrapped("ab -中文字中文字", 8), ["ab -中文", "字中文字"]);
        assert_eq!(wrapped("ab ‐éabc", 6), ["ab", "‐éabc"]);
        // A word with no break of its own after an ideograph starts on a line of its own.
        let hash = "0123456789abcdef0123456789abcdef0123456789";
        assert_eq!(
            wrapped(&format!("ab 中{hash}"), 40),
            ["ab 中", &hash[..40], &hash[40..]]
        );
        // After a `…`, which Chrome also breaks through ICU.
        assert_eq!(wrapped("ab abc…defg", 8), ["ab abc…", "defg"]);
    }

    #[test]
    fn a_path_joined_to_cjk_text_breaks_after_a_slash_and_the_text_around_it_as_any_other() {
        let wrapped = |t: &str, w: usize| -> Vec<String> {
            (wrap_message_line(t, w).into_iter())
                .map(|(p, link)| {
                    assert_eq!(link, None);
                    p
                })
                .collect()
        };
        // The copy marks only the path inside the word, so the text before and after it
        // breaks between its characters, and never before a closing mark.
        assert_eq!(
            wrapped(
                "ログは/var/log/tower/output.logに書いてあるので原因を調べます。明日また報告します。",
                40
            ),
            [
                "ログは/var/log/tower/output.logに書いて",
                "あるので原因を調べます。明日また報告しま",
                "す。"
            ]
        );
        assert_eq!(
            wrapped(
                "日志在/opt/tower/var/log/tower.log里面，请看第三行和第四行，然后告诉我结果。",
                25
            ),
            [
                "日志在/opt/tower/var/log/",
                "tower.log里面，请看第三行",
                "和第四行，然后告诉我结",
                "果。"
            ]
        );
        // A path under 16 characters is no path to the copy: it does not break after a `/`.
        assert_eq!(
            wrapped(
                "テストはsrc/ui/chat.rsに追加しました。レビューをお願いします。",
                40
            ),
            [
                "テストはsrc/ui/chat.rsに追加しました。レ",
                "ビューをお願いします。"
            ]
        );
    }

    #[test]
    fn the_tags_and_age_columns_are_what_the_name_line_draws() {
        for text in ["plain", "blocked: x", "decision: y"] {
            let m = &msgs(&[("dev-1", text, 0)])[0];
            let drawn: usize = meta(m, 59 * 60, Depth::Ansi256)
                .iter()
                .map(Span::width)
                .sum();
            // "59m" is one column under the widest age.
            assert_eq!(drawn + AGE_COLUMNS - 3, meta_columns(m), "{text}");
        }
    }

    #[test]
    fn a_later_message_in_a_run_has_its_tags_and_age_on_a_line_of_its_own() {
        // Everyone else's at the bubble's left, under the name line holding the first's age.
        let m = msgs(&[("dev-1", "one", 0), ("dev-1", "two", 60)]);
        let text = drawn(&m, 100, 120, People::default());
        let i = text.iter().position(|r| r.contains("▌ one")).unwrap();
        assert_eq!(text[i - 1].trim_end(), "  dev-1 · developer · 2m");
        assert_eq!(text[i + 1].trim_end(), " ▌ 1m", "{text:#?}");
        assert!(text[i + 2].contains("▌ two"), "{text:#?}");
        // The owner's at the right, with its tags; no dashed rule in either.
        let m = msgs(&[("manager", "first", 0), ("manager", "blocked: second", 60)]);
        let text = drawn(&m, 100, 120, People::default());
        let i = text.iter().position(|r| r.contains("first")).unwrap();
        assert!(text[i - 1].trim_end().ends_with("you (manager) · 2m"));
        assert!(
            text[i + 1].trim_end().ends_with(" blocked · 1m ▐"),
            "{text:#?}"
        );
        assert!(text[i + 2].contains("blocked: second"), "{text:#?}");
        assert!(!text.iter().any(|r| r.contains('╌')), "{text:#?}");
    }

    #[test]
    fn the_owner_is_on_the_right_and_everyone_else_on_the_left() {
        let m = msgs(&[("dev-1", "hello there", 0), ("manager", "go ahead", 60)]);
        let text = drawn(&m, 100, 120, People::default());
        let dev = text.iter().find(|r| r.contains("hello there")).unwrap();
        assert!(dev.starts_with(" ▌ hello"), "{dev:?}");
        let me = text.iter().find(|r| r.contains("go ahead")).unwrap();
        // The owner's edge is one column in from the right edge.
        assert_eq!(me.chars().nth(98), Some('▐'), "{me:?}");
        assert_eq!(me.chars().count(), 100);
        let name = text.iter().find(|r| r.contains("you (manager)")).unwrap();
        assert_eq!(name.trim_end().chars().count(), 98, "{name:?}");
        // Short messages get the narrowest bubble, 24 columns: the owner's starts 24 in
        // from its edge column.
        let lead_in = me.chars().take_while(|c| *c == ' ').count();
        assert_eq!(lead_in, 100 - 1 - BUBBLE_MIN + 1, "{me:?}");
        // Each message's age is on its name line, after the name, not in its text.
        assert!(name.trim_end().ends_with("you (manager) · 1m"), "{name:?}");
        let dev_name = text.iter().find(|r| r.contains("dev-1")).unwrap();
        assert_eq!(
            dev_name.trim_end(),
            "  dev-1 · developer · 2m",
            "{dev_name:?}"
        );
        assert_eq!(dev.trim_end(), " ▌ hello there", "{dev:?}");
        assert_eq!(me.trim_start(), "go ahead              ▐ ", "{me:?}");
    }

    #[test]
    fn any_repos_lead_reads_as_a_lead_and_the_role_word_names_its_repo() {
        let own = lead();
        let other = Lead {
            name: "Search Lead".into(),
            session: "search-lead".into(),
            colour: None,
        };
        let others = [("search".to_string(), &other)];
        let people = People {
            lead: Some(&own),
            others: &others,
        };
        let m = msgs(&[("kids-lead", "here", 0), ("search-lead", "there", 60)]);
        let text = drawn(&m, 100, 120, people).join("\n");
        assert!(text.contains("kids-lead · lead"), "{text}");
        assert!(text.contains("search-lead · search lead"), "{text}");
        assert_eq!(
            people.colour("search-lead", Depth::Ansi256),
            accent(Depth::Ansi256)
        );
        assert_eq!(
            people.colour("kids-lead", Depth::Ansi256),
            accent(Depth::Ansi256)
        );
    }

    #[test]
    fn a_long_ack_fills_each_line_from_its_arrow_and_ends_with_its_age() {
        let m = msgs(&[
            ("dev-1", &"word ".repeat(12), 0),
            ("senior", "copy, reviewing the layout and the docs", 60),
        ]);
        let rows = drawn(&m, 30, 120, People::default());
        let at = rows.iter().position(|r| r.contains("↳ senior")).unwrap();
        let ack: Vec<&str> = rows[at..].iter().map(|r| r.trim()).collect();
        // Headless Chrome on the copy's `.ack` 23 wide, as under a bubble 25 wide here: each
        // line filled, and each starting where the first does.
        assert_eq!(
            ack,
            [
                "↳ senior: copy,",
                "reviewing the layout",
                "and the docs · 1m"
            ],
            "{rows:#?}"
        );
        assert!(rows[at..].iter().all(|r| start(r) == 3), "{rows:#?}");
    }

    #[test]
    fn an_ack_fills_each_line_and_lays_its_age_out_after_its_last_word_as_the_copy_does() {
        // Headless Chrome on the copy's `.ack`, `· age` inline after the text: the words fill
        // each line to the ack's width; ` · age` follows the last word where it fits, else
        // `·` ends that line where it fits and the age starts the next, else `· age` does.
        // Each line of `ack` as (its first column, its last column + 1, its text).
        let ack = |w: usize, bubble: &str, text: &str, now: i64| -> Vec<(usize, usize, String)> {
            let m = msgs(&[(bubble, "Report", 0), ("qa", text, 0)]);
            let rows = drawn(&m, w, now, People::default());
            let at = rows.iter().position(|r| r.contains("↳ qa")).unwrap();
            (rows[at..].iter())
                .take_while(|r| !r.trim().is_empty())
                .map(|r| (start(r), start(r) + r.trim().width(), r.trim().to_string()))
                .collect()
        };
        let texts = |lines: &[(usize, usize, String)]| -> Vec<String> {
            lines.iter().map(|(_, _, t)| t.clone()).collect()
        };
        // The issue's: lines 20 wide (a bubble of 22, in a chat of 26), then 15 (17, in 20).
        let got = ack(26, "dev-1", "copy, testing the thing now", 8 * 60);
        assert_eq!(texts(&got), ["↳ qa: copy, testing", "the thing now · 8m"]);
        let got = ack(20, "dev-1", "copy senior-reviewer-2", 30);
        assert_eq!(
            texts(&got),
            ["↳ qa: copy", "senior-", "reviewer-2 ·", "now"]
        );
        // A hash 26 to 32 wide at a bubble of 34, in a chat of 40: lines 32 wide. A hash that
        // fits a line is not cut, and the age goes where Chrome puts it.
        let hash = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6";
        for (n, want) in [
            (26, vec!["↳ qa: copy", "a1b2c3d4e5f6a7b8c9d0e1f2a3 · 8m"]),
            (
                28,
                vec!["↳ qa: copy", "a1b2c3d4e5f6a7b8c9d0e1f2a3b4 ·", "8m"],
            ),
            (
                30,
                vec!["↳ qa: copy", "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5 ·", "8m"],
            ),
            (32, vec!["↳ qa: copy", hash, "· 8m"]),
        ] {
            let text = format!("copy {}", &hash[..n]);
            // Under someone else's bubble, at the left, each line starts at 3.
            let got = ack(40, "dev-1", &text, 8 * 60);
            assert_eq!(texts(&got), want, "{n}: {got:#?}");
            assert!(got.iter().all(|&(at, _, _)| at == 3), "{n}: {got:#?}");
            // Under the owner's, at the right, each line ends at the bubble's edge, 38.
            let got = ack(40, "manager", &text, 8 * 60);
            assert_eq!(texts(&got), want, "{n}: {got:#?}");
            assert!(got.iter().all(|&(_, end, _)| end == 38), "{n}: {got:#?}");
        }
    }

    #[test]
    fn an_acks_age_after_a_word_too_wide_for_a_line_starts_the_next_line() {
        // Headless Chrome on the copy's `.ack`: a word too wide for a line overflows it and
        // "· 8m" starts the next line, while a last word that fits keeps it ("ok · 8m").
        // Under a bubble 25 wide, in a chat of 30, an ack's lines are 23 wide; the hash is 25.
        let ack = |text: &str| -> Vec<String> {
            let m = msgs(&[("dev-1", "Report", 0), ("qa", text, 0)]);
            let rows = drawn(&m, 30, 8 * 60, People::default());
            let at = rows.iter().position(|r| r.contains("↳ qa")).unwrap();
            rows[at..]
                .iter()
                .map(|r| r.trim_end().to_string())
                .collect()
        };
        let trimmed = |rows: &[String]| -> Vec<String> {
            rows.iter().map(|r| r.trim().to_string()).collect()
        };
        let cut = "a1b2c3d4e5f6a7b8c9d0e1f";
        // A hash, and a word whose last piece after its `-` is one: the age on its own line,
        // where the line before it starts.
        for (text, want) in [
            (
                "copy a1b2c3d4e5f6a7b8c9d0e1f2a",
                ["↳ qa: copy", cut, "2a", "· 8m"],
            ),
            (
                "copy senior-a1b2c3d4e5f6a7b8c9d0e1f2a",
                ["↳ qa: copy senior-", cut, "2a", "· 8m"],
            ),
        ] {
            let rows = ack(text);
            assert_eq!(trimmed(&rows[..4]), want, "{rows:#?}");
            assert_eq!(rows[2].find('2'), rows[3].find('·'), "{rows:#?}");
        }
        // A word that fits after the cut one, or a last piece that fits, keeps the age.
        for (text, want) in [
            (
                "copy a1b2c3d4e5f6a7b8c9d0e1f2a ok",
                ["↳ qa: copy", cut, "2a", "ok · 8m"],
            ),
            (
                "copy a1b2c3d4e5f6a7b8c9d0e1f2a-ok",
                ["↳ qa: copy", cut, "2a-", "ok · 8m"],
            ),
        ] {
            let rows = ack(text);
            assert_eq!(trimmed(&rows[..4]), want, "{rows:#?}");
            assert!(!rows.get(4).is_some_and(|r| r.contains('·')), "{rows:#?}");
        }
    }

    #[test]
    fn a_preview_never_starts_on_an_ack_it_cannot_place_and_shows_tower_lines_alone() {
        let none = BTreeSet::new();
        let preview = |m: &[Message], missing| {
            let s = Shape {
                clip: 2,
                open: &none,
                now: 60,
                preview: Some((3, 2)),
                people: People::default(),
                full: false,
            };
            plan(m, missing, 60, s)
        };
        // A window starting on an ack, after messages not loaded: the ack is earlier.
        let m = msgs(&[("qa", "copy", 0), ("dev-1", "hello", 60)]);
        let rows = preview(&m, 5);
        assert_eq!(rows[0], Row::Earlier { n: 6, loaded: true });
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Name { .. }))
                .count(),
            1
        );
        // With the thread's start loaded, an ack that opens it is a bubble.
        assert_eq!(
            preview(&m, 0)
                .iter()
                .filter(|r| matches!(r, Row::Name { .. }))
                .count(),
            2
        );
        // Only tower's lines: the last two show, with no bubble.
        let m = msgs(&[
            ("system", "first", 0),
            ("system", "second", 60),
            ("system", "third", 120),
        ]);
        let rows = preview(&m, 4);
        assert_eq!(rows[0], Row::Earlier { n: 5, loaded: true });
        let events: Vec<&str> = (rows.iter())
            .filter_map(|r| match r {
                Row::Event { piece, .. } => Some(piece.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(events, ["second", "third"]);
    }

    #[test]
    fn a_message_one_line_past_the_clip_shows_whole_and_two_past_folds() {
        let line = |n: usize| {
            (0..n)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for (n, folded) in [(5, false), (6, true)] {
            let m = msgs(&[("dev-1", &line(n), 0)]);
            let rows = drawn(&m, 80, 60, People::default());
            assert_eq!(rows.iter().any(|r| r.contains("read more")), folded, "{n}");
            let shown = rows.iter().filter(|r| r.contains("line ")).count();
            assert_eq!(shown, if folded { 4 } else { n }, "{n}");
        }
    }

    #[test]
    fn a_preview_drops_only_the_orphan_acks_and_never_shows_an_empty_box() {
        let none = BTreeSet::new();
        let preview = |m: &[Message], missing| {
            let s = Shape {
                clip: 2,
                open: &none,
                now: 180,
                preview: Some((1, 2)),
                people: People::default(),
                full: false,
            };
            plan(m, missing, 60, s)
        };
        let texts = |rows: &[Row]| -> Vec<String> {
            (rows.iter())
                .filter_map(|r| match r {
                    Row::Text { piece, .. } => Some(piece.clone()),
                    Row::Ack { piece, .. } => Some(piece.clone()),
                    _ => None,
                })
                .collect()
        };
        // "dev-1: ok" opens the window, then dev-1's report: the ack goes, the report stays.
        let m = msgs(&[("dev-1", "ok", 0), ("dev-1", "Report: all green", 60)]);
        let rows = preview(&m, 3);
        assert_eq!(rows[0], Row::Earlier { n: 4, loaded: true });
        assert_eq!(texts(&rows), ["Report: all green"]);
        // A window of acks only (a resolved thread's last four): they show, not an empty box.
        let m = msgs(&[
            ("qa", "copy", 0),
            ("dev-1", "ok", 60),
            ("senior", "thanks", 120),
            ("qa", "copy", 180),
        ]);
        let rows = preview(&m, 10);
        assert!(!texts(&rows).is_empty(), "{rows:#?}");
        assert!(
            rows.iter().any(|r| matches!(r, Row::Name { .. })),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_first_lines_tag_after_an_indent_wider_than_the_bubble_stays_plain() {
        // At chat 40 an indent of 44 spaces is wider than the bubble's text, so it hangs on
        // a blank row and the next piece starts with the tag. The copy tones the raw text,
        // which starts with spaces: both draw plain, as the name lines show no tag. The
        // control, with no indent, is toned.
        let indent = " ".repeat(44);
        let m = msgs(&[
            (
                "dev-2",
                &format!("{indent}blocked: the press is jammed again"),
                0,
            ),
            (
                "dev-1",
                &format!("{indent}decision: the rye goes in first. @manager for your awareness"),
                60,
            ),
            ("qa", "blocked: the oven door sticks", 120),
        ]);
        let none = BTreeSet::new();
        let rows = plan(&m, 0, 40, shape(&none, 180, People::default()));
        let ctx = Ctx {
            msgs: &m,
            people: People::default(),
            now: 180,
            depth: Depth::TrueColor,
            selected: None,
        };
        let drawn = lines(&rows, 40, &ctx);
        for (start, tag, toned) in [
            ("blocked: the press", "blocked:", false),
            ("decision: the rye", "decision:", false),
            ("blocked: the oven", "blocked:", true),
        ] {
            let line = (drawn.iter())
                .find(|l| l.to_string().contains(start))
                .unwrap_or_else(|| panic!("{start}: {drawn:#?}"));
            let span = (line.spans.iter())
                .find(|s| s.content.starts_with(tag))
                .unwrap_or_else(|| panic!("{start}: {line:?}"));
            let bold = span.style.add_modifier.contains(Modifier::BOLD);
            assert_eq!(bold, toned, "{start}: {line:?}");
        }
    }

    #[test]
    fn a_selected_question_keeps_its_red_edge_on_the_selected_background() {
        let m = msgs(&[
            ("dev-1", "question: @manager approve?", 0),
            ("qa", "plain words here", 60),
        ]);
        let none = BTreeSet::new();
        let rows = plan(&m, 0, 80, shape(&none, 120, People::default()));
        let d = Depth::TrueColor;
        for (selected, edge) in [(0, bad(d)), (1, accent(d))] {
            let ctx = Ctx {
                msgs: &m,
                people: People::default(),
                now: 120,
                depth: d,
                selected: Some(selected),
            };
            let text = ["question:", "plain words"][selected];
            let line = (lines(&rows, 80, &ctx).into_iter())
                .find(|l| l.to_string().contains(text))
                .unwrap();
            let mark = line.spans.iter().find(|s| s.content == "▌").unwrap();
            assert_eq!(mark.style.fg, Some(edge), "{text}");
            assert_eq!(mark.style.bg, Some(selected_bg(d)), "{text}");
        }
    }

    #[test]
    fn the_left_herdr_line_opens_its_full_text_with_how_to_clear_it() {
        let text = "@kids-lead #1 is in progress but its worker dev-1 is gone from herdr (no \
                    live agent by that name); if it was closed: tower task unassign 1, or set \
                    the next worker.";
        let m = msgs(&[("system", text, 0)]);
        let people = People::default();
        let shut = drawn(&m, 100, 30, people).join("\n");
        assert!(
            shut.contains("dev-1 left herdr while on #1 · details"),
            "{shut}"
        );
        assert!(!shut.contains("tower task unassign"), "{shut}");
        let open = BTreeSet::from([1]);
        let rows = plan(&m, 0, 100, shape(&open, 30, people));
        assert!(
            rows.iter()
                .any(|r| matches!(r, Row::Failure { link, .. } if link == "hide details"))
        );
        assert!(rows.iter().any(|r| r.toggles() == Some(0)));
        let ctx = Ctx {
            msgs: &m,
            people,
            now: 30,
            depth: Depth::Ansi256,
            selected: None,
        };
        let all: String = lines(&rows, 100, &ctx)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert!(all.contains("hide details"), "{all}");
        assert!(all.contains("tower task unassign 1"), "{all}");
    }

    #[test]
    fn an_open_failures_details_keep_a_lines_indent_and_runs_of_spaces() {
        let text = "@manager could not deliver to 2 recipients; their posts stay on the \
                    board:\n- qa-1:  pane   closed\n    exit  1, see the watch's log\n- dev-4: \
                    herdr has no agent named dev-4     twice";
        let m = msgs(&[("system", text, 0)]);
        let open = BTreeSet::from([1]);
        // 44 wide: the details take the events' 40 columns.
        let rows = plan(&m, 0, 44, shape(&open, 30, People::default()));
        let got: Vec<&str> = (rows.iter())
            .filter_map(|r| match r {
                Row::Detail { piece, .. } => Some(piece.as_str()),
                _ => None,
            })
            .collect();
        // As the copy's pre-wrap details: each line's indent and runs stay, and a line that
        // breaks at a run drops it, so the next line starts with its word.
        assert_eq!(
            got,
            [
                "@manager could not deliver to 2",
                "recipients; their posts stay on the",
                "board:",
                "- qa-1:  pane   closed",
                "    exit  1, see the watch's log",
                "- dev-4: herdr has no agent named dev-4",
                "twice",
            ]
        );
    }

    /// An open failure drawn `w` wide: its line, then its details as drawn.
    fn opened(text: &str, w: usize) -> (String, Vec<String>) {
        let m = msgs(&[("system", text, 0)]);
        let open = BTreeSet::from([1]);
        let rows = plan(&m, 0, w, shape(&open, 30, People::default()));
        let ctx = Ctx {
            msgs: &m,
            people: People::default(),
            now: 30,
            depth: Depth::Ansi256,
            selected: None,
        };
        let drawn: Vec<String> = (lines(&rows, w, &ctx).iter())
            .map(|l| l.to_string().trim_end().to_string())
            .collect();
        let at = (drawn.iter())
            .position(|l| l.contains("· hide details"))
            .unwrap_or_else(|| panic!("{drawn:#?}"));
        let details = drawn[at + 1..].iter().take_while(|l| !l.is_empty());
        (drawn[at].clone(), details.cloned().collect())
    }

    /// The column a drawn line's text starts at.
    fn start(line: &str) -> usize {
        line.chars().take_while(|&c| c == ' ').count()
    }

    #[test]
    fn an_open_failures_details_sit_left_aligned_in_its_centred_event_box() {
        // Narrower than the failure's line (38 wide): the box fits that line, so the details
        // start where it does.
        let (line, details) = opened("@manager could not deliver to qa-1: x", 105);
        assert_eq!(line.trim(), "delivery failed to qa-1 · hide details");
        assert_eq!((start(&line), details.len()), (33, 1));
        assert_eq!(start(&details[0]), 33);
        assert_eq!(details[0].trim(), "@manager could not deliver to qa-1: x");
        // A line's trailing spaces widen the box, as the copy's pre-wrap shrink-to-fit counts
        // them: 41 columns and 20 spaces make it 61 wide (22.0 in headless Chrome), though the
        // drawn line drops them.
        let text = format!(
            "@manager could not deliver to qa-1: gone.{}\n- exit 1",
            " ".repeat(20)
        );
        let (_, details) = opened(&text, 105);
        assert_eq!(
            details,
            [
                format!(
                    "{}@manager could not deliver to qa-1: gone.",
                    " ".repeat(22)
                ),
                format!("{}- exit 1", " ".repeat(22))
            ]
        );
        // A wider line of the text sets the box: 73 columns, centred, every line from its
        // left edge, an indented one keeping its indent there.
        let text = "@manager could not deliver to 1 recipient; their posts stay on the board:\n\
                    - qa-1: pane closed\n  exit 1";
        let (_, details) = opened(text, 105);
        let starts: Vec<usize> = details.iter().map(|d| start(d)).collect();
        assert_eq!(starts, [16, 16, 18], "{details:#?}");
        assert_eq!(details[0].chars().count(), 16 + 73);
        // Wider than EVENT_MAX: the box takes its 80 columns, centred, and the text wraps there.
        let long = format!("@manager could not deliver to qa-1: {}", "word ".repeat(30));
        let (_, details) = opened(long.trim_end(), 160);
        assert!(details.len() > 1, "{details:#?}");
        for d in &details {
            assert_eq!(start(d), 40, "{details:#?}");
            assert!(d.chars().count() - 40 <= EVENT_MAX, "{details:#?}");
        }
        assert_eq!(details[0].chars().count(), 40 + EVENT_MAX, "{details:#?}");
        // In a narrow chat the box is the events' width, two columns in from each edge.
        let (_, details) = opened(long.trim_end(), 44);
        assert!(details.iter().all(|d| start(d) == 2), "{details:#?}");
        assert!(
            details.iter().all(|d| d.chars().count() <= 42),
            "{details:#?}"
        );
    }

    #[test]
    fn the_left_herdr_flags_details_sit_in_the_same_box() {
        let text = "@kids-lead #1 is in progress but its worker dev-1 is gone from herdr (no \
                    live agent by that name); if it was closed: tower task unassign 1, or set \
                    the next worker.";
        // One line wider than EVENT_MAX: the box takes its 80 columns, centred in the 120.
        let (line, details) = opened(text, 120);
        assert_eq!(line.trim(), "dev-1 left herdr while on #1 · hide details");
        assert!(details.len() > 1, "{details:#?}");
        assert!(details.iter().all(|d| start(d) == 20), "{details:#?}");
    }

    /// The failure `text`'s line drawn, `open` or shut, in a chat `ew + 4` wide, whose
    /// events take `ew` columns: each of its lines with the column it starts at.
    fn failure_lines(text: &str, open: bool, ew: usize) -> Vec<(usize, String)> {
        let w = ew + 4;
        let m = msgs(&[("system", text, 0)]);
        let shown = if open {
            BTreeSet::from([1])
        } else {
            BTreeSet::new()
        };
        let rows = plan(&m, 0, w, shape(&shown, 30, People::default()));
        let ctx = Ctx {
            msgs: &m,
            people: People::default(),
            now: 30,
            depth: Depth::Ansi256,
            selected: None,
        };
        (rows.iter())
            .filter(|r| matches!(r, Row::Failure { .. }))
            .map(|r| line(r, w, &ctx).to_string())
            .map(|l| (start(&l), l.trim().to_string()))
            .collect()
    }

    #[test]
    fn an_ideographic_space_ending_a_line_never_moves_a_centred_or_right_aligned_line() {
        // As headless Chrome draws the copy's `.evt` and an owner's `.ack`, a U+3000 two
        // columns wide: one ending a line hangs past its end whether it fits or not, so the
        // line sits where the text before it alone would.
        let none = BTreeSet::new();
        let drawn_rows = |m: &[Message], w: usize, keep: fn(&Row) -> bool| -> Vec<String> {
            let rows = plan(m, 0, w, shape(&none, 120, People::default()));
            let ctx = Ctx {
                msgs: m,
                people: People::default(),
                now: 120,
                depth: Depth::Ansi256,
                selected: None,
            };
            (rows.iter())
                .filter(|r| keep(r))
                .map(|r| line(r, w, &ctx).to_string())
                .collect()
        };
        let at = |l: &String| (start(l), l.trim().to_string());
        let event = |r: &Row| matches!(r, Row::Event { .. });
        // An event in a 10-column box, in a chat of 14: Chrome puts both lines at 1.
        let m = msgs(&[("system", "確認です　了解です", 0)]);
        let got: Vec<(usize, String)> = drawn_rows(&m, 14, event).iter().map(at).collect();
        assert_eq!(got, [(3, "確認です".into()), (3, "了解です".into())]);
        // The issue's, in a 47-column box, in a chat of 51: Chrome puts both at 1.5.
        let (a, i) = ("あ".repeat(22), "い".repeat(22));
        let m = msgs(&[("system", &format!("{a}　{i}"), 0)]);
        let got: Vec<(usize, String)> = drawn_rows(&m, 51, event).iter().map(at).collect();
        assert_eq!(got, [(3, a), (3, i)]);
        // A delivery failure's line: Chrome puts the line ending in one at 1, as the
        // text before it alone; its lines in a chat `ew + 4` wide.
        let failed = "@manager could not deliver to 確認です　了解です: gone.";
        assert_eq!(
            failure_lines(failed, false, 10),
            [
                (3, "delivery".into()),
                (2, "failed to".into()),
                (3, "確認です".into()),
                (2, "了解です ·".into()),
                (3, "details".into()),
            ]
        );
        // An owner's ack, its bubble 24 wide at the right of a chat of 29, its lines 22: Chrome
        // ends each line's text at the bubble's edge, the first's before its U+3000.
        let m = msgs(&[
            ("manager", "go ahead", 0),
            ("dev-1", "ok 確認です　了解です", 60),
        ]);
        let acks = drawn_rows(&m, 29, |r| matches!(r, Row::Ack { .. }));
        let ends: Vec<(String, usize)> = (acks.iter())
            .map(|l| (l.trim().to_string(), start(l) + l.trim().width()))
            .collect();
        assert_eq!(
            ends,
            [
                ("↳ dev-1: ok 確認です".into(), 27),
                ("了解です · 1m".into(), 27)
            ]
        );
        // A space after the run hangs with it: a word ending with a U+3000, then a space, fits
        // at the end of the first line, and Chrome ends the text before the run at the edge.
        // The bubble 25 wide in a chat of 30, its lines 23, ends at 28.
        let m = msgs(&[
            ("manager", "check rollout 4", 0),
            ("dev-1", "ok ああああ　 いいいいいいいい", 60),
        ]);
        let acks = drawn_rows(&m, 30, |r| matches!(r, Row::Ack { .. }));
        let ends: Vec<(String, usize)> = (acks.iter())
            .map(|l| (l.trim().to_string(), start(l) + l.trim().width()))
            .collect();
        assert_eq!(
            ends,
            [
                ("↳ dev-1: ok ああああ".into(), 28),
                ("いいいいいいいい · 1m".into(), 28)
            ]
        );
    }

    #[test]
    fn an_ack_starts_its_next_line_with_the_run_of_ideographic_spaces_it_carries() {
        // As headless Chrome draws the copy's `.ack` 36 wide: a word that is only a U+3000,
        // after a space, does not end the first line, but starts the next with the word
        // after it, under the owner's bubble (at the right, each line ending at the edge)
        // and under anyone else's (at the left). Chrome's lines: "↳ dev-7: ok ab あああああ
        // ああああ" / "　 いいいい · 1m". The bubble 38 wide in a chat of 45, its ack's
        // lines 36.
        let none = BTreeSet::new();
        let text = "ok ab あああああああああ 　 いいいい";
        for (author, owner) in [("manager", true), ("qa-2", false)] {
            let m = msgs(&[(author, "go ahead", 0), ("dev-7", text, 60)]);
            let rows = plan(&m, 0, 45, shape(&none, 120, People::default()));
            let ctx = Ctx {
                msgs: &m,
                people: People::default(),
                now: 120,
                depth: Depth::Ansi256,
                selected: None,
            };
            let got: Vec<(usize, String)> = (rows.iter())
                .filter(|r| matches!(r, Row::Ack { .. }))
                .map(|r| line(r, 45, &ctx).to_string())
                .map(|l| (start(&l), l.trim_matches(' ').to_string()))
                .collect();
            let (first, second) = if owner { (10, 27) } else { (3, 3) };
            assert_eq!(
                got,
                [
                    (first, "↳ dev-7: ok ab あああああああああ".into()),
                    (second, "　 いいいい · 1m".into())
                ],
                "under {author}'s bubble"
            );
        }
    }

    #[test]
    fn a_tagged_events_rest_keeps_every_run_of_a_chain_its_first_line_carries() {
        // A chain of words that are only U+3000s, after the first piece: the rest starts
        // with the whole chain, wrapped at its own width as an untagged event's lines are
        // (`wrap_event_line`), so no run is lost where the first piece's lines left one alone
        // on a line.
        let texts = |text: &str, first: usize, w: usize| -> Vec<String> {
            (pieces(text, first, w).into_iter())
                .map(|(s, _)| s)
                .collect()
        };
        assert_eq!(
            wrap_event_line("ab 　 　 cd ef", 6),
            ["ab", "　", "　 cd", "ef"]
        );
        assert_eq!(texts("ab 　 　 cd ef", 6, 20), ["ab", "　 　 cd ef"]);
        assert_eq!(texts("ab 　 　 cd ef", 6, 7), ["ab", "　", "　 cd", "ef"]);
    }

    #[test]
    fn an_events_or_an_acks_word_too_wide_for_a_line_ends_its_last_line() {
        // The copy's `.evt` and `.ack` leave `overflow-wrap` normal: Chrome lets a word too
        // wide for a line overflow it and starts what follows on the next line. Here the
        // word breaks where its lines end, and the next word starts a line of its own.
        let none = BTreeSet::new();
        let rows = |m: &[Message], w: usize| plan(m, 0, w, shape(&none, 60, People::default()));
        let events = |m: &[Message], w: usize| -> Vec<String> {
            (rows(m, w).into_iter())
                .filter_map(|r| match r {
                    Row::Event { piece, .. } => Some(piece),
                    _ => None,
                })
                .collect()
        };
        // An event 8 columns wide, in a chat of 12.
        let m = msgs(&[("system", "ab abcdefghij cd", 0)]);
        assert_eq!(events(&m, 12), ["ab", "abcdefgh", "ij", "cd"]);
        // A tagged one: its first line at what the tag leaves, the rest at 20, in a chat of 24.
        let m = msgs(&[("system", "decision: ab a1b2c3d4e5f6a7b8c9d0e1f2 cd", 0)]);
        assert_eq!(
            events(&m, 24),
            ["decision:", "ab", "a1b2c3d4e5f6a7b8c9d0", "e1f2", "cd"]
        );
        // An ack under a bubble 25 wide, in a chat of 30: lines 23 wide, the last's piece
        // without its age. Posted in the same minute as the message, so no hour line parts
        // them.
        let m = msgs(&[
            ("dev-1", "Report", 0),
            ("qa", "copy a1b2c3d4e5f6a7b8c9d0e1f2a ok", 0),
        ]);
        let acks: Vec<String> = (rows(&m, 30).into_iter())
            .filter_map(|r| match r {
                Row::Ack { piece, .. } => Some(piece),
                _ => None,
            })
            .collect();
        assert_eq!(
            acks,
            ["↳ qa: copy", "a1b2c3d4e5f6a7b8c9d0e1f", "2a", "ok · "]
        );
    }

    #[test]
    fn a_failures_line_wraps_where_the_copys_event_box_does() {
        let failed = "@manager could not deliver to 3 recipients; their posts stay on the \
                      board:\n- qa-1: gone\n- dev-2: gone\n- dev-3: gone";
        let left = "@kids-lead #1 is in progress but its worker dev-1 is gone from herdr (no \
                    live agent by that name); if it was closed: tower task unassign 1, or set \
                    the next worker.";
        let named = "@manager could not deliver to senior-reviewer-2: gone. Its posts stay on \
                     the board.";
        let hashed = "@manager could not deliver to a1b2c3d4e5f6a7b8c9d0e1f2: gone. Its posts \
                      stay on the board.";
        // Where headless Chrome breaks the copy's `.evt` line in a box `ew` ch wide: its words
        // as an event's, and its link whole, ending the last line when it fits there.
        let cases: [(&str, bool, usize, &[&str]); 12] = [
            (
                failed,
                false,
                26,
                &["delivery failed to 3", "recipients · details"],
            ),
            (
                failed,
                false,
                36,
                &["delivery failed to 3 recipients ·", "details"],
            ),
            (
                failed,
                true,
                24,
                &["delivery failed to 3", "recipients ·", "hide details"],
            ),
            (
                failed,
                true,
                25,
                &["delivery failed to 3", "recipients · hide details"],
            ),
            (
                failed,
                true,
                31,
                &["delivery failed to 3 recipients", "· hide details"],
            ),
            (
                failed,
                true,
                47,
                &["delivery failed to 3 recipients · hide details"],
            ),
            (
                left,
                false,
                14,
                &["dev-1 left", "herdr while on", "#1 · details"],
            ),
            (
                left,
                false,
                28,
                &["dev-1 left herdr while on #1", "· details"],
            ),
            // A name breaks after a `-`.
            (
                named,
                false,
                27,
                &["delivery failed to senior-", "reviewer-2 · details"],
            ),
            (
                named,
                false,
                14,
                &[
                    "delivery",
                    "failed to",
                    "senior-",
                    "reviewer-2 ·",
                    "details",
                ],
            ),
            // A name too wide for a line starts on a line of its own; Chrome lets it overflow
            // that line and starts what follows on the next, so it breaks where its lines end
            // and the next line starts after it.
            (
                hashed,
                false,
                14,
                &[
                    "delivery",
                    "failed to",
                    "a1b2c3d4e5f6a7",
                    "b8c9d0e1f2",
                    "· details",
                ],
            ),
            // A link wider than a line breaks inside too.
            (
                failed,
                true,
                11,
                &[
                    "delivery",
                    "failed to 3",
                    "recipients",
                    "·",
                    "hide",
                    "details",
                ],
            ),
        ];
        for (text, open, ew, want) in cases {
            let got = failure_lines(text, open, ew);
            let said: Vec<&str> = got.iter().map(|(_, l)| l.as_str()).collect();
            assert_eq!(said, want, "{ew} wide, open {open}");
            // Each line centred in the chat, as the copy's `text-align: center` puts it.
            for (at, l) in &got {
                assert_eq!(*at, (ew + 4 - l.width()) / 2, "{l:?}, {ew} wide");
            }
        }
        // Every line of it opens and folds it; only its first starts it, for the keys.
        let m = msgs(&[("system", failed, 0)]);
        let none = BTreeSet::new();
        let rows = plan(&m, 0, 30, shape(&none, 30, People::default()));
        let failure: Vec<&Row> = (rows.iter())
            .filter(|r| matches!(r, Row::Failure { .. }))
            .collect();
        assert_eq!(failure.len(), 2, "{rows:#?}");
        assert!(failure.iter().all(|r| r.toggles() == Some(0)), "{rows:#?}");
        let starts: Vec<Option<usize>> = failure.iter().map(|r| r.starts()).collect();
        assert_eq!(starts, [Some(0), None]);
    }
}
