use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::git::{EntryKind, Git, Swap, TreeEntry};
use crate::model::{Issue, Memory, Meta, SCHEMA_VERSION, canonical};

pub const DATA_REF: &str = "refs/foam/data";

const RETRIES: usize = 5;

/// Everything on the data ref, decoded.
#[derive(Debug, Clone)]
pub struct Db {
    pub meta: Meta,
    pub issues: BTreeMap<String, Issue>,
    pub memories: BTreeMap<String, Memory>,
}

/// One loaded commit of the data ref.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub db: Db,

    /// The bytes of each file as loaded, by path.
    //
    // a write reuses the oid of any file whose bytes
    // did not change, so only edited records cost a
    // hash-object call
    files: BTreeMap<String, (String, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub git: Git,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        Ok(Store {
            git: Git::open(dir)?,
        })
    }

    pub fn exists(&self) -> Result<bool> {
        Ok(self.git.rev_parse(DATA_REF)?.is_some())
    }

    /// Load the data ref, or `None` when `foam init` has not run.
    pub fn load(&self) -> Result<Option<Snapshot>> {
        let Some(commit) = self.git.rev_parse(DATA_REF)? else {
            return Ok(None);
        };
        Ok(Some(self.load_commit(&commit)?))
    }

    fn load_commit(&self, commit: &str) -> Result<Snapshot> {
        let entries = self.git.ls_tree(commit)?;
        let oids: Vec<&str> = entries.iter().map(|(_, oid)| oid.as_str()).collect();
        let blobs = self.git.cat_file_batch(&oids)?;

        let mut files = BTreeMap::new();
        for ((path, oid), bytes) in entries.into_iter().zip(blobs) {
            files.insert(path, (oid, bytes));
        }

        let db = Db::decode(&files)?;
        Ok(Snapshot {
            commit: commit.to_string(),
            db,
            files,
        })
    }

    /// Create the data ref with an empty database.
    pub fn init(&self, prefix: &str) -> Result<()> {
        let db = Db {
            meta: Meta {
                prefix: prefix.to_string(),
                schema_version: SCHEMA_VERSION,
                created_at: Timestamp::now(),
            },
            issues: BTreeMap::new(),
            memories: BTreeMap::new(),
        };
        let tree = self.write_tree(&db, &BTreeMap::new())?;
        let commit = self.git.commit_tree(&tree, &[], "init")?;
        match self.git.update_ref(DATA_REF, &commit, None)? {
            Swap::Done => Ok(()),
            Swap::Lost => bail!("{DATA_REF} already exists"),
        }
    }

    /// Apply `change` to the current database and commit the
    /// result, retrying from a fresh load if another writer
    /// got there first.
    pub fn write<T>(
        &self,
        message: &str,
        mut change: impl FnMut(&mut Db) -> Result<T>,
    ) -> Result<T> {
        for attempt in 1..=RETRIES {
            let snap = self.load()?.context("foam is not initialized here")?;
            let mut db = snap.db;
            let out = change(&mut db)?;
            let tree = self.write_tree(&db, &snap.files)?;
            let commit = self.git.commit_tree(&tree, &[&snap.commit], message)?;
            match self.git.update_ref(DATA_REF, &commit, Some(&snap.commit))? {
                Swap::Done => return Ok(out),
                Swap::Lost => {
                    // a random wait so writers that lost together
                    // do not reload and collide together
                    let jitter = rand::random::<u64>() % 20;
                    std::thread::sleep(Duration::from_millis(attempt as u64 * jitter));
                }
            }
        }
        bail!("gave up after {RETRIES} attempts: another writer kept winning")
    }

    fn write_tree(&self, db: &Db, loaded: &BTreeMap<String, (String, Vec<u8>)>) -> Result<String> {
        let blob = |path: String, bytes: Vec<u8>| -> Result<TreeEntry> {
            let oid = match loaded.get(&path) {
                Some((oid, old)) if *old == bytes => oid.clone(),
                _ => self.git.hash_object(&bytes)?,
            };
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            Ok(TreeEntry {
                name,
                oid,
                kind: EntryKind::Blob,
            })
        };

        let mut issues = Vec::new();
        for (id, issue) in &db.issues {
            issues.push(blob(format!("issues/{id}.json"), canonical(issue))?);
        }
        let mut memories = Vec::new();
        for (slug, memory) in &db.memories {
            memories.push(blob(format!("memories/{slug}.json"), canonical(memory))?);
        }
        let meta = blob("meta.json".to_string(), canonical(&db.meta))?;

        let root = vec![
            TreeEntry {
                name: "issues".to_string(),
                oid: self.git.mktree(&issues)?,
                kind: EntryKind::Tree,
            },
            TreeEntry {
                name: "memories".to_string(),
                oid: self.git.mktree(&memories)?,
                kind: EntryKind::Tree,
            },
            meta,
        ];
        self.git.mktree(&root)
    }
}

impl Db {
    fn decode(files: &BTreeMap<String, (String, Vec<u8>)>) -> Result<Db> {
        let (_, meta) = files.get("meta.json").context("meta.json is missing")?;
        let meta: Meta = serde_json::from_slice(meta).context("meta.json")?;
        if meta.schema_version > SCHEMA_VERSION {
            bail!(
                "data is schema {} but this foam reads up to {SCHEMA_VERSION}",
                meta.schema_version
            );
        }

        let mut issues = BTreeMap::new();
        let mut memories = BTreeMap::new();
        for (path, (_, bytes)) in files {
            if let Some(name) = path.strip_prefix("issues/") {
                let issue: Issue = serde_json::from_slice(bytes).with_context(|| path.clone())?;
                issues.insert(name.trim_end_matches(".json").to_string(), issue);
            } else if let Some(name) = path.strip_prefix("memories/") {
                let memory: Memory = serde_json::from_slice(bytes).with_context(|| path.clone())?;
                memories.insert(name.trim_end_matches(".json").to_string(), memory);
            }
        }
        Ok(Db {
            meta,
            issues,
            memories,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Stamp;
    use crate::model::{Kind, Stamps, Status};
    use std::process::Command;

    fn repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn issue(id: &str) -> Issue {
        let now = Timestamp::now();
        let stamp = Stamp {
            commit: "none".into(),
            branch: "main".into(),
        };
        Issue {
            id: id.into(),
            title: "t".into(),
            body: String::new(),
            kind: Kind::Task,
            status: Status::Open,
            priority: 2,
            labels: vec![],
            parent: None,
            blocked_by: vec![],
            related: vec![],
            assignee: None,
            lease_expires: None,
            defer_until: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            close_reason: None,
            notes: vec![],
            stamps: Stamps {
                created: stamp,
                closed: None,
            },
        }
    }

    #[test]
    fn init_then_load_round_trips() {
        let (_dir, store) = repo();
        assert!(store.load().unwrap().is_none());
        store.init("t").unwrap();
        let snap = store.load().unwrap().unwrap();
        assert_eq!(snap.db.meta.prefix, "t");
        assert!(snap.db.issues.is_empty());
        assert!(store.init("t").is_err());
    }

    #[test]
    fn write_commits_on_top_of_the_last_commit() {
        let (_dir, store) = repo();
        store.init("t").unwrap();
        let first = store.load().unwrap().unwrap().commit;
        store
            .write("create", |db| {
                db.issues.insert("t-000001".into(), issue("t-000001"));
                Ok(())
            })
            .unwrap();
        let snap = store.load().unwrap().unwrap();
        assert_ne!(snap.commit, first);
        assert_eq!(snap.db.issues["t-000001"].title, "t");
        assert!(snap.files.contains_key("issues/t-000001.json"));
    }

    #[test]
    fn unchanged_files_keep_their_oid() {
        let (_dir, store) = repo();
        store.init("t").unwrap();
        store
            .write("a", |db| {
                db.issues.insert("t-000001".into(), issue("t-000001"));
                Ok(())
            })
            .unwrap();
        let before = store.load().unwrap().unwrap();
        store
            .write("b", |db| {
                db.issues.insert("t-000002".into(), issue("t-000002"));
                Ok(())
            })
            .unwrap();
        let after = store.load().unwrap().unwrap();
        assert_eq!(
            before.files["issues/t-000001.json"].0,
            after.files["issues/t-000001.json"].0
        );
        assert_eq!(before.files["meta.json"].0, after.files["meta.json"].0);
    }

    #[test]
    fn a_lost_swap_reloads_and_retries() {
        let (_dir, store) = repo();
        store.init("t").unwrap();
        // the first attempt commits behind the closure's back,
        // so its swap loses and the second attempt sees the
        // interloper
        let mut seen = Vec::new();
        let interloper = store.clone();
        let mut first = true;
        let n = store
            .write("outer", |db| {
                if first {
                    first = false;
                    interloper
                        .write("inner", |db| {
                            db.issues.insert("t-inner0".into(), issue("t-inner0"));
                            Ok(())
                        })
                        .unwrap();
                }
                seen.push(db.issues.len());
                db.issues.insert("t-outer0".into(), issue("t-outer0"));
                Ok(db.issues.len())
            })
            .unwrap();
        assert_eq!(n, 2);
        let snap = store.load().unwrap().unwrap();
        assert_eq!(snap.db.issues.len(), 2);
    }
}
