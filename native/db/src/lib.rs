//! Pure Rust drivers. Integration tests explicitly enable the test endpoint fence.
mod guards;
mod metadata;
mod tls;
mod values;
mod wire;
use anyhow::{Context, Result, bail};
pub use guards::{CommitReview, EngineWarning};
use serde_json::Value;
use telecat_core::{
    changes::Stmt,
    sql::{first_keyword, split_statements},
    types::*,
};
use tokio::sync::Mutex;
use wire::Wire;
pub use wire::{buffer_cell, format_bytes, validate_endpoint, validate_test_endpoint};

pub struct Session {
    pub driver: Driver,
    pub backend_id: String,
    pub server_version: String,
    pub tls: Option<String>,
    pub user: String,
    pub database: String,
    data: Mutex<Option<Wire>>,
    admin: Mutex<Option<Wire>>,
    control: Mutex<Option<Wire>>,
    password: String,
    config: ConnectionConfig,
}
impl Session {
    /// Tests use this constructor, avoiding process-global env changes/races.
    pub async fn connect_test(config: &ConnectionConfig, password: &str) -> Result<Self> {
        validate_test_endpoint(config)?;
        Self::connect(config, password).await
    }
    pub async fn connect(config: &ConnectionConfig, password: &str) -> Result<Self> {
        validate_endpoint(config)?;
        let mut data = Wire::connect(config, password).await?;
        let pg = config.driver == Driver::Postgres;
        let version = data
            .run(
                if pg {
                    "SHOW server_version"
                } else {
                    "SELECT VERSION()"
                },
                &[],
            )
            .await?
            .0
            .rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .flatten()
            .unwrap_or_default();
        let status = data
            .run(
                if pg {
                    "SELECT current_database(), current_user, pg_backend_pid()"
                } else {
                    "SELECT DATABASE(), CURRENT_USER(), CONNECTION_ID()"
                },
                &[],
            )
            .await?
            .0
            .rows;
        let row = status.first().context("No server identity")?;
        let tls = data
            .run(
                if pg {
                    "SELECT version FROM pg_stat_ssl WHERE pid = pg_backend_pid()"
                } else {
                    "SHOW SESSION STATUS LIKE 'Ssl_version'"
                },
                &[],
            )
            .await
            .ok()
            .and_then(|r| {
                r.0.rows
                    .first()
                    .and_then(|r| r.get(if pg { 0 } else { 1 }))
                    .cloned()
                    .flatten()
            })
            .filter(|s| !s.is_empty());
        let mut admin = Wire::connect(config, password).await?;
        // Same timeout and best-effort behavior as Electron main/index.ts:admin.
        let _ = admin
            .run(
                if pg {
                    "SET lock_timeout = '15s'"
                } else {
                    "SET SESSION innodb_lock_wait_timeout = 15"
                },
                &[],
            )
            .await;
        let control = Wire::connect(config, password).await?;
        Ok(Self {
            driver: config.driver,
            backend_id: row[2].clone().unwrap_or_default(),
            server_version: format!("{} {version}", if pg { "PostgreSQL" } else { "MySQL" }),
            tls,
            user: if pg {
                row[1].clone().unwrap_or_else(|| config.user.clone())
            } else {
                config.user.clone()
            },
            database: row[0].clone().unwrap_or_default(),
            data: Mutex::new(Some(data)),
            admin: Mutex::new(Some(admin)),
            control: Mutex::new(Some(control)),
            password: password.into(),
            config: config.clone(),
        })
    }
    /// Reconnect all workers before replacing the caller's session. Failure leaves
    /// the original session usable; credentials remain in memory, never re-read.
    pub async fn with_database(&self, database: &str) -> Result<Self> {
        let mut config = self.config.clone();
        config.database = database.to_owned();
        Self::connect(&config, &self.password).await
    }
    /// Electron's session:verifyPassword compares against this session's password.
    /// This never reads Keychain or opens a new database connection.
    pub fn verify_password(&self, candidate: &str) -> bool {
        let mut difference = self.password.len() ^ candidate.len();
        for (a, b) in self.password.bytes().zip(candidate.bytes()) {
            difference |= (a ^ b) as usize;
        }
        difference == 0
    }
    async fn run(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.data
            .lock()
            .await
            .as_mut()
            .context("Connection is closed")?
            .run(sql, params)
            .await
            .map(|v| v.0)
    }
    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        self.run(sql, &[]).await
    }
    pub async fn query_checked(&self, sql: &str, confirmed: bool) -> Result<QueryResult> {
        if !confirmed {
            if let Some(reason) = telecat_core::sql::dangerous_reason(sql, self.driver) {
                bail!("Confirmation required: {reason}");
            }
        }
        self.query(sql).await
    }
    /// Guarded calls always have a transaction, including when `transaction` is false.
    /// The admin connection is taken out of its slot during execution. Dropping the future
    /// drops that connection, so an interrupted transaction can never be reused or committed.
    pub async fn execute(&self, stmts: &[Stmt], transaction: bool) -> Result<Vec<QueryResult>> {
        self.execute_after_review(stmts, transaction, false).await
    }
    /// Inspect engine requirements before presenting the confirmation. Revalidated at execution.
    pub async fn review_commit(&self, stmts: &[Stmt]) -> Result<CommitReview> {
        let mut slot = self.admin.lock().await;
        guards::review(
            slot.as_mut().context("Admin connection is closed")?,
            self.driver,
            &self.database,
            stmts,
        )
        .await
    }
    /// `allow_non_transactional` is set only after the caller displays CommitReview::message.
    pub async fn execute_after_review(
        &self,
        stmts: &[Stmt],
        transaction: bool,
        allow_non_transactional: bool,
    ) -> Result<Vec<QueryResult>> {
        let guarded = stmts
            .iter()
            .any(|s| s.expect.is_some() || s.check.is_some());
        let tx = transaction || guarded;
        for st in stmts {
            if split_statements(&st.sql, self.driver).len() != 1 {
                bail!("Only one SQL statement is allowed per call");
            }
            let kw = first_keyword(&st.sql, self.driver);
            if tx
                && [
                    "BEGIN",
                    "START",
                    "COMMIT",
                    "ROLLBACK",
                    "SAVEPOINT",
                    "RELEASE",
                    "SET",
                    "LOCK",
                    "UNLOCK",
                    "XA",
                ]
                .contains(&kw.as_str())
            {
                bail!("Transaction control cannot be used inside execute");
            }
            if tx
                && self.driver == Driver::Mysql
                && [
                    "CREATE",
                    "ALTER",
                    "DROP",
                    "TRUNCATE",
                    "RENAME",
                    "GRANT",
                    "REVOKE",
                    "ANALYZE",
                    "OPTIMIZE",
                    "REPAIR",
                    "FLUSH",
                    "RESET",
                    "INSTALL",
                    "UNINSTALL",
                    "LOAD",
                    "CALL",
                ]
                .contains(&kw.as_str())
            {
                bail!("MySQL implicit-commit statements cannot be guarded transactionally");
            }
            if let Some(c) = &st.check {
                if split_statements(&c.sql, self.driver).len() != 1
                    || first_keyword(&c.sql, self.driver) != "SELECT"
                {
                    bail!("A guard check must be a single SELECT");
                }
            }
        }
        let mut slot = self.admin.lock().await;
        let mut conn = slot
            .take()
            .context("Admin connection is closed; reconnect")?;
        let mut nontransactional_started = false;
        let result = async {
            let review = if tx { guards::review(&mut conn, self.driver, &self.database, stmts).await? } else { CommitReview::default() };
            if !review.non_transactional.is_empty() && !allow_non_transactional {
                bail!(review.message());
            }
            let table_locks = !review.non_transactional.is_empty();
            if tx {
                if table_locks {
                    conn.run("SET autocommit=0", &[]).await?;
                    conn.run(&format!("LOCK TABLES {}", review.targets.iter().map(|t| format!("{t} WRITE")).collect::<Vec<_>>().join(", ")), &[]).await?;
                } else if self.driver == Driver::Mysql {
                    conn.run("START TRANSACTION", &[]).await?;
                } else {
                    conn.run("BEGIN", &[]).await?;
                }
            }
            if tx && self.driver == Driver::Mysql {
                // Hold metadata locks until commit and recheck engines under those locks.
                for target in &review.targets { conn.run(&format!("SELECT 1 FROM {target} LIMIT 0 FOR UPDATE"), &[]).await?; }
                if !table_locks && !guards::review(&mut conn, self.driver, &self.database, stmts).await?.non_transactional.is_empty() { bail!("Table engine changed; review the commit again"); }
            }
            let mut out = vec![];
            for st in stmts {
                if let Some(check) = &st.check {
                    let n = Some(guards::locked_count(&mut conn, &check.sql, &check.params).await?);
                    if n != Some(check.expect) {
                        bail!("{} (matched {})", check.message, n.map_or("-1".into(), |n| n.to_string()));
                    }
                }
                if table_locks { guards::precheck_write(&mut conn, &self.database, st).await?; }
                if table_locks { nontransactional_started = true; }
                let (r, n) = conn.run(&st.sql, &st.params).await?;
                if let Some(expect) = st.expect {
                    if n != expect {
                        if table_locks { bail!("Expected {expect} row but {n} were affected. This engine cannot roll back; changes may have been saved. Reload the table."); }
                        if n == 0 {
                            bail!("The row was not found — it may have been changed or deleted by someone else. Nothing was saved; reload and try again.");
                        }
                        bail!("Expected to change {expect} row but {n} would be affected. Nothing was saved.");
                    }
                }
                out.push(r);
            }
            if tx { conn.run("COMMIT", &[]).await?; }
            Ok::<_, anyhow::Error>(out)
        }.await;
        if result.is_err() && tx {
            if let Err(e) = conn.run("ROLLBACK", &[]).await {
                conn.close().await;
                return Err(result
                    .err()
                    .unwrap()
                    .context(format!("Rollback failed; admin connection closed: {e}")));
            }
        }
        // Harmless without table locks; restore connection state even after a rejected precheck.
        if tx && self.driver == Driver::Mysql {
            if let Err(e) = async {
                conn.run("UNLOCK TABLES", &[]).await?;
                conn.run("SET autocommit=1", &[]).await?;
                Ok::<_, anyhow::Error>(())
            }
            .await
            {
                conn.close().await;
                return Err(e.context("Admin connection cleanup failed; reconnect"));
            }
        }
        *slot = Some(conn);
        if nontransactional_started {
            result.map_err(|e| e.context("A non-transactional table was written: earlier changes may have been saved and cannot be rolled back. Reload the table."))
        } else {
            result
        }
    }
    pub async fn process_list(&self, db: Option<&str>) -> Result<QueryResult> {
        let lit = |s| telecat_core::changes::literal(self.driver, s);
        let sql = if self.driver == Driver::Mysql {
            format!(
                "SELECT ID AS id, USER AS user, HOST AS host, DB AS db, COMMAND AS command, TIME AS time, STATE AS state, INFO AS query FROM information_schema.PROCESSLIST WHERE ID <> CONNECTION_ID(){} ORDER BY TIME DESC",
                db.filter(|s| !s.is_empty())
                    .map_or(String::new(), |s| format!(" AND DB = {}", lit(s)))
            )
        } else {
            format!(
                "SELECT pid, usename AS user, client_addr::text AS host, datname AS db, state, to_char(now() - query_start, 'HH24:MI:SS.MS') AS duration, wait_event_type || ':' || wait_event AS wait, query FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND backend_type = 'client backend'{} ORDER BY query_start NULLS LAST",
                db.filter(|s| !s.is_empty())
                    .map_or(String::new(), |s| format!(" AND datname = {}", lit(s)))
            )
        };
        self.control
            .lock()
            .await
            .as_mut()
            .context("Control connection is closed")?
            .run(&sql, &[])
            .await
            .map(|v| v.0)
    }
    pub async fn kill(&self, id: &str, connection: bool) -> Result<()> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            bail!("Invalid process id {id}");
        }
        let sql = if self.driver == Driver::Mysql {
            format!("KILL {}{id};", if connection { "" } else { "QUERY " })
        } else {
            format!(
                "SELECT {}({id});",
                if connection {
                    "pg_terminate_backend"
                } else {
                    "pg_cancel_backend"
                }
            )
        };
        let r = self
            .control
            .lock()
            .await
            .as_mut()
            .context("Control connection is closed")?
            .run(&sql, &[])
            .await?
            .0;
        if self.driver == Driver::Postgres
            && r.rows
                .first()
                .and_then(|r| r.first())
                .and_then(|s| s.as_deref())
                != Some("true")
        {
            bail!(
                "Process {id} could not be signalled (it may have finished, or you lack permission)"
            );
        }
        Ok(())
    }
    pub async fn cancel(&self) -> Result<()> {
        self.kill(&self.backend_id, false).await
    }
    pub async fn close(&self) {
        for slot in [&self.data, &self.admin, &self.control] {
            if let Some(c) = slot.lock().await.take() {
                c.close().await;
            }
        }
    }
}
