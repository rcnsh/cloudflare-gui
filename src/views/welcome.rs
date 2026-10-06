//! The first center tab: shortcuts, so the empty state is useful.

use gpui::prelude::*;
use gpui::{App, FocusHandle, Window, div};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::{ActiveTheme as _, StyledExt as _, h_flex, kbd::Kbd, v_flex};

use crate::panel_basics;

pub struct Welcome {
    focus_handle: FocusHandle,
}

impl Welcome {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

panel_basics!(Welcome);

impl BasePanel for Welcome {
    fn panel_name(&self) -> &'static str {
        "Welcome"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for Welcome {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Start"
    }
}

const SHORTCUTS: &[(&str, &str)] = &[
    (
        "cmd-k",
        "Jump to any database, table, namespace, bucket or Worker",
    ),
    ("enter", "Open the resource selected in the sidebar"),
    ("cmd-t", "New SQL tab for the current database"),
    ("cmd-enter", "Run the query in a SQL tab"),
    ("cmd-shift-w", "Toggle write mode for a SQL tab"),
    ("cmd-r", "Refresh resource lists"),
    ("cmd-b", "Show or hide the sidebar"),
    ("cmd-w", "Close the current tab"),
];

impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .gap_4()
                    .max_w_128()
                    .child(div().text_xl().font_semibold().child("Read-only by default"))
                    .child(div().text_color(muted).child(
                        "Browsing never changes anything. To run a statement that writes, turn on write mode in \
                         that SQL tab, then confirm the exact statement and database.",
                    ))
                    .child(v_flex().gap_2().children(SHORTCUTS.iter().map(|(keys, what)| {
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w_32()
                                    .flex()
                                    .justify_end()
                                    .when_some(gpui::Keystroke::parse(keys).ok(), |this, k| this.child(Kbd::new(k))),
                            )
                            .child(div().text_sm().child(*what))
                    }))),
            )
    }
}
