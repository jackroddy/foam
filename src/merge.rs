use std::collections::HashSet;

use crate::model::{Issue, Memory, Note};

/// Merge one issue's three versions: `None` is "not in that tree".
pub fn issue(base: Option<&Issue>, ours: Option<&Issue>, theirs: Option<&Issue>) -> Option<Issue> {
    let (o, t) = match (ours, theirs) {
        (None, None) => return None,
        (Some(o), None) => return keep_if_changed(base, o),
        (None, Some(t)) => return keep_if_changed(base, t),
        (Some(o), Some(t)) => (o, t),
    };
    let ours_newer = o.updated_at >= t.updated_at;

    // status moves with the fields that only mean something
    // alongside it, so a close on one side and a claim on
    // the other cannot leave a closed issue with a lease
    let (status, closed_at, close_reason, assignee, lease_expires, defer_until, closed_stamp) =
        field(base, o, t, ours_newer, |i| {
            (
                i.status,
                i.closed_at,
                i.close_reason.clone(),
                i.assignee.clone(),
                i.lease_expires,
                i.defer_until,
                i.stamps.closed.clone(),
            )
        });

    let mut stamps = field(base, o, t, ours_newer, |i| i.stamps.clone());
    stamps.closed = closed_stamp;

    Some(Issue {
        id: o.id.clone(),
        title: field(base, o, t, ours_newer, |i| i.title.clone()),
        body: field(base, o, t, ours_newer, |i| i.body.clone()),
        kind: field(base, o, t, ours_newer, |i| i.kind),
        status,
        priority: field(base, o, t, ours_newer, |i| i.priority),
        labels: sets(base.map(|b| &b.labels), &o.labels, &t.labels),
        parent: field(base, o, t, ours_newer, |i| i.parent.clone()),
        blocked_by: sets(base.map(|b| &b.blocked_by), &o.blocked_by, &t.blocked_by),
        related: sets(base.map(|b| &b.related), &o.related, &t.related),
        assignee,
        lease_expires,
        defer_until,
        created_at: o.created_at.min(t.created_at),
        updated_at: o.updated_at.max(t.updated_at),
        closed_at,
        close_reason,
        notes: notes(base.map(|b| &b.notes), &o.notes, &t.notes),
        stamps,
    })
}

pub fn memory(
    base: Option<&Memory>,
    ours: Option<&Memory>,
    theirs: Option<&Memory>,
) -> Option<Memory> {
    let (o, t) = match (ours, theirs) {
        (None, None) => return None,
        (Some(o), None) => return keep_if_changed(base, o),
        (None, Some(t)) => return keep_if_changed(base, t),
        (Some(o), Some(t)) => (o, t),
    };
    let ours_newer = o.updated_at >= t.updated_at;
    let (text, stamp) = pick(
        base.map(|b| (b.text.clone(), b.stamp.clone())),
        (o.text.clone(), o.stamp.clone()),
        (t.text.clone(), t.stamp.clone()),
        ours_newer,
    );
    Some(Memory {
        slug: o.slug.clone(),
        text,
        created_at: o.created_at.min(t.created_at),
        updated_at: o.updated_at.max(t.updated_at),
        stamp,
    })
}

fn field<T: PartialEq>(
    base: Option<&Issue>,
    ours: &Issue,
    theirs: &Issue,
    ours_newer: bool,
    f: impl Fn(&Issue) -> T,
) -> T {
    pick(base.map(&f), f(ours), f(theirs), ours_newer)
}

/// One side deleted the record. Deletion stands unless the
/// other side changed it since the base.
fn keep_if_changed<T: Clone + PartialEq>(base: Option<&T>, survivor: &T) -> Option<T> {
    match base {
        None => Some(survivor.clone()),
        Some(b) if b == survivor => None,
        Some(_) => Some(survivor.clone()),
    }
}

/// A scalar: the side that changed it wins; when both changed
/// it to different values, the newer record wins.
fn pick<T: PartialEq>(base: Option<T>, ours: T, theirs: T, ours_newer: bool) -> T {
    if ours == theirs {
        return ours;
    }
    match base {
        Some(b) if b == ours => theirs,
        Some(b) if b == theirs => ours,
        _ if ours_newer => ours,
        _ => theirs,
    }
}

/// A set: what either side added, minus what either removed.
fn sets(base: Option<&Vec<String>>, ours: &[String], theirs: &[String]) -> Vec<String> {
    let base: Vec<String> = base.cloned().unwrap_or_default();
    let has = |v: &[String], x: &String| v.contains(x);
    let removed: HashSet<&String> = base
        .iter()
        .filter(|x| !has(ours, x) || !has(theirs, x))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for x in base.iter().chain(ours).chain(theirs) {
        if !removed.contains(x) && !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

/// Notes: the base plus everything either side appended, in
/// time order.
fn notes(base: Option<&Vec<Note>>, ours: &[Note], theirs: &[Note]) -> Vec<Note> {
    let mut out: Vec<Note> = base.cloned().unwrap_or_default();
    for n in ours.iter().chain(theirs) {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out.sort_by_key(|n| n.at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Stamp;
    use crate::model::{Status, test_issue};
    use jiff::Timestamp;

    fn later(i: &mut Issue) {
        i.updated_at += jiff::SignedDuration::from_secs(10);
    }

    fn note(text: &str, secs: i64) -> Note {
        Note {
            at: Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(secs),
            author: "a".into(),
            text: text.into(),
            commit: "none".into(),
            branch: "main".into(),
        }
    }

    #[test]
    fn one_sided_changes_pass_through() {
        let base = test_issue("t-a");
        let mut ours = base.clone();
        ours.title = "renamed".into();
        let mut theirs = base.clone();
        theirs.priority = 0;
        let m = issue(Some(&base), Some(&ours), Some(&theirs)).unwrap();
        assert_eq!(m.title, "renamed");
        assert_eq!(m.priority, 0);
    }

    #[test]
    fn both_changed_takes_the_newer_record() {
        let base = test_issue("t-a");
        let mut ours = base.clone();
        ours.title = "ours".into();
        let mut theirs = base.clone();
        theirs.title = "theirs".into();
        later(&mut theirs);
        let m = issue(Some(&base), Some(&ours), Some(&theirs)).unwrap();
        assert_eq!(m.title, "theirs");
        let m = issue(Some(&base), Some(&theirs), Some(&ours)).unwrap();
        assert_eq!(m.title, "theirs");
    }

    #[test]
    fn status_moves_as_a_group() {
        let base = test_issue("t-a");
        let mut ours = base.clone();
        ours.claim("ann", jiff::SignedDuration::from_mins(15));
        let mut theirs = base.clone();
        theirs.close(
            Some("done".into()),
            &Stamp {
                commit: "c".into(),
                branch: "b".into(),
            },
        );
        later(&mut theirs);
        let m = issue(Some(&base), Some(&ours), Some(&theirs)).unwrap();
        assert_eq!(m.status, Status::Closed);
        assert_eq!(m.assignee, None);
        assert_eq!(m.lease_expires, None);
        assert_eq!(m.close_reason.as_deref(), Some("done"));
        assert_eq!(m.stamps.closed.as_ref().unwrap().commit, "c");
    }

    #[test]
    fn removals_survive_unrelated_edits_on_the_other_side() {
        let mut base = test_issue("t-a");
        base.labels = vec!["x".into(), "y".into()];
        let mut ours = base.clone();
        ours.labels = vec!["y".into()];
        let mut theirs = base.clone();
        theirs.labels = vec!["x".into(), "y".into(), "z".into()];
        let m = issue(Some(&base), Some(&ours), Some(&theirs)).unwrap();
        assert_eq!(m.labels, ["y", "z"]);
        let m = issue(Some(&base), Some(&theirs), Some(&ours)).unwrap();
        assert_eq!(m.labels, ["y", "z"]);
    }

    #[test]
    fn notes_from_both_sides_interleave_by_time() {
        let mut base = test_issue("t-a");
        base.notes = vec![note("base", 1)];
        let mut ours = base.clone();
        ours.notes.push(note("ours", 3));
        let mut theirs = base.clone();
        theirs.notes.push(note("theirs", 2));
        let m = issue(Some(&base), Some(&ours), Some(&theirs)).unwrap();
        let texts: Vec<&str> = m.notes.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, ["base", "theirs", "ours"]);
    }

    #[test]
    fn deletion_yields_to_a_change() {
        let base = test_issue("t-a");
        assert!(issue(Some(&base), Some(&base), None).is_none());
        let mut changed = base.clone();
        changed.title = "kept".into();
        assert_eq!(
            issue(Some(&base), None, Some(&changed)).unwrap().title,
            "kept"
        );
        assert!(issue(None, Some(&base), None).is_some());
    }

    #[test]
    fn same_slug_written_on_both_sides_takes_the_newer() {
        let a = Memory {
            slug: "s".into(),
            text: "old".into(),
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
            stamp: Stamp {
                commit: "1".into(),
                branch: "main".into(),
            },
        };
        let mut b = a.clone();
        b.text = "new".into();
        b.updated_at = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(5);
        b.stamp.commit = "2".into();
        let m = memory(None, Some(&a), Some(&b)).unwrap();
        assert_eq!(m.text, "new");
        assert_eq!(m.stamp.commit, "2");
        assert_eq!(m.created_at, a.created_at);
    }
}
