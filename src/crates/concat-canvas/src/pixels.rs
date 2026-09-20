// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// the layer-pixel indirection that made its undo history copy-free.

//! Decoded bitmaps, held beside the document instead of inside it.
//!
//! A layer names its pixels with a [`PixelId`]; the [`PixelStore`] owns the
//! bitmaps. The indirection is what makes undo cheap: a history snapshot
//! clones the document tree - ids, transforms, masks - and shares the pixels,
//! so no edit ever copies a bitmap, whatever the canvas size.
//!
//! Ids are document-lifetime, not process-lifetime: a store is created with
//! the document and dies with it. `Frame::id()` underneath still gives the
//! renderer its upload key, unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use concat_core::frame::Frame;

/// A layer's pixels, by name. Never zero.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct PixelId(pub(crate) u64);

impl PixelId {
    /// The id no id is: what a field holds before anything was painted into
    /// it. Documents are only handed out with every id filled, but a
    /// `Default` is convenient for `#[derive]` on the UI's view models.
    pub const NONE: PixelId = PixelId(0);
}

/// The bitmaps a document's layers and masks name.
///
/// One store per document. Not serialized: a saved project writes the
/// bitmaps out as image files and re-mints ids on load.
#[derive(Default)]
pub struct PixelStore {
    frames: HashMap<PixelId, Arc<Frame>>,
    next: u64,
}

impl PixelStore {
    /// An empty store, ready to mint ids alongside a new document.
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
            next: 1,
        }
    }

    /// Takes custody of a bitmap and names it.
    pub fn put(&mut self, frame: Frame) -> PixelId {
        let id = PixelId(self.next);
        self.next += 1;
        self.frames.insert(id, Arc::new(frame));
        id
    }

    /// Swaps the bitmap a name refers to, keeping the name. The brush's
    /// commit: the document tree never changes, so a history snapshot taken
    /// before the stroke still shares the old `Arc` - undo is a `replace`
    /// back, zero pixels copied.
    pub fn replace(&mut self, id: PixelId, frame: Frame) {
        if id != PixelId::NONE {
            self.frames.insert(id, Arc::new(frame));
        }
    }

    /// The bitmap a name refers to, for as long as something holds the arc.
    pub fn get(&self, id: PixelId) -> Option<Arc<Frame>> {
        if id == PixelId::NONE {
            None
        } else {
            self.frames.get(&id).cloned()
        }
    }

    /// Whether the pixels a layer names still exist. They always do while
    /// the store lives; the check is for the UI's benefit when a store has
    /// been rebuilt from a save.
    pub fn contains(&self, id: PixelId) -> bool {
        id != PixelId::NONE && self.frames.contains_key(&id)
    }

    /// How many bitmaps the store holds.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the store holds nothing.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Drops the bitmaps nothing in the document names anymore - the previous
    /// versions a paint left behind. The document is read, never changed;
    /// ids it still uses are kept, in any group depth, including masks and
    /// adjustment sources.
    ///
    /// Returns the ids that were dropped, so a caller can also drop any GPU
    /// textures keyed on them.
    pub fn retain_document(&mut self, document: &super::document::ImageDocument) -> Vec<PixelId> {
        let mut used = Vec::new();
        document.collect_pixels(&mut used);
        let mut dropped = Vec::new();
        self.frames.retain(|id, _| {
            if used.contains(id) {
                true
            } else {
                dropped.push(*id);
                false
            }
        });
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_core::frame::Frame;

    #[test]
    fn ids_are_never_zero_and_never_reused() {
        let mut store = PixelStore::new();
        let a = store.put(Frame::black(2, 2));
        let b = store.put(Frame::black(2, 2));
        assert_ne!(a, b);
        assert_ne!(a, PixelId::NONE);
    }

    #[test]
    fn none_names_nothing() {
        let mut store = PixelStore::new();
        store.put(Frame::black(1, 1));
        assert!(!store.contains(PixelId::NONE));
        assert!(store.get(PixelId::NONE).is_none());
    }

    #[test]
    fn replace_keeps_the_name_and_swaps_the_bitmap() {
        let mut store = PixelStore::new();
        let id = store.put(Frame::black(4, 4));
        let before = store.get(id).expect("before");
        store.replace(id, Frame::black(6, 6));
        let after = store.get(id).expect("after");
        assert_eq!(after.width(), 6);
        assert!(!Arc::ptr_eq(&before, &after));
        // NONE stays nothing, whatever arrives.
        store.replace(PixelId::NONE, Frame::black(1, 1));
        assert!(store.get(PixelId::NONE).is_none());
    }

    #[test]
    fn get_hands_out_the_same_arc() {
        let mut store = PixelStore::new();
        let id = store.put(Frame::black(4, 4));
        let a = store.get(id).expect("just put");
        let b = store.get(id).expect("still there");
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.width(), 4);
    }
}
