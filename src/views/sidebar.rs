//! Resource tree: D1 databases (with tables loaded on expand), KV namespaces,
//! R2 buckets and Workers.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, SharedString, Subscription,
    Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel, PanelEvent};
use gpui_component::list::ListItem;
use gpui_component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{OpenResource, RefreshResources};
use crate::state::{Loadable, Resource, Session, SessionEvent};

actions!(sidebar, [OpenSelected]);

const CONTEXT: &str = "ResourceSidebar";

pub fn bind_keys(cx: &mut App) {
    // Bound after gpui-component's own keys, so inside the sidebar Enter opens
    // the selected resource instead of only toggling folders.
    cx.bind_keys([KeyBinding::new(
        "enter",
        OpenSelected,
        Some("ResourceSidebar > Tree"),
    )]);
}

const D1: &str = "group:d1";
const KV: &str = "group:kv";
const R2: &str = "group:r2";
const WORKERS: &str = "group:workers";

#[derive(Clone)]
struct Node {
    resource: Option<Resource>,
    icon: Lucide,
    /// Greyed-out status rows such as "Loading…" or an error.
    status: bool,
}

pub struct Sidebar {
    session: Entity<Session>,
    tree: Entity<TreeState>,
    nodes: Rc<HashMap<SharedString, Node>>,
    expanded: HashSet<SharedString>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Sidebar {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            // The tree owns the keyboard handling, so whenever the panel itself
            // is focused (startup, clicking the dock) hand focus to the tree.
            cx.on_focus(&focus_handle, window, |this, window, cx| {
                this.focus_tree(window, cx);
                if this.tree.read(cx).selected_index().is_none() {
                    this.tree
                        .update(cx, |t, cx| t.set_selected_index(Some(0), cx));
                }
            }),
            cx.subscribe(&session, |this, _, _: &SessionEvent, cx| this.rebuild(cx)),
            cx.subscribe_in(
                &tree,
                window,
                |this, _, event: &TreeEvent, _, cx| match event {
                    TreeEvent::Expanded(id) => {
                        this.expanded.insert(id.clone());
                        if let Some(Resource::D1Database { id: db, .. }) =
                            this.nodes.get(id).and_then(|n| n.resource.clone())
                        {
                            this.session
                                .update(cx, |s, cx| s.load_tables(&db, false, cx));
                        }
                    }
                    TreeEvent::Collapsed(id) => {
                        this.expanded.remove(id);
                    }
                },
            ),
        ];
        let mut this = Self {
            session,
            tree,
            nodes: Rc::new(HashMap::new()),
            expanded: [D1, KV, R2, WORKERS]
                .into_iter()
                .map(SharedString::from)
                .collect(),
            focus_handle,
            _subscriptions: subscriptions,
        };
        this.rebuild(cx);
        this.session.update(cx, |s, cx| s.load_resources(false, cx));
        this
    }

    /// Rebuilds the tree from the session's cached lists, keeping expansion
    /// and selection, since tree items carry their own expansion state.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let mut nodes = HashMap::new();
        let expanded = &self.expanded;
        let mut node = |id: SharedString,
                        label: String,
                        icon: Lucide,
                        resource: Option<Resource>,
                        status: bool| {
            nodes.insert(
                id.clone(),
                Node {
                    resource,
                    icon,
                    status,
                },
            );
            TreeItem::new(id.clone(), label).expanded(expanded.contains(&id))
        };

        let d1_children = list_children(&session.d1, &mut node, D1, |db, node| {
            let id: SharedString = format!("d1:{}", db.uuid).into();
            let resource = Resource::D1Database {
                id: db.uuid.clone(),
                name: db.name.clone(),
            };
            let tables = match session.tables.get(&db.uuid) {
                Some(Loadable::Loaded { value, .. }) if value.is_empty() => {
                    vec![node(
                        format!("{id}:empty").into(),
                        "No tables".into(),
                        Lucide::Info,
                        None,
                        true,
                    )]
                }
                Some(Loadable::Loaded { value, .. }) => value
                    .iter()
                    .map(|t| {
                        node(
                            format!("{id}:t:{}", t.name).into(),
                            t.name.clone(),
                            if t.kind == "view" {
                                Lucide::Eye
                            } else {
                                Lucide::Table
                            },
                            Some(Resource::D1Table {
                                database_id: db.uuid.clone(),
                                database_name: db.name.clone(),
                                table: t.name.clone(),
                            }),
                            false,
                        )
                    })
                    .collect(),
                Some(Loadable::Failed(e)) => {
                    vec![node(
                        format!("{id}:error").into(),
                        e.clone(),
                        Lucide::CircleAlert,
                        None,
                        true,
                    )]
                }
                // Not loaded yet: a placeholder child makes the row expandable.
                _ => vec![node(
                    format!("{id}:loading").into(),
                    "Loading…".into(),
                    Lucide::LoaderCircle,
                    None,
                    true,
                )],
            };
            node(id, db.name.clone(), Lucide::Database, Some(resource), false).children(tables)
        });
        let kv_children = list_children(&session.kv, &mut node, KV, |ns, node| {
            node(
                format!("kv:{}", ns.id).into(),
                ns.title.clone(),
                Lucide::KeyRound,
                Some(Resource::KvNamespace {
                    id: ns.id.clone(),
                    title: ns.title.clone(),
                }),
                false,
            )
        });
        let r2_children = list_children(&session.r2, &mut node, R2, |bucket, node| {
            node(
                format!("r2:{}", bucket.name).into(),
                bucket.name.clone(),
                Lucide::Archive,
                Some(Resource::R2Bucket(bucket.clone())),
                false,
            )
        });
        let worker_children =
            list_children(&session.workers, &mut node, WORKERS, |script, node| {
                node(
                    format!("worker:{}", script.id).into(),
                    script.id.clone(),
                    Lucide::ScrollText,
                    Some(Resource::Worker {
                        name: script.id.clone(),
                    }),
                    false,
                )
            });

        let roots = vec![
            node(
                D1.into(),
                "D1 Databases".into(),
                Lucide::Database,
                None,
                false,
            )
            .children(d1_children),
            node(
                KV.into(),
                "KV Namespaces".into(),
                Lucide::KeyRound,
                None,
                false,
            )
            .children(kv_children),
            node(R2.into(), "R2 Buckets".into(), Lucide::Archive, None, false)
                .children(r2_children),
            node(WORKERS.into(), "Workers".into(), Lucide::Cog, None, false)
                .children(worker_children),
        ];
        self.nodes = Rc::new(nodes);
        self.tree.update(cx, |tree, cx| {
            let selected = tree.selected_item().map(|i| i.id.clone());
            tree.set_items(roots, cx);
            if let Some(ix) = selected.and_then(|id| tree.index_of(&id)) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
        cx.notify();
    }

    fn open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        let resource = self
            .tree
            .read(cx)
            .selected_item()
            .and_then(|item| self.nodes.get(&item.id))
            .and_then(|node| node.resource.clone());
        if let Some(resource) = resource {
            window.dispatch_action(Box::new(OpenResource(resource)), cx);
        }
    }

    pub fn focus_tree(&self, window: &mut Window, cx: &mut App) {
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
    }
}

/// Children for a group row: the items, or a single status row while loading
/// or after an error.
fn list_children<T>(
    list: &Loadable<Vec<T>>,
    node: &mut impl FnMut(SharedString, String, Lucide, Option<Resource>, bool) -> TreeItem,
    group: &str,
    mut build: impl FnMut(
        &T,
        &mut dyn FnMut(SharedString, String, Lucide, Option<Resource>, bool) -> TreeItem,
    ) -> TreeItem,
) -> Vec<TreeItem> {
    match list {
        Loadable::Loaded { value, .. } if value.is_empty() => {
            vec![node(
                format!("{group}:empty").into(),
                "None".into(),
                Lucide::Info,
                None,
                true,
            )]
        }
        Loadable::Loaded { value, .. } => value.iter().map(|item| build(item, node)).collect(),
        Loadable::Failed(e) => vec![node(
            format!("{group}:error").into(),
            e.clone(),
            Lucide::CircleAlert,
            None,
            true,
        )],
        Loadable::Loading | Loadable::NotLoaded => {
            vec![node(
                format!("{group}:loading").into(),
                "Loading…".into(),
                Lucide::LoaderCircle,
                None,
                true,
            )]
        }
    }
}

impl EventEmitter<PanelEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for Sidebar {
    fn panel_name(&self) -> &'static str {
        "ResourceSidebar"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for Sidebar {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Resources"
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nodes = self.nodes.clone();
        let muted = cx.theme().muted_foreground;
        let account = self.session.read(cx).account.name.clone();
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_selected))
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().text_sm().font_semibold().truncate().child(account))
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::RefreshCw))
                            .tooltip("Refresh resource lists (⌘R)")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(RefreshResources), cx)
                            }),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    tree(&self.tree, move |ix, entry, _selected, _window, _cx| {
                        let id = entry.item().id.clone();
                        let node = nodes.get(&id).cloned();
                        let status = node.as_ref().is_some_and(|n| n.status);
                        let icon = node.as_ref().map(|n| n.icon).unwrap_or(Lucide::Dot);
                        let resource = node.and_then(|n| n.resource);
                        let chevron = if entry.is_folder() {
                            if entry.is_expanded() {
                                Lucide::ChevronDown
                            } else {
                                Lucide::ChevronRight
                            }
                        } else {
                            Lucide::Dot
                        };
                        ListItem::new(("node", ix))
                            .pl(gpui::px(6. + 14. * entry.depth() as f32))
                            .py_0p5()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .text_sm()
                                    .when(status, |this| this.text_color(muted))
                                    .child(div().w_3().when(entry.is_folder(), |this| {
                                        this.child(Icon::new(chevron).xsmall().text_color(muted))
                                    }))
                                    .child(
                                        Icon::new(icon)
                                            .small()
                                            .when(!entry.is_root(), |i| i.text_color(muted)),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .when(entry.is_root(), |this| this.font_semibold())
                                            .child(entry.item().label.clone()),
                                    ),
                            )
                            .when_some(resource, |item, resource| {
                                item.on_click(move |event, window, cx| {
                                    // Single click selects; double click (or Enter) opens.
                                    if event.click_count() >= 2 {
                                        window.dispatch_action(
                                            Box::new(OpenResource(resource.clone())),
                                            cx,
                                        );
                                    }
                                })
                            })
                    })
                    .size_full(),
                ),
            )
    }
}
