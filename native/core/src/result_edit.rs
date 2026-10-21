//! Editing a SQL query's result set in place.
//!
//! Which cells are editable is decided from the server's column metadata (see
//! [`ColumnOrigin`](crate::types::ColumnOrigin)) checked against the catalog, never from the SQL
//! text. A row of a source table is identified only by that table's complete primary or unique
//! NOT NULL key, read unmodified from the result: never by position, ctid or fingerprint. Every
//! statement must match exactly one row, inside one transaction, or nothing is saved.
use crate::changes::{
    Cell, EditRow, IdentityKind, Params, Stmt, cell_modified, qualified, quote_ident,
};
use crate::types::{Driver, TableRef};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};

/// A source table instance of a result, as resolved from the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceTable {
    pub table: TableRef,
    /// Why rows can't be written here at all ("view", …); `None` for a base table.
    pub not_a_table: Option<String>,
    /// The primary key, or else a unique key whose columns are all NOT NULL; empty if none.
    pub key: Vec<String>,
    /// MySQL: how each key column compares exactly. Empty for PostgreSQL.
    pub key_kinds: Vec<IdentityKind>,
    /// Columns that can't take a text value: generated, binary and bit columns.
    pub read_only_columns: BTreeSet<String>,
    /// Shown more than once in the query (a self-join): its columns can't be told apart.
    pub repeated: bool,
    /// MySQL: every column as the catalog lists it (name → type and generation), so a plan made
    /// again later notices a column renamed, dropped, retyped or made generated. Empty for
    /// PostgreSQL, whose origins (table OID, attribute number) already change then.
    pub columns: BTreeMap<String, String>,
}
/// A result's columns mapped onto their source tables.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultSources {
    pub tables: Vec<SourceTable>,
    /// For each result column, the table instance and base column it shows unmodified.
    pub columns: Vec<Option<(usize, String)>>,
}
/// What can be changed in one result set, and how its rows are identified.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultEditPlan {
    pub driver: Driver,
    pub sources: ResultSources,
    /// For each source table: `Ok(result columns holding its key, in key order)` or why its
    /// columns are read-only.
    pub tables: Vec<Result<Vec<usize>, String>>,
    pub editable: Vec<bool>,
    /// The table whose rows Delete removes, or why rows can't be deleted.
    pub delete: Result<usize, String>,
    /// Why nothing at all can be changed.
    pub read_only: Option<String>,
    /// The settings of the session the query ran in that decide how typed values are read
    /// (time zone, date style, …). The commit applies them so a value means the same there.
    pub session: Vec<(String, String)>,
}
const COMPUTED: &str = "computed column (expression, alias of an expression or aggregate)";
const KEY_COLUMN: &str = "key column: it identifies the row";
fn exact_key_kind(k: IdentityKind) -> bool {
    matches!(
        k,
        IdentityKind::Signed | IdentityKind::Unsigned | IdentityKind::Decimal | IdentityKind::Other
    )
}
impl ResultEditPlan {
    /// Why edits staged under `old` can't be saved under this plan, made again for the same
    /// result on another session (after a reconnect); `None` when nothing they rely on changed.
    pub fn drift_from(&self, old: &ResultEditPlan) -> Option<String> {
        // Nothing could be resolved at all (the query no longer runs): that is the reason.
        if self.read_only.is_some() && self.sources.columns.iter().all(Option::is_none) {
            return self.read_only.clone();
        }
        for (c, origin) in old.sources.columns.iter().enumerate() {
            let Some((ti, name)) = origin else { continue };
            let Some(was) = old.sources.tables.get(*ti) else {
                continue;
            };
            let table = &was.table.name;
            let Some(Some((nti, now_name))) = self.sources.columns.get(c) else {
                return Some(format!("column {name} of {table} no longer exists"));
            };
            let Some(now) = self.sources.tables.get(*nti) else {
                return Some(format!("column {name} of {table} no longer exists"));
            };
            if now_name != name || now.table != was.table {
                return Some(format!("column {name} of {table} now shows another column"));
            }
            if now.columns.get(name) != was.columns.get(name) {
                return Some(format!(
                    "column {name} of {table} changed (now {}, was {})",
                    now.columns.get(name).map_or("missing", String::as_str),
                    was.columns.get(name).map_or("missing", String::as_str)
                ));
            }
        }
        if let Some(reason) = &self.read_only {
            return Some(reason.clone());
        }
        if self.sources != old.sources
            || self.tables != old.tables
            || self.editable != old.editable
            || self.delete != old.delete
        {
            return Some(
                "the tables behind them changed (their keys, generated columns or the table itself)".into(),
            );
        }
        None
    }
    /// A result nothing can be changed in, and why.
    pub fn read_only(driver: Driver, columns: usize, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            driver,
            sources: ResultSources {
                tables: vec![],
                columns: vec![None; columns],
            },
            tables: vec![],
            editable: vec![false; columns],
            delete: Err(reason.clone()),
            read_only: Some(reason),
            session: vec![],
        }
    }
    /// `binary` marks result columns shown as bytes, which are never edited as text.
    pub fn new(driver: Driver, sources: ResultSources, binary: &[bool]) -> Self {
        let tables: Vec<Result<Vec<usize>, String>> = sources
            .tables
            .iter()
            .enumerate()
            .map(|(ti, t)| {
                let name = &t.table.name;
                if let Some(r) = &t.not_a_table {
                    return Err(format!("{name} is a {r}"));
                }
                if t.repeated {
                    return Err(format!("{name} appears more than once in the query"));
                }
                if t.key.is_empty() {
                    return Err(format!("{name} has no primary key or unique NOT NULL key"));
                }
                if t.key_kinds.len() != t.key.len()
                    || !t.key_kinds.iter().copied().all(exact_key_kind)
                {
                    return Err(format!("{name}'s key type can't be matched exactly"));
                }
                t.key
                    .iter()
                    .map(|k| {
                        sources
                            .columns
                            .iter()
                            .position(|c| c.as_ref().is_some_and(|(i, n)| *i == ti && n == k))
                            .ok_or_else(|| format!("{name}'s key column {k} is not in the result"))
                    })
                    .collect()
            })
            .collect();
        let editable: Vec<bool> = sources
            .columns
            .iter()
            .enumerate()
            .map(|(c, origin)| {
                origin.as_ref().is_some_and(|(ti, name)| {
                    let t = &sources.tables[*ti];
                    tables[*ti].is_ok()
                        && !t.key.contains(name)
                        && !t.read_only_columns.contains(name)
                        && !binary.get(c).copied().unwrap_or(false)
                })
            })
            .collect();
        let delete = (|| {
            let mut used = BTreeSet::new();
            for origin in &sources.columns {
                match origin {
                    Some((ti, _)) => used.insert(*ti),
                    None => return Err(format!("the result has a {COMPUTED}")),
                };
            }
            match used.into_iter().collect::<Vec<_>>()[..] {
                [ti] => tables[ti].clone().map(|_| ti),
                [] => Err("the result has no columns".into()),
                _ => Err("the result joins several tables".into()),
            }
        })();
        let read_only = (!editable.iter().any(|e| *e) && delete.is_err()).then(|| {
            tables
                .iter()
                .find_map(|t| t.clone().err())
                .unwrap_or_else(|| {
                    if sources.columns.iter().all(Option::is_none) {
                        "no column comes from a table".into()
                    } else {
                        "no editable columns".into()
                    }
                })
        });
        Self {
            driver,
            sources,
            tables,
            editable,
            delete,
            read_only,
            session: vec![],
        }
    }
    /// The result header's label.
    pub fn badge(&self) -> String {
        if let Some(r) = &self.read_only {
            return format!("Read-only: {r}");
        }
        let names: Vec<_> = self
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_ok())
            .map(|(i, _)| self.sources.tables[i].table.name.clone())
            .collect();
        format!("Editable · {}", names.join(", "))
    }
    /// Why the cells of column `col` can't be edited; `None` when they can.
    pub fn column_reason(&self, col: usize) -> Option<String> {
        if self.editable.get(col).copied().unwrap_or(false) {
            return None;
        }
        // A result read-only as a whole (not a plain SELECT, no metadata) has no per-column story.
        if let (Some(r), true) = (&self.read_only, self.tables.is_empty()) {
            return Some(r.clone());
        }
        Some(match self.sources.columns.get(col).cloned().flatten() {
            None => COMPUTED.into(),
            Some((ti, name)) => match &self.tables[ti] {
                Err(r) => r.clone(),
                Ok(_) if self.sources.tables[ti].key.contains(&name) => KEY_COLUMN.into(),
                Ok(_) => "generated, binary, bit or float column (no exact form to check)".into(),
            },
        })
    }
    /// The key values that identify `row` in source table `ti`; `None` when the row has none
    /// (an outer join's missing side).
    fn key_of(&self, ti: usize, row: &EditRow) -> Option<Vec<String>> {
        let cols = self.tables.get(ti)?.as_ref().ok()?;
        let orig = row.orig.as_ref()?;
        cols.iter()
            .map(|&c| orig.get(c).cloned().flatten())
            .collect()
    }
    /// Why cell (`row`, `col`) can't be changed; `None` when it can.
    pub fn cell_reason(&self, rows: &[EditRow], row: usize, col: usize) -> Option<String> {
        if let Some(r) = self.column_reason(col) {
            return Some(r);
        }
        let r = rows.get(row)?;
        if r.deleted {
            return Some("the row is marked for deletion".into());
        }
        let (ti, _) = self.sources.columns[col].clone()?;
        self.key_of(ti, r).is_none().then(|| {
            format!(
                "this row has no {} row (its key is NULL)",
                self.sources.tables[ti].table.name
            )
        })
    }
    /// Sets cell (`row`, `col`) and every displayed copy of the same stored value: other rows
    /// with the same key (a join's fan-out) and other columns showing the same column.
    pub fn stage(
        &self,
        rows: &mut [EditRow],
        row: usize,
        col: usize,
        value: Cell,
    ) -> Result<(), String> {
        if let Some(r) = self.cell_reason(rows, row, col) {
            return Err(r);
        }
        let (ti, name) = self.sources.columns[col].clone().expect("checked");
        let key = self.key_of(ti, &rows[row]);
        let same: Vec<usize> = (0..self.sources.columns.len())
            .filter(|&c| self.sources.columns[c].as_ref() == Some(&(ti, name.clone())))
            .collect();
        for r in rows.iter_mut() {
            if self.key_of(ti, r) == key {
                for &c in &same {
                    r.cur[c] = value.clone();
                }
            }
        }
        Ok(())
    }
    /// Marks the selected rows deleted, with every other displayed copy of the same rows.
    pub fn delete(&self, rows: &mut [EditRow], selected: &[usize]) -> Result<(), String> {
        let ti = self.delete.clone()?;
        let keys: BTreeSet<Vec<String>> = selected
            .iter()
            .filter_map(|&r| rows.get(r).and_then(|r| self.key_of(ti, r)))
            .collect();
        if keys.is_empty() {
            return Err("no deletable row is selected".into());
        }
        for r in rows.iter_mut() {
            if self.key_of(ti, r).is_some_and(|k| keys.contains(&k)) {
                r.deleted = true;
            }
        }
        Ok(())
    }
    fn key_where(&self, ti: usize, key: &[String], p: &mut Params) -> String {
        let t = &self.sources.tables[ti];
        let d = self.driver;
        t.key
            .iter()
            .zip(key)
            .enumerate()
            .map(|(i, (col, v))| {
                let q = quote_ident(d, col);
                match d {
                    Driver::Mysql => t.key_kinds[i].key_condition(&q, v, p),
                    // Parsed by the column's type, then compared as the text it was shown as, so
                    // a value parsed differently (another DateStyle) can't match another row.
                    Driver::Postgres => {
                        format!(
                            "{q} = {} AND {q}::text = {}",
                            p.add(Some(v)),
                            p.add(Some(v))
                        )
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }
    /// The original value still stored, NULL-safe: a row changed since it was read doesn't match.
    fn unchanged(&self, col: &str, orig: Option<&str>, p: &mut Params) -> String {
        let q = quote_ident(self.driver, col);
        match (orig, self.driver) {
            (None, _) => format!("{q} IS NULL"),
            (Some(v), Driver::Mysql) => {
                format!("BINARY CAST({q} AS CHAR) = BINARY {}", p.add(Some(v)))
            }
            (Some(v), Driver::Postgres) => format!("{q}::text = {}", p.add(Some(v))),
        }
    }
    fn finish(&self, sql: String) -> String {
        if self.driver == Driver::Mysql {
            format!("{sql} LIMIT 1;")
        } else {
            format!("{sql};")
        }
    }
    /// One statement per changed or deleted stored row, each required to match exactly one row.
    pub fn statements(&self, rows: &[EditRow]) -> Result<Vec<Stmt>> {
        let d = self.driver;
        let mut out = vec![];
        let deleted_in = self.delete.clone().ok();
        if let Some(ti) = deleted_in {
            let mut done = BTreeSet::new();
            for r in rows.iter().filter(|r| r.deleted) {
                let Some(key) = self.key_of(ti, r) else {
                    bail!("A row marked for deletion has no key");
                };
                if done.insert(key.clone()) {
                    let mut p = Params::new(d);
                    let w = self.key_where(ti, &key, &mut p);
                    out.push(Stmt {
                        sql: self.finish(format!(
                            "DELETE FROM {} WHERE {w}",
                            qualified(d, &self.sources.tables[ti].table)
                        )),
                        params: p.list,
                        expect: Some(1),
                        check: None,
                    });
                }
            }
        }
        for (ti, cols) in self.tables.iter().enumerate() {
            let Ok(_) = cols else { continue };
            // Stored column -> (new value, original value), per key.
            let mut changes: BTreeMap<Vec<String>, BTreeMap<String, (Cell, Option<String>)>> =
                BTreeMap::new();
            for r in rows
                .iter()
                .filter(|r| !(r.deleted && deleted_in == Some(ti)))
            {
                for (c, origin) in self.sources.columns.iter().enumerate() {
                    let Some((t, name)) = origin else { continue };
                    if *t != ti || !cell_modified(r, c) {
                        continue;
                    }
                    if !self.editable[c] {
                        bail!("{name} can't be changed here");
                    }
                    let Some(key) = self.key_of(ti, r) else {
                        bail!(
                            "A changed row has no key in {}",
                            self.sources.tables[ti].table.name
                        );
                    };
                    let orig = r.orig.as_ref().and_then(|o| o.get(c).cloned().flatten());
                    changes
                        .entry(key)
                        .or_default()
                        .insert(name.clone(), (r.cur[c].clone(), orig));
                }
            }
            for (key, sets) in changes {
                // Every displayed copy of this stored row must agree.
                for r in rows
                    .iter()
                    .filter(|r| self.key_of(ti, r).as_ref() == Some(&key))
                {
                    for (c, origin) in self.sources.columns.iter().enumerate() {
                        if let Some((t, name)) = origin {
                            if *t == ti && sets.get(name).is_some_and(|(v, _)| &r.cur[c] != v) {
                                bail!(
                                    "Conflicting edits to {name} of the same {} row. Nothing was saved.",
                                    self.sources.tables[ti].table.name
                                );
                            }
                        }
                    }
                }
                let mut p = Params::new(d);
                let set: Vec<String> = sets
                    .iter()
                    .map(|(name, (v, _))| {
                        let value = match v {
                            Cell::Text(s) => p.add(Some(s)),
                            Cell::Null => "NULL".into(),
                            Cell::Unset { .. } => "DEFAULT".into(),
                        };
                        format!("{} = {value}", quote_ident(d, name))
                    })
                    .collect();
                let mut w = vec![self.key_where(ti, &key, &mut p)];
                for (name, (_, orig)) in &sets {
                    w.push(self.unchanged(name, orig.as_deref(), &mut p));
                }
                out.push(Stmt {
                    sql: self.finish(format!(
                        "UPDATE {} SET {} WHERE {}",
                        qualified(d, &self.sources.tables[ti].table),
                        set.join(", "),
                        w.join(" AND ")
                    )),
                    params: p.list,
                    expect: Some(1),
                    check: None,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::to_edit_rows;
    fn table(name: &str, key: &[&str]) -> SourceTable {
        SourceTable {
            table: TableRef {
                schema: "s".into(),
                name: name.into(),
            },
            not_a_table: None,
            key: key.iter().map(|k| k.to_string()).collect(),
            key_kinds: key.iter().map(|_| IdentityKind::Signed).collect(),
            read_only_columns: BTreeSet::new(),
            repeated: false,
            columns: BTreeMap::new(),
        }
    }
    fn col(t: usize, name: &str) -> Option<(usize, String)> {
        Some((t, name.into()))
    }
    fn rows(values: &[&[Option<&str>]]) -> Vec<EditRow> {
        to_edit_rows(
            &values
                .iter()
                .map(|r| r.iter().map(|v| v.map(str::to_owned)).collect())
                .collect::<Vec<_>>(),
            None,
        )
    }
    #[test]
    fn single_table_with_key_edits_and_deletes_by_key_only() {
        let plan = ResultEditPlan::new(
            Driver::Mysql,
            ResultSources {
                tables: vec![table("t", &["id"])],
                columns: vec![col(0, "id"), col(0, "name"), None],
            },
            &[false, false, false],
        );
        assert_eq!(plan.editable, [false, true, false]);
        assert_eq!(plan.delete, Err(format!("the result has a {COMPUTED}")));
        assert_eq!(plan.badge(), "Editable · t");
        assert_eq!(plan.column_reason(0).unwrap(), KEY_COLUMN);
        let mut rows = rows(&[
            &[Some("1"), Some("a"), Some("x")],
            &[Some("2"), None, Some("y")],
        ]);
        plan.stage(&mut rows, 1, 1, Cell::Text("ธนาคาร".into()))
            .unwrap();
        plan.stage(&mut rows, 0, 1, Cell::Null).unwrap();
        assert!(plan.stage(&mut rows, 0, 2, Cell::Null).is_err());
        let st = plan.statements(&rows).unwrap();
        assert_eq!(st.len(), 2);
        assert_eq!(
            st[0].sql,
            "UPDATE `s`.`t` SET `name` = NULL WHERE `id` = CAST(? AS DECIMAL(20, 0)) AND BINARY CAST(`name` AS CHAR) = BINARY ? LIMIT 1;"
        );
        assert_eq!(
            st[0].params,
            [serde_json::json!("1"), serde_json::json!("a")]
        );
        assert_eq!(
            st[1].sql,
            "UPDATE `s`.`t` SET `name` = ? WHERE `id` = CAST(? AS DECIMAL(20, 0)) AND `name` IS NULL LIMIT 1;"
        );
        assert!(st.iter().all(|s| s.expect == Some(1)));
    }
    #[test]
    fn delete_needs_every_column_from_one_keyed_table() {
        let plan = ResultEditPlan::new(
            Driver::Postgres,
            ResultSources {
                tables: vec![table("t", &["a", "b"])],
                columns: vec![col(0, "a"), col(0, "b"), col(0, "v")],
            },
            &[],
        );
        assert_eq!(plan.delete, Ok(0));
        let mut r = rows(&[
            &[Some("1"), Some("2"), Some("v")],
            &[Some("1"), Some("3"), Some("w")],
        ]);
        plan.delete(&mut r, &[1]).unwrap();
        assert!(!r[0].deleted && r[1].deleted);
        let st = plan.statements(&r).unwrap();
        assert_eq!(
            st[0].sql,
            r#"DELETE FROM "s"."t" WHERE "a" = $1 AND "a"::text = $2 AND "b" = $3 AND "b"::text = $4;"#
        );
    }
    #[test]
    fn missing_key_view_repeated_and_inexact_keys_are_read_only() {
        let mut view = table("v", &["id"]);
        view.not_a_table = Some("view".into());
        let mut repeated = table("r", &["id"]);
        repeated.repeated = true;
        let mut ts = table("ts", &["at"]);
        ts.key_kinds = vec![IdentityKind::Timestamp];
        for (t, reason) in [
            (
                table("t", &["id"]),
                "t's key column id is not in the result",
            ),
            (
                table("n", &[]),
                "n has no primary key or unique NOT NULL key",
            ),
            (view, "v is a view"),
            (repeated, "r appears more than once in the query"),
            (ts, "ts's key type can't be matched exactly"),
        ] {
            let plan = ResultEditPlan::new(
                Driver::Mysql,
                ResultSources {
                    tables: vec![t],
                    columns: vec![col(0, "name"), col(0, "at")],
                },
                &[],
            );
            assert_eq!(plan.read_only.as_deref(), Some(reason));
            assert_eq!(plan.badge(), format!("Read-only: {reason}"));
            assert!(
                plan.statements(&rows(&[&[Some("x"), Some("1")]]))
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn joins_edit_only_keyed_sides_and_keep_fan_out_copies_consistent() {
        // orders(id, customer) JOIN customers(id, name) without customers.id: only orders edits.
        let plan = ResultEditPlan::new(
            Driver::Mysql,
            ResultSources {
                tables: vec![table("orders", &["id"]), table("customers", &["id"])],
                columns: vec![col(0, "id"), col(0, "note"), col(1, "name")],
            },
            &[],
        );
        assert_eq!(plan.editable, [false, true, false]);
        assert_eq!(plan.delete, Err("the result joins several tables".into()));
        assert_eq!(
            plan.column_reason(2).unwrap(),
            "customers's key column id is not in the result"
        );
        // Fan-out: customers(id) keyed, orders repeated per customer.
        let plan = ResultEditPlan::new(
            Driver::Mysql,
            ResultSources {
                tables: vec![table("customers", &["id"]), table("orders", &["id"])],
                columns: vec![col(0, "id"), col(0, "name"), col(1, "id")],
            },
            &[],
        );
        let mut r = rows(&[
            &[Some("1"), Some("ann"), Some("10")],
            &[Some("1"), Some("ann"), Some("11")],
            &[Some("2"), Some("bob"), Some("12")],
        ]);
        plan.stage(&mut r, 0, 1, Cell::Text("anne".into())).unwrap();
        assert_eq!(
            r[1].cur[1],
            Cell::Text("anne".into()),
            "the other copy follows"
        );
        assert_eq!(r[2].cur[1], Cell::Text("bob".into()));
        let st = plan.statements(&r).unwrap();
        assert_eq!(st.len(), 1, "one UPDATE per stored row");
        // A conflicting copy (only possible by bypassing stage) is refused.
        r[1].cur[1] = Cell::Text("other".into());
        assert!(
            plan.statements(&r)
                .unwrap_err()
                .to_string()
                .contains("Conflicting edits")
        );
    }
    #[test]
    fn outer_join_rows_without_a_key_are_not_editable() {
        let plan = ResultEditPlan::new(
            Driver::Postgres,
            ResultSources {
                tables: vec![table("a", &["id"]), table("b", &["id"])],
                columns: vec![col(0, "id"), col(1, "id"), col(1, "v")],
            },
            &[],
        );
        let mut r = rows(&[&[Some("1"), None, None]]);
        assert!(
            plan.stage(&mut r, 0, 2, Cell::Text("x".into()))
                .unwrap_err()
                .contains("NULL")
        );
    }
    #[test]
    fn only_a_single_plain_select_is_eligible_under_every_server_reading() {
        use crate::guard::editable_select_reason as reason;
        for d in [Driver::Mysql, Driver::Postgres] {
            for ok in [
                "SELECT id, name FROM t",
                "select a.id, b.x from a join b on b.a = a.id where a.id > 1 order by 1 limit 5;",
                "SELECT id, 'GROUP BY' AS label FROM t",
            ] {
                assert_eq!(reason(ok, d), None, "{d:?}: {ok}");
            }
            for (sql, why) in [
                ("SELECT DISTINCT id FROM t", "DISTINCT"),
                ("SELECT id FROM t GROUP BY id", "GROUP BY"),
                ("SELECT count(*) FROM t", "aggregate"),
                (
                    "SELECT id FROM a UNION SELECT id FROM b",
                    "UNION/INTERSECT/EXCEPT",
                ),
                (
                    "SELECT id FROM t WHERE id IN (SELECT id FROM u)",
                    "subquery",
                ),
                ("WITH x AS (SELECT 1) SELECT * FROM x", "not a SELECT"),
                ("SELECT id, rank() OVER () FROM t", "window function"),
                ("SELECT id FROM t FOR UPDATE", "locking read"),
                ("SELECT id FROM t; SELECT 1", "not a single statement"),
                ("UPDATE t SET a = 1", "not a SELECT"),
                ("CALL p()", "not a SELECT"),
            ] {
                assert_eq!(reason(sql, d).as_deref(), Some(why), "{d:?}: {sql}");
            }
        }
        // MySQL runs versioned comments: what they hide counts.
        assert_eq!(
            reason("SELECT /*!50000 DISTINCT */ id FROM t", Driver::Mysql).as_deref(),
            Some("DISTINCT")
        );
    }
    #[test]
    fn binary_generated_and_computed_columns_stay_read_only() {
        let mut t = table("t", &["id"]);
        t.read_only_columns.insert("g".into());
        let plan = ResultEditPlan::new(
            Driver::Mysql,
            ResultSources {
                tables: vec![t],
                columns: vec![col(0, "id"), col(0, "g"), col(0, "blob"), None],
            },
            &[false, false, true, false],
        );
        assert_eq!(plan.editable, [false, false, false, false]);
        assert_eq!(plan.read_only.as_deref(), Some("no editable columns"));
        assert_eq!(plan.column_reason(3).unwrap(), COMPUTED);
    }
}
