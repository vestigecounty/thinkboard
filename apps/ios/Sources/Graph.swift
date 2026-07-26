import Foundation

/// The canvas as a navigable graph, built once from the parsed nodes + edges.
/// Adjacency is precomputed so the structure view can look up a node's children
/// (its "following" nodes) and siblings cheaply while navigating.
struct Graph {
    private let nodesById: [String: Node]
    private let childIds: [String: [String]]
    private let parentIds: [String: [String]]

    /// Entry points: nodes with no incoming edge. Falls back to every node if
    /// the graph is fully cyclic, so navigation always has somewhere to start.
    let roots: [String]

    init(nodes: [Node], edges: [Edge]) {
        var byId: [String: Node] = [:]
        var order: [String] = []
        for node in nodes {
            byId[node.id] = node
            order.append(node.id)
        }

        var children: [String: [String]] = [:]
        var parents: [String: [String]] = [:]
        var hasParent = Set<String>()
        for edge in edges {
            // Skip dangling edges (endpoint not among the nodes).
            guard byId[edge.fromNode] != nil, byId[edge.toNode] != nil else { continue }
            children[edge.fromNode, default: []].append(edge.toNode)
            parents[edge.toNode, default: []].append(edge.fromNode)
            hasParent.insert(edge.toNode)
        }

        self.nodesById = byId
        self.childIds = children
        self.parentIds = parents

        let roots = order.filter { !hasParent.contains($0) }
        self.roots = roots.isEmpty ? order : roots
    }

    func node(_ id: String) -> Node? { nodesById[id] }
    func children(of id: String) -> [String] { childIds[id] ?? [] }
    func parents(of id: String) -> [String] { parentIds[id] ?? [] }

    /// A short one-line label for a node: its first non-empty text line, else
    /// its file name, else a placeholder.
    func label(_ id: String) -> String {
        guard let node = nodesById[id] else { return "(missing)" }
        if let line = node.text?
            .split(whereSeparator: \.isNewline)
            .map({ $0.trimmingCharacters(in: .whitespaces) })
            .first(where: { !$0.isEmpty })
        {
            // Drop a leading markdown heading marker for the label.
            return line.drop(while: { $0 == "#" }).trimmingCharacters(in: .whitespaces)
        }
        return node.file ?? "(untitled)"
    }
}
