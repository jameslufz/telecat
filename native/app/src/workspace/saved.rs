//! What table tabs remember across launches (filter rules, column widths and order), Go Back /
//! Go Forward, Reload Current Tab and the Data / Structure switch.
use super::components::GridKey;
use super::*;
use telecat_app::table_model::Part;
use telecat_core::table_state::{SavedTable, column_order, table_key};

/// Writes wait for edits to pause, so dragging a column edge or typing a filter value doesn't
/// write the file on every change.
const SAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(400);

impl Workspace {
    fn saved_key(&self, table: &TableRef) -> Option<String> {
        table_key(&self.state.connection.id, &self.state.database, table)
    }
    /// After a table tab opens: its remembered filter rules (shown, not applied, so nothing is
    /// read differently until the user applies them) and column layout.
    pub(super) fn restore_table_state(&mut self, tab_id: u64) {
        let Some(table) = self
            .state
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.table.as_ref())
            .map(|t| t.table.clone())
        else {
            return;
        };
        let Some(saved) = self
            .saved_key(&table)
            .and_then(|key| self.services.profile.saved_table(&key))
        else {
            return;
        };
        if let Some(t) = self.table_of(tab_id) {
            if !saved.filters.is_empty() {
                t.filters = saved.filters.clone();
            }
            t.combine = saved.combine;
            t.show_filters = saved.show_filters;
            // Checked against the columns once the rows are read (see `sanitize_layout`).
            t.column_widths = saved.widths;
            t.column_order = saved.order;
        }
    }
    /// Once a table's columns are known: drops remembered layout for columns it no longer has.
    pub(super) fn sanitize_layout(&mut self, tab_id: u64) {
        let Some(t) = self.table_of(tab_id) else {
            return;
        };
        let columns = t.data.result.columns.clone();
        let saved = SavedTable {
            widths: std::mem::take(&mut t.column_widths),
            order: std::mem::take(&mut t.column_order),
            ..Default::default()
        }
        .sanitized(&columns);
        t.column_widths = saved.widths;
        t.column_order = saved.order;
    }
    /// Remembers the table tab's filters and layout, once edits pause.
    pub(super) fn queue_table_state(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        self.table_state_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |this, _| this.save_table_state(tab_id));
        }));
    }
    fn save_table_state(&mut self, tab_id: u64) {
        let Some(t) = self
            .state
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.table.as_ref())
        else {
            return;
        };
        if t.is_new {
            return;
        }
        let Some(key) = self.saved_key(&t.table) else {
            return;
        };
        let saved = SavedTable {
            filters: t
                .filters
                .iter()
                .filter(|f| !(f.value.is_empty() && f.column.is_empty() && f.op == FilterOp::Eq))
                .cloned()
                .collect(),
            combine: t.combine,
            show_filters: t.show_filters,
            widths: t.column_widths.clone(),
            order: t.column_order.clone(),
            used: 0,
        };
        if let Err(e) = self.services.profile.save_table(&key, saved) {
            log::warn!("Remember table layout: {e:#}");
        }
    }
    /// After a column was resized, moved or fitted: the grid's widths and order become the
    /// table's, and are remembered.
    pub(super) fn remember_layout(&mut self, k: GridKey, cx: &mut Context<Self>) {
        let Some(grid) = self.components.grid(k).map(|g| g.view.clone()) else {
            return;
        };
        let model = grid.read(cx).model();
        let names: Vec<String> = model.columns.iter().map(|c| c.name.clone()).collect();
        let widths: Vec<f32> = model.widths.clone();
        let order: Vec<String> = model
            .display_columns()
            .into_iter()
            .map(|i| names[i].clone())
            .collect();
        let Some(t) = self.table_of(k.0) else {
            return;
        };
        if t.part != Part::Data || t.data.result.columns != names {
            return;
        }
        t.column_widths = names.iter().cloned().zip(widths).collect();
        t.column_order = column_order(&order, &names);
        self.queue_table_state(k.0, cx);
    }
    /// ⌥⌘R: reads the current tab's rows (or structure) again; a query tab has nothing to do.
    pub(super) fn reload_current_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.state.current();
        match (&tab.kind, &tab.table) {
            (TabKind::Table(_), Some(t)) if t.part == Part::Data => {
                // Through the discard prompt: reloading replaces staged rows.
                let (offset, limit) = (tab.offset, tab.limit);
                self.navigate_table(TableNavigation::Page(offset, limit), window, cx);
            }
            (TabKind::Table(_), Some(t)) => {
                if t.buffer().dirty() {
                    self.show_hint(
                        "Save (⌘S) or discard (Esc) the changes before reloading.",
                        cx,
                    );
                    return;
                }
                let id = self.state.active;
                if let Some(t) = self.table_of(id) {
                    t.buffer_mut().loaded = false;
                }
                self.load_metadata(cx);
            }
            (TabKind::Running, _) => self.refresh_tab(cx),
            _ => {}
        }
        cx.notify();
    }
    /// ⌃⌘[ / ⌃⌘]: the Data or Structure view of the current table.
    pub(super) fn show_part(&mut self, part: Part, cx: &mut Context<Self>) {
        let Some(t) = &self.state.current().table else {
            return;
        };
        if t.is_new && part == Part::Data {
            return;
        }
        if t.part != part {
            self.switch_part(part, cx);
        }
    }
    /// Go Back / Go Forward (⌃⌘← / ⌃⌘→): the previous or next tab, showing the filters, sort
    /// and page it had.
    pub(super) fn go(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.flush_grid_edits(window, cx);
        if let Some(view) = self.state.navigate(forward) {
            self.reset_component_focus();
            self.navigate_table(TableNavigation::View(view), window, cx);
        }
        cx.notify();
    }
}
