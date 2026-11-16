//! A URL in a message as one terminal hyperlink (OSC 8) however it wraps.
//!
//! The frame is drawn as plain text. Each pane that draws bubbles says, line by line, which
//! URL a line shows a piece of; `placed` turns that into the cells the piece was drawn in
//! (the line's underlined span, as `chat` styles a URL), and `hyperlink` wraps exactly those
//! cells in the link, so one URL never claims another's cells, whatever their text. Every
//! piece of one URL carries the same link `id`, so a terminal that supports OSC 8 treats them
//! as one link.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU16;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// A piece of a URL where the frame drew it: `width` cells from (`x`, `y`), and the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub url: String,
}

/// The pieces of URLs that `lines` show when drawn in `area` from line `scroll` on (as
/// `render_scrolled` draws them): in each line `urls` gives a URL for, the cells of its
/// underlined spans, within the area.
pub fn placed(lines: &[Line], urls: &[Option<String>], scroll: usize, area: Rect) -> Vec<Link> {
    let mut out = Vec::new();
    let shown = (lines.iter().zip(urls).enumerate())
        .skip(scroll)
        .take(usize::from(area.height));
    for (k, (line, url)) in shown {
        let Some(url) = url else { continue };
        let y = area.y + (k - scroll) as u16;
        let mut col = 0usize;
        for span in &line.spans {
            let w = span.width();
            if span.style.add_modifier.contains(Modifier::UNDERLINED) {
                let end = (col + w).min(usize::from(area.width));
                if end > col {
                    let x = area.x + col as u16;
                    let width = (end - col) as u16;
                    match out.last_mut() {
                        Some(Link {
                            x: lx,
                            y: ly,
                            width: lw,
                            url: lu,
                        }) if *ly == y && *lx + *lw == x && lu == url => *lw += width,
                        _ => out.push(Link {
                            x,
                            y,
                            width,
                            url: url.clone(),
                        }),
                    }
                }
            }
            col += w;
        }
    }
    out
}

/// Each cell of `links` in `buf` made part of its URL's link. A URL holding a control
/// character stays plain text, with no link: written into the escape sequence, it could end
/// the sequence or start another. The board refuses such a character in every post today
/// (`board::check_lines`); this does not rest on that alone.
pub fn hyperlink(buf: &mut Buffer, links: &[Link]) {
    let area = buf.area;
    for link in links {
        if link.url.chars().any(char::is_control) {
            continue;
        }
        let id = link_id(&link.url);
        for x in link.x..link.x.saturating_add(link.width) {
            if !area.contains(Position::new(x, link.y)) {
                continue;
            }
            let cell = &mut buf[(x, link.y)];
            let shown = cell.symbol().to_string();
            let width = NonZeroU16::new((shown.width() as u16).max(1)).expect("not zero");
            cell.set_symbol(&format!(
                "\x1b]8;id={id};{}\x1b\\{shown}\x1b]8;;\x1b\\",
                link.url
            ));
            cell.set_diff_option(CellDiffOption::ForcedWidth(width));
        }
    }
}

/// A link's id from its URL, so the pieces of one URL are one link to the terminal.
fn link_id(url: &str) -> String {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("ct{:x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Span;

    fn link(x: u16, y: u16, width: u16, url: &str) -> Link {
        Link {
            x,
            y,
            width,
            url: url.into(),
        }
    }

    /// The URL each linked cell of row `y` links to, by column.
    fn linked(buf: &Buffer, y: u16) -> Vec<(u16, String)> {
        (0..buf.area.width)
            .filter_map(|x| {
                let s = buf[(x, y)].symbol();
                let rest = s.strip_prefix("\x1b]8;id=")?;
                let url = rest.split_once(';')?.1.split_once("\x1b\\")?.0;
                Some((x, url.to_string()))
            })
            .collect()
    }

    #[test]
    fn a_url_with_a_control_character_stays_plain_text() {
        let plain = |text: &str| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
            buf.set_string(0, 0, text, Style::new());
            buf
        };
        for url in [
            "https://a.b/\x1b]8;;x",
            "https://a.b/\x07c",
            "https://a.b/\u{9b}c",
            "https://a.b/\nc",
        ] {
            let mut buf = plain("https://a.b/c");
            hyperlink(&mut buf, &[link(0, 0, 13, url)]);
            assert_eq!(buf, plain("https://a.b/c"), "{url:?}");
        }
        // The same link without one is linked, so the check above is not idle.
        let mut buf = plain("https://a.b/c");
        hyperlink(&mut buf, &[link(0, 0, 13, "https://a.b/c")]);
        assert_eq!(linked(&buf, 0).len(), 13);
    }

    #[test]
    fn exactly_the_cells_given_are_linked_each_to_its_own_url() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        buf.set_string(0, 0, "https://a.b/c", Style::new());
        buf.set_string(0, 1, "  https://a.b/c/d", Style::new());
        // The second URL starts with the first; each keeps its own cells.
        let (c, d) = ("https://a.b/c", "https://a.b/c/d");
        hyperlink(&mut buf, &[link(0, 0, 13, c), link(2, 1, 15, d)]);
        let row0: Vec<(u16, String)> = (0..13).map(|x| (x, c.to_string())).collect();
        let row1: Vec<(u16, String)> = (2..17).map(|x| (x, d.to_string())).collect();
        assert_eq!(linked(&buf, 0), row0);
        assert_eq!(linked(&buf, 1), row1);
        // Each cell keeps what it showed, one column wide.
        let id = link_id(d);
        assert_eq!(
            buf[(2, 1)].symbol(),
            format!("\x1b]8;id={id};{d}\x1b\\h\x1b]8;;\x1b\\")
        );
        let one = CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap());
        assert_eq!(buf[(2, 1)].diff_option, one);
        assert_ne!(link_id(c), link_id(d));
        // A link past the buffer's edge is cut there.
        hyperlink(&mut buf, &[link(18, 0, 5, c)]);
        assert_eq!(linked(&buf, 0).last(), Some(&(19, c.to_string())));
    }

    #[test]
    fn placed_finds_the_underlined_cells_of_the_lines_with_a_url() {
        let under = Style::new().add_modifier(Modifier::UNDERLINED);
        let line = |pre: &str, url: &str| {
            Line::from(vec![
                Span::raw(pre.to_string()),
                Span::styled(url.to_string(), under),
            ])
        };
        let lines = vec![
            line(" ▌ ", "https://a.b/"),
            line(" ▌   ", "c/d"),
            // Underlined, but no URL said for it: not a link.
            line(" ▌ ", "https://x.y/"),
            line(" ▌ ", "https://e.f/"),
        ];
        let urls = vec![
            Some("https://a.b/c/d".to_string()),
            Some("https://a.b/c/d".to_string()),
            None,
            Some("https://e.f/".to_string()),
        ];
        let area = Rect::new(4, 10, 14, 3);
        // From line 1 on, three lines in the area, at its left and top.
        assert_eq!(
            placed(&lines, &urls, 1, area),
            [
                link(9, 10, 3, "https://a.b/c/d"),
                link(7, 12, 11, "https://e.f/")
            ]
        );
        // Cut at the area's width: "https://e.f/" is 12 columns after 3.
        assert_eq!(placed(&lines, &urls, 1, area)[1].width, 11);
        assert_eq!(
            placed(&lines, &urls, 0, Rect::new(0, 0, 40, 1)),
            [link(3, 0, 12, "https://a.b/c/d")]
        );
    }
}
