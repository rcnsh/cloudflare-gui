//! Live logs from one Worker: start a tail, stream events into a filterable
//! list, pause and clear, and see any event's full JSON.
//!
//! Starting a tail creates a short-lived tail session on the account, so it
//! only happens when the user presses Start, and the session is deleted when
//! they stop, the tab closes or the app quits (see `state::tails`).

use std::collections::VecDeque;

use futures::StreamExt as _;
use gpui::prelude::*;
use gpui::{
    App, Entity, FocusHandle, Hsla, SharedString, Subscription, Task, UniformListScrollHandle,
    Window, div, px, uniform_list,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{resizable_panel, v_resizable};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{ClearLog, FocusSearch, TAIL_CONTEXT, ToggleTail, ToggleTailPause};
use crate::cloudflare::workers::{TailEvent, TailUpdate, format_log_message, stream_tail};
use crate::runtime::{self, AbortOnDrop};
use crate::state::Session;
use crate::state::tails::{self, OpenTail};
use crate::views::results_table::fmt_count;
use crate::views::value_viewer::ValueViewer;

/// Older lines are dropped past this, so a busy Worker can't grow memory forever.
const MAX_LINES: usize = 20_000;
const ROW_HEIGHT: f32 = 22.;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn parse(level: &str) -> Self {
        match level {
            "debug" | "trace" => Level::Debug,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// The invocation itself: trigger and outcome.
    Invocation,
    Log,
    Exception,
}

#[derive(Debug, Clone)]
pub struct Line {
    /// Which event this line came from, for the detail view.
    pub event: u64,
    pub timestamp: i64,
    pub level: Level,
    pub kind: LineKind,
    pub text: SharedString,
    /// Lowercased once so filtering doesn't allocate per keystroke per line.
    haystack: String,
}

impl Line {
    fn new(event: u64, timestamp: i64, level: Level, kind: LineKind, text: String) -> Self {
        Self {
            event,
            timestamp,
            level,
            kind,
            haystack: text.to_lowercase(),
            text: text.into(),
        }
    }
}

/// One line for the invocation, then one per console call and exception.
pub fn lines_for(seq: u64, event: &TailEvent) -> Vec<Line> {
    let failed = event.outcome != "ok" || !event.exceptions.is_empty();
    let mut header = event.trigger();
    if event.outcome != "ok" && !event.outcome.is_empty() {
        header.push_str(&format!(" · {}", event.outcome));
    }
    let mut lines = vec![Line::new(
        seq,
        event.event_timestamp,
        if failed { Level::Error } else { Level::Info },
        LineKind::Invocation,
        header,
    )];
    for log in &event.logs {
        lines.push(Line::new(
            seq,
            log.timestamp,
            Level::parse(&log.level),
            LineKind::Log,
            format_log_message(&log.message),
        ));
    }
    for e in &event.exceptions {
        let message = match &e.message {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        lines.push(Line::new(
            seq,
            e.timestamp,
            Level::Error,
            LineKind::Exception,
            format!("{}: {message}", e.name),
        ));
    }
    lines
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub min_level: Option<Level>,
    /// Lowercased.
    pub text: String,
}

impl Filter {
    pub fn matches(&self, line: &Line) -> bool {
        self.min_level.is_none_or(|min| line.level >= min)
            && (self.text.is_empty() || line.haystack.contains(&self.text))
    }
}

fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S%.3f")
                .to_string()
        })
        .unwrap_or_default()
}

enum State {
    Idle,
    Starting,
    Live,
    Reconnecting(SharedString),
    Stopped,
    Failed(SharedString),
}

pub struct WorkerTail {
    script: String,
    session: Entity<Session>,
    state: State,
    tail_id: Option<String>,
    paused: bool,
    /// Events that arrived while paused, shown on resume.
    held: Vec<TailEvent>,
    events: VecDeque<(u64, TailEvent)>,
    next_seq: u64,
    lines: VecDeque<Line>,
    /// Indices into `lines` that pass the filter.
    visible: Vec<usize>,
    filter: Filter,
    selected: Option<u64>,
    search: Entity<InputState>,
    viewer: Entity<ValueViewer>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _stream: Option<AbortOnDrop>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl WorkerTail {
    pub fn new(
        session: Entity<Session>,
        script: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter logs…")
                .clean_on_escape()
        });
        let viewer = cx.new(|cx| ValueViewer::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if let InputEvent::Change = event {
                    this.filter.text = state.read(cx).value().to_lowercase();
                    this.refilter(cx);
                }
            }),
        ];
        Self {
            script,
            session,
            state: State::Idle,
            tail_id: None,
            paused: false,
            held: Vec::new(),
            events: VecDeque::new(),
            next_seq: 0,
            lines: VecDeque::new(),
            visible: Vec::new(),
            filter: Filter::default(),
            selected: None,
            search,
            viewer,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _stream: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    fn is_running(&self) -> bool {
        matches!(
            self.state,
            State::Starting | State::Live | State::Reconnecting(_)
        )
    }

    fn toggle(&mut self, _: &ToggleTail, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            self.stop(State::Stopped, cx);
        } else {
            self.start(cx);
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.state = State::Starting;
        cx.notify();
        #[cfg(feature = "devtools")]
        if let Some(url) = crate::devtools::fake_tail_url() {
            self._task =
                Some(cx.spawn(async move |this, cx| Self::stream(this, url, None, cx).await));
            return;
        }
        let (client, account_id) = {
            let s = self.session.read(cx);
            (s.client.clone(), s.account_id())
        };
        let script = self.script.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let created = {
                let (client, account_id, script) =
                    (client.clone(), account_id.clone(), script.clone());
                runtime::run(async move { client.create_tail(&account_id, &script).await }).await
            };
            let error = match created {
                Ok(Ok(tail)) => {
                    let open = OpenTail {
                        client,
                        account_id,
                        script,
                        id: tail.id,
                    };
                    return Self::stream(this, tail.url, Some(open), cx).await;
                }
                Ok(Err(e)) => e.to_string(),
                Err(e) => e.to_string(),
            };
            let _ = this.update(cx, |this, cx| this.stop(State::Failed(error.into()), cx));
        }));
    }

    /// Connects to the tail and feeds its events into the view until the
    /// stream ends or the view goes away.
    async fn stream(
        this: gpui::WeakEntity<Self>,
        url: String,
        open: Option<OpenTail>,
        cx: &mut gpui::AsyncApp,
    ) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        // Registered in the same synchronous step that hands the id to the
        // view, so there's no moment where the tail exists but nobody will
        // delete it.
        let attached = this.update(cx, |this, _| {
            if let Some(open) = &open {
                tails::register(open.clone());
                this.tail_id = Some(open.id.clone());
            }
            let stream = runtime::spawn(stream_tail(url, tx));
            this._stream = Some(AbortOnDrop(stream.abort_handle()));
        });
        if attached.is_err() {
            if let Some(open) = open {
                let id = open.id.clone();
                tails::register(open);
                tails::close(&id);
            }
            return;
        }
        while let Some(first) = rx.next().await {
            // Busy Workers send bursts; one redraw per burst is plenty.
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            if this.update(cx, |this, cx| this.apply(batch, cx)).is_err() {
                return;
            }
        }
    }

    /// Stops streaming and deletes the tail session. Safe to call repeatedly.
    fn stop(&mut self, state: State, cx: &mut Context<Self>) {
        self._stream = None;
        self._task = None;
        if let Some(id) = self.tail_id.take() {
            tails::close(&id);
        }
        self.state = state;
        cx.notify();
    }

    fn apply(&mut self, updates: Vec<TailUpdate>, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        for update in updates {
            match update {
                TailUpdate::Connected => self.state = State::Live,
                TailUpdate::Reconnecting { attempt, reason } => {
                    self.state = State::Reconnecting(format!("attempt {attempt}: {reason}").into())
                }
                TailUpdate::Failed(reason) => {
                    self.stop(State::Failed(reason.into()), cx);
                }
                TailUpdate::Event(event) => events.push(*event),
            }
        }
        if self.paused {
            self.held.extend(events);
        } else {
            self.ingest(events);
        }
        cx.notify();
    }

    fn ingest(&mut self, events: Vec<TailEvent>) {
        if events.is_empty() {
            return;
        }
        // Only follow new lines if the user hasn't scrolled up to read something.
        let follow = self.scroll.is_scrolled_to_end().unwrap_or(true);
        for event in events {
            let seq = self.next_seq;
            self.next_seq += 1;
            for line in lines_for(seq, &event) {
                if self.filter.matches(&line) {
                    self.visible.push(self.lines.len());
                }
                self.lines.push_back(line);
            }
            self.events.push_back((seq, event));
        }
        if self.lines.len() > MAX_LINES {
            // Trim a chunk at a time so this doesn't run on every event.
            let drop = self.lines.len() - MAX_LINES + MAX_LINES / 10;
            self.lines.drain(..drop);
            let oldest = self.lines.front().map(|l| l.event).unwrap_or(u64::MAX);
            while self.events.front().is_some_and(|(seq, _)| *seq < oldest) {
                self.events.pop_front();
            }
            self.recompute_visible();
        }
        if follow {
            self.scroll.scroll_to_bottom();
        }
    }

    fn recompute_visible(&mut self) {
        self.visible = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| self.filter.matches(line))
            .map(|(ix, _)| ix)
            .collect();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.recompute_visible();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn set_min_level(&mut self, level: Option<Level>, cx: &mut Context<Self>) {
        self.filter.min_level = level;
        self.refilter(cx);
    }

    fn toggle_pause(&mut self, _: &ToggleTailPause, _: &mut Window, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        if !self.paused {
            let held = std::mem::take(&mut self.held);
            self.ingest(held);
        }
        cx.notify();
    }

    fn clear(&mut self, _: &ClearLog, _: &mut Window, cx: &mut Context<Self>) {
        self.events.clear();
        self.lines.clear();
        self.visible.clear();
        self.held.clear();
        self.selected = None;
        self.viewer.update(cx, |v, cx| v.clear(cx));
        cx.notify();
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn select(&mut self, seq: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(seq);
        if let Some((_, event)) = self.events.iter().find(|(s, _)| *s == seq) {
            let bytes = serde_json::to_vec(&event.raw).unwrap_or_default();
            self.viewer
                .update(cx, |v, cx| v.show(&bytes, None, window, cx));
        }
        cx.notify();
    }

    fn level_color(&self, level: Level, cx: &App) -> Hsla {
        match level {
            Level::Debug => cx.theme().muted_foreground,
            Level::Info => cx.theme().foreground,
            Level::Warn => cx.theme().warning,
            Level::Error => cx.theme().danger,
        }
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let selected_bg = cx.theme().list_active;
        range
            .filter_map(|ix| {
                let line = self.lines.get(*self.visible.get(ix)?)?;
                let seq = line.event;
                let invocation = line.kind == LineKind::Invocation;
                Some(
                    h_flex()
                        .id(ix)
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px_2()
                        .gap_3()
                        .text_sm()
                        .font_family(mono.clone())
                        .when(self.selected == Some(seq), |d| d.bg(selected_bg))
                        .when(invocation && ix > 0, |d| {
                            d.border_t_1().border_color(cx.theme().border)
                        })
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.select(seq, window, cx)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(clock(line.timestamp)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(44.))
                                .text_color(self.level_color(line.level, cx))
                                .child(if invocation { "" } else { line.level.label() }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(!invocation, |d| d.pl_4())
                                .text_color(match line.kind {
                                    LineKind::Invocation if line.level == Level::Error => {
                                        cx.theme().danger
                                    }
                                    LineKind::Invocation => cx.theme().foreground,
                                    _ => self.level_color(line.level, cx),
                                })
                                .child(line.text.clone()),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn status_text(&self) -> String {
        let count = format!(
            "{} event{}",
            fmt_count(self.events.len() as u64),
            if self.events.len() == 1 { "" } else { "s" }
        );
        let state = match &self.state {
            State::Idle => return "Not started".into(),
            State::Starting => "Starting…".to_string(),
            State::Live if self.paused => {
                format!("Paused · {} waiting", fmt_count(self.held.len() as u64))
            }
            State::Live => "Live".to_string(),
            State::Reconnecting(reason) => format!("Reconnecting ({reason})"),
            State::Stopped => "Stopped".to_string(),
            State::Failed(reason) => format!("Stopped: {reason}"),
        };
        format!("{state} · {count}")
    }
}

impl Drop for WorkerTail {
    // Signing out or switching accounts drops the whole workspace without
    // telling each panel, so clean up here too.
    fn drop(&mut self) {
        if let Some(id) = self.tail_id.take() {
            tails::close(&id);
        }
    }
}

crate::panel_basics!(WorkerTail);

impl BasePanel for WorkerTail {
    fn panel_name(&self) -> &'static str {
        "WorkerTail"
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.stop(State::Stopped, cx);
    }
}

impl Panel for WorkerTail {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(Lucide::ScrollText).small())
            .child(self.script.clone())
            .when(self.is_running(), |d| d.child("·").child("live"))
    }
}

impl Render for WorkerTail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.is_running();
        let muted = cx.theme().muted_foreground;
        let min = self.filter.min_level;
        let levels = [
            ("All", None),
            ("Info", Some(Level::Info)),
            ("Warn", Some(Level::Warn)),
            ("Error", Some(Level::Error)),
        ];
        let failed = matches!(self.state, State::Failed(_));

        let toolbar = h_flex()
            .p_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("start")
                    .small()
                    .when(running, |b| b.outline())
                    .when(!running, |b| b.primary())
                    .icon(Icon::new(if running {
                        Lucide::Square
                    } else {
                        Lucide::Play
                    }))
                    .label(if running { "Stop" } else { "Start" })
                    .tooltip(if running {
                        "Stops streaming and deletes the tail session (⌘↩)"
                    } else {
                        "Creates a temporary tail session on this Worker (⌘↩)"
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle(&ToggleTail, window, cx)),
                    ),
            )
            .child(
                Button::new("pause")
                    .small()
                    .ghost()
                    .disabled(!running)
                    .selected(self.paused)
                    .icon(Icon::new(Lucide::Pause))
                    .label(if self.paused { "Resume" } else { "Pause" })
                    .tooltip("⌘.")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_pause(&ToggleTailPause, window, cx)
                    })),
            )
            .child(
                Button::new("clear")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::Eraser))
                    .label("Clear")
                    .tooltip("⌘⇧K")
                    .on_click(cx.listener(|this, _, window, cx| this.clear(&ClearLog, window, cx))),
            )
            .child(
                ButtonGroup::new("levels")
                    .outline()
                    .small()
                    .children(levels.iter().enumerate().map(|(ix, (label, level))| {
                        Button::new(("level", ix))
                            .label(*label)
                            .selected(min == *level)
                    }))
                    .on_click(cx.listener(move |this, ixs: &Vec<usize>, _, cx| {
                        if let Some(&ix) = ixs.first() {
                            this.set_min_level(levels[ix].1, cx);
                        }
                    })),
            )
            .child(
                div().w(px(280.)).child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(Lucide::Search).small())
                        .cleanable(true),
                ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if failed { cx.theme().danger } else { muted })
                    .truncate()
                    .child(self.status_text()),
            );

        let list = if self.lines.is_empty() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(match self.state {
                    State::Idle | State::Stopped | State::Failed(_) => {
                        "Press Start to stream this Worker's logs. A tail only sees \
                         invocations that happen while it is running."
                    }
                    _ => "Waiting for the Worker to be invoked…",
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .size_full()
                .child(
                    uniform_list(
                        "tail-lines",
                        self.visible.len(),
                        cx.processor(Self::render_rows),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };

        v_flex()
            .size_full()
            .key_context(TAIL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle))
            .on_action(cx.listener(Self::toggle_pause))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::focus_search))
            .child(toolbar)
            .child(
                v_resizable("tail-split")
                    .child(resizable_panel().child(list))
                    .child(
                        resizable_panel().size(px(240.)).child(
                            v_flex()
                                .size_full()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(div().p_2().text_sm().text_color(muted).child(
                                    if self.selected.is_some() {
                                        "Event"
                                    } else {
                                        "Click a line to see the whole event"
                                    },
                                ))
                                .child(div().flex_1().min_h_0().child(self.viewer.clone())),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::testing::fixture;
    use crate::cloudflare::workers::parse_tail_event;

    fn event(name: &str) -> TailEvent {
        parse_tail_event(fixture(name).as_bytes()).unwrap()
    }

    #[test]
    fn request_event_becomes_invocation_logs_and_exceptions() {
        let lines = lines_for(7, &event("tail_event_request.json"));
        assert_eq!(lines[0].kind, LineKind::Invocation);
        assert_eq!(
            lines[0].level,
            Level::Error,
            "the fixture's outcome is exception"
        );
        assert!(lines[0].text.contains("exception"));
        assert!(lines.iter().all(|l| l.event == 7));
        assert!(lines.iter().any(|l| l.kind == LineKind::Log));
        assert_eq!(lines.last().unwrap().kind, LineKind::Exception);
    }

    #[test]
    fn scheduled_event_is_a_plain_invocation() {
        let lines = lines_for(0, &event("tail_event_scheduled.json"));
        assert!(lines[0].text.starts_with("cron "));
        assert_eq!(lines[0].level, Level::Info);
    }

    #[test]
    fn filter_by_level_and_case_insensitive_text() {
        let line = |level, text: &str| Line::new(0, 0, level, LineKind::Log, text.into());
        let warn = line(Level::Warn, "Cache MISS for /api");
        let mut filter = Filter::default();
        assert!(filter.matches(&warn));
        filter.min_level = Some(Level::Warn);
        assert!(filter.matches(&warn));
        assert!(!filter.matches(&line(Level::Info, "hello")));
        filter.min_level = Some(Level::Error);
        assert!(!filter.matches(&warn));
        filter.min_level = None;
        filter.text = "miss".into();
        assert!(filter.matches(&warn));
        filter.text = "hit".into();
        assert!(!filter.matches(&warn));
    }

    #[test]
    fn levels_parse_with_log_as_info() {
        assert_eq!(Level::parse("log"), Level::Info);
        assert_eq!(Level::parse("info"), Level::Info);
        assert_eq!(Level::parse("debug"), Level::Debug);
        assert_eq!(Level::parse("warn"), Level::Warn);
        assert_eq!(Level::parse("error"), Level::Error);
    }
}
