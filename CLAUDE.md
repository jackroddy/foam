# foam

A Rust CLI issue tracker and agent memory for one git repository. Issues,
their blocker graph, notes and memories live as JSON files in a chain of
commits on `refs/foam/data`, written with git plumbing and never checked
out. `foam prime` renders the agent's context from that data at session
start.

## Branches

All working changes go on `dev`. Do not open a feature branch unless I ask for
one.

`main` carries releases and nothing else. Push to it when cutting a release,
and leave it alone the rest of the time.

## Releases

foam follows [semantic versioning](https://semver.org/spec/v2.0.0.html). While
the crate is below 1.0, a breaking change takes the minor number. Tag every
release, annotated, as `vMAJOR.MINOR.PATCH`, on the commit that was published.

A release is a milestone titled `vX.Y.Z`, priority 1. Its children are the
milestones and issues that ship in it, plus two standing chores: review the
README against the binary, and cut the release (version in Cargo.toml, the
changelog's Unreleased section dated, an annotated tag, dev merged to main;
I push). The release is ready when every leaf below it is closed, and it is
closed with the reason "tagged vX.Y.Z at <commit>".

When the number changes, retitle the milestone: `foam update <id> --title
vX.Y.Z`. When issues slip, create the next release and move them:
`foam update <ids> --parent <next>`. When two planned releases merge, move
one's children into the other and close the empty one as dropped. Add the
next release's milestone as soon as something is known to belong to it.

## Formatting

`rustfmt` is the format. Run `cargo fmt` before committing.

## Changelog

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Anything that changes what someone using the tool sees goes under
`[Unreleased]` in the commit that changes it.

## Tests

Unit tests live inline in the module they cover, under `#[cfg(test)]`.
Integration tests in `tests/` run the built binary against a temporary git
repository. `tests/common/mod.rs` holds the repository, clone and actor
helpers.

## Sandbox

`cargo run --example sandbox <scenario>` builds an agent-usage scenario into
`.sandbox/<scenario>/` and leaves it there to look at, with the prime text
each simulated session saw and a `report.txt`. Anything learned there becomes
a test in `tests/cli.rs`; a scenario never grows an assertion. `all` runs
every scenario.

## Platforms

Anywhere git 2.38 or newer runs; `git merge-tree --write-tree` is the floor.
Developed on Linux.
