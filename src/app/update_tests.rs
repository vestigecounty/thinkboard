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
fn a_tasks_box_toggles_in_the_cards_text_as_one_undo_step() {
    let mut app = app_with(vec![text_node("a", "Before we go\n\n- [x] ryokan\n- [ ] rail passes")], vec![]);
    send(&mut app, Message::ToggleTask { index: 0, task: 1 });
    assert_eq!(app.nodes[0].text.as_deref(), Some("Before we go\n\n- [x] ryokan\n- [x] rail passes"));
    assert!(app.doc_dirty, "saved as any edit is");
    send(&mut app, Message::ToggleTask { index: 0, task: 0 });
    assert_eq!(app.nodes[0].text.as_deref(), Some("Before we go\n\n- [ ] ryokan\n- [x] rail passes"));

    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[0].text.as_deref(), Some("Before we go\n\n- [x] ryokan\n- [x] rail passes"));

    // A task the card hasn't got changes nothing, and records no step.
    send(&mut app, Message::ToggleTask { index: 0, task: 5 });
    send(&mut app, Message::Undo);
    assert_eq!(app.nodes[0].text.as_deref(), Some("Before we go\n\n- [x] ryokan\n- [ ] rail passes"));
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
fn a_new_card_and_its_first_text_are_one_undo_step() {
    // Adding a card wasn't undoable: its first edit skipped the snapshot, so
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

    // Two distant cards stay two clusters, and the frame joins neither.
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

#[test]
fn a_cluster_drag_carries_the_groups_wholly_in_it() {
    // A cluster's members are cards, never frames, so its groups were left
    // where they were as their cards went.
    let frame = |id: &str, x: f32, y: f32| CanvasNode { x, y, width: 400.0, height: 300.0, ..node_of(id, "group") };
    let card = |id: &str, x: f32, y: f32| CanvasNode { x, y, ..text_node(id, id) };
    let mut app = app_with(
        vec![
            frame("whole", 0.0, 0.0),
            card("a", 20.0, 60.0),
            card("b", 150.0, 160.0),
            frame("split", 600.0, 0.0),
            card("c", 620.0, 60.0),
            card("d", 750.0, 160.0),
            // Empty, and in the way of the drag below.
            frame("empty", 0.0, 500.0),
        ],
        Vec::new(),
    );
    app.clusters.list = vec![crate::cluster::Cluster {
        bounds: Rectangle::new(Point::ORIGIN, Size::new(900.0, 300.0)),
        text: "a".to_owned(),
        font_size: 20.0,
        image_id: None,
        members: vec![1, 2, 4],
    }];
    let before = app.nodes.clone();

    // Down over the empty group, in two moves as a drag makes many.
    send(&mut app, Message::MoveCluster { index: 0, delta: Vector::new(0.0, 300.0) });
    send(&mut app, Message::MoveCluster { index: 0, delta: Vector::new(0.0, 300.0) });
    send(&mut app, Message::ClusterDragEnd);

    let moved = |i: usize| app.nodes[i].y - before[i].y;
    assert_eq!([moved(0), moved(1), moved(2)], [600.0; 3], "the group whose cards all went, with them");
    assert_eq!(moved(4), 600.0, "the split group's card in the cluster");
    assert_eq!([moved(3), moved(5)], [0.0; 2], "the split group stays, with its card outside the cluster");
    assert_eq!(moved(6), 0.0, "an empty group dragged across isn't picked up");
    assert!(app.clusters.carried.is_none(), "forgotten when the drag ends");
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

    // Named as Obsidian names one, and selected, so typing replaces it.
    let editing = app.editing.as_ref().expect("the caption editor should be open");
    assert_eq!(editing.content.selection().as_deref(), Some(crate::model::UNTITLED_GROUP));
    type_text(&mut app, "Planning");
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes[frame].label(), Some("Planning"));
}

#[test]
fn a_new_group_left_as_it_opened_is_an_untitled_group() {
    // Unlike a drag-created card, a frame left unnamed is not taken back.
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);

    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes.len(), 3, "the frame survives");
    assert_eq!(app.nodes[frame].label(), Some(crate::model::UNTITLED_GROUP));
}

#[test]
fn return_ends_a_caption_on_one_line() {
    // Enter saves a group's name in Obsidian; it used to start a second line.
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    type_text(&mut app, "Plan");
    press_return(&mut app);

    assert!(app.editing.is_none(), "Return ends the edit");
    assert_eq!(app.nodes[frame].label(), Some("Plan"));
    assert_eq!(app.selected, vec![frame], "and leaves the frame selected");
    assert_eq!(app.nodes.len(), 3, "no card is added");
}

#[test]
fn a_line_break_pasted_into_a_caption_is_a_space() {
    let mut app = app_with(two_nodes(), Vec::new());
    let frame = grouped(&mut app);
    type_text(&mut app, "Trip\n  planning\n");
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes[frame].label(), Some("Trip planning"));
}

#[test]
fn a_frame_another_tool_left_unnamed_stays_so_when_nothing_is_typed() {
    // A double-click on its header and an Esc is no change to the file.
    let mut app = app_with(vec![node_of("frame", "group")], Vec::new());
    send(&mut app, Message::StartEditing { index: 0, at: None });
    send(&mut app, Message::StopEditing);

    assert_eq!(app.nodes[0].label(), None);
    assert!(!app.nodes[0].extra.contains_key("label"));
    assert!(!app.history.can_undo(), "and no undo step");
}

#[test]
fn a_caption_cleared_goes_back_to_untitled_group() {
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

    assert_eq!(app.nodes[frame].label(), Some(crate::model::UNTITLED_GROUP));
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
fn an_answer_about_a_card_still_prefers_the_space_beside_it() {
    // The frame exclusion is scoped to frames: a card's answer keeps landing
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
    // The exclusion must not leak into ordinary placement — dropping a card
    // into a group is the normal way to add one to it.
    let mut app = app_with(Vec::new(), Vec::new());
    let mut frame = node_of("frame", "group");
    frame.width = 800.0;
    frame.height = 600.0;
    app.nodes.push(frame);

    let inside = Point::new(400.0, 300.0);
    assert_eq!(app.find_free_placement(inside, 250.0, 120.0), inside);
}

// --- Auto-layout over groups ---

/// A group of three cards in a column, linked down it, and two cards well
/// away from it: `in` linked to the first, the last linked to `out`.
fn a_group_between_two_cards() -> Vec<CanvasNode> {
    let mut frame = node_of("frame", "group");
    (frame.width, frame.height) = (260.0, 420.0);
    let card = |id: &str, y: f32| CanvasNode { x: 30.0, y, ..text_node(id, id) };
    vec![
        frame,
        card("m1", 50.0),
        card("m2", 170.0),
        card("m3", 290.0),
        CanvasNode { x: -1200.0, y: 600.0, ..text_node("in", "in") },
        CanvasNode { x: 1200.0, y: -600.0, ..text_node("out", "out") },
    ]
}

fn chain_through_the_group() -> Vec<CanvasEdge> {
    vec![edge("in", "m1"), edge("m1", "m2"), edge("m2", "m3"), edge("m3", "out")]
}

/// Runs the layout's easing to its end.
fn finish_layout(app: &mut App) {
    send(app, Message::Tick(Instant::now() + std::time::Duration::from_secs(10)));
    assert!(app.layout_anim.is_none());
}

fn inside(node: &CanvasNode, frame: &CanvasNode) -> bool {
    node.x >= frame.x
        && node.y >= frame.y
        && node.x + node.width <= frame.x + frame.width
        && node.y + node.height <= frame.y + frame.height
}

#[test]
fn auto_layout_takes_a_group_whole_rather_than_pulling_its_cards_out() {
    // A marquee over a group and the cards beside it selects the group's
    // cards, never its frame. Laid out as loose cards, they left the frame.
    let mut edges = chain_through_the_group();
    // A link inside the group, between ports a top-down layout wouldn't pick.
    edges[1].from_side = "right".to_owned();
    edges[1].to_side = "right".to_owned();
    let mut app = app_with(a_group_between_two_cards(), edges);
    let before = app.nodes.clone();

    send(&mut app, Message::SelectNodes(vec![1, 2, 3, 4, 5]));
    send(&mut app, Message::AutoLayout(layout::Direction::Down));
    finish_layout(&mut app);

    let frame = &app.nodes[0];
    assert_ne!((frame.x, frame.y), (before[0].x, before[0].y), "the group moved");
    for m in 1..=3 {
        assert!(inside(&app.nodes[m], frame), "{} stays in the group", app.nodes[m].id);
        assert_eq!(app.nodes[m].x - frame.x, before[m].x - before[0].x, "and where it was in it");
        assert_eq!(app.nodes[m].y - frame.y, before[m].y - before[0].y);
    }
    // It's ranked by its cards' links: `in` above it, `out` below.
    assert!(app.nodes[4].y + app.nodes[4].height < frame.y);
    assert!(frame.y + frame.height < app.nodes[5].y);
    // Links into it are re-anchored, the one inside it left as it was.
    assert_eq!((app.edges[0].from_side.as_str(), app.edges[0].to_side.as_str()), ("bottom", "top"));
    assert_eq!((app.edges[1].from_side.as_str(), app.edges[1].to_side.as_str()), ("right", "right"));

    send(&mut app, Message::Undo);
    let place = |nodes: &[CanvasNode]| nodes.iter().map(|n| (n.x, n.y)).collect::<Vec<_>>();
    assert_eq!(place(&app.nodes), place(&before), "one undo step");
}

#[test]
fn auto_layout_inside_a_group_grows_it_to_hold_the_result() {
    // The second step: the group's own cards, now side by side.
    let mut app = app_with(a_group_between_two_cards(), vec![edge("m1", "m2"), edge("m2", "m3")]);
    let before = app.nodes[0].clone();

    send(&mut app, Message::SelectNodes(vec![1, 2, 3]));
    send(&mut app, Message::AutoLayout(layout::Direction::Right));
    finish_layout(&mut app);

    let frame = &app.nodes[0];
    assert!(app.nodes[1].x < app.nodes[2].x && app.nodes[2].x < app.nodes[3].x, "laid out left to right");
    assert!(frame.width > before.width, "grown to fit a row wider than it was");
    for member in &app.nodes[1..=3] {
        assert!(inside(member, frame), "{} stays in the group", member.id);
    }
    assert!(frame.x <= before.x && frame.y <= before.y, "grown, never moved inward");
    assert!(frame.x + frame.width >= before.x + before.width);
    assert!(frame.y + frame.height >= before.y + before.height);
}

#[test]
fn a_link_from_a_card_left_out_of_the_layout_meets_the_side_facing_it() {
    // The group's cards laid out top-down, `in` far off to the left and below,
    // `out` to the right and above. By the flow, `in` came up into m1's
    // bottom, the port m1 -> m2 leaves by, and m3 left by its top for `out`.
    let mut app = app_with(a_group_between_two_cards(), chain_through_the_group());
    send(&mut app, Message::SelectNodes(vec![1, 2, 3]));
    send(&mut app, Message::AutoLayout(layout::Direction::Down));
    finish_layout(&mut app);

    let sides = |i: usize| (app.edges[i].from_side.as_str(), app.edges[i].to_side.as_str());
    assert_eq!(sides(0), ("right", "left"), "in -> m1, side to side");
    assert_eq!(sides(1), ("bottom", "top"), "m1 -> m2, in the flow");
    assert_eq!(sides(2), ("bottom", "top"));
    assert_eq!(sides(3), ("right", "left"), "m3 -> out, side to side");
}

#[test]
fn a_card_added_beside_a_group_lays_the_group_out_whole() {
    // "Auto-layout when adding a linked card" lays out the whole graph the
    // new card joins, which runs into the group here.
    let mut app = app_with(a_group_between_two_cards(), chain_through_the_group());
    app.config.auto_layout = true;
    let before = app.nodes.clone();

    send(&mut app, Message::SelectNode(Some(4)));
    send(&mut app, Message::AddLinkedNode);
    finish_layout(&mut app);

    assert_eq!(app.nodes.len(), 7, "a card was added");
    let frame = &app.nodes[0];
    for m in 1..=3 {
        assert!(inside(&app.nodes[m], frame), "{} stays in the group", app.nodes[m].id);
        assert_eq!(app.nodes[m].x - frame.x, before[m].x - before[0].x);
        assert_eq!(app.nodes[m].y - frame.y, before[m].y - before[0].y);
    }
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
    // `max_card_width` (420) bounds how wide a card grows by itself. It must
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

/// A card past the height ceiling, in sections, with `word` once near its end.
fn long_node(word: &str) -> CanvasNode {
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
        app.config.center_card_text,
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
    let mut app = app_with(vec![long_node("Cacilhas")], Vec::new());
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
    // edge, in the part of the card it hadn't scrolled to.
    let mut app = app_with(vec![long_node("Cacilhas")], Vec::new());
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
    // A card Obsidian left smaller than its text grew over the one below on a
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
    // Height tracks content — otherwise deleting a long card leaves a tall
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

// --- Scrolling a long card ---

#[test]
fn a_cards_scroll_steps_add_up_and_stop_at_its_ends() {
    let long = "A line of the guide, long enough to need a scroll.\n\n".repeat(40);
    let mut app = app_with(vec![CanvasNode { height: 300.0, ..text_node("guide", &long) }], Vec::new());
    app.config.card_scrollbars = true;
    let scroll = |app: &App| board::scroll_offset(&app.config, &app.theme, &app.node_scroll, &app.nodes[0]);
    let by = |by: f32| Message::ScrollNodeBy { id: "guide".to_owned(), by };
    // Two from one batch, each worked out before the other landed: both count.
    send(&mut app, by(50.0));
    send(&mut app, by(50.0));
    assert_eq!(scroll(&app), 100.0);
    // Far past the end, and straight back from it rather than from where the
    // step would have taken it.
    send(&mut app, by(1e6));
    let end = scroll(&app);
    assert!(end > 100.0 && end < 1e6, "at its end: {end}");
    send(&mut app, by(-10.0));
    assert_eq!(scroll(&app), end - 10.0);
    send(&mut app, by(-1e6));
    assert_eq!(scroll(&app), 0.0);
}

// --- Editing overlay bounds ---

/// The card's on-screen rectangle, which the editing overlay has to stay within.
fn card_screen_rect(app: &App, index: usize) -> iced::Rectangle {
    app.camera
        .rect_to_screen(arrange::node_world_rect(&app.nodes[index]))
}

#[test]
fn the_editor_never_extends_past_the_card_it_edits() {
    // A card with far more text than it can show. The editor used to be
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
fn a_short_card_still_gets_an_editor_that_hugs_its_text() {
    // The clamp must not become "always fill the card": a short card's editor
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

/// The palette with nothing typed: its rows by id, and its sections' labels.
fn browsed(app: &App) -> (Vec<command::CommandId>, Vec<&'static str>) {
    let listing = command::listing(app, "");
    (listing.entries.iter().filter_map(command::Entry::command).collect(), listing.headers.iter().map(|(_, label)| *label).collect())
}

#[test]
fn the_empty_palette_is_a_menu_of_what_can_run_now() {
    use command::CommandId;
    let mut app = app_with(two_nodes(), Vec::new());
    let (ids, sections) = browsed(&app);
    // AI waits on a provider, and nothing is selected: neither is listed —
    // but for the way to a provider.
    assert_eq!(sections, ["Board", "Edit", "Appearance", "AI", "App"]);
    let ai = |ids: &[CommandId]| ids.iter().filter(|id| matches!(id, CommandId::ChooseProvider | CommandId::ChooseModel | CommandId::AskAi)).copied().collect::<Vec<_>>();
    assert_eq!(ai(&ids), [CommandId::ChooseProvider]);
    assert!(!ids.contains(&CommandId::EditNode));
    assert!(ids.iter().all(|id| id.is_enabled(&app)), "a menu lists nothing it would grey");
    assert!(!ids.iter().any(|id| matches!(id, CommandId::Colour(_))), "the colours are for typing");

    // A selection puts its own commands first, and a provider brings AI.
    app.selected = vec![0];
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    let (ids, sections) = browsed(&app);
    assert_eq!(sections, ["Selection", "Board", "Edit", "Appearance", "AI", "App"]);
    assert_eq!(ids.first(), Some(&CommandId::EditNode));
    assert!(ids.contains(&CommandId::AskAi));
}

#[test]
fn what_was_run_last_comes_first_and_only_once() {
    use command::CommandId;
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::RunCommand(CommandId::ChooseFont));
    send(&mut app, Message::RunCommand(CommandId::FitBoard));
    let (ids, sections) = browsed(&app);
    assert_eq!(sections.first(), Some(&"Recent commands"));
    assert_eq!(ids[..2], [CommandId::FitBoard, CommandId::ChooseFont], "latest first");
    assert_eq!(ids.iter().filter(|&&id| id == CommandId::FitBoard).count(), 1, "not again under Board");

    // A setting run from a search comes back too, though the menu lists none.
    send(&mut app, Message::RunCommand(CommandId::Setting(settings::Field::SnapToGrid)));
    assert_eq!(browsed(&app).0.first(), Some(&CommandId::Setting(settings::Field::SnapToGrid)));
}

#[test]
fn enter_runs_the_row_highlighted_under_the_headers() {
    // The headers take slots in the list but not in the selection: the first
    // row is the first command, under the first heading.
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenCommandPalette);
    send(&mut app, Message::PanelNav(picker::Panel::Command, picker::Nav::Step(1)));
    let highlighted = command::filtered(&app, "")[1].command();
    assert_eq!(highlighted, Some(command::CommandId::FitBoard));
    send(&mut app, Message::RunSelectedCommand);
    assert_eq!(app.recent_commands.first().copied(), highlighted);
}

#[test]
fn a_search_lists_what_cant_run_last_and_says_why() {
    let app = app_with(two_nodes(), Vec::new());
    // "card" finds the selection's commands, which can't run with nothing
    // selected, and "Add card", which can — first, whatever the ranking.
    let found = command::filtered(&app, "card");
    let first_waiting = found.iter().position(|entry| !entry.is_enabled(&app)).expect("one is waiting");
    assert!(found[..first_waiting].iter().any(|entry| entry.command() == Some(command::CommandId::AddNode)));
    assert!(found[first_waiting..].iter().all(|entry| !entry.is_enabled(&app)));
    assert!(command::listing(&app, "card").headers.is_empty(), "a search has no sections");
}

// --- Boards in the palette ---

/// A vault of real, empty boards at `relative` paths, for tests that open
/// them — the palette offers a recent board only while its file is there.
fn board_vault(name: &str, relative: &[&str]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("tb-boards-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for path in relative {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().expect("a folder")).unwrap();
        std::fs::write(&file, r#"{"nodes":[],"edges":[]}"#).unwrap();
    }
    root
}

/// What the palette lists for `query`: each row's title and detail, and its
/// sections' labels.
fn palette_rows(app: &App, query: &str) -> (Vec<(String, String)>, Vec<&'static str>) {
    let listing = command::listing(app, query);
    let rows = listing.entries.iter().map(|entry| (entry.title.to_string(), entry.detail.to_string())).collect();
    (rows, listing.headers.iter().map(|(_, label)| *label).collect())
}

#[test]
fn the_boards_opened_last_come_first_and_enter_switches_back() {
    let boards = ["Work/Kanban.canvas", "Reading.canvas", "Trips/Lisbon.canvas"];
    let root = board_vault("recent", &boards);
    let mut app = app_with(Vec::new(), Vec::new());
    app.config.vault = root.to_string_lossy().into_owned();
    for board in boards {
        send(&mut app, Message::OpenListedBoard(root.join(board)));
    }
    send(&mut app, Message::RunCommand(command::CommandId::FitBoard));
    send(&mut app, Message::OpenCommandPalette);

    let (rows, sections) = palette_rows(&app, "");
    assert_eq!(sections[..2], ["Recent boards", "Recent commands"]);
    // Latest first, by the folder they're in — and not the one that's open,
    // which there's nothing to switch to.
    let named = |name: &str, folder: &str| (name.to_owned(), folder.to_owned());
    assert_eq!(rows[..3], [named("Reading", ""), named("Kanban", "Work"), named("Fit board to view", "")]);
    let state = app.command_palette.as_ref().expect("the palette");
    assert_eq!(command::rows(&app, state).0[0].glyph, Some(chrome::Glyph::Board), "marked as a board");

    // So the board before this one is the palette and Enter away, and the
    // same again comes back.
    send(&mut app, Message::RunSelectedCommand);
    assert_eq!(app.canvas_path, root.join("Reading.canvas"));
    assert!(app.command_palette.is_none());
    send(&mut app, Message::OpenCommandPalette);
    send(&mut app, Message::RunSelectedCommand);
    assert_eq!(app.canvas_path, root.join("Trips/Lisbon.canvas"));
    assert_eq!(app.recent_commands.first(), Some(&command::CommandId::FitBoard), "opening a board isn't a command");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_board_since_deleted_or_moved_isnt_offered() {
    let root = board_vault("gone", &["Old.canvas", "New.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    send(&mut app, Message::OpenListedBoard(root.join("Old.canvas")));
    send(&mut app, Message::OpenListedBoard(root.join("New.canvas")));
    assert_eq!(palette_rows(&app, "").1.first(), Some(&"Recent boards"));

    std::fs::remove_file(root.join("Old.canvas")).unwrap();
    assert_ne!(palette_rows(&app, "").1.first(), Some(&"Recent boards"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_board_launched_on_counts_as_opened() {
    let launched = PathBuf::from("/boards/Launch.canvas");
    let app = App::build(Init {
        config: config::Config::default(),
        theme: ColorTheme::default(),
        document: CanvasDocument { nodes: Vec::new(), edges: Vec::new(), extra: serde_json::Map::new() },
        canvas_path: launched.clone(),
        base_path: PathBuf::from("/boards"),
        scratch: false,
        hotkey_manager: None,
        registered_hotkey: None,
        windowed: true,
    });
    assert_eq!(app.config.recent_boards, [launched.to_string_lossy()]);
}

// --- Back and forward through the boards visited ---

/// A vault of boards each holding one card, at a place of its own — so each,
/// fitted to the view, is framed differently.
fn boards_with_a_card(name: &str, boards: &[&str]) -> PathBuf {
    let root = board_vault(name, boards);
    for (at, board) in boards.iter().enumerate() {
        let x = 1000 * at;
        std::fs::write(
            root.join(board),
            format!(r#"{{"nodes":[{{"id":"n","type":"text","text":"{board}","x":{x},"y":0,"width":250,"height":60}}],"edges":[]}}"#),
        )
        .unwrap();
    }
    root
}

#[test]
fn a_board_opens_where_it_was_left_however_it_is_gone_back_to() {
    let root = boards_with_a_card("camera", &["A.canvas", "B.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    send(&mut app, Message::OpenListedBoard(root.join("A.canvas")));
    // Somewhere on it, zoomed in.
    app.camera.offset = iced::Vector::new(-321.0, 123.0);
    app.camera.zoom = 2.5;

    // A board not yet seen this session is framed as before.
    send(&mut app, Message::OpenListedBoard(root.join("B.canvas")));
    let mut fitted = app.camera;
    fitted.fit(app.content_bounds());
    assert_eq!((app.camera.offset, app.camera.zoom), (fitted.offset, fitted.zoom));

    // Back by the palette (or the browser): where it was left, not zoomed
    // out to all of it.
    send(&mut app, Message::OpenListedBoard(root.join("A.canvas")));
    assert_eq!((app.camera.offset, app.camera.zoom), (iced::Vector::new(-321.0, 123.0), 2.5));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn command_bracket_steps_back_and_letting_go_lands_there() {
    let root = boards_with_a_card("steps", &["A.canvas", "B.canvas", "C.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    for board in ["A.canvas", "B.canvas", "C.canvas"] {
        send(&mut app, Message::OpenListedBoard(root.join(board)));
    }
    let at_c = (app.camera.offset, app.camera.zoom);

    // Held: each press moves the highlight.
    for _ in 0..2 {
        send(&mut app, Message::StepBoards(-1));
    }
    assert_eq!(app.visits.strip.as_ref().map(|strip| strip.highlight), Some(0));
    assert_eq!(app.canvas_path, root.join("C.canvas"), "nothing moves until ⌘ is let go");
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("A.canvas"));
    assert!(app.visits.strip.is_none());

    // Stepping keeps what's ahead: forward twice comes back to C, as it was.
    send(&mut app, Message::StepBoards(1));
    send(&mut app, Message::StepBoards(1));
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("C.canvas"));
    assert_eq!((app.camera.offset, app.camera.zoom), at_c);

    // Past the last, round to the first.
    send(&mut app, Message::StepBoards(1));
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("A.canvas"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Opens each of `boards`, under `root`, in turn.
fn open_each(app: &mut App, root: &std::path::Path, boards: &[&str]) {
    for board in boards {
        send(app, Message::OpenListedBoard(root.join(board)));
    }
}

#[test]
fn work_finished_for_a_board_since_left_is_dropped() {
    let root = boards_with_a_card("stale", &["A.canvas", "B.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    open_each(&mut app, &root, &["A.canvas"]);
    let for_a = app.board_epoch;
    let a_clusters = crate::cluster::compute(&app.nodes, &app.edges, &app.theme);
    let a_bitmap = text_raster::snapshot_markdown("A.canvas", 250.0, 60.0, 230.0, 17.0, app.theme, false, 1.0).expect("a bitmap");

    // A's clustering and bitmaps, landing once B is on screen: neither is B's.
    open_each(&mut app, &root, &["B.canvas"]);
    let b_cards = app.clusters.list.iter().map(|cluster| cluster.bounds).collect::<Vec<_>>();
    send(&mut app, Message::ClustersReady(for_a, a_clusters, HashMap::new()));
    send(&mut app, Message::SnapshotsReady(for_a, vec![(0, a_bitmap)]));
    assert_eq!(app.clusters.list.iter().map(|cluster| cluster.bounds).collect::<Vec<_>>(), b_cards);
    assert!(!app.rasters.text.contains_key(&0));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_board_gone_back_to_keeps_its_bitmaps_unless_its_file_changed() {
    let root = boards_with_a_card("warm", &["A.canvas", "B.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    open_each(&mut app, &root, &["A.canvas"]);
    let bitmap = text_raster::snapshot_markdown("A.canvas", 250.0, 60.0, 230.0, 17.0, app.theme, false, 1.0).expect("a bitmap");
    let epoch = app.board_epoch;
    send(&mut app, Message::SnapshotsReady(epoch, vec![(0, bitmap)]));
    assert!(app.rasters.text.contains_key(&0));

    // Back from B: what A had, without making it again.
    open_each(&mut app, &root, &["B.canvas", "A.canvas"]);
    assert!(app.rasters.text.contains_key(&0), "A's bitmap, kept while it was away");

    // Changed while B was on screen: made afresh.
    open_each(&mut app, &root, &["B.canvas"]);
    std::thread::sleep(Duration::from_millis(20));
    let text = std::fs::read_to_string(root.join("A.canvas")).unwrap();
    std::fs::write(root.join("A.canvas"), text.replace("\"A.canvas\"", "\"A, changed\"")).unwrap();
    open_each(&mut app, &root, &["A.canvas"]);
    assert!(!app.rasters.text.contains_key(&0), "the file changed while it was away");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_board_opened_on_its_clusters_has_them_at_once() {
    // Cards far apart: fitted, the view is zoomed out to where clusters show.
    let root = board_vault("clusters", &["Wide.canvas"]);
    let card = |id: &str, x: i32| format!(r#"{{"id":"{id}","type":"text","text":"Card {id}","x":{x},"y":0,"width":250,"height":60}}"#);
    let cards: Vec<String> = (0..6).map(|at| card(&format!("n{at}"), at * 4000)).collect();
    std::fs::write(root.join("Wide.canvas"), format!(r#"{{"nodes":[{}],"edges":[]}}"#, cards.join(","))).unwrap();
    let mut app = app_with(Vec::new(), Vec::new());
    open_each(&mut app, &root, &["Wide.canvas"]);
    assert!(board::cluster_opacity(app.camera.zoom) > 0.0, "zoomed out to clusters: {}", app.camera.zoom);
    let expected = crate::cluster::compute(&app.nodes, &app.edges, &app.theme);
    assert!(!expected.clusters.is_empty());
    assert_eq!(app.clusters.list.len(), expected.clusters.len(), "computed with the switch, not after it");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn command_held_by_itself_brings_up_the_strip_and_any_other_use_of_it_doesnt() {
    use crate::canvas::board::{Interaction, SHOW_SWITCHER_AFTER};
    use iced::keyboard::{self, Key, Modifiers, key};
    use iced::widget::canvas::{Event, Program};
    use std::time::Instant;
    let app = app_with(Vec::new(), Vec::new());
    let board = app.board(app.camera);
    let bounds = iced::Rectangle::new(iced::Point::ORIGIN, app.camera.viewport);
    let held = |modifiers| Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers));
    let press = |letter: &str| {
        let key = Key::Character(letter.into());
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: None,
            repeat: false,
        })
    };
    let scroll = Event::Mouse(iced::mouse::Event::WheelScrolled { delta: iced::mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 } });
    // A frame past the one the board asks for — these are made before ⌘ is
    // fed to it — or one sooner.
    let due = || Event::Window(iced::window::Event::RedrawRequested(Instant::now() + 2 * SHOW_SWITCHER_AFTER));
    let soon = || Event::Window(iced::window::Event::RedrawRequested(Instant::now()));
    let shows = |events: Vec<Event>| {
        let mut state = Interaction::default();
        // The first event of all is taken to report the board's size.
        let _ = board.update(&mut state, &soon(), bounds, iced::mouse::Cursor::Unavailable);
        events.iter().filter_map(|event| board.update(&mut state, event, bounds, iced::mouse::Cursor::Unavailable)).any(
            |action| matches!(action.into_inner().0, Some(Message::ShowSwitcher)),
        )
    };

    assert!(shows(vec![held(Modifiers::COMMAND), due()]));
    assert!(!shows(vec![held(Modifiers::COMMAND), soon()]), "not before its time");
    assert!(shows(vec![held(Modifiers::COMMAND), press("["), press("["), due()]), "[ and ] are what it's for");
    // ⌘ put to another use: a chord, a ⌘-scroll, another modifier with it.
    assert!(!shows(vec![held(Modifiers::COMMAND), press("z"), due()]));
    assert!(!shows(vec![held(Modifiers::COMMAND), scroll, due()]));
    assert!(!shows(vec![held(Modifiers::COMMAND), held(Modifiers::COMMAND | Modifiers::SHIFT), due()]));
    assert!(!shows(vec![held(Modifiers::COMMAND | Modifiers::SHIFT), due()]));
    assert!(!shows(vec![held(Modifiers::COMMAND), held(Modifiers::empty()), due()]), "let go first");
}

#[test]
fn held_with_no_step_the_strip_opens_on_this_board_and_stays_there() {
    let root = boards_with_a_card("held", &["A.canvas", "B.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    send(&mut app, Message::OpenListedBoard(root.join("A.canvas")));
    // A board alone has nowhere to go, so nothing shows.
    send(&mut app, Message::ShowSwitcher);
    assert!(app.visits.strip.is_none());

    send(&mut app, Message::OpenListedBoard(root.join("B.canvas")));
    send(&mut app, Message::ShowSwitcher);
    assert_eq!(app.visits.strip.as_ref().map(|strip| strip.highlight), Some(1));
    assert!(app.strip_view().is_some());
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("B.canvas"));
    assert!(app.visits.strip.is_none());

    // A step taken before it shows is where it shows.
    send(&mut app, Message::StepBoards(-1));
    assert!(app.strip_view().is_none(), "a quick press shows nothing");
    send(&mut app, Message::ShowSwitcher);
    assert_eq!(app.visits.strip.as_ref().map(|strip| strip.highlight), Some(0));
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("A.canvas"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn esc_or_a_click_off_the_strip_stays_put() {
    let root = boards_with_a_card("cancel", &["A.canvas", "B.canvas"]);
    let mut app = app_with(Vec::new(), Vec::new());
    send(&mut app, Message::OpenListedBoard(root.join("A.canvas")));
    send(&mut app, Message::OpenListedBoard(root.join("B.canvas")));
    send(&mut app, Message::StepBoards(-1));
    send(&mut app, Message::CancelSwitch);
    send(&mut app, Message::CommandReleased);
    assert_eq!(app.canvas_path, root.join("B.canvas"));

    // A board in the strip can be clicked instead.
    send(&mut app, Message::StepBoards(-1));
    send(&mut app, Message::SwitchToVisited(0));
    assert_eq!(app.canvas_path, root.join("A.canvas"));
    assert!(app.visits.strip.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_search_finds_boards_in_a_section_that_leads_when_it_matches_better() {
    let mut app = app_with_vault(&["Projects/Roadmap.canvas", "Themes.canvas"]);
    // A board's name is the better match: it leads, and Enter would open it.
    let (rows, sections) = palette_rows(&app, "road");
    assert_eq!(sections.first(), Some(&"Boards"));
    assert_eq!(rows[0], ("Roadmap".to_owned(), "Projects".to_owned()));
    assert_eq!(command::filtered(&app, "road")[0].action, command::Action::Open(PathBuf::from("/vault/Projects/Roadmap.canvas")));

    // A tie goes to the commands, so a command typed by name still runs.
    let (rows, sections) = palette_rows(&app, "theme");
    assert_eq!(sections, ["Commands", "Boards"]);
    assert_eq!(rows[0].0, "Change theme…");

    // Nothing but commands matching is the search it always was.
    assert!(palette_rows(&app, "undo").1.is_empty());

    // The board that's open isn't found — there's nothing to switch to — and
    // with no board left, no sections either.
    app.canvas_path = PathBuf::from("/vault/Themes.canvas");
    app.scratch = false;
    assert!(palette_rows(&app, "theme").1.is_empty());
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
        // A part at a time, as `vault::active_theme` joins it: one `join`
        // of ".obsidian/themes" keeps its `/`, and on Windows the path the
        // app found, all `\`, didn't equal it.
        self.0
            .join(".obsidian")
            .join("themes")
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
                5 => Message::Settings(settings::Msg::ChooseProvider),
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
            + usize::from(app.provider_picker.is_some())
            + usize::from(app.search.is_some())
    };

    for first in 0..7 {
        for second in 0..7 {
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
    Model { id: id.to_owned(), name: None, free: false, price: None, images: None }
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
fn free_models_come_first_with_nothing_typed_and_a_search_keeps_the_sections_best_first() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    let free = |id: &str| Model { free: true, ..model(id) };
    let models = vec![model("anthropic/claude-sonnet-4"), free("google/gemma-4-31b-it:free"), model("openai/gpt-4o"), free("openrouter/free")];
    send(&mut app, Message::ModelsFetched(url, Ok(models)));

    let ids = |app: &App, query: &str| -> Vec<String> {
        app.model_matches(query).into_iter().map(|index| app.models()[index].id.clone()).collect()
    };
    let (_, headers) = app.model_listing("");
    assert_eq!(ids(&app, ""), ["openrouter/free", "google/gemma-4-31b-it:free", "anthropic/claude-sonnet-4", "openai/gpt-4o"]);
    assert_eq!(headers.iter().map(|(at, ..)| *at).collect::<Vec<_>>(), [0, 1, 2], "the router, the free ones, the paid");
    assert!(headers[1].1.contains("rate-limited"), "free isn't put forward as dependable: {}", headers[1].1);
    assert!(headers[2].1.starts_with("Paid"), "{}", headers[2].1);
    assert_eq!(app.highlighted_model().as_deref(), Some("openrouter/free"), "the highlight is on the first row shown");

    // Searched, the sections stay — a free model found is still under its
    // header — and the one with the best match leads, so Enter takes it.
    let labels = |query: &str| app.model_listing(query).1.into_iter().map(|(_, label, _)| label).collect::<Vec<_>>();
    assert_eq!(labels("gpt").len(), 1, "one section left is still named");
    assert!(labels("gpt")[0].starts_with("Paid"));
    assert_eq!(ids(&app, "gpt"), ["openai/gpt-4o"]);
    assert_eq!(labels("gemma"), ["Free · rate-limited"]);
    assert_eq!(labels("free")[0], "Free · routes to an available model");
    assert_eq!(ids(&app, "claude")[0], "anthropic/claude-sonnet-4");
    send(&mut app, Message::PanelInput(picker::Panel::Model, "gpt".to_owned()));
    assert_eq!(app.highlighted_model().as_deref(), Some("openai/gpt-4o"));
}

#[test]
fn a_model_searched_for_by_name_leads_its_free_variant() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    let models = vec![model("qwen/qwen3-30b-a3b"), Model { free: true, ..model("qwen/qwen3-30b-a3b:free") }];
    send(&mut app, Message::ModelsFetched(url, Ok(models)));
    let labels: Vec<_> = app.model_listing("qwen").1.into_iter().map(|(_, label, _)| label).collect();
    assert!(labels[0].starts_with("Paid") && labels[1] == "Free · rate-limited", "a tie goes to the paid: {labels:?}");
    // With nothing typed, free still comes first.
    assert_eq!(app.model_listing("").1[0].1, "Free · rate-limited");
}

#[test]
fn the_picker_opens_on_the_model_in_use_and_keeps_its_highlight_as_the_list_is_refetched() {
    // Asked for the first time, the highlight lands on it with the list.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    app.config.ai_model = "qwen/qwen3-30b-a3b".to_owned();
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    let listed = |ids: &[&str]| Ok(ids.iter().map(|id| model(id)).collect());
    let ids = ["anthropic/claude-sonnet-4", "openai/gpt-4o", "qwen/qwen3-30b-a3b"];
    send(&mut app, Message::ModelsFetched(url.clone(), listed(&ids)));
    assert_eq!(app.highlighted_model().as_deref(), Some("qwen/qwen3-30b-a3b"));

    // Opened again, on the list kept from before, at once.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, Message::OpenModelPicker);
    assert_eq!(app.highlighted_model().as_deref(), Some("qwen/qwen3-30b-a3b"));

    // Moved, it stays where it was moved to when the refetch lands, though
    // the list under it has grown.
    send(&mut app, Message::PanelNav(picker::Panel::Model, picker::Nav::Step(-1)));
    send(&mut app, Message::ModelsFetched(url, listed(&["aaa/new", ids[0], ids[1], ids[2]])));
    assert_eq!(app.highlighted_model().as_deref(), Some("openai/gpt-4o"));
}

#[test]
fn the_form_asks_openrouter_for_its_prices_once_and_not_again_after_a_failure() {
    let mut app = app_with(two_nodes(), Vec::new());
    let openrouter = "https://openrouter.ai/api/v1".to_owned();
    app.config.ai_url = openrouter.clone();
    assert!(app.form_wants_models(), "nothing kept yet");
    send(&mut app, Message::ModelsFetched(openrouter.clone(), Err("No API key".to_owned())));
    assert!(!app.form_wants_models(), "not again after a failure");
    send(&mut app, Message::ModelsFetched(openrouter, Ok(vec![model("openai/gpt-4o")])));
    assert!(!app.form_wants_models(), "kept");

    // Only OpenRouter's list has prices; another's is fetched for nothing.
    for url in ["https://api.openai.com/v1", "http://localhost:1234/v1"] {
        app.config.ai_url = url.to_owned();
        assert!(!app.form_wants_models(), "{url}");
    }
}

#[test]
fn providers_are_chosen_by_sort_from_a_picker_over_the_form() {
    let mut app = app_with(two_nodes(), Vec::new());
    send(&mut app, Message::OpenSettings);
    send(&mut app, Message::Settings(settings::Msg::ChooseProvider));
    let selected = app.provider_picker.as_ref().map(|picker| picker.state.selected);
    assert_eq!(selected, Some(0), "open over the form, on Off, the provider in use");
    send(&mut app, Message::RunningHere(vec!["LM Studio", "Ollama"]));

    // A section to each sort, Off first under none, Custom last; of the
    // runtimes here, the ones that answered.
    let (names, headers) = app.provider_listing("");
    assert_eq!(names.first(), Some(&"Off"));
    assert_eq!(names.last(), Some(&"Custom"));
    let headed = |label: &str| headers.iter().find(|(_, said)| *said == label).map(|&(heads, _)| names[heads]);
    assert_eq!(headed("On this machine"), Some("LM Studio"));
    assert_eq!(headed("Many models, one key"), Some("OpenRouter"));
    assert_eq!(headed("Model makers"), Some("OpenAI"));
    assert_eq!(headed("Any other endpoint"), Some("Custom"));
    assert_eq!(headers.len(), 4);
    assert!(!names.contains(&"llama.cpp") && !names.contains(&"Jan"), "not running: {names:?}");

    // Typed into, one ranked list: by name, and by sort after those.
    let (found, headers) = app.provider_listing("olla");
    assert_eq!((found, headers.len()), (vec!["Ollama"], 0));
    let (found, _) = app.provider_listing("this machine");
    assert_eq!(found, ["LM Studio", "Ollama"]);

    // Enter on one is the form's own choice: the URL filled in, the picker
    // put away — and with a runtime here, the models next.
    send(&mut app, Message::PanelInput(picker::Panel::Provider, "olla".to_owned()));
    send(&mut app, Message::CommitProvider);
    assert_eq!(app.config.ai_url, "http://localhost:11434/v1");
    assert!(app.provider_picker.is_none());
    assert!(app.model_picker.is_some(), "the model picker, as picking a preset opens it");

    // Reopened, it's on the one in use — and stays on it as the answers
    // change the rows above it.
    send(&mut app, Message::OpenSettings);
    send(&mut app, Message::Settings(settings::Msg::ChooseProvider));
    let highlighted = |app: &App| {
        let at = app.provider_picker.as_ref().map(|picker| picker.state.selected)?;
        app.provider_listing("").0.get(at).copied()
    };
    assert_eq!(highlighted(&app), Some("Ollama"));
    send(&mut app, Message::RunningHere(vec!["Jan", "llama.cpp", "LM Studio", "Ollama"]));
    assert_eq!(highlighted(&app), Some("Ollama"));

    // Not running, the one in use is still listed, and says so; and with
    // nothing else running, the section says that rather than go.
    send(&mut app, Message::RunningHere(Vec::new()));
    let (names, _) = app.provider_listing("");
    assert_eq!(names[1..3], ["Ollama", "OpenRouter"], "the one in use, alone in its section");
    app.config.ai_url = String::new();
    let (names, headers) = app.provider_listing("");
    assert_eq!((names[1], headers[0]), ("None running", (1, "On this machine")));
    send(&mut app, Message::PanelNav(picker::Panel::Provider, picker::Nav::First));
    send(&mut app, Message::PanelNav(picker::Panel::Provider, picker::Nav::Step(1)));
    send(&mut app, Message::CommitProvider);
    assert!(app.provider_picker.is_some(), "the stand-in can't be chosen");
}

#[test]
fn ask_ai_says_what_a_question_goes_to_and_whether_its_free() {
    let words = |app: &App| app.ask_ai_label().words();
    // Before any list is fetched, the id says it: its last part, and
    // whether it's free by OpenRouter's `:free` variants and its router.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    app.config.ai_model = "openrouter/free".to_owned();
    assert_eq!(words(&app), "Free Models Router", "free by its name, said once");
    app.config.ai_model = "anthropic/claude-sonnet-4.5".to_owned();
    assert_eq!(words(&app), "claude-sonnet-4.5");

    // Once one is, by the name it lists, less its vendor; and free by its
    // price, whatever the name: free offers come and go.
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    let models = vec![
        Model { free: true, name: Some("Sonoma Alpha".to_owned()), ..model("openrouter/sonoma-alpha") },
        model("acme/fast:free"),
        Model { name: Some("Anthropic: Claude Sonnet 4.5".to_owned()), ..model("anthropic/claude-sonnet-4.5") },
    ];
    send(&mut app, Message::ModelsFetched(url, Ok(models)));
    assert_eq!(words(&app), "Claude Sonnet 4.5");
    app.config.ai_model = "openrouter/sonoma-alpha".to_owned();
    assert_eq!(words(&app), "Sonoma Alpha · free");
    // A long name is what's cut, not that it's free.
    assert_eq!(app.ask_ai_label().after(), " · free");
    app.config.ai_model = "acme/fast:free".to_owned();
    assert!(!app.asks_a_free_model(), "listed with a price");

    // A runtime on this machine, its tag dropped where it says nothing.
    app.config.ai_url = "http://localhost:11434/v1".to_owned();
    app.config.ai_model = "llama3:latest".to_owned();
    assert_eq!(words(&app), "llama3");
    // With no model named, the runtime answers with what it has loaded.
    app.config.ai_model = String::new();
    assert_eq!(words(&app), "Ollama");
    app.config.ai_url = "http://localhost:8080/v1".to_owned();
    assert_eq!(words(&app), "localhost:8080");
}

#[test]
fn a_sign_in_keeps_its_key_with_openrouter_and_lists_its_models() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    // Waiting, as the row shows it once the browser's been opened.
    let waiting = |app: &mut App| {
        let (_, handle) = iced::Task::<Message>::none().abortable();
        app.signing = sign_in::Signing::Waiting { _stop: sign_in::Stop::new(), _task: handle };
    };

    waiting(&mut app);
    send(&mut app, Message::SignedIn(Err("The sign-in had run out of time — sign in again.".to_owned())));
    assert!(matches!(&app.signing, sign_in::Signing::Failed(why) if why.contains("run out of time")));

    waiting(&mut app);
    send(&mut app, Message::SignedIn(Ok("sk-or-v1-abc".to_owned())));
    assert!(matches!(app.signing, sign_in::Signing::Idle));
    assert_eq!(app.config.ai_key(), "sk-or-v1-abc");
    assert!(
        app.model_picker.as_ref().and_then(|picker| picker.notice).is_some_and(|notice| notice.starts_with("Signed in")),
        "said over the models it opened",
    );
    assert!(app.model_picker.as_ref().is_some_and(|picker| !picker.over_settings), "its models next, over the board with the form closed");

    // From the settings window, over the form there: it has no board.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::OpenSettings);
    waiting(&mut app);
    send(&mut app, Message::SignedIn(Ok("sk-or-v1-abc".to_owned())));
    assert!(app.model_picker.as_ref().is_some_and(|picker| picker.over_settings));

    // Another provider chosen while signing in: the key is still OpenRouter's.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://api.openai.com/v1".to_owned();
    waiting(&mut app);
    send(&mut app, Message::SignedIn(Ok("sk-or-v1-def".to_owned())));
    assert_eq!(app.config.ai_key(), "", "not OpenAI's");
    assert_eq!(app.config.ai_api_keys.get("https://openrouter.ai/api/v1").map(String::as_str), Some("sk-or-v1-def"));
    assert!(app.model_picker.is_none());

    // Cancelled: whatever comes back is no one's.
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    waiting(&mut app);
    send(&mut app, Message::CancelSignIn);
    send(&mut app, Message::SignedIn(Ok("sk-or-v1-late".to_owned())));
    assert_eq!(app.config.ai_key(), "");
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

    // A local runtime needs no key, so its list comes up at once — in place,
    // under the Model row, with the form still there beneath it.
    send(&mut app, Message::Settings(settings::Msg::PickPreset("LM Studio".to_owned())));
    assert!(app.model_picker.as_ref().is_some_and(|picker| picker.over_settings));
    assert!(app.settings_overlay);

    // A hosted one with no key would only list a refusal: the form stays put.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::PickPreset("OpenAI".to_owned())));
    assert!(app.model_picker.is_none());
    assert!(app.settings_overlay);
    // With its key in place, it's ready.
    app.config.set_ai_key("sk-1");
    send(&mut app, Message::Settings(settings::Msg::PickPreset("OpenAI".to_owned())));
    assert!(app.model_picker.is_some());

    // And the button beside the field, whenever.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    assert!(app.model_picker.is_some());
    assert!(app.settings_overlay, "the form stays, under it");
    // A model chosen leaves the form where it was.
    send(&mut app, Message::PickModel("gpt-4o".to_owned()));
    assert!(app.model_picker.is_none() && app.settings_overlay);
}

#[test]
fn a_picker_from_the_form_waits_for_its_place_under_the_control() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ChooseProvider));
    let place = |app: &App| app.provider_picker.as_ref().map(|picker| picker.place);
    assert_eq!(place(&app), Some(picker::Place::Finding));
    assert!(app.settings_picker().is_none(), "not drawn till it's placed");

    let anchor = picker::Anchor {
        at: Rectangle::new(Point::new(500.0, 120.0), Size::new(196.0, 32.0)),
        window: Size::new(1000.0, 700.0),
    };
    send(&mut app, Message::Placed(picker::Panel::Provider, Some(anchor)));
    assert_eq!(place(&app), Some(picker::Place::Under(anchor)));
    assert!(app.settings_picker().is_some());
    // An answer for a panel not waiting changes nothing.
    send(&mut app, Message::Placed(picker::Panel::Provider, None));
    assert_eq!(place(&app), Some(picker::Place::Under(anchor)));

    // A control that couldn't be found puts it at the top, not nowhere.
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    assert_eq!(app.model_picker.as_ref().map(|picker| picker.place), Some(picker::Place::Finding));
    send(&mut app, Message::Placed(picker::Panel::Model, None));
    assert_eq!(app.model_picker.as_ref().map(|picker| picker.place), Some(picker::Place::Top));
    assert!(app.settings_picker().is_some());
}

#[test]
fn a_provider_from_the_palette_is_chosen_over_the_board_and_the_form_asks_what_it_still_needs() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url.clear();
    // What the command sends: running it hands that back as a task.
    let open = app.command_message(command::CommandId::ChooseProvider).expect("runs with no provider: it's the way to one");
    send(&mut app, open.clone());
    let picker = app.provider_picker.as_ref().map(|picker| (picker.over_settings, picker.place));
    assert_eq!(picker, Some((false, picker::Place::Top)), "over the board, where the palette was");

    // A runtime here: its models next, over the board too.
    send(&mut app, Message::Settings(settings::Msg::PickPreset("LM Studio".to_owned())));
    assert!(app.model_picker.as_ref().is_some_and(|picker| !picker.over_settings));
    assert!(!app.settings_overlay);

    // A hosted one without a key: the form, at the key.
    send(&mut app, Message::ClosePanel(picker::Panel::Model));
    send(&mut app, open.clone());
    send(&mut app, Message::Settings(settings::Msg::PickPreset("Anthropic".to_owned())));
    assert!(app.settings_overlay);
    assert_eq!(app.settings_focus, Some(settings::Field::AiApiKey));

    // Custom: the form, at the URL.
    send(&mut app, Message::CloseSettingsOverlay);
    send(&mut app, open.clone());
    send(&mut app, Message::Settings(settings::Msg::PickPreset("Custom".to_owned())));
    assert_eq!(app.settings_focus, Some(settings::Field::AiUrl));
}

#[test]
fn esc_over_the_settings_window_closes_only_the_picker() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::OpenSettings);
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    assert!(app.model_picker.as_ref().is_some_and(|picker| picker.over_settings), "the window has no board");
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
    assert!(app.settings_window.is_some(), "the form it was chosen from stays");

    // Closing the form takes a picker opened from it along.
    send(&mut app, Message::Settings(settings::Msg::ChooseModel));
    let window = app.settings_window.expect("the settings window");
    send(&mut app, Message::WindowCloseRequested(window));
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
    send(&mut app, Message::Settings(settings::Msg::Edit(settings::Field::MaxCardWidth, "600".to_owned())));
    send(&mut app, Message::Settings(settings::Msg::Reset(settings::Field::MaxCardWidth)));
    assert_eq!(app.draft(settings::Field::MaxCardWidth), "420");

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
fn a_reload_that_changes_the_blur_waits_for_tiles_at_the_new_one() {
    // Without a wallpaper to find — a Linux CI runner has no desktop — there's
    // no tile to wait for.
    if !crate::config::OverlayBackdrop::offered().contains(&crate::config::OverlayBackdrop::Wallpaper) {
        return;
    }
    use crate::platform::BackdropPicture;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.overlay_blur = 2.0;
    send(&mut app, Message::OpenSettingsOverlay);
    send(&mut app, Message::Settings(settings::Msg::ReloadConfig));
    assert_eq!(app.config.overlay_blur, 1.0);
    // The tile asked for before the reload lands after it, and isn't what the
    // overlay would show any more.
    send(&mut app, Message::BackdropPreview(BackdropPicture::Wallpaper, 2.0, Some(vec![0])));
    assert!(app.backdrop_preview(&BackdropPicture::Wallpaper).is_none());
    send(&mut app, Message::BackdropPreview(BackdropPicture::Wallpaper, 1.0, Some(vec![0])));
    assert!(app.backdrop_preview(&BackdropPicture::Wallpaper).is_some());
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
    run(&mut app, first.command().expect("a command"));
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
    run(&mut app, scrollbars.command().expect("a command"));
    assert!(app.config.card_scrollbars);

    // A number is set in the form: it opens there, on its tab, its row focused.
    let inertia = command::filtered(&app, "pan inertia").into_iter().next().expect("a hit");
    assert_eq!(inertia.detail, "Canvas");
    run(&mut app, inertia.command().expect("a command"));
    assert!(app.settings_overlay);
    assert_eq!(app.settings_tab, settings::Tab::Canvas);
    assert_eq!(app.settings_focus, Some(settings::Field::PanInertia));
}

#[test]
fn settings_join_a_search_not_the_bare_palette() {
    let app = app_with(two_nodes(), Vec::new());
    // Nothing typed: the commands, as a list to browse.
    assert!(command::filtered(&app, "").iter().all(|entry| !matches!(
        entry.command(),
        Some(command::CommandId::Setting(_) | command::CommandId::Choice(..))
    )));
    // A setting that can't act yet says what would let it.
    let mut app = app;
    app.config.ai_url.clear();
    let thinking = command::filtered(&app, "think first").into_iter().next().expect("a hit");
    assert!(thinking.unavailable(&app).is_some_and(|why| why.contains("AI provider")));
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
    // Where there's no window to show a picture in, the form doesn't offer
    // one, and a config naming one is read as the live desktop.
    if !crate::platform::SUPPORTS_BACKDROP_PICTURE {
        return;
    }
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
    // The missing provider is the reason that matters even with a card
    // selected: selecting one more won't help.
    assert!(AskAi.unavailable(&app).is_some_and(|why| why.contains("AI provider")));

    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::SelectNode(None));
    assert_eq!(AskAi.unavailable(&app), Some("Select a card to ask about"));

    send(&mut app, Message::SelectNode(Some(0)));
    assert_eq!(AskAi.unavailable(&app), None);
}

#[test]
fn a_failure_partway_keeps_what_was_written() {
    use thinkboard_core::ai::Event;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    let text = |app: &App, id: &str| app.nodes.iter().find(|n| n.id == id).and_then(|n| n.text.clone());

    // Before the first word, the card holds only the dots: the failure is it.
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
fn a_model_known_to_read_text_only_is_said_to_and_one_not_known_isnt() {
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    app.config.ai_model = "meta-llama/llama-3.3-70b-instruct".to_owned();
    assert!(!app.reads_text_only(), "nothing listed yet: no sign either way");
    send(&mut app, Message::OpenModelPicker);
    let url = app.config.ai_url.clone();
    let models = vec![
        Model { images: Some(false), ..model("meta-llama/llama-3.3-70b-instruct") },
        Model { images: Some(true), ..model("google/gemma-3-27b-it") },
        model("acme/silent"),
    ];
    send(&mut app, Message::ModelsFetched(url, Ok(models)));
    assert!(app.reads_text_only());
    app.config.ai_model = "google/gemma-3-27b-it".to_owned();
    assert!(!app.reads_text_only());
    app.config.ai_model = "acme/silent".to_owned();
    assert!(!app.reads_text_only(), "listed, but not said");
}

#[test]
fn a_newer_release_is_told_until_its_put_away_and_an_older_one_never() {
    use thinkboard_core::update::Release;
    let release = |version: &str| Release { version: version.to_owned(), url: format!("https://github.com/vestigecounty/thinkboard/releases/tag/v{version}") };
    let mut app = app_with(two_nodes(), Vec::new());
    let told = |app: &App| app.update_to_tell().map(|release| release.version.clone());

    // This one, or one before it: nothing to say.
    send(&mut app, Message::UpdateChecked(Ok(release(updates::VERSION))));
    assert_eq!(told(&app), None);
    send(&mut app, Message::UpdateChecked(Ok(release("0.0.1"))));
    assert_eq!(told(&app), None);
    // A failed check says nothing either.
    send(&mut app, Message::UpdateChecked(Err("offline".to_owned())));
    assert_eq!(told(&app), None);

    // A later one is told, with its page to open.
    send(&mut app, Message::UpdateChecked(Ok(release("999.0.0"))));
    assert_eq!(told(&app).as_deref(), Some("999.0.0"));
    assert!(app.update_notice().is_some());
    // Checking turned off, it's not said.
    app.config.check_for_updates = false;
    assert_eq!(told(&app), None);
    app.config.check_for_updates = true;

    // Put away, it stays away for that release, and a later one is told.
    send(&mut app, Message::DismissUpdate);
    assert_eq!(app.config.dismissed_update, "999.0.0");
    assert_eq!(told(&app), None);
    send(&mut app, Message::UpdateChecked(Ok(release("999.1.0"))));
    assert_eq!(told(&app).as_deref(), Some("999.1.0"));
}

#[test]
fn an_answer_asked_without_its_images_begins_by_saying_so() {
    use thinkboard_core::ai::Event;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::AskAi);
    let id = app.ai.streams.keys().next().expect("asked").clone();
    let text = |app: &App| app.nodes.iter().find(|n| n.id == id).and_then(|n| n.text.clone());

    // Not said at once: the dots stay, the answer still coming.
    send(&mut app, Message::Ai(id.clone(), Event::ImagesLeftOut));
    assert!(app.ai.pending.contains(&id), "still waiting on the first words");
    for event in [Event::Delta("Going by the cards ".to_owned()), Event::Delta("alone.".to_owned()), Event::Split, Event::Delta("And another.".to_owned())] {
        send(&mut app, Message::Ai(id.clone(), event));
    }
    assert_eq!(
        text(&app).as_deref(),
        Some("*The images weren't sent: this model reads text only.*\n\nGoing by the cards alone."),
        "once, ahead of the first card's words"
    );
}

#[test]
fn what_an_answer_cost_is_said_under_its_last_card_and_nothing_from_a_runtime_here() {
    use thinkboard_core::ai::{Event, Usage};
    let ask = |app: &mut App, card: usize, usage: Usage| {
        send(app, Message::SelectNode(Some(card)));
        send(app, Message::AskAi);
        let id = app.ai.streams.keys().next().expect("asked").clone();
        for event in [Event::Delta("Tram 28".to_owned()), Event::Split, Event::Delta("Or walk".to_owned())] {
            send(app, Message::Ai(id.clone(), event));
        }
        send(app, Message::Ai(id.clone(), Event::Usage(usage)));
        send(app, Message::Ai(id, Event::Done));
    };
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "https://openrouter.ai/api/v1".to_owned();
    app.config.ai_model = "qwen/qwen3-30b-a3b".to_owned();
    ask(&mut app, 0, Usage { prompt_tokens: 812, completion_tokens: 403, cost: Some(0.00031) });
    let spent = app.ai.spent.last().expect("said");
    assert_eq!(spent.words, "$0.00031 · 1.2k tokens");
    assert_eq!(Some(&spent.node), app.nodes.last().map(|n| &n.id), "under the answer's last card");

    // A free model's answer costs nothing, and says so.
    app.config.ai_model = "openrouter/free".to_owned();
    ask(&mut app, 1, Usage { prompt_tokens: 90, completion_tokens: 40, cost: Some(0.0) });
    assert_eq!(app.ai.spent.last().map(|s| s.words.as_str()), Some("free · 130 tokens"));

    // A runtime on this machine gives no cost, and gets no label.
    let before = app.ai.spent.len();
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    ask(&mut app, 0, Usage { prompt_tokens: 90, completion_tokens: 40, cost: None });
    assert_eq!(app.ai.spent.len(), before);
}

#[test]
fn an_answers_cards_share_one_width() {
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

#[test]
fn an_answers_card_widened_to_the_others_keeps_no_bitmap_of_it_narrow() {
    // A `---` starts a batch of bitmaps partway through an answer, and the
    // end widens its cards to one width: a bitmap of a card made before that
    // was drawn stretched over it, its lines still wrapped at the old width.
    use thinkboard_core::ai::Event;
    let mut app = app_with(two_nodes(), Vec::new());
    app.config.ai_url = "http://localhost:1234/v1".to_owned();
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::AskAi);
    let id = app.ai.streams.keys().next().expect("asked").clone();
    send(&mut app, Message::Ai(id.clone(), Event::Delta("Tram 28".to_owned())));
    send(&mut app, Message::Ai(id.clone(), Event::Split));
    // What the split's batch makes of the first card, as it stands.
    let short = 2;
    let bitmap_of = |app: &App, index: usize| {
        let node = &app.nodes[index];
        let text = node.text.as_deref().unwrap_or_default();
        let wrap = text_raster::card_text_wrap(node.width);
        let (theme, center, zoom) = (app.theme, app.config.center_card_text, app.camera.zoom);
        text_raster::snapshot_markdown(text, node.width, node.height, wrap, FONT_SIZE, theme, center, zoom).expect("a bitmap")
    };
    let narrow = bitmap_of(&app, short);
    let width = app.nodes[short].width;
    let has_bitmap = |app: &App, index| app.rasters.text.contains_key(&index) || app.rasters.staging.contains_key(&index);

    // Landed before the end, it's the card as it is, and goes on.
    let epoch = app.board_epoch;
    send(&mut app, Message::SnapshotsReady(epoch, vec![(short, narrow.clone())]));
    assert!(has_bitmap(&app, short));
    // Partway through the next card's text, a bitmap is made of it as it was.
    send(&mut app, Message::Ai(id.clone(), Event::Delta("**Time Out Market** — a quick lunch, ".to_owned())));
    let partway = bitmap_of(&app, short + 1);
    send(&mut app, Message::Ai(id.clone(), Event::Delta("half the city's kitchens under one roof.".to_owned())));
    send(&mut app, Message::Ai(id.clone(), Event::Done));
    assert!(app.nodes[short].width > width, "widened to the other card");
    assert!(!has_bitmap(&app, short), "the bitmap of it narrow is let go");

    // Landed after, it's turned away — and so is one of a card's text before
    // the rest of it came.
    send(&mut app, Message::SnapshotsReady(epoch, vec![(short, narrow), (short + 1, partway)]));
    assert!(!has_bitmap(&app, short) && !has_bitmap(&app, short + 1));
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
    let epoch = app.board_epoch;
    send(&mut app, Message::SnapshotsReady(epoch, vec![(0, stale)]));

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
    send(&mut app, Message::SnapshotsReady(epoch, vec![(0, fresh)]));
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
    let _clipboard = clipboard::system::exclusive();
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
    send(&mut app, Message::Copy { chain: false, mermaid: false });
    let (_, path) = file_app("held-image", Vec::new());
    let _ = app.open_canvas(path.clone());
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    let file = app.nodes[0].file.clone().expect("an image node");
    assert!(path.parent().unwrap().join(&file).exists(), "{file} beside the board");
    *clipboard::system::held() = None;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn files_copied_in_a_file_manager_are_pasted_as_themselves() {
    let _clipboard = clipboard::system::exclusive();
    let (mut app, path) = file_app("pasted-files", Vec::new());
    let vault = path.parent().unwrap().to_path_buf();
    app.base_path = vault.clone();
    let elsewhere = std::env::temp_dir().join(format!("tb-elsewhere-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(vault.join("attachments")).unwrap();
    std::fs::create_dir_all(vault.join("notes")).unwrap();
    let photo = elsewhere.join("photo.jpg");
    ::image::save_buffer(&photo, &[90; 8 * 4 * 3], 8, 4, ::image::ExtendedColorType::Rgb8).unwrap();
    let inside = vault.join("attachments/inside.png");
    ::image::save_buffer(&inside, &[90; 4 * 4 * 4], 4, 4, ::image::ExtendedColorType::Rgba8).unwrap();
    let packing = vault.join("notes/Packing.md");
    std::fs::write(&packing, "- passport\n- adapter").unwrap();
    let ideas = elsewhere.join("Ideas.md");
    std::fs::write(&ideas, "Take the 28 tram early").unwrap();
    let pdf = elsewhere.join("itinerary.pdf");
    std::fs::write(&pdf, "%PDF-1.7").unwrap();
    // No extension, which in a board would mean a note.
    let makefile = elsewhere.join("Makefile");
    std::fs::write(&makefile, "all:").unwrap();
    // A picture of the same name is already beside the board.
    std::fs::write(vault.join("photo.jpg"), "taken").unwrap();

    // Finder's icons ride along as an image; the files are what's pasted.
    let files = vec![photo.clone(), inside, packing, ideas.clone(), pdf.clone(), makefile.clone()];
    *clipboard::system::held() = Some(clipboard::Clip::Files(files));
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    assert_eq!(app.nodes.len(), 6);
    assert_eq!(app.selected, vec![0, 1, 2, 3, 4, 5], "left selected, as pasted cards are");

    // From outside the vault: copied beside the board, under its own name,
    // numbered where that's taken, and not re-encoded.
    let copied = &app.nodes[0];
    assert_eq!(copied.node_kind(), NodeKind::Image);
    assert_eq!(copied.file.as_deref(), Some("photo 1.jpg"));
    assert_eq!(std::fs::read(vault.join("photo 1.jpg")).unwrap(), std::fs::read(&photo).unwrap());
    assert_eq!(copied.width / copied.height, 2.0, "the picture's shape: {}×{}", copied.width, copied.height);
    assert!(app.images.contains_key(&copied.id));

    // From inside it: used where it lies.
    assert_eq!(app.nodes[1].file.as_deref(), Some("attachments/inside.png"));
    assert!(app.images.contains_key(&app.nodes[1].id));

    // A note shows its text, by the same rule: where it lies inside the
    // vault, copied in from outside it.
    assert_eq!(app.nodes[2].node_kind(), NodeKind::Note);
    assert_eq!(app.nodes[2].file.as_deref(), Some("notes/Packing.md"));
    assert_eq!(app.notes.get(&app.nodes[2].id).map(String::as_str), Some("- passport\n- adapter"));
    assert_eq!(app.nodes[3].file.as_deref(), Some("Ideas.md"));
    assert_eq!(app.notes.get(&app.nodes[3].id).map(String::as_str), Some("Take the 28 tram early"));

    // Anything else: its path, as text.
    for (node, file) in app.nodes[4..].iter().zip([&pdf, &makefile]) {
        assert_eq!(node.node_kind(), NodeKind::Text);
        assert_eq!(node.text.as_deref(), Some(file.display().to_string().as_str()));
    }
    // Laid out apart, not stacked on one another.
    let rects: Vec<_> = app.nodes.iter().map(arrange::node_world_rect).collect();
    for (i, a) in rects.iter().enumerate() {
        assert!(rects[i + 1..].iter().all(|b| !a.intersects(b)), "card {i} overlaps another");
    }

    // On the welcome board, which has no folder, a file is used where it
    // lies, by its full path…
    let mut app = app_with(Vec::new(), Vec::new());
    *clipboard::system::held() = Some(clipboard::Clip::Files(vec![photo.clone(), ideas.clone()]));
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    assert_eq!(app.nodes[0].file.as_deref(), Some(photo.to_string_lossy().as_ref()));
    assert!(app.images.contains_key(&app.nodes[0].id));
    assert_eq!(app.nodes[1].file.as_deref(), Some(ideas.to_string_lossy().as_ref()));
    assert_eq!(app.notes.get(&app.nodes[1].id).map(String::as_str), Some("Take the 28 tram early"));

    // …and brought in when its card is copied onto a board with one.
    app.selected = vec![0, 1];
    send(&mut app, Message::Copy { chain: false, mermaid: false });
    let (_, onward) = file_app("pasted-files-onward", Vec::new());
    let _ = app.open_canvas(onward.clone());
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    let folder = onward.parent().unwrap();
    assert_eq!(app.nodes[0].file.as_deref(), Some("photo.jpg"));
    assert!(folder.join("photo.jpg").exists());
    assert_eq!(app.nodes[1].file.as_deref(), Some("Ideas.md"));
    assert_eq!(app.notes.get(&app.nodes[1].id).map(String::as_str), Some("Take the 28 tram early"));

    *clipboard::system::held() = None;
    let _ = std::fs::remove_dir_all(&vault);
    let _ = std::fs::remove_dir_all(&elsewhere);
    let _ = std::fs::remove_dir_all(folder);
}

// --- Copying cards out ---

/// A bug reasoned about, left to right: a symptom, a cause ruled out, the
/// next thing to try.
fn a_chain_of_three() -> (Vec<CanvasNode>, Vec<CanvasEdge>) {
    let card = |id: &str, text: &str, x: f32| CanvasNode { x, ..text_node(id, text) };
    let nodes = vec![
        card("symptom", "Login fails after the token refresh", 0.0),
        card("skew", "Clock skew", 400.0),
        card("cache", "The token cache is shared between tabs", 800.0),
    ];
    let mut ruled_out = edge("symptom", "skew");
    ruled_out.set_label("ruled out");
    (nodes, vec![ruled_out, edge("skew", "cache")])
}

const THE_CHAIN: &str = "- Login fails after the token refresh\n\
                         - → ruled out: Clock skew\n\
                         - The token cache is shared between tabs";

#[test]
fn copied_cards_are_text_to_another_app_and_cards_to_this_one() {
    let _clipboard = clipboard::system::exclusive();
    let (nodes, edges) = a_chain_of_three();
    let mut app = app_with(nodes, edges);

    // Selected in any order, they're written in the order they're linked.
    send(&mut app, Message::SelectNodes(vec![2, 0, 1]));
    send(&mut app, Message::Copy { chain: false, mermaid: false });
    assert_eq!(clipboard::read_text().as_deref(), Some(THE_CHAIN), "not JSON with positions");
    let html = clipboard::system::html().expect("and as HTML");
    assert!(html.contains("<li>→ ruled out: Clock skew</li>"), "{html}");

    // Pasted back here, they're the cards, linked and labelled as they were.
    send(&mut app, Message::Paste { world: Point::new(0.0, 600.0) });
    assert_eq!(app.nodes.len(), 6);
    assert_eq!(app.edges.len(), 4);
    assert!(app.edges[2..].iter().any(|e| e.label() == Some("ruled out")));
}

#[test]
fn copy_chain_takes_each_card_leading_in_without_selecting_it() {
    let _clipboard = clipboard::system::exclusive();
    let (nodes, edges) = a_chain_of_three();
    let mut app = app_with(nodes, edges);

    send(&mut app, Message::SelectNode(Some(2)));
    send(&mut app, Message::Copy { chain: true, mermaid: false });
    assert_eq!(clipboard::read_text().as_deref(), Some(THE_CHAIN));
    assert_eq!(app.selected, vec![2], "the selection is left as it was");

    // It's swept once to show what went, and then the sweep is done.
    let flow = app.ai.flows.last().expect("a sweep");
    assert_eq!((flow.target.as_ref(), flow.nodes.len(), flow.edges.len()), (None, 3, 2));
    app.ai.flows.last_mut().unwrap().since = Instant::now() - Duration::from_secs(5);
    send(&mut app, Message::Tick(Instant::now()));
    assert!(app.ai.flows.is_empty());
}

#[test]
fn a_copied_group_is_its_outline_and_pastes_with_its_cards() {
    let _clipboard = clipboard::system::exclusive();
    let mut packing = node_of("packing", "group");
    (packing.width, packing.height) = (400.0, 300.0);
    packing.set_label("Packing");
    let card = |id: &str, y: f32| CanvasNode { x: 40.0, y, ..text_node(id, id) };
    let mut app = app_with(vec![packing, card("Passport", 60.0), card("Charger", 180.0)], Vec::new());

    // The frame alone, as a click on its header selects it.
    send(&mut app, Message::SelectNode(Some(0)));
    send(&mut app, Message::Copy { chain: false, mermaid: false });
    assert_eq!(clipboard::read_text().as_deref(), Some("Packing\n- Passport\n- Charger"));
    send(&mut app, Message::Paste { world: Point::new(0.0, 900.0) });
    assert_eq!(app.nodes.len(), 6, "the frame and both its cards");
}

#[test]
fn select_chain_selects_a_card_and_what_leads_into_it() {
    let (nodes, edges) = a_chain_of_three();
    let mut app = app_with(nodes, edges);

    // ⌥-click on the last card.
    send(&mut app, Message::SelectChain(Some(2)));
    assert_eq!(app.selected, vec![0, 1, 2]);
    // The palette's, on what's selected.
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::SelectChain(None));
    assert_eq!(app.selected, vec![0, 1]);
}

#[test]
fn copy_as_mermaid_is_a_flowchart_and_nothing_else() {
    let _clipboard = clipboard::system::exclusive();
    let (nodes, edges) = a_chain_of_three();
    let mut app = app_with(nodes, edges);

    send(&mut app, Message::SelectNode(Some(2)));
    send(&mut app, Message::Copy { chain: true, mermaid: true });
    let text = clipboard::read_text().expect("a flowchart");
    assert!(text.starts_with("```mermaid\nflowchart LR\n    n1[\"Login fails"), "{text}");
    assert!(text.contains("    n1 -->|\"ruled out\"| n2\n    n2 --> n3\n"), "{text}");
    // No HTML beside it, which an app that pastes HTML would take instead.
    assert_eq!(clipboard::system::html(), None);
}

#[test]
fn a_pasted_flowchart_is_the_cards_it_draws() {
    let _clipboard = clipboard::system::exclusive();
    let mut app = app_with(Vec::new(), Vec::new());
    // As an AI tool writes one, its cards in a subgraph.
    let chart = "```mermaid\n\
                 flowchart LR\n    \
                 issue[Login fails] -->|ruled out| skew[Clock skew]\n    \
                 subgraph next [To try]\n        \
                 cache[Shared token cache] --> tabs[One tab at a time]\n    \
                 end\n    \
                 skew --> cache\n\
                 ```";
    *clipboard::system::held() = Some(clipboard::Clip::Text(chart.to_owned()));
    send(&mut app, Message::Paste { world: Point::ORIGIN });

    assert_eq!(app.nodes.len(), 5, "four cards and their group");
    assert_eq!(app.edges.len(), 3);
    assert_eq!(app.selected.len(), 5, "selected, as pasted cards are");
    let group = app.nodes.iter().position(|n| n.is_group()).expect("the subgraph");
    assert_eq!(app.nodes[group].label(), Some("To try"));
    let inside = model::enclosed_by(&app.nodes, group);
    let texts: Vec<&str> = inside.iter().filter_map(|&i| app.nodes[i].text.as_deref()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.contains(&"Shared token cache") && texts.contains(&"One tab at a time"));
    assert!(app.edges.iter().any(|e| e.label() == Some("ruled out")));

    send(&mut app, Message::Undo);
    assert!(app.nodes.is_empty() && app.edges.is_empty(), "one undo step");

    // Its source, when that's what's wanted.
    send(&mut app, Message::PasteAsText { world: Point::ORIGIN });
    assert_eq!(app.nodes.len(), 1);
    assert_eq!(app.nodes[0].text.as_deref(), Some(chart));

    // And one it can't read all of is text, rather than cards with a line lost.
    let broken = "flowchart LR\n    A --> B\n    A -- B";
    *clipboard::system::held() = Some(clipboard::Clip::Text(broken.to_owned()));
    send(&mut app, Message::Paste { world: Point::ORIGIN });
    assert_eq!(app.nodes.len(), 2);
    assert_eq!(app.nodes[1].text.as_deref(), Some(broken));
    *clipboard::system::held() = None;
}

#[test]
fn a_chain_copied_as_mermaid_pastes_back_as_its_cards() {
    let _clipboard = clipboard::system::exclusive();
    let (nodes, edges) = a_chain_of_three();
    let mut app = app_with(nodes, edges);
    send(&mut app, Message::SelectNode(Some(2)));
    send(&mut app, Message::Copy { chain: true, mermaid: true });
    send(&mut app, Message::Paste { world: Point::new(0.0, 800.0) });

    assert_eq!(app.nodes.len(), 6);
    assert_eq!(app.edges.len(), 4);
    let pasted: Vec<&str> = app.nodes[3..].iter().filter_map(|n| n.text.as_deref()).collect();
    assert_eq!(pasted, ["Login fails after the token refresh", "Clock skew", "The token cache is shared between tabs"]);
    assert!(app.edges[2..].iter().any(|e| e.label() == Some("ruled out")));
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
fn tab_adds_a_linked_card_beside_the_selected_one() {
    let mut app = app_with(plan(), Vec::new());
    send(&mut app, Message::SelectNode(Some(1)));
    send(&mut app, Message::AddLinkedNode);
    assert_eq!(app.nodes.len(), 4);
    let (eat, new) = (&app.nodes[1], &app.nodes[3]);
    assert!(new.x > eat.x + eat.width, "to its right");
    assert!(app.edges.iter().any(|e| e.from_node == "eat" && e.to_node == new.id), "linked from it");
    assert_eq!(selected_ids(&app), [new.id.as_str()]);
    // Its editor opens as a drag-created card's does; written and closed with
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

