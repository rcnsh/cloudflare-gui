//! Browse an R2 bucket like a file system: folders come from the `/`
//! delimiter, objects show size and modified time, and selecting one previews
//! text, JSON or images.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Entity, FocusHandle, KeyBinding, SharedString, Subscription, Task, Window, actions, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{BROWSER_CONTEXT, FocusSearch};
use crate::cloudflare::Client;
use crate::cloudflare::r2::{Bucket, Object};
use crate::runtime;
use crate::state::Session;
use crate::views::results_table::{fmt_count, fmt_ms};
use crate::views::value_viewer::{Kind, ValueViewer};

actions!(r2_browser, [OpenEntry, ParentFolder]);

const LIST_CONTEXT: &str = "ObjectList";

/// Previews read at most this much of an object.
const PREVIEW_LIMIT: usize = 8 * 1024 * 1024;

pub fn bind_keys(cx: &mut App) {
    // Scoped to the table so Backspace still deletes text in the filter box.
    let ctx = Some("ObjectList > DataTable");
    cx.bind_keys([
        KeyBinding::new("enter", OpenEntry, ctx),
        KeyBinding::new("backspace", ParentFolder, ctx),
        KeyBinding::new("cmd-up", ParentFolder, None),
    ]);
}

#[derive(Debug, Clone)]
enum Entry {
    /// The full prefix, ending in `/`.
    Folder(String),
    Object(Object),
}

/// One folder level, paged with Cloudflare's cursor as the user scrolls.
pub struct ObjectTable {
    client: Client,
    account_id: String,
    bucket: Bucket,
    /// The folder being shown, ending in `/` (or empty for the root).
    folder: String,
    /// Typed filter, appended to `folder` to form the listing prefix.
    filter: String,
    entries: Vec<Entry>,
    cursor: Option<String>,
    fetching: bool,
    first_load: bool,
    error: Option<SharedString>,
    /// Bumped on every new listing so late pages from an old one are dropped.
    generation: u64,
    _fetch: Option<Task<()>>,
}

impl ObjectTable {
    fn fetch(&mut self, cx: &mut Context<TableState<Self>>) {
        self.fetching = true;
        let generation = self.generation;
        let (client, account, bucket, prefix, cursor) = (
            self.client.clone(),
            self.account_id.clone(),
            self.bucket.clone(),
            format!("{}{}", self.folder, self.filter),
            self.cursor.clone(),
        );
        let request = runtime::run(async move {
            client
                .list_r2_objects(&account, &bucket, &prefix, cursor.as_deref())
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
                    Ok(Ok(listing)) => {
                        d.entries
                            .extend(listing.prefixes.into_iter().map(Entry::Folder));
                        d.entries
                            .extend(listing.page.items.into_iter().map(Entry::Object));
                        d.cursor = listing.page.cursor;
                        d.error = None;
                    }
                    Ok(Err(e)) => d.error = Some(e.to_string().into()),
                    Err(e) => d.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    fn restart(&mut self, cx: &mut Context<TableState<Self>>) {
        self.generation += 1;
        self.entries.clear();
        self.cursor = None;
        self.first_load = true;
        self.error = None;
        self.fetch(cx);
    }

    /// The part of a key or prefix below the current folder.
    fn display_name<'a>(&self, full: &'a str) -> &'a str {
        full.strip_prefix(self.folder.as_str()).unwrap_or(full)
    }
}

impl TableDelegate for ObjectTable {
    fn columns_count(&self, _: &App) -> usize {
        3
    }

    fn rows_count(&self, _: &App) -> usize {
        self.entries.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        match ix {
            0 => Column::new("name", "Name").width(px(300.)),
            1 => Column::new("size", "Size").width(px(90.)).text_right(),
            _ => Column::new("modified", "Modified").width(px(150.)),
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
        let muted = cx.theme().muted_foreground;
        let cell = div().truncate().text_sm();
        match (&self.entries[row], col) {
            (Entry::Folder(prefix), 0) => cell
                .child(
                    h_flex()
                        .gap_1()
                        .child(Icon::new(Lucide::Folder).small().text_color(muted))
                        .child(self.display_name(prefix).to_string()),
                )
                .into_any_element(),
            (Entry::Object(o), 0) => cell
                .font_family(cx.theme().mono_font_family.clone())
                .child(self.display_name(&o.key).to_string())
                .into_any_element(),
            (Entry::Object(o), 1) => cell
                .text_right()
                .text_color(muted)
                .child(fmt_size(o.size))
                .into_any_element(),
            (Entry::Object(o), 2) => cell
                .text_color(muted)
                .child(o.last_modified.as_deref().map(fmt_time).unwrap_or_default())
                .into_any_element(),
            _ => cell.into_any_element(),
        }
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
        match (&self.entries[row], col) {
            (Entry::Folder(p), 0) => p.clone(),
            (Entry::Object(o), 0) => o.key.clone(),
            (Entry::Object(o), 1) => o.size.to_string(),
            _ => String::new(),
        }
    }
}

/// `1536` → `1.5 KB`.
pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

fn fmt_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

/// The folder above `folder`: `a/b/` → `a/`, `a/` → ``.
fn parent(folder: &str) -> String {
    let trimmed = folder.strip_suffix('/').unwrap_or(folder);
    match trimmed.rfind('/') {
        Some(ix) => trimmed[..=ix].to_string(),
        None => String::new(),
    }
}

/// Whether a preview of this object is worth fetching.
fn previewable(object: &Object) -> Result<(), String> {
    let image = object
        .http_metadata
        .content_type
        .as_deref()
        .is_some_and(|t| t.starts_with("image/"));
    // A cut-off image can't be decoded, so don't download part of one.
    if image && object.size as usize > PREVIEW_LIMIT {
        return Err(format!(
            "Image is {}, larger than the {} preview limit",
            fmt_size(object.size),
            fmt_size(PREVIEW_LIMIT as u64)
        ));
    }
    Ok(())
}

enum Preview {
    None,
    Folder(String),
    Loading(Object),
    Loaded {
        object: Object,
        content_type: Option<String>,
        truncated: bool,
        elapsed_ms: f64,
    },
    Skipped(Object, String),
    Failed(Object, SharedString),
}

pub struct R2Browser {
    bucket: Bucket,
    session: Entity<Session>,
    search: Entity<InputState>,
    objects: Entity<TableState<ObjectTable>>,
    viewer: Entity<ValueViewer>,
    preview: Preview,
    focus_handle: FocusHandle,
    _search: Option<Task<()>>,
    _preview: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl R2Browser {
    pub fn new(
        session: Entity<Session>,
        bucket: Bucket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (client, account_id) = {
            let s = session.read(cx);
            (s.client.clone(), s.account_id())
        };
        let delegate = ObjectTable {
            client,
            account_id,
            bucket: bucket.clone(),
            folder: String::new(),
            filter: String::new(),
            entries: Vec::new(),
            cursor: None,
            fetching: false,
            first_load: true,
            error: None,
            generation: 0,
            _fetch: None,
        };
        let objects = cx.new(|cx| {
            let mut state = TableState::new(delegate, window, cx)
                .col_movable(false)
                .sortable(false)
                .col_selectable(false);
            state.delegate_mut().fetch(cx);
            state
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter by name prefix…")
                .clean_on_escape()
        });
        let viewer = cx.new(|cx| ValueViewer::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| match event {
                InputEvent::Change => {
                    let filter = state.read(cx).value().to_string();
                    this.filter_later(filter, Duration::from_millis(350), cx);
                }
                InputEvent::PressEnter { .. } => {
                    let filter = state.read(cx).value().to_string();
                    this.filter_later(filter, Duration::ZERO, cx);
                }
                _ => {}
            }),
            cx.subscribe_in(
                &objects,
                window,
                |this, _, event: &TableEvent, window, cx| match event {
                    TableEvent::SelectRow(ix) => this.select(*ix, window, cx),
                    TableEvent::DoubleClickedRow(ix) => this.open(*ix, window, cx),
                    _ => {}
                },
            ),
        ];
        Self {
            bucket,
            session,
            search,
            objects,
            viewer,
            preview: Preview::None,
            focus_handle: cx.focus_handle(),
            _search: None,
            _preview: None,
            _subscriptions: subscriptions,
        }
    }

    /// Waits for typing to pause before listing, so each keystroke isn't a request.
    fn filter_later(&mut self, filter: String, delay: Duration, cx: &mut Context<Self>) {
        let objects = self.objects.clone();
        self._search = Some(cx.spawn(async move |_, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            objects.update(cx, |table, cx| {
                let d = table.delegate_mut();
                if d.filter != filter || d.error.is_some() {
                    d.filter = filter;
                    d.restart(cx);
                    cx.notify();
                }
            });
        }));
    }

    fn navigate(&mut self, folder: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self._search = None;
        self._preview = None;
        self.preview = Preview::None;
        self.viewer.update(cx, |v, cx| v.clear(cx));
        self.objects.update(cx, |table, cx| {
            table.clear_selection(cx);
            let d = table.delegate_mut();
            d.folder = folder;
            d.filter.clear();
            d.restart(cx);
            cx.notify();
        });
        cx.notify();
    }

    fn open(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.objects.read(cx).delegate().entries.get(ix).cloned();
        match entry {
            Some(Entry::Folder(prefix)) => self.navigate(prefix, window, cx),
            Some(Entry::Object(_)) => self.select(ix, window, cx),
            None => {}
        }
    }

    fn open_selected(&mut self, _: &OpenEntry, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.objects.read(cx).selected_row() {
            self.open(ix, window, cx);
        }
    }

    fn parent_folder(&mut self, _: &ParentFolder, window: &mut Window, cx: &mut Context<Self>) {
        let folder = self.objects.read(cx).delegate().folder.clone();
        if !folder.is_empty() {
            self.navigate(parent(&folder), window, cx);
        }
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.objects.read(cx).delegate().entries.get(ix).cloned() else {
            return;
        };
        let object = match entry {
            Entry::Folder(prefix) => {
                self._preview = None;
                self.viewer.update(cx, |v, cx| v.clear(cx));
                self.preview = Preview::Folder(prefix);
                cx.notify();
                return;
            }
            Entry::Object(object) => object,
        };
        if let Err(reason) = previewable(&object) {
            self._preview = None;
            self.viewer.update(cx, |v, cx| v.clear(cx));
            self.preview = Preview::Skipped(object, reason);
            cx.notify();
            return;
        }
        self.preview = Preview::Loading(object.clone());
        let (client, account) = {
            let s = self.session.read(cx);
            (s.client.clone(), s.account_id())
        };
        let bucket = self.bucket.clone();
        // Dropping the previous task cancels its download, and the short delay
        // means arrowing through the list doesn't fetch every object it passes.
        self._preview = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let started = std::time::Instant::now();
            let key = object.key.clone();
            let result = runtime::run(async move {
                client
                    .get_r2_object(&account, &bucket, &key, PREVIEW_LIMIT)
                    .await
            })
            .await;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(body)) => {
                        // The listing's metadata is what the uploader set; the
                        // response header is the fallback.
                        let content_type = object
                            .http_metadata
                            .content_type
                            .clone()
                            .or(body.content_type);
                        this.viewer.update(cx, |v, cx| {
                            v.show(&body.bytes, content_type.as_deref(), window, cx)
                        });
                        this.preview = Preview::Loaded {
                            object,
                            content_type,
                            truncated: body.truncated,
                            elapsed_ms,
                        };
                    }
                    Ok(Err(e)) => {
                        this.viewer.update(cx, |v, cx| v.clear(cx));
                        this.preview = Preview::Failed(object, e.to_string().into());
                    }
                    Err(e) => {
                        this.viewer.update(cx, |v, cx| v.clear(cx));
                        this.preview = Preview::Failed(object, e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_breadcrumbs(&self, folder: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let mut crumbs = vec![(self.bucket.name.clone(), String::new())];
        let mut acc = String::new();
        for part in folder.split('/').filter(|p| !p.is_empty()) {
            acc.push_str(part);
            acc.push('/');
            crumbs.push((part.to_string(), acc.clone()));
        }
        let last = crumbs.len() - 1;
        h_flex()
            .gap_0p5()
            .min_w_0()
            .children(crumbs.into_iter().enumerate().map(|(ix, (label, target))| {
                h_flex()
                    .when(ix > 0, |d| {
                        d.child(
                            Icon::new(Lucide::ChevronRight)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .child(
                        Button::new(("crumb", ix))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .disabled(ix == last)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate(target.clone(), window, cx)
                            })),
                    )
            }))
    }

    fn render_preview_header(&self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let kind = self.viewer.read(cx).kind;
        let display_truncated = self.viewer.read(cx).truncated;
        let title = |key: &str| {
            div()
                .font_family(mono.clone())
                .font_semibold()
                .truncate()
                .child(key.to_string())
        };
        let body = match &self.preview {
            Preview::None => div()
                .text_color(muted)
                .child("Select an object to preview it")
                .into_any_element(),
            Preview::Folder(prefix) => v_flex()
                .gap_1()
                .child(title(prefix))
                .child(
                    div()
                        .text_color(muted)
                        .child("Folder · press Enter or double-click to open"),
                )
                .into_any_element(),
            Preview::Loading(o) => v_flex()
                .gap_1()
                .child(title(&o.key))
                .child(div().text_color(muted).child("Loading…"))
                .into_any_element(),
            Preview::Skipped(o, reason) => v_flex()
                .gap_1()
                .child(title(&o.key))
                .child(div().text_color(muted).child(reason.clone()))
                .into_any_element(),
            Preview::Failed(o, e) => v_flex()
                .gap_1()
                .child(title(&o.key))
                .child(div().text_color(cx.theme().danger).child(e.clone()))
                .into_any_element(),
            Preview::Loaded {
                object,
                content_type,
                truncated,
                elapsed_ms,
            } => {
                let custom: Vec<String> = {
                    let mut pairs: Vec<_> = object.custom_metadata.iter().collect();
                    pairs.sort();
                    pairs.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
                };
                v_flex()
                    .gap_1()
                    .child(title(&object.key))
                    .child(
                        h_flex()
                            .gap_3()
                            .text_color(muted)
                            .child(format!(
                                "{} ({} bytes)",
                                fmt_size(object.size),
                                fmt_count(object.size)
                            ))
                            .child(content_type.clone().unwrap_or_else(|| {
                                match kind {
                                    Kind::Json => "JSON",
                                    Kind::Text => "Text",
                                    Kind::Image => "Image",
                                    Kind::Binary => "Binary",
                                    Kind::Empty => "Empty",
                                }
                                .to_string()
                            }))
                            .when_some(object.last_modified.as_deref(), |d, t| {
                                d.child(format!("Modified {}", fmt_time(t)))
                            })
                            .child(fmt_ms(*elapsed_ms))
                            .when(*truncated || display_truncated, |d| {
                                d.child(format!(
                                    "Showing the first {}",
                                    fmt_size(PREVIEW_LIMIT.min(object.size as usize) as u64)
                                ))
                            }),
                    )
                    .when(!custom.is_empty(), |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .child(div().text_color(muted).child("Metadata"))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .truncate()
                                        .child(custom.join("  ")),
                                ),
                        )
                    })
                    .when_some(object.etag.as_deref(), |d, etag| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .font_family(mono.clone())
                                .child(format!("ETag {etag}")),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .p_2()
            .text_sm()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(body)
    }
}

impl gpui::EventEmitter<gpui_component::dock::PanelEvent> for R2Browser {}

impl gpui::Focusable for R2Browser {
    // Arrow keys should move through objects as soon as the tab opens.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.objects.read(cx).focus_handle(cx)
    }
}

impl BasePanel for R2Browser {
    fn panel_name(&self) -> &'static str {
        "R2Browser"
    }
}

impl Panel for R2Browser {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(Lucide::Archive).small())
            .child(self.bucket.name.clone())
    }
}

impl Render for R2Browser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let table = self.objects.read(cx).delegate();
        let folder = table.folder.clone();
        let loaded = table.entries.len();
        let more = table.cursor.is_some();
        let fetching = table.fetching;
        let error = table.error.clone();
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .key_context(BROWSER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::parent_folder))
            .child(
                h_flex()
                    .p_2()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("up")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::ArrowUp))
                            .tooltip("Parent folder (⌘↑)")
                            .disabled(folder.is_empty())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.parent_folder(&ParentFolder, window, cx)
                            })),
                    )
                    .child(self.render_breadcrumbs(&folder, cx))
                    .child(
                        div().w(px(280.)).child(
                            Input::new(&self.search)
                                .small()
                                .prefix(Icon::new(Lucide::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .child(div().text_sm().text_color(muted).child(format!(
                        "{} item{}{}{}",
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
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .truncate()
                                .child(e),
                        )
                    }),
            )
            .child(
                h_resizable("r2-split")
                    .child(
                        resizable_panel().size(px(560.)).child(
                            div()
                                .size_full()
                                .key_context(LIST_CONTEXT)
                                .child(DataTable::new(&self.objects).small().stripe(true)),
                        ),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .child(self.render_preview_header(cx))
                                .child(div().flex_1().min_h_0().child(self.viewer.clone())),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1023), "1023 B");
        assert_eq!(fmt_size(1536), "1.5 KB");
        assert_eq!(fmt_size(48_213), "47 KB");
        assert_eq!(fmt_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn parent_folders() {
        assert_eq!(parent("a/b/"), "a/");
        assert_eq!(parent("a/"), "");
        assert_eq!(parent(""), "");
    }

    #[test]
    fn big_images_are_not_previewed_but_big_text_is() {
        let object = |size, content_type: &str| Object {
            key: "k".into(),
            size,
            last_modified: None,
            etag: None,
            http_metadata: crate::cloudflare::r2::HttpMetadata {
                content_type: Some(content_type.into()),
                ..Default::default()
            },
            custom_metadata: Default::default(),
            storage_class: None,
        };
        assert!(previewable(&object(1024, "image/png")).is_ok());
        assert!(previewable(&object(PREVIEW_LIMIT as u64 + 1, "image/png")).is_err());
        assert!(previewable(&object(PREVIEW_LIMIT as u64 * 10, "text/plain")).is_ok());
    }
}
