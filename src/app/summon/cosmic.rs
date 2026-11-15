//! The summon key on COSMIC, which has no GlobalShortcuts portal: given to
//! the desktop's own custom shortcuts instead, as a command that runs
//! `thinkboard` — which hands over to the copy running (`summon::hand_over`).
//! It's added once, where no shortcut runs ThinkBoard yet, on the key from
//! Settings; from then on the key is COSMIC's to change, as the portal's is
//! on the desktops that have one, and one taken out there isn't put back
//! (`Config::cosmic_shortcut_added`).
//!
//! **The file is the desktop's, and every custom shortcut is in it.** COSMIC
//! reads it whole, and one that doesn't parse loses the user all of them, so
//! it isn't edited as text. It's read with the parser COSMIC reads it with,
//! each shortcut kept exactly as written — one we don't understand goes back
//! as it was — and ours is added after them. What would be written is read
//! back first, and has to give every shortcut that was there, unchanged, and
//! ours; anything else, and nothing is written. It's replaced whole, as
//! COSMIC's own settings replace it, under a name its watcher skips until
//! it's renamed into place.

use std::path::{Path, PathBuf};

use global_hotkey::hotkey::{HotKey, Modifiers};
use ron::value::RawValue;
use serde::{Deserialize, Serialize};

/// What COSMIC Settings lists the shortcut as.
const NAME: &str = "ThinkBoard";

/// Where the shortcuts are under a config root: cosmic-config's version 1 of
/// `com.system76.CosmicSettings.Shortcuts` — the user's in the key `custom`,
/// the desktop's own in `defaults`, under a data directory.
const SHORTCUTS: &str = "cosmic/com.system76.CosmicSettings.Shortcuts/v1";

/// A key combination as COSMIC's `Binding` writes one, as far as it's read
/// here: a binding by keycode, say, is only ever copied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Binding {
    #[serde(default)]
    modifiers: Vec<Modifier>,
    /// The key's xkb name.
    #[serde(default)]
    key: String,
    /// The shortcut's name, where it was given one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// COSMIC's modifiers, in the order its settings write them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

/// What a shortcut does: of COSMIC's actions, the one this needs.
#[derive(Debug, Serialize, Deserialize)]
enum Action {
    Spawn(String),
}

/// The key of a shortcut that summons ThinkBoard, as the settings row names
/// it, if COSMIC has one.
pub(super) fn summon_key() -> Option<String> {
    bound(&std::fs::read_to_string(custom_path()?).ok()?)
}

/// Adds the summon shortcut to COSMIC's, on `hotkey` (as Settings writes
/// one): the key as the settings row names it, or why it wasn't.
pub(super) fn add_summon(hotkey: &str) -> Result<String, String> {
    let hotkey = super::super::startup::parse_hotkey(hotkey).ok_or("no key is set")?;
    let binding = binding(hotkey).ok_or("the key has no name COSMIC knows")?;
    if forbidden_alone(&binding) {
        return Err(format!("COSMIC won't take {} without a modifier", written(&binding)));
    }
    // One of the desktop's own on the same key would be overridden by ours.
    for defaults in data_dirs().map(|dir| dir.join(SHORTCUTS).join("defaults")) {
        if let Ok(file) = std::fs::read_to_string(&defaults)
            && taken(&entries(&file).unwrap_or_default(), &binding)
        {
            return Err(format!("{} is one of COSMIC's own shortcuts", written(&binding)));
        }
    }
    let path = custom_path().ok_or("there's no config directory")?;
    // Replaced, a link — a dotfile kept elsewhere — would become a file.
    if path.symlink_metadata().is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(format!("{} is a link", path.display()));
    }
    let file = match std::fs::read_to_string(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let whole = with_summon(&file, &binding, &command())?;
    let folder = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    // cosmic-config's watcher skips `.atomicwrite…`, which is what its own
    // writes are made under before they're renamed into place.
    let draft = folder.join(".atomicwrite-thinkboard");
    std::fs::write(&draft, whole).map_err(|error| error.to_string())?;
    std::fs::rename(&draft, &path).map_err(|error| error.to_string())?;
    Ok(written(&binding))
}

/// The user's shortcuts file: under `$XDG_CONFIG_HOME`, or `~/.config`.
fn custom_path() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(root.join(SHORTCUTS).join("custom"))
}

/// Where the desktop's own configs are: `$XDG_DATA_DIRS`, or its default.
fn data_dirs() -> impl Iterator<Item = PathBuf> {
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::env::split_paths(&dirs).collect::<Vec<_>>().into_iter()
}

/// The shortcuts in a file of them, each its binding and its action as
/// written. A file with nothing in it has none.
fn entries(file: &str) -> Result<Vec<(String, String)>, String> {
    if file.trim().is_empty() {
        return Ok(Vec::new());
    }
    // An extension switched on for the file changes what the rest of it
    // means, and wouldn't be written back.
    if file.trim_start().starts_with("#!") {
        return Err("the file switches on a RON extension".to_owned());
    }
    struct Entries(Vec<(String, String)>);
    impl<'de> Deserialize<'de> for Entries {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visit;
            impl<'de> serde::de::Visitor<'de> for Visit {
                type Value = Entries;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a map of shortcuts")
                }
                fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Entries, M::Error> {
                    let mut entries = Vec::new();
                    while let Some((binding, action)) = map.next_entry::<&RawValue, &RawValue>()? {
                        entries.push((binding.get_ron().trim().to_owned(), action.get_ron().trim().to_owned()));
                    }
                    Ok(Entries(entries))
                }
            }
            deserializer.deserialize_map(Visit)
        }
    }
    ron::from_str::<Entries>(file).map(|Entries(entries)| entries).map_err(|error| error.to_string())
}

/// The key of the shortcut in `file` that summons ThinkBoard — ours, by its
/// name, or one bound by hand to run it.
fn bound(file: &str) -> Option<String> {
    entries(file).ok()?.iter().find_map(|(binding, action)| {
        let binding: Binding = ron::from_str(binding).ok()?;
        let runs = ron::from_str::<Action>(action).is_ok_and(|Action::Spawn(command)| runs_thinkboard(&command));
        (runs || binding.description.as_deref() == Some(NAME)).then(|| written(&binding))
    })
}

/// Whether a shell command runs `thinkboard`, by name or by a path to it,
/// quoted or not.
fn runs_thinkboard(command: &str) -> bool {
    Path::new(&first_word(command)).file_name().is_some_and(|name| name == "thinkboard")
}

/// The program a shell command runs: its first word, as the shell reads it —
/// quoted in parts or not, backslashes escaping.
fn first_word(command: &str) -> String {
    let mut word = String::new();
    let mut chars = command.trim_start().chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => break,
            '\'' => word.extend(chars.by_ref().take_while(|&c| c != '\'')),
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        c => word.push(c),
                    }
                }
            }
            '\\' => word.extend(chars.next()),
            c => word.push(c),
        }
    }
    word
}

/// `file` with the summon shortcut after every shortcut already in it, each
/// of those written back as it was — checked by reading the result back.
fn with_summon(file: &str, binding: &Binding, command: &str) -> Result<String, String> {
    let kept = entries(file)?;
    if taken(&kept, binding) {
        return Err(format!("{} is taken already", written(binding)));
    }
    let pretty = || ron::ser::PrettyConfig::new();
    // Indented a level, as an entry of the map is; the others keep theirs.
    let ours = (
        ron::ser::to_string_pretty(binding, pretty()).map_err(|error| error.to_string())?.replace('\n', "\n    "),
        ron::ser::to_string_pretty(&Action::Spawn(command.to_owned()), pretty()).map_err(|error| error.to_string())?,
    );
    let mut whole = String::from("{\n");
    for (binding, action) in kept.iter().chain([&ours]) {
        whole.push_str(&format!("    {binding}: {action},\n"));
    }
    whole.push_str("}\n");

    // Read back as COSMIC will read it: what was there, as it was, then ours.
    let again = entries(&whole)?;
    let (before, added) = again.split_at(again.len().saturating_sub(1));
    let ours_again = added.first().and_then(|(binding, action)| {
        Some((ron::from_str::<Binding>(binding).ok()?, ron::from_str::<Action>(action).ok()?))
    });
    let read_back = before == kept.as_slice()
        && ours_again.is_some_and(|(again, Action::Spawn(runs))| {
            same_key(&again, binding) && again.description == binding.description && runs == command
        });
    if !read_back {
        return Err("the file didn't read back as it would be written".to_owned());
    }
    Ok(whole)
}

/// Whether one of `entries` is on `binding`'s key already.
fn taken(entries: &[(String, String)], binding: &Binding) -> bool {
    entries.iter().any(|(other, _)| ron::from_str::<Binding>(other).is_ok_and(|other| same_key(&other, binding)))
}

/// Whether two bindings are one key combination, as COSMIC tells them
/// apart: the modifiers held and the key, whatever the name.
fn same_key(a: &Binding, b: &Binding) -> bool {
    let held = |binding: &Binding| {
        let mut held = binding.modifiers.clone();
        held.sort();
        held.dedup();
        held
    };
    // A letter is its lowercase keysym, as COSMIC's settings write it.
    let key = |binding: &Binding| match binding.key.chars().count() {
        1 => binding.key.to_lowercase(),
        _ => binding.key.clone(),
    };
    held(a) == held(b) && key(a) == key(b) && !a.key.is_empty() && a.key != "None"
}

/// Settings' hotkey as a COSMIC binding, named so its settings list it.
fn binding(hotkey: HotKey) -> Option<Binding> {
    let held = [
        (Modifiers::SUPER | Modifiers::META, Modifier::Super),
        (Modifiers::CONTROL, Modifier::Ctrl),
        (Modifiers::ALT, Modifier::Alt),
        (Modifiers::SHIFT, Modifier::Shift),
    ];
    Some(Binding {
        modifiers: held.iter().filter(|(mods, _)| hotkey.mods.intersects(*mods)).map(|&(_, modifier)| modifier).collect(),
        key: super::key_name(hotkey.key)?,
        description: Some(NAME.to_owned()),
    })
}

/// A key COSMIC won't bind by itself — a letter, a digit, Space, Tab,
/// Return, a lock key (`is_forbidden_unmodified_keysym`) — with nothing held.
fn forbidden_alone(binding: &Binding) -> bool {
    let letter_or_digit = binding.key.chars().count() == 1 && binding.key.chars().all(|c| c.is_ascii_alphanumeric());
    binding.modifiers.is_empty() && (letter_or_digit || matches!(binding.key.as_str(), "space" | "Tab" | "Return" | "Scroll_Lock"))
}

/// A binding as the settings row names a key: `Ctrl+Alt+T`.
fn written(binding: &Binding) -> String {
    let mut held = binding.modifiers.clone();
    held.sort();
    held.dedup();
    let mut parts: Vec<String> = held.iter().map(|modifier| format!("{modifier:?}")).collect();
    parts.push(match binding.key.chars().count() {
        1 => binding.key.to_uppercase(),
        _ => binding.key.clone(),
    });
    parts.join("+")
}

/// What the shortcut runs: `thinkboard` where that finds this copy on the
/// `PATH`, so an upgrade in place still answers; this copy's path otherwise,
/// quoted for the shell COSMIC runs it with.
fn command() -> String {
    let Ok(this) = std::env::current_exe().and_then(|exe| exe.canonicalize()) else {
        return "thinkboard".to_owned();
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path).map(|dir| dir.join("thinkboard")).find(|program| program.is_file());
    if found.and_then(|found| found.canonicalize().ok()).is_some_and(|found| found == this) {
        "thinkboard".to_owned()
    } else {
        quoted(&this.to_string_lossy())
    }
}

/// `text` as one word to the shell, nothing in it expanded.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shortcuts file as COSMIC's settings write it: a terminal bound by
    /// hand, a window action overridden, one of the desktop's own turned off,
    /// and a binding by keycode with a name.
    const CUSTOM: &str = r#"{
    (
        modifiers: [
            Super,
        ],
        key: "t",
    ): Spawn("cosmic-term"),
    (
        modifiers: [
            Super,
            Shift,
        ],
        key: "h",
    ): Focus(Left),
    (
        modifiers: [
            Super,
        ],
        key: "q",
    ): Disable,
    (
        modifiers: [
            Ctrl,
        ],
        keycode: 107,
        description: Some("Screenshot, \"quietly\""),
    ): Spawn("sh -c 'grim -g \"$(slurp)\" - | wl-copy'"),
}
"#;

    fn hotkey(spec: &str) -> Binding {
        binding(spec.parse().expect("a hotkey")).expect("a key COSMIC names")
    }

    #[test]
    fn every_shortcut_there_is_written_back_as_it_was_and_ours_after_them() {
        let whole = with_summon(CUSTOM, &hotkey("Ctrl+Alt+T"), "thinkboard").expect("written");
        let before = entries(CUSTOM).expect("read");
        let after = entries(&whole).expect("read back");
        assert_eq!(before.len(), 4);
        assert_eq!(after[..4], before[..]);
        assert_eq!(after.len(), 5);
        assert!(after[4].1.contains("Spawn(\"thinkboard\")"), "{}", after[4].1);
        assert!(after[4].0.contains("description: Some(\"ThinkBoard\")"), "{}", after[4].0);
        // And each of them reads as a shortcut, the way COSMIC reads them.
        for (binding, _) in &after {
            ron::from_str::<Binding>(binding).expect("a binding");
        }
        assert_eq!(bound(&whole).as_deref(), Some("Ctrl+Alt+T"));
    }

    #[test]
    fn with_no_file_yet_ours_is_the_only_shortcut() {
        let whole = with_summon("", &hotkey("Pause"), "'/opt/a b/thinkboard'").expect("written");
        assert_eq!(entries(&whole).expect("read back").len(), 1);
        assert_eq!(bound(&whole).as_deref(), Some("Pause"));
        let (binding, _) = &entries(&whole).expect("read back")[0];
        assert!(binding.contains("modifiers: []"), "{binding}");
    }

    #[test]
    fn a_file_that_doesnt_parse_or_switches_on_an_extension_is_left_alone() {
        assert!(with_summon("{ (key: \"t\"): Spawn(\"x\") ", &hotkey("Pause"), "thinkboard").is_err());
        assert!(with_summon("#![enable(implicit_some)]\n{}", &hotkey("Pause"), "thinkboard").is_err());
    }

    #[test]
    fn a_key_in_use_already_is_left_to_whatever_has_it() {
        assert!(with_summon(CUSTOM, &hotkey("Super+T"), "thinkboard").is_err());
        assert!(with_summon(CUSTOM, &hotkey("Shift+Super+H"), "thinkboard").is_err());
        assert!(with_summon(CUSTOM, &hotkey("Ctrl+Super+T"), "thinkboard").is_ok());
    }

    #[test]
    fn a_shortcut_that_runs_thinkboard_is_ours_whoever_bound_it() {
        let bound_by_hand = |command: &str| format!("{{(modifiers: [Ctrl, Alt], key: \"t\"): Spawn({command:?})}}");
        for command in ["thinkboard", "/home/me/.local/bin/thinkboard", "'/opt/a b/thinkboard' --windowed", "\"thinkboard\""] {
            assert_eq!(bound(&bound_by_hand(command)).as_deref(), Some("Ctrl+Alt+T"), "{command}");
        }
        for command in ["thinkboard-beta", "cosmic-term", "sh -c thinkboard"] {
            assert_eq!(bound(&bound_by_hand(command)), None, "{command}");
        }
        assert_eq!(bound(CUSTOM), None);
    }

    #[test]
    fn a_key_cosmic_wont_bind_alone_is_refused() {
        for spec in ["T", "1", "Space", "Enter"] {
            assert!(forbidden_alone(&hotkey(spec)), "{spec}");
        }
        for spec in ["Pause", "F12", "Ctrl+T", "Super+Space"] {
            assert!(!forbidden_alone(&hotkey(spec)), "{spec}");
        }
    }

    #[test]
    fn a_path_reaches_the_shell_as_one_word() {
        assert_eq!(quoted("/opt/a b/thinkboard"), "'/opt/a b/thinkboard'");
        assert_eq!(quoted("/opt/it's $HOME/thinkboard"), r"'/opt/it'\''s $HOME/thinkboard'");
        assert!(runs_thinkboard(&quoted("/opt/it's $HOME/thinkboard")));
        assert_eq!(first_word(r#"/opt/a\ b/"think"'board' --windowed"#), "/opt/a b/thinkboard");
    }
}
