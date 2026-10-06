//! First-run flow: paste a token, verify it against Cloudflare, pick an account.

use gpui::prelude::*;
use gpui::{
    App, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription, Task, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};

use crate::cloudflare::accounts::Account;
use crate::cloudflare::{Client, Token};
use crate::{keychain, runtime};

const CREATE_TOKEN_URL: &str = "https://dash.cloudflare.com/profile/api-tokens";

pub enum SetupEvent {
    Completed { token: Token, account: Account },
}

enum Step {
    EnterToken,
    PickAccount {
        token: Token,
        accounts: Vec<Account>,
    },
}

pub struct SetupView {
    token_input: Entity<InputState>,
    step: Step,
    verifying: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _verify: Option<Task<()>>,
    _input_events: Subscription,
}

impl EventEmitter<SetupEvent> for SetupView {}

impl SetupView {
    /// With an existing token (switching accounts) it goes straight to the account list.
    pub fn new(token: Option<Token>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste a Cloudflare API token")
                .masked(true)
        });
        let input_events = cx.subscribe_in(&token_input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.verify(window, cx);
            }
        });
        let mut this = Self {
            token_input,
            step: Step::EnterToken,
            verifying: false,
            error: None,
            focus_handle: cx.focus_handle(),
            _verify: None,
            _input_events: input_events,
        };
        if let Some(token) = token {
            this.check_token(token, false, cx);
        }
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.token_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn verify(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let value = self.token_input.read(cx).value().to_string();
        if value.trim().is_empty() {
            self.error = Some("Paste an API token first.".into());
            cx.notify();
            return;
        }
        self.check_token(Token::new(value), true, cx);
    }

    fn check_token(&mut self, token: Token, save: bool, cx: &mut Context<Self>) {
        self.verifying = true;
        self.error = None;
        cx.notify();
        let client = Client::new(token.clone());
        let request = runtime::run(async move { client.verify_and_list_accounts().await });
        self._verify = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.verifying = false;
                match result {
                    Ok(Ok(accounts)) => {
                        if save && let Err(e) = keychain::save_token(&token) {
                            this.error = Some(format!("{e:#}").into());
                        }
                        this.step = Step::PickAccount { token, accounts };
                    }
                    Ok(Err(e)) if e.is_auth() => {
                        this.error = Some(
                            format!("Cloudflare rejected this token: {e}. Check it hasn't expired and has the permissions listed below.").into(),
                        );
                    }
                    Ok(Err(e)) => this.error = Some(e.to_string().into()),
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    fn pick(&mut self, account: Account, cx: &mut Context<Self>) {
        if let Step::PickAccount { token, .. } = &self.step {
            cx.emit(SetupEvent::Completed {
                token: token.clone(),
                account,
            });
        }
    }

    fn render_token_step(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .child(div().text_xl().font_semibold().child("Connect your Cloudflare account"))
            .child(div().text_color(muted).child(
                "Create a custom API token scoped to one account, paste it here, and it's stored in your macOS Keychain. \
                 Nothing is written to your account: the app is read-only unless you turn on write mode in a tab.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.token_input).mask_toggle().cleanable(true)))
                    .child(
                        Button::new("verify")
                            .primary()
                            .label("Verify")
                            .loading(self.verifying)
                            .on_click(cx.listener(|this, _, window, cx| this.verify(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_color(cx.theme().danger)
                        .child(Icon::new(IconName::CircleX).small())
                        .child(div().flex_1().child(error)),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .text_sm()
                    .child(div().font_semibold().child("Token permissions (Account scope, Read):"))
                    .child(div().text_color(muted).child(
                        "Account Settings · D1 · Workers KV Storage · Workers R2 Storage · Workers Scripts · Workers Tail",
                    ))
                    .child(div().text_color(muted).child("Add D1 Edit only if you want write mode for SQL.")),
            )
            .child(
                Button::new("create-token")
                    .ghost()
                    .small()
                    .icon(IconName::ExternalLink)
                    .label("Create a token in the Cloudflare dashboard")
                    .on_click(|_, _, cx| cx.open_url(CREATE_TOKEN_URL)),
            )
    }

    fn render_account_step(
        &self,
        accounts: &[Account],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(div().text_xl().font_semibold().child("Choose an account"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("You can switch later from the app menu."),
            )
            .child(
                v_flex()
                    .gap_1()
                    .children(accounts.iter().enumerate().map(|(ix, account)| {
                        let picked = account.clone();
                        Button::new(("account", ix))
                            .outline()
                            .w_full()
                            .justify_start()
                            .icon(IconName::Building2)
                            .label(format!("{}  ·  {}", account.name, short_id(&account.id)))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.pick(picked.clone(), cx)),
                            )
                    })),
            )
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect::<String>() + "…"
}

impl Focusable for SetupView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.step {
            Step::EnterToken => self.render_token_step(cx).into_any_element(),
            Step::PickAccount { accounts, .. } => {
                let accounts = accounts.clone();
                self.render_account_step(&accounts, cx).into_any_element()
            }
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(560.))
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(body),
            )
    }
}
