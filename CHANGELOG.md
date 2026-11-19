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
