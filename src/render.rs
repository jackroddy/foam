use std::fmt::Write as _;
use std::io::IsTerminal;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::git::{Distance, Git, Stamp};
use crate::graph;
use crate::model::{Issue, Memory, Resolution, Status};
use crate::store::Db;

/// How text output is dressed: plain for a pipe, or the
/// terminal form with color, relative times, aligned columns
/// and wrapped bodies.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub human: bool,
    pub color: bool,
    pub width: usize,
}

impl Style {
    pub const PLAIN: Style = Style {
        human: false,
        color: false,
        width: 80,
    };

    /// The style for this process's stdout.
    pub fn detect(plain: bool) -> Style {
        if plain || !std::io::stdout().is_terminal() {
            return Style::PLAIN;
        }
        // NO_COLOR asks for no color and nothing more; an empty
        // value does not count as set
        // see: https://no-color.org
        let color = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            && std::env::var("TERM").ok().is_none_or(|t| t != "dumb");
        let width = terminal_size::terminal_size()
            .map(|(w, _)| usize::from(w.0))
            .unwrap_or(80)
            .clamp(40, 100);
        Style {
            human: true,
            color,
            width,
        }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color && !code.is_empty() && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }

    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }

    fn status(&self, s: Status) -> String {
        let code = match s {
            Status::Open => "32",
            Status::InProgress => "33",
            Status::Deferred => "34",
            Status::Closed => "2",
        };
        self.paint(code, &s.to_string())
    }

    fn priority(&self, p: u8) -> String {
        let code = match p {
            0 => "1;31",
            1 => "31",
            2 => "33",
            4 => "2",
            _ => "",
        };
        self.paint(code, &format!("P{p}"))
    }

    /// A timestamp: relative to now in the terminal, RFC 3339
    /// otherwise.
    pub fn when(&self, t: Timestamp) -> String {
        if self.human {
            relative(t, Timestamp::now())
        } else {
            t.to_string()
        }
    }

    /// A future timestamp: the local date and how far off it is
    /// in the terminal, RFC 3339 otherwise.
    pub fn until(&self, t: Timestamp) -> String {
        if self.human {
            format!("{} ({})", local_date(t), relative(t, Timestamp::now()))
        } else {
            t.to_string()
        }
    }

    /// Wrap `text` to the width, the first line starting `used`
    /// columns in and each later line indented by `hang` spaces,
    /// or leave it alone when plain.
    fn wrapped(&self, text: &str, used: usize, hang: usize) -> String {
        if !self.human {
            return text.to_string();
        }
        let first = self.width.saturating_sub(used).max(20);
        let rest = self.width.saturating_sub(hang).max(20);
        let mut out = String::new();
        for (n, line) in wrap_at(text, first, rest).iter().enumerate() {
            if n > 0 {
                out.push('\n');
                out.push_str(&" ".repeat(hang));
            }
            out.push_str(line);
        }
        out
    }
}

/// How long ago, or how far ahead, `t` is from `now`: minutes,
/// hours or days, and the local date past a week.
fn relative(t: Timestamp, now: Timestamp) -> String {
    let secs = now.duration_since(t).as_secs();
    let past = secs >= 0;
    let n = secs.unsigned_abs();
    let amount = if n < 60 {
        return if past { "just now" } else { "within a minute" }.to_string();
    } else if n < 3600 {
        format!("{}m", n / 60)
    } else if n < 86_400 {
        format!("{}h", n / 3600)
    } else if n < 7 * 86_400 {
        format!("{}d", n / 86_400)
    } else {
        return local_date(t);
    };
    if past {
        format!("{amount} ago")
    } else {
        format!("in {amount}")
    }
}

fn local_date(t: Timestamp) -> String {
    t.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d")
        .to_string()
}

/// Greedy word wrap of each paragraph of `text`, the first line
/// held to `first` characters and every later one to `rest`;
/// blank lines stay, and a word longer than the width gets a
/// line to itself.
fn wrap_at(text: &str, first: usize, rest: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let width = if lines.is_empty() { first } else { rest };
            let fits = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
            if !fits {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

/// Pad `s` on the right to `width` characters.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(n)))
}

/// The `[closed/total closed]` tag for an epic's line, or nothing.
pub fn rollup_tag(db: &Db, i: &Issue) -> String {
    match graph::rollup(db, i) {
        Some((closed, total)) => format!("  [{closed}/{total} closed]"),
        None => String::new(),
    }
}

/// Describe how far `HEAD` has moved since a stamp was taken.
pub fn age(stamp: &Stamp, distance: Distance) -> String {
    match distance {
        Distance::Behind(0) => format!("at {}, this commit", stamp.commit),
        Distance::Behind(1) => format!("at {}, 1 commit ago", stamp.commit),
        Distance::Behind(n) => format!("at {}, {n} commits ago", stamp.commit),
        Distance::Elsewhere => format!(
            "at {} on {}, not in this branch's history",
            stamp.commit, stamp.branch
        ),
        Distance::Unknown => format!(
            "at {} on {}, a commit this clone lacks",
            stamp.commit, stamp.branch
        ),
    }
}

/// What trails an issue's title in a listing: the epic rollup,
/// a dropped tag and the assignee.
fn tags(i: &Issue, db: Option<&Db>) -> String {
    let mut s = String::new();
    if let Some(db) = db {
        s.push_str(&rollup_tag(db, i));
    }
    if i.resolution == Some(Resolution::Dropped) {
        s.push_str("  [dropped]");
    }
    if let Some(a) = &i.assignee {
        let _ = write!(s, "  @{a}");
    }
    s
}

/// One line per issue, each followed by its suffix when that is
/// not empty. The terminal form aligns the columns over the whole
/// listing and adds the kind.
pub fn listing(style: &Style, db: Option<&Db>, rows: &[(&Issue, String)]) -> String {
    let mut out = String::new();
    if !style.human {
        for (i, suffix) in rows {
            let _ = write!(
                out,
                "{}  P{}  {}  {}{}",
                i.id,
                i.priority,
                i.status,
                i.title,
                tags(i, db)
            );
            if !suffix.is_empty() {
                let _ = write!(out, "  {suffix}");
            }
            out.push('\n');
        }
        return out;
    }
    let width = |f: &dyn Fn(&Issue) -> String| {
        rows.iter()
            .map(|(i, _)| f(i).chars().count())
            .max()
            .unwrap_or(0)
    };
    let id_w = width(&|i| i.id.clone());
    let status_w = width(&|i| i.status.to_string());
    let kind_w = width(&|i| i.kind.to_string());
    for (i, suffix) in rows {
        let status = i.status.to_string();
        let _ = write!(
            out,
            "{}  {}  {}{}  {}  {}{}",
            pad(&i.id, id_w),
            style.priority(i.priority),
            style.status(i.status),
            " ".repeat(status_w - status.chars().count()),
            style.dim(&pad(&i.kind.to_string(), kind_w)),
            i.title,
            style.dim(&tags(i, db)),
        );
        if !suffix.is_empty() {
            let _ = write!(out, "  {}", style.dim(suffix));
        }
        out.push('\n');
    }
    out
}

/// How much of an in-progress issue's lease is left.
pub fn lease_left(i: &Issue) -> String {
    match i.lease_expires {
        Some(t) => match t.duration_since(Timestamp::now()).as_mins() {
            m if m > 0 => format!("lease {m} min left"),
            _ => "lease expired".to_string(),
        },
        None => "no lease".to_string(),
    }
}

/// One issue in full.
pub fn issue(style: &Style, i: &Issue, db: &Db) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}  {}", i.id, style.bold(&i.title));
    let _ = writeln!(
        out,
        "type: {}  status: {}  priority: {}",
        i.kind,
        style.status(i.status),
        if style.human {
            style.priority(i.priority)
        } else {
            i.priority.to_string()
        }
    );
    if !i.labels.is_empty() {
        let _ = writeln!(out, "labels: {}", i.labels.join(", "));
    }
    if let Some(p) = &i.parent {
        let _ = writeln!(out, "parent: {p}");
    }
    let children = graph::children(db, &i.id);
    if !children.is_empty() {
        match graph::rollup(db, i) {
            Some((closed, total)) => {
                let _ = writeln!(out, "children ({closed}/{total} closed):");
            }
            None => out.push_str("children:\n"),
        }
        for c in children {
            let _ = writeln!(out, "  {}  {}  {}", c.id, style.status(c.status), c.title);
        }
    }
    if let Some(a) = &i.assignee {
        match i.lease_expires {
            Some(_) if style.human => {
                let _ = writeln!(out, "assignee: {a}  {}", style.dim(&lease_left(i)));
            }
            Some(t) => {
                let _ = writeln!(out, "assignee: {a}  lease until {t}");
            }
            None => {
                let _ = writeln!(out, "assignee: {a}");
            }
        }
    }
    if let Some(t) = i.defer_until {
        let _ = writeln!(out, "deferred until: {}", style.until(t));
    }
    if !i.blocked_by.is_empty() {
        out.push_str("blocked by:\n");
        for b in &i.blocked_by {
            let status = match db.issues.get(b) {
                Some(o) => style.status(o.status),
                None => "missing".to_string(),
            };
            let _ = writeln!(out, "  {b}  {status}");
        }
    }
    if style.human {
        let _ = writeln!(out, "created: {}", style.when(i.created_at));
    } else {
        let _ = writeln!(
            out,
            "created: {}  on {} ({})",
            i.created_at, i.stamps.created.branch, i.stamps.created.commit
        );
    }
    if let Some(c) = i.closed_at {
        let _ = write!(out, "closed: {}", style.when(c));
        if let Some(r) = i.resolution {
            let _ = write!(out, "  {r}");
        }
        if let Some(why) = &i.close_reason {
            let _ = write!(out, "  {why}");
        }
        out.push('\n');
    }
    if !i.body.is_empty() {
        out.push('\n');
        let _ = writeln!(out, "{}", style.wrapped(&i.body, 0, 0));
    }
    if !i.notes.is_empty() {
        out.push('\n');
        for n in &i.notes {
            let head = format!("[{} {}] ", style.when(n.at), n.author);
            let _ = writeln!(
                out,
                "{}{}",
                style.dim(&head),
                style.wrapped(&n.text, head.chars().count(), 2)
            );
        }
    }
    out
}

/// One line per memory: slug, text and how far `HEAD` has moved
/// since it was written.
pub fn memories(style: &Style, git: &Git, memories: &[&Memory]) -> String {
    let mut out = String::new();
    if !style.human {
        for m in memories {
            let distance = git.distance(&m.stamp.commit);
            let _ = writeln!(out, "{}  {}  ({})", m.slug, m.text, age(&m.stamp, distance));
        }
        return out;
    }
    let slug_w = memories
        .iter()
        .map(|m| m.slug.chars().count())
        .max()
        .unwrap_or(0);
    for m in memories {
        let hang = slug_w + 2;
        let text = style.wrapped(&m.text, hang, hang);
        let _ = writeln!(
            out,
            "{}  {}{}",
            style.bold(&pad(&m.slug, slug_w)),
            text,
            style.dim(&trailing_age(style, &text, hang, m, git)),
        );
    }
    out
}

/// A memory's text and age, as `recall` prints them.
pub fn memory(style: &Style, git: &Git, m: &Memory) -> String {
    let text = style.wrapped(&m.text, 0, 0);
    if style.human {
        format!(
            "{text}{}\n",
            style.dim(&trailing_age(style, &text, 0, m, git))
        )
    } else {
        format!(
            "{text}\n({})\n",
            age(&m.stamp, git.distance(&m.stamp.commit))
        )
    }
}

/// The age in parentheses, on the same line as the wrapped
/// text's last line when it fits there and on its own otherwise.
fn trailing_age(style: &Style, text: &str, hang: usize, m: &Memory, git: &Git) -> String {
    let age = format!("({})", age(&m.stamp, git.distance(&m.stamp.commit)));
    let last = text.rsplit('\n').next().unwrap_or("").chars().count();
    let first_line = !text.contains('\n');
    let used = if first_line { hang + last } else { last };
    if used + 2 + age.chars().count() <= style.width {
        format!("  {age}")
    } else {
        format!("\n{}{age}", " ".repeat(hang))
    }
}

/// The data ref's history, one commit per line.
pub fn log(style: &Style, entries: &[(String, String, String)]) -> String {
    let mut out = String::new();
    if !style.human {
        for (oid, when, msg) in entries {
            let _ = writeln!(out, "{}  {when}  {msg}", &oid[..7]);
        }
        return out;
    }
    let whens: Vec<String> = entries
        .iter()
        .map(|(_, when, _)| match when.parse::<Timestamp>() {
            Ok(t) => style.when(t),
            Err(_) => when.clone(),
        })
        .collect();
    let when_w = whens.iter().map(|w| w.chars().count()).max().unwrap_or(0);
    for ((oid, _, msg), when) in entries.iter().zip(&whens) {
        let _ = writeln!(
            out,
            "{}  {}  {msg}",
            style.dim(&oid[..7]),
            pad(when, when_w)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_issue;

    const HUMAN: Style = Style {
        human: true,
        color: false,
        width: 40,
    };

    const COLOR: Style = Style {
        human: true,
        color: true,
        width: 80,
    };

    fn ago(secs: i64) -> Timestamp {
        Timestamp::now() - jiff::SignedDuration::from_secs(secs)
    }

    #[test]
    fn relative_times_pick_the_largest_unit() {
        let now = Timestamp::now();
        let at = |secs: i64| now - jiff::SignedDuration::from_secs(secs);
        assert_eq!(relative(at(5), now), "just now");
        assert_eq!(relative(at(-5), now), "within a minute");
        assert_eq!(relative(at(90), now), "1m ago");
        assert_eq!(relative(at(3 * 3600 + 100), now), "3h ago");
        assert_eq!(relative(at(-2 * 86_400), now), "in 2d");
        let old = relative(at(30 * 86_400), now);
        assert_eq!(old.len(), 10, "{old}");
        assert!(old.starts_with("20"), "{old}");
    }

    #[test]
    fn wrap_breaks_on_words_and_keeps_paragraphs() {
        let lines = wrap_at("one two three four\n\nfive", 9, 9);
        assert_eq!(lines, ["one two", "three", "four", "", "five"]);
        assert_eq!(wrap_at("abcdefghijkl x", 5, 5), ["abcdefghijkl", "x"]);
        assert_eq!(wrap_at("aa bb cc dd", 2, 5), ["aa", "bb cc", "dd"]);
    }

    #[test]
    fn listing_aligns_columns_in_the_terminal_form() {
        let mut a = test_issue("t-000001");
        a.title = "first".into();
        a.status = Status::InProgress;
        a.kind = crate::model::Kind::Feature;
        let mut b = test_issue("t-000002");
        b.title = "second".into();
        b.priority = 0;
        let rows = vec![(&a, String::new()), (&b, "<- t-000001".to_string())];
        let text = listing(&HUMAN, None, &rows);
        assert_eq!(
            text,
            "t-000001  P2  in_progress  feature  first\n\
             t-000002  P0  open         task     second  <- t-000001\n"
        );
        let plain = listing(&Style::PLAIN, None, &rows);
        assert_eq!(
            plain,
            "t-000001  P2  in_progress  first\n\
             t-000002  P0  open  second  <- t-000001\n"
        );
        let colored = listing(&COLOR, None, &rows[..1]);
        assert!(colored.contains("\x1b[33min_progress\x1b[0m"), "{colored}");
        assert!(colored.contains("\x1b[33mP2\x1b[0m"), "{colored}");
    }

    #[test]
    fn issue_wraps_bodies_and_relative_times_only_in_the_terminal_form() {
        let db = crate::store::test_db();
        let mut i = test_issue("t-000001");
        i.body = "a body that is longer than the forty columns the test style allows".into();
        i.created_at = ago(2 * 3600);
        i.notes.push(crate::model::Note {
            at: ago(60),
            author: "ann".into(),
            text: "a note that also runs past the forty column mark".into(),
            commit: "none".into(),
            branch: "main".into(),
        });
        let text = issue(&HUMAN, &i, &db);
        assert!(text.contains("created: 2h ago\n"), "{text}");
        assert!(text.lines().all(|l| l.chars().count() <= 40), "{text}");
        assert!(text.contains("[1m ago ann] a note"), "{text}");
        assert!(text.contains("\n  "), "{text}");
        let plain = issue(&Style::PLAIN, &i, &db);
        assert!(plain.contains("  on main (none)\n"), "{plain}");
        assert!(plain.contains(&format!("\n{}\n", i.body)), "{plain}");
    }
}
