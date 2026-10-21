//! Adapters keep rendering/input in the components and all SQL/commit policy in the host.
use super::*;
use std::collections::BTreeMap;
use telecat_app::table_model::Part;
use telecat_core::changes::{Cell as Value, EditRow, cell_modified, is_modified, row_key};
use telecat_editor::{EditorEvent, EditorOptions, SqlEditor};
use telecat_grid::{Cell, Column, DataGrid, Edits, GridEvent, GridModel, Hooks, RowState};

type GridKey = (u64, u8);
const HINT_FOR: std::time::Duration = std::time::Duration::from_secs(4);
#[derive(Default)]
pub(super) struct Components {
    editors: BTreeMap<u64, Entity<SqlEditor>>,
    grids: BTreeMap<GridKey, GridBinding>,
    schema: telecat_editor::Schema,
    edit_nonce: u64,
    row_search: BTreeMap<u64, Entity<InputState>>,
    last_active: u64,
    /// Rows to select again once a reload's rows are shown: (tab, row keys).
    pending_selection: Option<(u64, Vec<String>)>,
}
/// A selected row before a reload: its primary-key values (if the table has a key) and all of
/// its values.
pub(super) struct RememberedRow {
    key_values: Option<Vec<Option<String>>>,
    values: Vec<Option<String>>,
}
fn row_values(row: &EditRow) -> Vec<Option<String>> {
    row.orig.clone().unwrap_or_else(|| {
        row.cur
            .iter()
            .map(|v| v.value().map(str::to_owned))
            .collect()
    })
}
fn key_values(
    t: &telecat_app::table_model::TableEditor,
    row: &EditRow,
) -> Option<Vec<Option<String>>> {
    if t.primary_key.is_empty() {
        return None;
    }
    t.primary_key
        .iter()
        .map(|k| {
            let i = t.data.result.columns.iter().position(|c| c == k)?;
            match &row.orig {
                Some(orig) => orig.get(i).cloned(),
                None => row.cur.get(i).map(|v| v.value().map(str::to_owned)),
            }
        })
        .collect::<Option<Vec<_>>>()
        .filter(|v| v.iter().all(Option::is_some))
}
struct GridBinding {
    view: Entity<DataGrid>,
    loaded: bool,
    stamp: String,
    rows: Vec<usize>,
    _subscription: Subscription,
}
fn key(tab: &Tab) -> GridKey {
    (
        tab.id,
        tab.table.as_ref().map_or(3, |t| match t.part {
            Part::Data => 0,
            Part::Structure => 1,
            Part::Index => 2,
        }),
    )
}
impl Workspace {
    pub(super) fn prepare_components(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: BTreeSet<_> = self.state.tabs.iter().map(|t| t.id).collect();
        self.components.editors.retain(|id, _| ids.contains(id));
        self.components.grids.retain(|(id, _), _| ids.contains(id));
        let tab = self.state.current();
        let id = tab.id;
        if self.components.last_active != id {
            self.components.last_active = id;
            if !self.tree_focus.contains_focused(window, cx) {
                window.focus(&self.focus, cx);
            }
            if self.sheet.is_none() {
                if let Some(e) = self.components.editors.get(&id).cloned() {
                    e.update(cx, |e, cx| e.focus(window, cx));
                }
            }
        }
        if tab.kind == TabKind::Query && !self.components.editors.contains_key(&id) {
            let options = EditorOptions {
                driver: self.state.connection.driver,
                text: tab.editor.text.clone(),
                schema: self.components.schema.clone(),
                ..Default::default()
            };
            let cursor = tab.editor.cursor_utf16;
            let editor = cx.new(|cx| {
                let mut e = SqlEditor::new(options, window, cx);
                e.set_selections(
                    &[telecat_editor::Selection {
                        anchor: cursor,
                        head: cursor,
                    }],
                    cx,
                );
                e
            });
            self.subscriptions.push(cx.subscribe_in(
                &editor,
                window,
                move |this, editor, event, window, cx| {
                    match event {
                        EditorEvent::Change(_) | EditorEvent::SelectionChange(_) => {
                            let e = editor.read(cx);
                            let doc = EditorDocument {
                                text: e.text(cx).to_string(),
                                cursor_utf16: e.cursor(cx),
                                selection: e.selected_text(cx),
                                selection_from: e.selection(cx).start,
                            };
                            if let Some(t) = this.state.tabs.iter_mut().find(|t| t.id == id) {
                                t.editor = doc;
                            }
                            if matches!(event, EditorEvent::Change(_)) {
                                this.queue_drafts(cx);
                            }
                        }
                        EditorEvent::RunCurrent => this.run(false, window, cx),
                        EditorEvent::RunAll => this.run(true, window, cx),
                        EditorEvent::Beautify => this.format_editor(false, window, cx),
                        EditorEvent::Uglify => this.format_editor(true, window, cx),
                        EditorEvent::PreviousTab => this.state.step(-1),
                        EditorEvent::NextTab => this.state.step(1),
                        EditorEvent::SwitchConnection => {
                            crate::show_welcome(this.services.clone(), false, cx)
                        }
                        _ => {}
                    }
                    cx.notify();
                },
            ));
            if self.sheet.is_none() {
                let freeze = self.services.scenario;
                let e = editor.clone();
                window.on_next_frame(move |window, cx| {
                    e.update(cx, |e, cx| {
                        e.focus(window, cx);
                        if freeze {
                            e.freeze_caret(true, cx);
                        }
                    })
                });
            }
            self.components.editors.insert(id, editor);
        }
        let tab = self.state.current();
        let k = key(tab);
        let search = if let Some(input) = self.components.row_search.get(&id) {
            input.read(cx).value().to_lowercase()
        } else {
            self.table_inputs
                .get(&id)
                .map(|i| i.search.read(cx).value().to_lowercase())
                .unwrap_or_default()
        };
        let stamp = if let Some(t) = &tab.table {
            format!(
                "{}:{}:{:?}:{:?}:{:?}:{}",
                t.buffer().revision,
                tab.offset,
                t.hidden,
                t.order_by,
                t.column_filter,
                search
            )
        } else {
            format!(
                "{}:{}:{}:{:?}",
                tab.grid.revision,
                tab.shown,
                search,
                tab.result_edit.as_ref().map(|r| r.data.revision)
            )
        };
        let stamp = format!("{stamp}:{:?}", std::mem::discriminant(tab.load(tab.slot())));
        if self
            .components
            .grids
            .get(&k)
            .is_some_and(|g| g.stamp == stamp)
        {
            return;
        }
        let (mut model, rows) = make_grid(tab, self.state.connection.driver, &search);
        let staged = match (&tab.table, &tab.result_edit) {
            (Some(t), _) => Some(&t.buffer().rows),
            (None, Some(r)) => Some(&r.data.rows),
            (None, None) => None,
        };
        let keys: Vec<String> = staged.map_or_else(Vec::new, |staged| {
            rows.iter().map(|&real| staged[real].key.clone()).collect()
        });
        // "No rows" only once a load finished empty; never while loading or after a failure.
        let placeholder = tab
            .load(tab.slot())
            .placeholder(!model.rows.is_empty(), std::time::Instant::now());
        if !matches!(placeholder, Placeholder::None | Placeholder::Empty) {
            model.options.empty_text = String::new();
        }
        let loaded = tab.table.as_ref().is_none_or(|t| t.buffer().loaded);
        let running = tab.kind == TabKind::Running;
        if running {
            let name = if self.state.connection.driver == Driver::Mysql {
                "id"
            } else {
                "pid"
            };
            if let Some(c) = model.columns.iter().position(|c| c.name == name) {
                model.selection.rows = (0..model.rows.len())
                    .filter(|r| {
                        model
                            .cell_value(Cell { row: *r, col: c })
                            .is_some_and(|id| self.running.selected.contains(id))
                    })
                    .collect();
                model.selection.anchor = model.selection.rows.first().copied();
            }
        }
        let initial = !self.components.grids.contains_key(&k);
        if initial {
            if let Some(row) = tab.grid.selected_row {
                model.selection = telecat_grid::Selection::single(row);
            }
            if self.services.scenario {
                scenario_grid(&self.scenario, tab, &mut model);
            }
        }
        if let Some(binding) = self.components.grids.get_mut(&k) {
            binding.view.update(cx, |grid, cx| {
                grid.update_model(
                    |old| {
                        // Rebind by identity before the new rows replace the old ones.
                        old.set_row_keys(keys);
                        old.replace_data(model.columns, model.rows);
                        // Electron mounts its metadata grid only after the first load.
                        // Match that sizing point, then retain widths through edits/refresh.
                        if !binding.loaded && loaded {
                            old.widths = model.widths;
                        }
                        if running {
                            old.selection = model.selection;
                        }
                        old.edits = model.edits;
                        old.options = model.options;
                        old.sort = model.sort;
                    },
                    cx,
                )
            });
            binding.stamp = stamp;
            binding.loaded = loaded;
            if let Some((_, keys)) = self
                .components
                .pending_selection
                .take_if(|(tab, _)| *tab == k.0 && k.1 == 0)
            {
                binding.view.update(cx, |g, cx| {
                    g.update_model(
                        |m| {
                            let at: Vec<usize> = keys
                                .iter()
                                .filter_map(|key| {
                                    (0..m.rows.len()).find(|&r| m.row_key(r) == Some(key.as_str()))
                                })
                                .collect();
                            m.selection.anchor = at.first().copied();
                            m.selection.rows = at.into_iter().collect();
                        },
                        cx,
                    )
                });
            }
            // The selected row shown in the inspector is the one the grid highlights.
            let tab = self.state.current();
            if tab.table.is_some() || tab.result_edit.is_some() {
                let anchor = binding.view.read(cx).model().selection.anchor;
                let selected = anchor.and_then(|r| rows.get(r)).copied();
                self.state.current_mut().grid.selected_row = selected;
            }
            binding.rows = rows;
        } else {
            model.set_row_keys(keys);
            let view = cx.new(|cx| DataGrid::new(model, cx));
            let subscription = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
                this.grid_event(k, event.clone(), window, cx)
            });
            self.components.grids.insert(
                k,
                GridBinding {
                    view,
                    loaded,
                    stamp,
                    rows,
                    _subscription: subscription,
                },
            );
        }
    }
    /// The Data grid's selected rows of tab `tab_id`, to select again after a reload.
    pub(super) fn remember_selection(&self, tab_id: u64, cx: &App) -> Vec<RememberedRow> {
        let (Some(g), Some(t)) = (
            self.components.grids.get(&(tab_id, 0)),
            self.state
                .tabs
                .iter()
                .find(|t| t.id == tab_id)
                .and_then(|t| t.table.as_ref()),
        ) else {
            return vec![];
        };
        let model = g.view.read(cx).model();
        model
            .selection
            .anchor
            .into_iter()
            .chain(model.selection.rows.iter().copied())
            .filter_map(|r| model.row_key(r))
            .filter_map(|key| t.data.rows.iter().position(|r| r.key == key))
            .fold(vec![], |mut out: Vec<usize>, r| {
                if !out.contains(&r) {
                    out.push(r);
                }
                out
            })
            .into_iter()
            .map(|position| RememberedRow {
                key_values: key_values(t, &t.data.rows[position]),
                values: row_values(&t.data.rows[position]),
            })
            .collect()
    }
    /// After a reload: select the remembered rows again. A row is found by its primary key or,
    /// without one, by being the only row with exactly its values. Anything else (the row left
    /// the filter or page, or the server changed it on save) is not selected at all: a position
    /// could name another row, and the user's next edit would land there. The first one found
    /// is the inspected row. Selection only: no editor is opened.
    pub(super) fn restore_selection(&mut self, tab_id: u64, remembered: Vec<RememberedRow>) {
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(t) = &tab.table else {
            return;
        };
        let found: Vec<usize> = remembered
            .iter()
            .filter_map(|r| match &r.key_values {
                Some(values) => t
                    .data
                    .rows
                    .iter()
                    .position(|row| key_values(t, row).as_ref() == Some(values)),
                None => {
                    let mut same = t
                        .data
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| row_values(row) == r.values)
                        .map(|(i, _)| i);
                    match (same.next(), same.next()) {
                        (Some(i), None) => Some(i),
                        _ => None,
                    }
                }
            })
            .collect();
        tab.grid.selected_row = found.first().copied();
        let keys = found.iter().map(|&r| t.data.rows[r].key.clone()).collect();
        self.components.pending_selection = Some((tab_id, keys));
    }
    pub(super) fn reset_component_focus(&mut self) {
        self.components.last_active = 0;
    }
    pub(super) fn clear_table_components(&mut self) {
        self.components.grids.clear();
    }
    pub(super) fn replace_editor_text(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(e) = self.components.editors.get(&self.state.active).cloned() {
            e.update(cx, |e, cx| {
                e.set_text(text, window, cx);
                e.focus(window, cx);
            });
            self.sync_editor_document(cx);
        } else {
            self.state.current_mut().editor.text = text;
        }
        self.persist_drafts();
        cx.notify();
    }
    pub(super) fn editor_view(&self) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .children(self.components.editors.get(&self.state.active).cloned())
    }
    pub(super) fn grid_view(&self, cx: &App) -> Div {
        let grid = self.components.grids.get(&key(self.state.current()));
        let has_rows = grid.is_some_and(|g| !g.view.read(cx).model().rows.is_empty());
        let placeholder = self.grid_placeholder(has_rows);
        let dimmed = matches!(placeholder, Placeholder::Loading { dimmed: true });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .child(
                div()
                    .size_full()
                    .flex()
                    .when(dimmed, |d| d.opacity(0.35))
                    .children(grid.map(|g| g.view.clone())),
            )
            .children(super::panels::load_state_view(&placeholder, "grid-state"))
            .children(self.hint.as_ref().map(|(text, _)| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(18.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("grid-hint")
                            .max_w(px(560.))
                            .px(px(12.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(0x5a4a1a))
                            .bg(rgba(0x2a2a2af2))
                            .font(ui_font())
                            .text_size(px(12.5))
                            .text_color(rgb(0xf0d27a))
                            .child(text.clone()),
                    )
            }))
    }
    /// The current grid's loading state. Electron's "No rows" pill stays the grid's own.
    pub(super) fn grid_placeholder(&self, has_rows: bool) -> Placeholder {
        let tab = self.state.current();
        tab.load(tab.slot())
            .placeholder(has_rows, std::time::Instant::now())
    }
    pub(super) fn sync_editor_document(&mut self, cx: &App) {
        if let Some(e) = self.components.editors.get(&self.state.active) {
            let e = e.read(cx);
            self.state.current_mut().editor = EditorDocument {
                text: e.text(cx).to_string(),
                cursor_utf16: e.cursor(cx),
                selection: e.selected_text(cx),
                selection_from: e.selection(cx).start,
            };
        }
    }
    pub(super) fn format_editor(
        &mut self,
        uglify: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.saving {
            return;
        }
        if let Some(e) = self.components.editors.get(&self.state.active).cloned() {
            e.update(cx, |e, cx| {
                if uglify {
                    e.uglify(window, cx)
                } else {
                    e.beautify(window, cx)
                }
            });
            self.sync_editor_document(cx);
            self.persist_drafts();
            cx.notify();
        }
    }
    pub(crate) fn flush_grid_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_inspector(window, cx);
        let grids: Vec<_> = self
            .components
            .grids
            .iter()
            .map(|(k, g)| (*k, g.view.clone()))
            .collect();
        for (k, grid) in grids {
            let events = grid.update(cx, |g, cx| g.flush_edit(cx));
            for event in events {
                self.grid_event(k, event, window, cx);
            }
        }
    }
    /// Grid events of a query result mapped onto tables. `false` for events every grid handles
    /// the same way.
    fn result_grid_event(
        &mut self,
        k: GridKey,
        event: &GridEvent,
        grid: &Entity<DataGrid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == k.0) else {
            return false;
        };
        if tab.table.is_some() {
            return false;
        }
        let Some(re) = tab.result_edit.as_mut() else {
            return false;
        };
        // Rows are found by identity, never by the index the grid showed (see grid_event).
        let locate = |re: &telecat_app::workspace_model::ResultEditor, key: Option<&str>| {
            key.and_then(|key| re.data.rows.iter().position(|r| r.key == key))
        };
        let model = grid.read(cx).model();
        let key_at = |r: usize| model.row_key(r).map(str::to_owned);
        let mut refused = None;
        match event {
            GridEvent::Selection { selection, .. } => {
                let anchor = selection.anchor.or_else(|| selection.rows.first().copied());
                let key = anchor.and_then(key_at);
                tab.grid.selected_row = locate(re, key.as_deref());
            }
            GridEvent::Edit { cell, value, key } => {
                if let Some(row) = locate(re, key.as_deref()) {
                    refused = re.stage(row, cell.col, Value::Text(value.clone())).err();
                }
            }
            GridEvent::DeleteRows(selected) => {
                let rows: Vec<usize> = selected
                    .iter()
                    .filter_map(|r| locate(re, r.key.as_deref()))
                    .collect();
                if !rows.is_empty() {
                    refused = re.delete(&rows).err();
                }
            }
            GridEvent::Menu { id, cell, key } => {
                let selected: Vec<String> = model
                    .selection
                    .rows
                    .iter()
                    .filter_map(|&r| key_at(r))
                    .collect();
                let Some(row) = locate(re, key.as_deref()) else {
                    return true;
                };
                refused = match id.as_str() {
                    "null" => re.stage(row, cell.col, Value::Null).err(),
                    "empty" => re.stage(row, cell.col, Value::Text(String::new())).err(),
                    "delete" => {
                        let rows: Vec<usize> = selected
                            .iter()
                            .filter_map(|k| locate(re, Some(k)))
                            .collect();
                        re.delete(&rows).err()
                    }
                    _ => None,
                };
            }
            GridEvent::ContextMenu { cell, bounds, key } => {
                let mut items = vec![];
                if re.plan.editable.get(cell.col).copied().unwrap_or(false) {
                    items.extend([
                        json!({"id":"null","label":"Set NULL"}),
                        json!({"id":"empty","label":"Set EMPTY"}),
                    ]);
                }
                if re.plan.delete.is_ok() {
                    items.push(json!({"id":"delete","label":"Delete Row"}));
                }
                if !items.is_empty() {
                    items.push(json!({"type":"separator"}));
                }
                items.extend(telecat_grid::COPY_MENU.iter().map(|item| {
                    item.map_or(
                        json!({"type":"separator"}),
                        |(id, label)| json!({"id":id,"label":label}),
                    )
                }));
                let (cell, key, grid) = (*cell, key.clone(), grid.clone());
                native_menu::choose(
                    json!(items),
                    point(px(bounds.x), px(bounds.y)),
                    window,
                    cx,
                    move |_, id, _, cx| {
                        grid.update(cx, |g, cx| g.activate_menu(&id, cell, key.as_deref(), cx))
                    },
                );
                return true;
            }
            GridEvent::Escape => re.data.discard(),
            GridEvent::EditRefused(cell) => {
                let key = key_at(cell.row);
                refused = Some(
                    match locate(re, key.as_deref()) {
                        Some(row) => re.plan.cell_reason(&re.data.rows, row, cell.col),
                        None => re.plan.column_reason(cell.col),
                    }
                    .unwrap_or_else(|| "This cell can't be edited.".into()),
                );
            }
            _ => return false,
        }
        if let Some(reason) = refused {
            self.show_hint(format!("Read-only: {reason}"), cx);
        }
        cx.notify();
        true
    }
    /// A short message over the grid, such as why a cell can't be edited. Fades after a while.
    pub(super) fn show_hint(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let shown = std::time::Instant::now();
        self.hint = Some((text.into(), shown));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HINT_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.hint.as_ref().is_some_and(|h| h.1 == shown) {
                    this.hint = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    /// Keeps the Messages text of the current query tab in a read-only, selectable text field.
    pub(super) fn prepare_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: BTreeSet<u64> = self.state.tabs.iter().map(|t| t.id).collect();
        self.messages.retain(|id, _| ids.contains(id));
        let tab = self.state.current();
        if tab.kind != TabKind::Query
            || self
                .messages
                .get(&tab.id)
                .is_some_and(|m| m.0 == messages_revision(tab))
        {
            return;
        }
        let (id, revision, text) = (tab.id, messages_revision(tab), tab.messages_text());
        let state = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx)
                .auto_grow(1, 100_000)
                .soft_wrap(true)
                .default_value(text);
            s.set_readonly(true, cx);
            s.set_editor_style(InputEditorStyle {
                foreground: rgb(telecat_theme::TEXT_SECONDARY).into(),
                muted_foreground: rgb(0x666666).into(),
                background: gpui_kit::transparent_black(),
                border: gpui_kit::transparent_black(),
                selection: rgba(0x2f6fed66).into(),
                caret: rgb(telecat_theme::TEXT).into(),
                ..Default::default()
            });
            s
        });
        self.messages.insert(id, (revision, state));
    }
    /// Puts the editor's caret on an error's location and selects the character there.
    pub(super) fn jump_to_location(
        &mut self,
        at: Location,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.components.editors.get(&self.state.active).cloned() else {
            return;
        };
        // The location is in the SQL as it ran; after an edit it could name another character.
        let tab = self.state.current();
        if tab.run_text != editor.read(cx).text(cx).as_ref() {
            self.show_hint(
                "The SQL changed since it ran: run it again to locate the error.",
                cx,
            );
            return;
        }
        editor.update(cx, |e, cx| {
            let text = e.text(cx);
            let len: usize = text.encode_utf16().count();
            let start = at.utf16.min(len);
            let width = text
                .chars()
                .scan(0usize, |units, c| {
                    let here = *units;
                    *units += c.len_utf16();
                    Some((here, c))
                })
                .find(|(here, _)| *here == start)
                .map_or(0, |(_, c)| if c == '\n' { 0 } else { c.len_utf16() });
            e.set_selections(
                &[telecat_editor::Selection {
                    anchor: start,
                    head: start + width,
                }],
                cx,
            );
            e.focus(window, cx);
        });
        self.sync_editor_document(cx);
        cx.notify();
    }
    /// The SQL can't change under staged result edits: they belong to the result it produced.
    pub(super) fn lock_sql_editor(&self, cx: &mut Context<Self>) {
        let tab = self.state.current();
        let locked = tab.result_dirty();
        if let Some(e) = self.components.editors.get(&tab.id) {
            let state = e.read(cx).input_state().clone();
            state.update(cx, |s, cx| {
                if s.is_editable() == locked {
                    s.set_readonly(locked, cx);
                }
            });
        }
    }
    /// Closes every open cell editor without staging it.
    pub(super) fn cancel_grid_edits(&mut self, cx: &mut Context<Self>) {
        for g in self.components.grids.values() {
            g.view.update(cx, |g, cx| g.cancel_edit(cx));
        }
    }
    pub(super) fn focus_current_grid(&self, window: &mut Window, cx: &mut App) {
        if let Some(g) = self.components.grids.get(&key(self.state.current())) {
            window.focus(&g.view.focus_handle(cx), cx);
        }
    }
    pub(super) fn grid_editing(&self, cx: &App) -> bool {
        self.components
            .grids
            .get(&key(self.state.current()))
            .is_some_and(|g| g.view.read(cx).model().editing.is_some())
    }
    pub(super) fn update_completion_schema(&mut self, cx: &mut Context<Self>) {
        let mut schema = telecat_editor::Schema::new();
        for o in &self.state.objects {
            if matches!(o.kind, ObjectKind::Table | ObjectKind::View) {
                schema.insert(display_name(&o.table, self.state.connection.driver), vec![]);
            }
        }
        // Workspace.tsx supplies table/view names with empty column lists. Do
        // not crawl structure metadata or make completion depend on open tabs.
        self.components.schema = schema;
        for e in self.components.editors.values() {
            e.update(cx, |e, cx| e.set_schema(&self.components.schema, cx));
        }
    }
    fn grid_event(
        &mut self,
        k: GridKey,
        event: GridEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let Some(binding) = self.components.grids.get(&k) else {
            return;
        };
        let grid = binding.view.clone();
        let rows = binding.rows.clone();
        if self.result_grid_event(k, &event, &grid, window, cx) {
            return;
        }
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == k.0) else {
            return;
        };
        // Events carry their originating tab/part, never the currently selected tab.
        let mut inserted = None;
        let part = match k.1 {
            0 => Part::Data,
            1 => Part::Structure,
            _ => Part::Index,
        };
        // A grid event names rows as the grid showed them. Table rows are found again by
        // identity, because the rows may have been replaced since (a reload landing in the same
        // frame as a keystroke); a row that is gone takes its action with it.
        let locate = |t: &mut telecat_app::table_model::TableEditor, key: Option<&str>| {
            key.and_then(|key| buffer(t, part).rows.iter().position(|r| r.key == key))
        };
        match event {
            GridEvent::Selection { selection, .. } => {
                let anchor = selection.anchor.or_else(|| selection.rows.first().copied());
                tab.grid.selected_row = match &mut tab.table {
                    Some(t) => {
                        let key = anchor
                            .and_then(|r| grid.read(cx).model().row_key(r).map(str::to_owned));
                        locate(t, key.as_deref())
                    }
                    None => anchor.and_then(|r| rows.get(r)).copied(),
                };
                if tab.kind == TabKind::Running {
                    let name = if self.state.connection.driver == Driver::Mysql {
                        "id"
                    } else {
                        "pid"
                    };
                    if let Some(c) = tab.grid.result.columns.iter().position(|c| c == name) {
                        self.running.selected = selection
                            .rows
                            .iter()
                            .filter_map(|r| {
                                tab.grid
                                    .result
                                    .rows
                                    .get(*r)
                                    .and_then(|r| r.get(c))
                                    .cloned()
                                    .flatten()
                            })
                            .collect();
                    }
                }
            }
            GridEvent::Edit { cell, value, key } => {
                if let Some(t) = &mut tab.table {
                    if let Some(row) = locate(t, key.as_deref()) {
                        stage_cell(t, part, row, cell.col, Value::Text(value));
                    }
                }
            }
            GridEvent::DeleteRows(selected) => {
                if tab.kind == TabKind::Running {
                    self.kill_processes(false, window, cx);
                    return;
                }
                if let Some(t) = &mut tab.table {
                    let selected: Vec<_> = selected
                        .iter()
                        .filter_map(|r| locate(t, r.key.as_deref()))
                        .collect();
                    buffer(t, part).delete(&selected.iter().copied().collect::<Vec<_>>());
                }
            }
            GridEvent::Escape => {
                if let Some(t) = &mut tab.table {
                    buffer(t, part).discard();
                }
            }
            GridEvent::ToggleDetail => self.panel = !self.panel,
            GridEvent::QuickLook(cell) => {
                let model = grid.read(cx).model();
                self.sheet = Some(Sheet::QuickLook {
                    title: model.columns[cell.col].name.clone(),
                    value: model.cell_value(cell).unwrap_or("NULL").to_owned(),
                });
            }
            GridEvent::InsertRow => {
                self.add_grid_row(window, cx);
                return;
            }
            GridEvent::DuplicateRows(selected) => {
                if let Some(t) = &mut tab.table {
                    let found: Vec<_> = selected
                        .iter()
                        .filter_map(|r| locate(t, r.key.as_deref()))
                        .collect();
                    let values = found.iter().map(|&r| t.data.rows[r].cur.clone()).collect();
                    inserted = Some(insert_values(t, values));
                }
            }
            GridEvent::Paste(values) => {
                if let Some(t) = &mut tab.table {
                    if values
                        .iter()
                        .any(|r| r.len() != t.data.result.columns.len())
                    {
                        self.error("Cannot paste rows: clipboard columns must match the table. Copy whole tab-separated rows.",cx);
                        return;
                    }
                    inserted = Some(insert_values(
                        t,
                        values
                            .into_iter()
                            .map(|r| r.into_iter().map(Value::Text).collect())
                            .collect(),
                    ));
                }
            }
            GridEvent::Sort(sort) => {
                let sort = sort.map(|s| OrderBy {
                    column: s.column,
                    dir: if s.direction == telecat_grid::Direction::Asc {
                        Direction::Asc
                    } else {
                        Direction::Desc
                    },
                });
                self.navigate_table(TableNavigation::Sort(sort), window, cx);
                return;
            }
            GridEvent::ContextMenu { cell, bounds, key } => {
                let real = tab.table.as_mut().and_then(|t| locate(t, key.as_deref()));
                if tab.table.is_some() && real.is_none() {
                    return;
                }
                let mut items = vec![];
                if let Some(t) = &tab.table {
                    let real = real.unwrap_or(cell.row);
                    if part == Part::Data {
                        items.extend([
                            json!({"id":"null","label":"Set NULL"}),
                            json!({"id":"empty","label":"Set EMPTY"}),
                        ]);
                        if t.data.rows.get(real).is_some_and(|r| r.orig.is_none()) {
                            items.push(json!({"id":"default","label":"Set DEFAULT"}));
                        }
                        items.push(json!({"type":"separator"}));
                    }
                    items.push(json!({"id":"delete","label":if part==Part::Data {"Delete Row"} else {"Delete"}}));
                    if part != Part::Data
                        || t.data
                            .rows
                            .get(real)
                            .is_some_and(|r| r.deleted || is_modified(r))
                    {
                        items.push(json!({"id":"revert","label":if part==Part::Data {"Revert Row"}else{"Revert"}}));
                    }
                    items.push(json!({"type":"separator"}));
                }
                items.extend(telecat_grid::COPY_MENU.iter().map(|item| {
                    item.map_or(
                        json!({"type":"separator"}),
                        |(id, label)| json!({"id":id,"label":label}),
                    )
                }));
                native_menu::choose(
                    json!(items),
                    point(px(bounds.x), px(bounds.y)),
                    window,
                    cx,
                    // The menu is modal: the grid acts only if the same row is still at `cell`.
                    move |_, id, _, cx| {
                        grid.update(cx, |g, cx| g.activate_menu(&id, cell, key.as_deref(), cx))
                    },
                );
            }
            GridEvent::Menu { id, cell, key } => {
                let model = grid.read(cx).model();
                let selected_keys: Vec<_> = model
                    .selection
                    .rows
                    .iter()
                    .filter_map(|&r| model.row_key(r).map(str::to_owned))
                    .collect();
                if let Some(t) = &mut tab.table {
                    let Some(row) = locate(t, key.as_deref()) else {
                        return;
                    };
                    let cell = Cell { row, ..cell };
                    let selected: Vec<_> = selected_keys
                        .iter()
                        .filter_map(|k| locate(t, Some(k)))
                        .collect();
                    let b = buffer(t, part);
                    match id.as_str() {
                        "null" => b.set_cell(cell.row, cell.col, Value::Null),
                        "empty" => b.set_cell(cell.row, cell.col, Value::Text(String::new())),
                        "default" => b.set_cell(cell.row, cell.col, Value::unset()),
                        "delete" => b.delete(&selected),
                        "revert" => b.apply(|rows| {
                            if rows.get(cell.row).is_some_and(|r| r.orig.is_none()) {
                                rows.remove(cell.row);
                                return;
                            }
                            if let Some(r) = rows.get_mut(cell.row) {
                                if let Some(orig) = &r.orig {
                                    r.cur = orig.iter().cloned().map(Value::from).collect();
                                    r.deleted = false;
                                }
                            }
                        }),
                        _ => {}
                    }
                }
            }
            GridEvent::CellPopup { cell, bounds, key } => {
                self.metadata_popup(k, key, cell, bounds, window, cx);
                return;
            }
            GridEvent::Resize { .. } => {}
            GridEvent::EditRefused(cell) => {
                let reason = match &tab.table {
                    Some(_) => "This column can't be edited as text (binary data).".to_owned(),
                    None if tab.kind == TabKind::Query => {
                        "Checking whether this result can be edited… Try again in a moment.".into()
                    }
                    None => "These results are read-only.".into(),
                };
                let _ = cell;
                self.show_hint(reason, cx);
                return;
            }
        }
        if let Some(range) = inserted.filter(|r| !r.is_empty()) {
            tab.grid.selected_row = Some(range.start);
            self.prepare_components(window, cx);
            if let Some(g) = self.components.grids.get(&k) {
                let selected: BTreeSet<_> = g
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(visible, real)| range.contains(real).then_some(visible))
                    .collect();
                g.view.update(cx, |g, cx| {
                    g.update_model(
                        |m| {
                            m.selection.anchor = selected.first().copied();
                            m.selection.rows = selected;
                            if let Some(row) = m.selection.anchor {
                                m.scroll_to_row(row);
                            }
                        },
                        cx,
                    )
                });
            }
        }
        cx.notify();
    }
    pub(super) fn add_grid_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let d = self.state.connection.driver;
        let tab = self.state.current_mut();
        let Some(t) = &mut tab.table else {
            return;
        };
        if !t.buffer().loaded {
            return;
        }
        let real = t.buffer().rows.len();
        let part = t.part;
        let preferred = |name: &str| match part {
            Part::Data => !t.primary_key.iter().any(|k| k == name),
            Part::Structure => name == "column_name",
            Part::Index => true,
        };
        let preferred: Vec<bool> = t
            .buffer()
            .result
            .columns
            .iter()
            .map(|c| preferred(c))
            .collect();
        t.add_row(d);
        self.prepare_components(window, cx);
        self.components.edit_nonce += 1;
        let Some(g) = self.components.grids.get(&key(self.state.current())) else {
            cx.notify();
            return;
        };
        let nonce = self.components.edit_nonce;
        let row = g.rows.iter().position(|&r| r == real);
        g.view.update(cx, |g, cx| {
            // Only a column the user can see gets the editor; hidden ones never do.
            let visible: Vec<usize> = g.model().visible_columns().collect();
            let col = visible
                .iter()
                .copied()
                .find(|&c| preferred.get(c).copied().unwrap_or(false))
                .or_else(|| visible.first().copied());
            match (row, col) {
                (Some(row), Some(col)) => g.request_edit(Cell { row, col }, nonce, window, cx),
                (Some(row), None) => g.update_model(
                    |m| {
                        m.selection = telecat_grid::Selection::single(row);
                        m.scroll_to_row(row);
                    },
                    cx,
                ),
                _ => {}
            }
        });
        cx.notify();
    }
}
/// Stages one cell edit. The grid and the row inspector both go through here, so every edit
/// lands in the same undoable buffer and is committed by the same guarded ⌘S path.
pub(super) fn stage_cell(
    t: &mut telecat_app::table_model::TableEditor,
    part: Part,
    row: usize,
    col: usize,
    value: Value,
) {
    buffer(t, part).set_cell(row, col, value);
}
/// Whether a Data column can be edited as text: binary values never round-trip through text.
pub(super) fn data_column_editable(result: &QueryResult, col: usize) -> bool {
    !result
        .binary
        .as_ref()
        .and_then(|b| b.get(col))
        .copied()
        .unwrap_or(false)
}
fn buffer(
    t: &mut telecat_app::table_model::TableEditor,
    part: Part,
) -> &mut telecat_app::table_model::EditBuffer {
    match part {
        Part::Data => &mut t.data,
        Part::Structure => &mut t.structure,
        Part::Index => &mut t.index,
    }
}
fn insert_values(
    t: &mut telecat_app::table_model::TableEditor,
    values: Vec<Vec<Value>>,
) -> std::ops::Range<usize> {
    let start = t.data.rows.len();
    if values.is_empty() || !t.data.loaded {
        return start..start;
    }
    let columns = &t.data.result.columns;
    let pk = &t.primary_key;
    let new: Vec<_> = values
        .into_iter()
        .map(|values| EditRow {
            key: row_key(),
            orig: None,
            deleted: false,
            rid: None,
            cur: columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if pk.contains(c) {
                        Value::unset()
                    } else {
                        match values.get(i) {
                            Some(Value::Text(v)) if v == "NULL" => Value::Null,
                            Some(Value::Text(v)) if v == "DEFAULT" => Value::unset(),
                            Some(v) => v.clone(),
                            None => Value::unset(),
                        }
                    }
                })
                .collect(),
        })
        .collect();
    t.data.apply(|rows| rows.extend(new));
    start..t.data.rows.len()
}
/// Grid rows for staged rows, with their colours; `mapping` gets each shown row's index.
fn buffer_grid_rows(
    rows: &[EditRow],
    edits: &mut Edits,
    mapping: &mut Vec<usize>,
    keep: impl Fn(&EditRow) -> bool,
) -> Vec<Vec<Option<String>>> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| keep(r))
        .enumerate()
        .map(|(row, (real, r))| {
            mapping.push(real);
            if r.deleted {
                edits.rows.insert(row, RowState::Deleted);
            } else if r.orig.is_none() {
                edits.rows.insert(row, RowState::New);
            }
            r.cur
                .iter()
                .enumerate()
                .map(|(col, v)| {
                    if matches!(v, Value::Unset { .. }) {
                        edits.defaults.insert(Cell { row, col });
                    }
                    if cell_modified(r, col) {
                        edits.modified.insert(Cell { row, col });
                    }
                    v.value().map(str::to_owned)
                })
                .collect()
        })
        .collect()
}
fn make_grid(tab: &Tab, driver: Driver, search: &str) -> (GridModel, Vec<usize>) {
    let result = match (&tab.table, &tab.result_edit) {
        (Some(t), _) => &t.buffer().result,
        (None, Some(r)) => &r.data.result,
        (None, None) => &tab.grid.result,
    };
    let part = tab.table.as_ref().map(|t| t.part);
    let columns = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut c = Column::new(name);
            c.numeric = result
                .numeric
                .as_ref()
                .and_then(|n| n.get(i))
                .copied()
                .unwrap_or(false);
            c.editable = match part {
                Some(Part::Data) => data_column_editable(result, i),
                Some(Part::Structure) => {
                    matches!(
                        name.as_str(),
                        "column_name" | "data_type" | "column_default" | "comment"
                    ) || (name == "collation" && driver == Driver::Mysql)
                }
                Some(Part::Index) => matches!(name.as_str(), "index_name" | "column_name"),
                None => tab
                    .result_edit
                    .as_ref()
                    .is_some_and(|r| r.plan.editable.get(i).copied().unwrap_or(false)),
            };
            c.chevron = match part {
                Some(Part::Structure) => {
                    matches!(
                        name.as_str(),
                        "data_type" | "is_nullable" | "column_default"
                    ) || (driver == Driver::Mysql && name == "extra")
                }
                Some(Part::Index) => matches!(
                    name.as_str(),
                    "index_algorithm" | "is_unique" | "column_name"
                ),
                _ => false,
            };
            c.hidden = tab.table.as_ref().is_some_and(|t| {
                t.part == Part::Data
                    && (t.hidden.contains(name)
                        || t.column_filter
                            .as_ref()
                            .is_some_and(|q| !name.to_lowercase().contains(&q.to_lowercase())))
            });
            c
        })
        .collect();
    let mut edits = Edits::default();
    let mut mapping = vec![];
    let rows = if let Some(t) = &tab.table {
        buffer_grid_rows(&t.buffer().rows, &mut edits, &mut mapping, |r| {
            search.is_empty()
                || t.part == Part::Data
                || r.cur.iter().enumerate().any(|(i, c)| {
                    (t.part == Part::Index || result.columns[i] == "column_name")
                        && c.value().unwrap_or("").to_lowercase().contains(search)
                })
        })
    } else if let Some(re) = &tab.result_edit {
        buffer_grid_rows(&re.data.rows, &mut edits, &mut mapping, |r| {
            search.is_empty()
                || r.cur
                    .iter()
                    .any(|v| v.value().unwrap_or("").to_lowercase().contains(search))
        })
    } else {
        result
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                search.is_empty()
                    || r.iter()
                        .flatten()
                        .any(|v| v.to_lowercase().contains(search))
            })
            .map(|(i, r)| {
                mapping.push(i);
                r.clone()
            })
            .collect()
    };
    let mut m = GridModel::new(columns, rows);
    if matches!(part, Some(Part::Structure | Part::Index))
        && tab.table.as_ref().is_some_and(|t| !t.is_new)
    {
        // A staged fixture is a grid mounted before editing, just like TableTab.tsx.
        // New NULL/DEFAULT cells must not enlarge already measured columns.
        m.widths = telecat_grid::default_widths(&m.columns, &result.rows);
    }
    m.edits = edits;
    m.options.show_row_numbers = !matches!(part, Some(Part::Structure | Part::Index));
    m.options.show_empty = !m.options.show_row_numbers;
    m.options.row_text_offset = if m.options.show_empty { -1. } else { 0. };
    m.options.row_offset = if part == Some(Part::Data) {
        tab.offset as usize
    } else {
        0
    };
    m.options.empty_text = if tab.kind == TabKind::Query && result.columns.is_empty() {
        ""
    } else {
        "No rows"
    }
    .into();
    let result_edit = tab.result_edit.as_ref();
    m.options.hooks = Hooks {
        sort: part == Some(Part::Data),
        edit: part.is_some() || result_edit.is_some(),
        delete: part.is_some()
            || tab.kind == TabKind::Running
            || result_edit.is_some_and(|r| r.plan.delete.is_ok()),
        insert: part == Some(Part::Data),
        duplicate: part == Some(Part::Data),
        paste: part == Some(Part::Data),
        detail: part.is_none() || part == Some(Part::Data),
        quick_look: true,
        popup: part.is_some(),
        escape: tab.table.as_ref().is_some_and(|t| t.buffer().dirty()) || tab.result_dirty(),
    };
    m.sort = tab
        .table
        .as_ref()
        .and_then(|t| t.order_by.as_ref())
        .map(|s| telecat_grid::Sort {
            column: s.column.clone(),
            direction: if s.dir == Direction::Asc {
                telecat_grid::Direction::Asc
            } else {
                telecat_grid::Direction::Desc
            },
        });
    (m, mapping)
}
fn scenario_grid(name: &str, tab: &Tab, m: &mut GridModel) {
    if tab.table.as_ref().is_some_and(|t| {
        t.part == Part::Data && t.table.name == "sa_banks" && m.columns.len() == 10
    }) {
        m.widths = vec![60., 106., 166., 303., 128., 75., 128., 121., 166., 131.];
    }
    if name == "51-data-staged" {
        m.scroll_x = 526.5;
        m.selected_column = Some(6);
    }
    match name {
        "53-structure-staged" => {
            m.selection = telecat_grid::Selection::single(9);
            m.selected_column = Some(2);
        }
        "54-index-staged" => {
            m.selection = telecat_grid::Selection::single(4);
            m.selected_column = Some(2);
        }
        "55-new-table" => {
            m.selection = telecat_grid::Selection::single(1);
            m.selected_column = Some(1);
        }
        "a1-preview" => m.selected_column = Some(1),
        _ => {}
    }
    if m.columns.len() == 1 && m.columns[0].name == "bank_name_th" {
        m.widths = vec![166.];
    }
}
impl Workspace {
    fn metadata_popup(
        &mut self,
        k: GridKey,
        row_key: Option<String>,
        visible: Cell,
        bounds: telecat_grid::Rect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self
            .state
            .tabs
            .iter()
            .find(|t| t.id == k.0)
            .and_then(|t| t.table.as_ref())
        else {
            return;
        };
        let part = if k.1 == 1 {
            Part::Structure
        } else {
            Part::Index
        };
        let b = if part == Part::Structure {
            &t.structure
        } else {
            &t.index
        };
        let Some(row) = b.rows.iter().position(|r| Some(&r.key) == row_key.as_ref()) else {
            return;
        };
        let cell = Cell { row, ..visible };
        let Some(name) = b.result.columns.get(cell.col).cloned() else {
            return;
        };
        let current = b
            .rows
            .get(cell.row)
            .and_then(|r| r.cur.get(cell.col))
            .and_then(Value::value)
            .unwrap_or("")
            .to_owned();
        let pg = self.state.connection.driver == Driver::Postgres;
        let mut options: Vec<String> = match name.as_str() {
            "data_type" => if pg {
                vec![
                    "integer",
                    "bigint",
                    "smallint",
                    "numeric(10,2)",
                    "real",
                    "double precision",
                    "boolean",
                    "varchar(255)",
                    "text",
                    "uuid",
                    "json",
                    "jsonb",
                    "date",
                    "timestamp",
                    "timestamptz",
                    "time",
                    "bytea",
                ]
            } else {
                vec![
                    "int",
                    "bigint",
                    "tinyint",
                    "smallint",
                    "decimal(10,2)",
                    "float",
                    "double",
                    "varchar(255)",
                    "char(1)",
                    "text",
                    "mediumtext",
                    "longtext",
                    "json",
                    "date",
                    "datetime",
                    "timestamp",
                    "time",
                    "boolean",
                    "blob",
                ]
            }
            .into_iter()
            .map(str::to_owned)
            .collect(),
            "is_nullable" => vec!["YES".into(), "NO".into()],
            "column_default" => vec![
                "NULL".into(),
                "EMPTY STRING".into(),
                if pg { "now()" } else { "CURRENT_TIMESTAMP" }.into(),
            ],
            "extra" => vec![
                "".into(),
                "auto_increment".into(),
                "on update CURRENT_TIMESTAMP".into(),
            ],
            "index_algorithm" => if pg {
                vec!["btree", "hash", "gist", "gin", "brin", "spgist"]
            } else {
                vec!["BTREE", "HASH", "FULLTEXT", "SPATIAL"]
            }
            .into_iter()
            .map(str::to_owned)
            .collect(),
            "is_unique" => vec!["TRUE".into(), "FALSE".into()],
            "column_name" if part == Part::Index => {
                let col = t
                    .structure
                    .result
                    .columns
                    .iter()
                    .position(|c| c == "column_name");
                t.structure
                    .rows
                    .iter()
                    .filter(|r| !r.deleted)
                    .filter_map(|r| {
                        col.and_then(|c| r.cur.get(c))
                            .and_then(Value::value)
                            .map(str::to_owned)
                    })
                    .collect()
            }
            _ => return,
        };
        if !matches!(name.as_str(), "is_nullable" | "is_unique") {
            options.push("\0manual".into());
        }
        let items:Vec<_>=options.iter().enumerate().map(|(i,v)|json!({"id":i.to_string(),"label":if v=="\0manual" {"Manual input..."}else if v.is_empty(){"(none)"}else{v},"checked":if name=="column_name" {current.split(',').any(|c|c.trim()==v)}else{v==&current||(name=="column_default"&&current.is_empty()&&v=="NULL")},"separatorBefore":v=="\0manual"})).collect();
        native_menu::choose(
            json!(items),
            point(px(bounds.x + 2.), px(bounds.y + 2.)),
            window,
            cx,
            move |this, id, window, cx| {
                let Some(value) = id.parse::<usize>().ok().and_then(|i| options.get(i)) else {
                    return;
                };
                // The menu is modal; a reload may have replaced the row meanwhile.
                let Some(g) = this.components.grids.get(&k).map(|g| g.view.clone()) else {
                    return;
                };
                if g.read(cx).model().row_key(visible.row) != row_key.as_deref() {
                    return;
                }
                if value == "\0manual" {
                    this.components.edit_nonce += 1;
                    if let Some(g) = this.components.grids.get(&k) {
                        let nonce = this.components.edit_nonce;
                        g.view
                            .update(cx, |g, cx| g.request_edit(visible, nonce, window, cx));
                    }
                } else if let Some(t) = this
                    .state
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == k.0)
                    .and_then(|t| t.table.as_mut())
                {
                    let v = if name == "column_name" {
                        let mut columns: Vec<_> = current
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect();
                        if columns.contains(value) {
                            columns.retain(|c| c != value);
                        } else {
                            columns.push(value.clone());
                        }
                        if columns.is_empty() {
                            Value::Null
                        } else {
                            Value::Text(columns.join(", "))
                        }
                    } else if (name == "column_default" && value == "NULL") || value.is_empty() {
                        Value::Null
                    } else {
                        Value::Text(if name == "column_default" && value == "EMPTY STRING" {
                            "''".into()
                        } else {
                            value.clone()
                        })
                    };
                    let b = buffer(t, part);
                    if let Some(row) = b.rows.iter().position(|r| Some(&r.key) == row_key.as_ref())
                    {
                        b.set_cell(row, cell.col, v);
                    }
                }
                cx.notify();
            },
        );
    }
    pub(super) fn selected_process_ids(&self, cx: &App) -> Vec<String> {
        let tab = self.state.current();
        let Some(g) = self.components.grids.get(&key(tab)) else {
            return vec![];
        };
        let model = g.view.read(cx).model();
        let column = if self.state.connection.driver == Driver::Mysql {
            "id"
        } else {
            "pid"
        };
        let Some(c) = model.columns.iter().position(|c| c.name == column) else {
            return vec![];
        };
        model
            .selection
            .rows
            .iter()
            .filter_map(|r| {
                model
                    .cell_value(Cell { row: *r, col: c })
                    .map(str::to_owned)
            })
            .collect()
    }
    pub(super) fn kill_processes(
        &mut self,
        connection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let ids = self.selected_process_ids(cx);
        if ids.is_empty() {
            return;
        }
        let what = if connection {
            "kill the connection of"
        } else {
            "kill the running query of"
        };
        let target = if ids.len() == 1 {
            format!("process {}", ids[0])
        } else {
            format!("{} processes", ids.len())
        };
        let prompt = window.prompt(
            PromptLevel::Warning,
            &format!("Do you want to {what} {target}?"),
            Some(if connection {
                "The client is disconnected."
            } else {
                "The statement is aborted; the client stays connected."
            }),
            &[
                if connection {
                    "Kill Connection"
                } else {
                    "Kill Query"
                },
                "Cancel",
            ],
            cx,
        );
        let rt = self.services.runtime.clone();
        cx.spawn(async move |this, cx| {
            if prompt.await != Ok(0) {
                return;
            }
            let result = rt
                .spawn(async move {
                    for id in ids {
                        session.kill(&id, connection).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => this.refresh_tab(cx),
                    Ok(Err(e)) => this.failed(e, cx),
                    Err(e) => this.error(e, cx),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
impl Workspace {
    pub(super) fn input_focused(&self, window: &Window, cx: &App) -> bool {
        [
            &self.search,
            &self.field_search,
            &self.sheet_input,
            &self.gate_password,
        ]
        .iter()
        .any(|i| i.focus_handle(cx).contains_focused(window, cx))
            || self.table_input_focused(window, cx)
            || self.inspector_focused(window, cx)
            || self
                .messages
                .values()
                .any(|m| m.1.focus_handle(cx).contains_focused(window, cx))
            || self
                .components
                .row_search
                .values()
                .any(|i| i.focus_handle(cx).contains_focused(window, cx))
    }
    pub(super) fn edit_menu(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.components.editors.get(&self.state.active).cloned() {
            if editor.focus_handle(cx).contains_focused(window, cx) {
                editor.update(cx, |e, cx| e.menu_action(name, window, cx));
                return;
            }
        }
        if self.input_focused(window, cx) || self.grid_editing(cx) {
            native_edit(name, window, cx);
            return;
        }
        if matches!(name, "undo" | "redo") {
            let tab = self.state.current_mut();
            let buffer = match (&mut tab.table, &mut tab.result_edit) {
                (Some(t), _) => Some(t.buffer_mut()),
                (None, Some(r)) => Some(&mut r.data),
                (None, None) => None,
            };
            let changed = buffer.is_some_and(|b| if name == "undo" { b.undo() } else { b.redo() });
            if name == "undo" && !changed {
                self.state.operations.pop();
            }
        } else if let Some(g) = self
            .components
            .grids
            .get(&key(self.state.current()))
            .map(|g| g.view.clone())
        {
            match name {
                "copy" => g.update(cx, |g, cx| g.copy_selection(cx)),
                "paste" | "paste-match" => g.update(cx, |g, cx| g.paste_clipboard(cx)),
                "select-all" => g.update(cx, |g, cx| {
                    g.update_model(
                        |m| {
                            m.selection.rows = (0..m.rows.len()).collect();
                            m.selection.anchor = Some(0);
                        },
                        cx,
                    );
                    cx.emit(GridEvent::Selection {
                        selection: g.model().selection.clone(),
                        column: g.model().selected_column,
                    });
                }),
                "delete" => {
                    let model = g.read(cx).model();
                    let rows = model
                        .selection
                        .rows
                        .iter()
                        .map(|&r| model.row_ref(r))
                        .collect();
                    self.grid_event(
                        key(self.state.current()),
                        GridEvent::DeleteRows(rows),
                        window,
                        cx,
                    );
                }
                _ => native_edit(name, window, cx),
            }
        } else {
            native_edit(name, window, cx);
        }
        cx.notify();
    }
}
pub(super) fn native_edit(name: &str, window: &mut Window, cx: &mut App) {
    use gpui_kit::base::input::{Copy, Cut, Delete, Paste, Redo, SelectAll, Undo};
    let action: Box<dyn Action> = match name {
        "undo" => Box::new(Undo),
        "redo" => Box::new(Redo),
        "copy" => Box::new(Copy),
        "cut" => Box::new(Cut),
        "paste" | "paste-match" => Box::new(Paste),
        "select-all" => Box::new(SelectAll),
        "delete" => Box::new(Delete),
        _ => return,
    };
    window.dispatch_action(action, cx);
}

#[cfg(test)]
mod empty_tests;
#[cfg(test)]
pub(in crate::workspace) mod tests;
impl Workspace {
    pub(super) fn toggle_row_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.state.active;
        if self.components.row_search.remove(&id).is_none() {
            let input = text_input("", "Filter rows…", false, window, cx);
            self.subscriptions
                .push(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            input.update(cx, |s, cx| s.focus(window, cx));
            self.components.row_search.insert(id, input);
        }
        cx.notify();
    }
    pub(super) fn row_search_view(&self) -> Option<Div> {
        self.components
            .row_search
            .get(&self.state.active)
            .map(|input| div().w(px(160.)).child(input_box(input, false, true)))
    }
    pub(super) fn visible_row_count(&self, cx: &App) -> usize {
        self.components
            .grids
            .get(&key(self.state.current()))
            .map_or(0, |g| g.view.read(cx).model().rows.len())
    }
    pub(super) fn export_result(&mut self, cx: &mut Context<Self>) {
        let tab = self.state.current();
        let (name, columns, rows) = if let Some(t) = &tab.table {
            (
                t.table.name.clone(),
                t.data.result.columns.clone(),
                t.data.result.rows.clone(),
            )
        } else {
            let Some(g) = self.components.grids.get(&key(tab)) else {
                return;
            };
            let m = g.view.read(cx).model();
            (
                "query_result".into(),
                m.columns.iter().map(|c| c.name.clone()).collect(),
                m.rows.iter().map(|r| r.to_owned()).collect(),
            )
        };
        if columns.is_empty() {
            return;
        }
        let directory = self.services.profile.root.clone();
        cx.spawn(async move |this, cx| {
            let path = match crate::export_dialog::choose_path(&directory, &name).await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.error(e, cx));
                    return;
                }
            };
            let output = export_text(
                &name,
                &columns,
                &rows,
                path.extension().and_then(|s| s.to_str()).unwrap_or("csv"),
            );
            let show = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(path, output) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => cx.reveal_path(&show),
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
}
fn export_text(
    name: &str,
    columns: &[String],
    rows: &[Vec<Option<String>>],
    extension: &str,
) -> String {
    match extension {
        "json" => serde_json::to_string_pretty(
            &rows
                .iter()
                .map(|r| {
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            (
                                c.clone(),
                                r.get(i)
                                    .cloned()
                                    .flatten()
                                    .map_or(serde_json::Value::Null, serde_json::Value::String),
                            )
                        })
                        .collect::<serde_json::Map<_, _>>()
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default(),
        "sql" => {
            let value = |v: &Option<String>| {
                v.as_ref().map_or("NULL".into(), |s| {
                    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
                })
            };
            rows.iter()
                .map(|r| {
                    format!(
                        "INSERT INTO {name} ({}) VALUES ({});",
                        columns.join(", "),
                        r.iter().map(value).collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        }
        _ => {
            let escape = |s: &str| {
                if s.contains([',', '"', '\r', '\n']) {
                    format!("\"{}\"", s.replace('"', "\"\""))
                } else {
                    s.to_owned()
                }
            };
            "\u{feff}".to_owned()
                + &std::iter::once(
                    columns
                        .iter()
                        .map(|c| escape(c))
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .chain(rows.iter().map(|r| {
                    r.iter()
                        .map(|v| escape(v.as_deref().unwrap_or("")))
                        .collect::<Vec<_>>()
                        .join(",")
                }))
                .collect::<Vec<_>>()
                .join("\r\n")
                + "\r\n"
        }
    }
}

/// Changes whenever the Messages text does: new results or a new note.
fn messages_revision(tab: &Tab) -> u64 {
    tab.grid
        .revision
        .wrapping_add((tab.notes.len() as u64) << 40)
}
