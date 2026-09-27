use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::git::Push;
use crate::graph;
use crate::model::{
    Issue, Kind, MEMORY_MAX_BYTES, Memory, Note, Resolution, Stamps, Status, check_slug, new_id,
    parse_when,
};
use crate::prime;
use crate::render::{self, Style};
use crate::store::{Absorbed, DATA_REF, Db, ORIGIN_REF, Snapshot, Store};
use crate::{Cli, Cmd, ConfigKey, DepCmd, SetupCmd};

/// The global flags, split from the subcommand so both can move.
struct Options {
    json: bool,
    actor: Option<String>,
    style: Style,
}

pub fn run(cli: Cli) -> Result<()> {
    if let Cmd::Setup {
        command: SetupCmd::Bash,
    } = cli.command
    {
        print!("{BASH_COMPLETION}");
        return Ok(());
    }
    let store = Store::open(&cli.directory)?;
    let command = cli.command;
    let cli = Options {
        json: cli.json,
        actor: cli.actor,
        style: Style::detect(cli.plain || cli.json),
    };
    match command {
        Cmd::Init { prefix } => init(&store, prefix),
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
            let db = load(&store)?.db;
            let blocked_by = resolve_all(&db, &blocked_by)?;
            let parent = parent.map(|p| resolve(&db, &p)).transpose()?;
            let issue = store.write_named(|db| {
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
                    resolution: None,
                    notes: Vec::new(),
                    stamps: Stamps {
                        created: stamp.clone(),
                        closed: None,
                    },
                };
                db.issues.insert(id.clone(), issue.clone());
                Ok((issue, format!("create {id}")))
            })?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&issue)?);
            } else {
                println!("{}", issue.id);
            }
            Ok(())
        }
        Cmd::Show { id } => {
            let snap = load(&store)?;
            let id = match id {
                Some(q) => resolve(&snap.db, &q)?,
                None => pick(&snap.db)?,
            };
            let issue = get(&snap.db, &id)?;
            if cli.json {
                let mut v = serde_json::to_value(issue)?;
                let children: Vec<&str> = graph::children(&snap.db, &id)
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                v["children"] = serde_json::json!(children);
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print!("{}", render::issue(&cli.style, issue, &snap.db));
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
            print_issues(&issues, &snap.db, &cli)
        }
        Cmd::Ready { limit } => {
            let snap = load(&store)?;
            let mut issues = graph::ready(&snap.db, Timestamp::now());
            if let Some(n) = limit {
                issues.truncate(n);
            }
            print_issues(&issues, &snap.db, &cli)
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
                let rows: Vec<(&Issue, String)> = blocked
                    .iter()
                    .map(|(i, by)| (*i, format!("<- {}", by.join(" "))))
                    .collect();
                print!("{}", render::listing(&cli.style, Some(&snap.db), &rows));
            }
            Ok(())
        }
        Cmd::Board => board(&store, &cli),
        Cmd::Pick => {
            let snap = load(&store)?;
            let id = pick(&snap.db)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(get(&snap.db, &id)?)?);
            } else {
                println!("{id}");
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
            resolution,
            reason,
        } => {
            let actor = actor(&store, &cli);
            let db = load(&store)?.db;
            let id = resolve(&db, &id)?;
            let parent = parent
                .map(|p| {
                    if p.is_empty() {
                        Ok(p)
                    } else {
                        resolve(&db, &p)
                    }
                })
                .transpose()?;
            let defer_until = defer_until
                .as_deref()
                .map(parse_when)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let issue = store.write(&format!("update {id}"), |db| {
                let lease = db.meta.lease();
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
                    Some(Status::Closed) => bail!("close it with `foam close {id} --reason ..`"),
                    Some(Status::Open) => {
                        issue.reopen();
                        issue.unclaim();
                    }
                    Some(Status::InProgress) => issue.claim(&actor, lease),
                    Some(Status::Deferred) if issue.defer_until.is_none() => {
                        bail!("deferring needs --defer-until")
                    }
                    Some(Status::Deferred) => issue.status = Status::Deferred,
                    None => {}
                }
                if resolution.is_some() || reason.is_some() {
                    if issue.status != Status::Closed {
                        bail!("{id} is not closed; a resolution and reason belong to a close");
                    }
                    if let Some(r) = resolution {
                        issue.resolution = Some(r);
                    }
                    if let Some(why) = &reason {
                        issue.close_reason = Some(why.clone());
                    }
                }
                issue.touch();
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
        }
        Cmd::Close {
            ids,
            reason,
            dropped,
        } => {
            let stamp = store.git.head_stamp()?;
            let ids = resolve_all(&load(&store)?.db, &ids)?;
            let resolution = if dropped {
                Resolution::Dropped
            } else {
                Resolution::Done
            };
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
                    get_mut(db, id)?.close(reason.clone(), resolution, &stamp);
                }
                Ok(())
            })?;
            done(&ids, "closed", cli.json)
        }
        Cmd::Reopen { ids } => {
            let ids = resolve_all(&load(&store)?.db, &ids)?;
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
            done(&ids, "reopened", cli.json)
        }
        Cmd::Claim { id, force } => {
            let actor = actor(&store, &cli);
            let id = resolve(&load(&store)?.db, &id)?;
            let issue = store.write(&format!("claim {id} by {actor}"), |db| {
                let lease = db.meta.lease();
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
                issue.claim(&actor, lease);
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
        }
        Cmd::Unclaim { id, force } => {
            let actor = actor(&store, &cli);
            let id = resolve(&load(&store)?.db, &id)?;
            let issue = store.write(&format!("unclaim {id}"), |db| {
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
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
        }
        Cmd::Heartbeat { id } => {
            let actor = actor(&store, &cli);
            let id = resolve(&load(&store)?.db, &id)?;
            let issue = store.write(&format!("heartbeat {id}"), |db| {
                let lease = db.meta.lease();
                let issue = get_mut(db, &id)?;
                if issue.status != Status::InProgress || issue.assignee.as_deref() != Some(&*actor)
                {
                    bail!("{id} is not held by {actor}");
                }
                issue.claim(&actor, lease);
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
        }
        Cmd::Reclaim => {
            let freed = reclaim(&store)?;
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
            let id = resolve(&load(&store)?.db, &id)?;
            let stamp = store.git.head_stamp()?;
            let issue = store.write(&format!("note {id}"), |db| {
                let issue = get_mut(db, &id)?;
                issue.notes.push(Note {
                    at: Timestamp::now(),
                    author: actor.clone(),
                    text: text.clone(),
                    commit: stamp.commit.clone(),
                    branch: stamp.branch.clone(),
                });
                issue.touch();
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
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
            let memories: Vec<&Memory> = snap
                .db
                .memories
                .values()
                .filter(|m| hit(&m.slug) || hit(&m.text))
                .collect();
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "issues": issues,
                        "memories": memories,
                    }))?
                );
            } else {
                print_issues(&issues, &snap.db, &cli)?;
                print!("{}", render::memories(&cli.style, &store.git, &memories));
            }
            Ok(())
        }
        Cmd::Remember { slug, text } => {
            check_slug(&slug).map_err(anyhow::Error::msg)?;
            if text.len() > MEMORY_MAX_BYTES {
                bail!(
                    "a memory is a fact, not a document: {} bytes, the most is {MEMORY_MAX_BYTES}",
                    text.len()
                );
            }
            let stamp = store.git.head_stamp()?;
            let memory = store.write(&format!("remember {slug}"), |db| {
                let now = Timestamp::now();
                let created_at = db.memories.get(&slug).map_or(now, |m| m.created_at);
                let memory = Memory {
                    slug: slug.clone(),
                    text: text.clone(),
                    created_at,
                    updated_at: now,
                    stamp: stamp.clone(),
                };
                db.memories.insert(slug.clone(), memory.clone());
                Ok(memory)
            })?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&memory)?);
            } else {
                println!("remembered {slug}");
            }
            Ok(())
        }
        Cmd::Memories => {
            let snap = load(&store)?;
            let memories: Vec<&Memory> = snap.db.memories.values().collect();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&memories)?);
            } else {
                print!("{}", render::memories(&cli.style, &store.git, &memories));
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
                print!("{}", render::memory(&cli.style, &store.git, m));
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
            done(std::slice::from_ref(&slug), "forgot", cli.json)
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
            // the hook pipes in JSON with the session id; the
            // Bash tool exports the same id, so the actor prime
            // names is the one later commands resolve to
            let session = if hook_json { hook_session_id() } else { None };
            if hook_json && session.is_none() {
                eprintln!("foam: the SessionStart hook input carried no session_id");
            }
            let actor = actor_in_session(&store, &cli, session.as_deref());
            let mut snap = snap;
            let reclaimed = if snap
                .db
                .issues
                .values()
                .any(|i| lease_expired(i, Timestamp::now()))
            {
                let freed = reclaim(&store)?;
                snap = load(&store)?;
                freed
            } else {
                Vec::new()
            };
            let text = prime::render(&snap.db, &store.git, &actor, limit, &reclaimed);
            if hook_json {
                println!("{}", prime::hook_json(&text));
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Cmd::Config { key, value } => {
            let show =
                |meta: &crate::model::Meta, key: Option<ConfigKey>, json: bool| -> Result<()> {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "lease-minutes": meta.lease_minutes,
                                "stale-after": meta.stale_after,
                            }))?
                        );
                        return Ok(());
                    }
                    match key {
                        Some(ConfigKey::LeaseMinutes) => println!("{}", meta.lease_minutes),
                        Some(ConfigKey::StaleAfter) => println!("{}", meta.stale_after),
                        None => {
                            println!("lease-minutes  {}", meta.lease_minutes);
                            println!("stale-after    {}", meta.stale_after);
                        }
                    }
                    Ok(())
                };
            match (key, value) {
                (Some(key), Some(value)) => {
                    let name = match key {
                        ConfigKey::LeaseMinutes => "lease-minutes",
                        ConfigKey::StaleAfter => "stale-after",
                    };
                    let meta = store.write(&format!("config {name} {value}"), |db| {
                        match key {
                            ConfigKey::LeaseMinutes => db.meta.lease_minutes = value,
                            ConfigKey::StaleAfter => db.meta.stale_after = u64::from(value),
                        }
                        Ok(db.meta.clone())
                    })?;
                    show(&meta, Some(key), cli.json)
                }
                (key, None) => show(&load(&store)?.db.meta, key, cli.json),
                (None, Some(_)) => bail!("a value needs a key"),
            }
        }
        Cmd::SessionEnd => {
            if !store.exists()? {
                return Ok(());
            }
            let session = hook_session_id();
            let actor = actor_in_session(&store, &cli, session.as_deref());
            let stamp = store.git.head_stamp()?;
            let released = store.write(&format!("session end {actor}"), |db| {
                let mut released = Vec::new();
                for issue in db.issues.values_mut() {
                    if issue.status == Status::InProgress
                        && issue.assignee.as_deref() == Some(&*actor)
                    {
                        issue.notes.push(Note {
                            at: Timestamp::now(),
                            author: actor.clone(),
                            text: "released at session end".into(),
                            commit: stamp.commit.clone(),
                            branch: stamp.branch.clone(),
                        });
                        issue.unclaim();
                        released.push(issue.id.clone());
                    }
                }
                Ok(released)
            })?;
            done(&released, "released", cli.json)
        }
        Cmd::Setup { command } => match command {
            SetupCmd::Claude { remove } => setup_claude(&store, remove),
            SetupCmd::Bash => unreachable!("handled before the store opens"),
        },
        Cmd::Sync { remote, setup } => {
            if setup {
                setup_sync(&store, &remote)?;
            }
            sync(&store, &remote)
        }
        Cmd::Doctor => doctor(&store, cli.json),
        Cmd::Log { id, limit } => {
            load(&store)?;
            let entries = store.git.log(DATA_REF, limit, id.as_deref())?;
            if cli.json {
                let rows: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|(oid, when, msg)| serde_json::json!({ "commit": oid, "at": when, "message": msg }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print!("{}", render::log(&cli.style, &entries));
            }
            Ok(())
        }
        Cmd::Dep { command } => match command {
            DepCmd::Add { id, blocker } => {
                let db = load(&store)?.db;
                let id = resolve(&db, &id)?;
                let blocker = resolve(&db, &blocker)?;
                let issue = store.write(&format!("dep {id} <- {blocker}"), |db| {
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
                    Ok(issue.clone())
                })?;
                print_written(&issue, &cli)
            }
            DepCmd::Rm { id, blocker } => {
                let db = load(&store)?.db;
                let id = resolve(&db, &id)?;
                let blocker = resolve(&db, &blocker)?;
                let issue = store.write(&format!("undep {id} <- {blocker}"), |db| {
                    let issue = get_mut(db, &id)?;
                    let before = issue.blocked_by.len();
                    issue.blocked_by.retain(|b| *b != blocker);
                    if issue.blocked_by.len() == before {
                        bail!("{id} does not wait on {blocker}");
                    }
                    issue.touch();
                    Ok(issue.clone())
                })?;
                print_written(&issue, &cli)
            }
            DepCmd::Tree { id } => {
                let snap = load(&store)?;
                let id = resolve(&snap.db, &id)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&graph::tree_json(&snap.db, &id))?
                    );
                } else {
                    print!("{}", graph::tree(&snap.db, &id));
                }
                Ok(())
            }
        },
    }
}

fn init(store: &Store, prefix: Option<String>) -> Result<()> {
    if store.exists()? {
        bail!("foam is already initialized here");
    }
    let prefix = match prefix {
        Some(p) => p,
        None => {
            let dir = store.git.toplevel()?;
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
    let remote = "origin";
    let has_remote = store.git.remote_url(remote).is_some();
    let remote_has_data = has_remote
        && match store.git.ls_remote(remote, DATA_REF) {
            Ok(has) => has,
            Err(e) => {
                eprintln!("foam: could not reach {remote} ({e:#}); starting locally, sync later");
                false
            }
        };
    if remote_has_data {
        store.git.fetch(remote, &format!("{DATA_REF}:{DATA_REF}"))?;
        println!("fetched {DATA_REF} from {remote}");
    } else {
        store.init(&prefix)?;
        println!("initialized {DATA_REF} with prefix {prefix}");
    }
    if has_remote {
        setup_sync(store, remote)?;
    }
    println!("run `foam setup claude` to load context into Claude Code at session start");
    Ok(())
}

/// What `setup bash` prints: fzf's own completion hook, fed the
/// listing, with the id cut out of the chosen line.
const BASH_COMPLETION: &str = "\
# foam: `foam show **<TAB>` searches the issues in fzf and inserts the id.
# Load fzf's bash integration first: eval \"$(fzf --bash)\"
_fzf_complete_foam() {
  _fzf_complete --reverse -- \"$@\" < <(foam list --all --plain)
}
_fzf_complete_foam_post() {
  awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^[a-z0-9-]+-[0-9a-f]{6}$/) { print $i; exit } }'
}
[ -n \"$BASH\" ] && type _fzf_complete >/dev/null 2>&1 \\
  && complete -F _fzf_complete_foam -o default -o bashdefault foam
";

/// Let the person choose an issue in fzf and return its id; open
/// issues come first.
fn pick(db: &Db) -> Result<String> {
    let mut issues: Vec<&Issue> = db.issues.values().collect();
    issues.sort_by_key(|i| (i.status == Status::Closed, i.priority, i.created_at));
    let feed = render::listing(&Style::PLAIN, Some(db), &plain_rows(&issues));
    let mut fzf = match std::process::Command::new("fzf")
        .args(["--reverse", "--no-multi", "--prompt", "issue> "])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("fzf is not on PATH, so pass an id")
        }
        Err(e) => return Err(e).context("fzf"),
    };
    // fzf closes its stdin once the person has chosen, so a
    // write after that is not a failure
    let _ = std::io::Write::write_all(&mut fzf.stdin.take().unwrap(), feed.as_bytes());
    let out = fzf.wait_with_output()?;
    if !out.status.success() {
        bail!("nothing picked");
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find(|w| db.issues.contains_key(*w))
        .map(str::to_string)
        .context("nothing picked")
}

/// The hooks `setup claude` installs, by event.
const CLAUDE_HOOKS: [(&str, &str); 2] = [
    ("SessionStart", "foam prime --hook-json"),
    ("SessionEnd", "foam session-end"),
];

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
    let mut changed = false;
    for (event, command) in CLAUDE_HOOKS {
        let entries = hooks
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(entries) = entries.as_array_mut() else {
            bail!("{}: \"{event}\" is not an array", path.display());
        };
        let is_ours = |entry: &serde_json::Value| {
            entry["hooks"]
                .as_array()
                .is_some_and(|hs| hs.iter().any(|h| h["command"].as_str() == Some(command)))
        };
        let present = entries.iter().any(is_ours);
        if remove && present {
            entries.retain(|e| !is_ours(e));
            changed = true;
        } else if !remove && !present {
            entries.push(serde_json::json!({
                "matcher": "",
                "hooks": [{ "type": "command", "command": command }],
            }));
            changed = true;
        }
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(&settings)?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes)?;
    match (remove, changed) {
        (true, true) => println!("removed the foam hooks from {}", path.display()),
        (true, false) => println!("no foam hooks in {}", path.display()),
        (false, true) => println!("added the foam hooks to {}", path.display()),
        (false, false) => println!("the foam hooks are already in {}", path.display()),
    }
    Ok(())
}

const HOOK_MARK: &str = "# foam: push the data ref alongside code";

const PRE_PUSH: &str = "\
#!/bin/sh
# foam: push the data ref alongside code
cat >/dev/null
[ -n \"$FOAM_IN_HOOK\" ] && exit 0
command -v foam >/dev/null 2>&1 || exit 0
git remote get-url \"$1\" >/dev/null 2>&1 || exit 0
git rev-parse -q --verify refs/foam/data >/dev/null || exit 0
FOAM_IN_HOOK=1 foam sync --remote \"$1\" || exit 1
";

/// Add the fetch refspec and the pre-push hook to this clone.
fn setup_sync(store: &Store, remote: &str) -> Result<()> {
    if store.git.remote_url(remote).is_none() {
        bail!("no remote named {remote}");
    }
    let key = format!("remote.{remote}.fetch");
    let spec = format!("+{DATA_REF}:{ORIGIN_REF}");
    if !store.git.config_all(&key).contains(&spec) {
        store.git.config_add(&key, &spec)?;
        println!("added the fetch refspec for {DATA_REF} to {remote}");
    }
    let hook = store.git.hooks_dir()?.join("pre-push");
    if store.git.hooks_redirected() {
        eprintln!(
            "foam: core.hooksPath is set, so no hook was installed; add this to {}:\n{}",
            hook.display(),
            PRE_PUSH.trim_start_matches("#!/bin/sh\n")
        );
        return Ok(());
    }
    match std::fs::read_to_string(&hook) {
        Ok(existing) if existing.contains(HOOK_MARK) => {}
        Ok(existing) if !is_shell(&existing) => {
            eprintln!(
                "foam: {} is not a shell script, so it was left alone; run this from it:\n{}",
                hook.display(),
                PRE_PUSH.trim_start_matches("#!/bin/sh\n")
            );
        }
        Ok(existing) => {
            // keep whatever was there and run ours after it,
            // minus our own shebang line
            let ours = PRE_PUSH.trim_start_matches("#!/bin/sh\n");
            std::fs::write(&hook, format!("{existing}\n{ours}"))?;
            println!("appended the foam block to {}", hook.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(hook.parent().unwrap())?;
            std::fs::write(&hook, PRE_PUSH)?;
            println!("installed {}", hook.display());
        }
        Err(e) => return Err(e).with_context(|| hook.display().to_string()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Whether a hook script's shebang names a Bourne-style shell.
fn is_shell(script: &str) -> bool {
    match script.lines().next() {
        Some(first) if first.starts_with("#!") => {
            first.ends_with("sh") || first.contains("sh ") || first.ends_with("bash")
        }
        // no shebang: git runs it through sh
        _ => true,
    }
}

/// Fetch, merge and push the data ref, retrying while someone
/// else keeps pushing first.
fn sync(store: &Store, remote: &str) -> Result<()> {
    if store.git.remote_url(remote).is_none() {
        bail!("no remote named {remote}");
    }
    for _ in 0..3 {
        if store.git.ls_remote(remote, DATA_REF)? {
            store
                .git
                .fetch(remote, &format!("+{DATA_REF}:{ORIGIN_REF}"))?;
        }
        match store.absorb_origin()? {
            Absorbed::Nothing => {}
            Absorbed::FastForward => println!("fast-forwarded to {remote}"),
            Absorbed::Merged { conflicts: 0 } => println!("merged {remote}"),
            Absorbed::Merged { conflicts } => {
                println!("merged {remote}, settling {conflicts} record(s) both sides changed")
            }
        }
        if store.git.rev_parse(DATA_REF)?.is_none() {
            bail!("foam is not initialized here");
        }
        match store.git.push(remote, &format!("{DATA_REF}:{DATA_REF}"))? {
            Push::Done => {
                println!("pushed {DATA_REF} to {remote}");
                return Ok(());
            }
            Push::Rejected => continue,
        }
    }
    bail!("could not push {DATA_REF}: {remote} kept moving")
}

/// Report what is wrong, if anything, and exit 1 if something is.
fn doctor(store: &Store, json: bool) -> Result<()> {
    let snap = load(store)?;
    let db = &snap.db;
    let now = Timestamp::now();
    let mut problems: Vec<String> = Vec::new();
    let mut report = |line: String| problems.push(line);
    for i in db.issues.values() {
        for b in &i.blocked_by {
            if !db.issues.contains_key(b) {
                report(format!("{}: waits on {b}, which does not exist", i.id));
            }
        }
        if let Some(p) = i.parent.as_ref().filter(|p| !db.issues.contains_key(*p)) {
            report(format!("{}: parent {p} does not exist", i.id));
        }
        if i.status == Status::InProgress && i.lease_expires.is_none_or(|t| t <= now) {
            report(format!(
                "{}: in progress with an expired lease; `foam reclaim` reopens it",
                i.id
            ));
        }
        let ahead = now + jiff::SignedDuration::from_mins(5);
        if i.updated_at > ahead {
            report(format!(
                "{}: updated_at is in the future; a clock is wrong somewhere",
                i.id
            ));
        }
    }
    let rules = claude_md(store)?;
    for m in db.memories.values() {
        if m.updated_at > now + jiff::SignedDuration::from_mins(5) {
            report(format!("memory {}: updated_at is in the future", m.slug));
        }
        if let Some(file) = rules
            .iter()
            .find(|(_, text)| text.contains(&squeeze(&m.text)))
        {
            report(format!(
                "memory {}: its text is also in {}, so it lands in every session twice; `foam forget {}` drops the copy",
                m.slug, file.0, m.slug
            ));
        }
    }
    if store.git.remote_url("origin").is_some() {
        let spec = format!("+{DATA_REF}:{ORIGIN_REF}");
        if !store.git.config_all("remote.origin.fetch").contains(&spec) {
            report(
                "origin has no fetch refspec for the data ref; `foam sync --setup` adds it".into(),
            );
        }
        let hook = store.git.hooks_dir()?.join("pre-push");
        if !std::fs::read_to_string(&hook).is_ok_and(|h| h.contains(HOOK_MARK)) {
            report("no foam pre-push hook; `foam sync --setup` installs it".into());
        }
    }
    if session_gap() {
        report(format!(
            "{CLAUDE_MARK} is set but {CLAUDE_SESSION} is not; every Claude Code session gets the same actor"
        ));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&problems)?);
    } else if problems.is_empty() {
        println!(
            "ok: {} issue(s), {} memor{}",
            db.issues.len(),
            db.memories.len(),
            if db.memories.len() == 1 { "y" } else { "ies" }
        );
    } else {
        for p in &problems {
            println!("{p}");
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{} problem(s)", problems.len())
    }
}

/// The most ready issues and log entries the board shows.
const BOARD_READY: usize = 10;
const BOARD_LOG: usize = 8;

fn by_priority(mut v: Vec<&Issue>) -> Vec<&Issue> {
    v.sort_by_key(|i| (i.priority, i.created_at));
    v
}

fn plain_rows<'a>(v: &[&'a Issue]) -> Vec<(&'a Issue, String)> {
    v.iter().map(|i| (*i, String::new())).collect()
}

/// One screen of where the backlog stands.
fn board(store: &Store, cli: &Options) -> Result<()> {
    let snap = load(store)?;
    let db = &snap.db;
    let now = Timestamp::now();
    let milestones = by_priority(
        db.issues
            .values()
            .filter(|i| i.kind == Kind::Milestone && i.status != Status::Closed)
            .collect(),
    );
    let in_progress = by_priority(
        db.issues
            .values()
            .filter(|i| i.status == Status::InProgress)
            .collect(),
    );
    let ready = graph::ready(db, now);
    let blocked = graph::blocked(db, now);
    let log = store.git.log(DATA_REF, BOARD_LOG, None)?;
    if cli.json {
        let blocked: Vec<serde_json::Value> = blocked
            .iter()
            .map(|(i, by)| serde_json::json!({ "issue": i, "blocked_by": by }))
            .collect();
        let log: Vec<serde_json::Value> = log
            .iter()
            .map(
                |(oid, when, msg)| serde_json::json!({ "commit": oid, "at": when, "message": msg }),
            )
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "milestones": milestones,
                "in_progress": in_progress,
                "ready": ready,
                "blocked": blocked,
                "log": log,
            }))?
        );
        return Ok(());
    }
    let style = &cli.style;
    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let stamp = store.git.head_stamp()?;
    println!(
        "{}  on {} at {}  {}",
        style.bold(&db.meta.prefix),
        stamp.branch,
        stamp.commit,
        style.dim(&format!(
            "{} open, {} in progress, {} deferred, {} closed",
            count(Status::Open),
            count(Status::InProgress),
            count(Status::Deferred),
            count(Status::Closed),
        ))
    );
    if !milestones.is_empty() {
        println!("\n{}", style.bold("Milestones"));
        print!(
            "{}",
            render::listing(style, Some(db), &plain_rows(&milestones))
        );
    }
    if !in_progress.is_empty() {
        println!("\n{}", style.bold("In progress"));
        let rows: Vec<(&Issue, String)> = in_progress
            .iter()
            .map(|i| (*i, render::lease_left(i)))
            .collect();
        print!("{}", render::listing(style, Some(db), &rows));
    }
    if ready.len() > BOARD_READY {
        println!(
            "\n{}",
            style.bold(&format!("Ready ({BOARD_READY} of {})", ready.len()))
        );
    } else {
        println!("\n{}", style.bold("Ready"));
    }
    if ready.is_empty() {
        println!("nothing is ready");
    }
    print!(
        "{}",
        render::listing(
            style,
            Some(db),
            &plain_rows(&ready[..ready.len().min(BOARD_READY)])
        )
    );
    if !blocked.is_empty() {
        println!("\n{}", style.bold("Blocked"));
        let rows: Vec<(&Issue, String)> = blocked
            .iter()
            .map(|(i, by)| (*i, format!("<- {}", by.join(" "))))
            .collect();
        print!("{}", render::listing(style, Some(db), &rows));
    }
    if !log.is_empty() {
        println!("\n{}", style.bold("Recent"));
        print!("{}", render::log(style, &log));
    }
    Ok(())
}

/// The files Claude Code loads as rules at session start, with
/// whitespace squeezed for matching.
const RULE_FILES: [&str; 3] = ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"];

fn claude_md(store: &Store) -> Result<Vec<(&'static str, String)>> {
    let top = store.git.toplevel()?;
    Ok(RULE_FILES
        .iter()
        .filter_map(|name| {
            std::fs::read_to_string(top.join(name))
                .ok()
                .map(|text| (*name, squeeze(&text)))
        })
        .collect())
}

/// Lowercase with every run of whitespace one space, so a memory
/// matches a rule that was only rewrapped.
fn squeeze(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Print what a write did to several ids.
fn done(ids: &[String], verb: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(ids)?);
    } else {
        for id in ids {
            println!("{verb} {id}");
        }
    }
    Ok(())
}

fn lease_expired(issue: &Issue, now: Timestamp) -> bool {
    issue.status == Status::InProgress && issue.lease_expires.is_none_or(|t| t <= now)
}

/// Reopen every in-progress issue whose lease has expired,
/// and return their ids.
fn reclaim(store: &Store) -> Result<Vec<String>> {
    store.write("reclaim", |db| {
        let now = Timestamp::now();
        let mut freed = Vec::new();
        for issue in db.issues.values_mut() {
            if lease_expired(issue, now) {
                issue.unclaim();
                freed.push(issue.id.clone());
            }
        }
        Ok(freed)
    })
}

fn load(store: &Store) -> Result<Snapshot> {
    store.load()?.context("foam is not initialized here")
}

/// The full id `query` names: itself, the one id whose hex part
/// starts with it, or the one issue whose title contains it, open
/// issues first.
fn resolve(db: &Db, query: &str) -> Result<String> {
    if db.issues.contains_key(query) {
        return Ok(query.to_string());
    }
    let hex = query
        .strip_prefix(&format!("{}-", db.meta.prefix))
        .unwrap_or(query);
    let mut found: Vec<&Issue> = if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        db.issues
            .values()
            .filter(|i| {
                i.id.rsplit_once('-')
                    .is_some_and(|(_, h)| h.starts_with(hex))
            })
            .collect()
    } else {
        Vec::new()
    };
    if found.is_empty() {
        let q = query.to_lowercase();
        found = db
            .issues
            .values()
            .filter(|i| i.title.to_lowercase().contains(&q))
            .collect();
        if found.iter().any(|i| i.status != Status::Closed) {
            found.retain(|i| i.status != Status::Closed);
        }
    }
    match found.as_slice() {
        [] => bail!("no such issue: {query}"),
        [one] => Ok(one.id.clone()),
        _ => {
            found.sort_by_key(|i| (i.priority, i.created_at));
            let rows: Vec<(&Issue, String)> = found.iter().map(|i| (*i, String::new())).collect();
            bail!(
                "{query} could be any of {}:\n{}",
                found.len(),
                render::listing(&Style::PLAIN, Some(db), &rows).trim_end()
            )
        }
    }
}

fn resolve_all(db: &Db, queries: &[String]) -> Result<Vec<String>> {
    queries.iter().map(|q| resolve(db, q)).collect()
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

/// The session id from a Claude Code hook's JSON on stdin, when
/// stdin is a pipe and carries one.
fn hook_session_id() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v["session_id"].as_str().map(str::to_string))
}

/// Set in every process Claude Code's Bash tool runs.
//
// both names were read off a live session's environment,
// not from documentation; if a Claude Code release drops
// or renames one, session_gap() is what notices
const CLAUDE_MARK: &str = "CLAUDECODE";
const CLAUDE_SESSION: &str = "CLAUDE_CODE_SESSION_ID";

/// Whether this is a Claude Code session with no session id,
/// which would give every session the same actor.
fn session_gap() -> bool {
    std::env::var_os(CLAUDE_MARK).is_some() && std::env::var_os(CLAUDE_SESSION).is_none()
}

fn actor(store: &Store, cli: &Options) -> String {
    let actor = actor_in_session(store, cli, None);
    if cli.actor.is_none() && std::env::var_os("FOAM_ACTOR").is_none() && session_gap() {
        eprintln!(
            "foam: no session id from Claude Code; acting as {actor}, pass --actor to keep sessions apart"
        );
    }
    actor
}

/// Who is acting. An explicit `--actor` or `$FOAM_ACTOR` is taken
/// whole; otherwise the git user gets a session suffix, so two
/// sessions of one person do not share their claims.
fn actor_in_session(store: &Store, cli: &Options, session: Option<&str>) -> String {
    if let Some(a) = cli
        .actor
        .clone()
        .or_else(|| std::env::var("FOAM_ACTOR").ok())
    {
        return a;
    }
    let base = store
        .git
        .config("user.name")
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let session = std::env::var(CLAUDE_SESSION)
        .ok()
        .or_else(|| session.map(str::to_string));
    match session {
        Some(s) if !s.is_empty() => format!("{base}/{}", s.chars().take(8).collect::<String>()),
        _ => base,
    }
}

fn fresh_id(db: &Db) -> String {
    loop {
        let id = new_id(&db.meta.prefix);
        if !db.issues.contains_key(&id) {
            return id;
        }
    }
}

/// Print an issue a write just committed.
fn print_written(issue: &Issue, cli: &Options) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issue)?);
    } else {
        let rows = [(issue, String::new())];
        print!("{}", render::listing(&cli.style, None, &rows));
    }
    Ok(())
}

fn print_issues(issues: &[&Issue], db: &Db, cli: &Options) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issues)?);
    } else {
        let rows: Vec<(&Issue, String)> = issues.iter().map(|i| (*i, String::new())).collect();
        print!("{}", render::listing(&cli.style, Some(db), &rows));
    }
    Ok(())
}
