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

/// Obsidian's registry file, per platform. `None` where we don't know where it
/// would be — which is not the same as Obsidian not being installed, so callers
/// treat it the same as an empty list.
pub fn registry_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    {
        Some(home?.join("Library/Application Support/obsidian/obsidian.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join("obsidian/obsidian.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // The XDG location, with the usual fallback.
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".config")))
            .map(|dir| dir.join("obsidian/obsidian.json"))
    }
}

/// The vaults Obsidian knows about that still exist on disk, best guess first.
pub fn vaults() -> Vec<Vault> {
    let Some(path) = registry_path() else {
        return Vec::new();
    };
    let Ok(json) = std::fs::read_to_string(&path) else {
        return Vec::new(); // no Obsidian, or no vault ever opened
    };
    let mut found = parse(&json);
    found.retain(|vault| vault.path.is_dir());
    found
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
    vaults.sort_by_key(|vault| (!vault.open, std::cmp::Reverse(vault.last_opened)));
    vaults
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
