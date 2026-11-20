# Changelog

All notable changes to control-tower are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each version is a release tag
`v<version>` (README, Releasing).

Each PR adds its line under Unreleased. At release, Unreleased becomes the version, with the
tag's date, and a new empty Unreleased goes above it.

## [Unreleased]

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
- **Where a thread stands keeps tower's `question:`.** When the thread's newest word is
  tower's question for you, such as a worker waiting on a herdr prompt, its line on the
  thread's header (on the message board and the full board) and in the thread list keeps
  its leading `question:`, as the UI design does, where it had dropped it; the chat's line
  still drops it after its "needs you" tag (#68).

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
