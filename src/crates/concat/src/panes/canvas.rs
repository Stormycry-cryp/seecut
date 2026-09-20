// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The canvas pane: the image editor's document in a seat of its own.
//!
//! The pane holds the ported document - [`concat_canvas`]'s layer tree over
//! a pixel store - and the [`Navigator`] that owns its view. Everything the
//! pane's gestures report goes straight into the navigator; everything it
//! shows is published back out of it. The pane itself decides nothing,
//! which is the point: the same view state drives a wheel on macOS, a
//! ctrl-wheel on Windows, and whatever input a pen or a trackpad becomes by
//! the time Slint reports it.
//!
//! Composition runs on the window's own device, the one the renderer draws
//! with and the monitor composites on. A composed canvas is therefore a
//! texture the renderer samples directly - nothing is read back - and the
//! resident-texture cache underneath it means a pan, a zoom or a brush
//! stroke re-composites without re-uploading a single layer.
//!
//! Editing tools arrive with the next phases of the port; the pane is the
//! stage they will act on, and the view they will not have to think about.

use std::path::Path;
use std::sync::Arc;

use concat_canvas::{
    BrushSettings, BrushStroke, CanvasGpu, CanvasViewport, ImageDocument, LayerNode, Mask,
    NavInput, Navigator, PixelId, PixelStore, SelectionShape, erase_region, fill_region,
};
use concat_core::frame::Frame;
use slint::SharedPixelBuffer;

use crate::i18n::tf;
use crate::studio::Studio;

/// What [`CanvasPane::edit_selection`] does to a selection.
#[derive(Clone, Copy, Debug)]
enum EditKind {
    Fill,
    Delete,
}

/// The brush tile edge, in pixels - [`BrushStroke`] paints in these tiles,
/// and the dirty rectangles follow them.
const TILE: usize = 256;

/// The tray's colour swatches, straight RGB. The eraser needs no colour;
/// picking a swatch also leaves erase mode.
const PALETTE: &[[u8; 3]] = &[
    [30, 30, 34],
    [235, 235, 240],
    [212, 59, 55],
    [235, 152, 47],
    [235, 205, 62],
    [88, 168, 78],
    [58, 118, 205],
    [138, 78, 205],
];

/// Everything that can happen to the canvas.
#[derive(Debug)]
pub enum CanvasMsg {
    /// The picker came back with files; the first image is the one opened.
    Picked(Vec<std::path::PathBuf>),
    /// The viewport's box changed, in viewport pixels.
    Resized(f64, f64),
    /// A scroll: pixel deltas, whether the modifier makes it a zoom, and
    /// where over the viewport it happened.
    Scroll {
        dx: f64,
        dy: f64,
        zoom_modifier: bool,
        x: f64,
        y: f64,
    },
    /// A hand-tool or middle-button drag went down, moved, ended.
    PanPress(f64, f64),
    PanMove(f64, f64),
    /// A zoom-tool drag went down (with the option state), moved, ended.
    ZoomPress {
        x: f64,
        y: f64,
        alt: bool,
    },
    ZoomMove(f64, f64),
    /// The pointer went up, whichever drag was live; `alt` rides for the
    /// zoom tool's click-step.
    Release(bool),
    /// Fit the document to the pane.
    Fit,
    /// The tray's tool picker: 0 move, 1 hand, 2 zoom, 3 brush, 4 eraser.
    Tool(i32),
    /// A paint stroke went down, moved, ended. Coordinates are viewport
    /// pixels; the pane converts them through the view it holds.
    BrushPress {
        x: f64,
        y: f64,
    },
    BrushMove(f64, f64),
    BrushRelease,
    /// The brush options' knobs: diameter in pixels, opacity and hardness
    /// in 0..1, and the colour swatch's index into the tray's palette.
    BrushSize(f64),
    BrushOpacity(f64),
    BrushHardness(f64),
    BrushColor(usize),
    /// Step the pixel history back or forward.
    Undo,
    Redo,
    /// The marquee drag went down, moved, ended - viewport pixels.
    MarqueePress {
        x: f64,
        y: f64,
    },
    MarqueeMove(f64, f64),
    MarqueeRelease,
    /// The wand clicked: the selection becomes the colour run under the
    /// pointer.
    WandClick {
        x: f64,
        y: f64,
    },
    /// Whole-frame selection, and dropping whatever is selected.
    SelectAll,
    Deselect,
    /// Fill the selection with the brush's colour, or clear it.
    FillSelection,
    DeleteSelection,
    /// The layers panel: pick a layer, toggle its visibility, set its
    /// opacity, add or delete one, and move one up or down. The index is
    /// the row's position in the panel's list.
    LayerPick(i32),
    LayerToggleVisibility(i32),
    LayerOpacity(i32, f32),
    LayerAdd,
    LayerDelete(i32),
    LayerMove(i32, i32),
    /// Export the composed canvas as a PNG, through a save dialog.
    ExportPng,
}

/// The canvas pane's state.
pub struct CanvasPane {
    /// The composed document, as the pane shows it.
    pub image: slint::Image,
    /// The view, gestures and all.
    pub nav: Navigator,
    /// The open document, or none while the pane is empty.
    pub document: Option<ImageDocument>,
    /// The document's pixels, keyed by identity.
    pub store: PixelStore,
    /// The base layer's pixels, while a document is open.
    pub layer: Option<PixelId>,
    /// The layer the panel has picked, which the tools act on.
    pub active: Option<concat_canvas::LayerId>,
    /// The tray's tool: 0 move, 1 hand, 2 zoom.
    pub tool: usize,
    /// The view's zoom, in percent, read out of the navigator.
    pub zoom: f64,
    /// The view's offset from centre, in viewport pixels.
    pub pan: (f64, f64),
    /// The document's box on screen, in viewport pixels.
    pub stage: (f64, f64),
    /// The open file's name, as the header readout.
    pub name: String,
    /// The brush the painting tools drive: diameter in pixels, opacity and
    /// hardness in 0..1, straight-alpha colour, and whether it erases.
    pub brush: BrushSettings,
    /// The palette index behind the brush's colour, so the tray's swatch
    /// ring reads back.
    pub brush_color_index: usize,
    /// The stroke in flight, while a paint tool is dragging.
    stroke: Option<BrushStroke>,
    /// The layer's pixels as they stood before the stroke: the base the
    /// changed tiles re-copy from, and the undo entry's snapshot.
    stroke_base: Option<Arc<Frame>>,
    /// The stroke's working copy: pre-stroke pixels with the whole stroke
    /// composited so far. One copy per stroke, not per event.
    stroke_scratch: Option<Frame>,
    /// Pixel snapshots, newest last: `(layer, pixels before that edit)`.
    undo_stack: Vec<(PixelId, Arc<Frame>)>,
    /// Undone edits, newest last, for [`CanvasMsg::Redo`].
    redo_stack: Vec<(PixelId, Arc<Frame>)>,
    /// The live selection over the document, if any.
    pub selection: Option<Mask>,
    /// The marquee drag's first corner, while it is in flight.
    marquee_start: Option<(f64, f64)>,
    /// The marquee's box in document pixels, for the overlay while dragging.
    pub marquee: Option<(f64, f64, f64, f64)>,
    /// The selection's box in viewport pixels, for the overlay.
    pub selection_view: Option<(f64, f64, f64, f64)>,
    /// The marquee's box in viewport pixels, for the overlay.
    pub marquee_view: Option<(f64, f64, f64, f64)>,
    /// The compositor, built on the window's device the first time the
    /// pane has something to compose. `None` without a device, and then
    /// every frame goes through the CPU.
    gpu: Option<CanvasGpu>,
    /// Said once: a canvas that cannot compose says so, and then stops
    /// repeating itself.
    failed: bool,
}

impl Default for CanvasPane {
    fn default() -> Self {
        Self {
            image: slint::Image::default(),
            nav: Navigator::new(CanvasViewport::default()),
            document: None,
            store: PixelStore::new(),
            layer: None,
            active: None,
            tool: 0,
            zoom: 100.0,
            pan: (0.0, 0.0),
            stage: (0.0, 0.0),
            name: String::new(),
            brush: BrushSettings {
                diameter: 24.0,
                opacity: 1.0,
                hardness: 0.8,
                color: [30, 30, 34],
                erasing: false,
            },
            brush_color_index: 0,
            stroke: None,
            stroke_base: None,
            stroke_scratch: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            selection: None,
            marquee_start: None,
            marquee: None,
            selection_view: None,
            marquee_view: None,
            gpu: None,
            failed: false,
        }
    }
}

impl CanvasPane {
    /// Applies one message. The studio is the rest of the window; the host
    /// inside it is where the shared device lives. Everything runs on the
    /// event-loop thread, which is the one thread the window's renderer
    /// submits from - the same rule the monitor's drawing keeps.
    pub fn update(&mut self, msg: CanvasMsg, studio: &mut Studio) {
        match msg {
            CanvasMsg::Picked(paths) => {
                if let Some(path) = paths.first() {
                    self.open(path, studio);
                }
            }
            CanvasMsg::Resized(width, height) => {
                let document = self.document_size();
                self.nav
                    .viewport_mut()
                    .resize((width.max(1.0), height.max(1.0)), 1.0, document);
                self.sync_view();
            }
            CanvasMsg::Scroll {
                dx,
                dy,
                zoom_modifier,
                x,
                y,
            } => {
                // Slint reports scroll deltas in pixels, for a wheel notch
                // and a trackpad's fingers alike, so every scroll is the
                // precise kind by the time it gets here.
                self.nav.apply(NavInput::Scroll {
                    delta: (dx, dy),
                    precise: true,
                    zoom_modifier,
                    pointer: (x, y),
                });
                self.render(studio);
            }
            CanvasMsg::PanPress(x, y) => {
                self.nav.apply(NavInput::PanPress((x, y)));
            }
            CanvasMsg::PanMove(x, y) => {
                self.nav.apply(NavInput::PanMove((x, y)));
                self.sync_view();
            }
            CanvasMsg::ZoomPress { x, y, alt } => {
                self.nav.apply(NavInput::ZoomPress {
                    pointer: (x, y),
                    option: alt,
                });
            }
            CanvasMsg::ZoomMove(x, y) => {
                self.nav.apply(NavInput::ZoomMove {
                    pointer: (x, y),
                    option: false,
                });
                self.render(studio);
            }
            CanvasMsg::Release(option) => {
                self.nav.apply(NavInput::Release { option });
                self.render(studio);
            }
            CanvasMsg::Fit => {
                self.nav.apply(NavInput::Fit);
                self.render(studio);
            }
            CanvasMsg::Tool(tool) => self.set_tool(tool),
            CanvasMsg::BrushPress { x, y } => {
                self.brush_press(x, y);
                self.render(studio);
            }
            CanvasMsg::BrushMove(x, y) => {
                let Some(document) = self.document.as_ref() else {
                    return;
                };
                let (dx, dy) = self.to_document(x, y);
                let radius = self.brush.diameter / 2.0;
                // A move past the canvas still paints the fringe it leaves
                // inside; only a move well clear of it is dropped.
                let (w, h) = (f64::from(document.width), f64::from(document.height));
                if dx < -radius || dy < -radius || dx > w + radius || dy > h + radius {
                    return;
                }
                let changed = self.paint_at(dx, dy);
                self.commit_tiles(&changed);
                self.render(studio);
            }
            CanvasMsg::BrushRelease => {
                let changed = self.brush_release();
                self.commit_tiles(&changed);
                self.render(studio);
            }
            CanvasMsg::BrushSize(diameter) => {
                self.brush.diameter = diameter.clamp(1.0, 2000.0);
            }
            CanvasMsg::BrushOpacity(opacity) => {
                self.brush.opacity = opacity.clamp(0.0, 1.0);
            }
            CanvasMsg::BrushHardness(hardness) => {
                self.brush.hardness = hardness.clamp(0.0, 1.0);
            }
            CanvasMsg::BrushColor(index) => {
                if let Some(color) = PALETTE.get(index) {
                    self.brush.color = *color;
                    self.brush.erasing = false;
                    self.brush_color_index = index;
                }
            }
            CanvasMsg::Undo => self.undo(),
            CanvasMsg::Redo => self.redo(),
            CanvasMsg::MarqueePress { x, y } => {
                if self.document.is_some() {
                    self.marquee_start = Some(self.to_document(x, y));
                    self.marquee = Some((x, y, 0.0, 0.0));
                    self.sync_view();
                }
            }
            CanvasMsg::MarqueeMove(x, y) => {
                let Some(start) = self.marquee_start else {
                    return;
                };
                let (dx, dy) = self.to_document(x, y);
                self.marquee = Some((
                    start.0.min(dx),
                    start.1.min(dy),
                    (dx - start.0).abs(),
                    (dy - start.1).abs(),
                ));
                self.sync_view();
            }
            CanvasMsg::MarqueeRelease => {
                if let Some((x, y, w, h)) = self.marquee {
                    let Some((doc_w, doc_h)) = self.document_size() else {
                        return;
                    };
                    let mask = Mask::from_shape(
                        &SelectionShape::Rect {
                            x0: x as f32,
                            y0: y as f32,
                            x1: (x + w) as f32,
                            y1: (y + h) as f32,
                        },
                        doc_w as u32,
                        doc_h as u32,
                        0.0,
                    );
                    if mask.is_empty() {
                        self.selection = None;
                    } else {
                        // Union the dragged box with whatever was selected,
                        // the way a second marquee drag extends a selection.
                        match &mut self.selection {
                            Some(existing) => existing.add(&mask),
                            None => self.selection = Some(mask),
                        }
                    }
                }
                self.marquee_start = None;
                self.marquee = None;
                self.sync_view();
            }
            CanvasMsg::WandClick { x, y } => self.wand_click(x, y),
            CanvasMsg::SelectAll => {
                if let Some((w, h)) = self.document_size() {
                    self.selection = Some(Mask::all(w as u32, h as u32));
                    self.sync_view();
                }
            }
            CanvasMsg::Deselect => {
                self.selection = None;
                self.sync_view();
            }
            CanvasMsg::FillSelection => {
                self.edit_selection(EditKind::Fill);
                self.render(studio);
            }
            CanvasMsg::DeleteSelection => {
                self.edit_selection(EditKind::Delete);
                self.render(studio);
            }
            CanvasMsg::LayerPick(index) => {
                let Some(document) = &self.document else {
                    return;
                };
                let nodes = document.walk();
                let Some(node) = nodes.get(index as usize) else {
                    return;
                };
                self.active = Some(node.id());
                // Painting lands on the picked layer when it can hold
                // pixels; groups and adjustments fall back to the base.
                self.layer = node_image_pixels(node);
                self.sync_view();
            }
            CanvasMsg::LayerToggleVisibility(index) => {
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                let nodes = document.walk();
                if let Some(node) = nodes.get(index as usize) {
                    let id = node.id();
                    match document.find_mut(id) {
                        Some(LayerNode::Layer(layer)) => layer.hidden = !layer.hidden,
                        Some(LayerNode::Group(group)) => group.hidden = !group.hidden,
                        Some(LayerNode::Adjustment(adjustment)) => {
                            adjustment.hidden = !adjustment.hidden;
                        }
                        None => {}
                    }
                }
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerOpacity(index, opacity) => {
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                let nodes = document.walk();
                if let Some(node) = nodes.get(index as usize) {
                    let id = node.id();
                    match document.find_mut(id) {
                        Some(LayerNode::Layer(layer)) => layer.opacity = opacity.clamp(0.0, 1.0),
                        Some(LayerNode::Group(group)) => group.opacity = opacity.clamp(0.0, 1.0),
                        Some(LayerNode::Adjustment(adjustment)) => {
                            adjustment.opacity = opacity.clamp(0.0, 1.0)
                        }
                        None => {}
                    }
                }
                self.render(studio);
            }
            CanvasMsg::LayerAdd => {
                let Some(document) = self.document.as_ref() else {
                    return;
                };
                let (w, h) = (document.width, document.height);
                let count = document.walk().len();
                let frame = Frame::transparent(w, h);
                let pixels = self.store.put(frame);
                let name = tf("Layer {0}", &[&count.to_string()]).to_string();
                let id = self
                    .document
                    .as_mut()
                    .expect("checked")
                    .new_layer(name, pixels);
                self.active = Some(id);
                self.layer = Some(pixels);
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerDelete(index) => {
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                let nodes = document.walk();
                let Some(node) = nodes.get(index as usize) else {
                    return;
                };
                let id = node.id();
                document.remove(id);
                self.store.retain_document(document);
                // The active layer follows: the topmost image layer left.
                let topmost = document
                    .root
                    .children
                    .iter()
                    .rev()
                    .find(|n| matches!(n, LayerNode::Layer(_)));
                self.active = topmost.map(LayerNode::id);
                self.layer = topmost.and_then(node_image_pixels);
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerMove(index, direction) => {
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                let nodes = document.walk();
                let Some(node) = nodes.get(index as usize) else {
                    return;
                };
                let id = node.id();
                let from = document
                    .root
                    .children
                    .iter()
                    .position(|c| c.id() == id)
                    .unwrap_or(index as usize);
                // The panel lists front-to-back, the children are back-to-
                // front: up in the panel is down in the children.
                let to = (from as i64 - i64::from(direction)).max(0) as usize;
                if to < document.root.children.len() {
                    let node = document.root.children.remove(from);
                    document.root.children.insert(to, node);
                }
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::ExportPng => self.export_png(studio),
        }
    }

    /// The tray's tool, clamped into it: a picker can only offer what the
    /// tray has, but the number arrives over a boundary. The eraser is the
    /// brush with its erasing bit on, so the settings stay shared.
    pub fn set_tool(&mut self, tool: i32) {
        self.tool = (tool.max(0) as usize).min(6);
        self.brush.erasing = self.tool == 4;
    }

    /// The wand's click: the selection becomes the colour run under the
    /// pointer, flood-filled from the layer's pixels.
    fn wand_click(&mut self, x: f64, y: f64) {
        let Some(layer) = self.layer else {
            return;
        };
        let Some(frame) = self.store.get(layer) else {
            return;
        };
        let (dx, dy) = self.to_document(x, y);
        let (w, h) = (frame.width(), frame.height());
        if dx < 0.0 || dy < 0.0 || dx >= f64::from(w) || dy >= f64::from(h) {
            return;
        }
        let mask = Mask::from_magic_wand(&frame, (dx as u32, dy as u32), 0.1, true);
        if mask.is_empty() {
            self.selection = None;
        } else {
            match &mut self.selection {
                Some(existing) => existing.add(&mask),
                None => self.selection = Some(mask),
            }
        }
        self.sync_view();
    }

    /// Fill or clear whatever is selected on the active layer: one undo
    /// entry, one whole-rect re-upload, one recomposite.
    fn edit_selection(&mut self, kind: EditKind) {
        let Some(layer) = self.layer else {
            return;
        };
        let Some(mask) = &self.selection else {
            return;
        };
        let Some(before) = self.store.get(layer) else {
            return;
        };
        let mut frame = (*before).clone();
        match kind {
            EditKind::Fill => fill_region(&mut frame, mask, self.brush.color),
            EditKind::Delete => erase_region(&mut frame, mask),
        }
        self.undo_stack.push((layer, before));
        self.redo_stack.clear();
        self.store.replace(layer, frame.clone());
        if let Some(gpu) = &mut self.gpu {
            gpu.upload(layer, &frame, None);
        }
    }

    /// Viewport pixels to document pixels, through the view the navigator
    /// holds - the one conversion, for the canvas gestures and the brush
    /// alike, with the device scale already inside it.
    fn to_document(&self, x: f64, y: f64) -> (f64, f64) {
        match self.document_size() {
            Some(size) => self.nav.viewport().document_point((x, y), size),
            None => (x, y),
        }
    }

    /// Starts a stroke: the layer's pixels are snapshotted for the undo
    /// entry and for the tiles' base, a scratch frame is copied once, and
    /// the first dab goes down. Pixel work only; the caller renders.
    fn brush_press(&mut self, x: f64, y: f64) {
        let Some((w, h)) = self.document_size().map(|(w, h)| (w as u32, h as u32)) else {
            return;
        };
        let Some(layer) = self.layer else {
            return;
        };
        // A second press while one stroke is live finishes it first: two
        // pointers, or a lost release, must not nest strokes.
        let finished = self.brush_release();
        self.commit_tiles(&finished);
        let (dx, dy) = self.to_document(x, y);
        // A press off the canvas starts nothing; a drag onto it does, via
        // the move's fringe rule.
        if dx < 0.0 || dy < 0.0 || dx >= f64::from(w) || dy >= f64::from(h) {
            return;
        }
        let Ok(stroke) = BrushStroke::new(w, h, self.brush) else {
            return;
        };
        let base = self.store.get(layer);
        self.stroke_base = base.clone();
        self.stroke_scratch = base.map(|f| (*f).clone());
        self.undo_stack
            .push((layer, self.stroke_base.clone().expect("just set")));
        self.redo_stack.clear();
        self.stroke = Some(stroke);
        let changed = self.paint_at(dx, dy);
        self.commit_tiles(&changed);
    }

    /// One pointer sample into the live stroke. Returns the tiles whose
    /// coverage changed.
    fn paint_at(&mut self, dx: f64, dy: f64) -> Vec<(usize, usize)> {
        match &mut self.stroke {
            Some(stroke) => stroke.append((dx, dy)),
            None => Vec::new(),
        }
    }

    /// Ends the stroke: the tail settles and the working pixels land in
    /// the store. Returns the tiles the final settle changed.
    fn brush_release(&mut self) -> Vec<(usize, usize)> {
        let Some(mut stroke) = self.stroke.take() else {
            return Vec::new();
        };
        let changed = stroke.flush();
        if let (Some(layer), Some(scratch)) = (self.layer, self.stroke_scratch.take()) {
            self.store.replace(layer, scratch);
        }
        self.stroke_base = None;
        changed
    }

    /// Stamps the changed tiles: they re-copy from the pre-stroke pixels
    /// (the stroke's coverage is cumulative, so a tile is always built
    /// from the base, never over an earlier stamp), the store keeps the
    /// working pixels when no compositor will upload them, and the GPU's
    /// resident texture takes just those rectangles.
    fn commit_tiles(&mut self, changed: &[(usize, usize)]) {
        if changed.is_empty() {
            return;
        }
        let Some(layer) = self.layer else {
            return;
        };
        let (Some(base), Some(scratch)) = (self.stroke_base.clone(), self.stroke_scratch.as_mut())
        else {
            return;
        };
        let (width, height) = (scratch.width(), scratch.height());
        let (src, dst) = (base.pixels(), scratch.pixels_mut());
        for &(tx, ty) in changed {
            copy_tile(src, dst, width, height, tx, ty);
        }
        if let Some(stroke) = &self.stroke {
            stroke.composite_tiles(scratch.pixels_mut(), changed);
        }
        match &mut self.gpu {
            Some(gpu) => {
                for &(tx, ty) in changed {
                    gpu.upload(
                        layer,
                        scratch,
                        Some(concat_canvas::DirtyRect {
                            x: (tx * TILE) as u32,
                            y: (ty * TILE) as u32,
                            width: TILE as u32,
                            height: TILE as u32,
                        }),
                    );
                }
            }
            // No compositor: the CPU picture reads the store, so the
            // working pixels land there and the next render recomposites.
            None => self.store.replace(layer, scratch.clone()),
        }
    }

    /// Steps the pixel history back one edit. The undone pixels move to
    /// the redo stack; the resident texture re-uploads whole, which an
    /// undo's single frame can afford.
    fn undo(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        let Some((layer, before)) = self.undo_stack.pop() else {
            return;
        };
        if let Some(current) = self.store.get(layer) {
            self.redo_stack.push((layer, current));
        }
        self.store.replace(layer, (*before).clone());
        if let Some(gpu) = &mut self.gpu {
            gpu.upload(layer, &before, None);
        }
    }

    /// Steps the pixel history forward one undone edit.
    fn redo(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        let Some((layer, after)) = self.redo_stack.pop() else {
            return;
        };
        if let Some(current) = self.store.get(layer) {
            self.undo_stack.push((layer, current));
        }
        self.store.replace(layer, (*after).clone());
        if let Some(gpu) = &mut self.gpu {
            gpu.upload(layer, &after, None);
        }
    }

    /// The layers panel's rows, front-to-back: identity, name, visibility,
    /// opacity, and whether the row is the active one.
    pub fn layers_data(&self) -> Vec<(u64, String, bool, f32, bool)> {
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        document
            .root
            .children
            .iter()
            .rev()
            .map(|node| {
                (
                    node.id().as_u64(),
                    node.name().to_owned(),
                    node.hidden(),
                    node.opacity(),
                    self.active == Some(node.id()),
                )
            })
            .collect()
    }

    /// Composites the document and writes it out as a PNG, through a save
    /// dialog. The GPU path reads its own texture back; the CPU path
    /// composites from the store.
    fn export_png(&mut self, studio: &mut Studio) {
        let Some(document) = self.document.clone() else {
            return;
        };
        let frame = match &mut self.gpu {
            Some(gpu) => gpu.compose_frame(&document, &self.store),
            None => concat_canvas::compose(&document, &self.store),
        };
        let name = match self.name.rsplit_once('.') {
            Some((stem, _)) => format!("{stem}.png"),
            None => self.name.clone(),
        };
        let Some(path) =
            crate::platform::save_file(&tf("Export as PNG", &[]), &name, Some(("PNG", &["png"])))
        else {
            return;
        };
        match encode_png(&frame) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&path, bytes) {
                    log::warn!("canvas: {error}");
                    studio.notify(&tf("Canvas failed: {0}", &[&error.to_string()]), true);
                }
            }
            Err(error) => {
                log::warn!("canvas: {error}");
                studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
            }
        }
    }

    /// The document's size, as the navigator wants it: `None` while the
    /// pane is empty, so the guards ignore navigation there.
    fn document_size(&self) -> Option<(f64, f64)> {
        self.document
            .as_ref()
            .map(|d| (f64::from(d.width), f64::from(d.height)))
    }

    /// Reads the view out of the navigator into the published fields. The
    /// zoom readout and the stage box follow the navigator; the pane
    /// carries no view state of its own. The selection and the in-flight
    /// marquee come along, converted to viewport pixels for the overlay.
    fn sync_view(&mut self) {
        self.zoom = self.nav.viewport().zoom() * 100.0;
        self.pan = self.nav.viewport().pan();
        self.stage = self
            .document_size()
            .map(|(width, height)| {
                let zoom = self.nav.viewport().zoom();
                (width * zoom, height * zoom)
            })
            .unwrap_or((0.0, 0.0));
        self.selection_view = self
            .selection
            .as_ref()
            .and_then(|mask| self.mask_view_rect(mask.bounds()?));
        self.marquee_view = self.marquee.and_then(|(x, y, w, h)| {
            if w <= 0.0 || h <= 0.0 {
                return None;
            }
            self.mask_view_rect((x.max(0.0) as u32, y.max(0.0) as u32, w as u32, h as u32))
        });
    }

    /// A document-pixel box as a viewport-pixel box, through the view.
    fn mask_view_rect(&self, (x, y, w, h): (u32, u32, u32, u32)) -> Option<(f64, f64, f64, f64)> {
        let size = self.document_size()?;
        let zoom = self.nav.viewport().zoom();
        let (vx, vy) = self
            .nav
            .viewport()
            .view_point((f64::from(x), f64::from(y)), size);
        Some((vx, vy, f64::from(w) * zoom, f64::from(h) * zoom))
    }

    /// Opens an image as a one-layer document: decoded straight-alpha, one
    /// layer the size of the canvas, the view fitted to it.
    fn open(&mut self, path: &Path, studio: &mut Studio) {
        match decode(path) {
            Ok(frame) => {
                let (width, height) = (frame.width(), frame.height());
                let mut document = ImageDocument::new(width, height);
                let pixels = self.store.put(frame);
                let base = document.new_layer(
                    path.file_stem().unwrap_or_default().to_string_lossy(),
                    pixels,
                );
                self.document = Some(document);
                self.layer = Some(pixels);
                self.active = Some(base);
                self.selection = None;
                self.marquee = None;
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.failed = false;
                // The view starts over: fitted to what was just opened, in
                // whatever box the pane has.
                let size = (f64::from(width), f64::from(height));
                self.nav.set_document(Some(size));
                self.nav.viewport_mut().fit(size);
                self.render(studio);
            }
            Err(error) => {
                log::warn!("canvas: {error}");
                if !self.failed {
                    self.failed = true;
                    studio.notify(&tf("Could not open {0}", &[&error]), true);
                }
            }
        }
    }

    /// Composites the document and publishes the picture. The GPU path
    /// leaves the result as a texture the renderer samples; the CPU path
    /// hands Slint the pixels. Both run on this thread.
    pub fn render(&mut self, studio: &mut Studio) {
        self.sync_view();
        let Some(document) = self.document.clone() else {
            return;
        };
        if self.gpu.is_none() {
            // The device is taken, not cloned: once the pane owns the
            // compositor it owns the only handle it needs. The monitor
            // keeps its own, and wgpu handles are cheap shells over one
            // device.
            self.gpu = studio
                .host
                .gpu_device
                .take()
                .zip(studio.host.gpu_queue.take())
                .map(|(device, queue)| CanvasGpu::with_device(device, queue));
        }
        let picture = match &mut self.gpu {
            Some(gpu) => Some(
                slint::Image::try_from(gpu.compose_texture(&document, &self.store))
                    .map_err(|error| format!("canvas texture: {error}")),
            ),
            None => None,
        };
        let picture = match picture {
            Some(picture) => picture,
            None => Ok(cpu_picture(&document, &self.store)),
        };
        match picture {
            Ok(image) => self.image = image,
            Err(error) => {
                log::warn!("canvas: {error}");
                if !self.failed {
                    self.failed = true;
                    studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The automation surface.
//
// Everything a hand can do to the canvas, an agent can do in code. These
// methods call the same private methods the `CanvasMsg` branches call - the
// brush's press-move-release, the selection edits, the layer reshuffles -
// so a driven pane walks exactly the path a driven hand does, minus the
// render callbacks a headless run has no window for. Coordinates are
// document pixels unless a name says otherwise. The `agent_` prefix keeps
// the surface greppable: one search lists the whole contract.
// ---------------------------------------------------------------------------
// The surface is an API, not a call site: an embedding (an agent harness, a
// test, a future scripting bridge) reaches it from outside the binary, so
// the app's own build seeing no callers is not dead code.
#[allow(dead_code)]
impl CanvasPane {
    /// The tray's tool: 0 move, 1 hand, 2 zoom, 3 brush, 4 eraser.
    pub fn agent_select_tool(&mut self, tool: i32) {
        self.set_tool(tool);
    }

    /// The brush's four knobs: diameter in pixels, opacity and hardness in
    /// 0..1, and a palette index. The colour rides along with the stroke,
    /// exactly as the tray's swatches feed it.
    pub fn agent_set_brush(&mut self, diameter: f64, opacity: f64, hardness: f64, palette: usize) {
        self.brush.diameter = diameter.clamp(1.0, 2000.0);
        self.brush.opacity = opacity.clamp(0.0, 1.0);
        self.brush.hardness = hardness.clamp(0.0, 1.0);
        if let Some(color) = PALETTE.get(palette) {
            self.brush.color = *color;
            self.brush.erasing = false;
            self.brush_color_index = palette;
        }
    }

    /// Paints one stroke through the points, in document pixels: the same
    /// press-move-release a pointer makes, with the provisional tail and
    /// the curve settle exactly where the hand leaves them.
    pub fn agent_paint_stroke(&mut self, points: &[(f64, f64)]) {
        let mut points = points.iter().copied().peekable();
        let Some(first) = points.next() else {
            return;
        };
        self.brush_press(first.0, first.1);
        for (x, y) in points {
            let Some(document) = self.document.as_ref() else {
                continue;
            };
            let (dx, dy) = self.to_document(x, y);
            let radius = self.brush.diameter / 2.0;
            let (w, h) = (f64::from(document.width), f64::from(document.height));
            if dx < -radius || dy < -radius || dx > w + radius || dy > h + radius {
                continue;
            }
            let changed = self.paint_at(dx, dy);
            self.commit_tiles(&changed);
        }
        let changed = self.brush_release();
        self.commit_tiles(&changed);
    }

    /// Selects a rectangle, in document pixels: one marquee drag's result.
    pub fn agent_select_rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        let Some((doc_w, doc_h)) = self.document_size() else {
            return;
        };
        let mask = Mask::from_shape(
            &SelectionShape::Rect {
                x0: x as f32,
                y0: y as f32,
                x1: (x + width) as f32,
                y1: (y + height) as f32,
            },
            doc_w as u32,
            doc_h as u32,
            0.0,
        );
        if mask.is_empty() {
            self.selection = None;
        } else {
            match &mut self.selection {
                Some(existing) => existing.add(&mask),
                None => self.selection = Some(mask),
            }
        }
        self.sync_view();
    }

    /// Selects the colour run under a point, in document pixels: one wand
    /// click.
    pub fn agent_wand(&mut self, x: f64, y: f64) {
        self.wand_click(x, y);
    }

    /// Selects the whole frame, or drops whatever is selected.
    pub fn agent_select_all(&mut self) {
        if let Some((w, h)) = self.document_size() {
            self.selection = Some(Mask::all(w as u32, h as u32));
            self.sync_view();
        }
    }

    pub fn agent_deselect(&mut self) {
        self.selection = None;
        self.sync_view();
    }

    /// Fills the selection with the brush's colour, or clears it.
    pub fn agent_fill_selection(&mut self) {
        self.edit_selection(EditKind::Fill);
    }

    pub fn agent_delete_selection(&mut self) {
        self.edit_selection(EditKind::Delete);
    }

    /// Steps the pixel history back or forward one edit.
    pub fn agent_undo(&mut self) {
        self.undo();
    }

    pub fn agent_redo(&mut self) {
        self.redo();
    }

    /// The layers panel, by row index in [`Self::layers_data`]'s order:
    /// pick, toggle visibility, set opacity (0..1), add, delete, and move
    /// (`direction` -1 up, 1 down) - the panel's own verbs, on its rows.
    pub fn agent_layer_pick(&mut self, row: i32) {
        let Some(document) = &self.document else {
            return;
        };
        let nodes = document.walk();
        let Some(node) = nodes.get(row as usize) else {
            return;
        };
        self.active = Some(node.id());
        self.layer = node_image_pixels(node);
        self.sync_view();
    }

    pub fn agent_layer_toggle_visibility(&mut self, row: i32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let nodes = document.walk();
        if let Some(node) = nodes.get(row as usize) {
            let id = node.id();
            match document.find_mut(id) {
                Some(LayerNode::Layer(layer)) => layer.hidden = !layer.hidden,
                Some(LayerNode::Group(group)) => group.hidden = !group.hidden,
                Some(LayerNode::Adjustment(adjustment)) => {
                    adjustment.hidden = !adjustment.hidden;
                }
                None => {}
            }
            self.sync_view();
        }
    }

    pub fn agent_layer_opacity(&mut self, row: i32, opacity: f32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let nodes = document.walk();
        if let Some(node) = nodes.get(row as usize) {
            let id = node.id();
            match document.find_mut(id) {
                Some(LayerNode::Layer(layer)) => layer.opacity = opacity.clamp(0.0, 1.0),
                Some(LayerNode::Group(group)) => group.opacity = opacity.clamp(0.0, 1.0),
                Some(LayerNode::Adjustment(adjustment)) => {
                    adjustment.opacity = opacity.clamp(0.0, 1.0);
                }
                None => {}
            }
        }
    }

    pub fn agent_layer_add(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let (w, h) = (document.width, document.height);
        let count = document.walk().len();
        let pixels = self.store.put(Frame::transparent(w, h));
        let name = tf("Layer {0}", &[&count.to_string()]).to_string();
        let id = self
            .document
            .as_mut()
            .expect("checked")
            .new_layer(name, pixels);
        self.active = Some(id);
        self.layer = Some(pixels);
        self.sync_view();
    }

    pub fn agent_layer_delete(&mut self, row: i32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let nodes = document.walk();
        let Some(node) = nodes.get(row as usize) else {
            return;
        };
        let id = node.id();
        document.remove(id);
        self.store.retain_document(document);
        let topmost = document
            .root
            .children
            .iter()
            .rev()
            .find(|n| matches!(n, LayerNode::Layer(_)));
        self.active = topmost.map(LayerNode::id);
        self.layer = topmost.and_then(node_image_pixels);
        self.sync_view();
    }

    pub fn agent_layer_move(&mut self, row: i32, direction: i32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let nodes = document.walk();
        let Some(node) = nodes.get(row as usize) else {
            return;
        };
        let id = node.id();
        let from = document
            .root
            .children
            .iter()
            .position(|c| c.id() == id)
            .unwrap_or(row as usize);
        // The panel lists front-to-back, the children are back-to-front:
        // up in the panel is down in the children.
        let to = (from as i64 - i64::from(direction)).max(0) as usize;
        if to < document.root.children.len() {
            let node = document.root.children.remove(from);
            document.root.children.insert(to, node);
        }
        self.sync_view();
    }

    /// Writes the composed canvas to `path` as PNG - the export without
    /// the save dialog, which an unattended run cannot answer.
    pub fn agent_export_png(&mut self, path: &Path) -> Result<(), String> {
        let Some(document) = self.document.clone() else {
            return Err(tf("No image open", &[]));
        };
        let frame = match &mut self.gpu {
            Some(gpu) => gpu.compose_frame(&document, &self.store),
            None => concat_canvas::compose(&document, &self.store),
        };
        let bytes = encode_png(&frame)?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    /// The document's box, in pixels - `None` with nothing open.
    pub fn agent_document_size(&self) -> Option<(f64, f64)> {
        self.document_size()
    }

    /// The selection's document-pixel bounds, `None` with nothing selected.
    pub fn agent_selection_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        self.selection.as_ref().and_then(|mask| mask.bounds())
    }

    /// The layers panel's rows, front-to-back - the same rows the panel
    /// shows, and the row indexes the layer methods take.
    pub fn agent_layers(&self) -> Vec<(u64, String, bool, f32, bool)> {
        self.layers_data()
    }

    /// The brush as it is set right now.
    pub fn agent_brush(&self) -> concat_canvas::BrushSettings {
        self.brush
    }
}

/// Composites on the CPU and hands Slint the pixels - the path when the
/// machine offered no adapter, and the canvas still has to work.
fn cpu_picture(document: &ImageDocument, store: &PixelStore) -> slint::Image {
    let frame = concat_canvas::compose(document, store);
    let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        frame.pixels(),
        frame.width(),
        frame.height(),
    );
    slint::Image::from_rgba8(buffer)
}

/// Copies one brush tile's pixels from `src` to `dst`, clipped to the
/// frame. Both frames stride `width`; the tile index is `(tx, ty)`.
fn copy_tile(src: &[u8], dst: &mut [u8], width: u32, height: u32, tx: usize, ty: usize) {
    let x0 = (tx * TILE) as u32;
    let y0 = (ty * TILE) as u32;
    let tw = (TILE as u32).min(width.saturating_sub(x0));
    let th = (TILE as u32).min(height.saturating_sub(y0));
    for row in 0..th {
        let start = ((y0 + row) * width + x0) as usize * 4;
        let len = tw as usize * 4;
        dst[start..start + len].copy_from_slice(&src[start..start + len]);
    }
}

/// The pixels a node's painting lands on: an image layer's own pixels, and
/// nothing for groups or adjustments.
fn node_image_pixels(node: &LayerNode) -> Option<PixelId> {
    match node {
        LayerNode::Layer(layer) => Some(layer.pixels),
        LayerNode::Group(_) | LayerNode::Adjustment(_) => None,
    }
}

/// Encodes a frame as PNG bytes.
fn encode_png(frame: &Frame) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut out);
        let mut encoder = png::Encoder::new(&mut cursor, frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(frame.pixels())
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decodes an image file into straight-alpha RGBA. PNG through the `png`
/// crate, which the window already keeps; everything else through `image`,
/// whose feature set here covers the JPEG the artwork cache wanted - a
/// format outside that set is the error, not a panic.
fn decode(path: &Path) -> Result<Frame, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path
        .extension()
        .map(|e| e.to_ascii_lowercase().to_string_lossy().into_owned())
        .unwrap_or_default();
    let (width, height, pixels) = match name.as_str() {
        "png" => decode_png(&bytes)?,
        _ => {
            let image = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let (width, height) = image.dimensions();
            (width, height, image.into_raw())
        }
    };
    Frame::from_rgba(width, height, pixels).ok_or_else(|| "empty image".to_owned())
}

/// PNG to RGBA, any of the colour types a still is likely to come in.
/// Returns `(width, height, rgba)`.
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let plain = &buffer[..info.buffer_size()];
    let pixels = match info.color_type {
        png::ColorType::Rgba => plain.to_vec(),
        png::ColorType::Rgb => expand(plain, 3),
        png::ColorType::Grayscale => expand(plain, 1),
        png::ColorType::GrayscaleAlpha => expand(plain, 2),
        other => return Err(format!("png: unsupported colour type {other:?}")),
    };
    Ok((info.width, info.height, pixels))
}

/// Greys and RGBs to RGBA, one output pixel per `channels` input bytes. The
/// alpha comes from the second channel of a grey+alpha pair and is opaque
/// otherwise.
fn expand(bytes: &[u8], channels: usize) -> Vec<u8> {
    let alpha_at = match channels {
        2 => Some(1),
        4 => Some(3),
        _ => None,
    };
    let mut out = Vec::with_capacity(bytes.len() / channels * 4);
    for pixel in bytes.chunks_exact(channels) {
        let (r, g, b) = match channels {
            1 | 2 => (pixel[0], pixel[0], pixel[0]),
            _ => (pixel[0], pixel[1], pixel[2]),
        };
        let a = alpha_at.map(|at| pixel[at]).unwrap_or(255);
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_stays_in_the_tray() {
        let mut pane = CanvasPane::default();
        pane.set_tool(4);
        assert_eq!(pane.tool, 4);
        pane.set_tool(6);
        assert_eq!(pane.tool, 6);
        pane.set_tool(9);
        assert_eq!(pane.tool, 6, "a tool past the tray is the last one");
        pane.set_tool(-4);
        assert_eq!(pane.tool, 0, "a tool before the tray is the first one");
    }

    #[test]
    fn an_opened_document_is_one_layer_the_size_of_the_canvas() {
        let mut pane = CanvasPane::default();
        let frame = Frame::from_rgba(4, 3, vec![128; 4 * 3 * 4]).expect("frame");
        let mut document = ImageDocument::new(frame.width(), frame.height());
        let pixels = pane.store.put(frame);
        document.new_layer("probe", pixels);
        pane.document = Some(document);
        pane.layer = Some(pixels);

        assert_eq!(pane.document_size(), Some((4.0, 3.0)));
        assert_eq!(pane.store.get(pixels).map(|f| f.pixels().len()), Some(48));
    }

    /// A pane with a 300x200 document whose view sits at 100% over a
    /// 300x200 well, so viewport and document pixels coincide.
    fn painting_pane() -> (CanvasPane, PixelId) {
        let mut pane = CanvasPane::default();
        let frame = Frame::from_rgba(300, 200, vec![0; 300 * 200 * 4]).expect("frame");
        let mut document = ImageDocument::new(frame.width(), frame.height());
        let pixels = pane.store.put(frame);
        document.new_layer("probe", pixels);
        pane.document = Some(document);
        pane.layer = Some(pixels);
        pane.nav.set_document(Some((300.0, 200.0)));
        pane.nav
            .viewport_mut()
            .resize((300.0, 200.0), 1.0, Some((300.0, 200.0)));
        (pane, pixels)
    }

    #[test]
    fn a_click_with_the_view_at_one_paints_one_dab() {
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        pane.commit_tiles(&[(0, 0)]);

        let frame = pane.store.get(pixels).expect("pixels");
        let alpha: Vec<u8> = frame.pixels()[3..].iter().step_by(4).copied().collect();
        assert!(
            alpha.iter().any(|&a| a > 0),
            "the dab painted something opaque"
        );
        let centre = alpha[100 * 300 + 150];
        assert_eq!(centre, 255, "a hard 100% brush saturates its centre");
    }

    #[test]
    fn undo_restores_the_pre_stroke_pixels_and_redo_repeats_it() {
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let painted = pane.store.get(pixels).expect("pixels").clone();

        pane.undo();
        let undone = pane.store.get(pixels).expect("pixels");
        assert!(
            undone.pixels().iter().all(|&b| b == 0),
            "undo returns the empty layer"
        );

        pane.redo();
        let redone = pane.store.get(pixels).expect("pixels");
        assert_eq!(redone.pixels(), painted.pixels(), "redo repeats the stroke");
    }

    #[test]
    fn the_eraser_clears_alpha() {
        let (mut pane, pixels) = painting_pane();
        {
            let frame = pane.store.get(pixels).expect("pixels");
            let mut editable = (*frame).clone();
            for pixel in editable.pixels_mut().chunks_exact_mut(4) {
                pixel.copy_from_slice(&[10, 20, 30, 255]);
            }
            pane.store.replace(pixels, editable);
        }
        pane.set_tool(4);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();

        let frame = pane.store.get(pixels).expect("pixels");
        let centre = frame.pixels()[(100 * 300 + 150) * 4 + 3];
        assert_eq!(centre, 0, "the eraser cleared its centre");
    }

    #[test]
    fn a_press_off_the_canvas_starts_nothing() {
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(500.0, 100.0);
        pane.brush_release();
        assert!(pane.undo_stack.is_empty(), "no stroke, no history entry");
        assert!(pane.store.get(pixels).unwrap().pixels()[3] == 0);
    }

    #[test]
    fn viewport_pixels_convert_through_the_view() {
        let (mut pane, _) = painting_pane();
        // Zoom to 200%: the stage grows past the well, centred still, so
        // the well's centre reads the canvas centre.
        pane.nav.set_document(Some((300.0, 200.0)));
        pane.nav
            .viewport_mut()
            .resize((300.0, 200.0), 1.0, Some((300.0, 200.0)));
        pane.nav
            .viewport_mut()
            .set_zoom(2.0, (150.0, 100.0), (300.0, 200.0));
        let (dx, dy) = pane.to_document(150.0, 100.0);
        assert_eq!(dx, 150.0, "the anchor stays put");
        assert_eq!(dy, 100.0);
    }

    #[test]
    fn an_agent_paints_and_fills_through_the_automation_surface() {
        let (mut pane, pixels) = painting_pane();
        pane.agent_select_tool(3);
        pane.agent_set_brush(20.0, 1.0, 1.0, 2);
        pane.agent_paint_stroke(&[(100.0, 100.0), (160.0, 100.0)]);

        let frame = pane.store.get(pixels).expect("pixels");
        let centre = frame.pixels()[(100 * 300 + 130) * 4 + 3];
        assert_eq!(centre, 255, "the stroke painted its midpoint");

        // The rectangle selection lands where the document pixels said.
        pane.agent_select_rect(80.0, 80.0, 100.0, 40.0);
        let (sx, sy, sw, sh) = pane
            .agent_selection_bounds()
            .expect("the marquee left a selection");
        assert_eq!((sx, sy, sw, sh), (80, 80, 100, 40));

        pane.agent_delete_selection();
        let frame = pane.store.get(pixels).expect("pixels");
        let cleared = frame.pixels()[(100 * 300 + 130) * 4 + 3];
        assert_eq!(cleared, 0, "the delete cleared the stroked midpoint");
        let kept = frame.pixels()[(20 * 300 + 20) * 4 + 3];
        assert_eq!(kept, 0, "the pixels outside the selection stayed put");
    }

    #[test]
    fn an_agent_export_writes_a_decodable_png() {
        let (mut pane, _) = painting_pane();
        let dir = std::env::temp_dir().join("concat-agent-export");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent.png");
        pane.agent_export_png(&path).expect("the export wrote");

        let bytes = std::fs::read(&path).expect("the file");
        let (width, height, rgba) = decode_png(&bytes).expect("a decodable png");
        assert_eq!((width, height), (300, 200));
        assert_eq!(rgba.len(), 300 * 200 * 4);
        let _ = std::fs::remove_file(&path);

        let mut empty = CanvasPane::default();
        assert!(
            empty.agent_export_png(&path).is_err(),
            "nothing open, no export"
        );
    }
}
