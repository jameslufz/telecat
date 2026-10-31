//! The Data grid's TablePlus menus and actions: the cell and column-header menus, Set Value,
//! Quick Filter, Copy As, Paste to Selected Cells, enum and boolean dropdowns, foreign keys and
//! paging. Every value change is staged in the tab's undoable buffer and saved by ⌘S.
use super::components::GridKey;
use super::*;
use telecat_app::table_model::{Part, TableEditor};
use telecat_core::changes::Cell as Value;
use telecat_core::columns::{
    ColumnDetail, ValueKind, boolean_literals, json_minify, json_pretty, toggle_set_member,
};
use telecat_core::copy::{CopyFormat, CopyRows, column_values, copy_text};
use telecat_core::filters::{foreign_key_filters, quick_filters};
use telecat_core::paste::{PasteTarget, plan_paste};
use telecat_grid::{Cell, Direction as GridDirection, GridEvent};

/// What the grid of a tab shows and has selected, as staged row and data column indices.
pub(super) struct GridPick {
    /// The selected rows, top to bottom.
    pub rows: Vec<usize>,
    /// The selected columns, left to right.
    pub columns: Vec<usize>,
    /// Every row shown, top to bottom.
    pub all_rows: Vec<usize>,
    /// Every visible column, left to right.
    pub all_columns: Vec<usize>,
}
/// A value the Set Value menu (and the row detail's field menu) writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetValue {
    Empty,
    Null,
    Default,
    Now,
    Pretty,
    Minify,
}
impl SetValue {
    pub(super) fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "sv-empty" => Self::Empty,
            "sv-null" => Self::Null,
            "sv-default" => Self::Default,
            "sv-now" => Self::Now,
            "sv-pretty" => Self::Pretty,
            "sv-minify" => Self::Minify,
            _ => return None,
        })
    }
}
/// The rows a tab's Data grid edits: a table's Data rows or a query result mapped to tables.
fn edit_buffer(tab: &Tab) -> Option<&telecat_app::table_model::EditBuffer> {
    match (&tab.table, &tab.result_edit) {
        (Some(t), _) => (t.part == Part::Data).then_some(&t.data),
        (None, Some(r)) => Some(&r.data),
        _ => None,
    }
}
/// Why cell (`row`, `col`) can't take a value, or `None` when it can.
fn cell_refusal(tab: &Tab, row: usize, col: usize) -> Option<String> {
    match (&tab.table, &tab.result_edit) {
        (Some(t), _) => {
            if t.part != Part::Data {
                Some("Only the Data view's cells take values here.".into())
            } else if !t.data.loaded {
                Some("Reload table data before editing again.".into())
            } else if t.data.rows.get(row).is_some_and(|r| r.deleted) {
                Some(telecat_core::paste::PasteRefusal::DeletedRow.message())
            } else if !t.data_column_editable(col) {
                Some(t.data_column_refusal(col).into())
            } else {
                None
            }
        }
        (None, Some(r)) => r
            .plan
            .cell_reason(&r.data.rows, row, col)
            .map(|reason| format!("Read-only: {reason}")),
        _ => Some("These results are read-only.".into()),
    }
}
/// Stages `cells` into the tab's rows as one undo step, or nothing at all.
pub(super) fn stage_cells(tab: &mut Tab, cells: Vec<(usize, usize, Value)>) -> Result<(), String> {
    if let Some(reason) = cells
        .iter()
        .find_map(|(row, col, _)| cell_refusal(tab, *row, *col))
    {
        return Err(reason);
    }
    match (&mut tab.table, &mut tab.result_edit) {
        (Some(t), _) => {
            t.data.set_cells(cells);
            Ok(())
        }
        (None, Some(r)) => r.stage_cells(cells),
        _ => Err("These results are read-only.".into()),
    }
}
fn column_detail(tab: &Tab, col: usize) -> Option<&ColumnDetail> {
    let t = tab.table.as_ref()?;
    let name = t.data.result.columns.get(col)?;
    t.columns.as_ref()?.get(name)
}
fn value_kind(tab: &Tab, col: usize) -> ValueKind {
    column_detail(tab, col).map_or(ValueKind::Other, |c| c.kind)
}
/// A staged cell's value as JSON text: the server's own text for an unedited MySQL JSON value
/// (the grid shows unwrapped strings), else what the cell holds.
pub(super) fn json_source(tab: &Tab, row: usize, col: usize) -> Option<String> {
    let buffer = edit_buffer(tab)?;
    let r = buffer.rows.get(row)?;
    let edited = r.orig.is_none() || telecat_core::changes::cell_modified(r, col);
    if !edited
        && let Some(text) = buffer
            .result
            .json_text
            .as_ref()
            .and_then(|j| j.iter().find(|(c, _)| *c == col))
            .and_then(|(_, values)| values.get(row))
    {
        return text.clone();
    }
    r.cur.get(col).and_then(|v| v.value().map(str::to_owned))
}
/// What a tab's rows are, as a paste of them names their origin: `MySQL zixma.users`.
pub(super) fn copy_source(tab: &Tab, driver: Driver) -> String {
    let server = if driver == Driver::Mysql {
        "MySQL"
    } else {
        "PostgreSQL"
    };
    match &tab.table {
        Some(t) => format!("{server} {}.{}", t.table.schema, t.table.name),
        None => format!("a {server} query result"),
    }
}
/// The tab's row behind grid row `shown`: the binding's mapping, checked against the row's
/// identity when the rows are staged, so a row replaced since is never picked in its place.
fn real_row(
    tab: &Tab,
    binding: &super::components::GridBinding,
    model: &telecat_grid::GridModel,
    shown: usize,
) -> Option<usize> {
    let real = *binding.rows.get(shown)?;
    match edit_buffer(tab) {
        Some(b) => {
            (b.rows.get(real).map(|r| r.key.as_str()) == model.row_key(shown)).then_some(real)
        }
        None => Some(real),
    }
}
impl Workspace {
    /// The grid's selection of tab `k.0` as staged rows (or result rows) and data columns.
    pub(super) fn grid_pick(&self, k: GridKey, cx: &App) -> Option<GridPick> {
        let rows = self.selected_rows(k, cx)?;
        let binding = self.components.grid(k)?;
        let model = binding.view.read(cx).model();
        let tab = self.state.tabs.iter().find(|t| t.id == k.0)?;
        Some(GridPick {
            rows,
            columns: model.selected_columns(),
            all_rows: (0..model.rows.len())
                .filter_map(|shown| real_row(tab, binding, model, shown))
                .collect(),
            all_columns: model.visible_columns().collect(),
        })
    }
    /// Only the selected rows of grid `k`, top to bottom (cheap: the row inspector asks on every
    /// render).
    pub(super) fn selected_rows(&self, k: GridKey, cx: &App) -> Option<Vec<usize>> {
        let binding = self.components.grid(k)?;
        let model = binding.view.read(cx).model();
        let tab = self.state.tabs.iter().find(|t| t.id == k.0)?;
        // The selection is ordered by grid row, which is the order shown.
        Some(
            model
                .selection
                .rows
                .iter()
                .filter_map(|&shown| real_row(tab, binding, model, shown))
                .collect(),
        )
    }
    /// The values a copy reads for `rows` × `columns`: what the grid shows, with MySQL JSON as
    /// the server's text where the cell wasn't edited (as an export writes it).
    fn copy_values(
        &self,
        k: GridKey,
        rows: &[usize],
        columns: &[usize],
    ) -> Vec<Vec<Option<String>>> {
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return vec![];
        };
        rows.iter()
            .map(|&row| {
                columns
                    .iter()
                    .map(|&col| match edit_buffer(tab) {
                        Some(b) => {
                            let json = b
                                .result
                                .json_text
                                .as_ref()
                                .is_some_and(|j| j.iter().any(|(c, _)| *c == col));
                            if json {
                                json_source(tab, row, col)
                            } else {
                                b.rows
                                    .get(row)
                                    .and_then(|r| r.cur.get(col))
                                    .and_then(|v| v.value().map(str::to_owned))
                            }
                        }
                        None => tab
                            .grid
                            .result
                            .rows
                            .get(row)
                            .and_then(|r| r.get(col))
                            .cloned()
                            .flatten(),
                    })
                    .collect()
            })
            .collect()
    }
    /// Copy Rows As / Copy Selected Cells As.
    pub(super) fn copy_as(
        &mut self,
        k: GridKey,
        format: CopyFormat,
        cells: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(pick) = self.grid_pick(k, cx) else {
            return;
        };
        if pick.rows.is_empty() {
            return;
        }
        // Rows as shown (visible columns, display order), like ⌘C; every format names its
        // columns or keeps them in that order, so INSERT lists stay right whatever the order.
        let columns: Vec<usize> = if cells {
            pick.columns.clone()
        } else {
            pick.all_columns.clone()
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        let result = edit_buffer(tab).map_or(&tab.grid.result, |b| &b.result);
        let names: Vec<String> = columns
            .iter()
            .map(|&c| result.columns.get(c).cloned().unwrap_or_default())
            .collect();
        let binary: Vec<bool> = columns
            .iter()
            .map(|&c| {
                result
                    .binary
                    .as_ref()
                    .and_then(|b| b.get(c))
                    .copied()
                    .unwrap_or(false)
            })
            .collect();
        let driver = self.state.connection.driver;
        let target = match &tab.table {
            Some(t) => telecat_core::export::insert_target(driver, &t.table),
            None => super::export::single_source(tab).map_or_else(
                || telecat_core::changes::quote_ident(driver, "query_result"),
                |t| telecat_core::export::insert_target(driver, &t),
            ),
        };
        let rows = self.copy_values(k, &pick.rows, &columns);
        let text = copy_text(
            format,
            &CopyRows {
                driver,
                target,
                columns: &names,
                binary: &binary,
                rows: &rows,
            },
        );
        if format == CopyFormat::PlainText {
            // Telecat's own clipboard text: a paste of it maps values by column name.
            let from = telecat_grid::CopiedFrom {
                columns: names,
                whole_rows: !cells,
                source: copy_source(tab, driver),
            };
            telecat_grid::copy_to_clipboard(cx, text, from);
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    /// Copy All Column Values: every shown row's value in the clicked column.
    fn copy_column(&mut self, k: GridKey, col: usize, cx: &mut Context<Self>) {
        let Some(pick) = self.grid_pick(k, cx) else {
            return;
        };
        let values: Vec<Option<String>> = self
            .copy_values(k, &pick.all_rows, &[col])
            .into_iter()
            .map(|mut r| r.remove(0))
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(column_values(&values)));
    }
    /// Paste to Selected Cells: the copied block over the selection, all or nothing.
    pub(super) fn paste_cells(
        &mut self,
        k: GridKey,
        copied: Vec<Vec<String>>,
        from: Option<telecat_grid::CopiedFrom>,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let Some(pick) = self.grid_pick(k, cx) else {
            return;
        };
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == k.0) else {
            return;
        };
        let Some(buffer) = edit_buffer(tab) else {
            self.show_hint("These results are read-only.", cx);
            return;
        };
        let rows = &buffer.rows;
        // Whole rows Telecat copied (⌘C) go into the columns of their names, from the first
        // selected row; a block of cells (⇧⌘C) or outside text goes where it's put.
        let named = match from.filter(|f| f.whole_rows) {
            Some(from) => {
                let mapped = telecat_core::paste::columns_by_name(
                    &from.columns,
                    &buffer.result.columns,
                    &from.source,
                )
                .and_then(|cols| match cols.iter().find(|c| !pick.all_columns.contains(c)) {
                    Some(&hidden) => Err(format!(
                        "Cannot paste: the column {} is hidden here. Show it (Show all columns) to paste the copied rows. Nothing was pasted.",
                        buffer.result.columns[hidden]
                    )),
                    None => Ok(cols),
                });
                match mapped {
                    Ok(cols) => Some(cols),
                    Err(message) => {
                        self.show_hint(message, cx);
                        return;
                    }
                }
            }
            None => None,
        };
        let plan = plan_paste(
            &copied,
            &PasteTarget {
                rows: &pick.rows,
                columns: named.as_deref().unwrap_or(&pick.columns),
                all_rows: &pick.all_rows,
                all_columns: named.as_deref().unwrap_or(&pick.all_columns),
            },
            |row, col| cell_refusal(tab, row, col).is_none(),
            |row| rows.get(row).is_some_and(|r| r.deleted),
            |row| rows.get(row).is_some_and(|r| r.orig.is_none()),
        );
        let plan = match plan {
            Ok(plan) => plan,
            Err(refusal) => {
                self.show_hint(refusal.message(), cx);
                return;
            }
        };
        if let Err(reason) = stage_cells(tab, plan.cells) {
            self.show_hint(reason, cx);
        }
        cx.notify();
    }
    /// Set Value (⌥↩, ⌥-click or the cell menu) for every selected cell.
    pub(super) fn set_selected_value(
        &mut self,
        k: GridKey,
        value: SetValue,
        cx: &mut Context<Self>,
    ) {
        let Some(pick) = self.grid_pick(k, cx) else {
            return;
        };
        let cells: Vec<(usize, usize)> = pick
            .rows
            .iter()
            .flat_map(|&r| pick.columns.iter().map(move |&c| (r, c)))
            .collect();
        self.set_cells_value(k.0, cells, value, cx);
    }
    /// Writes `value` into `cells` of tab `tab_id` as one undo step. NOW() reads the server's
    /// clock for each column first.
    pub(super) fn set_cells_value(
        &mut self,
        tab_id: u64,
        cells: Vec<(usize, usize)>,
        value: SetValue,
        cx: &mut Context<Self>,
    ) {
        if self.saving || cells.is_empty() {
            return;
        }
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        if value == SetValue::Now {
            self.stage_now(tab_id, cells, cx);
            return;
        }
        let mut staged = vec![];
        for (row, col) in cells {
            let new = match value {
                SetValue::Empty => Value::Text(String::new()),
                SetValue::Null => Value::Null,
                SetValue::Default => {
                    let new_row = edit_buffer(tab)
                        .and_then(|b| b.rows.get(row))
                        .is_some_and(|r| r.orig.is_none());
                    if !new_row || tab.table.is_none() {
                        self.show_hint("DEFAULT can only be set on a new row.", cx);
                        return;
                    }
                    Value::unset()
                }
                SetValue::Pretty | SetValue::Minify => {
                    let Some(text) = json_source(tab, row, col) else {
                        continue;
                    };
                    let shaped = if value == SetValue::Pretty {
                        json_pretty(&text)
                    } else {
                        json_minify(&text)
                    };
                    match shaped {
                        Ok(t) => Value::Text(t),
                        Err(message) => {
                            self.show_hint(message, cx);
                            return;
                        }
                    }
                }
                SetValue::Now => unreachable!("handled above"),
            };
            staged.push((row, col, new));
        }
        if let Err(reason) = stage_cells(tab, staged) {
            self.show_hint(reason, cx);
        }
        cx.notify();
    }
    /// Set Value ▸ NOW(): the server's current date or time as each column stores it, read when
    /// chosen and staged as that value (like TablePlus, the time is fixed at the click).
    fn stage_now(&mut self, tab_id: u64, cells: Vec<(usize, usize)>, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == tab_id) else {
            return;
        };
        let mut columns: Vec<(usize, ColumnDetail)> = vec![];
        for &(_, col) in &cells {
            match column_detail(tab, col) {
                Some(c) if c.kind.is_temporal() => {
                    if !columns.iter().any(|(i, _)| *i == col) {
                        columns.push((col, c.clone()));
                    }
                }
                _ => {
                    self.show_hint("NOW() can only be set on a date or time column.", cx);
                    return;
                }
            }
        }
        let keys: Vec<(String, usize)> = edit_buffer(tab).map_or_else(Vec::new, |b| {
            cells
                .iter()
                .filter_map(|&(row, col)| b.rows.get(row).map(|r| (r.key.clone(), col)))
                .collect()
        });
        let task = self.services.runtime.spawn(async move {
            let mut now = vec![];
            for (col, detail) in columns {
                let text = session
                    .now_text(&detail)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("The server returned no time"))?;
                now.push((col, text));
            }
            Ok::<_, anyhow::Error>(now)
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                let now = match r {
                    Ok(Ok(now)) => now,
                    Ok(Err(e)) => return this.failed(e, cx),
                    Err(e) => return this.error(e, cx),
                };
                let Some(tab) = this.state.tabs.iter_mut().find(|t| t.id == tab_id) else {
                    return;
                };
                // Rows are found again by identity: a reload may have replaced them meanwhile.
                let cells: Vec<(usize, usize, Value)> =
                    edit_buffer(tab).map_or_else(Vec::new, |b| {
                        keys.iter()
                            .filter_map(|(key, col)| {
                                let row = b.rows.iter().position(|r| &r.key == key)?;
                                let text = now.iter().find(|(c, _)| c == col)?.1.clone();
                                Some((row, *col, Value::Text(text)))
                            })
                            .collect()
                    });
                if cells.len() != keys.len() {
                    this.show_hint(
                        "The rows changed before NOW() arrived; nothing was set.",
                        cx,
                    );
                } else if let Err(reason) = stage_cells(tab, cells) {
                    this.show_hint(reason, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// The Set Value items for the cell: EMPTY, NULL, DEFAULT, then NOW() and Date Picker… for
    /// dates and times, PRETTY and MINIFY for JSON.
    fn set_value_items(&self, tab: &Tab, row: usize, col: usize) -> Vec<serde_json::Value> {
        let kind = value_kind(tab, col);
        let new_row = edit_buffer(tab)
            .and_then(|b| b.rows.get(row))
            .is_some_and(|r| r.orig.is_none());
        let mut items = vec![
            json!({"id":"sv-empty","label":"EMPTY"}),
            json!({"type":"separator"}),
            json!({"id":"sv-null","label":"NULL"}),
            json!({"id":"sv-default","label":"DEFAULT","enabled":new_row && tab.table.is_some()}),
        ];
        if kind.is_temporal() {
            items.extend([
                json!({"type":"separator"}),
                json!({"id":"sv-now","label":"NOW()"}),
                json!({"id":"sv-date","label":"Date Picker..."}),
            ]);
        }
        let json = kind == ValueKind::Json
            || json_source(tab, row, col).is_some_and(|t| {
                t.trim_start().starts_with(['{', '['])
                    && serde_json::from_str::<serde_json::Value>(&t).is_ok()
            });
        if json {
            items.extend([
                json!({"type":"separator"}),
                json!({"id":"sv-pretty","label":"PRETTY"}),
                json!({"id":"sv-minify","label":"MINIFY"}),
            ]);
        }
        items
    }
    /// The staged row and data column of grid cell `cell` of `k`, found by the row's identity.
    pub(super) fn locate_grid_cell(
        &self,
        k: GridKey,
        cell: Cell,
        key: Option<&str>,
        cx: &App,
    ) -> Option<(usize, usize)> {
        let tab = self.state.tabs.iter().find(|t| t.id == k.0)?;
        let row = match edit_buffer(tab) {
            Some(b) => {
                let key = key?;
                b.rows.iter().position(|r| r.key == key)?
            }
            None => {
                let binding = self.components.grid(k)?;
                let _ = cx;
                *binding.rows.get(cell.row)?
            }
        };
        Some((row, cell.col))
    }
    /// ⌥↩ / ⌥-click: the Set Value menu for the selected cells.
    pub(super) fn set_value_menu(
        &mut self,
        k: GridKey,
        cell: Cell,
        bounds: telecat_grid::Rect,
        key: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.locate_grid_cell(k, cell, key.as_deref(), cx) else {
            return;
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        if let Some(reason) = cell_refusal(tab, row, col) {
            self.show_hint(reason, cx);
            return;
        }
        let items = self.set_value_items(tab, row, col);
        native_menu::choose(
            json!(items),
            point(px(bounds.x), px(bounds.y + bounds.height)),
            window,
            cx,
            move |this, id, window, cx| {
                this.data_menu_action(k, &id, cell, key.clone(), bounds, window, cx)
            },
        );
    }
    /// The cell menu of a table's Data grid or a query result, as TablePlus lays it out.
    pub(super) fn data_cell_menu(
        &mut self,
        k: GridKey,
        cell: Cell,
        bounds: telecat_grid::Rect,
        key: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.locate_grid_cell(k, cell, key.as_deref(), cx) else {
            return;
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        let table = tab.table.as_ref();
        let result = table.is_none();
        let writable = cell_refusal(tab, row, col).is_none();
        let rows_writable = table.is_some_and(|t| !t.read_only() && t.data.loaded);
        let deletable = match (table, &tab.result_edit) {
            (Some(t), _) => !t.read_only(),
            (None, Some(r)) => r.plan.delete.is_ok(),
            _ => false,
        };
        let shortcut = |label: &str, id: &str, key: &str, enabled: bool| json!({"id":id,"label":label,"key":key,"enabled":enabled});
        let formats = |prefix: &str| -> Vec<serde_json::Value> {
            let mut items = vec![];
            for f in CopyFormat::ALL {
                if f.separated() {
                    items.push(json!({"type":"separator"}));
                }
                items.push(json!({"id":format!("{prefix}:{}", f.id()),"label":f.label(result)}));
            }
            items
        };
        let mut items = vec![shortcut("Quick Look Editor", "quicklook", "⌘↩", true)];
        if writable {
            items.push(json!({"label":"Set Value          ⌥↩","submenu":self.set_value_items(tab, row, col)}));
        }
        items.push(json!({"type":"separator"}));
        if table.is_some() {
            items.push(shortcut("Refresh", "refresh", "⌥⌘R", true));
            items.push(json!({"type":"separator"}));
            items.push(shortcut("Paste", "paste", "⌘V", rows_writable));
        }
        if table.is_some() || tab.result_edit.is_some() {
            items.push(shortcut(
                "Paste to Selected Cells",
                "paste-cells",
                "⇧⌘V",
                writable,
            ));
        }
        if table.is_some() {
            items.push(shortcut("Add Row", "add-row", "⌘I", rows_writable));
            items.push(shortcut("Duplicate", "duplicate", "⌘D", rows_writable));
        }
        items.push(json!({"type":"separator"}));
        items.push(shortcut("Copy Rows", "copy-rows", "⌘C", true));
        items.push(json!({"label":"Copy Rows As","submenu":formats("copy-rows-as")}));
        items.push(shortcut("Copy Selected Cells", "copy-cells", "⇧⌘C", true));
        items.push(json!({"label":"Copy Selected Cells As","submenu":formats("copy-cells-as")}));
        items.push(json!({"id":"copy-column","label":"Copy All Column Values"}));
        if let Some(t) = table {
            let name = t.data.result.columns.get(col).cloned().unwrap_or_default();
            let value = t
                .data
                .rows
                .get(row)
                .and_then(|r| r.orig.as_ref())
                .and_then(|o| o.get(col).cloned())
                .flatten();
            let loaded_value = t.data.rows.get(row).is_some_and(|r| r.orig.is_some());
            let quick: Vec<serde_json::Value> = quick_filters(&name, value.as_deref())
                .into_iter()
                .enumerate()
                .map(|(i, q)| match q {
                    None => json!({"type":"separator"}),
                    Some(q) => json!({"id":format!("quick:{i}"),"label":q.label,"enabled":q.enabled && loaded_value}),
                })
                .collect();
            items.push(json!({"type":"separator"}));
            items.push(json!({"label":"Quick Filter","submenu":quick}));
        }
        items.push(json!({"type":"separator"}));
        items.push(shortcut("Delete", "delete", "⌫", deletable));
        if let Some(t) = table
            && t.data
                .rows
                .get(row)
                .is_some_and(|r| r.deleted || telecat_core::changes::is_modified(r))
        {
            items.push(json!({"id":"revert","label":"Revert Row"}));
        }
        native_menu::choose(
            json!(items),
            point(px(bounds.x), px(bounds.y)),
            window,
            cx,
            move |this, id, window, cx| {
                this.data_menu_action(k, &id, cell, key.clone(), bounds, window, cx)
            },
        );
    }
    /// Runs a chosen cell-menu item. The row is found again by its identity: if it is gone
    /// (a reload landed while the menu was open), nothing happens.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn data_menu_action(
        &mut self,
        k: GridKey,
        id: &str,
        cell: Cell,
        key: Option<String>,
        bounds: telecat_grid::Rect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let Some((row, col)) = self.locate_grid_cell(k, cell, key.as_deref(), cx) else {
            return;
        };
        let grid = self.components.grid(k).map(|g| g.view.clone());
        if let Some(value) = SetValue::from_id(id) {
            self.set_selected_value(k, value, cx);
            return;
        }
        if let Some(format) = id
            .strip_prefix("copy-rows-as:")
            .and_then(CopyFormat::from_id)
        {
            self.copy_as(k, format, false, cx);
            return;
        }
        if let Some(format) = id
            .strip_prefix("copy-cells-as:")
            .and_then(CopyFormat::from_id)
        {
            self.copy_as(k, format, true, cx);
            return;
        }
        if let Some(i) = id
            .strip_prefix("quick:")
            .and_then(|i| i.parse::<usize>().ok())
        {
            self.quick_filter(k, row, col, i, window, cx);
            return;
        }
        match id {
            "quicklook" => self.open_quick_look(k, row, col, window, cx),
            "sv-date" => self.open_date_picker(k, row, col, bounds, window, cx),
            "refresh" => self.reload_current_tab(window, cx),
            "paste" => {
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| g.paste_clipboard(cx));
                }
            }
            "paste-cells" => {
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| g.paste_cells(cx));
                }
            }
            "add-row" => self.add_grid_row(window, cx),
            "duplicate" | "delete" => {
                let Some(grid) = grid else {
                    return;
                };
                let model = grid.read(cx).model();
                let selected: Vec<telecat_grid::RowRef> = model
                    .selection
                    .rows
                    .iter()
                    .map(|&r| model.row_ref(r))
                    .collect();
                let event = if id == "delete" {
                    GridEvent::DeleteRows(selected)
                } else {
                    GridEvent::DuplicateRows(selected)
                };
                self.grid_event(k, event, window, cx);
            }
            "copy-rows" => {
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| g.copy_selection(cx));
                }
            }
            "copy-cells" => {
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| g.copy_cells(cx));
                }
            }
            "copy-column" => self.copy_column(k, col, cx),
            "revert" => {
                if let Some(t) = self
                    .state
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == k.0)
                    .and_then(|t| t.table.as_mut())
                {
                    t.data.apply(|rows| {
                        if rows.get(row).is_some_and(|r| r.orig.is_none()) {
                            rows.remove(row);
                        } else if let Some(r) = rows.get_mut(row)
                            && let Some(orig) = &r.orig
                        {
                            r.cur = orig.iter().cloned().map(Value::from).collect();
                            r.deleted = false;
                        }
                    });
                }
            }
            _ => {}
        }
        cx.notify();
    }
    /// Quick Filter: shows the filter bar with just this rule and applies it.
    fn quick_filter(
        &mut self,
        k: GridKey,
        row: usize,
        col: usize,
        index: usize,
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
        let name = t.data.result.columns.get(col).cloned().unwrap_or_default();
        let value = t
            .data
            .rows
            .get(row)
            .and_then(|r| r.orig.as_ref())
            .and_then(|o| o.get(col).cloned())
            .flatten();
        let Some(quick) = quick_filters(&name, value.as_deref())
            .into_iter()
            .nth(index)
            .flatten()
            .filter(|q| q.enabled)
        else {
            return;
        };
        self.state.activate(k.0);
        self.navigate_table(TableNavigation::Show(vec![quick.filter], false), window, cx);
    }
    /// The column header menu. A query result's lacks Filter with column, Hide this column and
    /// sorting, which act on the table's query.
    pub(super) fn header_menu(
        &mut self,
        k: GridKey,
        column: usize,
        bounds: telecat_grid::Rect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        let table = tab.table.as_ref().filter(|t| t.part == Part::Data);
        let sorted = table.is_some_and(|t| !t.order_by.is_empty());
        let hidden = table.is_some_and(|t| !t.hidden.is_empty());
        let mut items = vec![
            json!({"id":"h-copy-name","label":"Copy name"}),
            json!({"type":"separator"}),
        ];
        if table.is_some() {
            items.extend([
                json!({"id":"h-filter","label":"Filter with column"}),
                json!({"id":"h-hide","label":"Hide this column"}),
                json!({"type":"separator"}),
            ]);
        }
        items.extend([
            json!({"id":"h-fit","label":"Expand columns to fit the content"}),
            json!({"id":"h-reset","label":"Reset column positions"}),
        ]);
        if table.is_some() {
            items.extend([
                json!({"id":"h-show-all","label":"Show all columns","enabled":hidden}),
                json!({"type":"separator"}),
                json!({"id":"h-asc","label":"Sort Ascending"}),
                json!({"id":"h-desc","label":"Sort Descending"}),
                json!({"label":"Sort multiple Columns","submenu":[
                    {"id":"h-add-asc","label":"Ascending"},
                    {"id":"h-add-desc","label":"Descending"}
                ]}),
                json!({"type":"separator"}),
                json!({"id":"h-remove-sort","label":"Remove sort","enabled":sorted}),
            ]);
        }
        let name = edit_buffer(tab)
            .map_or(&tab.grid.result, |b| &b.result)
            .columns
            .get(column)
            .cloned()
            .unwrap_or_default();
        native_menu::choose(
            json!(items),
            point(px(bounds.x), px(bounds.y + bounds.height)),
            window,
            cx,
            move |this, id, window, cx| this.header_action(k, column, &name, &id, window, cx),
        );
    }
    pub(super) fn header_action(
        &mut self,
        k: GridKey,
        column: usize,
        name: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let grid = self.components.grid(k).map(|g| g.view.clone());
        // The menu was modal: act only if the column is still the one clicked.
        let still = grid.as_ref().is_some_and(|g| {
            g.read(cx)
                .model()
                .columns
                .get(column)
                .is_some_and(|c| c.name == name)
        });
        if !still {
            return;
        }
        let order = |dir| OrderBy {
            column: name.to_owned(),
            dir,
        };
        match id {
            "h-copy-name" => cx.write_to_clipboard(ClipboardItem::new_string(name.to_owned())),
            "h-filter" => self.filter_with_column(k.0, name, window, cx),
            "h-hide" => {
                if let Some(t) = self.table_of(k.0) {
                    t.hidden.insert(name.to_owned());
                }
            }
            "h-show-all" => {
                if let Some(t) = self.table_of(k.0) {
                    t.hidden.clear();
                }
            }
            "h-fit" => {
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| {
                        g.update_model(
                            |m| {
                                m.fit_columns();
                            },
                            cx,
                        )
                    });
                }
                self.remember_layout(k, cx);
            }
            "h-reset" => {
                if let Some(t) = self.table_of(k.0) {
                    t.column_order.clear();
                }
                if let Some(grid) = grid {
                    grid.update(cx, |g, cx| g.update_model(|m| m.set_order(vec![]), cx));
                }
                self.remember_layout(k, cx);
            }
            "h-asc" | "h-desc" | "h-add-asc" | "h-add-desc" | "h-remove-sort" => {
                let Some(t) = self.table_of(k.0) else {
                    return;
                };
                let dir = if id.ends_with("asc") {
                    Direction::Asc
                } else {
                    Direction::Desc
                };
                let mut sort = t.order_by.clone();
                match id {
                    "h-remove-sort" => sort.clear(),
                    "h-asc" | "h-desc" => sort = vec![order(dir)],
                    _ => {
                        sort.retain(|o| o.column != name);
                        sort.push(order(dir));
                    }
                }
                self.state.activate(k.0);
                self.navigate_table(TableNavigation::Sort(sort), window, cx);
                return;
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn table_of(&mut self, tab_id: u64) -> Option<&mut TableEditor> {
        self.state
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.table.as_mut())
    }
    /// Filter with column: the filter bar with a rule on `column`, its value field focused.
    pub(super) fn filter_with_column(
        &mut self,
        tab_id: u64,
        column: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.activate(tab_id);
        let Some(t) = self.table_of(tab_id) else {
            return;
        };
        t.show_filters = true;
        let at = match t
            .filters
            .iter()
            .position(|f| f.value.is_empty() && f.column.is_empty())
        {
            Some(i) => i,
            None => {
                t.filters.push(telecat_app::table_model::new_filter());
                t.filters.len() - 1
            }
        };
        t.filters[at].column = column.to_owned();
        self.table_inputs.remove(&tab_id);
        self.prepare_table_inputs(window, cx);
        if let Some((input, _)) = self
            .table_inputs
            .get(&tab_id)
            .and_then(|i| i.filter_input(at))
        {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        self.queue_table_state(tab_id, cx);
        cx.notify();
    }
    /// An enum, SET or boolean cell's dropdown (its chevron in the Data grid).
    pub(super) fn value_dropdown(
        &mut self,
        k: GridKey,
        cell: Cell,
        bounds: telecat_grid::Rect,
        key: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.locate_grid_cell(k, cell, key.as_deref(), cx) else {
            return;
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        if let Some(reason) = cell_refusal(tab, row, col) {
            self.show_hint(reason, cx);
            return;
        }
        let Some(detail) = column_detail(tab, col).cloned() else {
            return;
        };
        let current = edit_buffer(tab)
            .and_then(|b| b.rows.get(row))
            .and_then(|r| r.cur.get(col))
            .and_then(|v| v.value().map(str::to_owned));
        let mut items: Vec<serde_json::Value> = match detail.kind {
            ValueKind::Boolean => boolean_literals(self.state.connection.driver)
                .iter()
                .map(|(label, literal)| {
                    let on = current.as_deref().is_some_and(|c| {
                        c.eq_ignore_ascii_case(literal)
                            || c.eq_ignore_ascii_case(label)
                            || (*label == "true" && c == "t")
                            || (*label == "false" && c == "f")
                    });
                    json!({"id":format!("put:{literal}"),"label":label,"checked":on})
                })
                .collect(),
            ValueKind::Enum => detail
                .values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    json!({"id":format!("enum:{i}"),"label":if v.is_empty() {"EMPTY"} else {v},"checked":current.as_deref() == Some(v)})
                })
                .collect(),
            ValueKind::Set => detail
                .values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let on = current.as_deref().is_some_and(|c| c.split(',').any(|m| m == v));
                    json!({"id":format!("set:{i}"),"label":v,"checked":on})
                })
                .collect(),
            _ => return,
        };
        if detail.nullable {
            items.push(json!({"type":"separator"}));
            items.push(json!({"id":"put-null","label":"NULL","checked":current.is_none()}));
        }
        native_menu::choose(
            json!(items),
            point(px(bounds.x), px(bounds.y + bounds.height)),
            window,
            cx,
            move |this, id, _, cx| {
                let Some((row, col)) = this.locate_grid_cell(k, cell, key.as_deref(), cx) else {
                    return;
                };
                let value = if id == "put-null" {
                    Value::Null
                } else if let Some(literal) = id.strip_prefix("put:") {
                    Value::Text(literal.to_owned())
                } else if let Some(v) = id
                    .strip_prefix("enum:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| detail.values.get(i))
                {
                    Value::Text(v.clone())
                } else if let Some(v) = id
                    .strip_prefix("set:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| detail.values.get(i))
                {
                    Value::Text(toggle_set_member(current.as_deref(), v, &detail.values))
                } else {
                    return;
                };
                if let Some(tab) = this.state.tabs.iter_mut().find(|t| t.id == k.0)
                    && let Err(reason) = stage_cells(tab, vec![(row, col, value)])
                {
                    this.show_hint(reason, cx);
                }
                cx.notify();
            },
        );
    }
    /// The arrow of a foreign-key cell: opens the referenced table filtered to the referenced
    /// row, in its open tab or a new one.
    pub(super) fn follow_foreign_key(
        &mut self,
        k: GridKey,
        cell: Cell,
        key: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.locate_grid_cell(k, cell, key.as_deref(), cx) else {
            return;
        };
        let Some(t) = self
            .state
            .tabs
            .iter()
            .find(|t| t.id == k.0)
            .and_then(|t| t.table.as_ref())
        else {
            return;
        };
        let name = t.data.result.columns.get(col).cloned().unwrap_or_default();
        let Some(fk) = t
            .columns
            .as_ref()
            .and_then(|c| c.foreign_key(&name))
            .cloned()
        else {
            return;
        };
        let values: Option<Vec<Option<String>>> = fk
            .columns
            .iter()
            .map(|c| {
                let i = t.data.result.columns.iter().position(|x| x == c)?;
                let v = t.data.rows.get(row)?.cur.get(i)?;
                Some(v.value().map(str::to_owned))
            })
            .collect();
        let Some(filters) = values.and_then(|v| foreign_key_filters(&fk, &v)) else {
            self.show_hint("This row references nothing: a key column is NULL.", cx);
            return;
        };
        self.state.remember_current_view();
        let open = self
            .state
            .tabs
            .iter()
            .find(|t| t.kind == TabKind::Table(fk.target.clone()))
            .map(|t| t.id);
        match open {
            Some(id) => {
                // The same tab (a key to its own table) gets a step of its own; another open
                // tab is a step already.
                let same = id == self.state.active;
                self.state.activate(id);
                self.navigate_table(TableNavigation::Show(filters, same), window, cx);
                if !same {
                    self.state.remember_current_view();
                }
            }
            None => {
                let id = self.state.open_table(fk.target.clone());
                self.restore_table_state(id);
                if let Some(t) = self.table_of(id) {
                    t.filters = filters;
                    t.show_filters = true;
                    t.apply_filters(Combine::And, None);
                }
                // Opening the tab was the step; it is recorded with the filtered view.
                self.state.remember_current_view();
                self.refresh_tab(cx);
            }
        }
        cx.notify();
    }
    /// ⌘← / ⌘→: the previous or next page of the current table.
    pub(super) fn page_step(&mut self, delta: i8, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.state.current();
        if tab.table.as_ref().is_none_or(|t| t.part != Part::Data) {
            return;
        }
        let (offset, limit) = (tab.offset, tab.limit);
        if delta < 0 {
            if offset > 0 {
                self.navigate_table(
                    TableNavigation::Page(offset.saturating_sub(limit), limit),
                    window,
                    cx,
                );
            }
        } else if has_next_page(tab) {
            self.navigate_table(TableNavigation::Page(offset + limit, limit), window, cx);
        }
    }
    /// A grid cell's column header sort: TablePlus's click cycle replaces every sorted column.
    pub(super) fn grid_sort(
        &mut self,
        sort: Option<telecat_grid::Sort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sort: Vec<OrderBy> = sort
            .map(|s| OrderBy {
                column: s.column,
                dir: if s.direction == GridDirection::Asc {
                    Direction::Asc
                } else {
                    Direction::Desc
                },
            })
            .into_iter()
            .collect();
        self.navigate_table(TableNavigation::Sort(sort), window, cx);
    }
}
/// Whether a table tab has a page after the current one. With an estimated total the page is
/// only known to continue when it came back full.
pub(super) fn has_next_page(tab: &Tab) -> bool {
    let shown = tab.table.as_ref().map_or(0, |t| {
        t.data.rows.iter().filter(|r| r.orig.is_some()).count() as u64
    });
    if tab.estimated {
        shown >= tab.limit
    } else {
        tab.offset + tab.limit < tab.total
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn set_value_ids_round_trip() {
        use super::SetValue;
        for (id, v) in [
            ("sv-empty", SetValue::Empty),
            ("sv-null", SetValue::Null),
            ("sv-default", SetValue::Default),
            ("sv-now", SetValue::Now),
            ("sv-pretty", SetValue::Pretty),
            ("sv-minify", SetValue::Minify),
        ] {
            assert_eq!(SetValue::from_id(id), Some(v));
        }
        assert_eq!(SetValue::from_id("sv-date"), None);
    }
}
