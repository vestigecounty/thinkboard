import SwiftUI
import UIKit

extension Color {
    /// Bridge the core's resolved RGBA to a SwiftUI color.
    init(_ rgba: RgbaColor) {
        self.init(
            red: Double(rgba.r),
            green: Double(rgba.g),
            blue: Double(rgba.b),
            opacity: Double(rgba.a)
        )
    }

    /// A 0xRRGGBB literal as a SwiftUI color.
    init(hex: UInt) {
        self.init(
            red: Double((hex >> 16) & 0xff) / 255,
            green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255
        )
    }
}

/// The Obsidian canvas color presets `"1"`–`"6"`, matching the desktop palette
/// (see `core::model::parse_color`). Stored as the preset code so files round-trip.
struct NodeColor: Identifiable {
    let code: String
    let name: String
    let color: Color
    var id: String { code }

    static let presets: [NodeColor] = [
        NodeColor(code: "1", name: "Red", color: Color(hex: 0xFB464C)),
        NodeColor(code: "2", name: "Orange", color: Color(hex: 0xE9973F)),
        NodeColor(code: "3", name: "Yellow", color: Color(hex: 0xE0DE71)),
        NodeColor(code: "4", name: "Green", color: Color(hex: 0x44CF6E)),
        NodeColor(code: "5", name: "Cyan", color: Color(hex: 0x53DFDD)),
        NodeColor(code: "6", name: "Purple", color: Color(hex: 0xA882FF)),
    ]

    /// A filled circle in this color, baked into an image so it keeps its color
    /// in the menu (SF Symbols render monochrome there).
    var swatch: UIImage {
        let size = CGSize(width: 22, height: 22)
        return UIGraphicsImageRenderer(size: size).image { ctx in
            UIColor(color).setFill()
            ctx.cgContext.fillEllipse(in: CGRect(origin: .zero, size: size).insetBy(dx: 1, dy: 1))
        }.withRenderingMode(.alwaysOriginal)
    }
}

/// The content of a single node — an image (for image file nodes) or its fully
/// rendered markdown. Reused for both the focused card and the following cards,
/// so full content is always shown; there is no separate detail view.
struct NodeContent: View {
    let graph: Graph
    let vault: Vault
    let nodeId: String
    /// When set, tapping an image opens it fullscreen, growing out of its
    /// on-screen position. Left nil for the small following cards, whose tap
    /// descends instead. The closure gets the image and its global frame.
    var onPreview: ((UIImage, CGRect) -> Void)? = nil
    /// When set, the (text) node is editable: tap the text to edit it in place,
    /// caret at the tap. Left nil for following cards and non-focused siblings.
    var edit: EditContext? = nil

    var body: some View {
        content
    }

    @ViewBuilder private var content: some View {
        if let node = graph.node(nodeId) {
            if node.kind == "file", let file = node.file, isImagePath(file) {
                imageContent(file)
            } else if node.kind == "text" {
                // Route every text node — including a freshly added, still-empty
                // one — through here so it stays tappable-to-edit.
                textContent(node: node, text: node.text ?? "")
            } else if let file = node.file {
                Label(file, systemImage: "doc")
                    .foregroundStyle(.secondary)
            } else {
                Text("(empty card)")
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// A text node: rendered markdown, or — when this card is editable and being
    /// edited — an in-place editor. A spatial tap on the rendered text starts
    /// editing with the caret dropped where you tapped.
    @ViewBuilder private func textContent(node: Node, text: String) -> some View {
        if let edit, node.kind == "text" {
            if edit.editingId == nodeId {
                InlineTextEditor(text: edit.draft, caretPoint: edit.caretPoint, onCommit: edit.commit)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                renderedText(text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    // SpatialTapGesture (iOS 16-safe) reports the tap point, so
                    // the caret can land under it.
                    .gesture(SpatialTapGesture().onEnded { edit.begin(nodeId, $0.location) })
            }
        } else {
            renderedText(text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Rendered markdown, or a muted placeholder for an empty card (so a fresh
    /// node still shows a tappable target).
    @ViewBuilder private func renderedText(_ text: String) -> some View {
        if text.isEmpty {
            Text("Empty card")
                .italic()
                .foregroundStyle(.secondary)
        } else {
            MarkdownText(source: text)
        }
    }

    @ViewBuilder private func imageContent(_ file: String) -> some View {
        if let image = vault.image(forRelativePath: file) {
            let view = Image(uiImage: image)
                .resizable()
                .scaledToFit()
                .frame(maxWidth: .infinity)
                .clipShape(RoundedRectangle(cornerRadius: 10))
            if let onPreview {
                // A clear overlay captures both the tap and — through its
                // geometry — the image's on-screen frame, so the viewer can
                // grow out of exactly that spot.
                view.overlay(
                    GeometryReader { geo in
                        Color.clear
                            .contentShape(Rectangle())
                            .onTapGesture { onPreview(image, geo.frame(in: .global)) }
                    }
                )
            } else {
                view
            }
        } else {
            Label("Missing image · \(file)", systemImage: "photo")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

/// A tapped image, to be shown fullscreen. Carries the thumbnail's global frame
/// so the viewer can grow from — and collapse back into — exactly that spot.
struct PreviewItem: Identifiable {
    let id = UUID()
    let image: UIImage
    let frame: CGRect
}

/// A fullscreen image viewer that opens *from* the tapped thumbnail: it grows
/// out of `sourceFrame` while the black backdrop fades in, and a swipe throws it
/// off in the swipe's direction (up or down) while the backdrop fades back out.
/// A tap collapses it back into the card. Pinch or double-tap to zoom, pan when
/// zoomed. Presented as a root-level overlay (not a `fullScreenCover`) so the
/// transition is ours to animate rather than the system's slide-up.
struct HeroImageViewer: View {
    let image: UIImage
    /// The source thumbnail's frame, in global coordinates.
    let sourceFrame: CGRect
    let onClose: () -> Void

    @State private var open = false        // false = at the thumbnail, true = fullscreen
    @State private var scale: CGFloat = 1   // committed zoom
    @State private var pinch: CGFloat = 1   // live pinch
    @State private var pan: CGSize = .zero  // committed pan (when zoomed)
    @State private var drag: CGSize = .zero // live drag

    private var zoom: CGFloat { scale * pinch }

    var body: some View {
        GeometryReader { geo in
            // The GeometryReader fills the screen (safe areas ignored below), so
            // its local space *is* the global screen space and the captured
            // source frame maps straight in — no safe-area offset to correct.
            let originGlobal = geo.frame(in: .global)
            let source = sourceFrame.offsetBy(dx: -originGlobal.minX, dy: -originGlobal.minY)
            let full = fitRect(image.size, in: geo.size)
            let frame = open ? full : source

            ZStack {
                Color.black.opacity(backdropOpacity(geo))
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(width: frame.width, height: frame.height)
                    .scaleEffect(zoom)
                    .position(
                        x: frame.midX + pan.width + drag.width,
                        y: frame.midY + pan.height + drag.height
                    )
            }
            .frame(width: geo.size.width, height: geo.size.height)
            .contentShape(Rectangle())
            .gesture(zoomAndDrag(geo))
            .onTapGesture(count: 2) { toggleZoom() }
            .onTapGesture { close() }
            .onAppear {
                withAnimation(.spring(response: 0.34, dampingFraction: 0.86)) { open = true }
            }
        }
        .ignoresSafeArea()
        .statusBarHidden()
    }

    private func zoomAndDrag(_ geo: GeometryProxy) -> some Gesture {
        MagnificationGesture()
            .onChanged { pinch = $0 }
            .onEnded { value in
                scale = min(max(scale * value, 1), 4)
                pinch = 1
                if scale == 1 {
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) { pan = .zero }
                }
            }
            .simultaneously(with:
                DragGesture()
                    .onChanged { drag = $0.translation }
                    .onEnded { value in
                        if zoom > 1 {
                            // Pan: commit the drag into the offset.
                            pan.width += value.translation.width
                            pan.height += value.translation.height
                            drag = .zero
                        } else if abs(value.translation.height) > 110 {
                            dismiss(direction: value.translation.height < 0 ? -1 : 1, geo: geo)
                        } else {
                            withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) { drag = .zero }
                        }
                    }
            )
    }

    /// Throw the image off the screen in the swipe's direction (up or down) while
    /// the backdrop fades, then tear the viewer down.
    private func dismiss(direction: CGFloat, geo: GeometryProxy) {
        withAnimation(.easeOut(duration: 0.26)) {
            drag.height = direction * geo.size.height * 1.15
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.24) { onClose() }
    }

    /// A tap collapses the image back into the card it grew from — or, if it's
    /// zoomed in, just resets the zoom.
    private func close() {
        if zoom > 1 {
            withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) { scale = 1; pan = .zero }
            return
        }
        withAnimation(.spring(response: 0.32, dampingFraction: 0.88)) { open = false }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.26) { onClose() }
    }

    private func toggleZoom() {
        withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) {
            if scale > 1 { scale = 1; pan = .zero } else { scale = 2 }
        }
    }

    /// Fades in as the image opens, out as it closes or is swiped away.
    private func backdropOpacity(_ geo: GeometryProxy) -> Double {
        guard open else { return 0 }
        if zoom > 1 { return 1 }
        let fade = min(abs(drag.height) / (geo.size.height * 0.6), 1)
        return 1 - fade
    }

    /// The image drawn `scaledToFit` and centered in `size` — the fullscreen
    /// frame it grows into.
    private func fitRect(_ imageSize: CGSize, in size: CGSize) -> CGRect {
        guard imageSize.width > 0, imageSize.height > 0 else {
            return CGRect(origin: .zero, size: size)
        }
        let s = min(size.width / imageSize.width, size.height / imageSize.height)
        let w = imageSize.width * s, h = imageSize.height * s
        return CGRect(x: (size.width - w) / 2, y: (size.height - h) / 2, width: w, height: h)
    }
}

/// What an editable card needs to edit its text in place. Bundled so it can be
/// passed down as a single optional (nil = not editable). One context is shared
/// by every editable card in a level; each compares `editingId` to its own id to
/// know whether *it* is the one being edited.
struct EditContext {
    /// The node currently being edited, if any.
    let editingId: String?
    let draft: Binding<String>
    /// Where to drop the caret when editing begins, in the text's local space.
    let caretPoint: CGPoint?
    /// Begin editing `nodeId`, dropping the caret nearest `point`.
    let begin: (String, CGPoint) -> Void
    /// Finish editing (and save).
    let commit: () -> Void
}

/// Long-press actions available on any card. Passed down so both the focused
/// card and the nested following cards can offer the same menu.
struct NodeActions {
    /// Set a node's color to a preset code, or `nil` to clear it.
    let setColor: (String, String?) -> Void
    /// Add a child of this node.
    let addChild: (String) -> Void
    /// Add a sibling of this node (a new cluster if it's a root).
    let addSibling: (String) -> Void
    let remove: (String) -> Void
}

/// The long-press context menu for a card: add a child or sibling, pick a color
/// (the desktop presets) or clear it, and delete the node.
@ViewBuilder
func nodeContextMenu(graph: Graph, actions: NodeActions, id: String) -> some View {
    Button {
        actions.addChild(id)
    } label: {
        Label("Add Child", systemImage: "arrow.turn.down.right")
    }
    Button {
        actions.addSibling(id)
    } label: {
        Label("Add Sibling", systemImage: "plus")
    }
    Divider()
    Menu {
        ForEach(NodeColor.presets) { preset in
            Button {
                actions.setColor(id, preset.code)
            } label: {
                Label {
                    Text(preset.name)
                } icon: {
                    Image(uiImage: preset.swatch)
                }
            }
        }
        Divider()
        Button {
            actions.setColor(id, nil)
        } label: {
            Label("No Color", systemImage: "slash.circle")
        }
    } label: {
        Label("Color", systemImage: "paintpalette")
    }
    Button(role: .destructive) {
        actions.remove(id)
    } label: {
        Label("Remove", systemImage: "trash")
    }
}

extension View {
    /// Attach the card's long-press menu, unless the card is being edited — a
    /// long press there should reach the text editor (selection), not the menu.
    @ViewBuilder
    func nodeMenu(graph: Graph, actions: NodeActions, id: String, enabled: Bool) -> some View {
        if enabled {
            contextMenu { nodeContextMenu(graph: graph, actions: actions, id: id) }
        } else {
            self
        }
    }
}

/// An in-place editor for a node's markdown source, backed by `UITextView` so we
/// can focus it and drop the caret at a tapped point — neither of which SwiftUI's
/// `TextEditor` allows on iOS 16. It grows with its content; scrolling is the
/// outer ScrollView's job.
struct InlineTextEditor: UIViewRepresentable {
    @Binding var text: String
    /// A point (in the view's own space) to place the caret nearest to on focus.
    let caretPoint: CGPoint?
    let onCommit: () -> Void

    func makeUIView(context: Context) -> UITextView {
        let tv = UITextView()
        tv.delegate = context.coordinator
        tv.font = .preferredFont(forTextStyle: .body)
        tv.backgroundColor = .clear
        tv.textContainerInset = .zero
        // Align the text to the top-left, matching the rendered markdown, so the
        // tapped point maps to about the same caret.
        tv.textContainer.lineFragmentPadding = 0
        tv.isScrollEnabled = false
        tv.text = text

        // A Done button above the keyboard finishes editing.
        let bar = UIToolbar()
        bar.sizeToFit()
        bar.items = [
            UIBarButtonItem(barButtonSystemItem: .flexibleSpace, target: nil, action: nil),
            UIBarButtonItem(barButtonSystemItem: .done, target: context.coordinator,
                            action: #selector(Coordinator.done)),
        ]
        tv.inputAccessoryView = bar
        context.coordinator.textView = tv
        return tv
    }

    func updateUIView(_ tv: UITextView, context: Context) {
        context.coordinator.parent = self
        if tv.text != text { tv.text = text }
        // On first appearance, focus and drop the caret near the tapped point.
        guard !context.coordinator.didFocus else { return }
        context.coordinator.didFocus = true
        DispatchQueue.main.async {
            tv.becomeFirstResponder()
            let caret = caretPoint.flatMap { tv.closestPosition(to: $0) } ?? tv.endOfDocument
            tv.selectedTextRange = tv.textRange(from: caret, to: caret)
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        let width = proposal.width ?? uiView.frame.width
        let fitted = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: fitted.height)
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: InlineTextEditor
        weak var textView: UITextView?
        var didFocus = false

        init(_ parent: InlineTextEditor) { self.parent = parent }

        func textViewDidChange(_ tv: UITextView) { parent.text = tv.text }
        func textViewDidEndEditing(_ tv: UITextView) { parent.onCommit() }

        @objc func done() { textView?.resignFirstResponder() }
    }
}

/// A node as a full-content card: its content, then its following nodes as
/// full-content cards. The card is intrinsic-height — its border spans exactly
/// its content (including all the following cards) — and scrolling happens one
/// level up, so nested cards never clip against a fixed box.
struct NodeCard: View {
    let graph: Graph
    let vault: Vault
    let nodeId: String
    let onDescend: (_ parent: String, _ child: String) -> Void
    /// Open the focused card's image fullscreen, from its on-screen frame.
    let onPreview: (UIImage, CGRect) -> Void
    /// Tap-to-edit for the focused text node; nil when this card isn't editable.
    var edit: EditContext? = nil
    /// Long-press actions for this card and its following cards.
    let actions: NodeActions

    var body: some View {
        let children = graph.children(of: nodeId)
        let tint = graph.color(of: nodeId).map { Color($0) }
        VStack(alignment: .leading, spacing: 18) {
            NodeContent(graph: graph, vault: vault, nodeId: nodeId, onPreview: onPreview, edit: edit)
                // Scroll anchor for lifting this node above the keyboard when edited.
                .id(nodeId)
                // Long-press the parent card's own content for its menu, unless
                // it's being edited (then long-press is the editor's own).
                .nodeMenu(graph: graph, actions: actions, id: nodeId, enabled: edit?.editingId != nodeId)

            if !children.isEmpty {
                Label("Following", systemImage: "arrow.turn.down.right")
                    .font(.caption.weight(.semibold).smallCaps())
                    .foregroundStyle(.secondary)
                    .padding(.top, 4)

                ForEach(children, id: \.self) { child in
                    FollowingCard(
                        graph: graph,
                        vault: vault,
                        parentId: nodeId,
                        nodeId: child,
                        onDescend: onDescend,
                        edit: edit,
                        actions: actions
                    )
                    // Scroll anchor for lifting a leaf above the keyboard when edited.
                    .id(child)
                }
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill((tint ?? Color.secondary).opacity(tint == nil ? 0.06 : 0.10))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 20)
                .strokeBorder(tint ?? Color.secondary.opacity(0.18), lineWidth: tint == nil ? 1 : 2)
        )
    }
}

/// A following node, shown as a full-content card. Only nodes that lead
/// somewhere (have children) are tappable — a leaf just shows its content.
private struct FollowingCard: View {
    let graph: Graph
    let vault: Vault
    let parentId: String
    let nodeId: String
    let onDescend: (_ parent: String, _ child: String) -> Void
    var edit: EditContext? = nil
    let actions: NodeActions

    var body: some View {
        let hasChildren = !graph.children(of: nodeId).isEmpty
        let tint = graph.color(of: nodeId).map { Color($0) }
        // A branch node descends on tap, so it stays non-editable (edit it after
        // descending, where it's the focused card). A leaf — otherwise a dead
        // end — takes the edit context so tapping its text edits it in place.
        let card = NodeContent(graph: graph, vault: vault, nodeId: nodeId, edit: hasChildren ? nil : edit)
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .fill((tint ?? Color.secondary).opacity(tint == nil ? 0.10 : 0.14))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14)
                    .strokeBorder(tint ?? Color.clear, lineWidth: tint == nil ? 0 : 1.5)
            )
            .overlay(alignment: .bottomTrailing) {
                if hasChildren {
                    Image(systemName: "arrow.down.forward")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .padding(10)
                }
            }

        Group {
            if hasChildren {
                card
                    .contentShape(Rectangle())
                    .onTapGesture { onDescend(parentId, nodeId) }
            } else {
                card
            }
        }
        // Long-press a nested card for the same menu as the parent, unless it's
        // being edited (a leaf in edit mode).
        .nodeMenu(graph: graph, actions: actions, id: nodeId, enabled: edit?.editingId != nodeId)
    }
}
