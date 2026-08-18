//! The `.canvas` files inside an Obsidian vault: finding them, and ranking them
//! against what's been typed into the vault browser.
//!
//! A vault is a plain folder, so this is a filtered directory walk — no index
//! and nothing cached on disk. The desktop runs it off-thread and keeps the
//! result until the browser is opened again.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fuzzy;

/// The most canvases one walk will collect. A real vault holds a few dozen;
/// this exists so a misconfigured root (a home directory, a network mount)
/// can't spin forever. [`Scan::truncated`] reports when it bit.
pub const MAX_ENTRIES: usize = 5_000;

/// How deep below the vault root the walk goes. Obsidian vaults nest a few
/// levels; past this it's a mount or a symlinked tree we shouldn't be in.
pub const MAX_DEPTH: usize = 12;

/// Penalty applied to a folder-path match, so a query is ranked against the
/// board's own name first and only falls back to where it lives. Mirrors the
/// command palette's title-then-keywords fallback.
const PATH_PENALTY: i32 = 4;

/// One `.canvas` file found in the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasEntry {
    /// Absolute path, for opening it.
    pub path: PathBuf,
    /// Path relative to the vault root, always `/`-separated so the display and
    /// the fuzzy match read the same on every platform. Includes the extension.
    pub relative: String,
    /// Last-modified time, for ordering. `None` when the filesystem wouldn't say.
    pub modified: Option<SystemTime>,
}

impl CanvasEntry {
    /// The board's name — the file stem, which is what the user calls it.
    pub fn name(&self) -> &str {
        let file = self.relative.rsplit('/').next().unwrap_or(&self.relative);
        // `file_stem` drops the extension whatever its case; the walk accepts
        // `.CANVAS` as readily as `.canvas`.
        Path::new(file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(file)
    }

    /// The folders above it inside the vault; empty for a board at the root.
    pub fn folder(&self) -> &str {
        match self.relative.rfind('/') {
            Some(cut) => &self.relative[..cut],
            None => "",
        }
    }
}

/// The result of one walk.
#[derive(Debug, Default)]
pub struct Scan {
    /// Every canvas found, most recently modified first — so an empty query is
    /// already in the order the browser wants.
    pub entries: Vec<CanvasEntry>,
    /// The walk hit [`MAX_ENTRIES`] and stopped early, so `entries` is a
    /// partial list rather than the whole vault.
    pub truncated: bool,
}

/// Every `.canvas` file under `root`, most recently modified first.
///
/// Skips dot-directories (`.obsidian`, `.trash`, `.git`), which hold Obsidian's
/// own state rather than boards, and never follows a symlinked directory —
/// `file_type` reports the link itself, so a vault containing a link back to an
/// ancestor can't send the walk round in circles.
pub fn scan(root: &Path) -> Scan {
    let mut entries = Vec::new();
    let mut truncated = false;
    // (directory, depth below the root)
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue; // unreadable (permissions, a stale mount) — skip it
        };
        for entry in listing.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // The child sits at `depth + 1`, which must still be within
                // the limit.
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            } else if is_canvas(&name) {
                if entries.len() >= MAX_ENTRIES {
                    truncated = true;
                    break;
                }
                let path = entry.path();
                let Some(relative) = relative_to(root, &path) else {
                    continue;
                };
                let modified = entry.metadata().ok().and_then(|meta| meta.modified().ok());
                entries.push(CanvasEntry { path, relative, modified });
            }
        }
        if truncated {
            break;
        }
    }

    // Newest first, then by path so equal timestamps keep a stable order rather
    // than whatever the directory happened to yield.
    entries.sort_by(|a, b| {
        let (at, bt) = (
            a.modified.unwrap_or(SystemTime::UNIX_EPOCH),
            b.modified.unwrap_or(SystemTime::UNIX_EPOCH),
        );
        bt.cmp(&at).then_with(|| a.relative.cmp(&b.relative))
    });
    Scan { entries, truncated }
}

/// The `theme.css` of the theme Obsidian is currently set to in this vault.
///
/// Obsidian records the choice in `.obsidian/appearance.json` as `cssTheme`,
/// and keeps each installed theme in `.obsidian/themes/<name>/theme.css`. So
/// the file the app already knows how to read is sitting at a path we can
/// derive, rather than one the user has to go and find.
///
/// `None` when there's no vault config, when `cssTheme` is absent or empty
/// (Obsidian's own default theme, which we have no CSS for and shouldn't
/// invent one for), or when the named theme isn't installed.
pub fn active_theme(root: &Path) -> Option<PathBuf> {
    let config = root.join(".obsidian");
    let appearance = std::fs::read_to_string(config.join("appearance.json")).ok()?;
    let name = serde_json::from_str::<serde_json::Value>(&appearance)
        .ok()?
        .get("cssTheme")?
        .as_str()?
        .trim()
        .to_owned();
    if name.is_empty() {
        return None;
    }
    let css = config.join("themes").join(name).join("theme.css");
    css.is_file().then_some(css)
}

/// Whether `path` is a theme we resolved from `vault` ourselves, rather than
/// one the user pointed at by hand.
///
/// The difference decides whether re-reading the vault may overwrite it: a
/// theme we adopted should track what Obsidian is set to, and one the user
/// chose deliberately should not be taken away from them.
pub fn is_vault_theme(path: &Path, vault: &Path) -> bool {
    path.starts_with(vault.join(".obsidian").join("themes"))
}

/// Whether a file name is a canvas we can open (case-insensitive, like the
/// drag-and-drop filter).
fn is_canvas(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("canvas"))
}

/// `path` expressed relative to `root` with `/` separators. `None` if it isn't
/// under `root` at all.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Indices of the entries matching `query`, best first. Each is ranked against
/// its own name, falling back to the folders above it at a small penalty — so
/// "road" finds `Projects/Roadmap` on its name, "proj/road" still finds it on
/// its path, and a deep folder can't outrank a board actually called that.
///
/// An empty query keeps the incoming order (most recently modified first).
pub fn rank(entries: &[CanvasEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut scored: Vec<(usize, i32)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let score = fuzzy::score(query, entry.name()).or_else(|| {
                fuzzy::score(query, &entry.relative).map(|score| score - PATH_PENALTY)
            })?;
            Some((i, score))
        })
        .collect();
    // Stable sort, so equal scores stay in modified order.
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(relative: &str) -> CanvasEntry {
        CanvasEntry {
            path: PathBuf::from(relative),
            relative: relative.to_owned(),
            modified: None,
        }
    }

    /// A throwaway directory tree, removed when the test ends.
    struct Temp(PathBuf);


    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tb-vault-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn file(&self, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}").unwrap();
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn name_and_folder_split_the_relative_path() {
        let nested = entry("Projects/2026/Roadmap.canvas");
        assert_eq!(nested.name(), "Roadmap");
        assert_eq!(nested.folder(), "Projects/2026");
        // A board at the vault root has no folder to show.
        let root = entry("Inbox.canvas");
        assert_eq!(root.name(), "Inbox");
        assert_eq!(root.folder(), "");
    }

    #[test]
    fn scan_finds_nested_canvases_and_ignores_everything_else() {
        let vault = Temp::new("scan");
        vault.file("Inbox.canvas");
        vault.file("Projects/Roadmap.canvas");
        vault.file("Projects/Deep/Notes.CANVAS"); // extension match is case-insensitive
        vault.file("Projects/Roadmap.md"); // a note, not a board
        vault.file(".obsidian/workspace.canvas"); // Obsidian's own state
        vault.file(".trash/Deleted.canvas"); // and its wastebasket

        let found = scan(&vault.0);
        let mut names: Vec<&str> = found.entries.iter().map(CanvasEntry::name).collect();
        names.sort_unstable();
        assert_eq!(names, ["Inbox", "Notes", "Roadmap"]);
        assert!(!found.truncated);
    }

    #[test]
    fn scan_orders_the_most_recently_modified_first() {
        let vault = Temp::new("order");
        vault.file("Old.canvas");
        // Written second, so it's the newer of the two whatever the clock's
        // resolution — the browser opens on this one.
        std::thread::sleep(std::time::Duration::from_millis(20));
        vault.file("New.canvas");

        let found = scan(&vault.0);
        assert_eq!(
            found.entries.iter().map(CanvasEntry::name).collect::<Vec<_>>(),
            ["New", "Old"]
        );
    }

    #[test]
    fn the_active_theme_is_resolved_from_obsidians_own_config() {
        let vault = Temp::new("theme");
        vault.file(".obsidian/appearance.json");
        std::fs::write(
            vault.0.join(".obsidian/appearance.json"),
            r#"{"accentColor":"","theme":"obsidian","cssTheme":"Things"}"#,
        )
        .unwrap();
        // Named but not installed: nothing to read, so nothing to report.
        assert_eq!(active_theme(&vault.0), None);

        vault.file(".obsidian/themes/Things/theme.css");
        let found = active_theme(&vault.0).expect("the installed theme");
        assert!(found.ends_with(".obsidian/themes/Things/theme.css"), "{found:?}");
        // And it's recognisable as ours rather than a hand-picked path.
        assert!(is_vault_theme(&found, &vault.0));
        assert!(!is_vault_theme(Path::new("/elsewhere/theme.css"), &vault.0));
    }

    #[test]
    fn obsidians_default_theme_resolves_to_nothing() {
        let vault = Temp::new("theme-default");
        vault.file(".obsidian/appearance.json");
        // An empty `cssTheme` is how Obsidian records "use my own default" —
        // there is no CSS for it, so we keep the app's built-in colours rather
        // than inventing an approximation.
        std::fs::write(vault.0.join(".obsidian/appearance.json"), r#"{"cssTheme":""}"#).unwrap();
        assert_eq!(active_theme(&vault.0), None);

        // A vault Obsidian has never opened has no config at all.
        let bare = Temp::new("theme-bare");
        bare.file("Inbox.canvas");
        assert_eq!(active_theme(&bare.0), None);
    }

    #[test]
    fn rank_prefers_a_name_match_over_a_folder_one() {
        let entries = vec![
            entry("Roadmapping/Sprint.canvas"),
            entry("Inbox/Roadmap.canvas"),
        ];
        // The board *called* Roadmap wins, though both match "roadmap".
        let hits = rank(&entries, "roadmap");
        assert_eq!(hits.first(), Some(&1));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn rank_matches_across_the_folder_separator() {
        let entries = vec![entry("Notes/Misc.canvas"), entry("Projects/Roadmap.canvas")];
        assert_eq!(rank(&entries, "proj/road"), vec![1]);
        // Nothing matching drops out entirely.
        assert!(rank(&entries, "zzz").is_empty());
    }

    #[test]
    fn rank_leaves_an_empty_query_in_scan_order() {
        let entries = vec![entry("B.canvas"), entry("A.canvas")];
        assert_eq!(rank(&entries, ""), vec![0, 1]);
    }
}
