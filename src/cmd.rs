use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::graph;
use crate::model::{Issue, Stamps, Status, new_id, parse_when};
use crate::store::{Db, Snapshot, Store};
use crate::{Cli, Cmd, DepCmd};

/// The global flags, split from the subcommand so both can move.
struct Options {
    directory: PathBuf,
    json: bool,
}

pub fn run(cli: Cli) -> Result<()> {
    let store = Store::open(&cli.directory)?;
    let command = cli.command;
    let cli = Options {
        directory: cli.directory,
        json: cli.json,
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
        println!("assignee: {a}");
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
