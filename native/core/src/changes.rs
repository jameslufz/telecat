use crate::{
    sql::{re, trim},
    types::*,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub sql: String,
    pub params: Vec<Value>,
    pub expect: u64,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stmt {
    pub sql: String,
    pub params: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
}
impl Stmt {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: vec![],
            expect: None,
            check: None,
        }
    }
}
/// JSON null is SQL NULL. The explicit object is the fixture/wire encoding of JS undefined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Cell {
    Text(String),
    Null,
    Unset {
        #[serde(rename = "$undefined")]
        undefined: bool,
    },
}
impl Cell {
    pub fn unset() -> Self {
        Self::Unset { undefined: true }
    }
    pub fn value(&self) -> Option<&str> {
        if let Self::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn is_unset(&self) -> bool {
        matches!(self, Self::Unset { .. })
    }
}
impl From<Option<String>> for Cell {
    fn from(s: Option<String>) -> Self {
        s.map_or(Self::Null, Self::Text)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditRow {
    pub key: String,
    pub orig: Option<Vec<Option<String>>>,
    pub cur: Vec<Cell>,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<Vec<String>>,
}
pub fn row_key() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("r{}", SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}
pub fn to_edit_rows(rows: &[Vec<Option<String>>], rids: Option<&[Vec<String>]>) -> Vec<EditRow> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| EditRow {
            key: row_key(),
            orig: Some(r.clone()),
            cur: r.iter().cloned().map(Cell::from).collect(),
            deleted: false,
            rid: rids.and_then(|v| v.get(i)).cloned(),
        })
        .collect()
}
pub fn is_new_row(r: &EditRow) -> bool {
    r.orig.is_none()
}
pub fn cell_modified(r: &EditRow, i: usize) -> bool {
    r.orig.as_ref().is_some_and(|o| {
        r.cur.get(i).cloned().unwrap_or_else(Cell::unset)
            != o.get(i)
                .cloned()
                .map(Cell::from)
                .unwrap_or_else(Cell::unset)
    })
}
pub fn is_modified(r: &EditRow) -> bool {
    r.orig.is_some() && (0..r.cur.len()).any(|i| cell_modified(r, i))
}
pub fn has_changes(rows: &[EditRow]) -> bool {
    rows.iter().any(|r| {
        if r.orig.is_none() {
            !r.deleted
        } else {
            r.deleted || is_modified(r)
        }
    })
}
pub fn quote_ident(d: Driver, n: &str) -> String {
    let q = if d == Driver::Mysql { '`' } else { '"' };
    format!("{q}{}{q}", n.replace(q, &format!("{q}{q}")))
}
pub fn qualified(d: Driver, t: &TableRef) -> String {
    format!("{}.{}", quote_ident(d, &t.schema), quote_ident(d, &t.name))
}
pub fn literal(d: Driver, s: &str) -> String {
    format!(
        "'{}'",
        if d == Driver::Mysql {
            s.replace('\\', "\\\\")
        } else {
            s.into()
        }
        .replace('\'', "''")
    )
}
struct Params {
    d: Driver,
    list: Vec<Value>,
}
impl Params {
    fn new(d: Driver) -> Self {
        Self { d, list: vec![] }
    }
    fn add(&mut self, s: Option<&str>) -> String {
        self.list
            .push(s.map_or(Value::Null, |s| Value::String(s.into())));
        if self.d == Driver::Mysql {
            "?".into()
        } else {
            format!("${}", self.list.len())
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMeta {
    pub columns: Vec<String>,
    pub primary_key: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<Vec<bool>>,
}
fn hex(s: &str) -> bool {
    re(r"(?i)^0x[0-9a-f]*$").is_match(s)
}
fn binary(m: &TableMeta, i: usize) -> bool {
    m.binary
        .as_ref()
        .and_then(|v| v.get(i))
        .copied()
        .unwrap_or(false)
}
fn row_where(d: Driver, m: &TableMeta, r: &EditRow, p: &mut Params) -> Result<(String, bool)> {
    let orig = r
        .orig
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cannot identify a new row"))?;
    let cond = |c: &str, exact: bool, p: &mut Params| -> Result<String> {
        let i = m
            .columns
            .iter()
            .position(|v| v == c)
            .ok_or_else(|| anyhow::anyhow!("Unknown key column {c}"))?;
        let v = orig
            .get(i)
            .ok_or_else(|| anyhow::anyhow!("Missing original cell"))?;
        let col = quote_ident(d, c);
        let Some(v) = v else {
            return Ok(format!("{col} IS NULL"));
        };
        Ok(if d == Driver::Mysql && binary(m, i) && hex(v) {
            format!("{col} = UNHEX({})", p.add(Some(&v[2..])))
        } else if exact {
            format!("{col} = {}", p.add(Some(v)))
        } else if d == Driver::Mysql {
            format!("BINARY {col} = BINARY {}", p.add(Some(v)))
        } else {
            format!("{col}::text = {}", p.add(Some(v)))
        })
    };
    if d == Driver::Postgres {
        if let Some(rid) = &r.rid {
            if rid.len() != 2 {
                bail!("Invalid physical row identity");
            }
            let mut parts = vec![
                format!("tableoid = {}::oid", p.add(Some(&rid[0]))),
                format!("ctid = {}::tid", p.add(Some(&rid[1]))),
            ];
            for c in &m.primary_key {
                parts.push(cond(c, true, p)?);
            }
            return Ok((parts.join(" AND "), true));
        }
    }
    if !m.primary_key.is_empty() {
        return Ok((
            m.primary_key
                .iter()
                .map(|c| cond(c, true, p))
                .collect::<Result<Vec<_>>>()?
                .join(" AND "),
            true,
        ));
    }
    if m.columns.is_empty() {
        bail!("Cannot identify the row: no columns");
    }
    Ok((
        m.columns
            .iter()
            .map(|c| cond(c, false, p))
            .collect::<Result<Vec<_>>>()?
            .join(" AND "),
        false,
    ))
}
fn value(d: Driver, m: &TableMeta, i: usize, v: &Cell, p: &mut Params) -> String {
    match v.value() {
        None => "NULL".into(),
        Some(s) if d == Driver::Mysql && binary(m, i) && hex(s) => {
            format!("UNHEX({})", p.add(Some(&s[2..])))
        }
        Some(s) => p.add(Some(s)),
    }
}
pub const NOT_UNIQUE: &str = "This row can't be identified uniquely: the table has no primary key and another row has the same values. Nothing was changed — add a primary key to edit it.";
fn one_row(
    d: Driver,
    t: &TableRef,
    m: &TableMeta,
    r: &EditRow,
    head: String,
    mut p: Params,
) -> Result<Stmt> {
    let (w, unique) = row_where(d, m, r, &mut p)?;
    let mut st = Stmt {
        sql: format!(
            "{head} WHERE {w}{};",
            if !unique && d == Driver::Mysql {
                " LIMIT 1"
            } else {
                ""
            }
        ),
        params: p.list,
        expect: Some(1),
        check: None,
    };
    if !unique {
        let mut cp = Params::new(d);
        let (w, _) = row_where(d, m, r, &mut cp)?;
        st.check = Some(Check {
            sql: format!("SELECT COUNT(*) FROM {} WHERE {w}", qualified(d, t)),
            params: cp.list,
            expect: 1,
            message: NOT_UNIQUE.into(),
        });
    }
    Ok(st)
}
pub fn data_statements(
    d: Driver,
    t: &TableRef,
    m: &TableMeta,
    rows: &[EditRow],
) -> Result<Vec<Stmt>> {
    let mut out = vec![];
    let table = qualified(d, t);
    for r in rows {
        if r.orig.is_none() {
            if r.deleted {
                continue;
            }
            let mut p = Params::new(d);
            let mut cols = vec![];
            let mut vals = vec![];
            for (i, v) in r.cur.iter().enumerate() {
                if v.is_unset() {
                    continue;
                }
                let col = m
                    .columns
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("Missing column"))?;
                cols.push(quote_ident(d, col));
                vals.push(value(d, m, i, v, &mut p));
            }
            out.push(Stmt {
                sql: if !cols.is_empty() {
                    format!(
                        "INSERT INTO {table} ({}) VALUES ({});",
                        cols.join(", "),
                        vals.join(", ")
                    )
                } else if d == Driver::Mysql {
                    format!("INSERT INTO {table} () VALUES ();")
                } else {
                    format!("INSERT INTO {table} DEFAULT VALUES;")
                },
                params: p.list,
                expect: Some(1),
                check: None,
            });
        } else if r.deleted {
            out.push(one_row(
                d,
                t,
                m,
                r,
                format!("DELETE FROM {table}"),
                Params::new(d),
            )?);
        } else if is_modified(r) {
            let mut p = Params::new(d);
            let mut sets = vec![];
            for (i, v) in r.cur.iter().enumerate() {
                if cell_modified(r, i) {
                    let col = m
                        .columns
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("Missing column"))?;
                    sets.push(format!(
                        "{} = {}",
                        quote_ident(d, col),
                        value(d, m, i, v, &mut p)
                    ));
                }
            }
            out.push(one_row(
                d,
                t,
                m,
                r,
                format!("UPDATE {table} SET {}", sets.join(", ")),
                p,
            )?);
        }
    }
    Ok(out)
}
fn field<'a>(cols: &[String], row: &'a [Cell], name: &str) -> Option<&'a str> {
    cols.iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i))
        .and_then(Cell::value)
}
fn orig_cells(r: &EditRow) -> Option<Vec<Cell>> {
    r.orig
        .as_ref()
        .map(|v| v.iter().cloned().map(Cell::from).collect())
}
fn mysql_default(v: Option<&str>, extra: &str, ty: &str, server: bool) -> Option<String> {
    v.map(|v|{
    if re("(?i)DEFAULT_GENERATED").is_match(extra){return if re(r"(?i)^CURRENT_TIMESTAMP(\(\d*\))?$").is_match(v)||re(r"^\(.*\)$").is_match(v){v.into()}else{format!("({v})")};}
    if server{return if re("(?i)^bit").is_match(ty)&&re("(?i)^b'[01]*'$").is_match(v){v.into()}else{literal(Driver::Mysql,v)};}
    if re(r"(?is)^NULL$|^CURRENT_TIMESTAMP(\(\d*\))?$|^b'[01]*'$|^\(.*\)$|^'.*'$").is_match(v)||(re("(?i)^(tiny|small|medium|big)?int|^integer|^decimal|^numeric|^float|^double|^real|^bit|^bool|^serial|^bigserial|^smallserial").is_match(ty)&&re(r"^-?\d+(\.\d+)?$").is_match(v)){v.into()}else{literal(Driver::Mysql,v)}
})
}
fn pg_default(v: Option<&str>) -> Option<String> {
    v.map(|v| {
        if re(r#"(?is-u)^-?\d+(\.\d+)?$|^(true|false|null)$|^'.*'(::[\w\s."\[\]]+)?$|[()]|::"#)
            .is_match(v)
        {
            v.into()
        } else {
            literal(Driver::Postgres, v)
        }
    })
}
fn column_def(d: Driver, cols: &[String], row: &[Cell], orig: Option<&[Cell]>) -> Result<String> {
    let f = |n| field(cols, row, n);
    let name = f("column_name").unwrap_or("");
    let ty = f("data_type").unwrap_or("");
    let extra = f("extra").unwrap_or("");
    if d == Driver::Mysql && re(r"(?i-u)\b(VIRTUAL|STORED) GENERATED\b").is_match(extra) {
        bail!("Column {name}: editing generated columns isn't supported");
    }
    if trim(name).is_empty() {
        bail!("A column needs a name");
    }
    if trim(ty).is_empty() {
        bail!("Column {name}: data_type is required");
    }
    let mut s = format!("{} {ty}", quote_ident(d, name));
    if d == Driver::Mysql {
        if let Some(coll) = f("collation").filter(|s| !s.is_empty()) {
            if re("(?i)char|text|enum|set").is_match(ty) {
                s += &format!(" COLLATE {coll}");
            }
        }
    } else if re("(?i)identity").is_match(extra) {
        s += " GENERATED BY DEFAULT AS IDENTITY";
    }
    if f("is_nullable").unwrap_or("YES").eq_ignore_ascii_case("NO") {
        s += " NOT NULL";
    } else if d == Driver::Mysql {
        s += " NULL";
    }
    let untouched = orig.is_some_and(|o| {
        field(cols, o, "column_default") == f("column_default")
            && field(cols, o, "extra") == Some(extra)
    });
    let def = if d == Driver::Mysql {
        mysql_default(f("column_default"), extra, ty, untouched)
    } else {
        pg_default(f("column_default"))
    };
    if let Some(def) = def {
        s += &format!(" DEFAULT {def}");
    }
    if d == Driver::Mysql {
        let clean = re("(?i)DEFAULT_GENERATED").replace_all(extra, "");
        let clean = trim(&clean);
        if !clean.is_empty() {
            s += &format!(
                " {}",
                if clean.to_ascii_uppercase().starts_with("ON UPDATE") {
                    clean.into()
                } else {
                    clean.to_uppercase()
                }
            );
        }
        if let Some(cm) = f("comment").filter(|s| !s.is_empty()) {
            s += &format!(" COMMENT {}", literal(d, cm));
        }
    }
    Ok(s)
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureChange {
    pub table: TableRef,
    pub new_name: String,
    pub is_new: bool,
    pub primary_key: Vec<String>,
    pub columns: Vec<String>,
    pub rows: Vec<EditRow>,
}
pub fn structure_statements(d: Driver, c: &StructureChange) -> Result<Vec<Stmt>> {
    let mut out = vec![];
    let live = c.rows.iter().filter(|r| !r.deleted).collect::<Vec<_>>();
    let target = TableRef {
        schema: c.table.schema.clone(),
        name: c.new_name.clone(),
    };
    let table = qualified(d, &c.table);
    if c.is_new {
        if trim(&c.new_name).is_empty() {
            bail!("The new table needs a name");
        }
        if live.is_empty() {
            bail!("A table needs at least one column");
        }
        let mut defs = live
            .iter()
            .map(|r| column_def(d, &c.columns, &r.cur, None))
            .collect::<Result<Vec<_>>>()?;
        if !c.primary_key.is_empty() {
            defs.push(format!(
                "PRIMARY KEY ({})",
                c.primary_key
                    .iter()
                    .map(|k| quote_ident(d, k))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push(Stmt::new(format!(
            "CREATE TABLE {} (\n  {}\n);",
            qualified(d, &target),
            defs.join(",\n  ")
        )));
        if d == Driver::Postgres {
            for r in live {
                if let Some(cm) = field(&c.columns, &r.cur, "comment").filter(|s| !s.is_empty()) {
                    out.push(Stmt::new(format!(
                        "COMMENT ON COLUMN {}.{} IS {};",
                        qualified(d, &target),
                        quote_ident(d, field(&c.columns, &r.cur, "column_name").unwrap_or("")),
                        literal(d, cm)
                    )));
                }
            }
        }
        return Ok(out);
    }
    if d == Driver::Mysql {
        let mut clauses = vec![];
        let mut prev: Option<&str> = None;
        for r in &c.rows {
            let orig = orig_cells(r);
            let on = orig
                .as_ref()
                .and_then(|v| field(&c.columns, v, "column_name"))
                .unwrap_or("");
            if r.deleted {
                if r.orig.is_some() {
                    clauses.push(format!("DROP COLUMN {}", quote_ident(d, on)));
                }
                continue;
            }
            let name = field(&c.columns, &r.cur, "column_name");
            if r.orig.is_none() {
                clauses.push(format!(
                    "ADD COLUMN {}{}",
                    column_def(d, &c.columns, &r.cur, None)?,
                    prev.filter(|s| !s.is_empty())
                        .map_or(" FIRST".into(), |s| format!(" AFTER {}", quote_ident(d, s)))
                ));
            } else if is_modified(r) {
                clauses.push(format!(
                    "CHANGE COLUMN {} {}",
                    quote_ident(d, on),
                    column_def(d, &c.columns, &r.cur, orig.as_deref())?
                ));
            }
            prev = name;
        }
        if !clauses.is_empty() {
            out.push(Stmt::new(format!(
                "ALTER TABLE {table}\n  {};",
                clauses.join(",\n  ")
            )));
        }
    } else {
        for r in &c.rows {
            let orig = orig_cells(r);
            let on = orig
                .as_ref()
                .and_then(|v| field(&c.columns, v, "column_name"))
                .unwrap_or("");
            if r.deleted {
                if r.orig.is_some() {
                    out.push(Stmt::new(format!(
                        "ALTER TABLE {table} DROP COLUMN {};",
                        quote_ident(d, on)
                    )));
                }
                continue;
            }
            let f = |n| field(&c.columns, &r.cur, n);
            let name = f("column_name").unwrap_or("");
            let col = quote_ident(d, name);
            if r.orig.is_some() && f("column_name").is_none() {
                bail!("Cannot read properties of null (reading 'replace')");
            }
            if r.orig.is_none() {
                out.push(Stmt::new(format!(
                    "ALTER TABLE {table} ADD COLUMN {};",
                    column_def(d, &c.columns, &r.cur, None)?
                )));
                if let Some(cm) = f("comment").filter(|s| !s.is_empty()) {
                    out.push(Stmt::new(format!(
                        "COMMENT ON COLUMN {table}.{col} IS {};",
                        literal(d, cm)
                    )));
                }
                continue;
            }
            let o = |n| orig.as_ref().and_then(|v| field(&c.columns, v, n));
            if name != on {
                out.push(Stmt::new(format!(
                    "ALTER TABLE {table} RENAME COLUMN {} TO {col};",
                    quote_ident(d, on)
                )));
            }
            if f("data_type") != o("data_type") {
                let ty = f("data_type").unwrap_or("null");
                out.push(Stmt{sql:format!("ALTER TABLE {table} ALTER COLUMN {col} TYPE {ty} USING {col}::{ty};"),params:vec![],expect:None,check:Some(Check{sql:format!("SELECT COUNT(*) FROM {table} WHERE {col} IS NOT NULL AND ({col}::{ty})::text IS DISTINCT FROM {col}::text"),params:vec![],expect:0,message:format!("Changing {name} to {ty} would alter or truncate existing values. Nothing was changed.")})});
            }
            if f("is_nullable").unwrap_or("YES").to_uppercase()
                != o("is_nullable").unwrap_or("YES").to_uppercase()
            {
                out.push(Stmt::new(format!(
                    "ALTER TABLE {table} ALTER COLUMN {col} {} NOT NULL;",
                    if f("is_nullable").unwrap_or("YES").eq_ignore_ascii_case("NO") {
                        "SET"
                    } else {
                        "DROP"
                    }
                )));
            }
            if f("column_default") != o("column_default") {
                out.push(Stmt::new(format!(
                    "ALTER TABLE {table} ALTER COLUMN {col} {};",
                    pg_default(f("column_default"))
                        .map_or("DROP DEFAULT".into(), |v| format!("SET DEFAULT {v}"))
                )));
            }
            if f("comment") != o("comment") {
                out.push(Stmt::new(format!(
                    "COMMENT ON COLUMN {table}.{col} IS {};",
                    f("comment")
                        .filter(|s| !s.is_empty())
                        .map_or("NULL".into(), |s| literal(d, s))
                )));
            }
        }
    }
    if c.new_name != c.table.name {
        out.push(Stmt::new(if d == Driver::Mysql {
            format!("RENAME TABLE {table} TO {};", qualified(d, &target))
        } else {
            format!(
                "ALTER TABLE {table} RENAME TO {};",
                quote_ident(d, &c.new_name)
            )
        }));
    }
    Ok(out)
}
pub fn index_statements(
    d: Driver,
    table: &TableRef,
    columns: &[String],
    rows: &[EditRow],
) -> Result<Vec<Stmt>> {
    let mut out = vec![];
    let t = qualified(d, table);
    for r in rows {
        if !(if r.orig.is_some() {
            r.deleted || is_modified(r)
        } else {
            !r.deleted
        }) {
            continue;
        }
        let mut parts = vec![];
        if let Some(o) = orig_cells(r) {
            let n = field(columns, &o, "index_name").unwrap_or("");
            parts.push(if d == Driver::Mysql {
                if n == "PRIMARY" {
                    "DROP PRIMARY KEY".into()
                } else {
                    format!("DROP INDEX {}", quote_ident(d, n))
                }
            } else if field(columns, &o, "constraint_type").is_some_and(|s| !s.is_empty()) {
                format!("ALTER TABLE {t} DROP CONSTRAINT {};", quote_ident(d, n))
            } else {
                format!(
                    "DROP INDEX {}.{};",
                    quote_ident(d, &table.schema),
                    quote_ident(d, n)
                )
            });
        }
        if !r.deleted {
            let f = |n| field(columns, &r.cur, n);
            let name = trim(f("index_name").unwrap_or(""));
            let cols = f("column_name")
                .unwrap_or("")
                .split(',')
                .map(trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    if re(r"(?-u)^[A-Za-z_][\w$]*$").is_match(s) {
                        quote_ident(d, s)
                    } else {
                        s.into()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            if name.is_empty() {
                bail!("An index needs a name");
            }
            if cols.is_empty() {
                bail!("Index {name}: choose at least one column");
            }
            let algo = trim(f("index_algorithm").unwrap_or(""));
            let unique = if f("is_unique").unwrap_or("").eq_ignore_ascii_case("TRUE") {
                "UNIQUE "
            } else {
                ""
            };
            let q = quote_ident(d, name);
            parts.push(if d == Driver::Mysql {
                if name == "PRIMARY" {
                    format!("ADD PRIMARY KEY ({cols})")
                } else if re("(?i)^(FULLTEXT|SPATIAL)$").is_match(algo) {
                    format!("ADD {} INDEX {q} ({cols})", algo.to_uppercase())
                } else {
                    format!(
                        "ADD {unique}INDEX {q} ({cols}){}",
                        if !algo.is_empty() && !algo.eq_ignore_ascii_case("DEFAULT") {
                            format!(" USING {}", algo.to_uppercase())
                        } else {
                            String::new()
                        }
                    )
                }
            } else if f("constraint_type") == Some("primary") {
                format!("ALTER TABLE {t} ADD CONSTRAINT {q} PRIMARY KEY ({cols});")
            } else {
                format!(
                    "CREATE {unique}INDEX {q} ON {t}{} ({cols}){};",
                    if !algo.is_empty() && !algo.eq_ignore_ascii_case("DEFAULT") {
                        format!(" USING {}", algo.to_lowercase())
                    } else {
                        String::new()
                    },
                    f("condition")
                        .filter(|s| !s.is_empty())
                        .map_or(String::new(), |s| format!(" WHERE {s}"))
                )
            });
        }
        if d == Driver::Mysql {
            out.push(Stmt::new(format!("ALTER TABLE {t} {};", parts.join(", "))));
        } else {
            out.extend(parts.into_iter().map(Stmt::new));
        }
    }
    Ok(out)
}
pub fn table_op_statement(d: Driver, t: &TableRef, truncate: bool, view: bool) -> Stmt {
    Stmt::new(format!(
        "{} {};",
        if truncate {
            "TRUNCATE TABLE"
        } else if view {
            "DROP VIEW"
        } else {
            "DROP TABLE"
        },
        qualified(d, t)
    ))
}
pub fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Number(n) => js_number(n.as_f64().unwrap_or(f64::NAN)),
        _ => v.to_string(),
    }
}
/// ECMAScript Number::toString, including exponent thresholds and negative zero.
pub fn js_number(n: f64) -> String {
    ryu_js::Buffer::new().format(n).to_owned()
}
pub fn inline_params(d: Driver, st: &Stmt) -> String {
    let mut i = 0;
    re(if d == Driver::Mysql {
        r"\?"
    } else {
        r"\$(\d+)"
    })
    .replace_all(&st.sql, |m: &regex::Captures| {
        let k = if d == Driver::Mysql {
            let k = i;
            i += 1;
            k
        } else {
            m[1].parse::<usize>().unwrap_or(0).wrapping_sub(1)
        };
        st.params
            .get(k)
            .filter(|v| !v.is_null())
            .map_or("NULL".into(), |v| literal(d, &js_string(v)))
    })
    .into_owned()
}
