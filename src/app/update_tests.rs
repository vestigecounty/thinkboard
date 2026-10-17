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
        text: Some(text.to_owned()),
        width: 200.0,
        height: 100.0,
        ..thinkboard_core::fixtures::node(id)
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

/// Presses Return in the open editor along the real path.
fn press_return(app: &mut App) {
    send(app, Message::EditorReturn);
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

// --- Stacking ---

fn ids(app: &App) -> Vec<&str> {
    app.nodes.iter().map(|node| node.id.as_str()).collect()
}

#[test]
fn a_card_dropped_on_a_later_one_comes_to_the_front() {
    // Stacking is list order: dropped onto `b`, `a` would go back under it
    // once deselected.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(350.0, 20.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });

    assert_eq!(ids(&app), ["b", "a"]);
    assert_eq!(app.selected, vec![1], "the selection follows the card it named");
}

#[test]
fn the_order_only_changes_when_the_drop_covers_something() {
    // A drop onto empty board, and a click that moved nothing, leave the file
    // alone: an order changed for nothing is a file changed for nothing.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 300.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    assert_eq!(ids(&app), ["a", "b"]);

    send(&mut app, Message::MoveNode { index: 0, position: Point::new(350.0, 20.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: false });
    assert_eq!(ids(&app), ["a", "b"], "a click is not a drop");
}

// --- The link menu ---

fn open_link_menu(app: &mut App, edge: usize) {
    send(app, Message::OpenLinkMenu { screen: Point::new(0.0, 0.0), edge });
}

#[test]
fn a_links_label_is_what_the_field_holds_when_the_menu_closes() {
    let mut app = app_with(two_nodes(), vec![edge("a", "b")]);
    open_link_menu(&mut app, 0);
    send(&mut app, Message::LinkLabelInput("depends on".to_owned()));
    assert_eq!(app.edges[0].label(), None, "nothing is written while typing");
    send(&mut app, Message::CloseContextMenu);
    assert_eq!(app.edges[0].label(), Some("depends on"));

    send(&mut app, Message::Undo);
    assert_eq!(app.edges[0].label(), None, "one undo step takes it back");
}

#[test]
fn deleting_a_link_from_its_menu_labels_nothing_else() {
    // The draft names its link by id: deleted, the next link slides into its
    // index, and must not inherit the half-typed label.
    let mut nodes = two_nodes();
    nodes.push(text_node("c", "node c"));
    let mut app = app_with(nodes, vec![edge("a", "b"), edge("b", "c")]);
    open_link_menu(&mut app, 0);
    send(&mut app, Message::LinkLabelInput("half-typed".to_owned()));
    send(&mut app, Message::DeleteSelected);

    assert_eq!(app.edges.len(), 1);
    assert_eq!(app.edges[0].label(), None);
}

#[test]
fn the_link_menu_sets_arrows_and_colour_on_the_link() {
    use crate::model::EdgeEnd;
    let mut app = app_with(two_nodes(), vec![edge("a", "b")]);
    open_link_menu(&mut app, 0);
    send(&mut app, Message::SetLinkEnds { from: EdgeEnd::Arrow, to: EdgeEnd::Arrow });
    assert_eq!((app.edges[0].from_end(), app.edges[0].to_end()), (EdgeEnd::Arrow, EdgeEnd::Arrow));

    // The swatches are the node menu's: with a link selected, they color it.
    open_link_menu(&mut app, 0);
    send(&mut app, Message::SetNodeColor(Some("1".to_owned())));
    assert!(app.edges[0].palette_color().is_some());
    assert!(app.nodes.iter().all(|node| node.color.is_none()), "and no node");
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
fn a_drag_is_one_undo_step_however_many_moves_it_took() {
    // Drags weren't undoable at all, for fear of a step per mouse-move — so a
    // card dropped in the wrong place couldn't be put back.
    let mut app = app_with(two_nodes(), Vec::new());
    for x in [100.0, 300.0, 500.0] {
        send(&mut app, Message::MoveNode { index: 0, position: Point::new(x, 500.0) });
    }
    assert!(!app.history.can_undo(), "nothing recorded until it ends");
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    assert_eq!(app.nodes[0].x, 500.0);

    send(&mut app, Message::Undo);
    assert_eq!((app.nodes[0].x, app.nodes[0].y), (0.0, 0.0), "back where the drag began");
    assert!(!app.history.can_undo(), "one step for the whole drag");
    send(&mut app, Message::Redo);
    assert_eq!(app.nodes[0].x, 500.0);
}

#[test]
fn a_press_that_moved_nothing_is_no_step() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::NodeDragEnd { index: 0, moved: false });
    // A drag that came back to where it began.
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(80.0, 0.0) });
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 0.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    assert!(!app.history.can_undo());
}

#[test]
fn a_resize_is_one_undo_step() {
    let mut app = app_with(two_nodes(), Vec::new());
    let anchor = Point::new(0.0, 0.0);
    for corner in [Point::new(260.0, 120.0), Point::new(320.0, 160.0)] {
        send(&mut app, Message::ResizeNode { index: 0, anchor, cursor: corner });
    }
    send(&mut app, Message::ResizeEnd);
    assert_eq!(app.nodes[0].width, 320.0);
    send(&mut app, Message::Undo);
    assert_eq!((app.nodes[0].width, app.nodes[0].height), (200.0, 100.0));
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
fn a_new_note_and_its_first_text_are_one_undo_step() {
    // Adding a note wasn't undoable: its first edit skipped the snapshot, so
    // Cmd+Z went straight past it to whatever came before.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::CreateNode { world: Point::new(0.0, 400.0) });
    send(&mut app, Message::StartEditing { index: 2, at: None });
    type_text(&mut app, "Pack an umbrella");
    send(&mut app, Message::StopEditing);
    assert_eq!(app.nodes.len(), 3);

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes.len(), 2, "the note is gone, not just emptied");
    assert!(!app.history.can_undo());
    send(&mut app, Message::Redo);
    assert_eq!(text_of(&app, 2), Some("Pack an umbrella"));

    // One left empty is taken back without leaving a step to undo.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::CreateNode { world: Point::new(0.0, 400.0) });
    send(&mut app, Message::StartEditing { index: 2, at: None });
    send(&mut app, Message::StopEditing);
    assert!(!app.history.can_undo());
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
fn return_writes_a_list_and_return_on_an_empty_item_ends_it() {
    let mut app = app_with(vec![text_node("a", "")], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });

    type_text(&mut app, "Packing");
    press_return(&mut app);
    type_text(&mut app, "- passport");
    press_return(&mut app);
    type_text(&mut app, "charger");
    press_return(&mut app);
    press_return(&mut app);
    type_text(&mut app, "Shoes?");

    assert_eq!(text_of(&app, 0), Some("Packing\n- passport\n- charger\nShoes?"));
}

#[test]
fn a_card_grows_down_from_its_top_edge() {
    // Grown about its centre, each new line pushed the top up half a line and
    // the text already written crept upward as you typed.
    let mut app = app_with(vec![text_node("a", "")], Vec::new());
    let top = app.nodes[0].y;

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "one\ntwo\nthree\nfour\nfive");
    assert!(app.nodes[0].height > 100.0, "it grew: {}", app.nodes[0].height);
    assert_eq!(app.nodes[0].y, top);
    send(&mut app, Message::StopEditing);
    assert_eq!(app.nodes[0].y, top);
}

#[test]
fn ending_an_edit_leaves_a_heading_card_where_it_was() {
    // A heading is typed at body size and rendered large; sized to the source
    // alone while editing, the card jumped on Esc.
    let mut app = app_with(vec![text_node("a", "")], Vec::new());

    send(&mut app, Message::StartEditing { index: 0, at: None });
    type_text(&mut app, "# Trip to Lisbon in May\n\nFlights first");
    let editing = (app.nodes[0].width, app.nodes[0].height);
    send(&mut app, Message::StopEditing);

    assert_eq!((app.nodes[0].width, app.nodes[0].height), editing);
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

/// A note past the height ceiling, in sections, with `word` once near its end.
fn long_note(word: &str) -> CanvasNode {
    let mut text = String::from("# A long note");
    for section in 1..=14 {
        text.push_str(&format!(
            "\n\n## Section {section}\n\nSome **text** for section {section}, long enough to wrap onto a second line in the card."
        ));
    }
    text.push_str(&format!("\n\nThe last paragraph mentions {word} once."));
    CanvasNode {
        text: Some(text),
        width: 400.0,
        height: text_raster::MAX_AUTO_HEIGHT,
        ..thinkboard_core::fixtures::node("long")
    }
}

/// Where `word` is drawn on node `index` (world), found the way the board lays
/// the card out, scroll and all.
fn drawn_at(app: &App, index: usize, word: &str) -> Point {
    let node = &app.nodes[index];
    let scroll = board::scrolls(&app.config, node)
        .then(|| board::scroll_offset(&app.config, &app.theme, &app.node_scroll, node));
    let frame = board::text_frame(arrange::node_world_rect(node), scroll, 1.0);
    let text = node.text.as_deref().unwrap();
    text_raster::with_markdown_layout(
        text, frame.width, frame.height, text_raster::card_text_wrap(node.width), FONT_SIZE, app.theme,
        app.config.center_node_text,
        |runs| {
            let run = runs.iter().find(|run| run.text.contains(word)).expect("the word is drawn");
            let before = &run.text[..run.text.find(word).unwrap()];
            let x = run.x + text_raster::run_advance(before, run, &app.theme) + 2.0;
            Point::new(frame.x + x, frame.y + run.y + run.size * text_raster::LINE_HEIGHT / 2.0)
        },
    )
}

#[test]
fn double_clicking_a_long_card_puts_the_caret_on_the_word_under_it() {
    // The editor shows the source, where a heading is body size, and opens at
    // its top: clicked at the same point, the caret landed in a paragraph
    // above the word — on a scrolled card, one that hadn't been on screen.
    let mut app = app_with(vec![long_note("Cacilhas")], Vec::new());
    for scroll in [0.0, 400.0] {
        app.node_scroll.insert("long".to_owned(), scroll);
        let at = drawn_at(&app, 0, "Section 13");
        send(&mut app, Message::StartEditing { index: 0, at: Some(at) });
        let editing = app.editing.as_ref().expect("editing");
        let position = editing.content.cursor().position;
        let line = editing.content.line(position.line).expect("a line").text.into_owned();
        assert_eq!(line, "## Section 13", "scrolled {scroll}: caret on {line:?}");
        assert!((3..=6).contains(&position.column), "on the word clicked, column {}", position.column);
        send(&mut app, Message::StopEditing);
    }
}

#[test]
fn find_scrolls_a_long_card_to_its_hit() {
    // The hit was counted and highlighted, and out of sight below the card's
    // edge, in the part of the note it hadn't scrolled to.
    let mut app = app_with(vec![long_note("Cacilhas")], Vec::new());
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("cacilhas".to_owned()));
    let scroll = app.node_scroll.get("long").copied().unwrap_or(0.0);
    assert!(scroll > 0.0, "scrolled to it");
    let node = &app.nodes[0];
    let hit = drawn_at(&app, 0, "Cacilhas");
    assert!(hit.y > node.y && hit.y < node.y + node.height, "the hit is inside the card: {} in {}..{}", hit.y, node.y, node.y + node.height);
    // And the camera goes to the hit, not the middle of the card.
    let focus = app.camera_focus.expect("flying to it");
    assert!((focus.target_offset.y / -focus.target_zoom - hit.y).abs() < 20.0);
}

#[test]
fn an_edit_that_changes_nothing_leaves_the_card_as_it_was() {
    // A card Obsidian left smaller than its note grew over the one below on a
    // double-click and an Esc.
    let small = CanvasNode {
        width: 300.0,
        height: 60.0,
        ..text_node("a", "Taberna da Rua das Flores — no bookings, so arrive at opening; the menu is a chalkboard.")
    };
    let mut app = app_with(vec![small], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::StopEditing);
    assert_eq!((app.nodes[0].width, app.nodes[0].height), (300.0, 60.0));
    // A change still fits it.
    send(&mut app, Message::StartEditing { index: 0, at: None });
    caret_to_end(&mut app);
    type_text(&mut app, " Cash only.");
    send(&mut app, Message::StopEditing);
    assert!(app.nodes[0].height > 60.0);
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

    send(&mut app, Message::PanelInput(picker::Panel::Vault, "road".to_owned()));
    assert_eq!(app.vault_matches("road").len(), 1);
    assert_eq!(
        app.selected_canvas(),
        Some(PathBuf::from("/vault/Projects/Roadmap.canvas"))
    );

    // A query matching nothing leaves nothing to open, rather than opening
    // whatever the highlight last pointed at.
    send(&mut app, Message::PanelInput(picker::Panel::Vault, "zzz".to_owned()));
    assert_eq!(app.selected_canvas(), None);
}

#[test]
fn arrowing_moves_the_highlight_through_the_filtered_list() {
    let mut app = app_with_vault(&["A.canvas", "B.canvas", "C.canvas"]);
    send(&mut app, Message::SwitchCanvas);

    send(&mut app, Message::PanelNav(picker::Panel::Vault, picker::Nav::Step(1)));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/B.canvas")));
    // Up from the first row wraps to the last, as in the palette.
    send(&mut app, Message::PanelNav(picker::Panel::Vault, picker::Nav::First));
    send(&mut app, Message::PanelNav(picker::Panel::Vault, picker::Nav::Step(-1)));
    assert_eq!(app.selected_canvas(), Some(PathBuf::from("/vault/C.canvas")));
}

#[test]
fn a_finished_scan_replaces_the_list_and_drops_the_stale_highlight() {
    let mut app = app_with_vault(&["A.canvas", "B.canvas", "C.canvas"]);
    send(&mut app, Message::SwitchCanvas);
    send(&mut app, Message::PanelNav(picker::Panel::Vault, picker::Nav::Last));
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

// --- Following the vault's Obsidian theme ---

/// A throwaway vault whose `.obsidian` config names `theme` as the active one.
struct ThemedVault(PathBuf);

impl ThemedVault {
    fn new(tag: &str, css_theme: &str, install: bool) -> Self {
        let root = std::env::temp_dir().join(format!("tb-theme-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(
            root.join(".obsidian/appearance.json"),
            format!(r#"{{"theme":"obsidian","cssTheme":"{css_theme}"}}"#),
        )
        .unwrap();
        if install {
            let dir = root.join(".obsidian/themes").join(css_theme);
            std::fs::create_dir_all(&dir).unwrap();
            // A theme narrow enough to check it was actually read.
            std::fs::write(dir.join("theme.css"), ".theme-dark { --text-normal: #ff0000; }").unwrap();
        }
        ThemedVault(root)
    }

    fn theme_path(&self, name: &str) -> String {
        self.0
            .join(".obsidian/themes")
            .join(name)
            .join("theme.css")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for ThemedVault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A default config pointed at `vault`, with `theme_css` as given.
fn config_for(vault: &ThemedVault, theme_css: Option<&str>) -> config::Config {
    config::Config {
        vault: vault.0.to_string_lossy().into_owned(),
        theme_css: theme_css.map(str::to_owned),
        ..config::Config::default()
    }
}

#[test]
fn setting_a_vault_adopts_the_theme_obsidian_is_using() {
    let vault = ThemedVault::new("adopt", "Things", true);
    let mut config = config_for(&vault, None);

    assert!(config.adopt_vault_theme(), "should have found a theme");
    assert_eq!(config.theme_css, Some(vault.theme_path("Things")));
    // Idempotent: nothing changed the second time, so nothing to persist.
    assert!(!config.adopt_vault_theme());
}

#[test]
fn a_hand_picked_theme_survives_the_vault() {
    let vault = ThemedVault::new("manual", "Things", true);
    // Someone deliberately pointed at a CSS file of their own. The vault's
    // theme must not quietly replace it.
    let mut config = config_for(&vault, Some("/elsewhere/mine.css"));

    assert!(!config.adopt_vault_theme());
    assert_eq!(config.theme_css.as_deref(), Some("/elsewhere/mine.css"));
}

#[test]
fn a_theme_cleared_in_obsidian_is_cleared_here_too() {
    // Obsidian records its own default theme as an empty `cssTheme`. We have no
    // CSS for it, so the app goes back to its built-in palette rather than
    // keeping the theme that was set before.
    let vault = ThemedVault::new("cleared", "", false);
    let previous = vault.theme_path("Things");
    let mut config = config_for(&vault, Some(&previous));

    assert!(config.adopt_vault_theme());
    assert_eq!(config.theme_css, None);
}

#[test]
fn a_theme_named_but_not_installed_is_not_adopted() {
    let vault = ThemedVault::new("missing", "Nonexistent", false);
    let mut config = config_for(&vault, None);

    assert!(!config.adopt_vault_theme());
    assert_eq!(config.theme_css, None);
}

#[test]
fn no_vault_means_no_opinion_about_the_theme() {
    let mut config = config::Config {
        theme_css: Some("/somewhere/theme.css".to_owned()),
        ..config::Config::default()
    };
    assert!(!config.adopt_vault_theme());
    assert_eq!(config.theme_css.as_deref(), Some("/somewhere/theme.css"));
}

#[test]
fn applying_a_theme_recolours_the_board_without_a_restart() {
    let vault = ThemedVault::new("live", "Things", true);
    let mut app = app_with(vec![text_node("a", "hello")], Vec::new());
    // Stand in a bitmap, as a settled board would have for every card.
    app.rasters.text.insert(
        0,
        text_raster::snapshot_markdown(
            "hello", 200.0, 100.0, 180.0, 17.0, app.theme, false, 1.0,
        )
        .expect("a bitmap for the card"),
    );
    let before = app.theme.text;

    app.config.vault = vault.0.to_string_lossy().into_owned();
    let _ = app.adopt_vault_theme();

    assert_eq!(app.config.theme_css, Some(vault.theme_path("Things")));
    assert_ne!(app.theme.text, before, "the theme's --text-normal should be live");
    assert_eq!(app.theme.text, iced::Color::from_rgb8(0xff, 0x00, 0x00));
    // The bitmaps held the old colours, so they can't survive the swap.
    assert!(app.rasters.text.is_empty());
}

// --- The theme picker ---

#[test]
fn the_picker_opens_on_the_theme_in_use() {
    // So the first arrow press is a step from where you are, not a jump to the
    // top of a list of twenty.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.theme = "Nord".to_owned();
    send(&mut app, Message::OpenThemePicker);
    assert_eq!(app.highlighted_theme_name().as_deref(), Some("Nord"));
}

#[test]
fn scrolling_the_picker_previews_on_the_board() {
    let mut app = app_with(two_nodes(), Vec::new());
    // Stand in a bitmap, as a settled board has for every card.
    app.rasters.text.insert(
        0,
        text_raster::snapshot_markdown("node a", 200.0, 100.0, 180.0, 17.0, app.theme, false, 1.0)
            .expect("a bitmap"),
    );
    let before = app.theme.background;

    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::PanelNav(picker::Panel::Theme, picker::Nav::Step(1)));
    assert_ne!(app.theme.background, before, "the board should show the highlight");
    // The bitmaps carried the old colours, so the preview drops them — which is
    // also what makes it cheap enough to run on every keypress, since the cards
    // fall back to drawing live.
    assert!(app.rasters.text.is_empty());
}

#[test]
fn dismissing_the_picker_puts_the_old_theme_back() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.theme = "Nord".to_owned();
    // Bring the live theme in line with the config, as startup does — the two
    // only disagree in a test that sets one without the other.
    let _ = app.apply_theme_css();
    let nord = app.theme.background;
    send(&mut app, Message::OpenThemePicker);

    send(&mut app, Message::PanelNav(picker::Panel::Theme, picker::Nav::Step(2)));
    assert_ne!(app.theme.background, nord);

    send(&mut app, Message::ClosePanel(picker::Panel::Theme));
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.background, nord, "Esc must undo the preview");
    assert_eq!(app.config.theme, "Nord", "and not have written the previewed one");
}

#[test]
fn choosing_a_theme_keeps_it() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::PanelInput(picker::Panel::Theme, "solarized".to_owned()));
    let previewed = app.theme.background;

    send(&mut app, Message::CommitTheme);
    assert!(app.theme_picker.is_none());
    assert_eq!(app.config.theme, "Solarized Dark");
    assert_eq!(app.theme.background, previewed, "what you saw is what you kept");
}

#[test]
fn typing_narrows_the_themes_by_name_and_author() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::PanelInput(picker::Panel::Theme, "gruv".to_owned()));
    assert_eq!(app.highlighted_theme_name().as_deref(), Some("Gruvbox Dark"));

    // Authors are searchable but never shown — hidden terms, like the command
    // palette's keywords.
    send(&mut app, Message::PanelInput(picker::Panel::Theme, "schoonover".to_owned()));
    assert_eq!(app.highlighted_theme_name().as_deref(), Some("Solarized Dark"));
}

/// Every panel against every other, rather than the pairs someone thought to
/// name. The lists each opener kept had drifted — the font and model pickers
/// could be open together, with the font preview left applied over a config
/// that said otherwise — and a table is the only version of this test that
/// stays true when a sixth panel is added.
#[test]
fn opening_any_panel_closes_every_other() {
    let open = |app: &mut App, which: usize| {
        send(
            app,
            match which {
                0 => Message::OpenCommandPalette,
                1 => Message::OpenVaultBrowser,
                2 => Message::OpenThemePicker,
                3 => Message::OpenFontPicker,
                4 => Message::OpenModelPicker,
                _ => Message::OpenSearch,
            },
        );
    };
    let open_count = |app: &App| {
        usize::from(app.command_palette.is_some())
            + usize::from(app.vault_browser.is_some())
            + usize::from(app.theme_picker.is_some())
            + usize::from(app.font_picker.is_some())
            + usize::from(app.model_picker.is_some())
            + usize::from(app.search.is_some())
    };

    for first in 0..6 {
        for second in 0..6 {
            if first == second {
                continue;
            }
            let mut app = app_with(two_nodes(), Vec::new());
            app.config.vault = "/tmp".to_owned(); // so the browser has something to open
            app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
            open(&mut app, first);
            open(&mut app, second);
            assert_eq!(
                open_count(&app),
                1,
                "opening {second} over {first} left {} panels up",
                open_count(&app),
            );
        }
    }
}

#[test]
fn opening_a_panel_undoes_a_preview_left_by_another() {
    // The font and theme pickers show their highlight on the board. Displacing
    // one has to put the board back, or it keeps a face the config never got.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    assert_eq!(app.theme.font_text, None);

    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    assert!(app.theme.font_text.is_some(), "previewing a font");

    send(&mut app, Message::OpenModelPicker);
    assert!(app.font_picker.is_none());
    assert_eq!(app.theme.font_text, None, "the preview must not outlive its picker");
}

#[test]
fn reopening_the_find_bar_keeps_what_was_typed() {
    // `dismiss_panels` clears the find bar along with the panels, so the
    // find-bar opener has to notice it is already open.
    let mut app = app_with(vec![text_node("a", "ThinkBoard")], Vec::new());
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("hinkBo".to_owned()));
    send(&mut app, Message::OpenSearch);
    assert_eq!(
        app.search.as_ref().map(|s| s.query.as_str()),
        Some("hinkBo"),
        "re-summoning the find bar must not empty it",
    );
}

#[test]
fn only_one_panel_is_open_at_a_time_including_the_theme_picker() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::OpenCommandPalette);
    assert!(app.theme_picker.is_none(), "the palette displaces the picker");

    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::OpenSearch);
    assert!(app.theme_picker.is_none(), "and so does the find bar");
}

#[test]
fn the_theme_picker_can_hand_the_board_back_to_the_stylesheet() {
    // A named theme wins over `theme_css`, so without a way to clear the name
    // a vault's Obsidian theme is unreachable once you try one of ours on.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.theme = "Nord".to_owned();
    let _ = app.apply_theme_css();
    let nord = app.theme.background;

    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::PickTheme(String::new()));
    assert_eq!(app.config.theme, "", "the name is cleared, not set to a theme");
    assert_ne!(app.theme.background, nord, "and the board stops being Nord");
}

#[test]
fn the_picker_opens_on_the_unnamed_row_when_no_theme_is_named() {
    let mut app = app_with(two_nodes(), Vec::new());
    assert_eq!(app.config.theme, "");
    send(&mut app, Message::OpenThemePicker);
    assert_eq!(
        app.highlighted_theme_name().as_deref(),
        Some(""),
        "the highlight opens on what's in use, which is no named theme",
    );
}

#[test]
fn naming_a_theme_in_settings_takes_effect_without_a_restart() {
    // The settings field and the picker write the same config value, so they
    // have to apply it the same way.
    let mut app = app_with(two_nodes(), Vec::new());
    let before = app.theme.background;
    let type_theme = |app: &mut App, name: &str| {
        send(app, Message::Settings(settings::Msg::Edit(settings::Field::Theme, name.to_owned())));
        send(app, Message::Settings(settings::Msg::Commit));
    };

    type_theme(&mut app, "Nord");
    assert_eq!(app.config.theme, "Nord");
    assert_ne!(app.theme.background, before, "the board should already be Nord");

    // And clearing it goes back, rather than leaving the last theme stuck on.
    type_theme(&mut app, "");
    assert_eq!(app.config.theme, "");
    assert_eq!(app.theme.background, before);
}

// --- The model picker ---

use thinkboard_core::ai::Model;

fn model(id: &str) -> Model {
    Model { id: id.to_owned(), name: None, context: None }
}

/// An app with a provider configured and a list already fetched for it.
fn app_with_models(ids: &[&str]) -> App {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    send(&mut app, Message::ModelsFetched(url, Ok(ids.iter().map(|id| model(id)).collect())));
    app
}

#[test]
fn a_fetched_list_fills_the_picker() {
    let app = app_with_models(&["anthropic/claude-sonnet-4", "openai/gpt-4o"]);
    assert_eq!(app.model_matches("").len(), 2);
    assert_eq!(app.highlighted_model().as_deref(), Some("anthropic/claude-sonnet-4"));
}

#[test]
fn typing_narrows_the_models() {
    let mut app = app_with_models(&["anthropic/claude-sonnet-4", "openai/gpt-4o"]);
    send(&mut app, Message::PanelInput(picker::Panel::Model, "gpt".to_owned()));
    assert_eq!(app.highlighted_model().as_deref(), Some("openai/gpt-4o"));
}

#[test]
fn choosing_a_model_writes_the_id_the_endpoint_gave() {
    // The whole point: the id goes through exactly, rather than being retyped
    // from memory and failing at the next question asked.
    let mut app = app_with_models(&["accounts/fireworks/models/llama-v3p1-70b-instruct"]);
    send(&mut app, Message::CommitModel);
    assert!(app.model_picker.is_none());
    assert_eq!(app.config.ai_model, "accounts/fireworks/models/llama-v3p1-70b-instruct");
    // And the settings form shows it, rather than the value it opened with.
    assert_eq!(
        app.settings_drafts.get(&settings::Field::AiModel).map(String::as_str),
        Some("accounts/fireworks/models/llama-v3p1-70b-instruct"),
    );
}

#[test]
fn a_failed_fetch_is_shown_rather_than_swallowed() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    send(&mut app, Message::ModelsFetched(url, Err("401: key rejected".to_owned())));
    let picker = app.model_picker.as_ref().expect("still open");
    assert!(!picker.loading);
    assert_eq!(picker.error.as_deref(), Some("401: key rejected"));
}

#[test]
fn a_fetch_for_another_endpoint_is_discarded() {
    // Switch provider while a request is in flight and the answer still lands.
    // Taking it would offer one provider's models under another's name.
    let mut app = app_with_models(&["openai/gpt-4o"]);
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    app.config.ai_url = "https://api.fireworks.ai/inference/v1".to_owned();
    send(&mut app, Message::OpenModelPicker);

    send(
        &mut app,
        Message::ModelsFetched(
            "https://openrouter.ai/api/v1".to_owned(),
            Ok(vec![model("openai/gpt-4o")]),
        ),
    );
    assert!(app.model_matches("").is_empty(), "the stale list must not show");
    assert!(app.model_picker.as_ref().expect("open").loading, "still waiting on its own");
}

#[test]
fn the_command_is_greyed_without_an_endpoint() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url.clear();
    assert!(!command::CommandId::ChooseModel.is_enabled(&app));
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    assert!(command::CommandId::ChooseModel.is_enabled(&app));
}

#[test]
fn custom_is_remembered_before_its_url_is_typed() {
    // An empty URL reads as "Off", so without this the form would snap back
    // to "Off" and hide the URL field "Custom" was picked to reveal.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url.clear();
    send(&mut app, Message::Settings(settings::Msg::PickPreset("Custom".to_owned())));
    assert!(app.settings_custom_ai);
    assert!(!app.config.ai_enabled(), "picking Custom writes no URL of its own");
    send(&mut app, Message::Settings(settings::Msg::PickPreset(settings::PROVIDER_OFF.to_owned())));
    assert!(!app.settings_custom_ai);
}

#[test]
fn picking_a_provider_that_can_answer_asks_for_the_model_next() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url.clear();
    send(&mut app, Message::OpenSettingsOverlay);

    // A local runtime needs no key, so its list comes up at once — over the
    // form it was chosen in, not hidden under it on the board.
    send(&mut app, Message::Settings(settings::Msg::PickPreset("LM Studio".to_owned())));
    assert!(app.model_picker.as_ref().is_some_and(|picker| picker.over_settings));

    // A hosted one with no key would only list a refusal: the form stays put.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, Message::Settings(settings::Msg::PickPreset("OpenAI".to_owned())));
    assert!(app.model_picker.is_none());
    // With its key in place, it's ready.
    app.config.set_ai_key("sk-1");
    send(&mut app, Message::Settings(settings::Msg::PickPreset("OpenAI".to_owned())));
    assert!(app.model_picker.is_some());

    // And the button beside the field, whenever.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    assert!(app.model_picker.as_ref().is_some_and(|picker| picker.over_settings));
}

#[test]
fn esc_over_the_settings_closes_only_the_picker() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    let esc = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        physical_key: iced::keyboard::key::Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: iced::keyboard::Location::Standard,
        modifiers: iced::keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    });
    let replies: Vec<Message> = app
        .event_listeners()
        .iter()
        .filter_map(|listen| listen(esc.clone(), iced::event::Status::Ignored, window::Id::unique()))
        .collect();
    assert_eq!(replies.len(), 1, "one Esc, one thing closed");
    for reply in replies {
        send(&mut app, reply);
    }
    assert!(app.model_picker.is_none());
    assert!(app.settings_overlay, "the form it was chosen from stays");

    // Closing the form takes a picker opened from it along.
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    send(&mut app, Message::CloseSettingsOverlay);
    assert!(app.model_picker.is_none(), "left open, it would block the board unseen");
}

#[test]
fn using_a_control_focuses_its_row_and_a_tab_starts_unfocused() {
    // A row's description shows while it's focused, and a press on a control
    // never reaches the row around it — so using one has to focus it.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenSettingsOverlay);
    assert_eq!(app.settings_focus, None);
    send(&mut app, Message::Settings(settings::Msg::Slide(settings::Field::OverlayDim, 0.3)));
    assert_eq!(app.settings_focus, Some(settings::Field::OverlayDim));
    send(&mut app, Message::Settings(settings::Msg::Tab(settings::Tab::Canvas)));
    assert_eq!(app.settings_tab, settings::Tab::Canvas);
    assert_eq!(app.settings_focus, None, "the focused row isn't on this tab");

    // Closed and opened again, the form is where it was left.
    send(&mut app, Message::CloseSettingsOverlay);
    send(&mut app, Message::OpenSettingsOverlay);
    assert_eq!(app.settings_tab, settings::Tab::Canvas);
}

#[test]
fn reset_puts_a_setting_back_and_clears_its_mark() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::Slide(settings::Field::ZoomSpeed, 1.6)));
    assert!(settings::Field::ZoomSpeed.modified(&app.config));
    send(&mut app, Message::Settings(settings::Msg::Reset(settings::Field::ZoomSpeed)));
    assert_eq!(app.config.zoom_speed, 1.0);
    assert!(!settings::Field::ZoomSpeed.modified(&app.config));

    // A typed number goes back in its field too, not just in the config.
    send(&mut app, Message::Settings(settings::Msg::Edit(settings::Field::MaxNodeWidth, "600".to_owned())));
    send(&mut app, Message::Settings(settings::Msg::Reset(settings::Field::MaxNodeWidth)));
    assert_eq!(app.draft(settings::Field::MaxNodeWidth), "420");

    // The provider's way back is Off.
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::Settings(settings::Msg::Reset(settings::Field::AiProvider)));
    assert!(!app.config.ai_enabled());
}

#[test]
fn reload_takes_what_the_file_says_and_keeps_the_board_open() {
    // Under test the file is a fresh config (`Config::load` won't read the
    // developer's own), so a reload is a return to the defaults — except the
    // open board, which a reload of settings mustn't switch.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.canvas = "/boards/open.canvas".to_owned();
    app.config.overlay_dim = 0.2;
    app.config.layout_rank_gap = 300.0;
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ReloadConfig));
    assert_eq!(app.config.overlay_dim, crate::config::Config::default().overlay_dim);
    assert_eq!(app.config.layout_rank_gap, crate::config::Config::default().layout_rank_gap);
    assert_eq!(app.config.canvas, "/boards/open.canvas");
}

#[test]
fn a_theme_picked_from_the_form_is_drawn_over_it_and_closes_with_it() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenSettingsOverlay);
    let before = app.theme;
    send(&mut app, Message::Settings(settings::Msg::ChooseTheme));
    assert!(app.theme_picker.as_ref().is_some_and(|picker| picker.over_settings));
    send(&mut app, Message::PanelNav(picker::Panel::Theme, picker::Nav::Step(3)));
    assert_ne!(app.theme.background, before.background, "the highlight previews");

    // Closing the form cancels the preview, as any dismissal of it does.
    send(&mut app, Message::CloseSettingsOverlay);
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.background, before.background);

    // The font picker the same way.
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ChooseFont));
    assert!(app.font_picker.as_ref().is_some_and(|picker| picker.over_settings));
    send(&mut app, Message::CloseSettingsOverlay);
    assert!(app.font_picker.is_none());
}

/// Runs a palette command as choosing it does — what it sends, sent.
fn run(app: &mut App, id: command::CommandId) {
    let message = app.command_message(id).expect("a command that runs");
    send(app, message);
}

#[test]
fn the_palette_finds_a_settings_choice_and_applies_it() {
    let mut app = app_with(two_nodes(), Vec::new());
    let hits = command::filtered(&app, "mermaid");
    let first = hits.first().expect("a hit");
    assert_eq!(first.title, "Link style › Mermaid");
    assert_eq!(first.detail, "", "not the one in force yet");
    run(&mut app, first.id);
    assert_eq!(crate::config::LinkStyle::of(&app.config), crate::config::LinkStyle::Mermaid);

    // Searched by the setting, every choice is there, the one in force marked.
    let hits = command::filtered(&app, "link style");
    let marked: Vec<&str> =
        hits.iter().filter(|entry| entry.detail == "current").map(|entry| entry.title.as_ref()).collect();
    assert_eq!(marked, ["Link style › Mermaid"]);
}

#[test]
fn the_palette_flips_a_switch_and_opens_the_form_at_anything_else() {
    let mut app = app_with(two_nodes(), Vec::new());
    // A switch says its state, and flips.
    let scrollbars = command::filtered(&app, "card scrollbars").into_iter().next().expect("a hit");
    assert_eq!((scrollbars.title.as_ref(), scrollbars.detail.as_ref()), ("Card scrollbars", "Off"));
    run(&mut app, scrollbars.id);
    assert!(app.config.node_scrollbars);

    // A number is set in the form: it opens there, on its tab, its row focused.
    let inertia = command::filtered(&app, "pan inertia").into_iter().next().expect("a hit");
    assert_eq!(inertia.detail, "Canvas");
    run(&mut app, inertia.id);
    assert!(app.settings_overlay);
    assert_eq!(app.settings_tab, settings::Tab::Canvas);
    assert_eq!(app.settings_focus, Some(settings::Field::PanInertia));
}

#[test]
fn settings_join_a_search_not_the_bare_palette() {
    let app = app_with(two_nodes(), Vec::new());
    // Nothing typed: the commands, as a list to browse.
    assert!(command::filtered(&app, "").iter().all(|entry| !matches!(
        entry.id,
        command::CommandId::Setting(_) | command::CommandId::Choice(..)
    )));
    // A setting that can't act yet says what would let it.
    let mut app = app;
    app.config.ai_url.clear();
    let thinking = command::filtered(&app, "think first").into_iter().next().expect("a hit");
    assert!(thinking.id.unavailable(&app).is_some_and(|why| why.contains("AI provider")));
}

#[test]
fn a_settings_file_dialog_puts_the_overlay_away_and_brings_it_back() {
    // The overlay floats above every window: a dialog opened over it came up
    // behind the board, out of reach.
    let mut app = app_with(two_nodes(), Vec::new());
    app.overlay = true;
    app.phase = Phase::Running;
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::Browse(settings::Field::OverlayImage)));
    assert!(matches!(app.phase, Phase::Hidden), "put away for the dialog");
    assert!(app.settings_overlay, "the form is still there to come back to");
    // The answer — a cancel, here — is what brings it back.
    send(&mut app, Message::SettingsBrowsed(settings::Field::OverlayImage, None));
    assert!(!app.settings_dialog_hid_overlay, "and only once");
}

#[test]
fn the_image_background_is_chosen_by_its_picture() {
    use crate::config::OverlayBackdrop;
    // With the overlay up, so asking puts it away rather than opening a real
    // dialog under the test.
    let mut app = app_with(two_nodes(), Vec::new());
    app.overlay = true;
    app.phase = Phase::Running;
    // No picture yet: the tile asks for one, and changes nothing until it has it.
    send(&mut app, Message::Settings(settings::Msg::Backdrop(OverlayBackdrop::Image)));
    assert_eq!(OverlayBackdrop::of(&app.config), OverlayBackdrop::Transparent);
    send(&mut app, Message::SettingsBrowsed(settings::Field::OverlayImage, None));
    assert_eq!(OverlayBackdrop::of(&app.config), OverlayBackdrop::Transparent, "a cancel keeps what was there");
    // A picture chosen chooses the image background.
    app.phase = Phase::Running;
    send(&mut app, Message::Settings(settings::Msg::Backdrop(OverlayBackdrop::Image)));
    send(&mut app, Message::SettingsBrowsed(settings::Field::OverlayImage, Some(PathBuf::from("/pictures/dunes.jpg"))));
    assert_eq!(OverlayBackdrop::of(&app.config), OverlayBackdrop::Image);
    assert_eq!(app.config.overlay_image, "/pictures/dunes.jpg");
}

#[test]
fn a_greyed_command_says_what_would_let_it_run() {
    use command::CommandId::AskAi;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url.clear();
    send(&mut app, Message::SelectNode(Some(0)));
    // The missing provider is the reason that matters even with a note
    // selected: selecting one more won't help.
    assert!(AskAi.unavailable(&app).is_some_and(|why| why.contains("AI provider")));

    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::SelectNode(None));
    assert_eq!(AskAi.unavailable(&app), Some("Select a note to ask about"));

    send(&mut app, Message::SelectNode(Some(0)));
    assert_eq!(AskAi.unavailable(&app), None);
}

#[test]
fn a_failure_partway_keeps_what_was_written() {
    use thinkboard_core::ai::Event;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    let text = |app: &App, id: &str| app.nodes.iter().find(|n| n.id == id).and_then(|n| n.text.clone());

    // Before the first word, the note holds only the dots: the failure is it.
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::AskAi);
    let first = app.ai.streams.keys().next().expect("asked").clone();
    send(&mut app, Message::Ai(first.clone(), Event::Error("Couldn't reach LM Studio".to_owned())));
    assert_eq!(text(&app, &first).as_deref(), Some("⚠ Couldn't reach LM Studio"));

    // After some, the answer stays and the failure follows it.
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::AskAi);
    let second = app.ai.streams.keys().next().expect("asked").clone();
    send(&mut app, Message::Ai(second.clone(), Event::Delta("Take the train from Rossio ".to_owned())));
    send(&mut app, Message::Ai(second.clone(), Event::Error("The connection dropped".to_owned())));
    assert_eq!(text(&app, &second).as_deref(), Some("Take the train from Rossio\n\n⚠ The connection dropped"));
}

#[test]
fn an_answers_notes_share_one_width() {
    use thinkboard_core::ai::Event;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::AskAi);
    let id = app.ai.streams.keys().next().expect("asked").clone();
    for event in [
        Event::Delta("**Time Out Market** — a quick lunch, half the city's kitchens under one roof.".to_owned()),
        Event::Split,
        Event::Delta("Tram 28".to_owned()),
        Event::Done,
    ] {
        send(&mut app, Message::Ai(id.clone(), event));
    }
    let answer: Vec<&CanvasNode> = app.nodes.iter().skip(2).collect();
    assert_eq!(answer.len(), 2);
    assert_eq!(answer[0].width, answer[1].width, "one column, one edge");
    assert!(answer[1].height < answer[0].height, "each still as tall as its own text");
}

// --- The font picker ---

/// A family name that is nobody's font.
///
/// Most of what these tests check is bookkeeping — what the config holds, what
/// the picker restores — and none of it needs a face that exists. A made-up
/// name keeps them the same on every machine; `use_font` simply finds no
/// weights for it and leaves the theme's alone.
const MADE_UP_FAMILY: &str = "Nonesuch Grotesk";

/// A family the running machine actually has, for the one test that needs the
/// picker's list to contain it. `None` where the font database is empty.
fn installed_family() -> Option<String> {
    text_raster::families().first().map(|f| f.name.clone())
}

#[test]
fn the_font_picker_opens_on_the_font_in_use() {
    // The one font test that has to consult the real database: the highlight
    // can only open on a family the picker is listing, and the picker lists
    // what the rasterizer can draw with.
    let Some(family) = installed_family() else {
        return; // no fonts installed: nothing to open on
    };
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.font = family.clone();
    send(&mut app, Message::OpenFontPicker);
    assert_eq!(app.highlighted_font().as_deref(), Some(family.as_str()));
}

#[test]
fn the_first_row_is_the_theme_s_own_font() {
    // Blank config, so nothing overrides the theme — the highlight opens on the
    // entry that means exactly that.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenFontPicker);
    assert_eq!(app.highlighted_font().as_deref(), Some(""));
}

#[test]
fn scrolling_the_font_picker_previews_on_the_board() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.rasters.text.insert(
        0,
        text_raster::snapshot_markdown("node a", 200.0, 100.0, 180.0, 17.0, app.theme, false, 1.0)
            .expect("a bitmap"),
    );
    assert_eq!(app.theme.font_text, None, "the default theme names no family");

    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    assert!(app.theme.font_text.is_some(), "the board should show the highlight");
    // Same trick as the theme preview: drop the bitmaps and let the cards draw
    // live, rather than re-rasterizing on every arrow press.
    assert!(app.rasters.text.is_empty());
}

#[test]
fn dismissing_the_font_picker_puts_the_old_font_back() {
    let mut app = app_with(two_nodes(), Vec::new());
    let family = MADE_UP_FAMILY.to_owned();
    app.config.font = family.clone();
    let _ = app.apply_theme_css();
    let before = app.theme.font_text;

    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    send(&mut app, Message::ClosePanel(picker::Panel::Font));

    assert!(app.font_picker.is_none());
    assert_eq!(app.theme.font_text, before, "Esc must undo the preview");
    assert_eq!(app.config.font, family, "and not have written the previewed one");
}

#[test]
fn choosing_a_font_keeps_it() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    let previewed = app.theme.font_text.expect("a family is highlighted");

    send(&mut app, Message::CommitFont);
    assert!(app.font_picker.is_none());
    assert_eq!(app.config.font, previewed, "the config records what was previewed");
    assert_eq!(app.theme.font_text, Some(previewed), "what you saw is what you kept");
}

#[test]
fn the_first_row_clears_a_font_back_to_the_theme_s() {
    // The way back. Without it a font could be set but never unset, since the
    // list is otherwise all families.
    let mut app = app_with(two_nodes(), Vec::new());
    let family = MADE_UP_FAMILY.to_owned();
    app.config.font = family;
    let _ = app.apply_theme_css();
    assert!(app.theme.font_text.is_some());

    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PickFont(String::new()));
    assert_eq!(app.config.font, "");
    assert_eq!(app.theme.font_text, None, "back to the theme's own face");
}

#[test]
fn a_font_survives_changing_the_theme() {
    // The point of keeping it in the config rather than in the theme: adopting
    // a theme takes its colours, not its typeface.
    let mut app = app_with(two_nodes(), Vec::new());
    let family = MADE_UP_FAMILY.to_owned();
    app.config.font = family.clone();
    let _ = app.apply_theme_css();

    send(&mut app, Message::OpenThemePicker);
    send(&mut app, Message::PanelInput(picker::Panel::Theme, "solarized".to_owned()));
    send(&mut app, Message::CommitTheme);

    assert_eq!(app.config.theme, "Solarized Dark");
    assert_eq!(app.theme.font_text, Some(family.as_str()));
}

#[test]
fn another_panel_cancels_a_font_preview_rather_than_dropping_it() {
    // A dropped picker would leave the previewed face on the board with a
    // config that says otherwise — invisible until the next relaunch.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    assert!(app.theme.font_text.is_some());

    send(&mut app, Message::OpenCommandPalette);
    assert!(app.font_picker.is_none(), "the palette displaces the picker");
    assert_eq!(app.theme.font_text, None, "and undoes what it was previewing");

    send(&mut app, Message::OpenFontPicker);
    send(&mut app, Message::PanelNav(picker::Panel::Font, picker::Nav::Step(1)));
    send(&mut app, Message::OpenSearch);
    assert!(app.font_picker.is_none(), "and so does the find bar");
    assert_eq!(app.theme.font_text, None);
}

#[test]
fn a_raster_in_flight_cannot_un_highlight_a_search_hit() {
    // Rasterizing runs in the background, so a batch started before the search
    // lands after it. Installing its bitmap would hide the highlight, since
    // only the live path paints one — and nothing would invalidate it again,
    // so the hit would stay unmarked until the card happened to be redrawn for
    // some other reason.
    let mut app = app_with(vec![text_node("a", "ThinkBoard")], Vec::new());
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("hinkBo".to_owned()));
    assert!(app.is_search_match(0), "the node should be a match");

    let stale = text_raster::snapshot_markdown(
        "ThinkBoard", 200.0, 100.0, 180.0, 17.0, app.theme, false, 1.0,
    )
    .expect("a bitmap");
    send(&mut app, Message::SnapshotsReady(vec![(0, stale)]));

    assert!(
        !app.rasters.text.contains_key(&0) && !app.rasters.staging.contains_key(&0),
        "a matched node must keep drawing live so its highlight shows"
    );

    // Once the search is over it rasterizes again like any other card.
    send(&mut app, Message::CloseSearch);
    let fresh = text_raster::snapshot_markdown(
        "ThinkBoard", 200.0, 100.0, 180.0, 17.0, app.theme, false, 1.0,
    )
    .expect("a bitmap");
    send(&mut app, Message::SnapshotsReady(vec![(0, fresh)]));
    assert!(app.rasters.text.contains_key(&0));
}

#[test]
fn links_are_drawn_obsidians_way_unless_asked_otherwise() {
    // A board is an Obsidian document; its links should look like Obsidian's
    // out of the box.
    let mut app = app_with(two_nodes(), Vec::new());
    assert_eq!(crate::config::LinkStyle::of(&app.config), crate::config::LinkStyle::Obsidian);

    // The form offers labels and the file keeps names, so picking one has to
    // survive the round trip between them.
    for style in crate::config::LinkStyle::ALL {
        send(
            &mut app,
            Message::Settings(settings::Msg::Pick(
                settings::Field::LinkStyle,
                style.label().to_owned(),
            )),
        );
        assert_eq!(app.config.link_style, style.as_str());
        assert_eq!(crate::config::LinkStyle::of(&app.config), style);
    }
}

// --- The board's file, changed by another program ---

/// An app with a board open from a real file in a folder of its own, as
/// [`app_with`] but saved, watched and merged like a configured board.
fn file_app(name: &str, nodes: Vec<CanvasNode>) -> (App, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tb-disk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Trip.canvas");
    let mut app = app_with(nodes, Vec::new());
    app.current_document().save(&path).unwrap();
    app.canvas_path = path.clone();
    app.disk = sync::OnDisk::new(app.current_document(), &path);
    app.scratch = false;
    (app, path)
}

/// Changes the file the way Obsidian does: reads it, changes it, writes it
/// back whole.
fn elsewhere(path: &std::path::Path, change: impl FnOnce(&mut CanvasDocument)) {
    // A write within the same tick as the last would leave the time alone, and
    // so look like nothing happened — and Windows stamps files from a clock
    // that ticks every 15 ms or so.
    std::thread::sleep(Duration::from_millis(20));
    let mut document = CanvasDocument::load(path).unwrap();
    change(&mut document);
    document.save(path).unwrap();
}

/// Puts the editor's caret after the last character.
fn caret_to_end(app: &mut App) {
    send(app, Message::EditorAction(text_editor::Action::Move(text_editor::Motion::DocumentEnd)));
}

/// Runs the autosave as if its debounce had passed.
fn autosave(app: &mut App) {
    app.last_edit = Instant::now() - Duration::from_secs(5);
    send(app, Message::SaveTick);
}

#[test]
fn a_card_added_there_while_one_is_edited_here_survives_the_save() {
    // What the tour found: the card added in Obsidian was written over by the
    // edit's autosave, and gone from the file.
    let (mut app, path) = file_app("editing", two_nodes());
    send(&mut app, Message::StartEditing { index: 1, at: None });
    caret_to_end(&mut app);
    type_text(&mut app, " near Alfama");
    elsewhere(&path, |doc| doc.nodes.push(text_node("c", "Buy tram passes")));
    send(&mut app, Message::FileChanged);

    assert_eq!(ids(&app), ["a", "b", "c"]);
    let editing = app.editing.as_ref().expect("still editing");
    assert_eq!(app.nodes[editing.index].id, "b");
    assert_eq!(editing.content.text(), "node b near Alfama", "the edit is untouched");

    send(&mut app, Message::StopEditing);
    autosave(&mut app);
    let saved = CanvasDocument::load(&path).unwrap();
    let texts: Vec<_> = saved.nodes.iter().filter_map(|n| n.text.as_deref()).collect();
    assert_eq!(texts, ["node a", "node b near Alfama", "Buy tram passes"]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_save_takes_in_a_change_the_watcher_has_not_reported_yet() {
    // FSEvents can deliver a change a second after it's made: a save in that
    // window used to write over it.
    let (mut app, path) = file_app("unreported", two_nodes());
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 300.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    elsewhere(&path, |doc| doc.nodes.push(text_node("c", "Buy tram passes")));

    autosave(&mut app);
    assert_eq!(ids(&app), ["a", "b", "c"], "taken in before saving");
    autosave(&mut app);
    let saved = CanvasDocument::load(&path).unwrap();
    assert_eq!(saved.nodes.len(), 3);
    assert_eq!(saved.nodes[0].y, 300.0, "and the move here saved with it");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn an_outside_change_is_a_step_undo_takes_back() {
    // The history used to be dropped on any outside write, so Cmd+Z after a
    // sync did nothing.
    let (mut app, path) = file_app("undo", two_nodes());
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::SetNodeColor(Some("4".to_owned())));
    autosave(&mut app);
    elsewhere(&path, |doc| doc.nodes[1].x = 900.0);
    send(&mut app, Message::FileChanged);
    assert_eq!(app.nodes[1].x, 900.0);

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[1].x, 400.0, "the outside change is undone first");
    assert!(app.doc_dirty, "and the undo is saved back");
    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[0].color, None, "then what was done here before it");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn undoing_an_edit_made_across_an_outside_change_keeps_the_change() {
    let (mut app, path) = file_app("across", two_nodes());
    send(&mut app, Message::StartEditing { index: 1, at: None });
    caret_to_end(&mut app);
    type_text(&mut app, " near Alfama");
    elsewhere(&path, |doc| doc.nodes.push(text_node("c", "Buy tram passes")));
    send(&mut app, Message::FileChanged);
    send(&mut app, Message::StopEditing);

    send(&mut app, Message::Undo);
    assert_eq!(text_of(&app, 1), Some("node b"), "the edit is undone");
    assert_eq!(ids(&app), ["a", "b", "c"], "and the card added there stays");
    send(&mut app, Message::Undo);
    assert_eq!(ids(&app), ["a", "b"]);
    assert_eq!(text_of(&app, 1), Some("node b"), "without bringing half the edit back");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_editor_follows_a_card_changed_there_before_anything_was_typed() {
    let (mut app, path) = file_app("follow", two_nodes());
    send(&mut app, Message::StartEditing { index: 1, at: None });
    elsewhere(&path, |doc| doc.nodes[1].text = Some("node b, reworded there".to_owned()));
    send(&mut app, Message::FileChanged);
    let editing = app.editing.as_ref().expect("still editing");
    assert_eq!(editing.content.text(), "node b, reworded there");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_selection_survives_an_outside_change() {
    let (mut app, path) = file_app("selection", two_nodes());
    send(&mut app, Message::SelectNode(Some(1)));
    // A card added in front of it, so its index moves.
    elsewhere(&path, |doc| doc.nodes.insert(0, text_node("first", "Packing")));
    send(&mut app, Message::FileChanged);
    assert_eq!(ids(&app), ["first", "a", "b"]);
    assert_eq!(app.selected, [2], "still b");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_board_renamed_there_is_saved_under_its_new_name() {
    // It used to be written back under its old name, and the folder held both.
    let (mut app, path) = file_app("rename", two_nodes());
    let renamed = path.with_file_name("Lisbon.canvas");
    std::fs::rename(&path, &renamed).unwrap();
    send(&mut app, Message::FileChanged);
    assert_eq!(app.canvas_path, renamed);
    assert!(!app.disk.missing);

    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 300.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    autosave(&mut app);
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, ["Lisbon.canvas"]);
    assert_eq!(CanvasDocument::load(&renamed).unwrap().nodes[0].y, 300.0);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_board_whose_file_went_is_not_saved_until_told_to() {
    let (mut app, path) = file_app("gone", two_nodes());
    std::fs::remove_file(&path).unwrap();
    send(&mut app, Message::FileChanged);
    assert!(app.disk.missing);

    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 300.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    autosave(&mut app);
    assert!(!path.exists(), "not quietly recreated");
    assert!(app.doc_dirty);

    send(&mut app, Message::RestoreFile);
    assert!(!app.disk.missing);
    assert_eq!(CanvasDocument::load(&path).unwrap().nodes[0].y, 300.0);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_file_left_unreadable_is_saved_over_rather_than_waited_on() {
    let (mut app, path) = file_app("unreadable", two_nodes());
    send(&mut app, Message::MoveNode { index: 0, position: Point::new(0.0, 300.0) });
    send(&mut app, Message::NodeDragEnd { index: 0, moved: true });
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "{\"nodes\":[{\"id\":").unwrap();

    // Just written: maybe half of something, so nothing is saved over it.
    autosave(&mut app);
    autosave(&mut app);
    assert!(CanvasDocument::load(&path).is_err());

    // Left that way: it was finished broken, and the board is saved.
    let old = std::time::SystemTime::now() - Duration::from_secs(10);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
    autosave(&mut app);
    autosave(&mut app);
    assert_eq!(CanvasDocument::load(&path).unwrap().nodes[0].y, 300.0);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn what_another_app_copied_is_pasted_whole_and_in_view() {
    // One test, since the clipboard stand-in is shared by the whole binary.
    let mut app = app_with(two_nodes(), Vec::new());
    app.camera.viewport = Size::new(1200.0, 800.0);

    // A paragraph: sized to its text, not three lines of it.
    let paragraph = "Lisbon's trams are part of the city: the 28 climbs from Martim Moniz through Graça and \
                     Alfama to Estrela, past most of what a first visit wants to see, in old wooden cars \
                     that rattle and lean on every corner. Ride it early, before the queues.";
    *clipboard::system::held() = Some(clipboard::Clip::Text(paragraph.to_owned()));
    // Asked for below the window's edge, as a nudge to a free spot can land.
    let below = app.camera.to_world(Point::new(600.0, 1100.0));
    send(&mut app, Message::Paste { world: below });
    let pasted = app.nodes.last().unwrap().clone();
    let wrap = text_raster::card_text_wrap(pasted.width);
    let (_, needs) = text_raster::measure_markdown(paragraph, wrap, FONT_SIZE, &app.theme);
    assert!(pasted.height >= needs + text_raster::NODE_PAD_Y - 0.5, "{} for {needs}", pasted.height);

    // And the view follows it, at the zoom it was at.
    let focus = app.camera_focus.expect("scrolled to it");
    assert_eq!(focus.target_zoom, app.camera.zoom);
    let mut camera = app.camera;
    camera.offset = focus.target_offset;
    let screen = camera.rect_to_screen(arrange::node_world_rect(&pasted));
    assert!(screen.y >= 0.0 && screen.y + screen.height <= 800.0, "{screen:?}");

    // Too tall for the window, it's shown from its top, which is where it's
    // read from — centred, its first lines were off the screen.
    let long = "A paragraph of the guide, long enough to wrap twice in a card.\n\n".repeat(30);
    *clipboard::system::held() = Some(clipboard::Clip::Text(long));
    app.camera_focus = None;
    send(&mut app, Message::Paste { world: below });
    let pasted = app.nodes.last().unwrap().clone();
    assert!(pasted.height > 800.0, "taller than the window: {}", pasted.height);
    let mut camera = app.camera;
    camera.offset = app.camera_focus.expect("scrolled to it").target_offset;
    assert_eq!(camera.rect_to_screen(arrange::node_world_rect(&pasted)).y, 24.0);

    // An image on the welcome board is held, not written where the app runs.
    let mut app = app_with(Vec::new(), Vec::new());
    *clipboard::system::held() = Some(clipboard::Clip::Image { width: 4, height: 2, rgba: vec![200; 32] });
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    let file = app.nodes[0].file.clone().expect("an image node");
    assert!(app.held_images.contains_key(&file));
    assert!(!std::path::Path::new(&file).exists(), "nothing written to the working directory");
    // An undo and redo rebuild the image handles; it's still there after.
    send(&mut app, Message::Undo);
    send(&mut app, Message::Redo);
    assert!(app.images.contains_key(&app.nodes[0].id));

    // Copied from there onto a board with a folder, it's written there.
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::CopyNodes);
    let (_, path) = file_app("held-image", Vec::new());
    let _ = app.open_canvas(path.clone());
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    let file = app.nodes[0].file.clone().expect("an image node");
    assert!(path.parent().unwrap().join(&file).exists(), "{file} beside the board");
    *clipboard::system::held() = None;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// --- The board by keyboard ---

/// A card left, two to its right — one up, one down — as in the tour's trip.
fn plan() -> Vec<CanvasNode> {
    vec![
        CanvasNode { x: -520.0, y: -60.0, width: 300.0, height: 120.0, ..text_node("trip", "Trip") },
        CanvasNode { x: -120.0, y: -200.0, width: 240.0, height: 60.0, ..text_node("eat", "Where should we eat?") },
        CanvasNode { x: -120.0, y: 140.0, width: 240.0, height: 60.0, ..text_node("sintra", "Sintra") },
    ]
}

fn selected_ids(app: &App) -> Vec<&str> {
    app.selected.iter().map(|&i| app.nodes[i].id.as_str()).collect()
}

#[test]
fn the_arrows_move_the_selection_from_card_to_card() {
    use thinkboard_core::navigate::Toward;
    let mut app = app_with(plan(), Vec::new());
    app.camera.viewport = Size::new(1440.0, 900.0);
    // Nothing selected: the card nearest the middle of the view.
    app.camera.offset = Vector::new(370.0, 0.0); // the trip at the middle
    send(&mut app, Message::SelectToward { toward: Toward::Right, extend: false });
    assert_eq!(selected_ids(&app), ["trip"]);
    send(&mut app, Message::SelectToward { toward: Toward::Right, extend: false });
    assert_eq!(selected_ids(&app), ["eat"]);
    send(&mut app, Message::SelectToward { toward: Toward::Down, extend: false });
    assert_eq!(selected_ids(&app), ["sintra"]);
    // Shift adds to it rather than moving it.
    send(&mut app, Message::SelectToward { toward: Toward::Up, extend: true });
    assert_eq!(selected_ids(&app), ["sintra", "eat"]);
}

#[test]
fn enter_edits_at_the_end_and_esc_leaves_the_card_selected() {
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::EditSelected);
    let editing = app.editing.as_ref().expect("editing");
    let cursor = editing.content.cursor().position;
    assert_eq!((cursor.line, cursor.column), (0, "Where should we eat?".len()), "the caret at the end");
    type_text(&mut app, " Near Alfama.");
    send(&mut app, Message::FinishEditing);
    assert_eq!(text_of(&app, 1), Some("Where should we eat? Near Alfama."));
    assert_eq!(selected_ids(&app), ["eat"], "where you are after Esc");
}

#[test]
fn tab_adds_a_linked_note_beside_the_selected_card() {
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::AddLinkedNote);
    assert_eq!(app.nodes.len(), 4);
    let (eat, new) = (&app.nodes[1], &app.nodes[3]);
    assert!(new.x > eat.x + eat.width, "to its right");
    assert!(app.edges.iter().any(|e| e.from_node == "eat" && e.to_node == new.id), "linked from it");
    assert_eq!(selected_ids(&app), [new.id.as_str()]);
    // Its editor opens as a drag-created note's does; written and closed with
    // Esc, it stays selected for the next Tab.
    send(&mut app, Message::StartEditing { index: 3, at: None });
    type_text(&mut app, "Tascas");
    send(&mut app, Message::FinishEditing);
    assert_eq!(text_of(&app, 3), Some("Tascas"));
    assert_eq!(app.selected, [3]);
}

#[test]
fn a_run_of_nudges_is_one_undo_step() {
    use thinkboard_core::navigate::Toward;
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::SelectNode(Some(1)));
    for _ in 0..3 {
        send(&mut app, Message::NudgeSelection(Toward::Right));
    }
    assert_eq!(app.nodes[1].x, -120.0 + 30.0);
    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[1].x, -120.0, "all three at once");
    assert!(!app.history.can_undo());
    assert_eq!(selected_ids(&app), ["eat"], "and still on the card that moved");
}

#[test]
fn a_click_on_the_board_puts_find_away() {
    // On a card: the bar goes, and the card clicked stays the selection —
    // not the hit, as Esc would select.
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("sintra".to_owned()));
    let eat = app.nodes.iter().position(|node| node.id == "eat").expect("eat");
    send(&mut app, Message::SelectNode(Some(eat)));
    send(&mut app, Message::NodeDragEnd { index: eat, moved: true });
    assert!(app.search.is_some(), "a drag is looking around, not a click");
    send(&mut app, Message::NodeDragEnd { index: eat, moved: false });
    assert!(app.search.is_none());
    assert_eq!(selected_ids(&app), ["eat"]);

    // On a link, the same.
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("sintra".to_owned()));
    send(&mut app, Message::SelectEdge(Some(0)));
    assert!(app.search.is_none());
    assert_eq!(app.selected_edge, Some(0));
}

#[test]
fn closing_find_selects_what_it_found() {
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::OpenSearch);
    send(&mut app, Message::SearchInput("sintra".to_owned()));
    send(&mut app, Message::CloseSearch);
    assert_eq!(selected_ids(&app), ["sintra"]);
}

