use std::collections::HashSet;

use jiff::Timestamp;

use crate::model::{Issue, Status};
use crate::store::Db;

/// Whether an issue is waiting to be worked: open, or deferred
/// to a time that has passed.
pub fn is_pending(issue: &Issue, now: Timestamp) -> bool {
    match issue.status {
        Status::Open => issue.defer_until.is_none_or(|t| t <= now),
        Status::Deferred => issue.defer_until.is_some_and(|t| t <= now),
        Status::InProgress | Status::Closed => false,
    }
}

/// The ids in `blocked_by` that still hold the issue back.
//
// a blocker that no longer exists is skipped rather than
// blocking forever; `doctor` reports those
pub fn open_blockers<'a>(db: &'a Db, issue: &'a Issue) -> Vec<&'a str> {
    issue
        .blocked_by
        .iter()
        .filter(|b| {
            db.issues
                .get(*b)
                .is_some_and(|o| o.status != Status::Closed)
        })
        .map(String::as_str)
        .collect()
}

pub fn is_ready(db: &Db, issue: &Issue, now: Timestamp) -> bool {
    is_pending(issue, now) && open_blockers(db, issue).is_empty()
}

/// Pending issues with nothing holding them back, highest
/// priority first.
pub fn ready(db: &Db, now: Timestamp) -> Vec<&Issue> {
    let mut out: Vec<&Issue> = db
        .issues
        .values()
        .filter(|i| is_ready(db, i, now))
        .collect();
    out.sort_by_key(|i| (i.priority, i.created_at));
    out
}

/// Pending issues that are held back, with what holds them.
pub fn blocked(db: &Db, now: Timestamp) -> Vec<(&Issue, Vec<&str>)> {
    let mut out: Vec<(&Issue, Vec<&str>)> = db
        .issues
        .values()
        .filter(|i| is_pending(i, now))
        .map(|i| (i, open_blockers(db, i)))
        .filter(|(_, b)| !b.is_empty())
        .collect();
    out.sort_by_key(|(i, _)| (i.priority, i.created_at));
    out
}

/// Whether making `issue` depend on `blocker` would close a loop.
pub fn would_cycle(db: &Db, issue: &str, blocker: &str) -> bool {
    if issue == blocker {
        return true;
    }
    let mut seen = HashSet::new();
    let mut stack = vec![blocker];
    while let Some(id) = stack.pop() {
        if id == issue {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(i) = db.issues.get(id) {
            stack.extend(i.blocked_by.iter().map(String::as_str));
        }
    }
    false
}

/// Render `id` and everything it waits on, one line per issue,
/// indented by depth.
pub fn tree(db: &Db, id: &str) -> String {
    let mut out = String::new();
    let mut seen = HashSet::new();
    walk(db, id, 0, &mut seen, &mut out);
    out
}

/// The same tree as nested JSON: `{id, status, title, blocked_by: [...]}`.
pub fn tree_json(db: &Db, id: &str) -> serde_json::Value {
    let mut seen = HashSet::new();
    node(db, id, &mut seen)
}

fn node(db: &Db, id: &str, seen: &mut HashSet<String>) -> serde_json::Value {
    let Some(issue) = db.issues.get(id) else {
        return serde_json::json!({ "id": id, "missing": true });
    };
    let children: Vec<serde_json::Value> = if seen.insert(id.to_string()) {
        issue.blocked_by.iter().map(|b| node(db, b, seen)).collect()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "id": id,
        "status": issue.status,
        "title": issue.title,
        "blocked_by": children,
    })
}

fn walk(db: &Db, id: &str, depth: usize, seen: &mut HashSet<String>, out: &mut String) {
    let indent = "  ".repeat(depth);
    let Some(issue) = db.issues.get(id) else {
        out.push_str(&format!("{indent}{id}  missing\n"));
        return;
    };
    out.push_str(&format!(
        "{indent}{id}  {}  {}\n",
        issue.status, issue.title
    ));
    if !seen.insert(id.to_string()) {
        return;
    }
    for b in &issue.blocked_by {
        walk(db, b, depth + 1, seen, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_issue;
    use crate::store::test_db;

    fn now() -> Timestamp {
        Timestamp::now()
    }

    #[test]
    fn a_chain_frees_one_link_at_a_time() {
        let mut db = test_db();
        let a = test_issue("t-a");
        let mut b = test_issue("t-b");
        b.blocked_by = vec!["t-a".into()];
        let mut c = test_issue("t-c");
        c.blocked_by = vec!["t-b".into()];
        for i in [a, b, c] {
            db.issues.insert(i.id.clone(), i);
        }

        fn ids(v: Vec<&Issue>) -> Vec<String> {
            v.iter().map(|i| i.id.clone()).collect()
        }
        assert_eq!(ids(ready(&db, now())), ["t-a"]);
        assert_eq!(blocked(&db, now()).len(), 2);

        db.issues.get_mut("t-a").unwrap().status = Status::Closed;
        assert_eq!(ids(ready(&db, now())), ["t-b"]);
        // c waits on b, and b is open even though b is
        // no longer blocked
        assert_eq!(blocked(&db, now())[0].1, ["t-b"]);
    }

    #[test]
    fn a_diamond_needs_both_sides_closed() {
        let mut db = test_db();
        let top = test_issue("t-top");
        let mut l = test_issue("t-l");
        l.blocked_by = vec!["t-top".into()];
        let mut r = test_issue("t-r");
        r.blocked_by = vec!["t-top".into()];
        let mut bottom = test_issue("t-bottom");
        bottom.blocked_by = vec!["t-l".into(), "t-r".into()];
        for i in [top, l, r, bottom] {
            db.issues.insert(i.id.clone(), i);
        }
        db.issues.get_mut("t-top").unwrap().status = Status::Closed;
        db.issues.get_mut("t-l").unwrap().status = Status::Closed;
        assert!(!is_ready(&db, &db.issues["t-bottom"], now()));
        db.issues.get_mut("t-r").unwrap().status = Status::Closed;
        assert!(is_ready(&db, &db.issues["t-bottom"], now()));
    }

    #[test]
    fn cycles_are_detected_at_any_depth() {
        let mut db = test_db();
        let a = test_issue("t-a");
        let mut b = test_issue("t-b");
        b.blocked_by = vec!["t-a".into()];
        let mut c = test_issue("t-c");
        c.blocked_by = vec!["t-b".into()];
        for i in [a, b, c] {
            db.issues.insert(i.id.clone(), i);
        }
        assert!(would_cycle(&db, "t-a", "t-a"));
        assert!(would_cycle(&db, "t-a", "t-c"));
        assert!(would_cycle(&db, "t-a", "t-b"));
        assert!(!would_cycle(&db, "t-c", "t-a"));
        assert!(!would_cycle(&db, "t-a", "t-nope"));
    }

    #[test]
    fn deferral_and_dangling_blockers() {
        let mut db = test_db();
        let mut a = test_issue("t-a");
        a.status = Status::Deferred;
        a.defer_until = Some(now() + jiff::SignedDuration::from_hours(1));
        let mut b = test_issue("t-b");
        b.status = Status::Deferred;
        b.defer_until = Some(now() - jiff::SignedDuration::from_hours(1));
        let mut c = test_issue("t-c");
        c.blocked_by = vec!["t-gone".into()];
        for i in [a, b, c] {
            db.issues.insert(i.id.clone(), i);
        }
        let ids: Vec<&str> = ready(&db, now()).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["t-b", "t-c"]);
    }

    #[test]
    fn tree_renders_depth_and_missing() {
        let mut db = test_db();
        let mut a = test_issue("t-a");
        a.blocked_by = vec!["t-b".into(), "t-gone".into()];
        let b = test_issue("t-b");
        for i in [a, b] {
            db.issues.insert(i.id.clone(), i);
        }
        assert_eq!(
            tree(&db, "t-a"),
            "t-a  open  t\n  t-b  open  t\n  t-gone  missing\n"
        );
    }
}
