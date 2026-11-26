# control-tower

A taskboard for a team of Claude Code agents in the terminal (Rex): what the work is, how
far along it is, and where the owner is needed.

It is one binary, `tower`. Agents change the board only through the `tower` commands,
which check every write and refuse a bad one with a message and a non-zero exit. The
dashboard, `tower ui`, shows the board in a terminal tab and refreshes on its own; the
owner's actions as `manager` (posting, marking an inbox item read) go through it, with the
same checks, and the commands refuse them.

## Build

Needs a Rust toolchain (edition 2024). SQLite is built in.

    cargo build --release

The binary is `target/release/tower`. A change to this repo is checked and built in CI, not
on the machine it is written on: SDLC.md, Building and testing.

## Run

The owner starts work without typing `tower`, in either of two ways:

- **From the dashboard:** select a repo without a lead (or anything in it), press `n` (or
  click "new lead (n) ▶" on the alert line), type the goal in one line (up to 200
  characters) and press Enter; Esc cancels and an empty goal is refused. The input says
  where the lead starts: "new lead on search, its goal:". In one write the dashboard adds
  the milestone in that repo (title and goal from that line) and registers its lead on the
  repo, named after the repo: its name in lowercase ASCII letters and digits, each run of
  anything else one `-`, plus `-lead` (`checkpoint-lead` on repo `checkpoint`,
  `my-app-v2-lead` on `My App.v2`); in no repo yet, or for a name with no letter or digit,
  after the milestone (`m4-lead`); a name another lead or a task's assignee has gets the
  milestone id after it (`checkpoint-lead-m4`). It is both its name and its session, one
  word a mention reads whole (`@checkpoint-lead`). The repo's row shows the lead at once,
  and a second `n` on it is refused. Then it opens a new Rex session named
  after the lead on the repo's main checkout, running `env TOWER_DB=<the dashboard's board,
  absolute> claude -n <lead name> "/control-tower:lead <M>"`, so the lead uses the board
  you are looking at. Rex does not show a new session in the app, so your focus stays where
  it is. The skill finds itself registered, arms its board waiter and attaches. A repo has
  one lead: on a repo that has one, or anything in it, `n` is refused, naming the repo and
  its lead. A repo whose main checkout is not on the
  board yet is refused too, saying to set it with `tower repo set R<n> --path <checkout>`,
  and so is one whose main checkout is no longer a directory.
  With something in no repo selected (or nothing), the milestone is added in no repo yet and
  the session opens in your home directory, where the lead asks you for its repo's main
  checkout and registers itself. The keys line says "started search-lead for M3 on
  search in Rex". When Rex fails, the keys line says why and the start is undone in one
  write: the milestone and the lead it registered are removed again (a milestone something
  already uses, or a lead that has attached, stays), so `n` can simply be pressed again.
  When `rex new` is stopped, or replies without the new session's id, the keys line also
  says a session with that label may be left, to `rex kill`.
  Every argument reaches Rex as its own word, never as a shell string, and of the goal only
  the lead name does (made of letters, digits, spaces, `-`, `_` and `.`, starting with a
  letter or digit). `TOWER_REX` names rex; else tower takes `rex` in `REX_BIN_DIR`, where
  Rex keeps it and which Rex sets in its own tabs, else `rex` on the PATH. A rex that cannot
  be found is refused before anything is written, saying what was tried.
- **From a session in progress:** `/control-tower:lead <the goal in words>` creates the
  milestone from those words and the conversation, then leads it; `/control-tower:lead M3`
  takes over M3. A session with no name is told which `/rename` to type.

The commands underneath, which leads and scripts use:

    tower repo add ~/src/catalogue --link https://github.com/example/bookshop
    tower milestone add "First shop pages" --repo R1 \
        --goal "A reader can find a book, see its page and save it."
    tower lead add R1 --name "Catalogue Lead" --session cat-lead
    tower task add --milestone M1 "Bookmarks page"
    tower ui

Titles and goals are one line of plain text, up to 200 characters. The commands that
create something with a new id (`repo add`, `milestone add`, `task add`, `subtask add`,
`task criterion add` and `thread new`) print only the new id on stdout, like `R1`, `M1`,
`#1`, `1` or `T1`, so a script can capture it (`M=$(tower milestone add ...)`); the
sentence for a person, like "added milestone M1", goes to stderr.

A repo is a git repository, named by its main checkout: `tower repo add` takes the
checkout's absolute path (a directory whose `.git` is a directory, so not a linked
worktree), stores it with links resolved and refuses one already on the board; `--link` is
its forge link. `tower repo set R1 --path ... --link ...` changes either, and `tower repo
show R1` prints the repo with its lead, milestones, standalone tasks and its threads, in
the dashboard's order; `tower repo list` gives every repo on one line each: its id, main
checkout, lead, milestones and standalone tasks, then one line naming the milestones and
standalone tasks in no repo yet, if any. Every milestone and standalone task belongs to one
repo: `--repo` on `milestone add`, `milestone set`, `task add` (standalone) and `task set`
(standalone) says which. A milestone or standalone task moved to another repo takes its
threads (and a milestone's tasks' threads) along, says where it is now, its lead there and
how many threads went, and is refused while it has threads and the new repo has no lead;
`task set --repo` is refused for a milestone's task, whose repo is its milestone's, and for
a subtask. A repo can have a lead and no path; leave that path unset.
Once such a lead is closed, the repo's lead adopts its milestones
with `tower milestone set M3 --repo R1` (the lead skill says when). A milestone with no
lead, and a standalone task added with no `--repo`, has no repo until `tower milestone set
M3 --repo R1` or `tower task set 12 --repo R1` gives it one; `tower repo list` names them.
That is a passing state, not something to make anew.

`tower repo remove R5` retires a repo with nothing left in it, such as one an old lead
left with no path: it removes the repo, its lead and the threads left on its
board with their messages (and so their inbox items), in one go, and says what went, like
"removed R5: its lead Old (session old-lead), 1 thread, 2 messages, 0 inbox items". It
refuses a repo that still holds a milestone or a standalone task, naming each; move them
first with `tower milestone set M3 --repo R1` or `tower task set 12 --repo R1`, or remove
an abandoned standalone task with `tower task remove 12`. The dashboard and the watch stop
showing it on their next read, and a running `tower lead wait R5` ends with the line
"control-tower: R5 was removed from the board (tower repo remove); this waiter ends, so do
not arm it again." Its lock file (`board.db.R5.stream`) goes too: `repo remove` deletes one
no waiter holds, and a running waiter deletes its own at its next look, within a second. One
killed before that look leaves an empty lock file, which the next `tower repo remove R5` or
`tower lead wait R5` clears (both are refused, R5 being gone). The count of inbox items is
what the owner's inbox listed of those messages: need-you and unread awareness items, not
read ones.

Each repo has one lead, and each lead one repo, leading every milestone in it: `tower lead
add` refuses a second lead on a repo, a lead name or session that already leads
another one, and a session that is any task's assignee, naming the tasks: a lead is never a
task's assignee, so unassign them first (`tower task unassign N`), as `tower task set
--assignee` refuses a lead's session. The lead commands take the repo (`R1`) or one of its
milestones (`M1`).
`--name` is what the dashboard shows; `--session` is the Claude session name workers
message the lead by. Both are one line of plain text, up to 200 characters, and the
session is ASCII letters, digits, `-`, `_`, `.` and single spaces between words (Claude
session names can have them, like "Catalogue Lead": quote such a name in commands; a
mention stops at a space, so the dashboard names a new lead `<repo>-lead`, like
`catalogue-lead`, the lead skill suggests that name to a session that has none, and leads
already on the board keep their names);
both are compared ignoring the case of ASCII letters. `--colour`
picks the lead's colour from blue, purple, orange, teal, pink and green (the first three
are the signed design's); without it the lead gets the first colour no lead has yet.

    tower lead set M1 --session cat-lead-two

`tower lead set` changes a lead's `--name`, `--session` or `--colour` by the same rules,
for example when its Claude session was restarted under a new name, or another session
takes over the repo: the message board belongs to the repo, so its threads stay.

    tower lead remove R5

`tower lead remove` takes a repo's lead off the board, for a lead nobody runs any more,
such as the old lead of a repo with no path, so the watch stops saying it is
not listening. The repo, its milestones, tasks and threads stay for the next lead (`tower
lead add R5`); the lead's open "Lead not listening" thread is resolved, which takes its item
off the alert line, and its lock file goes. Its name and session are kept as a retired
lead's, so an @mention of it later tells the post's author, not you. It says what
went, like "removed the lead of R5, Old (session old-lead, colour blue); its milestones,
tasks and threads stay for the next lead (tower lead add R5); resolved T7, its "Lead not
listening" thread". It refuses a repo with no lead, and one whose board waiter runs: a
session still leads it then, so close that session first. To retire a repo and its lead
together, remove its abandoned standalone tasks (`tower task remove`), move its milestones,
then `tower repo remove`.

The dashboard groups each milestone and its tasks under its lead's name in that colour, or
says "no lead yet". It picks the colour depth from `COLORTERM`: 24-bit when it is
`truecolor` or `24bit`, the 256-colour palette otherwise.

    tower task stage 12 in-progress
    tower milestone done M1

A task is `waiting`, `in-progress` or `done`, and starts waiting; `tower task stage` moves
it to any of the three (the id is the number; `#12` works too, quoted, since an unquoted
`#` starts a shell comment). The tree marks each task ✓ done, ● in progress or ○ waiting.

A milestone's percentage is its done tasks out of all its tasks, rounded down, so 199 of
200 is 99%; its 8-cell bar is filled from that percentage, also rounded down. A milestone
is in progress on its own once any task has left waiting. Only its lead marks it done,
with `tower milestone done`, which is refused while any task is not done and for a
milestone with no tasks. Moving a task out of done, or adding a task, takes the mark back:
the milestone is in progress again until the lead marks it done once more.

    tower subtask add --task 12 "Bookmark icons"

A task can have subtasks, one level only: a subtask of a subtask is refused (it goes back
to the lead, who adds a new task). Subtasks share the tasks' ids and their three stages,
and `tower task stage` moves them too. They never count towards the milestone's
percentage or its done mark; the task shows its own count instead, for example `#12 is in
progress, 3/5 subtasks done` in the output of `tower task stage` and on the stderr of
`tower subtask add`.
A task cannot move to done while any of its subtasks is not done (the merge can be a
subtask, so progress never reads complete while the owner's review is pending), and a
subtask moved out of done, or added, takes a done task back to in progress, which takes
back its milestone's done mark. The tree
does not show subtasks; the dashboard's task cards show their count, for example "3/5",
and so do standalone task rows.

    tower task set 12 --description "What and why." --assignee dev-2 \
        --context "Use the sample catalogue." --link https://github.com/o/r/issues/12 \
        --mr "!44 draft into feat/bookmarks · pipeline running"
    tower task criterion add 12 "Loads in under 2 s"
    tower task criterion check 12 1
    tower task criterion remove 12 1
    tower task unassign 12
    tower task remove 12
    tower task blocked-by add 12 9
    tower milestone set M1 --context "One branch per task." --spec docs/spec.md \
        --link https://github.com/o/r/milestone/1
    tower task show 12
    tower milestone show M1

Items carry details for their detail views. `tower task set` sets any of a task's
`--description` and standing `--context` (plain text, line breaks allowed, up to 2000
characters), its tracker `--link` (one `http` or `https` URL with a host, read by a URL
parser), its `--mr` (one line on its merge request and pipeline, like "!44 draft into
feat/bookmarks · pipeline running"), its `--assignee` (one line) and, for a standalone
task,
its `--repo` (see repos above); each replaces what was stored, and the assignee replaces
"unassigned" on the task's card. The link is where the dashboard and the listings take a
task's ticket from: `tower milestone show` and `tower repo show` list each task with
`ticket #64`, `ticket !19`, the link for another tracker, or "no ticket". `tower milestone
set` sets a milestone's `--title` and `--goal` (one line each, as `milestone add` takes
them), `--context` (as a task's), `--spec` (one line, the spec it came from) and `--link`.
Details belong to tasks, not subtasks. Acceptance criteria are one line each, a task has
each once, and they are numbered from 1 in the order added: `tower task criterion check`
marks one met, `uncheck` marks it not met again, and `remove` takes it off, moving the ones
after it up a number (it says so). `tower task unassign` takes a task's assignee away.
`tower task blocked-by add` records that a task is blocked by another task, a dependency it
waits for, refusing the task itself, a task it is already blocked by and one that would
close a loop; `blocked-by remove` takes it back. Blocked by is not the blocked flag a worker
sets when stuck (see GLOSSARY.md). `tower task remove 12` removes an abandoned standalone
task, Done or not, with its subtasks, criteria, blocked-by records and the threads linked to
it with their messages (a task's thread is on the board through its task), in one go, and
says what went, like `removed #12 Spike: 2 subtasks, 1 criterion, 1 thread, 2 messages, 0
inbox items`; its repo can then be removed. It refuses a milestone's task and a subtask, a
task with an assignee (unassign it once nobody is on it) and one another task is blocked by,
naming each with the `blocked-by remove` that frees it. `tower task show` and `tower
milestone show` print an item with all of it (a standalone task its repo, or "none yet" with
the command that gives it one); each ends with its threads (`task show` the ones linked to
the task, `milestone show` all of its repo's, each one linked to a task naming it, as in
`about #2`), open ones first, then resolved, each newest activity first, as the dashboard
lists them. None of this changes stages or a Done mark.

    tower thread new M1 --by cat-lead --task 12 "Bookmark sync" "Opening this thread for its team."
    tower post T1 --by dev-2 "copy, reading the spec"
    printf '%s\n' "-1 on this, see T2" | tower post T1 --by qa -
    tower thread resolve T1 --by senior "bookmarks match on 3 phones and 2 tablets"
    tower thread show T1

Each repo has a message board of threads, kept when its lead is replaced. `tower thread
new` opens one on the board of a milestone's repo (`M1`), about that milestone, or on the
board of a lead's repo by the lead's session name (`cat-lead`), about the whole repo, with
its title and opening message; it refuses a repo with no lead. `--task` links the thread
to a task by its id instead, a stored link never matched by title, and only to a task of
that repo (not a subtask or another repo's task). The dashboard shows a repo's whole
board on the repo, and on a milestone or task only the threads linked to it. `tower post` adds a
message to an open thread, and `tower thread resolve` resolves it with evidence, posted as
its last message, with who resolved it and when; after that the thread is read-only and
posting to it is refused. `tower thread show` prints a thread whole: its title, status (open, or
who resolved it and how long ago), the task it is about, its board and who started it,
then every message oldest first, each its author and age (as on the full board: "now",
"8m ago") above its full text. A message of `-` alone makes `tower post` read the message
from stdin instead, so a message starting with `-` is never taken for an option. Authors
(`--by`) are ASCII letters, digits, `-`, `_`, `.` and single spaces between words, so a lead
posts as its session name, quoted when it has spaces (`--by "Catalogue Lead"`), and a
lookalike such as a fullwidth or Cyrillic "manager" is refused (names already on the board
still show). `manager` and `system` are refused in any case and spacing. Authors are
compared ignoring ASCII case for colour: the lead's session or display name shows in the lead's colour; `dev`, `senior` and
`qa`, alone or followed by `-` or a digit (`dev-2`, `qa1`), in teal, lilac and pink;
anyone else (like `manager`, or `devon`) in the default colour. `system` is kept for
tower's own messages, and `manager` for the owner: only the owner posts as `manager`, from
the dashboard, so `tower post`, `tower thread new` and `tower thread resolve` refuse
`--by manager` in any case. A thread's title is one line of plain text, up to 200 characters;
messages and evidence are plain text up to 4000 characters and may span lines, which the
board keeps as it wraps them. Pasted text works: Windows (CRLF) and old Mac (CR) line ends
become plain line breaks and a tab becomes four spaces before the text is checked. Leading
blank lines, a hidden whitespace character such as a form feed before the text, and the end's
spaces and blank lines go; an indented first line keeps its indent, so a list reads in line. A leading `blocked:`, `decision:` or `question:` counts only at the
very start: after an indent it is plain text.

    tower task add "Sort the poetry shelf by poet"
    tower task hand 31 M1

`tower task add` without `--milestone` adds a standalone task, under no milestone, by the
same checks. It takes subtasks, details and stages like any task. `tower task hand` hands a
standalone task, with its subtasks, to the lead of a milestone: it moves under that
milestone, which must have a lead, and into its repo. A task already under a milestone, a
subtask, and a task in another repo than the milestone's are refused. The milestone's
task set changes, so its done mark goes back, as with a new task.

    tower task block 14 "Printer has not sent the cover images"
    tower task unblock 14

A worker or lead flags a stuck task with `tower task block` and a reason (one line, up to
200 characters); blocking a blocked task replaces its reason, and `tower task unblock`
clears the flag, as moving the task to done does (a done task is not stuck, so blocking
one is refused). Blocked is a flag on tasks, standalone ones included: a subtask is
refused (block its task). The flag alone puts an amber "!" on the task's row in the tree
and on its card, "N blocked" on its repo's row, "blocked: <reason>" where its thread
says where it stands, a "blocked" row in its detail and the reason beside the task in `tower milestone show`. It changes no stage and is not blocked by.

Blocking posts nothing. A stuck worker does both: `tower task block` for the board, and a
post starting "blocked: ..." on its lead's message board, so the lead hears of it.

    tower post T1 --by dev-2 --progress "testing a bookmark kept across two devices"

A task's doing line is its latest progress post, a message posted with
`tower post --progress`, by anyone on the team, in any thread linked to the task
(`thread new --task`); `--progress` is refused in a thread linked to no task. Plain posts,
like "copy", never become the doing line, so a worker who forgets `--progress` gets no
doing line. Its first line is where the task's first open thread stands, under the
thread's header on the board; the task detail and `tower task show` show it whole. While
the task is blocked its reason shows there instead; its detail keeps the doing line and
shows the reason on a "blocked" row of its own. With no progress post yet there is no
doing line.

A milestone's stage shows in the output of `tower task stage` and `tower milestone done`,
for example "M1 is in progress, 1/5 tasks done (20%)", and in the dashboard's milestone
overview.

The dashboard shows the tree on the left and the view of the selection on the right. The
tree is the repos, each under a rule from the one above: a repo's row is its fold mark and
name, with its lead's badges at the right ("⚠1" need you in red, "ⓘ1" for your awareness)
and "N blocked" in amber while any of its tasks carries the blocked flag; its lead's name is
under it in the accent colour, or "no lead yet: n starts one". Then its milestones, each
with its bar and percentage and its tasks, then its standalone tasks under a dim "standalone
· no milestone" label with their done count, a blank line giving each milestone and the
label air. Task rows are quiet: the stage mark, a dim title (normal text only while In
progress), an amber "!" while blocked and the task's ticket at the right: the number its
tracker link names, in the accent colour (`#64` for a GitLab issue or work item or a GitHub
issue or pull request, GitHub Enterprise included, "!19" for a GitLab merge request,
whatever page of it the link opens, "link" for any other tracker), or a dim "no ticket" for
a task with no link. `o`, or a click on the ticket, opens the link in the browser; if the
browser cannot be opened, the keys line says so (`could not open #64: open exited 1`).
Tower's own task id is not on the row; it is in the task's detail and the CLI. A standalone
task adds its assignee (or "unassigned") and done subtasks, ` #64 · dev-3 · 1/3`, and drops
them before its ticket in a narrow tree. Milestones and standalone tasks in no repo on the
board stay visible in a last group under a rule: one "No repo yet" label and a dim line
saying how to give each its repo ("tower milestone set / task set --repo R<n>"), then its
milestones with their tasks and its standalone tasks.

A milestone with the lead's Done mark leaves its repo's milestones for the repo's Done
group, one row at the bottom of the repo: "▸ Done  3 milestones", with the need-you and
awareness badges of the threads linked to those milestones at the right, so a folded group
never hides what needs you. It starts folded; unfolded, it shows a line per milestone, most
recently marked first: "✓ M6 Author pages" with the day it was marked Done (DD/MM/YYYY,
or "date not recorded" for a mark set before the board kept the date). Selecting the group
lists its milestones, each with its tasks and day ("2/2 · 14/01/2027"), with the board
narrowed to their threads; Enter opens nothing more. Selecting a Done line shows that
milestone's overview, "marked Done 14/01/2027" (or "marked Done, date not recorded") beside
its bar, read-only: its cards and its detail's task rows select nothing. A milestone at 100%
without the mark stays among the repo's milestones, its overview saying "Done mark not set",
and any change to a Done milestone's tasks takes the mark away, so it comes back.

The dashboard opens on the first repo row, with its overview.

Selecting a repo shows its overview: its name, lead and forge link with "open repo (enter)
▶", its main checkout, a line per active milestone with its stage mark, bar and percentage
(a click selects it), its standalone tasks counted by stage and its Done milestones counted,
and its whole message board. Enter opens the repo's detail: its lead, team (everyone on its
tasks and its board), main checkout, tracker link and work, its milestones (its Done ones
under a "Done" label, each with its tasks and day, "2/2 · 14/01/2027"), its standalone tasks
(a click on one opens its detail) and its whole board.

Selecting a milestone shows its overview: its lead and tracker link ("M1  First shop pages ·
Catalogue Lead · <link>", cut to the width) with "open milestone (enter) ▶", or "open
task (enter) ▶" with a task selected, at the right, its goal (in a readable grey, wrapping
to two lines), its stage, a 24-cell bar with the done count, and its tasks as cards in three
columns, waiting, in progress and done, each column as tall as its cards. A card is one
line: its ticket in the accent colour when it has one, who is on it
("unassigned" with nobody, or "done
by lead" for a done task the lead finished, `tower task stage N done --by <lead's
session>`), its subtasks done of all, dim, and a bold amber "!" while it is blocked. Its
title is in the tree, and what it is doing in its thread's header. In the tree a milestone's
or task's title is shown whole, wrapping to an indented second line and, only past two,
ending in "…" after the last piece that fits, the first line keeping whole words; task
titles are dim but for tasks in progress
and the selected one, and a rule and a blank line part the repos. Selecting a task keeps its
milestone's overview and highlights the task's card on the selected background. Under the
columns is the milestone's part of its repo's message board, the threads linked to it or one
of its tasks, under a heading that names it ("MESSAGE BOARD · M1 and its tasks <lead>", `·
#13 <lead>` with a task selected, "· whole repo <lead>" on a repo), with "open full board
(b) ▶" on it: open threads first, then resolved ones, each group by its latest message,
newest first. Every board starts with a pinned "Needs you" strip: the open questions for the
owner in that board's threads (those `r` has not cleared), newest first, each its author,
its first line and the thread it is in; "nothing on this board needs you" when there are
none. The overview and detail views scroll, so they list every question; the full board
gives the strip at most a third of its height, and always one question, ending it with how
many more are in the inbox when they do not all fit. A click on one opens the full board on
that question, selected, the keys writing to its thread and a "replying to" line over the
input: "replying to" and the author stay whole, and the question is cut to fit.

A thread reads in words: one linked to a task is named by the task's title, with
the task's stage and who is on it, dim; one not about a task keeps its title, "on M1" or
"repo-wide". Its id ("T3"), how many messages and its age sit at the right of its header,
and under it a line on where it stands. On the task's home thread (its first open thread a
person has posted in) that is the task's doing line (its blocked reason while it is
blocked); on any other thread, and before any progress post, the first line of its newest
message by a person that is not an ack; either way without a "Status:" prefix. Tower's own
word there is in the chat's words for it, with no "system:" label, in the text colour like a
person's words; a question for you keeps its "question:", having no "needs you" tag there. A
thread holding only tower's messages (the watch's nudges and flags) about a task with a home
thread is no thread of its own: its messages join the home thread's conversation in time
order, counted in its messages and age. A resolved tower thread joins too (its lines are
history), but nothing joins a resolved thread: an open tower thread beside a resolved one
stays in the list, so a question in it can still be answered. Every open thread shows as a
compact chat preview, its last three bubbles, each bubble its last two messages folded to
two lines, after "N earlier messages", then "open chat ▶". Resolved threads fold into one
row under the open ones, "RESOLVED 3 ▸", as on the full board: a click unfolds it ("▾") to
one line per resolved thread, a green ✓, its name and its age, and a click folds it again. A
board's overview and detail share the fold, and every board starts folded each time the
dashboard starts. A click on a resolved thread's line opens it on the full board. A preview
is drawn from the thread's latest messages the board loads (twelve for an open thread, four
for a resolved one): acks at the start of them that answer a message not loaded count among
the earlier ones, and when those latest messages are all tower's (nudges, flags) the preview
shows tower's last lines and no bubble; "open chat ▶" shows the whole thread.

The board reads as a group chat, in the previews and the full board alike.
Each message sits in a bubble that fits its text, from 24 columns up to 85% of the
conversation's width with at most 100 columns of text, or, on the full
board in a narrow pane where the thread list sits above, its whole width; the
author's name
is above it in their role's colour (any repo's lead in the accent, devs teal, reviewers
lilac, testers pink) with a dim role word ("dev-2 · developer", "qa · tester", "senior ·
reviewer", "<lead> · lead", or "<repo> lead" for another repo's lead). A message's tags
and age are not in its text: the first message of a run has them on the name
line ("cat-lead · lead · needs you · 9m"), so every text line takes the bubble's full
width. Your own messages are on the right, as "you (manager)" in red. One person's run of
messages is one bubble; each later message in it has its tags and age on a dim line of
their own above its text, which spaces the messages. A URL in a message starts on a line
of its own, whole, in the accent and underlined; wider than the bubble, it breaks only
after `/`, `?`, `&` or `-`, the rest indented two columns, and the text after it starts a
line of its own too. In a terminal that supports OSC 8 hyperlinks, every piece of it is one
link to the whole URL, in a preview too where only its first line shows. A path (16
characters or more with two `/`) stays in its line and breaks after a `/` or a `-`. A long message shows its first four lines (two in a preview) and "read
more", which opens it in place, in a preview too; a message that would hide one line only
shows in full. A short ack ("copy", "ok", "plan approved", up to 40 characters, never one
mentioning `@manager`) is a "↳ qa: copy" reply under the bubble it answers, wrapping when
long. Tower's own messages are quiet centred lines, wrapped at 80 columns: nudges of one
worker in a row as "dev-1 was nudged 3 times", the watch's flags in plain words (`dev-1
left Rex while on #12`), a question tower asks you with its "needs you" tag, and a
delivery failure as "delivery failed to X · details". In the full board's conversation
(not the previews), separators mark where how long ago the messages were changes ("now",
"within the hour", "3h ago", "yesterday"). A question
for the owner (it starts `question:` and mentions `@manager`) has a red "needs you" tag
and a red edge and tint; a message starting `decision:` a cyan "decision" tag and one
starting `blocked:` (in any case) an amber "blocked" tag. A `blocked:` message that
mentions `@manager` keeps its amber tag and is for your awareness, not a question: only
`question:` needs you. Every `@manager` is in bold red,
in any case; one inside backticks, a fenced block or a quoted line (starting with `>`), or
written as code (`@manager.check()`), is no mention and is not marked. Selecting a task
shows only the threads linked to it, or "no thread for this task yet". Up and down move the
selection over repos, milestones and tasks, skipping labels and summary lines; a click on a
tree row or a card selects it. Each pane scrolls on its own with the mouse wheel over it;
PgUp and PgDn scroll the overview, or the tree when it is alone. In a narrow pane the
columns stack, and below about 61 columns the tree takes the whole width: Enter then opens
the selection's overview across the pane, and Esc goes back to the tree. Each Esc closes
something you can see; widening the pane closes the overview opened across it, since it then
shows beside the tree. The keys line at the bottom lists the keys that work, dropping whole
hints when the pane is narrow and always keeping `q quit`.

A repo, a milestone's task list and a Done group fold. Each line starts with ▾ while its
contents show and ▸ while they are folded away; either arrow, left or right, folds the
selected repo, milestone or Done group, or the selected task's milestone (a standalone
task's repo, a Done line's group), when it is unfolded (on a task or Done line, it selects
the folded row and closes the detail) and unfolds it when it is folded, and `z` folds every
active milestone, or unfolds them all when every one is folded already; repos and Done
groups keep their folds. A click on the mark folds or unfolds it. A folded repo keeps its
row, with its ⚠, ⓘ and "N blocked" markers, and its lead line, and shows one line in place
of its contents: "2 milestones · 3 standalone ✓1 Done", its active milestones, standalone
tasks and Done milestones, with "!1" in amber for its blocked tasks. A folded milestone
keeps its bar and percentage and shows one line in place of its tasks: "4 tasks ✓2 ●1 ○1",
with "!1" in amber for its blocked tasks (a zero count is left out). A click on a summary
line unfolds it. Up and down skip what is folded away. Picking something folded another way,
by its card or a line of a view, unfolds what hides it. Folds are kept on the board, so they
last across restarts and two dashboards on one board show the same; when what is selected is
folded from the other one, the folded row becomes the selection. Every
repo and milestone starts unfolded, and every Done group folded. The "No repo yet" group
does not fold.

Enter, or a second click on the selected row or card, opens the selection's detail over the
overview. A milestone's detail shows its goal, stage, lead, team, standing context, spec,
tracker link, its tasks with the first line of each description, and its message board; a
task's shows its id as the task's (`task #12`, never taken for its ticket), its milestone
and lead, who is on it and the team, its doing line, the blocked flag's reason in amber on
its own "blocked" row while it is blocked, what it is blocked by, its MR, standing context,
its ticket and tracker link, description, acceptance criteria and subtasks as checklists,
and its thread. The team is the assignees of the milestone's tasks and everyone who posted
in its threads, each in their role's colour, leaving out the lead, `manager`, `system` and
"you". Each detail has "◀ close (esc)" at the right of its first line. A click on a task row
in a milestone's detail opens that task's; Esc goes back one view at a time, to exactly
where you came from. Up, down or a click on another tree row closes the details and moves
the selection; a refresh that removes the item a detail shows closes the details too. Each
detail scrolls with the wheel and PgUp and PgDn. The milestone's message board and the
task's thread show as in the overview's summary.

Selecting a standalone task shows its task detail straight away, since there is no
milestone overview above it; in a narrow pane Enter opens it across the pane. A repo's
standalone task names "standalone, no milestone", its repo and the repo's lead, and shows
its thread. One in no repo says how to give it its repo ("tower task set 41 --repo R<n>")
and has no thread. Only on a board with no repos at all does it say "standalone, no lead
yet: tower task hand 41 M<n>": the CLI hands it to a milestone's lead; the dashboard does
not.

The controls ("open repo ▶", "open milestone ▶", "open task ▶", "open full board ▶" and
"◀ close") show their key, as in "(enter)", while there is room, and drop it, then go,
in a narrow pane; a click on one presses its key, so the two always do the same. Clicks
and the wheel need the terminal to pass the mouse to the dashboard.

A click on a thread's box (its "N earlier messages" line included; "read more" opens the
message in place instead), or `b` on the overview or a detail, opens the selection's repo's
whole message board across the pane, from a milestone or task too: the "Needs you" strip
across the top, then a list of its threads read like a chat app's conversations, grouped
OPEN and RESOLVED. An open thread takes two lines: its mark (a red ●
while a question waits on you in it, ○ open), its name in words and age, then where it
stands; what it is on is in its header.
A long name wraps to a second line, with the age on the first, and only past two ends in
"…" after the last piece that fits, never cut into a thread's own title, which keeps its
room there.
A blank line parts two open threads and comes
before RESOLVED, a blank row is under the "Needs you" strip, and one between the thread's
header and its conversation. RESOLVED is one row, "RESOLVED 3 ▸", folded each time the
board opens; right or a click on it unfolds it ("▾"), and left or a click folds it again.
Unfolded, each resolved thread is one line: a green ✓, its name, its age. A thread on a task
with more than one thread adds its own title, dim, after the task's ("Bookmarks page ·
Delivery failed"), here, in the open threads above and in the overview's
RESOLVED lines, so two never read alike; short of room the name is cut first. Up and
down stop on the RESOLVED row (the conversation keeps the thread shown last); left on a
resolved thread folds the group with its row selected. A board opened on a resolved thread
opens with the group unfolded, so the thread shown is always in the list. The list sits
beside the selected thread's header and whole conversation. In a pane narrower than about 69
columns it sits above them, one line a thread (its mark, name and age), taking its content's
height up to half the pane, so every open thread and the RESOLVED row show at once; the
header there leaves out its "where it stands" line. `b` opens on the selected task's
thread, else the first. The conversation opens at
the latest message; PgUp and PgDn or the wheel scroll back through it, staying on the
message you are reading while new ones arrive, and the wheel over the list scrolls the list.
Up and down, or a click in the list, move between threads; Esc or a click on "◀ back" goes
back to where you came from. Right moves into the chat and selects its newest message
(outlined), and the keys line offers it ("→ read its messages") only on a thread with a
message to select, not one holding only tower's lines; then up and down move between
messages, right opens or folds the selected one ("read more" or "show less", a delivery
failure's or a "left Rex" line's details; Enter does too on a resolved thread) and left
goes back to the thread list. A click on "read more", "show less" or "details" does the same
for that message.

Under the conversation is the composer, where you post to an open thread as `manager`.
On an open thread typing always writes: the composer has the keys from the moment the
thread shows, and shows it with "manager" in the accent colour, an accent bar down the
input's left edge and the cursor in it. Every letter goes into the message, `q` and `c`
included; clicks in the conversation ("read more", "show less", "details") and scrolling
keep it so. Up and down still move between threads or messages. Esc goes back, keeping what
you wrote (each thread keeps its own draft): Esc, then `q`, to quit.
While there is a draft, left and right move the cursor in it, and typing, Backspace (the
character before the cursor) and a paste act there; with no draft they do what they do on
the board. Alt+Enter starts a new line, and a paste keeps its line breaks. The input grows
from one line to four as the draft wraps, the conversation giving up the rows; a longer
draft shows the lines around the cursor. Enter or a click on "Post" posts it: the
dashboard writes it itself, as `manager`, through the same checked write `tower post`
makes, on a thread of its own so the screen never waits; the keys line says "posting to
T3…", then how it went. On success the input is cleared and the message appears at once;
on a refusal the reason shows in red and the draft stays. Ctrl-C quits even while writing.
A resolved thread says "resolved threads are read-only" instead and takes no writing: its
keys are commands, and `q` quits.

There is no header line: the alert line is the first, with the clock at its end. It takes
one line, never more: "⚠ N need you · ⓘ N for
your awareness · newest: <author> in '<thread>' · <repo> · <age>" (the newest question's
author in their role colour and its thread in words; its text is in the inbox) with
"inbox (i) ▶" at the right; with nothing needing you it says "nothing needs you",
followed by the awareness count while there is one. Short of room, where the newest item
is goes first: its thread's name is cut, then left out, then its repo's is cut, then left
out once fewer than four columns of it fit; its author and age stay together. "new lead (n) ▶" goes next, and "inbox (i) ▶" stays
while the author and age fit beside it; then the item takes its place (a click anywhere on
the line opens the inbox), and when not even its author and age fit, the item goes whole
and the controls come back while they fit. The counts stay. Each `@manager` message counts once. A need-you
message is one that starts with `question:` and mentions `@manager`: the lead tags what
needs you. It stays until you clear it (`r` on it in the inbox) or its thread is resolved;
opening its thread does not clear it, as a question seen is not answered. Any other message
that mentions `@manager` is for your awareness (answers, reports, decisions, delivery
failures): it leaves the alert line once you open its thread, mark it read or its thread is
resolved. Opening a thread means the full board shows it: the one you pick, or the first
thread when `b` opens the board with none chosen. If that read fails (the board is locked,
say), it is tried again a couple of seconds later; the keys line says why it failed, then
that the items are read now.
Each repo row in the tree shows its board's counts as badges, "⚠1 ⓘ1". An
inbox item opens its repo's full board. `i`, or a click
on the alert line, opens the inbox across the pane: NEED YOU first, then FOR YOUR
AWARENESS, newest first (read items are not listed; they stay readable in their threads),
each item its author in their role colour, its
message on at most two lines, its line breaks and runs of spaces folded to one space and
the second line ending in "…" when it goes on (a delivery failure as "@manager delivery
failed to X"), and, dim at the right of its first line, its repo, its thread in words and
its age, in at most a third of the line: the thread's name is cut first, then the repo's,
and in a narrow pane that part goes whole so the message keeps its room. Up and down
select an item, bringing both its lines into view, and Enter, or a click on either line,
opens the item's thread on the full board, and Esc there comes back to the inbox; `r`
marks the selected item read, or on a need-you item clears it (the keys line says
"r clear" there).
Either way the item leaves the inbox and the next one takes the selection. The wheel, and
PgUp and PgDn, move the selection with the view: an item scrolled out of sight hands it to
the nearest item in view, so Enter and `r` never act on one you cannot see. When no item
shows whole, the selection goes to one whose first line shows, if one does, and PgUp and
PgDn always stop where one does. Where the wheel leaves no item's first line in view, as it
can on a short list, nothing is highlighted and Enter and `r` do nothing until the arrows or
the wheel bring one back; the inbox opens with its selected item's first line in view. An
empty inbox's keys line offers only "esc back". Opening a thread
on the full board marks its awareness items read and leaves its need-you items. Marking
read is the owner's alone, so there is no CLI command for it: the dashboard writes it
itself, off the screen's thread as a post, and refuses any message that does not mention
`@manager`. Esc, `i` or a click on "◀ back" leaves the inbox.

For a phone push on an `@manager` message, control-tower relies on Claude's own push
notification, which the lead sends when it posts `@manager`; `tower` itself sends none.
Modules on the roadmap (such as notifications) enhance this later.

    tower watch

`tower watch` spots stuck workers by fixed rules. Installed by brew it runs as a
background service (see "Install"), so it keeps going whether the dashboard or the
terminal is open or not: every 20 seconds it reads the board and the terminal's live
workers, logs one line per action to stderr, and stops cleanly on SIGTERM or Ctrl-C.

The terminal is Rex, reached through its one module (the terminal module). A look lists
every session on the local Rex server and every block in each, and asks each terminal block
for its program status, reading only the root record (a session's background shells and
subagents report as child records, so a worker waiting on a background job reads as what it
is). A block whose root record is Claude Code's, with the process that reported it, is
joined by that process id to `claude agents --json`: the worker's name is that Claude
session's name and its status the record's state (`idle`, `working`, `done`, `blocked` with
what it waits on). Blocks running no Claude, and Claude sessions Claude does not list, are
no one. Claude does not list a session still at its "trust this folder" dialog, so a worker
started in a folder Claude has not trusted is not seen (no need-you item for that dialog,
and orphaned after three looks if its task is in progress) until the folder is trusted:
start workers in a trusted folder; a state the watch does not know is unknown, never idle.
The look has a 15 second limit, and one call that fails (Rex not running, a block that
cannot be read, `claude` failing) skips the whole look, says so, and looks again 20 seconds
later: a Rex half read never reads as workers gone. The watch never starts a Rex server.
`TOWER_REX` names the rex executable, else `rex` in `$REX_BIN_DIR` (where Rex keeps it),
else `rex` on the PATH; `TOWER_CLAUDE` names `claude`. When rex cannot be found the log
says once what it tried and how to link it into Homebrew's bin.

A worker is linked to its live Claude session in Rex by name: **a task's assignee is the
Claude session name of the worker on it** (compared ignoring case; the lead names it with
`claude -n <name>`). The lead skill names each worker `<role>-<task>-<area>`, like
`dev-119-db`, `qa-119-help` or `senior-119-api`: the role first, so the board shows it in
its role's colour, then the task's id number and one short word for the part of the code.
The same name is its board author and its @mention handle, and Claude shows it by that
name too (in `claude agents`, and as "Message from @dev-119-db"). Only assigned tasks under a milestone with a lead, not Done, are
watched, since every flag is a message on that lead's board, posted by `system` in the
task's open thread (its newest), or in a new "Watch on #N" thread linked to it.

A task's **team** is its assignee and every name its threads carry: each author of a post
in them and each name a post @mentions (`@qa-2`), but a lead's or the owner's. A
mention inside backticks, on a quoted line (starting with `>`) or written as code does not
count. So a tester or reviewer is on the team from its first post, or from when the lead
names it on the thread, whichever comes first. A worker on several teams has its prompt
flagged on one: the task it is assigned to, else the one whose threads it last posted in,
else the one whose threads last mentioned it; the flag stays there until the worker leaves
the prompt or that team, so one prompt is one need-you item. A worker that has neither
posted on a task nor been named there is on no team, and is not seen; nor is the team of an
unassigned task.

- **Waiting on a prompt** (Rex shows any member of the team blocked: at a permission,
  question or credential prompt): a message starting `question: @manager` at once, naming
  that member and quoting what the prompt asks when Claude Code says ("It asks: approve
  Bash: ...", on one line, at most 200 characters), so it is a need-you item in the inbox
  and on the alert line; two members waiting are two items. When the member leaves the
  prompt, a plain note says "qa-2's prompt was answered."; the need-you item itself stays
  until the owner clears it or its thread is resolved, as every need-you item does. The
  signals below are the assignee's alone.
- **Says it is stuck** (its own `blocked:` post): nothing for the watch.
- **Stopped without reporting** (the assignee idle or done, no post for 5 min) and **silent
  while working** (task in progress, assignee working, no post for 30 min): the worker is
  nudged through the relay (Live delivery), "no post on #N for M min, post your status",
  and the nudge is recorded as a plain message, "nudged dev-1: ...", saying so if Rex
  refused delivery or "not confirmed by Rex" when it may have arrived (a nudge sent to
  another session is recorded as an item for the owner naming who got it), with the relay's
  line in the watch's log; with no post 2 minutes later, a message tells the lead. A task
  flagged blocked is never nudged: its worker already said it is stuck, and the lead owns
  it.
- **Orphaned** (task in progress, no live Claude session in Rex by its assignee's name for
  three looks in a row): a message to the lead, saying it is "gone from Rex" and ending
  with how to clear it ("if it was closed: tower task unassign N, or set the next
  worker"). An empty list of workers (Rex restarting, or no team running) orphans no one.

A post is any message in the task's threads but the watch's own; a task with none counts
from when the watch first saw it with that assignee, kept on the board so a restart does
not reset it. Claude does not keep session names unique, so several live sessions can share
a worker's name (names are compared ignoring case): they are read as one worker. A prompt
is raised when any of them is blocked, quoting the first that is, and the worker's status
is the busiest of theirs (blocked over working over unknown over idle or done), so a stale
twin never hides a prompt or draws a "stopped" nudge while the other works. Changing a
task's assignee ends the old assignee's nudge and orphan episodes and starts the clock
afresh; a prompt goes on while the old assignee is still on the team. Renaming a worker's
Claude session away from the assignee makes it look gone (orphaned after three looks), so
rename the assignee with it. Each flag is raised once, and again only after its episode
ends (the prompt answered or the member off the team, a post after the nudge, the worker
back, the task Done, or for a nudge or orphan flag, reassigned). If Rex cannot be read, the
watch skips that look and says so, rather than taking every worker for gone. `system`
messages show their author in slate grey, a colour no role or lead has; no one else can
post as `system`.

A board written by an older `tower` is upgraded by `tower upgrade` (or by any other
`tower` command that writes); until then `tower ui` says so instead of showing the tree.
Every Claude session's Stop hook runs `tower lead hook` (the control-tower plugin), which
opens the board to read and write it: while the installed `tower` is older than the board's
schema, each session shows a Stop hook error when its turn ends (it does not block the
session) until `brew upgrade control-tower`.

### Live delivery

The watch also delivers the board: every 5 seconds, on a thread of its own so a slow
terminal never holds a look, it hands out the posts made since it last did, by the CLI, the
watch itself or the dashboard alike. The posts for one recipient in one round are one send,
so a burst is one message. A post made while no watch runs is delivered when one starts, so
after a long outage old posts and mentions arrive late.

Each post reaches each recipient at most once. The board keeps a cursor (every recipient
has been offered every post up to it) and how far each recipient was sent beyond it,
recorded before each send. A round starts sends to workers for at most 10 seconds and
checks for a stop before every send; a send it starts has its own limit of 60 seconds, so
none is cut short. What a round does not get to (out of time, stopped, or Rex not
answering) stays after the cursor and goes in the next round, to the recipients that have
not had it. So a stop (SIGTERM, Ctrl-C) or an upgrade restart waits at most about 70
seconds and loses nothing. A crash or a kill during a
send loses that one send, and no one is told.

- **To the lead:** nothing: a lead hears of its board's posts through its own waiter and
  Stop hook (below, "How posts reach a lead").
- **To workers:** a post that @mentions a worker (as the team rule reads mentions), and the
  lead's posts in the thread of a task not done to that task's assignee (a done task's
  worker has finished and may be closed already; @mention it to reach it), reach the
  worker's Claude session as a message from `@control-tower`, through the relay (below),
  every control character but newline stripped first. Only to a worker the look finds in
  Rex by that name (its Claude session name, compared ignoring case, so `@Senior` reaches
  senior). A name must start with a letter or digit. Never the post's author, a lead or the
  owner, and never a `system` post (the watch nudges workers itself), except tower's reply
  to an author below, which goes to that author. A name that is no worker in Rex counts as
  a failed send, so quote example names in backticks. Code is no mention: a name followed
  straight by `(` and an ASCII character or the end of its line (`@cart(x)`), or by `.` and
  an ASCII letter, digit or `_` (`@cart.isEmpty()`), while a sentence ending on a name
  (`@qa.`) still mentions it, and so does a name before `.` or `(` and any character that
  is not ASCII, such as Chinese or Japanese (`请@qa.谢谢`). While Rex cannot be read, posts
  for workers wait (said once in the log) and go when it answers. A lead receives the posts
  on its own board only, so a post naming another board's lead reaches no one there:
  tower replies on the post's thread to its author, naming that lead's board (`R<n>`) and
  how to post on it, a minute after the post, in the same one reply as any name in the post
  no worker has had. The owner gets no item.
- **To the owner:** unchanged, the `@manager` items.

**The relay.** tower cannot use Claude Code's cross-session messaging itself, so each send
to a worker (a post or a nudge) starts a headless Claude session named `control-tower`
(`claude -p -n control-tower --model haiku`) that sends the text with its SendMessage tool,
once, to the worker's Claude session by name; the worker sees "Message from
@control-tower". There is no typing into the worker's terminal. The relay has only the
SendMessage, ListAgents and ToolSearch tools, so a post's text can never get it to run a
command, no MCP servers, and no saved session. It gets its instruction and the text on
stdin, never in its arguments, the text between two marker lines carrying a nonce made for
that send (so a post quoting the markers still goes whole) and named as data to pass on,
never instructions. It loads the owner's plugins and hooks as any Claude session does, but
runs with `TOWER_RELAY=1`, so tower's own `tower lead hook` does nothing in it: a relay is
never attached as a lead, counts no lead's turn and is told of no posts, whatever a lead is
named. Each send is a paid model call (about $0.003 to $0.006, and 6 to 10 seconds, on
Haiku), and the text is copied by a model, so it may not arrive verbatim.

A worker at a prompt (any of its Claude sessions blocked in Rex, looked up afresh just
before the send) is sent nothing: the send fails "at a permission prompt (approve Bash:
...)", naming the kind and what it asks. How a send went is read from the relay's own
events, never from what the model says, and from a relay stopped at its 60 second limit the
events it printed until then are read too. A SendMessage to any other session is a
misroute: it always posts its own item for the owner naming who got it, whatever failure
episode is open, for a post, tower's reply to an author or a nudge, and is never resent.
Else a relay stopped at its limit may have arrived, so it is logged as unsure and never
resent; a SendMessage to the worker that its tool answered as sent is delivered; one with
no answer is unsure; one refused (as for a name no session has), or none at all, fails.
What the relay or its tool said goes into the reason escaped on one line, as in its log
line. A worker running with permissions bypassed holds a cross-session message for its own
user's approval (Claude Code's rule), which the relay cannot see: that send reads as
delivered.

Every send logs one line starting `relay to <name>:`: how the relay ended, how long it
took, its cost and turns, each SendMessage it made with the tool's answer and whether the
text went verbatim (and what went if not), the model's closing words, any stderr, and the
text tower asked it to send, so the relay can be checked in use.

A send to a name Rex has no worker for waits first, until its oldest post is a minute old:
a lead posts a worker's brief, then starts it, and Rex lists the new session a few seconds
later. Each round logs "1 post(s) for qa-7 wait: Rex has no agent named qa-7 yet; a worker
may be starting" and tries again, and the round Rex lists the name, the send goes, with no
item and no reply. Only that name's posts wait: every other recipient's go at once. A name
still unlisted after the minute is one of the failures below. A send made only of tower's
replies to an author never waits, since their author had started.

A name Rex has no worker for, that no worker on the board ever had (no task's assignee has
had it and nobody has posted under it, in any case), is a name the post only seemed to give:
an example like `@name`, a CSS rule like `@media`, or a worker's name misspelt. A lead
taken off the board with `tower lead remove` counts as no worker too, though it posted there:
a mention of it is its author's to fix. It raises no item for the owner. Instead
tower replies on the post's thread, as `system`, to the post's author: "@dev-2 no worker is
named `@name`, so your post reached no one by that name: ... If you meant a worker, check
the name; if you were quoting, put it in backticks." A send to such a name is judged one
post at a time, each once it is a minute old, so a post gets one reply naming every such
name in it, a minute and up to a round (5 seconds) after the post, whatever is posted after
it. The reply goes to a worker author like
any post, once; a lead reads it through its board waiter, and the owner, when the post was
theirs, finds it in the inbox for awareness. A reply that cannot reach its author (closed
since it posted) is only logged, never an item for the owner. A reply is never written for
tower's own posts, nor on a thread resolved meanwhile (the log says so).

Any other send that fails (a worker at a prompt, a worker the board knew that Rex no longer
has, a relay that sent nothing) opens a failure episode for that recipient and posts an
item for the owner's awareness (not need-you), a `system` message starting "@manager could
not deliver to" in the thread of the first post it carried (or a new "Delivery failed"
thread when that one is resolved). A round posts one item per board, naming each recipient
newly failing there and why, so a post naming several gone workers is one item. A recipient
Rex has no worker for that is the assignee of a task not done on that board has its line
end with how to clear it ("if it was closed: tower task unassign N, or set the next
worker"). One Rex has no worker for that is the assignee of no task not done is the
owner's item only when a post in the send is less than an hour old; an older one is a closed
worker's, found by a watch catching up after a stop, and is only logged, opening no episode:
"could not deliver 2 post(s) to dev-24: Rex has no agent named dev-24, a worker with no task
not done and no post to it in the last hour; no item for the owner". A worker Rex lists is
sent its posts however old they are. While an episode is open, further failures to that
recipient are only logged; a send that gets through ends it, and one still failing after an
hour is told again with a new item. A send that may have arrived (the relay stopped at its
limit, or a SendMessage with no answer) is logged only, and never resent.

### How posts reach a lead

A lead hears of every post on its repo's board by anyone but itself (workers, the owner,
and `system`'s flags) as one quiet line each, not as a turn in its pane,
and an idle lead is never woken while nothing is posted:

- **The waiter:** `tower lead wait <R>` (or `<M>`) runs, silent, until a post on the board
  is unread, then prints one line, like "control-tower: 2 unread posts on the board of R1:
  read them with tower lead unread R1, then arm this waiter again.", and ends. It tells
  nothing and moves no cursor, and ends at once when posts are already unread, so a post
  that lands while no waiter runs is never skipped. The lead runs it with Claude Code's Bash
  `run_in_background`: a background command in a terminal session has no time limit, and
  its end wakes the lead, so the lead is woken only when there is something to read. One
  waiter runs per repo: it holds a lock file beside the board (`board.db.R1.stream`). A
  waiter that takes the lock marks the lead's turns ended so far as the watch's baseline, so
  a turn counted just before it held the lock never raises "Lead not listening". A
  waiter whose repo is removed (`tower repo remove`) deletes its lock file, prints a line
  saying not to arm it again and ends; one stopped by a newer one (below) says so and ends.
  Every end is exit 0.
- **Reading:** `tower lead unread <R>` prints one line per post the lead has not been told
  of, as `[T3 #12] dev-2: plan: read the bookmarks spec...`: the thread, its task, the
  author and the post's first 80 characters on one line, never the whole post, and counts
  them as told; nothing when there are none. Then, when a waiter for the repo
  already runs, one line saying so, so a lead woken by a waiter that a newer one stopped
  does not arm another. On each waiter's end the lead runs `unread`, handles what it
  prints and arms the waiter again in the same turn, without a word about it.
- **One cursor:** the board keeps, per lead, the last post it has been told of. A post is
  unread while it is on the lead's board, after that cursor and not the lead's own (by its
  name or session, ignoring case), and not the watch's own posts in a "Lead not listening"
  thread, which are about the lead, for the owner (the owner's reply there is unread like
  any post). No board transaction is held while a line is written, and the cursor moves only
  from where the teller read it, so `unread` and the Stop hook telling at once may name a
  post twice but never skip one. `tower thread show` reads a thread without moving it. A new
  lead starts the cursor at the newest post.
- **Newest waiter wins:** a waiter writes its process id in the lock file, and one started
  while another holds the lock stops that one (SIGTERM, then SIGKILL after 3 seconds) and
  takes over, so a re-arm always works, even over one orphaned by a Claude Code that went
  away without ending it. The stopped one's end wakes the lead too, so the lead arms a
  waiter only when the skill says to. It signals only a process whose argv0's file name is
  `tower` and whose arguments are exactly `lead wait <repo>`, or `lead stream <repo>` (an
  older form of the waiter), for this
  repo on this board (`ps`); a lock held by anything else is left alone and the new one
  refuses to start. One asked to stop (SIGTERM, SIGINT, SIGHUP) ends cleanly and empties its
  lock file, so it leaves no process id behind.
- **The Stop hook:** the control-tower Claude Code plugin runs `tower lead hook` when a
  session ends its turn. For the session a lead attached (`tower lead attach <R>`, run from
  the lead's own Bash: it reads `CLAUDE_CODE_SESSION_ID`, and refuses unless
  `claude agents --json`, or `TOWER_CLAUDE`, lists that session under the session name the
  lead registered, ignoring case), with posts unread, it blocks the end of the turn and
  names them, which counts as telling them, so it holds once per batch. It says so too when
  no waiter of the lead's is running, with how to arm one. A stop of that session that ends
  its turn with no waiter running is counted, for the watch's lead check ("Lead not
  listening"). A turn that ends on an API error (a rate limit, an overload, the output
  limit) runs no Stop hook: the plugin's StopFailure hook runs `tower lead hook` instead,
  which counts it the same way and never tells (Claude Code ignores its output, so the posts
  stay unread). A turn the user interrupts runs neither and is not counted. Any other
  session, and any session when the board does not exist, passes. It looks the session up
  on the board opened read only, waiting at most a second on a writer, and opens it to
  write only for a lead's session. Right after `brew upgrade`, until the watch upgrades the board (its
  next look), each session's Stop hook says the board needs `tower upgrade` (not blocking
  the session). `tower milestone show` says whether the lead is attached;
  `tower lead set --session` with a new session forgets it until it attaches again.
- **The SessionStart hook:** the plugin runs `tower lead hook` when a session
  starts, resumes, is cleared (`/clear`), compacts or forks, so a lead whose session
  restarts is attached again with no one running anything. A session whose name (Claude
  Code's `session_title`) is the session a lead registered, ignoring case, is attached by
  the checks `tower lead attach` makes (`claude agents --json`, or `TOWER_CLAUDE`, lists it
  under that name), and told at once what to do. After a resume or fork, it reads with
  `tower lead unread <R>` and arms `tower lead wait <R>` and the forge waiter itself; after
  a `/clear` while its board waiter runs (background commands outlive it), nothing is to arm
  again. After a compaction its conversation is a summary, which keeps the work but not the
  lead's rules, so it is handed them back in a few lines: what to re-read (the lead skill's
  Who you are table and Principles, by the plugin's `CLAUDE_PLUGIN_ROOT` when Claude Code
  gives it, the checkout's `SDLC.md` when it has one, each open milestone's standing
  context, its memory); its board now (the repo and its checkout, the open milestones, each
  task in progress, or blocked, with its assignee, live when `claude agents --json` lists a
  session of that name, and its PR, at most 10, the rest counted with the command that
  shows each place holding one; the live workers, everyone who posted on its open threads
  whom `claude agents --json` lists, the lead aside, at most 10, left out when there are
  none; and how many posts are unread, counted, never told); and that its waiters still
  run, or, with no board waiter running, how to arm them. After a fresh start, or a
  `/clear` with no waiter running, it has no conversation, and the lead skill is started
  only by the owner, so it arms its board waiter itself (never deaf) and asks you to run
  `/control-tower:lead <R>`, which arms the forge waiter too. On a start, resume or fork, while the lead's attached session is a
  different one that `claude agents --json` still lists (a fork, or a second `claude -n`
  with the name), it stays attached, and the new session is told it is not the lead's; a
  `/clear` and a compaction are the same process, so they always attach. Any other session,
  one with no name, and any session when the board does not exist, passes before `claude` is
  asked (each ask bounded to 5 s) or the board opened to write. A hook cannot stop a session
  from starting: a board that needs `tower upgrade`, or `claude` failing, prints nothing and
  goes to Claude Code's debug log.
- **A lead outside Rex:** a lead's session with no `REX_SESSION` (not run in a Rex tab) is
  attached all the same, by `tower lead attach` and by the SessionStart hook, and both tell
  it that the board's team runs in Rex and what the board does not see from outside it (the
  workers it starts there: their prompts, and delivery to them), to raise with you. The hook
  says so after the rest, a compaction's reply too. With `REX_SESSION` set,
  nothing more is said.
- **Lead not listening:** each look, the watch checks every registered lead, attached or
  not, asking no terminal, so the check runs whether or not the terminal answers; an
  attached one by its Claude session, as `claude agents --json` (or `TOWER_CLAUDE`) lists it
  by its `CLAUDE_CODE_SESSION_ID` (`busy` is working, `idle` is idle, any other status says
  nothing), asked once a look and only when a lead has no waiter running (a `claude` that
  hangs holds that look up to 30 seconds). The lead's hooks count, on the board, each turn
  its attached session ends with no waiter running: by its Stop hook (not a stop it holds
  for unread posts, which goes on) or, for a turn that ends on an API error, by its
  StopFailure hook; a turn the user interrupts is not counted. With no waiter running, it
  opens a repo-wide "Lead not listening" thread with one `system` message mentioning
  `@manager` (for your awareness), once per episode, naming how to arm a waiter and, for a
  lead nobody runs any more, `tower lead remove`. It does so for a lead with no attached
  session once the watch has seen it so for 5 minutes (the time a lead started from the
  dashboard takes to boot and arm its waiter; a restart of the watch starts the 5 minutes
  again), for an attached lead when Claude no longer lists the session ("its Claude session
  is gone"), or when it is idle having finished a turn with no waiter since the watch last
  saw its waiter running, or a waiter last took its lock, or the watch first saw it, idle or
  working, with none (the count moved). So a turn that starts and ends between two looks is
  counted, also right after a look that saw the waiter running, and the gap between a
  waiter's end and the lead re-arming never raises one: the end wakes the lead, which
  re-arms in that turn, and a turn ended with the waiter running is not counted. A session
  working, or with a status that says nothing, is not raised, and an empty list (likelier a
  glitch than every Claude session gone) changes nothing. A list that cannot be read skips
  those leads for the look, with one log line. When a waiter runs again, the watch resolves
  that thread, which takes the item off the alert line.

## The board

One SQLite file per machine, at `~/.local/state/control-tower/board.db`, created on first
use. Set `TOWER_DB` to use another file, for example a throwaway board:

    TOWER_DB=/tmp/try.db tower ui

A relative `TOWER_DB` names the file in the directory `tower` is started in, and is handed
on as that file's absolute path, so a lead the dashboard starts in another directory uses
the dashboard's board.

Only the brew install creates or upgrades the default board. A `tower` built from a
checkout (`cargo build`, `cargo run`) refuses to create it, or to upgrade it from an older
schema version, on every command that opens the board for writing, naming `TOWER_DB`: an
upgrade by a newer dev build would lock out the installed tower, the watch and every lead.
`milestone show`, `task show` and `thread show` open it that way too, so they are refused
on an older board as well; the dashboard's screen only reads it and is unchanged. At its
own schema version a dev build opens the board as before. The default board is the one in
`HOME` and the one in the account's own home (from the password database), so a run with a
temp `HOME` is still guarded. It is matched by its resolved path, so `TOWER_DB` naming it
is refused too: by any path once it exists, and before it exists through a link or a
relative path, but not through `..` or another letter case in the folders not created yet.
Give a dev build a test board in `TOWER_DB`.

## Install

Needs Rex, the terminal the team runs in, on macOS (Apple silicon). Releases are built for
Apple silicon only; on an Intel Mac `brew install` refuses with Homebrew's own message.

    brew install daderoode/tap/control-tower
    brew services start control-tower

`brew install` puts `tower` on your PATH. `brew services start` runs the stuck-worker
watch, `tower watch`, as a launchd service: it starts at login, restarts if it stops, finds
rex on the Homebrew PATH and logs to `$(brew --prefix)/var/log/control-tower.log`. No
terminal tab is needed for it. Rex keeps `rex` in `$REX_BIN_DIR`, not on the PATH, so the
log says once which path it tried: link it into Homebrew's bin
(`ln -s "$REX_BIN_DIR/rex" "$(brew --prefix)/bin/rex"`, with `REX_BIN_DIR` as a Rex tab has
it); the next look finds it. A watch run by hand takes its full path in `TOWER_REX`. Brew does
not restart the service on `brew upgrade`; the watch notices its binary changed and
restarts itself between looks, logging one line.

The lead skill is a Claude Code plugin in `claude-plugin/`, `control-tower`; this repo is
its marketplace, also `control-tower`. A lead starts it with `/control-tower:lead M1`.
`brew install` puts the marketplace in the formula's share directory, and the watch
registers it with Claude Code itself (brew's own install step cannot write `~/.claude`).
When Claude Code has no marketplace `control-tower`, the watch runs, once:

    claude plugin marketplace add "$(brew --prefix)/opt/control-tower/share/control-tower/claude"
    claude plugin install control-tower@control-tower

and logs one line. Claude Code keeps a local marketplace at the path it was added with,
here the `opt` link that every `brew upgrade` repoints, and loads the plugin in place from
it. So each upgrade updates the skill with no copy and no reinstall. A running lead keeps
the skill it loaded until it is restarted or runs `/reload-plugins`.

The service finds `claude` only on Homebrew's PATH, which has `$(brew --prefix)/bin`, where
the Claude Code cask links it. A `claude` installed elsewhere (the native installer's
`~/.local/bin`) is not on it, and the log says which path it tried: link it into Homebrew's
bin (`ln -s ~/.local/bin/claude "$(brew --prefix)/bin/claude"`) and run `brew services
restart control-tower`. A watch run by hand takes `claude`'s full path in `TOWER_CLAUDE`.
If `claude` fails (not logged in, say), the watch logs why once and tries again every 10
minutes until it succeeds.

A `control-tower` added from GitHub (`daderoode/control-tower`) is
moved to the brew path, keeping the plugin installed. Any other `control-tower`, such as a
source checkout, is left alone: the watch logs where it is and the two commands above, which
switch it to the brew one. If the plugin is installed but disabled, it stays disabled. `brew
uninstall` leaves the marketplace pointing at nothing (`claude plugin list` shows the plugin
failed to load); `claude plugin marketplace remove control-tower` clears it.

Every `tower` command, the dashboard and the watch use the board at
`~/.local/state/control-tower/board.db`; set `TOWER_DB` to another file in each of their
environments to use another board (see "The board" above).

### The dashboard's keys in Rex

The dashboard is `tower ui` in a Rex tab. Two keys bring it into view, as Rex actions that
brew installs in `$(brew --prefix)/opt/control-tower/share/control-tower/rex/control-tower.lua`,
with brew's prefix written in. Nothing loads that file until you do: add to
`~/.config/rex/init.lua`, with the keys you want, and run `rex config reload`:

    dofile("/opt/homebrew/opt/control-tower/share/control-tower/rex/control-tower.lua")
    rex.bind("<key>", "tower_space")
    rex.bind("<key>", "tower_popup")

(That is Apple silicon's prefix; `brew info control-tower` prints the line with yours.) Each action runs
`tower space` or `tower popup` with the session and block the key was pressed in; tower
reaches Rex through the rex linked into Homebrew's bin (above), since Rex runs the
commands with its own PATH.

- **`tower_space`, the dashboard's tab.** Pressed anywhere else, it shows the dashboard's
  tab, in whichever session it is (the key's own session first), and remembers the tab it
  was pressed in. With no dashboard yet, it opens one in a new tab of the key's session,
  labelled control-tower. Pressed in the dashboard's tab, it goes back to the tab it was
  last pressed in, if Rex still has it; if not, or if it was never pressed elsewhere,
  nothing moves and Rex's log says why. `q` in the dashboard's tab only closes the
  dashboard; it does not go back.
- **`tower_popup`, the popup.** Opens the dashboard over the tab the key was pressed in,
  85% of it each way, compact; `q`, or Esc at its top level, closes it, and the popup goes
  with it.

The dashboard's tab is found by asking Rex, never by its label: a `tower ui` in a tab of
its own names itself and its board to Rex in its program status, as
`control-tower-<8 hex digits>`, from a hash of the board's path (only when it runs in Rex,
where `REX_BLOCK` is set). A tab is the dashboard while the program that named it is still
the tab's foreground process and it names the key's own board: a `tower ui` on another
board (a try on a `TOWER_DB` of its own) is passed by. Pressed in any tab of a dashboard of
the key's board, if there are two, the key goes back. A dashboard started by a tower older
than these keys names nothing, so after upgrading close it and let the key open a new one.
A tab or session Rex cannot read while the key looks (one closing just then) is passed over
and Rex's log says so. The key fails, saying why in Rex's log and changing nothing on
screen, when Rex cannot list its sessions, the session or tab it was pressed in cannot be
read, Rex does not answer within 15 seconds, the back file cannot be written, or the new
dashboard's tab cannot be opened; the popup fails when the tab cannot be read or the
popup cannot be opened.
Where the key was pressed is kept in a file beside the board,
`~/.local/state/control-tower/board.space-back` (for a board `TOWER_DB` names, that file's
name with `.space-back` in place of `.db`), its folder made if need be. The keys move what the Rex app
shows only because a key was pressed in it, so Rex's Remote Control setting is never
needed and tower never asks for it.

herdr is no longer supported: a team in herdr is not seen, and its plugin is gone. If an
older control-tower linked that plugin, remove what it left: `herdr plugin unlink
control-tower`, then `rm -r "$(brew --prefix)/var/control-tower"`, delete any key binding
for its actions (`control-tower.space`, `control-tower.popup`) in herdr's config
(`~/.config/herdr/config.toml`), and, if you used herdr's `tower space`, the file it kept
beside the board, which nothing reads now:
`rm -f ~/.local/state/control-tower/board.space-from` (for a board `TOWER_DB` names, that
file's name with `.space-from` in place of `.db`).

### From a checkout (development)

Needs a Rust toolchain (edition 2024).

    cargo install --path .   # puts tower in ~/.cargo/bin
    tower watch              # in a terminal of its own, while you work on it

A `tower` built this way is not a brew install, so it registers nothing itself.
Stop the brew service (`brew services stop control-tower`) while a dev watch runs, so one
watch writes the board.

To try the lead skill from the checkout, start Claude with it:

    claude --plugin-dir ./claude-plugin

Both are named `control-tower`, so while the brew marketplace is registered, the checkout's
plugin replaces the installed one for that session only.

## Releasing

`.github/workflows/release.yml` publishes the binaries on the `daderoode/homebrew-tap` repo,
next to the formula that installs them:

1. Bump `version` in `Cargo.toml` and `claude-plugin/.claude-plugin/plugin.json` to the
   same new version, rename `## [Unreleased]`
   in `CHANGELOG.md` to `## [<version>] - <date>` with a new empty `## [Unreleased]` above
   it, and merge to master.
2. Pull master after the merge, tag that commit with a signed, annotated tag, and push it:
   `git tag -s v<version> -m "control-tower <version>" && git push origin v<version>`.

On the tag the workflow checks that the tag matches both versions, builds `tower` for
`aarch64-apple-darwin` only (no Intel build), creates the release
`control-tower-v<version>` on `daderoode/homebrew-tap` with
`tower-v<version>-<target>.tar.gz` attached (the binary and, under
`claude/`, the lead skill's marketplace: `.claude-plugin/` and `claude-plugin/`), and
commits `Formula/control-tower.rb` there, rendered from
`packaging/control-tower.rb.in` by `packaging/render-formula.sh`. Edit the template here,
never the formula in the tap. If a run fails partway, re-run it: an existing tap release
gets its assets replaced.

### One-time setup: the `HOMEBREW_TAP` secret

The workflow writes to the tap with a token in the repository secret `HOMEBREW_TAP`. Only
the two tap steps read it.

1. GitHub, Settings, Developer settings, Fine-grained personal access tokens, Generate new
   token.
2. Resource owner `daderoode`; Repository access: Only select repositories,
   `daderoode/homebrew-tap`.
3. Repository permissions: Contents, Read and write. Nothing else.
4. Store it: `gh secret set HOMEBREW_TAP --repo daderoode/control-tower` and paste the
   token.
