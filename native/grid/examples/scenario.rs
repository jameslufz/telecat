#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/perf.rs"]
mod perf;
use gpui_kit::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, rc::Rc, time::Instant};
use telecat_grid::{DataGrid, GridEvent, RowState};

fn print_window(window: &Window, name: &str, width: f32, height: f32, cx: &mut App) {
    let Ok(raw) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(h) = raw.as_raw() else {
        return;
    };
    let Some(view) = (unsafe {
        objc2::rc::Retained::retain(h.ns_view.as_ptr() as *mut objc2::runtime::AnyObject)
    }) else {
        return;
    };
    let name = name.to_owned();
    // Native frame changes can re-enter GPUI. Defer outside its window/entity borrow.
    cx.spawn(async move |_| unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let w: *mut objc2::runtime::AnyObject = objc2::msg_send![&*view, window];
        let _: () = objc2::msg_send![w,setStyleMask:0usize];
        let mut frame: NSRect = objc2::msg_send![w, frame];
        frame.origin.y += frame.size.height - height as f64;
        frame.size = NSSize::new(width as f64, height as f64);
        let _: () = objc2::msg_send![w,setFrame: frame, display: false];
        let content: *mut objc2::runtime::AnyObject = objc2::msg_send![w, contentView];
        let bounds = NSRect::new(NSPoint::new(0., 0.), frame.size);
        let _: () = objc2::msg_send![content,setFrame:bounds];
        let _: () = objc2::msg_send![&*view,setFrame:bounds];
        let id: isize = objc2::msg_send![w, windowNumber];
        println!("WINDOW {name} {id}");
    })
    .detach();
}
struct Scenario {
    grid: Entity<DataGrid>,
    _events: Subscription,
}
impl Render for Scenario {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.grid.clone())
    }
}
struct Sweep {
    remaining: usize,
    last: Instant,
    intervals: Vec<f64>,
}
fn sweep(window: &mut Window, grid: Entity<DataGrid>, state: Rc<RefCell<Sweep>>) {
    window.on_next_frame(move |window, cx| {
        let mut s = state.borrow_mut();
        let now = Instant::now();
        let interval = now.duration_since(s.last).as_secs_f64() * 1000.;
        if s.remaining < 360 {
            s.intervals.push(interval);
        }
        s.last = now;
        if s.remaining == 0 {
            println!("PERF phys_footprint_bytes={:?}", perf::footprint());
            perf::report("scroll_frame_intervals", s.intervals.clone());
            grid.read_with(cx, |g, _| {
                perf::report("grid_prepare_paint_cpu", g.frame_stats.milliseconds());
                println!(
                    "PERF max_painted_cells={} model_storage_bytes={}",
                    g.frame_stats.max_cells,
                    g.model().storage_bytes()
                );
            });
            // No frame callback remains: idle is event-driven again.
            return;
        }
        let step = 360 - s.remaining;
        s.remaining -= 1;
        drop(s);
        grid.update(cx, |g, cx| {
            g.update_model(
                |m| {
                    m.scroll_y = step as f32 * 173.25;
                    m.scroll_x = (step / 120) as f32 * 800.;
                },
                cx,
            )
        });
        sweep(window, grid, state);
    });
}
fn main() {
    let profile = std::env::var_os("TELECAT_USER_DATA")
        .expect("set TELECAT_USER_DATA to a temporary directory");
    assert!(
        std::path::Path::new(&profile).is_absolute(),
        "TELECAT_USER_DATA must be absolute"
    );
    let name = std::env::var("TELECAT_SCENARIO")
        .unwrap_or_else(|_| std::env::args().nth(1).unwrap_or_else(|| "grid-07".into()));
    let (model, width, height) = fixtures::fixture(&name);
    let measured = name == "grid-100k" && std::env::var_os("TELECAT_GRID_PERF").is_some();
    println!(
        "SCENARIO {name} logical={width}x{height} model_storage_bytes={} phys_footprint_bytes={:?}",
        model.storage_bytes(),
        perf::footprint()
    );
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(width), px(height)),
                cx,
            ))),
            titlebar: None,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            let grid = cx.new(|cx| DataGrid::new(model, cx));
            grid.focus_handle(cx).focus(window, cx);
            print_window(window, &name, width, height, cx);
            if measured {
                sweep(
                    window,
                    grid.clone(),
                    Rc::new(RefCell::new(Sweep {
                        remaining: 360,
                        last: Instant::now(),
                        intervals: vec![],
                    })),
                );
            }
            cx.new(|cx| {
                let events = cx.subscribe(&grid, |_, grid, event, cx| {
                    println!("EVENT {event:?}");
                    grid.update(cx, |g, cx| match event {
                        GridEvent::Edit { cell, value, .. } => g.update_model(
                            |m| {
                                m.rows.set(cell.row, cell.col, Some(value.clone()));
                                m.edits.defaults.remove(cell);
                                m.edits.modified.insert(*cell);
                                m.edits.rows.entry(cell.row).or_insert(RowState::Modified);
                            },
                            cx,
                        ),
                        GridEvent::Sort(sort) => g.update_model(|m| m.sort = sort.clone(), cx),
                        _ => {}
                    });
                });
                Scenario {
                    grid,
                    _events: events,
                }
            })
        })
        .expect("open grid scenario");
        cx.activate(true);
    });
}
