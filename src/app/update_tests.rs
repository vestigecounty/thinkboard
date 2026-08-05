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
pub(super) fn app_with(nodes: Vec<CanvasNode>, edges: Vec<CanvasEdge>) -> App {
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

// --- Obsidian node kinds ---

fn node_of(id: &str, kind: &str) -> CanvasNode {
    CanvasNode {
        id: id.to_owned(),
        kind: kind.to_owned(),
        text: None,
        file: None,
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
        color: None,
        extra: Default::default(),
    }
}

#[test]
fn a_group_frame_never_blocks_placement_inside_it() {
    // A frame is drawn around nodes, so a new node has to be allowed to land
    // inside one. Treating it as occupied space flung every new node clear of
    // the whole group.
    let mut app = app_with(vec![node_of("frame", "group")], Vec::new());

    let requested = Point::new(400.0, 300.0); // dead centre of the frame
    assert_eq!(app.find_free_placement(requested, 200.0, 100.0), requested);

    // And creating one there really does leave it there.
    send(&mut app, Message::CreateNode { world: requested });
    let created = &app.nodes[1];
    assert!(
        created.x > 0.0 && created.y > 0.0 && created.x < 800.0,
        "a new node should sit inside the frame, got ({}, {})",
        created.x,
        created.y
    );
}

#[test]
fn a_card_inside_a_frame_still_widens_to_fit() {
    // The frame straddles the card's centre, so counted as a horizontal
    // neighbour it pins the card at its current width — the card can then only
    // ever grow downwards, however much room the frame actually leaves.
    let mut inner = text_node("a", "");
    inner.x = 100.0;
    inner.y = 100.0;
    inner.width = 250.0; // the configured minimum, so growth is unambiguous
    let mut app = app_with(vec![node_of("frame", "group"), inner], Vec::new());

    send(&mut app, Message::StartEditing { index: 1, at: None });
    // One long line with nowhere to wrap: fitting it means widening.
    type_text(&mut app, "one long unbroken line of text that has to widen the card");

    assert!(
        app.nodes[1].width > 250.0,
        "the card should have widened to fit; got {}",
        app.nodes[1].width
    );
}

#[test]
fn frames_take_no_part_in_clustering() {
    let mut a = text_node("a", "node a");
    a.x = 0.0;
    let mut b = text_node("b", "node b");
    b.x = 2000.0;
    let app = app_with(vec![node_of("frame", "group"), a, b], Vec::new());

    // Two distant notes stay two clusters, and the frame joins neither.
    assert_eq!(app.clusters.list.len(), 2);
    assert!(app.clusters.list.iter().all(|c| !c.members.contains(&0)));
}

#[test]
fn dragging_a_frame_carries_what_it_encloses() {
    // A frame is a container: leaving its contents behind would empty it.
    let mut inside = text_node("in", "inside");
    inside.x = 100.0;
    inside.y = 100.0;
    let mut outside = text_node("out", "outside");
    outside.x = 2000.0;
    outside.y = 2000.0;
    let mut app = app_with(vec![node_of("frame", "group"), inside, outside], Vec::new());

    send(
        &mut app,
        Message::MoveNode {
            index: 0,
            position: Point::new(50.0, 30.0), // the frame starts at (0, 0)
        },
    );

    assert_eq!((app.nodes[0].x, app.nodes[0].y), (50.0, 30.0), "the frame");
    assert_eq!((app.nodes[1].x, app.nodes[1].y), (150.0, 130.0), "rides along");
    assert_eq!(
        (app.nodes[2].x, app.nodes[2].y),
        (2000.0, 2000.0),
        "a node outside the frame stays put"
    );
}

#[test]
fn dragging_a_card_carries_nothing() {
    let mut a = text_node("a", "a");
    a.x = 100.0;
    let mut b = text_node("b", "b");
    b.x = 200.0;
    let mut app = app_with(vec![a, b], Vec::new());

    send(
        &mut app,
        Message::MoveNode {
            index: 0,
            position: Point::new(500.0, 500.0),
        },
    );
    assert_eq!((app.nodes[0].x, app.nodes[0].y), (500.0, 500.0));
    assert_eq!((app.nodes[1].x, app.nodes[1].y), (200.0, 0.0));
}

#[test]
fn a_frames_contents_move_exactly_one_delta() {
    // The enclosed node is *also* in the selection being dragged. Counted
    // twice it would slide at double speed away from its frame.
    let mut inside = text_node("in", "inside");
    inside.x = 100.0;
    inside.y = 100.0;
    let mut app = app_with(vec![node_of("frame", "group"), inside], Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(
        &mut app,
        Message::MoveNode {
            index: 0,
            position: Point::new(10.0, 20.0),
        },
    );

    assert_eq!((app.nodes[1].x, app.nodes[1].y), (110.0, 120.0));
}

#[test]
fn a_nested_frame_and_its_contents_ride_along() {
    // Outer frame (800x600 at origin) holds an inner frame, which holds a card.
    let mut inner = node_of("inner", "group");
    inner.x = 50.0;
    inner.y = 50.0;
    inner.width = 300.0;
    inner.height = 200.0;
    let mut card = text_node("card", "card");
    card.x = 100.0;
    card.y = 100.0;
    let mut app = app_with(vec![node_of("outer", "group"), inner, card], Vec::new());

    send(
        &mut app,
        Message::MoveNode {
            index: 0,
            position: Point::new(10.0, 10.0),
        },
    );

    assert_eq!((app.nodes[1].x, app.nodes[1].y), (60.0, 60.0), "inner frame");
    assert_eq!((app.nodes[2].x, app.nodes[2].y), (110.0, 110.0), "its card");
}
