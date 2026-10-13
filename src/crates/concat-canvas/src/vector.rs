// SPDX-License-Identifier: AGPL-3.0-or-later
// Knot layout and rectangle/ellipse construction adapted from PhotoCraft
// 48c17214704d4768ad7b1f28c5d2767b1d87110c (MIT).
// Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors.
// See THIRD_PARTY_NOTICES.md. SeeCut uses its existing layer/history/pixel store.

//! Editable cubic geometry. Pixels are a disposable cache, never the source.
use concat_core::frame::Frame;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
/// A cubic anchor with absolute incoming and outgoing controls.
pub struct Knot {
    /// Anchor in intrinsic layer pixels.
    pub anchor: [f32; 2],
    /// Incoming cubic control in intrinsic layer pixels.
    pub incoming: [f32; 2],
    /// Outgoing cubic control in intrinsic layer pixels.
    pub outgoing: [f32; 2],
}

impl Knot {
    /// Creates a sharp anchor with coincident controls.
    pub fn corner(point: [f32; 2]) -> Self {
        Self {
            anchor: point,
            incoming: point,
            outgoing: point,
        }
    }

    /// Moves an anchor and both attached controls by the same delta.
    pub fn move_anchor(&mut self, point: [f32; 2]) {
        for (axis, coordinate) in point.iter().enumerate() {
            let delta = coordinate - self.anchor[axis];
            self.incoming[axis] += delta;
            self.outgoing[axis] += delta;
        }
        self.anchor = point;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Original primitive, or freely editable path.
pub enum ShapeKind {
    /// Axis-aligned rectangle before layer transform.
    Rectangle,
    /// Four cubic ellipse segments.
    Ellipse,
    /// An open or closed free path.
    Path,
}

/// Coordinates are in the layer's intrinsic bitmap space. The normal layer
/// transform applies exactly once, shared by rendering, handles and hit tests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VectorShape {
    /// Geometry format version.
    pub version: u32,
    /// Primitive identity; editing knots converts it to a path.
    pub kind: ShapeKind,
    /// Ordered cubic anchors.
    pub knots: Vec<Knot>,
    /// Connect the last anchor back to the first.
    pub closed: bool,
    /// Optional straight-alpha RGBA interior.
    pub fill: Option<[u8; 4]>,
    /// Optional straight-alpha RGBA outline.
    pub stroke: Option<[u8; 4]>,
    /// Outline width in intrinsic pixels.
    pub stroke_width: f32,
}

impl VectorShape {
    /// Constructs a primitive between two corners, or starts a one-anchor path.
    pub fn new(kind: ShapeKind, a: [f32; 2], b: [f32; 2]) -> Self {
        let (x, y) = (a[0].min(b[0]), a[1].min(b[1]));
        let (w, h) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
        let knots = match kind {
            ShapeKind::Rectangle => [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
                .map(Knot::corner)
                .to_vec(),
            ShapeKind::Ellipse => {
                let (rx, ry) = (w / 2.0, h / 2.0);
                let (cx, cy) = (x + rx, y + ry);
                let (kx, ky) = (rx * 0.552_284_8, ry * 0.552_284_8);
                vec![
                    Knot {
                        anchor: [cx, cy - ry],
                        incoming: [cx - kx, cy - ry],
                        outgoing: [cx + kx, cy - ry],
                    },
                    Knot {
                        anchor: [cx + rx, cy],
                        incoming: [cx + rx, cy - ky],
                        outgoing: [cx + rx, cy + ky],
                    },
                    Knot {
                        anchor: [cx, cy + ry],
                        incoming: [cx + kx, cy + ry],
                        outgoing: [cx - kx, cy + ry],
                    },
                    Knot {
                        anchor: [cx - rx, cy],
                        incoming: [cx - rx, cy + ky],
                        outgoing: [cx - rx, cy - ky],
                    },
                ]
            }
            ShapeKind::Path => vec![Knot::corner(a)],
        };
        Self {
            version: 1,
            kind,
            knots,
            closed: kind != ShapeKind::Path,
            fill: if kind == ShapeKind::Path {
                None
            } else {
                Some([255, 95, 61, 255])
            },
            stroke: Some([30, 30, 34, 255]),
            stroke_width: 3.0,
        }
    }

    /// Checks saved geometry and finite numeric limits.
    pub fn validate(&self) -> bool {
        self.version == 1
            && (2..=4096).contains(&self.knots.len())
            && self.stroke_width.is_finite()
            && (0.0..=1024.0).contains(&self.stroke_width)
            && self.knots.iter().all(|k| {
                k.anchor
                    .iter()
                    .chain(&k.incoming)
                    .chain(&k.outgoing)
                    .all(|v| v.is_finite() && v.abs() <= 1_000_000.0)
            })
    }

    /// Conservative control-point bounds expanded by the stroke radius.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let first = self.knots.first()?.anchor;
        let mut bounds = (first[0], first[1], first[0], first[1]);
        for p in self
            .knots
            .iter()
            .flat_map(|k| [k.anchor, k.incoming, k.outgoing])
        {
            bounds.0 = bounds.0.min(p[0]);
            bounds.1 = bounds.1.min(p[1]);
            bounds.2 = bounds.2.max(p[0]);
            bounds.3 = bounds.3.max(p[1]);
        }
        let pad = if self.stroke.is_some() {
            self.stroke_width / 2.0
        } else {
            0.0
        };
        Some((
            bounds.0 - pad,
            bounds.1 - pad,
            bounds.2 + pad,
            bounds.3 + pad,
        ))
    }

    /// Rebuilds the disposable straight-alpha cache for both compositors.
    pub fn rasterize(&self, width: u32, height: u32) -> Option<Frame> {
        use tiny_skia::{
            FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
        };
        let mut pixmap = Pixmap::new(width, height)?;
        if self.knots.len() >= 2 {
            let mut builder = PathBuilder::new();
            let first = self.knots[0].anchor;
            builder.move_to(first[0], first[1]);
            let count = if self.closed {
                self.knots.len()
            } else {
                self.knots.len() - 1
            };
            for i in 0..count {
                let a = self.knots[i];
                let b = self.knots[(i + 1) % self.knots.len()];
                builder.cubic_to(
                    a.outgoing[0],
                    a.outgoing[1],
                    b.incoming[0],
                    b.incoming[1],
                    b.anchor[0],
                    b.anchor[1],
                );
            }
            if self.closed {
                builder.close();
            }
            if let Some(path) = builder.finish() {
                let mut paint = Paint {
                    anti_alias: true,
                    ..Default::default()
                };
                if let Some(c) = self.fill {
                    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
                    pixmap.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
                if let Some(c) = self.stroke.filter(|_| self.stroke_width > 0.0) {
                    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
                    let stroke = Stroke {
                        width: self.stroke_width,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Default::default()
                    };
                    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        // Reuse Skia's buffer. Only partial-alpha pixels in the geometry's
        // bounds need conversion; opaque and untouched pixels are already
        // straight-alpha. Include one pixel for antialias coverage.
        let mut rgba = pixmap.take();
        if let Some((left, top, right, bottom)) = self.bounds() {
            let x0 = (left.floor() - 1.0).clamp(0.0, width as f32) as usize;
            let y0 = (top.floor() - 1.0).clamp(0.0, height as f32) as usize;
            let x1 = (right.ceil() + 1.0).clamp(0.0, width as f32) as usize;
            let y1 = (bottom.ceil() + 1.0).clamp(0.0, height as f32) as usize;
            let stride = width as usize * 4;
            for y in y0..y1 {
                for pixel in rgba[y * stride + x0 * 4..y * stride + x1 * 4].chunks_exact_mut(4) {
                    if pixel[3] != 0 && pixel[3] != 255 {
                        let c = tiny_skia::PremultipliedColorU8::from_rgba(
                            pixel[0], pixel[1], pixel[2], pixel[3],
                        )?
                        .demultiply();
                        pixel[0] = c.red();
                        pixel[1] = c.green();
                        pixel[2] = c.blue();
                    }
                }
            }
        }
        Frame::from_rgba(width, height, rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Composer, ImageDocument, PixelStore};

    #[test]
    fn shape_geometry_survives_document_json_and_rebuilds_identical_pixels() {
        for kind in [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Path] {
            let mut shape = VectorShape::new(kind, [8.0, 8.0], [48.0, 40.0]);
            if kind == ShapeKind::Path {
                shape.knots[0].outgoing = [8.0, 40.0];
                shape.knots.push(Knot {
                    anchor: [48.0, 8.0],
                    incoming: [48.0, 40.0],
                    outgoing: [48.0, 8.0],
                });
            }
            assert!(shape.validate());
            let pixels = shape.rasterize(64, 48).unwrap();
            assert!(pixels.pixels().chunks_exact(4).any(|p| p[3] > 0));
            let mut store = PixelStore::new();
            let id = store.put(pixels);
            let mut doc = ImageDocument::new(64, 48);
            let layer = doc.new_layer("shape", id);
            doc.layer_mut(layer).unwrap().vector = Some(shape);
            doc.validate().unwrap();
            let back: ImageDocument = serde_json::from_str(&doc.to_json().unwrap()).unwrap();
            let crate::LayerNode::Layer(layer) = back.find(layer).unwrap() else {
                panic!()
            };
            assert_eq!(
                layer
                    .vector
                    .as_ref()
                    .unwrap()
                    .rasterize(64, 48)
                    .unwrap()
                    .pixels(),
                store.get(id).unwrap().pixels()
            );
            assert_eq!(
                Composer::new(&doc, &store).compose().pixels(),
                Composer::new(&back, &store).compose().pixels()
            );
        }
    }

    #[test]
    fn shape_fill_outline_and_transparency_produce_straight_alpha() {
        let mut shape = VectorShape::new(ShapeKind::Rectangle, [8.0, 8.0], [40.0, 40.0]);
        shape.stroke = None;
        shape.fill = Some([240, 60, 30, 128]);
        let pixels = shape.rasterize(48, 48).unwrap();
        let p = &pixels.pixels()[(24 * 48 + 24) * 4..][..4];
        assert!((p[0] as i32 - 240).abs() <= 1);
        assert_eq!(p[3], 128);
        shape.fill = None;
        shape.stroke = Some([255, 255, 255, 255]);
        shape.stroke_width = 4.0;
        let pixels = shape.rasterize(48, 48).unwrap();
        assert_eq!(pixels.pixels()[(24 * 48 + 24) * 4 + 3], 0);
        assert_eq!(pixels.pixels()[(24 * 48 + 8) * 4 + 3], 255);
    }

    #[test]
    fn shape_anchor_controls_and_validation() {
        let mut shape = VectorShape::new(ShapeKind::Ellipse, [10.0, 10.0], [50.0, 30.0]);
        let old = shape.knots[0];
        shape.knots[0].move_anchor([35.0, 13.0]);
        assert_eq!(shape.knots[0].incoming[0], old.incoming[0] + 5.0);
        assert_eq!(shape.knots[0].outgoing[1], old.outgoing[1] + 3.0);
        assert!(shape.validate());
        shape.knots[0].incoming[0] = f32::NAN;
        assert!(!shape.validate());
        shape.knots[0] = old;
        shape.version = 2;
        assert!(!shape.validate());
    }
}
