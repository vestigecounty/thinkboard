//! The desktop's wallpaper on Linux, where there's no one way to ask: each
//! desktop keeps it its own way, and the XDG wallpaper portal only sets one.
//! Found once a launch, by the desktop `XDG_CURRENT_DESKTOP` names. Where it
//! isn't found, or isn't a picture this reads — a slideshow, a folder of them,
//! a JPEG XL — the wallpaper isn't offered, rather than offered and black.

// Off Linux it's compiled for its tests alone, which read what each desktop
// writes but never ask a desktop.
#![cfg_attr(any(target_os = "macos", windows), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The wallpaper's file, looked up the first time it's asked for.
pub(super) fn find() -> Option<&'static Path> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(look_up).as_deref()
}

fn look_up() -> Option<PathBuf> {
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
    let on = |name: &str| desktops.split(':').any(|desktop| desktop == name);
    let found = if on("kde") {
        kde()
    } else if on("xfce") {
        xfce()
    } else if on("cosmic") {
        cosmic()
    } else if on("gnome") || on("unity") || on("budgie") {
        gnome()
    } else {
        None
    };
    found.filter(|path| readable(path))
}

/// GNOME, and the desktops on its settings: `picture-uri`, or in dark mode
/// `picture-uri-dark` where it's set.
fn gnome() -> Option<PathBuf> {
    let dark = gsettings("org.gnome.desktop.interface", "color-scheme").is_some_and(|scheme| scheme.contains("dark"));
    let dark = dark.then(|| gsettings("org.gnome.desktop.background", "picture-uri-dark")).flatten();
    let uri = dark.filter(|uri| !unquoted(uri).is_empty()).or_else(|| gsettings("org.gnome.desktop.background", "picture-uri"))?;
    Some(from_uri(unquoted(&uri)))
}

fn gsettings(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings").args(["get", schema, key]).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// KDE Plasma: the desktop containment's `Image=` — a file, or a wallpaper
/// package, a folder holding the picture at several sizes.
fn kde() -> Option<PathBuf> {
    let file = config_home()?.join("plasma-org.kde.plasma.desktop-appletsrc");
    let path = from_uri(&kde_image(&std::fs::read_to_string(file).ok()?)?);
    if path.is_dir() { package_picture(&path) } else { Some(path) }
}

/// The first `Image=` under a `[Wallpaper][org.kde.image][General]` section.
fn kde_image(appletsrc: &str) -> Option<String> {
    let mut in_image = false;
    for line in appletsrc.lines().map(str::trim) {
        if line.starts_with('[') {
            in_image = line.ends_with("[Wallpaper][org.kde.image][General]");
        } else if in_image && let Some(image) = line.strip_prefix("Image=") {
            return Some(image.trim().to_owned()).filter(|image| !image.is_empty());
        }
    }
    None
}

/// The largest picture in a KDE wallpaper package, by the size its name says
/// (`contents/images/3840x2160.png`).
fn package_picture(package: &Path) -> Option<PathBuf> {
    std::fs::read_dir(package.join("contents/images"))
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| readable(path))
        .max_by_key(|path| path.file_stem().and_then(|stem| stem.to_str()).map_or(0, pixels))
}

/// The pixels a `WIDTHxHEIGHT` name stands for; 0 for any other name.
fn pixels(name: &str) -> u64 {
    name.split_once('x').and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?)).unwrap_or(0)
}

/// Xfce: the first monitor's `last-image`, from its settings.
fn xfce() -> Option<PathBuf> {
    let xfconf = |args: &[&str]| {
        let output = Command::new("xfconf-query").args(["-c", "xfce4-desktop"]).args(args).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let properties = xfconf(&["-l"])?;
    let image = xfconf(&["-p", xfce_property(&properties)?])?;
    Some(PathBuf::from(image.trim()))
}

/// The property holding a picture, among those `xfconf-query -l` lists: the
/// first workspace's, where there's one.
fn xfce_property(properties: &str) -> Option<&str> {
    let images: Vec<&str> = properties.lines().map(str::trim).filter(|p| p.ends_with("/last-image")).collect();
    images.iter().find(|p| p.contains("/workspace0/")).or(images.first()).copied()
}

/// COSMIC: its background's own config, one for all outputs.
fn cosmic() -> Option<PathBuf> {
    let file = config_home()?.join("cosmic/com.system76.CosmicBackground/v1/all");
    cosmic_source(&std::fs::read_to_string(file).ok()?).map(PathBuf::from)
}

/// The file in a COSMIC background entry's `source: Path("…")`. A colour or
/// gradient has none.
fn cosmic_source(entry: &str) -> Option<String> {
    let rest = &entry[entry.find("source:")? + "source:".len()..];
    let path = rest.trim_start().strip_prefix("Path(\"")?;
    Some(path[..path.find('"')?].to_owned())
}

/// The host's config directory, where a desktop keeps its wallpaper — not
/// `$XDG_CONFIG_HOME` inside a Flatpak, where that's the app's own.
fn config_home() -> Option<PathBuf> {
    thinkboard_core::sandbox::host_config_home()
}

/// A `file://` URI as a path, its escapes undone; anything else as it is.
fn from_uri(uri: &str) -> PathBuf {
    let Some(path) = uri.strip_prefix("file://") else {
        return PathBuf::from(uri);
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|&b| (b as char).to_digit(16));
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

/// A gsettings string without the quotes it prints around one.
fn unquoted(value: &str) -> &str {
    let value = value.trim();
    value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).unwrap_or(value)
}

/// A picture file this can read: JPEG or PNG, which wallpapers mostly are.
fn readable(path: &Path) -> bool {
    let picture = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| ["png", "jpg", "jpeg"].contains(&extension.to_ascii_lowercase().as_str()));
    picture && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gnome_uri_is_its_file() {
        assert_eq!(from_uri(unquoted("'file:///usr/share/backgrounds/My%20Hill.png'")), PathBuf::from("/usr/share/backgrounds/My Hill.png"));
        assert_eq!(from_uri(unquoted("'/home/sev/Pictures/plain.jpg'")), PathBuf::from("/home/sev/Pictures/plain.jpg"));
        assert_eq!(from_uri("file:///a/%C3%A9t%C3%A9%2.jpg"), PathBuf::from("/a/été%2.jpg"), "a stray % stays");
    }

    #[test]
    fn kde_names_its_wallpaper_under_the_desktop_containment() {
        let appletsrc = "[Containments][1]\nplugin=org.kde.panel\n\n\
                         [Containments][2][Wallpaper][org.kde.image][General]\n\
                         Image=file:///home/sev/Pictures/dunes.jpg\nSlidePaths=/usr/share/wallpapers/\n\n\
                         [Containments][3][Wallpaper][org.kde.image][General]\nImage=/usr/share/wallpapers/Next/\n";
        assert_eq!(kde_image(appletsrc).as_deref(), Some("file:///home/sev/Pictures/dunes.jpg"));
        assert_eq!(kde_image("[Containments][2][General]\nImage=elsewhere.png\n"), None);
        assert_eq!(pixels("3840x2160"), 3840 * 2160);
        assert_eq!(pixels("screenshot"), 0);
    }

    #[test]
    fn xfce_takes_the_first_workspaces_picture() {
        let list = "/backdrop/screen0/monitorVirtual-1/workspace1/last-image\n\
                    /backdrop/screen0/monitorVirtual-1/workspace0/color-style\n\
                    /backdrop/screen0/monitorVirtual-1/workspace0/last-image\n";
        assert_eq!(xfce_property(list), Some("/backdrop/screen0/monitorVirtual-1/workspace0/last-image"));
        assert_eq!(xfce_property("/backdrop/single-workspace-mode\n"), None);
    }

    #[test]
    fn cosmic_names_its_wallpaper_by_path() {
        let all = "(\n    output: \"all\",\n    source: Path(\"/usr/share/backgrounds/cosmic/orion.jpg\"),\n    filter_by_theme: true,\n)";
        assert_eq!(cosmic_source(all).as_deref(), Some("/usr/share/backgrounds/cosmic/orion.jpg"));
        assert_eq!(cosmic_source("(output: \"all\", source: Color(Single((0.1, 0.1, 0.1))))"), None);
    }
}
