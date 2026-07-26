import SwiftUI

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
}

/// The content of a single node — an image (for image file nodes) or its fully
/// rendered markdown. Reused for both the focused card and the following cards,
/// so full content is always shown; there is no separate detail view.
struct NodeContent: View {
    let graph: Graph
    let vault: Vault
    let nodeId: String

    var body: some View {
        if let node = graph.node(nodeId) {
            if node.kind == "file", let file = node.file, isImagePath(file) {
                if let image = vault.image(forRelativePath: file) {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                } else {
                    Label("Missing image · \(file)", systemImage: "photo")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            } else if let text = node.text, !text.isEmpty {
                MarkdownText(source: text)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if let file = node.file {
                Label(file, systemImage: "doc")
                    .foregroundStyle(.secondary)
            } else {
                Text("(empty note)")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

/// A node as a full-content card: its content, then its following nodes as
/// full-content cards. Scrolling is enabled only when the content overflows the
/// card.
struct NodeCard: View {
    let graph: Graph
    let vault: Vault
    let nodeId: String
    let onDescend: (_ parent: String, _ child: String) -> Void

    @State private var contentHeight: CGFloat = 0

    var body: some View {
        let children = graph.children(of: nodeId)
        GeometryReader { geo in
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    NodeContent(graph: graph, vault: vault, nodeId: nodeId)

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
                                onDescend: onDescend
                            )
                        }
                    }
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    GeometryReader { proxy in
                        Color.clear.preference(key: ContentHeightKey.self, value: proxy.size.height)
                    }
                )
            }
            // No pointless scroll/bounce when everything already fits.
            .scrollDisabled(contentHeight <= geo.size.height)
            .onPreferenceChange(ContentHeightKey.self) { contentHeight = $0 }
        }
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

    var body: some View {
        let hasChildren = !graph.children(of: nodeId).isEmpty
        let tint = graph.color(of: nodeId).map { Color($0) }
        let card = NodeContent(graph: graph, vault: vault, nodeId: nodeId)
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

        if hasChildren {
            card
                .contentShape(Rectangle())
                .onTapGesture { onDescend(parentId, nodeId) }
        } else {
            card
        }
    }
}

/// Measures the scrollable content's height so the card can disable scrolling
/// when it fits.
private struct ContentHeightKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
