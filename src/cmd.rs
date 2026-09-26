use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::git::{Distance, Git, Stamp};
use crate::graph;
use crate::model::{Issue, Memory, Note, Stamps, Status, check_slug, new_id, parse_when};
use crate::prime;
use crate::store::{Db, Snapshot, Store};
use crate::{Cli, Cmd, DepCmd, SetupCmd};

/// The global flags, split from the subcommand so both can move.
struct Options {
    directory: PathBuf,
    json: bool,
    actor: Option<String>,
}

pub fn run(cli: Cli) -> Result<()> {
    let store = Store::open(&cli.directory)?;
    let command = cli.command;
    let cli = Options {
        directory: cli.directory,
        json: cli.json,
        actor: cli.actor,
    };
    match command {
        Cmd::Init { prefix } => init(&store, prefix, &cli),
        Cmd::Create {
            title,
            kind,
            priority,
            labels,
            parent,
            body,
            blocked_by,
        } => {
            let stamp = store.git.head_stamp()?;
            let id = store.write("create", |db| {
                let id = fresh_id(db);
                for other in blocked_by.iter().chain(parent.iter()) {
                    if !db.issues.contains_key(other) {
                        bail!("no such issue: {other}");
                    }
                }
                let now = Timestamp::now();
                let issue = Issue {
                    id: id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    kind,
                    status: Status::Open,
                    priority,
                    labels: labels.clone(),
                    parent: parent.clone(),
                    blocked_by: blocked_by.clone(),
                    related: Vec::new(),
                    assignee: None,
                    lease_expires: None,
                    defer_until: None,
                    created_at: now,
                    updated_at: now,
                    closed_at: None,
                    close_reason: None,
                    notes: Vec::new(),
                    stamps: Stamps {
                        created: stamp.clone(),
                        closed: None,
                    },
                };
                db.issues.insert(id.clone(), issue);
                Ok(id)
            })?;
            let snap = load(&store)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snap.db.issues[&id])?);
            } else {
                println!("{id}");
            }
            Ok(())
        }
        Cmd::Show { id } => {
            let snap = load(&store)?;
            let issue = get(&snap.db, &id)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(issue)?);
            } else {
                print_issue(issue, &snap.db);
            }
            Ok(())
        }
        Cmd::List {
            status,
            kind,
            label,
            assignee,
            all,
        } => {
            let snap = load(&store)?;
            let mut issues: Vec<&Issue> = snap
                .db
                .issues
                .values()
                .filter(|i| match status {
                    Some(s) => i.status == s,
                    None => all || matches!(i.status, Status::Open | Status::InProgress),
                })
                .filter(|i| kind.is_none_or(|k| i.kind == k))
                .filter(|i| label.as_ref().is_none_or(|l| i.labels.contains(l)))
                .filter(|i| {
                    assignee
                        .as_ref()
                        .is_none_or(|a| i.assignee.as_ref() == Some(a))
                })
                .collect();
            issues.sort_by_key(|i| (i.priority, i.created_at));
            print_issues(&issues, cli.json)
        }
        Cmd::Ready { limit } => {
            let snap = load(&store)?;
            let mut issues = graph::ready(&snap.db, Timestamp::now());
            if let Some(n) = limit {
                issues.truncate(n);
            }
            print_issues(&issues, cli.json)
        }
        Cmd::Blocked => {
            let snap = load(&store)?;
            let blocked = graph::blocked(&snap.db, Timestamp::now());
            if cli.json {
                let rows: Vec<serde_json::Value> = blocked
                    .iter()
                    .map(|(i, by)| serde_json::json!({ "issue": i, "blocked_by": by }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                for (i, by) in blocked {
                    println!("{}  <- {}", line(i), by.join(" "));
                }
            }
            Ok(())
        }
        Cmd::Update {
            id,
            title,
            body,
            kind,
            priority,
            status,
            assignee,
            add_label,
            rm_label,
            parent,
            defer_until,
        } => {
            let stamp = store.git.head_stamp()?;
            let defer_until = defer_until
                .as_deref()
                .map(parse_when)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            store.write(&format!("update {id}"), |db| {
                if let Some(p) = parent.as_ref().filter(|p| !p.is_empty()) {
                    if !db.issues.contains_key(p) {
                        bail!("no such issue: {p}");
                    }
                    if *p == id {
                        bail!("an issue cannot be its own parent");
                    }
                }
                let issue = get_mut(db, &id)?;
                if let Some(t) = &title {
                    issue.title = t.clone();
                }
                if let Some(b) = &body {
                    issue.body = b.clone();
                }
                if let Some(k) = kind {
                    issue.kind = k;
                }
                if let Some(p) = priority {
                    issue.priority = p;
                }
                if let Some(a) = &assignee {
                    issue.assignee = Some(a.clone()).filter(|a| !a.is_empty());
                }
                for l in &add_label {
                    if !issue.labels.contains(l) {
                        issue.labels.push(l.clone());
                    }
                }
                issue.labels.retain(|l| !rm_label.contains(l));
                if let Some(p) = &parent {
                    issue.parent = Some(p.clone()).filter(|p| !p.is_empty());
                }
                if let Some(t) = defer_until {
                    issue.defer_until = Some(t);
                    issue.status = Status::Deferred;
                }
                match status {
                    Some(Status::Closed) => issue.close(None, &stamp),
                    Some(Status::Open) => issue.reopen(),
                    Some(s) => issue.status = s,
                    None => {}
                }
                issue.touch();
                Ok(())
            })?;
            show_after(&store, &id, &cli)
        }
        Cmd::Close { ids, reason } => {
            let stamp = store.git.head_stamp()?;
            store.write(&format!("close {}", ids.join(" ")), |db| {
                for id in &ids {
                    let issue = get_mut(db, id)?;
                    if issue.status == Status::Closed {
                        bail!("{id} is already closed");
                    }
                }
                for id in &ids {
                    let open_children: Vec<String> = db
                        .issues
                        .values()
                        .filter(|c| c.parent.as_deref() == Some(id) && c.status != Status::Closed)
                        .map(|c| c.id.clone())
                        .collect();
                    if !open_children.is_empty() {
                        eprintln!(
                            "foam: closing {id} with open children: {}",
                            open_children.join(" ")
                        );
                    }
                    get_mut(db, id)?.close(reason.clone(), &stamp);
                }
                Ok(())
            })?;
            for id in &ids {
                println!("closed {id}");
            }
            Ok(())
        }
        Cmd::Reopen { ids } => {
            store.write(&format!("reopen {}", ids.join(" ")), |db| {
                for id in &ids {
                    let issue = get_mut(db, id)?;
                    if issue.status != Status::Closed {
                        bail!("{id} is not closed");
                    }
                    issue.reopen();
                }
                Ok(())
            })?;
            for id in &ids {
                println!("reopened {id}");
            }
            Ok(())
        }
        Cmd::Claim { id, force } => {
            let actor = actor(&store, &cli);
            store.write(&format!("claim {id} by {actor}"), |db| {
                let issue = get_mut(db, &id)?;
                if issue.status == Status::Closed {
                    bail!("{id} is closed");
                }
                if !force && issue.held_by_other(&actor, Timestamp::now()) {
                    bail!(
                        "{id} is held by {} until {}",
                        issue.assignee.as_deref().unwrap_or("?"),
                        issue.lease_expires.unwrap()
                    );
                }
                issue.claim(&actor);
                Ok(())
            })?;
            show_after(&store, &id, &cli)
        }
        Cmd::Unclaim { id, force } => {
            let actor = actor(&store, &cli);
            store.write(&format!("unclaim {id}"), |db| {
                let issue = get_mut(db, &id)?;
                if issue.status != Status::InProgress {
                    bail!("{id} is not in progress");
                }
                if !force && issue.held_by_other(&actor, Timestamp::now()) {
                    bail!(
                        "{id} is held by {}; pass --force to release it",
                        issue.assignee.as_deref().unwrap_or("?")
                    );
                }
                issue.unclaim();
                Ok(())
            })?;
            show_after(&store, &id, &cli)
        }
        Cmd::Heartbeat { id } => {
            let actor = actor(&store, &cli);
            store.write(&format!("heartbeat {id}"), |db| {
                let issue = get_mut(db, &id)?;
                if issue.status != Status::InProgress || issue.assignee.as_deref() != Some(&*actor)
                {
                    bail!("{id} is not held by {actor}");
                }
                issue.claim(&actor);
                Ok(())
            })?;
            show_after(&store, &id, &cli)
        }
        Cmd::Reclaim => {
            let freed = store.write("reclaim", |db| {
                let now = Timestamp::now();
                let mut freed = Vec::new();
                for issue in db.issues.values_mut() {
                    if issue.status == Status::InProgress
                        && issue.lease_expires.is_none_or(|t| t <= now)
                    {
                        issue.unclaim();
                        freed.push(issue.id.clone());
                    }
                }
                Ok(freed)
            })?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&freed)?);
            } else {
                for id in freed {
                    println!("reopened {id}");
                }
            }
            Ok(())
        }
        Cmd::Note { id, text } => {
            let actor = actor(&store, &cli);
            let stamp = store.git.head_stamp()?;
            store.write(&format!("note {id}"), |db| {
                let issue = get_mut(db, &id)?;
                issue.notes.push(Note {
                    at: Timestamp::now(),
                    author: actor.clone(),
                    text: text.clone(),
                    commit: stamp.commit.clone(),
                    branch: stamp.branch.clone(),
                });
                issue.touch();
                Ok(())
            })?;
            show_after(&store, &id, &cli)
        }
        Cmd::Search { query } => {
            let snap = load(&store)?;
            let q = query.to_lowercase();
            let hit = |s: &str| s.to_lowercase().contains(&q);
            let mut issues: Vec<&Issue> = snap
                .db
                .issues
                .values()
                .filter(|i| hit(&i.title) || hit(&i.body) || i.notes.iter().any(|n| hit(&n.text)))
                .collect();
            issues.sort_by_key(|i| (i.status == Status::Closed, i.priority, i.created_at));
            print_issues(&issues, cli.json)
        }
        Cmd::Remember { slug, text } => {
            check_slug(&slug).map_err(anyhow::Error::msg)?;
            let stamp = store.git.head_stamp()?;
            store.write(&format!("remember {slug}"), |db| {
                let now = Timestamp::now();
                let created_at = db.memories.get(&slug).map_or(now, |m| m.created_at);
                db.memories.insert(
                    slug.clone(),
                    Memory {
                        slug: slug.clone(),
                        text: text.clone(),
                        created_at,
                        updated_at: now,
                        stamp: stamp.clone(),
                    },
                );
                Ok(())
            })?;
            println!("remembered {slug}");
            Ok(())
        }
        Cmd::Memories => {
            let snap = load(&store)?;
            let memories: Vec<&Memory> = snap.db.memories.values().collect();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&memories)?);
            } else {
                for m in memories {
                    println!("{}", memory_line(m, &store.git));
                }
            }
            Ok(())
        }
        Cmd::Recall { slug } => {
            let snap = load(&store)?;
            let m = snap
                .db
                .memories
                .get(&slug)
                .with_context(|| format!("no such memory: {slug}"))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(m)?);
            } else {
                println!("{}", m.text);
                println!("({})", age(&m.stamp, &store.git));
            }
            Ok(())
        }
        Cmd::Forget { slug } => {
            store.write(&format!("forget {slug}"), |db| {
                db.memories
                    .remove(&slug)
                    .with_context(|| format!("no such memory: {slug}"))?;
                Ok(())
            })?;
            println!("forgot {slug}");
            Ok(())
        }
        Cmd::Prime { hook_json, limit } => {
            let Some(snap) = store.load()? else {
                if hook_json {
                    // a hook must not fail a session in a repo
                    // that never ran foam init
                    return Ok(());
                }
                bail!("foam is not initialized here");
            };
            let actor = actor(&store, &cli);
            let text = prime::render(&snap.db, &store.git, &actor, limit);
            if hook_json {
                println!("{}", prime::hook_json(&text));
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Cmd::Setup { command } => match command {
            SetupCmd::Claude { remove } => setup_claude(&store, remove),
        },
        Cmd::Dep { command } => match command {
            DepCmd::Add { id, blocker } => {
                store.write(&format!("dep {id} <- {blocker}"), |db| {
                    if !db.issues.contains_key(&blocker) {
                        bail!("no such issue: {blocker}");
                    }
                    if graph::would_cycle(db, &id, &blocker) {
                        bail!("{id} waiting on {blocker} would form a cycle");
                    }
                    let issue = get_mut(db, &id)?;
                    if !issue.blocked_by.contains(&blocker) {
                        issue.blocked_by.push(blocker.clone());
                        issue.touch();
                    }
                    Ok(())
                })?;
                show_after(&store, &id, &cli)
            }
            DepCmd::Rm { id, blocker } => {
                store.write(&format!("undep {id} <- {blocker}"), |db| {
                    let issue = get_mut(db, &id)?;
                    let before = issue.blocked_by.len();
                    issue.blocked_by.retain(|b| *b != blocker);
                    if issue.blocked_by.len() == before {
                        bail!("{id} does not wait on {blocker}");
                    }
                    issue.touch();
                    Ok(())
                })?;
                show_after(&store, &id, &cli)
            }
            DepCmd::Tree { id } => {
                let snap = load(&store)?;
                get(&snap.db, &id)?;
                print!("{}", graph::tree(&snap.db, &id));
                Ok(())
            }
        },
    }
}

fn init(store: &Store, prefix: Option<String>, cli: &Options) -> Result<()> {
    if store.exists()? {
        bail!("foam is already initialized here");
    }
    let prefix = match prefix {
        Some(p) => p,
        None => {
            let dir = std::fs::canonicalize(&cli.directory)?;
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.to_lowercase()
                        .replace(|c: char| !c.is_alphanumeric(), "-")
                })
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "foam".to_string())
        }
    };
    store.init(&prefix)?;
    println!(
        "initialized {} with prefix {prefix}",
        crate::store::DATA_REF
    );
    println!("run `foam setup claude` to load context into Claude Code at session start");
    Ok(())
}

const HOOK_COMMAND: &str = "foam prime --hook-json";

/// Add or remove the SessionStart hook in `.claude/settings.json`,
/// leaving everything else in the file as it was.
fn setup_claude(store: &Store, remove: bool) -> Result<()> {
    let path = store.git.toplevel()?.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| path.display().to_string()),
    };
    if !settings.is_object() {
        bail!("{} does not hold a JSON object", path.display());
    }
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        bail!("{}: \"hooks\" is not an object", path.display());
    }
    let session_start = hooks
        .as_object_mut()
        .unwrap()
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    let Some(entries) = session_start.as_array_mut() else {
        bail!("{}: \"SessionStart\" is not an array", path.display());
    };
    let is_ours = |entry: &serde_json::Value| {
        entry["hooks"].as_array().is_some_and(|hs| {
            hs.iter()
                .any(|h| h["command"].as_str() == Some(HOOK_COMMAND))
        })
    };
    let present = entries.iter().any(is_ours);
    if remove {
        entries.retain(|e| !is_ours(e));
    } else if !present {
        entries.push(serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": HOOK_COMMAND }],
        }));
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(&settings)?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes)?;
    match (remove, present) {
        (true, true) => println!("removed the foam hook from {}", path.display()),
        (true, false) => println!("no foam hook in {}", path.display()),
        (false, true) => println!("the foam hook is already in {}", path.display()),
        (false, false) => println!("added the foam hook to {}", path.display()),
    }
    Ok(())
}

fn load(store: &Store) -> Result<Snapshot> {
    store.load()?.context("foam is not initialized here")
}

fn get<'a>(db: &'a Db, id: &str) -> Result<&'a Issue> {
    db.issues
        .get(id)
        .with_context(|| format!("no such issue: {id}"))
}

fn get_mut<'a>(db: &'a mut Db, id: &str) -> Result<&'a mut Issue> {
    db.issues
        .get_mut(id)
        .with_context(|| format!("no such issue: {id}"))
}

fn actor(store: &Store, cli: &Options) -> String {
    cli.actor
        .clone()
        .or_else(|| std::env::var("FOAM_ACTOR").ok())
        .or_else(|| store.git.config("user.name"))
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

fn fresh_id(db: &Db) -> String {
    loop {
        let id = new_id(&db.meta.prefix);
        if !db.issues.contains_key(&id) {
            return id;
        }
    }
}

/// Print the issue as it is after a write.
fn show_after(store: &Store, id: &str, cli: &Options) -> Result<()> {
    let snap = load(store)?;
    let issue = get(&snap.db, id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issue)?);
    } else {
        println!("{}", line(issue));
    }
    Ok(())
}

fn print_issues(issues: &[&Issue], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(issues)?);
    } else {
        for i in issues {
            println!("{}", line(i));
        }
    }
    Ok(())
}

/// Describe how far `HEAD` has moved since a stamp was taken.
pub fn age(stamp: &Stamp, git: &Git) -> String {
    match git.distance(&stamp.commit) {
        Distance::Behind(0) => format!("at {}, this commit", stamp.commit),
        Distance::Behind(1) => format!("at {}, 1 commit ago", stamp.commit),
        Distance::Behind(n) => format!("at {}, {n} commits ago", stamp.commit),
        Distance::Elsewhere => format!(
            "at {} on {}, not in this branch's history",
            stamp.commit, stamp.branch
        ),
        Distance::Unknown => format!(
            "at {} on {}, a commit this clone lacks",
            stamp.commit, stamp.branch
        ),
    }
}

pub fn memory_line(m: &Memory, git: &Git) -> String {
    format!("{}  {}  ({})", m.slug, m.text, age(&m.stamp, git))
}

fn line(i: &Issue) -> String {
    let mut s = format!("{}  P{}  {:<11}  {}", i.id, i.priority, i.status, i.title);
    if let Some(a) = &i.assignee {
        s.push_str(&format!("  @{a}"));
    }
    s
}

fn print_issue(i: &Issue, db: &Db) {
    println!("{}  {}", i.id, i.title);
    println!(
        "type: {}  status: {}  priority: {}",
        i.kind, i.status, i.priority
    );
    if !i.labels.is_empty() {
        println!("labels: {}", i.labels.join(", "));
    }
    if let Some(p) = &i.parent {
        println!("parent: {p}");
    }
    if let Some(a) = &i.assignee {
        match i.lease_expires {
            Some(t) => println!("assignee: {a}  lease until {t}"),
            None => println!("assignee: {a}"),
        }
    }
    if let Some(t) = i.defer_until {
        println!("deferred until: {t}");
    }
    if !i.blocked_by.is_empty() {
        println!("blocked by:");
        for b in &i.blocked_by {
            let status = db
                .issues
                .get(b)
                .map(|o| o.status.to_string())
                .unwrap_or_else(|| "missing".to_string());
            println!("  {b}  {status}");
        }
    }
    println!(
        "created: {}  on {} ({})",
        i.created_at, i.stamps.created.branch, i.stamps.created.commit
    );
    if let Some(c) = i.closed_at {
        println!("closed: {c}  {}", i.close_reason.as_deref().unwrap_or(""));
    }
    if !i.body.is_empty() {
        println!();
        println!("{}", i.body);
    }
    if !i.notes.is_empty() {
        println!();
        for n in &i.notes {
            println!("[{} {}] {}", n.at, n.author, n.text);
        }
    }
}
