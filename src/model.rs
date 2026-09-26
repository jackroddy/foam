use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::git::Stamp;

pub const SCHEMA_VERSION: u32 = 1;

/// The contents of `meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub prefix: String,
    pub schema_version: u32,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Task,
    Bug,
    Feature,
    Chore,
    Epic,
    Spike,
    Decision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub at: Timestamp,
    pub author: String,
    pub text: String,
    pub commit: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamps {
    pub created: Stamp,
    pub closed: Option<Stamp>,
}

/// One issue, as stored in `issues/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub status: Status,
    pub priority: u8,
    pub labels: Vec<String>,
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
    pub related: Vec<String>,
    pub assignee: Option<String>,
    pub lease_expires: Option<Timestamp>,
    pub defer_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub close_reason: Option<String>,
    pub notes: Vec<Note>,
    pub stamps: Stamps,
}

/// One memory, as stored in `memories/<slug>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub slug: String,
    pub text: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub stamp: Stamp,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Closed => "closed",
            Status::Deferred => "deferred",
        })
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Task => "task",
            Kind::Bug => "bug",
            Kind::Feature => "feature",
            Kind::Chore => "chore",
            Kind::Epic => "epic",
            Kind::Spike => "spike",
            Kind::Decision => "decision",
        })
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(Status::Open),
            "in_progress" => Ok(Status::InProgress),
            "closed" => Ok(Status::Closed),
            "deferred" => Ok(Status::Deferred),
            other => Err(format!("unknown status: {other}")),
        }
    }
}

/// Serialize a record in the one byte layout the store commits.
pub fn canonical<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("records serialize");
    bytes.push(b'\n');
    bytes
}

/// A fresh issue id: the prefix and six random hex digits.
pub fn new_id(prefix: &str) -> String {
    let n = rand::random::<u32>() & 0xff_ffff;
    format!("{prefix}-{n:06x}")
}
