struct Camera { view_projection: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var block_atlas: texture_2d<f32>;
@group(1) @binding(1) var block_sampler: sampler;
@group(1) @binding(2) var cutout_block_sampler: sampler;

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
    // Vanilla 26.1.2's terrain atlas is RGBA8 (not an sRGB texture), and its
    // shader multiplies normalized texture/tint/light values in that encoded
    // domain. Cubic keeps an sRGB atlas and sRGB presentation surface, so
    // explicitly reconstruct that same encoded-domain product between the two
    // hardware transfer conversions.
    // Sample outside control flow so implicit derivatives remain valid on all
    // WebGPU backends; `layer` then selects the cutout coverage policy.
    let filtered_sample = textureSample(block_atlas, block_sampler, input.uv);
    let cutout_sample = textureSample(block_atlas, cutout_block_sampler, input.uv);
    let material_layer = input.layer & 0xffu;
    let debug_face = (input.layer >> 8u) & 0xffu;
    let debug_clipped = ((input.layer >> 16u) & 1u) != 0u;
    let sampled = select(filtered_sample, cutout_sample, material_layer == 1u);
    if debug_face != 0u {
        var debug_color = vec3<f32>(1.0, 1.0, 1.0);
        switch debug_face {
            case 1u: { debug_color = vec3<f32>(1.0, 0.0, 1.0); }
            case 2u: { debug_color = vec3<f32>(1.0, 0.0, 0.0); }
            case 3u: { debug_color = vec3<f32>(0.0, 1.0, 0.0); }
            case 4u: { debug_color = vec3<f32>(0.0, 0.25, 1.0); }
            case 5u: { debug_color = vec3<f32>(1.0, 1.0, 0.0); }
            case 6u: { debug_color = vec3<f32>(0.0, 1.0, 1.0); }
            default: {}
        }
        if debug_clipped { debug_color = mix(debug_color, vec3<f32>(1.0), 0.4); }
        return vec4<f32>(srgb_to_linear(debug_color), 1.0);
    }
    let encoded_texture = linear_to_srgb(sampled.rgb);
    let encoded_color = encoded_texture * input.tint;
    let color = vec4<f32>(srgb_to_linear(encoded_color), sampled.a);
    if material_layer == 1u && color.a < 0.5 { discard; }
    return color;
}

fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
    let low = value / 12.92;
    let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, value <= vec3<f32>(0.04045));
}

fn linear_to_srgb(value: vec3<f32>) -> vec3<f32> {
    let low = value * 12.92;
    let high = 1.055 * pow(max(value, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, value <= vec3<f32>(0.0031308));
}
