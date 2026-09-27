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

On a terminal the listings come colored and aligned, times are relative
and long text wraps; `foam board` is the one-screen overview. A pipe gets
plain text, so does `--plain`, and `NO_COLOR` removes only the color.

Every write records the commit and branch it happened on. `memories` and
`prime` say how far `HEAD` has moved since a memory was written, or that it
came from a branch this one never merged.

## agents

`foam setup claude` adds one SessionStart hook to `.claude/settings.json`
that runs `foam prime --hook-json`. That is the whole integration; `foam`
generates no other files.

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
