//! Snapshot-based undo/redo history.
//!
//! An action records a snapshot of the document *before* it mutates the live
//! state. [`History::undo`] then swaps the live state for the most recent
//! snapshot (stashing the live state so [`History::redo`] can reverse it).
//! Recording a new action discards any redo future — it has diverged from
//! what was undone. The undo depth is capped so a long session can't grow
//! the history without bound.
//!
//! The type is generic over the snapshot payload so it can be tested in
//! isolation with trivial values; the app instantiates it as
//! `History<DocSnapshot>`.

/// A capped undo/redo stack of state snapshots of type `T`.
pub struct History<T> {
    /// Snapshots of past states, oldest first; the top is the next undo.
    past: Vec<T>,
    /// States that were undone, available to redo; the top is the next redo.
    future: Vec<T>,
    /// Maximum number of undo snapshots kept.
    cap: usize,
}

impl<T> History<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            cap,
        }
    }

    /// Records `snapshot` as an undo point and clears the redo future.
    /// Evicts the oldest snapshot once the cap is exceeded.
    pub fn record(&mut self, snapshot: T) {
        self.past.push(snapshot);
        if self.past.len() > self.cap {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Swaps in the most recent undo snapshot: stashes `current` for redo and
    /// returns the state to restore, or `None` when there's nothing to undo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let previous = self.past.pop()?;
        self.future.push(current);
        Some(previous)
    }

    /// Reverses the last undo: stashes `current` for undo and returns the
    /// redone state, or `None` when there's nothing to redo.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.future.pop()?;
        self.past.push(current);
        Some(next)
    }

    /// Drops all history (e.g. when the document is reloaded from disk and
    /// the old snapshots no longer apply).
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_history_has_nothing_to_undo_or_redo() {
        let mut history: History<i32> = History::new(8);
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.undo(0), None);
        assert_eq!(history.redo(0), None);
    }

    #[test]
    fn undo_returns_the_recorded_state_and_enables_redo() {
        let mut history = History::new(8);
        history.record(1); // snapshot of the state before it became 2

        assert!(history.can_undo());
        // Undoing from live state 2 restores 1 and stashes 2 for redo.
        assert_eq!(history.undo(2), Some(1));
        assert!(!history.can_undo());
        assert!(history.can_redo());
        // Redoing from restored state 1 returns to 2.
        assert_eq!(history.redo(1), Some(2));
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn recording_a_new_action_discards_the_redo_future() {
        let mut history = History::new(8);
        history.record(1);
        assert_eq!(history.undo(2), Some(1)); // 2 is now redoable
        assert!(history.can_redo());

        // A fresh action diverges from the undone future.
        history.record(1);
        assert!(!history.can_redo());
        assert_eq!(history.redo(9), None);
    }

    #[test]
    fn the_oldest_snapshot_is_evicted_past_the_cap() {
        let mut history = History::new(2);
        history.record(1);
        history.record(2);
        history.record(3); // evicts 1

        // Only the two most recent undo points remain.
        assert_eq!(history.undo(4), Some(3));
        assert_eq!(history.undo(3), Some(2));
        assert_eq!(history.undo(2), None);
    }

    #[test]
    fn delete_then_undo_then_redo_round_trips_a_document() {
        // Payload stands in for DocSnapshot: a list of edge ids.
        let mut history = History::new(100);
        let mut live = vec!["e0", "e1"];

        // Deleting a link records the pre-change state, then mutates.
        history.record(live.clone());
        live.remove(0);
        assert_eq!(live, ["e1"]);

        // Undo brings the link back.
        live = history.undo(live).unwrap();
        assert_eq!(live, ["e0", "e1"]);

        // Redo removes it again.
        live = history.redo(live).unwrap();
        assert_eq!(live, ["e1"]);
    }

    /// Drives the stack the way `App` does: each `record` snapshots the state
    /// *before* an action, and `undo`/`redo` are handed the current live state.
    /// This walks a multi-step edit history all the way down and back up.
    #[test]
    fn a_multi_step_history_walks_all_the_way_down_and_back_up() {
        let mut history = History::new(100);
        let mut live = 0;
        // Three actions 0->1->2->3, each recording the pre-action state.
        for next in 1..=3 {
            history.record(live);
            live = next;
        }
        // Undo all the way to the start.
        for expected in (0..=2).rev() {
            live = history.undo(live).expect("can undo");
            assert_eq!(live, expected);
        }
        assert!(!history.can_undo());
        // Redo all the way back to the top.
        for expected in 1..=3 {
            live = history.redo(live).expect("can redo");
            assert_eq!(live, expected);
        }
        assert!(!history.can_redo());
    }

    /// The exact shape of the fixed Ask-AI undo bug: two generations, each
    /// recording a pre-generation snapshot, must undo one at a time — not both
    /// at once, and not skipping the state between them.
    #[test]
    fn two_recorded_actions_undo_one_at_a_time() {
        let mut history = History::new(100);
        // "generation 1": snapshot the empty doc, then it holds [g1].
        history.record(vec![]);
        let after_gen1 = vec!["g1"];
        // "generation 2": snapshot [g1], then it holds [g1, g2].
        history.record(after_gen1.clone());
        let live = vec!["g1", "g2"];

        // First undo peels off only generation 2.
        let live = history.undo(live).unwrap();
        assert_eq!(live, ["g1"]);
        // Second undo peels off generation 1.
        let live = history.undo(live).unwrap();
        assert_eq!(live, Vec::<&str>::new());
        assert!(!history.can_undo());
    }

    #[test]
    fn a_fresh_record_after_undo_prunes_the_redo_future_only_once() {
        let mut history = History::new(100);
        history.record(1);
        history.record(2);
        // Undo twice: 1 and 2 are both redoable.
        let live = history.undo(3).unwrap();
        assert_eq!(live, 2);
        let live = history.undo(live).unwrap();
        assert_eq!(live, 1);
        assert!(history.can_redo());

        // A new action from here diverges: the whole redo future is gone.
        history.record(live);
        assert!(!history.can_redo());
        assert_eq!(history.redo(9), None);
        // But the earlier undo point (the one we just recorded) still stands.
        assert_eq!(history.undo(42), Some(1));
    }
}
