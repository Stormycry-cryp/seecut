// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// the blend modes its layers offer. The math here follows the PDF 32000
// compositing model exactly - including the source-alpha handling that Core
// Graphics gets wrong for Color Dodge and Color Burn and that Compositor had
// to route around through Core Image (its SeparableBlend); one
// implementation does it right for every mode, and this is the CPU reference
// the GPU path will be checked against.

//! Blend modes, as the reference CPU math.
//!
//! Everything operates on straight (non-premultiplied) 8-bit RGBA, the one
//! pixel format `concat-core` uses, and composites **source over backdrop**
//! with the PDF spec's alpha-correct formula:
//!
//! ```text
//! αo = αs + αb·(1 − αs)
//! Co = (αs·(1 − αb)·Cs + αb·(1 − αs)·Cb + αs·αb·B(Cb, Cs)) / αo
//! ```
//!
//! where `B` is the mode's colour function. The middle term is what a naive
//! implementation drops: a half-transparent source contributes half of its
//! *colour* and half of the backdrop's, so a soft-edged Color Dodge stays
//! soft instead of snapping to a hard edge. Every mode here gets that right
//! by construction, because they all go through the same shell.
//!
//! The non-separable modes (Hue, Saturation, Color, Luminosity) follow the
//! spec's `Lum`/`Sat`/`SetLum`/`SetSat` building blocks. Their formulas are
//! derived from each mode's definition rather than copied from the published
//! tables, because the spec's Color formula is a known erratum - it is
//! printed identical to Hue's. The derivations are spelled out on
//! [`blend_pixel`]'s non-separable arm.

use serde::{Deserialize, Serialize};

/// How a layer's colour meets what is beneath it.
///
/// The video timeline keeps its own smaller set (`concat_core::timeline::Blend`);
/// this one is the canvas's, matching the image editor's full set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum BlendMode {
    /// Source over: the layer covers what is beneath by its alpha.
    #[default]
    Normal,
    /// The ground times the layer.
    Multiply,
    /// The inverses multiplied, inverted.
    Screen,
    /// Multiply or screen by how dark the ground is.
    Overlay,
    /// The darker of the two, per channel.
    Darken,
    /// The brighter of the two, per channel.
    Lighten,
    /// The ground subtracted from the layer, absolute.
    Difference,
    /// The ground divided by the inverse of the layer, brightening.
    ColorDodge,
    /// The inverse of the ground divided by the layer, darkening.
    ColorBurn,
    /// The layer's hue over the ground's saturation and luminosity.
    Hue,
    /// The layer's saturation over the ground's hue and luminosity.
    Saturation,
    /// The layer's hue and saturation over the ground's luminosity.
    Color,
    /// The layer's luminosity over the ground's hue and saturation.
    Luminosity,
}

impl BlendMode {
    /// Every mode, in the layers panel's menu order.
    pub const ALL: [BlendMode; 13] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// The mode's name in a saved document: "normal", "color-dodge", ...
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Multiply => "multiply",
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::Darken => "darken",
            Self::Lighten => "lighten",
            Self::Difference => "difference",
            Self::ColorDodge => "color-dodge",
            Self::ColorBurn => "color-burn",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Color => "color",
            Self::Luminosity => "luminosity",
        }
    }

    /// The mode a document names, Normal for anything unknown.
    pub fn parse(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name() == name.trim())
            .unwrap_or_default()
    }

    /// Whether the mode works on whole colours rather than one channel at a
    /// time. The shell differs: separable modes blend per channel, these
    /// four swap and stretch the three channels together.
    pub fn is_nonseparable(self) -> bool {
        matches!(
            self,
            Self::Hue | Self::Saturation | Self::Color | Self::Luminosity
        )
    }
}

/// Composites one source pixel over one backdrop pixel in `mode`, at a layer
/// `opacity` in `0.0..=1.0` that scales the source's alpha before anything
/// else. Both pixels are straight RGBA; the result is too.
pub fn blend_pixel(mode: BlendMode, backdrop: [u8; 4], source: [u8; 4], opacity: f32) -> [u8; 4] {
    let opacity = opacity.clamp(0.0, 1.0);
    let alpha_s = source[3] as f32 / 255.0 * opacity;
    if alpha_s <= 0.0 {
        return backdrop;
    }
    let alpha_b = backdrop[3] as f32 / 255.0;
    let alpha_o = alpha_s + alpha_b * (1.0 - alpha_s);
    if alpha_o <= 0.0 {
        return [0, 0, 0, 0];
    }

    let cb = colour(backdrop);
    let cs = colour(source);
    let blended = if mode.is_nonseparable() {
        // Each derivation starts from the colour that keeps two of the three
        // components, then forces the third - which is exactly what the
        // mode's definition asks for:
        //   Hue        source hue,        backdrop sat + lum
        //   Saturation source sat,        backdrop hue + lum
        //   Color      source hue + sat,  backdrop lum
        //   Luminosity source lum,        backdrop hue + sat
        match mode {
            BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
            _ => unreachable!("only the four above are non-separable"),
        }
    } else {
        let mut per_channel = [0.0f32; 3];
        for channel in 0..3 {
            per_channel[channel] = mix_channel(mode, cb[channel], cs[channel]);
        }
        per_channel
    };

    let mut out = [0u8; 4];
    for channel in 0..3 {
        let co = (alpha_s * (1.0 - alpha_b) * cs[channel]
            + alpha_b * (1.0 - alpha_s) * cb[channel]
            + alpha_s * alpha_b * blended[channel])
            / alpha_o;
        out[channel] = quantize(co);
    }
    out[3] = quantize(alpha_o);
    out
}

/// Rounds a `0.0..=1.0` colour value to its byte, clamping float dust.
fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A pixel's colour as three `0.0..=1.0` floats, ignoring alpha.
fn colour(pixel: [u8; 4]) -> [f32; 3] {
    [
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    ]
}

/// The separable colour function `B(Cb, Cs)` for one channel.
fn mix_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            unreachable!("non-separable modes take the whole-colour arm")
        }
    }
}

/// Overlay is Hard Light with the two colours swapped.
fn hard_light(a: f32, b: f32) -> f32 {
    if a <= 0.5 {
        2.0 * a * b
    } else {
        1.0 - 2.0 * (1.0 - a) * (1.0 - b)
    }
}

/// Luminosity, the spec's weights.
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// Saturation: the spread of the channels.
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Sets a colour's luminosity to `l`, keeping its hue and saturation, by
/// adding the difference and clipping what falls outside `0.0..=1.0` the
/// way the spec's `ClipColor` does.
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let delta = l - lum(c);
    clip_colour([c[0] + delta, c[1] + delta, c[2] + delta])
}

/// `ClipColor`: pulls an out-of-range colour back to `0.0..=1.0` while
/// keeping its luminosity - each channel moves toward `lum(c)` by as much as
/// it takes to fit.
fn clip_colour(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let low = c[0].min(c[1]).min(c[2]);
    let high = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if low < 0.0 {
        for channel in &mut out {
            *channel = l + (*channel - l) * l / (l - low);
        }
    }
    if high > 1.0 {
        for channel in &mut out {
            *channel = l + (*channel - l) * (1.0 - l) / (high - l);
        }
    }
    out.map(|channel| channel.clamp(0.0, 1.0))
}

/// Sets a colour's saturation to `s`, keeping its hue: the spec's channel
/// sort, rescale and unsort.
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Channel indices, sorted by value, lowest to highest.
    let mut order = [0usize, 1, 2];
    order.sort_by(|a, b| c[*a].partial_cmp(&c[*b]).expect("finite colours"));
    let (low, mid, high) = (order[0], order[1], order[2]);
    let mut out = c;
    if c[high] > c[low] {
        out[mid] = (c[mid] - c[low]) * s / (c[high] - c[low]);
        out[high] = s;
        out[low] = 0.0;
    } else {
        // A zero spread carries no hue to keep; the spec flattens it.
        out = [0.0, 0.0, 0.0];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte equality with a rounding step of grace: every golden value below
    /// is hand-computed in f32, so a different-but-valid rounding order may
    /// differ by one.
    fn assert_pixel(actual: [u8; 4], expected: [u8; 4], label: &str) {
        for channel in 0..4 {
            let diff = actual[channel] as i32 - expected[channel] as i32;
            assert!(
                diff.abs() <= 1,
                "{label} channel {channel}: {actual:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn names_round_trip() {
        for mode in BlendMode::ALL {
            assert_eq!(BlendMode::parse(mode.name()), mode);
        }
        assert_eq!(BlendMode::parse("nonsense"), BlendMode::Normal);
        assert_eq!(BlendMode::ALL.len(), 13);
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn opaque_normal_replaces_the_backdrop() {
        let out = blend_pixel(
            BlendMode::Normal,
            [10, 20, 30, 255],
            [200, 100, 50, 255],
            1.0,
        );
        assert_pixel(out, [200, 100, 50, 255], "normal");
    }

    #[test]
    fn a_fully_transparent_source_leaves_every_mode_untouched() {
        for mode in BlendMode::ALL {
            let backdrop = [10, 20, 30, 128];
            assert_eq!(
                blend_pixel(mode, backdrop, [255, 0, 0, 0], 1.0),
                backdrop,
                "mode {mode:?}"
            );
            // And a zero layer opacity has the same effect as zero alpha.
            assert_eq!(
                blend_pixel(mode, backdrop, [255, 0, 0, 255], 0.0),
                backdrop,
                "mode {mode:?}"
            );
        }
    }

    #[test]
    fn layer_opacity_scales_the_source_alpha() {
        // Half-opacity white over opaque black, normal: 128s of everything.
        let out = blend_pixel(BlendMode::Normal, [0, 0, 0, 255], [255, 255, 255, 255], 0.5);
        assert_pixel(out, [128, 128, 128, 255], "half opacity");
    }

    #[test]
    fn over_a_transparent_backdrop_the_source_arrives_whole() {
        for mode in BlendMode::ALL {
            let out = blend_pixel(mode, [0, 0, 0, 0], [200, 100, 50, 255], 1.0);
            assert_pixel(out, [200, 100, 50, 255], &format!("mode {mode:?}"));
        }
    }

    #[test]
    fn alpha_composites_over_a_transparent_backdrop() {
        // Half-alpha source over nothing: alpha halves, colour survives.
        let out = blend_pixel(BlendMode::Normal, [0, 0, 0, 0], [200, 100, 50, 128], 1.0);
        assert_pixel(out, [200, 100, 50, 128], "alpha");
    }

    #[test]
    fn separable_golden_values() {
        // Opaque backdrop and source, so B(Cb, Cs) lands in the output whole.
        /// One hand-computed case: mode, backdrop, source, expected.
        type Case = (BlendMode, [u8; 4], [u8; 4], [u8; 4]);
        let cases: [Case; 7] = [
            // 204*128/255 = 102.4
            (
                BlendMode::Multiply,
                [204, 204, 204, 255],
                [128, 128, 128, 255],
                [102, 102, 102, 255],
            ),
            // 51+102-51*102/255 = 132.6
            (
                BlendMode::Screen,
                [51, 51, 51, 255],
                [102, 102, 102, 255],
                [133, 133, 133, 255],
            ),
            // source 0.4 <= 0.5: 2*0.4*0.8 = 0.64 -> 163
            (
                BlendMode::Overlay,
                [204, 204, 204, 255],
                [102, 102, 102, 255],
                [163, 163, 163, 255],
            ),
            (
                BlendMode::Darken,
                [200, 50, 50, 255],
                [100, 150, 50, 255],
                [100, 50, 50, 255],
            ),
            (
                BlendMode::Lighten,
                [200, 50, 50, 255],
                [100, 150, 50, 255],
                [200, 150, 50, 255],
            ),
            (
                BlendMode::Difference,
                [200, 50, 50, 255],
                [100, 150, 50, 255],
                [100, 100, 0, 255],
            ),
            // dodge: cb=0.392, cs=0.2 -> min(1, 0.392/0.8) = 0.49 -> 125
            (
                BlendMode::ColorDodge,
                [100, 100, 100, 255],
                [51, 51, 51, 255],
                [125, 125, 125, 255],
            ),
        ];
        for (mode, backdrop, source, expected) in cases {
            assert_pixel(
                blend_pixel(mode, backdrop, source, 1.0),
                expected,
                &format!("mode {mode:?}"),
            );
        }
    }

    #[test]
    fn a_semi_transparent_dodge_stays_soft() {
        // The case Core Graphics gets wrong and Compositor routed through
        // Core Image: half-alpha red over grey. The red channel blends half
        // (dodge to white), the others stay half the backdrop's grey:
        // 0.5*0.392 + 0.5*1 = 0.696 -> 178; 0.5*0.392 + 0.5*0.392 -> 100.
        let out = blend_pixel(
            BlendMode::ColorDodge,
            [100, 100, 100, 255],
            [255, 0, 0, 128],
            1.0,
        );
        assert_pixel(out, [178, 100, 100, 255], "dodge");
    }

    #[test]
    fn a_semi_transparent_burn_stays_soft() {
        // Half-alpha pure red over light grey: the channels the source has
        // nothing in burn toward zero by half; the red stays.
        // cb=0.784, cs=0 -> B=0: 0.5*0.784 + 0.5*0 = 0.392 -> 100.
        // cb=0.784, cs=1 -> B=0.784: unchanged 200.
        let out = blend_pixel(
            BlendMode::ColorBurn,
            [200, 200, 200, 255],
            [255, 0, 0, 128],
            1.0,
        );
        assert_pixel(out, [200, 100, 100, 255], "burn");
    }

    #[test]
    fn luminosity_takes_the_source_lightness_and_the_backdrop_colour() {
        // backdrop (200,50,50), source mid grey: the result is the backdrop
        // re-lit to the grey's luminosity - (205,55,55), hand-computed
        // through SetLum.
        let out = blend_pixel(
            BlendMode::Luminosity,
            [200, 50, 50, 255],
            [100, 100, 100, 255],
            1.0,
        );
        assert_pixel(out, [205, 55, 55, 255], "luminosity");
    }

    #[test]
    fn hue_keeps_the_backdrop_saturation_and_lightness() {
        // A saturated red over a saturated blue: whatever hue comes out, its
        // saturation and luminosity must be the backdrop's.
        let backdrop = [30, 60, 200, 255];
        let source = [255, 40, 40, 255];
        let out = blend_pixel(BlendMode::Hue, backdrop, source, 1.0);
        let c = colour(out);
        assert!(
            (sat(c) - sat(colour(backdrop))).abs() < 0.01,
            "saturation drifted: {c:?}"
        );
        assert!(
            (lum(c) - lum(colour(backdrop))).abs() < 0.01,
            "luminosity drifted: {c:?}"
        );
    }

    #[test]
    fn saturation_keeps_the_backdrop_hue() {
        let backdrop = [30, 60, 200, 255];
        let source = [255, 0, 0, 255];
        let out = blend_pixel(BlendMode::Saturation, backdrop, source, 1.0);
        let c = colour(out);
        // Luminosity is the backdrop's exactly; the saturation is the
        // source's once clipping has given back what the luminosity could
        // not hold (ClipColor), so this is near 1.0 rather than exactly.
        assert!(
            (lum(c) - lum(colour(backdrop))).abs() < 0.01,
            "luminosity drifted: {c:?}"
        );
        assert!(sat(c) > 0.9, "saturation lost: {c:?}");
        // The backdrop's blue must dominate the hue still.
        assert!(
            c[2] > c[1] - 0.01 && c[1] > c[0],
            "hue was not the backdrop's: {c:?}"
        );
    }

    #[test]
    fn color_takes_the_source_saturation_and_the_backdrop_lightness() {
        let backdrop = [120, 120, 120, 255];
        let source = [255, 0, 0, 255];
        let out = blend_pixel(BlendMode::Color, backdrop, source, 1.0);
        let c = colour(out);
        // The backdrop's grey luminosity is what the colour is laid on, so
        // the result is re-lit to exactly that.
        assert!(
            (lum(c) - lum(colour(backdrop))).abs() < 0.01,
            "luminosity drifted: {c:?}"
        );
        // The published spec's Color formula (identical to Hue's) would have
        // flattened saturation to the grey backdrop's zero; this is the
        // erratum the derivation avoids. Clipping trims the source red's
        // full saturation to what the luminosity allows, so 1.0 is not the
        // bar - anything well above zero proves the colour survived.
        assert!(sat(c) > 0.5, "saturation lost: {c:?}");
    }

    #[test]
    fn color_over_a_grey_of_the_same_lightness_is_the_colour_itself() {
        // A grey whose luminosity is exactly the source red's (0.3) leaves
        // the source untouched, no clipping needed.
        let grey = (0.3f32 * 255.0).round() as u8;
        let out = blend_pixel(
            BlendMode::Color,
            [grey, grey, grey, 255],
            [255, 0, 0, 255],
            1.0,
        );
        assert_pixel(out, [255, 0, 0, 255], "grey at the colour's own lightness");
    }

    #[test]
    fn hue_over_grey_stays_grey() {
        // No saturation in the backdrop means nothing to carry a hue in:
        // every hue lands as the backdrop's own grey.
        let out = blend_pixel(BlendMode::Hue, [100, 100, 100, 255], [255, 0, 0, 255], 1.0);
        assert_pixel(out, [100, 100, 100, 255], "hue over grey");
    }

    #[test]
    fn clip_colour_pulls_saturation_in_from_the_ends() {
        // set_lum of a saturated colour to near-white clips, and the result
        // stays in range with its luminosity exact.
        let clipped = set_lum([1.0, 0.0, 0.0], 0.95);
        assert!((lum(clipped) - 0.95).abs() < 0.001);
        assert!(clipped.iter().all(|c| (0.0..=1.0).contains(c)));
    }

    #[test]
    fn the_result_alpha_covers_both_inputs() {
        // αo = αs + αb·(1 − αs) is never below either input, whatever the
        // blend mode does to the colours. Every mode and every opacity step
        // must keep that, and no arm may panic or wrap.
        for mode in BlendMode::ALL {
            for step in 0..=10 {
                let opacity = step as f32 / 10.0;
                let backdrop = [200, 30, 90, 200];
                let source = [10, 240, 130, 170];
                let out = blend_pixel(mode, backdrop, source, opacity);
                let alpha_s = source[3] as f32 * opacity;
                let floor = alpha_s.max(backdrop[3] as f32);
                assert!(
                    out[3] as f32 >= floor - 1.0,
                    "mode {mode:?} opacity {opacity}: alpha {} under {floor}",
                    out[3]
                );
            }
        }
    }
}
