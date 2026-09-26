use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

/// A handle on the repository that contains `dir`.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

/// One entry of a tree, as `git mktree` takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub oid: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Blob,
    Tree,
}

/// Where `HEAD` was when a write happened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    pub commit: String,
    pub branch: String,
}

/// Where a stamped commit stands relative to `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distance {
    /// In this branch's history, this many commits back.
    Behind(u64),
    /// Not in this branch's history.
    Elsewhere,
    /// The commit is not in this repository at all.
    Unknown,
}

/// The result of `git merge-tree --write-tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub tree: String,
    pub conflicts: Vec<Conflict>,
}

/// One conflicted path with its three blobs; a missing stage
/// means the path was absent from that side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

/// The outcome of a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    Done,
    Rejected,
}

/// The outcome of a compare-and-swap on a ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swap {
    Done,
    Lost,
}

impl Git {
    pub fn open(dir: &Path) -> Result<Git> {
        let git = Git {
            dir: dir.to_path_buf(),
        };
        git.run(&["rev-parse", "--git-dir"])
            .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
        Ok(git)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.dir).args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_with_input(args, &[])
    }

    fn run_with_input(&self, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run git")?;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(input)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// The object id `rev` names, or `None` if nothing does.
    pub fn rev_parse(&self, rev: &str) -> Result<Option<String>> {
        let out = self
            .command(&["rev-parse", "--verify", "-q", rev])
            .stderr(Stdio::null())
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8(out.stdout)?.trim_end().to_string()))
    }

    /// Every blob under `commit`, as `(path, oid)`.
    pub fn ls_tree(&self, commit: &str) -> Result<Vec<(String, String)>> {
        let out = self.run(&["ls-tree", "-r", "-z", commit])?;
        let mut entries = Vec::new();
        for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record)?;
            // "<mode> <type> <oid>\t<path>"
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("malformed ls-tree record: {record}"))?;
            let oid = meta
                .rsplit(' ')
                .next()
                .ok_or_else(|| anyhow!("malformed ls-tree record: {record}"))?;
            entries.push((path.to_string(), oid.to_string()));
        }
        Ok(entries)
    }

    /// The contents of each of `oids`, in order.
    pub fn cat_file_batch(&self, oids: &[&str]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = oids.join("\n");
        input.push('\n');
        let out = self.run_with_input(&["cat-file", "--batch"], input.as_bytes())?;
        let mut blobs = Vec::with_capacity(oids.len());
        let mut rest = &out[..];
        for oid in oids {
            // "<oid> <type> <size>\n<content>\n"
            let nl = rest
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| anyhow!("truncated cat-file output at {oid}"))?;
            let header = std::str::from_utf8(&rest[..nl])?;
            let size: usize = header
                .rsplit(' ')
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("object {oid} is missing: {header}"))?;
            let start = nl + 1;
            blobs.push(rest[start..start + size].to_vec());
            rest = &rest[start + size + 1..];
        }
        Ok(blobs)
    }

    pub fn hash_object(&self, content: &[u8]) -> Result<String> {
        let out = self.run_with_input(&["hash-object", "-w", "--stdin"], content)?;
        Ok(String::from_utf8(out)?.trim_end().to_string())
    }

    pub fn mktree(&self, entries: &[TreeEntry]) -> Result<String> {
        let mut input = String::new();
        for e in entries {
            let (mode, kind) = match e.kind {
                EntryKind::Blob => ("100644", "blob"),
                EntryKind::Tree => ("040000", "tree"),
            };
            input.push_str(&format!("{mode} {kind} {}\t{}\n", e.oid, e.name));
        }
        let out = self.run_with_input(&["mktree"], input.as_bytes())?;
        Ok(String::from_utf8(out)?.trim_end().to_string())
    }

    pub fn commit_tree(&self, tree: &str, parents: &[&str], message: &str) -> Result<String> {
        let mut args = vec!["commit-tree", tree];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        args.push("-m");
        args.push(message);
        let out = self.run(&args)?;
        Ok(String::from_utf8(out)?.trim_end().to_string())
    }

    /// Point `name` at `new`, provided it still points at `old`
    /// (`None` means it must not exist yet).
    pub fn update_ref(&self, name: &str, new: &str, old: Option<&str>) -> Result<Swap> {
        let out = self
            .command(&["update-ref", name, new, old.unwrap_or("")])
            .stderr(Stdio::piped())
            .output()?;
        if out.status.success() {
            return Ok(Swap::Done);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        // git reports a failed old-value check as "cannot lock
        // ref", the same words as a real lock contention; both
        // clear on a retry so both are reported as Lost
        if err.contains("cannot lock ref") || err.contains("but expected") {
            return Ok(Swap::Lost);
        }
        bail!("git update-ref {name} failed: {}", err.trim());
    }

    pub fn head_stamp(&self) -> Result<Stamp> {
        let commit = self
            .rev_parse("HEAD")?
            .map(|oid| oid[..7].to_string())
            .unwrap_or_else(|| "none".to_string());
        let branch = self
            .command(&["symbolic-ref", "--short", "-q", "HEAD"])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
            .unwrap_or_else(|| "detached".to_string());
        Ok(Stamp { commit, branch })
    }

    pub fn config(&self, key: &str) -> Option<String> {
        let out = self
            .command(&["config", "--get", key])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let value = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        (!value.is_empty()).then_some(value)
    }

    pub fn distance(&self, commit: &str) -> Distance {
        if self
            .rev_parse(&format!("{commit}^{{commit}}"))
            .ok()
            .flatten()
            .is_none()
        {
            return Distance::Unknown;
        }
        let ancestor = self
            .command(&["merge-base", "--is-ancestor", commit, "HEAD"])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ancestor {
            return Distance::Elsewhere;
        }
        let range = format!("{commit}..HEAD");
        self.command(&["rev-list", "--count", &range])
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .map(Distance::Behind)
            .unwrap_or(Distance::Unknown)
    }

    /// The root of the working tree.
    pub fn toplevel(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(String::from_utf8(out)?.trim_end()))
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let status = self
            .command(&["merge-base", "--is-ancestor", ancestor, descendant])
            .stderr(Stdio::null())
            .status()?;
        Ok(status.success())
    }

    /// Three-way merge `theirs` into `ours` without a checkout.
    pub fn merge_tree(&self, ours: &str, theirs: &str) -> Result<Merged> {
        let out = self
            .command(&["merge-tree", "--write-tree", "-z", ours, theirs])
            .stderr(Stdio::piped())
            .output()?;
        // exit 0 is clean, 1 is conflicts; anything else failed
        if !matches!(out.status.code(), Some(0 | 1)) {
            bail!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let mut records = out.stdout.split(|b| *b == 0);
        let tree = std::str::from_utf8(records.next().unwrap_or_default())?.to_string();
        let mut conflicts: Vec<Conflict> = Vec::new();
        // "<mode> <oid> <stage>\t<path>" until an empty record
        // ends the section
        for record in records {
            if record.is_empty() {
                break;
            }
            let record = std::str::from_utf8(record)?;
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("malformed merge-tree record: {record}"))?;
            let mut fields = meta.split(' ');
            let (Some(_mode), Some(oid), Some(stage)) =
                (fields.next(), fields.next(), fields.next())
            else {
                bail!("malformed merge-tree record: {record}");
            };
            let entry = match conflicts.iter_mut().find(|c| c.path == path) {
                Some(c) => c,
                None => {
                    conflicts.push(Conflict {
                        path: path.to_string(),
                        ..Default::default()
                    });
                    conflicts.last_mut().unwrap()
                }
            };
            let slot = match stage {
                "1" => &mut entry.base,
                "2" => &mut entry.ours,
                "3" => &mut entry.theirs,
                _ => bail!("unexpected merge stage in: {record}"),
            };
            *slot = Some(oid.to_string());
        }
        Ok(Merged { tree, conflicts })
    }

    /// Whether `remote` has a ref named `name`.
    pub fn ls_remote(&self, remote: &str, name: &str) -> Result<bool> {
        let out = self
            .command(&["ls-remote", "--exit-code", remote, name])
            .stderr(Stdio::piped())
            .output()?;
        match out.status.code() {
            Some(0) => Ok(true),
            // 2 is "no matching refs"
            Some(2) => Ok(false),
            _ => bail!(
                "git ls-remote {remote} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    pub fn fetch(&self, remote: &str, refspec: &str) -> Result<()> {
        self.run(&["fetch", "--quiet", remote, refspec])?;
        Ok(())
    }

    pub fn push(&self, remote: &str, refspec: &str) -> Result<Push> {
        // the pre-push hook foam installs runs foam sync,
        // which would push again; the variable tells it
        // this push is already that
        let out = self
            .command(&["push", "--quiet", remote, refspec])
            .env("FOAM_IN_HOOK", "1")
            .stderr(Stdio::piped())
            .output()?;
        if out.status.success() {
            return Ok(Push::Done);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("[rejected]")
            || err.contains("non-fast-forward")
            || err.contains("fetch first")
        {
            return Ok(Push::Rejected);
        }
        bail!("git push failed: {}", err.trim());
    }

    pub fn remote_url(&self, remote: &str) -> Option<String> {
        self.config(&format!("remote.{remote}.url"))
    }

    pub fn config_all(&self, key: &str) -> Vec<String> {
        self.command(&["config", "--get-all", key])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn config_add(&self, key: &str, value: &str) -> Result<()> {
        self.run(&["config", "--add", key, value])?;
        Ok(())
    }

    /// Where this repository's hooks live.
    pub fn hooks_dir(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--git-path", "hooks"])?;
        let path = PathBuf::from(String::from_utf8(out)?.trim_end());
        Ok(if path.is_absolute() {
            path
        } else {
            self.dir.join(path)
        })
    }
}
