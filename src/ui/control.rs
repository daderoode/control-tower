//! The clickable controls of the signed design's layout E, styled like links: "open repo ▶",
//! "open milestone ▶" or "open task ▶" on the overview's header, "open full board ▶" on its message board's
//! heading, "◀ close" on a detail view and "new lead (n) ▶" on the alert line. Each is
//! a key: a click on it is that key pressed, so the two can never do different things, and
//! the control names its key where there is room, as in "open task (enter) ▶".

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::dash::Input;
use super::theme::fit;

/// A control, by what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    OpenRepo,
    OpenMilestone,
    OpenTask,
    FullBoard,
    Close,
    NewLead,
}

impl Control {
    /// The key a click on it presses.
    pub fn key(self) -> Input {
        match self {
            Control::OpenRepo | Control::OpenMilestone | Control::OpenTask => Input::Enter,
            Control::FullBoard => Input::Char('b'),
            Control::Close => Input::Esc,
            Control::NewLead => Input::Char('n'),
        }
    }

    /// Its text with its key, then without it, for a narrower pane.
    pub fn texts(self) -> [&'static str; 2] {
        match self {
            Control::OpenRepo => ["open repo (enter) ▶", "open repo ▶"],
            Control::OpenMilestone => ["open milestone (enter) ▶", "open milestone ▶"],
            Control::OpenTask => ["open task (enter) ▶", "open task ▶"],
            Control::FullBoard => ["open full board (b) ▶", "open full board ▶"],
            Control::Close => ["◀ close (esc)", "◀ close"],
            Control::NewLead => ["new lead (n) ▶", "new lead ▶"],
        }
    }
}

/// Where a control is in a view: on line `line`, from column `from` up to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlAt {
    pub control: Control,
    pub line: usize,
    pub from: usize,
    pub to: usize,
}

impl ControlAt {
    /// Whether it covers column `x` of line `line`.
    pub fn covers(&self, x: usize, line: usize) -> bool {
        line == self.line && (self.from..self.to).contains(&x)
    }
}

/// The control under column `x` of line `line`, if any.
pub fn control_at(controls: &[ControlAt], x: usize, line: usize) -> Option<Control> {
    (controls.iter())
        .find(|c| c.covers(x, line))
        .map(|c| c.control)
}

/// A line exactly `w` wide: `left`, cut to fit, with `control` at the right edge and a
/// space after it, with its key while `min_left` columns of `left` still fit beside it,
/// else without, else left out. Returns the line and the columns the control takes.
pub fn with_control(
    left: Vec<Span<'static>>,
    min_left: usize,
    control: Control,
    style: Style,
    w: usize,
) -> (Line<'static>, Option<(usize, usize)>) {
    let Some(text) = (control.texts().into_iter()).find(|t| min_left + 1 + t.width() < w) else {
        return (fit(left, w), None);
    };
    let from = w - 1 - text.width();
    let mut spans = fit(left, from - 1).spans;
    spans.extend([Span::raw(" "), Span::styled(text, style), Span::raw(" ")]);
    (Line::from(spans), Some((from, w - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_control_keeps_its_key_then_drops_it_then_goes() {
        let left = || vec![Span::raw(" M1  First shop pages")];
        let c = Control::OpenTask;
        // 10 + 1 + 19 + 1 = 31 columns for the text with its key.
        let (l, at) = with_control(left(), 10, c, Style::new(), 31);
        assert_eq!(text(&l), " M1  Firs… open task (enter) ▶ ");
        assert_eq!(at, Some((11, 30)));
        let (l, at) = with_control(left(), 10, c, Style::new(), 30);
        assert_eq!(text(&l), " M1  First shop … open task ▶ ");
        assert_eq!(l.width(), 30);
        assert_eq!(at, Some((18, 29)));
        let (l, at) = with_control(left(), 10, c, Style::new(), 22);
        assert_eq!((text(&l).as_str(), at), (" M1  First shop pages ", None));
    }

    #[test]
    fn a_click_lands_only_on_the_control_columns_of_its_line() {
        let at = [ControlAt {
            control: Control::Close,
            line: 0,
            from: 5,
            to: 13,
        }];
        assert_eq!(control_at(&at, 5, 0), Some(Control::Close));
        assert_eq!(control_at(&at, 12, 0), Some(Control::Close));
        assert_eq!(control_at(&at, 4, 0), None);
        assert_eq!(control_at(&at, 13, 0), None);
        assert_eq!(control_at(&at, 6, 1), None);
    }
}
