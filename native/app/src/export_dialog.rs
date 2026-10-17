//! Native save dialog, including Electron's CSV / JSON / SQL INSERT filters.
//! AppKit widgets here are confined to the system file dialog.
use anyhow::{Context, Result};
use block2::RcBlock;
use futures::channel::oneshot;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSPopUpButton, NSSavePanel};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};
const FORMATS: [(&str, &str); 3] = [("CSV", "csv"), ("JSON", "json"), ("SQL INSERT", "sql")];
struct FormatState {
    panel: Retained<NSSavePanel>,
}
define_class!(
    #[unsafe(super=NSObject)] #[thread_kind=MainThreadOnly] #[ivars=FormatState]
    struct FormatTarget;
    unsafe impl NSObjectProtocol for FormatTarget {}
    impl FormatTarget {
        #[unsafe(method(chooseFormat:))]
        fn choose_format(&self, popup:&NSPopUpButton) {
            if let Some((_,extension))=FORMATS.get(popup.indexOfSelectedItem() as usize) {
                apply_format(&self.ivars().panel,extension);
            }
        }
    }
);
fn filename(name: &str, extension: &str) -> String {
    Path::new(name)
        .with_extension(extension)
        .to_string_lossy()
        .into_owned()
}
#[allow(deprecated)] // Same extension-based filters as Electron; supports older macOS too.
fn apply_format(panel: &NSSavePanel, extension: &str) {
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
        extension,
    )])));
    panel.setNameFieldStringValue(&NSString::from_str(&filename(
        &panel.nameFieldStringValue().to_string(),
        extension,
    )));
}
pub async fn choose_path(directory: &Path, name: &str) -> Result<Option<PathBuf>> {
    let mtm = MainThreadMarker::new().context("Export dialog must run on the main thread")?;
    let panel = NSSavePanel::savePanel(mtm);
    panel.setCanCreateDirectories(true);
    panel.setExtensionHidden(false);
    panel.setAllowsOtherFileTypes(false);
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath_isDirectory(
        &NSString::from_str(&directory.to_string_lossy()),
        true,
    )));
    panel.setNameFieldStringValue(&NSString::from_str(&format!("{name}.csv")));
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(240., 28.)),
        false,
    );
    popup.addItemsWithTitles(&NSArray::from_retained_slice(
        &FORMATS
            .iter()
            .map(|(label, _)| NSString::from_str(label))
            .collect::<Vec<_>>(),
    ));
    popup.selectItemAtIndex(0);
    let target = FormatTarget::alloc(mtm).set_ivars(FormatState {
        panel: panel.clone(),
    });
    let target: Retained<FormatTarget> = unsafe { msg_send![super(target), init] };
    unsafe {
        popup.setTarget(Some(&target));
        popup.setAction(Some(sel!(chooseFormat:)));
    }
    panel.setAccessoryView(Some(&popup));
    apply_format(&panel, "csv");
    let (tx, rx) = oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let block = RcBlock::new(move |response: isize| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(response);
        }
    });
    panel.beginWithCompletionHandler(&block);
    let response = rx.await;
    // Keep the non-retained target alive until AppKit has finished the dialog.
    unsafe {
        popup.setTarget(None);
    }
    panel.setAccessoryView(None);
    drop(target);
    let response = response.context("Export dialog closed without a response")?;
    Ok(if response == 1 {
        panel
            .URL()
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()))
    } else {
        None
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn format_changes_preserve_names_and_select_the_matching_writer_extension() {
        for (label, ext) in super::FORMATS {
            assert!(!label.is_empty());
            assert_eq!(
                super::filename("ธนาคาร 🐈.v2.csv", ext),
                format!("ธนาคาร 🐈.v2.{ext}")
            );
        }
    }
}
