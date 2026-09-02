use bytemuck::{Pod, Zeroable};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingType, BlendComponent, BlendFactor, BlendOperation, BlendState,
    Buffer, BufferBindingType, BufferUsages, ColorTargetState, ColorWrites, Device, Extent3d,
    FilterMode, FragmentState, MipmapFilterMode, MultisampleState, Origin3d,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology, Queue,
    RenderPass, RenderPipeline, RenderPipelineDescriptor, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, TexelCopyBufferLayout,
    TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, TextureViewDescriptor, TextureViewDimension, VertexState,
    util::{BufferInitDescriptor, DeviceExt},
};

use crate::GuiSpriteData;

const VANILLA_CROSSHAIR_LOGICAL_SIZE: u32 = 15;
const CROSSHAIR_SHADER: &str = include_str!("crosshair.wgsl");
const CROSSHAIR_COLOR_BLEND: BlendComponent = BlendComponent {
    src_factor: BlendFactor::OneMinusDst,
    dst_factor: BlendFactor::OneMinusSrc,
    operation: BlendOperation::Add,
};
const CROSSHAIR_ALPHA_BLEND: BlendComponent = BlendComponent {
    src_factor: BlendFactor::One,
    dst_factor: BlendFactor::Zero,
    operation: BlendOperation::Add,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct CrosshairUniform {
    viewport: [f32; 2],
    origin: [f32; 2],
    size: [f32; 2],
    _padding: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CrosshairPlacement {
    gui_scale: u32,
    gui_size: [u32; 2],
    origin: [f32; 2],
    size: [f32; 2],
}

impl CrosshairPlacement {
    fn new(width: u32, height: u32) -> Self {
        let gui_scale = automatic_gui_scale(width, height);
        let gui_width = width.div_ceil(gui_scale);
        let gui_height = height.div_ceil(gui_scale);
        let logical_size = VANILLA_CROSSHAIR_LOGICAL_SIZE;
        let logical_x = gui_width.saturating_sub(logical_size) / 2;
        let logical_y = gui_height.saturating_sub(logical_size) / 2;
        let scaled = logical_size * gui_scale;
        Self {
            gui_scale,
            gui_size: [gui_width, gui_height],
            origin: [
                (logical_x * gui_scale) as f32,
                (logical_y * gui_scale) as f32,
            ],
            size: [scaled as f32, scaled as f32],
        }
    }
}

fn automatic_gui_scale(width: u32, height: u32) -> u32 {
    let mut scale = 1_u32;
    while width / (scale + 1) >= 320 && height / (scale + 1) >= 240 {
        scale += 1;
    }
    scale
}

pub(crate) struct CrosshairRenderer {
    pipeline: RenderPipeline,
    uniform: Buffer,
    bind_group: BindGroup,
}

impl CrosshairRenderer {
    pub(crate) fn new(
        device: &Device,
        queue: &Queue,
        surface_format: TextureFormat,
        sprite: &GuiSpriteData,
    ) -> Self {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("Cubic exact-version crosshair sprite"),
            size: Extent3d {
                width: sprite.width,
                height: sprite.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            // Minecraft's HUD sprite channels reach its blend stage without
            // an sRGB texture decode. This is significant for resource-pack
            // crosshairs and leaves partial alpha exactly as authored.
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &sprite.rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(sprite.width * 4),
                rows_per_image: Some(sprite.height),
            },
            Extent3d {
                width: sprite.width,
                height: sprite.height,
                depth_or_array_layers: 1,
            },
        );
        let texture_view = texture.create_view(&TextureViewDescriptor::default());
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("Cubic pixel-crisp crosshair sampler"),
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            mipmap_filter: MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&BufferInitDescriptor {
            label: Some("Cubic crosshair placement"),
            contents: bytemuck::bytes_of(&CrosshairUniform::zeroed()),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        });
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("Cubic crosshair bind-group layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Cubic crosshair bind group"),
            layout: &layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Cubic vanilla crosshair shader"),
            source: ShaderSource::Wgsl(CROSSHAIR_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Cubic crosshair pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Cubic vanilla crosshair pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: surface_format,
                    blend: Some(BlendState {
                        color: CROSSHAIR_COLOR_BLEND,
                        alpha: CROSSHAIR_ALPHA_BLEND,
                    }),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            uniform,
            bind_group,
        }
    }

    pub(crate) fn prepare(&self, queue: &Queue, width: u32, height: u32) {
        let placement = CrosshairPlacement::new(width, height);
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&CrosshairUniform {
                viewport: [width as f32, height as f32],
                origin: placement.origin,
                size: placement.size,
                _padding: [0.0; 2],
            }),
        );
    }

    pub(crate) fn draw<'pass>(&'pass self, pass: &mut RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_crosshair_uses_integer_centered_fifteen_pixel_sprite() {
        assert_eq!(
            CrosshairPlacement::new(1280, 720),
            CrosshairPlacement {
                gui_scale: 3,
                gui_size: [427, 240],
                origin: [618.0, 336.0],
                size: [45.0, 45.0],
            }
        );
        assert_eq!(
            CrosshairPlacement::new(1279, 719),
            CrosshairPlacement {
                gui_scale: 2,
                gui_size: [640, 360],
                origin: [624.0, 344.0],
                size: [30.0, 30.0],
            }
        );
        let resized = CrosshairPlacement::new(1920, 1080);
        assert_eq!(resized.gui_scale, 4);
        assert_eq!(resized.gui_size, [480, 270]);
        assert_eq!(resized.origin, [928.0, 508.0]);
        assert_eq!(resized.size, [60.0; 2]);
    }

    #[test]
    fn vanilla_crosshair_blend_is_destination_inverting() {
        assert_eq!(CROSSHAIR_COLOR_BLEND.src_factor, BlendFactor::OneMinusDst);
        assert_eq!(CROSSHAIR_COLOR_BLEND.dst_factor, BlendFactor::OneMinusSrc);
        assert_eq!(CROSSHAIR_COLOR_BLEND.operation, BlendOperation::Add);
        assert_eq!(CROSSHAIR_ALPHA_BLEND.src_factor, BlendFactor::One);
        assert_eq!(CROSSHAIR_ALPHA_BLEND.dst_factor, BlendFactor::Zero);
        assert_eq!(CROSSHAIR_ALPHA_BLEND.operation, BlendOperation::Add);
    }

    #[test]
    fn crosshair_shader_discards_only_transparent_texels_and_preserves_sample_alpha() {
        assert!(CROSSHAIR_SHADER.contains("if colour.a == 0.0"));
        assert!(CROSSHAIR_SHADER.contains("return colour;"));
    }
}
