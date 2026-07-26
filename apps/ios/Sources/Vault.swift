import UIKit

/// An opened folder — a vault, or any folder containing a canvas and its
/// assets. Picking a folder (rather than a single `.canvas`) is what lets the
/// app read the images that file nodes reference by relative path. The
/// security-scoped access is held for the object's lifetime.
final class Vault {
    let root: URL
    private let scoped: Bool

    init(root: URL) {
        self.root = root
        self.scoped = root.startAccessingSecurityScopedResource()
    }

    deinit {
        if scoped { root.stopAccessingSecurityScopedResource() }
    }

    /// Every `.canvas` file under the root (recursively), sorted by path.
    func canvasFiles() -> [URL] {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey]
        ) else {
            return []
        }
        var found: [URL] = []
        for case let url as URL in enumerator where url.pathExtension.lowercased() == "canvas" {
            found.append(url)
        }
        return found.sorted { $0.path < $1.path }
    }

    /// Load an image referenced by a node's (vault-relative) file path, if it
    /// resolves under the root and is a readable image.
    func image(forRelativePath path: String) -> UIImage? {
        let url = root.appendingPathComponent(path)
        return UIImage(contentsOfFile: url.path)
    }
}

/// Whether a file path looks like a displayable image.
func isImagePath(_ path: String) -> Bool {
    let ext = (path as NSString).pathExtension.lowercased()
    return ["png", "jpg", "jpeg", "gif", "heic", "heif", "webp", "bmp", "tiff"].contains(ext)
}
