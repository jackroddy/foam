use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::git::Push;
use crate::graph;
use crate::migrate;
use crate::model::{Issue, Memory, SCHEMA_VERSION, Status, parse_when};
use crate::ops::{self, resolve};
use crate::prime;
use crate::render::{self, Style};
use crate::store::{Absorbed, DATA_REF, Db, NOT_INITIALIZED, ORIGIN_REF, Snapshot, Store};
use crate::{Cli, Cmd, ConfigKey, DepCmd, SetupCmd};

/// The global flags, split from the subcommand so both can move.
struct Options {
    json: bool,
    actor: Option<String>,
    style: Style,
}

/// Open the board as a screen in this terminal.
#[cfg(feature = "tui")]
pub fn tui(cli: Cli) -> Result<()> {
    let store = Store::open(&cli.directory)?;
    let options = Options {
        json: false,
        actor: cli.actor,
        style: Style::PLAIN,
    };
    let actor = actor(&store, &options);
    crate::tui::run(&store, &actor)
}

pub fn run(cli: Cli, command: Cmd) -> Result<()> {
    if let Cmd::Setup {
        command: SetupCmd::Bash,
    } = command
    {
        print!("{BASH_COMPLETION}");
        return Ok(());
    }
    let store = Store::open(&cli.directory)?;
    let cli = Options {
        json: cli.json,
        actor: cli.actor,
        style: Style::detect(cli.plain || cli.json),
    };
    let result = match command {
        Cmd::Init { prefix } => init(&store, prefix),
        Cmd::Create {
            title,
            kind,
            priority,
            labels,
            parent,
            body,
            blocked_by,
        } => {
            let issue = ops::create(
                &store,
                &ops::NewIssue {
                    title,
                    body,
                    kind,
                    priority,
                    labels,
                    parent,
                    blocked_by,
                },
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&issue)?);
            } else {
                println!("{}", issue.id);
            }
            Ok(())
        }
        Cmd::Show { id } => {
            let snap = load(&store)?;
            let id = match id {
                Some(q) => resolve(&snap.db, &q)?,
                None => pick(&snap.db)?,
            };
            let issue = get(&snap.db, &id)?;
            if cli.json {
                let mut v = serde_json::to_value(issue)?;
                let children: Vec<&str> = graph::children(&snap.db, &id)
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                v["children"] = serde_json::json!(children);
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print!("{}", render::issue(&cli.style, issue, &snap.db));
            }
            Ok(())
        }
        Cmd::List {
            status,
            kind,
            label,
            assignee,
            all,
        } => {
            let snap = load(&store)?;
            let mut issues: Vec<&Issue> = snap
                .db
                .issues
                .values()
                .filter(|i| match status {
                    Some(s) => i.status == s,
                    None => all || matches!(i.status, Status::Open | Status::InProgress),
                })
                .filter(|i| kind.is_none_or(|k| i.kind == k))
                .filter(|i| label.as_ref().is_none_or(|l| i.labels.contains(l)))
                .filter(|i| {
                    assignee
                        .as_ref()
                        .is_none_or(|a| i.assignee.as_ref() == Some(a))
                })
                .collect();
            issues.sort_by_key(|i| (i.priority, i.created_at));
            print_issues(&issues, &snap.db, &cli)
        }
        Cmd::Ready { limit } => {
            let snap = load(&store)?;
            let mut issues = graph::ready(&snap.db, Timestamp::now());
            if let Some(n) = limit {
                issues.truncate(n);
            }
            print_issues(&issues, &snap.db, &cli)
        }
        Cmd::Blocked => {
            let snap = load(&store)?;
            let blocked = graph::blocked(&snap.db, Timestamp::now());
            if cli.json {
                let rows: Vec<serde_json::Value> = blocked
                    .iter()
                    .map(|(i, by)| serde_json::json!({ "issue": i, "blocked_by": by }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                let rows: Vec<(&Issue, String)> = blocked
                    .iter()
                    .map(|(i, by)| (*i, format!("waits on {}", by.join(" "))))
                    .collect();
                print!("{}", render::listing(&cli.style, Some(&snap.db), &rows));
            }
            Ok(())
        }
        Cmd::Board => board(&store, &cli),
        Cmd::Pick => {
            let snap = load(&store)?;
            let id = pick(&snap.db)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(get(&snap.db, &id)?)?);
            } else {
                println!("{id}");
            }
            Ok(())
        }
        Cmd::Update {
            ids,
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
        } => {
            let actor = actor(&store, &cli);
            let defer_until = defer_until
                .as_deref()
                .map(parse_when)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let changes = ops::Changes {
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
            };
            let issues = ops::update(&store, &ids, &actor, &changes)?;
            if cli.json {
                match issues.as_slice() {
                    [one] => println!("{}", serde_json::to_string_pretty(one)?),
                    many => println!("{}", serde_json::to_string_pretty(many)?),
                }
            } else {
                let rows: Vec<(&Issue, String)> =
                    issues.iter().map(|i| (i, String::new())).collect();
                print!("{}", render::listing(&cli.style, None, &rows));
            }
            Ok(())
        }
        Cmd::Close {
            ids,
            reason,
            dropped,
        } => {
            let closed = ops::close(&store, &ids, &reason, dropped)?;
            for (issue, open_children) in &closed {
                if !open_children.is_empty() {
                    eprintln!(
                        "foam: closing {} with open children: {}",
                        issue.id,
                        open_children.join(" ")
                    );
                }
            }
            let ids: Vec<String> = closed.into_iter().map(|(i, _)| i.id).collect();
            done(&ids, "closed", cli.json)
        }
        Cmd::Reopen { ids } => {
            let ids = ids_of(&ops::reopen(&store, &ids)?);
            done(&ids, "reopened", cli.json)
        }
        Cmd::Claim { id, force } => {
            let actor = actor(&store, &cli);
            let issue = ops::claim(&store, &id, &actor, force)?;
            print_written(&issue, &cli)
        }
        Cmd::Unclaim { id, force } => {
            let actor = actor(&store, &cli);
            let issue = ops::unclaim(&store, &id, &actor, force)?;
            print_written(&issue, &cli)
        }
        Cmd::Heartbeat { id } => {
            let actor = actor(&store, &cli);
            let issue = ops::heartbeat(&store, &id, &actor)?;
            print_written(&issue, &cli)
        }
        Cmd::Reclaim => {
            let freed = ids_of(&ops::reclaim(&store)?);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&freed)?);
            } else {
                for id in freed {
                    println!("reopened {id}");
                }
            }
            Ok(())
        }
        Cmd::Note {
            id,
            text,
            reply_to,
            edit,
            rm,
            history,
        } => note_command(
            &store,
            &cli,
            &id,
            NoteArgs {
                text,
                reply_to,
                edit,
                rm,
                history,
            },
        ),
        Cmd::Search { query } => {
            let snap = load(&store)?;
            let q = query.to_lowercase();
            let hit = |s: &str| s.to_lowercase().contains(&q);
            let mut issues: Vec<&Issue> = snap
                .db
                .issues
                .values()
                .filter(|i| hit(&i.title) || hit(&i.body) || i.notes.iter().any(|n| hit(&n.text)))
                .collect();
            issues.sort_by_key(|i| (i.status == Status::Closed, i.priority, i.created_at));
            let memories: Vec<&Memory> = snap
                .db
                .memories
                .values()
                .filter(|m| hit(&m.slug) || hit(&m.text))
                .collect();
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "issues": issues,
                        "memories": memories,
                    }))?
                );
            } else {
                print_issues(&issues, &snap.db, &cli)?;
                print!("{}", render::memories(&cli.style, &store.git, &memories));
            }
            Ok(())
        }
        Cmd::Remember { slug, text } => {
            let memory = ops::remember(&store, &slug, &text)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&memory)?);
            } else {
                println!("remembered {slug}");
            }
            Ok(())
        }
        Cmd::Memories => {
            let snap = load(&store)?;
            let memories: Vec<&Memory> = snap.db.memories.values().collect();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&memories)?);
            } else {
                print!("{}", render::memories(&cli.style, &store.git, &memories));
            }
            Ok(())
        }
        Cmd::Recall { slug } => {
            let snap = load(&store)?;
            let m = snap
                .db
                .memories
                .get(&slug)
                .with_context(|| format!("no such memory: {slug}"))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(m)?);
            } else {
                print!("{}", render::memory(&cli.style, &store.git, m));
            }
            Ok(())
        }
        Cmd::Forget { slug } => {
            ops::forget(&store, &slug)?;
            done(std::slice::from_ref(&slug), "forgot", cli.json)
        }
        Cmd::Prime { hook_json, limit } => {
            let Some(snap) = store.load()? else {
                if hook_json {
                    // a hook must not fail a session in a repo
                    // that never ran foam init
                    return Ok(());
                }
                bail!("{NOT_INITIALIZED}");
            };
            // the hook pipes in JSON with the session id; the
            // Bash tool exports the same id, so the actor prime
            // names is the one later commands resolve to
            let session = if hook_json { hook_session_id() } else { None };
            if hook_json && session.is_none() {
                eprintln!("foam: the SessionStart hook input carried no session_id");
            }
            let actor = actor_in_session(&store, &cli, session.as_deref());
            let mut snap = snap;
            let reclaimed = if snap
                .db
                .issues
                .values()
                .any(|i| ops::lease_expired(i, Timestamp::now()))
            {
                let freed = ids_of(&ops::reclaim(&store)?);
                snap = load(&store)?;
                freed
            } else {
                Vec::new()
            };
            let text = prime::render(&snap.db, &store.git, &actor, limit, &reclaimed);
            if hook_json {
                println!("{}", prime::hook_json(&text));
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Cmd::Config { key, value } => {
            let show =
                |meta: &crate::model::Meta, key: Option<ConfigKey>, json: bool| -> Result<()> {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "lease-minutes": meta.lease_minutes,
                                "stale-after": meta.stale_after,
                            }))?
                        );
                        return Ok(());
                    }
                    match key {
                        Some(ConfigKey::LeaseMinutes) => println!("{}", meta.lease_minutes),
                        Some(ConfigKey::StaleAfter) => println!("{}", meta.stale_after),
                        None => {
                            println!("lease-minutes  {}", meta.lease_minutes);
                            println!("stale-after    {}", meta.stale_after);
                        }
                    }
                    Ok(())
                };
            match (key, value) {
                (Some(key), Some(value)) => {
                    let meta = ops::config_set(&store, key, value)?;
                    show(&meta, Some(key), cli.json)
                }
                (key, None) => show(&load(&store)?.db.meta, key, cli.json),
                (None, Some(_)) => bail!("a value needs a key"),
            }
        }
        Cmd::SessionEnd => {
            if !store.exists()? {
                return Ok(());
            }
            let session = hook_session_id();
            let actor = actor_in_session(&store, &cli, session.as_deref());
            let released = ids_of(&ops::session_end(&store, &actor)?);
            done(&released, "released", cli.json)
        }
        Cmd::Setup { command } => match command {
            SetupCmd::Claude { remove } => setup_claude(&store, remove),
            SetupCmd::Bash => unreachable!("handled before the store opens"),
        },
        Cmd::Sync { remote, setup } => sync(&store, &remote, setup),
        Cmd::Doctor => doctor(&store, cli.json),
        Cmd::Stats => stats(&store, cli.json),
        Cmd::Log { id, limit } => {
            load(&store)?;
            let entries = store.git.log(DATA_REF, limit, id.as_deref())?;
            if cli.json {
                let rows: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|(oid, when, msg)| serde_json::json!({ "commit": oid, "at": when, "message": msg }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print!("{}", render::log(&cli.style, &entries));
            }
            Ok(())
        }
        Cmd::Dep { command } => match command {
            DepCmd::Add { id, blocker } => {
                let issue = ops::dep_add(&store, &id, &blocker)?;
                print_written(&issue, &cli)
            }
            DepCmd::Rm { id, blocker } => {
                let issue = ops::dep_rm(&store, &id, &blocker)?;
                print_written(&issue, &cli)
            }
            DepCmd::Tree { id } => {
                let snap = load(&store)?;
                let id = resolve(&snap.db, &id)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&graph::tree_json(&snap.db, &id))?
                    );
                } else {
                    print!("{}", graph::tree(&snap.db, &id));
                }
                Ok(())
            }
        },
    };
    if let Some(from) = store.upgraded_from() {
        let text = format!(
            "foam: this write took the data ref from schema {from} to {SCHEMA_VERSION}; foam older than {} refuses it from now on",
            migrate::introduced(SCHEMA_VERSION)
        );
        if cli.style.color {
            eprintln!("\x1b[1;33m{text}\x1b[0m");
        } else {
            eprintln!("{text}");
        }
    }
    result
}

fn init(store: &Store, prefix: Option<String>) -> Result<()> {
    if store.exists()? {
        bail!("foam is already initialized here");
    }
    let prefix = match prefix {
        Some(p) => p,
        None => {
            let dir = store.git.toplevel()?;
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.to_lowercase()
                        .replace(|c: char| !c.is_alphanumeric(), "-")
                })
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "foam".to_string())
        }
    };
    let remote = "origin";
    let has_remote = store.git.remote_url(remote).is_some();
    let remote_data = if has_remote {
        match store.git.ls_remote(remote, DATA_REF) {
            Ok(has) => Some(has),
            Err(e) => {
                eprintln!(
                    "foam: could not reach {remote} ({e:#}); the data stays in this clone until `foam sync --setup` publishes it"
                );
                None
            }
        }
    } else {
        None
    };
    if remote_data == Some(true) {
        store.git.fetch(remote, &format!("{DATA_REF}:{DATA_REF}"))?;

        // sync's hook and doctor take this ref as the mark of a
        // clone that shares its data with origin
        if let Some(tip) = store.git.rev_parse(DATA_REF)? {
            store.git.update_ref(ORIGIN_REF, &tip, None)?;
        }
        println!("fetched {DATA_REF} from {remote}");
    } else {
        store.init(&prefix)?;
        println!("initialized {DATA_REF} with prefix {prefix}");
    }
    if remote_data == Some(true) {
        add_refspec(store, remote)?;
        install_hook(store, true)?;
    } else if remote_data == Some(false) {
        println!(
            "{remote} has no {DATA_REF}, so the data stays in this clone; `foam sync --setup` publishes it to {remote}"
        );
    } else if !has_remote {
        println!(
            "no remote named {remote}, so the data stays in this clone; `foam sync --setup` publishes it once there is one"
        );
    }
    println!("run `foam setup claude` to load context into Claude Code at session start");
    Ok(())
}

/// What `setup bash` prints: fzf's own completion hook, fed the
/// listing, with the id cut out of the chosen line.
const BASH_COMPLETION: &str = "\
# foam: `foam show **<TAB>` searches the issues in fzf and inserts the id.
# Load fzf's bash integration first: eval \"$(fzf --bash)\"
_fzf_complete_foam() {
  _fzf_complete --reverse -- \"$@\" < <(foam list --plain)
}
_fzf_complete_foam_post() {
  awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^[a-z0-9-]+-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]$/) { print $i; exit } }'
}
[ -n \"$BASH\" ] && type _fzf_complete >/dev/null 2>&1 \\
  && complete -F _fzf_complete_foam -o default -o bashdefault foam
";

/// Let the person choose among the open and in-progress issues in
/// fzf and return the id.
fn pick(db: &Db) -> Result<String> {
    let mut issues: Vec<&Issue> = db
        .issues
        .values()
        .filter(|i| matches!(i.status, Status::Open | Status::InProgress))
        .collect();
    issues.sort_by_key(|i| (i.priority, i.created_at));
    let feed = render::listing(&Style::PLAIN, Some(db), &plain_rows(&issues));
    let mut fzf = match std::process::Command::new("fzf")
        .args(["--reverse", "--no-multi", "--prompt", "issue> "])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("fzf is not on PATH, so pass an id")
        }
        Err(e) => return Err(e).context("fzf"),
    };
    // fzf closes its stdin once the person has chosen, so a
    // write after that is not a failure
    let _ = std::io::Write::write_all(&mut fzf.stdin.take().unwrap(), feed.as_bytes());
    let out = fzf.wait_with_output()?;
    if !out.status.success() {
        bail!("nothing picked");
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find(|w| db.issues.contains_key(*w))
        .map(str::to_string)
        .context("nothing picked")
}

/// The hooks `setup claude` installs, by event.
const CLAUDE_HOOKS: [(&str, &str); 2] = [
    ("SessionStart", "foam prime --hook-json"),
    ("SessionEnd", "foam session-end"),
];

/// Add or remove the SessionStart hook in `.claude/settings.json`,
/// leaving everything else in the file as it was.
fn setup_claude(store: &Store, remove: bool) -> Result<()> {
    let path = store.git.toplevel()?.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| path.display().to_string()),
    };
    if !settings.is_object() {
        bail!("{} does not hold a JSON object", path.display());
    }
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        bail!("{}: \"hooks\" is not an object", path.display());
    }
    let mut changed = false;
    for (event, command) in CLAUDE_HOOKS {
        let entries = hooks
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(entries) = entries.as_array_mut() else {
            bail!("{}: \"{event}\" is not an array", path.display());
        };
        let is_ours = |entry: &serde_json::Value| {
            entry["hooks"]
                .as_array()
                .is_some_and(|hs| hs.iter().any(|h| h["command"].as_str() == Some(command)))
        };
        let present = entries.iter().any(is_ours);
        if remove && present {
            entries.retain(|e| !is_ours(e));
            changed = true;
        } else if !remove && !present {
            entries.push(serde_json::json!({
                "matcher": "",
                "hooks": [{ "type": "command", "command": command }],
            }));
            changed = true;
        }
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(&settings)?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes)?;
    match (remove, changed) {
        (true, true) => println!("removed the foam hooks from {}", path.display()),
        (true, false) => println!("no foam hooks in {}", path.display()),
        (false, true) => println!("added the foam hooks to {}", path.display()),
        (false, false) => println!("the foam hooks are already in {}", path.display()),
    }
    Ok(())
}

const HOOK_MARK: &str = "# foam: push the data ref alongside code";

const PRE_PUSH: &str = "\
#!/bin/sh
# foam: push the data ref alongside code
cat >/dev/null
[ -n \"$FOAM_IN_HOOK\" ] && exit 0
command -v foam >/dev/null 2>&1 || exit 0
git remote get-url \"$1\" >/dev/null 2>&1 || exit 0
git rev-parse -q --verify refs/foam/data >/dev/null || exit 0
FOAM_IN_HOOK=1 foam sync --remote \"$1\" || exit 1
";

/// Add the fetch refspec for the data ref to `remote`.
fn add_refspec(store: &Store, remote: &str) -> Result<()> {
    let key = format!("remote.{remote}.fetch");
    let spec = format!("+{DATA_REF}:{ORIGIN_REF}");
    if !store.git.config_all(&key).contains(&spec) {
        store.git.config_add(&key, &spec)?;
        println!("added the fetch refspec for {DATA_REF} to {remote}");
    }
    Ok(())
}

/// Install the pre-push hook. With `advise`, say how to do it
/// by hand where it cannot be installed.
fn install_hook(store: &Store, advise: bool) -> Result<()> {
    let hook = store.git.hooks_dir()?.join("pre-push");
    if store.git.hooks_redirected() {
        if advise {
            eprintln!(
                "foam: core.hooksPath is set, so no hook was installed; add this to {}:\n{}",
                hook.display(),
                PRE_PUSH.trim_start_matches("#!/bin/sh\n")
            );
        }
        return Ok(());
    }
    match std::fs::read_to_string(&hook) {
        Ok(existing) if existing.contains(HOOK_MARK) => {}
        Ok(existing) if !is_shell(&existing) => {
            if advise {
                eprintln!(
                    "foam: {} is not a shell script, so it was left alone; run this from it:\n{}",
                    hook.display(),
                    PRE_PUSH.trim_start_matches("#!/bin/sh\n")
                );
            }
        }
        Ok(existing) => {
            // keep whatever was there and run ours after it,
            // minus our own shebang line
            let ours = PRE_PUSH.trim_start_matches("#!/bin/sh\n");
            std::fs::write(&hook, format!("{existing}\n{ours}"))?;
            println!("appended the foam block to {}", hook.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(hook.parent().unwrap())?;
            std::fs::write(&hook, PRE_PUSH)?;
            println!("installed {}", hook.display());
        }
        Err(e) => return Err(e).with_context(|| hook.display().to_string()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Whether a hook script's shebang names a Bourne-style shell.
fn is_shell(script: &str) -> bool {
    match script.lines().next() {
        Some(first) if first.starts_with("#!") => {
            first.ends_with("sh") || first.contains("sh ") || first.ends_with("bash")
        }
        // no shebang: git runs it through sh
        _ => true,
    }
}

/// Fetch, merge and push the data ref, retrying while someone
/// else keeps pushing first. Only `publish` puts the ref on a
/// remote that lacks it.
fn sync(store: &Store, remote: &str, publish: bool) -> Result<()> {
    if store.git.remote_url(remote).is_none() {
        bail!("no remote named {remote}");
    }
    let in_hook = std::env::var_os("FOAM_IN_HOOK").is_some();

    // a clone that has never fetched or pushed the ref has not
    // published it, so its hook lets the push through without
    // reaching the network
    if in_hook && store.git.rev_parse(ORIGIN_REF)?.is_none() {
        return Ok(());
    }

    // git sends a hook's stdout to stderr, between its own
    // lines, so there every line says it is foam's
    let say = |line: String| {
        if in_hook {
            eprintln!("foam: {line}");
        } else {
            println!("{line}");
        }
    };
    eprintln!("foam: syncing {DATA_REF} with {remote}");
    for _ in 0..3 {
        if store
            .git
            .fetch_if_present(remote, &format!("+{DATA_REF}:{ORIGIN_REF}"))?
        {
            add_refspec(store, remote)?;
            install_hook(store, publish)?;
        } else if !publish {
            if in_hook {
                return Ok(());
            }
            let flag = if remote == "origin" {
                String::new()
            } else {
                format!(" --remote {remote}")
            };
            bail!(
                "{remote} has no {DATA_REF}, so the data stays in this clone; `foam sync --setup{flag}` publishes it there"
            );
        }
        match store.absorb_origin()? {
            Absorbed::Nothing => {}
            Absorbed::FastForward => say(format!("fast-forwarded to {remote}")),
            Absorbed::Merged { conflicts: 0 } => say(format!("merged {remote}")),
            Absorbed::Merged { conflicts } => say(format!(
                "merged {remote}, settling {conflicts} record(s) both sides changed"
            )),
        }
        let Some(data) = store.git.rev_parse(DATA_REF)? else {
            bail!("{NOT_INITIALIZED}");
        };
        if store.git.rev_parse(ORIGIN_REF)?.as_deref() == Some(data.as_str()) {
            if !in_hook {
                println!("{DATA_REF} is up to date with {remote}");
            }
            return Ok(());
        }
        eprintln!("foam: pushing {DATA_REF} to {remote}");
        match store.git.push(remote, &format!("{DATA_REF}:{DATA_REF}"))? {
            Push::Done => {
                say(format!("pushed {DATA_REF} to {remote}"));

                // the remote now holds this tip, which the hook and
                // doctor otherwise learn from the next fetch
                let seen = store.git.rev_parse(ORIGIN_REF)?;
                store.git.update_ref(ORIGIN_REF, &data, seen.as_deref())?;
                add_refspec(store, remote)?;
                install_hook(store, publish)?;
                return Ok(());
            }
            Push::Rejected => eprintln!("foam: {remote} moved, fetching again"),
        }
    }
    bail!("could not push {DATA_REF}: {remote} kept moving")
}

/// Report what is wrong, if anything, and exit 1 if something is.
/// How many of the largest records stats lists.
const LARGEST: usize = 5;

fn stats(store: &Store, json: bool) -> Result<()> {
    let schema = load(store)?.db.meta.schema_version;
    let size = store.git.ref_size(DATA_REF)?;
    let mut files = store.git.tree_sizes(DATA_REF)?;
    let tip_bytes: u64 = files.iter().map(|(_, b)| b).sum();
    let under = |dir: &str| files.iter().filter(|(p, _)| p.starts_with(dir)).count();
    let (issues, memories) = (under("issues/"), under("memories/"));
    let total_files = files.len();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    files.truncate(LARGEST);
    let days = size
        .since
        .as_deref()
        .and_then(|s| s.parse::<Timestamp>().ok())
        .map(|t| Timestamp::now().duration_since(t).as_hours() / 24);
    if json {
        let largest: Vec<serde_json::Value> = files
            .iter()
            .map(|(path, bytes)| serde_json::json!({ "path": path, "bytes": bytes }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ref": DATA_REF,
                "schema": { "ref": schema, "foam": SCHEMA_VERSION },
                "commits": size.commits,
                "since": size.since,
                "days": days,
                "objects": {
                    "only": size.only_objects,
                    "only_bytes": size.only_bytes,
                    "shared": size.shared_objects,
                    "shared_bytes": size.shared_bytes,
                },
                "tip": {
                    "files": total_files,
                    "bytes": tip_bytes,
                    "issues": issues,
                    "memories": memories,
                },
                "largest": largest,
            }))?
        );
        return Ok(());
    }
    let since = match (&size.since, days) {
        (Some(s), Some(d)) => format!(" since {}, {d} days", &s[..10.min(s.len())]),
        _ => String::new(),
    };
    println!("{DATA_REF:<16} {} commits{since}", size.commits);
    println!(
        "{:<16} {schema}, this foam reads {SCHEMA_VERSION}",
        "schema"
    );
    println!(
        "{:<16} {} held only by the ref, {} on disk",
        "objects",
        size.only_objects,
        render::bytes(size.only_bytes)
    );
    println!(
        "{:<16} {} shared with the branches, {}",
        "",
        size.shared_objects,
        render::bytes(size.shared_bytes)
    );
    println!(
        "{:<16} {} files, {}: {issues} issues, {memories} memories",
        "tip",
        total_files,
        render::bytes(tip_bytes)
    );
    for (n, (path, bytes)) in files.iter().enumerate() {
        println!(
            "{:<16} {path:<28} {}",
            if n == 0 { "largest" } else { "" },
            render::bytes(*bytes)
        );
    }
    println!(
        "{:<16} on-disk sizes are before git gc packs the objects",
        ""
    );
    Ok(())
}

fn doctor(store: &Store, json: bool) -> Result<()> {
    let snap = load(store)?;
    let db = &snap.db;
    let now = Timestamp::now();
    if !json && db.meta.schema_version < SCHEMA_VERSION {
        println!(
            "the ref is schema {}; this foam writes {SCHEMA_VERSION}, so the next write upgrades it and foam older than {} refuses it after that",
            db.meta.schema_version,
            migrate::introduced(SCHEMA_VERSION)
        );
    }
    let mut problems: Vec<String> = Vec::new();
    let mut report = |line: String| problems.push(line);
    for i in db.issues.values() {
        for b in &i.blocked_by {
            if !db.issues.contains_key(b) {
                report(format!("{}: waits on {b}, which does not exist", i.id));
            }
        }
        if let Some(p) = i.parent.as_ref().filter(|p| !db.issues.contains_key(*p)) {
            report(format!("{}: parent {p} does not exist", i.id));
        }
    }
    for id in graph::parent_loops(db) {
        report(format!(
            "{id}: its parent chain loops back to it; `foam update {id} --parent \"\"` breaks it"
        ));
    }
    for i in db.issues.values() {
        if i.status == Status::InProgress && i.lease_expires.is_none_or(|t| t <= now) {
            report(format!(
                "{}: in progress with an expired lease; `foam reclaim` reopens it",
                i.id
            ));
        }
        let ahead = now + jiff::SignedDuration::from_mins(5);
        if i.updated_at > ahead {
            report(format!(
                "{}: updated_at is in the future; a clock is wrong somewhere",
                i.id
            ));
        }
    }
    let rules = claude_md(store)?;
    for m in db.memories.values() {
        if m.updated_at > now + jiff::SignedDuration::from_mins(5) {
            report(format!("memory {}: updated_at is in the future", m.slug));
        }
        let text = squeeze(&m.text);
        if text.chars().count() >= DUPLICATE_MIN_CHARS
            && let Some(file) = rules.iter().find(|(_, rule)| rule.contains(&text))
        {
            report(format!(
                "memory {}: its text is also in {}, so it lands in every session twice; `foam forget {}` drops the copy",
                m.slug, file.0, m.slug
            ));
        }
    }
    // a clone that has never fetched or pushed the ref keeps
    // its data local and needs neither the refspec nor the hook
    if store.git.remote_url("origin").is_some() && store.git.rev_parse(ORIGIN_REF)?.is_some() {
        let spec = format!("+{DATA_REF}:{ORIGIN_REF}");
        if !store.git.config_all("remote.origin.fetch").contains(&spec) {
            report("origin has no fetch refspec for the data ref; `foam sync` adds it".into());
        }
        let hook = store.git.hooks_dir()?.join("pre-push");
        if !std::fs::read_to_string(&hook).is_ok_and(|h| h.contains(HOOK_MARK)) {
            report("no foam pre-push hook; `foam sync` installs it".into());
        }
    }
    if session_gap() {
        report(format!(
            "{CLAUDE_MARK} is set but {CLAUDE_SESSION} is not; every Claude Code session gets the same actor"
        ));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&problems)?);
    } else if problems.is_empty() {
        println!(
            "ok: {} issue(s), {} memor{}",
            db.issues.len(),
            db.memories.len(),
            if db.memories.len() == 1 { "y" } else { "ies" }
        );
    } else {
        for p in &problems {
            println!("{p}");
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{} problem(s)", problems.len())
    }
}

/// The most ready issues and log entries the board shows.
fn plain_rows<'a>(v: &[&'a Issue]) -> Vec<(&'a Issue, String)> {
    v.iter().map(|i| (*i, String::new())).collect()
}

/// One screen of where the backlog stands.
fn board(store: &Store, cli: &Options) -> Result<()> {
    let snap = load(store)?;
    let db = &snap.db;
    let graph::Board {
        milestones,
        in_progress,
        ready,
        blocked,
    } = graph::board(db, Timestamp::now());
    let log = store.git.log(DATA_REF, graph::BOARD_LOG, None)?;
    if cli.json {
        let blocked: Vec<serde_json::Value> = blocked
            .iter()
            .map(|(i, by)| serde_json::json!({ "issue": i, "blocked_by": by }))
            .collect();
        let log: Vec<serde_json::Value> = log
            .iter()
            .map(
                |(oid, when, msg)| serde_json::json!({ "commit": oid, "at": when, "message": msg }),
            )
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "milestones": milestones,
                "in_progress": in_progress,
                "ready": ready,
                "blocked": blocked,
                "log": log,
            }))?
        );
        return Ok(());
    }
    let style = &cli.style;
    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let stamp = store.git.head_stamp()?;
    println!(
        "{}  on {} at {}  {}",
        style.bold(&db.meta.prefix),
        stamp.branch,
        stamp.commit,
        style.dim(&format!(
            "{} open, {} in progress, {} deferred, {} closed",
            count(Status::Open),
            count(Status::InProgress),
            count(Status::Deferred),
            count(Status::Closed),
        ))
    );
    if !milestones.is_empty() {
        println!("\n{}", style.bold("Milestones"));
        print!(
            "{}",
            render::listing(style, Some(db), &plain_rows(&milestones))
        );
    }
    if !in_progress.is_empty() {
        println!("\n{}", style.bold("In progress"));
        let rows: Vec<(&Issue, String)> = in_progress
            .iter()
            .map(|i| (*i, render::lease_left(i)))
            .collect();
        print!("{}", render::listing(style, Some(db), &rows));
    }
    if ready.len() > graph::BOARD_READY {
        println!(
            "\n{}",
            style.bold(&format!(
                "Ready ({} of {})",
                graph::BOARD_READY,
                ready.len()
            ))
        );
    } else {
        println!("\n{}", style.bold("Ready"));
    }
    if ready.is_empty() {
        println!("nothing is ready");
    }
    print!(
        "{}",
        render::listing(
            style,
            Some(db),
            &plain_rows(&ready[..ready.len().min(graph::BOARD_READY)])
        )
    );
    if !blocked.is_empty() {
        println!("\n{}", style.bold("Blocked"));
        let rows: Vec<(&Issue, String)> = blocked
            .iter()
            .map(|(i, by)| (*i, format!("waits on {}", by.join(" "))))
            .collect();
        print!("{}", render::listing(style, Some(db), &rows));
    }
    if !log.is_empty() {
        println!("\n{}", style.bold("Recent"));
        print!("{}", render::log(style, &log));
    }
    Ok(())
}

/// The rule files Claude Code loads at session start, relative
/// to the repository; the person's own under `~/.claude` joins them.
const RULE_FILES: [&str; 3] = ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"];

/// A memory shorter than this is a word or two that any rules
/// file may hold by chance, so doctor does not match it.
const DUPLICATE_MIN_CHARS: usize = 16;

fn claude_md(store: &Store) -> Result<Vec<(String, String)>> {
    let top = store.git.toplevel()?;
    let mut files: Vec<(String, PathBuf)> = RULE_FILES
        .iter()
        .map(|name| (name.to_string(), top.join(name)))
        .collect();
    if let Some(home) = std::env::home_dir() {
        files.push(("~/.claude/CLAUDE.md".into(), home.join(".claude/CLAUDE.md")));
    }
    Ok(files
        .into_iter()
        .filter_map(|(name, path)| {
            std::fs::read_to_string(path)
                .ok()
                .map(|text| (name, squeeze(&text)))
        })
        .collect())
}

/// Lowercase with every run of whitespace one space, so a memory
/// matches a rule that was only rewrapped.
fn squeeze(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn ids_of(issues: &[Issue]) -> Vec<String> {
    issues.iter().map(|i| i.id.clone()).collect()
}

/// Print what a write did to several ids.
fn done(ids: &[String], verb: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(ids)?);
    } else {
        for id in ids {
            println!("{verb} {id}");
        }
    }
    Ok(())
}

fn load(store: &Store) -> Result<Snapshot> {
    store.load()?.context(NOT_INITIALIZED)
}

fn get<'a>(db: &'a Db, id: &str) -> Result<&'a Issue> {
    db.issues
        .get(id)
        .with_context(|| format!("no such issue: {id}"))
}

/// The session id from a Claude Code hook's JSON on stdin, when
/// stdin is a pipe and carries one.
fn hook_session_id() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v["session_id"].as_str().map(str::to_string))
}

/// Set in every process Claude Code's Bash tool runs.
//
// both names were read off a live session's environment,
// not from documentation; if a Claude Code release drops
// or renames one, session_gap() is what notices
const CLAUDE_MARK: &str = "CLAUDECODE";
const CLAUDE_SESSION: &str = "CLAUDE_CODE_SESSION_ID";

/// Whether this is a Claude Code session with no session id,
/// which would give every session the same actor.
fn session_gap() -> bool {
    std::env::var_os(CLAUDE_MARK).is_some() && std::env::var_os(CLAUDE_SESSION).is_none()
}

fn actor(store: &Store, cli: &Options) -> String {
    let actor = actor_in_session(store, cli, None);
    if cli.actor.is_none() && std::env::var_os("FOAM_ACTOR").is_none() && session_gap() {
        eprintln!(
            "foam: no session id from Claude Code; acting as {actor}, pass --actor to keep sessions apart"
        );
    }
    actor
}

/// Who is acting. An explicit `--actor` or `$FOAM_ACTOR` is taken
/// whole; otherwise the git user gets a session suffix, so two
/// sessions of one person do not share their claims.
fn actor_in_session(store: &Store, cli: &Options, session: Option<&str>) -> String {
    if let Some(a) = cli
        .actor
        .clone()
        .or_else(|| std::env::var("FOAM_ACTOR").ok())
    {
        return a;
    }
    let base = store
        .git
        .config("user.name")
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let session = std::env::var(CLAUDE_SESSION)
        .ok()
        .or_else(|| session.map(str::to_string));
    match session {
        Some(s) if !s.is_empty() => format!("{base}/{}", s.chars().take(8).collect::<String>()),
        _ => base,
    }
}

/// Print an issue a write just committed.
/// What `foam note` was asked to do beyond naming the issue.
struct NoteArgs {
    text: Option<String>,
    reply_to: Option<String>,
    edit: Option<String>,
    rm: Option<String>,
    history: Option<String>,
}

fn note_command(store: &Store, cli: &Options, id: &str, args: NoteArgs) -> Result<()> {
    let NoteArgs {
        text,
        reply_to,
        edit,
        rm,
        history,
    } = args;
    if let Some(note) = history {
        let snap = load(store)?;
        let versions = ops::note_history(store, &snap.db, id, &note)?;
        if cli.json {
            let versions: Vec<serde_json::Value> = versions
                .iter()
                .map(|(commit, at, text)| {
                    serde_json::json!({ "commit": commit, "at": at, "text": text })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&versions)?);
        } else {
            for (commit, at, text) in &versions {
                let when = at
                    .parse::<Timestamp>()
                    .map(|t| cli.style.when(t))
                    .unwrap_or_else(|_| at.clone());
                println!(
                    "{}  {text}",
                    cli.style.dim(&format!("[{when} {}]", &commit[..7]))
                );
            }
        }
        return Ok(());
    }
    let actor = actor(store, cli);
    let issue = if let Some(note) = rm {
        ops::rm_note(store, id, &note)?
    } else {
        let Some(text) = text else {
            bail!("say what the note should say");
        };
        match edit {
            Some(note) => ops::edit_note(store, id, &note, &text)?,
            None => ops::note(store, id, &actor, &text, reply_to.as_deref())?,
        }
    };
    print_written(&issue, cli)
}

fn print_written(issue: &Issue, cli: &Options) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issue)?);
    } else {
        let rows = [(issue, String::new())];
        print!("{}", render::listing(&cli.style, None, &rows));
    }
    Ok(())
}

/// Print issues, each held one trailing `<- a b` for its open
/// blockers, so a listing shows the whole graph.
fn print_issues(issues: &[&Issue], db: &Db, cli: &Options) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(issues)?);
    } else {
        let rows: Vec<(&Issue, String)> = issues
            .iter()
            .map(|i| {
                let by = graph::open_blockers(db, i);
                let suffix = if by.is_empty() {
                    String::new()
                } else {
                    format!("waits on {}", by.join(" "))
                };
                (*i, suffix)
            })
            .collect();
        print!("{}", render::listing(&cli.style, Some(db), &rows));
    }
    Ok(())
}
