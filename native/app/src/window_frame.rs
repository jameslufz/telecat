//! Exact sizing is a scenario-only operation. Never replace AppKit window methods.
use anyhow::{Context, Result, bail, ensure};
use objc2_app_kit::{NSScreen, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

/// Test runs only: `TELECAT_TEST_SCREEN="x,y,width,height"` is the display, in GPUI's top-left
/// global coordinates, that windows open on. They then open there without taking focus.
pub fn test_screen() -> Option<[f32; 4]> {
    parse_test_screen(&std::env::var("TELECAT_TEST_SCREEN").ok()?)
}
fn parse_test_screen(value: &str) -> Option<[f32; 4]> {
    let parts: Vec<f32> = value
        .split(',')
        .map(|p| p.trim().parse().ok().filter(|v: &f32| v.is_finite()))
        .collect::<Option<_>>()?;
    match parts[..] {
        [x, y, w, h] if w > 0. && h > 0. => Some([x, y, w, h]),
        _ => None,
    }
}
/// The display a placed window ended up on, in AppKit coordinates, for test verification.
pub fn screen_frame(view: &NSView) -> Option<NSRect> {
    Some(view.window()?.screen()?.frame())
}

fn positive(size: NSSize) -> bool {
    size.width.is_finite() && size.height.is_finite() && size.width > 0. && size.height > 0.
}

/// AppKit global coordinates, including screens left of or below the primary.
/// Filter by BOTH dimensions before preferring the largest usable display.
fn destination(visible: &[NSRect], requested: NSSize) -> Result<(usize, NSRect)> {
    ensure!(
        positive(requested),
        "Invalid requested scenario size: {requested:?}"
    );
    let (index, screen) = visible
        .iter()
        .enumerate()
        .filter(|(_, screen)| {
            positive(screen.size)
                && screen.origin.x.is_finite()
                && screen.origin.y.is_finite()
                && screen.size.width >= requested.width
                && screen.size.height >= requested.height
        })
        .max_by(|(_, a), (_, b)| {
            (a.size.width * a.size.height).total_cmp(&(b.size.width * b.size.height))
        })
        .with_context(|| {
            format!("No screen visibleFrame can hold {requested:?}; available: {visible:?}")
        })?;
    Ok((
        index,
        NSRect::new(
            NSPoint::new(
                screen.origin.x + ((screen.size.width - requested.width) / 2.).floor(),
                screen.origin.y + ((screen.size.height - requested.height) / 2.).floor(),
            ),
            requested,
        ),
    ))
}

fn check_sizes(requested: Option<NSSize>, sizes: &[(&str, NSSize)]) -> Result<()> {
    if let Some(size) = requested {
        ensure!(positive(size), "Invalid requested scenario size: {size:?}");
    }
    for (label, actual) in sizes {
        ensure!(positive(*actual), "Invalid {label} size: {actual:?}");
        if let Some(expected) = requested {
            ensure!(
                *actual == expected,
                "{label} size mismatch: requested {expected:?}, got {actual:?}"
            );
        }
    }
    Ok(())
}

/// Call outside GPUI borrows: moving/resizing invokes the platform callbacks.
/// The window keeps its original class and normal AppKit constraint behavior.
pub fn place_scenario(view: &NSView, requested: NSSize) -> Result<()> {
    let window = view
        .window()
        .context("Scenario view has no native window")?;
    let mtm = MainThreadMarker::new().context("Scenario placement must run on the main thread")?;
    let screens = NSScreen::screens(mtm);
    let visible: Vec<_> = screens.iter().map(|screen| screen.visibleFrame()).collect();
    let (index, frame) = destination(&visible, requested)?;

    // Move first so AppKit uses the destination screen's constraints when resizing.
    // This avoids shrinking the desired frame against the smaller built-in display.
    window.setFrameOrigin(frame.origin);
    window.setFrame_display(frame, false);
    let actual = window.frame();
    check_sizes(Some(requested), &[("window frame", actual.size)])?;
    let screen = visible[index];
    if actual.origin.x < screen.origin.x
        || actual.origin.y < screen.origin.y
        || actual.origin.x + actual.size.width > screen.origin.x + screen.size.width
        || actual.origin.y + actual.size.height > screen.origin.y + screen.size.height
    {
        bail!("Scenario frame {actual:?} is outside selected visibleFrame {screen:?}");
    }

    // These windows use full-size content views. Preserve the accepted M2 outer
    // size/titlebar inset correction, but only after validating the actual frame.
    let content = window
        .contentView()
        .context("Scenario window has no content view")?;
    let bounds = NSRect::new(NSPoint::new(0., 0.), requested);
    content.setFrame(bounds);
    view.setFrame(bounds);
    check_window(view, Some(requested), None)?;
    // Moving between displays may change key status. Scenario captures must use
    // the real active window appearance, including AppKit's traffic lights.
    if test_screen().is_some() {
        window.orderFront(None);
    } else {
        window.makeKeyAndOrderFront(None);
    }
    Ok(())
}

/// Recheck after the next GPUI frame, before publishing the capturable window ID.
/// Production windows must be nonzero, but may be constrained below requested size.
pub fn check_window(
    view: &NSView,
    requested: Option<NSSize>,
    viewport: Option<NSSize>,
) -> Result<isize> {
    let window = view.window().context("View has no native window")?;
    let content = window.contentView().context("Window has no content view")?;
    let mut sizes = vec![
        ("window frame", window.frame().size),
        ("content frame", content.frame().size),
        ("content bounds", content.bounds().size),
        ("GPUI view frame", view.frame().size),
        ("GPUI view bounds", view.bounds().size),
    ];
    if let Some(viewport) = viewport {
        sizes.push(("GPUI viewport", viewport));
    }
    check_sizes(requested, &sizes)?;
    let id = window.windowNumber();
    ensure!(id > 0, "Invalid native window number: {id}");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }
    #[test]
    fn workspace_uses_largest_screen_that_fits_both_dimensions() {
        // Built-in Retina display is too small in points. The external display
        // fits; a larger-area portrait screen still cannot fit the width.
        let screens = [
            rect(0., 25., 1280., 775.),
            rect(1280., -1080., 1920., 1055.),
            rect(-1000., 0., 1000., 3000.),
        ];
        for height in [815., 816.] {
            let (index, frame) = destination(&screens, NSSize::new(1440., height)).unwrap();
            assert_eq!(index, 1);
            assert_eq!(
                frame,
                rect(
                    1520.,
                    -1080. + ((1055. - height) / 2.).floor(),
                    1440.,
                    height
                )
            );
        }
        assert_eq!(
            destination(&screens[..2], NSSize::new(700., 480.))
                .unwrap()
                .0,
            1
        );
        assert!(destination(&screens[..1], NSSize::new(1440., 816.)).is_err());
        assert!(destination(&[], NSSize::new(1440., 816.)).is_err());
    }
    #[test]
    fn exact_fit_and_invalid_geometry() {
        assert_eq!(
            destination(
                &[rect(-1440., -816., 1440., 816.)],
                NSSize::new(1440., 816.)
            )
            .unwrap()
            .1,
            rect(-1440., -816., 1440., 816.)
        );
        assert!(destination(&[rect(0., 0., 1920., 1080.)], NSSize::new(0., 816.)).is_err());
        assert!(
            destination(
                &[rect(f64::NAN, 0., 1920., 1080.)],
                NSSize::new(1440., 816.)
            )
            .is_err()
        );
    }
    #[test]
    fn every_frame_and_content_layer_must_be_nonzero_and_exact() {
        let expected = NSSize::new(1440., 816.);
        let layers = [
            "window frame",
            "content frame",
            "content bounds",
            "GPUI view frame",
            "GPUI view bounds",
            "GPUI viewport",
        ];
        let good: Vec<_> = layers.into_iter().map(|label| (label, expected)).collect();
        check_sizes(Some(expected), &good).unwrap();
        for i in 0..good.len() {
            for bad in [
                NSSize::new(0., 0.),
                NSSize::new(1440., 813.),
                NSSize::new(1440., 817.),
                NSSize::new(f64::NAN, 816.),
            ] {
                let mut sizes = good.clone();
                sizes[i].1 = bad;
                assert!(
                    check_sizes(Some(expected), &sizes).is_err(),
                    "{} accepted {bad:?}",
                    sizes[i].0
                );
            }
        }
        // Production accepts AppKit's smaller positive frame; it never forces 1440x816.
        check_sizes(None, &[("window frame", NSSize::new(1280., 775.))]).unwrap();
        assert!(check_sizes(None, &[("window frame", NSSize::new(0., 0.))]).is_err());
    }
}

#[cfg(test)]
mod test_screen_tests {
    use super::parse_test_screen;
    #[test]
    fn test_screen_accepts_only_four_finite_numbers_with_a_positive_size() {
        assert_eq!(
            parse_test_screen("168,-1080,1920,1080"),
            Some([168., -1080., 1920., 1080.])
        );
        assert_eq!(
            parse_test_screen(" 0, 0 , 10, 10"),
            Some([0., 0., 10., 10.])
        );
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "a,0,1,1",
            "0,0,0,10",
            "0,0,10,-1",
            "NaN,0,1,1",
            "inf,0,1,1",
        ] {
            assert_eq!(parse_test_screen(bad), None, "{bad}");
        }
    }
}
