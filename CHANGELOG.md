# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
