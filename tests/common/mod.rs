//! Repositories, clones and actors, shared by the integration
//! tests and the sandbox bench so a line in either reads the same.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The foam binary this build produced.
pub fn foam_bin() -> PathBuf {
    // cargo names the binary for an integration test at compile
    // time; an example gets no such variable and sits one
    // directory below the binary instead
    match option_env!("CARGO_BIN_EXE_foam") {
        Some(p) => PathBuf::from(p),
        None => {
            let exe = std::env::current_exe().unwrap();
            exe.parent().unwrap().parent().unwrap().join("foam")
        }
    }
}

/// `$PATH` with the built binary's directory in front, so the
/// pre-push hook's `foam` is this build.
fn path_with_foam() -> String {
    let bin = foam_bin();
    let dir = bin.parent().unwrap().to_str().unwrap();
    format!("{dir}:{}", std::env::var("PATH").unwrap_or_default())
}

/// A foam command in `dir`, outside any Claude Code session.
pub fn foam(dir: &Path) -> Command {
    let mut cmd = Command::new(foam_bin());
    cmd.current_dir(dir);
    cmd.env("PATH", path_with_foam());
    cmd.env_remove("CLAUDECODE");
    cmd.env_remove("CLAUDE_CODE_SESSION_ID");
    cmd.env_remove("FOAM_ACTOR");
    cmd
}

/// A foam command in `dir` acting as `name`.
pub fn actor(dir: &Path, name: &str) -> Command {
    let mut cmd = foam(dir);
    cmd.args(["--actor", name]);
    cmd
}

/// Run a command that must succeed and return its stdout,
/// trailing whitespace trimmed.
pub fn stdout(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

/// Run a command that may fail and return its output.
pub fn run(cmd: &mut Command) -> Output {
    cmd.output().unwrap()
}

/// Run git in `dir` as a fixed test user and return everything
/// it produced, success or not.
pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("PATH", path_with_foam())
        .current_dir(dir)
        .output()
        .unwrap()
}

/// Run git in `dir` as a fixed test user and return its stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("PATH", path_with_foam())
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

/// Run git in `dir` with `input` on its stdin and return its stdout.
pub fn git_in(dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("PATH", path_with_foam())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

/// The files of a frozen schema fixture under tests/schemas, as
/// (path, contents) with the paths the ref uses.
pub fn fixture_files(schema: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/schemas")
        .join(schema);
    let mut files = Vec::new();
    for dir in ["issues", "memories"] {
        let mut entries: Vec<_> = std::fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_str().unwrap();
            files.push((
                format!("{dir}/{name}"),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
    files.push((
        "meta.json".to_string(),
        std::fs::read_to_string(root.join("meta.json")).unwrap(),
    ));
    files
}

/// Commit `files` as the tree of refs/foam/data in `dir`, on top
/// of `parent` when there is one, with git plumbing and no foam.
pub fn seed_ref(dir: &Path, files: &[(String, String)], parent: Option<&str>) -> String {
    let blob = |contents: &str| git_in(dir, &["hash-object", "-w", "--stdin"], contents);
    let tree = |entries: &[(String, String, bool)]| {
        let listing: String = entries
            .iter()
            .map(|(name, oid, is_tree)| {
                if *is_tree {
                    format!("040000 tree {oid}\t{name}\n")
                } else {
                    format!("100644 blob {oid}\t{name}\n")
                }
            })
            .collect();
        git_in(dir, &["mktree"], &listing)
    };
    let mut root = Vec::new();
    for sub in ["issues", "memories"] {
        let entries: Vec<(String, String, bool)> = files
            .iter()
            .filter_map(|(path, contents)| {
                let name = path.strip_prefix(&format!("{sub}/"))?;
                Some((name.to_string(), blob(contents), false))
            })
            .collect();
        root.push((sub.to_string(), tree(&entries), true));
    }
    let meta = files
        .iter()
        .find(|(p, _)| p == "meta.json")
        .expect("meta.json");
    root.push(("meta.json".to_string(), blob(&meta.1), false));
    let tree = tree(&root);
    let mut args = vec!["commit-tree", tree.as_str(), "-m", "seed"];
    if let Some(p) = parent {
        args.extend(["-p", p]);
    }
    let commit = git(dir, &args);
    git(dir, &["update-ref", "refs/foam/data", &commit]);
    commit
}

/// Make `dir` a repository on `main` with nothing in it.
pub fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
}

/// An empty commit in `dir`.
pub fn commit(dir: &Path, msg: &str) {
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
}

/// A bare remote under `root` and two clones of it, `a` and
/// `b`, with one commit on `main` pushed from `a`.
pub fn two_clones_in(root: &Path) -> (PathBuf, PathBuf) {
    let bare = root.join("remote.git");
    git(
        root,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let a = root.join("a");
    let b = root.join("b");
    git(
        root,
        &["clone", "-q", bare.to_str().unwrap(), a.to_str().unwrap()],
    );
    commit(&a, "one");
    git(&a, &["push", "-q", "-u", "origin", "main"]);
    git(
        root,
        &["clone", "-q", bare.to_str().unwrap(), b.to_str().unwrap()],
    );
    (a, b)
}
