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

#[test]
fn ready_and_blocked_follow_the_graph() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    let b = stdout(foam(dir.path()).args(["create", "b"]));
    let c = stdout(foam(dir.path()).args(["create", "c"]));
    foam(dir.path())
        .args(["dep", "add", &b, &a])
        .assert()
        .success();
    foam(dir.path())
        .args(["dep", "add", &c, &b])
        .assert()
        .success();

    // a cycle is refused, and so is waiting on yourself
    foam(dir.path())
        .args(["dep", "add", &a, &c])
        .assert()
        .code(1);
    foam(dir.path())
        .args(["dep", "add", &a, &a])
        .assert()
        .code(1);

    let ready = stdout(foam(dir.path()).arg("ready"));
    assert_eq!(ready.lines().count(), 1);
    assert!(ready.starts_with(&a));
    let blocked = stdout(foam(dir.path()).arg("blocked"));
    assert_eq!(blocked.lines().count(), 2);
    assert!(blocked.contains(&format!("<- {a}")));

    stdout(foam(dir.path()).args(["close", &a, "--reason", "done"]));
    let ready = stdout(foam(dir.path()).arg("ready"));
    assert!(ready.starts_with(&b), "{ready}");
    let tree = stdout(foam(dir.path()).args(["dep", "tree", &c]));
    assert_eq!(tree.lines().count(), 3);
    assert!(tree.lines().nth(2).unwrap().contains("closed"));

    foam(dir.path())
        .args(["dep", "rm", &c, &b])
        .assert()
        .success();
    let ready = stdout(foam(dir.path()).arg("ready"));
    assert_eq!(ready.lines().count(), 2);

    foam(dir.path()).args(["close", &a]).assert().code(1);
    stdout(foam(dir.path()).args(["reopen", &a]));
    foam(dir.path()).args(["reopen", &a]).assert().code(1);
}

#[test]
fn update_changes_fields_and_deferral() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    let json = stdout(foam(dir.path()).args([
        "--json",
        "update",
        &a,
        "--title",
        "renamed",
        "-p",
        "0",
        "--add-label",
        "x",
        "--add-label",
        "y",
        "--rm-label",
        "x",
        "--assignee",
        "me",
    ]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["title"], "renamed");
    assert_eq!(v["priority"], 0);
    assert_eq!(v["labels"], serde_json::json!(["y"]));
    assert_eq!(v["assignee"], "me");

    stdout(foam(dir.path()).args(["update", &a, "--defer-until", "2999-01-01"]));
    assert_eq!(stdout(foam(dir.path()).arg("ready")), "");
    let listed = stdout(foam(dir.path()).arg("list"));
    assert_eq!(listed, "", "deferred issues are hidden by default");
    stdout(foam(dir.path()).args(["update", &a, "--defer-until", "2000-01-01"]));
    assert_eq!(stdout(foam(dir.path()).arg("ready")).lines().count(), 1);

    stdout(foam(dir.path()).args(["update", &a, "--status", "open", "--assignee", ""]));
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", &a]))).unwrap();
    assert_eq!(v["defer_until"], serde_json::Value::Null);
    assert_eq!(v["assignee"], serde_json::Value::Null);
    foam(dir.path())
        .args(["update", &a, "--defer-until", "soon"])
        .assert()
        .code(1);
    foam(dir.path())
        .args(["update", &a, "--parent", &a])
        .assert()
        .code(1);
}

#[test]
fn claims_and_leases() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "a"]));

    let json = stdout(foam(dir.path()).args(["--json", "--actor", "ann", "claim", &a]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["status"], "in_progress");
    assert_eq!(v["assignee"], "ann");
    assert!(v["lease_expires"].is_string());
    assert_eq!(stdout(foam(dir.path()).arg("ready")), "");

    // another actor cannot take or release it while the lease holds
    foam(dir.path())
        .args(["--actor", "bob", "claim", &a])
        .assert()
        .code(1);
    foam(dir.path())
        .args(["--actor", "bob", "unclaim", &a])
        .assert()
        .code(1);
    foam(dir.path())
        .args(["--actor", "bob", "heartbeat", &a])
        .assert()
        .code(1);
    stdout(foam(dir.path()).args(["--actor", "ann", "heartbeat", &a]));

    // nothing has expired, so reclaim frees nothing
    assert_eq!(stdout(foam(dir.path()).arg("reclaim")), "");

    let json = stdout(foam(dir.path()).args(["--json", "--actor", "bob", "claim", &a, "--force"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["assignee"], "bob");

    stdout(foam(dir.path()).args(["--actor", "bob", "unclaim", &a]));
    assert_eq!(stdout(foam(dir.path()).arg("ready")).lines().count(), 1);
    foam(dir.path()).args(["unclaim", &a]).assert().code(1);

    stdout(foam(dir.path()).args(["close", &a]));
    foam(dir.path()).args(["claim", &a]).assert().code(1);
}

#[test]
fn actor_falls_back_to_the_environment() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    let json = stdout(
        foam(dir.path())
            .env("FOAM_ACTOR", "env-actor")
            .args(["--json", "claim", &a]),
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["assignee"], "env-actor");
}

fn commit(dir: &Path, msg: &str) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            msg,
        ])
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

#[test]
fn notes_search_and_memories() {
    let dir = repo();
    commit(dir.path(), "one");
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "Wire the parser", "--body", "uses nom"]));
    let b = stdout(foam(dir.path()).args(["create", "Other"]));
    stdout(foam(dir.path()).args(["--actor", "ann", "note", &a, "Tried PEG first"]));

    let shown = stdout(foam(dir.path()).args(["show", &a]));
    assert!(shown.contains("ann] Tried PEG first"), "{shown}");
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", &a]))).unwrap();
    assert_eq!(v["notes"][0]["branch"], "main");

    assert!(stdout(foam(dir.path()).args(["search", "peg"])).starts_with(&a));
    assert!(stdout(foam(dir.path()).args(["search", "NOM"])).starts_with(&a));
    assert!(stdout(foam(dir.path()).args(["search", "other"])).starts_with(&b));
    assert_eq!(stdout(foam(dir.path()).args(["search", "zzz"])), "");
    stdout(foam(dir.path()).args(["close", &b]));
    assert!(stdout(foam(dir.path()).args(["search", "other"])).contains("closed"));

    stdout(foam(dir.path()).args(["remember", "parser-lib", "we use nom"]));
    foam(dir.path())
        .args(["remember", "Bad Slug", "x"])
        .assert()
        .code(1);
    assert_eq!(
        stdout(foam(dir.path()).args(["recall", "parser-lib"]))
            .lines()
            .next(),
        Some("we use nom")
    );
    let listed = stdout(foam(dir.path()).arg("memories"));
    assert!(listed.contains("this commit"), "{listed}");

    commit(dir.path(), "two");
    commit(dir.path(), "three");
    let listed = stdout(foam(dir.path()).arg("memories"));
    assert!(listed.contains("2 commits ago"), "{listed}");

    // a memory written on a branch that main never merged
    let ok = Command::new("git")
        .args(["checkout", "-q", "-b", "side"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    commit(dir.path(), "side work");
    stdout(foam(dir.path()).args(["remember", "side-note", "from side"]));
    let ok = Command::new("git")
        .args(["checkout", "-q", "main"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let listed = stdout(foam(dir.path()).arg("memories"));
    assert!(listed.contains("side-note  from side  (at "), "{listed}");
    assert!(
        listed.contains("on side, not in this branch's history"),
        "{listed}"
    );

    stdout(foam(dir.path()).args(["remember", "parser-lib", "we use winnow"]));
    let v: serde_json::Value = serde_json::from_str(&stdout(foam(dir.path()).args([
        "--json",
        "recall",
        "parser-lib",
    ])))
    .unwrap();
    assert_eq!(v["text"], "we use winnow");
    assert_ne!(v["created_at"], v["updated_at"]);

    stdout(foam(dir.path()).args(["forget", "parser-lib"]));
    foam(dir.path())
        .args(["recall", "parser-lib"])
        .assert()
        .code(1);
    foam(dir.path())
        .args(["forget", "parser-lib"])
        .assert()
        .code(1);
}
