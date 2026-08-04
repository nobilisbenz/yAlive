struct Camera {
    center: vec2<f32>,
    viewport: vec2<f32>,
    zoom: f32,
    _padding: vec3<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;

struct CircleInstance {
    center_radius: vec4<f32>,
    color: vec4<f32>,
}

struct CircleOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
}

fn corner(index: u32) -> vec2<f32> {
    let corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0),
    );
    return corners[index];
}

fn world_to_clip(world: vec2<f32>) -> vec4<f32> {
    let offset = (world - camera.center) * camera.zoom;
    return vec4(offset.x * 2.0 / camera.viewport.x, -offset.y * 2.0 / camera.viewport.y, 0.0, 1.0);
}

@vertex
fn circle_vertex(
    @builtin(vertex_index) vertex: u32,
    @location(0) center_radius: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> CircleOut {
    let local = corner(vertex);
    var out: CircleOut;
    out.position = world_to_clip(center_radius.xy + local * center_radius.z);
    out.local = local;
    out.color = color;
    return out;
}

@fragment
fn circle_fragment(in: CircleOut) -> @location(0) vec4<f32> {
    let distance = length(in.local);
    if distance > 1.0 { discard; }
    let edge = 1.0 - smoothstep(0.92, 1.0, distance);
    return vec4(in.color.rgb, in.color.a * (0.34 + edge * 0.66));
}

struct LineOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn line_vertex(
    @builtin(vertex_index) vertex: u32,
    @location(0) endpoints: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) width: f32,
) -> LineOut {
    let along = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    let side = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    let delta = endpoints.zw - endpoints.xy;
    let normal = normalize(vec2(-delta.y, delta.x));
    let world = mix(endpoints.xy, endpoints.zw, along[vertex])
        + normal * side[vertex] * width / camera.zoom;
    var out: LineOut;
    out.position = world_to_clip(world);
    out.color = color;
    return out;
}

@fragment
fn line_fragment(in: LineOut) -> @location(0) vec4<f32> {
    return in.color;
}
