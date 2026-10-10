//! A lexical highlighter for the code in a fenced block: comments,
//! strings, numbers and keywords, a line at a time.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Plain,
    Comment,
    Str,
    Number,
    Keyword,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Sh,
    Json,
    Toml,
    Yaml,
    Diff,
}

/// What a line ends inside of, carried to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    Nothing,
    BlockComment,
    Triple(char),
    Raw(usize),
}

/// One code block's highlighting state.
pub struct Lexer {
    lang: Lang,
    open: Open,
}

const RUST: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for", "while",
    "loop", "if", "else", "match", "return", "self", "Self", "where", "as", "in", "ref", "type",
    "const", "static", "unsafe", "async", "await", "move", "dyn", "break", "continue", "crate",
    "super", "true", "false",
];

const PYTHON: &[&str] = &[
    "def", "class", "return", "if", "elif", "else", "for", "while", "in", "not", "and", "or", "is",
    "None", "True", "False", "import", "from", "as", "with", "try", "except", "finally", "raise",
    "lambda", "pass", "break", "continue", "yield", "async", "await", "global",
];

const SH: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in",
    "function", "return", "local", "export", "exit",
];

const TOML: &[&str] = &["true", "false"];

const YAML: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

const JSON: &[&str] = &["true", "false", "null"];

impl Lexer {
    /// A lexer for the language a fence names, `None` for one it
    /// does not know.
    pub fn new(lang: &str) -> Option<Lexer> {
        let lang = match lang.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Lang::Rust,
            "python" | "py" => Lang::Python,
            "sh" | "bash" | "shell" | "zsh" => Lang::Sh,
            "json" => Lang::Json,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "diff" | "patch" => Lang::Diff,
            _ => return None,
        };
        Some(Lexer {
            lang,
            open: Open::Nothing,
        })
    }

    /// The classed segments of the next line of the block, which
    /// concatenate back to `line`.
    pub fn line(&mut self, line: &str) -> Vec<(String, Class)> {
        let c: Vec<char> = line.chars().collect();
        let mut out = Vec::new();
        if self.lang == Lang::Diff {
            push(&mut out, &c, diff(line));
            return out;
        }
        let mut i = 0;
        if self.open != Open::Nothing {
            let class = match self.open {
                Open::BlockComment => Class::Comment,
                _ => Class::Str,
            };
            i = self.resume(&c, 0);
            push(&mut out, &c[..i], class);
        }
        if self.lang == Lang::Yaml
            && i == 0
            && let Some((start, end)) = yaml_key(&c)
        {
            push(&mut out, &c[..start], Class::Plain);
            push(&mut out, &c[start..end], Class::Keyword);
            i = end;
        }
        while i < c.len() {
            let (end, class) = self.token(&c, i);
            push(&mut out, &c[i..end], class);
            i = end;
        }
        out
    }

    /// Where the open construct ends, scanning from `i`, closing it
    /// if it ends on this line.
    fn resume(&mut self, c: &[char], mut i: usize) -> usize {
        while i < c.len() {
            match self.open {
                Open::BlockComment if c[i..].starts_with(&['*', '/']) => {
                    self.open = Open::Nothing;
                    return i + 2;
                }
                Open::Triple(_) if c[i] == '\\' => i += 1,
                Open::Triple(q) if c[i..].starts_with(&[q, q, q]) => {
                    self.open = Open::Nothing;
                    return i + 3;
                }
                Open::Raw(hashes)
                    if c[i] == '"'
                        && c.len() > i + hashes
                        && c[i + 1..=i + hashes].iter().all(|&h| h == '#') =>
                {
                    self.open = Open::Nothing;
                    return i + 1 + hashes;
                }
                _ => {}
            }
            i += 1;
        }
        c.len()
    }

    /// The end and class of the token starting at `c[i]`.
    fn token(&mut self, c: &[char], i: usize) -> (usize, Class) {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|p| c[p]);
        let lang = self.lang;
        if lang == Lang::Rust && ch == '/' && next == Some('/') {
            return (c.len(), Class::Comment);
        }
        if lang == Lang::Rust && ch == '/' && next == Some('*') {
            self.open = Open::BlockComment;
            return (self.resume(c, i + 2), Class::Comment);
        }
        if ch == '#' && self.hash_comment(prev) {
            return (c.len(), Class::Comment);
        }
        if lang == Lang::Python && (ch == '"' || ch == '\'') && c[i..].starts_with(&[ch, ch, ch]) {
            self.open = Open::Triple(ch);
            return (self.resume(c, i + 3), Class::Str);
        }
        if ch == '"' {
            return (quoted(c, i, true), Class::Str);
        }
        if ch == '\'' {
            match lang {
                Lang::Python => return (quoted(c, i, true), Class::Str),
                Lang::Sh | Lang::Toml | Lang::Yaml => return (quoted(c, i, false), Class::Str),
                Lang::Rust => {
                    if let Some(end) = char_literal(c, i) {
                        return (end, Class::Str);
                    }
                }
                _ => {}
            }
        }
        if lang == Lang::Rust && ch == 'r' {
            let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
            if c.get(i + 1 + hashes) == Some(&'"') {
                self.open = Open::Raw(hashes);
                return (self.resume(c, i + 2 + hashes), Class::Str);
            }
        }
        let signed = matches!(lang, Lang::Json | Lang::Toml | Lang::Yaml)
            && ch == '-'
            && next.is_some_and(|n| n.is_ascii_digit());
        if (ch.is_ascii_digit() || signed) && !prev.is_some_and(|p| self.word_char(p)) {
            return (number(c, i + 1), Class::Number);
        }
        if ch.is_alphabetic() || ch == '_' {
            let end = i + c[i..].iter().take_while(|&&w| self.word_char(w)).count();
            let word: String = c[i..end].iter().collect();
            let class = if self.keywords().contains(&word.as_str()) {
                Class::Keyword
            } else {
                Class::Plain
            };
            return (end, class);
        }
        let alone = |x: Option<char>| !x.is_some_and(|x| self.word_char(x));
        if lang == Lang::Yaml && ch == '~' && alone(prev) && alone(next) {
            return (i + 1, Class::Keyword);
        }
        (i + 1, Class::Plain)
    }

    fn hash_comment(&self, prev: Option<char>) -> bool {
        match self.lang {
            Lang::Python | Lang::Toml => true,
            // a `#` inside a word is not a comment, which keeps
            // `$#` and `${#x}` out of one
            Lang::Sh | Lang::Yaml => {
                prev.is_none_or(|p| p.is_whitespace() || matches!(p, ';' | '|' | '&' | '('))
            }
            _ => false,
        }
    }

    fn word_char(&self, ch: char) -> bool {
        ch.is_alphanumeric()
            || ch == '_'
            || (ch == '-' && matches!(self.lang, Lang::Sh | Lang::Toml | Lang::Yaml))
    }

    fn keywords(&self) -> &'static [&'static str] {
        match self.lang {
            Lang::Rust => RUST,
            Lang::Python => PYTHON,
            Lang::Sh => SH,
            Lang::Json => JSON,
            Lang::Toml => TOML,
            Lang::Yaml => YAML,
            Lang::Diff => &[],
        }
    }
}

fn push(out: &mut Vec<(String, Class)>, s: &[char], class: Class) {
    if s.is_empty() {
        return;
    }
    match out.last_mut() {
        Some((t, last)) if *last == class => t.extend(s),
        _ => out.push((s.iter().collect(), class)),
    }
}

fn diff(line: &str) -> Class {
    if ["diff", "index", "---", "+++"]
        .iter()
        .any(|p| line.starts_with(p))
    {
        Class::Comment
    } else if line.starts_with("@@") {
        Class::Keyword
    } else if line.starts_with('+') {
        Class::Str
    } else if line.starts_with('-') {
        Class::Number
    } else {
        Class::Plain
    }
}

/// The end of the string opened by the quote at `c[i]`, the end of
/// the line when it does not close.
fn quoted(c: &[char], i: usize, escapes: bool) -> usize {
    let q = c[i];
    let mut j = i + 1;
    while j < c.len() {
        if escapes && c[j] == '\\' {
            j += 2;
            continue;
        }
        if c[j] == q {
            return j + 1;
        }
        j += 1;
    }
    c.len()
}

/// The end of a char literal at `c[i]`, `None` for a lifetime.
fn char_literal(c: &[char], i: usize) -> Option<usize> {
    if c.get(i + 1) == Some(&'\\') {
        let close = c.get(i + 3..)?.iter().position(|&x| x == '\'')?;
        return Some(i + 4 + close);
    }
    (c.get(i + 2) == Some(&'\'')).then_some(i + 3)
}

fn number(c: &[char], mut j: usize) -> usize {
    while j < c.len() {
        let ch = c[j];
        let exponent = matches!(ch, '+' | '-')
            && matches!(c[j - 1], 'e' | 'E')
            && c.get(j + 1).is_some_and(char::is_ascii_digit);
        let fraction = ch == '.' && c.get(j + 1).is_some_and(char::is_ascii_digit);
        if ch.is_alphanumeric() || ch == '_' || fraction || exponent {
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// The span of a mapping key at the start of a yaml line, after any
/// indent and list marker.
fn yaml_key(c: &[char]) -> Option<(usize, usize)> {
    let mut start = c.iter().take_while(|ch| ch.is_whitespace()).count();
    if c[start..].starts_with(&['-', ' ']) {
        start += 2;
        start += c[start..]
            .iter()
            .take_while(|ch| ch.is_whitespace())
            .count();
    }
    let end = (start..c.len())
        .find(|&j| c[j] == ':' && c.get(j + 1).is_none_or(|n| n.is_whitespace()))?;
    let key = &c[start..end];
    let plain = !key.is_empty() && !key.iter().any(|ch| "\"'#{}[],".contains(*ch));
    plain.then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Class::*;

    fn lex(lang: &str, lines: &[&str]) -> Vec<Vec<(String, Class)>> {
        let mut lexer = Lexer::new(lang).unwrap();
        lines
            .iter()
            .map(|l| {
                let segs = lexer.line(l);
                assert_eq!(segs.iter().map(|(s, _)| s.as_str()).collect::<String>(), *l);
                segs
            })
            .collect()
    }

    fn class_of(segs: &[(String, Class)], text: &str) -> Class {
        segs.iter()
            .find(|(s, _)| s == text)
            .unwrap_or_else(|| panic!("no {text:?} in {segs:?}"))
            .1
    }

    #[test]
    fn rust_line() {
        let segs = &lex("RS", &["let x = \"a\" + 0x1f; // done"])[0];
        assert_eq!(class_of(segs, "let"), Keyword);
        assert_eq!(class_of(segs, "\"a\""), Str);
        assert_eq!(class_of(segs, "0x1f"), Number);
        assert_eq!(class_of(segs, "// done"), Comment);
    }

    #[test]
    fn python_line() {
        let segs = &lex("py", &["def f(): return 'x', 1.5  # note"])[0];
        assert_eq!(class_of(segs, "def"), Keyword);
        assert_eq!(class_of(segs, "return"), Keyword);
        assert_eq!(class_of(segs, "'x'"), Str);
        assert_eq!(class_of(segs, "1.5"), Number);
        assert_eq!(class_of(segs, "# note"), Comment);
    }

    #[test]
    fn sh_line() {
        let segs = &lex("bash", &["if [ $# -gt 2 ]; then echo 'hi'; fi # end"])[0];
        assert_eq!(class_of(segs, "if"), Keyword);
        assert_eq!(class_of(segs, "then"), Keyword);
        assert_eq!(class_of(segs, "2"), Number);
        assert_eq!(class_of(segs, "'hi'"), Str);
        assert_eq!(class_of(segs, "# end"), Comment);
    }

    #[test]
    fn json_line() {
        let segs = &lex("json", &["{\"a\": -12, \"b\": true, \"c\": \"#x\"}"])[0];
        assert_eq!(class_of(segs, "\"a\""), Str);
        assert_eq!(class_of(segs, "-12"), Number);
        assert_eq!(class_of(segs, "true"), Keyword);
        assert_eq!(class_of(segs, "\"#x\""), Str);
        assert!(segs.iter().all(|(_, c)| *c != Comment));
    }

    #[test]
    fn toml_line() {
        let segs = &lex("toml", &["on = true  # key", "n = 1_000", "s = 'lit'"]);
        assert_eq!(class_of(&segs[0], "true"), Keyword);
        assert_eq!(class_of(&segs[0], "# key"), Comment);
        assert_eq!(class_of(&segs[1], "1_000"), Number);
        assert_eq!(class_of(&segs[2], "'lit'"), Str);
    }

    #[test]
    fn yaml_line() {
        let segs = &lex("yml", &["  - name: \"x\" # c", "on: 3", "v: ~"]);
        assert_eq!(class_of(&segs[0], "name"), Keyword);
        assert_eq!(class_of(&segs[0], "\"x\""), Str);
        assert_eq!(class_of(&segs[0], "# c"), Comment);
        assert_eq!(class_of(&segs[1], "on"), Keyword);
        assert_eq!(class_of(&segs[1], "3"), Number);
        assert_eq!(class_of(&segs[2], "~"), Keyword);
    }

    #[test]
    fn diff_lines_are_one_segment_each() {
        let lines = [
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -1 +1 @@",
            "-old",
            "+new",
            " same",
        ];
        let classes: Vec<Vec<Class>> = lex("patch", &lines)
            .iter()
            .map(|segs| segs.iter().map(|s| s.1).collect())
            .collect();
        assert_eq!(
            classes,
            [
                vec![Comment],
                vec![Comment],
                vec![Comment],
                vec![Keyword],
                vec![Number],
                vec![Str],
                vec![Plain],
            ]
        );
    }

    #[test]
    fn a_rust_block_comment_runs_across_lines() {
        let segs = lex("rust", &["fn a() /* one", "two */ let"]);
        assert_eq!(class_of(&segs[0], "/* one"), Comment);
        assert_eq!(class_of(&segs[1], "two */"), Comment);
        assert_eq!(class_of(&segs[1], "let"), Keyword);
    }

    #[test]
    fn a_rust_raw_string_runs_across_lines() {
        let segs = lex("rust", &["let s = r#\"a \"quote\"", "b\"# ;"]);
        assert_eq!(class_of(&segs[0], "r#\"a \"quote\""), Str);
        assert_eq!(class_of(&segs[1], "b\"#"), Str);
    }

    #[test]
    fn a_rust_char_literal_quote_does_not_open_a_string() {
        let segs = &lex("rust", &["let q = '\"'; 'a: loop {}"])[0];
        assert_eq!(class_of(segs, "'\"'"), Str);
        assert_eq!(class_of(segs, "loop"), Keyword);
    }

    #[test]
    fn a_python_triple_quote_runs_across_lines() {
        let segs = lex("python", &["x = \"\"\"one", "# two", "three\"\"\" if"]);
        assert_eq!(class_of(&segs[0], "\"\"\"one"), Str);
        assert_eq!(segs[1], [("# two".to_string(), Str)]);
        assert_eq!(class_of(&segs[2], "three\"\"\""), Str);
        assert_eq!(class_of(&segs[2], "if"), Keyword);
    }

    #[test]
    fn dollar_hash_in_sh_is_not_a_comment() {
        let segs = &lex("sh", &["echo $# ${#a}"])[0];
        assert!(segs.iter().all(|(_, c)| *c != Comment), "{segs:?}");
    }

    #[test]
    fn a_comment_marker_in_a_string_is_text() {
        let segs = &lex("rust", &["\"// no\" \"a\\\"#\""])[0];
        assert!(segs.iter().all(|(_, c)| *c != Comment), "{segs:?}");
        let segs = &lex("python", &["'# no'"])[0];
        assert_eq!(segs, &[("'# no'".to_string(), Str)]);
    }

    #[test]
    fn a_digit_glued_to_a_word_is_plain() {
        let segs = &lex("rust", &["x1 = y_2"])[0];
        assert!(segs.iter().all(|(_, c)| *c == Plain), "{segs:?}");
        let segs = &lex("yaml", &["- a-1"])[0];
        assert!(segs.iter().all(|(_, c)| *c == Plain), "{segs:?}");
    }

    #[test]
    fn an_unknown_language_has_no_lexer() {
        assert!(Lexer::new("nosuchlang").is_none());
    }
}
