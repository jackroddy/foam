//! The listing, the board and one issue as screens, with a cursor
//! over the issues.

use std::collections::HashSet;
use std::ops::Range;

use anyhow::Result;
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
};
use ratatui::{DefaultTerminal, Frame};

use crate::git::{Distance, Stamp};
use crate::graph::{self, BOARD_LOG};
use crate::md;
use crate::model::{Issue, Kind, Memory, Status};
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
    /// The filter in a few words; on the memories only the text
    /// applies, so only it shows.
    fn describe(&self, view: View) -> String {
        let mut parts = Vec::new();
        if view == View::Memories {
            if !self.text.is_empty() {
                parts.push(format!("/{}", self.text));
            }
            return parts.join(" · ");
        }
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

    /// Whether a memory's slug or text contains the text.
    fn keeps_memory(&self, m: &Memory) -> bool {
        let text = self.text.to_lowercase();
        text.is_empty()
            || m.slug.to_lowercase().contains(&text)
            || m.text.to_lowercase().contains(&text)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// What `foam list` prints: open and in-progress issues as
    /// a tree.
    List,
    /// What `foam board` prints: milestones, in progress, ready,
    /// blocked and the recent log.
    Board,
    /// What `foam memories` prints, with how far behind each one is.
    Memories,
}

impl View {
    fn next(self) -> View {
        match self {
            View::List => View::Board,
            View::Board => View::Memories,
            View::Memories => View::List,
        }
    }

    fn previous(self) -> View {
        match self {
            View::List => View::Memories,
            View::Board => View::List,
            View::Memories => View::Board,
        }
    }

    fn name(self) -> &'static str {
        match self {
            View::List => "list",
            View::Board => "board",
            View::Memories => "memories",
        }
    }

    fn keys(self) -> &'static str {
        match self {
            View::List => "j/k move   Enter open   Tab board   r reload   q quit",
            View::Board => "j/k move   Enter open   Tab memories   r reload   q quit",
            View::Memories => "j/k move   Enter open   Tab list   r reload   q quit",
        }
    }
}

/// Where the person is: on the rows, on an issue's page, or on a
/// memory's.
enum Mode {
    Rows,
    Detail(Page),
    Recall(Recall),
}

/// A memory on screen, and how far its text has scrolled.
struct Recall {
    slug: String,
    scroll: usize,
}

/// An issue on screen, the issues followed to reach it under it,
/// and where the cursor is.
struct Page {
    ids: Vec<String>,
    scroll: usize,
    /// The box under the cursor, counting the body as the first.
    cursor: usize,
    /// Whether the next draw scrolls the chosen box into view.
    reveal: bool,
    /// Notes whose replies are hidden.
    folded: HashSet<String>,
    /// The tie under the cursor, when the cursor is on the ties.
    tie: Option<usize>,
}

impl Page {
    fn open(id: String) -> Page {
        Page {
            ids: vec![id],
            scroll: 0,
            cursor: 0,
            reveal: true,
            folded: HashSet::new(),
            tie: None,
        }
    }

    fn go(&mut self, to: usize) {
        self.cursor = to;
        self.reveal = true;
    }

    /// The cursor onto the next box, or the previous one.
    fn step_box(&mut self, boxes: &[NoteBox], by: isize) {
        let last = boxes.len().saturating_sub(1);
        self.go(match by.is_positive() {
            true => (self.cursor + 1).min(last),
            false => self.cursor.saturating_sub(1),
        });
    }

    /// The cursor onto the note this one answers; a top-level note
    /// answers the issue, so the body.
    fn up(&mut self, boxes: &[NoteBox]) {
        let Some(b) = boxes.get(self.cursor) else {
            return;
        };
        let to = b
            .reply_to
            .as_deref()
            .and_then(|p| boxes.iter().position(|x| x.id.as_deref() == Some(p)));
        self.go(to.unwrap_or(0));
    }

    /// The cursor onto the next, or the previous, box answering
    /// the same note; stays put when there is none.
    fn sibling(&mut self, boxes: &[NoteBox], by: isize) {
        let Some(b) = boxes.get(self.cursor) else {
            return;
        };
        let same = |i: &usize| boxes[*i].reply_to == b.reply_to;
        let to = match by.is_positive() {
            true => (self.cursor + 1..boxes.len()).find(same),
            false => (0..self.cursor).rev().find(same),
        };
        if let Some(to) = to {
            self.go(to);
        }
    }

    /// Hide the replies under the chosen note, or show them again.
    fn fold(&mut self, boxes: &[NoteBox]) {
        let Some(NoteBox {
            id: Some(id),
            replies: 1..,
            ..
        }) = boxes.get(self.cursor)
        else {
            return;
        };
        if !self.folded.remove(id) {
            self.folded.insert(id.clone());
        }
        self.reveal = true;
    }

    fn id(&self) -> &str {
        self.ids.last().expect("a page shows an issue")
    }

    fn follow(&mut self, id: String) {
        self.ids.push(id);
        self.scroll = 0;
        self.go(0);
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
        self.go(0);
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

/// What the last draw of a page allows: how far the notes scroll,
/// how many rows the pane shows, the boxes on it, and which issues
/// the ties lead to.
#[derive(Debug, Default)]
struct Limits {
    scroll: usize,
    rows: usize,
    boxes: Vec<NoteBox>,
    ties: Vec<String>,
}

/// One box on the page, and the lines it spans.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteBox {
    /// `None` for the body.
    id: Option<String>,
    reply_to: Option<String>,
    /// How many replies are under it, shown or hidden.
    replies: usize,
    lines: Range<usize>,
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
    Memory {
        memory: &'a Memory,
        /// How far behind HEAD it was remembered.
        distance: Distance,
        /// Whether it may no longer hold: old, or from another
        /// branch, by the rule prime uses.
        marked: bool,
    },
    /// The bottom edge of a section's box on the board.
    End,
}

impl Row<'_> {
    fn selectable(&self) -> bool {
        matches!(self, Row::Issue { .. } | Row::Memory { .. })
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
    slug: usize,
}

struct Screen<'a> {
    view: View,
    header: Header,
    rows: Vec<Row<'a>>,
    widths: Widths,
}

/// What every screen says at the top: the prefix, where HEAD is,
/// and the counts as badges in the status colours.
struct Header {
    prefix: String,
    stamp: String,
    counts: Vec<Span<'static>>,
}

/// Everything a key can change.
struct State {
    view: View,
    mode: Mode,
    list: ListState,
    preview: bool,
    filter: Filter,
    /// Whether keys go into the filter's text.
    typing: bool,
    folded: HashSet<String>,
}

impl State {
    fn new() -> State {
        State {
            view: View::List,
            mode: Mode::Rows,
            list: ListState::default(),
            preview: true,
            filter: Filter::default(),
            typing: false,
            folded: HashSet::new(),
        }
    }
}

/// What the loop does once a key has changed the state.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    /// Read the ref again and build the rows from scratch.
    Reload,
    Draw,
    /// Build the rows again from the same data, and put the
    /// cursor where the variant says.
    Rebuild(After),
}

/// Where the cursor lands after the rows are rebuilt.
#[derive(Debug, PartialEq, Eq)]
enum After {
    /// Where it was, if that is still an issue row.
    Kept,
    First,
    On(String),
}

fn screen(terminal: &mut DefaultTerminal, store: &Store, actor: &str) -> Result<()> {
    let mut st = State::new();
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let mut screen = build(store, &snap.db, st.view, &st.filter, actor, &st.folded)?;
        keep(&mut st.list, &screen.rows);
        // a page whose issues a reload took away falls back
        match &mut st.mode {
            Mode::Detail(page) => {
                page.prune(&snap.db);
                if page.ids.is_empty() {
                    st.mode = Mode::Rows;
                }
            }
            Mode::Recall(r) if !snap.db.memories.contains_key(&r.slug) => st.mode = Mode::Rows,
            _ => {}
        }
        loop {
            let mut limits = Limits::default();
            match &mut st.mode {
                Mode::Rows => {
                    let right = if st.typing {
                        format!("/{}▏", st.filter.text)
                    } else {
                        st.filter.describe(st.view)
                    };
                    let (list, preview) = (&mut st.list, st.preview);
                    terminal
                        .draw(|frame| draw_rows(frame, &screen, list, &snap.db, preview, &right))?;
                }
                Mode::Detail(page) => {
                    let issue = &snap.db.issues[page.id()];
                    terminal.draw(|frame| {
                        limits = draw_detail(frame, &screen.header, st.view, &snap.db, issue, page);
                    })?;
                }
                Mode::Recall(recall) => {
                    let memory = &snap.db.memories[&recall.slug];
                    let row = screen.rows.iter().find_map(|r| match r {
                        Row::Memory {
                            memory, distance, ..
                        } if memory.slug == recall.slug => Some(*distance),
                        _ => None,
                    });
                    // the rows may be another view's
                    let distance = row.unwrap_or_else(|| store.git.distance(&memory.stamp.commit));
                    let marked =
                        !matches!(distance, Distance::Behind(n) if n < snap.db.meta.stale_after);
                    terminal.draw(|frame| {
                        limits = draw_recall(
                            frame,
                            &screen.header,
                            st.view,
                            memory,
                            distance,
                            marked,
                            recall,
                        );
                    })?;
                }
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match handle(&mut st, key, &screen.rows, &snap.db, &limits) {
                Action::Quit => return Ok(()),
                Action::Reload => break,
                Action::Draw => {}
                Action::Rebuild(after) => {
                    screen = build(store, &snap.db, st.view, &st.filter, actor, &st.folded)?;
                    match after {
                        After::Kept => keep(&mut st.list, &screen.rows),
                        After::First => st.list.select(step(&screen.rows, None, 1)),
                        After::On(id) => {
                            st.list.select(screen.rows.iter().position(
                                |r| matches!(r, Row::Issue { issue, .. } if issue.id == id),
                            ))
                        }
                    }
                }
            }
        }
    }
}

/// Apply one key to the state and say what the loop does next.
fn handle(st: &mut State, key: KeyEvent, rows: &[Row], db: &Db, limits: &Limits) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if st.typing {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => st.typing = false,
            KeyCode::Backspace => {
                st.filter.text.pop();
            }
            KeyCode::Char(c) if !ctrl => st.filter.text.push(c),
            KeyCode::Char('c') => return Action::Quit,
            _ => {}
        }
        return Action::Rebuild(After::Kept);
    }
    match key.code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('c') if ctrl => return Action::Quit,
        KeyCode::Char('r') => return Action::Reload,
        _ => {}
    }
    match &mut st.mode {
        Mode::Rows => handle_rows(st, key.code, rows, db),
        Mode::Recall(recall) => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                    st.mode = Mode::Rows;
                }
                KeyCode::Char('d') if ctrl => {
                    recall.scroll = (recall.scroll + limits.rows / 2).min(limits.scroll);
                }
                KeyCode::Char('u') if ctrl => {
                    recall.scroll = recall.scroll.saturating_sub(limits.rows / 2);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    recall.scroll = (recall.scroll + 1).min(limits.scroll);
                }
                KeyCode::Char('k') | KeyCode::Up => recall.scroll = recall.scroll.saturating_sub(1),
                KeyCode::Char('g') | KeyCode::Home => recall.scroll = 0,
                KeyCode::Char('G') | KeyCode::End => recall.scroll = limits.scroll,
                _ => {}
            }
            Action::Draw
        }
        Mode::Detail(page) => {
            let last_tie = limits.ties.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                    if !page.back() {
                        st.mode = Mode::Rows;
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
                KeyCode::Char('d') if ctrl => {
                    page.scroll = (page.scroll + limits.rows / 2).min(limits.scroll);
                }
                KeyCode::Char('u') if ctrl => {
                    page.scroll = page.scroll.saturating_sub(limits.rows / 2);
                }
                KeyCode::Char('j') | KeyCode::Down => match page.tie {
                    Some(k) => page.tie = Some((k + 1).min(last_tie)),
                    None => page.step_box(&limits.boxes, 1),
                },
                KeyCode::Char('k') | KeyCode::Up => match page.tie {
                    Some(k) => page.tie = Some(k.saturating_sub(1)),
                    None => page.step_box(&limits.boxes, -1),
                },
                KeyCode::Char('g') | KeyCode::Home => match page.tie {
                    Some(_) => page.tie = Some(0),
                    None => page.go(0),
                },
                KeyCode::Char('G') | KeyCode::End => match page.tie {
                    Some(_) => page.tie = Some(last_tie),
                    None => page.go(limits.boxes.len().saturating_sub(1)),
                },
                KeyCode::Char(' ') if page.tie.is_none() => page.fold(&limits.boxes),
                KeyCode::Char('u') if page.tie.is_none() => page.up(&limits.boxes),
                KeyCode::Char('[') if page.tie.is_none() => page.sibling(&limits.boxes, -1),
                KeyCode::Char(']') if page.tie.is_none() => page.sibling(&limits.boxes, 1),
                _ => {}
            }
            Action::Draw
        }
    }
}

/// A key on the rows.
fn handle_rows(st: &mut State, code: KeyCode, rows: &[Row], db: &Db) -> Action {
    let row = st.list.selected().and_then(|i| rows.get(i));
    let chosen = match row {
        Some(Row::Issue { issue, .. }) => Some(*issue),
        _ => None,
    };
    let memories = st.view == View::Memories;
    match code {
        KeyCode::Esc if st.filter.is_empty() => Action::Quit,
        KeyCode::Esc => {
            st.filter = Filter::default();
            Action::Rebuild(After::Kept)
        }
        KeyCode::Char('p') => {
            st.preview = !st.preview;
            Action::Draw
        }
        KeyCode::Char(' ') => match chosen {
            Some(issue) if !graph::children(db, &issue.id).is_empty() => {
                if !st.folded.remove(&issue.id) {
                    st.folded.insert(issue.id.clone());
                }
                Action::Rebuild(After::On(issue.id.clone()))
            }
            _ => Action::Draw,
        },
        KeyCode::Char('s' | 't' | 'a' | 'c') if memories => Action::Draw,
        KeyCode::Char('s') => {
            st.filter.status = cycle(st.filter.status);
            Action::Rebuild(After::Kept)
        }
        KeyCode::Char('t') => {
            st.filter.kind = cycle(st.filter.kind);
            Action::Rebuild(After::Kept)
        }
        KeyCode::Char('a') => {
            st.filter.mine = !st.filter.mine;
            Action::Rebuild(After::Kept)
        }
        KeyCode::Char('c') => {
            st.filter.closed = !st.filter.closed;
            Action::Rebuild(After::Kept)
        }
        KeyCode::Char('/') => {
            st.typing = true;
            Action::Rebuild(After::Kept)
        }
        KeyCode::Tab => {
            st.view = st.view.next();
            Action::Rebuild(After::First)
        }
        KeyCode::BackTab => {
            st.view = st.view.previous();
            Action::Rebuild(After::First)
        }
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
            match row {
                Some(Row::Issue { issue, .. }) => {
                    st.mode = Mode::Detail(Page::open(issue.id.clone()));
                }
                Some(Row::Memory { memory, .. }) => {
                    st.mode = Mode::Recall(Recall {
                        slug: memory.slug.clone(),
                        scroll: 0,
                    });
                }
                _ => {}
            }
            Action::Draw
        }
        KeyCode::Char('j') | KeyCode::Down => {
            st.list.select(step(rows, st.list.selected(), 1));
            Action::Draw
        }
        KeyCode::Char('k') | KeyCode::Up => {
            st.list.select(step(rows, st.list.selected(), -1));
            Action::Draw
        }
        KeyCode::Char('g') | KeyCode::Home => {
            st.list.select(step(rows, None, 1));
            Action::Draw
        }
        KeyCode::Char('G') | KeyCode::End => {
            st.list.select(step(rows, None, -1));
            Action::Draw
        }
        _ => Action::Draw,
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
    let header = header(db, &stamp);
    let mut rows = match view {
        View::List => list_rows(db, filter, actor, folded),
        View::Board => board_rows(db, now),
        View::Memories => memory_rows(db, filter, |commit| store.git.distance(commit)),
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
        header,
        rows,
        widths,
    })
}

fn widths(rows: &[Row]) -> Widths {
    let mut widths = Widths::default();
    for row in rows {
        match row {
            Row::Issue {
                lead,
                issue,
                rollup,
                ..
            } => {
                widths.id = widths
                    .id
                    .max(lead.chars().count() + issue.id.chars().count());
                widths.status = widths.status.max(issue.status.to_string().len());
                widths.kind = widths.kind.max(issue.kind.to_string().len());
                widths.rollup = widths.rollup.max(rollup_text(*rollup).len());
            }
            Row::Memory { memory, .. } => {
                widths.slug = widths.slug.max(memory.slug.chars().count());
            }
            _ => {}
        }
    }
    widths
}

/// What `foam memories` prints: every memory the filter's text
/// matches, by slug, with how far behind HEAD each was remembered
/// and whether that marks it, by the rule prime uses.
fn memory_rows<'a>(
    db: &'a Db,
    filter: &Filter,
    distance: impl Fn(&str) -> Distance,
) -> Vec<Row<'a>> {
    let rows: Vec<Row> = db
        .memories
        .values()
        .filter(|m| filter.keeps_memory(m))
        .map(|m| {
            let d = distance(&m.stamp.commit);
            Row::Memory {
                memory: m,
                distance: d,
                marked: !matches!(d, Distance::Behind(n) if n < db.meta.stale_after),
            }
        })
        .collect();
    if rows.is_empty() {
        return vec![Row::Note(if filter.text.is_empty() {
            "nothing remembered yet"
        } else {
            "no memory matches"
        })];
    }
    rows
}

/// A memory's distance from HEAD in a few words.
fn short_age(stamp: &Stamp, distance: Distance) -> String {
    match distance {
        Distance::Behind(0) => "this commit".into(),
        Distance::Behind(1) => "1 commit ago".into(),
        Distance::Behind(n) => format!("{n} commits ago"),
        Distance::Elsewhere => format!("on {}", stamp.branch),
        Distance::Unknown => "a commit this clone lacks".into(),
    }
}

fn header(db: &Db, stamp: &Stamp) -> Header {
    let count = |s: Status| db.issues.values().filter(|i| i.status == s).count();
    let mut counts = Vec::new();
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
            counts.push(Span::raw(" "));
        }
        counts.push(badge(&format!("{} {}", count(s), s), status_colour(s)));
    }
    Header {
        prefix: db.meta.prefix.clone(),
        stamp: format!("{} @ {}", stamp.branch, stamp.commit),
        counts,
    }
}

/// The header's rows: the prefix and the views, with the one in
/// use bright and the trail of pages opened from it after it, and
/// HEAD at the right edge; the counts, with `right` at the edge;
/// a rule.
fn header_lines(
    h: &Header,
    view: View,
    trail: &[&str],
    right: &str,
    width: usize,
) -> [Line<'static>; 3] {
    let mut top = vec![Span::raw(h.prefix.clone()).bold().cyan()];
    // on a page only the view it was opened from shows, with the
    // trail hanging off it
    let views: Vec<View> = if trail.is_empty() {
        vec![View::List, View::Board, View::Memories]
    } else {
        vec![view]
    };
    for v in views {
        top.push(Span::raw("  "));
        top.push(if v == view {
            Span::raw(v.name()).bold().cyan()
        } else {
            Span::raw(v.name()).dim()
        });
    }
    for (n, id) in trail.iter().enumerate() {
        top.push(Span::raw(" › ").dim());
        top.push(if n + 1 == trail.len() {
            Span::raw(id.to_string()).bold().cyan()
        } else {
            Span::raw(id.to_string()).cyan()
        });
    }
    let against = |mut spans: Vec<Span<'static>>, right: &str| {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let gap = width.saturating_sub(used + right.chars().count());
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::raw(right.to_string()).dim());
        Line::from(spans)
    };
    [
        against(top, &h.stamp),
        against(h.counts.clone(), right),
        Line::from(Span::raw("─".repeat(width)).dim()),
    ]
}

fn draw_header(frame: &mut Frame, area: Rect, h: &Header, view: View, trail: &[&str], right: &str) {
    let lines = header_lines(h, view, trail, right, usize::from(area.width));
    frame.render_widget(Paragraph::new(lines.to_vec()), area);
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
        Constraint::Length(3),
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
    draw_header(frame, top, &screen.header, screen.view, &[], right);
    let chosen = match state.selected().map(|i| &screen.rows[i]) {
        Some(row @ (Row::Issue { .. } | Row::Memory { .. })) if preview => Some(row),
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
    match (chosen, preview_area) {
        (Some(Row::Issue { issue, .. }), Some(area)) => {
            let lines = preview_lines(db, issue, inner(area.width));
            frame.render_widget(
                Paragraph::new(lines)
                    .block(pane(Span::raw(format!(" {} ", issue.id)).cyan(), false)),
                area,
            );
        }
        (
            Some(Row::Memory {
                memory,
                distance,
                marked,
            }),
            Some(area),
        ) => {
            let lines = memory_lines(memory, *distance, *marked, inner(area.width));
            frame.render_widget(
                Paragraph::new(lines)
                    .block(pane(Span::raw(format!(" {} ", memory.slug)).cyan(), false)),
                area,
            );
        }
        _ => {}
    }
    let keys = if screen.view == View::Memories {
        format!(
            "{}   p {}   / filter",
            screen.view.keys(),
            if preview { "no preview" } else { "preview" }
        )
    } else {
        format!(
            "{}   Space fold   p {}   s/t/a/c / filter",
            screen.view.keys(),
            if preview { "no preview" } else { "preview" }
        )
    };
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
        lines.extend(md::render(&issue.body, width.max(10)));
    }
    lines
}

/// What the about pane says of a memory: when it was remembered
/// and changed, where, and whether that still holds.
fn memory_about(m: &Memory, distance: Distance, marked: bool) -> Vec<Line<'static>> {
    let style = human();
    let label = |l: &str| Span::raw(pad(l, 12)).dim();
    let mut lines = vec![Line::from(vec![
        label("remembered"),
        Span::raw(style.when(m.created_at)),
    ])];
    if m.updated_at != m.created_at {
        lines.push(Line::from(vec![
            label("changed"),
            Span::raw(style.when(m.updated_at)),
        ]));
    }
    lines.push(Line::from(vec![
        label("on"),
        Span::raw(format!("{} @ {}", m.stamp.branch, m.stamp.commit)).cyan(),
    ]));
    lines.push(Line::default());
    let age = short_age(&m.stamp, distance);
    lines.push(Line::from(if marked {
        Span::raw(format!("{age}: may no longer hold")).yellow()
    } else {
        Span::raw(age).dim()
    }));
    lines
}

/// What the preview shows of a memory: the slug, the about block,
/// and the text.
fn memory_lines(m: &Memory, distance: Distance, marked: bool, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::raw(m.slug.clone()).bold())];
    lines.push(Line::default());
    lines.extend(memory_about(m, distance, marked));
    lines.push(Line::default());
    lines.extend(md::render(&m.text, width.max(10)));
    lines
}

/// Draw a memory's page as an issue's: the card, the about pane
/// and the text in a box.
fn draw_recall(
    frame: &mut Frame,
    header: &Header,
    view: View,
    memory: &Memory,
    distance: Distance,
    marked: bool,
    recall: &mut Recall,
) -> Limits {
    let [top, body, foot] = frame_areas(frame);
    draw_header(frame, top, header, view, &[&memory.slug], "memory");
    let wide = body.width >= WIDE;
    let about = memory_about(memory, distance, marked);
    let heading = Line::from(Span::raw(memory.slug.clone()).bold());
    let (card, about_area, text_area) = if wide {
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(ABOUT_WIDTH)]).areas(body);
        let [card, text] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(left);
        (card, right, text)
    } else {
        let about_h = u16::try_from(about.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(body.height / 2);
        let [card, about_area, text] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(about_h),
            Constraint::Min(0),
        ])
        .areas(body);
        (card, about_area, text)
    };
    frame.render_widget(
        Paragraph::new(heading).block(pane(Span::raw(" memory ").cyan(), false)),
        card,
    );
    frame.render_widget(
        Paragraph::new(about).block(pane(Span::raw(" about "), false)),
        about_area,
    );
    let lines = post_box(
        &Post {
            id: None,
            author: None,
            edited: (memory.updated_at != memory.created_at).then_some(memory.updated_at),
            at: memory.created_at,
            text: &memory.text,
            branch: &memory.stamp.branch,
            commit: &memory.stamp.commit,
            hidden: 0,
        },
        &human(),
        inner(text_area.width).max(20),
        true,
    );
    let rows = rows_in(text_area.height);
    let max = lines.len().saturating_sub(rows);
    recall.scroll = recall.scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(Span::raw(" text "), true))
            .scroll((u16::try_from(recall.scroll).unwrap_or(u16::MAX), 0)),
        text_area,
    );
    let keys = "j/k scroll   g/G ends   ^d/^u page   Esc back   r reload   q quit";
    frame.render_widget(Paragraph::new(keys).dim(), foot);
    Limits {
        scroll: max,
        rows,
        ..Limits::default()
    }
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
fn draw_detail(
    frame: &mut Frame,
    header: &Header,
    view: View,
    db: &Db,
    issue: &Issue,
    page: &mut Page,
) -> Limits {
    let [top, body, foot] = frame_areas(frame);
    let trail: Vec<&str> = page.ids.iter().map(String::as_str).collect();
    draw_header(frame, top, header, view, &trail, "issue");
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

    let (lines, boxes) = posts(issue, inner(posts_area.width), page, page.tie.is_none());
    let rows = rows_in(posts_area.height);
    let max = lines.len().saturating_sub(rows);
    page.cursor = page.cursor.min(boxes.len().saturating_sub(1));
    if page.reveal {
        if let Some(b) = boxes.get(page.cursor) {
            if b.lines.start < page.scroll {
                page.scroll = b.lines.start;
            } else if b.lines.end > page.scroll + rows {
                // a box taller than the pane shows its top
                page.scroll = b.lines.end.saturating_sub(rows).min(b.lines.start);
            }
        }
        page.reveal = false;
    }
    page.scroll = page.scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(Span::raw(" notes "), page.tie.is_none()))
            .scroll((u16::try_from(page.scroll).unwrap_or(u16::MAX), 0)),
        posts_area,
    );

    let keys = if page.tie.is_some() {
        "j/k pick   Enter follow   Tab notes   Esc back   q quit"
    } else if about.ties.is_empty() {
        "j/k notes   Space fold   u/[/] thread   ^d/^u scroll   Esc back   q quit"
    } else {
        "j/k notes   Space fold   u/[/] thread   ^d/^u scroll   Tab ties   Esc back   q quit"
    };
    frame.render_widget(Paragraph::new(keys).dim(), foot);
    Limits {
        scroll: max,
        rows,
        boxes,
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

/// The body as the issue's first post, then the notes that are
/// not folded away, each a box `width` wide, with the box under
/// the page's cursor drawn plain when `focused`.
fn posts(
    i: &Issue,
    width: usize,
    page: &Page,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<NoteBox>) {
    let style = human();
    let wide = width.max(20);
    let threaded = crate::model::threaded(&i.notes);
    // each shown note with its depth and how many replies it has,
    // the ones under a folded note skipped
    let mut shown = Vec::new();
    let mut k = 0;
    while let Some(&(depth, n)) = threaded.get(k) {
        let replies = threaded[k + 1..]
            .iter()
            .take_while(|(d, _)| *d > depth)
            .count();
        shown.push((depth, n, replies));
        k += 1 + if page.folded.contains(&n.id) {
            replies
        } else {
            0
        };
    }
    let count = shown.len() + usize::from(!i.body.is_empty());
    let cursor = page.cursor.min(count.saturating_sub(1));
    let mut out = Vec::new();
    let mut boxes = Vec::new();
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
                hidden: 0,
            },
            &style,
            wide,
            focused && cursor == 0,
        ));
        boxes.push(NoteBox {
            id: None,
            reply_to: None,
            replies: 0,
            lines: 0..out.len(),
        });
    }
    for (depth, n, replies) in shown {
        if !out.is_empty() {
            out.push(Line::default());
        }
        let start = out.len();
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
                hidden: if page.folded.contains(&n.id) {
                    replies
                } else {
                    0
                },
            },
            &style,
            wide - indent,
            focused && cursor == boxes.len(),
        );
        for line in &mut lines {
            line.spans.insert(0, Span::raw(" ".repeat(indent)));
        }
        out.extend(lines);
        boxes.push(NoteBox {
            id: Some(n.id.clone()),
            reply_to: n.reply_to.clone(),
            replies,
            lines: start..out.len(),
        });
    }
    (out, boxes)
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
    /// How many replies are folded away under it.
    hidden: usize,
}

/// A post as a rounded box `width` columns wide: who and when in
/// the top border, the text inside, where it was written in the
/// bottom border. The box under the cursor is `chosen`, in the
/// accent colour; the rest are dim.
fn post_box(p: &Post, style: &render::Style, width: usize, chosen: bool) -> Vec<Line<'static>> {
    let border = if chosen {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };
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
    for l in md::render(p.text, inner) {
        let mut spans = vec![edge("│ ".into())];
        spans.extend(l.spans);
        spans.push(edge(" │".into()));
        out.push(Line::from(spans));
    }
    let mut from = String::new();
    if p.hidden > 0 {
        let s = if p.hidden == 1 { "reply" } else { "replies" };
        from.push_str(&format!("▸ {} {s} · ", p.hidden));
    }
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
        Row::Memory {
            memory,
            distance,
            marked,
        } => {
            let mut spans = vec![
                Span::raw(pad(&memory.slug, widths.slug)).bold().cyan(),
                Span::raw("  "),
            ];
            let used = widths.slug + 2;
            let age = short_age(&memory.stamp, *distance);
            let tail = vec![
                Span::raw("  "),
                if *marked {
                    Span::raw(age).yellow()
                } else {
                    Span::raw(age).dim()
                },
            ];
            let first = memory.text.lines().next().unwrap_or("");
            let room = width.saturating_sub(used);
            let tail_len: usize = tail.iter().map(|s| s.content.chars().count()).sum();
            let (text_room, tail_room) = split(first.chars().count(), tail_len, room);
            spans.push(Span::raw(cut(first, text_room)));
            spans.extend(cut_spans(tail, tail_room));
            inside(spans)
        }
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
            slug: 0,
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

    fn test_header() -> Header {
        Header {
            prefix: "t".into(),
            stamp: "dev @ abc1234".into(),
            counts: vec![badge("1 open", Color::Green)],
        }
    }

    #[test]
    fn the_preview_stands_beside_the_rows_or_under_them() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (db, _) = fixture();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let screen = Screen {
            view: View::List,
            header: test_header(),
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
        let col = wide[3]
            .find("╭ t-000002 ")
            .map(|b| wide[3][..b].chars().count());
        assert_eq!(col, Some(76), "{}", wide[3]);
        assert!(
            at(&wide, "│ a title that is long enough").is_some(),
            "{wide:#?}"
        );
        assert!(at(&wide, "│  P2   task   open ").is_some(), "{wide:#?}");
        assert!(at(&wide, "│ one paragraph").is_some(), "{wide:#?}");
        // the rows keep their columns at the narrower width
        assert!(wide[3].starts_with("  t-000001"), "{}", wide[3]);
        // under: the pane takes the lower half
        let narrow = render(70, 30, true);
        assert!(narrow[16].starts_with("╭ t-000002 "), "{}", narrow[16]);
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
        assert!(f.is_empty() && f.describe(View::List).is_empty());
        assert_eq!(ids(&f), ["t-000001", "t-000002", "t-000004"]);
        // closed joins the default set, and a status replaces it
        f.closed = true;
        assert_eq!(ids(&f).len(), 4);
        f.status = Some(Status::Closed);
        assert_eq!(ids(&f), ["t-000003"]);
        assert_eq!(f.describe(View::List), "closed · closed");
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
        assert_eq!(f.describe(View::List), "mine");
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
        let h = super::header(&db, &stamp);
        let lines = header_lines(&h, View::Board, &[], "mine", 60);
        let text = strings(&lines);
        assert_eq!(
            text[0],
            "t  list  board  memories                       dev @ abc1234"
        );
        assert_eq!(
            text[1],
            " 1 open   1 in_progress   0 deferred   0 closed         mine"
        );
        assert_eq!(text[2], "─".repeat(60));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Cyan));
        let board = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "board")
            .unwrap();
        assert!(board.style.add_modifier.contains(Modifier::BOLD));
        let list = lines[0].spans.iter().find(|s| s.content == "list").unwrap();
        assert!(list.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(lines[1].spans[0].style.bg, Some(Color::Green));
        assert_eq!(lines[1].spans[2].style.bg, Some(Color::Yellow));
        // on a page only the view it came from shows, then the trail
        let lines = header_lines(&h, View::List, &["t-000001", "t-000002"], "issue", 60);
        let text = strings(&lines);
        assert_eq!(
            text[0],
            "t  list › t-000001 › t-000002                  dev @ abc1234"
        );
        assert!(text[1].ends_with("issue"));
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
        let lines = strings(&posts(&i, 40, &Page::open(i.id.clone()), true).0);
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
        let title = test_header();
        let render = |width: u16, height: u16, tie: Option<usize>| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut page = Page::open(i.id.clone());
            page.tie = tie;
            terminal
                .draw(|f| {
                    draw_detail(f, &title, View::List, &db, &i, &mut page);
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
        assert!(!dim(&buffer[(0, 6)]) && dim(&buffer[(70, 3)]));
        assert_eq!(buffer[(74, 9)].fg, Color::Cyan);
        let buffer = render(100, 30, Some(0));
        assert!(dim(&buffer[(0, 6)]) && !dim(&buffer[(70, 3)]));
        let col_of = |row: &str, needle: &str| row.find(needle).map(|b| row[..b].chars().count());

        // wide: the card and the posts on the left, the about pane
        // at the right edge, all starting under the header
        let wide = rows(100, 30);
        assert!(wide[0].starts_with("t  list › t-000002"), "{}", wide[0]);
        assert!(wide[0].ends_with("dev @ abc1234"), "{}", wide[0]);
        assert!(wide[1].ends_with("issue"), "{}", wide[1]);
        assert!(wide[2].starts_with("────"), "{}", wide[2]);
        assert!(wide[3].starts_with("╭ t-000002 "), "{}", wide[3]);
        assert_eq!(col_of(&wide[3], "╭ about "), Some(70), "{}", wide[3]);
        assert!(wide[4].contains("││  P2   task   open "), "{}", wide[4]);
        let posts = wide.iter().position(|r| r.starts_with("╭ notes ")).unwrap();
        let body = &wide[posts + 1];
        assert!(body.starts_with("│ ╭─ just now "), "{body}");
        assert!(
            body.ends_with("╮ ││                            │"),
            "{body}"
        );

        // narrow: card, about and posts stacked, each full width
        let narrow = rows(60, 30);
        let at = |needle: &str| narrow.iter().position(|r| r.starts_with(needle)).unwrap();
        let (card, about, posts) = (at("╭ t-000002 "), at("╭ about "), at("╭ notes "));
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

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// The fixture's rows, a state with the cursor on the first
    /// one, and the fixture's db.
    fn on_rows() -> (Db, State) {
        let (db, _) = fixture();
        let mut st = State::new();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        st.list.select(step(&rows, None, 1));
        (db, st)
    }

    fn chosen<'a>(st: &State, rows: &[Row<'a>]) -> &'a str {
        match &rows[st.list.selected().unwrap()] {
            Row::Issue { issue, .. } => issue.id.as_str(),
            _ => panic!("an issue row"),
        }
    }

    #[test]
    fn keys_on_the_rows_move_open_and_come_back() {
        let (db, mut st) = on_rows();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let limits = Limits::default();
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        assert_eq!(chosen(&st, &rows), "t-000001");
        assert_eq!(key(&mut st, KeyCode::Char('j')), Action::Draw);
        assert_eq!(chosen(&st, &rows), "t-000002");
        assert_eq!(key(&mut st, KeyCode::Char('G')), Action::Draw);
        assert_eq!(chosen(&st, &rows), "t-000004");
        assert_eq!(key(&mut st, KeyCode::Char('k')), Action::Draw);
        assert_eq!(key(&mut st, KeyCode::Enter), Action::Draw);
        assert!(matches!(&st.mode, Mode::Detail(p) if p.id() == "t-000002"));
        assert_eq!(key(&mut st, KeyCode::Esc), Action::Draw);
        assert!(matches!(st.mode, Mode::Rows));
        assert_eq!(key(&mut st, KeyCode::Esc), Action::Quit);
        assert_eq!(key(&mut st, KeyCode::Char('q')), Action::Quit);
        assert_eq!(key(&mut st, KeyCode::Char('r')), Action::Reload);
        assert_eq!(
            handle(&mut st, ctrl('c'), &rows, &db, &limits),
            Action::Quit
        );
    }

    #[test]
    fn the_filter_takes_typed_text_and_esc_clears_it() {
        let (db, mut st) = on_rows();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let limits = Limits::default();
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        assert_eq!(
            key(&mut st, KeyCode::Char('/')),
            Action::Rebuild(After::Kept)
        );
        assert!(st.typing);
        // q and j are text while typing, not keys
        assert_eq!(
            key(&mut st, KeyCode::Char('q')),
            Action::Rebuild(After::Kept)
        );
        assert_eq!(
            key(&mut st, KeyCode::Char('j')),
            Action::Rebuild(After::Kept)
        );
        assert_eq!(
            key(&mut st, KeyCode::Backspace),
            Action::Rebuild(After::Kept)
        );
        assert_eq!(st.filter.text, "q");
        assert_eq!(key(&mut st, KeyCode::Enter), Action::Rebuild(After::Kept));
        assert!(!st.typing);
        assert_eq!(
            key(&mut st, KeyCode::Char('c')),
            Action::Rebuild(After::Kept)
        );
        assert!(st.filter.closed);
        assert_eq!(key(&mut st, KeyCode::Esc), Action::Rebuild(After::Kept));
        assert!(st.filter.is_empty());
        st.typing = true;
        assert_eq!(
            handle(&mut st, ctrl('c'), &rows, &db, &limits),
            Action::Quit
        );
    }

    #[test]
    fn space_folds_an_issue_with_children_and_tab_swaps_the_view() {
        let (db, mut st) = on_rows();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let limits = Limits::default();
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        assert_eq!(
            key(&mut st, KeyCode::Char(' ')),
            Action::Rebuild(After::On("t-000001".into()))
        );
        assert!(st.folded.contains("t-000001"));
        assert_eq!(
            key(&mut st, KeyCode::Char(' ')),
            Action::Rebuild(After::On("t-000001".into()))
        );
        assert!(st.folded.is_empty());
        key(&mut st, KeyCode::Char('G'));
        assert_eq!(key(&mut st, KeyCode::Char(' ')), Action::Draw);
        assert_eq!(key(&mut st, KeyCode::Tab), Action::Rebuild(After::First));
        assert_eq!(st.view, View::Board);
        assert_eq!(key(&mut st, KeyCode::Char('p')), Action::Draw);
        assert!(!st.preview);
    }

    #[test]
    fn keys_on_a_page_scroll_the_notes_or_walk_the_ties() {
        let (db, mut st) = on_rows();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        st.mode = Mode::Detail(Page::open("t-000002".into()));
        let note_box = |id: &str| NoteBox {
            id: Some(id.into()),
            reply_to: None,
            replies: 0,
            lines: 0..1,
        };
        let limits = Limits {
            scroll: 2,
            rows: 10,
            boxes: vec![note_box("a"), note_box("b"), note_box("c")],
            ties: vec!["t-000001".into(), "t-000004".into()],
        };
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        let page = |st: &State| match &st.mode {
            Mode::Detail(p) => (p.id().to_string(), p.cursor, p.scroll, p.tie),
            Mode::Rows | Mode::Recall(_) => panic!("a page"),
        };
        for _ in 0..4 {
            assert_eq!(key(&mut st, KeyCode::Char('j')), Action::Draw);
        }
        assert_eq!(page(&st), ("t-000002".into(), 2, 0, None));
        key(&mut st, KeyCode::Char('g'));
        assert_eq!(page(&st).1, 0);
        key(&mut st, KeyCode::Char('G'));
        assert_eq!(page(&st).1, 2);
        assert_eq!(
            handle(&mut st, ctrl('d'), &rows, &db, &limits),
            Action::Draw
        );
        assert_eq!(page(&st).2, 2);
        handle(&mut st, ctrl('u'), &rows, &db, &limits);
        assert_eq!(page(&st).2, 0);
        key(&mut st, KeyCode::Char(']'));
        assert_eq!(page(&st).1, 2);
        key(&mut st, KeyCode::Char('['));
        key(&mut st, KeyCode::Char('u'));
        assert_eq!(page(&st).1, 0);
        key(&mut st, KeyCode::Tab);
        key(&mut st, KeyCode::Char('j'));
        key(&mut st, KeyCode::Char('j'));
        assert_eq!(page(&st).3, Some(1));
        key(&mut st, KeyCode::Enter);
        assert_eq!(page(&st), ("t-000004".into(), 0, 0, None));
        key(&mut st, KeyCode::Char('h'));
        assert_eq!(page(&st).0, "t-000002");
        key(&mut st, KeyCode::Backspace);
        assert!(matches!(st.mode, Mode::Rows));
    }

    /// An issue with a body and four notes: `a` answered by `b`,
    /// `b` answered by `c`, and `d` on its own.
    fn thread() -> Issue {
        let mut i = test_issue("t-000009");
        i.body = "the body".into();
        let note = |id: &str, reply_to: Option<&str>| Note {
            id: id.into(),
            reply_to: reply_to.map(String::from),
            edited_at: None,
            deleted_at: None,
            at: Timestamp::now(),
            author: "ann".into(),
            text: format!("note {id}"),
            commit: String::new(),
            branch: String::new(),
        };
        i.notes = vec![
            note("aaaa0000", None),
            note("bbbb0000", Some("aaaa0000")),
            note("cccc0000", Some("bbbb0000")),
            note("dddd0000", None),
        ];
        i
    }

    fn box_ids(boxes: &[NoteBox]) -> Vec<Option<&str>> {
        boxes.iter().map(|b| b.id.as_deref()).collect()
    }

    #[test]
    fn folding_a_note_hides_the_replies_under_it() {
        let i = thread();
        let mut page = Page::open(i.id.clone());
        let (lines, boxes) = posts(&i, 40, &page, true);
        assert_eq!(
            box_ids(&boxes),
            [
                None,
                Some("aaaa0000"),
                Some("bbbb0000"),
                Some("cccc0000"),
                Some("dddd0000")
            ]
        );
        assert_eq!(
            boxes.iter().map(|b| b.replies).collect::<Vec<_>>(),
            [0, 2, 1, 0, 0]
        );
        // each box's lines are its own, in order, with a blank
        // line between
        for pair in boxes.windows(2) {
            assert_eq!(pair[0].lines.end + 1, pair[1].lines.start);
        }
        assert_eq!(boxes[4].lines.end, lines.len());

        page.cursor = 1;
        page.fold(&boxes);
        assert!(page.folded.contains("aaaa0000"));
        let (lines, boxes) = posts(&i, 40, &page, true);
        assert_eq!(box_ids(&boxes), [None, Some("aaaa0000"), Some("dddd0000")]);
        let bottom = lines[boxes[1].lines.end - 1].to_string();
        assert!(bottom.contains("▸ 2 replies · aaaa0000"), "{bottom}");

        // the body and a note without replies do not fold
        page.cursor = 0;
        page.fold(&boxes);
        page.cursor = 2;
        page.fold(&boxes);
        assert_eq!(page.folded.len(), 1);
        page.cursor = 1;
        page.fold(&boxes);
        assert!(page.folded.is_empty());
    }

    #[test]
    fn the_cursor_walks_boxes_threads_and_siblings() {
        let i = thread();
        let mut page = Page::open(i.id.clone());
        let (_, boxes) = posts(&i, 40, &page, true);
        page.step_box(&boxes, -1);
        assert_eq!(page.cursor, 0);
        page.step_box(&boxes, 1);
        page.step_box(&boxes, 1);
        page.step_box(&boxes, 1);
        assert_eq!(page.cursor, 3);
        // c answers b answers a answers the issue
        page.up(&boxes);
        assert_eq!(page.cursor, 2);
        page.up(&boxes);
        assert_eq!(page.cursor, 1);
        page.up(&boxes);
        assert_eq!(page.cursor, 0);
        // the body's siblings are the top-level notes
        page.sibling(&boxes, 1);
        assert_eq!(page.cursor, 1);
        page.sibling(&boxes, 1);
        assert_eq!(page.cursor, 4);
        page.sibling(&boxes, 1);
        assert_eq!(page.cursor, 4);
        page.sibling(&boxes, -1);
        assert_eq!(page.cursor, 1);
        // b has no sibling
        page.go(2);
        page.sibling(&boxes, 1);
        page.sibling(&boxes, -1);
        assert_eq!(page.cursor, 2);
        page.reveal = false;
        page.step_box(&boxes, 1);
        assert!(page.reveal);
    }

    #[test]
    fn the_chosen_box_is_drawn_plain_and_scrolled_into_view() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let i = thread();
        let mut page = Page::open(i.id.clone());
        let (lines, boxes) = posts(&i, 40, &page, true);
        let plain = |l: &Line| {
            let corner = l.spans.iter().find(|s| s.content.contains('╭')).unwrap();
            corner.style.fg == Some(Color::Cyan)
        };
        assert!(plain(&lines[boxes[0].lines.start]));
        assert!(!plain(&lines[boxes[1].lines.start]));
        page.go(1);
        let (lines, _) = posts(&i, 40, &page, true);
        assert!(!plain(&lines[boxes[0].lines.start]));
        assert!(plain(&lines[boxes[1].lines.start]));
        // on the ties, no box is chosen
        let (lines, _) = posts(&i, 40, &page, false);
        assert!(!plain(&lines[boxes[1].lines.start]));

        let (db, _) = fixture();
        let title = test_header();
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        let mut draw = |page: &mut Page| {
            let mut limits = Limits::default();
            terminal
                .draw(|f| limits = draw_detail(f, &title, View::List, &db, &i, page))
                .unwrap();
            limits
        };
        let limits = draw(&mut page);
        assert!(limits.scroll > 0, "the notes do not all fit: {limits:?}");
        assert_eq!(page.scroll, 0);
        page.go(4);
        let limits = draw(&mut page);
        assert_eq!(page.scroll, limits.scroll);
        assert!(!page.reveal);
        page.go(0);
        draw(&mut page);
        assert_eq!(page.scroll, 0);
    }

    fn memory(slug: &str, text: &str, commit: &str) -> Memory {
        let now = Timestamp::now();
        Memory {
            slug: slug.into(),
            text: text.into(),
            created_at: now,
            updated_at: now,
            stamp: Stamp {
                commit: commit.into(),
                branch: "dev".into(),
            },
        }
    }

    fn with_memories() -> Db {
        let mut db = test_db();
        for m in [
            memory("prefix", "ids use the prefix `t`", "aaaaaaa"),
            memory(
                "review",
                "the review found nine issues\nall fixed",
                "bbbbbbb",
            ),
        ] {
            db.memories.insert(m.slug.clone(), m);
        }
        db
    }

    fn distance(commit: &str) -> Distance {
        match commit {
            "aaaaaaa" => Distance::Behind(1),
            _ => Distance::Elsewhere,
        }
    }

    #[test]
    fn memory_rows_match_the_text_and_mark_the_old_ones() {
        let db = with_memories();
        let rows = memory_rows(&db, &Filter::default(), distance);
        let ages: Vec<(&str, Distance, bool)> = rows
            .iter()
            .map(|r| match r {
                Row::Memory {
                    memory,
                    distance,
                    marked,
                } => (memory.slug.as_str(), *distance, *marked),
                _ => panic!("a memory row"),
            })
            .collect();
        assert_eq!(
            ages,
            [
                ("prefix", Distance::Behind(1), false),
                ("review", Distance::Elsewhere, true)
            ]
        );
        let filter = Filter {
            text: "NINE".into(),
            ..Filter::default()
        };
        let rows = memory_rows(&db, &filter, distance);
        assert!(matches!(&rows[..], [Row::Memory { memory, .. }] if memory.slug == "review"));
        let filter = Filter {
            text: "nope".into(),
            ..Filter::default()
        };
        let rows = memory_rows(&db, &filter, distance);
        assert!(matches!(&rows[..], [Row::Note("no memory matches")]));
        assert!(matches!(
            &memory_rows(&test_db(), &Filter::default(), distance)[..],
            [Row::Note("nothing remembered yet")]
        ));
    }

    #[test]
    fn a_memory_row_is_the_slug_the_first_line_and_the_age() {
        let db = with_memories();
        let rows = memory_rows(&db, &Filter::default(), distance);
        let widths = widths(&rows);
        assert_eq!(widths.slug, 6);
        let columns = Columns::at(60);
        let lines = strings(&[
            line(&rows[0], &widths, 60, columns, false),
            line(&rows[1], &widths, 60, columns, false),
        ]);
        assert_eq!(lines[0], "prefix  ids use the prefix `t`  1 commit ago");
        assert_eq!(lines[1], "review  the review found nine issues  on dev");
        // too narrow for both, the text and the age share the room
        let short = strings(&[line(&rows[1], &widths, 30, columns, false)]);
        assert_eq!(short[0].chars().count(), 30, "{}", short[0]);
        assert!(short[0].starts_with("review  the review"), "{}", short[0]);
        assert!(short[0].ends_with("…  on d…"), "{}", short[0]);
    }

    #[test]
    fn tab_reaches_the_memories_and_enter_opens_one() {
        let db = with_memories();
        let rows = memory_rows(&db, &Filter::default(), distance);
        let mut st = State::new();
        let limits = Limits {
            scroll: 3,
            rows: 4,
            ..Limits::default()
        };
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        key(&mut st, KeyCode::Tab);
        key(&mut st, KeyCode::Tab);
        assert_eq!(st.view, View::Memories);
        assert_eq!(key(&mut st, KeyCode::Tab), Action::Rebuild(After::First));
        assert_eq!(st.view, View::List);
        key(&mut st, KeyCode::BackTab);
        assert_eq!(st.view, View::Memories);
        // the issue filters do nothing here
        assert_eq!(key(&mut st, KeyCode::Char('s')), Action::Draw);
        assert!(st.filter.status.is_none());
        st.list.select(Some(1));
        key(&mut st, KeyCode::Enter);
        assert!(matches!(&st.mode, Mode::Recall(r) if r.slug == "review" && r.scroll == 0));
        key(&mut st, KeyCode::Char('j'));
        key(&mut st, KeyCode::Char('G'));
        assert!(matches!(&st.mode, Mode::Recall(r) if r.scroll == 3));
        handle(&mut st, ctrl('u'), &rows, &db, &limits);
        assert!(matches!(&st.mode, Mode::Recall(r) if r.scroll == 1));
        key(&mut st, KeyCode::Esc);
        assert!(matches!(st.mode, Mode::Rows));
        assert_eq!(st.filter.describe(View::Memories), "");
        st.filter.closed = true;
        st.filter.text = "x".into();
        assert_eq!(st.filter.describe(View::Memories), "/x");
    }

    #[test]
    fn a_memory_page_is_laid_out_as_an_issue_page() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let db = with_memories();
        let m = &db.memories["review"];
        let header = test_header();
        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        let mut recall = Recall {
            slug: "review".into(),
            scroll: 0,
        };
        terminal
            .draw(|f| {
                draw_recall(
                    f,
                    &header,
                    View::Memories,
                    m,
                    Distance::Elsewhere,
                    true,
                    &mut recall,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        assert!(rows[0].starts_with("t  memories › review"), "{}", rows[0]);
        assert!(rows[1].ends_with("memory"), "{}", rows[1]);
        assert!(rows[3].starts_with("╭ memory "), "{}", rows[3]);
        assert!(rows[3].contains("╭ about "), "{}", rows[3]);
        assert!(rows[4].starts_with("│ review "), "{}", rows[4]);
        assert!(rows[6].starts_with("╭ text "), "{}", rows[6]);
        assert!(rows[7].contains("╭─ just now "), "{}", rows[7]);
        assert!(
            rows[8].contains("│ the review found nine issues"),
            "{}",
            rows[8]
        );
        let on = rows
            .iter()
            .position(|r| r.contains("on          dev @ bbbbbbb"))
            .unwrap();
        let warn = rows
            .iter()
            .find(|r| r.contains("on dev: may no longer hold"))
            .unwrap();
        let at = warn.find("on dev:").unwrap();
        let x = u16::try_from(warn[..at].chars().count()).unwrap();
        let y = u16::try_from(rows.iter().position(|r| r == warn).unwrap()).unwrap();
        assert!(y > u16::try_from(on).unwrap());
        assert_eq!(buffer[(x, y)].fg, Color::Yellow);
    }
}
