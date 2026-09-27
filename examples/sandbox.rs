//! A bench that builds the state an agent-usage scenario leaves
//! behind and keeps it under `.sandbox/<scenario>/` to look at.
//! Anything learned here becomes a test in `tests/cli.rs`; a
//! scenario never grows an assertion.
//!
//! `cargo run --example sandbox <scenario>|all`

#[path = "../tests/common/mod.rs"]
mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use common::*;

type Run = fn(&mut Scenario);

const SCENARIOS: [(&str, Run); 5] = [
    ("one-session", one_session),
    ("two-agents", two_agents),
    ("dead-agent", dead_agent),
    ("milestone-mid-task", milestone_mid_task),
    ("big-backlog", big_backlog),
];

fn main() {
    let name = std::env::args().nth(1).unwrap_or_default();
    let chosen: Vec<_> = SCENARIOS
        .iter()
        .filter(|(n, _)| name == "all" || *n == name)
        .collect();
    if chosen.is_empty() {
        eprintln!("usage: cargo run --example sandbox <scenario>|all");
        eprintln!("scenarios:");
        for (n, _) in SCENARIOS {
            eprintln!("  {n}");
        }
        std::process::exit(2);
    }
    build_foam();
    for (n, run) in chosen {
        let mut scenario = Scenario::new(n);
        run(&mut scenario);
        scenario.finish();
    }
}

/// `cargo run --example` builds the example alone, so the binary
/// the scenarios drive may be stale or missing without this.
fn build_foam() {
    let ok = Command::new(env!("CARGO"))
        .args(["build", "-q", "--bin", "foam"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "cargo build failed");
}

/// One scenario's directory, rebuilt from scratch, and the
/// prime texts its sessions saw.
struct Scenario {
    name: &'static str,
    root: PathBuf,
    sessions: usize,
    report: String,
}

impl Scenario {
    fn new(name: &'static str) -> Scenario {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".sandbox")
            .join(name);
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        println!("{name}: building in {}", root.display());
        Scenario {
            name,
            root,
            sessions: 0,
            report: String::new(),
        }
    }

    /// A repository with no remote, one commit in.
    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);
        commit(&dir, "one");
        dir
    }

    /// A bare `origin.git` and two clones of it.
    fn clones(&self) -> (PathBuf, PathBuf) {
        two_clones_in(&self.root)
    }

    /// Start a session as `who` in `dir`: run prime the way the
    /// SessionStart hook would and keep what it printed.
    fn session(&mut self, dir: &Path, who: &str) {
        self.sessions += 1;
        let text = stdout(actor(dir, who).arg("prime"));
        let file = format!("prime-{:02}-{who}.md", self.sessions);
        fs::write(self.root.join(&file), format!("{text}\n")).unwrap();
        let _ = writeln!(self.report, "session {} as {who}: {file}", self.sessions);
    }

    fn note(&mut self, line: impl AsRef<str>) {
        let _ = writeln!(self.report, "{}", line.as_ref());
    }

    fn finish(&self) {
        fs::write(self.root.join("report.txt"), &self.report).unwrap();
        print!("{}", self.report);
        println!("{}: done", self.name);
    }
}

/// One agent, one session: prime, claim, note, close, remember,
/// and a second session that sees the memory.
fn one_session(s: &mut Scenario) {
    let r = s.repo("repo");
    stdout(foam(&r).args(["init", "--prefix", "s"]));
    let a = stdout(foam(&r).args(["create", "Wire the parser", "-p", "1"]));
    stdout(foam(&r).args(["create", "Test the parser", "--blocked-by", &a]));
    s.session(&r, "ann");
    stdout(actor(&r, "ann").args(["claim", &a]));
    stdout(actor(&r, "ann").args(["note", &a, "tried PEG first, went with winnow"]));
    stdout(actor(&r, "ann").args(["close", &a, "--reason", "parser wired"]));
    stdout(actor(&r, "ann").args(["remember", "parser-lib", "winnow, not nom"]));
    commit(&r, "wire the parser");
    s.session(&r, "ann");
}

/// Two agents on two clones claim the same issue without seeing
/// each other, then both push through the hook.
fn two_agents(s: &mut Scenario) {
    let (a, b) = s.clones();
    stdout(foam(&a).args(["init", "--prefix", "s"]));
    let x = stdout(foam(&a).args(["create", "Shared work"]));
    stdout(foam(&a).arg("sync"));
    stdout(foam(&b).arg("init"));
    s.session(&a, "ann");
    s.session(&b, "bob");
    stdout(actor(&a, "ann").args(["claim", &x]));
    stdout(actor(&b, "bob").args(["claim", &x]));
    commit(&a, "ann's work");
    git(&a, &["push", "-q"]);
    git(&b, &["checkout", "-q", "-b", "bob"]);
    commit(&b, "bob's work");
    git(&b, &["push", "-q", "-u", "origin", "bob"]);
    s.note("after both pushed, b's view:");
    s.note(stdout(foam(&b).args(["show", &x])));
    git(&a, &["fetch", "-q"]);
    s.note("a's view after a fetch:");
    s.note(stdout(foam(&a).args(["show", &x])));
    s.session(&a, "ann");
    s.session(&b, "bob");
}

/// An agent claims and vanishes; with a zero lease the next
/// session's prime reclaims the issue.
fn dead_agent(s: &mut Scenario) {
    let r = s.repo("repo");
    stdout(foam(&r).args(["init", "--prefix", "s"]));
    stdout(foam(&r).args(["config", "lease-minutes", "0"]));
    let a = stdout(foam(&r).args(["create", "Abandoned"]));
    s.session(&r, "ann");
    stdout(actor(&r, "ann").args(["claim", &a]));
    stdout(actor(&r, "ann").args(["note", &a, "halfway"]));
    s.session(&r, "bob");
    s.note(stdout(foam(&r).args(["show", &a])));
}

/// An agent working one issue files a milestone with children for
/// what it found, and closes the milestone's first child itself.
fn milestone_mid_task(s: &mut Scenario) {
    let r = s.repo("repo");
    stdout(foam(&r).args(["init", "--prefix", "s"]));
    let a = stdout(foam(&r).args(["create", "Add the export command"]));
    s.session(&r, "ann");
    stdout(actor(&r, "ann").args(["claim", &a]));
    let milestone =
        stdout(actor(&r, "ann").args(["create", "Export formats", "--type", "milestone"]));
    let csv = stdout(actor(&r, "ann").args(["create", "CSV export", "--parent", &milestone]));
    stdout(actor(&r, "ann").args(["create", "JSON export", "--parent", &milestone]));
    stdout(actor(&r, "ann").args(["dep", "add", &a, &milestone]));
    stdout(actor(&r, "ann").args(["claim", &csv]));
    stdout(actor(&r, "ann").args(["close", &csv, "--reason", "writes rows"]));
    stdout(actor(&r, "ann").args(["note", &a, "export waits on the formats milestone"]));
    s.session(&r, "ann");
    s.note(stdout(foam(&r).arg("blocked")));
    s.note(stdout(foam(&r).args(["dep", "tree", &a])));
}

/// Two hundred issues and thirty memories: how big prime gets,
/// and how long the read commands take.
fn big_backlog(s: &mut Scenario) {
    let r = s.repo("repo");
    stdout(foam(&r).args(["init", "--prefix", "s"]));
    let start = Instant::now();
    let kinds = ["task", "bug", "feature", "chore"];
    for i in 0..200 {
        let p = (i % 5).to_string();
        stdout(foam(&r).args([
            "create",
            &format!("Issue number {i} with a title of ordinary length"),
            "--type",
            kinds[i % 4],
            "-p",
            &p,
            "--body",
            &"Body text. ".repeat(20),
        ]));
    }
    for i in 0..30 {
        stdout(foam(&r).args([
            "remember",
            &format!("fact-{i:02}"),
            &format!("memory {i}: {}", "some words ".repeat(8)),
        ]));
    }
    s.note(format!("230 writes took {:?}", start.elapsed()));
    for args in [
        vec!["prime"],
        vec!["ready"],
        vec!["list", "--all"],
        vec!["search", "number 19"],
        vec!["search", "memory 2"],
        vec!["board"],
    ] {
        let t = Instant::now();
        let out = stdout(foam(&r).args(&args));
        s.note(format!(
            "{:<20} {:>6} bytes  {:>4} lines  {:?}",
            args.join(" "),
            out.len(),
            out.lines().count(),
            t.elapsed()
        ));
    }
    s.session(&r, "ann");
}
