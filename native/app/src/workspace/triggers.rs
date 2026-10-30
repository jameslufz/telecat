//! The Structure tab's Triggers sheet: list, add, edit and delete a table's triggers. Edits are
//! staged like every other table change and saved with ⌘S (Preview and Safe Mode included).
use super::*;
use telecat_core::changes::row_key;
use telecat_core::objects::{Trigger, TriggerEdit, new_trigger, trigger_events, trigger_timings};
use telecat_editor::{EditorEvent, EditorOptions, SqlEditor};

pub(super) struct TriggersUi {
    tab: u64,
    name: Entity<InputState>,
    statement: Entity<SqlEditor>,
    /// The edit the fields show.
    shown: Option<String>,
    /// The fields need the selected trigger's values (after the list loaded).
    fill: bool,
    _subscriptions: Vec<Subscription>,
}
/// Why the Triggers button can't be used for this table, if it can't.
pub(super) fn triggers_unavailable(
    t: &telecat_app::table_model::TableEditor,
) -> Option<&'static str> {
    if t.is_new {
        Some("Save the table first (⌘S): triggers are added to an existing table.")
    } else if t.read_only() {
        Some("Views have no triggers here.")
    } else {
        None
    }
}
impl Workspace {
    pub(super) fn open_triggers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let id = self.state.active;
        let Some(t) = self.state.current().table.as_ref() else {
            return;
        };
        if let Some(why) = triggers_unavailable(t) {
            self.show_hint(why, cx);
            return;
        }
        let table = t.table.clone();
        let loaded = t.triggers.edits.is_some();
        let name = text_input("", "trigger_name", false, window, cx);
        let statement = cx.new(|cx| {
            SqlEditor::new(
                EditorOptions {
                    driver: self.state.connection.driver,
                    text: String::new(),
                    schema: self.completion_schema(),
                    placeholder: if self.state.connection.driver == Driver::Mysql {
                        "BEGIN … END, or one statement".into()
                    } else {
                        "FOR EACH ROW EXECUTE FUNCTION function_name()".into()
                    },
                },
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe(&name, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.edit_shown_trigger(|t| t.name = value, cx);
                }
            }),
            cx.subscribe(&statement, |this, editor, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::Change(_)) {
                    let value = editor.read(cx).text(cx).to_string();
                    this.edit_shown_trigger(|t| t.statement = value, cx);
                }
            }),
        ];
        self.triggers_ui = Some(TriggersUi {
            tab: id,
            name,
            statement,
            shown: None,
            fill: false,
            _subscriptions: subscriptions,
        });
        self.sheet = Some(Sheet::Triggers);
        if loaded {
            self.show_selected_trigger(window, cx);
        } else {
            self.load_triggers(id, table, cx);
        }
        cx.notify();
    }
    fn load_triggers(&mut self, id: u64, table: TableRef, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.begin_load(Some((id, LoadSlot::Triggers)), cx);
        let task = self
            .services
            .runtime
            .spawn(async move { session.triggers(&table).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.end_load(Some((id, LoadSlot::Triggers)), &flatten(&r));
                match r {
                    Ok(Ok(list)) => {
                        if let Some(t) = this
                            .state
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == id)
                            .and_then(|t| t.table.as_mut())
                            .filter(|t| t.triggers.edits.is_none())
                        {
                            t.triggers.edits = Some(
                                list.into_iter()
                                    .map(|t| TriggerEdit {
                                        key: row_key(),
                                        orig: Some(t.clone()),
                                        cur: t,
                                        deleted: false,
                                    })
                                    .collect(),
                            );
                            t.triggers.selected = 0;
                        }
                        if let Some(ui) = this.triggers_ui.as_mut().filter(|u| u.tab == id) {
                            // Filled in on the next frame, which has a window.
                            ui.fill = true;
                        }
                    }
                    Ok(Err(e)) if telecat_db::is_connection_lost(&e) => this.failed(e, cx),
                    Ok(Err(_)) => {}
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn prepare_triggers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.triggers_ui.as_ref().is_some_and(|u| u.fill) {
            if let Some(ui) = &mut self.triggers_ui {
                ui.fill = false;
            }
            self.show_selected_trigger(window, cx);
        }
    }
    fn trigger_state_mut(&mut self) -> Option<&mut telecat_app::table_model::TriggerState> {
        let id = self.triggers_ui.as_ref()?.tab;
        self.state
            .tabs
            .iter_mut()
            .find(|t| t.id == id)
            .and_then(|t| t.table.as_mut())
            .map(|t| &mut t.triggers)
    }
    fn trigger_state(&self) -> Option<&telecat_app::table_model::TriggerState> {
        let id = self.triggers_ui.as_ref()?.tab;
        self.state
            .tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.table.as_ref())
            .map(|t| &t.triggers)
    }
    fn edit_shown_trigger(&mut self, edit: impl FnOnce(&mut Trigger), cx: &mut Context<Self>) {
        let Some(shown) = self.triggers_ui.as_ref().and_then(|u| u.shown.clone()) else {
            return;
        };
        if self.saving {
            return;
        }
        if let Some(e) = self
            .trigger_state_mut()
            .and_then(|s| s.edits.as_mut())
            .and_then(|edits| edits.iter_mut().find(|e| e.key == shown))
        {
            edit(&mut e.cur);
        }
        cx.notify();
    }
    /// Puts the selected trigger's values into the fields.
    pub(super) fn show_selected_trigger(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.trigger_state().and_then(|s| {
            s.edits
                .as_ref()
                .and_then(|e| e.get(s.selected))
                .map(|e| (e.key.clone(), e.cur.clone()))
        });
        let Some(ui) = &mut self.triggers_ui else {
            return;
        };
        let (key, trigger) = match selected {
            Some((key, t)) => (Some(key), t),
            None => (None, Trigger::default()),
        };
        // Cleared first so filling the fields doesn't count as editing the old selection.
        ui.shown = None;
        let (name, statement) = (ui.name.clone(), ui.statement.clone());
        name.update(cx, |s, cx| s.set_value(trigger.name.clone(), window, cx));
        statement.update(cx, |e, cx| {
            e.set_text(trigger.statement.clone(), window, cx)
        });
        if let Some(ui) = &mut self.triggers_ui {
            ui.shown = key;
        }
    }
    fn select_trigger(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = self.trigger_state_mut() {
            s.selected = index;
        }
        self.show_selected_trigger(window, cx);
        cx.notify();
    }
    pub(super) fn add_trigger(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let d = self.state.connection.driver;
        let table = self
            .triggers_ui
            .as_ref()
            .and_then(|u| self.state.tabs.iter().find(|t| t.id == u.tab))
            .and_then(|t| t.table.as_ref())
            .map(|t| t.table.name.clone())
            .unwrap_or_default();
        let Some(state) = self.trigger_state_mut() else {
            return;
        };
        let Some(edits) = state.edits.as_mut() else {
            return;
        };
        let taken: Vec<String> = edits.iter().map(|e| e.cur.name.clone()).collect();
        let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
        edits.push(TriggerEdit {
            key: row_key(),
            orig: None,
            cur: new_trigger(d, &table, &taken),
            deleted: false,
        });
        state.selected = edits.len() - 1;
        self.show_selected_trigger(window, cx);
        if let Some(ui) = &self.triggers_ui {
            ui.name.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }
    /// Marks the selected trigger for deletion (or undeletes it); a new one is just removed.
    fn delete_trigger(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(state) = self.trigger_state_mut() else {
            return;
        };
        let selected = state.selected;
        let Some(edits) = state.edits.as_mut() else {
            return;
        };
        let Some(e) = edits.get_mut(selected) else {
            return;
        };
        if e.orig.is_none() {
            edits.remove(selected);
            state.selected = selected.min(edits.len().saturating_sub(1));
        } else {
            e.deleted = !e.deleted;
        }
        self.show_selected_trigger(window, cx);
        cx.notify();
    }
    pub(super) fn triggers_sheet(&self, cx: &mut Context<Self>) -> Div {
        let d = self.state.connection.driver;
        let Some(ui) = &self.triggers_ui else {
            return sheet(400., "Triggers");
        };
        let tab = self.state.tabs.iter().find(|t| t.id == ui.tab);
        let title = tab
            .and_then(|t| t.table.as_ref())
            .map_or("Triggers".to_owned(), |t| {
                format!("Triggers of {}", t.table.name)
            });
        let state = tab.and_then(|t| t.table.as_ref()).map(|t| &t.triggers);
        let load = tab.map_or(LoadState::Idle, |t| t.load(LoadSlot::Triggers).clone());
        let edits = state.and_then(|s| s.edits.clone());
        let selected = state.map_or(0, |s| s.selected);
        let current = edits.as_ref().and_then(|e| e.get(selected)).cloned();
        let list = match (&edits, &load) {
            (None, LoadState::Failed(e)) => div()
                .p(px(12.))
                .text_color(rgb(0xff8a80))
                .child(format!("Couldn't load the triggers: {e}")),
            (None, _) => div()
                .p(px(12.))
                .text_color(rgb(telecat_theme::MUTED))
                .child("Loading…"),
            (Some(e), _) if e.is_empty() => div()
                .p(px(12.))
                .text_color(rgb(telecat_theme::MUTED))
                .child("No triggers. Press + Trigger to add one."),
            (Some(e), _) => div().children(e.iter().enumerate().map(|(i, edit)| {
                let status = if edit.deleted {
                    Some(0x5b1616)
                } else if edit.orig.is_none() {
                    Some(0x2d5724)
                } else if edit.changed() {
                    Some(0x6b5518)
                } else {
                    None
                };
                div()
                    .id(("trigger", i))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when_some(status, |d, c| d.bg(rgb(c)))
                    .when(i == selected, |d| {
                        d.border_1().border_color(rgb(telecat_theme::ACCENT))
                    })
                    .child(
                        div()
                            .font(mono())
                            .text_size(px(12.))
                            .truncate()
                            .when(edit.deleted, |d| d.line_through())
                            .child(if edit.cur.name.trim().is_empty() {
                                "(no name)".to_owned()
                            } else {
                                edit.cur.name.clone()
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(telecat_theme::MUTED))
                            .truncate()
                            .child(format!("{} {}", edit.cur.timing, edit.cur.event)),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_trigger(i, window, cx)),
                    )
            })),
        };
        let field = |label: &'static str| {
            div()
                .w(px(70.))
                .flex_shrink_0()
                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                .child(label)
        };
        let editable = current.as_ref().is_some_and(|c| !c.deleted) && !self.saving;
        let choice = |id: &'static str, value: String, options: &'static [&'static str]| {
            small_button(id, value)
                .w(px(170.))
                .h(px(28.))
                .justify_between()
                .child(icon("UpDown.svg", 12., 0xb8b8b8))
                .when(!editable, |d| d.opacity(0.4))
                .when(editable, |d| {
                    d.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, e: &MouseDownEvent, window, cx| {
                            let items: Vec<_> = options
                                .iter()
                                .map(|o| json!({"id": o, "label": o}))
                                .collect();
                            native_menu::choose(
                                json!(items),
                                e.position,
                                window,
                                cx,
                                move |this, chosen, _, cx| {
                                    this.edit_shown_trigger(
                                        |t| {
                                            if id == "trigger-timing" {
                                                t.timing = chosen
                                            } else {
                                                t.event = chosen
                                            }
                                        },
                                        cx,
                                    )
                                },
                            );
                        }),
                    )
                })
        };
        let form = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(current.is_none(), |d| d.opacity(0.4))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(field("Name"))
                    .child(input_box(&ui.name, !editable, false).font(mono())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(field("Timing"))
                    .child(choice(
                        "trigger-timing",
                        current
                            .as_ref()
                            .map_or(String::new(), |c| c.cur.timing.clone()),
                        trigger_timings(d),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(field("Event"))
                    .child(choice(
                        "trigger-event",
                        current
                            .as_ref()
                            .map_or(String::new(), |c| c.cur.event.clone()),
                        trigger_events(d),
                    )),
            )
            .child(field("Statement"))
            .child(
                div()
                    .h(px(200.))
                    .border_1()
                    .border_color(rgb(telecat_theme::BORDER))
                    .rounded(px(7.))
                    .overflow_hidden()
                    .when(!editable, |d| d.opacity(0.5))
                    .child(ui.statement.clone()),
            );
        let can_edit = edits.is_some() && !self.saving;
        sheet(760., title)
            .child(
                div()
                    .flex()
                    .gap(px(14.))
                    .child(
                        div()
                            .id("trigger-list")
                            .w(px(220.))
                            .h(px(320.))
                            .flex_shrink_0()
                            .overflow_y_scroll()
                            .p(px(4.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(telecat_theme::BORDER))
                            .bg(rgb(telecat_theme::INPUT))
                            .child(list),
                    )
                    .child(form),
            )
            .child(
                div()
                    .mt(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        small_button("trigger-add", "+ Trigger")
                            .tooltip(text_tooltip("Add a trigger (⌘I)"))
                            .when(!can_edit, |d| d.opacity(0.4))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_trigger(window, cx)
                            })),
                    )
                    .child(
                        small_button(
                            "trigger-delete",
                            if current.as_ref().is_some_and(|c| c.deleted) {
                                "Undo Delete"
                            } else {
                                "Delete"
                            },
                        )
                        .when(current.is_none() || !can_edit, |d| d.opacity(0.4))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.delete_trigger(window, cx)
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(rgb(telecat_theme::MUTED))
                            .child("Changes are staged: close this and press ⌘S to save, ⇧⌘P to preview."),
                    )
                    .child(button("trigger-done", "Done", true, false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.cancel_sheet(cx);
                        },
                    ))),
            )
    }
}
