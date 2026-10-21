//! The row inspector: the selected table row's values as editable fields in the right panel.
//! Edits are staged exactly like grid edits, through `components::stage_cell`.
use super::components::{data_column_editable, stage_cell};
use super::*;
use gpui_kit::base::input::{self, Textarea, TextareaState};
use telecat_app::table_model::Part;
use telecat_core::changes::{Cell as Value, EditRow, cell_modified};

pub(super) struct Inspector {
    tab: u64,
    row: String,
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
/// An editable field and the value it last showed, so outside changes (grid edits, undo)
/// reach it without clobbering what the user is typing.
struct Field {
    input: Entity<TextareaState>,
    shown: Value,
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
/// The staged value for `text` typed over `current`, or `None` when nothing changed. Like the
/// grid's editor, leaving a NULL or DEFAULT field empty keeps it as it was.
pub(super) fn edited_value(current: &Value, text: &str) -> Option<Value> {
    let unchanged = match current {
        Value::Text(s) => s == text,
        Value::Null | Value::Unset { .. } => text.is_empty(),
    };
    (!unchanged).then(|| Value::Text(text.to_owned()))
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
        (Some(t), _) => t.part == Part::Data && data_column_editable(&t.data.result, col),
        (None, Some(r)) => r.plan.editable.get(col).copied().unwrap_or(false),
        (None, None) => false,
    }
}
/// Stages a value into row `real` of the inspected rows: the grid's own staging path.
pub(super) fn stage_row_value(
    tab: &mut Tab,
    real: usize,
    col: usize,
    value: Value,
) -> Result<(), String> {
    match (&mut tab.table, &mut tab.result_edit) {
        (Some(t), _) => {
            stage_cell(t, Part::Data, real, col, value);
            Ok(())
        }
        (None, Some(r)) => r.stage(real, col, value),
        (None, None) => Err("this result can't be edited".into()),
    }
}
fn selected_row(tab: &Tab) -> Option<(usize, &EditRow)> {
    let rows = edit_rows(tab)?;
    let real = tab.grid.selected_row?;
    rows.rows.get(real).map(|r| (real, r))
}
impl Workspace {
    /// Keeps the inspector's inputs bound to the selected row. Inputs are created only when the
    /// selected row changes and never take focus by themselves.
    pub(super) fn prepare_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current =
            selected_row(self.state.current()).map(|(_, row)| (self.state.active, row.key.clone()));
        let same = |i: &Inspector, state: &WorkspaceState| {
            current.as_ref().is_some_and(|(tab, key)| {
                i.tab == *tab
                    && i.row == *key
                    && selected_row(state.current()).is_some_and(|(_, r)| {
                        r.cur.iter().map(too_large).eq(i.large.iter().copied())
                    })
                    && i.columns
                        == edit_rows(state.current()).map_or(&vec![], |r| &r.result.columns)[..]
            })
        };
        if let Some(old) = self.inspector.take_if(|i| !same(i, &self.state)) {
            self.stage_unsaved_fields(old, cx);
        }
        let tab = self.state.current();
        let Some((_, row)) = selected_row(tab) else {
            return;
        };
        let columns = &edit_rows(tab)
            .expect("selected_row has rows")
            .result
            .columns;
        if self.inspector.is_none() {
            let (tab_id, key) = (tab.id, row.key.clone());
            let large: Vec<bool> = row.cur.iter().map(too_large).collect();
            let editable: Vec<bool> = (0..columns.len())
                .map(|c| column_editable(tab, c) && !large[c])
                .collect();
            let values = row.cur.clone();
            let columns = columns.clone();
            let mut subscriptions = vec![];
            let fields = values
                .into_iter()
                .enumerate()
                .map(|(col, value)| {
                    editable[col].then(|| {
                        let input = field_input(&value, window, cx);
                        let key = key.clone();
                        // The field remembers its own row: a blur can arrive after the
                        // selection has already moved to another row.
                        subscriptions.push(cx.subscribe_in(
                            &input,
                            window,
                            move |this, input, event: &InputEvent, _, cx| {
                                if matches!(event, InputEvent::Blur) {
                                    let text = input.read(cx).value().to_string();
                                    this.stage_inspector_text(tab_id, &key, col, text, cx);
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
                row: key,
                columns,
                large,
                fields,
                _subscriptions: subscriptions,
            });
            return;
        }
        let current = row.cur.clone();
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
                    let (text, placeholder) = field_text(&value);
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
            if text != field_text(&field.shown).0 {
                self.stage_inspector_text(old.tab, &old.row, col, text, cx);
            }
        }
    }
    /// Stages `text` typed into column `col` of row `key`, the same way a grid edit is staged.
    pub(super) fn stage_inspector_text(
        &mut self,
        tab_id: u64,
        key: &str,
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
        let Some(real) = rows.rows.iter().position(|r| r.key == key) else {
            return;
        };
        let Some(value) = rows.rows[real]
            .cur
            .get(col)
            .and_then(|current| edited_value(current, &text))
        else {
            return;
        };
        if let Some(field) = self
            .inspector
            .as_mut()
            .filter(|i| i.tab == tab_id && i.row == key)
            .and_then(|i| i.fields.get_mut(col))
            .and_then(Option::as_mut)
        {
            field.shown = value.clone();
        }
        if let Err(reason) = stage_row_value(tab, real, col, value) {
            self.show_hint(reason, cx);
        }
        cx.notify();
    }
    /// Sets field `col` of the inspected row to NULL, like the grid's "Set NULL".
    pub(super) fn set_inspector_null(&mut self, col: usize, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(inspector) = &self.inspector else {
            return;
        };
        if inspector.fields.get(col).is_none_or(Option::is_none) {
            return;
        }
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == inspector.tab) else {
            return;
        };
        let key = inspector.row.clone();
        let Some(real) = edit_rows(tab).and_then(|r| r.rows.iter().position(|r| r.key == key))
        else {
            return;
        };
        if let Err(reason) = stage_row_value(tab, real, col, Value::Null) {
            self.show_hint(reason, cx);
        }
        cx.notify();
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
            let (tab, key) = (i.tab, i.row.clone());
            let text = i.fields[col]
                .as_ref()
                .map(|f| f.input.read(cx).value().to_string())
                .unwrap_or_default();
            self.stage_inspector_text(tab, &key, col, text, cx);
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
        let text = field_text(&field.shown).0.to_owned();
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
    /// The inspector's input for column `col` of the selected row, if that field is editable.
    pub(super) fn inspector_input(&self, col: usize) -> Option<Entity<TextareaState>> {
        self.inspector
            .as_ref()
            .and_then(|i| i.fields.get(col))
            .and_then(Option::as_ref)
            .map(|f| f.input.clone())
    }
    /// The selected row's fields for the right panel, editable where the grid would allow it.
    pub(super) fn inspector_fields(&self, query: &str, cx: &mut Context<Self>) -> Option<Div> {
        let tab = self.state.current();
        let (_, row) = selected_row(tab)?;
        let columns = &edit_rows(tab)?.result.columns;
        let saving = self.saving;
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
                .children(
                    columns
                        .iter()
                        .enumerate()
                        .filter(|(_, label)| label.to_lowercase().contains(query))
                        .map(|(col, label)| {
                            let value = row.cur.get(col).cloned().unwrap_or(Value::Null);
                            let state = FieldState::of(row, col);
                            let input = self.inspector_input(col);
                            let can_null = input.is_some() && !saving && value != Value::Null;
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
                                                .text_size(px(12.5))
                                                .line_height(px(15.))
                                                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                                .truncate()
                                                .child(label.clone()),
                                        )
                                        .when(can_null, |d| {
                                            d.child(
                                                div()
                                                    .id(("inspector-null", col))
                                                    .flex_shrink_0()
                                                    .px(px(5.))
                                                    .rounded(px(4.))
                                                    .text_size(px(11.))
                                                    .line_height(px(15.))
                                                    .text_color(rgb(telecat_theme::MUTED))
                                                    .hover(|d| {
                                                        d.bg(rgb(telecat_theme::INPUT_2))
                                                            .text_color(rgb(telecat_theme::TEXT))
                                                    })
                                                    .cursor_pointer()
                                                    .tooltip(text_tooltip("Set this field to NULL"))
                                                    .child("Set NULL")
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_inspector_null(col, cx)
                                                        },
                                                    )),
                                            )
                                        }),
                                )
                                .child(match input {
                                    Some(input) => editable_field(&input, state),
                                    None => read_only_field(field_text(&value), state),
                                })
                        }),
                ),
        )
    }
}
fn field_input(value: &Value, window: &mut Window, cx: &mut App) -> Entity<TextareaState> {
    let (text, placeholder) = field_text(value);
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
        .text_size(px(13.))
        .line_height(px(18.))
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
