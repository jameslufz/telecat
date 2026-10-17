mod assets;
mod export_dialog;
mod form_view;
mod native_menu;
mod ui;
mod welcome;
mod window_frame;
mod workspace;
use gpui_kit::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use telecat_app::{
    credentials::{Keychain, MacKeychain, MemoryKeychain},
    store::Profile,
};
use telecat_core::types::*;
#[derive(Clone)]
pub struct Services {
    pub profile: Arc<Profile>,
    pub runtime: Arc<telecat_app::worker::Runtime>,
    pub model: Entity<Model>,
    pub scenario: bool,
}
pub struct Model {
    pub snapshot: StoreSnapshot,
    pub import_error: Option<String>,
}
impl Services {
    fn start_import(&self, cx: &mut App) {
        if self.scenario {
            return;
        }
        let Some(source) = telecat_app::import::electron_default_path() else {
            return;
        };
        let services = self.clone();
        let profile = self.profile.clone();
        let task = self.runtime.spawn_blocking(move || {
            telecat_app::import::import_electron(&profile, &source, profile.keychain.as_ref())
        });
        cx.spawn(async move |cx| {
            let result = task.await;
            let _ = cx.update(|cx| {
                // Ordinary import failures are recorded in electron-import.json; missing
                // Electron profiles are normal. A worker panic must be visible in the UI.
                if let Err(error) = result {
                    services.model.update(cx, |model, cx| {
                        model.import_error = Some(error.to_string());
                        cx.notify();
                    });
                }
                services.refresh(cx);
            });
        })
        .detach();
    }
    pub fn refresh(&self, cx: &mut App) {
        let s = self.profile.snapshot();
        self.model.update(cx, |m, cx| {
            m.snapshot = s;
            cx.notify();
        });
    }
}
fn window_failure(name: &str, scenario: bool, error: impl std::fmt::Display) {
    eprintln!("Window {name}: {error}");
    if scenario {
        // Never publish WINDOW for an uncapturable scenario.
        std::process::exit(1);
    }
}
fn print_window(
    window: &Window,
    name: &str,
    width: f64,
    height: f64,
    scenario: bool,
    cx: &mut App,
) {
    let view = (|| -> anyhow::Result<_> {
        let raw = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("Native window handle: {error:?}"))?;
        let RawWindowHandle::AppKit(handle) = raw.as_raw() else {
            anyhow::bail!("Expected an AppKit window handle");
        };
        unsafe {
            objc2::rc::Retained::retain(handle.ns_view.as_ptr() as *mut objc2_app_kit::NSView)
        }
        .ok_or_else(|| anyhow::anyhow!("Missing native view"))
    })();
    let view = match view {
        Ok(view) => view,
        Err(e) => {
            window_failure(name, scenario, e);
            return;
        }
    };
    let handle = window.window_handle();
    let name = name.to_owned();
    let requested = objc2_foundation::NSSize::new(width, height);
    // Geometry changes invoke resize callbacks. Run outside all GPUI borrows.
    cx.spawn(async move |cx| {
        if scenario {
            if let Err(e) = window_frame::place_scenario(&view, requested) {
                window_failure(&name, true, e);
                return;
            }
        }
        let failure_name = name.clone();
        let result = handle.update(cx, |_, window, cx| {
            if scenario {
                cx.activate(true);
                window.activate_window();
            }
            // Let the platform's resize/scale callbacks settle before announcing.
            window.on_next_frame(move |window, _| {
                let viewport = window.viewport_size();
                let viewport = objc2_foundation::NSSize::new(
                    f64::from(viewport.width.as_f32()),
                    f64::from(viewport.height.as_f32()),
                );
                match window_frame::check_window(
                    &view,
                    scenario.then_some(requested),
                    Some(viewport),
                ) {
                    Ok(id) => println!("WINDOW {name} {id}"),
                    Err(e) => window_failure(&name, scenario, e),
                }
            });
        });
        if let Err(e) = result {
            window_failure(&failure_name, scenario, e);
        }
    })
    .detach();
}
pub fn options(cx: &App, width: f32, height: f32, blurred: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Telecat".into()),
            appears_transparent: true,
            traffic_light_position: Some(if width >= 900. {
                point(px(16.), px(20.))
            } else {
                point(px(14.), px(14.))
            }),
        }),
        is_resizable: false,
        is_minimizable: false,
        window_background: if blurred {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        ..Default::default()
    }
}
pub fn open_form(
    services: Services,
    c: ConnectionConfig,
    password: String,
    scenario: &str,
    cx: &mut App,
) {
    let name = if scenario.is_empty() {
        "connection-form"
    } else {
        scenario
    }
    .to_owned();
    let state = scenario.to_owned();
    let result = gpui_kit::open_window(options(cx, 520., 560., false), cx, move |window, cx| {
        print_window(window, &name, 520., 560., services.scenario, cx);
        cx.new(|cx| form_view::ConnectionForm::new(services, c, password, &state, window, cx))
    });
    if let Err(e) = result {
        eprintln!("Open form: {e}");
    }
}
pub fn open_workspace(
    services: Services,
    session: Arc<telecat_db::Session>,
    connection: ConnectionConfig,
    cx: &mut App,
) {
    workspace::host::open(services, session, connection, cx);
}
fn scenario_snapshot() -> StoreSnapshot {
    let mut seat = empty_connection(Driver::Mysql);
    seat.id = "qa-seatgold".into();
    seat.name = "SeatGold".into();
    seat.color = "#e48e00".into();
    seat.host = "127.0.0.1".into();
    seat.port = 3307;
    seat.user = "root".into();
    seat.database = "zixma".into();
    seat.tag = "local".into();
    seat.group_id = Some("qa-fa-agent".into());
    let mut mochi = empty_connection(Driver::Postgres);
    mochi.id = "qa-mochi".into();
    mochi.name = "Mochi".into();
    mochi.color = "#ff3ad9".into();
    mochi.port = 5434;
    mochi.user = "postgres".into();
    mochi.database = "main_db".into();
    mochi.tag = "local".into();
    let mut broken = seat.clone();
    broken.id = "qa-broken".into();
    broken.name = "Broken".into();
    broken.color = "#6e1109".into();
    broken.group_id = None;
    StoreSnapshot {
        connections: vec![seat, mochi, broken],
        groups: vec![ConnectionGroup {
            id: "qa-fa-agent".into(),
            name: "FA Agent".into(),
            collapsed: Some(false),
        }],
    }
}
fn main() {
    let launch_timing = telecat_app::launch::LaunchTiming::from_env();
    telecat_app::worker::install_private_panic_hook();
    let scenario = std::env::var("TELECAT_SCENARIO").unwrap_or_default();
    if !scenario.is_empty() {
        assert!(
            std::env::var_os("TELECAT_USER_DATA").is_some(),
            "Scenarios require a temporary TELECAT_USER_DATA"
        );
        assert!(
            [
                "smoke",
                "01-welcome-empty",
                "02-picker-mysql",
                "20-welcome-list",
                "03-form-mysql",
                "50-form-testing",
                "30-form-pg"
            ]
            .contains(&scenario.as_str())
                || workspace::fixtures::is_workspace_scenario(&scenario),
            "Unknown scenario"
        );
    }
    let root = Profile::default_path().expect("profile directory");
    assert!(root.is_absolute(), "TELECAT_USER_DATA must be absolute");
    let keys: Arc<dyn Keychain> =
        if !scenario.is_empty() || std::env::var_os("TELECAT_NO_KEYCHAIN").is_some() {
            Arc::new(MemoryKeychain::default())
        } else {
            Arc::new(MacKeychain)
        };
    let profile = Arc::new(Profile::new(root, keys));
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("async runtime")
            .into(),
    );
    if !scenario.is_empty() {
        profile
            .seed(&if scenario == "20-welcome-list" {
                scenario_snapshot()
            } else {
                StoreSnapshot {
                    connections: vec![],
                    groups: vec![],
                }
            })
            .expect("scenario fixture");
    }
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            telecat_theme::init(cx);
            cx.set_window_appearance(Some(WindowAppearance::Dark));
            let services = Services {
                model: cx.new(|_| Model {
                    import_error: None,
                    snapshot: profile.snapshot(),
                }),
                profile,
                runtime,
                scenario: !scenario.is_empty(),
            };
            if workspace::fixtures::is_workspace_scenario(&scenario) {
                workspace::open(
                    services.clone(),
                    None,
                    workspace::fixtures::connection(&scenario),
                    &scenario,
                    cx,
                );
            } else if ["03-form-mysql", "50-form-testing", "30-form-pg"]
                .contains(&scenario.as_str())
            {
                let mut c = empty_connection(if scenario == "30-form-pg" {
                    Driver::Postgres
                } else {
                    Driver::Mysql
                });
                let mut password = String::new();
                if scenario == "03-form-mysql" {
                    c.name = "Local".into();
                    c.port = 3307;
                    c.user = "root".into();
                    c.database = "zixma".into();
                    password = "secret".into();
                }
                if scenario == "50-form-testing" {
                    c.host = "10.255.255.1".into();
                }
                open_form(services.clone(), c, password, &scenario, cx);
            } else {
                let svc = services.clone();
                let name = if scenario.is_empty() {
                    "welcome"
                } else {
                    &scenario
                }
                .to_owned();
                gpui_kit::open_window(options(cx, 700., 480., true), cx, move |window, cx| {
                    print_window(window, &name, 700., 480., svc.scenario, cx);
                    let after_frame = svc.clone();
                    window.on_next_frame(move |window, _| {
                        // GPUI drains callbacks before presenting. A second one-shot callback
                        // guarantees the first scene has actually been sent to the display.
                        window.on_next_frame(move |_, cx| {
                            if let Some(timing) = launch_timing {
                                timing.report();
                            }
                            // No profile import or Keychain prompt before that first frame.
                            after_frame.start_import(cx);
                        });
                    });
                    cx.new(|cx| welcome::Welcome::new(svc, &name, window, cx))
                })
                .expect("welcome window");
            }
            install_menus(services.clone(), cx);
            cx.activate(true);
        });
}

gpui_kit::actions!(
    telecat,
    [Quit, NewConnection, SwitchConnection, CloseWindow]
);
fn install_menus(services: Services, cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewConnection, None),
        KeyBinding::new("cmd-shift-k", SwitchConnection, None),
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
    ]);
    cx.on_action(|_: &Quit, cx| {
        let mut dirty = false;
        for handle in cx.windows() {
            let state = handle
                .update(cx, |_, window, cx| {
                    if let Some(host) = workspace::host::root_host(window, cx) {
                        host.update(cx, |host, cx| host.flush(window, cx));
                    } else if let Some(view) = workspace::host::active_workspace(window, cx) {
                        view.update(cx, |w, cx| w.flush_grid_edits(window, cx));
                    }
                    workspace::host::close_state(window, cx)
                })
                .unwrap_or((false, false));
            if state.0 {
                return;
            }
            dirty |= state.1;
        }
        if !dirty {
            cx.quit();
            return;
        }
        if let Some(handle) = cx.active_window() {
            let prompt = handle.update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    "Discard unsaved changes?",
                    Some("Quitting discards all staged changes. SQL drafts are kept."),
                    &["Discard and Quit", "Cancel"],
                    cx,
                )
            });
            if let Ok(prompt) = prompt {
                cx.spawn(async move |cx| {
                    if prompt.await == Ok(0) {
                        let _ = cx.update(|cx| cx.quit());
                    }
                })
                .detach();
            }
        }
    });
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, cx| {
                if let Some(host) = workspace::host::root_host(window, cx) {
                    host.update(cx, |host, cx| host.close_window(window, cx));
                } else if let Some(workspace) = workspace::host::active_workspace(window, cx) {
                    workspace.update(cx, |this, cx| this.request_close_window(window, cx));
                } else {
                    window.remove_window();
                }
            });
        }
    });
    let svc = services.clone();
    cx.on_action(move |_: &NewConnection, cx| show_welcome(svc.clone(), true, cx));
    cx.on_action(move |_: &SwitchConnection, cx| show_welcome(services.clone(), false, cx));
    workspace::commands::install(cx);
    use workspace::commands::Command as Ws;
    cx.set_menus([
        Menu::new("Telecat").items([
            MenuItem::action("About Telecat", Ws::new("about")),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Telecat", Ws::new("hide")),
            MenuItem::action("Hide Others", Ws::new("hide-others")),
            MenuItem::action("Show All", Ws::new("show-all")),
            MenuItem::separator(),
            MenuItem::action("Quit Telecat", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Connection…", NewConnection),
            MenuItem::action("Switch Connection…", SwitchConnection),
            MenuItem::separator(),
            MenuItem::action("New Tab", Ws::new("new-query")),
            MenuItem::action("Open SQL Query", Ws::new("open-sql-query")),
            MenuItem::action("Open Anything…", Ws::new("quick-open")),
            MenuItem::action("Open SQL File…", Ws::new("open-file")),
            MenuItem::action("Save SQL As…", Ws::new("save-file")),
            MenuItem::separator(),
            MenuItem::action("Close Tab", Ws::new("close-tab")),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Undo", Ws::new("undo")),
            MenuItem::action("Redo", Ws::new("redo")),
            MenuItem::separator(),
            MenuItem::action("Cut", Ws::new("cut")),
            MenuItem::action("Copy", Ws::new("copy")),
            MenuItem::action("Paste", Ws::new("paste")),
            MenuItem::action("Paste and Match Style", Ws::new("paste-match")),
            MenuItem::action("Delete", Ws::new("delete")),
            MenuItem::action("Select All", Ws::new("select-all")),
        ]),
        Menu::new("Database").items([
            MenuItem::action("Commit Changes", Ws::new("commit")),
            MenuItem::action("Preview Changes…", Ws::new("preview")),
            MenuItem::action("Discard Changes", Ws::new("discard")),
            MenuItem::separator(),
            MenuItem::action("Switch Database…", Ws::new("switch-db")),
            MenuItem::action("Running Queries", Ws::new("running-queries")),
            MenuItem::action("New Table", Ws::new("new-table")),
            MenuItem::separator(),
            MenuItem::action("Reload Workspace", Ws::new("refresh")),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", Ws::new("toggle-sidebar")),
            MenuItem::action("Toggle Console Log", Ws::new("toggle-console")),
            MenuItem::action("Toggle Right Panel", Ws::new("toggle-panel")),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", Ws::new("fullscreen")),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Ws::new("minimize")),
            MenuItem::action("Zoom", Ws::new("zoom")),
            MenuItem::separator(),
            MenuItem::action("Previous Tab", Ws::new("prev-tab")),
            MenuItem::action("Next Tab", Ws::new("next-tab")),
            MenuItem::action("Tab 1", Ws::new("tab-1")),
            MenuItem::action("Tab 2", Ws::new("tab-2")),
            MenuItem::action("Tab 3", Ws::new("tab-3")),
            MenuItem::action("Tab 4", Ws::new("tab-4")),
            MenuItem::action("Tab 5", Ws::new("tab-5")),
            MenuItem::action("Tab 6", Ws::new("tab-6")),
            MenuItem::action("Tab 7", Ws::new("tab-7")),
            MenuItem::action("Tab 8", Ws::new("tab-8")),
            MenuItem::action("Tab 9", Ws::new("tab-9")),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", Ws::new("front")),
        ]),
        Menu::new("Help").items([MenuItem::action("Keyboard Shortcuts", Ws::new("shortcuts"))]),
    ]);
}
fn show_welcome(services: Services, picker: bool, cx: &mut App) {
    for handle in cx.windows() {
        let found = handle
            .update(cx, |_, window, cx| {
                let root = window.root::<gpui_kit::base::Root>().flatten()?;
                let view = root
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<welcome::Welcome>()
                    .ok()?;
                view.update(cx, |v, cx| {
                    if picker {
                        v.show_picker(cx);
                    }
                });
                window.activate_window();
                Some(())
            })
            .ok()
            .flatten();
        if found.is_some() {
            return;
        }
    }
    let _ = gpui_kit::open_window(options(cx, 700., 480., true), cx, move |window, cx| {
        print_window(window, "welcome", 700., 480., services.scenario, cx);
        cx.new(|cx| {
            let mut welcome = welcome::Welcome::new(services, "", window, cx);
            if picker {
                welcome.show_picker(cx);
            }
            welcome
        })
    });
}
