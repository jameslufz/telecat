//! QA re-verification probes for the new guard (core/src/guard.rs). Printing + asserting the
//! SAFE expectation. cargo test -p telecat-core --test qa_reverify -- --test-threads=1 --nocapture
use Driver::*;
use std::time::Instant;
use telecat_core::{sql::*, types::*};

fn show(cases: &[(Driver, &str)]) -> Vec<String> {
    let mut missed = vec![];
    for (d, s) in cases {
        let r = dangerous_reason(s, *d);
        println!(
            "{:<8} {:<70} danger={:<5} ro={}",
            format!("{d:?}"),
            s.replace('\n', "\\n"),
            r.is_some(),
            is_read_only(s, *d)
        );
        if r.is_none() {
            missed.push(format!("{d:?}: {s}"));
        }
    }
    missed
}

#[test]
fn qa2_new_bypasses() {
    let cases: &[(Driver, &str)] = &[
        // PG dollar tags may contain non-ASCII letters; the guard only accepts ASCII tags.
        (Postgres, "DELETE FROM t WHERE $ä$x$ä$ <> 'y'"),
        (
            Postgres,
            "DELETE FROM t WHERE $ä$ id = 1 $ä$ = $b$ id = 1 $b$",
        ),
        // Non-ASCII chars that the servers treat as identifier characters.
        (Mysql, "DELETE FROM t AS a\u{a0}WHERE\u{a0}b"),
        (Postgres, "DELETE FROM t AS a\u{a0}WHERE\u{a0}b"),
        (Mysql, "DELETE FROM t AS a·WHERE·b"),
        (Mysql, "UPDATE t AS a·WHERE·x SET v = 0"),
        (Postgres, "DELETE FROM t AS a·WHERE·b"),
        // Constant string comparisons treated as numbers.
        (Mysql, "DELETE FROM t WHERE 'a' <> 'b'"),
        (Postgres, "DELETE FROM t WHERE 'a' <> 'b'"),
        (Mysql, "DELETE FROM t WHERE 'a' < 'b'"),
        (Mysql, "DELETE FROM t WHERE 'a' = 'A'"),
        (Mysql, "DELETE FROM t WHERE 1 = '1'"),
        (Mysql, "DELETE FROM t WHERE 1 IN (1)"),
        (Mysql, "DELETE FROM t WHERE EXISTS (SELECT 1)"),
        (Mysql, "DELETE FROM t WHERE 1 BETWEEN 0 AND 2"),
        (Mysql, "DELETE FROM t WHERE 'x' LIKE '%'"),
        (Mysql, "DELETE FROM t WHERE NULL IS NULL"),
        (Mysql, "DELETE FROM t WHERE id IS NULL OR id IS NOT NULL"),
        (Mysql, "DELETE FROM t WHERE ~0"),
        (Mysql, "DELETE FROM t WHERE b'1'"),
        (Mysql, "DELETE FROM t WHERE id = id + 0"),
        // Unterminated comments (server behaviour checked live).
        (Mysql, "DELETE FROM t /*!99999 WHERE id = 1"),
        (Mysql, "DELETE FROM t /* WHERE id = 1"),
        (Postgres, "DELETE FROM t /* WHERE id = 1"),
        // Misc
        (Mysql, "DELETE LOW_PRIORITY QUICK IGNORE FROM t"),
        (Mysql, "DELETE t FROM t"),
        (Mysql, "UPDATE LOW_PRIORITY t SET v = 0"),
        (Postgres, "DELETE FROM ONLY t"),
        (Postgres, "UPDATE t SET v = 0 FROM u"),
        (Postgres, "WITH RECURSIVE x AS (SELECT 1) DELETE FROM t"),
        (Postgres, "EXPLAIN (ANALYZE TRUE) DELETE FROM t"),
        (Postgres, "explain analyse delete from t"),
        (Postgres, "PREPARE p AS DELETE FROM t"),
        (Mysql, "TABLE t"),
        (Mysql, "SELECT * FROM t INTO OUTFILE '/tmp/x'"),
    ];
    let missed = show(cases);
    println!("\nNOT FLAGGED ({}):\n{}", missed.len(), missed.join("\n"));
}

#[test]
fn qa2_false_positives() {
    let cases: &[(Driver, &str)] = &[
        (Mysql, "SELECT * FROM t WHERE a = 'x;y' -- note"),
        (Mysql, "SELECT a--b FROM t"),
        (Mysql, "SHOW TABLES"),
        (Mysql, "SHOW CREATE TABLE t"),
        (Mysql, "DESCRIBE t"),
        (Mysql, "EXPLAIN SELECT * FROM t"),
        (Mysql, "EXPLAIN UPDATE t SET a = 1"),
        (Mysql, "EXPLAIN FORMAT=JSON SELECT 1"),
        (Postgres, "EXPLAIN (VERBOSE, COSTS OFF) SELECT 1"),
        (Postgres, "EXPLAIN DELETE FROM t"),
        (Mysql, "UPDATE t SET a = 1 WHERE id = 5"),
        (Mysql, "DELETE FROM t WHERE id IN (1, 2, 3)"),
        (Mysql, "UPDATE t SET a = '--' WHERE id = 1"),
        (Mysql, "UPDATE t SET a = 'it''s' WHERE id = 1"),
        (Mysql, "UPDATE t SET a = 'O\\'Brien' WHERE id = 1"),
        (Postgres, "UPDATE t SET a = 'C:\\' WHERE id = 1"),
        (Postgres, "UPDATE t SET a = E'x\\'y' WHERE id = 1"),
        (Mysql, "INSERT INTO t VALUES (1, 'a')"),
        (
            Mysql,
            "INSERT INTO t (id, v) VALUES (1, 2) ON DUPLICATE KEY UPDATE v = 2",
        ),
        (
            Postgres,
            "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET v = 1",
        ),
        (Mysql, "CREATE TABLE x (id int)"),
        (Mysql, "CREATE INDEX i ON t (a)"),
        (Mysql, "ALTER TABLE t ADD COLUMN `type` int"),
        (Mysql, "ALTER TABLE t ADD COLUMN c int AFTER b"),
        (Postgres, "ALTER TABLE t ALTER COLUMN c SET DEFAULT 0"),
        (Postgres, "ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0)"),
        (Mysql, "SET NAMES utf8mb4"),
        (Mysql, "SET @x = 1"),
        (Postgres, "SET search_path TO app, public"),
        (Postgres, "BEGIN"),
        (Postgres, "COMMIT"),
        (Postgres, "CREATE OR REPLACE VIEW v AS SELECT 1"),
        (
            Postgres,
            "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
        ),
        (Mysql, "CALL report_monthly()"),
        (Mysql, "SET SESSION sql_mode = 'STRICT_ALL_TABLES'"),
        (Mysql, "SET autocommit = 1"),
        (Postgres, "COPY t TO STDOUT"),
        (Postgres, "SELECT * FROM t WHERE id = $1"),
        (Mysql, "SELECT JSON_EXTRACT(j, '$.a') FROM t"),
        (Mysql, "SELECT j->>'$.a' FROM t"),
        (Postgres, "SELECT '{\"a\":1}'::jsonb ->> 'a'"),
        (
            Mysql,
            "DELETE FROM t WHERE created_at < NOW() - INTERVAL 30 DAY",
        ),
        (
            Mysql,
            "UPDATE t SET deleted_at = NULL WHERE id = 7 AND deleted_at IS NOT NULL",
        ),
        (Postgres, "UPDATE t SET a = 1 FROM u WHERE t.id = u.id"),
        (
            Mysql,
            "UPDATE t JOIN u ON t.id = u.id SET t.a = 1 WHERE u.flag = 1",
        ),
        (Postgres, "DELETE FROM t WHERE id = ANY($1)"),
        (Mysql, "SELECT 'ไทย', '😺' FROM t"),
        (
            Mysql,
            "CREATE PROCEDURE p() BEGIN UPDATE t SET a = 1 WHERE id = 1; SELECT 1; END",
        ),
        (
            Mysql,
            "CREATE DEFINER=`root`@`%` TRIGGER tr BEFORE UPDATE ON t FOR EACH ROW BEGIN SET NEW.a = 1; SET NEW.b = 2; END",
        ),
        (
            Postgres,
            "CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN DELETE FROM log; RETURN NEW; END $body$",
        ),
        (
            Postgres,
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $fn_ä$ BEGIN NULL; END $fn_ä$",
        ),
    ];
    let mut flagged = vec![];
    for (d, s) in cases {
        if let Some(r) = dangerous_reason(s, *d) {
            flagged.push(format!("{d:?}: {s}\n      -> {r}"));
        }
    }
    println!(
        "FLAGGED ({} of {}):\n{}",
        flagged.len(),
        cases.len(),
        flagged.join("\n")
    );
    let ro_fail: Vec<_> = [
        (Mysql, "SELECT 1"),
        (Mysql, "SHOW TABLES"),
        (Mysql, "EXPLAIN SELECT 1"),
        (Postgres, "SELECT $tag$a;b$tag$"),
        (Postgres, "SELECT 'a' -- x;\n"),
        (Mysql, "SELECT a--b FROM t"),
    ]
    .iter()
    .filter(|(d, s)| !is_read_only(s, *d))
    .collect();
    println!("not read-only (should be): {ro_fail:?}");
}

#[test]
fn qa2_performance_and_panics() {
    let big = format!(
        "SELECT {} FROM t WHERE id = 1",
        vec!["'a;b''c\\'d', `x`, \"y\", /* c */ 1"; 40_000].join(", ")
    );
    for d in [Mysql, Postgres] {
        let t = Instant::now();
        let _ = dangerous_reason(&big, d);
        let _ = is_read_only(&big, d);
        println!("{d:?}: {} KB in {:?}", big.len() / 1024, t.elapsed());
    }
    let deep = format!(
        "DELETE FROM t WHERE {}1=1{}",
        "(".repeat(100_000),
        ")".repeat(100_000)
    );
    let t = Instant::now();
    println!(
        "deep parens -> {:?} in {:?}",
        dangerous_reason(&deep, Mysql),
        t.elapsed()
    );
    let ors = format!(
        "DELETE FROM t WHERE {}",
        vec!["id = 1"; 50_000].join(" OR ")
    );
    let t = Instant::now();
    println!(
        "50k ORs -> {:?} in {:?}",
        dangerous_reason(&ors, Mysql).is_some(),
        t.elapsed()
    );
    let dollars = "$a$ ".repeat(20_000);
    let t = Instant::now();
    let _ = dangerous_reason(&format!("SELECT {dollars}"), Postgres);
    println!("20k unterminated $a$ in {:?}", t.elapsed());
    let comments = "/*!50000 ".repeat(20_000);
    let t = Instant::now();
    let _ = dangerous_reason(&format!("SELECT 1 {comments}"), Mysql);
    println!("20k unterminated /*!50000 in {:?}", t.elapsed());
    let dashes = "--1 ".repeat(50_000);
    let t = Instant::now();
    let _ = dangerous_reason(&format!("DELETE FROM t WHERE id = 1 {dashes}"), Mysql);
    println!("50k --1 in {:?}", t.elapsed());
}

#[test]
fn qa2_run_all_noise() {
    // The app splits first (QueryPlan::new) and classifies each piece. Count what the NEW guard
    // adds per piece, grouped by reason prefix, across the Electron SQL fixture corpus.
    let v: serde_json::Value = serde_json::from_str(include_str!("fixtures/sql.json")).unwrap();
    let mut pieces = 0usize;
    let mut by: std::collections::BTreeMap<String, usize> = Default::default();
    let mut samples: std::collections::BTreeMap<String, String> = Default::default();
    for c in v.as_array().unwrap() {
        let s = c["sql"].as_str().unwrap();
        let d: Driver = serde_json::from_value(c["d"].clone()).unwrap();
        for p in split_statements(s, d) {
            pieces += 1;
            if let Some(r) = telecat_core::guard::server_dangerous_reason(&p.text, d) {
                let key: String = r.split(':').next().unwrap().chars().take(50).collect();
                *by.entry(key.clone()).or_default() += 1;
                samples
                    .entry(key)
                    .or_insert_with(|| p.text.chars().take(100).collect());
            }
        }
    }
    println!("{pieces} split statements; guard-flagged by reason:");
    for (k, n) in &by {
        println!("{n:>6}  {k:<52} e.g. {:?}", samples[k]);
    }
    let scripts = [
        (
            Postgres,
            "BEGIN;\nSET search_path TO app;\nCREATE TABLE IF NOT EXISTS users (id serial PRIMARY KEY, email text);\nCREATE OR REPLACE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.updated_at = now(); RETURN NEW; END $$;\nCREATE OR REPLACE VIEW active_users AS SELECT * FROM users;\nINSERT INTO users (email) VALUES ('a@b.c');\nUPDATE users SET email = lower(email) WHERE id > 0;\nCOMMIT;",
        ),
        (
            Mysql,
            "/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;\n/*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */;\nCREATE TABLE t (id int PRIMARY KEY);\nLOCK TABLES `t` WRITE;\n/*!40000 ALTER TABLE `t` DISABLE KEYS */;\nINSERT INTO `t` VALUES (1),(2);\n/*!40000 ALTER TABLE `t` ENABLE KEYS */;\nUNLOCK TABLES;",
        ),
        (
            Mysql,
            "SELECT * FROM orders WHERE status = 'new';\nSELECT COUNT(*) FROM users;\nUPDATE orders SET status = 'paid' WHERE id = 42;\nCALL refresh_stats();",
        ),
    ];
    for (d, s) in scripts {
        let flagged: Vec<_> = split_statements(s, d)
            .into_iter()
            .filter_map(|p| {
                dangerous_reason(&p.text, d)
                    .map(|r| format!("{} -> {r}", p.text.lines().next().unwrap()))
            })
            .collect();
        println!(
            "\nscript ({d:?}) flagged {}:\n  {}",
            flagged.len(),
            flagged.join("\n  ")
        );
    }
}

#[test]
fn qa2_versioned_perf_source() {
    let s = format!("SELECT 1 {}", "/*!50000 ".repeat(20_000));
    let t = Instant::now();
    let _ = telecat_core::guard::server_dangerous_reason(&s, Mysql);
    println!("guard::server_dangerous_reason {:?}", t.elapsed());
    let t = Instant::now();
    let _ = telecat_core::guard::server_read_only(&s, Mysql);
    println!("guard::server_read_only {:?}", t.elapsed());
    let t = Instant::now();
    let _ = first_keyword(&s, Mysql);
    println!("legacy first_keyword/code_only {:?}", t.elapsed());
    let t = Instant::now();
    let _ = split_statements(&s, Mysql);
    println!("split_statements {:?}", t.elapsed());
    let s2 = format!("SELECT 1 {}", "/*!50000 ".repeat(80_000));
    let t = Instant::now();
    let _ = dangerous_reason(&s2, Mysql);
    println!("80k (4x input): dangerous_reason {:?}", t.elapsed());
}
