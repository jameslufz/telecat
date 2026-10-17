//! Read-only fixture refresh. Only approved throwaway endpoints are accepted.
use telecat_core::types::*;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut out = serde_json::Map::new();
    for (driver, port, db, user, key) in [
        (Driver::Mysql, 3307, "zixma", "root", "mysql"),
        (Driver::Postgres, 5434, "main_db", "postgres", "postgres"),
    ] {
        let mut c = empty_connection(driver);
        c.host = "127.0.0.1".into();
        c.port = port;
        c.database = db.into();
        c.user = user.into();
        let s = telecat_db::Session::connect_test(&c, "secret").await?;
        let t = TableRef {
            schema: if driver == Driver::Mysql {
                db.into()
            } else {
                "public".into()
            },
            name: "sa_banks".into(),
        };
        out.insert(key.into(),serde_json::json!({"structure":s.structure(&t).await?,"indexes":s.indexes(&t).await?,"info":s.table_info(&t).await?}));
        if driver == Driver::Postgres {
            let t = TableRef {
                schema: "billing".into(),
                name: "invoices".into(),
            };
            out.insert("invoices".into(),serde_json::json!({"structure":s.structure(&t).await?,"indexes":s.indexes(&t).await?,"info":s.table_info(&t).await?}));
        }
        s.close().await;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
