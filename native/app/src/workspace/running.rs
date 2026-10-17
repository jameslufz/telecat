use super::*;
#[derive(Default)]
pub(super) struct Running {
    pub database: Option<String>,
    pub every: u64,
    pub loading: bool,
    pub selected: BTreeSet<String>,
    pub updated: String,
    pub timer: Option<Task<()>>,
    pub timer_key: Option<(u64, u64)>,
}
impl Workspace {
    pub(super) fn prepare_running_timer(&mut self, cx: &mut Context<Self>) {
        let desired = (self.host_active
            && self.state.current().kind == TabKind::Running
            && self.running.every > 0
            && self.session.is_some()
            && !self.services.scenario)
            .then_some((self.state.active, self.running.every));
        if desired == self.running.timer_key {
            return;
        }
        self.running.timer = None;
        self.running.timer_key = desired;
        if let Some((id, seconds)) = desired {
            self.running.timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(seconds))
                        .await;
                    let keep = this
                        .update(cx, |this, cx| {
                            if this.state.active != id || this.running.every != seconds {
                                return false;
                            }
                            if this.saving {
                                return true;
                            }
                            this.refresh_tab(cx);
                            true
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            }));
        }
    }
    pub(super) fn process_database_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items = vec![
            json!({"id":"\u{0}all","label":"All databases","checked":self.running.database.is_none()}),
        ];
        items.extend(
            self.databases.iter().map(
                |d| json!({"id":d,"label":d,"checked":self.running.database.as_ref()==Some(d)}),
            ),
        );
        native_menu::choose(json!(items), position, window, cx, |this, id, _, cx| {
            this.running.database = if id == "\0all" { None } else { Some(id) };
            this.running.selected.clear();
            this.refresh_tab(cx);
            cx.notify();
        });
    }
    pub(super) fn process_interval_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items:Vec<_>=[0,1,2,5,10,30,60,300].into_iter().map(|s|json!({"id":s.to_string(),"label":interval_label(s),"checked":s==self.running.every})).collect();
        native_menu::choose(json!(items), position, window, cx, |this, id, _, cx| {
            if let Ok(s) = id.parse() {
                this.running.every = s;
                this.prepare_running_timer(cx);
                cx.notify();
            }
        });
    }
    pub(super) fn load_databases(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else {
            return;
        };
        self.databases_loading = true;
        let backend = s.backend_id.clone();
        let task = self
            .services
            .runtime
            .spawn(async move { s.list_databases().await });
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
                this.databases_loading = false;
                this.databases = result.ok().and_then(Result::ok).unwrap_or_default();
                this.reset_picker_selection(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
pub(super) fn interval_label(s: u64) -> String {
    if s == 0 {
        "Auto refresh: Off".into()
    } else if s < 60 {
        format!("Every {s} second{}", if s == 1 { "" } else { "s" })
    } else {
        format!("Every {} minute{}", s / 60, if s == 60 { "" } else { "s" })
    }
}
