#![cfg(target_os = "macos")]
use gpui_apple::AppleTextSystem;
use gpui_kit::{Font, FontRun, PlatformTextSystem, px};
#[test]
fn native_mono_and_thai_fallback_shape() {
    let ts = AppleTextSystem::new();
    let family = telecat_theme::resolve_mono(&ts.all_font_names());
    let font = Font {
        family: family.into(),
        ..telecat_theme::mono()
    };
    let id = ts.font_id(&font).unwrap();
    let shape = |text: &str, id| {
        ts.layout_line(
            text,
            px(13.),
            &[FontRun {
                len: text.len(),
                font_id: id,
            }],
        )
    };
    let narrow = shape("iiiiiiii", id);
    let wide = shape("WWWWWWWW", id);
    assert!(
        (narrow.width - wide.width).as_f32().abs() < 0.01,
        "primary family is proportional: {family}"
    );
    let thai = "ธนาคารกรุงเทพ กิ่ กุ้ น้ำ";
    let fallback = shape(thai, id);
    let thonburi = ts
        .font_id(&Font {
            family: "Thonburi".into(),
            ..Default::default()
        })
        .unwrap();
    let direct = shape(thai, thonburi);
    // Compare each Thai cluster independently: spaces intentionally stay monospaced.
    for word in thai.split(' ') {
        let a = shape(word, id);
        let b = shape(word, thonburi);
        let glyphs = |line: gpui_kit::LineLayout| {
            line.runs
                .into_iter()
                .flat_map(|r| r.glyphs)
                .map(|g| (g.id, g.position, g.index))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            glyphs(a),
            glyphs(b),
            "Thai fallback must shape exactly as Thonburi: {word}"
        );
    }
    let mixed = shape("SELECT 'ธนาคารกรุงเทพ 😺';", id);
    assert!(mixed.runs.len() >= 3, "Latin/Thai/emoji must use a cascade");
    assert!(
        mixed
            .runs
            .iter()
            .flat_map(|r| &r.glyphs)
            .all(|g| g.id.0 != 0)
    );
    println!(
        "native font={family}; ASCII width={:?}; Thai fallback width={:?}, Thonburi={:?}; mixed runs={}",
        narrow.width,
        fallback.width,
        direct.width,
        mixed.runs.len()
    );
}
