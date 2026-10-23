// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! Bounded, on-demand copies of an already adopted monitor texture.
//!
//! A Host owns exactly one long-lived reader per actual Device. This module
//! enforces one physical read within that reader; the Host enforces unique
//! ownership. Keep it on the device's event thread for capture and polling,
//! alongside the compositor's queue submissions. Capture must happen in the
//! same event callback that validates the adopted texture, before a later
//! composite can overwrite its ring slot. A texture handle alone is not frozen.
//!
//! Polling never waits for GPU work. The two-second application deadline ends
//! delivery, not submitted GPU commands. Cancelled/expired work keeps its slot
//! and resources until the queue completion callback runs. Keep polling during
//! idle/paused periods, including after cancellation, or let the next capture
//! attempt do one non-blocking cleanup. A stalled device stays busy until the
//! Host tears down the reader with that device; recreating readers to bypass
//! busy is outside the ownership contract.

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Maximum output edge, in pixels.
pub const MAX_EDGE: u32 = 256;
/// Maximum application wait; a shorter caller deadline takes precedence.
pub const MAX_WAIT: Duration = Duration::from_secs(2);

// Match the public clip-preview wire contract before allocating read resources.
const MAX_SOURCE_EDGE: u32 = 16_384;

const PENDING: u8 = 0;
const CANCELLED: u8 = 1;
const EXPIRED: u8 = 2;
const MAP_FAILED: u8 = 3;
const POLL_FAILED: u8 = 4;
const DELIVERED: u8 = 5;

/// A bounded preview request could not be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewError {
    /// An earlier physical read has not been reclaimed yet.
    Busy,
    /// The source must be single-sample/mip D2 Rgba8Unorm, sampled, with
    /// nonzero width and height no greater than 16384.
    InvalidSource,
    /// The requested maximum edge must be between one and 256 inclusive.
    InvalidMaxEdge,
    /// The application deadline elapsed; late bytes must not be delivered.
    Expired,
    /// The caller cancelled or dropped the ticket.
    Cancelled,
    /// GPU buffer mapping failed; no fallback image is returned.
    MappingUnavailable,
    /// Device polling failed; no fallback image is returned.
    PollUnavailable,
    /// The ticket belongs to another request or was already delivered.
    InvalidTicket,
}

/// Packed, top-to-bottom RGBA8 pixels; no row padding, overlays, or PNG encoding.
#[derive(Debug)]
pub struct PreviewRgba {
    /// Output width, never greater than the requested maximum edge.
    pub width: u32,
    /// Output height, never greater than the requested maximum edge.
    pub height: u32,
    /// Exactly width * height * 4 bytes, preserving source alpha.
    pub pixels: Vec<u8>,
}

/// Result of one non-blocking poll. Ready bytes are delivered at most once.
#[derive(Debug)]
pub enum PreviewPoll {
    /// GPU mapping/queue completion has not been observed yet.
    Pending,
    /// The dedicated captured thumbnail is ready for worker-side encoding.
    Ready(PreviewRgba),
    /// The request cannot deliver pixels.
    Failed(PreviewError),
}

struct TicketState {
    status: AtomicU8,
}

/// Cancellation identity, deliberately not Clone. Dropping records cancellation
/// intent; the reader still owns all resources and the physical in-flight slot.
pub struct PreviewTicket {
    state: Arc<TicketState>,
}

impl Drop for PreviewTicket {
    fn drop(&mut self) {
        let _ = self.state.status.compare_exchange(
            PENDING,
            CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

struct InFlight {
    state: Arc<TicketState>,
    deadline: Instant,
    // Kept independent of compositor/export and monitor ring resources.
    _target: wgpu::Texture,
    staging: wgpu::Buffer,
    padded_row: usize,
    width: u32,
    height: u32,
    mapped: Receiver<Result<(), wgpu::BufferAsyncError>>,
    map_result: Option<Result<(), wgpu::BufferAsyncError>>,
    queue_done: Arc<AtomicBool>,
    unmapped: bool,
}

impl InFlight {
    fn unmap(&mut self) {
        if !self.unmapped {
            self.staging.unmap();
            self.unmapped = true;
        }
    }

    fn abort(&mut self, status: u8) {
        let _ = self.state.status.compare_exchange(
            PENDING,
            status,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.unmap();
    }

    fn enforce_deadline(&mut self, now: Instant) {
        if self.state.status.load(Ordering::Acquire) != PENDING {
            self.unmap();
        } else if now >= self.deadline {
            self.abort(EXPIRED);
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.unmap();
    }
}

/// One reader for the Host's existing Device/Queue, with at most one physical
/// read outstanding. It does not create an adapter, device, renderer, or timer.
pub struct PreviewReader {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    active: Option<InFlight>,
    // Submission ordering belongs to the same UI/device event thread.
    _event_thread: PhantomData<Rc<()>>,
    #[cfg(test)]
    allocations: usize,
}

impl PreviewReader {
    /// Reuses the Host's original device and queue. Construct once per actual
    /// Device, keep it for that Device's lifetime, and reuse after each request.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("concat adopted preview"),
            source: wgpu::ShaderSource::Wgsl(THUMBNAIL_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("concat adopted preview"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("concat adopted preview"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("concat adopted preview"),
            layout: Some(&pipeline_layout),
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
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("concat adopted preview"),
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            device,
            queue,
            pipeline,
            layout,
            sampler,
            active: None,
            _event_thread: PhantomData,
            #[cfg(test)]
            allocations: 0,
        }
    }

    /// Immediately samples the entire current final texture into a dedicated
    /// bounded target and submits its copy to independent staging. The source
    /// must belong to this device; the trusted Host supplies the adopted texture.
    /// No original ring handle is retained for deferred capture.
    pub fn capture(
        &mut self,
        source: &wgpu::Texture,
        max_edge: u32,
        caller_deadline: Instant,
    ) -> Result<PreviewTicket, PreviewError> {
        self.maintain();
        if self.active.is_some() {
            return Err(PreviewError::Busy);
        }
        if !(1..=MAX_EDGE).contains(&max_edge) {
            return Err(PreviewError::InvalidMaxEdge);
        }
        let now = Instant::now();
        if caller_deadline <= now {
            return Err(PreviewError::Expired);
        }
        let size = source.size();
        if source.format() != wgpu::TextureFormat::Rgba8Unorm
            || source.dimension() != wgpu::TextureDimension::D2
            || source.sample_count() != 1
            || source.mip_level_count() != 1
            || size.depth_or_array_layers != 1
            || !valid_source_size(size.width, size.height)
            || !source
                .usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
        {
            return Err(PreviewError::InvalidSource);
        }
        let (width, height) = thumbnail_size(size.width, size.height, max_edge);
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("concat captured preview"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // Both dimensions <=256: row <=1024 and padded buffer <=262144 bytes.
        let padded_row = (width as usize * 4).div_ceil(super::ROW_ALIGN) * super::ROW_ALIGN;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat captured preview staging"),
            size: (padded_row * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        #[cfg(test)]
        {
            self.allocations += 1;
        }
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("concat captured preview"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("concat captured preview"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("concat captured preview"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);
        let (tx, mapped) = mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        let queue_done = Arc::new(AtomicBool::new(false));
        let completed = Arc::clone(&queue_done);
        self.queue.on_submitted_work_done(move || {
            completed.store(true, Ordering::Release);
        });
        let state = Arc::new(TicketState {
            status: AtomicU8::new(PENDING),
        });
        self.active = Some(InFlight {
            state: Arc::clone(&state),
            deadline: caller_deadline.min(now + MAX_WAIT),
            _target: target,
            staging,
            padded_row,
            width,
            height,
            mapped,
            map_result: None,
            queue_done,
            unmapped: false,
        });
        Ok(PreviewTicket { state })
    }

    /// Cancels delivery and unmaps immediately, while retaining the physical
    /// slot until actual queue completion. A different reader's ticket is rejected.
    pub fn cancel(&mut self, ticket: &PreviewTicket) -> Result<(), PreviewError> {
        let active = self
            .active
            .as_mut()
            .filter(|a| Arc::ptr_eq(&a.state, &ticket.state))
            .ok_or(PreviewError::InvalidTicket)?;
        active.abort(CANCELLED);
        Ok(())
    }

    /// Advances mapping and reclamation once with PollType::Poll. Safe to call
    /// with no request; the Host must schedule this during paused/idle capture.
    pub fn maintain(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.enforce_deadline(Instant::now());
        } else {
            return;
        }
        let poll_failed = self.device.poll(wgpu::PollType::Poll).is_err();
        let active = self
            .active
            .as_mut()
            .expect("active is retained across poll");
        active.enforce_deadline(Instant::now());
        if poll_failed {
            active.abort(POLL_FAILED);
        }
        if active.map_result.is_none() {
            match active.mapped.try_recv() {
                Ok(result) => {
                    active.map_result = Some(result);
                }
                Err(mpsc::TryRecvError::Disconnected) => active.abort(MAP_FAILED),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if matches!(active.map_result, Some(Err(_))) {
            active.abort(MAP_FAILED);
        }
        if active.state.status.load(Ordering::Acquire) != PENDING
            && active.queue_done.load(Ordering::Acquire)
        {
            self.active = None;
        }
    }

    /// Delivers tightly packed RGBA only after successful map and observed
    /// completion, and only before the deadline. No blocking wait or PNG work.
    pub fn poll(&mut self, ticket: &PreviewTicket) -> PreviewPoll {
        self.maintain();
        let status = ticket.state.status.load(Ordering::Acquire);
        if status != PENDING {
            return PreviewPoll::Failed(status_error(status));
        }
        let Some(active) = self
            .active
            .as_mut()
            .filter(|a| Arc::ptr_eq(&a.state, &ticket.state))
        else {
            return PreviewPoll::Failed(PreviewError::InvalidTicket);
        };
        if !matches!(active.map_result, Some(Ok(()))) || !active.queue_done.load(Ordering::Acquire)
        {
            return PreviewPoll::Pending;
        }
        let row_bytes = active.width as usize * 4;
        let mut pixels = vec![0; row_bytes * active.height as usize];
        {
            let data = active.staging.slice(..).get_mapped_range();
            for (row, output) in pixels.chunks_exact_mut(row_bytes).enumerate() {
                output.copy_from_slice(
                    &data[row * active.padded_row..row * active.padded_row + row_bytes],
                );
            }
        }
        active.enforce_deadline(Instant::now());
        let status = active.state.status.load(Ordering::Acquire);
        if status != PENDING {
            self.active = None;
            return PreviewPoll::Failed(status_error(status));
        }
        let result = PreviewRgba {
            width: active.width,
            height: active.height,
            pixels,
        };
        active.state.status.store(DELIVERED, Ordering::Release);
        active.unmap();
        self.active = None;
        PreviewPoll::Ready(result)
    }
}

fn status_error(status: u8) -> PreviewError {
    match status {
        CANCELLED => PreviewError::Cancelled,
        EXPIRED => PreviewError::Expired,
        MAP_FAILED => PreviewError::MappingUnavailable,
        POLL_FAILED => PreviewError::PollUnavailable,
        _ => PreviewError::InvalidTicket,
    }
}

fn thumbnail_size(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let edge = width.max(height);
    if edge <= max_edge {
        return (width, height);
    }
    (
        (u64::from(width) * u64::from(max_edge) / u64::from(edge)).max(1) as u32,
        (u64::from(height) * u64::from(max_edge) / u64::from(edge)).max(1) as u32,
    )
}

fn valid_source_size(width: u32, height: u32) -> bool {
    (1..=MAX_SOURCE_EDGE).contains(&width) && (1..=MAX_SOURCE_EDGE).contains(&height)
}

const THUMBNAIL_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var output: VertexOutput;
    output.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    output.uv = uv;
    return output;
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, input.uv);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, WgpuCompositor};
    use concat_core::frame::Frame;

    #[test]
    fn bounded_dimensions_include_extreme_aspects_without_overflow() {
        assert!(valid_source_size(16_384, 16_384));
        assert!(!valid_source_size(16_385, 1));
        assert!(!valid_source_size(1, 16_385));
        assert!(!valid_source_size(0, 1));
        assert!(!valid_source_size(1, 0));
        assert_eq!(thumbnail_size(600, 300, 127), (127, 63));
        assert_eq!(thumbnail_size(7, 5, 256), (7, 5));
        assert_eq!(thumbnail_size(u32::MAX, 1, 256), (256, 1));
        assert_eq!(thumbnail_size(1, u32::MAX, 256), (1, 256));
        assert_eq!(thumbnail_size(u32::MAX, u32::MAX, 256), (256, 256));
    }

    // This test MUST fail without a real adapter. A skip/CPU compositor result
    // cannot count as proof of shader sampling, staging copy and successful map.
    #[test]
    fn headless_gpu_captures_full_frame_and_reclaims_cancelled_work() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("real headless GPU adapter is required; skipping is not success");
        let info = adapter.get_info();
        assert_ne!(
            info.backend,
            wgpu::Backend::Noop,
            "noop is not GPU evidence"
        );
        eprintln!("preview headless adapter={info:?}");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("headless device");
        let mut reader = PreviewReader::new(device.clone(), queue.clone());

        // Distant corners and two axes distinguish a full-frame downsample from
        // a top-left copy, a vertical flip, and swapped RGBA channels.
        let mut quadrants = Frame::transparent(600, 300);
        let colours = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [225, 75, 150, 255],
        ];
        for y in 0..300 {
            for x in 0..600 {
                quadrants.set_pixel(x, y, colours[(y >= 150) as usize * 2 + (x >= 300) as usize]);
            }
        }
        let source = upload(&device, &queue, &quadrants);
        let ticket = reader
            .capture(&source, 127, deadline())
            .expect("capture quadrants");
        assert_eq!(reader.active.as_ref().unwrap().padded_row, 512);
        let out = ready(&mut reader, &ticket);
        assert_eq!(
            (out.width, out.height, out.pixels.len()),
            (127, 63, 127 * 63 * 4)
        );
        assert_eq!(pixel(&out, 10, 10), colours[0]);
        assert_eq!(pixel(&out, 116, 10), colours[1]);
        assert_eq!(pixel(&out, 10, 52), colours[2]);
        assert_eq!(pixel(&out, 116, 52), colours[3]);
        assert!(matches!(
            reader.poll(&ticket),
            PreviewPoll::Failed(PreviewError::InvalidTicket)
        ));

        // No scaling: every channel including alpha must match exactly and
        // padded 256-byte GPU rows must become packed 28-byte CPU rows.
        let mut exact = Frame::transparent(7, 5);
        for y in 0..5 {
            for x in 0..7 {
                exact.set_pixel(
                    x,
                    y,
                    [
                        (x * 25 + 3) as u8,
                        (y * 45 + 2) as u8,
                        231,
                        (x * 20 + y * 7) as u8,
                    ],
                );
            }
        }
        let exact_source = upload(&device, &queue, &exact);
        let ticket = reader.capture(&exact_source, 256, deadline()).unwrap();
        assert_eq!(reader.active.as_ref().unwrap().padded_row, 256);
        let out = ready(&mut reader, &ticket);
        assert_eq!(
            out.pixels,
            exact.pixels(),
            "orientation, RGBA, alpha and padding"
        );

        // Use the actual compositor's three-slot monitor ring. Capture an
        // already shader-treated result, then overwrite that exact ring slot
        // before polling. Only an immediate independent capture preserves cyan.
        let mut compositor = WgpuCompositor::with_device(device.clone(), queue.clone());
        let manifest = concat_effects::Manifest::parse(
            "[effect]\nid = \"test.preview-invert\"\nname = \"Invert\"\nkind = \"filter\"\n[wgsl]\nentry = \"effect.wgsl\"\n"
        ).unwrap();
        let shader = concat_effects::Shader::compile(&manifest,
            "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(vec3<f32>(1.0) - c.rgb, c.a); }").unwrap();
        let passes = [shader.pass(&Default::default(), &[], 1.0, None)];
        let mut red = Frame::black(64, 32);
        red.fill([255, 0, 0, 255]);
        let adopted = compositor
            .composite_texture(64, 32, &[Layer::new(&red).with_passes(&passes)])
            .unwrap();
        let ticket = reader.capture(&adopted, 31, deadline()).unwrap();
        let mut green = Frame::black(64, 32);
        green.fill([0, 255, 0, 255]);
        for _ in 0..3 {
            compositor
                .composite_texture(64, 32, &[Layer::new(&green)])
                .unwrap();
        }
        let out = ready(&mut reader, &ticket);
        assert!(out.pixels.chunks_exact(4).all(|p| p == [0, 255, 255, 255]));
        let ticket = reader.capture(&adopted, 31, deadline()).unwrap();
        let overwritten = ready(&mut reader, &ticket);
        assert!(
            overwritten
                .pixels
                .chunks_exact(4)
                .all(|p| p == [0, 255, 0, 255]),
            "the source ring slot was really overwritten"
        );

        // Deliberately withhold the observed completion notification. Real GPU
        // callbacks still run, but this simulates late completion delivery and
        // proves cancellation cannot create arbitrarily many physical buffers.
        let ticket = reader.capture(&source, 64, deadline()).unwrap();
        let actual_done = Arc::clone(&reader.active.as_ref().unwrap().queue_done);
        reader.active.as_mut().unwrap().queue_done = Arc::new(AtomicBool::new(false));
        let allocations = reader.allocations;
        reader.cancel(&ticket).unwrap();
        assert!(reader.active.as_ref().unwrap().unmapped);
        for _ in 0..16 {
            assert!(matches!(
                reader.capture(&source, 64, deadline()),
                Err(PreviewError::Busy)
            ));
            assert!(matches!(
                reader.poll(&ticket),
                PreviewPoll::Failed(PreviewError::Cancelled)
            ));
        }
        assert_eq!(
            reader.allocations, allocations,
            "cancel cannot bypass physical capacity"
        );
        await_done(&device, &actual_done);
        reader.active.as_mut().unwrap().queue_done = actual_done;
        reader.maintain();
        assert!(
            reader.active.is_none(),
            "actual completion permits reclamation"
        );
        assert!(
            matches!(
                reader.poll(&ticket),
                PreviewPoll::Failed(PreviewError::Cancelled)
            ),
            "late callback cannot deliver cancelled pixels"
        );

        // Deadline wins over a successful late map. Inject only the application
        // clock boundary; the buffer and eventual completion remain real GPU work.
        let ticket = reader.capture(&source, 64, deadline()).unwrap();
        let actual_done = Arc::clone(&reader.active.as_ref().unwrap().queue_done);
        reader.active.as_mut().unwrap().queue_done = Arc::new(AtomicBool::new(false));
        reader.active.as_mut().unwrap().deadline = Instant::now();
        assert!(matches!(
            reader.poll(&ticket),
            PreviewPoll::Failed(PreviewError::Expired)
        ));
        assert!(reader.active.as_ref().unwrap().unmapped);
        let allocations = reader.allocations;
        for _ in 0..16 {
            assert!(matches!(
                reader.capture(&source, 64, deadline()),
                Err(PreviewError::Busy)
            ));
        }
        assert_eq!(
            reader.allocations, allocations,
            "expiry retains physical capacity"
        );
        await_done(&device, &actual_done);
        reader.active.as_mut().unwrap().queue_done = actual_done;
        reader.maintain();
        assert!(reader.active.is_none());
        assert!(matches!(
            reader.poll(&ticket),
            PreviewPoll::Failed(PreviewError::Expired)
        ));
        let ticket = reader.capture(&exact_source, 256, deadline()).unwrap();
        assert_eq!(
            ready(&mut reader, &ticket).pixels,
            exact.pixels(),
            "resources reusable after cancellation and timeout"
        );

        // Drop is cancellation intent and maintain performs the unmap/reclaim.
        let ticket = reader.capture(&source, 64, deadline()).unwrap();
        let state = Arc::clone(&ticket.state);
        drop(ticket);
        assert_eq!(state.status.load(Ordering::Acquire), CANCELLED);
        let until = Instant::now() + MAX_WAIT;
        while reader.active.is_some() {
            reader.maintain();
            assert!(
                Instant::now() < until,
                "drop cancellation must reclaim after queue completion"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let allocations = reader.allocations;
        // The monitor's final output always has one mip. Reject a valid GPU
        // texture with two mips before allocating target/staging or sampling an
        // implicit lower-resolution mip through the default texture view.
        let mipmapped = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview rejected mipmapped source"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 2,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        assert!(matches!(
            reader.capture(&mipmapped, 256, deadline()),
            Err(PreviewError::InvalidSource)
        ));
        assert_eq!(
            reader.allocations, allocations,
            "invalid source allocates no read resources"
        );
        assert!(matches!(
            reader.capture(&source, 257, deadline()),
            Err(PreviewError::InvalidMaxEdge)
        ));
        assert!(matches!(
            reader.capture(&source, 0, deadline()),
            Err(PreviewError::InvalidMaxEdge)
        ));
        assert!(matches!(
            reader.capture(&source, 256, Instant::now()),
            Err(PreviewError::Expired)
        ));
        assert_eq!(
            reader.allocations, allocations,
            "invalid requests allocate nothing"
        );
        let ticket = reader
            .capture(&source, 256, Instant::now() + Duration::from_secs(60))
            .unwrap();
        assert!(
            reader.active.as_ref().unwrap().deadline <= Instant::now() + MAX_WAIT,
            "two-second application cap"
        );
        reader.cancel(&ticket).unwrap();
        reader.maintain();
        eprintln!(
            "preview real GPU map succeeded; full frame, padding/RGBA/alpha, ring overwrite, cancel/expiry/late callback and resource reuse verified"
        );
    }

    fn deadline() -> Instant {
        Instant::now() + MAX_WAIT
    }

    fn upload(device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width: frame.width(),
            height: frame.height(),
            depth_or_array_layers: 1,
        };
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview GPU test source"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            frame.pixels(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width() * 4),
                rows_per_image: Some(frame.height()),
            },
            size,
        );
        source
    }

    fn ready(reader: &mut PreviewReader, ticket: &PreviewTicket) -> PreviewRgba {
        let until = deadline();
        loop {
            match reader.poll(ticket) {
                PreviewPoll::Ready(frame) => return frame,
                PreviewPoll::Failed(error) => panic!("real GPU capture failed: {error:?}"),
                PreviewPoll::Pending => {}
            }
            assert!(Instant::now() < until, "real map did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn await_done(device: &wgpu::Device, done: &AtomicBool) {
        let until = deadline();
        while !done.load(Ordering::Acquire) {
            device.poll(wgpu::PollType::Poll).unwrap();
            assert!(Instant::now() < until, "real GPU completion not observed");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn pixel(frame: &PreviewRgba, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * frame.width as usize + x) * 4;
        frame.pixels[offset..offset + 4].try_into().unwrap()
    }
}
