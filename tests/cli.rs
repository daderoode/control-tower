//! `tower` as agents use it: a separate process per command, one board file.

use std::path::Path;
use std::process::{Command, Output};

use control_tower::store::Store;

#[path = "../src/test_exe.rs"]
mod test_exe;

fn tower(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tower"))
        .args(args)
        .env("TOWER_DB", db)
        .output()
        .unwrap()
}

/// This build's `tower`, to copy where a test runs it from (with `test_exe::executable`).
fn tower_bytes() -> Vec<u8> {
    std::fs::read(env!("CARGO_BIN_EXE_tower")).unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Puts milestone `m` in a new repo, whose main checkout is a new folder beside the board,
/// as `tower repo add` and `tower milestone set --repo` do, and returns the repo's id.
#[track_caller]
fn in_repo(db: &Path, m: &str) -> String {
    let checkout = db.with_file_name(format!("checkout-{m}"));
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    let out = tower(db, &["repo", "add", checkout.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let repo = text(&out.stdout).trim().to_string();
    let out = tower(db, &["milestone", "set", m, "--repo", &repo]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    repo
}

#[track_caller]
fn assert_refused(out: &Output, message: &str) {
    assert!(!out.status.success(), "expected a refusal, got success");
    let err = text(&out.stderr);
    assert!(err.contains(message), "stderr {err:?} lacks {message:?}");
    assert!(
        out.stdout.is_empty(),
        "a refusal printed to stdout: {:?}",
        text(&out.stdout)
    );
}

#[test]
fn creates_the_board_on_first_use_and_prints_ids() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fresh/board.db");
    // Each add prints the bare id on stdout, so a lead can capture it and pass it back,
    // and the sentence a person reads on stderr.
    let add = |args: &[&str], id: &str, sentence: &str| {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), format!("{id}\n"), "{args:?}");
        assert_eq!(text(&out.stderr), format!("{sentence}\n"), "{args:?}");
        id.to_string()
    };
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    let repo = add(
        &["repo", "add", checkout.to_str().unwrap()],
        "R1",
        "added repo R1",
    );
    assert!(db.exists());
    let m = add(
        &[
            "milestone",
            "add",
            "First shop pages",
            "--goal",
            "Usable.",
            "--repo",
            &repo,
        ],
        "M1",
        "added milestone M1 in R1",
    );
    let out = tower(
        &db,
        &["lead", "add", &m, "--name", "Lead", "--session", "lead"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let task = add(
        &["task", "add", "--milestone", &m, "Bookmarks page"],
        "#1",
        "added task #1 under M1",
    );
    add(
        &["task", "add", "Sort the shelves"],
        "#2",
        "added standalone task #2",
    );
    add(
        &["subtask", "add", "--task", &task, "New-book ribbon"],
        "#3",
        "added subtask #3 under #1; #1 is waiting, 0/1 subtasks done",
    );
    add(
        &[
            "task",
            "criterion",
            "add",
            &task,
            "Shows the 12 newest books",
        ],
        "1",
        "added criterion 1 to task #1",
    );
    let thread = add(
        &[
            "thread",
            "new",
            &m,
            "--by",
            "lead",
            "--task",
            &task,
            "Team",
            "Opening it.",
        ],
        "T1",
        "opened thread T1 on the board of R1 about #1",
    );
    let out = tower(&db, &["post", &thread, "--by", "dev-1", "copy"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn refuses_bad_input_with_a_message_and_non_zero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let ok = tower(&db, &["milestone", "add", "M", "--goal", "g"]);
    assert!(ok.status.success());

    let long = "x".repeat(201);
    let cases: [(&[&str], &str); 7] = [
        (
            &["milestone", "add", "  ", "--goal", "g"],
            "the milestone title is empty",
        ),
        (
            &["milestone", "add", "t", "--goal", "a\nb"],
            "the milestone goal contains a control character",
        ),
        (&["milestone", "add", "t"], "--goal"),
        (
            &["task", "add", "--milestone", "M1", &long],
            "the task title is 201 characters long",
        ),
        (
            &["task", "add", "--milestone", "M7", "t"],
            "there is no milestone M7 on the board",
        ),
        (
            &["task", "add", "--milestone", "7", "t"],
            "not a milestone id",
        ),
        (&["task", "add", "--milestone", "M1"], "<TITLE>"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
}

#[test]
fn refuses_an_empty_tower_db() {
    let out = Command::new(env!("CARGO_BIN_EXE_tower"))
        .args(["milestone", "add", "t", "--goal", "g"])
        .env("TOWER_DB", "")
        .output()
        .unwrap();
    assert_refused(&out, "TOWER_DB is set but empty");
}

#[test]
fn concurrent_agent_processes_all_succeed() {
    const AGENTS: usize = 6;
    const EACH: usize = 10;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let out = tower(&db, &["milestone", "add", "shared", "--goal", "g"]);
    assert!(out.status.success());
    let children: Vec<_> = (0..AGENTS)
        .map(|a| {
            let script = (0..EACH)
                .map(|i| format!("\"$0\" task add --milestone M1 'agent {a} task {i}' || exit 1"))
                .collect::<Vec<_>>()
                .join("\n");
            Command::new("/bin/sh")
                .args(["-c", &script, env!("CARGO_BIN_EXE_tower")])
                .env("TOWER_DB", &db)
                .stderr(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    let board = Store::open(&db).unwrap().load().unwrap();
    assert_eq!(board.milestones[0].tasks.len(), AGENTS * EACH);
}

#[test]
fn registers_leads_and_refuses_a_second_lead_either_way() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    for (m, title) in [("M1", "First shop pages"), ("M2", "Checkout")] {
        assert!(
            tower(&db, &["milestone", "add", title, "--goal", "g"])
                .status
                .success()
        );
        in_repo(&db, m);
    }
    let cat = &[
        "lead",
        "add",
        "M1",
        "--name",
        "Catalogue Lead",
        "--session",
        "cat-lead",
    ];
    let out = tower(&db, cat);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "added lead Catalogue Lead (session cat-lead, colour blue) for R1\n"
    );
    let out = tower(
        &db,
        &[
            "lead",
            "add",
            "M2",
            "--name",
            "Checkout",
            "--session",
            "checkout-lead",
            "--colour",
            "teal",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "added lead Checkout (session checkout-lead, colour teal) for R2\n"
    );

    let cases: [(&[&str], &str); 8] = [
        (cat, "R1 already has a lead, Catalogue Lead"),
        (
            &[
                "lead",
                "add",
                "M2",
                "--name",
                "Catalogue Lead",
                "--session",
                "x",
            ],
            "R2 already has a lead, Checkout",
        ),
        (
            &["lead", "add", "M3", "--name", "x", "--session", "y"],
            "there is no milestone M3 on the board",
        ),
        (
            &["lead", "add", "2", "--name", "x", "--session", "y"],
            "neither a repo id (like R2) nor a milestone id (like M2)",
        ),
        (
            &["lead", "add", "M1", "--name", " ", "--session", "y"],
            "the lead name is empty",
        ),
        (
            &[
                "lead",
                "add",
                "M1",
                "--name",
                "x",
                "--session",
                "y",
                "--colour",
                "red",
            ],
            "\"red\" is not a lead colour: pick one of blue, purple, orange, teal, pink, green",
        ),
        (&["lead", "add", "M1", "--name", "x"], "--session"),
        (&["lead", "add", "M1", "--session", "y"], "--name"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }

    // A milestone in no repo has no lead to register; a lead already on a repo is refused
    // on a new one, by name or by session.
    assert!(
        tower(&db, &["milestone", "add", "Third", "--goal", "g"])
            .status
            .success()
    );
    let out = tower(&db, &["lead", "add", "M3", "--name", "x", "--session", "y"]);
    assert_refused(&out, "M3 is in no repo yet; set its repo first");
    let r3 = in_repo(&db, "M3");
    for (name, session) in [("Catalogue Lead", "new"), ("New Lead", "cat-lead")] {
        let out = tower(
            &db,
            &["lead", "add", &r3, "--name", name, "--session", session],
        );
        assert_refused(&out, "a lead has one repo");
    }
    let board = Store::open(&db).unwrap().load().unwrap();
    assert_eq!(board.milestones[2].lead, None);
}

#[test]
fn changes_a_lead_and_refuses_what_add_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    for (m, title) in [("M1", "a"), ("M2", "b"), ("M3", "c")] {
        assert!(
            tower(&db, &["milestone", "add", title, "--goal", "g"])
                .status
                .success()
        );
        in_repo(&db, m);
    }
    for (m, name, session) in [
        ("M1", "Catalogue Lead", "cat-lead"),
        ("M2", "Checkout", "checkout-lead"),
    ] {
        let out = tower(
            &db,
            &["lead", "add", m, "--name", name, "--session", session],
        );
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    let out = tower(&db, &["lead", "set", "M1", "--session", "cat-lead-two"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "lead of R1 is now Catalogue Lead (session cat-lead-two, colour blue)\n"
    );
    let out = tower(
        &db,
        &["lead", "set", "M1", "--name", "Cat", "--colour", "teal"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "lead of R1 is now Cat (session cat-lead-two, colour teal)\n"
    );

    let cases: [(&[&str], &str); 7] = [
        (
            &["lead", "set", "M1"],
            "nothing to change: give --name, --session or --colour",
        ),
        (
            &["lead", "set", "M3", "--name", "x"],
            "R3 has no lead yet; add one with tower lead add R3",
        ),
        (
            &["lead", "set", "M1", "--name", "checkout"],
            "checkout already leads R2",
        ),
        (
            &["lead", "set", "M1", "--session", "Checkout-Lead"],
            "the session Checkout-Lead is already Checkout",
        ),
        (
            &["lead", "set", "M1", "--session", "a  b"],
            "has two spaces in a row",
        ),
        (
            &["lead", "set", "M1", "--colour", "red"],
            "\"red\" is not a lead colour",
        ),
        (
            &[
                "lead",
                "add",
                "M3",
                "--name",
                "x",
                "--session",
                "Cat-Lead-Two",
            ],
            "a lead has one repo",
        ),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
    let board = Store::open(&db).unwrap().load().unwrap();
    let lead = board.milestones[0].lead.as_ref().unwrap();
    assert_eq!(
        (lead.name.as_str(), lead.session.as_str()),
        ("Cat", "cat-lead-two")
    );
    assert_eq!(board.milestones[2].lead, None);
}

#[test]
fn upgrade_brings_an_older_board_up_to_date_and_keeps_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE milestone (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, \
         goal TEXT NOT NULL);
         CREATE TABLE task (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         milestone_id INTEGER REFERENCES milestone(id), title TEXT NOT NULL);
         INSERT INTO milestone (title, goal) VALUES ('old', 'g');
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);
    let problem = control_tower::ui::refresh(&db).unwrap_err();
    assert!(
        problem.ends_with("is schema version 1; run tower upgrade"),
        "{problem}"
    );

    let want = format!(
        "board {} is at schema version {}\n",
        db.display(),
        control_tower::store::SCHEMA_VERSION
    );
    for _ in 0..2 {
        // The second run finds it current and changes nothing.
        let out = tower(&db, &["upgrade"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), want);
    }
    let board = control_tower::ui::refresh(&db).unwrap();
    assert_eq!(board.milestones[0].title, "old");
    assert_eq!(board.milestones[0].lead, None);

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    assert_refused(
        &tower(&db, &["upgrade"]),
        "schema version 99, newer than this tower understands",
    );
}

#[test]
fn moves_tasks_through_stages_and_marks_a_milestone_done_only_when_every_task_is() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let run = |args: &[&str]| {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    };
    run(&["milestone", "add", "First shop pages", "--goal", "g"]);
    run(&["milestone", "add", "Empty", "--goal", "g"]);
    for t in ["one", "two", "three", "four", "five"] {
        run(&["task", "add", "--milestone", "M1", t]);
    }

    assert_eq!(
        run(&["task", "stage", "1", "in-progress"]),
        "task #1 is in progress; M1 is in progress, 0/5 tasks done (0%)\n"
    );
    // The id as shown, quoted.
    assert_eq!(
        run(&["task", "stage", "#1", "done"]),
        "task #1 is done; M1 is in progress, 1/5 tasks done (20%)\n"
    );
    assert_refused(
        &tower(&db, &["milestone", "done", "M1"]),
        "M1 is not done until every task is: #2 waiting, #3 waiting, #4 waiting, #5 waiting",
    );
    for t in ["2", "3", "4", "5"] {
        run(&["task", "stage", t, "done"]);
    }
    assert_eq!(run(&["milestone", "done", "M1"]), "M1 is done\n");
    // Moving a task back takes the Done mark back.
    assert_eq!(
        run(&["task", "stage", "3", "waiting"]),
        "task #3 is waiting; M1 is in progress, 4/5 tasks done (80%)\n"
    );
    assert_eq!(
        run(&["task", "stage", "3", "done"]),
        "task #3 is done; M1 is in progress, 5/5 tasks done (100%)\n"
    );
    assert_eq!(run(&["milestone", "done", "M1"]), "M1 is done\n");

    let cases: [(&[&str], &str); 9] = [
        (
            &["milestone", "done", "M2"],
            "M2 has no tasks, so it cannot be done",
        ),
        (&["milestone", "done", "M1"], "M1 is already done"),
        (
            &["milestone", "done", "M9"],
            "there is no milestone M9 on the board",
        ),
        (&["task", "stage", "3", "done"], "task #3 is already done"),
        (
            &["task", "stage", "9", "done"],
            "there is no task #9 on the board",
        ),
        (
            &["task", "stage", "1", "blocked"],
            "\"blocked\" is not a stage",
        ),
        (&["task", "stage", "1", "in progress"], "is not a stage"),
        (&["task", "stage", "M1", "done"], "\"M1\" is not a task id"),
        (&["task", "stage", "1"], "<STAGE>"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
}

#[test]
fn a_standalone_task_moves_through_stages_without_a_milestone() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "m", "--goal", "g"]);
    assert_eq!(ok(&db, &["task", "add", "loose"]), "#1\n");
    assert_refused(
        &tower(&db, &["task", "add", " "]),
        "the task title is empty",
    );
    assert_eq!(
        ok(&db, &["task", "stage", "1", "done"]),
        "task #1 is done\n"
    );
    let shown = ok(&db, &["task", "show", "1"]);
    assert!(
        shown.contains("  milestone  none (standalone)\n"),
        "{shown}"
    );
}

#[test]
fn hands_a_standalone_task_to_a_lead_and_refuses_what_cannot_be_handed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "Kids", "--goal", "g"]);
    ok(&db, &["milestone", "add", "Unled", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Kids Lead",
            "--session",
            "kids",
        ],
    );
    ok(&db, &["task", "add", "--milestone", "M1", "first"]);
    ok(&db, &["task", "stage", "1", "done"]);
    ok(&db, &["milestone", "done", "M1"]);
    ok(&db, &["task", "add", "Sort the shelves"]);
    ok(&db, &["subtask", "add", "--task", "2", "sand the shelf"]);

    let cases: [(&[&str], &str); 5] = [
        (&["task", "hand", "2", "M2"], "M2 has no lead yet"),
        (&["task", "hand", "2", "M9"], "there is no milestone M9"),
        (&["task", "hand", "3", "M1"], "#3 is a subtask of #2"),
        (&["task", "hand", "1", "M1"], "#1 is already under M1"),
        (&["task", "hand", "2"], "<MILESTONE>"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
    assert_eq!(
        ok(&db, &["task", "hand", "2", "M1"]),
        "handed task #2 to Kids Lead on M1; M1 is in progress, 1/2 tasks done (50%)\n"
    );
    let shown = ok(&db, &["milestone", "show", "M1"]);
    assert!(
        shown.contains("  #2 Sort the shelves (waiting, unassigned, no ticket)\n"),
        "{shown}"
    );
    let shown = ok(&db, &["task", "show", "2"]);
    assert!(shown.contains("  milestone  M1 Kids\n"), "{shown}");
    assert!(shown.contains("  #3 sand the shelf (waiting)\n"), "{shown}");
}

#[test]
fn adds_subtasks_one_level_deep_and_counts_them_apart_from_the_milestone() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let run = |args: &[&str]| {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    };
    run(&["milestone", "add", "First shop pages", "--goal", "g"]);
    run(&["task", "add", "--milestone", "M1", "New book shelf"]);
    run(&["task", "add", "--milestone", "M1", "Basket"]);
    assert_eq!(
        run(&["subtask", "add", "--task", "1", "New-book ribbon"]),
        "#3\n"
    );
    let out = tower(&db, &["subtask", "add", "--task", "#1", "Merge"]);
    assert_eq!(text(&out.stdout), "#4\n");
    assert_eq!(
        text(&out.stderr),
        "added subtask #4 under #1; #1 is waiting, 0/2 subtasks done\n"
    );
    assert_eq!(
        run(&["task", "stage", "3", "done"]),
        "task #3 is done; #1 is waiting, 1/2 subtasks done\n"
    );
    // The milestone counts its two tasks only.
    assert_eq!(
        run(&["task", "stage", "2", "done"]),
        "task #2 is done; M1 is in progress, 1/2 tasks done (50%)\n"
    );
    assert_refused(
        &tower(&db, &["milestone", "done", "M1"]),
        "M1 is not done until every task is: #1 waiting",
    );

    // A task waits for its subtasks, and a subtask reopening takes the task back.
    assert_refused(
        &tower(&db, &["task", "stage", "1", "done"]),
        "task #1 is not done until every subtask is: #4 waiting",
    );
    run(&["task", "stage", "4", "done"]);
    run(&["task", "stage", "1", "done"]);
    assert_eq!(run(&["milestone", "done", "M1"]), "M1 is done\n");
    assert_eq!(
        run(&["task", "stage", "4", "waiting"]),
        "task #4 is waiting; #1 is in progress, 1/2 subtasks done\n"
    );
    assert_refused(
        &tower(&db, &["milestone", "done", "M1"]),
        "M1 is not done until every task is: #1 in progress",
    );

    let cases: [(&[&str], &str); 4] = [
        (
            &["subtask", "add", "--task", "3", "deeper"],
            "#3 is a subtask of #1; a subtask has no subtasks of its own: ask the lead for a new task",
        ),
        (
            &["subtask", "add", "--task", "9", "orphan"],
            "there is no task #9 on the board",
        ),
        (
            &["subtask", "add", "--task", "1", ""],
            "the subtask title is empty",
        ),
        (&["subtask", "add", "orphan"], "--task"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
}

/// Runs `tower` and returns its stdout, failing the test on a refusal.
#[track_caller]
fn ok(db: &Path, args: &[&str]) -> String {
    let out = tower(db, args);
    assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    text(&out.stdout)
}

/// A board with M1 holding tasks #1 and #2, #3 a subtask of #1, and M2 empty.
fn detail_board(db: &Path) {
    ok(
        db,
        &["milestone", "add", "First shop pages", "--goal", "Usable."],
    );
    ok(db, &["milestone", "add", "Checkout", "--goal", "g"]);
    in_repo(db, "M1");
    ok(db, &["task", "add", "--milestone", "M1", "Bookmarks page"]);
    ok(db, &["task", "add", "--milestone", "M1", "New book shelf"]);
    ok(db, &["subtask", "add", "--task", "1", "Search API"]);
}

#[test]
fn sets_and_shows_task_details() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    let out = ok(
        &db,
        &[
            "task",
            "set",
            "2",
            "--description",
            "Show the newest books.\r\nSo a reader sees what came in.",
            "--context",
            "Use the sample catalogue.",
            "--link",
            "https://github.com/o/r/issues/13",
            "--mr",
            " !44 draft into feat/bookmarks · pipeline running ",
            "--assignee",
            " dev-2 ",
        ],
    );
    assert_eq!(out, "set the details of task #2\n");
    // A later set replaces only what it gives.
    ok(&db, &["task", "set", "#2", "--assignee", "dev-3"]);
    assert_eq!(
        ok(
            &db,
            &["task", "criterion", "add", "2", "Shows the 12 newest books"]
        ),
        "1\n"
    );
    ok(&db, &["task", "criterion", "add", "2", "Newest first"]);
    assert_eq!(
        ok(&db, &["task", "criterion", "check", "2", "1"]),
        "criterion 1 of task #2 is met: Shows the 12 newest books\n"
    );
    assert_eq!(
        ok(&db, &["task", "blocked-by", "add", "2", "1"]),
        "task #2 is blocked by #1\n"
    );
    let shown = ok(&db, &["task", "show", "2"]);
    assert_eq!(
        shown,
        "#2 New book shelf
  stage      waiting
  blocked    no
  milestone  M1 First shop pages
  assignee   dev-3
  doing      nothing posted yet
  blocked by #1
  MR         !44 draft into feat/bookmarks · pipeline running
  context    Use the sample catalogue.
  tracker    https://github.com/o/r/issues/13
description
  Show the newest books.
  So a reader sees what came in.
acceptance criteria
  1. [x] Shows the 12 newest books
  2. [ ] Newest first
subtasks
  none
threads
  none
"
    );
    assert_eq!(
        ok(&db, &["task", "criterion", "uncheck", "2", "1"]),
        "criterion 1 of task #2 is not met: Shows the 12 newest books\n"
    );
    assert_eq!(
        ok(&db, &["task", "blocked-by", "remove", "2", "1"]),
        "task #2 is no longer blocked by #1\n"
    );
    // Removing a criterion moves the ones after it up a number.
    ok(
        &db,
        &[
            "task",
            "criterion",
            "add",
            "2",
            "A ribbon on this week's books",
        ],
    );
    assert_eq!(
        ok(&db, &["task", "criterion", "remove", "2", "1"]),
        "removed criterion 1 of task #2: Shows the 12 newest books; \
         the criteria after it move up a number\n"
    );
    let shown = ok(&db, &["task", "show", "2"]);
    assert!(
        shown.contains("  1. [ ] Newest first\n  2. [ ] A ribbon on this week's books\nsubtasks"),
        "{shown}"
    );
    assert_eq!(
        ok(&db, &["task", "unassign", "2"]),
        "task #2 is unassigned\n"
    );
    assert!(ok(&db, &["task", "show", "2"]).contains("  assignee   unassigned\n"));
    let shown = ok(&db, &["task", "show", "1"]);
    assert!(shown.contains("  assignee   unassigned\n"), "{shown}");
    assert!(shown.contains("  blocked by nothing\n"), "{shown}");
    assert!(
        shown.contains("subtasks\n  #3 Search API (waiting)\n"),
        "{shown}"
    );
}

#[test]
fn refuses_a_leads_session_as_assignee_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(&db, &["task", "set", "2", "--assignee", "dev-1"]);
    let before = ok(&db, &["task", "show", "2"]);
    // The session as given, in another ASCII case, and padded: all the lead.
    for name in ["cat-lead", "Cat-Lead", " cat-lead "] {
        assert_refused(
            &tower(&db, &["task", "set", "2", "--assignee", name]),
            "cat-lead is the session of the lead Catalogue, and a lead is never a task's \
             assignee: assign the worker doing the step, or leave the task unassigned \
             between steps with tower task unassign 2",
        );
        assert_eq!(ok(&db, &["task", "show", "2"]), before, "{name}");
    }
    // Refused with the other fields too: nothing of the call is stored.
    assert_refused(
        &tower(
            &db,
            &["task", "set", "2", "--mr", "!9", "--assignee", "Cat-Lead"],
        ),
        "tower task unassign 2",
    );
    assert_eq!(ok(&db, &["task", "show", "2"]), before);
    // A worker's name, and one that only contains the lead's session, are still taken.
    for name in ["cat-lead-2", "dev-cat-lead", "dev-2"] {
        ok(&db, &["task", "set", "2", "--assignee", name]);
        let shown = ok(&db, &["task", "show", "2"]);
        assert!(shown.contains(&format!("  assignee   {name}\n")), "{shown}");
    }
}

#[test]
fn refuses_a_tasks_assignee_as_a_leads_session_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    in_repo(&db, "M2");
    ok(&db, &["task", "set", "1", "--assignee", "dev-one"]);
    let lead_add = |session: &str| {
        tower(
            &db,
            &[
                "lead",
                "add",
                "R2",
                "--name",
                "Checkout",
                "--session",
                session,
            ],
        )
    };
    // lead add, for a task in another repo: the session as given, in another ASCII case,
    // and padded, are all the assignee.
    let before = ok(&db, &["repo", "show", "R2"]);
    for session in ["dev-one", "Dev-One", " dev-one "] {
        assert_refused(
            &lead_add(session),
            &format!(
                "the session {} is the assignee of task #1, and a lead is never a task's \
                 assignee: unassign it first with tower task unassign 1",
                session.trim()
            ),
        );
        assert_eq!(ok(&db, &["repo", "show", "R2"]), before, "{session}");
    }
    // Several tasks, one of them Done (a Done task's assignee counts too):
    // each is named, with its unassign.
    ok(&db, &["task", "set", "2", "--assignee", "Dev-One"]);
    ok(&db, &["task", "stage", "2", "done"]);
    assert!(ok(&db, &["task", "show", "2"]).contains("  stage      done\n"));
    assert_refused(
        &lead_add("dev-one"),
        "the session dev-one is the assignee of tasks #1, #2, and a lead is never a task's \
         assignee: unassign them first with tower task unassign 1 and tower task unassign 2",
    );
    // lead set --session: refused the same way, with nothing of the call stored.
    ok(
        &db,
        &[
            "lead",
            "add",
            "R1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    let before = ok(&db, &["repo", "show", "R1"]);
    assert_refused(
        &tower(&db, &["lead", "set", "M1", "--session", "Dev-One"]),
        "the session Dev-One is the assignee of tasks #1, #2, and a lead is never a task's \
         assignee: unassign them first with tower task unassign 1 and tower task unassign 2",
    );
    assert_refused(
        &tower(
            &db,
            &[
                "lead",
                "set",
                "R1",
                "--name",
                "Renamed",
                "--session",
                "dev-one",
            ],
        ),
        "tower task unassign 1 and tower task unassign 2",
    );
    assert_eq!(ok(&db, &["repo", "show", "R1"]), before);
    // A session that only contains the assignee's name is not it.
    for session in ["dev-one-lead", "lead dev-one", "dev-ones"] {
        ok(&db, &["lead", "set", "R1", "--session", session]);
        let shown = ok(&db, &["repo", "show", "R1"]);
        assert!(shown.contains(&format!("(session {session}, ")), "{shown}");
    }
    ok(
        &db,
        &[
            "lead",
            "add",
            "R2",
            "--name",
            "Checkout",
            "--session",
            "checkout-lead",
        ],
    );
    // Once both tasks are unassigned, as the refusal says, the session is free.
    ok(&db, &["task", "unassign", "1"]);
    ok(&db, &["task", "unassign", "2"]);
    ok(&db, &["lead", "set", "R1", "--session", "dev-one"]);
    let shown = ok(&db, &["repo", "show", "R1"]);
    assert!(shown.contains("(session dev-one, "), "{shown}");
}

#[test]
fn refuses_bad_task_details_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(&db, &["task", "criterion", "add", "2", "Newest first"]);
    ok(&db, &["task", "blocked-by", "add", "2", "1"]);
    let before = ok(&db, &["task", "show", "2"]);
    let long = "x".repeat(2001);
    let long201 = "x".repeat(201);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["task", "set", "2"],
            "nothing to change: give --description",
        ),
        (
            vec!["task", "set", "9", "--assignee", "a"],
            "there is no task #9 on the board",
        ),
        (
            vec!["task", "set", "3", "--assignee", "a"],
            "#3 is a subtask of #1; details belong to tasks",
        ),
        (
            vec!["task", "set", "2", "--link", "not a url"],
            "the tracker link contains a space",
        ),
        (
            vec!["task", "set", "2", "--link", "example.com/x"],
            "is not a URL",
        ),
        (
            vec!["task", "set", "2", "--link", "ftp://example.com/x"],
            "is not an http or https link",
        ),
        (
            vec!["task", "set", "2", "--link", "javascript:alert(1)"],
            "is not an http or https link",
        ),
        (vec!["task", "set", "2", "--mr", " "], "the MR is empty"),
        (
            vec!["task", "set", "2", "--assignee", ""],
            "the assignee is empty: to clear it, tower task unassign 2",
        ),
        (
            vec!["task", "set", "2", "--mr", "!44\nmerged"],
            "the MR contains a control character (U+000A)",
        ),
        (
            vec!["task", "set", "2", "--mr", &long201],
            "the MR is 201 characters long; the most is 200",
        ),
        (
            vec!["task", "set", "2", "--description", " "],
            "the description is empty",
        ),
        (
            vec!["task", "set", "2", "--description", &long],
            "the description is 2001 characters",
        ),
        (
            vec!["task", "set", "2", "--context", "a\u{1b}[31m"],
            "the standing context contains a control character (U+001B)",
        ),
        (
            vec!["task", "set", "2", "--assignee", "dev\n2"],
            "the assignee contains a control character",
        ),
        (
            vec!["task", "criterion", "add", "2", " "],
            "the criterion is empty",
        ),
        (
            vec!["task", "criterion", "add", "2", " Newest first "],
            "#2 already has the criterion \"Newest first\" (number 1)",
        ),
        (
            vec!["task", "criterion", "add", "3", "c"],
            "#3 is a subtask of #1",
        ),
        (
            vec!["task", "criterion", "check", "2", "2"],
            "#2 has no criterion 2; it has 1",
        ),
        (
            vec!["task", "criterion", "check", "2", "0"],
            "#2 has no criterion 0",
        ),
        (
            vec!["task", "criterion", "uncheck", "2", "1"],
            "criterion 1 of #2 is already not met",
        ),
        (
            vec!["task", "criterion", "remove", "2", "2"],
            "#2 has no criterion 2; it has 1",
        ),
        (
            vec!["task", "criterion", "remove", "3", "1"],
            "#3 is a subtask of #1",
        ),
        (
            vec!["task", "unassign", "2"],
            "#2 is not assigned to anyone",
        ),
        (
            vec!["task", "unassign", "9"],
            "there is no task #9 on the board",
        ),
        (
            vec!["task", "blocked-by", "add", "2", "2"],
            "#2 cannot be blocked by itself",
        ),
        (
            vec!["task", "blocked-by", "add", "2", "1"],
            "#2 is already blocked by #1",
        ),
        (
            vec!["task", "blocked-by", "add", "2", "9"],
            "there is no task #9 on the board",
        ),
        (
            vec!["task", "blocked-by", "add", "2", "3"],
            "#3 is a subtask of #1",
        ),
        (
            vec!["task", "blocked-by", "add", "1", "2"],
            "#2 already waits for #1",
        ),
        (
            vec!["task", "blocked-by", "remove", "1", "2"],
            "#1 is not blocked by #2",
        ),
    ];
    for (args, message) in &cases {
        assert_refused(&tower(&db, args), message);
    }
    assert_eq!(ok(&db, &["task", "show", "2"]), before);
}

#[test]
fn sets_and_shows_milestone_details_and_refuses_bad_ones() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(&db, &["task", "set", "1", "--assignee", "dev-1"]);
    assert_eq!(
        ok(
            &db,
            &[
                "milestone",
                "set",
                "M1",
                "--context",
                "One branch per task.",
                "--spec",
                "docs/specs/first-shop-pages.md",
                "--link",
                "HTTPS://Example.com/m/1",
            ],
        ),
        "set the details of M1\n"
    );
    assert_eq!(
        ok(&db, &["milestone", "show", "M1"]),
        "M1 First shop pages
  goal       Usable.
  stage      waiting, 0/2 tasks done (0%)
  repo       R1
  lead       none yet
  context    One branch per task.
  spec       docs/specs/first-shop-pages.md
  tracker    https://example.com/m/1
tasks
  #1 Bookmarks page (waiting, dev-1, no ticket)
  #2 New book shelf (waiting, unassigned, no ticket)
threads
  none
"
    );
    let before = ok(&db, &["milestone", "show", "M1"]);
    let cases: [(&[&str], &str); 5] = [
        (
            &["milestone", "set", "M1"],
            "nothing to change: give --title, --goal, --context, --spec, --link or --repo",
        ),
        (
            &["milestone", "set", "M9", "--spec", "s"],
            "there is no milestone M9 on the board",
        ),
        (
            &["milestone", "set", "M1", "--link", "mailto:a@b.c"],
            "is not an http or https link",
        ),
        (
            &["milestone", "set", "M1", "--spec", "a\nb"],
            "the spec contains a control character",
        ),
        (
            &["milestone", "show", "M9"],
            "there is no milestone M9 on the board",
        ),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
    assert_eq!(ok(&db, &["milestone", "show", "M1"]), before);
    let shown = ok(&db, &["milestone", "show", "M2"]);
    assert!(
        shown.ends_with("  tracker    none\ntasks\n  none yet\nthreads\n  none\n"),
        "{shown}"
    );
}

#[test]
fn milestone_set_changes_the_title_and_goal_with_their_checks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(
        &db,
        &[
            "milestone",
            "add",
            "audiobook samples, first try",
            "--goal",
            "g",
        ],
    );
    ok(&db, &["milestone", "set", "M1", "--spec", "docs/s.md"]);
    let out = ok(
        &db,
        &[
            "milestone",
            "set",
            "M1",
            "--title",
            " Audiobooks ",
            "--goal",
            "Every audiobook has a sample.",
        ],
    );
    assert_eq!(out, "set the details of M1\n");
    let shown = ok(&db, &["milestone", "show", "M1"]);
    assert!(shown.starts_with("M1 Audiobooks\n"), "{shown}");
    assert!(
        shown.contains("  goal       Every audiobook has a sample.\n"),
        "{shown}"
    );
    // Only what is given changes: the spec set before stays.
    assert!(shown.contains("  spec       docs/s.md\n"), "{shown}");
    let long = "x".repeat(201);
    for (args, message) in [
        (&["--title", " "][..], "the milestone title is empty"),
        (
            &["--goal", "a\nb"],
            "the milestone goal contains a control character",
        ),
        (
            &["--title", long.as_str()],
            "the milestone title is 201 characters long",
        ),
    ] {
        let mut all = vec!["milestone", "set", "M1"];
        all.extend(args);
        assert_refused(&tower(&db, &all), message);
    }
    assert_eq!(ok(&db, &["milestone", "show", "M1"]), shown);
}

#[test]
fn threads_open_take_posts_and_resolve_with_evidence_then_refuse_posts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(
        &db,
        &["milestone", "add", "First shop pages", "--goal", "g"],
    );
    ok(&db, &["milestone", "add", "Checkout", "--goal", "g"]);
    in_repo(&db, "M1");
    in_repo(&db, "M2");
    for args in [
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue Lead",
            "--session",
            "cat-lead",
        ][..],
        &["task", "add", "--milestone", "M1", "New book shelf"],
        &["task", "add", "--milestone", "M2", "Basket"],
    ] {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    let ok = |args: &[&str], want: &str| {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), want, "{args:?}");
    };
    ok(
        &[
            "thread",
            "new",
            "M1",
            "--by",
            "cat-lead",
            "--task",
            "1",
            "Task #1 team",
            "Opening it.",
        ],
        "T1\n",
    );
    ok(
        &[
            "thread",
            "new",
            "cat-lead",
            "--by",
            "dev-1",
            "Cover images",
            "blocked: no covers yet",
        ],
        "T2\n",
    );
    ok(
        &["post", "T1", "--by", "dev-2", "copy"],
        "posted to T1, 1 reply\n",
    );
    ok(
        &["post", "T1", "--by", "qa", "copy, testing"],
        "posted to T1, 2 replies\n",
    );
    ok(
        &[
            "thread",
            "resolve",
            "T2",
            "--by",
            "cat-lead",
            "covers in for the whole shelf",
        ],
        "T2 is resolved, 1 reply\n",
    );

    let cases: [(&[&str], &str); 7] = [
        (
            &["post", "T2", "--by", "dev-1", "more"],
            "T2 is resolved; resolved threads are read-only",
        ),
        (&["thread", "resolve", "T1", "--by", "qa"], "<EVIDENCE>"),
        (
            &["thread", "resolve", "T1", "--by", "qa", " "],
            "the evidence is empty",
        ),
        (
            &[
                "thread", "new", "M1", "--by", "lead", "--task", "2", "t", "m",
            ],
            "#2 is in R2, not R1",
        ),
        (
            &["thread", "new", "M2", "--by", "lead", "t", "m"],
            "M2 has no lead yet, so it has no message board",
        ),
        (
            &["post", "T1", "--by", "dev  2", "hi"],
            "has two spaces in a row",
        ),
        (&["post", "3", "--by", "qa", "hi"], "not a thread id"),
    ];
    for (args, message) in cases {
        assert_refused(&tower(&db, args), message);
    }
    // A long post with line breaks is taken whole.
    let plan = format!("Plan:\n{}", "step ".repeat(700));
    ok(
        &["post", "T1", "--by", "dev-2", &plan],
        "posted to T1, 3 replies\n",
    );
    let too_long = "x".repeat(4001);
    assert_refused(
        &tower(&db, &["post", "T1", "--by", "dev-2", &too_long]),
        "the message is 4001 characters long; the most is 4000",
    );
    assert_refused(
        &tower(&db, &["post", "T1", "--by", "dev-2", "a\u{7}b"]),
        "the message contains a control character (U+0007)",
    );
    // Pasted text: CRLF and tabs are normalised.
    ok(
        &["post", "T1", "--by", "qa", "one\r\n\ttwo"],
        "posted to T1, 4 replies\n",
    );
    let mut store = Store::open(&db).unwrap();
    let t1 = store.thread("T1".parse().unwrap()).unwrap();
    assert_eq!(t1.messages[3].text, plan.trim());
    assert_eq!(t1.messages[4].text, "one\n    two");
    let board = store.load().unwrap();
    let counts: Vec<(usize, usize)> = board.milestones[0]
        .threads
        .iter()
        .map(|t| (t.total, t.replies()))
        .collect();
    assert_eq!(counts, vec![(5, 4), (2, 1)]);
}

#[test]
fn blocks_and_unblocks_a_task_and_shows_its_doing_post() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Cat Lead",
            "--session",
            "cat-lead",
        ],
    );
    ok(&db, &["task", "set", "2", "--assignee", "dev-2"]);
    ok(
        &db,
        &[
            "thread", "new", "M1", "--by", "cat-lead", "--task", "2", "Team", "go",
        ],
    );
    ok(&db, &["post", "T1", "--by", "dev-2", "copy"]);
    assert!(ok(&db, &["task", "show", "2"]).contains("  doing      nothing posted yet\n"));
    assert_eq!(
        ok(
            &db,
            &[
                "post",
                "T1",
                "--by",
                "dev-2",
                "--progress",
                "step 1: layout done"
            ]
        ),
        "posted progress to T1, 2 replies\n"
    );
    ok(&db, &["post", "T1", "--by", "qa", "copy, testing"]);
    ok(&db, &["thread", "new", "M1", "--by", "qa", "Chat", "hi"]);
    assert_refused(
        &tower(&db, &["post", "T2", "--by", "qa", "--progress", "x"]),
        "T2 is linked to no task",
    );
    let shown = ok(&db, &["task", "show", "2"]);
    assert!(shown.contains("  blocked    no\n"), "{shown}");
    assert!(
        shown.contains("  doing      step 1: layout done\n"),
        "{shown}"
    );

    assert_eq!(
        ok(
            &db,
            &["task", "block", "2", " printer has not sent the covers "]
        ),
        "task #2 is blocked: printer has not sent the covers; moving it to done clears it\n"
    );
    assert_eq!(
        ok(&db, &["task", "block", "#2", "still no covers"]),
        "task #2 is blocked: still no covers (was: printer has not sent the covers); \
         moving it to done clears it\n"
    );
    let shown = ok(&db, &["milestone", "show", "M1"]);
    assert!(
        shown.contains(
            "  #2 New book shelf (waiting, dev-2, blocked: still no covers, no ticket)\n"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("  #1 Bookmarks page (waiting, unassigned, no ticket)\n"),
        "{shown}"
    );
    let shown = ok(&db, &["task", "show", "2"]);
    assert!(shown.contains("  blocked    still no covers\n"), "{shown}");
    // The flag is not blocked by.
    assert!(shown.contains("  blocked by nothing\n"), "{shown}");

    for (args, message) in [
        (&["task", "block", "3", "r"][..], "#3 is a subtask of #1"),
        (&["task", "block", "2"], "<REASON>"),
        (&["task", "block", "2", "  "], "the blocked reason is empty"),
        (
            &["task", "block", "9", "r"],
            "there is no task #9 on the board",
        ),
        (&["task", "unblock", "1"], "#1 is not blocked"),
    ] {
        assert_refused(&tower(&db, args), message);
    }
    assert_eq!(
        ok(&db, &["task", "unblock", "2"]),
        "task #2 is no longer blocked (was: still no covers)\n"
    );
    assert!(ok(&db, &["task", "show", "2"]).contains("  blocked    no\n"));
    // Done clears the flag, and a done task cannot be blocked.
    ok(&db, &["task", "block", "2", "again"]);
    ok(&db, &["task", "stage", "2", "done"]);
    assert!(ok(&db, &["task", "show", "2"]).contains("  blocked    no\n"));
    assert_refused(
        &tower(&db, &["task", "block", "2", "r"]),
        "#2 is done, and a done task is not stuck",
    );
}

#[test]
fn task_show_lists_the_tasks_threads_open_first_then_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Cat",
            "--session",
            "cat-lead",
        ],
    );
    assert!(ok(&db, &["task", "show", "2"]).ends_with("threads\n  none\n"));

    let new = |by: &str, task: Option<&str>, title: &str| {
        let mut args = vec!["thread", "new", "M1", "--by", by];
        if let Some(task) = task {
            args.extend(["--task", task]);
        }
        args.extend([title, "go"]);
        ok(&db, &args);
    };
    new("cat-lead", Some("2"), "Task #2 team");
    ok(&db, &["post", "T1", "--by", "dev-2", "copy"]);
    assert!(
        ok(&db, &["task", "show", "2"])
            .ends_with("threads\n  T1 Task #2 team (open, by cat-lead, 1 reply)\n")
    );

    new("dev-2", Some("2"), "Spike on caching");
    ok(
        &db,
        &["thread", "resolve", "T2", "--by", "senior", "no gain"],
    );
    new("qa", None, "Chat");
    new("cat-lead", Some("1"), "Task #1 team");
    // Opened last, so its activity is the newest, or ties with T1's; the tie rule (newer id
    // first) is tested in src/ui/message_board.rs.
    new("cat-lead", Some("2"), "Watch on #2");
    let shown = ok(&db, &["task", "show", "2"]);
    assert!(
        shown.ends_with(
            "subtasks\n  none\nthreads\n\
             \x20 T5 Watch on #2 (open, by cat-lead, 0 replies)\n\
             \x20 T1 Task #2 team (open, by cat-lead, 1 reply)\n\
             \x20 T2 Spike on caching (resolved, by dev-2, 1 reply)\n"
        ),
        "{shown}"
    );
    assert!(
        ok(&db, &["task", "show", "1"])
            .ends_with("threads\n  T4 Task #1 team (open, by cat-lead, 0 replies)\n")
    );

    // The milestone lists all of them in the same order, each naming its task.
    let shown = ok(&db, &["milestone", "show", "M1"]);
    assert!(
        shown.ends_with(
            "threads\n\
             \x20 T5 Watch on #2 (open, about #2, by cat-lead, 0 replies)\n\
             \x20 T4 Task #1 team (open, about #1, by cat-lead, 0 replies)\n\
             \x20 T3 Chat (open, by qa, 0 replies)\n\
             \x20 T1 Task #2 team (open, about #2, by cat-lead, 1 reply)\n\
             \x20 T2 Spike on caching (resolved, about #2, by dev-2, 1 reply)\n"
        ),
        "{shown}"
    );

    ok(&db, &["task", "add", "loose"]);
    assert!(ok(&db, &["task", "show", "4"]).ends_with("threads\n  none\n"));
    assert_refused(
        &tower(&db, &["task", "show", "3"]),
        "there is no task #3 on the board",
    );
}

#[test]
fn thread_show_prints_a_thread_whole_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    detail_board(&db);
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Cat",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread",
            "new",
            "M1",
            "--by",
            "cat-lead",
            "--task",
            "2",
            "Task #2 team",
            "Opening it.",
        ],
    );
    ok(
        &db,
        &[
            "post",
            "T1",
            "--by",
            "dev-2",
            "Plan:\n1. layout\n\n2. tests",
        ],
    );
    ok(
        &db,
        &["post", "T1", "--by", "dev-2", "--progress", "tests green"],
    );
    // Every message is posted within the test's run, so every time is "now"; the other ages
    // are tested in src/main.rs with a fixed clock.
    let whole = "T1 Task #2 team
  status     open
  task       #2 New book shelf
  board      R1
  started by cat-lead
messages
  cat-lead  now
    Opening it.
  dev-2  now
    Plan:
    1. layout

    2. tests
  dev-2  now
    tests green
";
    assert_eq!(ok(&db, &["thread", "show", "T1"]), whole);

    ok(
        &db,
        &["thread", "resolve", "T1", "--by", "senior", "approved"],
    );
    assert_eq!(
        ok(&db, &["thread", "show", "T1"]),
        whole.replace("status     open", "status     resolved by senior, now")
            + "  senior  now\n    approved\n"
    );

    ok(&db, &["thread", "new", "M1", "--by", "qa", "Chat", "hi"]);
    assert!(ok(&db, &["thread", "show", "T2"]).starts_with(
        "T2 Chat\n  status     open\n  task       none\n  board      R1, about M1 First shop pages\n"
    ));
    // A thread on another repo's board names that board and its task.
    in_repo(&db, "M2");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M2",
            "--name",
            "Checkout",
            "--session",
            "checkout-lead",
        ],
    );
    let task = ok(&db, &["task", "add", "--milestone", "M2", "Basket"]);
    assert_eq!(task, "#4\n");
    ok(
        &db,
        &[
            "thread",
            "new",
            "M2",
            "--by",
            "checkout-lead",
            "--task",
            "4",
            "Basket team",
            "go",
        ],
    );
    let shown = ok(&db, &["thread", "show", "T3"]);
    assert!(
        shown.starts_with(
            "T3 Basket team\n  status     open\n  task       #4 Basket\n  board      R2\n"
        ),
        "{shown}"
    );
    assert_refused(
        &tower(&db, &["thread", "show", "T9"]),
        "there is no thread T9 on the board",
    );
}

/// Runs `tower` with `input` on its stdin.
fn tower_with_stdin(db: &Path, args: &[&str], input: impl AsRef<[u8]>) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_tower"))
        .args(args)
        .env("TOWER_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A command refused before reading stdin can exit first and close the pipe:
    // that write fails with BrokenPipe, and the output and status still tell.
    match child.stdin.take().unwrap().write_all(input.as_ref()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            panic!("writing to tower's stdin failed: {e}")
        }
        _ => {}
    }
    child.wait_with_output().unwrap()
}

#[test]
fn post_reads_the_message_from_stdin_when_it_is_a_dash() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(
        &db,
        &["milestone", "add", "First shop pages", "--goal", "g"],
    );
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Lead",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &["thread", "new", "M1", "--by", "cat-lead", "t", "Open."],
    );
    ok(
        &db,
        &["thread", "new", "M1", "--by", "cat-lead", "u", "Open."],
    );
    ok(
        &db,
        &["thread", "resolve", "T2", "--by", "cat-lead", "done"],
    );

    // Text starting with "-", spanning lines, never parsed as an option.
    let post = ["post", "T1", "--by", "qa", "-"];
    let out = tower_with_stdin(&db, &post, "--by qa -x\napproved\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "posted to T1, 1 reply\n");
    let t = Store::open(&db)
        .unwrap()
        .thread("T1".parse().unwrap())
        .unwrap();
    let last = t.messages.last().unwrap();
    assert_eq!(
        (last.author.as_str(), last.text.as_str()),
        ("qa", "--by qa -x\napproved")
    );

    // A list keeps its first line's indent, so it reads in line (#16); leading blank lines
    // and the end go, as the signed design's composer drops them.
    let list = "\n  - preheat the left oven\n  - proof the rye\n";
    let out = tower_with_stdin(&db, &post, list);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = Store::open(&db)
        .unwrap()
        .thread("T1".parse().unwrap())
        .unwrap();
    assert_eq!(
        t.messages.last().unwrap().text,
        "  - preheat the left oven\n  - proof the rye"
    );
    // A form feed first, as text pasted from a pager can start: it goes, as it always did.
    let out = tower_with_stdin(&db, &post, "\u{c}the glue is late\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = Store::open(&db)
        .unwrap()
        .thread("T1".parse().unwrap())
        .unwrap();
    assert_eq!(t.messages.last().unwrap().text, "the glue is late");

    // Checked as any other post.
    assert_refused(&tower_with_stdin(&db, &post, " \n"), "the message is empty");
    assert_refused(
        &tower_with_stdin(&db, &post, "x".repeat(4001)),
        "the message is 4001 characters long",
    );
    assert_refused(
        &tower_with_stdin(&db, &["post", "T2", "--by", "qa", "-"], "hi"),
        "T2 is resolved; resolved threads are read-only",
    );
    assert_refused(
        &tower_with_stdin(&db, &["post", "T1", "--by", "qa", "-"], "a\u{1b}b"),
        "control character",
    );
    assert_refused(
        &tower_with_stdin(&db, &post, [0xff, 0xfe]),
        "cannot read the message from stdin",
    );
}

#[test]
fn the_cli_refuses_to_post_as_manager_and_has_no_inbox_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "M", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &["thread", "new", "M1", "--by", "cat-lead", "t", "Open."],
    );
    // Only the owner posts as manager, from the dashboard: any case, posts and new threads.
    let why = "only the owner posts as manager, from the dashboard";
    for by in ["manager", "Manager", "MANAGER"] {
        assert_refused(&tower(&db, &["post", "T1", "--by", by, "hi"]), why);
        assert_refused(
            &tower_with_stdin(&db, &["post", "T1", "--by", by, "-"], "hi"),
            why,
        );
        assert_refused(
            &tower(&db, &["post", "T1", "--by", by, "--progress", "hi"]),
            why,
        );
        assert_refused(
            &tower(&db, &["thread", "new", "M1", "--by", by, "u", "Open."]),
            why,
        );
        assert_refused(
            &tower(&db, &["thread", "resolve", "T1", "--by", by, "done"]),
            why,
        );
    }
    // Lookalikes are refused too: fullwidth and Cyrillic "manager", and a lead session.
    for by in ["ｍａｎａｇｅｒ", "m\u{430}nager"] {
        assert_refused(
            &tower(&db, &["post", "T1", "--by", by, "hi"]),
            "use only ASCII letters, digits",
        );
    }
    assert_refused(
        &tower(&db, &["lead", "set", "M1", "--session", "сat-lead"]),
        "use only ASCII letters, digits",
    );
    // Nothing was written.
    let board = Store::open(&db).unwrap().load().unwrap();
    assert_eq!(board.milestones[0].threads.len(), 1);
    assert_eq!(board.milestones[0].threads[0].total, 1);
    assert!(board.milestones[0].threads[0].resolved.is_none());
    // Names that only start like it are anyone's.
    ok(&db, &["post", "T1", "--by", "managers", "hi"]);
    // Marking read is the dashboard's: the CLI has no inbox command.
    let out = tower(&db, &["inbox", "read", "1"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("unrecognized subcommand 'inbox'"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn watch_starts_orphans_no_one_on_its_first_look_and_stops_cleanly_on_sigterm() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "M", "--goal", "g"]);
    in_repo(&db, "M1");
    for args in [
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "L",
            "--session",
            "shelf-lead",
        ][..],
        &["task", "add", "--milestone", "M1", "t"],
        &["task", "set", "1", "--assignee", "dev-9"],
        &["task", "stage", "1", "in-progress"],
    ] {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    // A Rex with no sessions: the watch's look never reaches a real rex (REX_BIN_DIR).
    let rex = dir.path().join("rex");
    test_exe::executable(&rex, "#!/bin/sh\necho '{\"sessions\":[]}'\n");
    /// Kills the watch if the test fails before it stops, so none is left running.
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reap = Reap(
        Command::new(env!("CARGO_BIN_EXE_tower"))
            .arg("watch")
            .env("TOWER_DB", &db)
            .env("TOWER_REX", &rex)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let child = &mut reap.0;
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut next = || lines.next().unwrap().unwrap();
    assert!(next().contains("tower watch: watching"));
    // A tower outside a brew Cellar does not register the lead skill: claude is never asked.
    assert!(next().ends_with("not registering the lead skill: this tower is not a brew install"));
    // SIGTERM ends it between looks, with a last line and a clean exit.
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(next().ends_with("tower watch: stopped"));
    assert!(child.wait().unwrap().success());
    // dev-9 is missing from Rex, but for one look only: orphaned takes three.
    let mut store = Store::open(&db).unwrap();
    assert!(store.flags().unwrap().is_empty());
    assert!(store.load().unwrap().milestones[0].threads.is_empty());
}

// macOS only (SDLC.md): the watch keeps the path it was started as, as macOS reports
// `current_exe` (the brew opt link, unresolved); Linux resolves it, so it never sees the
// link move. The product ships only for macOS.
#[cfg(target_os = "macos")]
#[test]
fn watch_restarts_itself_when_its_path_is_repointed_to_another_binary() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // A Rex with no sessions: the watch's look never reaches a real rex (REX_BIN_DIR).
    let rex = dir.path().join("rex");
    test_exe::executable(&rex, "#!/bin/sh\necho '{\"sessions\":[]}'\n");
    // Two installed versions and an opt link, as brew lays them out.
    let (old, new) = (dir.path().join("0.1.1"), dir.path().join("0.1.2"));
    for v in [&old, &new] {
        std::fs::create_dir(v).unwrap();
        test_exe::executable(&v.join("tower"), tower_bytes());
    }
    let opt = dir.path().join("opt");
    std::os::unix::fs::symlink(&old, &opt).unwrap();
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reap = Reap(
        Command::new(opt.join("tower"))
            .arg("watch")
            .env("TOWER_DB", &db)
            .env("TOWER_REX", &rex)
            .env("HOME", dir.path())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let pid = reap.0.id();
    let (tx, rx) = std::sync::mpsc::channel();
    let stderr = reap.0.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = Vec::new();
    let mut wait_for = |want: &str| loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                seen.push(line.clone());
                if line.contains(want) {
                    return;
                }
            }
            Err(_) => panic!("no {want:?} within 30 s; stderr so far: {seen:#?}"),
        }
    };
    wait_for("tower watch: watching");
    wait_for("not registering the lead skill");
    // brew upgrade: opt now points at the new version.
    let next = dir.path().join("opt.next");
    std::os::unix::fs::symlink(&new, &next).unwrap();
    std::fs::rename(&next, &opt).unwrap();
    wait_for("tower watch: tower was upgraded, restarting");
    wait_for("tower watch: watching");
    assert_eq!(reap.0.id(), pid);
    assert!(
        reap.0.try_wait().unwrap().is_none(),
        "the restarted watch runs on"
    );
}

#[test]
fn a_lead_session_name_with_spaces_works_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(
        &db,
        &["milestone", "add", "First shop pages", "--goal", "g"],
    );
    in_repo(&db, "M1");
    let cl = "Catalogue Lead";
    ok(&db, &["lead", "add", "M1", "--name", cl, "--session", cl]);
    ok(&db, &["task", "add", "--milestone", "M1", "Bookmarks page"]);
    // Its board by session name, posting and resolving as it.
    ok(
        &db,
        &["thread", "new", cl, "--by", cl, "--task", "1", "t", "Open."],
    );
    ok(&db, &["post", "T1", "--by", cl, "copy"]);
    ok(&db, &["post", "T1", "--by", " Catalogue Lead ", "trimmed"]);
    let board = Store::open(&db).unwrap().load().unwrap();
    let m = &board.milestones[0];
    assert_eq!(m.lead.as_ref().unwrap().session, cl);
    assert_eq!(m.threads[0].authors, [cl]);
    // The lead is never on its own team, by its spaced name.
    assert!(m.team().is_empty());
    for (by, why) in [
        ("Catalogue  Desk", "has two spaces in a row"),
        (" manager ", "only the owner posts as manager"),
        ("Man ager", "only the owner posts as manager"),
        (" system ", "the author system is kept"),
    ] {
        assert_refused(&tower(&db, &["post", "T1", "--by", by, "hi"]), why);
    }
    assert_refused(
        &tower(&db, &["lead", "set", "M1", "--session", "Catalogue  Desk"]),
        "has two spaces in a row",
    );
    ok(&db, &["thread", "resolve", "T1", "--by", cl, "done"]);
}

#[test]
fn space_and_popup_are_the_rex_keys_commands_and_need_where_the_key_was_pressed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    for command in ["space", "popup"] {
        // With neither, clap names both; with only the session, the block alone.
        for (args, missing) in [
            (
                &[command][..],
                "  --session <SESSION>\n  --block <BLOCK>\n\n",
            ),
            (
                &[command, "--session", "session:a"],
                "  --block <BLOCK>\n\n",
            ),
        ] {
            let out = tower(&db, args);
            assert!(!out.status.success(), "tower {args:?} ran");
            let err = text(&out.stderr);
            let want = format!("the following required arguments were not provided:\n{missing}");
            assert!(err.contains(&want), "{args:?}: {err}");
        }
    }
    // Refused before the board is opened: nothing was written.
    assert!(!db.exists());
}

#[test]
fn task_stage_done_by_records_who_and_refuses_by_for_another_stage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "M", "--goal", "g"]);
    in_repo(&db, "M1");
    for args in [
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "L",
            "--session",
            "shelf-lead",
        ][..],
        &["task", "add", "--milestone", "M1", "t"],
    ] {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    let out = tower(
        &db,
        &["task", "stage", "1", "in-progress", "--by", "shelf-lead"],
    );
    assert_refused(&out, "give it only with done");
    let out = tower(&db, &["task", "stage", "1", "done", "--by", "shelf-lead"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let mut store = Store::open(&db).unwrap();
    let t = store.task(control_tower::board::TaskId(1)).unwrap();
    assert_eq!(t.details.done_by.as_deref(), Some("shelf-lead"));
}

/// `exe` run as from a shell with `HOME` at `home` and no `TOWER_DB`, so on the default board.
fn on_default_board(exe: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .env("HOME", home)
        .env_remove("TOWER_DB")
        .output()
        .unwrap()
}

fn version(db: &Path) -> i32 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// A board at the current schema, its version then set to `v`. Only the number matters to
/// a refusal, which comes before any migration runs.
fn board_at_version(db: &Path, v: i32) {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    Store::open(db)
        .unwrap()
        .add_milestone("M", "g", None)
        .unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.pragma_update(None, "user_version", v).unwrap();
}

#[test]
fn a_dev_build_never_creates_or_upgrades_the_default_board() {
    use control_tower::store::SCHEMA_VERSION;
    let home = tempfile::tempdir().unwrap();
    let dev = Path::new(env!("CARGO_BIN_EXE_tower"));
    let db = home.path().join(".local/state/control-tower/board.db");
    let refusal = "is not the brew install and would create it. Set TOWER_DB to a test board";

    // No board yet: refused, and nothing is created.
    let out = on_default_board(dev, home.path(), &["milestone", "add", "M", "--goal", "g"]);
    assert_refused(&out, refusal);
    assert!(!db.exists(), "a dev build created the default board");
    // Nor when TOWER_DB names it another way before it exists: with neither the board nor
    // its directory there yet, HOME through a link and TOWER_DB through the real path ...
    let link = tempfile::tempdir().unwrap();
    let linked_home = link.path().join("home");
    std::os::unix::fs::symlink(home.path(), &linked_home).unwrap();
    let real = home
        .path()
        .canonicalize()
        .unwrap()
        .join(".local/state/control-tower/board.db");
    let out = Command::new(dev)
        .args(["milestone", "add", "M", "--goal", "g"])
        .env("HOME", &linked_home)
        .env("TOWER_DB", &real)
        .output()
        .unwrap();
    assert_refused(&out, refusal);
    assert!(!db.exists(), "a dev build created the default board");
    // ... and, with its directory there, a relative TOWER_DB from inside it.
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let out = Command::new(dev)
        .args(["milestone", "add", "M", "--goal", "g"])
        .current_dir(db.parent().unwrap())
        .env("HOME", home.path())
        .env("TOWER_DB", "board.db")
        .output()
        .unwrap();
    assert_refused(&out, refusal);
    assert!(!db.exists(), "a dev build created the default board");

    // A board one version behind this build: a read and a write are
    // refused, and the board keeps its version and bytes.
    board_at_version(&db, SCHEMA_VERSION - 1);
    let before = std::fs::read(&db).unwrap();
    let upgrade = format!(
        "would upgrade it from schema version {} to {SCHEMA_VERSION}, locking out the installed tower",
        SCHEMA_VERSION - 1
    );
    for args in [
        &["milestone", "show", "M1"][..],
        &["milestone", "add", "N", "--goal", "g"],
        &["upgrade"],
    ] {
        let out = on_default_board(dev, home.path(), args);
        assert_refused(&out, &upgrade);
        assert_refused(&out, "Set TOWER_DB to a test board");
    }
    // TOWER_DB naming the same file another way is the default board all the same.
    let other_name = home
        .path()
        .join(".local/state/control-tower/../control-tower/board.db");
    let named = |db: &Path, args: &[&str]| {
        Command::new(dev)
            .args(args)
            .env("HOME", home.path())
            .env("TOWER_DB", db)
            .output()
            .unwrap()
    };
    let out = named(&other_name, &["milestone", "show", "M1"]);
    assert_refused(&out, &upgrade);
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "the board's bytes changed"
    );
    assert_eq!(version(&db), SCHEMA_VERSION - 1);

    // At this build's own schema it opens the default board as before.
    rusqlite::Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .unwrap();
    let out = on_default_board(dev, home.path(), &["milestone", "show", "M1"]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    // Any other board it upgrades as before: an empty file is schema version 0.
    let elsewhere = home.path().join("try.db");
    std::fs::write(&elsewhere, b"").unwrap();
    let out = named(&elsewhere, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(version(&elsewhere), SCHEMA_VERSION);
}

#[test]
fn the_brew_tower_creates_and_upgrades_the_default_board() {
    use control_tower::store::SCHEMA_VERSION;
    let home = tempfile::tempdir().unwrap();
    // The same binary where brew installs it.
    let bin = home.path().join("brew/Cellar/control-tower/0.1.4/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let brew = bin.join("tower");
    test_exe::executable(&brew, tower_bytes());
    let db = home.path().join(".local/state/control-tower/board.db");

    let out = on_default_board(
        &brew,
        home.path(),
        &["milestone", "add", "M", "--goal", "g"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(version(&db), SCHEMA_VERSION);

    // An older board (an empty file is schema version 0) it upgrades.
    std::fs::remove_file(&db).unwrap();
    std::fs::write(&db, b"").unwrap();
    let out = on_default_board(&brew, home.path(), &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(version(&db), SCHEMA_VERSION);
}

#[test]
fn a_lead_attaches_its_claude_session_from_that_session_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // A fake claude: `agents --json` prints `sessions.json`, or fails without it. Never the
    // real claude, and never this test's own session: each run sets or removes the variable.
    let claude = dir.path().join("claude");
    test_exe::executable(
        &claude,
        "#!/bin/sh\n[ \"$1 $2\" = \"agents --json\" ] || exit 2\n\
         d=$(dirname \"$0\")\n[ -f \"$d/sessions.json\" ] || { echo 'not logged in' >&2; exit 1; }\n\
         cat \"$d/sessions.json\"\n",
    );
    let sessions = |json: &str| std::fs::write(dir.path().join("sessions.json"), json).unwrap();
    let attach = |m: &str, vars: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tower"));
        cmd.args(["lead", "attach", m])
            .env("TOWER_DB", &db)
            .env("TOWER_CLAUDE", &claude)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("REX_SESSION", "session:7");
        for (k, v) in vars {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    let ok = |args: &[&str]| {
        let out = tower(&db, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    };
    ok(&["milestone", "add", "MVP", "--goal", "g"]);
    let both = [("CLAUDE_CODE_SESSION_ID", "sess-1")];
    assert_refused(&attach("M1", &both), "M1 is in no repo yet");
    in_repo(&db, "M1");
    assert_refused(&attach("M1", &both), "R1 has no lead yet");
    ok(&[
        "lead",
        "add",
        "M1",
        "--name",
        "Catalogue",
        "--session",
        "cat-lead",
    ]);
    assert!(
        ok(&["milestone", "show", "M1"])
            .contains("  session    not attached (tower lead attach)\n")
    );
    let not_claude = "CLAUDE_CODE_SESSION_ID is not set: run tower lead attach from the \
                      lead's own Claude session (its Bash)";
    assert_refused(&attach("M1", &[]), not_claude);
    // Who is asking: claude cannot be asked, the session is not listed, or it is another
    // session than the one the lead registered.
    assert_refused(
        &attach("M1", &both),
        "cannot ask claude who is asking, so the session is not attached: claude agents --json \
         exited 1: not logged in",
    );
    sessions(r#"[{"sessionId":"sess-9","name":"cat-lead","kind":"interactive"}]"#);
    assert_refused(
        &attach("M1", &both),
        "claude agents --json lists no session sess-1",
    );
    sessions(r#"[{"sessionId":"sess-1","name":"dev-1","kind":"interactive"}]"#);
    assert_refused(
        &attach("M1", &both),
        "this Claude session is named \"dev-1\", not \"cat-lead\", the session the lead of R1 \
         registered; only the lead attaches its session",
    );
    sessions(r#"[{"sessionId":"sess-1","kind":"interactive"}]"#);
    assert_refused(
        &attach("M1", &both),
        "this Claude session is named nothing, not",
    );
    sessions(
        r#"[{"sessionId":"sess-0"},{"sessionId":"sess-1","name":"Cat-Lead","kind":"interactive"}]"#,
    );
    assert_refused(&attach("M9", &both), "there is no milestone M9");
    let out = attach("M1", &both);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "attached Catalogue as Claude session sess-1; posts on the board of R1 reach it through \
         its board waiter (tower lead wait R1) and its Stop hook\n"
    );
    // Outside Rex (no REX_SESSION, or an empty one): attached all the same, and told what the
    // board does not see from there.
    for outside in [None, Some("")] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tower"));
        cmd.args(["lead", "attach", "M1"])
            .env("TOWER_DB", &db)
            .env("TOWER_CLAUDE", &claude)
            .env("CLAUDE_CODE_SESSION_ID", "sess-1");
        match outside {
            None => cmd.env_remove("REX_SESSION"),
            Some(v) => cmd.env("REX_SESSION", v),
        };
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        let said = text(&out.stdout);
        assert!(
            said.starts_with("attached Catalogue as Claude session sess-1; posts on the board")
                && said.contains(
                    " and its Stop hook\ncontrol-tower: this session runs outside Rex \
                     (REX_SESSION is not set), but the board's team runs in Rex."
                )
                && said.ends_with("question: post to @manager.\n"),
            "{outside:?}: {said}"
        );
    }
    assert!(
        ok(&["milestone", "show", "M1"]).contains("  session    attached, Claude session sess-1\n")
    );
    let leads = Store::open(&db).unwrap().attached_leads().unwrap();
    assert_eq!(leads[0].claude_session.as_deref(), Some("sess-1"));
    // The same session in another case stays attached; a new session forgets it.
    ok(&["lead", "set", "M1", "--session", "Cat-Lead"]);
    assert!(
        ok(&["milestone", "show", "M1"]).contains("  session    attached, Claude session sess-1\n")
    );
    ok(&["lead", "set", "M1", "--session", "cat-lead-2"]);
    assert!(
        ok(&["milestone", "show", "M1"])
            .contains("  session    not attached (tower lead attach)\n")
    );
    assert!(
        Store::open(&db)
            .unwrap()
            .attached_leads()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_standalone_task_with_no_repo_is_listed_and_gets_one_with_task_set() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // As a board upgraded to schema 14 leaves them: M1 and the standalone #1 in no repo.
    ok(&db, &["milestone", "add", "Old work", "--goal", "g"]);
    ok(&db, &["task", "add", "Loose task"]);
    assert!(ok(&db, &["task", "show", "1"]).contains(
        "  milestone  none (standalone)\n  repo       none yet (tower task set 1 --repo R<n>)\n"
    ));
    let unassigned = "no repo yet: M1, #1; give each one with tower milestone set M<n> \
                      --repo R<n> or tower task set <n> --repo R<n>\n";
    // With no repo on the board, repo list still names them.
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!("no repos yet: add one with tower repo add <main checkout>\n{unassigned}")
    );
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    let real = checkout.canonicalize().unwrap();
    ok(&db, &["repo", "add", checkout.to_str().unwrap()]);
    ok(&db, &["task", "add", "--milestone", "M1", "Milestone task"]);
    ok(&db, &["subtask", "add", "--task", "1", "A step"]);
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!(
            "R1 {}, no lead, no milestones or standalone tasks\n{unassigned}",
            real.display()
        )
    );
    for (args, message) in [
        (
            &["task", "set", "2", "--repo", "R1"][..],
            "#2 is under M1, and its repo is M1's: only a standalone task takes --repo",
        ),
        (
            &["task", "set", "3", "--repo", "R1"],
            "#3 is a subtask of #1; details belong to tasks",
        ),
        (
            &["task", "set", "1", "--repo", "R99"],
            "there is no repo R99 on the board",
        ),
        (&["task", "set", "9", "--repo", "R1"], "there is no task #9"),
        (&["task", "set", "1"], "--assignee or --repo"),
    ] {
        assert_refused(&tower(&db, args), message);
    }
    assert_eq!(
        ok(&db, &["task", "set", "1", "--repo", "R1"]),
        "set the details of task #1; it is in R1, no lead yet, and its 0 thread(s) are on R1's \
         board\n"
    );
    assert!(
        ok(&db, &["task", "show", "1"])
            .contains("  milestone  none (standalone)\n  repo       R1\n  assignee")
    );
    // A milestone's task has no repo line: its milestone line names where it is.
    assert!(!ok(&db, &["task", "show", "2"]).contains("  repo "));
    assert!(
        ok(&db, &["repo", "show", "R1"])
            .contains("standalone tasks\n  #1 Loose task (no ticket)\n")
    );
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!(
            "R1 {}, no lead, #1\n\
             no repo yet: M1; give each one with tower milestone set M<n> --repo R<n> \
             or tower task set <n> --repo R<n>\n",
            real.display()
        )
    );
    ok(&db, &["milestone", "set", "M1", "--repo", "R1"]);
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!("R1 {}, no lead, M1 #1\n", real.display())
    );
    // A standalone task with threads moves only to a repo with a lead, as a milestone does.
    ok(
        &db,
        &[
            "lead",
            "add",
            "R1",
            "--name",
            "Cat",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "cat-lead", "--by", "cat-lead", "--task", "1", "Shelves", "go",
        ],
    );
    let other = dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    ok(&db, &["repo", "add", other.to_str().unwrap()]);
    assert_refused(
        &tower(&db, &["task", "set", "1", "--repo", "R2"]),
        "#1 has 1 thread(s), and R2 has no lead to read them there; add its lead first: \
         tower lead add R2",
    );
    ok(
        &db,
        &["lead", "add", "R2", "--name", "Other", "--session", "other"],
    );
    assert_eq!(
        ok(&db, &["task", "set", "1", "--repo", "R2"]),
        "set the details of task #1; it is in R2, led by Other, and its 1 thread(s) are on \
         R2's board\n"
    );
    assert!(ok(&db, &["thread", "show", "T1"]).contains("\n  board      R2"));
    // repo list names a repo's standalone tasks, also when it has no milestone.
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!(
            "R1 {}, lead Cat (session cat-lead), M1\n\
             R2 {}, lead Other (session other), #1\n",
            real.display(),
            other.canonicalize().unwrap().display()
        )
    );
}

/// The listings give each task's ticket from its tracker link, so a lead matches the
/// forge's numbers to tasks without the title carrying them.
#[test]
fn repo_and_milestone_listings_give_each_tasks_ticket_from_its_link() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    ok(&db, &["repo", "add", checkout.to_str().unwrap()]);
    ok(
        &db,
        &["milestone", "add", "MVP", "--goal", "g", "--repo", "R1"],
    );
    let gl = "https://gitlab.example.com/acme/bookshop/-";
    for (title, link) in [
        ("Author index", Some(format!("{gl}/work_items/36"))),
        ("Spell check", Some(format!("{gl}/merge_requests/23"))),
        (
            "Design review",
            Some("https://example.com/notes/7".to_string()),
        ),
        ("Tidy", None),
    ] {
        let id = ok(&db, &["task", "add", "--milestone", "M1", title]);
        if let Some(link) = link {
            ok(&db, &["task", "set", id.trim(), "--link", &link]);
        }
    }
    ok(&db, &["task", "add", "Sort the shelves", "--repo", "R1"]);
    ok(
        &db,
        &[
            "task",
            "set",
            "5",
            "--link",
            "https://github.com/example/bookshop/issues/41",
        ],
    );
    let shown = ok(&db, &["milestone", "show", "M1"]);
    for want in [
        "  #1 Author index (waiting, unassigned, ticket #36)\n",
        "  #2 Spell check (waiting, unassigned, ticket !23)\n",
        "  #3 Design review (waiting, unassigned, ticket https://example.com/notes/7)\n",
        "  #4 Tidy (waiting, unassigned, no ticket)\n",
    ] {
        assert!(shown.contains(want), "{want:?} missing from\n{shown}");
    }
    let repo = ok(&db, &["repo", "show", "R1"]);
    assert!(
        repo.contains("standalone tasks\n  #5 Sort the shelves (ticket #41)\n"),
        "{repo}"
    );
}

#[test]
fn a_repo_holds_milestones_standalone_tasks_one_lead_and_its_board() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    let path = checkout.to_str().unwrap();
    let link = "https://github.com/example/bookshop";
    assert_eq!(ok(&db, &["repo", "add", path, "--link", link]), "R1\n");
    ok(
        &db,
        &["milestone", "add", "MVP", "--goal", "g", "--repo", "R1"],
    );
    ok(&db, &["milestone", "add", "Later", "--goal", "g"]);
    assert_eq!(
        ok(&db, &["task", "add", "Sort the shelves", "--repo", "R1"]),
        "#1\n"
    );
    ok(
        &db,
        &[
            "lead",
            "add",
            "R1",
            "--name",
            "Cat",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "cat-lead", "--by", "cat-lead", "--task", "1", "Shelves", "go",
        ],
    );
    let out = tower(
        &db,
        &[
            "thread", "new", "cat-lead", "--by", "cat-lead", "Triage", "go",
        ],
    );
    assert_eq!(text(&out.stderr), "opened thread T2 on the board of R1\n");
    let real = checkout.canonicalize().unwrap();
    assert_eq!(
        ok(&db, &["repo", "show", "R1"]),
        format!(
            "R1\n\
             \x20 path       {}\n\
             \x20 forge      {link}\n\
             \x20 lead       Cat (session cat-lead, colour blue)\n\
             milestones\n\
             \x20 M1 MVP\n\
             standalone tasks\n\
             \x20 #1 Sort the shelves (no ticket)\n\
             threads\n\
             \x20 T2 Triage (open, by cat-lead, 0 replies)\n\
             \x20 T1 Shelves (open, about #1, by cat-lead, 0 replies)\n",
            real.display()
        )
    );
    // The standalone task's thread shows with it; the repo-wide one with the milestone.
    let shown = ok(&db, &["task", "show", "1"]);
    assert!(
        shown.ends_with("threads\n  T1 Shelves (open, by cat-lead, 0 replies)\n"),
        "{shown}"
    );
    let shown = ok(&db, &["milestone", "show", "M1"]);
    assert!(
        shown.contains("  T2 Triage (open, by cat-lead, 0 replies)\n"),
        "{shown}"
    );
    // A repo-wide thread's board is the repo.
    assert!(ok(&db, &["thread", "show", "T2"]).contains("\n  board      R1\n"));
    // M2, in no repo, points to milestone set --repo first; then it joins R1 and has its
    // lead, and says so.
    assert_refused(
        &tower(&db, &["thread", "new", "M2", "--by", "x", "t", "m"]),
        "M2 has no lead yet, so it has no message board; it is in no repo yet: \
         tower milestone set M2 --repo R<n> first, then tower lead add M2",
    );
    assert!(ok(&db, &["milestone", "show", "M2"]).contains(
        "\n  repo       none yet (tower milestone set M2 --repo R<n>)\n  lead       none yet\n"
    ));
    assert_eq!(
        ok(&db, &["milestone", "set", "M2", "--repo", "R1"]),
        "set the details of M2; it is in R1, led by Cat, and its 0 thread(s) are on R1's board\n"
    );
    assert!(ok(&db, &["milestone", "show", "M2"]).contains("  lead       Cat (session cat-lead"));
    // A milestone with threads does not move to a repo with no lead.
    ok(
        &db,
        &["thread", "new", "M2", "--by", "cat-lead", "About M2", "go"],
    );
    let other = dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    ok(&db, &["repo", "add", other.to_str().unwrap()]);
    assert_refused(
        &tower(&db, &["milestone", "set", "M2", "--repo", "R2"]),
        "M2 has 1 thread(s), and R2 has no lead to read them there; add its lead first: \
         tower lead add R2",
    );
    ok(
        &db,
        &["lead", "add", "R2", "--name", "Other", "--session", "other"],
    );
    assert_eq!(
        ok(&db, &["milestone", "set", "M2", "--repo", "R2"]),
        "set the details of M2; it is in R2, led by Other, and its 1 thread(s) are on R2's board\n"
    );
    assert!(ok(&db, &["thread", "show", "T3"]).contains("\n  board      R2, about M2 Later\n"));
    // repo list gives each repo on one line; one the schema 14 upgrade made has no path.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO repo (path) VALUES (NULL)", [])
        .unwrap();
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!(
            "R1 {}, lead Cat (session cat-lead), M1 #1\n\
             R2 {}, lead Other (session other), M2\n\
             R3 (path not set), no lead, no milestones or standalone tasks\n",
            real.display(),
            other.canonicalize().unwrap().display()
        )
    );
    let empty = dir.path().join("empty.db");
    assert_eq!(
        ok(&empty, &["repo", "list"]),
        "no repos yet: add one with tower repo add <main checkout>\n"
    );
    for (args, message) in [
        (&["repo", "add", path][..], "is already R1"),
        (&["repo", "add", "rel"], "the repo path rel is not absolute"),
        (
            &["repo", "show", "R99"],
            "there is no repo R99 on the board",
        ),
        (&["repo", "show", "M1"], "is not a repo id"),
        (
            &["task", "add", "x", "--repo", "R1", "--milestone", "M1"],
            "cannot be used with",
        ),
        (
            &["lead", "add", "M1", "--name", "Two", "--session", "two"],
            "R1 already has a lead, Cat",
        ),
    ] {
        assert_refused(&tower(&db, args), message);
    }
}

#[test]
fn the_hook_names_a_leads_unread_posts_once_and_says_how_to_arm_its_waiter() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "cat-lead", "M1", "Reviews", "opened",
        ],
    );
    ok(&db, &["post", "--by", "cat-lead", "T1", "my own"]);
    ok(&db, &["post", "--by", "dev-1", "T1", "status: while deaf"]);
    let mut store = Store::open(&db).unwrap();
    store
        .attach_lead(control_tower::board::RepoId(1), "sess-1")
        .unwrap();
    drop(store);
    let hook = |session: &str| {
        let json = format!(r#"{{"session_id":"{session}","hook_event_name":"Stop"}}"#);
        let out = tower_with_stdin(&db, &["lead", "hook"], json);
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout)
    };
    // Another session passes; the lead's is held once, naming what waits (not its own
    // post), and, with no waiter running, how to arm it.
    assert_eq!(hook("sess-9"), "", "another session was held");
    let held = hook("sess-1");
    let v: serde_json::Value = serde_json::from_str(&held).unwrap();
    assert_eq!(v["decision"], "block");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains("\n[T1] dev-1: status: while deaf\n"),
        "{reason}"
    );
    assert!(!reason.contains("my own"), "{reason}");
    assert!(reason.contains("tower lead wait R1"), "{reason}");
    assert_eq!(hook("sess-1"), "", "held twice for the same post");
}

/// A board whose R1 is led by Catalogue (session cat-lead), attached as the Claude session
/// sess-1, with a thread T1.
fn attached_lead_board() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "cat-lead", "M1", "Reviews", "opened",
        ],
    );
    let mut store = Store::open(&db).unwrap();
    store
        .attach_lead(control_tower::board::RepoId(1), "sess-1")
        .unwrap();
    (dir, db)
}

/// One look of the watch's lead check at `now`, with `claude agents --json` listing sess-1
/// as `status`.
fn lead_look(db: &Path, status: &str, now: i64) -> Vec<String> {
    let list = format!(r#"[{{"sessionId":"sess-1","kind":"interactive","status":"{status}"}}]"#);
    let mut store = Store::open(db).unwrap();
    let mut unheard = std::collections::HashMap::new();
    control_tower::listen::check_leads(&mut store, db, || Ok(list.clone()), now, &mut unheard)
}

/// Holds R1's listener lock as a running waiter would, until dropped.
fn r1_waiter_runs(db: &Path) -> std::fs::File {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(control_tower::listen::lock_path(
            db,
            control_tower::board::RepoId(1),
        ))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

/// The turns R1's lead has ended with no waiter running, by its hooks.
fn r1_turns(db: &Path) -> i64 {
    Store::open(db).unwrap().leads().unwrap()[0].turns
}

/// `tower lead hook` given `event`'s input for `session`: its stdout.
fn lead_hook(db: &Path, event: &str, session: &str) -> String {
    let json =
        format!(r#"{{"session_id":"{session}","hook_event_name":"{event}","error":"rate_limit"}}"#);
    let out = tower_with_stdin(db, &["lead", "hook"], json);
    assert!(out.status.success(), "{}", text(&out.stderr));
    text(&out.stdout)
}

#[test]
fn a_lead_turn_ended_on_an_api_error_is_counted_and_never_told() {
    let (_dir, db) = attached_lead_board();
    // The plugin runs tower lead hook for a turn that ends on an API error, too.
    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/claude-plugin/hooks/hooks.json"
        ))
        .unwrap(),
    )
    .unwrap();
    for event in ["Stop", "StopFailure", "SessionStart"] {
        assert_eq!(
            hooks["hooks"][event][0]["hooks"][0]["command"], "tower lead hook",
            "{event}"
        );
    }
    // A look sees its waiter; a post ends it; the lead works through two looks and the
    // turn fails on an API error before it re-arms, so no Stop hook runs.
    let held = r1_waiter_runs(&db);
    assert!(lead_look(&db, "idle", 10).is_empty());
    drop(held);
    ok(
        &db,
        &["post", "--by", "dev-1", "T1", "status: for the lead"],
    );
    assert!(lead_look(&db, "busy", 20).is_empty());
    assert!(lead_look(&db, "busy", 30).is_empty());
    // StopFailure: counted, nothing printed (Claude Code ignores it), the post still unread.
    assert_eq!(lead_hook(&db, "StopFailure", "sess-1"), "");
    let r1 = control_tower::board::RepoId(1);
    assert!(Store::open(&db).unwrap().has_unread(r1).unwrap(), "told");
    assert_eq!(r1_turns(&db), 1);
    // Idle with nothing to wake it: not listening.
    assert_eq!(
        lead_look(&db, "idle", 40),
        [
            "Catalogue, the lead of R1, is not listening (its Claude session finished a turn \
             without one); told the owner in T2"
        ]
    );
    // With its waiter running, or for another session: not counted.
    let held = r1_waiter_runs(&db);
    assert_eq!(lead_hook(&db, "StopFailure", "sess-1"), "");
    drop(held);
    assert_eq!(lead_hook(&db, "StopFailure", "sess-9"), "");
    assert_eq!(r1_turns(&db), 1);
}

#[test]
fn a_stop_counted_while_a_waiter_arms_is_absorbed_by_its_baseline() {
    use std::io::Write;
    use std::process::Stdio;
    let (_dir, db) = attached_lead_board();
    // Another writer holds the board, so the lead's Stop hook (no waiter running yet) queues
    // behind it.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_tower"))
        .args(["lead", "hook"])
        .env("TOWER_DB", &db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (hook.stdin.take().unwrap())
        .write_all(br#"{"session_id":"sess-1","hook_event_name":"Stop"}"#)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    // Meanwhile the re-armed waiter takes its lock and its baseline lands first, as
    // `tower lead wait` writes it.
    let _waiter = r1_waiter_runs(&db);
    conn.execute_batch("UPDATE lead SET quiet_turns = turns WHERE repo_id = 1; COMMIT")
        .unwrap();
    let out = hook.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    // The hook saw the waiter inside its own write: the count stays at the baseline.
    let lead = &Store::open(&db).unwrap().leads().unwrap()[0];
    assert_eq!(Some(lead.turns), lead.quiet_turns, "{lead:?}");
}

#[test]
fn a_turn_counted_just_before_the_re_armed_waiter_takes_its_lock_never_raises() {
    let (_dir, db) = attached_lead_board();
    // A look sees its waiter, which then ends.
    let held = r1_waiter_runs(&db);
    assert!(lead_look(&db, "idle", 10).is_empty());
    drop(held);
    // The lead re-arms and ends its turn, its Stop hook running before the new waiter holds
    // its lock: counted as a turn with no waiter.
    assert_eq!(lead_hook(&db, "Stop", "sess-1"), "");
    assert_eq!(r1_turns(&db), 1);
    // The new waiter takes its lock and a post ends it at once, before the next look.
    ok(
        &db,
        &["post", "--by", "dev-1", "T1", "status: for the lead"],
    );
    let woke = ok(&db, &["lead", "wait", "R1"]);
    assert!(woke.contains("1 unread post on the board of R1"), "{woke}");
    // That look finds it idle with no waiter, in the gap before it re-arms: nothing raised.
    assert!(lead_look(&db, "idle", 30).is_empty());
    assert!(
        Store::open(&db)
            .unwrap()
            .load()
            .unwrap()
            .inbox
            .awareness
            .is_empty()
    );
}

// macOS only (SDLC.md): a waiter knows the one it stops by its command line, and
// `ps -o comm=` gives the executable's path on macOS but only its name on Linux
// (`listen::listener_argument`). The product ships only for macOS.
#[cfg(target_os = "macos")]
#[test]
fn a_waiter_left_running_is_stopped_by_the_next_one_at_once() {
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "cat-lead", "M1", "Reviews", "opened",
        ],
    );
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_tower"))
            .args(["lead", "wait", "R1"])
            .env("TOWER_DB", &db)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    // Left running by a Claude Code that went away, with nothing unread: it waits on. (Its
    // output goes to a file Claude Code made, which stays; here a pipe nobody reads.)
    let mut orphan = spawn();
    let lock = db.with_file_name("board.db.R1.stream");
    let wait = |what: &str, done: &mut dyn FnMut() -> bool| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < until, "{what} never happened");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    wait("the orphan took the lock", &mut || {
        std::fs::read_to_string(&lock).is_ok_and(|p| p.trim() == orphan.id().to_string())
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        orphan.try_wait().unwrap().is_none(),
        "the orphan ended by itself"
    );
    // The re-arm: a new waiter stops it at once and holds the lock.
    let mut rearmed = spawn();
    assert!(ended(&mut orphan, "the waiter left running").success());
    wait("the new waiter took the lock", &mut || {
        std::fs::read_to_string(&lock).is_ok_and(|p| p.trim() == rearmed.id().to_string())
    });
    assert!(
        rearmed.try_wait().unwrap().is_none(),
        "the new waiter ended"
    );
    rearmed.kill().unwrap();
    let out = rearmed.wait_with_output().unwrap();
    let said = text(&out.stderr);
    assert!(
        said.contains(&format!(
            "stopped the earlier board listener for R1 (process {})",
            orphan.id()
        )),
        "{said}"
    );
}

#[test]
fn a_waiter_never_signals_what_holds_its_lock_unless_it_is_a_waiter_for_that_repo() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    for m in ["M1", "M2"] {
        ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
        in_repo(&db, m);
        let session = format!("lead-{m}");
        ok(
            &db,
            &["lead", "add", m, "--name", &session, "--session", &session],
        );
    }
    // A real waiter, but for R2, and a process that is no waiter at all.
    let mut r2 = Command::new(env!("CARGO_BIN_EXE_tower"))
        .args(["lead", "wait", "R2"])
        .env("TOWER_DB", &db)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    // This test process holds R1's lock; the file names, in turn, itself, the sleep and the
    // R2 waiter. Each is refused and nothing is signalled.
    let mut held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(db.with_file_name("board.db.R1.stream"))
        .unwrap();
    held.try_lock().unwrap();
    for pid in [std::process::id(), sleeper.id(), r2.id()] {
        held.set_len(0).unwrap();
        std::io::Seek::rewind(&mut held).unwrap();
        writeln!(held, "{pid}").unwrap();
        let out = tower(&db, &["lead", "wait", "R1"]);
        assert_refused(
            &out,
            "is locked by something that is not a board listener for R1",
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        sleeper.try_wait().unwrap().is_none(),
        "the sleep was signalled"
    );
    assert!(
        r2.try_wait().unwrap().is_none(),
        "R2's waiter was signalled"
    );
    // Let go of R1's lock explicitly, not by closing it.
    held.unlock().unwrap();
    drop(held);
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
    r2.kill().unwrap();
    r2.wait().unwrap();
}

/// Sends SIGTERM to `pid`.
fn nix_term(pid: u32) {
    let out = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// Waits up to 10 s for `child` to end, killing it and failing when it does not.
#[track_caller]
fn ended(child: &mut std::process::Child, what: &str) -> std::process::ExitStatus {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() > until {
            let _ = child.kill();
            panic!("{what} did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Counts the rows `sql` selects on the board at `db`.
fn count(db: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

#[test]
fn repo_remove_refuses_a_repo_holding_work_and_removes_an_empty_one_with_its_lead_and_board() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // R1 keeps working: a milestone, its lead and a thread with an inbox item.
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &["lead", "add", "R1", "--name", "Keep", "--session", "keep"],
    );
    ok(
        &db,
        &[
            "thread",
            "new",
            "--by",
            "keep",
            "M1",
            "Kept",
            "@manager fyi",
        ],
    );
    // R2 as the schema 14 upgrade left one: no path, an old lead, its threads, one of them
    // the watch's "Lead not listening", an inbox question and a message read by the owner.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO repo (path) VALUES (NULL)", [])
        .unwrap();
    ok(
        &db,
        &[
            "lead",
            "add",
            "R2",
            "--name",
            "Old",
            "--session",
            "old-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "old-lead", "old-lead", "Old plan", "go",
        ],
    );
    ok(
        &db,
        &[
            "post",
            "--by",
            "dev-1",
            "T2",
            "question: @manager drop this?",
        ],
    );
    ok(
        &db,
        &["post", "--by", "dev-1", "T2", "@manager read already"],
    );
    // Quoting the owner is no mention: the inbox never listed it, so it is not counted.
    ok(
        &db,
        &["post", "--by", "dev-1", "T2", "> @manager quoted only"],
    );
    let mut store = Store::open(&db).unwrap();
    let r2 = control_tower::board::RepoId(2);
    let deaf = store.lead_deaf(r2, "not listening", 5).unwrap();
    assert_eq!(deaf.map(|t| t.to_string()).as_deref(), Some("T3"));
    drop(store);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE message SET read_at = 9 WHERE text = '@manager read already'",
            [],
        )
        .unwrap();
    // Work in R2 is refused, all of it named, a Done milestone too, and nothing goes. The
    // standalone task has a thread of its own.
    ok(
        &db,
        &["milestone", "add", "Later", "--goal", "g", "--repo", "R2"],
    );
    ok(&db, &["task", "add", "step", "--milestone", "M2"]);
    ok(&db, &["task", "stage", "1", "done"]);
    ok(&db, &["milestone", "done", "M2"]);
    ok(&db, &["task", "add", "loose", "--repo", "R2"]);
    ok(
        &db,
        &[
            "thread", "new", "--by", "old-lead", "--task", "2", "old-lead", "Loose", "about it",
        ],
    );
    let before = count(&db, "SELECT count(*) FROM message");
    assert_refused(
        &tower(&db, &["repo", "remove", "R2"]),
        "R2 still holds M2, #2; move each to another repo first with tower milestone set \
         M<n> --repo R<n> or tower task set <n> --repo R<n>, or remove an abandoned \
         standalone task with tower task remove <n>",
    );
    assert_refused(&tower(&db, &["repo", "remove", "R1"]), "R1 still holds M1");
    assert_eq!(count(&db, "SELECT count(*) FROM message"), before);
    assert!(ok(&db, &["repo", "show", "R2"]).contains("lead       Old (session old-lead"));
    // Moved out (the task's thread T4 with it), R2 goes with its lead, its two threads and
    // their five messages, one of them an inbox item: the question (the one read is not
    // listed); R1 keeps everything, T4 included.
    ok(&db, &["milestone", "set", "M2", "--repo", "R1"]);
    ok(&db, &["task", "set", "2", "--repo", "R1"]);
    assert_eq!(
        ok(&db, &["repo", "remove", "R2"]),
        "removed R2: its lead Old (session old-lead), 2 threads, 5 messages, 1 inbox item\n"
    );
    assert_eq!(before - count(&db, "SELECT count(*) FROM message"), 5);
    assert!(ok(&db, &["thread", "show", "T4"]).contains("\n  board      R1\n"));
    for (sql, left) in [
        ("SELECT count(*) FROM repo", 1),
        ("SELECT count(*) FROM lead", 1),
        ("SELECT count(*) FROM thread", 2),
        ("SELECT count(*) FROM message", 2),
        ("SELECT count(*) FROM lead WHERE session = 'keep'", 1),
    ] {
        assert_eq!(count(&db, sql), left, "{sql}");
    }
    // No row anywhere points at what went.
    assert_eq!(
        count(&db, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
    let inbox = Store::open(&db).unwrap().load().unwrap().inbox;
    assert_eq!(inbox.awareness.len(), 1, "{inbox:?}");
    assert!(inbox.need_you.is_empty(), "{inbox:?}");
    assert_eq!(
        ok(&db, &["repo", "list"]),
        format!(
            "R1 {}, lead Keep (session keep), M1 M2 #2\n",
            db.with_file_name("checkout-M1")
                .canonicalize()
                .unwrap()
                .display()
        )
    );
    // Gone: removing it again, or showing it, says so; its lead's session is free again.
    assert_refused(
        &tower(&db, &["repo", "remove", "R2"]),
        "there is no repo R2 on the board",
    );
    assert_refused(
        &tower(&db, &["repo", "show", "R2"]),
        "there is no repo R2 on the board",
    );
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO repo (path) VALUES (NULL)", [])
        .unwrap();
    ok(
        &db,
        &[
            "lead",
            "add",
            "R3",
            "--name",
            "Old",
            "--session",
            "old-lead",
        ],
    );
    // An empty repo with a lead and no threads, or with neither, says so.
    assert_eq!(
        ok(&db, &["repo", "remove", "R3"]),
        "removed R3: its lead Old (session old-lead), 0 threads, 0 messages, 0 inbox items\n"
    );
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO repo (path) VALUES (NULL)", [])
        .unwrap();
    assert_eq!(
        ok(&db, &["repo", "remove", "R4"]),
        "removed R4: no lead, 0 threads, 0 messages, 0 inbox items\n"
    );
}

#[test]
fn repo_remove_and_a_refused_waiter_delete_a_stale_lock_but_never_one_a_waiter_holds() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let empty_repo = || {
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute("INSERT INTO repo (path) VALUES (NULL)", [])
            .unwrap();
    };
    ok(&db, &["repo", "list"]);
    // R1's last waiter ended long ago and left its lock file, emptied.
    empty_repo();
    let r1 = db.with_file_name("board.db.R1.stream");
    std::fs::write(&r1, "").unwrap();
    ok(&db, &["repo", "remove", "R1"]);
    assert!(!r1.exists(), "a lock nobody holds was left behind");
    // R2's lock is held (as by a waiter between two looks): it stays for its holder.
    empty_repo();
    let r2 = db.with_file_name("board.db.R2.stream");
    let held = std::fs::File::create(&r2).unwrap();
    held.try_lock().unwrap();
    ok(&db, &["repo", "remove", "R2"]);
    assert!(r2.exists(), "a held lock was deleted under its waiter");
    // Its holder killed before its next look, the file stays: the next remove of R2, though
    // refused, clears it. Let go explicitly, not by closing: a child forked meanwhile by
    // another test would keep the lock.
    held.unlock().unwrap();
    drop(held);
    assert!(r2.exists());
    assert_refused(
        &tower(&db, &["repo", "remove", "R2"]),
        "there is no repo R2 on the board",
    );
    assert!(!r2.exists(), "a refused remove left a stale lock");
    // So does a waiter refused for a repo not on the board, but never under a holder.
    std::fs::write(&r1, "").unwrap();
    assert_refused(
        &tower(&db, &["lead", "wait", "R1"]),
        "there is no repo R1 on the board",
    );
    assert!(!r1.exists(), "a refused waiter left a stale lock");
    let held = std::fs::File::create(&r1).unwrap();
    held.try_lock().unwrap();
    assert_refused(
        &tower(&db, &["lead", "wait", "R1"]),
        "there is no repo R1 on the board",
    );
    assert!(r1.exists(), "a refused waiter deleted a held lock");
    held.unlock().unwrap();
    drop(held);
    // R3 never had a waiter: nothing to delete, and no lock file made.
    empty_repo();
    ok(&db, &["repo", "remove", "R3"]);
    assert!(!db.with_file_name("board.db.R3.stream").exists());
}

/// A child killed and reaped when dropped, so a failing test leaves no waiter running.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn task_remove_refuses_what_is_not_abandoned_and_removes_a_standalone_task_so_its_repo_can_go() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    ok(&db, &["repo", "add", checkout.to_str().unwrap()]);
    ok(
        &db,
        &["lead", "add", "R1", "--name", "Lead", "--session", "lead-1"],
    );
    // #1 Spike, standalone in R1, with subtasks #2 and #3, a criterion, waiting for #4,
    // with a thread T1 of two messages, one of them a question for the owner; #4 Other,
    // standalone in R1; #5 under M1, in no repo, waiting for #1; a repo-wide thread T2.
    ok(&db, &["task", "add", "--repo", "R1", "Spike"]);
    ok(&db, &["subtask", "add", "--task", "1", "Sub a"]);
    ok(&db, &["subtask", "add", "--task", "1", "Sub b"]);
    ok(&db, &["task", "add", "--repo", "R1", "Other"]);
    ok(&db, &["milestone", "add", "Mile", "--goal", "g"]);
    ok(&db, &["task", "add", "--milestone", "M1", "Under"]);
    ok(&db, &["task", "criterion", "add", "1", "Spiked"]);
    ok(&db, &["task", "blocked-by", "add", "1", "4"]);
    ok(&db, &["task", "blocked-by", "add", "5", "1"]);
    ok(
        &db,
        &[
            "thread", "new", "--by", "dev-3", "--task", "1", "lead-1", "Spike", "starting",
        ],
    );
    ok(
        &db,
        &[
            "post",
            "--by",
            "dev-3",
            "T1",
            "@manager should we drop this?",
        ],
    );
    ok(
        &db,
        &[
            "thread",
            "new",
            "--by",
            "lead-1",
            "lead-1",
            "Repo",
            "about the repo",
        ],
    );
    // The watch's rows on #1, as `tower watch` writes them.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "INSERT INTO watch_flag (task_id, signal, worker, raised_at) \
             VALUES (1, 'silent', 'dev-3', 1); \
             INSERT INTO watch_seen (task_id, worker, since) VALUES (1, 'dev-3', 1);",
        )
        .unwrap();
    let before = ok(&db, &["task", "show", "1"]);

    // Refused, changing nothing: what is not a standalone task, an assigned one, and one
    // another task waits for.
    let refusals = [
        (vec!["99"], "there is no task #99 on the board"),
        (
            vec!["2"],
            "#2 is a subtask of #1; only a standalone task is removed, with its subtasks",
        ),
        (
            vec!["5"],
            "#5 is under M1; only a standalone task is removed",
        ),
        (
            vec!["1"],
            "#1 cannot go while other tasks wait for it: #5 is blocked by #1; take each \
             away first with tower task blocked-by remove 5 1",
        ),
    ];
    for (args, refusal) in refusals {
        let args: Vec<&str> = ["task", "remove"].into_iter().chain(args).collect();
        assert_refused(&tower(&db, &args), refusal);
        assert_eq!(ok(&db, &["task", "show", "1"]), before, "{args:?}");
    }
    ok(&db, &["task", "add", "--repo", "R1", "Later"]);
    ok(&db, &["task", "blocked-by", "add", "6", "1"]);
    assert_refused(
        &tower(&db, &["task", "remove", "1"]),
        "#1 cannot go while other tasks wait for it: #5 is blocked by #1, #6 is blocked by \
         #1; take each away first with tower task blocked-by remove 5 1 and tower task \
         blocked-by remove 6 1",
    );
    ok(&db, &["task", "blocked-by", "remove", "5", "1"]);
    ok(&db, &["task", "blocked-by", "remove", "6", "1"]);
    // A subtask in a blocked-by: `blocked-by add` refuses one (details belong to tasks), but
    // the schema does not, so a board written otherwise may hold it. An outside task
    // waiting for subtask #2 refuses the removal, naming it, and `blocked-by remove` frees
    // it; a dependency inside the task (#3 waiting for #2) refuses nothing and goes with it.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("INSERT INTO blocker (task_id, blocker_id) VALUES (6, 2), (3, 2)")
        .unwrap();
    let before = ok(&db, &["task", "show", "1"]);
    assert_refused(
        &tower(&db, &["task", "remove", "1"]),
        "#1 cannot go while other tasks wait for it: #6 is blocked by #2; take each away \
         first with tower task blocked-by remove 6 2",
    );
    assert_eq!(ok(&db, &["task", "show", "1"]), before);
    ok(&db, &["task", "blocked-by", "remove", "6", "2"]);
    assert_eq!(
        count(&db, "SELECT count(*) FROM blocker WHERE task_id = 3"),
        1
    );
    ok(&db, &["task", "set", "1", "--assignee", "dev-3"]);
    let before = ok(&db, &["task", "show", "1"]);
    assert_refused(
        &tower(&db, &["task", "remove", "1"]),
        "#1 is assigned to dev-3; if nobody is on it any more, unassign it first with tower \
         task unassign 1",
    );
    assert_eq!(ok(&db, &["task", "show", "1"]), before);
    ok(&db, &["task", "unassign", "1"]);

    // Removed with all that hangs on it, and nothing else.
    assert_eq!(
        ok(&db, &["task", "remove", "#1"]),
        "removed #1 Spike: 2 subtasks, 1 criterion, 1 thread, 2 messages, 1 inbox item\n"
    );
    for gone in ["1", "2", "3"] {
        assert_refused(
            &tower(&db, &["task", "show", gone]),
            &format!("there is no task #{gone} on the board"),
        );
    }
    assert_refused(
        &tower(&db, &["thread", "show", "T1"]),
        "there is no thread T1 on the board",
    );
    for table in ["criterion", "blocker", "watch_flag", "watch_seen"] {
        assert_eq!(
            count(&db, &format!("SELECT count(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    assert_eq!(count(&db, "SELECT count(*) FROM thread"), 1, "T2 stays");
    assert_eq!(
        count(&db, "SELECT count(*) FROM task"),
        3,
        "#4, #5 and #6 stay"
    );
    assert!(ok(&db, &["task", "show", "4"]).contains("  repo       R1\n"));

    // A Done task goes by the same rules; with R1 empty, it goes too.
    ok(&db, &["task", "stage", "4", "done"]);
    assert_eq!(
        ok(&db, &["task", "remove", "4"]),
        "removed #4 Other: 0 subtasks, 0 criteria, 0 threads, 0 messages, 0 inbox items\n"
    );
    ok(&db, &["task", "remove", "6"]);
    assert_eq!(
        ok(&db, &["repo", "remove", "R1"]),
        "removed R1: its lead Lead (session lead-1), 1 thread, 1 message, 0 inbox items\n"
    );
}

#[test]
fn a_relative_tower_db_is_handed_on_as_the_board_it_names_here() {
    let dir = tempfile::tempdir().unwrap();
    let shop = dir.path().join("shop");
    std::fs::create_dir(&shop).unwrap();
    // A claude that records the TOWER_DB tower hands it, as a lead started from `n` gets it,
    // and lists no session, so the attach that asks it goes no further.
    let claude = dir.path().join("claude");
    let handed = dir.path().join("handed");
    test_exe::executable(
        &claude,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$TOWER_DB\" > '{}'\necho '[]'\n",
            handed.display()
        ),
    );
    let relative = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tower"))
            .args(args)
            .current_dir(&shop)
            .env("TOWER_DB", "board.db")
            .env("TOWER_CLAUDE", &claude)
            .env("CLAUDE_CODE_SESSION_ID", "sess-1")
            .env("HOME", dir.path())
            .output()
            .unwrap()
    };
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    for args in [
        &["milestone", "add", "M", "--goal", "g"][..],
        &["repo", "add", checkout.to_str().unwrap()],
        &[
            "lead",
            "add",
            "R1",
            "--name",
            "L",
            "--session",
            "shelf-lead",
        ],
    ] {
        let out = relative(args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    relative(&["lead", "attach", "R1"]);
    // The board named from `shop`, absolute, so a process started in any other directory
    // opens the same file.
    let board = shop.canonicalize().unwrap().join("board.db");
    assert_eq!(
        std::fs::read_to_string(&handed).unwrap(),
        format!("{}\n", board.display())
    );
    assert!(
        Store::open(&board)
            .unwrap()
            .load()
            .unwrap()
            .milestones
            .len()
            == 1
    );
    // With no working directory, a relative TOWER_DB names nothing: refused, never guessed.
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg("mkdir gone && cd gone && rmdir ../gone && exec \"$0\" repo list")
        .arg(env!("CARGO_BIN_EXE_tower"))
        .current_dir(&shop)
        .env("TOWER_DB", "board.db")
        .output()
        .unwrap();
    assert_refused(
        &out,
        "the board's path board.db is relative and the working directory is unknown; give \
         TOWER_DB an absolute path",
    );
}

#[test]
fn a_board_waiter_ends_on_an_unread_post_tells_nothing_and_unread_tells_it_once() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "cat-lead", "M1", "Reviews", "opened",
        ],
    );
    let spawn = |args: &[&str]| {
        Reaped(
            Command::new(env!("CARGO_BIN_EXE_tower"))
                .args(args)
                .env("TOWER_DB", &db)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let wait = || spawn(&["lead", "wait", "R1"]);
    // Its whole output once it has ended, which it must within 10 s, exit 0.
    let out = |mut w: Reaped| {
        assert!(ended(&mut w.0, "the waiter").success());
        let mut s = String::new();
        std::io::Read::read_to_string(&mut w.0.stdout.take().unwrap(), &mut s).unwrap();
        s
    };
    let unread = || ok(&db, &["lead", "unread", "R1"]);
    let lock = db.with_file_name("board.db.R1.stream");
    let held_by = |w: &Reaped| {
        let until = Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(&lock).unwrap_or_default().trim() != w.0.id().to_string() {
            assert!(Instant::now() < until, "the waiter never took the lock");
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Nothing unread: it waits, holding the lock (listening), and the lead's own post and
    // the watch's are not its to wake it for.
    let mut w = wait();
    held_by(&w);
    ok(&db, &["post", "--by", "cat-lead", "T1", "my own"]);
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        w.0.try_wait().unwrap().is_none(),
        "it ended with nothing unread"
    );
    // While it runs, unread says so: a lead woken otherwise does not arm another.
    assert_eq!(
        unread(),
        "control-tower: a board waiter for R1 is already running, so do not arm another.\n"
    );
    // A post: it ends, saying how to read it, and lets go of the lock.
    ok(&db, &["post", "--by", "dev-1", "T1", "status: first"]);
    let one = "control-tower: 1 unread post on the board of R1: read it with tower lead \
               unread R1, then arm this waiter again.\n";
    assert_eq!(out(w), one);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "");
    // It told nothing: a waiter armed again ends at once on the same post.
    let start = Instant::now();
    assert_eq!(out(wait()), one);
    assert!(start.elapsed() < Duration::from_secs(5));
    // Posted between the waiter's end and the lead's read: read with it, never skipped.
    ok(&db, &["post", "--by", "qa", "T1", "status: in the gap"]);
    assert_eq!(
        unread(),
        "[T1] dev-1: status: first\n[T1] qa: status: in the gap\n"
    );
    // Told once: nothing the second time, and no listener runs, so no line about one.
    assert_eq!(unread(), "");
    // Posted after the read and before the re-arm: the re-armed waiter ends at once.
    ok(
        &db,
        &["post", "--by", "qa", "T1", "status: before the re-arm"],
    );
    assert_eq!(out(wait()), one);
    assert_eq!(unread(), "[T1] qa: status: before the re-arm\n");

    // The newest waiter wins: each stops the one before it, which says so and ends
    // cleanly. On macOS only, as a waiter knows another by its command line, which `ps`
    // gives as macOS does (`a_waiter_left_running_is_stopped_by_the_next_one_at_once`);
    // elsewhere one waiter runs on.
    let last = if cfg!(target_os = "macos") {
        let first = wait();
        held_by(&first);
        let second = wait();
        held_by(&second);
        let stopped = "control-tower: this board waiter for R1 was stopped (a newer one took \
                       over, or it was asked to stop); tower lead unread R1 says whether one \
                       runs.\n";
        assert_eq!(out(first), stopped);
        let third = wait();
        held_by(&third);
        assert_eq!(out(second), stopped);
        third
    } else {
        let only = wait();
        held_by(&only);
        only
    };

    // Its repo removed (M1 moved to R2 first, as repo remove asks; a milestone with threads
    // moves only to a repo with a lead): it ends saying not to arm it again, and its lock
    // file goes.
    let r2 = dir.path().join("elsewhere");
    std::fs::create_dir_all(r2.join(".git")).unwrap();
    ok(&db, &["repo", "add", r2.to_str().unwrap()]);
    ok(
        &db,
        &[
            "lead",
            "add",
            "R2",
            "--name",
            "Two",
            "--session",
            "two-lead",
        ],
    );
    ok(&db, &["milestone", "set", "M1", "--repo", "R2"]);
    ok(&db, &["repo", "remove", "R1"]);
    assert_eq!(
        out(last),
        "control-tower: R1 was removed from the board (tower repo remove); this waiter ends, \
         so do not arm it again.\n"
    );
    assert!(!lock.exists());
}

/// qa-55's finding on PR #121: the relay, a Claude session named control-tower, ran tower's
/// SessionStart hook on the board and took over a lead registered under that name. In the
/// relay (TOWER_RELAY set) every hook does nothing.
#[test]
fn no_hook_in_the_relay_touches_the_board_even_for_a_lead_named_like_it() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // A fake claude listing the relay's session and the lead's, both named control-tower,
    // leaving a mark that it was asked. Never the real claude.
    let claude = dir.path().join("claude");
    test_exe::executable(
        &claude,
        "#!/bin/sh\n[ \"$1 $2\" = \"agents --json\" ] || exit 2\n: > \"$(dirname \"$0\")/asked\"\n\
         echo '[{\"sessionId\":\"sess-r\",\"name\":\"control-tower\",\"kind\":\"interactive\"},\
         {\"sessionId\":\"sess-l\",\"name\":\"control-tower\",\"kind\":\"interactive\"}]'\n",
    );
    let asked = dir.path().join("asked");
    let hook = |event: &str, session: &str, relay: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tower"));
        cmd.args(["lead", "hook"])
            .env("TOWER_DB", &db)
            .env("TOWER_CLAUDE", &claude)
            .env("REX_SESSION", "session:7")
            .env_remove("TOWER_RELAY");
        if relay {
            cmd.env("TOWER_RELAY", "1");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = serde_json::json!({
            "session_id": session, "hook_event_name": event, "source": "startup",
            "session_title": "control-tower", "cwd": "/anywhere", "error": "rate_limit"
        });
        (child.stdin.take().unwrap())
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{event}: {}", text(&out.stderr));
        out
    };
    let session = || ok(&db, &["milestone", "show", "M1"]);
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Tower",
            "--session",
            "control-tower",
        ],
    );
    ok(
        &db,
        &[
            "thread",
            "new",
            "--by",
            "control-tower",
            "M1",
            "Reviews",
            "opened",
        ],
    );
    // The relay starting: nothing printed, claude never asked, the lead not attached.
    let out = hook("SessionStart", "sess-r", true);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(!asked.exists());
    assert!(session().contains("  session    not attached (tower lead attach)\n"));
    // The lead's own start attaches it, as the relay's start did before.
    let out = hook("SessionStart", "sess-l", false);
    assert!(!out.stdout.is_empty());
    assert!(session().contains("  session    attached, Claude session sess-l\n"));
    // A relay starting later never takes it over.
    hook("SessionStart", "sess-r", true);
    assert!(session().contains("  session    attached, Claude session sess-l\n"));
    // With a post unread, a Stop or StopFailure in a relay, even under the lead's own
    // session id, passes, counts no turn and tells nothing.
    ok(
        &db,
        &["post", "--by", "dev-1", "T1", "status: for the lead"],
    );
    let turns = r1_turns(&db);
    for event in ["Stop", "StopFailure"] {
        let out = hook(event, "sess-l", true);
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{event}");
    }
    assert_eq!(r1_turns(&db), turns);
    let r1 = control_tower::board::RepoId(1);
    assert!(Store::open(&db).unwrap().has_unread(r1).unwrap(), "told");
    // Outside a relay the same Stop is held for the post.
    let out = hook("Stop", "sess-l", false);
    assert!(
        text(&out.stdout).contains("\"decision\":\"block\""),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn a_restarted_leads_session_start_attaches_it_and_never_blocks_a_start() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    // A fake claude: `agents --json` prints `sessions.json` and leaves a mark that it was
    // asked, or fails without it. Never the real claude.
    let claude = dir.path().join("claude");
    test_exe::executable(
        &claude,
        "#!/bin/sh\n[ \"$1 $2\" = \"agents --json\" ] || exit 2\n\
         d=$(dirname \"$0\")\n: > \"$d/asked\"\n\
         [ -f \"$d/sessions.json\" ] || { echo 'not logged in' >&2; exit 1; }\n\
         cat \"$d/sessions.json\"\n",
    );
    let asked = dir.path().join("asked");
    // In Rex unless `rex` is false, when no REX_SESSION is set.
    let hook_in = |input: serde_json::Value, rex: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tower"));
        cmd.env("REX_SESSION", "session:7");
        if !rex {
            cmd.env_remove("REX_SESSION");
        }
        let mut child = cmd
            .args(["lead", "hook"])
            .env("TOWER_DB", &db)
            .env("TOWER_CLAUDE", &claude)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        (child.stdin.take().unwrap())
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let hook = |input: serde_json::Value| hook_in(input, true);
    let start = |session: &str, title: &str| {
        serde_json::json!({
            "session_id": session, "hook_event_name": "SessionStart", "source": "resume",
            "session_title": title, "cwd": "/anywhere"
        })
    };
    // No board yet: every start passes, nothing made, claude never asked.
    let out = hook(start("sess-2", "cat-lead"));
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(!db.exists() && !asked.exists());
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(
        &db,
        &[
            "thread", "new", "--by", "cat-lead", "M1", "Reviews", "opened",
        ],
    );
    // A session that is no lead's: passes, claude never asked.
    let out = hook(start("sess-9", "dev-4"));
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(!asked.exists());
    // The lead's, with claude failing: the start goes on, nothing printed, the reason on
    // stderr (Claude Code's debug log), nothing attached.
    let out = hook(start("sess-2", "Cat-Lead"));
    assert!(
        out.status.success(),
        "a failed attach stopped a session start"
    );
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("not logged in"),
        "{}",
        text(&out.stderr)
    );
    assert!(asked.exists());
    assert!(
        ok(&db, &["milestone", "show", "M1"])
            .contains("  session    not attached (tower lead attach)\n")
    );
    // The lead's, with claude listing it under the name: attached, and told what to do.
    std::fs::write(
        dir.path().join("sessions.json"),
        r#"[{"sessionId":"sess-2","name":"Cat-Lead","kind":"interactive"}]"#,
    )
    .unwrap();
    let out = hook(start("sess-2", "Cat-Lead"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(
        (v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap())
        .starts_with("control-tower: re-attached as Catalogue, the lead of R1;"),
        "{v}"
    );
    assert!(
        ok(&db, &["milestone", "show", "M1"])
            .contains("  session    attached, Claude session sess-2\n")
    );
    // Its Stop hook now holds it while a post is unread; the old Stop path is unchanged.
    ok(
        &db,
        &["post", "--by", "dev-1", "T1", "status: after the restart"],
    );
    let out = hook(serde_json::json!({"session_id": "sess-2", "hook_event_name": "Stop"}));
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["decision"], "block");
    // In Rex the reply ends where it did; outside Rex (no REX_SESSION) the same start is
    // attached and told, after the rest, what the board does not see from there.
    let context = |out: Output| {
        assert!(out.status.success(), "{}", text(&out.stderr));
        let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let inside = context(hook(start("sess-2", "Cat-Lead")));
    assert!(
        inside.ends_with("(lead skill, New issues and idle).") && !inside.contains("Rex"),
        "{inside}"
    );
    let outside = context(hook_in(start("sess-2", "Cat-Lead"), false));
    assert_eq!(
        outside,
        format!(
            "{inside}\ncontrol-tower: this session runs outside Rex (REX_SESSION is not set), \
             but the board's team runs in Rex. Outside Rex the board does not see the workers \
             you start: not their prompts, and posts and nudges are not delivered to them. \
             Raise it with the owner in a question: post to @manager."
        )
    );
    assert!(
        ok(&db, &["milestone", "show", "M1"])
            .contains("  session    attached, Claude session sess-2\n")
    );
}

#[test]
fn a_session_start_never_fails_on_the_boards_path() {
    use std::io::Write;
    use std::process::Stdio;
    // The board's path cannot be had: TOWER_DB empty, or neither TOWER_DB nor HOME set (so
    // never the owner's board). A SessionStart passes quietly; the reason goes to stderr.
    let input = r#"{"session_id":"s","hook_event_name":"SessionStart","source":"startup","session_title":"cat-lead"}"#;
    for (db, why) in [
        (Some(""), "TOWER_DB is set but empty"),
        (None, "HOME is not set"),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tower"));
        cmd.args(["lead", "hook"])
            .env_remove("TOWER_DB")
            .env_remove("HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(db) = db {
            cmd.env("TOWER_DB", db);
        }
        let mut child = cmd.spawn().unwrap();
        (child.stdin.take().unwrap())
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{why}: a session start failed");
        assert!(out.stdout.is_empty(), "{why}: {}", text(&out.stdout));
        let err = text(&out.stderr);
        assert!(
            err.starts_with("tower lead hook (SessionStart): ") && err.contains(why),
            "{err}"
        );
    }
}

#[test]
fn lead_remove_takes_off_a_lead_nobody_runs_and_keeps_its_repos_work() {
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    ok(&db, &["task", "add", "--milestone", "M1", "Reviews"]);
    ok(
        &db,
        &["thread", "new", "--by", "dev-1", "M1", "Reviews", "opened"],
    );
    // The watch had told the owner it was not listening: T2, open.
    let r1 = control_tower::board::RepoId(1);
    let t2 = Store::open(&db)
        .unwrap()
        .lead_deaf(
            r1,
            "@manager Catalogue, the lead of R1, is not listening",
            1,
        )
        .unwrap()
        .unwrap();
    assert_eq!(t2.to_string(), "T2");
    let wait = || {
        Reaped(
            Command::new(env!("CARGO_BIN_EXE_tower"))
                .args(["lead", "wait", "R1"])
                .env("TOWER_DB", &db)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let lock = db.with_file_name("board.db.R1.stream");
    let held_by = |w: &Reaped| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::fs::read_to_string(&lock).unwrap_or_default().trim() != w.0.id().to_string() {
            assert!(
                std::time::Instant::now() < until,
                "the waiter never took the lock"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    // A session leads R1 while its waiter runs (its posts read, so the waiter waits):
    // refused, and nothing changes.
    ok(&db, &["lead", "unread", "R1"]);
    let mut w = wait();
    held_by(&w);
    assert_refused(
        &tower(&db, &["lead", "remove", "R1"]),
        "Catalogue is listening: a board waiter for R1 runs, so a session still leads it; close \
         that session (or stop its waiter) first, then tower lead remove R1",
    );
    assert!(ok(&db, &["repo", "show", "R1"]).contains("  lead       Catalogue (session cat-lead"));
    nix_term(w.0.id());
    assert!(ended(&mut w.0, "the waiter").success());
    // Nobody runs it: removed, its episode resolved, and its repo's work kept.
    assert_eq!(
        ok(&db, &["lead", "remove", "M1"]),
        "removed the lead of R1, Catalogue (session cat-lead, colour blue); its milestones, tasks \
         and threads stay for the next lead (tower lead add R1); resolved T2, its \"Lead not \
         listening\" thread\n"
    );
    let shown = ok(&db, &["repo", "show", "R1"]);
    assert!(shown.contains("  lead       none yet"), "{shown}");
    assert!(shown.contains("  M1 MVP"), "{shown}");
    assert!(shown.contains("T1 Reviews (open"), "{shown}");
    assert!(shown.contains("T2 Lead not listening (resolved"), "{shown}");
    assert!(ok(&db, &["task", "show", "1"]).contains("#1 Reviews"));
    assert!(!lock.exists(), "its lock file was left");
    // No lead now: removing again and arming a waiter are refused.
    assert_refused(&tower(&db, &["lead", "remove", "R1"]), "R1 has no lead yet");
    assert_refused(&tower(&db, &["lead", "wait", "R1"]), "R1 has no lead yet");
    // The next lead takes the repo, with its work.
    ok(
        &db,
        &[
            "lead",
            "add",
            "R1",
            "--name",
            "Next",
            "--session",
            "next-lead",
        ],
    );
    // A lead taken off while its waiter runs (by a board writer, not this command, which
    // refuses then): the waiter ends, saying not to arm it again, and its lock file goes.
    let w = wait();
    held_by(&w);
    Store::open(&db).unwrap().remove_lead(r1, "gone").unwrap();
    let mut w = w;
    assert!(ended(&mut w.0, "the waiter").success());
    let mut out = String::new();
    std::io::Read::read_to_string(&mut w.0.stdout.take().unwrap(), &mut out).unwrap();
    assert_eq!(
        out,
        "control-tower: R1 has no lead any more (tower lead remove); this waiter ends, so do \
         not arm it again.\n"
    );
    assert!(!lock.exists());
}

#[test]
fn a_waiter_that_cannot_read_the_board_fails_and_never_says_its_lead_is_gone() {
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    ok(&db, &["milestone", "add", "MVP", "--goal", "g"]);
    in_repo(&db, "M1");
    ok(
        &db,
        &[
            "lead",
            "add",
            "M1",
            "--name",
            "Catalogue",
            "--session",
            "cat-lead",
        ],
    );
    let mut w = Reaped(
        Command::new(env!("CARGO_BIN_EXE_tower"))
            .args(["lead", "wait", "R1"])
            .env("TOWER_DB", &db)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let lock = db.with_file_name("board.db.R1.stream");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read_to_string(&lock).unwrap_or_default().trim() != w.0.id().to_string() {
        assert!(
            std::time::Instant::now() < until,
            "the waiter never took the lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The board can no longer be read as the waiter reads it (its leads table gone): a
    // read error, not a lead taken off. It fails, so the lead reads why, and keeps its
    // lock file; it never says the lead is gone.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("ALTER TABLE lead RENAME TO lead_away")
        .unwrap();
    let status = ended(&mut w.0, "the waiter");
    assert!(
        !status.success(),
        "a read error ended the waiter as if its lead were gone"
    );
    let mut out = String::new();
    std::io::Read::read_to_string(&mut w.0.stdout.take().unwrap(), &mut out).unwrap();
    assert_eq!(out, "");
    let mut err = String::new();
    std::io::Read::read_to_string(&mut w.0.stderr.take().unwrap(), &mut err).unwrap();
    assert!(err.contains("lead"), "{err}");
    assert!(lock.exists());
}
