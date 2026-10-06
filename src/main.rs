// Client methods for the D1, KV, R2 and tail views land slice by slice; drop
// this once every view uses them.
#![allow(dead_code)]

mod actions;
mod cloudflare;
#[cfg(feature = "devtools")]
mod devtools;
mod keychain;
mod runtime;
mod sql;
mod state;
mod views;

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, TitleBar};

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("cloudflare_gui=info,warn"),
    )
    .init();

    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(|cx: &mut App| {
            // Must run before any window opens, or the window gets no dialog or
            // notification layer.
            gpui_component::init(cx);
            Theme::change(cx.window_appearance(), None, cx);

            actions::bind_keys(cx);
            views::sidebar::bind_keys(cx);
            actions::set_menus(cx);
            cx.on_action(|_: &actions::Quit, cx: &mut App| cx.quit());
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                ..TitleBar::window_options()
            };
            let window = cx
                .open_window(options, |window, cx| {
                    let view = cx.new(|cx| views::app::AppView::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open the main window");
            #[cfg(feature = "devtools")]
            devtools::run_script(window.into(), cx);
            #[cfg(not(feature = "devtools"))]
            let _ = window;
            cx.activate(true);
        });
}
