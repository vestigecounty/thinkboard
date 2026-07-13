//! System clipboard reads for paste (text and images).
//!
//! iced's own clipboard only handles text, so images use `arboard`, which
//! wraps the platform clipboard (NSPasteboard on macOS). Reads are one-shot:
//! a fresh `Clipboard` is opened per paste rather than held, since the app
//! only touches it on an explicit paste.

use serde::{Deserialize, Serialize};

use crate::model::{CanvasEdge, CanvasNode};

/// Private marker key that wraps copied canvas nodes on the clipboard, so a
/// paste can tell "these are ThinkBoard nodes" from arbitrary copied text.
const CANVAS_CLIP_KEY: &str = "thinkboard/canvas";

/// Copied nodes plus the edges among them, round-tripped through the system
/// clipboard as JSON (so a copy survives a paste elsewhere, or a restart).
#[derive(Serialize, Deserialize)]
pub struct NodeClip {
    pub nodes: Vec<CanvasNode>,
    pub edges: Vec<CanvasEdge>,
}

/// Writes copied nodes to the clipboard, wrapped under [`CANVAS_CLIP_KEY`].
pub fn write_nodes(clip: &NodeClip) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(encode_nodes(clip));
    }
}

/// Reads copied canvas nodes, or `None` if the clipboard doesn't hold our
/// marker (so a normal text/image paste can take over).
pub fn read_nodes() -> Option<NodeClip> {
    decode_nodes(&read_text()?)
}

/// Serializes a [`NodeClip`] to its clipboard JSON string.
fn encode_nodes(clip: &NodeClip) -> String {
    let inner = serde_json::to_value(clip).unwrap_or(serde_json::Value::Null);
    let mut wrapper = serde_json::Map::new();
    wrapper.insert(CANVAS_CLIP_KEY.to_owned(), inner);
    serde_json::Value::Object(wrapper).to_string()
}

/// Parses a [`NodeClip`] from clipboard text, or `None` if it isn't our format.
fn decode_nodes(text: &str) -> Option<NodeClip> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(value.get(CANVAS_CLIP_KEY)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CanvasEdge, CanvasNode};

    fn node(id: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some("hi".to_owned()),
            file: None,
            x: 1.0,
            y: 2.0,
            width: 250.0,
            height: 60.0,
            color: Some("3".to_owned()),
            extra: Default::default(),
        }
    }

    #[test]
    fn nodes_round_trip_through_the_clipboard_format() {
        let clip = NodeClip {
            nodes: vec![node("a"), node("b")],
            edges: vec![CanvasEdge {
                id: "e".to_owned(),
                from_node: "a".to_owned(),
                from_side: "right".to_owned(),
                to_node: "b".to_owned(),
                to_side: "left".to_owned(),
                extra: Default::default(),
            }],
        };
        let decoded = decode_nodes(&encode_nodes(&clip)).unwrap();
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].id, "a");
        assert_eq!(decoded.nodes[0].color.as_deref(), Some("3"));
        assert_eq!(decoded.edges.len(), 1);
        assert_eq!(decoded.edges[0].from_node, "a");
    }

    #[test]
    fn plain_text_is_not_mistaken_for_copied_nodes() {
        assert!(decode_nodes("just some copied text").is_none());
        // Valid JSON but without our marker key.
        assert!(decode_nodes(r#"{"nodes":[]}"#).is_none());
    }
}

/// What the clipboard currently holds, as far as we can paste it. Images take
/// priority over text: a screenshot on the clipboard is offered as an image
/// even though some sources also expose a textual fallback.
pub enum Clip {
    Text(String),
    /// Raw RGBA8 pixels, row-major, `width * height * 4` bytes.
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
}

/// The clipboard's text, if any. Cheap (unlike an image read), so it's fine
/// to call on menu open to look for a pasteable color value.
pub fn read_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Reads the clipboard, or `None` if it's empty, unreadable, or holds nothing
/// we can turn into a node.
pub fn read() -> Option<Clip> {
    let mut clipboard = arboard::Clipboard::new().ok()?;

    if let Ok(image) = clipboard.get_image() {
        if image.width > 0 && image.height > 0 {
            return Some(Clip::Image {
                width: image.width,
                height: image.height,
                rgba: image.bytes.into_owned(),
            });
        }
    }

    match clipboard.get_text() {
        Ok(text) if !text.trim().is_empty() => Some(Clip::Text(text)),
        _ => None,
    }
}
