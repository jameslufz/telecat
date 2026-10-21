//! Electron's SQL lexer. Positions are UTF-16 code units, including surrogate pairs.
use crate::types::Driver;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

pub(crate) fn re(pattern: &str) -> Arc<Regex> {
    static CACHE: LazyLock<Mutex<HashMap<String, Arc<Regex>>>> = LazyLock::new(Default::default);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(pattern.into())
        .or_insert_with(|| Arc::new(Regex::new(&js_pattern(pattern)).expect("static regex")))
        .clone()
}
fn js_pattern(pattern: &str) -> String {
    // JS's non-/u case folding does not turn ſ into s or K into k. Keep case
    // folding scoped to ASCII tokens; whitespace still includes ECMAScript's
    // Unicode space set. JS dot also excludes CR, LS, and PS without /s.
    let mut p = pattern;
    let mut insensitive = false;
    let mut dotall = false;
    for (prefix, i, s) in [
        ("(?is-u)", true, true),
        ("(?i-u)", true, false),
        ("(?is)", true, true),
        ("(?i)", true, false),
        ("(?s)", false, true),
        ("(?-u)", false, false),
    ] {
        if let Some(rest) = p.strip_prefix(prefix) {
            p = rest;
            insensitive = i;
            dotall = s;
            break;
        }
    }
    let mut expanded = if dotall {
        "(?s)".to_owned()
    } else {
        String::new()
    };
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            expanded.push(c);
            if let Some(next) = chars.next() {
                expanded.push(next);
            }
        } else if c == '[' {
            let mut class = String::from("[");
            let mut escaped = false;
            for x in chars.by_ref() {
                class.push(x);
                if x == ']' && !escaped {
                    break;
                }
                if x == '\\' && !escaped {
                    escaped = true;
                } else {
                    escaped = false;
                }
            }
            // The only one-case alphabetic range in the reference patterns.
            if insensitive {
                class = class.replace("a-f", "a-fA-F");
            }
            expanded.push_str(&class);
        } else if insensitive && c.is_ascii_alphabetic() {
            let mut word = c.to_string();
            while chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                word.push(chars.next().unwrap());
            }
            expanded.push_str(&format!("(?i-u:{word})"));
        } else if c == '.' && !dotall {
            expanded.push_str(r"[^\n\r\u{2028}\u{2029}]");
        } else {
            expanded.push(c);
        }
    }
    let whitespace = r"\t\n\x0B\x0C\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
    expanded
        .replace(r"[\s\S]", "(?s:.)")
        .replace(r"\b", r"(?-u:\b)")
        .replace(r"\d", "[0-9]")
        .replace(r"\w", "[A-Za-z0-9_]")
        .replace(r"\s", &format!("[{whitespace}]"))
        .replace(r"\S", &format!("[^{whitespace}]"))
}
pub(crate) fn ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
pub fn trim(s: &str) -> &str {
    s.trim_matches(ws)
}
fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn string(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}
fn ident(c: u16) -> bool {
    c < 128 && ((c as u8).is_ascii_alphanumeric() || c == 95 || c == 36)
}
fn alpha(c: u16) -> bool {
    c < 128 && (c as u8).is_ascii_alphabetic()
}
fn space(c: u16) -> bool {
    char::from_u32(c as u32).is_some_and(ws)
}
fn at(s: &[u16], i: usize) -> u16 {
    s.get(i).copied().unwrap_or(0)
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Comment,
    Hint,
    String,
}
fn skip(s: &[u16], i: usize, d: Driver) -> Option<(usize, Kind)> {
    let c = at(s, i);
    let n = at(s, i + 1);
    let len = s.len();
    if (c == 45 && n == 45) || (c == 35 && d == Driver::Mysql) {
        return Some((
            (i..len).find(|&j| s[j] == 10).map_or(len, |j| j + 1),
            Kind::Comment,
        ));
    }
    if c == 47 && n == 42 {
        if d == Driver::Postgres {
            let mut depth = 0;
            let mut j = i;
            while j < len {
                if at(s, j) == 47 && at(s, j + 1) == 42 {
                    depth += 1;
                    j += 2;
                } else if at(s, j) == 42 && at(s, j + 1) == 47 {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        return Some((j, Kind::Comment));
                    }
                } else {
                    j += 1;
                }
            }
            return Some((len, Kind::Comment));
        }
        return Some((
            (i + 2..len)
                .find(|&j| at(s, j) == 42 && at(s, j + 1) == 47)
                .map_or(len, |j| j + 2),
            if matches!(at(s, i + 2), 33 | 43) {
                Kind::Hint
            } else {
                Kind::Comment
            },
        ));
    }
    if c == 39 || c == 34 || (c == 96 && d == Driver::Mysql) {
        let es = d == Driver::Postgres
            && c == 39
            && i > 0
            && matches!(s[i - 1], 69 | 101)
            && (i < 2 || !ident(s[i - 2]));
        let backslash = c != 96 && (d == Driver::Mysql || es);
        let mut j = i + 1;
        while j < len {
            if backslash && s[j] == 92 {
                j += 2;
                continue;
            }
            if s[j] == c {
                if at(s, j + 1) == c {
                    j += 2;
                    continue;
                }
                return Some((j + 1, Kind::String));
            }
            j += 1;
        }
        return Some((len, Kind::String));
    }
    if c == 36 && d == Driver::Postgres && (i == 0 || !ident(s[i - 1])) {
        let chunk = string(&s[i..len.min(i + 64)]);
        if let Some(m) = re(r"^\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$").find(&chunk) {
            let tag = units(m.as_str());
            let start = i + tag.len();
            let end = s[start..]
                .windows(tag.len())
                .position(|v| v == tag)
                .map_or(len, |j| start + j + tag.len());
            return Some((end, Kind::String));
        }
    }
    None
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    pub text: String,
    pub from: usize,
    pub to: usize,
}
pub fn strip_comments(sql: &str, d: Driver) -> String {
    let s = units(sql);
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if let Some((end, k)) = skip(&s, i, d) {
            if k != Kind::Comment {
                out.extend_from_slice(&s[i..end]);
            } else {
                out.push(if s[i] == 47 { 32 } else { 10 });
            }
            i = end;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    string(&out)
}
fn push(out: &mut Vec<Statement>, s: &[u16], start: usize, end: usize, d: Driver) {
    let raw = string(&s[start..end]);
    if !trim(&strip_comments(&raw, d)).is_empty() {
        let lead = raw
            .chars()
            .take_while(|c| ws(*c))
            .map(char::len_utf16)
            .sum::<usize>();
        let len = raw.trim_end_matches(ws).encode_utf16().count();
        out.push(Statement {
            text: trim(&raw).into(),
            from: start + lead,
            to: start + len,
        });
    }
}
pub fn split_statements(sql: &str, d: Driver) -> Vec<Statement> {
    let s = units(sql);
    let n = s.len();
    let mut out = vec![];
    let mut start = 0;
    let mut i = 0;
    let mut delimiter = vec![59];
    let mut words = vec![];
    let mut program = false;
    let mut depth: usize = 0;
    let mut skip_next = false;
    let mut prev = String::new();
    while i < n {
        if d == Driver::Mysql && (i == 0 || s[i - 1] == 10) {
            let chunk = string(&s[i..n.min(i + 200)]);
            if let Some(m) = re(r"(?i)^[ \t]*DELIMITER[ \t]+(\S+)[ \t]*(\r?\n|$)").captures(&chunk)
            {
                push(&mut out, &s, start, i, d);
                words.clear();
                program = false;
                depth = 0;
                skip_next = false;
                prev.clear();
                delimiter = units(&m[1]);
                i += m[0].encode_utf16().count();
                start = i;
                continue;
            }
        }
        if let Some((end, _)) = skip(&s, i, d) {
            i = end;
            continue;
        }
        let c = s[i];
        if (alpha(c) || c == 95) && (i == 0 || !ident(s[i - 1])) {
            let mut j = i + 1;
            while j < n && ident(s[j]) {
                j += 1;
            }
            let w = string(&s[i..j]).to_ascii_uppercase();
            if words.len() < 12 {
                words.push(w.clone());
                if d == Driver::Mysql
                    && words[0] == "CREATE"
                    && ["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"].contains(&w.as_str())
                {
                    program = true;
                }
            }
            if program {
                if skip_next {
                    skip_next = false;
                } else if w == "END" {
                    depth = depth.saturating_sub(1);
                    let chunk = string(&s[j..n.min(j + 20)]);
                    if let Some(a) = re(r"^\s*([A-Za-z]+)").captures(&chunk) {
                        if ["IF", "CASE", "LOOP", "WHILE", "REPEAT"]
                            .contains(&a[1].to_ascii_uppercase().as_str())
                        {
                            skip_next = true;
                        }
                    }
                } else if ["BEGIN", "CASE", "IF", "LOOP", "WHILE", "REPEAT"].contains(&w.as_str())
                    && (w != "IF"
                        || [
                            "", ";", ":", "BEGIN", "THEN", "ELSE", "DO", "LOOP", "REPEAT",
                        ]
                        .contains(&prev.as_str()))
                {
                    depth += 1;
                }
            }
            prev = w;
            i = j;
            continue;
        }
        if !space(c) {
            prev = string(&[c]);
        }
        if s[i..].starts_with(&delimiter) && (delimiter != [59] || depth == 0) {
            push(&mut out, &s, start, i, d);
            words.clear();
            program = false;
            depth = 0;
            skip_next = false;
            prev.clear();
            i += delimiter.len();
            start = i;
            continue;
        }
        i += 1;
    }
    push(&mut out, &s, start, n, d);
    out
}
fn code_only(sql: &str, d: Driver) -> String {
    // Stack frames are ranges into one UTF-16 buffer. Nested executable comments
    // use neither recursive Rust calls nor quadratic retained string copies.
    let source = units(sql);
    // Where the next `*/` starts, for every position. Searching for it afresh for each nested,
    // unterminated `/*!` made long inputs quadratic.
    let mut next_close = vec![source.len(); source.len() + 1];
    for j in (0..source.len().saturating_sub(1)).rev() {
        next_close[j] = if source[j] == 42 && source[j + 1] == 47 {
            j
        } else {
            next_close[j + 1]
        };
    }
    let mut stack = vec![(0usize, source.len(), 0usize)];
    let mut out = Vec::new();
    while let Some((base, limit, i)) = stack.last_mut() {
        let s = &source[*base..*limit];
        if *i >= s.len() {
            stack.pop();
            if !stack.is_empty() {
                out.push(32);
            }
            continue;
        }
        let mysql_comment =
            (d == Driver::Mysql && at(s, *i) == 47 && at(s, *i + 1) == 42).then(|| {
                // Same result as `skip`: the first `*/` inside this frame, else the frame's end.
                let close = next_close[*base + *i + 2];
                let end = if close + 2 <= *limit {
                    close + 2 - *base
                } else {
                    s.len()
                };
                let kind = if matches!(at(s, *i + 2), 33 | 43) {
                    Kind::Hint
                } else {
                    Kind::Comment
                };
                (end, kind)
            });
        if let Some((end, k)) = mysql_comment.or_else(|| skip(s, *i, d)) {
            let start = *i;
            *i = end;
            if k == Kind::Hint && at(s, start + 2) == 33 {
                let mut inner_start = *base + start + 3;
                let inner_end = *base + (start + 3).max(end.saturating_sub(2));
                let digits = source[inner_start..inner_end]
                    .iter()
                    .take(6)
                    .take_while(|c| **c >= 48 && **c <= 57)
                    .count();
                if digits >= 5 {
                    inner_start += digits;
                }
                out.push(32);
                stack.push((inner_start, inner_end, 0));
            } else {
                out.extend(units(if k == Kind::String {
                    if matches!(s[start], 96 | 34) {
                        " x "
                    } else {
                        " '' "
                    }
                } else {
                    " "
                }));
            }
        } else {
            out.push(s[*i]);
            *i += 1;
        }
    }
    string(&out)
}
fn top_level(code: &str) -> String {
    let mut out = String::new();
    let mut depth: usize = 0;
    for c in code.chars() {
        if c == '(' {
            if depth == 0 {
                out.push_str(" ( ) ");
            }
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}
pub fn statement_at(sql: &str, pos: usize, d: Driver) -> Option<Statement> {
    let stmts = split_statements(sql, d);
    for s in &stmts {
        if pos >= s.from && pos <= s.to + 1 {
            return Some(s.clone());
        }
    }
    let mut best = stmts.first().cloned();
    for s in stmts {
        if s.to <= pos {
            best = Some(s);
        }
    }
    best
}
pub fn first_keyword(sql: &str, d: Driver) -> String {
    re(r"^[\s(]*([A-Za-z]+)")
        .captures(&code_only(sql, d))
        .map_or(String::new(), |m| m[1].to_ascii_uppercase())
}
/// False when the server could write. Stricter than Electron's classification.
pub fn is_read_only(sql: &str, d: Driver) -> bool {
    legacy_is_read_only(sql, d) && crate::guard::server_read_only(sql, d)
}
fn legacy_is_read_only(sql: &str, d: Driver) -> bool {
    let code = code_only(sql, d);
    let kw = first_keyword(sql, d);
    let write = re(
        r"(?i-u)\b(INSERT|UPDATE|DELETE|MERGE|REPLACE|UPSERT|CREATE|DROP|ALTER|TRUNCATE|GRANT|REVOKE|CALL|DO|COPY)\b",
    );
    match kw.as_str() {
        "SHOW" | "DESC" | "DESCRIBE" => true,
        "EXPLAIN" => {
            !(re(r"(?i)^[\s]*EXPLAIN\s*(\([^)]*\bANALY[SZ]E\b[^)]*\)|ANALY[SZ]E\b)")
                .is_match(&code)
                && write.is_match(&code))
        }
        "SELECT" | "WITH" | "TABLE" | "VALUES" => {
            !write.is_match(&re(r"(?i-u)\b(REPLACE|CREATE|DO)\s*\(").replace_all(&code, ""))
                && !re(r"(?i-u)\bINTO\b").is_match(&code)
        }
        _ => false,
    }
}
/// Why the statement needs explicit confirmation. Electron's reasons come first and keep their
/// wording; the server-accurate checks add what it misses.
pub fn dangerous_reason(sql: &str, d: Driver) -> Option<String> {
    legacy_dangerous_reason(sql, d).or_else(|| crate::guard::server_dangerous_reason(sql, d))
}
/// A reason to confirm that only applies outside Silent mode: routine statements that may still
/// change data or session behaviour, such as `CALL` or `SET autocommit = 0`.
pub fn caution_reason(sql: &str, d: Driver) -> Option<String> {
    crate::guard::server_caution_reason(sql, d)
}
fn legacy_dangerous_reason(sql: &str, d: Driver) -> Option<String> {
    let raw = code_only(sql, d);
    let mut code = re(r";+\s*$").replace(trim(&raw), "").into_owned();
    let mut kw = first_keyword(sql, d);
    if kw == "EXPLAIN" {
        if let Some(ex) = re(r"(?i)^\s*EXPLAIN\s*(\([^)]*\)|ANALY[SZ]E\b|\s)*").find(&code) {
            if re(r"(?i)ANALY[SZ]E").is_match(ex.as_str()) {
                code = code[ex.end()..].into();
                kw = re(r"^\s*([A-Za-z]+)")
                    .captures(&code)
                    .map_or(String::new(), |m| m[1].to_ascii_uppercase());
            }
        }
    }
    if kw == "TRUNCATE" {
        return Some("TRUNCATE removes every row of the table".into());
    }
    if kw == "DROP" {
        return Some("DROP permanently removes the object and its data".into());
    }
    if kw == "ALTER" && re(r"(?i-u)\bDROP\b").is_match(&code) {
        return Some("ALTER … DROP removes a column, index or constraint".into());
    }
    if kw == "DELETE"
        || kw == "UPDATE"
        || (kw == "WITH" && re(r"(?i-u)\b(DELETE|UPDATE)\b").is_match(&code))
    {
        let top = top_level(&code);
        let what = if kw == "UPDATE" || re(r"(?i-u)\bUPDATE\b").is_match(&top) {
            "UPDATE"
        } else {
            "DELETE"
        };
        if let Some(w) = re(r"(?i-u)\bWHERE\b([\s\S]*)$").captures(&top) {
            let cond = re(r"(?i-u)\b(ORDER\s+BY|LIMIT|RETURNING)\b[\s\S]*$").replace(&w[1], "");
            if re(r"(?i)^(1|TRUE|1\s*=\s*1|''\s*=\s*''|NOT\s+FALSE|1\s*<>\s*0)$")
                .is_match(trim(&cond))
            {
                return Some(format!(
                    "{what} with an always-true WHERE affects every row of the table"
                ));
            }
        } else {
            return Some(format!(
                "{what} without WHERE affects every row of the table"
            ));
        }
    }
    None
}
pub fn apply_limit(sql: &str, limit: Option<i64>, d: Driver) -> String {
    let Some(limit) = limit.filter(|n| *n != 0) else {
        return sql.into();
    };
    let kw = first_keyword(sql, d);
    if !["SELECT", "WITH"].contains(&kw.as_str()) || !legacy_is_read_only(sql, d) {
        return sql.into();
    }
    let body = strip_comments(sql, d);
    let body = re(r";+\s*$").replace(trim(&body), "");
    if re(r"(?i-u)\blimit\s+\d+(\s*(,|offset)\s*\d+)?\s*$").is_match(&body)
        || re(r"(?i-u)\bfor\s+(update|share)\b[\s\S]*$|\block\s+in\s+share\s+mode\s*$")
            .is_match(&body)
    {
        return sql.into();
    }
    format!(
        "{}\nLIMIT {limit}",
        sql.trim_end_matches(|c| ws(c) || c == ';')
    )
}
pub fn uglify(sql: &str, d: Driver) -> String {
    let s = units(sql);
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if let Some((end, k)) = skip(&s, i, d) {
            if k == Kind::Comment {
                if !out.is_empty() && out.last() != Some(&32) {
                    out.push(32);
                }
            } else {
                out.extend_from_slice(&s[i..end]);
            }
            i = end;
            continue;
        }
        let c = s[i];
        if space(c) {
            if !out.is_empty() && out.last() != Some(&32) {
                out.push(32);
            }
            i += 1;
        } else {
            if matches!(c, 44 | 59 | 41) && out.last() == Some(&32) {
                out.pop();
            }
            out.push(c);
            if c == 40 {
                while i + 1 < s.len() && space(s[i + 1]) {
                    i += 1;
                }
            }
            i += 1;
        }
    }
    trim(&string(&out)).into()
}
