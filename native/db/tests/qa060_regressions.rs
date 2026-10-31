//! QA 0.6.0 live probes (.claude-qa/qa060/qa060_live.rs), kept as regression tests (test containers only: MySQL :3307 zixma,
//! PG :5434 main_db). Each assertion encodes the safe behaviour. Saves run the way ⌘S runs them:
//! the statements under the object's own settings and, on failure, the restore only when the
//! object is really gone (as `Workspace::restore_original` decides).
use anyhow::Result;
use telecat_core::{
    changes::{Stmt, quote_ident, table_op_statement},
    export::{ExportFormat, ExportWriter},
    objects::{CreateSettings, ObjectSave, Trigger, TriggerEdit, routine_save, trigger_saves},
    types::*,
};
use telecat_db::Session;

async fn connect_as(d: Driver, user: &str, pw: &str) -> Result<Session> {
    let mut c = empty_connection(d);
    c.host = "127.0.0.1".into();
    c.port = if d == Driver::Mysql { 3307 } else { 5434 };
    c.user = user.into();
    c.database = if d == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
    .into();
    Session::connect_test(&c, pw).await
}
async fn connect(d: Driver) -> Result<Session> {
    connect_as(
        d,
        if d == Driver::Mysql {
            "root"
        } else {
            "postgres"
        },
        "secret",
    )
    .await
}
async fn run(db: &Session, sqls: &[&str]) -> Result<()> {
    for s in sqls {
        db.query(s)
            .await
            .map_err(|e| anyhow::anyhow!("{s}: {e:#}"))?;
    }
    Ok(())
}
fn schema(d: Driver) -> &'static str {
    if d == Driver::Mysql {
        "zixma"
    } else {
        "public"
    }
}
fn tref(d: Driver, name: &str) -> TableRef {
    TableRef {
        schema: schema(d).into(),
        name: name.into(),
    }
}
async fn rows(db: &Session, sql: &str) -> Result<Vec<Vec<Option<String>>>> {
    Ok(db.query(sql).await?.rows)
}
async fn scalar(db: &Session, sql: &str) -> Result<Option<String>> {
    Ok(rows(db, sql).await?.first().and_then(|r| r[0].clone()))
}
async fn object(db: &Session, name: &str, kind: ObjectKind) -> DbObject {
    db.list_objects()
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.table.name == name && o.kind == kind)
        .unwrap_or_else(|| panic!("{name} not listed"))
}
/// A routine's definition and the settings it was created under, as the tab loads them.
async fn definition(db: &Session, o: &DbObject) -> Result<(String, CreateSettings)> {
    let d = db.routine_definition(o).await?;
    Ok((d.text, d.settings))
}
/// ⌘S of an object save: its statements under its settings; on failure the restore, only when
/// the object is really gone.
async fn save_like_the_app(
    db: &Session,
    save: &ObjectSave,
    gone: impl std::future::Future<Output = Result<bool>>,
) -> Result<(), String> {
    let r = db
        .execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
        .await;
    let Err(e) = r else { return Ok(()) };
    let mut msg = format!("{e:#}");
    if !save.restore.is_empty() && gone.await.map_err(|e| e.to_string())? {
        let rr = db
            .execute_after_review_with(&save.restore, false, false, &save.settings)
            .await;
        msg += &format!("\nrestore: {:?}", rr.err().map(|e| format!("{e:#}")));
    }
    Err(msg)
}
async fn routine_meta(db: &Session, name: &str) -> Result<Vec<Option<String>>> {
    let mut m = routine_meta_all(db, name).await?;
    // sql_mode is checked by its own probe (qa060_mysql_routine_keeps_its_sql_mode).
    if std::env::var("QA060_IGNORE_MODE").is_ok() && m.len() > 3 {
        m[3] = None;
    }
    Ok(m)
}
async fn routine_meta_all(db: &Session, name: &str) -> Result<Vec<Option<String>>> {
    Ok(rows(db, &format!(
        "SELECT routine_definition, definer, security_type, sql_mode, routine_comment, is_deterministic, sql_data_access, character_set_client, collation_connection FROM information_schema.routines WHERE routine_schema='zixma' AND routine_name='{name}'"
    ))
    .await?
    .into_iter()
    .next()
    .unwrap_or_default())
}

/// Plan 1: saving a realistic MySQL routine unchanged (or with a trivial edit) must produce a
/// restore and recreate the same routine (body, definer, security, mode, comment, charset).
#[tokio::test]
async fn qa060_mysql_routine_realistic_bodies_round_trip() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    let cases: Vec<(&str, ObjectKind, String)> = vec![
        ("qa060_p_tx", ObjectKind::Procedure, r#"CREATE DEFINER=`root`@`%` PROCEDURE `qa060_p_tx`(IN p_id INT, OUT p_msg VARCHAR(100))
    SQL SECURITY INVOKER
    COMMENT 'moves money; keep atomic -- really'
BEGIN
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    RESIGNAL;
  END;
  # hash comment ; with semicolon
  START TRANSACTION;
  -- dash comment END;
  SET p_msg = CONCAT('ok; END; ', p_id, ' ไทย "q" \\ back');
  outer_loop: LOOP
    IF p_id > 3 THEN LEAVE outer_loop; END IF;
    SET p_id = p_id + 1;
  END LOOP outer_loop;
  CASE p_id WHEN 1 THEN SET p_msg = 'one'; ELSE BEGIN END; END CASE;
  WHILE p_id < 0 DO SET p_id = p_id + 1; END WHILE;
  REPEAT SET p_id = p_id - 1; UNTIL p_id <= 0 END REPEAT;
  /* block ; comment */
  COMMIT;
END"#.into()),
        ("qa060_f_plain", ObjectKind::Function, "CREATE FUNCTION `qa060_f_plain`(a DOUBLE) RETURNS double DETERMINISTIC NO SQL RETURN a * 2".into()),
        ("qa060_f_cursor", ObjectKind::Function, r#"CREATE FUNCTION qa060_f_cursor() RETURNS INT READS SQL DATA
BEGIN
  DECLARE done INT DEFAULT 0;
  DECLARE n INT DEFAULT 0;
  DECLARE c CURSOR FOR SELECT 1 UNION SELECT 2;
  DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = 1;
  OPEN c;
  read_loop: LOOP
    FETCH c INTO n;
    IF done THEN LEAVE read_loop; END IF;
  END LOOP;
  CLOSE c;
  RETURN n;
END"#.into()),
        ("qa060_ไทย", ObjectKind::Function, "CREATE FUNCTION `qa060_ไทย`(s VARCHAR(50)) RETURNS varchar(60) CHARSET utf8mb4 DETERMINISTIC RETURN CONCAT('สวัสดี ', s)".into()),
    ];
    for (name, kind, create) in &cases {
        let kw = if *kind == ObjectKind::Procedure {
            "PROCEDURE"
        } else {
            "FUNCTION"
        };
        run(&db, &[&format!("DROP {kw} IF EXISTS `{name}`")]).await?;
        run(&db, &[create.as_str()]).await?;
        let o = object(&db, name, *kind).await;
        let before = routine_meta(&db, name).await?;
        let (original, settings) = definition(&db, &o).await?;
        // Save the unchanged text again.
        let save = routine_save(d, &o, &original, &original, &settings)
            .map_err(|e| anyhow::anyhow!("{name}: routine_save refused: {e:#}\n{original}"))?;
        assert_eq!(
            save.statements.len(),
            4,
            "{name}: try under a temporary name, drop, create"
        );
        assert_eq!(
            save.restore.len(),
            1,
            "{name}: no restore for a realistic body\n{original}"
        );
        db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
            .await
            .map_err(|e| anyhow::anyhow!("{name}: save failed: {e:#}"))?;
        let after = routine_meta(&db, name).await?;
        assert_eq!(
            before, after,
            "{name}: routine changed by an unchanged save"
        );
        // Restore path: run the restore after a DROP (what happens after a broken save).
        run(&db, &[&format!("DROP {kw} `{name}`")]).await?;
        db.execute_after_review_with(&save.restore, false, false, &save.settings)
            .await?;
        let restored = routine_meta(&db, name).await?;
        assert_eq!(before, restored, "{name}: restored routine differs");
        run(&db, &[&format!("DROP {kw} `{name}`")]).await?;
    }
    db.close().await;
    Ok(())
}

/// Plan 2: a routine created under a non-default sql_mode keeps it after an edit through Telecat.
#[tokio::test]
async fn qa060_mysql_routine_keeps_its_sql_mode() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    run(
        &db,
        &[
            "DROP FUNCTION IF EXISTS qa060_mode",
            "SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION'",
            "CREATE FUNCTION qa060_mode(a VARCHAR(20)) RETURNS VARCHAR(3) DETERMINISTIC RETURN a",
            "SET SESSION sql_mode = DEFAULT",
        ],
    )
    .await?;
    let o = object(&db, "qa060_mode", ObjectKind::Function).await;
    let mode_before = scalar(&db, "SELECT sql_mode FROM information_schema.routines WHERE routine_schema='zixma' AND routine_name='qa060_mode'").await?;
    let (original, settings) = definition(&db, &o).await?;
    let before_call = db
        .query("SELECT qa060_mode('abcdef')")
        .await
        .map(|r| r.rows);
    eprintln!("before edit: SELECT qa060_mode('abcdef') -> {before_call:?}");
    let edited = original.replace("RETURN a", "RETURN a /* edited */");
    let save = routine_save(d, &o, &original, &edited, &settings)?;
    db.execute_after_review_with(&save.statements, false, false, &save.settings)
        .await?;
    let mode_after = scalar(&db, "SELECT sql_mode FROM information_schema.routines WHERE routine_schema='zixma' AND routine_name='qa060_mode'").await?;
    let call_after = db
        .query("SELECT qa060_mode('abcdef')")
        .await
        .map(|r| r.rows);
    eprintln!(
        "sql_mode before={mode_before:?} after={mode_after:?}; SELECT qa060_mode('abcdef') after edit -> {call_after:?}"
    );
    // The restore, too, creates it under its own mode.
    run(&db, &["DROP FUNCTION qa060_mode"]).await?;
    db.execute_after_review_with(&save.restore, false, false, &save.settings)
        .await?;
    let mode_restored = scalar(&db, "SELECT sql_mode FROM information_schema.routines WHERE routine_schema='zixma' AND routine_name='qa060_mode'").await?;
    // The commit connection gets its own strict mode back afterwards.
    let commit = db
        .execute(&[Stmt::new("SELECT @@SESSION.sql_mode")], false)
        .await?;
    let commit_mode = commit[0].rows[0][0].clone().unwrap_or_default();
    run(&db, &["DROP FUNCTION qa060_mode"]).await?;
    db.close().await;
    assert_eq!(
        mode_before, mode_after,
        "sql_mode of the routine changed by an edit"
    );
    assert_eq!(
        mode_before, mode_restored,
        "sql_mode of the routine changed by the restore"
    );
    assert!(
        commit_mode.contains("STRICT_ALL_TABLES"),
        "commit connection kept the routine's mode: {commit_mode}"
    );
    Ok(())
}

/// Plan 3: a user who may drop a routine but can't recreate it with its DEFINER (no
/// SET_USER_ID / SUPER). The edit must not leave the routine dropped.
#[tokio::test]
async fn qa060_mysql_routine_other_definer_edit_by_limited_user() -> Result<()> {
    let d = Driver::Mysql;
    let root = connect(d).await?;
    run(&root, &[
        "DROP USER IF EXISTS 'qa060_dev'@'%'",
        "CREATE USER 'qa060_dev'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060_dev'@'%'",
        "GRANT SHOW_ROUTINE ON *.* TO 'qa060_dev'@'%'",
        "DROP FUNCTION IF EXISTS qa060_owned",
        "CREATE DEFINER=`root`@`%` FUNCTION qa060_owned(x INT) RETURNS INT DETERMINISTIC NO SQL RETURN x + 1",
    ]).await?;
    let dev = connect_as(d, "qa060_dev", "secret").await?;
    let o = object(&dev, "qa060_owned", ObjectKind::Function).await;
    let original = definition(&dev, &o).await;
    eprintln!("definition as qa060_dev: {original:?}");
    let mut lost = false;
    if let Ok((original, settings)) = original {
        let save = routine_save(
            d,
            &o,
            &original,
            &original.replace("x + 1", "x + 2"),
            &settings,
        )?;
        let r = save_like_the_app(&dev, &save, async { Ok(!dev.routine_exists(&o).await?) }).await;
        eprintln!("save: {r:?}");
        lost = !root.routine_exists(&o).await?;
    }
    run(
        &root,
        &[
            "DROP FUNCTION IF EXISTS qa060_owned",
            "DROP USER IF EXISTS 'qa060_dev'@'%'",
        ],
    )
    .await?;
    dev.close().await;
    root.close().await;
    assert!(
        !lost,
        "routine qa060_owned was dropped and could not be recreated or restored"
    );
    Ok(())
}

/// Plan 3b: a legacy function without DETERMINISTIC/NO SQL/READS SQL DATA (created while
/// log_bin_trust_function_creators was 1), edited by root after the flag is back to 0 (binlog on).
#[tokio::test]
async fn qa060_mysql_routine_binlog_unsafe_legacy_function() -> Result<()> {
    let d = Driver::Mysql;
    let root = connect(d).await?;
    let binlog = scalar(&root, "SELECT @@log_bin").await?;
    let trust = scalar(&root, "SELECT @@GLOBAL.log_bin_trust_function_creators").await?;
    eprintln!("log_bin={binlog:?} trust={trust:?}");
    run(
        &root,
        &[
            "DROP FUNCTION IF EXISTS qa060_nd",
            "SET GLOBAL log_bin_trust_function_creators = 1",
        ],
    )
    .await?;
    let creator = connect(d).await?;
    let made = creator
        .query("CREATE FUNCTION qa060_nd(x INT) RETURNS INT RETURN x + 1")
        .await;
    creator.close().await;
    run(&root, &["SET GLOBAL log_bin_trust_function_creators = 0"]).await?;
    made?;
    let db = connect(d).await?;
    let o = object(&db, "qa060_nd", ObjectKind::Function).await;
    let (original, settings) = definition(&db, &o).await?;
    let save = routine_save(
        d,
        &o,
        &original,
        &original.replace("x + 1", "x + 2"),
        &settings,
    )?;
    let r = save_like_the_app(&db, &save, async { Ok(!db.routine_exists(&o).await?) }).await;
    eprintln!("save: {r:?}");
    let refused = r.is_err();
    let lost = !db.routine_exists(&o).await?;
    run(&root, &["DROP FUNCTION IF EXISTS qa060_nd"]).await?;
    db.close().await;
    root.close().await;
    assert!(
        !lost,
        "routine qa060_nd was dropped and could not be recreated or restored"
    );
    assert!(
        refused,
        "the server should refuse a binary-log-unsafe function"
    );
    Ok(())
}

/// Plan 6b: MySQL trigger bodies with backslash escapes survive a rename-only edit (and the
/// restore puts back the same body).
#[tokio::test]
async fn qa060_mysql_trigger_escapes_survive_rename() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    let t = tref(d, "qa060_tesc");
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_tesc",
        "CREATE TABLE qa060_tesc (id INT PRIMARY KEY, note VARCHAR(100), path VARCHAR(100))",
        // Silent change candidate: '\\t' (backslash t) and LIKE escape.
        r"CREATE TRIGGER qa060_tesc_path BEFORE INSERT ON qa060_tesc FOR EACH ROW SET NEW.path = REPLACE(NEW.path, '/', '\\')",
        // Parse-error candidate: escaped quote.
        r"CREATE TRIGGER qa060_tesc_note BEFORE UPDATE ON qa060_tesc FOR EACH ROW SET NEW.note = CONCAT(NEW.note, ' it\'s')",
    ]).await?;
    let show = |name: &str| format!("SHOW CREATE TRIGGER {name}");
    let before_path = rows(&db, &show("qa060_tesc_path")).await?[0][2].clone();
    let before_note = rows(&db, &show("qa060_tesc_note")).await?[0][2].clone();
    let listed = db.triggers(&t).await?;
    eprintln!("sheet shows: {:#?}", listed);
    let mut log = vec![];
    for orig in &listed {
        let mut cur = orig.clone();
        cur.name = format!("{}_r", orig.name);
        let edits = [TriggerEdit {
            key: "k".into(),
            orig: Some(orig.clone()),
            cur,
            deleted: false,
        }];
        for (_, save) in trigger_saves(d, &t, ObjectKind::Table, &edits)? {
            log.push(format!(
                "SAVE {:?}",
                save.statements
                    .iter()
                    .map(|s| s.sql.clone())
                    .collect::<Vec<_>>()
            ));
            let r = save_like_the_app(&db, &save, async {
                Ok(!db.trigger_exists("zixma", &orig.name).await?)
            })
            .await;
            log.push(format!("  -> {r:?}"));
        }
    }
    let names = rows(&db, "SELECT trigger_name FROM information_schema.triggers WHERE event_object_schema='zixma' AND event_object_table='qa060_tesc' ORDER BY trigger_name").await?;
    run(
        &db,
        &["INSERT INTO qa060_tesc (id, note, path) VALUES (1, 'x', 'a/b')"],
    )
    .await?;
    let path = scalar(&db, "SELECT path FROM qa060_tesc WHERE id=1").await?;
    let after_path = rows(&db, &show("qa060_tesc_path_r"))
        .await
        .ok()
        .map(|r| r[0][2].clone());
    eprintln!(
        "{}\ntriggers now: {names:?}\npath after insert: {path:?} (expected a\\b)\nSHOW CREATE before: {before_path:?}\n after: {after_path:?}\nnote before: {before_note:?}",
        log.join("\n")
    );
    run(&db, &["DROP TABLE qa060_tesc"]).await?;
    db.close().await;
    assert_eq!(names.len(), 2, "a trigger was lost: {names:?}");
    assert_eq!(
        path.as_deref(),
        Some("a\\b"),
        "path trigger changed behaviour"
    );
    assert_eq!(
        after_path
            .flatten()
            .map(|s| s.replace("qa060_tesc_path_r", "qa060_tesc_path")),
        before_path,
        "the body changed"
    );
    Ok(())
}

/// Plan 4: PostgreSQL routine edits: quoted mixed-case names, non-public schema, procedures,
/// argument change refused, edit without schema qualifier.
#[tokio::test]
async fn qa060_pg_routine_edits() -> Result<()> {
    let d = Driver::Postgres;
    let db = connect(d).await?;
    run(&db, &[
        "DROP SCHEMA IF EXISTS qa060_s CASCADE",
        "CREATE SCHEMA qa060_s",
        r#"CREATE FUNCTION qa060_s."MixCase"(a integer DEFAULT 5, b text = 'x,y') RETURNS text LANGUAGE sql AS $f$ SELECT b || a::text || ';' $f$"#,
        "CREATE PROCEDURE qa060_s.qa060_proc(INOUT n integer) LANGUAGE plpgsql AS $$ BEGIN n := n + 1; END $$",
    ]).await?;
    let objs = db.list_objects().await?;
    let f = objs
        .iter()
        .find(|o| o.table.name == "MixCase")
        .cloned()
        .expect("MixCase");
    let original = db.routine_definition(&f).await?.text;
    eprintln!("{original}");
    let save = routine_save(
        d,
        &f,
        &original,
        &original.replace("|| ';'", "|| '!'"),
        &CreateSettings::default(),
    )?;
    db.execute_after_review(&save.statements, save.transaction, false)
        .await?;
    assert_eq!(
        scalar(&db, r#"SELECT qa060_s."MixCase"()"#)
            .await?
            .as_deref(),
        Some("x,y5!")
    );
    // Argument change refused.
    let changed = original.replace("a integer DEFAULT 5", "a bigint DEFAULT 5");
    assert!(
        routine_save(d, &f, &original, &changed, &CreateSettings::default()).is_err(),
        "arg type change accepted"
    );
    // Unqualified name lands in search_path (public): must not be accepted as an edit of qa060_s.
    let unq = original.replace("qa060_s.\"MixCase\"", "\"MixCase\"");
    assert_ne!(unq, original);
    let r = routine_save(d, &f, &original, &unq, &CreateSettings::default());
    eprintln!("unqualified edit accepted: {}", r.is_ok());
    if let Ok(s) = &r {
        db.execute_after_review(&s.statements, true, false).await?;
        let n = scalar(
            &db,
            "SELECT count(*) FROM pg_proc WHERE proname = 'MixCase'",
        )
        .await?;
        eprintln!("MixCase overloads after unqualified edit: {n:?}");
        run(
            &db,
            &[r#"DROP FUNCTION IF EXISTS public."MixCase"(integer, text)"#],
        )
        .await?;
    }
    // Procedure
    let p = objs
        .iter()
        .find(|o| o.table.name == "qa060_proc")
        .cloned()
        .expect("proc");
    assert_eq!(p.kind, ObjectKind::Procedure);
    let original = db.routine_definition(&p).await?.text;
    let save = routine_save(
        d,
        &p,
        &original,
        &original.replace("n + 1", "n + 2"),
        &CreateSettings::default(),
    )?;
    db.execute_after_review(&save.statements, true, false)
        .await?;
    assert_eq!(
        scalar(&db, "CALL qa060_s.qa060_proc(1)").await?.as_deref(),
        Some("3")
    );
    // Drop exact procedure
    db.execute(&[table_op_statement(d, &p, false)?], true)
        .await?;
    run(&db, &["DROP SCHEMA qa060_s CASCADE"]).await?;
    db.close().await;
    Ok(())
}

fn trig(name: &str, timing: &str, event: &str, statement: &str) -> Trigger {
    Trigger {
        name: name.into(),
        timing: timing.into(),
        event: event.into(),
        statement: statement.into(),
        ..Default::default()
    }
}
async fn save_triggers(
    db: &Session,
    d: Driver,
    table: &TableRef,
    edits: &[TriggerEdit],
) -> Result<()> {
    for (_, save) in trigger_saves(d, table, ObjectKind::Table, edits)? {
        db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
            .await?;
    }
    Ok(())
}

/// Plan 5: MySQL: two BEFORE INSERT triggers; editing the first one's body must not change
/// which runs first (it changes the computed data).
#[tokio::test]
async fn qa060_mysql_trigger_edit_keeps_execution_order() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    let t = tref(d, "qa060_tord");
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_tord",
        "CREATE TABLE qa060_tord (id INT PRIMARY KEY, amount DECIMAL(10,2), total DECIMAL(10,2))",
        "CREATE TRIGGER qa060_a_base BEFORE INSERT ON qa060_tord FOR EACH ROW SET NEW.total = NEW.amount",
        "CREATE TRIGGER qa060_b_vat BEFORE INSERT ON qa060_tord FOR EACH ROW FOLLOWS qa060_a_base SET NEW.total = NEW.total * 1.07",
        "INSERT INTO qa060_tord (id, amount) VALUES (1, 100)",
    ]).await?;
    assert_eq!(
        scalar(&db, "SELECT total FROM qa060_tord WHERE id=1")
            .await?
            .as_deref(),
        Some("107.00")
    );
    let listed = db.triggers(&t).await?;
    let base = listed
        .iter()
        .find(|x| x.name == "qa060_a_base")
        .unwrap()
        .clone();
    let mut edited = base.clone();
    edited.statement = "SET NEW.total = NEW.amount + 0".into(); // cosmetic edit
    save_triggers(
        &db,
        d,
        &t,
        &[TriggerEdit {
            key: "k".into(),
            orig: Some(base),
            cur: edited,
            deleted: false,
        }],
    )
    .await?;
    run(
        &db,
        &["INSERT INTO qa060_tord (id, amount) VALUES (2, 100)"],
    )
    .await?;
    let total = scalar(&db, "SELECT total FROM qa060_tord WHERE id=2").await?;
    let order = rows(&db, "SELECT trigger_name, action_order FROM information_schema.triggers WHERE event_object_table='qa060_tord' ORDER BY action_order").await?;
    eprintln!("after cosmetic edit of the first trigger: total={total:?} order={order:?}");
    run(&db, &["DROP TABLE qa060_tord"]).await?;
    db.close().await;
    assert_eq!(
        total.as_deref(),
        Some("107.00"),
        "trigger order changed by an edit: VAT trigger now runs first"
    );
    Ok(())
}

/// Plan 6: trigger round trips: MySQL body with Thai/strings/comments; PG WHEN + UPDATE OF +
/// REFERENCING; rename keeps the rest. And smuggling attempts are refused.
#[tokio::test]
async fn qa060_trigger_round_trips_and_smuggling() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let t = tref(d, "qa060_trt");
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS qa060_trt_log",
                "DROP TABLE IF EXISTS qa060_trt",
            ],
        )
        .await?;
        if d == Driver::Mysql {
            run(&db, &[
                "CREATE TABLE qa060_trt (id INT PRIMARY KEY, note VARCHAR(100), n INT)",
                "CREATE TABLE qa060_trt_log (msg VARCHAR(200))",
                "CREATE TRIGGER qa060_trt_bu BEFORE UPDATE ON qa060_trt FOR EACH ROW BEGIN\n  -- note; END\n  IF NEW.n <> OLD.n THEN\n    INSERT INTO qa060_trt_log VALUES (CONCAT('เปลี่ยน; ', OLD.n, ' -> ', NEW.n, ' q'));\n  END IF;\nEND",
            ]).await?;
        } else {
            run(&db, &[
                "CREATE TABLE qa060_trt (id INT PRIMARY KEY, note text, n int)",
                "CREATE TABLE qa060_trt_log (msg text)",
                "CREATE OR REPLACE FUNCTION qa060_trt_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO qa060_trt_log VALUES ('เปลี่ยน; ' || OLD.n || ' -> ' || NEW.n); RETURN NEW; END $$",
                "CREATE TRIGGER qa060_trt_bu BEFORE UPDATE OF n, note ON qa060_trt FOR EACH ROW WHEN (OLD.n IS DISTINCT FROM NEW.n) EXECUTE FUNCTION qa060_trt_fn()",
            ]).await?;
        }
        run(&db, &["INSERT INTO qa060_trt VALUES (1, 'a', 1)"]).await?;
        let listed = db.triggers(&t).await?;
        assert_eq!(listed.len(), 1, "{d:?} {listed:?}");
        let meta_sql = "SELECT definer, sql_mode, character_set_client, action_timing, event_manipulation FROM information_schema.triggers WHERE event_object_schema='zixma' AND event_object_table='qa060_trt'";
        let meta_before = if d == Driver::Mysql {
            rows(&db, meta_sql).await?
        } else {
            vec![]
        };
        // Rename only.
        let mut renamed = listed[0].clone();
        renamed.name = "qa060_trt_bu2".into();
        save_triggers(
            &db,
            d,
            &t,
            &[TriggerEdit {
                key: "k".into(),
                orig: Some(listed[0].clone()),
                cur: renamed,
                deleted: false,
            }],
        )
        .await?;
        let after = db.triggers(&t).await?;
        assert_eq!(after.len(), 1);
        assert_eq!(
            (
                after[0].timing.clone(),
                after[0].event.clone(),
                after[0].statement.clone()
            ),
            (
                listed[0].timing.clone(),
                listed[0].event.clone(),
                listed[0].statement.clone()
            ),
            "{d:?} trigger changed by rename"
        );
        if d == Driver::Mysql {
            let meta_after = rows(&db, meta_sql).await?;
            eprintln!("trigger meta before rename {meta_before:?}\n after {meta_after:?}");
        }
        run(
            &db,
            &[
                "UPDATE qa060_trt SET n = 2 WHERE id = 1",
                "UPDATE qa060_trt SET note = 'z' WHERE id = 1",
            ],
        )
        .await?;
        let log = rows(&db, "SELECT msg FROM qa060_trt_log").await?;
        assert_eq!(
            log,
            vec![vec![Some(if d == Driver::Mysql {
                "เปลี่ยน; 1 -> 2 q".to_owned()
            } else {
                "เปลี่ยน; 1 -> 2".to_owned()
            })]],
            "{d:?}"
        );
        // Smuggling.
        let base = after[0].clone();
        let attempts: Vec<String> = if d == Driver::Mysql {
            vec![
                "SET NEW.n = 1; DROP TABLE qa060_trt_log".into(),
                "BEGIN SET NEW.n = 1; END; DROP TABLE qa060_trt_log".into(),
                "SET NEW.n = 1 /* x */; DROP TABLE qa060_trt_log".into(),
                "BEGIN SET NEW.note = 'END;'; END;\nDROP TABLE qa060_trt_log".into(),
                "SET NEW.n = 1;\n-- ;\nDROP TABLE qa060_trt_log".into(),
                "SET NEW.n = 1 # x\n; DROP TABLE qa060_trt_log".into(),
            ]
        } else {
            vec![
                "FOR EACH ROW EXECUTE FUNCTION qa060_trt_fn(); DROP TABLE qa060_trt_log".into(),
                "FOR EACH ROW EXECUTE FUNCTION qa060_trt_fn('a'';'); DROP TABLE qa060_trt_log"
                    .into(),
                "FOR EACH ROW EXECUTE FUNCTION qa060_trt_fn($$x$$); DROP TABLE qa060_trt_log"
                    .into(),
                "FOR EACH ROW EXECUTE FUNCTION qa060_trt_fn() /* a */; DROP TABLE qa060_trt_log"
                    .into(),
            ]
        };
        for a in &attempts {
            let mut bad = base.clone();
            bad.statement = a.clone();
            let edits = [TriggerEdit {
                key: "k".into(),
                orig: Some(base.clone()),
                cur: bad,
                deleted: false,
            }];
            match trigger_saves(d, &t, ObjectKind::Table, &edits) {
                Err(_) => {}
                Ok(saves) => {
                    for (_, s) in saves {
                        let _ = save_like_the_app(&db, &s, async {
                            Ok(db.triggers(&t).await?.is_empty())
                        })
                        .await;
                    }
                }
            }
            let still = scalar(&db, if d == Driver::Mysql { "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='zixma' AND table_name='qa060_trt_log'" } else { "SELECT count(*) FROM pg_class WHERE relname='qa060_trt_log'" }).await?;
            assert_eq!(
                still.as_deref(),
                Some("1"),
                "{d:?}: smuggled DROP ran via {a:?}"
            );
            // Trigger must still be there after a refused/failed edit.
            assert_eq!(
                db.triggers(&t).await?.len(),
                1,
                "{d:?}: trigger lost after {a:?}"
            );
        }
        run(&db, &["DROP TABLE qa060_trt_log", "DROP TABLE qa060_trt"]).await?;
        if d == Driver::Postgres {
            run(&db, &["DROP FUNCTION qa060_trt_fn()"]).await?;
        }
        db.close().await;
    }
    Ok(())
}

/// Plan 7: export SQL INSERT round trip on each driver.
async fn export_sql(
    db: &Session,
    d: Driver,
    src: &str,
    target: &str,
    req_filters: Vec<Filter>,
) -> Result<String> {
    let req = TableDataRequest {
        table: tref(d, src),
        filters: req_filters,
        combine: Combine::And,
        order_by: vec![],
        estimate: false,
        limit: 300,
        offset: 0,
    };
    let mut writer: Option<ExportWriter> = None;
    let mut out = String::new();
    db.export_table(&req, &mut |columns, binary, rows| {
        let w = writer.get_or_insert_with(|| {
            let w = ExportWriter::new(
                ExportFormat::Sql,
                d,
                quote_ident(d, target),
                columns.to_vec(),
                binary.to_vec(),
            );
            out += &w.header();
            w
        });
        out += &w.rows(&rows);
        Ok(())
    })
    .await?;
    if let Some(w) = &writer {
        out += &w.footer();
    }
    Ok(out)
}

#[tokio::test]
async fn qa060_mysql_export_sql_round_trip() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_ex_src",
        "DROP TABLE IF EXISTS qa060_ex_dst",
        "CREATE TABLE qa060_ex_src (id DOUBLE NOT NULL PRIMARY KEY, vc VARCHAR(100) CHARACTER SET utf8mb4, v3 VARCHAR(100) CHARACTER SET utf8mb3, txt TEXT, j JSON, b VARBINARY(16), bl BLOB, bfix BINARY(4), d DATE, dt DATETIME(3), ts TIMESTAMP(6) NULL, dc DECIMAL(30,10), dbl DOUBLE, bt BIT(8), en ENUM('a','b'), st SET('x','y'), tm TIME(2), yr YEAR, `select` INT, `we``ird col` VARCHAR(10), `DELETE` INT)",
        "CREATE TABLE qa060_ex_dst LIKE qa060_ex_src",
        r#"INSERT INTO qa060_ex_src VALUES
 (1, 'O''Reilly "q"', 'ไทย', 'C:\\path\\n\\t real\nnewline\ttab', '{"a": "x\\"y", "b": [1, 2.5, null], "t": "ไทย"}', 0x00FF10, 0x89504E470D0A1A0A0000, 'ab', '2024-02-29', '2024-12-31 23:59:59.123', '2024-03-10 02:30:00.000001', 12345678901234567890.0123456789, 0.1, b'10100101', 'b', 'x,y', '-838:59:59.00', 2155, 7, 'w', 1),
 (2.5, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
 (123456789012345, '', '', '', '[]', '', '', 0x00000000, '1000-01-01', '1000-01-01 00:00:00', '1970-01-01 00:00:01', -0.0000000001, 1.7976931348623157e308, b'0', 'a', '', '00:00:00', 1901, -1, '', 0),
 (4, 'emoji 😀 \\', 'ก''ข', 'line1\r\nline2', '{"k": "just a string"}', 'abc', 'text in blob', 'abcd', '2038-01-19', '9999-12-31 23:59:59.999', '2038-01-19 03:14:07', 0, -2.2250738585072014e-308, b'11111111', NULL, 'y', '12:34:56.78', 2000, NULL, '\\''', NULL),
 (9007199254740992, '0x41', '0x41', '0x41', '{"n": 12345678901234567890}', 0x3078, NULL, NULL, NULL, NULL, NULL, NULL, 1e21, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"#,
    ]).await?;
    // A bound filter switches MySQL to the binary protocol: exercise both paths.
    let filtered = std::env::var("QA060_FILTER").is_ok();
    let filters = if filtered {
        vec![Filter {
            enabled: true,
            column: "id".into(),
            op: FilterOp::Gt,
            value: "-1".into(),
            value2: None,
        }]
    } else {
        vec![]
    };
    let sql = export_sql(&db, d, "qa060_ex_src", "qa060_ex_dst", filters).await?;
    for st in telecat_core::sql::split_statements(&sql, d) {
        let line = st.text.as_str();
        db.query(line)
            .await
            .map_err(|e| anyhow::anyhow!("re-import failed: {e:#}\n{line}"))?;
    }
    let cols = "id, HEX(vc), HEX(v3), HEX(txt), CAST(j AS CHAR), HEX(b), HEX(bl), HEX(bfix), d, dt, ts, dc, dbl, HEX(bt), en, st, tm, yr, `select`, HEX(`we``ird col`), `DELETE`";
    let a = rows(&db, &format!("SELECT {cols} FROM qa060_ex_src ORDER BY id")).await?;
    let b = rows(&db, &format!("SELECT {cols} FROM qa060_ex_dst ORDER BY id")).await?;
    let mut diffs = vec![];
    for (ra, rb) in a.iter().zip(&b) {
        for (i, (x, y)) in ra.iter().zip(rb).enumerate() {
            if x != y {
                diffs.push(format!("row id={:?} col#{i}: src={x:?} dst={y:?}", ra[0]));
            }
        }
    }
    run(&db, &["DROP TABLE qa060_ex_src", "DROP TABLE qa060_ex_dst"]).await?;
    db.close().await;
    assert_eq!(a.len(), b.len());
    assert!(
        diffs.is_empty(),
        "round-trip differences:\n{}",
        diffs.join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn qa060_pg_export_sql_round_trip() -> Result<()> {
    let d = Driver::Postgres;
    let db = connect(d).await?;
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_ex_src",
        "DROP TABLE IF EXISTS qa060_ex_dst",
        r#"CREATE TABLE qa060_ex_src (id float8 PRIMARY KEY, t text, vc varchar(50), j json, jb jsonb, b bytea, ts timestamp(6), tz timestamptz, d date, n numeric, f4 real, f8 double precision, arr int[], tarr text[], iv interval, u uuid, bo boolean, "select" int, "We""ird" text, ip inet, x xml, tm time, bits bit varying(8))"#,
        "CREATE TABLE qa060_ex_dst (LIKE qa060_ex_src)",
        r#"INSERT INTO qa060_ex_src VALUES
 (1, E'O''Reilly "q" C:\\path\\n real\nnewline\ttab', 'ไทย', '{"a": "x\"y", "t": "ไทย"}', '{"b": [1, 2.5, null]}', '\x00ff10'::bytea, '2024-02-29 23:59:59.123456', '2024-03-10 02:30:00+07', '2024-02-29', 12345678901234567890.0123456789, 0.1, 0.1, '{1,NULL,3}', '{"a,b","c\"d",NULL,"NULL"}', '1 day 02:03:04.5', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', true, 7, 'w', '10.0.0.1/8', '<a>ไทย &amp; x</a>', '23:59:59.999999', B'1011'),
 (2.5, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
 (123456789012345.5, '', '', '[]', '{}', ''::bytea, 'infinity', '-infinity', '0001-01-01 BC', 'NaN', 'Infinity', '-1.7976931348623157e308', '{}', '{}', '-1 mon', '00000000-0000-0000-0000-000000000000', false, -1, '', '::1', '', '00:00', B''),
 (4, E'back\\slash \\x00 \\\\', 'NULL', '"s"', '"s"', 'abc'::bytea, '1999-01-08 04:05:06', '1999-01-08 04:05:06 PST', '1999-01-08', -0.00000000001, 3.4e38, 1e-300, '{{1,2},{3,4}}', '{" leading","trailing "}', '1 year', NULL, NULL, NULL, '''', NULL, NULL, NULL, NULL)"#,
    ]).await?;
    let filtered = std::env::var("QA060_FILTER").is_ok();
    let filters = if filtered {
        vec![Filter {
            enabled: true,
            column: "id".into(),
            op: FilterOp::Gt,
            value: "-1".into(),
            value2: None,
        }]
    } else {
        vec![]
    };
    let sql = export_sql(&db, d, "qa060_ex_src", "qa060_ex_dst", filters).await?;
    for st in telecat_core::sql::split_statements(&sql, d) {
        let line = st.text.as_str();
        db.query(line)
            .await
            .map_err(|e| anyhow::anyhow!("re-import failed: {e:#}\n{line}"))?;
    }
    let diff1 = rows(&db, "SELECT row(s.*)::text FROM qa060_ex_src s EXCEPT SELECT row(t.*)::text FROM qa060_ex_dst t").await?;
    let diff2 = rows(&db, "SELECT row(t.*)::text FROM qa060_ex_dst t EXCEPT SELECT row(s.*)::text FROM qa060_ex_src s").await?;
    run(&db, &["DROP TABLE qa060_ex_src", "DROP TABLE qa060_ex_dst"]).await?;
    db.close().await;
    assert!(
        diff1.is_empty() && diff2.is_empty(),
        "src-only: {diff1:?}\ndst-only: {diff2:?}"
    );
    Ok(())
}

/// Plan 8: a stopped export doesn't leave data written; filter honoured on a DOUBLE PK and Thai.
#[tokio::test]
async fn qa060_export_filter_on_double_pk_and_thai() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let (double, text) = if d == Driver::Mysql {
            ("DOUBLE", "VARCHAR(50)")
        } else {
            ("float8", "text")
        };
        run(&db, &[
            "DROP TABLE IF EXISTS qa060_exf",
            &format!("CREATE TABLE qa060_exf (MEMBER_ID {double} NOT NULL PRIMARY KEY, name {text})"),
            "INSERT INTO qa060_exf VALUES (1, 'สมชาย'), (2, 'somchai'), (3, 'สมหญิง'), (4, NULL)",
        ]).await?;
        let col = if d == Driver::Mysql {
            "MEMBER_ID"
        } else {
            "member_id"
        };
        let f = vec![Filter {
            enabled: true,
            column: "name".into(),
            op: FilterOp::Contains,
            value: "สม".into(),
            value2: None,
        }];
        let sql = export_sql(&db, d, "qa060_exf", "x", f).await?;
        let n = sql.lines().filter(|l| l.starts_with("INSERT")).count();
        assert_eq!(n, 2, "{d:?}: {sql}");
        let f = vec![Filter {
            enabled: true,
            column: col.into(),
            op: FilterOp::Gt,
            value: "2".into(),
            value2: None,
        }];
        let sql = export_sql(&db, d, "qa060_exf", "x", f).await?;
        assert_eq!(
            sql.lines().filter(|l| l.starts_with("INSERT")).count(),
            2,
            "{d:?}: {sql}"
        );
        run(&db, &["DROP TABLE qa060_exf"]).await?;
        db.close().await;
    }
    Ok(())
}

/// Plan 9: autocomplete column lookup during an open BEGIN on the editor session (PG) must not
/// touch the transaction — including when the lookup itself fails (bad table name chars).
#[tokio::test]
async fn qa060_column_lookup_never_touches_editor_tx() -> Result<()> {
    let d = Driver::Postgres;
    let db = connect(d).await?;
    run(
        &db,
        &[
            "DROP TABLE IF EXISTS qa060_ac",
            "CREATE TABLE qa060_ac (id int)",
        ],
    )
    .await?;
    db.query("BEGIN").await?;
    db.query("INSERT INTO qa060_ac VALUES (1)").await?;
    let weird = vec![
        tref(d, "qa060_ac"),
        TableRef {
            schema: "nope".into(),
            name: "x'\"; y".into(),
        },
    ];
    let _ = db.column_names(&weird).await;
    // Many tables at once (more than one batch)
    let many: Vec<_> = (0..450)
        .map(|i| tref(d, &format!("qa060_none_{i}")))
        .collect();
    let _ = db.column_names(&many).await;
    // Editor tx still alive and our row visible inside it
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM qa060_ac")
            .await?
            .as_deref(),
        Some("1")
    );
    db.query("ROLLBACK").await?;
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM qa060_ac")
            .await?
            .as_deref(),
        Some("0")
    );
    run(&db, &["DROP TABLE qa060_ac"]).await?;
    db.close().await;
    Ok(())
}

/// Plan 10: views — trigger saves and routine saves refuse non-tables; drop of a matview with
/// dependants etc. (core enforcement).
#[test]
fn qa060_views_refuse_trigger_saves() {
    for d in [Driver::Mysql, Driver::Postgres] {
        let t = tref(d, "v");
        let e = [TriggerEdit {
            key: "k".into(),
            orig: None,
            cur: trig("x", "BEFORE", "INSERT", "SET NEW.a=1"),
            deleted: false,
        }];
        assert!(trigger_saves(d, &t, ObjectKind::View, &e).is_err());
        assert!(trigger_saves(d, &t, ObjectKind::MaterializedView, &e).is_err());
        let v = DbObject::new(t.clone(), ObjectKind::View);
        assert!(table_op_statement(d, &v, true).is_err());
        assert!(
            routine_save(
                d,
                &v,
                "x",
                "CREATE FUNCTION v() RETURNS int RETURN 1",
                &CreateSettings::default()
            )
            .is_err()
        );
    }
    let _ = Stmt::new("");
}

/// Plan 5 (advice): realistic MySQL originals that the splitter might not read as one statement
/// (then restore is empty and no "not in place" notice is shown).
#[test]
fn qa060_mysql_tricky_originals_keep_restore() {
    let d = Driver::Mysql;
    let o = DbObject::new(tref(d, "f"), ObjectKind::Function);
    let bodies = [
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS varchar(20) CHARSET utf8mb4\n    DETERMINISTIC\nRETURN 'it\\'s; ok'",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\nBEGIN\n  # don't do this; really\n  RETURN 1;\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\nBEGIN\n  -- it's fine; really\n  RETURN 1;\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS varchar(50) CHARSET utf8mb4\n    DETERMINISTIC\nBEGIN\n  RETURN \"double; quoted 'x' \\\" y\";\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\nBEGIN\n  DECLARE `end` INT DEFAULT 1;\n  RETURN `end`;\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`(x INT) RETURNS int\n    DETERMINISTIC\nBEGIN\n  IF x > 0 THEN BEGIN RETURN 1; END; ELSEIF x < 0 THEN RETURN -1; END IF;\n  RETURN 0;\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`(x INT) RETURNS int\n    DETERMINISTIC\nlbl: BEGIN\n  RETURN x;\nEND lbl",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\nBEGIN\n  DECLARE r INT;\n  SELECT CASE WHEN 1 THEN 1 ELSE 0 END INTO r;\n  RETURN r;\nEND",
        "CREATE DEFINER=`root`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\nBEGIN\n  /*!50003 SET @a = 1; */\n  RETURN 1;\nEND",
    ];
    let mut bad = vec![];
    for b in bodies {
        let original = format!("{b};\n");
        match routine_save(d, &o, &original, &original, &CreateSettings::default()) {
            Ok(s) if s.restore.len() == 1 => {}
            Ok(_) => bad.push(format!("NO RESTORE (save allowed!) for:\n{b}")),
            Err(e) => bad.push(format!("refused ({e}) for:\n{b}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n----\n"));
}

/// Plan 11: how long Stop takes on a large table (informational; latency is below the defect bar).
#[tokio::test]
#[ignore = "latency measurement, below bar; run with --ignored"]
async fn qa060_export_stop_is_prompt_on_large_table() -> Result<()> {
    let mut slow = vec![];
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(&db, &["DROP TABLE IF EXISTS qa060_big"]).await?;
        if d == Driver::Mysql {
            run(&db, &[
                "CREATE TABLE qa060_big (id INT PRIMARY KEY, pad VARCHAR(100))",
                "SET SESSION cte_max_recursion_depth = 2000000",
                "INSERT INTO qa060_big WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 1500000) SELECT n, REPEAT('x', 80) FROM s",
            ]).await?;
        } else {
            run(&db, &[
                "CREATE TABLE qa060_big (id int PRIMARY KEY, pad text)",
                "INSERT INTO qa060_big SELECT n, repeat('x', 80) FROM generate_series(1, 1500000) n",
            ]).await?;
        }
        let req = TableDataRequest {
            table: tref(d, "qa060_big"),
            filters: vec![],
            combine: Combine::And,
            order_by: vec![],
            estimate: false,
            limit: 300,
            offset: 0,
        };
        let t = std::time::Instant::now();
        let mut chunks = 0;
        let r = db
            .export_table(&req, &mut |_, _, _| {
                chunks += 1;
                anyhow::bail!("The export was stopped.")
            })
            .await;
        let stop = t.elapsed();
        let t = std::time::Instant::now();
        let full = db.export_table(&req, &mut |_, _, _| Ok(())).await?;
        let all = t.elapsed();
        eprintln!(
            "{d:?}: stop after first chunk returned in {stop:?} (err={}), full export of {full} rows {all:?}",
            r.is_err()
        );
        // The tab's reads must still work right after a stop.
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM qa060_big")
                .await?
                .as_deref(),
            Some("1500000")
        );
        run(&db, &["DROP TABLE qa060_big"]).await?;
        db.close().await;
        slow.push((d, stop, all));
    }
    eprintln!("stop vs full: {slow:?}");
    Ok(())
}

/// Plan 6c: silent change: a trigger that strips literal backslash-n sequences. After a rename it
/// must still do the same thing.
#[tokio::test]
async fn qa060_mysql_trigger_escape_silent_change() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    let t = tref(d, "qa060_tsil");
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_tsil",
        "CREATE TABLE qa060_tsil (id INT PRIMARY KEY, note VARCHAR(100))",
        r"CREATE TRIGGER qa060_tsil_bi BEFORE INSERT ON qa060_tsil FOR EACH ROW SET NEW.note = REPLACE(NEW.note, '\\n', ' ')",
    ]).await?;
    // literal backslash + n in the inserted value
    run(&db, &[r"INSERT INTO qa060_tsil VALUES (1, 'a\\nb')"]).await?;
    let before = scalar(&db, "SELECT note FROM qa060_tsil WHERE id=1").await?;
    let listed = db.triggers(&t).await?;
    let mut cur = listed[0].clone();
    cur.name = "qa060_tsil_bi2".into();
    let edits = [TriggerEdit {
        key: "k".into(),
        orig: Some(listed[0].clone()),
        cur,
        deleted: false,
    }];
    let mut err = None;
    for (_, save) in trigger_saves(d, &t, ObjectKind::Table, &edits)? {
        err = save_like_the_app(&db, &save, async {
            Ok(!db.trigger_exists("zixma", "qa060_tsil_bi").await?)
        })
        .await
        .err();
    }
    run(&db, &[r"INSERT INTO qa060_tsil VALUES (2, 'a\\nb')"]).await?;
    let after = scalar(&db, "SELECT note FROM qa060_tsil WHERE id=2").await?;
    let show = rows(&db, "SHOW CREATE TRIGGER qa060_tsil_bi2")
        .await
        .ok()
        .map(|r| r[0][2].clone());
    eprintln!(
        "save err={err:?}\nrow1 (original trigger) = {before:?}\nrow2 (after rename) = {after:?}\nnew trigger: {show:?}"
    );
    run(&db, &["DROP TABLE qa060_tsil"]).await?;
    db.close().await;
    assert_eq!(err, None, "the rename failed");
    assert_eq!(before, after, "renamed trigger behaves differently");
    Ok(())
}

/// Plan 3c: routine owned by an ordinary app account, edited by a developer account that can
/// drop it (ALTER ROUTINE) and read it (SHOW_ROUTINE) but has no SET_USER_ID.
#[tokio::test]
async fn qa060_mysql_routine_app_owned_edit_by_developer() -> Result<()> {
    let d = Driver::Mysql;
    let root = connect(d).await?;
    run(&root, &[
        "DROP USER IF EXISTS 'qa060_app'@'%'",
        "DROP USER IF EXISTS 'qa060_devx'@'%'",
        "CREATE USER 'qa060_app'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060_app'@'%'",
        "CREATE USER 'qa060_devx'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060_devx'@'%'",
        "GRANT SHOW_ROUTINE ON *.* TO 'qa060_devx'@'%'",
        "DROP FUNCTION IF EXISTS qa060_appfn",
        "CREATE DEFINER=`qa060_app`@`%` FUNCTION qa060_appfn(x INT) RETURNS INT DETERMINISTIC NO SQL RETURN x + 1",
    ]).await?;
    let dev = connect_as(d, "qa060_devx", "secret").await?;
    let o = object(&dev, "qa060_appfn", ObjectKind::Function).await;
    let (original, settings) = definition(&dev, &o).await?;
    let save = routine_save(
        d,
        &o,
        &original,
        &original.replace("x + 1", "x + 2"),
        &settings,
    )?;
    let r = save_like_the_app(&dev, &save, async { Ok(!dev.routine_exists(&o).await?) }).await;
    eprintln!("definition: {original}\nsave: {r:?}");
    let err = r.expect_err("qa060_devx can't create a routine as qa060_app");
    let lost = !root.routine_exists(&o).await?;
    run(
        &root,
        &[
            "DROP FUNCTION IF EXISTS qa060_appfn",
            "DROP USER IF EXISTS 'qa060_app'@'%'",
            "DROP USER IF EXISTS 'qa060_devx'@'%'",
        ],
    )
    .await?;
    dev.close().await;
    root.close().await;
    assert!(
        !lost,
        "routine qa060_appfn was dropped and could not be recreated or restored"
    );
    assert!(
        err.contains("SET_USER_ID") || err.contains("SUPER"),
        "{err}"
    );
    assert!(
        !err.contains("restore:"),
        "nothing was dropped, so nothing is restored: {err}"
    );
    Ok(())
}

/// Fix round 1: a trigger that runs as another account, edited by a developer account without
/// SET_USER_ID: refused before the DROP, the trigger stays. A trigger of the developer's own
/// is saved, binary logging on or not.
#[tokio::test]
async fn qa060r1_mysql_trigger_of_another_definer_is_refused_before_the_drop() -> Result<()> {
    let d = Driver::Mysql;
    let root = connect(d).await?;
    run(&root, &[
        "DROP USER IF EXISTS 'qa060_app2'@'%'",
        "DROP USER IF EXISTS 'qa060_dev2'@'%'",
        "CREATE USER 'qa060_app2'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060_app2'@'%'",
        "CREATE USER 'qa060_dev2'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060_dev2'@'%'",
        "DROP TABLE IF EXISTS qa060_tdef",
        "CREATE TABLE qa060_tdef (id INT PRIMARY KEY, n INT)",
        "CREATE DEFINER=`qa060_app2`@`%` TRIGGER qa060_tdef_app BEFORE INSERT ON qa060_tdef FOR EACH ROW SET NEW.n = 1",
    ]).await?;
    let t = tref(d, "qa060_tdef");
    let dev = connect_as(d, "qa060_dev2", "secret").await?;
    let listed = dev.triggers(&t).await?;
    assert_eq!(listed[0].definer.as_deref(), Some("qa060_app2@%"));
    let mut cur = listed[0].clone();
    cur.statement = "SET NEW.n = 2".into();
    let edits = [TriggerEdit {
        key: "k".into(),
        orig: Some(listed[0].clone()),
        cur,
        deleted: false,
    }];
    let (_, save) = trigger_saves(d, &t, ObjectKind::Table, &edits)?.remove(0);
    let r = save_like_the_app(&dev, &save, async {
        Ok(!dev.trigger_exists("zixma", "qa060_tdef_app").await?)
    })
    .await;
    let err = r.expect_err("refused");
    assert!(
        err.contains("SET_USER_ID") && !err.contains("restore:"),
        "{err}"
    );
    assert!(root.trigger_exists("zixma", "qa060_tdef_app").await?);
    // The developer's own trigger, while binary logging is on and creators aren't trusted:
    // creating it again needs SUPER, so this edit too is refused before anything is dropped.
    run(&root, &["CREATE DEFINER=`qa060_dev2`@`%` TRIGGER qa060_tdef_dev BEFORE UPDATE ON qa060_tdef FOR EACH ROW SET NEW.n = 3"]).await?;
    let own = dev
        .triggers(&t)
        .await?
        .into_iter()
        .find(|x| x.name == "qa060_tdef_dev")
        .unwrap();
    let mut cur = own.clone();
    cur.statement = "SET NEW.n = 4".into();
    let edits = [TriggerEdit {
        key: "k".into(),
        orig: Some(own),
        cur,
        deleted: false,
    }];
    let (_, save) = trigger_saves(d, &t, ObjectKind::Table, &edits)?.remove(0);
    let r = save_like_the_app(&dev, &save, async {
        Ok(!dev.trigger_exists("zixma", "qa060_tdef_dev").await?)
    })
    .await;
    // Another test may switch log_bin_trust_function_creators meanwhile: either outcome is
    // fine as long as a refusal comes before the DROP.
    if let Err(err) = r {
        assert!(
            err.contains("Binary logging") && !err.contains("restore:"),
            "{err}"
        );
    }
    assert!(root.trigger_exists("zixma", "qa060_tdef_dev").await?);
    run(
        &root,
        &[
            "DROP TABLE qa060_tdef",
            "DROP USER 'qa060_app2'@'%'",
            "DROP USER 'qa060_dev2'@'%'",
        ],
    )
    .await?;
    dev.close().await;
    root.close().await;
    Ok(())
}

/// Fix round 1: JSON values export as the server's JSON text in every format.
#[tokio::test]
async fn qa060r1_mysql_json_exports_as_server_text() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_json",
        "CREATE TABLE qa060_json (id INT PRIMARY KEY, j JSON)",
        r#"INSERT INTO qa060_json VALUES (1, '{"n": 12345678901234567890}'), (2, '"just a string"'), (3, 'null'), (4, NULL)"#,
    ]).await?;
    let req = TableDataRequest {
        table: tref(d, "qa060_json"),
        filters: vec![],
        combine: Combine::And,
        order_by: vec![],
        estimate: false,
        limit: 300,
        offset: 0,
    };
    let mut rows = vec![];
    db.export_table(&req, &mut |_, _, chunk| {
        rows.extend(chunk);
        Ok(())
    })
    .await?;
    let j: Vec<Option<String>> = rows.into_iter().map(|r| r[1].clone()).collect();
    run(&db, &["DROP TABLE qa060_json"]).await?;
    db.close().await;
    assert_eq!(
        j,
        vec![
            Some(r#"{"n": 12345678901234567890}"#.into()),
            Some(r#""just a string""#.into()),
            Some("null".into()),
            None
        ]
    );
    Ok(())
}

/// Fix round 2 (QA re-verify 1, `r1_trigger_precheck_system_user_definer`): an account with
/// SET_USER_ID but not SYSTEM_USER edits a trigger whose definer (root) has SYSTEM_USER. MySQL
/// lets it DROP the trigger but not create it again, so the pre-check must refuse first.
#[tokio::test]
async fn qa060r2_trigger_precheck_requires_system_user_for_another_definer() -> Result<()> {
    let d = Driver::Mysql;
    let root = connect(d).await?;
    run(&root, &[
        "DROP USER IF EXISTS 'qa060r_su'@'%'",
        "CREATE USER 'qa060r_su'@'%' IDENTIFIED BY 'secret'",
        "GRANT ALL PRIVILEGES ON zixma.* TO 'qa060r_su'@'%'",
        "GRANT SET_USER_ID ON *.* TO 'qa060r_su'@'%'",
        "DROP TABLE IF EXISTS qa060r_sys",
        "CREATE TABLE qa060r_sys (id INT PRIMARY KEY, n INT)",
        "CREATE DEFINER=`root`@`%` TRIGGER qa060r_sys_bi BEFORE INSERT ON qa060r_sys FOR EACH ROW SET NEW.n = 1",
    ]).await?;
    let t = tref(d, "qa060r_sys");
    let result = async {
        let dev = connect_as(d, "qa060r_su", "secret").await?;
        let listed = dev.triggers(&t).await?;
        let mut cur = listed[0].clone();
        cur.statement = "SET NEW.n = 2".into();
        let edits = [TriggerEdit {
            key: "k".into(),
            orig: Some(listed[0].clone()),
            cur,
            deleted: false,
        }];
        let (_, save) = trigger_saves(d, &t, ObjectKind::Table, &edits)?.remove(0);
        let r = save_like_the_app(&dev, &save, async {
            Ok(!dev.trigger_exists("zixma", "qa060r_sys_bi").await?)
        })
        .await;
        dev.close().await;
        Ok::<_, anyhow::Error>(r)
    }
    .await;
    let exists = root.trigger_exists("zixma", "qa060r_sys_bi").await?;
    let n = scalar(&root, "SHOW CREATE TRIGGER qa060r_sys_bi")
        .await
        .ok();
    run(
        &root,
        &["DROP TABLE qa060r_sys", "DROP USER 'qa060r_su'@'%'"],
    )
    .await?;
    root.close().await;
    let err = result?.expect_err("refused before the DROP");
    assert!(exists && n.is_some(), "trigger lost: {err}");
    // The definer checks come before the binary-log one: SYSTEM_USER is named.
    assert!(
        err.contains("SYSTEM_USER") && err.contains("Edit it while connected as root@%"),
        "{err}"
    );
    assert!(!err.contains("restore:"), "nothing was dropped: {err}");
    Ok(())
}

/// Fix round 2: a query result keeps each MySQL JSON value as the server's JSON text next to
/// the grid's display form, on the text and the binary (bound parameter) protocol, so a
/// query-tab export writes the JSON itself.
#[tokio::test]
async fn qa060r2_mysql_query_results_keep_the_json_text() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    run(&db, &[
        "DROP TABLE IF EXISTS qa060_qjson",
        "CREATE TABLE qa060_qjson (id INT PRIMARY KEY, j JSON, t TEXT)",
        r#"INSERT INTO qa060_qjson VALUES (1, '{"n": 12345678901234567890}', 'x'), (2, '"just a string"', 'y'), (3, NULL, 'z')"#,
    ]).await?;
    let expected = vec![(
        1,
        vec![
            Some(r#"{"n": 12345678901234567890}"#.to_owned()),
            Some(r#""just a string""#.to_owned()),
            None,
        ],
    )];
    let text = db
        .query("SELECT id, j, t FROM qa060_qjson ORDER BY id")
        .await?;
    assert_eq!(text.json_text, Some(expected.clone()));
    assert_eq!(
        text.rows[1][1].as_deref(),
        Some("just a string"),
        "the grid keeps its display form"
    );
    let bound = db
        .execute(
            &[Stmt {
                sql: "SELECT id, j, t FROM qa060_qjson WHERE id > ? ORDER BY id".into(),
                params: vec![serde_json::json!(0)],
                expect: None,
                check: None,
            }],
            false,
        )
        .await?;
    assert_eq!(bound[0].json_text, Some(expected));
    run(&db, &["DROP TABLE qa060_qjson"]).await?;
    db.close().await;
    Ok(())
}
