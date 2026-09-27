mod cmd;
mod git;
mod graph;
mod merge;
mod model;
mod prime;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::model::{Kind, Status};

/// An issue tracker and agent memory that lives on a git ref
#[derive(Parser)]
#[command(name = "foam", version)]
struct Cli {
    /// Run as if started in this directory
    #[arg(short = 'C', long, global = true, default_value = ".")]
    directory: PathBuf,

    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,

    /// Who is acting; defaults to $FOAM_ACTOR, then git user.name or $USER with a Claude Code session suffix
    #[arg(long, global = true)]
    actor: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the data ref in this repository
    Init {
        /// Issue id prefix; defaults to the directory name
        #[arg(long)]
        prefix: Option<String>,
    },
    /// Create an issue
    Create {
        title: String,
        /// Kind of issue
        #[arg(long = "type", value_enum, default_value_t = Kind::Task)]
        kind: Kind,
        /// 0 blocks all other work, 1 this session, 2 soon, 3 when convenient, 4 someday
        #[arg(long, short, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: u8,
        /// Add a label; repeatable
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Parent epic
        #[arg(long)]
        parent: Option<String>,
        /// Longer description
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        body: String,
        /// Issue that must close first; repeatable
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
    },
    /// Show one issue in full
    Show { id: String },
    /// List issues; open and in progress unless filtered
    List {
        #[arg(long, value_enum)]
        status: Option<Status>,
        #[arg(long = "type", value_enum)]
        kind: Option<Kind>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        /// Include closed and deferred issues
        #[arg(long)]
        all: bool,
    },
    /// Issues that can be worked now: open, past any deferral, nothing blocking
    Ready {
        /// Show at most this many
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Open issues that something still blocks
    Blocked,
    /// Change fields of an issue
    Update {
        id: String,
        #[arg(long, allow_hyphen_values = true)]
        title: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        body: Option<String>,
        #[arg(long = "type", value_enum)]
        kind: Option<Kind>,
        #[arg(long, short, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,
        /// open clears any deferral; closed is the same as `close`
        #[arg(long, value_enum)]
        status: Option<Status>,
        /// Empty string clears
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long = "add-label")]
        add_label: Vec<String>,
        #[arg(long = "rm-label")]
        rm_label: Vec<String>,
        /// Empty string clears
        #[arg(long)]
        parent: Option<String>,
        /// Defer until an RFC 3339 timestamp or a YYYY-MM-DD date (UTC)
        #[arg(long = "defer-until")]
        defer_until: Option<String>,
    },
    /// Close one or more issues
    Close {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long, allow_hyphen_values = true)]
        reason: Option<String>,
    },
    /// Reopen one or more closed issues
    Reopen {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Take an issue: mark it in progress under your name with a lease
    Claim {
        id: String,
        /// Take it even if someone else holds an unexpired lease
        #[arg(long)]
        force: bool,
    },
    /// Give an issue back: open again, unassigned
    Unclaim {
        id: String,
        /// Release it even if someone else holds it
        #[arg(long)]
        force: bool,
    },
    /// Extend the lease on an issue you hold
    Heartbeat { id: String },
    /// Reopen every in-progress issue whose lease has expired
    Reclaim,
    /// Append a note to an issue
    Note {
        id: String,
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    /// Search issue titles, bodies and notes, and memories, case-insensitively; closed issues included
    Search { query: String },
    /// Store a memory under a slug, replacing any with the same slug
    Remember {
        slug: String,
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    /// List every memory
    Memories,
    /// Print one memory
    Recall { slug: String },
    /// Delete a memory
    Forget { slug: String },
    /// Print the context an agent needs at session start
    Prime {
        /// Wrap the output as a Claude Code SessionStart hook payload
        #[arg(long = "hook-json")]
        hook_json: bool,
        /// Ready issues to list
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Install or remove an editor integration
    Setup {
        #[command(subcommand)]
        command: SetupCmd,
    },
    /// Fetch the remote's data ref, merge it, and push the result
    Sync {
        /// Which remote
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Also add the fetch refspec and the pre-push hook to this clone
        #[arg(long)]
        setup: bool,
    },
    /// Check the data and this clone's setup for problems
    Doctor,
    /// The history of the data ref, newest first
    Log {
        /// Only entries that touched this issue or memory
        id: Option<String>,
        /// Show at most this many
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Manage what an issue waits on
    Dep {
        #[command(subcommand)]
        command: DepCmd,
    },
    /// Show or set a setting shared by every clone of this repository
    Config {
        key: Option<ConfigKey>,
        /// The new value; omit it to print the current one
        value: Option<u32>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum ConfigKey {
    /// How many minutes a claim holds before reclaim may take it back
    LeaseMinutes,
    /// How many commits behind HEAD a memory may be before prime marks it old
    StaleAfter,
}

#[derive(Subcommand)]
enum SetupCmd {
    /// A SessionStart hook in .claude/settings.json that runs `foam prime --hook-json`
    Claude {
        /// Take the hook out again
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum DepCmd {
    /// Make `id` wait on `blocker`
    Add { id: String, blocker: String },
    /// Stop `id` waiting on `blocker`
    Rm { id: String, blocker: String },
    /// Show `id` and everything it waits on
    Tree { id: String },
}

fn main() -> ExitCode {
    // a reader that stops early, `foam list | head`, closes the
    // pipe; with SIGPIPE ignored, the next println! panics
    // instead of ending the process the way head expects
    #[cfg(unix)]
    unsafe {
        // SAFETY: signal(2) with SIG_DFL only changes the
        // disposition of one signal in this process, before
        // any other thread exists
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match cmd::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("foam: {err:#}");
            if err.to_string().contains("not initialized") {
                ExitCode::from(3)
            } else {
                ExitCode::from(1)
            }
        }
    }
}
