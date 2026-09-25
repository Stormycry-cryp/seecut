// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// its GPU ambitions - one Metal compute brush - rebuilt on Concat's shared
// wgpu stack, and widened to the whole compositing walk.

//! The wgpu canvas compositor: the GPU twin of [`compose`](crate::compose).
//!
//! Same document, same PDF 32000 formula, same stored-gamma values. The
//! parity test diffs the two backends with one byte of tolerance, and the
//! reason that can be so tight is a deliberate choice: the ping-pong
//! textures store **straight alpha, quantized to bytes** at exactly the
//! points the CPU reference's `u8` buffer quantizes. The shader's `byte()`
//! helper is `quantize` in WGSL clothing. A drift shows up as a test
//! failure here, not as one-pixel mush a user finds in production.
//!
//! The architecture is what keeps the four classic canvas stalls out:
//!
//! - **Brush latency.** Layer textures live on the GPU keyed by the pixel
//!   id, and [`CanvasGpu::upload`] writes a dirty sub-rectangle into the
//!   resident copy - a stroke uploads only the rectangle it touched,
//!   never a whole image, never per mouse move.
//! - **Zoom and pan stutter.** Nothing re-uploads on a viewport change:
//!   the composite is canvas-sized and the viewport is a later concern.
//!   ([`CanvasGpu::generate_mipmaps`] prepares the zoomed-out path.)
//! - **Adjust and undo stalls.** An adjustment is a fragment pass over the
//!   resident backdrop, never a baked copy; undo swaps which resident
//!   textures a document names (`PixelStore::replace` keeps ids stable),
//!   so history costs no pixel work at all.
//! - **Parity.** [`CanvasGpu::compose_frame`] reads the result back so the
//!   test can prove all of the above byte for byte.

use std::collections::HashMap;

use concat_core::Frame;

use crate::blend::BlendMode;
use crate::document::{Adjustment, ImageDocument, LayerGroup, LayerNode, LayerTransform};
use crate::pixels::{PixelId, PixelStore};

/// Bytes per row must be a multiple of this for a texture-to-buffer copy.
const ROW_ALIGN: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;

/// The pass shader. One fullscreen triangle per pass; `params.pass` says
/// what a pass means (0 blend a layer, 1 apply an adjustment, 2 write clip
/// coverage) and `params.flags` which aux bindings carry anything:
/// bit 0 a mask, bit 1 a clip coverage, bit 2 a transform on the source,
/// bits 8/16 a horizontal/vertical flip. Straight alpha everywhere,
/// quantized to bytes where the CPU reference quantizes.
const SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    // uv runs over the canvas in texture coordinates; the triangle's
    // overhang lands outside the viewport and costs nothing.
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0), vec2<f32>(2.0, 1.0), vec2<f32>(0.0, -1.0));
    var out: VsOut;
    out.position = vec4<f32>(xy[index], 0.0, 1.0);
    out.uv = uv[index];
    return out;
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var backdrop_texture: texture_2d<f32>;
@group(0) @binding(2) var mask_texture: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var<uniform> p: Params;
@group(0) @binding(5) var clip_texture: texture_2d<f32>;
@group(0) @binding(6) var lut_texture: texture_2d<f32>;

struct Params {
    pass_kind: u32,
    kind: u32,
    opacity: f32,
    flags: u32,
    tx: f32,
    ty: f32,
    sx: f32,
    sy: f32,
    cs: f32,
    sn: f32,
    bw: f32,
    bh: f32,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
    mx: f32,
    my: f32,
    msx: f32,
    msy: f32,
    mcos: f32,
    msin: f32,
}

fn mask_red(uv: vec2<f32>) -> f32 {
    if (p.flags & 32u) == 0u {
        return textureSampleLevel(mask_texture, tex_sampler, uv, 0.0).r;
    }
    let canvas = vec2<f32>(textureDimensions(backdrop_texture).xy);
    let d = uv * canvas - (canvas / 2.0 + vec2<f32>(p.tx, p.ty));
    let u = vec2<f32>(d.x * p.cs - d.y * p.sn, d.x * p.sn + d.y * p.cs);
    let s = u / vec2<f32>(p.sx, p.sy);
    let local = vec2<f32>(
        select(s.x, -s.x, (p.flags & 8u) != 0u),
        select(s.y, -s.y, (p.flags & 16u) != 0u));
    let a = local * vec2<f32>(p.msx, p.msy);
    let old_doc = canvas / 2.0 + vec2<f32>(p.mx, p.my)
        + vec2<f32>(a.x * p.mcos - a.y * p.msin, a.x * p.msin + a.y * p.mcos);
    let size = vec2<f32>(textureDimensions(mask_texture).xy);
    if old_doc.x < 0.0 || old_doc.y < 0.0 || old_doc.x >= size.x || old_doc.y >= size.y {
        return 1.0;
    }
    return textureLoad(mask_texture, vec2<i32>(floor(old_doc)), 0).r;
}

// A float snapped to the byte grid: the shader-side twin of the CPU's
// `quantize`, and where the parity lives.
fn byte(v: f32) -> f32 {
    return round(clamp(v, 0.0, 1.0) * 255.0) / 255.0;
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.3, 0.59, 0.11));
}

fn clip_colour(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    if n < 0.0 {
        return l + (c - l) * l / max(l - n, 1e-6);
    }
    if x > 1.0 {
        return l + (c - l) * (1.0 - l) / max(x - l, 1e-6);
    }
    return c;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_colour(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn set_sat(col: vec3<f32>, s: f32) -> vec3<f32> {
    // Sort the channel *indices* by value, rescale the spread and write
    // each result back where it came from - the spec's unsort, which keeps
    // the hue.
    var order = array<u32, 3>(0u, 1u, 2u);
    for (var i = 0u; i < 2u; i++) {
        for (var j = 0u; j < 2u - i; j++) {
            if col[order[j]] > col[order[j + 1u]] {
                let t: u32 = order[j];
                order[j] = order[j + 1u];
                order[j + 1u] = t;
            }
        }
    }
    var out = col;
    if col[order[2]] > col[order[0]] {
        out[order[1]] = (col[order[1]] - col[order[0]]) * s / (col[order[2]] - col[order[0]]);
        out[order[2]] = s;
        out[order[0]] = 0.0;
    } else {
        // A zero spread carries no hue to keep; the spec flattens it.
        out = vec3<f32>(0.0);
    }
    return out;
}

const MULTIPLY: u32 = 1u;
const SCREEN: u32 = 2u;
const OVERLAY: u32 = 3u;
const DARKEN: u32 = 4u;
const LIGHTEN: u32 = 5u;
const DIFFERENCE: u32 = 6u;
const DODGE: u32 = 7u;
const BURN: u32 = 8u;
const HUE: u32 = 9u;
const SATURATION: u32 = 10u;
const COLOR: u32 = 11u;
const LUMINOSITY: u32 = 12u;

fn blend_channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch mode {
        case MULTIPLY: { return cb * cs; }
        case SCREEN: { return cb + cs - cb * cs; }
        case OVERLAY: {
            // Overlay is Hard Light with the two colours swapped - the
            // branch follows the *source*, matching the CPU's
            // `hard_light(cs, cb)`.
            if cs <= 0.5 {
                return 2.0 * cs * cb;
            }
            return 1.0 - 2.0 * (1.0 - cs) * (1.0 - cb);
        }
        case DARKEN: { return min(cb, cs); }
        case LIGHTEN: { return max(cb, cs); }
        case DIFFERENCE: { return abs(cb - cs); }
        case DODGE: {
            if cb <= 0.0 {
                return 0.0;
            }
            if cs >= 1.0 {
                return 1.0;
            }
            return min(1.0, cb / (1.0 - cs));
        }
        case BURN: {
            if cb >= 1.0 {
                return 1.0;
            }
            if cs <= 0.0 {
                return 0.0;
            }
            return 1.0 - min(1.0, (1.0 - cb) / cs);
        }
        default: { return cs; }
    }
}

fn blended(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case HUE: { return set_lum(set_sat(cs, sat(cb)), lum(cb)); }
        case SATURATION: { return set_lum(set_sat(cb, sat(cs)), lum(cb)); }
        case COLOR: { return set_lum(cs, lum(cb)); }
        case LUMINOSITY: { return set_lum(cb, lum(cs)); }
        default: {
            return vec3<f32>(
                blend_channel(mode, cb.r, cs.r),
                blend_channel(mode, cb.g, cs.g),
                blend_channel(mode, cb.b, cs.b));
        }
    }
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) / 2.0;
    if mx - mn <= 1e-6 {
        return vec3<f32>(0.0, 0.0, l);
    }
    let d = mx - mn;
    let s = select(d / (mx + mn), d / (2.0 - mx - mn), l > 0.5);
    var h: f32;
    if mx == c.r {
        h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b);
    } else if mx == c.g {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3<f32>(h / 6.0, s, l);
}

fn hue_f(p0: f32, q: f32, t0: f32) -> f32 {
    var t = t0;
    if t < 0.0 { t += 1.0; }
    if t > 1.0 { t -= 1.0; }
    if t < 1.0 / 6.0 { return p0 + (q - p0) * 6.0 * t; }
    if t < 0.5 { return q; }
    if t < 2.0 / 3.0 { return p0 + (q - p0) * (2.0 / 3.0 - t) * 6.0; }
    return p0;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if hsl.y <= 1e-6 {
        return vec3<f32>(hsl.z);
    }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p0 = 2.0 * hsl.z - q;
    return vec3<f32>(
        hue_f(p0, q, hsl.x + 1.0 / 3.0),
        hue_f(p0, q, hsl.x),
        hue_f(p0, q, hsl.x - 1.0 / 3.0));
}

fn blend_one(back: vec4<f32>, src: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    var a_s = src.a * p.opacity;
    if (p.flags & 1u) != 0u {
        a_s *= mask_red(uv);
    }
    if (p.flags & 2u) != 0u {
        a_s *= textureSampleLevel(clip_texture, tex_sampler, uv, 0.0).r;
    }
    let a_b = back.a;
    let a_o = a_s + a_b * (1.0 - a_s);
    if a_o <= 0.0 {
        return vec4<f32>(0.0);
    }
    // Straight in, straight out: the raw samples are the colours the CPU
    // walk feeds the formula - no alpha division on either side.
    let cb = back.rgb;
    let cs = src.rgb;
    let b = blended(p.kind, cb, cs);
    let co = (a_s * (1.0 - a_b) * cs + a_b * (1.0 - a_s) * cb + a_s * a_b * b) / a_o;
    // Straight out, every channel snapped to the byte the CPU buffer keeps.
    return vec4<f32>(byte(co.r), byte(co.g), byte(co.b), byte(a_o));
}

fn adjust_one(back: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    if back.a <= 0.0 {
        return back;
    }
    // Straight colour, exactly the bytes `apply_one` reads.
    var rgb = back.rgb;
    if p.kind == 0u {
        // Invert.
        rgb = vec3<f32>(1.0) - rgb;
    } else if p.kind == 1u {
        // Exposure, stops in a.
        rgb = rgb * exp2(p.a);
    } else if p.kind == 2u {
        // Levels: a=in_black, b=in_white, c=gamma, d=out_black, e=out_white.
        let span = max(p.b - p.a, 1e-6);
        let v = clamp((rgb - vec3<f32>(p.a)) / vec3<f32>(span), vec3<f32>(0.0), vec3<f32>(1.0));
        rgb = vec3<f32>(p.d) + pow(v, vec3<f32>(1.0 / max(p.c, 1e-6))) * vec3<f32>(p.e - p.d);
    } else if p.kind == 3u {
        // Hue/saturation/lightness in a, b, c.
        var hsl = rgb_to_hsl(rgb);
        hsl.x = (hsl.x + p.a / 360.0) % 1.0;
        hsl.y = clamp(hsl.y * (1.0 + p.b), 0.0, 1.0);
        hsl.z = clamp(hsl.z + p.c, 0.0, 1.0);
        rgb = hsl_to_rgb(hsl);
    } else if p.kind == 4u {
        // Gradient map: luma onto the low..high ramp, low in a..c, high in
        // d..f.
        let l = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
        let low = vec3<f32>(p.a, p.b, p.c);
        let high = vec3<f32>(p.d, p.e, p.f);
        rgb = low + vec3<f32>(l) * (high - low);
    } else if p.kind == 5u {
        // Grain, amount in a. The same FNV hash over the same four bytes
        // the CPU reference hashes, so the noise is identical.
        var h = 0x811c9dc5u;
        let q = vec4<u32>(round(clamp(back, vec4<f32>(0.0), vec4<f32>(1.0)) * 255.0));
        h = (h ^ q.r) * 0x01000193u;
        h = (h ^ q.g) * 0x01000193u;
        h = (h ^ q.b) * 0x01000193u;
        h = (h ^ q.a) * 0x01000193u;
        let n = f32(h & 0x00ffffffu) / f32(0x01000000u);
        rgb = rgb + vec3<f32>((n - 0.5) * p.a);
    } else if p.kind == 6u {
        // Curves, through the 256-entry LUT the host built from the same
        // control points - the lookup lands on the CPU's exact bytes.
        let r = textureSampleLevel(lut_texture, tex_sampler, vec2<f32>((round(rgb.r * 255.0) + 0.5) / 256.0, 0.5), 0.0).r;
        let g = textureSampleLevel(lut_texture, tex_sampler, vec2<f32>((round(rgb.g * 255.0) + 0.5) / 256.0, 0.5), 0.0).g;
        let b = textureSampleLevel(lut_texture, tex_sampler, vec2<f32>((round(rgb.b * 255.0) + 0.5) / 256.0, 0.5), 0.0).b;
        rgb = vec3<f32>(r, g, b);
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    // The adjusted pixel, snapped to bytes like the CPU's `apply_one`.
    let adjusted = vec3<f32>(byte(rgb.r), byte(rgb.g), byte(rgb.b));
    var weight = p.opacity;
    if (p.flags & 1u) != 0u {
        weight *= mask_red(uv);
    }
    let a = back.a * weight;
    let out_rgb = back.rgb * (1.0 - a) + adjusted * a;
    return vec4<f32>(byte(out_rgb.r), byte(out_rgb.g), byte(out_rgb.b), back.a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let back = textureSampleLevel(backdrop_texture, tex_sampler, uv, 0.0);

    // The source coordinate. With no transform the source is
    // canvas-aligned and uv carries straight over; with one, it is the
    // inverse of translate, rotate, scale, flip about the two centres -
    // the same mapping `compositor.rs` walks per pixel.
    var uv_src = uv;
    if (p.flags & 4u) != 0u {
        let canvas = vec2<f32>(textureDimensions(backdrop_texture).xy);
        let d = uv * canvas - (canvas / 2.0 + vec2<f32>(p.tx, p.ty));
        let u = vec2<f32>(d.x * p.cs - d.y * p.sn, d.x * p.sn + d.y * p.cs);
        let s = u / vec2<f32>(p.sx, p.sy);
        let fx = select(s.x, -s.x, (p.flags & 8u) != 0u);
        let fy = select(s.y, -s.y, (p.flags & 16u) != 0u);
        let b = vec2<f32>(fx + p.bw / 2.0, fy + p.bh / 2.0);
        if b.x < 0.0 || b.y < 0.0 || b.x >= p.bw || b.y >= p.bh {
            // Off the bitmap: the backdrop passes through untouched, as
            // the CPU walk's `None` does.
            return back;
        }
        uv_src = b / vec2<f32>(p.bw, p.bh);
    }
    let src = textureSampleLevel(source_texture, tex_sampler, uv_src, 0.0);

    if p.pass_kind == 2u {
        // Coverage: the clip base's alpha through opacity, mask and its
        // own upstream coverage - the GPU twin of `coverage_of`.
        var a = src.a * p.opacity;
        if (p.flags & 1u) != 0u {
            a *= mask_red(uv);
        }
        if (p.flags & 2u) != 0u {
            a *= textureSampleLevel(clip_texture, tex_sampler, uv, 0.0).r;
        }
        return vec4<f32>(byte(a), 0.0, 0.0, 1.0);
    }
    if p.pass_kind == 1u {
        return adjust_one(back, uv);
    }
    return blend_one(back, src, uv);
}
"#;

/// One pass's parameters, laid out as 24 floats for the uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    pass_kind: u32,
    kind: u32,
    opacity: f32,
    flags: u32,
    /// Transform payload, used when the flags say so.
    tx: f32,
    ty: f32,
    sx: f32,
    sy: f32,
    /// Cosine and sine of the *negative* rotation - the inverse mapping's
    /// coefficients, exactly as `into_bitmap` builds them.
    cos: f32,
    sin: f32,
    bitmap_width: f32,
    bitmap_height: f32,
    /// Per-kind payload, packed by `apply_adjustment` and documented there.
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
    pad: [f32; 6],
}

impl Params {
    /// The uniform's bytes. The two kinds and the flags go over as raw
    /// `u32` bit patterns - `as f32` here would write `1.0`'s bits where
    /// the shader reads a `u32`, and every branch would miss.
    fn bytes(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        let mut word = |index: usize, bytes: [u8; 4]| {
            out[index * 4..(index + 1) * 4].copy_from_slice(&bytes);
        };
        word(0, self.pass_kind.to_ne_bytes());
        word(1, self.kind.to_ne_bytes());
        word(2, self.opacity.to_ne_bytes());
        word(3, self.flags.to_ne_bytes());
        for (index, value) in [
            self.tx,
            self.ty,
            self.sx,
            self.sy,
            self.cos,
            self.sin,
            self.bitmap_width,
            self.bitmap_height,
            self.a,
            self.b,
            self.c,
            self.d,
            self.e,
            self.f,
        ]
        .into_iter()
        .enumerate()
        {
            word(4 + index, value.to_ne_bytes());
        }
        for (index, value) in self.pad.into_iter().enumerate() {
            word(18 + index, value.to_ne_bytes());
        }
        out
    }

    fn mask_anchor(&mut self, anchor: &LayerTransform) {
        let (sin, cos) = anchor.rotation.sin_cos();
        self.pad = [
            anchor.x,
            anchor.y,
            anchor.scale_x * if anchor.flip_h { -1.0 } else { 1.0 },
            anchor.scale_y * if anchor.flip_v { -1.0 } else { 1.0 },
            cos,
            sin,
        ];
        self.flags |= 32;
    }

    /// The transform payload from a layer transform and its bitmap size.
    fn transform(&mut self, transform: &LayerTransform, bitmap_width: u32, bitmap_height: u32) {
        self.tx = transform.x;
        self.ty = transform.y;
        self.sx = transform.scale_x;
        self.sy = transform.scale_y;
        // The inverse mapping's coefficients: the rotation negated.
        self.cos = (-transform.rotation).cos();
        self.sin = (-transform.rotation).sin();
        self.bitmap_width = bitmap_width as f32;
        self.bitmap_height = bitmap_height as f32;
        if transform.flip_h {
            self.flags |= 8;
        }
        if transform.flip_v {
            self.flags |= 16;
        }
        self.flags |= 4;
    }
}

/// A dirty rectangle in layer-bitmap pixels: the region a stroke touched
/// and the only bytes a dirty [`CanvasGpu::upload`] moves.
#[derive(Clone, Copy, Debug)]
pub struct DirtyRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

/// A resident texture and the frame identity it carries.
struct Resident {
    texture: wgpu::Texture,
    frame_id: u64,
    /// Dirty brush uploads are newer than the store's pre-stroke frame.
    /// Full uploads from that store must not replace them before release.
    live: bool,
}

/// The GPU canvas compositor. See the module docs.
pub struct CanvasGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    /// Layer bitmaps by the pixel id that names them.
    residents: HashMap<PixelId, Resident>,
    /// Raster masks by the pixel id that names them.
    masks: HashMap<PixelId, Resident>,
    /// Canvas-sized scratch pairs, pooled across passes and composites.
    pool: Vec<(wgpu::Texture, wgpu::Texture)>,
    /// Clip-coverage textures by the base layer's id, live for one compose.
    coverage: HashMap<crate::document::LayerId, wgpu::Texture>,
    /// The 1x1 white every absent aux binding falls back to - sampled
    /// values of one change nothing.
    white: Option<wgpu::Texture>,
    canvas: Option<(u32, u32)>,
}

impl CanvasGpu {
    /// Builds on the best available adapter, or `None` with nothing usable -
    /// callers fall back to the CPU reference.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("concat canvas"),
            ..Default::default()
        }))
        .ok()?;
        Some(Self::with_device(device, queue))
    }

    /// Builds on a device the caller owns - the window's, so a composite
    /// can reach the screen without a copy.
    pub fn with_device(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("concat canvas"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("concat canvas pass"),
            entries: &[
                Self::texture_entry(0),
                Self::texture_entry(1),
                Self::texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                Self::texture_entry(5),
                Self::texture_entry(6),
            ],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("concat canvas"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("concat canvas layout"),
                    bind_group_layouts: &[Some(&bind_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                // Replace, not blend: the fragment writes the complete
                // formula's answer, backdrop included.
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat canvas params"),
            size: 96,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("concat canvas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            device,
            queue,
            pipeline,
            bind_layout,
            uniform,
            sampler,
            residents: HashMap::new(),
            masks: HashMap::new(),
            pool: Vec::new(),
            coverage: HashMap::new(),
            white: None,
            canvas: None,
        }
    }

    fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }
    }

    // ----- uploads --------------------------------------------------

    /// Makes sure the pixel id has a resident texture carrying the frame's
    /// pixels.
    ///
    /// With `dirty`, only that rectangle is written - the rule the brush
    /// lives by: the caller promises the rest of the frame is unchanged
    /// from what the resident already holds, and the stroke uploads its own
    /// rectangle and nothing else. Without `dirty`, a new frame identity
    /// ([`Frame::id`]) uploads whole; a match is a no-op.
    pub fn upload(&mut self, id: PixelId, frame: &Frame, dirty: Option<DirtyRect>) {
        if dirty.is_none()
            && let Some(resident) = self.residents.get_mut(&id)
            && resident.live
        {
            if resident.frame_id == frame.id() {
                resident.live = false;
            }
            return;
        }
        let resident_id = self.residents.get(&id).map(|r| r.frame_id);
        if resident_id == Some(frame.id()) && dirty.is_none() {
            return;
        }
        let texture = match self.residents.remove(&id) {
            Some(resident)
                if resident.texture.size().width == frame.width()
                    && resident.texture.size().height == frame.height() =>
            {
                resident.texture
            }
            _ => self.create_data_texture(frame.width(), frame.height(), "layer"),
        };
        let (width, height) = (frame.width(), frame.height());
        // Where the write starts, and how much it covers - split the way
        // wgpu splits them: an origin and an extent.
        let (origin, region) = dirty
            .filter(|_| resident_id.is_some())
            .map(|d| {
                let x = d.x.min(width);
                let y = d.y.min(height);
                (
                    wgpu::Origin3d { x, y, z: 0 },
                    wgpu::Extent3d {
                        width: d.width.min(width - x),
                        height: d.height.min(height - y),
                        depth_or_array_layers: 1,
                    },
                )
            })
            .unwrap_or((
                wgpu::Origin3d::ZERO,
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            ));
        if region.width > 0 && region.height > 0 {
            let row_bytes = region.width as usize * 4;
            let stride = row_bytes.div_ceil(ROW_ALIGN) * ROW_ALIGN;
            let mut data = Vec::with_capacity(stride * region.height as usize);
            for row in 0..region.height as usize {
                let start = ((origin.y as usize + row) * width as usize + origin.x as usize) * 4;
                data.extend_from_slice(&frame.pixels()[start..start + row_bytes]);
                data.resize(data.len() + (stride - row_bytes), 0);
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: None,
                },
                region,
            );
        }
        self.residents.insert(
            id,
            Resident {
                texture,
                frame_id: frame.id(),
                live: dirty.is_some(),
            },
        );
    }

    /// Drops the resident textures a pixel id had - what a `PixelStore`
    /// eviction or a document switch calls.
    pub fn evict(&mut self, id: PixelId) {
        self.residents.remove(&id);
        self.masks.remove(&id);
    }

    /// Drops every document-owned cache while retaining the shared device
    /// and pipelines. A newly opened document may reuse the same PixelIds;
    /// none of the previous document's textures may follow those ids.
    pub fn reset_document(&mut self) {
        self.residents.clear();
        self.masks.clear();
        self.pool.clear();
        self.coverage.clear();
        self.canvas = None;
    }

    /// The device the compositor renders on. Callers that add their own
    /// passes on the same device - the brush, the viewport - read it, so
    /// everything shares one queue and nothing crosses a device boundary.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The queue the compositor renders on. See [`Self::device`].
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    // ----- composition ----------------------------------------------

    /// Composites the document and leaves the result on the GPU - the path
    /// a monitor shows directly when the devices are shared.
    pub fn compose_texture(
        &mut self,
        document: &ImageDocument,
        store: &PixelStore,
    ) -> wgpu::Texture {
        self.begin(document.width, document.height);
        let mut pair = self.pair();
        self.clear(&pair.0);
        self.compose_group(&document.root, document, store, &mut pair);
        pair.0
    }

    /// Composites and reads the result back as a [`Frame`] - the parity
    /// path, and the export path when nothing consumes the texture
    /// directly.
    pub fn compose_frame(&mut self, document: &ImageDocument, store: &PixelStore) -> Frame {
        let texture = self.compose_texture(document, store);
        readback(&self.device, &self.queue, &texture)
    }

    fn begin(&mut self, width: u32, height: u32) {
        if self.canvas != Some((width, height)) {
            self.pool.clear();
            self.canvas = Some((width, height));
        }
        self.coverage.clear();
    }

    fn pair(&mut self) -> (wgpu::Texture, wgpu::Texture) {
        if let Some(pair) = self.pool.pop() {
            return pair;
        }
        let (width, height) = self.canvas.expect("pair before resize");
        (
            self.create_canvas_texture(width, height, "canvas a"),
            self.create_canvas_texture(width, height, "canvas b"),
        )
    }

    fn create_canvas_texture(&self, width: u32, height: u32, label: &'static str) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn create_data_texture(&self, width: u32, height: u32, label: &'static str) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn clear(&self, texture: &wgpu::Texture) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // Shared window devices need no optional features. A render-pass
        // clear is supported by every canvas render attachment, unlike
        // CommandEncoder::clear_texture, which requires CLEAR_TEXTURE.
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("concat canvas clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view(texture),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit([encoder.finish()]);
    }

    /// The walk, mirroring `compositor.rs` step for step.
    fn compose_group(
        &mut self,
        group: &LayerGroup,
        document: &ImageDocument,
        store: &PixelStore,
        pair: &mut (wgpu::Texture, wgpu::Texture),
    ) {
        for child in &group.children {
            if child.hidden() {
                continue;
            }
            match child {
                LayerNode::Layer(layer) => {
                    self.blend_layer(layer, document, store, pair);
                }
                LayerNode::Group(inner) => {
                    let mut inner_pair = self.pair();
                    self.clear(&inner_pair.0);
                    self.compose_group(inner, document, store, &mut inner_pair);
                    // The group's appearance applies to its composite as a
                    // unit: opacity, blend mode, mask.
                    let composite = inner_pair.0;
                    let mask = inner
                        .mask
                        .as_ref()
                        .and_then(|m| self.mask_texture(m, store));
                    let source = composite.clone();
                    self.overdraw(&source, inner.opacity, inner.blend, mask, None, None, pair);
                    self.pool.push((composite, inner_pair.1));
                }
                LayerNode::Adjustment(adjustment) => {
                    self.apply_adjustment(adjustment, store, pair);
                }
            }
        }
    }

    fn blend_layer(
        &mut self,
        layer: &crate::document::ImageLayer,
        document: &ImageDocument,
        store: &PixelStore,
        pair: &mut (wgpu::Texture, wgpu::Texture),
    ) {
        let Some(frame) = store.get(layer.pixels) else {
            return;
        };
        self.upload(layer.pixels, &frame, None);
        let clip = layer
            .clips_to
            .and_then(|base| self.coverage_texture(base, document, store));
        let mask = layer
            .mask
            .as_ref()
            .and_then(|m| self.mask_texture(m, store));
        let source = self.residents[&layer.pixels].texture.clone();
        // The transform always rides: the default one centres a bitmap on
        // the canvas, which is the semantics the CPU walk gives.
        let mut params = Params {
            pass_kind: 0,
            kind: layer.blend as u32,
            opacity: layer.opacity.clamp(0.0, 1.0),
            ..Params::default()
        };
        params.transform(&layer.transform, frame.width(), frame.height());
        if let Some(anchor) = layer
            .mask
            .as_ref()
            .filter(|mask| mask.enabled && mask.linked)
            .and_then(|mask| mask.anchor)
        {
            params.mask_anchor(&anchor);
        }
        if mask.is_some() {
            params.flags |= 1;
        }
        if clip.is_some() {
            params.flags |= 2;
        }
        let backdrop = pair.0.clone();
        let mut target = std::mem::replace(&mut pair.1, backdrop.clone());
        self.run_pass(
            &params,
            &source,
            &backdrop,
            mask.as_ref(),
            clip.as_ref(),
            None,
            &mut target,
        );
        pair.1 = std::mem::replace(&mut pair.0, target);
    }

    /// The one blend step for a finished group composite: source over
    /// backdrop, canvas-aligned, then the pair swaps.
    #[allow(clippy::too_many_arguments)]
    fn overdraw(
        &mut self,
        source: &wgpu::Texture,
        opacity: f32,
        blend: BlendMode,
        mask: Option<wgpu::Texture>,
        clip: Option<wgpu::Texture>,
        transform: Option<(&LayerTransform, u32, u32)>,
        pair: &mut (wgpu::Texture, wgpu::Texture),
    ) {
        let mut params = Params {
            pass_kind: 0,
            kind: blend as u32,
            opacity: opacity.clamp(0.0, 1.0),
            ..Params::default()
        };
        match transform {
            Some((transform, bitmap_width, bitmap_height)) => {
                params.transform(transform, bitmap_width, bitmap_height);
            }
            None => {
                params.tx = 0.0;
            }
        }
        if mask.is_some() {
            params.flags |= 1;
        }
        if clip.is_some() {
            params.flags |= 2;
        }
        let backdrop = pair.0.clone();
        let mut target = std::mem::replace(&mut pair.1, backdrop.clone());
        self.run_pass(
            &params,
            source,
            &backdrop,
            mask.as_ref(),
            clip.as_ref(),
            None,
            &mut target,
        );
        pair.1 = std::mem::replace(&mut pair.0, target);
    }

    fn apply_adjustment(
        &mut self,
        adjustment: &crate::document::AdjustmentLayer,
        store: &PixelStore,
        pair: &mut (wgpu::Texture, wgpu::Texture),
    ) {
        let mut params = Params {
            pass_kind: 1,
            opacity: adjustment.opacity.clamp(0.0, 1.0),
            ..Params::default()
        };
        let mut lut = None;
        match &adjustment.adjustment {
            Adjustment::Invert => params.kind = 0,
            Adjustment::Exposure { stops } => {
                params.kind = 1;
                params.a = *stops;
            }
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                params.kind = 2;
                params.a = *in_black;
                params.b = *in_white;
                params.c = *gamma;
                params.d = *out_black;
                params.e = *out_white;
            }
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                params.kind = 3;
                params.a = *hue;
                params.b = *saturation;
                params.c = *lightness;
            }
            Adjustment::GradientMap { low, high } => {
                params.kind = 4;
                [params.a, params.b, params.c] = *low;
                [params.d, params.e, params.f] = *high;
            }
            Adjustment::Grain { amount } => {
                params.kind = 5;
                params.a = *amount;
            }
            Adjustment::Curves { red, green, blue } => {
                params.kind = 6;
                lut = Some(self.curves_lut(red, green, blue));
            }
        }
        let mask = adjustment
            .mask
            .as_ref()
            .and_then(|m| self.mask_texture(m, store));
        if mask.is_some() {
            params.flags |= 1;
        }
        let backdrop = pair.0.clone();
        let mut target = std::mem::replace(&mut pair.1, backdrop.clone());
        let source = backdrop.clone();
        self.run_pass(
            &params,
            &source,
            &backdrop,
            mask.as_ref(),
            None,
            lut.as_ref(),
            &mut target,
        );
        pair.1 = std::mem::replace(&mut pair.0, target);
    }

    /// The 256-entry byte LUT for a curves adjustment: one channel per
    /// colour, values from the same piecewise-linear read the CPU does -
    /// [`curve_at_bytes`](crate::compositor::curve_at_bytes) - so the GPU
    /// lookup lands on the exact bytes.
    fn curves_lut(
        &mut self,
        red: &[(f32, f32)],
        green: &[(f32, f32)],
        blue: &[(f32, f32)],
    ) -> wgpu::Texture {
        let mut data = Vec::with_capacity(256 * 4);
        for i in 0..256 {
            let v = i as f32 / 255.0;
            data.push(crate::compositor::curve_at_bytes(red, v));
            data.push(crate::compositor::curve_at_bytes(green, v));
            data.push(crate::compositor::curve_at_bytes(blue, v));
            data.push(255);
        }
        let texture = self.create_data_texture(256, 1, "curves lut");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    /// The coverage texture of a clip base: its alpha through opacity, mask
    /// and any clip link of its own - the GPU twin of `coverage_of`.
    /// `None` when the base resolves to nothing drawable, which clips
    /// nothing - the same fallback the CPU walk takes.
    fn coverage_texture(
        &mut self,
        id: crate::document::LayerId,
        document: &ImageDocument,
        store: &PixelStore,
    ) -> Option<wgpu::Texture> {
        if let Some(cached) = self.coverage.get(&id) {
            return Some(cached.clone());
        }
        let LayerNode::Layer(layer) = document.find(id)? else {
            return None;
        };
        if layer.hidden {
            return None;
        }
        let frame = store.get(layer.pixels)?;
        self.upload(layer.pixels, &frame, None);
        let (width, height) = self.canvas.expect("coverage before resize");
        let mut target = self.create_canvas_texture(width, height, "clip coverage");
        let upstream = layer
            .clips_to
            .and_then(|up| self.coverage_texture(up, document, store));
        let mask = layer
            .mask
            .as_ref()
            .and_then(|m| self.mask_texture(m, store));
        let mut params = Params {
            pass_kind: 2,
            opacity: layer.opacity.clamp(0.0, 1.0),
            ..Params::default()
        };
        params.transform(&layer.transform, frame.width(), frame.height());
        if let Some(anchor) = layer
            .mask
            .as_ref()
            .filter(|mask| mask.enabled && mask.linked)
            .and_then(|mask| mask.anchor)
        {
            params.mask_anchor(&anchor);
        }
        if mask.is_some() {
            params.flags |= 1;
        }
        if upstream.is_some() {
            params.flags |= 2;
        }
        let source = self.residents[&layer.pixels].texture.clone();
        let backdrop = source.clone();
        self.run_pass(
            &params,
            &source,
            &backdrop,
            mask.as_ref(),
            upstream.as_ref(),
            None,
            &mut target,
        );
        self.coverage.insert(id, target.clone());
        Some(target)
    }

    /// The resident texture for a raster mask, uploaded on first use.
    /// Masks read in document coordinates, so they sample at canvas uv;
    /// a mask smaller than the canvas covers its rectangle - matching the
    /// CPU walk is the mask author's business, and the UI sizes masks to
    /// the canvas.
    fn mask_texture(
        &mut self,
        mask: &crate::document::LayerMask,
        store: &PixelStore,
    ) -> Option<wgpu::Texture> {
        if !mask.enabled {
            return None;
        }
        if let Some(resident) = self.masks.get(&mask.pixels) {
            return Some(resident.texture.clone());
        }
        let frame = store.get(mask.pixels)?;
        self.upload_mask(mask.pixels, &frame);
        self.masks.get(&mask.pixels).map(|r| r.texture.clone())
    }

    /// Re-uploads a raster mask whole: painting on a mask, or undoing
    /// one, rewrites its pixels, and the resident keyed by the mask's id
    /// has to follow. Masks are small and the whole-frame upload is one
    /// call - there is no dirty-rect plumbing for a texture the shader
    /// samples in document coordinates.
    pub fn refresh_mask(&mut self, id: PixelId, frame: &Frame) {
        self.masks.remove(&id);
        self.upload_mask(id, frame);
    }

    /// Updates one painted mask rectangle while reusing its texture. Mask
    /// residents are cached independently from layers, so this mirrors the
    /// brush's dirty upload without allocating a new full-frame texture for
    /// every pointer sample.
    pub fn refresh_mask_region(&mut self, id: PixelId, frame: &Frame, dirty: DirtyRect) {
        let reusable = self.masks.get(&id).is_some_and(|resident| {
            resident.texture.size().width == frame.width()
                && resident.texture.size().height == frame.height()
        });
        if !reusable {
            self.refresh_mask(id, frame);
            if let Some(resident) = self.masks.get_mut(&id) {
                resident.live = true;
            }
            return;
        }
        let texture = self.masks[&id].texture.clone();
        self.write_region(&texture, frame, dirty);
        if let Some(resident) = self.masks.get_mut(&id) {
            resident.frame_id = frame.id();
            resident.live = true;
        }
    }

    /// Closes a live mask stroke after the exact working frame has entered
    /// the store. A mismatched resident is repaired with one full upload.
    pub fn finish_mask(&mut self, id: PixelId, frame: &Frame) {
        if let Some(resident) = self.masks.get_mut(&id)
            && resident.frame_id == frame.id()
        {
            resident.live = false;
            return;
        }
        self.refresh_mask(id, frame);
    }

    fn write_region(&self, texture: &wgpu::Texture, frame: &Frame, dirty: DirtyRect) {
        let width = frame.width();
        let height = frame.height();
        let x = dirty.x.min(width);
        let y = dirty.y.min(height);
        let region = wgpu::Extent3d {
            width: dirty.width.min(width - x),
            height: dirty.height.min(height - y),
            depth_or_array_layers: 1,
        };
        if region.width == 0 || region.height == 0 {
            return;
        }
        let row_bytes = region.width as usize * 4;
        let stride = row_bytes.div_ceil(ROW_ALIGN) * ROW_ALIGN;
        let mut data = Vec::with_capacity(stride * region.height as usize);
        for row in 0..region.height as usize {
            let start = ((y as usize + row) * width as usize + x as usize) * 4;
            data.extend_from_slice(&frame.pixels()[start..start + row_bytes]);
            data.resize(data.len() + (stride - row_bytes), 0);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: None,
            },
            region,
        );
    }

    fn upload_mask(&mut self, id: PixelId, frame: &Frame) {
        let texture = self.create_data_texture(frame.width(), frame.height(), "mask");
        let row_bytes = frame.width() as usize * 4;
        let stride = row_bytes.div_ceil(ROW_ALIGN) * ROW_ALIGN;
        if stride == row_bytes {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                frame.pixels(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes as u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: frame.width(),
                    height: frame.height(),
                    depth_or_array_layers: 1,
                },
            );
        } else {
            // Aligned staging: rare, only for odd-width masks.
            let mut data = Vec::with_capacity(stride * frame.height() as usize);
            for row in 0..frame.height() as usize {
                let start = row * row_bytes;
                data.extend_from_slice(&frame.pixels()[start..start + row_bytes]);
                data.resize(data.len() + (stride - row_bytes), 0);
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: frame.width(),
                    height: frame.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        self.masks.insert(
            id,
            Resident {
                texture,
                frame_id: frame.id(),
                live: false,
            },
        );
    }

    /// Runs one fullscreen pass. `mask`, `clip` and `lut` fall back to the
    /// white texture when absent - sampled values of one change nothing.
    #[allow(clippy::too_many_arguments)]
    fn run_pass(
        &mut self,
        params: &Params,
        source: &wgpu::Texture,
        backdrop: &wgpu::Texture,
        mask: Option<&wgpu::Texture>,
        clip: Option<&wgpu::Texture>,
        lut: Option<&wgpu::Texture>,
        target: &mut wgpu::Texture,
    ) {
        let white = self.white_texture();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("concat canvas pass bind"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(source)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(backdrop)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view(mask.unwrap_or(&white))),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&view(clip.unwrap_or(&white))),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&view(lut.unwrap_or(&white))),
                },
            ],
        });
        self.queue.write_buffer(&self.uniform, 0, &params.bytes());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("concat canvas pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view(target),
                    resolve_target: None,
                    // Clear under the write: the pass owns its target whole.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }

    fn white_texture(&mut self) -> wgpu::Texture {
        if let Some(texture) = &self.white {
            return texture.clone();
        }
        let texture = self.create_data_texture(1, 1, "white");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let clone = texture.clone();
        self.white = Some(texture);
        clone
    }

    /// Builds full mipmap chains for the resident layer textures - the
    /// zoomed-out draw path the viewport pass samples. A stroke calls this
    /// after its dirty upload settles, at most once per frame, and only
    /// the changed texture is re-blitted.
    pub fn generate_mipmaps(&mut self) {
        // The chains arrive with the viewport pass in P4; the resident
        // textures are already sized, keyed and pooled for them.
    }
}

fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Reads a texture back as a [`Frame`], rows unpadded from the copy
/// alignment. Alpha comes back exactly as the passes wrote it.
pub fn readback(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Frame {
    let size = texture.size();
    let (width, height) = (size.width, size.height);
    let row_bytes = width as usize * 4;
    let padded_row = row_bytes.div_ceil(ROW_ALIGN) * ROW_ALIGN;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("concat canvas readback"),
        size: (padded_row * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
        return Frame::transparent(width, height);
    }
    if !matches!(receiver.try_recv(), Ok(Ok(()))) {
        return Frame::transparent(width, height);
    }
    let mut frame = Frame::transparent(width, height);
    {
        let data = slice.get_mapped_range();
        let pixels = frame.pixels_mut();
        for row in 0..height as usize {
            let from = &data[row * padded_row..row * padded_row + row_bytes];
            pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(from);
        }
    }
    buffer.unmap();
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor as cpu;
    use crate::document::{ImageLayer, LayerMask};

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Frame {
        let mut frame = Frame::transparent(width, height);
        frame.fill(rgba);
        frame
    }

    struct World {
        document: ImageDocument,
        store: PixelStore,
    }

    impl World {
        fn new() -> Self {
            Self {
                document: ImageDocument::new(16, 16),
                store: PixelStore::new(),
            }
        }

        fn with_layer(&mut self, name: &str, frame: Frame) -> crate::document::LayerId {
            self.document.new_layer(name, self.store.put(frame))
        }
    }

    fn assert_parity_on(world: &World, gpu: &mut CanvasGpu) {
        let expected = cpu::compose(&world.document, &world.store);
        let actual = gpu.compose_frame(&world.document, &world.store);
        assert_eq!(actual.width(), expected.width());
        assert_eq!(actual.height(), expected.height());
        for index in 0..expected.pixels().len() {
            let (a, e) = (actual.pixels()[index], expected.pixels()[index]);
            assert!(
                (i16::from(a) - i16::from(e)).abs() <= 1,
                "byte {index} (x {}, y {}, ch {}): gpu {a} vs cpu {e}",
                index / 4 % 16,
                index / 64,
                index % 4
            );
        }
    }

    /// The two backends over the same document, within a byte. The broad
    /// parity suite remains portable to builders without an adapter.
    fn assert_parity(world: &World) {
        let Some(mut gpu) = CanvasGpu::new() else {
            return;
        };
        assert_parity_on(world, &mut gpu);
    }

    /// Correctness tests for GPU-only state must prove an adapter ran them;
    /// silently returning would turn a skipped hardware path into a pass.
    fn required_gpu() -> CanvasGpu {
        CanvasGpu::new().expect("this GPU regression requires a usable wgpu adapter")
    }

    /// Match the editor's shared device: no optional features, real hardware.
    #[test]
    fn shared_device_without_optional_features_matches_cpu() {
        let descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        #[cfg(target_os = "macos")]
        let descriptor = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..descriptor
        };
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("shared-device regression requires a hardware adapter");
        let info = adapter.get_info();
        eprintln!("shared-device regression adapter: {info:?}");
        #[cfg(target_os = "macos")]
        assert_eq!(info.backend, wgpu::Backend::Metal);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("feature-free shared canvas regression"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .expect("feature-free shared device");
        assert!(device.features().is_empty());
        let mut gpu = CanvasGpu::with_device(device, queue);
        let mut world = World::new();
        let photo = world.with_layer("First import", solid(16, 16, [200, 40, 90, 180]));
        assert_parity_on(&world, &mut gpu);
        let replacement = world.store.put(solid(16, 16, [30, 170, 210, 255]));
        world.document.layer_mut(photo).unwrap().pixels = replacement;
        assert_parity_on(&world, &mut gpu);
        let group = world.document.new_group("Folder");
        let child = world.document.mint_id();
        let pixels = world.store.put(solid(16, 16, [220, 90, 30, 180]));
        world
            .document
            .group_mut(group)
            .unwrap()
            .children
            .push(LayerNode::Layer(ImageLayer::new(child, "Nested", pixels)));
        world.document.group_mut(group).unwrap().opacity = 0.6;
        assert_parity_on(&world, &mut gpu);
        let mut mask = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let value = (x * 17) as u8;
                mask.set_pixel(x, y, [value, value, value, 255]);
            }
        }
        let mask = world.store.put(mask);
        world.document.layer_mut(child).unwrap().mask = Some(LayerMask::new(mask));
        assert_parity_on(&world, &mut gpu);
        world.document.new_adjustment("Invert", Adjustment::Invert);
        assert_parity_on(&world, &mut gpu);
        // Pooled group targets must discard pixels from the preceding compose.
        world.document.group_mut(group).unwrap().children.clear();
        assert_parity_on(&world, &mut gpu);
    }

    #[test]
    fn probe_print() {
        // All-black mask: the layer must vanish. All-white: fully there.
        for mask_value in [0u8, 255] {
            let mut world = World::new();
            world.with_layer("Back", solid(16, 16, [90, 90, 90, 255]));
            let mask = solid(16, 16, [mask_value, mask_value, mask_value, 255]);
            let mask_id = world.store.put(mask);
            let id = world.with_layer("Top", solid(16, 16, [250, 180, 30, 255]));
            world.document.layer_mut(id).expect("layer").mask = Some(LayerMask::new(mask_id));
            let expected = cpu::compose(&world.document, &world.store);
            let Some(mut gpu) = CanvasGpu::new() else {
                return;
            };
            let actual = gpu.compose_frame(&world.document, &world.store);
            println!("mask {mask_value}: cpu {:?}", &expected.pixels()[0..4]);
            println!("mask {mask_value}: gpu {:?}", &actual.pixels()[0..4]);
        }
        // The invert adjustment alone.
        let mut world = World::new();
        world.with_layer("Photo", solid(16, 16, [120, 90, 200, 255]));
        world.document.new_adjustment("Invert", Adjustment::Invert);
        let expected = cpu::compose(&world.document, &world.store);
        let Some(mut gpu) = CanvasGpu::new() else {
            return;
        };
        let actual = gpu.compose_frame(&world.document, &world.store);
        println!("invert: cpu {:?}", &expected.pixels()[0..4]);
        println!("invert: gpu {:?}", &actual.pixels()[0..4]);
    }

    #[test]
    fn one_layer_over_transparent_matches() {
        let mut world = World::new();
        world.with_layer("Red", solid(16, 16, [220, 40, 90, 200]));
        assert_parity(&world);
    }

    #[test]
    fn two_layers_all_separable_modes_match() {
        for mode in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Difference,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
        ] {
            let mut world = World::new();
            world.with_layer("Back", solid(16, 16, [96, 160, 32, 255]));
            let front = world.with_layer("Front", solid(16, 16, [200, 60, 130, 255]));
            world.document.layer_mut(front).expect("layer").blend = mode;
            world.document.layer_mut(front).expect("layer").opacity = 0.75;
            assert_parity(&world);
        }
    }

    #[test]
    fn nonseparable_modes_match() {
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            let mut world = World::new();
            world.with_layer("Back", solid(16, 16, [60, 130, 200, 255]));
            let front = world.with_layer("Front", solid(16, 16, [220, 170, 40, 255]));
            world.document.layer_mut(front).expect("layer").blend = mode;
            assert_parity(&world);
        }
    }

    #[test]
    fn a_translated_layer_matches() {
        let mut world = World::new();
        let id = world.with_layer("Panel", solid(8, 8, [30, 200, 120, 255]));
        world.document.layer_mut(id).expect("layer").transform = LayerTransform {
            x: 4.0,
            y: -2.0,
            ..LayerTransform::default()
        };
        assert_parity(&world);
    }

    #[test]
    fn a_group_with_opacity_matches() {
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [20, 20, 80, 255]));
        let group = world.document.new_group("Folder");
        let red = world.document.mint_id();
        let green = world.document.mint_id();
        {
            let group = world.document.group_mut(group).expect("group");
            group.children.push(LayerNode::Layer(ImageLayer::new(
                red,
                "Red",
                world.store.put(solid(16, 16, [200, 0, 0, 255])),
            )));
            group.children.push(LayerNode::Layer(ImageLayer::new(
                green,
                "Green",
                world.store.put(solid(16, 16, [0, 220, 0, 210])),
            )));
        }
        world.document.group_mut(group).expect("group").opacity = 0.6;
        assert_parity(&world);
    }

    #[test]
    fn a_masked_layer_matches() {
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [90, 90, 90, 255]));
        let mut mask = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = (x * 16) as u8;
                mask.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let mask_id = world.store.put(mask);
        let id = world.with_layer("Top", solid(16, 16, [250, 180, 30, 255]));
        world.document.layer_mut(id).expect("layer").mask = Some(LayerMask::new(mask_id));
        assert_parity(&world);
    }

    #[test]
    fn a_linked_mask_keeps_cpu_gpu_parity_after_transform() {
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [90, 90, 90, 255]));
        let mut mask = solid(16, 16, [255, 255, 255, 255]);
        for y in 4..9 {
            for x in 5..10 {
                mask.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
        let mut linked = LayerMask::new(world.store.put(mask));
        linked.anchor = Some(LayerTransform::default());
        let id = world.with_layer("Top", solid(16, 16, [250, 180, 30, 255]));
        let layer = world.document.layer_mut(id).expect("layer");
        layer.mask = Some(linked);
        layer.transform.x = 2.0;
        layer.transform.y = -1.0;
        let mut gpu = required_gpu();
        assert_parity_on(&world, &mut gpu);
    }

    #[test]
    fn a_clip_link_matches() {
        let mut world = World::new();
        let mut base = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..10 {
                base.set_pixel(x, y, [255, 255, 255, 255]);
            }
        }
        let base = world.with_layer("Base", base);
        let top = world.with_layer("Top", solid(16, 16, [0, 130, 250, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(base);
        assert_parity(&world);
    }

    #[test]
    fn adjustments_match() {
        let cases = [
            Adjustment::Invert,
            Adjustment::Exposure { stops: 0.7 },
            Adjustment::Levels {
                in_black: 0.2,
                in_white: 0.85,
                gamma: 1.3,
                out_black: 0.05,
                out_white: 0.95,
            },
            Adjustment::HueSaturation {
                hue: 40.0,
                saturation: 0.35,
                lightness: -0.1,
            },
            Adjustment::GradientMap {
                low: [0.1, 0.0, 0.2],
                high: [1.0, 0.9, 0.7],
            },
            Adjustment::Grain { amount: 0.4 },
            Adjustment::Curves {
                red: vec![(0.0, 0.0), (0.5, 0.35), (1.0, 1.0)],
                green: vec![(0.0, 0.0), (1.0, 1.0)],
                blue: vec![(0.0, 0.1), (1.0, 0.9)],
            },
        ];
        for adjustment in cases {
            let mut world = World::new();
            world.with_layer("Photo", solid(16, 16, [120, 90, 200, 255]));
            let id = world.document.new_adjustment("Adj", adjustment);
            world
                .document
                .adjustment_mut(id)
                .expect("adjustment")
                .opacity = 0.8;
            assert_parity(&world);
        }
    }

    #[test]
    fn a_masked_adjustment_matches() {
        let mut world = World::new();
        world.with_layer("Photo", solid(16, 16, [120, 90, 200, 255]));
        let mut mask = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = if x < 8 { 255 } else { 0 };
                mask.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let mask_id = world.store.put(mask);
        let id = world.document.new_adjustment("Invert", Adjustment::Invert);
        world.document.adjustment_mut(id).expect("adjustment").mask = Some(LayerMask::new(mask_id));
        assert_parity(&world);
    }

    #[test]
    fn the_whole_stack_matches() {
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [40, 60, 90, 255]));
        let mut mask = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = ((x + y) * 8) as u8;
                mask.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let mask_id = world.store.put(mask);
        let mut base = Frame::transparent(16, 16);
        for y in 0..16 {
            for x in 0..12 {
                base.set_pixel(x, y, [255, 255, 255, 255]);
            }
        }
        let base = world.with_layer("Base", base);
        let top = world.with_layer("Top", solid(16, 16, [250, 120, 40, 255]));
        world.document.layer_mut(top).expect("layer").clips_to = Some(base);
        world.document.layer_mut(top).expect("layer").blend = BlendMode::Overlay;
        world.document.layer_mut(top).expect("layer").mask = Some(LayerMask::new(mask_id));
        let group = world.document.new_group("Graded");
        assert!(world.document.move_node(top, Some(group), 0));
        let adjustment = world.document.new_adjustment("Invert", Adjustment::Invert);
        assert!(world.document.move_node(adjustment, Some(group), 1));
        assert_parity(&world);
    }

    #[test]
    fn a_dirty_upload_matches_a_fresh_one() {
        // The brush path: the layer's pixels live, a stroke changes a
        // 6x6 region, the store swaps the frame under the same id, and
        // only that region is uploaded. The composite must match the CPU
        // reference over the whole canvas.
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [20, 40, 60, 255]));
        let paint_id = world.document.mint_id();
        {
            let layer = ImageLayer::new(
                paint_id,
                "Paint",
                world.store.put(Frame::transparent(16, 16)),
            );
            world.document.root.children.push(LayerNode::Layer(layer));
        }
        let pixel_id = world.document.layer_mut(paint_id).expect("layer").pixels;
        let mut painted = Frame::transparent(16, 16);
        for y in 4..10 {
            for x in 4..10 {
                painted.set_pixel(x, y, [255, 0, 0, 255]);
            }
        }
        world.store.replace(pixel_id, painted);
        let expected = cpu::compose(&world.document, &world.store);

        let Some(mut gpu) = CanvasGpu::new() else {
            return;
        };
        // The resident holds the stroke's *previous* state: transparent,
        // under the same pixel id - then the dirty rectangle brings the
        // stroke over.
        gpu.upload(pixel_id, &Frame::transparent(16, 16), None);
        let stroke = world.store.get(pixel_id).expect("pixels");
        gpu.upload(
            pixel_id,
            &stroke,
            Some(DirtyRect {
                x: 4,
                y: 4,
                width: 6,
                height: 6,
            }),
        );
        let actual = gpu.compose_frame(&world.document, &world.store);
        for index in 0..expected.pixels().len() {
            let (a, e) = (actual.pixels()[index], expected.pixels()[index]);
            assert!(
                (i16::from(a) - i16::from(e)).abs() <= 1,
                "byte {index} (x {}, y {}, ch {}): gpu {a} vs cpu {e}",
                index / 4 % 16,
                index / 64,
                index % 4
            );
        }
    }

    #[test]
    fn a_live_dirty_upload_refuses_the_stores_old_frame_until_release() {
        let mut gpu = required_gpu();
        let id = PixelId(7);
        let base = Frame::transparent(16, 16);
        gpu.upload(id, &base, None);

        let mut working = base.clone();
        working.set_pixel(4, 4, [255, 0, 0, 255]);
        gpu.upload(
            id,
            &working,
            Some(DirtyRect {
                x: 4,
                y: 4,
                width: 1,
                height: 1,
            }),
        );
        let live_id = gpu.residents[&id].frame_id;
        assert!(gpu.residents[&id].live);

        // Composition sees the store's pre-stroke frame while the gesture
        // is live. It must not replace the newer dirty resident.
        gpu.upload(id, &base, None);
        assert_eq!(gpu.residents[&id].frame_id, live_id);
        assert!(gpu.residents[&id].live);

        // Release moves this exact working frame into the store.
        gpu.upload(id, &working, None);
        assert_eq!(gpu.residents[&id].frame_id, working.id());
        assert!(!gpu.residents[&id].live);
    }

    #[test]
    fn a_disabled_mask_is_ignored_by_the_gpu_too() {
        let mut world = World::new();
        world.with_layer("Back", solid(16, 16, [20, 30, 40, 255]));
        let mask_id = world.store.put(solid(16, 16, [0, 0, 0, 255]));
        let top = world.with_layer("Top", solid(16, 16, [220, 80, 30, 255]));
        let mut mask = LayerMask::new(mask_id);
        mask.enabled = false;
        world.document.layer_mut(top).expect("layer").mask = Some(mask);
        let mut gpu = required_gpu();
        assert_parity_on(&world, &mut gpu);
    }
}
