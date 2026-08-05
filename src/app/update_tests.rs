//! Tests that drive [`App::update`] directly.
//!
//! The pure helpers around it are unit-tested next to themselves; this covers
//! the parts that only exist as state transitions — what a message does to the
//! selection, when an action becomes an undo step, and the editing lifecycle.
//!
//! Every app here is built as a *scratch* board, which suppresses all disk
//! writes, and as *windowed*, which starts it in [`Phase::Running`] rather than
//! playing the overlay entrance. Follow-up `Task`s are dropped: they carry
//! rasterization and reclustering, which the assertions don't depend on.

use super::*;

/// An app holding `nodes` and `edges`, as if a board were already open.
fn app_with(nodes: Vec<CanvasNode>, edges: Vec<CanvasEdge>) -> App {
    App::build(Init {
        config: config::Config::default(),
        theme: ColorTheme::default(),
        document: CanvasDocument {
            nodes,
            edges,
            extra: serde_json::Map::new(),
        },
        canvas_path: PathBuf::new(),
        base_path: PathBuf::from("."),
        file_mtime: None,
        scratch: true,
        hotkey_manager: None,
        registered_hotkey: None,
        windowed: true,
    })
}

/// Delivers one message, dropping the follow-up task.
fn send(app: &mut App, message: Message) {
    let _ = app.update(message);
}

/// Types into the open editor along the real path a keystroke takes.
fn type_text(app: &mut App, text: &str) {
    send(
        app,
        Message::EditorAction(text_editor::Action::Edit(text_editor::Edit::Paste(
            std::sync::Arc::new(text.to_owned()),
        ))),
    );
}

fn text_node(id: &str, text: &str) -> CanvasNode {
    CanvasNode {
        id: id.to_owned(),
        kind: "text".to_owned(),
        text: Some(text.to_owned()),
        file: None,
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 100.0,
        color: None,
        extra: Default::default(),
    }
}

/// Two text nodes, 400 apart so neither constrains the other's auto-size.
fn two_nodes() -> Vec<CanvasNode> {
    let mut b = text_node("b", "node b");
    b.x = 400.0;
    vec![text_node("a", "node a"), b]
}

fn edge(from: &str, to: &str) -> CanvasEdge {
    CanvasEdge {
        id: format!("{from}->{to}"),
        from_node: from.to_owned(),
        from_side: "right".to_owned(),
        to_node: to.to_owned(),
        to_side: "left".to_owned(),
        extra: Default::default(),
    }
}

fn text_of(app: &App, index: usize) -> Option<&str> {
    app.nodes[index].text.as_deref()
}

// --- Selection ---

#[test]
fn selecting_a_node_replaces_the_selection() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    assert_eq!(app.selected, vec![0]);

    send(&mut app, Message::SelectNode(Some(1)));
    assert_eq!(app.selected, vec![1]);

    send(&mut app, Message::SelectNode(None));
    assert!(app.selected.is_empty());
}

#[test]
fn toggling_adds_then_removes_a_node() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::ToggleNode(0));
    send(&mut app, Message::ToggleNode(1));
    assert_eq!(app.selected, vec![0, 1]);

    send(&mut app, Message::ToggleNode(0));
    assert_eq!(app.selected, vec![1]);
}

#[test]
fn node_and_edge_selection_are_mutually_exclusive() {
    let mut app = app_with(two_nodes(), vec![edge("a", "b")]);

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SelectEdge(Some(0)));
    assert!(app.selected.is_empty());
    assert_eq!(app.selected_edge, Some(0));

    send(&mut app, Message::SelectNode(Some(1)));
    assert_eq!(app.selected, vec![1]);
    assert_eq!(app.selected_edge, None);
}

#[test]
fn a_marquee_result_replaces_the_whole_selection() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SelectNodes(vec![0, 1]));
    assert_eq!(app.selected, vec![0, 1]);
}

// --- Undo boundaries ---

#[test]
fn deleting_a_node_is_undoable_and_redoable() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::DeleteSelected);
    assert_eq!(app.nodes.len(), 1);
    assert_eq!(app.nodes[0].id, "b");

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes.len(), 2);

    send(&mut app, Message::Redo);
    assert_eq!(app.nodes.len(), 1);
    assert_eq!(app.nodes[0].id, "b");
}

#[test]
fn deleting_a_node_drops_the_edges_touching_it() {
    let mut app = app_with(two_nodes(), vec![edge("a", "b")]);

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::DeleteSelected);
    assert!(app.edges.is_empty());

    // The link comes back with the node it hung off.
    send(&mut app, Message::Undo);
    assert_eq!(app.edges.len(), 1);
}

#[test]
fn deleting_a_link_is_undoable() {
    let mut app = app_with(two_nodes(), vec![edge("a", "b")]);

    send(&mut app, Message::SelectEdge(Some(0)));
    send(&mut app, Message::DeleteSelected);
    assert!(app.edges.is_empty());
    assert_eq!(app.nodes.len(), 2, "deleting a link keeps its endpoints");

    send(&mut app, Message::Undo);
    assert_eq!(app.edges.len(), 1);
}

#[test]
fn connecting_two_nodes_is_undoable() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(
        &mut app,
        Message::AddEdge {
            from: 0,
            side: "right",
            to: 1,
            to_side: "left",
        },
    );
    assert_eq!(app.edges.len(), 1);

    send(&mut app, Message::Undo);
    assert!(app.edges.is_empty());
}

#[test]
fn dragging_a_node_records_no_undo_step() {
    // Drags are deliberately not undoable — they'd flood the history with a
    // step per mouse-move.
    let mut app = app_with(two_nodes(), Vec::new());

    send(
        &mut app,
        Message::MoveNode {
            index: 0,
            position: Point::new(500.0, 500.0),
        },
    );
    assert_eq!(app.nodes[0].x, 500.0);
    assert!(!app.history.can_undo());
}

#[test]
fn recoloring_records_a_step_only_when_the_color_changes() {
    let mut app = app_with(two_nodes(), Vec::new());

    // The node is already uncolored, so clearing its color changes nothing.
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SetNodeColor(None));
    assert!(!app.history.can_undo());

    // Picking a color does — and applying the *same* one again does not.
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SetNodeColor(Some("4".to_owned())));
    assert_eq!(app.nodes[0].color.as_deref(), Some("4"));
    assert!(app.history.can_undo());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SetNodeColor(Some("4".to_owned())));
    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[0].color, None, "one step, not two");
}

#[test]
fn undo_on_a_fresh_board_is_a_no_op() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::Undo);
    send(&mut app, Message::Redo);
    assert_eq!(app.nodes.len(), 2);
}

#[test]
fn a_new_action_discards_the_redo_future() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::DeleteSelected);
    send(&mut app, Message::Undo);
    assert!(app.history.can_redo());

    // Deleting again diverges from the undone future, which is discarded.
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::DeleteSelected);
    assert!(!app.history.can_redo());
}

// --- Editing lifecycle ---

#[test]
fn only_text_nodes_open_an_editor() {
    let mut file = text_node("img", "");
    file.kind = "file".to_owned();
    file.text = None;
    file.file = Some("pic.png".to_owned());
    let mut app = app_with(vec![file], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    assert!(app.editing.is_none());
}

#[test]
fn ending_an_edit_trims_the_text_and_closes_the_editor() {
    let mut app = app_with(vec![text_node("a", "")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    assert!(app.editing.is_some());

    type_text(&mut app, "  hello  ");
    send(&mut app, Message::StopEditing);

    assert_eq!(text_of(&app, 0), Some("hello"));
    assert!(app.editing.is_none());
}

#[test]
fn an_edit_that_changes_the_text_is_one_undo_step() {
    let mut app = app_with(vec![text_node("a", "same")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "!");
    send(&mut app, Message::StopEditing);
    assert_ne!(text_of(&app, 0), Some("same"));

    send(&mut app, Message::Undo);
    assert_eq!(text_of(&app, 0), Some("same"));
}

#[test]
fn an_edit_that_changes_nothing_records_no_undo_step() {
    let mut app = app_with(vec![text_node("a", "same")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::StopEditing);

    assert_eq!(text_of(&app, 0), Some("same"));
    assert!(!app.history.can_undo());
}

#[test]
fn a_freshly_created_node_left_empty_disappears() {
    let mut app = app_with(Vec::new(), Vec::new());

    send(&mut app, Message::CreateNode { world: Point::ORIGIN });
    assert_eq!(app.nodes.len(), 1);
    assert_eq!(app.fresh_node, Some(0));

    // The spawn animation normally gates the editor; open it the way the
    // finished animation does.
    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::StopEditing);

    assert!(app.nodes.is_empty(), "an untyped new node is taken back");
    assert_eq!(app.fresh_node, None);
}

#[test]
fn a_freshly_created_node_with_text_stays() {
    let mut app = app_with(Vec::new(), Vec::new());

    send(&mut app, Message::CreateNode { world: Point::ORIGIN });
    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "kept");
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes.len(), 1);
    assert_eq!(text_of(&app, 0), Some("kept"));
}

#[test]
fn deleting_is_ignored_while_the_editor_is_open() {
    // Backspace belongs to the text while editing, not to the node.
    let mut app = app_with(vec![text_node("a", "text")], Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::DeleteSelected);

    assert_eq!(app.nodes.len(), 1);
}

#[test]
fn typing_resizes_the_card_to_fit() {
    let mut app = app_with(vec![text_node("a", "")], Vec::new());
    let height = app.nodes[0].height;

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight");

    assert!(
        app.nodes[0].height > height,
        "the card should grow to fit the text it now holds"
    );
}

#[test]
fn undo_leaves_no_stale_selection_behind() {
    // Restoring swaps the whole node list, so indices from before the undo
    // would no longer mean the same nodes.
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::DeleteSelected);
    send(&mut app, Message::Undo);

    assert!(app.selected.is_empty());
    assert_eq!(app.selected_edge, None);
}
