//! QA 0.7.0 regressions (native/.claude-qa/qa-0.7.0.md).
//!
//! BUG-001: a MySQL query-result save runs under the result's time_zone and used to reset the
//! commit connection to the server's default zone afterwards, dropping the zone the startup
//! SQL set. Every later save then wrote TIMESTAMPs shifted from what the table showed.
use anyhow::Result;
use telecat_core::{changes::Stmt, types::*};
use telecat_db::Session;

fn mysql(startup: &str) -> ConnectionConfig {
    let mut c = empty_connection(Driver::Mysql);
    c.host = "127.0.0.1".into();
    c.port = 3307;
    c.user = "root".into();
    c.database = "zixma".into();
    c.startup_sql = startup.into();
    c
}
async fn scalar(s: &Session, sql: &str) -> Result<Option<String>> {
    Ok(s.query(sql)
        .await?
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten()))
}
fn stmt(sql: &str, params: &[&str]) -> Stmt {
    let mut st = Stmt::new(sql);
    st.params = params.iter().map(|p| (*p).into()).collect();
    st.expect = Some(1);
    st
}
/// A table edit (grid save, no session settings).
async fn grid_save(s: &Session, st: Stmt) -> Result<()> {
    s.execute_after_review(&[st], true, false).await?;
    Ok(())
}
/// A query-result edit save, under the result's time zone as `result_edit_plan` reports it.
async fn result_save(s: &Session, table: &str, id: &str, zone: &str) -> Result<()> {
    s.execute_after_review_with(
        &[stmt(
            &format!("UPDATE `zixma`.`{table}` SET `v` = `v` + 1 WHERE `id` = ?"),
            &[id],
        )],
        true,
        false,
        &[("time_zone".into(), zone.into())],
    )
    .await?;
    Ok(())
}
async fn table(s: &Session, name: &str) -> Result<()> {
    s.query(&format!("DROP TABLE IF EXISTS {name}")).await?;
    s.query(&format!(
        "CREATE TABLE {name} (id INT PRIMARY KEY, ts TIMESTAMP NULL, v INT NOT NULL DEFAULT 0)"
    ))
    .await?;
    s.query(&format!(
        "INSERT INTO {name} VALUES (1, '2024-01-01 00:00:00', 0), (2, '2024-01-01 00:00:00', 0)"
    ))
    .await?;
    Ok(())
}
async fn shown(s: &Session, name: &str) -> Result<Vec<(String, String)>> {
    Ok(s.query(&format!("SELECT id, ts FROM {name} ORDER BY id"))
        .await?
        .rows
        .into_iter()
        .map(|r| {
            (
                r[0].clone().unwrap_or_default(),
                r[1].clone().unwrap_or_default(),
            )
        })
        .collect())
}

/// QA probe P1, made permanent.
#[tokio::test]
async fn p1_startup_time_zone_survives_a_result_edit_save() -> Result<()> {
    let s = Session::connect_test(&mysql("SET time_zone = '+07:00'"), "secret").await?;
    assert_eq!(
        scalar(&s, "SELECT @@session.time_zone").await?.as_deref(),
        Some("+07:00")
    );
    let t = "qa070r_p1";
    table(&s, t).await?;
    let update = |id: &str| {
        stmt(
            "UPDATE `zixma`.`qa070r_p1` SET `ts` = ? WHERE `id` = ?",
            &["2024-01-01 10:00:00", id],
        )
    };
    grid_save(&s, update("1")).await?;
    result_save(&s, t, "2", "+07:00").await?;
    grid_save(&s, update("2")).await?;
    let rows = shown(&s, t).await?;
    s.query(&format!("DROP TABLE {t}")).await?;
    s.close().await;
    assert_eq!(rows[0].1, "2024-01-01 10:00:00");
    assert_eq!(rows[1].1, "2024-01-01 10:00:00", "row 2 saved shifted");
    Ok(())
}

/// New and duplicated rows after a result save, including one made under another zone than
/// the startup SQL's (the user ran `SET time_zone` in the editor first): the commit
/// connection always returns to its own zone.
#[tokio::test]
async fn inserted_and_duplicated_rows_keep_the_startup_time_zone() -> Result<()> {
    let s = Session::connect_test(&mysql("SET time_zone = '+07:00'"), "secret").await?;
    let t = "qa070r_insert";
    table(&s, t).await?;
    for (id, zone) in [(10, "+07:00"), (20, "+00:00"), (30, "-05:00")] {
        result_save(&s, t, "1", zone).await?;
        let id = id.to_string();
        // A new row typed in the grid.
        grid_save(
            &s,
            stmt(
                "INSERT INTO `zixma`.`qa070r_insert` (`id`, `ts`, `v`) VALUES (?, ?, ?)",
                &[&id, "2024-06-01 08:30:00", "0"],
            ),
        )
        .await?;
        // A duplicated row: the copy carries the shown value of the original.
        let original = shown(&s, t).await?.into_iter().find(|r| r.0 == id).unwrap();
        let copy = (id.parse::<u32>()? + 1000).to_string();
        grid_save(
            &s,
            stmt(
                "INSERT INTO `zixma`.`qa070r_insert` (`id`, `ts`, `v`) VALUES (?, ?, ?)",
                &[&copy, &original.1, "0"],
            ),
        )
        .await?;
        let rows = shown(&s, t).await?;
        for wanted in [&id, &copy] {
            let row = rows.iter().find(|r| &r.0 == wanted).unwrap();
            assert_eq!(
                row.1, "2024-06-01 08:30:00",
                "after a result save under {zone}"
            );
        }
    }
    // The raw instants are the +07:00 ones: 08:30 at +07:00 is 01:30 UTC.
    let utc = scalar(
        &s,
        &format!("SELECT CONVERT_TZ(ts, '+07:00', '+00:00') FROM {t} WHERE id >= 1000 LIMIT 1"),
    )
    .await?;
    assert_eq!(utc.as_deref(), Some("2024-06-01 01:30:00"));
    s.query(&format!("DROP TABLE {t}")).await?;
    s.close().await;
    Ok(())
}

/// Without startup SQL the commit connection keeps the server's zone, as before 0.7.
#[tokio::test]
async fn without_startup_sql_the_server_zone_stays() -> Result<()> {
    let s = Session::connect_test(&mysql(""), "secret").await?;
    let t = "qa070r_default";
    table(&s, t).await?;
    result_save(&s, t, "1", "+03:00").await?;
    grid_save(
        &s,
        stmt(
            "UPDATE `zixma`.`qa070r_default` SET `ts` = ? WHERE `id` = ?",
            &["2024-01-01 10:00:00", "2"],
        ),
    )
    .await?;
    let rows = shown(&s, t).await?;
    s.query(&format!("DROP TABLE {t}")).await?;
    s.close().await;
    assert_eq!(rows[1].1, "2024-01-01 10:00:00");
    Ok(())
}

/// "Commit result unknown" row checks run on the commit connection: after a result save they
/// still read TIMESTAMPs in the startup SQL's zone, the zone the grid showed the values in.
#[tokio::test]
async fn commit_unknown_row_checks_read_in_the_startup_time_zone() -> Result<()> {
    use telecat_core::changes::{Check, RowVerify};
    use telecat_db::{CommitProgress, CommitVerdict};
    let s = Session::connect_test(&mysql("SET time_zone = '+07:00'"), "secret").await?;
    let t = "qa070r_verify";
    table(&s, t).await?;
    grid_save(
        &s,
        stmt(
            "UPDATE `zixma`.`qa070r_verify` SET `ts` = ? WHERE `id` = ?",
            &["2024-01-01 10:00:00", "1"],
        ),
    )
    .await?;
    result_save(&s, t, "2", "-05:00").await?;
    let check = |value: &str| Check {
        sql: "SELECT COUNT(*) FROM `zixma`.`qa070r_verify` WHERE `id` = ? AND `ts` = ?".into(),
        params: vec!["1".into(), value.into()],
        expect: 1,
        message: String::new(),
    };
    let verdict = s
        .verify_interrupted_commit(
            &CommitProgress {
                transaction: true,
                commit_sent: true,
                ..Default::default()
            },
            &[RowVerify {
                applied: Some(check("2024-01-01 10:00:00")),
                not_applied: Some(check("2024-01-01 00:00:00")),
            }],
        )
        .await?;
    s.query(&format!("DROP TABLE {t}")).await?;
    s.close().await;
    assert_eq!(verdict, CommitVerdict::Applied);
    Ok(())
}
