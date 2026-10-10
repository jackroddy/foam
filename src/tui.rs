//! The listing, the board and one issue as screens, with a cursor
//! over the issues.

use std::collections::HashSet;
use std::ops::Range;

use anyhow::Result;
use jiff::Timestamp;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::Position;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
};
use ratatui::{DefaultTerminal, Frame};

use crate::compose::{Editor, Field, Form, Target};
use crate::git::{Distance, Stamp};
use crate::graph::{self, BOARD_LOG};
use crate::md;
use crate::model::{Issue, Kind, Memory, Note, Status};
use crate::ops;
use crate::ops::NewIssue;
use crate::palette;
use crate::render;
use crate::store::{DATA_REF, Db, Store};

pub fn run(store: &Store, actor: &str) -> Result<()> {
    ratatui::run(|terminal| {
        // a terminal that speaks the kitty keyboard protocol tells
        // shift+enter from enter; the rest send the same byte
        let enhanced = supports_keyboard_enhancement().unwrap_or(false);
        if enhanced {
            ratatui::crossterm::execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        let out = screen(terminal, store, actor, enhanced);
        if enhanced {
            let _ = ratatui::crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        out
    })
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
            parts.push(status_name(s).to_string());
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
    /// What `foam board` prints: milestones, claimed, ready,
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
            View::List => "j/k move   enter open   tab board   r reload   q quit",
            View::Board => "j/k move   enter open   tab memories   r reload   q quit",
            View::Memories => "j/k move   tab list   r reload   q quit",
        }
    }
}

/// Where the person is: on the rows, or on an issue's page.
#[allow(clippy::large_enum_variant)]
enum Mode {
    Rows,
    Detail(Page),
}

/// The cursor over the memories, each a box.
#[derive(Default)]
struct Shelf {
    cursor: usize,
    scroll: usize,
    /// Whether the next draw scrolls the chosen box into view.
    reveal: bool,
}

impl Shelf {
    fn go(&mut self, to: usize) {
        self.cursor = to;
        self.reveal = true;
    }
}

/// What a page is rooted at: the issue, with its body and every
/// note; the body alone; or a note and the replies under it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Root {
    Issue,
    Body,
    Note(String),
}

/// A page left behind on the way to the one on screen, with its
/// cursor, so going back lands where it was.
struct Behind {
    issue: String,
    root: Root,
    scroll: usize,
    cursor: usize,
    tie: Option<usize>,
}

/// A page on screen, the pages opened to reach it, and where the
/// cursor is.
struct Page {
    issue: String,
    root: Root,
    /// The pages this one was opened from, oldest first.
    behind: Vec<Behind>,
    scroll: usize,
    /// The box under the cursor, counting the body as the first.
    cursor: usize,
    /// Whether the next draw scrolls the chosen box into view.
    reveal: bool,
    /// The tie under the cursor, when the cursor is on the ties.
    tie: Option<usize>,
    /// The choice box over the selected note, and what it offers.
    ask: Option<Target>,
    /// A note being written, and whether its box is open.
    draft: Option<Editor>,
    open: bool,
    /// The note to put the cursor on once the next draw finds it.
    select: Option<String>,
}

impl Page {
    fn open(id: String) -> Page {
        Page {
            issue: id,
            root: Root::Issue,
            behind: Vec::new(),
            scroll: 0,
            cursor: 0,
            reveal: true,
            tie: None,
            ask: None,
            draft: None,
            open: false,
            select: None,
        }
    }

    /// Start writing, or go on with a draft for the same target.
    fn compose(&mut self, target: Target, text: &str) {
        if self.draft.as_ref().is_none_or(|d| d.target != target) {
            self.draft = Some(Editor::new(target, text));
        }
        self.open = true;
        self.ask = None;
    }

    fn go(&mut self, to: usize) {
        self.cursor = to;
        self.reveal = true;
    }

    /// The cursor onto the next box, or the previous one; a box
    /// taller than the pane's `rows` scrolls through first, a line
    /// a step, so the cursor leaves it only once it has all shown.
    fn step_box(&mut self, boxes: &[NoteBox], rows: usize, by: isize) {
        if let Some(b) = boxes.get(self.cursor) {
            if by.is_positive() && b.lines.end > self.scroll.saturating_add(rows) {
                self.scroll += 1;
                return;
            }
            if !by.is_positive() && b.lines.start < self.scroll {
                self.scroll -= 1;
                return;
            }
        }
        let last = boxes.len().saturating_sub(1);
        let to = match by.is_positive() {
            true => (self.cursor + 1).min(last),
            false => self.cursor.saturating_sub(1),
        };
        // at either end a step goes nowhere, and does not reveal
        // the box's top again
        if to != self.cursor {
            self.go(to);
        }
    }

    fn id(&self) -> &str {
        &self.issue
    }

    /// Open another issue's page from this one.
    fn follow(&mut self, id: String) {
        self.leave();
        self.issue = id;
        self.root = Root::Issue;
    }

    /// From the issue's page, open the page of the box under the
    /// cursor: the body's, or a note's with its thread.
    fn enter_box(&mut self, boxes: &[NoteBox]) {
        if self.root != Root::Issue {
            return;
        }
        let Some(b) = boxes.get(self.cursor) else {
            return;
        };
        let root = match &b.id {
            None => Root::Body,
            Some(id) => Root::Note(id.clone()),
        };
        self.leave();
        self.root = root;
    }

    /// Put the page on screen behind the next one and start the
    /// cursor over.
    fn leave(&mut self) {
        self.behind.push(Behind {
            issue: self.issue.clone(),
            root: self.root.clone(),
            scroll: self.scroll,
            cursor: self.cursor,
            tie: self.tie,
        });
        self.scroll = 0;
        self.go(0);
        self.tie = None;
        self.ask = None;
        self.draft = None;
        self.open = false;
    }

    /// Back to the page this one was opened from, with its cursor
    /// where it was; `false` on the first.
    fn back(&mut self) -> bool {
        let Some(b) = self.behind.pop() else {
            return false;
        };
        self.issue = b.issue;
        self.root = b.root;
        self.scroll = b.scroll;
        self.go(b.cursor);
        self.tie = b.tie;
        self.ask = None;
        self.draft = None;
        self.open = false;
        true
    }

    /// Drop every page, behind or on screen, whose issue or note
    /// `db` no longer has; `false` when none is left.
    fn prune(&mut self, db: &Db) -> bool {
        let held = |issue: &str, root: &Root| match (root, db.issues.get(issue)) {
            (_, None) => false,
            (Root::Note(id), Some(i)) => i
                .notes
                .iter()
                .any(|n| n.id == *id && n.deleted_at.is_none()),
            _ => true,
        };
        self.behind.retain(|b| held(&b.issue, &b.root));
        while !held(&self.issue, &self.root) {
            if !self.back() {
                return false;
            }
        }
        true
    }

    /// What the header says of the way here: each issue, and the
    /// body or note opened on it.
    fn trail(&self) -> Vec<String> {
        let name = |issue: &str, root: &Root| match root {
            Root::Issue => issue.to_string(),
            Root::Body => "body".to_string(),
            Root::Note(id) => id.clone(),
        };
        let mut out: Vec<String> = self
            .behind
            .iter()
            .map(|b| name(&b.issue, &b.root))
            .collect();
        out.push(name(&self.issue, &self.root));
        out
    }
}

/// How many lines of its text a box on the issue's page shows.
const BOX_LINES: usize = 5;

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
}

struct Screen<'a> {
    view: View,
    header: Header,
    rows: Vec<Row<'a>>,
    widths: Widths,
    /// The data ref's last commits, newest first, under the list
    /// and the board.
    recent: Vec<Row<'a>>,
}

/// What every screen says at the top: the prefix, where HEAD is,
/// and the counts as badges in the status colours.
struct Header {
    prefix: String,
    stamp: String,
    stats: Vec<Span<'static>>,
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
    shelf: Shelf,
    /// Whether the terminal tells shift+enter from enter.
    enhanced: bool,
    /// What the last write did, or why it did not, for one draw.
    notice: Option<String>,
    /// A new issue being written on the rows, and whether its form
    /// is shown.
    form: Option<Form>,
    form_shown: bool,
    /// The issue to put the cursor on once the rows are rebuilt.
    land: Option<String>,
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
            shelf: Shelf::default(),
            enhanced: true,
            notice: None,
            form: None,
            form_shown: false,
            land: None,
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
    /// Write the page's draft to the ref, then reload.
    Send,
}

/// Where the cursor lands after the rows are rebuilt.
#[derive(Debug, PartialEq, Eq)]
enum After {
    /// Where it was, if that is still an issue row.
    Kept,
    First,
    On(String),
}

fn screen(
    terminal: &mut DefaultTerminal,
    store: &Store,
    actor: &str,
    enhanced: bool,
) -> Result<()> {
    let mut st = State::new();
    st.enhanced = enhanced;
    loop {
        let snap = store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("{}", crate::store::NOT_INITIALIZED))?;
        let mut screen = build(store, &snap.db, st.view, &st.filter, actor, &st.folded)?;
        keep(&mut st.list, &screen.rows);
        if let Some(id) = st.land.take() {
            let at = screen
                .rows
                .iter()
                .position(|r| matches!(r, Row::Issue { issue, .. } if issue.id == id));
            if at.is_some() {
                st.list.select(at);
            }
        }
        // a page whose issues a reload took away falls back
        if let Mode::Detail(page) = &mut st.mode
            && !page.prune(&snap.db)
        {
            st.mode = Mode::Rows;
        }
        loop {
            let mut limits = Limits::default();
            match &mut st.mode {
                Mode::Rows => {
                    let context = if st.typing {
                        vec![Span::raw(format!("/{}▏", st.filter.text))]
                    } else {
                        vec![Span::raw(st.filter.describe(st.view)).dim()]
                    };
                    if st.view == View::Memories {
                        let shelf = &mut st.shelf;
                        terminal.draw(|frame| {
                            limits = draw_memories(frame, &screen, shelf, &context);
                        })?;
                    } else {
                        let form = st.form_shown.then_some(st.form.as_ref()).flatten();
                        let (list, preview) = (&mut st.list, st.preview);
                        let (enhanced, notice) = (st.enhanced, st.notice.as_deref());
                        terminal.draw(|frame| {
                            draw_rows(
                                frame,
                                &screen,
                                list,
                                &snap.db,
                                preview,
                                &context,
                                Compose {
                                    form,
                                    actor,
                                    enhanced,
                                    notice,
                                },
                            )
                        })?;
                    }
                }
                Mode::Detail(page) => {
                    let issue = &snap.db.issues[page.id()];
                    let (enhanced, notice) = (st.enhanced, st.notice.as_deref());
                    terminal.draw(|frame| {
                        limits = draw_detail(
                            frame,
                            &screen.header,
                            st.view,
                            &snap.db,
                            issue,
                            page,
                            actor,
                            enhanced,
                            notice,
                        );
                    })?;
                }
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            st.notice = None;
            match handle(&mut st, key, &screen.rows, &snap.db, &limits) {
                Action::Quit => return Ok(()),
                Action::Reload => break,
                Action::Draw => {}
                Action::Send => {
                    if send(&mut st, store, actor) {
                        break;
                    }
                }
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

/// Write the page's draft through the ops the commands use. The
/// footer says what happened; on success the page reloads with the
/// cursor on the note.
fn send(st: &mut State, store: &Store, actor: &str) -> bool {
    if st.form_shown
        && let Some(form) = &st.form
    {
        if form.title.is_blank() {
            st.notice = Some("a title is needed".into());
            return false;
        }
        let result = match &form.edit {
            Some(id) => ops::update(
                store,
                std::slice::from_ref(id),
                actor,
                &ops::Changes {
                    title: Some(form.title.text()),
                    body: Some(form.body.text()),
                    kind: Some(form.kind),
                    priority: Some(form.priority),
                    status: None,
                    assignee: None,
                    add_label: Vec::new(),
                    rm_label: Vec::new(),
                    // an empty parent clears it
                    parent: Some(form.parent_id().unwrap_or("").to_string()),
                    defer_until: None,
                    resolution: None,
                    reason: None,
                },
            )
            .map(|mut issues| ("updated", issues.remove(0))),
            None => ops::create(
                store,
                &NewIssue {
                    title: form.title.text(),
                    body: form.body.text(),
                    kind: form.kind,
                    priority: form.priority,
                    labels: Vec::new(),
                    parent: form.parent_id().map(str::to_string),
                    blocked_by: Vec::new(),
                },
            )
            .map(|issue| ("created", issue)),
        };
        return match result {
            Ok((said, issue)) => {
                st.notice = Some(format!("{said} {}", issue.id));
                st.land = Some(issue.id);
                st.form = None;
                st.form_shown = false;
                true
            }
            Err(e) => {
                st.notice = Some(e.to_string());
                false
            }
        };
    }
    let Mode::Detail(page) = &mut st.mode else {
        return false;
    };
    let Some(draft) = &page.draft else {
        return false;
    };
    let text = draft.text();
    let result = match &draft.target {
        Target::Note => ops::note(store, &page.issue, actor, &text, None),
        Target::Reply(to) => ops::note(store, &page.issue, actor, &text, Some(to)),
        Target::Edit(id) => ops::edit_note(store, &page.issue, id, &text),
    };
    match result {
        Ok(issue) => {
            let (said, id) = match &draft.target {
                Target::Edit(id) => ("edited", id.clone()),
                _ => (
                    "noted",
                    issue.notes.last().map(|n| n.id.clone()).unwrap_or_default(),
                ),
            };
            st.notice = Some(format!("{said} {id}"));
            page.select = Some(id);
            page.draft = None;
            page.open = false;
            true
        }
        Err(e) => {
            st.notice = Some(e.to_string());
            false
        }
    }
}

/// Apply one key to the state and say what the loop does next.
fn handle(st: &mut State, key: KeyEvent, rows: &[Row], db: &Db, limits: &Limits) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if st.form_shown
        && let Some(form) = &mut st.form
    {
        return handle_form(form, key, &mut st.form_shown);
    }
    if let Mode::Detail(page) = &mut st.mode {
        if page.open {
            return handle_compose(page, key);
        }
        if page.ask.is_some() {
            return handle_ask(page, key.code, limits, db);
        }
    }
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
        Mode::Rows => handle_rows(st, key.code, ctrl, rows, db, limits),
        Mode::Detail(page) => {
            let last_tie = limits.ties.len().saturating_sub(1);
            // a thread's page has no cursor: it scrolls by the line
            let thread = matches!(page.root, Root::Note(_));
            match key.code {
                KeyCode::Esc | KeyCode::Backspace => {
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
                KeyCode::Enter => match page.tie {
                    Some(k) => {
                        if let Some(id) = limits.ties.get(k) {
                            page.follow(id.clone());
                        }
                    }
                    None if thread => {
                        if limits.boxes.get(page.cursor).is_some() {
                            page.ask = Some(Target::Reply(String::new()));
                        }
                    }
                    None => page.enter_box(&limits.boxes),
                },
                KeyCode::Char('l') | KeyCode::Right if thread && page.tie.is_none() => {
                    let last = limits.boxes.len().saturating_sub(1);
                    page.go((page.cursor + 1).min(last));
                }
                KeyCode::Char('h') | KeyCode::Left if thread && page.tie.is_none() => {
                    page.go(page.cursor.saturating_sub(1));
                }
                KeyCode::Char('n') if page.root == Root::Issue && page.tie.is_none() => {
                    page.compose(Target::Note, "");
                }
                KeyCode::Char('d') if ctrl => {
                    page.scroll = (page.scroll + limits.rows / 2).min(limits.scroll);
                }
                KeyCode::Char('u') if ctrl => {
                    page.scroll = page.scroll.saturating_sub(limits.rows / 2);
                }
                KeyCode::Char('j') | KeyCode::Down => match page.tie {
                    Some(k) => page.tie = Some((k + 1).min(last_tie)),
                    None if thread => page.scroll = (page.scroll + 1).min(limits.scroll),
                    None => page.step_box(&limits.boxes, limits.rows, 1),
                },
                KeyCode::Char('k') | KeyCode::Up => match page.tie {
                    Some(k) => page.tie = Some(k.saturating_sub(1)),
                    None if thread => page.scroll = page.scroll.saturating_sub(1),
                    None => page.step_box(&limits.boxes, limits.rows, -1),
                },
                KeyCode::Char('g') | KeyCode::Home => match page.tie {
                    Some(_) => page.tie = Some(0),
                    None if thread => page.scroll = 0,
                    None => page.go(0),
                },
                KeyCode::Char('G') | KeyCode::End => match page.tie {
                    Some(_) => page.tie = Some(last_tie),
                    None if thread => page.scroll = limits.scroll,
                    None => page.go(limits.boxes.len().saturating_sub(1)),
                },
                _ => {}
            }
            Action::Draw
        }
    }
}

/// A key on the new-issue form. Typing into a text field: enter or
/// esc leaves it, shift+enter, alt+enter and ctrl+j break a line in
/// the body. Otherwise j, k and tab walk the fields, enter opens a
/// text field or turns a choice, h and l turn a choice or pick a
/// button, enter on create sends, and esc or cancel closes the
/// form and keeps it.
fn handle_form(form: &mut Form, key: KeyEvent, shown: &mut bool) -> Action {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if form.editing {
        let body = form.field == Field::Body;
        let Some(ed) = form.editor() else {
            form.editing = false;
            return Action::Draw;
        };
        match key.code {
            KeyCode::Esc => form.editing = false,
            KeyCode::Enter if body && (shift || alt) => ed.newline(),
            KeyCode::Char('j') if body && ctrl => ed.newline(),
            // enter confirms the field and moves to the next, which
            // is typed into at once when it is the body
            KeyCode::Enter => {
                form.field = form.field.next();
                form.editing = form.field == Field::Body;
            }
            KeyCode::Backspace => ed.backspace(),
            KeyCode::Delete => ed.delete(),
            KeyCode::Left => ed.left(),
            KeyCode::Right => ed.right(),
            KeyCode::Up => ed.up(),
            KeyCode::Down => ed.down(),
            KeyCode::Home => ed.home(),
            KeyCode::End => ed.end(),
            KeyCode::Char(c) if !ctrl => ed.insert(c),
            _ => {}
        }
        return Action::Draw;
    }
    match key.code {
        KeyCode::Esc => *shown = false,
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => form.field = form.field.next(),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => form.field = form.field.previous(),
        KeyCode::Char('l') | KeyCode::Right => form.turn(1),
        KeyCode::Char('h') | KeyCode::Left => form.turn(-1),
        KeyCode::Enter => match form.field {
            Field::Title | Field::Body => form.editing = true,
            Field::Buttons if form.cancel => *shown = false,
            Field::Buttons => return Action::Send,
            _ => form.field = form.field.next(),
        },
        _ => {}
    }
    Action::Draw
}

/// A key on the choice box over a selected note: h, l, j and k
/// move between reply and edit, enter takes the one shown, esc
/// closes the box.
fn handle_ask(page: &mut Page, code: KeyCode, limits: &Limits, db: &Db) -> Action {
    let Some(id) = limits.boxes.get(page.cursor).and_then(|b| b.id.clone()) else {
        page.ask = None;
        return Action::Draw;
    };
    let editing = matches!(page.ask, Some(Target::Edit(_)));
    match code {
        KeyCode::Esc => page.ask = None,
        KeyCode::Char('h' | 'l' | 'j' | 'k') | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
            page.ask = Some(if editing {
                Target::Reply(String::new())
            } else {
                Target::Edit(String::new())
            });
        }
        KeyCode::Enter => {
            let note = db
                .issues
                .get(&page.issue)
                .and_then(|i| i.notes.iter().find(|n| n.id == id));
            if editing {
                let text = note.map(|n| n.text.clone()).unwrap_or_default();
                page.compose(Target::Edit(id), &text);
            } else {
                page.compose(Target::Reply(id), "");
            }
        }
        _ => {}
    }
    Action::Draw
}

/// A key in an open compose box: enter sends, shift+enter,
/// alt+enter and ctrl+j break the line, esc closes the box and
/// keeps the draft, ctrl+c quits.
fn handle_compose(page: &mut Page, key: KeyEvent) -> Action {
    let Some(ed) = &mut page.draft else {
        page.open = false;
        return Action::Draw;
    };
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => page.open = false,
        KeyCode::Enter if shift || alt => ed.newline(),
        KeyCode::Char('j') if ctrl => ed.newline(),
        KeyCode::Char('c') if ctrl => return Action::Quit,
        KeyCode::Enter => {
            if !ed.is_blank() {
                return Action::Send;
            }
        }
        KeyCode::Backspace => ed.backspace(),
        KeyCode::Delete => ed.delete(),
        KeyCode::Left => ed.left(),
        KeyCode::Right => ed.right(),
        KeyCode::Up => ed.up(),
        KeyCode::Down => ed.down(),
        KeyCode::Home => ed.home(),
        KeyCode::End => ed.end(),
        KeyCode::Tab => {
            for _ in 0..4 {
                ed.insert(' ');
            }
        }
        KeyCode::Char(c) if !ctrl => ed.insert(c),
        _ => {}
    }
    Action::Draw
}

/// A key on the rows.
fn handle_rows(
    st: &mut State,
    code: KeyCode,
    ctrl: bool,
    rows: &[Row],
    db: &Db,
    limits: &Limits,
) -> Action {
    match code {
        KeyCode::Esc if st.filter.is_empty() => return Action::Quit,
        KeyCode::Esc => {
            st.filter = Filter::default();
            return Action::Rebuild(After::Kept);
        }
        KeyCode::Char('/') => {
            st.typing = true;
            return Action::Rebuild(After::Kept);
        }
        KeyCode::Tab => {
            st.view = st.view.next();
            st.form = None;
            return Action::Rebuild(After::First);
        }
        KeyCode::BackTab => {
            st.view = st.view.previous();
            st.form = None;
            return Action::Rebuild(After::First);
        }
        _ => {}
    }
    if st.view == View::Memories {
        return handle_shelf(&mut st.shelf, code, ctrl, limits);
    }
    let row = st.list.selected().and_then(|i| rows.get(i));
    let chosen = match row {
        Some(Row::Issue { issue, .. }) => Some(*issue),
        _ => None,
    };
    match code {
        KeyCode::Char('n') => {
            // under the chosen issue when that is a milestone; a
            // kept form comes back as it was
            if st.form.is_none() {
                let parents: Vec<(String, String)> = db
                    .issues
                    .values()
                    .filter(|i| i.kind == Kind::Milestone && i.status != Status::Closed)
                    .map(|i| (i.id.clone(), i.title.clone()))
                    .collect();
                let under = chosen
                    .filter(|i| i.kind == Kind::Milestone)
                    .map(|i| i.id.as_str());
                st.form = Some(Form::new(parents, under));
            }
            st.form_shown = true;
            Action::Draw
        }
        KeyCode::Char('e') => {
            if let Some(i) = chosen {
                let parents: Vec<(String, String)> = db
                    .issues
                    .values()
                    .filter(|m| {
                        m.kind == Kind::Milestone && m.status != Status::Closed && m.id != i.id
                    })
                    .map(|m| (m.id.clone(), m.title.clone()))
                    .collect();
                st.form = Some(Form::edit(
                    &i.id,
                    &i.title,
                    &i.body,
                    i.kind,
                    i.priority,
                    i.parent.as_deref(),
                    parents,
                ));
                st.form_shown = true;
            }
            Action::Draw
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
        KeyCode::Enter => {
            if let Some(issue) = chosen {
                st.mode = Mode::Detail(Page::open(issue.id.clone()));
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

/// A key on the memories.
fn handle_shelf(shelf: &mut Shelf, code: KeyCode, ctrl: bool, limits: &Limits) -> Action {
    let last = limits.boxes.len().saturating_sub(1);
    match code {
        KeyCode::Char('d') if ctrl => {
            shelf.scroll = (shelf.scroll + limits.rows / 2).min(limits.scroll);
        }
        KeyCode::Char('u') if ctrl => {
            shelf.scroll = shelf.scroll.saturating_sub(limits.rows / 2);
        }
        KeyCode::Char('j') | KeyCode::Down => shelf.go((shelf.cursor + 1).min(last)),
        KeyCode::Char('k') | KeyCode::Up => shelf.go(shelf.cursor.saturating_sub(1)),
        KeyCode::Char('g') | KeyCode::Home => shelf.go(0),
        KeyCode::Char('G') | KeyCode::End => shelf.go(last),
        _ => {}
    }
    Action::Draw
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
    let rows = match view {
        View::List => list_rows(db, filter, actor, folded),
        View::Board => board_rows(db, now),
        View::Memories => memory_rows(db, filter, |commit| store.git.distance(commit)),
    };
    let mut recent = Vec::new();
    if view != View::Memories {
        let style = human();
        let log: Vec<(String, String, String)> = store
            .git
            .log(DATA_REF, BOARD_LOG, None)?
            .into_iter()
            .map(|(oid, when, msg)| {
                let when = when
                    .parse::<Timestamp>()
                    .map(|t| style.when(t))
                    .unwrap_or(when);
                (oid, when, msg)
            })
            .collect();
        // the times line up, right-aligned to the widest
        let when_w = log
            .iter()
            .map(|(_, w, _)| w.chars().count())
            .max()
            .unwrap_or(0);
        for (oid, when, msg) in log {
            recent.push(Row::Log {
                oid: oid[..7].to_string(),
                when: format!("{when:>when_w$}"),
                msg,
            });
        }
    }
    let widths = widths(&rows);
    Ok(Screen {
        view,
        header,
        rows,
        widths,
        recent,
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
            widths.status = widths.status.max(status_name(issue.status).len());
            widths.kind = widths.kind.max(issue.kind.to_string().len());
            widths.rollup = widths.rollup.max(rollup_text(*rollup).len());
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
    let mut parts: Vec<(String, Color)> = [
        Status::Open,
        Status::InProgress,
        Status::Deferred,
        Status::Closed,
    ]
    .into_iter()
    .map(|s| {
        (
            format!("{} {} {}", status_glyph(s), count(s), status_name(s)),
            status_colour(s),
        )
    })
    .collect();
    parts.push((format!("✦ {} memories", db.memories.len()), palette::AGENT));
    Header {
        prefix: db.meta.prefix.clone(),
        stamp: format!("{} @ {}", stamp.branch, stamp.commit),
        stats: flow(&parts),
    }
}

/// Badges run together, each flowing into the next through an
/// arrow in both their colours.
fn flow(parts: &[(String, Color)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut previous = None;
    for (text, colour) in parts {
        // U+E0B2 is powerline's left-pointing arrow, which a Nerd
        // or Powerline font has and most others lack
        let mut arrow = Style::new().fg(*colour);
        if let Some(p) = previous {
            arrow = arrow.bg(p);
        }
        spans.push(Span::styled("\u{e0b2}", arrow));
        spans.push(badge(text, *colour));
        previous = Some(*colour);
    }
    spans
}

/// The status as the screen says it; the stored word differs for
/// a claim, until the schema is next bumped.
fn status_name(s: Status) -> &'static str {
    match s {
        Status::Open => "open",
        Status::InProgress => "claimed",
        Status::Deferred => "deferred",
        Status::Closed => "closed",
    }
}

fn status_glyph(s: Status) -> &'static str {
    match s {
        Status::Open => "○",
        Status::InProgress => "◐",
        Status::Deferred => "◌",
        Status::Closed => "●",
    }
}

/// The header's rows: the prefix as a badge and the views with
/// the one in use bright, HEAD at the right edge; `context`, what
/// the screen under it is about, with the stats at the edge; a
/// rule.
fn header_lines(
    h: &Header,
    view: View,
    context: &[Span<'static>],
    width: usize,
) -> [Line<'static>; 3] {
    let dim = |s: &str| Span::raw(s.to_string()).dim();
    let mut top = vec![badge(&h.prefix, palette::REF), Span::raw("  "), dim("[ ")];
    for (n, v) in [View::List, View::Board, View::Memories]
        .into_iter()
        .enumerate()
    {
        if n > 0 {
            top.push(dim(" │ "));
        }
        top.push(if v == view {
            Span::raw(v.name()).bold().fg(palette::FOCUS)
        } else {
            dim(v.name())
        });
    }
    top.push(dim(" ]"));
    let against = |mut spans: Vec<Span<'static>>, right: Vec<Span<'static>>| {
        let len =
            |spans: &[Span]| -> usize { spans.iter().map(|s| s.content.chars().count()).sum() };
        let gap = width.saturating_sub(len(&spans) + len(&right));
        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(right);
        Line::from(spans)
    };
    [
        against(top, vec![dim(&h.stamp)]),
        against(context.to_vec(), h.stats.clone()),
        Line::from(Span::raw("─".repeat(width)).dim()),
    ]
}

/// The trail of pages opened from a view, as the header's context.
fn trail_spans(view: View, ids: &[String]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(view.name()).dim()];
    for (n, id) in ids.iter().enumerate() {
        spans.push(Span::raw(" › ").dim());
        spans.push(if n + 1 == ids.len() {
            Span::raw(id.clone()).bold().fg(palette::REF)
        } else {
            Span::raw(id.clone()).fg(palette::REF)
        });
    }
    spans
}

fn draw_header(frame: &mut Frame, area: Rect, h: &Header, view: View, context: &[Span<'static>]) {
    let lines = header_lines(h, view, context, usize::from(area.width));
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
        rows.push(Row::Header("milestones"));
        rows.extend(
            board
                .milestones
                .into_iter()
                .map(|i| issue(i, String::new())),
        );
        rows.push(Row::End);
    }
    if !board.in_progress.is_empty() {
        rows.push(Row::Header("claimed"));
        rows.extend(
            board
                .in_progress
                .into_iter()
                .map(|i| issue(i, render::lease_left(i))),
        );
        rows.push(Row::End);
    }
    rows.push(Row::Header("ready"));
    if board.ready.is_empty() {
        rows.push(Row::Note("nothing is ready"));
    }
    rows.extend(board.ready.into_iter().map(|i| issue(i, String::new())));
    rows.push(Row::End);
    if !board.blocked.is_empty() {
        rows.push(Row::Header("blocked"));
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
        Some((closed, total)) if total > 0 && closed == total => {
            Style::new().add_modifier(Modifier::BOLD)
        }
        _ => Style::new(),
    }
}

/// A person's name is cyan, an agent's magenta: the session
/// suffix is what tells them apart.
/// A person is bold; the session suffix marks an agent, and the
/// colour says so from across the room.
fn actor(name: &str) -> Style {
    if name.contains('/') {
        Style::new().fg(palette::AGENT)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

/// A milestone's kind stands out; every other kind recedes.
fn kind_style(kind: Kind) -> Style {
    if kind == Kind::Milestone {
        Style::new().add_modifier(Modifier::BOLD)
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
        spans.push(Span::raw(id.to_string()).fg(palette::REF));
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

/// What the rows draw besides themselves: a new-issue form, who is
/// writing it, what the terminal's keyboard can say, and a notice.
struct Compose<'a> {
    form: Option<&'a Form>,
    actor: &'a str,
    enhanced: bool,
    notice: Option<&'a str>,
}

fn draw_rows(
    frame: &mut Frame,
    screen: &Screen,
    state: &mut ListState,
    db: &Db,
    preview: bool,
    context: &[Span<'static>],
    compose: Compose,
) {
    let [top, body, foot] = frame_areas(frame);
    draw_header(frame, top, &screen.header, screen.view, context);
    let chosen = match state.selected().map(|i| &screen.rows[i]) {
        Some(Row::Issue { issue, .. }) if preview => Some(*issue),
        _ => None,
    };
    // the recent commits take the lower third, and the preview
    // gives way inside what is left
    let [upper, recent_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(body.height / 3)]).areas(body);
    let (rows_area, preview_area) = match chosen {
        None => (upper, None),
        Some(_) if body.width >= WIDE => {
            let [rows, preview] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(PREVIEW_WIDTH)])
                    .areas(upper);
            (rows, Some(preview))
        }
        Some(_) => {
            let [rows, preview] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(upper.height / 2)])
                    .areas(upper);
            (rows, Some(preview))
        }
    };
    let boxed = screen.view == View::Board;
    // the list sits in a pane; the board's sections are boxes
    // of their own
    let rows_area = if boxed {
        rows_area
    } else {
        let block = pane(Span::raw(" list "), true);
        let inside = block.inner(rows_area);
        frame.render_widget(block, rows_area);
        inside
    };
    // the gutter takes two columns from every row of the list; on
    // the board the marker sits inside the box
    let width = usize::from(rows_area.width).saturating_sub(if boxed { 0 } else { 2 });
    let columns = Columns::at(if boxed {
        width.saturating_sub(6)
    } else {
        width
    });
    // the section the cursor is in has the bright title, as a pane
    // with the cursor does
    let section = state.selected().and_then(|i| {
        (0..=i)
            .rev()
            .find(|&k| matches!(screen.rows[k], Row::Header(_)))
    });
    let mut items: Vec<ListItem> = screen
        .rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let chosen = state.selected() == Some(n) || section == Some(n);
            let mut line = line(row, &screen.widths, width, columns, boxed, chosen);
            // a boxed row shades only what is inside its edges
            if boxed && state.selected() == Some(n) {
                let last = line.spans.len().saturating_sub(1);
                for span in &mut line.spans[1..last] {
                    span.style = span.style.patch(CHOSEN);
                }
            }
            ListItem::new(line)
        })
        .collect();
    // the form is one item under the chosen row, and the List is
    // told it is the chosen one so it scrolls into view; the real
    // cursor stays where it was
    let mut cursor = None;
    let at = state.selected().map_or(0, |n| (n + 1).min(items.len()));
    if let Some(form) = compose.form {
        let edge = usize::from(boxed) * 4;
        let (mut lines, caret) = form_box(form, compose.actor, width.saturating_sub(edge));
        if boxed {
            let dim = Style::new().add_modifier(Modifier::DIM);
            for l in &mut lines {
                l.spans.insert(0, Span::styled("│ ", dim));
                l.spans.push(Span::styled(" │", dim));
            }
        }
        cursor = caret.map(|(row, col)| (row, col + edge / 2));
        items.insert(at, ListItem::new(lines));
    }
    // a row of padding keeps a section's top and bottom borders
    // in view when the cursor is on its first or last issue
    let list = List::new(items)
        .highlight_style(if boxed || compose.form.is_some() {
            Style::new()
        } else {
            CHOSEN
        })
        // the gutter stays while the form is up, so the rows do not
        // shift; the marker alone goes
        .highlight_symbol(match (boxed, compose.form.is_some()) {
            (true, _) => "",
            (false, true) => "  ",
            (false, false) => "› ",
        })
        .highlight_spacing(HighlightSpacing::Always)
        .scroll_padding(1);
    if compose.form.is_some() {
        let mut shown = *state;
        shown.select(Some(at));
        frame.render_stateful_widget(list, rows_area, &mut shown);
        *state.offset_mut() = shown.offset();
        // the terminal's cursor sits in the field being typed into
        if let Some((row, col)) = cursor
            && at >= state.offset()
        {
            let y = rows_area.y + u16::try_from(at - state.offset() + row).unwrap_or(u16::MAX);
            let gutter = if boxed { 0 } else { 2 };
            let x = rows_area.x + gutter + u16::try_from(col).unwrap_or(u16::MAX);
            if y < rows_area.y + rows_area.height {
                frame.set_cursor_position(Position { x, y });
            }
        }
    } else {
        frame.render_stateful_widget(list, rows_area, state);
    }
    draw_recent(frame, recent_area, screen);
    if let (Some(issue), Some(area)) = (chosen, preview_area) {
        let lines = preview_lines(db, issue, inner(area.width));
        frame.render_widget(
            Paragraph::new(lines).block(pane(
                Span::raw(format!(" {} ", issue.id)).fg(palette::REF),
                false,
            )),
            area,
        );
    }
    let keys = if let Some(notice) = compose.notice {
        notice.to_string()
    } else if let Some(form) = compose.form {
        let newline = if compose.enhanced {
            "shift+enter newline"
        } else {
            "alt+enter newline"
        };
        match (form.editing, form.field) {
            (true, Field::Body) => format!("enter done   {newline}   esc done"),
            (true, _) => "enter done   esc done".to_string(),
            (false, Field::Title | Field::Body) => {
                "j/k field   enter type   esc keep the draft".to_string()
            }
            (false, Field::Buttons) => "h/l pick   enter go   esc keep the draft".to_string(),
            (false, _) => "j/k field   h/l turn   esc keep the draft".to_string(),
        }
    } else {
        format!(
            "{}   n new   e edit   space fold   p {}   s/t/a/c / filter",
            screen.view.keys(),
            if preview { "no preview" } else { "preview" }
        )
    };
    frame.render_widget(Paragraph::new(keys).dim(), foot);
}

/// The new-issue form as a box `width` wide: a label and a value
/// per field, the selected label bright, the body as tall as its
/// text, then the two buttons; and where the terminal's cursor goes
/// when a text field is being typed into.
fn form_box(
    form: &Form,
    actor: &str,
    width: usize,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let border = Style::new().fg(palette::FOCUS);
    let edge = |s: String| Span::styled(s, border);
    let inner = width.saturating_sub(4).max(24);
    let label_w = 10;
    let value_w = inner - label_w;
    let who = Span::styled(
        actor.to_string(),
        self::actor(actor).add_modifier(Modifier::BOLD),
    );
    let what = match &form.edit {
        Some(id) => format!(" · editing {id} "),
        None => " · new issue ".to_string(),
    };
    let used = 2 + actor.chars().count() + what.chars().count() + 1;
    let mut out = vec![Line::from(vec![
        edge("╭ ".into()),
        who,
        edge(what),
        edge("─".repeat(width.saturating_sub(used))),
        edge("╮".into()),
    ])];
    let mut caret = None;
    let label = |name: &str, field: Field| {
        if form.field == field {
            Span::raw(pad(name, label_w)).bold().fg(palette::FOCUS)
        } else {
            Span::raw(pad(name, label_w)).dim()
        }
    };
    let row = |spans: Vec<Span<'static>>, out: &mut Vec<Line<'static>>| {
        let mut line = vec![edge("│ ".into())];
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        line.extend(spans);
        line.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        line.push(edge(" │".into()));
        out.push(Line::from(line));
    };
    // a text field: its rows, with the cursor when it is typed into
    let mut text = |name: &str, field: Field, ed: &Editor, out: &mut Vec<Line<'static>>| {
        let (rows, (r, c)) = ed.wrapped(value_w);
        for (n, piece) in rows.iter().enumerate() {
            let first = if n == 0 {
                label(name, field)
            } else {
                Span::raw(" ".repeat(label_w))
            };
            row(vec![first, Span::raw(pad(piece, value_w))], out);
        }
        if form.editing && form.field == field {
            caret = Some((out.len() - rows.len() + r, 2 + label_w + c));
        }
    };
    text("title", Field::Title, &form.title, &mut out);
    text("body", Field::Body, &form.body, &mut out);
    row(
        vec![
            label("kind", Field::Kind),
            Span::styled(form.kind.to_string(), kind_style(form.kind)),
        ],
        &mut out,
    );
    row(
        vec![label("priority", Field::Priority), priority(form.priority)],
        &mut out,
    );
    let parent = match form.parent {
        Some(k) => vec![
            Span::raw(form.parents[k].0.clone()).fg(palette::REF),
            Span::raw(cut(
                &format!("  {}", form.parents[k].1),
                value_w.saturating_sub(form.parents[k].0.chars().count()),
            ))
            .dim(),
        ],
        None => vec![Span::raw("none").dim()],
    };
    let mut spans = vec![label("parent", Field::Parent)];
    spans.extend(parent);
    row(spans, &mut out);
    row(Vec::new(), &mut out);
    let button = |name: &str, on: bool| {
        if form.field == Field::Buttons && on {
            Span::raw(name.to_string()).bold().fg(palette::FOCUS)
        } else if form.field == Field::Buttons {
            Span::raw(name.to_string()).dim()
        } else {
            Span::raw(name.to_string())
        }
    };
    row(
        vec![
            Span::raw(" ".repeat(label_w)),
            button(
                if form.edit.is_some() {
                    "save"
                } else {
                    "create"
                },
                !form.cancel,
            ),
            Span::raw("   "),
            button("cancel", form.cancel),
        ],
        &mut out,
    );
    out.push(Line::from(vec![
        edge("╰".into()),
        edge("─".repeat(width.saturating_sub(2))),
        edge("╯".into()),
    ]));
    (out, caret)
}

/// The recent pane: as many of the data ref's last commits as fit.
fn draw_recent(frame: &mut Frame, area: Rect, screen: &Screen) {
    let block = pane(Span::raw(" recent "), false);
    let inside = block.inner(area);
    let width = usize::from(inside.width);
    let mut lines: Vec<Line> = screen
        .recent
        .iter()
        .take(usize::from(inside.height))
        .map(|r| line(r, &screen.widths, width, Columns::at(width), false, false))
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::raw("nothing yet").dim()));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
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
        Span::raw(format!("{} @ {}", m.stamp.branch, m.stamp.commit)).fg(palette::REF),
    ]));
    lines.push(Line::default());
    let age = short_age(&m.stamp, distance);
    lines.push(Line::from(if marked {
        Span::raw(format!("{age}: may no longer hold")).fg(palette::ALARM)
    } else {
        Span::raw(age).dim()
    }));
    lines
}

/// Draw the memories as an issue's page: each a box in the main
/// pane, the one under the cursor in the accent colour, and the
/// about pane saying where that one stands.
fn draw_memories(
    frame: &mut Frame,
    screen: &Screen,
    shelf: &mut Shelf,
    context: &[Span<'static>],
) -> Limits {
    let [top, body, foot] = frame_areas(frame);
    draw_header(frame, top, &screen.header, screen.view, context);
    let memories: Vec<(&Memory, Distance, bool)> = screen
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Memory {
                memory,
                distance,
                marked,
            } => Some((*memory, *distance, *marked)),
            _ => None,
        })
        .collect();
    shelf.cursor = shelf.cursor.min(memories.len().saturating_sub(1));
    let about = match memories.get(shelf.cursor) {
        Some((m, d, marked)) => {
            let mut lines = vec![
                Line::from(Span::raw(m.slug.clone()).bold()),
                Line::default(),
            ];
            lines.extend(memory_about(m, *d, *marked));
            lines
        }
        None => Vec::new(),
    };
    let wide = body.width >= WIDE;
    let (boxes_area, about_area) = if wide {
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(ABOUT_WIDTH)]).areas(body);
        (left, right)
    } else {
        let about_h = u16::try_from(about.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(body.height / 2);
        let [about_area, boxes] =
            Layout::vertical([Constraint::Length(about_h), Constraint::Min(0)]).areas(body);
        (boxes, about_area)
    };
    frame.render_widget(
        Paragraph::new(about).block(pane(Span::raw(" about "), false)),
        about_area,
    );

    let style = human();
    let width = inner(boxes_area.width).max(20);
    let mut lines = Vec::new();
    let mut boxes = Vec::new();
    for (k, (m, d, marked)) in memories.iter().enumerate() {
        if k > 0 {
            lines.push(Line::default());
        }
        let start = lines.len();
        lines.extend(post_box(
            &Post {
                id: None,
                author: Some((&m.slug, Style::new().fg(palette::REF))),
                edited: (m.updated_at != m.created_at).then_some(m.updated_at),
                at: m.created_at,
                text: &m.text,
                branch: &m.stamp.branch,
                commit: &m.stamp.commit,
                thread: None,
                flag: marked.then(|| format!("{}, may no longer hold", short_age(&m.stamp, *d))),
                limit: None,
            },
            &style,
            width,
            k == shelf.cursor,
        ));
        boxes.push(NoteBox {
            id: Some(m.slug.clone()),
            lines: start..lines.len(),
        });
    }
    if memories.is_empty() {
        for r in &screen.rows {
            if let Row::Note(n) = r {
                lines.push(Line::from(Span::raw(*n).dim()));
            }
        }
    }
    let rows = rows_in(boxes_area.height);
    let max = lines.len().saturating_sub(rows);
    if shelf.reveal {
        reveal(&mut shelf.scroll, shelf.cursor, &boxes, rows);
        shelf.reveal = false;
    }
    shelf.scroll = shelf.scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(Span::raw(" memories "), true))
            .scroll((u16::try_from(shelf.scroll).unwrap_or(u16::MAX), 0)),
        boxes_area,
    );
    let keys = "j/k move   g/G ends   ^d/^u scroll   tab list   / filter   r reload   q quit";
    frame.render_widget(Paragraph::new(keys).dim(), foot);
    Limits {
        scroll: max,
        rows,
        boxes,
        ties: Vec::new(),
    }
}

/// Scroll so the box at `cursor` is in view; a box taller than
/// the pane shows its top.
fn reveal(scroll: &mut usize, cursor: usize, boxes: &[NoteBox], rows: usize) {
    if let Some(b) = boxes.get(cursor) {
        reveal_lines(scroll, &b.lines, rows);
    }
}

fn reveal_lines(scroll: &mut usize, lines: &Range<usize>, rows: usize) {
    if lines.start < *scroll {
        *scroll = lines.start;
    } else if lines.end > *scroll + rows {
        *scroll = lines.end.saturating_sub(rows).min(lines.start);
    }
}

/// The choice box over the notes: reply or edit, the one shown
/// bright.
fn draw_ask(frame: &mut Frame, area: Rect, choice: &Target) {
    let editing = matches!(choice, Target::Edit(_));
    let pick = |text: &str, on: bool| {
        if on {
            Span::raw(text.to_string()).bold().fg(palette::FOCUS)
        } else {
            Span::raw(text.to_string()).dim()
        }
    };
    let line = Line::from(vec![
        pick("reply", !editing),
        Span::raw("   "),
        pick("edit", editing),
    ]);
    let width = 18;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(3) / 2;
    let popup = Rect {
        x,
        y,
        width: width.min(area.width),
        height: 3.min(area.height),
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(line).block(pane(Span::raw(" note "), true)),
        popup,
    );
}

/// The row under the cursor: a background under its colours,
/// never in place of them.
const CHOSEN: Style = Style::new().bg(palette::SHADE);

/// The about pane's width, borders included, when it stands
/// beside the posts.
const ABOUT_WIDTH: u16 = 30;

/// A terminal narrower than this stacks the about pane under the
/// header instead.
const WIDE: u16 = 90;

/// Draw an issue's page: the header card, the about pane and the
/// posts, side by side or stacked.
#[allow(clippy::too_many_arguments)]
fn draw_detail(
    frame: &mut Frame,
    header: &Header,
    view: View,
    db: &Db,
    issue: &Issue,
    page: &mut Page,
    actor: &str,
    enhanced: bool,
    notice: Option<&str>,
) -> Limits {
    let [top, body, foot] = frame_areas(frame);
    draw_header(frame, top, header, view, &trail_spans(view, &page.trail()));
    let wide = body.width >= WIDE;
    let left_w = if wide {
        body.width - ABOUT_WIDTH
    } else {
        body.width
    };
    let about_w = if wide { ABOUT_WIDTH } else { body.width };
    let heading = heading(issue, inner(left_w));
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
        Paragraph::new(heading).block(pane(
            Span::raw(format!(" {} ", issue.id)).fg(palette::REF),
            false,
        )),
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

    let (lines, boxes, editor) = posts(
        issue,
        inner(posts_area.width),
        page,
        page.tie.is_none(),
        actor,
    );
    let rows = rows_in(posts_area.height);
    let max = lines.len().saturating_sub(rows);
    // a note just written is where the cursor goes
    if let Some(id) = page.select.take()
        && let Some(k) = boxes.iter().position(|b| b.id.as_deref() == Some(&id))
    {
        page.go(k);
    }
    page.cursor = page.cursor.min(boxes.len().saturating_sub(1));
    if page.reveal {
        reveal(&mut page.scroll, page.cursor, &boxes, rows);
        page.reveal = false;
    }
    // an open compose box is always in view
    if let Some((range, _)) = &editor {
        reveal_lines(&mut page.scroll, range, rows);
    }
    page.scroll = page.scroll.min(max);
    let pane_title = match page.root {
        Root::Issue | Root::Note(_) => " notes ",
        Root::Body => " body ",
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(Span::raw(pane_title), page.tie.is_none()))
            .scroll((u16::try_from(page.scroll).unwrap_or(u16::MAX), 0)),
        posts_area,
    );
    // the terminal's cursor sits in the compose box
    if let Some((_, (line, col))) = editor
        && line >= page.scroll
        && line < page.scroll + rows
    {
        frame.set_cursor_position(Position {
            x: posts_area.x + 2 + u16::try_from(col).unwrap_or(u16::MAX),
            y: posts_area.y + 1 + u16::try_from(line - page.scroll).unwrap_or(u16::MAX),
        });
    }
    if let Some(choice) = &page.ask {
        draw_ask(frame, posts_area, choice);
    }

    let ties = if about.ties.is_empty() {
        ""
    } else {
        "tab ties   "
    };
    let keys = if let Some(notice) = notice {
        notice.to_string()
    } else if page.open {
        let newline = if enhanced {
            "shift+enter newline"
        } else {
            "alt+enter newline"
        };
        format!("enter send   {newline}   esc keep the draft")
    } else if page.ask.is_some() {
        "h/l choose   enter go   esc close".to_string()
    } else if page.tie.is_some() {
        "j/k pick   enter follow   tab back   esc back   q quit".to_string()
    } else if matches!(page.root, Root::Note(_)) {
        format!(
            "h/l select   enter reply or edit   j/k scroll   ^d/^u page   {ties}esc back   q quit"
        )
    } else {
        format!("j/k move   enter open   n note   ^d/^u scroll   {ties}esc back   q quit")
    };
    let keys = keys.as_str();
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
        (Style::new(), title.bold().fg(palette::FOCUS))
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
            .fg(palette::INK)
            .add_modifier(Modifier::BOLD),
    )
}

fn priority_badge(p: u8) -> Span<'static> {
    let bg = match p {
        0 => palette::ALARM,
        1 => palette::ALARM_SOFT,
        2 => palette::QUIET,
        _ => palette::SHADE,
    };
    badge(&format!("p{p}"), bg)
}

fn status_colour(s: Status) -> Color {
    match s {
        Status::Open => palette::OPEN,
        Status::InProgress => palette::CLAIMED,
        Status::Deferred => palette::DEFERRED,
        Status::Closed => palette::SHADE,
    }
}

fn status_badge(s: Status) -> Span<'static> {
    badge(status_name(s), status_colour(s))
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
        let mut spans = vec![
            Span::raw("› ").dim(),
            Span::raw(id.to_string()).fg(palette::REF),
        ];
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

/// The boxes of a page, each `width` wide, with the box under the
/// page's cursor drawn in the focus colour when `focused`: on the
/// issue's page the body and the top-level notes, each cut to a few
/// lines, a note's border saying what its thread holds; on the
/// body's page the body whole; on a note's page the note whole, then
/// every reply under it whole.
#[allow(clippy::type_complexity)]
fn posts(
    i: &Issue,
    width: usize,
    page: &Page,
    focused: bool,
    actor: &str,
) -> (
    Vec<Line<'static>>,
    Vec<NoteBox>,
    Option<(Range<usize>, (usize, usize))>,
) {
    let style = human();
    let wide = width.max(20);
    let threaded = crate::model::threaded(&i.notes);
    // what a note's thread holds: how many replies, and when the
    // last was written
    let thread = |k: usize| -> Option<(usize, Timestamp)> {
        let depth = threaded[k].0;
        let replies: Vec<&Note> = threaded[k + 1..]
            .iter()
            .take_while(|(d, _)| *d > depth)
            .map(|(_, n)| *n)
            .collect();
        let last = replies.iter().map(|n| n.at).max()?;
        Some((replies.len(), last))
    };
    // each box as a post, its depth, and its id
    let mut shown: Vec<(Post, usize, Option<String>)> = Vec::new();
    match &page.root {
        Root::Issue => {
            if !i.body.is_empty() {
                shown.push((body_post(i, Some(BOX_LINES)), 0, None));
            }
            for (k, (depth, n)) in threaded.iter().enumerate() {
                if *depth == 0 {
                    shown.push((
                        note_post(n, thread(k), Some(BOX_LINES)),
                        0,
                        Some(n.id.clone()),
                    ));
                }
            }
        }
        Root::Body => shown.push((body_post(i, None), 0, None)),
        Root::Note(id) => {
            if let Some(k) = threaded.iter().position(|(_, n)| n.id == *id) {
                let (depth, root) = threaded[k];
                shown.push((note_post(root, None, None), 0, Some(root.id.clone())));
                for (d, n) in threaded[k + 1..].iter().take_while(|(d, _)| *d > depth) {
                    shown.push((note_post(n, None, None), d - depth, Some(n.id.clone())));
                }
            }
        }
    }
    let cursor = page.cursor.min(shown.len().saturating_sub(1));
    let mut out = Vec::new();
    let mut boxes = Vec::new();
    // the compose box goes where the note will: an edit in place of
    // the note, a reply after the last reply under it, a new note at
    // the end
    let draft = page.open.then_some(page.draft.as_ref()).flatten();
    let mut editor_at = None;
    let mut shown: Vec<(Post, usize, Option<String>)> = shown;
    if let Some(ed) = draft {
        let at = match &ed.target {
            Target::Note => Some((shown.len(), 0)),
            Target::Reply(to) => shown
                .iter()
                .position(|(_, _, id)| id.as_deref() == Some(to))
                .map(|k| {
                    let depth = shown[k].1;
                    let end = k
                        + 1
                        + shown[k + 1..]
                            .iter()
                            .take_while(|(_, d, _)| *d > depth)
                            .count();
                    (end, depth + 1)
                }),
            Target::Edit(id) => shown
                .iter()
                .position(|(_, _, x)| x.as_deref() == Some(id))
                .map(|k| (k, shown[k].1)),
        };
        if let Some((k, depth)) = at {
            editor_at = Some((k, depth));
            if matches!(ed.target, Target::Edit(_)) {
                shown.remove(k);
            }
        }
    }
    let mut editor = None;
    // a reply hangs from a rail in its own column, down from what
    // it answers; the rail runs on past earlier replies while a
    // later one at that depth is still to come under the same note
    // the compose box counts as a box for the rails, so one runs
    // down to it past what is between
    let mut depths: Vec<usize> = shown.iter().map(|(_, d, _)| *d).collect();
    if let Some((at, d)) = editor_at {
        depths.insert(at, d);
    }
    let pos = |k: usize| match editor_at {
        Some((at, _)) if at <= k => k + 1,
        _ => k,
    };
    let rail_at = |from: usize, level: usize| {
        level > 0
            && depths[from..]
                .iter()
                .take_while(|d| **d >= level)
                .any(|d| *d == level)
    };
    let prefix = |from: usize, depth: usize| -> String {
        (0..depth)
            .map(|l| if rail_at(from, l) { "│ " } else { "  " })
            .collect()
    };
    let deepest = wide.saturating_sub(20) / 2;
    let count = shown.len();
    for (k, (post, depth, id)) in shown.into_iter().enumerate() {
        // the compose box, before the box it stands in for or at
        // the end
        if let (Some(ed), Some((at, d))) = (draft, editor_at)
            && at == k
        {
            {
                let d = d.min(deepest);
                if !boxes.is_empty() || !out.is_empty() {
                    let mut above = prefix(k, d);
                    if d > 0 {
                        above.push('│');
                    }
                    out.push(Line::from(Span::raw(above).dim()));
                }
                let start = out.len();
                let (mut lines, (row, col)) = editor_box(ed, actor, wide - 2 * d);
                for line in &mut lines {
                    line.spans.insert(0, Span::raw(prefix(k + 1, d)).dim());
                }
                out.extend(lines);
                editor = Some((start..out.len(), (start + 1 + row, 2 * d + 2 + col)));
            }
        }
        let depth = depth.min(deepest);
        if !out.is_empty() {
            // the line above a reply ends in its own rail
            let mut above = prefix(pos(k), depth);
            if depth > 0 {
                above.push('│');
            }
            out.push(Line::from(Span::raw(above).dim()));
        }
        let start = out.len();
        let mut lines = post_box(
            &post,
            &style,
            wide - 2 * depth,
            focused && cursor == boxes.len() && draft.is_none(),
        );
        for line in &mut lines {
            line.spans
                .insert(0, Span::raw(prefix(pos(k) + 1, depth)).dim());
        }
        out.extend(lines);
        boxes.push(NoteBox {
            id,
            lines: start..out.len(),
        });
    }
    // a new note, or a reply to the last note, goes after everything
    if let (Some(ed), Some((at, d))) = (draft, editor_at)
        && at == count
    {
        {
            let d = d.min(deepest);
            if !out.is_empty() {
                let mut above = prefix(count, d);
                if d > 0 {
                    above.push('│');
                }
                out.push(Line::from(Span::raw(above).dim()));
            }
            let start = out.len();
            let (mut lines, (row, col)) = editor_box(ed, actor, wide - 2 * d);
            for line in &mut lines {
                line.spans.insert(0, Span::raw(prefix(count + 1, d)).dim());
            }
            out.extend(lines);
            editor = Some((start..out.len(), (start + 1 + row, 2 * d + 2 + col)));
        }
    }
    (out, boxes, editor)
}

/// The compose box: who is writing and what it will be in the top
/// border, the text as typed inside, and where the cursor is among
/// its rows.
fn editor_box(ed: &Editor, actor: &str, width: usize) -> (Vec<Line<'static>>, (usize, usize)) {
    let border = Style::new().fg(palette::FOCUS);
    let edge = |s: String| Span::styled(s, border);
    let what = match &ed.target {
        Target::Note => "a note".to_string(),
        Target::Reply(to) => format!("reply to {to}"),
        Target::Edit(id) => format!("editing {id}"),
    };
    let who = Span::styled(
        actor.to_string(),
        self::actor(actor).add_modifier(Modifier::BOLD),
    );
    let used = 2 + actor.chars().count() + 3 + what.chars().count() + 1 + 1;
    let mut out = vec![Line::from(vec![
        edge("╭ ".into()),
        who,
        edge(format!(" · {what} ")),
        edge("─".repeat(width.saturating_sub(used))),
        edge("╮".into()),
    ])];
    let inner = width.saturating_sub(4).max(16);
    let (rows, at) = ed.wrapped(inner);
    for r in rows {
        out.push(Line::from(vec![
            edge("│ ".into()),
            Span::raw(pad(&r, inner)),
            edge(" │".into()),
        ]));
    }
    out.push(Line::from(vec![
        edge("╰".into()),
        edge("─".repeat(width.saturating_sub(2))),
        edge("╯".into()),
    ]));
    (out, at)
}

/// The body as the issue's own first post.
fn body_post(i: &Issue, limit: Option<usize>) -> Post<'_> {
    Post {
        id: None,
        author: None,
        edited: None,
        at: i.created_at,
        text: &i.body,
        branch: &i.stamps.created.branch,
        commit: &i.stamps.created.commit,
        thread: None,
        flag: None,
        limit,
    }
}

fn note_post<'a>(
    n: &'a Note,
    thread: Option<(usize, Timestamp)>,
    limit: Option<usize>,
) -> Post<'a> {
    Post {
        id: Some(&n.id),
        author: Some((&n.author, actor(&n.author))),
        edited: n.edited_at,
        at: n.at,
        text: &n.text,
        branch: &n.branch,
        commit: &n.commit,
        thread,
        flag: None,
        limit,
    }
}

/// What a box on the page shows: a note, or the body as the
/// issue's own first post.
struct Post<'a> {
    id: Option<&'a str>,
    /// Who wrote it, in their colour; `None` for the body.
    author: Option<(&'a str, Style)>,
    edited: Option<Timestamp>,
    at: Timestamp,
    text: &'a str,
    branch: &'a str,
    commit: &'a str,
    /// How many replies its thread holds and when the last was
    /// written, on the issue's page.
    thread: Option<(usize, Timestamp)>,
    /// A warning for the top border.
    flag: Option<String>,
    /// How many lines of the text to show; the rest is counted in
    /// the bottom border.
    limit: Option<usize>,
}

/// A post as a rounded box `width` columns wide: who and when in
/// the top border, the text inside, where it was written in the
/// bottom border. The box under the cursor is `chosen`, in the
/// accent colour; the rest are dim.
fn post_box(p: &Post, style: &render::Style, width: usize, chosen: bool) -> Vec<Line<'static>> {
    let border = if chosen {
        Style::new().fg(palette::FOCUS)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };
    let edge = |s: String| Span::styled(s, border);
    let mut top = vec![edge("╭ ".into())];
    let mut used = 2 + 1;
    if let Some((author, style)) = p.author {
        let who = style.add_modifier(Modifier::BOLD);
        top.push(Span::styled(author.to_string(), who));
        top.push(edge(" · ".into()));
        used += author.chars().count() + 3;
    }
    let when = format!("{} ", style.when(p.at));
    used += when.chars().count();
    top.push(edge(when));
    if let Some(flag) = &p.flag {
        let flag = format!("· {flag} ");
        used += flag.chars().count();
        top.push(Span::raw(flag).fg(palette::ALARM));
    }
    top.push(edge("─".repeat(width.saturating_sub(used))));
    top.push(edge("╮".into()));
    let mut out = vec![Line::from(top)];
    let inner = width.saturating_sub(4).max(16);
    let mut lines = md::render(p.text, inner);
    let more = match p.limit {
        Some(limit) if lines.len() > limit => {
            let more = lines.len() - limit;
            lines.truncate(limit);
            more
        }
        _ => 0,
    };
    for l in lines {
        let mut spans = vec![edge("│ ".into())];
        spans.extend(l.spans);
        spans.push(edge(" │".into()));
        out.push(Line::from(spans));
    }
    // what enter would show, in its colour, under the last line
    if more > 0 {
        out.push(Line::from(vec![
            edge("│ ".into()),
            Span::styled(
                pad(&format!("⋯ {more} more"), inner),
                Style::new().fg(palette::FOCUS),
            ),
            edge(" │".into()),
        ]));
    }
    let mut from = String::new();
    if let Some((replies, last)) = p.thread {
        let s = if replies == 1 { "reply" } else { "replies" };
        from.push_str(&format!("▸ {replies} {s} · {} · ", style.when(last)));
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

/// A commit message as spans: the verb bold in its colour, each
/// issue id cyan, and whoever it names after `by` in their colour.
fn log_spans(msg: &str) -> Vec<Span<'_>> {
    let (head, who) = match msg.split_once(" by ") {
        Some((head, who)) => (head, Some(who)),
        None => (msg, None),
    };
    let mut spans = Vec::new();
    for (n, word) in head.split(' ').enumerate() {
        if n > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(if n == 0 {
            Span::styled(word, verb_style(word))
        } else if is_id(word) {
            Span::raw(word).fg(palette::REF)
        } else {
            Span::raw(word)
        });
    }
    if let Some(who) = who {
        spans.push(Span::raw(" by ").dim());
        spans.push(Span::styled(who, actor(who)));
    }
    spans
}

/// A commit's verb, bold, in the colour of what it does to the
/// issue; a verb that changes no status is bold alone.
fn verb_style(verb: &str) -> Style {
    let colour = match verb {
        "create" | "reopen" | "unclaim" => Some(palette::OPEN),
        "claim" => Some(palette::CLAIMED),
        "remember" | "forget" => Some(palette::AGENT),
        _ => None,
    };
    let style = Style::new().add_modifier(Modifier::BOLD);
    colour.map_or(style, |c| style.fg(c))
}

/// Whether a word is shaped like an issue id: a prefix, a dash,
/// and six hex digits, with trailing punctuation allowed.
fn is_id(word: &str) -> bool {
    let word = word.trim_end_matches([',', ':', '.', ';']);
    let Some((prefix, hex)) = word.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty() && hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())
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
    chosen: bool,
) -> Line<'a> {
    let edge = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::DIM));
    let inside = |spans: Vec<Span<'a>>| -> Line<'a> {
        if !boxed {
            return Line::from(spans);
        }
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let mut out = vec![
            edge("│ ".into()),
            Span::raw(if chosen { "› " } else { "  " }),
        ];
        out.extend(spans);
        // the padding is its own span, so a shade over the inside
        // of the box reaches its right edge
        out.push(Span::raw(" ".repeat(width.saturating_sub(6 + used))));
        out.push(edge(" │".into()));
        Line::from(out)
    };
    match row {
        // a section is a pane: its title bright when the cursor is
        // in it, dim otherwise
        Row::Header(h) if boxed => Line::from(vec![
            edge("╭ ".into()),
            if chosen {
                Span::raw(*h).bold().fg(palette::FOCUS)
            } else {
                Span::raw(*h).dim()
            },
            edge(format!(
                " {}╮",
                "─".repeat(width.saturating_sub(h.chars().count() + 4))
            )),
        ]),
        Row::Header(h) => Line::from(Span::raw(*h).bold()),
        Row::End => Line::from(edge(format!("╰{}╯", "─".repeat(width.saturating_sub(2))))),
        Row::Note(n) => inside(vec![Span::raw(*n).dim()]),
        // the memories are drawn as boxes, never as rows
        Row::Memory { .. } => Line::default(),
        Row::Log { oid, when, msg } => {
            let mut spans = vec![
                Span::raw(oid.as_str()).dim(),
                Span::raw("  "),
                Span::raw(when.as_str()).dim(),
                Span::raw("  "),
            ];
            spans.extend(log_spans(msg));
            inside(spans)
        }
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
                Span::raw(issue.id.as_str()).fg(palette::REF),
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
                width.saturating_sub(6)
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
        0 => Style::new().fg(palette::ALARM).add_modifier(Modifier::BOLD),
        1 => Style::new().fg(palette::ALARM),
        4 => Style::new().add_modifier(Modifier::DIM),
        _ => Style::new(),
    };
    Span::styled(format!("p{p}"), style)
}

fn status(s: Status, width: usize) -> Span<'static> {
    let style = match s {
        Status::Open => Style::new().fg(palette::OPEN),
        Status::InProgress => Style::new().fg(palette::CLAIMED),
        Status::Deferred => Style::new().fg(palette::DEFERRED),
        Status::Closed => Style::new().add_modifier(Modifier::DIM),
    };
    Span::styled(pad(status_name(s), width), style)
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
        let child = line(&rows[2], &widths, 80, Columns::ALL, false, false);
        assert_eq!(child.spans[0].content, "  └─ ");
        assert!(child.spans[0].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(child.spans[1].content, "t-000002");
        assert_eq!(child.spans[1].style.fg, Some(Color::Cyan));
        let held = line(&rows[0], &widths, 80, Columns::ALL, false, false);
        let last = held.spans.last().unwrap();
        assert_eq!(last.content, "t-000002");
        assert_eq!(last.style.fg, Some(Color::Cyan));
        assert_eq!(
            line(&rows[1], &widths, 80, Columns::ALL, false, false)
                .to_string()
                .trim_end(),
            "t-000001       p2  open  milestone  0/1  t"
        );
        // a narrow terminal drops the kind, then the status
        assert_eq!(
            line(&rows[1], &widths, 70, Columns::at(70), false, false)
                .to_string()
                .trim_end(),
            "t-000001       p2  open  0/1  t"
        );
        assert_eq!(
            line(&rows[1], &widths, 50, Columns::at(50), false, false)
                .to_string()
                .trim_end(),
            "t-000001       p2  0/1  t"
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
        assert_eq!(headers, ["milestones", "claimed", "ready", "blocked"]);
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
            .map(|r| line(r, &widths, 40, Columns::at(34), true, false).to_string())
            .collect();
        assert!(lines.iter().all(|l| l.chars().count() == 40), "{lines:#?}");
        assert!(lines[0].starts_with("╭ milestones ─"), "{}", lines[0]);
        assert!(lines[0].ends_with("─╮"), "{}", lines[0]);
        assert!(lines[1].starts_with("│   t-000001  p2  "), "{}", lines[1]);
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
        assert!(matches!(rows[3], Row::Header("ready")));
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

    fn no_compose() -> Compose<'static> {
        Compose {
            form: None,
            actor: "ann",
            enhanced: true,
            notice: None,
        }
    }

    fn test_header() -> Header {
        Header {
            prefix: "t".into(),
            stamp: "dev @ abc1234".into(),
            stats: vec![badge("1 open", Color::Green)],
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
            recent: vec![Row::Log {
                oid: "abc1234".into(),
                when: "just now".into(),
                msg: "Close t-000003".into(),
            }],
        };
        let render = |width: u16, height: u16, preview: bool| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = ListState::default();
            // the second row is the issue with the body
            state.select(Some(1));
            terminal
                .draw(|f| draw_rows(f, &screen, &mut state, &db, preview, &[], no_compose()))
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
        assert!(at(&wide, "│  p2   task   open ").is_some(), "{wide:#?}");
        assert!(at(&wide, "│ one paragraph").is_some(), "{wide:#?}");
        // the rows keep their columns at the narrower width, inside
        // the list's pane
        assert!(wide[3].starts_with("╭ list "), "{}", wide[3]);
        assert!(wide[4].starts_with("│   t-000001"), "{}", wide[4]);
        // the recent commits take the lower third: 8 of 26 rows
        assert!(wide[21].starts_with("╭ recent "), "{}", wide[21]);
        assert!(
            wide[22].starts_with("│ abc1234  just now  Close t-000003"),
            "{}",
            wide[22]
        );
        // under: the pane takes the lower half of what is above
        // the recent commits
        let narrow = render(70, 30, true);
        assert!(narrow[12].starts_with("╭ t-000002 "), "{}", narrow[12]);
        assert!(narrow[21].starts_with("╭ recent "), "{}", narrow[21]);
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
        let text = line(&rows[0], &widths, 80, Columns::ALL, false, false).to_string();
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
        let lines = header_lines(&h, View::Board, &[Span::raw("mine")], 90);
        let text = strings(&lines);
        assert!(
            text[0].starts_with(" t   [ list │ board │ memories ]"),
            "{}",
            text[0]
        );
        assert!(text[0].ends_with("dev @ abc1234"), "{}", text[0]);
        assert!(text[1].starts_with("mine  "), "{}", text[1]);
        let stats = [
            "\u{e0b2} ○ 1 open ",
            "\u{e0b2} ◐ 1 claimed ",
            "\u{e0b2} ◌ 0 deferred ",
            "\u{e0b2} ● 0 closed ",
            "\u{e0b2} ✦ 0 memories ",
        ]
        .concat();
        assert!(text[1].ends_with(&stats), "{}", text[1]);
        assert!(
            text[..2].iter().all(|t| t.chars().count() == 90),
            "{text:#?}"
        );
        assert_eq!(text[2], "─".repeat(90));
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Cyan));
        let board = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "board")
            .unwrap();
        assert!(board.style.add_modifier.contains(Modifier::BOLD));
        let list = lines[0].spans.iter().find(|s| s.content == "list").unwrap();
        assert!(list.style.add_modifier.contains(Modifier::DIM));
        // each arrow is the badge after it over the badge before it
        assert_eq!(lines[1].spans[2].style.fg, Some(Color::Green));
        assert_eq!(lines[1].spans[2].style.bg, None);
        assert_eq!(lines[1].spans[3].style.bg, Some(Color::Green));
        assert_eq!(lines[1].spans[4].style.fg, Some(Color::Yellow));
        assert_eq!(lines[1].spans[4].style.bg, Some(Color::Green));
        assert_eq!(lines[1].spans[5].style.bg, Some(Color::Yellow));
        // on a page the context is the trail from the view
        let trail = trail_spans(View::List, &["t-000001".into(), "t-000002".into()]);
        assert_eq!(Line::from(trail).to_string(), "list › t-000001 › t-000002");
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
        assert_eq!(lines[0], " p2   task   open ");
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
        let lines = strings(&posts(&i, 40, &Page::open(i.id.clone()), true, "ann").0);
        let wide = |l: &String| l.chars().count() == 40;
        assert!(
            lines.iter().filter(|l| !l.is_empty()).all(wide),
            "{lines:#?}"
        );
        // the body carries the creation stamp and no author
        assert!(lines[0].starts_with("╭ just now ─"), "{}", lines[0]);
        assert_eq!(lines[1], format!("│ {:<36} │", "one paragraph"));
        assert_eq!(lines[2], format!("│ {:<36} │", ""));
        assert_eq!(lines[3], format!("│ {:<36} │", "another"));
        assert!(lines[4].ends_with(" main @ none ─╯"), "{}", lines[4]);
        assert_eq!(lines[5], "");
        assert!(lines[6].starts_with("╭ ann · just now ─"), "{}", lines[6]);
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
        assert!(page.prune(&db));
        assert_eq!(page.id(), "t-000002");
        page.issue = "t-gone".into();
        assert!(!page.prune(&db));
        // back lands where the cursor was
        let mut page = Page::open("t-000002".into());
        page.go(3);
        page.scroll = 2;
        page.follow("t-000001".into());
        assert_eq!((page.cursor, page.scroll), (0, 0));
        assert!(page.back());
        assert_eq!((page.cursor, page.scroll, page.reveal), (3, 2, true));
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
                    draw_detail(f, &title, View::List, &db, &i, &mut page, "ann", true, None);
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
        assert!(
            wide[0].starts_with(" t   [ list │ board │ memories ]"),
            "{}",
            wide[0]
        );
        assert!(wide[0].ends_with("dev @ abc1234"), "{}", wide[0]);
        assert!(wide[1].starts_with("list › t-000002"), "{}", wide[1]);
        assert!(wide[1].ends_with(" 1 open "), "{}", wide[1]);
        assert!(wide[2].starts_with("────"), "{}", wide[2]);
        assert!(wide[3].starts_with("╭ t-000002 "), "{}", wide[3]);
        assert_eq!(col_of(&wide[3], "╭ about "), Some(70), "{}", wide[3]);
        assert!(wide[4].contains("││  p2   task   open "), "{}", wide[4]);
        let posts = wide.iter().position(|r| r.starts_with("╭ notes ")).unwrap();
        let body = &wide[posts + 1];
        assert!(body.starts_with("│ ╭ just now "), "{body}");
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
            Mode::Rows => panic!("a page"),
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
        key(&mut st, KeyCode::Char('g'));
        key(&mut st, KeyCode::Tab);
        key(&mut st, KeyCode::Char('j'));
        key(&mut st, KeyCode::Char('j'));
        assert_eq!(page(&st).3, Some(1));
        key(&mut st, KeyCode::Enter);
        assert_eq!(page(&st), ("t-000004".into(), 0, 0, None));
        key(&mut st, KeyCode::Esc);
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
    fn the_issue_page_shows_the_top_level_notes_with_their_threads_in_the_border() {
        let i = thread();
        let page = Page::open(i.id.clone());
        let (lines, boxes, _) = posts(&i, 44, &page, true, "ann");
        assert_eq!(box_ids(&boxes), [None, Some("aaaa0000"), Some("dddd0000")]);
        // each box's lines are its own, in order, with a blank line
        // between
        for pair in boxes.windows(2) {
            assert_eq!(pair[0].lines.end + 1, pair[1].lines.start);
        }
        assert_eq!(boxes[2].lines.end, lines.len());
        let bottom = |k: usize| lines[boxes[k].lines.end - 1].to_string();
        assert!(
            bottom(1).contains("▸ 2 replies · just now · aaaa0000"),
            "{}",
            bottom(1)
        );
        assert!(!bottom(2).contains("repl"), "{}", bottom(2));
    }

    #[test]
    fn the_chosen_box_is_drawn_plain_and_scrolled_into_view() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let i = thread();
        let mut page = Page::open(i.id.clone());
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        let plain = |l: &Line| {
            let corner = l.spans.iter().find(|s| s.content.contains('╭')).unwrap();
            corner.style.fg == Some(palette::FOCUS)
        };
        assert!(plain(&lines[boxes[0].lines.start]));
        assert!(!plain(&lines[boxes[1].lines.start]));
        page.go(1);
        let (lines, _, _) = posts(&i, 40, &page, true, "ann");
        assert!(!plain(&lines[boxes[0].lines.start]));
        assert!(plain(&lines[boxes[1].lines.start]));
        // on the ties, no box is chosen
        let (lines, _, _) = posts(&i, 40, &page, false, "ann");
        assert!(!plain(&lines[boxes[1].lines.start]));

        let (db, _) = fixture();
        let title = test_header();
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        let mut draw = |page: &mut Page| {
            let mut limits = Limits::default();
            terminal
                .draw(|f| {
                    limits = draw_detail(f, &title, View::List, &db, &i, page, "ann", true, None)
                })
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
    fn tab_reaches_the_memories_and_the_cursor_walks_them() {
        let db = with_memories();
        let rows = memory_rows(&db, &Filter::default(), distance);
        let mut st = State::new();
        let note_box = |id: &str| NoteBox {
            id: Some(id.into()),
            lines: 0..1,
        };
        let limits = Limits {
            scroll: 3,
            rows: 4,
            boxes: vec![note_box("prefix"), note_box("review")],
            ties: Vec::new(),
        };
        let key = |st: &mut State, code| handle(st, press(code), &rows, &db, &limits);
        key(&mut st, KeyCode::Tab);
        key(&mut st, KeyCode::Tab);
        assert_eq!(st.view, View::Memories);
        assert_eq!(key(&mut st, KeyCode::Tab), Action::Rebuild(After::First));
        assert_eq!(st.view, View::List);
        key(&mut st, KeyCode::BackTab);
        assert_eq!(st.view, View::Memories);
        // the issue filters and Enter do nothing here
        assert_eq!(key(&mut st, KeyCode::Char('s')), Action::Draw);
        assert!(st.filter.status.is_none());
        key(&mut st, KeyCode::Enter);
        assert!(matches!(st.mode, Mode::Rows));
        st.shelf.reveal = false;
        key(&mut st, KeyCode::Char('j'));
        assert_eq!((st.shelf.cursor, st.shelf.reveal), (1, true));
        key(&mut st, KeyCode::Char('j'));
        assert_eq!(st.shelf.cursor, 1);
        key(&mut st, KeyCode::Char('g'));
        assert_eq!(st.shelf.cursor, 0);
        key(&mut st, KeyCode::Char('G'));
        assert_eq!(st.shelf.cursor, 1);
        handle(&mut st, ctrl('d'), &rows, &db, &limits);
        assert_eq!(st.shelf.scroll, 2);
        handle(&mut st, ctrl('u'), &rows, &db, &limits);
        assert_eq!(st.shelf.scroll, 0);
        assert_eq!(
            key(&mut st, KeyCode::Char('/')),
            Action::Rebuild(After::Kept)
        );
        assert!(st.typing);
        key(&mut st, KeyCode::Esc);
        assert_eq!(st.filter.describe(View::Memories), "");
        st.filter.closed = true;
        st.filter.text = "x".into();
        assert_eq!(st.filter.describe(View::Memories), "/x");
    }

    #[test]
    fn the_memories_are_boxes_beside_an_about_pane() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let db = with_memories();
        let rows = memory_rows(&db, &Filter::default(), distance);
        let screen = Screen {
            view: View::Memories,
            header: test_header(),
            widths: widths(&rows),
            rows,
            recent: Vec::new(),
        };
        let mut shelf = Shelf::default();
        shelf.go(1);
        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        let mut limits = Limits::default();
        terminal
            .draw(|f| limits = draw_memories(f, &screen, &mut shelf, &[Span::raw("/x")]))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        assert!(
            rows[0].starts_with(" t   [ list │ board │ memories ]"),
            "{}",
            rows[0]
        );
        assert!(rows[0].ends_with("dev @ abc1234"), "{}", rows[0]);
        assert!(rows[1].starts_with("/x"), "{}", rows[1]);
        assert!(rows[1].ends_with(" 1 open "), "{}", rows[1]);
        assert!(rows[3].starts_with("╭ memories "), "{}", rows[3]);
        assert!(rows[3].contains("╭ about "), "{}", rows[3]);
        assert!(rows[4].starts_with("│ ╭ prefix · just now "), "{}", rows[4]);
        assert!(rows[4].contains("│ review "), "{}", rows[4]);
        assert!(rows[5].contains("│ ids use the prefix t "), "{}", rows[5]);
        let review = rows
            .iter()
            .position(|r| r.contains("╭ review · just now · on dev, may no longer hold "))
            .unwrap();
        assert_eq!(limits.boxes.len(), 2);
        assert_eq!(limits.boxes[1].lines.start + 4, review);
        // the chosen box is cyan, the other dim, and the flag yellow
        let at = rows[review].find("╭ review").unwrap();
        let x = u16::try_from(rows[review][..at].chars().count()).unwrap();
        assert_eq!(buffer[(2, 4)].fg, Color::Reset);
        assert_eq!(
            buffer[(x, u16::try_from(review).unwrap())].fg,
            palette::FOCUS
        );
        let flag = rows[review].find("on dev, may").unwrap();
        let x = u16::try_from(rows[review][..flag].chars().count()).unwrap();
        assert_eq!(
            buffer[(x, u16::try_from(review).unwrap())].fg,
            palette::ALARM
        );
        let about = rows
            .iter()
            .find(|r| r.contains("on dev: may no longer hold"))
            .unwrap();
        assert!(about.contains("││ "), "{about}");
    }

    /// A board with far more rows than a screen holds: twenty
    /// milestones, forty ready tasks, twenty blocked ones.
    fn crowded() -> Db {
        let mut issues = Vec::new();
        for n in 0..20 {
            let mut m = test_issue(&format!("t-m{n:05}"));
            m.kind = Kind::Milestone;
            m.title = format!("milestone {n}");
            issues.push(m);
        }
        for n in 0..40 {
            let mut i = test_issue(&format!("t-r{n:05}"));
            i.title = format!("ready {n}");
            issues.push(i);
        }
        for n in 0..20 {
            let mut i = test_issue(&format!("t-b{n:05}"));
            i.title = format!("blocked {n}");
            i.blocked_by = vec!["t-r00000".into()];
            issues.push(i);
        }
        db(issues)
    }

    #[test]
    fn the_board_keeps_the_chosen_row_in_view_all_the_way_down_and_back() {
        for (width, height, preview) in [(100, 30, false), (70, 24, true), (100, 12, true)] {
            board_walk(width, height, preview);
        }
    }

    fn board_walk(width: u16, height: u16, preview: bool) {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let db = crowded();
        let rows = board_rows(&db, Timestamp::now());
        let screen = Screen {
            view: View::Board,
            header: test_header(),
            widths: widths(&rows),
            rows,
            recent: Vec::new(),
        };
        let mut st = State::new();
        st.view = View::Board;
        st.preview = preview;
        st.list.select(step(&screen.rows, None, 1));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let limits = Limits::default();
        let mut shown = |st: &mut State| -> (String, ratatui::buffer::Buffer) {
            let (list, preview) = (&mut st.list, st.preview);
            terminal
                .draw(|f| draw_rows(f, &screen, list, &db, preview, &[], no_compose()))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let text = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                        + "\n"
                })
                .collect();
            (text, buffer)
        };
        // press until the cursor stops, checking it stays in view
        let mut walk = |st: &mut State, code: KeyCode| -> String {
            loop {
                let before = st.list.selected();
                handle(st, press(code), &screen.rows, &db, &limits);
                let chosen = match &screen.rows[st.list.selected().unwrap()] {
                    Row::Issue { issue, .. } => issue.id.clone(),
                    _ => panic!("an issue row"),
                };
                let (text, buffer) = shown(st);
                assert!(
                    text.contains(&format!("│ › {chosen}")),
                    "{chosen} off screen:\n{text}"
                );
                // the shade stays inside the box: not under the
                // gutter or the edges, under the row's inside and
                // its padding
                let y = text.lines().position(|l| l.starts_with("│ › ")).unwrap();
                let y = u16::try_from(y).unwrap();
                // the box's own right edge: the second │ on the row
                let row = text.lines().nth(usize::from(y)).unwrap();
                let right = row.chars().position(|c| c == '│').unwrap() + 1;
                let right = right + row.chars().skip(right).position(|c| c == '│').unwrap();
                let right = u16::try_from(right).unwrap();
                assert_ne!(buffer[(0, y)].bg, palette::SHADE);
                assert_ne!(buffer[(1, y)].bg, palette::SHADE);
                assert_eq!(buffer[(2, y)].bg, palette::SHADE);
                assert_eq!(buffer[(4, y)].bg, palette::SHADE);
                assert_eq!(buffer[(right - 2, y)].bg, palette::SHADE);
                assert_ne!(buffer[(right, y)].bg, palette::SHADE);
                if st.list.selected() == before {
                    return text;
                }
            }
        };
        // at either end the section's border row shows too
        let text = walk(&mut st, KeyCode::Char('j'));
        assert_eq!(st.list.selected(), Some(screen.rows.len() - 2));
        assert!(text.contains("\n╰─"), "the last box's bottom:\n{text}");
        let text = walk(&mut st, KeyCode::Char('k'));
        assert_eq!(st.list.selected(), Some(1));
        assert!(
            text.contains("╭ milestones "),
            "the first box's top:\n{text}"
        );
        let text = walk(&mut st, KeyCode::Char('G'));
        assert_eq!(st.list.selected(), Some(screen.rows.len() - 2));
        assert!(text.contains("\n╰─"), "the last box's bottom:\n{text}");
        let text = walk(&mut st, KeyCode::Char('g'));
        assert_eq!(st.list.selected(), Some(1));
        assert!(
            text.contains("╭ milestones "),
            "the first box's top:\n{text}"
        );
    }

    #[test]
    fn a_log_line_colours_the_verb_the_ids_and_whoever_acted() {
        let spans = log_spans("claim t-abc123 by Jack Roddy/2aff5e6c");
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            texts,
            ["claim", " ", "t-abc123", " by ", "Jack Roddy/2aff5e6c"]
        );
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].style.fg, Some(Color::Cyan));
        // the whole name, session and all, in the agent's colour
        assert_eq!(spans[4].style.fg, Some(Color::Magenta));
        let spans = log_spans("dep t-abc123 <- t-def456");
        assert_eq!(spans[0].style.fg, None);
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[4].style.fg, None);
        assert_eq!(spans[6].style.fg, Some(Color::Cyan));
        assert_eq!(log_spans("forget prefix")[0].style.fg, Some(palette::AGENT));
        // a verb that changes no status is bold alone
        let update = log_spans("update t-abc123");
        assert_eq!(update[0].style.fg, None);
        assert!(update[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_long_box_stops_at_five_lines_and_counts_the_rest() {
        let mut i = thread();
        i.body = (1..=9)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let page = Page::open(i.id.clone());
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        let body = strings(&lines[boxes[0].lines.clone()]);
        // top, five lines, the count, bottom
        assert_eq!(body.len(), 8, "{body:#?}");
        assert!(body[5].starts_with("│ line 3"), "{}", body[5]);
        assert!(body[6].starts_with("│ ⋯ 12 more"), "{}", body[6]);
        assert_eq!(lines[6].spans[1].style.fg, Some(palette::FOCUS));
        assert!(body[7].starts_with("╰"), "{}", body[7]);
        // the body's own page shows it whole
        let mut page = Page::open(i.id.clone());
        page.enter_box(&boxes);
        assert_eq!(page.root, Root::Body);
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        assert_eq!(boxes.len(), 1);
        assert_eq!(lines.len(), 19);
        assert!(!lines[18].to_string().contains("more"));
    }

    #[test]
    fn a_thread_page_is_the_note_and_every_reply_whole() {
        let mut i = thread();
        i.notes[1].text = (1..=9)
            .map(|n| format!("b {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut page = Page::open(i.id.clone());
        let (_, boxes, _) = posts(&i, 40, &page, true, "ann");
        // enter on a: its thread holds a, b and c, whole, with c one
        // step in from b
        page.go(1);
        page.enter_box(&boxes);
        assert_eq!(page.root, Root::Note("aaaa0000".into()));
        assert_eq!(page.trail(), ["t-000009", "aaaa0000"]);
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        assert_eq!(
            box_ids(&boxes),
            [Some("aaaa0000"), Some("bbbb0000"), Some("cccc0000")]
        );
        assert_eq!(
            boxes[1].lines.len(),
            19,
            "nine paragraphs of b and its borders"
        );
        assert!(
            lines[boxes[1].lines.start]
                .to_string()
                .starts_with("  ╭ ann")
        );
        assert!(
            lines[boxes[2].lines.start]
                .to_string()
                .starts_with("    ╭ ann")
        );
        // the line above a reply is its rail, in its own column
        assert_eq!(lines[boxes[1].lines.start - 1].to_string(), "  │");
        assert_eq!(lines[boxes[2].lines.start - 1].to_string(), "    │");
        assert!(!lines.iter().any(|l| l.to_string().contains("more")));
        // every line of every box is exactly as wide as the pane
        for b in &boxes {
            for l in &lines[b.lines.clone()] {
                assert_eq!(l.to_string().chars().count(), 40, "{l}");
            }
        }
        assert!(!lines.iter().any(|l| l.to_string().contains("more")));
        // enter does nothing on a thread's page
        page.go(1);
        page.enter_box(&boxes);
        assert_eq!(page.root, Root::Note("aaaa0000".into()));
        // back lands on a in the issue's page
        assert!(page.back());
        assert_eq!((page.root.clone(), page.cursor), (Root::Issue, 1));
        assert!(!page.back());
        // a's note removed: its thread page drops out, and a page on
        // it goes back to the issue
        let mut gone = i.clone();
        gone.notes[0].deleted_at = Some(Timestamp::now());
        let db = db(vec![gone]);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        assert_eq!(page.trail(), ["t-000009", "aaaa0000"]);
        assert!(page.prune(&db));
        assert_eq!(page.root, Root::Issue);
    }

    #[test]
    fn a_thread_opens_in_place_under_the_issue_card() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let i = thread();
        let db = db(vec![i.clone()]);
        let header = test_header();
        let mut page = Page::open(i.id.clone());
        let draw = |page: &mut Page| {
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
            let mut limits = Limits::default();
            terminal
                .draw(|f| {
                    limits = draw_detail(f, &header, View::List, &db, &i, page, "ann", true, None)
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
            (rows, limits)
        };
        page.go(1);
        let (before, limits) = draw(&mut page);
        let row_of = |rows: &[String], needle: &str| rows.iter().position(|r| r.contains(needle));
        let was = row_of(&before, "╭ ann · just now").unwrap();
        page.enter_box(&limits.boxes);
        let (after, limits) = draw(&mut page);
        // the card, the about pane and the pane's title stay; the
        // note goes to the top of the pane; the body and d are gone
        assert!(
            after[1].starts_with("list › t-000009 › aaaa0000"),
            "{}",
            after[1]
        );
        assert!(after[3].starts_with("╭ t-000009 "), "{}", after[3]);
        assert!(after[3].contains("╭ about "), "{}", after[3]);
        let pane = row_of(&after, "╭ notes ").unwrap();
        assert!(was > pane + 1, "the note was not at the top before");
        assert_eq!(
            row_of(&after, "╭ ann · just now"),
            Some(pane + 1),
            "{after:#?}"
        );
        assert!(row_of(&after, "the body").is_none(), "{after:#?}");
        assert!(row_of(&after, "note dddd0000").is_none(), "{after:#?}");
        assert!(row_of(&after, "│     ╭ ann").is_some(), "{after:#?}");
        assert_eq!(limits.boxes.len(), 3);
        // back puts everything where it was
        assert!(page.back());
        let (again, _) = draw(&mut page);
        assert_eq!(again, before);
    }

    #[test]
    fn a_box_taller_than_the_pane_scrolls_through_before_the_cursor_leaves() {
        let mut i = thread();
        i.notes[0].text = (1..=12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut page = Page::open(i.id.clone());
        page.go(1);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        // the root is 25 lines in a pane of 10
        assert_eq!(boxes[0].lines, 0..25, "{boxes:#?}");
        let rows = 10;
        reveal(&mut page.scroll, page.cursor, &boxes, rows);
        assert_eq!(page.scroll, 0);
        for n in 1..=15 {
            page.step_box(&boxes, rows, 1);
            assert_eq!((page.cursor, page.scroll), (0, n));
        }
        // the bottom shows: the next step moves on to b
        page.step_box(&boxes, rows, 1);
        assert_eq!(page.cursor, 1);
        // on the body's page there is nowhere to move on to, so the
        // bottom stays put
        let mut body = Page::open(i.id.clone());
        i.body = i.notes[0].text.clone();
        body.enter_box(&posts(&i, 40, &body, true, "ann").1);
        let (_, one, _) = posts(&i, 40, &body, true, "ann");
        assert_eq!(one.len(), 1);
        for _ in 0..40 {
            body.step_box(&one, rows, 1);
        }
        body.reveal = false;
        body.step_box(&one, rows, 1);
        assert_eq!((body.cursor, body.scroll, body.reveal), (0, 15, false));
        reveal(&mut page.scroll, page.cursor, &boxes, rows);
        assert!(page.scroll + rows >= boxes[1].lines.end);
        // back up: b stays the box until the top of a shows again
        page.step_box(&boxes, rows, -1);
        assert_eq!(page.cursor, 0);
        reveal(&mut page.scroll, page.cursor, &boxes, rows);
        assert_eq!(page.scroll, 0);
        let _ = lines;
    }

    #[test]
    fn a_rail_runs_past_earlier_replies_while_a_later_one_is_due() {
        // a; b answers a; c answers b; e answers a
        let mut i = thread();
        i.notes.push(Note {
            id: "eeee0000".into(),
            reply_to: Some("aaaa0000".into()),
            edited_at: None,
            deleted_at: None,
            at: Timestamp::now(),
            author: "ann".into(),
            text: "note eeee0000".into(),
            commit: String::new(),
            branch: String::new(),
        });
        let mut page = Page::open(i.id.clone());
        page.go(1);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        let (lines, boxes, _) = posts(&i, 40, &page, true, "ann");
        assert_eq!(
            box_ids(&boxes),
            [
                Some("aaaa0000"),
                Some("bbbb0000"),
                Some("cccc0000"),
                Some("eeee0000")
            ]
        );
        let text = strings(&lines);
        let top = |k: usize| text[boxes[k].lines.start].clone();
        let above = |k: usize| text[boxes[k].lines.start - 1].clone();
        // b's rail drops from a; c's lines and the line above c carry
        // the rail on to e; e's rail ends the run
        assert_eq!(above(1), "  │");
        assert!(top(1).starts_with("  ╭ ann"), "{}", top(1));
        assert_eq!(above(2), "  │ │");
        assert!(top(2).starts_with("  │ ╭ ann"), "{}", top(2));
        assert!(text[boxes[2].lines.end - 1].starts_with("  │ ╰"));
        assert_eq!(above(3), "  │");
        assert!(top(3).starts_with("  ╭ ann"), "{}", top(3));
    }

    fn shifted(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn on_a_thread_h_and_l_select_and_enter_offers_reply_or_edit() {
        let i = thread();
        let db = db(vec![i.clone()]);
        let rows = Vec::new();
        let mut st = State::new();
        let mut page = Page::open(i.id.clone());
        page.go(1);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        let (_, boxes, _) = posts(&i, 40, &page, true, "ann");
        st.mode = Mode::Detail(page);
        let limits = Limits {
            scroll: 0,
            rows: 20,
            boxes,
            ties: Vec::new(),
        };
        let key = |st: &mut State, key: KeyEvent| handle(st, key, &rows, &db, &limits);
        fn shown(st: &State) -> &Page {
            match &st.mode {
                Mode::Detail(p) => p,
                Mode::Rows => panic!("a page"),
            }
        }
        key(&mut st, press(KeyCode::Char('l')));
        key(&mut st, press(KeyCode::Char('l')));
        assert_eq!((shown(&st).cursor, shown(&st).reveal), (2, true));
        key(&mut st, press(KeyCode::Char('h')));
        assert_eq!(shown(&st).cursor, 1);
        // enter offers reply; l turns it to edit; enter opens the
        // editor on b's text; esc keeps the draft, enter again
        // brings it back
        key(&mut st, press(KeyCode::Enter));
        assert_eq!(shown(&st).ask, Some(Target::Reply(String::new())));
        key(&mut st, press(KeyCode::Char('l')));
        assert_eq!(shown(&st).ask, Some(Target::Edit(String::new())));
        key(&mut st, press(KeyCode::Enter));
        let draft = shown(&st).draft.as_ref().unwrap();
        assert_eq!(draft.target, Target::Edit("bbbb0000".into()));
        assert_eq!(draft.text(), "note bbbb0000");
        assert!(shown(&st).open && shown(&st).ask.is_none());
        // typing goes in; shift+enter breaks the line; q is a
        // letter here, not quit; enter sends
        key(&mut st, press(KeyCode::Char(' ')));
        key(&mut st, press(KeyCode::Char('q')));
        key(&mut st, shifted(KeyCode::Enter));
        key(&mut st, press(KeyCode::Char('x')));
        assert_eq!(
            shown(&st).draft.as_ref().unwrap().text(),
            "note bbbb0000 q\nx"
        );
        key(&mut st, press(KeyCode::Esc));
        assert!(!shown(&st).open && shown(&st).draft.is_some());
        key(&mut st, press(KeyCode::Enter));
        key(&mut st, press(KeyCode::Char('l')));
        key(&mut st, press(KeyCode::Enter));
        assert!(shown(&st).open);
        assert_eq!(
            shown(&st).draft.as_ref().unwrap().text(),
            "note bbbb0000 q\nx"
        );
        assert_eq!(key(&mut st, press(KeyCode::Enter)), Action::Send);
        // a reply to b starts empty, and an empty one does not send
        key(&mut st, press(KeyCode::Esc));
        key(&mut st, press(KeyCode::Enter));
        key(&mut st, press(KeyCode::Enter));
        assert_eq!(
            shown(&st).draft.as_ref().unwrap().target,
            Target::Reply("bbbb0000".into())
        );
        assert_eq!(key(&mut st, press(KeyCode::Enter)), Action::Draw);
        // leaving the page drops the draft
        key(&mut st, press(KeyCode::Esc));
        key(&mut st, press(KeyCode::Esc));
        assert!(shown(&st).draft.is_none() && shown(&st).root == Root::Issue);
        // n on the issue's page opens a new note
        key(&mut st, press(KeyCode::Char('n')));
        assert_eq!(shown(&st).draft.as_ref().unwrap().target, Target::Note);
    }

    #[test]
    fn the_compose_box_sits_where_the_note_will_and_holds_the_cursor() {
        let i = thread();
        let mut page = Page::open(i.id.clone());
        page.go(1);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        // a reply to b goes after c, one deeper than b, hanging from
        // its rail
        page.compose(Target::Reply("bbbb0000".into()), "");
        let (lines, boxes, editor) = posts(&i, 40, &page, true, "ann");
        let text = strings(&lines);
        let (range, (line, col)) = editor.unwrap();
        assert_eq!(range.start, boxes[2].lines.end + 1, "{text:#?}");
        assert!(
            text[range.start].starts_with("    ╭ ann · reply to bbbb0000 "),
            "{}",
            text[range.start]
        );
        assert_eq!((line, col), (range.start + 1, 6));
        assert_eq!(text[range.start - 1], "    │");
        // a reply to a goes after c, and the rail to it runs past
        // b and c
        page.compose(Target::Reply("aaaa0000".into()), "");
        let (lines, boxes, editor) = posts(&i, 40, &page, true, "ann");
        let text = strings(&lines);
        let (range, _) = editor.unwrap();
        assert!(
            text[range.start].starts_with("  ╭ ann · reply to aaaa0000 "),
            "{}",
            text[range.start]
        );
        assert!(
            text[boxes[2].lines.start].starts_with("  │ ╭ ann"),
            "{}",
            text[boxes[2].lines.start]
        );
        assert_eq!(text[range.start - 1], "  │");
        // an edit stands in for the note's own box
        page.compose(Target::Edit("bbbb0000".into()), "note bbbb0000");
        let (lines, boxes, editor) = posts(&i, 40, &page, true, "ann");
        let text = strings(&lines);
        let (range, (line, col)) = editor.unwrap();
        assert_eq!(box_ids(&boxes), [Some("aaaa0000"), Some("cccc0000")]);
        assert_eq!(range.start, boxes[0].lines.end + 1);
        assert!(
            text[range.start].starts_with("  ╭ ann · editing bbbb0000 "),
            "{}",
            text[range.start]
        );
        assert!(
            text[range.start + 1].starts_with("  │ note bbbb0000"),
            "{}",
            text[range.start + 1]
        );
        assert_eq!((line, col), (range.start + 1, 2 + 2 + 13));
        // a new note goes at the end of the issue's page
        let mut page = Page::open(i.id.clone());
        page.compose(Target::Note, "");
        let (lines, boxes, editor) = posts(&i, 40, &page, true, "ann");
        let (range, _) = editor.unwrap();
        assert_eq!(range.start, boxes.last().unwrap().lines.end + 1);
        assert_eq!(range.end, lines.len());
    }

    #[test]
    fn the_choice_box_and_the_cursor_are_drawn() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let i = thread();
        let db = db(vec![i.clone()]);
        let header = test_header();
        let mut page = Page::open(i.id.clone());
        page.go(1);
        page.enter_box(&posts(&i, 40, &page, true, "ann").1);
        page.ask = Some(Target::Edit(String::new()));
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| {
                draw_detail(
                    f,
                    &header,
                    View::List,
                    &db,
                    &i,
                    &mut page,
                    "ann",
                    true,
                    None,
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
        assert!(rows.iter().any(|r| r.contains("╭ note ")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("reply   edit")), "{rows:#?}");
        assert!(rows[19].starts_with("h/l choose"), "{}", rows[19]);
        // typing: the cursor is in the box and the footer says how
        // to send
        page.compose(Target::Reply("aaaa0000".into()), "hi");
        terminal
            .draw(|f| {
                draw_detail(
                    f,
                    &header,
                    View::List,
                    &db,
                    &i,
                    &mut page,
                    "ann",
                    false,
                    None,
                );
            })
            .unwrap();
        let shown = terminal.get_cursor_position().unwrap();
        let (x, y) = (shown.x, shown.y);
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..buffer.area.width)
            .map(|c| buffer[(c, y)].symbol().to_string())
            .collect();
        assert!(row.contains("│ hi "), "{row}");
        assert_eq!(buffer[(x - 1, y)].symbol(), "i");
        let foot: String = (0..buffer.area.width)
            .map(|c| buffer[(c, 19)].symbol().to_string())
            .collect();
        assert!(foot.starts_with("enter send   alt+enter newline"), "{foot}");
    }

    #[test]
    fn n_on_the_rows_opens_a_form_that_walks_types_and_creates() {
        let (db, mut st) = on_rows();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let limits = Limits::default();
        let key = |st: &mut State, key: KeyEvent| handle(st, key, &rows, &db, &limits);
        // the cursor is on the milestone, so the form starts under it,
        // typing into the title
        assert_eq!(chosen(&st, &rows), "t-000001");
        key(&mut st, press(KeyCode::Char('n')));
        assert!(st.form_shown);
        let form = st.form.as_ref().unwrap();
        assert_eq!(form.parent_id(), Some("t-000001"));
        assert_eq!(
            (form.kind, form.priority, form.field, form.editing),
            (Kind::Task, 2, Field::Title, true)
        );
        // q and n are letters in the title; enter moves on to the body
        // and keeps typing; shift+enter breaks the body; enter again
        // lands on the kind
        for c in "Wire q n".chars() {
            key(&mut st, press(KeyCode::Char(c)));
        }
        key(&mut st, press(KeyCode::Enter));
        let form = st.form.as_ref().unwrap();
        assert_eq!((form.field, form.editing), (Field::Body, true));
        key(&mut st, press(KeyCode::Char('a')));
        key(&mut st, shifted(KeyCode::Enter));
        key(&mut st, press(KeyCode::Char('b')));
        key(&mut st, press(KeyCode::Enter));
        let form = st.form.as_ref().unwrap();
        assert_eq!(
            (form.title.text(), form.body.text()),
            ("Wire q n".into(), "a\nb".into())
        );
        assert_eq!((form.field, form.editing), (Field::Kind, false));
        // l turns the kind, enter moves on; down to the parent, h
        // takes it off; the buttons: l is cancel, h is create, enter
        // sends
        key(&mut st, press(KeyCode::Char('l')));
        key(&mut st, press(KeyCode::Enter));
        key(&mut st, press(KeyCode::Down));
        key(&mut st, press(KeyCode::Char('h')));
        let form = st.form.as_ref().unwrap();
        assert_eq!(
            (form.kind, form.parent_id(), form.field),
            (Kind::Bug, None, Field::Parent)
        );
        key(&mut st, press(KeyCode::Char('j')));
        key(&mut st, press(KeyCode::Char('l')));
        assert!(st.form.as_ref().unwrap().cancel);
        key(&mut st, press(KeyCode::Char('h')));
        assert_eq!(key(&mut st, press(KeyCode::Enter)), Action::Send);
        // esc keeps the form; n brings it back as it was; tab drops it
        key(&mut st, press(KeyCode::Esc));
        assert!(!st.form_shown && st.form.is_some());
        key(&mut st, press(KeyCode::Char('n')));
        assert!(st.form_shown);
        assert_eq!(st.form.as_ref().unwrap().title.text(), "Wire q n");
        key(&mut st, press(KeyCode::Esc));
        key(&mut st, press(KeyCode::Tab));
        assert!(st.form.is_none());
        // e fills the form from the chosen issue, itself left out of
        // the parents, and saves instead of creating
        key(&mut st, press(KeyCode::BackTab));
        assert_eq!(chosen(&st, &rows), "t-000001");
        key(&mut st, press(KeyCode::Char('e')));
        let form = st.form.as_ref().unwrap();
        assert_eq!(form.edit.as_deref(), Some("t-000001"));
        assert_eq!(form.title.text(), "the parent");
        assert_eq!(form.kind, Kind::Milestone);
        assert!(form.parents.is_empty());
        assert!(form.editing);
    }

    #[test]
    fn the_form_is_drawn_under_the_chosen_row_with_the_cursor_in_its_field() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (db, _) = fixture();
        let rows = list_rows(&db, &Filter::default(), "ann", &HashSet::new());
        let screen = Screen {
            view: View::List,
            header: test_header(),
            widths: widths(&rows),
            rows,
            recent: Vec::new(),
        };
        let mut state = ListState::default();
        state.select(Some(0));
        let mut form = Form::new(
            vec![("t-000001".into(), "the parent".into())],
            Some("t-000001"),
        );
        form.editing = true;
        for c in "Wire".chars() {
            form.title.insert(c);
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                draw_rows(
                    f,
                    &screen,
                    &mut state,
                    &db,
                    false,
                    &[],
                    Compose {
                        form: Some(&form),
                        actor: "jack",
                        enhanced: true,
                        notice: None,
                    },
                )
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
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        let first = at("t-000001").unwrap();
        assert_eq!(at("╭ jack · new issue "), Some(first + 1), "{rows:#?}");
        assert_eq!(at("│ title     Wire"), Some(first + 2), "{rows:#?}");
        assert!(
            at("│ parent    t-000001  the parent").is_some(),
            "{rows:#?}"
        );
        assert!(at("create   cancel").is_some(), "{rows:#?}");
        assert_eq!(
            at("t-000002"),
            Some(first + 10),
            "the rows go on under the form"
        );
        let caret = terminal.get_cursor_position().unwrap();
        assert_eq!(usize::from(caret.y), first + 2);
        assert_eq!(buffer[(caret.x - 1, caret.y)].symbol(), "e");
        assert!(
            rows[29].starts_with("enter done   esc done"),
            "{}",
            rows[29]
        );
    }
}
