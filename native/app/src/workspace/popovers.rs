//! Small popovers of the Data view, drawn like TablePlus's: page settings, the exact row count
//! of a large table and the date picker. Escape or a click outside closes them with nothing
//! changed.
use super::components::GridKey;
use super::*;
use telecat_app::table_model::Part;
use telecat_core::columns::ValueKind;
use telecat_core::datetime::{self, Stamp};
use telecat_core::filters::page_settings;

pub(super) enum Popover {
    Paging {
        anchor: Point<Pixels>,
        limit: Entity<InputState>,
        offset: Entity<InputState>,
        error: Option<String>,
        _subscriptions: Vec<Subscription>,
    },
    Count {
        anchor: Point<Pixels>,
        filtered: bool,
        counting: bool,
        result: Option<u64>,
        error: Option<String>,
    },
    Date(DatePicker),
}
pub(super) struct DatePicker {
    anchor: Point<Pixels>,
    tab: u64,
    /// The cells it sets: (row identity, data column).
    cells: Vec<(String, usize)>,
    kind: ValueKind,
    picked: Stamp,
    /// The month shown.
    month: (i32, u32),
    time: Entity<InputState>,
    error: Option<String>,
    _subscription: Subscription,
}
const WEEKDAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// The bottom bar's row count: `1-300 of 1,234 rows`, `1-300 of ~1,234,567 rows` for an
/// estimate.
pub(super) fn row_counter(offset: u64, shown: u64, total: u64, estimated: bool) -> String {
    let first = if shown == 0 { 0 } else { offset + 1 };
    let last = offset + shown;
    let total_text = if estimated {
        format!("~{}", super::panels::grouped(total))
    } else {
        super::panels::grouped(total)
    };
    format!(
        "{}-{} of {} {}",
        super::panels::grouped(first),
        super::panels::grouped(last),
        total_text,
        if total == 1 && !estimated {
            "row"
        } else {
            "rows"
        }
    )
}
/// Today on this Mac's clock and time zone.
fn local_now() -> Stamp {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as libc::time_t);
    // SAFETY: localtime_r only writes the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        let (y, m, d) = datetime::civil_from_days(secs / 86_400);
        return Stamp::date(y, m, d);
    }
    Stamp {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec.min(59) as u32,
    }
}
fn time_text(s: Stamp) -> String {
    format!("{:02}:{:02}:{:02}", s.hour, s.minute, s.second)
}
impl Workspace {
    pub(super) fn close_popover(&mut self, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            cx.notify();
        }
    }
    /// The paging popover: Limit and Offset, checked like TablePlus when Go is pressed.
    pub(super) fn open_page_settings(
        &mut self,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.state.current();
        if tab.table.as_ref().is_none_or(|t| t.part != Part::Data) {
            return;
        }
        let limit = text_input(tab.limit.to_string(), "Limit", false, window, cx);
        let offset = text_input(
            if tab.offset == 0 {
                String::new()
            } else {
                tab.offset.to_string()
            },
            "Offset",
            false,
            window,
            cx,
        );
        let mut subscriptions = vec![];
        for input in [&limit, &offset] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.apply_page_settings(window, cx);
                    }
                },
            ));
        }
        offset.update(cx, |s, cx| s.focus(window, cx));
        self.popover = Some(Popover::Paging {
            anchor,
            limit,
            offset,
            error: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }
    fn apply_page_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popover::Paging {
            limit,
            offset,
            error,
            ..
        }) = &mut self.popover
        else {
            return;
        };
        let tab = self.state.current();
        let total = (!tab.estimated).then_some(tab.total);
        match page_settings(
            limit.read(cx).value().as_ref(),
            offset.read(cx).value().as_ref(),
            total,
        ) {
            Ok((limit, offset)) => {
                self.popover = None;
                self.navigate_table(TableNavigation::Page(offset, limit), window, cx);
            }
            Err(message) => *error = Some(message),
        }
        cx.notify();
    }
    /// The estimated count's popover: an exact COUNT on request.
    pub(super) fn open_count_popover(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.popover = Some(Popover::Count {
            anchor,
            filtered: false,
            counting: false,
            result: None,
            error: None,
        });
        cx.notify();
    }
    fn run_count(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(Popover::Count {
            filtered,
            counting,
            error,
            ..
        }) = &mut self.popover
        else {
            return;
        };
        if *counting {
            return;
        }
        *counting = true;
        *error = None;
        let filtered = *filtered;
        let tab = self.state.current();
        let Some(t) = &tab.table else {
            return;
        };
        let id = tab.id;
        let req = TableDataRequest {
            table: t.table.clone(),
            filters: t.applied.clone(),
            combine: t.combine,
            order_by: vec![],
            estimate: false,
            limit: tab.limit,
            offset: 0,
        };
        let task = self
            .services
            .runtime
            .spawn(async move { session.count_rows(&req, filtered).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                let outcome = match r {
                    Ok(Ok(n)) => Ok(n),
                    Ok(Err(e)) => Err(format!("{e:#}")),
                    Err(e) => Err(e.to_string()),
                };
                if let Some(Popover::Count {
                    counting,
                    result,
                    error,
                    filtered: f,
                    ..
                }) = &mut this.popover
                {
                    *counting = false;
                    match &outcome {
                        Ok(n) => *result = Some(*n),
                        Err(e) => *error = Some(e.clone()),
                    }
                    // The count is what the page counter shows when it covers the same rows:
                    // with the filters, or of a table shown without any.
                    let covers_view = *f
                        || this
                            .state
                            .tabs
                            .iter()
                            .find(|t| t.id == id)
                            .and_then(|t| t.table.as_ref())
                            .is_some_and(|t| t.applied.is_empty());
                    if let (Ok(n), true) = (&outcome, covers_view)
                        && let Some(tab) = this.state.tabs.iter_mut().find(|t| t.id == id)
                    {
                        tab.total = *n;
                        tab.estimated = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    /// Set Value ▸ Date Picker…: a month calendar and a time for the selected cells.
    pub(super) fn open_date_picker(
        &mut self,
        k: GridKey,
        row: usize,
        col: usize,
        bounds: telecat_grid::Rect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pick) = self.grid_pick(k, cx) else {
            return;
        };
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        let Some(t) = &tab.table else {
            return;
        };
        let kind = t
            .data
            .result
            .columns
            .get(col)
            .and_then(|name| t.columns.as_ref()?.get(name))
            .map_or(ValueKind::Other, |c| c.kind);
        if !kind.is_temporal() {
            return;
        }
        let current = t
            .data
            .rows
            .get(row)
            .and_then(|r| r.cur.get(col))
            .and_then(|v| v.value())
            .and_then(datetime::parse);
        let picked = current.unwrap_or_else(local_now);
        let rows: Vec<usize> = if pick.rows.contains(&row) {
            pick.rows.clone()
        } else {
            vec![row]
        };
        let columns: Vec<usize> = if pick.columns.contains(&col) {
            pick.columns.clone()
        } else {
            vec![col]
        };
        let cells: Vec<(String, usize)> = rows
            .iter()
            .filter_map(|&r| t.data.rows.get(r).map(|x| x.key.clone()))
            .flat_map(|key| columns.iter().map(move |&c| (key.clone(), c)))
            .collect();
        let time = text_input(time_text(picked), "HH:MM:SS", false, window, cx);
        let subscription = cx.subscribe_in(&time, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_date_picker(cx);
            }
        });
        self.popover = Some(Popover::Date(DatePicker {
            anchor: point(px(bounds.x), px(bounds.y + bounds.height)),
            tab: k.0,
            cells,
            kind,
            month: (picked.year, picked.month),
            picked,
            time,
            error: None,
            _subscription: subscription,
        }));
        cx.notify();
    }
    fn apply_date_picker(&mut self, cx: &mut Context<Self>) {
        let Some(Popover::Date(p)) = &mut self.popover else {
            return;
        };
        let mut stamp = p.picked;
        if p.kind != ValueKind::Date {
            let text = p.time.read(cx).value().to_string();
            match datetime::parse(&text) {
                Some(t) if text.trim().len() >= 5 && text.as_bytes().get(2) == Some(&b':') => {
                    stamp.hour = t.hour;
                    stamp.minute = t.minute;
                    stamp.second = t.second;
                }
                _ => {
                    p.error = Some("Enter the time as HH:MM:SS".into());
                    cx.notify();
                    return;
                }
            }
        }
        let text = datetime::format(stamp, p.kind);
        let (tab_id, keys) = (p.tab, p.cells.clone());
        self.popover = None;
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(t) = &tab.table else {
            return;
        };
        let cells: Vec<(usize, usize, telecat_core::changes::Cell)> = keys
            .iter()
            .filter_map(|(key, col)| {
                let row = t.data.rows.iter().position(|r| &r.key == key)?;
                Some((row, *col, telecat_core::changes::Cell::Text(text.clone())))
            })
            .collect();
        if cells.len() != keys.len() {
            self.show_hint(
                "The rows changed while the picker was open; nothing was set.",
                cx,
            );
        } else if let Err(reason) = super::data::stage_cells(tab, cells) {
            self.show_hint(reason, cx);
        }
        cx.notify();
    }
    fn date_picker_step(&mut self, months: i32, cx: &mut Context<Self>) {
        if let Some(Popover::Date(p)) = &mut self.popover {
            let shown = Stamp::date(p.month.0, p.month.1, 1).add_months(months);
            p.month = (shown.year, shown.month);
            cx.notify();
        }
    }
    fn date_picker_pick(&mut self, day: u32, cx: &mut Context<Self>) {
        if let Some(Popover::Date(p)) = &mut self.popover {
            p.picked = Stamp {
                year: p.month.0,
                month: p.month.1,
                day,
                ..p.picked
            };
            cx.notify();
        }
    }
    fn date_picker_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Popover::Date(p)) = &mut self.popover {
            let now = local_now();
            p.picked = now;
            p.month = (now.year, now.month);
            p.time
                .update(cx, |s, cx| s.set_value(time_text(now), window, cx));
            cx.notify();
        }
    }
    /// The open popover over everything else, with a click-away layer under it.
    pub(super) fn popover_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let popover = self.popover.as_ref()?;
        let (anchor, width, body) = match popover {
            Popover::Paging {
                anchor,
                limit,
                offset,
                error,
                ..
            } => (
                *anchor,
                260.,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(field_row("Offset", input_box(offset, false, false)))
                    .child(field_row("Limit", input_box(limit, false, false)))
                    .when_some(error.clone(), |d, e| {
                        d.child(div().text_color(rgb(0xff6b6b)).text_size(px(11.5)).child(e))
                    })
                    .child(div().flex().justify_end().child(
                        button("page-go", "Go", true, false).on_click(
                            cx.listener(|this, _, window, cx| this.apply_page_settings(window, cx)),
                        ),
                    )),
            ),
            Popover::Count {
                anchor,
                filtered,
                counting,
                result,
                error,
            } => {
                let filtered = *filtered;
                let tab = self.state.current();
                let has_filters = tab.table.as_ref().is_some_and(|t| !t.applied.is_empty());
                (
                    *anchor,
                    300.,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .child(row_counter(tab.offset, tab.limit.min(tab.total), tab.total, tab.estimated)),
                        )
                        .child(
                            checkbox("count-filtered", "Include current filter conditions", filtered, !has_filters || *counting)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let applied = this.state.current().table.as_ref().is_some_and(|t| !t.applied.is_empty());
                                    if let Some(Popover::Count { filtered, counting: false, .. }) = &mut this.popover
                                        && applied
                                    {
                                        *filtered = !*filtered;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .items_start()
                                .text_size(px(11.5))
                                .text_color(rgb(telecat_theme::MUTED))
                                .child(icon("Warning.svg", 13., 0xe0b84a))
                                .child("Counting every row of a large table may affect your server performance."),
                        )
                        .when_some(*result, |d, n| {
                            d.child(div().text_size(px(12.5)).child(format!(
                                "{} {}",
                                super::panels::grouped(n),
                                if n == 1 { "row" } else { "rows" }
                            )))
                        })
                        .when_some(error.clone(), |d, e| {
                            d.child(div().text_color(rgb(0xff6b6b)).text_size(px(11.5)).child(e))
                        })
                        .child(
                            div().flex().justify_end().child(
                                button("count-run", if *counting { "counting..." } else { "Count" }, true, *counting)
                                    .on_click(cx.listener(|this, _, _, cx| this.run_count(cx))),
                            ),
                        ),
                )
            }
            Popover::Date(p) => (p.anchor, 250., self.date_picker_view(p, cx)),
        };
        let viewport = self.viewport_height;
        let left = (f32::from(anchor.x) - width / 2.).max(8.);
        // Below the anchor when it fits, otherwise above it (the bottom bar).
        let below = f32::from(anchor.y) + 12. < viewport - 300.;
        let panel = div()
            .id("popover")
            .occlude()
            .absolute()
            .left(px(left))
            .w(px(width))
            .when(below, |d| d.top(px(f32::from(anchor.y) + 6.)))
            .when(!below, |d| {
                d.bottom(px(viewport - f32::from(anchor.y) + 14.))
            })
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(0x484848))
            .bg(rgb(telecat_theme::CARD))
            .shadow(vec![BoxShadow {
                inset: false,
                color: rgba(0x00000099).into(),
                offset: point(px(0.), px(8.)),
                blur_radius: px(20.),
                spread_radius: px(0.),
            }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(body);
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("popover-away")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.close_popover(cx)),
                        ),
                )
                .child(panel),
        )
    }
    fn date_picker_view(&self, p: &DatePicker, cx: &mut Context<Self>) -> Div {
        let (year, month) = p.month;
        let days = datetime::month_grid(year, month);
        let today = local_now();
        let calendar = p.kind != ValueKind::Time;
        let nav = |id: &'static str, path: &'static str, months: i32| {
            div()
                .id(id)
                .size(px(22.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .hover(|d| d.bg(rgb(0x383838)))
                .child(icon(path, 12., 0xe8e8e8))
                .on_click(cx.listener(move |this, _, _, cx| this.date_picker_step(months, cx)))
        };
        let mut body = div().flex().flex_col().gap(px(8.));
        if calendar {
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(nav("date-prev", "ChevronLeft.svg", -1))
                        .child(
                            div()
                                .flex_1()
                                .text_center()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{} {year}", MONTHS[(month - 1) as usize])),
                        )
                        .child(nav("date-next", "ChevronRight.svg", 1)),
                )
                .child(div().flex().children(WEEKDAYS.iter().map(|d| {
                    div()
                        .flex_1()
                        .text_center()
                        .text_size(px(10.5))
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(*d)
                })))
                .children(days.chunks(7).map(|week| {
                    div().flex().children(week.iter().map(|day| {
                        let Some(day) = *day else {
                            return div().flex_1().h(px(24.));
                        };
                        let picked =
                            p.picked.year == year && p.picked.month == month && p.picked.day == day;
                        let is_today =
                            today.year == year && today.month == month && today.day == day;
                        div().flex_1().h(px(24.)).flex().justify_center().child(
                            div()
                                .id(("date-day", day as usize))
                                .size(px(24.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .when(is_today && !picked, |d| {
                                    d.text_color(rgb(telecat_theme::ACCENT))
                                })
                                .when(picked, |d| {
                                    d.bg(rgb(telecat_theme::ACCENT)).text_color(rgb(0xffffff))
                                })
                                .when(!picked, |d| d.hover(|d| d.bg(rgb(0x383838))))
                                .child(day.to_string())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.date_picker_pick(day, cx)
                                })),
                        )
                    }))
                }));
        }
        if p.kind != ValueKind::Date {
            body = body.child(field_row("Time", input_box(&p.time, false, false)));
        }
        body.when_some(p.error.clone(), |d, e| {
            d.child(div().text_color(rgb(0xff6b6b)).text_size(px(11.5)).child(e))
        })
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(
                    button(
                        "date-now",
                        if p.kind == ValueKind::Date {
                            "Today"
                        } else {
                            "Now"
                        },
                        false,
                        false,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.date_picker_now(window, cx))),
                )
                .child(div().flex_1())
                .child(
                    button("date-cancel", "Cancel", false, false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_popover(cx))),
                )
                .child(
                    button("date-ok", "OK", true, false)
                        .on_click(cx.listener(|this, _, _, cx| this.apply_date_picker(cx))),
                ),
        )
    }
}
fn field_row(label: &'static str, input: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().w(px(52.)).text_size(px(12.)).child(label))
        .child(input.h(px(28.)))
}

#[cfg(test)]
mod tests {
    use super::row_counter;
    #[test]
    fn the_row_counter_reads_like_tableplus() {
        assert_eq!(row_counter(0, 3, 3, false), "1-3 of 3 rows");
        assert_eq!(row_counter(0, 1, 1, false), "1-1 of 1 row");
        assert_eq!(row_counter(0, 0, 0, false), "0-0 of 0 rows");
        assert_eq!(
            row_counter(300, 300, 2_500_000, true),
            "301-600 of ~2,500,000 rows"
        );
    }
}
