//! The Data view's per-column facts (see `telecat_core::columns`), read from the catalog on the
//! control connection.
use crate::Session;
use anyhow::{Context, Result};
use serde_json::json;
use telecat_core::{
    changes::{literal, qualified},
    columns::{
        ColumnDetail, TableColumns, ValueKind, mysql_generated, mysql_kind, mysql_values, now_sql,
        pg_kind,
    },
    filters::ForeignKey,
    types::*,
};

impl Session {
    /// Every column's kind, allowed values, nullability and generation, and the table's foreign
    /// keys (composite ones with their columns in key order).
    pub async fn column_details(&self, t: &TableRef) -> Result<TableColumns> {
        let t = self.table(t);
        let d = self.driver;
        let text =
            |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
        let truthy = |s: String| matches!(s.as_str(), "true" | "t" | "1" | "YES");
        let mut out = TableColumns::default();
        if d == Driver::Mysql {
            let params = [json!(t.schema), json!(t.name)];
            for r in self
                .catalog(
                    "SELECT column_name, column_type, data_type, is_nullable, extra FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ordinal_position",
                    &params,
                )
                .await?
            {
                let column_type = text(&r, 1);
                let kind = mysql_kind(&text(&r, 2), &column_type);
                let extra = text(&r, 4);
                out.columns.push(ColumnDetail {
                    name: text(&r, 0),
                    values: if matches!(kind, ValueKind::Enum | ValueKind::Set) {
                        mysql_values(&column_type)
                    } else {
                        vec![]
                    },
                    data_type: column_type,
                    kind,
                    nullable: truthy(text(&r, 3)),
                    generated: mysql_generated(&extra),
                });
            }
            for r in self
                .catalog(
                    "SELECT constraint_name, column_name, referenced_table_schema, referenced_table_name, referenced_column_name FROM information_schema.key_column_usage WHERE table_schema = ? AND table_name = ? AND referenced_table_name IS NOT NULL ORDER BY constraint_name, ordinal_position",
                    &params,
                )
                .await?
            {
                let name = text(&r, 0);
                let target = TableRef {
                    schema: text(&r, 2),
                    name: text(&r, 3),
                };
                match out.foreign_keys.last_mut().filter(|fk| fk.name == name) {
                    Some(fk) => {
                        fk.columns.push(text(&r, 1));
                        fk.target_columns.push(text(&r, 4));
                    }
                    None => out.foreign_keys.push(ForeignKey {
                        name,
                        columns: vec![text(&r, 1)],
                        target,
                        target_columns: vec![text(&r, 4)],
                    }),
                }
            }
        } else {
            let rel = literal(d, &qualified(d, &t));
            for r in self
                .catalog(
                    &format!(
                        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), (ty.typtype = 'e')::text, (NOT a.attnotnull)::text, (a.attgenerated <> '')::text, CASE WHEN ty.typtype = 'e' THEN (SELECT json_agg(e.enumlabel ORDER BY e.enumsortorder)::text FROM pg_enum e WHERE e.enumtypid = ty.oid) END FROM pg_attribute a JOIN pg_type ty ON ty.oid = a.atttypid WHERE a.attrelid = {rel}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
                    ),
                    &[],
                )
                .await?
            {
                let is_enum = truthy(text(&r, 2));
                let data_type = text(&r, 1);
                out.columns.push(ColumnDetail {
                    name: text(&r, 0),
                    kind: pg_kind(&data_type, is_enum),
                    data_type,
                    values: r
                        .get(5)
                        .cloned()
                        .flatten()
                        .map(|labels| serde_json::from_str(&labels))
                        .transpose()
                        .context("Enum labels")?
                        .unwrap_or_default(),
                    nullable: truthy(text(&r, 3)),
                    generated: truthy(text(&r, 4)),
                });
            }
            let names = |side: &str, rel: &str| {
                format!(
                    "(SELECT json_agg(a.attname ORDER BY k.n)::text FROM unnest(con.{side}) WITH ORDINALITY AS k(attnum, n) JOIN pg_attribute a ON a.attrelid = con.{rel} AND a.attnum = k.attnum)"
                )
            };
            for r in self
                .catalog(
                    &format!(
                        "SELECT con.conname::text, nf.nspname::text, cf.relname::text, {}, {} FROM pg_constraint con JOIN pg_class cf ON cf.oid = con.confrelid JOIN pg_namespace nf ON nf.oid = cf.relnamespace WHERE con.conrelid = {rel}::regclass AND con.contype = 'f' ORDER BY con.conname",
                        names("conkey", "conrelid"),
                        names("confkey", "confrelid")
                    ),
                    &[],
                )
                .await?
            {
                let list = |i: usize| -> Result<Vec<String>> {
                    serde_json::from_str(&text(&r, i)).context("Foreign key columns")
                };
                out.foreign_keys.push(ForeignKey {
                    name: text(&r, 0),
                    columns: list(3)?,
                    target: TableRef {
                        schema: text(&r, 1),
                        name: text(&r, 2),
                    },
                    target_columns: list(4)?,
                });
            }
        }
        Ok(out)
    }
    /// The server's "now" as `column` stores it (Set Value ▸ NOW()); `None` for a column that
    /// isn't a date or time.
    pub async fn now_text(&self, column: &ColumnDetail) -> Result<Option<String>> {
        let Some(sql) = now_sql(self.driver, column) else {
            return Ok(None);
        };
        let rows = self.catalog(&sql, &[]).await?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next().flatten()))
    }
}
