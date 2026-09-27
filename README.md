# foam

> **<u>f</u>ocused & <u>o</u>rganized <u>a</u>gent <u>m</u>emory**

`foam` helps organize and persist the things your agent should remember

## about

The handoff between coding agent sessions can be messy. When a fresh agent
session is started up in an ongoing project, the context window is empty and
three things need to find their way into it: **rules**, **memories**, and
**plans**. Rules are things that a human decides about the way a project runs
(even if they were *proposed* by an agent). Memories are things an agent has
discovered about the project. Plans are descriptions and reports of work:
ideas, goals, steps taken, etc.

Rules tend to be pretty well managed by your instruction files (`CLAUDE.md`,
`AGENTS.md`); they naturally settle as a permanent fixture, and they often
scope to an entire project. On the other hand, memories and plans are
(probably) sound when they are written, but they can drift *aggressively* as
the code and project change. Agents will place memories and plans in whatever
way the harness (or your instructions) tells them to. If you're not intentional
about it, it's pretty easy to end up with a `CLAUDE.md` that reads like a fever
dream, dozens of `PLAN-FOR-<X>.md` files, and memories in the harness that
you've never actually read.

`foam` gives your agent's memories and plans a structured place to live.
Memories are kept in a store, and plans becomed tracked issues (like GitHub
issues). Your agent learns how to use the `foam` CLI, and `foam` keeps track of
everything as JSON in a git ref. This way, the memory store and issue tracking
live alongside your project, but with a separate history; everything `foam`
records is invisible to your working tree and branches, but it's still
versioned and accessible.

`foam` was inspired by [beads](https://github.com/gastownhall/beads). The same
core ideas are at play here, but `foam` does a lot less than `beads`, and it
won't scale nearly as well. If you are trying to track issues by the thousand
produced by swarms of agents, you probably want `beads`; if that sounds like
overkill, you probably want `foam`.

## quickstart

```
cargo install foam
cd your-repo
foam init
foam setup claude
```

`foam init` creates the ref (and sets up sync, if your repo has a remote).
`foam setup claude` adds two hooks to `.claude/settings.json`: one that
runs `foam prime` when a Claude Code session starts, and one that releases
the session's claims when it ends. That's the whole integration. From
there your agent figures out the rest on its own: it files issues, claims
them, notes what it learns, and closes what it finishes. You can check in
on it with `foam board` or `foam list`.

## documentation

### storage and sync

`refs/foam/data` is a chain of commits. Each commit's tree holds
`meta.json`, one `issues/<id>.json` per issue, and one
`memories/<slug>.json` per memory. Every `foam` command reads that tree
with git plumbing and writes a new commit on top of it. Nothing is ever
checked out, and a plain `git clone` won't fetch the ref. Every write also
records the commit and branch you were on at the time, which is how `foam`
knows later that a memory is fifty commits behind. If two `foam` commands
in the same clone race, the ref's compare-and-swap catches it and the loser
retries.

If your repo has a remote, `foam init` adds a fetch refspec so `git fetch`
lands the remote's data on `refs/foam/origin`, and installs a pre-push hook
that runs `foam sync` before every push. Any `foam` command merges whatever
a fetch brought in before it does its own work, so a clone is never behind
what it has fetched. `foam sync` does the fetch, merge, and push by hand,
and `foam sync --setup` adds the refspec and hook to a clone that was made
before `foam init` ran somewhere else.

When two clones change the same record, `foam` merges it field by field: a
removal on either side stands, notes interleave by time, and a field both
sides changed takes the newer value. Ids are random, so two clones creating
issues at the same time won't collide.

On a single machine none of this applies; everything reads and writes your
local `.git`.

### how humans use foam

Mostly you just look. Your agent files and closes the issues; you read the
board, open an issue now and then, and step in with `foam create`,
`foam update`, or `foam close` when the plan needs changing.

#### foam board

`foam board` puts everything on one screen, with milestones, what's in
progress, what's ready, what's blocked and by what, and the recent
history.

```
foam  on main at 9f25882  5 open, 1 in progress, 0 deferred, 2 closed

Milestones
foam-f168e4       P1  open  milestone  2/4  v0.2.0
  └─ foam-c8576c  P1  open  milestone  2/3  Terminal output

In progress
foam-dac906  P1  in_progress  task  Wire the parser  @ann  lease 14 min left

Ready
foam-4368f8  P2  open  task  One-screen board
foam-1ad1b3  P2  open  task  Review the README

Blocked
foam-f168e4       P1  open  milestone  2/4  v0.2.0  waits on foam-c8576c foam-1ad1b3
  └─ foam-c8576c  P1  open  milestone  2/3  Terminal output  waits on foam-4368f8
foam-a5def3       P2  open  task            Test the parser  waits on foam-dac906

Recent
4b3389f  2m ago   remember parser-lib
9536e8b  5m ago   note foam-dac906
e5abdcc  20m ago  claim foam-dac906 by ann
e21cab6  1h ago   close foam-b2e188
```

#### foam list

`foam list` shows the open issues as a tree. Children sit under their
milestones, a milestone's closed count gets its own column, and anything
that's waiting on something says so.

```
foam-f168e4         P1  open         milestone  2/4  v0.2.0
  ├─ foam-c8576c    P1  open         milestone  2/3  Terminal output
    └─ foam-4368f8  P2  open         task            One-screen board
  └─ foam-1ad1b3    P2  open         task            Review the README
foam-dac906         P1  in_progress  task            Wire the parser  @ann
foam-a5def3         P2  open         task            Test the parser  waits on foam-dac906
```

`--all` includes closed and deferred issues, and `--status`, `--type`,
`--label`, and `--assignee` narrow the list.

#### foam show

`foam show` prints one issue in full, with its fields, children, blockers,
body, and notes.

```
foam-dac906  Wire the parser
type: task  status: in_progress  priority: P1
assignee: ann  lease 14 min left
created: 3h ago

The grammar is in docs/grammar.md. Winnow, not nom: see the parser-lib memory.

[5m ago ann] PEG was a dead end: left recursion in the grammar
```

#### foam ready and foam blocked

`foam ready` lists what could be worked on right now, meaning open issues
with nothing blocking them and no deferral holding them. `foam blocked`
lists everything that's held back, along with what's holding it.

```
foam-4368f8  P2  open  task  One-screen board
foam-1ad1b3  P2  open  task  Review the README
```

#### foam memories

`foam memories` lists every memory with how far `HEAD` has moved since it
was written, and `foam recall <slug>` prints one.

```
parser-lib  winnow, not nom  (at 3d7c1a2, 1 commit ago)
```

#### short ids and the picker

Ids look like `foam-c4574c`, which is a pain to type. Any command that
takes an id also takes a unique prefix of the hex part (`foam show c45`),
or a word from the title when only one open issue has it
(`foam show parser`). If you have fzf installed, `foam show` on its own
opens a picker, and `foam pick` prints the chosen id so you can do
`foam claim $(foam pick)`. Put `eval "$(foam setup bash)"` in your
`.bashrc` after fzf's own line and `foam show **<TAB>` searches the issues
too.

#### terminal and pipe

On a terminal, output is colored, aligned, and wrapped, and times are
relative ("3h ago"). Through a pipe it's plain text with full timestamps
and no tree glyphs, so the first field of a line is always the id.
`--plain` gets you that form on a terminal, `NO_COLOR` turns off only the
color, and every command takes `--json`.

### the records foam keeps

`foam` keeps two kinds of record. Issues are work with a lifecycle, and
memories are facts. Everything above is a view of these two.

#### issues

An issue has a title, a kind, a priority from P0 to P4, a status, and a
body. The kinds are task, bug, feature, chore, milestone, spike, and
decision. The statuses are open, in progress, deferred, and closed. Notes
get appended over time, each stamped with its author and the commit it was
written at. A closed issue carries a reason, and whether the work was done
or dropped.

An issue can wait on other issues, in which case it stays out of `ready`
until they close. It can have a parent, which is how milestones are built.
Claiming an issue marks it in progress under your name (or your agent's)
for fifteen minutes by default. `foam heartbeat` extends the claim,
`foam unclaim` gives it back, and an expired claim gets released at the
next session start, so an agent that dies doesn't hold its issue forever.

```
foam create "Wire the parser" -p 1
foam create "Test the parser" --blocked-by <id>
foam claim <id>
foam note <id> "PEG was a dead end: left recursion in the grammar"
foam close <id> --reason "parser wired"
foam update <ids> --priority 0 --parent <milestone>
foam update <id> --defer-until 2026-10-01
foam dep add <id> <blocker>
```

#### milestones

A milestone is an issue whose children are its work. It stays out of
`ready` while anything below it is still open, and its rollup counts the
issues at the bottom of its tree, so a milestone of milestones shows how
much of the actual work is done. A release is a milestone titled `vX.Y.Z`
whose children are whatever ships in it.

```
foam create "v0.2.0" --type milestone -p 1
foam create "Terminal output" --type milestone --parent <release>
foam create "Color the status column" --parent <milestone>
```

#### memories

A memory is a fact the next session would otherwise have to rediscover, like
which library you picked, where a fixture lives, or what a flag turned out to
mean. It has a slug, up to 512 bytes of text, and the commit and branch it
was written at. `foam memories` and `foam prime` say how far `HEAD` has moved
since, or that the memory came from a branch this one never merged, and a
memory that's fallen behind gets a line in prime telling the agent to confirm
it or forget it.

```
foam remember parser-lib "winnow, not nom"
foam recall parser-lib
foam forget parser-lib
```

If you're not sure whether something is a memory, ask who's wrong if it turns
out to be false. If a person decided it, it's a rule and belongs in
`CLAUDE.md`. If the world moved on, it's a memory. If nothing is wrong yet
but something should change, it's an issue. A memory shouldn't repeat
`CLAUDE.md`, since both land in every session, and `foam doctor` will flag
one that does. Facts only ever move up. An agent remembers something, you see
it in prime a few sessions in a row, and you make it a rule or an issue.

### how agents use foam

Your agent never sees a terminal. It reads what `foam prime` prints at
session start, then runs the same commands you would through its shell
tool. This section is what `foam setup claude` wires up and what the agent
gets told.

The SessionStart hook runs `foam prime --hook-json`, which drops the text
below into the session's context. The SessionEnd hook runs
`foam session-end`, which releases any claims the session still holds.
Each Claude Code session acts under its own name (your git user plus a
session suffix), so two sessions of yours don't share claims; `--actor` or
`$FOAM_ACTOR` overrides that.

This is what a session reads first, taken from a small repo:

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

`foam config` sets the two numbers every clone shares. `lease-minutes` is
how long a claim holds, and `stale-after` is how many commits behind `HEAD`
a memory can be before prime marks it old.

### json

`--json` output is for scripts and agents, and its shape is kept stable on
purpose. Within a minor release, keys only get added, never removed or
renamed, and a value never changes type. Removing or renaming a key bumps
the minor while the crate is below 1.0 (and the major after), and the
changelog names the command and the key. There's no version key in the
output; `foam --version` tells you which shape you have.

### building from source

You need git 2.38 or newer and a Rust toolchain.

```
git clone https://github.com/jackroddy/foam
cd foam
cargo install --path .        # installs foam into ~/.cargo/bin
cargo build --release         # or just build it: target/release/foam
cargo test                    # unit tests and the integration tests
```

## license

MIT or Apache-2.0, at your option.
