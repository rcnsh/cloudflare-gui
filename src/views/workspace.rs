//! The signed-in layout: resource sidebar on the left, resource tabs in the center.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyWeakEntity, App, Entity, FocusHandle, Focusable, Subscription, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::dock::{
    DockArea, DockLayout, DockPlacement, DockSkin, Panel, PanelId, panel_handle,
};

use crate::actions::{NewSqlTab, OpenResource, RefreshResources, ToggleSidebar, WORKSPACE_CONTEXT};
use crate::state::{Resource, Session};
use crate::views::d1_table::TableBrowser;
use crate::views::kv_browser::KvBrowser;
use crate::views::placeholder::Placeholder;
use crate::views::sidebar::Sidebar;
use crate::views::sql_editor::SqlEditor;
use crate::views::welcome::Welcome;
use crate::views::worker_tail::WorkerTail;

struct OpenTab {
    resource: Resource,
    panel: AnyWeakEntity,
}

pub struct Workspace {
    pub session: Entity<Session>,
    dock: Entity<DockArea>,
    sidebar: Entity<Sidebar>,
    tabs: Vec<OpenTab>,
    last_database: Option<(String, String)>,
    focus_handle: FocusHandle,
    _skin: Rc<DockSkin>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock, skin) = DockSkin::dock_area("workspace", Some(1), window, cx);
        skin.set_close_button_visible(true, cx);

        let sidebar = cx.new(|cx| Sidebar::new(session.clone(), window, cx));
        let welcome = cx.new(Welcome::new);
        dock.update(cx, |area, cx| {
            area.set_center(
                DockLayout::tabs().panel_view(panel_handle(welcome), cx),
                window,
                cx,
            );
            area.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(sidebar.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Left, px(280.), window, cx);
            area.set_dock_collapsible(DockPlacement::Left, true, window, cx);
        });

        Self {
            session,
            dock,
            sidebar,
            tabs: Vec::new(),
            last_database: None,
            focus_handle: cx.focus_handle(),
            _skin: skin,
            _subscriptions: Vec::new(),
        }
    }

    fn open_resource(
        &mut self,
        action: &OpenResource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(action.0.clone(), window, cx);
    }

    /// Focuses the existing tab for `resource`, or opens a new one.
    pub fn open(&mut self, resource: Resource, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.retain(|tab| tab.panel.upgrade().is_some());
        if let Some(tab) = self.tabs.iter().find(|tab| tab.resource == resource) {
            let id = PanelId::from(tab.panel.entity_id());
            self.dock
                .update(cx, |area, cx| area.select_panel(id, window, cx));
            return;
        }
        let session = self.session.clone();
        match resource.clone() {
            Resource::D1Database { id, name } => {
                self.last_database = Some((id.clone(), name.clone()));
                let panel = cx.new(|cx| SqlEditor::new(session, id, name, None, window, cx));
                self.add_tab(resource, panel, window, cx);
            }
            Resource::D1Table {
                database_id,
                database_name,
                table,
            } => {
                self.last_database = Some((database_id.clone(), database_name.clone()));
                let panel = cx.new(|cx| {
                    TableBrowser::new(session, database_id, database_name, table, window, cx)
                });
                self.add_tab(resource, panel, window, cx);
            }
            Resource::KvNamespace { id, title } => {
                let panel = cx.new(|cx| KvBrowser::new(session, id, title, window, cx));
                self.add_tab(resource, panel, window, cx);
            }
            Resource::Worker { name } => {
                let panel = cx.new(|cx| WorkerTail::new(session, name, window, cx));
                self.add_tab(resource, panel, window, cx);
            }
            _ => {
                let panel = cx.new(|cx| Placeholder::new(resource.clone(), cx));
                self.add_tab(resource, panel, window, cx);
            }
        }
    }

    /// A second SQL tab for the database used most recently. Not tracked as the
    /// database's tab, so opening the database again focuses its first tab.
    fn new_sql_tab(&mut self, _: &NewSqlTab, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.last_database.clone().or_else(|| {
            self.session
                .read(cx)
                .d1
                .value()
                .and_then(|dbs| dbs.first())
                .map(|db| (db.uuid.clone(), db.name.clone()))
        });
        let Some((id, name)) = target else {
            return;
        };
        let session = self.session.clone();
        let panel = cx.new(|cx| SqlEditor::new(session, id, name, None, window, cx));
        let focus = panel.focus_handle(cx);
        self.dock.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
        // The panel's elements don't exist until the dock renders it, and a
        // focus handle that isn't in the tree can't take focus.
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    fn add_tab<P: Panel>(
        &mut self,
        resource: Resource,
        panel: Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tabs.push(OpenTab {
            resource,
            panel: panel.downgrade().into(),
        });
        let focus = panel.focus_handle(cx);
        self.dock.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
        // The panel's elements don't exist until the dock renders it, and a
        // focus handle that isn't in the tree can't take focus.
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    fn refresh(&mut self, _: &RefreshResources, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| s.load_resources(true, cx));
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |area, cx| {
            area.toggle_dock(DockPlacement::Left, window, cx)
        });
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Land in the resource tree so arrow keys work straight away.
        self.sidebar.read(cx).focus_handle(cx)
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_resource))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::new_sql_tab))
            .bg(cx.theme().background)
            .child(self.dock.clone())
    }
}
