//! Colours, styles and the one rule every line follows: exactly the pane's width.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::board::{Colour, Stage, filled_cells};

/// How many colours the terminal shows: 24-bit, or the 256-colour palette as the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Ansi256,
}

impl Depth {
    /// 24-bit only when the terminal says so in `COLORTERM`, as truecolor terminals do; the
    /// 256-colour palette otherwise, which nearly every terminal shows.
    pub fn from_colorterm(colorterm: Option<&str>) -> Depth {
        match colorterm {
            Some(v) if v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit") => {
                Depth::TrueColor
            }
            _ => Depth::Ansi256,
        }
    }
}

/// A lead's colour on this terminal: the signed design's lead colours first, then three more
/// that read as well on its dark background (#13161c, at least 7:1 contrast), in 24-bit or
/// as the nearest 256-colour stand-in.
pub fn lead_colour(c: Colour, depth: Depth) -> Color {
    let (rgb, index) = match c {
        Colour::Blue => ((0x6c, 0xb6, 0xff), 75),
        Colour::Purple => ((0xd3, 0x8a, 0xea), 176),
        Colour::Orange => ((0xf0, 0xa3, 0x5e), 215),
        Colour::Teal => ((0x4f, 0xd1, 0xc5), 80),
        Colour::Pink => ((0xf4, 0x7f, 0xb0), 211),
        Colour::Green => ((0x7c, 0xcf, 0x6a), 113),
    };
    match depth {
        Depth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        Depth::Ansi256 => Color::Indexed(index),
    }
}

/// A stage's colour, as in the signed design: Done in its green, In progress in its cyan,
/// Waiting dim.
pub fn stage_style(stage: Stage, depth: Depth) -> Style {
    match stage {
        Stage::Done => Style::new().fg(ok(depth)),
        Stage::InProgress => Style::new().fg(info(depth)),
        Stage::Waiting => dim(depth),
    }
}

/// A task's stage mark: ✓ Done, ● In progress, ○ Waiting, in the stage's colour. The green
/// also fills the progress bar.
pub fn stage_mark(stage: Stage, depth: Depth) -> Span<'static> {
    Span::styled(stage_glyph(stage), stage_style(stage, depth))
}

/// A task's stage mark without its colour: ✓ Done, ● In progress, ○ Waiting.
pub(crate) fn stage_glyph(stage: Stage) -> &'static str {
    match stage {
        Stage::Done => "✓",
        Stage::InProgress => "●",
        Stage::Waiting => "○",
    }
}

/// The signed design's "ok" green, #7bd88f.
pub fn ok(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0x7b, 0xd8, 0x8f),
        Depth::Ansi256 => Color::Indexed(114),
    }
}

/// The signed design's "info" cyan, #78dce8: In progress, and an awareness message.
pub fn info(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0x78, 0xdc, 0xe8),
        Depth::Ansi256 => Color::Indexed(116),
    }
}

/// The signed design's "bad" red, #ff6b6b: `@manager`, a message waiting on the owner, and the
/// owner's name, `manager`, in a conversation.
pub fn bad(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0xff, 0x6b, 0x6b),
        Depth::Ansi256 => Color::Indexed(203),
    }
}

/// The signed design's "warn" amber, #e6c07b: an open thread's pill, and anything blocked.
pub fn warn(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0xe6, 0xc0, 0x7b),
        Depth::Ansi256 => Color::Indexed(180),
    }
}

/// The signed design's selected-row background, #23324a.
pub fn selected_bg(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0x23, 0x32, 0x4a),
        Depth::Ansi256 => Color::Indexed(236),
    }
}

/// The signed design's accent, #6cb6ff: the border of the selected card.
pub fn accent(depth: Depth) -> Color {
    lead_colour(Colour::Blue, depth)
}

/// The reference's goal grey, #9ba1ad: dimmer than the text, readable unlike `dim`.
pub fn goal(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0x9b, 0xa1, 0xad),
        Depth::Ansi256 => Color::Indexed(248),
    }
}

/// The signed design's dim grey, #6c7280: ids, counts, labels and the bar's empty track.
pub fn dim_colour(depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb(0x6c, 0x72, 0x80),
        Depth::Ansi256 => Color::Indexed(243),
    }
}

pub(crate) fn dim(depth: Depth) -> Style {
    Style::new().fg(dim_colour(depth))
}

pub(crate) fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A progress bar of `cells`, filled in green from `percent` rounded down.
pub(crate) fn bar(percent: u8, cells: usize, depth: Depth) -> [Span<'static>; 2] {
    let filled = filled_cells(percent, cells);
    [
        Span::styled("█".repeat(filled), Style::new().fg(ok(depth))),
        Span::styled("░".repeat(cells - filled), dim(depth)),
    ]
}

/// Makes a line exactly `w` columns: cut with "…" when too long, padded with spaces when
/// short. Widths are display widths, so wide characters never spill past the edge.
pub(crate) fn fit(spans: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let total: usize = spans.iter().map(|s| s.width()).sum();
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut used = 0;
    if total <= w {
        out.extend(spans);
        used = total;
    } else if w > 0 {
        let budget = w - 1; // room for the ellipsis
        let mut last = Style::new();
        'spans: for s in spans {
            let mut kept = String::new();
            for c in s.content.chars() {
                let cw = c.width().unwrap_or(0);
                if used + cw > budget {
                    out.push(Span::styled(kept, s.style));
                    last = s.style;
                    break 'spans;
                }
                used += cw;
                kept.push(c);
            }
            last = s.style;
            out.push(Span::styled(kept, s.style));
        }
        out.push(Span::styled("…", last));
        used += 1;
    }
    if used < w {
        out.push(Span::raw(" ".repeat(w - used)));
    }
    Line::from(out)
}
