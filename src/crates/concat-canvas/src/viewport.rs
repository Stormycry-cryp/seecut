// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// `CanvasViewport` plus the navigation half of `EditorCanvas`'s pointer
// handling - scroll, pinch, the hand and zoom tools. The math is that
// file's, kept to the constants it shipped with (zoom 0.001..=32, a 96 pt
// fit margin, doubling for every 100 pt of zoom-tool drag); what changed is
// the shell around it, an explicit input enum instead of NSView overrides,
// so the same controller drives a Slint TouchArea on every platform.

//! The view onto the document: zoom, pan, and the navigation gestures.
//!
//! Coordinates come in two flavours. *View* points are what the UI hands
//! over - Slint logical pixels, y down, origin at the canvas pane's top
//! left. *Document* points are pixels in the document's own space, also
//! y down. Zoom 1 means one document pixel per view pixel; the backing
//! scale is carried because fit needs it (fit picks a zoom so the document
//! fills the pane at one document pixel per *device* pixel, like the
//! original's fit).

/// Where the document sits in the view. Port of the original's
/// `CanvasViewport`, field for field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasViewport {
    view_size: (f64, f64),
    backing_scale: f64,
    zoom: f64,
    pan: (f64, f64),
    follows_fit: bool,
}

/// The zoom bounds the original shipped with.
pub const ZOOM_RANGE: (f64, f64) = (0.001, 32.0);
/// The fit leaves this much view around the document, in points.
const FIT_MARGIN: f64 = 96.0;

impl Default for CanvasViewport {
    fn default() -> Self {
        Self {
            view_size: (0.0, 0.0),
            backing_scale: 1.0,
            zoom: 1.0,
            pan: (0.0, 0.0),
            follows_fit: true,
        }
    }
}

impl CanvasViewport {
    /// Document pixels per view point at the current zoom.
    pub fn points_per_pixel(&self) -> f64 {
        self.zoom / self.backing_scale
    }

    /// The view's centre, the anchor `fit` and default zooms balance on.
    pub fn center(&self) -> (f64, f64) {
        (self.view_size.0 / 2.0, self.view_size.1 / 2.0)
    }

    /// The current zoom: document pixels per view point times the backing
    /// scale.
    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The view offset from the centred position.
    pub fn pan(&self) -> (f64, f64) {
        self.pan
    }

    /// The pane's size in view points.
    pub fn view_size(&self) -> (f64, f64) {
        self.view_size
    }

    /// Whether the view is still on the fit the document opened with - a
    /// manual zoom or pan anywhere breaks it, and a resize re-fits only
    /// while it holds.
    pub fn follows_fit(&self) -> bool {
        self.follows_fit
    }

    /// The document's on-screen rect, centred then pushed by the pan.
    pub fn document_rect(&self, document_size: (f64, f64)) -> (f64, f64, f64, f64) {
        let pixel = self.points_per_pixel();
        let (cx, cy) = self.center();
        let width = document_size.0 * pixel;
        let height = document_size.1 * pixel;
        (
            cx - width / 2.0 + self.pan.0,
            cy - height / 2.0 + self.pan.1,
            width,
            height,
        )
    }

    /// A view point as a document point.
    pub fn document_point(&self, point: (f64, f64), document_size: (f64, f64)) -> (f64, f64) {
        let (origin_x, origin_y, _, _) = self.document_rect(document_size);
        let pixel = self.points_per_pixel();
        ((point.0 - origin_x) / pixel, (point.1 - origin_y) / pixel)
    }

    /// A document point as a view point.
    pub fn view_point(&self, point: (f64, f64), document_size: (f64, f64)) -> (f64, f64) {
        let (origin_x, origin_y, _, _) = self.document_rect(document_size);
        let pixel = self.points_per_pixel();
        (origin_x + point.0 * pixel, origin_y + point.1 * pixel)
    }

    /// Fits the document in the view, never below one device pixel per
    /// document pixel unless the document is the bigger of the two.
    pub fn fit(&mut self, document_size: (f64, f64)) {
        let (vw, vh) = self.view_size;
        if vw <= 0.0 || vh <= 0.0 {
            self.follows_fit = true;
            return;
        }
        let width_zoom = (vw - FIT_MARGIN).max(1.0) / document_size.0;
        let height_zoom = (vh - FIT_MARGIN).max(1.0) / document_size.1;
        self.zoom = clamp((width_zoom.min(height_zoom)) * self.backing_scale);
        self.pan = (0.0, 0.0);
        self.follows_fit = true;
    }

    /// The pane changed size or display. The centre document point stays
    /// put, so moving between displays does not jump the picture.
    pub fn resize(
        &mut self,
        size: (f64, f64),
        backing_scale: f64,
        document_size: Option<(f64, f64)>,
    ) {
        let old_pixel = self.points_per_pixel();
        self.view_size = size;
        self.backing_scale = backing_scale.max(1.0);
        match document_size {
            Some(document) if self.follows_fit => self.fit(document),
            _ => {
                let ratio = self.points_per_pixel() / old_pixel;
                self.pan.0 *= ratio;
                self.pan.1 *= ratio;
            }
        }
    }

    /// Sets the zoom keeping the document point under `anchor` fixed -
    /// the property every zoom gesture needs to feel attached to the
    /// pointer.
    pub fn set_zoom(&mut self, value: f64, anchor: (f64, f64), document_size: (f64, f64)) {
        if !value.is_finite() {
            return;
        }
        let pixel = self.document_point(anchor, document_size);
        self.zoom = clamp(value);
        let moved = self.view_point(pixel, document_size);
        self.pan.0 += anchor.0 - moved.0;
        self.pan.1 += anchor.1 - moved.1;
        self.follows_fit = false;
    }

    /// Slides the view by view-point deltas.
    pub fn translate(&mut self, delta: (f64, f64)) {
        self.pan.0 += delta.0;
        self.pan.1 += delta.1;
        self.follows_fit = false;
    }
}

fn clamp(value: f64) -> f64 {
    value.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)
}

/// One navigation gesture, from the pointer or the keyboard.
///
/// The controller owns the state a drag spans (`EditorCanvas` kept it in
/// ivars); the caller feeds events as they come and reads the viewport
/// after each. Anything beyond navigation - painting, selections, crop -
/// belongs to the tools, not here; the controller simply ignores input
/// while a tool owns the pointer, which is what the original's guards did.
#[derive(Debug)]
pub struct Navigator {
    /// The space bar: while held, every drag pans, whatever the tool.
    space_held: bool,
    /// A hand-tool/space drag in flight, at the last pointer position.
    pan_drag: Option<(f64, f64)>,
    /// A zoom-tool drag: where it began, the zoom it began at, and whether
    /// it moved far enough to count as a drag rather than a click.
    zoom_drag: Option<ZoomDrag>,
    /// The document's size in pixels, remembered so callers pass each
    /// input alone - the original read `session.document` in its handlers.
    document: Option<(f64, f64)>,
    viewport: CanvasViewport,
}

#[derive(Debug, Clone, Copy)]
struct ZoomDrag {
    start: (f64, f64),
    zoom: f64,
    moved: bool,
}

/// Zoom-tool drags must move this far before they are drags; less is a
/// click, which steps the zoom on release.
const ZOOM_DRAG_THRESHOLD: f64 = 3.0;
/// Right doubles for every this many points dragged left, and the reverse.
const ZOOM_DRAG_DISTANCE: f64 = 100.0;
/// Wheel zoom, matching the original: `exp(-delta_y * 0.015)` per event.
const WHEEL_ZOOM_RATE: f64 = 0.015;
/// Line-based scroll deltas (a plain notched mouse wheel) move this much
/// faster than precise trackpad deltas.
const LINE_SCROLL_MULTIPLIER: f64 = 12.0;

/// What the pointer or keyboard did. View points in, viewport changes out.
#[derive(Clone, Copy, Debug)]
pub enum NavInput {
    /// The space bar went down or up.
    Space(bool),
    /// A press that should pan: the hand tool, or any tool with space
    /// held. The tools' own presses are not sent here.
    PanPress((f64, f64)),
    /// A move while a pan drag is down.
    PanMove((f64, f64)),
    /// The wheel or two-finger scroll. `precise` is trackpad-style pixel
    /// deltas; `zoom_modifier` is Ctrl/Cmd/Option held (the pinch
    /// substitute on a mouse).
    Scroll {
        /// Scroll deltas, x right / y down, in points (precise) or lines.
        delta: (f64, f64),
        /// Whether the deltas are precise trackpad pixels, not wheel lines.
        precise: bool,
        /// Ctrl/Cmd/Option held: the wheel zooms instead of panning.
        zoom_modifier: bool,
        /// Where the pointer is, for an anchored zoom.
        pointer: (f64, f64),
    },
    /// A trackpad pinch.
    Pinch {
        /// The gesture's magnification this event: 0 none, 0.5 half again.
        magnification: f64,
        /// Where the pointer is, for an anchored zoom.
        pointer: (f64, f64),
    },
    /// A zoom-tool press. `option` zooms out on a plain click.
    ZoomPress {
        /// Where the press landed, the drag's anchor.
        pointer: (f64, f64),
        /// Option held: a plain click steps out instead of in.
        option: bool,
    },
    /// A move while a zoom-tool drag is down; dragging right zooms in.
    ZoomMove {
        /// Where the pointer is now.
        pointer: (f64, f64),
        /// Option held (unused mid-drag; kept for symmetry with the press).
        option: bool,
    },
    /// The pointer went up. `option` rides along because a zoom-tool click
    /// steps out with it - the drag itself records no buttons.
    Release {
        /// Option held at release: a click steps out instead of in.
        option: bool,
    },
    /// Fit the document in the view (the keyboard shortcut, the 100%..fit
    /// button).
    Fit,
}

impl Navigator {
    /// A navigator over `viewport`, knowing no document yet.
    pub fn new(viewport: CanvasViewport) -> Self {
        Self {
            space_held: false,
            pan_drag: None,
            zoom_drag: None,
            document: None,
            viewport,
        }
    }

    /// The controller learns what it is navigating - call when a document
    /// opens or closes, or its canvas size changes.
    pub fn set_document(&mut self, size: Option<(f64, f64)>) {
        self.document = size;
    }

    /// The viewport the navigation drives.
    pub fn viewport(&self) -> &CanvasViewport {
        &self.viewport
    }

    /// The viewport, for the UI that keeps the pane's size in it.
    pub fn viewport_mut(&mut self) -> &mut CanvasViewport {
        &mut self.viewport
    }

    /// Whether a pan drag is in flight, for the closed-hand cursor.
    pub fn is_panning(&self) -> bool {
        self.pan_drag.is_some()
    }

    /// Whether the space bar is down, for the open-hand cursor.
    pub fn space_held(&self) -> bool {
        self.space_held
    }

    /// Applies one input. `document_size` anchors zooms; events that
    /// arrive before a document exists are ignored, as the original's
    /// guards ignored them.
    /// Applies one input against the stored document. Zooms anchor where
    /// they must; events before a document is set are ignored, the way
    /// the original's guards ignored them.
    pub fn apply(&mut self, input: NavInput) {
        let document = self.document;
        match input {
            NavInput::Space(held) => {
                self.space_held = held;
                if !held {
                    // The original cleared the hand drag when space left
                    // mid-press; the drag's memory went with it.
                    self.pan_drag = None;
                }
            }
            NavInput::PanPress(point) => self.pan_drag = Some(point),
            NavInput::PanMove(point) => {
                if let Some(last) = self.pan_drag {
                    self.viewport
                        .translate((point.0 - last.0, point.1 - last.1));
                    self.pan_drag = Some(point);
                }
            }
            NavInput::Release { option } => {
                self.pan_drag = None;
                // A press that stayed put steps the zoom instead - in, or
                // out with Option. A drag keeps what it zoomed to.
                if let Some(drag) = self.zoom_drag.take()
                    && !drag.moved
                    && let Some(document) = document
                {
                    let factor = if option { 0.5 } else { 2.0 };
                    let anchor = drag.start;
                    self.viewport
                        .set_zoom(self.viewport.zoom * factor, anchor, document);
                }
            }
            NavInput::Scroll {
                delta,
                precise,
                zoom_modifier,
                pointer,
            } => {
                let Some(document) = document else {
                    return;
                };
                if zoom_modifier {
                    self.viewport.set_zoom(
                        self.viewport.zoom * (-delta.1 * WHEEL_ZOOM_RATE).exp(),
                        pointer,
                        document,
                    );
                } else {
                    let multiplier = if precise { 1.0 } else { LINE_SCROLL_MULTIPLIER };
                    self.viewport
                        .translate((delta.0 * multiplier, delta.1 * multiplier));
                }
            }
            NavInput::Pinch {
                magnification,
                pointer,
            } => {
                if let Some(document) = document {
                    self.viewport.set_zoom(
                        self.viewport.zoom * (1.0 + magnification),
                        pointer,
                        document,
                    );
                }
            }
            NavInput::ZoomPress { pointer, .. } => {
                self.zoom_drag = Some(ZoomDrag {
                    start: pointer,
                    zoom: self.viewport.zoom,
                    moved: false,
                });
            }
            NavInput::ZoomMove { pointer, .. } => {
                let Some(document) = document else {
                    return;
                };
                let Some(drag) = self.zoom_drag.as_mut() else {
                    return;
                };
                let dx = pointer.0 - drag.start.0;
                if dx.abs() >= ZOOM_DRAG_THRESHOLD {
                    drag.moved = true;
                }
                // Right zooms in, left out: doubling for every 100 points
                // dragged, about where the drag began.
                if drag.moved {
                    let target = drag.zoom * (2.0f64).powf(dx / ZOOM_DRAG_DISTANCE);
                    self.viewport.set_zoom(target, drag.start, document);
                }
            }
            NavInput::Fit => {
                if let Some(document) = document {
                    self.viewport.fit(document);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> CanvasViewport {
        let mut vp = CanvasViewport::default();
        vp.resize((800.0, 600.0), 1.0, Some((400.0, 300.0)));
        vp
    }

    #[test]
    fn fit_fills_the_pane_with_the_margin_kept() {
        let vp = viewport();
        // 800-96 = 704 / 400 = 1.76; 600-96 = 504 / 300 = 1.68; the min
        // rules, so the taller axis fits and the document is centred.
        assert!((vp.zoom() - 1.68).abs() < 1e-9);
        assert!(vp.follows_fit());
        let (x, y, w, h) = vp.document_rect((400.0, 300.0));
        assert!((x - (800.0 - w) / 2.0).abs() < 1e-9);
        assert!((y - (600.0 - h) / 2.0).abs() < 1e-9);
        assert!((w - 400.0 * 1.68).abs() < 1e-9);
    }

    #[test]
    fn fit_never_shrinks_a_small_document_below_device_pixels() {
        let mut vp = CanvasViewport::default();
        vp.resize((800.0, 600.0), 2.0, None);
        vp.fit((50.0, 50.0));
        // min(704/50, 504/50) = 10.08, times the 2x backing scale: the fit
        // rides the device pixel, whatever the point zoom says.
        assert!((vp.zoom() - 20.16).abs() < 1e-9);
        // A tiny document pushes the fit past the clamp's ceiling.
        vp.fit((10.0, 10.0));
        assert!((vp.zoom() - ZOOM_RANGE.1).abs() < 1e-9);
    }

    #[test]
    fn a_zoom_keeps_the_point_under_the_anchor() {
        let mut vp = viewport();
        let anchor = (300.0, 200.0);
        let document = vp.document_point(anchor, (400.0, 300.0));
        vp.set_zoom(vp.zoom() * 2.0, anchor, (400.0, 300.0));
        let again = vp.document_point(anchor, (400.0, 300.0));
        assert!((document.0 - again.0).abs() < 1e-9);
        assert!((document.1 - again.1).abs() < 1e-9);
        assert!(!vp.follows_fit());
    }

    #[test]
    fn a_resize_keeps_the_centre_document_point() {
        let mut vp = viewport();
        let before = vp.document_point(vp.center(), (400.0, 300.0));
        vp.resize((1000.0, 800.0), 1.0, None);
        let after = vp.document_point(vp.center(), (400.0, 300.0));
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
    }

    #[test]
    fn a_resize_while_fitting_refits() {
        let mut vp = viewport();
        vp.resize((1000.0, 800.0), 1.0, Some((400.0, 300.0)));
        assert!(vp.follows_fit());
        // (1000-96)/400 = 2.26; (800-96)/300 = 2.346...
        assert!((vp.zoom() - 2.26).abs() < 1e-9);
    }

    #[test]
    fn wheel_scrolling_pans_and_the_modifier_zooms() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        let pan0 = nav.viewport().pan();
        nav.apply(NavInput::Scroll {
            delta: (10.0, -5.0),
            precise: true,
            zoom_modifier: false,
            pointer: (0.0, 0.0),
        });
        let pan1 = nav.viewport().pan();
        assert!((pan1.0 - pan0.0 - 10.0).abs() < 1e-9);
        assert!((pan1.1 - pan0.1 + 5.0).abs() < 1e-9);

        let zoom0 = nav.viewport().zoom();
        let anchor = (400.0, 300.0);
        let document = nav.viewport().document_point(anchor, (400.0, 300.0));
        nav.apply(NavInput::Scroll {
            delta: (0.0, -20.0),
            precise: true,
            zoom_modifier: true,
            pointer: anchor,
        });
        assert!(nav.viewport().zoom() > zoom0);
        let again = nav.viewport().document_point(anchor, (400.0, 300.0));
        assert!(
            (document.0 - again.0).abs() < 1e-9,
            "zoom anchors at the pointer"
        );
    }

    #[test]
    fn line_scrolls_move_twelve_times_precise_ones() {
        let mut precise = Navigator::new(viewport());
        precise.apply(NavInput::Scroll {
            delta: (1.0, 0.0),
            precise: true,
            zoom_modifier: false,
            pointer: (0.0, 0.0),
        });
        let mut line = Navigator::new(viewport());
        line.apply(NavInput::Scroll {
            delta: (1.0, 0.0),
            precise: false,
            zoom_modifier: false,
            pointer: (0.0, 0.0),
        });
        assert!((line.viewport().pan().0 - precise.viewport().pan().0 * 12.0).abs() < 1e-9);
    }

    #[test]
    fn a_pinch_zooms_about_the_pointer() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        let zoom0 = nav.viewport().zoom();
        nav.apply(NavInput::Pinch {
            magnification: 0.5,
            pointer: (400.0, 300.0),
        });
        assert!((nav.viewport().zoom() - zoom0 * 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_hand_drag_pans_by_the_pointer_delta() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        nav.apply(NavInput::PanPress((100.0, 100.0)));
        assert!(nav.is_panning());
        nav.apply(NavInput::PanMove((140.0, 90.0)));
        nav.apply(NavInput::PanMove((160.0, 90.0)));
        let pan = nav.viewport().pan();
        assert!((pan.0 - 60.0).abs() < 1e-9);
        assert!((pan.1 + 10.0).abs() < 1e-9);
        nav.apply(NavInput::Release { option: false });
        assert!(!nav.is_panning());
        // The pan stays after the drag ends.
        assert!((nav.viewport().pan().0 - 60.0).abs() < 1e-9);
    }

    #[test]
    fn letting_space_go_mid_drag_stops_the_pan() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        nav.apply(NavInput::Space(true));
        nav.apply(NavInput::PanPress((0.0, 0.0)));
        nav.apply(NavInput::PanMove((30.0, 0.0)));
        nav.apply(NavInput::Space(false));
        assert!(!nav.is_panning());
        let pan = nav.viewport().pan();
        nav.apply(NavInput::PanMove((60.0, 0.0)));
        assert_eq!(nav.viewport().pan(), pan, "the drag is dead");
    }

    #[test]
    fn a_zoom_drag_doubles_for_every_hundred_points() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        let zoom0 = nav.viewport().zoom();
        nav.apply(NavInput::ZoomPress {
            pointer: (400.0, 300.0),
            option: false,
        });
        // Under the threshold: nothing yet.
        nav.apply(NavInput::ZoomMove {
            pointer: (401.0, 300.0),
            option: false,
        });
        assert!((nav.viewport().zoom() - zoom0).abs() < 1e-12);
        nav.apply(NavInput::ZoomMove {
            pointer: (500.0, 300.0),
            option: false,
        });
        assert!((nav.viewport().zoom() - zoom0 * 2.0).abs() < 1e-9);
        nav.apply(NavInput::Release { option: false });
        // The drag zoom stands; a release after a drag does not step again.
        assert!((nav.viewport().zoom() - zoom0 * 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_zoom_click_steps_twice_or_half_with_option() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        let zoom0 = nav.viewport().zoom();
        nav.apply(NavInput::ZoomPress {
            pointer: (10.0, 10.0),
            option: false,
        });
        nav.apply(NavInput::Release { option: false });
        assert!((nav.viewport().zoom() - zoom0 * 2.0).abs() < 1e-9);

        let zoom1 = nav.viewport().zoom();
        nav.apply(NavInput::ZoomPress {
            pointer: (10.0, 10.0),
            option: true,
        });
        nav.apply(NavInput::Release { option: true });
        assert!((nav.viewport().zoom() - zoom1 * 0.5).abs() < 1e-9);
    }

    #[test]
    fn zoom_stays_inside_the_range() {
        let mut nav = Navigator::new(viewport());
        nav.set_document(Some((400.0, 300.0)));
        for _ in 0..10 {
            nav.apply(NavInput::ZoomPress {
                pointer: (10.0, 10.0),
                option: false,
            });
            nav.apply(NavInput::Release { option: false });
        }
        assert!((nav.viewport().zoom() - ZOOM_RANGE.1).abs() < 1e-9);
        for _ in 0..30 {
            nav.apply(NavInput::ZoomPress {
                pointer: (10.0, 10.0),
                option: true,
            });
            nav.apply(NavInput::Release { option: true });
        }
        assert!((nav.viewport().zoom() - ZOOM_RANGE.0).abs() < 1e-9);
    }

    #[test]
    fn navigation_ignores_a_missing_document() {
        let mut nav = Navigator::new(viewport());
        let zoom0 = nav.viewport().zoom();
        nav.apply(NavInput::Scroll {
            delta: (0.0, -20.0),
            precise: true,
            zoom_modifier: true,
            pointer: (0.0, 0.0),
        });
        nav.apply(NavInput::Pinch {
            magnification: 3.0,
            pointer: (0.0, 0.0),
        });
        nav.apply(NavInput::Fit);
        assert_eq!(nav.viewport().zoom(), zoom0);
    }
}
