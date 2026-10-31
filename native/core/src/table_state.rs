//! What a table tab remembers between launches (its filter rules and its columns' widths and
//! order), and the view a Go Back / Go Forward step returns to.
use crate::types::{Combine, Filter, OrderBy, TableRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The narrowest and widest a remembered column may be.
pub const MIN_WIDTH: f32 = 40.;
pub const MAX_WIDTH: f32 = 2000.;
/// At most this many tables are remembered; the least recently used are forgotten first.
pub const MAX_TABLES: usize = 500;
/// At most this many filter rules are remembered per table.
const MAX_RULES: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedTable {
    pub filters: Vec<Filter>,
    pub combine: Combine,
    pub show_filters: bool,
    /// Column name → width in points.
    pub widths: BTreeMap<String, f32>,
    /// Column names in the order the user arranged them; empty for the table's own order.
    pub order: Vec<String>,
    /// When it was last saved (seconds since 1970), to forget the oldest tables first.
    pub used: u64,
}
impl SavedTable {
    /// The saved state made safe for the table's current `columns`: the file can be edited by
    /// hand or come from an older table, so unknown columns are dropped, widths clamped and the
    /// order completed with columns added since.
    pub fn sanitized(mut self, columns: &[String]) -> Self {
        self.widths
            .retain(|name, width| columns.contains(name) && width.is_finite());
        for width in self.widths.values_mut() {
            *width = width.clamp(MIN_WIDTH, MAX_WIDTH);
        }
        self.order = column_order(&self.order, columns);
        self.filters.truncate(MAX_RULES);
        self.filters.retain(|f| {
            f.column.is_empty() || f.column == crate::types::RAW_SQL || columns.contains(&f.column)
        });
        self
    }
}
/// `columns` arranged by `saved`: the saved names that still exist, in their saved order, then
/// any other columns in the table's order. Empty when that is just the table's order.
pub fn column_order(saved: &[String], columns: &[String]) -> Vec<String> {
    let mut order: Vec<String> = vec![];
    for name in saved {
        if columns.contains(name) && !order.contains(name) {
            order.push(name.clone());
        }
    }
    for name in columns {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    if order == columns { vec![] } else { order }
}
/// The key a table's saved state is stored under, or `None` when it must not be stored: a
/// connection that isn't saved (opened from a URL) has no stable identity.
pub fn table_key(connection_id: &str, database: &str, table: &TableRef) -> Option<String> {
    if connection_id.trim().is_empty() {
        return None;
    }
    Some(
        [connection_id, database, &table.schema, &table.name]
            .map(|part| part.replace('\\', "\\\\").replace('/', "\\/"))
            .join("/"),
    )
}
/// Keeps at most `MAX_TABLES` entries, forgetting the least recently used.
pub fn prune(tables: &mut BTreeMap<String, SavedTable>) {
    if tables.len() <= MAX_TABLES {
        return;
    }
    let mut by_age: Vec<(u64, String)> = tables.iter().map(|(k, v)| (v.used, k.clone())).collect();
    by_age.sort();
    for (_, key) in by_age.into_iter().take(tables.len() - MAX_TABLES) {
        tables.remove(&key);
    }
}
/// What a table tab shows: the view Go Back returns to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableView {
    pub filters: Vec<Filter>,
    pub applied: Vec<Filter>,
    pub combine: Combine,
    pub show_filters: bool,
    pub order_by: Vec<OrderBy>,
    pub offset: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FilterOp, RAW_SQL};
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn filter(column: &str) -> Filter {
        Filter {
            enabled: true,
            column: column.into(),
            op: FilterOp::Eq,
            value: "1".into(),
            value2: None,
        }
    }
    #[test]
    fn saved_state_is_made_safe_for_the_current_columns() {
        let saved = SavedTable {
            filters: vec![filter("gone"), filter("a"), filter(""), filter(RAW_SQL)],
            widths: [("a", 10.), ("b", 9e9), ("gone", 100.), ("c", f32::NAN)]
                .map(|(k, v)| (k.to_owned(), v))
                .into(),
            order: names(&["b", "gone", "a", "b"]),
            ..Default::default()
        };
        let s = saved.sanitized(&names(&["a", "b", "c"]));
        assert_eq!(
            s.widths,
            [("a".to_owned(), MIN_WIDTH), ("b".to_owned(), MAX_WIDTH)].into()
        );
        assert_eq!(s.order, names(&["b", "a", "c"]));
        assert_eq!(s.filters.len(), 3);
        assert!(s.filters.iter().all(|f| f.column != "gone"));
    }
    #[test]
    fn order_equal_to_the_table_is_stored_as_none() {
        assert!(column_order(&names(&["a", "b"]), &names(&["a", "b"])).is_empty());
        assert_eq!(column_order(&[], &names(&["a"])), Vec::<String>::new());
        assert_eq!(
            column_order(&names(&["c", "x"]), &names(&["a", "b", "c"])),
            names(&["c", "a", "b"])
        );
    }
    #[test]
    fn keys_need_a_saved_connection_and_cannot_collide() {
        let t = |s: &str, n: &str| TableRef {
            schema: s.into(),
            name: n.into(),
        };
        assert_eq!(table_key("", "db", &t("s", "t")), None);
        assert_eq!(
            table_key("id", "db", &t("s", "t")).as_deref(),
            Some("id/db/s/t")
        );
        assert_ne!(
            table_key("id", "db", &t("s/x", "t")),
            table_key("id", "db", &t("s", "x/t"))
        );
    }
    #[test]
    fn deserialises_partial_and_unknown_fields() {
        let s: SavedTable = serde_json::from_str(r#"{"showFilters":true,"extra":1}"#).unwrap();
        assert!(s.show_filters && s.filters.is_empty());
    }
    #[test]
    fn prune_forgets_the_least_recently_used() {
        let mut all: BTreeMap<String, SavedTable> = (0..MAX_TABLES as u64 + 3)
            .map(|i| {
                (
                    format!("k{i}"),
                    SavedTable {
                        used: i,
                        ..Default::default()
                    },
                )
            })
            .collect();
        prune(&mut all);
        assert_eq!(all.len(), MAX_TABLES);
        assert!(!all.contains_key("k0") && !all.contains_key("k2") && all.contains_key("k3"));
    }
}
