use gpui_kit::*;
use std::borrow::Cow;
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "spinner.svg" => Some(Cow::Borrowed(include_bytes!("../assets/spinner.svg"))),
            "ChevronDown.svg" => Some(Cow::Borrowed(include_bytes!("../assets/ChevronDown.svg"))),
            "ChevronLeft.svg" => Some(Cow::Borrowed(include_bytes!("../assets/ChevronLeft.svg"))),
            "ChevronRight.svg" => Some(Cow::Borrowed(include_bytes!("../assets/ChevronRight.svg"))),
            "Console.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Console.svg"))),
            "Database.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Database.svg"))),
            "Eye.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Eye.svg"))),
            "Filter.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Filter.svg"))),
            "Folder.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Folder.svg"))),
            "Function.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Function.svg"))),
            "History.svg" => Some(Cow::Borrowed(include_bytes!("../assets/History.svg"))),
            "Key.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Key.svg"))),
            "Lock.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Lock.svg"))),
            "Minus.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Minus.svg"))),
            "PanelRight.svg" => Some(Cow::Borrowed(include_bytes!("../assets/PanelRight.svg"))),
            "Pin.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Pin.svg"))),
            "Play.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Play.svg"))),
            "Plug.svg" => Some(Cow::Borrowed(include_bytes!("../assets/plug.svg"))),
            "Plus.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Plus.svg"))),
            "Refresh.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Refresh.svg"))),
            "Search.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Search.svg"))),
            "Sidebar.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Sidebar.svg"))),
            "Sliders.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Sliders.svg"))),
            "Stop.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Stop.svg"))),
            "Table.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Table.svg"))),
            "Trash.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Trash.svg"))),
            "Unlock.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Unlock.svg"))),
            "UpDown.svg" => Some(Cow::Borrowed(include_bytes!("../assets/UpDown.svg"))),
            "View.svg" => Some(Cow::Borrowed(include_bytes!("../assets/View.svg"))),
            "Warning.svg" => Some(Cow::Borrowed(include_bytes!("../assets/Warning.svg"))),
            "X.svg" => Some(Cow::Borrowed(include_bytes!("../assets/X.svg"))),
            "download.svg" => Some(Cow::Borrowed(include_bytes!("../assets/download.svg"))),
            "logo.png" => Some(Cow::Borrowed(include_bytes!("../assets/logo.png"))),
            "plug.svg" => Some(Cow::Borrowed(include_bytes!("../assets/plug.svg"))),
            "safe-unlock.svg" => Some(Cow::Borrowed(include_bytes!("../assets/safe-unlock.svg"))),
            "upload.svg" => Some(Cow::Borrowed(include_bytes!("../assets/upload.svg"))),
            "welcome-gradient.png" => Some(Cow::Borrowed(include_bytes!(
                "../assets/welcome-gradient.png"
            ))),
            _ => None,
        })
    }
    fn list(&self, _: &str) -> gpui_kit::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::Assets;
    use gpui_kit::AssetSource;
    #[::core::prelude::v1::test]
    fn group_plug_is_registered_with_the_name_requested_by_the_view() {
        let asset = Assets
            .load("Plug.svg")
            .unwrap()
            .expect("group header plug asset");
        assert_eq!(
            asset.as_ref(),
            Assets.load("plug.svg").unwrap().unwrap().as_ref()
        );
        assert!(std::str::from_utf8(&asset).unwrap().contains("<path"));
    }
}
