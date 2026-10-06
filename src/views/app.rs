//! The window's content: the setup flow until there's a token and an account,
//! then the workspace.

use gpui::prelude::*;
use gpui::{Entity, FocusHandle, Focusable, Subscription, Window, div};
use gpui_component::{
    ActiveTheme as _, StyledExt as _, Theme, ThemeMode, TitleBar, WindowExt as _, h_flex, v_flex,
};

use crate::actions::{SignOut, SwitchAccount, ToggleDarkMode};
use crate::cloudflare::accounts::Account;
use crate::cloudflare::{Client, Token};
use crate::keychain;
use crate::state::Session;
use crate::state::settings::Settings;
use crate::views::setup::{SetupEvent, SetupView};
use crate::views::workspace::Workspace;

enum Screen {
    /// Waiting on the Keychain, which may be showing an access prompt.
    Loading,
    Setup(Entity<SetupView>),
    Workspace(Entity<Workspace>),
}

pub struct AppView {
    screen: Screen,
    focus_handle: FocusHandle,
    _screen_subscription: Option<Subscription>,
    _appearance: Subscription,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        #[cfg(feature = "devtools")]
        if let Some((base, account)) = crate::devtools::demo::start() {
            let mut this = Self {
                screen: Screen::Loading,
                focus_handle: cx.focus_handle(),
                _screen_subscription: None,
                _appearance: appearance,
            };
            let client = Client::with_base(Token::new("demo"), base);
            this.show_workspace(client, account, window, cx);
            return this;
        }
        // The Keychain can block for as long as its access prompt is open, so
        // read it off the main thread and keep the window responsive.
        let load = cx
            .background_executor()
            .spawn(async { keychain::load_token() });
        cx.spawn_in(window, async move |this, cx| {
            let token = load.await.unwrap_or_else(|e| {
                log::error!("{e:#}");
                None
            });
            let settings = Settings::load();
            let _ = this.update_in(cx, |this, window, cx| match (token, settings.account_id) {
                (Some(token), Some(id)) => {
                    let account = Account {
                        name: settings.account_name.unwrap_or_else(|| id.clone()),
                        id,
                        kind: None,
                    };
                    this.show_workspace(Client::new(token), account, window, cx);
                }
                (token, _) => this.show_setup(token, window, cx),
            });
        })
        .detach();
        Self {
            screen: Screen::Loading,
            focus_handle: cx.focus_handle(),
            _screen_subscription: None,
            _appearance: appearance,
        }
    }

    fn show_setup(&mut self, token: Option<Token>, window: &mut Window, cx: &mut Context<Self>) {
        let setup = cx.new(|cx| SetupView::new(token, window, cx));
        self._screen_subscription =
            Some(
                cx.subscribe_in(&setup, window, |this, _, event, window, cx| {
                    let SetupEvent::Completed { token, account } = event;
                    Settings {
                        account_id: Some(account.id.clone()),
                        account_name: Some(account.name.clone()),
                    }
                    .save();
                    this.show_workspace(Client::new(token.clone()), account.clone(), window, cx);
                }),
            );
        setup.update(cx, |setup, cx| setup.focus(window, cx));
        self.screen = Screen::Setup(setup);
        cx.notify();
    }

    fn show_workspace(
        &mut self,
        client: Client,
        account: Account,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.new(|_| Session::new(client, account));
        let workspace = cx.new(|cx| Workspace::new(session, window, cx));
        workspace.read(cx).focus_handle(cx).focus(window, cx);
        self._screen_subscription = None;
        self.screen = Screen::Workspace(workspace);
        cx.notify();
    }

    fn sign_out(&mut self, _: &SignOut, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = keychain::delete_token() {
            window.push_notification(
                gpui_component::notification::Notification::error(format!("{e:#}")),
                cx,
            );
        }
        Settings::default().save();
        self.show_setup(None, window, cx);
    }

    fn switch_account(&mut self, _: &SwitchAccount, window: &mut Window, cx: &mut Context<Self>) {
        let token = match &self.screen {
            Screen::Workspace(w) => Some(w.read(cx).session.read(cx).client.token().clone()),
            Screen::Setup(_) | Screen::Loading => None,
        };
        self.show_setup(token, window, cx);
    }

    fn toggle_dark_mode(
        &mut self,
        _: &ToggleDarkMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, Some(window), cx);
    }
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let account = match &self.screen {
            Screen::Workspace(w) => Some(w.read(cx).session.read(cx).account.name.clone()),
            Screen::Setup(_) | Screen::Loading => None,
        };
        let title = h_flex()
            .gap_2()
            .child(div().font_semibold().child("Cloudflare GUI"))
            .when_some(account, |this, name| {
                this.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("· {name}")),
                )
            });
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::sign_out))
            .on_action(cx.listener(Self::switch_account))
            .on_action(cx.listener(Self::toggle_dark_mode))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(title))
            .child(div().flex_1().min_h_0().map(|this| {
                match &self.screen {
                    Screen::Loading => this.flex().items_center().justify_center().child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Reading the API token from the Keychain…"),
                    ),
                    Screen::Setup(view) => this.child(view.clone()),
                    Screen::Workspace(view) => this.child(view.clone()),
                }
            }))
    }
}
