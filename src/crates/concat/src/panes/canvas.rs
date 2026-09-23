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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use concat_canvas::{
    Adjustment, BrushSettings, BrushStroke, CanvasGpu, CanvasViewport, ImageDocument, LayerMask,
    LayerNode, Mask, NavInput, Navigator, PixelId, PixelStore, SelectionShape, erase_region,
    fill_region,
};
use concat_core::frame::Frame;
use slint::{ComponentHandle, SharedPixelBuffer};

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

/// The editor keeps at most this many reversible canvas edits, and trims
/// older pixel versions once their retained storage reaches the byte cap.
const HISTORY_ENTRY_LIMIT: usize = 100;
const HISTORY_BYTE_LIMIT: usize = 256 * 1024 * 1024;
const PROJECT_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const PROJECT_PIXEL_LIMIT: usize = 512 * 1024 * 1024;
const PROJECT_BITMAP_LIMIT: usize = 1_000;
/// CPU decoding has a finite product limit as well, but a very thin image can
/// stay below it while exceeding every supported 2D texture dimension.
const DEFAULT_IMAGE_DIMENSION_LIMIT: u32 = 32_768;

#[derive(Clone)]
struct CanvasSnapshot {
    document: ImageDocument,
    store: PixelStore,
    active: Option<concat_canvas::LayerId>,
    layer: Option<PixelId>,
    paint_mask: bool,
    revision: u64,
}

#[derive(Clone)]
struct CanvasSaveData {
    document: ImageDocument,
    store: PixelStore,
    name: String,
}

impl CanvasSnapshot {
    fn same_state(&self, other: &Self) -> bool {
        self.document == other.document
            && self.store.same_versions(&other.store)
            && self.active == other.active
            && self.layer == other.layer
            && self.paint_mask == other.paint_mask
            && self.revision == other.revision
    }
}

struct CanvasHistoryEntry {
    before: CanvasSnapshot,
    after: CanvasSnapshot,
    retained_bytes: usize,
    coalesce_kind: u8,
}

/// A thumbnail is tied to both the pixel revision and the immutable frame
/// version. Pointer moves update the GPU scratch texture without changing
/// either, so the layer panel can publish repeatedly without resampling every
/// row on every event. A committed edit changes the revision or Arc source.
#[derive(Clone)]
struct ThumbnailCacheEntry {
    revision: u64,
    source: Weak<Frame>,
    mask: bool,
    image: slint::Image,
}

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
/// mask, whether that mask is the one the painting tools are on, and
/// whether that mask is enabled at all. The tuple the panel publishes
/// and the agent reads.
pub type LayerRow = (
    u64,
    String,
    bool,
    f32,
    bool,
    usize,
    bool,
    bool,
    bool,
    bool,
    bool,
);

/// Everything that can happen to the canvas.
#[derive(Debug)]
pub enum CanvasMsg {
    /// The picker came back with files; the first image is the one opened.
    Picked(Vec<std::path::PathBuf>),
    /// Adds images to the current document as separate editable layers.
    ImportLayers(Vec<std::path::PathBuf>),
    HandoffImport(Vec<std::path::PathBuf>),
    NewAndImport(Vec<std::path::PathBuf>),
    OpenAndImport(PathBuf, Vec<std::path::PathBuf>),
    /// Creates an editable blank canvas in managed project storage.
    New,
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
    /// A custom RGB colour from the colour controls.
    BrushRgb(f64, f64, f64),
    /// A custom six-digit hexadecimal colour. Invalid text is ignored.
    BrushHex(String),
    /// Sample one composited pixel without starting a brush stroke.
    PickColor(f64, f64),
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
    /// The active node's mask, gone.
    LayerMaskRemove,
    /// The active node's mask applied or set aside, whole.
    LayerMaskToggle,
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
    /// Export the composed canvas to the managed personal library.
    ExportLibrary,
    /// Save the whole document - tree, ids and pixels - as a `.comp`
    /// project package, through a save dialog.
    SaveComp,
    /// Save the whole document to a new `.comp` path, even when an existing
    /// project path is already recorded.
    SaveCompAs,
    /// Resolve a pending open request while the current document is edited.
    OpenDiscard,
    OpenCancel,
    OpenSave,
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
    pub brush_color_index: i32,
    /// The stroke in flight, while a paint tool is dragging.
    stroke: Option<BrushStroke>,
    /// The layer's pixels as they stood before the stroke: the base the
    /// changed tiles re-copy from, and the undo entry's snapshot.
    stroke_base: Option<Arc<Frame>>,
    /// The stroke's working copy: pre-stroke pixels with the whole stroke
    /// composited so far. One copy per stroke, not per event.
    stroke_scratch: Option<Frame>,
    /// The pixel id and mask mode chosen at press time. They stay fixed for
    /// the whole gesture even if a layer-selection message arrives before
    /// the pointer release.
    stroke_target: Option<PixelId>,
    stroke_on_mask: bool,
    /// Unified document and pixel edits, newest last. Immutable frames are
    /// shared between snapshots; only changed versions consume the byte cap.
    undo_stack: Vec<CanvasHistoryEntry>,
    /// Undone edits, newest last, for [`CanvasMsg::Redo`].
    redo_stack: Vec<CanvasHistoryEntry>,
    /// The state before the current gesture or immediate command.
    pending_history: Option<(CanvasSnapshot, u8)>,
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
    /// Monotonic document revision and the revision last saved or opened.
    revision: u64,
    saved_revision: u64,
    document_generation: u64,
    autosave_inflight: Option<u64>,
    /// A requested image/project held while the discard dialog is visible.
    pending_open: Option<PathBuf>,
    pending_handoff_paths: Option<Vec<PathBuf>>,
    pub open_confirm: bool,
    /// The full path of the current `.comp` package, when this document has
    /// one. Ordinary Save writes here; a newly opened image uses Save As
    /// before it acquires a project path.
    project_path: Option<PathBuf>,
    /// Small layer and mask previews, keyed by their immutable pixel source.
    thumbnail_cache: HashMap<PixelId, ThumbnailCacheEntry>,
    thumbnail_rows: (Vec<slint::Image>, Vec<slint::Image>),
    /// Per-pixel dirty revision. A live CPU stroke may replace its working
    /// Arc on every pointer move; this only advances when the stroke lands.
    pixel_revisions: HashMap<PixelId, u64>,
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
            stroke_target: None,
            stroke_on_mask: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_history: None,
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
            revision: 1,
            saved_revision: 1,
            document_generation: 1,
            autosave_inflight: None,
            pending_open: None,
            pending_handoff_paths: None,
            open_confirm: false,
            project_path: None,
            thumbnail_cache: HashMap::new(),
            thumbnail_rows: (Vec::new(), Vec::new()),
            pixel_revisions: HashMap::new(),
        }
    }
}

impl CanvasPane {
    /// Applies one message and records every document or pixel mutation in
    /// the same ordered history. Pointer gestures own their transaction from
    /// press through release; immediate commands are wrapped here.
    pub fn update(&mut self, msg: CanvasMsg, studio: &mut Studio) {
        let previous_revision = self.revision;
        let history = match &msg {
            CanvasMsg::FillSelection => Some(0),
            CanvasMsg::DeleteSelection => Some(0),
            CanvasMsg::LayerToggleVisibility(_) => Some(0),
            CanvasMsg::LayerOpacity(_, _) => Some(1),
            CanvasMsg::LayerAdd => Some(0),
            CanvasMsg::ImportLayers(_) => Some(0),
            CanvasMsg::HandoffImport(_) => Some(0),
            CanvasMsg::LayerAddGroup => Some(0),
            CanvasMsg::LayerDelete(_) => Some(0),
            CanvasMsg::LayerMove(_, _) | CanvasMsg::LayerDrop(_, _, _) => Some(0),
            CanvasMsg::LayerMaskAdd => Some(0),
            CanvasMsg::LayerMaskRemove => Some(0),
            CanvasMsg::LayerMaskToggle => Some(0),
            CanvasMsg::GradientColor(_, _) => Some(0),
            CanvasMsg::AdjustmentAdd(_) => Some(0),
            CanvasMsg::AdjustmentParam(_, _) => Some(2),
            CanvasMsg::CurveSet(_, _, _, _) => Some(3),
            CanvasMsg::CurveAdd(_, _, _) | CanvasMsg::CurveRemove(_, _) => Some(0),
            _ => None,
        };
        if let Some(coalesce) = history {
            self.begin_history_mode(coalesce);
        }
        self.update_inner(msg, studio);
        if history.is_some() {
            self.commit_history();
        }
        if self.revision != previous_revision && self.is_modified() {
            let revision = self.revision;
            slint::Timer::single_shot(std::time::Duration::from_millis(900), move || {
                crate::host::Shell::with(|shell, _app| {
                    let mut studio = shell.studio.borrow_mut();
                    if studio.canvas.revision == revision && studio.canvas.is_modified() {
                        if let Err(error) = studio.canvas.start_auto_save() {
                            studio.notify(&format!("画布自动保存失败：{error}"), true);
                        }
                    }
                });
            });
        }
    }

    /// Applies one message. The studio is the rest of the window; the host
    /// inside it is where the shared device lives. Everything runs on the
    /// event-loop thread, which is the one thread the window's renderer
    /// submits from - the same rule the monitor's drawing keeps.
    fn update_inner(&mut self, msg: CanvasMsg, studio: &mut Studio) {
        match msg {
            CanvasMsg::Picked(paths) => {
                if let Some(path) = paths.first() {
                    self.request_open(path, studio);
                }
            }
            CanvasMsg::ImportLayers(paths) => {
                self.import_layers(&paths, studio);
            }
            CanvasMsg::HandoffImport(paths) => {
                let success = self.import_layers(&paths, studio);
                report_canvas_handoff(
                    success,
                    if success {
                        ""
                    } else {
                        "画布素材导入失败，请重试"
                    },
                );
            }
            CanvasMsg::New => self.new_blank(studio),
            CanvasMsg::NewAndImport(paths) => {
                if !self.validate_import_paths(&paths, studio) {
                    report_canvas_handoff(false, "画布素材导入失败，请重试");
                    return;
                }
                let generation = self.document_generation;
                self.new_blank(studio);
                if self.document_generation != generation {
                    self.begin_history_mode(0);
                    let success = self.import_layers(&paths, studio);
                    self.commit_history();
                    report_canvas_handoff(
                        success,
                        if success {
                            ""
                        } else {
                            "画布素材导入失败，请重试"
                        },
                    );
                } else {
                    report_canvas_handoff(false, "画布项目未能创建，请重试");
                }
            }
            CanvasMsg::OpenAndImport(path, paths) => {
                if !self.validate_import_paths(&paths, studio) {
                    report_canvas_handoff(false, "画布素材导入失败，请重试");
                    return;
                }
                self.pending_handoff_paths = Some(paths);
                self.request_open(&path, studio);
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
                self.sync_view();
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
                self.sync_view();
            }
            CanvasMsg::Release(option) => {
                self.nav.apply(NavInput::Release { option });
                self.sync_view();
            }
            CanvasMsg::Fit => {
                self.nav.apply(NavInput::Fit);
                self.sync_view();
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
                self.brush_release();
                self.render(studio);
            }
            CanvasMsg::BrushSize(diameter) => {
                self.brush.diameter = diameter.clamp(1.0, 2000.0);
            }
            CanvasMsg::BrushOpacity(opacity) => {
                self.brush.opacity = opacity.clamp(0.01, 1.0);
            }
            CanvasMsg::BrushHardness(hardness) => {
                self.brush.hardness = hardness.clamp(0.0, 1.0);
            }
            CanvasMsg::BrushColor(index) => {
                if let Some(color) = PALETTE.get(index) {
                    self.brush.color = *color;
                    self.brush.erasing = false;
                    self.brush_color_index = index as i32;
                }
            }
            CanvasMsg::BrushRgb(red, green, blue) => {
                self.set_brush_rgb(red, green, blue);
            }
            CanvasMsg::BrushHex(text) => {
                self.set_brush_hex(&text);
            }
            CanvasMsg::PickColor(x, y) => {
                self.pick_color(x, y);
            }
            CanvasMsg::Undo => {
                self.undo();
                self.render(studio);
            }
            CanvasMsg::Redo => {
                self.redo();
                self.render(studio);
            }
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
                let fold =
                    self.rows()
                        .get(index.max(0) as usize)
                        .and_then(|(node, _)| match node {
                            LayerNode::Group(group) => Some(group.id),
                            _ => None,
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
                self.delete_row(index);
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
            CanvasMsg::LayerMaskRemove => {
                self.remove_mask();
                self.render(studio);
            }
            CanvasMsg::LayerMaskToggle => {
                self.toggle_mask();
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
            CanvasMsg::ExportPng => self.export_png(studio, false),
            CanvasMsg::ExportLibrary => self.export_png(studio, true),
            CanvasMsg::SaveComp => {
                self.save_comp_dialog(studio);
            }
            CanvasMsg::SaveCompAs => {
                self.save_comp_as_dialog(studio);
            }
            CanvasMsg::OpenDiscard => {
                self.open_confirm = false;
                if let Some(path) = self.pending_open.take() {
                    self.open_now(&path, studio);
                }
            }
            CanvasMsg::OpenCancel => {
                self.pending_open = None;
                self.open_confirm = false;
                if self.pending_handoff_paths.take().is_some() {
                    report_canvas_handoff(false, "");
                }
            }
            CanvasMsg::OpenSave => {
                if self.save_comp_dialog(studio) {
                    self.open_confirm = false;
                    if let Some(path) = self.pending_open.take() {
                        self.open_now(&path, studio);
                    }
                }
            }
        }
    }

    /// The tray's tool, clamped into it: a picker can only offer what the
    /// tray has, but the number arrives over a boundary. The eraser is the
    /// brush with its erasing bit on, so the settings stay shared.
    pub fn set_tool(&mut self, tool: i32) {
        self.tool = (tool.max(0) as usize).min(6);
        self.brush.erasing = self.tool == 4;
    }

    /// Sets a custom brush colour from RGB controls. Values outside the
    /// byte range are clamped after rounding; non-finite input is ignored so
    /// a malformed field cannot replace the last valid colour.
    fn set_brush_rgb(&mut self, red: f64, green: f64, blue: f64) {
        if !red.is_finite() || !green.is_finite() || !blue.is_finite() {
            return;
        }
        self.brush.color = [
            red.round().clamp(0.0, 255.0) as u8,
            green.round().clamp(0.0, 255.0) as u8,
            blue.round().clamp(0.0, 255.0) as u8,
        ];
        if self.tool == 4 {
            self.set_tool(3);
        }
        self.brush_color_index = PALETTE
            .iter()
            .position(|color| color == &self.brush.color)
            .map(|index| index as i32)
            .unwrap_or(-1);
    }

    /// The current brush colour as a canonical six-digit hexadecimal value.
    pub fn brush_hex(&self) -> String {
        format!(
            "#{:02X}{:02X}{:02X}",
            self.brush.color[0], self.brush.color[1], self.brush.color[2]
        )
    }

    /// The target shown alongside the current tool and colour controls.
    pub fn paint_target_label(&self) -> &'static str {
        if self.paint_mask { "蒙版" } else { "图层" }
    }

    /// Accepts `#RRGGBB` or `RRGGBB`. Invalid text leaves the colour alone.
    fn set_brush_hex(&mut self, text: &str) {
        let text = text.trim();
        let text = text.strip_prefix('#').unwrap_or(text);
        if text.len() != 6 || !text.is_ascii() {
            return;
        }
        let Ok(red) = u8::from_str_radix(&text[0..2], 16) else {
            return;
        };
        let Ok(green) = u8::from_str_radix(&text[2..4], 16) else {
            return;
        };
        let Ok(blue) = u8::from_str_radix(&text[4..6], 16) else {
            return;
        };
        self.set_brush_rgb(f64::from(red), f64::from(green), f64::from(blue));
    }

    /// Samples one pixel from the composed document. This is deliberately
    /// called only after the explicit colour-picker click, so a pointer move
    /// never causes a full composite readback or a brush press.
    fn pick_color(&mut self, x: f64, y: f64) {
        let (dx, dy) = self.to_document(x, y);
        self.pick_color_document(dx, dy);
    }

    fn pick_color_document(&mut self, dx: f64, dy: f64) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if !dx.is_finite()
            || !dy.is_finite()
            || dx < 0.0
            || dy < 0.0
            || dx >= f64::from(document.width)
            || dy >= f64::from(document.height)
        {
            return;
        }
        let point = (dx.floor() as u32, dy.floor() as u32);
        let sampled = match &mut self.gpu {
            Some(gpu) => gpu
                .compose_frame(document, &self.store)
                .pixel(point.0, point.1),
            None => concat_canvas::compose(document, &self.store).pixel(point.0, point.1),
        };
        if let Some([red, green, blue, _]) = sampled {
            self.set_brush_rgb(f64::from(red), f64::from(green), f64::from(blue));
        }
    }

    /// The wand's click: the selection becomes the colour run under the
    /// pointer, flood-filled from the layer's pixels.
    fn wand_click(&mut self, x: f64, y: f64) {
        let (dx, dy) = self.to_document(x, y);
        self.wand_document(dx, dy);
    }

    /// Runs the wand at a document pixel. Pointer input converts through
    /// [`CanvasPane::wand_click`], while the automation API is already here.
    fn wand_document(&mut self, dx: f64, dy: f64) {
        let Some(layer) = self.layer else {
            return;
        };
        let Some(frame) = self.store.get(layer) else {
            return;
        };
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
        let on_mask = self.is_mask_pixels(layer);
        match kind {
            EditKind::Fill => fill_region(&mut frame, mask, self.brush.color),
            EditKind::Delete => erase_region(&mut frame, mask),
        }
        if on_mask {
            normalize_mask_pixels(&mut frame);
        }
        let frame = Arc::new(frame);
        self.store.replace_shared(layer, frame.clone());
        self.bump_thumbnail_revision(layer);
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

    fn snapshot(&self) -> Option<CanvasSnapshot> {
        Some(CanvasSnapshot {
            document: self.document.clone()?,
            store: self.store.clone(),
            active: self.active,
            layer: self.layer,
            paint_mask: self.paint_mask,
            revision: self.revision,
        })
    }

    fn begin_history(&mut self) {
        self.begin_history_mode(0);
    }

    fn begin_history_mode(&mut self, coalesce_kind: u8) {
        if self.pending_history.is_none() {
            self.pending_history = self.snapshot().map(|snapshot| (snapshot, coalesce_kind));
        }
    }

    fn history_edit(&mut self, edit: impl FnOnce(&mut Self)) {
        self.begin_history();
        edit(self);
        self.commit_history();
    }

    fn commit_history(&mut self) {
        let Some((before, coalesce_kind)) = self.pending_history.take() else {
            return;
        };
        let Some(mut after) = self.snapshot() else {
            return;
        };
        if before.same_state(&after) {
            return;
        }
        self.revision = self.revision.wrapping_add(1).max(1);
        after.revision = self.revision;
        if coalesce_kind != 0
            && let Some(last) = self.undo_stack.last_mut()
            && last.coalesce_kind == coalesce_kind
            && last.after.same_state(&before)
            && last.before.store.same_versions(&before.store)
        {
            last.after = after;
            last.retained_bytes = last
                .before
                .store
                .unshared_bytes(&last.after.store)
                .max(last.after.store.unshared_bytes(&last.before.store));
            self.redo_stack.clear();
            return;
        }
        let retained_bytes = before
            .store
            .unshared_bytes(&after.store)
            .max(after.store.unshared_bytes(&before.store));
        self.undo_stack.push(CanvasHistoryEntry {
            before,
            after,
            retained_bytes,
            coalesce_kind,
        });
        self.redo_stack.clear();
        self.trim_history();
    }

    fn trim_history(&mut self) {
        while self.undo_stack.len() > HISTORY_ENTRY_LIMIT
            || self
                .undo_stack
                .iter()
                .map(|entry| entry.retained_bytes)
                .sum::<usize>()
                > HISTORY_BYTE_LIMIT
        {
            self.undo_stack.remove(0);
        }
    }

    fn restore_snapshot(&mut self, snapshot: CanvasSnapshot) {
        self.document = Some(snapshot.document);
        self.store = snapshot.store;
        self.active = snapshot.active;
        self.layer = snapshot.layer;
        self.paint_mask = snapshot.paint_mask;
        self.revision = snapshot.revision;
        self.stroke = None;
        self.stroke_base = None;
        self.stroke_scratch = None;
        self.stroke_target = None;
        self.stroke_on_mask = false;
        self.pending_history = None;
        self.thumbnail_cache.clear();
        self.pixel_revisions.clear();
        if let Some(gpu) = &mut self.gpu {
            gpu.reset_document();
        }
        self.sync_view();
    }

    /// Whether the canvas toolbar should enable its history actions.
    pub fn can_undo(&self) -> bool {
        self.stroke.is_none() && !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        self.stroke.is_none() && !self.redo_stack.is_empty()
    }

    pub fn is_modified(&self) -> bool {
        self.document.is_some() && self.revision != self.saved_revision
    }

    /// Starts a stroke: the paint target's pixels are snapshotted for the
    /// undo entry and for the tiles' base, a scratch frame is copied once,
    /// and the first dab goes down. Pixel work only; the caller renders.
    fn brush_press(&mut self, x: f64, y: f64) {
        let (dx, dy) = self.to_document(x, y);
        self.brush_press_document(dx, dy);
    }

    /// Starts a stroke at a document pixel. Pointer input converts through
    /// [`CanvasPane::brush_press`]; automation already speaks this space.
    fn brush_press_document(&mut self, dx: f64, dy: f64) {
        let Some((w, h)) = self.document_size().map(|(w, h)| (w as u32, h as u32)) else {
            return;
        };
        let Some(layer) = self.paint_target() else {
            return;
        };
        // A second press while one stroke is live finishes it first: two
        // pointers, or a lost release, must not nest strokes.
        self.brush_release();
        // A press off the canvas starts nothing; a drag onto it does, via
        // the move's fringe rule.
        if dx < 0.0 || dy < 0.0 || dx >= f64::from(w) || dy >= f64::from(h) {
            return;
        }
        let Ok(stroke) = BrushStroke::new(w, h, self.brush) else {
            return;
        };
        self.begin_history();
        let base = self.store.get(layer);
        self.stroke_base = base.clone();
        self.stroke_scratch = base.map(|frame| (*frame).clone());
        self.stroke_target = Some(layer);
        self.stroke_on_mask = self.is_mask_pixels(layer);
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
        let Some(stroke) = self.stroke.as_mut() else {
            return Vec::new();
        };
        let changed = stroke.flush();
        self.commit_tiles(&changed);
        let target = self.stroke_target;
        self.stroke = None;
        self.stroke_base = None;
        if let (Some(target), Some(scratch)) = (target, self.stroke_scratch.take()) {
            let on_mask = self.stroke_on_mask;
            self.store.replace(target, scratch);
            self.bump_thumbnail_revision(target);
            if let (Some(gpu), Some(frame)) = (&mut self.gpu, self.store.get(target)) {
                if on_mask {
                    gpu.finish_mask(target, &frame);
                } else {
                    // Same frame id as the last dirty upload: this closes the
                    // live resident without uploading the whole frame again.
                    gpu.upload(target, &frame, None);
                }
            }
        }
        self.stroke_target = None;
        self.stroke_on_mask = false;
        self.commit_history();
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
        let Some(layer) = self.stroke_target else {
            return;
        };
        // Read before the scratch frame is borrowed out of the pane.
        let on_mask = self.stroke_on_mask;
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
        if let Some(selection) = &self.selection {
            restrict_to_selection(scratch, &base, selection, changed);
        }
        if on_mask {
            normalize_mask_tiles(scratch, changed);
        }
        if on_mask {
            // The store stays the mask's truth, and the mask texture
            // re-uploads whole; a mask has no tile residents to poke.
            if let Some(gpu) = &mut self.gpu {
                for &(tx, ty) in changed {
                    gpu.refresh_mask_region(
                        layer,
                        scratch,
                        concat_canvas::DirtyRect {
                            x: (tx * TILE) as u32,
                            y: (ty * TILE) as u32,
                            width: TILE as u32,
                            height: TILE as u32,
                        },
                    );
                }
            } else {
                self.store.replace(layer, scratch.clone());
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
        let Some(entry) = self.undo_stack.pop() else {
            return;
        };
        self.restore_snapshot(entry.before.clone());
        self.redo_stack.push(entry);
    }

    /// Steps the pixel history forward one undone edit. Masks upload as
    /// masks, for the same reason [`CanvasPane::undo`] gives.
    fn redo(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        let Some(entry) = self.redo_stack.pop() else {
            return;
        };
        self.restore_snapshot(entry.after.clone());
        self.undo_stack.push(entry);
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

    /// Removes a visible row while keeping an existing selection whenever it
    /// still names a node. If the selected node was removed with the row (or
    /// there was no selection), the nearest image sibling in the same parent
    /// wins before the visible tree is searched as a fallback.
    fn delete_row(&mut self, index: i32) {
        let Some(target) = self
            .rows()
            .get(index.max(0) as usize)
            .map(|(node, _)| node.id())
        else {
            return;
        };
        let (parent_slot, active_removed) = {
            let Some(document) = self.document.as_ref() else {
                return;
            };
            let parent_slot = node_parent_slot(document, target);
            let active_removed = self.active.is_some_and(|active| {
                active == target
                    || matches!(
                        document.find(target),
                        Some(LayerNode::Group(group)) if group.find(active).is_some()
                    )
            });
            (parent_slot, active_removed)
        };
        {
            let Some(document) = self.document.as_mut() else {
                return;
            };
            document.remove(target);
            self.collapsed.remove(&target);
            self.store.retain_document(document);
        }

        if !active_removed && self.active.is_some() {
            // Keep the selected adjustment or group, but rebuild the image
            // target in its parent after the deleted image has been removed.
            // `layer` may have named that image even though `active` did not.
            self.refresh_active_layer();
            self.sync_view();
            return;
        }
        let picked = parent_slot
            .and_then(|(parent, position)| self.sibling_image(parent, position))
            .or_else(|| {
                self.rows()
                    .iter()
                    .find_map(|(node, _)| node_image_pixels(node).map(|pixels| (node.id(), pixels)))
            });
        self.active = picked.map(|(id, _)| id);
        self.layer = picked.map(|(_, pixels)| pixels);
        // A deleted active row cannot leave the tools pointed at a mask that
        // no longer exists. The next selected row starts on its pixels.
        self.paint_mask = false;
        self.sync_view();
    }

    /// Rebuilds the pixel target for a selected node that survived a sibling
    /// deletion. Image rows keep their own pixels; adjustments and groups use
    /// the nearest image in their direct parent, then the document root.
    fn refresh_active_layer(&mut self) {
        let Some(document) = self.document.as_ref() else {
            self.layer = None;
            self.paint_mask = false;
            return;
        };
        let parent = self
            .active
            .and_then(|id| node_parent_slot(document, id))
            .and_then(|(parent, _)| parent);
        let active_pixels = self
            .active
            .and_then(|id| document.find(id))
            .and_then(node_image_pixels);
        self.layer = active_pixels
            .or_else(|| topmost_image_in_parent(document, parent))
            .or_else(|| topmost_image_in_parent(document, None));
        if self
            .active
            .and_then(|id| document.find(id))
            .and_then(|node| node.mask())
            .is_none()
        {
            self.paint_mask = false;
        }
    }

    /// Returns the nearest direct image sibling around a deleted row's old
    /// position. `parent == None` is the document root.
    fn sibling_image(
        &self,
        parent: Option<concat_canvas::LayerId>,
        position: usize,
    ) -> Option<(concat_canvas::LayerId, PixelId)> {
        let document = self.document.as_ref()?;
        let children = parent
            .and_then(|id| document.find(id))
            .and_then(|node| match node {
                LayerNode::Group(group) => Some(&group.children),
                _ => None,
            })
            .unwrap_or(&document.root.children);
        let start = position.min(children.len());
        for node in children.iter().skip(start) {
            if let Some(pixels) = node_image_pixels(node) {
                return Some((node.id(), pixels));
            }
        }
        for node in children[..start].iter().rev() {
            if let Some(pixels) = node_image_pixels(node) {
                return Some((node.id(), pixels));
            }
        }
        None
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
        self.tool = 3;
        self.brush.erasing = false;
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
        self.tool = 3;
        self.brush.erasing = false;
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
    /// at all, whether it carries a mask, whether that mask is the one
    /// being painted, and whether that mask is enabled. Folded groups
    /// still show their own row.
    pub fn layers_data(&self) -> Vec<LayerRow> {
        self.rows()
            .iter()
            .map(|(node, depth)| {
                let expanded = match node {
                    LayerNode::Group(group) => !self.collapsed.contains(&group.id),
                    _ => false,
                };
                let active = self.active == Some(node.id());
                let mask = node.mask();
                (
                    node.id().as_u64(),
                    node.name().to_owned(),
                    node.hidden(),
                    node.opacity(),
                    active,
                    *depth,
                    expanded,
                    matches!(node, LayerNode::Group(_)),
                    mask.is_some(),
                    self.paint_mask && active,
                    mask.map(|m| m.enabled).unwrap_or(true),
                )
            })
            .collect()
    }

    /// Returns one image and one mask thumbnail for every visible layer row.
    /// The two vectors intentionally keep the row index, so the UI can make
    /// the image and mask chips independently selectable. Empty entries are
    /// transparent placeholders for groups and unmasked rows.
    pub fn layer_thumbnails(&self) -> (Vec<slint::Image>, Vec<slint::Image>) {
        self.thumbnail_rows.clone()
    }

    fn refresh_thumbnails(&mut self) {
        let ids: Vec<(Option<PixelId>, Option<PixelId>)> = self
            .rows()
            .into_iter()
            .map(|(node, _)| (node_image_pixels(node), node.mask().map(|mask| mask.pixels)))
            .collect();
        // Keep cache allocations bounded when rows disappear or a group closes.
        self.thumbnail_cache.retain(|id, _| {
            ids.iter()
                .any(|(layer, mask)| *layer == Some(*id) || *mask == Some(*id))
        });
        let mut layers = Vec::with_capacity(ids.len());
        let mut masks = Vec::with_capacity(ids.len());
        for (layer, mask) in ids {
            layers.push(self.thumbnail(layer, false));
            masks.push(self.thumbnail(mask, true));
        }
        self.thumbnail_rows = (layers, masks);
    }

    fn thumbnail(&mut self, id: Option<PixelId>, mask: bool) -> slint::Image {
        let Some(id) = id else {
            return slint::Image::default();
        };
        let Some(frame) = self.store.get(id) else {
            return slint::Image::default();
        };
        // A Weak retains the allocation identity without retaining old pixel data.
        let source = Arc::downgrade(&frame);
        let revision = self.pixel_revisions.get(&id).copied().unwrap_or(0);
        if let Some(cached) = self.thumbnail_cache.get(&id)
            && cached.revision == revision
            && cached.mask == mask
            && (cached.source.ptr_eq(&source)
                || (self.stroke.is_some() && self.stroke_target == Some(id)))
        {
            return cached.image.clone();
        }
        let image = frame_thumbnail(&frame, 42, 28, mask);
        self.thumbnail_cache.insert(
            id,
            ThumbnailCacheEntry {
                revision,
                source,
                mask,
                image: image.clone(),
            },
        );
        image
    }

    fn bump_thumbnail_revision(&mut self, id: PixelId) {
        let revision = self.pixel_revisions.entry(id).or_default();
        *revision = revision.wrapping_add(1).max(1);
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
        // The insertion index is relative to the active node's direct
        // parent. A nested selection therefore stays inside its group, while
        // an absent selection appends to the root.
        let (parent, index) = self
            .active
            .and_then(|id| node_parent_slot(document, id))
            .map(|(parent, position)| (parent, position + 1))
            .unwrap_or((None, document.root.children.len()));
        let name = adjustment_label(kind);
        let Some(adjustment) = default_adjustment(kind) else {
            return;
        };
        let document = self.document.as_mut().expect("checked above");
        let id = document.new_adjustment(name, adjustment);
        // `new_adjustment` appends to the root; move the node into the
        // captured parent after the append so the borrows stay disjoint.
        let node = {
            let children = &mut document.root.children;
            let from = children
                .iter()
                .position(|child| child.id() == id)
                .expect("just appended");
            children.remove(from)
        };
        if let Some(children) = children_for_parent_mut(document, parent) {
            let to = index.min(children.len());
            children.insert(to, node);
        } else {
            document.root.children.push(node);
        }
        self.active = Some(id);
        // Adjustments hold no pixels; the tools keep working on the
        // topmost image layer in the same parent beneath them.
        self.layer = topmost_image_in_parent(document, parent)
            .or_else(|| topmost_image_in_parent(document, None));
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
        let (parent, index) = self
            .active
            .and_then(|id| node_parent_slot(document, id))
            .map(|(parent, position)| (parent, position + 1))
            .unwrap_or((None, document.root.children.len()));
        let name = tf("Group {0}", &[&count.to_string()]).to_string();
        let document = self.document.as_mut().expect("checked above");
        let id = document.new_group(name);
        let node = {
            let children = &mut document.root.children;
            let from = children
                .iter()
                .position(|child| child.id() == id)
                .expect("just appended");
            children.remove(from)
        };
        if let Some(children) = children_for_parent_mut(document, parent) {
            let to = index.min(children.len());
            children.insert(to, node);
        } else {
            document.root.children.push(node);
        }
        self.active = Some(id);
        self.layer = topmost_image_in_parent(document, parent)
            .or_else(|| topmost_image_in_parent(document, None));
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
    fn export_png(&mut self, studio: &mut Studio, to_library: bool) {
        let Some(document) = self.document.clone() else {
            return;
        };
        if let Err(error) = validate_canvas_dimensions(
            document.width,
            document.height,
            self.gpu
                .as_ref()
                .map(|gpu| gpu.device().limits().max_texture_dimension_2d),
        ) {
            log::warn!("canvas: {error}");
            studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
            return;
        }
        let frame = match &mut self.gpu {
            Some(gpu) => gpu.compose_frame(&document, &self.store),
            None => concat_canvas::compose(&document, &self.store),
        };
        let name = match self.name.rsplit_once('.') {
            Some((stem, _)) => format!("{stem}.png"),
            None => format!("{}.png", self.name),
        };
        let path = if to_library {
            let folder = match concat_host::AppDirs::locate() {
                Ok(dirs) => dirs.data.join("canvas-exports"),
                Err(error) => {
                    studio.notify(&format!("无法打开个人资产库：{error}"), true);
                    return;
                }
            };
            if let Err(error) = std::fs::create_dir_all(&folder) {
                studio.notify(&format!("无法创建画布导出目录：{error}"), true);
                return;
            }
            let safe_name = name
                .trim_end_matches(".png")
                .chars()
                .map(|ch| {
                    if ch == '/' || ch == '\\' || ch.is_control() {
                        '_'
                    } else {
                        ch
                    }
                })
                .collect::<String>();
            folder.join(format!("{}-{safe_name}.png", uuid::Uuid::new_v4()))
        } else {
            let Some(path) = crate::platform::save_file(
                &tf("Export as PNG", &[]),
                &name,
                Some(("PNG", &["png"])),
            ) else {
                return;
            };
            path
        };
        match encode_png(&frame) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&path, bytes) {
                    log::warn!("canvas: {error}");
                    studio.notify(&tf("Canvas failed: {0}", &[&error.to_string()]), true);
                } else if to_library {
                    let path = path.to_string_lossy().into_owned();
                    crate::host::Shell::with(|_, app| {
                        app.global::<crate::ui::SeeCut>()
                            .invoke_action("personal-register-canvas".into(), path.into());
                    });
                } else {
                    studio.notify("图片已导出", false);
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

    /// Saves the open document to its recorded path, opening Save As only
    /// for a document that does not have one yet.
    fn save_comp_dialog(&mut self, studio: &mut Studio) -> bool {
        match self.save_current() {
            Ok(saved) => saved,
            Err(error) => {
                log::warn!("canvas: {error}");
                studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
                false
            }
        }
    }

    /// Saves the open document to a newly selected `.comp` path.
    fn save_comp_as_dialog(&mut self, studio: &mut Studio) -> bool {
        match self.save_as() {
            Ok(saved) => saved,
            Err(error) => {
                log::warn!("canvas: {error}");
                studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
                false
            }
        }
    }

    /// Performs an ordinary save without needing the surrounding `Studio`.
    /// The boolean is false when there is no document or the Save As dialog
    /// was cancelled.
    pub fn save_current(&mut self) -> Result<bool, String> {
        if self.document.is_none() {
            return Ok(false);
        }
        if let Some(path) = self.project_path.clone() {
            self.save_to_path(&path)?;
            return Ok(true);
        }
        self.save_as()
    }

    /// Opens the Save As dialog and records the chosen path only after the
    /// package has been written successfully.
    fn save_as(&mut self) -> Result<bool, String> {
        if self.document.is_none() {
            return Ok(false);
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
            return Ok(false);
        };
        self.save_to_path(&path)?;
        Ok(true)
    }

    fn save_to_path(&mut self, path: &Path) -> Result<(), String> {
        self.save_comp(path)?;
        self.saved_revision = self.revision;
        self.project_path = Some(path.to_owned());
        remember_canvas_project(path)?;
        notify_canvas_registry_changed();
        Ok(())
    }

    fn save_auto(&mut self) -> Result<(), String> {
        if self.document.is_none() {
            return Ok(());
        }
        let path = match self.project_path.clone() {
            Some(path) => path,
            None => {
                let dirs = concat_host::AppDirs::locate()?;
                let folder = dirs.data.join("canvas-projects");
                std::fs::create_dir_all(&folder)
                    .map_err(|error| format!("无法创建画布项目目录：{error}"))?;
                folder.join(format!("{}.comp", uuid::Uuid::new_v4()))
            }
        };
        self.save_to_path(&path)
    }

    fn start_auto_save(&mut self) -> Result<(), String> {
        if self.document.is_none() || self.autosave_inflight.is_some() {
            return Ok(());
        }
        let path = match self.project_path.clone() {
            Some(path) => path,
            None => {
                let dirs = concat_host::AppDirs::locate()?;
                let folder = dirs.data.join("canvas-projects");
                std::fs::create_dir_all(&folder)
                    .map_err(|error| format!("无法创建画布项目目录：{error}"))?;
                folder.join(format!("{}.comp", uuid::Uuid::new_v4()))
            }
        };
        let data = self.save_data()?;
        let revision = self.revision;
        let generation = self.document_generation;
        let ticket = canvas_save_ticket(&path);
        let sequence = ticket.fetch_add(1, Ordering::SeqCst) + 1;
        self.project_path = Some(path.clone());
        self.autosave_inflight = Some(revision);
        std::thread::spawn(move || {
            let result =
                Self::write_canvas_snapshot(&data, &path, &ticket, sequence).and_then(|written| {
                    if written {
                        remember_canvas_project(&path)?;
                    }
                    Ok(written)
                });
            let _ = slint::invoke_from_event_loop(move || {
                crate::host::Shell::with(|shell, app| {
                    let mut studio = shell.studio.borrow_mut();
                    let pane = &mut studio.canvas;
                    if pane.document_generation != generation
                        || pane.project_path.as_deref() != Some(path.as_path())
                    {
                        if result.as_ref().is_ok_and(|written| *written) {
                            notify_canvas_registry_changed();
                        }
                        return;
                    }
                    pane.autosave_inflight = None;
                    match result {
                        Ok(true) => {
                            if pane.revision == revision {
                                pane.saved_revision = revision;
                            }
                            notify_canvas_registry_changed();
                            if pane.is_modified() {
                                pane.schedule_auto_save(350);
                            }
                        }
                        Ok(false) => {
                            if pane.is_modified() {
                                pane.schedule_auto_save(350);
                            }
                        }
                        Err(error) => studio.notify(&format!("画布自动保存失败：{error}"), true),
                    }
                    studio.publish(&app, &shell.models);
                });
            });
        });
        Ok(())
    }

    fn schedule_auto_save(&self, delay_ms: u64) {
        let generation = self.document_generation;
        let revision = self.revision;
        slint::Timer::single_shot(std::time::Duration::from_millis(delay_ms), move || {
            crate::host::Shell::with(|shell, _| {
                let mut studio = shell.studio.borrow_mut();
                if studio.canvas.document_generation == generation
                    && studio.canvas.revision == revision
                    && studio.canvas.is_modified()
                {
                    if let Err(error) = studio.canvas.start_auto_save() {
                        studio.notify(&format!("画布自动保存失败：{error}"), true);
                    }
                }
            });
        });
    }

    fn save_data(&self) -> Result<CanvasSaveData, String> {
        Ok(CanvasSaveData {
            document: self
                .document
                .clone()
                .ok_or_else(|| tf("No image open", &[]))?,
            store: self.store.clone(),
            name: self.name.clone(),
        })
    }

    /// Marks the current in-memory canvas as intentionally discarded after
    /// the surrounding clip project has closed successfully.
    pub fn discard_unsaved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Writes the document as a `.comp` package: `manifest.json` - the
    /// format version, the canvas box, and the document tree exactly as
    /// serde sees it - beside `images/<pixel id>.png` for every bitmap the
    /// tree still names, layers and masks alike. The package is staged in
    /// a sibling temporary directory and swapped in, so a failed save
    /// leaves the previous save untouched.
    fn save_comp(&self, path: &Path) -> Result<(), String> {
        let data = self.save_data()?;
        let ticket = canvas_save_ticket(path);
        let sequence = ticket.fetch_add(1, Ordering::SeqCst) + 1;
        if !Self::write_canvas_snapshot(&data, path, &ticket, sequence)? {
            return Err("save: a newer revision superseded this save".into());
        }
        Ok(())
    }

    /// Validates and writes an immutable snapshot away from the window thread.
    fn write_canvas_snapshot(
        data: &CanvasSaveData,
        path: &Path,
        ticket: &Arc<AtomicU64>,
        sequence: u64,
    ) -> Result<bool, String> {
        static SAVE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = SAVE_WRITE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| "save: save worker lock is poisoned".to_owned())?;
        if ticket.load(Ordering::SeqCst) != sequence {
            return Ok(false);
        }
        let document = &data.document;
        // Every bitmap the document names, layers and masks at any depth.
        let mut used = Vec::new();
        document.collect_pixels(&mut used);
        used.sort();
        used.dedup();
        if used.len() > PROJECT_BITMAP_LIMIT
            || checked_frame_bytes(document.width, document.height)
                .unwrap_or(usize::MAX)
                .checked_mul(used.len())
                .is_none_or(|bytes| bytes > PROJECT_PIXEL_LIMIT)
        {
            return Err("save: project pixel data is too large".into());
        }

        let staging = sibling_temp(path);
        let result = (|| {
            std::fs::create_dir_all(staging.join("images")).map_err(|e| format!("save: {e}"))?;
            for id in &used {
                let Some(frame) = data.store.get(*id) else {
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
                "name": data.name,
                "width": document.width,
                "height": document.height,
                "document": document,
            })
            .to_string();
            std::fs::write(staging.join("manifest.json"), manifest)
                .map_err(|e| format!("save: {e}"))?;
            let preview = concat_canvas::compose(document, &data.store);
            let source = image::RgbaImage::from_raw(
                preview.width(),
                preview.height(),
                preview.pixels().to_vec(),
            )
            .ok_or("save: invalid preview pixels")?;
            let thumbnail = image::imageops::thumbnail(&source, 480, 320);
            let frame =
                Frame::from_rgba(thumbnail.width(), thumbnail.height(), thumbnail.into_raw())
                    .ok_or("save: invalid thumbnail pixels")?;
            std::fs::write(staging.join("preview.png"), encode_png(&frame)?)
                .map_err(|e| format!("save: {e}"))?;

            // The swap: staging takes the target's place only after every
            // file is complete.
            if ticket.load(Ordering::SeqCst) != sequence {
                return Ok(false);
            }
            replace_package(&staging, path)?;
            Ok(true)
        })();
        if staging.exists() {
            std::fs::remove_dir_all(&staging).ok();
        }
        result
    }

    /// Reads a `.comp` package back: the manifest's document tree keeps
    /// its ids, every bitmap it names is decoded and restored under the
    /// same id, and the tree is validated before anything is shown. The
    /// returned pixels were never re-minted, so a save of the re-opened
    /// document is byte-for-byte the same tree again.
    fn load_comp(&mut self, path: &Path, texture_limit: Option<u32>) -> Result<(), String> {
        let manifest_path = path.join("manifest.json");
        if std::fs::metadata(&manifest_path)
            .map_err(|e| format!("open: {e}"))?
            .len()
            > PROJECT_FILE_LIMIT
        {
            return Err("open: manifest is too large".into());
        }
        let manifest_bytes = std::fs::read(manifest_path).map_err(|e| format!("open: {e}"))?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).map_err(|e| format!("open: {e}"))?;
        if manifest.get("concat-project").and_then(|v| v.as_u64()) != Some(1) {
            return Err("open: not a concat project".into());
        }
        let document: ImageDocument = serde_json::from_value(
            manifest
                .get("document")
                .cloned()
                .ok_or("open: no document")?,
        )
        .map_err(|e| format!("open: {e}"))?;
        if document.width == 0 || document.height == 0 {
            return Err("open: empty canvas".into());
        }
        validate_canvas_dimensions(document.width, document.height, texture_limit)
            .map_err(|error| format!("open: {error}"))?;
        document.validate().map_err(|e| format!("open: {e}"))?;

        let mut used = Vec::new();
        document.collect_pixels(&mut used);
        used.sort();
        used.dedup();
        if used.len() > PROJECT_BITMAP_LIMIT {
            return Err("open: too many project bitmaps".into());
        }
        let canvas_bytes =
            checked_frame_bytes(document.width, document.height).unwrap_or(usize::MAX);
        if canvas_bytes == 0
            || canvas_bytes
                .checked_mul(used.len())
                .is_none_or(|bytes| bytes > PROJECT_PIXEL_LIMIT)
        {
            return Err("open: project pixel data is too large".into());
        }
        let mut store = PixelStore::new();
        for id in &used {
            let file = path.join("images").join(format!("{}.png", id.as_u64()));
            if std::fs::metadata(&file)
                .map_err(|e| format!("open: {e}"))?
                .len()
                > PROJECT_FILE_LIMIT
            {
                return Err("open: project bitmap is too large".into());
            }
            let bytes = std::fs::read(&file).map_err(|e| format!("open: {e}"))?;
            let (width, height, rgba) = decode_png(&bytes, texture_limit)?;
            if width != document.width || height != document.height {
                return Err("open: project bitmap dimensions do not match canvas".into());
            }
            let frame = Frame::from_rgba(width, height, rgba).ok_or("open: empty image")?;
            store.restore(*id, frame);
        }

        self.document = Some(document);
        self.store = store;
        self.thumbnail_cache.clear();
        self.pixel_revisions.clear();
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
        self.pending_history = None;
        self.revision = self.revision.wrapping_add(1).max(1);
        self.saved_revision = self.revision;
        self.document_generation = self.document_generation.wrapping_add(1).max(1);
        self.autosave_inflight = None;
        self.pending_open = None;
        self.open_confirm = false;
        self.project_path = Some(path.to_owned());
        self.name = manifest["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                path.file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "画布项目".to_owned());
        self.failed = false;
        let (width, height) = self
            .document
            .as_ref()
            .map(|d| (d.width, d.height))
            .expect("just set");
        self.checker = checker_image(width, height);
        if let Some(gpu) = &mut self.gpu {
            gpu.reset_document();
        }
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
        self.refresh_thumbnails();
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
    fn new_blank(&mut self, studio: &mut Studio) {
        if self.is_modified()
            && let Err(error) = self.save_auto()
        {
            studio.notify(&format!("画布保存失败：{error}"), true);
            return;
        }
        let (width, height) = (1920, 1080);
        self.store = PixelStore::new();
        let pixels = self.store.put(Frame::transparent(width, height));
        let mut document = ImageDocument::new(width, height);
        let layer = document.new_layer("图层 1", pixels);
        self.document = Some(document);
        self.layer = Some(pixels);
        self.active = Some(layer);
        self.selection = None;
        self.marquee = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.pending_history = None;
        self.project_path = None;
        self.name = "未命名画布".to_owned();
        self.revision = self.revision.wrapping_add(1).max(1);
        self.document_generation = self.document_generation.wrapping_add(1).max(1);
        self.autosave_inflight = None;
        self.failed = false;
        self.checker = checker_image(width, height);
        if let Some(gpu) = &mut self.gpu {
            gpu.reset_document();
        }
        let size = (width as f64, height as f64);
        self.nav.set_document(Some(size));
        self.nav.viewport_mut().fit(size);
        self.sync_view();
        self.render(studio);
    }

    fn validate_import_paths(&self, paths: &[PathBuf], studio: &mut Studio) -> bool {
        if paths.is_empty() {
            return false;
        }
        let texture_limit = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.device().limits().max_texture_dimension_2d)
            .or_else(|| {
                studio
                    .host
                    .gpu_device
                    .as_ref()
                    .map(|device| device.limits().max_texture_dimension_2d)
            });
        for path in paths {
            if let Err(error) = decode(path, texture_limit) {
                studio.notify(&format!("无法导入 {}：{error}", path.display()), true);
                return false;
            }
        }
        true
    }

    fn import_layers(&mut self, paths: &[PathBuf], studio: &mut Studio) -> bool {
        if paths.is_empty() {
            return false;
        }
        let texture_limit = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.device().limits().max_texture_dimension_2d)
            .or_else(|| {
                studio
                    .host
                    .gpu_device
                    .as_ref()
                    .map(|device| device.limits().max_texture_dimension_2d)
            });
        let mut decoded = Vec::with_capacity(paths.len());
        for path in paths {
            match decode(path, texture_limit) {
                Ok(frame) => decoded.push((path, frame)),
                Err(error) => {
                    studio.notify(&format!("无法导入 {}：{error}", path.display()), true);
                    return false;
                }
            }
        }
        if self.document.is_none() {
            self.open_now(paths[0].as_path(), studio);
            if self.document.is_none() {
                return false;
            }
            decoded.remove(0);
        }
        let (width, height) = self
            .document
            .as_ref()
            .map(|document| (document.width, document.height))
            .expect("checked");
        for (path, frame) in decoded {
            let fit = (width as f64 / frame.width() as f64)
                .min(height as f64 / frame.height() as f64)
                .min(1.0);
            let layer_width = ((frame.width() as f64 * fit).round() as u32).max(1);
            let layer_height = ((frame.height() as f64 * fit).round() as u32).max(1);
            let source =
                image::RgbaImage::from_raw(frame.width(), frame.height(), frame.pixels().to_vec())
                    .expect("decoded RGBA frame");
            let fitted = if layer_width == frame.width() && layer_height == frame.height() {
                source
            } else {
                image::imageops::resize(
                    &source,
                    layer_width,
                    layer_height,
                    image::imageops::FilterType::Lanczos3,
                )
            };
            let mut canvas = Frame::transparent(width, height);
            let x = (width - layer_width) as usize / 2;
            let y = (height - layer_height) as usize / 2;
            for row in 0..layer_height as usize {
                let destination = ((y + row) * width as usize + x) * 4;
                let source = row * layer_width as usize * 4;
                canvas.pixels_mut()[destination..destination + layer_width as usize * 4]
                    .copy_from_slice(&fitted.as_raw()[source..source + layer_width as usize * 4]);
            }
            let pixels = self.store.put(canvas);
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let id = self
                .document
                .as_mut()
                .expect("checked")
                .new_layer(name, pixels);
            self.active = Some(id);
            self.layer = Some(pixels);
        }
        self.render(studio);
        true
    }

    fn request_open(&mut self, path: &Path, studio: &mut Studio) {
        self.brush_release();
        if self.is_modified() {
            self.pending_open = Some(path.to_owned());
            self.open_confirm = true;
            return;
        }
        self.open_now(path, studio);
    }

    fn open_now(&mut self, path: &Path, studio: &mut Studio) {
        let texture_limit = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.device().limits().max_texture_dimension_2d)
            .or_else(|| {
                studio
                    .host
                    .gpu_device
                    .as_ref()
                    .map(|device| device.limits().max_texture_dimension_2d)
            });
        if path.is_dir() {
            match self.load_comp(path, texture_limit) {
                Ok(()) => {
                    self.failed = false;
                    self.render(studio);
                    if let Err(error) = remember_canvas_project(path) {
                        studio.notify(&error, true);
                    }
                    notify_canvas_registry_changed();
                    if let Some(paths) = self.pending_handoff_paths.take() {
                        self.begin_history_mode(0);
                        let success = self.import_layers(&paths, studio);
                        self.commit_history();
                        report_canvas_handoff(
                            success,
                            if success {
                                ""
                            } else {
                                "画布素材导入失败，请重试"
                            },
                        );
                    }
                }
                Err(error) => {
                    if self.pending_handoff_paths.take().is_some() {
                        report_canvas_handoff(false, &error);
                    }
                    log::warn!("canvas: {error}");
                    if !self.failed {
                        self.failed = true;
                        studio.notify(&tf("Could not open {0}", &[&error]), true);
                    }
                }
            }
            return;
        }
        match decode(path, texture_limit) {
            Ok(frame) => {
                let (width, height) = (frame.width(), frame.height());
                let mut document = ImageDocument::new(width, height);
                self.store = PixelStore::new();
                self.thumbnail_cache.clear();
                self.pixel_revisions.clear();
                if let Some(gpu) = &mut self.gpu {
                    gpu.reset_document();
                }
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
                self.pending_history = None;
                self.revision = self.revision.wrapping_add(1).max(1);
                self.saved_revision = 0;
                self.document_generation = self.document_generation.wrapping_add(1).max(1);
                self.autosave_inflight = None;
                self.pending_open = None;
                self.open_confirm = false;
                self.project_path = None;
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
        let texture_limit = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.device().limits().max_texture_dimension_2d)
            .or_else(|| {
                studio
                    .host
                    .gpu_device
                    .as_ref()
                    .map(|device| device.limits().max_texture_dimension_2d)
            });
        if let Err(error) =
            validate_canvas_dimensions(document.width, document.height, texture_limit)
        {
            log::warn!("canvas: {error}");
            if !self.failed {
                self.failed = true;
                studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
            }
            return;
        }
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
        self.brush.opacity = opacity.clamp(0.01, 1.0);
        self.brush.hardness = hardness.clamp(0.0, 1.0);
        if let Some(color) = PALETTE.get(palette) {
            self.brush.color = *color;
            self.brush.erasing = false;
            self.brush_color_index = palette as i32;
        }
    }

    /// Sets a custom RGB colour through the same path as the colour controls.
    pub fn agent_set_brush_rgb(&mut self, red: u8, green: u8, blue: u8) {
        self.set_brush_rgb(f64::from(red), f64::from(green), f64::from(blue));
    }

    /// Sets a custom hexadecimal colour, preserving the previous value when
    /// the input is malformed.
    pub fn agent_set_brush_hex(&mut self, text: &str) {
        self.set_brush_hex(text);
    }

    /// Samples a composited document pixel without creating history.
    pub fn agent_pick_color(&mut self, x: f64, y: f64) {
        self.pick_color_document(x, y);
    }

    /// Paints one stroke through the points, in document pixels: the same
    /// press-move-release a pointer makes, with the provisional tail and
    /// the curve settle exactly where the hand leaves them.
    pub fn agent_paint_stroke(&mut self, points: &[(f64, f64)]) {
        let mut points = points.iter().copied().peekable();
        let Some(first) = points.next() else {
            return;
        };
        self.brush_press_document(first.0, first.1);
        for (x, y) in points {
            let Some(document) = self.document.as_ref() else {
                continue;
            };
            let radius = self.brush.diameter / 2.0;
            let (w, h) = (f64::from(document.width), f64::from(document.height));
            if x < -radius || y < -radius || x > w + radius || y > h + radius {
                continue;
            }
            let changed = self.paint_at(x, y);
            self.commit_tiles(&changed);
        }
        self.brush_release();
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
        self.wand_document(x, y);
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
        self.history_edit(|pane| pane.edit_selection(EditKind::Fill));
    }

    pub fn agent_delete_selection(&mut self) {
        self.history_edit(|pane| pane.edit_selection(EditKind::Delete));
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
        self.history_edit(|pane| {
            let target = pane
                .rows()
                .get(row.max(0) as usize)
                .map(|(node, _)| node.id());
            let Some(id) = target else { return };
            let Some(document) = pane.document.as_mut() else {
                return;
            };
            match document.find_mut(id) {
                Some(LayerNode::Layer(layer)) => layer.hidden = !layer.hidden,
                Some(LayerNode::Group(group)) => group.hidden = !group.hidden,
                Some(LayerNode::Adjustment(adjustment)) => adjustment.hidden = !adjustment.hidden,
                None => {}
            }
            pane.sync_view();
        });
    }

    pub fn agent_layer_opacity(&mut self, row: i32, opacity: f32) {
        self.history_edit(|pane| {
            let target = pane
                .rows()
                .get(row.max(0) as usize)
                .map(|(node, _)| node.id());
            let Some(id) = target else { return };
            let Some(document) = pane.document.as_mut() else {
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
        });
    }

    pub fn agent_layer_add(&mut self) {
        self.history_edit(|pane| {
            let Some(document) = pane.document.as_ref() else {
                return;
            };
            let (w, h) = (document.width, document.height);
            let count = document.walk().len();
            let pixels = pane.store.put(Frame::transparent(w, h));
            let name = tf("Layer {0}", &[&count.to_string()]).to_string();
            let id = pane
                .document
                .as_mut()
                .expect("checked")
                .new_layer(name, pixels);
            pane.active = Some(id);
            pane.layer = Some(pixels);
            pane.sync_view();
        });
    }

    pub fn agent_layer_delete(&mut self, row: i32) {
        self.history_edit(|pane| pane.delete_row(row));
        self.sync_view();
    }

    pub fn agent_layer_move(&mut self, row: i32, direction: i32) {
        self.history_edit(|pane| {
            let target = pane
                .rows()
                .get(row.max(0) as usize)
                .map(|(node, _)| node.id());
            let Some(id) = target else { return };
            pane.move_within_container(id, direction);
            pane.sync_view();
        });
    }

    /// Writes the composed canvas to `path` as PNG - the export without
    /// the save dialog, which an unattended run cannot answer.
    pub fn agent_export_png(&mut self, path: &Path) -> Result<(), String> {
        let Some(document) = self.document.clone() else {
            return Err(tf("No image open", &[]));
        };
        validate_canvas_dimensions(
            document.width,
            document.height,
            self.gpu
                .as_ref()
                .map(|gpu| gpu.device().limits().max_texture_dimension_2d),
        )?;
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
    /// carries a mask, whether that mask is the one being painted, and
    /// whether that mask is enabled.
    pub fn agent_layers(&self) -> Vec<LayerRow> {
        self.layers_data()
    }

    /// Adds a group above the active row - the panel's "new group"
    /// without the panel.
    pub fn agent_layer_group(&mut self) {
        self.history_edit(|pane| pane.add_group());
    }

    /// Moves the node at `row` into the group at `into`, appended at the
    /// back of that group's children (its front in the panel). `into` of
    /// `None` sends the node back to the root. The engine's own
    /// `move_node` does the walking; the row indexes are the panel's.
    pub fn agent_layer_move_into(&mut self, row: i32, into: Option<i32>) {
        self.begin_history();
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
        if into.is_some() && target.is_none() {
            self.pending_history = None;
            return;
        }
        drop(rows);
        let Some(moving) = moving else {
            self.pending_history = None;
            return;
        };
        let Some(document) = self.document.as_mut() else {
            self.pending_history = None;
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
        self.commit_history();
    }

    /// The panel drag, without the panel: the row `source` dropped onto
    /// the row `target`, into the target when `below` says so and the
    /// target is a group, beside it otherwise. The same resolution the
    /// gesture's drop gets.
    pub fn agent_layer_drop(&mut self, source: i32, target: i32, below: bool) {
        self.history_edit(|pane| pane.move_row_onto(source, target, below));
    }

    /// A white mask over the row's node - the panel's "add mask" without
    /// the panel - and the painting tools pointed at it.
    pub fn agent_layer_mask_add(&mut self, row: i32) {
        self.begin_history();
        let picked = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| (node.id(), node_image_pixels(node)));
        if let Some((id, pixels)) = picked {
            self.active = Some(id);
            self.layer = pixels;
            self.add_mask();
        }
        self.commit_history();
    }

    /// The row's node's mask, gone.
    pub fn agent_layer_mask_remove(&mut self, row: i32) {
        self.begin_history();
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        if let Some(id) = target {
            self.active = Some(id);
            self.remove_mask();
        }
        self.commit_history();
    }

    /// The row's node's mask applied or set aside, whole.
    pub fn agent_layer_mask_toggle(&mut self, row: i32) {
        self.begin_history();
        let target = self
            .rows()
            .get(row.max(0) as usize)
            .map(|(node, _)| node.id());
        if let Some(id) = target {
            self.active = Some(id);
            self.toggle_mask();
        }
        self.commit_history();
    }

    /// Whether the painting tools are on the active row's mask - the
    /// mode the mask chip toggles.
    pub fn agent_paint_mask(&self) -> bool {
        self.paint_mask
    }

    /// Sets the active gradient map's low (`0`) or high (`1`) colour
    /// from the tray palette's `index`.
    pub fn agent_gradient_color(&mut self, slot: i32, index: i32) {
        self.history_edit(|pane| pane.set_gradient_color(slot, index));
    }

    /// Folds or unfolds the group at `row`, the way the panel's chevron
    /// does. A row that is not a group does nothing.
    pub fn agent_layer_fold(&mut self, row: i32) {
        let fold = self
            .rows()
            .get(row.max(0) as usize)
            .and_then(|(node, _)| match node {
                LayerNode::Group(group) => Some(group.id),
                _ => None,
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
        self.history_edit(|pane| pane.set_curve_point(channel, index, x, y));
    }

    /// Inserts a curves control point on `channel` at `(x, y)`.
    pub fn agent_curve_add(&mut self, channel: i32, x: f64, y: f64) {
        self.history_edit(|pane| pane.add_curve_point(channel, x, y));
    }

    /// Drops the curves control point `index` on `channel`; the corners
    /// refuse to leave.
    pub fn agent_curve_remove(&mut self, channel: i32, index: i32) {
        self.history_edit(|pane| pane.remove_curve_point(channel, index));
    }

    /// The brush as it is set right now.
    pub fn agent_brush(&self) -> concat_canvas::BrushSettings {
        self.brush
    }

    /// Adds an adjustment of `kind` above the active layer - the kinds
    /// [`ADJUSTMENT_KINDS`] numbers, `1` Invert through `7` Curves - and
    /// makes it the active row.
    pub fn agent_adjustment_add(&mut self, kind: i32) {
        self.history_edit(|pane| pane.add_adjustment(kind));
    }

    /// Sets the active adjustment's parameter `index` to `value`, the
    /// order [`Self::adjustment_state`] publishes them in.
    pub fn agent_adjustment_param(&mut self, index: i32, value: f32) {
        self.history_edit(|pane| pane.set_adjustment_param(index, value));
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
        self.save_to_path(path)
    }

    /// Opens a `.comp` project package, or an image, from `path` - the
    /// open without the dialog.
    pub fn agent_open(&mut self, path: &Path) -> Result<(), String> {
        self.brush_release();
        if self.is_modified() {
            self.pending_open = Some(path.to_owned());
            self.open_confirm = true;
            return Err("open: current canvas has unsaved changes".into());
        }
        if path.is_dir() {
            self.load_comp(
                path,
                self.gpu
                    .as_ref()
                    .map(|gpu| gpu.device().limits().max_texture_dimension_2d),
            )
        } else {
            match decode(
                path,
                self.gpu
                    .as_ref()
                    .map(|gpu| gpu.device().limits().max_texture_dimension_2d),
            ) {
                Ok(frame) => {
                    let (width, height) = (frame.width(), frame.height());
                    let mut document = ImageDocument::new(width, height);
                    self.store = PixelStore::new();
                    self.thumbnail_cache.clear();
                    self.pixel_revisions.clear();
                    if let Some(gpu) = &mut self.gpu {
                        gpu.reset_document();
                    }
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
                    self.stroke = None;
                    self.stroke_base = None;
                    self.stroke_scratch = None;
                    self.stroke_target = None;
                    self.stroke_on_mask = false;
                    self.undo_stack.clear();
                    self.redo_stack.clear();
                    self.pending_history = None;
                    self.revision = self.revision.wrapping_add(1).max(1);
                    self.saved_revision = self.revision;
                    self.document_generation = self.document_generation.wrapping_add(1).max(1);
                    self.autosave_inflight = None;
                    self.pending_open = None;
                    self.open_confirm = false;
                    self.project_path = None;
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

/// Produces a small nearest-neighbour preview without touching the full
/// canvas. Masks are displayed as opaque grayscale so a black mask is still
/// legible against the panel's dark surface.
fn frame_thumbnail(frame: &Frame, width: u32, height: u32, mask: bool) -> slint::Image {
    let source_width = frame.width().max(1);
    let source_height = frame.height().max(1);
    let scale = (f64::from(width) / f64::from(source_width))
        .min(f64::from(height) / f64::from(source_height));
    let width = (f64::from(source_width) * scale).round().max(1.0) as u32;
    let height = (f64::from(source_height) * scale).round().max(1.0) as u32;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let source_x = x * source_width / width.max(1);
            let source_y = y * source_height / height.max(1);
            let mut rgba = frame
                .pixel(
                    source_x.min(source_width - 1),
                    source_y.min(source_height - 1),
                )
                .unwrap_or([0, 0, 0, 0]);
            if mask {
                let coverage = rgba[0];
                rgba = [coverage, coverage, coverage, 255];
            }
            let offset = ((y * width + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&rgba);
        }
    }
    let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width, height);
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

/// Applies selection coverage to freshly composited brush tiles. Outside the
/// selection the pre-stroke bytes are restored; a soft edge interpolates
/// between the base and painted result.
fn restrict_to_selection(
    frame: &mut Frame,
    base: &Frame,
    selection: &Mask,
    changed: &[(usize, usize)],
) {
    let (width, height) = (frame.width(), frame.height());
    let pixels = frame.pixels_mut();
    for &(tx, ty) in changed {
        let x0 = (tx * TILE) as u32;
        let y0 = (ty * TILE) as u32;
        let x1 = (x0 + TILE as u32).min(width);
        let y1 = (y0 + TILE as u32).min(height);
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = u16::from(selection.at(x, y));
                if coverage == 255 {
                    continue;
                }
                let offset = ((y * width + x) * 4) as usize;
                for channel in 0..4 {
                    let old = u16::from(base.pixels()[offset + channel]);
                    let painted = u16::from(pixels[offset + channel]);
                    pixels[offset + channel] =
                        ((old * (255 - coverage) + painted * coverage + 127) / 255) as u8;
                }
            }
        }
    }
}

/// Raster masks use one canonical coverage value in RGB and opaque alpha.
/// Brush erasing and selection deletion lower alpha in the generic painter;
/// folding alpha into red makes those public operations actually clear mask
/// coverage for both CPU and GPU compositors.
fn normalize_mask_pixels(frame: &mut Frame) {
    for pixel in frame.pixels_mut().chunks_exact_mut(4) {
        let coverage = ((u16::from(pixel[0]) * u16::from(pixel[3]) + 127) / 255) as u8;
        pixel.copy_from_slice(&[coverage, coverage, coverage, 255]);
    }
}

fn normalize_mask_tiles(frame: &mut Frame, changed: &[(usize, usize)]) {
    let (width, height) = (frame.width(), frame.height());
    let pixels = frame.pixels_mut();
    for &(tx, ty) in changed {
        let x0 = (tx * TILE) as u32;
        let y0 = (ty * TILE) as u32;
        let x1 = (x0 + TILE as u32).min(width);
        let y1 = (y0 + TILE as u32).min(height);
        for y in y0..y1 {
            for x in x0..x1 {
                let offset = ((y * width + x) * 4) as usize;
                let coverage =
                    ((u16::from(pixels[offset]) * u16::from(pixels[offset + 3]) + 127) / 255) as u8;
                pixels[offset..offset + 4].copy_from_slice(&[coverage, coverage, coverage, 255]);
            }
        }
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

/// Finds the direct parent container and child position of `id`. The root is
/// represented by `None`; nested groups carry their own stable layer id.
fn node_parent_slot(
    document: &ImageDocument,
    id: concat_canvas::LayerId,
) -> Option<(Option<concat_canvas::LayerId>, usize)> {
    fn walk(
        group: &concat_canvas::LayerGroup,
        id: concat_canvas::LayerId,
        is_root: bool,
    ) -> Option<(Option<concat_canvas::LayerId>, usize)> {
        if let Some(position) = group.children.iter().position(|child| child.id() == id) {
            return Some(((!is_root).then_some(group.id), position));
        }
        for child in &group.children {
            if let LayerNode::Group(nested) = child
                && let Some(found) = walk(nested, id, false)
            {
                return Some(found);
            }
        }
        None
    }
    walk(&document.root, id, true)
}

/// Returns the mutable child vector for a parent container. `None` means the
/// document root, which is a group too but is not a `LayerNode`.
fn children_for_parent_mut(
    document: &mut ImageDocument,
    parent: Option<concat_canvas::LayerId>,
) -> Option<&mut Vec<LayerNode>> {
    match parent {
        None => Some(&mut document.root.children),
        Some(id) => document.group_mut(id).map(|group| &mut group.children),
    }
}

fn topmost_image_in_parent(
    document: &ImageDocument,
    parent: Option<concat_canvas::LayerId>,
) -> Option<PixelId> {
    let children = parent
        .and_then(|id| document.find(id))
        .and_then(|node| match node {
            LayerNode::Group(group) => Some(&group.children),
            _ => None,
        })
        .unwrap_or(&document.root.children);
    children.iter().rev().find_map(node_image_pixels)
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
    let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width, height);
    slint::Image::from_rgba8(buffer)
}

/// A temporary directory beside `path`, unique to this process: the
/// staging ground of an atomic package save.
fn canvas_registry_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        Ok(std::env::temp_dir().join(format!(
            "concat-canvas-projects-{}.json",
            std::process::id()
        )))
    }
    #[cfg(not(test))]
    {
        concat_host::AppDirs::locate().map(|dirs| dirs.data.join("canvas-projects.json"))
    }
}

fn canvas_save_ticket(path: &Path) -> Arc<AtomicU64> {
    static TICKETS: OnceLock<Mutex<HashMap<PathBuf, Arc<AtomicU64>>>> = OnceLock::new();
    let mut tickets = TICKETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("canvas save ticket lock poisoned");
    tickets
        .entry(path.to_owned())
        .or_insert_with(|| Arc::new(AtomicU64::new(0)))
        .clone()
}

fn notify_canvas_registry_changed() {
    crate::host::Shell::with(|_, app| {
        app.global::<crate::ui::SeeCut>()
            .invoke_action("canvas-projects-refresh".into(), "".into());
    });
}

fn report_canvas_handoff(success: bool, detail: &str) {
    crate::host::Shell::with(|_, app| {
        app.global::<crate::ui::SeeCut>().invoke_action(
            if success {
                "handoff-complete"
            } else {
                "handoff-failed"
            }
            .into(),
            detail.into(),
        );
    });
}

pub(crate) fn canvas_recent_paths() -> Result<Vec<PathBuf>, String> {
    let path = canvas_registry_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("无法读取画布项目索引：{error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("画布项目索引已损坏，请先备份：{error}"))
}

fn remember_canvas_project(path: &Path) -> Result<(), String> {
    static REGISTRY_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = REGISTRY_WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "画布项目索引锁已损坏".to_owned())?;
    let registry = canvas_registry_path()?;
    let mut paths = canvas_recent_paths()?;
    paths.retain(|entry| entry != path);
    paths.insert(0, path.to_owned());
    paths.truncate(100);
    if let Some(parent) = registry.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法保存画布项目索引：{error}"))?;
    }
    let temporary = registry.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec(&paths).map_err(|error| format!("无法生成画布项目索引：{error}"))?;
    std::fs::write(&temporary, bytes).map_err(|error| format!("无法保存画布项目索引：{error}"))?;
    std::fs::rename(&temporary, &registry)
        .map_err(|error| format!("无法更新画布项目索引：{error}"))?;
    Ok(())
}

fn sibling_temp(path: &Path) -> std::path::PathBuf {
    unique_sibling(path, "tmp")
}

fn checked_frame_bytes(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

fn unique_sibling(path: &Path, kind: &str) -> std::path::PathBuf {
    static NEXT_SAVE: AtomicU64 = AtomicU64::new(1);
    loop {
        let suffix = NEXT_SAVE.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_extension(format!("{kind}-{}-{suffix}", std::process::id()));
        if !candidate.exists() {
            return candidate;
        }
    }
}

/// Moves `staging` onto `target`: a previous package steps aside first,
/// the staging takes its place, and only then is the old one dropped. A
/// failure on the way puts the previous package back, so a save either
/// lands whole or leaves what was there.
fn replace_package(staging: &Path, target: &Path) -> Result<(), String> {
    let aside = unique_sibling(target, "backup");
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
fn decode(path: &Path, texture_limit: Option<u32>) -> Result<Frame, String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > PROJECT_FILE_LIMIT {
        return Err("image file is too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path
        .extension()
        .map(|e| e.to_ascii_lowercase().to_string_lossy().into_owned())
        .unwrap_or_default();
    let (width, height, pixels) = match name.as_str() {
        "png" => decode_png(&bytes, texture_limit)?,
        _ => {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            let dimension_limit = texture_limit.unwrap_or(DEFAULT_IMAGE_DIMENSION_LIMIT);
            limits.max_image_width = Some(dimension_limit);
            limits.max_image_height = Some(dimension_limit);
            limits.max_alloc = Some(PROJECT_PIXEL_LIMIT as u64);
            reader.limits(limits);
            let image = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
            let (width, height) = image.dimensions();
            (width, height, image.into_raw())
        }
    };
    Frame::from_rgba(width, height, pixels).ok_or_else(|| "empty image".to_owned())
}

/// PNG to RGBA, any of the colour types a still is likely to come in.
/// Returns `(width, height, rgba)`.
fn decode_png(bytes: &[u8], texture_limit: Option<u32>) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: PROJECT_PIXEL_LIMIT,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    validate_canvas_dimensions(width, height, texture_limit)?;
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

fn validate_canvas_dimensions(
    width: u32,
    height: u32,
    texture_limit: Option<u32>,
) -> Result<(), String> {
    let limit = texture_limit.unwrap_or(DEFAULT_IMAGE_DIMENSION_LIMIT);
    if width == 0 || height == 0 {
        return Err("empty canvas".into());
    }
    if width > limit || height > limit {
        return Err(format!(
            "canvas {width}x{height} exceeds this device's {limit}px texture limit"
        ));
    }
    Ok(())
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
    fn custom_colour_input_rejects_invalid_hex_and_picks_composite_without_painting() {
        let (mut pane, pixels) = painting_pane();
        pane.agent_set_brush_rgb(12, 34, 56);
        assert_eq!(pane.brush.color, [12, 34, 56]);
        pane.agent_set_brush_hex("#0A141E");
        assert_eq!(pane.brush.color, [10, 20, 30]);
        pane.agent_set_brush_hex("not-a-colour");
        assert_eq!(
            pane.brush.color,
            [10, 20, 30],
            "invalid text keeps the last colour"
        );

        let mut frame = Frame::transparent(300, 200);
        frame.set_pixel(150, 100, [201, 102, 43, 255]);
        pane.store.replace(pixels, frame);
        let mut overlay = Frame::transparent(300, 200);
        overlay.set_pixel(150, 100, [11, 22, 33, 255]);
        let overlay_pixels = pane.store.put(overlay);
        pane.document
            .as_mut()
            .expect("document")
            .new_layer("Overlay", overlay_pixels);
        // Agent coordinates stay in document space even when the viewport differs.
        pane.nav
            .viewport_mut()
            .resize((900.0, 500.0), 1.0, Some((300.0, 200.0)));
        pane.agent_pick_color(150.0, 100.0);
        assert_eq!(
            pane.brush.color,
            [11, 22, 33],
            "sample includes the layer above the active layer"
        );
        pane.agent_pick_color(f64::NAN, 0.0);
        assert_eq!(pane.brush.color, [11, 22, 33]);
        assert!(
            pane.undo_stack.is_empty(),
            "sampling does not create a stroke"
        );
    }

    #[test]
    fn thumbnails_wait_for_the_committed_pixel_revision_during_a_stroke() {
        let (mut pane, pixels) = painting_pane();
        pane.refresh_thumbnails();
        let initial = pane.thumbnail_cache.get(&pixels).expect("layer thumbnail");
        let initial_revision = initial.revision;
        let initial_source = initial.source.clone();

        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.refresh_thumbnails();
        let during = pane
            .thumbnail_cache
            .get(&pixels)
            .expect("cached layer thumbnail");
        assert_eq!(during.revision, initial_revision);
        assert!(
            during.source.ptr_eq(&initial_source),
            "pointer moves do not resample the row"
        );

        pane.brush_release();
        pane.refresh_thumbnails();
        let after = pane
            .thumbnail_cache
            .get(&pixels)
            .expect("updated layer thumbnail");
        assert!(
            after.revision > initial_revision,
            "release advances the pixel revision"
        );
    }

    #[test]
    fn thumbnail_sources_update_independently_and_preserve_aspect_ratio() {
        let (mut pane, pixels) = painting_pane();
        pane.agent_layer_add();
        let other = pane.layer.expect("second layer");
        pane.refresh_thumbnails();
        let other_source = pane.thumbnail_cache[&other].source.clone();
        let initial_source = pane.thumbnail_cache[&pixels].source.clone();
        pane.store.replace(pixels, Frame::transparent(20, 40));
        pane.refresh_thumbnails();
        assert!(pane.thumbnail_cache[&other].source.ptr_eq(&other_source));
        assert!(!pane.thumbnail_cache[&pixels].source.ptr_eq(&initial_source));
        let size = pane.thumbnail_cache[&pixels].image.size();
        assert_eq!((size.width, size.height), (14, 28));
    }

    /// Builds the smallest nested tree used by layer operation regressions:
    /// one group with two direct image children and no unrelated root image.
    fn grouped_painting_pane() -> CanvasPane {
        let (mut pane, _) = painting_pane();
        pane.agent_layer_add();
        pane.agent_layer_group();

        for _ in 0..2 {
            let rows = pane.agent_layers();
            let group = rows.iter().position(|row| row.7).expect("group row");
            let source = rows
                .iter()
                .enumerate()
                .find(|(_, row)| row.5 == 0 && !row.7)
                .map(|(index, _)| index)
                .expect("root image row");
            pane.agent_layer_move_into(source as i32, Some(group as i32));
        }
        pane
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
    fn a_stroke_copies_its_base_once_and_keeps_that_snapshot_shared() {
        let (mut pane, _) = painting_pane();
        pane.set_tool(3);
        pane.brush_press_document(40.0, 40.0);
        let base = pane.stroke_base.as_ref().expect("base");
        assert_eq!(
            Arc::strong_count(base),
            2,
            "store and stroke share the base"
        );
        let base_id = base.id();
        for point in [(60.0, 50.0), (90.0, 70.0), (120.0, 90.0)] {
            let changed = pane.paint_at(point.0, point.1);
            pane.commit_tiles(&changed);
            let base = pane.stroke_base.as_ref().expect("base stays");
            assert_eq!(base.id(), base_id);
            assert_eq!(Arc::strong_count(base), 2, "moves did not clone the base");
        }
        pane.brush_release();
    }

    #[test]
    fn release_commits_the_same_settled_curve_as_the_brush_engine() {
        let points = [(40.0, 40.0), (80.0, 55.0), (130.0, 110.0), (180.0, 90.0)];
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush.diameter = 36.0;
        pane.agent_paint_stroke(&points);
        let actual = pane.store.get(pixels).expect("painted");

        let mut stroke = BrushStroke::new(300, 200, pane.brush).expect("stroke");
        let mut changed = Vec::new();
        for point in points {
            changed.extend(stroke.append(point));
        }
        changed.extend(stroke.flush());
        changed.sort_unstable();
        changed.dedup();
        let mut expected = Frame::transparent(300, 200);
        stroke.composite_tiles(expected.pixels_mut(), &changed);
        assert_eq!(actual.pixels(), expected.pixels());
    }

    #[test]
    fn a_brush_stroke_is_clipped_to_the_selection() {
        let (mut pane, pixels) = painting_pane();
        pane.agent_select_rect(100.0, 80.0, 20.0, 40.0);
        pane.set_tool(3);
        pane.brush.diameter = 80.0;
        pane.brush_press_document(110.0, 100.0);
        pane.brush_release();
        let frame = pane.store.get(pixels).expect("pixels");
        assert_eq!(frame.pixel(110, 100).expect("inside")[3], 255);
        assert_eq!(frame.pixel(80, 100).expect("outside")[3], 0);
        assert_eq!(frame.pixel(140, 100).expect("outside")[3], 0);
    }

    #[test]
    fn document_and_pixel_edits_share_one_ordered_history() {
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        pane.agent_layer_toggle_visibility(0);
        assert!(pane.document.as_ref().unwrap().walk()[0].hidden());

        pane.undo();
        assert!(!pane.document.as_ref().unwrap().walk()[0].hidden());
        assert!(pane.store.get(pixels).unwrap().pixel(150, 100).unwrap()[3] > 0);
        pane.undo();
        assert_eq!(
            pane.store.get(pixels).unwrap().pixel(150, 100).unwrap()[3],
            0
        );
    }

    #[test]
    fn history_enforces_entry_and_retained_byte_limits() {
        let (mut pane, _) = painting_pane();
        for _ in 0..(HISTORY_ENTRY_LIMIT + 20) {
            pane.agent_layer_toggle_visibility(0);
        }
        assert_eq!(pane.undo_stack.len(), HISTORY_ENTRY_LIMIT);
        pane.undo_stack[0].retained_bytes = HISTORY_BYTE_LIMIT + 1;
        pane.trim_history();
        assert!(pane.undo_stack.len() < HISTORY_ENTRY_LIMIT);
        assert!(
            pane.undo_stack
                .iter()
                .map(|entry| entry.retained_bytes)
                .sum::<usize>()
                <= HISTORY_BYTE_LIMIT
        );
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
    fn agent_wand_uses_document_pixels_under_an_offset_zoomed_view() {
        let (mut pane, pixels) = painting_pane();
        let mut frame = Frame::transparent(300, 200);
        for y in 0..200 {
            for x in 0..300 {
                let color = if x < 100 {
                    [240, 20, 20, 255]
                } else {
                    [20, 20, 240, 255]
                };
                frame.set_pixel(x, y, color);
            }
        }
        pane.store.replace(pixels, frame);
        pane.nav
            .viewport_mut()
            .set_zoom(2.0, (150.0, 100.0), (300.0, 200.0));
        pane.nav.viewport_mut().translate((35.0, -20.0));

        pane.agent_wand(20.0, 20.0);
        assert_eq!(pane.agent_selection_bounds(), Some((0, 0, 100, 200)));
    }

    #[test]
    fn an_agent_export_writes_a_decodable_png() {
        let (mut pane, _) = painting_pane();
        let dir = std::env::temp_dir().join("concat-agent-export");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent.png");
        pane.agent_export_png(&path).expect("the export wrote");

        let bytes = std::fs::read(&path).expect("the file");
        let (width, height, rgba) = decode_png(&bytes, None).expect("a decodable png");
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
    fn png_decoder_expands_palette_and_strips_sixteen_bit_channels() {
        let mut indexed = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut indexed, 2, 1);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(vec![10, 20, 30, 200, 210, 220]);
            encoder.set_trns(vec![255, 128]);
            let mut writer = encoder.write_header().expect("indexed header");
            writer.write_image_data(&[0, 1]).expect("indexed pixels");
        }
        let (_, _, rgba) = decode_png(&indexed, None).expect("indexed png");
        assert_eq!(rgba, vec![10, 20, 30, 255, 200, 210, 220, 128]);

        let mut sixteen = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut sixteen, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Sixteen);
            let mut writer = encoder.write_header().expect("16-bit header");
            writer
                .write_image_data(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc])
                .expect("16-bit pixels");
        }
        let (_, _, rgba) = decode_png(&sixteen, None).expect("16-bit png");
        assert_eq!(rgba, vec![0x12, 0x56, 0x9a, 255]);
    }

    #[test]
    fn device_texture_limit_rejects_a_thin_canvas_before_gpu_upload() {
        assert!(validate_canvas_dimensions(16_384, 1, Some(16_384)).is_ok());
        let error = validate_canvas_dimensions(16_385, 1, Some(16_384)).unwrap_err();
        assert!(error.contains("16384px texture limit"));
    }

    #[test]
    fn project_open_checks_the_device_limit_before_replacing_the_document() {
        let dir = std::env::temp_dir().join(format!("concat-thin-comp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let document = ImageDocument::new(16_385, 1);
        let manifest = serde_json::json!({
            "concat-project": 1,
            "width": document.width,
            "height": document.height,
            "document": document,
        });
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).expect("manifest");

        let (mut pane, _) = painting_pane();
        let error = pane.load_comp(&dir, Some(16_384)).unwrap_err();
        assert!(error.contains("16384px texture limit"));
        assert_eq!(pane.document_size(), Some((300.0, 200.0)));
        let _ = std::fs::remove_dir_all(dir);
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
            pane.document
                .as_ref()
                .expect("open")
                .find(pane.active.expect("active")),
            Some(LayerNode::Layer(_))
        ));
    }

    #[test]
    fn imported_images_render_on_the_actual_shared_window_device() {
        let shared = crate::gpu::Gpu::acquire().expect("shared window GPU is required");
        println!(
            "shared window adapter: {:?}; features: {:?}",
            shared.adapter.get_info(),
            shared.device.features()
        );
        assert!(
            !shared
                .device
                .features()
                .contains(wgpu::Features::CLEAR_TEXTURE)
        );
        let dir =
            std::env::temp_dir().join(format!("concat-window-gpu-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("isolated image fixtures");
        let mut pane = CanvasPane {
            gpu: Some(CanvasGpu::with_device(shared.device, shared.queue)),
            ..CanvasPane::default()
        };
        for (index, (extension, format)) in [
            ("png", image::ImageFormat::Png),
            ("jpg", image::ImageFormat::Jpeg),
            ("webp", image::ImageFormat::WebP),
            ("bmp", image::ImageFormat::Bmp),
        ]
        .into_iter()
        .enumerate()
        {
            let width = 13 + index as u32;
            let height = 9 + index as u32;
            let image = image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([(x * 11) as u8, (y * 17) as u8, 40 + index as u8 * 30])
            });
            let path = dir.join(format!("import.{extension}"));
            if extension == "png" {
                let mut frame = Frame::transparent(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let rgb = image.get_pixel(x, y).0;
                        frame.set_pixel(x, y, [rgb[0], rgb[1], rgb[2], 255]);
                    }
                }
                std::fs::write(&path, encode_png(&frame).expect("encode PNG fixture"))
                    .expect("write PNG");
            } else {
                image::DynamicImage::ImageRgb8(image)
                    .save_with_format(&path, format)
                    .expect("encode image fixture");
            }
            pane.agent_open(&path).expect("decode and open image");
            let document = pane.document.as_ref().expect("opened document");
            let expected = concat_canvas::compose(document, &pane.store);
            let actual = pane
                .gpu
                .as_mut()
                .expect("shared compositor")
                .compose_frame(document, &pane.store);
            assert_eq!((actual.width(), actual.height()), (width, height));
            assert!(
                actual
                    .pixels()
                    .iter()
                    .zip(expected.pixels())
                    .all(|(a, b)| (i16::from(*a) - i16::from(*b)).abs() <= 1),
                "{extension} GPU image differs from decoded CPU image"
            );
            assert_eq!(pane.name, format!("import.{extension}"));
            println!("imported and GPU-rendered {extension}: {width}x{height}");
        }
        std::fs::remove_dir_all(dir).expect("remove isolated image fixtures");
    }

    #[test]
    fn a_project_package_round_trips_through_a_save_and_a_load() {
        let (mut pane, pixels) = painting_pane();
        pane.name = "分层画布".to_owned();
        // Paint something so the saved bitmap differs from a blank one,
        // then add an adjustment so the tree is not trivial.
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        pane.agent_adjustment_add(6); // Grain
        let tree = pane
            .document
            .as_ref()
            .expect("open")
            .to_json()
            .expect("json");

        let dir = std::env::temp_dir().join("concat-agent-comp");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("round-trip.comp");
        pane.agent_save_comp(&path).expect("the save wrote");
        assert_eq!(pane.project_path.as_deref(), Some(path.as_path()));
        assert!(
            !pane.is_modified(),
            "a successful save clears the canvas dirty state"
        );
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        assert!(pane.is_modified(), "a post-save stroke is dirty");
        pane.undo();
        assert_eq!(pane.project_path.as_deref(), Some(path.as_path()));
        assert!(!pane.is_modified(), "undo restores the saved revision");
        assert!(path.join("manifest.json").is_file());
        assert!(
            path.join("images")
                .read_dir()
                .expect("images")
                .next()
                .is_some()
        );

        // A fresh pane loads the package: the same tree, the same pixel
        // ids, the painted stroke back.
        let mut back = CanvasPane::default();
        back.agent_open(&path).expect("the package opened");
        assert_eq!(
            back.name, "分层画布",
            "project title comes from the manifest, not the storage path"
        );
        assert_eq!(back.project_path.as_deref(), Some(path.as_path()));
        assert_eq!(
            back.document
                .as_ref()
                .expect("open")
                .to_json()
                .expect("json"),
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
    fn multi_layer_auto_save_uses_an_immutable_snapshot_and_leaves_later_edits_dirty() {
        let root = std::env::temp_dir().join(format!(
            "concat-autosave-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let path = root.join("stable-project-id.comp");
        let mut pane = CanvasPane::default();
        let mut document = ImageDocument::new(1920, 1080);
        for index in 0..4 {
            let id = pane.store.put(Frame::transparent(1920, 1080));
            document.new_layer(format!("图层 {index}"), id);
        }
        pane.document = Some(document);
        pane.project_path = Some(path.clone());
        pane.name = "未命名画布".to_owned();
        pane.revision = 10;
        pane.saved_revision = 9;
        let started = std::time::Instant::now();
        pane.start_auto_save().expect("start background save");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "encoding must run after the call returns"
        );
        pane.name = "编辑后的标题".to_owned();
        pane.revision = 11;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !path.join("manifest.json").is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(path.join("manifest.json")).expect("background save completed"),
        )
        .expect("manifest");
        assert_eq!(manifest["name"], "未命名画布");
        assert_eq!(
            manifest["document"]["root"]["children"]
                .as_array()
                .map(Vec::len),
            Some(4)
        );
        assert!(pane.is_modified(), "an edit after the snapshot stays dirty");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_current_writes_new_stroke_to_recorded_path_and_clears_dirty() {
        let dir =
            std::env::temp_dir().join(format!("concat-save-current-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("isolated temp directory");
        let path = dir.join("original.comp");
        let (mut pane, pixels) = painting_pane();
        pane.save_to_path(&path).expect("initial save records path");
        let bitmap_path = path.join("images").join(format!("{}.png", pixels.as_u64()));
        let original_bitmap = std::fs::read(&bitmap_path).expect("initial bitmap");
        pane.set_tool(3);
        pane.brush.color = [12, 34, 56];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        assert!(pane.is_modified(), "new stroke must be dirty");
        let expected_pixels = pane
            .store
            .get(pixels)
            .expect("painted frame")
            .pixels()
            .to_vec();

        assert!(
            pane.save_current()
                .expect("ordinary save uses recorded path")
        );
        assert_eq!(pane.project_path.as_deref(), Some(path.as_path()));
        assert!(!pane.is_modified(), "ordinary save clears dirty");
        assert_ne!(
            std::fs::read(&bitmap_path).expect("updated bitmap"),
            original_bitmap,
            "ordinary save actually rewrites the recorded package"
        );
        let mut reopened = CanvasPane::default();
        reopened.agent_open(&path).expect("reopen ordinary save");
        assert_eq!(
            reopened.store.get(pixels).expect("reopened frame").pixels(),
            expected_pixels.as_slice()
        );
        assert_eq!(reopened.project_path.as_deref(), Some(path.as_path()));
        assert!(!reopened.is_modified());
        std::fs::remove_dir_all(&dir).expect("remove isolated fixture");
    }

    #[test]
    fn failed_save_to_path_preserves_original_path_dirty_state_and_saved_content() {
        let dir =
            std::env::temp_dir().join(format!("concat-save-failure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("isolated temp directory");
        let path = dir.join("original.comp");
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(80.0, 60.0);
        pane.brush_release();
        pane.save_to_path(&path).expect("initial saved package");
        let manifest_path = path.join("manifest.json");
        let bitmap_path = path.join("images").join(format!("{}.png", pixels.as_u64()));
        let saved_manifest = std::fs::read(&manifest_path).expect("saved manifest");
        let saved_bitmap = std::fs::read(&bitmap_path).expect("saved bitmap");
        let saved_pixels = pane
            .store
            .get(pixels)
            .expect("saved frame")
            .pixels()
            .to_vec();
        let saved_revision = pane.saved_revision;
        let saved_name = pane.name.clone();
        pane.brush_press(220.0, 140.0);
        pane.brush_release();
        assert!(pane.is_modified());
        let edited_pixels = pane
            .store
            .get(pixels)
            .expect("edited frame")
            .pixels()
            .to_vec();
        assert_ne!(edited_pixels, saved_pixels);

        // A plain file cannot contain a package or its sibling staging directory.
        // This fails through real filesystem I/O without permission assumptions.
        let blocker = dir.join("ordinary-file");
        std::fs::write(&blocker, b"fixture blocker").expect("ordinary file fixture");
        assert!(pane.save_to_path(&blocker.join("unsavable.comp")).is_err());
        assert_eq!(pane.project_path.as_deref(), Some(path.as_path()));
        assert_eq!(pane.name, saved_name);
        assert_eq!(pane.saved_revision, saved_revision);
        assert!(pane.is_modified(), "failed save leaves edits dirty");
        assert_eq!(
            pane.store.get(pixels).expect("edits survive").pixels(),
            edited_pixels.as_slice()
        );
        assert_eq!(
            std::fs::read(&manifest_path).expect("original manifest remains"),
            saved_manifest
        );
        assert_eq!(
            std::fs::read(&bitmap_path).expect("original bitmap remains"),
            saved_bitmap
        );
        assert_eq!(
            std::fs::read(&blocker).expect("blocker remains"),
            b"fixture blocker"
        );
        let mut reopened = CanvasPane::default();
        reopened
            .agent_open(&path)
            .expect("original package still opens");
        assert_eq!(
            reopened
                .store
                .get(pixels)
                .expect("original saved frame")
                .pixels(),
            saved_pixels.as_slice()
        );
        std::fs::remove_dir_all(&dir).expect("remove isolated fixture");
    }

    #[test]
    fn opening_a_second_image_replaces_pixels_with_no_old_store_residue() {
        let dir = std::env::temp_dir().join(format!("concat-open-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = dir.join("first.png");
        let second = dir.join("second.png");
        std::fs::write(&first, encode_png(&solid_frame([10, 20, 30, 255])).unwrap()).unwrap();
        std::fs::write(
            &second,
            encode_png(&solid_frame([90, 80, 70, 255])).unwrap(),
        )
        .unwrap();

        let mut pane = CanvasPane::default();
        pane.agent_open(&first).expect("first opens");
        let first_id = pane.layer.expect("first id");
        pane.agent_open(&second).expect("second opens");
        let second_id = pane.layer.expect("second id");
        assert!(
            pane.project_path.is_none(),
            "an image starts a fresh Save As path"
        );
        assert_eq!(pane.store.len(), 1);
        assert_eq!(second_id.as_u64(), 1, "a new document has a fresh store");
        assert_eq!(
            pane.store.get(second_id).unwrap().pixel(0, 0).unwrap(),
            [90, 80, 70, 255]
        );
        assert_eq!(first_id, second_id, "ids may repeat across documents");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_open_holds_a_pending_path_until_modified_work_is_saved() {
        let dir = std::env::temp_dir().join(format!("concat-unsaved-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let next = dir.join("next.png");
        std::fs::write(&next, encode_png(&solid_frame([4, 5, 6, 255])).unwrap()).unwrap();
        let save = dir.join("saved.comp");

        let (mut pane, _) = painting_pane();
        pane.set_tool(3);
        pane.brush_press_document(20.0, 20.0);
        pane.brush_release();
        assert!(pane.is_modified());
        assert!(pane.agent_open(&next).is_err());
        assert!(pane.open_confirm);
        assert_eq!(pane.pending_open.as_deref(), Some(next.as_path()));

        pane.agent_save_comp(&save).expect("project saved");
        assert!(!pane.is_modified());
        pane.agent_open(&next).expect("open proceeds after save");
        assert_eq!(
            pane.store
                .get(pane.layer.unwrap())
                .unwrap()
                .pixel(0, 0)
                .unwrap(),
            [4, 5, 6, 255]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn solid_frame(rgba: [u8; 4]) -> Frame {
        let mut frame = Frame::transparent(2, 2);
        frame.fill(rgba);
        frame
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
        assert!(
            !rows[1].6 && !rows[1].7,
            "a layer is neither group nor expanded"
        );
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
        assert!(
            rows[0].6 && rows[0].7,
            "unfolded: shown children, is a group"
        );
        pane.agent_layer_fold(0);
        let rows = pane.agent_layers();
        assert_eq!(rows.len(), 2);
        assert!(
            !rows[0].6 && rows[0].7,
            "folded: children hidden, still a group"
        );
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
            concat_canvas::compose(pane.document.as_ref().expect("open"), &pane.store)
                .pixel(150, 100)
                .expect("a painted pixel")
        };
        assert!(
            pixel[0] > plain[0],
            "the lifted curve lifted the red channel"
        );

        // The other channels keep their identity while red bends.
        assert_eq!(pane.agent_curve_channels()[1], vec![(0.0, 0.0), (1.0, 1.0)]);

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
    fn nested_delete_keeps_sibling_and_refreshes_adjustment_target() {
        let mut pane = grouped_painting_pane();
        let rows = pane.agent_layers();
        let children: Vec<(usize, u64)> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.5 == 1 && !row.7)
            .map(|(index, row)| (index, row.0))
            .collect();
        assert_eq!(children.len(), 2, "the fixture has two group children");

        // Removing the selected nested image picks its nearest image sibling
        // in the same group instead of jumping to a root row.
        pane.agent_layer_pick(children[0].0 as i32);
        pane.agent_layer_delete(children[0].0 as i32);
        assert_eq!(pane.active.map(|id| id.as_u64()), Some(children[1].1));
        let active_pixels = pane.active.and_then(|id| {
            let document = pane.document.as_ref()?;
            document.find(id).and_then(node_image_pixels)
        });
        assert_eq!(
            pane.layer, active_pixels,
            "the selected sibling is paintable"
        );

        // An adjustment stays selected when one of its image siblings is
        // removed, and its fallback target is rebuilt from the same parent.
        let mut pane = grouped_painting_pane();
        let rows = pane.agent_layers();
        let child = rows
            .iter()
            .position(|row| row.5 == 1 && !row.7)
            .expect("nested image");
        pane.agent_layer_pick(child as i32);
        pane.agent_adjustment_add(1);
        let adjustment = pane.active;
        let image_to_remove = pane
            .rows()
            .iter()
            .enumerate()
            .find(|(_, (node, depth))| *depth == 1 && node_image_pixels(node).is_some())
            .map(|(index, _)| index)
            .expect("remaining nested image");
        pane.agent_layer_delete(image_to_remove as i32);
        assert_eq!(pane.active, adjustment, "the adjustment survives");
        let parent = pane.active.and_then(|id| {
            let document = pane.document.as_ref()?;
            node_parent_slot(document, id).map(|(parent, _)| parent)
        });
        let expected = parent.and_then(|parent| {
            pane.document
                .as_ref()
                .and_then(|document| topmost_image_in_parent(document, parent))
        });
        assert_eq!(pane.layer, expected, "the sibling remains the paint target");
        assert!(pane.layer.is_some_and(|id| pane.store.get(id).is_some()));
    }

    #[test]
    fn nested_adjustment_and_group_stay_in_the_selected_parent() {
        let mut pane = grouped_painting_pane();
        let child_row = pane
            .agent_layers()
            .iter()
            .position(|row| row.5 == 1 && !row.7)
            .expect("nested image");
        pane.agent_layer_pick(child_row as i32);
        let parent = pane
            .active
            .and_then(|id| {
                let document = pane.document.as_ref()?;
                node_parent_slot(document, id).and_then(|(parent, _)| parent)
            })
            .expect("the image has a group parent");

        pane.agent_adjustment_add(1);
        let adjustment = pane.active.expect("new adjustment is active");
        let adjustment_parent = pane
            .document
            .as_ref()
            .and_then(|document| node_parent_slot(document, adjustment))
            .and_then(|(parent, _)| parent);
        assert_eq!(adjustment_parent, Some(parent));
        assert_eq!(
            pane.agent_layers()
                .iter()
                .find(|row| row.0 == adjustment.as_u64())
                .map(|row| row.5),
            Some(1),
            "the adjustment remains nested"
        );

        pane.agent_layer_group();
        let group = pane.active.expect("new group is active");
        let group_parent = pane
            .document
            .as_ref()
            .and_then(|document| node_parent_slot(document, group))
            .and_then(|(parent, _)| parent);
        assert_eq!(group_parent, Some(parent));
        assert_eq!(
            pane.agent_layers()
                .iter()
                .find(|row| row.0 == group.as_u64())
                .map(|row| row.5),
            Some(1),
            "the new group remains nested"
        );
    }

    #[test]
    fn a_mask_hides_where_the_brush_paints_it_black() {
        let (mut pane, pixels) = painting_pane();
        // Something on the layer for the mask to hide.
        pane.set_tool(3);
        pane.brush.color = [10, 20, 30];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let original_layer = pane.store.get(pixels).expect("layer before mask");
        let document = pane.document.clone().expect("open");
        let showed = concat_canvas::compose(&document, &pane.store)
            .pixel(150, 100)
            .expect("the painted pixel")[3];
        assert_eq!(showed, 255, "the stroke showed before any mask");

        // A white mask over the base layer, the painting tools pointed
        // at it.
        pane.set_tool(4);
        pane.agent_layer_mask_add(0);
        assert_eq!(pane.tool, 3, "adding a mask switches to the brush");
        assert!(
            !pane.brush.erasing,
            "mask painting starts with a normal brush"
        );
        assert!(pane.agent_paint_mask(), "the tools point at the fresh mask");
        pane.paint_mask = false;
        pane.set_tool(2);
        pane.mask_chip_click(0);
        assert_eq!(pane.tool, 3, "the mask chip leaves zoom mode for painting");
        assert!(!pane.brush.erasing, "mask chip painting is not erasing");
        assert!(pane.agent_paint_mask(), "the chip selects the mask target");
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
            mask.pixels()
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 255, 255, 255]),
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
        assert_eq!(
            layer.pixels(),
            original_layer.pixels(),
            "painting the mask preserves every source RGBA byte"
        );
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
    fn a_mask_sets_aside_and_applies_whole() {
        let (mut pane, _) = painting_pane();
        // Something on the layer for the mask to hide.
        pane.set_tool(3);
        pane.brush.color = [10, 20, 30];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        pane.agent_layer_mask_add(0);
        pane.brush.color = [0, 0, 0];
        pane.brush_press(150.0, 100.0);
        pane.brush_release();

        // The fresh mask is enabled, and it hides the dab.
        let rows = pane.agent_layers();
        assert!(rows[0].8, "the row carries a mask");
        assert!(rows[0].9, "that mask is the one being painted");
        assert!(rows[0].10, "and it is enabled");
        let document = pane.document.clone().expect("open");
        assert_eq!(
            concat_canvas::compose(&document, &pane.store)
                .pixel(150, 100)
                .expect("the painted pixel")[3],
            0,
            "black on the mask hides what is under it"
        );

        // Setting the mask aside keeps every pixel - painted or not -
        // and stops the compositing from reading it.
        pane.agent_layer_mask_toggle(0);
        assert!(!pane.agent_layers()[0].10, "the mask is set aside");
        let document = pane.document.clone().expect("open");
        assert_eq!(
            concat_canvas::compose(&document, &pane.store)
                .pixel(150, 100)
                .expect("the painted pixel")[3],
            255,
            "a set-aside mask hides nothing"
        );

        // Applied again, the painted black goes back to work - and the
        // brush still points at the same mask the whole time.
        assert!(pane.agent_paint_mask(), "the tools never left the mask");
        pane.agent_layer_mask_toggle(0);
        assert!(pane.agent_layers()[0].10, "the mask applies again");
        let document = pane.document.clone().expect("open");
        assert_eq!(
            concat_canvas::compose(&document, &pane.store)
                .pixel(150, 100)
                .expect("the painted pixel")[3],
            0,
            "the hiding came back with the mask"
        );
    }

    #[test]
    fn mask_eraser_and_selection_delete_clear_red_coverage_for_cpu_and_gpu() {
        let (mut pane, pixels) = painting_pane();
        pane.set_tool(3);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        let source = pane.store.get(pixels).expect("source");
        pane.agent_layer_mask_add(0);
        let mask_id = pane.paint_target().expect("mask target");

        pane.set_tool(4);
        pane.brush_press(150.0, 100.0);
        pane.brush_release();
        assert_eq!(
            pane.store.get(mask_id).unwrap().pixel(150, 100).unwrap(),
            [0, 0, 0, 255],
            "eraser becomes zero mask coverage"
        );
        assert_eq!(
            pane.store.get(pixels).unwrap().pixels(),
            source.pixels(),
            "mask erasing leaves source RGBA untouched"
        );
        let document = pane.document.as_ref().expect("document");
        let cpu = concat_canvas::compose(document, &pane.store);
        assert_eq!(cpu.pixel(150, 100).unwrap()[3], 0);
        let mut gpu =
            CanvasGpu::new().expect("mask CPU/GPU consistency requires a usable wgpu adapter");
        let gpu = gpu.compose_frame(document, &pane.store);
        assert!(
            (i16::from(gpu.pixel(150, 100).unwrap()[3])
                - i16::from(cpu.pixel(150, 100).unwrap()[3]))
            .abs()
                <= 1
        );

        pane.undo();
        pane.agent_select_rect(140.0, 90.0, 20.0, 20.0);
        pane.agent_delete_selection();
        assert_eq!(
            pane.store.get(mask_id).unwrap().pixel(150, 100).unwrap(),
            [0, 0, 0, 255],
            "selection delete also clears mask coverage"
        );
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
