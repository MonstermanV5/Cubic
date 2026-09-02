struct Camera {
    view_projection: mat4x4<f32>,
    eye_and_view_shrink: vec4<f32>,
    viewport_and_line_width: vec4<f32>,
    selection_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var destroy_texture: texture_2d<f32>;
@group(1) @binding(1) var destroy_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(destroy_texture, destroy_sampler, input.uv);
    if color.a < 0.1 {
        discard;
    }
    return color;
}
