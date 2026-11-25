//! The dashboard rendered with ratatui's TestBackend. Snapshots live in tests/snapshots;
//! rewrite them with `UPDATE_SNAPSHOTS=1 cargo test --test ui` and review the diff.

use std::collections::BTreeSet;

use control_tower::board::{
    Board, BoardOf, Colour, Criterion, Lead, Mention, Message, Milestone, MilestoneDetails,
    MilestoneId, Repo, RepoId, RepoOf, Resolution, Stage, Task, TaskDetails, TaskId, Thread,
    ThreadId, ThreadStatus,
};
use control_tower::inbox::Inbox;
use control_tower::store::Store;
use control_tower::ui::{
    Control, Dash, Depth, Fold, Input, Item, Layout, Mode, OverviewLayout, Reader, Request, Shown,
    Stop, View, ViewKind, accent, alert_line, bad, detail_layout, dim_colour, draw, hyperlink,
    layout, lead_colour, milestone_detail, ok, overview, overview_layout, refresh, selected_bg,
    task_detail, tree_items, tree_lines, warn,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// The time frames are drawn at, in seconds since the Unix epoch.
const NOW: i64 = 1_800_000_000;

/// A board as it first shows: no message opened in full, RESOLVED folded.
const FOLDED: Shown = Shown {
    opened: &NO_MESSAGES,
    unfolded: false,
};
static NO_MESSAGES: BTreeSet<i64> = BTreeSet::new();

fn fixture() -> Board {
    use Stage::*;
    let task = |id, title: &str, stage| Task {
        id: TaskId(id),
        title: title.into(),
        stage,
        repo: None,
        subtasks: Vec::new(),
        details: TaskDetails::default(),
    };
    let mut board = Board {
        repos: Vec::new(),
        milestones: vec![
            Milestone {
                id: MilestoneId(1),
                title: "First shop pages".into(),
                goal: "A reader can find a book, see its page and save it.".into(),
                repo: Some(RepoId(1)),
                lead: None,
                tasks: vec![
                    task(12, "Bookmarks page", Done),
                    task(13, "New book shelf", InProgress),
                    task(
                        14,
                        "Show each book's reviews under its cover, newest first, with a link to the rest",
                        Waiting,
                    ),
                ],
                marked_done: false,
                done_at: None,
                folded: false,
                threads: Vec::new(),
                details: MilestoneDetails::default(),
            },
            Milestone {
                id: MilestoneId(2),
                title: "Checkout with a title long enough to be cut in a narrow pane".into(),
                goal: "Readers keep their basket across phone, tablet and desktop.".into(),
                repo: Some(RepoId(2)),
                lead: None,
                tasks: vec![
                    task(21, "Shelf layout", Done),
                    task(22, "日本語のタスク名と絵文字です", Done),
                ],
                // Every task Done, no Done mark yet: it stays in the active tree.
                marked_done: false,
                done_at: None,
                folded: false,
                threads: Vec::new(),
                details: MilestoneDetails::default(),
            },
            Milestone {
                id: MilestoneId(3),
                title: "Search by author".into(),
                goal: "Readers find every book by an author from the search box.".into(),
                repo: Some(RepoId(3)),
                lead: None,
                tasks: vec![],
                // A stray mark on a milestone with no tasks shows nothing done.
                marked_done: true,
                done_at: None,
                folded: false,
                threads: Vec::new(),
                details: MilestoneDetails::default(),
            },
        ],
        standalone: Vec::new(),
        inbox: Default::default(),
    };
    // One task links to its ticket, the others to none.
    board.milestones[0].tasks[1].details.link =
        Some("https://github.com/example/bookshop/issues/113".into());
    sync(&mut board);
    board
}

/// The repos' names in the fixtures, by id: their main checkouts are `/src/<name>`.
const REPO_NAMES: [&str; 3] = ["catalogue", "checkout", "search"];

/// Rebuilds `board`'s repos from its milestones, as a load reads them: one per repo id a
/// milestone names (keeping its fold), checked out at `/src/<name>`, led by the lead of its
/// first milestone with one, its board every thread of its milestones; each milestone then
/// carries its repo's whole board and lead, and the inbox is worked out afresh. A thread
/// with neither link is linked to the milestone it was found on.
fn sync(board: &mut Board) {
    let mut repos: Vec<Repo> = Vec::new();
    for m in &board.milestones {
        let Some(id) = m.repo else { continue };
        let i = match repos.iter().position(|r| r.id == id) {
            Some(i) => i,
            None => {
                let folded = board.repo(id).is_some_and(|r| r.folded);
                let done_folded = board.repo(id).is_none_or(|r| r.done_folded);
                let link = board.repo(id).and_then(|r| r.link.clone());
                repos.push(Repo {
                    id,
                    path: Some(format!("/src/{}", REPO_NAMES[id.0 as usize - 1])),
                    link,
                    lead: None,
                    folded,
                    done_folded,
                    threads: Vec::new(),
                });
                repos.len() - 1
            }
        };
        let r = &mut repos[i];
        if r.lead.is_none() {
            r.lead = m.lead.clone();
        }
        for t in &m.threads {
            if !r.threads.iter().any(|x| x.id == t.id) {
                // A thread with no task on a milestone's board is about that milestone, as
                // the schema 14 migration makes it.
                let mut t = t.clone();
                if t.task.is_none() && t.milestone.is_none() {
                    t.milestone = Some(m.id);
                }
                r.threads.push(t);
            }
        }
    }
    repos.sort_by_key(|r| r.id.0);
    for r in &mut repos {
        r.threads.sort_by_key(|t| t.id.0);
    }
    for m in &mut board.milestones {
        if let Some(r) = repos.iter().find(|r| Some(r.id) == m.repo) {
            m.threads = r.threads.clone();
            m.lead = r.lead.clone();
        }
    }
    board.repos = repos;
    board.inbox = Inbox::of(&board.repos, mentions_of(board));
}

/// The fixture with two teams led and the third still without a lead.
fn led_fixture() -> Board {
    let mut board = fixture();
    board.milestones[0].lead = Some(Lead {
        name: "Catalogue Lead".into(),
        session: "cat-lead".into(),
        colour: Some(Colour::Blue),
    });
    board.milestones[1].lead = Some(Lead {
        name: "Checkout Lead with a name long enough to be cut".into(),
        session: "checkout-lead".into(),
        colour: Some(Colour::Purple),
    });
    sync(&mut board);
    board
}

/// The led fixture with a message board on M1, as in the signed design: T3 for #13's team with
/// seven messages, T4 for #14, and T2 resolved with its evidence.
fn board_fixture() -> Board {
    let mut board = led_fixture();
    let thread = |id, title: &str, task: Option<i64>, status, msgs: &[(&str, &str, i64)]| {
        let messages: Vec<Message> = (msgs.iter().enumerate())
            .map(|(i, (author, text, minutes))| Message {
                id: id * 100 + i as i64,
                author: (*author).into(),
                text: (*text).into(),
                at: NOW - minutes * 60,
            })
            .collect();
        let last = messages.last().unwrap();
        let resolved = (status == ThreadStatus::Resolved).then(|| Resolution {
            by: last.author.clone(),
            at: last.at,
        });
        Thread::whole(
            ThreadId(id),
            title.into(),
            task.map(TaskId),
            resolved,
            messages,
        )
    };
    board.milestones[0].threads = vec![
        thread(
            2,
            "Ribbon hides the cover on phones",
            None,
            ThreadStatus::Resolved,
            &[(
                "senior",
                "resolved: the ribbon sits under the cover now; checked on phone, tablet and desktop",
                120,
            )],
        ),
        thread(
            3,
            "Task #13 team",
            Some(13),
            ThreadStatus::Open,
            &[
                (
                    "cat-lead",
                    "Picked up #13 from the M1 spec. Opening this thread for its team.",
                    58,
                ),
                (
                    "dev-2",
                    "copy, reading the spec and the shelf fields in the catalogue",
                    55,
                ),
                ("senior", "copy, I'll review as soon as there is a diff", 54),
                (
                    "qa",
                    "copy, preparing the shelf at three sizes: phone, tablet, desktop",
                    52,
                ),
                (
                    "dev-2",
                    "I have done the shelf grid. Please test while I do the ribbon.",
                    12,
                ),
                ("senior", "copy, reviewing the layout", 10),
                (
                    "qa",
                    "copy, testing: phone and tablet pass, desktop shows 13 books, not 12",
                    8,
                ),
            ],
        ),
        thread(
            4,
            "Review stars",
            Some(14),
            ThreadStatus::Open,
            &[
                (
                    "dev-1",
                    "Starting the reading reminders with the sample catalogue.",
                    45,
                ),
                (
                    "dev-1",
                    "blocked: the printer has not sent the cover images",
                    20,
                ),
                (
                    "cat-lead",
                    "question: the covers are late again. @manager can you call the printer?",
                    15,
                ),
            ],
        ),
    ];
    sync(&mut board);
    with_alerts(&mut board, &[]);
    board
}

/// Works out `board`'s inbox afresh from its threads, as a refresh does, with the messages
/// `read` marked read.
fn with_alerts(board: &mut Board, read: &[i64]) {
    sync(board);
    let mut mentions = mentions_of(board);
    for m in &mut mentions {
        m.read = read.contains(&m.message.id);
    }
    board.inbox = Inbox::of(&board.repos, mentions);
}

/// The `@manager` mentions of every thread on `board`, as the store loads them, unread: the
/// fixtures' threads are whole.
fn mentions_of(board: &Board) -> Vec<Mention> {
    let mut all: Vec<Mention> = (board.repos.iter())
        .flat_map(|r| &r.threads)
        .flat_map(|t| {
            (t.messages.iter())
                .filter(|m| m.text.contains("@manager"))
                .map(|m| Mention {
                    thread: t.id,
                    message: m.clone(),
                    read: false,
                })
        })
        .collect();
    all.sort_by_key(|m| (m.message.at, m.message.id));
    all
}

fn render(view: &View, w: u16, h: u16) -> Buffer {
    render_at(view, w, h, Depth::TrueColor)
}

fn render_at(view: &View, w: u16, h: u16, depth: Depth) -> Buffer {
    render_dash(view, &clocked(), w, h, depth)
}

fn render_dash(view: &View, dash: &Dash, w: u16, h: u16, depth: Depth) -> Buffer {
    // As the edge does before each frame: the full board's separators at the frame's time.
    let mut dash = dash.clone();
    dash.set_now(NOW);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            draw(f, view, &dash, "14:32", depth);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Each row as the terminal shows it: a wide character's trailing cell is skipped.
fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0;
            while x < area.width {
                let sym = buf[(x, y)].symbol();
                row.push_str(sym);
                x += sym.width().max(1) as u16;
            }
            utc_days(row)
        })
        .collect()
}

fn assert_snapshot(name: &str, buf: &Buffer) {
    let text = rows(buf).join("\n") + "\n";
    let path = format!("{}/tests/snapshots/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    // The gate runs in CI (SDLC.md), so a missing or changed file prints the
    // rendering as it is, to be taken from CI's log once checked against the spec and, for
    // a UI change, its signed design (SDLC.md).
    let take = "run with UPDATE_SNAPSHOTS=1 to write it, or take it from here";
    let want =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}; {take}:\n{text}"));
    assert!(text == want, "{name} differs from {path}; {take}:\n{text}");
}

fn assert_exact_width(buf: &Buffer) {
    let w = buf.area.width as usize;
    for (y, row) in rows(buf).iter().enumerate() {
        assert_eq!(row.width(), w, "row {y} is not {w} columns: {row:?}");
    }
}

#[test]
fn tree_snapshot_at_80_columns_no_leads_yet() {
    let buf = render(&Ok(fixture()), 80, 20);
    assert_exact_width(&buf);
    assert_snapshot("tree_80x20", &buf);
}

#[test]
fn tree_snapshot_at_34_columns_no_leads_yet() {
    let buf = render(&Ok(fixture()), 34, 20);
    assert_exact_width(&buf);
    assert_snapshot("tree_34x20", &buf);
}

#[test]
fn two_lead_snapshot_at_80_columns() {
    let buf = render(&Ok(led_fixture()), 80, 20);
    assert_exact_width(&buf);
    assert_snapshot("leads_80x20", &buf);
}

#[test]
fn two_lead_snapshot_at_34_columns() {
    let buf = render(&Ok(led_fixture()), 34, 20);
    assert_exact_width(&buf);
    assert_snapshot("leads_34x20", &buf);
}

/// The index of the first row that starts with `prefix`.
fn find_row(all: &[String], prefix: &str) -> usize {
    all.iter()
        .position(|r| r.starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?}: {all:#?}"))
}

#[test]
fn a_repo_row_is_its_name_in_bold_with_its_lead_under_it_in_the_accent_colour() {
    for depth in [Depth::TrueColor, Depth::Ansi256] {
        let mut board = led_fixture();
        // A lead's stored colour, or none, does not change it: every lead is in the accent.
        board.milestones[0].lead.as_mut().unwrap().colour = None;
        sync(&mut board);
        let buf = render_at(&Ok(board), 60, 20, depth);
        let all = rows(&buf);
        for (repo, lead) in [
            (" ▾ catalogue", "   Catalogue Lead"),
            (" ▾ checkout", "   Checkout Lead"),
        ] {
            let y = find_row(&all, repo);
            for x in 3..repo.chars().count() as u16 {
                assert!(
                    buf[(x, y as u16)].modifier.contains(Modifier::BOLD),
                    "{repo} x={x}"
                );
            }
            assert!(all[y + 1].starts_with(lead), "{all:#?}");
            for x in 3..lead.len() as u16 {
                let cell = &buf[(x, y as u16 + 1)];
                assert_eq!(cell.fg, accent(depth), "{lead} x={x}");
                assert!(!cell.modifier.contains(Modifier::BOLD), "{lead} x={x}");
            }
            // Its milestone sits right under its lead line, in plain text.
            assert!(all[y + 2].starts_with("   ▾ M"), "{all:#?}");
            let m = &buf[(7, y as u16 + 2)];
            assert!(!m.modifier.contains(Modifier::BOLD), "{repo}");
            assert_eq!(m.fg, Color::Reset, "{repo}");
        }
    }
}

#[test]
fn an_older_board_says_how_to_upgrade_it_instead_of_a_database_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE milestone (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, \
         goal TEXT NOT NULL); PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);
    let problem = refresh(&path).unwrap_err();
    assert!(
        problem.ends_with("is schema version 1; run tower upgrade"),
        "{problem}"
    );
}

#[test]
fn colours_are_24_bit_only_when_the_terminal_says_so() {
    assert_eq!(Depth::from_colorterm(Some("truecolor")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("24bit")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("TrueColor")), Depth::TrueColor);
    for other in [None, Some(""), Some("yes"), Some("256")] {
        assert_eq!(Depth::from_colorterm(other), Depth::Ansi256, "{other:?}");
    }
    for c in Colour::ALL {
        assert!(matches!(lead_colour(c, Depth::TrueColor), Color::Rgb(..)));
        assert!(matches!(
            lead_colour(c, Depth::Ansi256),
            Color::Indexed(16..=231)
        ));
    }
    // Every lead gets a colour of its own until the palette runs out.
    let mut seen: Vec<Color> = Colour::ALL
        .iter()
        .map(|c| lead_colour(*c, Depth::Ansi256))
        .collect();
    seen.dedup();
    assert_eq!(seen.len(), Colour::ALL.len());
}

#[test]
fn a_repo_without_a_lead_says_n_starts_one_under_its_name() {
    let all = rows(&render(&Ok(led_fixture()), 140, 30));
    let y = find_row(&all, " ▾ search");
    assert_eq!(
        all[y + 1].split('│').next().unwrap().trim_end(),
        "   no lead yet: n starts one"
    );
    assert!(
        all[y + 2].starts_with("   ▾ M3 Search by author"),
        "{all:#?}"
    );
}

#[test]
fn a_milestone_in_no_repo_shows_in_the_last_group_saying_to_give_it_one() {
    let mut board = led_fixture();
    let m3 = (board.milestones.iter_mut())
        .find(|m| m.id == MilestoneId(3))
        .unwrap();
    m3.repo = None;
    sync(&mut board);
    let all = rows(&render(&Ok(board), 140, 30));
    let tree = |y: usize| all[y].split('│').next().unwrap().trim_end().to_string();
    let y = find_row(&all, " No repo yet");
    assert!(tree(y - 1).chars().all(|c| c == '─'), "{all:#?}");
    // One label, then one line saying how to give each item its repo.
    assert_eq!(tree(y + 1), "   tower milestone set / task set --repo R<n>");
    assert!(
        tree(y + 3).starts_with("   ▾ M3 Search by author"),
        "{all:#?}"
    );
    // Its repo row is gone with it: R3 has no milestone left, so it is not on the board.
    assert!(!all.iter().any(|r| r.starts_with(" ▾ search")), "{all:#?}");
}

#[test]
fn every_row_is_exactly_the_pane_width_at_any_size() {
    let views: [View; 4] = [
        Ok(fixture()),
        Ok(led_fixture()),
        Ok(Board::default()),
        Err("no board at /a/very/long/path/to/the/board.db yet; add a milestone".into()),
    ];
    for w in (1..=40).chain([79, 80, 81, 200, 1000]) {
        for h in (1..=8).chain([40, 500]) {
            for view in &views {
                assert_exact_width(&render(view, w, h));
            }
        }
    }
}

#[test]
fn tree_lines_are_cut_or_padded_to_the_width_not_clipped_by_the_terminal() {
    for board in [fixture(), led_fixture(), Board::default()] {
        for w in 0..=120 {
            for line in tree_lines(&board, w, Depth::Ansi256) {
                assert_eq!(line.width(), w, "{line:?}");
            }
        }
    }
}

#[test]
fn zero_sized_panes_do_not_panic() {
    for (w, h) in [(0, 0), (0, 10), (10, 0)] {
        render(&Ok(fixture()), w, h);
    }
}

#[test]
fn the_alert_line_ends_with_the_clock_and_there_is_no_header_line() {
    let all = rows(&render(&Ok(fixture()), 40, 10));
    assert!(all[0].starts_with(" nothing needs you"), "{:?}", all[0]);
    assert!(all[0].ends_with("inbox (i) ▶  14:32 "), "{:?}", all[0]);
    assert!(!all.iter().any(|r| r.contains("control-tower")), "{all:?}");
    // Too narrow to keep room for both, the alert line keeps it.
    let narrow = &rows(&render(&Ok(fixture()), 39, 10))[0];
    assert!(!narrow.contains("14:32"), "{narrow:?}");
}

#[test]
fn a_problem_takes_the_place_of_the_tree_and_wraps_rather_than_cuts() {
    let problem =
        "no board at /tmp/some/long/path/board.db yet; add a milestone with tower milestone add";
    let all = rows(&render(&Err(problem.into()), 30, 10));
    assert!(all[1].starts_with(" no board at"), "{all:?}");
    let joined: String = all[1..6]
        .iter()
        .map(|r| r.trim_end())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("tower milestone add"), "{all:?}");
    assert!(
        !all.iter().any(|r| r.contains("M1")),
        "stale tree rows shown: {all:?}"
    );
    assert!(all[9].starts_with(" ↑↓ select"), "{all:?}");
}

/// A new repo on the board, its checkout a temp main checkout gone once it is added.
fn new_repo(store: &mut Store) -> RepoId {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    store.add_repo(dir.path(), None).unwrap()
}

fn seed(path: &std::path::Path, title: &str) {
    Store::open(path)
        .unwrap()
        .add_milestone(title, "goal", None)
        .unwrap();
}

#[test]
fn refresh_follows_the_file_as_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let titles =
        |v: View| -> Vec<String> { v.unwrap().milestones.into_iter().map(|m| m.title).collect() };

    let missing = refresh(&path).unwrap_err();
    assert!(missing.starts_with("no board at "), "{missing}");
    assert!(!path.exists(), "refresh created the board");

    seed(&path, "first board");
    assert_eq!(titles(refresh(&path)), ["first board"]);

    // Replaced: another board renamed over it, as a restore from backup would.
    let other = dir.path().join("other.db");
    seed(&other, "replacement");
    std::fs::rename(&other, &path).unwrap();
    assert_eq!(titles(refresh(&path)), ["replacement"]);

    std::fs::remove_file(&path).unwrap();
    let gone = refresh(&path).unwrap_err();
    assert!(gone.starts_with("no board at "), "{gone}");
}

#[test]
fn an_empty_board_says_how_to_add_a_repo() {
    let all = rows(&render(&Ok(Board::default()), 80, 8)).join("\n");
    assert!(
        all.contains("no repos yet: add one with tower repo add"),
        "{all}"
    );
}

#[test]
fn each_task_shows_its_stage_mark_in_the_golden_colours() {
    for depth in [Depth::TrueColor, Depth::Ansi256] {
        let buf = render_at(&Ok(fixture()), 80, 20, depth);
        let all = rows(&buf);
        let cyan = match depth {
            Depth::TrueColor => Color::Rgb(0x78, 0xdc, 0xe8),
            Depth::Ansi256 => Color::Indexed(116),
        };
        for (prefix, mark, colour, title) in [
            ("     ✓ Bookmarks", "✓", ok(depth), dim_colour(depth)),
            ("     ● New book shelf", "●", cyan, Color::Reset),
            (
                "     ○ Show each",
                "○",
                dim_colour(depth),
                dim_colour(depth),
            ),
        ] {
            let y = find_row(&all, prefix) as u16;
            let cell = &buf[(5, y)];
            assert_eq!((cell.symbol(), cell.fg), (mark, colour), "{prefix}");
            // The row is quiet: a dim title, plain only while In progress...
            assert_eq!(buf[(8, y)].fg, title, "{prefix}");
            // ...and at the right edge its ticket in the accent colour, or "no ticket" dim,
            // whatever the stage.
            let row = all[y as usize].trim_end();
            let (at, colour) = match row.rfind("no ticket") {
                Some(at) => (at, dim_colour(depth)),
                None => (row.rfind('#').unwrap(), accent(depth)),
            };
            let x = row[..at].chars().count() as u16;
            assert_eq!(buf[(x, y)].fg, colour, "{prefix}");
        }
    }
}

#[test]
fn dim_text_and_the_bar_track_are_the_copy_grey_at_both_depths() {
    // The signed design's --dim, #6c7280, and its nearest 256-colour grey: never
    // ANSI DarkGray, which a dark terminal theme maps to near the background.
    for (depth, grey) in [
        (Depth::TrueColor, Color::Rgb(0x6c, 0x72, 0x80)),
        (Depth::Ansi256, Color::Indexed(243)),
    ] {
        assert_eq!(dim_colour(depth), grey);
        let buf = render_at(&Ok(fixture()), 40, 20, depth);
        let all = rows(&buf);
        let y = find_row(&all, "   ▾ M1");
        // M1 is 1 of 3 done: two green cells, then the six-cell track.
        assert_eq!(all[y], "   ▾ M1 First shop pages  ██░░░░░░  33% ");
        for x in 28..34 {
            let cell = &buf[(x, y as u16)];
            assert_eq!((cell.symbol(), cell.fg), ("░", grey), "x={x} {depth:?}");
        }
        // A done task's title is dim too.
        let y = find_row(&all, "     ✓ Bookmarks");
        assert_eq!(buf[(8, y as u16)].fg, grey, "{depth:?}");
    }
}

/// The milestone row for `m` (like "M1") at width `w`, as the terminal shows it.
fn milestone_row(board: &Board, m: &str, w: u16) -> (Buffer, u16, String) {
    let buf = render(&Ok(board.clone()), w, 30);
    let all = rows(&buf);
    let y = find_row(&all, &format!("   ▾ {m}"));
    let row = all[y].clone();
    (buf, y as u16, row)
}

#[test]
fn a_milestone_shows_its_bar_and_percentage_at_the_right_edge() {
    let board = fixture();
    let (buf, y, row) = milestone_row(&board, "M1", 40);
    // 1 of 3 done: 33%, and 33% of 8 cells rounded down is 2.
    assert_eq!(row, "   ▾ M1 First shop pages  ██░░░░░░  33% ");
    for x in 26..28 {
        assert_eq!(buf[(x, y)].fg, ok(Depth::TrueColor), "x={x}");
    }
    for x in 28..34 {
        assert_eq!(buf[(x, y)].fg, dim_colour(Depth::TrueColor), "x={x}");
    }
    assert!(buf[(37, y)].modifier.contains(Modifier::BOLD));

    // A long title wraps to an indented second line, cut only past it.
    let (_, _, row) = milestone_row(&board, "M2", 40);
    assert_eq!(row, "   ▾ M2 Checkout with a   ████████ 100% ");
    let lines: Vec<String> = (tree_lines(&board, 40, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("   ▾ M2 "))
        .unwrap();
    assert_eq!(lines[at + 1], "     title long enough to be cut in a…  ");
    // No tasks: an empty bar at 0%, even with a Done mark stored.
    let (_, _, row) = milestone_row(&board, "M3", 40);
    assert_eq!(row, "   ▾ M3 Search by author  ░░░░░░░░   0% ");
}

#[test]
fn the_bar_matches_the_rounded_down_percentage() {
    let task = |id, stage| Task {
        id: TaskId(id),
        title: "t".into(),
        stage,
        repo: None,
        subtasks: Vec::new(),
        details: TaskDetails::default(),
    };
    for (done, all, want) in [
        (1, 5, "█░░░░░░░  20%"),
        (1, 8, "░░░░░░░░  12%"),
        (199, 200, "███████░  99%"),
    ] {
        let tasks = (0..all)
            .map(|i| {
                task(
                    i as i64 + 1,
                    if i < done {
                        Stage::Done
                    } else {
                        Stage::Waiting
                    },
                )
            })
            .collect();
        let board = Board {
            repos: Vec::new(),
            milestones: vec![Milestone {
                id: MilestoneId(1),
                title: "m".into(),
                goal: "g".into(),
                repo: Some(RepoId(1)),
                lead: None,
                tasks,
                marked_done: false,
                done_at: None,
                folded: false,
                threads: Vec::new(),
                details: MilestoneDetails::default(),
            }],
            standalone: Vec::new(),
            inbox: Default::default(),
        };
        let mut board = board;
        sync(&mut board);
        // Under the repo's row and lead line.
        let line = tree_lines(&board, 30, Depth::TrueColor)[2].to_string();
        assert!(
            line.ends_with(&format!(" {want} ")),
            "{done}/{all}: {line:?}"
        );
    }
}

#[test]
fn a_narrow_pane_drops_the_bar_before_the_milestone_id() {
    let board = fixture();
    // "   ▾ M1…" plus " ██░░░░░░  33% " is 23 columns.
    let (_, _, row) = milestone_row(&board, "M1", 23);
    assert_eq!(row, "   ▾ M1  ██░░░░░░  33% ");
    let (_, _, row) = milestone_row(&board, "M1", 22);
    assert_eq!(row, "   ▾ M1 First shop    ");
}

// The two panes: the tree on the left, the overview of the selection on the right.

/// A dashboard with its clock set, as the edge sets it before every frame and input.
fn clocked() -> Dash {
    let mut dash = Dash::default();
    dash.set_now(NOW);
    dash
}

fn select(item: Item) -> Dash {
    let mut dash = Dash::new(Some(item));
    dash.set_now(NOW);
    dash
}

/// The right pane of a layout: where it is and its overview's layout.
struct Right {
    area: Rect,
    overview: OverviewLayout,
}

fn right(l: &Layout) -> Right {
    Right {
        area: l.right.expect("an overview pane"),
        overview: l.overview.clone().expect("an overview"),
    }
}

fn area(w: u16, h: u16) -> Rect {
    Rect::new(0, 0, w, h)
}

#[test]
fn two_pane_snapshots_with_a_milestone_and_with_a_task_selected() {
    for (w, h) in [(120, 30), (72, 30)] {
        for (name, dash) in [
            ("milestone", select(Item::Milestone(MilestoneId(1)))),
            ("task", select(Item::Task(TaskId(13)))),
        ] {
            let buf = render_dash(&Ok(led_fixture()), &dash, w, h, Depth::TrueColor);
            assert_exact_width(&buf);
            assert_snapshot(&format!("overview_{name}_{w}x{h}"), &buf);
        }
    }
}

/// The overview's and a detail's boards fold resolved threads into one RESOLVED row;
/// a click on it unfolds them, one line each, and a second folds them. The overview and
/// the detail of a selection share it. A click on a line opens that thread on the full
/// board.
#[test]
fn the_overview_and_detail_boards_fold_resolved_threads_and_a_click_unfolds_them() {
    let view: View = Ok(board_fixture());
    // Tall enough for the milestone detail's whole board.
    let a = area(120, 70);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let row = |dash: &Dash, mark: &str| {
        let all = screen(&view, dash, a);
        let y = (all
            .iter()
            .position(|r| r.contains(&format!("│ RESOLVED 1 {mark}"))))
        .unwrap_or_else(|| panic!("no RESOLVED 1 {mark}: {all:#?}"));
        let x = all[y].split("RESOLVED").next().unwrap().chars().count() as u16;
        (all, x, y as u16)
    };
    let (all, x, y) = row(&dash, "▸");
    assert!(
        !all.iter().any(|r| r.contains("New-book ribbon")),
        "{all:#?}"
    );
    assert_eq!(dash.handle(&view, a, Input::Click { x, y }), None);
    let (all, _, unfolded) = row(&dash, "▾");
    assert_eq!(unfolded, y);
    let line = &all[usize::from(y) + 1];
    assert!(line.contains("│ ✓ Ribbon hides the cover"), "{line}");
    // The milestone's detail shows its board unfolded too; a click there folds it.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
    let (_, x, y) = row(&dash, "▾");
    dash.handle(&view, a, Input::Click { x, y });
    let (all, x, y) = row(&dash, "▸");
    assert!(
        !all.iter().any(|r| r.contains("New-book ribbon")),
        "{all:#?}"
    );
    // Unfolded again, a click on the resolved thread's line opens it on the full board.
    dash.handle(&view, a, Input::Click { x, y });
    row(&dash, "▾");
    dash.handle(&view, a, Input::Click { x: x + 4, y: y + 1 });
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread_wanted(), Some(ThreadId(2)));
}

#[test]
fn message_board_snapshots_wide_and_narrow_and_narrowed_by_task() {
    let view: View = Ok(board_fixture());
    for (w, h) in [(120, 50), (72, 60)] {
        for (name, dash) in [
            ("milestone", select(Item::Milestone(MilestoneId(1)))),
            ("task", select(Item::Task(TaskId(13)))),
        ] {
            let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
            assert_exact_width(&buf);
            assert_snapshot(&format!("board_{name}_{w}x{h}"), &buf);
        }
    }
    // A pane too narrow for the tree beside it: Enter opens the overview full width.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    // Tall enough for the whole board, so no bubble is cut at the bottom.
    dash.handle(&view, area(40, 110), Input::Enter);
    let buf = render_dash(&view, &dash, 40, 110, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("board_milestone_40x110", &buf);
}

#[test]
fn the_overview_scrolls_down_to_the_end_of_the_message_board() {
    let view: View = Ok(board_fixture());
    let a = area(120, 20);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    for _ in 0..10 {
        dash.handle(&view, a, Input::PageDown);
    }
    let all = rows(&render_dash(&view, &dash, 120, 20, Depth::TrueColor));
    // The body's last row (above the rule and keys) is the folded RESOLVED row, under the
    // last open thread's box.
    assert!(all[17].contains("│ RESOLVED 1 ▸"), "{all:#?}");
    assert!(all[16].contains(" └──"), "{all:#?}");
    assert!(all[15].contains("open chat ▶"), "{all:#?}");
    // A task with no thread says so.
    let dash = select(Item::Task(TaskId(12)));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor)).join("\n");
    assert!(all.contains("MESSAGE BOARD · #12 Catalogue Lead"), "{all}");
    assert!(all.contains("no thread for this task yet"), "{all}");
}

#[test]
fn every_row_with_a_message_board_is_exactly_the_width_at_any_size() {
    let view: View = Ok(board_fixture());
    for item in [Item::Milestone(MilestoneId(1)), Item::Task(TaskId(13))] {
        for w in [1, 20, 40, 61, 62, 75, 91, 100, 133] {
            let buf = render_dash(&view, &select(item), w, 40, Depth::Ansi256);
            assert_exact_width(&buf);
        }
    }
}

#[test]
fn tower_ui_opens_on_the_first_repo_row_until_the_owner_picks() {
    let mut board = led_fixture();
    assert_eq!(clocked().selection(&board), Some(Item::Repo(RepoId(1))));
    // With its overview, as UI v0.1.7 opens.
    let view: View = Ok(board.clone());
    let all = screen(&view, &clocked(), area(120, 30));
    assert!(all[1].contains("catalogue · Catalogue Lead"), "{}", all[1]);
    // No repos: the first row the tree shows, in no repo.
    board.repos.clear();
    assert_eq!(
        clocked().selection(&board),
        Some(Item::Milestone(MilestoneId(1)))
    );
    // An empty board: nothing.
    assert_eq!(clocked().selection(&Board::default()), None);
}

#[test]
fn up_and_down_move_over_milestones_and_tasks_and_stop_at_the_ends() {
    let view: View = Ok(led_fixture());
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let mut seen = vec![dash.selection(view.as_ref().unwrap()).unwrap()];
    for _ in 0..10 {
        dash.handle(&view, area(120, 30), Input::Down);
        seen.push(dash.selected.unwrap());
    }
    use Item::{Milestone as M, Repo as Rp, Task as T};
    // Each repo row is a stop of its own, between the repos' contents.
    let want = [
        M(MilestoneId(1)),
        T(TaskId(12)),
        T(TaskId(13)),
        T(TaskId(14)),
        Rp(RepoId(2)),
        M(MilestoneId(2)),
        T(TaskId(21)),
        T(TaskId(22)),
        Rp(RepoId(3)),
        M(MilestoneId(3)),
        M(MilestoneId(3)),
    ];
    assert_eq!(seen, want);
    for _ in 0..10 {
        dash.handle(&view, area(120, 30), Input::Up);
    }
    assert_eq!(dash.selected, Some(Rp(RepoId(1))));
}

#[test]
fn the_selection_follows_its_item_across_a_refresh() {
    let mut board = led_fixture();
    let dash = select(Item::Task(TaskId(21)));
    // A new milestone and task above it do not move it.
    board.milestones.insert(0, {
        let mut m = board.milestones[2].clone();
        m.id = MilestoneId(9);
        m
    });
    assert_eq!(dash.selection(&board), Some(Item::Task(TaskId(21))));
    // Gone from the board: the first repo instead.
    board.milestones.retain(|m| m.id != MilestoneId(2));
    sync(&mut board);
    assert_eq!(dash.selection(&board), Some(Item::Repo(RepoId(1))));
}

#[test]
fn the_selected_tree_rows_and_card_are_highlighted() {
    let depth = Depth::TrueColor;
    let view: View = Ok(led_fixture());
    let buf = render_dash(
        &view,
        &select(Item::Milestone(MilestoneId(2))),
        120,
        30,
        depth,
    );
    let all = rows(&buf);
    let bg = |prefix: &str| buf[(1, find_row(&all, prefix) as u16)].bg;
    assert_eq!(bg("   ▾ M2 Checkout"), selected_bg(depth));
    // The repo's rows are the repo's, not the milestone's.
    assert_eq!(bg(" ▾ checkout"), Color::Reset);
    assert_eq!(bg("   Checkout Lead"), Color::Reset);
    assert_eq!(bg("   Catalogue Lead"), Color::Reset);
    assert_eq!(bg("     ✓ Shelf"), Color::Reset);
    // A selected repo is both its lines.
    let buf = render_dash(&view, &select(Item::Repo(RepoId(2))), 120, 30, depth);
    let all = rows(&buf);
    let bg = |prefix: &str| buf[(1, find_row(&all, prefix) as u16)].bg;
    assert_eq!(bg(" ▾ checkout"), selected_bg(depth));
    assert_eq!(bg("   Checkout Lead"), selected_bg(depth));
    assert_eq!(bg("   ▾ M2 Checkout"), Color::Reset);

    let buf = render_dash(&view, &select(Item::Task(TaskId(13))), 120, 30, depth);
    let all = rows(&buf);
    assert_eq!(
        buf[(1, find_row(&all, "     ● New book") as u16)].bg,
        selected_bg(depth)
    );
    assert_eq!(
        buf[(1, find_row(&all, "   ▾ M1 First") as u16)].bg,
        Color::Reset
    );
    // The overview stays on the task's milestone, with the task's card in the accent colour.
    assert!(all[1].contains("M1  First shop pages"), "{all:#?}");
    let s = layout(&view, &select(Item::Task(TaskId(13))), area(120, 30));
    let r = right(&s);
    let card = r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(13))
        .unwrap();
    let other = r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(12))
        .unwrap();
    // The selected card is on the selected background, bold; another is not.
    let cell =
        |c: &control_tower::ui::Card| &buf[(r.area.x + c.x as u16 + 1, r.area.y + c.line as u16)];
    assert_eq!(
        (cell(card).symbol(), cell(card).bg),
        ("#", selected_bg(depth))
    );
    // Not bold, as the signed design (UI v0.1.13); the selected background runs to the
    // card's end.
    assert!(!cell(card).modifier.contains(Modifier::BOLD));
    let end = &buf[(
        r.area.x + (card.x + card.width - 1) as u16,
        r.area.y + card.line as u16,
    )];
    assert_eq!(end.bg, selected_bg(depth));
    assert_ne!(cell(other).symbol(), " ");
    assert_ne!(cell(other).bg, selected_bg(depth));
}

#[test]
fn the_header_shows_the_milestone_stage_pill() {
    let mut board = led_fixture();
    // Every task done but not marked by the lead: still in progress.
    board.milestones[1].marked_done = false;
    for (m, want) in [
        (
            0,
            " [in progress] ███████░░░░░░░░░░░░░░░░░ 33% 1/3 tasks done",
        ),
        (
            1,
            " [in progress] ████████████████████████ 100% 2/2 tasks done Done mark not set",
        ),
        (2, " [waiting] ░░░░░░░░░░░░░░░░░░░░░░░░ 0% 0/0 tasks done"),
    ] {
        let o = overview(&board.milestones[m], None, 80, Depth::TrueColor);
        assert_eq!(o.lines[2].to_string().trim_end(), want, "M{}", m + 1);
    }
    board.milestones[1].marked_done = true;
    let o = overview(&board.milestones[1], None, 80, Depth::TrueColor);
    // Marked before the board kept the date: it says so rather than guess one.
    assert!(
        (o.lines[2].to_string().trim_end()).ends_with(" tasks done marked Done, date not recorded"),
        "{}",
        o.lines[2]
    );
    // The milestone in its lead's colour, bold, as in the tree.
    let style = o.lines[0].spans[1].style;
    assert_eq!(
        style.fg,
        Some(lead_colour(Colour::Purple, Depth::TrueColor))
    );
    assert!(style.add_modifier.contains(Modifier::BOLD));
    assert!(o.lines[2].to_string().starts_with(" [done] "));
    assert_eq!(
        o.lines[0].to_string(),
        " M2  Checkout with a title long enough to be cut in a… open milestone (enter) ▶ "
    );
    assert_eq!(
        o.lines[1].to_string().trim_end(),
        " goal Readers keep their basket across phone, tablet and desktop."
    );
    // Too narrow for the 24-cell bar: the bar goes, the stage and count stay.
    // The goal wraps to a second line, indented under its text.
    let o = overview(&board.milestones[0], None, 40, Depth::TrueColor);
    assert_eq!(
        o.lines[1].to_string().trim_end(),
        " goal A reader can find a book, see its"
    );
    assert_eq!(o.lines[2].to_string().trim_end(), "      page and save it.");
    assert_eq!(
        o.lines[3].to_string().trim_end(),
        " [in progress] 33% 1/3 tasks done"
    );
}

#[test]
fn columns_sit_side_by_side_when_wide_and_stack_when_narrow() {
    let board = led_fixture();
    let m = &board.milestones[0];
    let wide = overview(m, None, 60, Depth::TrueColor);
    let heads = wide.lines[4].to_string();
    for name in ["WAITING 1", "IN PROGRESS 1", "DONE 1"] {
        assert!(heads.contains(name), "{heads:?}");
    }
    let lines: Vec<usize> = wide.cards.iter().map(|c| c.line).collect();
    assert_eq!(lines, [6, 6, 6], "one row of cards");
    let narrow = overview(m, None, 59, Depth::TrueColor);
    let lines: Vec<usize> = narrow.cards.iter().map(|c| c.line).collect();
    assert!(lines[0] < lines[1] && lines[1] < lines[2], "{lines:?}");
    assert!(narrow.cards.iter().all(|c| c.x == 2 && c.width == 55));
    // An empty stage says so.
    let o = overview(&board.milestones[2], None, 80, Depth::TrueColor);
    assert!(
        o.lines[6].to_string().contains("│ none"),
        "{:?}",
        o.lines[6]
    );
}

#[test]
fn overview_lines_are_exactly_the_pane_width() {
    for board in [fixture(), led_fixture()] {
        for m in &board.milestones {
            for sel in [None, m.tasks.first().map(|t| t.id)] {
                for w in 0..=130 {
                    for line in overview(m, sel, w, Depth::Ansi256).lines {
                        assert_eq!(line.width(), w, "w={w} {line:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn every_two_pane_row_is_exactly_the_width_at_any_size() {
    let view: View = Ok(led_fixture());
    for w in (1..=130).chain([200, 1000]) {
        for h in [1, 3, 5, 8, 30] {
            for dash in [
                clocked(),
                select(Item::Task(TaskId(22))),
                select(Item::Milestone(MilestoneId(3))),
            ] {
                assert_exact_width(&render_dash(&view, &dash, w, h, Depth::TrueColor));
            }
        }
    }
}

#[test]
fn clicking_a_card_selects_its_task_and_clicking_elsewhere_does_not() {
    let view: View = Ok(led_fixture());
    for w in [120, 72] {
        let mut dash = select(Item::Task(TaskId(12)));
        let s = layout(&view, &dash, area(w, 40));
        let r = right(&s);
        for c in &r.overview.cards {
            // A card is one line: its first and last columns and one in between.
            assert_eq!(c.height, 1);
            for (dx, dy) in [(0, 0), (c.width - 1, 0), (4, 0)] {
                let (x, y) = (
                    r.area.x + (c.x + dx) as u16,
                    r.area.y + (c.line + dy) as u16,
                );
                assert_eq!(s.hit(&dash, x, y), Some(Item::Task(c.task)), "w={w} {c:?}");
            }
            // Just outside it: the column's border on the left, the line above.
            let (x, y) = (r.area.x + c.x as u16 - 1, r.area.y + c.line as u16);
            assert_eq!(s.hit(&dash, x, y), None, "w={w} left of {c:?}");
            let (x, y) = (r.area.x + c.x as u16, r.area.y + c.line as u16 - 1);
            assert_eq!(s.hit(&dash, x, y), None, "w={w} above {c:?}");
        }
        // The header and the separator select nothing.
        assert_eq!(s.hit(&dash, r.area.x + 3, r.area.y), None);
        assert_eq!(s.hit(&dash, r.area.x - 1, r.area.y + 6), None);
        let c = r.overview.cards[1];
        dash.handle(
            &view,
            area(w, 40),
            Input::Click {
                x: r.area.x + c.x as u16 + 2,
                y: r.area.y + c.line as u16,
            },
        );
        assert_eq!(dash.selected, Some(Item::Task(c.task)), "w={w}");
    }
}

#[test]
fn clicking_a_tree_row_selects_it() {
    let view: View = Ok(led_fixture());
    let all = rows(&render(&view, 120, 30));
    let mut dash = clocked();
    for (prefix, want) in [
        ("   Checkout Lead", Some(Item::Repo(RepoId(2)))),
        (" ▾ checkout", Some(Item::Repo(RepoId(2)))),
        ("     ✓ Shelf", Some(Item::Task(TaskId(21)))),
        ("   ▾ M3 Search", Some(Item::Milestone(MilestoneId(3)))),
    ] {
        let y = find_row(&all, prefix) as u16;
        dash.handle(&view, area(120, 30), Input::Click { x: 5, y });
        assert_eq!(dash.selected, want, "{prefix}");
    }
    // The rule between repos, "no tasks yet", the header and the keys line keep the
    // selection.
    let blank = find_row(&all, " ▾ checkout") as u16 - 1;
    let none = find_row(&all, "     no tasks yet") as u16;
    for y in [blank, none, 0, 29] {
        dash.handle(&view, area(120, 30), Input::Click { x: 5, y });
        assert_eq!(
            dash.selected,
            Some(Item::Milestone(MilestoneId(3))),
            "y={y}"
        );
    }
}

#[test]
fn a_click_lands_on_what_is_shown_after_scrolling() {
    let view: View = Ok(led_fixture());
    let a = area(72, 14);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    for _ in 0..3 {
        dash.handle(
            &view,
            a,
            Input::Wheel {
                x: 50,
                y: 5,
                down: true,
            },
        );
    }
    assert!(dash.right_scroll() > 0);
    let s = layout(&view, &dash, a);
    let r = right(&s);
    let c = r
        .overview
        .cards
        .iter()
        .find(|c| c.line >= dash.right_scroll())
        .unwrap();
    let y = r.area.y + (c.line - dash.right_scroll()) as u16;
    assert_eq!(s.hit(&dash, r.area.x + 5, y), Some(Item::Task(c.task)));
}

#[test]
fn each_pane_scrolls_on_its_own_and_stops_at_its_ends() {
    let view: View = Ok(led_fixture());
    let a = area(72, 14); // a body of 11 rows
    let s = layout(&view, &select(Item::Task(TaskId(12))), a);
    let tree_len = tree_lines(view.as_ref().unwrap(), 30, Depth::TrueColor).len();
    let right_len = right(&s).overview.lines;
    let (tree_max, right_max) = (tree_len - 11, right_len - 11);
    assert!(tree_max > 0 && right_max > 0);

    let mut dash = select(Item::Task(TaskId(12)));
    let wheel = |dash: &mut Dash, x, down| dash.handle(&view, a, Input::Wheel { x, y: 6, down });
    for _ in 0..50 {
        wheel(&mut dash, 60, true);
    }
    assert_eq!((dash.tree_scroll, dash.right_scroll()), (0, right_max));
    for _ in 0..50 {
        wheel(&mut dash, 5, true);
    }
    assert_eq!(
        (dash.tree_scroll, dash.right_scroll()),
        (tree_max, right_max)
    );
    wheel(&mut dash, 60, false);
    assert_eq!(dash.right_scroll(), right_max - 3);
    for _ in 0..50 {
        wheel(&mut dash, 60, false);
        wheel(&mut dash, 5, false);
    }
    assert_eq!((dash.tree_scroll, dash.right_scroll()), (0, 0));

    // PgDn and PgUp page the overview by a body less one line, within the same bounds.
    dash.handle(&view, a, Input::PageDown);
    assert_eq!(dash.right_scroll(), 10.min(right_max));
    for _ in 0..20 {
        dash.handle(&view, a, Input::PageDown);
    }
    assert_eq!(dash.right_scroll(), right_max);
    for _ in 0..20 {
        dash.handle(&view, a, Input::PageUp);
    }
    assert_eq!((dash.tree_scroll, dash.right_scroll()), (0, 0));
}

#[test]
fn moving_the_selection_scrolls_the_tree_to_keep_it_in_view() {
    let view: View = Ok(led_fixture());
    let a = area(72, 9); // a body of 5 rows
    let mut dash = select(Item::Task(TaskId(12)));
    for _ in 0..8 {
        dash.handle(&view, a, Input::Down);
    }
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(3))));
    let buf = render_dash(&view, &dash, 72, 9, Depth::TrueColor);
    let body = rows(&buf)[2..7].join("\n");
    assert!(
        body.contains(" no lead yet") && body.contains(" ▾ M3 Search"),
        "{body}"
    );
    for _ in 0..10 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.tree_scroll, 0);
}

#[test]
fn selecting_a_task_scrolls_the_overview_to_its_card() {
    let view: View = Ok(led_fixture());
    let a = area(72, 12); // stacked columns, a body of 9 rows
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Down); // #12, done: the last stacked column
    let s = layout(&view, &dash, a);
    let r = right(&s);
    let card = r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(12))
        .unwrap();
    assert!(dash.right_scroll() <= card.line);
    assert!(card.line + card.height <= dash.right_scroll() + 9);
    // Back on another milestone, the overview starts at its top.
    for _ in 0..4 {
        dash.handle(&view, a, Input::Down);
    }
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(2))));
    assert_eq!(dash.right_scroll(), 0);
}

#[test]
fn below_the_split_width_the_tree_takes_the_whole_pane() {
    let view: View = Ok(led_fixture());
    let s = layout(&view, &clocked(), area(60, 20));
    assert!(s.right.is_none());
    assert_eq!(s.tree.unwrap().width, 60);
    let s = layout(&view, &clocked(), area(61, 20));
    assert_eq!((s.tree.unwrap().width, right(&s).area.width), (30, 30));
}

#[test]
fn a_card_shows_its_done_subtasks_when_it_has_any() {
    use control_tower::board::Subtask;
    let mut board = led_fixture();
    board.milestones[0].tasks[1].subtasks = [Stage::Done, Stage::Waiting, Stage::InProgress]
        .into_iter()
        .enumerate()
        .map(|(i, stage)| Subtask {
            id: TaskId(30 + i as i64),
            title: "s".into(),
            stage,
        })
        .collect();
    let m = &board.milestones[0];
    let text = |w| -> String {
        overview(m, None, w, Depth::TrueColor)
            .lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    // One line: its number (dim), who is on it, and its subtasks done of all (dim). The
    // title is the tree's, not the card's.
    let wide = text(80);
    assert!(wide.contains("│ #113 unassigned · 1/3"), "{wide}");
    assert!(!wide.contains("New book shelf"), "{wide}");
    // A task with no subtasks shows no count.
    assert!(wide.contains("│ unassigned "), "{wide}");
    assert!(!wide.contains("0/0"), "{wide}");
    let o = overview(m, None, 80, Depth::TrueColor);
    let card = o.cards.iter().find(|c| c.task == TaskId(13)).unwrap();
    assert_eq!(card.height, 1);
    let line = &o.lines[card.line];
    let style = |text: &str| {
        line.spans
            .iter()
            .find(|s| s.content.contains(text))
            .unwrap()
            .style
    };
    assert_eq!(style("#113").fg, Some(accent(Depth::TrueColor)));
    assert_eq!(style("1/3").fg, Some(dim_colour(Depth::TrueColor)));
    assert_eq!(style("unassigned").fg, None);
    // Every width stays exact, the lines cut with an ellipsis.
    for w in 0..=130 {
        for line in overview(m, Some(TaskId(13)), w, Depth::Ansi256).lines {
            assert_eq!(line.width(), w, "w={w} {line:?}");
        }
    }
}

#[test]
fn the_layout_matches_what_is_drawn() {
    for board in [fixture(), led_fixture()] {
        for m in &board.milestones {
            for w in 0..=130 {
                let l = overview_layout(m, None, w);
                let o = overview(m, None, w, Depth::Ansi256);
                assert_eq!((l.lines, &l.cards), (o.lines.len(), &o.cards), "w={w}");
                // Each card's ticket, or who is on it when it has none, is drawn where the
                // layout says (tower's id is not on a card).
                for c in l.cards.iter().filter(|_| w >= 30) {
                    let t = m.tasks.iter().find(|t| t.id == c.task).unwrap();
                    let first = (t.ticket().map(|k| k.to_string()))
                        .unwrap_or_else(|| t.who(m.lead.as_ref()).to_string());
                    let row = o.lines[c.line].to_string();
                    let at: String = row.chars().skip(c.x + 1).take(1).collect();
                    assert_eq!(
                        at,
                        first.chars().take(1).collect::<String>(),
                        "w={w} {c:?}\n{row}"
                    );
                }
            }
        }
        let items = tree_items(&board, 60);
        let lines = tree_lines(&board, 60, Depth::Ansi256);
        assert_eq!(items.len(), lines.len());
        // A wrapped title's second line selects its item too: the first carries the
        // ticket, or "no ticket".
        let mut last = None;
        for (item, line) in items.iter().zip(&lines) {
            let text = line.to_string();
            let first = *item != last;
            last = *item;
            match item {
                Some(Item::Task(id)) if first => {
                    let t = board.task(*id).unwrap();
                    let ticket = t.ticket().map_or("no ticket".into(), |k| k.to_string());
                    assert!(text.contains(&format!(" {ticket} ")), "{text}")
                }
                Some(Item::Task(_)) => assert!(text.starts_with("      "), "{text}"),
                Some(Item::Milestone(_) | Item::Repo(_) | Item::Done(_)) => {}
                None => assert!(!text.contains('#'), "{text}"),
            }
        }
    }
}

#[test]
fn esc_with_only_the_overview_open_and_other_keys_change_nothing() {
    let view: View = Ok(led_fixture());
    let a = area(120, 30);
    let mut dash = clocked();
    for input in [Input::Esc, Input::Char('x'), Input::Esc] {
        let before = dash.clone();
        dash.handle(&view, a, input);
        assert_eq!(dash, before, "{input:?}");
    }
    assert_eq!((dash.views(), dash.view()), (1, ViewKind::Overview));
}

#[test]
fn in_a_narrow_pane_enter_opens_the_overview_full_width_and_esc_returns_to_the_tree() {
    let view: View = Ok(led_fixture());
    let a = area(50, 20);
    let mut dash = select(Item::Task(TaskId(12)));
    let l = layout(&view, &dash, a);
    assert!(l.overview_hidden && l.right.is_none());
    let keys = rows(&render_dash(&view, &dash, 50, 20, Depth::TrueColor))[19].clone();
    assert!(keys.contains("enter open"), "{keys}");

    dash.handle(&view, a, Input::Enter);
    assert_eq!(
        (dash.views(), dash.view(), dash.across()),
        (1, ViewKind::Overview, true)
    );
    let l = layout(&view, &dash, a);
    assert_eq!((l.tree, l.right), (None, Some(Rect::new(0, 1, 50, 17))));
    let buf = render_dash(&view, &dash, 50, 20, Depth::TrueColor);
    assert_exact_width(&buf);
    let all = rows(&buf);
    assert!(all[1].starts_with(" M1  First shop pages"), "{all:#?}");
    assert!(all[19].contains("esc back"), "{}", all[19]);
    // Up and down still move the selection; the overview follows it.
    for _ in 0..5 {
        dash.handle(&view, a, Input::Down);
    }
    assert_eq!(dash.selected, Some(Item::Task(TaskId(21))));
    // ...scrolled to the selected task's card.
    let all = rows(&render_dash(&view, &dash, 50, 20, Depth::TrueColor)).join("\n");
    assert!(
        all.contains("│ DONE 2") && all.contains("│ unassigned"),
        "{all}"
    );
    assert!(!all.contains("#113"), "{all}");
    // Enter again opens the task's detail across the pane; Esc goes back to the overview,
    // then to the tree.
    dash.handle(&view, a, Input::Enter);
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::TaskDetail));
    let l = layout(&view, &dash, a);
    assert_eq!((l.tree, l.right), (None, Some(Rect::new(0, 1, 50, 17))));
    dash.handle(&view, a, Input::Esc);
    assert_eq!(
        (dash.views(), dash.view(), dash.across()),
        (1, ViewKind::Overview, true)
    );
    dash.handle(&view, a, Input::Esc);
    assert_eq!((dash.views(), dash.across()), (1, false));
    let l = layout(&view, &dash, a);
    assert_eq!((l.tree, l.right), (Some(Rect::new(0, 1, 50, 17)), None));
    // Nothing to open on an empty board.
    let empty: View = Ok(Board::default());
    dash.handle(&empty, a, Input::Enter);
    assert_eq!(dash.views(), 1);
}

#[test]
fn a_refresh_that_removes_the_selection_starts_the_overview_at_its_top() {
    let view: View = Ok(led_fixture());
    let a = area(72, 12);
    let mut dash = select(Item::Task(TaskId(21)));
    dash.handle(&view, a, Input::PageDown);
    assert!(dash.right_scroll() > 0);
    // Still on the board: nothing moves.
    let before = dash.clone();
    dash.refreshed(&view, a);
    dash.refreshed(&Err("gone for a moment".into()), a);
    assert_eq!(dash, before);
    // M2 and its tasks removed: the first repo, from the top.
    let mut board = led_fixture();
    board.milestones.retain(|m| m.id != MilestoneId(2));
    sync(&mut board);
    dash.refreshed(&Ok(board), a);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
    assert_eq!(dash.right_scroll(), 0);
}

#[test]
fn a_click_while_the_board_cannot_be_read_does_nothing() {
    let view: View = Ok(led_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Task(TaskId(12)));
    let r = right(&layout(&view, &dash, a));
    let c = r.overview.cards[0];
    let (x, y) = (r.area.x + c.x as u16 + 2, r.area.y + c.line as u16 + 1);
    let before = dash.clone();
    let problem: View = Err("no board at /tmp/board.db yet".into());
    for input in [
        Input::Click { x, y },
        Input::Click { x: 3, y: 4 },
        Input::Down,
        Input::PageDown,
        Input::Enter,
    ] {
        dash.handle(&problem, a, input);
    }
    assert_eq!(dash, before);
}

#[test]
fn clicking_a_tree_task_reveals_its_card() {
    let view: View = Ok(led_fixture());
    let a = area(72, 12); // stacked columns: #12 is in the last, below the fold
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = rows(&render_dash(&view, &dash, 72, 12, Depth::TrueColor));
    let y = find_row(&all, "     ✓ Bookmarks") as u16;
    dash.handle(&view, a, Input::Click { x: 6, y });
    assert_eq!(dash.selected, Some(Item::Task(TaskId(12))));
    let r = right(&layout(&view, &dash, a));
    let card = r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(12))
        .unwrap();
    assert!(dash.right_scroll() > 0);
    assert!(dash.right_scroll() <= card.line);
    assert!(card.line + card.height <= dash.right_scroll() + usize::from(r.area.height));
}

#[test]
fn the_keys_line_drops_whole_hints_and_keeps_quit() {
    let view: View = Ok(led_fixture());
    for w in 8..=140u16 {
        let keys = rows(&render(&view, w, 10))[9].trim_end().to_string();
        assert!(keys.ends_with("q quit"), "w={w}: {keys:?}");
        assert!(!keys.contains('…'), "w={w}: {keys:?}");
    }
    let keys = |w| rows(&render(&view, w, 10))[9].trim_end().to_string();
    assert_eq!(keys(34), " ↑↓ select · enter open · q quit");
    // Scrolling goes first, then folding: b, i and n stay wherever they showed before.
    assert_eq!(
        keys(70),
        " ↑↓ select · enter open · b board · i inbox · n new lead · q quit"
    );
    assert_eq!(
        keys(90),
        " ↑↓ select · ←→ fold · z fold all · enter open · b board · i inbox · n new lead · q quit"
    );
    assert_eq!(
        keys(120),
        " ↑↓ select · ←→ fold · z fold all · enter open · b board · i inbox · n new lead · PgUp PgDn scroll · q quit"
    );
    assert_eq!(
        keys(140),
        " ↑↓ select · ←→ fold · z fold all · enter open · b board · i inbox · n new lead · PgUp PgDn scroll · click a row or card · q quit"
    );
    assert_eq!(keys(10), " q quit");
    // In the popup too, folding in a 100-column popup as in the design's compact line.
    let popup = |w| {
        let dash = popup_dash(Item::Milestone(MilestoneId(1)));
        rows(&render_dash(&view, &dash, w, 10, Depth::TrueColor))[9]
            .trim_end()
            .to_string()
    };
    assert_eq!(
        popup(100),
        " ↑↓ select · ←→ fold · z fold all · enter open · b board · i inbox · n new lead · esc back · q close"
    );
    assert_eq!(
        popup(80),
        " ↑↓ select · enter open · b board · i inbox · n new lead · esc back · q close"
    );
}

// Detail views: opened with Enter or a second click, closed with Esc.

/// The led fixture with details on M1 and its tasks, as the signed design's example.
fn detail_fixture() -> Board {
    let mut board = led_fixture();
    let m = &mut board.milestones[0];
    m.details = MilestoneDetails {
        context: Some(
            "One branch per task; you approve each release. Test with the demo shop only.".into(),
        ),
        spec: Some("docs/specs/first-shop-pages.md (from /to-spec)".into()),
        link: Some("https://github.com/example/bookshop/milestone/1".into()),
    };
    m.tasks[0].details.assignee = Some("dev-1".into());
    m.tasks[0].details.description =
        Some("A shelf of the newest books, so a reader sees what came in.".into());
    m.tasks[1].details = TaskDetails {
        description: Some(
            "The shelf shows the twelve newest books with their covers, newest \
             first.\nA ribbon marks the books that came in this week."
                .into(),
        ),
        context: Some("Test with the sample catalogue.".into()),
        link: Some("https://github.com/example/bookshop/issues/13".into()),
        mr: Some("!44 draft into feat/bookmarks · pipeline running".into()),
        assignee: Some("dev-2".into()),
        criteria: vec![
            Criterion {
                text: "Shows the 12 newest books, newest first".into(),
                done: true,
            },
            Criterion {
                text: "Checked on phone, tablet and desktop".into(),
                done: false,
            },
        ],
        blocked_by: vec![TaskId(14), TaskId(99)],
        ..TaskDetails::default()
    };
    m.tasks[1].subtasks = vec![
        control_tower::board::Subtask {
            id: TaskId(31),
            title: "Shelf grid".into(),
            stage: Stage::Done,
        },
        control_tower::board::Subtask {
            id: TaskId(32),
            title: "New-book ribbon".into(),
            stage: Stage::InProgress,
        },
    ];
    board
}

/// A dashboard with `item` selected and its detail opened with Enter, in a pane `a`.
fn opened(view: &View, item: Item, a: Rect) -> Dash {
    let mut dash = select(item);
    dash.handle(view, a, Input::Enter);
    dash
}

#[test]
fn detail_views_show_the_real_message_board_and_the_tasks_thread() {
    let mut board = detail_fixture();
    board.milestones[0].threads = board_fixture().milestones[0].threads.clone();
    let view: View = Ok(board);
    for (name, item) in [
        ("milestone", Item::Milestone(MilestoneId(1))),
        ("task", Item::Task(TaskId(13))),
    ] {
        let dash = opened(&view, item, area(120, 70));
        let buf = render_dash(&view, &dash, 120, 70, Depth::TrueColor);
        assert_exact_width(&buf);
        assert_snapshot(&format!("detail_board_{name}_120x70"), &buf);
        let all = rows(&buf).join("\n");
        assert!(all.contains("[open] New book shelf"), "{all}");
        // The milestone detail shows the whole board; the task's THREAD only its own.
        let t4 = all.contains("[open] Show each book's reviews");
        assert_eq!(t4, name == "milestone", "{all}");
        // Resolved T2 (no task) only on the milestone's, folded into its RESOLVED row.
        let t2 = all.contains("│ RESOLVED 1 ▸");
        assert_eq!(t2, name == "milestone", "{all}");
        assert!(!all.contains("[resolved]"), "{all}");
    }
    // A task with no thread says so, and the layout still matches what is drawn.
    let dash = opened(&view, Item::Task(TaskId(12)), area(120, 70));
    let all = rows(&render_dash(&view, &dash, 120, 70, Depth::TrueColor)).join("\n");
    assert!(all.contains("no thread for this task yet"), "{all}");
    let Ok(board) = &view else { unreachable!() };
    let m = &board.milestones[0];
    for w in [20, 50, 81, 120] {
        let ml = detail_layout(
            board,
            ViewKind::MilestoneDetail,
            Item::Milestone(m.id),
            w,
            FOLDED,
            NOW,
        )
        .unwrap();
        assert_eq!(
            ml.lines,
            milestone_detail(board, m, w, NOW, Depth::Ansi256)
                .lines
                .len(),
            "w={w}"
        );
        let tl = detail_layout(
            board,
            ViewKind::TaskDetail,
            Item::Task(TaskId(13)),
            w,
            FOLDED,
            NOW,
        )
        .unwrap();
        let td = task_detail(board, Some(m), &m.tasks[1], w, NOW, Depth::Ansi256);
        assert_eq!(tl.lines, td.lines.len(), "w={w}");
        assert!(td.lines.iter().all(|l| l.width() == w), "w={w}");
    }
}

#[test]
fn detail_snapshots_at_a_wide_and_a_narrow_width() {
    let view: View = Ok(detail_fixture());
    for (w, h) in [(120, 40), (50, 40)] {
        for (name, item, kind) in [
            (
                "milestone",
                Item::Milestone(MilestoneId(1)),
                ViewKind::MilestoneDetail,
            ),
            ("task", Item::Task(TaskId(13)), ViewKind::TaskDetail),
        ] {
            let mut dash = opened(&view, item, area(w, h));
            if w == 50 {
                // Narrow: the first Enter opens the overview across the pane, the second
                // the detail.
                dash.handle(&view, area(w, h), Input::Enter);
            }
            assert_eq!(dash.view(), kind, "{name} {w}");
            let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
            assert_exact_width(&buf);
            assert_snapshot(&format!("detail_{name}_{w}x{h}"), &buf);
        }
    }
}

#[test]
fn detail_rows_are_exactly_the_pane_width_at_any_size() {
    let view: View = Ok(detail_fixture());
    for w in [1u16, 2, 5, 12, 13, 20, 31, 45, 61, 62, 80, 97, 200] {
        for item in [Item::Milestone(MilestoneId(1)), Item::Task(TaskId(13))] {
            let a = area(w, 30);
            let mut dash = opened(&view, item, a);
            dash.handle(&view, a, Input::Enter);
            let buf = render_dash(&view, &dash, w, 30, Depth::Ansi256);
            assert_exact_width(&buf);
        }
    }
}

#[test]
fn the_task_detail_shows_every_field_and_empty_ones_say_so() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 60);
    let all = rows(&render_dash(
        &view,
        &opened(&view, Item::Task(TaskId(13)), a),
        120,
        60,
        Depth::TrueColor,
    ))
    .join("\n");
    for want in [
        "#13 New book shelf [in progress]",
        "milestone  M1 First shop pages · Catalogue Lead",
        "assigned   dev-2 · team dev-1, dev-2",
        "blocked by ○ #14 Show each book's reviews",
        "│            #99 ",
        "MR         !44 draft into feat/bookmarks · pipeline running",
        "context    Test with the sample catalogue.",
        "tracker    https://github.com/example/bookshop/issues/13",
        " DESCRIPTION",
        " A ribbon marks the books that came in this week.",
        " ACCEPTANCE CRITERIA 1/2",
        " [x] Shows the 12 newest books, newest first",
        " [ ] Checked on phone, tablet and desktop",
        " SUBTASKS 1/2",
        " [x] #31 Shelf grid",
        " [ ] #32 New-book ribbon · in progress",
        " THREAD",
        " no thread for this task yet",
    ] {
        assert!(all.contains(want), "{want:?} missing from\n{all}");
    }
    // A task with nothing set: each field says so.
    let all = rows(&render_dash(
        &view,
        &opened(&view, Item::Task(TaskId(14)), a),
        120,
        60,
        Depth::TrueColor,
    ))
    .join("\n");
    for want in [
        "assigned   unassigned · team dev-1, dev-2",
        "blocked by nothing",
        "MR         none yet",
        "context    none",
        "tracker    none",
        " no description yet",
        " ACCEPTANCE CRITERIA 0/0",
    ] {
        assert!(all.contains(want), "{want:?} missing from\n{all}");
    }
}

#[test]
fn the_milestone_detail_lists_tasks_with_their_first_description_line() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 40);
    let dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    let l = layout(&view, &dash, a);
    let d = l.detail.clone().expect("a detail layout");
    assert!(l.overview.is_none());
    // #12 and #13 have a description line under them; #14 has none.
    let heights: Vec<(TaskId, usize)> = d.rows.iter().map(|r| (r.task, r.height)).collect();
    assert_eq!(
        heights,
        vec![(TaskId(12), 2), (TaskId(13), 2), (TaskId(14), 1)]
    );
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    let all = rows(&buf);
    let r = l.right.unwrap();
    // The right pane's cells alone: the tree beside it may hold wide characters.
    let row = |line: usize| -> String {
        let y = r.y + line as u16;
        (r.x..r.x + r.width).map(|x| buf[(x, y)].symbol()).collect()
    };
    let first = d.rows[1].line;
    assert!(
        row(first).starts_with(" ● #13 New book shelf"),
        "{}",
        row(first)
    );
    assert!(
        row(first).trim_end().ends_with("dev-2 [in progress]"),
        "{}",
        row(first)
    );
    assert!(
        row(first + 1).starts_with("      The shelf shows the twelve"),
        "{}",
        row(first + 1)
    );
    let text = all.join("\n");
    for want in [
        "team       dev-1, dev-2",
        "spec       docs/specs/first-shop-pages.md (from /to-spec)",
        " TASKS 1/3",
        " MESSAGE BOARD",
        " no threads yet",
    ] {
        assert!(text.contains(want), "{want:?} missing from\n{text}");
    }
}

/// The screen position of line `line` of the right pane, scrolled as `dash` is.
fn right_cell(l: &Layout, dash: &Dash, line: usize) -> (u16, u16) {
    let r = l.right.unwrap();
    (r.x + 3, r.y + (line - dash.right_scroll()) as u16)
}

#[test]
fn a_task_row_in_the_milestone_detail_opens_its_task_and_esc_comes_back() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 40);
    let mut dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::MilestoneDetail));
    let l = layout(&view, &dash, a);
    let row = l.detail.as_ref().unwrap().rows[1];
    // Its description line opens it too.
    let (x, y) = right_cell(&l, &dash, row.line + 1);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!((dash.views(), dash.view()), (3, ViewKind::TaskDetail));
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    // Esc: back to the milestone detail with the milestone selected, then the overview.
    dash.handle(&view, a, Input::Esc);
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::MilestoneDetail));
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    dash.handle(&view, a, Input::Esc);
    assert_eq!((dash.views(), dash.view()), (1, ViewKind::Overview));
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // A click in the milestone detail off any task row opens nothing.
    let mut dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    let (x, y) = right_cell(&layout(&view, &dash, a), &dash, 1);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::MilestoneDetail));
}

#[test]
fn a_second_click_on_the_selected_row_or_card_opens_it() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    let m1 = find_row(&all, "   ▾ M1 First shop pages") as u16;
    let t13 = find_row(&all, "     ● New book") as u16;
    dash.handle(&view, a, Input::Click { x: 5, y: m1 });
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::MilestoneDetail));
    // Another row: the detail closes and that row is selected, not opened.
    dash.handle(&view, a, Input::Click { x: 5, y: t13 });
    assert_eq!((dash.views(), dash.view()), (1, ViewKind::Overview));
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    dash.handle(&view, a, Input::Click { x: 5, y: t13 });
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::TaskDetail));
    dash.handle(&view, a, Input::Esc);
    // A card: the first click selects it, the second opens it.
    let r = right(&layout(&view, &dash, a));
    let c = *r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(14))
        .unwrap();
    let (x, y) = (r.area.x + c.x as u16 + 2, r.area.y + c.line as u16);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(
        (dash.views(), dash.selected),
        (1, Some(Item::Task(TaskId(14))))
    );
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::TaskDetail));
    // Enter in a detail opens nothing more.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.views(), 2);
}

#[test]
fn moving_the_selection_closes_the_details() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 40);
    let mut dash = opened(&view, Item::Task(TaskId(13)), a);
    dash.handle(&view, a, Input::Down);
    assert_eq!((dash.views(), dash.view()), (1, ViewKind::Overview));
    assert_eq!(dash.selected, Some(Item::Task(TaskId(14))));
    // In a narrow pane the overview across the pane stays open under the detail.
    let a = area(50, 20);
    let mut dash = opened(&view, Item::Task(TaskId(13)), a);
    dash.handle(&view, a, Input::Enter);
    assert_eq!((dash.views(), dash.across()), (2, true));
    dash.handle(&view, a, Input::Up);
    assert_eq!(
        (dash.views(), dash.view(), dash.across()),
        (1, ViewKind::Overview, true)
    );
    assert_eq!(dash.selected, Some(Item::Task(TaskId(12))));
}

#[test]
fn a_refresh_that_removes_the_shown_item_closes_the_details() {
    let a = area(120, 40);
    let view: View = Ok(detail_fixture());
    let mut dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    let l = layout(&view, &dash, a);
    let (x, y) = right_cell(&l, &dash, l.detail.as_ref().unwrap().rows[1].line);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(dash.views(), 3);
    // Unchanged, or unreadable for a moment: nothing closes.
    let before = dash.clone();
    dash.refreshed(&view, a);
    dash.refreshed(&Err("gone for a moment".into()), a);
    assert_eq!(dash, before);
    // Another task removed: nothing closes.
    let mut board = detail_fixture();
    board.milestones[0].tasks.retain(|t| t.id != TaskId(14));
    dash.refreshed(&Ok(board.clone()), a);
    assert_eq!(dash, before);
    // The shown task removed: back to the overview, on the fallback selection.
    board.milestones[0].tasks.retain(|t| t.id != TaskId(13));
    dash.refreshed(&Ok(board), a);
    assert_eq!((dash.views(), dash.view()), (1, ViewKind::Overview));
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
    // The shown milestone removed, in a narrow pane: back to the overview across it.
    let a = area(50, 20);
    let mut dash = opened(&view, Item::Milestone(MilestoneId(2)), a);
    dash.handle(&view, a, Input::Enter);
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::MilestoneDetail));
    let mut board = detail_fixture();
    board.milestones.retain(|m| m.id != MilestoneId(2));
    dash.refreshed(&Ok(board), a);
    assert_eq!(
        (dash.views(), dash.view(), dash.across()),
        (1, ViewKind::Overview, true)
    );
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
}

#[test]
fn a_detail_scrolls_on_its_own_and_esc_keeps_the_overview_scroll() {
    let view: View = Ok(detail_fixture());
    let a = area(72, 12);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, a, Input::PageDown);
    let overview_scroll = dash.right_scroll();
    assert!(overview_scroll > 0);
    dash.handle(&view, a, Input::Enter);
    assert_eq!(
        (dash.view(), dash.right_scroll()),
        (ViewKind::TaskDetail, 0)
    );
    let l = layout(&view, &dash, a);
    let (r, lines) = (l.right.unwrap(), l.right_lines().unwrap());
    let last = lines - usize::from(r.height);
    let wheel = |dash: &mut Dash, down| {
        dash.handle(
            &view,
            a,
            Input::Wheel {
                x: r.x + 2,
                y: r.y + 1,
                down,
            },
        );
    };
    wheel(&mut dash, true);
    assert_eq!(dash.right_scroll(), 3);
    for _ in 0..20 {
        dash.handle(&view, a, Input::PageDown);
    }
    assert_eq!(dash.right_scroll(), last);
    // The last line shows at the bottom.
    let all = rows(&render_dash(&view, &dash, 72, 12, Depth::TrueColor));
    assert!(all[9].contains("no thread for this task yet"), "{all:#?}");
    for _ in 0..20 {
        wheel(&mut dash, false);
    }
    assert_eq!(dash.right_scroll(), 0);
    dash.handle(&view, a, Input::PageDown);
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.right_scroll(), overview_scroll);
}

#[test]
fn the_keys_line_offers_open_on_the_overview_and_back_on_a_detail() {
    let view: View = Ok(detail_fixture());
    let a = area(120, 30);
    let keys = |dash: &Dash| {
        rows(&render_dash(&view, dash, 120, 30, Depth::TrueColor))[29]
            .trim_end()
            .to_string()
    };
    let mut dash = select(Item::Task(TaskId(13)));
    assert!(keys(&dash).contains("enter open"), "{}", keys(&dash));
    dash.handle(&view, a, Input::Enter);
    assert_eq!(
        keys(&dash),
        " ↑↓ select · ←→ fold · z fold all · esc back · o open ticket · b board · i inbox · n new lead · q quit"
    );
}

#[test]
fn a_card_shows_its_assignee_in_place_of_unassigned() {
    let view: View = Ok(detail_fixture());
    let buf = render_dash(
        &view,
        &select(Item::Task(TaskId(13))),
        120,
        30,
        Depth::TrueColor,
    );
    let all = rows(&buf).join("\n");
    // #14 waiting, #13 in progress and #12 done, in their columns' cards.
    for want in ["│ unassigned ", "│ #13 dev-2 · 1/2 ", "│ dev-1 "] {
        assert!(all.contains(want), "{want:?} missing from\n{all}");
    }
}

/// `o` opens the selected task's ticket, its tracker link, as does a click on the
/// ticket in the tree; a task with no link says so, and `o` does nothing where the tree's
/// selection is not on screen (the full board, the inbox).
#[test]
fn o_or_a_click_on_its_ticket_opens_a_tasks_tracker_link() {
    let view: View = Ok(fixture());
    let a = area(120, 30);
    let link = "https://github.com/example/bookshop/issues/113".to_string();
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(
        dash.handle(&view, a, Input::Char('o')),
        Some(Request::Open(link.clone()))
    );
    assert_eq!(
        dash.notice,
        Some(("opening #113 in the browser".into(), false))
    );
    // No link: no request, and the notice says how to give it one.
    let mut dash = select(Item::Task(TaskId(12)));
    assert_eq!(dash.handle(&view, a, Input::Char('o')), None);
    assert_eq!(
        dash.notice,
        Some((
            "this task has no ticket (tower task set 12 --link <url>)".into(),
            false
        ))
    );
    // On the full board `o` opens nothing.
    let led: View = Ok(led_fixture());
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&led, a, Input::Char('b'));
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.handle(&led, a, Input::Char('o')), None);
    // A click on the ticket in the tree opens it and selects its task; a click on the
    // title only selects.
    let mut dash = select(Item::Task(TaskId(12)));
    let all = screen(&view, &dash, a);
    let y = find_row(&all, "     ● New book shelf");
    let x = all[y]
        .find("#113")
        .map(|b| all[y][..b].chars().count())
        .unwrap() as u16;
    assert_eq!(
        dash.handle(
            &view,
            a,
            Input::Click {
                x: x + 1,
                y: y as u16
            }
        ),
        Some(Request::Open(link))
    );
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    let mut dash = select(Item::Task(TaskId(12)));
    assert_eq!(
        dash.handle(&view, a, Input::Click { x: 8, y: y as u16 }),
        None
    );
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    // The keys line names it while a task is selected.
    let keys = screen(&view, &dash, a).last().unwrap().clone();
    assert!(keys.contains("o open ticket"), "{keys}");
}

/// A task's detail says its id as the task's, "task #13", and has its ticket.
#[test]
fn a_tasks_detail_names_its_id_as_the_tasks_and_shows_its_ticket() {
    let view: View = Ok(fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, a, Input::Enter);
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("task #13 New book shelf"), "{all}");
    assert!(all.contains(" ticket     #113"), "{all}");
    let mut dash = select(Item::Task(TaskId(12)));
    dash.handle(&view, a, Input::Enter);
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains(" ticket     no ticket"), "{all}");
}

/// What the screen shows, for checking that an input changed it.
fn screen(view: &View, dash: &Dash, a: Rect) -> Vec<String> {
    rows(&render_dash(
        view,
        dash,
        a.width,
        a.height,
        Depth::TrueColor,
    ))
}

#[test]
fn every_esc_closes_something_that_shows_even_after_the_pane_is_widened() {
    let view: View = Ok(detail_fixture());
    let (narrow, wide) = (area(50, 20), area(120, 30));
    // Narrow: the overview across the pane, Down, then the pane is widened.
    let mut dash = select(Item::Task(TaskId(12)));
    dash.handle(&view, narrow, Input::Enter);
    dash.handle(&view, narrow, Input::Down);
    assert!(dash.across());
    let keys = screen(&view, &dash, wide)[29].clone();
    assert!(!keys.contains("esc back"), "{keys}");
    // Wide, nothing is open over the overview: Esc changes nothing, and opening then
    // closing a detail takes one Esc each way.
    let before = screen(&view, &dash, wide);
    dash.handle(&view, wide, Input::Esc);
    assert_eq!(screen(&view, &dash, wide), before);
    assert!(!dash.across());
    dash.handle(&view, wide, Input::Enter);
    assert_eq!((dash.views(), dash.view()), (2, ViewKind::TaskDetail));
    dash.handle(&view, wide, Input::Esc);
    assert_eq!(screen(&view, &dash, wide), before);
    // Narrow again: the tree, since the overview across it was closed while wide.
    assert_eq!(
        layout(&view, &dash, narrow).tree,
        Some(Rect::new(0, 1, 50, 17))
    );
    // Narrow with a detail open, widened, narrowed: each Esc while "esc back" shows changes
    // the screen, and then "esc back" is gone.
    dash.handle(&view, narrow, Input::Enter);
    dash.handle(&view, narrow, Input::Enter);
    dash.handle(&view, wide, Input::Down);
    dash.handle(&view, narrow, Input::Enter);
    dash.handle(&view, narrow, Input::Enter);
    for a in [narrow, wide] {
        let mut d = dash.clone();
        let mut escs = 0;
        while screen(&view, &d, a)[usize::from(a.height) - 1].contains("esc back") {
            let before = screen(&view, &d, a);
            d.handle(&view, a, Input::Esc);
            assert_ne!(
                screen(&view, &d, a),
                before,
                "Esc {escs} at width {}",
                a.width
            );
            escs += 1;
            assert!(escs < 5);
        }
        assert_eq!(escs, if a == narrow { 2 } else { 1 }, "width {}", a.width);
    }
}

#[test]
fn the_detail_layout_needs_no_colour_depth_and_matches_what_is_drawn() {
    let board = detail_fixture();
    let m = &board.milestones[0];
    for w in [1usize, 13, 30, 50, 79, 120] {
        let ml = detail_layout(
            &board,
            ViewKind::MilestoneDetail,
            Item::Milestone(m.id),
            w,
            FOLDED,
            NOW,
        );
        let tl = detail_layout(
            &board,
            ViewKind::TaskDetail,
            Item::Task(TaskId(13)),
            w,
            FOLDED,
            NOW,
        );
        for depth in [Depth::TrueColor, Depth::Ansi256] {
            let md = milestone_detail(&board, m, w, NOW, depth);
            assert_eq!(ml.as_ref(), Some(&md.layout), "milestone w={w}");
            assert_eq!(md.layout.lines, md.lines.len(), "milestone w={w}");
            let td = task_detail(&board, Some(m), &m.tasks[1], w, NOW, depth);
            assert_eq!(tl.as_ref(), Some(&td.layout), "task w={w}");
            assert_eq!(td.layout.lines, td.lines.len(), "task w={w}");
        }
    }
    // The overview is not a detail view.
    assert_eq!(
        detail_layout(
            &board,
            ViewKind::Overview,
            Item::Milestone(m.id),
            80,
            FOLDED,
            NOW
        ),
        None
    );
}

#[test]
fn widening_a_narrow_pane_closes_the_overview_open_across_it() {
    let view: View = Ok(detail_fixture());
    let (narrow, wide) = (area(50, 20), area(120, 30));
    let mut dash = clocked();
    dash.handle(&view, narrow, Input::Enter);
    assert!(dash.across());
    // Still narrow: nothing changes.
    dash.fit(&view, narrow);
    assert!(dash.across());
    // Widened, as the input loop does before every frame: the flag goes with no input.
    dash.fit(&view, wide);
    assert!(!dash.across());
    // Nothing is open, so Esc does nothing and "esc back" is not offered.
    let before = screen(&view, &dash, wide);
    assert!(!before[29].contains("esc back"), "{}", before[29]);
    let state = dash.clone();
    dash.handle(&view, wide, Input::Esc);
    assert_eq!((screen(&view, &dash, wide), &dash), (before, &state));
    // Narrow again: the tree, not the overview.
    assert_eq!(
        layout(&view, &dash, narrow).tree,
        Some(Rect::new(0, 1, 50, 17))
    );
    // With a detail open when widened, one Esc closes that detail on screen.
    dash.handle(&view, narrow, Input::Enter);
    dash.handle(&view, narrow, Input::Enter);
    dash.fit(&view, wide);
    let before = screen(&view, &dash, wide);
    dash.handle(&view, wide, Input::Esc);
    assert_ne!(screen(&view, &dash, wide), before);
    assert_eq!((dash.views(), dash.across()), (1, false));
}

// Standalone tasks: their own group at the bottom of the tree, and a task detail straight
// away.

/// The led fixture with three standalone tasks: one assigned with subtasks, one with a
/// title long enough to be cut, one done.
fn standalone_fixture() -> Board {
    use Stage::*;
    let mut board = led_fixture();
    let task = |id, title: &str, stage| Task {
        id: TaskId(id),
        title: title.into(),
        stage,
        repo: None,
        subtasks: Vec::new(),
        details: TaskDetails::default(),
    };
    let mut poetry = task(41, "Sort the poetry shelf by poet", InProgress);
    poetry.details.assignee = Some("dev-3".into());
    // A GitLab work item, shown as its ticket "#64".
    poetry.details.link = Some("https://gitlab.example.com/acme/bookshop/-/work_items/64".into());
    poetry.details.description = Some("Books by the same poet sit together.".into());
    poetry.subtasks = [(51, Done), (52, Waiting), (53, Waiting)]
        .map(|(id, stage)| control_tower::board::Subtask {
            id: TaskId(id),
            title: format!("step {id}"),
            stage,
        })
        .to_vec();
    board.standalone = vec![
        poetry,
        task(
            42,
            "Add a notes box to each bookmark with a word limit",
            Waiting,
        ),
        task(43, "Dust the shelves", Done),
    ];
    board
}

/// `board` as a board with no repos at all shows: everything in the No repo yet group, its
/// milestones keeping their leads, as the only board where a standalone task is still
/// handed to a lead from the dashboard.
fn no_repos(mut board: Board) -> Board {
    board.repos.clear();
    board
}

/// The standalone fixture's tasks in the catalogue repo instead, as v0.1.7 shows a
/// repo's standalone tasks: under its milestones.
fn repo_standalone_fixture() -> Board {
    let mut board = standalone_fixture();
    for t in &mut board.standalone {
        t.repo = Some(RepoId(1));
    }
    board
}

#[test]
fn standalone_group_snapshots_at_a_wide_and_a_narrow_width() {
    for (name, board) in [
        ("standalone", standalone_fixture()),
        ("repo_standalone", repo_standalone_fixture()),
    ] {
        let view: View = Ok(board);
        for w in [80, 34] {
            let buf = render(&view, w, 26);
            assert_exact_width(&buf);
            assert_snapshot(&format!("{name}_{w}x26"), &buf);
        }
    }
}

#[test]
fn the_standalone_group_counts_done_tasks_and_shows_assignee_and_subtasks() {
    let board = repo_standalone_fixture();
    let group = " ▾ catalogue";
    let tree: Vec<String> = (tree_lines(&board, 50, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    let from = find_row(&tree, group);
    let head = from + find_row(&tree[from..], "   standalone · no milestone");
    // A blank line gives the dim label air from what is above it.
    assert_eq!(tree[head - 1].trim(), "", "{tree:#?}");
    assert!(tree[head].ends_with(" 1/3 done "), "{:?}", tree[head]);
    let poetry = &tree[head + 1];
    assert!(
        poetry.starts_with("     ● Sort the poetry shelf by"),
        "{poetry:?}"
    );
    assert!(poetry.ends_with(" #64 · dev-3 · 1/3 "), "{poetry:?}");
    // A long title wraps to an indented second line, its ticket and assignee on the first.
    let at = find_row(&tree, "     ○ Add a notes");
    assert!(
        tree[at].ends_with(" no ticket · unassigned "),
        "{:?}",
        tree[at]
    );
    assert!(tree[at + 1].starts_with("       "), "{:?}", tree[at + 1]);
    let last = &tree[find_row(&tree, "     ✓ Dust the shelves")];
    assert!(last.ends_with(" no ticket · unassigned "), "{last:?}");
    // In a repo, they come after its milestones, before the next repo's rule.
    let tree: Vec<String> = (tree_lines(&repo_standalone_fixture(), 40, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    let head = find_row(&tree, "   standalone");
    assert!(head > find_row(&tree, "   ▾ M1") && head < find_row(&tree, " ▾ checkout"));
    // In no repo: no label of their own, right under the group's hint after a blank line.
    let tree: Vec<String> = (tree_lines(&standalone_fixture(), 50, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    let from = find_row(&tree, " No repo yet");
    assert!(
        !tree[from..].iter().any(|l| l.contains("standalone")),
        "{tree:#?}"
    );
    let poetry = find_row(&tree, "     ● Sort the poetry shelf by");
    assert_eq!(tree[poetry - 1].trim(), "", "{tree:#?}");
    assert!(tree[poetry].ends_with(" #64 · dev-3 · 1/3 "), "{tree:#?}");
    // No standalone tasks, no label.
    let all = rows(&render(&Ok(led_fixture()), 34, 26));
    assert!(!all.iter().any(|r| r.contains("standalone")));
}

#[test]
fn a_narrow_tree_drops_the_assignee_before_the_ticket() {
    let mut board = standalone_fixture();
    board.standalone[0].details.assignee = Some("a-very-long-assignee-name".into());
    for w in [20u16, 30, 40] {
        let lines = tree_lines(&board, usize::from(w), Depth::TrueColor);
        let row = lines
            .iter()
            .map(|l| l.to_string())
            .find(|l| l.contains("#64"));
        let row = row.unwrap_or_else(|| panic!("the ticket #64 cut away at {w}"));
        assert_eq!(row.width(), usize::from(w), "{row:?}");
        assert!(!row.contains("a-very"), "{w}: {row:?}");
    }
    // Too narrow for the ticket beside a few columns of title: the title alone.
    let lines = tree_lines(&board, 12, Depth::TrueColor);
    let row = lines
        .iter()
        .map(|l| l.to_string())
        .find(|l| l.contains("● Sor"));
    assert_eq!(row.as_deref(), Some("     ● Sort "));
}

#[test]
fn up_and_down_reach_the_standalone_tasks_and_skip_the_group_head() {
    let view: View = Ok(repo_standalone_fixture());
    let a = area(120, 40);
    // In its repo, after its last milestone's last task, the label skipped.
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(41))));
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(43))));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(2))));
    for _ in 0..4 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.selected, Some(Item::Task(TaskId(14))));
    // The label and the blank line above it select nothing.
    let l = layout(&view, &dash, a);
    let all = screen(&view, &dash, a);
    let head = find_row(&all, "   standalone") as u16;
    for y in [head - 1, head] {
        let mut d = dash.clone();
        d.handle(&view, a, Input::Click { x: 3, y });
        assert_eq!(d.selected, dash.selected, "row {y}");
    }
    let notes = find_row(&all, "     ○ Add a") as u16;
    assert_eq!(l.hit(&dash, 5, notes), Some(Item::Task(TaskId(42))));
    // Its second line selects it too.
    assert_eq!(l.hit(&dash, 5, notes + 1), Some(Item::Task(TaskId(42))));
}

#[test]
fn selecting_a_standalone_task_shows_its_detail_straight_away() {
    let view: View = Ok(standalone_fixture());
    let a = area(120, 36);
    let mut dash = select(Item::Task(TaskId(12)));
    let all = screen(&view, &dash, a);
    let y = find_row(&all, "     ● Sort the") as u16;
    dash.handle(&view, a, Input::Click { x: 6, y });
    assert_eq!(dash.selected, Some(Item::Task(TaskId(41))));
    let l = layout(&view, &dash, a);
    assert!(l.overview.is_none());
    assert!(l.detail.is_some());
    let buf = render_dash(&view, &dash, 120, 36, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("standalone_detail_120x36", &buf);
    let all = rows(&buf);
    assert!(
        all.iter()
            .any(|r| r
                .contains(" milestone  standalone, in no repo yet: tower task set 41 --repo R<n>")),
        "{all:#?}"
    );
    // No repo, so no thread; nothing to close, nothing more to open and no hand to a lead
    // once repos exist.
    assert!(!all.iter().any(|r| r.contains("THREAD")));
    let keys = all.last().unwrap();
    assert!(!keys.contains("h hand to a lead"), "{keys:?}");
    assert!(
        !keys.contains("enter open") && !keys.contains("esc back"),
        "{keys:?}"
    );
    let before = dash.clone();
    for input in [Input::Enter, Input::Esc] {
        dash.handle(&view, a, input);
        assert_eq!(dash, before, "{input:?}");
    }
    // A second click on its row opens nothing more either.
    dash.handle(&view, a, Input::Click { x: 6, y });
    assert_eq!(dash, before);
    // Moving up to a milestone brings its overview back.
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(3))));
    assert!(layout(&view, &dash, a).overview.is_some());
}

#[test]
fn in_a_narrow_pane_enter_opens_a_standalone_task_detail_across_and_esc_returns() {
    let view: View = Ok(standalone_fixture());
    let a = area(50, 30);
    let mut dash = select(Item::Task(TaskId(42)));
    let l = layout(&view, &dash, a);
    assert!(l.right.is_none() && l.overview_hidden);
    dash.handle(&view, a, Input::Enter);
    let l = layout(&view, &dash, a);
    assert_eq!(l.right, Some(Rect::new(0, 1, 50, 27)));
    assert!(l.detail.is_some() && l.tree.is_none());
    let buf = render_dash(&view, &dash, 50, 30, Depth::TrueColor);
    assert_exact_width(&buf);
    assert!(
        rows(&buf)[1].starts_with(" task #42 Add"),
        "{:#?}",
        rows(&buf)
    );
    dash.handle(&view, a, Input::Esc);
    assert!(layout(&view, &dash, a).tree.is_some());
}

#[test]
fn the_keys_line_shows_a_notice_until_the_next_input() {
    let view: View = Ok(no_repos(standalone_fixture()));
    let a = area(80, 20);
    let mut dash = select(Item::Task(TaskId(41)));
    dash.notice = Some(("T3 is resolved".into(), true));
    let buf = render_dash(&view, &dash, 80, 20, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_eq!(rows(&buf)[19].trim_end(), " T3 is resolved");
    assert_eq!(buf[(1, 19)].fg, bad(Depth::TrueColor));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.notice, None);
}

#[test]
fn a_standalone_task_on_a_board_with_no_repos_names_the_cli_hand_and_h_does_nothing() {
    // The dashboard no longer hands a task to a lead; `tower task hand` does.
    let view: View = Ok(no_repos(standalone_fixture()));
    let a = area(120, 30);
    let mut dash = select(Item::Task(TaskId(41)));
    let before = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&before);
    assert!(
        all.iter()
            .any(|r| r.contains(" milestone  standalone, no lead yet: tower task hand 41 M<n>")),
        "{all:#?}"
    );
    assert!(
        !all.iter().any(|r| r.contains("hand to a lead")),
        "{all:#?}"
    );
    assert!(!all.last().unwrap().contains(" h "), "{:?}", all.last());
    // h asks for nothing and changes nothing on screen.
    assert_eq!(dash.handle(&view, a, Input::Char('h')), None);
    assert_eq!(
        rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor)),
        rows(&before)
    );
}

#[test]
fn an_error_in_the_keys_line_is_one_line_cut_to_the_width() {
    let view: View = Ok(standalone_fixture());
    let mut dash = select(Item::Task(TaskId(41)));
    let long = "x".repeat(300);
    dash.notice = Some((
        format!("tower: refused\n\tsecond line\x1b[31m\r{long}"),
        true,
    ));
    for w in [10u16, 40, 80] {
        let buf = render_dash(&view, &dash, w, 12, Depth::TrueColor);
        assert_exact_width(&buf);
        let keys = &rows(&buf)[11];
        assert!(keys.starts_with(" tower:"), "{keys:?}");
        assert!(keys.ends_with('…'), "{keys:?}");
        assert!(!keys.chars().any(char::is_control), "{keys:?}");
    }
    let keys = &rows(&render_dash(&view, &dash, 80, 12, Depth::TrueColor))[11];
    assert!(
        keys.starts_with(" tower: refused second line [31m xxx"),
        "{keys:?}"
    );
}

#[test]
fn every_row_is_exactly_the_width_with_standalone_tasks() {
    let view: View = Ok(no_repos(standalone_fixture()));
    for w in (1..=40).chain([61, 62, 79, 80, 120, 200]) {
        for h in [1u16, 3, 5, 8, 30] {
            let a = area(w, h);
            let mut dash = select(Item::Task(TaskId(42)));
            assert_exact_width(&render_dash(&view, &dash, w, h, Depth::Ansi256));
            dash.handle(&view, a, Input::Enter);
            assert_exact_width(&render_dash(&view, &dash, w, h, Depth::Ansi256));
        }
    }
}

#[test]
fn a_standalone_task_loaded_from_the_board_shows_in_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    store.add_milestone("M", "g", Some(r)).unwrap();
    let t = store.add_standalone_task("Loose end", Some(r)).unwrap();
    store.add_subtask(t, "step").unwrap();
    store.add_standalone_task("Stray", None).unwrap();
    let tree: Vec<String> = match refresh(&path) {
        Ok(board) => (tree_lines(&board, 60, Depth::Ansi256).iter())
            .map(|l| l.to_string())
            .collect(),
        Err(e) => panic!("{e}"),
    };
    // In its repo, under its milestones...
    let head = find_row(&tree, "   standalone");
    assert!(head < find_row(&tree, " No repo yet"), "{tree:#?}");
    assert!(tree[head].ends_with(" 0/1 done "), "{tree:#?}");
    assert!(
        tree[head + 1].ends_with(" no ticket · unassigned · 0/1 "),
        "{tree:#?}"
    );
    // ...and one in no repo in the last group.
    assert!(tree.last().unwrap().contains("Stray"), "{tree:#?}");
}

// The full message board.

/// The screen (x, y) of line `line` of the right pane's view, as scrolled now.
fn right_line(l: &Layout, dash: &Dash, line: usize) -> (u16, u16) {
    let r = l.right.expect("a right pane");
    let scroll = (dash.right_scroll()).min(
        l.right_lines()
            .unwrap()
            .saturating_sub(usize::from(r.height)),
    );
    (r.x + 3, r.y + (line - scroll) as u16)
}

/// The full board's layout on `view` in `a`, which must be open.
fn board_of(view: &View, dash: &Dash, a: Rect) -> control_tower::ui::BoardLayout {
    layout(view, dash, a).board.expect("the full board")
}

#[test]
fn b_opens_the_full_board_and_esc_returns_to_where_the_owner_came_from() {
    let view: View = Ok(board_fixture());
    let a = area(120, 40);
    for opened in [vec![], vec![Input::Enter]] {
        let mut dash = select(Item::Milestone(MilestoneId(1)));
        for i in &opened {
            dash.handle(&view, a, *i);
        }
        let before = screen(&view, &dash, a);
        let kind = dash.view();
        assert!(before[39].contains("b board"), "{}", before[39]);
        dash.handle(&view, a, Input::Char('b'));
        assert_eq!(dash.view(), ViewKind::Board);
        let shown = screen(&view, &dash, a);
        assert!(
            shown[0]
                .starts_with(" ◀ back  MESSAGE BOARD · catalogue · Catalogue Lead · whole repo"),
            "{}",
            shown[0]
        );
        assert!(
            shown[39].contains(
                "↑↓ threads · → read its messages · type to write · enter posts · \
                 alt+enter new line · esc back"
            ),
            "{}",
            shown[39]
        );
        // Opened on the milestone, it shows the first open thread.
        assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
        dash.handle(&view, a, Input::Esc);
        assert_eq!(dash.view(), kind);
        assert_eq!(screen(&view, &dash, a), before);
    }
    // On a task it opens that task's thread.
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    // A milestone without a lead has no board: b does nothing and is not offered.
    let mut dash = select(Item::Milestone(MilestoneId(3)));
    let keys = screen(&view, &dash, a)[39].clone();
    assert!(!keys.contains("b board"), "{keys}");
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.view(), ViewKind::Overview);
}

#[test]
fn a_point_after_a_space_a_mention_or_a_tag_is_drawn_as_the_copy_draws_it() {
    // A point (U+05B8) draws on the cell before it: a line that breaks at the space before
    // one starts with it, as the signed design's (UI v0.1.14) does; after a mention or a
    // tag it stays on the letter it was written on. A Prepend character (U+0D4E, MALAYALAM
    // LETTER DOT REPH) joins the letter after it instead, and draws with it wherever it is.
    // Invented text, as the last message of the thread `b` opens.
    let (q, reph) = ('\u{05b8}', '\u{0d4e}');
    let breaks = |mark: &str| -> String {
        let words: Vec<String> = (1..=10)
            .map(|n| format!("{} {mark}y", "k".repeat(n)))
            .collect();
        words.join(" ")
    };
    let cases = [
        ("a point at a line start", breaks(&format!("{q}x")), q, 10),
        (
            "a point after a mention",
            format!("the shelf grid is ready for @manager{q} to check on a phone"),
            q,
            1,
        ),
        (
            "a point after a tag",
            format!("blocked:{q}x the shelf is late"),
            q,
            1,
        ),
        (
            "a reph at a message start",
            format!("{reph}\u{0d30} the crate is packed"),
            reph,
            1,
        ),
        (
            "a reph after a mention",
            format!("ready for @manager{reph}\u{0d30} to check"),
            reph,
            1,
        ),
        (
            "a reph at a line start",
            breaks(&format!("{reph}\u{0d30}")),
            reph,
            10,
        ),
    ];
    for (what, text, mark, written) in cases {
        let mut board = board_fixture();
        let thread = &mut board.milestones[0].threads[1];
        assert_eq!(thread.id, ThreadId(3));
        thread.messages.last_mut().unwrap().text = text.clone();
        sync(&mut board);
        let view: View = Ok(board);
        for w in 60..=130 {
            let a = area(w, 60);
            let mut dash = select(Item::Milestone(MilestoneId(1)));
            dash.handle(&view, a, Input::Char('b'));
            let conv = layout(&view, &dash, a).board.unwrap().conv;
            let buf = render_dash(&view, &dash, w, 60, Depth::TrueColor);
            let drawn: usize = (conv.top()..conv.bottom())
                .flat_map(|y| (conv.left()..conv.right()).map(move |x| (x, y)))
                .map(|at| buf[at].symbol().matches(mark).count())
                .sum();
            assert_eq!(
                drawn,
                written,
                "{what} at {w}: {text:?}\n{}",
                rows(&buf).join("\n")
            );
        }
    }
}

#[test]
fn clicking_a_thread_box_or_its_earlier_line_opens_that_thread() {
    let view: View = Ok(board_fixture());
    let a = area(120, 90);
    let dash = select(Item::Milestone(MilestoneId(1)));
    let l = layout(&view, &dash, a);
    let all = screen(&view, &dash, a);
    for (needle, want) in [
        ("3 earlier messages", 3),
        // Threads read in words: T4 by its task's title.
        ("[open] Show each book's reviews", 4),
    ] {
        let y = all.iter().position(|r| r.contains(needle)).unwrap() as u16;
        let mut d = dash.clone();
        d.handle(&view, a, Input::Click { x: 60, y });
        assert_eq!(d.view(), ViewKind::Board, "{needle}");
        assert_eq!(d.board_thread(&view), Some(ThreadId(want)), "{needle}");
    }
    // The box borders count; the blank line above the summary does not.
    let t3 = l.threads.iter().find(|t| t.thread == ThreadId(3)).unwrap();
    let (x, y) = right_line(&l, &dash, t3.line);
    let mut d = dash.clone();
    d.handle(&view, a, Input::Click { x, y });
    assert_eq!(d.board_thread(&view), Some(ThreadId(3)));
    let (x, y) = right_line(&l, &dash, t3.line - 2);
    let mut d = dash.clone();
    d.handle(&view, a, Input::Click { x, y });
    assert_eq!(d.view(), ViewKind::Overview);
    // From a detail view's thread box too, and Esc comes back to the detail.
    let mut d = select(Item::Task(TaskId(13)));
    d.handle(&view, a, Input::Enter);
    let l = layout(&view, &d, a);
    let t3 = l.threads.iter().find(|t| t.thread == ThreadId(3)).unwrap();
    let (x, y) = right_line(&l, &d, t3.line + 1);
    d.handle(&view, a, Input::Click { x, y });
    assert_eq!(d.board_thread(&view), Some(ThreadId(3)));
    d.handle(&view, a, Input::Esc);
    assert_eq!(d.view(), ViewKind::TaskDetail);
}

#[test]
fn a_board_opened_on_a_resolved_thread_lists_it_with_the_group_unfolded() {
    let view: View = Ok(board_fixture());
    let a = area(120, 90);
    let dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    // Its line under the overview's RESOLVED row, unfolded.
    let y = (all.iter())
        .position(|r| r.contains("│ RESOLVED 1 ▸"))
        .unwrap() as u16;
    let mut d = dash.clone();
    d.handle(&view, a, Input::Click { x: 60, y });
    d.handle(&view, a, Input::Click { x: 60, y: y + 1 });
    assert_eq!(d.board_thread(&view), Some(ThreadId(2)));
    let b = board_of(&view, &d, a);
    assert!(b.fold.unfolded, "{b:?}");
    let row = (b.list_rows.iter())
        .position(|r| *r == Some(Stop::Thread(ThreadId(2))))
        .unwrap();
    let buf = render_dash(&view, &d, 120, 90, Depth::TrueColor);
    let y = b.list.y + row as u16;
    assert!(rows(&buf)[usize::from(y)].starts_with(" ✓ Ribbon hides the cover"));
    assert_eq!(buf[(5, y)].bg, selected_bg(Depth::TrueColor));
    // Moving off it keeps the group open: the list does not jump under the owner.
    d.handle(&view, a, Input::Up);
    d.handle(&view, a, Input::Up);
    assert_eq!(d.board_thread(&view), Some(ThreadId(4)));
    assert!(board_of(&view, &d, a).fold.unfolded);
    // A board with no open thread shows its first resolved one, listed.
    let mut board = board_fixture();
    (board.milestones[0].threads).retain(|t| t.status() == ThreadStatus::Resolved);
    sync(&mut board);
    let only: View = Ok(board);
    let mut d = select(Item::Milestone(MilestoneId(1)));
    d.handle(&only, a, Input::Char('b'));
    let b = board_of(&only, &d, a);
    assert_eq!(b.thread, Some(ThreadId(2)));
    assert!(
        b.list_rows.contains(&Some(Stop::Thread(ThreadId(2)))),
        "{b:?}"
    );
}

#[test]
fn a_thread_resolving_while_shown_unfolds_its_group_and_stays_in_view() {
    let view: View = Ok(board_fixture());
    // A list of five lines: OPEN, T4's three (its name wraps), a blank, RESOLVED; T3's
    // resolved line would be below. (The strip has a blank row under it.)
    let a = area(120, 11);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    dash.refreshed(&view, a);
    let mut board = board_fixture();
    for t in &mut board.milestones[0].threads {
        if t.id == ThreadId(3) {
            t.resolved = Some(Resolution {
                by: "cat-lead".into(),
                at: NOW,
            });
        }
    }
    sync(&mut board);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    let b = board_of(&later, &dash, a);
    assert!(b.fold.unfolded, "{b:?}");
    let row = (b.list_rows.iter())
        .position(|r| *r == Some(Stop::Thread(ThreadId(3))))
        .unwrap();
    assert!(row >= usize::from(b.list.height), "{row} {b:?}");
    let top = dash.list_scroll();
    assert!(
        (top..top + usize::from(b.list.height)).contains(&row),
        "{row} {top}"
    );
}

#[test]
fn a_thread_resolving_while_shown_in_an_unfolded_group_stays_in_view() {
    let view: View = Ok(board_fixture());
    // A list of four lines (the strip has a blank row under it).
    let a = area(120, 10);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    // Down to RESOLVED, unfold it, and back up to T3: the group stays unfolded, so T3
    // resolving changes no fold, only the status of the thread shown.
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    assert_eq!(board_of(&view, &dash, a).current(), Some(Stop::Resolved));
    dash.handle(&view, a, Input::Right);
    dash.handle(&view, a, Input::Up);
    dash.handle(&view, a, Input::Up);
    dash.fit(&view, a);
    let b = board_of(&view, &dash, a);
    assert_eq!(b.current(), Some(Stop::Thread(ThreadId(3))), "{b:?}");
    assert!(b.fold.unfolded && !b.fold.on_row, "{b:?}");
    dash.refreshed(&view, a);
    let before = dash.list_scroll();
    let mut board = board_fixture();
    for t in &mut board.milestones[0].threads {
        if t.id == ThreadId(3) {
            t.resolved = Some(Resolution {
                by: "cat-lead".into(),
                at: NOW,
            });
        }
    }
    sync(&mut board);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    let b = board_of(&later, &dash, a);
    assert!(b.fold.unfolded, "{b:?}");
    let height = usize::from(b.list.height);
    let row = (b.list_rows.iter())
        .position(|r| *r == Some(Stop::Thread(ThreadId(3))))
        .unwrap();
    // T3's resolved line is out of sight where the list was scrolled ...
    assert!(
        !(before..before + height).contains(&row),
        "{row} {before} {b:?}"
    );
    // ... and the list scrolls to it.
    let top = dash.list_scroll();
    assert!((top..top + height).contains(&row), "{row} {top}");
}

#[test]
fn the_thread_shown_stays_in_view_when_an_open_thread_above_it_resolves() {
    // T4 is shown, just below T3; T3 resolves, so T4's lines move up the list
    // while neither the fold nor T4 changes. At every height, folded and unfolded, T4's
    // title stays in view, and all of it where the list has room.
    for unfold in [false, true] {
        for h in 6..=12 {
            let view: View = Ok(board_fixture());
            let a = area(120, h);
            let mut dash = select(Item::Milestone(MilestoneId(1)));
            dash.handle(&view, a, Input::Char('b'));
            dash.handle(&view, a, Input::Down);
            if unfold {
                dash.handle(&view, a, Input::Down);
                dash.handle(&view, a, Input::Right);
                dash.handle(&view, a, Input::Up);
            }
            dash.fit(&view, a);
            let b = board_of(&view, &dash, a);
            assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
            assert_eq!(b.fold.unfolded, unfold, "{b:?}");
            dash.refreshed(&view, a);
            let mut board = board_fixture();
            for t in &mut board.milestones[0].threads {
                if t.id == ThreadId(3) {
                    t.resolved = Some(Resolution {
                        by: "cat-lead".into(),
                        at: NOW,
                    });
                }
            }
            sync(&mut board);
            let later: View = Ok(board);
            dash.refreshed(&later, a);
            dash.fit(&later, a);
            let b = board_of(&later, &dash, a);
            assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
            let rows: Vec<usize> = (b.list_rows.iter().enumerate())
                .filter(|(_, r)| **r == Some(Stop::Thread(ThreadId(4))))
                .map(|(i, _)| i)
                .collect();
            let (first, last) = (rows[0], rows[rows.len() - 1]);
            let (top, height) = (dash.list_scroll(), usize::from(b.list.height));
            let shows = |row: usize| (top..top + height).contains(&row);
            assert!(
                shows(first),
                "unfolded {unfold}, height {h}: {first} {top} {b:?}"
            );
            if last - first < height {
                assert!(
                    shows(last),
                    "unfolded {unfold}, height {h}: {last} {top} {b:?}"
                );
            }
        }
    }
}

#[test]
fn a_list_scrolled_away_from_the_thread_shown_stays_put_when_the_thread_moves() {
    // The owner wheels the list to its top, T4 (shown) out of sight; T3 resolves and T4
    // moves up: the list stays where the owner put it, as it does for any refresh.
    let view: View = Ok(board_fixture());
    // A list two lines high: OPEN, T3's two, a blank, T4's three (its name wraps), a
    // blank, RESOLVED.
    let a = area(120, 8);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    dash.handle(&view, a, Input::Down);
    let b = board_of(&view, &dash, a);
    assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
    for _ in 0..4 {
        let (x, y) = (b.list.x, b.list.y);
        dash.handle(&view, a, Input::Wheel { x, y, down: false });
    }
    assert_eq!(dash.list_scroll(), 0);
    dash.refreshed(&view, a);
    let mut board = board_fixture();
    for t in &mut board.milestones[0].threads {
        if t.id == ThreadId(3) {
            t.resolved = Some(Resolution {
                by: "cat-lead".into(),
                at: NOW,
            });
        }
    }
    sync(&mut board);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    let b = board_of(&later, &dash, a);
    assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
    // T4 is now lines 1 to 3; kept in view regardless, the list would scroll to 1.
    assert_eq!(dash.list_scroll(), 0, "{b:?}");
}

#[test]
fn the_thread_shown_with_only_its_lower_lines_in_view_stays_in_view_when_it_moves() {
    // T4 is shown with its title just above the window and its last line in it; T3
    // resolves and T4 moves up. Partly in view counts as in view: the list follows it.
    let view: View = Ok(board_fixture());
    // A list three lines high: OPEN, T3's two (1, 2), a blank, T4's three (4 to 6: its name
    // wraps), a blank, RESOLVED (8).
    let a = area(120, 9);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    dash.handle(&view, a, Input::Down);
    let b = board_of(&view, &dash, a);
    assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
    let (x, y) = (b.list.x, b.list.y);
    dash.handle(&view, a, Input::Wheel { x, y, down: true });
    // Lines 6 to 8 show: T4's last line, a blank and RESOLVED, its title (4, 5) above.
    assert_eq!(dash.list_scroll(), 6);
    dash.refreshed(&view, a);
    let mut board = board_fixture();
    for t in &mut board.milestones[0].threads {
        if t.id == ThreadId(3) {
            t.resolved = Some(Resolution {
                by: "cat-lead".into(),
                at: NOW,
            });
        }
    }
    sync(&mut board);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    let b = board_of(&later, &dash, a);
    assert_eq!(b.current(), Some(Stop::Thread(ThreadId(4))), "{b:?}");
    // T4 is now lines 1 to 3: its title shows.
    let top = dash.list_scroll();
    assert!(
        (top..top + usize::from(b.list.height)).contains(&1),
        "{top} {b:?}"
    );
}

#[test]
fn an_item_leaving_a_scrolled_inbox_keeps_the_selection_in_view() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 10);
    let mut dash = inbox_dash(&view, a);
    for _ in 0..13 {
        dash.handle(&view, a, Input::Down);
    }
    let ids: Vec<i64> = (view.as_ref().unwrap().inbox.all())
        .map(|x| x.message.id)
        .collect();
    assert_eq!(
        dash.inbox_selected(view.as_ref().unwrap()),
        ids.last().copied()
    );
    assert!(dash.inbox_scroll() > 0);
    // The last six awareness items are read elsewhere: the list shrinks under the scroll.
    let mut board = volume_fixture();
    let mut read: Vec<i64> = vec![500];
    read.extend(5000..5110);
    read.extend(&ids[ids.len() - 6..]);
    with_alerts(&mut board, &read);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    let left: Vec<i64> = (later.as_ref().unwrap().inbox.all())
        .map(|x| x.message.id)
        .collect();
    assert_eq!(
        dash.inbox_selected(later.as_ref().unwrap()),
        left.last().copied()
    );
    let ib = layout(&later, &dash, a).inbox.unwrap();
    let row = (ib.rows.iter())
        .position(|r| *r == left.last().copied())
        .unwrap();
    let top = dash.inbox_scroll();
    assert!(
        (top..top + usize::from(ib.list.height)).contains(&row),
        "{row} {top} {ib:?}"
    );
    // The scroll kept is within the shorter list, not past its end.
    let max = ib.rows.len().saturating_sub(usize::from(ib.list.height));
    assert!(top <= max, "{top} > {max}");
}

#[test]
fn items_read_by_opening_their_thread_from_a_scrolled_inbox_leave_its_scroll_in_the_list() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 10);
    let mut dash = inbox_dash(&view, a);
    for _ in 0..13 {
        dash.handle(&view, a, Input::Down);
    }
    let ids: Vec<i64> = (view.as_ref().unwrap().inbox.all())
        .map(|x| x.message.id)
        .collect();
    assert_eq!(
        dash.inbox_selected(view.as_ref().unwrap()),
        ids.last().copied()
    );
    assert!(dash.inbox_scroll() > 0);
    // Enter opens the last item's thread, which reads its awareness items (spec 3a): they
    // leave the inbox while the board covers it.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::Board);
    let opened = dash.opened(&view);
    assert!(opened.len() > 1, "{opened:?}");
    let mut board = volume_fixture();
    let mut read: Vec<i64> = vec![500];
    read.extend(5000..5110);
    read.extend(&opened);
    with_alerts(&mut board, &read);
    let later: View = Ok(board);
    dash.refreshed(&later, a);
    dash.fit(&later, a);
    // Esc comes back to the shorter inbox: its scroll is within the list, with the item
    // now in the opened one's place selected and in view.
    dash.handle(&later, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::Inbox);
    dash.fit(&later, a);
    let ib = layout(&later, &dash, a).inbox.unwrap();
    let height = usize::from(ib.list.height);
    let max = ib.rows.len().saturating_sub(height);
    let top = dash.inbox_scroll();
    assert!(top <= max, "{top} > {max} {ib:?}");
    let left: Vec<i64> = (later.as_ref().unwrap().inbox.all())
        .map(|x| x.message.id)
        .collect();
    assert_eq!(
        dash.inbox_selected(later.as_ref().unwrap()),
        left.last().copied()
    );
    let row = (ib.rows.iter())
        .position(|r| *r == left.last().copied())
        .unwrap();
    assert!((top..top + height).contains(&row), "{row} {top}");
}

#[test]
fn up_and_down_switch_threads_in_list_order_and_stop_at_the_ends() {
    let view: View = Ok(board_fixture());
    let a = area(120, 40);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    // The stop selected: a thread, or the RESOLVED row ("R"), with the thread shown.
    let at = |dash: &Dash| {
        let b = board_of(&view, dash, a);
        let stop = match b.current() {
            Some(Stop::Resolved) => "R".to_string(),
            Some(Stop::Thread(t)) => t.0.to_string(),
            None => "-".into(),
        };
        (stop, dash.board_thread(&view).unwrap().0)
    };
    let mut seen = vec![at(&dash)];
    for _ in 0..3 {
        dash.handle(&view, a, Input::Down);
        seen.push(at(&dash));
    }
    // Folded, the RESOLVED row is the last stop; the conversation keeps T4 on it.
    let s = |x: &str, t: i64| (x.to_string(), t);
    assert_eq!(seen, [s("3", 3), s("4", 4), s("R", 4), s("R", 4)]);
    // → unfolds; down then reaches the resolved thread, and stops there.
    dash.handle(&view, a, Input::Right);
    assert!(board_of(&view, &dash, a).fold.unfolded);
    assert_eq!(at(&dash), s("R", 4));
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    assert_eq!(at(&dash), s("2", 2));
    // ← on a resolved thread folds the group, the selection on its row, not the hidden
    // thread.
    dash.handle(&view, a, Input::Left);
    let b = board_of(&view, &dash, a);
    assert!(!b.fold.unfolded);
    assert_eq!(at(&dash), s("R", 2));
    assert!(!b.list_rows.contains(&Some(Stop::Thread(ThreadId(2)))));
    // ← on the folded row does nothing more; → and ← unfold and fold it.
    dash.handle(&view, a, Input::Left);
    assert!(!board_of(&view, &dash, a).fold.unfolded);
    dash.handle(&view, a, Input::Right);
    assert!(board_of(&view, &dash, a).fold.unfolded);
    dash.handle(&view, a, Input::Left);
    assert!(!board_of(&view, &dash, a).fold.unfolded);
    // On the row the keys work the fold, as the keys line says: nothing writes to
    // the thread the conversation shows, Enter does nothing and a letter goes nowhere.
    // Back on the thread, typing writes again.
    dash.handle(&view, a, Input::Up);
    assert_eq!(at(&dash), s("4", 4));
    dash.handle(&view, a, Input::Down);
    assert_eq!(at(&dash), s("R", 4));
    let before = board_of(&view, &dash, a);
    assert!(!dash.writing(&view, a));
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    assert_eq!(board_of(&view, &dash, a), before);
    dash.handle(&view, a, Input::Char('x'));
    assert_eq!(dash.draft(ThreadId(4)), "");
    assert!(
        !screen(&view, &dash, a)
            .last()
            .unwrap()
            .contains("type to write")
    );
    dash.handle(&view, a, Input::Up);
    assert_eq!(at(&dash), s("4", 4));
    assert!(dash.writing(&view, a));
    dash.handle(&view, a, Input::Char('x'));
    assert_eq!(dash.draft(ThreadId(4)), "x");
    // Up/down never moved the tree's selection under the board.
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    let all = screen(&view, &dash, a);
    assert!(
        all.iter()
            .any(|r| r.contains("[open] Show each book's reviews")),
        "{all:#?}"
    );
    // A click on a list item shows it.
    let b = board_of(&view, &dash, a);
    let row = b
        .list_rows
        .iter()
        .position(|r| *r == Some(Stop::Thread(ThreadId(3))))
        .unwrap();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.list.x + 2,
            y: b.list.y + row as u16 + 1,
        },
    );
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    // A click on "◀ back" goes back.
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.top.x + 3,
            y: b.top.y,
        },
    );
    assert_eq!(dash.view(), ViewKind::Overview);
}

#[test]
fn a_message_selected_from_the_resolved_row_takes_the_keys_the_keys_line_names() {
    // Each open thread's newest message is long, so its "read more" shows.
    let mut board = board_fixture();
    let long = (0..120)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    for (n, t) in board.milestones[0].threads.iter_mut().enumerate() {
        if t.status() == ThreadStatus::Open {
            let mut messages = t.messages.clone();
            messages.push(Message {
                id: 390 + n as i64,
                author: "dev-2".into(),
                text: long.clone(),
                at: NOW - 60,
            });
            *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
        }
    }
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 40);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    let b = board_of(&view, &dash, a);
    assert!(b.fold.on_row && !b.fold.unfolded, "{b:?}");
    // With the row selected, a click on "read more" opens the message and selects it.
    let y = (screen(&view, &dash, a).iter())
        .position(|r| r.contains("▌ read more"))
        .unwrap() as u16;
    dash.handle(&view, a, Input::Click { x: b.conv.x + 4, y });
    let id = dash.board_message().expect("the message is selected");
    assert!(dash.open_messages().contains(&id));
    let keys = screen(&view, &dash, a)[39].clone();
    assert!(keys.contains("→ read more or less") && keys.contains("← threads"));
    assert!(!keys.contains("enter or →"), "{keys}");
    // → reads less, as the keys line says: the group stays folded.
    dash.handle(&view, a, Input::Right);
    let b = board_of(&view, &dash, a);
    assert!(!dash.open_messages().contains(&id));
    assert_eq!(dash.board_message(), Some(id));
    assert!(b.fold.on_row && !b.fold.unfolded, "{b:?}");
    // ← goes back to the threads, on the row, the group still folded; then → unfolds it.
    dash.handle(&view, a, Input::Left);
    let b = board_of(&view, &dash, a);
    assert_eq!(dash.board_message(), None);
    assert!(b.fold.on_row && !b.fold.unfolded, "{b:?}");
    dash.handle(&view, a, Input::Right);
    assert!(board_of(&view, &dash, a).fold.unfolded);
}

#[test]
fn the_conversation_opens_at_the_latest_and_scrolls_back_to_the_first_message() {
    let view: View = Ok(board_fixture());
    // Room under the "Needs you" strip and the thread's header for a few lines of chat,
    // with the blank rows under each.
    let a = area(100, 16);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let b = board_of(&view, &dash, a);
    let conv = |dash: &Dash| -> Vec<String> {
        let all = screen(&view, dash, a);
        (b.conv.y..b.conv.y + b.conv.height)
            .map(|y| {
                all[usize::from(y)]
                    .chars()
                    .skip(usize::from(b.conv.x))
                    .collect()
            })
            .collect()
    };
    assert!(b.conv_lines > usize::from(b.conv.height) + 10, "{b:?}");
    // The latest message's last line is at the bottom, wherever the bubble's width wraps it.
    let rows = conv(&dash);
    let last = rows.last().unwrap();
    assert!(
        last.contains(" ▌ ") && last.trim_end().ends_with("not 12"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains(" ▌ copy, testing: phone and tablet pass")),
        "{rows:#?}"
    );
    // PgUp all the way: the opening message at the top, and no further.
    for _ in 0..40 {
        dash.handle(&view, a, Input::PageUp);
    }
    let rows = conv(&dash);
    // The conversation's first line is its first hour separator; the opening message's
    // name follows a blank line.
    assert!(rows[0].contains("─ within the hour ─"), "{rows:#?}");
    assert_eq!(
        b.starts.first().map(|(id, line)| (*id, *line)).unwrap().1,
        2
    );
    assert_eq!(b.conv_top(dash.board_anchor()), 0);
    // PgDn back to the latest, and no further.
    for _ in 0..40 {
        dash.handle(&view, a, Input::PageDown);
    }
    assert_eq!(dash.board_anchor(), None);
    // The wheel over the conversation scrolls it; over the list, only the list.
    let (cx, cy) = (b.conv.x + 2, b.conv.y + 1);
    dash.handle(
        &view,
        a,
        Input::Wheel {
            x: cx,
            y: cy,
            down: false,
        },
    );
    assert_eq!(b.conv_top(dash.board_anchor()), b.max_top() - 3);
    let (lx, ly) = (b.list.x + 2, b.list.y + 1);
    dash.handle(
        &view,
        a,
        Input::Wheel {
            x: lx,
            y: ly,
            down: true,
        },
    );
    assert_eq!(b.conv_top(dash.board_anchor()), b.max_top() - 3);
    assert_eq!(
        dash.list_scroll(),
        b.list_rows.len().saturating_sub(usize::from(b.list.height))
    );
    dash.handle(
        &view,
        a,
        Input::Wheel {
            x: lx,
            y: ly,
            down: true,
        },
    );
    assert_eq!(
        dash.list_scroll(),
        b.list_rows.len().saturating_sub(usize::from(b.list.height))
    );
    // Switching threads opens the new one at its latest.
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.board_anchor(), None);
}

fn full_board_snapshot(w: u16, h: u16) {
    let view: View = Ok(board_fixture());
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, area(w, h), Input::Char('b'));
    let b = board_of(&view, &dash, area(w, h));
    assert_eq!(b.beside, w == 120);
    let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot(&format!("full_board_{w}x{h}"), &buf);
}

#[test]
fn full_board_snapshot_wide_with_the_list_beside() {
    full_board_snapshot(120, 30);
}

#[test]
fn full_board_snapshot_narrow_with_the_list_above() {
    full_board_snapshot(50, 30);
}

/// In a narrow pane (the list above the conversation) the list is one line a thread
/// and takes its content's height up to half the pane, so every open thread and the
/// RESOLVED row show at once; the thread's header leaves out its "where it stands" line.
#[test]
fn in_a_narrow_pane_the_list_is_a_line_a_thread_with_resolved_in_view() {
    // A busy board: two open threads and 34 resolved ones.
    let view: View = Ok(volume_fixture());
    let a = area(60, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    dash.fit(&view, a);
    let b = board_of(&view, &dash, a);
    assert!(!b.beside);
    // OPEN, a line each for the two open threads, a blank, RESOLVED: all of it shows.
    assert_eq!(b.list_rows.len(), 1 + 2 + 1 + 1, "{b:?}");
    assert_eq!(usize::from(b.list.height), b.list_rows.len(), "{b:?}");
    let all = screen(&view, &dash, a);
    let list: Vec<&String> = (b.list.y..b.list.y + b.list.height)
        .map(|y| &all[usize::from(y)])
        .collect();
    assert!(list[4].starts_with(" RESOLVED 34 ▸"), "{list:#?}");
    assert!(list[3].trim().is_empty(), "{list:#?}");
    for row in &list[1..3] {
        assert!(
            row.starts_with(" ○ ") || row.starts_with(" ● "),
            "{list:#?}"
        );
    }
    // The header: no "where it stands" line here; beside the list (wide), it has one.
    let head: Vec<&String> = (b.head.y..b.head.y + b.head.height)
        .map(|y| &all[usize::from(y)])
        .collect();
    assert!(
        !head.iter().any(|r| r.contains("where it stands")),
        "{head:#?}"
    );
    let wide = area(120, 30);
    let mut d = select(Item::Milestone(MilestoneId(1)));
    d.handle(&view, wide, Input::Char('b'));
    let w = board_of(&view, &d, wide);
    assert!(w.beside);
    let all = screen(&view, &d, wide);
    assert!(
        (w.head.y..w.head.y + w.head.height)
            .any(|y| all[usize::from(y)].contains("where it stands")),
        "{all:#?}"
    );
    // Taller than half the pane, the list stops at half and scrolls.
    let mut d = dash.clone();
    d.handle(&view, a, Input::Down);
    d.handle(&view, a, Input::Down);
    d.handle(&view, a, Input::Right);
    d.fit(&view, a);
    let b = board_of(&view, &d, a);
    assert_eq!(b.list_rows.len(), 1 + 2 + 1 + 1 + 34, "{b:?}");
    let rest = a.height - 2 - b.top.height - b.strip.height;
    assert_eq!(b.list.height, (rest / 2).max(3), "{b:?}");
}

/// A task's threads that would read alike (both named by the task's title) each add
/// their own title, dim, in the full board's list, its RESOLVED lines and the overview's.
/// Short of room the name is cut first and the title keeps all but 12 columns.
#[test]
fn a_tasks_threads_carry_their_own_title_so_they_never_read_alike() {
    let delivery = |title: &str| {
        let mut board = board_fixture();
        board.milestones[0].threads.push(Thread::whole(
            ThreadId(9),
            title.into(),
            Some(TaskId(13)),
            Some(Resolution {
                by: "cat-lead".into(),
                at: NOW - 90 * 60,
            }),
            vec![Message {
                id: 900,
                author: "dev-2".into(),
                text: "the post went through on the second try".into(),
                at: NOW - 90 * 60,
            }],
        ));
        sync(&mut board);
        let view: View = Ok(board);
        view
    };
    // 40 columns: 31 for the name and title, "New book shelf · Second try" fits whole.
    let view = delivery("Second try");
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    // Unfold RESOLVED: down to its row, then right.
    while board_of(&view, &dash, a).current() != Some(Stop::Resolved) {
        dash.handle(&view, a, Input::Down);
    }
    dash.handle(&view, a, Input::Right);
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    let list = |r: &String| r.chars().take(40).collect::<String>();
    // Both of #13's threads: T3 (open) and T9 (resolved), each with its own title, dim.
    let y = find_row_containing(&all, "✓ New book shelf · Second try");
    let x = all[y]
        .find(" · Second")
        .map(|b| all[y][..b].chars().count())
        .unwrap();
    assert_eq!(
        buf[(x as u16 + 3, y as u16)].fg,
        dim_colour(Depth::TrueColor)
    );
    assert_ne!(buf[(5, y as u16)].fg, dim_colour(Depth::TrueColor));
    find_row_containing(&all, "○ New book shelf · Task #13 team");
    // #14's only thread and T2 (no task) add nothing.
    let t4 = find_row_containing(&all, "Show each book's reviews");
    assert!(!list(&all[t4]).contains(" · "), "{}", all[t4]);
    let t2 = find_row_containing(&all, "✓ Ribbon hides the cover");
    assert!(!list(&all[t2]).contains(" · "), "{}", all[t2]);
    // The overview's RESOLVED lines say it too.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, area(120, 70));
    let y = all
        .iter()
        .position(|r| r.contains("│ RESOLVED 2 ▸"))
        .unwrap();
    let x = all[y].split("RESOLVED").next().unwrap().chars().count() as u16;
    dash.handle(&view, area(120, 70), Input::Click { x, y: y as u16 });
    let all = screen(&view, &dash, area(120, 70)).join("\n");
    assert!(all.contains("✓ New book shelf · Second try"), "{all}");
    // A list line 40 wide with a long own title: the name is cut first, to 12 columns.
    let view = delivery("Delivery failed to Catalogue Lead on the second release");
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    while board_of(&view, &dash, a).current() != Some(Stop::Resolved) {
        dash.handle(&view, a, Input::Down);
    }
    dash.handle(&view, a, Input::Right);
    let all = screen(&view, &dash, a);
    let y = find_row_containing(&all, "✓ New book sh");
    let line = list(&all[y]);
    // " ✓ " then the name in 12 columns ("New book sh…"), then the title in the other 19,
    // cut, then the age.
    assert!(
        line.starts_with(" ✓ New book sh… · Delivery failed…"),
        "{line:?}"
    );
}

/// Beside the conversation a long thread name wraps to a second line, its own title
/// with it, dim; past two lines it is cut with "…". In a narrow pane it stays
/// one line, cut.
#[test]
fn a_long_thread_name_wraps_to_two_lines_beside_the_conversation() {
    let list = |r: &String| r.chars().take(40).collect::<String>();
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    // #14's title in a 40-column list: 31 columns for the name, so two lines; the age on
    // the first, at the right.
    let y = find_row_containing(&all, "● Show each book's reviews ");
    let first = list(&all[y]);
    assert!(
        first.starts_with(" ● Show each book's reviews "),
        "{first:?}"
    );
    assert_ne!(first.chars().nth(38), Some(' '), "{first:?}");
    assert_eq!(
        list(&all[y + 1]).trim_end(),
        "   its cover, newest first, with …"
    );
    // All three of its lines are the selected item's.
    for k in 0..3 {
        assert_eq!(
            buf[(5, y as u16 + k)].bg,
            selected_bg(Depth::TrueColor),
            "{k}"
        );
    }
    // In a narrow pane: one line, cut.
    let n = area(50, 30);
    let mut d = select(Item::Task(TaskId(14)));
    d.handle(&view, n, Input::Char('b'));
    let all = screen(&view, &d, n);
    let y = find_row_containing(&all, "● Show each book's reviews under");
    assert!(all[y].contains('…'), "{}", all[y]);
    assert!(!all[y + 1].contains("the rest"), "{all:#?}");
    // With its own title, which wraps with it, dim on the second line.
    let mut board = board_fixture();
    board.milestones[0].threads[1].title = "Shelf grid and the ribbon on narrow phones".into();
    board.milestones[0].threads.push(Thread::whole(
        ThreadId(9),
        "Delivery failed".into(),
        Some(TaskId(13)),
        Some(Resolution {
            by: "cat-lead".into(),
            at: NOW - 90 * 60,
        }),
        vec![Message {
            id: 900,
            author: "dev-2".into(),
            text: "the post went through on the second try".into(),
            at: NOW - 90 * 60,
        }],
    ));
    sync(&mut board);
    let view: View = Ok(board);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, a, Input::Char('b'));
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    let y = find_row_containing(&all, "○ New book shelf · Shelf grid");
    assert_eq!(
        list(&all[y + 1]).trim_end(),
        "   the ribbon on narrow phones"
    );
    assert_eq!(buf[(3, y as u16 + 1)].fg, dim_colour(Depth::TrueColor));
    assert_ne!(buf[(3, y as u16)].fg, dim_colour(Depth::TrueColor));
    // Two open threads on #14: its long title and each own title need more than
    // two lines, so the name is cut first and each own title keeps its room on the second
    // line: the two never read alike.
    let mut board = board_fixture();
    board.milestones[0].threads.push(Thread::whole(
        ThreadId(9),
        "Delivery failed".into(),
        Some(TaskId(14)),
        None,
        vec![Message {
            id: 900,
            author: "dev-1".into(),
            text: "the reminder post went through on the second try".into(),
            at: NOW - 30 * 60,
        }],
    ));
    sync(&mut board);
    let view: View = Ok(board);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    let seconds: Vec<String> = (all.iter().enumerate())
        .filter(|(_, r)| list(r).contains(" Show each book's reviews "))
        .map(|(y, _)| list(&all[y + 1]).trim_end().to_string())
        .collect();
    assert_eq!(
        seconds,
        [
            "   its cover,… · Review stars",
            "   its cover,… · Delivery failed"
        ],
        "{all:#?}"
    );
}

/// An own title whose first word is too wide for the room it keeps on a thread's second
/// list line is cut by character after its " · ", as a lone piece is, never down to "·…",
/// so two threads on one task still read apart (#50).
#[test]
fn an_own_title_too_long_for_its_room_keeps_its_start_after_the_dot() {
    let list = |r: &String| r.chars().take(40).collect::<String>();
    let mut board = board_fixture();
    board.milestones[0].threads.push(Thread::whole(
        ThreadId(9),
        "Shelfgridribbonalignmentcheck notes".into(),
        Some(TaskId(14)),
        None,
        vec![Message {
            id: 900,
            author: "dev-1".into(),
            text: "the grid lines up on the second try".into(),
            at: NOW - 30 * 60,
        }],
    ));
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    let seconds: Vec<String> = (all.iter().enumerate())
        .filter(|(_, r)| list(r).contains(" Show each book's reviews "))
        .map(|(y, _)| list(&all[y + 1]).trim_end().to_string())
        .collect();
    assert_eq!(
        seconds,
        [
            "   its cover,… · Review stars",
            "   its cover,… · Shelfgridribbona…"
        ],
        "{all:#?}"
    );
}

#[test]
fn the_selected_list_item_is_on_the_selected_background_and_marks_are_coloured() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    let y = all
        .iter()
        .position(|r| r.contains("○ New book shelf"))
        .unwrap() as u16;
    // Both lines of the selected item, its name and where it stands, and not the blank line
    // under it; what it is on is in its header, not the list.
    for k in 0..2 {
        assert_eq!(buf[(5, y + k)].bg, selected_bg(Depth::TrueColor));
    }
    assert_ne!(buf[(5, y + 2)].bg, selected_bg(Depth::TrueColor));
    let list = |r: &String| r.chars().take(40).collect::<String>();
    assert_eq!(list(&all[usize::from(y) + 2]).trim(), "", "{all:#?}");
    assert!(!all.iter().any(|r| list(r).contains("· T3")), "{all:#?}");
    // T4 waits on the owner: a red mark, in the list (not the strip above it).
    let y4 = all
        .iter()
        .position(|r| r.contains("● Show each book's reviews"))
        .unwrap() as u16;
    assert_eq!(buf[(1, y4)].fg, bad(Depth::TrueColor));
    assert_ne!(buf[(5, y4)].bg, selected_bg(Depth::TrueColor));
    // Its name wraps to two lines, then where it stands.
    assert!(
        all[usize::from(y4) + 2].contains("cat-lead: question: the covers"),
        "{all:#?}"
    );
    assert!(all.iter().any(|r| r.starts_with(" OPEN 2 ")), "{all:#?}");
    // RESOLVED is one row, folded: its thread is not listed.
    let yr = all
        .iter()
        .position(|r| r.starts_with(" RESOLVED 1 ▸ "))
        .unwrap_or_else(|| panic!("{all:#?}"));
    assert!(
        !all.iter().any(|r| r.contains("Ribbon hides the cover")),
        "{all:#?}"
    );
    // A click on it unfolds it: the resolved thread is one line, ✓ in green, name, age.
    let mut d = dash.clone();
    d.handle(&view, a, Input::Click { x: 3, y: yr as u16 });
    let buf = render_dash(&view, &d, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    assert!(all[yr].starts_with(" RESOLVED 1 ▾ "), "{all:#?}");
    let y2 = yr + 1;
    assert!(all[y2].starts_with(" ✓ Ribbon hides the cover"), "{all:#?}");
    assert!(all[y2].contains("2h │"), "{}", all[y2]);
    assert!(!all[y2 + 1].contains("T2"), "{all:#?}");
    assert_eq!(buf[(1, y2 as u16)].fg, ok(Depth::TrueColor));
    // The selection is on the row, and the conversation still shows T3.
    assert_eq!(buf[(5, yr as u16)].bg, selected_bg(Depth::TrueColor));
    assert_eq!(d.board_thread(&view), Some(ThreadId(3)));
    // A click on the thread shows it; a click on the row folds it again.
    d.handle(&view, a, Input::Click { x: 3, y: y2 as u16 });
    assert_eq!(d.board_thread(&view), Some(ThreadId(2)));
    d.handle(&view, a, Input::Click { x: 3, y: yr as u16 });
    assert!(!board_of(&view, &d, a).fold.unfolded);
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    // Under the top line, the "Needs you" strip; then the thread's header in words.
    assert!(all[1].contains("⚠ Needs you · 1"), "{}", all[1]);
    let head = all
        .iter()
        .position(|r| r.contains("[open] New book shelf"))
        .unwrap();
    assert!(
        all[head].contains("  T3 · 7 messages ·   8m"),
        "{}",
        all[head]
    );
    assert!(
        all[head + 1].contains("in progress · unassigned · started by cat-lead"),
        "{}",
        all[head + 1]
    );
    assert!(all[head + 2].contains("where it stands qa: copy, testing"));
}

#[test]
fn every_full_board_row_is_exactly_the_width_at_any_size() {
    let view: View = Ok(board_fixture());
    for w in [1, 10, 30, 50, 68, 69, 70, 100, 133] {
        for h in [1, 3, 5, 8, 30] {
            let mut dash = select(Item::Task(TaskId(13)));
            dash.handle(&view, area(w, h), Input::Char('b'));
            for _ in 0..3 {
                dash.handle(&view, area(w, h), Input::PageUp);
            }
            let buf = render_dash(&view, &dash, w, h, Depth::Ansi256);
            assert_exact_width(&buf);
        }
    }
    // A lead with no threads yet says so.
    let mut empty = board_fixture();
    empty.milestones[0].threads.clear();
    sync(&mut empty);
    let view: View = Ok(empty);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, area(120, 20), Input::Char('b'));
    let all = screen(&view, &dash, area(120, 20)).join("\n");
    assert!(
        all.contains("no threads yet") && all.contains("OPEN 0"),
        "{all}"
    );
}

/// The board fixture with one more message on T3, posted now with id `id` (ids are never
/// reused on a board).
fn with_new_message(id: i64, text: &str) -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    messages.push(Message {
        id,
        author: "dev-2".into(),
        text: text.into(),
        at: NOW,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

#[test]
fn scrolled_back_the_conversation_stays_on_its_message_and_at_the_latest_it_follows() {
    let view: View = Ok(board_fixture());
    // A few lines of chat under the strip and the header and their blank rows.
    let a = area(100, 14);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, a, Input::Char('b'));
    for _ in 0..2 {
        dash.handle(&view, a, Input::PageUp);
    }
    let first = |v: &View, d: &Dash| {
        let b = board_of(v, d, a);
        screen(v, d, a)[usize::from(b.conv.y)].clone()
    };
    let before = first(&view, &dash);
    assert!(dash.board_anchor().is_some());
    // A long new message arrives: the line being read stays at the top.
    let newer = with_new_message(398, &"more news ".repeat(30));
    dash.refreshed(&newer, a);
    assert_eq!(first(&newer, &dash), before);
    // At the latest, it follows: the new message's last line is at the bottom.
    for _ in 0..20 {
        dash.handle(&newer, a, Input::PageDown);
    }
    assert_eq!(dash.board_anchor(), None);
    let latest = with_new_message(399, "the very latest");
    let b = board_of(&latest, &dash, a);
    let all = screen(&latest, &dash, a);
    let bottom = &all[usize::from(b.conv.y + b.conv.height - 1)];
    assert!(bottom.contains("the very latest"), "{all:#?}");
}

#[test]
fn a_thread_gone_from_the_board_falls_back_to_the_first_at_its_latest() {
    let view: View = Ok(board_fixture());
    let a = area(100, 12);
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Char('b'));
    dash.handle(&view, a, Input::PageUp);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    assert!(dash.board_anchor().is_some());
    let mut board = board_fixture();
    board.milestones[0].threads.retain(|t| t.id != ThreadId(4));
    sync(&mut board);
    let gone: View = Ok(board);
    dash.refreshed(&gone, a);
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread(&gone), Some(ThreadId(3)));
    assert_eq!(dash.board_thread_wanted(), Some(ThreadId(3)));
    assert_eq!(dash.board_anchor(), None);
}

#[test]
fn opening_the_board_in_a_narrow_pane_scrolls_the_list_to_the_thread() {
    // Six newer open threads above #14's, so its one line starts below the list.
    let mut board = board_fixture();
    for i in 0..6 {
        board.milestones[0].threads.push(Thread::whole(
            ThreadId(20 + i),
            format!("Newer thread {i}"),
            None,
            None,
            vec![Message {
                id: 2000 + i,
                author: "dev-2".into(),
                text: "starting".into(),
                at: NOW - 60,
            }],
        ));
    }
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(50, 14);
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Char('b'));
    let b = board_of(&view, &dash, a);
    assert!(!b.beside);
    assert!(b.list_rows.len() > usize::from(b.list.height), "{b:?}");
    let all = screen(&view, &dash, a);
    let list: Vec<&String> = (b.list.y..b.list.y + b.list.height)
        .map(|y| &all[usize::from(y)])
        .collect();
    assert!(
        list.iter().any(|r| r.contains("Show each book's reviews")),
        "{list:#?}"
    );
    assert!(dash.list_scroll() > 0);
}

#[test]
fn the_reader_fills_in_the_whole_history_and_reads_again_only_on_a_new_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    let m = store.add_milestone("m", "g", Some(r)).unwrap();
    store
        .add_lead(RepoOf::Milestone(m), "Lead", "lead", None)
        .unwrap();
    let (t, _) = store
        .open_thread(&BoardOf::Milestone(m), "lead", None, "talk", "one")
        .unwrap();
    for i in 2..=14 {
        store.post(t, "dev-1", &format!("message {i}")).unwrap();
    }
    // The full board reads the repo's board, which the reader fills in.
    let loaded = |v: &View| v.as_ref().unwrap().repos[0].threads[0].messages.len();
    let mut reader = Reader::new(&path);
    assert_eq!(loaded(&reader.read(None)), 12);
    assert_eq!(reader.reads(), 0);
    let view = reader.read(Some(t));
    assert_eq!((loaded(&view), reader.reads()), (14, 1));
    // The same count: the kept history, not a second read.
    assert_eq!(loaded(&reader.read(Some(t))), 14);
    assert_eq!(reader.reads(), 1);
    store.post(t, "qa", "nine").unwrap();
    assert_eq!(loaded(&reader.read(Some(t))), 15);
    assert_eq!(reader.reads(), 2);
    let mut dash = select(Item::Milestone(m));
    dash.handle(&view, area(100, 60), Input::Char('b'));
    let all = screen(&view, &dash, area(100, 60)).join("\n");
    assert!(
        all.contains("▌ one") && all.contains("▌ message 14"),
        "{all}"
    );
    assert!(!all.contains("not loaded"), "{all}");
    // A thread not on the board leaves the view as refreshed.
    assert!(reader.read(Some(ThreadId(99))).is_ok());
    assert_eq!(reader.reads(), 2);
    // A board that cannot be read shows why, as a refresh does.
    std::fs::remove_file(&path).unwrap();
    assert!(reader.read(Some(t)).is_err());
}

#[test]
fn a_thread_only_partly_loaded_says_how_many_earlier_messages_are_not() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    t.messages.drain(..4);
    sync(&mut board);
    let view: View = Ok(board);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, area(100, 30), Input::Char('b'));
    let all = screen(&view, &dash, area(100, 30)).join("\n");
    assert!(all.contains("… 4 earlier not loaded yet"), "{all}");
}

/// A progress post with `text`, as the store reads one.
fn progress(text: impl Into<String>) -> Option<Message> {
    Some(Message {
        id: 1,
        author: "dev-2".into(),
        text: text.into(),
        at: NOW,
    })
}

/// The board fixture with dev-2 working on #13 with a progress post, and
/// dev-1 on #14, whose task is flagged blocked (its latest post is under the flag).
fn blocked_fixture() -> Board {
    let mut board = board_fixture();
    let m = &mut board.milestones[0];
    m.tasks[1].details.assignee = Some("dev-2".into());
    m.tasks[1].details.doing = progress("testing the shelf grid\nthen the ribbon");
    m.tasks[2].details.assignee = Some("dev-1".into());
    m.tasks[2].details.doing =
        progress("Starting the reading reminders with the sample catalogue.");
    m.tasks[2].details.blocked = Some("printer has not sent the covers".into());
    board
}

#[test]
fn blocked_snapshots_of_the_tree_cards_and_task_detail() {
    let view: View = Ok(blocked_fixture());
    for (w, h) in [(120, 40), (72, 40)] {
        let buf = render_dash(
            &view,
            &select(Item::Milestone(MilestoneId(1))),
            w,
            h,
            Depth::TrueColor,
        );
        assert_exact_width(&buf);
        assert_snapshot(&format!("blocked_overview_{w}x{h}"), &buf);
    }
    let a = area(120, 30);
    let buf = render_dash(
        &view,
        &opened(&view, Item::Task(TaskId(14)), a),
        120,
        30,
        Depth::TrueColor,
    );
    assert_exact_width(&buf);
    assert_snapshot("blocked_detail_task_120x30", &buf);
}

#[test]
fn the_tree_marks_blocked_tasks_and_counts_them_on_the_repo_row_from_the_flag_only() {
    let mut board = blocked_fixture();
    // A post that says "blocked:" without the flag marks nothing.
    board.milestones[0].tasks[1].details.doing = progress("blocked: maybe");
    // No badges beside the count here: they have their own test.
    board.inbox = Default::default();
    let lines = tree_lines(&board, 40, Depth::TrueColor);
    let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let row = |prefix: &str| find_row(&text, prefix);
    let repo = row(" ▾ catalogue");
    assert_eq!(
        text[repo],
        format!("{:<29}{}", " ▾ catalogue", " 1 blocked ")
    );
    let blocked = &lines[repo]
        .spans
        .iter()
        .find(|s| s.content == " 1 blocked")
        .unwrap();
    assert_eq!(blocked.style.fg, Some(warn(Depth::TrueColor)));
    let t14 = row("     ○ Show each");
    assert_eq!(text[t14], "     ○ Show each book's     ! no ticket ");
    assert!(
        text[t14 + 1].starts_with("       reviews under"),
        "{}",
        text[t14 + 1]
    );
    let bang = lines[t14].spans.iter().find(|s| s.content == " !").unwrap();
    assert_eq!(bang.style.fg, Some(warn(Depth::TrueColor)));
    assert!(
        !text[row("     ● New book")].contains('!'),
        "{}",
        text[row("     ● New book")]
    );
    // No blocked task, no count: checkout's row is the name alone.
    assert!(!text[row(" ▾ checkout")].contains("blocked"));
    // Two flags count two, and a repo without a lead still shows the count.
    board.milestones[0].tasks[0].details.blocked = Some("r".into());
    board.milestones[0].lead = None;
    sync(&mut board);
    board.inbox = Default::default();
    let text: Vec<String> = (tree_lines(&board, 60, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    assert!(text[0].starts_with(" ▾ catalogue"), "{}", text[0]);
    assert!(text[0].ends_with(" 2 blocked "), "{}", text[0]);
    assert!(text[1].starts_with("   no lead yet"), "{}", text[1]);
    // A blocked standalone task of the repo counts on its row too.
    let mut loose = repo_standalone_fixture();
    loose.standalone[1].details.blocked = Some("r".into());
    loose.inbox = Default::default();
    let text: Vec<String> = (tree_lines(&loose, 60, Depth::Ansi256).iter())
        .map(|l| l.to_string())
        .collect();
    assert!(text[0].ends_with(" 1 blocked "), "{}", text[0]);
    // Every width stays exact; too narrow for the count beside the name, the count goes.
    for w in 0..=80 {
        for line in tree_lines(&board, w, Depth::TrueColor) {
            assert_eq!(line.width(), w, "w={w} {line:?}");
        }
    }
    let narrow: Vec<String> = (tree_lines(&blocked_fixture(), 12, Depth::TrueColor).iter())
        .map(|l| l.to_string())
        .collect();
    assert_eq!(narrow[0], " ▾ catalogue");
}

#[test]
fn a_card_is_one_line_with_a_bang_when_blocked_and_leaves_the_doing_line_to_the_thread() {
    let board = blocked_fixture();
    let m = &board.milestones[0];
    let o = overview(m, None, 120, Depth::TrueColor);
    let text: Vec<String> = o.lines.iter().map(|l| l.to_string()).collect();
    let all = text.join("\n");
    let card = |id: i64| *o.cards.iter().find(|c| c.task == TaskId(id)).unwrap();
    // Every card is one line; the doing line and the blocked reason are not on it (the
    // thread's header says where it stands).
    for id in [12, 13, 14] {
        assert_eq!(card(id).height, 1, "#{id}");
    }
    assert!(!all.contains("testing the shelf grid"), "{all}");
    assert!(!all.contains("printer has not sent"), "{all}");
    // #14 is blocked: a bold amber "!" at the end of its card; #13 is not.
    let line = &o.lines[card(14).line];
    let bang = line.spans.iter().find(|s| s.content == "!").unwrap();
    assert_eq!(bang.style.fg, Some(warn(Depth::TrueColor)));
    assert!(bang.style.add_modifier.contains(Modifier::BOLD));
    // Cards in other columns share the line: look only at #13's own columns.
    let c13 = card(13);
    let own: String = text[c13.line].chars().skip(c13.x).take(c13.width).collect();
    assert!(own.contains("#113") && !own.contains('!'), "{own:?}");
    // Every width stays exact, and the layout is what is drawn.
    for w in 0..=130 {
        let o = overview(m, Some(TaskId(14)), w, Depth::Ansi256);
        for line in &o.lines {
            assert_eq!(line.width(), w, "w={w} {line:?}");
        }
        assert_eq!(
            overview_layout(m, Some(TaskId(14)), w).cards,
            o.cards,
            "w={w}"
        );
    }
}

#[test]
fn the_task_detail_shows_the_doing_post_in_full_and_the_blocked_flag_on_its_own_row() {
    let mut board = blocked_fixture();
    board.milestones[0].tasks[1].details.doing = progress(format!("{}end", "word ".repeat(30)));
    let m = &board.milestones[0];
    let text = |id: i64, w: usize| -> Vec<String> {
        let t = m.tasks.iter().find(|t| t.id == TaskId(id)).unwrap();
        let d = task_detail(&board, Some(m), t, w, NOW, Depth::TrueColor);
        for line in &d.lines {
            assert_eq!(line.width(), w);
        }
        d.lines.iter().map(|l| l.to_string()).collect()
    };
    // The blocked flag in its own row, right above blocked by (the dependencies); the
    // doing line keeps the latest post.
    let t14 = text(14, 80);
    let at = find_row(&t14, " doing ");
    assert_eq!(
        t14[at..at + 3],
        [
            format!(
                "{:<80}",
                " doing      Starting the reading reminders with the sample catalogue."
            ),
            format!("{:<80}", " blocked    printer has not sent the covers"),
            format!("{:<80}", " blocked by nothing"),
        ],
        "{t14:#?}"
    );
    let t = &m.tasks[2];
    let d = task_detail(&board, Some(m), t, 80, NOW, Depth::TrueColor);
    let reason = (d.lines[at + 1].spans.iter())
        .find(|s| s.content.contains("printer"))
        .unwrap();
    assert_eq!(reason.style.fg, Some(warn(Depth::TrueColor)));
    // An unblocked task has no blocked row, only blocked by.
    assert!(!text(13, 80).iter().any(|l| l.starts_with(" blocked  ")));
    // The whole post, wrapped under the value's column.
    let t13 = text(13, 60);
    let at = find_row(&t13, " doing      word");
    assert!(t13[at + 1].starts_with("            word"), "{t13:#?}");
    assert!(
        t13[at + 3].starts_with("            word word word end "),
        "{t13:#?}"
    );
    // Nothing posted yet says so.
    assert!(
        text(12, 80)
            .iter()
            .any(|l| l.starts_with(" doing      nothing posted yet"))
    );
}

#[test]
fn a_doing_posts_indent_as_wide_as_its_value_hangs_on_a_blank_row() {
    // The doing post wraps as its bubble's text does: an indent as wide as the value, or
    // one the first word does not fit after, hangs on the doing row, drawn blank, and the
    // words start the next row at the value's column, as the signed design's (UI v0.1.14)
    // bubble draws them. With them goes a run of ideographic spaces starting the first
    // word. A narrower indent stays before the words on the doing row.
    let mut board = blocked_fixture();
    // The detail `w` wide (its value 12 columns narrower) with `text` as #13's doing post:
    // its doing row and the one after it.
    let mut doing = |text: &str, w: usize| -> Vec<String> {
        board.milestones[0].tasks[1].details.doing = progress(text);
        let m = &board.milestones[0];
        let t = m.tasks.iter().find(|t| t.id == TaskId(13)).unwrap();
        let d = task_detail(&board, Some(m), t, w, NOW, Depth::TrueColor);
        let rows: Vec<String> = d.lines.iter().map(|l| l.to_string()).collect();
        let at = find_row(&rows, " doing ");
        rows[at..at + 2].to_vec()
    };
    let hung = |words: &str, w: usize| {
        [
            format!("{:<w$}", " doing"),
            format!("{:<w$}", format!("{:12}{words}", "")),
        ]
    };
    let sp = |n: usize| " ".repeat(n);
    for n in [92, 93] {
        let text = format!("{}the press is jammed", sp(n));
        assert_eq!(doing(&text, 105), hung("the press is jammed", 105), "{n}");
    }
    assert_eq!(doing(&format!("{}a b c", sp(60)), 72), hung("a b c", 72));
    for w in [105, 72] {
        let text = format!("{}\u{3000}ab cd", sp(93));
        assert_eq!(doing(&text, w), hung("ab cd", w), "{w}");
    }
    let narrow = doing("  the press is jammed", 60);
    assert_eq!(
        narrow[0],
        format!("{:<60}", " doing        the press is jammed")
    );
}

#[test]
fn a_blocked_standalone_task_shows_its_bang_before_its_ticket() {
    let mut board = standalone_fixture();
    board.standalone[0].details.blocked = Some("no lead yet".into());
    let lines = tree_lines(&board, 50, Depth::TrueColor);
    let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let row = find_row(&text, "     ● Sort the");
    assert!(
        text[row].ends_with(" ! #64 · dev-3 · 1/3 "),
        "{:?}",
        text[row]
    );
    assert!(!text[find_row(&text, "     ○ Add a")].contains('!'));
    for w in 0..=60 {
        for line in tree_lines(&board, w, Depth::TrueColor) {
            assert_eq!(line.width(), w, "w={w}");
        }
    }
}

/// The full board open on T3 of the board fixture in `a`.
fn on_t3(view: &View, a: Rect) -> Dash {
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(view), Some(ThreadId(3)));
    dash
}

fn typed(dash: &mut Dash, view: &View, a: Rect, text: &str) {
    for c in text.chars() {
        assert_eq!(dash.handle(view, a, Input::Char(c)), None, "{c:?}");
    }
}

#[test]
fn typing_on_an_open_thread_writes_c_and_q_included_and_esc_goes_back_keeping_the_draft() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    // The keys write from the moment the thread shows, with no c first.
    assert!(dash.writing(&view, a) && dash.typing(&view, a));
    // Every single-key binding is typed instead: q, b, h and c.
    typed(&mut dash, &view, a, "qbhc ok");
    dash.handle(&view, a, Input::Backspace);
    assert_eq!(dash.draft(ThreadId(3)), "qbhc o");
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    let all = screen(&view, &dash, a);
    assert!(all[27].contains("│ manager ▌qbhc o "), "{}", all[27]);
    // Esc goes back, the draft kept for the thread; then q quits.
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    assert_eq!(dash.view(), ViewKind::Overview);
    assert!(!dash.writing(&view, a));
    assert_eq!(dash.draft(ThreadId(3)), "qbhc o");
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
    // Back on the board the draft is there, and each thread keeps its own.
    dash.handle(&view, a, Input::Char('b'));
    assert!(screen(&view, &dash, a)[27].contains("▌qbhc o"));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    assert!(dash.writing(&view, a));
    assert!(
        screen(&view, &dash, a)[27].contains("▌Type to write to the team, e.g. copy, approved")
    );
    dash.handle(&view, a, Input::Up);
    typed(&mut dash, &view, a, "k");
    assert_eq!(dash.draft(ThreadId(3)), "qbhc ok");
}

#[test]
fn the_keys_line_names_writing_on_an_open_thread_and_a_resolved_one_keeps_its_commands() {
    let view: View = Ok(board_fixture());
    let a = area(160, 30);
    let mut dash = on_t3(&view, a);
    let keys = |dash: &Dash| screen(&view, dash, a)[29].trim_end().to_string();
    assert_eq!(
        keys(&dash),
        " ↑↓ threads · → read its messages · type to write · enter posts · alt+enter new line \
         · esc back · PgUp PgDn scroll · click a thread · ctrl-c quit"
    );
    // With a message focused, → reads more or less and Enter posts.
    dash.handle(&view, a, Input::Right);
    assert!(dash.board_message().is_some());
    assert_eq!(
        keys(&dash),
        " ↑↓ message · → read more or less · ← threads · type to write · enter posts · \
         alt+enter new line · esc back · PgUp PgDn scroll · click a thread · ctrl-c quit"
    );
    // A resolved thread takes no writing: its keys are commands, and q quits.
    dash.handle(&view, a, Input::Left);
    for input in [Input::Down, Input::Down, Input::Right, Input::Down] {
        dash.handle(&view, a, input);
    }
    assert_eq!(dash.board_thread(&view), Some(ThreadId(2)));
    assert!(!dash.writing(&view, a));
    assert_eq!(
        keys(&dash),
        " ↑↓ threads · → read its messages · ← fold resolved · esc back · PgUp PgDn scroll · \
         click a thread · q quit"
    );
    // ← there folds the group, as the keys line says.
    dash.handle(&view, a, Input::Left);
    assert!(!board_of(&view, &dash, a).fold.unfolded);
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
}

#[test]
fn in_the_popup_q_close_shows_only_on_a_resolved_thread_and_esc_then_q_closes() {
    let view: View = Ok(board_fixture());
    let a = area(160, 30);
    let keys = |dash: &Dash| screen(&view, dash, a)[29].trim_end().to_string();
    let mut dash = popup_dash(Item::Task(TaskId(13)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    // On an open thread q is typed, never closing, and the keys line does not offer it.
    assert!(!keys(&dash).contains("q close"), "{}", keys(&dash));
    assert!(keys(&dash).contains("· esc back ·"), "{}", keys(&dash));
    assert_eq!(dash.handle(&view, a, Input::Char('q')), None);
    assert_eq!(dash.draft(ThreadId(3)), "q");
    // Esc, then q, to leave: Esc goes back, the draft kept, and q closes the popup.
    let mut left = dash.clone();
    assert_eq!(left.handle(&view, a, Input::Esc), None);
    assert_eq!(left.view(), ViewKind::Overview);
    assert_eq!(left.handle(&view, a, Input::Char('q')), Some(Request::Quit));
    assert_eq!(left.draft(ThreadId(3)), "q");
    // On a resolved thread nothing is written: "esc back · q close", and q closes.
    for input in [Input::Down, Input::Down, Input::Right, Input::Down] {
        dash.handle(&view, a, input);
    }
    assert_eq!(dash.board_thread(&view), Some(ThreadId(2)));
    assert!(
        keys(&dash).ends_with("esc back · q close"),
        "{}",
        keys(&dash)
    );
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
}

#[test]
fn enter_posts_the_draft_once_and_success_clears_it() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    // Nothing to post yet: Enter does nothing.
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    typed(&mut dash, &view, a, " ");
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    dash.handle(&view, a, Input::Backspace);
    typed(&mut dash, &view, a, "-x copy, approved");
    assert_eq!(
        dash.handle(&view, a, Input::Enter),
        Some(Request::Post {
            thread: ThreadId(3),
            text: "-x copy, approved".into()
        })
    );
    assert_eq!(dash.posting, Some(ThreadId(3)));
    assert!(screen(&view, &dash, a)[29].starts_with(" posting to T3… "));
    // While it runs: no second post, and the draft stays as it is.
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    typed(&mut dash, &view, a, "zz");
    dash.handle(&view, a, Input::Backspace);
    assert_eq!(dash.draft(ThreadId(3)), "-x copy, approved");
    dash.posted(("posted to T3, 7 replies".into(), false));
    assert_eq!(dash.posting, None);
    assert_eq!(dash.draft(ThreadId(3)), "");
    assert!(dash.writing(&view, a));
    let all = screen(&view, &dash, a);
    assert!(
        all[29].starts_with(" posted to T3, 7 replies "),
        "{}",
        all[29]
    );
    // The input is empty, ready for the next message.
    assert!(
        all[27].contains("│ manager ▌Type to write to the team, e.g. copy, approved"),
        "{}",
        all[27]
    );
}

#[test]
fn a_failed_post_shows_why_in_red_and_keeps_the_draft() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "approved");
    assert!(dash.handle(&view, a, Input::Enter).is_some());
    dash.posted((
        "tower: T3 is resolved; resolved threads are read-only".into(),
        true,
    ));
    assert_eq!(dash.draft(ThreadId(3)), "approved");
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let all = rows(&buf);
    assert!(
        all[29].starts_with(" tower: T3 is resolved; resolved threads are read-only"),
        "{}",
        all[29]
    );
    assert_eq!(buf[(2, 29)].fg, bad(Depth::TrueColor));
    // Enter tries again.
    assert_eq!(
        dash.handle(&view, a, Input::Enter),
        Some(Request::Post {
            thread: ThreadId(3),
            text: "approved".into()
        })
    );
}

#[test]
fn a_resolved_thread_is_read_only() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Right);
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(2)));
    let b = board_of(&view, &dash, a);
    assert_eq!((b.input.width, b.post.width), (0, 0));
    let all = screen(&view, &dash, a);
    assert!(
        all[27].ends_with(
            "│ resolved threads are read-only                                                "
        ),
        "{}",
        all[27]
    );
    assert!(!all[29].contains("type to write"), "{}", all[29]);
    dash.handle(&view, a, Input::Char('c'));
    dash.handle(&view, a, Input::Click { x: 60, y: 27 });
    assert!(!dash.writing(&view, a));
    assert_eq!(dash.draft(ThreadId(2)), "");
    // q still quits here: nothing is being written.
    assert!(all[29].contains("q quit"));
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
}

#[test]
fn writing_stops_when_a_refresh_resolves_the_thread_and_the_draft_stays() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    assert!(dash.writing(&view, a));
    typed(&mut dash, &view, a, "hi");
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    assert_eq!(t.id, ThreadId(3));
    let resolution = Resolution {
        by: "senior".into(),
        at: NOW,
    };
    *t = Thread::whole(
        t.id,
        t.title.clone(),
        t.task,
        Some(resolution),
        t.messages.clone(),
    );
    sync(&mut board);
    let resolved: View = Ok(board);
    dash.refreshed(&resolved, a);
    assert!(!dash.writing(&resolved, a));
    assert_eq!(dash.draft(ThreadId(3)), "hi");
    // q is a command again there.
    assert_eq!(
        dash.handle(&resolved, a, Input::Char('q')),
        Some(Request::Quit)
    );
}

#[test]
fn the_mouse_keeps_writing_posts_and_a_click_on_another_thread_writes_there() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    let b = board_of(&view, &dash, a);
    assert_eq!((b.input.y, b.post.y), (27, 27));
    assert!(dash.writing(&view, a));
    typed(&mut dash, &view, a, "approved");
    // A click on the input keeps writing; on Post it posts.
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.input.x + 5,
            y: b.input.y,
        },
    );
    assert!(dash.writing(&view, a));
    assert_eq!(
        dash.handle(
            &view,
            a,
            Input::Click {
                x: b.post.x + 2,
                y: b.post.y
            }
        ),
        Some(Request::Post {
            thread: ThreadId(3),
            text: "approved".into()
        })
    );
    dash.posted(("posted".into(), false));
    // A click on another open thread in the list shows it, and the keys write there; the
    // first thread keeps its draft.
    typed(&mut dash, &view, a, "later");
    let y = screen(&view, &dash, a)
        .iter()
        .position(|r| r.contains("Show each book's reviews"))
        .unwrap() as u16;
    dash.handle(&view, a, Input::Click { x: 3, y });
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    assert!(dash.writing(&view, a));
    typed(&mut dash, &view, a, "q");
    assert_eq!(dash.draft(ThreadId(4)), "q");
    assert_eq!(dash.draft(ThreadId(3)), "later");
}

/// T3's composer `w` by `h` with a draft of a line break and a line wider than the input:
/// its snapshot, the caret after the draft's last character, and the focus showing.
fn composer_snapshot(w: u16, h: u16, lines: u16) {
    let view: View = Ok(board_fixture());
    let a = area(w, h);
    let mut dash = on_t3(&view, a);
    // A typed line break starts a line; a line wider than the input wraps after a space.
    typed(
        &mut dash,
        &view,
        a,
        "copy, approved.\nShip it after qa checks the shelf on a phone",
    );
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            draw(f, &view, &dash, "14:32", Depth::TrueColor);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    assert_exact_width(&buf);
    assert_snapshot(&format!("composer_{w}x{h}"), &buf);
    // The input grew a line for each, the draft's start on its first.
    let b = board_of(&view, &dash, a);
    assert_eq!(b.input.height, lines);
    let all = rows(&buf);
    let first = &all[usize::from(b.input.y)];
    assert!(first.contains("▌copy, approved."), "{first:?}");
    // The terminal's caret sits right after the draft's last character, on its last line.
    let at = terminal.get_cursor_position().unwrap();
    assert_eq!(at.y, b.input.y + b.input.height - 1);
    let row = &all[usize::from(at.y)];
    let before: String = row.chars().take(usize::from(at.x)).collect();
    assert!(before.ends_with("on a phone"), "{row:?} caret at {at:?}");
    // The focus shows: the input on its own background, the accent bar down its left edge,
    // and "manager" in the accent, bold.
    let d = Depth::TrueColor;
    assert_eq!(buf[(b.input.x + 1, b.input.y)].bg, selected_bg(d));
    for y in b.input.y..b.input.y + b.input.height {
        assert_eq!(buf[(b.input.x, y)].symbol(), "▌");
        assert_eq!(buf[(b.input.x, y)].fg, accent(d));
    }
    let label = &buf[(b.input.x - 8, b.input.y)];
    assert_eq!(label.symbol(), "m");
    assert_eq!(label.fg, accent(d));
    assert!(label.modifier.contains(Modifier::BOLD));
}

#[test]
fn composer_snapshot_while_writing_wide() {
    composer_snapshot(120, 30, 2);
}

#[test]
fn composer_snapshot_while_writing_narrow() {
    // 31 columns of text: "Ship it ... the " fills one, "the shelf on a phone" the next.
    composer_snapshot(50, 30, 3);
}

/// What the terminal's caret is at, drawing `dash` `w` by `h`.
fn caret(view: &View, dash: &Dash, w: u16, h: u16) -> (u16, u16) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            draw(f, view, dash, "14:32", Depth::TrueColor);
        })
        .unwrap();
    let at = terminal.get_cursor_position().unwrap();
    (at.x, at.y)
}

#[test]
fn the_input_grows_to_four_lines_as_the_draft_wraps_and_shows_those_around_the_cursor() {
    let view: View = Ok(board_fixture());
    let words = (0..120)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    for (w, h) in [(120, 30), (50, 30), (120, 40)] {
        let a = area(w, h);
        let mut dash = on_t3(&view, a);
        let empty = board_of(&view, &dash, a);
        assert_eq!(empty.input.height, 1, "{w}x{h}");
        dash.paste(&view, a, &words);
        let b = board_of(&view, &dash, a);
        // Four lines at most; the conversation gives up the rows, the composer's bottom stays.
        assert_eq!(b.input.height, 4, "{w}x{h}");
        assert_eq!(b.conv.height, empty.conv.height - 3, "{w}x{h}");
        assert_eq!(
            b.composer.y + b.composer.height,
            empty.composer.y + empty.composer.height
        );
        let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
        assert_exact_width(&buf);
        // The cursor at the end: the last lines show, the draft's end on the last.
        let all = rows(&buf);
        let input = |all: &[String]| -> Vec<String> {
            (b.input.y..b.input.y + 4)
                .map(|y| all[usize::from(y)].clone())
                .collect()
        };
        assert!(input(&all)[3].contains("w119"), "{:#?}", input(&all));
        assert!(!input(&all).iter().any(|r| r.contains("▌w0 w1 ")));
        assert_eq!(caret(&view, &dash, w, h).1, b.input.y + 3, "{w}x{h}");
        // Left back to the start: the first lines show, the caret on the first.
        for _ in 0..words.len() {
            dash.handle(&view, a, Input::Left);
        }
        assert_eq!(dash.draft_at(ThreadId(3)), 0);
        let all = screen(&view, &dash, a);
        assert!(input(&all)[0].contains("▌w0 w1 w2"), "{:#?}", input(&all));
        assert_eq!(caret(&view, &dash, w, h), (b.input.x + 1, b.input.y));
    }
}

#[test]
fn a_paste_goes_in_whole_at_the_cursor_as_if_typed() {
    // One insert for the whole paste, its line ends made plain and a tab
    // four spaces, as typed; a paste at the message limit lands whole.
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    let t = ThreadId(3);
    typed(&mut dash, &view, a, "ab");
    dash.handle(&view, a, Input::Left);
    dash.paste(&view, a, "1\r\n2\r3\t4");
    assert_eq!(dash.draft(t), "a1\n2\n3    4b");
    assert_eq!(dash.draft_at(t), "a1\n2\n3    4".len());
    let long = "x".repeat(4_000);
    dash.paste(&view, a, &long);
    assert_eq!(dash.draft(t), format!("a1\n2\n3    4{long}b"));
    assert_eq!(dash.draft_at(t), format!("a1\n2\n3    4{long}").len());
}

#[test]
fn the_composer_keeps_its_rule_above_the_input_at_every_height() {
    // In the shortest panes the input never sits on the rule. Where no
    // line is left under the rule there is no input, so nothing writes.
    for w in [120u16, 50] {
        for h in 6u16..=40 {
            for draft in ["", "one line", &"word ".repeat(60)] {
                let view: View = Ok(board_fixture());
                let a = area(w, h);
                let mut dash = on_t3(&view, a);
                // Only while the keys write: elsewhere a letter is a command.
                if dash.writing(&view, a) {
                    typed(&mut dash, &view, a, draft);
                }
                let b = board_of(&view, &dash, a);
                if b.composer.height == 0 {
                    continue;
                }
                let shown = rows(&render_dash(&view, &dash, w, h, Depth::TrueColor));
                let rule: String = (shown[usize::from(b.composer.y)].chars())
                    .skip(usize::from(b.composer.x))
                    .take(usize::from(b.composer.width))
                    .collect();
                assert!(
                    !rule.is_empty() && rule.chars().all(|c| c == '─'),
                    "{w}x{h} {draft:?}: {rule:?} {b:?}"
                );
                if b.input.height > 0 {
                    assert!(b.input.y > b.composer.y, "{w}x{h} {draft:?}: {b:?}");
                    assert!(
                        b.input.y + b.input.height <= b.composer.y + b.composer.height,
                        "{w}x{h} {draft:?}: {b:?}"
                    );
                } else {
                    assert!(!dash.writing(&view, a), "{w}x{h} {draft:?}: {b:?}");
                }
            }
        }
    }
}

#[test]
fn left_and_right_move_the_cursor_in_a_draft_where_typing_pasting_and_backspace_act() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    let t = ThreadId(3);
    typed(&mut dash, &view, a, "copy approved");
    for _ in 0.."approved ".len() {
        dash.handle(&view, a, Input::Left);
    }
    // The keys stay in the draft: no message is selected.
    assert_eq!(dash.board_message(), None);
    assert_eq!(dash.draft_at(t), 4);
    typed(&mut dash, &view, a, ",");
    assert_eq!(dash.draft(t), "copy, approved");
    // Backspace takes the character before the cursor, not the last.
    dash.handle(&view, a, Input::Backspace);
    assert_eq!((dash.draft(t), dash.draft_at(t)), ("copy approved", 4));
    typed(&mut dash, &view, a, ",");
    // A paste goes in at the cursor too.
    dash.handle(&view, a, Input::Right);
    dash.paste(&view, a, "all ");
    assert_eq!(dash.draft(t), "copy, all approved");
    // The caret is drawn at the cursor.
    let b = board_of(&view, &dash, a);
    let at = "copy, all ".len() as u16;
    assert_eq!(
        caret(&view, &dash, 120, 30),
        (b.input.x + 1 + at, b.input.y)
    );
    // Right stops at the end, left at the start.
    for _ in 0..30 {
        dash.handle(&view, a, Input::Right);
    }
    assert_eq!(dash.draft_at(t), dash.draft(t).len());
    for _ in 0..30 {
        dash.handle(&view, a, Input::Left);
    }
    assert_eq!(dash.draft_at(t), 0);
    assert_eq!(dash.board_message(), None);
    // A step crosses a whole grapheme cluster, and a combining mark typed after a letter
    // joins it, the cursor after both.
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "a👍🏽b");
    dash.handle(&view, a, Input::Left);
    dash.handle(&view, a, Input::Left);
    assert_eq!(dash.draft_at(t), 1);
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.draft_at(t), "a👍🏽".len());
    typed(&mut dash, &view, a, "e\u{301}");
    assert_eq!(dash.draft(t), "a👍🏽e\u{301}b");
    assert_eq!(dash.draft_at(t), "a👍🏽e\u{301}".len());
    dash.handle(&view, a, Input::Backspace);
    assert_eq!(dash.draft(t), "a👍🏽b");
    // With no draft, the arrows are the board's again: → reads the thread's messages.
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "x");
    dash.handle(&view, a, Input::Backspace);
    assert_eq!(dash.draft(t), "");
    dash.handle(&view, a, Input::Right);
    assert!(dash.board_message().is_some());
}

#[test]
fn alt_enter_starts_a_new_line_in_the_draft_and_anywhere_else_is_enter() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "copy");
    dash.handle(&view, a, Input::Left);
    dash.handle(&view, a, Input::Left);
    assert_eq!(dash.handle(&view, a, Input::NewLine), None);
    assert_eq!(dash.draft(ThreadId(3)), "co\npy");
    assert_eq!(board_of(&view, &dash, a).input.height, 2);
    // Enter posts it, the line break kept.
    assert_eq!(
        dash.handle(&view, a, Input::Enter),
        Some(Request::Post {
            thread: ThreadId(3),
            text: "co\npy".into()
        })
    );
    // Off the board it is Enter: it opens the selection's detail.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::NewLine);
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
}

#[test]
fn a_click_on_read_more_or_show_less_and_scrolling_keep_the_keys_in_the_draft() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    let long = (0..60)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    messages.push(Message {
        id: 398,
        author: "dev-2".into(),
        text: long,
        at: NOW - 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "copy");
    let b = board_of(&view, &dash, a);
    let row = |dash: &Dash, needle: &str| {
        (screen(&view, dash, a).iter())
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?}")) as u16
    };
    let y = row(&dash, "▌ read more");
    dash.handle(&view, a, Input::Click { x: b.conv.x + 4, y });
    assert!(dash.open_messages().contains(&398));
    assert!(dash.writing(&view, a));
    typed(&mut dash, &view, a, ", ok");
    let y = row(&dash, "▌ show less");
    let b = board_of(&view, &dash, a);
    dash.handle(&view, a, Input::Click { x: b.conv.x + 4, y });
    assert!(!dash.open_messages().contains(&398));
    typed(&mut dash, &view, a, " then");
    // Scrolling, by PgUp and by the wheel, keeps them there too.
    dash.handle(&view, a, Input::PageUp);
    let (x, y) = (b.conv.x + 2, b.conv.y + 1);
    dash.handle(&view, a, Input::Wheel { x, y, down: true });
    typed(&mut dash, &view, a, "!");
    assert_eq!(dash.draft(ThreadId(3)), "copy, ok then!");
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
}

#[test]
fn every_composer_row_is_exactly_the_width_at_any_size() {
    let view: View = Ok(board_fixture());
    for w in [1, 10, 17, 20, 30, 50, 68, 69, 70, 100, 133] {
        for h in [1, 3, 5, 8, 13, 30] {
            let a = area(w, h);
            let mut dash = select(Item::Task(TaskId(13)));
            dash.handle(&view, a, Input::Char('b'));
            typed(&mut dash, &view, a, "界 wide 界 text\nand more");
            let buf = render_dash(&view, &dash, w, h, Depth::Ansi256);
            assert_exact_width(&buf);
            if let Some(b) = layout(&view, &dash, a).board {
                assert!(
                    b.composer.y + b.composer.height
                        <= b.conv.y + b.conv.height + b.composer.height
                );
                assert!(b.input.x + b.input.width <= a.width && b.post.x + b.post.width <= a.width);
                assert!(b.input.height <= 4 && b.input.y + b.input.height <= a.height);
            }
        }
    }
}

#[test]
fn a_paste_keeps_line_breaks_turns_tabs_into_spaces_and_drops_hidden_characters() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    // Outside the composer a paste does nothing.
    let mut dash = select(Item::Task(TaskId(13)));
    dash.paste(&view, a, "ignored");
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.draft(ThreadId(3)), "");
    dash.paste(&view, a, "a\tb\x1b[31mc\r\nd\re\u{200b}\u{202e}f\u{7}");
    assert_eq!(dash.draft(ThreadId(3)), "a    b[31mc\nd\nef");
    // Typed hidden characters are dropped the same way.
    typed(&mut dash, &view, a, "\u{1b}\u{feff}");
    assert_eq!(dash.draft(ThreadId(3)), "a    b[31mc\nd\nef");
    // A paste of only hidden characters leaves no draft behind.
    let mut dash = on_t3(&view, a);
    dash.paste(&view, a, "\u{1b}\u{7}");
    assert_eq!(dash.draft(ThreadId(3)), "");
    assert!(screen(&view, &dash, a)[29].contains("enter posts"));
}

#[test]
fn a_paste_while_a_post_runs_says_so_and_leaves_the_draft() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "approved");
    assert!(dash.handle(&view, a, Input::Enter).is_some());
    dash.paste(&view, a, " and more");
    assert_eq!(dash.draft(ThreadId(3)), "approved");
    assert_eq!(
        dash.notice,
        Some((
            "a post is running: paste again once it is done".into(),
            true
        ))
    );
}

#[test]
fn backspace_takes_a_whole_grapheme_and_the_input_never_shows_half_of_one() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    // A combining accent, a flag, an emoji with a skin tone, a wide CJK character. A ZWJ
    // is a format character tower refuses, so it never enters the draft.
    typed(&mut dash, &view, a, "e\u{301}🇫🇷👍🏽界👩\u{200d}💻");
    assert_eq!(dash.draft(ThreadId(3)), "e\u{301}🇫🇷👍🏽界👩💻");
    for want in [
        "e\u{301}🇫🇷👍🏽界👩",
        "e\u{301}🇫🇷👍🏽界",
        "e\u{301}🇫🇷👍🏽",
        "e\u{301}🇫🇷",
        "e\u{301}",
        "",
    ] {
        dash.handle(&view, a, Input::Backspace);
        assert_eq!(dash.draft(ThreadId(3)), want);
    }
    // A long draft of accented letters: each line of the input starts on a whole letter,
    // and every row stays exactly the width, wide characters and emoji included.
    for (w, text) in [
        (50, "e\u{301}".repeat(80)),
        (50, "👍🏽界".repeat(30)),
        (51, "a👍🏽".repeat(30)),
    ] {
        let a = area(w, 30);
        let mut dash = on_t3(&view, a);
        typed(&mut dash, &view, a, &text);
        let buf = render_dash(&view, &dash, w, 30, Depth::TrueColor);
        assert_exact_width(&buf);
        let b = board_of(&view, &dash, a);
        for y in b.input.y..b.input.y + b.input.height {
            let first = buf[(b.input.x + 1, y)].symbol().to_string();
            assert!(
                !first.starts_with('\u{301}') && !first.starts_with('\u{1f3fd}'),
                "{first:?}"
            );
        }
    }
}

#[test]
fn enter_or_post_on_a_blank_draft_says_there_is_nothing_to_post() {
    let view: View = Ok(board_fixture());
    let a = area(120, 30);
    let mut dash = on_t3(&view, a);
    typed(&mut dash, &view, a, "  \n ");
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    assert_eq!(dash.notice, Some(("nothing to post".into(), false)));
    assert!(screen(&view, &dash, a)[29].starts_with(" nothing to post "));
    let b = board_of(&view, &dash, a);
    let post = Input::Click {
        x: b.post.x + 1,
        y: b.post.y,
    };
    assert_eq!(dash.handle(&view, a, post), None);
    assert_eq!(dash.notice, Some(("nothing to post".into(), false)));
    assert_eq!(dash.posting, None);
}

#[test]
fn the_owner_shows_as_manager_in_red_in_the_conversation() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    messages.push(Message {
        id: 397,
        author: "Manager".into(),
        text: "approved, go ahead and call the printer".into(),
        at: NOW,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 30);
    let dash = on_t3(&view, a);
    for depth in [Depth::TrueColor, Depth::Ansi256] {
        let buf = render_dash(&view, &dash, 120, 30, depth);
        let all = rows(&buf);
        // On the right, as "you (manager)", its name ending at the bubble's right edge.
        let y = all
            .iter()
            .position(|r| r.contains("you (manager)"))
            .unwrap() as u16;
        let row = &all[usize::from(y)];
        let x = row[..row.find("you (manager)").unwrap()].chars().count() as u16;
        assert_eq!(buf[(x, y)].fg, bad(depth));
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        let text = all[usize::from(y) + 1].trim_end();
        assert!(text.ends_with("▐"), "{text:?}");
        assert!(text.contains("approved, go ahead"), "{text:?}");
        // Anyone else keeps their role colour, on the left.
        let y = all
            .iter()
            .position(|r| r.contains("│  qa · tester"))
            .unwrap() as u16;
        let x = all[usize::from(y)].find("qa · tester").unwrap();
        let x = all[usize::from(y)][..x].chars().count() as u16;
        assert_ne!(buf[(x, y)].fg, bad(depth));
    }
}

// Alerts and the inbox.

/// The board fixture with every kind of alert: on M1, T4's need-you ask (15m) and a lead's
/// decision in T3 (7m); on M2, an open T5 with the lead's decision (2m), a worker's ask
/// (30m) and a decision already read (40m), and a resolved T6 whose ask is gone and whose
/// decision is read by being resolved.
fn alerts_fixture() -> Board {
    let mut board = board_fixture();
    let msg = |id, author: &str, text: &str, minutes: i64| Message {
        id,
        author: author.into(),
        text: text.into(),
        at: NOW - minutes * 60,
    };
    board.milestones[0].threads[1].messages.push(msg(
        310,
        "cat-lead",
        "decision: books out of print also leave the shelf. @manager for your awareness",
        7,
    ));
    let home = vec![
        msg(
            500,
            "checkout-lead",
            "decision: 3 books per row. @manager for your awareness",
            40,
        ),
        msg(
            501,
            "dev-1",
            "question: @manager which book goes first?",
            30,
        ),
        msg(
            502,
            "checkout-lead",
            "decision: phones get 1 book per row. @manager for your awareness",
            2,
        ),
    ];
    let gone = vec![
        msg(600, "dev-2", "question: @manager approve the font?", 90),
        msg(
            601,
            "checkout-lead",
            "decision: system font. @manager for your awareness",
            80,
        ),
        msg(
            602,
            "checkout-lead",
            "resolved: font approved in review",
            70,
        ),
    ];
    board.milestones[1].threads = vec![
        Thread::whole(ThreadId(5), "Shelf".into(), None, None, home),
        Thread::whole(
            ThreadId(6),
            "Font".into(),
            None,
            Some(Resolution {
                by: "checkout-lead".into(),
                at: NOW - 70 * 60,
            }),
            gone,
        ),
    ];
    with_alerts(&mut board, &[500]);
    board
}

#[test]
fn a_frame_drawn_mid_minute_shows_each_message_at_one_age() {
    // Drawn 45 s into a minute, the alert line, the inbox and the full board's list reckoned
    // a message's age at that second while the bubbles reckoned it at the minute, so
    // "newest: cat-lead ... · 15m" sat over "cat-lead · needs you · 14m". Every age on a
    // frame goes by the dashboard's minute, so the frame is the one drawn on the minute.
    let mut board = alerts_fixture();
    // Each message posted at second 30 of its minute, the alerts taken again from them.
    for t in board
        .milestones
        .iter_mut()
        .flat_map(|m| m.threads.iter_mut())
    {
        for m in &mut t.messages {
            m.at += 30;
        }
    }
    with_alerts(&mut board, &[500]);
    let view: View = Ok(board);
    let a = area(160, 50);
    let frame = |dash: &Dash, at: i64| -> Vec<String> {
        let mut dash = dash.clone();
        dash.set_now(at);
        let mut terminal = Terminal::new(TestBackend::new(a.width, a.height)).unwrap();
        terminal
            .draw(|f| {
                draw(f, &view, &dash, "14:32", Depth::TrueColor);
            })
            .unwrap();
        rows(terminal.backend().buffer())
    };
    let overview = select(Item::Milestone(MilestoneId(1)));
    let on = frame(&overview, NOW);
    assert!(
        on[0].contains("newest: cat-lead") && on[0].contains(" · 14m "),
        "{}",
        on[0]
    );
    let mut inbox = overview.clone();
    inbox.handle(&view, a, Input::Char('i'));
    assert_eq!(inbox.view(), ViewKind::Inbox);
    let mut full = overview.clone();
    full.handle(&view, a, Input::Char('b'));
    assert!(layout(&view, &full, a).board.is_some());
    for (name, dash) in [
        ("overview", &overview),
        ("inbox", &inbox),
        ("full board", &full),
    ] {
        assert_eq!(frame(dash, NOW + 45), frame(dash, NOW), "{name}");
    }
}

#[test]
fn the_alert_line_counts_by_message_and_names_the_newest_need_you_item() {
    let view: View = Ok(alerts_fixture());
    let all = rows(&render(&view, 160, 20));
    // The newest question's author and where it is, in words, no text; the thread's
    // name is cut first, so its repo and age still show.
    let newest = " ⚠ 2 need you · ⓘ 2 for your awareness · newest: cat-lead in ‘Show each \
                  book's reviews under its cover, newest …’ · catalogue · 15m";
    let line = alert_line(view.as_ref().unwrap(), 160, Some(NOW), Depth::Ansi256).to_string();
    assert!(line.starts_with(newest), "{line}");
    assert!(line.ends_with(" new lead (n) ▶  inbox (i) ▶ "), "{line}");
    assert_eq!(line.chars().count(), 160);
    assert!(!line.contains("the covers are late again"));
    // On screen it is the first line, ending with the clock.
    assert!(
        all[0].starts_with(" ⚠ 2 need you · ⓘ 2 for"),
        "{:?}",
        all[0]
    );
    assert!(
        all[0].ends_with("new lead (n) ▶  inbox (i) ▶  14:32 "),
        "{:?}",
        all[0]
    );
    // The body starts right under it.
    assert!(all[1].starts_with(" ▾ catalogue"), "{:?}", all[1]);
}

#[test]
fn the_alert_line_says_nothing_needs_you_and_keeps_awareness() {
    let mut board = led_fixture();
    let view: View = Ok(board.clone());
    let line = rows(&render(&view, 80, 20))[0].clone();
    assert_eq!(
        line,
        format!(
            "{:<44}{}",
            " nothing needs you", " new lead (n) ▶  inbox (i) ▶  14:32 "
        )
    );
    let buf = render(&view, 80, 20);
    assert_eq!(buf[(1, 0)].fg, ok(Depth::TrueColor));
    // Only a decision: nothing needs you, one for your awareness.
    board.milestones[0].threads = vec![Thread::whole(
        ThreadId(1),
        "t".into(),
        None,
        None,
        vec![Message {
            id: 1,
            author: "cat-lead".into(),
            text: "decision: x. @manager for your awareness".into(),
            at: NOW,
        }],
    )];
    with_alerts(&mut board, &[]);
    let line = rows(&render(&Ok(board), 80, 20))[0].trim_end().to_string();
    assert_eq!(
        line,
        format!(
            "{:<44}{}",
            " nothing needs you · ⓘ 1 for your awareness", " new lead (n) ▶  inbox (i) ▶  14:32"
        )
    );
}

#[test]
fn the_alert_line_is_one_line_of_exactly_the_width_at_any_width() {
    let view: View = Ok(alerts_fixture());
    for w in 1..=170u16 {
        let buf = render(&view, w, 12);
        assert_exact_width(&buf);
        if w < 4 {
            continue;
        }
        let all = rows(&buf);
        // Never a second line: the tree's first repo row is still right under it.
        assert!(all[1].starts_with(" ▾"), "w={w}: {:?}", all[1]);
    }
    let line = |w: u16| {
        alert_line(
            view.as_ref().unwrap(),
            usize::from(w),
            Some(NOW),
            Depth::Ansi256,
        )
        .to_string()
    };
    // Where the newest item is gets cut first (thread, then repo); its author and age stay
    // together. Beside "inbox (i) ▶" while they fit, else in its place, else it goes whole
    // and "inbox (i) ▶" is back while it fits; the counts stay whole until they alone are
    // too wide.
    let counts = " ⚠ 2 need you · ⓘ 2 for your awareness";
    let item = " · newest: cat-lead · 15m";
    // The line reserves an age at its widest ("999d"), one column more than "15m".
    let (cw, iw) = (
        counts.chars().count() as u16,
        item.chars().count() as u16 + 1,
    );
    assert_eq!(
        line(80),
        // Fewer than four columns of the repo would fit: it is left out, no "m…" stub.
        format!("{counts} · newest: cat-lead · 15m     inbox (i) ▶ ")
    );
    // Room for author, age and "inbox (i) ▶" only: both.
    assert_eq!(line(cw + iw + 13), format!("{counts}{item}  inbox (i) ▶ "));
    // One column short: the item takes the place of "inbox (i) ▶", and the room it frees
    // goes to its repo.
    assert_eq!(
        line(cw + iw + 12),
        format!("{counts} · newest: cat-lead · catalogue · 15m ")
    );
    assert_eq!(line(cw + iw), format!("{counts}{item} "));
    // Too narrow for author and age: no stub, "inbox (i) ▶" while it fits.
    assert_eq!(
        line(cw + iw - 1),
        format!("{counts}{} inbox (i) ▶ ", " ".repeat(usize::from(iw) - 14))
    );
    // Too narrow for a few characters of it: "inbox (i) ▶" while it fits.
    assert_eq!(line(cw + 15), format!("{counts}   inbox (i) ▶ "));
    assert_eq!(line(cw + 13), format!("{counts} inbox (i) ▶ "));
    assert_eq!(
        line(cw + 12),
        format!("{counts:<w$}", w = usize::from(cw) + 12)
    );
    assert_eq!(line(cw), counts);
    assert_eq!(line(14), " ⚠ 2 need you…");
    assert_eq!(line(10), " ⚠ 2 need…");
}

#[test]
fn alert_line_snapshots_wide_and_narrow() {
    let view: View = Ok(alerts_fixture());
    for (w, h) in [(120, 24), (40, 24)] {
        let buf = render(&view, w, h);
        assert_exact_width(&buf);
        assert_snapshot(&format!("alerts_{w}x{h}"), &buf);
    }
}

#[test]
fn each_repo_row_shows_its_own_badges() {
    let board = alerts_fixture();
    let lines = tree_lines(&board, 60, Depth::TrueColor);
    let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let cat = find_row(&text, " ▾ catalogue");
    assert!(text[cat].ends_with(" ⚠1 ⓘ1 "), "{:?}", text[cat]);
    let home = find_row(&text, " ▾ checkout");
    assert!(text[home].ends_with(" ⚠1 ⓘ1 "), "{:?}", text[home]);
    let span = |row: usize, s: &str| {
        (lines[row].spans.iter())
            .find(|x| x.content == s)
            .unwrap_or_else(|| panic!("{s:?} not on {:?}", text[row]))
            .style
    };
    assert_eq!(span(cat, " ⚠1").fg, Some(Color::Rgb(0xff, 0x6b, 0x6b)));
    // Awareness in normal text, in bold (red is for what needs the owner).
    assert_eq!(span(cat, " ⓘ1").fg, None);
    assert!(span(cat, " ⓘ1").add_modifier.contains(Modifier::BOLD));
    // No alerts, no badges.
    let quiet = tree_lines(&led_fixture(), 60, Depth::TrueColor);
    assert!(!quiet[0].to_string().contains('⚠'));
}

#[test]
fn reading_or_resolving_takes_an_item_off_the_alert_line_and_badges() {
    let mut board = alerts_fixture();
    with_alerts(&mut board, &[500, 310]);
    let all = rows(&render(&Ok(board.clone()), 160, 20));
    assert!(all[0].contains("ⓘ 1 for your awareness"), "{}", all[0]);
    assert!(
        all[1].contains(" ⚠1 │") && !all[1].contains('ⓘ'),
        "{:?}",
        all[1]
    );
    // T4 resolved: its ask leaves; M2's ask is now the newest need-you item.
    board.milestones[0].threads[2].resolved = Some(Resolution {
        by: "cat-lead".into(),
        at: NOW,
    });
    with_alerts(&mut board, &[500, 310]);
    let all = rows(&render(&Ok(board), 160, 20));
    assert!(
        all[0].starts_with(" ⚠ 1 need you · ⓘ 1 for your awareness · newest: dev-1 in ‘Shelf’"),
        "{}",
        all[0]
    );
}

fn inbox_dash(view: &View, a: Rect) -> Dash {
    let mut dash = clocked();
    assert_eq!(dash.handle(view, a, Input::Char('i')), None);
    assert_eq!(dash.view(), ViewKind::Inbox);
    dash
}

/// The alerts board at a busy board's volume: 33 resolved threads on one repo's
/// board, 12 awareness items and 80 read ones.
fn volume_fixture() -> Board {
    let mut board = alerts_fixture();
    let topics = [
        "Shelf labels",
        "Author photos",
        "Search typos",
        "Staff picks",
        "Cover sizes",
        "Poetry corner",
        "Signing day",
        "Opening hours",
        "Reading list",
    ];
    for i in 0..33 {
        let id = 1000 + i;
        let minutes = 60 * (i + 1);
        let topic = topics[i as usize % topics.len()];
        let title = format!("{topic}, round {}", i as usize / topics.len() + 1);
        let messages = vec![
            Message {
                id: id * 10,
                author: "dev-2".into(),
                text: format!("done: {topic} ready to look at. @manager for your awareness"),
                at: NOW - (minutes + 5) * 60,
            },
            Message {
                id: id * 10 + 1,
                author: "cat-lead".into(),
                text: "resolved: live on the site; qa checked it on a phone".into(),
                at: NOW - minutes * 60,
            },
        ];
        let resolved = Some(Resolution {
            by: "cat-lead".into(),
            at: NOW - minutes * 60,
        });
        board.milestones[0].threads.push(Thread::whole(
            ThreadId(id),
            title,
            None,
            resolved,
            messages,
        ));
    }
    // On the open Shelf thread: 10 more unread awareness items (12 in all) and 80 read.
    let shelf = &mut board.milestones[1].threads[0].messages;
    let mut read = vec![500];
    for i in 0..90 {
        let id = 5000 + i;
        shelf.push(Message {
            id,
            author: "checkout-lead".into(),
            text: format!(
                "decision: item {i}, books keep their order. @manager for your awareness"
            ),
            at: NOW - (200 - i) * 60,
        });
        if i < 80 {
            read.push(id);
        }
    }
    shelf.sort_by_key(|m| (m.at, m.id));
    with_alerts(&mut board, &read);
    board
}

#[test]
fn at_volume_the_inbox_lists_no_read_items() {
    let view: View = Ok(volume_fixture());
    let b0 = view.as_ref().unwrap();
    assert_eq!(b0.inbox.awareness.len(), 12);
    // The board holds 80 read mentions of @manager besides those items; none is listed.
    assert!(mentions_of(b0).len() >= b0.inbox.all().count() + 80);
    // The inbox: its two sections, nothing read, fits a 40-line screen.
    let a = area(120, 40);
    let dash = inbox_dash(&view, a);
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("volume_inbox_120x40", &buf);
    let all = rows(&buf);
    assert!(!all.iter().any(|r| r.contains("READ")), "{all:#?}");
    let ib = layout(&view, &dash, a).inbox.unwrap();
    // An item on each of its lines: a message that wraps takes two.
    let mut listed: Vec<i64> = ib.rows.iter().flatten().copied().collect();
    listed.dedup();
    assert_eq!(listed.len(), 2 + 12);
}

/// The full board of the volume fixture's M1 in `a`.
fn volume_board(view: &View, a: Rect) -> Dash {
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(view, a, Input::Char('b'));
    dash
}

#[test]
fn at_volume_the_full_board_folds_resolved_threads() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 40);
    // 2 open threads, then one RESOLVED row, folded.
    let dash = volume_board(&view, a);
    let b = board_of(&view, &dash, a);
    // OPEN, two lines a thread with a blank between (three for #14's, whose name wraps), a blank,
    // RESOLVED.
    assert_eq!(b.list_rows.len(), 1 + 2 + 1 + 3 + 1 + 1, "{b:?}");
    // Its snapshot is a test of its own, so CI prints each changed one.
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    assert!(rows(&buf).iter().any(|r| r.starts_with(" RESOLVED 34 ▸ ")));
}

#[test]
fn at_volume_the_unfolded_list_keeps_its_selection_listed_and_in_view() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 40);
    let mut dash = volume_board(&view, a);
    // Walk every stop down and back up, unfolded on the way: the thread shown is always
    // listed, and the selection never on a line the list does not have.
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Right);
    let b = board_of(&view, &dash, a);
    assert_eq!(b.list_rows.len(), 1 + 2 + 1 + 3 + 1 + 1 + 34, "{b:?}");
    for input in [Input::Down; 50].into_iter().chain([Input::Up; 50]) {
        dash.handle(&view, a, input);
        let b = board_of(&view, &dash, a);
        // A stop is selected, and it is one of the list's rows: never a hidden thread.
        assert!(b.current().is_some(), "{b:?}");
        assert!(b.list_rows.contains(&b.current()), "{b:?}");
        // The selected stop is on screen.
        let row = b.list_rows.iter().position(|r| *r == b.current()).unwrap();
        let top = dash.list_scroll();
        assert!(
            (top..top + usize::from(b.list.height)).contains(&row),
            "{row} {top}"
        );
    }
    // Down to the twentieth resolved thread: the unfolded list, scrolled to it.
    dash.handle(&view, a, Input::Down);
    for _ in 0..22 {
        dash.handle(&view, a, Input::Down);
    }
    // (volume_full_board_unfolded_snapshot draws it.)
    // The wheel scrolls the long list on its own.
    let b = board_of(&view, &dash, a);
    let before = dash.list_scroll();
    let (x, y) = (b.list.x + 2, b.list.y + 1);
    dash.handle(&view, a, Input::Wheel { x, y, down: false });
    assert!(dash.list_scroll() < before);
    // Further down, the list scrolled well past its folded length.
    for _ in 0..20 {
        dash.handle(&view, a, Input::Down);
    }
    assert!(dash.list_scroll() > 8, "{}", dash.list_scroll());
    // ← folds it, the selection on the row, scrolled into view; the hidden thread's
    // conversation stays until another thread is picked.
    let shown = dash.board_thread(&view);
    dash.handle(&view, a, Input::Left);
    // The next fit (before each frame) keeps the scroll within the folded list.
    dash.fit(&view, a);
    let b = board_of(&view, &dash, a);
    assert_eq!(
        (b.current(), b.fold.unfolded),
        (Some(Stop::Resolved), false)
    );
    let max = b.list_rows.len().saturating_sub(usize::from(b.list.height));
    assert!(dash.list_scroll() <= max, "{} > {max}", dash.list_scroll());
    assert_eq!(dash.board_thread(&view), shown);
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    let all = rows(&buf);
    let y = all
        .iter()
        .position(|r| r.starts_with(" RESOLVED 34 ▸ "))
        .unwrap();
    assert_eq!(buf[(5, y as u16)].bg, selected_bg(Depth::TrueColor));
    assert!(all[39].contains("→ unfold"), "{}", all[39]);
}

#[test]
fn volume_full_board_snapshot() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 40);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("volume_full_board_120x40", &buf);
}

/// The full board at volume, unfolded and scrolled to the twentieth resolved thread, by
/// the same steps as `at_volume_the_unfolded_list_keeps_its_selection_listed_and_in_view`.
#[test]
fn volume_full_board_unfolded_snapshot() {
    let view: View = Ok(volume_fixture());
    let a = area(120, 40);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    for input in [Input::Down, Input::Down, Input::Right]
        .into_iter()
        .chain([Input::Down; 50])
        .chain([Input::Up; 50])
        .chain([Input::Down; 23])
    {
        dash.handle(&view, a, input);
    }
    let buf = render_dash(&view, &dash, 120, 40, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("volume_full_board_unfolded_120x40", &buf);
}

#[test]
fn inbox_snapshots_wide_and_narrow() {
    let view: View = Ok(alerts_fixture());
    for (w, h) in [(120, 24), (40, 24)] {
        let dash = inbox_dash(&view, area(w, h));
        let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
        assert_exact_width(&buf);
        assert_snapshot(&format!("inbox_{w}x{h}"), &buf);
    }
}

#[test]
fn the_inbox_lists_need_you_then_awareness_newest_first_and_no_read_section() {
    let view: View = Ok(alerts_fixture());
    let dash = inbox_dash(&view, area(120, 30));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    let body: Vec<String> = all[0..15]
        .iter()
        .map(|r| r.trim_end().to_string())
        .collect();
    // Each item: its author (the column as wide as the longest), the message, and at the
    // right its repo, its thread in words (cut so the repo and age stay) and its age. A
    // message too long for its line goes on in a second one, under its first.
    let item = |author: &str, text: &str, meta: &str| {
        let left = format!("   {author:<13}  {text}");
        format!("{left:<w$}{meta}", w = 120 - meta.chars().count() - 1)
    };
    let more = |text: &str| format!("{:18}{text}", "");
    assert_eq!(
        body,
        [
            format!("{:<114}14:32", " ◀ back  INBOX"),
            " ⚠ 2 NEED YOU".into(),
            item(
                "cat-lead",
                "question: the covers are late again. @manager can you call the",
                " catalogue · Show each book's re… · 15m"
            ),
            more("printer?"),
            item(
                "dev-1",
                "question: @manager which book goes first?",
                " checkout · Shelf · 30m"
            ),
            String::new(),
            " ⓘ 2 FOR YOUR AWARENESS".into(),
            item(
                "checkout-lead",
                "decision: phones get 1 book per row. @manager for your awareness",
                " checkout · Shelf · 2m"
            ),
            item(
                "cat-lead",
                "decision: books out of print also leave the shelf. @manager for",
                " catalogue · New book shelf · 7m"
            ),
            more("your awareness"),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ]
    );
    // The read items are not in the inbox; they stay in their threads.
    assert!(
        !all.iter()
            .any(|r| r.contains("READ") || r.contains("3 books per row"))
    );
    // Authors in their role colours: the lead in the accent, a dev teal.
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    assert_eq!(buf[(3, 2)].fg, accent(Depth::TrueColor));
    assert_eq!(
        buf[(3, 4)].fg,
        control_tower::ui::role_colour("dev-1", None, Depth::TrueColor)
    );
    // The first item is selected, and the keys line says what works here: r clears a
    // need-you item.
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    assert_eq!(buf[(2, 2)].bg, selected_bg(Depth::TrueColor));
    assert_eq!(
        all[29].trim_end(),
        " ↑↓ select · enter open thread · r clear · esc back · PgUp PgDn scroll · click an item · q quit"
    );
    // On an awareness item, r marks it read.
    let mut dash = dash;
    let a = area(120, 30);
    for _ in 0..2 {
        dash.handle(&view, a, Input::Down);
    }
    let keys = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor))[29].clone();
    assert!(keys.contains(" · r mark read · "), "{keys}");
}

#[test]
fn the_inbox_opens_a_thread_and_esc_comes_back_to_the_inbox_then_the_tree() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Task(TaskId(21)));
    dash.handle(&view, a, Input::Char('i'));
    // Down past the end stops at the last item; Up comes back.
    for _ in 0..3 {
        dash.handle(&view, a, Input::Down);
    }
    let board = view.as_ref().unwrap();
    assert_eq!(dash.inbox_selected(board), Some(310));
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    assert!(all[0].contains("Catalogue Lead"), "{:?}", all[0]);
    // Esc: back to the inbox, on the same item.
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::Inbox);
    assert_eq!(dash.inbox_selected(board), Some(310));
    // Another lead's item opens that lead's board.
    dash.handle(&view, a, Input::Up);
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(5)));
    dash.handle(&view, a, Input::Esc);
    // Esc again: back to the tree, the selection as it was.
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::Overview);
    assert_eq!(dash.selection(board), Some(Item::Task(TaskId(21))));
    // `i` toggles the inbox shut too, as in the signed design.
    dash.handle(&view, a, Input::Char('i'));
    dash.handle(&view, a, Input::Char('i'));
    assert_eq!(dash.view(), ViewKind::Overview);
}

#[test]
fn clicks_open_the_inbox_from_the_alert_line_and_a_thread_from_an_item() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = clocked();
    dash.handle(&view, a, Input::Click { x: 30, y: 0 });
    assert_eq!(dash.view(), ViewKind::Inbox);
    // Row 4 is the second need-you item (M2's T5), under the first's two lines.
    dash.handle(&view, a, Input::Click { x: 40, y: 4 });
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(5)));
    dash.handle(&view, a, Input::Esc);
    // A click on a section head does nothing; one on "◀ back" goes back.
    dash.handle(&view, a, Input::Click { x: 5, y: 1 });
    assert_eq!(dash.view(), ViewKind::Inbox);
    dash.handle(&view, a, Input::Click { x: 3, y: 0 });
    assert_eq!(dash.view(), ViewKind::Overview);
}

#[test]
fn opening_a_thread_reads_its_awareness_items_once_and_leaves_its_need_you_items() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = clocked();
    // Nothing is open: nothing is read.
    assert!(dash.opened(&view).is_empty());
    dash.handle(&view, a, Input::Click { x: 30, y: 0 });
    // The inbox lists T5's items, but listing them is not opening their thread.
    assert!(dash.opened(&view).is_empty());
    // Row 4 is T5's need-you item (501), under 402's two lines; T5 also has unread 502
    // and read 500.
    dash.handle(&view, a, Input::Click { x: 40, y: 4 });
    assert_eq!(dash.board_thread(&view), Some(ThreadId(5)));
    assert_eq!(dash.opened(&view), [502]);
    // Once, as it opens: the next look reads nothing, even before the write lands.
    assert!(dash.opened(&view).is_empty());
    // An item posted while T5 stays open stays unread, and the question still needs the
    // owner.
    let mut board = alerts_fixture();
    board.milestones[1].threads[0].messages.push(Message {
        id: 503,
        author: "checkout-lead".into(),
        text: "@manager done: shelf merged".into(),
        at: NOW,
    });
    with_alerts(&mut board, &[500, 502]);
    let later: View = Ok(board);
    // A refresh that fails in between is no closing: the next good one is no opening.
    let failed: View = Err("the board is locked".into());
    assert!(dash.opened(&failed).is_empty());
    assert_eq!(dash.board_thread(&later), Some(ThreadId(5)));
    assert!(dash.opened(&later).is_empty());
    let inbox = &later.as_ref().unwrap().inbox;
    assert!(inbox.awareness.iter().any(|a| a.message.id == 503));
    assert!(inbox.need_you.iter().any(|a| a.message.id == 501));
    // Opened again (back to the inbox, then the thread), it is read.
    dash.handle(&later, a, Input::Esc);
    assert!(dash.opened(&later).is_empty());
    dash.handle(&later, a, Input::Click { x: 40, y: 4 });
    assert_eq!(dash.board_thread(&later), Some(ThreadId(5)));
    assert_eq!(dash.opened(&later), [503]);
}

#[test]
fn r_asks_to_mark_a_need_you_or_unread_awareness_item_read() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = inbox_dash(&view, a);
    // The owner clears a need-you item with r.
    // Named as the inbox names it, never by a message id.
    assert_eq!(
        dash.handle(&view, a, Input::Char('r')),
        Some(Request::Clear {
            id: 402,
            item: "cat-lead in ‘Show each book's reviews under its cover, newest first, with a link to the rest’ \
                   · catalogue"
                .into()
        })
    );
    assert_eq!(dash.handing.as_deref(), Some("clearing…"));
    dash.handing = None;
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Down);
    assert_eq!(
        dash.handle(&view, a, Input::Char('r')),
        Some(Request::MarkRead {
            id: 502,
            item: "checkout-lead in ‘Shelf’ · checkout".into()
        })
    );
    assert_eq!(dash.handing.as_deref(), Some("marking read…"));
    // No second write while one runs.
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.handle(&view, a, Input::Char('r')), None);
    // Once read, the item leaves the inbox (no read section) and the item now in its
    // place is selected, not the first. The list was 402 and 501 (need you), then 502 and
    // 310 (awareness); 502 was third.
    dash.handing = None;
    dash.handle(&view, a, Input::Up);
    let ids = |b: &Board| b.inbox.all().map(|x| x.message.id).collect::<Vec<_>>();
    assert_eq!(ids(view.as_ref().unwrap()), [402, 501, 502, 310]);
    let mut board = alerts_fixture();
    with_alerts(&mut board, &[500, 502]);
    assert_eq!(ids(&board), [402, 501, 310]);
    assert_eq!(dash.inbox_selected(&board), Some(310));
    // 310 read too: nothing is in its place, so the last item.
    let mut board = alerts_fixture();
    with_alerts(&mut board, &[500, 502, 310]);
    assert_eq!(ids(&board), [402, 501]);
    assert_eq!(dash.inbox_selected(&board), Some(501));
}

#[test]
fn an_empty_inbox_says_nothing_in_each_section() {
    let view: View = Ok(led_fixture());
    let a = area(60, 14);
    let mut dash = inbox_dash(&view, a);
    let all = rows(&render_dash(&view, &dash, 60, 14, Depth::TrueColor));
    let body: Vec<&str> = all[1..6].iter().map(|r| r.trim_end()).collect();
    assert_eq!(
        body,
        [
            " ⚠ 0 NEED YOU",
            "   nothing",
            "",
            " ⓘ 0 FOR YOUR AWARENESS",
            "   nothing",
        ]
    );
    for input in [Input::Down, Input::Up, Input::Enter, Input::Char('r')] {
        assert_eq!(dash.handle(&view, a, input), None);
        assert_eq!(dash.view(), ViewKind::Inbox);
    }
    // Its keys line offers only the way out, in the popup too.
    assert_eq!(all[13].trim_end(), " esc back · q quit");
    let mut popup = popup_dash(Item::Milestone(MilestoneId(1)));
    popup.set_now(NOW);
    popup.handle(&view, a, Input::Char('i'));
    assert_eq!(popup.view(), ViewKind::Inbox);
    let keys = screen(&view, &popup, a)[13].trim_end().to_string();
    assert_eq!(keys, " esc back · q close");
}

/// In the inbox the wheel (and PgUp PgDn) move the selection with the view: an item
/// scrolled out of sight hands it to the nearest item in view, so Enter and `r` never act on
/// an item the owner cannot see.
#[test]
fn the_inbox_selection_stays_in_view_as_the_wheel_scrolls() {
    // A busy board: twelve awareness items, far more than nine rows show.
    let view: View = Ok(volume_fixture());
    let Ok(board) = &view else { unreachable!() };
    let a = area(120, 9);
    let mut dash = inbox_dash(&view, a);
    let first = dash.inbox_selected(board);
    let in_view = |dash: &Dash| {
        let ib = layout(&view, dash, a).inbox.unwrap();
        let h = usize::from(ib.list.height);
        let top = dash.inbox_scroll().min(ib.rows.len().saturating_sub(h));
        let id = dash.inbox_selected(board);
        let lines: Vec<usize> = (ib.rows.iter().enumerate())
            .filter(|(_, r)| **r == id)
            .map(|(i, _)| i)
            .collect();
        assert!(!lines.is_empty(), "{id:?} not listed");
        let whole = lines.iter().all(|l| (top..top + h).contains(l));
        (whole, top, ib.list)
    };
    let (_, _, list) = in_view(&dash);
    let (x, y) = (list.x + 4, list.y + 1);
    let mut moved = false;
    for down in [true; 12].into_iter().chain([false; 12]) {
        dash.handle(&view, a, Input::Wheel { x, y, down });
        let (whole, top, _) = in_view(&dash);
        assert!(whole, "selection out of view at scroll {top}");
        moved |= dash.inbox_selected(board) != first;
    }
    assert!(moved, "the wheel never moved the selection");
    for input in [Input::PageDown, Input::PageDown, Input::PageUp] {
        dash.handle(&view, a, input);
        assert!(in_view(&dash).0, "{input:?}");
    }
    // Enter opens the item in view.
    let id = dash.inbox_selected(board).unwrap();
    let thread = board
        .inbox
        .all()
        .find(|i| i.message.id == id)
        .unwrap()
        .thread;
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.board_thread_wanted(), Some(thread));
}

/// A thread whose only message is a system line has no message → can select, so
/// the full board does not offer "→ read its messages", and → does nothing there.
#[test]
fn a_thread_of_only_a_system_line_offers_no_read_its_messages() {
    let mut board = led_fixture();
    board.milestones[0].threads = vec![Thread::whole(
        ThreadId(9),
        "Lead not listening".into(),
        None,
        None,
        vec![Message {
            id: 900,
            author: "system".into(),
            text: "the lead is not listening on its board".into(),
            at: NOW - 60,
        }],
    )];
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(9)));
    let keys = screen(&view, &dash, a)[29].clone();
    assert!(keys.contains("↑↓ thread"), "{keys}");
    assert!(!keys.contains("read its messages"), "{keys}");
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), None);
}

#[test]
fn every_inbox_row_is_exactly_the_width_at_any_size() {
    let view: View = Ok(alerts_fixture());
    for w in 1..=130u16 {
        for h in [3, 8, 24] {
            let dash = inbox_dash(&view, area(w, h));
            assert_exact_width(&render_dash(&view, &dash, w, h, Depth::TrueColor));
        }
    }
}

/// The board fixture with M1's tracker link and its threads, for the controls.
fn controls_fixture() -> Board {
    let mut board = board_fixture();
    board.milestones[0].details.link = Some("https://github.com/o/r/milestone/1".into());
    board
}

/// Clicks the middle of `control` in the right pane of `dash`'s layout in `a`.
fn click_control(view: &View, dash: &mut Dash, a: Rect, control: Control) {
    let l = layout(view, dash, a);
    let r = l.right.expect("a right pane");
    let controls = (l.overview.as_ref().map(|o| o.controls.clone()))
        .or(l.detail.as_ref().map(|d| d.controls.clone()))
        .unwrap();
    let c = (controls.iter())
        .find(|c| c.control == control)
        .unwrap_or_else(|| panic!("no {control:?} in {controls:?}"));
    let x = r.x + ((c.from + c.to) / 2) as u16;
    dash.handle(
        view,
        a,
        Input::Click {
            x,
            y: r.y + c.line as u16,
        },
    );
}

#[test]
fn the_overview_header_shows_the_tracker_link_after_the_lead_and_the_open_control() {
    let board = controls_fixture();
    let m = &board.milestones[0];
    let line = |task, w| overview(m, task, w, Depth::TrueColor).lines[0].to_string();
    assert_eq!(
        line(None, 104),
        format!(
            "{:<79}{}",
            " M1  First shop pages · Catalogue Lead · https://github.com/o/r/milestone/1",
            "open milestone (enter) ▶ "
        )
    );
    // With a task selected it opens the task; the link is cut to the width.
    assert_eq!(
        line(Some(TaskId(13)), 80),
        " M1  First shop pages · Catalogue Lead · https://github.co… open task (enter) ▶ "
    );
    // Short of room the key goes, then the control; the id and 8 columns of title stay.
    assert_eq!(line(None, 35), " M1  First shop … open milestone ▶ ");
    assert_eq!(line(None, 31), " M1  First s… open milestone ▶ ");
    assert_eq!(line(None, 30), " M1  First shop pages · Catal…");
    // No link: the header ends at the lead, as before.
    let plain = &led_fixture().milestones[0];
    let o = overview(plain, None, 80, Depth::TrueColor);
    assert!(
        o.lines[0]
            .to_string()
            .starts_with(" M1  First shop pages · Catalogue Lead  ")
    );
    // The control is in the accent colour, and every line is exactly the width.
    let o = overview(m, None, 100, Depth::TrueColor);
    let span = (o.lines[0].spans.iter())
        .find(|s| s.content.starts_with("open milestone"))
        .unwrap();
    assert_eq!(span.style.fg, Some(accent(Depth::TrueColor)));
    for w in 0..=130 {
        for task in [None, Some(TaskId(13))] {
            let o = overview(m, task, w, Depth::Ansi256);
            assert!(o.lines.iter().all(|l| l.width() == w), "w={w}");
            // The layout's control is where the line draws it.
            for c in overview_layout(m, task, w).controls {
                let row: String = o.lines[c.line].to_string().chars().skip(c.from).collect();
                assert!(row.starts_with("open "), "w={w} {row:?}");
            }
        }
    }
}

#[test]
fn the_message_board_heading_has_open_full_board_while_there_is_a_lead() {
    let view: View = Ok(controls_fixture());
    let a = area(120, 50);
    let all = rows(&render_dash(
        &view,
        &select(Item::Milestone(MilestoneId(1))),
        120,
        50,
        Depth::TrueColor,
    ));
    let at = (all.iter())
        .position(|r| r.contains("│ MESSAGE BOARD"))
        .unwrap();
    assert!(all[at].ends_with("open full board (b) ▶ "), "{:?}", all[at]);
    let l = layout(&view, &select(Item::Milestone(MilestoneId(1))), a);
    let o = l.overview.unwrap();
    let c = o
        .controls
        .iter()
        .find(|c| c.control == Control::FullBoard)
        .unwrap();
    assert_eq!(l.right.unwrap().y as usize + c.line, at);
    // No lead, no board: no control.
    let m3 = Item::Milestone(MilestoneId(3));
    let l = layout(&view, &select(m3), a);
    assert!(
        l.overview
            .unwrap()
            .controls
            .iter()
            .all(|c| c.control != Control::FullBoard)
    );
}

#[test]
fn a_click_on_each_control_does_what_its_key_does() {
    let view: View = Ok(controls_fixture());
    let a = area(120, 40);
    // "open milestone ▶" is Enter: the milestone's detail.
    let mut by_click = select(Item::Milestone(MilestoneId(1)));
    click_control(&view, &mut by_click, a, Control::OpenMilestone);
    let by_key = opened(&view, Item::Milestone(MilestoneId(1)), a);
    assert_eq!(by_click, by_key);
    assert_eq!(by_click.view(), ViewKind::MilestoneDetail);
    // "◀ close" is Esc: back to the overview.
    click_control(&view, &mut by_click, a, Control::Close);
    assert_eq!((by_click.views(), by_click.view()), (1, ViewKind::Overview));
    // "open task ▶" is Enter on a task.
    let mut by_click = select(Item::Task(TaskId(13)));
    click_control(&view, &mut by_click, a, Control::OpenTask);
    assert_eq!(by_click, opened(&view, Item::Task(TaskId(13)), a));
    assert_eq!(by_click.view(), ViewKind::TaskDetail);
    // "open full board ▶" is b: the board on the selected task's thread.
    let mut by_click = select(Item::Task(TaskId(13)));
    click_control(&view, &mut by_click, a, Control::FullBoard);
    let mut by_key = select(Item::Task(TaskId(13)));
    by_key.handle(&view, a, Input::Char('b'));
    assert_eq!(by_click, by_key);
    assert_eq!(by_click.view(), ViewKind::Board);
    // A task opened from the milestone's detail: "◀ close" goes back to that detail.
    let mut dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    let l = layout(&view, &dash, a);
    let row = l.detail.as_ref().unwrap().rows[1];
    let r = l.right.unwrap();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: r.x + 4,
            y: r.y + row.line as u16,
        },
    );
    assert_eq!(dash.view(), ViewKind::TaskDetail);
    click_control(&view, &mut dash, a, Control::Close);
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // A click just left of a control does nothing.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let l = layout(&view, &dash, a);
    let c = l.overview.as_ref().unwrap().controls[0];
    let r = l.right.unwrap();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: r.x + c.from as u16 - 1,
            y: r.y,
        },
    );
    assert_eq!(dash, select(Item::Milestone(MilestoneId(1))));
}

#[test]
fn detail_views_have_close_except_a_standalone_task_shown_straight_away() {
    let view: View = Ok(controls_fixture());
    for item in [Item::Milestone(MilestoneId(1)), Item::Task(TaskId(13))] {
        for w in [120u16, 50] {
            let a = area(w, 40);
            let mut dash = select(item);
            dash.handle(&view, a, Input::Enter);
            if w == 50 {
                dash.handle(&view, a, Input::Enter);
            }
            let l = layout(&view, &dash, a);
            let controls = &l.detail.as_ref().unwrap().controls;
            assert_eq!(controls.len(), 1, "{item:?} w={w}");
            let all = rows(&render_dash(&view, &dash, w, 40, Depth::TrueColor));
            let first = &all[l.right.unwrap().y as usize];
            assert!(first.ends_with("◀ close (esc) "), "{first:?}");
        }
    }
    // Too narrow for the key: "◀ close" alone.
    let board = controls_fixture();
    let d = milestone_detail(&board, &board.milestones[0], 24, NOW, Depth::TrueColor);
    assert_eq!(d.lines[0].to_string(), " M1  First sho… ◀ close ");
    assert_eq!(d.layout.controls[0].control, Control::Close);
    // A standalone task's detail in no repo has no control, with repos or without.
    for s in [no_repos(standalone_fixture()), standalone_fixture()] {
        let s: View = Ok(s);
        let l = layout(&s, &select(Item::Task(TaskId(42))), area(120, 30));
        assert!(l.detail.as_ref().unwrap().controls.is_empty());
    }
}

#[test]
fn the_team_is_assignees_and_thread_authors_in_role_colours() {
    let mut board = controls_fixture();
    board.milestones[0].tasks[0].details.assignee = Some("dev-1".into());
    let m = &board.milestones[0];
    let d = milestone_detail(&board, m, 100, NOW, Depth::TrueColor);
    let text: Vec<String> = d.lines.iter().map(|l| l.to_string()).collect();
    let at = find_row(&text, " team ");
    // dev-1 assigned, then who posts, by thread (T2 first); cat-lead and manager do not count.
    assert_eq!(text[at].trim_end(), " team       dev-1, senior, dev-2, qa");
    let colour = |name: &str| {
        (d.lines[at].spans.iter())
            .find(|s| s.content == name)
            .unwrap_or_else(|| panic!("{name} not on {:?}", text[at]))
            .style
            .fg
    };
    assert_eq!(colour("dev-2"), Some(Color::Rgb(0x8f, 0xd6, 0xc4)));
    assert_eq!(colour("senior"), Some(Color::Rgb(0xc3, 0xa6, 0xff)));
    assert_eq!(colour("qa"), Some(Color::Rgb(0xf2, 0xa7, 0xc3)));
    // Wrapped between names in a narrow pane, every line exactly the width.
    let d = milestone_detail(&board, m, 30, NOW, Depth::TrueColor);
    let text: Vec<String> = d.lines.iter().map(|l| l.to_string()).collect();
    let at = find_row(&text, " team ");
    assert_eq!(text[at].trim_end(), " team       dev-1, senior,");
    assert_eq!(text[at + 1].trim_end(), "            dev-2, qa");
    assert!(d.lines.iter().all(|l| l.width() == 30));
    // The task detail shows the same team after the assignee.
    let t = &m.tasks[1];
    let d = task_detail(&board, Some(m), t, 100, NOW, Depth::TrueColor);
    let text: Vec<String> = d.lines.iter().map(|l| l.to_string()).collect();
    let at = find_row(&text, " assigned ");
    assert_eq!(
        text[at].trim_end(),
        " assigned   unassigned · team dev-1, senior, dev-2, qa"
    );
    // Nobody on it yet says so.
    let led = led_fixture();
    let lonely = &led.milestones[2];
    let d = milestone_detail(&led, lonely, 80, NOW, Depth::TrueColor);
    let text: Vec<String> = d.lines.iter().map(|l| l.to_string()).collect();
    assert!(text.iter().any(|l| l.starts_with(" team       nobody yet")));
}

/// What the dashboard's edge does with a write request: makes it through the store.
fn write(path: &std::path::Path, request: Request) -> (String, bool) {
    use control_tower::ui::writer::{Write, write_now};
    let w = match request {
        Request::Post { thread, text } => Write::Post { thread, text },
        Request::MarkRead { id, item } => Write::MarkRead { id, item },
        Request::Clear { id, item } => Write::Clear { id, item },
        Request::Fold {
            repos,
            milestones,
            done_groups,
            folded,
        } => Write::Fold {
            repos,
            milestones,
            done_groups,
            folded,
        },
        Request::StartLead { .. } => panic!("a start asks Rex: see the writer's tests"),
        Request::Quit => panic!("quitting writes nothing"),
        Request::Open(_) => panic!("opening a ticket writes nothing"),
    };
    write_now(path, &w)
}

#[test]
fn the_dashboard_posts_as_manager_through_the_store_and_a_failure_keeps_the_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    let m = store
        .add_milestone("First shop pages", "g", Some(r))
        .unwrap();
    store
        .add_lead(RepoOf::Milestone(m), "Catalogue", "cat-lead", None)
        .unwrap();
    let of = BoardOf::Milestone(m);
    store
        .open_thread(&of, "cat-lead", None, "t", "Open.")
        .unwrap();
    let a = area(120, 30);
    let view = refresh(&path);
    let mut dash = select(Item::Milestone(m));
    dash.handle(&view, a, Input::Char('b'));
    typed(&mut dash, &view, a, "-x approved");
    let request = dash.handle(&view, a, Input::Enter).unwrap();
    dash.posted(write(&path, request));
    assert_eq!(dash.draft(ThreadId(1)), "");
    let t = Store::open(&path).unwrap().thread(ThreadId(1)).unwrap();
    let last = t.messages.last().unwrap();
    assert_eq!(
        (last.author.as_str(), last.text.as_str()),
        ("manager", "-x approved")
    );
    // The thread is resolved between Enter and the write: refused, the draft stays.
    typed(&mut dash, &view, a, "and merged");
    let request = dash.handle(&view, a, Input::Enter).unwrap();
    store
        .resolve_thread(ThreadId(1), "cat-lead", "done")
        .unwrap();
    dash.posted(write(&path, request));
    assert_eq!(dash.draft(ThreadId(1)), "and merged");
    let all = rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor));
    assert!(all[29].contains("T1 is resolved"), "{}", all[29]);
}

#[test]
fn the_dashboard_marks_an_awareness_item_read_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    let m = store.add_milestone("M", "g", Some(r)).unwrap();
    store
        .add_lead(RepoOf::Milestone(m), "Catalogue", "cat-lead", None)
        .unwrap();
    let of = BoardOf::Milestone(m);
    let decision = "decision: the shelf shows 12 books, not 10. @manager for your awareness";
    store
        .open_thread(&of, "cat-lead", None, "t", decision)
        .unwrap();
    let a = area(120, 30);
    let view = refresh(&path);
    let mut dash = clocked();
    dash.handle(&view, a, Input::Char('i'));
    let request = dash.handle(&view, a, Input::Char('r')).unwrap();
    assert!(
        matches!(&request, Request::MarkRead { id: 1, .. }),
        "{request:?}"
    );
    // The repo is named after the temp board's folder.
    let (said, failed) = write(&path, request);
    assert!(
        !failed && said.starts_with("marked read: cat-lead in ‘t’ · "),
        "{said}"
    );
    assert!(!said.contains("message"), "{said}");
    let board = refresh(&path).unwrap();
    assert!(board.inbox.awareness.is_empty());
}

#[test]
fn n_opens_a_one_line_input_that_starts_a_lead_or_cancels() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    // On search, the repo without a lead.
    let mut dash = select(Item::Repo(RepoId(3)));
    assert_eq!(dash.handle(&view, a, Input::Char('n')), None);
    assert_eq!(dash.new_lead(), Some(""));
    assert!(dash.typing(&view, a) && !dash.writing(&view, a));
    // Empty, or only spaces: refused, saying so, and the input stays open.
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    assert!(
        dash.notice
            .as_ref()
            .is_some_and(|(t, failed)| *failed && t.contains("empty"))
    );
    dash.handle(&view, a, Input::Char(' '));
    assert_eq!(dash.handle(&view, a, Input::Enter), None);
    assert_eq!(dash.new_lead(), Some(" "));
    // Keys that do something elsewhere are text here; hidden characters are dropped.
    for c in "qbhi\u{1b}\u{200b}x".chars() {
        dash.handle(&view, a, Input::Char(c));
    }
    assert_eq!(dash.new_lead(), Some(" qbhix"));
    assert_eq!(dash.view(), ViewKind::Overview);
    dash.handle(&view, a, Input::Backspace);
    dash.paste(&view, a, "é\r\nRead aloud 📚");
    assert_eq!(dash.new_lead(), Some(" qbhié  Read aloud 📚"));
    dash.handle(&view, a, Input::Backspace);
    // Esc cancels: nothing is asked for and the next `n` starts empty.
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    assert_eq!(dash.new_lead(), None);
    assert!(!dash.typing(&view, a));
    dash.handle(&view, a, Input::Char('n'));
    assert_eq!(dash.new_lead(), Some(""));
    for c in " Bookmarks sync on every device ".chars() {
        dash.handle(&view, a, Input::Char(c));
    }
    assert_eq!(
        dash.handle(&view, a, Input::Enter),
        Some(Request::StartLead {
            line: "Bookmarks sync on every device".into(),
            repo: Some(RepoId(3))
        })
    );
    assert_eq!(dash.new_lead(), None);
    assert_eq!(dash.handing.as_deref(), Some("starting a lead…"));
    // No second start while one runs.
    assert_eq!(dash.handle(&view, a, Input::Char('n')), None);
    assert_eq!(dash.new_lead(), None);
}

#[test]
fn a_goal_stops_at_a_board_line_and_says_so() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = select(Item::Repo(RepoId(3)));
    dash.handle(&view, a, Input::Char('n'));
    dash.paste(&view, a, &"g".repeat(205));
    assert_eq!(dash.new_lead().map(|g| g.chars().count()), Some(200));
    assert!(
        dash.notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("at most 200"))
    );
}

#[test]
fn the_new_lead_control_on_the_alert_line_is_the_n_key() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let dash = clocked();
    let l = layout(&view, &dash, a);
    let at = l.new_lead.expect("the control shows at 120 columns");
    let row = &rows(&render_dash(&view, &dash, 120, 30, Depth::TrueColor))[0];
    let cells: String = row
        .chars()
        .skip(at.x as usize)
        .take(at.width as usize)
        .collect();
    assert_eq!(cells, "new lead (n) ▶");
    let mut dash = select(Item::Repo(RepoId(3)));
    dash.handle(&view, a, Input::Click { x: at.x, y: at.y });
    assert_eq!(dash.new_lead(), Some(""));
    assert_eq!(dash.view(), ViewKind::Overview, "not the inbox");
    // Beside it, the line still opens the inbox.
    let mut dash = clocked();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: at.x - 2,
            y: at.y,
        },
    );
    assert_eq!(dash.view(), ViewKind::Inbox);
    // Too narrow for it: gone, and `n` still works.
    let narrow = area(40, 24);
    assert_eq!(layout(&view, &clocked(), narrow).new_lead, None);
}

#[test]
fn failures_are_the_golden_red_at_both_depths() {
    // The signed design's #ff6b6b and its 256-colour red, never the terminal palette's red,
    // which a translucent theme can mute like it muted ANSI dark grey.
    for (depth, red) in [
        (Depth::TrueColor, Color::Rgb(0xff, 0x6b, 0x6b)),
        (Depth::Ansi256, Color::Indexed(203)),
    ] {
        assert_eq!(bad(depth), red);
        // A problem in the tree's place.
        let problem: View = Err("the board is locked".into());
        let buf = render_dash(&problem, &clocked(), 60, 10, depth);
        let all = rows(&buf);
        assert!(all[1].starts_with(" the board is locked"), "{all:?}");
        assert_eq!(buf[(1, 1)].fg, red, "problem {depth:?}");
        // A failed post, in the keys' place.
        let view: View = Ok(no_repos(standalone_fixture()));
        let mut dash = select(Item::Task(TaskId(41)));
        dash.notice = Some(("T3 is resolved".into(), true));
        let buf = render_dash(&view, &dash, 80, 20, depth);
        assert_eq!(rows(&buf)[19].trim_end(), " T3 is resolved");
        assert_eq!(buf[(1, 19)].fg, red, "notice {depth:?}");
        // A refused new lead, at the right of its input.
        let view: View = Ok(alerts_fixture());
        let a = area(120, 24);
        let mut dash = select(Item::Repo(RepoId(3)));
        dash.handle(&view, a, Input::Char('n'));
        dash.notice = Some(("give it a goal".into(), true));
        let buf = render_dash(&view, &dash, 120, 24, depth);
        let last = rows(&buf).last().unwrap().clone();
        let x = last
            .find("give it a goal")
            .unwrap_or_else(|| panic!("{last:?}"));
        let x = last[..x].chars().count() as u16;
        assert_eq!(buf[(x, 23)].fg, red, "new lead {depth:?}");
    }
}

#[test]
fn new_lead_input_snapshots() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 24);
    let mut dash = select(Item::Repo(RepoId(3)));
    dash.handle(&view, a, Input::Char('n'));
    for c in "Bookmarks sync on every device".chars() {
        dash.handle(&view, a, Input::Char(c));
    }
    let buf = render_dash(&view, &dash, 120, 24, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot("new_lead_120x24", &buf);
    // A goal longer than the line shows its end; refused, the reason in the keys' place.
    let narrow = area(40, 24);
    dash.handle(&view, narrow, Input::Esc);
    dash.handle(&view, narrow, Input::Char('n'));
    dash.paste(
        &view,
        narrow,
        "check the new author pages on a small phone tonight",
    );
    let buf = render_dash(&view, &dash, 40, 24, Depth::TrueColor);
    assert_exact_width(&buf);
    let last = rows(&buf).last().unwrap().clone();
    // The input says where the new lead starts.
    assert!(
        last.starts_with(" new lead on search, its goal: ") && last.ends_with("ght▏"),
        "{last:?}"
    );
    assert_snapshot("new_lead_40x24", &buf);
}

/// The board fixture as the v0.1.3 reference shows it: five threads on M1 (two past the
/// popup's three), a goal long enough to wrap and a task title past three card lines.
fn reference_fixture() -> Board {
    let mut board = board_fixture();
    let m = &mut board.milestones[0];
    m.goal = "A small shop's whole catalogue on one page that a reader can search, sort and \
              share with a friend, from a phone or a laptop, with the newest books first."
        .into();
    // #12 is done with nobody on it, finished by the lead.
    m.tasks[0].details.done_by = Some("cat-lead".into());
    m.tasks[2].title = "Let a reader save a basket for later and pick it up on another device, \
                        even after signing out"
        .into();
    for (id, title) in [(6, "Scope check"), (7, "Cover sizes for the shelf")] {
        m.threads.push(Thread::whole(
            ThreadId(id),
            title.into(),
            None,
            None,
            vec![Message {
                id: id * 100,
                author: "dev-2".into(),
                text: "Which cover size do we use?".into(),
                at: NOW - 50 * 60 - id,
            }],
        ));
    }
    sync(&mut board);
    board
}

fn popup_dash(item: Item) -> Dash {
    let mut dash = select(item);
    dash.mode = Mode::Popup;
    dash
}

#[test]
fn v013_snapshots_in_its_own_space_and_in_the_popup_at_four_widths() {
    let view: View = Ok(reference_fixture());
    for w in [60u16, 100, 160, 220] {
        for (name, dash) in [
            ("space", select(Item::Milestone(MilestoneId(1)))),
            ("popup", popup_dash(Item::Milestone(MilestoneId(1)))),
        ] {
            let buf = render_dash(&view, &dash, w, 50, Depth::TrueColor);
            assert_exact_width(&buf);
            assert_snapshot(&format!("v013_{name}_{w}x50"), &buf);
            let all = rows(&buf);
            // No header line of its own: the alert line is first and ends with the clock.
            assert!(
                all[0].starts_with(" ⚠ 1 need you"),
                "{w} {name}: {:?}",
                all[0]
            );
            assert!(all[0].ends_with(" 14:32 "), "{w} {name}: {:?}", all[0]);
            assert!(!all[0].contains("control-tower"));
            // Every line drawn matches the layout's height for the right pane.
            let l = layout(&view, &dash, area(w, 50));
            if let Some(o) = &l.overview {
                let lines = right_lines_of(&view, &dash, w);
                assert_eq!(o.lines, lines, "{w} {name}");
            }
        }
    }
}

/// The right pane's line count as drawn, from a tall frame.
fn right_lines_of(view: &View, dash: &Dash, w: u16) -> usize {
    let tall = 400;
    let l = layout(view, dash, area(w, tall));
    let r = l.right.unwrap();
    let all = rows(&render_dash(view, dash, w, tall, Depth::Ansi256));
    let col = |y: usize| -> String { all[y].chars().skip(usize::from(r.x)).collect() };
    (usize::from(r.y)..usize::from(r.y + r.height))
        .rev()
        .find(|y| !col(*y).trim().is_empty())
        .map_or(0, |y| y + 1 - usize::from(r.y))
}

#[test]
fn the_popup_board_shows_three_open_threads_with_their_last_bubbles_and_links_the_rest() {
    let view: View = Ok(reference_fixture());
    let a = area(160, 80);
    let mut dash = popup_dash(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    // Four open threads and one resolved: three open boxes, the link for the fourth, then
    // the folded RESOLVED row.
    let boxes = all.iter().filter(|r| r.contains("│ [open] "));
    assert_eq!(boxes.count(), 3, "{all:#?}");
    let more = find_row_containing(&all, "… 1 more open thread, open the full board ▶");
    assert!(find_row_containing(&all, "│ RESOLVED 1 ▸") > more);
    assert!(!all.iter().any(|r| r.contains("[resolved]")), "{all:#?}");
    assert!(
        all.iter().any(|r| r.contains("4 earlier messages")),
        "{all:#?}"
    );
    // In its own space: every thread, an open one's last three bubbles.
    let space = screen(&view, &select(Item::Milestone(MilestoneId(1))), a);
    let boxes = space.iter().filter(|r| r.contains("│ [open] "));
    assert_eq!(boxes.count(), 4, "{space:#?}");
    find_row_containing(&space, "│ RESOLVED 1 ▸");
    assert!(
        space.iter().any(|r| r.contains("3 earlier messages")),
        "{space:#?}"
    );
    assert!(!space.iter().any(|r| r.contains("more open")));
    // A click on the link opens the full board.
    let x = all[more].find('…').unwrap() as u16;
    dash.handle(
        &view,
        a,
        Input::Click {
            x: x + 3,
            y: more as u16,
        },
    );
    assert_eq!(dash.view(), ViewKind::Board);
}

fn find_row_containing(all: &[String], needle: &str) -> usize {
    all.iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}: {all:#?}"))
}

#[test]
fn in_the_popup_esc_at_the_top_closes_it_after_stepping_back_and_q_closes_it() {
    let view: View = Ok(reference_fixture());
    let a = area(120, 40);
    let mut dash = popup_dash(Item::Milestone(MilestoneId(1)));
    // The keys line ends "esc back · q close", in each view.
    let keys = |dash: &Dash| screen(&view, dash, a)[39].trim_end().to_string();
    assert!(
        keys(&dash).ends_with("esc back · q close"),
        "{}",
        keys(&dash)
    );
    // Esc steps back from what is open first.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
    assert!(
        keys(&dash).ends_with("esc back · q close"),
        "{}",
        keys(&dash)
    );
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    assert_eq!(dash.view(), ViewKind::Overview);
    dash.handle(&view, a, Input::Char('i'));
    assert!(
        keys(&dash).ends_with("esc back · q close"),
        "{}",
        keys(&dash)
    );
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    // On the board's open thread q is typed, so "q close" is not offered.
    dash.handle(&view, a, Input::Char('b'));
    assert!(keys(&dash).contains("· esc back ·"), "{}", keys(&dash));
    assert!(!keys(&dash).contains("q close"), "{}", keys(&dash));
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    // With nothing to step back from, Esc closes the popup.
    assert_eq!(dash.handle(&view, a, Input::Esc), Some(Request::Quit));
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
    // While writing, q is typed, not closing; Esc, then q, closes.
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.handle(&view, a, Input::Char('q')), None);
    assert_eq!(dash.draft(ThreadId(3)), "q");
    assert_eq!(dash.handle(&view, a, Input::Esc), None);
    assert_eq!(dash.handle(&view, a, Input::Char('q')), Some(Request::Quit));
    // In its own space Esc at the top does nothing, q still quits, and the keys say quit.
    let mut own = select(Item::Milestone(MilestoneId(1)));
    assert_eq!(own.handle(&view, a, Input::Esc), None);
    assert_eq!(own.handle(&view, a, Input::Char('q')), Some(Request::Quit));
    assert!(keys(&own).ends_with("q quit"), "{}", keys(&own));
}

#[test]
fn the_mode_comes_from_tower_ui_mode() {
    assert_eq!(Mode::from_env(None).unwrap(), Mode::Space);
    assert_eq!(Mode::from_env(Some("")).unwrap(), Mode::Space);
    assert_eq!(Mode::from_env(Some("space")).unwrap(), Mode::Space);
    assert_eq!(Mode::from_env(Some("popup")).unwrap(), Mode::Popup);
    let e = Mode::from_env(Some("Popup")).unwrap_err().to_string();
    assert!(e.contains("TOWER_UI_MODE is \"Popup\""), "{e}");
}

#[test]
fn cards_are_one_line_and_always_show_the_assignee() {
    let board = reference_fixture();
    let m = &board.milestones[0];
    for w in [60usize, 100, 160, 220] {
        let o = overview(m, None, w, Depth::Ansi256);
        let text: Vec<String> = o.lines.iter().map(|l| l.to_string()).collect();
        for c in &o.cards {
            let t = m.tasks.iter().find(|t| t.id == c.task).unwrap();
            // One line, its assignee on it (cut with "…" only when the column is narrow).
            assert_eq!(c.height, 1);
            let who = t.who(m.lead.as_ref());
            let line = &text[c.line];
            // The whole assignee where its column has room for it, else its start.
            let own: String = line.chars().skip(c.x).take(c.width).collect();
            let room = c.width.saturating_sub(c.task.to_string().len() + 2);
            if who.chars().count() <= room {
                assert!(own.contains(who), "w={w} {who}: {own:?}");
            } else {
                let start: String = who.chars().take(room.saturating_sub(1)).collect();
                assert!(own.contains(&start), "w={w} {who}: {own:?}");
            }
        }
        // The goal wraps to at most two lines, in the readable goal grey, not dim.
        let goal = (o.lines[1].spans.iter())
            .find(|s| s.content.starts_with("A small shop"))
            .unwrap();
        assert_eq!(goal.style.fg, Some(Color::Indexed(248)), "w={w}");
        assert!(text[1].starts_with(" goal "));
        let progress = find_row(&text, " [in progress]");
        assert!(progress == 2 || progress == 3, "w={w}");
        if progress == 3 {
            assert!(text[2].starts_with("      "), "w={w}: {}", text[2]);
        }
    }
}

#[test]
fn a_clamped_goal_ends_after_its_last_whole_piece() {
    // As the signed design's (UI v0.1.13) `.goal clamp2` ends it in Chrome (#50): after
    // "shed-lead-senior-", the last piece that fits, not cut inside "reviewer".
    let mut board = reference_fixture();
    board.milestones[0].goal = "the long garden-shed-lead-senior-reviewer keeps it".into();
    let o = overview(&board.milestones[0], None, 24, Depth::Ansi256);
    let text: Vec<String> = o.lines.iter().map(|l| l.to_string()).collect();
    assert_eq!(text[1], " goal the long garden-  ");
    assert_eq!(text[2], "      shed-lead-senior-…");
}

#[test]
fn a_done_card_with_no_assignee_says_done_by_lead_only_when_the_lead_finished_it() {
    let board = reference_fixture();
    let m = &board.milestones[0];
    let lead = m.lead.as_ref();
    let mut t = m.tasks[0].clone();
    assert_eq!(t.stage, Stage::Done);
    assert_eq!(t.who(lead), "done by lead");
    let o = overview(m, None, 120, Depth::Ansi256);
    let c = o.cards.iter().find(|c| c.task == t.id).unwrap();
    let card: String = o.lines[c.line..c.line + c.height]
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(card.contains("done by lead"), "{card}");
    // By the lead's name too, in any case.
    t.details.done_by = Some("CATALOGUE LEAD".into());
    assert_eq!(t.who(lead), "done by lead");
    // Someone else, nobody named, or no lead: unassigned.
    for by in [Some("dev-1"), None] {
        t.details.done_by = by.map(String::from);
        assert_eq!(t.who(lead), "unassigned", "{by:?}");
    }
    t.details.done_by = Some("cat-lead".into());
    assert_eq!(t.who(None), "unassigned");
    // Not done: unassigned; an assignee always wins.
    t.stage = Stage::InProgress;
    assert_eq!(t.who(lead), "unassigned");
    t.details.assignee = Some("dev-2".into());
    t.stage = Stage::Done;
    assert_eq!(t.who(lead), "dev-2");
}

#[test]
fn the_popup_tree_is_narrower_than_in_its_own_space() {
    let view: View = Ok(reference_fixture());
    // At 60 columns its own space has the tree alone; the popup fits the overview beside it.
    for (w, space, popup) in [(100u16, 34, 27), (160, 54, 43), (60, 60, 24)] {
        let tree = |dash: &Dash| layout(&view, dash, area(w, 50)).tree.map(|t| t.width);
        assert_eq!(
            tree(&select(Item::Milestone(MilestoneId(1)))),
            Some(space),
            "{w}"
        );
        assert_eq!(
            tree(&popup_dash(Item::Milestone(MilestoneId(1)))),
            Some(popup),
            "{w}"
        );
    }
}

// Folding a milestone's task list in the tree (UI v0.1.4).

/// `board` with milestones `ids` folded.
fn folded(mut board: Board, ids: &[i64]) -> Board {
    for m in &mut board.milestones {
        m.folded = ids.contains(&m.id.0);
    }
    board
}

fn fold(ids: &[i64], folded: bool) -> Option<Request> {
    Some(Request::Fold {
        repos: Vec::new(),
        milestones: ids.iter().map(|i| MilestoneId(*i)).collect(),
        done_groups: Vec::new(),
        folded,
    })
}

#[test]
fn folded_tree_snapshots_at_the_tree_and_v013_sizes() {
    let view: View = Ok(folded(blocked_fixture(), &[1, 3]));
    for (w, h) in [(80, 20), (34, 20)] {
        let buf = render(&view, w, h);
        assert_exact_width(&buf);
        assert_snapshot(&format!("folded_{w}x{h}"), &buf);
    }
    let view: View = Ok(folded(reference_fixture(), &[1]));
    for w in [60u16, 100, 160, 220] {
        for (name, dash) in [
            ("space", select(Item::Milestone(MilestoneId(1)))),
            ("popup", popup_dash(Item::Milestone(MilestoneId(1)))),
        ] {
            let buf = render_dash(&view, &dash, w, 50, Depth::TrueColor);
            assert_exact_width(&buf);
            assert_snapshot(&format!("folded_{name}_{w}x50"), &buf);
        }
    }
}

#[test]
fn a_folded_milestone_shows_one_summary_line_with_its_counts_and_blocked() {
    let text = |board: &Board| -> Vec<String> {
        (tree_lines(board, 40, Depth::TrueColor).iter())
            .map(|l| l.to_string().trim_end().to_string())
            .collect()
    };
    let unfolded = text(&blocked_fixture());
    assert!(
        unfolded
            .iter()
            .any(|l| l.starts_with("   ▾ M1 First shop pages"))
    );
    assert!(unfolded.iter().any(|l| l.starts_with("     ✓ Bookmarks")));
    let board = folded(blocked_fixture(), &[1, 2, 3]);
    let all = text(&board);
    let m1 = find_row(&all, "   ▸ M1 First shop pages");
    // The repo row keeps its markers, the milestone line its bar, and one line replaces
    // the tasks: zero counts left out, the blocked count in bold amber.
    assert!(all[m1 - 2].contains("1 blocked"), "{all:#?}");
    assert!(all[m1].ends_with("33%"), "{all:#?}");
    assert_eq!(all[m1 + 1], "     3 tasks ✓1 ●1 ○1 !1");
    assert!(!all.iter().any(|l| l.contains("#12")), "{all:#?}");
    let m2 = find_row(&all, "   ▸ M2 ");
    assert_eq!(all[m2 + 2], "     2 tasks ✓2");
    let m3 = find_row(&all, "   ▸ M3 ");
    assert_eq!(all[m3 + 1], "     0 tasks");
    let lines = tree_lines(&board, 40, Depth::TrueColor);
    let bang = (lines[m1 + 1].spans.iter())
        .find(|s| s.content == " !1")
        .unwrap();
    assert_eq!(bang.style.fg, Some(warn(Depth::TrueColor)));
    assert!(bang.style.add_modifier.contains(Modifier::BOLD));
    // One task is "1 task"; the summary line selects nothing.
    let mut one = folded(led_fixture(), &[1]);
    one.milestones[0].tasks.truncate(1);
    assert!(text(&one).contains(&"     1 task ✓1".to_string()));
    let items = tree_items(&board, 40);
    assert_eq!(items[m1 + 1], None);
    assert_eq!(items[m1], Some(Item::Milestone(MilestoneId(1))));
}

#[test]
fn left_and_right_each_toggle_the_fold_of_the_selected_milestone_or_the_selected_tasks() {
    let a = area(120, 30);
    for key in [Input::Left, Input::Right] {
        let view: View = Ok(led_fixture());
        // On a task: its milestone is selected, the task's detail closes, the milestone folds.
        let mut dash = opened(&view, Item::Task(TaskId(13)), a);
        assert_eq!(dash.view(), ViewKind::TaskDetail);
        assert_eq!(dash.handle(&view, a, key), fold(&[1], true), "{key:?}");
        assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
        assert_eq!(dash.view(), ViewKind::Overview);
        // On a milestone: it folds, the milestone detail stays.
        let mut dash = opened(&view, Item::Milestone(MilestoneId(2)), a);
        assert_eq!(dash.handle(&view, a, key), fold(&[2], true), "{key:?}");
        assert_eq!(dash.view(), ViewKind::MilestoneDetail);
        // On a folded milestone: it unfolds.
        let view: View = Ok(folded(standalone_fixture(), &[1]));
        let mut dash = select(Item::Milestone(MilestoneId(1)));
        assert_eq!(dash.handle(&view, a, key), fold(&[1], false), "{key:?}");
        assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
        // A standalone task has no milestone: nothing to write.
        let standalone = view.as_ref().unwrap().standalone[0].id;
        let mut dash = select(Item::Task(standalone));
        assert_eq!(dash.handle(&view, a, key), None, "{key:?}");
        assert_eq!(dash.selected, Some(Item::Task(standalone)));
    }
}

#[test]
fn z_folds_every_unfolded_milestone_then_unfolds_them_all() {
    let a = area(120, 30);
    let view: View = Ok(folded(led_fixture(), &[2]));
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(dash.handle(&view, a, Input::Char('z')), fold(&[1, 3], true));
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    let view: View = Ok(folded(led_fixture(), &[1, 2, 3]));
    assert_eq!(
        dash.handle(&view, a, Input::Char('z')),
        fold(&[1, 2, 3], false)
    );
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // An empty board has nothing to fold.
    let empty: View = Ok(Board {
        repos: Vec::new(),
        milestones: Vec::new(),
        standalone: Vec::new(),
        inbox: Default::default(),
    });
    assert_eq!(clocked().handle(&empty, a, Input::Char('z')), None);
}

#[test]
fn fold_keys_do_nothing_while_the_tree_does_not_show() {
    let view: View = Ok(led_fixture());
    // Narrow: Enter opens the overview across the pane, over the tree.
    let a = area(40, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Enter);
    assert!(layout(&view, &dash, a).tree.is_none());
    for key in [Input::Left, Input::Right, Input::Char('z')] {
        assert_eq!(dash.handle(&view, a, key), None, "{key:?}");
    }
    let keys = rows(&render_dash(&view, &dash, 40, 30, Depth::TrueColor))[29].clone();
    assert!(!keys.contains("fold"), "{keys}");
}

#[test]
fn up_and_down_skip_folded_tasks_and_the_first_repo_is_the_default() {
    let view: View = Ok(folded(led_fixture(), &[1]));
    let a = area(120, 30);
    // Nothing selected yet: the first repo row.
    assert_eq!(
        clocked().selection(view.as_ref().unwrap()),
        Some(Item::Repo(RepoId(1)))
    );
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(2))));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(2))));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(21))));
    let mut dash = select(Item::Repo(RepoId(2)));
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // Every milestone folded: the first milestone.
    let all: View = Ok(folded(led_fixture(), &[1, 2, 3]));
    assert_eq!(
        clocked().selection(all.as_ref().unwrap()),
        Some(Item::Repo(RepoId(1)))
    );
}

#[test]
fn a_refresh_folding_the_selected_tasks_milestone_selects_the_milestone() {
    let view: View = Ok(led_fixture());
    let a = area(120, 30);
    let mut dash = opened(&view, Item::Task(TaskId(13)), a);
    // The other dashboard folds M1.
    let after: View = Ok(folded(led_fixture(), &[1]));
    dash.refreshed(&after, a);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    assert_eq!(dash.view(), ViewKind::Overview);
    // A fold elsewhere leaves the selection alone.
    let mut dash = select(Item::Task(TaskId(13)));
    dash.refreshed(&Ok(folded(led_fixture(), &[2])), a);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    // While its unfold runs, the picked task stays selected and shows as its milestone.
    let mut dash = select(Item::Task(TaskId(13)));
    dash.unfolding = Some(Fold::Milestone(MilestoneId(1)));
    dash.refreshed(&after, a);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    assert_eq!(
        dash.tree_selection(after.as_ref().unwrap()),
        Some(Item::Milestone(MilestoneId(1)))
    );
}

#[test]
fn picking_a_folded_task_by_its_card_unfolds_its_milestone() {
    let view: View = Ok(folded(led_fixture(), &[1]));
    let a = area(120, 30);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let l = layout(&view, &dash, a);
    let r = right(&l);
    let card = r
        .overview
        .cards
        .iter()
        .find(|c| c.task == TaskId(13))
        .unwrap();
    let (x, y) = (r.area.x + card.x as u16 + 1, r.area.y + card.line as u16);
    let asked = dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(asked, fold(&[1], false));
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    assert_eq!(dash.unfolding, Some(Fold::Milestone(MilestoneId(1))));
    // The selected task's milestone rows are highlighted meanwhile.
    let buf = render_dash(&view, &dash, 120, 30, Depth::TrueColor);
    let m1 = find_row(&rows(&buf), "   ▸ M1") as u16;
    assert_eq!(buf[(5, m1)].bg, selected_bg(Depth::TrueColor));
}

#[test]
fn a_click_on_the_mark_toggles_and_on_the_summary_line_unfolds() {
    let a = area(120, 30);
    let view: View = Ok(folded(led_fixture(), &[2]));
    let all = rows(&render(&view, 120, 30));
    let m1 = find_row(&all, "   ▾ M1") as u16;
    let m2 = find_row(&all, "   ▸ M2") as u16;
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(
        dash.handle(&view, a, Input::Click { x: 3, y: m1 }),
        fold(&[1], true)
    );
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    assert_eq!(
        dash.handle(&view, a, Input::Click { x: 4, y: m2 }),
        fold(&[2], false)
    );
    let summary = find_row(&all, "     2 tasks") as u16;
    assert_eq!(
        dash.handle(&view, a, Input::Click { x: 20, y: summary }),
        fold(&[2], false)
    );
    // Past the mark, a click selects as before.
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(dash.handle(&view, a, Input::Click { x: 5, y: m2 }), None);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(2))));
    // The second line of a wrapped title has no mark.
    let narrow: View = Ok(led_fixture());
    let n = area(60, 30);
    let all = rows(&render(&narrow, 60, 30));
    let m2 = find_row(&all, "   ▾ M2") as u16;
    assert!(all[usize::from(m2) + 1].starts_with("     "), "{all:#?}");
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(
        dash.handle(&narrow, n, Input::Click { x: 1, y: m2 + 1 }),
        None
    );
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(2))));
}

#[test]
fn folds_are_written_through_the_store_and_kept_on_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let m1 = store.add_milestone("One", "g", None).unwrap();
    let m2 = store.add_milestone("Two", "g", None).unwrap();
    store.add_task(m1, "t").unwrap();
    let a = area(120, 30);
    let view = refresh(&path);
    assert!(view.as_ref().unwrap().milestones.iter().all(|m| !m.folded));
    let mut dash = clocked();
    let asked = dash.handle(&view, a, Input::Char('z')).unwrap();
    assert_eq!(write(&path, asked), (format!("folded {m1}, {m2}"), false));
    // A dashboard started now shows the same folds.
    let view = refresh(&path);
    let all = rows(&render(&view, 120, 30));
    assert!(all.iter().any(|l| l.contains("▸ M1 One")), "{all:#?}");
    assert!(
        all.iter().any(|l| l.starts_with("     1 task ○1")),
        "{all:#?}"
    );
    let asked = dash.handle(&view, a, Input::Char('z')).unwrap();
    assert!(!write(&path, asked).1);
    assert!(refresh(&path).unwrap().milestones.iter().all(|m| !m.folded));
}

#[test]
fn a_word_the_first_line_breaks_stays_whole_on_the_second() {
    let mut board = led_fixture();
    board.milestones[2].title = "Search by author  now".into();
    let lines: Vec<String> = (tree_lines(&board, 24, Depth::Ansi256).iter())
        .map(|l| l.to_string().trim_end().to_string())
        .collect();
    let m3 = find_row(&lines, "   ▾ M3");
    assert_eq!(lines[m3], "   ▾ M3   ░░░░░░░░   0%", "{lines:#?}");
    assert_eq!(lines[m3 + 1], "     Search by author…", "{lines:#?}");
}

#[test]
fn a_picked_folded_task_stays_selected_until_a_refresh_shows_it_unfolded() {
    let a = area(120, 30);
    let before: View = Ok(folded(led_fixture(), &[1]));
    let after: View = Ok(led_fixture());
    let pick = || {
        let mut dash = select(Item::Milestone(MilestoneId(1)));
        let l = layout(&before, &dash, a);
        let r = right(&l);
        let card = (r.overview.cards.iter())
            .find(|c| c.task == TaskId(13))
            .unwrap();
        let (x, y) = (r.area.x + card.x as u16 + 1, r.area.y + card.line as u16);
        assert_eq!(
            dash.handle(&before, a, Input::Click { x, y }),
            fold(&[1], false)
        );
        dash
    };
    let mut dash = pick();
    // Refreshes read before the unfold landed, while it runs and after it finished: the
    // task stays selected.
    for _ in 0..2 {
        dash.refreshed(&before, a);
        assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
        assert_eq!(dash.unfolding, Some(Fold::Milestone(MilestoneId(1))));
    }
    // The refresh that shows it unfolded ends the unfold; a later fold (the other
    // dashboard) selects the milestone again.
    dash.refreshed(&after, a);
    assert_eq!(dash.unfolding, None);
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    dash.refreshed(&before, a);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // A refused unfold (the edge clears `unfolding`): the next refresh selects the milestone.
    let mut dash = pick();
    dash.unfolding = None;
    dash.refreshed(&before, a);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
}

#[test]
fn picking_a_folded_task_by_a_milestone_detail_row_unfolds_it_and_opens_its_detail() {
    let view: View = Ok(folded(detail_fixture(), &[1]));
    let a = area(120, 40);
    let mut dash = opened(&view, Item::Milestone(MilestoneId(1)), a);
    let l = layout(&view, &dash, a);
    let row = l.detail.as_ref().unwrap().rows[1];
    let (x, y) = right_cell(&l, &dash, row.line);
    assert_eq!(
        dash.handle(&view, a, Input::Click { x, y }),
        fold(&[1], false)
    );
    assert_eq!((dash.views(), dash.view()), (3, ViewKind::TaskDetail));
    assert_eq!(dash.selected, Some(Item::Task(TaskId(13))));
    assert_eq!(dash.unfolding, Some(Fold::Milestone(MilestoneId(1))));
    // A refresh still showing it folded leaves the task's detail open.
    dash.refreshed(&view, a);
    assert_eq!(dash.view(), ViewKind::TaskDetail);
}

#[test]
fn a_fold_scrolls_the_selection_into_a_scrolled_tall_tree() {
    let tall = |folds: &[i64]| -> View {
        let mut board = led_fixture();
        for i in 0..30 {
            board.milestones[0].tasks.push(Task {
                id: TaskId(100 + i),
                title: format!("task {i}"),
                stage: Stage::Waiting,
                repo: None,
                subtasks: Vec::new(),
                details: TaskDetails::default(),
            });
        }
        Ok(folded(board, folds))
    };
    let a = area(120, 12);
    let shows = |view: &View, dash: &Dash, item: Item| {
        let l = layout(view, dash, a);
        let t = l.tree.unwrap();
        let scroll = dash.tree_scroll;
        (l.tree_items.iter().enumerate())
            .any(|(i, it)| *it == Some(item) && i >= scroll && i < scroll + usize::from(t.height))
    };
    // Left far down M1's tasks: M1, high above, comes into view, and stays there once the
    // fold lands.
    let view = tall(&[]);
    let mut dash = select(Item::Task(TaskId(129)));
    dash.refreshed(&view, a);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Up);
    assert!(shows(&view, &dash, Item::Task(TaskId(129))));
    assert_eq!(dash.handle(&view, a, Input::Left), fold(&[1], true));
    dash.fit(&view, a);
    assert!(shows(&view, &dash, Item::Milestone(MilestoneId(1))));
    let after = tall(&[1]);
    dash.refreshed(&after, a);
    dash.fit(&after, a);
    assert!(shows(&after, &dash, Item::Milestone(MilestoneId(1))));
    // M2 selected below M1's tasks; the other dashboard folds M1, and M2 moves up: it is
    // scrolled back into view.
    let mut dash = select(Item::Milestone(MilestoneId(2)));
    dash.refreshed(&view, a);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Up);
    assert!(shows(&view, &dash, Item::Milestone(MilestoneId(2))));
    dash.refreshed(&after, a);
    dash.fit(&after, a);
    assert!(shows(&after, &dash, Item::Milestone(MilestoneId(2))));
    // A refresh with the same folds leaves a tree the owner scrolled where it is.
    dash.tree_scroll = 0;
    dash.refreshed(&after, a);
    dash.fit(&after, a);
    assert_eq!(dash.tree_scroll, 0);
}

// UI v0.1.7: repo rows, their folds, their views and starting a lead on one.

/// The board fixture with a repo-wide thread on catalogue's board (T8, about
/// neither a milestone nor a task) and a forge link on the repo.
fn repo_board_fixture() -> Board {
    let mut board = board_fixture();
    board.repos[0].link = Some("https://github.com/example/catalogue".into());
    board.repos[0].threads.push(Thread::whole(
        ThreadId(8),
        "Triage of the new issues".into(),
        None,
        None,
        vec![Message {
            id: 800,
            author: "cat-lead".into(),
            text: "Triage: two bugs labelled ready-for-agent.".into(),
            at: NOW - 3 * 60,
        }],
    ));
    for m in &mut board.milestones {
        if m.repo == Some(RepoId(1)) {
            m.threads = board.repos[0].threads.clone();
        }
    }
    board
}

#[test]
fn a_milestone_shows_its_part_of_the_board_and_the_repo_its_whole_board() {
    let view: View = Ok(repo_board_fixture());
    // Tall enough for each board's last row, its RESOLVED row.
    let a = area(140, 120);
    let text = |item| screen(&view, &select(item), a).join("\n");
    let repo = text(Item::Repo(RepoId(1)));
    assert!(repo.contains("Triage of the new issues"), "{repo}");
    // Each heading says which part of the board it is (UI v0.1.7).
    assert!(
        repo.contains(" MESSAGE BOARD · whole repo Catalogue Lead"),
        "{repo}"
    );
    assert!(
        text(Item::Milestone(MilestoneId(1)))
            .contains(" MESSAGE BOARD · M1 and its tasks Catalogue Lead"),
    );
    assert!(text(Item::Task(TaskId(14))).contains(" MESSAGE BOARD · #14 Catalogue Lead"));
    // Resolved T2, on M1, is on the repo's board and M1's, folded into its RESOLVED row
    //; a task's board has no resolved thread.
    assert!(repo.contains("│ RESOLVED 1 ▸"), "{repo}");
    let m1 = text(Item::Milestone(MilestoneId(1)));
    assert!(!m1.contains("Triage of the new issues"), "{m1}");
    assert!(
        m1.contains("│ RESOLVED 1 ▸") && m1.contains("Show each book's reviews"),
        "{m1}"
    );
    // A task's: its own thread only.
    let t14 = text(Item::Task(TaskId(14)));
    assert!(
        t14.contains("Show each book's reviews") && !t14.contains("RESOLVED"),
        "{t14}"
    );
    // The full board is the whole repo's, from a milestone too.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("○ Triage of the new issues"), "{all}");
    // What T8 is on is in its header: "repo-wide".
    for _ in 0..4 {
        if dash.board_thread(&view) == Some(ThreadId(8)) {
            break;
        }
        dash.handle(&view, a, Input::Down);
    }
    assert_eq!(dash.board_thread(&view), Some(ThreadId(8)));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains(" repo-wide · started by cat-lead"), "{all}");
}

#[test]
fn repo_overview_and_detail_snapshots() {
    let view: View = Ok(repo_board_fixture());
    for (w, h) in [(140u16, 50u16), (72, 50)] {
        let a = area(w, h);
        let dash = select(Item::Repo(RepoId(1)));
        let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
        assert_exact_width(&buf);
        assert_snapshot(&format!("repo_overview_{w}x{h}"), &buf);
        let dash = opened(&view, Item::Repo(RepoId(1)), a);
        assert_eq!(dash.view(), ViewKind::RepoDetail);
        let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
        assert_exact_width(&buf);
        assert_snapshot(&format!("repo_detail_{w}x{h}"), &buf);
    }
}

#[test]
fn the_repo_overview_lists_its_milestones_and_a_click_on_one_selects_it() {
    let view: View = Ok(repo_standalone_fixture());
    let a = area(140, 40);
    let mut dash = select(Item::Repo(RepoId(1)));
    let all = screen(&view, &dash, a);
    let r = right(&layout(&view, &dash, a));
    assert!(all[1].contains("catalogue · Catalogue Lead"), "{}", all[1]);
    assert!(all[1].contains("open repo (enter) ▶"), "{}", all[1]);
    assert!(all[2].contains("checkout /src/catalogue"), "{}", all[2]);
    let y = find_row_containing(&all, "● M1 First shop pages") as u16;
    assert!(
        all[usize::from(y) + 1].contains("standalone 3 ✓1 ●1 ○1"),
        "{all:#?}"
    );
    dash.handle(&view, a, Input::Click { x: r.area.x + 4, y });
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    // Enter on the repo opens its detail; Esc comes back.
    let mut dash = select(Item::Repo(RepoId(1)));
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::RepoDetail);
    let all = screen(&view, &dash, a).join("\n");
    for want in [
        "lead       Catalogue Lead",
        "checkout   /src/catalogue",
        "work       1 active milestone · 3 standalone tasks · 0 Done milestones",
        " MILESTONES",
        " STANDALONE TASKS 1/3",
        " MESSAGE BOARD",
    ] {
        assert!(all.contains(want), "{want:?} missing from\n{all}");
    }
    // A standalone task's row opens its detail, which names the repo and its lead.
    let l = layout(&view, &dash, a);
    let d = l.detail.clone().unwrap();
    let row = d.rows.iter().find(|r| r.task == TaskId(42)).unwrap();
    let (x, y) = right_cell(&l, &dash, row.line);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(dash.selected, Some(Item::Task(TaskId(42))));
    let all = screen(&view, &dash, a).join("\n");
    assert!(
        all.contains("milestone  standalone, no milestone · catalogue · Catalogue Lead"),
        "{all}"
    );
    assert!(!all.contains("hand to a lead"), "{all}");
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::RepoDetail);
}

#[test]
fn a_repo_folds_to_its_row_and_one_summary_line() {
    let a = area(120, 30);
    let view: View = Ok(repo_standalone_fixture());
    // Left on a repo row folds it; on a standalone task, its repo.
    for item in [Item::Repo(RepoId(1)), Item::Task(TaskId(42))] {
        let mut dash = select(item);
        assert_eq!(
            dash.handle(&view, a, Input::Left),
            Some(Request::Fold {
                repos: vec![RepoId(1)],
                milestones: Vec::new(),
                done_groups: Vec::new(),
                folded: true
            }),
            "{item:?}"
        );
        assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
    }
    let mut board = repo_standalone_fixture();
    board.repos[0].folded = true;
    board.milestones[0].tasks[2].details.blocked = Some("r".into());
    let lines: Vec<String> = (tree_lines(&board, 50, Depth::Ansi256).iter())
        .map(|l| l.to_string().trim_end().to_string())
        .collect();
    assert_eq!(
        lines[0].split_whitespace().take(2).collect::<Vec<_>>(),
        ["▸", "catalogue"]
    );
    assert!(lines[0].ends_with("1 blocked"), "{lines:#?}");
    assert_eq!(lines[1], "   Catalogue Lead");
    assert_eq!(lines[2], "   1 milestone · 3 standalone !1");
    assert!(lines[3].starts_with('─'), "{lines:#?}");
    // Up and down skip the summary line and what it hides; a click on it unfolds.
    let view: View = Ok(board.clone());
    let mut dash = select(Item::Repo(RepoId(1)));
    dash.handle(&view, a, Input::Down);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(2))));
    let all = rows(&render(&view, 120, 30));
    let y = find_row(&all, "   1 milestone") as u16;
    assert_eq!(
        dash.handle(&view, a, Input::Click { x: 8, y }),
        Some(Request::Fold {
            repos: vec![RepoId(1)],
            milestones: Vec::new(),
            done_groups: Vec::new(),
            folded: false
        })
    );
    // z folds milestones only; the repo keeps its fold.
    let mut dash = select(Item::Repo(RepoId(2)));
    match dash.handle(&view, a, Input::Char('z')) {
        Some(Request::Fold { repos, .. }) => assert!(repos.is_empty()),
        other => panic!("{other:?}"),
    }
    // A refresh that folds the selected task's repo selects the repo.
    let mut dash = select(Item::Task(TaskId(13)));
    dash.refreshed(&view, a);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
}

#[test]
fn a_repo_fold_is_written_through_the_store_and_kept_on_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    store.add_milestone("One", "g", Some(r)).unwrap();
    let a = area(120, 30);
    let view = refresh(&path);
    let mut dash = select(Item::Repo(r));
    let asked = dash.handle(&view, a, Input::Left).unwrap();
    assert_eq!(write(&path, asked), (format!("folded {r}"), false));
    let view = refresh(&path);
    assert!(view.as_ref().unwrap().repos[0].folded);
    let all = rows(&render(&view, 120, 30));
    assert!(
        all.iter().any(|l| l.starts_with("   1 milestone")),
        "{all:#?}"
    );
    let asked = dash.handle(&view, a, Input::Right).unwrap();
    assert!(!write(&path, asked).1);
    assert!(!refresh(&path).unwrap().repos[0].folded);
}

#[test]
fn n_starts_the_lead_on_the_selections_repo_and_refuses_a_led_one() {
    let a = area(120, 24);
    let view: View = Ok(led_fixture());
    let start = |view: &View, item| {
        let mut dash = select(item);
        dash.handle(view, a, Input::Char('n'));
        for c in "Comics".chars() {
            dash.handle(view, a, Input::Char(c));
        }
        let repo = dash.new_lead_repo();
        (repo, dash.handle(view, a, Input::Enter))
    };
    // search (R3) has no lead: M3, or the repo itself, starts it there.
    for item in [Item::Milestone(MilestoneId(3)), Item::Repo(RepoId(3))] {
        assert_eq!(
            start(&view, item),
            (
                Some(RepoId(3)),
                Some(Request::StartLead {
                    line: "Comics".into(),
                    repo: Some(RepoId(3))
                })
            ),
            "{item:?}"
        );
    }
    // The input names the repo it starts on.
    let mut dash = select(Item::Repo(RepoId(3)));
    dash.handle(&view, a, Input::Char('n'));
    let last = rows(&render_dash(&view, &dash, 120, 24, Depth::TrueColor))[23].clone();
    assert!(
        last.starts_with(" new lead on search, its goal: ▏"),
        "{last:?}"
    );
    // A repo whose main checkout is not on the board: refused, saying how to set it.
    let mut board = led_fixture();
    board.repos[2].path = None;
    let no_path: View = Ok(board);
    let mut dash = select(Item::Repo(RepoId(3)));
    assert_eq!(dash.handle(&no_path, a, Input::Char('n')), None);
    assert_eq!(dash.new_lead(), None);
    assert_eq!(
        dash.notice,
        Some((
            "R3 has no main checkout on the board: tower repo set R3 --path <checkout>, then n"
                .into(),
            true
        ))
    );
    // catalogue has its lead: n is refused, naming the repo and its lead, and no
    // input opens.
    let mut dash = select(Item::Task(TaskId(13)));
    assert_eq!(dash.handle(&view, a, Input::Char('n')), None);
    assert_eq!(dash.new_lead(), None);
    let (why, failed) = dash.notice.clone().unwrap();
    assert!(failed);
    assert!(
        why.starts_with("catalogue has its lead, Catalogue Lead"),
        "{why}"
    );
    let last = rows(&render_dash(&view, &dash, 120, 24, Depth::TrueColor))[23].clone();
    assert!(last.contains("catalogue has its lead"), "{last:?}");
    // Something in no repo starts it in no repo yet.
    let mut board = led_fixture();
    board.milestones[2].repo = None;
    sync(&mut board);
    let unplaced: View = Ok(board);
    assert_eq!(
        start(&unplaced, Item::Milestone(MilestoneId(3))),
        (
            None,
            Some(Request::StartLead {
                line: "Comics".into(),
                repo: None
            })
        )
    );
}

#[test]
fn an_inbox_item_opens_its_repos_full_board() {
    let view: View = Ok(alerts_fixture());
    let a = area(120, 30);
    let mut dash = clocked();
    dash.handle(&view, a, Input::Char('i'));
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
}

#[test]
fn a_milestone_line_in_the_repo_detail_opens_its_detail_and_esc_comes_back() {
    let a = area(140, 40);
    let mut board = repo_standalone_fixture();
    // Folded, so the click has to unfold the repo for the tree to show the milestone.
    board.repos[0].folded = true;
    let view: View = Ok(board);
    let mut dash = opened(&view, Item::Repo(RepoId(1)), a);
    assert_eq!(dash.view(), ViewKind::RepoDetail);
    let l = layout(&view, &dash, a);
    let d = l.detail.clone().unwrap();
    let line = d
        .milestones
        .iter()
        .find(|(_, m)| *m == MilestoneId(1))
        .unwrap()
        .0;
    let (x, y) = right_cell(&l, &dash, line);
    assert_eq!(
        dash.handle(&view, a, Input::Click { x, y }),
        Some(Request::Fold {
            repos: vec![RepoId(1)],
            milestones: Vec::new(),
            done_groups: Vec::new(),
            folded: false
        })
    );
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(1))));
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::RepoDetail);
    assert_eq!(dash.selected, Some(Item::Repo(RepoId(1))));
    // Any other line of the detail is not a milestone's.
    assert_eq!(d.milestone_at(0), None);
}

// The Done group: a repo's milestones with the lead's Done mark, in one row at the
// bottom of the repo.

/// Noon UTC on 14/01/2027 and on 13/01/2027, the day before `NOW`. The dashboard shows a
/// Done day in the local zone, which is another day where it is twelve or more hours from
/// UTC, so the screens read here give each as its UTC day (`utc_days`).
const DAY_14: i64 = 1_799_928_000;
const DAY_13: i64 = DAY_14 - 86_400;

/// `text` with the local days of `DAY_14` and `DAY_13`, as the dashboard draws them, read as
/// their UTC days, so a test passes in any time zone. The days are all ten columns wide,
/// so no layout moves; placeholders keep a day that is both (14/01 local for 13/01 UTC) from
/// being read twice.
fn utc_days(text: String) -> String {
    use chrono::TimeZone;
    let local = |at: i64| {
        (chrono::Local.timestamp_opt(at, 0).single())
            .map_or_else(String::new, |d| d.format("%d/%m/%Y").to_string())
    };
    let (l14, l13) = (local(DAY_14), local(DAY_13));
    if l14 == "14/01/2027" && l13 == "13/01/2027" {
        return text;
    }
    text.replace(&l14, "\u{0}14")
        .replace(&l13, "\u{0}13")
        .replace("\u{0}14", "14/01/2027")
        .replace("\u{0}13", "13/01/2027")
}

/// The board fixture with three Done milestones in catalogue: M6 marked on 14/01, M4
/// on 13/01 with a need-you and an awareness item on its thread, and M5 marked before the
/// board kept the date. M2, in checkout, is 100% without the mark.
fn done_fixture() -> Board {
    let mut board = board_fixture();
    let done = |id, title: &str, tasks: &[i64], at: Option<i64>| Milestone {
        id: MilestoneId(id),
        title: title.into(),
        goal: format!("{title}, released."),
        repo: Some(RepoId(1)),
        lead: None,
        tasks: (tasks.iter())
            .map(|t| Task {
                id: TaskId(*t),
                title: format!("task {t}"),
                stage: Stage::Done,
                repo: None,
                subtasks: Vec::new(),
                details: TaskDetails::default(),
            })
            .collect(),
        marked_done: true,
        done_at: at,
        folded: false,
        threads: Vec::new(),
        details: MilestoneDetails::default(),
    };
    let message = |id, text: &str| Message {
        id,
        author: "cat-lead".into(),
        text: text.into(),
        at: NOW - 30 * 60,
    };
    let mut m4 = done(4, "0.1.6: dashboard workspace", &[61, 62], Some(DAY_13));
    m4.threads = vec![
        Thread::whole(
            ThreadId(9),
            "Is 0.1.6 ready to mark Done?".into(),
            None,
            None,
            vec![message(
                900,
                "question: @manager 0.1.6 is out. Do you want to trial it before I mark it Done?",
            )],
        ),
        Thread::whole(
            ThreadId(10),
            "Release notes for 0.1.6".into(),
            Some(TaskId(62)),
            None,
            vec![message(
                1000,
                "decision: @manager notes go in CHANGELOG.md only",
            )],
        ),
    ];
    board.milestones.push(m4);
    board
        .milestones
        .push(done(5, "0.1.5: lead skill", &[71], None));
    board
        .milestones
        .push(done(6, "0.1.7: repo leads", &[81], Some(DAY_14)));
    with_alerts(&mut board, &[]);
    board
}

fn tree_text(board: &Board, w: usize) -> Vec<String> {
    (tree_lines(board, w, Depth::Ansi256).iter())
        .map(|l| utc_days(l.to_string().trim_end().to_string()))
        .collect()
}

#[test]
fn done_milestones_leave_the_active_list_for_one_folded_row_with_their_badges() {
    let board = done_fixture();
    let tree = tree_text(&board, 60);
    let at = find_row(&tree, "   ▸ Done  3 milestones");
    // At the bottom of the repo, after a blank line, with the badges of M4's thread at
    // the right, and the rule of the next repo under it.
    assert_eq!(tree[at - 1], "", "{tree:#?}");
    assert!(tree[at].ends_with(" ⚠1 ⓘ1"), "{tree:#?}");
    assert!(tree[at + 1].starts_with('─'), "{tree:#?}");
    for gone in ["M4", "M5", "M6", "#61"] {
        assert!(!tree.iter().any(|l| l.contains(gone)), "{gone}: {tree:#?}");
    }
    // The repo's own badges still count every item, the Done group's among them.
    assert!(tree[0].contains("⚠2 ⓘ1"), "{tree:#?}");
    // A 100% milestone without the mark stays in the active list.
    assert!(tree.iter().any(|l| l.contains("M2 Checkout")), "{tree:#?}");
    // Folded, the repo counts its Done milestones on its summary line.
    let mut folded = board.clone();
    folded.repos[0].folded = true;
    assert_eq!(tree_text(&folded, 60)[2], "   1 milestone ✓3 Done");
}

/// A selected Done group whose last Done milestone reopens hands the selection
/// to its own repo, not to the first repo on the board.
#[test]
fn a_done_group_that_empties_hands_the_selection_to_its_own_repo() {
    let mut board = done_fixture();
    // Its repo, R1, is the second on the board.
    board.repos.swap(0, 1);
    assert_ne!(board.repos[0].id, RepoId(1));
    let a = area(120, 30);
    let mut dash = select(Item::Done(RepoId(1)));
    dash.refreshed(&Ok(board.clone()), a);
    assert_eq!(dash.selection(&board), Some(Item::Done(RepoId(1))));
    for m in (board.milestones.iter_mut()).filter(|m| m.repo == Some(RepoId(1))) {
        m.marked_done = false;
        m.done_at = None;
    }
    dash.refreshed(&Ok(board.clone()), a);
    assert_eq!(dash.selection(&board), Some(Item::Repo(RepoId(1))));
}

#[test]
fn unfolded_the_group_lists_each_milestone_newest_first_with_the_day_it_was_marked() {
    let mut board = done_fixture();
    board.repos[0].done_folded = false;
    let tree = tree_text(&board, 60);
    let at = find_row(&tree, "   ▾ Done  3 milestones");
    assert_eq!(
        tree[at + 1..at + 4],
        [
            "     ✓ M6 0.1.7: repo leads                      14/01/2027",
            "     ✓ M4 0.1.6: dashboard workspace             13/01/2027",
            "     ✓ M5 0.1.5: lead skill               date not recorded",
        ]
    );
    // Too narrow for the date beside the title: the date goes.
    let narrow = tree_text(&board, 22);
    assert!(
        narrow.iter().any(|l| l == "     ✓ M6 0.1.7: repo"),
        "{narrow:#?}"
    );
    // Its tasks are never in the tree: a Done milestone is shown read-only.
    let items = tree_items(&board, 60);
    assert!(!items.contains(&Some(Item::Task(TaskId(61)))));
    assert_eq!(items[at], Some(Item::Done(RepoId(1))));
    assert_eq!(items[at + 1], Some(Item::Milestone(MilestoneId(6))));
}

#[test]
fn done_group_snapshots_folded_unfolded_and_selected() {
    let board = done_fixture();
    let mut open = board.clone();
    open.repos[0].done_folded = false;
    for (name, board, item) in [
        ("done_folded", board.clone(), Item::Done(RepoId(1))),
        ("done_unfolded", open.clone(), Item::Done(RepoId(1))),
        ("done_milestone", open, Item::Milestone(MilestoneId(4))),
    ] {
        let view: View = Ok(board);
        for (w, h) in [(140, 50), (72, 50)] {
            let buf = render_dash(&view, &select(item), w, h, Depth::Ansi256);
            assert_exact_width(&buf);
            assert_snapshot(&format!("{name}_{w}x{h}"), &buf);
        }
    }
}

#[test]
fn selecting_the_group_lists_its_milestones_with_the_board_narrowed_to_their_threads() {
    let mut board = done_fixture();
    board.repos[0].done_folded = false;
    let view: View = Ok(board);
    let a = area(140, 50);
    let mut dash = select(Item::Done(RepoId(1)));
    let all = screen(&view, &dash, a);
    let joined = all.join("\n");
    for want in [
        "Done  3 milestones · catalogue · Catalogue Lead",
        "Milestones with the lead's Done mark, most recently done first.",
        "✓ M6 0.1.7: repo leads",
        "1/1 · 14/01/2027",
        "2/2 · 13/01/2027",
        "1/1 · date not recorded",
        "Is 0.1.6 ready to mark Done?",
        // T10 is linked to task #62, so it reads by that task's title.
        "[open] task 62",
    ] {
        assert!(joined.contains(want), "{want:?} missing from\n{joined}");
    }
    // M1's threads are not about a Done milestone.
    assert!(!joined.contains("Task #13 team"), "{joined}");
    // Enter opens nothing: the overview is all the group has.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::Overview);
    // A click on a milestone's line in it selects that milestone.
    let r = right(&layout(&view, &dash, a));
    let line = (r.overview.milestones.iter())
        .find(|(_, m)| *m == MilestoneId(5))
        .unwrap()
        .0;
    let y = r.area.y + line as u16;
    dash.handle(&view, a, Input::Click { x: r.area.x + 4, y });
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(5))));
}

#[test]
fn a_done_line_shows_its_milestone_read_only_with_the_day_it_was_marked() {
    let mut board = done_fixture();
    board.repos[0].done_folded = false;
    let view: View = Ok(board);
    let a = area(140, 50);
    let mut dash = select(Item::Milestone(MilestoneId(4)));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("[done]"), "{all}");
    assert!(
        all.contains("2/2 tasks done marked Done 13/01/2027"),
        "{all}"
    );
    // A click on one of its cards selects nothing.
    let l = layout(&view, &dash, a);
    let r = right(&l);
    let card = r.overview.cards[0];
    let (x, y) = (
        r.area.x + card.x as u16 + 2,
        r.area.y + card.line as u16 + 1,
    );
    assert_eq!(dash.handle(&view, a, Input::Click { x, y }), None);
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(4))));
    // Its detail opens, but its task rows open nothing either.
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("marked Done 13/01/2027"), "{all}");
    let l = layout(&view, &dash, a);
    let row = l.detail.clone().unwrap().rows[0];
    let (x, y) = right_cell(&l, &dash, row.line);
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(dash.selected, Some(Item::Milestone(MilestoneId(4))));
    assert_eq!(dash.view(), ViewKind::MilestoneDetail);
}

#[test]
fn keys_fold_the_group_from_its_row_or_a_done_line_and_z_leaves_it_alone() {
    let a = area(140, 50);
    let fold = |folded| {
        Some(Request::Fold {
            repos: Vec::new(),
            milestones: Vec::new(),
            done_groups: vec![RepoId(1)],
            folded,
        })
    };
    // Folded: left or right on the row unfolds it.
    let view: View = Ok(done_fixture());
    for key in [Input::Left, Input::Right] {
        let mut dash = select(Item::Done(RepoId(1)));
        assert_eq!(dash.handle(&view, a, key), fold(false), "{key:?}");
    }
    // Unfolded: on the row or a Done line it folds, and the row becomes the selection.
    let mut board = done_fixture();
    board.repos[0].done_folded = false;
    let view: View = Ok(board.clone());
    for item in [Item::Done(RepoId(1)), Item::Milestone(MilestoneId(5))] {
        let mut dash = select(item);
        assert_eq!(dash.handle(&view, a, Input::Left), fold(true), "{item:?}");
        assert_eq!(dash.selected, Some(Item::Done(RepoId(1))));
    }
    // Up and down pass over the row and its lines, from the last standalone task.
    let mut dash = select(Item::Task(TaskId(14)));
    let mut seen = Vec::new();
    for _ in 0..5 {
        dash.handle(&view, a, Input::Down);
        seen.push(dash.selected.unwrap());
    }
    assert_eq!(
        seen,
        [
            Item::Done(RepoId(1)),
            Item::Milestone(MilestoneId(6)),
            Item::Milestone(MilestoneId(4)),
            Item::Milestone(MilestoneId(5)),
            Item::Repo(RepoId(2)),
        ]
    );
    // z folds every active milestone and no Done milestone or group.
    let mut dash = select(Item::Repo(RepoId(1)));
    match dash.handle(&view, a, Input::Char('z')) {
        Some(Request::Fold {
            repos,
            milestones,
            done_groups,
            folded: true,
        }) => {
            assert!(repos.is_empty() && done_groups.is_empty());
            assert_eq!(milestones, [MilestoneId(1), MilestoneId(2), MilestoneId(3)]);
        }
        other => panic!("{other:?}"),
    }
    // A click on the row's fold mark toggles it.
    let all = rows(&render(&view, 140, 50));
    let y = find_row(&all, "   ▾ Done") as u16;
    assert_eq!(dash.handle(&view, a, Input::Click { x: 4, y }), fold(true));
    // A refresh that folds the group selects its row in place of a Done line.
    let mut dash = select(Item::Milestone(MilestoneId(6)));
    dash.refreshed(&Ok(done_fixture()), a);
    assert_eq!(dash.selected, Some(Item::Done(RepoId(1))));
}

#[test]
fn the_group_fold_is_kept_on_the_board_and_a_task_change_brings_a_milestone_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let mut store = Store::open(&path).unwrap();
    let r = new_repo(&mut store);
    let m = store.add_milestone("Shipped", "g", Some(r)).unwrap();
    let t = store.add_task(m, "only").unwrap();
    store.set_task_stage(t, Stage::Done).unwrap();
    // 100% without the mark: active, its overview says the mark is missing.
    let view = refresh(&path);
    let all = rows(&render(&view, 140, 30)).join("\n");
    assert!(all.contains("▾ M1 Shipped"), "{all}");
    assert!(!all.contains("Done  1 milestone"), "{all}");
    let shown = screen(&view, &select(Item::Milestone(m)), area(140, 30)).join("\n");
    assert!(shown.contains("Done mark not set"), "{shown}");
    store.mark_milestone_done(m).unwrap();
    let view = refresh(&path);
    let all = rows(&render(&view, 140, 30)).join("\n");
    assert!(all.contains("▸ Done  1 milestone"), "{all}");
    assert!(!all.contains("M1 Shipped"), "{all}");
    // Unfolding is written through the store, so every dashboard shows it.
    let a = area(140, 30);
    let mut dash = select(Item::Done(r));
    let asked = dash.handle(&view, a, Input::Right).unwrap();
    assert_eq!(
        write(&path, asked),
        (format!("unfolded {r}'s Done group"), false)
    );
    let view = refresh(&path);
    assert!(!view.as_ref().unwrap().repos[0].done_folded);
    let all = rows(&render(&view, 140, 30)).join("\n");
    assert!(all.contains("✓ M1 Shipped"), "{all}");
    // A task change clears the mark: the milestone is back in the active list.
    store.add_task(m, "late").unwrap();
    let view = refresh(&path);
    let all = rows(&render(&view, 140, 30)).join("\n");
    assert!(all.contains("▾ M1 Shipped"), "{all}");
    assert!(!all.contains("Done  1 milestone"), "{all}");
}

#[test]
fn a_selected_task_whose_milestone_gets_the_done_mark_hands_the_selection_to_its_milestone() {
    let a = area(140, 50);
    let mut before = done_fixture();
    // M7 in checkout, the second repo, with its task Done but no mark yet.
    let mut m7 = before.milestones[3].clone();
    m7.id = MilestoneId(7);
    m7.title = "Checkout buttons".into();
    m7.repo = Some(RepoId(2));
    m7.marked_done = false;
    m7.done_at = None;
    m7.threads = Vec::new();
    m7.tasks.truncate(1);
    m7.tasks[0].id = TaskId(91);
    before.milestones.push(m7);
    with_alerts(&mut before, &[]);
    let marked = |folded: bool| {
        let mut b = before.clone();
        let m = b
            .milestones
            .iter_mut()
            .find(|m| m.id == MilestoneId(7))
            .unwrap();
        m.marked_done = true;
        m.done_at = Some(DAY_14);
        b.repos[1].done_folded = folded;
        Ok(b)
    };
    for (folded, want) in [
        (true, Item::Done(RepoId(2))),
        (false, Item::Milestone(MilestoneId(7))),
    ] {
        let view: View = Ok(before.clone());
        let mut dash = select(Item::Task(TaskId(91)));
        dash.handle(&view, a, Input::Enter);
        assert_eq!(dash.view(), ViewKind::TaskDetail);
        let after: View = marked(folded);
        dash.refreshed(&after, a);
        // Its own repo's Done row or line, never the first repo on the board.
        assert_eq!(dash.selected, Some(want), "folded {folded}");
        assert_eq!(dash.view(), ViewKind::Overview, "folded {folded}");
    }
}

#[test]
fn the_repo_detail_lists_done_milestones_under_a_label_with_their_tally_and_day() {
    let view: View = Ok(done_fixture());
    let a = area(140, 60);
    let mut dash = select(Item::Repo(RepoId(1)));
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.view(), ViewKind::RepoDetail);
    let all = screen(&view, &dash, a);
    let at = find_row_containing(&all, " Done 3 milestones");
    for (i, want) in [
        ("M6 0.1.7: repo leads", "1/1 · 14/01/2027"),
        ("M4 0.1.6: dashboard workspace", "2/2 · 13/01/2027"),
        ("M5 0.1.5: lead skill", "1/1 · date not recorded"),
    ]
    .iter()
    .enumerate()
    {
        let line = &all[at + 1 + i];
        assert!(line.contains(want.0) && line.contains(want.1), "{line}");
    }
    assert!(
        all.join("\n")
            .contains("1 active milestone · 0 standalone tasks · 3 Done milestones"),
        "{all:#?}"
    );
}

#[test]
fn right_moves_into_the_chat_and_its_keys_step_through_messages_and_read_more() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    let long = (0..120)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    messages.push(Message {
        id: 398,
        author: "dev-2".into(),
        text: long,
        at: NOW - 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    assert_eq!(dash.board_message(), None);
    // → selects the newest message, outlined, and the keys line says what the keys do now.
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(398));
    let all = screen(&view, &dash, a);
    assert!(
        all[39]
            .contains("↑↓ message · → read more or less · ← threads · type to write · enter posts"),
        "{}",
        all[39]
    );
    let b = board_of(&view, &dash, a);
    let closed = b.conv_lines;
    assert!(all.iter().any(|r| r.contains("▌ read more")), "{all:#?}");
    // → opens it in place (Enter posts now); → again folds it.
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().contains(&398));
    let open = board_of(&view, &dash, a);
    assert!(open.conv_lines > closed);
    assert!(
        screen(&view, &dash, a)
            .iter()
            .any(|r| r.contains("▌ show less"))
    );
    dash.handle(&view, a, Input::Right);
    assert!(!dash.open_messages().contains(&398));
    // ↑ moves to the message before it; the thread stays.
    let before = b.starts[b.starts.len() - 2].0;
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.board_message(), Some(before));
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    // ← returns to the thread list, where ↑↓ move between threads again.
    dash.handle(&view, a, Input::Left);
    assert_eq!(dash.board_message(), None);
    dash.handle(&view, a, Input::Down);
    assert_ne!(dash.board_thread(&view), Some(ThreadId(3)));
    // A click on "read more" opens that message.
    let mut dash = on_t3(&view, a);
    let b = board_of(&view, &dash, a);
    let all = screen(&view, &dash, a);
    let y = all.iter().position(|r| r.contains("▌ read more")).unwrap() as u16;
    dash.handle(&view, a, Input::Click { x: b.conv.x + 4, y });
    assert!(dash.open_messages().contains(&398));
    assert_eq!(dash.board_message(), Some(398));
}

/// The board fixture with T3's chat holding everything the chat draws: messages from
/// yesterday and hours ago (separators), a run, acks, nudges of one worker, a delivery
/// failure, the watch's flag, a long message folded behind "read more" and an owner reply.
fn chat_fixture() -> Board {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let msg = |id, author: &str, text: &str, ago: i64| Message {
        id,
        author: author.into(),
        text: text.into(),
        at: NOW - ago,
    };
    let long = (1..=8)
        .map(|i| format!("{i}. a step of the plan with enough words to wrap a little"))
        .collect::<Vec<_>>()
        .join("\n");
    let messages = vec![
        msg(
            501,
            "cat-lead",
            "Picked up #13 yesterday; team below.",
            30 * 3600,
        ),
        msg(502, "dev-2", "copy", 30 * 3600 - 60),
        msg(503, "dev-2", &long, 3 * 3600),
        msg(
            504,
            "system",
            "nudged dev-2: no post for 20 min",
            2 * 3600 + 300,
        ),
        msg(
            505,
            "system",
            "nudged dev-2: no post for 20 min",
            2 * 3600 + 240,
        ),
        msg(
            506,
            "system",
            "@manager could not deliver to Catalogue Lead, the lead of M1: its pane is \
             gone (it attaches again with tower lead attach M1). Its posts stay on the board.",
            2 * 3600 + 180,
        ),
        msg(507, "qa", "blocked: the printer has not replied", 50 * 60),
        msg(508, "qa", "unblocked, testing now", 40 * 60),
        msg(
            509,
            "cat-lead",
            "question: @manager print the reading list now, or wait for the reply?",
            20 * 60,
        ),
        msg(510, "manager", "Wait for the proofs.", 10 * 60),
        msg(511, "cat-lead", "copy, waiting", 5 * 60),
        msg(
            512,
            "system",
            "@cat-lead #13 is in progress but its worker dev-2 is gone from herdr (no live agent).",
            30,
        ),
    ];
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    board
}

/// The chat fixture's T3 `w` by `h`, qa's run selected: its snapshot, every kind of line,
/// and the selected message outlined.
fn chat_snapshot(w: u16, h: u16) {
    let view: View = Ok(chat_fixture());
    let a = area(w, h);
    let mut dash = on_t3(&view, a);
    // Into the chat (its newest, the "left herdr" line, which has details), then up to
    // qa's run: its first message is outlined.
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(512));
    for _ in 0..4 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.board_message(), Some(507));
    let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot(&format!("chat_{w}x{h}"), &buf);
    let all = rows(&buf).join("\n");
    for want in [
        "─ yesterday ─",
        "─ 3h ago ─",
        "─ 2h ago ─",
        "─ within the hour ─",
        "─ now ─",
        "dev-2 was nudged 2 times",
        "delivery failed to Catalogue Lead · details",
        "dev-2 left herdr while on #13 · details",
        "you (manager)",
        "read more",
        "↳ dev-2: copy",
        "↳ cat-lead: copy, waiting",
    ] {
        assert!(all.contains(want), "{want:?} at {w}\n{all}");
    }
    // The selected message's edge is the accent on the selected background.
    let y = rows(&buf)
        .iter()
        .position(|r| r.contains("▌ blocked: the"))
        .unwrap() as u16;
    let x = rows(&buf)[usize::from(y)].find('▌').unwrap();
    let x = rows(&buf)[usize::from(y)][..x].chars().count() as u16;
    assert_eq!(buf[(x, y)].fg, accent(Depth::TrueColor));
    assert_eq!(buf[(x, y)].bg, selected_bg(Depth::TrueColor));
}

#[test]
fn chat_snapshot_with_every_kind_of_line_and_a_selected_message_wide() {
    chat_snapshot(120, 70);
}

#[test]
fn chat_snapshot_with_every_kind_of_line_and_a_selected_message_narrow() {
    chat_snapshot(60, 90);
}

#[test]
fn read_more_in_an_inline_preview_opens_the_message_in_place() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    let long = (0..60)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    messages.push(Message {
        id: 399,
        author: "dev-2".into(),
        text: long,
        at: NOW - 30,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 90);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    let y = all.iter().position(|r| r.contains("▌ read more")).unwrap() as u16;
    let x = all[usize::from(y)].find("▌ read more").unwrap();
    let x = all[usize::from(y)][..x].chars().count() as u16 + 3;
    // A click on it opens the message where it is; the overview stays.
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(dash.view(), ViewKind::Overview);
    assert!(dash.open_messages().contains(&399));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("w59") && all.contains("▌ show less"), "{all}");
    // A click on "show less" folds it again.
    let rows = screen(&view, &dash, a);
    let y = rows.iter().position(|r| r.contains("▌ show less")).unwrap() as u16;
    dash.handle(&view, a, Input::Click { x, y });
    assert!(!dash.open_messages().contains(&399));
    // A click elsewhere in the box still opens the thread.
    let y = rows
        .iter()
        .position(|r| r.contains("[open] New book shelf"))
        .unwrap() as u16;
    dash.handle(&view, a, Input::Click { x, y });
    assert_eq!(dash.view(), ViewKind::Board);
}

#[test]
fn an_open_message_stays_open_through_a_post_and_a_thread_switch_until_its_thread_is_gone() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    let long = (0..120)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    messages.push(Message {
        id: 398,
        author: "dev-2".into(),
        text: long,
        at: NOW - 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board.clone());
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    dash.handle(&view, a, Input::Right);
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().contains(&398));
    // The owner posts: the refresh after it reads the plain board, which carries only the
    // thread's latest messages, here not the open one.
    let mut trimmed = board.clone();
    for r in &mut trimmed.repos {
        for t in &mut r.threads {
            if t.id == ThreadId(3) {
                t.messages.retain(|m| m.id != 398);
            }
        }
    }
    dash.refreshed(&Ok(trimmed), a);
    assert!(
        dash.open_messages().contains(&398),
        "a trimmed read is no sign it is gone"
    );
    // Another thread and back: still open.
    dash.handle(&view, a, Input::Left);
    dash.handle(&view, a, Input::Down);
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    assert!(dash.open_messages().contains(&398));
    let all = screen(&view, &dash, a).join("\n");
    assert!(
        all.contains("word119") && all.contains("show less"),
        "{all}"
    );
    // Its thread gone from the board: forgotten.
    let mut gone = board.clone();
    for r in &mut gone.repos {
        r.threads.retain(|t| t.id != ThreadId(3));
    }
    dash.refreshed(&Ok(gone), a);
    assert!(!dash.open_messages().contains(&398));
}

#[test]
fn a_question_in_the_needs_you_strip_opens_the_full_board_on_it_replying() {
    let view: View = Ok(board_fixture());
    let a = area(120, 50);
    // From the overview's strip: the question is T4's (402), "the covers are late again".
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    let strip = all
        .iter()
        .position(|r| r.contains("⚠ Needs you · 1"))
        .unwrap();
    let y = (strip + 1) as u16;
    assert!(
        all[usize::from(y)].contains("cat-lead question: the covers are late again"),
        "{all:#?}"
    );
    dash.handle(&view, a, Input::Click { x: 60, y });
    assert_eq!(dash.view(), ViewKind::Board);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    assert_eq!(dash.board_message(), Some(402));
    assert_eq!(dash.replying(), Some(402));
    assert!(dash.writing(&view, a), "the keys write to it");
    let all = screen(&view, &dash, a);
    let reply = all
        .iter()
        .position(|r| r.contains("replying to cat-lead"))
        .unwrap();
    assert!(
        all[reply].contains("question: the covers are late again"),
        "{}",
        all[reply]
    );
    // The question is in view, outlined.
    assert!(
        all.iter()
            .any(|r| r.contains("▌ question: the covers are late again"))
    );
    // Esc goes back and drops the reply line: the board opens again without it.
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::Overview);
    assert_eq!(dash.replying(), None);
    dash.handle(&view, a, Input::Char('b'));
    assert_eq!(dash.replying(), None);
    assert!(
        !screen(&view, &dash, a)
            .iter()
            .any(|r| r.contains("replying to"))
    );
    dash.handle(&view, a, Input::Esc);
    dash.handle(&view, a, Input::Click { x: 60, y });
    assert_eq!(dash.replying(), Some(402));
    // From the full board's own strip, on another thread: the same. Left leaves the chat
    // for the thread list first.
    dash.handle(&view, a, Input::Left);
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    let b = board_of(&view, &dash, a);
    let row = b.strip_rows.iter().position(|r| r.is_some()).unwrap() as u16;
    dash.handle(
        &view,
        a,
        Input::Click {
            x: 10,
            y: b.strip.y + row,
        },
    );
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
    assert_eq!(dash.replying(), Some(402));
    // A post that goes through drops the reply line; the thread stays.
    typed(&mut dash, &view, a, "approved");
    assert!(dash.handle(&view, a, Input::Enter).is_some());
    dash.posted(("posted to T4".into(), false));
    assert_eq!(dash.replying(), None);
    assert_eq!(dash.board_thread(&view), Some(ThreadId(4)));
}

#[test]
fn a_board_with_no_question_for_the_owner_says_so_in_its_strip() {
    let view: View = Ok(board_fixture());
    let a = area(120, 50);
    // T3's task (#13) has no question waiting.
    let dash = select(Item::Task(TaskId(13)));
    let all = screen(&view, &dash, a);
    assert!(
        all.iter()
            .any(|r| r.contains("nothing on this board needs you")),
        "{all:#?}"
    );
    // A task with no thread yet still has its strip, over why there is no thread.
    let all = screen(&view, &select(Item::Task(TaskId(12))), a);
    let strip = all
        .iter()
        .position(|r| r.contains("nothing on this board needs you"));
    let why = all
        .iter()
        .position(|r| r.contains("no thread for this task yet"));
    assert!(strip.is_some() && strip < why, "{all:#?}");
    // The milestone's board has T4's.
    let all = screen(&view, &select(Item::Milestone(MilestoneId(1))), a);
    assert!(
        all.iter().any(|r| r.contains("⚠ Needs you · 1")),
        "{all:#?}"
    );
}

/// The board fixture with T9, a thread only tower posted in about #13 (T3's task): a nudge
/// and a delivery failure, for the owner's awareness.
fn folded_fixture() -> Board {
    let mut board = board_fixture();
    let msg = |id, text: &str, minutes: i64| Message {
        id,
        author: "system".into(),
        text: text.into(),
        at: NOW - minutes * 60,
    };
    board.milestones[0].threads.push(Thread::whole(
        ThreadId(9),
        "#13 watch".into(),
        Some(TaskId(13)),
        None,
        vec![
            msg(901, "nudged dev-2: no post for 20 min", 9),
            msg(
                902,
                "@manager could not deliver to dev-2: herdr has no agent named dev-2. Its posts \
                 stay on the board.",
                1,
            ),
        ],
    ));
    sync(&mut board);
    board
}

#[test]
fn opening_a_conversation_reads_the_awareness_items_of_the_threads_folded_into_it() {
    let view: View = Ok(folded_fixture());
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    // T9 is not in the list; its lines are in T3's chat.
    let all = screen(&view, &dash, a).join("\n");
    assert!(!all.contains("#13 watch"), "{all}");
    assert!(all.contains("delivery failed to dev-2"), "{all}");
    let read = dash.opened(&view);
    assert!(read.contains(&902), "{read:?}");
}

#[test]
fn a_reply_survives_a_click_elsewhere_and_a_refused_post() {
    let view: View = Ok(board_fixture());
    let a = area(120, 50);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    let y = all
        .iter()
        .position(|r| r.contains("⚠ Needs you · 1"))
        .unwrap() as u16
        + 1;
    dash.handle(&view, a, Input::Click { x: 60, y });
    assert_eq!(dash.replying(), Some(402));
    // A click in the conversation, away from the composer: still replying.
    let b = board_of(&view, &dash, a);
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.conv.x + 2,
            y: b.conv.y,
        },
    );
    assert_eq!(dash.replying(), Some(402));
    // The click kept the keys in the draft. A post the board refuses keeps the
    // draft and the reply line.
    assert!(dash.writing(&view, a));
    typed(&mut dash, &view, a, "approved");
    assert!(dash.handle(&view, a, Input::Enter).is_some());
    dash.posted(("the thread is resolved".into(), true));
    assert_eq!(dash.replying(), Some(402));
}

#[test]
fn the_strip_lists_every_question_and_counts_the_rest_only_when_the_pane_is_short() {
    let mut board = board_fixture();
    // Five more questions in T3.
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    for i in 0..5 {
        messages.push(Message {
            id: 380 + i,
            author: "dev-2".into(),
            text: format!("question: @manager choice {i}?"),
            at: NOW - 60 * (5 - i),
        });
    }
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    // The overview scrolls: every question listed, no count line.
    let all = screen(
        &view,
        &select(Item::Milestone(MilestoneId(1))),
        area(120, 90),
    );
    assert!(
        all.iter().any(|r| r.contains("⚠ Needs you · 6")),
        "{all:#?}"
    );
    assert!(all.iter().filter(|r| r.contains("question:")).count() >= 6);
    assert!(
        !all.iter().any(|r| r.contains("more, in the inbox")),
        "{all:#?}"
    );
    // The full board's strip takes at most a third of what is under its top line: tall,
    // every question; short, as many as fit above "… N more, in the inbox (i)".
    for (h, all_fit) in [(12, false), (60, true)] {
        let a = area(120, h);
        let mut dash = select(Item::Milestone(MilestoneId(1)));
        dash.handle(&view, a, Input::Char('b'));
        let b = board_of(&view, &dash, a);
        let shown = b.strip_rows.iter().filter(|r| r.is_some()).count();
        assert_eq!(shown == 6, all_fit, "h={h}");
        assert!(shown >= 1, "h={h}");
        // Its heading, the questions, the count of the rest, and a blank row.
        assert_eq!(
            usize::from(b.strip.height),
            1 + shown + usize::from(!all_fit) + 1
        );
        let rows = screen(&view, &dash, a);
        let more = format!("… {} more, in the inbox (i)", 6 - shown);
        assert_eq!(
            rows.iter().any(|r| r.contains(&more)),
            !all_fit,
            "h={h}\n{rows:#?}"
        );
        // A click on each listed question opens it, replying.
        for (i, r) in b.strip_rows.iter().enumerate() {
            let Some((thread, id)) = r else { continue };
            let mut d = dash.clone();
            d.handle(
                &view,
                a,
                Input::Click {
                    x: 10,
                    y: b.strip.y + i as u16,
                },
            );
            assert_eq!(d.board_thread(&view), Some(*thread));
            assert_eq!(d.replying(), Some(*id));
        }
    }
}

#[test]
fn the_chat_keys_stop_at_the_ends_ignore_a_short_message_and_recover_a_gone_selection() {
    let view: View = Ok(board_fixture());
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    dash.handle(&view, a, Input::Right);
    let newest = dash.board_message().unwrap();
    // Right on a message with no "read more": nothing opens.
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().is_empty());
    assert_eq!(dash.board_message(), Some(newest));
    // Up all the way: the first message, and no further.
    let first = board_of(&view, &dash, a).starts[0].0;
    for _ in 0..20 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.board_message(), Some(first));
    dash.handle(&view, a, Input::Up);
    assert_eq!(dash.board_message(), Some(first));
    // The selected message gone from the chat (a refresh without it): Down selects the
    // newest rather than jumping to the top.
    let mut board = board_fixture();
    for r in &mut board.repos {
        for t in &mut r.threads {
            t.messages.retain(|m| m.id != first);
        }
    }
    let gone: View = Ok(board);
    dash.handle(&gone, a, Input::Down);
    assert_eq!(dash.board_message(), Some(newest));
}

#[test]
fn where_it_stands_reads_tower_s_word_as_the_chat_s_line_with_no_label() {
    let mut board = board_fixture();
    // T4 (task #14, no progress post) holds only the watch's flag, so where it stands is
    // tower's word.
    let t = &mut board.milestones[0].threads[2];
    let flag = Message {
        id: 499,
        author: "system".into(),
        text: "@cat-lead #14 is in progress but its worker dev-1 is gone from herdr (no live \
               agent by that name)."
            .into(),
        at: NOW - 30,
    };
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, vec![flag]);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 40);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    dash.handle(&view, a, Input::Char('b'));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("dev-1 left herdr while on #14"), "{all}");
    assert!(!all.contains("system: "), "{all}");
}

#[test]
fn where_it_stands_folds_the_runs_of_spaces_in_tower_s_word() {
    let mut board = board_fixture();
    // T4 (task #14, no progress post) holds only a delivery failure whose herdr error has
    // runs of spaces, so where it stands is tower's word.
    let t = &mut board.milestones[0].threads[2];
    let failure = Message {
        id: 498,
        author: "system".into(),
        text: "@manager could not deliver to dev-1:  herdr has   no agent named dev-1.  ".into(),
        at: NOW - 30,
    };
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, vec![failure]);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(160, 50);
    let mut dash = select(Item::Task(TaskId(14)));
    dash.handle(&view, a, Input::Char('b'));
    let all = screen(&view, &dash, a);
    // As the signed design's (UI v0.1.13) nowrap line: each run is one space, on the header
    // and in the list.
    let folded = "@manager could not deliver to dev-1: herdr has no agent named dev-1.";
    let head = find_row_containing(&all, "where it stands");
    assert!(
        all[head].contains(&format!("where it stands {folded}")),
        "{all:#?}"
    );
    assert!(
        (all.iter()).any(|r| r
            .split('│')
            .next()
            .unwrap()
            .contains("   @manager could not deliver to dev-1: herdr")),
        "{all:#?}"
    );
}

#[test]
fn where_it_stands_draws_tower_s_word_in_the_text_colour_with_only_its_lead_in_dim() {
    let mut board = board_fixture();
    // T4 (task #14, no progress post) holds only a delivery failure, so where it stands is
    // tower's word.
    let t = &mut board.milestones[0].threads[2];
    let failure = Message {
        id: 497,
        author: "system".into(),
        text: "@manager could not deliver to dev-1: herdr has no agent named dev-1.".into(),
        at: NOW - 30,
    };
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, vec![failure]);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(160, 50);
    let word = "@manager could not deliver to dev-1";
    let stands = format!("where it stands {word}");
    // The milestone's message board, with T4's card; the full board on T4, its header
    // beside the list.
    let card = select(Item::Milestone(MilestoneId(1)));
    let mut full = select(Item::Task(TaskId(14)));
    full.handle(&view, a, Input::Char('b'));
    for depth in [Depth::TrueColor, Depth::Ansi256] {
        for (dash, place) in [(&card, "card"), (&full, "full board")] {
            let buf = render_dash(&view, dash, 160, 50, depth);
            let all = rows(&buf);
            // Each line with tower's word: the card's or the header's, after its lead-in;
            // on the full board also the list's row, left of its bar, with no lead-in.
            let head = find_row_containing(&all, &stands);
            let mut lines = vec![(head, all[head].find(&stands).unwrap(), true)];
            if place == "full board" {
                let list = (all.iter())
                    .position(|r| r.split('│').next().unwrap().contains(word))
                    .unwrap_or_else(|| panic!("{all:#?}"));
                lines.push((list, all[list].find(word).unwrap(), false));
            }
            for (y, at, lead_in) in lines {
                let x = all[y][..at].width();
                let cell = |x: usize| buf[(x as u16, y as u16)].clone();
                let from = if lead_in {
                    // As the signed design (UI v0.1.13): only "where it stands" is dim.
                    assert_eq!(cell(x).symbol(), "w", "{place} {all:#?}");
                    assert_eq!(cell(x).fg, dim_colour(depth), "{place} {depth:?}");
                    x + "where it stands ".width()
                } else {
                    x
                };
                // Tower's word in the text colour, as a person's plain words, where it
                // had drawn dim.
                for (i, c) in word.chars().enumerate() {
                    let got = cell(from + i);
                    assert_eq!(got.symbol(), c.to_string(), "{place} {all:#?}");
                    assert_eq!(got.fg, Color::Reset, "{place} {lead_in} {depth:?}");
                }
            }
        }
    }
}

#[test]
fn where_it_stands_keeps_the_question_word_of_tower_s_question() {
    let mut board = board_fixture();
    // T4 (task #14, no progress post) holds only tower's question for the owner, so where
    // it stands is tower's word.
    let t = &mut board.milestones[0].threads[2];
    let question = Message {
        id: 498,
        author: "system".into(),
        text: "question: @manager dev-1 on #14 is waiting on a prompt in herdr that only you \
               can answer."
            .into(),
        at: NOW - 30,
    };
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, vec![question]);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(160, 50);
    let word = "question: @manager dev-1 on #14";
    // The milestone's message board, with T4's card; the full board on T4, its header
    // beside the list.
    let card = select(Item::Milestone(MilestoneId(1)));
    let mut full = select(Item::Task(TaskId(14)));
    full.handle(&view, a, Input::Char('b'));
    for (dash, place) in [(&card, "card"), (&full, "full board")] {
        let all = screen(&view, dash, a);
        // As the signed design (UI v0.1.13): no "needs you" tag on where it stands says it,
        // so the word stays.
        find_row_containing(&all, &format!("where it stands {word}"));
        if place == "full board" {
            // The thread list's row too, left of its bar, with no lead-in (the Needs you
            // strip's row starts with "system", the header's with "where it stands").
            assert!(
                (all.iter()).any(|r| r.split('│').next().unwrap().trim_start().starts_with(word)),
                "{all:#?}"
            );
            // The chat's event line keeps its tag and drops the word, as built: the tag
            // right before "@manager" leaves no room for "question:".
            find_row_containing(&all, "needs you @manager dev-1 on #14");
        }
    }
}

#[test]
fn opening_a_folded_conversation_leaves_its_question_waiting() {
    let mut board = folded_fixture();
    // T9 (tower's, about #13) also asks the owner something.
    let t9 = board.milestones[0]
        .threads
        .iter_mut()
        .find(|t| t.id == ThreadId(9))
        .unwrap();
    let mut messages = t9.messages.clone();
    messages.push(Message {
        id: 903,
        author: "system".into(),
        text: "question: @manager dev-2 on #13 is waiting on a prompt in herdr that only you \
               can answer (@cat-lead for your information)."
            .into(),
        at: NOW - 20,
    });
    *t9 = Thread::whole(t9.id, t9.title.clone(), t9.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 40);
    let mut dash = on_t3(&view, a);
    let read = dash.opened(&view);
    // The awareness item is read; the question is not: only r clears it.
    assert!(read.contains(&902), "{read:?}");
    assert!(!read.contains(&903), "{read:?}");
    let Ok(b) = &view else { unreachable!() };
    assert!(b.inbox.need_you.iter().any(|x| x.message.id == 903));
}

#[test]
fn the_tree_is_quiet_only_in_progress_and_selected_titles_in_normal_text() {
    let view: View = Ok(led_fixture());
    let d = Depth::TrueColor;
    // #14 is waiting, #13 in progress, #12 done; #14 selected.
    let buf = render_dash(&view, &select(Item::Task(TaskId(14))), 120, 30, d);
    let all = rows(&buf);
    let title_fg = |needle: &str| {
        let y = all.iter().position(|r| r.contains(needle)).unwrap();
        let x = all[y].find(needle).unwrap();
        let x = all[y][..x].chars().count() as u16;
        buf[(x, y as u16)].fg
    };
    assert_eq!(title_fg("Bookmarks page"), dim_colour(d));
    assert_ne!(title_fg("New book shelf"), dim_colour(d));
    assert_ne!(title_fg("Show each book's"), dim_colour(d));
    // Unselected, the waiting title is dim again.
    let buf = render_dash(&view, &select(Item::Task(TaskId(13))), 120, 30, d);
    let all = rows(&buf);
    let y = all
        .iter()
        .position(|r| r.contains("Show each book's"))
        .unwrap();
    let x = all[y].find("Show each book's").unwrap();
    let x = all[y][..x].chars().count() as u16;
    assert_eq!(buf[(x, y as u16)].fg, dim_colour(d));
    // A rule and a blank line between repos.
    let lines: Vec<String> = (tree_lines(&led_fixture(), 40, d).iter())
        .map(|l| l.to_string())
        .collect();
    let rule = lines.iter().position(|l| l.starts_with('─')).unwrap();
    assert!(lines[rule + 1].trim().is_empty(), "{lines:#?}");
    assert!(lines[rule + 2].contains("▾ checkout"), "{lines:#?}");
}

#[test]
fn right_on_the_left_herdr_line_opens_its_full_text() {
    let view: View = Ok(chat_fixture());
    let a = area(120, 70);
    let mut dash = on_t3(&view, a);
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(512));
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().contains(&512));
    // The edge fits before the next frame: what opened scrolls into view.
    dash.fit(&view, a);
    let all = screen(&view, &dash, a).join("\n");
    assert!(
        all.contains("dev-2 left herdr while on #13 · hide details"),
        "{all}"
    );
    assert!(
        all.contains("@cat-lead #13 is in progress but its worker dev-2"),
        "{all}"
    );
}

#[test]
fn a_question_older_than_the_loaded_window_still_gets_its_reply_line_and_selection() {
    let whole = board_fixture();
    // The same board as loaded: T4's question (402) is past the window, its count kept.
    let mut trimmed = whole.clone();
    for r in &mut trimmed.repos {
        for t in &mut r.threads {
            if t.id == ThreadId(4) {
                t.messages.retain(|m| m.id != 402);
            }
        }
    }
    let (trimmed, whole): (View, View) = (Ok(trimmed), Ok(whole));
    let a = area(120, 50);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&trimmed, &dash, a);
    let y = all
        .iter()
        .position(|r| r.contains("⚠ Needs you · 1"))
        .unwrap() as u16
        + 1;
    dash.handle(&trimmed, a, Input::Click { x: 60, y });
    assert_eq!(dash.board_thread(&trimmed), Some(ThreadId(4)));
    assert_eq!(dash.replying(), Some(402));
    assert!(dash.writing(&trimmed, a));
    // The reply line comes from the inbox item, though the chat does not hold it yet.
    let all = screen(&trimmed, &dash, a).join("\n");
    assert!(
        all.contains("replying to cat-lead: question: the covers are late again"),
        "{all}"
    );
    // Once the whole history is in, the question is selected and in view.
    dash.fit(&whole, a);
    assert_eq!(dash.board_message(), Some(402));
    let all = screen(&whole, &dash, a).join("\n");
    assert!(
        all.contains("▌ question: the covers are late again"),
        "{all}"
    );
}

#[test]
fn a_question_in_a_folded_thread_opens_its_home_conversation_replying() {
    let mut board = folded_fixture();
    let t9 = (board.milestones[0].threads.iter_mut())
        .find(|t| t.id == ThreadId(9))
        .unwrap();
    let mut messages = t9.messages.clone();
    messages.push(Message {
        id: 903,
        author: "system".into(),
        text: "question: @manager dev-2 on #13 is waiting on a prompt in herdr that only you \
               can answer (@cat-lead for your information)."
            .into(),
        at: NOW - 20,
    });
    *t9 = Thread::whole(t9.id, t9.title.clone(), t9.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 50);
    let mut dash = select(Item::Task(TaskId(13)));
    let all = screen(&view, &dash, a);
    let y = all
        .iter()
        .position(|r| r.contains("⚠ Needs you · 1"))
        .unwrap() as u16
        + 1;
    assert!(
        all[usize::from(y)].contains("in New book shelf"),
        "{all:#?}"
    );
    dash.handle(&view, a, Input::Click { x: 60, y });
    // T3, the conversation T9 joins, with the question selected and the composer open.
    assert_eq!(dash.board_thread(&view), Some(ThreadId(3)));
    assert_eq!(dash.board_message(), Some(903));
    assert_eq!(dash.replying(), Some(903));
    assert!(dash.writing(&view, a));
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("replying to system"), "{all}");
}

#[test]
fn an_inbox_item_keeps_its_message_and_its_repo_and_age_before_its_thread() {
    let mut board = alerts_fixture();
    let view: View = Ok(board.clone());
    // At 40 columns the message keeps its room and the right block goes whole.
    let all = rows(&render_dash(
        &view,
        &inbox_dash(&view, area(40, 24)),
        40,
        24,
        Depth::TrueColor,
    ));
    let item = all.iter().find(|r| r.contains("cat-lead")).unwrap();
    assert!(item.contains("question: the covers"), "{item:?}");
    assert!(!item.contains("catalogue"), "{item:?}");
    // At 120, with a long repo name, the right block is at most a third of the line, its
    // repo cut before its age goes.
    board.repos[0].path = Some("/src/a-repo-with-a-very-long-name-indeed-for-the-inbox".into());
    let view: View = Ok(board);
    let all = rows(&render_dash(
        &view,
        &inbox_dash(&view, area(120, 24)),
        120,
        24,
        Depth::TrueColor,
    ));
    let item = all
        .iter()
        .find(|r| r.contains("question: the covers"))
        .unwrap();
    let right = &item[item.find(" a-repo").unwrap()..];
    assert!(right.chars().count() <= 40, "{right:?}");
    assert!(right.trim_end().ends_with("· 15m"), "{right:?}");
    assert!(right.contains('…'), "{right:?}");
}

#[test]
fn the_alert_line_keeps_the_repo_and_age_and_cuts_the_thread_first() {
    // In order: the thread's name is cut, then left out, then the repo's; the author and
    // age stay together, and when even they do not fit the newest item goes whole.
    let check = |board: &Board, repo: &str| {
        let view: View = Ok(board.clone());
        for w in [60usize, 72, 90, 100, 120, 200] {
            let line = alert_line(view.as_ref().unwrap(), w, Some(NOW), Depth::Ansi256).to_string();
            assert_eq!(line.chars().count(), w, "{line:?}");
            match line.find(" · newest: ") {
                // Shown: always with its author and its age, never a stub.
                Some(at) => {
                    let item = &line[at..];
                    assert!(item.contains("cat-lead"), "w={w} {line:?}");
                    assert!(item.contains("· 15m"), "w={w} {line:?}");
                    // Where it is goes in order: a thread only with its repo.
                    if item.contains(" in ‘") {
                        assert!(
                            item.contains(&format!("’ · {}", &repo[..4])),
                            "w={w} {line:?}"
                        );
                    }
                }
                None => assert!(w <= 72, "w={w} {line:?}"),
            }
        }
        // Wide, the whole of where it is shows, the thread's name uncut.
        let line = alert_line(view.as_ref().unwrap(), 260, Some(NOW), Depth::Ansi256).to_string();
        let want = format!(
            "newest: cat-lead in ‘Show each book's reviews under its cover, newest first, with a link to the rest’ · {repo} · 15m"
        );
        assert!(line.contains(&want), "{line:?}");
    };
    let board = alerts_fixture();
    check(&board, "catalogue");
    let mut long = board.clone();
    long.repos[0].path = Some("/src/a-repo-with-a-very-long-name-indeed-for-the-line".into());
    check(&long, "a-repo-with-a-very-long-name-indeed-for-the-line");
    // With room past author and age, the repo shows (cut if it must) before the thread.
    let view: View = Ok(board.clone());
    for w in [90usize, 100, 120] {
        let line = alert_line(view.as_ref().unwrap(), w, Some(NOW), Depth::Ansi256).to_string();
        assert!(
            line.contains("· newest: cat-lead") && line.contains(" · ca"),
            "w={w} {line:?}"
        );
    }
    // At 120 with the normal name: author, repo and age, the thread left out or cut.
    let view: View = Ok(board);
    let line = alert_line(view.as_ref().unwrap(), 120, Some(NOW), Depth::Ansi256).to_string();
    assert!(line.contains("· catalogue · 15m"), "{line:?}");
    // Its author is not bold, as the signed design (UI v0.1.13).
    let buf = render(&view, 160, 20);
    let all = rows(&buf);
    let x = all[0].find("cat-lead").unwrap();
    let x = all[0][..x].chars().count() as u16;
    assert!(!buf[(x, 0)].modifier.contains(Modifier::BOLD));
}

#[test]
fn a_narrow_card_keeps_its_blocked_mark_and_cuts_the_assignee_first() {
    use control_tower::board::Subtask;
    let mut board = blocked_fixture();
    let t = &mut board.milestones[0].tasks[2];
    t.details.assignee = Some("a-worker-with-a-long-name".into());
    t.subtasks = vec![Subtask {
        id: TaskId(40),
        title: "s".into(),
        stage: Stage::Waiting,
    }];
    let m = &board.milestones[0];
    for w in [60usize, 72, 120] {
        let o = overview(m, None, w, Depth::Ansi256);
        let c = o.cards.iter().find(|c| c.task == TaskId(14)).unwrap();
        let line = o.lines[c.line].to_string();
        let own: String = line.chars().skip(c.x).take(c.width).collect();
        assert!(own.trim_end().ends_with('!'), "w={w} {own:?}");
        assert!(own.starts_with(" a-"), "w={w} {own:?}");
    }
}

#[test]
fn an_opened_message_in_the_middle_of_a_long_chat_scrolls_into_view_once() {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let long = |i: i64| -> String {
        (0..60)
            .map(|k| format!("m{i}w{k}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let messages: Vec<Message> = (0..20)
        .map(|i| Message {
            id: 700 + i,
            author: if i % 2 == 0 { "dev-2" } else { "qa" }.into(),
            text: long(i),
            at: NOW - (20 - i) * 60,
        })
        .collect();
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(120, 30);
    // By key: into the chat, up to the 10th message, open it.
    let mut dash = on_t3(&view, a);
    dash.handle(&view, a, Input::Right);
    for _ in 0..10 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.board_message(), Some(709));
    dash.handle(&view, a, Input::Right);
    dash.fit(&view, a);
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains("m9w0 "), "{all}");
    assert!(all.contains("show less"), "{all}");
    // A manual scroll after it stands: the next fit does not pull it back.
    let before = board_of(&view, &dash, a).conv_top(dash.board_anchor());
    dash.handle(&view, a, Input::PageUp);
    dash.fit(&view, a);
    let after = board_of(&view, &dash, a).conv_top(dash.board_anchor());
    assert!(after < before, "{before} {after}");
    // By mouse: scrolled up, a click on a "read more" opens that message in view.
    let mut dash = on_t3(&view, a);
    for _ in 0..3 {
        dash.handle(&view, a, Input::PageUp);
    }
    let b = board_of(&view, &dash, a);
    let rows_now = screen(&view, &dash, a);
    let y = (b.conv.y..b.conv.y + b.conv.height)
        .find(|y| rows_now[usize::from(*y)].contains("▌ read more"))
        .unwrap();
    dash.handle(&view, a, Input::Click { x: b.conv.x + 4, y });
    let id = dash.board_message().unwrap();
    dash.fit(&view, a);
    let first = format!("m{}w0 ", id - 700);
    let all = screen(&view, &dash, a).join("\n");
    assert!(all.contains(&first), "{first} {all}");
}

#[test]
fn a_question_that_never_reaches_the_chat_keeps_its_reply_until_esc_goes_back() {
    let mut board = board_fixture();
    for r in &mut board.repos {
        for t in &mut r.threads {
            if t.id == ThreadId(4) {
                t.messages.retain(|m| m.id != 402);
            }
        }
    }
    let view: View = Ok(board);
    let a = area(120, 50);
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    let y = all
        .iter()
        .position(|r| r.contains("⚠ Needs you · 1"))
        .unwrap() as u16
        + 1;
    dash.handle(&view, a, Input::Click { x: 60, y });
    assert!(dash.writing(&view, a));
    // A click in the conversation keeps writing, and the reply, while the question has
    // still not come.
    let b = board_of(&view, &dash, a);
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.conv.x + 2,
            y: b.conv.y,
        },
    );
    dash.fit(&view, a);
    dash.fit(&view, a);
    assert!(dash.writing(&view, a));
    assert_eq!(dash.replying(), Some(402));
    // The history never brings it: Esc goes back, and the wait ends with the reply.
    dash.handle(&view, a, Input::Esc);
    assert_eq!(dash.view(), ViewKind::Overview);
    assert_eq!(dash.replying(), None);
    dash.fit(&view, a);
    dash.fit(&view, a);
    assert_eq!(dash.view(), ViewKind::Overview);
    assert!(!dash.writing(&view, a));
}

#[test]
fn a_card_too_narrow_for_its_subtasks_drops_them_and_keeps_its_blocked_mark() {
    use control_tower::board::Subtask;
    let mut board = blocked_fixture();
    let t = &mut board.milestones[0].tasks[2];
    t.details.link = Some("https://github.com/example/bookshop/issues/14".into());
    t.subtasks = (0..120)
        .map(|i| Subtask {
            id: TaskId(40 + i),
            title: "s".into(),
            stage: Stage::Waiting,
        })
        .collect();
    let m = &board.milestones[0];
    // Wide enough: "#14 dev-1 · 0/120 !". In a column about 18 wide (60 columns, three
    // stage columns) the assignee would be cut below four columns, so the subtasks go.
    let card_at = |w: usize| {
        let o = overview(m, None, w, Depth::Ansi256);
        let c = *o.cards.iter().find(|c| c.task == TaskId(14)).unwrap();
        let line = o.lines[c.line].to_string();
        (
            c.width,
            line.chars().skip(c.x).take(c.width).collect::<String>(),
        )
    };
    let (_, wide) = card_at(120);
    assert!(wide.contains("#14 dev-1 · 0/120 !"), "{wide:?}");
    let (width, narrow) = card_at(60);
    assert!((14..=18).contains(&width), "{width}");
    assert!(narrow.trim_end().ends_with('!'), "{narrow:?}");
    assert!(!narrow.contains("0/120"), "{narrow:?}");
    assert!(narrow.contains("#14 dev-1"), "{narrow:?}");
}

#[test]
fn a_one_character_repo_name_still_shows_in_the_inbox_and_on_the_alert_line() {
    let mut board = alerts_fixture();
    board.repos[0].path = Some("/src/x".into());
    let view: View = Ok(board);
    let all = rows(&render_dash(
        &view,
        &inbox_dash(&view, area(120, 24)),
        120,
        24,
        Depth::TrueColor,
    ));
    let item = all
        .iter()
        .find(|r| r.contains("question: the covers"))
        .unwrap();
    assert!(item.contains(" x · Show each"), "{item:?}");
    let line = alert_line(view.as_ref().unwrap(), 120, Some(NOW), Depth::Ansi256).to_string();
    assert!(line.contains(" · x · 15m"), "{line:?}");
}

#[test]
fn the_new_lead_control_is_clickable_exactly_where_it_is_drawn_at_every_width() {
    let mut long = alerts_fixture();
    long.repos[0].path = Some("/src/a-repo-with-a-very-long-name-indeed-for-the-line".into());
    for board in [alerts_fixture(), long, led_fixture()] {
        let view: View = Ok(board);
        for w in 50u16..=140 {
            let a = area(w, 24);
            let l = layout(&view, &clocked(), a);
            let row = rows(&render(&view, w, 24))[0].clone();
            let label = "new lead (n) ▶";
            match l.new_lead {
                Some(r) => {
                    let x = row.find(label).unwrap_or_else(|| panic!("w={w} {row:?}"));
                    assert_eq!(row[..x].chars().count() as u16, r.x, "w={w} {row:?}");
                    assert_eq!(r.width as usize, label.chars().count(), "w={w}");
                }
                None => assert!(!row.contains(label), "w={w} {row:?}"),
            }
            // When the newest item shows, its author and age are both on the line.
            if let Some(at) = row.find(" · newest: ") {
                let item = &row[at..];
                assert!(
                    item.contains("cat-lead") && item.contains("· 15m"),
                    "w={w} {row:?}"
                );
            }
        }
    }
}

/// A question to the owner with a merge request URL after its first sentence, then a later
/// message in the run holding a long path.
fn mr_url_view() -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    for (k, text) in [
        "question: @manager the bookmarks preview is ready: \
         https://gitlab.example.com/acme/bookshop/web/-/merge_requests/27 It adds the bookmark list and \
         the reading reminder.",
        "Icons live in assets/icons/bookmarks/outline/bookmark-ribbon.svg, one file an icon.",
    ]
    .into_iter()
    .enumerate()
    {
        messages.push(Message {
            id: 9000 + k as i64,
            author: "cat-lead".into(),
            text: text.into(),
            at: NOW - 9 * 60 + k as i64 * 60,
        });
    }
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

const MR_URL: &str = "https://gitlab.example.com/acme/bookshop/web/-/merge_requests/27";

/// The rows of `buf` from column `x`, `w` wide.
fn columns(buf: &Buffer, x: u16, w: u16) -> Vec<String> {
    (rows(buf).iter())
        .map(|r| {
            r.chars()
                .skip(usize::from(x))
                .take(usize::from(w))
                .collect()
        })
        .collect()
}

// One test per size, so a run prints every snapshot that is missing or changed at once.
#[test]
fn a_url_starts_its_own_line_at_220_columns() {
    url_message("space", select(Item::Task(TaskId(13))), 220);
}

#[test]
fn a_url_starts_its_own_line_at_160_columns() {
    url_message("space", select(Item::Task(TaskId(13))), 160);
}

#[test]
fn a_url_starts_its_own_line_in_the_popup_at_100_columns() {
    url_message("popup", popup_dash(Item::Task(TaskId(13))), 100);
}

#[test]
fn a_url_starts_its_own_line_at_60_columns() {
    url_message("space", select(Item::Task(TaskId(13))), 60);
}

/// A message with a long URL on the full board `w` wide, in its space or the popup (`name`): the
/// URL starts its own line whole and breaks only after a separator, and the tags and age are
/// on the name line.
fn url_message(name: &str, mut dash: Dash, w: u16) {
    let view = mr_url_view();
    dash.handle(&view, area(w, 50), Input::Char('b'));
    let buf = render_dash(&view, &dash, w, 50, Depth::TrueColor);
    assert_exact_width(&buf);
    assert_snapshot(&format!("url_message_{name}_{w}x50"), &buf);
    let b = board_of(&view, &dash, area(w, 50));
    let conv = columns(&buf, b.conv.x, b.conv.width);
    let text = |r: &String| r.split_once('▌').map(|(_, t)| t.trim_end().to_string());
    // The question's name line holds its tags and age; its text lines none of them.
    let at = (conv.iter())
        .position(|r| r.contains("▌ question: @manager the bookmarks preview"))
        .unwrap_or_else(|| panic!("{w}: {conv:#?}"));
    assert_eq!(
        conv[at - 1].trim(),
        "cat-lead · lead · needs you · 9m",
        "{w}"
    );
    assert_eq!(
        text(&conv[at]).unwrap(),
        " question: @manager the bookmarks preview is ready:",
        "{w}"
    );
    // The URL: on lines of its own, the first at the text's start, the rest indented two
    // columns, each line but the last ending after a separator; together, the URL.
    let mut url = String::new();
    let mut k = at + 1;
    while let Some(t) = text(&conv[k]).filter(|t| k == at + 1 || t.starts_with("   ")) {
        if k == at + 1 {
            assert!(t.starts_with(" https://"), "{w}: {t:?}");
        } else {
            assert!(
                url.ends_with(['/', '?', '&', '-']),
                "{w}: broke after {url:?}"
            );
        }
        url.push_str(t.trim_start());
        k += 1;
    }
    assert_eq!(url, MR_URL, "{w}");
    // The text after it starts a line of its own.
    let after = text(&conv[k]).unwrap();
    assert!(
        after.starts_with(" It adds the bookmark list"),
        "{w}: {after:?}"
    );
    // The later message's age on a dim line of its own above its text; no dashed rule.
    let later = (conv.iter())
        .position(|r| r.contains("▌ Icons live in assets/"))
        .unwrap_or_else(|| panic!("{w}: {conv:#?}"));
    assert_eq!(text(&conv[later - 1]).unwrap(), " 8m", "{w}");
    assert!(!conv.iter().any(|r| r.contains('╌')), "{w}: {conv:#?}");
    // The path stays in its line, and breaks, if it must, only after a `/` or a `-`.
    let first = text(&conv[later]).unwrap();
    let path = "assets/icons/bookmarks/outline/bookmark-ribbon.svg,";
    if !first.contains(path) {
        let next = text(&conv[later + 1]).unwrap();
        assert!(first.ends_with(['/', '-']), "{w}: {first:?}");
        let joined = format!("{}{}", first.trim(), next.trim());
        assert!(joined.contains(path), "{w}: {first:?} {next:?}");
    }
}

/// A message whose list lines are indented as the owner wrote them, one of them with a URL.
fn indented_view() -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    messages.push(Message {
        id: 9100,
        author: "cat-lead".into(),
        text: "Left on the shelf grid:\n  - plain\n  - see https://example.com/a/b".into(),
        at: NOW - 9 * 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

#[test]
fn a_message_lines_indent_stays_in_its_bubble() {
    let view = indented_view();
    let w = 160;
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, area(w, 50), Input::Char('b'));
    let buf = render_dash(&view, &dash, w, 50, Depth::TrueColor);
    assert_exact_width(&buf);
    let b = board_of(&view, &dash, area(w, 50));
    let conv = columns(&buf, b.conv.x, b.conv.width);
    let text = |r: &String| r.split_once('▌').map(|(_, t)| t.trim_end().to_string());
    let at = (conv.iter())
        .position(|r| r.contains("▌ Left on the shelf grid:"))
        .unwrap_or_else(|| panic!("{conv:#?}"));
    // Each list line keeps its two spaces after the bubble's own; the URL's line is at the
    // text's column.
    let got: Vec<String> = (1..=3).map(|k| text(&conv[at + k]).unwrap()).collect();
    assert_eq!(got, ["   - plain", "   - see", " https://example.com/a/b"]);
    assert_snapshot(&format!("indented_message_space_{w}x50"), &buf);
}

/// A message whose lines are aligned in columns with runs of spaces.
fn aligned_view() -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    messages.push(Message {
        id: 9101,
        author: "cat-lead".into(),
        text: "Shelf rows by size:\nphone    2 rows\ntablet   3 rows\ndesktop  4 rows".into(),
        at: NOW - 9 * 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

#[test]
fn a_message_lines_inner_runs_of_spaces_stay_in_its_bubble() {
    let view = aligned_view();
    let w = 160;
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, area(w, 50), Input::Char('b'));
    let buf = render_dash(&view, &dash, w, 50, Depth::TrueColor);
    assert_exact_width(&buf);
    let b = board_of(&view, &dash, area(w, 50));
    let conv = columns(&buf, b.conv.x, b.conv.width);
    let text = |r: &String| r.split_once('▌').map(|(_, t)| t.trim_end().to_string());
    let at = (conv.iter())
        .position(|r| r.contains("▌ Shelf rows by size:"))
        .unwrap_or_else(|| panic!("{conv:#?}"));
    // Each line keeps its columns after the bubble's own space.
    let got: Vec<String> = (1..=3).map(|k| text(&conv[at + k]).unwrap()).collect();
    assert_eq!(
        got,
        [" phone    2 rows", " tablet   3 rows", " desktop  4 rows"]
    );
    assert_snapshot(&format!("aligned_message_space_{w}x50"), &buf);
}

/// A delivery failure whose details hold an indented line and runs of spaces.
fn spaced_failure_view() -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    messages.push(Message {
        id: 9102,
        author: "system".into(),
        text: "@manager could not deliver to 2 recipients; their posts stay on the board:\n\
               - qa-1:  pane   closed\n    exit  1, see the watch's log\n- dev-4: gone"
            .into(),
        at: NOW - 60,
    });
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

#[test]
fn a_delivery_failures_details_keep_their_indent_and_runs_of_spaces() {
    let view = spaced_failure_view();
    let (w, h) = (160, 50);
    let a = area(w, h);
    let mut dash = on_t3(&view, a);
    // Into the chat at its newest, the failure, then open its details.
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(9102));
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().contains(&9102));
    dash.fit(&view, a);
    let buf = render_dash(&view, &dash, w, h, Depth::TrueColor);
    assert_exact_width(&buf);
    let b = board_of(&view, &dash, a);
    let conv = columns(&buf, b.conv.x, b.conv.width);
    let all = conv.join("\n");
    assert!(
        all.contains("delivery failed to 2 recipients · hide details"),
        "{all}"
    );
    let at = (conv.iter())
        .position(|r| r.contains("@manager could not deliver"))
        .unwrap_or_else(|| panic!("{all}"));
    // Every detail line starts at the first one's column.
    let col = conv[at].chars().position(|c| c == '@').unwrap();
    let got: Vec<String> = (1..=3)
        .map(|k| conv[at + k].chars().skip(col).collect::<String>())
        .map(|r| r.trim_end().to_string())
        .collect();
    assert_eq!(
        got,
        [
            "- qa-1:  pane   closed",
            "    exit  1, see the watch's log",
            "- dev-4: gone"
        ]
    );
    assert_snapshot(&format!("spaced_failure_detail_{w}x{h}"), &buf);
}

/// The full board `view` drawn `w` x `h` with the message `id`'s details open: its failure
/// line's first column and the one past its end, and where each line of its details starts.
fn open_details(view: &View, id: i64, w: u16, h: u16) -> ((usize, usize), Vec<usize>) {
    let a = area(w, h);
    let mut dash = on_t3(view, a);
    // Into the chat at its newest, the failure, then open its details.
    dash.handle(view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(id));
    dash.handle(view, a, Input::Right);
    assert!(dash.open_messages().contains(&id));
    dash.fit(view, a);
    let buf = render_dash(view, &dash, w, h, Depth::TrueColor);
    let b = board_of(view, &dash, a);
    let conv = columns(&buf, b.conv.x, b.conv.width);
    let all = conv.join("\n");
    let at = (conv.iter())
        .position(|r| r.contains("· hide details"))
        .unwrap_or_else(|| panic!("{all}"));
    let start = |r: &str| r.chars().position(|c| c != ' ' && c != '│');
    let end = conv[at].trim_end().chars().count();
    let line = (start(&conv[at]).unwrap(), end);
    let details: Vec<usize> = conv[at + 1..].iter().map_while(|r| start(r)).collect();
    assert!(!details.is_empty(), "{all}");
    (line, details)
}

#[test]
fn an_open_failures_details_sit_left_aligned_in_its_centred_box() {
    // The box is as wide as the widest of the failure's line and its details' lines (74
    // columns here), at most EVENT_MAX, and centred as the failure's line is: twice its first
    // column plus its width is the line's first column plus its end, give or take the
    // column a centred line rounds down. Every line of the details starts at that column.
    let centred = |(from, to): (usize, usize), left: usize, bw: usize| {
        (2 * left + bw).abs_diff(from + to) <= 1
    };
    let (line, details) = open_details(&spaced_failure_view(), 9102, 160, 50);
    assert_eq!(details.len(), 4, "{details:?}");
    assert!(centred(line, details[0], 74), "{line:?} {details:?}");
    assert_eq!(
        details,
        [details[0], details[0], details[0] + 4, details[0]]
    );
    // The left-herdr flag's one line (85 columns) is wider than EVENT_MAX: the box takes its
    // 80 columns.
    let (line, details) = open_details(&Ok(chat_fixture()), 512, 160, 70);
    assert!(details.len() > 1, "{details:?}");
    assert!(centred(line, details[0], 80), "{line:?} {details:?}");
    assert!(details.iter().all(|&d| d == details[0]), "{details:?}");
}

#[test]
fn a_failures_line_wraps_in_a_narrow_chat_and_still_opens_by_keys_and_click() {
    // 40 wide the chat takes the pane and its events 36 columns, fewer than the failure's
    // line (41 shut, 46 open): what it says fills a centred line and its link a second.
    let view = spaced_failure_view();
    let (w, h) = (40, 60);
    let a = area(w, h);
    let mut dash = on_t3(&view, a);
    let drawn = |dash: &mut Dash| {
        dash.fit(&view, a);
        let buf = render_dash(&view, dash, w, h, Depth::TrueColor);
        assert_exact_width(&buf);
        let b = board_of(&view, dash, a);
        assert_eq!(b.conv.width, w);
        let conv = columns(&buf, b.conv.x, b.conv.width);
        let all = conv.join("\n");
        let at = (conv.iter())
            .position(|r| r.trim() == "delivery failed to 2 recipients ·")
            .unwrap_or_else(|| panic!("{all}"));
        let lines: Vec<String> = (conv[at..at + 3].iter())
            .map(|r| r.trim_end().to_string())
            .collect();
        (b, at, lines, all)
    };
    let (b, _, lines, _) = drawn(&mut dash);
    assert_eq!(
        lines[..2],
        [
            format!("{}delivery failed to 2 recipients ·", " ".repeat(3)),
            format!("{}details", " ".repeat(16)),
        ]
    );
    // Both lines open and fold it; the keys reach it once, as one message.
    let toggles: Vec<usize> = (b.toggles.iter())
        .filter(|(_, id)| *id == 9102)
        .map(|(l, _)| *l)
        .collect();
    assert_eq!(toggles.len(), 2, "{toggles:?}");
    assert_eq!(toggles[1], toggles[0] + 1);
    assert_eq!(b.starts.iter().filter(|(id, _)| *id == 9102).count(), 1);
    // Into the chat at its newest, the failure, then → opens it.
    dash.handle(&view, a, Input::Right);
    assert_eq!(dash.board_message(), Some(9102));
    dash.handle(&view, a, Input::Right);
    assert!(dash.open_messages().contains(&9102));
    let (b, at, lines, all) = drawn(&mut dash);
    assert_eq!(lines[1], format!("{}hide details", " ".repeat(14)), "{all}");
    // Its details under it in the box the events' width, two columns in.
    assert!(
        lines[2].starts_with("  @manager could not deliver to 2"),
        "{all}"
    );
    // A click on its link's line folds it, and one on what it says opens it again (the
    // rows are the screen's, from its top).
    let y = u16::try_from(at + 1).unwrap();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.conv.x + 20,
            y,
        },
    );
    assert!(!dash.open_messages().contains(&9102), "{all}");
    let (b, at, _, all) = drawn(&mut dash);
    let y = u16::try_from(at).unwrap();
    dash.handle(
        &view,
        a,
        Input::Click {
            x: b.conv.x + 20,
            y,
        },
    );
    assert!(dash.open_messages().contains(&9102), "{all}");
}

#[test]
fn a_needs_you_strip_row_folds_its_questions_runs_of_spaces() {
    // T4's question (402) written with runs of spaces between its words.
    let mut board = board_fixture();
    let t = (board.milestones[0].threads.iter_mut())
        .find(|t| t.id == ThreadId(4))
        .unwrap();
    let mut messages = t.messages.clone();
    let q = messages.iter_mut().find(|m| m.id == 402).unwrap();
    q.text = "question:  the   covers are late again.    @manager can you call the printer?".into();
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    let view: View = Ok(board);
    let a = area(160, 50);
    // The overview's strip is in its right column, where the row is cut in "call".
    let folded = "cat-lead question: the covers are late again. @manager";
    let mut on_board = select(Item::Task(TaskId(14)));
    on_board.handle(&view, a, Input::Char('b'));
    let overview = screen(&view, &select(Item::Milestone(MilestoneId(1))), a);
    let board = screen(&view, &on_board, a);
    for all in [&overview, &board] {
        let strip = (all.iter())
            .position(|r| r.contains("⚠ Needs you · 1"))
            .unwrap_or_else(|| panic!("{all:#?}"));
        // As the signed design's (UI v0.1.13) nowrap row: each run is one space.
        assert!(all[strip + 1].contains(folded), "{all:#?}");
    }
    // The bubble still keeps them, as the signed design's (UI v0.1.13) pre-wrap body does.
    let all = board.join("\n");
    assert!(
        all.contains("▌ question:  the   covers are late again.    @manager"),
        "{all}"
    );
}

#[test]
fn the_reply_line_where_it_stands_and_an_inbox_row_fold_a_questions_runs_of_spaces() {
    // T4's question (402) written with runs of spaces between its words.
    let mut board = board_fixture();
    let t = (board.milestones[0].threads.iter_mut())
        .find(|t| t.id == ThreadId(4))
        .unwrap();
    let mut messages = t.messages.clone();
    let q = messages.iter_mut().find(|m| m.id == 402).unwrap();
    q.text = "question:  the   covers are late again.    @manager can you call the printer?".into();
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    with_alerts(&mut board, &[]);
    let view: View = Ok(board);
    let a = area(160, 50);
    let folded = "cat-lead: question: the covers are late again. @manager can you call";
    // Replying to it from the strip: on the board, the reply line and where T4 stands, in
    // its header and its row in the thread list.
    let mut dash = select(Item::Milestone(MilestoneId(1)));
    let all = screen(&view, &dash, a);
    let y = find_row_containing(&all, "⚠ Needs you · 1") + 1;
    // In the overview's strip, right of the tree.
    let x = all[y][..all[y].rfind("cat-lead").unwrap()].width() as u16;
    dash.handle(&view, a, Input::Click { x, y: y as u16 });
    assert_eq!(dash.replying(), Some(402));
    let all = screen(&view, &dash, a);
    // As the signed design's (UI v0.1.13) nowrap lines: each run is one space.
    let reply = find_row_containing(&all, "replying to cat-lead");
    assert!(
        all[reply].contains(&format!(" replying to {folded}")),
        "{all:#?}"
    );
    let head = find_row_containing(&all, "where it stands");
    assert!(
        all[head].contains(&format!("where it stands {folded}")),
        "{all:#?}"
    );
    // The list's row, left of the conversation, is narrower: its start.
    assert!(
        (all.iter()).any(|r| r
            .split('│')
            .next()
            .unwrap()
            .contains("   cat-lead: question: the covers are")),
        "{all:#?}"
    );
    // The bubble still keeps them, as the signed design's (UI v0.1.13) pre-wrap body does.
    let all = all.join("\n");
    assert!(
        all.contains("▌ question:  the   covers are late again.    @manager"),
        "{all}"
    );
    // Its inbox row, as the signed design's (UI v0.1.13) normal row: each run is one space.
    let all = screen(&view, &inbox_dash(&view, a), a);
    let row = find_row_containing(&all, "question:");
    assert!(
        all[row].contains("question: the covers are late again. @manager can you call"),
        "{all:#?}"
    );
}

#[test]
fn where_it_stands_tones_a_leading_tag_as_its_bubble_does_on_the_header_and_the_list() {
    // T4 gets a newest message from cat-lead, starting with each leading tag in turn, on its
    // full board beside the list, with whether its tag is bold. Each round's message has its
    // own id: a message never changes, so the board keeps a conversation's bubbles by its
    // messages' ids.
    let tags = [
        (
            "decision:",
            "decision: the covers come from the second printer. @manager for your awareness",
            true,
        ),
        (
            "blocked:",
            "blocked: the second printer is out of ink too",
            true,
        ),
        (
            "question:",
            "question: the covers are late again. @manager can you call the printer?",
            false,
        ),
        // The owner mentioned on a later line: for your awareness, as its bubble takes it.
        (
            "decision:",
            "decision: the spine stays matte\n@manager for your awareness",
            true,
        ),
        // Not for your awareness (no mention of the owner): plain, as its bubble.
        ("decision:", "decision: the covers stay blue", false),
    ];
    for (id, (tag, text, bold_tag)) in (410..).zip(tags) {
        let mut board = board_fixture();
        let t = (board.milestones[0].threads.iter_mut())
            .find(|t| t.id == ThreadId(4))
            .unwrap();
        let mut messages = t.messages.clone();
        messages.push(Message {
            id,
            author: "cat-lead".into(),
            text: text.into(),
            at: NOW - 5 * 60,
        });
        *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
        with_alerts(&mut board, &[]);
        let view: View = Ok(board);
        let a = area(160, 50);
        let mut dash = select(Item::Task(TaskId(14)));
        dash.handle(&view, a, Input::Char('b'));
        for depth in [Depth::TrueColor, Depth::Ansi256] {
            let buf = render_dash(&view, &dash, 160, 50, depth);
            let all = rows(&buf);
            // The column and cell where `needle` first starts in row `y`, `skip` columns in.
            let at = |y: usize, needle: &str, skip: &str| {
                let x = all[y][..all[y].find(needle).unwrap()].width() + skip.width();
                (x, buf[(x as u16, y as u16)].clone())
            };
            // Its bubble line: the last with the tag, as the newest message is drawn last
            // (dev-1's blocked: and cat-lead's question: are above it).
            let bubble = (all.iter())
                .rposition(|r| r.contains(&format!("▌ {tag}")))
                .unwrap_or_else(|| panic!("{all:#?}"));
            let first = text.lines().next().unwrap();
            assert!(all[bubble].contains(&format!("▌ {first}")), "{all:#?}");
            let (_, want) = at(bubble, &format!("▌ {tag}"), "▌ ");
            let bold = |c: &ratatui::buffer::Cell| c.modifier.contains(Modifier::BOLD);
            assert_eq!(bold(&want), bold_tag, "{text:?} bubble {depth:?}");
            let said = format!("cat-lead: {tag}");
            // The header's line, right of the list's bar, by its lead-in; the list's row,
            // left of it. T4 is the newest thread, so its list row can share the header's
            // screen row.
            let stands = format!("where it stands {said}");
            let head = find_row_containing(&all, &stands);
            let list = (all.iter())
                .position(|r| r.split('│').next().unwrap().contains(&said))
                .unwrap_or_else(|| panic!("{all:#?}"));
            let bar = |y: usize| all[y][..all[y].find('│').unwrap()].width();
            let lead_in = "where it stands cat-lead: ";
            for (y, needle, skip, place) in [
                (head, stands.as_str(), lead_in, "header"),
                (list, said.as_str(), "cat-lead: ", "list"),
            ] {
                let (x, got) = at(y, needle, skip);
                assert_eq!(x > bar(y), place == "header", "{place} {all:#?}");
                assert_eq!(got.symbol(), &tag[..1], "{tag} {place} {depth:?}");
                // As its bubble: in the bubble's colour, bold exactly when the round says.
                assert_eq!(got.fg, want.fg, "{text:?} {place} {depth:?}");
                assert_eq!(bold(&got), bold_tag, "{text:?} {place} {depth:?}");
            }
        }
    }
}

#[test]
fn an_indented_first_line_is_classed_and_toned_by_its_raw_text_everywhere() {
    // T4 gets a newest message from cat-lead whose first line is indented, as a post now
    // keeps it (#16), starting with each leading tag in turn after the indent. The signed
    // design (UI v0.1.14) reads the raw text, so none starts with its tag: no tag is toned
    // or shown, and each is classed by the build's rule on the raw text (it mentions the
    // owner, so it is for your awareness, never a question).
    let tags = [
        (
            "blocked:",
            "   blocked: the second printer is out of ink too. @manager for your awareness",
        ),
        (
            "decision:",
            "   decision: the covers come from the second printer. @manager for your awareness",
        ),
        (
            "question:",
            "   question: the spine is late too. @manager can you call the binder?",
        ),
    ];
    for (id, (tag, text)) in (430..).zip(tags) {
        let mut board = board_fixture();
        let t = (board.milestones[0].threads.iter_mut())
            .find(|t| t.id == ThreadId(4))
            .unwrap();
        let mut messages = t.messages.clone();
        messages.push(Message {
            id,
            author: "cat-lead".into(),
            text: text.into(),
            at: NOW - 5 * 60,
        });
        *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
        with_alerts(&mut board, &[]);
        let view: View = Ok(board);
        let a = area(160, 50);
        let bold = |c: &ratatui::buffer::Cell| c.modifier.contains(Modifier::BOLD);
        let mut dash = select(Item::Task(TaskId(14)));
        dash.handle(&view, a, Input::Char('b'));
        let buf = render_dash(&view, &dash, 160, 50, Depth::TrueColor);
        let all = rows(&buf);
        // The column and cell where `needle` first starts in row `y`, `skip` columns in.
        let at = |y: usize, needle: &str, skip: &str| {
            let x = all[y][..all[y].find(needle).unwrap()].width() + skip.width();
            (x, buf[(x as u16, y as u16)].clone())
        };
        // The alert line, the overview's top row (the full board has none), and the strip:
        // T4's question (402) still the only need-you item, this one for your awareness.
        let overview = rows(&render(&view, 160, 50));
        assert!(
            overview[0].starts_with(" ⚠ 1 need you · ⓘ 1 for your awareness"),
            "{text:?} {overview:#?}"
        );
        assert!(
            (all.iter()).any(|r| r.contains("⚠ Needs you · 1")),
            "{text:?} {all:#?}"
        );
        // Its bubble keeps the indent, its tag plain and no tag on its meta line above.
        let bubble = (all.iter())
            .rposition(|r| r.contains(&format!("▌    {tag}")))
            .unwrap_or_else(|| panic!("{text:?} {all:#?}"));
        let (_, got) = at(bubble, &format!("▌    {tag}"), "▌    ");
        assert_eq!(got.symbol(), &tag[..1], "{text:?}");
        assert!(!bold(&got), "{text:?} bubble");
        let meta = &all[bubble - 1];
        assert!(meta.contains("5m"), "{text:?} {all:#?}");
        for word in ["needs you", "decision", "blocked"] {
            assert!(!meta.contains(word), "{text:?} {word} {all:#?}");
        }
        // Where it stands, on the header and the list row, its spaces folded: plain.
        let said = format!("cat-lead: {tag}");
        let stands = format!("where it stands {said}");
        let head = find_row_containing(&all, &stands);
        let list = (all.iter())
            .position(|r| r.split('│').next().unwrap().contains(&said))
            .unwrap_or_else(|| panic!("{text:?} {all:#?}"));
        for (y, needle, skip, place) in [
            (
                head,
                stands.as_str(),
                "where it stands cat-lead: ",
                "header",
            ),
            (list, said.as_str(), "cat-lead: ", "list"),
        ] {
            let (_, got) = at(y, needle, skip);
            assert_eq!(got.symbol(), &tag[..1], "{text:?} {place}");
            assert!(!bold(&got), "{text:?} {place}");
        }
        // Its inbox row, under FOR YOUR AWARENESS, its spaces folded: plain.
        let inbox = inbox_dash(&view, a);
        let buf = render_dash(&view, &inbox, 160, 50, Depth::TrueColor);
        let all = rows(&buf);
        let need = find_row_containing(&all, " ⚠ 1 NEED YOU");
        let aware = find_row_containing(&all, " ⓘ 1 FOR YOUR AWARENESS");
        // Its first words, which no other item starts with (T4's question is in NEED YOU).
        let folded = &text.trim_start()[..20];
        let row = find_row_containing(&all, folded);
        assert!(need < aware && aware < row, "{text:?} {all:#?}");
        let x = all[row][..all[row].find(folded).unwrap()].width();
        let got = &buf[(x as u16, row as u16)];
        assert_eq!(got.symbol(), &tag[..1], "{text:?}");
        assert!(!bold(got), "{text:?} inbox");
    }
}

#[test]
fn a_task_detail_tones_its_doing_lines_leading_tag_as_its_bubble_does() {
    // #14's doing post is T4's newest message, by cat-lead, starting with each leading tag
    // in turn, with whether its tag is bold. Each round's message has its own id, as in
    // the where-it-stands test.
    let tags = [
        (
            "decision:",
            "decision: the covers come from the second printer. @manager for your awareness",
            true,
        ),
        (
            "blocked:",
            "blocked: the second printer is out of ink too",
            true,
        ),
        (
            "question:",
            "question: the covers are late again. @manager can you call the printer?",
            false,
        ),
        // The owner mentioned on a later line: for your awareness, as its bubble takes it.
        (
            "decision:",
            "decision: the spine stays matte\n@manager for your awareness",
            true,
        ),
        // Not for your awareness (no mention of the owner): plain, as its bubble.
        ("decision:", "decision: the covers stay blue", false),
    ];
    for (id, (tag, text, bold_tag)) in (420..).zip(tags) {
        let mut board = board_fixture();
        let post = Message {
            id,
            author: "cat-lead".into(),
            text: text.into(),
            at: NOW - 5 * 60,
        };
        let t = (board.milestones[0].threads.iter_mut())
            .find(|t| t.id == ThreadId(4))
            .unwrap();
        let mut messages = t.messages.clone();
        messages.push(post.clone());
        *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
        let task = (board.milestones[0].tasks.iter_mut())
            .find(|t| t.id == TaskId(14))
            .unwrap();
        task.details.doing = Some(post);
        with_alerts(&mut board, &[]);
        let view: View = Ok(board);
        let a = area(160, 50);
        let mut detail = select(Item::Task(TaskId(14)));
        detail.handle(&view, a, Input::Enter);
        assert_eq!(detail.view(), ViewKind::TaskDetail);
        let mut full = select(Item::Task(TaskId(14)));
        full.handle(&view, a, Input::Char('b'));
        let bold = |c: &ratatui::buffer::Cell| c.modifier.contains(Modifier::BOLD);
        for depth in [Depth::TrueColor, Depth::Ansi256] {
            // The cell where `needle` first starts on the screen, `skip` columns in.
            let at = |dash: &Dash, needle: &str, skip: &str| {
                let buf = render_dash(&view, dash, 160, 50, depth);
                let all = rows(&buf);
                let y = (all.iter())
                    .rposition(|r| r.contains(needle))
                    .unwrap_or_else(|| panic!("{needle:?} {all:#?}"));
                let x = all[y][..all[y].find(needle).unwrap()].width() + skip.width();
                buf[(x as u16, y as u16)].clone()
            };
            // Its bubble line: the last with the tag, as the newest message is drawn last.
            let want = at(&full, &format!("▌ {tag}"), "▌ ");
            assert_eq!(bold(&want), bold_tag, "{text:?} bubble {depth:?}");
            let got = at(&detail, &format!("doing      {tag}"), "doing      ");
            assert_eq!(got.symbol(), &tag[..1], "{text:?} {depth:?}");
            // As its bubble: in the bubble's colour, bold exactly when the round says.
            assert_eq!(got.fg, want.fg, "{text:?} detail {depth:?}");
            assert_eq!(bold(&got), bold_tag, "{text:?} detail {depth:?}");
        }
    }
}

#[test]
fn an_inbox_row_shows_its_message_across_its_lines_folded_and_clamped_to_two() {
    // T4's question (402) on three lines, with runs of spaces between its words.
    let mut board = board_fixture();
    let t = (board.milestones[0].threads.iter_mut())
        .find(|t| t.id == ThreadId(4))
        .unwrap();
    let mut messages = t.messages.clone();
    let q = messages.iter_mut().find(|m| m.id == 402).unwrap();
    q.text = "question: the covers are late again.\n@manager can you call the printer?\n\
              The  shop says Friday,   then Monday."
        .into();
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    with_alerts(&mut board, &[]);
    let view: View = Ok(board);
    // As the signed design's (UI v0.1.13) row (`.txt clamp2`, `white-space: normal`): its
    // lines join with a space, each run of spaces is one, and it wraps at its words, the
    // second line under the first's text, on two lines at most: the second ends in "…"
    // right after its last word when the message goes on.
    for (w, first, second) in [
        (
            160,
            "question: the covers are late again. @manager can you call the printer? The shop \
             says Friday,",
            "then Monday.",
        ),
        (
            110,
            "question: the covers are late again. @manager can you call",
            "the printer? The shop says Friday, then Monday.",
        ),
        (
            80,
            "question: the covers are late again.",
            "@manager can you call the printer? The…",
        ),
    ] {
        let a = area(w, 24);
        let buf = render_dash(&view, &inbox_dash(&view, a), w, 24, Depth::TrueColor);
        assert_exact_width(&buf);
        let all = rows(&buf);
        let row = find_row_containing(&all, "question:");
        // Its author, then the message's first line, with its repo, thread and age at the
        // right.
        assert!(
            all[row].starts_with(&format!("   cat-lead  {first} ")),
            "w={w} {all:#?}"
        );
        assert!(all[row].trim_end().ends_with(" · 15m"), "w={w} {all:#?}");
        assert_eq!(
            all[row + 1].trim_end(),
            format!("{:13}{second}", ""),
            "w={w} {all:#?}"
        );
        assert_eq!(all[row + 2].trim_end(), "", "w={w} {all:#?}");
        // The mention on the second line is marked, and the item, selected, is selected
        // on both its lines.
        let y = row as u16 + 1;
        if w == 80 {
            assert_eq!(buf[(13, y)].fg, bad(Depth::TrueColor));
        }
        assert_eq!(buf[(1, y)].bg, selected_bg(Depth::TrueColor));
    }
}

#[test]
fn an_inbox_rows_clamp_ellipsis_is_plain_text_when_it_cuts_a_mention() {
    // At 40 columns 502's second line is full and ends inside "@manager": as the signed
    // design's (UI v0.1.13) clamp draws it, the mention's piece is red and bold and the "…"
    // after it is the row's plain text.
    let view: View = Ok(alerts_fixture());
    let a = area(40, 24);
    let buf = render_dash(&view, &inbox_dash(&view, a), 40, 24, Depth::TrueColor);
    let all = rows(&buf);
    let y = find_row_containing(&all, "book per row. @manage…") as u16;
    let x = all[usize::from(y)]
        .find('…')
        .map(|b| all[usize::from(y)][..b].width())
        .unwrap() as u16;
    assert_eq!(x, 39, "{all:#?}");
    let e = &buf[(x - 1, y)];
    assert_eq!((e.symbol(), e.fg), ("e", bad(Depth::TrueColor)));
    assert!(e.modifier.contains(Modifier::BOLD));
    let dots = &buf[(x, y)];
    assert_eq!((dots.symbol(), dots.fg), ("…", Color::Reset));
    assert!(!dots.modifier.contains(Modifier::BOLD));
}

#[test]
fn a_two_line_inbox_item_comes_whole_into_view_when_selected() {
    let view: View = Ok(alerts_fixture());
    // Eight list lines: the last item (310) takes the eighth and the ninth.
    let a = area(120, 11);
    let mut dash = inbox_dash(&view, a);
    for _ in 0..3 {
        dash.handle(&view, a, Input::Down);
    }
    assert_eq!(dash.inbox_selected(view.as_ref().unwrap()), Some(310));
    assert_eq!(dash.inbox_scroll(), 1);
    let all = screen(&view, &dash, a);
    assert!(all[7].contains("@manager for"), "{all:#?}");
    assert_eq!(all[8].trim_end(), format!("{:18}your awareness", ""));
    assert!(all[10].contains(" · r mark read · "), "{all:#?}");
    // Back up to the first item, two lines too, both still in view: the list stays.
    for _ in 0..3 {
        dash.handle(&view, a, Input::Up);
    }
    assert_eq!(dash.inbox_selected(view.as_ref().unwrap()), Some(402));
    assert_eq!(dash.inbox_scroll(), 1);
    let all = screen(&view, &dash, a);
    assert!(all[1].contains("@manager can you call the"), "{all:#?}");
    assert_eq!(all[2].trim_end(), format!("{:18}printer?", ""));
}

/// On a short inbox, paging across the gap between the sections never leaves only the
/// selected item's second line in view: PgUp, PgDn and the arrow keys keep its first line
/// in view at any height, and paging still reaches both ends. From five list lines up the
/// list pages as before, and the wheel scrolls freely at any height.
#[test]
fn a_selected_inbox_items_first_line_stays_in_view_at_any_height() {
    let view: View = Ok(alerts_fixture());
    let board = view.as_ref().unwrap();
    let ids: Vec<i64> = board.inbox.all().map(|a| a.message.id).collect();
    // At 40 columns every item takes two lines: NEED YOU, 402, 501, a blank, FOR YOUR
    // AWARENESS, 502, 310. A pane under five rows has no rule or keys line, so the list is
    // all of it but the top line; from five rows it is three rows shorter. Panes 2 to 8 rows
    // high give lists 1 to 5 lines high (2 and 3 lines twice, with and without the keys).
    let mut tested = BTreeSet::new();
    for pane_h in 2..=8u16 {
        let a = area(40, pane_h);
        let mut dash = inbox_dash(&view, a);
        let ib = layout(&view, &dash, a).inbox.unwrap();
        let list_h = ib.list.height;
        tested.insert(list_h);
        assert_eq!(ib.rows.len(), 11, "{:?}", ib.rows);
        let h = usize::from(list_h);
        let last_top = ib.rows.len() - h;
        let top = |dash: &Dash| dash.inbox_scroll().min(last_top);
        let first_in_view = |dash: &Dash, what: &str| {
            let id = dash.inbox_selected(board);
            let first = ib.rows.iter().position(|r| *r == id).unwrap();
            let top = top(dash);
            assert!(
                (top..top + h).contains(&first),
                "pane {pane_h}, list {list_h}, {what}: {id:?}'s first line {first} out of view at {top}\n{:#?}",
                screen(&view, dash, a)
            );
        };
        let page = h.saturating_sub(1).max(1);
        for (input, n) in [(Input::PageDown, 12), (Input::PageUp, 12)] {
            for k in 0..n {
                let before = top(&dash);
                dash.handle(&view, a, input);
                first_in_view(&dash, &format!("{input:?} {k}"));
                if list_h == 5 {
                    let paged = if input == Input::PageDown {
                        (before + page).min(last_top)
                    } else {
                        before.saturating_sub(page)
                    };
                    assert_eq!(top(&dash), paged, "pane {pane_h}, {input:?} {k}");
                }
            }
            // Paging never sticks: it gets to the end item's first line.
            let end = if input == Input::PageDown {
                ids.last()
            } else {
                ids.first()
            };
            let first = ib.rows.iter().position(|r| r.as_ref() == end).unwrap();
            let top = top(&dash);
            assert!(
                (top..top + h).contains(&first),
                "pane {pane_h}, list {list_h}, {input:?}: stuck at {top}"
            );
        }
        for input in [Input::Down; 4].into_iter().chain([Input::Up; 4]) {
            dash.handle(&view, a, input);
            first_in_view(&dash, &format!("{input:?}"));
        }
        // The wheel scrolls three lines a turn, wherever that leaves the items.
        let (x, y) = (ib.list.x + 4, ib.list.y);
        for down in [true; 4].into_iter().chain([false; 4]) {
            let before = top(&dash);
            dash.handle(&view, a, Input::Wheel { x, y, down });
            let wheeled = if down {
                (before + 3).min(last_top)
            } else {
                before.saturating_sub(3)
            };
            assert_eq!(
                top(&dash),
                wheeled,
                "pane {pane_h}, list {list_h}, wheel down {down}"
            );
        }
    }
    assert_eq!(tested, (1..=5).collect::<BTreeSet<u16>>());
}

/// On a short inbox the wheel can scroll every item's first line out of view: a list of 2
/// showing an item's second line and the gap, a list of 1 a head or only the selected
/// item's second line. Then nothing is highlighted, the keys line offers no Enter or `r`,
/// and both do nothing, so they never act on an item the owner cannot see. Wherever an
/// item's first line shows, the selection is on one whose first line shows, highlighted,
/// and Enter opens it. The wheel still scrolls three lines a turn.
#[test]
fn enter_and_r_never_act_on_an_inbox_item_the_wheel_scrolled_out_of_view() {
    let view: View = Ok(alerts_fixture());
    let board = view.as_ref().unwrap();
    let ids: Vec<i64> = board.inbox.all().map(|a| a.message.id).collect();
    // At 40 columns every item takes two lines: NEED YOU, 402, 501, a blank, FOR YOUR
    // AWARENESS, 502, 310. Panes 2 to 8 rows high give lists 1 to 5 lines high; from five
    // rows the pane has a keys line.
    let mut hidden = BTreeSet::new();
    for pane_h in 2..=8u16 {
        let a = area(40, pane_h);
        // Each start: the selection moved down `k` items and back up one, so the list sits
        // at another scroll before the wheel turns.
        for k in 0..4 {
            let mut dash = inbox_dash(&view, a);
            for input in std::iter::repeat_n(Input::Down, k).chain([Input::Up]) {
                dash.handle(&view, a, input);
            }
            let ib = layout(&view, &dash, a).inbox.unwrap();
            let h = usize::from(ib.list.height);
            let last_top = ib.rows.len() - h;
            let (x, y) = (ib.list.x + 4, ib.list.y);
            for down in [true; 4].into_iter().chain([false; 4]) {
                let before = dash.inbox_scroll().min(last_top);
                dash.handle(&view, a, Input::Wheel { x, y, down });
                let top = dash.inbox_scroll().min(last_top);
                let wheeled = if down {
                    (before + 3).min(last_top)
                } else {
                    before.saturating_sub(3)
                };
                assert_eq!(top, wheeled, "pane {pane_h}, k {k}, wheel down {down}");
                let what = format!("pane {pane_h}, list {h}, k {k}, top {top}");
                let shown = top..top + h;
                let first = |id: &i64| ib.rows.iter().position(|r| *r == Some(*id)).unwrap();
                let selected = dash.inbox_selected(board).unwrap();
                let buf = render_dash(&view, &dash, a.width, a.height, Depth::TrueColor);
                // The list lines highlighted, by their place in the list.
                let lit: Vec<usize> = (0..h)
                    .filter(|l| buf[(1, ib.list.y + *l as u16)].bg == selected_bg(Depth::TrueColor))
                    .map(|l| top + l)
                    .collect();
                if shown.contains(&first(&selected)) {
                    assert!(lit.contains(&first(&selected)), "{what}: {lit:?}");
                    let thread = (board.inbox.all())
                        .find(|i| i.message.id == selected)
                        .unwrap()
                        .thread;
                    let mut enter = dash.clone();
                    enter.handle(&view, a, Input::Enter);
                    assert_eq!(enter.board_thread_wanted(), Some(thread), "{what}");
                    continue;
                }
                // No item's first line is in view, or the selection would be on it.
                hidden.insert(h);
                assert!(
                    !ids.iter().any(|id| shown.contains(&first(id))),
                    "{what}: {selected} out of view\n{:#?}",
                    screen(&view, &dash, a)
                );
                assert!(lit.is_empty(), "{what}: {lit:?}");
                if pane_h >= 5 {
                    let keys = screen(&view, &dash, a)[usize::from(pane_h) - 1].clone();
                    let offered = ["enter open", "r mark", "r clear"];
                    assert!(!offered.iter().any(|k| keys.contains(k)), "{what}: {keys}");
                }
                for input in [Input::Enter, Input::Char('r')] {
                    let mut probe = dash.clone();
                    assert_eq!(probe.handle(&view, a, input), None, "{what}: {input:?}");
                    assert_eq!(probe.view(), ViewKind::Inbox, "{what}: {input:?}");
                    assert_eq!(probe.board_thread_wanted(), None, "{what}: {input:?}");
                    assert_eq!(probe.handing, None, "{what}: {input:?}");
                }
            }
        }
    }
    // Lists of 1 and 2 lines get there (and a list of 3, between the sections).
    assert!(hidden.contains(&1) && hidden.contains(&2), "{hidden:?}");
}

/// With nothing in NEED YOU the first awareness item's first line is the list's fifth (NEED
/// YOU, "nothing", a blank, FOR YOUR AWARENESS come first), so a list of four lines or
/// fewer opens scrolled to it, selected and highlighted, and Enter opens the item in view.
/// A list that shows its first line anyway opens at its top, as before.
#[test]
fn a_short_inbox_with_nothing_in_need_you_opens_on_its_first_item() {
    let mut board = alerts_fixture();
    // Nothing needs the owner: only the awareness items 502 and 310 are listed.
    board.inbox.need_you.clear();
    let view: View = Ok(board);
    let board = view.as_ref().unwrap();
    let thread = board.inbox.awareness[0].thread;
    for pane_h in 2..=10u16 {
        let a = area(40, pane_h);
        // Opened by `i`, and by a click on the alert line, which a pane shows from five rows.
        for open in [Input::Char('i'), Input::Click { x: 1, y: 0 }] {
            if open != Input::Char('i') && pane_h < 5 {
                continue;
            }
            let mut opened = clocked();
            opened.handle(&view, a, open);
            assert_eq!(opened.view(), ViewKind::Inbox, "pane {pane_h}, {open:?}");
            let ib = layout(&view, &opened, a).inbox.unwrap();
            assert_eq!(
                ib.rows[..6],
                [None, None, None, None, Some(502), Some(502)],
                "{:?}",
                ib.rows
            );
            let h = usize::from(ib.list.height);
            let top = opened.inbox_scroll().min(ib.rows.len() - h);
            let what = format!("pane {pane_h}, list {h}, {open:?}");
            assert_eq!(opened.inbox_selected(board), Some(502), "{what}");
            assert!(
                (top..top + h).contains(&4),
                "{what}: 502 out of view at {top}\n{:#?}",
                screen(&view, &opened, a)
            );
            if h >= 5 {
                assert_eq!(top, 0, "{what}");
            }
            let buf = render_dash(&view, &opened, a.width, a.height, Depth::TrueColor);
            let y = ib.list.y + (4 - top) as u16;
            assert_eq!(buf[(1, y)].bg, selected_bg(Depth::TrueColor), "{what}");
            opened.handle(&view, a, Input::Enter);
            assert_eq!(opened.board_thread_wanted(), Some(thread), "{what}");
        }
    }
}

/// A list of 1 line shows only row 0 at scroll 0, and row 0 is always the NEED YOU head, so
/// even with items in NEED YOU the inbox opens scrolled to its first item, selected and lit,
/// and Enter opens that item's thread.
#[test]
fn a_one_line_inbox_opens_on_its_first_need_you_item() {
    let view: View = Ok(alerts_fixture());
    let board = view.as_ref().unwrap();
    let a = area(40, 2);
    let mut dash = inbox_dash(&view, a);
    let ib = layout(&view, &dash, a).inbox.unwrap();
    assert_eq!(ib.list.height, 1);
    assert_eq!(ib.rows[..3], [None, Some(402), Some(402)], "{:?}", ib.rows);
    assert_eq!(dash.inbox_selected(board), Some(402));
    assert_eq!(dash.inbox_scroll(), 1, "{:#?}", screen(&view, &dash, a));
    let buf = render_dash(&view, &dash, a.width, a.height, Depth::TrueColor);
    assert_eq!(buf[(1, ib.list.y)].bg, selected_bg(Depth::TrueColor));
    let thread = (board.inbox.all())
        .find(|i| i.message.id == 402)
        .unwrap()
        .thread;
    dash.handle(&view, a, Input::Enter);
    assert_eq!(dash.board_thread_wanted(), Some(thread));
}

#[test]
fn every_piece_of_a_url_is_one_terminal_link_on_the_full_and_the_inline_board() {
    let view = mr_url_view();
    let id_of = |sym: &str| sym.split(';').nth(1).map(str::to_string);
    for (board, w, h) in [(true, 100u16, 50u16), (false, 160, 120)] {
        let mut dash = select(Item::Task(TaskId(13)));
        if board {
            dash.handle(&view, area(w, h), Input::Char('b'));
        }
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut plain = None;
        terminal
            .draw(|f| {
                let links = draw(f, &view, &dash, "14:32", Depth::TrueColor);
                plain = Some(f.buffer_mut().clone());
                hyperlink(f.buffer_mut(), &links);
            })
            .unwrap();
        let plain = plain.unwrap();
        let buf = terminal.backend().buffer();
        // Every cell of the URL's pieces shown, and only those, is a link to the URL, with one
        // id.
        let linked: Vec<(u16, u16)> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol().starts_with("\x1b]8;"))
            .collect();
        let shown: String = (linked.iter())
            .map(|&(x, y)| plain[(x, y)].symbol())
            .collect();
        // The inline board's preview shows a message's first two lines, so the URL's first
        // piece only; it is still a link to the whole URL.
        if board {
            assert_eq!(shown, MR_URL, "{:#?}", rows(&plain));
        } else {
            assert!(shown.len() > 20 && MR_URL.starts_with(&shown), "{shown:?}");
        }
        let ids: BTreeSet<_> = (linked.iter())
            .map(|&(x, y)| id_of(buf[(x, y)].symbol()))
            .collect();
        assert_eq!(ids.len(), 1, "board {board}");
        for &(x, y) in &linked {
            let s = buf[(x, y)].symbol();
            let c = plain[(x, y)].symbol();
            assert!(
                s.ends_with(&format!(";{MR_URL}\x1b\\{c}\x1b]8;;\x1b\\")),
                "{s:?}"
            );
        }
        // Underlined in the accent, as a link reads.
        let (x, y) = linked[0];
        assert_eq!(plain[(x, y)].fg, accent(Depth::TrueColor));
        assert!(plain[(x, y)].modifier.contains(Modifier::UNDERLINED));
    }
}

/// A URL that starts another, and two long URLs whose wrapped tails read the same, each in a
/// message of its own on T3.
fn look_alike_urls_view(urls: &[&str]) -> View {
    let mut board = board_fixture();
    let t = &mut board.milestones[0].threads[1];
    let mut messages = t.messages.clone();
    for (k, url) in urls.iter().enumerate() {
        messages.push(Message {
            id: 9100 + k as i64,
            author: if k % 2 == 0 { "dev-2" } else { "qa" }.into(),
            text: format!("see {url}"),
            at: NOW - 60 + k as i64,
        });
    }
    *t = Thread::whole(t.id, t.title.clone(), t.task, None, messages);
    sync(&mut board);
    Ok(board)
}

#[test]
fn each_url_links_exactly_the_cells_it_was_drawn_in() {
    let urls = [
        "https://a.b/c",
        "https://a.b/c/d",
        "https://gitlab1.example.com/acme/bookshop/web/bookmark-icons/-/merge_requests/31",
        "https://gitlab2.example.com/acme/bookshop/web/bookmark-icons/-/merge_requests/31",
    ];
    let view = look_alike_urls_view(&urls);
    let (w, h) = (100u16, 50u16);
    let mut dash = select(Item::Task(TaskId(13)));
    dash.handle(&view, area(w, h), Input::Char('b'));
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut plain = None;
    terminal
        .draw(|f| {
            let links = draw(f, &view, &dash, "14:32", Depth::TrueColor);
            plain = Some(f.buffer_mut().clone());
            hyperlink(f.buffer_mut(), &links);
        })
        .unwrap();
    let plain = plain.unwrap();
    let buf = terminal.backend().buffer();
    // For each URL, the cells linked to it, in reading order, and the rows they are on.
    let mut by_url: std::collections::BTreeMap<String, (String, BTreeSet<u16>)> =
        std::collections::BTreeMap::new();
    for y in 0..h {
        for x in 0..w {
            let s = buf[(x, y)].symbol();
            let Some(rest) = s.strip_prefix("\x1b]8;id=") else {
                continue;
            };
            let (_, rest) = rest.split_once(';').unwrap();
            let (url, _) = rest.split_once("\x1b\\").unwrap();
            let e = by_url.entry(url.to_string()).or_default();
            e.0.push_str(plain[(x, y)].symbol());
            e.1.insert(y);
        }
    }
    // Every URL's cells spell exactly that URL: none took another's cells, none lost any.
    for url in urls {
        let (text, on) = by_url
            .get(url)
            .unwrap_or_else(|| panic!("{url} not linked: {:#?}", rows(&plain)));
        assert_eq!(text, url, "{:#?}", rows(&plain));
        // The long two wrap, so each has a tail of its own on another line.
        if url.len() > 60 {
            assert_eq!(on.len(), 2, "{url}: {:#?}", rows(&plain));
        }
    }
    assert_eq!(by_url.len(), urls.len());
    // Their wrapped tails read the same, so a match by text alone could not tell them apart.
    // The text of the cells linked to `url` on the last row it is on.
    let tail = |url: &str| -> String {
        let y = *by_url[url].1.last().unwrap();
        let to = format!(";{url}\x1b\\");
        (0..w)
            .filter(|&x| buf[(x, y)].symbol().contains(&to))
            .map(|x| plain[(x, y)].symbol())
            .collect()
    };
    // The bubble's width decides where they break; they break alike, so their tails
    // are the same text.
    let (a, b) = (tail(urls[2]), tail(urls[3]));
    assert_eq!(a, b);
    assert!(
        a.len() > 10 && urls[2].ends_with(&a) && a.len() < urls[2].len(),
        "{a:?}"
    );
}
