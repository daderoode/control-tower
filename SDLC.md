# How work is done

How work on control-tower is done, for leads, workers, the owner and other humans. This file
is the one source for the process and is changed by PR. It holds process only: history is in
git, domain rules in `docs/adr/` and GLOSSARY.md.

The repo's docs are enough to do quality work without the lead skill. The lead skill adds
the board and herdr for visibility, with the same steps, roles and gates. Under `/lead` a
Standard change's implementer is a herdr worker; a Light change can go to a subagent, with
no herdr worktree. Where the lead skill differs from this file, this file wins, as the skill
itself says. The lead skill's default lifecycle is the other copy of this lifecycle: the two
differ in who merges; the skill adds the board mechanics (the gate, threads, merge task). A
change to one is checked against the other.

Keep it light.

## Roles

| Role | Does |
|---|---|
| Owner | Signs off a new goal's or milestone's spec and tickets before building starts. Asks for releases (the lead may propose one); the lead tags. UI sign-off. Has given the lead authority to merge to master in this repo |
| Lead | Only coordinates: never implements, tests or reviews, whatever the size. Picks the path, commits, opens the PR, delivers: merges, or hands the PR to the named reviewer when a peer review is asked for |
| Dev | Implements; stages the change and reports. Only the lead commits and pushes |
| QA | Tests |
| Senior reviewer | Stage 1 review |
| External skeptical reviewer | Stage 2 review |

A human working alone is out of scope.

## Lifecycle

Before any building on a new goal or milestone there is a spec and a set of tickets, and the
owner signs off both. A ready-for-agent issue (a triaged bug, or a feature the owner
approved on its issue) is a ticket in its own right and needs no spec.
The owner grills the idea, runs `/to-spec` and signs off the spec, then runs `/to-tickets`.
Those tickets, in their blocking order, are the plan. Both skills are the owner's to run:
the lead asks the owner to run them in the session that holds the context. When the spec is
posted, open a copy of it for the owner in Neovim in a pane to the right of yours (in Rex or
herdr; otherwise give its link), and make any change they ask for on the issue.

Each step picks up work marked ready for it and marks it ready for the next, by reporting to
the lead, who starts the next step (under `/lead`, the board shows it). Testing and review
may start as soon as there is something to test ("I have done A and B, please test them while
I do C"); for a Standard change, that partial hand-off is a commit and a hand-off like any
other, and the dev waits for it as Build once says.

| Step | Who | Does | Next |
|---|---|---|---|
| Implement | Dev | Makes the change with its local checks (`cargo fmt --all --check`, `cargo check --all-targets`; never the gate or a release build, see Building and testing) and its own basic sanity checks, stages the change and reports | Lead commits, pushes and opens a draft PR (Standard), and CI runs the gate and builds the hand-off (Build once); Test |
| Test | QA | Runs the change (for a Standard change, the hand-off, see Build once) and judges it as a person would, against the spec and ticket: does it do what was asked, the way it was asked, with no obvious nonsense a test would not catch. CI runs the tests; the reviews go deeper | Stage 1 |
| Stage 1 review | Senior reviewer | Reviews the PR diff and its CI result: against the spec and ticket, and for bugs, code smells and issues; flags tests that pass whatever the code does (tautologies) | Stage 2 |
| Stage 2 review | External skeptical reviewer, on the PR (not local code) | Everything, with no stake in the work. Sized by the lead: one reviewer, a review team, or a tabletop | Delivery |
| Delivery | Lead | Takes the PR out of draft and squash-merges it with the branch deleted. If the issue, the PR or the lead's standing context (a milestone's or task's standing instructions under `/lead`) asks for a peer review, instead marks the PR ready and hands it to the named reviewer (usually the owner) | Done |

- **Read the whole item first:** before planning or coding, read the issue's description and
  every comment, oldest to newest; again before reviewing or delivering.
- **Draft PR:** for a Standard change, when Implement passes the lead commits (signed),
  pushes and opens a draft PR, so CI runs on that commit (CI runs only on pull requests and
  on master). It stays a draft through Stage 2; Delivery takes it out of draft. For a Light
  change the lead opens the PR ready (not a draft) after Test passes. Fixes are added as
  commits and squashed at merge.
- **Build once:** for a Standard change CI's build of the hand-off commit is the only one:
  CI runs the quality gate on it and builds its binary (Building and testing).
  - The dev stages the whole change with nothing left unstaged, so what it checked is the
    commit's. After any hand-off, the dev stages nothing more until the lead says the
    commit is made.
  - Just before staging a hand-off, the dev brings its unpushed branch up to
    `origin/master`, so what CI checks and builds is what the PR will merge:
    `git fetch origin && git merge --ff-only origin/master`, which keeps the staged change
    (a rebase refuses one). If the merge refuses because master changed a file the change
    also changes: `git stash`, the merge, `git stash pop`, then resolve that file, stage it
    and `git stash drop`.
  - After opening the draft, the lead checks the PR is mergeable and CI has started: GitHub
    runs no `pull_request` CI on a PR that conflicts.
  - Merging master into a PR after its hand-off is not a fix: the hand-off stays and QA's
    result stands; CI on the merged head is the check of the merged tree, and the lead shows
    the PR's diff against master is unchanged.
  - The hand-off is the hand-off commit and what CI builds from it, and names the commit
    and the PR:
    - a change to what `cargo build` compiles (`src/`, `Cargo.toml`, `Cargo.lock`): the
      `tower` binary, which CI's macOS job (`.github/workflows/macos.yml`) builds from the
      commit and uploads as the artifact `tower-<sha12>`, after the commit's first 12
      characters. QA downloads it, once that run has passed, into
      `<handoff>/<sha12>/tower`, a folder outside every worktree, where a later build cannot
      touch it:

          sha=$(git rev-parse <commit>); d=<handoff>/${sha:0:12}
          run=$(gh run list -R daderoode/control-tower -w macos.yml -c "$sha" \
            --json databaseId --jq '.[0].databaseId')
          gh run download "$run" -R daderoode/control-tower -n "tower-${sha:0:12}" -D "$d"
          tar -xzf "$d/tower.tar.gz" -C "$d" && rm "$d/tower.tar.gz"

      The run's summary gives the binary's sha256. A commit with no artifact (its run
      cancelled by a newer push, or the artifact past its 7 days) gets one by running that
      commit's run again, `gh run rerun "$run" -R daderoode/control-tower`, or QA tests
      the newer hand-off instead;
    - a change not in the binary (the lead skill, hooks, `herdr-plugin.toml`, docs): the
      commit alone. QA reads the changed files from it (or the PR), or installs them into
      the throwaway environment (Building and testing), never the owner's own install, with
      no build;
    - a change to both: both.
  - QA runs the hand-off: no build and no worktree of its own. In its throwaway
    environment it puts the hand-off folder first on PATH, so the hooks and the herdr
    plugin, which call `tower` by name, run the hand-off.
  - The dev's worktree stays until Test, Stage 1 and every fix re-check are closed: fixes
    are made in it.
  - Stage 1 reviews the PR diff and its CI result. If a finding can only be proven by
    running code, the reviewer says so and proves it with the hand-off binary, for that
    finding only. `cargo check` proves only that it compiles. A finding that needs a
    different build (another test, an instrumented binary) goes back to the dev as a CI
    round, a commit CI checks and builds: never the gate or a release build locally, and
    never in the dev's worktree.
  - A fix is a new commit and a new hand-off. A CI failure on a hand-off commit is a fix
    back to the dev.
- **Findings:** whoever finds a problem supplies the evidence (a reproduction where one
  exists). The dev finds the root cause and fixes it or answers it (for example, "not a bug,
  because..."). An answer stands when whoever found the problem accepts it.
  - A fix, in order: the dev fixes it in its worktree with its local checks, as in
    Implement. Once a PR exists, the lead commits and pushes the fix as a new hand-off
    (Build once). If the fix changes behaviour, it goes to Test. The step that found the
    problem then re-checks the fix only (Stage 2 re-checks it on the PR): if CI on the
    fix's commit and the re-check of the fix pass, it is done. Stage 1 does not repeat for a Stage 2
    finding.
  - A disputed finding: the lead decides, and the decision is labelled as the lead's on the
    issue. Anything about UI, releases or scope goes to the owner.
- **The approved head:** for a Standard change, Delivery merges only the PR head Stage 2
  passed (or a later head whose fixes were re-checked). A new push voids an approval that was
  not re-checked. For a Light change, Delivery merges the head that passed Test.
- **A hold is a draft:** a hold puts the PR back to draft at once; a hold that lives only in
  chat is not a control.
- **CI green** on the PR's head before Delivery: the "Quality gate", which branch protection
  requires, and "macOS tests and binary" when it ran on that head, which protection does not
  check (a head it skipped, changing no build input, needs only the gate).

## Sizing

The lead picks the path and writes it on the issue or PR. Anyone can raise it; nobody can
lower it. When a Light change is raised to Standard, its PR goes back to draft and the full
lifecycle applies.

| Path | When | Steps |
|---|---|---|
| Light | Changes no behaviour: docs other than agent instructions, comments, typos, release notes | Implement, Test (read the rendered result against the ticket), Delivery. Can go to a subagent |
| Standard | Changes what the program, the herdr plugin, the lead skill, the install or a release does, however small (lead-skill prose counts). Agent-instruction docs (CLAUDE.md, SDLC.md, skills) count as Standard | Every step |

## Work items

- A Standard change needs a GitHub issue; a Light change may go through without one.
- The owner or the lead may move an issue to `ready-for-agent`.
- **Release milestones:** the lead acts for the owner. An open issue in no release milestone
  goes into one when the lead triages it or picks it up; the lead orders the issues and
  moves them between milestones as the work demands. The owner can move anything.
- Done: the PR body says `Closes #N` when there is an issue; the lead removes any worktree
  and every hand-off folder of the PR's commits, and records the outcome (the PR link and
  what was verified) on the issue, or on the PR if there is no issue. Problems the owner's
  trial finds are new issues.
- **Loose ends get filed:** follow-on work (a deferred case, a "part 2", a TODO) becomes its
  own issue before the parent closes.
- **Record who decided:** an owner decision is quoted and dated on the issue; a lead decision
  is labelled as the lead's.

## Building and testing

- Quality gate (CI's: `.github/workflows/ci.yml` runs it on Linux, on every pull request
  and on master):

      cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test

- **The gate and the release build run in CI, whoever does the work**: they are
  not run locally. While working, the dev runs `cargo fmt --all --check` and
  `cargo check --all-targets`. CI's result
  on the pushed commit is the gate: `gh pr checks <pr> --watch`, and a failure's log with
  `gh run view <run-id> --log-failed`. QA and the reviewers never run the gate.
- **macOS-only tests:** the product ships only for macOS, but the gate runs on Linux. A test
  that depends on how macOS behaves (how it reports `current_exe`, what `ps` prints) is
  `#[cfg(target_os = "macos")]`, with the reason beside it, and goes in `tests/cli.rs`,
  whose tests the macOS job also runs.
- **A new or changed snapshot** (`tests/snapshots`) is written only by
  `UPDATE_SNAPSHOTS=1 cargo test`, so it comes from CI: when the tests fail and a snapshot
  comes out new or changed, the gate (which runs the UI tests again that way) uploads the
  whole `tests/snapshots` folder as that run renders it, as the artifact `snapshots-<sha12>`
  of the PR's head commit, with `snapshots-changed.txt` naming the new and changed files.
  The dev downloads it into a temp folder, never over the worktree (`gh run
  download <run-id> -n snapshots-<sha12> -D <temp folder>`), checks each listed file against
  the signed UI copy, copies the ones that pass into `tests/snapshots` and stages them: one
  push round however many snapshots a change touches. Each failing test's log still prints
  its rendering.

- New tests are up to the dev and reviewer; no blanket rule.
- Test data and examples are invented: never real hosts, projects, companies, customers,
  servers or people, and never text copied from a real board or another project.
- Testing that touches herdr runs against a throwaway `herdr --session <name> server` with
  every `HERDR_*` variable unset, a temp `TOWER_DB`, and no real `claude` reachable from its
  panes (a temp HOME and a fake `claude`); delete its folder under
  `~/.config/herdr/sessions/` afterwards. A herdr or board in real use is only ever read,
  never written.
- Anything started for a test (a herdr server, a background process) has a time limit and is
  stopped afterwards.
- A database schema change ships a forward migration (`src/store.rs` `MIGRATIONS`) and never
  resets the board.

## UI changes

- A UI change is a visible change to the dashboard's layout or elements. Rewording a label is
  not one.
- The UI is defined by frozen, signed-off files the owner keeps outside this repo, each
  superseding the one before only for its listed changes:
  - `control-tower UI golden v0.1.0.html`;
  - `control-tower UI v0.1.3.html`;
  - `control-tower UI v0.1.4.html` (the fold);
  - `control-tower UI v0.1.7.html`;
  - `control-tower UI v0.1.12.html` (the resolved threads folded);
  - `control-tower UI v0.1.13.html`.

  The spec kept with them records each file's sha256 and listed changes.
- A UI change gets a new versioned copy, signed off by the owner in their own herdr or an
  artifact, never from a chat mockup, before it is built. A release milestone's UI changes
  share one copy, named in its description; when the milestone needs it, the copy is amended
  as its next version (for example v0.1.13), frozen and signed off the same way: a signed
  copy is never edited in place.
- Where the UI files disagree with the spec's errata, the spec wins.

## Releases

- A release is a GitHub milestone. Its title is the version SemVer gives its planned
  content, plus a short theme ("v1.2.0: Faster search"); its description says what it
  carries, why that version, and its UI copy, if it has one. The lead renames it when the
  content changes what SemVer says.
- Scope is judgment, not a rule: whether a release waits for an issue, or the issue moves to
  the next milestone, is the lead's call, made in the owner's interest.
- The lead may propose a release; when the owner asks, the lead tags it with a signed
  `v<version>` tag, the version SemVer gives what actually shipped. Once the release
  workflow has published it, the lead closes its milestone.
- Release notes: one `CHANGELOG.md`; each PR that changes behaviour adds its line under
  `Unreleased`. A Light PR does not need one.
- Mechanics: README, Releasing.
