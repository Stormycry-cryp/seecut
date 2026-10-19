// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Titles as pixels.
//!
//! A text clip is a style and some words; the compositor wants a picture. This
//! crate is the step between: it finds the face, shapes each line, turns the
//! glyph outlines into paths, and paints them - plate, shadow, outline, fill -
//! onto a canvas the size of the output frame, transparent everywhere the
//! words are not.
//!
//! Frame-sized on purpose. The compositor places a picture by fitting it into
//! the frame and then applying the clip's transform about its centre, so a
//! canvas that *is* the frame fits at exactly one, decodes without resampling,
//! and puts the canvas's centre where the clip's centre is. The clip's offset
//! and rotation then mean the same thing for a title as for footage.
//!
//! Where the block sits on that canvas is the alignment's to say. A centred
//! title has its block centred, so the clip's position is the block's middle.
//! A left-aligned one has its block's left edge on the canvas's centre, a
//! right-aligned one its right edge: the position is the edge the words are
//! aligned to, and typing more grows the block *away* from that edge rather
//! than out from the middle - which is what left and right mean everywhere
//! else, and what a title that keeps its left margin while its words change
//! needs. [`Rendered`] reports where the block landed so a monitor can draw
//! its outline there.
//!
//! Sizes in the style are fractions of the frame's height, as the document
//! stores them, so a title looks the same at 720p and 4K. Everything here
//! converts to pixels once, at the top.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect,
    Stroke, Transform,
};

/// How a title's lines sit within their block, and which point of the block
/// the clip's position holds still: its left edge, its centre or its right
/// edge. See the module docs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Align {
    /// Lines share a left edge, and the block's left edge is the anchor.
    Left,
    /// Lines are centred on each other, and the block's centre is the anchor.
    #[default]
    Center,
    /// Lines share a right edge, and the block's right edge is the anchor.
    Right,
}

/// Everything about a title's look. Mirrors the document's text style field
/// for field so the host can copy it across; this crate does not depend on
/// the document.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct TitleStyle {
    /// The words, newlines included.
    pub content: String,
    /// CSS-style family name; quotes are tolerated and stripped.
    pub font_family: String,
    /// Em size as a fraction of frame height.
    pub font_size: f64,
    /// CSS-scale weight, 100..=900.
    pub font_weight: f64,
    /// Italic when true.
    pub italic: bool,
    /// Fill colour as `#rrggbb` or `#rrggbbaa`.
    pub color: String,
    /// Line alignment within the block.
    pub align: Align,
    /// Outline thickness as a fraction of frame height; zero for none.
    pub stroke_width: f64,
    /// Outline colour.
    pub stroke_color: String,
    /// A soft drop shadow behind the words.
    pub shadow: bool,
    /// A plate behind the block, `#rrggbb[aa]`; empty for none.
    pub background: String,
    /// Baseline pitch as a multiple of the em.
    pub line_height: f64,
    /// Extra advance after every glyph, as a fraction of frame height.
    pub tracking: f64,
    /// The widest a line may run, as a fraction of frame width, before its
    /// words wrap; zero for no limit.
    pub max_width: f64,
}

/// The finished title as pixels: the canvas, RGBA with straight alpha,
/// for a monitor that wants it now and not from a file.
#[derive(Clone, PartialEq, Debug)]
pub struct RenderedFrame {
    /// The canvas, `width` by `height` RGBA, alpha straight.
    pub rgba: Vec<u8>,
    /// The canvas width: the frame's.
    pub width: u32,
    /// The canvas height: the frame's.
    pub height: u32,
    /// See [`Rendered::block_width`].
    pub block_width: u32,
    /// See [`Rendered::block_height`].
    pub block_height: u32,
    /// See [`Rendered::block_dx`].
    pub block_dx: i32,
    /// See [`Rendered::block_dy`].
    pub block_dy: i32,
}

/// The finished title.
#[derive(Clone, PartialEq, Debug)]
pub struct Rendered {
    /// The canvas, PNG-encoded, RGBA with alpha.
    pub png: Vec<u8>,
    /// The canvas width: the frame's.
    pub width: u32,
    /// The canvas height: the frame's.
    pub height: u32,
    /// The painted block's width in pixels, plate included - what an
    /// outline on a monitor should be drawn around.
    pub block_width: u32,
    /// The painted block's height, on the same terms.
    pub block_height: u32,
    /// Where the block's centre is, as an offset from the canvas's centre in
    /// pixels, x to the right. Zero for a centred title; half the block's
    /// width for a left-aligned one, whose block starts at the centre; minus
    /// that for a right-aligned one. An outline goes here, not on the clip.
    pub block_dx: i32,
    /// The vertical half of `block_dx`, y down. Always zero for now: the
    /// block is centred vertically whatever the alignment.
    pub block_dy: i32,
}

/// What can go wrong. Fonts fall back rather than fail, so this is short.
#[derive(Debug)]
pub enum Error {
    /// The frame is zero-sized or too large for a pixmap.
    Canvas(u32, u32),
    /// No face at all could be found, not even a system fallback.
    NoFont,
    /// No installed face covers a requested character.
    MissingGlyph(char),
    /// A face was found but its data could not be read as a font.
    BadFont,
    /// PNG encoding failed.
    Encode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Canvas(w, h) => write!(f, "cannot make a {w}×{h} canvas"),
            Error::NoFont => write!(f, "no font found, not even a system fallback"),
            Error::MissingGlyph(c) => write!(f, "no installed font covers U+{:04X}", *c as u32),
            Error::BadFont => write!(f, "the chosen font file could not be parsed"),
            Error::Encode(why) => write!(f, "PNG encoding failed: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The faces available to titles: the system's, plus any files a project
/// carries. Built once and kept; loading the system's fonts is the slow part.
pub struct Fonts {
    db: fontdb::Database,
    blobs: Mutex<HashMap<fontdb::ID, Arc<Vec<u8>>>>,
    fallback: Mutex<HashMap<(fontdb::ID, char), Option<fontdb::ID>>>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    /// The system's fonts.
    pub fn new() -> Fonts {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Fonts {
            db,
            blobs: Mutex::new(HashMap::new()),
            fallback: Mutex::new(HashMap::new()),
        }
    }

    /// Adds one font file. A file that does not parse is skipped; a title
    /// that names its family falls back to a system face.
    pub fn add_file(&mut self, path: &std::path::Path) -> bool {
        self.fallback.get_mut().expect("font cache").clear();
        self.db.load_font_file(path).is_ok()
    }

    /// The best face for a style: the named family at the nearest weight and
    /// slant, then any sans-serif, then anything at all.
    fn pick(&self, style: &TitleStyle) -> Result<fontdb::ID, Error> {
        let family = style
            .font_family
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        let weight = fontdb::Weight(style.font_weight.clamp(100.0, 900.0).round() as u16);
        let slant = if style.italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };
        let mut families: Vec<fontdb::Family<'_>> = Vec::new();
        if !family.is_empty() {
            families.push(fontdb::Family::Name(family));
        }
        families.push(fontdb::Family::SansSerif);
        let query = fontdb::Query {
            families: &families,
            weight,
            stretch: fontdb::Stretch::Normal,
            style: slant,
        };
        let id = self
            .db
            .query(&query)
            .or_else(|| self.db.faces().next().map(|face| face.id))
            .ok_or(Error::NoFont)?;
        Ok(id)
    }

    fn blob(&self, id: fontdb::ID) -> Result<Arc<Vec<u8>>, Error> {
        let mut cache = self.blobs.lock().expect("font cache");
        if let Some(blob) = cache.get(&id) {
            return Ok(blob.clone());
        }
        let blob = self
            .db
            .with_face_data(id, |data, index| {
                let mut out = index.to_le_bytes().to_vec();
                out.extend(data);
                Arc::new(out)
            })
            .ok_or(Error::BadFont)?;
        cache.insert(id, blob.clone());
        Ok(blob)
    }

    fn coverage(&self, primary: fontdb::ID, c: char) -> Result<fontdb::ID, Error> {
        if c.is_whitespace() {
            return Ok(primary);
        }
        let key = (primary, c);
        let known = {
            let cache = self.fallback.lock().expect("font cache");
            if let Some(found) = cache.get(&key) {
                return found.ok_or(Error::MissingGlyph(c));
            }
            let mut known = Vec::new();
            for ((face, _), fallback) in cache.iter() {
                if *face == primary
                    && let Some(id) = fallback
                    && *id != primary
                    && !known.contains(id)
                {
                    known.push(*id);
                }
            }
            known
        };
        let covers = |id| {
            self.blob(id).ok().is_some_and(|blob| {
                let index = u32::from_le_bytes(blob[..4].try_into().expect("font index"));
                ttf_parser::Face::parse(&blob[4..], index)
                    .ok()
                    .is_some_and(|f| f.glyph_index(c).is_some())
            })
        };
        let usable = |face: &fontdb::FaceInfo| {
            !face.families.iter().any(|(name, _)| {
                name.replace(' ', "")
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case("LastResort")
            }) && covers(face.id)
        };
        let requested = self.db.face(primary).ok_or(Error::BadFont)?;
        let found = if usable(requested) {
            Some(primary)
        } else if let Some(id) = known.into_iter().find(|id| covers(*id)) {
            Some(id)
        } else {
            let mut candidates: Vec<_> = self.db.faces().collect();
            candidates.sort_by_key(|f| {
                u32::from(f.weight.0.abs_diff(requested.weight.0))
                    + if f.style == requested.style { 0 } else { 1000 }
            });
            candidates.into_iter().find(|f| usable(f)).map(|f| f.id)
        };
        self.fallback.lock().expect("font cache").insert(key, found);
        found.ok_or(Error::MissingGlyph(c))
    }

    /// Requested family is kept by the document, even when this machine uses a fallback.
    pub fn has_family(&self, family: &str) -> bool {
        self.db.faces().any(|f| {
            f.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    /// Available installed family names, deduplicated for native controls.
    pub fn families(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .db
            .faces()
            .flat_map(|f| f.families.iter().map(|(n, _)| n.clone()))
            .filter(|name| !name.starts_with('.') && !name.eq_ignore_ascii_case("LastResort"))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Shared, lazily discovered system fonts; font data and fallback coverage stay cached.
pub fn system_fonts() -> &'static Fonts {
    static FONTS: std::sync::OnceLock<Fonts> = std::sync::OnceLock::new();
    FONTS.get_or_init(Fonts::new)
}

/// One shaped line: its outline path in pixels, pen at the origin, and its
/// advance width.
struct Line {
    path: Option<Path>,
    width: f32,
}

/// A colour from `#rrggbb` or `#rrggbbaa`; anything else is `None`.
fn colour(hex: &str) -> Option<Color> {
    let hex = hex.trim().strip_prefix('#')?;
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    match hex.len() {
        6 => Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, 255).into(),
        8 => Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, byte(6)?).into(),
        _ => None,
    }
}

/// Collects a glyph's outline, scaled and placed, into a path under
/// construction. The font's y goes up; the canvas's goes down.
struct Outliner<'a> {
    builder: &'a mut PathBuilder,
    scale: f32,
    x: f32,
    y: f32,
}

impl ttf_parser::OutlineBuilder for Outliner<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder
            .move_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder
            .line_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x2 * self.scale,
            self.y - y2 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

/// Chinese characters are legal line boundaries; Latin words stay together.
fn wrap_units(text: &str) -> Vec<String> {
    let mut units = Vec::<String>::new();
    let mut word = String::new();
    for c in text.chars() {
        let cjk = matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x323AF);
        let closing = "，。！？、；：）》】」』”.!?;:,".contains(c);
        if closing {
            if !word.is_empty() {
                word.push(c);
                continue;
            }
            if let Some(last) = units.last_mut() {
                last.push(c);
                continue;
            }
        }
        if cjk || c.is_whitespace() {
            if !word.is_empty() {
                units.push(std::mem::take(&mut word));
            }
            units.push(c.to_string());
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        units.push(word);
    }
    units
}

fn mixed_line(
    faces: &[rustybuzz::Face<'_>],
    coverage: &HashMap<char, usize>,
    text: &str,
    em: f32,
    tracking: f32,
) -> Line {
    let mut builder = PathBuilder::new();
    let mut width = 0.0;
    let mut run = String::new();
    let mut index = 0;
    let mut append = |index: usize, run: &str| {
        if run.is_empty() {
            return;
        }
        let line = shape_line(&faces[index], run, em, tracking);
        if let Some(path) = line
            .path
            .and_then(|p| p.transform(Transform::from_translate(width, 0.0)))
        {
            builder.push_path(&path);
        }
        width += line.width + tracking;
    };
    for c in text.chars() {
        let next = *coverage.get(&c).unwrap_or(&0);
        if next != index {
            append(index, &run);
            run.clear();
            index = next;
        }
        run.push(c);
    }
    append(index, &run);
    Line {
        path: builder.finish(),
        width: (width - tracking).max(0.0),
    }
}

fn wrap_line(
    faces: &[rustybuzz::Face<'_>],
    coverage: &HashMap<char, usize>,
    text: &str,
    em: f32,
    tracking: f32,
    max_w: f32,
) -> Vec<Line> {
    let shape = |s: &str| mixed_line(faces, coverage, s, em, tracking);
    if max_w <= 0.0 || text.trim().is_empty() {
        return vec![shape(text)];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for unit in wrap_units(text) {
        let candidate = format!("{current}{unit}");
        if !current.trim().is_empty() && !unit.trim().is_empty() && shape(&candidate).width > max_w
        {
            lines.push(shape(current.trim_end()));
            current = unit;
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(shape(current.trim_end()));
    }
    lines
}

fn shape_line(face: &rustybuzz::Face<'_>, text: &str, em: f32, tracking: f32) -> Line {
    if text.is_empty() {
        return Line {
            path: None,
            width: 0.0,
        };
    }
    let scale = em / face.units_per_em() as f32;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let shaped = rustybuzz::shape(face, &[], buffer);
    let mut builder = PathBuilder::new();
    let mut pen = 0.0_f32;
    for (info, position) in shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions().iter())
    {
        let glyph = ttf_parser::GlyphId(info.glyph_id as u16);
        let mut outliner = Outliner {
            builder: &mut builder,
            scale,
            x: pen + position.x_offset as f32 * scale,
            y: -(position.y_offset as f32 * scale),
        };
        face.outline_glyph(glyph, &mut outliner);
        pen += position.x_advance as f32 * scale + tracking;
    }
    // The tracking after the last glyph is air nobody sees.
    let width = (pen - tracking).max(0.0);
    Line {
        path: builder.finish(),
        width,
    }
}

/// A separable box blur over premultiplied RGBA, run twice for a soft
/// falloff. Radius in pixels; zero leaves the picture alone.
fn blur(pixmap: &mut Pixmap, radius: usize) {
    if radius == 0 {
        return;
    }
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let data = pixmap.data_mut();
    let mut scratch = vec![0u8; data.len()];
    for _ in 0..2 {
        // Horizontal, into scratch.
        for y in 0..height {
            let row = y * width * 4;
            for channel in 0..4 {
                let mut sum: u32 = 0;
                let window = (2 * radius + 1) as u32;
                let at = |x: isize| -> u32 {
                    let x = x.clamp(0, width as isize - 1) as usize;
                    u32::from(data[row + x * 4 + channel])
                };
                for x in -(radius as isize)..=(radius as isize) {
                    sum += at(x);
                }
                for x in 0..width {
                    scratch[row + x * 4 + channel] = (sum / window) as u8;
                    sum += at(x as isize + radius as isize + 1);
                    sum -= at(x as isize - radius as isize);
                }
            }
        }
        // Vertical, back into data.
        for x in 0..width {
            for channel in 0..4 {
                let mut sum: u32 = 0;
                let window = (2 * radius + 1) as u32;
                let at = |y: isize| -> u32 {
                    let y = y.clamp(0, height as isize - 1) as usize;
                    u32::from(scratch[(y * width + x) * 4 + channel])
                };
                for y in -(radius as isize)..=(radius as isize) {
                    sum += at(y);
                }
                for y in 0..height {
                    data[(y * width + x) * 4 + channel] = (sum / window) as u8;
                    sum += at(y as isize + radius as isize + 1);
                    sum -= at(y as isize - radius as isize);
                }
            }
        }
    }
}

/// Paints `style` onto a `width` × `height` transparent canvas, the block
/// anchored on the canvas's centre by its alignment (see [`Align`]), and
/// returns it PNG-encoded with the block's size and where it landed.
pub fn render(
    fonts: &Fonts,
    style: &TitleStyle,
    width: u32,
    height: u32,
) -> Result<Rendered, Error> {
    let (canvas, block) = paint(fonts, style, width, height)?;
    let png = canvas
        .encode_png()
        .map_err(|error| Error::Encode(error.to_string()))?;
    Ok(Rendered {
        png,
        width,
        height,
        block_width: block.0,
        block_height: block.1,
        block_dx: block.2,
        block_dy: block.3,
    })
}

/// [`render`], but the pixels rather than a PNG of them: no encoding, and
/// nothing for a reader to decode again. For a monitor showing a title
/// while its words are being pulled about, where a file per pointer step
/// was the whole of the lag.
pub fn render_frame(
    fonts: &Fonts,
    style: &TitleStyle,
    width: u32,
    height: u32,
) -> Result<RenderedFrame, Error> {
    let (canvas, block) = paint(fonts, style, width, height)?;
    straight_frame(canvas, block)
}

/// An editable layer cache, centred on the same anchor as the document.
/// Unlike a timeline title, this bitmap grows symmetrically to retain off-canvas
/// glyphs, strokes and shadows for later layer movement. Dimensions may exceed
/// the document; the ordinary layer transform maps its centre exactly once.
/// Font size remains the document-relative source value without timeline clamping.
pub fn render_layer_frame(
    fonts: &Fonts,
    style: &TitleStyle,
    width: u32,
    height: u32,
) -> Result<RenderedFrame, Error> {
    let (canvas, block) = paint_with_mode(fonts, style, width, height, true)?;
    straight_frame(canvas, block)
}

fn straight_frame(canvas: Pixmap, block: Block) -> Result<RenderedFrame, Error> {
    let (width, height) = (canvas.width(), canvas.height());
    // tiny-skia keeps premultiplied pixels; a frame carries straight alpha.
    let mut rgba = Vec::with_capacity(canvas.pixels().len() * 4);
    for pixel in canvas.pixels() {
        let straight = pixel.demultiply();
        rgba.extend_from_slice(&[
            straight.red(),
            straight.green(),
            straight.blue(),
            straight.alpha(),
        ]);
    }
    Ok(RenderedFrame {
        rgba,
        width,
        height,
        block_width: block.0,
        block_height: block.1,
        block_dx: block.2,
        block_dy: block.3,
    })
}

/// A painted block: (width, height, dx, dy), on [`Rendered`]'s terms.
type Block = (u32, u32, i32, i32);

/// The canvas with the title on it, and the block.
fn paint(
    fonts: &Fonts,
    style: &TitleStyle,
    width: u32,
    height: u32,
) -> Result<(Pixmap, Block), Error> {
    paint_with_mode(fonts, style, width, height, false)
}

fn paint_with_mode(
    fonts: &Fonts,
    style: &TitleStyle,
    mut width: u32,
    mut height: u32,
    layer_cache: bool,
) -> Result<(Pixmap, Block), Error> {
    let mut canvas = Pixmap::new(width, height).ok_or(Error::Canvas(width, height))?;
    let frame_h = height as f32;
    let relative_size = if layer_cache {
        style.font_size
    } else {
        style.font_size.clamp(0.005, 1.0)
    };
    let em = relative_size as f32 * frame_h;
    let tracking = style.tracking as f32 * frame_h;
    let pitch = em * (style.line_height.max(0.5) as f32);

    let primary = fonts.pick(style)?;
    let mut ids = vec![primary];
    let mut coverage = HashMap::new();
    for c in style.content.chars() {
        let id = fonts.coverage(primary, c)?;
        let index = if let Some(index) = ids.iter().position(|i| *i == id) {
            index
        } else {
            ids.push(id);
            ids.len() - 1
        };
        coverage.insert(c, index);
    }
    let blobs: Vec<_> = ids
        .into_iter()
        .map(|id| fonts.blob(id))
        .collect::<Result<_, _>>()?;
    let faces: Vec<_> = blobs
        .iter()
        .map(|blob| {
            let index = u32::from_le_bytes(blob[..4].try_into().expect("font index"));
            rustybuzz::Face::from_slice(&blob[4..], index).ok_or(Error::BadFont)
        })
        .collect::<Result<_, _>>()?;
    let ascent = faces
        .iter()
        .map(|f| f.ascender() as f32 / f.units_per_em() as f32 * em)
        .fold(0.0, f32::max);
    let descent = faces
        .iter()
        .map(|f| -(f.descender() as f32) / f.units_per_em() as f32 * em)
        .fold(0.0, f32::max);
    let max_w = (style.max_width as f32) * width as f32;
    let lines: Vec<Line> = style
        .content
        .split('\n')
        .flat_map(|line| wrap_line(&faces, &coverage, line, em, tracking, max_w))
        .collect();
    let rows = lines.len().max(1);
    let block_w = lines.iter().map(|line| line.width).fold(0.0, f32::max);
    let block_h = (rows as f32 - 1.0) * pitch + ascent + descent;
    if block_w <= 0.0 || lines.is_empty() {
        // Nothing to paint: an empty, valid canvas.
        return Ok((canvas, (0, 0, 0, 0)));
    }

    // The plate's padding is part of the block: it is what a monitor should
    // outline, and what the title's neighbours should keep clear of.
    let plate = colour(&style.background);
    let (pad_x, pad_y) = if plate.is_some() {
        (em * 0.35, em * 0.2)
    } else {
        (0.0, 0.0)
    };
    let outer_w = block_w + 2.0 * pad_x;
    let outer_h = block_h + 2.0 * pad_y;
    // The anchor is the canvas's centre - where the clip's position lands -
    // and the alignment says which edge of the block sits on it, plate and
    // all. Growing words then push the far edge and leave the anchored one
    // where it is.
    let anchor_x = width as f32 / 2.0;
    let outer_left = match style.align {
        Align::Left => anchor_x,
        Align::Center => anchor_x - outer_w / 2.0,
        Align::Right => anchor_x - outer_w,
    };
    let mut left = outer_left + pad_x;
    let mut top = (frame_h - outer_h) / 2.0 + pad_y;
    let block_dx = (outer_left + outer_w / 2.0 - anchor_x).round() as i32;

    // One path for all the words, placed. Each line is aligned within the
    // block's width and sits on its own baseline.
    let mut words = PathBuilder::new();
    for (row, line) in lines.iter().enumerate() {
        let Some(path) = &line.path else { continue };
        let indent = match style.align {
            Align::Left => 0.0,
            Align::Center => (block_w - line.width) / 2.0,
            Align::Right => block_w - line.width,
        };
        let baseline = top + ascent + row as f32 * pitch;
        let placed = path
            .clone()
            .transform(Transform::from_translate(left + indent, baseline))
            .expect("a translated glyph path stays finite");
        words.push_path(&placed);
    }
    let Some(mut words) = words.finish() else {
        return Ok((
            canvas,
            (outer_w.round() as u32, outer_h.round() as u32, block_dx, 0),
        ));
    };

    if layer_cache {
        let bounds = words.bounds();
        // Two box-blur passes each expand by radius. Include glyph overhangs,
        // the full outline, shadow offset and an antialias guard ring.
        let shadow_pad = if style.shadow {
            2.0 * (em * 0.04).round().max(1.0) + em * 0.07
        } else {
            0.0
        };
        let pad = (style.stroke_width.max(0.0) as f32 * frame_h).max(shadow_pad) + 2.0;
        let mut l = bounds.left();
        let mut r = bounds.right();
        let mut t = bounds.top();
        let mut b = bounds.bottom();
        if plate.is_some() {
            l = l.min(left - pad_x);
            r = r.max(left - pad_x + outer_w);
            t = t.min(top - pad_y);
            b = b.max(top - pad_y + outer_h);
        }
        let grow_x = (pad - l).max(r + pad - width as f32).max(0.0).ceil() as u64;
        let grow_y = (pad - t).max(b + pad - height as f32).max(0.0).ceil() as u64;
        let cache_w = width as u64 + 2 * grow_x;
        let cache_h = height as u64 + 2 * grow_y;
        // Match the canvas bitmap dimension and aggregate RGBA ceilings before allocating.
        if cache_w > 32_768 || cache_h > 32_768 || cache_w * cache_h > 128 * 1024 * 1024 {
            return Err(Error::Canvas(width, height));
        }
        if grow_x > 0 || grow_y > 0 {
            width = cache_w as u32;
            height = cache_h as u32;
            canvas = Pixmap::new(width, height).ok_or(Error::Canvas(width, height))?;
            words = words
                .transform(Transform::from_translate(grow_x as f32, grow_y as f32))
                .ok_or(Error::Canvas(width, height))?;
            left += grow_x as f32;
            top += grow_y as f32;
        }
    }

    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };

    // The plate, first and under everything.
    if let Some(fill) = plate
        && let Some(rect) = Rect::from_xywh(left - pad_x, top - pad_y, outer_w, outer_h)
    {
        paint.set_color(fill);
        let radius = em * 0.15;
        let mut plate_path = PathBuilder::new();
        push_rounded_rect(&mut plate_path, rect, radius);
        if let Some(plate_path) = plate_path.finish() {
            canvas.fill_path(
                &plate_path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    // The shadow: the words again, offset down and right, black, blurred on
    // a layer of their own and laid under the real words.
    if style.shadow
        && let Some(mut layer) = Pixmap::new(width, height)
    {
        let mut shade = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        shade.set_color(Color::from_rgba8(0, 0, 0, 150));
        let offset = Transform::from_translate(em * 0.05, em * 0.07);
        layer.fill_path(&words, &shade, FillRule::Winding, offset, None);
        blur(&mut layer, (em * 0.04).round().max(1.0) as usize);
        canvas.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    // The outline, under the fill so only its outer half shows - which is
    // why it is drawn at twice the asked-for width.
    let stroke_w = style.stroke_width.max(0.0) as f32 * frame_h;
    if stroke_w > 0.0
        && let Some(edge) = colour(&style.stroke_color)
    {
        paint.set_color(edge);
        let stroke = Stroke {
            width: stroke_w * 2.0,
            line_join: LineJoin::Round,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        canvas.stroke_path(&words, &paint, &stroke, Transform::identity(), None);
    }

    // The words.
    paint.set_color(colour(&style.color).unwrap_or(Color::WHITE));
    canvas.fill_path(
        &words,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    Ok((
        canvas,
        (outer_w.round() as u32, outer_h.round() as u32, block_dx, 0),
    ))
}

/// A rectangle with rounded corners, radius clamped to half the short side.
fn push_rounded_rect(builder: &mut PathBuilder, rect: Rect, radius: f32) {
    let r = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    let (l, t, rgt, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    // Quadratic corners: close enough to circular at these sizes, and half
    // the segments of a cubic approximation.
    builder.move_to(l + r, t);
    builder.line_to(rgt - r, t);
    builder.quad_to(rgt, t, rgt, t + r);
    builder.line_to(rgt, b - r);
    builder.quad_to(rgt, b, rgt - r, b);
    builder.line_to(l + r, b);
    builder.quad_to(l, b, l, b - r);
    builder.line_to(l, t + r);
    builder.quad_to(l, t, l + r, t);
    builder.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(content: &str) -> TitleStyle {
        TitleStyle {
            content: content.to_owned(),
            font_family: "\"No Such Family\"".to_owned(),
            font_size: 0.09,
            font_weight: 700.0,
            italic: false,
            color: "#ffffff".to_owned(),
            align: Align::Center,
            stroke_width: 0.0,
            stroke_color: "#000000".to_owned(),
            shadow: true,
            background: String::new(),
            line_height: 1.2,
            tracking: 0.0,
            max_width: 0.0,
        }
    }

    /// A limit narrower than the words wraps them: the block comes out no
    /// wider than the limit and taller than the one-line block.
    #[test]
    fn chinese_boundaries_keep_punctuation_and_latin_words() {
        assert_eq!(
            wrap_units("中文，Hello world。"),
            vec!["中", "文，", "Hello", " ", "world。"]
        );
    }

    #[test]
    fn missing_glyph_is_reported_instead_of_a_tofu_success() {
        let fonts = Fonts::new();
        assert!(matches!(
            render_frame(&fonts, &style("\u{10FFFF}"), 320, 180),
            Err(Error::MissingGlyph('\u{10FFFF}'))
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_chinese_fallback_renders_and_wraps_mixed_text() {
        let fonts = Fonts::new();
        let mut s = style("中文混排 Hello 世界");
        s.font_family = "Helvetica".into();
        let primary = fonts.pick(&s).unwrap();
        assert_ne!(fonts.coverage(primary, '中').unwrap(), primary);
        let wide = render_frame(&fonts, &s, 640, 360).unwrap();
        s.max_width = 0.18;
        let narrow = render_frame(&fonts, &s, 640, 360).unwrap();
        assert!(narrow.block_height > wide.block_height);
        assert!(narrow.rgba.chunks_exact(4).any(|p| p[3] > 0));
        assert_eq!(s.font_family, "Helvetica");
    }

    #[test]
    fn a_width_limit_wraps_words_onto_more_lines() {
        let fonts = Fonts::new();
        let one = render(&fonts, &style("one two three four five six"), 640, 360).expect("renders");
        let mut narrow = style("one two three four five six");
        narrow.max_width = 0.3;
        let wrapped = render(&fonts, &narrow, 640, 360).expect("renders");
        assert!(
            one.block_width > wrapped.block_width,
            "{} vs {}",
            one.block_width,
            wrapped.block_width
        );
        assert!(
            wrapped.block_width <= (0.3 * 640.0) as u32 + 1,
            "{}",
            wrapped.block_width
        );
        assert!(wrapped.block_height > one.block_height);
        // and a word no line can hold still gets a line, uncut
        let mut tiny = style("unbreakable");
        tiny.max_width = 0.01;
        let rendered = render(&fonts, &tiny, 640, 360).expect("renders");
        assert!(rendered.block_width > 7);
    }

    fn opaque_pixels(png: &[u8]) -> usize {
        let pixmap = Pixmap::decode_png(png).expect("our own PNG decodes");
        pixmap.pixels().iter().filter(|p| p.alpha() > 0).count()
    }

    /// The leftmost and rightmost columns holding any paint.
    fn painted_span(png: &[u8]) -> (u32, u32) {
        let pixmap = Pixmap::decode_png(png).expect("our own PNG decodes");
        let width = pixmap.width();
        let mut left = width;
        let mut right = 0;
        for (index, pixel) in pixmap.pixels().iter().enumerate() {
            if pixel.alpha() > 0 {
                let x = index as u32 % width;
                left = left.min(x);
                right = right.max(x);
            }
        }
        (left, right)
    }

    #[test]
    fn colours_parse_both_lengths() {
        assert_eq!(colour("#ff0000"), Some(Color::from_rgba8(255, 0, 0, 255)));
        assert_eq!(colour("#00ff0080"), Some(Color::from_rgba8(0, 255, 0, 128)));
        assert_eq!(colour(""), None);
        assert_eq!(colour("red"), None);
    }

    /// A missing family falls back to a system face and still paints words.
    #[test]
    fn a_title_paints_something_with_a_fallback_face() {
        let fonts = Fonts::new();
        let out = render(&fonts, &style("Hello"), 640, 360).expect("renders");
        assert_eq!((out.width, out.height), (640, 360));
        assert!(out.block_width > 0 && out.block_height > 0);
        assert!(out.block_width < 640);
        assert!(opaque_pixels(&out.png) > 100);
    }

    /// Left-aligned words start at the anchor and run right; right-aligned
    /// ones end there; centred ones straddle it. Growing the words moves
    /// only the far edge.
    #[test]
    fn alignment_anchors_the_block_on_its_edge() {
        let fonts = Fonts::new();
        let (width, height) = (640, 360);
        let centre = width / 2;
        // The shadow reaches a hair past the words to the right and below;
        // this is that hair, generously.
        let slack = 4;

        let mut left = style("Hello");
        left.align = Align::Left;
        left.shadow = false;
        let out = render(&fonts, &left, width, height).expect("renders");
        let (first, _) = painted_span(&out.png);
        assert!(
            first + slack >= centre,
            "left-aligned words start at {first}, left of the anchor"
        );
        assert!(out.block_dx > 0);
        assert!((out.block_dx as u32).abs_diff(out.block_width / 2) <= 1);
        // More words: the left edge stays, the block grows rightwards.
        let mut longer = left.clone();
        longer.content = "Hello there".to_owned();
        let more = render(&fonts, &longer, width, height).expect("renders");
        let (again, _) = painted_span(&more.png);
        assert!(
            again.abs_diff(first) <= 1,
            "the left edge moved from {first} to {again}"
        );
        assert!(more.block_width > out.block_width);

        let mut right = style("Hello");
        right.align = Align::Right;
        right.shadow = false;
        let out = render(&fonts, &right, width, height).expect("renders");
        let (_, last) = painted_span(&out.png);
        assert!(
            last <= centre + slack,
            "right-aligned words end at {last}, right of the anchor"
        );
        assert!(out.block_dx < 0);

        let out = render(&fonts, &style("Hello"), width, height).expect("renders");
        let (first, last) = painted_span(&out.png);
        assert!(first < centre && last > centre);
        assert_eq!(out.block_dx, 0);
    }

    /// The canvas is the frame; the block is not.
    #[test]
    fn empty_content_is_an_empty_canvas() {
        let fonts = Fonts::new();
        let out = render(&fonts, &style(""), 320, 180).expect("renders");
        assert_eq!((out.block_width, out.block_height), (0, 0));
        assert_eq!(opaque_pixels(&out.png), 0);
    }

    /// More lines, taller block; a plate grows it further.
    #[test]
    fn lines_and_plates_grow_the_block() {
        let fonts = Fonts::new();
        let one = render(&fonts, &style("One"), 640, 360).expect("renders");
        let two = render(&fonts, &style("One\nTwo"), 640, 360).expect("renders");
        assert!(two.block_height > one.block_height);
        let mut plated = style("One");
        plated.background = "#000000cc".to_owned();
        let plated = render(&fonts, &plated, 640, 360).expect("renders");
        assert!(plated.block_width > one.block_width);
        assert!(plated.block_height > one.block_height);
    }
    #[test]
    fn timeline_title_keeps_fixed_frame_anchor_and_legacy_size_clamp() {
        let fonts = Fonts::new();
        let mut s = style("Title");
        s.font_size = 8.0 / 2160.0;
        for align in [Align::Left, Align::Center, Align::Right] {
            s.align = align;
            let tiny = render_frame(&fonts, &s, 640, 2160).unwrap();
            let mut minimum = s.clone();
            minimum.font_size = 0.005;
            let expected = render_frame(&fonts, &minimum, 640, 2160).unwrap();
            assert_eq!(tiny.rgba, expected.rgba);
            assert_eq!((tiny.width, tiny.height), (640, 2160));
            assert_eq!(
                tiny.block_dx.signum(),
                match align {
                    Align::Left => 1,
                    Align::Center => 0,
                    Align::Right => -1,
                }
            );
        }
    }
}
