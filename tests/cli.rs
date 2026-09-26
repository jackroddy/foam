use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let ok = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    dir
}

fn foam(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("foam").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout(cmd: &mut Command) -> String {
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

#[test]
fn init_create_show_list() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "first", "-p", "1"]));
    assert!(a.starts_with("t-"), "{a}");
    let b = stdout(foam(dir.path()).args(["create", "second", "--blocked-by", &a]));

    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(shown.contains("second"));
    assert!(shown.contains(&a));

    let listed = stdout(foam(dir.path()).arg("list"));
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with(&a), "priority 1 sorts first: {listed}");

    let json = stdout(foam(dir.path()).args(["--json", "show", &a]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["priority"], 1);
    assert_eq!(v["status"], "open");
}

#[test]
fn nothing_touches_the_working_tree() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    foam(dir.path()).args(["create", "x"]).assert().success();
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");
    let branches = Command::new("git")
        .args(["branch", "-a"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout), "");
}

#[test]
fn exit_codes() {
    let dir = repo();
    foam(dir.path()).arg("list").assert().code(3);
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .code(1);
    foam(dir.path()).args(["show", "t-nope"]).assert().code(1);
    foam(dir.path())
        .args(["create", "x", "--blocked-by", "t-nope"])
        .assert()
        .code(1);
}

#[test]
fn concurrent_creates_all_land() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let n = 6;
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let path = dir.path().to_path_buf();
            std::thread::spawn(move || {
                foam(&path)
                    .args(["create", &format!("issue {i}")])
                    .output()
                    .unwrap()
            })
        })
        .collect();
    for h in handles {
        let out = h.join().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let listed = stdout(foam(dir.path()).arg("list"));
    assert_eq!(listed.lines().count(), n);
}
