use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::git::{Distance, Git, Push, Stamp};
use crate::graph;
use crate::model::{
    Issue, MEMORY_MAX_BYTES, Memory, Note, Stamps, Status, check_slug, new_id, parse_when,
};
use crate::prime;
use crate::store::{Absorbed, DATA_REF, Db, ORIGIN_REF, Snapshot, Store};
use crate::{Cli, Cmd, ConfigKey, DepCmd, SetupCmd};

/// The global flags, split from the subcommand so both can move.
struct Options {
    json: bool,
    actor: Option<String>,
}

pub fn run(cli: Cli) -> Result<()> {
    let store = Store::open(&cli.directory)?;
    let command = cli.command;
    let cli = Options {
        json: cli.json,
        actor: cli.actor,
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
            print_issues(&issues, &snap.db, cli.json)
        }
        Cmd::Ready { limit } => {
            let snap = load(&store)?;
            let mut issues = graph::ready(&snap.db, Timestamp::now());
            if let Some(n) = limit {
                issues.truncate(n);
            }
            print_issues(&issues, &snap.db, cli.json)
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
                    println!("{}  <- {}", line(i, Some(&snap.db)), by.join(" "));
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
            let actor = actor(&store, &cli);
            let stamp = store.git.head_stamp()?;
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
                    Some(Status::Closed) => issue.close(None, &stamp),
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
                issue.touch();
                Ok(issue.clone())
            })?;
            print_written(&issue, &cli)
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
            done(&ids, "closed", cli.json)
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
            done(&ids, "reopened", cli.json)
        }
        Cmd::Claim { id, force } => {
            let actor = actor(&store, &cli);
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
                for i in issues {
                    println!("{}", line(i, Some(&snap.db)));
                }
                for m in memories {
                    println!("{}", memory_line(m, &store.git));
                }
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
                println!("({})", age(&m.stamp, store.git.distance(&m.stamp.commit)));
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
            let actor = actor(&store, &cli);
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
        Cmd::Setup { command } => match command {
            SetupCmd::Claude { remove } => setup_claude(&store, remove),
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
                for (oid, when, msg) in entries {
                    println!("{}  {when}  {msg}", &oid[..7]);
                }
            }
            Ok(())
        }
        Cmd::Dep { command } => match command {
            DepCmd::Add { id, blocker } => {
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
                get(&snap.db, &id)?;
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
    for m in db.memories.values() {
        if m.updated_at > now + jiff::SignedDuration::from_mins(5) {
            report(format!("memory {}: updated_at is in the future", m.slug));
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

/// Print an issue a write just committed.
fn print_written(issue: &Issue, cli: &Options) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issue)?);
    } else {
        println!("{}", line(issue, None));
    }
    Ok(())
}

fn print_issues(issues: &[&Issue], db: &Db, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(issues)?);
    } else {
        for i in issues {
            println!("{}", line(i, Some(db)));
        }
    }
    Ok(())
}

/// The `[closed/total closed]` tag for an epic's line, or nothing.
pub fn rollup_tag(db: &Db, i: &Issue) -> String {
    match graph::rollup(db, i) {
        Some((closed, total)) => format!("  [{closed}/{total} closed]"),
        None => String::new(),
    }
}

/// Describe how far `HEAD` has moved since a stamp was taken.
pub fn age(stamp: &Stamp, distance: Distance) -> String {
    match distance {
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
    let distance = git.distance(&m.stamp.commit);
    format!("{}  {}  ({})", m.slug, m.text, age(&m.stamp, distance))
}

fn line(i: &Issue, db: Option<&Db>) -> String {
    let mut s = format!("{}  P{}  {:<11}  {}", i.id, i.priority, i.status, i.title);
    if let Some(db) = db {
        s.push_str(&rollup_tag(db, i));
    }
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
    let children = graph::children(db, &i.id);
    if !children.is_empty() {
        match graph::rollup(db, i) {
            Some((closed, total)) => println!("children ({closed}/{total} closed):"),
            None => println!("children:"),
        }
        for c in children {
            println!("  {}  {}  {}", c.id, c.status, c.title);
        }
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
