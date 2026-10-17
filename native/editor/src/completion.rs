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
        let begin = self.items.partition_point(|item| item.key < query);
        for item in &self.items[begin..] {
            if !item.key.starts_with(&query) {
                break;
            }
            if table.as_deref().is_some_and(|t| {
                item.table
                    .as_deref()
                    .is_none_or(|it| !it.eq_ignore_ascii_case(t))
            }) {
                continue;
            }
            if items
                .last()
                .is_some_and(|last: &Completion| last.label == item.label)
            {
                continue;
            }
            items.push(item.clone());
            if items.len() == 100 {
                break;
            }
        }
        CompletionResult {
            replace: start..cursor,
            items,
        }
    }
    /// Resolve common FROM/JOIN aliases using the complete document snapshot.
    /// This runs only in the completion worker, including text after the caret.
    fn resolve_alias(&self, text: &str, qualifier: &str) -> String {
        let tokens: Vec<_> = lex(text, self.driver)
            .into_iter()
            .filter(|t| !matches!(t.kind, Kind::Space | Kind::Comment))
            .map(|t| &text[t.range])
            .collect();
        for i in 0..tokens.len() {
            if !tokens[i].eq_ignore_ascii_case("FROM") && !tokens[i].eq_ignore_ascii_case("JOIN") {
                continue;
            }
            let Some(&table) = tokens.get(i + 1) else {
                continue;
            };
            let mut alias = i + 2;
            let mut table = table;
            if tokens.get(alias) == Some(&".") {
                if let Some(name) = tokens.get(alias + 1) {
                    table = name;
                    alias += 2;
                }
            }
            if tokens
                .get(alias)
                .is_some_and(|s| s.eq_ignore_ascii_case("AS"))
            {
                alias += 1;
            }
            if tokens
                .get(alias)
                .is_some_and(|s| s.trim_matches(['`', '"']).eq_ignore_ascii_case(qualifier))
            {
                return table.trim_matches(['`', '"']).to_string();
            }
        }
        qualifier.to_string()
    }
    pub fn allowed(text: &str, cursor: usize, driver: Driver) -> bool {
        !lex(text, driver).iter().any(|t| {
            t.range.start < cursor
                && cursor <= t.range.end
                && matches!(t.kind, Kind::String | Kind::Comment)
        })
    }
}
