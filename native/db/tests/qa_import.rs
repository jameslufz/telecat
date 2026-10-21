//! QA 0.3.0 regression tests: SQL dump import attacks. Uses throwaway databases qa030imp (target)
//! and qa030victim (must stay untouched).
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use telecat_core::types::*;
use telecat_db::{ImportOptions, ImportProgress, Session, scan_dump};

fn config(d: Driver, database: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa030-import".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = database.into();
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
            std::env::temp_dir().join(format!("telecat_qa_import_{}", std::process::id()))
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
    Ok(admin)
}
async fn victim_tables(d: Driver) -> Vec<String> {
    let v = Session::connect_test(&config(d, "qa030victim"), "secret")
        .await
        .unwrap();
    let sql = if d == Driver::Mysql {
        "SELECT table_name FROM information_schema.tables WHERE table_schema='qa030victim' ORDER BY 1"
    } else {
        "SELECT table_name FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog','information_schema') ORDER BY 1"
    };
    v.query(sql)
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect()
}
async fn run(d: Driver, name: &str, text: &str, transaction: bool) -> (String, String) {
    let p = dir().join(name);
    std::fs::write(&p, text).unwrap();
    let admin = Session::connect_test(&config(d, home(d)), "secret")
        .await
        .unwrap();
    let dbs = admin.list_databases().await.unwrap_or_default();
    let scan = match scan_dump(&p, d, "qa030imp", dbs) {
        Ok((s, _)) => format!(
            "stmts={} blocked={:?} risks={:?} no_tx={:?}",
            s.statements, s.blocked, s.risk_counts, s.no_transaction
        ),
        Err(e) => format!("SCAN ERROR {e}"),
    };
    let s = Session::connect_test(&config(d, "qa030imp"), "secret")
        .await
        .unwrap();
    let progress = ImportProgress::default();
    let cancel = AtomicBool::new(false);
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                expect: None,
                transaction,
            },
            &progress,
            &cancel,
        )
        .await;
    let res = match r {
        Ok(rep) => format!("OK {rep:?}"),
        Err(f) => format!(
            "FAIL line={} col={:?} rolled_back={} executed={} msg={}",
            f.line,
            f.column,
            f.rolled_back,
            f.executed,
            f.message.lines().next().unwrap_or("")
        ),
    };
    (scan, res)
}

#[tokio::test]
async fn qa_mysql_import_bypasses() -> Result<()> {
    let _turn = serial();
    let d = Driver::Mysql;
    let mut bad = vec![];
    for (name, dump) in [
        (
            "prepare",
            "CREATE TABLE a (id int);\nSET @s = CONCAT('CREATE TABLE ', 'qa030victim', '.pwned_prepare (id int)');\nPREPARE p FROM @s;\nEXECUTE p;\n",
        ),
        (
            "procedure",
            "DELIMITER ;;\nCREATE PROCEDURE p2() BEGIN SET @s = 'CREATE TABLE qa030victim.pwned_proc (id int)'; PREPARE x FROM @s; EXECUTE x; END ;;\nDELIMITER ;\nCALL p2();\n",
        ),
        (
            "use-comment",
            "USE /* hi */ qa030victim;\nCREATE TABLE pwned_use (id int);\n",
        ),
        (
            "use-versioned",
            "/*!40000 USE qa030victim */;\nCREATE TABLE pwned_usev (id int);\n",
        ),
        (
            "use-backtick",
            "USE `qa030victim`;\nCREATE TABLE pwned_useb (id int);\n",
        ),
        (
            "use-lower-string",
            "use 'qa030victim';\nCREATE TABLE pwned_uses (id int);\n",
        ),
        (
            "create-view-other",
            "CREATE VIEW qa030victim.pwned_view AS SELECT 1 AS x;\n",
        ),
        (
            "rename-other",
            "CREATE TABLE r (id int);\nRENAME TABLE r TO qa030victim.pwned_rename;\n",
        ),
        (
            "alter-rename-other",
            "CREATE TABLE r2 (id int);\nALTER TABLE r2 RENAME TO `qa030victim` . `pwned_alter`;\n",
        ),
        (
            "trigger-other",
            "CREATE TABLE tr (id int);\nCREATE TABLE qa030imp.ok (id int);\nDELIMITER ;;\nCREATE TRIGGER trg AFTER INSERT ON tr FOR EACH ROW INSERT INTO qa030victim.marker VALUES (NEW.id) ;;\nDELIMITER ;\nINSERT INTO tr VALUES (7);\n",
        ),
        (
            "ansi-quotes",
            "SET sql_mode = 'ANSI_QUOTES';\nCREATE TABLE \"qa030victim\".\"pwned_ansi\" (id int);\n",
        ),
        (
            "comment-dot",
            "CREATE TABLE qa030victim /* x */ . pwned_cdot (id int);\n",
        ),
        (
            "event",
            "CREATE EVENT ev ON SCHEDULE AT CURRENT_TIMESTAMP DO INSERT INTO qa030victim.marker VALUES (99);\n",
        ),
        ("set-global", "SET GLOBAL max_connections = 151;\n"),
        (
            "into-outfile",
            "SELECT 1 INTO OUTFILE '/tmp/qa030_outfile.txt';\n",
        ),
        (
            "nbe-split",
            "SET sql_mode = 'NO_BACKSLASH_ESCAPES';\nCREATE TABLE s (v text);\nINSERT INTO s VALUES ('a\\');\nCREATE TABLE qa030victim.pwned_nbe (id int);\n-- ');\n",
        ),
    ] {
        reset(d).await?;
        let (scan, res) = run(d, name, dump, false).await;
        let vt = victim_tables(d).await;
        let rows = Session::connect_test(&config(d, "qa030victim"), "secret")
            .await?
            .query("SELECT COUNT(*) FROM marker")
            .await?
            .rows;
        eprintln!(
            "\n[{name}]\n  scan: {scan}\n  import: {res}\n  victim tables: {vt:?} marker rows: {rows:?}"
        );
        if vt != vec!["marker".to_owned()] || rows != vec![vec![Some("0".to_owned())]] {
            bad.push(format!("{name}: victim database changed: {vt:?} {rows:?}"));
        }
    }
    // global setting probe cleanup
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa030imp").await?;
    admin.query("DROP DATABASE IF EXISTS qa030victim").await?;
    eprintln!("\nBAD-MYSQL-IMPORT: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_postgres_import_bypasses() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    let mut bad = vec![];
    for (name, dump, tx) in [
        (
            "dblink",
            "CREATE EXTENSION IF NOT EXISTS dblink;\nSELECT dblink_exec('dbname=qa030victim user=postgres', 'CREATE TABLE pwned_dblink (id int)');\n",
            false,
        ),
        (
            "copy-program",
            "COPY (SELECT 1) TO PROGRAM 'psql -U postgres -d qa030victim -c \"CREATE TABLE pwned_prog (id int)\"';\n",
            false,
        ),
        (
            "alter-database-other",
            "ALTER DATABASE qa030victim SET search_path = nowhere;\n",
            false,
        ),
        (
            "connect-variants",
            "CREATE TABLE a (id int);\n\\connect -reuse-previous=on dbname=qa030victim\nCREATE TABLE pwned_conn (id int);\n",
            true,
        ),
        (
            "c-quoted",
            "\\c \"qa030victim\"\nCREATE TABLE pwned_cq (id int);\n",
            true,
        ),
        (
            "commit-midway",
            "CREATE TABLE before_commit (id int);\nCOMMIT;\nCREATE TABLE after_commit (id int);\nSELECT 1/0;\n",
            true,
        ),
        (
            "rollback-midway",
            "CREATE TABLE r1 (id int);\nROLLBACK;\nCREATE TABLE r2 (id int);\n",
            true,
        ),
        (
            "do-execute",
            "DO $$ BEGIN EXECUTE 'CREATE TABLE pwned_do (id int)'; END $$;\n",
            true,
        ),
        (
            "set-role",
            "SET ROLE postgres;\nCREATE TABLE setrole (id int);\n",
            true,
        ),
    ] {
        reset(d).await?;
        let (scan, res) = run(d, name, dump, tx).await;
        let vt = victim_tables(d).await;
        let imp = Session::connect_test(&config(d, "qa030imp"), "secret").await?
            .query("SELECT string_agg(table_name, ',' ORDER BY table_name) FROM information_schema.tables WHERE table_schema = 'public'").await?.rows;
        let victim_sp = Session::connect_test(&config(d, home(d)), "secret").await?
            .query("SELECT coalesce(string_agg(array_to_string(setconfig, ','), ';'), '') FROM pg_db_role_setting s JOIN pg_database db ON db.oid = s.setdatabase WHERE db.datname = 'qa030victim'").await?.rows;
        eprintln!(
            "\n[{name}]\n  scan: {scan}\n  import: {res}\n  victim tables: {vt:?} victim db settings: {victim_sp:?}\n  target tables after: {imp:?}"
        );
        if vt != vec!["marker".to_owned()] || victim_sp != vec![vec![Some(String::new())]] {
            bad.push(format!(
                "{name}: victim database changed: {vt:?} {victim_sp:?}"
            ));
        }
        if name == "commit-midway"
            && res.contains("rolled_back=true")
            && imp
                .iter()
                .flatten()
                .flatten()
                .any(|t| t.contains("before_commit"))
        {
            bad.push(format!(
                "commit-midway: reported rolled back but tables remain: {imp:?}"
            ));
        }
        if name == "rollback-midway" && res.starts_with("OK") {
            bad.push(format!(
                "rollback-midway: import OK but earlier statements silently discarded: {imp:?}"
            ));
        }
    }
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    eprintln!("\nBAD-PG-IMPORT: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

async fn run_bytes(d: Driver, name: &str, bytes: &[u8], transaction: bool) -> (String, String) {
    let p = dir().join(name);
    std::fs::write(&p, bytes).unwrap();
    let scan = match scan_dump(&p, d, "qa030imp", vec![]) {
        Ok((s, _)) => format!(
            "stmts={} copy={} blocked={:?} risks={:?}",
            s.statements, s.copy_blocks, s.blocked, s.risk_counts
        ),
        Err(e) => format!("SCAN ERROR {e}"),
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
                expect: None,
                transaction,
            },
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    let res = match r {
        Ok(rep) => format!("OK {rep:?}"),
        Err(f) => format!(
            "FAIL line={} col={:?} rolled_back={} executed={} msg={} | excerpt={}",
            f.line,
            f.column,
            f.rolled_back,
            f.executed,
            f.message.lines().next().unwrap_or(""),
            f.excerpt
        ),
    };
    (scan, res)
}
async fn q(d: Driver, sql: &str) -> Vec<Vec<Option<String>>> {
    Session::connect_test(&config(d, "qa030imp"), "secret")
        .await
        .unwrap()
        .query(sql)
        .await
        .map(|r| r.rows)
        .unwrap_or_else(|e| vec![vec![Some(format!("ERR {e}"))]])
}

#[tokio::test]
async fn qa_mysql_splitter_and_errors() -> Result<()> {
    let _turn = serial();
    let d = Driver::Mysql;
    reset(d).await?;
    let dump = "-- CRLF, Thai\r\nCREATE TABLE t (id int PRIMARY KEY, v text) DEFAULT CHARSET utf8mb4;\r\n\
INSERT INTO t VALUES (1, 'semi;colon -- not a comment /* nor this */ # nor this'), (2, 'ไทย 😺 \\';'), (3, \"dq;'\");\r\n\
# hash comment ; with semicolon\r\n\
/* block ; comment */ INSERT INTO t VALUES (4, `v`) ;\r\n\
INSERT INTO t VALUES (5, 'x') ; SELECT 1--1;\r\n;\r\n\
CREATE TABLE `we;ird` (`c;1` int);\r\n\
DELIMITER $$\r\n\
CREATE PROCEDURE p1() BEGIN SELECT ';'; SELECT '$$'; END$$\r\n\
delimiter ;\r\n\
DELIMITER ;;\r\n\
/*!50003 CREATE*/ /*!50003 TRIGGER trg BEFORE INSERT ON t FOR EACH ROW BEGIN SET NEW.v = CONCAT(NEW.v, ';'); END */;;\r\n\
DELIMITER ;\r\n\
INSERT INTO t VALUES (6, 'trig');\r\n\
/* a /* b */ INSERT INTO t VALUES (7, 'after-nested');\r\n\
INSERT INTO t VALUES (8, 'multi\r\nline;\r\n');\r\n";
    let (scan, res) = run_bytes(d, "split.sql", dump.as_bytes(), false).await;
    eprintln!("\n[mysql-split]\n  scan: {scan}\n  import: {res}");
    eprintln!(
        "  rows: {:?}",
        q(d, "SELECT id, v FROM t ORDER BY id").await
    );
    eprintln!("  tables: {:?}", q(d, "SHOW TABLES").await);
    eprintln!(
        "  routines: {:?}",
        q(
            d,
            "SELECT routine_name FROM information_schema.routines WHERE routine_schema='qa030imp'"
        )
        .await
    );
    // Error line/column: error on the 3rd line of the 4th statement, after Thai text.
    reset(d).await?;
    let err = "CREATE TABLE e (id int, v text) DEFAULT CHARSET utf8mb4;\n\nINSERT INTO e VALUES (1, 'ไทย');\n-- comment line\nINSERT INTO e\n  VALUES (2, 'ไทย 😺'),\n\t(3, 'x') WHERE oops;\n";
    let (_, res) = run_bytes(d, "err.sql", err.as_bytes(), false).await;
    eprintln!("\n[mysql-err] expect line 7 (WHERE at col 12 with tab=1 char): {res}");
    assert!(res.contains("line=7 col=Some(11)"), "{res}");
    let err2 =
        "CREATE TABLE e2 (a int);\nSELECT a FROM e2 WHERE a = 1 AND a = 1 AND\n a = 1 AND b = 1;\n";
    reset(d).await?;
    let (_, res) = run_bytes(d, "err2.sql", err2.as_bytes(), false).await;
    eprintln!("[mysql-err-repeat] expect line 3 col 12 (unknown column b): {res}");
    // gzip, truncated gzip, binary, pg custom
    reset(d).await?;
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    std::io::Write::write_all(
        &mut gz,
        b"CREATE TABLE g (id int);\nINSERT INTO g VALUES (1),(2);\n",
    )?;
    let gzb = gz.finish()?;
    let (scan, res) = run_bytes(d, "ok.sql.gz", &gzb, false).await;
    eprintln!(
        "\n[gzip] {scan} | {res} | {:?}",
        q(d, "SELECT COUNT(*) FROM g").await
    );
    reset(d).await?;
    let mut big = String::from("CREATE TABLE g2 (id int);\n");
    for i in 0..20000 {
        big.push_str(&format!("INSERT INTO g2 VALUES ({i});\n"));
    }
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    std::io::Write::write_all(&mut gz, big.as_bytes())?;
    let gzb = gz.finish()?;
    let cut = &gzb[..gzb.len() * 2 / 3];
    let (scan, res) = run_bytes(d, "trunc.sql.gz", cut, false).await;
    eprintln!(
        "[gzip-truncated] {scan} | {res} | rows {:?}",
        q(d, "SELECT COUNT(*) FROM g2").await
    );
    let mut bin = b"CREATE TABLE b (id int);\n".to_vec();
    bin.extend((0..255u8).collect::<Vec<_>>());
    let (scan, res) = run_bytes(d, "bin.sql", &bin, false).await;
    eprintln!("[binary-after-text] {scan} | {res}");
    let (scan, res) = run_bytes(d, "custom.dump", b"PGDMP\x01\x0e\x00rest", false).await;
    eprintln!("[pg-custom] {scan} | {res}");
    let (scan, res) = run_bytes(d, "utf16.sql", &[0xff, 0xfe, b'C', 0, b'R', 0], false).await;
    eprintln!("[utf16] {scan} | {res}");
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa030imp").await?;
    admin.query("DROP DATABASE IF EXISTS qa030victim").await?;
    Ok(())
}

#[tokio::test]
async fn qa_postgres_splitter_and_errors() -> Result<()> {
    let _turn = serial();
    let d = Driver::Postgres;
    reset(d).await?;
    let dump = "-- pg plain\r\nCREATE TABLE t (id int PRIMARY KEY, v text);\r\n\
CREATE FUNCTION f() RETURNS text LANGUAGE sql AS $fn$ SELECT 'a;b' || $$;$$ $fn$;\r\n\
/* outer /* inner ; */ still comment ; */ INSERT INTO t VALUES (1, E'it\\'s; ok');\r\n\
INSERT INTO t VALUES (2, 'quote''s; -- no comment');\r\n\
CREATE TABLE \"we;ird\" (\"c;1\" int);\r\n\
COPY t (id, v) FROM stdin;\r\n\
3\ta.b\r\n\
4\t\\\\.\r\n\
5\tไทย 😺\r\n\
\\.\r\n\
SELECT f();\r\n\
INSERT INTO t VALUES (6, $q$multi\r\nline; $$ $q$);\r\n";
    let (scan, res) = run_bytes(d, "split.sql", dump.as_bytes(), true).await;
    eprintln!("\n[pg-split]\n  scan: {scan}\n  import: {res}");
    eprintln!(
        "  rows: {:?}",
        q(d, "SELECT id, v FROM t ORDER BY id").await
    );
    eprintln!("  f(): {:?}", q(d, "SELECT f()").await);
    // Error in a later statement: line/col, and full rollback.
    reset(d).await?;
    let err = "CREATE TABLE e (id int);\nINSERT INTO e VALUES (1);\n\n-- c\nINSERT INTO e\n  VALUES (2),\n\t(ไทย);\n";
    let (_, res) = run_bytes(d, "err.sql", err.as_bytes(), true).await;
    eprintln!(
        "\n[pg-err] expect line 7 col 3: {res} | e exists? {:?}",
        q(d, "SELECT to_regclass('public.e')::text").await
    );
    reset(d).await?;
    let err = "CREATE TABLE e (id int);\nCREATE FUNCTION g() RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 1 +;\nEND $$;\n";
    let (_, res) = run_bytes(d, "errfn.sql", err.as_bytes(), true).await;
    eprintln!("[pg-err-fnbody] expect line 4: {res}");
    // COPY data error mid-block
    reset(d).await?;
    let err = "CREATE TABLE c (id int);\nCOPY c FROM stdin;\n1\n2\nx\n3\n\\.\n";
    let (_, res) = run_bytes(d, "errcopy.sql", err.as_bytes(), true).await;
    eprintln!(
        "[pg-err-copy] expect line ~5: {res} | c? {:?}",
        q(d, "SELECT to_regclass('public.c')::text").await
    );
    // COPY never ends
    let (scan, res) = run_bytes(
        d,
        "copyopen.sql",
        b"CREATE TABLE c2 (id int);\nCOPY c2 FROM stdin;\n1\n2\n",
        true,
    )
    .await;
    eprintln!(
        "[pg-copy-unterminated] {scan} | {res} | c2? {:?}",
        q(d, "SELECT to_regclass('public.c2')::text").await
    );
    // standard_conforming_strings off
    let (scan, res) = run_bytes(
        d,
        "scs.sql",
        b"SET standard_conforming_strings = off;\nCREATE TABLE s (v text);\n",
        true,
    )
    .await;
    eprintln!("[pg-scs-off] {scan} | {res}");
    // psql meta commands
    let (scan, res) = run_bytes(
        d,
        "meta.sql",
        b"\\! touch /tmp/qa030_pwn\nCREATE TABLE m (id int);\n",
        true,
    )
    .await;
    eprintln!("[pg-meta-shell] {scan} | {res}");
    let (scan, res) = run_bytes(
        d,
        "meta2.sql",
        b"CREATE TABLE m2 (id int);\n  \\connect qa030victim\nCREATE TABLE m3 (id int);\n",
        true,
    )
    .await;
    eprintln!(
        "[pg-meta-indented-connect] {scan} | {res} | victim {:?}",
        victim_tables(d).await
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

fn rss_kb() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}
fn big_dump() -> PathBuf {
    let p = dir().join("big.sql");
    if !p.exists() {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
        writeln!(
            f,
            "CREATE TABLE big (id bigint, v text);\nCOPY big (id, v) FROM stdin;"
        )
        .unwrap();
        let pad = "ไทย-padding-".repeat(7);
        let mut i = 0u64;
        while f.get_ref().metadata().map(|m| m.len()).unwrap_or(0) < 500 * 1024 * 1024 {
            for _ in 0..10000 {
                writeln!(f, "{i}\t{pad}{i}").unwrap();
                i += 1;
            }
            f.flush().unwrap();
        }
        writeln!(f, "\\.\nCREATE TABLE after_big (id int);").unwrap();
    }
    p
}
#[tokio::test]
#[ignore = "QA 0.3.0 probe: 500 MB dump, needs QA_DIR and QA_BIG"]
async fn qa_postgres_big_memory_and_cancel() -> Result<()> {
    let _turn = serial();
    if std::env::var("QA_BIG").is_err() {
        return Ok(());
    }
    let d = Driver::Postgres;
    reset(d).await?;
    let p = big_dump();
    eprintln!(
        "dump size {} MB, rss before {} MB",
        std::fs::metadata(&p)?.len() / 1048576,
        rss_kb() / 1024
    );
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (s2, p2) = (stop.clone(), peak.clone());
    let sampler = std::thread::spawn(move || {
        while !s2.load(std::sync::atomic::Ordering::SeqCst) {
            p2.fetch_max(rss_kb(), std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    let t = std::time::Instant::now();
    let (scan, size) = scan_dump(&p, d, "qa030imp", vec![])?;
    eprintln!(
        "scan {} stmts {} copy blocks in {:?}, peak rss {} MB",
        scan.statements,
        scan.copy_blocks,
        t.elapsed(),
        peak.load(std::sync::atomic::Ordering::SeqCst) / 1024
    );
    let _ = size;
    let s = Session::connect_test(&config(d, "qa030imp"), "secret").await?;
    let t = std::time::Instant::now();
    let r = s
        .import_dump(
            &p,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                expect: None,
                transaction: true,
            },
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await;
    eprintln!(
        "import {:?} in {:?}, peak rss {} MB",
        r.as_ref().map_err(|f| f.message.clone()),
        t.elapsed(),
        peak.load(std::sync::atomic::Ordering::SeqCst) / 1024
    );
    eprintln!(
        "rows: {:?}",
        q(
            d,
            "SELECT count(*), to_regclass('after_big')::text FROM big"
        )
        .await
    );
    // Cancel mid-COPY
    reset(d).await?;
    let s = std::sync::Arc::new(Session::connect_test(&config(d, "qa030imp"), "secret").await?);
    let progress = std::sync::Arc::new(ImportProgress::default());
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let (s3, pr3, c3, p3) = (s.clone(), progress.clone(), cancel.clone(), p.clone());
    let job = tokio::spawn(async move {
        s3.import_dump(
            &p3,
            "qa030imp",
            ImportOptions {
                allow: Default::default(),
                expect: None,
                transaction: true,
            },
            &pr3,
            &c3,
        )
        .await
    });
    while progress.bytes.load(std::sync::atomic::Ordering::Relaxed) < 100 * 1024 * 1024 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = s.cancel_import().await;
    let t = std::time::Instant::now();
    let r = job.await?;
    eprintln!(
        "cancel -> {:?} after {:?}",
        r.as_ref()
            .map(|_| ())
            .map_err(|f| (f.cancelled, f.rolled_back, f.line, f.message.clone())),
        t.elapsed()
    );
    eprintln!(
        "after cancel: {:?}",
        q(d, "SELECT to_regclass('public.big')::text").await
    );
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    sampler.join().unwrap();
    let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030imp WITH (FORCE)")
        .await?;
    admin
        .query("DROP DATABASE IF EXISTS qa030victim WITH (FORCE)")
        .await?;
    Ok(())
}
