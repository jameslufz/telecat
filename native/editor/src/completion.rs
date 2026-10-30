use crate::syntax::{Kind, inventory, lex};
use std::{collections::BTreeMap, ops::Range, sync::Arc};
use telecat_core::types::Driver;
pub type Schema = BTreeMap<String, Vec<String>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Table,
    Column,
    Type,
    Function,
}
impl CompletionKind {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Keyword => "K",
            Self::Table | Self::Type => "T",
            Self::Column => "C",
            Self::Function => "F",
        }
    }
    pub fn color(self) -> u32 {
        match self {
            Self::Keyword => 0x2f6fed,
            Self::Table | Self::Type => 0x3a7d44,
            Self::Column => 0x9a7a45,
            Self::Function => 0x8e44ad,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Completion {
    pub label: Arc<str>,
    pub kind: CompletionKind,
    key: String,
    pub table: Option<Arc<str>>,
}
#[derive(Clone)]
pub struct CompletionIndex {
    items: Arc<[Completion]>,
    driver: Driver,
}
#[derive(Clone, Debug)]
pub struct CompletionResult {
    pub replace: Range<usize>,
    pub items: Vec<Completion>,
}
impl CompletionIndex {
    /// Rebuild when schema/driver changes, never while rendering a keystroke.
    pub fn new(driver: Driver, schema: &Schema) -> Self {
        let inv = inventory(driver);
        let mut items = vec![];
        let mut push = |label: &str, kind, table: Option<Arc<str>>| {
            items.push(Completion {
                label: Arc::from(label),
                kind,
                key: label.to_lowercase(),
                table,
            })
        };
        for k in &inv.keywords {
            push(k, CompletionKind::Keyword, None);
        }
        for k in &inv.types {
            push(k, CompletionKind::Type, None);
        }
        for k in &inv.builtin {
            push(k, CompletionKind::Function, None);
        }
        for (table, cols) in schema {
            push(table, CompletionKind::Table, None);
            for col in cols {
                push(col, CompletionKind::Column, Some(Arc::from(table.as_str())));
            }
        }
        items.sort_by(|a, b| a.key.cmp(&b.key).then(a.table.cmp(&b.table)));
        items.dedup_by(|a, b| a.key == b.key && a.table == b.table);
        Self {
            items: items.into(),
            driver,
        }
    }
    /// O(log(schema) + returned matches), bounded to 100, no font or IO calls.
    pub fn query(&self, text: &str, cursor: usize, explicit: bool) -> CompletionResult {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let start = text[..cursor]
            .char_indices()
            .rev()
            .find(|(_, c)| {
                !(c.is_alphanumeric()
                    || *c == '_'
                    || *c == '$'
                    || ('\u{0e31}'..='\u{0e4e}').contains(c))
            })
            .map_or(0, |(i, c)| i + c.len_utf8());
        let query = text[start..cursor].to_lowercase();
        let mut items = vec![];
        if query.is_empty() && !explicit {
            return CompletionResult {
                replace: start..cursor,
                items,
            };
        }
        let table = if start > 0 && text.as_bytes()[start - 1] == b'.' {
            let end = start - 1;
            let begin = text[..end]
                .char_indices()
                .rev()
                .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
                .map_or(0, |(i, c)| i + c.len_utf8());
            Some(&text[begin..end])
        } else {
            None
        };
        let table = table.map(|t| self.resolve_alias(text, t));
        // Unqualified: columns of the tables this statement uses come first.
        let referenced = if table.is_none() {
            referenced_tables(statement_around(text, cursor), self.driver)
        } else {
            vec![]
        };
        let begin = self.items.partition_point(|item| item.key < query);
        let mut preferred = vec![];
        for item in &self.items[begin..] {
            if !item.key.starts_with(&query) {
                break;
            }
            if table
                .as_deref()
                .is_some_and(|t| item.table.as_deref().is_none_or(|it| !same_table(it, t)))
            {
                continue;
            }
            let first = item
                .table
                .as_deref()
                .is_some_and(|it| referenced.iter().any(|r| same_table(it, r)));
            // The other matches stop at 100; the statement's own columns are looked for
            // through the whole prefix range, so many similar names can't push them out.
            let list = if first { &mut preferred } else { &mut items };
            if list.len() >= 100
                || list
                    .last()
                    .is_some_and(|last: &Completion| last.label == item.label)
            {
                if referenced.is_empty() && items.len() >= 100 {
                    break;
                }
                continue;
            }
            list.push(item.clone());
        }
        // A preferred column may also be listed for another table further down.
        items.retain(|i| !preferred.iter().any(|p| p.label == i.label));
        preferred.extend(items);
        preferred.truncate(100);
        CompletionResult {
            replace: start..cursor,
            items: preferred,
        }
    }
    /// The table an alias (`FROM users u … u.`) stands for, from the statement at the caret
    /// first and then the whole document; a name that is no alias is returned as it is.
    fn resolve_alias(&self, text: &str, qualifier: &str) -> String {
        table_aliases(text, self.driver)
            .into_iter()
            .find(|(_, alias)| {
                alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(qualifier))
            })
            .map_or_else(|| qualifier.to_string(), |(table, _)| table)
    }
    pub fn allowed(text: &str, cursor: usize, driver: Driver) -> bool {
        !lex(text, driver).iter().any(|t| {
            t.range.start < cursor
                && cursor <= t.range.end
                && matches!(t.kind, Kind::String | Kind::Comment)
        })
    }
}
/// A schema key (`table` or `schema.table`) names `table` (with or without its schema).
fn same_table(key: &str, table: &str) -> bool {
    key.eq_ignore_ascii_case(table)
        || key
            .rsplit_once('.')
            .is_some_and(|(_, name)| name.eq_ignore_ascii_case(table))
}
/// The statement the byte offset `cursor` is in: the text between the semicolons around it.
pub fn statement_around(text: &str, cursor: usize) -> &str {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = text[..cursor].rfind(';').map_or(0, |i| i + 1);
    let end = text[cursor..].find(';').map_or(text.len(), |i| cursor + i);
    &text[start..end]
}
/// Tables a statement reads or writes (see [`table_aliases`]), each once.
pub fn referenced_tables(statement: &str, driver: Driver) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (table, _) in table_aliases(statement, driver) {
        if !out.iter().any(|t| t.eq_ignore_ascii_case(&table)) {
            out.push(table);
        }
    }
    out
}
/// Tables named after FROM / JOIN / UPDATE / INTO, comma-separated FROM lists included, with
/// their aliases (`users u`, `users AS u`), without schema or quotes. Comments and strings are
/// skipped.
pub fn table_aliases(statement: &str, driver: Driver) -> Vec<(String, Option<String>)> {
    // Quoted identifiers lex as strings; only real string literals are skipped.
    let tokens: Vec<&str> = lex(statement, driver)
        .into_iter()
        .filter(|t| !matches!(t.kind, Kind::Space | Kind::Comment))
        .map(|t| &statement[t.range])
        .filter(|t| !t.starts_with('\'') && !t.starts_with("E'") && !t.starts_with('$'))
        .collect();
    let unquote = |s: &str| s.trim_matches(['`', '"']).to_string();
    let ident = |s: &str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '`' || c == '"')
    };
    let mut out = vec![];
    let mut i = 0;
    while i < tokens.len() {
        let introduces = ["FROM", "JOIN", "UPDATE", "INTO"]
            .iter()
            .any(|k| tokens[i].eq_ignore_ascii_case(k));
        i += 1;
        if !introduces {
            continue;
        }
        while let Some(&name) = tokens.get(i).filter(|t| ident(t) && !is_clause(t)) {
            let mut table = name;
            i += 1;
            if tokens.get(i) == Some(&".")
                && let Some(&n) = tokens.get(i + 1).filter(|t| ident(t))
            {
                table = n;
                i += 2;
            }
            if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("AS")) {
                i += 1;
            }
            let alias = tokens
                .get(i)
                .filter(|t| ident(t) && !is_clause(t))
                .map(|t| unquote(t));
            if alias.is_some() {
                i += 1;
            }
            out.push((unquote(table), alias));
            if tokens.get(i) == Some(&",") {
                i += 1;
            } else {
                break;
            }
        }
    }
    out
}
fn is_clause(word: &str) -> bool {
    [
        "WHERE",
        "JOIN",
        "LEFT",
        "RIGHT",
        "INNER",
        "OUTER",
        "CROSS",
        "FULL",
        "ON",
        "USING",
        "GROUP",
        "ORDER",
        "LIMIT",
        "SET",
        "VALUES",
        "SELECT",
        "HAVING",
        "UNION",
        "WINDOW",
        "NATURAL",
        "STRAIGHT_JOIN",
        "RETURNING",
        "OFFSET",
        "FOR",
        "INTO",
    ]
    .iter()
    .any(|k| word.eq_ignore_ascii_case(k))
}
