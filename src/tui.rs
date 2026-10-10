//! The listing, the board and one issue as screens, with a cursor
//! over the issues.

use std::collections::HashSet;

use anyhow::Result;
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
};
use ratatui::{DefaultTerminal, Frame};

use crate::git::Stamp;
use crate::graph::{self, BOARD_LOG};
use crate::model::{Issue, Kind, Status};
use crate::render;
use crate::store::{DATA_REF, Db, Store};

pub fn run(store: &Store, actor: &str) -> Result<()> {
    ratatui::run(|terminal| screen(terminal, store, actor))
}

/// What narrows the listed rows.
#[derive(Default, Clone, PartialEq, Eq)]
struct Filter {
    status: Option<Status>,
    kind: Option<Kind>,
    /// Only issues the acting person holds, in any session.
    mine: bool,
    /// Text the title or the id must contain, in any case.
    text: String,
    /// Closed issues too, when no status is picked.
    closed: bool,
}

impl Filter {
    fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    fn keeps(&self, issue: &Issue, actor: &str) -> bool {
        let status = match self.status {
            Some(s) => issue.status == s,
            None => {
                matches!(issue.status, Status::Open | Status::InProgress)
                    || (self.closed && issue.status == Status::Closed)
            }
        };
        let mine = !self.mine
            || issue
                .assignee
                .as_deref()
                .is_some_and(|a| person(a) == person(actor));
        let text = self.text.to_lowercase();
        let text = text.is_empty()
            || issue.title.to_lowercase().contains(&text)
            || issue.id.to_lowercase().contains(&text);
        status && issue_kind(issue, self.kind) && mine && text
    }

    /// What is on, for the strip; empty when nothing is.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.status {
            parts.push(s.to_string());
        }
        if let Some(k) = self.kind {
            parts.push(k.to_string());
        }
        if self.mine {
            parts.push("mine".to_string());
        }
        if !self.text.is_empty() {
            parts.push(format!("/{}", self.text));
        }
        if self.closed {
            parts.push("closed".to_string());
        }
        parts.join(" · ")
    }
}

fn issue_kind(issue: &Issue, kind: Option<Kind>) -> bool {
    kind.is_none_or(|k| issue.kind == k)
}

/// The person in an actor name: what comes before the session
/// suffix, or all of it.
fn person(actor: &str) -> &str {
    actor.split('/').next().unwrap_or(actor)
}

/// The value after `cur` in the enum's declared order, and none
/// after the last, so a key cycles through every choice and off.
fn cycle<T: clap::ValueEnum + Copy + PartialEq>(cur: Option<T>) -> Option<T> {
    let all = T::value_variants();
    match cur {
        None => all.first().copied(),
        Some(c) => all
            .iter()
            .position(|v| *v == c)
            .and_then(|i| all.get(i + 1))
            .copied(),
    }
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

    fn name(self) -> &'static str {
        match self {
            View::List => "list",
            View::Board => "board",
        }
    }

    fn keys(self) -> &'static str {
        match self {
            View::List => "j/k move   Enter open   Tab board   r reload   q quit",
            View::Board => "j/k move   Enter open   Tab list   r reload   q quit",
        }
    }
}

/// Where the person is: on the rows, or on an issue's page.
enum Mode {
    Rows,
    Detail(Page),
}

/// An issue on screen, the issues followed to reach it under it,
/// and where the cursor is.
struct Page {
    ids: Vec<String>,
    scroll: usize,
    /// The tie under the cursor, when the cursor is on the ties.
    tie: Option<usize>,
}

impl Page {
    fn open(id: String) -> Page {
        Page {
            ids: vec![id],
            scroll: 0,
            tie: None,
        }
    }

    fn id(&self) -> &str {
        self.ids.last().expect("a page shows an issue")
    }

    fn follow(&mut self, id: String) {
        self.ids.push(id);
        self.scroll = 0;
        self.tie = None;
    }

    /// Back to the issue this one was followed from; `false` on
    /// the first.
    fn back(&mut self) -> bool {
        if self.ids.len() < 2 {
            return false;
        }
        self.ids.pop();
        self.scroll = 0;
        self.tie = None;
        true
    }

    /// Drop, from the top, the issues `db` no longer has.
    fn prune(&mut self, db: &Db) {
        while self
            .ids
            .last()
            .is_some_and(|id| !db.issues.contains_key(id))
        {
            self.ids.pop();
        }
    }
}

/// What the last draw of a page allows: how far the posts scroll,
/// and which issues the ties lead to.
#[derive(Default)]
struct Limits {
    scroll: usize,
    ties: Vec<String>,
}

/// One line of a screen.
enum Row<'a> {
    Header(&'static str),
    Issue {
        /// The indent and connector of a row under its parent.
        lead: String,
        issue: &'a Issue,
        rollup: Option<(usize, usize)>,
        suffix: String,
        /// How many rows under this one are folded away.
        hidden: usize,
    },
    Log {
        oid: String,
        when: String,
        msg: String,
    },
    Note(&'static str),
    /// The bottom edge of a section's box on the board.
    End,
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
    strip: Line<'static>,
    rows: Vec<Row<'a>>,
    widths: Widths,
}

fn screen(terminal: &mut DefaultTerminal, store: &Store, actor: &str) -> Result<()> {
    let mut view = View::List;
    let mut mode = Mode::Rows;
    let mut state = ListState::default();
    let mut preview = true;
    let mut filter = Filter::default();
    // whether keys go into the filter's text
    let mut typing = false;
    let mut folded: HashSet<String> = HashSet::new();
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let mut screen = build(store, &snap.db, view, &filter, actor, &folded)?;
        keep(&mut state, &screen.rows);
        // a page whose issues a reload took away falls back
        if let Mode::Detail(page) = &mut mode {
            page.prune(&snap.db);
            if page.ids.is_empty() {
                mode = Mode::Rows;
            }
        }
        loop {
            let mut limits = Limits::default();
            match &mode {
                Mode::Rows => {
                    let right = match (typing, filter.describe()) {
                        (true, _) => format!("/{}▏", filter.text),
                        (false, on) if on.is_empty() => view.name().to_string(),
                        (false, on) => format!("{on}   {}", view.name()),
                    };
                    terminal.draw(|frame| {
                        draw_rows(frame, &screen, &mut state, &snap.db, preview, &right)
                    })?;
                }
                Mode::Detail(page) => {
                    let issue = &snap.db.issues[page.id()];
                    terminal.draw(|frame| {
                        limits = draw_detail(frame, &screen.strip, &snap.db, issue, page);
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
            if typing {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => typing = false,
                    KeyCode::Backspace => {
                        filter.text.pop();
                    }
                    KeyCode::Char(c) if !ctrl => filter.text.push(c),
                    KeyCode::Char('c') => return Ok(()),
                    _ => {}
                }
                screen = build(store, &snap.db, view, &filter, actor, &folded)?;
                keep(&mut state, &screen.rows);
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Char('r') => break,
                _ => {}
            }
            match &mut mode {
                Mode::Rows => match key.code {
                    KeyCode::Esc if filter.is_empty() => return Ok(()),
                    KeyCode::Esc => {
                        filter = Filter::default();
                        screen = build(store, &snap.db, view, &filter, actor, &folded)?;
                        keep(&mut state, &screen.rows);
                    }
                    KeyCode::Char('p') => preview = !preview,
                    KeyCode::Char(' ') => {
                        let chosen = match state.selected().map(|i| &screen.rows[i]) {
                            Some(Row::Issue { issue, .. })
                                if !graph::children(&snap.db, &issue.id).is_empty() =>
                            {
                                Some(issue.id.clone())
                            }
                            _ => None,
                        };
                        if let Some(id) = chosen {
                            if !folded.remove(&id) {
                                folded.insert(id.clone());
                            }
                            screen = build(store, &snap.db, view, &filter, actor, &folded)?;
                            state.select(screen.rows.iter().position(
                                |r| matches!(r, Row::Issue { issue, .. } if issue.id == id),
                            ));
                        }
                    }
                    KeyCode::Char('s')
                    | KeyCode::Char('t')
                    | KeyCode::Char('a')
                    | KeyCode::Char('c')
                    | KeyCode::Char('/') => {
                        match key.code {
                            KeyCode::Char('s') => filter.status = cycle(filter.status),
                            KeyCode::Char('t') => filter.kind = cycle(filter.kind),
                            KeyCode::Char('a') => filter.mine = !filter.mine,
                            KeyCode::Char('c') => filter.closed = !filter.closed,
                            _ => typing = true,
                        }
                        screen = build(store, &snap.db, view, &filter, actor, &folded)?;
                        keep(&mut state, &screen.rows);
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        view = view.other();
                        screen = build(store, &snap.db, view, &filter, actor, &folded)?;
                        state.select(step(&screen.rows, None, 1));
                    }
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                        if let Some(Row::Issue { issue, .. }) =
                            state.selected().map(|i| &screen.rows[i])
                        {
                            mode = Mode::Detail(Page::open(issue.id.clone()));
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
                Mode::Detail(page) => {
                    let last_tie = limits.ties.len().saturating_sub(1);
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                            if !page.back() {
                                mode = Mode::Rows;
                            }
                        }
                        KeyCode::Tab | KeyCode::BackTab => {
                            page.tie = match page.tie {
                                None if !limits.ties.is_empty() => Some(0),
                                _ => None,
                            };
                        }
                        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                            if let Some(id) = page.tie.and_then(|k| limits.ties.get(k)) {
                                page.follow(id.clone());
                            }
                        }
                        KeyCode::Char('j') | KeyCode::Down => match page.tie {
                            Some(k) => page.tie = Some((k + 1).min(last_tie)),
                            None => page.scroll = (page.scroll + 1).min(limits.scroll),
                        },
                        KeyCode::Char('k') | KeyCode::Up => match page.tie {
                            Some(k) => page.tie = Some(k.saturating_sub(1)),
                            None => page.scroll = page.scroll.saturating_sub(1),
                        },
                        KeyCode::Char('g') | KeyCode::Home => match page.tie {
                            Some(_) => page.tie = Some(0),
                            None => page.scroll = 0,
                        },
                        KeyCode::Char('G') | KeyCode::End => match page.tie {
                            Some(_) => page.tie = Some(last_tie),
                            None => page.scroll = limits.scroll,
                        },
                        _ => {}
                    }
                }
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

fn build<'a>(
    store: &Store,
    db: &'a Db,
    view: View,
    filter: &Filter,
    actor: &str,
    folded: &HashSet<String>,
) -> Result<Screen<'a>> {
    let now = Timestamp::now();
    let stamp = store.git.head_stamp()?;
    let strip = strip(db, &stamp);
    let mut rows = match view {
        View::List => list_rows(db, filter, actor, folded),
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
                rows.push(Row::Log {
                    oid: oid[..7].to_string(),
                    when,
                    msg: msg.clone(),
                });
            }
            rows.push(Row::End);
        }
    }
    let widths = widths(&rows);
    Ok(Screen {
        view,
        strip,
        rows,
        widths,
    })
}

fn widths(rows: &[Row]) -> Widths {
    let mut widths = Widths::default();
    for row in rows {
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
            widths.rollup = widths.rollup.max(rollup_text(*rollup).len());
        }
    }
    widths
}

/// The bar above the rows: the prefix, where HEAD is, and the
/// counts as badges in the status colours.
fn strip(db: &Db, stamp: &Stamp) -> Line<'static> {
    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let mut spans = vec![
        Span::raw(db.meta.prefix.clone()).bold().cyan(),
        Span::raw(format!("  {} @ {}  ", stamp.branch, stamp.commit)).dim(),
    ];
    for (n, s) in [
        Status::Open,
        Status::InProgress,
        Status::Deferred,
        Status::Closed,
    ]
    .into_iter()
    .enumerate()
    {
        if n > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(badge(&format!("{} {}", count(s), s), status_colour(s)));
    }
    Line::from(spans)
}

/// Draw the strip with `right` against the right edge.
fn draw_strip(frame: &mut Frame, area: Rect, strip: &Line, right: &str) {
    let [left, end] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(right.chars().count()).unwrap_or(u16::MAX)),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(strip.clone()), left);
    frame.render_widget(Paragraph::new(right).dim(), end);
}

/// What `foam list` prints: open and in-progress issues by
/// priority, each under its parent when the parent is listed.
fn list_rows<'a>(
    db: &'a Db,
    filter: &Filter,
    actor: &str,
    folded: &HashSet<String>,
) -> Vec<Row<'a>> {
    let mut issues: Vec<&Issue> = db
        .issues
        .values()
        .filter(|i| filter.keeps(i, actor))
        .collect();
    issues.sort_by_key(|i| (i.priority, i.created_at));
    let rows: Vec<(&Issue, String)> = issues
        .into_iter()
        .map(|i| (i, waits_on(graph::open_blockers(db, i))))
        .collect();
    let mut out: Vec<Row> = Vec::new();
    // the tree comes depth first, so everything under a folded
    // row follows it at once and is counted onto it
    let mut hide_below = None;
    for (depth, lead, issue, suffix) in render::forest(&rows) {
        if hide_below.is_some_and(|d| depth > d) {
            if let Some(Row::Issue { hidden, .. }) = out.last_mut() {
                *hidden += 1;
            }
            continue;
        }
        hide_below = folded.contains(&issue.id).then_some(depth);
        out.push(Row::Issue {
            lead,
            issue,
            rollup: rollup(db, issue),
            suffix: suffix.to_string(),
            hidden: 0,
        });
    }
    out
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
            hidden: 0,
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
        rows.push(Row::End);
    }
    if !board.in_progress.is_empty() {
        rows.push(Row::Header("In progress"));
        rows.extend(
            board
                .in_progress
                .into_iter()
                .map(|i| issue(i, render::lease_left(i))),
        );
        rows.push(Row::End);
    }
    rows.push(Row::Header("Ready"));
    if board.ready.is_empty() {
        rows.push(Row::Note("nothing is ready"));
    }
    rows.extend(board.ready.into_iter().map(|i| issue(i, String::new())));
    rows.push(Row::End);
    if !board.blocked.is_empty() {
        rows.push(Row::Header("Blocked"));
        rows.extend(
            board
                .blocked
                .into_iter()
                .map(|(i, by)| issue(i, waits_on(by))),
        );
        rows.push(Row::End);
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

fn rollup(db: &Db, issue: &Issue) -> Option<(usize, usize)> {
    graph::rollup(db, issue)
}

fn rollup_text(r: Option<(usize, usize)>) -> String {
    r.map(|(closed, total)| format!("{closed}/{total}"))
        .unwrap_or_default()
}

/// Green once every child is closed.
fn rollup_style(r: Option<(usize, usize)>) -> Style {
    match r {
        Some((closed, total)) if total > 0 && closed == total => Style::new().fg(Color::Green),
        _ => Style::new(),
    }
}

/// A person's name is cyan, an agent's magenta: the session
/// suffix is what tells them apart.
fn actor(name: &str) -> Style {
    Style::new().fg(if name.contains('/') {
        Color::Magenta
    } else {
        Color::Cyan
    })
}

/// A milestone's kind stands out; every other kind recedes.
fn kind_style(kind: Kind) -> Style {
    if kind == Kind::Milestone {
        Style::new().fg(Color::Magenta)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    }
}

/// A row's suffix, with the ids after "waits on" in the id colour.
fn suffix_spans(suffix: &str) -> Vec<Span<'static>> {
    let Some(ids) = suffix.strip_prefix("waits on ") else {
        return vec![Span::raw(suffix.to_string()).dim()];
    };
    let mut spans = vec![Span::raw("waits on ").dim()];
    for (n, id) in ids.split(' ').enumerate() {
        if n > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::raw(id.to_string()).cyan());
    }
    spans
}

/// `spans` cut to `room` columns, the last kept one ending in an
/// ellipsis when anything was dropped.
fn cut_spans(spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut left = room;
    for span in spans {
        let n = span.content.chars().count();
        if n <= left {
            left -= n;
            out.push(span);
        } else {
            out.push(Span::styled(cut(&span.content, left), span.style));
            break;
        }
    }
    out
}

fn frame_areas(frame: &Frame) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area())
}

/// The preview pane's width, borders included, when it stands
/// beside the rows.
const PREVIEW_WIDTH: u16 = 44;

fn draw_rows(
    frame: &mut Frame,
    screen: &Screen,
    state: &mut ListState,
    db: &Db,
    preview: bool,
    right: &str,
) {
    let [top, body, foot] = frame_areas(frame);
    draw_strip(frame, top, &screen.strip, right);
    let chosen = match state.selected().map(|i| &screen.rows[i]) {
        Some(Row::Issue { issue, .. }) if preview => Some(*issue),
        _ => None,
    };
    let (rows_area, preview_area) = match chosen {
        None => (body, None),
        Some(_) if body.width >= WIDE => {
            let [rows, preview] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(PREVIEW_WIDTH)])
                    .areas(body);
            (rows, Some(preview))
        }
        Some(_) => {
            let [rows, preview] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(body.height / 2)])
                    .areas(body);
            (rows, Some(preview))
        }
    };
    // the gutter takes two columns from every row
    let width = usize::from(rows_area.width.saturating_sub(2));
    let boxed = screen.view == View::Board;
    let columns = Columns::at(if boxed {
        width.saturating_sub(4)
    } else {
        width
    });
    let items: Vec<ListItem> = screen
        .rows
        .iter()
        .map(|row| ListItem::new(line(row, &screen.widths, width, columns, boxed)))
        .collect();
    let list = List::new(items)
        .highlight_style(CHOSEN)
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, rows_area, state);
    if let (Some(issue), Some(area)) = (chosen, preview_area) {
        let lines = preview_lines(db, issue, inner(area.width));
        frame.render_widget(
            Paragraph::new(lines).block(pane(Span::raw(format!(" {} ", issue.id)).cyan(), false)),
            area,
        );
    }
    let keys = format!(
        "{}   Space fold   p {}   s/t/a/c / filter",
        screen.view.keys(),
        if preview { "no preview" } else { "preview" }
    );
    frame.render_widget(Paragraph::new(keys).dim(), foot);
}

/// What the preview shows of an issue: the title, the about
/// block, and as much of the body as fits.
fn preview_lines(db: &Db, issue: &Issue, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(issue, width);
    lines.push(Line::default());
    lines.extend(about(db, issue, width, None).lines);
    if !issue.body.is_empty() {
        lines.push(Line::default());
        let wide = width.max(10);
        lines.extend(
            render::wrap_at(&issue.body, wide, wide)
                .into_iter()
                .map(|l| Line::from(Span::raw(l))),
        );
    }
    lines
}

/// The row under the cursor: a background under its colours,
/// never in place of them.
const CHOSEN: Style = Style::new().bg(Color::DarkGray);

/// The about pane's width, borders included, when it stands
/// beside the posts.
const ABOUT_WIDTH: u16 = 30;

/// A terminal narrower than this stacks the about pane under the
/// header instead.
const WIDE: u16 = 90;

/// Draw an issue's page: the header card, the about pane and the
/// posts, side by side or stacked.
fn draw_detail(frame: &mut Frame, strip: &Line, db: &Db, issue: &Issue, page: &Page) -> Limits {
    let [top, body, foot] = frame_areas(frame);
    draw_strip(frame, top, strip, "issue");
    let wide = body.width >= WIDE;
    let left_w = if wide {
        body.width - ABOUT_WIDTH
    } else {
        body.width
    };
    let heading = heading(issue, inner(left_w));
    let about_w = if wide { ABOUT_WIDTH } else { body.width };
    let about = about(db, issue, inner(about_w), page.tie);
    let card_h = u16::try_from(heading.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let (card, about_area, posts_area) = if wide {
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(ABOUT_WIDTH)]).areas(body);
        let [card, posts] =
            Layout::vertical([Constraint::Length(card_h), Constraint::Min(0)]).areas(left);
        (card, right, posts)
    } else {
        let about_h = u16::try_from(about.lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(body.height / 2);
        let [card, about_area, posts] = Layout::vertical([
            Constraint::Length(card_h),
            Constraint::Length(about_h),
            Constraint::Min(0),
        ])
        .areas(body);
        (card, about_area, posts)
    };

    frame.render_widget(
        Paragraph::new(heading).block(pane(Span::raw(format!(" {} ", issue.id)).cyan(), false)),
        card,
    );

    // the about pane scrolls only to keep the tie under the cursor
    // in view
    let about_scroll = page
        .tie
        .and_then(|k| about.tie_lines.get(k))
        .map_or(0, |&line| {
            line.saturating_sub(rows_in(about_area.height).saturating_sub(1))
        });
    frame.render_widget(
        Paragraph::new(about.lines)
            .block(pane(Span::raw(" about "), page.tie.is_some()))
            .scroll((u16::try_from(about_scroll).unwrap_or(u16::MAX), 0)),
        about_area,
    );

    let lines = posts(issue, inner(posts_area.width));
    let max = lines.len().saturating_sub(rows_in(posts_area.height));
    let scroll = page.scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(Span::raw(" posts "), page.tie.is_none()))
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        posts_area,
    );

    let keys = if page.tie.is_some() {
        "j/k pick   Enter follow   Tab posts   Esc back   q quit"
    } else if about.ties.is_empty() {
        "j/k scroll   g/G ends   Esc back   r reload   q quit"
    } else {
        "j/k scroll   Tab ties   Esc back   r reload   q quit"
    };
    frame.render_widget(Paragraph::new(keys).dim(), foot);
    Limits {
        scroll: max,
        ties: about.ties,
    }
}

/// The columns left inside a pane's border and padding.
fn inner(outer: u16) -> usize {
    usize::from(outer.saturating_sub(4))
}

/// The rows left inside a pane's border.
fn rows_in(outer: u16) -> usize {
    usize::from(outer.saturating_sub(2))
}

/// A bordered pane; the one holding the cursor is drawn plain
/// with a bold title, the others dim.
fn pane(title: Span<'static>, focused: bool) -> Block<'static> {
    let (border, title) = if focused {
        (Style::new(), title.bold())
    } else {
        (Style::new().add_modifier(Modifier::DIM), title.dim())
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title)
        .padding(Padding::horizontal(1))
}

/// The title, bold, wrapped to `width`.
fn heading(i: &Issue, width: usize) -> Vec<Line<'static>> {
    let width = width.max(10);
    render::wrap_at(&i.title, width, width)
        .into_iter()
        .map(|l| Line::from(Span::raw(l).bold()))
        .collect()
}

/// The about pane: badges, dates and ties, with the issues the
/// ties lead to and which line each one is on.
struct About {
    lines: Vec<Line<'static>>,
    ties: Vec<String>,
    tie_lines: Vec<usize>,
}

/// A filled badge, so the eye finds it without reading.
fn badge(text: &str, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new()
            .bg(bg)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
}

fn priority_badge(p: u8) -> Span<'static> {
    let bg = match p {
        0 => Color::Red,
        1 => Color::LightRed,
        2 => Color::Yellow,
        3 => Color::Gray,
        _ => Color::DarkGray,
    };
    badge(&format!("P{p}"), bg)
}

fn status_colour(s: Status) -> Color {
    match s {
        Status::Open => Color::Green,
        Status::InProgress => Color::Yellow,
        Status::Deferred => Color::Blue,
        Status::Closed => Color::DarkGray,
    }
}

fn status_badge(s: Status) -> Span<'static> {
    badge(&s.to_string(), status_colour(s))
}

fn about(db: &Db, i: &Issue, width: usize, tie: Option<usize>) -> About {
    let style = human();
    let width = width.max(16);
    let dim = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::DIM));
    let label = |l: &str| dim(pad(l, 10));
    let mut lines = vec![Line::from(vec![
        priority_badge(i.priority),
        Span::raw("  "),
        Span::styled(i.kind.to_string(), kind_style(i.kind)),
        Span::raw("  "),
        status_badge(i.status),
    ])];
    if let Some(a) = &i.assignee {
        lines.push(Line::from(Span::styled(
            cut(&format!("@{a}"), width),
            actor(a),
        )));
        if i.lease_expires.is_some() {
            lines.push(Line::from(dim(render::lease_left(i))));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        label("created"),
        Span::raw(style.when(i.created_at)),
    ]));
    if let Some(t) = i.defer_until {
        lines.push(Line::from(vec![
            label("deferred"),
            Span::raw(style.until(t)),
        ]));
    }
    if let Some(c) = i.closed_at {
        lines.push(Line::from(vec![label("closed"), Span::raw(style.when(c))]));
        let why = match (i.resolution, &i.close_reason) {
            (Some(r), Some(why)) => format!("{r}: {why}"),
            (Some(r), None) => r.to_string(),
            (None, Some(why)) => why.clone(),
            (None, None) => String::new(),
        };
        if !why.is_empty() {
            lines.extend(
                render::wrap_at(&why, width, width)
                    .into_iter()
                    .map(|l| Line::from(dim(l))),
            );
        }
    }

    let mut ties = Vec::new();
    let mut tie_lines = Vec::new();
    let mut tie_row = |lines: &mut Vec<Line<'static>>, id: &str, rest: Vec<Span<'static>>| {
        let mut spans = vec![Span::raw("› ").dim(), Span::raw(id.to_string()).cyan()];
        spans.extend(rest);
        let mut line = Line::from(spans);
        if tie == Some(ties.len()) {
            line = line.style(CHOSEN);
        }
        ties.push(id.to_string());
        tie_lines.push(lines.len());
        lines.push(line);
    };
    let mut section = Vec::new();
    if let Some(p) = &i.parent {
        section.push(Line::from(dim("parent".into())));
        let title = db
            .issues
            .get(p)
            .map(|parent| {
                vec![dim(cut(
                    &format!("  {}", parent.title),
                    width.saturating_sub(p.len() + 2),
                ))]
            })
            .unwrap_or_default();
        tie_row(&mut section, p, title);
    }
    let children = graph::children(db, &i.id);
    if !children.is_empty() {
        let count = match graph::rollup(db, i) {
            Some((closed, total)) => format!("{closed}/{total} closed"),
            None => children.len().to_string(),
        };
        section.push(Line::from(vec![label("children"), Span::raw(count)]));
        for c in children {
            tie_row(
                &mut section,
                &c.id,
                vec![Span::raw("  "), status(c.status, 0)],
            );
        }
    }
    if !i.blocked_by.is_empty() {
        section.push(Line::from(dim("blocked by".into())));
        for b in &i.blocked_by {
            match db.issues.get(b) {
                Some(o) => tie_row(&mut section, b, vec![Span::raw("  "), status(o.status, 0)]),
                None => section.push(Line::from(vec![
                    Span::raw(format!("  {b}  ")),
                    dim("missing".into()),
                ])),
            }
        }
    }
    if !i.labels.is_empty() {
        section.push(Line::from(dim("labels".into())));
        section.push(Line::from(Span::raw(cut(
            &format!("  {}", i.labels.join(", ")),
            width,
        ))));
    }
    if !section.is_empty() {
        lines.push(Line::default());
        let offset = lines.len();
        for l in &mut tie_lines {
            *l += offset;
        }
        lines.extend(section);
    }
    About {
        lines,
        ties,
        tie_lines,
    }
}

/// The body as the issue's first post, then the notes, each a box
/// `width` wide.
fn posts(i: &Issue, width: usize) -> Vec<Line<'static>> {
    let style = human();
    let wide = width.max(20);
    let mut out = Vec::new();
    if !i.body.is_empty() {
        out.extend(post_box(
            &Post {
                id: None,
                author: None,
                edited: None,
                at: i.created_at,
                text: &i.body,
                branch: &i.stamps.created.branch,
                commit: &i.stamps.created.commit,
            },
            &style,
            wide,
        ));
    }
    for (depth, n) in crate::model::threaded(&i.notes) {
        if !out.is_empty() {
            out.push(Line::default());
        }
        // a reply sits two columns in from what it answers
        let indent = (depth * 2).min(wide.saturating_sub(20));
        let mut lines = post_box(
            &Post {
                id: Some(&n.id),
                author: Some(&n.author),
                edited: n.edited_at,
                at: n.at,
                text: &n.text,
                branch: &n.branch,
                commit: &n.commit,
            },
            &style,
            wide - indent,
        );
        for line in &mut lines {
            line.spans.insert(0, Span::raw(" ".repeat(indent)));
        }
        out.extend(lines);
    }
    out
}

/// What a box on the page shows: a note, or the body as the
/// issue's own first post.
struct Post<'a> {
    id: Option<&'a str>,
    author: Option<&'a str>,
    edited: Option<Timestamp>,
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
    if let Some(id) = p.id {
        from.push_str(id);
        from.push_str(" · ");
    }
    if let Some(e) = p.edited {
        from.push_str(&format!("edited {} · ", style.when(e)));
    }
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

/// Which of the optional columns a row shows at a width: the
/// kind goes first, then the status, and the id, priority and
/// title stay.
#[derive(Clone, Copy)]
struct Columns {
    status: bool,
    kind: bool,
}

impl Columns {
    #[cfg(test)]
    const ALL: Columns = Columns {
        status: true,
        kind: true,
    };

    fn at(width: usize) -> Columns {
        Columns {
            status: width >= 60,
            kind: width >= 80,
        }
    }
}

/// A row as one line of `width` columns, the title cut to fit.
/// On the board the rows are `boxed`: each section's header and
/// end draw its edges and the rows between sit inside them.
fn line<'a>(
    row: &'a Row,
    widths: &Widths,
    width: usize,
    columns: Columns,
    boxed: bool,
) -> Line<'a> {
    let edge = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::DIM));
    let inside = |spans: Vec<Span<'a>>| -> Line<'a> {
        if !boxed {
            return Line::from(spans);
        }
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let mut out = vec![edge("│ ".into())];
        out.extend(spans);
        out.push(edge(format!(
            "{} │",
            " ".repeat(width.saturating_sub(4 + used))
        )));
        Line::from(out)
    };
    match row {
        Row::Header(h) if boxed => Line::from(vec![
            edge("╭─ ".into()),
            Span::raw(*h).bold().cyan(),
            edge(format!(
                " {}╮",
                "─".repeat(width.saturating_sub(h.chars().count() + 5))
            )),
        ]),
        Row::Header(h) => Line::from(Span::raw(*h).bold().cyan()),
        Row::End => Line::from(edge(format!("╰{}╯", "─".repeat(width.saturating_sub(2))))),
        Row::Note(n) => inside(vec![Span::raw(*n).dim()]),
        Row::Log { oid, when, msg } => inside(vec![
            Span::raw(oid.as_str()).dim(),
            Span::raw("  "),
            Span::raw(when.as_str()).dim(),
            Span::raw("  "),
            Span::raw(msg.as_str()),
        ]),
        Row::Issue {
            lead,
            issue,
            rollup,
            suffix,
            hidden,
        } => {
            let id_pad = widths
                .id
                .saturating_sub(lead.chars().count() + issue.id.chars().count());
            let mut spans = vec![
                Span::raw(lead.as_str()).dim(),
                Span::raw(issue.id.as_str()).cyan(),
                Span::raw(format!("{}  ", " ".repeat(id_pad))),
                priority(issue.priority),
                Span::raw("  "),
            ];
            let mut used = widths.id + 2 + 2 + 2;
            if columns.status {
                spans.push(status(issue.status, widths.status));
                spans.push(Span::raw("  "));
                used += widths.status + 2;
            }
            if columns.kind {
                spans.push(Span::styled(
                    pad(&issue.kind.to_string(), widths.kind),
                    kind_style(issue.kind),
                ));
                spans.push(Span::raw("  "));
                used += widths.kind + 2;
            }
            if widths.rollup > 0 {
                let r = rollup_text(*rollup);
                spans.push(Span::raw(" ".repeat(widths.rollup - r.len())));
                spans.push(Span::styled(r, rollup_style(*rollup)));
                spans.push(Span::raw("  "));
                used += widths.rollup + 2;
            }
            let mut tail = Vec::new();
            if let Some(a) = &issue.assignee {
                tail.push(Span::raw("  "));
                tail.push(Span::styled(format!("@{a}"), actor(a)));
            }
            if !suffix.is_empty() {
                tail.push(Span::raw("  "));
                tail.extend(suffix_spans(suffix));
            }
            if *hidden > 0 {
                tail.push(Span::raw(format!("  ▸ {hidden} folded")).dim());
            }
            let room = if boxed {
                width.saturating_sub(4)
            } else {
                width
            }
            .saturating_sub(used);
            let tail_len: usize = tail.iter().map(|s| s.content.chars().count()).sum();
            let (title_room, tail_room) = split(issue.title.chars().count(), tail_len, room);
            spans.push(Span::raw(cut(&issue.title, title_room)));
            spans.extend(cut_spans(tail, tail_room));
            inside(spans)
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

/// How a title `t` columns long and a tail `l` long share `room`
/// columns: the tail gives way first, down to a third of the
/// room, and only then the title.
fn split(t: usize, l: usize, room: usize) -> (usize, usize) {
    if t + l <= room {
        return (t, l);
    }
    let tail_room = l.min((room / 3).max(room.saturating_sub(t)));
    (room - tail_room, tail_room)
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

    /// A row's lead, rollup and suffix, joined with bars.
    fn parts(row: &Row) -> String {
        match row {
            Row::Issue {
                lead,
                rollup,
                suffix,
                ..
            } => format!("{lead}|{}|{suffix}", rollup_text(*rollup)),
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
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        // priority first, then the child under its parent; the
        // closed issue is not listed
        assert_eq!(ids(&rows), ["t-000003", "t-000001", "t-000002"]);
        assert_eq!(parts(&rows[0]), "||waits on t-000002");
        assert_eq!(parts(&rows[1]), "|0/1|");
        assert_eq!(parts(&rows[2]), "  └─ ||");
        // the id is cyan after a dim lead, and the ids a row waits
        // on are cyan inside the dim suffix
        let widths = Widths {
            id: 13,
            status: 4,
            kind: 9,
            rollup: 3,
        };
        let child = line(&rows[2], &widths, 80, Columns::ALL, false);
        assert_eq!(child.spans[0].content, "  └─ ");
        assert!(child.spans[0].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(child.spans[1].content, "t-000002");
        assert_eq!(child.spans[1].style.fg, Some(Color::Cyan));
        let held = line(&rows[0], &widths, 80, Columns::ALL, false);
        let last = held.spans.last().unwrap();
        assert_eq!(last.content, "t-000002");
        assert_eq!(last.style.fg, Some(Color::Cyan));
        assert_eq!(
            line(&rows[1], &widths, 80, Columns::ALL, false)
                .to_string()
                .trim_end(),
            "t-000001       P2  open  milestone  0/1  t"
        );
        // a narrow terminal drops the kind, then the status
        assert_eq!(
            line(&rows[1], &widths, 70, Columns::at(70), false)
                .to_string()
                .trim_end(),
            "t-000001       P2  open  0/1  t"
        );
        assert_eq!(
            line(&rows[1], &widths, 50, Columns::at(50), false)
                .to_string()
                .trim_end(),
            "t-000001       P2  0/1  t"
        );
        // every list row takes the cursor
        assert_eq!(step(&rows, None, 1), Some(0));
        assert_eq!(step(&rows, Some(0), 1), Some(1));
    }

    #[test]
    fn board_rows_follow_the_sections_and_the_cursor_skips_the_edges() {
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
        // each section closes its box
        assert_eq!(rows.iter().filter(|r| matches!(r, Row::End)).count(), 4);
        assert_eq!(rows.len(), 13);

        // edges are never selected and the ends hold
        let first = step(&rows, None, 1).unwrap();
        assert!(rows[first].selectable() && first == 1);
        let second = step(&rows, Some(first), 1).unwrap();
        assert_eq!(second, 4);
        assert_eq!(step(&rows, Some(second), -1), Some(first));
        assert_eq!(step(&rows, Some(first), -1), Some(first));
        let last = step(&rows, None, -1).unwrap();
        assert_eq!(last, 11);
        assert_eq!(step(&rows, Some(last), 1), Some(last));

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

        // boxed, every line is exactly as wide as the screen
        let widths = widths(&rows);
        let lines: Vec<String> = rows
            .iter()
            .map(|r| line(r, &widths, 40, Columns::at(36), true).to_string())
            .collect();
        assert!(lines.iter().all(|l| l.chars().count() == 40), "{lines:#?}");
        assert!(lines[0].starts_with("╭─ Milestones ─"), "{}", lines[0]);
        assert!(lines[0].ends_with("─╮"), "{}", lines[0]);
        assert!(lines[1].starts_with("│ t-000001  P2  "), "{}", lines[1]);
        assert!(lines[1].ends_with(" │"), "{}", lines[1]);
        assert!(lines[2].starts_with("╰─"), "{}", lines[2]);
        assert!(lines[2].ends_with("─╯"), "{}", lines[2]);
    }

    #[test]
    fn an_empty_ready_section_says_so() {
        let mut busy = test_issue("t-000001");
        busy.status = Status::InProgress;
        let db = db(vec![busy]);
        let rows = board_rows(&db, Timestamp::now());
        assert!(matches!(rows[3], Row::Header("Ready")));
        assert!(matches!(rows[4], Row::Note("nothing is ready")));
        assert!(matches!(rows[5], Row::End));
        assert_eq!(step(&rows, None, 1), Some(1));
    }

    /// An issue with a parent, a child, two blockers, a body and a
    /// note, and the db around it.
    fn fixture() -> (Db, Issue) {
        let mut m = test_issue("t-000001");
        m.kind = Kind::Milestone;
        m.title = "the parent".into();
        let mut i = test_issue("t-000002");
        i.title = "a title that is long enough to wrap at forty columns".into();
        i.parent = Some("t-000001".into());
        i.blocked_by = vec!["t-000003".into(), "t-nope".into()];
        i.body = "one paragraph\n\nanother".into();
        i.notes.push(Note {
            id: "0a1b2c3d".into(),
            reply_to: None,
            edited_at: None,
            deleted_at: None,
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
        (db(vec![m, i.clone(), c, b]), i)
    }

    fn strings(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    #[test]
    fn the_preview_stands_beside_the_rows_or_under_them() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (db, _) = fixture();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let screen = Screen {
            view: View::List,
            strip: Line::from("t"),
            widths: widths(&rows),
            rows,
        };
        let render = |width: u16, height: u16, preview: bool| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = ListState::default();
            // the second row is the issue with the body
            state.select(Some(1));
            terminal
                .draw(|f| draw_rows(f, &screen, &mut state, &db, preview, "list"))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        };
        let at = |rows: &[String], needle: &str| rows.iter().position(|r| r.contains(needle));
        // beside: the pane starts where the rows end, and shows the
        // title, the badges and the body
        let wide = render(120, 30, true);
        let col = wide[1]
            .find("╭ t-000002 ")
            .map(|b| wide[1][..b].chars().count());
        assert_eq!(col, Some(76), "{}", wide[1]);
        assert!(
            at(&wide, "│ a title that is long enough").is_some(),
            "{wide:#?}"
        );
        assert!(at(&wide, "│  P2   task   open ").is_some(), "{wide:#?}");
        assert!(at(&wide, "│ one paragraph").is_some(), "{wide:#?}");
        // the rows keep their columns at the narrower width
        assert!(wide[1].starts_with("  t-000001"), "{}", wide[1]);
        // under: the pane takes the lower half
        let narrow = render(70, 30, true);
        assert!(narrow[15].starts_with("╭ t-000002 "), "{}", narrow[15]);
        // off: no pane at all
        let off = render(120, 30, false);
        assert!(at(&off, "╭ t-000002 ").is_none(), "{off:#?}");
    }

    #[test]
    fn a_folded_row_hides_everything_under_it_and_says_how_much() {
        let (db, _) = fixture();
        let hidden = |rows: &[Row]| -> Vec<(String, usize)> {
            rows.iter()
                .filter_map(|r| match r {
                    Row::Issue { issue, hidden, .. } => Some((issue.id.clone(), *hidden)),
                    _ => None,
                })
                .collect()
        };
        let top: HashSet<String> = ["t-000001".to_string()].into();
        let rows = list_rows(&db, &Filter::default(), "ann", &top);
        assert_eq!(hidden(&rows), [("t-000001".to_string(), 2)]);
        let widths = widths(&rows);
        let text = line(&rows[0], &widths, 80, Columns::ALL, false).to_string();
        assert!(text.trim_end().ends_with("t  ▸ 2 folded"), "{text}");
        let middle: HashSet<String> = ["t-000002".to_string()].into();
        let rows = list_rows(&db, &Filter::default(), "ann", &middle);
        assert_eq!(
            hidden(&rows),
            [("t-000001".to_string(), 0), ("t-000002".to_string(), 1)]
        );
    }

    #[test]
    fn a_filter_narrows_the_rows_and_says_what_is_on() {
        let (db, _) = fixture();
        let ids = |f: &Filter| -> Vec<String> {
            list_rows(&db, f, "ann/s1", &HashSet::new())
                .iter()
                .filter_map(|r| match r {
                    Row::Issue { issue, .. } => Some(issue.id.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut f = Filter::default();
        assert!(f.is_empty() && f.describe().is_empty());
        assert_eq!(ids(&f), ["t-000001", "t-000002", "t-000004"]);
        // closed joins the default set, and a status replaces it
        f.closed = true;
        assert_eq!(ids(&f).len(), 4);
        f.status = Some(Status::Closed);
        assert_eq!(ids(&f), ["t-000003"]);
        assert_eq!(f.describe(), "closed · closed");
        f = Filter::default();
        // the kind, text in any case, and mine across sessions
        f.kind = Some(Kind::Milestone);
        assert_eq!(ids(&f), ["t-000001"]);
        f.kind = None;
        f.text = "FORTY".into();
        assert_eq!(ids(&f), ["t-000002"]);
        f.text = "000004".into();
        assert_eq!(ids(&f), ["t-000004"]);
        f.text.clear();
        f.mine = true;
        assert!(ids(&f).is_empty());
        let mut db = db;
        db.issues.get_mut("t-000004").unwrap().assignee = Some("ann/s2".into());
        let rows = list_rows(&db, &f, "ann/s1", &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(f.describe(), "mine");
        // s and t cycle through every value and back to off
        let mut s = None;
        let mut seen = Vec::new();
        loop {
            s = cycle::<Status>(s);
            let Some(v) = s else { break };
            seen.push(v);
        }
        assert_eq!(seen.len(), 4);
        assert_eq!(cycle::<Kind>(None), Some(Kind::Task));
    }

    #[test]
    fn the_strip_counts_every_status_in_its_colour() {
        let mut busy = test_issue("t-000001");
        busy.status = Status::InProgress;
        let db = db(vec![busy, test_issue("t-000002")]);
        let stamp = Stamp {
            commit: "abc1234".into(),
            branch: "dev".into(),
        };
        let line = strip(&db, &stamp);
        assert_eq!(
            line.to_string(),
            "t  dev @ abc1234   1 open   1 in_progress   0 deferred   0 closed "
        );
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(line.spans[2].style.bg, Some(Color::Green));
        assert_eq!(line.spans[4].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn the_heading_wraps_the_title() {
        let (_, i) = fixture();
        assert_eq!(
            strings(&heading(&i, 30)),
            ["a title that is long enough to", "wrap at forty columns"]
        );
    }

    #[test]
    fn the_about_pane_has_badges_dates_and_ties_in_order() {
        let (db, i) = fixture();
        let about = about(&db, &i, 26, Some(1));
        let lines = strings(&about.lines);
        assert_eq!(lines[0], " P2   task   open ");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "created   just now");
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "parent");
        assert_eq!(lines[5], "› t-000001  the parent");
        assert_eq!(lines[6], "children  1");
        assert_eq!(lines[7], "› t-000004  open");
        assert_eq!(lines[8], "blocked by");
        assert_eq!(lines[9], "› t-000003  closed");
        assert_eq!(lines[10], "  t-nope  missing");
        assert_eq!(lines.len(), 11);
        assert_eq!(about.ties, ["t-000001", "t-000004", "t-000003"]);
        assert_eq!(about.tie_lines, [5, 7, 9]);
        // the tie under the cursor stands out, the others do not
        let chosen = |l: &Line| l.style.bg == Some(Color::DarkGray);
        assert!(chosen(&about.lines[7]));
        assert!(!chosen(&about.lines[5]) && !chosen(&about.lines[9]));
    }

    #[test]
    fn the_posts_are_the_body_then_the_notes_as_boxes() {
        let (_, i) = fixture();
        let lines = strings(&posts(&i, 40));
        let wide = |l: &String| l.chars().count() == 40;
        assert!(
            lines.iter().filter(|l| !l.is_empty()).all(wide),
            "{lines:#?}"
        );
        // the body carries the creation stamp and no author
        assert!(lines[0].starts_with("╭─ just now ─"), "{}", lines[0]);
        assert_eq!(lines[1], format!("│ {:<36} │", "one paragraph"));
        assert_eq!(lines[2], format!("│ {:<36} │", ""));
        assert_eq!(lines[3], format!("│ {:<36} │", "another"));
        assert!(lines[4].ends_with(" main @ none ─╯"), "{}", lines[4]);
        assert_eq!(lines[5], "");
        assert!(lines[6].starts_with("╭─ ann · just now ─"), "{}", lines[6]);
        assert!(lines[6].ends_with("─╮"), "{}", lines[6]);
        assert_eq!(
            lines[7],
            format!("│ {:<36} │", "a note long enough that it has to")
        );
        assert_eq!(lines[8], format!("│ {:<36} │", "wrap onto a second line"));
        assert!(lines[9].ends_with(" dev @ abc1234 ─╯"), "{}", lines[9]);
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn a_page_follows_ties_and_comes_back_the_same_way() {
        let (db, _) = fixture();
        let mut page = Page::open("t-000002".into());
        page.scroll = 3;
        page.follow("t-000001".into());
        assert_eq!(page.id(), "t-000001");
        assert_eq!(page.scroll, 0);
        assert!(page.back());
        assert_eq!(page.id(), "t-000002");
        assert!(!page.back());
        // a reload that removed the issues on top drops to the
        // first that remains
        page.follow("t-gone".into());
        page.follow("t-also-gone".into());
        page.prune(&db);
        assert_eq!(page.id(), "t-000002");
        page.ids.clear();
        page.prune(&db);
        assert!(page.ids.is_empty());
    }

    #[test]
    fn the_page_puts_the_about_pane_beside_or_under_the_card() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (db, i) = fixture();
        let title = Line::from("t");
        let render = |width: u16, height: u16, tie: Option<usize>| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut page = Page::open(i.id.clone());
            page.tie = tie;
            terminal
                .draw(|f| {
                    draw_detail(f, &title, &db, &i, &page);
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let rows = |width: u16, height: u16| {
            let buffer = render(width, height, None);
            (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        };
        // the pane with the cursor is lit, the others dim, and an
        // id in the about pane is cyan
        let dim = |cell: &ratatui::buffer::Cell| cell.modifier.contains(Modifier::DIM);
        let buffer = render(100, 30, None);
        assert!(!dim(&buffer[(0, 4)]) && dim(&buffer[(70, 1)]));
        assert_eq!(buffer[(74, 7)].fg, Color::Cyan);
        let buffer = render(100, 30, Some(0));
        assert!(dim(&buffer[(0, 4)]) && !dim(&buffer[(70, 1)]));
        let col_of = |row: &str, needle: &str| row.find(needle).map(|b| row[..b].chars().count());

        // wide: the card and the posts on the left, the about pane
        // at the right edge, all starting under the title line
        let wide = rows(100, 30);
        assert!(wide[1].starts_with("╭ t-000002 "), "{}", wide[1]);
        assert_eq!(col_of(&wide[1], "╭ about "), Some(70), "{}", wide[1]);
        assert!(wide[2].contains("││  P2   task   open "), "{}", wide[2]);
        let posts = wide.iter().position(|r| r.starts_with("╭ posts ")).unwrap();
        let body = &wide[posts + 1];
        assert!(body.starts_with("│ ╭─ just now "), "{body}");
        assert!(
            body.ends_with("╮ ││                            │"),
            "{body}"
        );

        // narrow: card, about and posts stacked, each full width
        let narrow = rows(60, 30);
        let at = |needle: &str| narrow.iter().position(|r| r.starts_with(needle)).unwrap();
        let (card, about, posts) = (at("╭ t-000002 "), at("╭ about "), at("╭ posts "));
        assert!(card < about && about < posts, "{narrow:#?}");
        assert!(narrow[about].ends_with('╮'), "{}", narrow[about]);
    }

    #[test]
    fn the_title_keeps_its_room_before_the_tail() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abc", 0), "");
        assert_eq!(split(5, 6, 20), (5, 6));
        // the tail gives way first, down to a third
        assert_eq!(split(17, 28, 30), (17, 13));
        // then the title
        assert_eq!(split(30, 6, 20), (14, 6));
        // a cut span keeps its style and ends the line
        let spans = cut_spans(
            vec![
                Span::raw("ab").dim(),
                Span::raw("cdef").cyan(),
                Span::raw("gh"),
            ],
            4,
        );
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].content, "c…");
        assert_eq!(spans[1].style.fg, Some(Color::Cyan));
    }
}
