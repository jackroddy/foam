use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::model::{Issue, Stamps, Status, new_id};
use crate::store::{Db, Store};
use crate::{Cli, Cmd};

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
            let snap = store.load()?.context("foam is not initialized here")?;
            let issue = &snap.db.issues[&id];
            if cli.json {
                println!("{}", serde_json::to_string_pretty(issue)?);
            } else {
                println!("{id}");
            }
            Ok(())
        }
        Cmd::Show { id } => {
            let snap = store.load()?.context("foam is not initialized here")?;
            let issue = snap
                .db
                .issues
                .get(&id)
                .with_context(|| format!("no such issue: {id}"))?;
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
            let snap = store.load()?.context("foam is not initialized here")?;
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
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&issues)?);
            } else {
                for i in issues {
                    println!("{}", line(i));
                }
            }
            Ok(())
        }
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

fn fresh_id(db: &Db) -> String {
    loop {
        let id = new_id(&db.meta.prefix);
        if !db.issues.contains_key(&id) {
            return id;
        }
    }
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
