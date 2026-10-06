//! Read-only preview of a KV value or R2 object: pretty JSON, plain text,
//! images, or a hex dump for anything else.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Entity, Image, ImageFormat, ObjectFit, SharedString, Window, div, img};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Editor, EditorState};

/// Bigger text than this is truncated so the editor stays responsive.
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const HEX_PREVIEW_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Empty,
    Json,
    Text,
    Image,
    Binary,
}

enum Content {
    /// Nothing selected yet.
    None,
    Empty,
    Editor,
    Image(Arc<Image>),
    Binary(SharedString),
}

pub struct ValueViewer {
    editor: Entity<EditorState>,
    content: Content,
    pub kind: Kind,
    pub truncated: bool,
}

impl ValueViewer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .folding(true)
                .soft_wrap(true);
            state.set_readonly(true, cx);
            state
        });
        Self {
            editor,
            content: Content::None,
            kind: Kind::Empty,
            truncated: false,
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = Content::None;
        self.kind = Kind::Empty;
        cx.notify();
    }

    /// Shows `bytes`, using `content_type` (from R2 metadata) when available.
    pub fn show(
        &mut self,
        bytes: &[u8],
        content_type: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.truncated = false;
        if let Some(format) = content_type.and_then(ImageFormat::from_mime_type) {
            self.kind = Kind::Image;
            self.content = Content::Image(Arc::new(Image::from_bytes(format, bytes.to_vec())));
            cx.notify();
            return;
        }
        let (kind, text) = classify(bytes);
        self.kind = kind;
        match (kind, text) {
            (Kind::Json | Kind::Text, Some(mut text)) => {
                if text.len() > MAX_TEXT_BYTES {
                    let mut cut = MAX_TEXT_BYTES;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    self.truncated = true;
                }
                let language = if kind == Kind::Json { "json" } else { "text" };
                self.editor.update(cx, |e, cx| {
                    e.set_highlighter(language, cx);
                    e.set_value(text, window, cx);
                });
                self.content = Content::Editor;
            }
            (Kind::Empty, _) => self.content = Content::Empty,
            _ => self.content = Content::Binary(hex_dump(bytes).into()),
        }
        cx.notify();
    }
}

/// Pretty-prints JSON when the bytes parse as JSON; otherwise reports text
/// or binary.
pub fn classify(bytes: &[u8]) -> (Kind, Option<String>) {
    if bytes.is_empty() {
        return (Kind::Empty, None);
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (Kind::Binary, None);
    };
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && let Ok(pretty) = serde_json::to_string_pretty(&value)
    {
        return (Kind::Json, Some(pretty));
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return (Kind::Binary, None);
    }
    (Kind::Text, Some(text.to_string()))
}

pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (row, chunk) in bytes.chunks(16).take(HEX_PREVIEW_BYTES / 16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "{:08x}  {:<48}  {ascii}\n",
            row * 16,
            hex.join(" ")
        ));
    }
    if bytes.len() > HEX_PREVIEW_BYTES {
        out.push_str(&format!(
            "… {} more bytes\n",
            bytes.len() - HEX_PREVIEW_BYTES
        ));
    }
    out
}

impl Render for ValueViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        match &self.content {
            Content::None => div().size_full().into_any_element(),
            Content::Empty => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("Empty value")
                .into_any_element(),
            Content::Editor => div()
                .size_full()
                .child(
                    Editor::new(&self.editor)
                        .readonly(true)
                        .bordered(false)
                        .size_full(),
                )
                .into_any_element(),
            Content::Image(image) => div()
                .size_full()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(image.clone())
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Content::Binary(dump) => div()
                .id("hex")
                .size_full()
                .overflow_y_scroll()
                .p_3()
                .text_sm()
                .font_family(cx.theme().mono_font_family.clone())
                .whitespace_nowrap()
                .child(div().text_color(muted).mb_2().child("Binary value"))
                .children(dump.lines().map(|l| div().child(l.to_string())))
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_pretty_prints_json() {
        let (kind, text) = classify(br#"{"a":1,"b":[true,null]}"#);
        assert_eq!(kind, Kind::Json);
        assert_eq!(
            text.unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null\n  ]\n}"
        );
    }

    #[test]
    fn plain_text_and_json_lookalikes_are_text() {
        assert_eq!(classify(b"hello\nworld").0, Kind::Text);
        assert_eq!(classify(b"{not json").0, Kind::Text);
        assert_eq!(classify(b"42").0, Kind::Text);
    }

    #[test]
    fn binary_and_empty() {
        assert_eq!(classify(&[0xff, 0xfe, 0x00]).0, Kind::Binary);
        assert_eq!(classify(b"a\x00b").0, Kind::Binary);
        assert_eq!(classify(b"").0, Kind::Empty);
    }

    #[test]
    fn hex_dump_formats_rows_and_truncates() {
        let dump = hex_dump(b"ABC\x00");
        assert_eq!(dump, format!("00000000  {:<48}  ABC.\n", "41 42 43 00"));
        let big = vec![0u8; HEX_PREVIEW_BYTES + 10];
        assert!(hex_dump(&big).ends_with("… 10 more bytes\n"));
    }
}
