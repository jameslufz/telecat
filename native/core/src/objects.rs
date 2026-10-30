//! Functions, procedures and triggers: checking edited definitions and the SQL that saves them.
//!
//! The definitions are typed by the user and run as DDL through ⌘S, so every check here fails
//! closed: one statement only, of the expected kind, for the expected object.
use crate::{
    changes::{Stmt, qualified, quote_ident},
    guard::single_statement,
    sql::{split_statements, strip_comments, trim},
    types::*,
};
use anyhow::{Result, bail};
use regex::Regex;
use std::sync::LazyLock;

const MYSQL_IDENT: &str = r"`(?:``|[^`])+`|[0-9A-Za-z_$\x{80}-\x{10FFFF}]+";
const PG_IDENT: &str =
    r#""(?:""|[^"])+"|[A-Za-z_\x{80}-\x{10FFFF}][0-9A-Za-z_$\x{80}-\x{10FFFF}]*"#;

/// What a CREATE FUNCTION / PROCEDURE statement creates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineHeader {
    pub kind: ObjectKind,
    pub schema: Option<String>,
    pub name: String,
    /// PostgreSQL: whether it says CREATE OR REPLACE.
    pub or_replace: bool,
    /// PostgreSQL: the argument list as written, whitespace collapsed.
    pub args: Option<String>,
}
fn unquote(d: Driver, raw: &str) -> String {
    let q = if d == Driver::Mysql { '`' } else { '"' };
    if raw.len() >= 2 && raw.starts_with(q) && raw.ends_with(q) {
        raw[1..raw.len() - 1].replace(&format!("{q}{q}"), &q.to_string())
    } else if d == Driver::Postgres {
        raw.to_lowercase()
    } else {
        raw.to_owned()
    }
}
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// The text between the parenthesis at `open` and its match, skipping quoted text.
fn balanced(s: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, c) in s[open..].char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}
pub fn parse_routine_header(d: Driver, definition: &str) -> Result<RoutineHeader> {
    static MYSQL: LazyLock<Regex> = LazyLock::new(|| {
        let account = r#"(?:`(?:``|[^`])*`|'(?:''|\\.|[^'\\])*'|"(?:""|[^"])*"|[^\s@'"`]+)"#;
        Regex::new(&format!(
            r"(?is)^\s*CREATE\s+(?:DEFINER\s*=\s*(?:CURRENT_USER(?:\s*\(\s*\))?|{account}(?:\s*@\s*{account})?)\s+)?(?:AGGREGATE\s+)?(FUNCTION|PROCEDURE)\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:({MYSQL_IDENT})\s*\.\s*)?({MYSQL_IDENT})"
        ))
        .unwrap()
    });
    static PG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"(?is)^\s*CREATE\s+(OR\s+REPLACE\s+)?(FUNCTION|PROCEDURE)\s+(?:({PG_IDENT})\s*\.\s*)?({PG_IDENT})\s*\("
        ))
        .unwrap()
    });
    let code = strip_comments(definition, d);
    let (m, kind_at, schema_at, name_at) = if d == Driver::Mysql {
        (MYSQL.captures(&code), 1, 2, 3)
    } else {
        (PG.captures(&code), 2, 3, 4)
    };
    let Some(m) = m else {
        bail!(
            "The definition must start with CREATE {}FUNCTION or CREATE {0}PROCEDURE.",
            if d == Driver::Postgres {
                "OR REPLACE "
            } else {
                ""
            }
        );
    };
    let kind = if m[kind_at].eq_ignore_ascii_case("FUNCTION") {
        ObjectKind::Function
    } else {
        ObjectKind::Procedure
    };
    let args = if d == Driver::Postgres {
        let open = m.get(0).unwrap().end() - 1;
        Some(collapse(balanced(&code, open).ok_or_else(|| {
            anyhow::anyhow!("The argument list isn't closed.")
        })?))
    } else {
        None
    };
    Ok(RoutineHeader {
        kind,
        schema: m.get(schema_at).map(|s| unquote(d, s.as_str())),
        name: unquote(d, &m[name_at]),
        or_replace: d == Driver::Postgres && m.get(1).is_some(),
        args,
    })
}
/// `text` as one statement without its trailing semicolon, or why it isn't one.
fn one_statement(d: Driver, text: &str, what: &str) -> Result<String> {
    if d == Driver::Mysql
        && strip_comments(text, d).lines().any(|l| {
            l.trim_start()
                .to_ascii_uppercase()
                .starts_with("DELIMITER ")
        })
    {
        bail!(
            "Remove the DELIMITER lines: Telecat sends the {what} to the server as one statement."
        );
    }
    let statements = split_statements(text, d);
    if statements.is_empty() {
        bail!("The {what} is empty.");
    }
    if statements.len() != 1 || !single_statement(text, d) {
        bail!(
            "The {what} must be a single statement. Run anything else in a SQL Query tab instead."
        );
    }
    Ok(statements[0]
        .text
        .trim_end_matches(';')
        .trim_end()
        .to_owned())
}
fn same_name(d: Driver, a: &str, b: &str) -> bool {
    if d == Driver::Mysql {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}
/// The session settings MySQL stored with a routine or trigger (SHOW CREATE): the object runs
/// under them, so Telecat creates it again under the same ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CreateSettings {
    pub sql_mode: Option<String>,
    pub collation_connection: Option<String>,
}
impl CreateSettings {
    /// As session settings for the save (`execute_after_review_with`).
    pub fn session(&self) -> Vec<(String, String)> {
        [
            ("sql_mode", &self.sql_mode),
            ("collation_connection", &self.collation_connection),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.clone().map(|v| (k.to_owned(), v)))
        .collect()
    }
}
/// What saving an edited routine or trigger runs, and on MySQL, what puts the old one back when
/// the new definition fails after the old one was dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSave {
    pub statements: Vec<Stmt>,
    pub restore: Vec<Stmt>,
    pub transaction: bool,
    /// MySQL: the session settings both the statements and the restore run under.
    pub settings: Vec<(String, String)>,
}
/// Where the routine's name is in a MySQL CREATE statement (comments allowed in the header), so
/// the definition can be tried out under another name first.
fn mysql_name_span(definition: &str) -> Option<std::ops::Range<usize>> {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        let ws = r"(?:\s|/\*(?s:.*?)\*/|--[^\n]*(?:\n|$)|#[^\n]*(?:\n|$))";
        let account = r#"(?:`(?:``|[^`])*`|'(?:''|\\.|[^'\\])*'|"(?:""|[^"])*"|[^\s@'"`]+)"#;
        Regex::new(&format!(
            r"(?i)^{ws}*CREATE{ws}+(?:DEFINER{ws}*={ws}*(?:CURRENT_USER(?:{ws}*\({ws}*\))?|{account}(?:{ws}*@{ws}*{account})?){ws}+)?(?:AGGREGATE{ws}+)?(?:FUNCTION|PROCEDURE){ws}+(?:IF{ws}+NOT{ws}+EXISTS{ws}+)?(?:(?:{MYSQL_IDENT}){ws}*\.{ws}*)?({MYSQL_IDENT})"
        ))
        .unwrap()
    });
    HEADER.captures(definition)?.get(1).map(|m| m.range())
}
/// A name no routine has, for trying a definition out: unique per call, within MySQL's 64.
fn check_name(name: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let suffix = format!(
        "__telecat_check_{:x}",
        stamp ^ (n << 48) ^ u64::from(std::process::id())
    );
    let keep: String = name.chars().take(64 - suffix.len()).collect();
    format!("{keep}{suffix}")
}
/// Saving the edited definition of `object`. MySQL can't replace a routine, so it is dropped
/// and created again; PostgreSQL replaces it in place, in a transaction.
pub fn routine_save(
    d: Driver,
    object: &DbObject,
    original: &str,
    definition: &str,
    settings: &CreateSettings,
) -> Result<ObjectSave> {
    if !object.kind.is_routine() {
        bail!("Only functions and procedures are saved this way");
    }
    let kind = object.kind.label();
    let sql = one_statement(d, definition, &format!("{kind} definition"))?;
    let header = parse_routine_header(d, &sql)?;
    if header.kind != object.kind {
        bail!(
            "This tab edits the {kind} {}: the definition must create a {kind}, not a {}.",
            object.table.name,
            header.kind.label()
        );
    }
    let schema_ok = header
        .schema
        .as_deref()
        .is_none_or(|s| same_name(d, s, &object.table.schema));
    if !schema_ok || !same_name(d, &header.name, &object.table.name) {
        bail!(
            "This tab edits the {kind} {}. To create a {kind} with another name, run the CREATE statement in a SQL Query tab.",
            object.table.name
        );
    }
    if d == Driver::Postgres {
        if !header.or_replace {
            bail!("Keep CREATE OR REPLACE: the {kind} already exists.");
        }
        let before = parse_routine_header(d, original).ok().and_then(|h| h.args);
        if before.is_some_and(|b| !b.eq_ignore_ascii_case(header.args.as_deref().unwrap_or(""))) {
            bail!(
                "The arguments changed. PostgreSQL would add a second {kind} {} next to this one instead of replacing it. Keep the arguments, or create the new {kind} in a SQL Query tab.",
                object.table.name
            );
        }
        return Ok(ObjectSave {
            statements: vec![Stmt::new(format!("{sql};"))],
            restore: vec![],
            transaction: true,
            settings: vec![],
        });
    }
    let keyword = if object.kind == ObjectKind::Function {
        "FUNCTION"
    } else {
        "PROCEDURE"
    };
    // Without the original, a failed save couldn't be undone: refuse it.
    let restore = one_statement(d, original, "original definition").map_err(|e| {
        anyhow::anyhow!(
            "Telecat can't read back the {kind}'s current definition, so it won't replace it: {e}"
        )
    })?;
    // MySQL can't replace a routine in place. Before the original is dropped, the new definition
    // is created under a temporary name and dropped again: anything the server would refuse
    // (syntax, DEFINER privileges, binary-log safety) is refused while nothing has changed.
    let span = mysql_name_span(&sql)
        .ok_or_else(|| anyhow::anyhow!("Telecat can't find the {kind}'s name in the definition"))?;
    let temp = TableRef {
        schema: object.table.schema.clone(),
        name: check_name(&object.table.name),
    };
    let named_temp = format!(
        "{}{}{}",
        &sql[..span.start],
        quote_ident(d, &temp.name),
        &sql[span.end..]
    );
    // A schema written in the header stays: the temporary routine lands next to the original.
    Ok(ObjectSave {
        statements: vec![
            Stmt::new(format!("{named_temp};")),
            Stmt::new(format!("DROP {keyword} {};", qualified(d, &temp))),
            Stmt::new(format!(
                "DROP {keyword} IF EXISTS {};",
                qualified(d, &object.table)
            )),
            Stmt::new(format!("{sql};")),
        ],
        restore: vec![Stmt::new(format!("{restore};"))],
        transaction: false,
        settings: settings.session(),
    })
}

/// A table trigger as the Triggers sheet shows it. `statement` is MySQL's trigger body, and for
/// PostgreSQL everything after `ON <table>` (`FOR EACH ROW EXECUTE FUNCTION f()`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Trigger {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub statement: String,
    /// MySQL: the account it runs as (`user@host`, as information_schema lists it). Kept when
    /// it is created again.
    pub definer: Option<String>,
    /// MySQL: the settings it was created under, used again when it is created again.
    pub settings: CreateSettings,
    /// MySQL: its place (ACTION_ORDER) among the table's triggers with the same timing and
    /// event, kept with FOLLOWS / PRECEDES when it is created again.
    pub order: Option<u32>,
    /// MySQL: the triggers of the same timing and event just before and after it, when it was
    /// read. Used to put it back in place when the save doesn't list those triggers.
    pub neighbours: (Option<String>, Option<String>),
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TriggerEdit {
    pub key: String,
    pub orig: Option<Trigger>,
    pub cur: Trigger,
    pub deleted: bool,
}
impl TriggerEdit {
    pub fn changed(&self) -> bool {
        match &self.orig {
            None => !self.deleted,
            Some(o) => self.deleted || *o != self.cur,
        }
    }
}
/// When a table trigger can run. (PostgreSQL's INSTEAD OF is for views, which have no
/// Triggers sheet.)
pub fn trigger_timings(_: Driver) -> &'static [&'static str] {
    &["BEFORE", "AFTER"]
}
pub fn trigger_events(d: Driver) -> &'static [&'static str] {
    if d == Driver::Mysql {
        &["INSERT", "UPDATE", "DELETE"]
    } else {
        &["INSERT", "UPDATE", "DELETE", "TRUNCATE"]
    }
}
/// A new trigger's starting values: a name not taken yet and a statement to fill in.
pub fn new_trigger(d: Driver, table: &str, taken: &[&str]) -> Trigger {
    let mut n = 1;
    let name = loop {
        let name = format!("{table}_trigger_{n}");
        if !taken.iter().any(|t| same_name(d, t, &name)) {
            break name;
        }
        n += 1;
    };
    Trigger {
        name,
        timing: "BEFORE".into(),
        event: "INSERT".into(),
        statement: if d == Driver::Mysql {
            "BEGIN\n\nEND".into()
        } else {
            "FOR EACH ROW EXECUTE FUNCTION function_name()".into()
        },
        ..Default::default()
    }
}
fn check_trigger(d: Driver, t: &Trigger) -> Result<()> {
    static PG_EVENTS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)^(INSERT|DELETE|TRUNCATE|UPDATE(\s+OF\s+[^;]+?)?)(\s+OR\s+(INSERT|DELETE|TRUNCATE|UPDATE(\s+OF\s+[^;]+?)?))*$").unwrap()
    });
    if trim(&t.name).is_empty() {
        bail!("A trigger needs a name");
    }
    let timing = t.timing.to_ascii_uppercase();
    if !trigger_timings(d).contains(&timing.as_str()) {
        bail!(
            "Trigger {}: choose when it runs ({})",
            t.name,
            trigger_timings(d).join(", ")
        );
    }
    let event = collapse(&t.event);
    let event_ok = if d == Driver::Mysql {
        trigger_events(d).contains(&event.to_ascii_uppercase().as_str())
    } else {
        PG_EVENTS.is_match(&event)
    };
    if !event_ok {
        bail!(
            "Trigger {}: choose the event ({})",
            t.name,
            trigger_events(d).join(", ")
        );
    }
    if trim(&t.statement).is_empty() {
        bail!("Trigger {}: the statement is empty", t.name);
    }
    Ok(())
}
/// Where a re-created MySQL trigger goes among the triggers of its timing and event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Follows(String),
    Precedes(String),
}
/// `user@host` as information_schema writes a definer, quoted for DEFINER = …
fn mysql_account(definer: &str) -> String {
    let d = Driver::Mysql;
    match definer.rsplit_once('@') {
        Some((user, host)) => format!("{}@{}", quote_ident(d, user), quote_ident(d, host)),
        None => quote_ident(d, definer),
    }
}
pub fn create_trigger_sql(d: Driver, table: &TableRef, t: &Trigger) -> Result<String> {
    create_trigger_placed(d, table, t, None)
}
fn create_trigger_placed(
    d: Driver,
    table: &TableRef,
    t: &Trigger,
    placement: Option<&Placement>,
) -> Result<String> {
    check_trigger(d, t)?;
    let statement = trim(&t.statement).trim_end_matches(';').trim_end();
    let sql = if d == Driver::Mysql {
        let definer = t
            .definer
            .as_deref()
            .map_or(String::new(), |a| format!("DEFINER={} ", mysql_account(a)));
        let placement = match placement {
            Some(Placement::Follows(n)) => format!("FOLLOWS {} ", quote_ident(d, n)),
            Some(Placement::Precedes(n)) => format!("PRECEDES {} ", quote_ident(d, n)),
            None => String::new(),
        };
        format!(
            "CREATE {definer}TRIGGER {} {} {} ON {} FOR EACH ROW {placement}{statement};",
            qualified(
                d,
                &TableRef {
                    schema: table.schema.clone(),
                    name: trim(&t.name).into()
                }
            ),
            t.timing.to_ascii_uppercase(),
            t.event.to_ascii_uppercase(),
            qualified(d, table)
        )
    } else {
        format!(
            "CREATE TRIGGER {} {} {} ON {} {statement};",
            quote_ident(d, trim(&t.name)),
            t.timing.to_ascii_uppercase(),
            collapse(&t.event),
            qualified(d, table)
        )
    };
    // The statement is pasted into the CREATE, so it must not end it and start another one.
    one_statement(d, &sql, &format!("trigger {}", t.name))?;
    Ok(sql)
}
/// Before a MySQL trigger is dropped to be created again: proves this connection may create it,
/// so a save the server would refuse is refused while the trigger is still there. Fails closed:
/// - as another account: SET_USER_ID (SET_ANY_DEFINER on 8.2+, or SUPER), and on MySQL 8.0.16+ also SYSTEM_USER, since
///   that account may hold it and Telecat can't reliably tell whether it does;
/// - with binary logging on: SUPER, unless log_bin_trust_function_creators is set.
fn create_check(t: &Trigger) -> crate::changes::Check {
    const HAS: &str = "EXISTS (SELECT 1 FROM information_schema.user_privileges WHERE grantee = CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''', SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''') AND privilege_type IN";
    // Servers without the SYSTEM_USER privilege: MariaDB, MySQL 5.x–7.x and 8.0.0–8.0.15. Any
    // other version string counts as having it.
    const OLD: &str = "(@@version LIKE '%MariaDB%' OR CAST(SUBSTRING_INDEX(@@version, '.', 1) AS UNSIGNED) BETWEEN 5 AND 7 OR (SUBSTRING_INDEX(@@version, '.', 2) = '8.0' AND CAST(SUBSTRING_INDEX(SUBSTRING_INDEX(SUBSTRING_INDEX(@@version, '-', 1), '.', 3), '.', -1) AS UNSIGNED) < 16))";
    let definer = t
        .definer
        .clone()
        .map_or(serde_json::Value::Null, serde_json::Value::String);
    let who = t.definer.as_deref().unwrap_or("the current account");
    let name = &t.name;
    crate::changes::Check {
        sql: format!(
            "SELECT CASE WHEN NOT (? IS NULL OR CURRENT_USER() = ? OR {HAS} ('SUPER', 'SET_USER_ID', 'SET_ANY_DEFINER'))) THEN 2 WHEN NOT (? IS NULL OR CURRENT_USER() = ? OR {OLD} OR {HAS} ('SYSTEM_USER'))) THEN 3 WHEN NOT (@@GLOBAL.log_bin = 0 OR @@GLOBAL.log_bin_trust_function_creators = 1 OR {HAS} ('SUPER'))) THEN 4 ELSE 1 END"
        ),
        params: vec![definer.clone(), definer.clone(), definer.clone(), definer],
        expect: 1,
        message: [
            format!("Trigger {name} wasn't changed: MySQL would refuse to create it again with this account."),
            format!("when 2: Trigger {name} wasn't changed. It runs as {who}, and creating it again as that account needs the SET_USER_ID privilege (SET_ANY_DEFINER on MySQL 8.2 and later, or SUPER), which this account doesn't have. Edit it while connected as {who}."),
            format!("when 3: Trigger {name} wasn't changed. It runs as {who}, and on this MySQL version creating it again as another account also needs the SYSTEM_USER privilege, which this account doesn't have. Edit it while connected as {who}, or with an account that has SYSTEM_USER."),
            format!("when 4: Trigger {name} wasn't changed. Binary logging is on, so creating a trigger needs the SUPER privilege (or log_bin_trust_function_creators = 1), which this account doesn't have."),
        ]
        .join("\n"),
    }
}
pub fn drop_trigger_sql(d: Driver, table: &TableRef, name: &str) -> String {
    if d == Driver::Mysql {
        format!(
            "DROP TRIGGER {};",
            qualified(
                d,
                &TableRef {
                    schema: table.schema.clone(),
                    name: name.into()
                }
            )
        )
    } else {
        format!(
            "DROP TRIGGER {} ON {};",
            quote_ident(d, name),
            qualified(d, table)
        )
    }
}
/// The SQL for the staged trigger edits of `table`, one save per changed trigger (with its edit's
/// key): drops first, then edits, then new triggers, so a name freed by a drop can be reused.
pub fn trigger_saves(
    d: Driver,
    table: &TableRef,
    relation: ObjectKind,
    edits: &[TriggerEdit],
) -> Result<Vec<(String, ObjectSave)>> {
    let changed: Vec<_> = edits.iter().filter(|e| e.changed()).collect();
    if changed.is_empty() {
        return Ok(vec![]);
    }
    if relation != ObjectKind::Table {
        bail!("Triggers can only be changed on tables");
    }
    let mysql = d == Driver::Mysql;
    // Edits of existing triggers go in their original order, so each one can be put back after
    // the one before it (which is already back) or before the one after it (not touched yet).
    let rank = |e: &TriggerEdit| {
        e.orig.as_ref().map(|o| {
            (
                o.timing.to_ascii_uppercase(),
                o.event.to_ascii_uppercase(),
                o.order.unwrap_or(u32::MAX),
            )
        })
    };
    let mut ordered = changed.clone();
    ordered.sort_by_key(|e| {
        (
            match (&e.orig, e.deleted) {
                (Some(_), true) => 0,
                (Some(_), false) => 1,
                (None, _) => 2,
            },
            rank(e),
        )
    });
    let same_place = |o: &Trigger, t: &Trigger| {
        o.timing.eq_ignore_ascii_case(&t.timing) && o.event.eq_ignore_ascii_case(&t.event)
    };
    // The existing triggers that stay where they are, in their order.
    let mut stay: Vec<&TriggerEdit> = edits
        .iter()
        .filter(|e| !e.deleted && e.orig.as_ref().is_some_and(|o| same_place(o, &e.cur)))
        .collect();
    stay.sort_by_key(|e| rank(e));
    let placement = |e: &TriggerEdit| -> Option<Placement> {
        let o = e.orig.as_ref()?;
        if !mysql || !same_place(o, &e.cur) {
            return None;
        }
        let group: Vec<&&TriggerEdit> = stay
            .iter()
            .filter(|x| x.orig.as_ref().is_some_and(|xo| same_place(xo, o)))
            .collect();
        let i = group.iter().position(|x| x.key == e.key)?;
        if i > 0 {
            return Some(Placement::Follows(group[i - 1].cur.name.clone()));
        }
        if let Some(next) = group.get(1).and_then(|next| next.orig.as_ref()) {
            return Some(Placement::Precedes(next.name.clone()));
        }
        // The neighbours aren't part of this save: they are where they were when read, unless
        // this save drops, renames or moves them.
        let untouched = |name: &String| {
            !edits.iter().any(|x| {
                x.orig.as_ref().is_some_and(|xo| {
                    xo.name == *name
                        && (x.deleted || x.cur.name != xo.name || !same_place(xo, &x.cur))
                })
            })
        };
        let (before, after) = &o.neighbours;
        before
            .as_ref()
            .filter(|n| untouched(n))
            .map(|n| Placement::Follows(n.clone()))
            .or_else(|| {
                after
                    .as_ref()
                    .filter(|n| untouched(n))
                    .map(|n| Placement::Precedes(n.clone()))
            })
    };
    let transaction = d == Driver::Postgres;
    ordered
        .into_iter()
        .map(|e| {
            let mut statements = vec![];
            let mut restore = vec![];
            let place = placement(e);
            if let Some(o) = &e.orig {
                let mut drop = Stmt::new(drop_trigger_sql(d, table, &o.name));
                if mysql && !e.deleted {
                    drop.check = Some(create_check(&e.cur));
                    restore.push(Stmt::new(create_trigger_placed(
                        d,
                        table,
                        o,
                        place.as_ref(),
                    )?));
                }
                statements.push(drop);
            }
            if !e.deleted {
                statements.push(Stmt::new(create_trigger_placed(
                    d,
                    table,
                    &e.cur,
                    place.as_ref(),
                )?));
            }
            // Created again under the settings it had; a new trigger gets the connection's.
            let settings = if mysql {
                e.orig.as_ref().map_or(vec![], |o| o.settings.session())
            } else {
                vec![]
            };
            Ok((
                e.key.clone(),
                ObjectSave {
                    statements,
                    restore,
                    transaction,
                    settings,
                },
            ))
        })
        .collect()
}
/// A MySQL trigger's body as written, from SHOW CREATE TRIGGER (information_schema has string
/// escapes already resolved, so its text is not the SQL that created the trigger). Any
/// FOLLOWS / PRECEDES is left out: Telecat places the trigger itself.
pub fn parse_mysql_trigger_body(create: &str) -> Option<String> {
    static CREATE: LazyLock<Regex> = LazyLock::new(|| {
        let account = r#"(?:`(?:``|[^`])*`|'(?:''|\\.|[^'\\])*'|"(?:""|[^"])*"|[^\s@'"`]+)"#;
        let name = format!(r"(?:(?:{MYSQL_IDENT})\s*\.\s*)?(?:{MYSQL_IDENT})");
        Regex::new(&format!(
            r"(?is)^\s*CREATE\s+(?:DEFINER\s*=\s*(?:CURRENT_USER(?:\s*\(\s*\))?|{account}(?:\s*@\s*{account})?)\s+)?TRIGGER\s+(?:IF\s+NOT\s+EXISTS\s+)?{name}\s+(?:BEFORE|AFTER)\s+(?:INSERT|UPDATE|DELETE)\s+ON\s+{name}\s+FOR\s+EACH\s+ROW\s+(?:(?:FOLLOWS|PRECEDES)\s+(?:{MYSQL_IDENT})\s+)?(.*)$"
        ))
        .unwrap()
    });
    CREATE
        .captures(create)
        .map(|m| m[1].trim_end().trim_end_matches(';').trim_end().to_owned())
}
/// Splits PostgreSQL's `pg_get_triggerdef` text into the sheet's fields.
pub fn parse_pg_triggerdef(name: &str, def: &str) -> Option<Trigger> {
    static DEF: LazyLock<Regex> = LazyLock::new(|| {
        let rel = r#"(?:"(?:""|[^"])*"|[^\s."]+)(?:\.(?:"(?:""|[^"])*"|[^\s."]+))?"#;
        Regex::new(&format!(
            r#"(?is)^CREATE\s+TRIGGER\s+(?:"(?:""|[^"])*"|\S+)\s+(BEFORE|AFTER|INSTEAD\s+OF)\s+(.+?)\s+ON\s+{rel}\s+(.*)$"#
        ))
        .unwrap()
    });
    let m = DEF.captures(def.trim())?;
    Some(Trigger {
        name: name.into(),
        timing: collapse(&m[1]).to_ascii_uppercase(),
        event: m[2].to_owned(),
        statement: m[3].trim_end_matches(';').to_owned(),
        ..Default::default()
    })
}
#[cfg(test)]
mod tests;
