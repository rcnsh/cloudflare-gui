//! SQL editor for one D1 database: run with cmd-Enter, see results inline,
//! browse per-database history. Writes need write mode plus confirmation.

use gpui::prelude::*;
use gpui::{App, Entity, FocusHandle, SharedString, Subscription, Task, Window, div, px};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::input::{Editor, EditorState};
use gpui_component::switch::Switch;
use gpui_component::table::{DataTable, TableState};
use gpui_component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{RunQuery, SQL_EDITOR_CONTEXT, ToggleWriteMode};
use crate::cloudflare::d1::{QueryIntent, StatementResult};
use crate::sql::{self, Classification};
use crate::state::Session;
use crate::state::history::HistoryEntry;
use crate::views::results_table::{ResultGrid, fmt_count, fmt_ms, set_loading, show_results};

enum Status {
    Idle,
    Running,
    Done(SharedString),
    /// The statement may write and write mode is off; nothing was sent.
    Blocked(SharedString),
    Failed(SharedString),
}

pub struct SqlEditor {
    session: Entity<Session>,
    database_id: String,
    database_name: String,
    editor: Entity<EditorState>,
    grid: Entity<TableState<ResultGrid>>,
    write_mode: bool,
    status: Status,
    show_history: bool,
    focus_handle: FocusHandle,
    _run: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SqlEditor {
    pub fn new(
        session: Entity<Session>,
        database_id: String,
        database_name: String,
        initial_sql: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let last = session
            .read(cx)
            .history
            .entries(&database_id)
            .first()
            .map(|e| e.sql.clone());
        let initial = initial_sql.or(last).unwrap_or_else(|| {
            "SELECT name, type FROM sqlite_master\nWHERE type IN ('table', 'view')\nORDER BY name;".into()
        });
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .soft_wrap(false)
                .placeholder("SELECT * FROM …")
                .default_value(initial)
        });
        let grid = cx.new(|cx| {
            TableState::new(ResultGrid::default(), window, cx)
                .col_movable(false)
                .sortable(false)
        });
        let focus_handle = cx.focus_handle();
        let subscriptions = Vec::new();
        Self {
            session,
            database_id,
            database_name,
            editor,
            grid,
            write_mode: false,
            status: Status::Idle,
            show_history: false,
            focus_handle,
            _run: None,
            _subscriptions: subscriptions,
        }
    }

    /// The selection if there is one, otherwise the whole buffer.
    fn current_sql(&self, cx: &App) -> String {
        let editor = self.editor.read(cx);
        let selected = editor.selected_value();
        if selected.trim().is_empty() {
            editor.value().to_string()
        } else {
            selected.to_string()
        }
    }

    fn run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Running) {
            return;
        }
        let sql = self.current_sql(cx);
        let classification = sql::classify(&sql);
        if classification.is_empty() {
            self.status = Status::Failed("Nothing to run.".into());
            cx.notify();
            return;
        }
        if classification.is_read_only() {
            self.execute(sql, QueryIntent::Read, cx);
        } else if !self.write_mode {
            let what = classification
                .writes()
                .map(|s| s.reason.clone().unwrap_or_else(|| s.keyword.clone()))
                .collect::<Vec<_>>()
                .join(", ");
            self.status = Status::Blocked(
                format!(
                    "Not run: this may modify data ({what}). Turn on write mode (⇧⌘W) to run it."
                )
                .into(),
            );
            cx.notify();
        } else {
            self.confirm_write(sql, classification, window, cx);
        }
    }

    fn confirm_write(
        &mut self,
        sql: String,
        classification: Classification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let db_name = self.database_name.clone();
        let db_id = self.database_id.clone();
        let reasons: Vec<String> = classification
            .writes()
            .map(|s| s.reason.clone().unwrap_or_else(|| s.keyword.clone()))
            .collect();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let sql_for_ok = sql.clone();
            let this = this.clone();
            alert
                .title(format!("Run on {db_name}?"))
                .description("This statement may change data. It runs once and is not retried.")
                .width(px(640.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .child(div().font_semibold().child("Database"))
                                .child(format!("{db_name} ({db_id})")),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .child(div().font_semibold().child("Why"))
                                .child(reasons.join(", ")),
                        )
                        .child(
                            div()
                                .id("confirm-sql")
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().muted)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_sm()
                                .child(sql.clone()),
                        ),
                )
                .confirm()
                .ok_text("Run statement")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let sql = sql_for_ok.clone();
                    let _ = this.update(cx, |this, cx| this.execute(sql, QueryIntent::Write, cx));
                    true
                })
        });
    }

    fn execute(&mut self, sql: String, intent: QueryIntent, cx: &mut Context<Self>) {
        self.status = Status::Running;
        set_loading(&self.grid, true, cx);
        let query = self
            .session
            .read(cx)
            .query(&self.database_id, sql.clone(), intent);
        self._run = Some(cx.spawn(async move |this, cx| {
            let result = query.await;
            let _ = this.update(cx, |this, cx| {
                this.finish(sql, result, cx);
            });
        }));
        cx.notify();
    }

    fn finish(
        &mut self,
        sql: String,
        result: Result<Vec<StatementResult>, String>,
        cx: &mut Context<Self>,
    ) {
        let mut entry = HistoryEntry {
            sql,
            at: chrono::Utc::now().timestamp(),
            ok: result.is_ok(),
            duration_ms: None,
            rows: None,
        };
        match result {
            Ok(results) => {
                let duration: f64 = results.iter().filter_map(|r| r.meta.duration).sum();
                let rows_read: f64 = results.iter().filter_map(|r| r.meta.rows_read).sum();
                let changes: f64 = results.iter().filter_map(|r| r.meta.changes).sum();
                // Show the last statement that produced a result set.
                let shown = results
                    .iter()
                    .rev()
                    .find(|r| !r.results.columns.is_empty())
                    .or(results.last())
                    .cloned()
                    .unwrap_or_default();
                let rows = shown.results.rows.len();
                show_results(&self.grid, shown.results.columns, &shown.results.rows, cx);
                let mut parts = vec![
                    format!(
                        "{} row{}",
                        fmt_count(rows as u64),
                        if rows == 1 { "" } else { "s" }
                    ),
                    fmt_ms(duration),
                    format!("{} rows read", fmt_count(rows_read as u64)),
                ];
                if changes > 0.0 {
                    parts.push(format!("{} changed", fmt_count(changes as u64)));
                }
                if results.len() > 1 {
                    parts.push(format!("{} statements", results.len()));
                }
                entry.duration_ms = Some(duration);
                entry.rows = Some(rows);
                self.status = Status::Done(parts.join(" · ").into());
            }
            Err(e) => {
                set_loading(&self.grid, false, cx);
                self.status = Status::Failed(e.into());
            }
        }
        let db = self.database_id.clone();
        self.session.update(cx, |s, _| s.history.record(&db, entry));
        cx.notify();
    }

    fn toggle_write_mode(&mut self, _: &ToggleWriteMode, _: &mut Window, cx: &mut Context<Self>) {
        self.write_mode = !self.write_mode;
        if matches!(self.status, Status::Blocked(_)) {
            self.status = Status::Idle;
        }
        cx.notify();
    }

    fn load_from_history(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            e.set_value(sql, window, cx);
            e.focus(window, cx);
        });
    }

    fn render_status(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (color, icon, text): (_, Option<Lucide>, SharedString) = match &self.status {
            Status::Idle => (
                theme.muted_foreground,
                None,
                "⌘↵ runs the selection, or everything".into(),
            ),
            Status::Running => (
                theme.muted_foreground,
                Some(Lucide::LoaderCircle),
                "Running…".into(),
            ),
            Status::Done(s) => (theme.muted_foreground, Some(Lucide::CircleCheck), s.clone()),
            Status::Blocked(s) => (theme.warning, Some(Lucide::Lock), s.clone()),
            Status::Failed(s) => (theme.danger, Some(Lucide::CircleAlert), s.clone()),
        };
        h_flex()
            .gap_1p5()
            .text_sm()
            .text_color(color)
            .min_w_0()
            .when_some(icon, |this, icon| this.child(Icon::new(icon).small()))
            .child(div().min_w_0().child(text))
    }

    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<HistoryEntry> = self
            .session
            .read(cx)
            .history
            .entries(&self.database_id)
            .to_vec();
        let now = chrono::Utc::now().timestamp();
        v_flex()
            .id("history")
            .w(px(300.))
            .h_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .overflow_y_scroll()
            .child(
                div()
                    .p_2()
                    .text_sm()
                    .font_semibold()
                    .child(format!("History · {}", self.database_name)),
            )
            .when(entries.is_empty(), |this| {
                this.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Queries you run here appear in this list."),
                )
            })
            .children(entries.into_iter().enumerate().map(|(ix, entry)| {
                let sql = entry.sql.clone();
                div()
                    .id(("history-item", ix))
                    .px_2()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.load_from_history(sql.clone(), window, cx)
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                Icon::new(if entry.ok {
                                    Lucide::CircleCheck
                                } else {
                                    Lucide::CircleX
                                })
                                .xsmall()
                                .text_color(if entry.ok {
                                    cx.theme().success
                                } else {
                                    cx.theme().danger
                                }),
                            )
                            .child(ago(now - entry.at))
                            .when_some(entry.rows, |this, rows| {
                                this.child(format!("· {} rows", fmt_count(rows as u64)))
                            })
                            .when_some(entry.duration_ms, |this, ms| {
                                this.child(format!("· {}", fmt_ms(ms)))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .line_clamp(3)
                            .child(entry.sql.trim().to_string()),
                    )
            }))
    }
}

fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

impl gpui::EventEmitter<gpui_component::dock::PanelEvent> for SqlEditor {}

impl gpui::Focusable for SqlEditor {
    // Focusing the tab means typing into the editor straight away.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl BasePanel for SqlEditor {
    fn panel_name(&self) -> &'static str {
        "SqlEditor"
    }
}

impl Panel for SqlEditor {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(Lucide::SquareTerminal).small())
            .child(self.database_name.clone())
            .when(self.write_mode, |this| {
                this.child(
                    Icon::new(Lucide::LockOpen)
                        .xsmall()
                        .text_color(cx.theme().warning),
                )
            })
    }
}

impl Render for SqlEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = matches!(self.status, Status::Running);
        let write_mode = self.write_mode;
        v_flex()
            .size_full()
            .key_context(SQL_EDITOR_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::toggle_write_mode))
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(write_mode, |this| this.bg(cx.theme().warning.opacity(0.12)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("run")
                                    .primary()
                                    .small()
                                    .icon(Icon::new(Lucide::Play))
                                    .label("Run")
                                    .loading(running)
                                    .tooltip("Run (⌘↵)")
                                    .on_click(cx.listener(|this, _, w, cx| this.run(&RunQuery, w, cx))),
                            )
                            .child(
                                Switch::new("write-mode")
                                    .small()
                                    .checked(write_mode)
                                    .label(if write_mode { "Write mode on" } else { "Read-only" })
                                    .tooltip("Allow statements that modify data, after confirmation (⇧⌘W)")
                                    .on_click(cx.listener(|this, _: &bool, w, cx| {
                                        this.toggle_write_mode(&ToggleWriteMode, w, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("on {}", self.database_name)),
                            ),
                    )
                    .child(
                        Button::new("history")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::ListRestart))
                            .label("History")
                            .selected(self.show_history)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_history = !this.show_history;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().h(px(220.)).child(Editor::new(&self.editor).size_full().bordered(false)))
                            .child(
                                h_flex()
                                    .px_2()
                                    .py_1()
                                    .border_t_1()
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(self.render_status(cx)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(DataTable::new(&self.grid).stripe(true).small()),
                            ),
                    )
                    .when(self.show_history, |this| this.child(self.render_history(cx))),
            )
    }
}
