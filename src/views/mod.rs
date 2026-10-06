pub mod app;
pub mod d1_table;
pub mod kv_browser;
pub mod placeholder;
pub mod results_table;
pub mod setup;
pub mod sidebar;
pub mod sql_editor;
pub mod value_viewer;
pub mod welcome;
pub mod workspace;

/// The boilerplate every dock panel needs: a focus handle getter and the
/// `PanelEvent` emitter. Behaviour and titles stay in each view.
#[macro_export]
macro_rules! panel_basics {
    ($ty:ty) => {
        impl gpui::EventEmitter<gpui_component::dock::PanelEvent> for $ty {}

        impl gpui::Focusable for $ty {
            fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
                self.focus_handle.clone()
            }
        }
    };
}
