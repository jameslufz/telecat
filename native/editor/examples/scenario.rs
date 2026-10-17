use gpui_kit::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use telecat_editor::{EditorEvent, EditorOptions, Schema, Selection, SqlEditor, theme};
use telecat_editor_engine::input::{Editor, EditorState};
const SQL: &str = "SELECT * FROM members\nWHERE 1\n-- AND MEMBER_TYPE = 'x;y'\nAND MEMBER_TYPE = 'member'\nORDER BY MEMBER_ID DESC;\n\nSELECT bank_name_th FROM sa_banks;";
const PREVIEW: &str = "INSERT INTO `zixma`.`sa_banks` (`bank_code`, `bank_name_th`, `bank_name_en`, `bank_acronym`, `logo`, `country_code`, `is_disabled`, `created_at`, `swift_code`) VALUES ('002', 'ธนาคารกรุงเทพ', 'BANGKOK EDITED', 'BBL', NULL, 'TH', '0', '2026-10-17 00:35:38', NULL);\n\nINSERT INTO `zixma`.`sa_banks` () VALUES ();";
fn schema() -> Schema {
    let mut schema = Schema::new();
    for name in [
        "QA_Mixed", "members", "nopk", "qa_bit", "qa_blob", "qa_char", "qa_ci", "qa_cpk", "qa_dt",
        "qa_dup", "qa_dup2", "qa_float", "qa_g", "qa_json", "qa_pad",
    ] {
        schema.insert(name.into(), vec![]);
    }
    schema.insert(
        "sa_banks".into(),
        [
            "id",
            "bank_code",
            "bank_name_th",
            "bank_name_en",
            "bank_acronym",
            "logo",
            "country_code",
            "is_disabled",
            "created_at",
            "swift_code",
        ]
        .map(str::to_string)
        .to_vec(),
    );
    schema.insert(
        "members".into(),
        ["MEMBER_ID", "MEMBER_TYPE"].map(str::to_string).to_vec(),
    );
    schema
}
struct Scenario {
    editor: Option<Entity<SqlEditor>>,
    preview: Option<Entity<EditorState>>,
    _events: Option<Subscription>,
}
impl Render for Scenario {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(editor) = &self.editor {
            return div().size_full().child(editor.clone());
        }
        let mut d = div()
            .size_full()
            .p(px(10.))
            .border_1()
            .border_color(rgb(0x383838))
            .rounded(px(8.))
            .bg(rgb(0x1a1a1a))
            .text_color(rgb(0xe8e8e8))
            .font_family(theme::mono().family)
            .text_size(px(12.))
            .line_height(px(14.4));
        d.style().text.font_fallbacks = theme::mono().fallbacks;
        d.child(Editor::new(self.preview.as_ref().unwrap()))
    }
}
fn main() {
    let profile = std::env::var("TELECAT_USER_DATA")
        .expect("use editor/scripts/scenario.sh (temporary profile required)");
    assert!(std::path::Path::new(&profile).is_absolute());
    let name = std::env::var("TELECAT_SCENARIO").unwrap_or_else(|_| "editor-05".into());
    assert!(matches!(
        name.as_str(),
        "editor-05" | "editor-a1" | "editor-autocomplete" | "editor-empty" | "editor-latency"
    ));
    gpui_kit::application().run(move|cx|{
  gpui_kit::init(cx);telecat_editor::init(cx);
  let (width,height)=if name=="editor-a1"{(638.,109.)}else{(848.,237.)};
  gpui_kit::open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(Bounds::centered(None,size(px(width),px(height)),cx))),titlebar:None,is_resizable:false,window_background:WindowBackgroundAppearance::Opaque,..Default::default()},cx,move|window,cx|{
   if let Ok(handle)=HasWindowHandle::window_handle(window){if let RawWindowHandle::AppKit(h)=handle.as_raw(){unsafe{let view=h.ns_view.as_ptr()as*mut objc2::runtime::AnyObject;let w:*mut objc2::runtime::AnyObject=objc2::msg_send![view,window];let id:isize=objc2::msg_send![w,windowNumber];println!("WINDOW {name} {id}");}}}
   cx.new(|cx|{
    if name=="editor-a1"{
     let preview=cx.new(|cx|{let mut s=EditorState::new(window,cx).default_value(PREVIEW).line_number(false).folding(false).indent_guides(false).soft_wrap(true).cursor_surrounding_lines(Some(0));s.set_readonly(true,cx);let mut style=theme::style();style.background=rgb(0x1a1a1a).into();style.foreground=rgb(0xe8e8e8).into();style.editor_active_line=None;s.set_editor_style(style);s.set_editor_paddings(Edges::default());s});
     return Scenario{editor:None,preview:Some(preview),_events:None};
    }
    let text=if name=="editor-autocomplete"||name=="editor-latency"{format!("{SQL}\n\ns")}else if name=="editor-empty"{String::new()}else{SQL.into()};let end=text.encode_utf16().count();
    let editor=cx.new(|cx|SqlEditor::new(EditorOptions{text,schema:schema(),..Default::default()},window,cx));
    editor.update(cx,|s,cx|{s.set_selections(&[Selection{anchor:end,head:end}],cx);s.focus(window,cx);});
    if name=="editor-latency" {
     let probe=telecat_editor::latency::LatencyProbe::default();let e=editor.clone();let p=probe.clone();
     let large_schema:Schema=(0..10_000).map(|i|(format!("schema_table_{i:05}"),vec!["id".into(),"bank_name_th".into(),"created_at".into()])).collect();
     editor.update(cx,|s,cx|{s.set_schema(&large_schema,cx);s.set_latency_probe(p,cx);});
     window.spawn(cx,async move|cx|{
      use std::time::Duration;
      cx.background_executor().timer(Duration::from_millis(100)).await;
      for n in 0..220 {
       if n==20 {probe.clear();}
       let before=probe.summary().count;
       let _=cx.update(|window,cx|{probe.begin();window.dispatch_keystroke(Keystroke::parse(if n%2==0{"e"}else{"backspace"}).unwrap(),cx);});
       let mut painted=false;
       for _ in 0..100 {cx.background_executor().timer(Duration::from_millis(2)).await;if probe.summary().count>before{painted=true;break;}}
       if !painted {eprintln!("LATENCY sample {n} did not paint within 200ms");return;}
      }
      let s=probe.summary();println!("LATENCY native CPU-to-paint n={} p50={:.3}ms p95={:.3}ms max={:.3}ms schema=10000",s.count,s.p50_ms,s.p95_ms,s.max_ms);
      let _=cx.update(|_,cx|cx.quit());drop(e);
     }).detach();
    }
    let popup=name=="editor-autocomplete";let freeze=name!="editor-latency";
    // After focus effects and first layout, freeze the screenshot caret. No repeating scenario timers.
    let e=editor.clone();window.on_next_frame(move|window,cx|{e.update(cx,|s,cx|{if freeze{s.freeze_caret(true,cx);}if popup{s.request_completion(true,window,cx);}});});
    let events=cx.subscribe_in(&editor,window,|_,editor,event,w,cx|match event{EditorEvent::Beautify=>editor.update(cx,|s,cx|s.beautify(w,cx)),EditorEvent::Uglify=>editor.update(cx,|s,cx|s.uglify(w,cx)),_=>{}});
    Scenario{editor:Some(editor),preview:None,_events:Some(events)}
   })
  }).expect("open editor scenario");cx.activate(true);
 });
}
