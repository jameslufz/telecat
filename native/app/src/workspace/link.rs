//! Whether the workspace's connection is alive, and getting it back.
//!
//! A drop is noticed from an error that says so or from a cheap keepalive ping. The pill in
//! the toolbar then says so, and reconnecting (⌘⇧R, a click on the pill, or automatically
//! with backoff) opens a whole new session with the same settings. Work in flight is handled
//! honestly: a query is reported interrupted and never re-run, an interrupted save becomes
//! "result unknown" until it is checked, and staged edits stay.
use super::*;
use std::time::{Duration, Instant};
use telecat_db::{CommitVerdict, ImportProgress, Session, is_auth_failure, is_connection_lost};

/// Seconds between automatic reconnect attempts: about a minute in all, then it's the user's
/// call (⌘⇧R).
const BACKOFF: [u64; 7] = [1, 2, 5, 10, 10, 15, 15];
/// A ping that times out may only mean a busy server: this many in a row mean the link is gone.
const PING_TIMEOUTS: u32 = 2;
/// MySQL's "not applied", read from the rows: honest that another session could explain it.
pub(super) const NOT_APPLIED_FROM_ROWS: &str = "The rows still hold the values they had when you loaded them, so the save most likely didn't reach the database. Your changes are still staged. If another session may have changed them back, check the table before saving again.";
/// How long checking an interrupted save may take before it counts as unreachable.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Link {
    Connected,
    Disconnected {
        error: String,
        /// When the next automatic attempt starts, if one is scheduled.
        retry_at: Option<Instant>,
    },
    Reconnecting {
        attempt: u32,
        error: String,
    },
}
pub(super) struct LinkState {
    pub(super) link: Link,
    /// Bumped whenever an attempt starts, is cancelled or succeeds: timers and results of an
    /// older one are then ignored.
    generation: u64,
    /// Attempts in the current round of automatic retries.
    attempts: u32,
    ping_failures: u32,
    pinging: bool,
    rechecking: bool,
    /// The current manual reconnect started from a session that still worked.
    from_healthy: bool,
    pub(super) keepalive_every: Duration,
    pub(super) ping_timeout: Duration,
    keepalive: Option<Task<()>>,
    activation: Option<Subscription>,
}
impl Default for LinkState {
    fn default() -> Self {
        Self {
            link: Link::Connected,
            generation: 0,
            attempts: 0,
            ping_failures: 0,
            pinging: false,
            rechecking: false,
            from_healthy: false,
            keepalive_every: Duration::from_secs(45),
            ping_timeout: Duration::from_secs(5),
            keepalive: None,
            activation: None,
        }
    }
}
/// A save running on `session`; see [`Workspace::connection_lost`].
pub(super) struct InFlightCommit {
    pub(super) run: u64,
    pub(super) session: Arc<Session>,
    pub(super) batch: CommitBatch,
}
/// A save cut off by a lost connection: it may or may not have reached the database. ⌘S stays
/// blocked until it is checked or the user has reviewed it.
pub(super) struct UnknownCommit {
    pub(super) run: u64,
    old: Arc<Session>,
    batch: CommitBatch,
    pub(super) verdict: Option<CommitVerdict>,
    pub(super) checking: bool,
    /// The statements of the interrupted save, written to the profile.
    pub(super) file: Option<std::path::PathBuf>,
}
/// An import running on `session`'s own connection.
pub(super) struct InFlightImport {
    pub(super) session: Arc<Session>,
    pub(super) abort: tokio::task::AbortHandle,
    pub(super) file: String,
    pub(super) progress: Arc<ImportProgress>,
}
enum Credentials {
    /// The password the session already holds (automatic retries never prompt).
    Session,
    /// From the connection's password source again; `Some` when the user just typed it.
    Source(Option<String>),
}
impl Workspace {
    pub(super) fn connected(&self) -> bool {
        self.link.link == Link::Connected
    }
    /// Pings now and then, and whenever the window becomes active, so a dropped link is noticed
    /// before the user acts. Runs on the worker runtime: it never blocks the UI.
    pub(super) fn start_keepalive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.services.scenario || self.session.is_none() {
            return;
        }
        self.link.keepalive = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(every) = this.update(cx, |this, _| this.link.keepalive_every) else {
                    break;
                };
                cx.background_executor().timer(every).await;
                if this.update(cx, |this, cx| this.ping(cx)).is_err() {
                    break;
                }
            }
        }));
        self.link.activation = Some(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.ping(cx);
            }
        }));
    }
    pub(super) fn ping(&mut self, cx: &mut Context<Self>) {
        if self.link.pinging || !self.connected() {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        self.link.pinging = true;
        let (timeout, generation) = (self.link.ping_timeout, self.link.generation);
        let task = self
            .services
            .runtime
            .spawn(async move { session.ping(timeout).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.link.pinging = false;
                if this.link.generation != generation || !this.connected() {
                    return;
                }
                match result {
                    Ok(Ok(())) => this.link.ping_failures = 0,
                    Ok(Err(e)) if is_connection_lost(&e) => {
                        let timed_out = e.chain().any(|c| c.is::<tokio::time::error::Elapsed>());
                        this.link.ping_failures += 1;
                        if !timed_out || this.link.ping_failures >= PING_TIMEOUTS {
                            this.connection_lost(format!("{e:#}"), cx);
                        } else {
                            // Ask again soon rather than wait a whole keepalive interval.
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(Duration::from_secs(1)).await;
                                let _ = this.update(cx, |this, cx| this.ping(cx));
                            })
                            .detach();
                        }
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }
    /// Reports a failed database call: a lost connection goes to the status pill (and starts
    /// reconnecting) instead of a modal error that would block ⌘⇧R.
    pub(super) fn failed(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        if is_connection_lost(&e) {
            self.connection_lost(format!("{e:#}"), cx);
        } else {
            self.error(e, cx);
        }
    }
    /// The connection is gone: say so, settle what was in flight, and start retrying.
    pub(super) fn connection_lost(&mut self, error: String, cx: &mut Context<Self>) {
        if self.services.scenario || self.session.is_none() {
            return;
        }
        if !self.connected() {
            return;
        }
        self.link.generation += 1;
        self.link.attempts = 0;
        self.link.ping_failures = 0;
        self.link.from_healthy = false;
        // Disconnected first, so nothing settled below starts work on the dead session.
        self.link.link = Link::Disconnected {
            error: error.clone(),
            retry_at: None,
        };
        self.state.logs.push(LogEntry {
            time: timestamp(),
            sql: format!("-- Connection lost ({error}). Staged changes are kept."),
        });
        self.interrupt_in_flight(&error, cx);
        self.schedule_retry(error, cx);
    }
    /// [`Self::failed`] for a call made on `session`: a session already replaced by a
    /// reconnect says nothing about the current one.
    pub(super) fn failed_on(
        &mut self,
        session: &Arc<Session>,
        e: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        if self.is_current(session) {
            self.failed(e, cx);
        } else if !is_connection_lost(&e) {
            self.error(e, cx);
        }
    }
    pub(super) fn is_current(&self, session: &Arc<Session>) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, session))
    }
    fn interrupt_in_flight(&mut self, error: &str, cx: &mut Context<Self>) {
        if self.busy {
            // Never re-run: the user decides once they can see what happened.
            if let Some(abort) = self.query_abort.take() {
                abort.abort();
            }
            self.query_run += 1;
            self.busy = false;
            if let Some(id) = self.query_tab.take() {
                self.end_load(
                    Some((id, LoadSlot::Results)),
                    &Err::<(), _>("was interrupted: the connection was lost"),
                );
                if let Some(t) = self.state.tabs.iter_mut().find(|t| t.id == id) {
                    t.set_results(vec![QueryResult {
                        error: Some(format!("{INTERRUPTED} ({error})")),
                        ..Default::default()
                    }]);
                    t.message = true;
                }
            }
        }
        if let Some(c) = self.commit_in_flight.take() {
            self.mark_commit_unknown(c.run, c.session, c.batch, cx);
            self.saving = false;
        }
        if let Some(import) = self.import_in_flight.take() {
            import.abort.abort();
            self.saving = false;
            let executed = import
                .progress
                .statements
                .load(std::sync::atomic::Ordering::Relaxed);
            let transaction = import.session.import_progress().transaction;
            self.sheet = Some(Sheet::Import(import::ImportStage::Done {
                file: import.file.clone(),
                result: Err(telecat_db::ImportFailure {
                    line: 0,
                    column: None,
                    excerpt: String::new(),
                    message: format!("The connection was lost during the import ({error})."),
                    executed,
                    rolled_back: false,
                    cancelled: false,
                    outcome: if transaction {
                        telecat_db::Outcome::Unknown
                    } else {
                        telecat_db::Outcome::Partial
                    },
                }),
                seconds: 0.,
            }));
            self.import_check = Some((import.session, import.file));
        }
        cx.notify();
    }
    /// Disconnected, with the next automatic attempt scheduled while the round lasts.
    fn schedule_retry(&mut self, error: String, cx: &mut Context<Self>) {
        let Some(&wait) = BACKOFF.get(self.link.attempts as usize) else {
            self.link.link = Link::Disconnected {
                error,
                retry_at: None,
            };
            cx.notify();
            return;
        };
        let wait = Duration::from_secs(wait);
        self.link.link = Link::Disconnected {
            error,
            retry_at: Some(Instant::now() + wait),
        };
        let generation = self.link.generation;
        cx.spawn(async move |this, cx| {
            // Repaint the countdown every second until the attempt starts.
            let until = Instant::now() + wait;
            while Instant::now() < until {
                let left = until.saturating_duration_since(Instant::now());
                cx.background_executor()
                    .timer(left.min(Duration::from_secs(1)))
                    .await;
                let current = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.link.generation == generation
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.link.generation == generation {
                    this.attempt(Credentials::Session, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }
    /// ⌘⇧R or a click on the pill: reconnect now, starting the backoff over. Re-reads the
    /// password from its source, and asks for it when the connection is set to ask.
    pub(super) fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.services.scenario || self.session.is_none() {
            return;
        }
        self.link.generation += 1;
        self.link.attempts = 0;
        let in_flight =
            self.busy || self.commit_in_flight.is_some() || self.import_in_flight.is_some();
        // From a session that still works, cancelling goes back to it.
        self.link.from_healthy = self.connected() && !in_flight;
        if self.connected() && in_flight {
            // Work hung on the old session never comes back on the new one: settle it the way
            // a detected drop does (a save becomes "result unknown", a query interrupted).
            let error = "You asked to reconnect.".to_owned();
            self.link.link = Link::Disconnected {
                error: error.clone(),
                retry_at: None,
            };
            self.interrupt_in_flight(&error, cx);
        }
        if self.state.connection.password_mode == PasswordMode::Ask {
            self.gate_password
                .update(cx, |s, cx| s.set_value("", window, cx));
            self.gate_focused = false;
            self.sheet = Some(Sheet::ReconnectPassword);
            cx.notify();
            return;
        }
        self.attempt(Credentials::Source(None), cx);
    }
    pub(super) fn submit_reconnect_password(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typed = self.gate_password.read(cx).value().to_string();
        self.gate_password
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.sheet = None;
        self.attempt(Credentials::Source(Some(typed)), cx);
    }
    /// Stops reconnecting, automatic retries included, until ⌘⇧R.
    pub(super) fn cancel_reconnect(&mut self, cx: &mut Context<Self>) {
        if self.connected() {
            return;
        }
        self.link.generation += 1;
        if self.link.from_healthy {
            self.link.from_healthy = false;
            self.link.link = Link::Connected;
            cx.notify();
            return;
        }
        let error = self.link_error();
        self.link.link = Link::Disconnected {
            error,
            retry_at: None,
        };
        cx.notify();
    }
    fn link_error(&self) -> String {
        match &self.link.link {
            Link::Connected => String::new(),
            Link::Disconnected { error, .. } | Link::Reconnecting { error, .. } => error.clone(),
        }
    }
    fn attempt(&mut self, credentials: Credentials, cx: &mut Context<Self>) {
        let Some(old) = self.session.clone() else {
            return;
        };
        self.link.generation += 1;
        self.link.attempts += 1;
        let generation = self.link.generation;
        self.link.link = Link::Reconnecting {
            attempt: self.link.attempts,
            error: self.link_error(),
        };
        let keychain = self.services.profile.keychain.clone();
        let task = self.services.runtime.spawn(async move {
            match credentials {
                Credentials::Session => old.reopen().await,
                Credentials::Source(typed) => {
                    // Same server, database, user and TLS settings; only the password is read
                    // again, so a changed keychain entry or password command is picked up.
                    let config = old.config().clone();
                    let password = telecat_app::credentials::resolve_password(
                        &config,
                        typed.as_deref(),
                        keychain.as_ref(),
                    )
                    .await?;
                    Session::connect(&config, &password).await
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // A cancelled or superseded attempt: its session, if any, is dropped unused.
                if this.link.generation != generation {
                    return;
                }
                match result {
                    Ok(Ok(session)) => this.reconnected(session, cx),
                    Ok(Err(e)) => this.reconnect_failed(e, cx),
                    Err(e) => this.reconnect_failed(e.into(), cx),
                }
            });
        })
        .detach();
        cx.notify();
    }
    fn reconnect_failed(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        let error = format!("{e:#}");
        self.state.logs.push(LogEntry {
            time: timestamp(),
            sql: format!(
                "-- Reconnect attempt {} failed: {error}",
                self.link.attempts
            ),
        });
        if self.link.from_healthy {
            // The session it started from still works: keep using it.
            self.link.from_healthy = false;
            self.link.link = Link::Connected;
            self.show_hint(
                format!("Reconnect failed: {e}. The existing connection is still in use."),
                cx,
            );
            self.ping(cx);
            return;
        }
        if is_auth_failure(&e) || !is_connection_lost(&e) {
            // Retrying the same credentials can't help, and could lock the account.
            self.show_hint(format!("Reconnect failed: {e}"), cx);
            self.link.link = Link::Disconnected {
                error,
                retry_at: None,
            };
            cx.notify();
            return;
        }
        if BACKOFF.get(self.link.attempts as usize).is_none() {
            self.show_hint(
                format!("Still can't reach the server: {e}. Press ⌘⇧R to try again."),
                cx,
            );
        }
        self.schedule_retry(error, cx);
    }
    fn reconnected(&mut self, session: Session, cx: &mut Context<Self>) {
        let session = Arc::new(session);
        if let Some(old) = self.session.replace(session.clone()) {
            // Whatever the old connections still hold is dropped; nothing they ran is repeated.
            self.services
                .runtime
                .spawn(async move { old.close().await });
        }
        self.link.generation += 1;
        self.link.attempts = 0;
        self.link.from_healthy = false;
        self.link.ping_failures = 0;
        self.link.link = Link::Connected;
        self.state.server_version = session.server_version.clone();
        self.state.tls = session.tls.clone();
        self.state.user = session.user.clone();
        self.state.database = session.database.clone();
        let time = timestamp();
        let note = format!(
            "-- Reconnected at {}. This tab's server session started over: session variables, temporary tables and any open transaction from before are gone. Nothing was run again.",
            time.split_whitespace()
                .nth(1)
                .unwrap_or(&time)
                .split('.')
                .next()
                .unwrap_or("")
        );
        let driver = self.state.connection.driver;
        for t in self
            .state
            .tabs
            .iter_mut()
            .filter(|t| t.kind == TabKind::Query)
        {
            t.notes.push(note.clone());
            // A result's edit plan (and the settings its values were read under) belongs to the
            // old session. Staged edits stay, checked against the new session before saving.
            if let Some(r) = t.result_edit.as_mut() {
                if r.data.dirty() {
                    r.recheck = true;
                } else {
                    r.plan = telecat_core::result_edit::ResultEditPlan::read_only(
                        driver,
                        r.data.result.columns.len(),
                        RESULT_BEFORE_RECONNECT,
                    );
                }
            }
        }
        self.state.logs.push(LogEntry { time, sql: note });
        self.show_hint(
            "Reconnected. The SQL editor's session was reset; staged changes are kept.",
            cx,
        );
        self.refresh_objects(cx);
        self.load_databases(cx);
        self.reload_clean_tabs(cx);
        if self.commit_unknown.is_some() {
            self.verify_commit(cx);
        }
        if self.import_check.is_some() {
            self.verify_import(cx);
        }
        cx.notify();
    }
    /// Reloads tables without staged edits, keeping their filters, page and selection. Tables
    /// with staged edits keep the rows those edits refer to.
    fn reload_clean_tabs(&mut self, cx: &mut Context<Self>) {
        let active = self.state.active;
        let ids: Vec<_> = self
            .state
            .tabs
            .iter()
            .filter(|t| match t.kind {
                TabKind::Table(_) => t.table.as_ref().is_some_and(|e| !e.dirty() && !e.is_new),
                TabKind::Running => true,
                TabKind::Query => false,
            })
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.state.active = id;
            self.refresh_tab(cx);
        }
        self.state.active = active;
    }
    /// Keeps a save the connection dropped during: its outcome is unknown until checked.
    pub(super) fn mark_commit_unknown(
        &mut self,
        run: u64,
        old: Arc<Session>,
        batch: CommitBatch,
        cx: &mut Context<Self>,
    ) {
        let driver = self.state.connection.driver;
        let sql = batch
            .statements
            .iter()
            .map(|s| telecat_core::changes::inline_params(driver, s))
            .collect::<Vec<_>>()
            .join("\n");
        let file = telecat_app::recovery::write_interrupted(
            &self.services.profile.root,
            &self.state.connection.name,
            &sql,
        );
        self.state.logs.push(LogEntry {
            time: timestamp(),
            sql: "-- Save interrupted: the connection was lost before the server confirmed it. Its result is unknown until checked; nothing will be retried.".into(),
        });
        self.commit_unknown = Some(UnknownCommit {
            run,
            old,
            batch,
            verdict: None,
            checking: false,
            file,
        });
        if self.connected() {
            self.verify_commit(cx);
        }
        cx.notify();
    }
    /// Asks the server whether the interrupted save reached it.
    pub(super) fn verify_commit(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone().filter(|_| self.connected()) else {
            return;
        };
        let Some(unknown) = self.commit_unknown.as_mut().filter(|u| !u.checking) else {
            return;
        };
        unknown.checking = true;
        let (run, old, checks) = (
            unknown.run,
            unknown.old.clone(),
            unknown.batch.verify.clone(),
        );
        let task = self.services.runtime.spawn({
            let session = session.clone();
            async move {
                tokio::time::timeout(
                    CHECK_TIMEOUT,
                    check_settled(&session, || old.commit_progress(), &checks),
                )
                .await?
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(u) = this.commit_unknown.as_mut().filter(|u| u.run == run) {
                    u.checking = false;
                }
                match result {
                    Ok(Ok(verdict)) => this.resolve_commit(run, verdict, cx),
                    Ok(Err(e)) => this.failed_on(&session, e, cx),
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    /// Whether some staged result edits were made on a session a reconnect replaced; if so,
    /// starts checking them against the current session and says so.
    pub(super) fn results_need_recheck(&mut self, cx: &mut Context<Self>) -> bool {
        let stale: Vec<u64> = self
            .state
            .tabs
            .iter()
            .filter(|t| {
                t.result_edit
                    .as_ref()
                    .is_some_and(|r| r.recheck && r.data.dirty())
            })
            .map(|t| t.id)
            .collect();
        if stale.is_empty() {
            return false;
        }
        self.show_hint(
            "The edited results were read before the reconnect: checking them against the new connection first…",
            cx,
        );
        if self.link.rechecking {
            return true;
        }
        let Some(session) = self.session.clone() else {
            return true;
        };
        self.link.rechecking = true;
        for id in stale {
            let Some(t) = self.state.tabs.iter().find(|t| t.id == id) else {
                continue;
            };
            let Some(r) = t.result_edit.as_ref() else {
                continue;
            };
            let (sql, result, old) = (
                t.results.first().map(|r| r.sql.clone()).unwrap_or_default(),
                r.data.result.clone(),
                r.plan.clone(),
            );
            let task = self.services.runtime.spawn({
                let session = session.clone();
                async move {
                    tokio::time::timeout(CHECK_TIMEOUT, session.result_edit_plan(&sql, &result))
                        .await?
                }
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.link.rechecking = false;
                    // The same tables, columns, keys and editable columns: the staged edits still
                    // mean what they meant. The settings their values were typed under are kept.
                    let drift = match &result {
                        Ok(Ok(new)) => new.drift_from(&old),
                        Ok(Err(e)) => Some(e.to_string()),
                        Err(e) => Some(e.to_string()),
                    };
                    if let Some(why) = drift {
                        this.show_hint(
                            format!(
                                "The edited results can't be saved after the reconnect ({why}). Nothing was saved: discard the edits and re-run the query."
                            ),
                            cx,
                        );
                    } else {
                        if let Some(r) = this
                            .state
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == id)
                            .and_then(|t| t.result_edit.as_mut())
                        {
                            r.recheck = false;
                        }
                        this.show_hint("Checked against the new connection: press ⌘S to save.", cx);
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        true
    }
    /// Settles save `run` once its outcome is known (a check, or the old call returning late).
    pub(super) fn resolve_commit(
        &mut self,
        run: u64,
        verdict: CommitVerdict,
        cx: &mut Context<Self>,
    ) {
        let Some(unknown) = self.commit_unknown.as_mut().filter(|u| u.run == run) else {
            return;
        };
        let message = match &verdict {
            CommitVerdict::Applied => {
                "The interrupted save reached the database: those changes are saved."
            }
            // PostgreSQL says how the transaction ended. MySQL's rows can't tell "never
            // applied" from "applied, then changed back by another session".
            CommitVerdict::NotApplied if self.state.connection.driver == Driver::Postgres => {
                "The server rolled the interrupted save back: nothing from it was saved. Your changes are still staged: press ⌘S to save them."
            }
            CommitVerdict::NotApplied => NOT_APPLIED_FROM_ROWS,
            CommitVerdict::Unknown { .. } => {
                unknown.verdict = Some(verdict);
                self.sheet = Some(Sheet::CommitUnknown);
                cx.notify();
                return;
            }
        };
        let unknown = self.commit_unknown.take().expect("checked above");
        if verdict == CommitVerdict::Applied {
            for target in &unknown.batch.targets {
                self.state.committed(target);
            }
            self.reset_inspector();
            self.table_inputs.clear();
            self.refresh_objects(cx);
            self.reload_clean_tabs(cx);
        }
        if matches!(self.sheet, Some(Sheet::CommitUnknown)) {
            self.sheet = None;
        }
        self.state.logs.push(LogEntry {
            time: timestamp(),
            sql: format!("-- {message}"),
        });
        self.show_hint(message, cx);
        cx.notify();
    }
    /// Ends the previous commit connection, which the server still has open, so the
    /// interrupted save can no longer finish later; then checks again.
    pub(super) fn end_old_commit_connection(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone().filter(|_| self.connected()) else {
            return;
        };
        let Some(old) = self.commit_unknown.as_ref().map(|u| u.old.clone()) else {
            return;
        };
        let task = self.services.runtime.spawn({
            let session = session.clone();
            async move {
                // Only that exact connection: its id plus how the server listed it.
                tokio::time::timeout(
                    CHECK_TIMEOUT,
                    session.end_old_backend(&old.commit_progress()),
                )
                .await?
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // Give the server a moment to roll the transaction back before looking.
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Ok(())) => this.verify_commit(cx),
                Ok(Err(e)) if is_connection_lost(&e) => this.failed_on(&session, e, cx),
                // Nothing was ended (already gone, or not provably Telecat's): look again.
                Ok(Err(e)) => {
                    this.show_hint(e.to_string(), cx);
                    this.verify_commit(cx);
                }
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
    /// The user has reviewed an outcome that stays unknown: ⌘S works again.
    pub(super) fn keep_commit_staged(&mut self, cx: &mut Context<Self>) {
        self.commit_unknown = None;
        self.sheet = None;
        self.state.logs.push(LogEntry {
            time: timestamp(),
            sql: "-- Interrupted save left unresolved by the user; changes stay staged.".into(),
        });
        self.show_hint(
            "Changes stay staged. Check the table before saving again, so nothing is saved twice.",
            cx,
        );
        cx.notify();
    }
    /// After a reconnect: how the import cut off by the drop ended.
    fn verify_import(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some((old, file)) = self.import_check.take() else {
            return;
        };
        let task = self.services.runtime.spawn(async move {
            tokio::time::timeout(
                CHECK_TIMEOUT,
                check_settled(&session, || old.import_progress(), &[]),
            )
            .await?
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // The import's result sheet, if still open, says what the check found.
                if let (
                    Some(Sheet::Import(import::ImportStage::Done {
                        result: Err(failure),
                        ..
                    })),
                    Ok(Ok(verdict)),
                ) = (this.sheet.as_mut(), &result)
                {
                    match verdict {
                        CommitVerdict::Applied => {
                            failure.outcome = telecat_db::Outcome::Committed
                        }
                        CommitVerdict::NotApplied => {
                            failure.outcome = telecat_db::Outcome::RolledBack;
                            failure.rolled_back = true;
                        }
                        CommitVerdict::Unknown { .. } => {}
                    }
                }
                let message = match result {
                    Ok(Ok(CommitVerdict::Applied)) => {
                        format!("The import of {file} was committed before the connection dropped.")
                    }
                    Ok(Ok(CommitVerdict::NotApplied)) => format!(
                        "The import of {file} was rolled back: the database is as it was before it."
                    ),
                    Ok(Ok(CommitVerdict::Unknown { reason, .. })) => format!(
                        "How the import of {file} ended is unknown: {reason} Check the database before importing again."
                    ),
                    Ok(Err(e)) => format!("The import of {file} couldn't be checked: {e}"),
                    Err(e) => format!("The import of {file} couldn't be checked: {e}"),
                };
                this.state.logs.push(LogEntry {
                    time: timestamp(),
                    sql: format!("-- {message}"),
                });
                this.show_hint(message, cx);
            });
        })
        .detach();
    }
    /// The toolbar's status pill: the connection, or that it dropped (red, click or ⌘⇧R to
    /// reconnect), or a reconnect in progress (indeterminate progress bar, Cancel).
    pub(super) fn status_pill(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let pill = div()
            .id("status-pill")
            .relative()
            .overflow_hidden()
            .flex_1()
            .min_w(px(120.))
            .h(px(36.))
            .px(px(16.))
            .rounded(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .text_color(rgb(0xffffff))
            .font(mono())
            .text_size(px(13.))
            .line_height(px(36.));
        let text = |s: String| div().flex_1().min_w_0().truncate().child(s);
        match &self.link.link {
            Link::Connected => pill
                .bg(color(&self.state.connection.color))
                .tooltip(text_tooltip(self.state.status_text()))
                .child(text(self.state.status_text())),
            Link::Disconnected { error, retry_at } => {
                let label = match retry_at {
                    Some(at) => format!(
                        "Disconnected — retrying in {}s · ⌘⇧R to reconnect now",
                        at.saturating_duration_since(Instant::now()).as_secs() + 1
                    ),
                    None => "Disconnected — ⌘⇧R to reconnect".into(),
                };
                pill.bg(rgb(0xa8322b))
                    .cursor_pointer()
                    .tooltip(text_tooltip(format!(
                        "{error}\n\nClick or press ⌘⇧R to reconnect. Staged changes are kept."
                    )))
                    .child(text(label))
                    .on_click(cx.listener(|this, _, window, cx| this.reconnect(window, cx)))
            }
            Link::Reconnecting { attempt, error } => pill
                .bg(color(&self.state.connection.color))
                .tooltip(text_tooltip(if error.is_empty() {
                    "Reconnecting with the same settings.".to_owned()
                } else {
                    format!("Reconnecting after: {error}")
                }))
                .child(text(if *attempt > 1 {
                    format!("Reconnecting… attempt {attempt}")
                } else {
                    "Reconnecting…".into()
                }))
                .child(
                    div()
                        .id("reconnect-cancel")
                        .flex_shrink_0()
                        .px(px(8.))
                        .h(px(22.))
                        .line_height(px(22.))
                        .rounded(px(6.))
                        .bg(rgba(0x00000040))
                        .hover(|d| d.bg(rgba(0x00000066)))
                        .font(ui_font())
                        .text_size(px(12.))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.cancel_reconnect(cx);
                        })),
                )
                .child(progress_bar()),
        }
    }
    pub(super) fn reconnect_password_sheet(&self, cx: &mut Context<Self>) -> Div {
        sheet(380., "Reconnect")
            .child(
                div()
                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                    .mb(px(10.))
                    .child(format!(
                        "Enter the password for {} to reconnect.",
                        self.state.connection.name
                    )),
            )
            .child(input_box(&self.gate_password, false, true))
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("reconnect-password-cancel", "Cancel", false, false)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_sheet(cx))),
                    )
                    .child(
                        button("reconnect-password-ok", "Reconnect", true, false).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.submit_reconnect_password(window, cx)
                            }),
                        ),
                    ),
            )
    }
    pub(super) fn commit_unknown_sheet(&self, cx: &mut Context<Self>) -> Div {
        let unknown = self.commit_unknown.as_ref();
        let checking = unknown.is_some_and(|u| u.checking);
        let (reason, open_backend) = match unknown.and_then(|u| u.verdict.as_ref()) {
            Some(CommitVerdict::Unknown {
                reason,
                open_backend,
            }) => (reason.clone(), open_backend.clone()),
            _ if self.connected() => ("It hasn't been checked yet.".into(), None),
            _ => (
                "Reconnect (⌘⇧R) to check whether it reached the database.".into(),
                None,
            ),
        };
        let file = unknown
            .and_then(|u| u.file.as_ref())
            .map(|f| format!("\n\nThe statements of that save are in {}.", f.display()))
            .unwrap_or_default();
        let mut buttons = div().mt(px(16.)).flex().justify_end().gap(px(8.));
        if open_backend.is_some() {
            buttons = buttons.child(
                button("commit-end-old", "End Old Connection", false, false)
                    .on_click(cx.listener(|this, _, _, cx| this.end_old_commit_connection(cx))),
            );
        }
        buttons = buttons
            .child(
                button("commit-keep", "Keep Changes Staged", false, false)
                    .on_click(cx.listener(|this, _, _, cx| this.keep_commit_staged(cx))),
            )
            .child(
                button(
                    "commit-check",
                    if checking {
                        "Checking…"
                    } else {
                        "Check Again"
                    },
                    true,
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.verify_commit(cx))),
            )
            .child(
                button("commit-unknown-close", "Close", false, false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.sheet = None;
                        cx.notify();
                    },
                )),
            );
        sheet(560., "Commit result unknown")
            .child(
                div()
                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                    .child(format!(
                        "The connection was lost while your changes were being saved, before the server confirmed them. They may or may not have been saved, so ⌘S is blocked and nothing is retried.\n\n{reason}{}{file}",
                        if open_backend.is_some() {
                            "\n\nEnding the old connection makes the server roll back whatever it hadn't committed; then Telecat checks again."
                        } else {
                            ""
                        }
                    )),
            )
            .child(buttons)
    }
}
/// Checks an interrupted save, giving the server a few seconds to notice that the old
/// connection is gone (it does once the network lets the close through) before reporting it
/// still open.
async fn check_settled(
    session: &Session,
    progress: impl Fn() -> telecat_db::CommitProgress,
    checks: &[telecat_core::changes::RowVerify],
) -> anyhow::Result<CommitVerdict> {
    let mut tries = 0;
    loop {
        let verdict = session
            .verify_interrupted_commit(&progress(), checks)
            .await?;
        tries += 1;
        if tries >= 10
            || !matches!(
                verdict,
                CommitVerdict::Unknown {
                    open_backend: Some(_),
                    ..
                }
            )
        {
            return Ok(verdict);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
#[cfg(test)]
mod tests;
/// An indeterminate progress bar along the bottom of the status pill.
fn progress_bar() -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(3.))
        .bg(rgba(0xffffff26))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(relative(0.3))
                .rounded(px(2.))
                .bg(rgba(0xffffffd9))
                .with_animation(
                    "reconnect-progress",
                    Animation::new(Duration::from_millis(1400)).repeat(),
                    |bar, delta| bar.left(relative(delta * 1.3 - 0.3)),
                ),
        )
}
