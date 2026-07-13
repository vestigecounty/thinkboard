//! Autosave, external-change reload decisions, and the OS file watcher.

use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, SystemTime};

use super::Message;

/// The file's modification time, or `None` if it can't be read.
pub(super) fn file_mtime(path: &FsPath) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Whether to autosave: there are pending changes and they've paused for the
/// debounce interval.
pub(super) fn should_save(dirty: bool, since_last_edit: Duration, debounce: Duration) -> bool {
    dirty && since_last_edit >= debounce
}

/// Whether to reload after an external change. We never reload over unsaved
/// local edits or an open editor, never reload our own write (mtime matches
/// what we last saw), and only when the file's mtime actually differs.
pub(super) fn should_reload(
    dirty: bool,
    editing: bool,
    current: Option<SystemTime>,
    known: Option<SystemTime>,
) -> bool {
    !dirty && !editing && current.is_some() && current != known
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

    // --- Reload decision / race conditions ---

    #[test]
    fn external_change_triggers_reload() {
        assert!(should_reload(false, false, mtime(2), mtime(1)));
    }

    #[test]
    fn unsaved_local_edits_block_reload() {
        // A concurrent external write must not clobber our in-progress edits.
        assert!(!should_reload(true, false, mtime(2), mtime(1)));
    }

    #[test]
    fn open_editor_blocks_reload() {
        // Reloading mid-edit would destroy the editor and the user's caret.
        assert!(!should_reload(false, true, mtime(2), mtime(1)));
    }

    #[test]
    fn our_own_write_is_not_reloaded() {
        // After we save we adopt the new mtime; the watcher event that our
        // own write triggers must be ignored.
        assert!(!should_reload(false, false, mtime(5), mtime(5)));
    }

    #[test]
    fn missing_file_does_not_reload() {
        assert!(!should_reload(false, false, None, mtime(1)));
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

