use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::git::Stamp;

pub const SCHEMA_VERSION: u32 = 1;

/// How long a claim holds before `reclaim` may take it back.
pub const LEASE: jiff::SignedDuration = jiff::SignedDuration::from_mins(15);

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl Issue {
    pub fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }

    pub fn close(&mut self, reason: Option<String>, stamp: &Stamp) {
        let now = Timestamp::now();
        self.status = Status::Closed;
        self.closed_at = Some(now);
        self.close_reason = reason;
        self.stamps.closed = Some(stamp.clone());
        self.lease_expires = None;
        self.updated_at = now;
    }

    pub fn claim(&mut self, actor: &str) {
        let now = Timestamp::now();
        self.status = Status::InProgress;
        self.assignee = Some(actor.to_string());
        self.lease_expires = Some(now + LEASE);
        self.defer_until = None;
        self.updated_at = now;
    }

    pub fn unclaim(&mut self) {
        self.status = Status::Open;
        self.assignee = None;
        self.lease_expires = None;
        self.touch();
    }

    /// Whether someone other than `actor` holds an unexpired lease.
    pub fn held_by_other(&self, actor: &str, now: Timestamp) -> bool {
        self.status == Status::InProgress
            && self.assignee.as_deref() != Some(actor)
            && self.lease_expires.is_some_and(|t| t > now)
    }

    pub fn reopen(&mut self) {
        self.status = Status::Open;
        self.closed_at = None;
        self.close_reason = None;
        self.stamps.closed = None;
        self.defer_until = None;
        self.touch();
    }
}

/// Check a memory slug: lowercase letters, digits and hyphens.
pub fn check_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "slug must be lowercase letters, digits and hyphens: {slug}"
        ))
    }
}

/// Parse an RFC 3339 timestamp, or a date as midnight UTC.
pub fn parse_when(s: &str) -> Result<Timestamp, String> {
    if let Ok(t) = s.parse::<Timestamp>() {
        return Ok(t);
    }
    let date: jiff::civil::Date = s
        .parse()
        .map_err(|_| format!("not a timestamp or a date: {s}"))?;
    date.in_tz("UTC")
        .map(|z| z.timestamp())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub fn test_issue(id: &str) -> Issue {
    let now = Timestamp::now();
    let stamp = Stamp {
        commit: "none".into(),
        branch: "main".into(),
    };
    Issue {
        id: id.into(),
        title: "t".into(),
        body: String::new(),
        kind: Kind::Task,
        status: Status::Open,
        priority: 2,
        labels: vec![],
        parent: None,
        blocked_by: vec![],
        related: vec![],
        assignee: None,
        lease_expires: None,
        defer_until: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
        close_reason: None,
        notes: vec![],
        stamps: Stamps {
            created: stamp,
            closed: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bytes_are_stable_and_newline_terminated() {
        let issue = test_issue("t-000001");
        let a = canonical(&issue);
        let b = canonical(&issue);
        assert_eq!(a, b);
        assert!(a.ends_with(b"}\n"));
        assert!(a.starts_with(b"{\n  \"id\": \"t-000001\",\n"));
    }

    #[test]
    fn ids_have_the_prefix_and_six_hex_digits() {
        let id = new_id("foam");
        let (prefix, hex) = id.rsplit_once('-').unwrap();
        assert_eq!(prefix, "foam");
        assert_eq!(hex.len(), 6);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn when_accepts_dates_and_timestamps() {
        assert_eq!(
            parse_when("2026-10-01").unwrap().to_string(),
            "2026-10-01T00:00:00Z"
        );
        assert!(parse_when("2026-10-01T12:00:00Z").is_ok());
        assert!(parse_when("tomorrow").is_err());
    }
}
