import SwiftUI
import UniformTypeIdentifiers

/// Root screen: open a folder (a vault, or any folder holding a canvas and its
/// images), pick the canvas, and explore it in the structure view. Opening a
/// folder — rather than a single `.canvas` — is what grants access to the
/// images that file nodes reference.
/// A freshly-added node awaiting its first text, and the undo-stack depth from
/// before it was created (so an empty editor-close can roll back cleanly).
private struct Provisional {
    let id: String
    let undoDepth: Int
}

struct ContentView: View {
    @State private var vault: Vault?
    @State private var graph: Graph?
    @State private var canvasName: String?
    /// The parsed document + the file it came from, kept so edits can be saved.
    @State private var document: Document?
    @State private var canvasURL: URL?
    /// Canvases found in the opened folder, shown as a chooser when there's
    /// more than one.
    @State private var candidates: [URL] = []
    @State private var errorMessage: String?
    @State private var importing = false
    /// The file's last-seen modification date, to detect external changes
    /// (edited on another device, synced via iCloud, …) and reload.
    @State private var fileMtime: Date?
    /// Snapshot-based undo/redo of the whole document (JSON snapshots), coarse:
    /// one step per user action.
    @State private var undoStack: [String] = []
    @State private var redoStack: [String] = []
    /// Set when an edit session begins; the first real change of the session
    /// captures a single snapshot, so a whole edit is one undo step, not many.
    @State private var armEditSnapshot = false
    private let undoLimit = 50
    /// A just-added node the structure view should focus and start editing,
    /// with the parent it hangs off (captured now, from the document).
    @State private var focusRequest: PendingFocus?
    /// A just-created node awaiting its first text. If its editor closes empty,
    /// the creation is rolled back. `undoDepth` is the undo-stack size from
    /// before the add, so rolling back also drops the add's checkpoint(s).
    @State private var provisional: Provisional?
    @Environment(\.scenePhase) private var scenePhase
    private let reloadTimer = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        Group {
            if let graph, let vault, let canvasName {
                StructureView(
                    graph: graph, vault: vault, fileName: canvasName,
                    onEdit: editNodeText,
                    onEndEdit: endEditNode,
                    onSetColor: setNodeColor,
                    onAddChild: addChild,
                    onAddSibling: addSibling,
                    onAddRoot: addRoot,
                    focusRequest: $focusRequest,
                    onRemove: removeNode,
                    onBeginEdit: { armEditSnapshot = true },
                    canUndo: !undoStack.isEmpty,
                    canRedo: !redoStack.isEmpty,
                    onUndo: undo,
                    onRedo: redo,
                    onOpenAnother: { importing = true }
                )
                .id(canvasName)
            } else if candidates.count > 1 {
                canvasChooser
            } else {
                openScreen
            }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.folder], onCompletion: openFolder)
        .task { restoreRecent() }
        .onReceive(reloadTimer) { _ in reloadIfChanged() }
        .onChange(of: scenePhase) { phase in
            if phase == .active { reloadIfChanged() }
        }
    }

    /// On launch, reopen the most recently opened canvas by resolving its saved
    /// folder bookmark. Any failure (folder moved, access lapsed) falls back to
    /// the open screen and forgets the stale entry.
    private func restoreRecent() {
        guard vault == nil, graph == nil, let saved = RecentCanvas.load() else { return }

        var stale = false
        guard let root = try? URL(
            resolvingBookmarkData: saved.bookmark,
            options: [],
            relativeTo: nil,
            bookmarkDataIsStale: &stale
        ) else {
            RecentCanvas.clear()
            return
        }

        let opened = Vault(root: root)
        let canvasURL = root.appendingPathComponent(saved.relativePath)
        guard FileManager.default.fileExists(atPath: canvasURL.path) else {
            RecentCanvas.clear()
            return
        }
        vault = opened
        openCanvas(canvasURL)
    }

    private var openScreen: some View {
        VStack(spacing: 16) {
            Image(systemName: errorMessage == nil
                ? "point.3.connected.trianglepath.dotted"
                : "exclamationmark.triangle")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text(errorMessage ?? "Open the folder that holds your canvas (and its images) to explore it.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Open Folder") { importing = true }
                .buttonStyle(.borderedProminent)
            Text("core \(coreVersion())")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(40)
    }

    private var canvasChooser: some View {
        NavigationStack {
            List(candidates, id: \.self) { url in
                Button {
                    openCanvas(url)
                } label: {
                    Label(url.lastPathComponent, systemImage: "square.grid.2x2")
                }
            }
            .navigationTitle("Choose a canvas")
            .toolbar {
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Open Folder") { importing = true }
                }
            }
        }
    }

    private func openFolder(_ result: Result<URL, Error>) {
        errorMessage = nil
        do {
            let url = try result.get()
            let opened = Vault(root: url)
            let canvases = opened.canvasFiles()
            guard !canvases.isEmpty else {
                errorMessage = "No .canvas files found in “\(url.lastPathComponent)”."
                return
            }
            vault = opened
            graph = nil
            canvasName = nil
            if canvases.count == 1 {
                openCanvas(canvases[0])
            } else {
                candidates = canvases
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func openCanvas(_ url: URL) {
        guard let vault else { return }
        do {
            let json = try String(contentsOf: url, encoding: .utf8)
            let doc = try Document.parse(json: json)
            document = doc
            canvasURL = url
            fileMtime = modificationDate(url)
            graph = Graph(nodes: doc.nodes(), edges: doc.edges())
            canvasName = url.lastPathComponent
            candidates = []
            undoStack.removeAll()
            redoStack.removeAll()
            provisional = nil
            // Remember this as the most recent, to restore on next launch.
            RecentCanvas.save(vaultRoot: vault.root, canvasURL: url)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Set a node's text, write the canvas back to disk, and refresh the graph.
    private func editNodeText(_ nodeId: String, _ text: String) {
        guard let document else { return }
        // Ignore no-op writes (e.g. the draft being seeded when editing begins).
        guard (graph?.node(nodeId)?.text ?? "") != text else { return }
        // One undo step per session: snapshot before the session's first change.
        if armEditSnapshot { checkpoint(); armEditSnapshot = false }
        _ = document.setNodeText(id: nodeId, text: text)
        persist(document)
    }

    /// Set a node's color to a preset code (or `nil` to clear it) and save.
    private func setNodeColor(_ nodeId: String, _ code: String?) {
        guard let document, graph?.node(nodeId)?.color != code else { return }
        checkpoint()
        _ = document.setNodeColor(id: nodeId, color: code)
        persist(document)
    }

    /// Delete a node (and its edges) and save.
    private func removeNode(_ nodeId: String) {
        guard let document else { return }
        checkpoint()
        _ = document.removeNode(id: nodeId)
        persist(document)
    }

    /// Add an (empty) child of a node, placed and linked by the core.
    private func addChild(_ nodeId: String) {
        guard let document else { return }
        added { document.addChild(parentId: nodeId, text: "") }
    }

    /// Add an (empty) sibling of a node — a new cluster if it's a root.
    private func addSibling(_ nodeId: String) {
        guard let document else { return }
        added { document.addSibling(nodeId: nodeId, text: "") }
    }

    /// Add an (empty) standalone node as a new cluster in empty space.
    private func addRoot() {
        guard let document else { return }
        added { document.addRoot(text: "") }
    }

    /// Run an add operation with a checkpoint, then focus the new node and mark
    /// it provisional so an empty editor-close rolls the whole thing back.
    private func added(_ add: () -> String?) {
        guard let document else { return }
        let depth = undoStack.count
        checkpoint()
        let newId = add()
        persist(document)
        provisional = newId.map { Provisional(id: $0, undoDepth: depth) }
        requestFocus(newId)
    }

    /// Ask the structure view to reveal and edit a just-added node. Its parent is
    /// read from the document here (source of truth), so navigation never depends
    /// on the graph prop having propagated yet.
    private func requestFocus(_ newId: String?) {
        guard let document, let newId else { return }
        let parent = document.edges().first(where: { $0.toNode == newId })?.fromNode
        focusRequest = PendingFocus(id: newId, parent: parent)
    }

    /// An edit session ended. A just-created node left empty is rolled back
    /// entirely; anything else is a normal final commit.
    private func endEditNode(_ nodeId: String, _ text: String) {
        if let prov = provisional, prov.id == nodeId {
            provisional = nil
            if text.isEmpty {
                abandonNode(prov)
                return
            }
        }
        editNodeText(nodeId, text)
    }

    /// Roll back a provisional node: remove it and drop the checkpoints created
    /// since (its add, plus any typing), so the whole thing leaves no trace.
    private func abandonNode(_ prov: Provisional) {
        guard let document else { return }
        _ = document.removeNode(id: prov.id)
        if undoStack.count > prov.undoDepth {
            undoStack.removeLast(undoStack.count - prov.undoDepth)
        }
        redoStack.removeAll()
        persist(document)
    }

    /// Push the current document onto the undo stack (and clear the redo stack),
    /// so the next mutation can be reverted.
    private func checkpoint() {
        guard let document, let json = try? document.toJson() else { return }
        undoStack.append(json)
        if undoStack.count > undoLimit { undoStack.removeFirst(undoStack.count - undoLimit) }
        redoStack.removeAll()
    }

    private func undo() {
        guard let document, let current = try? document.toJson(),
              let previous = undoStack.popLast() else { return }
        redoStack.append(current)
        restore(previous)
    }

    private func redo() {
        guard let document, let current = try? document.toJson(),
              let next = redoStack.popLast() else { return }
        undoStack.append(current)
        restore(next)
    }

    /// Replace the live document with a snapshot and save it.
    private func restore(_ json: String) {
        guard let canvasURL, let doc = try? Document.parse(json: json) else { return }
        armEditSnapshot = false
        document = doc
        try? json.write(to: canvasURL, atomically: true, encoding: .utf8)
        fileMtime = modificationDate(canvasURL)
        graph = Graph(nodes: doc.nodes(), edges: doc.edges())
    }

    /// Write the document back to disk and refresh the in-memory graph. Our own
    /// write adopts the new mtime so it isn't later seen as an external change.
    private func persist(_ document: Document) {
        guard let canvasURL else { return }
        if let json = try? document.toJson() {
            try? json.write(to: canvasURL, atomically: true, encoding: .utf8)
            fileMtime = modificationDate(canvasURL)
        }
        graph = Graph(nodes: document.nodes(), edges: document.edges())
    }

    /// Reload the canvas if its file changed on disk since we last saw it —
    /// mirroring the desktop's external-change reload. Our own writes adopt the
    /// new mtime above, so they don't trigger a reload.
    private func reloadIfChanged() {
        guard let canvasURL,
              let current = modificationDate(canvasURL),
              current != fileMtime,
              let json = try? String(contentsOf: canvasURL, encoding: .utf8),
              let doc = try? Document.parse(json: json)
        else { return }
        document = doc
        fileMtime = current
        graph = Graph(nodes: doc.nodes(), edges: doc.edges())
        // The document changed underneath us; the old history no longer applies.
        undoStack.removeAll()
        redoStack.removeAll()
        provisional = nil
    }

    private func modificationDate(_ url: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }
}
