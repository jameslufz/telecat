//! AppKit is used only for native menus (as in Electron), never form controls.
use gpui_kit::{Context, Pixels, Point, Window};
use objc2::rc::Retained;
use objc2::{AllocAnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSBezierPath, NSColor, NSImage, NSMenu, NSMenuItem, NSView};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::Value;
use std::cell::Cell;
#[derive(Default)]
struct Selection {
    index: Cell<isize>,
}
define_class!(
 #[unsafe(super=NSObject)] #[thread_kind=MainThreadOnly] #[ivars=Selection]
 struct MenuTarget;
 unsafe impl NSObjectProtocol for MenuTarget {}
 impl MenuTarget {
  #[unsafe(method(choose:))] fn choose(&self,item:&NSMenuItem){self.ivars().index.set(item.tag());}
 }
);
fn build(
    items: &[Value],
    target: &MenuTarget,
    ids: &mut Vec<String>,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    menu.setAutoenablesItems(false);
    for v in items {
        if v["type"] == "separator" {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            continue;
        }
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(v["label"].as_str().unwrap_or("")),
                Some(sel!(choose:)),
                &NSString::from_str(""),
            )
        };
        item.setEnabled(v["enabled"].as_bool().unwrap_or(true));
        unsafe {
            let _: () = msg_send![&*item,setState:if v["checked"]==true {1isize}else{0isize}];
        }
        if let Some(kind) = v["icon"].as_str() {
            item.setImage(Some(&menu_icon(
                kind,
                v["color"].as_str().unwrap_or("#ffffff"),
            )));
        }
        item.setTag(ids.len() as isize);
        ids.push(v["id"].as_str().unwrap_or("").into());
        unsafe {
            item.setTarget(Some(target));
        }
        if let Some(sub) = v["submenu"].as_array() {
            item.setSubmenu(Some(&build(sub, target, ids, mtm)));
        }
        menu.addItem(&item);
    }
    menu
}
/// Menu tracking runs on the foreground executor after all GPUI borrows have ended.
/// AppKit pumps a nested event loop; keeping &mut App/Window across it would reenter GPUI.
pub fn choose<T: 'static>(
    items: Value,
    position: Point<Pixels>,
    window: &Window,
    cx: &mut Context<T>,
    apply: impl FnOnce(&mut T, String, &mut Window, &mut Context<T>) + 'static,
) {
    let Ok(raw) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(h) = raw.as_raw() else {
        return;
    };
    let Some(view) = (unsafe { Retained::retain(h.ns_view.as_ptr() as *mut NSView) }) else {
        return;
    };
    let menu = cx
        .foreground_executor()
        .spawn(async move { show(items, position, &view) });
    let handle = window.window_handle();
    cx.spawn(async move |this, cx| {
        if let Some(id) = menu.await {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| apply(this, id, window, cx));
            });
        }
    })
    .detach();
}
fn show(items: Value, position: Point<Pixels>, view: &NSView) -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    let target = unsafe {
        msg_send![
            super(MenuTarget::alloc(mtm).set_ivars(Selection {
                index: Cell::new(-1)
            })),
            init
        ]
    };
    let target: Retained<MenuTarget> = target;
    let mut ids = vec![];
    let menu = build(items.as_array()?, &target, &mut ids, mtm);
    let y = f32::from(position.y) as f64;
    let y = if view.isFlipped() {
        y
    } else {
        view.bounds().size.height - y
    };
    let checked = items
        .as_array()?
        .iter()
        .position(|v| v["checked"] == true)
        .and_then(|n| menu.itemAtIndex(n as isize));
    menu.popUpMenuPositioningItem_atLocation_inView(
        checked.as_deref(),
        NSPoint::new(f32::from(position.x) as f64, y),
        Some(view),
    );
    ids.get(target.ivars().index.get() as usize).cloned()
}

// The same 32 px paths as PopupSelect.iconPng, with a 16 pt logical size.
fn menu_icon(kind: &str, hex: &str) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(16., 16.));
    unsafe {
        let _: () = msg_send![&*image,lockFocusFlipped:true];
    }
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0xffffff);
    NSColor::colorWithSRGBRed_green_blue_alpha(
        ((rgb >> 16) & 255) as f64 / 255.,
        ((rgb >> 8) & 255) as f64 / 255.,
        (rgb & 255) as f64 / 255.,
        1.,
    )
    .set();
    let path = NSBezierPath::bezierPath();
    path.setLineWidth(1.3);
    if kind == "unlock" {
        path.appendBezierPathWithRect(NSRect::new(NSPoint::new(2.5, 7.5), NSSize::new(9., 6.5)));
        path.moveToPoint(NSPoint::new(8.5, 7.5));
        path.lineToPoint(NSPoint::new(8.5, 5.));
        path.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            NSPoint::new(11.5, 5.),
            3.,
            180.,
            360.,
            false,
        );
        path.lineToPoint(NSPoint::new(14.5, 6.5));
        path.stroke();
    } else {
        let radius = if kind == "dot" { 6. } else { 5.5 };
        path.appendBezierPathWithOvalInRect(NSRect::new(
            NSPoint::new(8. - radius, 8. - radius),
            NSSize::new(radius * 2., radius * 2.),
        ));
        if kind == "dot" {
            path.fill();
        } else {
            path.stroke();
        }
    }
    unsafe {
        let _: () = msg_send![&*image, unlockFocus];
    }
    image
}
