//! A small markdown subset, parsed into blocks and laid out as
//! lines of an exact width: headings, paragraphs, lists, fenced
//! code, tables, rules and inline code. Everything else stays
//! literal text.

use crate::hl::{Class, Lexer};
use ratatui::style::{Color, Modifier, Style};

use crate::palette;
use ratatui::text::{Line, Span};

/// A run of text inside a paragraph, list item or table cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
}

/// One entry of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// How far the item is nested, from its indent.
    pub depth: usize,
    /// The number of a numbered item, `None` for a bullet.
    pub number: Option<u64>,
    pub text: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        /// One to three, the number of `#`.
        level: u8,
        text: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    List(Vec<Item>),
    Code {
        lang: Option<String>,
        lines: Vec<String>,
    },
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

struct Theme {
    heading: Style,
    header: Style,
    code: Style,
    lang: Style,
    bullet: Style,
    rule: Style,
    comment: Style,
    string: Style,
    number: Style,
    keyword: Style,
}

const THEME: Theme = Theme {
    heading: Style::new().add_modifier(Modifier::BOLD),
    header: Style::new().add_modifier(Modifier::BOLD),
    code: Style::new().fg(palette::REF),
    lang: Style::new().add_modifier(Modifier::DIM),
    bullet: Style::new().add_modifier(Modifier::BOLD),
    rule: Style::new().add_modifier(Modifier::DIM),
    // inside a code block the hues name token kinds, not the
    // concepts they stand for elsewhere; the gutter keeps them apart
    comment: Style::new().add_modifier(Modifier::DIM),
    string: Style::new().fg(palette::OPEN),
    number: Style::new().fg(palette::AGENT),
    keyword: Style::new().fg(palette::DEFERRED),
};

const MIN_COLUMN: usize = 8;

/// `text` parsed and laid out at `width`.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    layout(&parse(text), width)
}

/// The blocks of `text`, read a line at a time.
pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut para: Vec<Inline> = Vec::new();
    let mut list: Vec<Item> = Vec::new();
    let mut text_column = 0;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(lang) = fence(line) {
            flush(&mut blocks, &mut para, &mut list);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !closes_fence(lines[i]) {
                code.push(lines[i].replace('\t', "    "));
                i += 1;
            }
            blocks.push(Block::Code { lang, lines: code });
            i += 1;
            continue;
        }
        if line.contains('|') && lines.get(i + 1).is_some_and(|l| delimiter_row(l)) {
            flush(&mut blocks, &mut para, &mut list);
            let header = cells(line);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') {
                rows.push(cells(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table { header, rows });
            continue;
        }
        if let Some((level, rest)) = heading(line) {
            flush(&mut blocks, &mut para, &mut list);
            blocks.push(Block::Heading {
                level,
                text: inlines(rest),
            });
            i += 1;
            continue;
        }
        if rule(line) {
            flush(&mut blocks, &mut para, &mut list);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some((item, col)) = list_item(line) {
            flush_paragraph(&mut blocks, &mut para);
            // a top-level item of the other kind starts a new list
            let other_kind = item.depth == 0
                && list
                    .first()
                    .is_some_and(|f| f.number.is_some() != item.number.is_some());
            if other_kind {
                flush(&mut blocks, &mut para, &mut list);
            }
            list.push(item);
            text_column = col;
            i += 1;
            continue;
        }
        if line.trim().is_empty() {
            flush_paragraph(&mut blocks, &mut para);
            i += 1;
            continue;
        }
        if let Some(last) = list.last_mut() {
            if indent(line) >= text_column {
                join(&mut last.text, inlines(line.trim()));
                i += 1;
                continue;
            }
            flush(&mut blocks, &mut para, &mut list);
        }
        join(&mut para, inlines(line.trim()));
        i += 1;
    }
    flush(&mut blocks, &mut para, &mut list);
    blocks
}

fn flush(blocks: &mut Vec<Block>, para: &mut Vec<Inline>, list: &mut Vec<Item>) {
    flush_paragraph(blocks, para);
    if !list.is_empty() {
        blocks.push(Block::List(std::mem::take(list)));
    }
}

fn flush_paragraph(blocks: &mut Vec<Block>, para: &mut Vec<Inline>) {
    if !para.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(para)));
    }
}

/// Append a line's inlines to `to`, a space between the lines.
fn join(to: &mut Vec<Inline>, line: Vec<Inline>) {
    if !to.is_empty() {
        push_text(to, " ");
    }
    for inline in line {
        match inline {
            Inline::Text(t) => push_text(to, &t),
            code => to.push(code),
        }
    }
}

fn push_text(to: &mut Vec<Inline>, s: &str) {
    if let Some(Inline::Text(t)) = to.last_mut() {
        t.push_str(s);
    } else {
        to.push(Inline::Text(s.to_string()));
    }
}

/// The level and text of a heading line: one to three `#`, a
/// space, and text.
fn heading(line: &str) -> Option<(u8, &str)> {
    let line = line.trim_start();
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].strip_prefix(' ')?.trim();
    (!rest.is_empty()).then_some((hashes as u8, rest))
}

/// Whether `line` is a rule: three or more `-` and nothing else.
fn rule(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.bytes().all(|b| b == b'-')
}

/// The language of a line that opens a fence, `Some(None)` for a
/// fence without one.
fn fence(line: &str) -> Option<Option<String>> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let rest = rest.strip_prefix("```")?;
    Some(
        rest.trim_start_matches('`')
            .split_whitespace()
            .next()
            .map(str::to_string),
    )
}

fn closes_fence(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("```") && t.trim_start_matches('`').is_empty()
}

fn delimiter_row(line: &str) -> bool {
    line.contains('|')
        && split_cells(line).iter().all(|c| {
            let c = c.trim();
            c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

fn split_cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

fn cells(line: &str) -> Vec<Vec<Inline>> {
    split_cells(line)
        .into_iter()
        .map(|c| inlines(c.trim()))
        .collect()
}

/// Leading whitespace in columns, a tab counting two.
fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum()
}

/// A list item and the column its text starts at.
fn list_item(line: &str) -> Option<(Item, usize)> {
    let lead = indent(line);
    let rest = line.trim_start();
    let (number, after) = if let Some(after) = rest
        .strip_prefix(['-', '*', '+'])
        .filter(|a| a.starts_with(' '))
    {
        (None, after)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let after = rest[digits..]
            .strip_prefix(['.', ')'])
            .filter(|a| digits > 0 && a.starts_with(' '))?;
        (Some(rest[..digits].parse().ok()?), after)
    };
    let text = after.trim_start();
    let marker = rest.chars().count() - after.chars().count();
    let gap = after.chars().count() - text.chars().count();
    let item = Item {
        depth: lead / 2,
        number,
        text: inlines(text.trim_end()),
    };
    Some((item, lead + marker + gap))
}

/// The inlines of one line: a run of backticks opens code that
/// the next run of the same length closes, and an unmatched run
/// is text.
fn inlines(line: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        text.push_str(&rest[..start]);
        let after = &rest[start..];
        let ticks = after.len() - after.trim_start_matches('`').len();
        let body = &after[ticks..];
        match closing(body, ticks) {
            Some(end) => {
                if !text.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut text)));
                }
                if end > 0 {
                    out.push(Inline::Code(body[..end].to_string()));
                }
                rest = &body[end + ticks..];
            }
            None => {
                text.push_str(&after[..ticks]);
                rest = body;
            }
        }
    }
    text.push_str(rest);
    if !text.is_empty() {
        out.push(Inline::Text(text));
    }
    out
}

/// Where in `s` a run of exactly `ticks` backticks starts.
fn closing(s: &str, ticks: usize) -> Option<usize> {
    let mut at = 0;
    while let Some(start) = s[at..].find('`') {
        let run = &s[at + start..];
        let len = run.len() - run.trim_start_matches('`').len();
        if len == ticks {
            return Some(at + start);
        }
        at += start + len;
    }
    None
}

type Seg = (String, Style);

/// The blocks as lines exactly `width` characters long, a blank
/// line between blocks.
pub fn layout(blocks: &[Block], width: usize) -> Vec<Line<'static>> {
    lines(blocks, width)
        .into_iter()
        .map(|segs| finish(segs, width))
        .collect()
}

/// `text` laid out at `width` for a terminal: one string per
/// line, trailing blanks dropped, and the screen's styles as
/// escape codes when `color`.
pub fn text(text: &str, width: usize, color: bool) -> Vec<String> {
    lines(&parse(text), width)
        .into_iter()
        .map(|segs| {
            let mut merged = Vec::new();
            for (s, style) in segs {
                push_seg(&mut merged, s, style);
            }
            let mut out = String::new();
            for (s, style) in trimmed(merged, color) {
                let codes = if color { sgr(style) } else { String::new() };
                if codes.is_empty() {
                    out.push_str(&s);
                } else {
                    out.push_str(&format!("\x1b[{codes}m{s}\x1b[0m"));
                }
            }
            out
        })
        .collect()
}

/// `segs` without trailing blanks, keeping those under a
/// background when `color` paints it.
fn trimmed(mut segs: Vec<Seg>, color: bool) -> Vec<Seg> {
    while let Some((s, style)) = segs.last_mut() {
        if color && *style != Style::new() {
            break;
        }
        let keep = s.trim_end().len();
        if keep > 0 {
            s.truncate(keep);
            break;
        }
        segs.pop();
    }
    segs
}

/// The SGR parameters of `style`, empty for the default.
fn sgr(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    let modifiers = [
        (Modifier::BOLD, 1),
        (Modifier::DIM, 2),
        (Modifier::UNDERLINED, 4),
    ];
    for (m, code) in modifiers {
        if style.add_modifier.contains(m) {
            codes.push(code.to_string());
        }
    }
    if let Some(c) = style.fg.and_then(ansi) {
        codes.push(c.to_string());
    }
    if let Some(c) = style.bg.and_then(ansi) {
        codes.push((c + 10).to_string());
    }
    codes.join(";")
}

/// The foreground parameter of a named colour.
fn ansi(color: Color) -> Option<u8> {
    Some(match color {
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
        _ => return None,
    })
}

/// The blocks as lines of at most `width` characters, a blank
/// line between blocks.
fn lines(blocks: &[Block], width: usize) -> Vec<Vec<Seg>> {
    let theme = &THEME;
    let mut out: Vec<Vec<Seg>> = Vec::new();
    for (n, block) in blocks.iter().enumerate() {
        if n > 0 {
            out.push(Vec::new());
        }
        match block {
            Block::Heading { level, text } => heading_lines(*level, text, width, theme, &mut out),
            Block::Paragraph(text) => out.extend(wrap(&words(text, theme), width)),
            Block::List(items) => list(items, width, theme, &mut out),
            Block::Code { lang, lines } => code(lang.as_deref(), lines, width, theme, &mut out),
            Block::Table { header, rows } => table(header, rows, width, theme, &mut out),
            Block::Rule => out.push(vec![("─".repeat(width), theme.rule)]),
        }
    }
    out
}

/// A line cut or padded to exactly `width`, adjacent segments of
/// one style merged into a span.
fn finish(segs: Vec<Seg>, width: usize) -> Line<'static> {
    let mut spans: Vec<Seg> = Vec::new();
    let mut room = width;
    for (s, style) in segs {
        let s: String = s.chars().take(room).collect();
        room -= s.chars().count();
        push_seg(&mut spans, s, style);
    }
    push_seg(&mut spans, " ".repeat(room), Style::new());
    Line::from(
        spans
            .into_iter()
            .map(|(s, style)| Span::styled(s, style))
            .collect::<Vec<_>>(),
    )
}

fn push_seg(to: &mut Vec<Seg>, s: String, style: Style) {
    if s.is_empty() {
        return;
    }
    match to.last_mut() {
        Some((t, last)) if *last == style => t.push_str(&s),
        _ => to.push((s, style)),
    }
}

fn len(segs: &[Seg]) -> usize {
    segs.iter().map(|(s, _)| s.chars().count()).sum()
}

/// The words of a run of inlines: whitespace in text splits
/// them, and inline code joins whatever touches it.
fn words(text: &[Inline], theme: &Theme) -> Vec<Vec<Seg>> {
    let mut out = Vec::new();
    let mut word: Vec<Seg> = Vec::new();
    for inline in text {
        match inline {
            Inline::Code(c) => push_seg(&mut word, c.clone(), theme.code),
            Inline::Text(t) => {
                for ch in t.chars() {
                    if ch.is_whitespace() {
                        if !word.is_empty() {
                            out.push(std::mem::take(&mut word));
                        }
                    } else {
                        push_seg(&mut word, ch.to_string(), Style::new());
                    }
                }
            }
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Greedy word wrap at `width`, a single space between words; a
/// word longer than the width is cut across lines. No words make
/// one empty line.
fn wrap(words: &[Vec<Seg>], width: usize) -> Vec<Vec<Seg>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line: Vec<Seg> = Vec::new();
    for word in words {
        let n = len(word);
        if !line.is_empty() && len(&line) + 1 + n <= width {
            push_seg(&mut line, " ".into(), Style::new());
        } else if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if n <= width {
            for (s, style) in word {
                push_seg(&mut line, s.clone(), *style);
            }
            continue;
        }
        for (s, style) in word {
            for ch in s.chars() {
                if len(&line) == width {
                    lines.push(std::mem::take(&mut line));
                }
                push_seg(&mut line, ch.to_string(), *style);
            }
        }
    }
    lines.push(line);
    lines
}

fn heading_lines(level: u8, text: &[Inline], width: usize, theme: &Theme, out: &mut Vec<Vec<Seg>>) {
    let style = match level {
        1 => theme.heading.add_modifier(Modifier::UNDERLINED),
        _ => theme.heading,
    };
    // patched after the wrap, so the spaces between the words
    // carry the underline too
    for line in wrap(&words(text, theme), width) {
        out.push(
            line.into_iter()
                .map(|(s, st)| (s, st.patch(style)))
                .collect(),
        );
    }
}

fn list(items: &[Item], width: usize, theme: &Theme, out: &mut Vec<Vec<Seg>>) {
    for (k, item) in items.iter().enumerate() {
        let marker = match item.number {
            Some(n) => {
                let w = siblings(items, k)
                    .filter_map(|s| s.number)
                    .map(|n| n.to_string().len())
                    .max()
                    .unwrap_or(1);
                format!("{n:>w$}. ")
            }
            None if item.depth == 0 => "• ".to_string(),
            None => "▪ ".to_string(),
        };
        let mark = marker.chars().count();
        // the indent gives way before the text does
        let lead = (item.depth * 2).min(width.saturating_sub(mark + 8));
        let col = (lead + mark).min(width.saturating_sub(1));
        for (j, mut line) in wrap(&words(&item.text, theme), width - col)
            .into_iter()
            .enumerate()
        {
            let mut segs = Vec::new();
            if j == 0 {
                push_seg(&mut segs, " ".repeat(lead), Style::new());
                push_seg(&mut segs, marker.clone(), theme.bullet);
            } else {
                push_seg(&mut segs, " ".repeat(col), Style::new());
            }
            segs.append(&mut line);
            out.push(segs);
        }
    }
}

/// The items at `items[k]`'s depth in the run around it that no
/// shallower item breaks.
fn siblings(items: &[Item], k: usize) -> impl Iterator<Item = &Item> {
    let depth = items[k].depth;
    let start = items[..k]
        .iter()
        .rposition(|i| i.depth < depth)
        .map_or(0, |p| p + 1);
    let end = items[k..]
        .iter()
        .position(|i| i.depth < depth)
        .map_or(items.len(), |p| k + p);
    items[start..end].iter().filter(move |i| i.depth == depth)
}

fn code(
    lang: Option<&str>,
    lines: &[String],
    width: usize,
    theme: &Theme,
    out: &mut Vec<Vec<Seg>>,
) {
    let inner = width.saturating_sub(2);
    if let Some(lang) = lang {
        out.push(vec![
            ("┌ ".into(), theme.rule),
            (lang.chars().take(inner).collect(), theme.lang),
        ]);
    }
    let mut lexer = lang.and_then(Lexer::new);
    for line in lines {
        let segs: Vec<Seg> = match &mut lexer {
            Some(lexer) => lexer
                .line(line)
                .into_iter()
                .map(|(s, class)| {
                    let style = match class {
                        Class::Plain => Style::new(),
                        Class::Comment => theme.comment,
                        Class::Str => theme.string,
                        Class::Number => theme.number,
                        Class::Keyword => theme.keyword,
                    };
                    (s, style)
                })
                .collect(),
            None => vec![(line.clone(), Style::new())],
        };
        let mut shown = vec![("│ ".to_string(), theme.rule)];
        if len(&segs) > inner {
            let mut room = inner.saturating_sub(1);
            for (s, style) in segs {
                let s: String = s.chars().take(room).collect();
                room -= s.chars().count();
                push_seg(&mut shown, s, style);
            }
            push_seg(&mut shown, "…".into(), theme.rule);
        } else {
            shown.extend(segs);
        }
        out.push(shown);
    }
}

fn table(
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    width: usize,
    theme: &Theme,
    out: &mut Vec<Vec<Seg>>,
) {
    let cols = header.len();
    let cell = |row: &[Vec<Inline>], c: usize| words(row.get(c).map_or(&[][..], |v| v), theme);
    let mut widths: Vec<usize> = (0..cols)
        .map(|c| {
            std::iter::once(header)
                .chain(rows.iter().map(Vec::as_slice))
                .map(|row| {
                    let w = cell(row, c);
                    len(&wrap(&w, usize::MAX)[0])
                })
                .max()
                .unwrap_or(0)
        })
        .collect();
    let gaps = 3 * cols.saturating_sub(1);
    while widths.iter().sum::<usize>() + gaps > width {
        let widest = (0..cols).max_by_key(|&c| (widths[c], usize::MAX - c));
        match widest {
            Some(c) if widths[c] > MIN_COLUMN => widths[c] -= 1,
            _ => return stacked(header, rows, width, theme, out),
        }
    }
    let sep = || (" │ ".to_string(), theme.rule);
    let draw = |row: &[Vec<Inline>], style: Style, out: &mut Vec<Vec<Seg>>| {
        let wrapped: Vec<Vec<Vec<Seg>>> = (0..cols)
            .map(|c| {
                let words: Vec<Vec<Seg>> = cell(row, c)
                    .into_iter()
                    .map(|w| w.into_iter().map(|(s, st)| (s, st.patch(style))).collect())
                    .collect();
                wrap(&words, widths[c])
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for l in 0..height {
            let mut segs = Vec::new();
            for c in 0..cols {
                if c > 0 {
                    segs.push(sep());
                }
                let part = wrapped[c].get(l).cloned().unwrap_or_default();
                let fill = widths[c] - len(&part);
                segs.extend(part);
                segs.push((" ".repeat(fill), Style::new()));
            }
            out.push(segs);
        }
    };
    draw(header, theme.header, out);
    let rule: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
    let rule_line = vec![(rule.join("─┼─"), theme.rule)];
    out.push(rule_line);
    for row in rows {
        draw(row, Style::new(), out);
    }
}

/// Each row as a record, a `header: cell` line per column.
fn stacked(
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    width: usize,
    theme: &Theme,
    out: &mut Vec<Vec<Seg>>,
) {
    for (n, row) in rows.iter().enumerate() {
        if n > 0 {
            out.push(Vec::new());
        }
        for (c, head) in header.iter().enumerate() {
            let mut label = words(head, theme);
            if let Some(last) = label.last_mut() {
                last.push((":".into(), Style::new()));
            }
            for word in &mut label {
                for (_, style) in word.iter_mut() {
                    *style = style.patch(theme.rule);
                }
            }
            label.extend(words(row.get(c).map_or(&[][..], |v| v), theme));
            out.extend(wrap(&label, width));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::wrap_at;
    use ratatui::style::Color;

    fn strings(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    fn padded(lines: &[&str], width: usize) -> Vec<String> {
        lines.iter().map(|l| format!("{l:<width$}")).collect()
    }

    #[test]
    fn plain_paragraphs_wrap_as_wrap_at_does() {
        let texts = [
            "one paragraph\n\nanother",
            "a note long enough that it has to wrap onto a second line",
            "words   with  runs of\tspace and a paragraph that keeps going past the edge\n\nand a second one, also long enough to wrap twice over at this width",
            "",
        ];
        for text in texts {
            for width in [12, 36, 80] {
                let want: Vec<String> = wrap_at(text, width, width)
                    .iter()
                    .map(|l| format!("{l:<width$}"))
                    .collect();
                assert_eq!(strings(&render(text, width)), want, "{text:?} at {width}");
            }
        }
    }

    #[test]
    fn every_line_is_exactly_the_width() {
        let text = "# head\n\na `code` b\n\n- item\n  - deeper\n\n```rs\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nanunbrokenwordlongerthanthewidth";
        for width in [3, 10, 30] {
            for line in strings(&render(text, width)) {
                assert_eq!(line.chars().count(), width, "{line:?}");
            }
        }
    }

    #[test]
    fn inline_code_has_the_colour_and_the_space_between_does_not() {
        let lines = render("run `foam prime` now", 30);
        assert_eq!(strings(&lines), padded(&["run foam prime now"], 30));
        let spans = &lines[0].spans;
        let code = spans.iter().find(|s| s.content == "foam prime").unwrap();
        assert_eq!(code.style.fg, Some(Color::Cyan));
        assert_eq!(code.style.bg, None);
        let before = spans.iter().find(|s| s.content.ends_with("run ")).unwrap();
        assert_eq!(before.style.fg, None);
    }

    #[test]
    fn inline_code_moves_whole_unless_longer_than_the_width() {
        let lines = strings(&render("aaaa `bb cc` d", 8));
        assert_eq!(lines, padded(&["aaaa", "bb cc d"], 8));
        let lines = strings(&render("x `0123456789`", 6));
        assert_eq!(lines, padded(&["x", "012345", "6789"], 6));
    }

    #[test]
    fn a_heading_ends_the_block_before_it_and_is_bold() {
        let blocks = parse("text\n## Two `c`\nmore\n- item\n# One");
        assert_eq!(
            blocks,
            [
                Block::Paragraph(vec![Inline::Text("text".into())]),
                Block::Heading {
                    level: 2,
                    text: vec![Inline::Text("Two ".into()), Inline::Code("c".into())],
                },
                Block::Paragraph(vec![Inline::Text("more".into())]),
                Block::List(vec![Item {
                    depth: 0,
                    number: None,
                    text: vec![Inline::Text("item".into())],
                }]),
                Block::Heading {
                    level: 1,
                    text: vec![Inline::Text("One".into())],
                },
            ]
        );
        let lines = render("# One Two\n\n### Three", 10);
        assert_eq!(strings(&lines), padded(&["One Two", "", "Three"], 10));
        let one = &lines[0].spans[0];
        assert_eq!(one.content, "One Two");
        assert!(
            one.style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
        let three = &lines[2].spans[0];
        assert!(three.style.add_modifier.contains(Modifier::BOLD));
        assert!(!three.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn a_line_of_dashes_is_a_rule_and_ends_the_block_before_it() {
        assert_eq!(
            parse("text\n---\n- item\n  ----\n--"),
            [
                Block::Paragraph(vec![Inline::Text("text".into())]),
                Block::Rule,
                Block::List(vec![Item {
                    depth: 0,
                    number: None,
                    text: vec![Inline::Text("item".into())],
                }]),
                Block::Rule,
                Block::Paragraph(vec![Inline::Text("--".into())]),
            ]
        );
        let lines = render("---", 6);
        assert_eq!(strings(&lines), ["──────"]);
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn four_hashes_or_a_bare_hash_stay_text() {
        for text in ["#### four", "#", "# ", "#nospace"] {
            assert_eq!(
                parse(text),
                [Block::Paragraph(vec![Inline::Text(text.trim().into())])],
                "{text:?}"
            );
        }
    }

    #[test]
    fn terminal_text_drops_trailing_blanks_and_paints_only_in_color() {
        let md = "# Head\n\nsee `x`\n\n```rust\nfn f() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(
            text(md, 20, false),
            [
                "Head",
                "",
                "see x",
                "",
                "┌ rust",
                "│ fn f() {}",
                "",
                "a │ b",
                "──┼──",
                "1 │ 2",
            ]
        );
        let painted = text(md, 20, true);
        assert_eq!(painted[0], "\x1b[1;4mHead\x1b[0m");
        assert_eq!(painted[2], "see \x1b[36mx\x1b[0m");
        assert_eq!(painted[4], "\x1b[2m┌ rust\x1b[0m");
        assert_eq!(painted[5], "\x1b[2m│ \x1b[0m\x1b[34mfn\x1b[0m f() {}");
        assert_eq!(
            painted[7],
            "\x1b[1ma\x1b[0m\x1b[2m │ \x1b[0m\x1b[1mb\x1b[0m"
        );
    }

    #[test]
    fn a_lone_backtick_is_text() {
        assert_eq!(
            parse("`code` and a ` alone"),
            vec![Block::Paragraph(vec![
                Inline::Code("code".into()),
                Inline::Text(" and a ` alone".into()),
            ])]
        );
        assert_eq!(strings(&render("a ` b", 6)), padded(&["a ` b"], 6));
    }

    #[test]
    fn lines_join_into_one_paragraph_and_blank_lines_separate_blocks() {
        let text = "one\ntwo\n\n\n\nthree\n- a\n\n- b\nfour";
        assert_eq!(
            strings(&render(text, 10)),
            padded(&["one two", "", "three", "", "• a", "• b", "", "four"], 10)
        );
    }

    #[test]
    fn nested_bullets_hang_under_their_text() {
        let text = "- first item wraps here\n  - nested one wraps too\n    and continues\n- last";
        assert_eq!(
            strings(&render(text, 14)),
            padded(
                &[
                    "• first item",
                    "  wraps here",
                    "  ▪ nested one",
                    "    wraps too",
                    "    and",
                    "    continues",
                    "• last",
                ],
                14
            )
        );
        let blocks = parse(text);
        let Block::List(items) = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!(items.iter().map(|i| i.depth).collect::<Vec<_>>(), [0, 1, 0]);
    }

    #[test]
    fn a_numbered_list_after_a_bulleted_one_is_its_own_block() {
        let blocks = parse("- a\n- b\n\n1. c\n2. d\n");
        assert_eq!(blocks.len(), 2, "{blocks:#?}");
        assert!(matches!(&blocks[0], Block::List(items) if items.len() == 2));
        assert!(matches!(&blocks[1], Block::List(items) if items[0].number == Some(1)));
    }

    #[test]
    fn a_deep_item_keeps_its_text_at_a_narrow_width() {
        let text = "- a\n  - b\n    - c\n      - d\n        - hello world";
        let lines = strings(&render(text, 16));
        assert!(lines.iter().any(|l| l.contains("▪ hello")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("▪ c")), "{lines:#?}");
    }

    #[test]
    fn numbers_align_to_the_widest_sibling() {
        let text = (1..=10)
            .map(|n| format!("{n}. item {n}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n  1) sub item that wraps";
        let lines = strings(&render(&text, 16));
        assert_eq!(lines[0], format!("{:<16}", " 1. item 1"));
        assert_eq!(lines[9], format!("{:<16}", "10. item 10"));
        assert_eq!(lines[10], format!("{:<16}", "  1. sub item"));
        assert_eq!(lines[11], format!("{:<16}", "     that wraps"));
        assert_eq!(lines.len(), 12);
    }

    #[test]
    fn a_fence_with_a_language_names_it_over_the_gutter() {
        let lines = render("```rust\nfn main() {}\n\tlet x = 1;\n```\nafter", 16);
        assert_eq!(
            strings(&lines),
            [
                "┌ rust          ",
                "│ fn main() {}  ",
                "│     let x = 1;",
                "                ",
                "after           ",
            ]
        );
        assert_eq!(lines[0].spans[0].content, "┌ rust");
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(lines[1].spans[0].content, "│ ");
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
        assert!(
            lines
                .iter()
                .all(|l| l.spans.iter().all(|s| s.style.bg.is_none()))
        );
    }

    #[test]
    fn a_known_fence_highlights_and_an_unknown_one_stays_plain() {
        let lines = render("```rust\nfn main() {}\n```", 16);
        let fun = lines[1].spans.iter().find(|s| s.content == "fn").unwrap();
        assert_eq!(fun.style.fg, Some(Color::Blue));
        let lines = render("```nosuchlang\nfn main() {}\n```", 16);
        assert_eq!(lines[1].spans.len(), 2);
        assert_eq!(lines[1].spans[1].style, Style::new());
    }

    #[test]
    fn a_fence_without_a_language_cuts_long_lines() {
        let lines = render("```\nshort\na line far too long\n```", 10);
        assert_eq!(strings(&lines), ["│ short   ", "│ a line …"]);
        let cut = lines[1].spans.iter().find(|s| s.content == "…").unwrap();
        assert!(cut.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        assert_eq!(
            parse("text\n```sh\nls\n\n- not a list"),
            vec![
                Block::Paragraph(vec![Inline::Text("text".into())]),
                Block::Code {
                    lang: Some("sh".into()),
                    lines: vec!["ls".into(), "".into(), "- not a list".into()],
                },
            ]
        );
    }

    #[test]
    fn a_table_header_is_bold_and_its_rows_are_not() {
        let lines = render("| a | b |\n|---|---|\n| 1 | 2 |", 10);
        let bold = |l: &Line, s: &str| {
            l.spans
                .iter()
                .find(|x| x.content.trim() == s)
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        };
        assert!(bold(&lines[0], "a") && bold(&lines[0], "b"));
        assert!(!bold(&lines[2], "1") && !bold(&lines[2], "2"));
    }

    #[test]
    fn a_table_that_fits_takes_its_natural_widths() {
        let text = "| name | n |\n|:-----|--:|\n| ann | `12` |\nbob | 3";
        let lines = render(text, 20);
        assert_eq!(
            strings(&lines),
            padded(&["name │ n", "─────┼───", "ann  │ 12", "bob  │ 3"], 20)
        );
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_table_too_wide_shrinks_its_widest_column() {
        let text = "| key | value |\n|---|---|\n| a | the long value that has to wrap |";
        assert_eq!(
            strings(&render(text, 20)),
            [
                "key │ value         ",
                "────┼───────────────",
                "a   │ the long value",
                "    │ that has to   ",
                "    │ wrap          ",
            ]
            .map(|l| format!("{l:<20}"))
        );
    }

    #[test]
    fn a_table_too_narrow_for_its_columns_stacks() {
        let text = "| first name | last name |\n|---|---|\n| ann | lee |\n| bob | ray |";
        let lines = render(text, 16);
        assert_eq!(
            strings(&lines),
            padded(
                &[
                    "first name: ann",
                    "last name: lee",
                    "",
                    "first name: bob",
                    "last name: ray",
                ],
                16
            )
        );
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
    }
}
