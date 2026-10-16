//! Autosave, external-change reload decisions, and the OS file watcher.

use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, SystemTime};

use super::Message;
use crate::model::CanvasDocument;

/// The board's file as this app last read or wrote it — what a change made
/// there is measured against.
pub(super) struct OnDisk {
    /// Its contents: the copy both sides of a merge started from.
    pub(super) document: CanvasDocument,
    /// Its modification time. A change event for a file still showing this is
    /// this app's own write coming back.
    pub(super) mtime: Option<SystemTime>,
    /// Which file it is, whatever it's called — how a rename is followed.
    pub(super) id: Option<FileId>,
    /// It was moved or deleted, and not found again: nothing is saved until
    /// it's back, or the board is told to write it again.
    pub(super) missing: bool,
}

impl OnDisk {
    pub(super) fn new(document: CanvasDocument, path: &FsPath) -> Self {
        Self { document, mtime: file_mtime(path), id: file_id(path), missing: false }
    }
}

/// The file's modification time, or `None` if it can't be read.
pub(super) fn file_mtime(path: &FsPath) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// What a file is, independent of its name: the device and inode, which a
/// rename keeps. Nowhere else has one std can read, and there a rename is
/// recognised by its modification time alone (see [`find_renamed`]).
#[cfg(unix)]
pub(super) type FileId = (u64, u64);
#[cfg(not(unix))]
pub(super) type FileId = ();

pub(super) fn file_id(path: &FsPath) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Where the board's file went, when `old` is gone: another canvas in the same
/// folder that is the same file — the same inode, or failing that the same
/// modification time, both of which a rename keeps.
///
/// Only the folder, because only the folder is watched; a board moved further
/// away is reported as missing rather than searched for. Renaming a board in
/// Obsidian is the case this is for, and Obsidian renames in place.
pub(super) fn find_renamed(old: &FsPath, id: Option<FileId>, mtime: Option<SystemTime>) -> Option<PathBuf> {
    let dir = old.parent()?;
    std::fs::read_dir(dir).ok()?.flatten().map(|entry| entry.path()).find(|path| {
        path != old
            && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("canvas"))
            && ((id.is_some() && file_id(path) == id) || (mtime.is_some() && file_mtime(path) == mtime))
    })
}

/// Whether to autosave: there are pending changes and they've paused for the
/// debounce interval.
pub(super) fn should_save(dirty: bool, since_last_edit: Duration, debounce: Duration) -> bool {
    dirty && since_last_edit >= debounce
}

/// Whether the file has been written since it was last read or written here:
/// its modification time differs from the one known. Our own write adopts
/// the time it left, so the event it raises changes nothing.
pub(super) fn changed_since(current: Option<SystemTime>, known: Option<SystemTime>) -> bool {
    current.is_some() && current != known
}

/// Watches the canvas file for external changes via the OS (FSEvents on
/// macOS) — no polling, so a large vault isn't touched. Watches the file's
/// directory (non-recursive) to survive editors that save via rename.
pub(super) fn watch_file(path: &PathBuf) -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    let path = path.clone();
    iced::stream::channel(4, move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        use iced::futures::SinkExt;
        use notify::{RecursiveMode, Watcher};

        let parent = path
            .parent()
            .map(FsPath::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let file_name = path.file_name().map(std::ffi::OsStr::to_os_string);

        let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
        let Ok(mut watcher) = notify::recommended_watcher(
            move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event {
                    let _ = sender.unbounded_send(event);
                }
            },
        ) else {
            return;
        };
        if watcher.watch(&parent, RecursiveMode::NonRecursive).is_err() {
            return;
        }

        // Hold the watcher for the lifetime of the stream.
        let _watcher = watcher;
        while let Some(event) = receiver.next().await {
            let touches_file = event
                .paths
                .iter()
                .any(|p| p.file_name() == file_name.as_deref());
            let relevant = matches!(
                event.kind,
                notify::EventKind::Modify(_)
                    | notify::EventKind::Create(_)
                    | notify::EventKind::Remove(_)
            );
            if touches_file && relevant {
                let _ = output.send(Message::FileChanged).await;
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::model::{self, CanvasDocument};

    fn mtime(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    // --- Autosave debounce ---

    #[test]
    fn saves_only_after_changes_pause() {
        const DEBOUNCE: Duration = Duration::from_millis(400);
        assert!(!should_save(false, DEBOUNCE, DEBOUNCE)); // nothing pending
        assert!(!should_save(true, DEBOUNCE - Duration::from_millis(1), DEBOUNCE)); // too soon
        assert!(should_save(true, DEBOUNCE, DEBOUNCE)); // paused long enough
    }

    // --- Reload decision ---

    #[test]
    fn external_change_triggers_reload() {
        assert!(changed_since(mtime(2), mtime(1)));
    }

    #[test]
    fn our_own_write_is_not_reloaded() {
        // After we save we adopt the new mtime; the watcher event that our
        // own write triggers must be ignored.
        assert!(!changed_since(mtime(5), mtime(5)));
    }

    #[test]
    fn missing_file_does_not_reload() {
        assert!(!changed_since(None, mtime(1)));
    }

    #[test]
    fn a_renamed_board_is_found_beside_where_it_was() {
        let dir = std::env::temp_dir().join(format!("thinkboard-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("Trip.canvas"), dir.join("Lisbon.canvas"));
        std::fs::write(&old, "{}").unwrap();
        // Another board beside it, which is not the one that moved — written
        // earlier, as it would have been: Windows stamps files from a clock
        // coarse enough to give two quick writes the same time.
        let other = dir.join("Other.canvas");
        std::fs::write(&other, "{}").unwrap();
        let earlier = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&other).unwrap().set_modified(earlier).unwrap();
        let (id, mtime) = (file_id(&old), file_mtime(&old));
        std::fs::rename(&old, &new).unwrap();
        assert_eq!(find_renamed(&old, id, mtime), Some(new.clone()));

        // Gone from the folder entirely: nothing to follow.
        std::fs::remove_file(&new).unwrap();
        assert_eq!(find_renamed(&old, id, mtime), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Disk round-trip between "instances" ---

    #[test]
    fn another_instances_write_is_read_back() {
        let dir = std::env::temp_dir().join(format!("thinkboard-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.canvas");

        // Instance A writes the sample.
        let doc = CanvasDocument::load(FsPath::new(model::DEFAULT_CANVAS)).unwrap();
        doc.save(&path).unwrap();

        // Instance B loads it, adds a node, writes back.
        let mut doc_b = CanvasDocument::load(&path).unwrap();
        let count = doc_b.nodes.len();
        doc_b.nodes.push(model::CanvasNode {
            id: model::random_id(),
            kind: "text".to_owned(),
            text: Some("from B".to_owned()),
            file: None,
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 40.0,
            color: None,
            extra: Default::default(),
        });
        doc_b.save(&path).unwrap();

        // Instance A reloads and sees B's node and preserved custom fields.
        let reloaded = CanvasDocument::load(&path).unwrap();
        assert_eq!(reloaded.nodes.len(), count + 1);
        assert!(reloaded.nodes.iter().any(|n| n.text.as_deref() == Some("from B")));
        assert!(reloaded.to_canvas_json().unwrap().contains(r#""ai":"done""#));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_write_is_rejected_not_panicking() {
        // A reader catching a mid-write file gets an Err, so reload is
        // skipped rather than crashing; the next event retries.
        let dir = std::env::temp_dir().join(format!("thinkboard-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.canvas");
        std::fs::write(&path, "{\n\t\"nodes\":[\n\t\t{\"id\":\"x\",\"ty").unwrap();

        assert!(CanvasDocument::load(&path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

