# foam

An issue tracker and agent memory that lives on a git ref.

## about

`foam` keeps a repository's issues, their blocker graph, notes and memories
as JSON in a chain of commits on `refs/foam/data`. It never checks anything
out or writes to the working tree, and a plain `git clone` never fetches it.
Coding agents call `foam` instead of keeping plan files in the repo, and
`foam prime` prints the current state for them at the start of a session.

It is a small take on the idea behind [beads](https://github.com/gastownhall/beads),
built for my own use and written mostly with Claude Code.

## install

```
cargo install --path .
```

Needs a git with `merge-tree --write-tree`, which arrived in 2.38; developed
against 2.55.

## use

```
foam init                          # creates refs/foam/data
foam create "Wire the parser" -p 1
foam create "Test the parser" --blocked-by <id>
foam ready                         # open, past any deferral, nothing blocking
foam claim <id>                    # in progress under your name, 15 minute lease
foam note <id> "tried PEG first"
foam close <id> --reason "shipped"
foam remember parser-lib "winnow, not nom"
foam prime                         # what an agent should read first
```

Every command takes `--json`. `foam --help` lists the rest: `blocked`,
`board`, `dep`, `update`, `search`, `memories`, `reclaim`, `log`, `doctor`.

An id can be shortened to any unique prefix of its hex part, `foam show
c45`, or replaced by a word from the title when one open issue has it,
`foam show parser`. With fzf on PATH, `foam show` alone opens a picker and
`foam pick` prints the chosen id, so `foam claim $(foam pick)` works; put
`eval "$(foam setup bash)"` in `.bashrc` after fzf's own integration and
`foam show **<TAB>` searches the issues too.

On a terminal the listings come colored and aligned, times are relative
and long text wraps; `foam board` is the one-screen overview. A pipe gets
plain text, so does `--plain`, and `NO_COLOR` removes only the color.

Every write records the commit and branch it happened on. `memories` and
`prime` say how far `HEAD` has moved since a memory was written, or that it
came from a branch this one never merged.

## json

`--json` output is for scripts and agents, and its shape is kept on the
same terms as the command line. Within a minor release, keys are only
added, never removed or renamed, and a value never changes type. Removing
or renaming a key takes the minor number while the crate is below 1.0 and
the major after, and the changelog names the command and the key under
Changed. There is no version key in the output; `foam --version` says
which shape you have.

## agents

`foam setup claude` adds a SessionStart hook to `.claude/settings.json`
that runs `foam prime --hook-json`, and a SessionEnd hook that releases the
session's claims. That is the whole integration; `foam` generates no other
files. What a session reads first, from a small repository:

```text
# foam

foam tracks this repository's issues and memories on a git ref; nothing is checked out and nothing here is a file to edit. It is the one place work and facts go: not a TODO comment, a plan file, a handoff note, a todo list in the harness, or the harness's own memory.

Work: `foam ready`, then `foam claim <id>` before touching code. When you find work that has no issue, including anything noticed on the way, `foam create` it and keep going. `foam note <id> <text>` when you decide something or hit a dead end, so the next session does not retry it. `foam close <id> --reason <why>` once it is done, and `foam unclaim <id>` anything you stop working on.

Memories are facts about the code or the world that a next session would otherwise rediscover: `foam remember <slug> <text>`. Remember facts, never rules; a rule is something a person decided and belongs in CLAUDE.md. When a fact would serve better as a rule or an issue, say so in your reply. A memory marked old or from another branch may no longer hold: check it against the code, `foam remember` it again if it still holds, `foam forget <slug>` if not.

## Commands

foam ready [--limit N]          issues that can be worked now
foam show <id>                  one issue in full, with notes, blockers and children
foam create <title> [--type T] [-p 0-4] [--blocked-by ID] [--parent ID]
foam claim <id> | unclaim <id> | heartbeat <id>
foam note <id> <text>           append a note
foam close <id> --reason <why> [--dropped]  | foam reopen <id>
foam dep add <id> <blocker>     make <id> wait on <blocker>
foam blocked                    what is waiting, and on what
foam search <query>             issue titles, bodies, notes; memories
foam remember <slug> <text> | memories | recall <slug> | forget <slug>
foam update <id> [--title ..] [--priority N] [--defer-until DATE]
Add --json to any command for machine-readable output.
Priority: P0 blocks all other work, P1 this session, P2 soon, P3 when convenient, P4 someday.

## Status

1 open, 0 in progress, 0 deferred, 1 closed; on main at a4b8722
acting as ann

## Ready (1 total)

s-c1f981  P2  task  Test the parser

## Memories

parser-lib: winnow, not nom
```

## rules, memories and issues

An agent carries three kinds of thing between sessions, and foam holds two.
A rule is an instruction a person wrote or approved: it lives in CLAUDE.md,
versioned with the code, changed by commit and review. A memory is an
observation an agent wrote, unreviewed, true until the world drifts: foam
stamps it with the commit it was written at and marks it old once HEAD has
moved on. An issue is an intention with a lifecycle. For a borderline item,
ask who is wrong if it is false. A person who set it: rule. The world moved:
memory. Nothing is wrong yet but something should change: issue. A memory
must not repeat CLAUDE.md, since both reach every session; `foam doctor`
reports one that does. A fact moves up, never down: an agent remembers it,
a person sees it in prime a few sessions running and makes it a rule or an
issue.

## sync

With a remote, `foam init` adds a fetch refspec so `git fetch` lands the
remote's data on `refs/foam/origin`, and installs a pre-push hook that runs
`foam sync` before each push. Any `foam` command merges what a fetch brought
in before it runs. When both sides changed one record, `foam` merges it
field by field: a removal on either side stands, notes interleave by time,
and a field both sides changed takes the newer side's value.

On a single machine none of that applies; every command reads and writes
the local `.git`.

## license

MIT or Apache-2.0, at your option.
