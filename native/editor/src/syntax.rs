//! CodeMirror SQL's lexical categories, using its actual dialect inventories.
//! No tree-sitter/stock theme or Unicode completion icons on the input path.
use gpui_kit::*;
use std::{
    collections::HashSet,
    ops::Range,
    rc::Rc,
    sync::{Arc, OnceLock},
};
use telecat_core::types::Driver;
use telecat_editor_engine::input::language_config::LanguageConfig;
use telecat_editor_engine::input::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyword,
    String,
    Number,
    Comment,
    Operator,
    Type,
    Special,
    Name,
    Plain,
    Space,
}
impl Kind {
    pub fn color(self) -> u32 {
        match self {
            Self::Keyword => 0x5fa8ff,
            Self::String => 0xf0a35e,
            Self::Number => 0xc792ea,
            Self::Comment => 0x6a9955,
            Self::Operator => 0xd4d4d4,
            Self::Type => 0x4ec9b0,
            Self::Special => 0x9cdcfe,
            Self::Plain | Self::Space => 0xe8e8e8,
            _ => 0xe6e6e6,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: Kind,
}
pub struct Inventory {
    pub keywords: HashSet<String>,
    pub types: HashSet<String>,
    pub builtin: HashSet<String>,
}
pub fn inventory(driver: Driver) -> &'static Inventory {
    static MYSQL: OnceLock<Inventory> = OnceLock::new();
    static PG: OnceLock<Inventory> = OnceLock::new();
    let (slot, data) = if driver == Driver::Mysql {
        (&MYSQL, include_str!("mysql.json"))
    } else {
        (&PG, include_str!("postgresql.json"))
    };
    slot.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(data).unwrap();
        let get = |k: &str| {
            v[k].as_str()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_ascii_uppercase)
                .collect()
        };
        Inventory {
            keywords: get("keywords"),
            types: get("types"),
            builtin: get("builtin"),
        }
    })
}
pub fn lex(text: &str, driver: Driver) -> Vec<Token> {
    let inv = inventory(driver);
    let mut result = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        let start = i;
        let c = text[i..].chars().next().unwrap();
        let kind;
        let charset_prefix = if c == '_' {
            let len = text[i..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            (len > 2 && bytes.get(i + len) == Some(&b'\'')).then_some(len)
        } else {
            None
        };
        if c.is_whitespace() {
            i += c.len_utf8();
            while i < bytes.len() && text[i..].chars().next().unwrap().is_whitespace() {
                i += text[i..].chars().next().unwrap().len_utf8();
            }
            kind = Kind::Space;
        } else if (text[i..].starts_with("--")
            && (driver != Driver::Mysql
                || bytes.get(i + 2).is_none_or(|b| b.is_ascii_whitespace())))
            || (c == '#' && driver == Driver::Mysql)
        {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            kind = Kind::Comment;
        } else if text[i..].starts_with("/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() {
                if text[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else if driver != Driver::Mysql && text[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else {
                    i += text[i..].chars().next().unwrap().len_utf8();
                }
            }
            kind = Kind::Comment;
        } else if charset_prefix.is_some()
            || c == '\''
            || c == '"'
            || c == '`' && driver == Driver::Mysql
            || matches!(c, 'E' | 'e' | 'N' | 'n' | 'X' | 'x' | 'B' | 'b')
                && bytes.get(i + 1) == Some(&b'\'')
        {
            let prefix = c != '\'' && c != '"' && c != '`';
            let quote = if prefix { '\'' } else { c };
            i += charset_prefix.map_or(if prefix { 2 } else { 1 }, |n| n + 1);
            while i < bytes.len() {
                let n = text[i..].chars().next().unwrap();
                i += n.len_utf8();
                if n == quote {
                    if text[i..].starts_with(quote) {
                        i += quote.len_utf8();
                    } else {
                        break;
                    }
                } else if n == '\\'
                    && i < bytes.len()
                    && (driver == Driver::Mysql || prefix && matches!(c, 'E' | 'e'))
                {
                    i += text[i..].chars().next().unwrap().len_utf8();
                }
            }
            kind = if prefix && matches!(c, 'X' | 'x' | 'B' | 'b') {
                Kind::Number
            } else {
                Kind::String
            };
        } else if c == '$' && driver != Driver::Mysql && dollar_tag(&text[i..]).is_some() {
            let tag = dollar_tag(&text[i..]).unwrap();
            i += tag.len();
            i = text[i..]
                .find(tag)
                .map_or(bytes.len(), |n| i + n + tag.len());
            kind = Kind::String;
        } else if c.is_ascii_digit() || c == '.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'.'
                    || (bytes[i] == b'-' || bytes[i] == b'+')
                        && matches!(bytes[i - 1], b'e' | b'E'))
            {
                i += 1;
            }
            kind = Kind::Number;
        } else if c.is_alphabetic() || c == '_' || (c == '@' || c == '?') && driver == Driver::Mysql
        {
            i += c.len_utf8();
            while i < bytes.len() {
                let n = text[i..].chars().next().unwrap();
                if n.is_alphanumeric()
                    || n == '_'
                    || n == '$'
                    || n == '@'
                    || ('\u{0e31}'..='\u{0e4e}').contains(&n)
                {
                    i += n.len_utf8();
                } else {
                    break;
                }
            }
            let upper = text[start..i].to_ascii_uppercase();
            kind = if bytes.get(i) == Some(&b'.') || start > 0 && bytes[start - 1] == b'.' {
                Kind::Name
            } else if matches!(upper.as_str(), "TRUE" | "FALSE" | "NULL") {
                Kind::Number
            } else if inv.types.contains(&upper) || inv.builtin.contains(&upper) {
                Kind::Type
            } else if inv.keywords.contains(&upper) {
                Kind::Keyword
            } else if c == '@' || c == '?' {
                Kind::Special
            } else {
                Kind::Name
            };
        } else {
            i += c.len_utf8();
            let operators = if driver == Driver::Mysql {
                "*+-%<>!=&|^()[]{}:,"
            } else {
                "+-*/<>=~!@#%^&|`?()[]{}:,"
            };
            kind = if operators.contains(c) {
                Kind::Operator
            } else {
                Kind::Plain
            };
        }
        result.push(Token {
            range: start..i,
            kind,
        });
    }
    result
}
fn dollar_tag(s: &str) -> Option<&str> {
    let end = s[1..].find('$')? + 1;
    s[1..end]
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        .then_some(&s[..=end])
}
#[derive(Clone)]
pub struct Syntax {
    pub driver: Driver,
    pub text: Arc<str>,
    pub tokens: Vec<Token>,
}
impl Syntax {
    pub fn new(driver: Driver) -> Self {
        Self {
            driver,
            text: Arc::from(""),
            tokens: vec![],
        }
    }
    pub fn update_text(&mut self, text: &str) {
        self.tokens = lex(text, self.driver);
        self.text = Arc::from(text);
    }
    pub fn context(&self, offset: usize) -> SyntaxContext {
        self.tokens
            .iter()
            .find(|t| t.range.start < offset && offset <= t.range.end)
            .map_or(SyntaxContext::Code, |t| match t.kind {
                Kind::String => SyntaxContext::String,
                Kind::Comment => SyntaxContext::Comment,
                _ => SyntaxContext::Code,
            })
    }
}
impl InputHighlighter for Syntax {
    fn language(&self) -> SharedString {
        "sql".into()
    }
    fn update(
        &mut self,
        _: Option<InputEdit>,
        text: &Rope,
        _: bool,
        _: &mut Window,
        _: &mut Context<EditorState>,
    ) {
        self.update_text(&text.to_string());
    }
    fn update_batch(
        &mut self,
        edits: &[(InputEdit, Rope)],
        _: bool,
        _: &mut Window,
        _: &mut Context<EditorState>,
    ) {
        if let Some((_, text)) = edits.last() {
            self.update_text(&text.to_string());
        }
    }
    fn styles(
        &self,
        range: &Range<usize>,
        _: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out = vec![];
        let mut at = range.start;
        let begin = self.tokens.partition_point(|t| t.range.end <= range.start);
        for token in &self.tokens[begin..] {
            if token.range.start >= range.end {
                break;
            }
            let r = token.range.start.max(range.start)..token.range.end.min(range.end);
            if at < r.start {
                out.push((at..r.start, HighlightStyle::default()));
            }
            at = r.end;
            out.push((
                r,
                HighlightStyle {
                    color: Some(rgb(token.kind.color()).into()),
                    ..Default::default()
                },
            ));
        }
        if at < range.end {
            out.push((at..range.end, HighlightStyle::default()));
        }
        out
    }
    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        vec![]
    }
}
pub struct Languages;
impl LanguageProvider for Languages {
    fn config(&self, _: &str) -> Rc<LanguageConfig> {
        let mut config = LanguageConfig::default();
        config.auto_closing_pairs.as_mut().unwrap().push(
            AutoClosingPair::new("`", "`").not_in([SyntaxContext::String, SyntaxContext::Comment]),
        );
        Rc::new(config)
    }

    fn syntax_context_provider(&self, name: &str) -> Option<Rc<dyn SyntaxContextProvider>> {
        Some(Rc::new(LexicalContext(if name == "mysql" {
            Driver::Mysql
        } else {
            Driver::Postgres
        })))
    }
}
struct LexicalContext(Driver);
impl SyntaxContextProvider for LexicalContext {
    fn context_at(&self, text: &Rope, offset: usize) -> SyntaxContext {
        let value = text.to_string();
        lex(&value, self.0)
            .iter()
            .find(|t| t.range.start < offset && offset <= t.range.end)
            .map_or(SyntaxContext::Code, |t| match t.kind {
                Kind::String => SyntaxContext::String,
                Kind::Comment => SyntaxContext::Comment,
                _ => SyntaxContext::Code,
            })
    }
}

/// Matching and mismatching brackets, ignoring quoted values/identifiers and comments.
pub fn brackets(text: &str, cursor: usize, driver: Driver) -> Vec<(Range<usize>, bool)> {
    let tokens = lex(text, driver);
    let braces: Vec<_> = tokens
        .iter()
        .filter(|t| t.kind == Kind::Operator)
        .filter_map(|t| {
            let b = text.as_bytes()[t.range.start];
            b"()[]{}".contains(&b).then_some((t.range.start, b))
        })
        .collect();
    let at = braces
        .iter()
        .position(|(i, _)| *i == cursor)
        .or_else(|| braces.iter().position(|(i, _)| i + 1 == cursor));
    let Some(at) = at else {
        return vec![];
    };
    let (offset, b) = braces[at];
    let forward = b"([{".contains(&b);
    let mate = |b| match b {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b')' => b'(',
        b']' => b'[',
        _ => b'{',
    };
    let indices: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new(at + 1..braces.len())
    } else {
        Box::new((0..at).rev())
    };
    let mut stack = vec![b];
    for i in indices {
        let (other, c) = braces[i];
        if offset.abs_diff(other) > 10000 {
            break;
        }
        if b"([{".contains(&c) == forward {
            stack.push(c);
        } else {
            let expected = mate(stack.pop().unwrap());
            if c != expected {
                return vec![(offset..offset + 1, false), (other..other + 1, false)];
            }
            if stack.is_empty() {
                return vec![(offset..offset + 1, true), (other..other + 1, true)];
            }
        }
    }
    vec![(offset..offset + 1, false)]
}
