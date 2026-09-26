mod cmd;
mod git;
mod model;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
        /// 0 is highest, 4 lowest
        #[arg(long, short, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: u8,
        /// Add a label; repeatable
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Parent epic
        #[arg(long)]
        parent: Option<String>,
        /// Longer description
        #[arg(long, default_value = "")]
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
}

fn main() -> ExitCode {
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
