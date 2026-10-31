//! The row inspector: the selected table rows' values as editable fields in the right panel.
//! With several rows selected a field edits that column of all of them. Edits are staged
//! exactly like grid edits, as one undo step each.
use super::data::{SetValue, stage_cells};
use super::*;
use gpui_kit::base::input::{self, Textarea, TextareaState};
use telecat_app::table_model::Part;
use telecat_core::changes::{Cell as Value, EditRow, cell_modified};
use telecat_core::columns::ValueKind;

pub(super) struct Inspector {
    tab: u64,
    /// The inspected rows' identities, top to bottom.
    rows: Vec<String>,
    columns: Vec<String>,
    /// Which values were too large to edit here when the fields were built.
    large: Vec<bool>,
    fields: Vec<Option<Field>>,
    _subscriptions: Vec<Subscription>,
}
/// Values longer than this (in bytes) are shown, not edited, in the inspector: a wrapping text
/// field over megabytes of text would make every selection and repaint slow. The grid's cell
/// editor still edits them.
pub(super) const EDIT_LIMIT: usize = 64 * 1024;
/// How much of a value a read-only field shows.
const PREVIEW_CHARS: usize = 2000;
fn too_large(value: &Value) -> bool {
    value.value().is_some_and(|s| s.len() > EDIT_LIMIT)
}
/// The start of `text` for display only; never staged.
pub(super) fn preview(text: &str) -> std::borrow::Cow<'_, str> {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((end, _)) => format!("{}… ({} KB)", &text[..end], text.len() / 1024).into(),
        None => text.into(),
    }
}
/// An editable field and the value it last showed (`None`: the rows differ), so outside
/// changes (grid edits, undo) reach it without clobbering what the user is typing.
struct Field {
    input: Entity<TextareaState>,
    shown: Option<Value>,
}
/// How a field looks next to the grid's colour coding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldState {
    Original,
    Modified,
    New,
    Deleted,
}
impl FieldState {
    pub(super) fn of(row: &EditRow, col: usize) -> Self {
        if row.deleted {
            Self::Deleted
        } else if row.orig.is_none() {
            Self::New
        } else if cell_modified(row, col) {
            Self::Modified
        } else {
            Self::Original
        }
    }
    fn background(self) -> u32 {
        match self {
            Self::Original => telecat_theme::INPUT,
            Self::Modified => telecat_theme::MOD_CELL,
            Self::New => telecat_theme::NEW,
            Self::Deleted => telecat_theme::DELETED,
        }
    }
}
/// The text a field shows for a value, and the placeholder that tells NULL, DEFAULT and an
/// empty string apart.
pub(super) fn field_text(value: &Value) -> (&str, &'static str) {
    match value {
        Value::Text(s) => (s, "EMPTY"),
        Value::Null => ("", "NULL"),
        Value::Unset { .. } => ("", "DEFAULT"),
    }
}
/// What a field shows for several rows: their value when they all hold the same, else nothing
/// with the placeholder "Multiple values".
fn shared_text(value: Option<&Value>) -> (&str, &'static str) {
    value.map_or(("", "Multiple values"), field_text)
}
/// The staged value for `text` typed over `current`, or `None` when nothing changed. Like the
/// grid's editor, leaving a NULL or DEFAULT field empty keeps it as it was.
pub(super) fn edited_value(current: &Value, text: &str) -> Option<Value> {
    let unchanged = match current {
        Value::Text(s) => s == text,
        Value::Null | Value::Unset { .. } => text.is_empty(),
    };
    (!unchanged).then(|| Value::Text(text.to_owned()))
}
/// The value every one of `rows` holds in column `col`, or `None` when they differ.
fn shared_value(rows: &[&EditRow], col: usize) -> Option<Value> {
    let first = rows.first()?.cur.get(col)?.clone();
    rows.iter()
        .all(|r| r.cur.get(col) == Some(&first))
        .then_some(first)
}
/// Where each of `keys` is among `rows`, in one pass; keys whose row is gone are left out.
fn positions(rows: &[EditRow], keys: &[String]) -> Vec<usize> {
    let at: std::collections::HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.key.as_str(), i))
        .collect();
    keys.iter()
        .filter_map(|k| at.get(k.as_str()).copied())
        .collect()
}
/// The rows the inspector edits: a table's Data rows, or a query result's once mapped to tables.
fn edit_rows(tab: &Tab) -> Option<&telecat_app::table_model::EditBuffer> {
    match (&tab.table, &tab.result_edit) {
        (Some(t), _) => (t.part == Part::Data).then_some(&t.data),
        (None, Some(r)) => Some(&r.data),
        (None, None) => None,
    }
}
fn column_editable(tab: &Tab, col: usize) -> bool {
    match (&tab.table, &tab.result_edit) {
        (Some(t), _) => t.part == Part::Data && t.data_column_editable(col),
        (None, Some(r)) => r.plan.editable.get(col).copied().unwrap_or(false),
        (None, None) => false,
    }
}
impl Workspace {
    /// The inspected rows of the current tab, top to bottom: the grid's selected rows.
    fn inspected_rows(&self, cx: &App) -> Vec<usize> {
        let tab = self.state.current();
        let Some(rows) = edit_rows(tab) else {
            return vec![];
        };
        let picked = super::components::grid_key(tab)
            .and_then(|k| self.selected_rows(k, cx))
            .unwrap_or_default();
        if !picked.is_empty() {
            return picked;
        }
        tab.grid
            .selected_row
            .filter(|r| *r < rows.rows.len())
            .into_iter()
            .collect()
    }
    /// Keeps the inspector's inputs bound to the selected rows. Inputs are created only when the
    /// selection changes and never take focus by themselves.
    pub(super) fn prepare_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let real = self.inspected_rows(cx);
        let tab = self.state.current();
        let keys: Vec<String> = edit_rows(tab).map_or_else(Vec::new, |b| {
            real.iter()
                .filter_map(|&r| b.rows.get(r).map(|x| x.key.clone()))
                .collect()
        });
        let large_now = |state: &WorkspaceState| -> Vec<bool> {
            let Some(b) = edit_rows(state.current()) else {
                return vec![];
            };
            let rows: Vec<&EditRow> = real.iter().filter_map(|&r| b.rows.get(r)).collect();
            (0..b.result.columns.len())
                .map(|c| rows.iter().any(|r| r.cur.get(c).is_some_and(too_large)))
                .collect()
        };
        let same = |i: &Inspector, state: &WorkspaceState| {
            !keys.is_empty()
                && i.tab == state.active
                && i.rows == keys
                && large_now(state) == i.large
                && i.columns
                    == edit_rows(state.current()).map_or(&vec![], |r| &r.result.columns)[..]
        };
        if let Some(old) = self.inspector.take_if(|i| !same(i, &self.state)) {
            self.stage_unsaved_fields(old, cx);
        }
        if keys.is_empty() {
            return;
        }
        let tab = self.state.current();
        let buffer = edit_rows(tab).expect("keys come from the rows");
        let rows: Vec<&EditRow> = real.iter().filter_map(|&r| buffer.rows.get(r)).collect();
        let columns = &buffer.result.columns;
        if self.inspector.is_none() {
            let tab_id = tab.id;
            let large = large_now(&self.state);
            let editable: Vec<bool> = (0..columns.len())
                .map(|c| column_editable(tab, c) && !large[c])
                .collect();
            let values: Vec<Option<Value>> =
                (0..columns.len()).map(|c| shared_value(&rows, c)).collect();
            let columns = columns.clone();
            let mut subscriptions = vec![];
            let fields = values
                .into_iter()
                .enumerate()
                .map(|(col, value)| {
                    editable[col].then(|| {
                        let input = field_input(value.as_ref(), window, cx);
                        let keys = keys.clone();
                        // The field remembers its own rows: a blur can arrive after the
                        // selection has already moved to other rows.
                        subscriptions.push(cx.subscribe_in(
                            &input,
                            window,
                            move |this, input, event: &InputEvent, _, cx| {
                                if matches!(event, InputEvent::Blur) {
                                    let text = input.read(cx).value().to_string();
                                    this.stage_inspector_text(tab_id, &keys, col, text, cx);
                                }
                            },
                        ));
                        Field {
                            input,
                            shown: value,
                        }
                    })
                })
                .collect();
            self.inspector = Some(Inspector {
                tab: tab_id,
                rows: keys,
                columns,
                large,
                fields,
                _subscriptions: subscriptions,
            });
            return;
        }
        let current: Vec<Option<Value>> =
            (0..columns.len()).map(|c| shared_value(&rows, c)).collect();
        let saving = self.saving;
        let Some(inspector) = self.inspector.as_mut() else {
            return;
        };
        for (field, value) in inspector.fields.iter_mut().zip(current) {
            let Some(field) = field else { continue };
            let focused = field.input.focus_handle(cx).is_focused(window);
            field.input.update(cx, |s, cx| {
                if s.is_editable() == saving {
                    s.set_readonly(saving, cx);
                }
                if field.shown != value && !focused {
                    let (text, placeholder) = shared_text(value.as_ref());
                    s.set_value(text.to_owned(), window, cx);
                    s.set_placeholder(placeholder, window, cx);
                    field.shown = value;
                }
            });
        }
    }
    /// A field's subscription goes with its inspector, and gpui reports a blur only after the
    /// next render has replaced it. So text typed but not yet staged is staged here.
    fn stage_unsaved_fields(&mut self, old: Inspector, cx: &mut Context<Self>) {
        for (col, field) in old.fields.iter().enumerate() {
            let Some(field) = field else { continue };
            let text = field.input.read(cx).value().to_string();
            if text != shared_text(field.shown.as_ref()).0 {
                self.stage_inspector_text(old.tab, &old.rows, col, text, cx);
            }
        }
    }
    /// Stages `text` typed into column `col` of the rows `keys`, the way a grid edit is staged,
    /// as one undo step. An empty field over differing values changes nothing.
    pub(super) fn stage_inspector_text(
        &mut self,
        tab_id: u64,
        keys: &[String],
        col: usize,
        text: String,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let Some(tab) = self
            .state
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .filter(|t| column_editable(t, col))
        else {
            return;
        };
        let Some(rows) = edit_rows(tab) else {
            return;
        };
        let at = positions(&rows.rows, keys);
        let found: Vec<&EditRow> = at.iter().map(|&i| &rows.rows[i]).collect();
        if found.len() > 1 && text.is_empty() && shared_value(&found, col).is_none() {
            return;
        }
        let cells: Vec<(usize, usize, Value)> = at
            .iter()
            .filter_map(|&real| {
                let value = edited_value(rows.rows[real].cur.get(col)?, &text)?;
                Some((real, col, value))
            })
            .collect();
        if cells.is_empty() {
            return;
        }
        if let Some(field) = self
            .inspector
            .as_mut()
            .filter(|i| i.tab == tab_id && i.rows == keys)
            .and_then(|i| i.fields.get_mut(col))
            .and_then(Option::as_mut)
        {
            field.shown = Some(Value::Text(text.clone()));
        }
        if let Err(reason) = stage_cells(tab, cells) {
            self.show_hint(reason, cx);
        }
        cx.notify();
    }
    /// The field's ⌄ menu: NULL, DEFAULT, EMPTY, then PRETTY and MINIFY, for every inspected row.
    fn field_menu(
        &mut self,
        col: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inspector) = &self.inspector else {
            return;
        };
        let tab_id = inspector.tab;
        let keys = inspector.rows.clone();
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(buffer) = edit_rows(tab) else {
            return;
        };
        let rows: Vec<&EditRow> = positions(&buffer.rows, &keys)
            .into_iter()
            .map(|i| &buffer.rows[i])
            .collect();
        let all_new = !rows.is_empty() && rows.iter().all(|r| r.orig.is_none());
        let json_kind = tab
            .table
            .as_ref()
            .and_then(|t| t.columns.as_ref()?.get(buffer.result.columns.get(col)?))
            .is_some_and(|c| c.kind == ValueKind::Json);
        let json = json_kind
            || rows.iter().any(|r| {
                r.cur.get(col).and_then(|v| v.value()).is_some_and(|t| {
                    serde_json::from_str::<serde_json::Value>(t).is_ok()
                        && t.trim_start().starts_with(['{', '['])
                })
            });
        let items = json!([
            {"id":"sv-null","label":"NULL"},
            {"id":"sv-default","label":"DEFAULT","enabled":all_new && tab.table.is_some()},
            {"id":"sv-empty","label":"EMPTY"},
            {"type":"separator"},
            {"id":"sv-pretty","label":"PRETTY","enabled":json},
            {"id":"sv-minify","label":"MINIFY","enabled":json},
        ]);
        native_menu::choose(items, at, window, cx, move |this, id, _, cx| {
            let Some(value) = SetValue::from_id(&id) else {
                return;
            };
            let Some(tab) = this.state.tabs.iter().find(|t| t.id == tab_id) else {
                return;
            };
            let Some(buffer) = edit_rows(tab) else {
                return;
            };
            let cells: Vec<(usize, usize)> = positions(&buffer.rows, &keys)
                .into_iter()
                .map(|r| (r, col))
                .collect();
            this.set_cells_value(tab_id, cells, value, cx);
        });
    }
    /// Drops the inspector's fields without staging what they hold, after the rows were replaced
    /// or deliberately thrown away. They are rebuilt from the rows on the next render.
    pub(super) fn reset_inspector(&mut self) {
        self.inspector = None;
    }
    /// Stages a focused field before a commit snapshot or anything else that reads the buffer.
    pub(super) fn flush_inspector(&mut self, window: &Window, cx: &mut Context<Self>) {
        let focused = self.inspector.as_ref().and_then(|i| {
            i.fields.iter().position(|f| {
                f.as_ref()
                    .is_some_and(|f| f.input.focus_handle(cx).is_focused(window))
            })
        });
        if let (Some(col), Some(i)) = (focused, &self.inspector) {
            let (tab, keys) = (i.tab, i.rows.clone());
            let text = i.fields[col]
                .as_ref()
                .map(|f| f.input.read(cx).value().to_string())
                .unwrap_or_default();
            self.stage_inspector_text(tab, &keys, col, text, cx);
        }
    }
    /// Escape in a field discards what was typed since the field last staged.
    fn revert_focused_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.inspector.as_ref().and_then(|i| {
            i.fields
                .iter()
                .flatten()
                .find(|f| f.input.focus_handle(cx).is_focused(window))
        }) else {
            return false;
        };
        let text = shared_text(field.shown.as_ref()).0.to_owned();
        field
            .input
            .update(cx, |s, cx| s.set_value(text, window, cx));
        true
    }
    pub(super) fn inspector_focused(&self, window: &Window, cx: &App) -> bool {
        self.inspector.as_ref().is_some_and(|i| {
            i.fields
                .iter()
                .flatten()
                .any(|f| f.input.focus_handle(cx).contains_focused(window, cx))
        })
    }
    /// The inspector's input for column `col` of the inspected rows, if that field is editable.
    pub(super) fn inspector_input(&self, col: usize) -> Option<Entity<TextareaState>> {
        self.inspector
            .as_ref()
            .and_then(|i| i.fields.get(col))
            .and_then(Option::as_ref)
            .map(|f| f.input.clone())
    }
    /// Sets field `col` of the inspected rows to NULL, like the field menu's NULL.
    #[cfg(test)]
    pub(super) fn set_inspector_null(&mut self, col: usize, cx: &mut Context<Self>) {
        let Some(i) = &self.inspector else {
            return;
        };
        if i.fields.get(col).is_none_or(Option::is_none) {
            return;
        }
        let (tab_id, keys) = (i.tab, i.rows.clone());
        let Some(buffer) = self
            .state
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(edit_rows)
        else {
            return;
        };
        let cells: Vec<(usize, usize)> = positions(&buffer.rows, &keys)
            .into_iter()
            .map(|r| (r, col))
            .collect();
        self.set_cells_value(tab_id, cells, SetValue::Null, cx);
    }
    /// How many rows the inspector edits at once.
    #[cfg(test)]
    pub(super) fn inspected_count(&self) -> usize {
        self.inspector.as_ref().map_or(0, |i| i.rows.len())
    }
    /// The inspected rows' fields for the right panel, editable where the grid would allow it.
    /// `query` matches field names fuzzily (TablePlus's default) or as plain text.
    pub(super) fn inspector_fields(&self, query: &str, cx: &mut Context<Self>) -> Option<Div> {
        let inspector = self
            .inspector
            .as_ref()
            .filter(|i| i.tab == self.state.active)?;
        let tab = self.state.current();
        let buffer = edit_rows(tab)?;
        let rows: Vec<&EditRow> = positions(&buffer.rows, &inspector.rows)
            .into_iter()
            .map(|i| &buffer.rows[i])
            .collect();
        if rows.is_empty() {
            return None;
        }
        let columns = &buffer.result.columns;
        let saving = self.saving;
        let fuzzy = self.field_fuzzy;
        let matches = |label: &str| {
            query.is_empty()
                || if fuzzy {
                    fuzzy_match(query, label).is_some()
                } else {
                    label.to_lowercase().contains(query)
                }
        };
        Some(
            div()
                // Enter stages the field; Shift+Enter inserts a line break and Escape restores
                // the field's value.
                .capture_action(cx.listener(|this, action: &input::Enter, window, cx| {
                    if !action.shift && this.inspector_focused(window, cx) {
                        this.flush_inspector(window, cx);
                        // Like the grid's editor, Enter hands focus back to the grid, so ⌘Z
                        // then undoes the staged edit.
                        this.focus_current_grid(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                    if this.revert_focused_field(window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .when(rows.len() > 1, |d| {
                    d.child(
                        div()
                            .mb(px(8.))
                            .text_size(px(11.))
                            .text_color(rgb(telecat_theme::MUTED))
                            .child(format!("{} rows selected: a field edits all of them", rows.len())),
                    )
                })
                .children(
                    columns
                        .iter()
                        .enumerate()
                        .filter(|(_, label)| matches(label))
                        .map(|(col, label)| {
                            let value = shared_value(&rows, col);
                            let state = if rows.len() == 1 {
                                FieldState::of(rows[0], col)
                            } else if rows.iter().any(|r| r.deleted) {
                                FieldState::Deleted
                            } else if rows.iter().any(|r| cell_modified(r, col)) {
                                FieldState::Modified
                            } else {
                                FieldState::Original
                            };
                            let input = self.inspector_input(col);
                            let can_menu = input.is_some() && !saving;
                            div()
                                .mb(px(8.))
                                .child(
                                    div()
                                        .mb(px(3.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .font(mono())
                                                .text_size(px(11.5))
                                                .line_height(px(13.5))
                                                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                                .truncate()
                                                .child(label.clone()),
                                        )
                                        .when(can_menu, |d| {
                                            d.child(
                                                div()
                                                    .id(("inspector-menu", col))
                                                    .flex_shrink_0()
                                                    .size(px(16.))
                                                    .rounded(px(4.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .hover(|d| d.bg(rgb(telecat_theme::INPUT_2)))
                                                    .cursor_pointer()
                                                    .tooltip(text_tooltip("Set NULL, DEFAULT, EMPTY, or format JSON"))
                                                    .child(icon("ChevronDown.svg", 11., telecat_theme::MUTED))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                                            this.field_menu(col, e.position, window, cx)
                                                        }),
                                                    ),
                                            )
                                        }),
                                )
                                .child(match input {
                                    Some(input) => editable_field(&input, state),
                                    None => read_only_field(
                                        value.as_ref().map_or(("", "Multiple values"), field_text),
                                        state,
                                    ),
                                })
                        }),
                ),
        )
    }
}
fn field_input(value: Option<&Value>, window: &mut Window, cx: &mut App) -> Entity<TextareaState> {
    let (text, placeholder) = shared_text(value);
    let text = text.to_owned();
    cx.new(|cx| {
        let mut s = TextareaState::new(window, cx)
            .auto_grow(1, 6)
            .soft_wrap(true)
            .placeholder(placeholder)
            .default_value(text);
        s.set_editor_style(InputEditorStyle {
            foreground: rgb(telecat_theme::TEXT).into(),
            muted_foreground: rgb(0x666666).into(),
            background: gpui_kit::transparent_black(),
            border: rgb(telecat_theme::BORDER).into(),
            selection: rgba(0x2f6fed66).into(),
            caret: rgb(telecat_theme::TEXT).into(),
            ..Default::default()
        });
        s
    })
}
fn field_box(state: FieldState) -> Div {
    div()
        .w_full()
        .px(px(8.))
        .py(px(5.))
        .border_1()
        .border_color(rgb(telecat_theme::BORDER))
        .rounded(px(6.))
        .bg(rgb(state.background()))
        .font(mono())
        .text_size(px(12.))
        .line_height(px(16.))
}
fn editable_field(input: &Entity<TextareaState>, state: FieldState) -> Div {
    field_box(state).min_h(px(30.)).child(Textarea::new(input))
}
fn read_only_field((text, placeholder): (&str, &'static str), state: FieldState) -> Div {
    let muted = text.is_empty();
    field_box(state)
        .h(px(30.))
        .overflow_hidden()
        .text_color(rgb(if muted { 0x666666 } else { 0xe8e8e8 }))
        .child(if muted {
            placeholder.to_owned()
        } else {
            preview(text).into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn typed_text_is_staged_only_when_it_changes_the_value() {
        let text = |s: &str| Value::Text(s.into());
        assert_eq!(edited_value(&text("a"), "a"), None);
        assert_eq!(edited_value(&text("a"), "b"), Some(text("b")));
        assert_eq!(edited_value(&text("a"), ""), Some(text("")));
        assert_eq!(edited_value(&text(""), ""), None);
        assert_eq!(
            edited_value(&Value::Null, ""),
            None,
            "an empty NULL field stays NULL"
        );
        assert_eq!(
            edited_value(&Value::unset(), ""),
            None,
            "and DEFAULT stays DEFAULT"
        );
        assert_eq!(edited_value(&Value::Null, "x"), Some(text("x")));
        assert_eq!(
            edited_value(&text("ธนาคาร"), "ธนาคาร\nกรุงเทพ"),
            Some(text("ธนาคาร\nกรุงเทพ"))
        );
    }
    #[::core::prelude::v1::test]
    fn large_values_are_previewed_not_edited() {
        let big = "ก".repeat(EDIT_LIMIT);
        assert!(too_large(&Value::Text(big.clone())));
        assert!(!too_large(&Value::Text("x".repeat(EDIT_LIMIT))));
        assert!(!too_large(&Value::Null));
        let shown = preview(&big);
        assert!(shown.starts_with(&"ก".repeat(PREVIEW_CHARS)));
        assert!(
            shown.ends_with("… (192 KB)"),
            "{}",
            &shown[shown.len() - 20..]
        );
        assert_eq!(preview("short ธนาคาร"), "short ธนาคาร");
    }
    #[::core::prelude::v1::test]
    fn several_rows_share_a_value_only_when_they_all_hold_it() {
        let row = |v: &str| EditRow {
            key: v.into(),
            orig: Some(vec![Some(v.into())]),
            cur: vec![Value::Text(v.into())],
            deleted: false,
            rid: None,
        };
        let (a, b, a2) = (row("a"), row("b"), row("a"));
        assert_eq!(shared_value(&[&a, &a2], 0), Some(Value::Text("a".into())));
        assert_eq!(shared_value(&[&a, &b], 0), None);
        assert_eq!(shared_value(&[], 0), None);
        assert_eq!(shared_text(None), ("", "Multiple values"));
    }
    #[::core::prelude::v1::test]
    fn finding_many_selected_rows_is_one_pass() {
        let rows: Vec<EditRow> = (0..100_000)
            .map(|i| EditRow {
                key: format!("r{i}"),
                orig: None,
                cur: vec![],
                deleted: false,
                rid: None,
            })
            .collect();
        let keys: Vec<String> = (0..100_000)
            .rev()
            .map(|i| format!("r{i}"))
            .chain(["gone".into()])
            .collect();
        let started = std::time::Instant::now();
        let at = positions(&rows, &keys);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(at.len(), 100_000);
        assert_eq!((at[0], at[99_999]), (99_999, 0));
    }
    #[::core::prelude::v1::test]
    fn placeholders_tell_null_default_and_empty_apart() {
        assert_eq!(field_text(&Value::Null), ("", "NULL"));
        assert_eq!(field_text(&Value::unset()), ("", "DEFAULT"));
        assert_eq!(field_text(&Value::Text(String::new())), ("", "EMPTY"));
        assert_eq!(field_text(&Value::Text("x".into())), ("x", "EMPTY"));
    }
    #[::core::prelude::v1::test]
    fn field_colours_follow_the_grid() {
        let row = |orig: Option<Vec<Option<String>>>, cur: Vec<Value>, deleted| EditRow {
            key: "r".into(),
            orig,
            cur,
            deleted,
            rid: None,
        };
        let loaded = row(
            Some(vec![Some("a".into()), Some("b".into())]),
            vec![Value::Text("a".into()), Value::Text("changed".into())],
            false,
        );
        assert_eq!(FieldState::of(&loaded, 0), FieldState::Original);
        assert_eq!(FieldState::of(&loaded, 1), FieldState::Modified);
        assert_eq!(
            FieldState::of(&row(None, vec![Value::unset()], false), 0),
            FieldState::New
        );
        assert_eq!(
            FieldState::of(&row(Some(vec![None]), vec![Value::Null], true), 0),
            FieldState::Deleted
        );
    }
}
