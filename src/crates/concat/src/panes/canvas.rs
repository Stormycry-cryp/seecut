// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The canvas pane: the image editor's document in a seat of its own.
//!
//! The pane holds the ported document - [`concat_canvas`]'s layer tree over
//! a pixel store - and the [`Navigator`] that owns its view. Everything the
//! pane's gestures report goes straight into the navigator; everything it
//! shows is published back out of it. The pane itself decides nothing,
//! which is the point: the same view state drives a wheel on macOS, a
//! ctrl-wheel on Windows, and whatever input a pen or a trackpad becomes by
//! the time Slint reports it.
//!
//! Composition runs on the window's own device, the one the renderer draws
//! with and the monitor composites on. A composed canvas is therefore a
//! texture the renderer samples directly - nothing is read back - and the
//! resident-texture cache underneath it means a pan, a zoom or a brush
//! stroke re-composites without re-uploading a single layer.
//!
//! Editing tools arrive with the next phases of the port; the pane is the
//! stage they will act on, and the view they will not have to think about.

use std::path::Path;

use concat_canvas::{
    CanvasGpu, CanvasViewport, ImageDocument, NavInput, Navigator, PixelId, PixelStore,
};
use concat_core::frame::Frame;
use slint::SharedPixelBuffer;

use crate::i18n::tf;
use crate::studio::Studio;

/// Everything that can happen to the canvas.
#[derive(Debug)]
pub enum CanvasMsg {
    /// The picker came back with files; the first image is the one opened.
    Picked(Vec<std::path::PathBuf>),
    /// The viewport's box changed, in viewport pixels.
    Resized(f64, f64),
    /// A scroll: pixel deltas, whether the modifier makes it a zoom, and
    /// where over the viewport it happened.
    Scroll {
        dx: f64,
        dy: f64,
        zoom_modifier: bool,
        x: f64,
        y: f64,
    },
    /// A hand-tool or middle-button drag went down, moved, ended.
    PanPress(f64, f64),
    PanMove(f64, f64),
    /// A zoom-tool drag went down (with the option state), moved, ended.
    ZoomPress {
        x: f64,
        y: f64,
        alt: bool,
    },
    ZoomMove(f64, f64),
    /// The pointer went up, whichever drag was live; `alt` rides for the
    /// zoom tool's click-step.
    Release(bool),
    /// Fit the document to the pane.
    Fit,
    /// The tray's tool picker: 0 move, 1 hand, 2 zoom.
    Tool(i32),
}

/// The canvas pane's state.
pub struct CanvasPane {
    /// The composed document, as the pane shows it.
    pub image: slint::Image,
    /// The view, gestures and all.
    pub nav: Navigator,
    /// The open document, or none while the pane is empty.
    pub document: Option<ImageDocument>,
    /// The document's pixels, keyed by identity.
    pub store: PixelStore,
    /// The base layer's pixels, while a document is open.
    pub layer: Option<PixelId>,
    /// The tray's tool: 0 move, 1 hand, 2 zoom.
    pub tool: usize,
    /// The view's zoom, in percent, read out of the navigator.
    pub zoom: f64,
    /// The view's offset from centre, in viewport pixels.
    pub pan: (f64, f64),
    /// The document's box on screen, in viewport pixels.
    pub stage: (f64, f64),
    /// The open file's name, as the header readout.
    pub name: String,
    /// The compositor, built on the window's device the first time the
    /// pane has something to compose. `None` without a device, and then
    /// every frame goes through the CPU.
    gpu: Option<CanvasGpu>,
    /// Said once: a canvas that cannot compose says so, and then stops
    /// repeating itself.
    failed: bool,
}

impl Default for CanvasPane {
    fn default() -> Self {
        Self {
            image: slint::Image::default(),
            nav: Navigator::new(CanvasViewport::default()),
            document: None,
            store: PixelStore::new(),
            layer: None,
            tool: 0,
            zoom: 100.0,
            pan: (0.0, 0.0),
            stage: (0.0, 0.0),
            name: String::new(),
            gpu: None,
            failed: false,
        }
    }
}

impl CanvasPane {
    /// Applies one message. The studio is the rest of the window; the host
    /// inside it is where the shared device lives. Everything runs on the
    /// event-loop thread, which is the one thread the window's renderer
    /// submits from - the same rule the monitor's drawing keeps.
    pub fn update(&mut self, msg: CanvasMsg, studio: &mut Studio) {
        match msg {
            CanvasMsg::Picked(paths) => {
                if let Some(path) = paths.first() {
                    self.open(path, studio);
                }
            }
            CanvasMsg::Resized(width, height) => {
                let document = self.document_size();
                self.nav
                    .viewport_mut()
                    .resize((width.max(1.0), height.max(1.0)), 1.0, document);
                self.sync_view();
            }
            CanvasMsg::Scroll {
                dx,
                dy,
                zoom_modifier,
                x,
                y,
            } => {
                // Slint reports scroll deltas in pixels, for a wheel notch
                // and a trackpad's fingers alike, so every scroll is the
                // precise kind by the time it gets here.
                self.nav.apply(NavInput::Scroll {
                    delta: (dx, dy),
                    precise: true,
                    zoom_modifier,
                    pointer: (x, y),
                });
                self.render(studio);
            }
            CanvasMsg::PanPress(x, y) => {
                self.nav.apply(NavInput::PanPress((x, y)));
            }
            CanvasMsg::PanMove(x, y) => {
                self.nav.apply(NavInput::PanMove((x, y)));
                self.sync_view();
            }
            CanvasMsg::ZoomPress { x, y, alt } => {
                self.nav.apply(NavInput::ZoomPress {
                    pointer: (x, y),
                    option: alt,
                });
            }
            CanvasMsg::ZoomMove(x, y) => {
                self.nav.apply(NavInput::ZoomMove {
                    pointer: (x, y),
                    option: false,
                });
                self.render(studio);
            }
            CanvasMsg::Release(option) => {
                self.nav.apply(NavInput::Release { option });
                self.render(studio);
            }
            CanvasMsg::Fit => {
                self.nav.apply(NavInput::Fit);
                self.render(studio);
            }
            CanvasMsg::Tool(tool) => self.set_tool(tool),
        }
    }

    /// The tray's tool, clamped into it: a picker can only offer what the
    /// tray has, but the number arrives over a boundary.
    pub fn set_tool(&mut self, tool: i32) {
        self.tool = (tool.max(0) as usize).min(2);
    }

    /// The document's size, as the navigator wants it: `None` while the
    /// pane is empty, so the guards ignore navigation there.
    fn document_size(&self) -> Option<(f64, f64)> {
        self.document
            .as_ref()
            .map(|d| (f64::from(d.width), f64::from(d.height)))
    }

    /// Reads the view out of the navigator into the published fields. The
    /// zoom readout and the stage box follow the navigator; the pane
    /// carries no view state of its own.
    fn sync_view(&mut self) {
        self.zoom = self.nav.viewport().zoom() * 100.0;
        self.pan = self.nav.viewport().pan();
        self.stage = self
            .document_size()
            .map(|(width, height)| {
                let zoom = self.nav.viewport().zoom();
                (width * zoom, height * zoom)
            })
            .unwrap_or((0.0, 0.0));
    }

    /// Opens an image as a one-layer document: decoded straight-alpha, one
    /// layer the size of the canvas, the view fitted to it.
    fn open(&mut self, path: &Path, studio: &mut Studio) {
        match decode(path) {
            Ok(frame) => {
                let (width, height) = (frame.width(), frame.height());
                let mut document = ImageDocument::new(width, height);
                let pixels = self.store.put(frame);
                document.new_layer(
                    path.file_stem().unwrap_or_default().to_string_lossy(),
                    pixels,
                );
                self.document = Some(document);
                self.layer = Some(pixels);
                self.name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.failed = false;
                // The view starts over: fitted to what was just opened, in
                // whatever box the pane has.
                let size = (f64::from(width), f64::from(height));
                self.nav.set_document(Some(size));
                self.nav.viewport_mut().fit(size);
                self.render(studio);
            }
            Err(error) => {
                log::warn!("canvas: {error}");
                if !self.failed {
                    self.failed = true;
                    studio.notify(&tf("Could not open {0}", &[&error]), true);
                }
            }
        }
    }

    /// Composites the document and publishes the picture. The GPU path
    /// leaves the result as a texture the renderer samples; the CPU path
    /// hands Slint the pixels. Both run on this thread.
    pub fn render(&mut self, studio: &mut Studio) {
        self.sync_view();
        let Some(document) = self.document.clone() else {
            return;
        };
        if self.gpu.is_none() {
            // The device is taken, not cloned: once the pane owns the
            // compositor it owns the only handle it needs. The monitor
            // keeps its own, and wgpu handles are cheap shells over one
            // device.
            self.gpu = studio
                .host
                .gpu_device
                .take()
                .zip(studio.host.gpu_queue.take())
                .map(|(device, queue)| CanvasGpu::with_device(device, queue));
        }
        let picture = match &mut self.gpu {
            Some(gpu) => Some(
                slint::Image::try_from(gpu.compose_texture(&document, &self.store))
                    .map_err(|error| format!("canvas texture: {error}")),
            ),
            None => None,
        };
        let picture = match picture {
            Some(picture) => picture,
            None => Ok(cpu_picture(&document, &self.store)),
        };
        match picture {
            Ok(image) => self.image = image,
            Err(error) => {
                log::warn!("canvas: {error}");
                if !self.failed {
                    self.failed = true;
                    studio.notify(&tf("Canvas failed: {0}", &[&error]), true);
                }
            }
        }
    }
}

/// Composites on the CPU and hands Slint the pixels - the path when the
/// machine offered no adapter, and the canvas still has to work.
fn cpu_picture(document: &ImageDocument, store: &PixelStore) -> slint::Image {
    let frame = concat_canvas::compose(document, store);
    let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        frame.pixels(),
        frame.width(),
        frame.height(),
    );
    slint::Image::from_rgba8(buffer)
}

/// Decodes an image file into straight-alpha RGBA. PNG through the `png`
/// crate, which the window already keeps; everything else through `image`,
/// whose feature set here covers the JPEG the artwork cache wanted - a
/// format outside that set is the error, not a panic.
fn decode(path: &Path) -> Result<Frame, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path
        .extension()
        .map(|e| e.to_ascii_lowercase().to_string_lossy().into_owned())
        .unwrap_or_default();
    let (width, height, pixels) = match name.as_str() {
        "png" => decode_png(&bytes)?,
        _ => {
            let image = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let (width, height) = image.dimensions();
            (width, height, image.into_raw())
        }
    };
    Frame::from_rgba(width, height, pixels).ok_or_else(|| "empty image".to_owned())
}

/// PNG to RGBA, any of the colour types a still is likely to come in.
/// Returns `(width, height, rgba)`.
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let plain = &buffer[..info.buffer_size()];
    let pixels = match info.color_type {
        png::ColorType::Rgba => plain.to_vec(),
        png::ColorType::Rgb => expand(plain, 3),
        png::ColorType::Grayscale => expand(plain, 1),
        png::ColorType::GrayscaleAlpha => expand(plain, 2),
        other => return Err(format!("png: unsupported colour type {other:?}")),
    };
    Ok((info.width, info.height, pixels))
}

/// Greys and RGBs to RGBA, one output pixel per `channels` input bytes. The
/// alpha comes from the second channel of a grey+alpha pair and is opaque
/// otherwise.
fn expand(bytes: &[u8], channels: usize) -> Vec<u8> {
    let alpha_at = match channels {
        2 => Some(1),
        4 => Some(3),
        _ => None,
    };
    let mut out = Vec::with_capacity(bytes.len() / channels * 4);
    for pixel in bytes.chunks_exact(channels) {
        let (r, g, b) = match channels {
            1 | 2 => (pixel[0], pixel[0], pixel[0]),
            _ => (pixel[0], pixel[1], pixel[2]),
        };
        let a = alpha_at.map(|at| pixel[at]).unwrap_or(255);
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_stays_in_the_tray() {
        let mut pane = CanvasPane::default();
        pane.set_tool(2);
        assert_eq!(pane.tool, 2);
        pane.set_tool(9);
        assert_eq!(pane.tool, 2, "a tool past the tray is the last one");
        pane.set_tool(-4);
        assert_eq!(pane.tool, 0, "a tool before the tray is the first one");
    }

    #[test]
    fn an_opened_document_is_one_layer_the_size_of_the_canvas() {
        let mut pane = CanvasPane::default();
        let frame = Frame::from_rgba(4, 3, vec![128; 4 * 3 * 4]).expect("frame");
        let mut document = ImageDocument::new(frame.width(), frame.height());
        let pixels = pane.store.put(frame);
        document.new_layer("probe", pixels);
        pane.document = Some(document);
        pane.layer = Some(pixels);

        assert_eq!(pane.document_size(), Some((4.0, 3.0)));
        assert_eq!(pane.store.get(pixels).map(|f| f.pixels().len()), Some(48));
    }
}
