use super::*;
use telecat_app::table_model::{Part, TableEditor, new_filter};

pub(super) struct TableInputs {
    name: Entity<InputState>,
    primary: Entity<InputState>,
    pub(super) search: Entity<InputState>,
    filters: Vec<(Entity<InputState>, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}
#[derive(Clone)]
pub(super) enum TableNavigation {
    Page(u64, u64),
    Sort(Option<OrderBy>),
    Filters(Combine, Option<usize>),
    Clear,
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
                    text_input(f.value.clone(), "EMPTY", false, window, cx),
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
        self.table_inputs.insert(
            id,
            TableInputs {
                name,
                primary,
                search,
                filters,
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
                    Ok(Err(e)) => this.error(e, cx),
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
        match action {
            TableNavigation::Sort(sort) => {
                t.order_by = sort;
                tab.offset = 0;
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
        let mut rows = div()
            .flex_shrink_0()
            .px(px(10.))
            .py(px(8.))
            .bg(rgb(0x242424))
            .border_b_1()
            .border_color(rgb(0x111111))
            .flex()
            .flex_col()
            .gap(px(6.));
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
            let options = choices()["FILTER_OPS"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let label = options
                .iter()
                .find(|o| o["value"] == f.op.as_str())
                .and_then(|o| o["label"].as_str())
                .unwrap_or("Contains")
                .to_string();
            let row =
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id(("filter-enabled", i))
                            .size(px(13.))
                            .mx(px(3.))
                            .rounded(px(2.))
                            .bg(rgb(if f.enabled { 0x9bcaff } else { 0x383838 }))
                            .text_color(rgb(0x333333))
                            .child(if f.enabled { "✓" } else { "" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(t) = &mut this.state.current_mut().table {
                                    t.filters[i].enabled = !t.filters[i].enabled;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        popup(("filter-column", i), column.to_owned(), 180.).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                let Some(t) = &this.state.current().table else {
                                    return;
                                };
                                let mut items: Vec<_> = t
                                    .data
                                    .result
                                    .columns
                                    .iter()
                                    .map(|c| json!({"id":c,"label":c}))
                                    .collect();
                                items.extend([
                                    json!({"type":"separator"}),
                                    json!({"id":"","label":"Any column"}),
                                    json!({"id":RAW_SQL,"label":"Raw SQL"}),
                                ]);
                                native_menu::choose(
                                    json!(items),
                                    e.position,
                                    window,
                                    cx,
                                    move |this, id, _, cx| {
                                        if let Some(t) = &mut this.state.current_mut().table {
                                            t.filters[i].column = id;
                                        }
                                        cx.notify();
                                    },
                                );
                            }),
                        ),
                    )
                    .when(!raw, |d| {
                        d.child(popup(("filter-op", i), label, 140.).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, e: &MouseDownEvent, window, cx| {
                                let items: Vec<_> = choices()["FILTER_OPS"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| json!({"id":v["value"],"label":v["label"]}))
                                    .collect();
                                native_menu::choose(
                                    json!(items),
                                    e.position,
                                    window,
                                    cx,
                                    move |this, id, _, cx| {
                                        if let (Some(t), Ok(op)) = (
                                            &mut this.state.current_mut().table,
                                            serde_json::from_value(json!(id)),
                                        ) {
                                            t.filters[i].op = op;
                                        }
                                        cx.notify();
                                    },
                                );
                            }),
                        ))
                    })
                    .child(
                        input_box(
                            &inputs.filters[i].0,
                            !raw && matches!(f.op, FilterOp::IsNull | FilterOp::IsNotNull),
                            (self.scenario == "08-table-filter" && i == 0)
                                || (self.scenario == "52-filter" && i == 1),
                        )
                        .h(px(28.))
                        .text_size(px(13.)),
                    )
                    .when(between && !raw, |d| {
                        d.child("and").child(
                            input_box(&inputs.filters[i].1, false, false)
                                .h(px(28.))
                                .text_size(px(13.)),
                        )
                    })
                    .child(
                        small_button(("apply-filter", i), "Apply").on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.navigate_table(
                                    TableNavigation::Filters(Combine::And, Some(i)),
                                    window,
                                    cx,
                                )
                            },
                        )),
                    )
                    .child(small_button(("remove-filter", i), "−").w(px(30.)).on_click(
                        cx.listener(move |this, _, window, cx| {
                            if let Some(t) = &mut this.state.current_mut().table {
                                t.filters.remove(i);
                                if t.filters.is_empty() {
                                    this.navigate_table(TableNavigation::Clear, window, cx);
                                }
                            }
                            this.table_inputs.remove(&this.state.active);
                            cx.notify();
                        }),
                    ))
                    .child(small_button(("insert-filter", i), "+").w(px(30.)).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if let Some(t) = &mut this.state.current_mut().table {
                                t.filters.insert(i + 1, new_filter());
                            }
                            this.table_inputs.remove(&this.state.active);
                            cx.notify();
                        }),
                    ));
            rows = rows.child(row);
        }
        rows.child(div().flex().items_center().gap(px(8.))
            .child(icon("Sliders.svg",16.,0x8a8a8a))
            .child(small_button("filter-export","Export").on_click(cx.listener(|this,_,_,cx|this.export_result(cx))))
            .child(small_button("filter-sql","SQL").on_click(cx.listener(|this,_,_,cx|this.filter_sql(cx))))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(rgb(telecat_theme::MUTED)).child("Show: ⌘F  Insert: ⌘I  Remove: ⇧⌘I  Apply All: ⌘Return  Up: ⌘↑  Down: ⌘↓"))
            .child(div().flex_1())
            .child(small_button("filter-clear","Clear").on_click(cx.listener(|this,_,window,cx|this.navigate_table(TableNavigation::Clear,window,cx))))
            .child(div().flex().h(px(28.)).border_1().border_color(rgb(0x3c3c3c)).rounded(px(7.)).bg(rgb(telecat_theme::INPUT_2))
                .child(div().id("filter-apply-all").px(px(12.)).flex().items_center().child("Apply All").on_click(cx.listener(|this,_,window,cx|this.navigate_table(TableNavigation::Filters(Combine::And,None),window,cx))))
                .child(div().id("filter-combine").px(px(7.)).border_l_1().border_color(rgba(0xffffff18)).flex().items_center().child(icon("ChevronDown.svg",13.,0xe8e8e8)).on_mouse_down(MouseButton::Left,cx.listener(|_,e:&MouseDownEvent,window,cx|native_menu::choose(json!([{"id":"AND","label":"Apply All Checked Filters with AND - Default    ⌘↵"},{"id":"OR","label":"Apply All Checked Filters with OR    ⇧⌘↵"}]),e.position,window,cx,|this,id,window,cx|this.navigate_table(TableNavigation::Filters(if id == "OR" {Combine::Or} else {Combine::And},None),window,cx)))))))
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
                Ok(Err(e)) => this.error(e, cx),
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
                    .child(div().text_size(px(14.)).ml(px(6.)).child("Name"))
                    .child(if structure {
                        input_box(&inputs.name, self.saving, false)
                            .flex_none()
                            .w(px(280.))
                            .font(mono())
                            .text_size(px(13.))
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
                            .text_size(px(14.))
                            .child(t.name.clone())
                    })
                    .when(structure, |d| {
                        d.child(div().text_size(px(14.)).ml(px(6.)).child("Primary"))
                            .child(if t.is_new {
                                input_box(&inputs.primary, self.saving, false)
                                    .font(mono())
                                    .text_size(px(12.5))
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
                                            .text_size(px(12.5))
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
                            .text_size(px(12.5))
                            .line_height(px(20.))
                            .child(if t.is_new {
                                "-- save the table first (⌘S)".into()
                            } else {
                                t.ddl.clone().unwrap_or("Loading…".into())
                            }),
                    )
                    .child(super::layout::scrollbar(&self.ddl_scroll, cx)),
            );
        } else {
            view = view.child(self.grid_view());
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
                        div()
                            .id(label)
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
        bottom = bottom.child(
            small_button("table-add", "")
                .gap(px(6.))
                .child(icon("Plus.svg", 13., 0xe8e8e8))
                .child(match t.part {
                    Part::Data => "Row",
                    Part::Structure => "Column",
                    Part::Index => "Index",
                })
                .on_click(cx.listener(|this, _, window, cx| this.add_grid_row(window, cx))),
        );
        if t.part == Part::Data {
            let mut counter = format!(
                "{}-{} of {} rows",
                super::panels::grouped(if tab.total == 0 { 0 } else { tab.offset + 1 }),
                super::panels::grouped((tab.offset + tab.limit).min(tab.total)),
                super::panels::grouped(tab.total)
            );
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
            bottom = bottom.child(div().flex_1().min_w_0().truncate().text_center().text_color(rgb(telecat_theme::MUTED)).child(counter))
                .child(small_button("filters","").gap(px(6.)).child(icon("Search.svg",13.,0xe8e8e8)).child("Filters").when(t.show_filters,|d|d.bg(rgb(telecat_theme::ACCENT)).border_color(rgb(telecat_theme::ACCENT))).on_click(cx.listener(|this,_,_,cx|{if let Some(t)=&mut this.state.current_mut().table{t.show_filters = !t.show_filters;}cx.notify();})))
                .when(t.column_filter.is_some(),|d|d.child(div().w(px(180.)).child(input_box(&inputs.search,false,true))))
                .child(small_button("columns","Columns").on_mouse_down(MouseButton::Left,cx.listener(|this,e:&MouseDownEvent,window,cx| {
                    let Some(t)=&this.state.current().table else{return;};let mut items=vec![json!({"id":"all","label":"Show All Columns"})];items.extend(t.data.result.columns.iter().enumerate().map(|(i,c)|json!({"id":i.to_string(),"label":c,"checked":!t.hidden.contains(c)})));
                    native_menu::choose(json!(items),e.position,window,cx,|this,id,_,cx|{if let Some(t)=&mut this.state.current_mut().table{if id=="all"{t.hidden.clear();}else if let Some(c)=id.parse::<usize>().ok().and_then(|i|t.data.result.columns.get(i)).cloned(){if !t.hidden.remove(&c){t.hidden.insert(c);}}}cx.notify();});
                })))
                .child(nav("page-prev","ChevronLeft.svg",tab.offset==0).on_click(cx.listener(|this,_,window,cx|{let t=this.state.current();this.navigate_table(TableNavigation::Page(t.offset.saturating_sub(t.limit),t.limit),window,cx);})))
                .child(popup("page-limit",tab.limit.to_string(),90.).on_mouse_down(MouseButton::Left,cx.listener(|_,e:&MouseDownEvent,window,cx|native_menu::choose(json!([100,300,500,1000,5000].map(|n|json!({"id":n.to_string(),"label":format!("{n} rows / page")}))),e.position,window,cx,|this,id,window,cx|{if let Ok(limit)=id.parse(){this.navigate_table(TableNavigation::Page(0,limit),window,cx);}}))))
                .child(nav("page-next","ChevronRight.svg",tab.offset+tab.limit>=tab.total).on_click(cx.listener(|this,_,window,cx|{let t=this.state.current();if t.offset+t.limit<t.total{this.navigate_table(TableNavigation::Page(t.offset+t.limit,t.limit),window,cx);}})));
        } else {
            if t.part == Part::Structure {
                bottom = bottom
                    .child(small_button("triggers", "Triggers").opacity(0.4))
                    .child(
                        small_button("ddl-toggle", "DDL")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_ddl(cx))),
                    );
            }
            bottom = bottom.child(div().flex_1().text_center().text_color(rgb(0xe0b84a)).when(
                t.buffer().dirty() || t.is_new || t.name != t.table.name,
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
                    Ok(Err(e)) => this.error(e, cx),
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
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if key == "enter" && mods.platform {
            self.navigate_table(
                TableNavigation::Filters(
                    if mods.shift {
                        Combine::Or
                    } else {
                        Combine::And
                    },
                    None,
                ),
                window,
                cx,
            );
            return true;
        }
        if key == "i" && mods.platform {
            if let Some(t) = &mut self.state.current_mut().table {
                if mods.shift {
                    t.filters.remove(i);
                    if t.filters.is_empty() {
                        t.filters.push(new_filter());
                        t.applied.clear();
                    }
                } else {
                    t.filters.insert(i + 1, new_filter());
                }
            }
            self.table_inputs.remove(&self.state.active);
            self.prepare_table_inputs(window, cx);
            let inputs = &self.table_inputs[&self.state.active];
            let next = if mods.shift {
                i.saturating_sub(1)
            } else {
                i + 1
            };
            if let Some((input, _)) = inputs.filters.get(next) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            cx.notify();
            return true;
        }
        if mods.platform && matches!(key, "up" | "down") {
            let next = if key == "up" {
                i.saturating_sub(1)
            } else {
                (i + 1).min(inputs.filters.len() - 1)
            };
            inputs.filters[next]
                .0
                .update(cx, |s, cx| s.focus(window, cx));
            return true;
        }
        false
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
