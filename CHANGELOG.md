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
- **An @mention right after kana or an ideograph is a mention.** In Japanese or Chinese,
  where words have no spaces between them, an `@manager` or `@dev-2` written straight after
  a character, as in "見てから@managerに返事します", now reaches the owner or the worker and is
  drawn marked, as the UI design marks it. An `@` right after an ASCII letter or digit, as in
  an e-mail address, is still no mention (#41).

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
