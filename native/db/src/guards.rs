use crate::wire::Wire;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use telecat_core::{
    changes::{Stmt, qualified},
    sql::{first_keyword, strip_comments},
    types::*,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineWarning {
    pub table: String,
    pub engine: String,
}
#[derive(Debug, Clone, Default)]
pub struct CommitReview {
    pub non_transactional: Vec<EngineWarning>,
    pub(crate) targets: Vec<String>,
}
impl CommitReview {
    pub fn message(&self) -> String {
        format!(
            "{} cannot roll back changes. Telecat will check the exact number of matching rows before writing, but this engine cannot undo a wrong affected-row count or a later failure. Continue?",
            self.non_transactional
                .iter()
                .map(|v| format!("{} ({})", v.table, v.engine))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
struct Target {
    table: TableRef,
    tail: String,
    keyword: String,
}
fn target(database: &str, sql: &str) -> Result<Option<Target>> {
    let keyword = first_keyword(sql, Driver::Mysql);
    if ["SELECT", "SHOW", "EXPLAIN", "DESC", "DESCRIBE"].contains(&keyword.as_str()) {
        return Ok(None);
    }
    static TARGET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?is)^\s*(?:UPDATE\s+|DELETE\s+FROM\s+|INSERT\s+INTO\s+|REPLACE\s+INTO\s+)(`(?:``|[^`])+`|[A-Za-z0-9_$]+)(?:\s*\.\s*(`(?:``|[^`])+`|[A-Za-z0-9_$]+))?\s*(.*)$").unwrap()
    });
    let clean = strip_comments(sql, Driver::Mysql);
    let m = TARGET
        .captures(&clean)
        .context("Cannot prove the guarded MySQL write target")?;
    let ident = |s: &str| {
        if s.starts_with('`') {
            s[1..s.len() - 1].replace("``", "`")
        } else {
            s.into()
        }
    };
    let table = if let Some(t) = m.get(2) {
        TableRef {
            schema: ident(&m[1]),
            name: ident(t.as_str()),
        }
    } else {
        TableRef {
            schema: database.into(),
            name: ident(&m[1]),
        }
    };
    let tail = m[3].trim().to_owned();
    let words = words(&tail);
    if (keyword == "UPDATE" && words.first().map(|w| w.2.as_str()) != Some("SET"))
        || (keyword == "DELETE"
            && words
                .first()
                .is_some_and(|w| !["WHERE", "ORDER", "LIMIT"].contains(&w.2.as_str())))
    {
        bail!("Multiple-table guarded MySQL writes are not supported");
    }
    Ok(Some(Target {
        table,
        tail,
        keyword,
    }))
}
/// Positions of top-level words and placeholders. Quoted content and subexpressions are skipped.
fn words(s: &str) -> Vec<(usize, usize, String)> {
    let b = s.as_bytes();
    let (mut i, mut depth) = (0, 0usize);
    let mut out = vec![];
    while i < b.len() {
        let c = b[i];
        if [b'\'', b'"', b'`'].contains(&c) {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == c {
                    i += 1;
                    if i < b.len() && b[i] == c {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'(' {
            depth += 1;
            i += 1;
        } else if c == b')' {
            depth = depth.saturating_sub(1);
            i += 1;
        } else if c.is_ascii_alphabetic() {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if depth == 0 {
                out.push((start, i, s[start..i].to_ascii_uppercase()));
            }
        } else {
            i += 1;
        }
    }
    out
}
fn placeholders(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        let c = b[i];
        if [b'\'', b'"', b'`'].contains(&c) {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == c {
                    i += 1;
                    if i < b.len() && b[i] == c {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else {
            if c == b'?' {
                n += 1;
            }
            i += 1;
        }
    }
    n
}
pub(crate) async fn review(
    conn: &mut Wire,
    d: Driver,
    database: &str,
    stmts: &[Stmt],
) -> Result<CommitReview> {
    let mut result = CommitReview::default();
    if d != Driver::Mysql {
        return Ok(result);
    }
    for st in stmts {
        // TableTab commits MySQL DDL as separate, non-transactional parts. Engine
        // consent applies to row writes, not CREATE/ALTER/DROP. Guarded DDL still
        // reaches the implicit-commit rejection in execute_after_review.
        if st.expect.is_none()
            && st.check.is_none()
            && ["CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME"]
                .contains(&first_keyword(&st.sql, d).as_str())
        {
            continue;
        }
        if let Some(t) = target(database, &st.sql)? {
            let name = qualified(d, &t.table);
            if result.targets.contains(&name) {
                continue;
            }
            let rows=conn.run("SELECT engine FROM information_schema.tables WHERE table_schema=? AND table_name=?",&[Value::String(t.table.schema),Value::String(t.table.name)]).await?.0.rows;
            let engine = rows
                .first()
                .and_then(|r| r.first())
                .and_then(|v| v.clone())
                .context("Write target is not a base table")?;
            if !matches!(engine.as_str(), "InnoDB" | "NDBCLUSTER" | "NDB") {
                result.non_transactional.push(EngineWarning {
                    table: name.clone(),
                    engine,
                });
            }
            result.targets.push(name);
        }
    }
    result.targets.sort();
    Ok(result)
}
pub(crate) async fn locked_count(conn: &mut Wire, sql: &str, params: &[Value]) -> Result<u64> {
    static COUNT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?is)^\s*SELECT\s+COUNT\s*\(\s*\*\s*\)\s+(?:AS\s+[A-Za-z_][A-Za-z0-9_]*\s+)?(FROM\s+.+?)\s*;?\s*$").unwrap()
    });
    if let Some(m) = COUNT.captures(sql) {
        let from = m[1].trim_end_matches(';');
        if words(from).iter().any(|w| {
            [
                "GROUP",
                "HAVING",
                "LIMIT",
                "OFFSET",
                "UNION",
                "INTERSECT",
                "EXCEPT",
                "FOR",
                "LOCK",
            ]
            .contains(&w.2.as_str())
        }) {
            bail!("Cannot prove exact row locks for this guard count shape");
        }
        let result = conn
            .run(&format!("SELECT 1 {from} FOR UPDATE"), params)
            .await?
            .0;
        return Ok(result.rows.len() as u64);
    }
    // Literal checks have no rows to lock. Refuse unrecognized aggregate shapes.
    if words(sql).iter().any(|w| w.2 == "FROM") {
        bail!("Guard count must be SELECT COUNT(*) FROM … so matching rows can be locked");
    }
    conn.run(sql, params)
        .await?
        .0
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.as_ref())
        .and_then(|v| v.parse().ok())
        .context("Guard did not return a count")
}
pub(crate) async fn precheck_write(conn: &mut Wire, database: &str, st: &Stmt) -> Result<()> {
    let Some(expect) = st.expect else {
        return Ok(());
    };
    let Some(t) = target(database, &st.sql)? else {
        return Ok(());
    };
    let n = if t.keyword == "INSERT" {
        let list = words(&t.tail);
        let v = list
            .iter()
            .find(|w| w.2 == "VALUES")
            .context("Cannot precheck this non-transactional INSERT")?;
        let after = t.tail[v.1..].trim().trim_end_matches(';').trim();
        // Grid inserts contain one VALUES tuple; reject INSERT SELECT/ON DUPLICATE etc.
        if !after.starts_with('(') || !after.ends_with(')') || !words(after).is_empty() {
            bail!("Cannot precheck this non-transactional INSERT");
        }
        let mut depth = 0;
        let mut count = 0;
        let mut quote = None;
        let mut chars = after.chars().peekable();
        while let Some(c) = chars.next() {
            if let Some(q) = quote {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    if chars.peek() == Some(&q) {
                        chars.next();
                    } else {
                        quote = None;
                    }
                }
            } else if ['\'', '"', '`'].contains(&c) {
                quote = Some(c);
            } else if c == '(' {
                if depth == 0 {
                    count += 1;
                }
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
        }
        count
    } else if ["UPDATE", "DELETE"].contains(&t.keyword.as_str()) {
        let tokens = words(&t.tail);
        let start = tokens.iter().find(|w| w.2 == "WHERE").map(|w| w.0);
        let end = tokens
            .iter()
            .find(|w| w.0 > start.unwrap_or(0) && ["ORDER", "LIMIT"].contains(&w.2.as_str()))
            .map(|w| w.0)
            .unwrap_or(t.tail.len());
        let (predicate, offset) = if let Some(start) = start {
            (&t.tail[start..end], placeholders(&t.tail[..start]))
        } else {
            ("", st.params.len())
        };
        conn.run(
            &format!(
                "SELECT 1 FROM {} {} FOR UPDATE",
                qualified(Driver::Mysql, &t.table),
                predicate.trim().trim_end_matches(';')
            ),
            st.params
                .get(offset..)
                .context("Missing predicate parameters")?,
        )
        .await?
        .0
        .rows
        .len() as u64
    } else {
        bail!("Cannot prove the affected count of this non-transactional write");
    };
    if n != expect {
        bail!(
            "Expected to change {expect} row but {n} match. Nothing was written by this statement."
        );
    }
    Ok(())
}
