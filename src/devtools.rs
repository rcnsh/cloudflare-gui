//! Scripted UI driving and frame capture for development, compiled only with
//! `--features devtools`. Screenshots come straight from the Metal renderer, so
//! they work without macOS screen-recording permission.
//!
//! `CFGUI_SCRIPT` is a `;`-separated list of steps:
//!   `wait 1.5`   pause, in seconds
//!   `key cmd-k`  press a keystroke
//!   `type users` type text into whatever has focus
//!   `shot a.png` save the current frame as PNG
//!   `quit`

use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, Keystroke};

pub fn run_script(window: AnyWindowHandle, cx: &mut App) {
    let Ok(script) = std::env::var("CFGUI_SCRIPT") else {
        return;
    };
    let steps: Vec<String> = script
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    cx.spawn(async move |cx: &mut AsyncApp| {
        for step in steps {
            let (verb, arg) = step.split_once(' ').unwrap_or((step.as_str(), ""));
            let arg = arg.trim().to_string();
            match verb {
                "wait" => {
                    let secs: f64 = arg.parse().unwrap_or(1.0);
                    cx.background_executor()
                        .timer(Duration::from_secs_f64(secs))
                        .await;
                }
                "key" => press(window, cx, &arg),
                "type" => {
                    for ch in arg.chars() {
                        let key = if ch == ' ' {
                            "space".to_string()
                        } else {
                            ch.to_string()
                        };
                        press(window, cx, &key);
                    }
                }
                "shot" => {
                    // A background window gets no display-link frames, so draw one now
                    // instead of capturing whatever was last presented.
                    let result = window.update(cx, |_, window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                        window.render_to_image()
                    });
                    match result {
                        Ok(Ok(image)) => match image.save(&arg) {
                            Ok(()) => log::info!("saved screenshot {arg}"),
                            Err(e) => log::error!("couldn't save {arg}: {e}"),
                        },
                        Ok(Err(e)) | Err(e) => log::error!("couldn't capture frame: {e}"),
                    }
                }
                "quit" => cx.update(|cx| cx.quit()),
                other => log::warn!("unknown script step {other:?}"),
            }
            // Let the UI settle between steps.
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
        }
    })
    .detach();
}

fn press(window: AnyWindowHandle, cx: &mut AsyncApp, keys: &str) {
    match Keystroke::parse(keys) {
        Ok(keystroke) => {
            let _ = window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
        }
        Err(e) => log::warn!("bad keystroke {keys:?}: {e}"),
    }
}
