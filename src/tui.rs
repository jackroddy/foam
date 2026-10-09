//! The board as a screen: what `foam board` prints, with a
//! cursor over the issues.

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

/// One line of the board.
enum Row<'a> {
    Header(&'static str),
    Issue {
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
/// so the columns line up down the whole board.
#[derive(Default)]
struct Widths {
    id: usize,
    status: usize,
    kind: usize,
    rollup: usize,
}

struct Board<'a> {
    title: Line<'static>,
    rows: Vec<Row<'a>>,
    widths: Widths,
}

fn screen(terminal: &mut DefaultTerminal, store: &Store) -> Result<()> {
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let board = build(store, &snap.db)?;
        let mut state = ListState::default();
        state.select(step(&board.rows, None, 1));
        loop {
            terminal.draw(|frame| draw(frame, &board, &mut state))?;
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
                KeyCode::Char('j') | KeyCode::Down => {
                    state.select(step(&board.rows, state.selected(), 1));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    state.select(step(&board.rows, state.selected(), -1));
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    state.select(step(&board.rows, None, 1));
                }
                KeyCode::Char('G') | KeyCode::End => {
                    state.select(step(&board.rows, None, -1));
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

fn build<'a>(store: &Store, db: &'a Db) -> Result<Board<'a>> {
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
    let mut rows = rows(db, now);
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
    let mut widths = Widths::default();
    for row in &rows {
        if let Row::Issue { issue, rollup, .. } = row {
            widths.id = widths.id.max(issue.id.chars().count());
            widths.status = widths.status.max(issue.status.to_string().len());
            widths.kind = widths.kind.max(issue.kind.to_string().len());
            widths.rollup = widths.rollup.max(rollup.len());
        }
    }
    Ok(Board {
        title,
        rows,
        widths,
    })
}

/// The issue sections of the board, in the order `foam board`
/// prints them.
fn rows(db: &Db, now: Timestamp) -> Vec<Row<'_>> {
    let board = graph::board(db, now);
    let mut rows = Vec::new();
    if !board.milestones.is_empty() {
        rows.push(Row::Header("Milestones"));
        rows.extend(board.milestones.into_iter().map(|issue| Row::Issue {
            issue,
            rollup: rollup(db, issue),
            suffix: String::new(),
        }));
    }
    if !board.in_progress.is_empty() {
        rows.push(Row::Header("In progress"));
        rows.extend(board.in_progress.into_iter().map(|issue| Row::Issue {
            issue,
            rollup: rollup(db, issue),
            suffix: render::lease_left(issue),
        }));
    }
    rows.push(Row::Header("Ready"));
    if board.ready.is_empty() {
        rows.push(Row::Note("nothing is ready"));
    }
    rows.extend(board.ready.into_iter().map(|issue| Row::Issue {
        issue,
        rollup: rollup(db, issue),
        suffix: String::new(),
    }));
    if !board.blocked.is_empty() {
        rows.push(Row::Header("Blocked"));
        rows.extend(board.blocked.into_iter().map(|(issue, by)| Row::Issue {
            issue,
            rollup: rollup(db, issue),
            suffix: format!("waits on {}", by.join(" ")),
        }));
    }
    rows
}

fn rollup(db: &Db, issue: &Issue) -> String {
    graph::rollup(db, issue)
        .map(|(closed, total)| format!("{closed}/{total}"))
        .unwrap_or_default()
}

fn draw(frame: &mut Frame, board: &Board, state: &mut ListState) {
    let [top, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(board.title.clone()), top);
    let items: Vec<ListItem> = board
        .rows
        .iter()
        .map(|row| ListItem::new(line(row, &board.widths, usize::from(body.width))))
        .collect();
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, body, state);
    frame.render_widget(
        Paragraph::new("j/k move   g/G ends   r reload   q quit").dim(),
        foot,
    );
}

/// A row as one line of `width` columns, the title cut to fit.
fn line<'a>(row: &'a Row, widths: &Widths, width: usize) -> Line<'a> {
    match row {
        Row::Header(h) => Line::from(Span::raw(*h).bold()),
        Row::Note(n) => Line::from(Span::raw(*n).dim()),
        Row::Log(l) => Line::from(Span::raw(l.as_str())),
        Row::Issue {
            issue,
            rollup,
            suffix,
        } => {
            let mut spans = vec![
                Span::raw(pad(&issue.id, widths.id)),
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

    #[test]
    fn rows_follow_the_board_sections_and_the_cursor_skips_headers() {
        let mut m = test_issue("t-000001");
        m.kind = Kind::Milestone;
        let mut busy = test_issue("t-000002");
        busy.status = Status::InProgress;
        let free = test_issue("t-000003");
        let mut held = test_issue("t-000004");
        held.blocked_by = vec!["t-000003".into()];
        let db = db(vec![m, busy, free, held]);
        let rows = rows(&db, Timestamp::now());
        let headers: Vec<&str> = rows.iter().filter_map(header).collect();
        assert_eq!(headers, ["Milestones", "In progress", "Ready", "Blocked"]);
        // the milestone waits on nothing listed, so it is ready too
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Issue { issue, .. } => Some(issue.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
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
        let rows = rows(&db, Timestamp::now());
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
