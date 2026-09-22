// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// the value-snapshot undo stack, minus its retained-byte budget - here the
// document holds no pixels, so a snapshot is a small tree clone by
// construction and only the entry count needs a ceiling.

//! Undo and redo over the document, by value snapshot.
//!
//! Every edit is one `begin` when the gesture starts and one `commit` when
//! it ends: a slider drag is one entry, not one per tick. Between the two,
//! the document is freely mutated - the stack holds whole documents, and a
//! snapshot of a canvas is cheap here because layers share pixels through
//! the store rather than carrying them (see [`crate::pixels`]).
//!
//! Undo restores the *before* document of the last entry; redo restores its
//! *after*. The document is external - the stack hands states back to the
//! caller rather than owning one - so the editor keeps exactly one live
//! document and the stack stays a pure function of what it was asked.

/// The undo/redo stack over a document.
///
/// The snapshot type is fixed to [`crate::document::ImageDocument`]; the
/// active layer rides along because undo should also restore which layer
/// was being edited, the way Compositor's did.
#[derive(Debug)]
pub struct DocumentHistory {
    past: Vec<Entry>,
    future: Vec<Entry>,
    pending: Option<(String, Snapshot)>,
    depth: u32,
    entry_limit: usize,
    revision: u64,
    saved_revision: u64,
}

/// A document state the stack can restore: the whole document, and which
/// layer was active in it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The document as it was.
    pub document: crate::document::ImageDocument,
    /// The layer the editor had selected, if any.
    pub active: Option<crate::document::LayerId>,
    revision: u64,
}

#[derive(Debug)]
struct Entry {
    name: String,
    before: Snapshot,
    after: Snapshot,
}

impl Snapshot {
    fn take(
        document: &crate::document::ImageDocument,
        active: Option<crate::document::LayerId>,
        revision: u64,
    ) -> Self {
        Self {
            document: document.clone(),
            active,
            revision,
        }
    }
}

impl DocumentHistory {
    /// A fresh stack for a fresh document: nothing to undo, nothing to redo,
    /// and the document counts as saved. At most `entry_limit` entries are
    /// kept; older ones fall off the front, as Compositor's did.
    pub fn new(entry_limit: usize) -> Self {
        let revision = 1;
        Self {
            past: Vec::new(),
            future: Vec::new(),
            pending: None,
            depth: 0,
            entry_limit,
            revision,
            saved_revision: revision,
        }
    }

    /// Whether an undo would do anything.
    pub fn can_undo(&self) -> bool {
        self.depth == 0 && !self.past.is_empty()
    }

    /// Whether a redo would do anything.
    pub fn can_redo(&self) -> bool {
        self.depth == 0 && !self.future.is_empty()
    }

    /// What undo would be called, for the menu item.
    pub fn undo_name(&self) -> &str {
        self.past
            .last()
            .map(|entry| entry.name.as_str())
            .unwrap_or("")
    }

    /// What redo would be called, for the menu item.
    pub fn redo_name(&self) -> &str {
        self.future
            .last()
            .map(|entry| entry.name.as_str())
            .unwrap_or("")
    }

    /// How many entries are held.
    pub fn len(&self) -> usize {
        self.past.len()
    }

    /// Whether the stack is empty.
    pub fn is_empty(&self) -> bool {
        self.past.is_empty()
    }

    /// Whether the document differs from the last saved state, for the
    /// window's edited dot. Undoing back to the saved point clears it again.
    pub fn is_modified(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Marks whatever the document is now as the saved state.
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Drops everything: for a document being replaced wholesale. The
    /// revision is bumped, so a stale "modified" cannot survive a reset.
    pub fn reset(&mut self) {
        self.past.clear();
        self.future.clear();
        self.pending = None;
        self.depth = 0;
        self.revision += 1;
        self.saved_revision = self.revision;
    }

    /// Starts an edit named for its menu entry, from the document as it
    /// stands. Call once per gesture; mutations follow; [`commit`](Self::commit)
    /// closes the gesture. A `begin` without a `commit` forgets itself on
    /// the next `begin`, the way a discarded drag should.
    pub fn begin(
        &mut self,
        name: impl Into<String>,
        document: &crate::document::ImageDocument,
        active: Option<crate::document::LayerId>,
    ) {
        if self.depth > 0 {
            // An undo/redo is replaying edits; do not record the replay.
            return;
        }
        self.pending = Some((name.into(), Snapshot::take(document, active, self.revision)));
    }

    /// Closes the gesture begun by the last [`begin`](Self::begin), recording
    /// the document as it now stands. Does nothing when no `begin` is open.
    pub fn commit(
        &mut self,
        document: &crate::document::ImageDocument,
        active: Option<crate::document::LayerId>,
    ) {
        let Some((name, before)) = self.pending.take() else {
            return;
        };
        if before.document == *document && before.active == active {
            // Nothing changed; an entry would be noise.
            return;
        }
        self.revision += 1;
        let after = Snapshot::take(document, active, self.revision);
        self.past.push(Entry {
            name,
            before,
            after,
        });
        self.future.clear();
        if self.past.len() > self.entry_limit {
            self.past.remove(0);
        }
    }

    /// Abandons the gesture in progress: the next commit records nothing.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Steps back one entry. The snapshot to restore is returned; the stack
    /// is marked as mid-replay until the matching
    /// [`end_replay`](Self::end_replay), so edits made while restoring are
    /// recorded as part of the undo, not as new history.
    pub fn undo(&mut self) -> Option<Snapshot> {
        if self.depth > 0 || self.past.is_empty() {
            return None;
        }
        let entry = self.past.pop().expect("checked non-empty");
        let restore = entry.before.clone();
        // The revision goes back with the document, so undoing to the state
        // a save was taken at clears the window's edited dot.
        self.revision = restore.revision;
        self.future.push(entry);
        self.depth += 1;
        Some(restore)
    }

    /// Steps forward one entry undone by [`undo`](Self::undo). The same
    /// replay rules apply.
    pub fn redo(&mut self) -> Option<Snapshot> {
        if self.depth > 0 || self.future.is_empty() {
            return None;
        }
        let entry = self.future.pop().expect("checked non-empty");
        let restore = entry.after.clone();
        self.revision = restore.revision;
        self.past.push(entry);
        self.depth += 1;
        Some(restore)
    }

    /// Ends the replay a `begin`/`commit` inside an undo or redo ran under.
    /// Call after applying the snapshot's document.
    pub fn end_replay(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ImageDocument;
    use crate::pixels::PixelStore;
    use concat_core::frame::Frame;

    fn editor() -> (ImageDocument, PixelStore, DocumentHistory) {
        let mut doc = ImageDocument::new(64, 64);
        let mut store = PixelStore::new();
        doc.new_layer("Layer", store.put(Frame::black(64, 64)));
        (doc, store, DocumentHistory::new(100))
    }

    fn rename(doc: &mut ImageDocument, history: &mut DocumentHistory, name: &str) {
        let id = doc.walk()[0].id();
        history.begin("Rename", doc, Some(id));
        doc.layer_mut(id).expect("layer").name = name.to_string();
        history.commit(doc, Some(id));
    }

    #[test]
    fn an_edit_undoes_and_redoes_exactly() {
        let (mut doc, _store, mut history) = editor();
        let id = doc.walk()[0].id();
        rename(&mut doc, &mut history, "Renamed");
        assert!(history.is_modified());
        assert_eq!(history.undo_name(), "Rename");

        let back = history.undo().expect("undoable");
        assert_eq!(back.document.walk()[0].name(), "Layer");
        assert_eq!(back.active, Some(id));
        history.end_replay();

        let again = history.redo().expect("redoable");
        assert_eq!(again.document.walk()[0].name(), "Renamed");
        assert_eq!(history.undo_name(), "Rename");
        history.end_replay();
    }

    #[test]
    fn undo_restores_the_active_layer() {
        let (mut doc, _store, mut history) = editor();
        let id = doc.walk()[0].id();
        history.begin("Hide", &doc, Some(id));
        doc.layer_mut(id).expect("layer").hidden = true;
        history.commit(&doc, Some(id));

        let back = history.undo().expect("undoable");
        assert_eq!(back.active, Some(id));
        assert!(!back.document.walk()[0].hidden());
        history.end_replay();
    }

    #[test]
    fn a_noop_commit_records_nothing() {
        let (doc, _store, mut history) = editor();
        let id = doc.walk()[0].id();
        history.begin("Nothing", &doc, Some(id));
        history.commit(&doc, Some(id));
        assert!(history.is_empty());
        assert!(!history.is_modified());
    }

    #[test]
    fn a_new_edit_drops_the_redo_future() {
        let (mut doc, _store, mut history) = editor();
        rename(&mut doc, &mut history, "One");
        let back = history.undo().expect("undoable");
        history.end_replay();
        assert_eq!(back.document.walk()[0].name(), "Layer");
        assert!(history.can_redo());
        rename(&mut doc, &mut history, "Two");
        assert!(!history.can_redo());
        assert_eq!(history.undo_name(), "Rename");
    }

    #[test]
    fn the_entry_limit_drops_the_oldest() {
        let (mut doc, _store, mut history) = editor();
        for step in 0..5 {
            rename(&mut doc, &mut history, &format!("Step {step}"));
        }
        assert_eq!(history.len(), 5);

        let (mut tight_doc, _tight_store, _) = editor();
        let mut tight = DocumentHistory::new(2);
        for step in 0..5 {
            rename(&mut tight_doc, &mut tight, &format!("Step {step}"));
        }
        assert_eq!(tight.len(), 2);
        assert_eq!(tight.undo_name(), "Rename");
        // Two undoes exhaust it.
        assert!(tight.undo().is_some());
        tight.end_replay();
        assert!(tight.undo().is_some());
        tight.end_replay();
        assert!(!tight.can_undo());
    }

    #[test]
    fn an_abandoned_gesture_records_nothing() {
        let (mut doc, _store, mut history) = editor();
        let id = doc.walk()[0].id();
        history.begin("Abandoned", &doc, Some(id));
        doc.layer_mut(id).expect("layer").opacity = 0.5;
        history.cancel();
        history.commit(&doc, Some(id));
        assert!(history.is_empty());
    }

    #[test]
    fn mark_saved_clears_and_un_clears_the_dot() {
        let (mut doc, _store, mut history) = editor();
        assert!(!history.is_modified());
        rename(&mut doc, &mut history, "Renamed");
        assert!(history.is_modified());
        history.mark_saved();
        assert!(!history.is_modified());
        // A further edit marks it again...
        rename(&mut doc, &mut history, "Renamed twice");
        assert!(history.is_modified());
        // ...and undoing back past the save clears it once more, because
        // the revision travels with the snapshot.
        let back = history.undo().expect("undoable");
        history.end_replay();
        assert_eq!(back.document.walk()[0].name(), "Renamed");
        assert!(!history.is_modified());
        let back = history.undo().expect("undoable");
        history.end_replay();
        assert_eq!(back.document.walk()[0].name(), "Layer");
        assert!(history.is_modified());
    }

    #[test]
    fn reset_forgets_everything_and_bumps_the_revision() {
        let (mut doc, _store, mut history) = editor();
        rename(&mut doc, &mut history, "Renamed");
        history.reset();
        assert!(history.is_empty());
        assert!(!history.can_redo());
        assert!(!history.is_modified());
    }

    #[test]
    fn replay_edits_are_not_recorded() {
        let (mut doc, _store, mut history) = editor();
        let id = doc.walk()[0].id();
        rename(&mut doc, &mut history, "One");
        let back = history.undo().expect("undoable");
        let mut doc = back.document;
        // While mid-replay, a begin/commit pair must not push history.
        history.begin("Replay noise", &doc, Some(id));
        doc.layer_mut(id).expect("layer").opacity = 0.5;
        history.commit(&doc, Some(id));
        history.end_replay();
        assert!(history.is_empty());
        assert!(!history.can_undo());
    }
}
