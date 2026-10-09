//! The listing, the board and one issue as screens, with a cursor
//! over the issues.

use anyhow::Result;
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
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

/// Which rows are up.
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
            View::List => "j/k move   Enter open   Tab board   r reload   q quit",
            View::Board => "j/k move   Enter open   Tab list   r reload   q quit",
        }
    }
}

/// Where the person is: on the rows, or inside one issue.
enum Mode {
    Rows,
    Detail { id: String, scroll: usize },
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
    let mut mode = Mode::Rows;
    let mut state = ListState::default();
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let mut screen = build(store, &snap.db, view)?;
        keep(&mut state, &screen.rows);
        loop {
            // an issue that a reload took away leaves its screen
            if let Mode::Detail { id, .. } = &mode
                && !snap.db.issues.contains_key(id)
            {
                mode = Mode::Rows;
            }
            let mut max_scroll = 0;
            match &mode {
                Mode::Rows => {
                    terminal.draw(|frame| draw_rows(frame, &screen, &mut state))?;
                }
                Mode::Detail { id, scroll } => {
                    let issue = &snap.db.issues[id];
                    terminal.draw(|frame| {
                        max_scroll = draw_detail(frame, &screen.title, &snap.db, issue, *scroll);
                    })?;
                }
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Char('r') => break,
                _ => {}
            }
            match &mut mode {
                Mode::Rows => match key.code {
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Tab | KeyCode::BackTab => {
                        view = view.other();
                        screen = build(store, &snap.db, view)?;
                        state.select(step(&screen.rows, None, 1));
                    }
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                        if let Some(Row::Issue { issue, .. }) =
                            state.selected().map(|i| &screen.rows[i])
                        {
                            mode = Mode::Detail {
                                id: issue.id.clone(),
                                scroll: 0,
                            };
                        }
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
                },
                Mode::Detail { scroll, .. } => match key.code {
                    KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                        mode = Mode::Rows;
                    }
                    KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(max_scroll),
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                    KeyCode::Char('G') | KeyCode::End => *scroll = max_scroll,
                    _ => {}
                },
            }
        }
    }
}

/// Keep the cursor where it was across a reload, as long as that
/// is still an issue row; otherwise the first one.
fn keep(state: &mut ListState, rows: &[Row]) {
    let kept = state
        .selected()
        .filter(|&i| i < rows.len() && rows[i].selectable());
    state.select(kept.or_else(|| step(rows, None, 1)));
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

/// The CLI's terminal style, for the relative times it renders.
fn human() -> render::Style {
    render::Style {
        human: true,
        color: false,
        width: 80,
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
            let style = human();
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

fn frame_areas(frame: &Frame) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area())
}

fn draw_rows(frame: &mut Frame, screen: &Screen, state: &mut ListState) {
    let [top, body, foot] = frame_areas(frame);
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

/// Draw one issue scrolled `scroll` lines down, and return how
/// far down it can scroll.
fn draw_detail(frame: &mut Frame, title: &Line, db: &Db, issue: &Issue, scroll: usize) -> usize {
    let [top, body, foot] = frame_areas(frame);
    frame.render_widget(Paragraph::new(title.clone()), top);
    let lines = detail(db, issue, usize::from(body.width));
    let max = lines.len().saturating_sub(usize::from(body.height));
    let scroll = scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        body,
    );
    frame.render_widget(
        Paragraph::new("j/k scroll   g/G ends   Esc back   r reload   q quit").dim(),
        foot,
    );
    max
}

/// One issue as a page: a title block, a line of badges, the
/// dates, what it is tied to, then the body and the notes as
/// posts, wrapped to `width`.
fn detail(db: &Db, i: &Issue, width: usize) -> Vec<Line<'static>> {
    let style = human();
    let dim = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::DIM));
    let mut out = Vec::new();
    let head = format!("{}  ", i.id);
    let room = width.saturating_sub(head.len()).max(20);
    for (n, l) in render::wrap_at(&i.title, room, room)
        .into_iter()
        .enumerate()
    {
        let lead = if n == 0 {
            dim(head.clone())
        } else {
            Span::raw(" ".repeat(head.len()))
        };
        out.push(Line::from(vec![lead, Span::raw(l).bold()]));
    }
    let mut badges = vec![
        priority(i.priority),
        Span::raw(format!("  {}  ", i.kind)),
        status(i.status, 0),
    ];
    if let Some(a) = &i.assignee {
        badges.push(dim(format!("  @{a}")));
        if i.lease_expires.is_some() {
            badges.push(dim(format!(" · {}", render::lease_left(i))));
        }
    }
    out.push(Line::from(badges));
    let mut dates = format!("created {}", style.when(i.created_at));
    if let Some(t) = i.defer_until {
        dates.push_str(&format!(" · deferred until {}", style.until(t)));
    }
    if let Some(c) = i.closed_at {
        dates.push_str(&format!(" · closed {}", style.when(c)));
        match (i.resolution, &i.close_reason) {
            (Some(r), Some(why)) => dates.push_str(&format!(", {r}: {why}")),
            (Some(r), None) => dates.push_str(&format!(", {r}")),
            (None, Some(why)) => dates.push_str(&format!(": {why}")),
            (None, None) => {}
        }
    }
    out.push(Line::from(dim(dates)));

    // the ties, each row labelled in a column of its own
    let label = |l: &str| dim(pad(l, 10));
    let blank = || Span::raw(" ".repeat(10));
    let mut ties = Vec::new();
    if let Some(p) = &i.parent {
        let mut spans = vec![label("parent"), Span::raw(p.clone())];
        if let Some(parent) = db.issues.get(p) {
            spans.push(dim(format!("  {}", parent.title)));
        }
        ties.push(Line::from(spans));
    }
    let children = graph::children(db, &i.id);
    if !children.is_empty() {
        let count = match graph::rollup(db, i) {
            Some((closed, total)) => format!("{closed}/{total} closed"),
            None => children.len().to_string(),
        };
        ties.push(Line::from(vec![label("children"), Span::raw(count)]));
        for c in children {
            ties.push(Line::from(vec![
                blank(),
                Span::raw(format!("{}  ", c.id)),
                status(c.status, 0),
                Span::raw(format!("  {}", c.title)),
                dim(render::rollup_tag(db, c)),
            ]));
        }
    }
    for (n, b) in i.blocked_by.iter().enumerate() {
        let status = match db.issues.get(b) {
            Some(o) => status(o.status, 0),
            None => Span::raw("missing"),
        };
        ties.push(Line::from(vec![
            if n == 0 { label("blocked") } else { blank() },
            Span::raw(format!("{b}  ")),
            status,
        ]));
    }
    if !i.labels.is_empty() {
        ties.push(Line::from(vec![
            label("labels"),
            Span::raw(i.labels.join(", ")),
        ]));
    }
    if !ties.is_empty() {
        out.push(Line::default());
        out.extend(ties);
    }

    let wide = width.max(20);
    if !i.body.is_empty() {
        out.push(Line::default());
        out.extend(post_box(
            &Post {
                author: None,
                at: i.created_at,
                text: &i.body,
                branch: &i.stamps.created.branch,
                commit: &i.stamps.created.commit,
            },
            &style,
            wide,
        ));
    }
    for n in &i.notes {
        out.push(Line::default());
        out.extend(post_box(
            &Post {
                author: Some(&n.author),
                at: n.at,
                text: &n.text,
                branch: &n.branch,
                commit: &n.commit,
            },
            &style,
            wide,
        ));
    }
    out
}

/// What a box on the page shows: a note, or the body as the
/// issue's own first post.
struct Post<'a> {
    author: Option<&'a str>,
    at: Timestamp,
    text: &'a str,
    branch: &'a str,
    commit: &'a str,
}

/// A post as a rounded box `width` columns wide: who and when in
/// the top border, the text inside, where it was written in the
/// bottom border.
fn post_box(p: &Post, style: &render::Style, width: usize) -> Vec<Line<'static>> {
    let border = Style::new().add_modifier(Modifier::DIM);
    let edge = |s: String| Span::styled(s, border);
    let mut top = vec![edge("╭─ ".into())];
    let mut used = 2 + 1 + 1;
    if let Some(author) = p.author {
        // the session suffix marks an agent, and the colour says
        // so from across the room
        let who = Style::new()
            .add_modifier(Modifier::BOLD)
            .fg(if author.contains('/') {
                Color::Magenta
            } else {
                Color::Cyan
            });
        top.push(Span::styled(author.to_string(), who));
        top.push(edge(" · ".into()));
        used += author.chars().count() + 3;
    }
    let when = format!("{} ", style.when(p.at));
    used += when.chars().count();
    top.push(edge(when));
    top.push(edge("─".repeat(width.saturating_sub(used))));
    top.push(edge("╮".into()));
    let mut out = vec![Line::from(top)];
    let inner = width.saturating_sub(4).max(16);
    for l in render::wrap_at(p.text, inner, inner) {
        out.push(Line::from(vec![
            edge("│ ".into()),
            Span::raw(pad(&l, inner)),
            edge(" │".into()),
        ]));
    }
    let mut from = String::new();
    if !p.branch.is_empty() {
        from.push_str(p.branch);
    }
    if !p.commit.is_empty() {
        if !from.is_empty() {
            from.push_str(" @ ");
        }
        from.extend(p.commit.chars().take(7));
    }
    if !from.is_empty() {
        from = format!(" {from} ");
    }
    let rest = width.saturating_sub(2 + from.chars().count() + 1);
    out.push(Line::from(vec![
        edge("╰".into()),
        edge("─".repeat(rest)),
        edge(from),
        edge("─╯".into()),
    ]));
    out
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
    use crate::model::{Kind, Note, test_issue};
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

        // a reload keeps the cursor on an issue row and nowhere else
        let mut state = ListState::default();
        state.select(Some(second));
        keep(&mut state, &rows);
        assert_eq!(state.selected(), Some(second));
        state.select(Some(0));
        keep(&mut state, &rows);
        assert_eq!(state.selected(), Some(first));
        state.select(Some(99));
        keep(&mut state, &rows);
        assert_eq!(state.selected(), Some(first));
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
    fn detail_shows_what_show_prints_wrapped_to_the_width() {
        let mut m = test_issue("t-000001");
        m.kind = Kind::Milestone;
        m.title = "the parent".into();
        let mut i = test_issue("t-000002");
        i.title = "a title that is long enough to wrap at forty columns".into();
        i.parent = Some("t-000001".into());
        i.blocked_by = vec!["t-000003".into(), "t-nope".into()];
        i.body = "one paragraph\n\nanother".into();
        i.notes.push(Note {
            at: Timestamp::now(),
            author: "ann".into(),
            text: "a note long enough that it has to wrap onto a second line".into(),
            commit: "abc1234def".into(),
            branch: "dev".into(),
        });
        let mut c = test_issue("t-000004");
        c.parent = Some("t-000002".into());
        let mut b = test_issue("t-000003");
        b.status = Status::Closed;
        let db = db(vec![m, i.clone(), c, b]);
        let lines: Vec<String> = detail(&db, &i, 40).iter().map(Line::to_string).collect();
        assert!(lines.iter().all(|l| l.chars().count() <= 40), "{lines:#?}");
        assert_eq!(lines[0], "t-000002  a title that is long enough to");
        assert_eq!(lines[1], "          wrap at forty columns");
        assert_eq!(lines[2], "P2  task  open");
        assert_eq!(lines[3], "created just now");
        assert_eq!(lines[4], "");
        assert_eq!(lines[5], "parent    t-000001  the parent");
        assert_eq!(lines[6], "children  1");
        assert_eq!(lines[7], "          t-000004  open  t");
        assert_eq!(lines[8], "blocked   t-000003  closed");
        assert_eq!(lines[9], "          t-nope  missing");
        assert_eq!(lines[10], "");
        // the body is the first post, with the creation stamp
        let wide = |l: &String| l.chars().count() == 40;
        assert!(lines[11..16].iter().all(wide), "{lines:#?}");
        assert!(lines[11].starts_with("╭─ just now ─"), "{}", lines[11]);
        assert_eq!(lines[12], format!("│ {:<36} │", "one paragraph"));
        assert_eq!(lines[13], format!("│ {:<36} │", ""));
        assert_eq!(lines[14], format!("│ {:<36} │", "another"));
        assert!(lines[15].ends_with(" main @ none ─╯"), "{}", lines[15]);
        assert_eq!(lines[16], "");
        // then the note, author first
        assert!(lines[17..21].iter().all(wide), "{lines:#?}");
        assert!(
            lines[17].starts_with("╭─ ann · just now ─"),
            "{}",
            lines[17]
        );
        assert!(lines[17].ends_with("─╮"), "{}", lines[17]);
        assert_eq!(
            lines[18],
            format!("│ {:<36} │", "a note long enough that it has to")
        );
        assert_eq!(lines[19], format!("│ {:<36} │", "wrap onto a second line"));
        assert!(lines[20].ends_with(" dev @ abc1234 ─╯"), "{}", lines[20]);
        assert_eq!(lines.len(), 21);
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
