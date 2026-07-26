import SwiftUI
import UniformTypeIdentifiers

/// Root screen: open a folder (a vault, or any folder holding a canvas and its
/// images), pick the canvas, and explore it in the structure view. Opening a
/// folder — rather than a single `.canvas` — is what grants access to the
/// images that file nodes reference.
struct ContentView: View {
    @State private var vault: Vault?
    @State private var graph: Graph?
    @State private var canvasName: String?
    /// Canvases found in the opened folder, shown as a chooser when there's
    /// more than one.
    @State private var candidates: [URL] = []
    @State private var errorMessage: String?
    @State private var importing = false

    var body: some View {
        Group {
            if let graph, let vault, let canvasName {
                StructureView(graph: graph, vault: vault, fileName: canvasName) {
                    importing = true
                }
                .id(canvasName)
            } else if candidates.count > 1 {
                canvasChooser
            } else {
                openScreen
            }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.folder], onCompletion: openFolder)
        .task { restoreRecent() }
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
            graph = Graph(nodes: doc.nodes(), edges: doc.edges())
            canvasName = url.lastPathComponent
            candidates = []
            // Remember this as the most recent, to restore on next launch.
            RecentCanvas.save(vaultRoot: vault.root, canvasURL: url)
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
