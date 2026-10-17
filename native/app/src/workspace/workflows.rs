use super::*;
impl Workspace {
    pub(super) fn save_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_grid_edits(window, cx);
        if self.state.has_changes() {
            self.request_commit(window, cx);
            return;
        }
        if self.state.current().kind != TabKind::Query {
            return;
        }
        self.sync_editor_document(cx);
        let tab = self.state.current();
        let sql = tab.editor.text.clone();
        if sql.trim().is_empty() {
            return;
        }
        let title = if tab.title == "SQL Query" {
            ""
        } else {
            &tab.title
        };
        self.sheet_input.update(cx, |s, cx| {
            s.set_placeholder("Query name", window, cx);
            s.set_value(title, window, cx);
            s.focus(window, cx);
        });
        self.sheet = Some(Sheet::SaveQuery { sql, tab: tab.id });
        cx.notify();
    }
    pub(super) fn save_named_query(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::SaveQuery { sql, tab }) = &self.sheet else {
            return;
        };
        let tab = *tab;
        let name = self.sheet_input.read(cx).value().trim().to_owned();
        let name = if name.is_empty() {
            "Untitled query".into()
        } else {
            name
        };
        match self.services.profile.save_query(
            &self.state.connection.id,
            SavedQuery {
                id: String::new(),
                name: name.clone(),
                sql: sql.clone(),
            },
        ) {
            Ok(queries) => {
                self.queries = queries;
                if let Some(t) = self.state.tabs.iter_mut().find(|t| t.id == tab) {
                    t.title = name;
                }
                self.sheet = None;
                self.persist_drafts();
            }
            Err(e) => self.error(e, cx),
        }
        cx.notify();
    }
    pub(super) fn open_named_query(&mut self, sql: &str, title: &str, cx: &mut Context<Self>) {
        self.state.open_query(title, sql);
        self.persist_drafts();
        cx.notify();
    }
    pub(super) fn open_history(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.current().kind != TabKind::Query {
            self.new_query(sql, cx);
            return;
        }
        self.sync_editor_document(cx);
        let old = self.state.current().editor.text.trim_end();
        let text = if old.is_empty() {
            sql.to_owned()
        } else {
            format!("{old}\n\n{sql}")
        };
        self.replace_editor_text(text, window, cx);
    }
    pub(super) fn query_menu(
        &mut self,
        q: SavedQuery,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        native_menu::choose(
            json!([{"id":"open","label":"Open"},{"id":"delete","label":"Delete"}]),
            position,
            window,
            cx,
            move |this, id, _, cx| {
                if id == "open" {
                    this.open_named_query(&q.sql, &q.name, cx);
                } else if id == "delete" {
                    match this
                        .services
                        .profile
                        .delete_query(&this.state.connection.id, &q.id)
                    {
                        Ok(()) => {
                            this.queries = this.services.profile.queries(&this.state.connection.id)
                        }
                        Err(e) => this.error(e, cx),
                    }
                }
                cx.notify();
            },
        );
    }
    pub(super) fn open_sql_file(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQL File".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let title = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(path) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(sql) => this.open_named_query(&sql, &title, cx),
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
    pub(super) fn save_sql_file(&mut self, cx: &mut Context<Self>) {
        self.sync_editor_document(cx);
        let tab = self.state.current();
        if tab.kind != TabKind::Query {
            return;
        }
        let sql = tab
            .editor
            .selection
            .clone()
            .unwrap_or_else(|| tab.editor.text.clone());
        if sql.trim().is_empty() {
            return;
        }
        let name = if tab.title == "SQL Query" {
            "query.sql".into()
        } else {
            format!("{}.sql", tab.title.trim_end_matches(".sql"))
        };
        let prompt = cx.prompt_for_new_path(&self.services.profile.root, Some(&name));
        let id = tab.id;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let title = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(path, sql) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        if let Some(t) = this.state.tabs.iter_mut().find(|t| t.id == id) {
                            t.title = title;
                        }
                        this.persist_drafts();
                    }
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn open_database_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_selection = 0;
        self.sheet_input.update(cx, |s, cx| {
            s.set_placeholder("Search for database...", window, cx);
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.sheet = Some(Sheet::SwitchDatabase);
        self.reset_picker_selection(cx);
        self.load_databases(cx);
        cx.notify();
    }
    pub(super) fn switch_database(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving || self.busy || database == self.state.database {
            self.sheet = None;
            cx.notify();
            return;
        }
        let Some(old) = self.session.clone() else {
            return;
        };
        self.flush_grid_edits(window, cx);
        let confirm = if self.state.has_changes() {
            Some(window.prompt(
                PromptLevel::Warning,
                "Discard unsaved changes?",
                Some(&format!(
                    "Switching to {database} reloads the workspace and throws away staged edits."
                )),
                &["Discard & Switch", "Cancel"],
                cx,
            ))
        } else {
            None
        };
        self.saving = true;
        let rt = self.services.runtime.clone();
        cx.spawn(async move |this, cx| {
            if let Some(confirm) = confirm {
                if confirm.await != Ok(0) {
                    let _ = this.update(cx, |this, cx| {
                        this.saving = false;
                        cx.notify();
                    });
                    return;
                }
            }
            let task = rt.spawn(async move { old.with_database(&database).await });
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(Ok(session)) => {
                        if let Some(old) = this.session.replace(Arc::new(session)) {
                            this.services.runtime.spawn(async move {
                                old.close().await;
                            });
                        }
                        let s = this.session.as_ref().unwrap();
                        this.state.database = s.database.clone();
                        this.state.connection.database = s.database.clone();
                        this.state.server_version = s.server_version.clone();
                        this.state.tls = s.tls.clone();
                        this.state.user = s.user.clone();
                        this.state.tabs.retain(|t| t.kind == TabKind::Query);
                        if this.state.tabs.is_empty() {
                            this.state.open_query("SQL Query", "");
                        }
                        let active = this.state.active;
                        if !this.state.tabs.iter().any(|t| t.id == active) {
                            this.state.active = this.state.tabs[0].id;
                        }
                        this.state.operations.clear();
                        this.state.objects.clear();
                        this.update_completion_schema(cx);
                        this.table_inputs.clear();
                        this.tree_state.columns.clear();
                        this.clear_table_components();
                        this.running.database = Some(this.state.database.clone());
                        this.running.selected.clear();
                        this.running.loading = false;
                        this.sheet = None;
                        this.refresh_objects(cx);
                        this.load_databases(cx);
                    }
                    Ok(Err(e)) => this.error(e, cx),
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn open_anything(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_selection = 0;
        self.sheet_input.update(cx, |s, cx| {
            s.set_placeholder("Table, view, function or saved query...", window, cx);
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.sheet = Some(Sheet::QuickOpen);
        cx.notify();
    }
}
#[derive(Clone)]
pub(super) enum Pick {
    Database(String),
    Object(DbObject),
    Query(SavedQuery),
}
impl Workspace {
    pub(super) fn picker_items(&self, cx: &App) -> Vec<(String, Pick)> {
        let q = self.sheet_input.read(cx).value().trim().to_lowercase();
        let mut out = vec![];
        if matches!(self.sheet, Some(Sheet::SwitchDatabase)) {
            out.extend(
                self.databases
                    .iter()
                    .map(|d| (d.clone(), Pick::Database(d.clone()))),
            );
        } else {
            out.extend(self.state.objects.iter().map(|o| {
                (
                    display_name(&o.table, self.state.connection.driver),
                    Pick::Object(o.clone()),
                )
            }));
            for t in &self.state.tabs {
                if let Some(t) = t.table.as_ref().filter(|t| t.is_new) {
                    if !out
                        .iter()
                        .any(|(_, p)| matches!(p, Pick::Object(o) if o.table==t.table))
                    {
                        out.push((
                            display_name(&t.table, self.state.connection.driver),
                            Pick::Object(DbObject {
                                table: t.table.clone(),
                                kind: ObjectKind::Table,
                            }),
                        ));
                    }
                }
            }
            out.extend(
                self.queries
                    .iter()
                    .map(|s| (s.name.clone(), Pick::Query(s.clone()))),
            );
        }
        let mut scored: Vec<_> = out
            .into_iter()
            .filter_map(|item| fuzzy_score(&q, &item.0).map(|score| (score, item)))
            .collect();
        scored.sort_by_key(|(score, _)| *score);
        scored.into_iter().take(200).map(|(_, item)| item).collect()
    }
    pub(super) fn reset_picker_selection(&mut self, cx: &App) {
        self.picker_selection = if self.sheet_input.read(cx).value().is_empty() {
            self.picker_items(cx)
                .iter()
                .position(|(_, pick)| matches!(pick,Pick::Database(db) if db==&self.state.database))
                .unwrap_or(0)
        } else {
            0
        };
        self.picker_scroll.scroll_to_item(self.picker_selection);
    }
    pub(super) fn choose_picker(
        &mut self,
        pick: Pick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sheet = None;
        match pick {
            Pick::Database(db) => self.switch_database(db, window, cx),
            Pick::Object(o) => {
                if matches!(o.kind, ObjectKind::Table | ObjectKind::View) {
                    self.open_table(o.table, cx);
                } else {
                    let sql = if self.state.connection.driver == Driver::Mysql {
                        format!(
                            "SHOW CREATE {} `{}`;",
                            if o.kind == ObjectKind::Procedure {
                                "PROCEDURE"
                            } else {
                                "FUNCTION"
                            },
                            o.table.name.replace('`', "``")
                        )
                    } else {
                        format!(
                            "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = '{}' AND p.proname = '{}';",
                            o.table.schema.replace('\'', "''"),
                            o.table.name.replace('\'', "''")
                        )
                    };
                    self.open_history(&sql, window, cx);
                }
            }
            Pick::Query(q) => self.open_named_query(&q.sql, &q.name, cx),
        }
        cx.notify();
    }
    pub(super) fn picker_view(&self, database: bool, cx: &mut Context<Self>) -> Div {
        let items = self.picker_items(cx);
        sheet(
            520.,
            if database {
                "Switch Database"
            } else {
                "Open Anything"
            },
        )
        .child(input_box(&self.sheet_input, false, true))
        .child(
            div()
                .relative()
                .mt(px(10.))
                .child(
                    div()
                        .id("picker-items")
                        .max_h(px(360.))
                        .track_scroll(&self.picker_scroll)
                        .overflow_y_scroll()
                        .when(database && self.databases_loading, |d| {
                            d.child(
                                div()
                                    .p(px(16.))
                                    .text_center()
                                    .text_color(rgb(telecat_theme::MUTED))
                                    .child("Loading…"),
                            )
                        })
                        .when(items.is_empty() && !self.databases_loading, |d| {
                            d.child(
                                div()
                                    .p(px(16.))
                                    .text_center()
                                    .text_color(rgb(telecat_theme::MUTED))
                                    .child("No matches"),
                            )
                        })
                        .children(items.into_iter().enumerate().map(|(i, (label, pick))| {
                            let selected = i == self.picker_selection;
                            let (path, detail, current) = match &pick {
                                Pick::Database(db) => {
                                    ("Database.svg", None, db == &self.state.database)
                                }
                                Pick::Query(_) => ("", Some("saved query"), false),
                                Pick::Object(o) => match o.kind {
                                    ObjectKind::Table => ("Table.svg", Some("table"), false),
                                    ObjectKind::View => ("View.svg", Some("view"), false),
                                    ObjectKind::Function => {
                                        ("Function.svg", Some("function"), false)
                                    }
                                    ObjectKind::Procedure => {
                                        ("Function.svg", Some("procedure"), false)
                                    }
                                },
                            };
                            div()
                                .id(("picker-item", i))
                                .px(px(10.))
                                .py(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(6.))
                                .font(mono())
                                .text_size(px(13.))
                                .line_height(px(16.))
                                .when(selected, |d| {
                                    d.bg(rgb(telecat_theme::ACCENT)).text_color(rgb(0xffffff))
                                })
                                .child(if matches!(pick, Pick::Query(_)) {
                                    div()
                                        .font(mono())
                                        .text_size(px(9.8))
                                        .font_weight(FontWeight::BOLD)
                                        .child("SQL")
                                        .into_any_element()
                                } else {
                                    icon(
                                        path,
                                        14.,
                                        if selected {
                                            0xffffff
                                        } else {
                                            telecat_theme::TEXT
                                        },
                                    )
                                    .into_any_element()
                                })
                                .child(div().flex_1().min_w_0().truncate().child(label))
                                .when(current, |d| {
                                    d.child(
                                        div()
                                            .font(ui_font())
                                            .text_size(px(11.))
                                            .line_height(px(13.))
                                            .px(px(6.))
                                            .py(px(1.))
                                            .rounded(px(4.))
                                            .bg(rgba(0x3fb95033))
                                            .text_color(rgb(telecat_theme::OK))
                                            .child("current"),
                                    )
                                })
                                .when_some(detail, |d, detail| {
                                    d.child(
                                        div()
                                            .font(ui_font())
                                            .text_size(px(12.))
                                            .text_color(rgb(if selected {
                                                0xdbe6ff
                                            } else {
                                                telecat_theme::MUTED
                                            }))
                                            .child(detail),
                                    )
                                })
                                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                    if this.picker_selection != i {
                                        this.picker_selection = i;
                                        cx.notify();
                                    }
                                }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose_picker(pick.clone(), window, cx)
                                }))
                        })),
                )
                .child(super::layout::scrollbar(&self.picker_scroll, cx)),
        )
    }
}
impl Workspace {
    pub(super) fn reload_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.flush_grid_edits(window, cx);
        let dirty = self
            .state
            .tabs
            .iter()
            .any(|t| t.table.as_ref().is_some_and(|t| t.dirty()));
        if !dirty {
            self.reload_all_tabs(cx);
            return;
        }
        let prompt=window.prompt(PromptLevel::Warning,"Discard unsaved changes?",Some("Refreshing reloads every table and throws away staged edits. Press ⌘S to save them first."),&["Discard & Refresh","Cancel"],cx);
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let yes = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if yes {
                    this.reload_all_tabs(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn reload_all_tabs(&mut self, cx: &mut Context<Self>) {
        let active = self.state.active;
        let ids: Vec<_> = self
            .state
            .tabs
            .iter()
            .filter(|t| t.kind != TabKind::Query)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.state.active = id;
            if let Some(t) = self.state.current_mut().table.as_mut() {
                if !t.is_new {
                    t.discard();
                    t.structure.loaded = false;
                    t.index.loaded = false;
                }
            }
            self.refresh_tab(cx);
            self.load_metadata(cx);
        }
        self.state.active = active;
        self.table_inputs.clear();
        self.refresh_objects(cx);
        cx.notify();
    }
}

// Picker.tsx: substring matches before subsequences, stable ties, UTF-16 offsets.
fn fuzzy_score(q: &str, text: &str) -> Option<usize> {
    if q.is_empty() {
        return Some(0);
    }
    let text: Vec<_> = text.to_lowercase().encode_utf16().collect();
    let query: Vec<_> = q.encode_utf16().collect();
    if let Some(index) = text.windows(query.len()).position(|s| s == query) {
        return Some(if index == 0 { 0 } else { 1000 + index });
    }
    let mut ti = 0;
    let mut gaps = 0;
    for ch in q.chars() {
        let mut buf = [0u16; 2];
        let needle = ch.encode_utf16(&mut buf);
        let f = text
            .get(ti..)?
            .windows(needle.len())
            .position(|s| s == needle)?;
        gaps += f;
        ti += f + 1;
    }
    Some(10000 + gaps * 1000)
}
#[cfg(test)]
mod tests {
    #[test]
    fn picker_fuzzy_matches_electron_utf16_scores_and_ordering() {
        for (q, text, expected) in [
            ("", "anything", Some(0)),
            ("qa", "qa_bit", Some(0)),
            ("qa", "public.qa_bit", Some(1007)),
            ("qbt", "qa_bit", Some(13000)),
            ("bank", "sa_banks", Some(1003)),
            ("🏦b", "🐈🏦bank", Some(1002)),
            ("ธก", "ธนาคารกรุงเทพ", Some(15000)),
            ("missing", "qa_bit", None),
        ] {
            assert_eq!(super::fuzzy_score(q, text), expected, "{q}: {text}");
        }
    }
}
