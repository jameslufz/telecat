use super::*;
#[derive(Default)]
pub(super) struct SidebarState {
    pub focused: Option<DbObject>,
    pub columns: std::collections::BTreeMap<String, Option<Vec<(String, String)>>>,
}
pub(super) fn table_key(t: &TableRef) -> String {
    format!("{}\0{}", t.schema, t.name)
}
impl Workspace {
    pub(super) fn toggle_tree_columns(&mut self, table: TableRef, cx: &mut Context<Self>) {
        let key = table_key(&table);
        if self.tree_state.columns.remove(&key).is_some() {
            cx.notify();
            return;
        }
        self.tree_state.columns.insert(key.clone(), None);
        let Some(s) = self.session.clone() else {
            let cols = self
                .state
                .tabs
                .iter()
                .find_map(|t| t.table.as_ref().filter(|t| t.table == table))
                .map(|t| {
                    t.data
                        .result
                        .columns
                        .iter()
                        .map(|c| (c.clone(), String::new()))
                        .collect()
                })
                .unwrap_or_default();
            self.tree_state.columns.insert(key, Some(cols));
            cx.notify();
            return;
        };
        let backend = s.backend_id.clone();
        let task = self
            .services
            .runtime
            .spawn(async move { s.structure(&table).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .session
                    .as_ref()
                    .is_none_or(|s| s.backend_id != backend)
                    || !this.tree_state.columns.contains_key(&key)
                {
                    return;
                }
                match r {
                    Ok(Ok(r)) => {
                        let name = r.columns.iter().position(|c| c == "column_name");
                        let ty = r.columns.iter().position(|c| c == "data_type");
                        let cols = r
                            .rows
                            .iter()
                            .filter_map(|r| {
                                name.and_then(|i| r.get(i))
                                    .and_then(|s| s.clone())
                                    .map(|name| {
                                        (
                                            name,
                                            ty.and_then(|i| r.get(i))
                                                .and_then(|s| s.clone())
                                                .unwrap_or_default(),
                                        )
                                    })
                            })
                            .collect();
                        this.tree_state.columns.insert(key, Some(cols));
                    }
                    Ok(Err(e)) => {
                        this.tree_state.columns.remove(&key);
                        this.error(e, cx);
                    }
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn tree_keyboard(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.tree_focus.contains_focused(window, cx) {
            return false;
        }
        let Some(object) = self.tree_state.focused.clone() else {
            return false;
        };
        if !matches!(object.kind, ObjectKind::Table | ObjectKind::View) {
            return false;
        }
        match event.keystroke.key.as_str() {
            "backspace" | "delete" if !event.keystroke.modifiers.platform => {
                if self.table_status(&object.table) != Some("new") {
                    let truncate =
                        event.keystroke.modifiers.alt && object.kind == ObjectKind::Table;
                    self.stage_operation(object, truncate, window, cx);
                }
                true
            }
            "escape" => {
                self.state
                    .operations
                    .retain(|o| o.object.table != object.table);
                cx.notify();
                true
            }
            _ => false,
        }
    }
}
