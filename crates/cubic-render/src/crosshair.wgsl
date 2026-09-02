struct Crosshair {
    viewport: vec2<f32>,
    origin: vec2<f32>,
    size: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var<uniform> crosshair: Crosshair;
@group(0) @binding(1) var sprite: texture_2d<f32>;
@group(0) @binding(2) var sprite_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex_index];
    let pixel = crosshair.origin + corner * crosshair.size;
    let clip = vec2<f32>(
        pixel.x / crosshair.viewport.x * 2.0 - 1.0,
        1.0 - pixel.y / crosshair.viewport.y * 2.0,
    );
    var output: VertexOutput;
    output.position = vec4<f32>(clip, 0.0, 1.0);
    output.uv = corner;
    return output;
}

@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let colour = textureSample(sprite, sprite_sampler, input.uv);
    if colour.a == 0.0 {
        discard;
    }
    return colour;
}
