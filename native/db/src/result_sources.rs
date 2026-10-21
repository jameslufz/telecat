//! Maps a query result's columns onto base tables from the protocol's column metadata, checked
//! against the catalog. See `telecat_core::result_edit`.
use crate::Session;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use telecat_core::{
    changes::IdentityKind,
    guard::editable_select_reason,
    result_edit::{ResultEditPlan, ResultSources, SourceTable},
    types::*,
};

impl Session {
    /// Catalog reads on the control connection, away from the editor's session and transaction.
    async fn catalog(&self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Option<String>>>> {
        Ok(self
            .control
            .lock()
            .await
            .as_mut()
            .context("Control connection is closed")?
            .run(sql, params)
            .await?
            .0
            .rows)
    }
    /// What can be edited in `result`, the only result of running `sql` alone. Anything that
    /// can't be proven is read-only.
    pub async fn result_edit_plan(
        &self,
        sql: &str,
        result: &QueryResult,
    ) -> Result<ResultEditPlan> {
        let n = result.columns.len();
        if let Some(reason) = editable_select_reason(sql, self.driver) {
            return Ok(ResultEditPlan::read_only(self.driver, n, reason));
        }
        let Some(origins) = result.origins.as_ref().filter(|o| o.len() == n) else {
            return Ok(ResultEditPlan::read_only(
                self.driver,
                n,
                "no column metadata",
            ));
        };
        let session = match self.session_settings().await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ResultEditPlan::read_only(
                    self.driver,
                    n,
                    format!("the session's settings couldn't be read ({e})"),
                ));
            }
        };
        let sources = match self.driver {
            Driver::Mysql => self.mysql_sources(origins).await?,
            Driver::Postgres => match self.pg_scan_roots(sql).await {
                Ok(Ok(scans)) => {
                    // Floats are shown rounded when extra_float_digits < 1: not exact text.
                    let lossy_floats = session
                        .iter()
                        .find(|(k, _)| k == "extra_float_digits")
                        .and_then(|(_, v)| v.parse::<i32>().ok())
                        .is_none_or(|v| v < 1);
                    self.pg_sources(origins, &scans, lossy_floats).await?
                }
                Ok(Err(reason)) => return Ok(ResultEditPlan::read_only(self.driver, n, reason)),
                Err(e) => {
                    return Ok(ResultEditPlan::read_only(
                        self.driver,
                        n,
                        format!("its query plan couldn't be checked ({e})"),
                    ));
                }
            },
        };
        let binary = result.binary.clone().unwrap_or_default();
        let mut plan = ResultEditPlan::new(self.driver, sources, &binary);
        plan.session = session;
        Ok(plan)
    }
    async fn mysql_sources(&self, origins: &[Option<ColumnOrigin>]) -> Result<ResultSources> {
        // A table instance is (schema, alias, table); the same table under two aliases is a
        // self-join whose rows can't be told apart.
        let mut instances: Vec<(String, String, String)> = vec![];
        let mut columns = vec![];
        for o in origins {
            columns.push(match o {
                Some(ColumnOrigin::Mysql {
                    schema,
                    alias,
                    table,
                    column,
                }) if !schema.is_empty() => {
                    let inst = (schema.clone(), alias.clone(), table.clone());
                    let i = instances
                        .iter()
                        .position(|x| *x == inst)
                        .unwrap_or_else(|| {
                            instances.push(inst);
                            instances.len() - 1
                        });
                    Some((i, column.clone()))
                }
                _ => None,
            });
        }
        let mut tables = vec![];
        for (schema, _, name) in &instances {
            let repeated = instances
                .iter()
                .filter(|(s, _, t)| s == schema && t == name)
                .count()
                > 1;
            tables.push(self.mysql_table(schema, name, repeated).await?);
        }
        // The protocol names the column as it was when the statement ran; one the catalog no
        // longer lists (renamed or dropped since) has no origin to write to.
        for c in columns.iter_mut() {
            if c.as_ref()
                .is_some_and(|(i, name)| !tables[*i].columns.contains_key(name))
            {
                *c = None;
            }
        }
        Ok(ResultSources { tables, columns })
    }
    async fn mysql_table(&self, schema: &str, name: &str, repeated: bool) -> Result<SourceTable> {
        let p = [json!(schema), json!(name)];
        let kind = self
            .catalog(
                "SELECT TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                &p,
            )
            .await?;
        // A temporary table on the editor's connection hides the stored table of the same name;
        // the catalog shows the stored one, which the commit would write to.
        let temporary = self
            .query(&format!(
                "SHOW CREATE TABLE {}.{}",
                telecat_core::changes::quote_ident(Driver::Mysql, schema),
                telecat_core::changes::quote_ident(Driver::Mysql, name)
            ))
            .await
            .ok()
            .and_then(|r| r.rows.first().and_then(|r| r.get(1)).cloned().flatten())
            .is_some_and(|ddl| ddl.starts_with("CREATE TEMPORARY TABLE"));
        let not_a_table = match kind.first().and_then(|r| r.first()).cloned().flatten() {
            _ if temporary => Some("temporary table".to_owned()),
            Some(t) if t == "BASE TABLE" => None,
            Some(t) if t == "VIEW" || t == "SYSTEM VIEW" => Some("view".to_owned()),
            Some(t) => Some(t.to_lowercase()),
            None => Some("table that can't be found".to_owned()),
        };
        let cols = self
            .catalog(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, EXTRA, IS_NULLABLE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                &p,
            )
            .await?;
        let mut kinds = BTreeMap::new();
        let mut columns = BTreeMap::new();
        let mut not_null = BTreeSet::new();
        let mut read_only_columns = BTreeSet::new();
        for r in &cols {
            let f = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
            let (col, ty, column_type, extra) = (f(0), f(1), f(2), f(3).to_ascii_uppercase());
            let kind = IdentityKind::from_mysql_type(&ty, &column_type);
            // FLOAT is shown with 6 significant digits: the original value can't be checked.
            if extra.contains("VIRTUAL GENERATED")
                || extra.contains("STORED GENERATED")
                || matches!(
                    kind,
                    IdentityKind::Binary | IdentityKind::Bit | IdentityKind::Float
                )
            {
                read_only_columns.insert(col.clone());
            }
            if f(4) == "NO" {
                not_null.insert(col.clone());
            }
            columns.insert(
                col.clone(),
                format!("{column_type} {extra}").trim().to_owned(),
            );
            kinds.insert(col, kind);
        }
        // Unique indexes over whole stored columns (no prefixes or expressions), PRIMARY first.
        let idx = self
            .catalog(
                "SELECT INDEX_NAME, COLUMN_NAME, SUB_PART FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND NON_UNIQUE = 0 ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
                &p,
            )
            .await?;
        let mut keys: Vec<(String, Vec<Option<String>>)> = vec![];
        for r in &idx {
            let index = r.first().cloned().flatten().unwrap_or_default();
            let column = r
                .get(1)
                .cloned()
                .flatten()
                .filter(|_| r.get(2).cloned().flatten().is_none());
            match keys.last_mut() {
                Some((i, cols)) if *i == index => cols.push(column),
                _ => keys.push((index, vec![column])),
            }
        }
        let key = keys
            .into_iter()
            .map(|(_, cols)| cols)
            .find(|cols| {
                cols.iter()
                    .all(|c| c.as_ref().is_some_and(|c| not_null.contains(c)))
            })
            .map(|cols| cols.into_iter().flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(SourceTable {
            table: TableRef {
                schema: schema.into(),
                name: name.into(),
            },
            not_a_table,
            key_kinds: key
                .iter()
                .map(|k| kinds.get(k).copied().unwrap_or(IdentityKind::Other))
                .collect(),
            key,
            read_only_columns,
            repeated,
            columns,
        })
    }
    /// The topmost ancestor(s) of a relation: the partition root or inheritance parent.
    async fn pg_roots(&self, by: &str, params: &[Value]) -> Result<Vec<u32>> {
        let sql = format!(
            "WITH RECURSIVE up(oid) AS (SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE {by} UNION SELECT i.inhparent FROM pg_inherits i JOIN up ON i.inhrelid = up.oid) SELECT oid::text FROM up WHERE oid NOT IN (SELECT inhrelid FROM pg_inherits)"
        );
        Ok(self
            .catalog(&sql, params)
            .await?
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten()?.parse().ok())
            .collect())
    }
    /// How many separate reads of each table the plan makes, by the OID of the table's root
    /// (partitions and inheritance children count as their root; the partitions of one Append
    /// are one read). Uses the editor's connection, so names resolve exactly as they did for the
    /// query; nothing is executed. `Err` when a plan node reads rows from something that isn't a
    /// table (a function, CTE, subquery or foreign scan).
    async fn pg_scan_roots(
        &self,
        sql: &str,
    ) -> Result<std::result::Result<BTreeMap<u32, usize>, String>> {
        let plan = self
            .query(&format!("EXPLAIN (VERBOSE, FORMAT JSON) {sql}"))
            .await?;
        let text = plan
            .rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .flatten()
            .context("EXPLAIN returned no plan")?;
        let json: Value = serde_json::from_str(&text)?;
        const OPAQUE: [&str; 8] = [
            "Function Scan",
            "Table Function Scan",
            "CTE Scan",
            "WorkTable Scan",
            "Foreign Scan",
            "Custom Scan",
            "Subquery Scan",
            "Named Tuplestore Scan",
        ];
        fn relations(
            node: &Value,
            out: &mut BTreeSet<(String, String)>,
        ) -> std::result::Result<(), String> {
            let kind = node["Node Type"].as_str().unwrap_or("");
            if OPAQUE.contains(&kind) {
                return Err(format!("the query reads rows through a {kind}"));
            }
            if let (Some(schema), Some(name)) =
                (node["Schema"].as_str(), node["Relation Name"].as_str())
            {
                out.insert((schema.to_owned(), name.to_owned()));
            }
            for child in node["Plans"].as_array().into_iter().flatten() {
                relations(child, out)?;
            }
            Ok(())
        }
        let mut names = BTreeSet::new();
        for top in json.as_array().into_iter().flatten() {
            if let Err(reason) = relations(&top["Plan"], &mut names) {
                return Ok(Err(reason));
            }
        }
        let mut roots: BTreeMap<(String, String), Vec<u32>> = BTreeMap::new();
        for (schema, name) in names {
            let r = self
                .pg_roots(
                    "n.nspname = $1 AND c.relname = $2",
                    &[json!(schema), json!(name)],
                )
                .await?;
            roots.insert((schema, name), r);
        }
        fn count(
            node: &Value,
            roots: &BTreeMap<(String, String), Vec<u32>>,
        ) -> BTreeMap<u32, usize> {
            let mut out: BTreeMap<u32, usize> = BTreeMap::new();
            if let (Some(schema), Some(name)) =
                (node["Schema"].as_str(), node["Relation Name"].as_str())
            {
                for r in roots
                    .get(&(schema.to_owned(), name.to_owned()))
                    .into_iter()
                    .flatten()
                {
                    *out.entry(*r).or_default() += 1;
                }
            }
            let children: Vec<_> = node["Plans"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| count(c, roots))
                .collect();
            // The children of an Append are alternatives (partitions, inheritance children): one
            // read of their root. Anything else reads each child.
            let append = matches!(node["Node Type"].as_str(), Some("Append" | "MergeAppend"));
            for c in children {
                for (r, n) in c {
                    let e = out.entry(r).or_default();
                    *e = if append { (*e).max(n) } else { *e + n };
                }
            }
            out
        }
        let mut out = BTreeMap::new();
        for top in json.as_array().into_iter().flatten() {
            for (r, n) in count(&top["Plan"], &roots) {
                *out.entry(r).or_default() += n;
            }
        }
        Ok(Ok(out))
    }
    /// The editor session's settings that decide how typed values are read, applied to the
    /// commit so a value means what it meant where the query ran.
    async fn session_settings(&self) -> Result<Vec<(String, String)>> {
        let (sql, names): (&str, &[&str]) = match self.driver {
            Driver::Mysql => ("SELECT @@session.time_zone", &["time_zone"]),
            Driver::Postgres => (
                "SELECT current_setting('TimeZone'), current_setting('DateStyle'), current_setting('IntervalStyle'), current_setting('extra_float_digits')",
                &[
                    "TimeZone",
                    "DateStyle",
                    "IntervalStyle",
                    "extra_float_digits",
                ],
            ),
        };
        let row = self
            .query(sql)
            .await?
            .rows
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(names
            .iter()
            .zip(row)
            .filter_map(|(n, v)| v.map(|v| (n.to_string(), v)))
            .collect())
    }
    async fn pg_sources(
        &self,
        origins: &[Option<ColumnOrigin>],
        scans: &BTreeMap<u32, usize>,
        lossy_floats: bool,
    ) -> Result<ResultSources> {
        // PostgreSQL reports only the table's OID: a self-join shows the same OID twice, so a
        // column reported more than once marks the table as repeated.
        let mut oids: Vec<u32> = vec![];
        let mut seen = BTreeSet::new();
        let mut repeated = BTreeSet::new();
        let mut raw = vec![];
        for o in origins {
            raw.push(match o {
                Some(ColumnOrigin::Postgres { table_oid, attnum }) => {
                    if !seen.insert((*table_oid, *attnum)) {
                        repeated.insert(*table_oid);
                    }
                    let i = oids.iter().position(|x| x == table_oid).unwrap_or_else(|| {
                        oids.push(*table_oid);
                        oids.len() - 1
                    });
                    Some((i, *attnum))
                }
                _ => None,
            });
        }
        let mut tables = vec![];
        let mut names: Vec<BTreeMap<i16, String>> = vec![];
        for oid in &oids {
            let (mut table, attnames) = self
                .pg_table(*oid, repeated.contains(oid), lossy_floats)
                .await?;
            // A self-join reports one OID for both sides: the plan shows the second read, of the
            // table or of its partitions / inheritance children, counted by their root.
            let roots = self
                .pg_roots("c.oid = $1::oid", &[json!(oid.to_string())])
                .await?;
            if roots.is_empty()
                || roots
                    .iter()
                    .any(|r| scans.get(r).copied().unwrap_or(0) != 1)
            {
                table.repeated = true;
            }
            tables.push(table);
            names.push(attnames);
        }
        let columns = raw
            .into_iter()
            .map(|c| c.and_then(|(i, attnum)| names[i].get(&attnum).map(|n| (i, n.clone()))))
            .collect();
        Ok(ResultSources { tables, columns })
    }
    async fn pg_table(
        &self,
        oid: u32,
        repeated: bool,
        lossy_floats: bool,
    ) -> Result<(SourceTable, BTreeMap<i16, String>)> {
        let p = [json!(oid.to_string())];
        let rel = self
            .catalog(
                "SELECT n.nspname, c.relname, c.relkind::text, c.relpersistence::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = $1::oid",
                &p,
            )
            .await?;
        let row = rel
            .first()
            .context("A result column's table no longer exists")?;
        let f = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let not_a_table = match f(2).as_str() {
            _ if f(3) == "t" => Some("temporary table".to_owned()),
            "r" | "p" => None,
            "v" => Some("view".to_owned()),
            "m" => Some("materialized view".to_owned()),
            "f" => Some("foreign table".to_owned()),
            _ => Some("relation that isn't a table".to_owned()),
        };
        let atts = self
            .catalog(
                "SELECT attnum::text, attname, atttypid::text, attgenerated::text, attnotnull::text FROM pg_attribute WHERE attrelid = $1::oid AND attnum > 0 AND NOT attisdropped",
                &p,
            )
            .await?;
        let mut names = BTreeMap::new();
        let mut not_null = BTreeSet::new();
        let mut read_only_columns = BTreeSet::new();
        let mut floats = BTreeSet::new();
        for a in &atts {
            let g = |i: usize| a.get(i).cloned().flatten().unwrap_or_default();
            let (Ok(num), name) = (g(0).parse::<i16>(), g(1)) else {
                continue;
            };
            let float = matches!(g(2).as_str(), "700" | "701");
            if float {
                floats.insert(name.clone());
            }
            // bytea, bit, bit varying; generated columns; floats shown rounded.
            if matches!(g(2).as_str(), "17" | "1560" | "1562")
                || !g(3).is_empty()
                || (float && lossy_floats)
            {
                read_only_columns.insert(name.clone());
            }
            if g(4) == "true" {
                not_null.insert(num);
            }
            names.insert(num, name);
        }
        let idx = self
            .catalog(
                "SELECT indkey::text FROM pg_index WHERE indrelid = $1::oid AND indisunique AND indimmediate AND indpred IS NULL AND indexprs IS NULL ORDER BY indisprimary DESC, indexrelid",
                &p,
            )
            .await?;
        let key = idx
            .iter()
            .filter_map(|r| r.first().cloned().flatten())
            .map(|k| {
                k.split_whitespace()
                    .map(|n| n.parse::<i16>().ok())
                    .collect::<Option<Vec<_>>>()
            })
            .find_map(|cols| {
                cols.filter(|c| !c.is_empty() && c.iter().all(|n| not_null.contains(n)))
            })
            .map(|cols| {
                cols.iter()
                    .filter_map(|n| names.get(n).cloned())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        // Float keys have no exact text form to find the row by (any session setting).
        let key_kinds = key
            .iter()
            .map(|k| {
                if floats.contains(k) {
                    IdentityKind::Float
                } else {
                    IdentityKind::Other
                }
            })
            .collect();
        Ok((
            SourceTable {
                table: TableRef {
                    schema: f(0),
                    name: f(1),
                },
                not_a_table,
                key,
                key_kinds,
                read_only_columns,
                repeated,
                columns: BTreeMap::new(),
            },
            names,
        ))
    }
}
