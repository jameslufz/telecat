//! Workspace chrome. Editor and grid internals belong to their parallel sessions.
pub mod commands;
mod components;
mod execution;
pub mod fixtures;
pub mod host;
mod layout;
mod panels;
use layout::{Layout, Splitter};
mod running;
mod sheets;
mod sidebar;
mod table;
mod table_fixtures;
mod workflows;
use crate::ui::InputEvent;
use crate::{Services, native_menu, ui::*};
use execution::PendingAction;
use gpui_kit::{prelude::*, *};
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use table::{TableInputs, TableNavigation};
use telecat_app::workspace_model::*;
use telecat_core::types::*;

pub fn open(
    services: Services,
    session: Option<Arc<telecat_db::Session>>,
    connection: ConnectionConfig,
    scenario: &str,
    cx: &mut App,
) {
    telecat_theme::init(cx);
    let name = if scenario.is_empty() {
        "workspace"
    } else {
        scenario
    }
    .to_owned();
    let scenario = scenario.to_owned();
    let height = fixtures::height(&scenario);
    let mut opts = crate::options(cx, 1440., height, false);
    opts.is_resizable = true;
    opts.is_minimizable = true;
    let result = gpui_kit::open_window(opts, cx, move |window, cx| {
        crate::print_window(window, &name, 1440., height as f64, services.scenario, cx);
        cx.new(|cx| Workspace::new(services, session, connection, scenario, window, cx))
    });
    if let Err(e) = result {
        eprintln!("Open workspace: {e}");
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    Items,
    Queries,
    History,
}
enum Sheet {
    Shortcuts,
    Preview,
    SwitchDatabase,
    Error(String),
    QuickLook {
        title: String,
        value: String,
    },
    SaveQuery {
        sql: String,
        tab: u64,
    },
    QuickOpen,
    Gate {
        sql: String,
        password: bool,
        danger: Option<String>,
        attempts: usize,
    },
}
pub struct Workspace {
    services: Services,
    session: Option<Arc<telecat_db::Session>>,
    state: WorkspaceState,
    scenario: String,
    focus: FocusHandle,
    search: Entity<InputState>,
    field_search: Entity<InputState>,
    sheet_input: Entity<InputState>,
    segment: Segment,
    collapsed: BTreeSet<&'static str>,
    sidebar: bool,
    panel: bool,
    console: bool,
    highlight: bool,
    sheet: Option<Sheet>,
    busy: bool,
    cancelled: Arc<AtomicBool>,
    queries: Vec<SavedQuery>,
    history: Vec<LogEntry>,
    subscriptions: Vec<Subscription>,
    tree_scroll: ScrollHandle,
    console_scroll: ScrollHandle,
    console_log_count: std::cell::Cell<usize>,
    table_inputs: std::collections::BTreeMap<u64, TableInputs>,
    saving: bool,
    pending: Option<PendingAction>,
    approval: QueryApproval,
    gate_password: Entity<InputState>,
    gate_focused: bool,
    components: components::Components,
    running: running::Running,
    databases: Vec<String>,
    picker_selection: usize,
    host: Option<WeakEntity<host::WorkspaceHost>>,
    host_active: bool,
    rail_visible: bool,
    draft_task: Option<Task<()>>,
    tree_state: sidebar::SidebarState,
    tree_focus: FocusHandle,
    viewport_height: f32,
    layout: Layout,
    picker_scroll: ScrollHandle,
    panel_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    ddl_scroll: ScrollHandle,
    databases_loading: bool,
}
impl Workspace {
    fn new(
        services: Services,
        session: Option<Arc<telecat_db::Session>>,
        connection: ConnectionConfig,
        scenario: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut state = if scenario.is_empty() {
            WorkspaceState::new(connection)
        } else {
            fixtures::state(&scenario)
        };
        if let Some(s) = &session {
            state.server_version = s.server_version.clone();
            state.tls = s.tls.clone();
            state.user = s.user.clone();
            state.database = s.database.clone();
            state.restore_drafts(&services.profile.drafts(&state.connection.id));
        }
        let search = text_input("", "Search for item...", false, window, cx);
        let field_search = text_input("", "Search for field...", false, window, cx);
        let sheet_input = text_input(
            if scenario == "a2-switched" {
                "other"
            } else {
                ""
            },
            "Search for database...",
            false,
            window,
            cx,
        );
        let mut subscriptions: Vec<_> = [&search, &field_search]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        subscriptions.push(
            cx.subscribe(&sheet_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.reset_picker_selection(cx);
                }
                cx.notify();
            }),
        );
        let sheet = match scenario.as_str() {
            "21-safe-prompt" => Some(Sheet::Gate {
                sql: "UPDATE sa_banks SET is_disabled = false WHERE id = 1".into(),
                password: true,
                danger: None,
                attempts: 0,
            }),
            "a1-preview" => Some(Sheet::Preview),
            "a2-switched" => Some(Sheet::SwitchDatabase),
            "a3-shortcuts" => Some(Sheet::Shortcuts),
            _ => None,
        };
        let gate_password = text_input("", "password", true, window, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                if this.saving {
                    return false;
                }
                if this.state.has_changes() {
                    this.request_close_window(window, cx);
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let queries = services.profile.queries(&state.connection.id);
        let history = services.profile.history(&state.connection.id);
        let mut this = Self {
            running: running::Running {
                database: Some(state.database.clone()),
                every: if scenario == "57-running" { 2 } else { 0 },
                ..Default::default()
            },
            databases: vec![state.database.clone()],
            picker_selection: 0,
            layout: Layout::default(),
            picker_scroll: ScrollHandle::new(),
            panel_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            ddl_scroll: ScrollHandle::new(),
            databases_loading: false,
            host: None,
            host_active: true,
            rail_visible: false,
            draft_task: None,
            tree_state: Default::default(),
            tree_focus: cx.focus_handle(),
            services,
            session,
            state,
            scenario: scenario.clone(),
            focus,
            search,
            field_search,
            sheet_input,
            segment: Segment::Items,
            collapsed: BTreeSet::from(["Functions", "Views", "Procedures"]),
            sidebar: true,
            panel: true,
            console: true,
            highlight: false,
            sheet,
            busy: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            queries,
            history,
            subscriptions,
            tree_scroll: ScrollHandle::new(),
            console_scroll: ScrollHandle::new(),
            console_log_count: std::cell::Cell::new(usize::MAX),
            table_inputs: Default::default(),
            saving: false,
            pending: None,
            approval: QueryApproval::default(),
            gate_password,
            gate_focused: false,
            components: components::Components::default(),
            viewport_height: fixtures::height(&scenario),
        };
        this.update_completion_schema(cx);
        if this.session.is_some() {
            this.refresh_objects(cx);
            this.load_databases(cx);
        }
        this
    }
    fn queue_drafts(&mut self, cx: &mut Context<Self>) {
        self.draft_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(350))
                .await;
            let _ = this.update(cx, |this, _| this.persist_drafts());
        }));
    }
    fn persist_drafts(&self) {
        if let Err(e) = self
            .services
            .profile
            .save_drafts(&self.state.connection.id, &self.state.drafts())
        {
            eprintln!("Save drafts: {e}");
        }
    }
    fn error(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.sheet = Some(Sheet::Error(error.to_string()));
        cx.notify();
    }
    fn refresh_objects(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let backend = session.backend_id.clone();
        let task = self
            .services
            .runtime
            .spawn(async move { session.list_objects().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .session
                    .as_ref()
                    .is_none_or(|s| s.backend_id != backend)
                {
                    return;
                }
                match result {
                    Ok(Ok(objects)) => {
                        this.state.objects = objects;
                        this.update_completion_schema(cx);
                    }
                    Ok(Err(e)) => this.error(e, cx),
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn refresh_tab(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let backend = session.backend_id.clone();
        let tab = self.state.current().clone();
        let id = tab.id;
        match tab.kind {
            TabKind::Table(table) => {
                let Some(editor) = tab.table else {
                    return;
                };
                if editor.is_new {
                    return;
                }
                let task = self.services.runtime.spawn(async move {
                    let data = session
                        .table_data(&TableDataRequest {
                            table: table.clone(),
                            filters: editor.applied,
                            combine: editor.combine,
                            order_by: editor.order_by,
                            limit: tab.limit,
                            offset: tab.offset,
                        })
                        .await?;
                    let info = session.table_info(&table).await?;
                    Ok::<_, anyhow::Error>((data, info))
                });
                cx.spawn(async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        if this
                            .session
                            .as_ref()
                            .is_none_or(|s| s.backend_id != backend)
                        {
                            return;
                        }
                        match r {
                            Ok(Ok((data, info))) => {
                                if let Some(t) = this.state.tabs.iter_mut().find(|t| t.id == id) {
                                    if t.table.as_ref().is_some_and(|e| e.data.dirty()) {
                                        return;
                                    }
                                    this.state.logs.push(LogEntry {
                                        time: timestamp(),
                                        sql: data.result.sql.clone(),
                                    });
                                    if let Some(editor) = &mut t.table {
                                        editor.load_data(&data);
                                    }
                                    t.grid = GridDocument {
                                        revision: t.grid.revision.wrapping_add(1),
                                        result: data.result,
                                        selected_row: None,
                                    };
                                    t.total = data.total;
                                    t.info = Some(info);
                                }
                            }
                            Ok(Err(e)) => this.error(e, cx),
                            Err(e) => this.error(e, cx),
                        }
                        this.update_completion_schema(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            TabKind::Running => {
                if self.running.loading {
                    return;
                }
                self.running.loading = true;
                let database = self.running.database.clone();
                let task = self
                    .services
                    .runtime
                    .spawn(async move { session.process_list(database.as_deref()).await });
                cx.spawn(async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        if this
                            .session
                            .as_ref()
                            .is_none_or(|s| s.backend_id != backend)
                        {
                            return;
                        }
                        this.running.loading = false;
                        this.running.updated = timestamp()
                            .split_whitespace()
                            .last()
                            .unwrap_or("")
                            .split('.')
                            .next()
                            .unwrap_or("")
                            .into();
                        match r {
                            Ok(Ok(result)) => {
                                let name = if this.state.connection.driver == Driver::Mysql {
                                    "id"
                                } else {
                                    "pid"
                                };
                                let selected_row =
                                    result.columns.iter().position(|c| c == name).and_then(|c| {
                                        result.rows.iter().position(|row| {
                                            row.get(c).and_then(|v| v.as_ref()).is_some_and(|id| {
                                                this.running.selected.contains(id)
                                            })
                                        })
                                    });
                                if let Some(t) = this.state.tabs.iter_mut().find(|t| t.id == id) {
                                    t.grid = GridDocument {
                                        revision: t.grid.revision.wrapping_add(1),
                                        result,
                                        selected_row,
                                    };
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
            TabKind::Query => {}
        }
    }
    fn open_table(&mut self, table: TableRef, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.state.open_table(table);
        self.refresh_tab(cx);
        cx.notify();
    }
    fn new_query(&mut self, sql: &str, cx: &mut Context<Self>) {
        self.state.open_query("SQL Query", sql);
        self.persist_drafts();
        cx.notify();
    }
    fn run(&mut self, all: bool, cx: &mut Context<Self>) {
        self.sync_editor_document(cx);
        if self.busy || self.saving || self.state.current().kind != TabKind::Query {
            return;
        }
        let plan = QueryPlan::new(
            &self.state.current().editor,
            all,
            self.state.connection.driver,
            self.state.connection.safe_mode,
            self.state.current().limit,
        );
        if plan.statements.is_empty() {
            return;
        }
        if !plan.dangers.is_empty() || plan.gate != Gate::None {
            self.pending = Some(PendingAction::Query {
                id: self.state.active,
                plan,
            });
            self.approval = QueryApproval::default();
            self.next_gate(cx);
            return;
        }
        self.execute_query(self.state.active, plan, QueryApproval::default(), cx);
    }
    fn execute_query(
        &mut self,
        id: u64,
        plan: QueryPlan,
        approval: QueryApproval,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.busy = true;
        self.cancelled.store(false, Ordering::SeqCst);
        let cancel = self.cancelled.clone();
        let history = LogEntry {
            time: timestamp(),
            sql: plan.original.join(";\n"),
        };
        for sql in &plan.statements {
            self.state.logs.push(LogEntry {
                time: timestamp(),
                sql: sql.clone(),
            });
        }
        let task = self
            .services
            .runtime
            .spawn(async move { run_plan(&session, &plan, approval, &cancel).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(Ok(results)) => {
                        if let Some(t) = this.state.tabs.iter_mut().find(|t| t.id == id) {
                            t.set_results(results);
                        }
                        if let Err(e) = this
                            .services
                            .profile
                            .add_history(&this.state.connection.id, history)
                        {
                            this.error(e, cx);
                        }
                        this.history = this.services.profile.history(&this.state.connection.id);
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
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(session) = self.session.clone() {
            let task = self
                .services
                .runtime
                .spawn(async move { session.cancel().await });
            cx.spawn(async move |this, cx| match task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = this.update(cx, |this, cx| this.error(e, cx));
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.error(e, cx));
                }
            })
            .detach();
        }
    }
    fn keyboard(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke.key;
        let mods = event.keystroke.modifiers;
        if key == "escape" && self.sheet.is_some() {
            self.cancel_sheet(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key == "enter" && matches!(self.sheet, Some(Sheet::SaveQuery { .. })) {
            self.save_named_query(cx);
            cx.stop_propagation();
            return;
        }
        if matches!(self.sheet, Some(Sheet::SwitchDatabase | Sheet::QuickOpen)) {
            let items = self.picker_items(cx);
            match key.as_str() {
                "down" => {
                    self.picker_selection =
                        (self.picker_selection + 1).min(items.len().saturating_sub(1))
                }
                "up" => self.picker_selection = self.picker_selection.saturating_sub(1),
                "enter" => {
                    if let Some((_, pick)) =
                        items.get(self.picker_selection.min(items.len().saturating_sub(1)))
                    {
                        self.choose_picker(pick.clone(), window, cx);
                    }
                }
                _ => return,
            }
            self.picker_scroll.scroll_to_item(self.picker_selection);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.sheet.is_some() {
            if key == "enter" && matches!(self.sheet, Some(Sheet::Gate { .. })) {
                if matches!(
                    self.sheet,
                    Some(Sheet::Gate {
                        danger: Some(_),
                        ..
                    })
                ) {
                    self.cancel_sheet(cx);
                } else {
                    self.approve_gate(window, cx);
                }
                cx.stop_propagation();
            }
            return;
        }
        if self.saving {
            cx.stop_propagation();
            return;
        }
        if key == "f" && mods.platform && mods.alt && self.state.current().table.is_some() {
            if let Some(t) = self.state.current_mut().table.as_mut() {
                t.column_filter = Some(String::new());
            }
            self.prepare_table_inputs(window, cx);
            if let Some(i) = self.table_inputs.get(&self.state.active) {
                i.search.update(cx, |s, cx| {
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key == "escape"
            && self
                .table_inputs
                .get(&self.state.active)
                .is_some_and(|i| i.search.focus_handle(cx).contains_focused(window, cx))
        {
            if let Some(t) = self.state.current_mut().table.as_mut() {
                t.column_filter = None;
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.tree_keyboard(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.filter_keys(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if (key == "escape" || (key == "z" && mods.platform))
            && (self.input_focused(window, cx) || self.grid_editing(cx))
        {
            return;
        }
        if self.state.current().kind == TabKind::Query
            && matches!(key.as_str(), "z" | "f" | "i")
            && mods.platform
        {
            return;
        }
        if key == "escape"
            && let Some(t) = &mut self.state.current_mut().table
        {
            if t.buffer().dirty() {
                t.buffer_mut().discard();
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        if mods.platform {
            let mut handled = true;
            match (key.as_str(), mods.shift) {
                ("0", false) => self.sidebar = !self.sidebar,
                ("0", true) => self.panel = !self.panel,
                ("c", true) => self.console = !self.console,
                ("t", false) => self.new_query("", cx),
                ("t", true) => self.new_table(cx),
                ("s", false) => self.save_command(window, cx),
                ("s", true) => self.save_sql_file(cx),
                ("o", false) => self.open_sql_file(cx),
                ("p", false) => self.open_anything(window, cx),
                ("f", false) => self.show_filters(window, cx),
                ("backspace", true) => self.discard_all(window, cx),
                ("z", redo) => self.edit_menu(if redo { "redo" } else { "undo" }, window, cx),
                ("w", false) => self.request_close_tab(self.state.active, window, cx),
                ("[", false) => self.state.step(-1),
                ("]", false) => self.state.step(1),
                (".", false) => {
                    self.state.open_running();
                    self.refresh_tab(cx);
                }
                ("r", false) => self.reload_workspace(window, cx),
                ("enter", false) => self.run(false, cx),
                ("enter", true) => self.run(true, cx),
                ("p", true) => {
                    self.flush_grid_edits(window, cx);
                    self.sheet = Some(Sheet::Preview);
                }
                ("k", false) => self.open_database_picker(window, cx),
                ("e", false) => {
                    self.reset_component_focus();
                    if let Some(id) = self
                        .state
                        .tabs
                        .iter()
                        .find(|t| t.kind == TabKind::Query)
                        .map(|t| t.id)
                    {
                        self.state.activate(id);
                    } else {
                        self.new_query("", cx);
                    }
                }
                (n, false)
                    if n.len() == 1 && n.as_bytes()[0] >= b'1' && n.as_bytes()[0] <= b'9' =>
                {
                    if let Some(t) = self.state.tabs.get((n.as_bytes()[0] - b'1') as usize) {
                        self.state.activate(t.id);
                    }
                }
                _ => handled = false,
            }
            if handled {
                cx.stop_propagation();
                cx.notify();
            }
        }
    }
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let rail = self.has_rail();
        let safe = self.state.connection.safe_mode;
        let mut lock = div().relative().size(px(18.)).child(icon(
            if safe == SafeMode::Silent {
                "Unlock.svg"
            } else {
                "Lock.svg"
            },
            18.,
            0xd0d0d0,
        ));
        if safe != SafeMode::Silent {
            lock = lock.child(
                div()
                    .absolute()
                    .right(px(-3.))
                    .bottom(px(-2.))
                    .size(px(8.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if matches!(safe, SafeMode::Safe1 | SafeMode::Safe2) {
                        0xe2574c
                    } else {
                        0xf2c14e
                    }))
                    .when(matches!(safe, SafeMode::Safe1 | SafeMode::Alert1), |d| {
                        d.bg(rgb(if safe == SafeMode::Safe1 {
                            0xe2574c
                        } else {
                            0xf2c14e
                        }))
                    }),
            );
        }
        div().h(px(54.)).flex_shrink_0().flex().items_center().gap(px(8.)).px(px(10.)).bg(rgb(0x262626)).border_b_1().border_color(rgb(0x111111))
            .when(!rail,|d|d.child(div().w(px(70.)).flex_shrink_0()))
            .child(tool_group().child(tool("sidebar","Sidebar.svg",!self.sidebar).on_click(cx.listener(|this,_,_,cx|{this.sidebar=!this.sidebar;cx.notify();}))).child(tool("connections","Plug.svg",false).on_click(cx.listener(|this,_,_,cx|crate::show_welcome(this.services.clone(),false,cx)))))
            .child(tool_group().child(tool_box("safe").child(lock).on_mouse_down(MouseButton::Left,cx.listener(|this,e:&MouseDownEvent,window,cx|{
                let mode=serde_json::to_value(this.state.connection.safe_mode).unwrap();let items:Vec<_>=choices()["SAFE_MODES"].as_array().unwrap().iter().map(|v|json!({"id":v["value"],"label":v["label"],"icon":v["icon"],"color":v["color"],"checked":v["value"]==mode})).collect();
                native_menu::choose(json!(items),e.position,window,cx,|this,id,_,cx|{
                    if let Ok(mode)=serde_json::from_value(json!(id)){this.change_safe_mode(mode,cx);}
                });
            }))).child(tool("database","Database.svg",false).on_click(cx.listener(|this,_,window,cx|{this.open_database_picker(window,cx);}))).child(tool_box("sql").text_size(px(13.)).font_weight(FontWeight::BOLD).child("SQL").on_click(cx.listener(|this,_,_,cx|this.new_query("",cx)))))
            .child(div().flex_1().min_w(px(120.)).h(px(36.)).px(px(16.)).rounded(px(10.)).bg(color(&self.state.connection.color)).text_color(rgb(0xffffff)).font(mono()).text_size(px(13.)).line_height(px(36.)).overflow_hidden().text_ellipsis().child(self.state.status_text()))
            .child(tool_group().child(tool("refresh","Refresh.svg",false).on_click(cx.listener(|this,_,window,cx|{this.reload_workspace(window,cx);}))))
            .child(tool_group().child(tool("console","Console.svg",!self.console).on_click(cx.listener(|this,_,_,cx|{this.console=!this.console;cx.notify();}))).child(tool("panel","PanelRight.svg",!self.panel).on_click(cx.listener(|this,_,_,cx|{this.panel=!this.panel;cx.notify();}))))
    }
    fn tabstrip(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(38.))
            .flex_shrink_0()
            .bg(rgb(0x242424))
            .border_b_1()
            .border_color(rgb(0x111111))
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .child(
                nav("back", "ChevronLeft.svg", !self.state.can_back()).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.state.navigate(false);
                        cx.notify();
                    },
                )),
            )
            .child(
                nav("forward", "ChevronRight.svg", !self.state.can_forward()).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.state.navigate(true);
                        cx.notify();
                    }),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .ml(px(4.))
                    .flex()
                    .gap(px(4.))
                    .children(self.state.tabs.iter().map(|t| {
                        let id = t.id;
                        let active = id == self.state.active;
                        div()
                            .id(ElementId::Name(format!("tab-{id}").into()))
                            .relative()
                            .flex_1()
                            .min_w(px(150.))
                            .max_w(px(600.))
                            .h(px(28.))
                            .px(px(26.))
                            .rounded(px(7.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(13.5))
                            .text_color(rgb(if active { 0xffffff } else { 0xb8b8b8 }))
                            .when(active, |d| d.bg(rgb(0x4a4a4a)))
                            .child(
                                div()
                                    .truncate()
                                    .when(matches!(t.kind, TabKind::Table(_)), |d| d.italic())
                                    .child(t.title.clone()),
                            )
                            .when(t.table.as_ref().is_some_and(|e| e.dirty()), |d| {
                                d.child(div().text_color(rgb(0xe0b84a)).child("•"))
                            })
                            .when(active && self.state.tabs.len() > 1, |d| {
                                d.child(
                                    div()
                                        .id(ElementId::Name(format!("close-{id}").into()))
                                        .absolute()
                                        .left(px(7.))
                                        .size(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(icon("X.svg", 12., 0xb8b8b8))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.request_close_tab(id, window, cx);
                                        })),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.activate(id);
                                cx.notify();
                            }))
                    })),
            )
    }
    fn has_rail(&self) -> bool {
        self.rail_visible || matches!(self.scenario.as_str(), "95-rail" | "a2-switched")
    }
}
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport_height = window.viewport_size().height.as_f32();
        self.prepare_table_inputs(window, cx);
        self.prepare_components(window, cx);
        self.prepare_running_timer(cx);
        if !self.services.scenario
            && !self.gate_focused
            && matches!(self.sheet, Some(Sheet::Gate { password: true, .. }))
        {
            self.gate_focused = true;
            self.gate_password.update(cx, |s, cx| s.focus(window, cx));
        }
        let _ = &self.subscriptions;
        div()
            .id("workspace")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::keyboard))
            .size_full()
            .relative()
            .flex()
            .font(ui_font())
            .text_size(px(13.))
            .line_height(px(16.))
            .text_color(rgb(telecat_theme::TEXT))
            .bg(rgb(telecat_theme::BG))
            .when(self.has_rail() && self.host.is_none(), |d| {
                d.child(self.rail(cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.toolbar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .when(self.sidebar, |d| {
                                d.child(self.sidebar_view(cx))
                                    .child(self.splitter(Splitter::Sidebar, cx))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .child(self.tabstrip(cx))
                                    .child(self.content(cx))
                                    .when(self.console, |d| {
                                        d.child(self.splitter(Splitter::Console, cx))
                                            .child(self.console_view(cx))
                                    }),
                            )
                            .when(self.panel, |d| {
                                d.child(self.splitter(Splitter::Panel, cx))
                                    .child(self.right_panel(cx))
                            }),
                    ),
            )
            .child(self.layout_listeners(cx))
            .when(self.saving, |d| {
                d.child(div().absolute().inset_0().occlude())
            })
            .when_some(self.sheet.as_ref(), |d, s| d.child(self.sheet_view(s, cx)))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.persist_drafts();
        if let Some(s) = self.session.clone() {
            self.services.runtime.spawn(async move {
                let _ = s.cancel().await;
                s.close().await;
            });
        }
    }
}
use telecat_theme::mono;
fn tool_group() -> Div {
    div()
        .h(px(36.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px(px(3.))
        .gap(px(2.))
        .rounded(px(10.))
        .border_1()
        .border_color(rgba(0xffffff10))
        .bg(rgba(0xffffff0d))
}
fn tool_box(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(36.))
        .h(px(30.))
        .flex_shrink_0()
        .rounded(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .hover(|d| d.bg(rgba(0xffffff14)))
}
fn tool(id: &'static str, path: &'static str, off: bool) -> Stateful<Div> {
    tool_box(id).child(icon(path, 18., if off { 0x777777 } else { 0xd0d0d0 }))
}
fn nav(id: &'static str, path: &'static str, disabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(26.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .opacity(if disabled { 0.3 } else { 1. })
        .child(icon(path, 16., 0xb8b8b8))
}
fn small_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    button(id, label, false, false)
        .h(px(26.))
        .px(px(11.))
        .text_size(px(13.))
}
fn subbar() -> Div {
    div()
        .h(px(42.))
        .flex_shrink_0()
        .px(px(10.))
        .gap(px(8.))
        .flex()
        .items_center()
        .bg(rgb(0x242424))
        .border_t_1()
        .border_color(rgb(0x111111))
}
fn timestamp() -> String {
    unsafe {
        let mut now: libc::time_t = 0;
        libc::time(&mut now);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.0000",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
