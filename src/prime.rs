use std::fmt::Write;

use jiff::Timestamp;

use crate::git::{Distance, Git};
use crate::graph;
use crate::model::Status;
use crate::render::{age, rollup_tag};
use crate::store::Db;

/// The most bytes the memories section may take.
const MEMORY_BUDGET: usize = 4096;

const CONTRACT: &str = "\
foam tracks this repository's issues and memories on a git ref; nothing is \
checked out and nothing here is a file to edit. Start with `foam ready` and \
`foam claim <id>` before working on an issue. Record progress with \
`foam note <id> <text>`, finish with `foam close <id> --reason <why>`, and \
`foam unclaim <id>` anything you stop working on. Store facts the next \
session needs with `foam remember <slug> <text>`; check `foam memories` \
before relying on one that is marked as old or from another branch.";

const CHEAT_SHEET: &str = "\
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
Priority: P0 blocks all other work, P1 this session, P2 soon, P3 when \
convenient, P4 someday.";

/// Render the session-start context for `actor`.
pub fn render(db: &Db, git: &Git, actor: &str, limit: usize, reclaimed: &[String]) -> String {
    let now = Timestamp::now();
    let mut out = String::new();
    out.push_str("# foam\n\n");
    out.push_str(CONTRACT);
    out.push_str("\n\n## Commands\n\n");
    out.push_str(CHEAT_SHEET);
    out.push('\n');

    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let stamp = git.head_stamp().ok();
    let (branch, commit) = stamp
        .as_ref()
        .map(|s| (s.branch.as_str(), s.commit.as_str()))
        .unwrap_or(("?", "?"));
    let _ = write!(
        out,
        "\n## Status\n\n{} open, {} in progress, {} deferred, {} closed; on {branch} at {commit}\nacting as {actor}\n",
        count(Status::Open),
        count(Status::InProgress),
        count(Status::Deferred),
        count(Status::Closed),
    );
    if !reclaimed.is_empty() {
        let _ = writeln!(
            out,
            "Reopened {} issue(s) whose lease had expired: {}",
            reclaimed.len(),
            reclaimed.join(" ")
        );
    }

    let mine: Vec<_> = db
        .issues
        .values()
        .filter(|i| i.status == Status::InProgress && i.assignee.as_deref() == Some(actor))
        .collect();
    if !mine.is_empty() {
        let _ = write!(out, "\n## In progress for {actor}\n\n");
        for i in mine {
            let lease = match i.lease_expires {
                Some(t) if t > now => {
                    let mins = t.duration_since(now).as_mins();
                    format!("lease {mins} min left")
                }
                _ => "lease expired".to_string(),
            };
            let _ = writeln!(out, "{}  P{}  {}  ({lease})", i.id, i.priority, i.title);
        }
    }

    let ready = graph::ready(db, now);
    if ready.len() > limit {
        let _ = write!(
            out,
            "\n## Ready ({limit} of {} shown; `foam ready` lists all)\n\n",
            ready.len()
        );
    } else {
        let _ = write!(out, "\n## Ready ({} total)\n\n", ready.len());
    }
    if ready.is_empty() {
        out.push_str("nothing is ready\n");
    }
    for i in ready.iter().take(limit) {
        let _ = writeln!(
            out,
            "{}  P{}  {}  {}{}",
            i.id,
            i.priority,
            i.kind,
            i.title,
            rollup_tag(db, i)
        );
    }

    if !db.memories.is_empty() {
        out.push_str("\n## Memories\n\n");
        let mut lines: Vec<(&str, Timestamp, String)> = db
            .memories
            .values()
            .map(|m| {
                let distance = git.distance(&m.stamp.commit);
                let note = match distance {
                    Distance::Behind(n) if n < db.meta.stale_after => String::new(),
                    _ => format!("  [{}]", age(&m.stamp, distance)),
                };
                let line = format!("{}: {}{note}\n", m.slug, m.text);
                (m.slug.as_str(), m.updated_at, line)
            })
            .collect();
        // the least recently updated memories are the ones cut
        lines.sort_by_key(|(_, updated, _)| std::cmp::Reverse(*updated));
        let marked = lines
            .iter()
            .filter(|(_, _, l)| l.contains("  [at "))
            .count();
        let mut used = 0;
        let mut kept: Vec<&(&str, Timestamp, String)> = Vec::new();
        for entry in &lines {
            if used + entry.2.len() > MEMORY_BUDGET && !kept.is_empty() {
                break;
            }
            used += entry.2.len();
            kept.push(entry);
        }
        let cut = lines.len() - kept.len();
        kept.sort_by_key(|(slug, _, _)| *slug);
        for (_, _, line) in kept {
            out.push_str(line);
        }
        if cut > 0 {
            let _ = writeln!(out, "{cut} more not shown; `foam memories` lists all");
        }
        if marked > 0 {
            let _ = writeln!(
                out,
                "{marked} marked [at ..] {} old or from another branch: check each against the code, \
                 `foam remember` it again if it still holds, `foam forget <slug>` if not.",
                if marked == 1 { "is" } else { "are" }
            );
        }
    }
    out
}

/// Wrap `context` the way Claude Code reads a SessionStart hook's output.
pub fn hook_json(context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    })
    .to_string()
}
