//! Importing a SQL dump file into the selected database on a dedicated connection.
//! See `telecat_core::dump` for how the file is read and what is flagged or blocked.
//!
//! The scan and the per-statement checks are best effort: an import runs the dump's SQL with
//! the connection's privileges. What they do is block the known ways a dump reaches past the
//! target database unless the user allows that kind of statement.
use crate::{ServerError, Session, wire::Wire};
use anyhow::{Context, Result};
use futures_util::SinkExt;
use std::collections::BTreeSet;
use std::hash::Hasher;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;
use telecat_core::{
    dump::{
        BlockKind, DumpItem, DumpRisk, DumpScan, DumpSplitter, classify_in, excerpt, meta_command,
        unsupported,
    },
    position,
    types::Driver,
};

const CHUNK: usize = 64 * 1024;
/// Restat the file every this many chunks while importing.
const RESTAT_EVERY: u64 = 16;
/// The dump as a byte stream, gzip-decompressed when it is a gzip file.
fn open(path: &Path) -> Result<Box<dyn Read + Send>> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Can't open {}", path.display()))?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    let file = std::fs::File::open(path)?;
    Ok(if n == 2 && magic == [0x1f, 0x8b] {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    })
}
/// What the file was when it was checked: the import runs only that exact content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileCheck {
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// A hash of everything read (after decompression).
    pub hash: u64,
}
fn stat(path: &Path) -> Result<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(path)?;
    Ok((m.len(), m.modified().ok()))
}
/// Reads the whole dump once, without keeping it, for the confirmation: statement counts, risks
/// and blocked statements. Runs synchronously; call it off the UI thread.
/// `databases` are the server's databases (see [`Session::list_databases`]).
pub fn scan_dump(
    path: &Path,
    driver: Driver,
    target: &str,
    databases: Vec<String>,
) -> Result<(DumpScan, u64)> {
    scan_dump_checked(path, driver, target, databases).map(|(scan, check)| (scan, check.size))
}
/// [`scan_dump`] with the [`FileCheck`] to pass to the import.
pub fn scan_dump_checked(
    path: &Path,
    driver: Driver,
    target: &str,
    databases: Vec<String>,
) -> Result<(DumpScan, FileCheck)> {
    let (size, modified) = stat(path)?;
    let mut reader = open(path)?;
    let mut splitter = DumpSplitter::new(driver);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut scan = DumpScan {
        databases,
        ..Default::default()
    };
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
        for item in splitter.push(&buf[..n])? {
            scan.add(&item, driver, target)?;
        }
    }
    for item in splitter.finish()? {
        scan.add(&item, driver, target)?;
    }
    Ok((
        scan,
        FileCheck {
            size,
            modified,
            hash: hasher.finish(),
        },
    ))
}
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Kinds of blocked statements the user allowed (see [`BlockKind`]).
    pub allow: BTreeSet<BlockKind>,
    /// PostgreSQL: one transaction, so any error leaves nothing behind.
    pub transaction: bool,
    /// The file as it was checked; the import stops if it differs.
    pub expect: Option<FileCheck>,
}
/// Live counters the UI shows while importing.
#[derive(Debug, Default)]
pub struct ImportProgress {
    pub statements: AtomicU64,
    pub bytes: AtomicU64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub statements: u64,
    pub copied_rows: u64,
    pub transaction: bool,
}
/// What an import that stopped left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The import's own transaction was still open and was rolled back.
    RolledBack,
    /// Nothing was in a transaction (MySQL, or a dump that can't run in one).
    Partial,
    /// The rollback can't be confirmed.
    Unknown,
    /// The connection dropped after the final COMMIT, which a later check found committed.
    Committed,
}
/// Why an import stopped, and where in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    pub line: usize,
    pub column: Option<usize>,
    pub excerpt: String,
    pub message: String,
    pub executed: u64,
    pub rolled_back: bool,
    pub cancelled: bool,
    pub outcome: Outcome,
}
impl std::fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.cancelled {
            write!(f, "Import cancelled at line {}", self.line)?;
        } else {
            match self.column {
                Some(c) => write!(
                    f,
                    "Error at line {}, column {c}: {}",
                    self.line, self.message
                )?,
                None => write!(f, "Error at line {}: {}", self.line, self.message)?,
            }
        }
        if !self.excerpt.is_empty() {
            write!(f, "\n{}", self.excerpt)?;
        }
        write!(
            f,
            "\n\n{} statement{} ran before it stopped. {}",
            self.executed,
            if self.executed == 1 { "" } else { "s" },
            match self.outcome {
                Outcome::RolledBack => "Everything was rolled back: the database is as it was.",
                Outcome::Partial => "Statements that ran before it may have been saved.",
                Outcome::Unknown =>
                    "The rollback couldn't be confirmed: statements that ran before it may have been saved.",
                Outcome::Committed =>
                    "The server committed the import before the connection dropped: everything in it was saved.",
            }
        )
    }
}
/// Where in the file an error points: the statement's line plus the server's position in it,
/// or the line within a function body, or the COPY data row.
fn located(e: &anyhow::Error, sql: &str, line: usize, d: Driver) -> (usize, Option<usize>) {
    let server = e.downcast_ref::<ServerError>();
    let at = match (d, server.and_then(|s| s.position)) {
        (Driver::Postgres, Some(p)) => Some(position::postgres(sql, 0, p)),
        (Driver::Mysql, _) => position::mysql(sql, 0, sql, &e.to_string()),
        (Driver::Postgres, None) => server.and_then(|s| {
            let context = s.context.as_deref()?;
            let internal = s.internal.as_ref().map(|(p, q)| (*p, q.as_str()));
            position::plpgsql(sql, 0, sql, context, internal)
        }),
    };
    match at {
        Some(at) => (line + at.line - 1, Some(at.column)),
        None => (line, None),
    }
}
/// The COPY data row an error points at: "COPY t, line N" counts data lines from 1.
fn copy_row_line(e: &anyhow::Error, data_line: usize) -> Option<usize> {
    let context = e.downcast_ref::<ServerError>()?.context.as_deref()?;
    let n: usize = regex::Regex::new(r"COPY [^,]+, line (\d+)")
        .ok()?
        .captures(context)?[1]
        .parse()
        .ok()?;
    Some(data_line + n - 1)
}
impl Session {
    /// Imports `path` into `target` on a dedicated connection to it. Stops at the first error;
    /// with `opts.transaction` (PostgreSQL) an error or cancel rolls everything back.
    pub async fn import_dump(
        &self,
        path: &Path,
        target: &str,
        opts: ImportOptions,
        progress: &ImportProgress,
        cancel: &AtomicBool,
    ) -> std::result::Result<ImportReport, ImportFailure> {
        let d = self.driver;
        let transaction = opts.transaction && d == Driver::Postgres;
        let fail = |line: usize, message: String| ImportFailure {
            line,
            column: None,
            excerpt: String::new(),
            message,
            executed: 0,
            rolled_back: false,
            cancelled: false,
            outcome: Outcome::Partial,
        };
        let mut config = self.config.clone();
        config.database = target.to_owned();
        let mut conn = Wire::connect(&config, &self.password)
            .await
            .map_err(|e| fail(0, format!("Can't connect to {target}: {e:#}")))?;
        let backend = self
            .backend_id_of(&mut conn)
            .await
            .map_err(|e| fail(0, e.to_string()))?;
        *self.importer_busy.lock().unwrap_or_else(|e| e.into_inner()) = Some(backend.clone());
        // What a check after a dropped connection needs (see `import_progress`). Without a
        // transaction every statement commits on its own.
        *self.import.lock().unwrap_or_else(|e| e.into_inner()) = crate::CommitProgress {
            transaction,
            wrote_outside_transaction: !transaction,
            backend: Some(backend),
            ..Default::default()
        };
        // The import's own transaction, identified by its id, so a rollback is only claimed when
        // that transaction is verifiably still open.
        let mut xid = None;
        if transaction {
            conn.execute_raw("BEGIN")
                .await
                .map_err(|e| fail(0, e.to_string()))?;
            xid = conn
                .run("SELECT txid_current()::text", &[])
                .await
                .ok()
                .and_then(|r| r.0.rows.into_iter().next())
                .and_then(|r| r.into_iter().next().flatten());
            self.import.lock().unwrap_or_else(|e| e.into_inner()).xid = xid.clone();
        }
        let result = self
            .import_items(&mut conn, path, target, &opts, progress, cancel)
            .await;
        *self.importer_busy.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let result = match result {
            Ok(report) if transaction => match {
                self.import
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .commit_sent = true;
                conn.execute_raw("COMMIT").await
            } {
                Ok(()) => Ok(report),
                Err(e) => Err(ImportFailure {
                    rolled_back: true,
                    outcome: Outcome::RolledBack,
                    ..fail(0, format!("The final COMMIT failed: {e:#}"))
                }),
            },
            Ok(report) => Ok(report),
            Err(mut f) => {
                f.outcome = Outcome::Partial;
                if transaction {
                    // A dead link would hold these for minutes: unconfirmed means unknown.
                    let open = tokio::time::timeout(
                        crate::CONNECT_TIMEOUT,
                        still_in(&mut conn, xid.as_deref()),
                    )
                    .await
                    .unwrap_or(false);
                    let rolled = open
                        && tokio::time::timeout(
                            crate::CONNECT_TIMEOUT,
                            conn.execute_raw("ROLLBACK"),
                        )
                        .await
                        .is_ok_and(|r| r.is_ok());
                    f.outcome = if open && rolled {
                        Outcome::RolledBack
                    } else {
                        Outcome::Unknown
                    };
                }
                f.rolled_back = f.outcome == Outcome::RolledBack;
                Err(f)
            }
        };
        {
            let mut progress = self.import.lock().unwrap_or_else(|e| e.into_inner());
            progress.finished = true;
            progress.returned = true;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), conn.close()).await;
        result.map(|r| ImportReport { transaction, ..r })
    }
    /// Stops a running import: the statement in progress is cancelled on the server.
    pub async fn cancel_import(&self) -> Result<()> {
        let id = self
            .importer_busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match id {
            Some(id) => self.kill(&id, false).await,
            None => Ok(()),
        }
    }
    async fn import_items(
        &self,
        conn: &mut Wire,
        path: &Path,
        target: &str,
        opts: &ImportOptions,
        progress: &ImportProgress,
        cancel: &AtomicBool,
    ) -> std::result::Result<ImportReport, ImportFailure> {
        let d = self.driver;
        let transaction = opts.transaction && d == Driver::Postgres;
        let mut report = ImportReport {
            statements: 0,
            copied_rows: 0,
            transaction: false,
        };
        let failure = |line: usize, message: String, report: &ImportReport| ImportFailure {
            line,
            column: None,
            excerpt: String::new(),
            message,
            executed: report.statements,
            rolled_back: false,
            cancelled: false,
            outcome: Outcome::Partial,
        };
        if let Some(check) = opts.expect {
            let now = stat(path).map_err(|e| failure(0, e.to_string(), &report))?;
            if now != (check.size, check.modified) {
                return Err(failure(
                    0,
                    "The file changed after it was checked. Nothing ran; import it again.".into(),
                    &report,
                ));
            }
        }
        let mut reader = open(path).map_err(|e| failure(0, e.to_string(), &report))?;
        let databases: Vec<String> = if d == Driver::Mysql {
            conn.run("SHOW DATABASES", &[])
                .await
                .map_err(|e| failure(0, e.to_string(), &report))?
                .0
                .rows
                .into_iter()
                .filter_map(|r| r.into_iter().next().flatten())
                .collect()
        } else {
            vec![]
        };
        let mut splitter = DumpSplitter::new(d);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        type Sink = std::pin::Pin<Box<tokio_postgres::CopyInSink<bytes::Bytes>>>;
        let mut copy: Option<(Sink, usize, String)> = None;
        let mut buf = vec![0u8; CHUNK];
        let mut done = false;
        let mut line = 0usize;
        let mut chunks = 0u64;
        while !done {
            let n = reader
                .read(&mut buf)
                .map_err(|e| failure(line, e.to_string(), &report))?;
            progress.bytes.fetch_add(n as u64, Ordering::Relaxed);
            hasher.write(&buf[..n]);
            chunks += 1;
            if let Some(check) = opts.expect.filter(|_| chunks % RESTAT_EVERY == 0) {
                if stat(path).ok() != Some((check.size, check.modified)) {
                    return Err(failure(
                        line,
                        "The file changed while it was being imported.".into(),
                        &report,
                    ));
                }
            }
            let items = if n == 0 {
                done = true;
                if let Some(check) = opts.expect {
                    if hasher.finish() != check.hash {
                        return Err(failure(
                            line,
                            "The file's content differs from what was checked.".into(),
                            &report,
                        ));
                    }
                }
                std::mem::replace(&mut splitter, DumpSplitter::new(d)).finish()
            } else {
                splitter.push(&buf[..n])
            }
            .map_err(|e| failure(line, e.to_string(), &report))?;
            for item in items {
                if cancel.load(Ordering::SeqCst) {
                    return Err(ImportFailure {
                        cancelled: true,
                        ..failure(line, "Cancelled".into(), &report)
                    });
                }
                match item {
                    DumpItem::Statement { sql, line: at } => {
                        line = at;
                        let refuse = |message: String, report: &ImportReport| ImportFailure {
                            excerpt: excerpt(&sql, 200),
                            ..failure(at, message, report)
                        };
                        if let Some(why) = unsupported(&sql, d) {
                            return Err(refuse(
                                format!("Refused: {why}. Nothing from it ran."),
                                &report,
                            ));
                        }
                        let info = classify_in(&sql, d, target, &databases);
                        if let Some(r) = info
                            .risks
                            .iter()
                            .find(|r| r.block_kind().is_some_and(|k| !opts.allow.contains(&k)))
                        {
                            return Err(refuse(
                                format!("Blocked: {}. Nothing from it ran.", r.label()),
                                &report,
                            ));
                        }
                        if transaction && info.risks.contains(&DumpRisk::TransactionControl) {
                            return Err(refuse(
                                "Refused: transaction control in a dump imported as one transaction. Nothing from it ran.".into(),
                                &report,
                            ));
                        }
                        let copy_in = d == Driver::Postgres
                            && telecat_core::dump::words(&sql, d)
                                .first()
                                .is_some_and(|w| w == "COPY")
                            && sql.to_ascii_uppercase().contains("STDIN");
                        let run = if copy_in {
                            conn.copy_in(&sql)
                                .await
                                .map(|sink| copy = Some((Box::pin(sink), at, sql.clone())))
                        } else {
                            conn.execute_raw(&sql).await
                        };
                        if let Err(e) = run {
                            let (line, column) = located(&e, &sql, at, d);
                            return Err(ImportFailure {
                                column,
                                excerpt: excerpt(&sql, 200),
                                cancelled: cancel.load(Ordering::SeqCst),
                                ..failure(line, format!("{e:#}"), &report)
                            });
                        }
                        report.statements += 1;
                        progress
                            .statements
                            .store(report.statements, Ordering::Relaxed);
                    }
                    DumpItem::CopyData(data) => {
                        let Some((sink, at, sql)) = copy.as_mut() else {
                            return Err(failure(
                                line,
                                "COPY data without a COPY statement".into(),
                                &report,
                            ));
                        };
                        if let Err(e) = sink.as_mut().send(bytes::Bytes::from(data)).await {
                            return Err(ImportFailure {
                                excerpt: excerpt(sql, 200),
                                ..failure(
                                    *at,
                                    format!("{:#}", crate::wire::server_error(e.into())),
                                    &report,
                                )
                            });
                        }
                    }
                    DumpItem::CopyEnd {
                        line: end,
                        data_line,
                    } => {
                        line = end;
                        let Some((mut sink, at, sql)) = copy.take() else {
                            return Err(failure(
                                end,
                                "\\. without a COPY statement".into(),
                                &report,
                            ));
                        };
                        match sink.as_mut().finish().await {
                            Ok(rows) => report.copied_rows += rows,
                            Err(e) => {
                                let e = crate::wire::server_error(e.into());
                                return Err(ImportFailure {
                                    excerpt: excerpt(&sql, 200),
                                    cancelled: cancel.load(Ordering::SeqCst),
                                    ..failure(
                                        copy_row_line(&e, data_line).unwrap_or(at),
                                        format!("{e:#}"),
                                        &report,
                                    )
                                });
                            }
                        }
                    }
                    DumpItem::Meta { command, line: at } => {
                        line = at;
                        match meta_command(&command, target) {
                            Ok(true) => {}
                            Ok(false) => {
                                return Err(failure(
                                    at,
                                    format!(
                                        "Refused: {command} switches to another database. Import that part into its own database instead."
                                    ),
                                    &report,
                                ));
                            }
                            Err(e) => return Err(failure(at, e.to_string(), &report)),
                        }
                    }
                }
            }
        }
        Ok(report)
    }
}
/// True when the import's transaction (`xid`) is verifiably still open: in a failed
/// transaction every query errors with 25P02; otherwise the current transaction id must match.
async fn still_in(conn: &mut Wire, xid: Option<&str>) -> bool {
    match conn
        .run("SELECT txid_current_if_assigned()::text", &[])
        .await
    {
        Err(e) => {
            e.downcast_ref::<ServerError>()
                .and_then(|s| s.code.as_deref())
                == Some("25P02")
        }
        Ok(r) => {
            let now =
                r.0.rows
                    .into_iter()
                    .next()
                    .and_then(|r| r.into_iter().next().flatten());
            xid.is_some() && now.as_deref() == xid
        }
    }
}
