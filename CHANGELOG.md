# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The screen shows the memories: Tab goes from the board to them, and
  BackTab goes the other way round. They are laid out as an issue's page,
  each memory a box with its slug and when it was remembered in the top
  border and where in the bottom one, and the about pane beside saying
  how far behind HEAD the chosen one stands. A memory `foam prime` would
  mark as old or from another branch says so in yellow in its border.
  `j` and `k` move between the boxes, and `/` matches the slug and the
  text.
- The screen renders a little markdown in the body and the notes:
  `backticked` code on a grey background, bulleted and numbered lists
  with a hanging indent, fenced code blocks as a panel with the language
  in its corner, and tables as a grid, with columns narrowed and cells
  wrapped when the box is too narrow, or one record per row when even
  that does not fit. The lines of a paragraph join and reflow; `foam
  show` prints the text as written. A fenced block tagged rust, python,
  sh, json, toml, yaml or diff has its comments dim, strings green,
  numbers magenta and keywords blue; any other tag stays plain.
- The screen's page has a cursor over the body and the notes. `j` and
  `k` move it and the chosen box has the cyan border; `u` goes to the
  note it answers, `[` and `]` to its neighbours in the thread, Space
  folds the replies under it, and Ctrl-d and Ctrl-u scroll without
  moving it.

### Fixed

- The screen's board scrolls so a section's top and bottom borders come
  into view with its first and last issue. The cursor never lands on a
  border row, so the rows scrolled only as far as the issue under it.

### Changed

- The screen's list and board each have the data ref's last commits in
  a pane across the lower third, where the board had them as its last
  section, and the list sits in a rounded pane of its own. The preview
  takes its half, or its side, of what is above. In the pane the times
  line up, the commit is magenta, the verb bold, each issue id cyan and
  whoever a commit names after `by` in their colour.
- The screen's header is three rows: the prefix as a badge and the three
  views with the one in use bright, HEAD at the right; the filter, or on
  a page the trail of ids opened from the view, with the counts at the
  right as badges run together into a powerline, each with a glyph for
  its status and one more for the memories; and a rule. The arrows are
  powerline glyphs, which need a Nerd or Powerline font.
- The screen says `claimed` where the commands say `in_progress`: in the
  columns, the badges, the filter and the board's section. The stored
  word and the commands keep `in_progress` until the schema next moves.
- The notes pane on the screen's page is titled `notes`, not `posts`, and
  the footer's Tab hint says the same.

## [0.5.0] - 2026-10-10

### Added

- Notes answer, change and go. `foam note <issue> <text> --reply-to
  <note>` answers a note, and `foam show` and the screen draw the thread,
  each reply under what it answers. `--edit <note> <text>` replaces a
  note's text and marks when; `--rm <note>` removes it, which `show` and
  the screen honour and `--json` records with `deleted_at`; `--history
  <note>` prints every text the note has had, oldest first, from the
  ref's history. A note is named by its id or any prefix of it that is
  unique on the issue. Replies are schema 3; a ref at 2 is taken there
  on the first write.

## [0.4.0] - 2026-10-10

### Added

- Schema migrations. A foam that reads a ref written at an older schema
  migrates it in memory, and its first write takes the ref to its own
  schema and says so; from then on an older foam refuses the ref. A ref
  newer than the foam, or older than the three schemas it migrates from,
  is refused with the foam version to use, and a sync refuses origin's
  ref when a newer foam wrote it, leaving the local ref where it was.
  `foam stats` shows the ref's schema beside the foam's, and `foam
  doctor` says when an upgrade is pending on the next write.

- Every note has an id: eight hex digits, random for a new note and a
  fixed hash of its fields for a note written before ids, so every clone
  gives an old note the same one. `foam show` prints it before the time,
  the screen shows it in a post's border, and `--json` carries it as
  `id` on each note. This is schema 2; a ref at schema 1 is migrated on
  read and taken to 2 on the first write.

### Changed

- When two clones changed meta.json, a sync takes the higher schema of
  the two instead of keeping its own.
- Two clones' notes merge by id, and the same note changed on both sides
  keeps the later change, rather than both copies.

## [0.3.0] - 2026-10-09

### Added

- A screen. `foam` with no command, in a terminal, opens what `foam
  list` prints as rows with a cursor and a preview of the chosen issue
  beside them. j and k move, Space folds a row's children, Enter opens
  the issue's page and Esc comes back, Tab switches to the board, where
  each section is a card, p hides the preview, and s, t, a, c and /
  filter by status, kind, your own claims, closed issues and text. A
  strip above the rows shows the branch, the counts and the filter. The
  page keeps the title and an about pane in view while the body and the
  notes scroll as a message board, one box per post; Tab moves onto the
  parent, children and blockers and Enter follows one. The screen sits
  behind the `tui` cargo feature, on by default; `cargo install
  --no-default-features` builds without it. Under a pipe or a hook,
  bare `foam` prints the usage as before.
- `foam stats` says how big the data ref has grown: its commits and
  age, the objects only the ref holds and their bytes on disk, what the
  tip holds, and the largest records. `--json` carries the same.
### Changed

- `foam close` warns about open children once, after the write, rather
  than once per retry while another clone is writing.

## [0.2.1] - 2026-10-09

### Changed

- "foam is not initialized here" now says that `foam init` fetches the
  data ref from origin when origin has one.
- `foam init` says when origin has no data ref yet, and when there is no
  remote, so sync is not set up.
- `foam sync` installs the pre-push hook and the refspec when they are
  missing, so a repo that added its remote after `foam init` needs no
  `--setup`. The flag remains and reports on the hook.
- `foam doctor` only reports a missing refspec once origin's data has
  been fetched.
- `foam sync` says on stderr what it is about to do before each round
  trip, so `git push` no longer goes quiet while the hook runs. It skips
  the push when origin already has the tip, and fetches without asking
  ls-remote first: two round trips fewer when nothing has changed. Under
  the hook every line starts with `foam:`; by hand, nothing to move
  prints "up to date".

- `foam setup claude --help` names both hooks it installs.
- Listings wrap a long title at the terminal width, with each later line
  under the title column, so the columns stay aligned. `--plain` output
  is unchanged.

### Fixed

- `foam init` no longer adds the fetch refspec before the remote has the
  data ref. Git fails every fetch whose configured refspec names a ref the
  remote lacks, so `git pull` broke in a clone initialized ahead of the
  first push. `foam sync` adds the refspec once it finds or pushes the ref.

## [0.2.0] - 2026-09-27

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
  bodies, notes and memories wrapped to the terminal width, a milestone's
  rollup in a column of its own before the title, and no commit stamps on
  an issue's created and closed lines. A pipe gets plain text,
  `--plain` gives that text on a terminal, `NO_COLOR` removes only the
  color, and `prime` always prints the plain form.
- `list`, `ready`, `blocked` and `search` draw an issue under its parent
  when both are listed, so a milestone's open children hang off it: with
  `├─` and `└─` on a terminal, and by indentation alone in a pipe, where
  the id stays the first field.
- When `foam prime` marks a memory as old or from another branch, it now
  says what to do: check it against the code, `foam remember` it again if
  it still holds, `foam forget` it if not.
- `foam doctor` reports a memory whose text is also in `CLAUDE.md`,
  `CLAUDE.local.md`, `.claude/CLAUDE.md` or `~/.claude/CLAUDE.md`, since
  both reach every session. Memories under sixteen characters are not
  matched, and `foam remember` refuses empty text.
- Wherever a command takes an issue id, a unique prefix of its hex part
  does too, with or without the `foam-` part, and so does a word from
  its title when exactly one open issue carries it. Anything that fits
  more than one issue prints them all and exits 1.
- `foam show` with no id, and the new `foam pick`, put the open and
  in-progress issues in fzf and take the chosen one; `pick` prints its id, for
  `foam claim $(foam pick)`. Both need fzf on PATH.
- `foam setup bash` prints a completion block for `.bashrc` that makes
  `foam show **<TAB>` search the issues in fzf and insert the id.
- `foam update` takes several ids and applies the same change to each,
  so `foam update a b c --parent <milestone>` moves three issues at once.
  With `--json` it prints the one issue as before, or an array for
  several.
- `foam update --parent` refuses a parent that is the issue itself or
  sits under it, and `foam doctor` reports a parent loop already in the
  data.
- `list` and `search` end a held issue's line with `waits on <ids>`, its
  open blockers, so a listing shows the whole graph: parents by nesting,
  blockers by suffix. `blocked` and `board` say `waits on` instead of `<-`.
- `foam board` puts the backlog on one screen: open milestones with their
  rollups, what is in progress and how much lease is left, what is ready,
  what is blocked and by what, and the last few log entries. `--json` gives
  the same five lists.

### Changed

- `--json` output has a stated policy, in the README: within a minor
  release keys are only added and never change type; a removal or rename
  takes the minor while below 1.0 and is named here. Two shapes changed
  in this cycle: `show` gained `children`, and `search` became an object
  with `issues` and `memories`.
- The contract at the top of `foam prime` now says where work and facts go
  and where they do not: not a TODO comment, a plan file, a handoff note,
  or the harness's own todo list or memory. It tells the agent to create
  an issue for work it finds, to note decisions and dead ends, and to
  remember facts and never rules, saying so when a fact would serve better
  as a rule or an issue. The old-memory line under the memories section is
  shorter and points at the contract.
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
  with `[closed/total closed]` over the leaves of its tree, the issues at
  the bottom, so a milestone of milestones counts the work and not the
  containers. `show` puts the same count on its children heading and on
  each child that is a milestone.
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

[Unreleased]: https://github.com/jackroddy/foam/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/jackroddy/foam/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jackroddy/foam/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jackroddy/foam/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/jackroddy/foam/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/jackroddy/foam/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jackroddy/foam/releases/tag/v0.1.0
