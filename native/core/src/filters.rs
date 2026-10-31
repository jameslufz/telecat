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
/// How a pattern operator compares letter case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Case {
    /// As the column's collation does (MySQL `LIKE`; PostgreSQL `LIKE` is case-sensitive).
    Server,
    Sensitive,
    Insensitive,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Contains,
    Prefix,
    Suffix,
}
/// The LIKE pattern a Contains / Has prefix / Has suffix operator builds: its shape, whether it
/// is negated and how it treats case. TablePlus's PostgreSQL "Contains" is a case-sensitive
/// `LIKE`, with "Case insensitive" variants using `ILIKE`; MySQL's "Contains" follows the
/// collation, with "Case sensitive" variants comparing bytes.
fn pattern(op: FilterOp) -> Option<(Shape, bool, Case)> {
    use FilterOp::*;
    Some(match op {
        Contains => (Shape::Contains, false, Case::Server),
        NotContains => (Shape::Contains, true, Case::Server),
        ContainsCs => (Shape::Contains, false, Case::Sensitive),
        NotContainsCs => (Shape::Contains, true, Case::Sensitive),
        ContainsCi => (Shape::Contains, false, Case::Insensitive),
        NotContainsCi => (Shape::Contains, true, Case::Insensitive),
        Prefix => (Shape::Prefix, false, Case::Server),
        Suffix => (Shape::Suffix, false, Case::Server),
        PrefixCs => (Shape::Prefix, false, Case::Sensitive),
        SuffixCs => (Shape::Suffix, false, Case::Sensitive),
        PrefixCi => (Shape::Prefix, false, Case::Insensitive),
        SuffixCi => (Shape::Suffix, false, Case::Insensitive),
        _ => return None,
    })
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
                FilterOp::Ilike if d == Driver::Mysql => {
                    format!("LOWER({col}) LIKE LOWER({})", p(f.value.clone()))
                }
                FilterOp::Ilike => format!("{col}::text ILIKE {}", p(f.value.clone())),
                op if pattern(op).is_some() => {
                    let (shape, negated, case) = pattern(op).expect("checked");
                    let pat = escape_like(&f.value);
                    let pat = match shape {
                        Shape::Prefix => format!("{pat}%"),
                        Shape::Suffix => format!("%{pat}"),
                        Shape::Contains => format!("%{pat}%"),
                    };
                    let neg = if negated { "NOT " } else { "" };
                    match (d, case) {
                        (Driver::Mysql, Case::Sensitive) => {
                            format!("CAST({col} AS BINARY) {neg}LIKE {}", p(pat))
                        }
                        (Driver::Mysql, Case::Insensitive) => {
                            format!("LOWER({col}) {neg}LIKE LOWER({})", p(pat))
                        }
                        (Driver::Mysql, Case::Server) => format!("{col} {neg}LIKE {}", p(pat)),
                        (Driver::Postgres, Case::Insensitive) => {
                            format!("{col}::text {neg}ILIKE {}", p(pat))
                        }
                        (Driver::Postgres, _) => format!("{col}::text {neg}LIKE {}", p(pat)),
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

/// The filter bar's operator menu for `d`, in TablePlus's order and wording; `None` is a
/// separator.
pub fn filter_ops(d: Driver) -> Vec<Option<(FilterOp, &'static str)>> {
    use FilterOp::*;
    let mut ops = vec![
        Some((Eq, "=")),
        Some((Ne, "<>")),
        Some((Lt, "<")),
        Some((Gt, ">")),
        Some((Le, "<=")),
        Some((Ge, ">=")),
        None,
        Some((In, "IN")),
        Some((NotIn, "NOT IN")),
        None,
        Some((IsNull, "IS NULL")),
        Some((IsNotNull, "IS NOT NULL")),
        None,
        Some((Between, "BETWEEN")),
        Some((NotBetween, "NOT BETWEEN")),
        None,
        Some((Like, "LIKE")),
    ];
    if d == Driver::Postgres {
        ops.push(Some((Ilike, "ILIKE")));
    }
    ops.extend([
        None,
        Some((Contains, "Contains")),
        Some((NotContains, "Not contains")),
        None,
    ]);
    if d == Driver::Mysql {
        ops.extend([
            Some((ContainsCs, "Contains - Case sensitive")),
            Some((NotContainsCs, "Not contains - Case sensitive")),
            None,
            Some((Prefix, "Has prefix")),
            Some((Suffix, "Has suffix")),
            None,
            Some((PrefixCs, "Has prefix - Case sensitive")),
            Some((SuffixCs, "Has suffix - Case sensitive")),
        ]);
    } else {
        ops.extend([
            Some((ContainsCi, "Contains - Case insensitive")),
            Some((NotContainsCi, "Not contains - Case insensitive")),
            None,
            Some((Prefix, "Has prefix")),
            Some((Suffix, "Has suffix")),
            None,
            Some((PrefixCi, "Has prefix - Case insensitive")),
            Some((SuffixCi, "Has suffix - Case insensitive")),
        ]);
    }
    ops
}
/// An operator's label in `d`'s menu. An operator from the other driver's menu (a filter saved
/// before) keeps a readable name.
pub fn op_label(d: Driver, op: FilterOp) -> &'static str {
    if let Some((_, label)) = filter_ops(d).into_iter().flatten().find(|(o, _)| *o == op) {
        return label;
    }
    use FilterOp::*;
    match op {
        ContainsCs => "Contains - Case sensitive",
        NotContainsCs => "Not contains - Case sensitive",
        PrefixCs => "Has prefix - Case sensitive",
        SuffixCs => "Has suffix - Case sensitive",
        ContainsCi => "Contains - Case insensitive",
        NotContainsCi => "Not contains - Case insensitive",
        PrefixCi => "Has prefix - Case insensitive",
        SuffixCi => "Has suffix - Case insensitive",
        Ilike => "ILIKE",
        _ => "=",
    }
}
/// The placeholder of a rule's value field.
pub fn value_placeholder(op: FilterOp) -> &'static str {
    if pattern(op).is_some() || matches!(op, FilterOp::Like | FilterOp::Ilike) {
        "Pattern"
    } else {
        "EMPTY"
    }
}
/// One item of the cell menu's Quick Filter submenu.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickFilter {
    pub label: String,
    pub filter: Filter,
    /// A comparison with NULL matches nothing, so only IS NULL / IS NOT NULL apply to one.
    pub enabled: bool,
}
const QUICK_LABEL_CHARS: usize = 40;
/// The cell menu's Quick Filter items for `column` and the clicked `value`, in TablePlus's
/// order (`name = 'bob'` … `name IS NOT NULL`); `None` is a separator.
pub fn quick_filters(column: &str, value: Option<&str>) -> Vec<Option<QuickFilter>> {
    use FilterOp::*;
    let shown = value.map(|v| {
        let one_line: String = v
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        match one_line.char_indices().nth(QUICK_LABEL_CHARS) {
            Some((end, _)) => format!("{}…", &one_line[..end]),
            None => one_line,
        }
    });
    let item = |op: FilterOp, text: String| {
        Some(QuickFilter {
            label: format!("{column} {text}"),
            filter: Filter {
                enabled: true,
                column: column.to_owned(),
                op,
                value: if matches!(op, IsNull | IsNotNull) {
                    String::new()
                } else {
                    value.unwrap_or_default().to_owned()
                },
                value2: None,
            },
            enabled: match op {
                IsNull | IsNotNull => true,
                // IN splits its value at commas and trims each part: a value with a comma
                // or surrounding spaces would select other rows than this one.
                In | NotIn => value.is_some_and(|v| !v.contains(',') && trim(v) == v),
                _ => value.is_some(),
            },
        })
    };
    let quoted = format!("'{}'", shown.as_deref().unwrap_or("NULL"));
    let listed = format!("({})", shown.as_deref().unwrap_or("NULL"));
    vec![
        item(Eq, format!("= {quoted}")),
        item(Ne, format!("<> {quoted}")),
        item(Lt, format!("< {quoted}")),
        item(Gt, format!("> {quoted}")),
        item(Le, format!("<= {quoted}")),
        item(Ge, format!(">= {quoted}")),
        None,
        item(Contains, format!("Contains {quoted}")),
        item(NotContains, format!("Not contains {quoted}")),
        None,
        item(Prefix, format!("Has prefix {quoted}")),
        item(Suffix, format!("Has suffix {quoted}")),
        None,
        item(In, format!("IN {listed}")),
        item(NotIn, format!("NOT IN {listed}")),
        None,
        item(IsNull, "IS NULL".into()),
        item(IsNotNull, "IS NOT NULL".into()),
    ]
}
/// Whether the filter bar's rule is what the table shows now: its Apply button then reads
/// "Applied".
pub fn rule_applied(rule: &Filter, applied: &[Filter]) -> bool {
    let same = |a: &Filter| {
        a.column == rule.column
            && a.op == rule.op
            && a.value == rule.value
            && (!matches!(rule.op, FilterOp::Between | FilterOp::NotBetween)
                || a.value2.as_deref().unwrap_or("") == rule.value2.as_deref().unwrap_or(""))
    };
    applied.iter().any(same)
}
/// A foreign key from one table's columns to another's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub target: TableRef,
    pub target_columns: Vec<String>,
}
/// The filters that select the row a foreign key points at, from the referencing row's values
/// (in the key's column order): one `=` per key column, values bound as parameters. `None`
/// when any key column is NULL, because such a row references nothing.
pub fn foreign_key_filters(fk: &ForeignKey, values: &[Option<String>]) -> Option<Vec<Filter>> {
    if fk.columns.is_empty()
        || values.len() != fk.columns.len()
        || fk.target_columns.len() != fk.columns.len()
    {
        return None;
    }
    fk.target_columns
        .iter()
        .zip(values)
        .map(|(column, value)| {
            value.as_ref().map(|v| Filter {
                enabled: true,
                column: column.clone(),
                op: FilterOp::Eq,
                value: v.clone(),
                value2: None,
            })
        })
        .collect()
}
/// ORDER BY for a table page: the chosen sort, else the primary key ascending, else none.
/// Sort columns the table doesn't have are left out.
pub fn order_clause(d: Driver, sort: &[OrderBy], columns: &[String], pk: &[String]) -> String {
    let chosen: Vec<String> = sort
        .iter()
        .filter(|o| columns.contains(&o.column))
        .map(|o| format!("{} {}", quote_ident(d, &o.column), o.dir.as_str()))
        .collect();
    if !chosen.is_empty() {
        format!(" ORDER BY {}", chosen.join(", "))
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
    }
}
/// Page settings typed into the paging popover, checked as TablePlus does. `total` is the
/// exact row count when it is known (an estimate can't bound the offset).
pub fn page_settings(limit: &str, offset: &str, total: Option<u64>) -> Result<(u64, u64), String> {
    let offset = offset.trim();
    let offset = if offset.is_empty() {
        0
    } else {
        offset
            .parse::<u64>()
            .map_err(|_| "offset should be positive".to_owned())?
    };
    let limit = limit
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|l| *l > 0)
        .ok_or_else(|| "limitation should be greater than zero".to_owned())?;
    if limit > MAX_PAGE_ROWS {
        return Err(format!(
            "limitation should be at most {MAX_PAGE_ROWS} (every row of a page is held in memory)"
        ));
    }
    if let Some(total) = total.filter(|t| *t > 0)
        && offset >= total
    {
        return Err("offset should be smaller than the total".into());
    }
    Ok((limit, offset))
}
/// Rows per table page unless the user picks another size.
pub const DEFAULT_PAGE_ROWS: u64 = 300;
/// The largest page the paging popover accepts.
pub const MAX_PAGE_ROWS: u64 = 100_000;

#[cfg(test)]
mod tests {
    use super::*;
    fn rule(column: &str, op: FilterOp, value: &str) -> Filter {
        Filter {
            enabled: true,
            column: column.into(),
            op,
            value: value.into(),
            value2: None,
        }
    }
    fn sql(d: Driver, op: FilterOp, value: &str) -> (String, Vec<serde_json::Value>) {
        let w = build_where(&[rule("n", op, value)], &["n".into()], d, Combine::And);
        (w.sql, w.params)
    }
    #[test]
    fn pattern_operators_follow_tableplus_per_driver() {
        use FilterOp::*;
        let pg = |op| sql(Driver::Postgres, op, "o_b").0;
        assert_eq!(pg(Contains), " WHERE \"n\"::text LIKE $1");
        assert_eq!(pg(NotContains), " WHERE \"n\"::text NOT LIKE $1");
        assert_eq!(pg(ContainsCi), " WHERE \"n\"::text ILIKE $1");
        assert_eq!(pg(NotContainsCi), " WHERE \"n\"::text NOT ILIKE $1");
        assert_eq!(pg(PrefixCi), " WHERE \"n\"::text ILIKE $1");
        assert_eq!(pg(Ilike), " WHERE \"n\"::text ILIKE $1");
        assert_eq!(
            sql(Driver::Postgres, Ilike, "A%").1,
            [serde_json::json!("A%")]
        );
        assert_eq!(
            sql(Driver::Postgres, SuffixCi, "o_b").1,
            [serde_json::json!("%o\\_b")]
        );
        let my = |op| sql(Driver::Mysql, op, "o_b").0;
        assert_eq!(my(Contains), " WHERE `n` LIKE ?");
        assert_eq!(my(ContainsCs), " WHERE CAST(`n` AS BINARY) LIKE ?");
        assert_eq!(my(NotContainsCs), " WHERE CAST(`n` AS BINARY) NOT LIKE ?");
        assert_eq!(my(PrefixCs), " WHERE CAST(`n` AS BINARY) LIKE ?");
        assert_eq!(
            sql(Driver::Mysql, PrefixCs, "o_b").1,
            [serde_json::json!("o\\_b%")]
        );
        assert_eq!(my(ContainsCi), " WHERE LOWER(`n`) LIKE LOWER(?)");
        assert_eq!(my(Ilike), " WHERE LOWER(`n`) LIKE LOWER(?)");
    }
    #[test]
    fn operator_menus_match_tableplus() {
        let labels = |d| {
            filter_ops(d)
                .into_iter()
                .map(|o| o.map_or("——", |(_, l)| l))
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert_eq!(
            labels(Driver::Postgres),
            "=, <>, <, >, <=, >=, ——, IN, NOT IN, ——, IS NULL, IS NOT NULL, ——, BETWEEN, NOT BETWEEN, ——, LIKE, ILIKE, ——, Contains, Not contains, ——, Contains - Case insensitive, Not contains - Case insensitive, ——, Has prefix, Has suffix, ——, Has prefix - Case insensitive, Has suffix - Case insensitive"
        );
        assert_eq!(
            labels(Driver::Mysql),
            "=, <>, <, >, <=, >=, ——, IN, NOT IN, ——, IS NULL, IS NOT NULL, ——, BETWEEN, NOT BETWEEN, ——, LIKE, ——, Contains, Not contains, ——, Contains - Case sensitive, Not contains - Case sensitive, ——, Has prefix, Has suffix, ——, Has prefix - Case sensitive, Has suffix - Case sensitive"
        );
        assert_eq!(
            op_label(Driver::Mysql, FilterOp::ContainsCi),
            "Contains - Case insensitive"
        );
        assert_eq!(op_label(Driver::Postgres, FilterOp::Ilike), "ILIKE");
        assert_eq!(value_placeholder(FilterOp::Contains), "Pattern");
        assert_eq!(value_placeholder(FilterOp::Eq), "EMPTY");
        // Every operator survives a save and load.
        for op in filter_ops(Driver::Postgres)
            .into_iter()
            .chain(filter_ops(Driver::Mysql))
            .flatten()
        {
            let text = serde_json::to_string(&op.0).unwrap();
            assert_eq!(serde_json::from_str::<FilterOp>(&text).unwrap(), op.0);
        }
    }
    #[test]
    fn quick_filters_use_the_clicked_value() {
        let items = quick_filters("name", Some("bob"));
        let labels: Vec<String> = items
            .iter()
            .map(|i| i.as_ref().map_or("——".into(), |i| i.label.clone()))
            .collect();
        assert_eq!(
            labels.join(" | "),
            "name = 'bob' | name <> 'bob' | name < 'bob' | name > 'bob' | name <= 'bob' | name >= 'bob' | —— | name Contains 'bob' | name Not contains 'bob' | —— | name Has prefix 'bob' | name Has suffix 'bob' | —— | name IN (bob) | name NOT IN (bob) | —— | name IS NULL | name IS NOT NULL"
        );
        assert!(items.iter().flatten().all(|i| i.enabled));
        let contains = items[7].as_ref().unwrap();
        assert_eq!(
            (contains.filter.op, contains.filter.value.as_str()),
            (FilterOp::Contains, "bob")
        );
        let is_null = items.last().unwrap().as_ref().unwrap();
        assert!(is_null.filter.value.is_empty());
        // NULL: only IS NULL / IS NOT NULL make sense.
        let null = quick_filters("name", None);
        let enabled: Vec<FilterOp> = null
            .iter()
            .flatten()
            .filter(|i| i.enabled)
            .map(|i| i.filter.op)
            .collect();
        assert_eq!(enabled, [FilterOp::IsNull, FilterOp::IsNotNull]);
        // IN would split a value with a comma into several.
        let comma = quick_filters("name", Some("a,b"));
        assert!(!comma[13].as_ref().unwrap().enabled && comma[0].as_ref().unwrap().enabled);
        assert!(!quick_filters("n", Some(" x"))[13].as_ref().unwrap().enabled);
        // Long and multi-line values are shortened in the label only.
        let long = "x".repeat(100) + "\nnext";
        let item = quick_filters("n", Some(&long))[0].clone().unwrap();
        assert_eq!(item.filter.value, long);
        assert!(item.label.ends_with("…'") && !item.label.contains('\n'));
    }
    #[test]
    fn foreign_keys_select_the_referenced_row_by_bound_values() {
        let fk = ForeignKey {
            name: "fk".into(),
            columns: vec!["a_id".into(), "b_id".into()],
            target: TableRef {
                schema: "s".into(),
                name: "t".into(),
            },
            target_columns: vec!["id".into(), "part".into()],
        };
        let filters = foreign_key_filters(&fk, &[Some("5".into()), Some("x'y".into())]).unwrap();
        let w = build_where(
            &filters,
            &["id".into(), "part".into()],
            Driver::Mysql,
            Combine::And,
        );
        assert_eq!(w.sql, " WHERE `id` = ? AND `part` = ?");
        assert_eq!(w.params, [serde_json::json!("5"), serde_json::json!("x'y")]);
        assert_eq!(foreign_key_filters(&fk, &[Some("5".into()), None]), None);
        assert_eq!(foreign_key_filters(&fk, &[Some("5".into())]), None);
        // A DOUBLE AUTO_INCREMENT key as the grid shows it.
        let one = ForeignKey {
            columns: vec!["a".into()],
            target_columns: vec!["id".into()],
            ..fk
        };
        let f = foreign_key_filters(&one, &[Some("1e+20".into())]).unwrap();
        assert_eq!(f[0].value, "1e+20");
    }
    #[test]
    fn order_by_lists_every_sorted_column_then_falls_back_to_the_key() {
        let cols: Vec<String> = ["id", "name", "at"].map(String::from).to_vec();
        let sort = vec![
            OrderBy {
                column: "name".into(),
                dir: Direction::Desc,
            },
            OrderBy {
                column: "gone".into(),
                dir: Direction::Asc,
            },
            OrderBy {
                column: "at".into(),
                dir: Direction::Asc,
            },
        ];
        assert_eq!(
            order_clause(Driver::Postgres, &sort, &cols, &["id".into()]),
            " ORDER BY \"name\" DESC, \"at\" ASC"
        );
        assert_eq!(
            order_clause(Driver::Mysql, &[], &cols, &["id".into()]),
            " ORDER BY `id`"
        );
        assert_eq!(order_clause(Driver::Mysql, &sort[1..2], &cols, &[]), "");
    }
    #[test]
    fn page_settings_are_checked_like_tableplus() {
        assert_eq!(page_settings("300", "", None), Ok((300, 0)));
        assert_eq!(page_settings(" 50 ", "600", Some(1000)), Ok((50, 600)));
        assert_eq!(
            page_settings("10", "-1", None).unwrap_err(),
            "offset should be positive"
        );
        assert_eq!(
            page_settings("10", "x", None).unwrap_err(),
            "offset should be positive"
        );
        assert_eq!(
            page_settings("0", "0", None).unwrap_err(),
            "limitation should be greater than zero"
        );
        assert_eq!(
            page_settings("", "0", None).unwrap_err(),
            "limitation should be greater than zero"
        );
        assert_eq!(
            page_settings("10", "1000", Some(1000)).unwrap_err(),
            "offset should be smaller than the total"
        );
        // An estimate (None) or an empty table can't bound the offset.
        assert!(page_settings("10", "5000", None).is_ok());
        assert!(page_settings("10", "5", Some(0)).is_ok());
        assert!(page_settings(&(MAX_PAGE_ROWS + 1).to_string(), "", None).is_err());
        assert!(page_settings("99999999999999999999999", "", None).is_err());
    }
    #[test]
    fn applied_rules_are_recognised() {
        let a = rule("n", FilterOp::Eq, "1");
        let mut disabled = a.clone();
        disabled.enabled = false;
        assert!(rule_applied(&disabled, std::slice::from_ref(&a)));
        assert!(!rule_applied(&rule("n", FilterOp::Eq, "2"), &[a.clone()]));
        assert!(!rule_applied(&a, &[]));
        let mut between = rule("n", FilterOp::Between, "1");
        between.value2 = Some("2".into());
        let mut other = between.clone();
        other.value2 = Some("3".into());
        assert!(!rule_applied(&other, &[between]));
    }
}
