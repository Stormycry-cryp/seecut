// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The image-editing canvas: a layered document, its blend modes and its
//! edit history.
//!
//! This crate is a port of [Compositor](https://github.com/robbietilton/Compositor)
//! (MIT) into Concat's stack. The port keeps that project's design - a
//! non-destructive layer tree over shared immutable pixels, value-snapshot
//! undo, fourteen blend modes - and changes the substance underneath: the
//! document model is Rust and serde, the pixels live in a side store keyed by
//! identity so history never copies them, and the blend mathematics follow
//! the PDF compositing spec exactly, including the alpha handling Core
//! Graphics gets wrong (see [`blend`]).
//!
//! The split mirrors `concat-render`'s: [`document`] and [`history`] are pure
//! data with no pixels and no IO, [`pixels`] owns the decoded bitmaps, and
//! [`blend`] is the reference compositing math a GPU path will be checked
//! against. Rendering itself arrives with the later phases of the port plan.
//!
//! The design decisions worth knowing before reading further:
//!
//! - Layers hold a [`pixels::PixelId`], never a bitmap. Painting mints a new
//!   id and a new entry in the [`pixels::PixelStore`]; the document around it
//!   changes and the old pixels stay where they were. A history snapshot is
//!   therefore a clone of a small tree, whatever the canvas size - the same
//!   zero-copy property Compositor's snapshots had through shared `CGImage`s.
//! - Transforms are non-destructive: a layer's pixels keep their full
//!   resolution however small the layer is made ([`document::LayerTransform`]).

pub mod blend;
pub mod compositor;
pub mod document;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod history;
pub mod pixels;
pub mod viewport;

pub use blend::BlendMode;
pub use compositor::{apply_one, compose, curve_at_bytes};
pub use document::{
    Adjustment, AdjustmentLayer, CLIP_CHAIN_LIMIT, DocumentError, ImageDocument, ImageLayer,
    LayerGroup, LayerId, LayerMask, LayerNode, LayerSampling, LayerTransform,
};
#[cfg(feature = "gpu")]
pub use gpu::{CanvasGpu, DirtyRect};
pub use history::{DocumentHistory, Snapshot};
pub use pixels::{PixelId, PixelStore};
pub use viewport::{CanvasViewport, NavInput, Navigator};
