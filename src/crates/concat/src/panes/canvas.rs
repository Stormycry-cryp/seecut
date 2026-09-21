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
    Adjustment, BrushSettings, BrushStroke, CanvasGpu, CanvasViewport, ImageDocument, LayerMask,
    LayerNode, Mask, NavInput, Navigator, PixelId, PixelStore, SelectionShape, erase_region,
    fill_region,
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

/// One row of the layers panel, front-to-back: identity, name, hidden,
/// opacity, active, nesting depth, whether the group's children are
/// shown, whether the row is a group at all, whether the row carries a
/// mask, and whether that mask is the one the painting tools are on. The
/// tuple the panel publishes and the agent reads.
pub type LayerRow = (u64, String, bool, f32, bool, usize, bool, bool, bool, bool);

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
    LayerAddGroup,
    /// Fold or unfold the group at the row - the panel's collapsed set.
    LayerFold(i32),
    LayerDelete(i32),
    LayerMove(i32, i32),
    /// A drag on the panel dropped the row `source` onto the row
    /// `target`: below it when `below` says so - which is into the
    /// target when the target is a group, the Photoshop drop-on-the-name
    /// read - and just above it otherwise.
    LayerDrop(i32, i32, bool),
    /// A white mask over the active node, the painting tools pointed at
    /// it.
    LayerMaskAdd,
    /// The mask chip on the row was clicked: pick the row and paint its
    /// mask - or, when the row is already the one being painted, stop.
    LayerMaskPaint(i32),
    /// The active gradient map's low (`0`) or high (`1`) colour, as an
    /// index into the tray's [`PALETTE`].
    GradientColor(i32, i32),
    /// The adjustment panel's picker: one of the kinds
    /// [`ADJUSTMENT_KINDS`] lists, added above the active layer so it
    /// colours everything beneath it.
    AdjustmentAdd(i32),
    /// One of the active adjustment's parameters, by its index in
    /// [`CanvasPane::adjustment_state`]'s list.
    AdjustmentParam(i32, f64),
    /// The active curves adjustment's channel (`0` red, `1` green, `2`
    /// blue): one control point moved, by its index, to a normalized
    /// position. The endpoints' inputs stay pinned.
    CurveSet(i32, i32, f64, f64),
    /// A new control point on the active curves adjustment's channel,
    /// inserted where the sorted input puts it.
    CurveAdd(i32, f64, f64),
    /// A control point off the active curves adjustment's channel, by its
    /// index; the endpoints refuse to leave.
    CurveRemove(i32, i32),
    /// Export the composed canvas as a PNG, through a save dialog.
    ExportPng,
    /// Save the whole document - tree, ids and pixels - as a `.comp`
    /// project package, through a save dialog.
    SaveComp,
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
    /// The transparency checkerboard at the document's own pixel size, one
    /// image per open document: the stage stretches it, so a square keeps
    /// its place in document space at any zoom.
    pub checker: slint::Image,
    /// The groups the panel has folded, by identity. Workspace state, not
    /// document state: a save carries the tree, not which boxes were shut.
    collapsed: std::collections::HashSet<concat_canvas::LayerId>,
    /// Whether the painting tools are on the active row's mask rather
    /// than its pixels. The target itself is [`CanvasPane::paint_target`]'s
    /// to say: a mode without a mask under it paints the pixels, so the
    /// flag can never strand a stroke.
    pub paint_mask: bool,
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
            checker: slint::Image::default(),
            collapsed: std::collections::HashSet::new(),
            paint_mask: false,
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
                // The id and the paint target come out before anything
                // moves: the rows borrow the tree, and the assignment
                // below writes through it.
                let picked = self
                    .rows()
                    .get(index.max(0) as usize)
                    .map(|(node, _)| (node.id(), node_image_pixels(node)));
                let Some((id, pixels)) = picked else {
                    return;
                };
                self.active = Some(id);
                // Painting lands on the picked layer when it can hold
                // pixels; groups and adjustments fall back to the base.
                self.layer = pixels;
                self.sync_view();
            }
            CanvasMsg::LayerToggleVisibility(index) => {
                let target = self
                    .rows()
                    .get(index.max(0) as usize)
                    .map(|(node, _)| node.id());
                let Some(id) = target else {
                    return;
                };
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                match document.find_mut(id) {
                    Some(LayerNode::Layer(layer)) => layer.hidden = !layer.hidden,
                    Some(LayerNode::Group(group)) => group.hidden = !group.hidden,
                    Some(LayerNode::Adjustment(adjustment)) => {
                        adjustment.hidden = !adjustment.hidden;
                    }
                    None => {}
                }
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerOpacity(index, opacity) => {
                let target = self
                    .rows()
                    .get(index.max(0) as usize)
                    .map(|(node, _)| node.id());
                let Some(id) = target else {
                    return;
                };
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                match document.find_mut(id) {
                    Some(LayerNode::Layer(layer)) => layer.opacity = opacity.clamp(0.0, 1.0),
                    Some(LayerNode::Group(group)) => group.opacity = opacity.clamp(0.0, 1.0),
                    Some(LayerNode::Adjustment(adjustment)) => {
                        adjustment.opacity = opacity.clamp(0.0, 1.0)
                    }
                    None => {}
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
            CanvasMsg::LayerAddGroup => {
                self.add_group();
                self.render(studio);
            }
            CanvasMsg::LayerFold(index) => {
                let fold = self.rows().get(index.max(0) as usize).and_then(|(node, _)| {
                    match node {
                        LayerNode::Group(group) => Some(group.id),
                        _ => None,
                    }
                });
                let Some(id) = fold else {
                    return;
                };
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerDelete(index) => {
                let target = self
                    .rows()
                    .get(index.max(0) as usize)
                    .map(|(node, _)| node.id());
                let Some(id) = target else {
                    return;
                };
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                document.remove(id);
                self.collapsed.remove(&id);
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
                let target = self
                    .rows()
                    .get(index.max(0) as usize)
                    .map(|(node, _)| node.id());
                let Some(id) = target else {
                    return;
                };
                // The node moves within whatever container holds it - the
                // root, or the group it sits in.
                self.move_within_container(id, direction);
                self.sync_view();
                self.render(studio);
            }
            CanvasMsg::LayerDrop(source, target, below) => {
                self.move_row_onto(source, target, below);
                self.render(studio);
            }
            CanvasMsg::LayerMaskAdd => {
                self.add_mask();
                self.render(studio);
            }
            CanvasMsg::LayerMaskPaint(index) => {
                self.mask_chip_click(index);
                self.sync_view();
            }
            CanvasMsg::GradientColor(slot, index) => {
                self.set_gradient_color(slot, index);
                self.render(studio);
            }
            CanvasMsg::AdjustmentAdd(kind) => {
                self.add_adjustment(kind);
                self.render(studio);
            }
            CanvasMsg::AdjustmentParam(index, value) => {
                self.set_adjustment_param(index, value as f32);
                self.render(studio);
            }
            CanvasMsg::CurveSet(channel, index, x, y) => {
                self.set_curve_point(channel, index, x, y);
                self.render(studio);
            }
            CanvasMsg::CurveAdd(channel, x, y) => {
                self.add_curve_point(channel, x, y);
                self.render(studio);
            }
            CanvasMsg::CurveRemove(channel, index) => {
                self.remove_curve_point(channel, index);
                self.render(studio);
            }
            CanvasMsg::ExportPng => self.export_png(studio),
            CanvasMsg::SaveComp => self.save_comp_dialog(studio),
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

    /// Fill or clear whatever is selected on the paint target - the
    /// active layer's pixels, or its mask while mask painting is up: one
    /// undo entry, one whole-rect re-upload, one recomposite.
    fn edit_selection(&mut self, kind: EditKind) {
        let Some(layer) = self.paint_target() else {
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
        let on_mask = self.is_mask_pixels(layer);
        self.undo_stack.push((layer, before));
        self.redo_stack.clear();
        self.store.replace(layer, frame.clone());
        if let Some(gpu) = &mut self.gpu {
            if on_mask {
                gpu.refresh_mask(layer, &frame);
            } else {
                gpu.upload(layer, &frame, None);
            }
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

    /// Starts a stroke: the paint target's pixels are snapshotted for the
    /// undo entry and for the tiles' base, a scratch frame is copied once,
    /// and the first dab goes down. Pixel work only; the caller renders.
    fn brush_press(&mut self, x: f64, y: f64) {
        let Some((w, h)) = self.document_size().map(|(w, h)| (w as u32, h as u32)) else {
            return;
        };
        let Some(layer) = self.paint_target() else {
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
    /// resident texture takes just those rectangles. A stroke on a mask
    /// keeps the store current and re-uploads the mask texture whole -
    /// masks sample their red channel straight off their own resident,
    /// and a per-tile upload has nothing to say to one.
    fn commit_tiles(&mut self, changed: &[(usize, usize)]) {
        if changed.is_empty() {
            return;
        }
        let Some(layer) = self.paint_target() else {
            return;
        };
        // Read before the scratch frame is borrowed out of the pane.
        let on_mask = self.painting_mask();
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
        if on_mask {
            // The store stays the mask's truth, and the mask texture
            // re-uploads whole; a mask has no tile residents to poke.
            self.store.replace(layer, scratch.clone());
            if let Some(gpu) = &mut self.gpu {
                gpu.refresh_mask(layer, scratch);
            }
            return;
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
    /// undo's single frame can afford. A mask entry re-uploads as a mask
    /// - the two residents are keyed apart, and the wrong one goes unseen.
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
        let on_mask = self.is_mask_pixels(layer);
        self.store.replace(layer, (*before).clone());
        if let Some(gpu) = &mut self.gpu {
            if on_mask {
                gpu.refresh_mask(layer, &before);
            } else {
                gpu.upload(layer, &before, None);
            }
        }
    }

    /// Steps the pixel history forward one undone edit. Masks upload as
    /// masks, for the same reason [`CanvasPane::undo`] gives.
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
        let on_mask = self.is_mask_pixels(layer);
        self.store.replace(layer, (*after).clone());
        if let Some(gpu) = &mut self.gpu {
            if on_mask {
                gpu.refresh_mask(layer, &after);
            } else {
                gpu.upload(layer, &after, None);
            }
        }
    }

    /// The rows the panel shows, front-to-back: the tree walked depth-first
    /// and reversed, with the folded groups' descendants left out. A row
    /// carries its depth, so the panel can indent. Every panel index -
    /// pick, visibility, opacity, fold, move, delete - is an index into
    /// this list, which is what keeps a panel row and a tree node the same
    /// thing at any depth.
    fn rows(&self) -> Vec<(&LayerNode, usize)> {
        fn walk_group<'a>(
            group: &'a concat_canvas::LayerGroup,
            depth: usize,
            collapsed: &std::collections::HashSet<concat_canvas::LayerId>,
            out: &mut Vec<(&'a LayerNode, usize)>,
        ) {
            for node in group.children.iter().rev() {
                out.push((node, depth));
                if let LayerNode::Group(child) = node
                    && !collapsed.contains(&child.id)
                {
                    walk_group(child, depth + 1, collapsed, out);
                }
            }
        }
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        walk_group(&document.root, 0, &self.collapsed, &mut out);
        out
    }

    /// The node `id` moves within its own container - up when
    /// `direction` is one, down when it is minus one - the panel's
    /// front-to-back flipped into the children's back-to-front. A move
    /// past either end stays put.
    fn move_within_container(&mut self, id: concat_canvas::LayerId, direction: i32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if let Some(children) = children_holding(&mut document.root, id)
            && let Some(from) = children.iter().position(|c| c.id() == id)
        {
            let to = (from as i64 - i64::from(direction)).max(0) as usize;
            if to < children.len() {
                let moved = children.remove(from);
                children.insert(to, moved);
            }
        }
    }

    /// A panel drag landed the row `source` on the row `target`. Below a
    /// group means into it, at the back of its children - its front in
    /// the panel, where a freshly adopted layer reads first; below a
    /// layer, or above anything, means a sibling slot beside the target.
    /// A group never lands inside its own subtree, and a drop on the
    /// moving row itself is a no-op, not a shuffle.
    fn move_row_onto(&mut self, source: i32, target: i32, below: bool) {
        let rows = self.rows();
        let moving = rows.get(source.max(0) as usize).map(|(node, _)| node.id());
        let Some(target_row) = rows.get(target.max(0) as usize) else {
            return;
        };
        let target_id = target_row.0.id();
        let into = below && matches!(target_row.0, LayerNode::Group(_));
        let Some(moving) = moving else {
            return;
        };
        if target_id == moving {
            return;
        }
        // The target cannot live inside the moved subtree: for the into
        // case `move_node` refuses, and for the sibling case the target
        // would vanish with the take, so both are refused up front.
        let inside_moved = matches!(
            self.document.as_ref().and_then(|d| d.find(moving)),
            Some(LayerNode::Group(group)) if group.find(target_id).is_some()
        );
        if inside_moved {
            return;
        }
        drop(rows);
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if into {
            let index = document
                .group_mut(target_id)
                .map(|group| group.children.len())
                .unwrap_or(0);
            document.move_node(moving, Some(target_id), index);
        } else {
            // Take the mover out first, then find the target's slot
            // again - the take shifts the very slot when both share a
            // container - and slide the mover in above or below it.
            let Some(node) = document.take_node(moving) else {
                return;
            };
            match locate_node(&mut document.root, target_id) {
                Some((children, position)) => {
                    let index = if below { position } else { position + 1 };
                    children.insert(index.min(children.len()), node);
                }
                // Unreachable after the refusal above; the mover goes
                // back to the root rather than being dropped.
                None => document.root.children.push(node),
            }
        }
        self.sync_view();
    }

    /// The pixels the painting tools land on: the active row's mask
    /// while mask painting is up and the row has one, the picked layer's
    /// pixels otherwise. A mode without a mask under it falls through,
    /// so a stroke is never lost.
    fn paint_target(&self) -> Option<PixelId> {
        if self.paint_mask
            && let Some(document) = self.document.as_ref()
            && let Some(node) = self.active.and_then(|id| document.find(id))
            && let Some(mask) = node.mask()
        {
            return Some(mask.pixels);
        }
        self.layer
    }

    /// Whether a stroke right now would land on a mask rather than on a
    /// layer's own pixels. [`CanvasPane::paint_target`] picks the pixels;
    /// this picks the plumbing - masks re-upload whole.
    fn painting_mask(&self) -> bool {
        self.paint_mask && self.paint_target() != self.layer
    }

    /// Whether the pixel id names a mask anywhere in the open document.
    fn is_mask_pixels(&self, id: PixelId) -> bool {
        let Some(document) = self.document.as_ref() else {
            return false;
        };
        document
            .walk()
            .iter()
            .any(|node| node.mask().map(|mask| mask.pixels) == Some(id))
    }

    /// A white mask over the active node - show everything, paint it
    /// down from there - sized to the canvas, and the painting tools
    /// pointed at it. A node that has a mask already takes no second.
    fn add_mask(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if document.find(id).and_then(|node| node.mask()).is_some() {
            return;
        }
        let pixels = vec![255u8; Frame::byte_len(document.width, document.height)];
        let frame = Frame::from_rgba(document.width, document.height, pixels)
            .expect("the mask is sized to the canvas");
        let mask_id = self.store.put(frame);
        let document = self.document.as_mut().expect("checked above");
        match document.find_mut(id) {
            Some(LayerNode::Layer(layer)) => layer.mask = Some(LayerMask::new(mask_id)),
            Some(LayerNode::Group(group)) => group.mask = Some(LayerMask::new(mask_id)),
            Some(LayerNode::Adjustment(adjustment)) => {
                adjustment.mask = Some(LayerMask::new(mask_id));
            }
            None => {
                // Nothing names the mask; the store does not keep it.
                self.store.retain_document(document);
            }
        }
        self.paint_mask = true;
        self.sync_view();
    }

    /// The active node's mask, gone. The mode falls with it, so the
    /// tools are never left pointing at pixels nothing names.
    fn remove_mask(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        match document.find_mut(id) {
            Some(LayerNode::Layer(layer)) => layer.mask = None,
            Some(LayerNode::Group(group)) => group.mask = None,
            Some(LayerNode::Adjustment(adjustment)) => adjustment.mask = None,
            None => return,
        }
        self.paint_mask = false;
        self.store.retain_document(document);
        self.sync_view();
    }

    /// The active node's mask applied or set aside, whole - the mask
    /// itself and everything painted into it stays.
    fn toggle_mask(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let found = match document.find_mut(id) {
            Some(LayerNode::Layer(layer)) => {
                if let Some(mask) = &mut layer.mask {
                    mask.enabled = !mask.enabled;
                    true
                } else {
                    false
                }
            }
            Some(LayerNode::Group(group)) => {
                if let Some(mask) = &mut group.mask {
                    mask.enabled = !mask.enabled;
                    true
                } else {
                    false
                }
            }
            Some(LayerNode::Adjustment(adjustment)) => {
                if let Some(mask) = &mut adjustment.mask {
                    mask.enabled = !mask.enabled;
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if found {
            self.sync_view();
        }
    }

    /// The mask chip on a row was clicked: pick the row, and either
    /// start painting its mask or - when this very row was already the
    /// one being painted - stop.
    fn mask_chip_click(&mut self, index: i32) {
        let picked = self
            .rows()
            .get(index.max(0) as usize)
            .map(|(node, _)| (node.id(), node_image_pixels(node), node.mask().is_some()));
        let Some((id, pixels, masked)) = picked else {
            return;
        };
        if !masked {
            return;
        }
        if self.paint_mask && self.active == Some(id) {
            self.paint_mask = false;
            return;
        }
        self.active = Some(id);
        self.layer = pixels;
        self.paint_mask = true;
        self.sync_view();
    }

    /// The active gradient map's colours, as the panel publishes them:
    /// `(low, high)`, each RGB in `0..1`. `None` when the row is not a
    /// gradient map.
    pub fn gradient_colors(&self) -> Option<([f32; 3], [f32; 3])> {
        let document = self.document.as_ref()?;
        let node = self.active.and_then(|id| document.find(id))?;
        let LayerNode::Adjustment(adjustment) = node else {
            return None;
        };
        match &adjustment.adjustment {
            Adjustment::GradientMap { low, high } => Some((*low, *high)),
            _ => None,
        }
    }

    /// Sets the active gradient map's low (`0`) or high (`1`) colour
    /// from the tray palette's `index`. Any other kind of row, or an
    /// index past the palette, is a no-op.
    fn set_gradient_color(&mut self, slot: i32, index: i32) {
        let Some(color) = PALETTE.get(index.max(0) as usize) else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let Some(id) = self.active else {
            return;
        };
        let Some(LayerNode::Adjustment(adjustment)) = document.find_mut(id) else {
            return;
        };
        if let Adjustment::GradientMap { low, high } = &mut adjustment.adjustment {
            // The palette speaks 0..255 bytes; the adjustment speaks
            // 0..1 floats. The same colour either way.
            let rgb = [
                f32::from(color[0]) / 255.0,
                f32::from(color[1]) / 255.0,
                f32::from(color[2]) / 255.0,
            ];
            if slot == 0 {
                *low = rgb;
            } else {
                *high = rgb;
            }
        }
    }

    /// The layers panel's rows, front-to-back: identity, name, visibility,
    /// opacity, whether the row is the active one, its nesting depth,
    /// whether the group's children are shown, whether the row is a group
    /// at all, whether it carries a mask, and whether that mask is the
    /// one being painted. Folded groups still show their own row.
    pub fn layers_data(&self) -> Vec<LayerRow> {
        self.rows()
            .iter()
            .map(|(node, depth)| {
                let expanded = match node {
                    LayerNode::Group(group) => !self.collapsed.contains(&group.id),
                    _ => false,
                };
                let active = self.active == Some(node.id());
                (
                    node.id().as_u64(),
                    node.name().to_owned(),
                    node.hidden(),
                    node.opacity(),
                    active,
                    *depth,
                    expanded,
                    matches!(node, LayerNode::Group(_)),
                    node.mask().is_some(),
                    self.paint_mask && active,
                )
            })
            .collect()
    }

    /// The active node's adjustment, as the panel publishes it: its kind
    /// ([`ADJUSTMENT_KINDS`]' numbering, `0` when the row is not an
    /// adjustment) and its editable parameters as `(label, value, minimum,
    /// maximum)` triples in a fixed order.
    pub fn adjustment_state(&self) -> (i32, Vec<(String, f32, f32, f32)>) {
        let Some(document) = self.document.as_ref() else {
            return (0, Vec::new());
        };
        let Some(node) = self.active.and_then(|id| document.find(id)) else {
            return (0, Vec::new());
        };
        let LayerNode::Adjustment(adjustment) = node else {
            return (0, Vec::new());
        };
        let kind = adjustment_kind(&adjustment.adjustment);
        (kind, adjustment_parameters(&adjustment.adjustment))
    }

    /// Adds an adjustment of `kind` above the active layer, so it colours
    /// everything beneath it - the whole point of the placement. A new
    /// adjustment becomes the active row; there is nothing to paint on it,
    /// so the paint target falls back to the topmost image layer.
    fn add_adjustment(&mut self, kind: i32) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        // The insertion index: just above the active node in the children,
        // which is above it in the panel too. A nested or absent active
        // row lands the adjustment at the top of the root.
        let index = self
            .active
            .and_then(|id| {
                document
                    .root
                    .children
                    .iter()
                    .position(|child| child.id() == id)
            })
            .map(|position| position + 1);
        let name = adjustment_label(kind);
        let Some(adjustment) = default_adjustment(kind) else {
            return;
        };
        let document = self.document.as_mut().expect("checked above");
        let id = document.new_adjustment(name, adjustment);
        // `new_adjustment` appended; move it into place now that we can.
        if let Some(index) = index {
            let from = document
                .root
                .children
                .iter()
                .position(|child| child.id() == id)
                .expect("just appended");
            let node = document.root.children.remove(from);
            let to = index.min(document.root.children.len());
            document.root.children.insert(to, node);
        }
        self.active = Some(id);
        // Adjustments hold no pixels; the tools keep working on the
        // topmost image layer beneath them.
        self.layer = document
            .root
            .children
            .iter()
            .rev()
            .find_map(node_image_pixels);
        self.sync_view();
    }

    /// Sets the active adjustment's parameter `index` (the order
    /// [`adjustment_parameters`] lists them in) to `value`, clamped into
    /// the parameter's own range.
    fn set_adjustment_param(&mut self, index: i32, value: f32) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let Some(id) = self.active else {
            return;
        };
        let Some(LayerNode::Adjustment(adjustment)) = document.find_mut(id) else {
            return;
        };
        let parameters = adjustment_parameters(&adjustment.adjustment);
        let Some((_, _, minimum, maximum)) = parameters.get(index.max(0) as usize) else {
            return;
        };
        let value = value.clamp(*minimum, *maximum);
        match &mut adjustment.adjustment {
            Adjustment::Exposure { stops } => {
                if index == 0 {
                    *stops = value;
                }
            }
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => match index {
                0 => *in_black = value,
                1 => *in_white = value,
                2 => *gamma = value,
                3 => *out_black = value,
                4 => *out_white = value,
                _ => {}
            },
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => match index {
                0 => *hue = value,
                1 => *saturation = value,
                2 => *lightness = value,
                _ => {}
            },
            Adjustment::Grain { amount } => {
                if index == 0 {
                    *amount = value;
                }
            }
            // The kinds with no numeric parameters take no edits.
            Adjustment::Invert | Adjustment::GradientMap { .. } | Adjustment::Curves { .. } => {}
        }
    }

    /// Adds a group above the active node, the way a new layer lands:
    /// the panel's "new group" is an insertion relative to what is
    /// picked, and a nested or absent pick lands the group at the top of
    /// the root. A group holds nothing until layers move into it, and
    /// paints nothing, so the paint target falls back to the topmost
    /// image layer.
    fn add_group(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let count = document.walk().len();
        let index = self
            .active
            .and_then(|id| {
                document
                    .root
                    .children
                    .iter()
                    .position(|child| child.id() == id)
            })
            .map(|position| position + 1);
        let name = tf("Group {0}", &[&count.to_string()]).to_string();
        let document = self.document.as_mut().expect("checked above");
        let id = document.new_group(name);
        if let Some(index) = index {
            let from = document
                .root
                .children
                .iter()
                .position(|child| child.id() == id)
                .expect("just appended");
            let node = document.root.children.remove(from);
            let to = index.min(document.root.children.len());
            document.root.children.insert(to, node);
        }
        self.active = Some(id);
        self.layer = document
            .root
            .children
            .iter()
            .rev()
            .find_map(node_image_pixels);
        self.sync_view();
    }

    /// The active curves adjustment's channels, as the editor publishes
    /// them: red, green, blue, each the channel's control points as
    /// normalized `(input, output)` pairs sorted by input. Anything else
    /// on the active row publishes an empty list.
    pub fn curve_channels(&self) -> Vec<Vec<(f32, f32)>> {
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        let Some(node) = self.active.and_then(|id| document.find(id)) else {
            return Vec::new();
        };
        let LayerNode::Adjustment(adjustment) = node else {
            return Vec::new();
        };
        let Adjustment::Curves { red, green, blue } = &adjustment.adjustment else {
            return Vec::new();
        };
        vec![red.clone(), green.clone(), blue.clone()]
    }

    /// The active curves adjustment's channel, `0` red, `1` green, `2`
    /// blue, as the mutable point list.
    fn curve_channel_mut(&mut self, channel: i32) -> Option<&mut Vec<(f32, f32)>> {
        let document = self.document.as_mut()?;
        let id = self.active?;
        let LayerNode::Adjustment(adjustment) = document.find_mut(id)? else {
            return None;
        };
        let Adjustment::Curves { red, green, blue } = &mut adjustment.adjustment else {
            return None;
        };
        Some(match channel {
            0 => red,
            1 => green,
            _ => blue,
        })
    }

    /// Moves the channel's control point `index` to `(x, y)`, both in
    /// `0..1`. The endpoints keep their places at the corners and give
    /// only their output; a middle point keeps the sorted input order,
    /// stopping just short of crossing a neighbour.
    fn set_curve_point(&mut self, channel: i32, index: i32, x: f64, y: f64) {
        let Some(points) = self.curve_channel_mut(channel) else {
            return;
        };
        let index = index.max(0) as usize;
        if index >= points.len() {
            return;
        }
        let y = (y as f32).clamp(0.0, 1.0);
        if index == 0 {
            points[0].1 = y;
            return;
        }
        if index == points.len() - 1 {
            points[index].1 = y;
            return;
        }
        let x = (x as f32).clamp(0.0, 1.0);
        let low = points[index - 1].0 + 0.001;
        let high = points[index + 1].0 - 0.001;
        points[index].0 = x.clamp(low, high);
        points[index].1 = y;
    }

    /// Inserts a control point on the channel at `(x, y)`, where the
    /// sorted input puts it. Sixteen points to a channel: a curve that
    /// needs more wants a different tool.
    fn add_curve_point(&mut self, channel: i32, x: f64, y: f64) {
        let Some(points) = self.curve_channel_mut(channel) else {
            return;
        };
        if points.len() >= 16 {
            return;
        }
        let x = (x as f32).clamp(0.0, 1.0);
        let y = (y as f32).clamp(0.0, 1.0);
        let at = points.partition_point(|(input, _)| *input < x);
        points.insert(at, (x, y));
    }

    /// Drops the channel's control point `index`. The two corners stay,
    /// so a curve is never fewer than the identity it starts as.
    fn remove_curve_point(&mut self, channel: i32, index: i32) {
        let Some(points) = self.curve_channel_mut(channel) else {
            return;
        };
        let index = index.max(0) as usize;
        // The length check leads: a short or empty channel - a hand off a
        // malformed project file can make one - refuses before the
        // endpoint arithmetic runs.
        if points.len() < 3 || index == 0 || index >= points.len() - 1 {
            return;
        }
        points.remove(index);
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

    /// Saves the open document as a `.comp` project package, through a
    /// save dialog seeded with the open file's stem.
    fn save_comp_dialog(&mut self, studio: &mut Studio) {
        if self.document.is_none() {
            return;
        }
        let stem = match self.name.rsplit_once('.') {
            Some((stem, _)) => stem.to_owned(),
            None => self.name.clone(),
        };
        let Some(path) = crate::platform::save_file(
            &tf("Save project", &[]),
            &format!("{stem}.comp"),
            Some(("Concat project", &["comp"])),
        ) else {
            return;
        };
        if let Err(error) = self.save_comp(&path) {
            log::warn!("canvas: {error}");
            studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
        }
    }

    /// Writes the document as a `.comp` package: `manifest.json` - the
    /// format version, the canvas box, and the document tree exactly as
    /// serde sees it - beside `images/<pixel id>.png` for every bitmap the
    /// tree still names, layers and masks alike. The package is staged in
    /// a sibling temporary directory and swapped in, so a failed save
    /// leaves the previous save untouched.
    fn save_comp(&self, path: &Path) -> Result<(), String> {
        let Some(document) = self.document.as_ref() else {
            return Err(tf("No image open", &[]));
        };
        // Every bitmap the document names, layers and masks at any depth.
        let mut used = Vec::new();
        document.collect_pixels(&mut used);
        used.sort();
        used.dedup();

        let staging = sibling_temp(path);
        std::fs::create_dir_all(staging.join("images"))
            .map_err(|e| format!("save: {e}"))?;
        for id in &used {
            let Some(frame) = self.store.get(*id) else {
                std::fs::remove_dir_all(&staging).ok();
                return Err(format!("save: pixels {id:?} are gone"));
            };
            let bytes = encode_png(&frame)?;
            std::fs::write(
                staging.join("images").join(format!("{}.png", id.as_u64())),
                bytes,
            )
            .map_err(|e| format!("save: {e}"))?;
        }
        let manifest = serde_json::json!({
            "concat-project": 1,
            "width": document.width,
            "height": document.height,
            "document": document,
        })
        .to_string();
        std::fs::write(staging.join("manifest.json"), manifest)
            .map_err(|e| format!("save: {e}"))?;

        // The swap: the staging takes the target's place, and the previous
        // save is only dropped once the new one is fully in place.
        replace_package(&staging, path)?;
        Ok(())
    }

    /// Reads a `.comp` package back: the manifest's document tree keeps
    /// its ids, every bitmap it names is decoded and restored under the
    /// same id, and the tree is validated before anything is shown. The
    /// returned pixels were never re-minted, so a save of the re-opened
    /// document is byte-for-byte the same tree again.
    fn load_comp(&mut self, path: &Path) -> Result<(), String> {
        let manifest_bytes = std::fs::read(path.join("manifest.json"))
            .map_err(|e| format!("open: {e}"))?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).map_err(|e| format!("open: {e}"))?;
        if manifest.get("concat-project").and_then(|v| v.as_u64()) != Some(1) {
            return Err("open: not a concat project".into());
        }
        let document: ImageDocument =
            serde_json::from_value(manifest.get("document").cloned().ok_or("open: no document")?)
                .map_err(|e| format!("open: {e}"))?;
        if document.width == 0 || document.height == 0 {
            return Err("open: empty canvas".into());
        }
        document.validate().map_err(|e| format!("open: {e}"))?;

        let mut used = Vec::new();
        document.collect_pixels(&mut used);
        used.sort();
        used.dedup();
        let mut store = PixelStore::new();
        for id in &used {
            let file = path.join("images").join(format!("{}.png", id.as_u64()));
            let bytes = std::fs::read(&file).map_err(|e| format!("open: {e}"))?;
            let (width, height, rgba) = decode_png(&bytes)?;
            let frame = Frame::from_rgba(width, height, rgba).ok_or("open: empty image")?;
            store.restore(*id, frame);
        }

        self.document = Some(document);
        self.store = store;
        // The active row: the topmost image layer, as a fresh open starts.
        let topmost = self
            .document
            .as_ref()
            .expect("just set")
            .root
            .children
            .iter()
            .rev()
            .find(|node| matches!(node, LayerNode::Layer(_)));
        self.active = topmost.map(LayerNode::id);
        self.layer = topmost.and_then(node_image_pixels);
        self.selection = None;
        self.marquee = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.failed = false;
        let (width, height) = self
            .document
            .as_ref()
            .map(|d| (d.width, d.height))
            .expect("just set");
        self.checker = checker_image(width, height);
        let size = (f64::from(width), f64::from(height));
        self.nav.set_document(Some(size));
        self.nav.viewport_mut().fit(size);
        self.sync_view();
        Ok(())
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

    /// Opens a path: a `.comp` project package loads as the document it
    /// saved; anything else decodes as one image, one layer the size of
    /// the canvas, the view fitted to it.
    fn open(&mut self, path: &Path, studio: &mut Studio) {
        if path.is_dir() {
            match self.load_comp(path) {
                Ok(()) => {
                    self.failed = false;
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
            return;
        }
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
                self.checker = checker_image(width, height);
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
        let picked = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| (node.id(), node_image_pixels(node)));
        let Some((id, pixels)) = picked else {
            return;
        };
        self.active = Some(id);
        self.layer = pixels;
        self.sync_view();
    }

    pub fn agent_layer_toggle_visibility(&mut self, row: i32) {
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        let Some(id) = target else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
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

    pub fn agent_layer_opacity(&mut self, row: i32, opacity: f32) {
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        let Some(id) = target else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        match document.find_mut(id) {
            Some(LayerNode::Layer(layer)) => layer.opacity = opacity.clamp(0.0, 1.0),
            Some(LayerNode::Group(group)) => group.opacity = opacity.clamp(0.0, 1.0),
            Some(LayerNode::Adjustment(adjustment)) => {
                adjustment.opacity = opacity.clamp(0.0, 1.0);
            }
            None => {}
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
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        let Some(id) = target else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        document.remove(id);
        self.collapsed.remove(&id);
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
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        let Some(id) = target else {
            return;
        };
        self.move_within_container(id, direction);
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
    /// shows, and the row indexes the layer methods take: identity, name,
    /// visibility, opacity, active, nesting depth, whether the group's
    /// children are shown, whether the row is a group at all, whether it
    /// carries a mask, and whether that mask is the one being painted.
    pub fn agent_layers(&self) -> Vec<LayerRow> {
        self.layers_data()
    }

    /// Adds a group above the active row - the panel's "new group"
    /// without the panel.
    pub fn agent_layer_group(&mut self) {
        self.add_group();
    }

    /// Moves the node at `row` into the group at `into`, appended at the
    /// back of that group's children (its front in the panel). `into` of
    /// `None` sends the node back to the root. The engine's own
    /// `move_node` does the walking; the row indexes are the panel's.
    pub fn agent_layer_move_into(&mut self, row: i32, into: Option<i32>) {
        // Both row lookups run off one borrowed listing, and both facts
        // come out as owned ids before the tree is touched.
        let rows = self.rows();
        let moving = rows.get(row.max(0) as usize).map(|(node, _)| node.id());
        let target = into
            .and_then(|group_row| rows.get(group_row.max(0) as usize))
            .and_then(|(node, _)| match node {
                LayerNode::Group(group) => Some(group.id),
                _ => None,
            });
        drop(rows);
        let Some(moving) = moving else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        // The append index: the back of the target's children, which is
        // its front in the panel. An empty target takes the node at 0.
        let index = match target {
            Some(into) => document
                .group_mut(into)
                .map(|group| group.children.len())
                .unwrap_or(0),
            None => document.root.children.len(),
        };
        document.move_node(moving, target, index);
        self.sync_view();
    }

    /// The panel drag, without the panel: the row `source` dropped onto
    /// the row `target`, into the target when `below` says so and the
    /// target is a group, beside it otherwise. The same resolution the
    /// gesture's drop gets.
    pub fn agent_layer_drop(&mut self, source: i32, target: i32, below: bool) {
        self.move_row_onto(source, target, below);
    }

    /// A white mask over the row's node - the panel's "add mask" without
    /// the panel - and the painting tools pointed at it.
    pub fn agent_layer_mask_add(&mut self, row: i32) {
        let picked = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| (node.id(), node_image_pixels(node)));
        if let Some((id, pixels)) = picked {
            self.active = Some(id);
            self.layer = pixels;
            self.add_mask();
        }
    }

    /// The row's node's mask, gone.
    pub fn agent_layer_mask_remove(&mut self, row: i32) {
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        if let Some(id) = target {
            self.active = Some(id);
            self.remove_mask();
        }
    }

    /// The row's node's mask applied or set aside, whole.
    pub fn agent_layer_mask_toggle(&mut self, row: i32) {
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        if let Some(id) = target {
            self.active = Some(id);
            self.toggle_mask();
        }
    }

    /// Whether the painting tools are on the active row's mask - the
    /// mode the mask chip toggles.
    pub fn agent_paint_mask(&self) -> bool {
        self.paint_mask
    }

    /// Sets the active gradient map's low (`0`) or high (`1`) colour
    /// from the tray palette's `index`.
    pub fn agent_gradient_color(&mut self, slot: i32, index: i32) {
        self.set_gradient_color(slot, index);
    }

    /// Folds or unfolds the group at `row`, the way the panel's chevron
    /// does. A row that is not a group does nothing.
    pub fn agent_layer_fold(&mut self, row: i32) {
        let fold = self.rows().get(row.max(0) as usize).and_then(|(node, _)| {
            match node {
                LayerNode::Group(group) => Some(group.id),
                _ => None,
            }
        });
        let Some(id) = fold else {
            return;
        };
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.sync_view();
    }

    /// The active row's curves channels, as the editor reads them:
    /// `[red, green, blue]`, each the sorted `(input, output)` points in
    /// `0..1`. Empty when the row is not a curves adjustment.
    pub fn agent_curve_channels(&self) -> Vec<Vec<(f32, f32)>> {
        self.curve_channels()
    }

    /// Moves the curves control point `index` on `channel` to `(x, y)`.
    pub fn agent_curve_set(&mut self, channel: i32, index: i32, x: f64, y: f64) {
        self.set_curve_point(channel, index, x, y);
    }

    /// Inserts a curves control point on `channel` at `(x, y)`.
    pub fn agent_curve_add(&mut self, channel: i32, x: f64, y: f64) {
        self.add_curve_point(channel, x, y);
    }

    /// Drops the curves control point `index` on `channel`; the corners
    /// refuse to leave.
    pub fn agent_curve_remove(&mut self, channel: i32, index: i32) {
        self.remove_curve_point(channel, index);
    }

    /// The brush as it is set right now.
    pub fn agent_brush(&self) -> concat_canvas::BrushSettings {
        self.brush
    }

    /// Adds an adjustment of `kind` above the active layer - the kinds
    /// [`ADJUSTMENT_KINDS`] numbers, `1` Invert through `7` Curves - and
    /// makes it the active row.
    pub fn agent_adjustment_add(&mut self, kind: i32) {
        self.add_adjustment(kind);
    }

    /// Sets the active adjustment's parameter `index` to `value`, the
    /// order [`Self::adjustment_state`] publishes them in.
    pub fn agent_adjustment_param(&mut self, index: i32, value: f32) {
        self.set_adjustment_param(index, value);
    }

    /// The active row's adjustment: `(kind, parameters)` exactly as the
    /// panel reads it, `(0, [])` when the row is not an adjustment.
    pub fn agent_adjustment_state(&self) -> (i32, Vec<(String, f32, f32, f32)>) {
        self.adjustment_state()
    }

    /// The kinds the popup offers, in order - the contract an agent needs
    /// to offer the same menu the panel does.
    pub fn agent_adjustment_kinds(&self) -> Vec<(i32, &'static str)> {
        ADJUSTMENT_KINDS.to_vec()
    }

    /// Writes the open document to `path` as a `.comp` project package -
    /// the save without the dialog.
    pub fn agent_save_comp(&mut self, path: &Path) -> Result<(), String> {
        self.save_comp(path)
    }

    /// Opens a `.comp` project package, or an image, from `path` - the
    /// open without the dialog.
    pub fn agent_open(&mut self, path: &Path) -> Result<(), String> {
        if path.is_dir() {
            self.load_comp(path)
        } else {
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
                    self.checker = checker_image(width, height);
                    let size = (f64::from(width), f64::from(height));
                    self.nav.set_document(Some(size));
                    self.nav.viewport_mut().fit(size);
                    self.sync_view();
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
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

/// The adjustment kinds the panel offers, as the boundary numbers them:
/// `1` Invert, `2` Exposure, `3` Levels, `4` Hue & Saturation, `5`
/// Gradient map, `6` Grain. `0` means "not an adjustment"; `7` is Curves,
/// which the document round-trips but the popup does not yet offer, a
/// curve wanting handles rather than knobs.
const ADJUSTMENT_KINDS: &[(i32, &str)] = &[
    (1, "Invert"),
    (2, "Exposure"),
    (3, "Levels"),
    (4, "Hue & Saturation"),
    (5, "Gradient map"),
    (6, "Grain"),
    (7, "Curves"),
];

/// One drawn stroke of a curves polyline, as the editor draws it: both
/// endpoints in normalized editor coordinates, with the output axis
/// already flipped to the screen's downward one. The editor is square,
/// so normalized numbers place a stroke at any size it draws at.
pub struct CurveSegment {
    pub x: f32,
    pub y: f32,
    pub x2: f32,
    pub y2: f32,
}

/// The polyline's strokes, one per consecutive pair of control points.
pub fn curve_segments(points: &[(f32, f32)]) -> Vec<CurveSegment> {
    points
        .windows(2)
        .map(|pair| {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            CurveSegment {
                x: x0,
                y: 1.0 - y0,
                x2: x1,
                y2: 1.0 - y1,
            }
        })
        .collect()
}

/// The children vector that holds `id`, at any depth: the root's own, or
/// the group the node sits in. A first immutable pass picks the branch,
/// so the mutable descent re-borrows cleanly.
fn children_holding(
    group: &mut concat_canvas::LayerGroup,
    id: concat_canvas::LayerId,
) -> Option<&mut Vec<LayerNode>> {
    if group.children.iter().any(|child| child.id() == id) {
        return Some(&mut group.children);
    }
    let branch = group.children.iter().position(|child| match child {
        LayerNode::Group(nested) => nested.find(id).is_some(),
        _ => false,
    })?;
    match &mut group.children[branch] {
        LayerNode::Group(nested) => children_holding(nested, id),
        _ => None,
    }
}

/// The children vector that holds `id`, with the id's slot in it - the
/// tree walk a sibling-slot insertion needs. The root group included,
/// since the root's children are the panel's flat rows.
fn locate_node(
    group: &mut concat_canvas::LayerGroup,
    id: concat_canvas::LayerId,
) -> Option<(&mut Vec<LayerNode>, usize)> {
    if let Some(position) = group.children.iter().position(|child| child.id() == id) {
        return Some((&mut group.children, position));
    }
    let branch = group
        .children
        .iter()
        .position(|child| matches!(child, LayerNode::Group(nested) if nested.find(id).is_some()))?;
    match &mut group.children[branch] {
        LayerNode::Group(nested) => locate_node(nested, id),
        _ => None,
    }
}

/// The kind number of an adjustment, as the panel publishes it.
fn adjustment_kind(adjustment: &Adjustment) -> i32 {
    match adjustment {
        Adjustment::Invert => 1,
        Adjustment::Exposure { .. } => 2,
        Adjustment::Levels { .. } => 3,
        Adjustment::HueSaturation { .. } => 4,
        Adjustment::GradientMap { .. } => 5,
        Adjustment::Grain { .. } => 6,
        Adjustment::Curves { .. } => 7,
    }
}

/// The adjustment kind's display name, localized.
fn adjustment_label(kind: i32) -> String {
    let key = ADJUSTMENT_KINDS
        .iter()
        .find(|(number, _)| *number == kind)
        .map(|(_, key)| *key)
        .unwrap_or("Adjustments");
    tf(key, &[])
}

/// A fresh adjustment of `kind`, its parameters at their neutral values -
/// the ones that change nothing until a knob moves.
fn default_adjustment(kind: i32) -> Option<Adjustment> {
    match kind {
        1 => Some(Adjustment::Invert),
        2 => Some(Adjustment::Exposure { stops: 0.0 }),
        3 => Some(Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }),
        4 => Some(Adjustment::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        }),
        5 => Some(Adjustment::GradientMap {
            low: [0.0, 0.0, 0.0],
            high: [1.0, 1.0, 1.0],
        }),
        6 => Some(Adjustment::Grain { amount: 0.0 }),
        7 => Some(Adjustment::Curves {
            red: vec![(0.0, 0.0), (1.0, 1.0)],
            green: vec![(0.0, 0.0), (1.0, 1.0)],
            blue: vec![(0.0, 0.0), (1.0, 1.0)],
        }),
        _ => None,
    }
}

/// The adjustment's editable parameters, in the fixed order an index
/// means: `(label, value, minimum, maximum)`. The kinds a slider cannot
/// express - a gradient map's two colours, a curve's points - publish
/// nothing.
fn adjustment_parameters(adjustment: &Adjustment) -> Vec<(String, f32, f32, f32)> {
    match adjustment {
        Adjustment::Invert | Adjustment::GradientMap { .. } | Adjustment::Curves { .. } => {
            Vec::new()
        }
        Adjustment::Exposure { stops } => {
            vec![(tf("Stops", &[]), *stops, -4.0, 4.0)]
        }
        Adjustment::Levels {
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
        } => vec![
            (tf("In black", &[]), *in_black, 0.0, 1.0),
            (tf("In white", &[]), *in_white, 0.0, 1.0),
            (tf("Gamma", &[]), *gamma, 0.1, 3.0),
            (tf("Out black", &[]), *out_black, 0.0, 1.0),
            (tf("Out white", &[]), *out_white, 0.0, 1.0),
        ],
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
        } => vec![
            (tf("Hue", &[]), *hue, -180.0, 180.0),
            (tf("Saturation", &[]), *saturation, -1.0, 1.0),
            (tf("Lightness", &[]), *lightness, -1.0, 1.0),
        ],
        Adjustment::Grain { amount } => {
            vec![(tf("Amount", &[]), *amount, 0.0, 1.0)]
        }
    }
}

/// The transparency checkerboard, at the document's own pixel size: 16
/// document pixels to a square, light and dark, `(0, 0)` light. The stage
/// stretches it to whatever the zoom says, so a square keeps its place in
/// document space. Very large documents cap at 2048 a side and stretch
/// from there - a checker of square one is still a checker.
fn checker_image(width: u32, height: u32) -> slint::Image {
    const CHECK: u32 = 16;
    const LIGHT: [u8; 3] = [255, 255, 255];
    const DARK: [u8; 3] = [203, 203, 203];
    let (width, height) = (width.clamp(1, 2048), height.clamp(1, 2048));
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let even = (x / CHECK + y / CHECK).is_multiple_of(2);
            let colour = if even { LIGHT } else { DARK };
            let at = ((y * width + x) * 4) as usize;
            pixels[at..at + 3].copy_from_slice(&colour);
            pixels[at + 3] = 255;
        }
    }
    let buffer =
        SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width, height);
    slint::Image::from_rgba8(buffer)
}

/// A temporary directory beside `path`, unique to this process: the
/// staging ground of an atomic package save.
fn sibling_temp(path: &Path) -> std::path::PathBuf {
    path.with_extension(format!("tmp-{}", std::process::id()))
}

/// Moves `staging` onto `target`: a previous package steps aside first,
/// the staging takes its place, and only then is the old one dropped. A
/// failure on the way puts the previous package back, so a save either
/// lands whole or leaves what was there.
fn replace_package(staging: &Path, target: &Path) -> Result<(), String> {
    let aside = target.with_extension("old");
    let had_previous = target.exists();
    if had_previous {
        std::fs::rename(target, &aside).map_err(|e| format!("save: {e}"))?;
    }
    if let Err(error) = std::fs::rename(staging, target) {
        if had_previous {
            std::fs::rename(&aside, target).ok();
        }
        return Err(format!("save: {error}"));
    }
    if had_previous {
        if aside.is_dir() {
            std::fs::remove_dir_all(&aside).map_err(|e| format!("save: {e}"))?;
        } else {
            std::fs::remove_file(&aside).map_err(|e| format!("save: {e}"))?;
        }
    }
    Ok(())
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

    #[test]
    fn an_adjustment_lands_above_the_active_layer_and_publishes_its_knobs() {
        let (mut pane, _) = painting_pane();
        // The active base layer, then the adjustment added above it.
        pane.agent_layer_pick(0);
        pane.agent_adjustment_add(2); // Exposure

        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 2, "the adjustment joined the stack");
        // The panel lists front-to-back; the adjustment went in above the
        // base, so it is the front row - and it became the active one.
        assert_eq!(rows[0].0, pane.active.expect("active").as_u64());
        // It is an adjustment: no pixels of its own.
        assert!(pane.layer.is_some(), "painting falls back to the base");

        let (kind, parameters) = pane.agent_adjustment_state();
        assert_eq!(kind, 2);
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].0, tf("Stops", &[]));
        assert_eq!(parameters[0].1, 0.0, "a fresh exposure changes nothing");

        // One stop brighter, clamped into the parameter's own range.
        pane.agent_adjustment_param(0, 5.0);
        let (_, parameters) = pane.agent_adjustment_state();
        assert_eq!(parameters[0].1, 4.0, "stops clamp at four");
        // A parameter edit on a non-adjustment row is a no-op, not a panic.
        pane.agent_layer_pick(1);
        pane.agent_adjustment_param(0, 1.0);
        assert!(matches!(
            pane.document.as_ref().expect("open").find(
                pane.active.expect("active")
            ),
            Some(LayerNode::Layer(_))
        ));
    }

    #[test]
    fn a_project_package_round_trips_through_a_save_and_a_load() {
        let (mut pane, pixels) = painting_pane();
        // Paint something so the saved bitmap differs from a blank one,
        // then add an adjustment so the tree is not trivial.
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        pane.agent_adjustment_add(6); // Grain
        let tree = pane.document.as_ref().expect("open").to_json().expect("json");

        let dir = std::env::temp_dir().join("concat-agent-comp");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("round-trip.comp");
        pane.agent_save_comp(&path).expect("the save wrote");
        assert!(path.join("manifest.json").is_file());
        assert!(path.join("images").read_dir().expect("images").next().is_some());

        // A fresh pane loads the package: the same tree, the same pixel
        // ids, the painted stroke back.
        let mut back = CanvasPane::default();
        back.agent_open(&path).expect("the package opened");
        assert_eq!(
            back.document.as_ref().expect("open").to_json().expect("json"),
            tree,
            "the tree round-trips exactly"
        );
        let frame = back.store.get(pixels).expect("pixels");
        let alpha: Vec<u8> = frame.pixels()[3..].iter().step_by(4).copied().collect();
        assert!(alpha.iter().any(|&a| a > 0), "the stroke came back");

        // Minting continues past the restored ids.
        let fresh = back.store.put(concat_core::frame::Frame::black(1, 1));
        assert!(
            fresh.as_u64() > pixels.as_u64(),
            "the counter sits above the save's ids"
        );

        let _ = std::fs::remove_dir_all(&path);
        // A directory that is not a project is refused, not crashed on.
        let junk = dir.join("junk.comp");
        std::fs::create_dir_all(&junk).expect("junk");
        assert!(back.agent_open(&junk).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_group_folds_and_its_rows_keep_their_indexes() {
        let (mut pane, _) = painting_pane();
        // One more layer, then a group above both: the panel reads
        // group, layer, layer, front to back, the group at depth zero
        // and the others one step in.
        pane.agent_layer_add();
        pane.agent_layer_group();
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].6, "the front row is a group");
        assert!(rows[0].7, "the group's row is a group at all");
        assert_eq!(rows[0].5, 0, "the group sits at the root's depth");
        assert_eq!(rows[1].5, 0, "the layers beside it sit at the root too");
        assert!(!rows[1].6 && !rows[1].7, "a layer is neither group nor expanded");
        assert!(!rows.iter().any(|row| row.8), "nothing carries a mask yet");

        // A layer into the group: the panel shows the group's row, the
        // layer it now holds a step deeper, and the layer still outside.
        pane.agent_layer_move_into(2, Some(0));
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 3, "folding aside, every row still shows");
        assert_eq!(rows[1].5, 1, "the adopted layer sits one step in");
        assert_eq!(rows[2].5, 0, "the outside layer stays at the root");

        // Folding the group hides its children from the rows - and the
        // indexes the rest of the panel uses follow the folded list.
        pane.agent_layer_fold(0);
        assert_eq!(
            pane.agent_layers().len(),
            2,
            "the adopted layer folded away, the outside one stayed"
        );
        // The folded row still toggles: unfold puts everything back.
        // The flag order matters here: a folded group is a group whose
        // children are not shown - the pair the chevron reads.
        pane.agent_layer_fold(0);
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 3, "the children came back");
        assert!(rows[0].6 && rows[0].7, "unfolded: shown children, is a group");
        pane.agent_layer_fold(0);
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].6 && rows[0].7, "folded: children hidden, still a group");
        pane.agent_layer_fold(0);

        // Moving the group moves with it everything it holds: up in the
        // panel makes it the root's first child, ahead of the layer.
        pane.agent_layer_move(0, 1);
        let rows = pane.agent_layers();
        assert!(!rows[0].6, "the outside layer now leads the panel");
        assert!(rows[1].6, "the group follows, children and all");
    }

    #[test]
    fn a_curves_adjustment_round_trips_through_the_editor_protocol() {
        let (mut pane, _) = painting_pane();
        pane.agent_adjustment_add(7); // Curves
        let channels = pane.agent_curve_channels();
        assert_eq!(channels.len(), 3);
        for points in &channels {
            assert_eq!(
                points,
                &vec![(0.0, 0.0), (1.0, 1.0)],
                "a fresh curve is the identity"
            );
        }

        // A middle point lands where the input sorts it, and moving it
        // respects its neighbours' inputs without crossing them.
        pane.agent_curve_add(0, 0.5, 0.6);
        pane.agent_curve_add(0, 0.25, 0.3);
        let red = pane.agent_curve_channels()[0].clone();
        assert_eq!(red, vec![(0.0, 0.0), (0.25, 0.3), (0.5, 0.6), (1.0, 1.0)]);

        // A drag pushes the point at 0.5 toward 0.25; it stops short of
        // its left neighbour rather than crossing it.
        pane.agent_curve_set(0, 2, 0.1, 0.6);
        let red = pane.agent_curve_channels()[0].clone();
        assert!(red[2].0 > red[1].0, "the input kept its order");
        assert_eq!(red[2].1, 0.6, "the output took the drag");

        // The corners give only their output, never their place.
        pane.agent_curve_set(0, 0, 0.9, 0.2);
        pane.agent_curve_set(0, 3, 0.1, 0.8);
        let red = pane.agent_curve_channels()[0].clone();
        assert_eq!(red[0].0, 0.0);
        assert_eq!(red[0].1, 0.2);
        assert_eq!(red[3].0, 1.0);
        assert_eq!(red[3].1, 0.8);

        // Removing a middle point works; removing a corner does not.
        pane.agent_curve_remove(0, 2);
        assert_eq!(pane.agent_curve_channels()[0].len(), 3);
        pane.agent_curve_remove(0, 0);
        assert_eq!(pane.agent_curve_channels()[0].len(), 3);

        // The drawn strokes carry the geometry the editor plots with:
        // the first stroke starts at the (0, 0.2) corner - screen y 0.8,
        // the output axis flipped - and runs to the input 0.25 point.
        let segments = curve_segments(&pane.agent_curve_channels()[0].clone());
        assert_eq!(segments.len(), 2);
        let first = &segments[0];
        assert!((first.x - 0.0).abs() < 1e-6 && (first.y - 0.8).abs() < 1e-6);
        assert!((first.x2 - 0.25).abs() < 1e-6);

        // Paint beneath the curve - the adjustment colours whatever the
        // layers under it already show - and compare against the same
        // stroke with no adjustment over it.
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let document = pane.document.as_ref().expect("open");
        let pixel = concat_canvas::compose(document, &pane.store)
            .pixel(150, 100)
            .expect("a painted pixel");
        let plain = {
            let (mut pane, _) = painting_pane();
            pane.set_tool(3);
            pane.brush_press(150.0, 100.0);
            pane.brush_release();
            concat_canvas::compose(
                pane.document.as_ref().expect("open"),
                &pane.store,
            )
            .pixel(150, 100)
            .expect("a painted pixel")
        };
        assert!(
            pixel[0] > plain[0],
            "the lifted curve lifted the red channel"
        );

        // The other channels keep their identity while red bends.
        assert_eq!(
            pane.agent_curve_channels()[1],
            vec![(0.0, 0.0), (1.0, 1.0)]
        );

        // A non-curves row publishes no curves at all.
        pane.agent_layer_pick(1);
        assert!(pane.agent_curve_channels().is_empty());
    }

    #[test]
    fn a_drag_reorders_and_adopts_through_the_drop_rule() {
        let (mut pane, _) = painting_pane();
        pane.agent_layer_add();
        pane.agent_layer_group();
        // Rows: group, added layer, base layer - front to back.
        let rows = pane.agent_layers();
        assert!(rows[0].7, "the group leads the panel");
        let group_id = rows[0].0;

        // The base row dropped below the group's row goes into the group:
        // the bottom half of a group row is the drop-on-the-name read.
        pane.agent_layer_drop(2, 0, true);
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].5, 1, "the base sits one step in, adopted");
        assert_eq!(rows[0].0, group_id, "the group keeps its identity");

        // The added row - row 2 since the adoption - dropped above the
        // base's row lands just above it, in the same group.
        pane.agent_layer_drop(2, 1, false);
        let rows = pane.agent_layers();
        assert_eq!(rows[0].0, group_id, "the group still leads");
        assert_eq!(rows[1].5, 1, "the base stays one step in");
        assert_eq!(rows[2].5, 1, "the added row joined it there");

        // The group dropped onto its own child is refused: a group never
        // lands inside its own subtree.
        pane.agent_layer_drop(0, 1, true);
        let rows = pane.agent_layers();
        assert_eq!(rows[0].0, group_id, "the refusal left the tree alone");
        assert_eq!(rows[1].5, 1);

        // The base dropped above the group's row leaves the group and
        // takes a root slot ahead of it - a move out of a nested slot,
        // which is exactly the take the engine once could not do.
        pane.agent_layer_drop(2, 0, false);
        let rows = pane.agent_layers();
        assert_eq!(rows[0].5, 0, "the base is out at the root's depth");
        assert_eq!(rows[1].0, group_id, "the group follows it");
        assert_eq!(rows[2].5, 1, "the added row stayed inside");
        assert_eq!(rows[2].5, 1, "the added row stayed inside");

        // A drop on the moving row itself is a wiggle, not a shuffle.
        pane.agent_layer_drop(0, 0, true);
        let rows = pane.agent_layers();
        assert_eq!(rows[0].5, 0, "nothing moved");
    }

    #[test]
    fn a_mask_hides_where_the_brush_paints_it_black() {
        let (mut pane, pixels) = painting_pane();
        // Something on the layer for the mask to hide.
        pane.set_tool(3);
        pane.brush.color = [10, 20, 30];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let document = pane.document.clone().expect("open");
        let showed = concat_canvas::compose(&document, &pane.store)
            .pixel(150, 100)
            .expect("the painted pixel")
            [3];
        assert_eq!(showed, 255, "the stroke showed before any mask");

        // A white mask over the base layer, the painting tools pointed
        // at it.
        pane.agent_layer_mask_add(0);
        assert!(pane.agent_paint_mask(), "the tools point at the fresh mask");
        let rows = pane.agent_layers();
        assert!(rows[0].8, "the row carries a mask");
        assert!(rows[0].9, "that mask is the one being painted");

        let mask_id = pane
            .document
            .as_ref()
            .expect("open")
            .find(pane.active.expect("active"))
            .and_then(|node| node.mask())
            .map(|mask| mask.pixels)
            .expect("the mask exists");
        let mask = pane.store.get(mask_id).expect("mask pixels");
        assert!(
            mask.pixels().chunks_exact(4).all(|pixel| pixel == [255, 255, 255, 255]),
            "a fresh mask shows everything"
        );

        // Painting black on the mask hides the stroke beneath it, and
        // only where the dab landed.
        pane.brush.color = [0, 0, 0];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let document = pane.document.clone().expect("open");
        let frame = concat_canvas::compose(&document, &pane.store);
        assert_eq!(
            frame.pixel(150, 100).expect("the centre pixel")[3],
            0,
            "black on the mask hides what is under it"
        );

        // The mask's undo entry puts the white back, and the stroke
        // shows again.
        pane.undo();
        let document = pane.document.clone().expect("open");
        let frame = concat_canvas::compose(&document, &pane.store);
        assert_eq!(
            frame.pixel(150, 100).expect("the centre pixel")[3],
            255,
            "undo restores the mask, and the stroke with it"
        );

        // The layer's own pixels were never touched by any of it.
        let layer = pane.store.get(pixels).expect("layer pixels");
        let alpha: Vec<u8> = layer.pixels()[3..].iter().step_by(4).copied().collect();
        assert!(alpha.contains(&255), "the stroke is still there");
        assert_eq!(
            pane.paint_target(),
            Some(mask_id),
            "the tools are still on the mask"
        );

        // Dropping the mask drops the mode with it.
        pane.agent_layer_mask_remove(0);
        assert!(!pane.agent_paint_mask(), "no mask, no mask painting");
        assert!(pane.store.get(mask_id).is_none(), "the pixels went too");
    }

    #[test]
    fn the_gradient_maps_colours_follow_the_palette() {
        let (mut pane, _) = painting_pane();
        pane.agent_adjustment_add(5); // Gradient map
        let (kind, parameters) = pane.agent_adjustment_state();
        assert_eq!(kind, 5);
        assert!(parameters.is_empty(), "colours are not knobs");
        let (low, high) = pane.gradient_colors().expect("a gradient map");
        assert_eq!(low, [0.0, 0.0, 0.0], "a fresh map starts black");
        assert_eq!(high, [1.0, 1.0, 1.0], "and ends white");

        // The palette speaks 0..255 bytes; the map speaks 0..1 floats.
        pane.agent_gradient_color(0, 2);
        let (low, high) = pane.gradient_colors().expect("still a gradient map");
        assert_eq!(low, [212.0 / 255.0, 59.0 / 255.0, 55.0 / 255.0]);
        assert_eq!(high, [1.0, 1.0, 1.0], "the other end stood still");
        pane.agent_gradient_color(1, 0);
        assert_eq!(
            pane.gradient_colors().expect("still").1,
            [30.0 / 255.0, 30.0 / 255.0, 34.0 / 255.0]
        );

        // And the map really maps: the near-black default brush takes the
        // low colour where it lands, on the layer beneath the adjustment.
        pane.agent_layer_pick(1);
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let document = pane.document.as_ref().expect("open");
        let pixel = concat_canvas::compose(document, &pane.store)
            .pixel(150, 100)
            .expect("a painted pixel");
        assert!(
            pixel[0] > 120 && pixel[1] < 120,
            "the stroke's dark end reads as the low colour"
        );

        // A non-gradient row publishes no colours at all.
        pane.agent_layer_pick(1);
        assert!(pane.gradient_colors().is_none());
    }
}
