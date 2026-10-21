//! Import ▸ From SQL Dump…: pick a file, read it once for the confirmation, then import it on a
//! dedicated connection to the selected database. See `telecat_db::import`.
use super::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use telecat_core::dump::{BlockKind, DumpScan};
use telecat_db::{FileCheck, ImportFailure, ImportOptions, ImportProgress, ImportReport, Outcome};

/// What the import sheet shows.
pub(super) enum ImportStage {
    Scanning {
        file: String,
    },
    Confirm(Box<ImportRequest>),
    Running {
        file: String,
        size: u64,
        statements: u64,
        progress: Arc<ImportProgress>,
        cancel: Arc<AtomicBool>,
    },
    Done {
        file: String,
        result: Result<ImportReport, ImportFailure>,
        seconds: f64,
    },
}
/// A scanned dump, ready to import once confirmed.
#[derive(Clone)]
pub(super) struct ImportRequest {
    pub path: PathBuf,
    pub file: String,
    pub size: u64,
    pub scan: DumpScan,
    pub database: String,
    pub tables: usize,
    /// The kinds of blocked statements the user allowed.
    pub allow: std::collections::BTreeSet<BlockKind>,
    /// The file as it was read for the confirmation.
    pub check: FileCheck,
}
impl ImportRequest {
    /// PostgreSQL runs a dump in one transaction unless it holds statements that can't.
    pub fn transaction(&self, d: Driver) -> bool {
        d == Driver::Postgres && self.scan.no_transaction.is_empty()
    }
    /// Why it can't run as confirmed; `None` when it can.
    pub fn blocked_reason(&self, d: Driver) -> Option<String> {
        if let Some((line, what)) = self.scan.refused.first() {
            return Some(format!(
                "Line {line}: {what}. Import that part into its own database instead."
            ));
        }
        if self.transaction(d) {
            if let Some(line) = self.scan.transaction_control.first() {
                return Some(format!(
                    "Line {line}: the dump has its own transaction control (BEGIN, COMMIT, ROLLBACK, …), so it can't be imported as one transaction. Remove it from the dump."
                ));
            }
        }
        let missing: Vec<_> = self
            .scan
            .blocked_kinds
            .iter()
            .filter(|k| !self.allow.contains(k))
            .collect();
        (!missing.is_empty())
            .then(|| "Some statements are blocked: allow each kind below, or edit the dump.".into())
    }
}
pub(super) fn size_label(bytes: u64) -> String {
    telecat_db::format_bytes(bytes as f64)
}
/// The Items sidebar's menu for its empty area (it also works with no tables at all).
pub(super) fn items_menu_items() -> serde_json::Value {
    json!([
        {"id":"import","label":"Import","submenu":[{"id":"import-dump","label":"From SQL Dump…"}]},
        {"id":"new","label":"New","submenu":[
            {"id":"new-table","label":"Table"},
            {"id":"new-query","label":"SQL Query"}
        ]},
        {"type":"separator"},
        {"id":"refresh","label":"Refresh"}
    ])
}
impl Workspace {
    pub(super) fn items_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        native_menu::choose(
            items_menu_items(),
            position,
            window,
            cx,
            |this, id, _, cx| this.items_menu_action(&id, cx),
        );
    }
    pub(super) fn items_menu_action(&mut self, id: &str, cx: &mut Context<Self>) {
        match id {
            "import-dump" => self.import_dump_file(cx),
            "new-table" => self.new_table(cx),
            "new-query" => self.new_query("", cx),
            "refresh" => self.refresh_objects(cx),
            _ => {}
        }
    }
    /// Import ▸ From SQL Dump…
    pub(super) fn import_dump_file(&mut self, cx: &mut Context<Self>) {
        if self.saving || self.busy || self.session.is_none() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import SQL Dump".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                let _ = this.update(cx, |this, cx| this.scan_dump_file(path, cx));
            }
        })
        .detach();
    }
    /// Reads the dump once for the confirmation, off the UI thread.
    pub(super) fn scan_dump_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let driver = self.state.connection.driver;
        let database = self.state.database.clone();
        let file = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.sheet = Some(Sheet::Import(ImportStage::Scanning { file: file.clone() }));
        let tables = self
            .state
            .objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Table)
            .count();
        let (scan_path, target) = (path.clone(), database.clone());
        let Some(session) = self.session.clone() else {
            return;
        };
        let task = self.services.runtime.spawn(async move {
            let databases = session.list_databases().await?;
            tokio::task::spawn_blocking(move || {
                telecat_db::scan_dump_checked(&scan_path, driver, &target, databases)
            })
            .await?
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(
                    this.sheet,
                    Some(Sheet::Import(ImportStage::Scanning { .. }))
                ) {
                    return;
                }
                this.sheet = Some(match r {
                    Ok(Ok((scan, check))) => {
                        Sheet::Import(ImportStage::Confirm(Box::new(ImportRequest {
                            path,
                            file,
                            size: check.size,
                            scan,
                            database,
                            tables,
                            allow: Default::default(),
                            check,
                        })))
                    }
                    Ok(Err(e)) => Sheet::Error(format!("Can't import {file}: {e:#}")),
                    Err(e) => Sheet::Error(format!("Can't import {file}: {e}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn toggle_import_allow(&mut self, kind: BlockKind, cx: &mut Context<Self>) {
        if let Some(Sheet::Import(ImportStage::Confirm(r))) = &mut self.sheet {
            if !r.allow.remove(&kind) {
                r.allow.insert(kind);
            }
            cx.notify();
        }
    }
    /// The Import button: Safe and Alert modes confirm as for dangerous SQL, then it runs.
    pub(super) fn confirm_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Sheet::Import(ImportStage::Confirm(request))) = &self.sheet else {
            return;
        };
        if request
            .blocked_reason(self.state.connection.driver)
            .is_some()
        {
            return;
        }
        self.pending = Some(PendingAction::Import((**request).clone()));
        self.approval = QueryApproval::default();
        self.sheet = None;
        if !self.next_gate(cx) {
            self.execute_pending(window, cx);
        }
    }
    /// The gate for an import: (summary, gate, danger) as for a query.
    pub(super) fn import_gate(&self, request: &ImportRequest) -> (String, Gate, Option<String>) {
        let mode = self.state.connection.safe_mode;
        let gate = match mode {
            SafeMode::Silent => Gate::None,
            SafeMode::Alert1 | SafeMode::Alert2 => Gate::Alert,
            SafeMode::Safe1 | SafeMode::Safe2 => Gate::Password,
        };
        let summary = format!(
            "-- Import {} ({}) into {}: {} statements",
            request.file,
            size_label(request.size),
            request.database,
            request.scan.statements
        );
        let danger = (mode != SafeMode::Silent
            && !request.scan.risk_counts.is_empty()
            && !self.approval.danger)
            .then(|| {
                format!(
                    "The dump contains risky statements: {}",
                    request
                        .scan
                        .risk_counts
                        .iter()
                        .map(|(k, n)| format!("{k} ×{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        (summary, gate, danger)
    }
    pub(super) fn start_import(&mut self, request: ImportRequest, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let driver = self.state.connection.driver;
        let progress = Arc::new(ImportProgress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        self.saving = true;
        self.sheet = Some(Sheet::Import(ImportStage::Running {
            file: request.file.clone(),
            size: request.size,
            statements: request.scan.statements,
            progress: progress.clone(),
            cancel: cancel.clone(),
        }));
        let opts = ImportOptions {
            allow: request.allow.clone(),
            transaction: request.transaction(driver),
            expect: Some(request.check),
        };
        let started = std::time::Instant::now();
        let (path, target) = (request.path.clone(), request.database.clone());
        let task = self.services.runtime.spawn({
            let (session, progress) = (session.clone(), progress.clone());
            async move {
                session
                    .import_dump(&path, &target, opts, &progress, &cancel)
                    .await
            }
        });
        self.import_in_flight = Some(super::link::InFlightImport {
            session,
            abort: task.abort_handle(),
            file: request.file.clone(),
            progress: progress.clone(),
        });
        // Repaint the progress while it runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.sheet, Some(Sheet::Import(ImportStage::Running { .. })))
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        let file = request.file.clone();
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r,
                Err(e) => Err(ImportFailure {
                    line: 0,
                    column: None,
                    excerpt: String::new(),
                    message: e.to_string(),
                    executed: 0,
                    rolled_back: false,
                    cancelled: false,
                    outcome: Outcome::Unknown,
                }),
            };
            let _ = this.update(cx, |this, cx| {
                // Cut off by a lost connection: already reported, and checked on reconnect.
                if this.import_in_flight.take().is_none() {
                    return;
                }
                this.saving = false;
                let sql = match &result {
                    Ok(r) => format!(
                        "-- Imported {file}: {} statements, {} rows copied",
                        r.statements, r.copied_rows
                    ),
                    Err(f) => format!(
                        "-- Import of {file} stopped: {}",
                        f.to_string().lines().next().unwrap_or("")
                    ),
                };
                let entry = LogEntry {
                    time: timestamp(),
                    sql,
                };
                this.state.logs.push(entry.clone());
                if let Err(e) = this
                    .services
                    .profile
                    .add_history(&this.state.connection.id, entry)
                {
                    eprintln!("History: {e}");
                }
                this.history = this.services.profile.history(&this.state.connection.id);
                this.sheet = Some(Sheet::Import(ImportStage::Done {
                    file,
                    result,
                    seconds: started.elapsed().as_secs_f64(),
                }));
                this.refresh_objects(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn cancel_import(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Import(ImportStage::Running { cancel, .. })) = &self.sheet else {
            return;
        };
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(session) = self.session.clone() {
            let task = self
                .services
                .runtime
                .spawn(async move { session.cancel_import().await });
            cx.spawn(async move |_, _| {
                let _ = task.await;
            })
            .detach();
        }
    }
    pub(super) fn import_sheet(&self, stage: &ImportStage, cx: &mut Context<Self>) -> Div {
        let d = self.state.connection.driver;
        let row = |label: &str, value: String| {
            div()
                .flex()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(110.))
                        .flex_shrink_0()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(label.to_owned()),
                )
                .child(div().min_w_0().truncate().child(value))
        };
        let note = |text: String, color: u32| div().mt(px(8.)).text_color(rgb(color)).child(text);
        let close = || {
            small_button("import-close", "Close").on_click(cx.listener(|this, _, _, cx| {
                this.sheet = None;
                cx.notify();
            }))
        };
        match stage {
            ImportStage::Scanning { file } => sheet(520., "Import SQL Dump")
                .child(
                    div()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(format!("Reading {file}…")),
                )
                .child(div().mt(px(14.)).flex().justify_end().child(close())),
            ImportStage::Confirm(r) => {
                let blocked = r.blocked_reason(d);
                let mut body = sheet(620., "Import SQL Dump")
                    .text_size(px(13.))
                    .child(row("Connection", self.state.connection.name.clone()))
                    .child(row(
                        "Into",
                        if d == Driver::Postgres {
                            format!("database {} (schema search_path as set by the dump, default public)", r.database)
                        } else {
                            format!("database {}", r.database)
                        },
                    ))
                    .child(row("File", format!("{} · {}", r.file, size_label(r.size))))
                    .child(row(
                        "Statements",
                        format!(
                            "{}{}",
                            r.scan.statements,
                            if r.scan.copy_blocks > 0 {
                                format!(" ({} COPY data blocks)", r.scan.copy_blocks)
                            } else {
                                String::new()
                            }
                        ),
                    ));
                if r.tables > 0 {
                    body = body.child(note(
                        format!(
                            "{} already has {} table{}: the dump may drop or change them.",
                            r.database,
                            r.tables,
                            if r.tables == 1 { "" } else { "s" }
                        ),
                        0xf0d27a,
                    ));
                }
                if !r.scan.risk_counts.is_empty() {
                    body = body
                        .child(note("Risky statements in the dump:".into(), 0xf0d27a))
                        .child(
                            div()
                                .id("import-risks")
                                .max_h(px(140.))
                                .overflow_y_scroll()
                                .mt(px(4.))
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .bg(rgb(telecat_theme::INPUT))
                                .font(mono())
                                .text_size(px(12.))
                                .children(
                                    r.scan
                                        .risk_counts
                                        .iter()
                                        .map(|(k, n)| div().child(format!("{k} ×{n}"))),
                                )
                                .children(r.scan.risks.iter().take(30).map(|(line, risk)| {
                                    div()
                                        .text_color(rgb(telecat_theme::MUTED))
                                        .child(format!("line {line}: {}", risk.label()))
                                })),
                        );
                }
                if !r.scan.blocked.is_empty() || !r.scan.refused.is_empty() {
                    body = body
                        .child(note("Blocked statements:".into(), 0xff8a80))
                        .child(
                            div()
                                .id("import-blocked")
                                .max_h(px(120.))
                                .overflow_y_scroll()
                                .mt(px(4.))
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .bg(rgb(telecat_theme::INPUT))
                                .font(mono())
                                .text_size(px(12.))
                                .text_color(rgb(0xff8a80))
                                .children(
                                    r.scan
                                        .refused
                                        .iter()
                                        .chain(r.scan.blocked.iter())
                                        .take(40)
                                        .map(|(line, what)| {
                                            div().child(format!("line {line}: {what}"))
                                        }),
                                ),
                        );
                    for kind in r.scan.blocked_kinds.iter().copied() {
                        let on = r.allow.contains(&kind);
                        body = body.child(
                            div()
                                .id(SharedString::from(format!("import-allow-{kind:?}")))
                                .mt(px(6.))
                                .flex()
                                .gap(px(6.))
                                .items_center()
                                .cursor_pointer()
                                .text_color(rgb(0xff8a80))
                                .child(
                                    div()
                                        .size(px(13.))
                                        .rounded(px(2.))
                                        .border_1()
                                        .border_color(rgb(0x888888))
                                        .bg(rgb(if on { 0x2f6fed } else { 0x383838 }))
                                        .when(on, |d| d.child("✓")),
                                )
                                .child(kind.allow_label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_import_allow(kind, cx)
                                })),
                        );
                    }
                }
                body = body.child(note(
                    "Telecat checks the dump as well as it can, but the import runs its SQL with this connection's privileges. Import only dumps you trust.".into(),
                    telecat_theme::MUTED,
                ));
                body = body.child(note(
                    if r.transaction(d) {
                        "Runs in one transaction: an error or Cancel leaves the database as it was.".into()
                    } else if d == Driver::Postgres {
                        format!(
                            "Can't run in one transaction (line {}: {}). An error leaves what ran before it.",
                            r.scan.no_transaction.first().map_or(0, |n| n.0),
                            r.scan.no_transaction.first().map_or(String::new(), |n| n.1.clone())
                        )
                    } else {
                        "MySQL saves each table change as it runs: an error or Cancel leaves what ran before it.".into()
                    },
                    telecat_theme::MUTED,
                ));
                if let Some(reason) = &blocked {
                    body = body.child(note(reason.clone(), 0xff8a80));
                }
                body.child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            button("import-cancel", "Cancel", false, false)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_sheet(cx))),
                        )
                        .child(
                            button("import-run", "Import", true, blocked.is_some()).on_click(
                                cx.listener(|this, _, window, cx| this.confirm_import(window, cx)),
                            ),
                        ),
                )
            }
            ImportStage::Running {
                file,
                size,
                statements,
                progress,
                ..
            } => {
                let done = progress
                    .statements
                    .load(std::sync::atomic::Ordering::Relaxed);
                let bytes = progress.bytes.load(std::sync::atomic::Ordering::Relaxed);
                let fraction = if *size > 0 {
                    (bytes as f32 / *size as f32).min(1.)
                } else {
                    0.
                };
                sheet(520., format!("Importing {file}"))
                    .text_size(px(13.))
                    .child(div().child(format!(
                        "{done} of {statements} statements · {} of {}",
                        size_label(bytes),
                        size_label(*size)
                    )))
                    .child(
                        div()
                            .mt(px(10.))
                            .h(px(6.))
                            .w_full()
                            .rounded(px(3.))
                            .bg(rgb(telecat_theme::INPUT))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded(px(3.))
                                    .bg(rgb(telecat_theme::ACCENT)),
                            ),
                    )
                    .child(
                        div().mt(px(14.)).flex().justify_end().child(
                            button("import-stop", "Cancel", false, false)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_import(cx))),
                        ),
                    )
            }
            ImportStage::Done {
                file,
                result,
                seconds,
            } => {
                let (title, text, color) = match result {
                    Ok(r) => (
                        format!("Imported {file}"),
                        format!(
                            "{} statements{} in {:.1} s{}.",
                            r.statements,
                            if r.copied_rows > 0 {
                                format!(", {} rows copied", r.copied_rows)
                            } else {
                                String::new()
                            },
                            seconds,
                            if r.transaction {
                                " (one transaction)"
                            } else {
                                ""
                            }
                        ),
                        0x9fd59f,
                    ),
                    Err(f) => (format!("Import of {file} stopped"), f.to_string(), 0xff8a80),
                };
                sheet(620., title)
                    .child(
                        div()
                            .id("import-result")
                            .max_h(px(260.))
                            .overflow_y_scroll()
                            .font(mono())
                            .text_size(px(12.5))
                            .text_color(rgb(color))
                            .child(text),
                    )
                    .child(div().mt(px(16.)).flex().justify_end().child(close()))
            }
        }
    }
}
