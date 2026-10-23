// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shape gestures share the canvas snapshot transaction and raster cache.
use super::*;
use concat_canvas::vector::{Knot, ShapeKind, VectorShape};

type ShapeSpace = (
    Option<concat_canvas::LayerTransform>,
    (f32, f32),
    (f32, f32),
);

#[derive(Clone, Copy)]
pub(super) struct ShapeDrag {
    start: [f32; 2],
    knot: usize,
    handle: usize,
}

impl CanvasPane {
    fn selected_shape(&self) -> Option<&VectorShape> {
        let node = self.document.as_ref()?.find(self.active?)?;
        if self.paint_mask {
            return node.mask()?.vector.as_ref().map(|v| &v.geometry);
        }
        let LayerNode::Layer(layer) = node else {
            return None;
        };
        layer.vector.as_ref()
    }

    fn selected_shape_mut(&mut self) -> Option<&mut VectorShape> {
        let node = self.document.as_mut()?.find_mut(self.active?)?;
        if self.paint_mask {
            return node
                .mask_slot_mut()
                .as_mut()?
                .vector
                .as_mut()
                .map(|v| &mut v.geometry);
        }
        let LayerNode::Layer(layer) = node else {
            return None;
        };
        layer.vector.as_mut()
    }

    fn shape_space(&self) -> Option<ShapeSpace> {
        let doc = self.document.as_ref()?;
        let canvas = (doc.width as f32, doc.height as f32);
        let node = doc.find(self.active?)?;
        let current = match node {
            LayerNode::Layer(l) => Some(l.transform),
            _ => None,
        };
        if self.paint_mask {
            let mask = node.mask()?;
            let vector = mask.vector.as_ref()?;
            let placement = current.map(|t| {
                if mask.linked {
                    t
                } else {
                    mask.anchor.unwrap_or(t)
                }
            });
            return Some((
                placement,
                (vector.width as f32, vector.height as f32),
                canvas,
            ));
        }
        Some((current, canvas, canvas))
    }

    fn shape_point(&self, x: f64, y: f64) -> Option<[f32; 2]> {
        let (placement, bitmap, canvas) = self.shape_space()?;
        let p = self.to_document(x, y);
        let p = (p.0 as f32, p.1 as f32);
        let p = match placement {
            Some(t) => t.to_bitmap(p, bitmap, canvas)?,
            None => p,
        };
        Some([p.0, p.1])
    }

    pub(super) fn shape_screen_point(&self, p: [f32; 2]) -> Option<(f64, f64)> {
        let (placement, bitmap, canvas) = self.shape_space()?;
        let p = match placement {
            Some(t) => t.from_bitmap((p[0], p[1]), bitmap, canvas)?,
            None => (p[0], p[1]),
        };
        Some(
            self.nav
                .viewport()
                .view_point((p.0 as f64, p.1 as f64), (canvas.0 as f64, canvas.1 as f64)),
        )
    }

    fn refresh_shape(&mut self) {
        if self.editing_vector_mask() {
            self.refresh_vector_mask();
            return;
        }
        let Some(doc) = self.document.as_ref() else {
            return;
        };
        let Some(LayerNode::Layer(layer)) = self.active.and_then(|id| doc.find(id)) else {
            return;
        };
        let Some(shape) = &layer.vector else {
            return;
        };
        let Some(frame) = shape.rasterize(doc.width, doc.height) else {
            return;
        };
        let pixels = layer.pixels;
        self.store.replace(pixels, frame);
        self.bump_thumbnail_revision(pixels);
        self.sync_view();
    }

    fn create_shape(&mut self, p: [f32; 2], kind: ShapeKind) {
        if self.document.is_none() {
            return;
        }
        self.begin_history();
        let doc = self.document.as_mut().expect("checked");
        let pixels = self.store.put(Frame::transparent(doc.width, doc.height));
        let label = match kind {
            ShapeKind::Rectangle => "矩形",
            ShapeKind::Ellipse => "椭圆",
            ShapeKind::Path => "路径",
        };
        let id = doc.new_layer(label, pixels);
        doc.layer_mut(id).expect("created").vector = Some(VectorShape::new(kind, p, p));
        self.active = Some(id);
        self.layer = Some(pixels);
        self.paint_mask = false;
        self.shape_session = Some(id);
    }

    pub(super) fn shape_pointer(&mut self, event: i32, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if event == 0 {
            if self.shape_drag.is_some() || self.transform_session.is_some() {
                return;
            }
            if matches!(self.tool, 7 | 8) || (self.tool == 9 && self.shape_session.is_none()) {
                let p = self.to_document(x, y);
                let p = [p.0 as f32, p.1 as f32];
                let kind = match self.tool {
                    7 => ShapeKind::Rectangle,
                    8 => ShapeKind::Ellipse,
                    _ => ShapeKind::Path,
                };
                self.create_shape(p, kind);
                self.shape_drag = Some(ShapeDrag {
                    start: p,
                    knot: 0,
                    handle: 0,
                });
            } else if self.tool == 9 && self.shape_session.is_some() {
                let Some(p) = self.shape_point(x, y) else {
                    return;
                };
                let closes = self
                    .selected_shape()
                    .filter(|s| s.knots.len() >= 3)
                    .and_then(|s| self.shape_screen_point(s.knots[0].anchor))
                    .is_some_and(|a| (a.0 - x).hypot(a.1 - y) < 9.0);
                if closes {
                    self.shape_finish(true, true);
                    return;
                }
                let Some(shape) = self.selected_shape_mut() else {
                    return;
                };
                if shape.knots.len() >= 4096 {
                    return;
                }
                shape.knots.push(Knot::corner(p));
                self.shape_drag = Some(ShapeDrag {
                    start: p,
                    knot: shape.knots.len() - 1,
                    handle: 0,
                });
            } else if self.tool == 10 {
                let hit = self.selected_shape().and_then(|shape| {
                    shape
                        .knots
                        .iter()
                        .enumerate()
                        .flat_map(|(i, k)| {
                            [(i, 0, k.anchor), (i, 1, k.incoming), (i, 2, k.outgoing)]
                        })
                        .filter_map(|(i, h, p)| {
                            self.shape_screen_point(p)
                                .map(|q| (i, h, (q.0 - x).hypot(q.1 - y)))
                        })
                        .filter(|(_, _, d)| *d < 9.0)
                        .min_by(|a, b| a.2.total_cmp(&b.2))
                        .map(|(i, h, _)| (i, h))
                });
                if let Some((knot, handle)) = hit {
                    let Some(start) = self.shape_point(x, y) else {
                        return;
                    };
                    self.begin_history();
                    self.shape_session = self.active;
                    self.shape_drag = Some(ShapeDrag {
                        start,
                        knot,
                        handle,
                    });
                } else if let Some(id) = self.hit_image(x, y) {
                    self.select_object_after_transform(Some(id));
                }
            }
            if self.tool != 10 {
                self.refresh_shape();
            }
        } else if event == 1 {
            let Some(drag) = self.shape_drag else { return };
            let Some(p) = self.shape_point(x, y) else {
                return;
            };
            let tool = self.tool;
            let Some(shape) = self.selected_shape_mut() else {
                return;
            };
            let before = shape.clone();
            if matches!(tool, 7 | 8) {
                *shape = VectorShape::new(shape.kind, drag.start, p);
            } else if tool == 9 {
                let k = &mut shape.knots[drag.knot];
                k.outgoing = p;
                k.incoming = [2.0 * k.anchor[0] - p[0], 2.0 * k.anchor[1] - p[1]];
            } else if tool == 10 {
                shape.kind = ShapeKind::Path;
                let k = &mut shape.knots[drag.knot];
                match drag.handle {
                    0 => k.move_anchor(p),
                    1 => k.incoming = p,
                    _ => k.outgoing = p,
                }
            }
            if *shape != before {
                self.refresh_shape();
            }
        } else if event == 2 {
            if self.shape_drag.take().is_some() && self.tool != 9 {
                self.shape_finish(true, false);
            }
        } else if event == 3 {
            self.shape_finish(false, false);
        }
    }

    pub(super) fn shape_finish(&mut self, commit: bool, closed: bool) {
        if self.shape_session.take().is_none() {
            return;
        }
        self.shape_drag = None;
        self.pending_transform_boundary = None;
        let valid = self.selected_shape().is_some_and(|s| {
            s.validate()
                && (s.kind == ShapeKind::Path
                    || s.bounds()
                        .is_some_and(|b| b.2 - b.0 > s.stroke_width && b.3 - b.1 > s.stroke_width))
        });
        if commit && valid {
            if self.tool == 9
                && let Some(shape) = self.selected_shape_mut()
            {
                shape.closed = closed;
            }
            if self.tool == 9 && closed {
                self.refresh_shape();
            }
            self.commit_history();
            if self.tool == 9 {
                self.tool = 10;
            }
        } else if let Some((before, _)) = self.pending_history.take() {
            self.restore_snapshot(before);
        }
        self.sync_view();
    }

    pub(super) fn shape_style(&mut self, field: i32, text: &str, value: f32) {
        if self.shape_session.is_some() || self.editing_vector_mask() {
            return;
        }
        let color = || {
            let s = text.trim().trim_start_matches('#');
            if s.len() != 6 || !s.is_ascii() {
                return None;
            }
            Some([
                u8::from_str_radix(&s[0..2], 16).ok()?,
                u8::from_str_radix(&s[2..4], 16).ok()?,
                u8::from_str_radix(&s[4..6], 16).ok()?,
                255,
            ])
        };
        let Some(shape) = self
            .document
            .as_mut()
            .and_then(|d| d.layer_mut(self.active?))
            .and_then(|l| l.vector.as_mut())
        else {
            return;
        };
        let before = shape.clone();
        match field {
            0 => {
                if let Some(c) = color() {
                    shape.fill = Some(c)
                }
            }
            1 => {
                if let Some(c) = color() {
                    shape.stroke = Some(c)
                }
            }
            2 => {
                if value.is_finite() {
                    shape.stroke_width = value.clamp(0.0, 1024.0)
                }
            }
            3 => {
                shape.fill = if value > 0.0 {
                    Some([255, 95, 61, 255])
                } else {
                    None
                }
            }
            4 => {
                shape.stroke = if value > 0.0 {
                    Some([30, 30, 34, 255])
                } else {
                    None
                }
            }
            5 => shape.closed = value > 0.0,
            _ => {}
        }
        if *shape != before {
            self.refresh_shape();
        }
    }

    pub(crate) fn publish_shape_controls(&self, app: &crate::App) {
        use slint::{ModelRc, VecModel};
        let controls = app.global::<crate::CanvasControls>();
        let shape = self.selected_shape();
        controls.set_shape_selected(shape.is_some() && !self.paint_mask);
        self.publish_vector_mask_controls(app);
        self.publish_psd_controls(app);
        controls.set_shape_drawing(self.shape_session.is_some() && self.tool == 9);
        controls.set_shape_editing(self.shape_session.is_some());
        let hex = |c: Option<[u8; 4]>| {
            let c = c.unwrap_or([30, 30, 34, 255]);
            format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]).into()
        };
        controls.set_shape_fill(shape.is_some_and(|s| s.fill.is_some()));
        controls.set_shape_stroke(shape.is_some_and(|s| s.stroke.is_some()));
        controls.set_shape_closed(shape.is_some_and(|s| s.closed));
        controls.set_shape_fill_hex(hex(shape.and_then(|s| s.fill)));
        controls.set_shape_stroke_hex(hex(shape.and_then(|s| s.stroke)));
        controls.set_shape_width(shape.map_or(3.0, |s| s.stroke_width));
        let mut points = Vec::new();
        let mut lines = Vec::new();
        if matches!(self.tool, 9 | 10)
            && let Some(shape) = shape
        {
            for k in &shape.knots {
                let screen = [k.anchor, k.incoming, k.outgoing]
                    .map(|p| self.shape_screen_point(p).unwrap_or_default());
                for p in screen {
                    points.push(crate::CanvasCurvePoint {
                        x: p.0 as f32,
                        y: p.1 as f32,
                    });
                }
                for p in &screen[1..] {
                    lines.push(crate::CanvasCurveSegment {
                        x: screen[0].0 as f32,
                        y: screen[0].1 as f32,
                        x2: p.0 as f32,
                        y2: p.1 as f32,
                    });
                }
            }
        }
        controls.set_shape_points(ModelRc::new(VecModel::from(points)));
        controls.set_shape_lines(ModelRc::new(VecModel::from(lines)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pane() -> CanvasPane {
        let mut pane = CanvasPane {
            document: Some(ImageDocument::new(300, 200)),
            ..CanvasPane::default()
        };
        pane.nav.set_document(Some((300.0, 200.0)));
        pane.nav
            .viewport_mut()
            .resize((300.0, 200.0), 1.0, Some((300.0, 200.0)));
        pane
    }
    fn rectangle(p: &mut CanvasPane) {
        p.set_tool(7);
        p.shape_pointer(0, 20.0, 30.0);
        p.shape_pointer(1, 160.0, 120.0);
        p.shape_pointer(2, 160.0, 120.0);
    }
    #[test]
    fn shape_drag_cancel_history_and_cache_update() {
        let mut p = pane();
        let before = p.snapshot().unwrap();
        p.set_tool(8);
        p.shape_pointer(0, 20.0, 30.0);
        p.shape_pointer(1, 160.0, 120.0);
        assert!(p.has_uncommitted_edit());
        assert!(p.save_data().is_err());
        p.shape_pointer(3, 160.0, 120.0);
        assert!(p.snapshot().unwrap().same_state(&before));
        rectangle(&mut p);
        assert_eq!(p.undo_stack.len(), 1);
        assert!(!p.has_uncommitted_edit());
        let created = p.snapshot().unwrap();
        p.undo();
        assert!(p.snapshot().unwrap().same_state(&before));
        p.redo();
        assert!(p.snapshot().unwrap().same_state(&created));
        let id = p.active.unwrap();
        let pixels = p.layer.unwrap();
        let old = p.store.get(pixels).unwrap();
        p.begin_history();
        p.shape_style(0, "#2030A0", 0.0);
        p.commit_history();
        assert_ne!(p.store.get(pixels).unwrap().pixels(), old.pixels());
        assert_eq!(p.hit_image(80.0, 80.0), Some(id));
        assert!(
            p.paint_target().is_none(),
            "paint may not mutate the derived shape cache"
        );
        p.undo();
        assert!(p.snapshot().unwrap().same_state(&created));
    }
    #[test]
    fn shape_path_boundary_and_transformed_handle_edit() {
        let mut p = pane();
        p.set_tool(9);
        p.shape_pointer(0, 20.0, 30.0);
        p.shape_pointer(1, 40.0, 30.0);
        p.shape_pointer(2, 40.0, 30.0);
        p.shape_pointer(0, 160.0, 100.0);
        p.shape_pointer(1, 180.0, 80.0);
        p.shape_pointer(2, 180.0, 80.0);
        assert_eq!(p.selected_shape().unwrap().knots.len(), 2);
        assert_eq!(p.undo_stack.len(), 0);
        assert!(p.route_transform_boundary(CanvasMsg::Tool(0)).is_none());
        assert!(p.transform_confirmation_open());
        p.resolve_transform_boundary(0);
        p.shape_finish(true, false);
        assert_eq!(p.undo_stack.len(), 1);
        assert_eq!(p.tool, 10);
        let id = p.active.unwrap();
        p.document
            .as_mut()
            .unwrap()
            .layer_mut(id)
            .unwrap()
            .transform
            .rotation = 0.3;
        p.nav
            .viewport_mut()
            .set_zoom(2.0, (150.0, 100.0), (300.0, 200.0));
        p.sync_view();
        let point = p.selected_shape().unwrap().knots[0].outgoing;
        let a = p.shape_screen_point(point).unwrap();
        let target = [point[0] + 12.0, point[1] - 7.0];
        let b = p.shape_screen_point(target).unwrap();
        p.shape_pointer(0, a.0, a.1);
        p.shape_pointer(1, b.0, b.1);
        p.shape_pointer(2, b.0, b.1);
        let actual = p.selected_shape().unwrap().knots[0].outgoing;
        assert!((actual[0] - target[0]).abs() < 0.01 && (actual[1] - target[1]).abs() < 0.01);
        assert_eq!(p.undo_stack.len(), 2);
    }
    #[test]
    fn shape_project_reopens_geometry_and_rebuilds_cache() {
        let mut p = pane();
        rectangle(&mut p);
        let id = p.active.unwrap();
        let expected = p.selected_shape().unwrap().clone();
        let dir =
            std::env::temp_dir().join(format!("seecut-shape-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let path = dir.join("editable.comp");
        let composed = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
        let png = dir.join("shapes.png");
        p.agent_export_png(&png).unwrap();
        let (_, _, rgba) = decode_png(&std::fs::read(png).unwrap(), None).unwrap();
        assert_eq!(rgba, composed.pixels());
        let provenance = canvas_export_provenance(&p.save_data().unwrap(), None).unwrap();
        assert_eq!(
            provenance.parameters["layers"][0]["vector"]["kind"],
            "Rectangle"
        );
        p.agent_save_comp(&path).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["concat-project"], 2);
        let pixels = p.layer.unwrap();
        drop(p);
        let mut reopened = CanvasPane::default();
        reopened.load_comp(&path, None).unwrap();
        assert_eq!(reopened.active, Some(id));
        assert_eq!(reopened.selected_shape(), Some(&expected));
        assert_eq!(
            reopened.store.get(pixels).unwrap().pixels(),
            expected.rasterize(300, 200).unwrap().pixels()
        );
        reopened.set_tool(10);
        let a = reopened
            .shape_screen_point(expected.knots[0].anchor)
            .unwrap();
        reopened.shape_pointer(0, a.0, a.1);
        reopened.shape_pointer(3, a.0, a.1);
        assert_eq!(reopened.selected_shape(), Some(&expected));
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
