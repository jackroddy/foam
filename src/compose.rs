//! A small editor for a note on the screen: lines of text and a
//! cursor, the keys a person expects, and soft wrapping for the box
//! it is drawn in; and the form a new issue is written on.

use crate::model::Kind;

/// What the text becomes when it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A new note on the issue.
    Note,
    /// An answer to the note with this id.
    Reply(String),
    /// The new text of the note with this id.
    Edit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub target: Target,
    lines: Vec<String>,
    /// The line the cursor is on, and the char it stands before.
    line: usize,
    col: usize,
}

impl Editor {
    /// An editor holding `text`, with the cursor at its end.
    pub fn new(target: Target, text: &str) -> Editor {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let line = lines.len() - 1;
        let col = lines[line].chars().count();
        Editor {
            target,
            lines,
            line,
            col,
        }
    }

    /// The text as it would be sent: the lines joined, trailing
    /// blank lines dropped.
    pub fn text(&self) -> String {
        self.lines.join("\n").trim_end().to_string()
    }

    pub fn is_blank(&self) -> bool {
        self.text().is_empty()
    }

    fn byte(&self) -> usize {
        self.lines[self.line]
            .char_indices()
            .nth(self.col)
            .map_or(self.lines[self.line].len(), |(b, _)| b)
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte();
        self.lines[self.line].insert(at, c);
        self.col += 1;
    }

    /// Break the line at the cursor.
    pub fn newline(&mut self) {
        let at = self.byte();
        let rest = self.lines[self.line].split_off(at);
        self.lines.insert(self.line + 1, rest);
        self.line += 1;
        self.col = 0;
    }

    /// Remove the char before the cursor, or join the line to the
    /// one above at its start.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let at = self.byte();
            self.lines[self.line].remove(at);
        } else if self.line > 0 {
            let rest = self.lines.remove(self.line);
            self.line -= 1;
            self.col = self.lines[self.line].chars().count();
            self.lines[self.line].push_str(&rest);
        }
    }

    /// Remove the char under the cursor, or join the line below to
    /// this one at its end.
    pub fn delete(&mut self) {
        if self.col < self.lines[self.line].chars().count() {
            let at = self.byte();
            self.lines[self.line].remove(at);
        } else if self.line + 1 < self.lines.len() {
            let rest = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&rest);
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.lines[self.line].chars().count();
        }
    }

    pub fn right(&mut self) {
        if self.col < self.lines[self.line].chars().count() {
            self.col += 1;
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    pub fn up(&mut self) {
        if self.line > 0 {
            self.line -= 1;
            self.col = self.col.min(self.lines[self.line].chars().count());
        }
    }

    pub fn down(&mut self) {
        if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = self.col.min(self.lines[self.line].chars().count());
        }
    }

    pub fn home(&mut self) {
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.col = self.lines[self.line].chars().count();
    }

    /// The lines as a box `width` chars wide shows them, each line
    /// broken by chars where it runs past the width, and the row
    /// and column of the cursor among them.
    pub fn wrapped(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut at = (0, 0);
        for (n, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            // a line always takes a row, and a cursor at the very
            // end of a full row stands at the start of the next
            let pieces = chars.len() / width + 1;
            for p in 0..pieces {
                let piece: String = chars[p * width..(chars.len()).min((p + 1) * width)]
                    .iter()
                    .collect();
                if n == self.line && self.col / width == p {
                    at = (rows.len(), self.col % width);
                }
                rows.push(piece);
            }
        }
        (rows, at)
    }
}

/// A field of the new-issue form, in the order they are walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Body,
    Kind,
    Priority,
    Parent,
    Buttons,
}

impl Field {
    const ALL: [Field; 6] = [
        Field::Title,
        Field::Body,
        Field::Kind,
        Field::Priority,
        Field::Parent,
        Field::Buttons,
    ];

    pub fn next(self) -> Field {
        let k = Field::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Field::ALL[(k + 1).min(Field::ALL.len() - 1)]
    }

    pub fn previous(self) -> Field {
        let k = Field::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Field::ALL[k.saturating_sub(1)]
    }
}

/// A new issue being written: its fields, which one is selected,
/// whether a text field is being typed into, and which button is
/// under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub title: Editor,
    pub body: Editor,
    pub kind: Kind,
    pub priority: u8,
    /// Which of `parents` the issue goes under; `None` for none.
    pub parent: Option<usize>,
    /// The milestones it may go under, as (id, title).
    pub parents: Vec<(String, String)>,
    pub field: Field,
    pub editing: bool,
    pub cancel: bool,
}

impl Form {
    /// A blank form: a task at p2, under `under` if that is one of
    /// `parents`.
    pub fn new(parents: Vec<(String, String)>, under: Option<&str>) -> Form {
        let parent = under.and_then(|u| parents.iter().position(|(id, _)| id == u));
        Form {
            title: Editor::new(Target::Note, ""),
            body: Editor::new(Target::Note, ""),
            kind: Kind::Task,
            priority: 2,
            parent,
            parents,
            field: Field::Title,
            editing: false,
            cancel: false,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.parent.map(|k| self.parents[k].0.as_str())
    }

    /// Turn the selected choice one step; `by` is 1 or -1.
    pub fn turn(&mut self, by: isize) {
        let step = |k: usize, n: usize| -> usize {
            if by.is_positive() {
                (k + 1) % n
            } else {
                (k + n - 1) % n
            }
        };
        match self.field {
            Field::Kind => {
                let all = <Kind as clap::ValueEnum>::value_variants();
                let k = all.iter().position(|k| *k == self.kind).unwrap_or(0);
                self.kind = all[step(k, all.len())];
            }
            Field::Priority => {
                self.priority = u8::try_from(step(usize::from(self.priority), 5)).unwrap_or(2)
            }
            Field::Parent => {
                // none comes first, then each milestone
                let n = self.parents.len() + 1;
                let k = self.parent.map_or(0, |k| k + 1);
                let k = step(k, n);
                self.parent = k.checked_sub(1);
            }
            Field::Buttons => self.cancel = !self.cancel,
            Field::Title | Field::Body => {}
        }
    }

    /// The text field being typed into, if any.
    pub fn editor(&mut self) -> Option<&mut Editor> {
        match (self.editing, self.field) {
            (true, Field::Title) => Some(&mut self.title),
            (true, Field::Body) => Some(&mut self.body),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Editor {
        let mut e = Editor::new(Target::Note, "");
        for c in s.chars() {
            if c == '\n' {
                e.newline();
            } else {
                e.insert(c);
            }
        }
        e
    }

    #[test]
    fn typing_breaking_and_joining_lines() {
        let mut e = typed("one\ntwo");
        assert_eq!(e.text(), "one\ntwo");
        assert_eq!((e.line, e.col), (1, 3));
        e.backspace();
        e.backspace();
        e.backspace();
        e.backspace();
        assert_eq!(e.text(), "one");
        assert_eq!((e.line, e.col), (0, 3));
        e.left();
        e.newline();
        assert_eq!(e.text(), "on\ne");
        e.up();
        e.end();
        e.delete();
        assert_eq!(e.text(), "one");
        e.home();
        e.insert('é');
        assert_eq!(e.text(), "éone");
        e.right();
        e.backspace();
        assert_eq!(e.text(), "éne");
    }

    #[test]
    fn an_edit_starts_with_the_text_and_the_cursor_at_its_end() {
        let e = Editor::new(Target::Edit("ab".into()), "first\n\nthird");
        assert_eq!((e.line, e.col), (2, 5));
        assert_eq!(e.text(), "first\n\nthird");
        assert!(Editor::new(Target::Note, "  \n").is_blank());
    }

    #[test]
    fn wrapping_breaks_long_lines_and_places_the_cursor() {
        let mut e = typed("abcdefgh\nij");
        let (rows, at) = e.wrapped(4);
        assert_eq!(rows, ["abcd", "efgh", "", "ij"]);
        assert_eq!(at, (3, 2));
        e.up();
        assert_eq!((e.line, e.col), (0, 2));
        assert_eq!(e.wrapped(4).1, (0, 2));
        e.end();
        // at the end of a full row the cursor stands on the next
        assert_eq!(e.wrapped(4).1, (2, 0));
        assert_eq!(Editor::new(Target::Note, "").wrapped(4).0, [""]);
    }

    #[test]
    fn a_form_walks_its_fields_and_turns_its_choices() {
        let parents = vec![
            ("m-1".to_string(), "one".to_string()),
            ("m-2".into(), "two".into()),
        ];
        let mut f = Form::new(parents, Some("m-2"));
        assert_eq!(f.parent_id(), Some("m-2"));
        assert_eq!(f.field, Field::Title);
        assert!(f.editor().is_none());
        f.editing = true;
        f.editor().unwrap().insert('t');
        assert_eq!(f.title.text(), "t");
        f.editing = false;
        f.field = f.field.next().next();
        assert_eq!(f.field, Field::Kind);
        f.turn(1);
        assert_eq!(f.kind, Kind::Bug);
        f.turn(-1);
        f.turn(-1);
        assert_eq!(f.kind, Kind::Decision);
        f.field = f.field.next();
        f.turn(-1);
        f.turn(-1);
        f.turn(-1);
        assert_eq!(f.priority, 4);
        f.turn(1);
        assert_eq!(f.priority, 0);
        f.field = f.field.next();
        f.turn(1);
        assert_eq!(f.parent_id(), None);
        f.turn(1);
        assert_eq!(f.parent_id(), Some("m-1"));
        f.turn(-1);
        f.turn(-1);
        assert_eq!(f.parent_id(), Some("m-2"));
        f.field = f.field.next();
        assert_eq!(f.field.next(), Field::Buttons);
        f.turn(1);
        assert!(f.cancel);
        assert_eq!(Field::Title.previous(), Field::Title);
        assert_eq!(Form::new(Vec::new(), Some("m-9")).parent, None);
    }
}
