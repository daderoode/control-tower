//! The edge: the terminal, mouse capture, the input loop and the refresh thread.

use std::io::stdout;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

use super::writer::{self, Write};
use super::{Dash, Depth, Input, Mode, Reader, Request, View, draw, hyperlink, refresh};
use crate::board::{ThreadId, Ticket};

const REFRESH: Duration = Duration::from_secs(2);
/// How long the input loop waits for input before redrawing (the clock and new board data).
const TICK: Duration = Duration::from_millis(250);
/// Runs the dashboard until `q` (outside the composer) or Ctrl-C, or in a popup
/// (`TOWER_UI_MODE=popup`) Esc at its top level. It reads the board read-only; a missing or unreadable board is shown on screen and picked up once it
/// appears. Its writes run off the UI thread: the owner's post, mark read, folds and a lead's
/// start through the store (`writer`).
pub fn run(path: &Path) -> Result<()> {
    let mode = Mode::from_env(std::env::var(super::MODE_VAR).ok().as_deref())?;
    let depth = Depth::from_colorterm(std::env::var("COLORTERM").ok().as_deref());
    let first = refresh(path);
    let (tx, rx) = mpsc::channel();
    // The thread the full board shows, sent when it changes: the refresh thread reads the
    // board at once then, with that thread's whole history, and every `REFRESH` after.
    let (want_tx, want_rx) = mpsc::channel::<Option<ThreadId>>();
    let mut reader = Reader::new(path);
    std::thread::spawn(move || {
        let mut wanted = None;
        loop {
            match want_rx.recv_timeout(REFRESH) {
                Ok(w) => wanted = want_rx.try_iter().last().unwrap_or(w),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if tx.send(reader.read(wanted)).is_err() {
                return;
            }
        }
    });
    let mut terminal = ratatui::init();
    // ratatui's own panic hook restores the screen but not mouse reporting.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
        hook(info);
    }));
    let result = execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            let edge = Edge { mode, depth };
            input_loop(&mut terminal, path, first, rx, &want_tx, edge)
        });
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}

/// What `run` learnt of where it runs: the mode and the terminal's colours.
struct Edge {
    mode: Mode,
    depth: Depth,
}

fn input_loop(
    terminal: &mut ratatui::DefaultTerminal,
    path: &Path,
    mut view: View,
    rx: Receiver<View>,
    want_tx: &Sender<Option<ThreadId>>,
    edge: Edge,
) -> Result<()> {
    let Edge { mode, depth } = edge;
    let mut dash = Dash::default();
    dash.mode = mode;
    dash.set_now(chrono::Local::now().timestamp());
    // The thread last asked of the refresh thread.
    let mut asked: Option<ThreadId> = None;
    let mut writes = Writes::default();
    loop {
        let done = writes.finish(&mut dash);
        if done.board {
            view = refresh(path);
            let size = terminal.size()?;
            dash.refreshed(&view, Rect::new(0, 0, size.width, size.height));
        }
        if done.post || done.fold {
            // Read the board now, the shown thread's whole history with it, so the message
            // appears without waiting for the next refresh. A fold is read this way only:
            // the refresh thread sends its reads in order, so none read before the fold
            // can come after the read that shows it.
            let _ = want_tx.send(asked);
        }
        loop {
            match rx.try_recv() {
                Ok(v) => {
                    view = v;
                    let size = terminal.size()?;
                    dash.refreshed(&view, Rect::new(0, 0, size.width, size.height));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    view = Err("refresh stopped: the board is no longer read".into());
                    break;
                }
            }
        }
        let wanted = dash.board_thread(&view);
        if wanted != asked {
            asked = wanted;
            // A refresh thread that is gone shows as "refresh stopped" above.
            let _ = want_tx.send(wanted);
        }
        // The owner opened a thread: its awareness items are read (spec 3a), once.
        let opened = dash.opened(&view);
        if !opened.is_empty() {
            let rx = writer::spawn(path, Write::Seen(opened.clone()));
            writes.seen.push((opened, rx));
        }
        // A read that failed is tried again, never at once and one at a time.
        if let Some(ids) = writes.retry_seen(std::time::Instant::now()) {
            let rx = writer::spawn(path, Write::Seen(ids.clone()));
            writes.seen.push((ids, rx));
        }
        let size = terminal.size()?;
        let now = chrono::Local::now();
        dash.set_now(now.timestamp());
        dash.fit(&view, Rect::new(0, 0, size.width, size.height));
        let clock = now.format("%H:%M").to_string();
        terminal.draw(|f| {
            // Drawn as text, then the URLs the bubbles show made links.
            let links = draw(f, &view, &dash, &clock, depth);
            hyperlink(f.buffer_mut(), &links);
        })?;
        if !event::poll(TICK)? {
            continue;
        }
        let size = terminal.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let input = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                // Ctrl-C quits even while writing; `q` (see `Dash::handle`) only outside the
                // composer.
                if ctrl && k.code == KeyCode::Char('c') {
                    return Ok(());
                }
                match k.code {
                    KeyCode::Up => Some(Input::Up),
                    KeyCode::Down => Some(Input::Down),
                    KeyCode::Left => Some(Input::Left),
                    KeyCode::Right => Some(Input::Right),
                    KeyCode::PageUp => Some(Input::PageUp),
                    KeyCode::PageDown => Some(Input::PageDown),
                    // Alt+Enter: a line break in the draft.
                    KeyCode::Enter if k.modifiers.contains(KeyModifiers::ALT) => {
                        Some(Input::NewLine)
                    }
                    KeyCode::Enter => Some(Input::Enter),
                    KeyCode::Esc => Some(Input::Esc),
                    KeyCode::Backspace => Some(Input::Backspace),
                    // No control characters go into a draft.
                    KeyCode::Char(_) if ctrl && dash.typing(&view, area) => None,
                    KeyCode::Char(c) => Some(Input::Char(c)),
                    _ => None,
                }
            }
            // A paste goes into the draft as typed, line breaks kept, never as Enter.
            Event::Paste(text) => {
                dash.paste(&view, area, &text);
                None
            }
            Event::Mouse(m) => {
                let (x, y) = (m.column, m.row);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => Some(Input::Click { x, y }),
                    MouseEventKind::ScrollUp => Some(Input::Wheel { x, y, down: false }),
                    MouseEventKind::ScrollDown => Some(Input::Wheel { x, y, down: true }),
                    _ => None,
                }
            }
            _ => None,
        };
        // Off the UI thread, so the dashboard keeps drawing while tower runs.
        // A mark read refreshes the board when done; a post updates its draft.
        match input.and_then(|i| dash.handle(&view, area, i)) {
            None => {}
            Some(Request::Quit) => return Ok(()),
            // The ticket opens in the browser with `open`, waited for off this thread.
            Some(Request::Open(link)) => writes.open.push(open_link("open", link)),
            Some(Request::Post { thread, text }) => {
                writes.post = Some(writer::spawn(path, Write::Post { thread, text }));
            }
            Some(Request::MarkRead { id, item }) => {
                writes.mark = Some(writer::spawn(path, Write::MarkRead { id, item }))
            }
            Some(Request::Clear { id, item }) => {
                writes.mark = Some(writer::spawn(path, Write::Clear { id, item }))
            }
            Some(Request::Fold {
                repos,
                milestones,
                done_groups,
                folded,
            }) => {
                let fold = Write::Fold {
                    repos,
                    milestones,
                    done_groups,
                    folded,
                };
                writes.fold.push(writer::spawn(path, fold));
            }
            Some(Request::StartLead { line, repo }) => {
                let terminal = crate::terminal::exe_from_env();
                let start = Write::start_lead_from_env(line, repo, terminal, std::env::var_os);
                writes.start = Some(writer::spawn(path, start))
            }
        }
    }
}

/// What the keys line says once a read that failed has worked.
const READ_NOW: &str = "the opened thread's items are read now";

/// An opened thread's read in flight: the items it reads and how it went.
type SeenRead = (Vec<i64>, Receiver<(String, bool)>);

/// The writes in flight, each tracked on its own so one finishing never drops another: a
/// mark read, the awareness items of an opened thread read, a lead started
/// (the store and the terminal), a post (the store) and folds (the store), as many as the owner
/// asks for.
#[derive(Default)]
struct Writes {
    mark: Option<Receiver<(String, bool)>>,
    /// Each opened thread's read, with the items it reads.
    seen: Vec<SeenRead>,
    /// The items whose read failed, and when they are tried again.
    unseen: Vec<i64>,
    unseen_at: Option<std::time::Instant>,
    /// What the last failed read said on the keys line, until a read works again.
    unseen_said: Option<String>,
    start: Option<Receiver<(String, bool)>>,
    post: Option<Receiver<(String, bool)>>,
    fold: Vec<Receiver<(String, bool)>>,
    /// The tickets being opened in the browser.
    open: Vec<Receiver<(String, bool)>>,
}

/// What `Writes::finish` found finished: a mark read, a lead started or an opened thread's items
/// read (the board needs reading again), a post, and a fold.
#[derive(Debug, Default, PartialEq, Eq)]
struct Finished {
    board: bool,
    post: bool,
    fold: bool,
}

impl Writes {
    /// The items to read again now, at `now`, if a read failed.
    fn retry_seen(&mut self, now: std::time::Instant) -> Option<Vec<i64>> {
        let due = self.unseen_at.is_some_and(|at| at <= now);
        if !due || !self.seen.is_empty() || self.unseen.is_empty() {
            return None;
        }
        self.unseen_at = None;
        Some(std::mem::take(&mut self.unseen))
    }

    /// Takes in every write that finished since the last look: the keys line shows each
    /// one's outcome, joined with " · " when several finish at once (red when any failed);
    /// a post's outcome also settles its draft. The rest stay in flight.
    fn finish(&mut self, dash: &mut Dash) -> Finished {
        let mut said: Vec<(String, bool)> = Vec::new();
        if let Some(s) = self.mark.as_ref().and_then(writer::poll) {
            self.mark = None;
            said.push(s);
        }
        if let Some(s) = self.start.as_ref().and_then(writer::poll) {
            self.start = None;
            said.push(s);
        }
        let mut board = !said.is_empty();
        // Reading an opened thread's items says nothing unless it fails.
        // One that failed is tried again after a refresh's while: the thread stays
        // open, so it would otherwise wait until the owner opened the thread again.
        let mut unseen = Vec::new();
        let mut seen_ok = false;
        let mut seen_failed: Option<String> = None;
        self.seen.retain(|(ids, rx)| match writer::poll(rx) {
            Some(s) => {
                board = true;
                if s.1 {
                    seen_failed = Some(s.0.clone());
                    said.push(s);
                    unseen.extend(ids);
                } else {
                    seen_ok = true;
                }
                false
            }
            None => true,
        });
        if !unseen.is_empty() {
            for id in unseen {
                if !self.unseen.contains(&id) {
                    self.unseen.push(id);
                }
            }
            self.unseen_at = Some(std::time::Instant::now() + REFRESH);
        }
        if board && self.mark.is_none() && self.start.is_none() {
            dash.handing = None;
        }
        // A fold says nothing when it works: the tree shows it once a refresh reads it. A
        // refusal shows, and ends an unfold the owner asked for by picking a folded task.
        let before = self.fold.len();
        self.fold.retain(|rx| match writer::poll(rx) {
            Some(s) => {
                if s.1 {
                    dash.unfolding = None;
                    said.push(s);
                }
                false
            }
            None => true,
        });
        let fold = self.fold.len() < before;
        // An open says nothing when it works ("opening #42 in the browser" stays); a
        // failure shows.
        self.open.retain(|rx| match writer::poll(rx) {
            Some(s) => {
                if s.1 {
                    said.push(s);
                }
                false
            }
            None => true,
        });
        let post = self.post.as_ref().and_then(writer::poll);
        if let Some(s) = &post {
            self.post = None;
            dash.posted(s.clone());
            said.push(s.clone());
        }
        if !said.is_empty() {
            let text = (said.iter())
                .map(|(t, _)| t.as_str())
                .filter(|t| !t.is_empty());
            let text = text.collect::<Vec<_>>().join(" · ");
            dash.notice = Some((text, said.iter().any(|(_, failed)| *failed)));
        }
        // A read that works after one failed: the failure it left on the keys line is
        // over, so it says so, unless something newer shows there.
        if let Some(failed) = seen_failed {
            self.unseen_said = Some(failed);
        } else if seen_ok
            && let Some(failed) = self.unseen_said.take()
            && dash.notice == Some((failed, true))
        {
            dash.notice = Some((READ_NOW.into(), false));
        }
        Finished {
            board,
            post: post.is_some(),
            fold,
        }
    }
}

/// Runs `program link` (`open`, which hands the link to the browser) on its own thread and
/// waits for it, so it never lingers unreaped; the returned channel says how it went: nothing
/// to say when it worked, "could not open #42: open exited 1" when it did not.
fn open_link(program: &'static str, link: String) -> Receiver<(String, bool)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let status = std::process::Command::new(program)
            .arg(&link)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let ticket = match Ticket::of(&link) {
            Ticket::Link => link.clone(),
            t => t.to_string(),
        };
        let said = match status {
            Ok(s) if s.success() => (String::new(), false),
            Ok(s) => match s.code() {
                Some(code) => (
                    format!("could not open {ticket}: {program} exited {code}"),
                    true,
                ),
                None => (
                    format!("could not open {ticket}: {program} was stopped"),
                    true,
                ),
            },
            Err(e) => (format!("could not open {ticket}: {e}"), true),
        };
        let _ = tx.send(said);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::MilestoneId;

    /// An `open` is waited for, and only its failure shows, naming the ticket.
    #[test]
    fn an_opened_ticket_is_waited_for_and_only_a_failure_shows() {
        let wait = |rx: Receiver<(String, bool)>| rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let link = "https://github.com/example/bookshop/issues/42".to_string();
        assert_eq!(
            wait(open_link("true", link.clone())),
            (String::new(), false)
        );
        assert_eq!(
            wait(open_link("false", link.clone())),
            ("could not open #42: false exited 1".into(), true)
        );
        let other = "https://example.com/notes/7".to_string();
        assert_eq!(
            wait(open_link("false", other)),
            (
                "could not open https://example.com/notes/7: false exited 1".into(),
                true
            )
        );
        let (said, failed) = wait(open_link("no-such-program-for-tower", link));
        assert!(failed && said.starts_with("could not open #42: "), "{said}");
        // `finish` shows the failure and keeps quiet about a success.
        let mut writes = Writes::default();
        let mut dash = Dash::default();
        dash.notice = Some(("opening #42 in the browser".into(), false));
        let (ok_tx, ok) = mpsc::channel();
        let (bad_tx, bad) = mpsc::channel();
        writes.open = vec![ok, bad];
        ok_tx.send((String::new(), false)).unwrap();
        writes.finish(&mut dash);
        assert_eq!(writes.open.len(), 1);
        assert_eq!(
            dash.notice,
            Some(("opening #42 in the browser".into(), false))
        );
        bad_tx
            .send(("could not open #42: open exited 1".into(), true))
            .unwrap();
        writes.finish(&mut dash);
        assert!(writes.open.is_empty());
        assert_eq!(
            dash.notice,
            Some(("could not open #42: open exited 1".into(), true))
        );
    }

    #[test]
    fn a_finishing_start_keeps_the_mark_read_and_post_in_flight_and_each_outcome_shows() {
        let (start_tx, start) = mpsc::channel();
        let (mark_tx, mark) = mpsc::channel();
        let (post_tx, post) = mpsc::channel();
        let mut writes = Writes {
            mark: Some(mark),
            seen: Vec::new(),
            unseen: Vec::new(),
            unseen_at: None,
            unseen_said: None,
            start: Some(start),
            post: Some(post),
            fold: Vec::new(),
            open: Vec::new(),
        };
        let mut dash = Dash::default();
        dash.handing = Some("starting a lead…".into());
        // Nothing finished: nothing changes.
        assert_eq!(writes.finish(&mut dash), Finished::default());
        assert_eq!(dash.notice, None);
        // The start finishes: the mark read and the post stay in flight.
        start_tx
            .send(("started M2's lead in Rex".into(), false))
            .unwrap();
        let done = writes.finish(&mut dash);
        assert_eq!((done.board, done.post), (true, false));
        assert!(writes.start.is_none() && writes.mark.is_some() && writes.post.is_some());
        assert_eq!(
            dash.notice,
            Some(("started M2's lead in Rex".into(), false))
        );
        assert!(dash.handing.is_some(), "the mark read still runs");
        // The mark read and a failed post finish at once: both show, in red.
        mark_tx
            .send((
                "marked read: qa in ‘New-book ribbon’ · storefront".into(),
                false,
            ))
            .unwrap();
        post_tx.send(("T3 is resolved".into(), true)).unwrap();
        let done = writes.finish(&mut dash);
        assert_eq!((done.board, done.post), (true, true));
        assert_eq!(
            dash.notice,
            Some((
                "marked read: qa in ‘New-book ribbon’ · storefront · T3 is resolved".into(),
                true
            ))
        );
        assert_eq!(dash.handing, None);
        assert!(writes.mark.is_none() && writes.post.is_none());
    }

    #[test]
    fn an_opened_threads_read_refreshes_the_board_and_says_only_a_failure() {
        let (tx, seen) = mpsc::channel();
        let mut writes = Writes {
            seen: vec![(vec![1], seen)],
            ..Writes::default()
        };
        let mut dash = Dash::default();
        dash.notice = Some(("posted to T3".into(), false));
        assert_eq!(writes.finish(&mut dash), Finished::default());
        tx.send((String::new(), false)).unwrap();
        let done = writes.finish(&mut dash);
        assert!(done.board && writes.seen.is_empty());
        // Quiet: the notice before it stays.
        assert_eq!(dash.notice, Some(("posted to T3".into(), false)));
        let (tx, seen) = mpsc::channel();
        writes.seen.push((vec![77], seen));
        tx.send(("there is no message 77 on the board".into(), true))
            .unwrap();
        assert!(writes.finish(&mut dash).board);
        assert_eq!(
            dash.notice,
            Some(("there is no message 77 on the board".into(), true))
        );
    }

    /// A failed read of an opened thread's items is tried again, after `REFRESH`,
    /// once, and not while another read is in flight; a read that worked is not.
    #[test]
    fn a_failed_read_of_an_opened_threads_items_is_tried_again() {
        let mut dash = Dash::default();
        let (tx, seen) = mpsc::channel();
        let mut writes = Writes {
            seen: vec![(vec![502, 503], seen)],
            ..Writes::default()
        };
        tx.send(("the board is locked".into(), true)).unwrap();
        assert!(writes.finish(&mut dash).board);
        let failed = std::time::Instant::now();
        assert_eq!(dash.notice, Some(("the board is locked".into(), true)));
        assert_eq!(writes.retry_seen(failed), None, "not at once");
        // Another read in flight: the retry waits for it.
        let (busy_tx, busy) = mpsc::channel();
        writes.seen.push((vec![7], busy));
        assert_eq!(writes.retry_seen(failed + REFRESH), None);
        busy_tx.send((String::new(), false)).unwrap();
        writes.finish(&mut dash);
        assert_eq!(writes.retry_seen(failed + REFRESH), Some(vec![502, 503]));
        assert_eq!(writes.retry_seen(failed + REFRESH * 2), None, "once");
        // The read that worked (7) is never tried again.
        assert!(writes.unseen.is_empty());
        // The retry works: the red failure gives way to a plain "read now".
        let (tx, seen) = mpsc::channel();
        writes.seen.push((vec![502, 503], seen));
        tx.send((String::new(), false)).unwrap();
        writes.finish(&mut dash);
        assert_eq!(dash.notice, Some((READ_NOW.into(), false)));
        // Once only, and never over something newer on the keys line.
        let (tx, seen) = mpsc::channel();
        writes.seen.push((vec![9], seen));
        tx.send(("the board is locked".into(), true)).unwrap();
        writes.finish(&mut dash);
        dash.notice = Some(("posted to T3".into(), false));
        let (tx, seen) = mpsc::channel();
        writes.seen.push((vec![9], seen));
        tx.send((String::new(), false)).unwrap();
        writes.finish(&mut dash);
        assert_eq!(dash.notice, Some(("posted to T3".into(), false)));
    }

    #[test]
    fn a_fold_asks_the_refresh_thread_to_read_and_says_only_a_refusal() {
        let (ok_tx, ok) = mpsc::channel();
        let (bad_tx, bad) = mpsc::channel();
        let mut writes = Writes {
            fold: vec![ok, bad],
            ..Writes::default()
        };
        let mut dash = Dash::default();
        dash.unfolding = Some(crate::ui::tree::Fold::Milestone(MilestoneId(1)));
        assert_eq!(writes.finish(&mut dash), Finished::default());
        // A fold that works: no read on this thread, no notice, and the unfold the owner
        // asked for still waits for a refresh that shows it.
        ok_tx.send(("unfolded M1".into(), false)).unwrap();
        let done = writes.finish(&mut dash);
        assert_eq!(
            done,
            Finished {
                fold: true,
                ..Finished::default()
            }
        );
        assert_eq!(dash.notice, None, "a fold that works says nothing");
        assert_eq!(
            dash.unfolding,
            Some(crate::ui::tree::Fold::Milestone(MilestoneId(1)))
        );
        // A refused one says why and ends the unfold.
        bad_tx
            .send(("there is no milestone M9 on the board".into(), true))
            .unwrap();
        assert!(writes.finish(&mut dash).fold);
        assert_eq!(
            dash.notice,
            Some(("there is no milestone M9 on the board".into(), true))
        );
        assert_eq!(dash.unfolding, None);
        assert!(writes.fold.is_empty());
    }
}
