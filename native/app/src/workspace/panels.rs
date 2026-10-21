use super::layout::scrollbar;
use super::*;
impl Workspace {
    pub(super) fn sidebar_view(&self, cx: &mut Context<Self>) -> Div {
        let query = self.search.read(cx).value().to_lowercase();
        let mut tree_height = 10.;
        let mut tree = div()
            .id("tree")
            .track_focus(&self.tree_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .track_scroll(&self.tree_scroll)
            .px(px(6.))
            .pb(px(10.))
            .font(mono())
            .text_size(px(13.))
            .line_height(px(16.));
        match self.segment {
            Segment::Items => {
                for (label, kind, path) in [
                    ("Functions", ObjectKind::Function, "Function.svg"),
                    ("Procedures", ObjectKind::Procedure, "Function.svg"),
                    ("Tables", ObjectKind::Table, "Table.svg"),
                    ("Views", ObjectKind::View, "View.svg"),
                ] {
                    let objects: Vec<_> = self
                        .state
                        .objects
                        .iter()
                        .filter(|o| {
                            o.kind == kind
                                && display_name(&o.table, self.state.connection.driver)
                                    .to_lowercase()
                                    .contains(&query)
                        })
                        .collect();
                    if objects.is_empty() {
                        continue;
                    }
                    let expanded = !self.collapsed.contains(label) || !query.is_empty();
                    tree_height += 24.;
                    tree = tree.child(
                        div()
                            .id(label)
                            .h(px(24.))
                            .flex_shrink_0()
                            .px(px(2.))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(px(13.5))
                            .child(icon(
                                if expanded {
                                    "ChevronDown.svg"
                                } else {
                                    "ChevronRight.svg"
                                },
                                13.,
                                0xe8e8e8,
                            ))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.collapsed.remove(label) {
                                    this.collapsed.insert(label);
                                }
                                cx.notify();
                            })),
                    );
                    if expanded {
                        for object in objects {
                            tree_height += 25.;
                            let table = object.table.clone();
                            let menu_object = object.clone();
                            let focus_object = object.clone();
                            let expand_table = table.clone();
                            let expanded_columns =
                                self.tree_state.columns.get(&sidebar::table_key(&table));
                            let status = self.table_status(&table);
                            let selected =
                                self.state.current().kind == TabKind::Table(table.clone());
                            let staged = status.is_some();
                            tree = tree.child(
                                div()
                                    .id(SharedString::from(format!(
                                        "object-{}-{}",
                                        table.schema, table.name
                                    )))
                                    .h(px(25.))
                                    .flex_shrink_0()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .pl(px(6.))
                                    .pr(px(4.))
                                    .rounded(px(5.))
                                    .tooltip(text_tooltip(display_name(
                                        &table,
                                        self.state.connection.driver,
                                    )))
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .when(selected, |d| {
                                        d.bg(rgb(telecat_theme::ACCENT)).text_color(rgb(0xffffff))
                                    })
                                    .when(staged, |d| {
                                        d.bg(rgb(match status {
                                            Some("new") => 0x2d5724,
                                            Some("drop" | "truncate") => 0x5b1616,
                                            _ => 0x6b5518,
                                        }))
                                    })
                                    .when(staged && selected, |d| {
                                        d.shadow(vec![BoxShadow {
                                            inset: true,
                                            color: rgb(telecat_theme::ACCENT).into(),
                                            offset: point(px(0.), px(0.)),
                                            blur_radius: px(0.),
                                            spread_radius: px(2.),
                                        }])
                                    })
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "expand-{}",
                                                sidebar::table_key(&table)
                                            )))
                                            .child(icon(
                                                if expanded_columns.is_some() {
                                                    "ChevronDown.svg"
                                                } else {
                                                    "ChevronRight.svg"
                                                },
                                                13.,
                                                if selected {
                                                    0xffffff
                                                } else {
                                                    telecat_theme::MUTED
                                                },
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_tree_columns(expand_table.clone(), cx);
                                            })),
                                    )
                                    .child(icon(
                                        path,
                                        14.,
                                        if selected { 0xffffff } else { 0x6fa0e8 },
                                    ))
                                    .child(
                                        div().flex_1().min_w_0().truncate().child(display_name(
                                            &table,
                                            self.state.connection.driver,
                                        )),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.object_menu(
                                                menu_object.clone(),
                                                e.position,
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.tree_state.focused = Some(focus_object.clone());
                                        window.focus(&this.tree_focus, cx);
                                        if matches!(
                                            focus_object.kind,
                                            ObjectKind::Table | ObjectKind::View
                                        ) {
                                            this.open_table(table.clone(), cx);
                                        }
                                    })),
                            );
                            if let Some(columns) = expanded_columns {
                                if let Some(columns) = columns {
                                    for (name, ty) in columns {
                                        let copy = name.clone();
                                        tree_height += 20.;
                                        tree = tree.child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "col-{}-{name}",
                                                    sidebar::table_key(&object.table)
                                                )))
                                                .h(px(20.))
                                                .min_w_0()
                                                .overflow_hidden()
                                                .pl(px(44.))
                                                .pr(px(8.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(10.))
                                                .text_size(px(12.))
                                                .tooltip(text_tooltip(format!("{name}  {ty}")))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .truncate()
                                                        .child(name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .flex_shrink_1()
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_color(rgb(telecat_theme::MUTED))
                                                        .child(ty.clone()),
                                                )
                                                .on_click(cx.listener(
                                                    move |_, e: &ClickEvent, _, cx| {
                                                        if e.click_count() == 2 {
                                                            cx.write_to_clipboard(
                                                                ClipboardItem::new_string(
                                                                    copy.clone(),
                                                                ),
                                                            );
                                                        }
                                                    },
                                                )),
                                        );
                                    }
                                } else {
                                    tree_height += 20.;
                                    tree = tree.child(
                                        div()
                                            .h(px(20.))
                                            .pl(px(44.))
                                            .text_color(rgb(telecat_theme::MUTED))
                                            .child("loading…"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Segment::Queries => {
                for q in self.queries.iter().filter(|q| {
                    q.name.to_lowercase().contains(&query) || q.sql.to_lowercase().contains(&query)
                }) {
                    let sql = q.sql.clone();
                    let title = q.name.clone();
                    let menu_query = q.clone();
                    tree = tree.child(
                        div()
                            .id(SharedString::from(q.id.clone()))
                            .h(px(25.))
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .px(px(6.))
                            .font(ui_font())
                            .tooltip(text_tooltip(q.name.clone()))
                            .child(div().flex_1().min_w_0().truncate().child(q.name.clone()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_named_query(&sql, &title, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                    this.query_menu(menu_query.clone(), e.position, window, cx)
                                }),
                            ),
                    );
                }
                if self.queries.is_empty() {
                    tree = tree.child(
                        div()
                            .p(px(20.))
                            .font(ui_font())
                            .text_color(rgb(telecat_theme::MUTED))
                            .child("No saved queries"),
                    );
                }
            }
            Segment::History => {
                for (i, entry) in self
                    .history
                    .iter()
                    .filter(|h| h.sql.to_lowercase().contains(&query))
                    .enumerate()
                {
                    let sql = entry.sql.clone();
                    tree = tree.child(
                        div()
                            .id(("history", i))
                            .min_w_0()
                            .overflow_hidden()
                            .px(px(8.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(rgba(0xffffff08))
                            .child(
                                div()
                                    .font(ui_font())
                                    .text_size(px(11.))
                                    .text_color(rgb(telecat_theme::MUTED))
                                    .child(entry.time.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_clamp(3)
                                    .overflow_hidden()
                                    .child(entry.sql.clone()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_history(&sql, window, cx)
                            })),
                    );
                }
                if !self.history.is_empty() {
                    tree = tree.child(small_button("clear-history", "Clear history").on_click(
                        cx.listener(|this, _, _, cx| {
                            match this
                                .services
                                .profile
                                .clear_history(&this.state.connection.id)
                            {
                                Ok(()) => this.history.clear(),
                                Err(e) => this.error(e, cx),
                            }
                            cx.notify();
                        }),
                    ));
                }
            }
        }
        let overflow = if self.segment == Segment::Items {
            tree_height > self.viewport_height - 168.
        } else {
            self.tree_scroll.max_offset().y > px(0.)
        };
        tree = tree.pr(px(if overflow { 16. } else { 6. }));
        // The three segments share the width and never overflow it; the sidebar's minimum width
        // (layout::SIDEBAR_MIN) keeps every label whole.
        let segment_tabs = [
            (Segment::Items, "Items"),
            (Segment::Queries, "Queries"),
            (Segment::History, "History"),
        ]
        .into_iter()
        .map(|(segment, label)| {
            div()
                .id(label)
                .flex_1()
                .min_w_0()
                .max_w(px(96.))
                .flex()
                .justify_center()
                .px(px(6.))
                .py(px(4.))
                .rounded(px(6.))
                .text_size(px(13.5))
                .line_height(px(16.))
                .text_color(rgb(if self.segment == segment {
                    0xe8e8e8
                } else {
                    0xb8b8b8
                }))
                .when(self.segment == segment, |d| d.bg(rgba(0xffffff16)))
                .child(div().min_w_0().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.segment = segment;
                    cx.notify();
                }))
        });
        let items_state = (self.segment == Segment::Items).then(|| {
            self.state
                .objects_load
                .placeholder(!self.state.objects.is_empty(), std::time::Instant::now())
        });
        let tree_dimmed = matches!(items_state, Some(Placeholder::Loading { dimmed: true }));
        let tree_state = items_state.as_ref().and_then(|p| match p {
            Placeholder::Empty => Some(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font(ui_font())
                    .text_color(rgb(telecat_theme::MUTED))
                    .child("No tables or views"),
            ),
            p => load_state_view(p, "items-state"),
        });
        div().w(px(self.layout.sidebar)).flex_shrink_0().h_full().overflow_hidden().bg(rgb(telecat_theme::PANEL)).flex().flex_col()
            .child(div().h(px(40.)).flex_shrink_0().min_w_0().overflow_hidden().pt(px(10.)).pb(px(6.)).px(px(8.)).flex().justify_center().gap(px(4.)).children(segment_tabs))
            .child(search_box(&self.search,28.).mx(px(8.)).mt(px(4.)).mb(px(8.)))
            .child(div().id("items-area").flex_1().min_h_0().min_w_0().relative().flex().child(tree.when(tree_dimmed, |d| d.opacity(0.35))).child(scrollbar(&self.tree_scroll, cx)).children(tree_state)
                .when(self.segment == Segment::Items, |d| d.on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, window, cx| this.items_menu(e.position, window, cx)))))
            .child(div().h(px(34.)).flex_shrink_0().border_t_1().border_color(rgb(0x111111)).px(px(8.)).flex().items_center().gap(px(4.)).child(nav("side-add","Plus.svg",false).on_click(cx.listener(|this,_,_,cx|this.new_table(cx)))).child(nav("side-menu","ChevronDown.svg",false).on_mouse_down(MouseButton::Left,cx.listener(|_this,e:&MouseDownEvent,window,cx|{native_menu::choose(json!([{"id":"table","label":"New Table"},{"id":"query","label":"New SQL Query"},{"type":"separator"},{"id":"import","label":"Import SQL Dump…"}]),e.position,window,cx,|this,id,_,cx|{if id=="query"{this.new_query("",cx);}else if id=="table"{this.new_table(cx);}else if id=="import"{this.import_dump_file(cx);}else{this.sheet=Some(Sheet::Shortcuts);cx.notify();}});}))))
    }
    pub(super) fn right_panel(&self, cx: &mut Context<Self>) -> Div {
        let tab = self.state.current();
        let query = self.field_search.read(cx).value().to_lowercase();
        let selected = if let Some(t) = &tab.table {
            if t.part == telecat_app::table_model::Part::Data {
                tab.grid
                    .selected_row
                    .and_then(|r| t.data.rows.get(r))
                    .map(|row| {
                        (
                            t.data.result.columns.clone(),
                            row.cur
                                .iter()
                                .map(|v| v.value().map(str::to_owned))
                                .collect::<Vec<_>>(),
                        )
                    })
            } else {
                None
            }
        } else {
            tab.grid
                .selected_row
                .and_then(|r| tab.grid.result.rows.get(r))
                .map(|row| (tab.grid.result.columns.clone(), row.clone()))
        };
        let fields: Vec<(String, Option<String>)> = if let Some((columns, row)) = selected {
            columns.into_iter().zip(row).collect()
        } else if let Some(info) = &tab.info {
            vec![
                ("data_size".into(), Some(info.data_size.clone())),
                ("index_size".into(), Some(info.index_size.clone())),
                ("total_size".into(), Some(info.total_size.clone())),
                ("comment".into(), Some(info.comment.clone())),
            ]
        } else {
            vec![]
        };
        let empty = fields.is_empty();
        let editable_fields = self.inspector_fields(&query, cx);
        div()
            .w(px(self.layout.panel))
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .bg(rgb(telecat_theme::PANEL))
            .flex()
            .flex_col()
            .child(
                div()
                    .pt(px(8.))
                    .pb(px(4.))
                    .pr(px(10.))
                    .flex()
                    .gap(px(6.))
                    .items_center()
                    .child(
                        search_box(&self.field_search, 30.)
                            .ml(px(10.))
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(icon("Sliders.svg", 16., 0x8a8a8a)),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(16.))
                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                        .child("No row selected"),
                )
            })
            .when(!empty, |d| {
                d.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(
                            div()
                                .id("fields")
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .overflow_y_scroll()
                                .overflow_x_hidden()
                                .track_scroll(&self.panel_scroll)
                                .px(px(10.))
                                .pt(px(4.))
                                .pb(px(16.))
                                .child(editable_fields.unwrap_or_else(|| {
                                    div().children(
                                        fields
                                            .into_iter()
                                            .filter(|(label, _)| {
                                                label.to_lowercase().contains(&query)
                                            })
                                            .map(|(label, value)| {
                                                let muted =
                                                    value.as_ref().is_none_or(|s| s.is_empty());
                                                let multi = value.as_ref().is_some_and(|s| {
                                                    s.len() > 60 || s.contains('\n')
                                                });
                                                div()
                                                    .mb(px(8.))
                                                    .child(
                                                        div()
                                                            .font(mono())
                                                            .text_size(px(12.5))
                                                            .line_height(px(15.))
                                                            .text_color(rgb(
                                                                telecat_theme::TEXT_SECONDARY,
                                                            ))
                                                            .mb(px(3.))
                                                            .truncate()
                                                            .child(label),
                                                    )
                                                    .child(
                                                        div()
                                                            .h(px(if multi { 90. } else { 30. }))
                                                            .w_full()
                                                            .overflow_hidden()
                                                            .px(px(8.))
                                                            .py(px(5.))
                                                            .border_1()
                                                            .border_color(rgb(
                                                                telecat_theme::BORDER,
                                                            ))
                                                            .rounded(px(6.))
                                                            .bg(rgb(telecat_theme::INPUT))
                                                            .font(mono())
                                                            .text_size(px(13.))
                                                            .text_color(rgb(if muted {
                                                                0x666666
                                                            } else {
                                                                0xe8e8e8
                                                            }))
                                                            .child(
                                                                value
                                                                    .map(|s| {
                                                                        if s.is_empty() {
                                                                            "EMPTY".into()
                                                                        } else {
                                                                            super::inspector::preview(&s)
                                                                                .into_owned()
                                                                        }
                                                                    })
                                                                    .unwrap_or("NULL".into()),
                                                            ),
                                                    )
                                            }),
                                    )
                                })),
                        )
                        .child(scrollbar(&self.panel_scroll, cx)),
                )
            })
    }
    pub(super) fn rail(&self, cx: &mut Context<Self>) -> Div {
        div()
            .w(px(84.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(telecat_theme::INPUT))
            .border_r_1()
            .border_color(rgb(0x111111))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(54.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(rgb(telecat_theme::CARD)),
            )
            .children(
                [("zixma", false), ("main_db", true)]
                    .into_iter()
                    .map(|(label, active)| {
                        div()
                            .h(px(84.))
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(rgb(telecat_theme::CARD))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(8.))
                            .when(active, |d| d.bg(rgb(0x262626)))
                            .child(icon(
                                "Database.svg",
                                26.,
                                if active { 0x3a7d44 } else { 0x8a8a8a },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(if active { 0xe8e8e8 } else { 0xb8b8b8 }))
                                    .child(label),
                            )
                    }),
            )
            .child(
                div().flex().justify_center().mt(px(10.)).child(
                    div()
                        .id("rail-add")
                        .size(px(30.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgba(0xffffff1c))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("Plus.svg", 16., 0xb8b8b8))
                        .on_click(cx.listener(|this, _, _, cx| {
                            crate::show_welcome(this.services.clone(), false, cx)
                        })),
                ),
            )
    }
    pub(super) fn console_view(&self, cx: &mut Context<Self>) -> Div {
        if self.console_log_count.replace(self.state.logs.len()) != self.state.logs.len() {
            self.console_scroll.scroll_to_bottom();
        }
        let mut lines = div()
            .id("console-log")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.console_scroll)
            .pl(px(10.))
            .pr(px(20.))
            .py(px(6.))
            .font(mono())
            .text_size(px(12.5))
            .line_height(px(19.375));
        for log in &self.state.logs {
            lines = lines.child(
                div()
                    // Taffy excludes the last child's bottom margin from the
                    // scroll extent. Padding preserves Electron's final 10 px.
                    .pb(px(10.))
                    .child(
                        div()
                            .text_color(rgb(telecat_theme::MUTED))
                            .child(format!("-- {}", log.time)),
                    )
                    .child(div().child(highlight_sql(&log.sql, self.highlight))),
            );
        }
        div()
            .h(px(self.layout.console))
            .flex_shrink_0()
            .bg(rgb(0x1f1f1f))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(lines)
                    .child(scrollbar(&self.console_scroll, cx)),
            )
            .child(
                div()
                    .h(px(34.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(rgb(0x111111))
                    .bg(rgb(0x242424))
                    .child(
                        nav("clear-console", "Trash.svg", false)
                            .w(px(28.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.logs.clear();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("highlight")
                            .pl(px(4.))
                            .flex()
                            .gap(px(6.))
                            .items_center()
                            .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                            .child(
                                div()
                                    .size(px(13.))
                                    .rounded(px(2.))
                                    .border_1()
                                    .border_color(rgb(0x888888))
                                    .bg(rgb(if self.highlight { 0x2f6fed } else { 0x383838 }))
                                    .when(self.highlight, |d| d.child("✓")),
                            )
                            .child("Enable syntax highlighting")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.highlight = !this.highlight;
                                cx.notify();
                            })),
                    ),
            )
    }
    pub(super) fn content(&self, cx: &mut Context<Self>) -> Div {
        if self.state.current().table.is_some() {
            return self.table_content(cx);
        }
        let tab = self.state.current();
        let query = tab.kind == TabKind::Query;
        let running = tab.kind == TabKind::Running;
        let mut content = div().flex_1().min_h_0().relative().flex().flex_col();
        if query {
            content=content.child(div().h(relative(self.layout.query_split(tab.id))).flex_shrink_0().flex().flex_col()
                .child(self.editor_view())
                .child(div().h(px(40.)).flex_shrink_0().flex().items_center().gap(px(8.)).px(px(10.)).font(mono()).text_size(px(12.5)).text_color(rgb(telecat_theme::MUTED)).child(icon("Sliders.svg",15.,0x8a8a8a)).child(format!("{} lines, {} characters",tab.editor.text.split('\n').count(),if self.scenario=="c1-lock"{50}else{tab.editor.text.encode_utf16().count()})).child(div().flex_1())
                .child(small_button("query-limit",format!("Limit {}",tab.limit)).h(px(28.)).font(ui_font()).gap(px(6.)).child(icon("UpDown.svg",12.,0xb8b8b8)).on_mouse_down(MouseButton::Left,cx.listener(|this,e:&MouseDownEvent,window,cx|{let items:Vec<_>=[0,100,300,500,1000,5000].iter().map(|n|json!({"id":n.to_string(),"label":if *n==0{"No limit".into()}else{format!("Limit {n}")},"checked":*n==this.state.current().limit})).collect();native_menu::choose(json!(items),e.position,window,cx,|this,id,_,cx|{if let Ok(n)=id.parse(){this.state.current_mut().limit=n;cx.notify();}});})))
                .child(small_button("beautify","Beautify ⌘I").font(ui_font()).on_click(cx.listener(|this,_,window,cx|this.format_editor(false,window,cx))))
                .child(div().h(px(28.)).flex_shrink_0().rounded(px(7.)).border_1().border_color(rgb(0x3c3c3c)).bg(rgb(telecat_theme::INPUT_2)).flex().items_center().font(ui_font()).text_size(px(13.)).text_color(rgb(telecat_theme::TEXT))
                    .child(div().id("run-current").px(px(12.)).h_full().flex().items_center().child(if self.busy{"Cancel"}else{"Run Current ⌘↵"}).on_click(cx.listener(|this,_,window,cx|{if this.busy{this.cancel(cx);}else{this.run(false,window,cx);}})))
                    .child(div().id("run-menu").px(px(7.)).h_full().flex().items_center().border_l_1().border_color(rgba(0xffffff18)).child(icon("ChevronDown.svg",13.,0xe8e8e8)).on_mouse_down(MouseButton::Left,cx.listener(|_this,e:&MouseDownEvent,window,cx|native_menu::choose(json!([{"id":"current","label":"Run Current  ⌘↵"},{"id":"all","label":"Run All  ⇧⌘↵"}]),e.position,window,cx,|this,id,window,cx|this.run(id=="all",window,cx))))))))
                .child(self.splitter(Splitter::Query(tab.id), cx))
                .child(self.query_measure(cx));
        }
        if running {
            content = content.child(
                div()
                    .h(px(46.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .bg(rgb(0x242424))
                    .border_b_1()
                    .border_color(rgb(0x111111))
                    .child("Database")
                    .child(
                        small_button(
                            "process-db",
                            self.running
                                .database
                                .clone()
                                .unwrap_or("All databases".into()),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, e: &MouseDownEvent, window, cx| {
                                this.process_database_menu(e.position, window, cx)
                            }),
                        )
                        .h(px(28.))
                        .w(px(180.))
                        .justify_between()
                        .child(icon("UpDown.svg", 12., 0xb8b8b8)),
                    )
                    .child(
                        small_button("process-every", running::interval_label(self.running.every))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                                    this.process_interval_menu(e.position, window, cx)
                                }),
                            )
                            .w(px(190.))
                            .h(px(28.))
                            .justify_between()
                            .child(icon("UpDown.svg", 12., 0xb8b8b8)),
                    )
                    .child(
                        small_button("process-refresh", "")
                            .gap(px(6.))
                            .child(icon("Refresh.svg", 13., 0xe8e8e8))
                            .child("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_tab(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        small_button("kill-query", "Kill Query")
                            .opacity(if self.selected_process_ids(cx).is_empty() {
                                0.4
                            } else {
                                1.
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.kill_processes(false, window, cx)
                            })),
                    )
                    .child(
                        small_button("kill-connection", "Kill Connection")
                            .text_color(rgb(telecat_theme::DANGER))
                            .opacity(if self.selected_process_ids(cx).is_empty() {
                                0.4
                            } else {
                                1.
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.kill_processes(true, window, cx)
                            })),
                    ),
            );
        }
        if tab.message && query {
            let locations: Vec<Location> = tab.error_locations.iter().flatten().copied().collect();
            // Locations are offsets into the SQL as it ran: no jumping once it was edited.
            let stale = tab.run_text != tab.editor.text;
            let text = self.messages.get(&tab.id).map(|m| m.1.clone());
            content = content.child(
                div()
                    .id("messages")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(14.))
                            .pt(px(6.))
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .items_center()
                            .font(ui_font())
                            .text_size(px(12.))
                            .children(locations.into_iter().enumerate().map(|(i, at)| {
                                div()
                                    .id(("error-location", i))
                                    .px(px(8.))
                                    .py(px(2.))
                                    .rounded(px(5.))
                                    .border_1()
                                    .border_color(rgb(0x6b2b27))
                                    .text_color(rgb(0xff8a80))
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgba(0xff8a8018)))
                                    .when(stale, |d| d.opacity(0.5).cursor_default())
                                    .child(if stale {
                                        format!(
                                            "Line {}, column {} (SQL changed since run)",
                                            at.line, at.column
                                        )
                                    } else {
                                        format!("Go to line {}, column {}", at.line, at.column)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.jump_to_location(at, window, cx)
                                    }))
                            }))
                            .child(div().flex_1())
                            .child(small_button("copy-messages", "Copy").on_click(cx.listener(
                                |this, _, _, cx| {
                                    let text = this.state.current().messages_text();
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                },
                            ))),
                    )
                    .child(
                        div()
                            .id("messages-text")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(14.))
                            .py(px(8.))
                            .font(mono())
                            .text_size(px(12.5))
                            .line_height(px(19.))
                            .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                            .children(text.map(|state| Textarea::new(&state))),
                    ),
            );
        } else {
            content = content.child(self.grid_view(cx));
        }
        let mut bottom = subbar();
        if query {
            bottom = bottom.child(segments(
                &["Data", "Message", "Chart"],
                if tab.message { 1 } else { 0 },
                true,
                cx,
            ));
            let count = tab
                .results
                .iter()
                .filter(|r| !r.columns.is_empty() && r.error.is_none())
                .count();
            if count > 1 {
                bottom=bottom.child(small_button("result",format!("Result {}",tab.shown+1)).gap(px(6.)).child(icon("UpDown.svg",12.,0xb8b8b8)).on_mouse_down(MouseButton::Left,cx.listener(move|this,e:&MouseDownEvent,window,cx|{let items:Vec<_>=(0..count).map(|i|json!({"id":i.to_string(),"label":format!("Result {}",i+1),"checked":i==this.state.current().shown})).collect();native_menu::choose(json!(items),e.position,window,cx,|this,id,_,cx|{if let Ok(i)=id.parse(){this.state.current_mut().select_result(i);cx.notify();}});})));
            }
            if !tab.results.is_empty() {
                bottom = bottom.child(div().text_color(rgb(telecat_theme::MUTED)).child(format!(
                    "{} ms",
                    tab.results.iter().map(|r| r.duration_ms).sum::<u64>()
                )));
            }
            if let Some(re) = &tab.result_edit {
                let editable = re.plan.read_only.is_none();
                let label = if re.data.dirty() {
                    "Unsaved changes · ⌘S save · Esc discard · SQL locked".to_owned()
                } else {
                    re.plan.badge()
                };
                bottom = bottom.child(
                    div()
                        .id("result-badge")
                        .max_w(px(360.))
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(rgb(if editable { 0x2f5a2a } else { 0x3a3a3a }))
                        .text_color(rgb(if editable {
                            0x8fd18a
                        } else {
                            telecat_theme::MUTED
                        }))
                        .truncate()
                        .tooltip(text_tooltip(re.plan.badge()))
                        .child(label),
                );
            }
            bottom = bottom
                .child(
                    div()
                        .flex_1()
                        .text_center()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(if tab.grid.result.columns.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "{}{} rows",
                                self.visible_row_count(cx),
                                if self.row_search_view().is_some() {
                                    format!(" of {}", tab.grid.result.rows.len())
                                } else {
                                    String::new()
                                }
                            )
                        }),
                )
                .children(self.row_search_view())
                .child(nav("row-search", "Search.svg", false).on_click(
                    cx.listener(|this, _, window, cx| this.toggle_row_search(window, cx)),
                ))
                .child(
                    small_button("export", "Export...")
                        .on_click(cx.listener(|this, _, _, cx| this.export_result(cx)))
                        .opacity(if tab.grid.result.columns.is_empty() {
                            0.4
                        } else {
                            1.
                        }),
                );
        } else if running {
            bottom = bottom
                .child(
                    div()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child("Running Queries"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_center()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(if self.scenario == "57-running" {
                            "1 process · updated 8:37:37 AM".into()
                        } else {
                            format!(
                                "{} processes · updated {}",
                                tab.grid.result.rows.len(),
                                self.running.updated
                            )
                        }),
                )
                .child(
                    div()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child("Select rows and press ⌫ to kill the query"),
                );
        } else {
            bottom = bottom
                .child(segments(&["Data", "Structure", "Index"], 0, false, cx))
                .child(small_button("add-row", "+ Row"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_center()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(format!(
                            "{}-{} of {} rows",
                            if tab.total == 0 { 0 } else { tab.offset + 1 },
                            (tab.offset + tab.limit).min(tab.total),
                            grouped(tab.total)
                        )),
                )
                .child(
                    small_button("filters", "")
                        .gap(px(6.))
                        .child(icon("Search.svg", 13., 0xe8e8e8))
                        .child("Filters"),
                )
                .child(small_button("columns", "Columns"))
                .child(
                    nav("page-prev", "ChevronLeft.svg", tab.offset == 0).on_click(cx.listener(
                        |this, _, _, cx| {
                            let t = this.state.current_mut();
                            t.offset = t.offset.saturating_sub(t.limit);
                            this.refresh_tab(cx);
                        },
                    )),
                )
                .child(
                    small_button("page-limit", tab.limit.to_string())
                        .w(px(90.))
                        .justify_between()
                        .child(icon("UpDown.svg", 12., 0xb8b8b8)),
                )
                .child(
                    nav(
                        "page-next",
                        "ChevronRight.svg",
                        tab.offset + tab.limit >= tab.total,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let t = this.state.current_mut();
                        if t.offset + t.limit < t.total {
                            t.offset += t.limit;
                            this.refresh_tab(cx);
                        }
                    })),
                );
        }
        content.child(bottom)
    }
}
pub(super) fn grouped(n: u64) -> String {
    let s = n.to_string();
    s.chars().enumerate().fold(String::new(), |mut a, (i, c)| {
        if i > 0 && (s.len() - i) % 3 == 0 {
            a.push(',');
        }
        a.push(c);
        a
    })
}
fn search_box(input: &Entity<InputState>, height: f32) -> Div {
    div()
        .h(px(height))
        .flex_shrink_0()
        .rounded(px(8.))
        .bg(rgba(0xffffff0c))
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .text_size(px(13.))
        .child(icon("Search.svg", 14., 0x8a8a8a))
        .child(div().flex_1().min_w_0().child(Input::new(input)))
}
fn segments(
    labels: &[&'static str],
    selected: usize,
    query: bool,
    cx: &mut Context<Workspace>,
) -> Div {
    div()
        .flex_shrink_0()
        .h(px(28.))
        .p(px(2.))
        .bg(rgba(0xffffff10))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(1.))
        .children(labels.iter().enumerate().map(|(i, label)| {
            div()
                .id(*label)
                .h(px(24.))
                .px(px(14.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .text_size(px(13.))
                .when(i == selected, |d| {
                    d.bg(rgb(telecat_theme::ACCENT)).text_color(rgb(0xffffff))
                })
                .when(query && i == 2, |d| d.opacity(0.4))
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if query && i < 2 {
                        this.state.current_mut().message = i == 1;
                        cx.notify();
                    }
                }))
        }))
}
fn highlight_sql(sql: &str, highlight: bool) -> StyledText {
    let text = SharedString::from(sql.to_owned());
    if !highlight {
        return StyledText::new(text);
    }
    static TOKENS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re=TOKENS.get_or_init(||regex::Regex::new(r"(?i)'(?:[^'\\]|\\.|'')*'|--[^\n]*|\b(?:SELECT|FROM|WHERE|AND|OR|NOT|IN|IS|NULL|AS|ON|JOIN|LEFT|RIGHT|INNER|OUTER|GROUP|BY|ORDER|LIMIT|OFFSET|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|ALTER|DROP|TABLE|INDEX|SHOW|CASE|WHEN|THEN|ELSE|END|ASC|DESC|LIKE|COUNT|DISTINCT|UNION|ALL|EXISTS|HAVING|WITH)\b").unwrap());
    StyledText::new(text).with_highlights(re.find_iter(sql).map(|m| {
        (
            m.range(),
            HighlightStyle {
                color: Some(
                    rgb(if m.as_str().starts_with("--") {
                        0x6a9955
                    } else if m.as_str().starts_with('\'') {
                        0xf0a35e
                    } else {
                        0x5fa8ff
                    })
                    .into(),
                ),
                ..Default::default()
            },
        )
    }))
}

/// The loading / error / cancelled state shown centred over a grid or list, styled like the
/// grid's "No rows" pill. `None` when the content should show as it is.
pub(super) fn load_state_view(placeholder: &Placeholder, id: &'static str) -> Option<Div> {
    let (spinner, text, color) = match placeholder {
        Placeholder::Loading { .. } => (true, "Loading…".to_owned(), 0xb8b8b8),
        Placeholder::Failed(e) => (false, format!("Couldn't load: {e}"), 0xff8a80),
        Placeholder::Cancelled => (false, "Loading was cancelled".to_owned(), 0xb8b8b8),
        _ => return None,
    };
    Some(
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.))
            .child(
                div()
                    .id(id)
                    .max_w(px(460.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(14.))
                    .py(px(7.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(0x3a3a3a))
                    .bg(rgba(0x2a2a2aee))
                    .font(ui_font())
                    .text_size(px(13.))
                    .text_color(rgb(color))
                    .when(spinner, |d| {
                        d.child(icon("spinner.svg", 14., 0xb8b8b8).with_animation(
                            "load-spin",
                            Animation::new(std::time::Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ))
                    })
                    .child(div().min_w_0().line_clamp(3).child(text)),
            ),
    )
}
