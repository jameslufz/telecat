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
pub(crate) struct Params {
    d: Driver,
    pub(crate) list: Vec<Value>,
}
impl Params {
    pub(crate) fn new(d: Driver) -> Self {
        Self { d, list: vec![] }
    }
    pub(crate) fn add(&mut self, s: Option<&str>) -> String {
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
fn unhex(s: &str) -> Option<Vec<u8>> {
    let digits = s.get(2..)?;
    if digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
        .collect()
}
/// Mirrors the grid's binary display: valid UTF-8 without control characters is shown as text,
/// anything else as `0x…`.
pub fn binary_shown_as_text(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|s| {
        !s.chars()
            .any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{fffd}'))
    })
}
fn binary(m: &TableMeta, i: usize) -> bool {
    m.binary
        .as_ref()
        .and_then(|v| v.get(i))
        .copied()
        .unwrap_or(false)
}

/// How a MySQL column takes part in exact row identity. See [`mysql_identity_columns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind {
    /// TIMESTAMP: its text depends on the session time zone and repeats in a DST fall-back hour.
    Timestamp,
    /// FLOAT: the server's text keeps 6 significant digits.
    Float,
    Double,
    /// Bytes the grid may display as text or hex.
    Binary,
    /// BIT: shown as bytes, compared as a number.
    Bit,
    /// Integers and DECIMAL: shown exactly, but compared with a string as a DOUBLE, which can't
    /// tell 2^53 + 1 from 2^53, so keys are compared as their own type.
    Signed,
    Unsigned,
    Decimal,
    Other,
}
impl IdentityKind {
    /// From information_schema `data_type` and `column_type`.
    pub fn from_mysql_type(data_type: &str, column_type: &str) -> Self {
        let unsigned = column_type.to_ascii_lowercase().contains("unsigned");
        match data_type.to_ascii_lowercase().as_str() {
            "timestamp" => Self::Timestamp,
            "float" => Self::Float,
            "double" | "real" => Self::Double,
            "bit" => Self::Bit,
            "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => {
                Self::Binary
            }
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" if unsigned => {
                Self::Unsigned
            }
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" => Self::Signed,
            "decimal" | "numeric" => Self::Decimal,
            _ => Self::Other,
        }
    }
    const CODES: [(Self, char); 9] = [
        (Self::Timestamp, 't'),
        (Self::Float, 'f'),
        (Self::Double, 'o'),
        (Self::Binary, 'b'),
        (Self::Bit, 'i'),
        (Self::Signed, 's'),
        (Self::Unsigned, 'u'),
        (Self::Decimal, 'd'),
        (Self::Other, 'c'),
    ];
    fn code(self) -> char {
        Self::CODES
            .iter()
            .find(|k| k.0 == self)
            .map_or('c', |k| k.1)
    }
    fn from_code(c: char) -> Option<Self> {
        Self::CODES.iter().find(|k| k.1 == c).map(|k| k.0)
    }
    /// The exact value a key column is identified by, selected next to the row. Every key
    /// column carries one: display text is session-formatted and may not round-trip.
    fn key_expression(self, col: &str) -> String {
        match self {
            Self::Binary => format!("HEX({col})"),
            Self::Timestamp => format!("UNIX_TIMESTAMP({col})"),
            Self::Float | Self::Double => format!("CAST({col} + 0e0 AS CHAR)"),
            Self::Bit => format!("CAST({col} AS UNSIGNED)"),
            _ => format!("CAST({col} AS CHAR)"),
        }
    }
    /// Matches a key column exactly. Integers go through DECIMAL(20,0): in a prepared statement
    /// MySQL clamps `CAST(? AS UNSIGNED)` of a string ≥ 2^63 to 2^63-1, and compared with a
    /// string an integer becomes a DOUBLE. Text-like keys add a byte-exact comparison so a change
    /// of case or trailing spaces under a case-insensitive or PAD SPACE collation doesn't match.
    pub(crate) fn key_condition(self, col: &str, value: &str, p: &mut Params) -> String {
        match self {
            Self::Binary => format!("{col} = UNHEX({})", p.add(Some(value))),
            Self::Timestamp => format!("UNIX_TIMESTAMP({col}) = {}", p.add(Some(value))),
            Self::Float | Self::Double => format!("{col} = {} + 0e0", p.add(Some(value))),
            Self::Bit | Self::Unsigned | Self::Signed => {
                format!("{col} = CAST({} AS DECIMAL(20, 0))", p.add(Some(value)))
            }
            // DECIMAL keys can be wider than CAST(? AS DECIMAL(65,30)) holds, so the indexed
            // comparison may only narrow; the byte-exact text decides.
            Self::Decimal | Self::Other => format!(
                "{col} = {} AND BINARY CAST({col} AS CHAR) = {}",
                p.add(Some(value)),
                p.add(Some(value))
            ),
        }
    }
}
const MYSQL_ROW_ID: &str = "mysql";
/// SHA-256 of the row's columns as the server stores them. Built the same way when the rows are
/// read and when a write looks for one. When the row is found by its key, the key columns are
/// left out: the key comparison already pins them, and MySQL substitutes the key's constant into
/// the rest of the WHERE, which would change how they hash.
fn mysql_fingerprint(columns: &[String], kinds: &[IdentityKind], keys: &[String]) -> String {
    let mut parts = vec!["''".to_owned()];
    parts.extend(
        columns
            .iter()
            .zip(kinds)
            .filter(|(c, _)| !keys.contains(c))
            .map(|(c, k)| {
                let col = quote_ident(Driver::Mysql, c);
                let exact = match k {
                    IdentityKind::Timestamp => format!("UNIX_TIMESTAMP({col})"),
                    IdentityKind::Float => format!("CAST({col} + 0e0 AS BINARY)"),
                    IdentityKind::Bit => format!("CAST({col} AS UNSIGNED)"),
                    _ => format!("CAST({col} AS BINARY)"),
                };
                format!("IFNULL(SHA2({exact}, 256), '-')")
            }),
    );
    format!("SHA2(CONCAT_WS(',', {}), 256)", parts.join(", "))
}
/// Extra `SELECT` expressions that give each MySQL row an exact identity: a fingerprint, plus an
/// exact value for each primary-key column whose display can't identify it (or that isn't
/// displayed, like an invisible primary key). Pass their values, in order, to [`mysql_row_id`].
pub fn mysql_identity_columns(
    columns: &[String],
    kinds: &[IdentityKind],
    primary_key: &[(String, IdentityKind)],
) -> Vec<String> {
    let names: Vec<String> = primary_key.iter().map(|k| k.0.clone()).collect();
    let mut out = vec![mysql_fingerprint(columns, kinds, &names)];
    for key in primary_key {
        out.push(key.1.key_expression(&quote_ident(Driver::Mysql, &key.0)));
    }
    out
}
/// Row id for the values of [`mysql_identity_columns`]. `None` when the server returned no
/// fingerprint or key value, so the row can't be identified exactly.
pub fn mysql_row_id(
    kinds: &[IdentityKind],
    primary_key: &[(String, IdentityKind)],
    values: &[Option<String>],
) -> Option<Vec<String>> {
    let mut values = values.iter();
    let fingerprint = values.next()?.clone()?;
    let mut out = vec![
        MYSQL_ROW_ID.to_owned(),
        format!(
            "kinds={}",
            kinds.iter().map(|k| k.code()).collect::<String>()
        ),
        format!("fp={fingerprint}"),
        format!(
            "keys={}",
            primary_key.iter().map(|k| k.1.code()).collect::<String>()
        ),
    ];
    for j in 0..primary_key.len() {
        out.push(format!("k{j}={}", values.next()?.clone()?));
    }
    Some(out)
}
struct MysqlIdentity {
    kinds: Vec<IdentityKind>,
    fingerprint: String,
    /// Kinds of the primary-key columns, in key order. Empty for a table without a key.
    key_kinds: Vec<IdentityKind>,
    /// Exact values by primary-key position.
    keys: std::collections::BTreeMap<usize, String>,
}
const UNIDENTIFIED: &str = "This row couldn't be identified exactly when it was loaded. Nothing was changed; reload the table and try again.";
fn parse_mysql_row_id(rid: &[String], columns: usize) -> Result<MysqlIdentity> {
    let invalid = || anyhow::anyhow!(UNIDENTIFIED);
    if rid.first().map(String::as_str) != Some(MYSQL_ROW_ID) {
        return Err(invalid());
    }
    let codes = |v: &str| {
        v.chars()
            .map(IdentityKind::from_code)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)
    };
    let (mut kinds, mut key_kinds, mut fingerprint) = (None, None, None);
    let mut keys = std::collections::BTreeMap::new();
    for part in &rid[1..] {
        let (name, value) = part.split_once('=').ok_or_else(invalid)?;
        match name {
            "kinds" => kinds = Some(codes(value)?),
            "keys" => key_kinds = Some(codes(value)?),
            "fp" => fingerprint = Some(value.to_owned()),
            _ => {
                let j = name
                    .strip_prefix('k')
                    .and_then(|j| j.parse::<usize>().ok())
                    .ok_or_else(invalid)?;
                keys.insert(j, value.to_owned());
            }
        }
    }
    Ok(MysqlIdentity {
        kinds: kinds.filter(|k| k.len() == columns).ok_or_else(invalid)?,
        fingerprint: fingerprint.filter(|f| !f.is_empty()).ok_or_else(invalid)?,
        key_kinds: key_kinds.ok_or_else(invalid)?,
        keys,
    })
}
pub const BINARY_AMBIGUOUS: &str = "can't tell whether this binary value is the text shown or the bytes it spells in hex. Nothing was changed; reload the table and try again.";
fn row_where(d: Driver, m: &TableMeta, r: &EditRow, p: &mut Params) -> Result<(String, bool)> {
    let orig = r
        .orig
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cannot identify a new row"))?;
    if d == Driver::Mysql {
        if let Some(rid) = &r.rid {
            return mysql_row_where(m, &parse_mysql_row_id(rid, m.columns.len())?, p);
        }
    }
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
        if d == Driver::Mysql && binary(m, i) && hex(v) {
            // The grid shows bytes as hex only when they aren't readable text, so text that
            // decodes to readable bytes must be the stored text itself.
            match unhex(v) {
                Some(bytes) if !binary_shown_as_text(&bytes) => {
                    bail!("{c}: {BINARY_AMBIGUOUS}")
                }
                _ => {}
            }
        }
        Ok(if exact {
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
            if !m.primary_key.is_empty() {
                return Ok((parts.join(" AND "), true));
            }
            // A ctid is a physical slot that TRUNCATE or VACUUM can hand to another row, so
            // without a key the original values must also match, under the COUNT check too.
            if m.columns.is_empty() {
                bail!("Cannot identify the row: no columns");
            }
            for c in &m.columns {
                parts.push(cond(c, false, p)?);
            }
            return Ok((parts.join(" AND "), false));
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
/// Exact MySQL identity: the key columns by their stored values plus the whole-row fingerprint.
/// A row someone else changed since it was loaded no longer matches, so the write fails instead of
/// overwriting their change. Tables without a usable key match on the fingerprint alone.
fn mysql_row_where(m: &TableMeta, id: &MysqlIdentity, p: &mut Params) -> Result<(String, bool)> {
    let d = Driver::Mysql;
    // The identity was built for the key the table had when it was read.
    if id.key_kinds.len() != m.primary_key.len() {
        bail!(UNIDENTIFIED);
    }
    let mut parts = vec![];
    for (j, (c, kind)) in m.primary_key.iter().zip(&id.key_kinds).enumerate() {
        let exact = id
            .keys
            .get(&j)
            .ok_or_else(|| anyhow::anyhow!(UNIDENTIFIED))?;
        parts.push(kind.key_condition(&quote_ident(d, c), exact, p));
    }
    parts.push(format!(
        "{} = {}",
        mysql_fingerprint(&m.columns, &id.kinds, &m.primary_key),
        p.add(Some(&id.fingerprint))
    ));
    // Without a key the fingerprint alone identifies the row, under a COUNT check.
    Ok((parts.join(" AND "), !m.primary_key.is_empty()))
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
pub const ROW_MISSING: &str = "The row was not found as it was loaded: it was changed or deleted since (in another session, tab or app), or its stored values can't be matched exactly. Nothing was saved; reload the table and try again.";
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
/// How to tell, after the connection dropped during a save, whether one row change of
/// [`data_statements`] reached the database: each check holds when its COUNT equals `expect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowVerify {
    /// Holds only if the change was applied: the row has the intended values.
    pub applied: Option<Check>,
    /// Holds only if it was not: the row is still exactly as it was loaded (the same match as
    /// the save's own guard), or a new row's key is still free.
    pub not_applied: Option<Check>,
}
/// [`RowVerify`] for each statement of [`data_statements`], in the same order. MySQL only:
/// PostgreSQL asks the server how the transaction ended instead. A check is left out when it
/// can't be decided from the row (no primary key, an auto-increment insert, a deleted row
/// that is gone, which another session could also explain), so the outcome stays unknown
/// rather than guessed.
pub fn mysql_verify_checks(t: &TableRef, m: &TableMeta, rows: &[EditRow]) -> Vec<RowVerify> {
    let d = Driver::Mysql;
    let table = qualified(d, t);
    let count = |w: String, p: Params, expect: u64| Check {
        sql: format!("SELECT COUNT(*) FROM {table} WHERE {w}"),
        params: p.list,
        expect,
        message: String::new(),
    };
    let column = |c: &str| m.columns.iter().position(|v| v == c);
    // The primary key as loaded (exact identity) or, for a new or re-keyed row, as typed.
    let key_where = |r: &EditRow, p: &mut Params| -> Option<String> {
        if m.primary_key.is_empty() {
            return None;
        }
        let identity = r
            .rid
            .as_ref()
            .and_then(|rid| parse_mysql_row_id(rid, m.columns.len()).ok());
        let mut parts = vec![];
        for (j, c) in m.primary_key.iter().enumerate() {
            let i = column(c)?;
            let col = quote_ident(d, c);
            let typed = r.orig.is_none() || (!r.deleted && cell_modified(r, i));
            match (&identity, typed) {
                (Some(id), false) => parts.push(id.key_kinds.get(j)?.key_condition(
                    &col,
                    id.keys.get(&j)?,
                    p,
                )),
                (None, false) => {
                    let orig = r.orig.as_ref()?.get(i)?.clone();
                    parts.push(format!(
                        "{col} <=> {}",
                        value(d, m, i, &Cell::from(orig), p)
                    ));
                }
                _ => {
                    let cell = r.cur.get(i)?;
                    if cell.is_unset() {
                        return None;
                    }
                    parts.push(format!("{col} <=> {}", value(d, m, i, cell, p)));
                }
            }
        }
        Some(parts.join(" AND "))
    };
    let mut out = vec![];
    for r in rows {
        // The row exactly as loaded. With a row identity that is the save's own guard (key plus
        // the whole-row fingerprint); without one, the key plus every loaded value.
        let as_loaded = || {
            let mut p = Params::new(d);
            if r.rid.is_some() {
                return match row_where(d, m, r, &mut p) {
                    Ok((w, true)) => Some(count(w, p, 1)),
                    _ => None,
                };
            }
            let orig = r.orig.as_ref()?;
            let mut w = key_where(r, &mut p)?;
            for (i, c) in m.columns.iter().enumerate() {
                if m.primary_key.contains(c) {
                    continue;
                }
                let cell = Cell::from(orig.get(i)?.clone());
                w += &format!(
                    " AND {} <=> {}",
                    quote_ident(d, c),
                    value(d, m, i, &cell, &mut p)
                );
            }
            Some(count(w, p, 1))
        };
        let with_values = |only_modified: bool| {
            let mut p = Params::new(d);
            let mut w = key_where(r, &mut p)?;
            for (i, cell) in r.cur.iter().enumerate() {
                if cell.is_unset() || (only_modified && !cell_modified(r, i)) {
                    continue;
                }
                let col = quote_ident(d, m.columns.get(i)?);
                w += &format!(" AND {col} <=> {}", value(d, m, i, cell, &mut p));
            }
            Some(count(w, p, 1))
        };
        let key_free = || {
            let mut p = Params::new(d);
            key_where(r, &mut p).map(|w| count(w, p, 0))
        };
        let verify = if r.orig.is_none() {
            if r.deleted {
                continue;
            }
            RowVerify {
                applied: with_values(false),
                not_applied: key_free(),
            }
        } else if r.deleted {
            RowVerify {
                applied: None,
                not_applied: as_loaded(),
            }
        } else if is_modified(r) {
            RowVerify {
                applied: with_values(true),
                not_applied: as_loaded(),
            }
        } else {
            continue;
        };
        out.push(verify);
    }
    out
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
        let mut lossy: Vec<(String, String)> = vec![];
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
                let old_type = orig
                    .as_ref()
                    .and_then(|v| field(&c.columns, v, "data_type"));
                let new_type = field(&c.columns, &r.cur, "data_type").unwrap_or("");
                if old_type.map(normal_type) != Some(normal_type(new_type)) {
                    let col = quote_ident(d, on);
                    lossy.push((
                        format!(
                            "({col} IS NOT NULL AND ({}))",
                            mysql_value_changes(&col, old_type.unwrap_or(""), new_type)
                                .unwrap_or_else(|| "TRUE".into())
                        ),
                        format!("{on} to {new_type}"),
                    ));
                }
            }
            prev = name;
        }
        if !clauses.is_empty() {
            let mut st = Stmt::new(format!("ALTER TABLE {table}\n  {};", clauses.join(",\n  ")));
            // MySQL converts existing values silently (DECIMAL scale, DATETIME precision,
            // DOUBLE to FLOAT…), so a type change only runs when no stored value would change.
            if !lossy.is_empty() {
                st.check = Some(Check {
                    sql: format!(
                        "SELECT COUNT(*) FROM {table} WHERE {}",
                        lossy
                            .iter()
                            .map(|l| l.0.as_str())
                            .collect::<Vec<_>>()
                            .join(" OR ")
                    ),
                    params: vec![],
                    expect: 0,
                    message: format!(
                        "Changing {} would alter or truncate existing values, or Telecat can't verify that it keeps them. Nothing was changed.",
                        lossy
                            .iter()
                            .map(|l| l.1.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            out.push(st);
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
/// Case and spacing normalised, except inside quoted ENUM/SET members, where case matters.
fn normal_type(t: &str) -> String {
    let mut quoted = false;
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| {
            if c == '\'' {
                quoted = !quoted;
            }
            if quoted { c } else { c.to_ascii_lowercase() }
        })
        .collect()
}
#[derive(Debug, Clone, PartialEq)]
enum MysqlType {
    Int {
        bits: u32,
        unsigned: bool,
    },
    Decimal(u64, u64),
    Float,
    Double,
    /// Character types; `fixed` CHAR drops trailing spaces when read back.
    Text {
        chars: Option<u64>,
        bytes: Option<u64>,
        fixed: bool,
    },
    Bytes {
        len: Option<u64>,
        fixed: bool,
    },
    Date,
    DateTime(u64),
    Timestamp(u64),
    Time(u64),
    Year,
    Json,
    Enum(String),
    Other,
}
fn mysql_type(declared: &str) -> MysqlType {
    use MysqlType::*;
    let ty = normal_type(declared);
    let base: String = ty.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let args: Vec<u64> = ty
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(a, _)| a.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_default();
    let arg = |i: usize| args.get(i).copied();
    let unsigned = ty.contains("unsigned");
    match base.as_str() {
        "tinyint" | "bool" | "boolean" => Int { bits: 8, unsigned },
        "smallint" => Int { bits: 16, unsigned },
        "mediumint" => Int { bits: 24, unsigned },
        "int" | "integer" => Int { bits: 32, unsigned },
        "bigint" => Int { bits: 64, unsigned },
        "decimal" | "numeric" | "dec" | "fixed" => {
            Decimal(arg(0).unwrap_or(10), arg(1).unwrap_or(0))
        }
        "float" if arg(0).is_none_or(|p| p <= 24) => Float,
        "float" | "double" | "real" => Double,
        "char" => Text {
            chars: Some(arg(0).unwrap_or(1)),
            bytes: None,
            fixed: true,
        },
        "varchar" => Text {
            chars: arg(0),
            bytes: None,
            fixed: false,
        },
        "tinytext" => Text {
            chars: None,
            bytes: Some(255),
            fixed: false,
        },
        "text" => Text {
            chars: None,
            bytes: Some(65_535),
            fixed: false,
        },
        "mediumtext" => Text {
            chars: None,
            bytes: Some(16_777_215),
            fixed: false,
        },
        "longtext" => Text {
            chars: None,
            bytes: Some(4_294_967_295),
            fixed: false,
        },
        "binary" => Bytes {
            len: Some(arg(0).unwrap_or(1)),
            fixed: true,
        },
        "varbinary" => Bytes {
            len: arg(0),
            fixed: false,
        },
        "tinyblob" => Bytes {
            len: Some(255),
            fixed: false,
        },
        "blob" => Bytes {
            len: Some(65_535),
            fixed: false,
        },
        "mediumblob" => Bytes {
            len: Some(16_777_215),
            fixed: false,
        },
        "longblob" => Bytes {
            len: Some(4_294_967_295),
            fixed: false,
        },
        "date" => Date,
        "datetime" => DateTime(arg(0).unwrap_or(0)),
        "timestamp" => Timestamp(arg(0).unwrap_or(0)),
        "time" => Time(arg(0).unwrap_or(0)),
        "year" => Year,
        "json" => Json,
        "enum" => declared
            .split_once('(')
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map_or(Other, |(list, _)| Enum(list.to_owned())),
        _ => Other,
    }
}
/// A condition that is true for a stored value MySQL would change when converting the column
/// from `old` to `new`: the value is converted the way the ALTER would and compared back with the
/// original, exactly (numbers through DECIMAL(65,30) and DOUBLE, times at microsecond precision,
/// everything else as binary text). `None` when that can't be expressed, which refuses the change
/// for any non-NULL value.
fn mysql_value_changes(col: &str, old: &str, new: &str) -> Option<String> {
    use MysqlType::*;
    let (from, to) = (mysql_type(old), mysql_type(new));
    let numeric = |t: &MysqlType| matches!(t, Int { .. } | Decimal(..) | Float | Double | Year);
    let temporal = |t: &MysqlType| matches!(t, Date | DateTime(_) | Timestamp(_) | Time(_));
    if from == Other {
        return None;
    }
    let mut extra = vec![];
    let converted = match &to {
        Int { bits, unsigned } => {
            let (lo, hi) = if *unsigned {
                ("0".to_owned(), (u128::pow(2, *bits) - 1).to_string())
            } else {
                (
                    format!("-{}", u128::pow(2, bits - 1)),
                    (u128::pow(2, bits - 1) - 1).to_string(),
                )
            };
            extra.push(format!("NOT ({col} + 0e0 BETWEEN {lo} AND {hi})"));
            format!(
                "CAST({col} AS {})",
                if *unsigned { "UNSIGNED" } else { "SIGNED" }
            )
        }
        Decimal(p, s) => format!("CAST({col} AS DECIMAL({p}, {s}))"),
        Float => format!("CAST({col} AS FLOAT)"),
        Double => format!("CAST({col} AS DOUBLE)"),
        Year => {
            extra.push(format!("NOT ({col} + 0 BETWEEN 1901 AND 2155)"));
            format!("CAST({col} AS YEAR)")
        }
        Text {
            chars,
            bytes,
            fixed,
        } => {
            if let Some(max) = bytes {
                extra.push(format!("LENGTH({col}) > {max}"));
            }
            let cast = match chars {
                Some(n) => format!("CAST({col} AS CHAR({n}))"),
                None => format!("CAST({col} AS CHAR)"),
            };
            if *fixed {
                format!("TRIM(TRAILING ' ' FROM {cast})")
            } else {
                cast
            }
        }
        Bytes { len, fixed } => match (len, fixed) {
            (Some(n), true) => format!("CAST({col} AS BINARY({n}))"),
            (Some(n), false) => {
                extra.push(format!("LENGTH({col}) > {n}"));
                format!("CAST({col} AS BINARY)")
            }
            (None, _) => format!("CAST({col} AS BINARY)"),
        },
        Date => format!("CAST({col} AS DATE)"),
        DateTime(n) => format!("CAST({col} AS DATETIME({n}))"),
        Timestamp(n) => {
            extra.push(format!(
                "{col} < '1970-01-01 00:00:01' OR {col} > '2038-01-19 03:14:07'"
            ));
            format!("CAST({col} AS DATETIME({n}))")
        }
        Time(n) => format!("CAST({col} AS TIME({n}))"),
        // JSON normalises spacing and key order; the meaning, not the text, is what's kept.
        Json => return Some(format!("NOT JSON_VALID({col})")),
        Enum(list) => return Some(format!("NOT (BINARY {col} IN ({list}))")),
        Other => return None,
    };
    let same = if numeric(&from) && (numeric(&to) || matches!(to, Text { .. })) {
        format!(
            "CAST({converted} AS DECIMAL(65, 30)) <=> CAST({col} AS DECIMAL(65, 30)) AND ({converted}) + 0e0 <=> {col} + 0e0"
        )
    } else if temporal(&from) && temporal(&to) {
        format!("CAST({converted} AS DATETIME(6)) <=> CAST({col} AS DATETIME(6))")
    } else {
        format!("BINARY CAST({converted} AS CHAR) <=> BINARY CAST({col} AS CHAR)")
    };
    extra.insert(0, format!("NOT ({same})"));
    Some(extra.join(" OR "))
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
/// The SQL the user approves, logs and error dialogs show. Placeholders are only replaced where
/// the server sees them, never inside quoted identifiers, string literals or comments.
pub fn inline_params(d: Driver, st: &Stmt) -> String {
    let placeholder = re(if d == Driver::Mysql {
        r"\?"
    } else {
        r"\$(\d+)"
    });
    let mut i = 0;
    let mut out = String::with_capacity(st.sql.len());
    let mut done = 0;
    for code in crate::guard::code_ranges(&st.sql, d) {
        out.push_str(&st.sql[done..code.start]);
        out.push_str(
            &placeholder.replace_all(&st.sql[code.clone()], |m: &regex::Captures| {
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
            }),
        );
        done = code.end;
    }
    out.push_str(&st.sql[done..]);
    out
}
