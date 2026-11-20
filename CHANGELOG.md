# Changelog

All notable changes to control-tower are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each version is a release tag
`v<version>` (README, Releasing).

Each PR adds its line under Unreleased. At release, Unreleased becomes the version, with the
tag's date, and a new empty Unreleased goes above it.

## [Unreleased]

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
