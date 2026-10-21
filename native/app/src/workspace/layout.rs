//! Electron Workspace.tsx / QueryTab.tsx splitters and shell scrollbars.
use super::*;
#[derive(Clone, Copy, Debug)]
pub(super) enum Splitter {
    Sidebar,
    Panel,
    Console,
    Query(u64),
}
#[derive(Clone)]
enum Drag {
    Split {
        kind: Splitter,
        origin: Point<Pixels>,
        start: f32,
    },
    Scroll {
        handle: ScrollHandle,
        y: f32,
        offset: f32,
        scale: f32,
    },
}
/// Narrowest sidebar that still shows the Items / Queries / History control in full.
pub(super) const SIDEBAR_MIN: f32 = 240.;
pub(super) const SIDEBAR_MAX: f32 = 520.;
pub(super) struct Layout {
    pub sidebar: f32,
    pub panel: f32,
    pub console: f32,
    pub query_splits: std::collections::BTreeMap<u64, f32>,
    pub query_bounds: Bounds<Pixels>,
    drag: Option<Drag>,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: 270.,
            panel: 320.,
            console: 170.,
            query_splits: Default::default(),
            query_bounds: Default::default(),
            drag: None,
        }
    }
}
impl Layout {
    pub fn set_sidebar(&mut self, width: f32) {
        self.sidebar = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }
    pub fn query_split(&self, id: u64) -> f32 {
        self.query_splits.get(&id).copied().unwrap_or(0.5)
    }
    fn start(&mut self, kind: Splitter, origin: Point<Pixels>) {
        let start = match kind {
            Splitter::Sidebar => self.sidebar,
            Splitter::Panel => self.panel,
            Splitter::Console => self.console,
            Splitter::Query(id) => self.query_split(id),
        };
        self.drag = Some(Drag::Split {
            kind,
            origin,
            start,
        });
    }
    fn drag_to(&mut self, position: Point<Pixels>) -> bool {
        let Some(drag) = &self.drag else { return false };
        match drag {
            Drag::Split {
                kind,
                origin,
                start,
            } => {
                let dx = (position.x - origin.x).as_f32();
                let dy = (position.y - origin.y).as_f32();
                match kind {
                    Splitter::Sidebar => self.set_sidebar(start + dx),
                    Splitter::Panel => self.panel = (start - dx).clamp(220., 600.),
                    Splitter::Console => self.console = (start - dy).clamp(60., 500.),
                    Splitter::Query(id) => {
                        let height = self.query_bounds.size.height.as_f32();
                        if height > 0. {
                            self.query_splits.insert(
                                *id,
                                ((position.y - self.query_bounds.top()).as_f32() / height)
                                    .clamp(0.15, 0.85),
                            );
                        }
                    }
                }
            }
            Drag::Scroll {
                handle,
                y,
                offset,
                scale,
            } => {
                let value = (offset + (position.y.as_f32() - y) * scale)
                    .clamp(0., handle.max_offset().y.as_f32().max(0.));
                handle.set_offset(point(handle.offset().x, px(-value)));
            }
        }
        true
    }
}
impl Workspace {
    pub(super) fn splitter(&self, kind: Splitter, cx: &mut Context<Self>) -> Div {
        let vertical = matches!(kind, Splitter::Sidebar | Splitter::Panel);
        let id = match kind {
            Splitter::Sidebar => "split-sidebar",
            Splitter::Panel => "split-panel",
            Splitter::Console => "split-console",
            Splitter::Query(_) => "split-query",
        };
        let hit = div()
            .id(id)
            .absolute()
            .when(vertical, |d| {
                d.left(px(-3.))
                    .top_0()
                    .bottom_0()
                    .w(px(7.))
                    .cursor_col_resize()
            })
            .when(!vertical, |d| {
                d.top(px(-3.))
                    .left_0()
                    .right_0()
                    .h(px(7.))
                    .cursor_row_resize()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    if this.saving || this.sheet.is_some() {
                        return;
                    }
                    this.layout.start(kind, e.position);
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            );
        div()
            .relative()
            .flex_shrink_0()
            .bg(rgb(0x111111))
            .when(vertical, |d| d.w(px(1.)).h_full())
            .when(!vertical, |d| d.h(px(1.)).w_full())
            .child(hit)
    }
    pub(super) fn layout_listeners(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let weak_move = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let _ = weak_move.update(cx, |this, cx| {
                        if e.pressed_button != Some(MouseButton::Left) {
                            this.layout.drag = None;
                            return;
                        }
                        if this.layout.drag_to(e.position) {
                            window.prevent_default();
                            cx.stop_propagation();
                            cx.notify();
                        }
                    });
                });
                let weak_up = weak.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && e.button == MouseButton::Left {
                        let _ = weak_up.update(cx, |this, cx| {
                            if this.layout.drag.take().is_some() {
                                cx.stop_propagation();
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
    pub(super) fn query_measure(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        canvas(
            move |bounds, _, cx| {
                let _ = weak.update(cx, |this, _| this.layout.query_bounds = bounds);
            },
            |_, _, _, _| (),
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}
#[derive(Debug)]
struct Thumb {
    length: f32,
    top: f32,
    scale: f32,
}
impl Thumb {
    fn new(height: f32, max: f32, offset: f32) -> Self {
        let length = (height * height / (height + max)).max(22.).min(height);
        let travel = (height - length).max(0.);
        Self {
            length,
            top: if max > 0. {
                (-offset / max).clamp(0., 1.) * travel
            } else {
                0.
            },
            scale: max / travel.max(1.),
        }
    }
}
/// Electron's 10 px scrollbar with a 2 px transparent thumb border.
pub(super) fn scrollbar(handle: &ScrollHandle, cx: &mut Context<Workspace>) -> Div {
    let handle = handle.clone();
    let weak = cx.entity().downgrade();
    div()
        .absolute()
        .right_0()
        .top_0()
        .bottom_0()
        .w(px(10.))
        .child(
            canvas(
                move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hit, window, _| {
                    let max = handle.max_offset().y.as_f32();
                    if max <= 0. {
                        return;
                    }
                    let thumb =
                        Thumb::new(bounds.size.height.as_f32(), max, handle.offset().y.as_f32());
                    let rect = Bounds::new(
                        bounds.origin + point(px(2.), px(thumb.top + 2.)),
                        size(px(6.), px((thumb.length - 4.).max(0.))),
                    );
                    window.paint_quad(
                        fill(
                            rect,
                            rgba(if hit.is_hovered(window) {
                                0xffffff50
                            } else {
                                0xffffff30
                            }),
                        )
                        .corner_radii(px(5.)),
                    );
                    let handle = handle.clone();
                    let weak = weak.clone();
                    window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble
                            || e.button != MouseButton::Left
                            || !hit.is_hovered(window)
                        {
                            return;
                        }
                        let _ = weak.update(cx, |this, cx| {
                            if this.saving {
                                return;
                            }
                            let y = (e.position.y - bounds.top()).as_f32();
                            if y < thumb.top || y > thumb.top + thumb.length {
                                handle.set_offset(point(
                                    handle.offset().x,
                                    px(-((y - thumb.length / 2.) * thumb.scale).clamp(0., max)),
                                ));
                            }
                            this.layout.drag = Some(Drag::Scroll {
                                handle: handle.clone(),
                                y: e.position.y.as_f32(),
                                offset: -handle.offset().y.as_f32(),
                                scale: thumb.scale,
                            });
                            window.prevent_default();
                            cx.stop_propagation();
                            cx.notify();
                        });
                    });
                },
            )
            .size_full(),
        )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn electron_splitter_limits_and_scroll_geometry() {
        let mut l = Layout::default();
        for (kind, min, max) in [
            (Splitter::Sidebar, SIDEBAR_MIN, SIDEBAR_MAX),
            (Splitter::Panel, 220., 600.),
            (Splitter::Console, 60., 500.),
        ] {
            l.start(kind, point(px(0.), px(0.)));
            for (p, high) in [(10000., true), (-10000., false)] {
                l.drag_to(point(px(p), px(p)));
                let (actual, high) = match kind {
                    Splitter::Sidebar => (l.sidebar, high),
                    Splitter::Panel => (l.panel, !high),
                    _ => (l.console, !high),
                };
                assert_eq!(actual, if high { max } else { min });
            }
        }
        l.query_bounds = Bounds::new(point(px(0.), px(100.)), size(px(800.), px(500.)));
        l.start(Splitter::Query(3), point(px(0.), px(350.)));
        for (y, expected) in [(100., 0.15), (350., 0.5), (600., 0.85)] {
            l.drag_to(point(px(0.), px(y)));
            assert_eq!(l.query_split(3), expected);
        }
        assert_eq!(l.query_split(4), 0.5);
        let thumb = Thumb::new(200., 800., -800.);
        assert_eq!((thumb.length, thumb.top, thumb.scale), (40., 160., 5.));
        l.drag = None;
        assert!(!l.drag_to(point(px(0.), px(0.))));
    }
    #[::core::prelude::v1::test]
    fn sidebar_width_is_clamped_on_drag_and_restore() {
        let mut layout = Layout::default();
        layout.set_sidebar(40.);
        assert_eq!(layout.sidebar, SIDEBAR_MIN);
        layout.set_sidebar(5000.);
        assert_eq!(layout.sidebar, SIDEBAR_MAX);
        layout.start(Splitter::Sidebar, point(px(300.), px(0.)));
        layout.drag_to(point(px(0.), px(0.)));
        assert_eq!(
            layout.sidebar, SIDEBAR_MIN,
            "dragging far left stops at the minimum"
        );
        assert!(Layout::default().sidebar >= SIDEBAR_MIN);
    }
}
