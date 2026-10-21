//! Safe Mode classification as the server will execute the text.
//!
//! `sql` keeps Electron's lexer for splitting, display and LIMIT handling. Servers tokenise
//! some inputs differently (MySQL `--1`, versioned `/*!NNNNN */` comments, backslash
//! escapes under another sql_mode), so this module re-reads the statement under every
//! plausible server interpretation and reports a danger if any of them is destructive.
use crate::types::Driver;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// Unquoted word, ASCII upper-cased.
    Word(String),
    /// Quoted identifier, exact content.
    Ident(String),
    Str(String),
    Num(String),
    Param(String),
    Op(String),
}
use Tok::*;

#[derive(Clone, Copy)]
struct Mode {
    driver: Driver,
    /// Backslash escapes in ordinary string literals (MySQL default; PostgreSQL with
    /// standard_conforming_strings off).
    backslash: bool,
    /// Treat `/*!NNNNN … */` content as code (server version at or above NNNNN).
    versioned: bool,
}

/// Whitespace exactly as both servers' lexers define it: MySQL's `my_isspace` and PostgreSQL's
/// `space` in scan.l. Anything else, NBSP and U+FEFF included, is not a separator.
pub fn is_sql_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}')
}
/// Characters that continue an unquoted identifier or keyword. Both servers accept every
/// non-ASCII character there, so `a\u{a0}WHERE` and `a·WHERE` are one identifier.
pub fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}
/// A PostgreSQL dollar-quote tag character: an identifier character other than `$`.
fn is_tag_char(c: char) -> bool {
    c != '$' && is_identifier_char(c)
}
fn word_char(c: char) -> bool {
    is_identifier_char(c)
}
/// What follows `--` for MySQL to treat it as a comment: whitespace or a control character.
fn mysql_comment_follows(c: Option<char>) -> bool {
    c.is_none_or(|a| a <= ' ' || a == '\u{7f}')
}

/// Statements as token lists, split on top-level `;`.
/// Statements as token lists, split on top-level `;`. `None` when this reading leaves a string,
/// quoted identifier or comment open: the server rejects such text before running anything.
fn lex(sql: &str, m: Mode) -> Option<Vec<Vec<Tok>>> {
    let s: Vec<char> = sql.chars().collect();
    let n = s.len();
    let mysql = m.driver == Driver::Mysql;
    let mut out = vec![vec![]];
    let mut depth = 0usize;
    let mut versioned_open = 0usize;
    let mut i = 0;
    let quoted = |i: usize, q: char, backslash: bool| -> Option<(usize, String)> {
        let mut j = i + 1;
        let mut text = String::new();
        while j < n {
            let c = s[j];
            if backslash && c == '\\' {
                if let Some(&next) = s.get(j + 1) {
                    text.push(next);
                }
                j += 2;
                continue;
            }
            if c == q {
                if s.get(j + 1) == Some(&q) {
                    text.push(q);
                    j += 2;
                    continue;
                }
                return Some((j + 1, text));
            }
            text.push(c);
            j += 1;
        }
        None
    };
    while i < n {
        let c = s[i];
        let next = s.get(i + 1).copied();
        if is_sql_space(c) {
            i += 1;
            continue;
        }
        if c == '-' && next == Some('-') {
            // MySQL only starts a comment when the second dash is followed by whitespace,
            // a control character or the end of input; `--1` is minus minus one.
            let after = s.get(i + 2).copied();
            if !mysql || mysql_comment_follows(after) {
                i = (i..n).find(|&j| s[j] == '\n').map_or(n, |j| j + 1);
                continue;
            }
        }
        if c == '#' && mysql {
            i = (i..n).find(|&j| s[j] == '\n').map_or(n, |j| j + 1);
            continue;
        }
        if c == '*' && next == Some('/') && versioned_open > 0 {
            versioned_open -= 1;
            i += 2;
            continue;
        }
        if c == '/' && next == Some('*') {
            let executable = mysql
                && (s.get(i + 2) == Some(&'!')
                    || (s.get(i + 2) == Some(&'M') && s.get(i + 3) == Some(&'!')));
            if executable {
                let mut j = i + if s[i + 2] == '!' { 3 } else { 4 };
                let digits = s[j..]
                    .iter()
                    .take(6)
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                // Without a version the server always runs the content.
                if digits == 0 || m.versioned {
                    if digits >= 5 {
                        j += digits;
                    }
                    versioned_open += 1;
                    i = j;
                    continue;
                }
            }
            if mysql {
                i = (i + 2..n)
                    .find(|&j| s[j] == '*' && s.get(j + 1) == Some(&'/'))
                    .map(|j| j + 2)?;
            } else {
                let mut level = 0;
                let mut j = i;
                while j < n {
                    if s[j] == '/' && s.get(j + 1) == Some(&'*') {
                        level += 1;
                        j += 2;
                    } else if s[j] == '*' && s.get(j + 1) == Some(&'/') {
                        level -= 1;
                        j += 2;
                        if level == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                if level != 0 {
                    return None;
                }
                i = j;
            }
            continue;
        }
        let tokens = out.last_mut().expect("at least one statement");
        if c == '\'' || (c == '"' && mysql) {
            let escape_prefix =
                !mysql && i > 0 && matches!(s[i - 1], 'E' | 'e') && (i < 2 || !word_char(s[i - 2]));
            // Typed literals (`b'1'`, `X'41'`, `N'x'`, `_utf8mb4'x'`, PG `E'x'`) are one literal.
            let prefixed = i > 0
                && word_char(s[i - 1])
                && matches!(tokens.last(), Some(Word(w)) if ["B", "X", "N", "E"].contains(&w.as_str()) || (mysql && w.starts_with('_')));
            // PostgreSQL `U&'…'` Unicode-escape strings.
            let unicode = !mysql
                && i >= 2
                && s[i - 1] == '&'
                && matches!(s[i - 2], 'U' | 'u')
                && (i < 3 || !word_char(s[i - 3]));
            if unicode {
                tokens.truncate(tokens.len().saturating_sub(2));
            }
            if escape_prefix || prefixed {
                tokens.pop();
            }
            let (end, text) = quoted(i, c, m.backslash || escape_prefix)?;
            tokens.push(Str(text));
            i = end;
            continue;
        }
        if (c == '`' && mysql) || (c == '"' && !mysql) {
            if c == '"'
                && i >= 2
                && s[i - 1] == '&'
                && matches!(s[i - 2], 'U' | 'u')
                && (i < 3 || !word_char(s[i - 3]))
            {
                tokens.truncate(tokens.len().saturating_sub(2));
            }
            let (end, text) = quoted(i, c, false)?;
            tokens.push(Ident(text));
            i = end;
            continue;
        }
        if c == '$' && !mysql && (i == 0 || !word_char(s[i - 1])) {
            if next.is_some_and(|d| d.is_ascii_digit()) {
                let end = (i + 1..n).find(|&j| !s[j].is_ascii_digit()).unwrap_or(n);
                tokens.push(Param(s[i..end].iter().collect()));
                i = end;
                continue;
            }
            let tag_end = (i + 1..n).find(|&j| !is_tag_char(s[j]));
            if let Some(t) = tag_end
                .filter(|&t| s[t] == '$' && !s.get(i + 1).is_some_and(|c| c.is_ascii_digit()))
            {
                let tag: Vec<char> = s[i..=t].to_vec();
                let body = t + 1;
                let close = (body..n).find(|&j| s[j..].starts_with(&tag));
                let j = close?;
                let (end, text) = (j + tag.len(), s[body..j].iter().collect());
                tokens.push(Str(text));
                i = end;
                continue;
            }
        }
        if c.is_ascii_digit() || (c == '.' && next.is_some_and(|d| d.is_ascii_digit())) {
            let mut j = i;
            if c == '0' && matches!(next, Some('x' | 'X')) {
                j += 2;
                while j < n && s[j].is_ascii_hexdigit() {
                    j += 1;
                }
            } else {
                // PostgreSQL 16+ allows `1_000`; in MySQL that is an identifier.
                while j < n
                    && (s[j].is_ascii_digit()
                        || s[j] == '.'
                        || (!mysql
                            && s[j] == '_'
                            && s.get(j + 1).is_some_and(|d| d.is_ascii_digit())))
                {
                    j += 1;
                }
                if j < n && matches!(s[j], 'e' | 'E') {
                    let mut k = j + 1;
                    if k < n && matches!(s[k], '+' | '-') {
                        k += 1;
                    }
                    if k < n && s[k].is_ascii_digit() {
                        j = k;
                        while j < n && s[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                }
            }
            if j < n && word_char(s[j]) {
                // `1abc` is an identifier in MySQL.
                while j < n && word_char(s[j]) {
                    j += 1;
                }
                tokens.push(Word(
                    s[i..j].iter().collect::<String>().to_ascii_uppercase(),
                ));
            } else {
                tokens.push(Num(s[i..j].iter().collect()));
            }
            i = j;
            continue;
        }
        if word_char(c) {
            let end = (i..n).find(|&j| !word_char(s[j])).unwrap_or(n);
            tokens.push(Word(
                s[i..end].iter().collect::<String>().to_ascii_uppercase(),
            ));
            i = end;
            continue;
        }
        if c == '?' && mysql {
            tokens.push(Param("?".into()));
            i += 1;
            continue;
        }
        if c == ';' && depth == 0 {
            out.push(vec![]);
            i += 1;
            continue;
        }
        let op = [
            "<=>", "->>", "<=", ">=", "<>", "!=", "||", "&&", "::", ":=", "->",
        ]
        .into_iter()
        .find(|op| s[i..].starts_with(&op.chars().collect::<Vec<_>>()))
        .map_or_else(|| c.to_string(), str::to_owned);
        if op == "(" {
            depth += 1;
        } else if op == ")" {
            depth = depth.saturating_sub(1);
        }
        i += op.chars().count();
        tokens.push(Op(op));
    }
    if versioned_open > 0 {
        return None;
    }
    out.retain(|t| !t.is_empty());
    Some(out)
}

fn variants(d: Driver) -> Vec<Mode> {
    let mut v = vec![];
    for backslash in [d == Driver::Mysql, d != Driver::Mysql] {
        for versioned in [true, false] {
            if d == Driver::Postgres && !versioned {
                continue;
            }
            v.push(Mode {
                driver: d,
                backslash,
                versioned,
            });
        }
    }
    v
}

fn is_word(t: Option<&Tok>, w: &str) -> bool {
    matches!(t, Some(Word(x)) if x == w)
}
fn is_op(t: Option<&Tok>, o: &str) -> bool {
    matches!(t, Some(Op(x)) if x == o)
}
fn has_word(t: &[Tok], w: &str) -> bool {
    t.iter().any(|t| is_word(Some(t), w))
}
/// Index of the first token after any leading `(`.
fn head(t: &[Tok]) -> usize {
    t.iter().take_while(|t| is_op(Some(t), "(")).count()
}
fn keyword(t: &[Tok]) -> &str {
    match t.get(head(t)) {
        Some(Word(w)) => w,
        _ => "",
    }
}

const EXECUTED_BODIES: [&str; 12] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "REPLACE", "MERGE", "WITH", "TABLE", "VALUES",
    "CREATE", "EXECUTE", "DECLARE",
];

/// `EXPLAIN ANALYZE …` / `DESC ANALYZE …` run the statement they explain.
fn analyzed_body(t: &[Tok]) -> Option<&[Tok]> {
    if !["EXPLAIN", "DESC", "DESCRIBE"].contains(&keyword(t)) {
        return None;
    }
    let start = head(t) + 1;
    let body = (start..t.len()).find(|&i| {
        matches!(&t[i], Word(w) if EXECUTED_BODIES.contains(&w.as_str()))
            && !is_op(t.get(i.wrapping_sub(1)), "=")
    })?;
    let options = &t[start..body];
    (has_word(options, "ANALYZE") || has_word(options, "ANALYSE")).then(|| &t[body..])
}

/// Positions of DELETE/UPDATE keywords that start a statement, including CTE bodies.
fn dml_starts(t: &[Tok]) -> Vec<usize> {
    let with = keyword(t) == "WITH";
    let mut depth = 0usize;
    let mut out = vec![];
    for (i, tok) in t.iter().enumerate() {
        match tok {
            Op(o) if o == "(" => depth += 1,
            Op(o) if o == ")" => depth = depth.saturating_sub(1),
            Word(w) if w == "DELETE" || w == "UPDATE" => {
                let prev = i.checked_sub(1).map(|p| &t[p]);
                let starts = match prev {
                    None => true,
                    Some(Op(o)) if o == "(" => true,
                    Some(Op(o)) if o == ")" => with && depth == 0,
                    _ => false,
                };
                if starts {
                    out.push(i);
                }
            }
            _ => {}
        }
    }
    out
}

/// Tokens of the statement starting at `start`, up to the end of its enclosing group.
fn group(t: &[Tok], start: usize) -> &[Tok] {
    let mut depth = 0isize;
    for (i, tok) in t.iter().enumerate().skip(start) {
        if is_op(Some(tok), "(") {
            depth += 1;
        } else if is_op(Some(tok), ")") {
            depth -= 1;
            if depth < 0 {
                return &t[start..i];
            }
        }
    }
    &t[start..]
}
/// Indices of tokens at the group's own nesting level.
fn top_indices(t: &[Tok]) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = vec![];
    for (i, tok) in t.iter().enumerate() {
        if is_op(Some(tok), "(") {
            if depth == 0 {
                out.push(i);
            }
            depth += 1;
        } else if is_op(Some(tok), ")") {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(i);
        }
    }
    out
}

fn where_reason(stmt: &[Tok]) -> Option<String> {
    let what = match stmt.first() {
        Some(Word(w)) => w.clone(),
        _ => return None,
    };
    let top = top_indices(stmt);
    let Some(&w) = top.iter().find(|&&i| is_word(stmt.get(i), "WHERE")) else {
        return Some(format!(
            "{what} without WHERE affects every row of the table"
        ));
    };
    let end = top
        .iter()
        .copied()
        .find(|&i| {
            i > w && matches!(&stmt[i], Word(x) if ["ORDER", "LIMIT", "RETURNING"].contains(&x.as_str()))
        })
        .unwrap_or(stmt.len());
    let cond = &stmt[w + 1..end];
    if cond.is_empty() {
        return Some(format!(
            "{what} without WHERE affects every row of the table"
        ));
    }
    match truth(cond, 0) {
        Truth::True => Some(format!(
            "{what} with an always-true WHERE affects every row of the table"
        )),
        Truth::TooDeep => Some(format!(
            "{what} has a WHERE too complex to verify; it may affect every row of the table"
        )),
        _ if !references_column(cond) => Some(format!(
            "{what} whose WHERE refers to no column can't limit which rows change; it may affect every row of the table"
        )),
        _ if cond.len() == 4
            && matches!(cond[0], Word(_) | Ident(_))
            && is_word(cond.get(1), "IS")
            && is_word(cond.get(2), "NOT")
            && is_word(cond.get(3), "NULL") =>
        {
            Some(format!(
                "{what} whose only condition is IS NOT NULL may affect every row of the table"
            ))
        }
        _ => None,
    }
}

/// Words in a condition that are syntax or built-in values, never a column of the table.
const NOT_COLUMNS: &[&str] = &[
    "AND",
    "OR",
    "XOR",
    "NOT",
    "IS",
    "NULL",
    "TRUE",
    "FALSE",
    "UNKNOWN",
    "IN",
    "BETWEEN",
    "SYMMETRIC",
    "LIKE",
    "ILIKE",
    "SIMILAR",
    "TO",
    "ESCAPE",
    "REGEXP",
    "RLIKE",
    "SOUNDS",
    "EXISTS",
    "ANY",
    "ALL",
    "SOME",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "DISTINCT",
    "FROM",
    "BINARY",
    "DIV",
    "MOD",
    "INTERVAL",
    "COLLATE",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "CURRENT_USER",
    "SESSION_USER",
    "CURRENT_ROLE",
    "AS",
    "USING",
    "ARRAY",
    "ROW",
    "UTC_DATE",
    "UTC_TIME",
    "UTC_TIMESTAMP",
    "CURRENT_CATALOG",
    "CURRENT_SCHEMA",
];
/// Whether a WHERE can depend on the row at all: it names a column outside a subquery, or a
/// qualified column inside one (which may be correlated). A WHERE of constants, parameters,
/// functions and uncorrelated subqueries gives the same answer for every row.
fn references_column(cond: &[Tok]) -> bool {
    let mut subquery_depth: Vec<usize> = vec![];
    let mut depth = 0usize;
    for (i, tok) in cond.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| &cond[p]);
        let next = cond.get(i + 1);
        match tok {
            Op(o) if o == "(" => {
                depth += 1;
                if is_word(next, "SELECT") {
                    subquery_depth.push(depth);
                }
            }
            Op(o) if o == ")" => {
                if subquery_depth.last() == Some(&depth) {
                    subquery_depth.pop();
                }
                depth = depth.saturating_sub(1);
            }
            Word(_) | Ident(_) => {
                let in_subquery = !subquery_depth.is_empty();
                let qualified = is_op(next, ".") || is_op(prev, ".");
                let syntax = match tok {
                    Word(w) => {
                        NOT_COLUMNS.contains(&w.as_str())
                            // A call, a type after `::`/AS/COLLATE, or an INTERVAL unit.
                            || is_op(next, "(")
                            || is_op(prev, "::")
                            || is_word(prev, "AS")
                            || is_word(prev, "COLLATE")
                            // `@user_var` / `@@system_var`.
                            || is_op(prev, "@")
                            || is_word(prev, "USING")
                            // Typed literals: DATE '…', TIMESTAMP '…', INTERVAL '…'.
                            || (["DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "TIMETZ", "INTERVAL"]
                                .contains(&w.as_str())
                                && matches!(next, Some(Str(_))))
                            || i >= 2 && is_word(cond.get(i - 2), "INTERVAL")
                    }
                    _ => false,
                };
                if !syntax && (!in_subquery || qualified) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Truth {
    True,
    False,
    Unknown,
    TooDeep,
}
fn not(t: Truth) -> Truth {
    match t {
        Truth::True => Truth::False,
        Truth::False => Truth::True,
        x => x,
    }
}
/// Split at top-level occurrences of any connective. Empty pieces mean a parse we don't know.
fn split_top<'a>(t: &'a [Tok], seps: &[&str]) -> Vec<&'a [Tok]> {
    let mut parts = vec![];
    let mut depth = 0usize;
    let mut start = 0;
    let mut between = false;
    for (i, tok) in t.iter().enumerate() {
        match tok {
            Op(o) if o == "(" => depth += 1,
            Op(o) if o == ")" => depth = depth.saturating_sub(1),
            Word(w) if w == "BETWEEN" => between = true,
            Word(w) | Op(w) if depth == 0 && seps.contains(&w.as_str()) => {
                // The AND of `BETWEEN a AND b` is not a connective.
                if w == "AND" && between {
                    between = false;
                    continue;
                }
                parts.push(&t[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&t[start..]);
    parts
}
fn constant(t: &[Tok]) -> Option<f64> {
    let (neg, rest) = match t {
        [Op(o), rest @ ..] if o == "-" => (true, rest),
        [Op(o), rest @ ..] if o == "+" => (false, rest),
        _ => (false, t),
    };
    let v = match rest {
        [Num(n)] => {
            if let Some(h) = n.strip_prefix("0x").or_else(|| n.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).ok()? as f64
            } else {
                n.replace('_', "").parse().ok()?
            }
        }
        [Word(w)] if w == "TRUE" => 1.0,
        [Word(w)] if w == "FALSE" => 0.0,
        // A string's truth or numeric value depends on the server and the column's collation.
        _ => return None,
    };
    Some(if neg { -v } else { v })
}
fn truth(t: &[Tok], depth: usize) -> Truth {
    if depth > 64 {
        return Truth::TooDeep;
    }
    let ors = split_top(t, &["OR", "||", "XOR"]);
    if ors.len() > 1 {
        if t.iter().any(|x| is_word(Some(x), "XOR")) {
            return Truth::Unknown;
        }
        let parts: Vec<_> = ors.iter().map(|p| truth(p, depth + 1)).collect();
        return if parts.contains(&Truth::TooDeep) {
            Truth::TooDeep
        } else if parts.contains(&Truth::True) || null_or_not_null(&ors) {
            Truth::True
        } else if parts.iter().all(|p| *p == Truth::False) {
            Truth::False
        } else {
            Truth::Unknown
        };
    }
    let ands = split_top(t, &["AND", "&&"]);
    if ands.len() > 1 {
        let parts: Vec<_> = ands.iter().map(|p| truth(p, depth + 1)).collect();
        return if parts.contains(&Truth::TooDeep) {
            Truth::TooDeep
        } else if parts.contains(&Truth::False) {
            Truth::False
        } else if parts.iter().all(|p| *p == Truth::True) {
            Truth::True
        } else {
            Truth::Unknown
        };
    }
    match t {
        [] => Truth::Unknown,
        [Word(w), rest @ ..] if w == "NOT" => not(truth(rest, depth + 1)),
        [Op(o), rest @ ..] if o == "!" => not(truth(rest, depth + 1)),
        [Op(open), inner @ .., Op(close)]
            if open == "(" && close == ")" && group(t, 1).len() == inner.len() =>
        {
            truth(inner, depth + 1)
        }
        _ => comparison(t),
    }
}
/// `x IS NULL OR x IS NOT NULL` for the same `x`.
fn null_or_not_null(parts: &[&[Tok]]) -> bool {
    let operand = |p: &[Tok], negated: bool| -> Option<Vec<Tok>> {
        let tail: &[&str] = if negated {
            &["IS", "NOT", "NULL"]
        } else {
            &["IS", "NULL"]
        };
        let n = p.len().checked_sub(tail.len())?;
        (n > 0 && p[n..].iter().zip(tail).all(|(t, w)| is_word(Some(t), w)))
            .then(|| p[..n].to_vec())
    };
    parts.iter().any(|a| {
        operand(a, false)
            .is_some_and(|x| parts.iter().any(|b| operand(b, true).as_ref() == Some(&x)))
    })
}
fn is_null(t: &[Tok]) -> bool {
    matches!(t, [Word(w)] if w == "NULL")
}
/// A constant operand: a number, TRUE/FALSE, a string or NULL. No column, call or subquery.
fn literal_operand(t: &[Tok]) -> bool {
    constant(t).is_some() || is_null(t) || matches!(t, [Str(_)])
}
fn comparison(t: &[Tok]) -> Truth {
    if let Some(v) = constant(t) {
        return if v != 0.0 { Truth::True } else { Truth::False };
    }
    if is_null(t) {
        return Truth::False;
    }
    // EXISTS (SELECT <constants>) without a FROM always has its one row.
    if let [Word(e), Op(open), Word(sel), inner @ .., Op(close)] = t {
        if e == "EXISTS"
            && open == "("
            && close == ")"
            && sel == "SELECT"
            && group(t, 2).len() == inner.len() + 1
            && !has_word(inner, "FROM")
            && !has_word(inner, "WHERE")
        {
            return Truth::True;
        }
    }
    let top = top_indices(t);
    // `c IN (…, c, …)` and `a BETWEEN b AND c` over constants.
    if let Some(&at) = top.iter().find(|&&i| is_word(t.get(i), "IN")) {
        let left = &t[..at];
        if literal_operand(left) && !is_null(left) {
            if let [Op(open), list @ .., Op(close)] = &t[at + 1..] {
                if open == "("
                    && close == ")"
                    && split_top(list, &[","]).iter().any(|item| *item == left)
                {
                    return Truth::True;
                }
            }
        }
        return Truth::Unknown;
    }
    if let Some(&at) = top.iter().find(|&&i| is_word(t.get(i), "BETWEEN")) {
        let bounds = split_top(&t[at + 1..], &["AND"]);
        if let (Some(v), [lo, hi]) = (constant(&t[..at]), bounds.as_slice()) {
            if let (Some(lo), Some(hi)) = (constant(lo), constant(hi)) {
                return if lo <= v && v <= hi {
                    Truth::True
                } else {
                    Truth::False
                };
            }
        }
        return Truth::Unknown;
    }
    let top = top_indices(t);
    let Some(&at) = top.iter().find(|&&i| {
        matches!(&t[i], Op(o) if ["=", "<=>", "<>", "!=", "<", ">", "<=", ">="].contains(&o.as_str()))
            || matches!(&t[i], Word(w) if w == "IS" || w == "LIKE")
    }) else {
        return Truth::Unknown;
    };
    let left = &t[..at];
    let (op, right) = match &t[at] {
        Word(w) if w == "IS" => {
            let rest = &t[at + 1..];
            let (negated, rest) = match rest {
                [Word(n), r @ ..] if n == "NOT" => (true, r),
                _ => (false, rest),
            };
            if let [Word(d), Word(f), r @ ..] = rest {
                if d == "DISTINCT" && f == "FROM" {
                    let same = !left.is_empty() && left == r;
                    return match (same, negated) {
                        (true, true) => Truth::True,
                        (true, false) => Truth::False,
                        _ => Truth::Unknown,
                    };
                }
            }
            let v = match rest {
                [Word(x)] if x == "TRUE" => constant(left).map(|v| v != 0.0),
                [Word(x)] if x == "FALSE" => constant(left).map(|v| v == 0.0),
                [Word(x)] if x == "NULL" => {
                    if is_null(left) {
                        Some(true)
                    } else {
                        constant(left).map(|_| false)
                    }
                }
                _ => None,
            };
            return match v {
                Some(b) => {
                    if b != negated {
                        Truth::True
                    } else {
                        Truth::False
                    }
                }
                None => Truth::Unknown,
            };
        }
        Word(_) => ("LIKE".to_owned(), &t[at + 1..]),
        Op(o) => (o.clone(), &t[at + 1..]),
        _ => return Truth::Unknown,
    };
    if left.is_empty() || right.is_empty() {
        return Truth::Unknown;
    }
    // Qualifiers are dropped only when one side has none (`t.id = id`): `t.id = u.id` is a join.
    let qualified = |t: &[Tok]| t.iter().any(|x| is_op(Some(x), "."));
    let unqualify = !(qualified(left) && qualified(right));
    let (left, right) = (
        &normalized(left, unqualify)[..],
        &normalized(right, unqualify)[..],
    );
    if op == "LIKE" && matches!(right, [Str(p)] if p == "%") && deterministic(left) {
        return Truth::True;
    }
    if left == right && deterministic(left) {
        return if ["=", "<=>", "<=", ">=", "LIKE"].contains(&op.as_str()) {
            Truth::True
        } else {
            Truth::False
        };
    }
    if let ([Str(a)], [Str(b)]) = (left, right) {
        if op == "=" || op == "<=>" {
            return if a == b { Truth::True } else { Truth::Unknown };
        }
    }
    let (Some(a), Some(b)) = (constant(left), constant(right)) else {
        return Truth::Unknown;
    };
    let r = match op.as_str() {
        "=" | "<=>" => a == b,
        "<>" | "!=" => a != b,
        "<" => a < b,
        ">" => a > b,
        "<=" => a <= b,
        ">=" => a >= b,
        _ => return Truth::Unknown,
    };
    if r { Truth::True } else { Truth::False }
}

/// Functions whose result can differ between two calls in the same statement.
const VOLATILE: &[&str] = &[
    "RAND",
    "RANDOM",
    "UUID",
    "UUID_SHORT",
    "GEN_RANDOM_UUID",
    "NEXTVAL",
    "SYSDATE",
    "CLOCK_TIMESTAMP",
    "RANDOM_BYTES",
    "SETVAL",
    "LAST_INSERT_ID",
    "ROW_COUNT",
    "FOUND_ROWS",
    "GET_LOCK",
    "SLEEP",
    "BENCHMARK",
];
fn deterministic(t: &[Tok]) -> bool {
    !t.iter().enumerate().any(|(i, x)| {
        matches!(x, Word(w) if VOLATILE.contains(&w.as_str()) && is_op(t.get(i + 1), "("))
            || matches!(x, Word(w) if w == "SELECT")
    })
}
/// An operand with qualifiers (`t.id`) and quoting (`` `id` ``, `"id"`) removed and identity
/// arithmetic folded (`x + 0`, `x * 1`, `x - x`, `x * 0`), so `id = t.id + 0` compares equal sides.
fn normalized(t: &[Tok], unqualify: bool) -> Vec<Tok> {
    let mut out: Vec<Tok> = vec![];
    for (i, x) in t.iter().enumerate() {
        let name = match x {
            Ident(w) => Some(Word(w.to_ascii_uppercase())),
            Word(_) => Some(x.clone()),
            _ => None,
        };
        if unqualify
            && name.is_some()
            && is_op(t.get(i + 1), ".")
            && matches!(t.get(i + 2), Some(Word(_) | Ident(_)))
        {
            continue;
        }
        if unqualify && is_op(Some(x), ".") && i > 0 && matches!(t[i - 1], Word(_) | Ident(_)) {
            continue;
        }
        out.push(name.unwrap_or_else(|| x.clone()));
    }
    let num = |t: Option<&Tok>, v: &str| matches!(t, Some(Num(n)) if n == v);
    loop {
        let n = out.len();
        if n >= 3
            && (is_op(out.get(n - 2), "+") || is_op(out.get(n - 2), "-"))
            && num(out.get(n - 1), "0")
            || n >= 3
                && (is_op(out.get(n - 2), "*") || is_op(out.get(n - 2), "/"))
                && num(out.get(n - 1), "1")
        {
            out.truncate(n - 2);
        } else if n >= 3
            && (num(out.first(), "0") && is_op(out.get(1), "+")
                || num(out.first(), "1") && is_op(out.get(1), "*"))
        {
            out.drain(..2);
        } else {
            break;
        }
    }
    // `x - x` and `x * 0` (x a single name) are 0.
    if let [a, Op(o), b] = &out[..] {
        let zero = (o == "-" && a == b && matches!(a, Word(_)))
            || (o == "*" && (num(Some(a), "0") || num(Some(b), "0")) && deterministic(&out));
        if zero {
            return vec![Num("0".into())];
        }
    }
    out
}
/// `var = value` pairs of a SET statement, with the variable name upper-cased.
fn set_assignments(t: &[Tok]) -> Vec<(String, &[Tok])> {
    split_top(&t[1..], &[","])
        .into_iter()
        .filter_map(|part| {
            let at = top_indices(part).into_iter().find(|&i| {
                is_op(part.get(i), "=") || is_op(part.get(i), ":=") || is_word(part.get(i), "TO")
            })?;
            let var = part[..at].iter().rev().find_map(|x| match x {
                Word(w) => Some(w.clone()),
                Ident(w) => Some(w.to_ascii_uppercase()),
                _ => None,
            })?;
            Some((var, &part[at + 1..]))
        })
        .collect()
}
fn is_off(value: &[Tok]) -> bool {
    match value {
        [Num(n)] => n == "0",
        [Word(w)] => w == "OFF" || w == "FALSE",
        [Str(s)] => ["0", "off", "false"].contains(&s.to_ascii_lowercase().as_str()),
        _ => false,
    }
}
/// A SET that switches off a protection: those need confirmation in every Safe Mode.
/// Restoring a saved value (`= @OLD_…`), strictening sql_mode or turning checks on doesn't.
fn weakening_setting(t: &[Tok]) -> Option<String> {
    set_assignments(t).into_iter().find_map(|(var, value)| {
        let weak = match var.as_str() {
            "SQL_SAFE_UPDATES" | "FOREIGN_KEY_CHECKS" => is_off(value),
            "TRANSACTION_READ_ONLY" | "DEFAULT_TRANSACTION_READ_ONLY" | "TX_READ_ONLY" => is_off(value),
            "SESSION_REPLICATION_ROLE" => matches!(value, [Word(w)] if w == "REPLICA" || w == "LOCAL")
                || matches!(value, [Str(s)] if s.eq_ignore_ascii_case("replica") || s.eq_ignore_ascii_case("local")),
            "SQL_MODE" => matches!(value, [Str(mode)] if {
                let m = mode.to_ascii_uppercase();
                !["STRICT_TRANS_TABLES", "STRICT_ALL_TABLES", "TRADITIONAL"].iter().any(|s| m.contains(s))
            }),
            _ => false,
        };
        weak.then(|| format!("SET {} turns off a safety check for this session", var.to_ascii_lowercase()))
    })
}
/// Reasons worth a confirmation only when the user chose a Safe Mode other than Silent.
fn statement_caution(t: &[Tok]) -> Option<String> {
    let t = &t[head(t)..];
    match keyword(t) {
        "CALL" => Some("CALL runs a stored procedure that may change data".into()),
        "SET" => set_assignments(t).into_iter().find_map(|(var, value)| {
            (["UNIQUE_CHECKS", "AUTOCOMMIT"].contains(&var.as_str()) && is_off(value)).then(|| {
                format!(
                    "SET {} = 0 changes how later statements are checked or committed",
                    var.to_ascii_lowercase()
                )
            })
        }),
        _ => None,
    }
}

fn statement_reason(t: &[Tok]) -> Option<String> {
    if let Some(body) = analyzed_body(t) {
        return statement_reason(body).map(|r| format!("EXPLAIN ANALYZE runs the statement: {r}"));
    }
    let t = &t[head(t)..];
    let kw = keyword(t);
    let reason = match kw {
        "TRUNCATE" => Some("TRUNCATE removes every row of the table".into()),
        "DROP" => Some("DROP permanently removes the object and its data".into()),
        "ALTER" if has_word(t, "DROP") => {
            Some("ALTER … DROP removes a column, index or constraint".into())
        }
        "ALTER" if changes_column_type_or_name(t) => Some(
            "ALTER that changes a column's type or a name can alter or truncate existing values"
                .into(),
        ),
        "RENAME" => {
            Some("RENAME changes which object existing queries and applications reach".into())
        }
        "REPLACE" => {
            Some("REPLACE deletes any existing row with the same key before inserting".into())
        }
        "INSERT"
            if has_word(t, "SELECT")
                && (has_word(t, "DUPLICATE")
                    || (has_word(t, "CONFLICT") && has_word(t, "UPDATE"))) =>
        {
            Some("INSERT … SELECT that updates on conflict can overwrite many existing rows".into())
        }
        "MERGE" if has_word(t, "DELETE") || has_word(t, "UPDATE") => {
            Some("MERGE … THEN DELETE/UPDATE can change every matched row".into())
        }
        "LOAD" => Some("LOAD DATA imports rows from a file and can replace existing rows".into()),
        "COPY" if has_word(t, "PROGRAM") => {
            Some("COPY … PROGRAM runs a shell command on the database server".into())
        }
        "COPY" if has_word(t, "FROM") => Some("COPY … FROM loads rows into the table".into()),
        "COPY" if has_word(t, "TO") && !has_word(t, "STDOUT") => {
            Some("COPY … TO writes a file on the database server".into())
        }
        "DO" => Some("DO runs code that Telecat can't inspect; it may change data".into()),
        "PREPARE" | "EXECUTE" => {
            Some("PREPARE/EXECUTE runs dynamic SQL that Telecat can't inspect".into())
        }
        "HANDLER" => {
            Some("HANDLER reads and writes rows directly, bypassing the usual checks".into())
        }
        "SET" => weakening_setting(t),
        // MariaDB's CREATE OR REPLACE TABLE drops the existing table and its rows.
        "CREATE"
            if is_word(t.get(1), "OR")
                && is_word(t.get(2), "REPLACE")
                && is_word(t.get(3), "TABLE") =>
        {
            Some("CREATE OR REPLACE TABLE drops the existing table and its data".into())
        }
        _ => None,
    };
    if reason.is_some() {
        return reason;
    }
    dml_starts(t)
        .into_iter()
        .find_map(|i| where_reason(group(t, i)))
}

/// `MODIFY`, `CHANGE`, `RENAME`, `CONVERT TO`, `ALTER [COLUMN] c [SET DATA] TYPE`. Column names
/// such as `type` don't count.
fn changes_column_type_or_name(t: &[Tok]) -> bool {
    t.iter().enumerate().any(|(i, tok)| match tok {
        Word(w) if ["MODIFY", "CHANGE", "RENAME"].contains(&w.as_str()) => {
            !is_word(t.get(i.wrapping_sub(1)), "COLUMN")
                && !is_word(t.get(i.wrapping_sub(1)), "ADD")
        }
        Word(w) if w == "CONVERT" => is_word(t.get(i + 1), "TO"),
        Word(w) if w == "TYPE" => {
            is_word(t.get(i.wrapping_sub(1)), "DATA")
                || is_word(t.get(i.wrapping_sub(2)), "COLUMN")
                || is_word(t.get(i.wrapping_sub(2)), "ALTER")
        }
        _ => false,
    })
}
fn is_program(t: &[Tok]) -> bool {
    keyword(t) == "CREATE"
        && t.iter().take(12).any(|x| {
            matches!(x, Word(w) if ["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"].contains(&w.as_str()))
        })
}

/// Dangers that Electron's classifier misses. `None` when every interpretation is safe.
pub fn server_dangerous_reason(sql: &str, d: Driver) -> Option<String> {
    let all = variants(d);
    for (n, m) in all.iter().enumerate() {
        // A reading that leaves a quote or comment open is a syntax error: it runs nothing.
        let Some(stmts) = lex(sql, *m) else {
            continue;
        };
        let note = if n == 0 {
            ""
        } else if !m.versioned {
            " (if the server skips a /*!…*/ versioned comment)"
        } else {
            " (under a different string-escaping mode)"
        };
        if stmts.len() > 1 && !is_program(&stmts[0]) {
            return Some(format!(
                "The server reads this as {} statements: comment or quote syntax hides part of it{note}",
                stmts.len()
            ));
        }
        if let Some(r) = stmts.first().and_then(|t| statement_reason(t)) {
            return Some(format!("{r}{note}"));
        }
    }
    None
}

/// A confirmation reason for Safe Modes other than Silent, for statements that are routine but
/// can still change data or session behaviour (CALL, `SET autocommit = 0`, …).
pub fn server_caution_reason(sql: &str, d: Driver) -> Option<String> {
    let stmts = lex(sql, variants(d)[0])?;
    stmts.first().and_then(|t| statement_caution(t))
}

/// False when any server interpretation can write.
pub fn server_read_only(sql: &str, d: Driver) -> bool {
    variants(d).into_iter().all(|m| {
        let Some(stmts) = lex(sql, m) else {
            return true;
        };
        if stmts.len() > 1 {
            return false;
        }
        let Some(t) = stmts.first() else {
            return true;
        };
        if let Some(body) = analyzed_body(t) {
            return keyword(body) == "SELECT"
                && dml_starts(body).is_empty()
                && !has_word(body, "INTO");
        }
        dml_starts(t).is_empty()
    })
}

const AGGREGATES: [&str; 30] = [
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "GROUP_CONCAT",
    "STRING_AGG",
    "ARRAY_AGG",
    "JSON_AGG",
    "JSONB_AGG",
    "JSON_OBJECT_AGG",
    "JSONB_OBJECT_AGG",
    "JSON_ARRAYAGG",
    "JSON_OBJECTAGG",
    "BIT_AND",
    "BIT_OR",
    "BIT_XOR",
    "BOOL_AND",
    "BOOL_OR",
    "EVERY",
    "STD",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "VARIANCE",
    "VAR_POP",
    "VAR_SAMP",
    "XMLAGG",
    "ANY_VALUE",
    "PERCENTILE_CONT",
];
/// Why one statement's tokens are not a plain single-table-style SELECT whose rows map one to one
/// onto stored rows. `None` when they are.
fn select_shape_reason(t: &[Tok]) -> Option<&'static str> {
    if !is_word(t.first(), "SELECT") {
        return Some("not a SELECT");
    }
    for (i, tok) in t.iter().enumerate() {
        let Word(w) = tok else { continue };
        let call = is_op(t.get(i + 1), "(");
        let reason = match w.as_str() {
            "SELECT" if i > 0 => "subquery",
            "DISTINCT" | "DISTINCTROW" => "DISTINCT",
            "GROUP" | "HAVING" | "ROLLUP" | "CUBE" | "GROUPING" => "GROUP BY",
            "UNION" | "INTERSECT" | "EXCEPT" | "MINUS" => "UNION/INTERSECT/EXCEPT",
            "OVER" | "WINDOW" => "window function",
            "WITH" | "RECURSIVE" => "WITH (CTE)",
            "INTO" => "SELECT … INTO",
            "FOR" | "LOCK" => "locking read",
            "LATERAL" | "JSON_TABLE" | "UNNEST" | "GENERATE_SERIES" => "table function",
            "PROCEDURE" | "CALL" => "procedure",
            w if call && AGGREGATES.contains(&w) => "aggregate",
            _ => continue,
        };
        return Some(reason);
    }
    None
}
/// Why a statement's result can't be edited row by row: `None` only when every way the server
/// may read the text is one plain SELECT without DISTINCT, grouping, aggregates, set operations,
/// window functions, CTEs, subqueries or locking clauses. Which columns are editable is then
/// decided from the result's protocol metadata, never from this text.
pub fn editable_select_reason(sql: &str, d: Driver) -> Option<String> {
    for (n, m) in variants(d).into_iter().enumerate() {
        let Some(stmts) = lex(sql, m) else {
            if n == 0 {
                return Some("the SQL could not be read".into());
            }
            continue;
        };
        let stmts: Vec<_> = stmts.into_iter().filter(|t| !t.is_empty()).collect();
        if stmts.len() != 1 {
            return Some("not a single statement".into());
        }
        if let Some(r) = select_shape_reason(&stmts[0]) {
            return Some(r.into());
        }
    }
    None
}

/// True when no way the server may read `sql` makes it more than one statement (a stored
/// program's body counts as one). Script runners that send text as-is must check this.
pub fn single_statement(sql: &str, d: Driver) -> bool {
    variants(d).into_iter().all(|m| {
        lex(sql, m).is_none_or(|stmts| {
            let stmts: Vec<_> = stmts.into_iter().filter(|t| !t.is_empty()).collect();
            stmts.len() <= 1 || is_program(&stmts[0])
        })
    })
}

/// Byte ranges of `sql` that the server parses as code (not strings, identifiers or
/// comments). Placeholders only exist there. Executable `/*!…*/` content counts as code.
pub fn code_ranges(sql: &str, d: Driver) -> Vec<std::ops::Range<usize>> {
    let mysql = d == Driver::Mysql;
    let b: Vec<(usize, char)> = sql.char_indices().collect();
    let n = b.len();
    let at = |i: usize| b.get(i).map(|x| x.1);
    let pos = |i: usize| b.get(i).map_or(sql.len(), |x| x.0);
    let mut out = vec![];
    let mut start = 0;
    let mut i = 0;
    let close = |out: &mut Vec<std::ops::Range<usize>>, from: usize, to: usize| {
        if to > from {
            out.push(pos(from)..pos(to));
        }
    };
    while i < n {
        let c = b[i].1;
        let next = at(i + 1);
        let skip_to =
            if c == '-' && next == Some('-') && (!mysql || mysql_comment_follows(at(i + 2)))
                || (c == '#' && mysql)
            {
                Some((i..n).find(|&j| b[j].1 == '\n').map_or(n, |j| j + 1))
            } else if c == '/' && next == Some('*') {
                if mysql && matches!(at(i + 2), Some('!')) {
                    i += 3;
                    continue;
                }
                if mysql {
                    Some(
                        (i + 2..n)
                            .find(|&j| b[j].1 == '*' && at(j + 1) == Some('/'))
                            .map_or(n, |j| j + 2),
                    )
                } else {
                    let mut level = 0;
                    let mut j = i;
                    while j < n {
                        if b[j].1 == '/' && at(j + 1) == Some('*') {
                            level += 1;
                            j += 2;
                        } else if b[j].1 == '*' && at(j + 1) == Some('/') {
                            level -= 1;
                            j += 2;
                            if level == 0 {
                                break;
                            }
                        } else {
                            j += 1;
                        }
                    }
                    Some(j)
                }
            } else if c == '\'' || c == '"' || (c == '`' && mysql) {
                let backslash = c != '`'
                    && (mysql
                        || (c == '\''
                            && i > 0
                            && matches!(b[i - 1].1, 'E' | 'e')
                            && (i < 2 || !word_char(b[i - 2].1))));
                let mut j = i + 1;
                let mut end = n;
                while j < n {
                    if backslash && b[j].1 == '\\' {
                        j += 2;
                        continue;
                    }
                    if b[j].1 == c {
                        if at(j + 1) == Some(c) {
                            j += 2;
                            continue;
                        }
                        end = j + 1;
                        break;
                    }
                    j += 1;
                }
                Some(end)
            } else if c == '$'
                && !mysql
                && (i == 0 || !word_char(b[i - 1].1))
                && !next.is_some_and(|d| d.is_ascii_digit())
            {
                let tag_end = (i + 1..n).find(|&j| !is_tag_char(b[j].1));
                tag_end.filter(|&t| b[t].1 == '$').map(|t| {
                    let tag: String = b[i..=t].iter().map(|x| x.1).collect();
                    let body = pos(t + 1);
                    sql[body..].find(&tag).map_or(n, |off| {
                        let byte = body + off + tag.len();
                        b.partition_point(|x| x.0 < byte)
                    })
                })
            } else {
                None
            };
        match skip_to {
            Some(end) => {
                close(&mut out, start, i);
                i = end.max(i + 1);
                start = i;
            }
            None => i += 1,
        }
    }
    close(&mut out, start, n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Driver::*;
    #[test]
    fn mysql_dash_dash_needs_whitespace() {
        assert_eq!(
            lex(
                "SELECT 1 --1",
                Mode {
                    driver: Mysql,
                    backslash: true,
                    versioned: true
                }
            )
            .unwrap()[0]
                .len(),
            5
        );
        assert_eq!(
            lex(
                "SELECT 1 -- 1",
                Mode {
                    driver: Mysql,
                    backslash: true,
                    versioned: true
                }
            )
            .unwrap()[0]
                .len(),
            2
        );
        assert_eq!(
            lex(
                "SELECT 1 --1",
                Mode {
                    driver: Postgres,
                    backslash: false,
                    versioned: true
                }
            )
            .unwrap()[0]
                .len(),
            2
        );
    }
    #[test]
    fn ordinary_statements_are_not_flagged() {
        for (d, sql) in [
            (Mysql, "DELETE FROM t WHERE id = 1"),
            (
                Mysql,
                "UPDATE t SET v = 1 WHERE id IN (1, 2) AND v BETWEEN 1 AND 3",
            ),
            (Mysql, "SELECT * FROM t WHERE a = b"),
            (Postgres, "UPDATE t SET v = 1 WHERE \"id\" = \"parent_id\""),
            (
                Postgres,
                "DELETE FROM t WHERE created < now() - interval '1 day'",
            ),
            (
                Postgres,
                "WITH x AS (SELECT 1) UPDATE t SET v = 1 WHERE id = 5",
            ),
            (
                Mysql,
                "INSERT INTO t (a) VALUES (1) ON DUPLICATE KEY UPDATE a = 2",
            ),
            (Mysql, "SELECT * FROM t FOR UPDATE"),
            (Mysql, "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END"),
            (Mysql, "SET NAMES utf8mb4"),
            (Postgres, "COPY t TO STDOUT"),
            (Postgres, "EXPLAIN DELETE FROM t"),
            (Mysql, "DELETE FROM t WHERE name = 'a' -- note"),
            (Mysql, "UPDATE t SET v = 'x' WHERE a = 'a' AND b = 'b'"),
        ] {
            assert_eq!(server_dangerous_reason(sql, d), None, "{sql}");
        }
    }
    #[test]
    fn deep_nesting_does_not_overflow() {
        let sql = format!(
            "DELETE FROM t WHERE {}1{}",
            "(".repeat(5000),
            ")".repeat(5000)
        );
        assert!(server_dangerous_reason(&sql, Mysql).is_some());
    }
    #[test]
    fn code_ranges_skip_quotes_and_comments() {
        let sql = "UPDATE `t?` SET a = ? WHERE b = '?' -- ?\n AND c = ?";
        let code: String = code_ranges(sql, Mysql)
            .into_iter()
            .map(|r| &sql[r])
            .collect();
        assert_eq!(code.matches('?').count(), 2, "{code}");
        let pg = "SELECT $1, '$2', \"$3\", $$ $4 $$, /* $5 */ $6";
        let code: String = code_ranges(pg, Postgres)
            .into_iter()
            .map(|r| &pg[r])
            .collect();
        assert!(
            code.contains("$1")
                && code.contains("$6")
                && !code.contains("$2")
                && !code.contains("$4")
                && !code.contains("$5"),
            "{code}"
        );
    }
}
