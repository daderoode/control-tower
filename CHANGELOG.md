# Changelog

All notable changes to control-tower are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each version is a release tag
`v<version>` (README, Releasing).

Each PR adds its line under Unreleased. At release, Unreleased becomes the version, with the
tag's date, and a new empty Unreleased goes above it.

## [Unreleased]

### Changed

- **A compacted lead is handed back its rules and its board.** After Claude Code compacts a
  lead's context, the SessionStart hook said nothing while its board waiter ran. It now
  says, in a few lines, what to re-read (the lead skill's Who you are table and Principles,
  by path, the repo's `SDLC.md`, each open milestone's standing context, its memory), the
  board's state (open milestones, tasks in progress (blocked ones marked) with their
  assignees, live or gone, and PRs, the live workers who posted on its open threads, and the
  unread count, never told) and that its waiters still run, or how to arm them (#150).

### Fixed

- **A task's forge ticket stays in its own repo.** `tower task set --link` refused nothing,
  so another repo's issue could be filed under this repo's milestone, recorded as this
  repo's work with its threads on this repo's board. A link to a GitHub or GitLab issue,
  merge request or pull request (on github.com, gitlab.com or the host of a repo's forge
  link on the board) is now refused when it is not in the project the task's repo link
  names, saying which repo it belongs to, when the repo has no forge link yet, and when the
  repo's link is a page past its project rather than the project's address. `tower task
  add` takes `--link`, checked before the task exists, so a refused link adds no task;
  `task set --repo` and `task hand` refuse a linked task into another repo the same way.
  `milestone set --repo` refuses a milestone whose linked tasks are not the new repo's.
  Other trackers' links, on any other host whatever their path, are taken as before, and
  so is a GitLab group's work item; the guard needs each repo's forge link. The lead skill
  files a task with its link in `task add` and says to file a repo's issue only under a
  milestone of that repo (#142).

- **A post naming another board's lead tells its author where to post.** A lead hears only
  its own board, so a post naming the lead of another repo reached no one, with no reply
  and no item. Its author now gets tower's reply on the post's thread, naming that lead's
  board and how to post there, a minute after the post and in the same one reply as any
  name in it no worker has had. A mention of the board's own lead is unchanged (#145).

## [0.6.3] - 2026-11-26

### Fixed

- **Workers leave the lead's Rex session its name.** Rex's start hook tells every Claude
  session to name its Rex session after its work, and a worker shares its lead's, so each
  worker relabelled the lead's whole session. The lead skill's worker recipes and every
  brief now say a worker never renames the Rex session: its tab is already named (#137).

- **A post naming two names no worker has had gets one reply naming both.** Since #133, a
  post naming two such names, one of them named in an older post too, got a reply for each,
  a minute apart. A send to such a name is now judged one post at a time, each once it is a
  minute old, so each post gets one reply naming every such name in it, a minute and up to a
  round after the post, whatever is posted after it. So a worker Rex lists only after the
  first post naming it was judged still gets the posts made since (#135).

## [0.6.2] - 2026-11-26

### Added

- **Keys for the dashboard in Rex: its own tab and a popup.** brew installs two Rex
  actions with its prefix written in; load them from `~/.config/rex/init.lua` with
  `dofile` and bind keys of your choice (README, The dashboard's keys in Rex).
  `tower_space` shows the dashboard's tab, opening one if there is none, and pressed in
  that tab goes back to where it was last pressed; `q` there only closes the dashboard.
  `tower_popup` opens the dashboard over your tab, 85% each way; `q` closes it. The tab is
  found by asking Rex: `tower ui` in a tab of its own names itself and its board in its
  program status, only in Rex, so a dashboard on another board is passed by. The keys move the Rex app only from your key press, so Remote Control is
  never needed (#85).

### Changed

- **Leads and workers go by one readable name.** A lead the dashboard starts (`n`) is named
  after its repo, `<repo>-lead` (`checkpoint-lead`; `m4-lead` in no repo yet), not the
  goal's first words plus " Lead", so `@checkpoint-lead` is read as one mention; a taken name
  gets `-m4` after it. The lead skill names workers `<role>-<task>-<area>` (`dev-119-db`),
  the same name as the task's assignee, the board author and the Claude session, shown in
  its role's colour. Leads and workers already on the board keep their names (#92).

### Fixed

- **A brief posted just before its worker starts is no false alarm.** A post to a name Rex
  did not list yet told the owner "could not deliver", or replied to the lead that no worker
  is named so, though the worker was live seconds later. A send to a name Rex does not list
  now waits until its oldest post is a minute old, holding back no other recipient's posts,
  and goes the round Rex lists the name; only one still unlisted after the minute is a
  failure (#133).

## [0.6.1] - 2026-11-25

### Fixed

- **A watch catching up after a stop no longer tells the owner about every closed worker.**
  The first watch after a long stop sent the owner one item naming every worker the old posts
  were for, all long closed (129 on 0.6.0's first start). A failure to a worker Rex no longer
  has is now the owner's item only while the worker is the assignee of a task not done or a
  post in the send is less than an hour old; an older one is logged only, opens no episode,
  and the post stays on the board. A send to a worker Rex lists goes however old the post
  is (#128).

## [0.6.0] - 2026-11-25

### Changed

- **The watch sees a team in Rex.** Each look reads every Rex session's terminal blocks and
  their program status (the root record only, so a worker on a background job reads as what
  it is), and names each Claude session it finds by `claude agents --json`; Rex blocks
  running no Claude and Claude sessions Claude does not list never show, and a state the
  watch does not know is unknown, never idle. A worker at a permission, question or
  credential prompt is a need-you item at once, quoting what the prompt asks; the stopped,
  silent and orphaned signals work as before, and an orphaned worker is "gone from Rex (no
  live Claude session by that name)". Claude sessions sharing a worker's name are one
  worker: a prompt on any of them is raised, and the busiest gives its status. A worker
  still at Claude's folder-trust dialog is not seen until the folder is trusted. A Rex
  that cannot be read, in whole or in part,
  skips the look and says so; the watch never starts a Rex server. `TOWER_REX` names rex,
  else `rex` in `REX_BIN_DIR`, else on the PATH; when it cannot be found the log says once
  what it tried and how to link it into Homebrew's bin (#110).
- **Posts and nudges reach workers in Rex, through a relay.** Each send starts a headless
  Claude session named `control-tower` (`claude -p`, on Haiku, with only the SendMessage,
  ListAgents and ToolSearch tools, no MCP servers and no saved session) that sends the
  text, once, to the worker's Claude session by name, as a message from `@control-tower`;
  nothing is typed into a worker's terminal. The text goes between marker lines carrying a
  nonce made for each send, so a post quoting them goes whole, and tower's own hooks do
  nothing in the relay (`TOWER_RELAY`), so it is never taken for a lead, whatever a lead is
  named. A worker at a prompt is sent nothing, and the send fails naming the prompt.
  Delivered means SendMessage answered as sent. A send to any other session always posts
  its own item for the owner naming who got it, for posts, tower's replies and nudges
  alike, even with a failure episode open or from a relay stopped at its limit, whose
  output until then is read too. A relay stopped at its 60 second limit, or a send with no
  answer, is logged as unsure and never resent, and a nudge that may have arrived is
  recorded as not confirmed, never as refused. A relay that sends nothing fails, what it
  said escaped on one line. Each send logs one `relay to <name>:` line: exit, duration,
  cost, turns, the tool's answer, whether the text went verbatim and what went if not, and
  the text. A round of delivery stops starting sends after 10 seconds but never cuts one
  short, so no second recipient's post is lost. Each send is a paid model call (about
  $0.003 to $0.006) and the text is copied by a model, so it may not arrive verbatim
  (#111).
- **"Lead not listening" asks no terminal.** The watch judges whether a lead's session is
  alive and working by `claude agents --json`, and whether it finished a turn without
  re-arming by the lead's hooks, which now count on the board each turn the lead's attached
  session ends with no waiter running: its Stop hook, and a new StopFailure hook for a turn
  that ends on an API error (a turn the user interrupts is not counted). A short turn
  between two looks is seen, and a waiter taking its lock resets what the watch compares
  with, so a lead that re-arms every turn is never raised. The check is right with the team
  in Rex and runs even when herdr does not answer. A lead Claude no longer lists is raised
  as "its Claude session is gone". A `claude agents --json` that cannot be read skips the
  check for that look with one log line. The board's schema moves to 19: an upgraded board
  keeps its "Lead not listening" episodes and raises nothing new.

- **The lead skill carries the principles of a lead that worked.** Twelve short rules, each
  with its reason and how a lead can tell it follows them: verify a claim before relaying
  it, targeted briefs for reviewers and testers, testing on what ships with a bug's baseline
  first, owner questions that stand alone, one release report shape, the steps run listed on
  every finish note and release report, every owner correction written down at once,
  production approvals given where the action runs, one fresh implementer per issue, cycle
  time measured, a protocol for shared test resources, and quiet by default. Optional
  standing roles (analyst, QA, deployer, reporter) may sit beside the per-issue workers. The
  forge waiter is now a script shipped beside the skill (`forge-wait`) instead of one the
  lead writes into the git directory, and it sees a waiter started from the old copy as
  already running (#84).
- **The lead skill starts and closes workers in Rex.** An implementer gets a worktree beside
  the repo and a tab of its own in the lead's Rex session, a tester or reviewer a block
  beside it, and a standing worker a tab in a folder beside the repo; each starts without
  focus as `claude -n <name>`, the name the watch and delivery find it by, and is closed by
  the ids its start printed. Workers start only in a folder Claude trusts, never under a
  live worker's name, and are checked to be listed once started. The herdr recipes stay
  for now. `tower lead attach` and the SessionStart hook, in a session with no
  `REX_SESSION`, still attach, and tell the lead that the board's team runs in Rex, what the
  board does not see from outside it (its workers' prompts, delivery), and to raise it with
  the owner.
- **The dashboard's `n` starts the lead in a new Rex session.** It opens a Rex session
  named after the lead on the repo's main checkout (or your home directory, for a milestone
  in no repo), without moving the Rex app's focus, running Claude with the dashboard's board
  as `TOWER_DB`, so the lead registers where you are looking. When Rex fails, the milestone
  and the lead it registered are removed again and the keys line says why. `TOWER_REX`
  names rex, else `rex` in `REX_BIN_DIR`, else on the PATH. A main checkout that is no
  longer a directory, or a rex that cannot be found, is refused before anything is written
  (#112).
- **The lead skill builds a UI change to a signed-off design and merges it only once its
  snapshots are approved.** A UI change is built to a prototype the owner signed off, an
  artifact linked from its issue with invented data only, and merges only once the owner
  has approved its new and changed snapshots in the PR, wherever the lead holds the merge;
  from then on the snapshots are the reference. The spec is in the repo, `docs/spec.md`
  (#122).

### Removed

- **herdr is no longer supported.** A team in herdr is no longer seen: the watch, delivery,
  the lead check, the dashboard's `n` and the lead skill work in Rex only, and the watch
  never runs `herdr`. The herdr plugin goes with it: `tower space`, `tower popup`, `q`
  returning to the workspace `tower space` was pressed in, the plugin's self-linking,
  `TOWER_HERDR` and `herdr-plugin.toml`; the formula installs no plugin files and needs
  Rex. `tower ui` in a Rex tab works as before, and the lead skill's Claude Code
  marketplace still registers itself. To clear what an older install left: `herdr plugin
  unlink control-tower`, `rm -r "$(brew --prefix)/var/control-tower"`, any key binding
  for `control-tower.space` or `control-tower.popup` in herdr's config, and the file
  `tower space` kept beside the board, `rm -f ~/.local/state/control-tower/board.space-from`
  (harmless; the formula's caveats say so too).

## [0.5.3] - 2026-11-24

### Changed

- **A message keeps its first line's indent.** A message or evidence whose first line was
  indented lost that indent, so a list posted on its own read out of line: the first item at
  the text's column, the rest indented. Every write now drops only leading blank lines (and,
  as before, a hidden whitespace character such as a form feed before the text) and trims the
  end, keeping the first line's indent, as the UI design's composer (v0.1.14) does.
  A tag after an indent (`   blocked:`) is plain text, as in the design: where it stands and
  the inbox no longer tone it once its spaces are folded, and it never makes a message
  blocked, a question or a decision (#16).

### Fixed

- **A word breaks after a hyphen between two Hebrew letters.** A word with a hyphen (`-`, a
  maqaf U+05BE, U+2010, an en dash and the like) between two Hebrew letters never broke
  there, so it moved whole to the next line or was cut where the line ended. It now breaks
  after the hyphen, as the UI design's does, also with a point (niqqud) on the letter or on
  the hyphen; a Hebrew letter's hyphen before a letter or digit that is not Hebrew still
  keeps them together. A hyphen that starts a word or follows a no-break space now keeps
  the letter after it when the hyphen or the no-break space carries a point, as it already
  did without one (#97).
- **A point after a space no longer vanishes.** A point (a combining mark, such as a
  qamats U+05B8) that followed a space was not drawn where a line broke at that space, so
  text the author wrote went missing; nor was one written right after a mention of the
  owner or a `blocked:` or `decision:` tag. It now starts the next line, as the UI design's
  does, and stays on the letter it follows, in every message, title, event and inbox row.
  Where every line breaks is unchanged (#102).
- **An indent as wide as a bubble's text hangs on a blank row.** A message line, or a line
  of a delivery failure's details, indented as wide as its text or wider lost the indent with
  no row for it, and a task's doing line could lose its first word to it. It now hangs on a
  blank row and the words start the next at the text's column, as the UI design (v0.1.14)
  draws it; a run of ideographic spaces starting the words hangs with the indent when the
  first word does not fit after it. The doing line breaks words as a bubble does (#103).

## [0.5.2] - 2026-11-23

### Fixed

- **A path in a message breaks after a `-` too.** A path in a message (16 characters or more
  with two `/`) broke only after a `/`, so a long piece such as `control-tower.log` moved
  whole to the next line. It now also breaks after a `-`, as the UI design's does (not before a
  `.` or a `/`, nor between a minus sign and a digit); a piece too wide for its line is still
  cut where the line ends. URLs keep their own breaks (#24).
- **A word that starts with an en dash keeps it with the letter after it.** A word starting
  with an en dash (U+2013) or a figure dash (U+2012) broke right after it before a letter;
  it no longer does, as the UI design's does not and as a hyphen (U+2010) already did not
  (#93).
- **A word breaks after a Garay hyphen inside it.** A word never broke after a Garay hyphen
  (U+10D6E), so one too wide for its line was cut where the line ended. It now breaks after
  it as after a hyphen (U+2010), and not between an ideograph and it, as the UI design's does
  (#95).

## [0.5.1] - 2026-11-21

### Fixed

- **Enter and `r` never act on an inbox item out of view.** In an inbox list of four lines or
  fewer, the wheel could leave no item's first line in view (only section heads, "nothing",
  the gap between the sections, or an item's second line) while Enter still opened the
  selected item's thread and `r` marked it read. There nothing is highlighted now, the keys
  line leaves Enter and `r` out, and both do nothing until the arrows or the wheel bring an
  item back; the wheel still scrolls three lines. A short inbox also opens with its selected
  item in view, where it could open on its section heads alone (#61).
- **An inbox delivery failure wraps onto two lines.** An inbox row for a delivery failure,
  "@manager delivery failed to X", stayed one line cut at the row's edge. It now wraps at
  its words onto a second line and ends in "…" there when it goes on, as the UI design's
  row does and as every other inbox row does (#51).
- **A message that wraps takes the widest bubble.** A message whose text wraps had a
  bubble only as wide as its widest wrapped line, so it could be much narrower than the
  widest a bubble gets. It now takes that widest, on either side, as the UI design's
  does; a message that fits keeps its width, counting a line "read more" hides (#77).
- **A clamped title, goal or thread name ends after a whole piece.** A milestone's or
  task's title in the tree, a milestone's goal, or a thread's name (and its own title) in
  the thread list too long for its two lines was cut mid-word, as in "shed-lead-senior-r…".
  It now ends in "…" right after the last piece that fits, "shed-lead-senior-…", or in
  place of the line's last character when the line is full, as the UI design's clamp and
  the inbox row do. A tree title's second line now folds each run of spaces to one, as
  the design does (#50).
- **Where a thread stands keeps tower's `question:`.** When where a thread stands is
  tower's question for you (a thread holding only tower's words, such as a worker waiting on
  a herdr prompt), that line on the thread's header (on the message board and the full
  board) and in the thread list keeps its leading `question:`, as the UI design does, where
  it had dropped it; the chat's line still drops it after its "needs you" tag (#68).
- **A word breaks before an opening bracket after a punctuation mark.** In a message, a
  delivery failure's opened details, and a title, goal, event, ack or inbox row, a word
  with a `(`, `[`, `{` or `<` after a mark such as `!`, `.`, `,` or `)`, such as
  `f(x),(y)`, moved to the next line whole, or broke only where a line ended. It now breaks
  before the bracket where the UI design does; not after a letter, a digit, another
  opening bracket, or one of `$ ' / @ ^ _` and a backquote (#45).
- **An ideographic space after a space hangs at a message line's end.** In a message and a
  delivery failure's opened details, a run of full-width spaces (U+3000) starting a word
  after an ordinary space, where a line ended, started the next line. It now hangs past the
  line's end with the spaces before it, as the UI design does, so the next line starts with
  the word after it. Titles, goals, events, acks and inbox rows still carry it to the next
  line with its word, as the design does there (#57).
- **An ack's age after a word, or the last piece of one, too wide for a line starts the
  next line.** An ack ending in a word, or the last piece of one, too wide for its line,
  such as a long hash, drew its age after the word's last piece, as in `5d6e7f8 · 8m`. Its
  `· 8m` now starts the next line, under the text, as the UI design draws it; an ack whose
  last word, or the last piece of it after a `-`, fits keeps its age on its last line (#52).
- **An ideographic space ending a line no longer moves a centred or right-aligned line.**
  A full-width space (U+3000) that fit at the end of an event's or a delivery failure's
  line counted toward its width, so that line sat a column left of the others, and an ack
  under one of your messages ended two columns short of the bubble's edge. It now hangs
  past the line's end, as the UI design does, so the line sits where the text before it
  does (#62).
- **An ideographic space before a word stays with it in a title, event or ack.** In a
  title, goal, thread name, event, ack or inbox row, a run of full-width spaces (U+3000)
  starting a word after an ordinary space, where a line broke before the word, was left at
  the end of the line before, out of sight, or dropped from a tree title's second line.
  It now starts the next line with the word, as the UI design does; a run that is a word
  of its own goes with the word after it, and one after a word ending in U+3000 hangs past
  the line's end with that word's (#74).
- **A wrapped ack fills each line, as the UI design does.** A wrapped ack kept room for
  its ` · age` at the end of every line, so it broke earlier than the design's, as in
  `↳ qa: copy,` / `testing the` / `thing now · 8m`. Its lines now fill to the ack's width,
  `↳ qa: copy, testing` / `the thing now · 8m`, and a hash that fits a line is no longer
  cut. Its age follows the last word where it fits; where only the `·` fits, the `·` ends
  that line and the age starts the next, at the age's own width on the message board too,
  where it had kept room for the widest age. An ack's lines after its first now start where
  the first does, under the arrow, no longer two columns further in. Every age on the
  dashboard now goes by its minute, as the full board's chat did, so a message shows one
  age everywhere on the screen, where the alert line, the inbox and the full board's
  thread list could say a minute more than its bubble (#75).
- **A selected inbox item's first line stays in view.** In an inbox list of four lines or
  fewer, PgUp or PgDn across the gap between its sections could leave only the selected
  item's second line in view. Paging there now goes on to the nearest item's first line, and
  the selection to an item whose first line shows; the wheel still scrolls freely (#44).
- **Every run of a chain of ideographic spaces stays where the UI design draws it.** In a
  title, goal, thread name, event, ack or inbox row, words that are only full-width spaces
  (U+3000), one after another, lost their second and later runs where they started a
  line, as in `build` / `　` / `a1b2c3... ok`. Where the design carries those runs to the
  next line with the word after them, they now start that line too, `build` / `　` /
  `　 a1b2c3...` / `ok`, and a tree title's second line keeps them; where the design hangs
  them past the line's end, they still hang (#82).

## [0.5.0] - 2026-11-20

### Changed

- **The owner signs off the spec, and sees it beside the lead.** The lead skill follows the
  front of the lifecycle for a new goal or milestone: the owner grills the idea, runs
  `/to-spec` and signs off the spec, then runs `/to-tickets`, whose tickets in blocking
  order are the plan. A ready-for-agent issue is a ticket in its own right. The lead asks the
  owner to run both skills in its session instead of trying to run them itself, and when the
  spec is posted it opens a copy in Neovim in a pane to the right of its own (in herdr or
  Rex), or gives the spec's link elsewhere.

### Fixed

- **prefix+a brings an open dashboard into view.** With the dashboard already open in its
  own workspace, prefix+a left you where you were: herdr 0.9.5's `plugin pane focus` moves
  only the herdr server's focus, not what your terminal shows. `tower space` now also
  focuses the dashboard's workspace, as it already did when it opened one (#3).
- **A message line's indent stays in its bubble.** A line after a message's first that is
  written with leading spaces, such as an indented list item, keeps them on its first drawn
  line, as the UI design does. A message's first line starts at the text's column, since a
  posted message is trimmed, and so does a URL's own line.
- **A message line's runs of spaces stay in its bubble.** Text aligned in columns with
  several spaces between words keeps them, as the UI design does, where they had drawn as
  one. A line that wraps at a run of spaces drops it, so the next line starts with its word,
  and the spaces next to a URL still go (#15).
- **A delivery failure's details keep their spaces.** Opened, a failure's details keep each
  line's indent and its runs of spaces between words, as the UI design does, where they had
  drawn folded, the indent gone and each run as one space (#19).
- **A Needs-you strip row folds its spaces.** A question in the strip draws each run of
  spaces between words as one, as the UI design does, where it had kept them as written;
  its bubble still keeps them (#25).
- **A delivery failure's details sit in its centred box.** Opened, the details of a delivery
  failure or of the watch's "left herdr" flag are drawn left-aligned in a box centred under
  the failure's line, as wide as its widest line and at most 80 columns, as the UI design
  does, where they had started at the chat's left edge at its full width (#26).
- **A word too wide for its bubble starts on a line of its own.** In a message, and in a
  delivery failure's opened details, a word with no break of its own, such as a long hash,
  wider than any line broke right after the text before it, so the two shared a line. It
  now starts on the next line and breaks where each line ends, as the UI design does. A word
  with a `-` now breaks after it where the design does, so it starts where the line is; a
  path in a message still breaks after a `/` (#18).
- **The replying-to line, where a thread stands and an inbox row fold their spaces.** Each
  draws a message's line with each run of spaces between words as one, as the UI design
  does, where it had kept them as written; the message's bubble still keeps them (#27).
- **A delivery failure's line wraps in a narrow chat.** In a chat narrower than the line of
  a delivery failure or of the watch's "left herdr" flag, it wraps onto centred lines inside
  its box, as the UI design does, its "details" or "hide details" kept whole and moved to a
  line of its own when it does not fit; a click on any of its lines still opens or folds
  it. It had been cut at the chat's edge (#30).
- **An inbox row shows its message across its lines.** An item drew only its message's
  first line, cut to one row. It now draws the whole message as the UI design does: its
  lines join with a space, each run of spaces is one, and it wraps at its words onto a
  second line under the first, ending in "…" when the message goes on. Selecting an item
  brings both its lines into view (#33).
- **CJK text breaks between its characters.** In a message, and in a delivery failure's
  opened details, Chinese, Japanese and Korean text with no spaces moved to the next line
  whole, or broke only where a line ended. It now breaks between two characters, and
  between one and a word, where the UI design does, never before a closing mark such as
  `。` or after an opening one such as `「`; a run too wide for a line fills the line it
  starts on, and a path written inside it with no spaces still breaks only after a `/`.
  A word also breaks after a `…` as the design does (#20).
- **An @mention right after a letter that is not ASCII is a mention.** In Japanese or
  Chinese, where words have no spaces between them, an `@manager` or `@dev-2` written
  straight after a character, as in "見てから@managerに返事します", now reaches the owner or the
  worker and is drawn marked, as the UI design marks it. Only an `@` right after an ASCII
  letter or digit, as in an e-mail address, is still no mention (#41).
- **An @mention followed by `.` or `(` and a character that is not ASCII is a mention.**
  A name followed straight by `.` or `(` and any character that is not ASCII, such as
  Chinese or Japanese in "请@dev-2.谢谢" or "请@dev-2(明天)看一下", now reaches the worker or
  the owner and is drawn marked, as the UI design marks it. Code stays no mention: a name
  followed by `(` and an ASCII character or the end of its line, as in `@cart(x)`, or by
  `.` and an ASCII letter, digit or `_`, as in `@cart.isEmpty()` (#48).
- **Titles, goals, events and inbox rows break a long or hyphenated word as the design
  does.** A word too wide for its line, such as a link or a hash, broke right after the
  text before it; it now starts on a line of its own and breaks where each line ends, and
  in an event, an ack or a delivery failure's line what follows it starts the next line. A
  word with a `-`, such as a name like `senior-reviewer-2`, now breaks after it where the
  design does, where it had moved whole or broken anywhere; CJK text there breaks between
  its characters as in a message (#23).
- **Where a thread stands tones a leading `decision:`.** On the thread's header and in the
  thread list, a line that starts with `decision:`, from a message for your awareness (its
  mention of you on any of its lines), draws it bold in the info colour, as its bubble
  does, where it had drawn plain (#34).
- **A word breaks after a `?`.** In a message, a delivery failure's opened details, and a
  title, goal, event, ack or inbox row, a word with a `?` inside, such as `what?why` or
  `search?q=rye`, moved to the next line whole, or broke only where a line ended. It now
  breaks after the `?` where the UI design does, not before a mark a line cannot start
  with, such as `!` or `)`, and after the last `?` of a run (#37).
- **Where a thread stands folds the spaces in tower's word.** When the thread's newest word
  is tower's, such as a delivery failure with herdr's error text, its line on the thread's
  header and in the thread list draws each run of spaces as one, as the UI design does,
  where it had kept them as written (#35).
- **An ideographic space at a line's end no longer breaks the word before it.** In a
  message, a delivery failure's opened details, and a title, goal, event, ack or inbox row,
  a full-width space (U+3000) that ended a line counted toward its width, so the word before
  it broke early, and a run of them could start the next line. It now hangs past the line's
  end as in the UI design, so the word before it fits and the next line starts with the
  word after it (a run after an ordinary space aside, #57); within a line it keeps its
  width (#40).
- **A task's doing line tones a leading `decision:`.** In a task's detail, a doing post
  that starts with `decision:` and is for your awareness (its mention of you on any of its
  lines) draws it bold in the info colour, as its bubble and where its thread stands do,
  where it had drawn plain (#54).
- **Where a thread stands draws tower's word in the text colour.** When the thread's newest
  word is tower's, such as a delivery failure, its line on the thread's header (on the
  message board and the full board) and in the thread list draws it in the text colour, as
  the UI design does, where it had drawn dim; only "where it stands" stays dim (#60).

## [0.4.0] - 2026-11-16

### Added

- **Developed in the open.** control-tower's repository is public, and its CI (the quality
  gate, the secret scan and the macOS job) runs on GitHub's runners.
- **control-tower's first public version.** A taskboard for a team of Claude Code agents
  working in herdr: what the work is, how far along it is, and where the owner is needed.
  Repos each have one lead; milestones and tasks move through waiting, in progress and
  done, with subtasks, acceptance criteria, tracker links and a blocked flag; each repo has
  a message board of threads where the team and the owner talk. `tower ui` shows it all in
  its own herdr space or a popup: a tree of repos, milestones and tasks, their overviews
  and details, the full message board and an inbox of what needs the owner. A watch nudges
  quiet workers and flags stuck ones, the `tower` CLI is how leads and workers update the
  board, and a Claude Code plugin's lead skill runs a repo's lead from it. Installs with
  Homebrew on Apple silicon.

### Notes

- **Upgrading an existing board:** after `brew upgrade control-tower`, the watch restarts on
  the new version and upgrades its board to the new schema by itself on its next look;
  nothing to run.
- **Links:** URLs in messages are terminal hyperlinks (OSC 8). herdr 0.9.5 does not pass
  them through, so inside herdr a URL shows whole but is not clickable.
- **Alt+Enter:** it breaks a line in the composer. Some macOS terminals send it only with
  "Option as Meta" set.
- **Platform:** Apple silicon only.
