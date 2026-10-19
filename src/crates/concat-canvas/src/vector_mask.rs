//! Closed editable mask geometry and the shared CPU/GPU coverage mapping.
use crate::{LayerMask, LayerTransform, vector::VectorShape};
use concat_core::frame::Frame;
use serde::{Deserialize, Serialize};

/// Geometry in intrinsic image pixels, or document pixels for groups/adjustments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorMask {
    /// Closed cubic geometry; styles are fixed to opaque fill and no stroke.
    pub geometry: VectorShape,
    /// Intrinsic cache width, independent of document resizing/placement.
    pub width: u32,
    /// Intrinsic cache height.
    pub height: u32,
}

impl VectorMask {
    /// Normalize a closed shape into a coverage source, never coloured pixels.
    pub fn new(mut geometry: VectorShape, width: u32, height: u32) -> Option<Self> {
        geometry.fill = Some([255; 4]);
        geometry.stroke = None;
        geometry.stroke_width = 0.0;
        let source = Self {
            geometry,
            width,
            height,
        };
        source.validate().then_some(source)
    }

    /// Reject malformed, open, non-finite or over-budget saved sources.
    pub fn validate(&self) -> bool {
        self.width > 0
            && self.height > 0
            && u64::from(self.width) * u64::from(self.height) <= 128 * 1024 * 1024
            && self.geometry.validate()
            && self.geometry.closed
            && self.geometry.knots.len() >= 3
            && self.geometry.fill == Some([255; 4])
            && self.geometry.stroke.is_none()
            && self.geometry.stroke_width == 0.0
    }

    /// Disposable grayscale coverage derived from geometric alpha, including AA.
    pub fn rasterize(&self) -> Option<Frame> {
        if !self.validate() {
            return None;
        }
        let frame = self.geometry.rasterize(self.width, self.height)?;
        let mut rgba = frame.pixels().to_vec();
        for p in rgba.chunks_exact_mut(4) {
            let a = p[3];
            p.copy_from_slice(&[a, a, a, 255]);
        }
        Frame::from_rgba(self.width, self.height, rgba)
    }
}

impl LayerMask {
    /// Vector masks hide outside their geometry; legacy bitmaps show outside it.
    pub fn outside_coverage(&self) -> f32 {
        if self.vector.is_some() {
            0.0
        } else {
            self.bitmap_bounds.map_or(1.0, |b| b.outside as f32 / 255.0)
        }
    }

    /// Document-pixel to cache-pixel affine map. The GPU receives the same six
    /// coefficients, normalized to canvas UV, so clipping and picking agree.
    pub fn coverage_mapping(
        &self,
        current: Option<&LayerTransform>,
        bitmap: (f32, f32),
        canvas: (f32, f32),
    ) -> Option<[f32; 6]> {
        let map = |p| {
            if self.vector.is_some() {
                match current {
                    Some(layer) => {
                        let placement = if self.linked {
                            *layer
                        } else {
                            self.anchor.unwrap_or(*layer)
                        };
                        placement.to_bitmap(p, bitmap, canvas)
                    }
                    None => Some(p),
                }
            } else if self.linked {
                if let (Some(layer), Some(anchor)) = (current, self.anchor) {
                    return anchor.from_bitmap(layer.to_bitmap(p, bitmap, canvas)?, bitmap, canvas);
                }
                Some(p)
            } else {
                Some(p)
            }
        };
        let cache = |p| {
            let (x, y) = map(p)?;
            Some(
                self.bitmap_bounds
                    .map_or((x, y), |b| (x - b.left as f32, y - b.top as f32)),
            )
        };
        let o = cache((0.0, 0.0))?;
        let x = cache((canvas.0, 0.0))?;
        let y = cache((0.0, canvas.1))?;
        Some([x.0 - o.0, x.1 - o.1, y.0 - o.0, y.1 - o.1, o.0, o.1])
    }

    /// Nearest cache sampling matches the existing mask contract on both paths.
    pub fn coverage(
        &self,
        frame: &Frame,
        current: Option<&LayerTransform>,
        canvas: (u32, u32),
        x: u32,
        y: u32,
    ) -> f32 {
        let Some(m) = self.coverage_mapping(
            current,
            (frame.width() as f32, frame.height() as f32),
            (canvas.0 as f32, canvas.1 as f32),
        ) else {
            return self.outside_coverage();
        };
        let u = (x as f32 + 0.5) / canvas.0 as f32;
        let v = (y as f32 + 0.5) / canvas.1 as f32;
        let px = m[0] * u + m[2] * v + m[4];
        let py = m[1] * u + m[3] * v + m[5];
        if !px.is_finite()
            || !py.is_finite()
            || px < 0.0
            || py < 0.0
            || px >= frame.width() as f32
            || py >= frame.height() as f32
        {
            return self.outside_coverage();
        }
        frame.pixels()[(py as usize * frame.width() as usize + px as usize) * 4] as f32 / 255.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Composer, ImageDocument, PixelStore, vector::ShapeKind};

    fn source(kind: ShapeKind) -> VectorMask {
        let mut shape = VectorShape::new(
            if kind == ShapeKind::Path {
                ShapeKind::Rectangle
            } else {
                kind
            },
            [2.25, 2.25],
            [9.75, 7.75],
        );
        shape.kind = kind;
        // A coloured transparent style must never become red-channel coverage.
        shape.fill = Some([0, 180, 50, 20]);
        VectorMask::new(shape, 12, 10).unwrap()
    }

    #[test]
    fn geometry_alpha_coverage_has_antialiasing_and_zero_outside() {
        for kind in [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Path] {
            let v = source(kind);
            let cache = v.rasterize().unwrap();
            assert_eq!(cache.pixel(6, 5).unwrap(), [255; 4]);
            assert_eq!(cache.pixel(0, 0).unwrap(), [0, 0, 0, 255]);
            assert!(
                cache
                    .pixels()
                    .chunks_exact(4)
                    .any(|p| p[0] > 0 && p[0] < 255)
            );
            let mut mask = LayerMask::new(crate::PixelStore::new().put(cache.clone()));
            mask.vector = Some(v);
            assert_eq!(mask.coverage(&cache, None, (16, 16), 15, 15), 0.0);
            mask.vector = None;
            assert_eq!(mask.coverage(&cache, None, (16, 16), 15, 15), 1.0);
        }
    }

    #[test]
    fn malformed_closed_source_and_shared_cache_are_rejected() {
        let v = source(ShapeKind::Rectangle);
        for case in 0..8 {
            let mut bad = v.clone();
            match case {
                0 => bad.geometry.closed = false,
                1 => bad.geometry.knots.truncate(2),
                2 => bad.geometry.knots[0].anchor[0] = f32::NAN,
                3 => bad.geometry.knots[0].outgoing[1] = 1_000_001.0,
                4 => bad.geometry.version = 2,
                5 => bad.geometry.knots = vec![bad.geometry.knots[0]; 4097],
                6 => bad.width = 0,
                _ => bad.width = u32::MAX,
            }
            assert!(!bad.validate());
            assert!(bad.rasterize().is_none());
        }
        let mut store = PixelStore::new();
        let pixels = store.put(v.rasterize().unwrap());
        let mut doc = ImageDocument::new(16, 16);
        let id = doc.new_layer("bad shared cache", pixels);
        let mut mask = LayerMask::new(pixels);
        mask.vector = Some(v);
        doc.layer_mut(id).unwrap().mask = Some(mask);
        assert!(doc.validate().is_err());
    }

    #[test]
    fn intrinsic_mapping_linked_detached_hit_and_clipping_share_coverage() {
        let mut store = PixelStore::new();
        let mut doc = ImageDocument::new(24, 20);
        let mut image = Frame::transparent(12, 10);
        image.fill([80, 100, 160, 128]);
        let id = doc.new_layer("image", store.put(image.clone()));
        let source = source(ShapeKind::Rectangle);
        let mut mask = LayerMask::new(store.put(source.rasterize().unwrap()));
        mask.vector = Some(source);
        let layer = doc.layer_mut(id).unwrap();
        layer.mask = Some(mask);
        layer.transform.x = 3.0;
        let composer = Composer::new(&doc, &store);
        assert!(composer.hit_coverage(id, 15, 10) > 0.49);
        assert_eq!(composer.hit_coverage(id, 6, 5), 0.0);
        let top = doc.new_layer("clip", store.put(image));
        doc.layer_mut(top).unwrap().clips_to = Some(id);
        let composer = Composer::new(&doc, &store);
        assert_eq!(composer.hit_coverage(top, 6, 5), 0.0);
        let layer = doc.layer_mut(id).unwrap();
        let mask = layer.mask.as_mut().unwrap();
        mask.linked = false;
        mask.anchor = Some(layer.transform);
        layer.transform.x = 6.0;
        let composer = Composer::new(&doc, &store);
        assert_eq!(composer.hit_coverage(id, 21, 10), 0.0);
        // Legacy no-anchor masks remain document-space at their first placement.
        let legacy = LayerMask::new(store.put(Frame::black(4, 4)));
        assert_eq!(
            legacy.coverage(
                &store.get(legacy.pixels).unwrap(),
                Some(&LayerTransform {
                    x: 7.0,
                    ..Default::default()
                }),
                (24, 20),
                1,
                1
            ),
            0.0
        );
    }
}
