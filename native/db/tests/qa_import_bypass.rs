//! Import-bypass attacks (QA 0.3.0 round 3) on the test servers. Target qa030imp; qa030victim
//! must stay untouched. No `allow` opt-ins are set (the default user path).
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use telecat_core::types::*;
use telecat_db::{ImportOptions, ImportProgress, Session, scan_dump};

fn config(d: Driver, db: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa030-bypass".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = db.into();
    c
}
fn home(d: Driver) -> &'static str {
    if d == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
}
/// The tests share the qa030imp/qa030victim databases, so they take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}
fn dir() -> PathBuf {
    let d = std::env::var("QA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("telecat_qa_bypass_{}", std::process::id()))
        })
        .join("dumps");
    std::fs::create_dir_all(&d).unwrap();
    d
}
async fn reset(d: Driver) -> Result<Session> {
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    for db in ["qa030imp", "qa030victim"] {
        if d == Driver::Postgres {
            admin
                .query(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"))
                .await?;
        } else {
            admin
                .query(&format!("DROP DATABASE IF EXISTS {db}"))
                .await?;
        }
        admin.query(&format!("CREATE DATABASE {db}")).await?;
    }
    let v = Session::connect_test(&config(d, "qa030victim"), "secret").await?;
    v.query("CREATE TABLE marker (id int)").await?;
    v.query("INSERT INTO marker VALUES (0)").await?;
    if d == Driver::Postgres {
        // a second schema inside the TARGET database, to judge cross-schema writes
        let t = Session::connect_test(&config(d, "qa030imp"), "secret").await?;
        t.query("CREATE SCHEMA secret").await?;
        t.query("CREATE TABLE secret.vault (id int)").await?;
    }
    Ok(admin)
}
/// victim tables + marker sum; for PG also the target's `secret` schema tables.
async fn state(d: Driver) -> (Vec<String>, String, Vec<String>) {
    let v = Session::connect_test(&config(d, "qa030victim"), "secret")
        .await
        .unwrap();
    let (tsql, msql) = if d == Driver::Mysql {
        (
            "SELECT table_name FROM information_schema.tables WHERE table_schema='qa030victim' ORDER BY 1",
            "SELECT COALESCE(SUM(id),-1) FROM marker",
        )
    } else {
        (
            "SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY 1",
            "SELECT COALESCE(SUM(id),-1) FROM marker",
        )
    };
    let tables = v
        .query(tsql)
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    let marker = v
        .query(msql)
        .await
        .map(|r| {
            r.rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next().flatten())
                .unwrap_or_default()
        })
        .unwrap_or_else(|e| format!("ERR {e}"));
    let secret = if d == Driver::Postgres {
        let t = Session::connect_test(&config(d, "qa030imp"), "secret")
            .await
            .unwrap();
        t.query("SELECT tablename FROM pg_tables WHERE schemaname='secret' ORDER BY 1")
            .await
            .unwrap()
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect()
    } else {
        vec![]
    };
    (tables, marker, secret)
}
async fn run(d: Driver, name: &str, text: &str, transaction: bool) -> (String, String) {
    let p = dir().join(format!("{name}.sql"));
    std::fs::write(&p, text).unwrap();
    let admin = Session::connect_test(&config(d, home(d)), "secret")
        .await
        .unwrap();
    let dbs = admin.list_databases().await.unwrap_or_default();
    let scan = match scan_dump(&p, d, "qa030imp", dbs) {
        Ok((s, _)) => format!(
            "stmts={} blocked_kinds={:?} risks={:?}",
            s.statements,
            s.blocked_kinds,
            s.risk_counts.keys().collect::<Vec<_>>()
        ),
        Err(e) => format!("SCAN-ERR {}", e.to_string().lines().next().unwrap_or("")),
    };
    let s = Session::connect_test(&config(d, "qa030imp"), "secret")
        .await
        .unwrap();
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                transaction,
                expect: None,
            },
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    let res = match r {
        Ok(rep) => format!("OK {rep:?}"),
        Err(f) => format!(
            "FAIL executed={} rolled={:?} msg={}",
            f.executed,
            f.outcome,
            f.message.lines().next().unwrap_or("")
        ),
    };
    (scan, res)
}

#[tokio::test]
async fn qa_mysql_import_bypass_round3() -> Result<()> {
    let _turn = serial();
    let d = Driver::Mysql;
    let mut bad = vec![];
    let cases: &[(&str, &str)] = &[
        (
            "use-comment-block",
            "CREATE TABLE/**/a/**/(id int);\nUSE/**/qa030victim;\nCREATE TABLE pwned (id int);\n",
        ),
        (
            "use-versioned",
            "/*!40101 USE qa030victim */;\nCREATE TABLE pwned (id int);\n",
        ),
        (
            "handler",
            "HANDLER qa030victim.marker OPEN;\nHANDLER qa030victim.marker READ FIRST;\nHANDLER qa030victim.marker CLOSE;\n",
        ),
        (
            "rename-to-other",
            "CREATE TABLE r (id int);\nRENAME TABLE r TO qa030victim.pwned_rename;\n",
        ),
        (
            "ctas-from-other",
            "CREATE TABLE ctas SELECT * FROM qa030victim.marker;\n",
        ),
        (
            "insert-select-other",
            "CREATE TABLE loot (id int);\nINSERT INTO loot SELECT * FROM qa030victim.marker;\n",
        ),
        (
            "view-other",
            "CREATE VIEW peek AS SELECT * FROM qa030victim.marker;\nSELECT * FROM peek;\n",
        ),
        (
            "definer-trigger-write",
            "CREATE TABLE t2 (id int);\nCREATE DEFINER=`root`@`%` TRIGGER trg AFTER INSERT ON t2 FOR EACH ROW INSERT INTO qa030victim.marker VALUES (NEW.id);\nINSERT INTO t2 VALUES (42);\n",
        ),
        (
            "definer-view-other",
            "CREATE DEFINER=`root`@`%` VIEW peek2 AS SELECT id FROM qa030victim.marker;\n",
        ),
        (
            "create-event",
            "CREATE DEFINER=`root`@`%` EVENT ev ON SCHEDULE AT CURRENT_TIMESTAMP + INTERVAL 1 HOUR DO INSERT INTO qa030victim.marker VALUES (7);\n",
        ),
        (
            "alter-event",
            "CREATE EVENT ev2 ON SCHEDULE EVERY 1 HOUR DO SELECT 1;\nALTER EVENT ev2 DO INSERT INTO qa030victim.marker VALUES (8);\n",
        ),
        (
            "procedure-write-other",
            "CREATE PROCEDURE p() INSERT INTO qa030victim.marker VALUES (9);\nCALL p();\n",
        ),
        (
            "create-server",
            "CREATE SERVER s FOREIGN DATA WRAPPER mysql OPTIONS (HOST '127.0.0.1', DATABASE 'qa030victim', USER 'root', PASSWORD 'secret', PORT 3306);\n",
        ),
        ("set-global-at", "SET @@global.max_connections = 151;\n"),
        (
            "set-session-log-bin",
            "SET SESSION sql_log_bin = 0;\nCREATE TABLE sneaky (id int);\n",
        ),
        (
            "case-whitespace-use",
            "Use\tqa030victim ;\nCREATE TABLE pwned (id int);\n",
        ),
    ];
    let blocked = ["create-server", "set-global-at", "set-session-log-bin"];
    for (name, dump) in cases {
        reset(d).await?;
        let (scan, res) = run(d, name, dump, false).await;
        if blocked.contains(name) && !res.contains("msg=Blocked:") {
            bad.push(format!("{name}: not blocked: {res}"));
        }
        let (vt, marker, _) = state(d).await;
        eprintln!(
            "\n[{name}]\n  scan: {scan}\n  import: {res}\n  victim: tables={vt:?} marker_sum={marker}"
        );
        if vt != vec!["marker".to_owned()] || marker != "0" {
            bad.push(format!(
                "{name}: victim changed tables={vt:?} marker={marker}"
            ));
        }
    }
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    // event scheduler could fire ev later: give it no chance, just drop.
    admin.query("DROP DATABASE IF EXISTS qa030imp").await?;
    admin.query("DROP DATABASE IF EXISTS qa030victim").await?;
    eprintln!("\nBAD-MYSQL: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_postgres_import_bypass_round3() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    let mut bad = vec![];
    let cases: &[(&str, &str, bool)] = &[
        (
            "func-sql-security-definer",
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql SECURITY DEFINER AS $$ CREATE TABLE secret.leaked (id int) $$;\nSELECT f();\n",
            true,
        ),
        (
            "create-rule",
            "CREATE TABLE t (id int);\nCREATE RULE r AS ON INSERT TO t DO ALSO INSERT INTO secret.vault VALUES (NEW.id);\nINSERT INTO t VALUES (1);\n",
            true,
        ),
        (
            "trigger-dynamic",
            "CREATE FUNCTION tf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'CREATE TABLE secret.boom (id int)'; RETURN NEW; END $$;\nCREATE TABLE t2 (id int);\nCREATE TRIGGER trg AFTER INSERT ON t2 FOR EACH ROW EXECUTE FUNCTION tf();\nINSERT INTO t2 VALUES (1);\n",
            true,
        ),
        (
            "alter-owner",
            "CREATE TABLE t3 (id int);\nALTER TABLE t3 OWNER TO postgres;\n",
            true,
        ),
        (
            "search-path-unqualified",
            "SET search_path = secret, public;\nCREATE TABLE sneaky (id int);\nINSERT INTO vault VALUES (99);\n",
            true,
        ),
        (
            "other-schema-create",
            "CREATE TABLE secret.direct (id int);\n",
            true,
        ),
        (
            "call-commit",
            "CREATE PROCEDURE pc() LANGUAGE plpgsql AS $$ BEGIN CREATE TABLE a (id int); COMMIT; CREATE TABLE b (id int); END $$;\nCALL pc();\nSELECT 1/0;\n",
            true,
        ),
        (
            "tx-in-versioned",
            "CREATE TABLE a (id int);\n/*!40000 COMMIT */;\nCREATE TABLE b (id int);\nSELECT 1/0;\n",
            true,
        ),
        (
            "tx-leading-comment",
            "CREATE TABLE a (id int);\n-- x\n/* y */ COMMIT;\nCREATE TABLE b (id int);\nSELECT 1/0;\n",
            true,
        ),
        (
            "event-trigger",
            "CREATE FUNCTION et() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN END $$;\nCREATE EVENT TRIGGER evt ON ddl_command_start EXECUTE FUNCTION et();\n",
            true,
        ),
        ("load-lib", "LOAD 'plpgsql';\n", true),
        ("create-language", "CREATE LANGUAGE plperl;\n", true),
        (
            "pg-terminate",
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'qa030victim';\n",
            true,
        ),
        (
            "set-config-searchpath",
            "SELECT set_config('search_path', 'secret, public', false);\nCREATE TABLE sneaky2 (id int);\n",
            true,
        ),
        (
            "set-config-role",
            "SELECT set_config('role', 'postgres', false);\nCREATE TABLE sneaky3 (id int);\n",
            true,
        ),
        (
            "create-publication",
            "CREATE PUBLICATION pub FOR ALL TABLES;\n",
            true,
        ),
        (
            "create-subscription",
            "CREATE SUBSCRIPTION sub CONNECTION 'dbname=qa030victim user=postgres' PUBLICATION pub WITH (connect = false);\n",
            false,
        ),
        (
            "copy-program-case",
            "copy (select 1) to program 'id';\n",
            true,
        ),
        (
            "dblink-obfus",
            "CREATE EXTENSION IF NOT EXISTS dblink;\nSELECT /**/ dblink_exec('dbname=qa030victim', 'CREATE TABLE secret.x (id int)');\n",
            true,
        ),
    ];
    let blocked = [
        "func-sql-security-definer",
        "load-lib",
        "pg-terminate",
        "set-config-searchpath",
        "set-config-role",
        "create-publication",
        "create-subscription",
        "dblink-obfus",
    ];
    for (name, dump, tx) in cases {
        reset(d).await?;
        let (scan, res) = run(d, name, dump, *tx).await;
        if blocked.contains(name) && !res.contains("msg=Blocked:") {
            bad.push(format!("{name}: not blocked: {res}"));
        }
        if *name == "copy-program-case" && !res.contains("msg=Refused:") {
            bad.push(format!("{name}: not refused: {res}"));
        }
        let (vt, marker, sec) = state(d).await;
        eprintln!(
            "\n[{name}]\n  scan: {scan}\n  import: {res}\n  victim: tables={vt:?} marker_sum={marker} target.secret={sec:?}"
        );
        let victim_clean = vt == vec!["marker".to_owned()] && marker == "0";
        // secret.vault is pre-existing; anything else in secret, or vault with rows, is a reach past the target's intended scope
        let secret_extra: Vec<_> = sec.iter().filter(|t| *t != "vault").collect();
        if !victim_clean {
            bad.push(format!(
                "{name}: VICTIM DB changed tables={vt:?} marker={marker}"
            ));
        }
        // Other schemas of the target database are in scope; only the gaps must not reach them.
        if blocked.contains(name) && !secret_extra.is_empty() {
            bad.push(format!("{name}: created {secret_extra:?}"));
        }
    }
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    eprintln!("\nBAD-PG (victim reached): {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_pg_meta_commands_round3() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    reset(d).await?;
    for (name, dump) in [
        (
            "copy-meta",
            "\\copy marker FROM '/etc/hostname'\nCREATE TABLE a (id int);\n",
        ),
        ("include-i", "\\i /etc/passwd\n"),
        ("include-ir", "\\ir other.sql\n"),
        ("include-word", "\\include /etc/passwd\n"),
        ("shell", "\\! id\n"),
        ("gexec", "SELECT 'CREATE TABLE secret.g (id int)' \\gexec\n"),
        ("o-file", "\\o /tmp/qa030_out\nSELECT 1;\n"),
        ("set-backtick", "\\set x `id`\nCREATE TABLE a (id int);\n"),
    ] {
        let (scan, res) = run(d, name, dump, true).await;
        let (_, _, sec) = state(d).await;
        eprintln!("[{name}] scan: {scan} | import: {res} | secret={sec:?}");
        assert_eq!(sec, ["vault"], "{name}");
        // `\set` only sets a psql variable (never run here); the rest must not import at all.
        if name == "set-backtick" {
            assert!(res.starts_with("OK"), "{name}: {res}");
        } else {
            assert!(res.starts_with("FAIL executed=0"), "{name}: {res}");
        }
        if !matches!(name, "gexec" | "set-backtick") {
            assert!(
                scan.starts_with("SCAN-ERR Unsupported psql command"),
                "{name}: {scan}"
            );
        }
    }
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    Ok(())
}

use std::io::Write;
use telecat_db::{FileCheck, scan_dump_checked};

#[tokio::test]
async fn qa_file_change_detection() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    let admin = reset(d).await?;
    let p = dir().join("change.sql");
    let mut body = String::from("CREATE TABLE t (id int);\n");
    for i in 0..50000 {
        body.push_str(&format!("INSERT INTO t VALUES ({i});\n"));
    }
    std::fs::write(&p, &body)?;
    let dbs = admin.list_databases().await.unwrap_or_default();
    let (_scan, check) = scan_dump_checked(&p, d, "qa030imp", dbs)?;
    let s = Session::connect_test(&config(d, "qa030imp"), "secret").await?;
    let opts = |c: FileCheck| ImportOptions {
        allow: Default::default(),
        transaction: true,
        expect: Some(c),
    };

    // (a) replace the file between scan and run (size/mtime differ)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&p, "CREATE TABLE evil (id int);\n")?;
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            opts(check.clone()),
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    let evil = s
        .query("SELECT to_regclass('public.evil')::text")
        .await?
        .rows;
    assert!(r.is_err(), "a file replaced after the scan must not import");
    assert_eq!(evil, vec![vec![None::<String>]]);

    // (b) same size + mtime but different content (content hash must catch it at finish)
    reset(d).await?;
    let s = Session::connect_test(&config(d, "qa030imp"), "secret").await?;
    std::fs::write(&p, &body)?;
    let (_scan, check) = scan_dump_checked(&p, d, "qa030imp", vec![])?;
    // reference file carrying the original mtime, to restore it after the swap
    let refp = dir().join("change.ref");
    std::fs::copy(&p, &refp)?;
    // craft a same-length body: swap one value so a late INSERT differs but length is identical
    let swapped = body.replacen(
        "INSERT INTO t VALUES (49999);",
        "INSERT INTO t VALUES (99999);",
        1,
    );
    assert_eq!(swapped.len(), body.len());
    std::fs::write(&p, &swapped)?;
    std::process::Command::new("touch")
        .arg("-r")
        .arg(&refp)
        .arg(&p)
        .status()?;
    assert_eq!(
        std::fs::metadata(&p)?.modified()?,
        std::fs::metadata(&refp)?.modified()?
    );
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            opts(check.clone()),
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    let has99999 = s
        .query("SELECT count(*) FROM t WHERE id = 99999")
        .await
        .map(|r| r.rows)
        .unwrap_or_default();
    let tcount = s
        .query("SELECT count(*), to_regclass('public.t')::text FROM t")
        .await
        .map(|r| r.rows)
        .unwrap_or_default();
    let message = r.err().map(|f| f.message).unwrap_or_default();
    assert!(message.contains("content differs"), "{message}");
    assert!(
        has99999.is_empty() && tcount.is_empty(),
        "the import must roll back"
    );

    // (c) grow the file while it is being imported (between chunks)
    reset(d).await?;
    let s = std::sync::Arc::new(Session::connect_test(&config(d, "qa030imp"), "secret").await?);
    let mut big = String::from("CREATE TABLE t (id int);\n");
    for i in 0..400000 {
        big.push_str(&format!("INSERT INTO t VALUES ({i});\n"));
    }
    std::fs::write(&p, &big)?;
    let (_scan, check) = scan_dump_checked(&p, d, "qa030imp", vec![])?;
    let progress = std::sync::Arc::new(ImportProgress::default());
    let (s2, pr2, p2, c2) = (s.clone(), progress.clone(), p.clone(), check.clone());
    let job = tokio::spawn(async move {
        s2.import_dump(
            &p2,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                transaction: true,
                expect: Some(c2),
            },
            &pr2,
            &AtomicBool::new(false),
        )
        .await
    });
    while progress.bytes.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    // append more statements mid-stream
    let mut f = std::fs::OpenOptions::new().append(true).open(&p)?;
    for i in 0..200000 {
        writeln!(f, "INSERT INTO t VALUES ({});", 1000000 + i)?;
    }
    f.flush()?;
    let r = job.await?;
    let message = r.err().map(|f| f.message).unwrap_or_default();
    assert!(message.contains("changed while"), "{message}");
    assert_eq!(
        s.query("SELECT to_regclass('public.t')::text").await?.rows,
        vec![vec![None::<String>]]
    );

    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    Ok(())
}

#[tokio::test]
async fn qa_copy_row_line() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    let admin = reset(d).await?;
    let p = dir().join("copyerr.sql");
    // bad integer on the 4th data row; COPY statement on file line 2
    let dump = "CREATE TABLE c (id int, v text);\nCOPY c (id, v) FROM stdin;\n1\ta\n2\tb\n3\tc\nBAD\td\n5\te\n\\.\n";
    std::fs::write(&p, dump)?;
    let _ = admin;
    let s = Session::connect_test(&config(d, "qa030imp"), "secret").await?;
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                transaction: true,
                expect: None,
            },
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    let f = r.expect_err("a bad COPY row must fail");
    assert_eq!(f.line, 6, "the file line of the BAD row");
    assert!(f.message.contains("22P02"), "{}", f.message);
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    Ok(())
}
