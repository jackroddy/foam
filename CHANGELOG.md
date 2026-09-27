# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A README.
- `foam session-end` releases every claim the session holds and notes
  "released at session end" on each. `foam setup claude` installs it as a
  SessionEnd hook beside the SessionStart one.
- `foam show` lists an issue's children, and `--json` output carries them
  under `children`.
- `foam config` shows and sets `lease-minutes` and `stale-after`. Both live
  in `meta.json` on the data ref, so every clone shares them.
- `foam search` matches memory slugs and texts too, printed after the
  issues. Its `--json` output is now an object with `issues` and
  `memories`.
- When stdout is a terminal, `list`, `ready`, `blocked`, `search`, `show`,
  `log`, `memories` and `recall` print for a person: times relative to now,
  columns aligned and a kind column added, status and priority colored,
  bodies, notes and memories wrapped to the terminal width, and no commit
  stamps on an issue's created and closed lines. A pipe gets plain text,
  `--plain` gives that text on a terminal, `NO_COLOR` removes only the
  color, and `prime` always prints the plain form.
- `list`, `ready`, `blocked` and `search` draw an issue under its parent
  when both are listed, with `├─` and `└─`, so a milestone's open children
  hang off it.
- When `foam prime` marks a memory as old or from another branch, it now
  says what to do: check it against the code, `foam remember` it again if
  it still holds, `foam forget` it if not.
- `foam doctor` reports a memory whose text is also in `CLAUDE.md`,
  `CLAUDE.local.md` or `.claude/CLAUDE.md`, since both reach every session.
- Wherever a command takes an issue id, a unique prefix of its hex part
  does too, with or without the `foam-` part, and so does a word from
  its title when exactly one open issue carries it. Anything that fits
  more than one issue prints them all and exits 1.
- `foam show` with no id, and the new `foam pick`, put the listing in
  fzf and take the chosen issue; `pick` prints its id, for
  `foam claim $(foam pick)`. Both need fzf on PATH.
- `foam setup bash` prints a completion block for `.bashrc` that makes
  `foam show **<TAB>` search the issues in fzf and insert the id.
- `foam board` puts the backlog on one screen: open milestones with their
  rollups, what is in progress and how much lease is left, what is ready,
  what is blocked and by what, and the last few log entries. `--json` gives
  the same five lists.

### Changed

- The `epic` kind is now `milestone`: an issue whose children are its
  work. `--type epic` is still accepted, and records already written as
  `epic` load as milestones.
- `foam close` requires `--reason`, and records a resolution: done, or
  dropped with `--dropped`. `show` prints it on the closed line, listings
  tag a dropped issue, and `update --status closed` now points at `close`.
  `update --resolution` and `update --reason` correct a closed issue.
- Inside a Claude Code session the default actor is the git user with a
  session suffix, `Jack/1a2b3c4d`, so two sessions of one person hold
  separate claims. `--actor` and `$FOAM_ACTOR` still win. The SessionStart
  hook reads the session id from its stdin, and `prime` now says who it is
  acting as. If Claude Code is present but the session id is not, every
  command warns on stderr and `doctor` reports it.
- A body, title, note, memory or close reason may start with a dash.
- `foam remember` refuses a text over 512 bytes.
- The priorities have meanings: P0 blocks all other work, P1 this session,
  P2 soon, P3 when convenient, P4 someday. `create --help` and the prime
  cheat sheet say so.
- A milestone's line in `list`, `ready`, `blocked`, `search` and `prime` ends
  with `[closed/total closed]` over its children, and `show` puts the same
  count on its children heading.
- `foam prime` reopens issues whose lease has expired and says which,
  instead of asking the agent to run `foam reclaim`.
- `foam prime` keeps its memories section under 4 KB, dropping the least
  recently updated memories first and saying how many it dropped.

- A milestone is not ready while any of its children is open. `foam blocked`
  lists it with the children holding it.
- When `foam prime` cuts the ready list short, the heading gives how many
  it shows and how many are ready.

## [0.1.0] - 2026-09-26

### Added

- `foam init`, `create`, `show` and `list`. Issues are JSON files in a chain
  of commits on `refs/foam/data`, written with git plumbing and never checked
  out.
- `foam ready`, `blocked`, `dep add`, `dep rm`, `dep tree`, `update`, `close`
  and `reopen`. An issue is ready when it is open, past any deferral, and
  every issue it waits on is closed. Adding a dependency that would form a
  cycle is refused.
- `foam claim`, `unclaim`, `heartbeat` and `reclaim`. A claim marks an issue
  in progress under the actor's name with a 15 minute lease; `reclaim`
  reopens every issue whose lease has run out. The actor is `--actor`, then
  `$FOAM_ACTOR`, then git's `user.name`, then `$USER`.
- `foam note`, `search`, `remember`, `memories`, `recall` and `forget`.
  Every write records the commit and branch it happened on, and `memories`
  says how far `HEAD` has moved since each memory was written, or that it
  was written on a branch this one never merged.
- `foam prime` renders the context an agent needs at session start: the
  workflow, a command sheet, counts, the actor's in-progress issues, what is
  ready, and every memory with an age note when it is old or from another
  branch. `--hook-json` wraps it for a Claude Code SessionStart hook, and
  `foam setup claude` adds that hook to `.claude/settings.json` without
  touching anything else in the file. `init` no longer writes to the
  working tree at all; it prints the setup command instead.
- `foam sync` and `doctor`. When the repository has an `origin`, `init`
  adopts the data ref already there or creates one, adds a fetch refspec so
  every `git fetch` lands the remote's data on `refs/foam/origin`, and
  installs a pre-push hook that syncs before each `git push`. Any `foam`
  command merges what a fetch brought in before it runs; records both sides
  changed are merged field by field, with removals kept and the newer side
  winning a field both changed. `doctor` reports dangling references,
  expired leases, future timestamps and a missing refspec or hook.
- `foam log` shows the data ref's history, optionally only the entries that
  touched one id. Every command accepts `--json`.

[Unreleased]: https://github.com/jackroddy/foam/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jackroddy/foam/releases/tag/v0.1.0
