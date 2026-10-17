use crate::{Session, format_bytes};
use anyhow::{Context, Result};
use telecat_core::{
    changes::{Stmt, inline_params, literal, qualified, quote_ident},
    filters::{Where, build_where},
    types::*,
};
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
    async fn select(
        &self,
        req: &TableDataRequest,
    ) -> Result<(String, Where, Vec<String>, String, bool)> {
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
        let ids = if d == Driver::Postgres {
            self.strings(&format!(
                "SELECT relkind::text FROM pg_class WHERE oid = {}::regclass",
                literal(d, &rel)
            ))
            .await?
            .first()
            .is_some_and(|s| s == "r" || s == "p")
        } else {
            false
        };
        let sql = format!(
            "SELECT *{} FROM {rel}{}{order} LIMIT {} OFFSET {};",
            if ids {
                ", tableoid::oid::text AS \"__telecat_tableoid\", ctid::text AS \"__telecat_ctid\""
            } else {
                ""
            },
            w.sql,
            req.limit,
            req.offset
        );
        Ok((sql, w, pk, rel, ids))
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
    pub async fn table_data(&self, req: &TableDataRequest) -> Result<TableDataResult> {
        let (sql, w, pk, rel, ids) = self.select(req).await?;
        let mut result = self.run(&sql, &w.params).await?;
        let total = self
            .run(
                &format!(
                    "SELECT COUNT(*) {}count FROM {rel}{};",
                    if self.driver == Driver::Mysql {
                        "as "
                    } else {
                        "AS "
                    },
                    w.sql
                ),
                &w.params,
            )
            .await?
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_ref())
            .context("Missing row count")?
            .parse()?;
        let row_ids = if ids {
            let n = result
                .columns
                .len()
                .checked_sub(2)
                .context("Missing physical identity columns")?;
            result.columns.truncate(n);
            if let Some(v) = &mut result.numeric {
                v.truncate(n);
            }
            if let Some(v) = &mut result.binary {
                v.truncate(n);
            }
            Some(
                result
                    .rows
                    .iter_mut()
                    .map(|r| {
                        r.split_off(n)
                            .into_iter()
                            .map(Option::unwrap_or_default)
                            .collect()
                    })
                    .collect(),
            )
        } else {
            None
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

/// mysql2.format display semantics (including its quote/backslash escapes).
fn mysql_preview(sql: &str, params: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut args = params.iter();
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
            return out;
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
    out
}
