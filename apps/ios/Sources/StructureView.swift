import SwiftUI

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
/// the native back-swipe ascends.
struct StructureView: View {
    let graph: Graph
    let vault: Vault
    let fileName: String
    let onOpenAnother: () -> Void

    @State private var path: [Level] = []

    var body: some View {
        NavigationStack(path: $path) {
            level(Level(parentId: nil, focus: graph.roots.first ?? ""))
                .navigationDestination(for: Level.self, destination: level)
        }
    }

    @ViewBuilder
    private func level(_ level: Level) -> some View {
        let siblings = level.parentId.map(graph.children(of:)) ?? graph.roots
        LevelView(graph: graph, vault: vault, siblings: siblings, focus: level.focus) { parent, child in
            path.append(Level(parentId: parent, focus: child))
        }
        .navigationTitle(title(for: level))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarTrailing) {
                Button("Open", action: onOpenAnother)
            }
        }
    }

    /// A level is titled by its context — the node you descended from (whose
    /// children are the siblings here) — so the title tracks where you are and
    /// the back button names where it returns to. The root shows the canvas name.
    private func title(for level: Level) -> String {
        if let parent = level.parentId {
            return graph.label(parent)
        }
        return (fileName as NSString).deletingPathExtension
    }
}

/// A peeking pager over sibling cards, with an explicit position indicator.
private struct LevelView: View {
    let graph: Graph
    let vault: Vault
    let siblings: [String]
    let onDescend: (_ parent: String, _ child: String) -> Void

    @State private var selection: String

    init(
        graph: Graph,
        vault: Vault,
        siblings: [String],
        focus: String,
        onDescend: @escaping (_ parent: String, _ child: String) -> Void
    ) {
        self.graph = graph
        self.vault = vault
        self.siblings = siblings
        self.onDescend = onDescend
        _selection = State(initialValue: focus)
    }

    var body: some View {
        VStack(spacing: 8) {
            if siblings.count > 1, let idx = siblings.firstIndex(of: selection) {
                siblingIndicator(index: idx)
            }
            PeekPager(ids: siblings, selection: $selection, peek: 20, spacing: 12) { id in
                NodeCard(graph: graph, vault: vault, nodeId: id, onDescend: onDescend)
                    .background(
                        RoundedRectangle(cornerRadius: 20)
                            .fill(Color.secondary.opacity(0.06))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 20)
                            .strokeBorder(Color.secondary.opacity(0.18))
                    )
            }
        }
        .padding(.vertical, 8)
    }

    private func siblingIndicator(index: Int) -> some View {
        HStack(spacing: 14) {
            Button { move(to: index - 1) } label: {
                Image(systemName: "chevron.left")
            }
            .disabled(index == 0)

            Text("\(index + 1) of \(siblings.count)")
                .monospacedDigit()
                .foregroundStyle(.secondary)

            Button { move(to: index + 1) } label: {
                Image(systemName: "chevron.right")
            }
            .disabled(index == siblings.count - 1)
        }
        .font(.footnote.weight(.semibold))
        .padding(.horizontal, 16)
        .padding(.vertical, 7)
        .background(Capsule().fill(Color.secondary.opacity(0.14)))
    }

    private func move(to index: Int) {
        guard siblings.indices.contains(index) else { return }
        withAnimation(.interactiveSpring(response: 0.32, dampingFraction: 0.86)) {
            selection = siblings[index]
        }
    }
}
