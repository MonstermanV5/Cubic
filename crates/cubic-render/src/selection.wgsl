struct Camera {
    view_projection: mat4x4<f32>,
    eye_and_selection_shrink: vec4<f32>,
    viewport_and_selection_width: vec4<f32>,
    selection_color: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) start: vec3<f32>,
    @location(1) end: vec3<f32>,
    // x selects the endpoint (0/1), y selects the perpendicular side (-1/1).
    @location(2) corner: vec2<f32>,
};

@vertex fn vs_main(input: VertexInput) -> @builtin(position) vec4<f32> {
    let eye = camera.eye_and_selection_shrink.xyz;
    let shrink = camera.eye_and_selection_shrink.w;
    let start = eye + (input.start - eye) * shrink;
    let end = eye + (input.end - eye) * shrink;
    let clip_start = camera.view_projection * vec4<f32>(start, 1.0);
    let clip_end = camera.view_projection * vec4<f32>(end, 1.0);
    let ndc_start = clip_start.xy / clip_start.w;
    let ndc_end = clip_end.xy / clip_end.w;
    let viewport = camera.viewport_and_selection_width.xy;
    let screen_delta = (ndc_end - ndc_start) * viewport * 0.5;
    let segment_length = max(length(screen_delta), 1.0e-5);
    let perpendicular = vec2<f32>(-screen_delta.y, screen_delta.x) / segment_length;
    let half_width = camera.viewport_and_selection_width.z * 0.5;
    let offset_ndc = perpendicular * input.corner.y * half_width * 2.0 / viewport;
    let clip = mix(clip_start, clip_end, input.corner.x);
    let ndc = mix(ndc_start, ndc_end, input.corner.x) + offset_ndc;
    return vec4<f32>(ndc * clip.w, clip.z, clip.w);
}

@fragment fn fs_main() -> @location(0) vec4<f32> {
    return camera.selection_color;
}
