//! Finding the vaults Obsidian already knows about.
//!
//! Obsidian keeps its own registry of every vault you've opened, next to its
//! settings. Reading it turns the worst question a first launch can ask —
//! "choose a folder" — into a list of names, for the users most likely to want
//! one. Nothing here is required: no Obsidian, no registry, no list, and the
//! folder picker is still there.
//!
//! **Read-only, and never authoritative.** A vault in the registry may have
//! been moved or deleted since, so [`vaults`] drops the ones that aren't there
//! any more rather than offering a path that fails on open.

use std::path::{Path, PathBuf};

/// A vault Obsidian has open or has opened before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    pub path: PathBuf,
    /// The folder's own name, which is what Obsidian shows and what its user
    /// calls it.
    pub name: String,
    /// Whether Obsidian currently has it open — the best guess at which one
    /// somebody means, when they have several.
    pub open: bool,
    /// Obsidian's last-opened stamp, for ordering the rest.
    pub last_opened: u64,
}

/// Where Obsidian's registry file may be, per platform: everywhere an install
/// of it could have put one. Empty where we don't know — which is not the same
/// as Obsidian not being installed, so callers treat it the same as no vaults.
pub fn registry_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    {
        home.map(|h| h.join("Library/Application Support/obsidian/obsidian.json")).into_iter().collect()
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("obsidian/obsidian.json")).into_iter().collect()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // The host's: inside a Flatpak, `$XDG_CONFIG_HOME` is ThinkBoard's own.
        let config = crate::sandbox::host_config_home();
        linux_registry_paths(home.as_deref(), config.as_deref())
    }
}

/// Linux has three: the XDG config directory, where the .deb, the AppImage and
/// the tarball keep it, and the ones Flatpak — the most common way it's
/// installed there — and Snap give it in its place, under the user's home.
/// Reading only the first, a Flatpak user's welcome board offered none of
/// their vaults.
#[cfg(any(not(any(target_os = "macos", target_os = "windows")), test))]
fn linux_registry_paths(home: Option<&Path>, xdg_config: Option<&Path>) -> Vec<PathBuf> {
    let config = xdg_config.map(Path::to_path_buf).or_else(|| home.map(|h| h.join(".config")));
    let mut paths: Vec<PathBuf> = config.into_iter().map(|dir| dir.join("obsidian/obsidian.json")).collect();
    if let Some(home) = home {
        paths.push(home.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"));
        paths.push(home.join("snap/obsidian/current/.config/obsidian/obsidian.json"));
    }
    paths
}

/// The vaults Obsidian knows about that still exist on disk, best guess first,
/// from every registry there is.
pub fn vaults() -> Vec<Vault> {
    // A registry that can't be read is no Obsidian there, or no vault ever
    // opened in it.
    let registries = registry_paths().into_iter().filter_map(|path| std::fs::read_to_string(path).ok()).map(|json| parse(&json));
    let mut found = merge(registries);
    found.retain(|vault| vault.path.is_dir());
    found
}

/// Several registries' vaults as one list, best guess first. A vault in more
/// than one — moved from one install to another — is offered once, as the
/// registry that has it open, or opened it last, says.
fn merge(registries: impl IntoIterator<Item = Vec<Vault>>) -> Vec<Vault> {
    let mut merged: Vec<Vault> = Vec::new();
    for vault in registries.into_iter().flatten() {
        match merged.iter_mut().find(|seen| seen.path == vault.path) {
            Some(seen) if (vault.open, vault.last_opened) > (seen.open, seen.last_opened) => *seen = vault,
            Some(_) => {}
            None => merged.push(vault),
        }
    }
    best_first(&mut merged);
    merged
}

/// Reads the registry's `vaults` map. Separate from the file so the shape can
/// be tested without one.
///
/// Ordered by the one Obsidian has open, then by when each was last opened —
/// so the first row is the vault the user is most likely to mean.
pub fn parse(json: &str) -> Vec<Vault> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(entries) = root.get("vaults").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut vaults: Vec<Vault> = entries
        .values()
        .filter_map(|entry| {
            let path = PathBuf::from(entry.get("path")?.as_str()?);
            Some(Vault {
                name: vault_name(&path)?,
                open: entry.get("open").and_then(serde_json::Value::as_bool).unwrap_or(false),
                last_opened: entry.get("ts").and_then(serde_json::Value::as_u64).unwrap_or(0),
                path,
            })
        })
        .collect();
    best_first(&mut vaults);
    vaults
}

/// The one Obsidian has open, then the rest by when each was last opened.
fn best_first(vaults: &mut [Vault]) {
    vaults.sort_by_key(|vault| (!vault.open, std::cmp::Reverse(vault.last_opened)));
}

/// A vault's display name: the folder it lives in.
fn vault_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "vaults": {
            "9f0": { "path": "/Users/x/Notes",   "ts": 1700000000000, "open": true },
            "1a2": { "path": "/Users/x/Archive", "ts": 1600000000000 },
            "3b4": { "path": "/Users/x/Recent",  "ts": 1800000000000 }
        },
        "frame": { "width": 1200 }
    }"#;

    #[test]
    fn the_open_vault_comes_first_then_the_most_recent() {
        let vaults = parse(SAMPLE);
        let names: Vec<&str> = vaults.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["Notes", "Recent", "Archive"],
            "the one Obsidian has open is the best guess at which is meant, and the \
             rest follow it most-recent first",
        );
        assert!(vaults[0].open);
        assert!(!vaults[1].open);
    }

    #[test]
    fn a_vault_is_named_by_its_folder() {
        let vaults = parse(SAMPLE);
        assert_eq!(vaults[0].name, "Notes");
        assert_eq!(vaults[0].path, PathBuf::from("/Users/x/Notes"));
    }

    #[test]
    fn anything_unreadable_is_simply_no_vaults() {
        // Not an error worth surfacing: no Obsidian, an old format, or a file
        // half-written while it was being read. The folder picker still works.
        for json in ["", "{}", "not json", r#"{"vaults": []}"#, r#"{"vaults": {"a": {}}}"#] {
            assert!(parse(json).is_empty(), "{json:?} should yield nothing");
        }
    }

    #[test]
    fn on_linux_the_flatpak_and_snap_installs_are_looked_in_too() {
        let home = Path::new("/home/x");
        assert_eq!(
            linux_registry_paths(Some(home), None),
            [
                PathBuf::from("/home/x/.config/obsidian/obsidian.json"),
                PathBuf::from("/home/x/.var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"),
                PathBuf::from("/home/x/snap/obsidian/current/.config/obsidian/obsidian.json"),
            ],
        );
        // XDG_CONFIG_HOME moves the native install's, not the sandboxes',
        // which keep their own.
        let paths = linux_registry_paths(Some(home), Some(Path::new("/cfg")));
        assert_eq!(paths[0], PathBuf::from("/cfg/obsidian/obsidian.json"));
        assert_eq!(paths.len(), 3);
        assert_eq!(linux_registry_paths(None, Some(Path::new("/cfg"))).len(), 1, "no home, no sandboxes to find");
    }

    #[test]
    fn a_vault_in_two_registries_is_offered_once_as_the_one_that_opened_it_last_says() {
        let native = parse(SAMPLE);
        let flatpak = parse(
            r#"{"vaults": {
                "c5d": { "path": "/Users/x/Archive", "ts": 1900000000000 },
                "e6f": { "path": "/Users/x/Garden",  "ts": 1750000000000 }
            }}"#,
        );
        let vaults = merge([native, flatpak]);
        let names: Vec<&str> = vaults.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["Notes", "Archive", "Recent", "Garden"],
            "Archive once, at the newer stamp the second registry gave it",
        );
        assert_eq!(vaults[1].last_opened, 1900000000000);
    }

    #[test]
    fn an_entry_without_a_usable_path_is_skipped_not_guessed_at() {
        let json = r#"{"vaults": {
            "a": { "path": "/", "ts": 1 },
            "b": { "path": "/Users/x/Good", "ts": 2 }
        }}"#;
        let vaults = parse(json);
        assert_eq!(vaults.len(), 1, "the root has no folder name to show");
        assert_eq!(vaults[0].name, "Good");
    }
}
