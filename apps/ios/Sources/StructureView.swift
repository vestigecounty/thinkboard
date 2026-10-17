import SwiftUI
import UIKit

/// A just-added node to reveal and edit, with the parent it hangs off (nil for a
/// root cluster). Captured at add time so navigation doesn't depend on deriving
/// the parent from a possibly-stale graph.
struct PendingFocus: Equatable {
    let id: String
    let parent: String?
}

/// One screen in the descent: a pager over a set of sibling nodes — the children
/// of `parentId`, or the roots at the top level.
private struct Level: Hashable {
    /// The parent whose children are the siblings shown here; nil at the root.
    let parentId: String?
    /// Which sibling is focused when this level opens.
    let focus: String
}

/// The mobile structure view: read a node as a full-content card, swipe ◀▶
/// between its siblings (their edges peek in), scroll down within a card to its
/// following nodes, tap one to descend. The descent is a `NavigationStack`, so
/// the native back-swipe ascends and the standard navigation bar handles the
/// title, back button, and the notch/safe area.
struct StructureView: View {
    let graph: Graph
    let vault: Vault
    let fileName: String
    /// Save a node's edited text (id, new text).
    let onEdit: (String, String) -> Void
    /// An edit session ended (final text). Distinct from the debounced `onEdit`
    /// so a just-created node left empty can be rolled back.
    let onEndEdit: (String, String) -> Void
    /// Set a node's color to a preset code, or `nil` to clear it.
    let onSetColor: (String, String?) -> Void
    /// Add a child / sibling of a node, or a standalone root cluster.
    let onAddChild: (String) -> Void
    let onAddSibling: (String) -> Void
    let onAddRoot: () -> Void
    /// A just-added node to navigate to, focus, and start editing.
    @Binding var focusRequest: PendingFocus?
    /// Delete a node.
    let onRemove: (String) -> Void
    /// Signal that a text-edit session is starting (arms one undo snapshot).
    let onBeginEdit: () -> Void
    let canUndo: Bool
    let canRedo: Bool
    let onUndo: () -> Void
    let onRedo: () -> Void
    let onOpenAnother: () -> Void

    @State private var path: [Level] = []
    /// The image being shown fullscreen, if any, with the frame it grew from.
    @State private var preview: PreviewItem?
    /// Overrides the root level's focused sibling (so a new root cluster can be
    /// focused). Changing it recreates the root level with that selection.
    @State private var rootFocus: String?
    /// A focused node whose editor should open once its level appears.
    @State private var pendingEdit: String?

    var body: some View {
        NavigationStack(path: $path) {
            level(Level(parentId: nil, focus: rootFocus ?? graph.roots.first ?? ""))
                .id(rootFocus)
                .navigationDestination(for: Level.self, destination: level)
        }
        // The viewer lives above the whole stack so it covers the nav bar and
        // notch, and can grow from a card that sits anywhere in the descent.
        .overlay {
            if let preview {
                HeroImageViewer(image: preview.image, sourceFrame: preview.frame) {
                    self.preview = nil
                }
            }
        }
        // Navigate to (and edit) a just-added node.
        .onChange(of: focusRequest) { request in
            guard let request else { return }
            focus(request)
            focusRequest = nil
        }
    }

    /// Bring the added node on screen as the focused card and queue its editor:
    /// reselect it if it's a sibling of the current level, descend to it if it
    /// hangs off another node, or refocus the root for a new cluster.
    private func focus(_ request: PendingFocus) {
        pendingEdit = request.id
        guard let parent = request.parent else {
            // A new root cluster: focus it at the root level.
            path = []
            rootFocus = request.id
            return
        }
        if let last = path.last, last.parentId == parent {
            // A sibling of the current level — reselect by recreating the level.
            path[path.count - 1] = Level(parentId: parent, focus: request.id)
        } else {
            // Hangs off another node — descend so it's the focused card.
            path.append(Level(parentId: parent, focus: request.id))
        }
    }

    @ViewBuilder
    private func level(_ level: Level) -> some View {
        let siblings = level.parentId.map(graph.children(of:)) ?? graph.roots
        LevelView(
            graph: graph,
            vault: vault,
            siblings: siblings,
            focus: level.focus,
            onDescend: { parent, child in path.append(Level(parentId: parent, focus: child)) },
            onEdit: onEdit,
            onEndEdit: onEndEdit,
            onPreview: { image, frame in preview = PreviewItem(image: image, frame: frame) },
            actions: NodeActions(
                setColor: onSetColor,
                addChild: onAddChild,
                addSibling: onAddSibling,
                remove: onRemove
            ),
            onBeginEdit: onBeginEdit,
            onAddRoot: onAddRoot,
            pendingEdit: $pendingEdit,
            isRoot: level.parentId == nil,
            onPop: { if !path.isEmpty { path.removeLast() } }
        )
        .navigationTitle(title(for: level))
        .navigationBarTitleDisplayMode(.inline)
        // Keep the bar transparent on every level (not just the root at
        // scroll-top) so content always runs to the top, under the notch.
        .toolbarBackground(.hidden, for: .navigationBar)
        .toolbar {
            ToolbarItemGroup(placement: .navigationBarTrailing) {
                Button(action: onUndo) {
                    Image(systemName: "arrow.uturn.backward")
                }
                .disabled(!canUndo)
                Button(action: onRedo) {
                    Image(systemName: "arrow.uturn.forward")
                }
                .disabled(!canRedo)
                Button("Open", action: onOpenAnother)
            }
        }
    }

    /// A level is titled by its context — the node you descended from — so the
    /// title tracks where you are. The root shows the canvas name.
    private func title(for level: Level) -> String {
        if let parent = level.parentId {
            return graph.label(parent)
        }
        return (fileName as NSString).deletingPathExtension
    }
}

/// A peeking pager over sibling cards, with page dots along the bottom.
private struct LevelView: View {
    let graph: Graph
    let vault: Vault
    let siblings: [String]
    let onDescend: (_ parent: String, _ child: String) -> Void
    let onEdit: (_ nodeId: String, _ text: String) -> Void
    let onEndEdit: (_ nodeId: String, _ text: String) -> Void
    let onPreview: (_ image: UIImage, _ frame: CGRect) -> Void
    let actions: NodeActions
    let onBeginEdit: () -> Void
    let onAddRoot: () -> Void
    @Binding var pendingEdit: String?
    /// Whether this is the root level (an empty root offers "Add Card"; an empty
    /// deeper level pops back instead).
    let isRoot: Bool
    let onPop: () -> Void

    @State private var selection: String
    @State private var draft = ""
    /// The node being edited (the focused card or one of its leaf following
    /// cards), kept apart from `selection` — which can change out from under an
    /// edit when the pager swipes — and where to drop the caret.
    @State private var editingId: String?
    @State private var caretPoint: CGPoint?
    @State private var saveTask: Task<Void, Never>?
    /// Height of the on-screen keyboard, so the scroll view can lift the edited
    /// card above it.
    @State private var keyboardHeight: CGFloat = 0

    /// Passed to the focused card (and its leaf following cards) so their text
    /// becomes editable in place.
    private var editContext: EditContext {
        EditContext(
            editingId: editingId,
            draft: $draft,
            caretPoint: caretPoint,
            begin: { id, point in beginEditing(id, point) },
            commit: { endEditing() }
        )
    }

    init(
        graph: Graph,
        vault: Vault,
        siblings: [String],
        focus: String,
        onDescend: @escaping (_ parent: String, _ child: String) -> Void,
        onEdit: @escaping (_ nodeId: String, _ text: String) -> Void,
        onEndEdit: @escaping (_ nodeId: String, _ text: String) -> Void,
        onPreview: @escaping (_ image: UIImage, _ frame: CGRect) -> Void,
        actions: NodeActions,
        onBeginEdit: @escaping () -> Void,
        onAddRoot: @escaping () -> Void,
        pendingEdit: Binding<String?>,
        isRoot: Bool,
        onPop: @escaping () -> Void
    ) {
        self.graph = graph
        self.vault = vault
        self.siblings = siblings
        self.onDescend = onDescend
        self.onEdit = onEdit
        self.onEndEdit = onEndEdit
        self.onPreview = onPreview
        self.actions = actions
        self.onBeginEdit = onBeginEdit
        self.onAddRoot = onAddRoot
        _pendingEdit = pendingEdit
        self.isRoot = isRoot
        self.onPop = onPop
        _selection = State(initialValue: focus)
    }

    var body: some View {
        GeometryReader { proxy in
            let safeTop = proxy.safeAreaInsets.top
            let safeBottom = proxy.safeAreaInsets.bottom
            if siblings.isEmpty {
                // A blank root offers a way to start (no card to long-press); a
                // deeper level that just emptied (e.g. its only child abandoned)
                // pops back — see the `siblings` change handler below.
                if isRoot { emptyState } else { Color.clear }
            } else {
            PeekPager(ids: siblings, selection: $selection, peek: 20, spacing: 20) { id, cardWidth in
                ScrollViewReader { scroll in
                    ScrollView {
                        HStack(spacing: 0) {
                            NodeCard(
                                graph: graph, vault: vault, nodeId: id,
                                onDescend: onDescend, onPreview: onPreview,
                                // Only the focused sibling is editable.
                                edit: id == selection ? editContext : nil,
                                actions: actions
                            )
                            .frame(width: cardWidth)
                            // The page runs wider than the card; this empty lane
                            // fills the rest so the scroll indicator sits out in the
                            // gap (its right edge), not on the card's border.
                            Spacer(minLength: 0)
                        }
                        .padding(.top, 8)
                        // Extra room so the last content can scroll clear of the
                        // home indicator and the page dots — and, while editing,
                        // enough to lift any card above the keyboard.
                        .padding(.bottom, max(safeBottom + 40, keyboardHeight + 24))
                    }
                    .noBounceWhenContentFits()
                    // Bring the card being edited up above the keyboard, both when
                    // editing starts and once the keyboard's height is known. Only
                    // the focused page holds the edited card, so only it reacts.
                    .onChange(of: editingId) { if id == selection { scrollToEdited(scroll, $0) } }
                    .onChange(of: keyboardHeight) { _ in if id == selection { scrollToEdited(scroll, editingId) } }
                }
            }
            // A gradient fade to the background: content dissolves under the
            // transparent nav bar (top) and toward the bottom edge.
            .overlay(alignment: .top) {
                edgeFade(fadingDown: true)
                    .frame(height: safeTop + 32)
                    .ignoresSafeArea(edges: .top)
                    .allowsHitTesting(false)
            }
            .overlay(alignment: .bottom) {
                edgeFade(fadingDown: false)
                    .frame(height: safeBottom + 64)
                    .ignoresSafeArea(edges: .bottom)
                    .allowsHitTesting(false)
            }
            // Dots above the home indicator, on top of the bottom blur.
            .overlay(alignment: .bottom) {
                if siblings.count > 1, let index = siblings.firstIndex(of: selection) {
                    pageDots(current: index)
                        .padding(.bottom, safeBottom)
                }
            }
            // Only the bottom runs to the screen edge; the top stays below the
            // title bar (the transparent nav bar still lets content scroll under).
            .ignoresSafeArea(edges: .bottom)
            }
        }
        // Keep `safeBottom` at the home-indicator value even while editing: the
        // keyboard would otherwise inflate the reader's bottom inset and carry
        // the dots (and bottom fade) up with it. The scroll view makes room for
        // the keyboard from the separately-tracked `keyboardHeight` instead.
        .ignoresSafeArea(.keyboard, edges: .bottom)
        // Debounced live save while typing, so nothing's lost mid-edit; the
        // final value is committed on Done / swipe-away too.
        .onChange(of: draft) { text in
            guard let id = editingId else { return }
            saveTask?.cancel()
            saveTask = Task {
                try? await Task.sleep(nanoseconds: 500_000_000)
                guard !Task.isCancelled else { return }
                onEdit(id, text)
            }
        }
        // Swiping to another sibling commits the current edit.
        .onChange(of: selection) { _ in
            if editingId != nil { endEditing() }
        }
        // If the focused sibling is deleted, move to a remaining one; if the
        // level emptied entirely (a deeper level), pop back to the parent.
        .onChange(of: siblings) { newSiblings in
            if newSiblings.isEmpty {
                if !isRoot { onPop() }
            } else if !newSiblings.contains(selection) {
                selection = newSiblings.first!
            }
        }
        // Open the editor on a just-added node once it's the focused card here —
        // whether this level just appeared (descend/reselect) or was already up.
        .onAppear { autoFocusIfPending() }
        .onChange(of: pendingEdit) { _ in autoFocusIfPending() }
        // Track the keyboard so the scroll view can make room for it.
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { note in
            if let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect {
                keyboardHeight = frame.height
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            keyboardHeight = 0
        }
    }

    /// Scroll the card being edited to the top of the visible area, so its text
    /// (and the caret) clears the keyboard.
    private func scrollToEdited(_ scroll: ScrollViewProxy, _ id: String?) {
        guard let id else { return }
        withAnimation(.easeOut(duration: 0.25)) {
            scroll.scrollTo(id, anchor: .top)
        }
    }

    /// Start editing `id`, dropping the caret nearest `point` (or at the end when
    /// `point` is nil, e.g. a freshly added node). Commits any edit already in
    /// progress first (e.g. tapping a leaf while the focused card is edited).
    private func beginEditing(_ id: String, _ point: CGPoint?) {
        guard graph.node(id)?.kind == "text" else { return }
        if let current = editingId, current != id { endEditing() }
        // Arm one undo snapshot for this edit session.
        onBeginEdit()
        draft = graph.node(id)?.text ?? ""
        editingId = id
        caretPoint = point
    }

    /// If a just-added node has landed as the focused card here, open its editor.
    /// A short delay lets any navigation settle before the keyboard rises.
    private func autoFocusIfPending() {
        guard let target = pendingEdit, target == selection,
              graph.node(target)?.kind == "text" else { return }
        pendingEdit = nil
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
            guard selection == target, editingId == nil else { return }
            beginEditing(target, nil)
        }
    }

    /// Finish editing and save to the node that was being edited (not the current
    /// selection, which may have moved).
    private func endEditing() {
        guard let id = editingId else { return }
        saveTask?.cancel()
        let text = draft
        caretPoint = nil
        editingId = nil
        onEndEdit(id, text)
    }

    /// Shown when the canvas has no nodes: nothing to long-press, so offer a
    /// direct way to add the first note.
    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 44))
                .foregroundStyle(.secondary)
            Text("This canvas is empty.")
                .foregroundStyle(.secondary)
            Button {
                onAddRoot()
            } label: {
                Label("Add Card", systemImage: "plus")
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// A page-control-style row: one dot per sibling, the current one filled.
    private func pageDots(current: Int) -> some View {
        HStack(spacing: 7) {
            ForEach(siblings.indices, id: \.self) { i in
                Circle()
                    .fill(i == current ? Color.primary : Color.secondary.opacity(0.35))
                    .frame(width: i == current ? 8 : 6, height: i == current ? 8 : 6)
            }
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
        .frame(maxWidth: .infinity)
    }
}

private extension View {
    /// Suppress scroll bounce when the content already fits (iOS 16.4+);
    /// a no-op on earlier versions.
    @ViewBuilder func noBounceWhenContentFits() -> some View {
        if #available(iOS 16.4, *) {
            scrollBounceBehavior(.basedOnSize)
        } else {
            self
        }
    }
}

/// A gradient fade to the system background — content dissolves at an edge
/// rather than sharply overlapping the bar.
private func edgeFade(fadingDown: Bool) -> some View {
    LinearGradient(
        colors: fadingDown
            ? [Color(uiColor: .systemBackground), .clear]
            : [.clear, Color(uiColor: .systemBackground)],
        startPoint: .top,
        endPoint: .bottom
    )
}
