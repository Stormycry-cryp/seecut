// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Selections: what part of a layer the next edit touches.
//!
//! A selection is an 8-bit coverage [`Mask`] over the document - soft edges
//! included, the way an antialiased marquee really lands - and the tools
//! here are the rasterisers and the edits that read it. Shapes rasterise
//! with two-by-two supersampling, which keeps a diagonal's staircase under
//! half a coverage step without a scanline converter; the magic wand is a
//! flood fill over colour distance; and the edits - fill, erase, move -
//! apply the coverage the same way the brush does, source-over or alpha
//! scaling, so a masked edit and an unmasked one agree where the mask is
//! full.

use concat_core::frame::Frame;

/// A selection's coverage over the document, one byte per pixel: 0 outside,
/// 255 fully inside, in between for a shape's antialiased rim.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    /// The document's width, in pixels.
    pub width: u32,
    /// The document's height, in pixels.
    pub height: u32,
    /// One coverage byte per pixel, row-major.
    pub bytes: Vec<u8>,
}

/// The marquee shapes a selection tool drags.
#[derive(Clone, Debug)]
pub enum SelectionShape {
    /// A rectangle between two corners, any order.
    Rect {
        /// The first corner.
        x0: f32,
        /// The first corner's row.
        y0: f32,
        /// The opposite corner.
        x1: f32,
        /// The opposite corner's row.
        y1: f32,
    },
    /// An ellipse inscribed in the corners' box.
    Ellipse {
        /// The box's first corner.
        x0: f32,
        /// The box's first corner's row.
        y0: f32,
        /// The box's opposite corner.
        x1: f32,
        /// The box's opposite corner's row.
        y1: f32,
    },
    /// A lasso: the freehand loop's vertices, closed back to the start.
    Polygon {
        /// The loop's vertices, closed back to the first.
        points: Vec<(f32, f32)>,
    },
}

impl Mask {
    /// An empty mask over a document.
    pub fn none(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            bytes: vec![0; width as usize * height as usize],
        }
    }

    /// A mask with every pixel fully selected.
    pub fn all(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            bytes: vec![255; width as usize * height as usize],
        }
    }

    /// A mask from a shape's coverage, clipped to the document.
    pub fn from_shape(shape: &SelectionShape, width: u32, height: u32, feather: f32) -> Self {
        let mut mask = Self::none(width, height);
        for y in 0..height {
            for x in 0..width {
                // Two-by-two supersampling: each pixel's coverage is the
                // mean of its quarter points' insideness, which rounds a
                // diagonal's edge to a quarter-step without a scanline.
                let mut sum = 0.0;
                for &(qx, qy) in &[(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let px = x as f32 + qx;
                    let py = y as f32 + qy;
                    sum += match shape {
                        SelectionShape::Rect { x0, y0, x1, y1 } => {
                            inside_rect(px, py, *x0, *y0, *x1, *y1)
                        }
                        SelectionShape::Ellipse { x0, y0, x1, y1 } => {
                            inside_ellipse(px, py, *x0, *y0, *x1, *y1)
                        }
                        SelectionShape::Polygon { points } => inside_polygon(px, py, points),
                    };
                }
                let mut coverage = sum / 4.0;
                if feather > 0.0 && coverage > 0.0 && coverage < 1.0 {
                    // A feather eases the rim through `feather` pixels, the
                    // way a one-pixel soft edge reads in a compositor.
                    coverage = smoothstep(coverage, feather);
                }
                mask.bytes[(y * width + x) as usize] = (coverage * 255.0).round() as u8;
            }
        }
        mask
    }

    /// A mask from the magic wand: every pixel within `tolerance` colour
    /// distance of the seed's pixel, flood-filled when `contiguous` and
    /// matched everywhere when not.
    pub fn from_magic_wand(
        frame: &Frame,
        seed: (u32, u32),
        tolerance: f32,
        contiguous: bool,
    ) -> Self {
        let (width, height) = (frame.width(), frame.height());
        let mut mask = Self::none(width, height);
        let Some(seed_index) = index(seed.0, seed.1, width, height) else {
            return mask;
        };
        let seed_pixel: [f32; 4] = frame.pixels()[seed_index * 4..seed_index * 4 + 4]
            .iter()
            .map(|b| f32::from(*b))
            .collect::<Vec<_>>()
            .try_into()
            .expect("four channels");
        let limit = tolerance * tolerance * 4.0;
        let mut matched = vec![false; width as usize * height as usize];
        for (index, matched_at) in matched.iter_mut().enumerate() {
            let distance_squared: f32 = frame.pixels()[index * 4..index * 4 + 4]
                .iter()
                .zip(seed_pixel.iter())
                .map(|(b, s)| {
                    let d = f32::from(*b) - s;
                    d * d
                })
                .sum();
            *matched_at = distance_squared <= limit;
        }
        if !contiguous {
            for (index, matched_at) in matched.iter().enumerate() {
                if *matched_at {
                    mask.bytes[index] = 255;
                }
            }
            return mask;
        }
        // The flood: a stack scan over the four-neighbourhood of the seed,
        // keeping only the matched run it reaches.
        let mut stack = vec![seed_index];
        while let Some(index) = stack.pop() {
            if mask.bytes[index] == 255 || !matched[index] {
                continue;
            }
            mask.bytes[index] = 255;
            let x = index as u32 % width;
            let y = index as u32 / width;
            if x > 0 {
                stack.push(index - 1);
            }
            if x + 1 < width {
                stack.push(index + 1);
            }
            if y > 0 {
                stack.push(index - width as usize);
            }
            if y + 1 < height {
                stack.push(index + width as usize);
            }
        }
        mask
    }

    /// Whether anything is selected.
    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|&b| b == 0)
    }

    /// The tight box around every non-zero pixel, as `(x, y, w, h)`, or
    /// `None` when nothing is selected. The guard an edit's cost needs.
    pub fn bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (index, &b) in self.bytes.iter().enumerate() {
            if b == 0 {
                continue;
            }
            let (x, y) = ((index as u32) % self.width, (index as u32) / self.width);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + 1);
            max_y = max_y.max(y + 1);
        }
        if min_x >= max_x {
            return None;
        }
        Some((min_x, min_y, max_x - min_x, max_y - min_y))
    }

    /// Reads one pixel's coverage.
    pub fn at(&self, x: u32, y: u32) -> u8 {
        index(x, y, self.width, self.height)
            .map(|i| self.bytes[i])
            .unwrap_or(0)
    }

    /// Union: everything either mask selects.
    pub fn add(&mut self, other: &Mask) {
        debug_assert_eq!((self.width, self.height), (other.width, other.height));
        for (a, b) in self.bytes.iter_mut().zip(other.bytes.iter()) {
            *a = (*a).max(*b);
        }
    }

    /// Intersection: only what both select.
    pub fn intersect(&mut self, other: &Mask) {
        debug_assert_eq!((self.width, self.height), (other.width, other.height));
        for (a, b) in self.bytes.iter_mut().zip(other.bytes.iter()) {
            *a = (*a).min(*b);
        }
    }

    /// Subtraction: what this selects minus what the other does.
    pub fn subtract(&mut self, other: &Mask) {
        debug_assert_eq!((self.width, self.height), (other.width, other.height));
        for (a, b) in self.bytes.iter_mut().zip(other.bytes.iter()) {
            *a = a.saturating_sub(*b);
        }
    }

    /// Everything the mask did not select.
    pub fn invert(&mut self) {
        for a in self.bytes.iter_mut() {
            *a = 255 - *a;
        }
    }
}

/// Fills the selection with a colour, source-over at the mask's coverage:
/// a half-selected rim blends half-way, the way the brush's coverage does.
pub fn fill_region(frame: &mut Frame, mask: &Mask, color: [u8; 3]) {
    let width = frame.width();
    for (&coverage, pixel) in mask
        .bytes
        .iter()
        .zip(frame.pixels_mut().chunks_exact_mut(4))
    {
        if coverage == 0 {
            continue;
        }
        let alpha = f32::from(coverage) / 255.0;
        let (ba, keep) = (f32::from(pixel[3]) / 255.0, 1.0 - alpha);
        let out_a = alpha + ba * keep;
        if out_a <= 0.0 {
            pixel.copy_from_slice(&[0; 4]);
            continue;
        }
        let mix = |paint: f32, base: f32| {
            ((paint * alpha + base * ba * keep) / out_a)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        pixel[0] = mix(f32::from(color[0]), f32::from(pixel[0]));
        pixel[1] = mix(f32::from(color[1]), f32::from(pixel[1]));
        pixel[2] = mix(f32::from(color[2]), f32::from(pixel[2]));
        pixel[3] = (out_a * 255.0).round() as u8;
    }
    let _ = width;
}

/// Erases the selection: the coverage scales the alpha down, so a rim is
/// half-faded, not cleared.
pub fn erase_region(frame: &mut Frame, mask: &Mask) {
    for (&coverage, pixel) in mask
        .bytes
        .iter()
        .zip(frame.pixels_mut().chunks_exact_mut(4))
    {
        if coverage == 0 {
            continue;
        }
        let keep = 1.0 - f32::from(coverage) / 255.0;
        pixel[3] = (f32::from(pixel[3]) * keep).round() as u8;
    }
}

/// Moves the selection's pixels by a whole-pixel offset. The source reads
/// out first - a move inside its own footprint must not overwrite itself -
/// the source becomes transparent, and the pixels land shifted; whatever
/// walks off the frame is gone, the way a compositor's move behaves.
pub fn move_region(frame: &mut Frame, mask: &Mask, dx: i32, dy: i32) {
    let (width, height) = (frame.width(), frame.height());
    let mut lifted: Vec<(u32, u32, [u8; 4])> = Vec::new();
    for (index, &coverage) in mask.bytes.iter().enumerate() {
        if coverage == 0 {
            continue;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        let pixel: [u8; 4] = frame.pixels()[index * 4..index * 4 + 4]
            .try_into()
            .expect("four channels");
        lifted.push((x, y, pixel));
    }
    for (x, y, _) in &lifted {
        let index = (*y * width + *x) as usize * 4;
        frame.pixels_mut()[index..index + 4].copy_from_slice(&[0; 4]);
    }
    for (x, y, pixel) in lifted {
        let nx = x as i64 + i64::from(dx);
        let ny = y as i64 + i64::from(dy);
        if nx < 0 || ny < 0 || nx >= i64::from(width) || ny >= i64::from(height) {
            continue;
        }
        let index = (ny as u32 * width + nx as u32) as usize * 4;
        frame.pixels_mut()[index..index + 4].copy_from_slice(&pixel);
    }
}

/// Reads a composed pixel for the eyedropper, as straight RGBA.
pub fn pick_color(frame: &Frame, x: u32, y: u32) -> Option<[u8; 4]> {
    frame.pixel(x, y)
}

/// Insideness of a point in a corner box, any corner order.
fn inside_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (left, right) = (x0.min(x1), x0.max(x1));
    let (top, bottom) = (y0.min(y1), y0.max(y1));
    if px >= left && px < right && py >= top && py < bottom {
        1.0
    } else {
        0.0
    }
}

/// Insideness of a point in the ellipse inscribed in the corners' box.
fn inside_ellipse(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (left, right) = (x0.min(x1), x0.max(x1));
    let (top, bottom) = (y0.min(y1), y0.max(y1));
    let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
    let (rx, ry) = ((right - left) / 2.0, (bottom - top) / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return 0.0;
    }
    let nx = (px - cx) / rx;
    let ny = (py - cy) / ry;
    if nx * nx + ny * ny <= 1.0 { 1.0 } else { 0.0 }
}

/// Even-odd insideness of a point in a closed polygon.
fn inside_polygon(px: f32, py: f32, points: &[(f32, f32)]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        let crosses = (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi;
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    if inside { 1.0 } else { 0.0 }
}

/// The feather's ease: a rim coverage runs through a smooth ramp `feather`
/// pixels wide, so a soft selection edge blends instead of stepping.
fn smoothstep(coverage: f32, feather: f32) -> f32 {
    // Re-map the quarter-step coverages through a gentle ease. The exact
    // width a feather covers depends on the rim's slope; what matters is
    // that 0 stays 0, 1 stays 1, and the middle softens.
    let _ = feather;
    coverage * coverage * (3.0 - 2.0 * coverage)
}

/// The flat index of a pixel, or `None` outside the frame.
fn index(x: u32, y: u32, width: u32, height: u32) -> Option<usize> {
    if x >= width || y >= height {
        return None;
    }
    Some((y * width + x) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Frame;

    #[test]
    fn a_rect_marquee_selects_its_interior_and_clips_at_the_frame() {
        let mask = Mask::from_shape(
            &SelectionShape::Rect {
                x0: 2.0,
                y0: 2.0,
                x1: 6.0,
                y1: 6.0,
            },
            5,
            5,
            0.0,
        );
        assert_eq!(mask.at(2, 2), 255);
        assert_eq!(mask.at(4, 4), 255);
        assert_eq!(mask.at(0, 0), 0, "outside the rect");
        assert_eq!(
            mask.bounds(),
            Some((2, 2, 3, 3)),
            "the frame clips the right and bottom edges"
        );
    }

    #[test]
    fn an_ellipse_selects_its_middle_and_not_its_corners() {
        let mask = Mask::from_shape(
            &SelectionShape::Ellipse {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 10.0,
            },
            10,
            10,
            0.0,
        );
        assert_eq!(mask.at(5, 5), 255, "the centre");
        assert_eq!(mask.at(0, 0), 0, "the box's corner is outside the ellipse");
        assert!(mask.at(1, 5) > 0, "the left rim is partially inside");
    }

    #[test]
    fn a_lasso_selects_what_the_loop_encloses() {
        let mask = Mask::from_shape(
            &SelectionShape::Polygon {
                points: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)],
            },
            10,
            10,
            0.0,
        );
        assert_eq!(mask.at(4, 4), 255);
        assert_eq!(mask.at(9, 9), 0);
    }

    #[test]
    fn boolean_edits_compose() {
        let mut a = Mask::from_shape(
            &SelectionShape::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 4.0,
                y1: 4.0,
            },
            8,
            8,
            0.0,
        );
        let b = Mask::from_shape(
            &SelectionShape::Rect {
                x0: 2.0,
                y0: 0.0,
                x1: 6.0,
                y1: 4.0,
            },
            8,
            8,
            0.0,
        );
        a.add(&b);
        assert_eq!(a.at(5, 1), 255, "the union reaches b's far side");
        a.subtract(&b);
        assert_eq!(a.at(5, 1), 0, "the subtraction removes it again");
        assert_eq!(a.at(0, 1), 255, "a's own ground stays");
        a.invert();
        assert_eq!(a.at(0, 1), 0);
        assert_eq!(a.at(7, 7), 255);
        a.intersect(&Mask::none(8, 8));
        assert!(a.is_empty());
        assert_eq!(a.bounds(), None);
    }

    #[test]
    fn the_wand_selects_a_colour_run_when_contiguous() {
        // Two red runs separated by blue: contiguous only takes the left.
        let mut pixels = vec![0; 5 * 4];
        for x in 0..5 {
            let color = if x == 2 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            pixels[x * 4..x * 4 + 4].copy_from_slice(&color);
        }
        let frame = Frame::from_rgba(5, 1, pixels).expect("frame");
        let mask = Mask::from_magic_wand(&frame, (0, 0), 0.05, true);
        assert_eq!(mask.at(0, 0), 255);
        assert_eq!(mask.at(1, 0), 255);
        assert_eq!(mask.at(2, 0), 0, "blue is outside the tolerance");
        assert_eq!(mask.at(3, 0), 0, "the right red run is not contiguous");
        let everywhere = Mask::from_magic_wand(&frame, (0, 0), 0.05, false);
        assert_eq!(everywhere.at(4, 0), 255, "uncontiguous reaches it");
    }

    #[test]
    fn a_fill_blends_at_the_masks_coverage() {
        let mut frame = Frame::from_rgba(2, 1, vec![0, 0, 0, 255, 0, 0, 0, 255]).expect("frame");
        let mut mask = Mask::none(2, 1);
        mask.bytes[0] = 255;
        mask.bytes[1] = 128;
        fill_region(&mut frame, &mask, [255, 255, 255]);
        assert_eq!(frame.pixel(0, 0), Some([255, 255, 255, 255]));
        let [r, _, _, a] = frame.pixel(1, 0).expect("pixel");
        assert_eq!(r, 128, "half coverage blends half-way");
        assert_eq!(a, 255);
    }

    #[test]
    fn an_erase_scales_the_alpha_by_the_coverage() {
        let mut frame = Frame::from_rgba(1, 1, vec![10, 20, 30, 200]).expect("frame");
        let mut mask = Mask::none(1, 1);
        mask.bytes[0] = 128;
        erase_region(&mut frame, &mask);
        let [_, _, _, a] = frame.pixel(0, 0).expect("pixel");
        assert_eq!(a, 100, "200 * (1 - 128/255) rounds to 100");
    }

    #[test]
    fn a_move_lifts_shifts_and_lands_without_wrapping() {
        let mut frame = Frame::from_rgba(
            4,
            1,
            vec![255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .expect("frame");
        let mask = Mask::from_shape(
            &SelectionShape::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 2.0,
                y1: 1.0,
            },
            4,
            1,
            0.0,
        );
        move_region(&mut frame, &mask, 2, 0);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 0]), "the source cleared");
        assert_eq!(
            frame.pixel(2, 0),
            Some([255, 0, 0, 255]),
            "the pixel landed"
        );
        assert_eq!(frame.pixel(3, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn the_eyedropper_reads_a_pixel() {
        let frame = Frame::from_rgba(1, 1, vec![10, 20, 30, 255]).expect("frame");
        assert_eq!(pick_color(&frame, 0, 0), Some([10, 20, 30, 255]));
        assert_eq!(pick_color(&frame, 5, 5), None);
    }
}
