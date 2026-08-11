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

// --- Grouping ---

#[test]
fn grouping_wraps_the_selection_in_a_frame() {
    let mut a = text_node("a", "a");
    a.x = 100.0;
    a.y = 100.0;
    let mut b = text_node("b", "b");
    b.x = 500.0;
    b.y = 300.0; // 200x100 cards, so the pair spans (100,100)-(700,400)
    let mut app = app_with(vec![a, b], Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(&mut app, Message::GroupSelection);

    assert_eq!(app.nodes.len(), 3);
    let frame = &app.nodes[2];
    assert!(frame.is_group());
    // It encloses both members, with room to spare.
    assert!(frame.x < 100.0 && frame.y < 100.0);
    assert!(frame.x + frame.width > 700.0);
    assert!(frame.y + frame.height > 400.0);
    // And is left selected, so it can be dragged straight away.
    assert_eq!(app.selected, vec![2]);
}

#[test]
fn a_new_frame_leaves_room_for_its_caption() {
    // The caption sits inside the frame's top edge, so the top inset has to
    // clear it or the label lands on top of the highest node.
    let mut a = text_node("a", "a");
    a.y = 200.0;
    let mut b = text_node("b", "b");
    b.x = 400.0;
    b.y = 200.0;
    let mut app = app_with(vec![a, b], Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(&mut app, Message::GroupSelection);

    let frame = &app.nodes[2];
    let top_gap = 200.0 - frame.y;
    let side_gap = 0.0 - frame.x;
    assert!(
        top_gap > side_gap,
        "top inset {top_gap} should exceed the side inset {side_gap}"
    );
}

#[test]
fn grouping_needs_more_than_one_node() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNode(Some(0)));
    assert!(!app.groupable_selection());
    send(&mut app, Message::GroupSelection);
    assert_eq!(app.nodes.len(), 2, "a lone node makes no frame");

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    assert!(app.groupable_selection());
}

#[test]
fn frames_do_not_count_toward_grouping() {
    // Selecting a frame plus one node isn't two groupable nodes.
    let mut app = app_with(vec![node_of("frame", "group"), text_node("a", "a")], Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    assert!(!app.groupable_selection());
    send(&mut app, Message::GroupSelection);
    assert_eq!(app.nodes.len(), 2);
}

#[test]
fn grouping_is_undoable() {
    let mut app = app_with(two_nodes(), Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(&mut app, Message::GroupSelection);
    assert_eq!(app.nodes.len(), 3);

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes.len(), 2);
    assert!(app.nodes.iter().all(|n| !n.is_group()));
}

#[test]
fn a_new_frame_immediately_carries_its_members() {
    // Grouping then dragging is the whole point, so the frame the action
    // creates has to enclose its members by the same test `drag_riders` uses.
    let mut a = text_node("a", "a");
    a.x = 100.0;
    a.y = 100.0;
    let mut b = text_node("b", "b");
    b.x = 400.0;
    b.y = 100.0;
    let mut app = app_with(vec![a, b], Vec::new());

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(&mut app, Message::GroupSelection);

    let (ax, ay) = (app.nodes[0].x, app.nodes[0].y);
    let target = Point::new(app.nodes[2].x + 50.0, app.nodes[2].y + 20.0);
    send(&mut app, Message::MoveNode { index: 2, position: target });
    assert_eq!((app.nodes[0].x, app.nodes[0].y), (ax + 50.0, ay + 20.0));
}

// --- Group labels ---

/// Creates a frame around two nodes and returns its index, leaving the caption
/// editor open the way the real flow does.
fn grouped(app: &mut App) -> usize {
    send(app, Message::SelectNodes(vec![0, 1]));
    send(app, Message::GroupSelection);
    // The action queues `StartEditing`; the harness drops tasks, so deliver it.
    let index = app.nodes.len() - 1;
    send(app, Message::StartEditing { index, at: None });
    index
}

#[test]
fn creating_a_group_opens_its_caption_for_naming() {
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);

    assert!(app.editing.is_some(), "the caption editor should be open");
    type_text(&mut app, "Planning");
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes[frame].label(), Some("Planning"));
}

#[test]
fn a_caption_left_empty_leaves_the_frame_unnamed() {
    // Unlike a drag-created note, an unnamed frame is not taken back.
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);

    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes.len(), 3, "the frame survives");
    assert_eq!(app.nodes[frame].label(), None);
    // And stores nothing rather than an empty caption.
    assert!(!app.nodes[frame].extra.contains_key("label"));
}

#[test]
fn a_caption_of_only_spaces_clears_the_label() {
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    type_text(&mut app, "Planning");
    send(&mut app, Message::StopEditing);
    assert_eq!(app.nodes[frame].label(), Some("Planning"));

    // Re-open, replace with whitespace.
    send(&mut app, Message::StartEditing { index: frame, at: None });
    send(&mut app, Message::EditorAction(text_editor::Action::SelectAll));
    type_text(&mut app, "   ");
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes[frame].label(), None);
    assert!(!app.nodes[frame].extra.contains_key("label"));
}

#[test]
fn renaming_a_frame_is_one_undo_step() {
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    type_text(&mut app, "First");
    send(&mut app, Message::StopEditing);

    send(&mut app, Message::StartEditing { index: frame, at: None });
    send(&mut app, Message::EditorAction(text_editor::Action::SelectAll));
    type_text(&mut app, "Second");
    send(&mut app, Message::StopEditing);
    assert_eq!(app.nodes[frame].label(), Some("Second"));

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[frame].label(), Some("First"), "one step back");
}

#[test]
fn naming_a_new_frame_does_not_add_a_second_undo_step() {
    // Creating and naming is one action to the user; undo should remove the
    // frame, not leave a nameless one behind.
    let mut app = app_with(two_nodes(), Vec::new());
    grouped(&mut app);
    type_text(&mut app, "Planning");
    send(&mut app, Message::StopEditing);
    assert_eq!(app.nodes.len(), 3);

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes.len(), 2, "one undo removes the frame it named");
}

#[test]
fn a_frames_caption_does_not_resize_it() {
    // A body edit auto-sizes its card; a caption must leave the frame's
    // geometry alone, since the user placed it.
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    let (w, h) = (app.nodes[frame].width, app.nodes[frame].height);

    type_text(&mut app, "a considerably longer caption than the frame is wide");
    send(&mut app, Message::StopEditing);

    assert_eq!((app.nodes[frame].width, app.nodes[frame].height), (w, h));
}

#[test]
fn editing_a_body_still_writes_the_body() {
    // The target split must not leak: a text node still edits its text, and
    // gains no label.
    let mut app = app_with(vec![text_node("a", "")], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "body text");
    send(&mut app, Message::StopEditing);

    assert_eq!(text_of(&app, 0), Some("body text"));
    assert_eq!(app.nodes[0].label(), None);
}

#[test]
fn asking_a_group_builds_its_prompt_from_what_it_encloses() {
    // Ask AI on a frame used to be a no-op: a group has no incoming links, so
    // the chain-walking prompt came out empty and the request never fired.
    let mut a = text_node("a", "root note");
    a.x = 100.0;
    a.y = 100.0;
    let mut b = text_node("b", "child note");
    b.x = 100.0;
    b.y = 400.0;
    let mut app = app_with(vec![a, b], vec![edge("a", "b")]);

    send(&mut app, Message::SelectNodes(vec![0, 1]));
    send(&mut app, Message::GroupSelection);
    let frame = app.nodes.len() - 1;

    let prompt = thinkboard_core::ai::group_prompt(&app.nodes, &app.edges, frame);
    assert!(prompt.contains("root note"));
    assert!(prompt.contains("child note"));
    // The link inside is preserved as nesting, not flattened away.
    assert!(
        prompt.find("root note").unwrap() < prompt.find("child note").unwrap(),
        "parent should precede its child: {prompt:?}"
    );
    assert!(prompt.contains("  - child note"), "child indented: {prompt:?}");
}

#[test]
fn a_frames_caption_leads_its_prompt() {
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    type_text(&mut app, "Planning");
    send(&mut app, Message::StopEditing);

    let prompt = thinkboard_core::ai::group_prompt(&app.nodes, &app.edges, frame);
    assert!(prompt.starts_with("Planning\n"), "{prompt:?}");
}

/// A frame at the origin, ringed by nodes so every spot around it is taken and
/// only its interior looks free. The board shape that exposed the bug.
fn crowded_frame(app: &mut App) -> usize {
    let mut frame = node_of("frame", "group");
    frame.width = 800.0;
    frame.height = 600.0;
    app.nodes.push(frame);
    let mut n = 0;
    for gx in -3..=6 {
        for gy in -4..=8 {
            let (x, y) = (gx as f32 * 290.0, gy as f32 * 160.0);
            if x > -60.0 && x < 800.0 && y > -60.0 && y < 600.0 {
                continue; // leave the interior clear
            }
            let mut blocker = text_node(&format!("b{n}"), "x");
            n += 1;
            blocker.x = x;
            blocker.y = y;
            blocker.width = 250.0;
            blocker.height = 120.0;
            app.nodes.push(blocker);
        }
    }
    0
}

#[test]
fn an_answer_about_a_frame_never_lands_inside_it() {
    // Otherwise it becomes one of the frame's members and is fed back into the
    // next question asked about that frame.
    let mut app = app_with(Vec::new(), Vec::new());
    let frame = crowded_frame(&mut app);

    let placed = app.ai_answer_placement(frame, 250.0, 120.0);
    let rect = arrange::node_world_rect(&app.nodes[frame]);
    assert!(
        !rect.contains(placed),
        "answer placed at {placed:?}, inside the frame {rect:?}"
    );
}

#[test]
fn an_answer_about_a_note_still_prefers_the_space_beside_it() {
    // The frame exclusion is scoped to frames: a note's answer keeps landing
    // just to its right when that space is free.
    let mut a = text_node("a", "question");
    a.x = 0.0;
    a.y = 0.0;
    let app = app_with(vec![a], Vec::new());

    let placed = app.ai_answer_placement(0, 250.0, 120.0);
    assert!(placed.x > 200.0, "should sit to the right, got {placed:?}");
    assert!((placed.y - 50.0).abs() < 1.0, "and level with it, got {placed:?}");
}

#[test]
fn a_new_node_can_still_be_placed_inside_a_frame() {
    // The exclusion must not leak into ordinary placement — dropping a note
    // into a group is the normal way to add one to it.
    let mut app = app_with(Vec::new(), Vec::new());
    let mut frame = node_of("frame", "group");
    frame.width = 800.0;
    frame.height = 600.0;
    app.nodes.push(frame);

    let inside = Point::new(400.0, 300.0);
    assert_eq!(app.find_free_placement(inside, 250.0, 120.0), inside);
}

// --- Auto-size ---

#[test]
fn a_manual_resize_survives_editing() {
    // The reported bug: widen a card by hand, start typing, and it snapped back
    // to whatever the text measured — throwing the deliberate size away.
    let mut node = text_node("a", "short");
    node.width = 900.0;
    let mut app = app_with(vec![node], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, " a little more");
    send(&mut app, Message::StopEditing);

    assert!(
        app.nodes[0].width >= 900.0,
        "manual width lost: {}",
        app.nodes[0].width
    );
}

#[test]
fn a_manual_width_beyond_the_auto_ceiling_is_kept() {
    // `max_node_width` (420) bounds how wide a card grows by itself. It must
    // not claw back one dragged wider than that.
    let mut node = text_node("a", "short");
    node.width = 1200.0;
    let mut app = app_with(vec![node], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "x");
    send(&mut app, Message::StopEditing);

    assert!(app.nodes[0].width >= 1200.0, "got {}", app.nodes[0].width);
}

#[test]
fn auto_size_still_widens_a_card_that_needs_it() {
    // Never-shrink must not become never-change.
    let mut node = text_node("a", "");
    node.width = 250.0;
    let mut app = app_with(vec![node], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "one long unbroken line of text that has to widen the card");
    send(&mut app, Message::StopEditing);

    assert!(app.nodes[0].width > 250.0, "got {}", app.nodes[0].width);
}

#[test]
fn a_card_stops_growing_at_the_height_ceiling() {
    // Past it the raster loses resolution in proportion, so the card scrolls
    // instead of growing.
    let mut app = app_with(vec![text_node("a", "")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    // Far more text than any card could show.
    type_text(&mut app, &"a line of the note\n".repeat(400));
    send(&mut app, Message::StopEditing);

    let height = app.nodes[0].height;
    assert!(
        height <= crate::text_raster::MAX_AUTO_HEIGHT + 0.5,
        "grew to {height}, past the ceiling"
    );
    assert!(height > 500.0, "but it should still have grown: {height}");
}

#[test]
fn height_still_settles_back_when_text_is_removed() {
    // Height tracks content — otherwise deleting a long note leaves a tall
    // empty card behind. Only the width is treated as a user decision.
    let mut app = app_with(vec![text_node("a", "")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, &"line\n".repeat(30));
    send(&mut app, Message::StopEditing);
    let tall = app.nodes[0].height;
    assert!(tall > 200.0);

    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::EditorAction(text_editor::Action::SelectAll));
    type_text(&mut app, "short");
    send(&mut app, Message::StopEditing);

    assert!(app.nodes[0].height < tall, "height should settle back");
}

// --- Editing overlay bounds ---

/// The card's on-screen rectangle, which the editing overlay has to stay within.
fn card_screen_rect(app: &App, index: usize) -> iced::Rectangle {
    app.camera
        .rect_to_screen(arrange::node_world_rect(&app.nodes[index]))
}

#[test]
fn the_editor_never_extends_past_the_card_it_edits() {
    // A note with far more text than its card can show. The editor used to be
    // sized to the whole text, so it spilled out of the card — over the board
    // above and below it — instead of scrolling within it.
    let mut app = app_with(vec![text_node("a", &"a line of the note\n".repeat(200))], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });

    let card = card_screen_rect(&app, 0);
    let editor = app.editor_box(0).expect("an open editor");
    assert!(
        editor.height <= card.height + 0.5,
        "editor is {} tall for a {} card",
        editor.height,
        card.height
    );
    assert!(
        editor.origin.y >= card.y - 0.5,
        "editor starts at {}, above the card at {}",
        editor.origin.y,
        card.y
    );
    assert!(
        editor.origin.y + editor.height <= card.y + card.height + 0.5,
        "editor ends at {}, below the card at {}",
        editor.origin.y + editor.height,
        card.y + card.height
    );
}

#[test]
fn a_short_note_still_gets_an_editor_that_hugs_its_text() {
    // The clamp must not become "always fill the card": a short note's editor
    // is sized to its text and centred, which is what matches read mode.
    let mut app = app_with(vec![text_node("a", "one line")], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });

    let card = card_screen_rect(&app, 0);
    let editor = app.editor_box(0).expect("an open editor");
    assert!(editor.height < card.height, "should hug: {}", editor.height);
    assert!(editor.origin.y > card.y, "and be centred, not flush to the top");
}

// --- The vault browser ---
//
// These stop short of actually opening a board: `open_canvas` persists the
// choice, and `persist_config` writes the *real* user config (there's no
// scratch guard on it, unlike the document saves). So the tests drive the
// browser up to the point of opening and assert on what it resolved to.

/// An app with a vault configured and `canvases` already found in it, as if a
/// scan had landed. Relative paths are taken as vault-relative.
fn app_with_vault(canvases: &[&str]) -> App {
    let mut app = app_with(Vec::new(), Vec::new());
    app.config.vault = "/vault".to_owned();
    app.vault_canvases = canvases
        .iter()
        .map(|relative| thinkboard_core::vault::CanvasEntry {
            path: PathBuf::from("/vault").join(relative),
            relative: (*relative).to_owned(),
            modified: None,
        })
        .collect();
    app
}

#[test]
fn switching_canvases_needs_a_vault_to_browse() {
    // Without one, Cmd+O stays on the old flow — the open dialog — rather than
    // showing an empty panel.
    let mut app = app_with(Vec::new(), Vec::new());
    send(&mut app, Message::SwitchCanvas);
    assert!(app.vault_browser.is_none());
    assert!(!app.has_vault());

    // With one, the same key opens the browser.
    let mut app = app_with_vault(&["Inbox.canvas"]);
    send(&mut app, Message::SwitchCanvas);
    assert!(app.vault_browser.is_some());
}

#[test]
fn the_browser_shows_the_cached_list_before_the_rescan_lands() {
    // Reopening is instant because the previous scan is still there; the fresh
    // walk is in the returned task, which the test drops.
    let mut app = app_with_vault(&["Inbox.canvas", "Projects/Roadmap.canvas"]);
    send(&mut app, Message::SwitchCanvas);
    assert_eq!(app.vault_matches("").len(), 2);
}

#[test]
fn typing_narrows_the_list_and_enter_resolves_the_highlighted_board() {
    let mut app = app_with_vault(&["Inbox.canvas", "Projects/Roadmap.canvas", "Ideas.canvas"]);
    send(&mut app, Message::SwitchCanvas);

    send(&mut app, Message::VaultBrowserInput("road".to_owned()));
    assert_eq!(app.vault_matches("road").len(), 1);
    assert_eq!(
        app.selected_canvas(),
        Some(PathBuf::from("/vault/Projects/Roadmap.canvas"))
    );

    // A query matching nothing leaves nothing to open, rather than opening
    // whatever the highlight last pointed at.
    send(&mut app, Message::VaultBrowserInput("zzz".to_owned()));
    assert_eq!(app.selected_canvas(), None);
}

#[test]
fn arrowing_moves_the_highlight_through_the_filtered_list() {
    let mut app = app_with_vault(&["A.canvas", "B.canvas", "C.canvas"]);
    send(&mut app, Message::SwitchCanvas);

    send(&mut app, Message::VaultBrowserNav(picker::Nav::Step(1)));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/B.canvas")));
    // Up from the first row wraps to the last, as in the palette.
    send(&mut app, Message::VaultBrowserNav(picker::Nav::First));
    send(&mut app, Message::VaultBrowserNav(picker::Nav::Step(-1)));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/C.canvas")));
}

#[test]
fn a_finished_scan_replaces_the_list_and_drops_the_stale_highlight() {
    let mut app = app_with_vault(&["A.canvas", "B.canvas", "C.canvas"]);
    send(&mut app, Message::SwitchCanvas);
    send(&mut app, Message::VaultBrowserNav(picker::Nav::Last));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/C.canvas")));

    // The rescan comes back with a shorter list. The highlight indexed the old
    // one, so it goes back to the top rather than pointing at a different board
    // (or off the end of the list entirely).
    let fresh = vec![thinkboard_core::vault::CanvasEntry {
        path: PathBuf::from("/vault/Only.canvas"),
        relative: "Only.canvas".to_owned(),
        modified: None,
    }];
    send(&mut app, Message::VaultScanned(fresh, false));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/Only.canvas")));
    assert!(!app.vault_scanning);
}

#[test]
fn only_one_panel_is_open_at_a_time() {
    let mut app = app_with_vault(&["Inbox.canvas"]);
    send(&mut app, Message::SwitchCanvas);
    send(&mut app, Message::OpenCommandPalette);
    assert!(app.vault_browser.is_none(), "the palette displaces the browser");

    send(&mut app, Message::SwitchCanvas);
    assert!(app.command_palette.is_none(), "and the browser displaces the palette");

    // The find bar displaces it too — all three share the same screen space.
    send(&mut app, Message::OpenSearch);
    assert!(app.vault_browser.is_none());
}

#[test]
fn switch_canvas_is_only_offered_once_there_is_a_vault() {
    let app = app_with(Vec::new(), Vec::new());
    assert!(!command::CommandId::SwitchCanvas.is_enabled(&app));
    // "Open vault…" is always available — it's how you get one.
    assert!(command::CommandId::OpenVault.is_enabled(&app));

    let app = app_with_vault(&["Inbox.canvas"]);
    assert!(command::CommandId::SwitchCanvas.is_enabled(&app));
}
