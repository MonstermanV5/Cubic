struct Camera { view_projection: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var block_atlas: texture_2d<f32>;
@group(1) @binding(1) var block_sampler: sampler;

struct VertexInput { @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) tint: vec3<f32>, @location(3) layer: u32 };
struct VertexOutput { @builtin(position) clip_position: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec3<f32>, @location(2) @interpolate(flat) layer: u32 };

@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.tint = input.tint;
    output.layer = input.layer;
    return output;
}

@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(block_atlas, block_sampler, input.uv) * vec4<f32>(input.tint, 1.0);
    if input.layer == 1u && color.a < 0.5 { discard; }
    return color;
}
