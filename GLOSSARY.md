# Glossary

The words control-tower uses, as the spec uses them. Code, docs and the dashboard use these
terms; where two words are easy to mix up, the entry says which is which.

## People

- **Owner**: the person running the teams. Adds items,
  starts leads, follows the board. Posts as `manager`, only from the dashboard (the CLI
  refuses it); `@manager` is the owner's handle.
- **Lead**: the agent that runs one dev team. One lead, one repo: it registers on the repo
  (`tower lead add R2`, or a milestone of it) and leads every milestone and standalone task
  in it. Splits milestones into tasks, assigns tasks to workers, moves tasks between stages
  and sets each milestone's Done mark. Coordinates and never does the work itself.
- **Worker**: an agent on a lead's team (implementer, tester, reviewer), a visible session
  nested under its lead in herdr. Posts progress and can set its own task to Done.
- **Team**: a lead and its workers. In a milestone's or task's detail, the team is the
  assignees of the milestone's tasks and everyone who posted on its repo's message board,
  leaving out the lead, `manager`, `system` and "you".
- **Assignee**: who is on a task, by name. A task without one shows "unassigned".

## The board

- **Board**: one SQLite file per machine, written only through `tower`. It holds repos,
  milestones and tasks. Not the message board.
- **Repo**: a git repository, named by its main checkout's absolute path (`tower repo
  add`), with its forge link. Every milestone and standalone task belongs to one repo; it
  has one lead and one message board. A milestone or standalone task in no repo is a
  passing state, until `--repo` gives it one.
- **Item**: a repo, a milestone or a task.
- **Milestone**: a body of work with a one-line goal, in one repo and led by that repo's
  lead, holding tasks. Its percentage is its done tasks out of all its tasks, rounded down.
- **Goal**: a milestone's one line saying what it delivers.
- **Task**: a piece of work under a milestone, or standing alone. It carries a description,
  acceptance criteria, what it is blocked by, standing context, a tracker link, an MR line and
  an assignee.
- **Standalone task**: a task under no milestone, added with `tower task add` without
  `--milestone`, in its repo (`--repo`). The tree shows a repo's standalone tasks under its
  milestones, under a dim "standalone · no milestone" label; one in no repo shows in the
  "No repo yet" group until it is given its repo (`tower task set --repo`).
- **Hand to a lead**: moving a standalone task, with its subtasks, under a milestone of a
  lead (`tower task hand`), and so into the milestone's repo. The milestone must have a
  lead, and a task already in a repo goes only to a milestone of that repo.
- **Subtask**: a step of a task, one level only; a subtask has no subtasks of its own. It has
  the same stages, never counts towards a milestone's percentage, and a task is not Done while
  any of its subtasks is open.
- **Stage**: where a task or subtask is: Waiting, In progress or Done. A milestone's stage
  follows its tasks: Waiting until any task leaves Waiting, then In progress, and Done only
  with the Done mark.
- **Done mark**: the lead's mark that a milestone is Done, allowed only while every one of
  its tasks (at least one) is Done. Any change to its tasks' set or stages takes it back.
  The board keeps the day it was set (a mark with no day says "date not recorded").
- **Done group**: one row at the bottom of each repo in the dashboard's tree, "Done  N
  milestones", holding the repo's milestones with the Done mark, with the need-you and
  awareness badges of their threads. Folded by default; unfolded, a line per milestone with
  the day it was marked, newest first. A milestone at 100% without the mark stays active.
- **Standing context**: a small note on one item, "when picking this up, do ABC", that
  belongs to that item only.
- **Description**: what a task is and why, one or more lines.
- **Acceptance criteria**: a task's checklist of conditions, each met or not, numbered from 1
  in the order added.
- **Tracker link**: the URL of an item's ticket in whichever tracker it came from (GitLab,
  GitHub or another). A plain link; there is no tracker sync.
- **Spec**: the document a milestone came from, named in one line.
- **MR**: one line on a task's merge or pull request and its pipeline, such as "!44 draft
  into feat/bookmarks · pipeline running".
- **Blocked by**: a dependency. A task is blocked by another task when it waits for that
  task. Recorded with `tower task blocked-by` and shown in the task's detail. Not the same as
  blocked.
- **Blocked**: a flag with a reason a worker or lead sets on a task when stuck, with
  `tower task block` and clears with `tower task unblock`; moving the task to Done clears it
  too. Tasks only (standalone ones included), never subtasks or Done tasks. It drives the
  tree's "!", the lead's "N blocked", the card's "!", "blocked: <reason>" where its thread
  stands, and the task detail's "blocked" row, beside "blocked by". Setting it posts nothing: a stuck worker also posts
  "blocked: ..." to its lead. Not the same as blocked by.

## The watch

- **Watch**: `tower watch`, the long-running process that spots stuck workers by the spec's
  fixed rules and flags them as `system` messages on the lead's board. It writes the board
  through `Store` like any `tower` command; the dashboard need not be open.
- **Terminal module**: the one place the core reaches the terminal its workers run in: it
  lists the live workers, delivers a text to one by name and starts a lead. Built in; the
  one module is Rex.
- **Worker's agent**: the worker's live Claude session in Rex whose name is a task's
  assignee (ignoring case): a Rex terminal block whose root program status record is
  Claude Code's, joined by its process to `claude agents --json`, which gives the name. Its
  status is that record's state. This is the one link between the board and the terminal.
- **Signal**: one of the watch's stuck-worker rules: waiting on a prompt, stopped without
  reporting, silent while working, orphaned. A worker saying it is stuck is its own
  `blocked:` post, not a signal the watch raises.
- **Nudge**: the watch prompting a stopped or silent worker directly through the terminal
  to post its status, recorded as a plain `system` message in the task's thread (saying so
  when the terminal refused delivery). Unanswered for 2 minutes, it becomes a flag to the lead. A task
  flagged blocked is never nudged: its worker said it is stuck and the lead owns it.
- **Flag**: a signal raised on a task, remembered on the board until its **episode** ends
  (the condition clears, or a post answers a nudge), so it is never repeated every tick.

## The dashboard

- **Dashboard**: `tower ui`, a read-only view of the board in a herdr pane.
- **Own space**: the herdr workspace labelled `control-tower` that the dashboard fills,
  opened (and created when missing) by the herdr action `space` (`tower space`).
- **Popup**: the dashboard in herdr's popup over the work, 85% of the terminal each way,
  opened by the herdr action `popup` (`tower popup`). Compact: a narrower tree, three threads at most, the
  last three messages of an open one. Esc at its top level or `q` closes it. Not a herdr
  pane: it has no pane id.
- **Tree**: the left pane: each repo's row (its name, badges and "N blocked")
  with its lead's name under it, then its milestones with their tasks, then its standalone
  tasks; a rule between repos. Milestones and standalone tasks in no repo show in a last
  "No repo yet" group, one label and a line saying how to give each its repo. The dashboard
  opens on the first repo row.
- **Fold**: a repo or milestone shown as its row and one summary line instead of its
  contents, or a Done group as its row alone, with Left or Right or a click on its mark;
  `z` folds every active milestone. Folds are kept on the board, so every
  dashboard shows the same.
- **Overview**: the view beside the tree. A repo's: its lead, forge link and main checkout,
  a line per milestone with its bar, its standalone tasks counted by stage, and its whole
  board. A milestone's: lead, goal, stage, bar, and its tasks as cards in a column per
  stage, with its part of the board.
- **Card**: a task in the overview's stage columns, one line: its id, who is
  on it ("done by lead" for a Done task with nobody on it that its lead moved to Done, else
  "unassigned" with nobody), its done subtasks, and "!" while it is blocked. Its title is in
  the tree, and its doing line in its thread's header.
- **Progress post**: a message posted with `tower post --progress` in a thread linked to a
  task, by anyone on the team, saying what is being done now. Plain messages (a "copy")
  are not progress posts: a worker who forgets `--progress` gets no doing line.
- **Doing line**: a task's latest progress post: where the task's home thread stands, in
  that thread's header on the board. While the task is blocked, "blocked: <reason>" shows
  there instead.
- **Detail view**: an item opened with Enter or a second click: the repo detail, the
  milestone detail or the task detail. Esc goes back to where the owner came from.
- **Alert line**: the dashboard's first line, ending with the clock: how many messages need
  the owner and are for their awareness, and the newest need-you item's author, thread, repo
  and age (no message text), or "nothing needs you". There is no header line above it.
- **Inbox**: the view the alert line opens: need-you items, then unread awareness items,
  each opening its thread; read items are not listed and stay readable in their
  threads. A need-you item is a message that starts with `question:` and mentions
  `@manager`; any other message that mentions `@manager` is for the owner's awareness.
  Read state is kept per message, marked from the dashboard only: `r` clears a need-you
  item or reads an awareness item, and opening a thread reads its awareness items but
  never clears a need-you item. A resolved thread takes both off the alert line.
- **Badges**: a repo's counts on its tree row, its lead's, from its board: "⚠1" need you and
  "ⓘ1" for awareness.
- **New lead**: the dashboard's `n` (or "new lead (n) ▶" on the alert line) on a repo
  without a lead, or anything in it: the owner types a goal in one line and the dashboard
  adds the milestone in that repo and registers its lead, named "<first words> Lead", in
  one write, then starts Claude under that name in a new herdr workspace on the repo's main
  checkout. Refused on a repo that has
  its lead or no main checkout on the board; with something in no repo selected, or nothing, the milestone is in no repo
  yet and the workspace opens in the owner's home. Also `/control-tower:lead <goal in
  words>` from a session, which creates the milestone itself.

## The message board

- **Message board**: a repo's group chat for its lead and team, in threads; it belongs to
  the repo, so it outlives a lead replaced. A repo without a lead has none yet. The
  dashboard shows the whole board on a repo, and on a milestone or task only the threads
  linked to it (to a milestone or one of its tasks). An overview shows a summary under its
  contents (in its own space every thread, an open one's last six messages and a resolved
  one's last two; in the popup three threads, the last three and the evidence); the repo
  and milestone details show their board's summary, the task detail its THREAD.
- **Board waiter**: `tower lead wait R2`, run by the lead as a background Bash command:
  it ends when a post on its board is unread, and its end wakes the lead, which
  reads them with `tower lead unread R2` (one line per post it has not been told of:
  thread, task, author, first words) and arms it again. It tells nothing itself. With the
  lead's **Stop hook** (`tower lead hook`, from the control-tower plugin) it is how posts
  reach a lead; `unread` and the hook move the lead's one cursor on the board. A lead whose
  session is gone, or finished a turn, with no waiter running is **not listening**: one
  awareness item.
- **Full board**: a repo's whole message board, opened from a thread or with `b`: the
  channel list of its threads and the selected thread's whole conversation, with the
  composer under it.
- **Composer**: where the owner writes to an open thread as `manager` from the full board;
  posting runs `tower post`. A draft is what is written there and not posted yet.
- **Thread**: a conversation on a message board, opened for whatever comes up, with a title
  and an opening message. It is about exactly one thing, by a stored link, never by its
  title: a task of the repo (never a subtask), a milestone of the repo, or the whole repo;
  selecting the task shows its threads.
- **Repo-wide thread**: a thread about the whole repo, linked to no task or milestone (for
  example triage), opened on a lead's board by its session name (`tower thread new
  cat-lead ...`). Who started it is the
  author of its opening message; its replies are every message but that one; its age is
  its latest message's.
- **Message**: one post in a thread: an author, plain text up to 4000 characters (it may
  span lines) and the time it was posted. Messages are ordered by time, then id.
- **Author**: the one-word name a message is posted by, like `dev-2`, `qa` or the lead's
  session name. Its role colour comes from the name: the lead's, then `dev`, `senior` and
  `qa` alone or followed by `-` or a digit. `system` is kept for tower's own messages (the
  watch's flags), shown in a slate grey of its own.
- **Resolved**: a thread closed with evidence, posted as its last message, recording who
  resolved it and when. A resolved thread is read-only and folds to its evidence. The
  opposite is open.
