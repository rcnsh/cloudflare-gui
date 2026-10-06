//! Stand-in tab for resource types whose view hasn't been built yet.

use gpui::prelude::*;
use gpui::{FocusHandle, Window, div};
use gpui_component::ActiveTheme as _;
use gpui_component::dock::{BasePanel, Panel};

use crate::panel_basics;
use crate::state::Resource;

pub struct Placeholder {
    resource: Resource,
    focus_handle: FocusHandle,
}

impl Placeholder {
    pub fn new(resource: Resource, cx: &mut Context<Self>) -> Self {
        Self {
            resource,
            focus_handle: cx.focus_handle(),
        }
    }
}

panel_basics!(Placeholder);

impl BasePanel for Placeholder {
    fn panel_name(&self) -> &'static str {
        "Placeholder"
    }
}

impl Panel for Placeholder {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.resource.label()
    }
}

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{} view isn't available yet", self.resource.kind()))
    }
}
