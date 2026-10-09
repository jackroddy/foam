//! The listing and the board as screens, with a cursor over the
//! issues.

use anyhow::Result;
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::graph::{self, BOARD_LOG};
use crate::model::{Issue, Status};
use crate::render;
use crate::store::{DATA_REF, Db, Store};

pub fn run(store: &Store) -> Result<()> {
    ratatui::run(|terminal| screen(terminal, store))
}

/// Which screen is up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    /// What `foam list` prints: open and in-progress issues as
    /// a tree.
    List,
    /// What `foam board` prints: milestones, in progress, ready,
    /// blocked and the recent log.
    Board,
}

impl View {
    fn other(self) -> View {
        match self {
            View::List => View::Board,
            View::Board => View::List,
        }
    }

    fn keys(self) -> &'static str {
        match self {
            View::List => "j/k move   g/G ends   Tab board   r reload   q quit",
            View::Board => "j/k move   g/G ends   Tab list   r reload   q quit",
        }
    }
}

/// One line of a screen.
enum Row<'a> {
    Header(&'static str),
    Issue {
        /// The indent and connector of a row under its parent.
        lead: String,
        issue: &'a Issue,
        rollup: String,
        suffix: String,
    },
    Log(String),
    Note(&'static str),
}

impl Row<'_> {
    fn selectable(&self) -> bool {
        matches!(self, Row::Issue { .. })
    }
}

/// The widest id, status, kind and rollup among the issue rows,
/// so the columns line up down the whole screen.
#[derive(Default)]
struct Widths {
    id: usize,
    status: usize,
    kind: usize,
    rollup: usize,
}

struct Screen<'a> {
    view: View,
    title: Line<'static>,
    rows: Vec<Row<'a>>,
    widths: Widths,
}

fn screen(terminal: &mut DefaultTerminal, store: &Store) -> Result<()> {
    let mut view = View::List;
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let mut screen = build(store, &snap.db, view)?;
        let mut state = ListState::default();
        state.select(step(&screen.rows, None, 1));
        loop {
            terminal.draw(|frame| draw(frame, &screen, &mut state))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Char('r') => break,
                KeyCode::Tab | KeyCode::BackTab => {
                    view = view.other();
                    screen = build(store, &snap.db, view)?;
                    state.select(step(&screen.rows, None, 1));
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    state.select(step(&screen.rows, state.selected(), 1));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    state.select(step(&screen.rows, state.selected(), -1));
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    state.select(step(&screen.rows, None, 1));
                }
                KeyCode::Char('G') | KeyCode::End => {
                    state.select(step(&screen.rows, None, -1));
                }
                _ => {}
            }
        }
    }
}

/// The selectable row `by` steps from `from`, or the first from
/// either end when there is none; stays put at the edges.
fn step(rows: &[Row], from: Option<usize>, by: isize) -> Option<usize> {
    let selectable = |i: &usize| rows[*i].selectable();
    match (from, by.is_positive()) {
        (None, true) => (0..rows.len()).find(selectable),
        (None, false) => (0..rows.len()).rev().find(selectable),
        (Some(at), true) => (at + 1..rows.len()).find(selectable).or(Some(at)),
        (Some(at), false) => (0..at).rev().find(selectable).or(Some(at)),
    }
}

fn build<'a>(store: &Store, db: &'a Db, view: View) -> Result<Screen<'a>> {
    let now = Timestamp::now();
    let stamp = store.git.head_stamp()?;
    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let title = Line::from(vec![
        Span::raw(db.meta.prefix.clone()).bold(),
        Span::raw(format!("  on {} at {}  ", stamp.branch, stamp.commit)),
        Span::raw(format!(
            "{} open, {} in progress, {} deferred, {} closed",
            count(Status::Open),
            count(Status::InProgress),
            count(Status::Deferred),
            count(Status::Closed),
        ))
        .dim(),
    ]);
    let mut rows = match view {
        View::List => list_rows(db),
        View::Board => board_rows(db, now),
    };
    if view == View::Board {
        let log = store.git.log(DATA_REF, BOARD_LOG, None)?;
        if !log.is_empty() {
            rows.push(Row::Header("Recent"));
            let style = render::Style {
                human: true,
                color: false,
                width: 80,
            };
            for (oid, when, msg) in &log {
                let when = when
                    .parse::<Timestamp>()
                    .map(|t| style.when(t))
                    .unwrap_or_else(|_| when.clone());
                rows.push(Row::Log(format!("{}  {when}  {msg}", &oid[..7])));
            }
        }
    }
    let mut widths = Widths::default();
    for row in &rows {
        if let Row::Issue {
            lead,
            issue,
            rollup,
            ..
        } = row
        {
            widths.id = widths
                .id
                .max(lead.chars().count() + issue.id.chars().count());
            widths.status = widths.status.max(issue.status.to_string().len());
            widths.kind = widths.kind.max(issue.kind.to_string().len());
            widths.rollup = widths.rollup.max(rollup.len());
        }
    }
    Ok(Screen {
        view,
        title,
        rows,
        widths,
    })
}

/// What `foam list` prints: open and in-progress issues by
/// priority, each under its parent when the parent is listed.
fn list_rows(db: &Db) -> Vec<Row<'_>> {
    let mut issues: Vec<&Issue> = db
        .issues
        .values()
        .filter(|i| matches!(i.status, Status::Open | Status::InProgress))
        .collect();
    issues.sort_by_key(|i| (i.priority, i.created_at));
    let rows: Vec<(&Issue, String)> = issues
        .into_iter()
        .map(|i| (i, waits_on(graph::open_blockers(db, i))))
        .collect();
    render::forest(&rows)
        .into_iter()
        .map(|(_, lead, issue, suffix)| Row::Issue {
            lead,
            issue,
            rollup: rollup(db, issue),
            suffix: suffix.to_string(),
        })
        .collect()
}

/// The issue sections of the board, in the order `foam board`
/// prints them.
fn board_rows(db: &Db, now: Timestamp) -> Vec<Row<'_>> {
    fn row<'a>(db: &Db, issue: &'a Issue, suffix: String) -> Row<'a> {
        Row::Issue {
            lead: String::new(),
            issue,
            rollup: rollup(db, issue),
            suffix,
        }
    }
    let board = graph::board(db, now);
    let issue = |issue, suffix| row(db, issue, suffix);
    let mut rows = Vec::new();
    if !board.milestones.is_empty() {
        rows.push(Row::Header("Milestones"));
        rows.extend(
            board
                .milestones
                .into_iter()
                .map(|i| issue(i, String::new())),
        );
    }
    if !board.in_progress.is_empty() {
        rows.push(Row::Header("In progress"));
        rows.extend(
            board
                .in_progress
                .into_iter()
                .map(|i| issue(i, render::lease_left(i))),
        );
    }
    rows.push(Row::Header("Ready"));
    if board.ready.is_empty() {
        rows.push(Row::Note("nothing is ready"));
    }
    rows.extend(board.ready.into_iter().map(|i| issue(i, String::new())));
    if !board.blocked.is_empty() {
        rows.push(Row::Header("Blocked"));
        rows.extend(
            board
                .blocked
                .into_iter()
                .map(|(i, by)| issue(i, waits_on(by))),
        );
    }
    rows
}

fn waits_on(by: Vec<&str>) -> String {
    if by.is_empty() {
        String::new()
    } else {
        format!("waits on {}", by.join(" "))
    }
}

fn rollup(db: &Db, issue: &Issue) -> String {
    graph::rollup(db, issue)
        .map(|(closed, total)| format!("{closed}/{total}"))
        .unwrap_or_default()
}

fn draw(frame: &mut Frame, screen: &Screen, state: &mut ListState) {
    let [top, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(screen.title.clone()), top);
    let items: Vec<ListItem> = screen
        .rows
        .iter()
        .map(|row| ListItem::new(line(row, &screen.widths, usize::from(body.width))))
        .collect();
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, body, state);
    frame.render_widget(Paragraph::new(screen.view.keys()).dim(), foot);
}

/// A row as one line of `width` columns, the title cut to fit.
fn line<'a>(row: &'a Row, widths: &Widths, width: usize) -> Line<'a> {
    match row {
        Row::Header(h) => Line::from(Span::raw(*h).bold()),
        Row::Note(n) => Line::from(Span::raw(*n).dim()),
        Row::Log(l) => Line::from(Span::raw(l.as_str())),
        Row::Issue {
            lead,
            issue,
            rollup,
            suffix,
        } => {
            let mut spans = vec![
                Span::raw(pad(&format!("{lead}{}", issue.id), widths.id)),
                Span::raw("  "),
                priority(issue.priority),
                Span::raw("  "),
                status(issue.status, widths.status),
                Span::raw("  "),
                Span::raw(pad(&issue.kind.to_string(), widths.kind)).dim(),
                Span::raw("  "),
            ];
            let mut used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            if widths.rollup > 0 {
                let r = format!("{}{rollup}", " ".repeat(widths.rollup - rollup.len()));
                used += widths.rollup + 2;
                spans.push(Span::raw(r).dim());
                spans.push(Span::raw("  "));
            }
            let mut tail = String::new();
            if let Some(a) = &issue.assignee {
                tail = format!("  @{a}");
            }
            if !suffix.is_empty() {
                tail.push_str("  ");
                tail.push_str(suffix);
            }
            let room = width.saturating_sub(used);
            let (title, tail) = fit(&issue.title, &tail, room);
            spans.push(Span::raw(title));
            if !tail.is_empty() {
                spans.push(Span::raw(tail).dim());
            }
            Line::from(spans)
        }
    }
}

fn priority(p: u8) -> Span<'static> {
    let style = match p {
        0 => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        1 => Style::new().fg(Color::Red),
        2 => Style::new().fg(Color::Yellow),
        4 => Style::new().add_modifier(Modifier::DIM),
        _ => Style::new(),
    };
    Span::styled(format!("P{p}"), style)
}

fn status(s: Status, width: usize) -> Span<'static> {
    let style = match s {
        Status::Open => Style::new().fg(Color::Green),
        Status::InProgress => Style::new().fg(Color::Yellow),
        Status::Deferred => Style::new().fg(Color::Blue),
        Status::Closed => Style::new().add_modifier(Modifier::DIM),
    };
    Span::styled(pad(&s.to_string(), width), style)
}

/// The title and its tail cut to share `room` columns: the tail
/// gives way first, down to a third of the room, and only then
/// the title.
fn fit(title: &str, tail: &str, room: usize) -> (String, String) {
    let (t, l) = (title.chars().count(), tail.chars().count());
    if t + l <= room {
        return (title.to_string(), tail.to_string());
    }
    let tail_room = l.min((room / 3).max(room.saturating_sub(t)));
    (cut(title, room - tail_room), cut(tail, tail_room))
}

fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.chars().count())))
}

/// `s` cut to `width` characters, the last one an ellipsis when
/// anything was dropped.
fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    if width > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, test_issue};
    use crate::store::test_db;

    fn db(issues: Vec<Issue>) -> Db {
        let mut db = test_db();
        for i in issues {
            db.issues.insert(i.id.clone(), i);
        }
        db
    }

    fn header(row: &Row) -> Option<&'static str> {
        match row {
            Row::Header(h) => Some(h),
            _ => None,
        }
    }

    fn ids<'a>(rows: &'a [Row]) -> Vec<&'a str> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Issue { issue, .. } => Some(issue.id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn parts<'r>(row: &'r Row<'_>) -> (&'r str, &'r str, &'r str) {
        match row {
            Row::Issue {
                lead,
                rollup,
                suffix,
                ..
            } => (lead, rollup, suffix),
            _ => panic!("an issue row"),
        }
    }

    #[test]
    fn list_rows_nest_children_and_say_what_a_row_waits_on() {
        let mut m = test_issue("t-000001");
        m.kind = Kind::Milestone;
        let mut child = test_issue("t-000002");
        child.parent = Some("t-000001".into());
        let mut held = test_issue("t-000003");
        held.blocked_by = vec!["t-000002".into()];
        held.priority = 1;
        let mut done = test_issue("t-000004");
        done.status = Status::Closed;
        let db = db(vec![m, child, held, done]);
        let rows = list_rows(&db);
        // priority first, then the child under its parent; the
        // closed issue is not listed
        assert_eq!(ids(&rows), ["t-000003", "t-000001", "t-000002"]);
        assert_eq!(parts(&rows[0]), ("", "", "waits on t-000002"));
        assert_eq!(parts(&rows[1]), ("", "0/1", ""));
        assert_eq!(parts(&rows[2]), ("  └─ ", "", ""));
        // every list row takes the cursor
        assert_eq!(step(&rows, None, 1), Some(0));
        assert_eq!(step(&rows, Some(0), 1), Some(1));
    }

    #[test]
    fn board_rows_follow_the_sections_and_the_cursor_skips_headers() {
        let mut m = test_issue("t-000001");
        m.kind = Kind::Milestone;
        let mut busy = test_issue("t-000002");
        busy.status = Status::InProgress;
        let free = test_issue("t-000003");
        let mut held = test_issue("t-000004");
        held.blocked_by = vec!["t-000003".into()];
        let db = db(vec![m, busy, free, held]);
        let rows = board_rows(&db, Timestamp::now());
        let headers: Vec<&str> = rows.iter().filter_map(header).collect();
        assert_eq!(headers, ["Milestones", "In progress", "Ready", "Blocked"]);
        // the milestone waits on nothing listed, so it is ready too
        assert_eq!(
            ids(&rows),
            ["t-000001", "t-000002", "t-000001", "t-000003", "t-000004"]
        );

        // headers are never selected and the ends hold
        let first = step(&rows, None, 1).unwrap();
        assert!(rows[first].selectable() && first == 1);
        let second = step(&rows, Some(first), 1).unwrap();
        assert_eq!(second, 3);
        assert_eq!(step(&rows, Some(second), -1), Some(first));
        assert_eq!(step(&rows, Some(first), -1), Some(first));
        let last = step(&rows, None, -1).unwrap();
        assert_eq!(step(&rows, Some(last), 1), Some(last));
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn an_empty_ready_section_says_so() {
        let mut busy = test_issue("t-000001");
        busy.status = Status::InProgress;
        let db = db(vec![busy]);
        let rows = board_rows(&db, Timestamp::now());
        assert!(matches!(rows[2], Row::Header("Ready")));
        assert!(matches!(rows[3], Row::Note("nothing is ready")));
        assert_eq!(step(&rows, None, 1), Some(1));
    }

    #[test]
    fn the_title_keeps_its_room_before_the_tail() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abc", 0), "");
        assert_eq!(
            fit("title", "  @ann", 20),
            ("title".into(), "  @ann".into())
        );
        // the tail gives way first, down to a third
        let (t, l) = fit("a long title here", "  @someone  lease 9 min left", 30);
        assert_eq!(t, "a long title here");
        assert_eq!(l.chars().count(), 13);
        assert!(l.ends_with('…'));
        // then the title
        let (t, l) = fit("a long title here that goes on", "  @ann", 20);
        assert_eq!(l, "  @ann");
        assert_eq!(t.chars().count(), 14);
    }
}
