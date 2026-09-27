mod common;

use std::process::Command;

use assert_cmd::prelude::*;
use common::*;

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    dir
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
    // free text may start with a dash
    stdout(foam(dir.path()).args(["update", &b, "--body", "--reason is optional"]));
    stdout(foam(dir.path()).args(["note", &b, "-v flag"]));
    stdout(foam(dir.path()).args(["remember", "dash", "--json everywhere"]));
    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(
        shown.contains("--reason is optional") && shown.contains("] -v flag"),
        "{shown}"
    );

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

    foam(dir.path())
        .args(["close", &a, "--reason", "again"])
        .assert()
        .code(1);
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

    stdout(foam(dir.path()).args(["close", &a, "--reason", "done"]));
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

    // a Claude Code session suffixes the git user, so two sessions
    // of one person hold separate claims
    let session = |id: &str| {
        let mut c = foam(dir.path());
        c.env("CLAUDE_CODE_SESSION_ID", id);
        c
    };
    stdout(foam(dir.path()).args(["--actor", "env-actor", "unclaim", &a]));
    let v: serde_json::Value = serde_json::from_str(&stdout(
        session("11111111-2222").args(["--json", "claim", &a]),
    ))
    .unwrap();
    let assignee = v["assignee"].as_str().unwrap().to_string();
    assert!(assignee.ends_with("/11111111"), "{assignee}");
    session("33333333-4444")
        .args(["claim", &a])
        .assert()
        .code(1);
    session("11111111-2222")
        .args(["heartbeat", &a])
        .assert()
        .success();

    // prime under the hook reads the session id from stdin
    let mut child = foam(dir.path())
        .args(["prime", "--hook-json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        child.stdin.as_mut().unwrap(),
        br#"{"session_id":"11111111-2222","hook_event_name":"SessionStart"}"#,
    )
    .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&format!("acting as {assignee}")), "{text}");
    assert!(
        text.contains(&format!("## In progress for {assignee}")),
        "{text}"
    );

    // the SessionEnd hook releases what the session held and
    // leaves a note saying so; other holders are untouched
    let b = stdout(foam(dir.path()).args(["create", "b"]));
    stdout(foam(dir.path()).args(["--actor", "ann", "claim", &b]));
    let mut child = foam(dir.path())
        .arg("session-end")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        child.stdin.as_mut().unwrap(),
        br#"{"session_id":"11111111-2222","hook_event_name":"SessionEnd","reason":"other"}"#,
    )
    .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("released {a}")
    );
    let shown = stdout(foam(dir.path()).args(["show", &a]));
    assert!(
        shown.contains("status: open") && shown.contains("released at session end"),
        "{shown}"
    );
    assert!(stdout(foam(dir.path()).args(["show", &b])).contains("assignee: ann"));
    let plain = repo();
    foam(plain.path())
        .arg("session-end")
        .assert()
        .success()
        .stdout("");

    // inside Claude Code with no session id, foam says so rather
    // than silently letting every session share one actor
    let out = foam(dir.path())
        .env("CLAUDECODE", "1")
        .args(["note", &a, "hello"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no session id from Claude Code"), "{err}");
    let out = foam(dir.path())
        .env("CLAUDECODE", "1")
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("CLAUDE_CODE_SESSION_ID is not"));
    assert!(stdout(foam(dir.path()).arg("doctor")).starts_with("ok:"));
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
    foam(dir.path()).args(["close", &b]).assert().code(2);
    stdout(foam(dir.path()).args(["close", &b, "--reason", "no longer wanted", "--dropped"]));
    let found = stdout(foam(dir.path()).args(["search", "other"]));
    assert!(
        found.contains("closed") && found.contains("[dropped]"),
        "{found}"
    );
    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(shown.contains("  dropped  no longer wanted"), "{shown}");
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", &b]))).unwrap();
    assert_eq!(v["resolution"], "dropped");
    stdout(foam(dir.path()).args(["reopen", &b]));
    foam(dir.path())
        .args(["update", &b, "--status", "closed"])
        .assert()
        .code(1);
    stdout(foam(dir.path()).args(["close", &b, "--reason", "done after all"]));
    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(shown.contains("  done  done after all"), "{shown}");
    // a resolution or reason can be corrected after the fact,
    // but only on a closed issue
    stdout(foam(dir.path()).args([
        "update",
        &b,
        "--resolution",
        "dropped",
        "--reason",
        "on reflection, no",
    ]));
    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(shown.contains("  dropped  on reflection, no"), "{shown}");
    foam(dir.path())
        .args(["update", &a, "--resolution", "done"])
        .assert()
        .code(1);
    stdout(foam(dir.path()).args(["remember", "peg-hole", "round pegs only"]));
    let found = stdout(foam(dir.path()).args(["search", "PEG"]));
    assert!(
        found.starts_with(&a) && found.contains("peg-hole  round pegs only"),
        "{found}"
    );
    let found = stdout(foam(dir.path()).args(["search", "round"]));
    assert!(found.starts_with("peg-hole"), "{found}");
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "search", "peg"]))).unwrap();
    assert_eq!(v["issues"][0]["id"], a);
    assert_eq!(v["memories"][0]["slug"], "peg-hole");

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
    assert_eq!(
        v["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
        "foam session-end"
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
    assert_eq!(v["hooks"]["SessionEnd"].as_array().unwrap().len(), 0);
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo hi"
    );
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");

    std::fs::write(&path, "not json").unwrap();
    foam(dir.path()).args(["setup", "claude"]).assert().code(1);
}

/// A bare remote and two clones of it, each with one commit on main.
fn two_clones() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = two_clones_in(root.path());
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
    // the hook runs foam from PATH, which git() sets to this build
    git(&a, &["push", "-q"]);
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
    let closed = stdout(foam(dir.path()).args(["--json", "close", &a, &b, "--reason", "done"]));
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

#[test]
fn two_clones_that_both_ran_init_still_merge() {
    let (_root, a, b) = two_clones();
    stdout(foam(&a).args(["init", "--prefix", "aa"]));
    stdout(foam(&b).args(["init", "--prefix", "bb"]));
    let x = stdout(foam(&a).args(["create", "from a"]));
    let y = stdout(foam(&b).args(["create", "from b"]));
    stdout(foam(&a).arg("sync"));
    let out = stdout(foam(&b).arg("sync"));
    assert!(out.contains("merged origin"), "{out}");
    let listed = stdout(foam(&b).arg("list"));
    assert!(listed.contains(&x) && listed.contains(&y), "{listed}");
    // b merged first, so its meta.json stood; a then
    // fast-forwards onto that merge and takes the prefix too
    assert!(stdout(foam(&b).args(["create", "later"])).starts_with("bb-"));
    git(&a, &["fetch", "-q"]);
    assert!(stdout(foam(&a).arg("list")).contains(&y));
    assert!(stdout(foam(&a).args(["create", "later"])).starts_with("bb-"));
}

#[test]
fn update_status_goes_through_claims_and_deferral() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    stdout(foam(dir.path()).args(["--actor", "ann", "claim", &a]));
    let v: serde_json::Value = serde_json::from_str(&stdout(
        foam(dir.path()).args(["--json", "update", &a, "--status", "open"]),
    ))
    .unwrap();
    assert_eq!(v["assignee"], serde_json::Value::Null);
    assert_eq!(v["lease_expires"], serde_json::Value::Null);

    let v: serde_json::Value = serde_json::from_str(&stdout(foam(dir.path()).args([
        "--json",
        "--actor",
        "bob",
        "update",
        &a,
        "--status",
        "in_progress",
    ])))
    .unwrap();
    assert_eq!(v["assignee"], "bob");
    assert!(v["lease_expires"].is_string());
    assert_eq!(stdout(foam(dir.path()).arg("reclaim")), "");

    foam(dir.path())
        .args(["update", &a, "--status", "deferred"])
        .assert()
        .code(1);
}

#[test]
fn init_offline_and_from_a_subdirectory() {
    let dir = repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "/nonexistent/remote.git"],
    );
    let sub = dir.path().join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    let out = foam(&sub).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("could not reach origin"));
    let id = stdout(foam(&sub).args(["create", "x"]));
    let prefix = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_lowercase();
    let prefix: String = prefix
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    assert!(id.starts_with(&format!("{prefix}-")), "{id} vs {prefix}");
}

#[test]
fn hook_install_respects_other_hooks() {
    // a redirected hooks path is left alone
    let dir = repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "/nonexistent/remote.git"],
    );
    let elsewhere = dir.path().join("myhooks");
    std::fs::create_dir_all(&elsewhere).unwrap();
    git(
        dir.path(),
        &["config", "core.hooksPath", elsewhere.to_str().unwrap()],
    );
    let out = foam(dir.path()).arg("init").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("core.hooksPath is set"));
    assert!(!elsewhere.join("pre-push").exists());
    assert!(!dir.path().join(".git/hooks/pre-push").exists());

    // a python hook is left alone; a shell hook gets the block appended
    let dir = repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "/nonexistent/remote.git"],
    );
    let hook = dir.path().join(".git/hooks/pre-push");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
    let out = foam(dir.path()).arg("init").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a shell script"));
    assert_eq!(
        std::fs::read_to_string(&hook).unwrap(),
        "#!/usr/bin/env python3\nprint('hi')\n"
    );
    std::fs::write(&hook, "#!/bin/bash\necho hi\n").unwrap();
    // the remote is unreachable, so the sync itself fails;
    // the setup half has already run
    let _ = foam(dir.path()).args(["sync", "--setup"]).output().unwrap();
    let text = std::fs::read_to_string(&hook).unwrap();
    assert!(text.starts_with("#!/bin/bash\necho hi\n"), "{text}");
    assert!(text.contains("foam sync --remote"), "{text}");
    assert!(text.contains("command -v foam"), "{text}");
}

#[test]
fn large_databases_load() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    // enough records that cat-file's input and output both
    // exceed a pipe buffer
    let body = "x".repeat(2000);
    for i in 0..60 {
        stdout(foam(dir.path()).args(["create", &format!("issue {i}"), "--body", &body]));
    }
    assert_eq!(stdout(foam(dir.path()).arg("list")).lines().count(), 60);
}

#[test]
fn a_closed_pipe_ends_the_process_quietly() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    for i in 0..50 {
        stdout(foam(dir.path()).args(["create", &format!("issue {i}")]));
    }
    let out = Command::new("bash")
        .args([
            "-c",
            &format!("{} list | head -c 1 >/dev/null", foam_bin().display()),
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "{err}");
    assert!(out.status.success());
}

#[test]
fn an_milestone_shows_its_children_and_waits_on_them() {
    let dir = repo();
    commit(dir.path(), "one");
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let milestone =
        stdout(foam(dir.path()).args(["create", "big", "--type", "milestone", "-p", "0"]));
    let a = stdout(foam(dir.path()).args(["create", "one", "--parent", &milestone]));
    let b = stdout(foam(dir.path()).args(["create", "two", "--parent", &milestone]));

    let shown = stdout(foam(dir.path()).args(["show", &milestone]));
    assert!(shown.contains("children (0/2 closed):\n"), "{shown}");
    let listed = stdout(foam(dir.path()).arg("list"));
    assert!(listed.contains("big  [0/2 closed]"), "{listed}");
    assert!(shown.contains(&format!("  {a}  open  one\n")), "{shown}");
    let v: serde_json::Value = serde_json::from_str(&stdout(
        foam(dir.path()).args(["--json", "show", &milestone]),
    ))
    .unwrap();
    assert_eq!(v["children"], serde_json::json!([a, b]));

    let ready = stdout(foam(dir.path()).arg("ready"));
    assert_eq!(ready.lines().count(), 2, "{ready}");
    assert!(!ready.contains(&milestone));
    let blocked = stdout(foam(dir.path()).arg("blocked"));
    assert!(blocked.starts_with(&milestone), "{blocked}");
    assert!(blocked.ends_with(&format!("<- {a} {b}")), "{blocked}");

    let text = stdout(foam(dir.path()).args(["prime", "--limit", "1"]));
    assert!(
        text.contains("## Ready (1 of 2 shown; `foam ready` lists all)"),
        "{text}"
    );
    assert!(text.contains(&a) && !text.contains(&b));
    let text = stdout(foam(dir.path()).args(["prime", "--limit", "2"]));
    assert!(text.contains("## Ready (2 total)"), "{text}");

    stdout(foam(dir.path()).args(["close", &a, "--reason", "done"]));
    let blocked = stdout(foam(dir.path()).arg("blocked"));
    assert!(blocked.contains("big  [1/2 closed]  <-"), "{blocked}");
    stdout(foam(dir.path()).args(["close", &b, "--reason", "done"]));
    let ready = stdout(foam(dir.path()).arg("ready"));
    assert!(
        ready.starts_with(&milestone) && ready.contains("[2/2 closed]"),
        "{ready}"
    );
    assert_eq!(stdout(foam(dir.path()).arg("blocked")), "");
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(text.contains("milestone  big  [2/2 closed]"), "{text}");
}

#[test]
fn config_sets_the_lease_and_prime_reclaims_expired_ones() {
    let dir = repo();
    commit(dir.path(), "one");
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    assert_eq!(
        stdout(foam(dir.path()).arg("config")),
        "lease-minutes  15\nstale-after    50"
    );
    assert_eq!(
        stdout(foam(dir.path()).args(["config", "lease-minutes", "0"])),
        "0"
    );
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "config"]))).unwrap();
    assert_eq!(v["lease-minutes"], 0);
    assert_eq!(v["stale-after"], 50);
    foam(dir.path())
        .args(["config", "lease-minutes"])
        .assert()
        .stdout("0\n");

    let a = stdout(foam(dir.path()).args(["create", "a"]));
    stdout(foam(dir.path()).args(["--actor", "ann", "claim", &a]));
    let text = stdout(foam(dir.path()).args(["--actor", "bob", "prime"]));
    assert!(
        text.contains(&format!(
            "Reopened 1 issue(s) whose lease had expired: {a}\n"
        )),
        "{text}"
    );
    assert!(text.contains("1 open, 0 in progress"), "{text}");
    assert!(text.contains(&format!("{a}  P2  task  a")), "{text}");
    let text = stdout(foam(dir.path()).args(["--actor", "bob", "prime"]));
    assert!(!text.contains("Reopened"), "{text}");

    // the memories section stops at its byte budget, dropping
    // the least recently updated first
    for i in 0..11 {
        let text = format!("{i}{}", "x".repeat(400));
        stdout(foam(dir.path()).args(["remember", &format!("m{i}"), &text]));
    }
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(
        text.contains("1 more not shown; `foam memories` lists all"),
        "{text}"
    );
    assert!(!text.contains("m0: "), "{text}");
    assert!(text.contains("m1: ") && text.contains("m10: "), "{text}");
    let out = foam(dir.path())
        .args(["remember", "long", &"y".repeat(513)])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("the most is 512"));

    stdout(foam(dir.path()).args(["config", "stale-after", "0"]));
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(text.contains("m4: 4xxx"), "{text}");
    assert!(text.contains("this commit]"), "{text}");
}

#[test]
fn concurrent_claims_admit_exactly_one() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let a = stdout(foam(dir.path()).args(["create", "a"]));
    let n = 6;
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let path = dir.path().to_path_buf();
            let a = a.clone();
            std::thread::spawn(move || {
                foam(&path)
                    .args(["--actor", &format!("agent{i}"), "claim", &a])
                    .output()
                    .unwrap()
            })
        })
        .collect();
    let mut won = 0;
    for h in handles {
        let out = h.join().unwrap();
        if out.status.success() {
            won += 1;
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("is held by"), "{err}");
        }
    }
    assert_eq!(won, 1);
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", &a]))).unwrap();
    assert_eq!(v["status"], "in_progress");
    assert!(v["assignee"].as_str().unwrap().starts_with("agent"));
}

#[test]
fn board_shows_every_section_and_plain_matches_a_pipe() {
    let dir = repo();
    commit(dir.path(), "one");
    foam(dir.path())
        .args(["init", "--prefix", "t"])
        .assert()
        .success();
    let milestone = stdout(foam(dir.path()).args(["create", "big", "--type", "milestone"]));
    let a = stdout(foam(dir.path()).args(["create", "one", "--parent", &milestone]));
    let b = stdout(foam(dir.path()).args(["create", "two", "--blocked-by", &a]));
    stdout(foam(dir.path()).args(["claim", &a, "--actor", "ann"]));

    let board = stdout(foam(dir.path()).arg("board"));
    assert!(board.starts_with("t  on main at "), "{board}");
    assert!(
        board.contains("2 open, 1 in progress, 0 deferred, 0 closed"),
        "{board}"
    );
    for section in [
        "\nMilestones\n",
        "\nIn progress\n",
        "\nReady\n",
        "\nBlocked\n",
        "\nRecent\n",
    ] {
        assert!(board.contains(section), "{section:?} missing from {board}");
    }
    assert!(
        board.contains(&format!("{a}  P2  in_progress  one  @ann  lease ")),
        "{board}"
    );
    assert!(
        board.contains(&format!("{b}  P2  open  two  <- {a}")),
        "{board}"
    );
    assert!(board.contains(&format!("claim {a} by ann")), "{board}");
    assert!(!board.contains("\x1b["), "{board}");

    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "board"]))).unwrap();
    assert_eq!(v["milestones"][0]["id"], milestone);
    assert_eq!(v["in_progress"][0]["id"], a);
    assert_eq!(v["ready"].as_array().unwrap().len(), 0);
    assert_eq!(v["blocked"][1]["issue"]["id"], b);
    assert!(v["log"].as_array().unwrap().len() >= 4);

    // a pipe already gets the plain form, so --plain changes nothing here
    for args in [["list"], ["board"], ["memories"]] {
        let piped = stdout(foam(dir.path()).args(args));
        let plain = stdout(foam(dir.path()).arg("--plain").args(args));
        assert_eq!(piped, plain);
    }
    let shown = stdout(foam(dir.path()).args(["--plain", "show", &a]));
    assert!(shown.contains("  on main ("), "{shown}");
}

#[test]
fn the_old_epic_name_is_still_accepted() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let m = stdout(foam(dir.path()).args(["create", "old name", "--type", "epic"]));
    let v: serde_json::Value =
        serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", &m]))).unwrap();
    assert_eq!(v["type"], "milestone");
    let listed = stdout(foam(dir.path()).args(["list", "--type", "milestone"]));
    assert!(listed.contains("old name"), "{listed}");
}

#[test]
fn prime_says_what_to_do_with_old_memories() {
    let dir = repo();
    commit(dir.path(), "one");
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    stdout(foam(dir.path()).args(["remember", "fresh", "still true"]));
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(!text.contains("marked [at ..]"), "{text}");
    stdout(foam(dir.path()).args(["config", "stale-after", "0"]));
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(
        text.contains("1 marked [at ..] is old or from another branch: check each"),
        "{text}"
    );
    assert!(text.contains("`foam forget <slug>` if not."), "{text}");
    stdout(foam(dir.path()).args(["remember", "other", "also old"]));
    let text = stdout(foam(dir.path()).arg("prime"));
    assert!(text.contains("2 marked [at ..] are old"), "{text}");
}

#[test]
fn doctor_flags_a_memory_that_repeats_claude_md() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        "# rules\n\nRun `cargo fmt`\nbefore committing.\n",
    )
    .unwrap();
    stdout(foam(dir.path()).args(["remember", "fmt", "run `cargo fmt` before committing"]));
    stdout(foam(dir.path()).args(["remember", "other", "the parser is winnow"]));
    let out = foam(dir.path()).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("memory fmt: its text is also in CLAUDE.md"),
        "{text}"
    );
    assert!(text.contains("`foam forget fmt`"), "{text}");
    assert!(!text.contains("memory other"), "{text}");
    stdout(foam(dir.path()).args(["forget", "fmt"]));
    assert!(stdout(foam(dir.path()).arg("doctor")).starts_with("ok:"));
}

#[test]
fn an_id_prefix_or_a_title_word_names_an_issue() {
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let a = stdout(foam(dir.path()).args(["create", "Wire the parser"]));
    let b = stdout(foam(dir.path()).args(["create", "Test the parser"]));
    let hex = &a[2..];
    let short = &hex[..3];
    let by_prefix = |q: &str| -> String {
        let v: serde_json::Value =
            serde_json::from_str(&stdout(foam(dir.path()).args(["--json", "show", q]))).unwrap();
        v["id"].as_str().unwrap().to_string()
    };
    // the digits, with or without the prefix, and a title word
    assert_eq!(by_prefix(short), a);
    assert_eq!(by_prefix(&format!("t-{short}")), a);
    assert_eq!(by_prefix("wire"), a);
    assert_eq!(by_prefix("TEST THE"), b);

    let out = foam(dir.path()).args(["show", "parser"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("parser could be any of 2:"), "{err}");
    assert!(err.contains(&a) && err.contains(&b), "{err}");
    let out = foam(dir.path()).args(["show", "zzz"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such issue: zzz"));

    // a closed issue yields to an open one with the same word
    stdout(foam(dir.path()).args(["close", "wire", "--reason", "done"]));
    assert_eq!(by_prefix("parser"), b);
    // and every other id argument resolves the same way
    stdout(foam(dir.path()).args(["--actor", "ann", "claim", "test"]));
    stdout(foam(dir.path()).args(["note", short, "seen"]));
    stdout(foam(dir.path()).args(["dep", "add", "test", short]));
    let shown = stdout(foam(dir.path()).args(["show", &b]));
    assert!(shown.contains(&format!("  {a}  closed")), "{shown}");
    assert!(stdout(foam(dir.path()).args(["show", "wire"])).contains("seen"));
}

#[test]
fn setup_bash_prints_the_completion_block_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let text = stdout(foam(dir.path()).args(["setup", "bash"]));
    assert!(text.contains("complete -F _fzf_complete_foam"), "{text}");
    assert!(text.contains("foam list --all --plain"), "{text}");
}

#[cfg(unix)]
#[test]
fn show_and_pick_take_the_issue_fzf_chooses() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo();
    stdout(foam(dir.path()).args(["init", "--prefix", "t"]));
    let a = stdout(foam(dir.path()).args(["create", "chosen", "-p", "0"]));
    stdout(foam(dir.path()).args(["create", "other"]));
    // a stand-in fzf that picks the first line it is offered
    let fake = dir.path().join("bin");
    std::fs::create_dir(&fake).unwrap();
    std::fs::write(fake.join("fzf"), "#!/bin/sh\nread -r line && echo \"$line\"\n").unwrap();
    std::fs::set_permissions(fake.join("fzf"), std::fs::Permissions::from_mode(0o755)).unwrap();
    // foam needs git, and nothing else, so PATH holds git alone
    // besides the stand-in
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let git = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    std::os::unix::fs::symlink(git.trim(), tools.join("git")).unwrap();
    let bin_dir = foam_bin().parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}:{}",
        fake.display(),
        bin_dir.display(),
        tools.display()
    );
    assert_eq!(stdout(foam(dir.path()).env("PATH", &path).arg("pick")), a);
    let shown = stdout(foam(dir.path()).env("PATH", &path).arg("show"));
    assert!(shown.starts_with(&format!("{a}  chosen")), "{shown}");

    // without fzf, both say so
    let bare = format!("{}:{}", bin_dir.display(), tools.display());
    for args in [vec!["show"], vec!["pick"]] {
        let out = foam(dir.path())
            .env("PATH", &bare)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("fzf is not on PATH"));
    }
}
