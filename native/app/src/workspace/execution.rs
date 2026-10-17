use super::*;
use telecat_core::changes::inline_params;

pub(super) enum PendingAction {
    Query { id: u64, plan: QueryPlan },
    Commit(Vec<CommitUnit>),
    Unlock(SafeMode),
}
impl Workspace {
    pub(super) fn request_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.busy {
            return;
        }
        self.flush_grid_edits(window, cx);
        let units = match self.state.collect_changes() {
            Ok(u) => u,
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        if units.is_empty() {
            return;
        }
        self.saving = true;
        self.pending = Some(PendingAction::Commit(units));
        self.approval = QueryApproval::default();
        if !self.next_gate(cx) {
            self.execute_pending(window, cx);
        }
    }
    pub(super) fn next_gate(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let (sql, gate, danger) = match pending {
            PendingAction::Query { plan, .. } => (
                plan.original.join(";\n\n"),
                plan.gate,
                if self.approval.danger {
                    None
                } else {
                    plan.dangers.first().map(|(_, reason)| reason.clone())
                },
            ),
            PendingAction::Commit(units) => {
                let sql = units
                    .iter()
                    .flat_map(|g| {
                        g.statements
                            .iter()
                            .map(|s| inline_params(self.state.connection.driver, s))
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let document = EditorDocument {
                    text: sql.clone(),
                    ..Default::default()
                };
                (
                    sql,
                    QueryPlan::new(
                        &document,
                        true,
                        self.state.connection.driver,
                        self.state.connection.safe_mode,
                        0,
                    )
                    .gate,
                    None,
                )
            }
            PendingAction::Unlock(_) => (String::new(), Gate::Password, None),
        };
        if danger.is_some() || (gate != Gate::None && !self.approval.safe_mode) {
            self.gate_focused = false;
            self.sheet = Some(Sheet::Gate {
                sql,
                password: danger.is_none() && gate == Gate::Password,
                danger,
                attempts: 0,
            });
            cx.notify();
            true
        } else {
            false
        }
    }
    pub(super) fn approve_gate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Sheet::Gate {
            password,
            danger,
            attempts,
            ..
        }) = &mut self.sheet
        else {
            return;
        };
        if *password {
            let pw = self.gate_password.read(cx).value();
            if self
                .session
                .as_ref()
                .is_none_or(|s| !s.verify_password(&pw))
            {
                *attempts += 1;
                cx.notify();
                return;
            }
        }
        if danger.is_some() {
            self.approval.danger = true;
        } else {
            self.approval.safe_mode = true;
        }
        self.gate_password
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.sheet = None;
        if !self.next_gate(cx) {
            self.execute_pending(window, cx);
        }
    }
    pub(super) fn cancel_sheet(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.sheet = None;
        self.saving = false;
        self.approval = QueryApproval::default();
        cx.notify();
    }
    fn execute_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.sheet = None;
        match pending {
            PendingAction::Query { id, plan } => {
                self.saving = false;
                self.execute_query(id, plan, self.approval, cx);
            }
            PendingAction::Commit(units) => self.execute_commit(units, window, cx),
            PendingAction::Unlock(mode) => {
                self.saving = false;
                self.set_safe_mode(mode, cx);
            }
        }
    }
    pub(super) fn change_safe_mode(&mut self, mode: SafeMode, cx: &mut Context<Self>) {
        if self.saving || mode == self.state.connection.safe_mode {
            return;
        }
        if matches!(
            self.state.connection.safe_mode,
            SafeMode::Safe1 | SafeMode::Safe2
        ) {
            self.pending = Some(PendingAction::Unlock(mode));
            self.approval = QueryApproval::default();
            self.next_gate(cx);
        } else {
            self.set_safe_mode(mode, cx);
        }
    }
    fn set_safe_mode(&mut self, mode: SafeMode, cx: &mut Context<Self>) {
        self.state.connection.safe_mode = mode;
        if !self.services.scenario
            && let Err(e) = self
                .services
                .profile
                .set_safe_mode(&self.state.connection.id, mode)
        {
            self.error(e, cx);
        }
        self.services.refresh(cx);
        cx.notify();
    }
    fn execute_commit(
        &mut self,
        units: Vec<CommitUnit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.saving = false;
            cx.notify();
            return;
        };
        self.saving = true;
        cx.notify();
        let runtime = self.services.runtime.clone();
        cx.spawn_in(window, async move |this, cx| {
            for unit in units {
                let s = session.clone();
                let stmts = unit.statements.clone();
                let review = runtime
                    .spawn(async move { s.review_commit(&stmts).await })
                    .await;
                let review = match review {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        let _ = this.update(cx, |this, cx| this.error(e, cx));
                        break;
                    }
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| this.error(e, cx));
                        break;
                    }
                };
                let consent = if review.non_transactional.is_empty() {
                    false
                } else {
                    let Ok(prompt) =
                        cx.update(|window, cx| telecat_app::commit::prompt(&review, window, cx))
                    else {
                        break;
                    };
                    if !prompt.await {
                        break;
                    }
                    true
                };
                let s = session.clone();
                let statements = unit.statements.clone();
                let transaction = unit.transaction;
                let _ = this.update(cx, |this, cx| {
                    for st in &unit.statements {
                        this.state.logs.push(LogEntry {
                            time: timestamp(),
                            sql: inline_params(this.state.connection.driver, st),
                        });
                    }
                    cx.notify();
                });
                let result = runtime
                    .spawn(async move {
                        telecat_app::commit::execute_reviewed_part(
                            &s,
                            &statements,
                            transaction,
                            move |_| async move { consent },
                        )
                        .await
                    })
                    .await;
                match result {
                    Ok(Ok(_)) => {
                        let _ = this.update(cx, |this, cx| {
                            this.state.committed(&unit.target);
                            cx.notify();
                        });
                    }
                    Ok(Err(e)) => {
                        let _ = this.update(cx, |this, cx| {
                            this.error(
                                format!(
                                    "{e}\n\n{}",
                                    unit.statements
                                        .iter()
                                        .map(|s| inline_params(this.state.connection.driver, s))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                ),
                                cx,
                            )
                        });
                        break;
                    }
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| this.error(e, cx));
                        break;
                    }
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                this.refresh_objects(cx);
                this.refresh_tab(cx);
                this.load_metadata(cx);
                this.table_inputs.clear();
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn preview_text(&self) -> anyhow::Result<String> {
        if self.scenario == "a1-preview" {
            return Ok(fixtures::PREVIEW.into());
        }
        Ok(self
            .state
            .collect_changes()?
            .iter()
            .flat_map(|g| {
                g.statements
                    .iter()
                    .map(|s| inline_params(self.state.connection.driver, s))
            })
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
    pub(super) fn stage_operation(
        &mut self,
        object: DbObject,
        truncate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let name = display_name(&object.table, self.state.connection.driver);
        let kind = if object.kind == ObjectKind::View {
            "view"
        } else {
            "table"
        };
        let prompt = window.prompt(
            PromptLevel::Warning,
            &format!(
                "Do you want to {} {kind} {name}?",
                if truncate { "truncate" } else { "delete" }
            ),
            Some(if truncate {
                "All rows will be removed when you press ⌘S to save changes."
            } else {
                "The table and all its data will be removed when you press ⌘S to save changes."
            }),
            &[if truncate { "Truncate" } else { "Delete" }, "Cancel"],
            cx,
        );
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let ok = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if ok {
                    this.state
                        .operations
                        .retain(|o| o.object.table != object.table);
                    this.state
                        .operations
                        .push(StagedOperation { object, truncate });
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn discard_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let prompt=window.prompt(PromptLevel::Warning,"Discard all changes?",Some("Every staged edit, new table, delete and truncate in this connection will be thrown away."),&["Discard","Cancel"],cx);
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let ok = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if ok {
                    let new: Vec<_> = this
                        .state
                        .tabs
                        .iter()
                        .filter(|t| t.table.as_ref().is_some_and(|t| t.is_new))
                        .map(|t| t.id)
                        .collect();
                    for id in new {
                        if this.state.tabs.len() == 1 {
                            this.state.open_query("SQL Query", "");
                        }
                        this.state.close_tab(id);
                    }
                    for t in &mut this.state.tabs {
                        if let Some(e) = &mut t.table {
                            e.discard();
                        }
                    }
                    this.state.operations.clear();
                    this.sheet = None;
                    this.table_inputs.clear();
                    this.refresh_objects(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
impl Workspace {
    pub(super) fn table_status(&self, table: &TableRef) -> Option<&'static str> {
        if self.scenario == "55-new-table" && table.name == "sa_banks" {
            return None;
        }

        if let Some(op) = self
            .state
            .operations
            .iter()
            .find(|o| o.object.table == *table)
        {
            return Some(if op.truncate { "truncate" } else { "drop" });
        }
        if let Some(e) = self
            .state
            .tabs
            .iter()
            .find(|t| t.kind == TabKind::Table(table.clone()))
            .and_then(|t| t.table.as_ref())
        {
            if e.is_new {
                return Some("new");
            }
            if e.dirty() {
                return Some("modified");
            }
        }
        if self.scenario == "57-running" {
            if table.name == "sa_banks" {
                return Some("modified");
            }
            if table.name == "untitled_table_337" {
                return Some("new");
            }
        }
        None
    }
    pub(super) fn object_menu(
        &mut self,
        object: DbObject,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let relational = matches!(object.kind, ObjectKind::Table | ObjectKind::View);
        let status = self.table_status(&object.table);
        let mut items = vec![];
        if relational {
            items.extend([
                json!({"id":"open","label":"Open in New Tab"}),
                json!({"id":"select","label":"SELECT * … LIMIT 100 in SQL Query"}),
                json!({"type":"separator"}),
            ]);
        }
        items.push(json!({"id":"copy","label":"Copy Name"}));
        if relational && status != Some("new") {
            items.push(json!({"type":"separator"}));
            if object.kind == ObjectKind::Table {
                items.push(json!({"id":"truncate","label":"Truncate…  ⌥⌫"}));
            }
            items.push(json!({"id":"drop","label":"Delete…  ⌫"}));
            if matches!(status, Some("drop" | "truncate")) {
                items.push(json!({"id":"cancel","label":"Cancel Pending Delete/Truncate"}));
            }
        }
        native_menu::choose(
            json!(items),
            position,
            window,
            cx,
            move |this, id, window, cx| {
                match id.as_str() {
                    "open" => this.open_table(object.table.clone(), cx),
                    "select" => {
                        let q = telecat_core::changes::qualified(
                            this.state.connection.driver,
                            &object.table,
                        );
                        this.new_query(&format!("SELECT * FROM {q} LIMIT 100;"), cx);
                    }
                    "copy" => {
                        cx.write_to_clipboard(ClipboardItem::new_string(object.table.name.clone()))
                    }
                    "truncate" | "drop" => {
                        this.stage_operation(object.clone(), id == "truncate", window, cx)
                    }
                    "cancel" => this
                        .state
                        .operations
                        .retain(|o| o.object.table != object.table),
                    _ => {}
                }
                cx.notify();
            },
        );
    }
}
impl Workspace {
    pub(super) fn request_close_tab(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        if self.state.tabs.len() == 1 {
            self.request_close_window(window, cx);
            return;
        }
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == id) else {
            return;
        };
        let query = tab.kind == TabKind::Query && !tab.editor.text.trim().is_empty();
        let dirty = tab.table.as_ref().is_some_and(|t| t.dirty());
        if !query && !dirty {
            self.state.close_tab(id);
            self.table_inputs.remove(&id);
            self.persist_drafts();
            cx.notify();
            return;
        }
        let prompt=window.prompt(PromptLevel::Warning,&if query{format!("Close {}?",tab.title)}else{format!("Discard unsaved changes to {}?",tab.title)},Some(if query{"The SQL in this tab will be deleted. (Telecat otherwise keeps it, even after quitting.)"}else{"Closing the tab throws away its staged edits."}),&[if query{"Close Tab"}else{"Discard"},"Cancel"],cx);
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let ok = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if ok {
                    this.state.close_tab(id);
                    this.table_inputs.remove(&id);
                    this.persist_drafts();
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_grid_edits(window, cx);
        if let Some(host) = self.host.clone() {
            let id = cx.entity_id();
            cx.spawn_in(window, async move |_, cx| {
                let _ = cx.update(|window, cx| {
                    let _ = host.update(cx, |host, cx| host.close_connection(id, window, cx));
                });
            })
            .detach();
            return;
        }
        if self.saving {
            return;
        }
        if !self.state.has_changes() {
            window.remove_window();
            return;
        }
        let prompt = window.prompt(
            PromptLevel::Warning,
            "Discard unsaved changes?",
            Some("This connection has staged changes. Close it and discard those changes?"),
            &["Discard", "Cancel"],
            cx,
        );
        self.saving = true;
        cx.spawn_in(window, async move |this, cx| {
            let ok = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                cx.notify();
            });
            if ok {
                let _ = cx.update(|window, _| window.remove_window());
            }
        })
        .detach();
    }
}

impl Workspace {
    pub(crate) fn close_state(&self) -> (bool, bool) {
        (self.saving, self.state.has_changes())
    }
}
