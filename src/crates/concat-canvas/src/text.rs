// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serializable text source. Layer transforms and pixels stay in the existing document.
use concat_text::{Align, TitleStyle};
use serde::{Deserialize, Serialize};

/// Versioned text content and style; its layer pixels are derived.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasText {
    /// Geometry/style format version.
    pub version: u32,
    /// Requested font and editable words; fallback never rewrites this.
    pub style: TitleStyle,
}
impl CanvasText {
    /// A readable title at the document centre.
    pub fn new(height: u32) -> Self {
        Self {
            version: 1,
            style: TitleStyle {
                content: "文字".into(),
                font_family: "Helvetica".into(),
                font_size: 48.0 / f64::from(height.max(1)),
                font_weight: 400.0,
                italic: false,
                color: "#244580".into(),
                align: Align::Center,
                stroke_width: 0.0,
                stroke_color: "#FFFFFF".into(),
                shadow: false,
                background: String::new(),
                line_height: 1.2,
                tracking: 0.0,
                max_width: 0.8,
            },
        }
    }
    /// Relative source values remain compatible with v1 documents. Bounds admit
    /// the inspector pixel ranges even on a one-pixel document; raster allocation
    /// is separately bounded by the layer renderer.
    pub fn validate(&self) -> bool {
        let s = &self.style;
        let color = |v: &str| {
            matches!(v.len(), 7 | 9)
                && v.starts_with('#')
                && v[1..].bytes().all(|c| c.is_ascii_hexdigit())
        };
        self.version == 1
            && s.content.len() <= 32768
            && s.font_family.len() <= 256
            && s.font_size.is_finite()
            && s.font_size > 0.0
            && s.font_size <= 1000.0
            && s.font_weight.is_finite()
            && (100.0..=900.0).contains(&s.font_weight)
            && s.stroke_width.is_finite()
            && (0.0..=100.0).contains(&s.stroke_width)
            && s.line_height.is_finite()
            && (0.5..=5.0).contains(&s.line_height)
            && s.tracking.is_finite()
            && (-20.0..=20.0).contains(&s.tracking)
            && s.max_width.is_finite()
            && (0.0..=1.0).contains(&s.max_width)
            && color(&s.color)
            && color(&s.stroke_color)
            && (s.background.is_empty() || color(&s.background))
    }
    /// Rebuild straight-alpha pixels with installed font fallback, without PNG encoding.
    pub fn rasterize(&self, width: u32, height: u32) -> Result<concat_core::frame::Frame, String> {
        if !self.validate() {
            return Err("无效文字样式".into());
        }
        let r = concat_text::render_layer_frame(
            concat_text::system_fonts(),
            &self.style,
            width,
            height,
        )
        .map_err(|e| e.to_string())?;
        concat_core::frame::Frame::from_rgba(r.width, r.height, r.rgba)
            .ok_or_else(|| "无法创建文字预览".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editable_text_source_is_versioned_and_exclusive_with_vector() {
        let mut doc = crate::ImageDocument::new(320, 180);
        let mut store = crate::PixelStore::new();
        let pixels = store.put(concat_core::frame::Frame::transparent(320, 180));
        let id = doc.new_layer("Text", pixels);
        doc.layer_mut(id).unwrap().text = Some(Box::new(CanvasText::new(180)));
        doc.validate().unwrap();
        let back: crate::ImageDocument = serde_json::from_str(&doc.to_json().unwrap()).unwrap();
        assert_eq!(back, doc);
        let layer = doc.layer_mut(id).unwrap();
        layer.vector = Some(crate::vector::VectorShape::new(
            crate::vector::ShapeKind::Rectangle,
            [0.0, 0.0],
            [10.0, 10.0],
        ));
        assert!(doc.validate().is_err());
        let layer = doc.layer_mut(id).unwrap();
        layer.vector = None;
        layer.text.as_mut().unwrap().version = 2;
        assert!(doc.validate().is_err());
    }
    #[test]
    fn text_export_provenance_keeps_exact_float_values_after_json_reload() {
        let mut text = CanvasText::new(360);
        text.style.line_height = 1.4_f32 as f64;
        text.style.tracking = 0.5 / 360.0;
        let original = serde_json::json!({"text": text, "rotation": 0.12_f32});
        let encoded = serde_json::to_vec(&original).unwrap();
        let restored: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, original);
        assert_eq!(serde_json::to_vec(&restored).unwrap(), encoded);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn long_mixed_line_keeps_all_pixels_when_alignment_changes() {
        let mut t = CanvasText::new(240);
        t.style.content = "中文 English 中文 English".into();
        t.style.font_size = 40.0 / 240.0;
        t.style.stroke_width = 2.0 / 240.0;
        t.style.shadow = true;
        let measure =
            concat_text::render_frame(concat_text::system_fonts(), &t.style, 640, 240).unwrap();
        assert!(
            (384..512).contains(&measure.block_width),
            "representative line: {}",
            measure.block_width
        );
        let mass = |f: &concat_core::frame::Frame| {
            f.pixels().chunks_exact(4).map(|p| p[3] as u64).sum::<u64>()
        };
        let center = t.rasterize(640, 240).unwrap();
        for align in [Align::Left, Align::Right] {
            t.style.align = align;
            let frame = t.rasterize(640, 240).unwrap();
            assert!(
                mass(&frame) as f64 > mass(&center) as f64 * 0.98,
                "{align:?} clipped: {} / {}",
                mass(&frame),
                mass(&center)
            );
            // A transparent guard ring proves stroke/blur were not cut at cache edges.
            for x in 0..frame.width() {
                assert_eq!(frame.pixel(x, 0).unwrap()[3], 0);
                assert_eq!(frame.pixel(x, frame.height() - 1).unwrap()[3], 0);
            }
            for y in 0..frame.height() {
                assert_eq!(frame.pixel(0, y).unwrap()[3], 0);
                assert_eq!(frame.pixel(frame.width() - 1, y).unwrap()[3], 0);
            }
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn pixel_sizes_work_on_hd_4k_and_small_canvases() {
        for height in [64, 1080, 2160] {
            let mut t = CanvasText::new(height);
            t.style.content = "Ab中".into();
            t.style.font_size = 8.0 / height as f64;
            assert!(t.validate(), "8px at {height}");
            let frame = t.rasterize(320, height).unwrap();
            let rows: Vec<_> = (0..frame.height())
                .filter(|y| (0..frame.width()).any(|x| frame.pixel(x, *y).unwrap()[3] > 0))
                .collect();
            assert!(
                (5..=12).contains(&rows.len()),
                "8px must not be silently clamped: {height} -> {} rows",
                rows.len()
            );
            t.style.content = "A".into();
            t.style.font_size = 6.0 / height as f64;
            t.style.stroke_width = 100.0 / height as f64;
            t.style.tracking = -20.0 / height as f64;
            assert!(
                t.validate(),
                "inspector minimum font/max stroke at {height}"
            );
            assert!(t.rasterize(320, height).is_ok());
            t.style.font_size = 1000.0 / height as f64;
            t.style.stroke_width = 0.0;
            t.style.tracking = 20.0 / height as f64;
            assert!(t.validate(), "1000px at {height}");
            assert!(t.rasterize(320, height).is_ok());
        }
    }
}
