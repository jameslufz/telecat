//! Pure Rust drivers. Integration tests explicitly enable the test endpoint fence.
mod guards;
mod import;
mod metadata;
mod result_sources;
mod tls;
mod values;
mod wire;
use anyhow::{Context, Result, bail};
pub use guards::{CommitReview, EngineWarning};
pub use import::{
    FileCheck, ImportFailure, ImportOptions, ImportProgress, ImportReport, Outcome, scan_dump,
    scan_dump_checked,
};
use serde_json::Value;
use telecat_core::{
    changes::{NOT_UNIQUE, ROW_MISSING, Stmt},
    sql::{first_keyword, split_statements},
    types::*,
};
use tokio::sync::Mutex;
use wire::Wire;
pub use wire::{
    CONNECT_TIMEOUT, ServerError, buffer_cell, format_bytes, is_auth_failure, is_connection_lost,
    validate_endpoint, validate_test_endpoint,
};
#[cfg(feature = "test-support")]
pub mod test_proxy;
/// How long a whole [`Session::connect`] may take: three connections, TLS fallback included.
const SESSION_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);
/// How far the last commit call got. Readable while that call hangs on a dead connection or
/// after it was abandoned, so an interrupted save can be judged honestly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitProgress {
    /// The writes ran in one transaction.
    pub transaction: bool,
    /// A write was sent that commits by itself (no transaction, a MySQL non-transactional
    /// table): it may stay even though the connection dropped.
    pub wrote_outside_transaction: bool,
    /// COMMIT was sent; whether the server received it is unknown until it answers.
    pub commit_sent: bool,
    /// The call returned (saved, or failed without losing the connection).
    pub finished: bool,
    /// The call is over, whatever its result. Until then a call hung on a dead link could
    /// still send more once the link comes back.
    pub returned: bool,
    /// PostgreSQL transaction id, to ask the server later how the transaction ended.
    pub xid: Option<String>,
    /// The commit connection's server id, to tell whether it is still open on the server.
    pub backend: Option<String>,
    /// More than the id, which the server reuses: enough to tell that connection apart.
    pub identity: Option<BackendIdentity>,
}
/// A connection as the server lists it, beyond its reusable numeric id: PostgreSQL's
/// `backend_start` and an `application_name` carrying a token unique to the connection; MySQL's
/// user and host plus the token in a session variable (`@telecat_connection`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendIdentity {
    pub id: String,
    pub token: String,
    pub started: Option<String>,
    pub user: Option<String>,
    pub host: Option<String>,
}
/// A token that tells one of Telecat's connections from any other, now or later.
fn connection_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}", h.finish())
}
impl CommitProgress {
    /// Nothing that could persist was sent: the server discards an open transaction when its
    /// connection ends, and a COMMIT that was never sent can't arrive later.
    pub fn nothing_sent(&self) -> bool {
        !self.commit_sent && !self.wrote_outside_transaction
    }
}
/// The connection dropped during a commit; see [`Session::commit_progress`].
#[derive(Debug)]
pub struct CommitInterrupted {
    pub cause: String,
    pub progress: CommitProgress,
}
impl std::fmt::Display for CommitInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The connection was lost during the save ({}). ",
            self.cause
        )?;
        f.write_str(if self.progress.nothing_sent() {
            "Nothing from this commit was saved: it never reached COMMIT, and the server discards an unfinished transaction."
        } else {
            "Whether it was saved is unknown until Telecat can check."
        })
    }
}
impl std::error::Error for CommitInterrupted {}
/// The verdict from MySQL row checks: every row has to say the same, since one that only
/// coincidentally holds the intended (or the loaded) values must not decide for the others.
fn mysql_verdict(r: &RowReadings) -> CommitVerdict {
    let unknown = |reason: String| CommitVerdict::Unknown {
        reason,
        open_backend: None,
    };
    let rows = r.rows;
    if rows == 0 {
        return unknown(
            "Its changes can't be checked row by row (a table without a primary key, or edits made in query results).".into(),
        );
    }
    if r.applied == rows {
        return CommitVerdict::Applied;
    }
    if r.not_applied == rows {
        return CommitVerdict::NotApplied;
    }
    let neither = rows - r.applied - r.not_applied - r.both;
    let mut parts = vec![];
    if r.applied > 0 {
        parts.push(format!(
            "{} of {rows} changed rows read as saved",
            r.applied
        ));
    }
    if r.not_applied > 0 {
        parts.push(format!("{} of {rows} as not saved", r.not_applied));
    }
    if r.both > 0 {
        parts.push(format!(
            "{} of {rows} hold a value that matches both what was loaded and what was typed (the typed value equals the original, for example numerically: 1.5 and 1.50), so they can't tell",
            r.both
        ));
    }
    if neither > 0 {
        parts.push(format!(
            "{neither} of {rows} match neither what was loaded nor what was typed (a trigger or another session changed them since, the row was deleted, or its key is generated by the server)"
        ));
    }
    unknown(format!("{}.", parts.join("; ")))
}
/// How the rows of an interrupted MySQL save read now.
#[derive(Default)]
struct RowReadings {
    rows: usize,
    applied: usize,
    not_applied: usize,
    /// Both checks hold: the typed value equals the loaded one.
    both: usize,
}
/// What a check after an interrupted commit found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitVerdict {
    Applied,
    NotApplied,
    Unknown {
        reason: String,
        /// The old commit connection is still open on the server and could still finish it.
        open_backend: Option<String>,
    },
}

/// The commit connection and its server id.
async fn open_admin(config: &ConnectionConfig, password: &str) -> Result<(Wire, BackendIdentity)> {
    let mut admin = Wire::connect(config, password).await?;
    // Same timeout and best-effort behavior as Electron main/index.ts:admin.
    let _ = admin
        .run(
            if config.driver == Driver::Postgres {
                "SET lock_timeout = '15s'"
            } else {
                "SET SESSION innodb_lock_wait_timeout = 15"
            },
            &[],
        )
        .await;
    if config.driver == Driver::Mysql {
        // Commits never let MySQL silently truncate, clamp or zero a value: in strict mode the
        // statement fails and the transaction rolls back instead. The server's other modes stay.
        admin
            .run(
                "SET SESSION sql_mode = CONCAT_WS(',', NULLIF(@@SESSION.sql_mode, ''), 'STRICT_ALL_TABLES')",
                &[],
            )
            .await?;
    }
    let identity = commit_identity(&mut admin, config.driver).await?;
    Ok((admin, identity))
}
/// Marks the commit connection with a token and reads back how the server lists it, so a later
/// check (or "End Old Connection") can't mistake another connection that got the same id.
async fn commit_identity(conn: &mut Wire, driver: Driver) -> Result<BackendIdentity> {
    let first = |rows: Vec<Vec<Option<String>>>, i: usize| {
        rows.into_iter()
            .next()
            .and_then(|r| r.into_iter().nth(i).flatten())
    };
    let id = backend_id(conn, driver).await?;
    let token = connection_token();
    let lit = |s: &str| telecat_core::changes::literal(driver, s);
    Ok(match driver {
        Driver::Postgres => {
            conn.run(
                &format!(
                    "SET application_name = {}",
                    lit(&pg_application_name(&token))
                ),
                &[],
            )
            .await?;
            let started = conn
                .run(
                    "SELECT backend_start::text FROM pg_stat_activity WHERE pid = pg_backend_pid()",
                    &[],
                )
                .await?
                .0
                .rows;
            BackendIdentity {
                id,
                started: first(started, 0),
                token,
                ..Default::default()
            }
        }
        Driver::Mysql => {
            conn.run(&format!("SET @telecat_connection = {}", lit(&token)), &[])
                .await?;
            // performance_schema lists the token; without access the identity stays partial and
            // the old connection is never ended.
            let listed = conn
                .run(
                    "SELECT PROCESSLIST_USER, PROCESSLIST_HOST FROM performance_schema.threads WHERE PROCESSLIST_ID = CONNECTION_ID()",
                    &[],
                )
                .await
                .map(|r| r.0.rows)
                .unwrap_or_default();
            BackendIdentity {
                id,
                user: first(listed.clone(), 0),
                host: first(listed, 1),
                token,
                ..Default::default()
            }
        }
    })
}
fn pg_application_name(token: &str) -> String {
    format!("Telecat {token}")
}
async fn backend_id(conn: &mut Wire, driver: Driver) -> Result<String> {
    conn.run(
        if driver == Driver::Postgres {
            "SELECT pg_backend_pid()::text"
        } else {
            "SELECT CONNECTION_ID()"
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
    .context("No backend id")
}
/// The commit connection, reopened when an earlier failure had to close it.
async fn take_admin(
    slot: &mut Option<Wire>,
    config: &ConnectionConfig,
    password: &str,
    id: &std::sync::Mutex<Option<BackendIdentity>>,
) -> Result<Wire> {
    match slot.take() {
        Some(conn) if !conn.is_closed() => Ok(conn),
        _ => {
            // The cause stays in the chain, so a dropped network is still recognised as one.
            let (conn, backend) = open_admin(config, password).await.map_err(|e| {
                let message =
                    format!("The commit connection was lost and could not be reopened: {e:#}");
                e.context(message)
            })?;
            *id.lock().unwrap_or_else(|e| e.into_inner()) = Some(backend);
            Ok(conn)
        }
    }
}
const IMPLICIT_COMMIT: [&str; 16] = [
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
];
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
    /// Table reads, opened on first use. Its session settings are never changed by the user, so
    /// rows read here are identified the same way the commit connection matches them.
    pub(crate) reader: Mutex<Option<Wire>>,
    /// Server id of the reader's backend while it runs a read, for cancel().
    pub(crate) reader_busy: std::sync::Mutex<Option<String>>,
    /// Server id of the import connection's backend while an import runs, for cancel_import().
    pub(crate) importer_busy: std::sync::Mutex<Option<String>>,
    /// Server id of the commit connection.
    admin_backend: std::sync::Mutex<Option<BackendIdentity>>,
    commit: std::sync::Mutex<CommitProgress>,
    pub(crate) import: std::sync::Mutex<CommitProgress>,
    password: String,
    config: ConnectionConfig,
}
impl Session {
    /// Tests use this constructor, avoiding process-global env changes/races.
    pub async fn connect_test(config: &ConnectionConfig, password: &str) -> Result<Self> {
        validate_test_endpoint(config)?;
        Self::connect(config, password).await
    }
    /// Opens every connection, or fails within [`SESSION_CONNECT_TIMEOUT`].
    pub async fn connect(config: &ConnectionConfig, password: &str) -> Result<Self> {
        validate_endpoint(config)?;
        tokio::time::timeout(SESSION_CONNECT_TIMEOUT, Self::open(config, password))
            .await
            .context("Connection timeout")?
    }
    async fn open(config: &ConnectionConfig, password: &str) -> Result<Self> {
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
        let (admin, admin_backend) = open_admin(config, password).await?;
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
            reader: Mutex::new(None),
            reader_busy: std::sync::Mutex::new(None),
            importer_busy: std::sync::Mutex::new(None),
            admin_backend: std::sync::Mutex::new(Some(admin_backend)),
            commit: Default::default(),
            import: Default::default(),
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
    /// A new session to the same server, database and user with the same password and TLS
    /// settings: every connection reopened, with the session setup Telecat applies itself.
    /// Nothing the user ran in the old session (variables, temporary tables, transactions)
    /// carries over.
    pub async fn reopen(&self) -> Result<Self> {
        Self::connect(&self.config, &self.password).await
    }
    /// The settings this session was opened with (its current database included).
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }
    /// See [`CommitProgress`].
    pub fn commit_progress(&self) -> CommitProgress {
        self.commit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// How far the last import got, in the same terms (see [`Self::verify_interrupted_commit`]).
    pub fn import_progress(&self) -> CommitProgress {
        self.import
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn note_commit(&self, f: impl FnOnce(&mut CommitProgress)) {
        f(&mut self.commit.lock().unwrap_or_else(|e| e.into_inner()));
    }
    /// A cheap check that the server is still reachable, safe to run every few seconds: a
    /// round trip on the control connection (reopened when needed), skipped while it is busy,
    /// plus PostgreSQL's own closed flag on the other connections. A connection whose ping
    /// timed out is discarded, since its replies may still arrive.
    pub async fn ping(&self, timeout: std::time::Duration) -> Result<()> {
        for slot in [&self.data, &self.admin, &self.reader] {
            if let Ok(guard) = slot.try_lock() {
                if guard.as_ref().is_some_and(Wire::is_closed) {
                    bail!("Connection is closed");
                }
            }
        }
        let Ok(mut slot) = self.control.try_lock() else {
            return Ok(());
        };
        let result = tokio::time::timeout(timeout, async {
            if slot.is_none() {
                *slot = Some(Wire::connect(&self.config, &self.password).await?);
            }
            slot.as_mut().context("Connection is closed")?.ping().await
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                if is_connection_lost(&e) {
                    slot.take();
                }
                Err(e)
            }
            Err(elapsed) => {
                slot.take();
                Err(anyhow::Error::new(elapsed).context("The server did not answer in time"))
            }
        }
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
        let mut conn =
            take_admin(&mut slot, &self.config, &self.password, &self.admin_backend).await?;
        let review = guards::review(&mut conn, self.driver, &self.database, stmts).await;
        *slot = Some(conn);
        review
    }
    /// `allow_non_transactional` is set only after the caller displays CommitReview::message.
    pub async fn execute_after_review(
        &self,
        stmts: &[Stmt],
        transaction: bool,
        allow_non_transactional: bool,
    ) -> Result<Vec<QueryResult>> {
        self.execute_after_review_with(stmts, transaction, allow_non_transactional, &[])
            .await
    }
    /// [`Self::execute_after_review`] with `settings` of the session the edited values were read
    /// in (see `ResultEditPlan::session`), applied for this transaction only.
    pub async fn execute_after_review_with(
        &self,
        stmts: &[Stmt],
        transaction: bool,
        allow_non_transactional: bool,
        settings: &[(String, String)],
    ) -> Result<Vec<QueryResult>> {
        let result = self
            .execute_guarded(stmts, transaction, allow_non_transactional, settings)
            .await;
        self.note_commit(|p| p.returned = true);
        result
    }
    async fn execute_guarded(
        &self,
        stmts: &[Stmt],
        transaction: bool,
        allow_non_transactional: bool,
        settings: &[(String, String)],
    ) -> Result<Vec<QueryResult>> {
        const ALLOWED: [&str; 5] = [
            "time_zone",
            "TimeZone",
            "DateStyle",
            "IntervalStyle",
            "extra_float_digits",
        ];
        if let Some((name, _)) = settings
            .iter()
            .find(|(n, _)| !ALLOWED.contains(&n.as_str()))
        {
            bail!("Unsupported session setting {name}");
        }
        if !settings.is_empty() && !transaction {
            bail!("Session settings need a transaction");
        }
        let implicit_commit = |st: &Stmt| {
            self.driver == Driver::Mysql
                && IMPLICIT_COMMIT.contains(&first_keyword(&st.sql, self.driver).as_str())
        };
        // A MySQL DDL statement commits by itself, so its value check runs right before it,
        // outside a transaction. Every other guard needs one.
        let guarded = stmts
            .iter()
            .any(|s| s.expect.is_some() || (s.check.is_some() && !implicit_commit(s)));
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
            if tx && implicit_commit(st) {
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
        self.note_commit(|p| {
            *p = CommitProgress {
                transaction: tx,
                ..Default::default()
            }
        });
        let mut slot = self.admin.lock().await;
        let mut conn =
            take_admin(&mut slot, &self.config, &self.password, &self.admin_backend).await?;
        let identity = self
            .admin_backend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.note_commit(|p| {
            p.backend = identity.as_ref().map(|i| i.id.clone());
            p.identity = identity;
        });
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
                    let xid = conn.run("SELECT txid_current()::text", &[]).await?.0.rows;
                    let xid = xid.first().and_then(|r| r.first()).cloned().flatten();
                    self.note_commit(|p| p.xid = xid);
                }
                for (name, value) in settings {
                    match self.driver {
                        // Reset to the global value after this transaction (see below).
                        Driver::Mysql if name == "time_zone" => conn.run("SET SESSION time_zone = ?", &[Value::String(value.clone())]).await?,
                        Driver::Mysql => bail!("Unsupported MySQL session setting {name}"),
                        // `true`: for this transaction only.
                        Driver::Postgres => conn
                            .run("SELECT set_config($1, $2, true)", &[Value::String(name.clone()), Value::String(value.clone())])
                            .await?,
                    };
                }
            }
            if tx && self.driver == Driver::Mysql {
                // Hold metadata locks until commit and recheck engines under those locks.
                for target in &review.targets { conn.run(&format!("SELECT 1 FROM {target} LIMIT 0 FOR UPDATE"), &[]).await?; }
                if !table_locks && !guards::review(&mut conn, self.driver, &self.database, stmts).await?.non_transactional.is_empty() { bail!("Table engine changed; review the commit again"); }
            }
            let mut out = vec![];
            for (i, st) in stmts.iter().enumerate() {
                if let Some(check) = &st.check {
                    let n = if tx {
                        guards::locked_count(&mut conn, &check.sql, &check.params).await?
                    } else {
                        guards::count(&mut conn, &check.sql, &check.params).await?
                    };
                    if n != check.expect {
                        if check.message == NOT_UNIQUE && n == 0 {
                            bail!("{ROW_MISSING} (matched 0)");
                        }
                        bail!("{} (matched {n})", check.message);
                    }
                }
                if table_locks { guards::precheck_write(&mut conn, &self.database, st).await?; }
                if table_locks { nontransactional_started = true; }
                if table_locks || !tx { self.note_commit(|p| p.wrote_outside_transaction = true); }
                let (r, n) = conn.run(&st.sql, &st.params).await?;
                if let Some(expect) = st.expect {
                    if n != expect {
                        if table_locks { bail!("Expected {expect} row but {n} were affected. This engine cannot roll back; changes may have been saved. Reload the table."); }
                        // Which statement: the caller lists them in order.
                        let which = format!("Statement {} of {}", i + 1, stmts.len());
                        if n == 0 {
                            bail!("{which}: {ROW_MISSING}");
                        }
                        bail!("{which}: expected to change {expect} row but {n} would be affected. Nothing was saved.");
                    }
                }
                out.push(r);
            }
            if tx {
                self.note_commit(|p| p.commit_sent = true);
                conn.run("COMMIT", &[]).await?;
            }
            Ok::<_, anyhow::Error>(out)
        }.await;
        // The app shows only an error's own message, so the cause leads every message here.
        let saved_note = if nontransactional_started {
            "A non-transactional table was written: earlier changes may have been saved and cannot be rolled back. Reload the table."
        } else {
            "Nothing from this commit was saved."
        };
        if let Err(cause) = &result {
            // ROLLBACK or a reset would wait on the dead link for minutes; the server ends the
            // transaction itself when the connection goes, and this one is never reused.
            if is_connection_lost(cause) {
                drop(conn);
                return Err(anyhow::Error::new(CommitInterrupted {
                    cause: format!("{cause:#}"),
                    progress: self.commit_progress(),
                }));
            }
            if tx {
                if let Err(e) = conn.run("ROLLBACK", &[]).await {
                    conn.close().await;
                    self.note_commit(|p| p.finished = true);
                    bail!(
                        "{cause:#}\n\nThe rollback request also failed ({e:#}). The connection was closed, so the server discards the open transaction. {saved_note} Telecat reconnects on the next commit."
                    );
                }
            }
        }
        // Harmless without table locks; restore connection state even after a rejected precheck.
        if tx && self.driver == Driver::Mysql {
            if let Err(e) = async {
                conn.run("UNLOCK TABLES", &[]).await?;
                conn.run("SET autocommit=1", &[]).await?;
                if !settings.is_empty() {
                    conn.run("SET SESSION time_zone = DEFAULT", &[]).await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await
            {
                conn.close().await;
                self.note_commit(|p| p.finished = true);
                bail!(
                    "{}Resetting the commit connection failed ({e:#}); Telecat reconnects on the next commit.",
                    result
                        .as_ref()
                        .err()
                        .map_or(String::new(), |c| format!("{c:#}\n\n"))
                );
            }
        }
        *slot = Some(conn);
        self.note_commit(|p| p.finished = true);
        if nontransactional_started {
            result.map_err(|e| anyhow::anyhow!("{e:#}\n\n{saved_note}"))
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
        self.control_run(&sql).await
    }
    /// Runs `sql` on the control connection, reopening it when an earlier failure closed it.
    async fn control_run(&self, sql: &str) -> Result<QueryResult> {
        let mut slot = self.control.lock().await;
        if slot.as_ref().is_none_or(Wire::is_closed) {
            *slot = Some(Wire::connect(&self.config, &self.password).await?);
        }
        let result = slot
            .as_mut()
            .context("Control connection is closed")?
            .run(sql, &[])
            .await;
        if result.as_ref().is_err_and(is_connection_lost) {
            slot.take();
        }
        result.map(|v| v.0)
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
        let r = self.control_run(&sql).await?;
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
    /// Cancels the running SQL-editor query and any table read in progress. Gives up after
    /// [`CONNECT_TIMEOUT`] when the server can't be reached.
    pub async fn cancel(&self) -> Result<()> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.cancel_now())
            .await
            .context("The server did not answer the cancel request")?
    }
    async fn cancel_now(&self) -> Result<()> {
        let reader = self
            .reader_busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let editor = self.kill(&self.backend_id, false).await;
        match reader {
            Some(id) => self.kill(&id, false).await.or(editor),
            None => editor,
        }
    }
    /// A connection for table reads, with the server's default session settings.
    /// Lock timeouts only: a table the editor's own transaction holds locked fails the read after
    /// 15 s instead of hanging every later table load. They don't change how values render.
    pub(crate) async fn open_reader(&self) -> Result<Wire> {
        let mut reader = Wire::connect(&self.config, &self.password).await?;
        reader
            .run(
                if self.driver == Driver::Postgres {
                    "SET lock_timeout = '15s'"
                } else {
                    "SET SESSION lock_wait_timeout = 15, innodb_lock_wait_timeout = 15"
                },
                &[],
            )
            .await?;
        Ok(reader)
    }
    pub(crate) async fn backend_id_of(&self, conn: &mut Wire) -> Result<String> {
        conn.run(
            if self.driver == Driver::Postgres {
                "SELECT pg_backend_pid()"
            } else {
                "SELECT CONNECTION_ID()"
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
        .context("No backend id")
    }
    /// Closes every connection. A connection still busy, or a server that no longer answers,
    /// is dropped after a short wait instead.
    pub async fn close(&self) {
        for slot in [&self.data, &self.admin, &self.control, &self.reader] {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                if let Some(c) = slot.lock().await.take() {
                    c.close().await;
                }
            })
            .await;
        }
    }
    /// After a reconnect, whether the commit that `progress` (read from the old session)
    /// describes reached the database. `checks` are the MySQL row checks of its statements
    /// (see `telecat_core::changes::mysql_verify_checks`); PostgreSQL asks the server how the
    /// transaction ended. Never writes and never ends the old connection.
    pub async fn verify_interrupted_commit(
        &self,
        progress: &CommitProgress,
        checks: &[telecat_core::changes::RowVerify],
    ) -> Result<CommitVerdict> {
        let unknown = |reason: &str| CommitVerdict::Unknown {
            reason: reason.into(),
            open_backend: None,
        };
        if progress.nothing_sent() && progress.returned {
            return Ok(CommitVerdict::NotApplied);
        }
        if let Some(id) = &progress.backend {
            if self.backend_open(id, progress.identity.as_ref()).await? {
                return Ok(CommitVerdict::Unknown {
                    reason: "The previous connection is still open on the server, so the save could still finish or roll back.".into(),
                    open_backend: Some(id.clone()),
                });
            }
        }
        // The old connection is gone, so a call still hung on it can't send anything more.
        if progress.nothing_sent() {
            return Ok(CommitVerdict::NotApplied);
        }
        if !progress.transaction {
            return Ok(unknown(
                "Its statements commit one by one, so some of them may have been saved.",
            ));
        }
        match self.driver {
            Driver::Postgres => {
                let Some(xid) = progress
                    .xid
                    .as_deref()
                    .filter(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
                else {
                    return Ok(unknown("The server did not report the transaction id."));
                };
                let status = self
                    .control_run(&format!("SELECT txid_status({xid}::bigint)"))
                    .await?
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .cloned()
                    .flatten();
                Ok(match status.as_deref() {
                    Some("committed") => CommitVerdict::Applied,
                    Some("aborted") => CommitVerdict::NotApplied,
                    Some("in progress") => unknown("The server still has the transaction open."),
                    _ => unknown("The server no longer knows how the transaction ended."),
                })
            }
            Driver::Mysql => {
                let mut slot = self.admin.lock().await;
                let mut conn =
                    take_admin(&mut slot, &self.config, &self.password, &self.admin_backend)
                        .await?;
                let mut holds = async |check: &Option<telecat_core::changes::Check>| {
                    Ok::<_, anyhow::Error>(match check {
                        Some(c) => {
                            Some(guards::count(&mut conn, &c.sql, &c.params).await? == c.expect)
                        }
                        None => None,
                    })
                };
                let mut rows = RowReadings {
                    rows: checks.len(),
                    ..Default::default()
                };
                for check in checks {
                    match (
                        holds(&check.applied).await?,
                        holds(&check.not_applied).await?,
                    ) {
                        (Some(true), Some(true)) => rows.both += 1,
                        (Some(true), _) => rows.applied += 1,
                        (_, Some(true)) => rows.not_applied += 1,
                        _ => {}
                    }
                }
                *slot = Some(conn);
                Ok(mysql_verdict(&rows))
            }
        }
    }
    /// Whether the connection `id` (with `identity`, when known) is still open on the server.
    /// When the identity can't be read back, any connection with the id counts: "still open"
    /// only keeps a verdict unknown.
    async fn backend_open(&self, id: &str, identity: Option<&BackendIdentity>) -> Result<bool> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(false);
        }
        if let Some(identity) = identity.filter(|i| i.id == id) {
            if let Ok(found) = self.identity_listed(identity).await {
                return Ok(found);
            }
        }
        let sql = match self.driver {
            Driver::Mysql => {
                format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}")
            }
            Driver::Postgres => format!("SELECT count(*) FROM pg_stat_activity WHERE pid = {id}"),
        };
        Ok(self.count_of(&sql).await? != 0)
    }
    async fn count_of(&self, sql: &str) -> Result<u64> {
        self.control_run(sql)
            .await?
            .rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .flatten()
            .context("No count")?
            .parse()
            .context("Not a count")
    }
    /// The SQL condition naming exactly that connection, or `None` when too little is known.
    fn identity_condition(&self, identity: &BackendIdentity) -> Option<String> {
        if identity.id.is_empty() || !identity.id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let lit = |s: &str| telecat_core::changes::literal(self.driver, s);
        let id = &identity.id;
        Some(match self.driver {
            Driver::Postgres => format!(
                "pid = {id} AND backend_start::text = {} AND application_name = {}",
                lit(identity.started.as_deref()?),
                lit(&pg_application_name(&identity.token))
            ),
            Driver::Mysql => format!(
                "t.PROCESSLIST_ID = {id} AND v.VARIABLE_NAME = 'telecat_connection' AND v.VARIABLE_VALUE = {} AND t.PROCESSLIST_USER = {} AND t.PROCESSLIST_HOST = {}",
                lit(&identity.token),
                lit(identity.user.as_deref()?),
                lit(identity.host.as_deref()?)
            ),
        })
    }
    async fn identity_listed(&self, identity: &BackendIdentity) -> Result<bool> {
        let condition = self
            .identity_condition(identity)
            .context("The connection's identity wasn't recorded")?;
        let sql = match self.driver {
            Driver::Postgres => format!("SELECT count(*) FROM pg_stat_activity WHERE {condition}"),
            Driver::Mysql => format!(
                "SELECT COUNT(*) FROM performance_schema.threads t JOIN performance_schema.user_variables_by_thread v ON v.THREAD_ID = t.THREAD_ID WHERE {condition}"
            ),
        };
        Ok(self.count_of(&sql).await? != 0)
    }
    /// Ends the commit connection of an interrupted save, so it can no longer finish later;
    /// the server rolls back whatever it hadn't committed. Only that exact connection: when
    /// its identity doesn't match (the server reused the id, or it already ended), nothing is
    /// ended.
    pub async fn end_old_backend(&self, progress: &CommitProgress) -> Result<()> {
        const GONE: &str = "The old connection is already gone (or can't be confirmed as Telecat's), so nothing was ended.";
        let Some(identity) = progress.identity.as_ref() else {
            bail!(GONE);
        };
        let Some(condition) = self.identity_condition(identity) else {
            bail!(GONE);
        };
        match self.driver {
            Driver::Postgres => {
                // Matched and signalled in one statement: no window for the pid to be reused.
                let ended = self
                    .control_run(&format!(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE {condition}"
                    ))
                    .await?;
                match ended
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .cloned()
                    .flatten()
                {
                    None => bail!(GONE),
                    Some(ok) if ok == "true" => Ok(()),
                    Some(_) => bail!("The old connection could not be ended (permission denied?)."),
                }
            }
            Driver::Mysql => {
                if !self.identity_listed(identity).await.unwrap_or(false) {
                    bail!(GONE);
                }
                self.kill(&identity.id, true).await
            }
        }
    }
}
