// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// its one Metal compute brush, rebuilt on Concat's shared wgpu stack.

//! The GPU brush: the WGSL twin of [`crate::brush::BrushStroke`]'s
//! integration and preview.
//!
//! Same path state machine, same density integral, same tail semantics -
//! only where the per-pixel work runs changes. The path (samples to
//! settled pieces and the provisional tail) stays on the CPU in
//! [`crate::brush::PathState`]; two compute passes replace the CPU tile
//! walk:
//!
//! - **integrate** folds this update's settled pieces into the permanent
//!   density buffer (soft tips: optical density accumulates and caps;
//!   hard tips: coverage is the max so far), over the settled pieces'
//!   tiles only, exactly where the CPU walk integrates.
//! - **preview** rewrites the 8-bit coverage over every tile this update
//!   touched - settled, current tail and the *previous* tail's tiles, so
//!   the abandoned tail leaves no ghost. The tail is added at preview
//!   time and never stored, the same rule the CPU lives by.
//!
//! The parity test runs the CPU and GPU strokes in lockstep and demands
//! the two coverage previews agree within a byte, the same bar the
//! compositor's parity holds. Floating-point rounding differs between
//! backends (WGSL may fuse multiply-adds); the byte bound absorbs exactly
//! that, and nothing looser.
//!
//! One deliberate difference from the CPU tiles: the density plane is one
//! full-layer `f32` buffer instead of a tile map. A stroke touches far
//! more area than a tile map's live-set heuristic tracks, and the GPU
//! wants dense writes; the tile *reporting* (dirty rectangles, per-tile
//! coverage) stays identical, which is what the upload path consumes.

use crate::brush::{BrushError, BrushSettings, PathState, TILE_SIZE, segment_keys};

/// The compute shader. One pass per entry point; `p.soft` picks the tip
/// behaviour inside both. The formulas are the CPU's `segment_density`,
/// `coverage_at` and the preview quantization, line for line.
const SHADER: &str = r#"
struct Params {
    layer_w: u32,
    layer_h: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
    seg_count: u32,
    tail_count: u32,
    radius: f32,
    hardness: f32,
    antialias: f32,
    spacing: f32,
    soft: u32,
    cap: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> density: array<f32>;
@group(0) @binding(2) var<storage, read> segs: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> coverage: array<u32>;

const NODES: array<f32, 4> = array<f32, 4>(
    0.18343465, 0.5255324, 0.79666644, 0.96028984);
const WEIGHTS: array<f32, 4> = array<f32, 4>(
    0.36268378, 0.31370666, 0.22238103, 0.10122854);

// Tip coverage at a squared distance: the CPU's `coverage_at`.
fn coverage_at(d2: f32) -> f32 {
    let distance = sqrt(d2);
    if p.hardness >= 1.0 {
        return (p.radius - distance) / p.antialias + 0.5;
    }
    let t = clamp((distance / p.radius - p.hardness) / (1.0 - p.hardness), 0.0, 1.0);
    let k = 2.5;
    let floor = exp(-k);
    return max((exp(-k * t * t) - floor) / (1.0 - floor), 0.0);
}

// The -log of the transparency one dab leaves: the CPU's `tip_density`.
fn tip_density(d2: f32) -> f32 {
    return -log(max(1.0 - coverage_at(d2), 0.001));
}

// Squared distance from the pixel to a segment: the CPU's
// `segment_distance_squared`.
fn seg_dist2(px: f32, py: f32, s: vec4<f32>) -> f32 {
    let vx = s.z - s.x;
    let vy = s.w - s.y;
    let t = clamp(
        ((px - s.x) * vx + (py - s.y) * vy) / max(vx * vx + vy * vy, 1e-12),
        0.0, 1.0);
    let dx = px - (s.x + t * vx);
    let dy = py - (s.y + t * vy);
    return dx * dx + dy * dy;
}

// The density one sweeping segment deposits on a pixel, eight-point
// Gauss-Legendre clipped to the tip's support: the CPU's
// `segment_density`, including its degenerate zero-length branch.
fn seg_density(px: f32, py: f32, s: vec4<f32>) -> f32 {
    let vx = s.z - s.x;
    let vy = s.w - s.y;
    let seg_len = sqrt(vx * vx + vy * vy);
    if seg_len < 1e-6 {
        return tip_density((px - s.x) * (px - s.x) + (py - s.y) * (py - s.y));
    }
    let dx = px - s.x;
    let dy = py - s.y;
    let ux = vx / seg_len;
    let uy = vy / seg_len;
    let projection = dx * ux + dy * uy;
    let perp_x = dx - projection * ux;
    let perp_y = dy - projection * uy;
    let perp2 = perp_x * perp_x + perp_y * perp_y;
    let r2 = p.radius * p.radius;
    if perp2 >= r2 {
        return 0.0;
    }
    let reach = sqrt(r2 - perp2);
    let lo = max(projection - reach, 0.0);
    let hi = min(projection + reach, seg_len);
    if hi <= lo {
        return 0.0;
    }
    let midpoint = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    var integral = 0.0;
    for (var i = 0u; i < 4u; i++) {
        let a = midpoint - half * NODES[i] - projection;
        let b = midpoint + half * NODES[i] - projection;
        integral += WEIGHTS[i]
            * (tip_density(perp2 + a * a) + tip_density(perp2 + b * b));
    }
    return integral * half / p.spacing;
}

// Settled pieces fold into the permanent buffer; the tail never appears
// here. Soft: density accumulates and caps. Hard: coverage is the max.
@compute @workgroup_size(16, 16)
fn integrate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.rect_w || gid.y >= p.rect_h {
        return;
    }
    let x = p.rect_x + gid.x;
    let y = p.rect_y + gid.y;
    let index = y * p.layer_w + x;
    let px = f32(x) + 0.5;
    let py = f32(y) + 0.5;
    if p.soft == 1u {
        var d = density[index];
        for (var i = 0u; i < p.seg_count; i++) {
            d = d + seg_density(px, py, segs[i]);
        }
        density[index] = min(d, p.cap);
    } else {
        var v = density[index];
        for (var i = 0u; i < p.seg_count; i++) {
            v = max(v, coverage_at(seg_dist2(px, py, segs[i])));
        }
        density[index] = v;
    }
}

// The 8-bit preview over every tile this update touched: permanent plus
// the current tail, tail never stored. The same quantization the CPU's
// `tile_coverage` applies.
@compute @workgroup_size(16, 16)
fn preview(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.rect_w || gid.y >= p.rect_h {
        return;
    }
    let x = p.rect_x + gid.x;
    let y = p.rect_y + gid.y;
    let index = y * p.layer_w + x;
    let px = f32(x) + 0.5;
    let py = f32(y) + 0.5;
    var value: f32;
    if p.soft == 1u {
        var tail = 0.0;
        for (var i = p.seg_count; i < p.seg_count + p.tail_count; i++) {
            tail = tail + seg_density(px, py, segs[i]);
        }
        value = 1.0 - exp(-min(density[index] + tail, p.cap));
    } else {
        var tail = 0.0;
        for (var i = p.seg_count; i < p.seg_count + p.tail_count; i++) {
            tail = max(tail, coverage_at(seg_dist2(px, py, segs[i])));
        }
        value = max(density[index], tail);
    }
    if p.soft == 3u {
        // Debug channel: raw seg_density bits for the first segment.
        coverage[index] = bitcast<u32>(seg_density(px, py, segs[0]));
        return;
    }
    coverage[index] = u32(round(clamp(value, 0.0, 1.0) * 255.0));
}
"#;

/// One update's parameters, 16 words for the uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    layer_w: u32,
    layer_h: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
    seg_count: u32,
    tail_count: u32,
    radius: f32,
    hardness: f32,
    antialias: f32,
    spacing: f32,
    soft: u32,
    cap: f32,
    pad0: u32,
    pad1: u32,
}

impl Params {
    fn bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut word = |index: usize, bytes: [u8; 4]| {
            out[index * 4..(index + 1) * 4].copy_from_slice(&bytes);
        };
        word(0, self.layer_w.to_ne_bytes());
        word(1, self.layer_h.to_ne_bytes());
        word(2, self.rect_x.to_ne_bytes());
        word(3, self.rect_y.to_ne_bytes());
        word(4, self.rect_w.to_ne_bytes());
        word(5, self.rect_h.to_ne_bytes());
        word(6, self.seg_count.to_ne_bytes());
        word(7, self.tail_count.to_ne_bytes());
        for (index, value) in [self.radius, self.hardness, self.antialias, self.spacing]
            .into_iter()
            .enumerate()
        {
            word(8 + index, value.to_ne_bytes());
        }
        word(12, self.soft.to_ne_bytes());
        word(13, self.cap.to_ne_bytes());
        out
    }
}

/// The area one pass covers, in layer pixels: origin then extent.
#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    fn empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// The bounding rectangle of a tile-key list, clamped to the layer.
fn rect_of_keys(keys: &[usize], columns: usize, width: u32, height: u32) -> Option<Rect> {
    let (mut tx0, mut ty0) = (usize::MAX, usize::MAX);
    let (mut tx1, mut ty1) = (0usize, 0usize);
    for &key in keys {
        let (tx, ty) = (key % columns, key / columns);
        tx0 = tx0.min(tx);
        ty0 = ty0.min(ty);
        tx1 = tx1.max(tx);
        ty1 = ty1.max(ty);
    }
    if keys.is_empty() {
        return None;
    }
    let x = (tx0 * TILE_SIZE) as u32;
    let y = (ty0 * TILE_SIZE) as u32;
    let w = ((tx1 - tx0 + 1) * TILE_SIZE) as u32;
    let h = ((ty1 - ty0 + 1) * TILE_SIZE) as u32;
    Some(Rect {
        x,
        y,
        w: w.min(width.saturating_sub(x)),
        h: h.min(height.saturating_sub(y)),
    })
}

/// A GPU stroke in flight over one layer, the WGSL twin of
/// [`crate::brush::BrushStroke`]. Same API, same tile reports; the
/// integration and preview run in two compute passes.
pub struct BrushStrokeGpu {
    width: u32,
    height: u32,
    settings: BrushSettings,
    spacing: f32,
    antialias: f32,
    path: PathState,
    /// Tiles the tail touched on the last update, to clear its preview.
    tail_keys: Vec<usize>,
    /// Tiles the stroke has ever touched - the CPU's live tile set, so
    /// `tile_coverage` answers `None` for tiles the stroke never saw.
    painted: std::collections::HashSet<usize>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline_integrate: wgpu::ComputePipeline,
    pipeline_preview: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    /// The whole-layer optical density (soft) or coverage (hard).
    density: wgpu::Buffer,
    /// The whole-layer 8-bit preview, one byte per pixel in a u32 slot.
    coverage: wgpu::Buffer,
    segments: wgpu::Buffer,
    segments_capacity: usize,
    staging: wgpu::Buffer,
    /// The preview plane as read back after the last update.
    plane: Vec<u8>,
}

/// A GPU layer needs this many pixels at most: two whole-layer buffers of
/// 4 bytes each stay inside wgpu's default 128 MiB storage binding limit.
const GPU_LAYER_PIXEL_LIMIT: u64 = 32 * 1024 * 1024;

impl BrushStrokeGpu {
    /// A stroke over a `width` x `height` layer-pixel canvas, on its own
    /// device. `Ok(None)` when no adapter is available - callers fall back
    /// to the CPU stroke.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(
        width: u32,
        height: u32,
        settings: BrushSettings,
    ) -> Result<Option<Self>, BrushError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok();
        let Some(adapter) = adapter else {
            return Ok(None);
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("concat brush"),
            ..Default::default()
        }))
        .map_err(|_| BrushError::TooLarge)?;
        Ok(Some(Self::with_device(
            device, queue, width, height, settings,
        )?))
    }

    /// A stroke on a device the caller owns - the window's, so the stroke's
    /// coverage can join the composite without a device hop.
    pub fn with_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
        settings: BrushSettings,
    ) -> Result<Self, BrushError> {
        settings.validate()?;
        if width == 0 || height == 0 || width > 30_000 || height > 30_000 {
            return Err(BrushError::TooLarge);
        }
        if width as u64 * height as u64 > GPU_LAYER_PIXEL_LIMIT {
            return Err(BrushError::TooLarge);
        }
        let pixels = (width * height) as usize;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("concat brush"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("concat brush"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("concat brush"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline_integrate = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("concat brush integrate"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("integrate"),
            compilation_options: Default::default(),
            cache: None,
        });
        let pipeline_preview = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("concat brush preview"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("preview"),
            compilation_options: Default::default(),
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush params"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // wgpu zero-initializes buffers: a fresh stroke paints nothing.
        let density = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush density"),
            size: (pixels * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let coverage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush coverage"),
            size: (pixels * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush staging"),
            size: (pixels * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let segments_capacity = 1024usize;
        let segments = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush segments"),
            size: (segments_capacity * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("concat brush"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: density.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: segments.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: coverage.as_entire_binding(),
                },
            ],
        });
        let spacing =
            (settings.diameter * crate::brush::spacing_fraction(settings.hardness)) as f32;
        Ok(Self {
            width,
            height,
            settings,
            spacing: spacing.max(0.25),
            antialias: 1.0,
            path: PathState::new(),
            tail_keys: Vec::new(),
            painted: std::collections::HashSet::new(),
            device,
            queue,
            pipeline_integrate,
            pipeline_preview,
            bind_group,
            uniform,
            density,
            coverage,
            segments,
            segments_capacity,
            staging,
            plane: vec![0; pixels],
        })
    }

    /// The settings the stroke was built with.
    pub fn settings(&self) -> &BrushSettings {
        &self.settings
    }

    /// A pointer sample in layer pixels; returns the tiles whose preview
    /// changed, exactly the CPU stroke's report.
    pub fn append(&mut self, point: (f64, f64)) -> Vec<(usize, usize)> {
        if !self.path.append(point) {
            return Vec::new();
        }
        self.update()
    }

    /// Replaces the provisional tail with the stroke's final curve piece.
    /// Safe to call repeatedly.
    pub fn flush(&mut self) -> Vec<(usize, usize)> {
        self.path.flush();
        self.update()
    }

    /// The 8-bit coverage preview of a tile, read back from the GPU.
    /// None for tiles the stroke never touched.
    pub fn tile_coverage(&self, tx: usize, ty: usize) -> Option<Vec<u8>> {
        let key = ty * self.columns() + tx;
        if !self.painted.contains(&key) {
            return None;
        }
        let tw = (TILE_SIZE as u32).min(self.width - tx as u32 * TILE_SIZE as u32) as usize;
        let th = (TILE_SIZE as u32).min(self.height - ty as u32 * TILE_SIZE as u32) as usize;
        let mut out = Vec::with_capacity(tw * th);
        for row in 0..th {
            let start = (ty * TILE_SIZE + row) * self.width as usize + tx * TILE_SIZE;
            out.extend_from_slice(&self.plane[start..start + tw]);
        }
        Some(out)
    }

    /// Composites the stroke onto a layer's pixels, the CPU stroke's
    /// source-over walk over the GPU's coverage.
    pub fn composite(&self, base: &mut [u8]) {
        let mut tiles: Vec<(usize, usize)> = self
            .painted
            .iter()
            .map(|&k| (k % self.columns(), k / self.columns()))
            .collect();
        tiles.sort_unstable();
        let lookup = |tx: usize, ty: usize| self.tile_coverage(tx, ty);
        crate::brush::composite_coverage(self.width, &self.settings, &tiles, &lookup, base);
    }

    /// Tiles with any permanent or tail coverage, as (tx, ty).
    pub fn touched_tiles(&self) -> Vec<(usize, usize)> {
        let mut keys: Vec<usize> = self.tail_keys.to_vec();
        keys.extend(self.painted.iter().copied());
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter()
            .map(|key| (key % self.columns(), key / self.columns()))
            .collect()
    }

    fn columns(&self) -> usize {
        (self.width as usize).div_ceil(TILE_SIZE)
    }

    /// Runs this update's integrate and preview passes and reads the
    /// coverage plane back. The geometry (which tiles changed) is decided
    /// on the CPU, shared with the reference stroke; the GPU does the
    /// per-pixel work.
    fn update(&mut self) -> Vec<(usize, usize)> {
        let columns = self.columns();
        let mut changed = self.tail_keys.clone();
        let settled_keys = segment_keys(
            self.width,
            self.height,
            columns,
            self.settings.diameter,
            self.path.settled(),
        );
        let tail_keys = segment_keys(
            self.width,
            self.height,
            columns,
            self.settings.diameter,
            self.path.tail(),
        );
        let mut preview_keys = settled_keys.clone();
        preview_keys.extend(tail_keys.iter().copied());
        preview_keys.extend(self.tail_keys.iter().copied());
        self.tail_keys = tail_keys;
        self.painted.extend(preview_keys.iter().copied());

        let settled: Vec<[f32; 4]> = self.path.take_settled();
        let tail: Vec<[f32; 4]> = self.path.tail().to_vec();
        let integrate_rect = rect_of_keys(&settled_keys, columns, self.width, self.height);
        let preview_rect = rect_of_keys(&preview_keys, columns, self.width, self.height);

        if let Some(rect) = preview_rect.filter(|r| !r.empty()) {
            let mut segs = settled.clone();
            segs.extend(tail.iter().copied());
            self.ensure_segments(segs.len());

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            if !segs.is_empty() {
                let bytes: Vec<u8> = segs
                    .iter()
                    .flat_map(|s| s.iter().flat_map(|v| v.to_ne_bytes()))
                    .collect();
                self.queue.write_buffer(&self.segments, 0, &bytes);
            }
            // Settled pieces fold into the permanent buffer first.
            if let Some(rect) = integrate_rect.filter(|r| !r.empty()) {
                self.queue.write_buffer(
                    &self.uniform,
                    0,
                    &self.params(rect, settled.len(), 0).bytes(),
                );
                self.dispatch(&mut encoder, &self.pipeline_integrate, &rect);
            }
            // Then the preview rewrites every tile this update touched,
            // including the tiles the abandoned tail covered.
            self.queue.write_buffer(
                &self.uniform,
                0,
                &self.params(rect, settled.len(), tail.len()).bytes(),
            );
            self.dispatch(&mut encoder, &self.pipeline_preview, &rect);
            encoder.copy_buffer_to_buffer(
                &self.coverage,
                0,
                &self.staging,
                0,
                self.plane_bytes() as u64,
            );
            self.queue.submit([encoder.finish()]);
            self.read_plane();
        }

        changed.extend(preview_keys);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .map(|key| (key % columns, key / columns))
            .collect()
    }

    fn params(&self, rect: Rect, seg_count: usize, tail_count: usize) -> Params {
        Params {
            layer_w: self.width,
            layer_h: self.height,
            rect_x: rect.x,
            rect_y: rect.y,
            rect_w: rect.w,
            rect_h: rect.h,
            seg_count: seg_count as u32,
            tail_count: tail_count as u32,
            radius: (self.settings.diameter / 2.0) as f32,
            hardness: self.settings.hardness as f32,
            antialias: self.antialias,
            spacing: self.spacing,
            soft: u32::from(self.settings.hardness < 1.0),
            cap: crate::brush::DENSITY_CAP,
            pad0: 0,
            pad1: 0,
        }
    }

    fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        rect: &Rect,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("concat brush"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.dispatch_workgroups(rect.w.div_ceil(16), rect.h.div_ceil(16), 1);
    }

    /// Grows the segment buffer when a longer piece list needs it. Old
    /// contents need no copying: every update rewrites the whole list.
    fn ensure_segments(&mut self, count: usize) {
        if count <= self.segments_capacity {
            return;
        }
        self.segments_capacity = count.next_power_of_two();
        self.segments = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat brush segments"),
            size: (self.segments_capacity * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("concat brush"),
            layout: &self.pipeline_integrate.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.density.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.segments.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.coverage.as_entire_binding(),
                },
            ],
        });
    }

    /// Copies the coverage buffer out and stores it as the preview plane.
    /// The synchronous wait is the parity path's price; the tile-pipeline
    /// stage of the port moves this to async residency.
    fn read_plane(&mut self) {
        let slice = self.staging.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        if self
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
            return;
        }
        if !matches!(receiver.try_recv(), Ok(Ok(()))) {
            return;
        }
        {
            let data = slice.get_mapped_range();
            // The shader writes one u32 slot per pixel holding 0..255; the
            // plane keeps just the byte.
            for (word, byte) in data.chunks_exact(4).zip(self.plane.iter_mut()) {
                *byte = u32::from_ne_bytes([word[0], word[1], word[2], word[3]]) as u8;
            }
        }
        self.staging.unmap();
    }

    /// The byte size of the whole-layer coverage plane, one u32 per pixel.
    fn plane_bytes(&self) -> usize {
        self.plane.len() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushStroke;

    fn soft() -> BrushSettings {
        BrushSettings {
            diameter: 60.0,
            hardness: 0.0,
            opacity: 1.0,
            color: [255, 0, 0],
            erasing: false,
        }
    }

    /// One lockstep move: a pointer sample, or lifting the brush.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Append(f64, f64),
        Flush,
    }

    /// The CPU reference and the GPU stroke over the same path.
    struct Pair {
        cpu: BrushStroke,
        gpu: BrushStrokeGpu,
    }

    impl Pair {
        /// `None` when no adapter is available in this environment - the
        /// same skip the compositor's parity tests take.
        fn new(width: u32, height: u32, settings: BrushSettings) -> Option<Self> {
            let cpu = BrushStroke::new(width, height, settings).ok()?;
            let gpu = match BrushStrokeGpu::new(width, height, settings) {
                Ok(Some(gpu)) => gpu,
                _ => return None,
            };
            Some(Self { cpu, gpu })
        }

        /// Runs the steps and demands, after every one, that the two
        /// strokes report the same changed tiles and their coverage
        /// previews agree within a byte on every touched tile.
        fn run(&mut self, steps: &[Step]) {
            for step in steps {
                let (cpu_changed, gpu_changed) = match *step {
                    Step::Append(x, y) => (self.cpu.append((x, y)), self.gpu.append((x, y))),
                    Step::Flush => (self.cpu.flush(), self.gpu.flush()),
                };
                assert_eq!(gpu_changed, cpu_changed, "changed tiles after {step:?}");
                let mut tiles = cpu_changed;
                tiles.extend(self.cpu.touched_tiles());
                tiles.extend(self.gpu.touched_tiles());
                tiles.sort_unstable();
                tiles.dedup();
                for &(tx, ty) in &tiles {
                    let cpu_cover = self.cpu.tile_coverage(tx, ty);
                    let gpu_cover = self.gpu.tile_coverage(tx, ty);
                    assert_eq!(
                        cpu_cover.is_some(),
                        gpu_cover.is_some(),
                        "tile presence ({tx}, {ty}) after {step:?}"
                    );
                    if let (Some(cpu_cover), Some(gpu_cover)) = (cpu_cover, gpu_cover) {
                        assert_eq!(cpu_cover.len(), gpu_cover.len());
                        for (index, (c, g)) in cpu_cover.iter().zip(gpu_cover.iter()).enumerate() {
                            assert!(
                                (i16::from(*c) - i16::from(*g)).abs() <= 1,
                                "tile ({tx}, {ty}) byte {index} after {step:?}: \
                                 gpu {g} vs cpu {c}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_soft_click_matches_the_reference() {
        let Some(mut pair) = Pair::new(400, 400, soft()) else {
            return;
        };
        pair.run(&[Step::Append(200.0, 200.0), Step::Flush]);
    }

    #[test]
    fn a_run_across_tiles_matches_step_by_step() {
        // A bending run across four tiles, sampled densely - the parity
        // must hold after every pointer event, tail churn included.
        let Some(mut pair) = Pair::new(900, 300, soft()) else {
            return;
        };
        let steps: Vec<Step> = (0..=24)
            .map(|i| {
                let t = f64::from(i) / 24.0;
                Step::Append(80.0 + 700.0 * t, 150.0 + 60.0 * (6.0 * t).sin())
            })
            .collect();
        pair.run(&steps);
        pair.run(&[Step::Flush]);
    }

    #[test]
    fn sparse_and_dense_events_match_the_reference() {
        // The invariant the density integral exists for, on both backends
        // and in between: one long segment vs the same path in forty
        // events, each against its own CPU reference.
        let dense: Vec<Step> = (0..=40)
            .map(|i| Step::Append(100.0 + 600.0 * f64::from(i) / 40.0, 100.0))
            .collect();
        let Some(mut pair) = Pair::new(800, 200, soft()) else {
            return;
        };
        pair.run(&dense);
        pair.run(&[Step::Flush]);
        let Some(mut pair) = Pair::new(800, 200, soft()) else {
            return;
        };
        pair.run(&[
            Step::Append(100.0, 100.0),
            Step::Append(700.0, 100.0),
            Step::Flush,
        ]);
    }

    #[test]
    fn a_hard_tip_matches_the_reference() {
        let settings = BrushSettings {
            hardness: 1.0,
            ..soft()
        };
        let Some(mut pair) = Pair::new(400, 400, settings) else {
            return;
        };
        pair.run(&[
            Step::Append(200.0, 200.0),
            Step::Append(340.0, 260.0),
            Step::Flush,
        ]);
    }

    #[test]
    fn the_tail_never_bakes_in_on_the_gpu() {
        // The zigzag that turns the tail into a curve: every intermediate
        // preview - tail out, settled in - must match the CPU's.
        let Some(mut pair) = Pair::new(600, 400, soft()) else {
            return;
        };
        pair.run(&[
            Step::Append(100.0, 200.0),
            Step::Append(500.0, 200.0),
            Step::Append(300.0, 380.0),
            Step::Flush,
        ]);
    }

    #[test]
    fn repeated_flushes_stay_identical_on_the_gpu() {
        let Some(mut pair) = Pair::new(400, 400, soft()) else {
            return;
        };
        pair.run(&[
            Step::Append(100.0, 100.0),
            Step::Append(300.0, 300.0),
            Step::Flush,
        ]);
        let once = pair.gpu.tile_coverage(0, 0).expect("touched");
        pair.run(&[Step::Flush, Step::Flush]);
        let again = pair.gpu.tile_coverage(0, 0).expect("touched");
        assert_eq!(once, again, "flush must be idempotent on the GPU too");
    }

    #[test]
    fn the_gpu_composite_tracks_the_cpu_composite() {
        let Some(mut pair) = Pair::new(300, 300, soft()) else {
            return;
        };
        pair.run(&[
            Step::Append(60.0, 60.0),
            Step::Append(240.0, 200.0),
            Step::Flush,
        ]);
        let mut cpu_base = vec![0u8; 300 * 300 * 4];
        let mut gpu_base = vec![0u8; 300 * 300 * 4];
        pair.cpu.composite(&mut cpu_base);
        pair.gpu.composite(&mut gpu_base);
        for (index, (c, g)) in cpu_base.iter().zip(gpu_base.iter()).enumerate() {
            assert!(
                (i16::from(*c) - i16::from(*g)).abs() <= 1,
                "composite byte {index}: gpu {g} vs cpu {c}"
            );
        }
    }
}
