//! SQL dump import, live: each test creates a throwaway database on the local test server,
//! imports into it and drops it. zixma and main_db are only used to create and drop it.
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use telecat_core::types::*;
use telecat_db::{ImportOptions, ImportProgress, Session, scan_dump};

fn config(d: Driver, database: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "import-test".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = database.into();
    c
}
/// A throwaway database, dropped when the test ends (even on failure).
struct Scratch {
    driver: Driver,
    name: String,
    dir: PathBuf,
}
impl Scratch {
    async fn new(d: Driver, tag: &str) -> Result<(Self, Session)> {
        let name = format!("telecat_import_qa_{tag}_{}", std::process::id());
        let admin = Session::connect_test(&config(d, home(d)), "secret").await?;
        admin
            .query(&format!("DROP DATABASE IF EXISTS {name}"))
            .await?;
        admin.query(&format!("CREATE DATABASE {name}")).await?;
        let dir = std::env::temp_dir().join(&name);
        std::fs::create_dir_all(&dir)?;
        let s = Session::connect_test(&config(d, &name), "secret").await?;
        Ok((
            Self {
                driver: d,
                name,
                dir,
            },
            s,
        ))
    }
    fn file(&self, name: &str, text: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }
}
fn home(d: Driver) -> &'static str {
    if d == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let (d, name) = (self.driver, self.name.clone());
        let _ = std::fs::remove_dir_all(&self.dir);
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                if let Ok(admin) = Session::connect_test(&config(d, home(d)), "secret").await {
                    if d == Driver::Postgres {
                        let _ = admin
                            .query(&format!(
                                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{name}' AND pid <> pg_backend_pid()"
                            ))
                            .await;
                    }
                    let _ = admin.query(&format!("DROP DATABASE IF EXISTS {name}")).await;
                }
            })
        })
        .join()
        .unwrap();
    }
}
async fn import(
    s: &Session,
    path: &PathBuf,
    opts: ImportOptions,
) -> Result<telecat_db::ImportReport, telecat_db::ImportFailure> {
    s.import_dump(
        path,
        &s.database,
        opts,
        &ImportProgress::default(),
        &AtomicBool::new(false),
    )
    .await
}
fn tx() -> ImportOptions {
    ImportOptions {
        transaction: true,
        ..Default::default()
    }
}
const MYSQLDUMP: &str = "-- MySQL dump 10.13\n/*!40101 SET NAMES utf8mb4 */;\n/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;\nDROP TABLE IF EXISTS `bank`;\n/*!40101 SET @saved_cs_client = @@character_set_client */;\nCREATE TABLE `bank` (\n  `id` int NOT NULL,\n  `name` varchar(60) DEFAULT 'a;b',\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\nLOCK TABLES `bank` WRITE;\n/*!40000 ALTER TABLE `bank` DISABLE KEYS */;\nINSERT INTO `bank` VALUES (1,'ธนาคาร 😺'),(2,'it\\'s; \\\"q\\\" \\\\ back'),(3,'tab\\there\\nline'),(4,NULL);\n/*!40000 ALTER TABLE `bank` ENABLE KEYS */;\nUNLOCK TABLES;\nDELIMITER ;;\nCREATE PROCEDURE `bump`()\nBEGIN\n  UPDATE `bank` SET `name` = CONCAT(`name`, '!') WHERE `id` = 1;\n  SELECT 1;\nEND ;;\n/*!50003 CREATE*/ /*!50003 TRIGGER `bank_bi` BEFORE INSERT ON `bank` FOR EACH ROW BEGIN SET NEW.name = UPPER(NEW.name); END */;;\nDELIMITER ;\n/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;\n";
#[tokio::test]
async fn mysql_dump_imports_byte_exact() -> Result<()> {
    let (scratch, s) = Scratch::new(Driver::Mysql, "m").await?;
    let path = scratch.file("dump.sql", MYSQLDUMP.as_bytes());
    let (scan, size) = scan_dump(&path, Driver::Mysql, &s.database, s.list_databases().await?)?;
    assert_eq!(size, MYSQLDUMP.len() as u64);
    assert_eq!(scan.statements, 13);
    assert!(scan.blocked.is_empty());
    assert_eq!(scan.risk_counts.get("DROP TABLE / VIEW"), Some(&1));
    let report = import(&s, &path, tx())
        .await
        .map_err(|f| anyhow::anyhow!("{f}"))?;
    assert_eq!(report.statements, 13);
    assert!(!report.transaction, "MySQL DDL commits by itself");
    let rows = s
        .query("SELECT id, name, HEX(name) FROM bank ORDER BY id")
        .await?
        .rows;
    let names: Vec<_> = rows.iter().map(|r| r[1].clone()).collect();
    assert_eq!(
        names,
        [
            Some("ธนาคาร 😺".to_owned()),
            Some("it's; \"q\" \\ back".to_owned()),
            Some("tab\there\nline".to_owned()),
            None
        ]
    );
    s.query("CALL bump()").await.ok();
    s.query("INSERT INTO bank (id, name) VALUES (5, 'lower')")
        .await?;
    assert_eq!(
        s.query("SELECT name FROM bank WHERE id IN (1, 5) ORDER BY id")
            .await?
            .rows,
        [[Some("ธนาคาร 😺!".to_owned())], [Some("LOWER".to_owned())]],
        "the procedure and the trigger were created"
    );
    // gzip-compressed, the same dump.
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(b"INSERT INTO `bank` VALUES (6,'gz');\n")?;
    let path = scratch.file("more.sql.gz", &gz.finish()?);
    import(&s, &path, tx())
        .await
        .map_err(|f| anyhow::anyhow!("{f}"))?;
    assert_eq!(
        s.query("SELECT name FROM bank WHERE id = 6").await?.rows,
        [[Some("GZ".to_owned())]]
    );
    Ok(())
}
#[tokio::test]
async fn mysql_error_reports_the_line_and_other_databases_are_blocked() -> Result<()> {
    let (scratch, s) = Scratch::new(Driver::Mysql, "e").await?;
    let path = scratch.file(
        "bad.sql",
        b"CREATE TABLE a (id int);\nINSERT INTO a VALUES (1);\nSELECT\n  1,\n  FROM nowhere;\nCREATE TABLE b (id int);\n",
    );
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert_eq!((f.line, f.column), (5, Some(3)), "{f}");
    assert_eq!(f.executed, 2);
    assert!(!f.rolled_back);
    assert_eq!(
        s.query("SELECT COUNT(*) FROM a").await?.rows,
        [[Some("1".to_owned())]],
        "partial import stays"
    );
    assert!(
        s.query("SELECT 1 FROM b").await.is_err(),
        "nothing after the error ran"
    );
    // Another database: blocked, before and while importing.
    let path = scratch.file("use.sql", b"CREATE TABLE c (id int);\nUSE zixma;\nCREATE TABLE telecat_import_must_not_exist (id int);\n");
    let (scan, _) = scan_dump(&path, Driver::Mysql, &s.database, s.list_databases().await?)?;
    assert_eq!(
        scan.blocked,
        [(2, "uses another database: ZIXMA".to_owned())]
    );
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert_eq!(f.line, 2);
    assert!(f.message.starts_with("Blocked"), "{f}");
    let zixma = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    assert!(
        zixma
            .query("SELECT 1 FROM telecat_import_must_not_exist")
            .await
            .is_err(),
        "zixma untouched"
    );
    // Hidden statements: under NO_BACKSLASH_ESCAPES the server would end the string early and
    // run `USE zixma` and the CREATE in it. Refused before and while importing.
    let path = scratch.file(
        "hidden.sql",
        b"SET sql_mode = 'NO_BACKSLASH_ESCAPES';\nSELECT 'a\\'; USE zixma; CREATE TABLE telecat_import_must_not_exist (id int); SELECT '';\n",
    );
    let err = scan_dump(&path, Driver::Mysql, &s.database, vec![])
        .unwrap_err()
        .to_string();
    // Either reading refuses it: never closed (backslash escapes) or several statements (none).
    assert!(
        err.contains("never closed") || err.contains("more than one statement"),
        "{err}"
    );
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert!(
        f.message.starts_with("Refused") || f.message.contains("never closed"),
        "{f}"
    );
    let gone = |r: Result<telecat_core::types::QueryResult>| r.is_err();
    assert!(
        gone(
            zixma
                .query("SELECT 1 FROM telecat_import_must_not_exist")
                .await
        ),
        "zixma untouched"
    );
    // A qualified name in another database.
    let path = scratch.file(
        "rename.sql",
        b"CREATE TABLE r (id int);\nRENAME TABLE r TO zixma.telecat_import_must_not_exist;\n",
    );
    let (scan, _) = scan_dump(&path, Driver::Mysql, &s.database, s.list_databases().await?)?;
    assert_eq!(scan.blocked.len(), 1, "{scan:?}");
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert!(f.message.starts_with("Blocked"), "{f}");
    assert!(gone(
        zixma
            .query("SELECT 1 FROM telecat_import_must_not_exist")
            .await
    ));
    Ok(())
}
const PGDUMP: &str = "--\n-- PostgreSQL database dump\n--\n\\restrict k3y\nSET statement_timeout = 0;\nSET client_encoding = 'UTF8';\nSELECT pg_catalog.set_config('search_path', '', false);\nCREATE FUNCTION public.shout(t text) RETURNS text\n    LANGUAGE plpgsql\n    AS $_$\nBEGIN\n  RETURN upper(t) || $$!;$$;\nEND;\n$_$;\nCREATE TABLE public.bank (\n    id integer NOT NULL,\n    name text\n);\nCOPY public.bank (id, name) FROM stdin;\n1\tธนาคาร 😺\n2\ttab\\there\\nnew line \\\\ back\n3\t\\N\n\\.\nALTER TABLE ONLY public.bank ADD CONSTRAINT bank_pkey PRIMARY KEY (id);\n\\unrestrict k3y\n";
#[tokio::test]
async fn postgres_dump_imports_copy_data_byte_exact() -> Result<()> {
    let (scratch, s) = Scratch::new(Driver::Postgres, "p").await?;
    let path = scratch.file("dump.sql", PGDUMP.as_bytes());
    let (scan, _) = scan_dump(&path, Driver::Postgres, &s.database, vec![])?;
    assert_eq!((scan.statements, scan.copy_blocks), (7, 1));
    let report = import(&s, &path, tx())
        .await
        .map_err(|f| anyhow::anyhow!("{f}"))?;
    assert_eq!(
        (report.statements, report.copied_rows, report.transaction),
        (7, 3, true)
    );
    assert_eq!(
        s.query("SELECT name, public.shout('a') FROM public.bank ORDER BY id")
            .await?
            .rows,
        [
            [Some("ธนาคาร 😺".to_owned()), Some("A!;".to_owned())],
            [
                Some("tab\there\nnew line \\ back".to_owned()),
                Some("A!;".to_owned())
            ],
            [None, Some("A!;".to_owned())],
        ]
    );
    Ok(())
}
#[tokio::test]
async fn postgres_error_or_cancel_rolls_everything_back() -> Result<()> {
    let (scratch, s) = Scratch::new(Driver::Postgres, "r").await?;
    let path = scratch.file(
        "bad.sql",
        b"CREATE TABLE a (id int);\nINSERT INTO a VALUES (1);\nSELECT 1\n  FRM a;\n",
    );
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert_eq!((f.line, f.column), (4, Some(7)), "{f}");
    assert!(f.rolled_back, "{f}");
    assert!(s.query("SELECT 1 FROM a").await.is_err(), "rolled back");
    // \connect to another database: blocked.
    let path = scratch.file(
        "connect.sql",
        b"CREATE TABLE b (id int);\n\\connect main_db\nCREATE TABLE c (id int);\n",
    );
    let (scan, _) = scan_dump(&path, Driver::Postgres, &s.database, vec![])?;
    assert_eq!(scan.refused.len(), 1);
    let f = import(&s, &path, tx()).await.unwrap_err();
    assert_eq!(f.line, 2);
    assert!(s.query("SELECT 1 FROM b").await.is_err(), "rolled back");
    // Cancel while a statement runs.
    let path = scratch.file(
        "slow.sql",
        b"CREATE TABLE d (id int);\nSELECT pg_sleep(20);\nCREATE TABLE e (id int);\n",
    );
    let s = std::sync::Arc::new(s);
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let (s2, c2) = (s.clone(), cancel.clone());
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        c2.store(true, std::sync::atomic::Ordering::SeqCst);
        s2.cancel_import().await
    });
    let started = std::time::Instant::now();
    let f = s
        .import_dump(
            &path,
            &s.database,
            tx(),
            &ImportProgress::default(),
            &cancel,
        )
        .await
        .unwrap_err();
    canceller.await??;
    assert!(f.cancelled && f.rolled_back, "{f}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(s.query("SELECT 1 FROM d").await.is_err());
    drop(s);
    drop(scratch);
    Ok(())
}
#[test]
fn binary_and_custom_format_files_are_refused() {
    let dir = std::env::temp_dir().join(format!("telecat_import_bin_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let custom = dir.join("dump.custom");
    std::fs::write(&custom, b"PGDMP\x01\x0f\x00\x04\x08\x01").unwrap();
    let err = scan_dump(&custom, Driver::Postgres, "x", vec![])
        .unwrap_err()
        .to_string();
    assert!(err.contains("pg_restore"), "{err}");
    let binary = dir.join("image.sql");
    std::fs::write(&binary, [0x89, b'P', b'N', b'G', 0, 0, 1]).unwrap();
    assert!(
        scan_dump(&binary, Driver::Mysql, "x", vec![])
            .unwrap_err()
            .to_string()
            .contains("binary")
    );
    std::fs::remove_dir_all(dir).unwrap();
}
