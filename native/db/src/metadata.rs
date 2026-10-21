use crate::{Session, format_bytes};
use anyhow::{Context, Result};
use telecat_core::{
    changes::{
        IdentityKind, Stmt, inline_params, literal, mysql_identity_columns, mysql_row_id,
        qualified, quote_ident,
    },
    filters::{Where, build_where},
    types::*,
};
pub(crate) enum Identity {
    None,
    Postgres,
    Mysql {
        columns: Vec<String>,
        kinds: Vec<IdentityKind>,
        keys: Vec<(String, IdentityKind)>,
        exprs: Vec<String>,
    },
}
impl Session {
    fn table(&self, t: &TableRef) -> TableRef {
        TableRef {
            schema: if t.schema.is_empty() {
                if self.driver == Driver::Mysql {
                    self.database.clone()
                } else {
                    "public".into()
                }
            } else {
                t.schema.clone()
            },
            name: t.name.clone(),
        }
    }
    async fn strings(&self, sql: &str) -> Result<Vec<String>> {
        Ok(self
            .query(sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }
    pub async fn list_databases(&self) -> Result<Vec<String>> {
        self.strings(if self.driver == Driver::Mysql {
            "SHOW DATABASES"
        } else {
            "SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY datname"
        })
        .await
    }
    pub async fn columns(&self, t: &TableRef) -> Result<Vec<String>> {
        let t = self.table(t);
        let d = self.driver;
        let rel = qualified(d, &t);
        self.strings(&if d==Driver::Mysql{format!("SELECT column_name FROM information_schema.columns WHERE table_schema={} AND table_name={} ORDER BY ordinal_position",literal(d,&t.schema),literal(d,&t.name))}else{format!("SELECT attname FROM pg_attribute WHERE attrelid = {}::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum",literal(d,&rel))}).await
    }
    pub async fn primary_key(&self, t: &TableRef) -> Result<Vec<String>> {
        let t = self.table(t);
        let d = self.driver;
        let rel = qualified(d, &t);
        self.strings(&if d==Driver::Mysql{format!("SELECT column_name FROM information_schema.key_column_usage WHERE table_schema={} AND table_name={} AND constraint_name='PRIMARY' ORDER BY ordinal_position",literal(d,&t.schema),literal(d,&t.name))}else{format!("SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid = {}::regclass AND i.indisprimary ORDER BY array_position(i.indkey, a.attnum)",literal(d,&rel))}).await
    }
    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        let mut out = vec![];
        if self.driver == Driver::Mysql {
            if self.database.is_empty() {
                return Ok(out);
            }
            let db = literal(self.driver, &self.database);
            for r in self.query(&format!("SELECT table_name, table_type FROM information_schema.tables WHERE table_schema={db} ORDER BY table_name")).await?.rows{out.push(DbObject{table:TableRef{schema:self.database.clone(),name:r[0].clone().unwrap_or_default()},kind:if r[1].as_deref()==Some("VIEW"){ObjectKind::View}else{ObjectKind::Table}});}
            for r in self.query(&format!("SELECT routine_name, routine_type FROM information_schema.routines WHERE routine_schema={db} ORDER BY routine_name")).await?.rows{out.push(DbObject{table:TableRef{schema:self.database.clone(),name:r[0].clone().unwrap_or_default()},kind:if r[1].as_deref()==Some("PROCEDURE"){ObjectKind::Procedure}else{ObjectKind::Function}});}
        } else {
            let sql = "SELECT n.nspname, c.relname, c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r','p','v','m','f') AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND NOT c.relispartition ORDER BY n.nspname <> 'public', n.nspname, c.relname";
            for r in self.query(sql).await?.rows {
                out.push(DbObject {
                    table: TableRef {
                        schema: r[0].clone().unwrap_or_default(),
                        name: r[1].clone().unwrap_or_default(),
                    },
                    kind: if matches!(r[2].as_deref(), Some("v" | "m")) {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                });
            }
            let sql = "SELECT n.nspname, p.proname, p.prokind FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') ORDER BY n.nspname, p.proname";
            for r in self.query(sql).await?.rows {
                out.push(DbObject {
                    table: TableRef {
                        schema: r[0].clone().unwrap_or_default(),
                        name: r[1].clone().unwrap_or_default(),
                    },
                    kind: if r[2].as_deref() == Some("p") {
                        ObjectKind::Procedure
                    } else {
                        ObjectKind::Function
                    },
                });
            }
        }
        Ok(out)
    }
    /// MySQL column names and identity kinds in `SELECT *` order (invisible columns excluded),
    /// and the kinds of every column, invisible ones included.
    async fn mysql_visible_columns(
        &self,
        t: &TableRef,
    ) -> Result<(Vec<String>, Vec<IdentityKind>, Vec<(String, IdentityKind)>)> {
        let d = self.driver;
        let rows = self
            .query(&format!(
                "SELECT column_name, data_type, extra, column_type FROM information_schema.columns WHERE table_schema={} AND table_name={} ORDER BY ordinal_position",
                literal(d, &t.schema),
                literal(d, &t.name)
            ))
            .await?
            .rows;
        let mut visible = (vec![], vec![]);
        let mut all = vec![];
        for r in rows {
            let field = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
            let (name, kind) = (
                field(0),
                IdentityKind::from_mysql_type(&field(1), &field(3)),
            );
            if !field(2).to_ascii_uppercase().contains("INVISIBLE") {
                visible.0.push(name.clone());
                visible.1.push(kind);
            }
            all.push((name, kind));
        }
        Ok((visible.0, visible.1, all))
    }
    async fn select(
        &self,
        req: &TableDataRequest,
    ) -> Result<(String, Where, Vec<String>, String, Identity)> {
        let d = self.driver;
        let t = self.table(&req.table);
        let rel = qualified(d, &t);
        let columns = self.columns(&t).await?;
        let pk = self.primary_key(&t).await?;
        let w = build_where(&req.filters, &columns, d, req.combine);
        let order = if let Some(o) = &req.order_by {
            format!(" ORDER BY {} {}", quote_ident(d, &o.column), o.dir.as_str())
        } else if !pk.is_empty() {
            format!(
                " ORDER BY {}",
                pk.iter()
                    .map(|s| quote_ident(d, s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        };
        let identity = if d == Driver::Postgres {
            let physical = self
                .strings(&format!(
                    "SELECT relkind::text FROM pg_class WHERE oid = {}::regclass",
                    literal(d, &rel)
                ))
                .await?
                .first()
                .is_some_and(|s| s == "r" || s == "p");
            if physical {
                Identity::Postgres
            } else {
                Identity::None
            }
        } else {
            let (columns, kinds, all) = self.mysql_visible_columns(&t).await?;
            let key_kinds: Vec<(String, IdentityKind)> = pk
                .iter()
                .map(|k| {
                    let kind = all
                        .iter()
                        .find(|c| &c.0 == k)
                        .map_or(IdentityKind::Other, |c| c.1);
                    (k.clone(), kind)
                })
                .collect();
            if columns.is_empty() {
                Identity::None
            } else {
                let exprs = mysql_identity_columns(&columns, &kinds, &key_kinds);
                Identity::Mysql {
                    columns,
                    kinds,
                    keys: key_kinds,
                    exprs,
                }
            }
        };
        let extra = match &identity {
            Identity::None => String::new(),
            Identity::Postgres => {
                ", tableoid::oid::text AS \"__telecat_tableoid\", ctid::text AS \"__telecat_ctid\""
                    .into()
            }
            Identity::Mysql { exprs, .. } => exprs
                .iter()
                .enumerate()
                .map(|(i, e)| format!(", {e} AS `__telecat_id{i}`"))
                .collect(),
        };
        let sql = format!(
            "SELECT *{extra} FROM {rel}{}{order} LIMIT {} OFFSET {};",
            w.sql, req.limit, req.offset
        );
        Ok((sql, w, pk, rel, identity))
    }
    pub async fn table_data_sql(&self, req: &TableDataRequest) -> Result<String> {
        let (sql, w, _, _, _) = self.select(req).await?;
        let mut st = Stmt::new(sql);
        st.params = w.params;
        Ok(if self.driver == Driver::Mysql {
            mysql_preview(&st.sql, &st.params)
        } else {
            inline_params(self.driver, &st)
        })
    }
    /// Table reads run on the reader connection: the editor's connection may hold the user's own
    /// transaction or session settings (time zone, sql_mode…) that would change what rows look
    /// like, and a slow read there or on the commit connection would block the editor or ⌘S.
    /// Raw SQL filters are user-written expressions that may call functions with side effects, so
    /// those reads run in a read-only transaction that is always rolled back.
    async fn read_rows(
        &self,
        page: &str,
        count: &str,
        params: &[serde_json::Value],
        raw: bool,
    ) -> Result<(QueryResult, QueryResult)> {
        let mut slot = self.reader.lock().await;
        let mut conn = match slot.take() {
            Some(c) => c,
            None => self.open_reader().await?,
        };
        let id = self.backend_id_of(&mut conn).await?;
        *self.reader_busy.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
        let begin = if self.driver == Driver::Mysql {
            "START TRANSACTION READ ONLY"
        } else {
            "BEGIN TRANSACTION READ ONLY"
        };
        let result = async {
            if raw {
                conn.run(begin, &[]).await?;
            }
            let page = conn.run(page, params).await?.0;
            let count = conn.run(count, params).await?.0;
            Ok::<_, anyhow::Error>((page, count))
        }
        .await;
        *self.reader_busy.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if raw {
            // A raw filter can leave session state behind that a rollback doesn't undo (GET_LOCK,
            // pg_advisory_lock, user variables, temporary tables). Its connection is discarded.
            let _ = conn.run("ROLLBACK", &[]).await;
            // Released explicitly too: the server drops a closed session's locks asynchronously.
            let _ = conn
                .run(
                    if self.driver == Driver::Mysql {
                        "SELECT RELEASE_ALL_LOCKS()"
                    } else {
                        "SELECT pg_advisory_unlock_all()"
                    },
                    &[],
                )
                .await;
            conn.close().await;
        } else {
            *slot = Some(conn);
        }
        result.map_err(|e| {
            let text = format!("{e:#}");
            if text.starts_with("ER_QUERY_INTERRUPTED") || text.starts_with("57014") {
                anyhow::anyhow!("Loading the table was cancelled.")
            } else if raw
                && (text.starts_with("ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION")
                    || text.starts_with("25006"))
            {
                anyhow::anyhow!(
                    "{text}\n\nRaw SQL filters run in a read-only transaction, so a filter that would change data is refused."
                )
            } else {
                e
            }
        })
    }
    pub async fn table_data(&self, req: &TableDataRequest) -> Result<TableDataResult> {
        let (sql, w, pk, rel, identity) = self.select(req).await?;
        let raw = req
            .filters
            .iter()
            .any(|f| f.enabled && f.column == RAW_SQL && !f.value.trim().is_empty());
        let count_sql = format!(
            "SELECT COUNT(*) {}count FROM {rel}{};",
            if self.driver == Driver::Mysql {
                "as "
            } else {
                "AS "
            },
            w.sql
        );
        let (mut result, count) = self.read_rows(&sql, &count_sql, &w.params, raw).await?;
        let total = count
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_ref())
            .context("Missing row count")?
            .parse()?;
        let hidden = match &identity {
            Identity::None => 0,
            Identity::Postgres => 2,
            Identity::Mysql { exprs, .. } => exprs.len(),
        };
        let row_ids = if hidden == 0 {
            None
        } else {
            let n = result
                .columns
                .len()
                .checked_sub(hidden)
                .context("Missing row identity columns")?;
            result.columns.truncate(n);
            if let Some(v) = &mut result.numeric {
                v.truncate(n);
            }
            if let Some(v) = &mut result.binary {
                v.truncate(n);
            }
            let ids: Vec<Vec<Option<String>>> =
                result.rows.iter_mut().map(|r| r.split_off(n)).collect();
            match &identity {
                Identity::Mysql {
                    columns,
                    kinds,
                    keys,
                    ..
                } => {
                    // Fail closed: if `SELECT *` didn't return exactly the columns the identity
                    // was built from, or the server returned no fingerprint, the row gets an
                    // identity that every write refuses ("reload the table").
                    let matches = *columns == result.columns;
                    Some(
                        ids.iter()
                            .map(|v| {
                                matches
                                    .then(|| mysql_row_id(kinds, keys, v))
                                    .flatten()
                                    .unwrap_or_else(unidentified_mysql_row)
                            })
                            .collect(),
                    )
                }
                _ => Some(
                    ids.into_iter()
                        .map(|v| v.into_iter().map(Option::unwrap_or_default).collect())
                        .collect(),
                ),
            }
        };
        let row_ids = if self.driver == Driver::Mysql && row_ids.is_none() {
            Some(vec![unidentified_mysql_row(); result.rows.len()])
        } else {
            row_ids
        };
        Ok(TableDataResult {
            result,
            total,
            primary_key: pk,
            row_ids,
        })
    }
    pub async fn structure(&self, t: &TableRef) -> Result<QueryResult> {
        let t = self.table(t);
        let d = self.driver;
        let rel = literal(d, &qualified(d, &t));
        let sql = if d == Driver::Mysql {
            let version = self.server_version.trim_start_matches("MySQL ");
            let has_srid = version
                .split('.')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n >= 8)
                && !version.to_lowercase().contains("mariadb");
            format!(
                "SELECT c.ordinal_position AS `#`, c.column_name AS column_name, {} AS data_type, c.character_set_name AS character_set, c.collation_name AS collation, c.is_nullable AS is_nullable, c.column_default AS column_default, c.extra AS extra, (SELECT CONCAT(k.referenced_table_name, '(', k.referenced_column_name, ')') FROM information_schema.key_column_usage k WHERE k.table_schema=c.table_schema AND k.table_name=c.table_name AND k.column_name=c.column_name AND k.referenced_table_name IS NOT NULL LIMIT 1) AS foreign_key, c.column_comment AS comment FROM information_schema.columns c WHERE c.table_schema={} AND c.table_name={} ORDER BY c.ordinal_position;",
                if has_srid {
                    "CONCAT(c.column_type, IF(c.srs_id IS NULL, '', CONCAT(' SRID ', c.srs_id)))"
                } else {
                    "c.column_type"
                },
                literal(d, &t.schema),
                literal(d, &t.name)
            )
        } else {
            format!(
                "SELECT a.attnum AS \"#\", a.attname AS column_name, format_type(a.atttypid, a.atttypmod) AS data_type, co.collname AS collation, CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END AS is_nullable, pg_get_expr(d.adbin, d.adrelid) AS column_default, CASE WHEN a.attidentity <> '' THEN 'identity' WHEN a.attgenerated <> '' THEN 'generated' ELSE NULL END AS extra, (SELECT confrelid::regclass::text || '(' || (SELECT attname FROM pg_attribute WHERE attrelid = con.confrelid AND attnum = con.confkey[array_position(con.conkey, a.attnum)]) || ')' FROM pg_constraint con WHERE con.conrelid = a.attrelid AND con.contype = 'f' AND a.attnum = ANY(con.conkey) LIMIT 1) AS foreign_key, col_description(a.attrelid, a.attnum) AS comment FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum LEFT JOIN pg_collation co ON co.oid = a.attcollation AND a.attcollation <> 0 WHERE a.attrelid = {rel}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum;"
            )
        };
        self.query(&sql).await
    }
    pub async fn indexes(&self, t: &TableRef) -> Result<QueryResult> {
        let t = self.table(t);
        let d = self.driver;
        let rel = literal(d, &qualified(d, &t));
        let sql = if d == Driver::Mysql {
            format!(
                "SELECT index_name AS index_name, index_type AS index_algorithm, CASE MAX(non_unique) WHEN 0 THEN 'TRUE' ELSE 'FALSE' END AS is_unique, GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', ') AS column_name FROM information_schema.statistics WHERE table_schema={} AND table_name={} GROUP BY index_name, index_type ORDER BY index_name='PRIMARY' DESC, MIN(seq_in_index), index_name;",
                literal(d, &t.schema),
                literal(d, &t.name)
            )
        } else {
            format!(
                "SELECT ic.relname AS index_name, am.amname AS index_algorithm, CASE WHEN i.indisunique THEN 'TRUE' ELSE 'FALSE' END AS is_unique, (SELECT string_agg(pg_get_indexdef(i.indexrelid, k, true), ', ' ORDER BY k) FROM generate_series(1, i.indnkeyatts) k) AS column_name, pg_get_expr(i.indpred, i.indrelid) AS condition, (SELECT CASE c.contype WHEN 'p' THEN 'primary' WHEN 'u' THEN 'unique' WHEN 'x' THEN 'exclusion' END FROM pg_constraint c WHERE c.conindid = i.indexrelid AND c.conrelid = i.indrelid LIMIT 1) AS constraint_type FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_am am ON am.oid = ic.relam WHERE i.indrelid = {rel}::regclass ORDER BY i.indisprimary DESC, ic.relname;"
            )
        };
        self.query(&sql).await
    }
    pub async fn table_info(&self, t: &TableRef) -> Result<TableInfo> {
        let t = self.table(t);
        let d = self.driver;
        let rel = literal(d, &qualified(d, &t));
        let sql = if d == Driver::Mysql {
            format!(
                "SELECT data_length, index_length, table_comment FROM information_schema.tables WHERE table_schema={} AND table_name={};",
                literal(d, &t.schema),
                literal(d, &t.name)
            )
        } else {
            format!(
                "SELECT pg_size_pretty(pg_relation_size({rel}::regclass)), pg_size_pretty(pg_indexes_size({rel}::regclass)), pg_size_pretty(pg_total_relation_size({rel}::regclass)), obj_description({rel}::regclass, 'pg_class');"
            )
        };
        let r = self.query(&sql).await?.rows;
        let get = |i| {
            r.first()
                .and_then(|r| r.get(i))
                .and_then(|v: &Option<String>| v.clone())
                .unwrap_or_default()
        };
        Ok(if d == Driver::Mysql {
            let a = get(0).parse().unwrap_or(0.0);
            let b = get(1).parse().unwrap_or(0.0);
            TableInfo {
                data_size: format_bytes(a),
                index_size: format_bytes(b),
                total_size: format_bytes(a + b),
                comment: get(2),
            }
        } else {
            TableInfo {
                data_size: get(0),
                index_size: get(1),
                total_size: get(2),
                comment: get(3),
            }
        })
    }
    pub async fn ddl(&self, t: &TableRef) -> Result<String> {
        let t = self.table(t);
        let d = self.driver;
        let rel = qualified(d, &t);
        let lit = literal(d, &rel);
        if d == Driver::Mysql {
            return Ok(self
                .query(&format!("SHOW CREATE TABLE {rel};"))
                .await?
                .rows
                .first()
                .and_then(|r| r.get(1))
                .cloned()
                .flatten()
                .unwrap_or_default());
        }
        let cols=self.query(&format!("SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(d.adbin, d.adrelid) FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum WHERE a.attrelid = {lit}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum")).await?.rows;
        let cons=self.query(&format!("SELECT conname, pg_get_constraintdef(oid, true) FROM pg_constraint WHERE conrelid = {lit}::regclass ORDER BY contype = 'p' DESC, conname")).await?.rows;
        let idx=self.query(&format!("SELECT pg_get_indexdef(indexrelid) FROM pg_index WHERE indrelid = {lit}::regclass AND NOT indisprimary AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = indexrelid)")).await?.rows;
        let mut lines = cols
            .iter()
            .map(|r| {
                format!(
                    "  {} {}{}{}",
                    quote_ident(d, r[0].as_deref().unwrap_or("")),
                    r[1].as_deref().unwrap_or(""),
                    if r[2].as_deref() == Some("true") {
                        " NOT NULL"
                    } else {
                        ""
                    },
                    r[3].as_ref()
                        .filter(|s| !s.is_empty())
                        .map_or(String::new(), |s| format!(" DEFAULT {s}"))
                )
            })
            .collect::<Vec<_>>();
        for r in cons {
            lines.push(format!(
                "  CONSTRAINT {} {}",
                quote_ident(d, r[0].as_deref().unwrap_or("")),
                r[1].as_deref().unwrap_or("")
            ));
        }
        let mut out = format!("CREATE TABLE {rel} (\n{}\n);", lines.join(",\n"));
        for r in idx {
            out += &format!("\n\n{};", r[0].as_deref().unwrap_or(""));
        }
        Ok(out)
    }
}

/// A MySQL row id that every write refuses, for rows Telecat couldn't identify exactly.
fn unidentified_mysql_row() -> Vec<String> {
    vec!["mysql".into()]
}
/// mysql2.format display semantics (including its quote/backslash escapes), applied only where
/// the server sees placeholders: never inside quoted identifiers, strings or comments.
fn mysql_preview(sql: &str, params: &[serde_json::Value]) -> String {
    let mut args = params.iter();
    let mut out = String::new();
    let mut done = 0;
    for code in telecat_core::guard::code_ranges(sql, Driver::Mysql) {
        out.push_str(&sql[done..code.start]);
        if !format_placeholders(&sql[code.clone()], &mut args, &mut out) {
            out.push_str(&sql[code.end..]);
            return out;
        }
        done = code.end;
    }
    out.push_str(&sql[done..]);
    out
}
/// False once the parameters run out; the rest of the text is then kept as written.
fn format_placeholders<'a>(
    sql: &str,
    args: &mut impl Iterator<Item = &'a serde_json::Value>,
    out: &mut String,
) -> bool {
    let mut i = 0;
    while let Some(rel) = sql[i..].find('?') {
        let start = i + rel;
        out.push_str(&sql[i..start]);
        let end = start + sql[start..].bytes().take_while(|b| *b == b'?').count();
        if end - start > 2 {
            out.push_str(&sql[start..end]);
            i = end;
            continue;
        }
        let Some(v) = args.next() else {
            out.push_str(&sql[start..]);
            return false;
        };
        if end - start == 2 {
            out.push_str(&quote_ident(
                Driver::Mysql,
                &telecat_core::changes::js_string(v),
            ));
        } else if v.is_null() {
            out.push_str("NULL");
        } else if v.is_number() || v.is_boolean() {
            out.push_str(&v.to_string());
        } else {
            out.push('\'');
            for c in telecat_core::changes::js_string(v).chars() {
                match c {
                    '\0' => out.push_str("\\0"),
                    '\u{8}' => out.push_str("\\b"),
                    '\t' => out.push_str("\\t"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\u{1a}' => out.push_str("\\Z"),
                    '"' => out.push_str("\\\""),
                    '\'' => out.push_str("\\'"),
                    '\\' => out.push_str("\\\\"),
                    _ => out.push(c),
                }
            }
            out.push('\'');
        }
        i = end;
    }
    out.push_str(&sql[i..]);
    true
}
