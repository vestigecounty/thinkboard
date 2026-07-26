import Foundation

/// Remembers the most recently opened canvas across launches.
///
/// The document picker's folder access doesn't survive a relaunch, so we store
/// a *security-scoped bookmark* of the folder (resolving it re-grants access to
/// the folder and its images) plus the chosen canvas's path relative to that
/// folder.
enum RecentCanvas {
    private static let bookmarkKey = "recentVaultBookmark"
    private static let pathKey = "recentCanvasRelativePath"

    static func save(vaultRoot: URL, canvasURL: URL) {
        // Creating the bookmark requires the folder's security scope to be
        // active — it is, because the open Vault is holding it.
        guard let bookmark = try? vaultRoot.bookmarkData() else { return }
        let defaults = UserDefaults.standard
        defaults.set(bookmark, forKey: bookmarkKey)
        defaults.set(relativePath(of: canvasURL, under: vaultRoot), forKey: pathKey)
    }

    static func load() -> (bookmark: Data, relativePath: String)? {
        let defaults = UserDefaults.standard
        guard let bookmark = defaults.data(forKey: bookmarkKey),
              let relative = defaults.string(forKey: pathKey)
        else { return nil }
        return (bookmark, relative)
    }

    static func clear() {
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: bookmarkKey)
        defaults.removeObject(forKey: pathKey)
    }

    private static func relativePath(of url: URL, under root: URL) -> String {
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        if url.path.hasPrefix(prefix) {
            return String(url.path.dropFirst(prefix.count))
        }
        return url.lastPathComponent
    }
}
