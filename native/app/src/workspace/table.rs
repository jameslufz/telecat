use super::*;
use std::{cell::Cell as Slot, rc::Rc};
use telecat_app::table_model::{Part, TableEditor, new_filter};
use telecat_core::filters::{filter_ops, op_label, rule_applied, value_placeholder};

/// Where a filter rule's column and operator popups were last drawn, so ⌘← / ⌘→ open their
/// menus in place.
#[derive(Clone, Default)]
struct Anchors {
    column: Rc<Slot<Point<Pixels>>>,
    op: Rc<Slot<Point<Pixels>>>,
}
pub(super) struct TableInputs {
    name: Entity<InputState>,
    primary: Entity<InputState>,
    pub(super) search: Entity<InputState>,
    filters: Vec<(Entity<InputState>, Entity<InputState>)>,
    anchors: Vec<Anchors>,
    _subscriptions: Vec<Subscription>,
}
impl TableInputs {
    /// Rule `i`'s value and second-value fields.
    pub(super) fn filter_input(
        &self,
        i: usize,
    ) -> Option<(&Entity<InputState>, &Entity<InputState>)> {
        self.filters.get(i).map(|(a, b)| (a, b))
    }
}
/// Records where an element was drawn (its top-left corner, below it) into `slot`.
fn anchor(slot: Rc<Slot<Point<Pixels>>>) -> impl IntoElement {
    canvas(
        move |bounds, _, _| slot.set(bounds.bottom_left()),
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}
#[derive(Clone)]
pub(super) enum TableNavigation {
    Page(u64, u64),
    Sort(Vec<OrderBy>),
    Filters(Combine, Option<usize>),
    Clear,
    /// The filter bar with exactly these rules, all applied (Quick Filter, a foreign key). With
    /// `true` the result is a new Go Back step (a foreign key into the same tab).
    Show(Vec<Filter>, bool),
    /// A view from Go Back / Go Forward.
    View(telecat_core::table_state::TableView),
}
impl Workspace {
    pub(super) fn prepare_table_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.state.current();
        let Some(table) = &tab.table else {
            return;
        };
        let id = tab.id;
        if self
            .table_inputs
            .get(&id)
            .is_some_and(|i| i.filters.len() == table.filters.len())
        {
            return;
        }
        let name = text_input(table.name.clone(), "", false, window, cx);
        let primary = text_input(table.primary.clone(), "column, column", false, window, cx);
        let search = text_input(
            table.column_filter.clone().unwrap_or_default(),
            if table.part == Part::Index {
                "Search for index..."
            } else {
                "Search for column..."
            },
            false,
            window,
            cx,
        );
        let filters: Vec<_> = table
            .filters
            .iter()
            .map(|f| {
                (
                    text_input(f.value.clone(), value_placeholder(f.op), false, window, cx),
                    text_input(
                        f.value2.clone().unwrap_or_default(),
                        "to",
                        false,
                        window,
                        cx,
                    ),
                )
            })
            .collect();
        let mut subscriptions = vec![];
        for (input, field) in [(&name, "name"), (&primary, "primary"), (&search, "search")] {
            subscriptions.push(
                cx.subscribe(input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) && !this.saving {
                        let value = input.read(cx).value().to_string();
                        if let Some(t) = this
                            .state
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == id)
                            .and_then(|t| t.table.as_mut())
                        {
                            match field {
                                "name" => t.name = value,
                                "primary" => t.primary = value,
                                "search" if t.part == Part::Data && t.column_filter.is_some() => {
                                    t.column_filter = Some(value)
                                }
                                _ => {}
                            }
                        }
                        cx.notify();
                    }
                }),
            );
        }
        for (i, pair) in filters.iter().enumerate() {
            for (input, second) in [(&pair.0, false), (&pair.1, true)] {
                subscriptions.push(cx.subscribe_in(
                    input,
                    window,
                    move |this, input, event: &InputEvent, window, cx| match event {
                        InputEvent::Change if !this.saving => {
                            let value = input.read(cx).value().to_string();
                            if let Some(f) = this
                                .state
                                .tabs
                                .iter_mut()
                                .find(|t| t.id == id)
                                .and_then(|t| t.table.as_mut())
                                .and_then(|t| t.filters.get_mut(i))
                            {
                                if second {
                                    f.value2 = Some(value);
                                } else {
                                    f.value = value;
                                }
                            }
                            this.queue_table_state(id, cx);
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. } => this.navigate_table(
                            TableNavigation::Filters(Combine::And, Some(i)),
                            window,
                            cx,
                        ),
                        _ => {}
                    },
                ));
            }
        }
        let anchors = vec![Anchors::default(); filters.len()];
        self.table_inputs.insert(
            id,
            TableInputs {
                name,
                primary,
                search,
                filters,
                anchors,
                _subscriptions: subscriptions,
            },
        );
    }
    pub(super) fn switch_part(&mut self, part: Part, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if let Some(t) = self.state.current_mut().table.as_mut() {
            t.part = part;
        }
        self.table_inputs.remove(&self.state.active);
        self.load_metadata(cx);
        cx.notify();
    }
    pub(super) fn load_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let tab = self.state.current();
        let Some(t) = &tab.table else {
            return;
        };
        if t.is_new {
            return;
        }
        let (id, part, table) = (tab.id, t.part, t.table.clone());
        if part == Part::Data || t.buffer().loaded {
            return;
        }
        self.begin_load(Some((id, LoadSlot::Part(part))), cx);
        let task = self.services.runtime.spawn(async move {
            let result = if part == Part::Structure {
                session.structure(&table).await?
            } else {
                session.indexes(&table).await?
            };
            let structure = if part == Part::Index {
                Some(session.structure(&table).await?)
            } else {
                None
            };
            Ok::<_, anyhow::Error>((result, structure))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.end_load(Some((id, LoadSlot::Part(part))), &super::flatten(&r));
                match r {
                    Ok(Ok((result, structure))) => {
                        this.state.logs.push(LogEntry {
                            time: timestamp(),
                            sql: result.sql.clone(),
                        });
                        if let Some(t) = this
                            .state
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == id)
                            .and_then(|t| t.table.as_mut())
                        {
                            if let Some(structure) = structure {
                                if !t.structure.dirty() {
                                    t.structure.reset(structure, None);
                                }
                            }
                            let buffer = if part == Part::Structure {
                                &mut t.structure
                            } else {
                                &mut t.index
                            };
                            // Never overwrite edits made while a read was in flight.
                            if !buffer.dirty() {
                                buffer.reset(result, None);
                            }
                        }
                    }
                    Ok(Err(e)) => this.failed(e, cx),
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn navigate_table(
        &mut self,
        action: TableNavigation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        self.flush_grid_edits(window, cx);
        let dirty = self
            .state
            .current()
            .table
            .as_ref()
            .is_some_and(|t| t.data.dirty());
        if !dirty {
            self.apply_table_navigation(action, cx);
            return;
        }
        let id = self.state.active;
        let prompt = window.prompt(
            PromptLevel::Warning,
            "Discard unsaved changes?",
            Some("You have edited rows on this page. Press ⌘S to save them first, or discard."),
            &["Discard", "Cancel"],
            cx,
        );
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let discard = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if discard && this.state.active == id {
                    if let Some(t) = this.state.current_mut().table.as_mut() {
                        t.data.discard();
                    }
                    this.apply_table_navigation(action, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn apply_table_navigation(&mut self, action: TableNavigation, cx: &mut Context<Self>) {
        let tab = self.state.current_mut();
        let Some(t) = tab.table.as_mut() else {
            return;
        };
        let step = matches!(action, TableNavigation::Show(_, true));
        match action {
            TableNavigation::Sort(sort) => {
                t.order_by = sort;
                tab.offset = 0;
            }
            TableNavigation::Show(filters, _) => {
                t.filters = filters;
                t.show_filters = true;
                t.apply_filters(Combine::And, None);
                tab.offset = 0;
                self.table_inputs.remove(&tab.id);
            }
            TableNavigation::View(view) => {
                t.restore_view(&view);
                tab.offset = view.offset;
                self.table_inputs.remove(&tab.id);
            }
            TableNavigation::Page(offset, limit) => {
                tab.offset = offset;
                tab.limit = limit;
            }
            TableNavigation::Filters(combine, one) => {
                t.apply_filters(combine, one);
                tab.offset = 0;
            }
            TableNavigation::Clear => {
                t.applied.clear();
                t.filters = vec![new_filter()];
                tab.offset = 0;
                self.table_inputs.remove(&tab.id);
            }
        }
        let id = tab.id;
        if step {
            // A new place for Go Back to return from.
            self.state.visit_current_view();
        }
        self.queue_table_state(id, cx);
        self.refresh_tab(cx);
        cx.notify();
    }
    pub(super) fn new_table(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut n = 100;
        while self
            .state
            .objects
            .iter()
            .any(|o| o.table.name == format!("untitled_table_{n}"))
            || self.state.tabs.iter().any(|t| {
                t.table
                    .as_ref()
                    .is_some_and(|e| e.name == format!("untitled_table_{n}"))
            })
        {
            n += 1;
        }
        let table = TableRef {
            schema: if self.state.connection.driver == Driver::Mysql {
                self.state.database.clone()
            } else {
                "public".into()
            },
            name: format!("untitled_table_{n}"),
        };
        self.state.open_table(table.clone());
        let editor = TableEditor::new(table.clone(), self.state.connection.driver, true);
        self.state.current_mut().table = Some(editor);
        self.state.objects.push(DbObject {
            table,
            kind: ObjectKind::Table,
            signature: None,
        });
        self.state
            .objects
            .sort_by_key(|o| o.table.name.to_lowercase());
        cx.notify();
    }
    pub(super) fn show_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.state.current_mut().table.as_mut() {
            t.part = Part::Data;
            t.show_filters = true;
        }
        self.prepare_table_inputs(window, cx);
        if let Some(input) = self
            .table_inputs
            .get(&self.state.active)
            .and_then(|i| i.filters.first())
            .map(|p| p.0.clone())
        {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }
    fn filters_view(&self, t: &TableEditor, inputs: &TableInputs, cx: &mut Context<Self>) -> Div {
        let driver = self.state.connection.driver;
        let mut rows = div()
            .flex_shrink_0()
            .px(px(10.))
            .py(px(8.))
            .bg(rgb(0x242424))
            .border_b_1()
            .border_color(rgb(0x111111))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(rgb(telecat_theme::MUTED))
                    .child("Show: ⌘F    Insert: ⌘I    Remove: ⌘⇧I    Apply All: ⌘Return    Up: ⌘↑    Down: ⌘↓    Columns: ⌘←    Operators: ⌘→    On/Off: ⌘B    Exit: Esc"),
            );
        for (i, f) in t.filters.iter().enumerate() {
            let raw = f.column == RAW_SQL;
            let between = matches!(f.op, FilterOp::Between | FilterOp::NotBetween);
            let column = if raw {
                "Raw SQL"
            } else if f.column.is_empty() {
                "Any column"
            } else {
                &f.column
            };
            let anchors = inputs.anchors.get(i).cloned().unwrap_or_default();
            let applied = rule_applied(f, &t.applied);
            let row = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    checkbox(("filter-enabled", i), "", f.enabled, false)
                        .tooltip(text_tooltip(
                            "Include/exclude this filter when Apply All (⌘B)",
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_filter_rule(i, cx)),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            popup(("filter-column", i), column.to_owned(), 180.)
                                .tooltip(text_tooltip("Table's columns (⌘←)"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                        this.filter_column_menu(i, e.position, window, cx)
                                    }),
                                ),
                        )
                        .child(anchor(anchors.column.clone())),
                )
                .when(!raw, |d| {
                    d.child(
                        div()
                            .relative()
                            .child(
                                popup(("filter-op", i), op_label(driver, f.op), 200.)
                                    .tooltip(text_tooltip("Query operators (⌘→)"))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                            this.filter_op_menu(i, e.position, window, cx)
                                        }),
                                    ),
                            )
                            .child(anchor(anchors.op.clone())),
                    )
                })
                .child(
                    input_box(
                        &inputs.filters[i].0,
                        !raw && matches!(f.op, FilterOp::IsNull | FilterOp::IsNotNull),
                        (self.scenario == "08-table-filter" && i == 0)
                            || (self.scenario == "52-filter" && i == 1),
                    )
                    .h(px(28.))
                    .text_size(px(12.)),
                )
                .when(between && !raw, |d| {
                    d.child("and").child(
                        input_box(&inputs.filters[i].1, false, false)
                            .h(px(28.))
                            .text_size(px(12.)),
                    )
                })
                .child(
                    small_button(
                        ("apply-filter", i),
                        if applied { "Applied" } else { "Apply" },
                    )
                    .w(px(68.))
                    .justify_center()
                    .when(applied, |d| d.text_color(rgb(telecat_theme::MUTED)))
                    .tooltip(text_tooltip("Apply the filter (↵)"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_table(
                            TableNavigation::Filters(Combine::And, Some(i)),
                            window,
                            cx,
                        )
                    })),
                )
                .child(
                    small_button(("remove-filter", i), "−")
                        .w(px(30.))
                        .tooltip(text_tooltip("Remove this filter (⌘⇧I)"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_filter_rule(i, window, cx)
                        })),
                )
                .child(
                    small_button(("insert-filter", i), "+")
                        .w(px(30.))
                        .tooltip(text_tooltip("Clone this filter (⌘I)"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.clone_filter_rule(i, window, cx)
                        })),
                );
            rows = rows.child(row);
        }
        rows.child(div().flex().items_center().gap(px(8.))
            .child(small_button("filter-export",if self.export_job.is_some(){"Stop Export"}else{"Export"}).tooltip(text_tooltip("Export the filter result")).on_click(cx.listener(|this,_,_,cx|this.export_result(cx))))
            .child(small_button("filter-sql","SQL").tooltip(text_tooltip("Show the filter queries")).on_click(cx.listener(|this,_,_,cx|this.filter_sql(cx))))
            .child(div().flex_1())
            .child(small_button("filter-clear","Clear").tooltip(text_tooltip("Reload the table content without the filters")).on_click(cx.listener(|this,_,window,cx|this.navigate_table(TableNavigation::Clear,window,cx))))
            .child(div().id("filter-apply-group").flex().h(px(28.)).border_1().border_color(rgb(0x3c3c3c)).rounded(px(7.)).bg(rgb(telecat_theme::INPUT_2)).tooltip(text_tooltip("Apply all checked filters (⌘↵)"))
                .child(div().id("filter-apply-all").px(px(12.)).flex().items_center().child("Apply All").on_click(cx.listener(|this,_,window,cx|this.navigate_table(TableNavigation::Filters(Combine::And,None),window,cx))))
                .child(div().id("filter-combine").px(px(7.)).border_l_1().border_color(rgba(0xffffff18)).flex().items_center().child(icon("ChevronDown.svg",13.,0xe8e8e8)).on_mouse_down(MouseButton::Left,cx.listener(|_,e:&MouseDownEvent,window,cx|native_menu::choose(json!([{"id":"AND","label":"Apply All Checked Filters with AND - Default","key":"⌘↩"},{"id":"OR","label":"Apply All Checked Filters with OR","key":"⇧⌘↩"}]),e.position,window,cx,|this,id,window,cx|this.navigate_table(TableNavigation::Filters(if id == "OR" {Combine::Or} else {Combine::And},None),window,cx)))))))
    }
    /// Rule `i`'s column menu: the table's columns, then Any column and Raw SQL.
    fn filter_column_menu(
        &mut self,
        i: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = &self.state.current().table else {
            return;
        };
        let current = t
            .filters
            .get(i)
            .map(|f| f.column.clone())
            .unwrap_or_default();
        let mut items: Vec<_> = t
            .data
            .result
            .columns
            .iter()
            .map(|c| json!({"id":c,"label":c,"checked":*c == current}))
            .collect();
        items.extend([
            json!({"type":"separator"}),
            json!({"id":ANY_COLUMN,"label":"Any column","checked":current == ANY_COLUMN}),
            json!({"id":RAW_SQL,"label":"Raw SQL","checked":current == RAW_SQL}),
        ]);
        let tab = self.state.active;
        native_menu::choose(json!(items), at, window, cx, move |this, id, _, cx| {
            if let Some(f) = this.table_of(tab).and_then(|t| t.filters.get_mut(i)) {
                f.column = id;
            }
            this.queue_table_state(tab, cx);
            cx.notify();
        });
    }
    /// Rule `i`'s operator menu: this driver's list, as TablePlus words it.
    fn filter_op_menu(
        &mut self,
        i: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = &self.state.current().table else {
            return;
        };
        let current = t.filters.get(i).map(|f| f.op);
        let items: Vec<_> = filter_ops(self.state.connection.driver)
            .into_iter()
            .map(|o| match o {
                None => json!({"type":"separator"}),
                Some((op, label)) => {
                    json!({"id":op.as_str(),"label":label,"checked":Some(op) == current})
                }
            })
            .collect();
        let tab = self.state.active;
        native_menu::choose(json!(items), at, window, cx, move |this, id, window, cx| {
            let Ok(op) = serde_json::from_value::<FilterOp>(json!(id)) else {
                return;
            };
            if let Some(f) = this.table_of(tab).and_then(|t| t.filters.get_mut(i)) {
                f.op = op;
            }
            if let Some((input, _)) = this.table_inputs.get(&tab).and_then(|x| x.filter_input(i)) {
                input.update(cx, |s, cx| {
                    s.set_placeholder(value_placeholder(op), window, cx)
                });
            }
            this.queue_table_state(tab, cx);
            cx.notify();
        });
    }
    fn toggle_filter_rule(&mut self, i: usize, cx: &mut Context<Self>) {
        let tab = self.state.active;
        if let Some(f) = self.table_of(tab).and_then(|t| t.filters.get_mut(i)) {
            f.enabled = !f.enabled;
        }
        self.queue_table_state(tab, cx);
        cx.notify();
    }
    fn remove_filter_rule(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.state.active;
        let mut empty = false;
        if let Some(t) = self.table_of(tab)
            && i < t.filters.len()
        {
            t.filters.remove(i);
            empty = t.filters.is_empty();
        }
        self.table_inputs.remove(&tab);
        if empty {
            self.navigate_table(TableNavigation::Clear, window, cx);
        }
        self.queue_table_state(tab, cx);
        cx.notify();
    }
    /// The + button and ⌘I: a copy of the rule right below it, its value field focused.
    fn clone_filter_rule(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.state.active;
        if let Some(t) = self.table_of(tab) {
            let copy = t.filters.get(i).cloned().unwrap_or_else(new_filter);
            t.filters.insert((i + 1).min(t.filters.len()), copy);
        }
        self.table_inputs.remove(&tab);
        self.prepare_table_inputs(window, cx);
        if let Some((input, _)) = self
            .table_inputs
            .get(&tab)
            .and_then(|x| x.filter_input(i + 1))
        {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        self.queue_table_state(tab, cx);
        cx.notify();
    }
    fn filter_sql(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let tab = self.state.current();
        let Some(t) = &tab.table else {
            return;
        };
        let req = TableDataRequest {
            table: t.table.clone(),
            filters: t.applied.clone(),
            combine: t.combine,
            order_by: t.order_by.clone(),
            estimate: false,
            limit: tab.limit,
            offset: tab.offset,
        };
        let task = self
            .services
            .runtime
            .spawn(async move { session.table_data_sql(&req).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| match r {
                Ok(Ok(sql)) => this.new_query(&sql, cx),
                Ok(Err(e)) => this.failed(e, cx),
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
    pub(super) fn table_content(&self, cx: &mut Context<Self>) -> Div {
        let tab = self.state.current();
        let t = tab.table.as_ref().unwrap();
        let inputs = &self.table_inputs[&tab.id];
        let mut view = div().flex_1().min_h_0().flex().flex_col();
        if t.part == Part::Data && t.show_filters {
            view = view.child(self.filters_view(t, inputs, cx));
        }
        if t.part != Part::Data {
            let structure = t.part == Part::Structure;
            view = view.child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .bg(rgb(0x242424))
                    .border_b_1()
                    .border_color(rgb(0x111111))
                    .child(div().text_size(px(12.5)).ml(px(6.)).child("Name"))
                    .child(if structure && !t.read_only() {
                        input_box(&inputs.name, self.saving, false)
                            .flex_none()
                            .w(px(280.))
                            .font(mono())
                            .text_size(px(12.))
                            .when(t.name != t.table.name, |d| d.border_color(rgb(0xe0b84a)))
                    } else {
                        div()
                            .flex_1()
                            .h(px(30.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .border_1()
                            .border_color(rgb(telecat_theme::BORDER))
                            .rounded(px(7.))
                            .bg(rgb(telecat_theme::INPUT))
                            .text_size(px(12.5))
                            .child(t.name.clone())
                    })
                    .when(structure && t.read_only(), |d| {
                        d.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(rgb(telecat_theme::MUTED))
                                .child(format!(
                                    "{}: read-only. Change it through its definition (DDL).",
                                    t.kind.label()
                                )),
                        )
                    })
                    .when(structure && !t.read_only(), |d| {
                        d.child(div().text_size(px(12.5)).ml(px(6.)).child("Primary"))
                            .child(if t.is_new {
                                input_box(&inputs.primary, self.saving, false)
                                    .font(mono())
                                    .text_size(px(11.5))
                            } else {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(30.))
                                    .border_1()
                                    .border_color(rgb(telecat_theme::BORDER))
                                    .rounded(px(7.))
                                    .bg(rgb(telecat_theme::INPUT))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .children(t.primary_key.iter().map(|key| {
                                        div()
                                            .rounded(px(4.))
                                            .bg(rgb(0x2c4a78))
                                            .px(px(10.))
                                            .py(px(1.))
                                            .font(mono())
                                            .text_size(px(11.5))
                                            .child(key.clone())
                                    }))
                            })
                    })
                    .child(
                        div()
                            .w(px(250.))
                            .h(px(30.))
                            .flex_shrink_0()
                            .rounded(px(15.))
                            .bg(rgba(0xffffff0c))
                            .flex()
                            .items_center()
                            .px(px(10.))
                            .gap(px(6.))
                            .child(icon("Search.svg", 13., 0x8a8a8a))
                            .child(Input::new(&inputs.search)),
                    ),
            );
        }
        if t.show_ddl && t.part == Part::Structure {
            view = view.child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("ddl")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.ddl_scroll)
                            .p(px(14.))
                            .font(mono())
                            .text_size(px(11.5))
                            .line_height(px(18.))
                            .child(if t.is_new {
                                "-- save the table first (⌘S)".into()
                            } else {
                                t.ddl.clone().unwrap_or("Loading…".into())
                            }),
                    )
                    .child(super::layout::scrollbar(&self.ddl_scroll, cx)),
            );
        } else {
            view = view.child(self.grid_view(cx));
        }
        let mut bottom = subbar().child(
            div()
                .h(px(28.))
                .flex_shrink_0()
                .p(px(2.))
                .flex()
                .gap(px(1.))
                .bg(rgba(0xffffff10))
                .rounded(px(7.))
                .children(
                    [
                        (Part::Data, "Data"),
                        (Part::Structure, "Structure"),
                        (Part::Index, "Index"),
                    ]
                    .into_iter()
                    .map(|(part, label)| {
                        let tip = match part {
                            Part::Data => "Show data (⌃⌘[)",
                            Part::Structure => "Show Structure (⌃⌘])",
                            Part::Index => "Show indexes",
                        };
                        div()
                            .id(label)
                            .tooltip(text_tooltip(tip))
                            .h(px(24.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_color(rgb(if t.part == part { 0xffffff } else { 0xcfcfcf }))
                            .when(t.part == part, |d| d.bg(rgb(telecat_theme::ACCENT)))
                            .opacity(if t.is_new && part == Part::Data {
                                0.4
                            } else {
                                1.
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this
                                    .state
                                    .current()
                                    .table
                                    .as_ref()
                                    .is_some_and(|t| t.is_new && part == Part::Data)
                                {
                                    this.switch_part(part, cx);
                                }
                            }))
                    }),
                ),
        );
        let read_only = t.read_only();
        bottom = bottom.child(
            small_button("table-add", "")
                .gap(px(6.))
                .child(icon("Plus.svg", 13., 0xe8e8e8))
                .child(match t.part {
                    Part::Data => "Row",
                    Part::Structure => "Column",
                    Part::Index => "Index",
                })
                .when(!read_only && t.part == Part::Data, |d| {
                    d.tooltip(text_tooltip("Add new row (⌘I)"))
                })
                .when(read_only, |d| {
                    d.opacity(0.4).tooltip(text_tooltip(format!(
                        "A {} can't be edited here",
                        t.kind.label()
                    )))
                })
                .on_click(cx.listener(|this, _, window, cx| this.add_grid_row(window, cx))),
        );
        if t.part == Part::Data {
            let shown = t.data.rows.iter().filter(|r| r.orig.is_some()).count() as u64;
            let mut counter =
                super::popovers::row_counter(tab.offset, shown, tab.total, tab.estimated);
            let plain_len = counter.len();
            if t.data.dirty() {
                counter.push_str(" · unsaved · ⌘S save · Esc discard");
            }
            let end = counter.len();
            let counter = StyledText::new(counter).with_highlights([(
                plain_len..end,
                HighlightStyle {
                    color: Some(rgb(0xe0b84a).into()),
                    ..Default::default()
                },
            )]);
            let estimated = tab.estimated;
            let first = tab.offset == 0;
            let last = !super::data::has_next_page(tab);
            bottom = bottom
                .child(
                    div()
                        .id("row-count")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_center()
                        .text_color(rgb(telecat_theme::MUTED))
                        .when(estimated, |d| {
                            d.cursor_pointer()
                                .tooltip(text_tooltip("Estimated count. Click to count the rows"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                        this.open_count_popover(e.position, cx)
                                    }),
                                )
                        })
                        .child(counter),
                )
                .child(small_button("filters","").gap(px(6.)).child(icon("Search.svg",13.,0xe8e8e8)).child("Filters").tooltip(text_tooltip("Toggle filter (⌘F)")).when(t.show_filters,|d|d.bg(rgb(telecat_theme::ACCENT)).border_color(rgb(telecat_theme::ACCENT))).on_click(cx.listener(|this,_,_,cx|{let id=this.state.active;if let Some(t)=this.table_of(id){t.show_filters = !t.show_filters;}this.queue_table_state(id,cx);cx.notify();})))
                .when(t.column_filter.is_some(),|d|d.child(div().w(px(180.)).child(input_box(&inputs.search,false,true))))
                .child(small_button("columns","Columns").tooltip(text_tooltip("Select columns to show (⌘⌥F)")).on_mouse_down(MouseButton::Left,cx.listener(|this,e:&MouseDownEvent,window,cx| {
                    let Some(t)=&this.state.current().table else{return;};let mut items=vec![json!({"id":"all","label":"Show All Columns"})];items.extend(t.data.result.columns.iter().enumerate().map(|(i,c)|json!({"id":i.to_string(),"label":c,"checked":!t.hidden.contains(c)})));
                    native_menu::choose(json!(items),e.position,window,cx,|this,id,_,cx|{if let Some(t)=&mut this.state.current_mut().table{if id=="all"{t.hidden.clear();}else if let Some(c)=id.parse::<usize>().ok().and_then(|i|t.data.result.columns.get(i)).cloned(){if !t.hidden.remove(&c){t.hidden.insert(c);}}}cx.notify();});
                })))
                .child(
                    div()
                        .flex()
                        .h(px(28.))
                        .border_1()
                        .border_color(rgb(0x3c3c3c))
                        .rounded(px(7.))
                        .bg(rgb(telecat_theme::INPUT_2))
                        .child(
                            div()
                                .id("page-prev")
                                .w(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(if first { 0.35 } else { 1. })
                                .tooltip(text_tooltip("Previous page (⌘←)"))
                                .child(icon("ChevronLeft.svg", 13., 0xe8e8e8))
                                .on_click(cx.listener(|this, _, window, cx| this.page_step(-1, window, cx))),
                        )
                        .child(
                            div()
                                .id("page-settings")
                                .w(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .border_l_1()
                                .border_r_1()
                                .border_color(rgba(0xffffff18))
                                .tooltip(text_tooltip("Page settings"))
                                .child(icon("Sliders.svg", 13., 0xe8e8e8))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, e: &MouseDownEvent, window, cx| {
                                        this.open_page_settings(e.position, window, cx)
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("page-next")
                                .w(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(if last { 0.35 } else { 1. })
                                .tooltip(text_tooltip("Next page (⌘→)"))
                                .child(icon("ChevronRight.svg", 13., 0xe8e8e8))
                                .on_click(cx.listener(|this, _, window, cx| this.page_step(1, window, cx))),
                        ),
                );
        } else {
            if t.part == Part::Structure {
                let unavailable = super::triggers::triggers_unavailable(t);
                let staged = t.triggers.dirty();
                bottom =
                    bottom
                        .child(
                            small_button("triggers", "Triggers")
                                .when(staged, |d| d.border_color(rgb(0xe0b84a)))
                                .when_some(unavailable, |d, why| {
                                    d.opacity(0.4).tooltip(text_tooltip(why))
                                })
                                .when(unavailable.is_none(), |d| {
                                    d.on_click(cx.listener(|this, _, window, cx| {
                                        this.open_triggers(window, cx)
                                    }))
                                }),
                        )
                        .child(
                            small_button("ddl-toggle", "DDL")
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_ddl(cx))),
                        );
            }
            bottom = bottom.child(div().flex_1().text_center().text_color(rgb(0xe0b84a)).when(
                t.buffer().dirty()
                    || t.is_new
                    || t.name != t.table.name
                    || (t.part == Part::Structure && t.triggers.dirty()),
                |d| d.child("unsaved changes — ⌘S to save"),
            ));
        }
        view.child(bottom)
    }
    fn toggle_ddl(&mut self, cx: &mut Context<Self>) {
        let Some(t) = &mut self.state.current_mut().table else {
            return;
        };
        t.show_ddl = !t.show_ddl;
        if !t.show_ddl || t.is_new || t.ddl.is_some() {
            cx.notify();
            return;
        }
        let table = t.table.clone();
        let id = self.state.active;
        let Some(session) = self.session.clone() else {
            return;
        };
        let task = self
            .services
            .runtime
            .spawn(async move { session.ddl(&table).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(Ok(ddl)) => {
                        if let Some(t) = this
                            .state
                            .tabs
                            .iter_mut()
                            .find(|t| t.id == id)
                            .and_then(|t| t.table.as_mut())
                        {
                            t.ddl = Some(ddl);
                        }
                    }
                    Ok(Err(e)) => this.failed(e, cx),
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
fn popup(id: impl Into<ElementId>, label: impl Into<SharedString>, width: f32) -> Stateful<Div> {
    small_button(id, label)
        .w(px(width))
        .h(px(28.))
        .justify_between()
        .child(icon("UpDown.svg", 12., 0xb8b8b8))
}
impl Workspace {
    pub(super) fn filter_keys(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(inputs) = self.table_inputs.get(&self.state.active) else {
            return false;
        };
        let Some(i) = inputs.filters.iter().position(|(a, b)| {
            a.read(cx).focus_handle(cx).is_focused(window)
                || b.read(cx).focus_handle(cx).is_focused(window)
        }) else {
            return false;
        };
        let values: Vec<Entity<InputState>> = inputs.filters.iter().map(|p| p.0.clone()).collect();
        let anchors = inputs.anchors.get(i).cloned().unwrap_or_default();
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if !mods.platform || mods.control || mods.alt {
            return false;
        }
        match (key, mods.shift) {
            ("enter", shift) => self.navigate_table(
                TableNavigation::Filters(if shift { Combine::Or } else { Combine::And }, None),
                window,
                cx,
            ),
            ("i", false) => self.clone_filter_rule(i, window, cx),
            ("i", true) => {
                let tab = self.state.active;
                if let Some(t) = self.table_of(tab) {
                    t.filters.remove(i);
                    if t.filters.is_empty() {
                        t.filters.push(new_filter());
                        t.applied.clear();
                    }
                }
                self.table_inputs.remove(&tab);
                self.prepare_table_inputs(window, cx);
                if let Some((input, _)) = self
                    .table_inputs
                    .get(&tab)
                    .and_then(|x| x.filter_input(i.saturating_sub(1)))
                {
                    input.update(cx, |s, cx| s.focus(window, cx));
                }
                self.queue_table_state(tab, cx);
            }
            ("up" | "down", false) => {
                let next = if key == "up" {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(values.len() - 1)
                };
                values[next].update(cx, |s, cx| s.focus(window, cx));
            }
            ("b", false) => self.toggle_filter_rule(i, cx),
            ("left", false) => self.filter_column_menu(i, anchors.column.get(), window, cx),
            ("right", false) => self.filter_op_menu(i, anchors.op.get(), window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }
}

impl Workspace {
    pub(super) fn table_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.table_inputs
            .get(&self.state.active)
            .is_some_and(|inputs| {
                [&inputs.name, &inputs.primary, &inputs.search]
                    .into_iter()
                    .chain(inputs.filters.iter().flat_map(|p| [&p.0, &p.1]))
                    .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            })
    }
}
