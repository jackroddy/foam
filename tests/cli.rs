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

#[test]
fn prime_reports_state_and_wraps_as_hook_json() {
    let dir = repo();
    commit(dir.path(), "one");
    // an uninitialized repo is not an error for the hook
    foam(dir.path())
        .args(["prime", "--hook-json"])
        .assert()
        .success()
        .stdout("");
    foam(dir.path()).arg("prime").assert().code(3);

    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let a = stdout(foam(dir.path()).args(["create", "first", "-p", "0"]));
    let b = stdout(foam(dir.path()).args(["create", "second", "--blocked-by", &a]));
    let c = stdout(foam(dir.path()).args(["create", "mine"]));
    stdout(foam(dir.path()).args(["--actor", "ann", "claim", &c]));
    stdout(foam(dir.path()).args(["remember", "style", "tabs not spaces"]));

    let text = stdout(foam(dir.path()).args(["--actor", "ann", "prime"]));
    assert!(text.starts_with("# foam\n"), "{text}");
    assert!(text.contains("## Commands"));
    assert!(text.contains("2 open, 1 in progress, 0 deferred, 0 closed; on main at "));
    assert!(text.contains(&format!("{c}  P2  mine  (lease 1")), "{text}");
    assert!(text.contains("## Ready (1 total)"));
    assert!(text.contains(&format!("{a}  P0  task  first")));
    assert!(!text.contains(&b));
    assert!(text.ends_with("style: tabs not spaces"), "{text}");

    let text = stdout(foam(dir.path()).args(["--actor", "bob", "prime", "--limit", "0"]));
    assert!(!text.contains("## In progress"));
    assert!(!text.contains("first"));

    let json = stdout(foam(dir.path()).args(["prime", "--hook-json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.starts_with("# foam\n"));
    assert!(
        json.lines().count() == 1,
        "one line, so a hook reads it whole"
    );
}

#[test]
fn setup_claude_merges_into_settings() {
    let dir = repo();
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let path = dir.path().join(".claude/settings.json");
    assert!(!path.exists(), "init leaves the working tree alone");

    stdout(foam(dir.path()).args(["setup", "claude"]));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "foam prime --hook-json"
    );

    // adding again is a no-op; other settings survive both ways
    std::fs::write(
        &path,
        r#"{"permissions":{"allow":["Bash(ls)"]},"hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"foam prime --hook-json"}]},{"matcher":"","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
    )
    .unwrap();
    stdout(foam(dir.path()).args(["setup", "claude"]));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");

    stdout(foam(dir.path()).args(["setup", "claude", "--remove"]));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo hi"
    );
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");

    std::fs::write(&path, "not json").unwrap();
    foam(dir.path()).args(["setup", "claude"]).assert().code(1);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
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

/// A bare remote and two clones of it, each with one commit on main.
fn two_clones() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let bare = root.path().join("remote.git");
    git(
        root.path(),
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let a = root.path().join("a");
    let b = root.path().join("b");
    git(
        root.path(),
        &["clone", "-q", bare.to_str().unwrap(), a.to_str().unwrap()],
    );
    commit(&a, "one");
    git(&a, &["push", "-q", "-u", "origin", "main"]);
    git(
        root.path(),
        &["clone", "-q", bare.to_str().unwrap(), b.to_str().unwrap()],
    );
    (root, a, b)
}

#[test]
fn sync_moves_data_between_clones_and_merges_it() {
    let (_root, a, b) = two_clones();
    // init in a: origin exists but has no data yet
    let out = stdout(foam(&a).args(["init", "--prefix", "t"]));
    assert!(out.contains("initialized"), "{out}");
    assert!(out.contains("fetch refspec"), "{out}");
    assert!(a.join(".git/hooks/pre-push").exists());
    let x = stdout(foam(&a).args(["create", "from a"]));
    stdout(foam(&a).arg("sync"));

    // init in b adopts the remote's data instead of starting fresh
    let out = stdout(foam(&b).args(["init"]));
    assert!(out.contains("fetched"), "{out}");
    assert!(stdout(foam(&b).arg("list")).contains("from a"));

    // each side adds an issue and edits the same one differently
    let y = stdout(foam(&b).args(["create", "from b"]));
    stdout(foam(&b).args(["--actor", "bee", "note", &x, "seen in b"]));
    stdout(foam(&b).args(["remember", "shared", "b says"]));
    stdout(foam(&b).arg("sync"));

    stdout(foam(&a).args(["update", &x, "--add-label", "urgent"]));
    stdout(foam(&a).args(["remember", "shared", "a says"]));
    let out = stdout(foam(&a).arg("sync"));
    assert!(out.contains("merged origin, settling"), "{out}");
    let listed = stdout(foam(&a).arg("list"));
    assert!(listed.contains(&x) && listed.contains(&y), "{listed}");
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(&a).args(["--json", "show", &x]))).unwrap();
    assert_eq!(v["labels"], serde_json::json!(["urgent"]));
    assert_eq!(v["notes"][0]["text"], "seen in b");
    // both wrote the memory with no base; a wrote last
    assert_eq!(
        stdout(foam(&a).args(["recall", "shared"])).lines().next(),
        Some("a says")
    );

    // b sees the merge after a plain git fetch, with no foam sync
    git(&b, &["fetch", "-q"]);
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(&b).args(["--json", "show", &x]))).unwrap();
    assert_eq!(v["labels"], serde_json::json!(["urgent"]));
    assert_eq!(
        stdout(foam(&b).args(["recall", "shared"])).lines().next(),
        Some("a says")
    );
    assert_eq!(
        git(&b, &["rev-parse", "refs/foam/data"]),
        git(&a, &["rev-parse", "refs/foam/data"])
    );

    // the merge commit has both parents
    let parents = git(&a, &["log", "-1", "--format=%P", "refs/foam/data"]);
    assert_eq!(parents.split(' ').count(), 2, "{parents}");
    stdout(foam(&a).arg("doctor"));
    stdout(foam(&b).arg("doctor"));
}

#[test]
fn git_push_carries_the_data_ref_through_the_hook() {
    let (root, a, b) = two_clones();
    stdout(foam(&a).args(["init", "--prefix", "t"]));
    stdout(foam(&a).args(["create", "hooked"]));
    commit(&a, "two");
    // the hook runs foam from PATH
    let bin = Command::cargo_bin("foam").unwrap();
    let path = format!(
        "{}:{}",
        bin.get_program()
            .to_str()
            .unwrap()
            .rsplit_once('/')
            .unwrap()
            .0,
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("git")
        .args(["push", "-q"])
        .env("PATH", path)
        .current_dir(&a)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bare = root.path().join("remote.git");
    git(&bare, &["rev-parse", "refs/foam/data"]);
    stdout(foam(&b).arg("init"));
    assert!(stdout(foam(&b).arg("list")).contains("hooked"));
}

#[test]
fn doctor_reports_problems() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    stdout(foam(dir.path()).args(["create", "fine"]));
    assert!(stdout(foam(dir.path()).arg("doctor")).starts_with("ok:"));

    // hand-write a record with a dangling blocker and a dead lease
    let json = stdout(foam(dir.path()).args(["--json", "create", "broken"]));
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    v["blocked_by"] = serde_json::json!(["t-nope"]);
    v["status"] = serde_json::json!("in_progress");
    v["lease_expires"] = serde_json::json!("2000-01-01T00:00:00Z");
    let blob = git(dir.path(), &["hash-object", "-w", "--stdin"]);
    let _ = blob;
    // easier: rebuild the tree through git plumbing
    let tmp = dir.path().join("rec.json");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let oid = git(dir.path(), &["hash-object", "-w", tmp.to_str().unwrap()]);
    std::fs::remove_file(&tmp).unwrap();
    let tree = git(dir.path(), &["ls-tree", "refs/foam/data"]);
    let issues_tree = tree
        .lines()
        .find(|l| l.ends_with("\tissues"))
        .unwrap()
        .split(' ')
        .nth(2)
        .unwrap()
        .split('\t')
        .next()
        .unwrap()
        .to_string();
    let issues = git(dir.path(), &["ls-tree", &issues_tree]);
    let mut entries: Vec<String> = issues
        .lines()
        .filter(|l| !l.ends_with(&format!("\t{id}.json")))
        .map(str::to_string)
        .collect();
    entries.push(format!("100644 blob {oid}\t{id}.json"));
    let mk = |input: String| {
        let mut child = Command::new("git")
            .args(["mktree"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let new_issues = mk(entries.join("\n") + "\n");
    let root_entries: Vec<String> = tree
        .lines()
        .map(|l| {
            if l.ends_with("\tissues") {
                format!("040000 tree {new_issues}\tissues")
            } else {
                l.to_string()
            }
        })
        .collect();
    let new_root = mk(root_entries.join("\n") + "\n");
    let old = git(dir.path(), &["rev-parse", "refs/foam/data"]);
    let commit = git(
        dir.path(),
        &["commit-tree", &new_root, "-p", &old, "-m", "tamper"],
    );
    git(dir.path(), &["update-ref", "refs/foam/data", &commit, &old]);

    let out = foam(dir.path()).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("t-nope, which does not exist"), "{text}");
    assert!(text.contains("expired lease"), "{text}");
    // the dangling blocker does not hold the issue back
    assert!(stdout(foam(dir.path()).arg("reclaim")).contains(&id));
    assert!(stdout(foam(dir.path()).arg("ready")).contains(&id));
}

#[test]
fn log_and_json_on_writes() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    let b = stdout(foam(dir.path()).args(["create", "b"]));
    stdout(foam(dir.path()).args(["dep", "add", &b, &a]));
    let closed = stdout(foam(dir.path()).args(["--json", "close", &a, &b]));
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&closed).unwrap(),
        [a.clone(), b.clone()]
    );
    stdout(foam(dir.path()).args(["remember", "m", "x"]));
    let forgot = stdout(foam(dir.path()).args(["--json", "forget", "m"]));
    assert_eq!(serde_json::from_str::<Vec<String>>(&forgot).unwrap(), ["m"]);

    let log = stdout(foam(dir.path()).arg("log"));
    assert!(log.lines().next().unwrap().ends_with("forget m"), "{log}");
    assert!(log.lines().last().unwrap().ends_with("init"), "{log}");
    let log_a = stdout(foam(dir.path()).args(["log", &a]));
    // create a, dep b <- a, close a b
    assert_eq!(log_a.lines().count(), 3, "{log_a}");
    let v: serde_json::Value = serde_json::from_str(&stdout(
        foam(dir.path()).args(["--json", "log", "--limit", "1"]),
    ))
    .unwrap();
    assert_eq!(v[0]["message"], "forget m");

    let tree: serde_json::Value = serde_json::from_str(&stdout(
        foam(dir.path()).args(["--json", "dep", "tree", &b]),
    ))
    .unwrap();
    assert_eq!(tree["blocked_by"][0]["id"], a);
    assert_eq!(tree["blocked_by"][0]["status"], "closed");
    let problems: Vec<String> =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "doctor"]))).unwrap();
    assert!(problems.is_empty());
}
