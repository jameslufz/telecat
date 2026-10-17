use crate::{changes::quote_ident, sql::trim, types::*};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Where {
    pub sql: String,
    pub params: Vec<serde_json::Value>,
}
fn as_text(d: Driver, c: &str) -> String {
    if d == Driver::Mysql {
        c.into()
    } else {
        format!("{c}::text")
    }
}
fn escape_like(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if matches!(c, '\\' | '%' | '_') {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
pub fn build_where(filters: &[Filter], columns: &[String], d: Driver, combine: Combine) -> Where {
    let mut params = vec![];
    let mut parts = vec![];
    let mut p = |s: String| {
        params.push(serde_json::Value::String(s));
        if d == Driver::Mysql {
            "?".into()
        } else {
            format!("${}", params.len())
        }
    };
    for f in filters {
        if !f.enabled {
            continue;
        }
        if f.column == RAW_SQL {
            if !trim(&f.value).is_empty() {
                parts.push(format!("({})", f.value));
            }
            continue;
        }
        if !matches!(f.op, FilterOp::IsNull | FilterOp::IsNotNull) && f.value.is_empty() {
            continue;
        }
        if !f.column.is_empty() && !columns.contains(&f.column) {
            continue;
        }
        let cols = if f.column.is_empty() {
            columns.to_vec()
        } else {
            vec![f.column.clone()]
        };
        let mut terms = vec![];
        for name in &cols {
            let col = quote_ident(d, name);
            let cmp = if f.column.is_empty() {
                as_text(d, &col)
            } else {
                col.clone()
            };
            let term = match f.op {
                FilterOp::IsNull => format!("{col} IS NULL"),
                FilterOp::IsNotNull => format!("{col} IS NOT NULL"),
                FilterOp::Like => format!("{} LIKE {}", as_text(d, &col), p(f.value.clone())),
                FilterOp::Contains
                | FilterOp::NotContains
                | FilterOp::ContainsCs
                | FilterOp::NotContainsCs
                | FilterOp::Prefix
                | FilterOp::Suffix => {
                    let pat = escape_like(&f.value);
                    let pat = match f.op {
                        FilterOp::Prefix => format!("{pat}%"),
                        FilterOp::Suffix => format!("%{pat}"),
                        _ => format!("%{pat}%"),
                    };
                    let cs = matches!(f.op, FilterOp::ContainsCs | FilterOp::NotContainsCs);
                    let neg = if matches!(f.op, FilterOp::NotContains | FilterOp::NotContainsCs) {
                        "NOT "
                    } else {
                        ""
                    };
                    if d == Driver::Mysql {
                        format!(
                            "{} {neg}LIKE {}",
                            if cs {
                                format!("CAST({col} AS BINARY)")
                            } else {
                                col
                            },
                            p(pat)
                        )
                    } else {
                        format!(
                            "{col}::text {neg}{} {}",
                            if cs { "LIKE" } else { "ILIKE" },
                            p(pat)
                        )
                    }
                }
                FilterOp::Between | FilterOp::NotBetween => format!(
                    "{cmp} {} {} AND {}",
                    if f.op == FilterOp::Between {
                        "BETWEEN"
                    } else {
                        "NOT BETWEEN"
                    },
                    p(f.value.clone()),
                    p(f.value2.clone().unwrap_or_default())
                ),
                FilterOp::In | FilterOp::NotIn => {
                    let vals = f
                        .value
                        .split(',')
                        .map(trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>();
                    if vals.is_empty() {
                        "1=1".into()
                    } else {
                        format!(
                            "{} {} ({})",
                            as_text(d, &col),
                            if f.op == FilterOp::In { "IN" } else { "NOT IN" },
                            vals.iter()
                                .map(|v| p((*v).into()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                }
                _ => format!("{cmp} {} {}", f.op.as_str(), p(f.value.clone())),
            };
            terms.push(term);
        }
        parts.push(if terms.len() == 1 {
            terms.remove(0)
        } else {
            format!("({})", terms.join(" OR "))
        });
    }
    Where {
        sql: if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(&format!(" {} ", combine.as_str())))
        },
        params,
    }
}
