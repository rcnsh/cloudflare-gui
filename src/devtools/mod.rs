//! Scripted UI driving and frame capture for development, compiled only with
//! `--features devtools`. Screenshots come straight from the Metal renderer, so
//! they work without macOS screen-recording permission.
//!
//! `CFGUI_SCRIPT` is a `;`-separated list of steps:
//!   `wait 1.5`   pause, in seconds
//!   `key cmd-k`  press a keystroke
//!   `type users` type text into whatever has focus
//!   `click 640 300` left-click at window coordinates (logical pixels)
//!   `paste a\nb`  paste text (`\n` for newlines) through the clipboard,
//!                then put the previous clipboard back
//!   `shot a.png` save the current frame as PNG
//!   `quit`
//!
//! With `CFGUI_FAKE_TAIL=1`, Worker tail tabs connect to a local WebSocket
//! that emits synthetic events instead of creating a tail on the account.

pub mod demo;

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
                "click" => click(window, cx, &arg),
                "paste" => {
                    let text = arg.replace("\\n", "\n");
                    let previous = cx.update(|cx| {
                        let previous = cx.read_from_clipboard();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        previous
                    });
                    press(window, cx, "cmd-v");
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if let Some(previous) = previous {
                        cx.update(|cx| cx.write_to_clipboard(previous));
                    }
                }
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

fn click(window: AnyWindowHandle, cx: &mut AsyncApp, arg: &str) {
    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, point,
        px,
    };
    let mut coords = arg.split_whitespace().filter_map(|n| n.parse::<f32>().ok());
    let (Some(x), Some(y)) = (coords.next(), coords.next()) else {
        log::warn!("bad click {arg:?}, expected `click x y`");
        return;
    };
    let position = point(px(x), px(y));
    let _ = window.update(cx, |_, window, cx| {
        let modifiers = Modifiers::default();
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
            }),
            cx,
        );
    });
}

fn press(window: AnyWindowHandle, cx: &mut AsyncApp, keys: &str) {
    match Keystroke::parse(keys) {
        Ok(keystroke) => {
            let _ = window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
        }
        Err(e) => log::warn!("bad keystroke {keys:?}: {e}"),
    }
}

/// With `CFGUI_FAKE_TAIL` set, tail tabs skip the API and connect straight to
/// the fake tail server.
pub fn fake_tail_url() -> Option<String> {
    std::env::var_os("CFGUI_FAKE_TAIL")?;
    fake_tail_server()
}

/// The URL of a local fake tail server, started on first use.
pub fn fake_tail_server() -> Option<String> {
    static URL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    URL.get_or_init(|| {
        // Bound with std so this works from any thread, including the demo
        // API's, where blocking on the runtime would panic.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.set_nonblocking(true).map(|()| l))
            .map_err(|e| log::error!("fake tail server: {e}"))
            .ok()?;
        let url = format!("ws://{}", listener.local_addr().ok()?);
        drop(crate::runtime::spawn(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => serve_fake_tail(listener).await,
                Err(e) => log::error!("fake tail server: {e}"),
            }
        }));
        log::info!("fake tail server at {url}");
        Some(url)
    })
    .clone()
}

// The handshake callback's error type is tungstenite's, not ours.
#[allow(clippy::result_large_err)]
async fn serve_fake_tail(listener: tokio::net::TcpListener) {
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::{Message, handshake::server, http::HeaderValue};

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            let echo = |_: &server::Request, mut response: server::Response| {
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    HeaderValue::from_static("trace-v1"),
                );
                Ok(response)
            };
            let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, echo).await else {
                return;
            };
            let _hello = socket.next().await;
            for n in 0u64.. {
                let event = fake_event(n);
                if socket.send(Message::text(event.to_string())).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(if n < 30 { 40 } else { 900 })).await;
            }
        });
    }
}

fn fake_event(n: u64) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp_millis();
    let path = [
        "/api/items",
        "/api/users/42",
        "/",
        "/assets/app.js",
        "/api/search?q=tea",
    ][n as usize % 5];
    let mut logs = vec![serde_json::json!({
        "message": [format!("handled {path}"), {"ms": 3 + n % 17}],
        "level": "log",
        "timestamp": now,
    })];
    if n % 4 == 1 {
        logs.push(serde_json::json!({
            "message": [format!("cache miss for items:{n}")],
            "level": "warn",
            "timestamp": now,
        }));
    }
    if n % 6 == 3 {
        logs.push(serde_json::json!({ "message": ["query plan", {"rows": n}], "level": "debug", "timestamp": now }));
    }
    let failed = n % 9 == 7;
    serde_json::json!({
        "outcome": if failed { "exception" } else { "ok" },
        "scriptName": "storefront",
        "exceptions": if failed {
            serde_json::json!([{ "name": "TypeError", "message": "Cannot read properties of undefined (reading 'id')", "timestamp": now }])
        } else {
            serde_json::json!([])
        },
        "logs": logs,
        "eventTimestamp": now,
        "event": {
            "request": { "url": format!("https://shop.example.com{path}"), "method": if n % 7 == 2 { "POST" } else { "GET" }, "headers": {} },
            "response": { "status": if failed { 500 } else { 200 } }
        }
    })
}
