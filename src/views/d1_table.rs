//! Pages through one D1 table, 100 rows at a time.

use gpui::prelude::*;
use gpui::{App, Entity, FocusHandle, SharedString, Task, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::table::{DataTable, TableState};
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName as Lucide;
use serde_json::Value;

use crate::actions::{BROWSER_CONTEXT, NextPage, OpenResource, PreviousPage};
use crate::cloudflare::d1::{QueryIntent, quote_ident};
use crate::panel_basics;
use crate::state::{Resource, Session};
use crate::views::results_table::{ResultGrid, fmt_count, fmt_ms, set_loading, show_results};

const PAGE_SIZE: usize = 100;

enum Status {
    Loading,
    Loaded {
        shown: usize,
        duration_ms: f64,
        rows_read: u64,
    },
    Failed(SharedString),
}

pub struct TableBrowser {
    session: Entity<Session>,
    database_id: String,
    database_name: String,
    table: String,
    grid: Entity<TableState<ResultGrid>>,
    page: usize,
    has_next: bool,
    status: Status,
    row_count: Option<Result<u64, SharedString>>,
    counting: bool,
    focus_handle: FocusHandle,
    _load: Option<Task<()>>,
    _count: Option<Task<()>>,
}

impl TableBrowser {
    pub fn new(
        session: Entity<Session>,
        database_id: String,
        database_name: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            TableState::new(ResultGrid::default(), window, cx)
                .col_movable(false)
                .sortable(false)
        });
        let mut this = Self {
            session,
            database_id,
            database_name,
            table,
            grid,
            page: 0,
            has_next: false,
            status: Status::Loading,
            row_count: None,
            counting: false,
            focus_handle: cx.focus_handle(),
            _load: None,
            _count: None,
        };
        this.load_page(0, cx);
        this
    }

    fn load_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.page = page;
        self.status = Status::Loading;
        set_loading(&self.grid, true, cx);
        // One extra row tells us whether a next page exists without a count(*),
        // which would read (and bill) every row in the table.
        let sql = format!(
            "SELECT * FROM {} LIMIT {} OFFSET {}",
            quote_ident(&self.table),
            PAGE_SIZE + 1,
            page * PAGE_SIZE
        );
        let query = self
            .session
            .read(cx)
            .query(&self.database_id, sql, QueryIntent::Read);
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = query.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(results) => {
                        let first = results.into_iter().next().unwrap_or_default();
                        let mut rows: Vec<Vec<Value>> = first.results.rows;
                        this.has_next = rows.len() > PAGE_SIZE;
                        rows.truncate(PAGE_SIZE);
                        this.status = Status::Loaded {
                            shown: rows.len(),
                            duration_ms: first.meta.duration.unwrap_or(0.0),
                            rows_read: first.meta.rows_read.unwrap_or(0.0) as u64,
                        };
                        show_results(&this.grid, first.results.columns, &rows, cx);
                    }
                    Err(e) => {
                        this.status = Status::Failed(e.into());
                        set_loading(&this.grid, false, cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_next && !matches!(self.status, Status::Loading) {
            self.load_page(self.page + 1, cx);
        }
    }

    fn previous_page(&mut self, _: &PreviousPage, _: &mut Window, cx: &mut Context<Self>) {
        if self.page > 0 && !matches!(self.status, Status::Loading) {
            self.load_page(self.page - 1, cx);
        }
    }

    fn count_rows(&mut self, cx: &mut Context<Self>) {
        self.counting = true;
        let sql = format!("SELECT count(*) FROM {}", quote_ident(&self.table));
        let query = self
            .session
            .read(cx)
            .query(&self.database_id, sql, QueryIntent::Read);
        self._count = Some(cx.spawn(async move |this, cx| {
            let result = query.await;
            let _ = this.update(cx, |this, cx| {
                this.counting = false;
                this.row_count = Some(
                    result
                        .and_then(|r| {
                            r.first()
                                .and_then(|s| s.results.rows.first())
                                .and_then(|row| row.first())
                                .and_then(Value::as_u64)
                                .ok_or_else(|| "no count returned".to_string())
                        })
                        .map_err(SharedString::from),
                );
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_status(&self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let start = self.page * PAGE_SIZE;
        let text: SharedString = match &self.status {
            Status::Loading => "Loading…".into(),
            Status::Loaded { shown: 0, .. } if self.page == 0 => "No rows".into(),
            Status::Loaded {
                shown,
                duration_ms,
                rows_read,
            } => format!(
                "Rows {}–{} · {} · {} rows read",
                fmt_count((start + 1) as u64),
                fmt_count((start + shown) as u64),
                fmt_ms(*duration_ms),
                fmt_count(*rows_read)
            )
            .into(),
            Status::Failed(_) => "Query failed".into(),
        };
        h_flex()
            .gap_3()
            .text_sm()
            .text_color(muted)
            .child(text)
            .when_some(self.row_count.clone(), |this, count| match count {
                Ok(n) => this.child(format!("{} rows total", fmt_count(n))),
                Err(e) => this.child(div().text_color(cx.theme().danger).child(e)),
            })
    }
}

panel_basics!(TableBrowser);

impl BasePanel for TableBrowser {
    fn panel_name(&self) -> &'static str {
        "D1Table"
    }
}

impl Panel for TableBrowser {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(Lucide::Table).small())
            .child(self.table.clone())
    }
}

impl Render for TableBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = matches!(self.status, Status::Loading);
        let database = Resource::D1Database {
            id: self.database_id.clone(),
            name: self.database_name.clone(),
        };
        v_flex()
            .size_full()
            .key_context(BROWSER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::previous_page))
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} /", self.database_name)),
                            )
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.table.clone()),
                            )
                            .child(self.render_status(cx)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("count")
                                    .ghost()
                                    .small()
                                    .label("Count rows")
                                    .loading(self.counting)
                                    .tooltip("Runs count(*), which reads every row")
                                    .on_click(cx.listener(|this, _, _, cx| this.count_rows(cx))),
                            )
                            .child(
                                Button::new("sql")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(Lucide::SquareTerminal))
                                    .label("SQL")
                                    .on_click(move |_, window, cx| {
                                        window.dispatch_action(
                                            Box::new(OpenResource(database.clone())),
                                            cx,
                                        )
                                    }),
                            )
                            .child(
                                Button::new("prev")
                                    .outline()
                                    .small()
                                    .icon(Icon::new(Lucide::ChevronLeft))
                                    .tooltip("Previous page (⌘[)")
                                    .disabled(self.page == 0 || loading)
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.previous_page(&PreviousPage, w, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .px_1()
                                    .child(format!("Page {}", self.page + 1)),
                            )
                            .child(
                                Button::new("next")
                                    .outline()
                                    .small()
                                    .icon(Icon::new(Lucide::ChevronRight))
                                    .tooltip("Next page (⌘])")
                                    .disabled(!self.has_next || loading)
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.next_page(&NextPage, w, cx)
                                    })),
                            ),
                    ),
            )
            .when_some(
                match &self.status {
                    Status::Failed(e) => Some(e.clone()),
                    _ => None,
                },
                |this, error| {
                    this.child(
                        div()
                            .p_2()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&self.grid).stripe(true).small()),
            )
    }
}
