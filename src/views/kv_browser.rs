//! Browse a KV namespace: prefix search over a cursor-paged key list, and a
//! value viewer with metadata and expiration.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, Entity, FocusHandle, SharedString, Subscription, Task, Window, div, px};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{BROWSER_CONTEXT, FocusSearch};
use crate::cloudflare::Client;
use crate::cloudflare::kv::Key;
use crate::runtime;
use crate::state::Session;
use crate::views::results_table::{fmt_count, fmt_ms};
use crate::views::value_viewer::{Kind, ValueViewer};

/// The key list, paged with Cloudflare's cursor as the user scrolls.
pub struct KeyTable {
    client: Client,
    account_id: String,
    namespace_id: String,
    prefix: String,
    keys: Vec<Key>,
    cursor: Option<String>,
    fetching: bool,
    first_load: bool,
    error: Option<SharedString>,
    /// Bumped on every new search so late pages from an old prefix are dropped.
    generation: u64,
    _fetch: Option<Task<()>>,
}

impl KeyTable {
    fn fetch(&mut self, cx: &mut Context<TableState<Self>>) {
        self.fetching = true;
        let generation = self.generation;
        let (client, account, ns, prefix, cursor) = (
            self.client.clone(),
            self.account_id.clone(),
            self.namespace_id.clone(),
            self.prefix.clone(),
            self.cursor.clone(),
        );
        let request = runtime::run(async move {
            client
                .list_kv_keys(&account, &ns, Some(&prefix), cursor.as_deref())
                .await
        });
        self._fetch = Some(cx.spawn(async move |table, cx| {
            let result = request.await;
            let _ = table.update(cx, |table, cx| {
                let d = table.delegate_mut();
                if d.generation != generation {
                    return;
                }
                d.fetching = false;
                d.first_load = false;
                match result {
                    Ok(Ok(page)) => {
                        d.keys.extend(page.items);
                        d.cursor = page.cursor;
                        d.error = None;
                    }
                    Ok(Err(e)) => d.error = Some(e.to_string().into()),
                    Err(e) => d.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    fn restart(&mut self, prefix: String, cx: &mut Context<TableState<Self>>) {
        self.generation += 1;
        self.prefix = prefix;
        self.keys.clear();
        self.cursor = None;
        self.first_load = true;
        self.error = None;
        self.fetch(cx);
    }
}

impl TableDelegate for KeyTable {
    fn columns_count(&self, _: &App) -> usize {
        3
    }

    fn rows_count(&self, _: &App) -> usize {
        self.keys.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        match ix {
            0 => Column::new("key", "Key").width(px(320.)),
            1 => Column::new("expires", "Expires").width(px(150.)),
            _ => Column::new("meta", "Metadata").width(px(220.)),
        }
        .movable(false)
    }

    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let key = &self.keys[row];
        let text: String = match col {
            0 => key.name.clone(),
            1 => key.expiration.map(relative_expiry).unwrap_or_default(),
            _ => key
                .metadata
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default(),
        };
        div()
            .truncate()
            .text_sm()
            .when(col == 0, |d| {
                d.font_family(cx.theme().mono_font_family.clone())
            })
            .when(col > 0, |d| d.text_color(cx.theme().muted_foreground))
            .child(text)
    }

    fn loading(&self, _: &App) -> bool {
        self.first_load && self.fetching
    }

    fn has_more(&self, _: &App) -> bool {
        self.cursor.is_some() && !self.fetching && self.error.is_none()
    }

    fn load_more_threshold(&self) -> usize {
        100
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.fetch(cx);
    }

    fn cell_text(&self, row: usize, col: usize, _: &App) -> String {
        match col {
            0 => self.keys[row].name.clone(),
            _ => String::new(),
        }
    }
}

enum ValueStatus {
    None,
    Loading(String),
    Loaded {
        key: String,
        size: usize,
        metadata: Option<serde_json::Value>,
        expiration: Option<i64>,
        elapsed_ms: f64,
    },
    Failed(String, SharedString),
}

pub struct KvBrowser {
    title: String,
    namespace_id: String,
    session: Entity<Session>,
    search: Entity<InputState>,
    keys: Entity<TableState<KeyTable>>,
    viewer: Entity<ValueViewer>,
    value: ValueStatus,
    focus_handle: FocusHandle,
    _search: Option<Task<()>>,
    _value: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl KvBrowser {
    pub fn new(
        session: Entity<Session>,
        namespace_id: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (client, account_id) = {
            let s = session.read(cx);
            (s.client.clone(), s.account_id())
        };
        let delegate = KeyTable {
            client,
            account_id,
            namespace_id: namespace_id.clone(),
            prefix: String::new(),
            keys: Vec::new(),
            cursor: None,
            fetching: false,
            first_load: true,
            error: None,
            generation: 0,
            _fetch: None,
        };
        let keys = cx.new(|cx| {
            let mut state = TableState::new(delegate, window, cx)
                .col_movable(false)
                .sortable(false)
                .col_selectable(false);
            state.delegate_mut().fetch(cx);
            state
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter by key prefix…")
                .clean_on_escape()
        });
        let viewer = cx.new(|cx| ValueViewer::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| match event {
                InputEvent::Change => {
                    let prefix = state.read(cx).value().to_string();
                    this.search_later(prefix, Duration::from_millis(350), cx);
                }
                InputEvent::PressEnter { .. } => {
                    let prefix = state.read(cx).value().to_string();
                    this.search_later(prefix, Duration::ZERO, cx);
                }
                _ => {}
            }),
            cx.subscribe_in(&keys, window, |this, _, event: &TableEvent, window, cx| {
                if let TableEvent::SelectRow(ix) = event {
                    this.select_key(*ix, window, cx);
                }
            }),
        ];
        Self {
            title,
            namespace_id,
            session,
            search,
            keys,
            viewer,
            value: ValueStatus::None,
            focus_handle: cx.focus_handle(),
            _search: None,
            _value: None,
            _subscriptions: subscriptions,
        }
    }

    /// Waits for typing to pause before listing, so each keystroke isn't a request.
    fn search_later(&mut self, prefix: String, delay: Duration, cx: &mut Context<Self>) {
        let keys = self.keys.clone();
        self._search = Some(cx.spawn(async move |_, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            keys.update(cx, |table, cx| {
                if table.delegate().prefix != prefix || table.delegate().error.is_some() {
                    table.delegate_mut().restart(prefix, cx);
                    cx.notify();
                }
            });
        }));
    }

    fn select_key(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.keys.read(cx).delegate().keys.get(ix).cloned() else {
            return;
        };
        self.value = ValueStatus::Loading(key.name.clone());
        let (client, account) = {
            let s = self.session.read(cx);
            (s.client.clone(), s.account_id())
        };
        let ns = self.namespace_id.clone();
        // Dropping the previous task cancels its request, and the short delay
        // means arrowing through the list doesn't fetch every key it passes.
        self._value = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let started = std::time::Instant::now();
            let name = key.name.clone();
            let result = runtime::run(async move {
                client
                    .get_kv_value(&account, &ns, &name, key.expiration)
                    .await
            })
            .await;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(value)) => {
                        this.viewer
                            .update(cx, |v, cx| v.show(&value.bytes, None, window, cx));
                        this.value = ValueStatus::Loaded {
                            key: key.name.clone(),
                            size: value.bytes.len(),
                            metadata: value.metadata,
                            expiration: value.expiration,
                            elapsed_ms,
                        };
                    }
                    Ok(Err(e)) => {
                        this.viewer.update(cx, |v, cx| v.clear(cx));
                        this.value = ValueStatus::Failed(key.name.clone(), e.to_string().into());
                    }
                    Err(e) => {
                        this.value = ValueStatus::Failed(key.name.clone(), e.to_string().into())
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn render_value_header(&self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let kind = self.viewer.read(cx).kind;
        let truncated = self.viewer.read(cx).truncated;
        let body = match &self.value {
            ValueStatus::None => div()
                .text_color(muted)
                .child("Select a key to see its value")
                .into_any_element(),
            ValueStatus::Loading(key) => div()
                .text_color(muted)
                .child(format!("Loading {key}…"))
                .into_any_element(),
            ValueStatus::Failed(key, e) => v_flex()
                .gap_1()
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(key.clone()),
                )
                .child(div().text_color(cx.theme().danger).child(e.clone()))
                .into_any_element(),
            ValueStatus::Loaded {
                key,
                size,
                metadata,
                expiration,
                elapsed_ms,
            } => v_flex()
                .gap_1()
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .font_semibold()
                        .child(key.clone()),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .text_color(muted)
                        .child(format!("{} bytes", fmt_count(*size as u64)))
                        .child(match kind {
                            Kind::Json => "JSON",
                            Kind::Text => "Text",
                            Kind::Image => "Image",
                            Kind::Binary => "Binary",
                            Kind::Empty => "Empty",
                        })
                        .child(match expiration {
                            Some(at) => format!("Expires {}", absolute_expiry(*at)),
                            None => "No expiration".to_string(),
                        })
                        .child(fmt_ms(*elapsed_ms))
                        .when(truncated, |this| this.child("Truncated for display")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().text_color(muted).child("Metadata"))
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .truncate()
                                .child(
                                    metadata
                                        .as_ref()
                                        .map(|m| m.to_string())
                                        .unwrap_or_else(|| "none".into()),
                                ),
                        ),
                )
                .into_any_element(),
        };
        div()
            .p_2()
            .text_sm()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(body)
    }
}

fn relative_expiry(at: i64) -> String {
    let secs = at - chrono::Utc::now().timestamp();
    match secs {
        s if s <= 0 => "expired".into(),
        s if s < 3600 => format!("in {}m", s / 60),
        s if s < 86_400 => format!("in {}h", s / 3600),
        s => format!("in {}d", s / 86_400),
    }
}

fn absolute_expiry(at: i64) -> String {
    let when = chrono::DateTime::from_timestamp(at, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| at.to_string());
    format!("{} ({when})", relative_expiry(at))
}

impl gpui::EventEmitter<gpui_component::dock::PanelEvent> for KvBrowser {}

impl gpui::Focusable for KvBrowser {
    // Arrow keys should move through keys as soon as the tab opens; cmd-F
    // jumps to the prefix search.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.keys.read(cx).focus_handle(cx)
    }
}

impl BasePanel for KvBrowser {
    fn panel_name(&self) -> &'static str {
        "KvBrowser"
    }
}

impl Panel for KvBrowser {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(Lucide::KeyRound).small())
            .child(self.title.clone())
    }
}

impl Render for KvBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let table = self.keys.read(cx).delegate();
        let loaded = table.keys.len();
        let more = table.cursor.is_some();
        let fetching = table.fetching;
        let error = table.error.clone();
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .key_context(BROWSER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_search))
            .child(
                h_flex()
                    .p_2()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().w(px(360.)).child(
                            Input::new(&self.search)
                                .small()
                                .prefix(Icon::new(Lucide::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .child(div().text_sm().text_color(muted).child(format!(
                        "{} key{}{}{}",
                        fmt_count(loaded as u64),
                        if loaded == 1 { "" } else { "s" },
                        if more {
                            " loaded, more as you scroll"
                        } else {
                            ""
                        },
                        if fetching { " · loading…" } else { "" },
                    )))
                    .when_some(error, |this, e| {
                        this.child(div().text_sm().text_color(cx.theme().danger).child(e))
                    }),
            )
            .child(
                h_resizable("kv-split")
                    .child(
                        resizable_panel()
                            .size(px(520.))
                            .child(DataTable::new(&self.keys).small().stripe(true)),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .child(self.render_value_header(cx))
                                .child(div().flex_1().min_h_0().child(self.viewer.clone())),
                        ),
                    ),
            )
    }
}
