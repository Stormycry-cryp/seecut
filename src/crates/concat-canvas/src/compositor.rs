// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// the layer compositing walk - groups, masks, clipping links and adjustment
// layers - here over Concat's Frame rather than Core Graphics.

//! The CPU reference compositor: a document plus its pixels in, a finished
//! [`Frame`] out.
//!
//! This is the correctness baseline every GPU path is diffed against, the
//! same discipline `concat-render` keeps. It is not the shipping path: it
//! recomposes the whole canvas from scratch on every call, which is exactly
//! what the port must not do while a brush stroke is going down. The GPU
//! compositor keeps layer textures resident and recomposites dirty tiles
//! only; this one exists so there is always a slower, obviously-right
//! answer to compare against.
//!
//! The walk follows the tree the way the source does. Within a group,
//! children composite back to front; an adjustment rewrites everything
//! beneath it in place; a group's own opacity, blend mode and mask apply to
//! its finished composite as a unit. A clipping link multiplies a layer's
//! alpha by the coverage of the layer it names, computed in document
//! coordinates and cached per composition.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use concat_core::Frame;

use crate::blend::{BlendMode, blend_pixel};
use crate::document::{
    Adjustment, ImageDocument, ImageLayer, LayerGroup, LayerMask, LayerNode, LayerSampling,
};
use crate::pixels::PixelStore;

/// A canvas-sized buffer of straight-alpha RGBA pixels, the working form
/// between compositing steps. [`Frame`] is the finished form only.
struct Buffer {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

impl Buffer {
    fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0, 0, 0, 0]; (width as usize) * (height as usize)],
        }
    }

    fn to_frame(&self) -> Frame {
        let mut flat = Vec::with_capacity(self.pixels.len() * 4);
        for pixel in &self.pixels {
            flat.extend_from_slice(pixel);
        }
        Frame::from_rgba(self.width, self.height, flat).expect("built to exact size")
    }

    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y as usize) * (self.width as usize) + (x as usize)]
    }

    fn set(&mut self, x: u32, y: u32, pixel: [u8; 4]) {
        self.pixels[(y as usize) * (self.width as usize) + (x as usize)] = pixel;
    }
}

/// A missing pixel id resolves to nothing at all: an empty layer draws
/// nothing, which keeps a half-built document composable while it is being
/// edited.
fn fetch(store: &PixelStore, id: crate::pixels::PixelId) -> Option<Arc<Frame>> {
    store.get(id)
}

/// One composition pass over a document. Holds the clip-coverage cache, so
/// make one per composition and drop it when the document changes.
pub struct Composer<'a> {
    document: &'a ImageDocument,
    store: &'a PixelStore,
    clip_cache: RefCell<HashMap<crate::document::LayerId, Arc<Vec<f32>>>>,
}

/// Composites the whole document onto a transparent canvas and returns the
/// finished frame, canvas-sized whatever the layers' own sizes are.
pub fn compose(document: &ImageDocument, store: &PixelStore) -> Frame {
    Composer::new(document, store).compose()
}

impl<'a> Composer<'a> {
    /// A composer over one document state.
    pub fn new(document: &'a ImageDocument, store: &'a PixelStore) -> Self {
        Self {
            document,
            store,
            clip_cache: RefCell::new(HashMap::new()),
        }
    }

    /// The finished canvas.
    pub fn compose(&self) -> Frame {
        let mut canvas = Buffer::transparent(self.document.width, self.document.height);
        self.compose_group_into(&self.document.root, &mut canvas);
        canvas.to_frame()
    }

    /// Visible coverage at one document pixel, including ancestor group
    /// masks and clipping links. Used by canvas picking so holes are click-through.
    pub fn hit_coverage(&self, id: crate::document::LayerId, x: u32, y: u32) -> f32 {
        if x >= self.document.width || y >= self.document.height {
            return 0.0;
        }
        let mut stack = vec![id];
        let Some(mut weight) = self.layer_coverage_at(id, x, y, &mut stack) else {
            return 0.0;
        };
        fn ancestors(
            composer: &Composer<'_>,
            group: &LayerGroup,
            id: crate::document::LayerId,
            x: u32,
            y: u32,
        ) -> Option<f32> {
            for child in &group.children {
                if child.id() == id {
                    return Some(1.0);
                }
                if let LayerNode::Group(inner) = child
                    && let Some(weight) = ancestors(composer, inner, id, x, y)
                {
                    if inner.hidden {
                        return Some(0.0);
                    }
                    let mask = inner
                        .mask
                        .as_ref()
                        .filter(|mask| mask.enabled)
                        .map_or(1.0, |mask| composer.mask_coverage(mask, None, x, y));
                    return Some(weight * inner.opacity * mask);
                }
            }
            None
        }
        weight *= ancestors(self, &self.document.root, id, x, y).unwrap_or(0.0);
        weight
    }

    fn layer_coverage_at(
        &self,
        id: crate::document::LayerId,
        x: u32,
        y: u32,
        stack: &mut Vec<crate::document::LayerId>,
    ) -> Option<f32> {
        let LayerNode::Layer(layer) = self.document.find(id)? else {
            return None;
        };
        if layer.hidden {
            return None;
        }
        let frame = fetch(self.store, layer.pixels)?;
        let mut weight = sample_alpha(
            &frame,
            &layer.transform,
            layer.sampling,
            self.document.width,
            self.document.height,
            x,
            y,
        ) * layer.opacity;
        if let Some(mask) = layer.mask.as_ref().filter(|mask| mask.enabled) {
            weight *= self.mask_coverage(mask, Some(&layer.transform), x, y);
        }
        if let Some(base) = layer.clips_to {
            if stack.contains(&base) {
                return Some(0.0);
            }
            stack.push(base);
            if let Some(base_weight) = self.layer_coverage_at(base, x, y, stack) {
                weight *= base_weight;
            }
            stack.pop();
        }
        Some(weight)
    }

    // ----- the walk -------------------------------------------------

    fn compose_group_into(&self, group: &LayerGroup, backdrop: &mut Buffer) {
        for child in &group.children {
            if child.hidden() {
                continue;
            }
            match child {
                LayerNode::Layer(layer) => self.blend_layer(layer, backdrop),
                LayerNode::Group(inner) => {
                    let mut composite = Buffer::transparent(backdrop.width, backdrop.height);
                    self.compose_group_into(inner, &mut composite);
                    self.blend_composite(
                        inner.opacity,
                        inner.blend,
                        inner.mask.as_ref(),
                        &composite,
                        backdrop,
                    );
                }
                LayerNode::Adjustment(adjustment) => {
                    self.apply_adjustment(adjustment, backdrop);
                }
            }
        }
    }

    /// One bitmap layer over the backdrop: sample, mask, clip, blend.
    /// The bitmap is always resampled through its transform, which centres
    /// it on the canvas - a bitmap of the canvas' own size lands exactly
    /// aligned, a smaller one is cropped to the middle, a larger one
    /// letterboxes.
    fn blend_layer(&self, layer: &ImageLayer, backdrop: &mut Buffer) {
        let Some(frame) = fetch(self.store, layer.pixels) else {
            return;
        };
        let bitmap = self.sample_transformed(&frame, &layer.transform, layer.sampling);
        let clip = layer.clips_to.and_then(|base| self.coverage_of(base));
        for y in 0..backdrop.height {
            for x in 0..backdrop.width {
                let source = bitmap.at(x, y);
                if source[3] == 0 {
                    continue;
                }
                let mut weight = layer.opacity;
                if let Some(mask) = layer.mask.as_ref().filter(|mask| mask.enabled) {
                    weight *= self.mask_coverage(mask, Some(&layer.transform), x, y);
                }
                if let Some(coverage) = &clip {
                    weight *= coverage[(y as usize) * (backdrop.width as usize) + (x as usize)];
                }
                if weight <= 0.0 {
                    continue;
                }
                let out = blend_pixel(layer.blend, backdrop.at(x, y), source, weight);
                backdrop.set(x, y, out);
            }
        }
    }

    /// A finished group composite over the backdrop, its appearance applied
    /// as a unit.
    fn blend_composite(
        &self,
        opacity: f32,
        blend: BlendMode,
        mask: Option<&LayerMask>,
        composite: &Buffer,
        backdrop: &mut Buffer,
    ) {
        for y in 0..backdrop.height {
            for x in 0..backdrop.width {
                let source = composite.at(x, y);
                if source[3] == 0 {
                    continue;
                }
                let mut weight = opacity;
                if let Some(mask) = mask.filter(|mask| mask.enabled) {
                    weight *= self.mask_coverage(mask, None, x, y);
                }
                if weight <= 0.0 {
                    continue;
                }
                let out = blend_pixel(blend, backdrop.at(x, y), source, weight);
                backdrop.set(x, y, out);
            }
        }
    }

    /// An adjustment layer rewrites the backdrop beneath it, weighted by its
    /// opacity and mask. Its blend mode decides how the adjusted colour
    /// meets what was there; `Normal` is the plain weighted mix.
    fn apply_adjustment(
        &self,
        adjustment: &crate::document::AdjustmentLayer,
        backdrop: &mut Buffer,
    ) {
        for y in 0..backdrop.height {
            for x in 0..backdrop.width {
                let here = backdrop.at(x, y);
                if here[3] == 0 {
                    continue;
                }
                let adjusted = apply_one(&adjustment.adjustment, here);
                if adjusted == here {
                    continue;
                }
                let mut weight = adjustment.opacity;
                if let Some(mask) = adjustment.mask.as_ref().filter(|mask| mask.enabled) {
                    weight *= self.mask_coverage(mask, None, x, y);
                }
                if weight <= 0.0 {
                    continue;
                }
                let out = if adjustment.blend == BlendMode::Normal {
                    mix(here, adjusted, weight)
                } else {
                    blend_pixel(adjustment.blend, here, adjusted, weight)
                };
                backdrop.set(x, y, out);
            }
        }
    }

    // ----- coverage -------------------------------------------------

    /// The document-space coverage of a layer: how much of each canvas pixel
    /// it holds, from its bitmap alpha through its transform, its opacity,
    /// its mask and any clip link of its own. The colour does not
    /// contribute - this is what a clipping mask multiplies by.
    ///
    /// `None` when the name resolves to nothing drawable - a dangling link
    /// mid-edit clips nothing rather than everything.
    fn coverage_of(&self, id: crate::document::LayerId) -> Option<Arc<Vec<f32>>> {
        if let Some(cached) = self.clip_cache.borrow().get(&id) {
            return Some(Arc::clone(cached));
        }
        let width = self.document.width;
        let height = self.document.height;
        let LayerNode::Layer(layer) = self.document.find(id)? else {
            return None;
        };
        if layer.hidden {
            return None;
        }
        let bitmap = fetch(self.store, layer.pixels)?;
        let mut coverage = vec![0.0_f32; (width as usize) * (height as usize)];
        if let Some(mask) = layer.mask.as_ref().filter(|mask| mask.enabled) {
            for y in 0..height {
                for x in 0..width {
                    let alpha = sample_alpha(
                        &bitmap,
                        &layer.transform,
                        layer.sampling,
                        width,
                        height,
                        x,
                        y,
                    );
                    let index = (y as usize) * (width as usize) + (x as usize);
                    coverage[index] = alpha
                        * layer.opacity
                        * self.mask_coverage(mask, Some(&layer.transform), x, y);
                }
            }
        } else {
            for y in 0..height {
                for x in 0..width {
                    let alpha = sample_alpha(
                        &bitmap,
                        &layer.transform,
                        layer.sampling,
                        width,
                        height,
                        x,
                        y,
                    );
                    coverage[(y as usize) * (width as usize) + (x as usize)] =
                        alpha * layer.opacity;
                }
            }
        }
        // The base's own clip link multiplies through, so a chain of
        // clipping layers narrows together.
        if let Some(above) = layer
            .clips_to
            .and_then(|upstream| self.coverage_of(upstream))
        {
            for (index, slot) in coverage.iter_mut().enumerate() {
                *slot *= above[index];
            }
        }
        let coverage = Arc::new(coverage);
        self.clip_cache
            .borrow_mut()
            .insert(id, Arc::clone(&coverage));
        Some(coverage)
    }

    /// A mask's coverage at a canvas pixel: the mask bitmap's red channel,
    /// sampled in document coordinates.
    fn mask_coverage(
        &self,
        mask: &LayerMask,
        layer: Option<&crate::document::LayerTransform>,
        x: u32,
        y: u32,
    ) -> f32 {
        let Some(frame) = fetch(self.store, mask.pixels) else {
            return 1.0;
        };
        if mask.linked
            && let (Some(current), Some(anchor)) = (layer, mask.anchor)
        {
            let bitmap = (frame.width() as f32, frame.height() as f32);
            let canvas = (self.document.width as f32, self.document.height as f32);
            let point = (x as f32 + 0.5, y as f32 + 0.5);
            if let Some(local) = current.to_bitmap(point, bitmap, canvas)
                && let Some(old_document) = anchor.from_bitmap(local, bitmap, canvas)
            {
                if old_document.0 < 0.0 || old_document.1 < 0.0 {
                    return 1.0;
                }
                return sample_channel(&frame, old_document.0 as u32, old_document.1 as u32, 0);
            }
        }
        sample_channel(&frame, x, y, 0)
    }

    // ----- sampling -------------------------------------------------

    /// A bitmap redrawn onto the canvas through a transform. Layers whose
    /// transform is the identity take a cheaper path in `blend_layer`.
    fn sample_transformed(
        &self,
        frame: &Frame,
        transform: &crate::document::LayerTransform,
        sampling: LayerSampling,
    ) -> Buffer {
        let width = self.document.width;
        let height = self.document.height;
        let mut out = Buffer::transparent(width, height);
        for y in 0..height {
            for x in 0..width {
                let Some(pixel) = sample_pixel(frame, transform, sampling, width, height, x, y)
                else {
                    continue;
                };
                out.set(x, y, pixel);
            }
        }
        out
    }
}

// ----- transform sampling ----------------------------------------------

/// Maps a canvas point back into a bitmap's own coordinates, through the
/// inverse of the layer transform: translate, unrotate, unscale, unflip.
/// The layer's centre sits `transform.x/y` away from the canvas centre, so
/// the default transform lays a bitmap of the canvas' own size exactly over
/// it.
fn into_bitmap(
    transform: &crate::document::LayerTransform,
    bitmap_width: u32,
    bitmap_height: u32,
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
) -> Option<(f32, f32)> {
    let (bx, by) = transform.to_bitmap(
        (x as f32 + 0.5, y as f32 + 0.5),
        (bitmap_width as f32, bitmap_height as f32),
        (canvas_width as f32, canvas_height as f32),
    )?;
    (bx >= 0.0 && by >= 0.0 && bx < bitmap_width as f32 && by < bitmap_height as f32)
        .then_some((bx, by))
}

/// A bitmap pixel at a canvas point, `None` when the transform puts the
/// point off the bitmap.
fn sample_pixel(
    frame: &Frame,
    transform: &crate::document::LayerTransform,
    sampling: LayerSampling,
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
) -> Option<[u8; 4]> {
    let (bx, by) = into_bitmap(
        transform,
        frame.width(),
        frame.height(),
        canvas_width,
        canvas_height,
        x,
        y,
    )?;
    Some(match sampling {
        LayerSampling::Nearest => frame.pixel(bx as u32, by as u32)?,
        LayerSampling::Smooth => bilinear(frame, bx, by)?,
    })
}

/// Just the alpha at a canvas point, the cheap read the clip coverage uses.
fn sample_alpha(
    frame: &Frame,
    transform: &crate::document::LayerTransform,
    sampling: LayerSampling,
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
) -> f32 {
    let Some((bx, by)) = into_bitmap(
        transform,
        frame.width(),
        frame.height(),
        canvas_width,
        canvas_height,
        x,
        y,
    ) else {
        return 0.0;
    };
    let alpha = match sampling {
        LayerSampling::Nearest => frame.pixel(bx as u32, by as u32).map(|p| p[3]),
        LayerSampling::Smooth => bilinear(frame, bx, by).map(|p| p[3]),
    };
    alpha.map_or(0.0, |a| a as f32 / 255.0)
}

/// One channel of a mask bitmap at a canvas point, untransformed: masks read
/// in document coordinates. `0..4` picks the channel.
fn sample_channel(frame: &Frame, x: u32, y: u32, channel: usize) -> f32 {
    if x >= frame.width() || y >= frame.height() {
        // Off the mask is unmasked: a smaller mask covers only part of the
        // canvas, and the rest lets everything through.
        return 1.0;
    }
    frame.pixel(x, y).map_or(1.0, |p| p[channel] as f32 / 255.0)
}

/// Bilinear over the bitmap's own pixels, alpha-weighted so edges keep
/// their coverage instead of bleeding black.
fn bilinear(frame: &Frame, bx: f32, by: f32) -> Option<[u8; 4]> {
    let x0 = (bx - 0.5).floor() as i64;
    let y0 = (by - 0.5).floor() as i64;
    let fx = bx - 0.5 - x0 as f32;
    let fy = by - 0.5 - y0 as f32;
    let mut sum = [0.0_f32; 4];
    let mut weight = 0.0_f32;
    for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
        for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
            let (x, y) = (x0 + dx, y0 + dy);
            if x < 0 || y < 0 || x >= frame.width() as i64 || y >= frame.height() as i64 {
                continue;
            }
            let pixel = frame.pixel(x as u32, y as u32)?;
            let a = pixel[3] as f32 / 255.0;
            let w = wx * wy;
            if a > 0.0 {
                // Premultiply for the average, so transparent neighbours
                // contribute coverage but not colour.
                for channel in 0..4 {
                    let value = if channel == 3 {
                        pixel[3] as f32
                    } else {
                        pixel[channel] as f32 * a
                    };
                    sum[channel] += value * w;
                }
            }
            weight += w * a;
        }
    }
    if weight <= f32::EPSILON {
        return None;
    }
    let mut out = [0_u8; 4];
    for channel in 0..4 {
        let value = if channel == 3 {
            sum[3]
        } else {
            sum[channel] * 255.0 / weight / 255.0
        };
        out[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    Some(out)
}

// ----- adjustments ------------------------------------------------------

/// The adjustment math over one pixel, straight-alpha in and out. Alpha
/// passes through untouched: adjustments recolour, they do not cut.
pub fn apply_one(adjustment: &Adjustment, pixel: [u8; 4]) -> [u8; 4] {
    let rgb = [pixel[0], pixel[1], pixel[2]];
    let out = match adjustment {
        Adjustment::Invert => [255 - rgb[0], 255 - rgb[1], 255 - rgb[2]],
        Adjustment::Exposure { stops } => {
            let factor = 2.0_f32.powf(*stops);
            rgb.map(|c| (c as f32 * factor).round().clamp(0.0, 255.0) as u8)
        }
        Adjustment::Levels {
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
        } => {
            let span = (in_white - in_black).max(f32::EPSILON);
            rgb.map(|c| {
                let v = ((c as f32 / 255.0) - in_black) / span;
                let v = v.clamp(0.0, 1.0);
                let v = v.powf(1.0 / gamma.max(f32::EPSILON));
                ((out_black + v * (out_white - out_black)) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            })
        }
        Adjustment::Curves { red, green, blue } => {
            let channels = [red, green, blue];
            let mut out = [0_u8; 3];
            for (channel, points) in channels.iter().enumerate() {
                let v = curve_at(points, rgb[channel] as f32 / 255.0);
                out[channel] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            out
        }
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            let (mut h, mut s, mut l) = rgb_to_hsl(rgb);
            h = (h + hue / 360.0).rem_euclid(1.0);
            s = (s * (1.0 + saturation)).clamp(0.0, 1.0);
            l = (l + lightness).clamp(0.0, 1.0);
            hsl_to_rgb(h, s, l)
        }
        Adjustment::GradientMap { low, high } => {
            let t = luminance(rgb);
            let mut out = [0_u8; 3];
            for channel in 0..3 {
                let v = low[channel] + t * (high[channel] - low[channel]);
                out[channel] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            out
        }
        Adjustment::Grain { amount } => {
            // Deterministic hash noise: the same pixel always grains the
            // same way, so undo and recomposition agree.
            let n = hash_noise(pixel);
            let delta = (n - 0.5) * amount * 255.0;
            rgb.map(|c| (c as f32 + delta).round().clamp(0.0, 255.0) as u8)
        }
    };
    [out[0], out[1], out[2], pixel[3]]
}

/// The curve read at `v`, quantized to the byte the LUT texture carries.
/// Public because both backends build their curves from it - the GPU's
/// lookup table is these exact bytes.
pub fn curve_at_bytes(points: &[(f32, f32)], v: f32) -> u8 {
    (curve_at(points, v).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Rec.601 luma, the weight the gradient map and the grain read by.
fn luminance(rgb: [u8; 3]) -> f32 {
    (0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32) / 255.0
}

/// A piecewise-linear read of a curve at `v`, clamped at both ends. Points
/// are sorted by input; anything less is treated as the identity.
fn curve_at(points: &[(f32, f32)], v: f32) -> f32 {
    if points.len() < 2 {
        return v;
    }
    if v <= points[0].0 {
        return points[0].1;
    }
    for window in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (window[0], window[1]);
        if v <= x1 {
            let t = if x1 - x0 <= f32::EPSILON {
                0.0
            } else {
                (v - x0) / (x1 - x0)
            };
            return y0 + t * (y1 - y0);
        }
    }
    points[points.len() - 1].1
}

/// RGB in `0..=255` to HSL, all `0..=1`.
fn rgb_to_hsl(rgb: [u8; 3]) -> (f32, f32, f32) {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max - min <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() <= f32::EPSILON {
        (g - b) / d + (if g < b { 6.0 } else { 0.0 })
    } else if (max - g).abs() <= f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

/// HSL, all `0..=1`, back to RGB in `0..=255`.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s <= f32::EPSILON {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        (hue(h + 1.0 / 3.0) * 255.0).round().clamp(0.0, 255.0) as u8,
        (hue(h) * 255.0).round().clamp(0.0, 255.0) as u8,
        (hue(h - 1.0 / 3.0) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// A deterministic per-pixel noise in `0..=1`, steady across recompositions.
fn hash_noise(pixel: [u8; 4]) -> f32 {
    let mut h = 0x811c_9dc5_u32;
    for byte in pixel {
        h ^= byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h & 0x00ff_ffff) as f32 / 0x0100_0000 as f32
}

/// A straight-alpha mix, the `Normal` adjustment path: `weight` of the
/// adjusted pixel over what was there, colour-weighted by the target's own
/// alpha so a translucent backdrop does not darken.
fn mix(backdrop: [u8; 4], adjusted: [u8; 4], weight: f32) -> [u8; 4] {
    let a = backdrop[3] as f32 / 255.0 * weight;
    let mut out = [0_u8; 4];
    for channel in 0..3 {
        out[channel] = (backdrop[channel] as f32 * (1.0 - a) + adjusted[channel] as f32 * a)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out[3] = backdrop[3];
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ImageLayer, LayerTransform};

    impl Buffer {
        fn from_frame(frame: &Frame) -> Self {
            let width = frame.width();
            let height = frame.height();
            let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
            for chunk in frame.pixels().chunks_exact(4) {
                pixels.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            Self {
                width,
                height,
                pixels,
            }
        }
    }

    struct World {
        document: ImageDocument,
        store: PixelStore,
    }

    impl World {
        fn new() -> Self {
            Self {
                document: ImageDocument::new(4, 4),
                store: PixelStore::new(),
            }
        }

        fn with_layer(&mut self, name: &str, frame: Frame) -> crate::document::LayerId {
            self.document.new_layer(name, self.store.put(frame))
        }

        fn compose(&self) -> Buffer {
            let frame = compose(&self.document, &self.store);
            Buffer::from_frame(&frame)
        }
    }

    #[test]
    fn an_empty_document_is_transparent() {
        let world = World::new();
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [0, 0, 0, 0]);
        assert_eq!(canvas.at(3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn an_identity_layer_draws_its_pixels_where_they_land() {
        let mut world = World::new();
        // A 4x4 solid red over a 4x4 canvas: the default transform lays a
        // bitmap of the canvas' own size exactly over it.
        world.with_layer("Red", solid(4, 4, [255, 0, 0, 255]));
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [255, 0, 0, 255]);
        assert_eq!(canvas.at(3, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn layers_composite_back_to_front() {
        let mut world = World::new();
        world.with_layer("Back", solid(4, 4, [0, 0, 255, 255]));
        let front = world.with_layer("Front", solid(4, 4, [255, 0, 0, 255]));
        world.document.layer_mut(front).expect("layer").opacity = 0.5;
        let canvas = world.compose();
        // Red at half opacity over blue: the blend formula's answer.
        let out =
            crate::blend::blend_pixel(BlendMode::Normal, [0, 0, 255, 255], [255, 0, 0, 255], 0.5);
        assert_eq!(canvas.at(0, 0), out);
    }

    #[test]
    fn hidden_layers_draw_nothing() {
        let mut world = World::new();
        let id = world.with_layer("Ghost", solid(4, 4, [255, 0, 0, 255]));
        world.document.layer_mut(id).expect("layer").hidden = true;
        let canvas = world.compose();
        assert_eq!(canvas.at(2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn opacity_weighs_the_layer() {
        let mut world = World::new();
        world.with_layer("Back", solid(4, 4, [0, 0, 255, 255]));
        let id = world.with_layer("Front", solid(4, 4, [255, 0, 0, 255]));
        world.document.layer_mut(id).expect("layer").opacity = 0.25;
        let canvas = world.compose();
        let expected =
            crate::blend::blend_pixel(BlendMode::Normal, [0, 0, 255, 255], [255, 0, 0, 255], 0.25);
        assert_eq!(canvas.at(1, 1), expected);
    }

    #[test]
    fn a_mask_hides_where_it_is_black() {
        let mut world = World::new();
        world.with_layer("Base", solid(4, 4, [255, 0, 0, 255]));
        // A layer half-covered by a mask that is white on the left column
        // and black elsewhere.
        let mut mask_frame = solid(4, 4, [0, 0, 0, 255]);
        for y in 0..4 {
            mask_frame.set_pixel(0, y, [255, 255, 255, 255]);
        }
        let mask_id = world.store.put(mask_frame);
        let id = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        world.document.layer_mut(id).expect("layer").mask = Some(LayerMask::new(mask_id));
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [0, 255, 0, 255]);
        assert_eq!(canvas.at(1, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn a_disabled_mask_changes_nothing() {
        let mut world = World::new();
        world.with_layer("Base", solid(4, 4, [255, 0, 0, 255]));
        let mask_id = world.store.put(solid(4, 4, [0, 0, 0, 255]));
        let id = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        let mut mask = LayerMask::new(mask_id);
        mask.enabled = false;
        world.document.layer_mut(id).expect("layer").mask = Some(mask);
        let canvas = world.compose();
        assert_eq!(canvas.at(3, 3), [0, 255, 0, 255]);
    }

    #[test]
    fn a_clip_link_keeps_the_layer_where_its_base_is_opaque() {
        let mut world = World::new();
        // The base is half the canvas: its right half transparent.
        let base_pixels = {
            let mut frame = Frame::transparent(4, 4);
            for y in 0..4 {
                for x in 0..2 {
                    frame.set_pixel(x, y, [255, 255, 255, 255]);
                }
            }
            frame
        };
        let base = world.with_layer("Base", base_pixels);
        let top = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(base);
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [0, 255, 0, 255]);
        assert_eq!(canvas.at(3, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn a_hidden_base_drops_the_clip_rather_than_the_layer() {
        let mut world = World::new();
        let mut base_frame = Frame::transparent(4, 4);
        for y in 0..4 {
            for x in 0..2 {
                base_frame.set_pixel(x, y, [255, 0, 0, 255]);
            }
        }
        let base = world.with_layer("Base", base_frame);
        let top = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(base);
        world.document.layer_mut(base).expect("layer").hidden = true;
        let canvas = world.compose();
        // A hidden base contributes neither colour nor coverage; the layer
        // that clipped to it draws whole, exactly as if the link had been
        // cleared - the state an editor passes through mid-edit.
        assert_eq!(canvas.at(0, 0), [0, 255, 0, 255]);
        assert_eq!(canvas.at(3, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn group_opacity_applies_to_the_composite_as_a_unit() {
        let mut world = World::new();
        world.with_layer("Back", solid(4, 4, [0, 0, 255, 255]));
        let group_id = world.document.new_group("Folder");
        let red = world.document.mint_id();
        let green = world.document.mint_id();
        {
            let group = world.document.group_mut(group_id).expect("group");
            group
                .children
                .push(crate::document::LayerNode::Layer(ImageLayer::new(
                    red,
                    "Red",
                    world.store.put(solid(4, 4, [255, 0, 0, 255])),
                )));
            group
                .children
                .push(crate::document::LayerNode::Layer(ImageLayer::new(
                    green,
                    "Green",
                    world.store.put(solid(4, 4, [0, 255, 0, 255])),
                )));
        }
        world.document.group_mut(group_id).expect("group").opacity = 0.5;
        let canvas = world.compose();
        // Green, the top child, fully covers red inside the group; the
        // group's half opacity then sits green over blue.
        let expected =
            crate::blend::blend_pixel(BlendMode::Normal, [0, 0, 255, 255], [0, 255, 0, 255], 0.5);
        assert_eq!(canvas.at(2, 2), expected);
    }

    #[test]
    fn an_adjustment_rewrites_the_backdrop_beneath_it() {
        let mut world = World::new();
        world.with_layer("Photo", solid(4, 4, [100, 150, 200, 255]));
        world.document.new_adjustment("Invert", Adjustment::Invert);
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [155, 105, 55, 255]);
    }

    #[test]
    fn an_adjustment_only_touches_what_is_beneath_it() {
        let mut world = World::new();
        world.document.new_adjustment("Invert", Adjustment::Invert);
        world.with_layer("Above", solid(4, 4, [10, 20, 30, 255]));
        let canvas = world.compose();
        // The adjustment had nothing beneath it; the layer drawn after is
        // untouched.
        assert_eq!(canvas.at(0, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn an_adjustment_obeys_its_mask() {
        let mut world = World::new();
        world.with_layer("Photo", solid(4, 4, [100, 150, 200, 255]));
        let mut mask_frame = solid(4, 4, [0, 0, 0, 255]);
        mask_frame.set_pixel(1, 1, [255, 255, 255, 255]);
        let mask_id = world.store.put(mask_frame);
        let id = world.document.new_adjustment("Invert", Adjustment::Invert);
        world.document.adjustment_mut(id).expect("adjustment").mask = Some(LayerMask::new(mask_id));
        let canvas = world.compose();
        assert_eq!(canvas.at(1, 1), [155, 105, 55, 255]);
        assert_eq!(canvas.at(0, 0), [100, 150, 200, 255]);
    }

    #[test]
    fn an_adjustment_weighs_by_its_opacity() {
        let mut world = World::new();
        world.with_layer("Photo", solid(4, 4, [100, 100, 100, 255]));
        let id = world
            .document
            .new_adjustment("Half invert", Adjustment::Invert);
        world
            .document
            .adjustment_mut(id)
            .expect("adjustment")
            .opacity = 0.5;
        let canvas = world.compose();
        // Half way from 100 to 155.
        assert_eq!(canvas.at(0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn a_nested_group_composes_through_both_levels() {
        let mut world = World::new();
        world.with_layer("Back", solid(4, 4, [0, 0, 255, 255]));
        let outer = world.document.new_group("Outer");
        let inner = world.document.new_group("Inner");
        world.document.move_node(inner, Some(outer), 0);
        let dot = world.document.mint_id();
        {
            let inner_group = world.document.group_mut(inner).expect("inner");
            inner_group
                .children
                .push(crate::document::LayerNode::Layer(ImageLayer::new(
                    dot,
                    "Dot",
                    world.store.put(solid(4, 4, [255, 0, 0, 255])),
                )));
        }
        world.document.group_mut(outer).expect("outer").opacity = 0.5;
        let canvas = world.compose();
        let expected =
            crate::blend::blend_pixel(BlendMode::Normal, [0, 0, 255, 255], [255, 0, 0, 255], 0.5);
        assert_eq!(canvas.at(0, 0), expected);
    }

    // ----- transforms and sampling ---------------------------------

    #[test]
    fn translation_moves_the_layer() {
        let mut world = World::new();
        let id = world.with_layer("Dot", dot_frame());
        world.document.layer_mut(id).expect("layer").transform = LayerTransform {
            x: 2.0,
            y: 1.0,
            ..LayerTransform::default()
        };
        let canvas = world.compose();
        // The dot's aligned home is (1, 1); two right and one down of that
        // is (3, 2).
        assert_eq!(canvas.at(3, 2), [255, 255, 255, 255]);
        assert_eq!(canvas.at(1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn double_scale_with_nearest_sampling_makes_the_dot_a_block() {
        let mut world = World::new();
        let id = world.with_layer("Dot", dot_frame());
        world.document.layer_mut(id).expect("layer").transform = LayerTransform {
            x: 2.0,
            y: 2.0,
            scale_x: 2.0,
            scale_y: 2.0,
            ..LayerTransform::default()
        };
        world.document.layer_mut(id).expect("layer").sampling =
            crate::document::LayerSampling::Nearest;
        let canvas = world.compose();
        // The bitmap dot at (1, 1) sits one up-left of its own centre;
        // doubled and shifted (2, 2) from the canvas centre, its square
        // covers canvas pixels (2..4)x(2..4).
        assert_eq!(canvas.at(2, 2), [255, 255, 255, 255]);
        assert_eq!(canvas.at(3, 3), [255, 255, 255, 255]);
        assert_eq!(canvas.at(0, 0), [0, 0, 0, 0]);
        assert_eq!(canvas.at(1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn quarter_turn_rotation_moves_the_dot() {
        let mut world = World::new();
        let id = world.with_layer("Dot", Frame::transparent(4, 4));
        // The bitmap has its dot one right and one up of its centre
        // (3, 1) about (2, 2); a quarter turn clockwise about the canvas
        // centre carries that to one left and one down - (2, 3).
        let mut frame = Frame::transparent(4, 4);
        frame.set_pixel(3, 1, [255, 255, 255, 255]);
        *world.document.layer_mut(id).expect("layer") = ImageLayer {
            id,
            name: "Dot".into(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            transform: LayerTransform {
                rotation: std::f32::consts::FRAC_PI_2,
                ..LayerTransform::default()
            },
            mask: None,
            clips_to: None,
            sampling: crate::document::LayerSampling::Nearest,
            hidden: false,
            pixels: world.store.put(frame),
        };
        let canvas = world.compose();
        assert_eq!(canvas.at(2, 3), [255, 255, 255, 255]);
        assert_eq!(canvas.at(1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn a_horizontal_flip_mirrors_the_layer() {
        let mut world = World::new();
        let mut frame = Frame::transparent(4, 4);
        frame.set_pixel(0, 2, [255, 255, 255, 255]);
        let id = world.with_layer("Mark", frame);
        world.document.layer_mut(id).expect("layer").transform = LayerTransform {
            flip_h: true,
            ..LayerTransform::default()
        };
        world.document.layer_mut(id).expect("layer").sampling =
            crate::document::LayerSampling::Nearest;
        let canvas = world.compose();
        assert_eq!(canvas.at(3, 2), [255, 255, 255, 255]);
        assert_eq!(canvas.at(0, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn a_zero_scale_draws_nothing() {
        let mut world = World::new();
        let id = world.with_layer("Gone", solid(4, 4, [255, 0, 0, 255]));
        world.document.layer_mut(id).expect("layer").transform = LayerTransform {
            scale_x: 0.0,
            scale_y: 0.0,
            ..LayerTransform::default()
        };
        let canvas = world.compose();
        assert_eq!(canvas.at(2, 2), [0, 0, 0, 0]);
    }

    // ----- adjustment math ------------------------------------------

    fn adjust_pixel(adjustment: &Adjustment, rgb: [u8; 3]) -> [u8; 3] {
        let out = apply_one(adjustment, [rgb[0], rgb[1], rgb[2], 255]);
        [out[0], out[1], out[2]]
    }

    #[test]
    fn invert_flips_each_channel() {
        assert_eq!(
            adjust_pixel(&Adjustment::Invert, [10, 128, 200]),
            [245, 127, 55]
        );
    }

    #[test]
    fn exposure_doubles_per_stop() {
        let out = adjust_pixel(&Adjustment::Exposure { stops: 1.0 }, [40, 50, 60]);
        assert_eq!(out, [80, 100, 120]);
        let back = adjust_pixel(&Adjustment::Exposure { stops: -1.0 }, [80, 100, 120]);
        assert_eq!(back, [40, 50, 60]);
    }

    #[test]
    fn levels_stretch_the_range() {
        // in 0.25..0.75 to out 0..1 maps 64 -> 0 and 191 -> 255.
        let adjustment = Adjustment::Levels {
            in_black: 0.25,
            in_white: 0.75,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        let low = adjust_pixel(&adjustment, [64, 64, 64]);
        for channel in low {
            assert!(channel <= 1, "{low:?}");
        }
        let high = adjust_pixel(&adjustment, [191, 191, 191]);
        for channel in high {
            assert!(channel >= 254, "{high:?}");
        }
    }

    #[test]
    fn levels_clamp_out_of_range() {
        let adjustment = Adjustment::Levels {
            in_black: 0.5,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        assert_eq!(adjust_pixel(&adjustment, [10, 10, 10]), [0, 0, 0]);
    }

    #[test]
    fn a_two_point_curve_is_a_straight_line() {
        let identity = [(0.0, 0.0), (1.0, 1.0)];
        let adjustment = Adjustment::Curves {
            red: identity.to_vec(),
            green: identity.to_vec(),
            blue: identity.to_vec(),
        };
        for value in [0_u8, 64, 128, 200, 255] {
            assert_eq!(adjust_pixel(&adjustment, [value, value, value]), [value; 3]);
        }
    }

    #[test]
    fn a_curve_holds_its_endpoints_and_lerps_between() {
        let points = vec![(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
        let adjustment = Adjustment::Curves {
            red: points.clone(),
            green: points.clone(),
            blue: points.clone(),
        };
        // 128/255 sits just past the 0.5 knee, so the answer is just past
        // 0.25 of full scale - within a step of 64.
        let out = adjust_pixel(&adjustment, [128, 128, 128]);
        for channel in out {
            assert!((channel as i32 - 64).abs() <= 1, "{out:?}");
        }
    }

    #[test]
    fn hue_rotation_keeps_luma_grey_where_it_started() {
        // Pure grey has no hue to rotate; it must come back grey.
        let out = adjust_pixel(
            &Adjustment::HueSaturation {
                hue: 180.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            [128, 128, 128],
        );
        assert_eq!(out, [128, 128, 128]);
    }

    #[test]
    fn saturation_zero_greys_a_colour() {
        let out = adjust_pixel(
            &Adjustment::HueSaturation {
                hue: 0.0,
                saturation: -1.0,
                lightness: 0.0,
            },
            [200, 30, 40],
        );
        // HSL's own grey is its lightness: the mean of the extremes.
        let l = (200.0 + 30.0) / 2.0;
        for channel in out {
            assert!(
                (channel as f32 - l).abs() <= 1.0,
                "{out:?} vs lightness {l}"
            );
        }
    }

    #[test]
    fn a_gradient_map_lays_the_ramp_over_luminance() {
        let out = adjust_pixel(
            &Adjustment::GradientMap {
                low: [0.0, 0.0, 0.0],
                high: [1.0, 1.0, 1.0],
            },
            [100, 100, 100],
        );
        let l = (luminance([100, 100, 100]) * 255.0).round() as u8;
        assert_eq!(out, [l, l, l]);
    }

    #[test]
    fn grain_is_deterministic_and_bounded() {
        let adjustment = Adjustment::Grain { amount: 0.5 };
        let a = adjust_pixel(&adjustment, [100, 100, 100]);
        let b = adjust_pixel(&adjustment, [100, 100, 100]);
        assert_eq!(a, b, "the same pixel must grain the same way twice");
        for channel in a {
            assert!((channel as i32 - 100).abs() <= 128, "grain ran away: {a:?}");
        }
    }

    // ----- clip chains ----------------------------------------------

    #[test]
    fn a_clip_chain_narrows_through_both_links() {
        let mut world = World::new();
        // The base covers only column 0; two full-canvas layers clip to it
        // in a chain.
        let mut base_frame = Frame::transparent(4, 4);
        for y in 0..4 {
            base_frame.set_pixel(0, y, [255, 255, 255, 255]);
        }
        let base = world.with_layer("Base", base_frame);
        let middle = world.with_layer("Middle", solid(4, 4, [255, 255, 255, 255]));
        world.document.layer_mut(middle).expect("layer").clips_to = Some(base);
        let top = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(middle);
        let canvas = world.compose();
        // Top draws last, but only where the chain's coverage lives.
        assert_eq!(canvas.at(0, 0), [0, 255, 0, 255]);
        // Off the base's column, both clipped layers vanish - the middle
        // one's own full bitmap notwithstanding.
        assert_eq!(canvas.at(1, 0), [0, 0, 0, 0]);
        assert_eq!(canvas.at(2, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn clip_coverage_honours_the_base_opacity() {
        let mut world = World::new();
        let base = world.with_layer("Base", solid(4, 4, [255, 255, 255, 255]));
        world.document.layer_mut(base).expect("layer").opacity = 0.5;
        let top = world.with_layer("Top", solid(4, 4, [0, 0, 255, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(base);
        let canvas = world.compose();
        // The base's coverage is full but its opacity halves the clip
        // weight, so the clipped blue blends at half strength - over the
        // base itself, which also sits at half. The golden values are the
        // formula's, applied in the same two steps.
        let base_step =
            crate::blend::blend_pixel(BlendMode::Normal, [0, 0, 0, 0], [255, 255, 255, 255], 0.5);
        let expected =
            crate::blend::blend_pixel(BlendMode::Normal, base_step, [0, 0, 255, 255], 0.5);
        assert_eq!(canvas.at(2, 2), expected);
        // And the weight really was halved: had the clip ignored the base's
        // opacity, the blue would have landed at full strength over the
        // half-white base and come out bluer.
        let stronger =
            crate::blend::blend_pixel(BlendMode::Normal, base_step, [0, 0, 255, 255], 1.0);
        assert_ne!(expected, stronger);
    }

    #[test]
    fn linked_mask_hole_moves_with_the_image_and_remains_click_through() {
        let mut world = World::new();
        let id = world.with_layer("Photo", solid(4, 4, [200, 20, 20, 255]));
        let mut mask = solid(4, 4, [255, 255, 255, 255]);
        mask.set_pixel(1, 1, [0, 0, 0, 255]);
        let mut link = LayerMask::new(world.store.put(mask));
        link.anchor = Some(LayerTransform::default());
        let layer = world.document.layer_mut(id).expect("layer");
        layer.mask = Some(link);
        layer.transform.x = 1.0;
        let composer = Composer::new(&world.document, &world.store);
        assert_eq!(composer.hit_coverage(id, 2, 1), 0.0);
        assert!(composer.hit_coverage(id, 1, 1) > 0.0);
        assert_eq!(world.compose().at(2, 1)[3], 0);
    }

    #[test]
    fn picking_respects_a_parent_group_mask_and_clipping_source() {
        let mut world = World::new();
        let group = world.document.new_group("Group");
        let base = world.document.mint_id();
        let top = world.document.mint_id();
        let mut base_frame = solid(4, 4, [255, 255, 255, 255]);
        base_frame.set_pixel(2, 2, [0, 0, 0, 0]);
        let base_pixels = world.store.put(base_frame);
        let top_pixels = world.store.put(solid(4, 4, [200, 20, 20, 255]));
        let mut top_layer = ImageLayer::new(top, "Top", top_pixels);
        top_layer.clips_to = Some(base);
        let mut mask = solid(4, 4, [255, 255, 255, 255]);
        mask.set_pixel(1, 1, [0, 0, 0, 255]);
        let group_node = world.document.group_mut(group).expect("group");
        group_node.mask = Some(LayerMask::new(world.store.put(mask)));
        group_node
            .children
            .push(LayerNode::Layer(ImageLayer::new(base, "Base", base_pixels)));
        group_node.children.push(LayerNode::Layer(top_layer));
        let composer = Composer::new(&world.document, &world.store);
        assert!(composer.hit_coverage(top, 0, 0) > 0.0);
        assert_eq!(composer.hit_coverage(top, 1, 1), 0.0);
        assert_eq!(composer.hit_coverage(top, 2, 2), 0.0);
    }

    #[test]
    fn a_missing_clip_source_draws_the_layer_unclipped() {
        // validate() refuses this document, but the compositor must not
        // panic on a state an editor can pass through mid-edit.
        let mut world = World::new();
        let top = world.with_layer("Top", solid(4, 4, [0, 255, 0, 255]));
        world.document.layer_mut(top).expect("layer").clips_to =
            Some(crate::document::LayerId(9_999));
        let canvas = world.compose();
        assert_eq!(canvas.at(0, 0), [0, 255, 0, 255]);
    }

    // ----- helpers --------------------------------------------------

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Frame {
        let mut frame = Frame::transparent(width, height);
        frame.fill(rgba);
        frame
    }

    /// A 4x4 transparent frame with one white pixel at (1, 1).
    fn dot_frame() -> Frame {
        let mut frame = Frame::transparent(4, 4);
        frame.set_pixel(1, 1, [255, 255, 255, 255]);
        frame
    }
}
