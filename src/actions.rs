//! Every user-facing command is a GPUI action, so it can be bound to a key,
//! shown in the menu bar and reached from the command palette.

use gpui::{App, KeyBinding, Menu, MenuItem, actions};
use gpui_component::dock::ClosePanel;

use crate::state::Resource;

actions!(
    cloudflare_gui,
    [
        Quit,
        CommandPalette,
        RefreshResources,
        ToggleSidebar,
        NewSqlTab,
        RunQuery,
        ToggleWriteMode,
        NextPage,
        PreviousPage,
        LoadMore,
        FocusSearch,
        ToggleTailPause,
        ClearLog,
        SwitchAccount,
        SignOut,
        ToggleDarkMode,
    ]
);

/// Opens a resource in a tab (or focuses the existing tab for it).
#[derive(Clone, PartialEq, serde::Deserialize, gpui::Action)]
#[action(namespace = cloudflare_gui, no_json)]
pub struct OpenResource(pub Resource);

impl std::fmt::Debug for OpenResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpenResource({})", self.0.label())
    }
}

pub const WORKSPACE_CONTEXT: &str = "Workspace";
pub const SQL_EDITOR_CONTEXT: &str = "SqlEditor";
pub const BROWSER_CONTEXT: &str = "ResourceBrowser";
pub const TAIL_CONTEXT: &str = "WorkerTail";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-k", CommandPalette, None),
        KeyBinding::new("cmd-p", CommandPalette, None),
        KeyBinding::new("cmd-r", RefreshResources, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(WORKSPACE_CONTEXT)),
        // The dock owns tab closing, including its own close-refusal rules.
        KeyBinding::new("cmd-w", ClosePanel, None),
        KeyBinding::new("cmd-t", NewSqlTab, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("cmd-shift-d", ToggleDarkMode, None),
        KeyBinding::new("cmd-enter", RunQuery, Some(SQL_EDITOR_CONTEXT)),
        // The editor binds cmd-enter itself (newline + event); this deeper binding,
        // registered after gpui-component's, wins so cmd-enter runs the query.
        KeyBinding::new("cmd-enter", RunQuery, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-shift-w", ToggleWriteMode, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-shift-w", ToggleWriteMode, Some(SQL_EDITOR_CONTEXT)),
        KeyBinding::new("cmd-]", NextPage, Some(BROWSER_CONTEXT)),
        KeyBinding::new("cmd-[", PreviousPage, Some(BROWSER_CONTEXT)),
        KeyBinding::new("cmd-shift-l", LoadMore, Some(BROWSER_CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(BROWSER_CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(TAIL_CONTEXT)),
        KeyBinding::new("cmd-.", ToggleTailPause, Some(TAIL_CONTEXT)),
        KeyBinding::new("cmd-shift-k", ClearLog, Some(TAIL_CONTEXT)),
    ]);
}

pub fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("Cloudflare GUI").items([
            MenuItem::action("Switch Account…", SwitchAccount),
            MenuItem::action("Sign Out", SignOut),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New SQL Tab", NewSqlTab),
            MenuItem::action("Close Tab", ClosePanel),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette", CommandPalette),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Refresh Resources", RefreshResources),
            MenuItem::separator(),
            MenuItem::action("Toggle Dark Mode", ToggleDarkMode),
        ]),
        Menu::new("Query").items([
            MenuItem::action("Run", RunQuery),
            MenuItem::action("Toggle Write Mode", ToggleWriteMode),
            MenuItem::separator(),
            MenuItem::action("Next Page", NextPage),
            MenuItem::action("Previous Page", PreviousPage),
            MenuItem::action("Load More", LoadMore),
        ]),
        Menu::new("Logs").items([
            MenuItem::action("Pause / Resume", ToggleTailPause),
            MenuItem::action("Clear", ClearLog),
        ]),
    ]);
}
