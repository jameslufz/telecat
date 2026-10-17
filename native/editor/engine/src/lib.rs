//! Isolated gpui-kit 0.7.3 input engine. See ../UPSTREAM.txt.
pub use gpui_kit::base::actions;
pub use gpui_kit::base::{
    AutoScroll, GlobalState, ObservedElement, RoleOverride, Scrollbar, SemanticThemeTokens,
    StepAction, StyledExt, TestSupportExt, Theme,
};
pub use state_style::StateStyle;
mod number_input {
    pub use gpui_kit::base::{NumberInputEvent, NumberStep};
}
mod global_state {
    pub use gpui_kit::base::GlobalState;
}
pub mod input;
mod state_style;
#[allow(dead_code)] // Kept verbatim with the upstream input support code.
mod text_boundary;
pub mod touch_selection;
pub fn init(cx: &mut gpui::App) {
    input::init(cx);
}
