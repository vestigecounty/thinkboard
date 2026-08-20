//! Where themes come from: the ones compiled in, and the ones on disk.
//!
//! A theme is named, not pathed. `config.theme` holds a name, and it resolves
//! against the user's own directory first so a bundled theme can be replaced by
//! editing a copy of it rather than by forking the app.

use std::path::PathBuf;

use thinkboard_core::theme::ThemeFile;

/// The themes compiled into the binary, as `(name, json)`.
///
/// These are base16 palettes reimplemented in our own format — sixteen colours
/// each, which is how their authors publish them. They cost about 600 bytes
/// apiece, so the whole set is smaller than one icon. Attribution lives in each
/// file's `author` and `license`.
const BUILT_IN: &[(&str, &str)] = &[
    ("ThinkBoard Dark", include_str!("../themes/thinkboard-dark.json")),
    ("Solarized Dark", include_str!("../themes/solarized-dark.json")),
    ("Nord", include_str!("../themes/nord.json")),
    ("Dracula", include_str!("../themes/dracula.json")),
    ("Gruvbox Dark", include_str!("../themes/gruvbox-dark.json")),
    ("Tokyo Night", include_str!("../themes/tokyo-night.json")),
    ("Catppuccin Mocha", include_str!("../themes/catppuccin-mocha.json")),
    ("One Dark", include_str!("../themes/one-dark.json")),
    ("Rosé Pine", include_str!("../themes/rose-pine.json")),
    ("Monokai", include_str!("../themes/monokai.json")),
];

/// Where a user's own themes live, beside their config.
pub(super) fn user_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("themes"))
}

/// Every theme available, user files first so one of theirs shadows a bundled
/// theme of the same name.
///
/// Unreadable or malformed files are skipped with a warning rather than
/// failing the list — one bad file in the directory shouldn't cost you the
/// other nine.
pub(super) fn all() -> Vec<ThemeFile> {
    let mut themes: Vec<ThemeFile> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    if let Some(dir) = user_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        // Directory order is arbitrary; the picker should be stable.
        files.sort();
        for path in files {
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|json| ThemeFile::parse(&json)) {
                Ok(theme) => {
                    seen.push(theme.name.clone());
                    themes.push(theme);
                }
                Err(error) => eprintln!("theme: skipping {} ({error})", path.display()),
            }
        }
    }

    for (name, json) in BUILT_IN {
        if seen.iter().any(|s| s == name) {
            continue;
        }
        match ThemeFile::parse(json) {
            Ok(theme) => themes.push(theme),
            // A bundled theme that doesn't parse is a build-time mistake, and
            // there's a test below that catches it.
            Err(error) => eprintln!("theme: built-in {name} is broken ({error})"),
        }
    }
    themes
}

/// The theme called `name`, or `None` if there isn't one.
pub(super) fn find(name: &str) -> Option<ThemeFile> {
    all().into_iter().find(|theme| theme.name == name)
}

/// The colours to draw with, given the config.
///
/// A named theme wins; failing that the Obsidian stylesheet, which is what a
/// vault's own theme still feeds. `None` means neither applied and the built-in
/// default stands.
pub(super) fn resolve(config: &crate::config::Config) -> Option<crate::theme::Theme> {
    let name = config.theme.trim();
    if name.is_empty() {
        return None;
    }
    let Some(file) = find(name) else {
        eprintln!("theme: no theme called \"{name}\" — using the default");
        return None;
    };
    let mut theme = crate::theme::Theme::default();
    theme.apply_resolved(&file.resolve());
    Some(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_theme_parses_and_resolves() {
        for (name, json) in BUILT_IN {
            let theme = ThemeFile::parse(json)
                .unwrap_or_else(|error| panic!("built-in {name}: {error}"));
            assert_eq!(&theme.name, name, "the registry name must match the file's");

            // A palette is the whole point of bundling these: sixteen colours
            // in, every role out. If one didn't resolve, the theme would be
            // silently half the built-in default.
            let resolved = theme.resolve();
            for (role, value) in [
                ("background", resolved.background),
                ("card_fill", resolved.card_fill),
                ("text", resolved.text),
                ("text_muted", resolved.text_muted),
                ("accent", resolved.accent),
                ("code", resolved.code),
            ] {
                assert!(value.is_some(), "{name} left {role} unresolved");
            }
            assert!(resolved.headings.is_some(), "{name} has no heading scale");
        }
    }

    #[test]
    fn built_in_themes_are_readable() {
        // The mapping is shared, so a scheme can still land text somewhere hard
        // to read on a card. Better to find that here than on someone's board —
        // and it has already earned its keep: it caught Solarized's inline code
        // at 2.8:1, which is what made the mapping check the slot it picks.
        for (name, json) in BUILT_IN {
            let warnings = ThemeFile::parse(json).unwrap().resolve().contrast_warnings();
            assert!(warnings.is_empty(), "{name} is hard to read: {warnings:?}");
        }
    }

    #[test]
    fn the_list_is_stable_and_has_no_duplicates() {
        let names: Vec<String> = all().into_iter().map(|t| t.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "two themes share a name: {names:?}");
        assert!(names.iter().any(|n| n == "ThinkBoard Dark"));
    }
}
