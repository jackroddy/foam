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
the crate is below 1.0, a breaking change takes the minor number.

Tag every release, annotated, as `vMAJOR.MINOR.PATCH`, on the commit that was
published.

## Formatting

`rustfmt` is the format. Run `cargo fmt` before committing.

## Changelog

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Anything that changes what someone using the tool sees goes under
`[Unreleased]` in the commit that changes it.

## Tests

Unit tests live inline in the module they cover, under `#[cfg(test)]`.
Integration tests in `tests/` run the built binary against a temporary git
repository.

## Platforms

Anywhere git 2.38 or newer runs; `git merge-tree --write-tree` is the floor.
Developed on Linux.
