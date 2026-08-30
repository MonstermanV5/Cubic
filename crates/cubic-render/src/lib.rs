//! Minimal cross-platform wgpu renderer for the graphical bootstrap.

mod block_resources;
mod mesher;
mod world;

pub use block_resources::{BlockResourceError, BlockResources, TextureAtlasData};

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use cubic_world::WorldRenderUpdate;
use wgpu::{
    Color, CommandEncoderDescriptor, CurrentSurfaceTexture, Device, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, Operations, Queue, RenderPassColorAttachment, RenderPassDescriptor,
    StoreOp, Surface, SurfaceConfiguration, TextureViewDescriptor,
};
use winit::{dpi::PhysicalSize, window::Window};
use world::WorldRenderer;

#[derive(Clone, Debug, Default)]
pub struct WorldRenderStats {
    pub dimension: Option<String>,
    pub geometry: Option<cubic_world::DimensionGeometry>,
    pub pose: Option<cubic_world::LocalPlayerPose>,
    pub loaded_chunks: usize,
    pub meshed_chunks: usize,
    pub pending_meshes: usize,
}

const CLEAR_COLOR: Color = Color {
    r: 0.035,
    g: 0.055,
    b: 0.09,
    a: 1.0,
};

#[derive(Default)]
struct TextureDeltaQueue {
    pending: egui::TexturesDelta,
}

impl TextureDeltaQueue {
    fn enqueue(&mut self, textures: egui::TexturesDelta) {
        self.pending.append(textures);
    }

    fn take_for_paint(&mut self) -> egui::TexturesDelta {
        std::mem::take(&mut self.pending)
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl Drop for TextureDeltaQueue {
    fn drop(&mut self) {
        // Once the GPU renderer is being destroyed, no later frame can consume pending work.
        self.pending.clear();
    }
}

/// Owns the GPU objects needed to clear and present a window surface.
pub struct Renderer {
    instance: Instance,
    window: Arc<Window>,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
    size: PhysicalSize<u32>,
    configured: bool,
    out_of_memory: Arc<AtomicBool>,
    ui_renderer: egui_wgpu::Renderer,
    ui_textures: TextureDeltaQueue,
    world_renderer: Option<WorldRenderer>,
}

impl Renderer {
    /// Initializes a GPU device and a presentation surface for `window`.
    pub async fn new(window: Arc<Window>) -> Result<Self, RendererInitError> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(RendererInitError::CreateSurface)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(RendererInitError::RequestAdapter)?;

        let adapter_info = adapter.get_info();
        tracing::info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            "selected graphics adapter"
        );

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("Cubic GPU device"),
                ..Default::default()
            })
            .await
            .map_err(RendererInitError::RequestDevice)?;

        let out_of_memory = Arc::new(AtomicBool::new(false));
        let callback_out_of_memory = Arc::clone(&out_of_memory);
        device.on_uncaptured_error(Arc::new(move |error| match error {
            wgpu::Error::OutOfMemory { .. } => {
                callback_out_of_memory.store(true, Ordering::Release);
                tracing::error!(%error, "fatal GPU out-of-memory error");
            }
            _ => tracing::error!(%error, "uncaptured GPU error"),
        }));

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or(RendererInitError::UnsupportedSurface)?;
        config.present_mode = wgpu::PresentMode::AutoVsync;

        let configured = has_renderable_area(size);
        if configured {
            surface.configure(&device, &config);
        }

        tracing::info!(
            width = size.width,
            height = size.height,
            format = ?config.format,
            "renderer initialized successfully"
        );

        let ui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        Ok(Self {
            instance,
            window,
            surface,
            device,
            queue,
            config,
            size,
            configured,
            out_of_memory,
            ui_renderer,
            ui_textures: TextureDeltaQueue::default(),
            world_renderer: None,
        })
    }

    /// Updates the presentation surface to the window's new physical size.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        self.configured = has_renderable_area(size);

        if !self.configured {
            tracing::debug!(width = size.width, height = size.height, "surface paused");
            return;
        }

        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        if let Some(world) = &mut self.world_renderer {
            world.resize(&self.device, size.width, size.height);
        }
        tracing::debug!(width = size.width, height = size.height, "surface resized");
    }

    /// Enables textured terrain using preloaded, exact-version block resources.
    pub fn enable_world(&mut self, resources: BlockResources) {
        self.world_renderer = Some(WorldRenderer::new(
            &self.device,
            &self.queue,
            self.config.format,
            self.size.width,
            self.size.height,
            resources,
        ));
    }

    pub fn apply_world_update(&mut self, update: WorldRenderUpdate) {
        if let Some(world) = &mut self.world_renderer {
            world.apply(update);
        }
    }

    /// Applies a render-only look preview between fixed simulation ticks. The
    /// network simulation receives the same delta separately and remains authoritative.
    pub fn preview_world_look(&mut self, sequence: u64, yaw_delta: f32, pitch_delta: f32) {
        if let Some(world) = &mut self.world_renderer {
            world.preview_look(sequence, yaw_delta, pitch_delta);
        }
    }

    #[must_use]
    pub fn world_has_pending_work(&self) -> bool {
        self.world_renderer
            .as_ref()
            .is_some_and(WorldRenderer::has_pending_work)
    }

    #[must_use]
    pub fn world_stats(&self) -> WorldRenderStats {
        self.world_renderer
            .as_ref()
            .map_or_else(WorldRenderStats::default, WorldRenderer::stats)
    }

    pub fn render_world(&mut self) -> Result<FrameStatus, RenderError> {
        if self.out_of_memory.load(Ordering::Acquire) {
            return Err(RenderError::OutOfMemory);
        }
        if !self.configured {
            return Ok(FrameStatus::Skipped);
        }
        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => {
                self.paint_world(frame);
                Ok(FrameStatus::Presented)
            }
            CurrentSurfaceTexture::Suboptimal(frame) => {
                self.paint_world(frame);
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                Ok(FrameStatus::Skipped)
            }
            CurrentSurfaceTexture::Outdated => {
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Lost => {
                self.recreate_surface()?;
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Validation => Err(RenderError::SurfaceValidation),
        }
    }

    fn paint_world(&mut self, frame: wgpu::SurfaceTexture) {
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let Some(world) = &mut self.world_renderer else {
            self.clear_and_present(frame);
            return;
        };
        world.prepare(&self.device, &self.queue, self.size.width, self.size.height);
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("Cubic World Mode encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Cubic World Mode pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(CLEAR_COLOR),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: world.depth_view(),
                    depth_ops: Some(Operations {
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            world.draw(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    /// Returns whether the surface currently has a non-zero drawable area.
    #[must_use]
    pub const fn is_renderable(&self) -> bool {
        self.configured
    }

    /// Returns whether egui texture work is waiting for a successful surface frame.
    #[must_use]
    pub fn has_pending_ui_textures(&self) -> bool {
        !self.ui_textures.is_empty()
    }

    /// Clears and presents one frame, or recovers the surface when possible.
    pub fn render(&mut self) -> Result<FrameStatus, RenderError> {
        if self.out_of_memory.load(Ordering::Acquire) {
            return Err(RenderError::OutOfMemory);
        }

        if !self.configured {
            return Ok(FrameStatus::Skipped);
        }

        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => {
                self.clear_and_present(frame);
                Ok(FrameStatus::Presented)
            }
            CurrentSurfaceTexture::Suboptimal(frame) => {
                self.clear_and_present(frame);
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                Ok(FrameStatus::Skipped)
            }
            CurrentSurfaceTexture::Outdated => {
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Lost => {
                self.recreate_surface()?;
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Validation => Err(RenderError::SurfaceValidation),
        }
    }

    /// Renders one event-driven egui frame over Cubic's clear color.
    pub fn render_ui(
        &mut self,
        paint_jobs: &[egui::ClippedPrimitive],
        textures: egui::TexturesDelta,
        pixels_per_point: f32,
    ) -> Result<FrameStatus, RenderError> {
        self.ui_textures.enqueue(textures);
        if self.out_of_memory.load(Ordering::Acquire) {
            return Err(RenderError::OutOfMemory);
        }
        if !self.configured {
            return Ok(FrameStatus::Skipped);
        }
        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => {
                self.paint_ui(frame, paint_jobs, pixels_per_point);
                Ok(FrameStatus::Presented)
            }
            CurrentSurfaceTexture::Suboptimal(frame) => {
                self.paint_ui(frame, paint_jobs, pixels_per_point);
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                Ok(FrameStatus::Skipped)
            }
            CurrentSurfaceTexture::Outdated => {
                self.reconfigure();
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Lost => {
                self.recreate_surface()?;
                Ok(FrameStatus::Reconfigured)
            }
            CurrentSurfaceTexture::Validation => Err(RenderError::SurfaceValidation),
        }
    }

    fn paint_ui(
        &mut self,
        frame: wgpu::SurfaceTexture,
        paint_jobs: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
    ) {
        let mut textures = self.ui_textures.take_for_paint();
        for (id, deltas) in textures.set.drain() {
            for delta in deltas {
                self.ui_renderer
                    .update_texture(&self.device, &self.queue, id, &delta);
            }
        }
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("Cubic Chat Mode encoder"),
            });
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.size.width, self.size.height],
            pixels_per_point,
        };
        let extra = self.ui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            &screen,
        );
        {
            let pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Cubic Chat Mode pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(CLEAR_COLOR),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.ui_renderer
                .render(&mut pass.forget_lifetime(), paint_jobs, &screen);
        }
        self.queue
            .submit(extra.into_iter().chain([encoder.finish()]));
        self.queue.present(frame);
        for id in textures.free.drain() {
            self.ui_renderer.free_texture(&id);
        }
    }

    fn clear_and_present(&self, frame: wgpu::SurfaceTexture) {
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("Cubic clear-frame encoder"),
            });

        {
            let _pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Cubic clear-frame pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(CLEAR_COLOR),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }

        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    fn reconfigure(&self) {
        if self.configured {
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn recreate_surface(&mut self) -> Result<(), RenderError> {
        self.surface = self
            .instance
            .create_surface(Arc::clone(&self.window))
            .map_err(RenderError::RecreateSurface)?;
        self.reconfigure();
        tracing::warn!("lost GPU surface recreated");
        Ok(())
    }
}

/// Result of attempting to render one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameStatus {
    /// A clear frame was submitted and presented.
    Presented,
    /// The surface changed and was recovered for a future frame.
    Reconfigured,
    /// No frame was needed or available, such as while minimized.
    Skipped,
}

/// Error returned while creating the renderer.
#[derive(Debug)]
pub enum RendererInitError {
    /// wgpu could not create a surface for the native window.
    CreateSurface(wgpu::CreateSurfaceError),
    /// No compatible graphics adapter was available.
    RequestAdapter(wgpu::RequestAdapterError),
    /// wgpu could not create the logical device and queue.
    RequestDevice(wgpu::RequestDeviceError),
    /// The selected adapter did not report a usable surface configuration.
    UnsupportedSurface,
}

impl fmt::Display for RendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSurface(error) => {
                write!(formatter, "could not create GPU surface: {error}")
            }
            Self::RequestAdapter(error) => {
                write!(formatter, "could not select a graphics adapter: {error}")
            }
            Self::RequestDevice(error) => write!(formatter, "could not create GPU device: {error}"),
            Self::UnsupportedSurface => {
                formatter.write_str("graphics adapter does not support the window surface")
            }
        }
    }
}

impl Error for RendererInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateSurface(error) => Some(error),
            Self::RequestAdapter(error) => Some(error),
            Self::RequestDevice(error) => Some(error),
            Self::UnsupportedSurface => None,
        }
    }
}

/// Fatal rendering error that requires application shutdown.
#[derive(Debug)]
pub enum RenderError {
    /// The GPU reported that it exhausted available memory.
    OutOfMemory,
    /// wgpu reported a validation error while acquiring a surface frame.
    SurfaceValidation,
    /// The native presentation surface could not be recreated after loss.
    RecreateSurface(wgpu::CreateSurfaceError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => formatter.write_str("GPU is out of memory"),
            Self::SurfaceValidation => {
                formatter.write_str("GPU surface acquisition failed validation")
            }
            Self::RecreateSurface(error) => {
                write!(formatter, "could not recreate lost GPU surface: {error}")
            }
        }
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RecreateSurface(error) => Some(error),
            Self::OutOfMemory | Self::SurfaceValidation => None,
        }
    }
}

const fn has_renderable_area(size: PhysicalSize<u32>) -> bool {
    size.width > 0 && size.height > 0
}

#[cfg(test)]
mod tests {
    use super::{TextureDeltaQueue, has_renderable_area};
    use egui::{Color32, ColorImage, TextureId, TextureOptions, epaint::ImageDelta};
    use winit::dpi::PhysicalSize;

    #[test]
    fn only_two_non_zero_dimensions_are_renderable() {
        assert!(has_renderable_area(PhysicalSize::new(1, 1)));
        assert!(!has_renderable_area(PhysicalSize::new(0, 1)));
        assert!(!has_renderable_area(PhysicalSize::new(1, 0)));
        assert!(!has_renderable_area(PhysicalSize::new(0, 0)));
    }

    #[test]
    fn texture_delta_queue_preserves_all_updates_until_paint() {
        let texture = TextureId::Managed(1);
        let retired = TextureId::Managed(2);
        let image = || ColorImage::filled([1, 1], Color32::WHITE);

        let mut first = egui::TexturesDelta::default();
        first.push(texture, ImageDelta::full(image(), TextureOptions::LINEAR));
        first.push(
            texture,
            ImageDelta::partial([0, 0], image(), TextureOptions::LINEAR),
        );
        first.free(retired);

        let mut second = egui::TexturesDelta::default();
        second.push(
            texture,
            ImageDelta::partial([0, 0], image(), TextureOptions::LINEAR),
        );

        let mut queue = TextureDeltaQueue::default();
        queue.enqueue(first);
        queue.enqueue(second);
        let mut ready = queue.take_for_paint();
        let update_count = ready.set.get(&texture).map_or(0, |updates| updates.len());
        let retained_free = ready.free.contains(&retired);
        ready.clear();

        assert_eq!(update_count, 3);
        assert!(retained_free);
        assert!(queue.take_for_paint().is_empty());
    }
}
