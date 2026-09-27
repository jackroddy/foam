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
