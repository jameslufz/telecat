use anyhow::Result;
use telecat_core::types::*;
use telecat_db::Session;
#[tokio::test]
async fn ssl_modes_and_negotiation_match_electron() -> Result<()> {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/ssl.json"))?;
    for case in cases {
        let c: ConnectionConfig = serde_json::from_value(case["config"].clone())?;
        let result = Session::connect_test(&c, "secret").await;
        assert_eq!(
            result.is_ok(),
            case["out"]["ok"].as_bool().unwrap(),
            "{:?} {:?} {:?}: {}",
            c.driver,
            c.ssl_mode,
            c.negotiation,
            result
                .as_ref()
                .err()
                .map_or(String::new(), |e| format!("{e:#}"))
        );
        if let Ok(db) = result {
            assert_eq!(serde_json::json!(db.tls), case["out"]["tls"]);
            db.close().await;
        }
    }
    Ok(())
}
#[tokio::test]
async fn authentication_failure_never_falls_back_to_success() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let mut c = empty_connection(d);
        c.port = if d == Driver::Mysql { 3307 } else { 5434 };
        c.user = if d == Driver::Mysql {
            "root"
        } else {
            "postgres"
        }
        .into();
        c.database = if d == Driver::Mysql {
            "zixma"
        } else {
            "main_db"
        }
        .into();
        for mode in [SslMode::Preferred, SslMode::Allow] {
            c.ssl_mode = mode;
            assert!(
                Session::connect_test(&c, "definitely-wrong-m1-password")
                    .await
                    .is_err()
            );
        }
    }
    Ok(())
}
