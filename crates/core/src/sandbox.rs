//! Running inside a Flatpak: what moves, and where the host's things are.
//!
//! Flatpak gives each app its own `$XDG_CONFIG_HOME` (`~/.var/app/<id>/config`)
//! and its own `/run/user/<uid>` for every instance, and runs it under a PID
//! namespace of its own. The app's own config belongs where Flatpak puts it.
//! What it reads of the host's — Obsidian's vault registry, COSMIC's
//! shortcuts, a desktop's wallpaper settings — is where the host keeps it,
//! which a sandbox let into the home sees as it is.

use std::path::PathBuf;

/// The Flatpak app id the app runs under, where it runs in one.
pub fn flatpak_id() -> Option<String> {
    std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty())
}

/// The host's config directory: `$XDG_CONFIG_HOME`, or `~/.config` — except
/// in a Flatpak, where `$XDG_CONFIG_HOME` is the app's own folder and the
/// host's is taken to be `~/.config`, its default. (One moved by the host's
/// own `$XDG_CONFIG_HOME` can't be seen from inside.)
pub fn host_config_home() -> Option<PathBuf> {
    resolve_config_home(
        flatpak_id().is_some(),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn resolve_config_home(flatpak: bool, xdg: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let xdg = xdg.filter(|dir| dir.is_absolute() && !flatpak);
    xdg.or_else(|| home.map(|home| home.join(".config")))
}

// Flatpak is Linux's, its paths Unix ones; Unix, so a Mac runs it too.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_hosts_config_is_found_from_inside_a_flatpak_too() {
        let home = Some(PathBuf::from("/home/x"));
        let app = Some(PathBuf::from("/home/x/.var/app/com.vestigecounty.ThinkBoard/config"));
        assert_eq!(resolve_config_home(false, app.clone(), home.clone()), app, "outside, as it says");
        assert_eq!(resolve_config_home(true, app, home.clone()), Some(PathBuf::from("/home/x/.config")), "inside, the host's");
        assert_eq!(resolve_config_home(false, Some(PathBuf::from("relative")), home), Some(PathBuf::from("/home/x/.config")));
        assert_eq!(resolve_config_home(false, None, None), None);
    }
}
