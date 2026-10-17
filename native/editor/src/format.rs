//! A native formatter for common MySQL/PostgreSQL queries.
//! Dialect vocabularies come from sql-formatter 15.9.0; the parity ledger records
//! differences in layouts unsupported by this compact implementation.
use crate::syntax::{Kind, lex};
use std::{collections::HashSet, sync::OnceLock};
use telecat_core::types::Driver;
struct Rules {
    keywords: HashSet<String>,
    functions: HashSet<String>,
    types: HashSet<String>,
    phrases: Vec<Vec<String>>,
    clauses: HashSet<String>,
    oneline: HashSet<String>,
    select: HashSet<String>,
    set_operations: HashSet<String>,
}
fn rules(driver: Driver) -> &'static Rules {
    static MY: OnceLock<Rules> = OnceLock::new();
    static PG: OnceLock<Rules> = OnceLock::new();
    let (slot, data) = if driver == Driver::Mysql {
        (&MY, include_str!("formatter-mysql.json"))
    } else {
        (&PG, include_str!("formatter-postgresql.json"))
    };
    slot.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(data).unwrap();
        let get = |k: &str| {
            v[k].as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default()
        };
        let mut phrases: Vec<Vec<String>> = [
            "select",
            "clauses",
            "joins",
            "setOperations",
            "phrases",
            "functions",
        ]
        .iter()
        .flat_map(|k| get(k))
        .filter(|s| s.contains(' '))
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .collect();
        phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
        let mut keywords = get("keywords");
        for key in ["select", "clauses", "joins", "setOperations", "phrases"] {
            keywords.extend(get(key));
        }
        Rules {
            keywords,
            functions: get("functions"),
            types: get("types"),
            phrases,
            clauses: get("clauses"),
            oneline: get("oneline"),
            select: get("select"),
            set_operations: get("setOperations"),
        }
    })
}
#[derive(Clone, Debug)]
struct Token {
    text: String,
    kind: Kind,
    newline: bool,
}
impl Token {
    fn upper(&self) -> String {
        self.text.to_ascii_uppercase()
    }
}
fn tokens(sql: &str, driver: Driver) -> Vec<Token> {
    let rules = rules(driver);
    let mut previous_end = 0;
    let mut raw: Vec<_> = lex(sql, driver)
        .into_iter()
        .filter(|t| t.kind != Kind::Space)
        .map(|t| {
            let newline = sql[previous_end..t.range.start].contains('\n');
            previous_end = t.range.end;
            Token {
                text: sql[t.range].into(),
                kind: t.kind,
                newline,
            }
        })
        .collect();
    let mut out = vec![];
    let mut i = 0;
    while i < raw.len() {
        let mut found = false;
        for phrase in &rules.phrases {
            if i + phrase.len() <= raw.len()
                && phrase.iter().enumerate().all(|(j, w)| {
                    !matches!(raw[i + j].kind, Kind::String | Kind::Comment)
                        && raw[i + j].upper() == *w
                })
            {
                let joined = phrase.join(" ");
                let preserve = rules.functions.contains(&joined);
                out.push(Token {
                    text: if preserve {
                        raw[i..i + phrase.len()]
                            .iter()
                            .map(|t| t.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        joined
                    },
                    kind: Kind::Keyword,
                    newline: raw[i].newline,
                });
                i += phrase.len();
                found = true;
                break;
            }
        }
        if found {
            continue;
        }
        let t = &raw[i];
        let upper = t.upper();
        let next = raw.get(i + 1).map(|t| t.text.as_str());
        let uppercase = (rules.keywords.contains(&upper)
            && !rules.types.contains(&upper)
            && (!rules.functions.contains(&upper) || rules.clauses.contains(&upper)))
            || (upper == "ARRAY" && next == Some("["));
        let mut t = t.clone();
        if uppercase && !matches!(t.kind, Kind::String | Kind::Comment) {
            t.text = upper;
        }
        // Prefix string literals are indivisible (E'...', N'...', X'...').
        if t.text == "$" && raw.get(i + 1).is_some_and(|t| t.kind == Kind::Number) {
            t.text.push_str(&raw[i + 1].text);
            i += 1;
        } else if matches!(t.upper().as_str(), "E" | "N" | "X" | "B")
            && raw.get(i + 1).is_some_and(|t| t.kind == Kind::String)
        {
            t.text.push_str(&raw[i + 1].text);
            t.kind = Kind::String;
            i += 1;
        } else if t.kind == Kind::Operator {
            for op in [
                "->>", "<=>", "::", "->", ">=", "<=", "<>", "!=", "||", ":=", "<<", ">>",
            ] {
                let n = op.len();
                if i + n <= raw.len() && (0..n).all(|j| raw[i + j].text == op[j..j + 1]) {
                    t.text = op.into();
                    i += n - 1;
                    break;
                }
            }
        }
        if t.kind == Kind::String
            && t.text.starts_with('_')
            && let Some(quote) = t.text.find('\'')
        {
            t.text.insert(quote, ' ');
        }
        out.push(t);
        i += 1;
    }
    raw.clear();
    out
}
struct Writer {
    lines: Vec<String>,
    line: String,
    level: usize,
}
impl Writer {
    fn new() -> Self {
        Self {
            lines: vec![],
            line: String::new(),
            level: 0,
        }
    }
    fn word(&mut self, s: &str) {
        if self.line.is_empty() {
            self.line = "  ".repeat(self.level);
        } else if !self.line.ends_with([' ', '(', '[', '.'])
            && !matches!(s, ")" | "]" | "," | ";" | "." | "::")
            && !self.line.ends_with("::")
        {
            self.line.push(' ');
        }
        self.line.push_str(s);
    }
    fn tight(&mut self, s: &str) {
        if self.line.is_empty() {
            self.line = "  ".repeat(self.level);
        }
        self.line.push_str(s);
    }
    fn nl(&mut self) {
        if !self.line.trim().is_empty() {
            self.lines.push(self.line.trim_end().to_string());
        }
        self.line.clear();
    }
    fn finish(mut self) -> String {
        self.nl();
        self.lines.join("\n")
    }
}
fn inline(ts: &[Token], rules: &Rules) -> String {
    let mut w = Writer::new();
    for (i, t) in ts.iter().enumerate() {
        match t.text.as_str() {
            "(" => {
                let prev = i.checked_sub(1).map(|i| ts[i].upper()).unwrap_or_default();
                if i > 0 && (rules.functions.contains(&prev) || rules.types.contains(&prev)) {
                    w.tight("(")
                } else {
                    w.word("(")
                }
            }
            "[" => w.tight("["),
            "," => {
                w.tight(",");
                w.tight(" ");
            }
            s => w.word(s),
        }
    }
    w.finish()
}
fn block(ts: &[Token], base: usize, w: &mut Writer, rules: &Rules) {
    let mut i = 0;
    let mut item = base;
    let mut clause = String::new();
    let mut between = false;
    let mut case_levels = vec![];
    w.level = base;
    while i < ts.len() {
        let t = &ts[i];
        let u = t.upper();
        if t.kind == Kind::Comment {
            if t.newline || t.text.starts_with("/*") {
                w.nl();
            }
            w.word(&t.text.replace('\n', &format!("\n{}", "  ".repeat(w.level))));
            w.nl();
            i += 1;
            continue;
        }
        if t.text == "(" || t.text == "[" {
            let close = if t.text == "(" { ")" } else { "]" };
            let mut depth = 1;
            let mut end = i + 1;
            while end < ts.len() {
                if ts[end].text == t.text {
                    depth += 1;
                } else if ts[end].text == close {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                end += 1;
            }
            let inner = &ts[i + 1..end];
            let flat = inline(inner, rules);
            let prev = i.checked_sub(1).map(|j| ts[j].upper()).unwrap_or_default();
            let function = i > 0
                && !matches!(prev.as_str(), "OVER" | "IN" | "NOT IN" | "AS" | "EXISTS")
                && (rules.functions.contains(&prev)
                    || rules.types.contains(&prev)
                    || matches!(ts[i - 1].kind, Kind::Name)
                        && !matches!(
                            clause.as_str(),
                            "INSERT INTO" | "REPLACE INTO" | "WITH" | "WITH RECURSIVE"
                        )
                        && !clause.starts_with("CREATE TABLE")
                        && !ts[..i].iter().any(|t| t.upper().ends_with("INDEX"))
                        && i.checked_sub(2)
                            .is_none_or(|j| ts[j].upper() != "REFERENCES"));
            if function || t.text == "[" {
                w.tight(&t.text);
            } else {
                w.word(&t.text);
            }
            let multiline = inner.iter().any(|t| {
                rules.select.contains(&t.upper())
                    || rules.clauses.contains(&t.upper())
                    || matches!(
                        t.upper().as_str(),
                        "WITH" | "AND" | "OR" | "PARTITION BY" | "ORDER BY"
                    )
            }) || flat.chars().count() > 50;
            if multiline {
                w.nl();
                block(inner, item + 1, w, rules);
                w.nl();
                w.level = item;
                w.word(close);
            } else {
                w.tight(&flat);
                if end < ts.len() {
                    w.tight(close);
                }
            }
            i = if end < ts.len() { end + 1 } else { end };
            continue;
        }
        if rules.oneline.contains(&u) {
            w.nl();
            w.level = base;
            w.word(&t.text);
            clause = u;
            item = base;
        } else if rules.clauses.contains(&u) || rules.select.contains(&u) {
            w.nl();
            w.level = base;
            w.word(&t.text);
            if ts
                .get(i + 1)
                .is_none_or(|t| t.kind != Kind::Comment || t.newline || t.text.starts_with("/*"))
            {
                w.nl();
            }
            item = base + 1;
            w.level = item;
            clause = u;
            between = false;
        } else if rules.set_operations.contains(&u) {
            w.nl();
            w.level = base;
            w.word(&t.text);
            w.nl();
            item = base;
            w.level = item;
        } else if u == "CASE" {
            w.word("CASE");
            w.nl();
            case_levels.push(item);
            item += 1;
            w.level = item;
        } else if u == "WHEN" || u == "ELSE" {
            w.nl();
            w.level = item;
            w.word(&t.text);
        } else if u == "END" && !case_levels.is_empty() {
            w.nl();
            item = case_levels.pop().unwrap();
            w.level = item;
            w.word("END");
        } else if u == "BETWEEN" {
            between = true;
            w.word(&t.text);
        } else if (u == "AND" && !between) || u == "OR" || u.ends_with("JOIN") {
            w.nl();
            w.level = item;
            w.word(&t.text);
        } else if u == "AND" {
            between = false;
            w.word(&t.text);
        } else if t.text == "," {
            w.tight(",");
            if clause == "LIMIT" {
                w.tight(" ");
            } else {
                w.nl();
                w.level = item;
            }
        } else if t.text == ";" {
            w.tight(";");
            w.nl();
            if i + 1 < ts.len() {
                w.lines.push(String::new());
            }
            w.level = base;
            item = base;
        } else if (t.text == "-" || t.text == "+")
            && i + 1 < ts.len()
            && ts[i + 1].kind == Kind::Number
            && (i == 0 || matches!(ts[i - 1].text.as_str(), "," | "(" | "=" | "SELECT"))
        {
            w.word(&format!("{}{}", t.text, ts[i + 1].text));
            i += 1;
        } else {
            w.word(&t.text);
        }
        i += 1;
    }
}
pub fn beautify(sql: &str, driver: Driver) -> String {
    let ts = tokens(sql, driver);
    let mut w = Writer::new();
    block(&ts, 0, &mut w, rules(driver));
    w.finish()
}
pub fn uglify(sql: &str, driver: Driver) -> String {
    telecat_core::sql::uglify(sql, driver)
}
