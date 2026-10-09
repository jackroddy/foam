use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::ConfigKey;
use crate::graph;
use crate::model::{
    Issue, Kind, MEMORY_MAX_BYTES, Memory, Meta, Note, Resolution, Stamps, Status, check_slug,
    new_id,
};
use crate::render::{self, Style};
use crate::store::{Db, Store};

/// The fields of an issue `create` writes.
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub kind: Kind,
    pub priority: u8,
    pub labels: Vec<String>,
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
}

/// The fields `update` changes; `None` and empty leave a field as it is.
pub struct Changes {
    pub title: Option<String>,
    pub body: Option<String>,
    pub kind: Option<Kind>,
    pub priority: Option<u8>,
    pub status: Option<Status>,
    pub assignee: Option<String>,
    pub add_label: Vec<String>,
    pub rm_label: Vec<String>,
    pub parent: Option<String>,
    pub defer_until: Option<Timestamp>,
    pub resolution: Option<Resolution>,
    pub reason: Option<String>,
}

pub fn create(store: &Store, new: &NewIssue) -> Result<Issue> {
    let stamp = store.git.head_stamp()?;
    store.write_named(|db| {
        let id = fresh_id(db);
        let blocked_by = resolve_all(db, &new.blocked_by)?;
        let parent = new.parent.as_deref().map(|p| resolve(db, p)).transpose()?;
        let now = Timestamp::now();
        let issue = Issue {
            id: id.clone(),
            title: new.title.clone(),
            body: new.body.clone(),
            kind: new.kind,
            status: Status::Open,
            priority: new.priority,
            labels: new.labels.clone(),
            parent,
            blocked_by,
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
    })
}

pub fn update(store: &Store, ids: &[String], actor: &str, changes: &Changes) -> Result<Vec<Issue>> {
    let Changes {
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
    } = changes;
    store.write_named(|db| {
        let ids = resolve_all(db, ids)?;
        let parent = parent
            .as_deref()
            .map(|p| {
                if p.is_empty() {
                    Ok(String::new())
                } else {
                    resolve(db, p)
                }
            })
            .transpose()?;
        let lease = db.meta.lease();
        let mut issues = Vec::new();
        for id in &ids {
            if let Some(p) = parent.as_ref().filter(|p| !p.is_empty())
                && graph::parent_would_loop(db, id, p)
            {
                bail!("{p} is {id} or sits under it, so it cannot be its parent");
            }
            let issue = get_mut(db, id)?;
            if let Some(t) = title {
                issue.title = t.clone();
            }
            if let Some(b) = body {
                issue.body = b.clone();
            }
            if let Some(k) = kind {
                issue.kind = *k;
            }
            if let Some(p) = priority {
                issue.priority = *p;
            }
            if let Some(a) = assignee {
                issue.assignee = Some(a.clone()).filter(|a| !a.is_empty());
            }
            for l in add_label {
                if !issue.labels.contains(l) {
                    issue.labels.push(l.clone());
                }
            }
            issue.labels.retain(|l| !rm_label.contains(l));
            if let Some(p) = &parent {
                issue.parent = Some(p.clone()).filter(|p| !p.is_empty());
            }
            if let Some(t) = defer_until {
                issue.defer_until = Some(*t);
                issue.status = Status::Deferred;
            }
            match status {
                Some(Status::Closed) => {
                    bail!("close it with `foam close {id} --reason ..`")
                }
                Some(Status::Open) => {
                    issue.reopen();
                    issue.unclaim();
                }
                Some(Status::InProgress) => issue.claim(actor, lease),
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
                    issue.resolution = Some(*r);
                }
                if let Some(why) = reason {
                    issue.close_reason = Some(why.clone());
                }
            }
            issue.touch();
            issues.push(issue.clone());
        }
        Ok((issues, format!("update {}", ids.join(" "))))
    })
}

/// Close the issues, each returned with the ids of its children
/// still open.
pub fn close(
    store: &Store,
    ids: &[String],
    reason: &str,
    dropped: bool,
) -> Result<Vec<(Issue, Vec<String>)>> {
    let stamp = store.git.head_stamp()?;
    let resolution = if dropped {
        Resolution::Dropped
    } else {
        Resolution::Done
    };
    store.write_named(|db| {
        let ids = resolve_all(db, ids)?;
        for id in &ids {
            let issue = get_mut(db, id)?;
            if issue.status == Status::Closed {
                bail!("{id} is already closed");
            }
        }
        let mut closed = Vec::new();
        for id in &ids {
            let open_children: Vec<String> = db
                .issues
                .values()
                .filter(|c| c.parent.as_deref() == Some(id) && c.status != Status::Closed)
                .map(|c| c.id.clone())
                .collect();
            let issue = get_mut(db, id)?;
            issue.close(reason.to_string(), resolution, &stamp);
            closed.push((issue.clone(), open_children));
        }
        Ok((closed, format!("close {}", ids.join(" "))))
    })
}

pub fn reopen(store: &Store, ids: &[String]) -> Result<Vec<Issue>> {
    store.write_named(|db| {
        let ids = resolve_all(db, ids)?;
        let mut reopened = Vec::new();
        for id in &ids {
            let issue = get_mut(db, id)?;
            if issue.status != Status::Closed {
                bail!("{id} is not closed");
            }
            issue.reopen();
            reopened.push(issue.clone());
        }
        Ok((reopened, format!("reopen {}", ids.join(" "))))
    })
}

pub fn claim(store: &Store, id: &str, actor: &str, force: bool) -> Result<Issue> {
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let lease = db.meta.lease();
        let issue = get_mut(db, &id)?;
        if issue.status == Status::Closed {
            bail!("{id} is closed");
        }
        if !force && issue.held_by_other(actor, Timestamp::now()) {
            bail!(
                "{id} is held by {} until {}",
                issue.assignee.as_deref().unwrap_or("?"),
                issue.lease_expires.unwrap()
            );
        }
        issue.claim(actor, lease);
        Ok((issue.clone(), format!("claim {id} by {actor}")))
    })
}

pub fn unclaim(store: &Store, id: &str, actor: &str, force: bool) -> Result<Issue> {
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let issue = get_mut(db, &id)?;
        if issue.status != Status::InProgress {
            bail!("{id} is not in progress");
        }
        if !force && issue.held_by_other(actor, Timestamp::now()) {
            bail!(
                "{id} is held by {}; pass --force to release it",
                issue.assignee.as_deref().unwrap_or("?")
            );
        }
        issue.unclaim();
        Ok((issue.clone(), format!("unclaim {id}")))
    })
}

pub fn heartbeat(store: &Store, id: &str, actor: &str) -> Result<Issue> {
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let lease = db.meta.lease();
        let issue = get_mut(db, &id)?;
        if issue.status != Status::InProgress || issue.assignee.as_deref() != Some(actor) {
            bail!("{id} is not held by {actor}");
        }
        issue.claim(actor, lease);
        Ok((issue.clone(), format!("heartbeat {id}")))
    })
}

pub fn note(store: &Store, id: &str, actor: &str, text: &str) -> Result<Issue> {
    let stamp = store.git.head_stamp()?;
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let issue = get_mut(db, &id)?;
        issue.notes.push(Note {
            at: Timestamp::now(),
            author: actor.to_string(),
            text: text.to_string(),
            commit: stamp.commit.clone(),
            branch: stamp.branch.clone(),
        });
        issue.touch();
        Ok((issue.clone(), format!("note {id}")))
    })
}

pub fn remember(store: &Store, slug: &str, text: &str) -> Result<Memory> {
    check_slug(slug).map_err(anyhow::Error::msg)?;
    if text.trim().is_empty() {
        bail!("a memory needs text");
    }
    if text.len() > MEMORY_MAX_BYTES {
        bail!(
            "a memory is a fact, not a document: {} bytes, the most is {MEMORY_MAX_BYTES}",
            text.len()
        );
    }
    let stamp = store.git.head_stamp()?;
    store.write(&format!("remember {slug}"), |db| {
        let now = Timestamp::now();
        let created_at = db.memories.get(slug).map_or(now, |m| m.created_at);
        let memory = Memory {
            slug: slug.to_string(),
            text: text.to_string(),
            created_at,
            updated_at: now,
            stamp: stamp.clone(),
        };
        db.memories.insert(slug.to_string(), memory.clone());
        Ok(memory)
    })
}

pub fn forget(store: &Store, slug: &str) -> Result<Memory> {
    store.write(&format!("forget {slug}"), |db| {
        db.memories
            .remove(slug)
            .with_context(|| format!("no such memory: {slug}"))
    })
}

pub fn config_set(store: &Store, key: ConfigKey, value: u32) -> Result<Meta> {
    let name = match key {
        ConfigKey::LeaseMinutes => "lease-minutes",
        ConfigKey::StaleAfter => "stale-after",
    };
    store.write(&format!("config {name} {value}"), |db| {
        match key {
            ConfigKey::LeaseMinutes => db.meta.lease_minutes = value,
            ConfigKey::StaleAfter => db.meta.stale_after = u64::from(value),
        }
        Ok(db.meta.clone())
    })
}

/// Release every issue `actor` holds, noting why on each.
pub fn session_end(store: &Store, actor: &str) -> Result<Vec<Issue>> {
    let stamp = store.git.head_stamp()?;
    store.write(&format!("session end {actor}"), |db| {
        let mut released = Vec::new();
        for issue in db.issues.values_mut() {
            if issue.status == Status::InProgress && issue.assignee.as_deref() == Some(actor) {
                issue.notes.push(Note {
                    at: Timestamp::now(),
                    author: actor.to_string(),
                    text: "released at session end".into(),
                    commit: stamp.commit.clone(),
                    branch: stamp.branch.clone(),
                });
                issue.unclaim();
                released.push(issue.clone());
            }
        }
        Ok(released)
    })
}

pub fn dep_add(store: &Store, id: &str, blocker: &str) -> Result<Issue> {
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let blocker = resolve(db, blocker)?;
        if graph::would_cycle(db, &id, &blocker) {
            bail!("{id} waiting on {blocker} would form a cycle");
        }
        let issue = get_mut(db, &id)?;
        if !issue.blocked_by.contains(&blocker) {
            issue.blocked_by.push(blocker.clone());
            issue.touch();
        }
        Ok((issue.clone(), format!("dep {id} <- {blocker}")))
    })
}

pub fn dep_rm(store: &Store, id: &str, blocker: &str) -> Result<Issue> {
    store.write_named(|db| {
        let id = resolve(db, id)?;
        let blocker = resolve(db, blocker)?;
        let issue = get_mut(db, &id)?;
        let before = issue.blocked_by.len();
        issue.blocked_by.retain(|b| *b != blocker);
        if issue.blocked_by.len() == before {
            bail!("{id} does not wait on {blocker}");
        }
        issue.touch();
        Ok((issue.clone(), format!("undep {id} <- {blocker}")))
    })
}

pub fn lease_expired(issue: &Issue, now: Timestamp) -> bool {
    issue.status == Status::InProgress && issue.lease_expires.is_none_or(|t| t <= now)
}

/// Reopen every in-progress issue whose lease has expired.
pub fn reclaim(store: &Store) -> Result<Vec<Issue>> {
    store.write("reclaim", |db| {
        let now = Timestamp::now();
        let mut freed = Vec::new();
        for issue in db.issues.values_mut() {
            if lease_expired(issue, now) {
                issue.unclaim();
                freed.push(issue.clone());
            }
        }
        Ok(freed)
    })
}

/// The full id `query` names: itself, the one id whose hex part
/// starts with it, or the one issue whose title contains it, open
/// issues first.
pub fn resolve(db: &Db, query: &str) -> Result<String> {
    if query.is_empty() {
        bail!("an issue id is needed");
    }
    if db.issues.contains_key(query) {
        return Ok(query.to_string());
    }
    let hex = query
        .strip_prefix(&format!("{}-", db.meta.prefix))
        .unwrap_or(query)
        .to_lowercase();
    let hex = hex.as_str();
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
