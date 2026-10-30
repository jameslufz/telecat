//! Routine definitions, triggers, column names for autocomplete and whole-table export.
//! Catalog reads run on the control connection and exports on their own connection: never on
//! the editor's, so a failing lookup can't abort a transaction the user has open there.
use crate::Session;
use crate::wire::{RowSink, Wire};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use telecat_core::{
    changes::qualified,
    objects::{CreateSettings, Trigger, parse_mysql_trigger_body, parse_pg_triggerdef},
    types::*,
};

/// Rows an export reads before writing them out.
pub const EXPORT_CHUNK: usize = 2000;
/// Tables whose column names one autocomplete lookup asks for.
const COLUMN_BATCH: usize = 200;

/// A routine's CREATE statement and the settings it was created under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineDefinition {
    pub text: String,
    pub settings: CreateSettings,
}
fn text(v: &str) -> Value {
    Value::String(v.to_owned())
}
impl Session {
    /// The CREATE statement of a function or procedure, ready to edit and save again, and on
    /// MySQL the settings it was created under.
    pub async fn routine_definition(&self, o: &DbObject) -> Result<RoutineDefinition> {
        let d = self.driver;
        let kind = o.kind.label();
        let mut settings = CreateSettings::default();
        let definition = if d == Driver::Mysql {
            let keyword = if o.kind == ObjectKind::Procedure {
                "PROCEDURE"
            } else {
                "FUNCTION"
            };
            let r = self
                .catalog(
                    &format!("SHOW CREATE {keyword} {}", qualified(d, &o.table)),
                    &[],
                )
                .await?;
            let row = r
                .first()
                .with_context(|| format!("The {kind} {} no longer exists", o.table.name))?;
            settings = CreateSettings {
                sql_mode: row.get(1).cloned().flatten(),
                collation_connection: row.get(4).cloned().flatten(),
            };
            row.get(2).cloned().flatten().with_context(|| {
                format!(
                    "You don't have the privileges to see the definition of {kind} {}",
                    o.table.name
                )
            })?
        } else {
            let r = self
                .catalog(
                    "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2 AND pg_get_function_identity_arguments(p.oid) = $3",
                    &[
                        text(&o.table.schema),
                        text(&o.table.name),
                        text(o.signature.as_deref().unwrap_or("")),
                    ],
                )
                .await?;
            r.first()
                .and_then(|r| r.first().cloned().flatten())
                .with_context(|| {
                    format!(
                        "The {kind} {} no longer exists. Reload the workspace (⌘R).",
                        o.table.name
                    )
                })?
        };
        Ok(RoutineDefinition {
            text: format!("{};\n", definition.trim_end().trim_end_matches(';')),
            settings,
        })
    }
    /// Whether a trigger of this name exists in `schema` now (MySQL: to tell whether a failed
    /// save left it dropped).
    pub async fn trigger_exists(&self, schema: &str, name: &str) -> Result<bool> {
        let (sql, params) = if self.driver == Driver::Mysql {
            (
                "SELECT COUNT(*) FROM information_schema.triggers WHERE trigger_schema = ? AND trigger_name = ?",
                vec![text(schema), text(name)],
            )
        } else {
            (
                "SELECT COUNT(*) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND t.tgname = $2",
                vec![text(schema), text(name)],
            )
        };
        let r = self.catalog(sql, &params).await?;
        Ok(r.first()
            .and_then(|r| r.first().cloned().flatten())
            .is_some_and(|n| n != "0"))
    }
    /// Whether the routine exists now (MySQL: to tell whether a failed save left it dropped).
    pub async fn routine_exists(&self, o: &DbObject) -> Result<bool> {
        let d = self.driver;
        let (sql, params) = if d == Driver::Mysql {
            (
                "SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema = ? AND routine_name = ? AND routine_type = ?",
                vec![
                    text(&o.table.schema),
                    text(&o.table.name),
                    text(if o.kind == ObjectKind::Procedure {
                        "PROCEDURE"
                    } else {
                        "FUNCTION"
                    }),
                ],
            )
        } else {
            (
                "SELECT COUNT(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2 AND pg_get_function_identity_arguments(p.oid) = $3",
                vec![
                    text(&o.table.schema),
                    text(&o.table.name),
                    text(o.signature.as_deref().unwrap_or("")),
                ],
            )
        };
        let r = self.catalog(sql, &params).await?;
        Ok(r.first()
            .and_then(|r| r.first().cloned().flatten())
            .is_some_and(|n| n != "0"))
    }
    /// The table's triggers, as the Triggers sheet shows them. PostgreSQL's internal triggers
    /// (foreign-key checks) and constraint triggers are left out: they aren't edited here.
    pub async fn triggers(&self, t: &TableRef) -> Result<Vec<Trigger>> {
        let d = self.driver;
        if d == Driver::Mysql {
            let r = self
                .catalog(
                    "SELECT trigger_name, action_timing, event_manipulation, action_order, definer, sql_mode, collation_connection FROM information_schema.triggers WHERE event_object_schema = ? AND event_object_table = ? ORDER BY action_timing, event_manipulation, action_order",
                    &[text(&t.schema), text(&t.name)],
                )
                .await?;
            let mut out = vec![];
            for r in r {
                let f = |i: usize| r.get(i).cloned().flatten();
                let name = f(0).unwrap_or_default();
                // information_schema has string escapes already resolved in ACTION_STATEMENT:
                // the body as written comes from SHOW CREATE TRIGGER.
                let create = self
                    .catalog(
                        &format!(
                            "SHOW CREATE TRIGGER {}",
                            qualified(
                                d,
                                &TableRef {
                                    schema: t.schema.clone(),
                                    name: name.clone(),
                                }
                            )
                        ),
                        &[],
                    )
                    .await?
                    .into_iter()
                    .next()
                    .and_then(|r| r.get(2).cloned().flatten())
                    .with_context(|| {
                        format!("You don't have the privileges to see trigger {name}")
                    })?;
                let statement = parse_mysql_trigger_body(&create).with_context(|| {
                    format!("Telecat can't read the definition of trigger {name}")
                })?;
                out.push(Trigger {
                    neighbours: (None, None),
                    timing: f(1).unwrap_or_default(),
                    event: f(2).unwrap_or_default(),
                    statement,
                    order: f(3).and_then(|o| o.parse().ok()),
                    definer: f(4),
                    settings: CreateSettings {
                        sql_mode: f(5),
                        collation_connection: f(6),
                    },
                    name,
                });
            }
            // Rows come in ACTION_ORDER within each timing and event.
            for i in 0..out.len() {
                let same =
                    |j: usize| out[j].timing == out[i].timing && out[j].event == out[i].event;
                let before = (i > 0 && same(i - 1)).then(|| out[i - 1].name.clone());
                let after = (i + 1 < out.len() && same(i + 1)).then(|| out[i + 1].name.clone());
                out[i].neighbours = (before, after);
            }
            return Ok(out);
        }
        let r = self
            .catalog(
                "SELECT t.tgname, pg_get_triggerdef(t.oid, true) FROM pg_trigger t WHERE t.tgrelid = to_regclass($1) AND NOT t.tgisinternal AND t.tgconstraint = 0 ORDER BY t.tgname",
                &[text(&qualified(d, t))],
            )
            .await?;
        r.into_iter()
            .map(|r| {
                let name = r.first().cloned().flatten().unwrap_or_default();
                let def = r.get(1).cloned().flatten().unwrap_or_default();
                parse_pg_triggerdef(&name, &def)
                    .with_context(|| format!("Telecat can't read the definition of trigger {name}"))
            })
            .collect()
    }
    /// Column names of `tables`, for autocomplete, keyed by (schema, table). Tables that no
    /// longer exist are simply missing from the answer.
    pub async fn column_names(
        &self,
        tables: &[TableRef],
    ) -> Result<BTreeMap<(String, String), Vec<String>>> {
        let d = self.driver;
        let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for batch in tables.chunks(COLUMN_BATCH) {
            let mut params = vec![];
            let pairs = batch
                .iter()
                .map(|t| {
                    params.push(text(&t.schema));
                    params.push(text(&t.name));
                    if d == Driver::Mysql {
                        "(?, ?)".to_owned()
                    } else {
                        format!("(${}, ${})", params.len() - 1, params.len())
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let sql = if d == Driver::Mysql {
                format!(
                    "SELECT table_schema, table_name, column_name FROM information_schema.columns WHERE (table_schema, table_name) IN ({pairs}) ORDER BY table_schema, table_name, ordinal_position"
                )
            } else {
                format!(
                    "SELECT n.nspname::text, c.relname::text, a.attname::text FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE a.attnum > 0 AND NOT a.attisdropped AND (n.nspname::text, c.relname::text) IN ({pairs}) ORDER BY 1, 2, a.attnum"
                )
            };
            for r in self.catalog(&sql, &params).await? {
                let f = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
                out.entry((f(0), f(1))).or_default().push(f(2));
            }
        }
        Ok(out)
    }
    /// Streams every row the table tab's filters and sort select (no paging) to `on_rows`, in
    /// chunks. Runs on its own connection in a read-only transaction that is rolled back, so a
    /// raw SQL filter can't write and a long export never blocks the tab's own reads. Returning
    /// an error from `on_rows` stops the export.
    pub async fn export_table(
        &self,
        req: &TableDataRequest,
        on_rows: &mut RowSink<'_>,
    ) -> Result<u64> {
        let (sql, params) = self.export_select(req).await?;
        let mut conn = self.open_reader().await?;
        let result = export_on(&mut conn, self.driver, &sql, &params, on_rows).await;
        let _ = conn.run("ROLLBACK", &[]).await;
        conn.close().await;
        result
    }
}
async fn export_on(
    conn: &mut Wire,
    d: Driver,
    sql: &str,
    params: &[Value],
    on_rows: &mut RowSink<'_>,
) -> Result<u64> {
    conn.run(
        if d == Driver::Mysql {
            "START TRANSACTION READ ONLY"
        } else {
            "BEGIN TRANSACTION READ ONLY"
        },
        &[],
    )
    .await?;
    let n = conn
        .stream(sql, params, EXPORT_CHUNK, on_rows)
        .await
        .map_err(|e| {
            let text = format!("{e:#}");
            if text.starts_with("ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION")
                || text.starts_with("25006")
            {
                anyhow::anyhow!(
                    "{text}\n\nExports run in a read-only transaction, so a filter that would change data is refused."
                )
            } else {
                e
            }
        })?;
    Ok(n)
}
