//! The painting engine, ported from Compositor's `BrushStroke` and its
//! `continuousBrush` Metal kernel.
//!
//! The tip is not a stamp repeater. Soft tips deposit *optical density* along
//! the smoothed pointer path - the density integrates over the distance each
//! curve segment sweeps past a pixel (8-point Gauss-Legendre), and coverage is
//! `1 - exp(-density)`, the continuous limit of source-over dabs at the
//! deposition spacing. Self-crossings and corners blend smoothly and the
//! result does not depend on how densely the pointer events arrived.
//!
//! Permanent paint and the provisional tail live separately: the tail (the
//! straight run to the cursor before the next sample turns it into a curve
//! piece) is re-derived on every update and never accumulated into permanent
//! density, so replacing it cannot double-count or leave ghosts. The opacity
//! setting caps the whole stroke, as in Photoshop.

/// Tile edge in layer pixels, as upstream: 256.
pub const TILE_SIZE: usize = 256;

/// Optical density saturates here; `1 - exp(-20)` rounds to full coverage.
const DENSITY_CAP: f32 = 20.0;

/// A stroke in flight over one layer. Paint coordinates are layer pixels.
pub struct BrushStroke {
    width: u32,
    height: u32,
    settings: BrushSettings,
    /// Document pixels between dab centres: diameter x 2.5% soft / 1.5% hard.
    spacing: f32,
    /// The antialias width of a hard tip's rim, in pixels.
    antialias: f32,
    /// The last pointer samples; the curve needs one sample past each piece.
    samples: Vec<(f64, f64)>,
    /// Settled curve pieces not yet integrated (this update's new geometry).
    settled: Vec<Segment>,
    /// The provisional straight run to the cursor.
    tail: Vec<Segment>,
    /// Tiles the tail touched on the last update, to clear its preview.
    tail_keys: Vec<usize>,
    tiles: std::collections::HashMap<usize, Tile>,
}

/// A curve piece in layer pixels: from (x1, y1) to (x2, y2).
type Segment = [f32; 4];

/// One brush: geometry, tip feel, and what it lays down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    /// Tip diameter in layer pixels, 1..=2000.
    pub diameter: f64,
    /// 0 fully soft, 1 fully hard.
    pub hardness: f64,
    /// Caps the whole accumulated stroke; overlapping dabs never exceed it.
    pub opacity: f64,
    /// Straight-RGB paint colour.
    pub color: [u8; 3],
    /// The stroke clears the layer's alpha instead of painting colour.
    pub erasing: bool,
}

/// Why a stroke refused to start.
#[derive(Debug)]
pub enum BrushError {
    /// A setting is outside its valid range.
    BadSettings,
    /// The layer or the touched area is too large to paint on.
    TooLarge,
}

struct Tile {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    /// Soft strokes: accumulated optical density. Hard strokes: the maximum
    /// coverage so far. The tail's contribution is never stored here.
    permanent: Vec<f32>,
}

impl BrushSettings {
    fn validate(&self) -> Result<(), BrushError> {
        if !self.diameter.is_finite() || !(1.0..=2000.0).contains(&self.diameter) {
            return Err(BrushError::BadSettings);
        }
        if !self.hardness.is_finite() || !(0.0..=1.0).contains(&self.hardness) {
            return Err(BrushError::BadSettings);
        }
        if !self.opacity.is_finite() || !(0.01..=1.0).contains(&self.opacity) {
            return Err(BrushError::BadSettings);
        }
        Ok(())
    }
}

/// Dab spacing as a fraction of the diameter: 1.5% hard, 2.5% soft.
pub fn spacing_fraction(hardness: f64) -> f64 {
    if hardness >= 1.0 { 0.015 } else { 0.025 }
}

/// Soft-tip falloff: a normalized Gaussian that fades across the whole radius
/// and reaches zero at the rim. `u` is distance / radius.
fn falloff(u: f32) -> f32 {
    const K: f32 = 2.5;
    let floor = (-K).exp();
    ((-K * u * u).exp() - floor) / (1.0 - floor)
}

/// Tip coverage at a squared distance from the tip centre.
fn coverage_at(distance_squared: f32, radius: f32, hardness: f32, antialias: f32) -> f32 {
    let distance = distance_squared.sqrt();
    if hardness >= 1.0 {
        // Hard tips keep a solid interior with a pixel-edge antialiased rim.
        return (radius - distance) / antialias + 0.5;
    }
    let t = ((distance / radius - hardness) / (1.0 - hardness)).clamp(0.0, 1.0);
    falloff(t).max(0.0)
}

/// Optical density of the tip at a squared distance: the -log of the
/// transparency a single dab would leave, so densities add like source-over.
fn tip_density(distance_squared: f32, radius: f32, hardness: f32, antialias: f32) -> f32 {
    let coverage = coverage_at(distance_squared, radius, hardness, antialias);
    -(1.0 - coverage).max(0.001).ln()
}

/// Squared distance from `p` to the segment.
fn segment_distance_squared(p: (f32, f32), s: Segment) -> f32 {
    let (vx, vy) = (s[2] - s[0], s[3] - s[1]);
    let t =
        (((p.0 - s[0]) * vx + (p.1 - s[1]) * vy) / (vx * vx + vy * vy).max(1e-12)).clamp(0.0, 1.0);
    let (dx, dy) = (p.0 - (s[0] + t * vx), p.1 - (s[1] + t * vy));
    dx * dx + dy * dy
}

/// The density one sweeping segment deposits on a pixel: the tip's density
/// integrated along the part of the segment the tip reaches, divided by the
/// deposition spacing. Eight-point Gauss-Legendre, clipped to the tip's
/// support, so sparse and dense pointer events paint identically.
fn segment_density(
    p: (f32, f32),
    s: Segment,
    radius: f32,
    hardness: f32,
    antialias: f32,
    spacing: f32,
) -> f32 {
    let (vx, vy) = (s[2] - s[0], s[3] - s[1]);
    let length = (vx * vx + vy * vy).sqrt();
    if length < 1e-6 {
        return tip_density(
            (p.0 - s[0]).powi(2) + (p.1 - s[1]).powi(2),
            radius,
            hardness,
            antialias,
        );
    }
    let (dx, dy) = (p.0 - s[0], p.1 - s[1]);
    let direction = (vx / length, vy / length);
    let projection = dx * direction.0 + dy * direction.1;
    let perpendicular = (dx - projection * direction.0, dy - projection * direction.1);
    let perpendicular_squared =
        perpendicular.0 * perpendicular.0 + perpendicular.1 * perpendicular.1;
    let radius_squared = radius * radius;
    if perpendicular_squared >= radius_squared {
        return 0.0;
    }
    let reach = (radius_squared - perpendicular_squared).sqrt();
    let lo = (projection - reach).max(0.0);
    let hi = (projection + reach).min(length);
    if hi <= lo {
        return 0.0;
    }
    let midpoint = (lo + hi) * 0.5;
    let half_length = (hi - lo) * 0.5;
    const NODES: [f32; 4] = [0.183_434_65, 0.525_532_4, 0.796_666_44, 0.960_289_84];
    const WEIGHTS: [f32; 4] = [0.362_683_78, 0.313_706_66, 0.222_381_03, 0.101_228_54];
    let mut integral = 0.0;
    for i in 0..4 {
        let a = midpoint - half_length * NODES[i] - projection;
        let b = midpoint + half_length * NODES[i] - projection;
        integral += WEIGHTS[i]
            * (tip_density(perpendicular_squared + a * a, radius, hardness, antialias)
                + tip_density(perpendicular_squared + b * b, radius, hardness, antialias));
    }
    integral * half_length / spacing
}

impl BrushStroke {
    /// A stroke over a `width` x `height` layer-pixel canvas.
    pub fn new(width: u32, height: u32, settings: BrushSettings) -> Result<Self, BrushError> {
        settings.validate()?;
        if width == 0 || height == 0 || width > 30_000 || height > 30_000 {
            return Err(BrushError::TooLarge);
        }
        let spacing = (settings.diameter * spacing_fraction(settings.hardness)) as f32;
        Ok(Self {
            width,
            height,
            settings,
            spacing: spacing.max(0.25),
            antialias: 1.0,
            samples: Vec::new(),
            settled: Vec::new(),
            tail: Vec::new(),
            tail_keys: Vec::new(),
            tiles: std::collections::HashMap::new(),
        })
    }

    /// The settings the stroke was built with.
    pub fn settings(&self) -> &BrushSettings {
        &self.settings
    }

    /// A pointer sample in layer pixels. The newest piece of path is drawn
    /// first as a provisional straight tail, then replaced by the curve when
    /// the next sample arrives (or by [`Self::flush`]); the stroke never
    /// trails the cursor. Returns the tiles whose preview changed.
    pub fn append(&mut self, point: (f64, f64)) -> Vec<(usize, usize)> {
        if !point.0.is_finite() || !point.1.is_finite() {
            return Vec::new();
        }
        if self.samples.last() == Some(&point) {
            return Vec::new();
        }
        self.samples.push(point);
        if self.samples.len() > 4 {
            self.samples.remove(0);
        }
        let n = self.samples.len();
        self.settled = if n == 1 {
            vec![segment(point, point)]
        } else if n >= 3 {
            continuous_curve(
                self.samples[n - 3],
                self.samples[n - 2],
                self.samples[n.saturating_sub(4)],
                self.samples[n - 1],
            )
        } else {
            Vec::new()
        };
        self.tail = if n >= 2 {
            vec![segment(self.samples[n - 2], point)]
        } else {
            Vec::new()
        };
        self.update()
    }

    /// Replaces the provisional tail with the stroke's final curve piece and
    /// clears it. Safe to call repeatedly.
    pub fn flush(&mut self) -> Vec<(usize, usize)> {
        let n = self.samples.len();
        if n >= 2 {
            self.settled = continuous_curve(
                self.samples[n - 2],
                self.samples[n - 1],
                self.samples[n.saturating_sub(3)],
                self.samples[n - 1],
            );
            self.samples = vec![self.samples[n - 1]];
        }
        self.tail.clear();
        self.update()
    }

    /// Integrates this update's settled pieces into the permanent density,
    /// re-derives the tail preview, and reports every tile that changed.
    fn update(&mut self) -> Vec<(usize, usize)> {
        let radius = (self.settings.diameter / 2.0) as f32;
        let hardness = self.settings.hardness as f32;
        let antialias = self.antialias;
        let spacing = self.spacing;
        let mut changed = self.tail_keys.clone();
        let mut keys = self.touched_keys(&self.settled);
        let tail_keys = self.touched_keys(&self.tail);
        keys.extend(tail_keys.iter().copied());
        keys.extend(self.tail_keys.iter().copied());
        self.tail_keys = tail_keys;
        // The segment lists are read per pixel; own them outside the tile
        // borrow. The tail is preview-only and lives on.
        let settled = std::mem::take(&mut self.settled);

        for key in keys {
            let tile = self.tile_mut(key);
            let (tw, th) = (tile.width, tile.height);
            let origin = (tile.x as f32, tile.y as f32);
            if hardness >= 1.0 {
                // Hard tips: coverage is the minimum distance to any settled
                // or tail segment; the tail rides on top, never accumulates.
                for index in 0..tw * th {
                    let local = (index % tw, index / tw);
                    let p = (
                        origin.0 + local.0 as f32 + 0.5,
                        origin.1 + local.1 as f32 + 0.5,
                    );
                    let value = settled
                        .iter()
                        .map(|s| {
                            coverage_at(
                                segment_distance_squared(p, *s),
                                radius,
                                hardness,
                                antialias,
                            )
                        })
                        .fold(0.0f32, f32::max);
                    let permanent = &mut tile.permanent[index];
                    *permanent = (*permanent).max(value);
                }
            } else {
                // Soft tips: integrate the new settled pieces into the
                // permanent density; the tail's density is preview-only.
                for index in 0..tw * th {
                    let local = (index % tw, index / tw);
                    let p = (
                        origin.0 + local.0 as f32 + 0.5,
                        origin.1 + local.1 as f32 + 0.5,
                    );
                    let mut value = tile.permanent[index];
                    for s in &settled {
                        value += segment_density(p, *s, radius, hardness, antialias, spacing);
                    }
                    tile.permanent[index] = value.min(DENSITY_CAP);
                }
            }
            changed.push(key);
        }
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .map(|key| (key % self.columns(), key / self.columns()))
            .collect()
    }

    /// The 8-bit coverage preview of a tile: permanent density plus the
    /// current tail, quantized. None for tiles the stroke never touched.
    pub fn tile_coverage(&self, tx: usize, ty: usize) -> Option<Vec<u8>> {
        let key = ty * self.columns() + tx;
        let tile = self.tiles.get(&key)?;
        let radius = (self.settings.diameter / 2.0) as f32;
        let hardness = self.settings.hardness as f32;
        let origin = (tile.x as f32, tile.y as f32);
        let mut preview = Vec::with_capacity(tile.permanent.len());
        for (index, &permanent) in tile.permanent.iter().enumerate() {
            let local = (index % tile.width, index / tile.width);
            let p = (
                origin.0 + local.0 as f32 + 0.5,
                origin.1 + local.1 as f32 + 0.5,
            );
            let value = if hardness >= 1.0 {
                let tail = self
                    .tail
                    .iter()
                    .map(|s| {
                        coverage_at(
                            segment_distance_squared(p, *s),
                            radius,
                            hardness,
                            self.antialias,
                        )
                    })
                    .fold(0.0f32, f32::max);
                permanent.max(tail)
            } else {
                let tail: f32 = self
                    .tail
                    .iter()
                    .map(|s| segment_density(p, *s, radius, hardness, self.antialias, self.spacing))
                    .sum();
                1.0 - (-(permanent + tail).min(DENSITY_CAP)).exp()
            };
            preview.push((value * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        Some(preview)
    }

    /// Composites the stroke onto a layer's pixels. `base` is the whole
    /// layer frame (RGBA, straight alpha); coverage is applied only in the
    /// touched tiles. Paint: source-over of the colour at coverage x opacity.
    /// Erase: the coverage scales the existing alpha down.
    pub fn composite(&self, base: &mut [u8]) {
        let opacity = self.settings.opacity as f32;
        let (r, g, b) = (
            f32::from(self.settings.color[0]),
            f32::from(self.settings.color[1]),
            f32::from(self.settings.color[2]),
        );
        for tile in self.tiles.values() {
            let coverage = match self.tile_coverage(tile.x / TILE_SIZE, tile.y / TILE_SIZE) {
                Some(c) => c,
                None => continue,
            };
            for (index, &c) in coverage.iter().enumerate() {
                if c == 0 {
                    continue;
                }
                let alpha = f32::from(c) / 255.0 * opacity;
                let x = tile.x + index % tile.width;
                let y = tile.y + index / tile.width;
                let pixel = (y * self.width as usize + x) * 4;
                if pixel + 3 >= base.len() {
                    continue;
                }
                if self.settings.erasing {
                    let keep = 1.0 - alpha;
                    base[pixel + 3] = (f32::from(base[pixel + 3]) * keep).round() as u8;
                } else {
                    let (ba, keep) = (f32::from(base[pixel + 3]) / 255.0, 1.0 - alpha);
                    let out_a = alpha + ba * keep;
                    if out_a <= 0.0 {
                        for offset in 0..4 {
                            base[pixel + offset] = 0;
                        }
                        continue;
                    }
                    let mix = |paint: f32, base_channel: f32| {
                        ((paint * alpha + base_channel * ba * keep) / out_a)
                            .round()
                            .clamp(0.0, 255.0) as u8
                    };
                    base[pixel] = mix(r, f32::from(base[pixel]));
                    base[pixel + 1] = mix(g, f32::from(base[pixel + 1]));
                    base[pixel + 2] = mix(b, f32::from(base[pixel + 2]));
                    base[pixel + 3] = (out_a * 255.0).round() as u8;
                }
            }
        }
    }

    /// Tiles with any permanent or tail coverage, as (tx, ty).
    pub fn touched_tiles(&self) -> Vec<(usize, usize)> {
        let mut keys: Vec<usize> = self.tail_keys.to_vec();
        keys.extend(self.tiles.keys().copied());
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter()
            .map(|key| (key % self.columns(), key / self.columns()))
            .collect()
    }

    fn columns(&self) -> usize {
        (self.width as usize).div_ceil(TILE_SIZE)
    }

    fn touched_keys(&self, segments: &[Segment]) -> Vec<usize> {
        let reach = (self.settings.diameter / 2.0 + 2.0) as f32;
        let mut keys = Vec::new();
        for s in segments {
            let min_x = (s[0].min(s[2]) - reach).max(0.0);
            let min_y = (s[1].min(s[3]) - reach).max(0.0);
            let max_x = (s[0].max(s[2]) + reach).min(self.width as f32);
            let max_y = (s[1].max(s[3]) + reach).min(self.height as f32);
            if min_x >= max_x || min_y >= max_y {
                continue;
            }
            let tx0 = min_x as usize / TILE_SIZE;
            let ty0 = min_y as usize / TILE_SIZE;
            let tx1 = (max_x.ceil() as usize).saturating_sub(1) / TILE_SIZE;
            let ty1 = (max_y.ceil() as usize).saturating_sub(1) / TILE_SIZE;
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    keys.push(ty * self.columns() + tx);
                }
            }
        }
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn tile_mut(&mut self, key: usize) -> &mut Tile {
        let tx = key % self.columns();
        let ty = key / self.columns();
        self.tiles.entry(key).or_insert_with(|| {
            let width = (TILE_SIZE as u32).min(self.width - tx as u32 * TILE_SIZE as u32) as usize;
            let height =
                (TILE_SIZE as u32).min(self.height - ty as u32 * TILE_SIZE as u32) as usize;
            Tile {
                x: tx * TILE_SIZE,
                y: ty * TILE_SIZE,
                width,
                height,
                permanent: vec![0.0; width * height],
            }
        })
    }
}

fn segment(a: (f64, f64), b: (f64, f64)) -> Segment {
    [a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32]
}

/// Centripetal Catmull-Rom between `start` and `end`, adaptively subdivided
/// until the centerline is within 0.2 pixels of the spline. Straight movement
/// stays one segment even at 4K.
fn continuous_curve(
    start: (f64, f64),
    end: (f64, f64),
    before: (f64, f64),
    after: (f64, f64),
) -> Vec<Segment> {
    fn knot(t: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
        t + (b.0 - a.0).hypot(b.1 - a.1).sqrt().max(0.0001)
    }
    fn mix(a: (f64, f64), b: (f64, f64), ta: f64, tb: f64, t: f64) -> (f64, f64) {
        let wa = (tb - t) / (tb - ta);
        let wb = (t - ta) / (tb - ta);
        (a.0 * wa + b.0 * wb, a.1 * wa + b.1 * wb)
    }
    let t0 = 0.0;
    let t1 = knot(t0, before, start);
    let t2 = knot(t1, start, end);
    let t3 = knot(t2, end, after);
    let point = |u: f64| -> (f64, f64) {
        if u == 0.0 {
            return start;
        }
        if u == 1.0 {
            return end;
        }
        let t = t1 + (t2 - t1) * u;
        let a = mix(before, start, t0, t1, t);
        let b = mix(start, end, t1, t2, t);
        let c = mix(end, after, t2, t3, t);
        mix(mix(a, b, t0, t2, t), mix(b, c, t1, t3, t), t1, t2, t)
    };
    let mut result = Vec::new();
    fn subdivide(
        point: &dyn Fn(f64) -> (f64, f64),
        result: &mut Vec<Segment>,
        a: (f64, f64),
        b: (f64, f64),
        lo: f64,
        hi: f64,
        depth: u32,
    ) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length_squared = dx * dx + dy * dy;
        let error = |p: (f64, f64)| -> f64 {
            let t = if length_squared > 0.0 {
                (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_squared).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
        };
        let mid = (lo + hi) / 2.0;
        let m = point(mid);
        let deviation = error(m)
            .max(error(point((lo + mid) / 2.0)))
            .max(error(point((mid + hi) / 2.0)));
        if deviation <= 0.2 || depth >= 10 {
            result.push(segment(a, b));
            return;
        }
        subdivide(point, result, a, m, lo, mid, depth + 1);
        subdivide(point, result, m, b, mid, hi, depth + 1);
    }
    subdivide(&point, &mut result, start, end, 0.0, 1.0, 0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soft() -> BrushSettings {
        BrushSettings {
            diameter: 60.0,
            hardness: 0.0,
            opacity: 1.0,
            color: [255, 0, 0],
            erasing: false,
        }
    }

    /// Peak coverage along a straight stroke's centreline, read from the
    /// preview two pixels off the line so the exact centre sample is avoided.
    fn centreline_coverage(stroke: &BrushStroke, x: usize, y: usize) -> f64 {
        let tx = x / TILE_SIZE;
        let ty = y / TILE_SIZE;
        let coverage = stroke.tile_coverage(tx, ty).expect("touched");
        let index = (y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE);
        f64::from(coverage[index]) / 255.0
    }

    #[test]
    fn spacing_fractions_match_the_reference() {
        assert!((spacing_fraction(1.0) - 0.015).abs() < 1e-9);
        assert!((spacing_fraction(0.0) - 0.025).abs() < 1e-9);
        assert!((spacing_fraction(0.5) - 0.025).abs() < 1e-9);
    }

    #[test]
    fn a_single_click_paints_a_full_soft_centre() {
        let mut stroke = BrushStroke::new(400, 400, soft()).expect("stroke");
        stroke.append((200.0, 200.0));
        stroke.flush();
        // The centre of a fully-integrated tip saturates the density cap.
        assert!(centreline_coverage(&stroke, 200, 200) > 0.99);
        // The rim falls to zero.
        assert!(centreline_coverage(&stroke, 200 + 29, 200) < 0.02);
    }

    #[test]
    fn sparse_and_dense_pointer_events_paint_the_same() {
        // The invariant the density integral exists for: one long segment and
        // the same path chopped into forty events must agree within a step of
        // the quantization.
        let run = |points: &[(f64, f64)]| -> f64 {
            let mut stroke = BrushStroke::new(800, 200, soft()).expect("stroke");
            for &p in points {
                stroke.append(p);
            }
            stroke.flush();
            centreline_coverage(&stroke, 400, 100)
        };
        let dense: Vec<(f64, f64)> = (0..=40)
            .map(|i| (100.0 + 600.0 * i as f64 / 40.0, 100.0))
            .collect();
        let sparse = vec![(100.0, 100.0), (700.0, 100.0)];
        let (a, b) = (run(&dense), run(&sparse));
        assert!((a - b).abs() < 0.02, "dense {a} vs sparse {b}");
        // And both are essentially full coverage along an 800px run of a
        // 60px tip: many overlapping dabs saturate.
        assert!(a > 0.95);
    }

    #[test]
    fn the_opacity_setting_caps_the_whole_stroke() {
        let settings = BrushSettings {
            opacity: 0.3,
            ..soft()
        };
        let mut base = vec![0u8; 800 * 200 * 4];
        let mut stroke = BrushStroke::new(800, 200, settings).expect("stroke");
        for i in 0..=40 {
            stroke.append((100.0 + 600.0 * i as f64 / 40.0, 100.0));
        }
        stroke.flush();
        stroke.composite(&mut base);
        // Everywhere on the centreline the alpha stays at the cap - the
        // 0.3 * saturated coverage product - never climbing with overlaps.
        let alpha = f64::from(base[(100 * 800 + 400) * 4 + 3]) / 255.0;
        assert!((alpha - 0.3).abs() < 0.02, "alpha {alpha}");
    }

    #[test]
    fn repeated_flushes_do_not_double_count_the_tail() {
        let mut stroke = BrushStroke::new(400, 400, soft()).expect("stroke");
        stroke.append((100.0, 100.0));
        stroke.append((300.0, 300.0));
        stroke.flush();
        let once = stroke.tile_coverage(0, 0).expect("touched");
        stroke.flush();
        stroke.flush();
        let again = stroke.tile_coverage(0, 0).expect("touched");
        assert_eq!(once, again, "flush must be idempotent");
    }

    #[test]
    fn the_tail_never_bakes_into_permanent_density() {
        // The tail (the straight run to the cursor) covers the chord; the
        // settled curve replacing it bows away. After flush, no pixel may
        // keep coverage beyond the settled curve's own reach - a baked tail
        // would leave paint on the abandoned chord.
        let mut stroke = BrushStroke::new(600, 400, soft()).expect("stroke");
        stroke.append((100.0, 200.0));
        stroke.append((500.0, 200.0));
        stroke.append((300.0, 380.0));
        stroke.flush();
        let mut curve = continuous_curve(
            (100.0, 200.0),
            (500.0, 200.0),
            (100.0, 200.0),
            (300.0, 380.0),
        );
        // The flush piece: the last curve from B to C, with A as the knot before.
        curve.extend(continuous_curve(
            (500.0, 200.0),
            (300.0, 380.0),
            (100.0, 200.0),
            (300.0, 380.0),
        ));
        let radius = (soft().diameter / 2.0) as f32;
        let limit = (radius + 2.0) * (radius + 2.0);
        for ty in 0..2 {
            for tx in 0..3 {
                let Some(coverage) = stroke.tile_coverage(tx, ty) else {
                    continue;
                };
                let width = TILE_SIZE.min(600 - tx * TILE_SIZE);
                for (index, &c) in coverage.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    let p = (
                        (tx * TILE_SIZE + index % width) as f32 + 0.5,
                        (ty * TILE_SIZE + index / width) as f32 + 0.5,
                    );
                    let nearest = curve
                        .iter()
                        .map(|s| segment_distance_squared(p, *s))
                        .fold(f32::INFINITY, f32::min);
                    assert!(
                        nearest <= limit,
                        "paint at ({}, {}) is {:.1}px from the settled curve - the tail left residue",
                        p.0,
                        p.1,
                        nearest.sqrt()
                    );
                }
            }
        }
    }

    #[test]
    fn a_hard_tip_keeps_its_silhouette_and_antialiased_rim() {
        let settings = BrushSettings {
            hardness: 1.0,
            ..soft()
        };
        let mut stroke = BrushStroke::new(400, 400, settings).expect("stroke");
        stroke.append((200.0, 200.0));
        stroke.flush();
        assert!(centreline_coverage(&stroke, 200, 200) > 0.99);
        // Just inside the 30px radius: solid.
        assert!(centreline_coverage(&stroke, 200 + 28, 200) > 0.99);
        // Well outside, same tile: nothing.
        assert_eq!(
            stroke.tile_coverage(0, 0).map(|c| c[200 * TILE_SIZE]),
            Some(0)
        );
    }

    #[test]
    fn coverage_is_continuous_across_tile_boundaries() {
        let mut stroke = BrushStroke::new(600, 600, soft()).expect("stroke");
        stroke.append((100.0, 255.0));
        stroke.append((500.0, 257.0));
        stroke.flush();
        // Read symmetric pairs either side of the y=256 boundary.
        for x in [150, 300, 450] {
            let above = centreline_coverage(&stroke, x, 255);
            let below = centreline_coverage(&stroke, x, 257);
            assert!(
                (above - below).abs() < 0.15,
                "x {x}: above {above} vs below {below}"
            );
        }
    }

    #[test]
    fn erasing_scales_the_existing_alpha_down() {
        let settings = BrushSettings {
            erasing: true,
            ..soft()
        };
        let mut base = vec![0u8; 200 * 200 * 4];
        for pixel in base.chunks_exact_mut(4) {
            pixel[..3].fill(200);
            pixel[3] = 255;
        }
        let mut stroke = BrushStroke::new(200, 200, settings).expect("stroke");
        stroke.append((100.0, 100.0));
        stroke.flush();
        stroke.composite(&mut base);
        let alpha = base[(100 * 200 + 100) * 4 + 3];
        assert!(alpha < 40, "centre should be nearly erased, got {alpha}");
        assert_eq!(base[(100 * 200 + 100) * 4], 200, "erase keeps rgb");
    }

    #[test]
    fn paint_composites_as_source_over() {
        let mut base = vec![0u8; 200 * 200 * 4];
        let mut stroke = BrushStroke::new(200, 200, soft()).expect("stroke");
        stroke.append((100.0, 100.0));
        stroke.flush();
        stroke.composite(&mut base);
        let pixel = &base[(100 * 200 + 100) * 4..(100 * 200 + 100) * 4 + 4];
        assert_eq!(pixel[0], 255);
        assert_eq!(pixel[1], 0);
        assert!(pixel[3] > 250);
    }

    #[test]
    fn straight_runs_stay_one_segment() {
        let curve = continuous_curve((0.0, 0.0), (1000.0, 0.0), (-100.0, 0.0), (1100.0, 0.0));
        assert_eq!(curve.len(), 1, "a straight run needs no subdivision");
    }

    #[test]
    fn a_bent_run_subdivides_to_the_tolerance() {
        let curve = continuous_curve((0.0, 0.0), (100.0, 100.0), (-100.0, 200.0), (300.0, 0.0));
        assert!(curve.len() > 1, "a sharp bend must be subdivided");
    }

    #[test]
    fn repeated_and_non_finite_points_are_ignored() {
        let mut stroke = BrushStroke::new(500, 100, soft()).expect("stroke");
        stroke.append((50.0, 50.0));
        // A repeat and a NaN change nothing.
        assert!(stroke.append((50.0, 50.0)).is_empty());
        assert!(stroke.append((f64::NAN, 0.0)).is_empty());
        assert_eq!(stroke.samples.len(), 1);
        // A point past the layer edge is clipped by the tiles, not rejected -
        // like the original, which only rejected non-finite input.
        assert!(!stroke.append((400.0, 50.0)).is_empty());
    }

    #[test]
    fn settings_are_validated() {
        assert!(
            BrushStroke::new(
                10,
                10,
                BrushSettings {
                    diameter: 0.5,
                    ..soft()
                }
            )
            .is_err()
        );
        assert!(
            BrushStroke::new(
                10,
                10,
                BrushSettings {
                    diameter: 3000.0,
                    ..soft()
                }
            )
            .is_err()
        );
        assert!(
            BrushStroke::new(
                10,
                10,
                BrushSettings {
                    opacity: 0.0,
                    ..soft()
                }
            )
            .is_err()
        );
        assert!(
            BrushStroke::new(
                10,
                10,
                BrushSettings {
                    hardness: 2.0,
                    ..soft()
                }
            )
            .is_err()
        );
        assert!(BrushStroke::new(0, 10, soft()).is_err());
    }

    #[test]
    fn tiles_clip_at_the_layer_edge() {
        let mut stroke = BrushStroke::new(300, 300, soft()).expect("stroke");
        stroke.append((290.0, 290.0));
        stroke.flush();
        let (tx, ty) = (1, 1);
        let coverage = stroke.tile_coverage(tx, ty).expect("edge tile");
        assert_eq!(coverage.len(), 44 * 44, "the last tile is 300-256=44 wide");
    }
}
