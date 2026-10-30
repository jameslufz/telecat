//! Startup SQL: statements a connection runs on every new database connection, right after it
//! opens and before Telecat's own session setup.
//!
//! Telecat keeps several connections per workspace (editor, commits, table reads, metadata,
//! imports) and opens new ones on reconnect, so these statements run many times. They are
//! limited to session settings: a write would repeat on every connection, and an open
//! transaction or autocommit turned off would break how saves are committed and checked.
use crate::{
    sql::{first_keyword, is_read_only, split_statements, strip_comments},
    types::Driver,
};
use anyhow::{Result, bail};

/// The statements of `sql`, in order, once each is known to only change session settings.
pub fn startup_statements(sql: &str, d: Driver) -> Result<Vec<String>> {
    let mut out = vec![];
    for (i, statement) in split_statements(sql, d).into_iter().enumerate() {
        let text = statement.text;
        if strip_comments(&text, d)
            .trim()
            .trim_end_matches(';')
            .trim()
            .is_empty()
        {
            continue;
        }
        if let Some(why) = refusal(&text, d) {
            bail!(
                "Startup SQL statement {} can't be used: {why} Startup SQL runs on each of Telecat's connections, so it may only change session settings (SET, RESET or a read-only SELECT).",
                i + 1
            );
        }
        out.push(text);
    }
    Ok(out)
}

fn refusal(sql: &str, d: Driver) -> Option<&'static str> {
    let code = strip_comments(sql, d).to_ascii_uppercase();
    let words: Vec<&str> = code
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@' || c == '.'))
        .filter(|w| !w.is_empty())
        .collect();
    match first_keyword(sql, d).as_str() {
        "SET" => set_refusal(&words, d),
        "RESET" if d == Driver::Postgres => None,
        "SELECT" if is_read_only(sql, d) => None,
        "SELECT" => Some("it may write."),
        _ => Some("it isn't a SET, RESET or SELECT statement."),
    }
}

fn set_refusal(words: &[&str], d: Driver) -> Option<&'static str> {
    let global = |w: &&str| {
        matches!(*w, "GLOBAL" | "PERSIST" | "PERSIST_ONLY")
            || w.starts_with("@@GLOBAL.")
            || w.starts_with("@@PERSIST")
    };
    if d == Driver::Mysql && words.iter().any(global) {
        return Some("it changes a server-wide setting.");
    }
    if d == Driver::Mysql && matches!(words.get(1), Some(&"PASSWORD" | &"DEFAULT")) {
        return Some("it changes an account.");
    }
    let autocommit = |w: &&str| {
        let name = w
            .trim_start_matches("@@SESSION.")
            .trim_start_matches("@@LOCAL.");
        matches!(name, "AUTOCOMMIT" | "@@AUTOCOMMIT")
    };
    if words.iter().any(autocommit) {
        return Some("Telecat's saves rely on autocommit staying on.");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_settings_are_kept_in_order() {
        let sql = "SET time_zone = '+07:00';\n-- comment only\n;SET NAMES utf8mb4; SELECT 1";
        assert_eq!(
            startup_statements(sql, Driver::Mysql).unwrap(),
            ["SET time_zone = '+07:00'", "SET NAMES utf8mb4", "SELECT 1"]
        );
        assert_eq!(
            startup_statements(
                "SET search_path TO app, public; RESET work_mem;",
                Driver::Postgres
            )
            .unwrap()
            .len(),
            2
        );
        assert!(startup_statements("", Driver::Postgres).unwrap().is_empty());
        assert!(
            startup_statements("  -- nothing\n", Driver::Mysql)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn writes_transactions_and_server_settings_are_refused() {
        for (sql, d) in [
            ("INSERT INTO t VALUES (1)", Driver::Mysql),
            ("BEGIN", Driver::Postgres),
            ("START TRANSACTION", Driver::Mysql),
            ("SET autocommit = 0", Driver::Mysql),
            ("SET @@autocommit = 0", Driver::Mysql),
            ("SET SESSION autocommit=0", Driver::Mysql),
            ("set @@session.autocommit = OFF", Driver::Mysql),
            ("SET GLOBAL read_only = 1", Driver::Mysql),
            ("SET @@global.max_connections = 1", Driver::Mysql),
            ("SET PERSIST sql_mode = ''", Driver::Mysql),
            ("SET PASSWORD = 'x'", Driver::Mysql),
            ("SELECT * INTO OUTFILE '/tmp/x' FROM t", Driver::Mysql),
            ("USE other", Driver::Mysql),
            ("DO $$ BEGIN END $$", Driver::Postgres),
            ("RESET QUERY CACHE", Driver::Mysql),
        ] {
            let e = startup_statements(&format!("SET a = 1; {sql}"), d).unwrap_err();
            let message = e.to_string();
            assert!(
                message.starts_with("Startup SQL statement 2 "),
                "{sql}: {message}"
            );
        }
    }

    #[test]
    fn a_refusal_never_echoes_the_statement() {
        let e = startup_statements("INSERT INTO secrets VALUES ('hunter2')", Driver::Mysql)
            .unwrap_err()
            .to_string();
        assert!(!e.contains("hunter2"));
    }
}
