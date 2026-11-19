# Changelog

All notable changes to control-tower are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each version is a release tag
`v<version>` (README, Releasing).

Each PR adds its line under Unreleased. At release, Unreleased becomes the version, with the
tag's date, and a new empty Unreleased goes above it.

## [Unreleased]

### Changed

- **The owner signs off the spec, and sees it beside the lead.** The lead skill follows the
  front of the lifecycle: the owner grills the idea, runs `/to-spec` and signs off the spec,
  then runs `/to-tickets`, whose tickets in blocking order are the plan. The lead asks the
  owner to run both skills in its session instead of trying to run them itself, and when the
  spec is posted it opens a copy in Neovim in a pane to the right of its own (in herdr or
  Rex), or gives the spec's link elsewhere.

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
