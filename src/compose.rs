//! A small editor for a note on the screen: lines of text and a
//! cursor, the keys a person expects, and soft wrapping for the box
//! it is drawn in.

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
}
