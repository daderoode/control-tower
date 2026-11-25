use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use control_tower::board::{
    BoardOf, Colour, Lead, Milestone, MilestoneId, RepoId, RepoOf, Stage, Task, TaskId, Thread,
    ThreadId, Ticket, ago, ordered_threads, refuse_owner,
};
use control_tower::deliver::check_asker;
use control_tower::register::{Registered, brew_dir, register};
use control_tower::store::{
    MilestoneFields, Moved, Removed, RepoDetails, SCHEMA_VERSION, Store, TaskFields, TaskRemoved,
    board_path,
};

/// The control-tower board: repos, their leads, milestones and tasks, and the dashboard.
///
/// The board is one SQLite file per machine, `~/.local/state/control-tower/board.db`, or the
/// file `TOWER_DB` names.
#[derive(Parser)]
#[command(name = "tower", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Repos on the board: each a git repository, named by its main checkout. Every
    /// milestone and standalone task belongs to one, and its lead and message board are
    /// the repo's.
    #[command(subcommand)]
    Repo(RepoCommand),
    /// Milestones on the board.
    #[command(subcommand)]
    Milestone(MilestoneCommand),
    /// Leads on the board: one lead per repo.
    #[command(subcommand)]
    Lead(LeadCommand),
    /// Tasks on the board.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Subtasks of a task, one level only. Move one with tower task stage.
    #[command(subcommand)]
    Subtask(SubtaskCommand),
    /// Threads on a repo's message board.
    #[command(subcommand)]
    Thread(ThreadCommand),
    /// Post a message to an open thread.
    Post {
        /// The thread, like T3.
        thread: ThreadId,
        /// Who posts it, like dev-2, senior, qa or the lead's session name (quoted when it has
        /// spaces); never
        /// manager, which only the owner posts as, from the dashboard.
        #[arg(long)]
        by: String,
        /// Up to 4000 characters; may span lines. A lone "-" reads the message from stdin
        /// instead, so text starting with "-" is never taken for an option.
        text: String,
        /// Post it as a progress post: the doing line of the task the thread is linked to,
        /// where its thread stands on the board, until a later one. Plain posts never are.
        #[arg(long)]
        progress: bool,
    },
    /// Open the dashboard (q or Ctrl-C closes it; in the space `tower space` opens, q returns
    /// to the workspace it came from). A brew-installed tower first links its
    /// herdr plugin if herdr has no control-tower yet.
    Ui,
    /// The herdr action `space`: focus the dashboard in its own herdr workspace, labelled
    /// control-tower, creating the workspace with the dashboard when there is none. q there
    /// returns to the workspace this was invoked from (HERDR_WORKSPACE_ID) and Ctrl-C closes
    /// the dashboard. TOWER_HERDR names the herdr executable (default herdr).
    Space,
    /// The herdr action `popup`: open the dashboard in herdr's popup, 85% of the terminal
    /// each way; Esc at its top level or q closes it.
    Popup,
    /// Watch for stuck workers until SIGTERM or Ctrl-C: every 20 s, read the board and the
    /// Claude sessions running in Rex (a task's assignee is its worker's Claude session
    /// name), nudge a worker that stopped or went silent, and post system flags in the
    /// task's thread. Logs one line per action to stderr. TOWER_REX names the rex
    /// executable (default rex in $REX_BIN_DIR, else on the PATH); TOWER_CLAUDE names
    /// claude, which also relays posts and nudges to workers (claude -p -n control-tower).
    /// A brew-installed tower also links its herdr plugin (TOWER_HERDR names herdr, default
    /// herdr) if herdr has no control-tower yet, trying before each look until herdr
    /// answers; it never relinks over another.
    Watch,
    /// Bring a board written by an older tower up to this tower's schema.
    Upgrade,
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Add a repo by its main checkout and print its id.
    Add {
        /// The main checkout: an absolute path to a directory with a .git directory (not a
        /// linked worktree).
        path: PathBuf,
        /// Its forge link: an http or https URL.
        #[arg(long)]
        link: Option<String>,
    },
    /// Set a repo's main checkout or forge link, each replacing the one stored: for example
    /// on a repo the schema 14 upgrade made, which has no path yet.
    Set {
        /// The repo, like R2.
        repo: RepoId,
        /// Its main checkout, by the rules of add.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Its forge link: an http or https URL.
        #[arg(long)]
        link: Option<String>,
    },
    /// List every repo, one per line: its id, main checkout, lead, milestones and standalone
    /// tasks; then, on one line, the milestones and standalone tasks in no repo yet. For
    /// example, to find the repos the schema 14 upgrade made, one per old lead, which have
    /// no path yet, and what it left in no repo.
    List,
    /// Print a repo: its path, forge link, lead, milestones, standalone tasks and threads.
    Show {
        /// The repo, like R2.
        repo: RepoId,
    },
    /// Remove a repo that holds no milestones or standalone tasks (move them first with
    /// tower milestone set --repo or tower task set --repo), with its lead and the threads
    /// left on its board, their messages and inbox items, and say what went. For example,
    /// an empty repo the schema 14 upgrade made for an old lead.
    Remove {
        /// The repo, like R2.
        repo: RepoId,
    },
}

#[derive(Subcommand)]
enum MilestoneCommand {
    /// Add a milestone and print its id.
    Add {
        /// One line, up to 200 characters.
        title: String,
        /// The milestone's one-line goal.
        #[arg(long)]
        goal: String,
        /// The repo it belongs to, like R2; without it the milestone has no repo, and so no
        /// lead, until tower milestone set --repo.
        #[arg(long)]
        repo: Option<RepoId>,
    },
    /// Mark a milestone Done, as its lead does once every one of its tasks is Done. It shows
    /// In progress on its own once any task has started.
    Done {
        /// The milestone, like M2.
        milestone: MilestoneId,
    },
    /// Set a milestone's title, goal and details: any of its title, goal, standing context,
    /// spec, tracker link and repo, each replacing the one stored. Moving it to another repo
    /// takes the threads about it and its tasks along.
    Set {
        /// The milestone, like M2.
        milestone: MilestoneId,
        /// Its title. One line, up to 200 characters.
        #[arg(long)]
        title: Option<String>,
        /// Its one-line goal, up to 200 characters.
        #[arg(long)]
        goal: Option<String>,
        /// What to do when picking it up, for this milestone only. Plain text, line breaks
        /// allowed, up to 2000 characters.
        #[arg(long)]
        context: Option<String>,
        /// The spec it came from, like "docs/specs/first-shop-pages.md". One line, up to 200 characters.
        #[arg(long)]
        spec: Option<String>,
        /// Its ticket in a tracker: an http or https URL.
        #[arg(long)]
        link: Option<String>,
        /// The repo it belongs to, like R2.
        #[arg(long)]
        repo: Option<RepoId>,
    },
    /// Print a milestone with its details, its tasks and its threads (open first, then
    /// resolved, each newest activity first).
    Show {
        /// The milestone, like M2.
        milestone: MilestoneId,
    },
}

#[derive(Subcommand)]
enum LeadCommand {
    /// Register the lead of a repo. A repo has one lead, and a lead one repo.
    Add {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
        /// The name shown in the tree, like "Catalogue Lead". One line, up to 200
        /// characters.
        #[arg(long)]
        name: String,
        /// The Claude session name workers message the lead by, like "Catalogue Lead".
        #[arg(long)]
        session: String,
        /// One of blue, purple, orange, teal, pink, green; the next free one if not given.
        #[arg(long)]
        colour: Option<Colour>,
    },
    /// Change the lead of a repo: any of its name, session and colour, by the same rules as
    /// add (for example after its Claude session was restarted under a new name). The
    /// repo's threads stay: the board is the repo's.
    Set {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
        /// A new name shown in the tree.
        #[arg(long)]
        name: Option<String>,
        /// A new Claude session name.
        #[arg(long)]
        session: Option<String>,
        /// A new colour: blue, purple, orange, teal, pink or green.
        #[arg(long)]
        colour: Option<Colour>,
    },
    /// Take a repo's lead off the board, for a lead nobody runs any more: the repo, its
    /// milestones, tasks and threads stay for the next lead (tower lead add). Refused while
    /// a board waiter for the repo runs, as a session still leads it then.
    Remove {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
    },
    /// Record the lead's Claude session, so its Stop hook knows it is the lead and the
    /// watch can tell whether it is listening. The lead runs it from its own Claude
    /// session's Bash, at start and after a restart, once its board waiter runs: it reads
    /// CLAUDE_CODE_SESSION_ID, and refuses unless `claude agents --json` lists that session
    /// under the session name the lead registered.
    Attach {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
    },
    /// The lead's board waiter: wait until a post on the lead's board is unread, then print
    /// one line saying how many and end, telling nothing. Ends at once when posts are
    /// unread already. The lead runs it with Claude Code's Bash run_in_background, so its
    /// end wakes the lead only when there is something to read: then tower lead unread, and
    /// arm it again in the same turn. One listener per repo, the newest: a new one stops the
    /// one before it, waiter or stream.
    Wait {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
    },
    /// Print one line per post on the lead's board it has not been told of (thread, task,
    /// author and first words), and count them as told; nothing when there are none. Then,
    /// when a board waiter for the repo already runs, one line saying so: do not arm
    /// another.
    Unread {
        /// The repo it leads, like R2, or a milestone of it, like M2.
        repo: RepoOf,
    },
    /// The lead's Claude Code hooks (run by the control-tower plugin, not by hand): reads
    /// the hook's JSON on stdin. Stop: for the session a lead attached, blocks the end of
    /// its turn while posts on its board are unread, naming them, and counts a turn it lets
    /// end with no board waiter running (not one it holds), for the watch's "Lead not
    /// listening" check. StopFailure: a turn of that session that ended on an API error is
    /// counted the same way, never told and nothing printed; a turn the user interrupts
    /// runs no hook and is not counted. SessionStart: a session
    /// named as a lead's registered session is attached again (as tower lead attach would)
    /// and told how to resume leading. Any other session passes.
    Hook,
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Add a task under a milestone, or a standalone task without --milestone, and print
    /// its id.
    Add {
        /// One line, up to 200 characters.
        title: String,
        /// The milestone it belongs to, like M2; without it the task is standalone, until it
        /// is handed to a lead with tower task hand.
        #[arg(long, conflicts_with = "repo")]
        milestone: Option<MilestoneId>,
        /// For a standalone task: the repo it belongs to, like R2.
        #[arg(long)]
        repo: Option<RepoId>,
    },
    /// Hand a standalone task, with its subtasks, to a lead: it moves under the lead's
    /// milestone, which takes back that milestone's Done mark.
    Hand {
        /// The standalone task, like 12 (or "#12", quoted).
        task: TaskId,
        /// The milestone of the lead it goes to, like M2; it must have a lead.
        milestone: MilestoneId,
    },
    /// Move a task or subtask to a stage: waiting, in-progress or done.
    Stage {
        /// The task or subtask, like 12 (or "#12", quoted).
        task: TaskId,
        /// waiting, in-progress or done.
        stage: Stage,
        /// With done: who moved it, like the lead's session name. A done card with no
        /// assignee says "done by lead" when it was the milestone's lead.
        #[arg(long)]
        by: Option<String>,
    },
    /// Set a task's details: any of its description, standing context, tracker link, MR line,
    /// assignee and, for a standalone task, repo, each replacing the one stored. Not for
    /// subtasks.
    Set {
        /// The task, like 12 (or "#12", quoted).
        task: TaskId,
        /// What and why. Plain text, line breaks allowed, up to 2000 characters.
        #[arg(long)]
        description: Option<String>,
        /// What to do when picking it up, for this task only. As --description.
        #[arg(long)]
        context: Option<String>,
        /// Its ticket in a tracker: an http or https URL.
        #[arg(long)]
        link: Option<String>,
        /// Its merge or pull request and pipeline, like "!44 draft into feat/bookmarks ·
        /// pipeline running". One line, up to 200 characters.
        #[arg(long)]
        mr: Option<String>,
        /// Who is on it, by name. One line, up to 200 characters. To clear it, use tower
        /// task unassign N.
        #[arg(long)]
        assignee: Option<String>,
        /// For a standalone task: the repo it belongs to, like R2, for example one the
        /// schema 14 upgrade left in no repo (tower repo list names them). A milestone's
        /// task is in its milestone's repo. A task with threads moves only to a repo with a
        /// lead.
        #[arg(long)]
        repo: Option<RepoId>,
    },
    /// Take a task's assignee away: its card says "unassigned" again.
    Unassign {
        /// The task, like 12 (or "#12", quoted).
        task: TaskId,
    },
    /// Remove an abandoned standalone task, with its subtasks, criteria and threads (with
    /// their messages), and say what went, so its repo can be removed. Refused for a
    /// milestone's task or a subtask, while it has an assignee, and while another task is
    /// blocked by it.
    Remove {
        /// The standalone task, like 12 (or "#12", quoted).
        task: TaskId,
    },
    /// Flag a task as blocked, with why: the tree shows "!" on it and its lead "N blocked".
    /// Blocking a blocked task replaces the reason; moving it to done clears the flag. Not
    /// for subtasks or done tasks; not blocked-by.
    Block {
        /// The task, like 12 (or "#12", quoted).
        task: TaskId,
        /// Why it is stuck, shown as "blocked: <reason>" where its thread stands on the board
        /// (its card shows a "!"). One line, up to 200 characters.
        reason: String,
    },
    /// Clear a task's blocked flag.
    Unblock {
        /// The task, like 12 (or "#12", quoted).
        task: TaskId,
    },
    /// Print a task with its details (a standalone task's repo among them), criteria, what it
    /// is blocked by, subtasks and the threads linked to it (open first, then resolved, each
    /// newest activity first).
    Show {
        /// The task, like 12 (or "#12", quoted).
        task: TaskId,
    },
    /// A task's acceptance criteria, numbered from 1 in the order added.
    #[command(subcommand)]
    Criterion(CriterionCommand),
    /// The other tasks a task waits for.
    #[command(subcommand, name = "blocked-by")]
    BlockedBy(BlockedByCommand),
}

#[derive(Subcommand)]
enum CriterionCommand {
    /// Add an acceptance criterion, not met yet, and print its number.
    Add {
        /// The task, like 12.
        task: TaskId,
        /// One line, up to 200 characters; a task has each criterion once.
        text: String,
    },
    /// Mark a criterion met.
    Check {
        /// The task, like 12.
        task: TaskId,
        /// The criterion's number, from 1, as tower task show lists them.
        number: usize,
    },
    /// Mark a criterion not met again.
    Uncheck {
        /// The task, like 12.
        task: TaskId,
        /// The criterion's number, from 1.
        number: usize,
    },
    /// Remove a criterion; the ones after it move up a number.
    Remove {
        /// The task, like 12.
        task: TaskId,
        /// The criterion's number, from 1.
        number: usize,
    },
}

#[derive(Subcommand)]
enum BlockedByCommand {
    /// Record that a task waits for another task.
    Add {
        /// The task that waits, like 12.
        task: TaskId,
        /// The task it waits for, like 9.
        other: TaskId,
    },
    /// Record that a task no longer waits for another.
    Remove {
        /// The task that waited, like 12.
        task: TaskId,
        /// The task it waited for, like 9.
        other: TaskId,
    },
}

#[derive(Subcommand)]
enum SubtaskCommand {
    /// Add a subtask under a task and print its id. A subtask cannot have subtasks: ask the
    /// lead for a new task instead. Subtasks never count towards the milestone's percentage.
    Add {
        /// One line, up to 200 characters.
        title: String,
        /// The task it belongs to, like 12 (or "#12", quoted).
        #[arg(long)]
        task: TaskId,
    },
}

#[derive(Subcommand)]
enum ThreadCommand {
    /// Open a thread on a repo's message board with its opening message, and print its id.
    New {
        /// Whose board: a milestone, like M2, for a thread about it on its repo's board, or
        /// a lead's session name, for a thread about the whole repo.
        board: BoardOf,
        /// Who starts it, like dev-2, senior, qa or the lead's session name; never
        /// manager.
        #[arg(long)]
        by: String,
        /// The task it belongs to, a task of that repo, like 12.
        #[arg(long)]
        task: Option<TaskId>,
        /// The thread's title, one line, up to 200 characters.
        title: String,
        /// The opening message: up to 4000 characters; may span lines.
        message: String,
    },
    /// Resolve an open thread with evidence, posted as its last message. A resolved thread
    /// takes no more messages.
    Resolve {
        /// The thread, like T3.
        thread: ThreadId,
        /// Who resolves it; never manager.
        #[arg(long)]
        by: String,
        /// What shows it is resolved, like "12 books on all three sizes": up to 4000
        /// characters; may span lines.
        evidence: String,
    },
    /// Print a thread: its title, status, task, board and who started it, then every
    /// message, oldest first, each in full.
    Show {
        /// The thread, like T3.
        thread: ThreadId,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tower: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// "Catalogue Lead (session cat-lead, colour blue)".
fn describe(lead: &Lead) -> String {
    let colour = lead.colour.map_or("unknown", Colour::name);
    format!("{} (session {}, colour {colour})", lead.name, lead.session)
}

/// "in progress, 1/5 tasks done (20%)".
fn progress(m: &Milestone) -> String {
    format!(
        "{}, {}/{} tasks done ({}%)",
        m.stage().label(),
        m.done_tasks(),
        m.tasks.len(),
        m.percent()
    )
}

/// "#12 is in progress, 3/5 subtasks done".
fn subtask_progress(t: &Task) -> String {
    format!(
        "{} is {}, {}/{} subtasks done",
        t.id,
        t.stage.label(),
        t.done_subtasks(),
        t.subtasks.len()
    )
}

/// "  label  value", the value's later lines indented under its first.
fn field(out: &mut String, label: &str, value: &str) {
    for (i, line) in value.lines().enumerate() {
        let label = if i == 0 { label } else { "" };
        out.push_str(&format!("  {label:<11}{line}\n"));
    }
}

fn or_none(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("none")
}

/// What `milestone set` and `task set` print: "set the details of M1", and, when it was
/// given a repo, where it is now, its lead there and the threads that went with it.
fn set_line(what: &str, moved: Option<&Moved>) -> String {
    let Some(moved) = moved else {
        return format!("set the details of {what}");
    };
    let lead =
        (moved.lead.as_ref()).map_or("no lead yet".to_string(), |l| format!("led by {}", l.name));
    format!(
        "set the details of {what}; it is in {}, {lead}, and its {} thread(s) are on {}'s board",
        moved.repo, moved.threads, moved.repo
    )
}

/// The repo a board waiter (`tower lead wait`) for `of` listens on.
/// Refuses a repo with no lead, and one not on the board, whose lock file, left by its last
/// listener, goes first.
fn listener_repo(path: &Path, of: RepoOf) -> Result<RepoId> {
    let mut store = Store::open(path)?;
    let found = store.lead(of);
    if let (Err(_), RepoOf::Repo(r)) = (&found, of)
        && !store.has_repo(r)?
    {
        control_tower::listen::forget_stream(path, r)?;
    }
    Ok(found?.1)
}

/// What `tower repo remove` says it removed: "removed R5: its lead Old Lead (session
/// old-lead), 1 thread, 2 messages, 0 inbox items".
fn removed_line(repo: RepoId, r: &Removed) -> String {
    let lead = (r.lead.as_ref()).map_or("no lead".to_string(), |l| {
        format!("its lead {} (session {})", l.name, l.session)
    });
    let n = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!(
        "removed {repo}: {lead}, {}, {}, {}",
        n(r.threads, "thread", "threads"),
        n(r.messages, "message", "messages"),
        n(r.inbox, "inbox item", "inbox items")
    )
}

/// What `tower task remove` removed: "removed #4 Spike: 2 subtasks, 1 criterion, 1 thread,
/// 3 messages, 0 inbox items".
fn task_removed_line(task: TaskId, r: &TaskRemoved) -> String {
    let n = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!(
        "removed {task} {}: {}, {}, {}, {}, {}",
        r.title,
        n(r.subtasks, "subtask", "subtasks"),
        n(r.criteria, "criterion", "criteria"),
        n(r.threads, "thread", "threads"),
        n(r.messages, "message", "messages"),
        n(r.inbox, "inbox item", "inbox items")
    )
}

/// One line of `tower repo list`: "R1 /path, lead Catalogue (session cat-lead), M1 M2 #4", its
/// milestones then its standalone tasks.
fn repo_line(r: &RepoDetails) -> String {
    let path = r.path.as_deref().unwrap_or("(path not set)");
    let lead = (r.lead.as_ref()).map_or("no lead".to_string(), |l| {
        format!("lead {} (session {})", l.name, l.session)
    });
    let items: Vec<String> = (r.milestones.iter().map(|(m, _)| m.to_string()))
        .chain(r.standalone.iter().map(|(t, _, _)| t.to_string()))
        .collect();
    let items = if items.is_empty() {
        "no milestones or standalone tasks".to_string()
    } else {
        items.join(" ")
    };
    format!("{} {path}, {lead}, {items}", r.id)
}

/// A task's ticket in a listing, from its tracker link, so a lead matches the forge's
/// numbers to tasks without the title carrying them: "ticket #64", "ticket !19", the link
/// itself for another tracker, or "no ticket".
fn ticket(link: Option<&str>) -> String {
    match link.map(|l| (l, Ticket::of(l))) {
        Some((link, Ticket::Link)) => format!("ticket {link}"),
        Some((_, k)) => format!("ticket {k}"),
        None => "no ticket".into(),
    }
}

/// `tower repo show`: the repo, its lead, milestones, standalone tasks and threads, the
/// threads in the dashboard's order.
fn show_repo(r: &RepoDetails) -> String {
    let mut out = format!("{}\n", r.id);
    let path = r
        .path
        .as_deref()
        .unwrap_or("not set (tower repo set --path)");
    field(&mut out, "path", path);
    field(&mut out, "forge", or_none(&r.link));
    let lead = r.lead.as_ref().map(describe);
    field(&mut out, "lead", lead.as_deref().unwrap_or("none yet"));
    out.push_str("milestones\n");
    if r.milestones.is_empty() {
        out.push_str("  none\n");
    }
    for (id, title) in &r.milestones {
        out.push_str(&format!("  {id} {title}\n"));
    }
    out.push_str("standalone tasks\n");
    if r.standalone.is_empty() {
        out.push_str("  none\n");
    }
    for (id, title, link) in &r.standalone {
        out.push_str(&format!("  {id} {title} ({})\n", ticket(link.as_deref())));
    }
    threads(&mut out, ordered_threads(&r.threads, None), true);
    out
}

/// `tower milestone show`: the milestone, its details, its tasks and its threads, one per
/// line, the threads in the dashboard's order.
fn show_milestone(m: &Milestone, session: Option<&str>) -> String {
    let mut out = format!("{} {}\n", m.id, m.title);
    field(&mut out, "goal", &m.goal);
    field(&mut out, "stage", &progress(m));
    let repo = m.repo.map_or_else(
        || format!("none yet (tower milestone set {} --repo R<n>)", m.id),
        |r| r.to_string(),
    );
    field(&mut out, "repo", &repo);
    let lead = m.lead.as_ref().map(describe);
    field(&mut out, "lead", lead.as_deref().unwrap_or("none yet"));
    if m.lead.is_some() {
        let attached = session.map_or("not attached (tower lead attach)".into(), |s| {
            format!("attached, Claude session {s}")
        });
        field(&mut out, "session", &attached);
    }
    field(&mut out, "context", or_none(&m.details.context));
    field(&mut out, "spec", or_none(&m.details.spec));
    field(&mut out, "tracker", or_none(&m.details.link));
    out.push_str("tasks\n");
    if m.tasks.is_empty() {
        out.push_str("  none yet\n");
    }
    for t in &m.tasks {
        let who = t.details.assignee.as_deref().unwrap_or("unassigned");
        let blocked = (t.details.blocked.as_deref())
            .map(|r| format!(", blocked: {r}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {} {} ({}, {who}{blocked}, {})\n",
            t.id,
            t.title,
            t.stage.label(),
            ticket(t.details.link.as_deref())
        ));
    }
    threads(&mut out, m.ordered_threads(None), true);
    out
}

/// `tower task show`: the task, its details, criteria, what it is blocked by, subtasks and
/// the threads linked to it, in the dashboard's order, from `board`, a milestone showing
/// its repo's board.
fn show_task(t: &Task, milestone: Option<&Milestone>, board: Option<&Milestone>) -> String {
    let d = &t.details;
    let mut out = format!("{} {}\n", t.id, t.title);
    field(&mut out, "stage", t.stage.label());
    field(&mut out, "blocked", d.blocked.as_deref().unwrap_or("no"));
    let under = milestone.map(|m| format!("{} {}", m.id, m.title));
    field(
        &mut out,
        "milestone",
        under.as_deref().unwrap_or("none (standalone)"),
    );
    if milestone.is_none() {
        let repo = d.repo.map_or_else(
            || format!("none yet (tower task set {} --repo R<n>)", t.id.0),
            |r| r.to_string(),
        );
        field(&mut out, "repo", &repo);
    }
    field(
        &mut out,
        "assignee",
        d.assignee.as_deref().unwrap_or("unassigned"),
    );
    field(
        &mut out,
        "doing",
        d.doing
            .as_ref()
            .map_or("nothing posted yet", |post| post.text.as_str()),
    );
    let by: Vec<String> = d.blocked_by.iter().map(TaskId::to_string).collect();
    let by = if by.is_empty() {
        "nothing".to_string()
    } else {
        by.join(", ")
    };
    field(&mut out, "blocked by", &by);
    field(&mut out, "MR", or_none(&d.mr));
    field(&mut out, "context", or_none(&d.context));
    field(&mut out, "tracker", or_none(&d.link));
    out.push_str("description\n");
    for line in or_none(&d.description).lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str("acceptance criteria\n");
    if d.criteria.is_empty() {
        out.push_str("  none\n");
    }
    for (i, c) in d.criteria.iter().enumerate() {
        let mark = if c.done { "[x]" } else { "[ ]" };
        out.push_str(&format!("  {}. {mark} {}\n", i + 1, c.text));
    }
    out.push_str("subtasks\n");
    if t.subtasks.is_empty() {
        out.push_str("  none\n");
    }
    for s in &t.subtasks {
        out.push_str(&format!("  {} {} ({})\n", s.id, s.title, s.stage.label()));
    }
    let linked = board.map_or(Vec::new(), |m| m.ordered_threads(Some(t.id)));
    threads(&mut out, linked, false);
    out
}

/// A show's "threads" section, one `thread_line` each, or "none".
fn threads(out: &mut String, threads: Vec<&Thread>, about: bool) {
    out.push_str("threads\n");
    if threads.is_empty() {
        out.push_str("  none\n");
    }
    for th in threads {
        out.push_str(&format!("  {}\n", thread_line(th, about)));
    }
}

/// "T3 Task #2 team (open, by cat-lead, 1 reply)", with `about` naming its task, if any:
/// "(open, about #2, by cat-lead, 1 reply)".
fn thread_line(th: &Thread, about: bool) -> String {
    let task = (th.task.filter(|_| about))
        .map(|t| format!(", about {t}"))
        .unwrap_or_default();
    format!(
        "{} {} ({}{task}, by {}, {})",
        th.id,
        th.title,
        th.status().name(),
        th.by,
        replies(th)
    )
}

/// `tower thread show`: the thread whole, `t` with every message, its linked task's title
/// if known, on `board` (its repo, and the milestone it is about if any); times are ages
/// at `now`.
fn show_thread(t: &Thread, board: &str, task_title: Option<&str>, now: i64) -> String {
    let mut out = format!("{} {}\n", t.id, t.title);
    let status = match &t.resolved {
        None => "open".to_string(),
        Some(r) => format!("resolved by {}, {}", r.by, ago(r.at, now)),
    };
    field(&mut out, "status", &status);
    let task = t.task.map(|id| match task_title {
        Some(title) => format!("{id} {title}"),
        None => id.to_string(),
    });
    field(&mut out, "task", task.as_deref().unwrap_or("none"));
    field(&mut out, "board", board);
    field(&mut out, "started by", &t.by);
    out.push_str("messages\n");
    for msg in &t.messages {
        out.push_str(&format!("  {}  {}\n", msg.author, ago(msg.at, now)));
        for line in msg.text.lines() {
            let indent = if line.is_empty() { "" } else { "    " };
            out.push_str(&format!("{indent}{line}\n"));
        }
    }
    out
}

/// "1 reply", "3 replies".
fn replies(t: &Thread) -> String {
    match t.replies() {
        1 => "1 reply".into(),
        n => format!("{n} replies"),
    }
}

/// `tower lead hook`, the plugin's Stop, StopFailure and SessionStart hooks, by the input's
/// `hook_event_name`. It runs before the board's path is read, so a SessionStart can keep
/// every failure quiet: a hook can never stop a session from starting, so what went
/// wrong, from an unset HOME to `claude` failing, goes to stderr (Claude Code's debug log),
/// nothing is printed and it exits 0. Of input it cannot read, only stdin failing to read
/// at all is quiet whatever the event (the event cannot be known then); bytes that are not
/// UTF-8 are read lossily, and input that is not JSON, or JSON naming no `hook_event_name`,
/// takes the Stop path, which refuses it with an error as it always has.
fn lead_hook() -> Result<()> {
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("tower lead hook: cannot read the hook's input: {e}");
        return Ok(());
    }
    let input = String::from_utf8_lossy(&input).into_owned();
    let event = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v["hook_event_name"].as_str().map(String::from));
    if event.as_deref() == Some("SessionStart") {
        let started = (|| {
            let path = board_path()?;
            let claude = control_tower::register::claude_path();
            let stop = std::sync::atomic::AtomicBool::new(false);
            // Each ask bounded, so all `session_start`'s asks fit the hook's own limit.
            let agents = || {
                control_tower::register::claude_within(
                    &claude,
                    &path,
                    &stop,
                    &["agents", "--json"],
                    control_tower::listen::LISTED_ASK,
                )
            };
            control_tower::listen::session_start(&path, &input, agents)
        })();
        match started {
            Ok(Some(reply)) => println!("{reply}"),
            Ok(None) => {}
            Err(e) => eprintln!("tower lead hook (SessionStart): {e:#}"),
        }
        return Ok(());
    }
    // A turn ended on an API error: counted, never told (Claude Code ignores this output).
    if event.as_deref() == Some("StopFailure") {
        return control_tower::listen::turn_failed(&board_path()?, &input);
    }
    if let Some(reply) = control_tower::listen::hook(&board_path()?, &input)? {
        println!("{reply}");
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Lead(LeadCommand::Hook)) {
        return lead_hook();
    }
    let path = board_path()?;
    match cli.command {
        Command::Repo(RepoCommand::Add {
            path: checkout,
            link,
        }) => {
            let id = Store::open(&path)?.add_repo(&checkout, link.as_deref())?;
            println!("{id}");
            eprintln!("added repo {id}");
        }
        Command::Repo(RepoCommand::Set {
            repo,
            path: checkout,
            link,
        }) => {
            Store::open(&path)?.set_repo(repo, checkout.as_deref(), link.as_deref())?;
            println!("set the details of {repo}");
        }
        Command::Repo(RepoCommand::List) => {
            let mut store = Store::open(&path)?;
            let repos = store.repos()?;
            if repos.is_empty() {
                println!("no repos yet: add one with tower repo add <main checkout>");
            }
            for r in &repos {
                println!("{}", repo_line(r));
            }
            let (milestones, tasks) = store.without_repo()?;
            let ids: Vec<String> = (milestones.iter().map(ToString::to_string))
                .chain(tasks.iter().map(ToString::to_string))
                .collect();
            if !ids.is_empty() {
                println!(
                    "no repo yet: {}; give each one with tower milestone set M<n> --repo R<n> \
                     or tower task set <n> --repo R<n>",
                    ids.join(", ")
                );
            }
        }
        Command::Repo(RepoCommand::Show { repo }) => {
            print!("{}", show_repo(&Store::open(&path)?.repo(repo)?));
        }
        Command::Repo(RepoCommand::Remove { repo }) => {
            let mut store = Store::open(&path)?;
            let removed = store.remove_repo(repo);
            if let Ok(r) = &removed {
                println!("{}", removed_line(repo, r));
            }
            // Gone (now or before): its stream lock goes, unless a stream holds it, which
            // deletes it itself when it sees the repo gone.
            if !store.has_repo(repo)? {
                control_tower::listen::forget_stream(&path, repo).with_context(|| {
                    format!("{repo} is not on the board, but its stream lock is")
                })?;
            }
            removed?;
        }
        Command::Milestone(MilestoneCommand::Add { title, goal, repo }) => {
            let id = Store::open(&path)?.add_milestone(&title, &goal, repo)?;
            println!("{id}");
            let into = repo.map(|r| format!(" in {r}")).unwrap_or_default();
            eprintln!("added milestone {id}{into}");
        }
        Command::Milestone(MilestoneCommand::Done { milestone }) => {
            Store::open(&path)?.mark_milestone_done(milestone)?;
            println!("{milestone} is done");
        }
        Command::Milestone(MilestoneCommand::Set {
            milestone,
            title,
            goal,
            context,
            spec,
            link,
            repo,
        }) => {
            let fields = MilestoneFields {
                title: title.as_deref(),
                goal: goal.as_deref(),
                context: context.as_deref(),
                spec: spec.as_deref(),
                link: link.as_deref(),
                repo,
            };
            let moved = Store::open(&path)?.set_milestone(milestone, fields)?;
            println!("{}", set_line(&milestone.to_string(), moved.as_ref()));
        }
        Command::Milestone(MilestoneCommand::Show { milestone }) => {
            let mut store = Store::open(&path)?;
            let board = store.load()?;
            let m = (board.milestones.iter())
                .find(|m| m.id == milestone)
                .with_context(|| format!("there is no milestone {milestone} on the board"))?;
            let leads = store.attached_leads()?;
            let attached = (leads.iter()).find(|l| Some(l.repo) == m.repo);
            print!(
                "{}",
                show_milestone(m, attached.and_then(|l| l.claude_session.as_deref()))
            );
        }
        Command::Task(TaskCommand::Set {
            task,
            description,
            context,
            link,
            mr,
            assignee,
            repo,
        }) => {
            let fields = TaskFields {
                description: description.as_deref(),
                context: context.as_deref(),
                link: link.as_deref(),
                mr: mr.as_deref(),
                assignee: assignee.as_deref(),
                repo,
            };
            let moved = Store::open(&path)?.set_task(task, fields)?;
            println!("{}", set_line(&format!("task {task}"), moved.as_ref()));
        }
        Command::Task(TaskCommand::Show { task }) => {
            let mut store = Store::open(&path)?;
            let t = store.task(task)?;
            let board = store.load()?;
            let m = (board.milestones.iter()).find(|m| m.tasks.iter().any(|x| x.id == task));
            // A standalone task's threads are on its repo's board, which each milestone of
            // the repo shows.
            let board_of = m.or_else(|| {
                (board.milestones.iter()).find(|m| m.threads.iter().any(|x| x.task == Some(task)))
            });
            print!("{}", show_task(&t, m, board_of));
        }
        Command::Task(TaskCommand::Criterion(CriterionCommand::Add { task, text })) => {
            let n = Store::open(&path)?.add_criterion(task, &text)?;
            println!("{n}");
            eprintln!("added criterion {n} to task {task}");
        }
        Command::Task(TaskCommand::Criterion(CriterionCommand::Check { task, number })) => {
            let text = Store::open(&path)?.check_criterion(task, number, true)?;
            println!("criterion {number} of task {task} is met: {text}");
        }
        Command::Task(TaskCommand::Criterion(CriterionCommand::Uncheck { task, number })) => {
            let text = Store::open(&path)?.check_criterion(task, number, false)?;
            println!("criterion {number} of task {task} is not met: {text}");
        }
        Command::Task(TaskCommand::Criterion(CriterionCommand::Remove { task, number })) => {
            let text = Store::open(&path)?.remove_criterion(task, number)?;
            println!(
                "removed criterion {number} of task {task}: {text}; \
                 the criteria after it move up a number"
            );
        }
        Command::Task(TaskCommand::Unassign { task }) => {
            Store::open(&path)?.unassign(task)?;
            println!("task {task} is unassigned");
        }
        Command::Task(TaskCommand::Remove { task }) => {
            let removed = Store::open(&path)?.remove_task(task)?;
            println!("{}", task_removed_line(task, &removed));
        }
        Command::Task(TaskCommand::Block { task, reason }) => {
            let was = Store::open(&path)?.block_task(task, &reason)?;
            let was = was.map(|r| format!(" (was: {r})")).unwrap_or_default();
            println!(
                "task {task} is blocked: {}{was}; moving it to done clears it",
                reason.trim()
            );
        }
        Command::Task(TaskCommand::Unblock { task }) => {
            let was = Store::open(&path)?.unblock_task(task)?;
            println!("task {task} is no longer blocked (was: {was})");
        }
        Command::Task(TaskCommand::BlockedBy(BlockedByCommand::Add { task, other })) => {
            Store::open(&path)?.add_blocker(task, other)?;
            println!("task {task} is blocked by {other}");
        }
        Command::Task(TaskCommand::BlockedBy(BlockedByCommand::Remove { task, other })) => {
            Store::open(&path)?.remove_blocker(task, other)?;
            println!("task {task} is no longer blocked by {other}");
        }
        Command::Lead(LeadCommand::Add {
            repo,
            name,
            session,
            colour,
        }) => {
            let (lead, repo) = Store::open(&path)?.add_lead(repo, &name, &session, colour)?;
            println!("added lead {} for {repo}", describe(&lead));
        }
        Command::Lead(LeadCommand::Set {
            repo,
            name,
            session,
            colour,
        }) => {
            let (lead, repo) =
                Store::open(&path)?.set_lead(repo, name.as_deref(), session.as_deref(), colour)?;
            println!("lead of {repo} is now {}", describe(&lead));
        }
        Command::Lead(LeadCommand::Remove { repo }) => {
            let mut store = Store::open(&path)?;
            let (lead, repo) = store.lead(repo)?;
            // A listener holds the lock only while it runs, in a session that leads the repo.
            if control_tower::listen::listening(&path, repo)? {
                bail!(
                    "{} is listening: a board waiter for {repo} runs, so a session still leads \
                     it; close that session (or stop its waiter) first, then tower lead \
                     remove {repo}",
                    lead.name
                );
            }
            let evidence = format!(
                "{} was taken off {repo} with tower lead remove: nobody leads it now.",
                lead.name
            );
            let (lead, resolved) = store.remove_lead(repo, &evidence)?;
            control_tower::listen::forget_stream(&path, repo)?;
            let resolved = resolved
                .map(|t| format!("; resolved {t}, its \"Lead not listening\" thread"))
                .unwrap_or_default();
            println!(
                "removed the lead of {repo}, {}; its milestones, tasks and threads stay for \
                 the next lead (tower lead add {repo}){resolved}",
                describe(&lead)
            );
        }
        Command::Lead(LeadCommand::Attach { repo }) => {
            let session = std::env::var("CLAUDE_CODE_SESSION_ID")
                .ok()
                .filter(|v| !v.is_empty())
                .context(
                    "CLAUDE_CODE_SESSION_ID is not set: run tower lead attach from the lead's own \
                     Claude session (its Bash)",
                )?;
            let mut store = Store::open(&path)?;
            // Only the lead's own session attaches: Claude Code says who is asking.
            let (lead, repo) = store.lead(repo)?;
            let stop = std::sync::atomic::AtomicBool::new(false);
            let claude = control_tower::register::claude_path();
            let json =
                control_tower::register::claude(&claude, &path, &stop, &["agents", "--json"])
                    .context("cannot ask claude who is asking, so the session is not attached")?;
            check_asker(&json, &session, &lead, repo)?;
            let lead = store.attach_lead(repo, &session)?;
            println!(
                "attached {} as Claude session {session}; posts on the board of {repo} reach \
                 it through its board waiter (tower lead wait {repo}) and its Stop hook",
                lead.name
            );
        }
        Command::Lead(LeadCommand::Wait { repo }) => {
            control_tower::listen::wait(&path, listener_repo(&path, repo)?)?;
        }
        Command::Lead(LeadCommand::Unread { repo }) => {
            let mut store = Store::open(&path)?;
            let repo = store.lead(repo)?.1;
            control_tower::listen::unread(&mut store, &path, repo, &mut std::io::stdout())?;
        }
        // Answered by `lead_hook` before the board's path is read.
        Command::Lead(LeadCommand::Hook) => unreachable!("tower lead hook runs before this"),
        Command::Task(TaskCommand::Add {
            title,
            milestone: Some(milestone),
            ..
        }) => {
            let id = Store::open(&path)?.add_task(milestone, &title)?;
            println!("{id}");
            eprintln!("added task {id} under {milestone}");
        }
        Command::Task(TaskCommand::Add {
            title,
            milestone: None,
            repo,
        }) => {
            let id = Store::open(&path)?.add_standalone_task(&title, repo)?;
            println!("{id}");
            let into = repo.map(|r| format!(" in {r}")).unwrap_or_default();
            eprintln!("added standalone task {id}{into}");
        }
        Command::Task(TaskCommand::Hand { task, milestone }) => {
            let mut store = Store::open(&path)?;
            let lead = store.hand_task(task, milestone)?;
            let board = store.load()?;
            let m = (board.milestones.iter())
                .find(|m| m.id == milestone)
                .with_context(|| format!("{milestone} vanished after the task moved"))?;
            println!(
                "handed task {task} to {} on {milestone}; {milestone} is {}",
                lead.name,
                progress(m)
            );
        }
        Command::Task(TaskCommand::Stage { task, stage, by }) => {
            let mut store = Store::open(&path)?;
            let milestone = store.set_task_stage_by(task, stage, by.as_deref())?;
            let under = match milestone {
                None => match store.parent_of(task)? {
                    None => String::new(),
                    Some(parent) => format!("; {}", subtask_progress(&store.task(parent)?)),
                },
                Some(id) => {
                    let board = store.load()?;
                    let m = board
                        .milestones
                        .iter()
                        .find(|m| m.id == id)
                        .with_context(|| format!("{id} vanished after the task moved"))?;
                    format!("; {id} is {}", progress(m))
                }
            };
            println!("task {task} is {}{under}", stage.label());
        }
        Command::Subtask(SubtaskCommand::Add { title, task }) => {
            let mut store = Store::open(&path)?;
            let id = store.add_subtask(task, &title)?;
            println!("{id}");
            eprintln!(
                "added subtask {id} under {task}; {}",
                subtask_progress(&store.task(task)?)
            );
        }
        Command::Thread(ThreadCommand::New {
            board,
            by,
            task,
            title,
            message,
        }) => {
            refuse_owner(&by)?;
            let (id, repo) =
                Store::open(&path)?.open_thread(&board, &by, task, &title, &message)?;
            let about = match (task, &board) {
                (Some(t), _) => format!(" about {t}"),
                (None, BoardOf::Milestone(m)) => format!(" about {m}"),
                (None, BoardOf::Session(_)) => String::new(),
            };
            println!("{id}");
            eprintln!("opened thread {id} on the board of {repo}{about}");
        }
        Command::Thread(ThreadCommand::Resolve {
            thread,
            by,
            evidence,
        }) => {
            refuse_owner(&by)?;
            let t = Store::open(&path)?.resolve_thread(thread, &by, &evidence)?;
            println!("{thread} is resolved, {}", replies(&t));
        }
        Command::Thread(ThreadCommand::Show { thread }) => {
            let mut store = Store::open(&path)?;
            let t = store.thread(thread)?;
            let (repo, about) = store.thread_place(thread)?;
            let board = match about {
                Some(m) => {
                    let all = store.load()?;
                    let title = (all.milestones.iter())
                        .find(|x| x.id == m)
                        .with_context(|| format!("{m} vanished while {thread} was read"))?
                        .title
                        .clone();
                    format!("{repo}, about {m} {title}")
                }
                None => repo.to_string(),
            };
            let title = t
                .task
                .map(|id| store.task(id))
                .transpose()?
                .map(|x| x.title);
            let now = chrono::Utc::now().timestamp();
            print!("{}", show_thread(&t, &board, title.as_deref(), now));
        }
        Command::Post {
            thread,
            by,
            text,
            progress,
        } => {
            refuse_owner(&by)?;
            let text = if text == "-" {
                let mut read = String::new();
                std::io::stdin()
                    .read_to_string(&mut read)
                    .context("cannot read the message from stdin")?;
                read
            } else {
                text
            };
            let mut store = Store::open(&path)?;
            let (t, what) = if progress {
                (store.post_progress(thread, &by, &text)?, "progress ")
            } else {
                (store.post(thread, &by, &text)?, "")
            };
            println!("posted {what}to {thread}, {}", replies(&t));
        }
        Command::Ui => {
            // A brew tower opened by hand links the herdr plugin if herdr has none; it says
            // so only when it linked or could not, before the screen takes over.
            let herdr = control_tower::terminal::herdr::path();
            match register(&herdr, &path, brew_dir().as_deref()) {
                Ok(done @ Registered::Linked(_)) => eprintln!("tower: {done}"),
                Ok(_) => {}
                Err(e) => eprintln!("tower: could not link the herdr plugin: {e:#}"),
            }
            control_tower::ui::run(&path)?
        }
        Command::Space => {
            let herdr = control_tower::terminal::herdr::path();
            // herdr gives an action its server's socket and the workspace it was invoked from.
            let socket = std::env::var(control_tower::open::SOCKET_VAR).unwrap_or_default();
            let workspace = std::env::var("HERDR_WORKSPACE_ID").ok();
            let from = control_tower::open::Press {
                socket: &socket,
                workspace: workspace.as_deref().filter(|ws| !ws.is_empty()),
            };
            println!("{}", control_tower::open::space(&herdr, &path, from)?);
        }
        Command::Popup => {
            let herdr = control_tower::terminal::herdr::path();
            println!("{}", control_tower::open::popup(&herdr, &path)?);
        }
        Command::Watch => control_tower::watch::run(&path)?,
        Command::Upgrade => {
            // Opening for writing runs any migrations, and refuses a newer board.
            Store::open(&path)?;
            println!(
                "board {} is at schema version {SCHEMA_VERSION}",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_tower::board::{Message, Resolution};

    fn message(id: i64, author: &str, text: &str, at: i64) -> Message {
        Message {
            id,
            author: author.into(),
            text: text.into(),
            at,
        }
    }

    #[test]
    fn a_thread_shows_whole_oldest_first_with_ages_at_now() {
        let now = 1_000_000;
        let t = Thread::whole(
            ThreadId(3),
            "Task #2 team".into(),
            Some(TaskId(2)),
            Some(Resolution {
                by: "senior".into(),
                at: now - 120,
            }),
            vec![
                message(1, "cat-lead", "Opening it.", now - 2 * 86_400 - 5),
                message(2, "dev-2", "Plan:\n1. layout\n\n2. tests", now - 3 * 3600),
                message(3, "qa", "all green", now - 59),
                message(4, "senior", "approved", now - 120),
            ],
        );
        assert_eq!(
            show_thread(&t, "R1", Some("New book shelf"), now),
            "T3 Task #2 team
  status     resolved by senior, 2m ago
  task       #2 New book shelf
  board      R1
  started by cat-lead
messages
  cat-lead  2d ago
    Opening it.
  dev-2  3h ago
    Plan:
    1. layout

    2. tests
  qa  now
    all green
  senior  2m ago
    approved
"
        );
    }

    #[test]
    fn an_open_thread_with_no_task_or_a_task_gone_from_the_board_says_so() {
        let open = |task| {
            Thread::whole(
                ThreadId(4),
                "Chat".into(),
                task,
                None,
                vec![message(1, "qa", "hi", 10)],
            )
        };
        let shown = show_thread(&open(None), "R1, about M1 MVP", None, 10);
        assert!(
            shown.starts_with(
                "T4 Chat\n  status     open\n  task       none\n  board      R1, about M1 MVP\n"
            ),
            "{shown}"
        );
        // A linked task with no title given shows by its id alone.
        let shown = show_thread(&open(Some(TaskId(9))), "R1", None, 10);
        assert!(shown.contains("\n  task       #9\n"), "{shown}");
    }

    #[test]
    fn a_thread_line_names_its_task_only_when_asked_and_only_if_it_has_one() {
        let t = |id, task| {
            Thread::whole(
                ThreadId(id),
                "Team".into(),
                task,
                None,
                vec![
                    message(1, "cat-lead", "go", 0),
                    message(2, "dev-2", "copy", 1),
                ],
            )
        };
        assert_eq!(
            thread_line(&t(3, Some(TaskId(2))), true),
            "T3 Team (open, about #2, by cat-lead, 1 reply)"
        );
        assert_eq!(
            thread_line(&t(3, Some(TaskId(2))), false),
            "T3 Team (open, by cat-lead, 1 reply)"
        );
        assert_eq!(
            thread_line(&t(5, None), true),
            "T5 Team (open, by cat-lead, 1 reply)"
        );
    }
}
