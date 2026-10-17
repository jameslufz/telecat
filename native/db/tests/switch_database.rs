use anyhow::Result;
use telecat_core::{changes::Stmt, types::*};
use telecat_db::Session;
async fn contract(driver: Driver) -> Result<()> {
    let mut c = empty_connection(driver);
    c.host = "127.0.0.1".into();
    c.port = if driver == Driver::Mysql { 3307 } else { 5434 };
    c.user = if driver == Driver::Mysql {
        "root"
    } else {
        "postgres"
    }
    .into();
    c.database = if driver == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
    .into();
    let old = Session::connect_test(&c, "secret").await?;
    assert!(old.list_databases().await?.contains(&c.database));
    assert!(
        old.with_database("telecat_shell_integration_missing_database")
            .await
            .is_err()
    );
    assert_eq!(
        old.query("SELECT 1").await?.rows[0][0].as_deref(),
        Some("1")
    );
    let new = old.with_database(&c.database).await?;
    assert_ne!(new.backend_id, old.backend_id);
    assert!(new.verify_password("secret"));
    assert!(!new.verify_password(""));
    assert_eq!(new.database, c.database);
    old.close().await;
    assert_eq!(
        new.query("SELECT 2").await?.rows[0][0].as_deref(),
        Some("2")
    );
    // The new admin worker is independent too, with the same transaction guard path.
    let result = new
        .execute(
            &[Stmt {
                sql: "SELECT 3".into(),
                params: vec![],
                expect: None,
                check: None,
            }],
            true,
        )
        .await?;
    assert_eq!(result[0].rows[0][0].as_deref(), Some("3"));
    new.close().await;
    Ok(())
}
#[tokio::test]
async fn mysql_switch_workers_failure_preserves_old_session() -> Result<()> {
    contract(Driver::Mysql).await
}
#[tokio::test]
async fn postgres_switch_workers_failure_preserves_old_session() -> Result<()> {
    contract(Driver::Postgres).await
}
