use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use clap::Parser;
use directories::ProjectDirs;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use yalive::db::Database;
use yalive::model::GraphData;

const SECTION_RADIUS: f32 = 13.0;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Markdown vault directory. Defaults to yalive's most recently opened vault.
    #[arg(short, long)]
    vault: Option<PathBuf>,
}

fn main() -> Result<()> {
    let vault = resolve_vault(Cli::parse().vault)?;
    let mut database = Database::open(&vault)?;
    database.index_vault(&vault)?;
    let graph = database.graph()?;
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut GraphApp::new(vault, graph))?;
    Ok(())
}

fn resolve_vault(vault: Option<PathBuf>) -> Result<PathBuf> {
    let path = if let Some(path) = vault {
        path
    } else {
        let directories = ProjectDirs::from("dev", "yalive", "yalive")
            .context("could not determine yalive configuration directory")?;
        let state = directories.config_dir().join("last-vault");
        PathBuf::from(
            fs::read_to_string(&state)
                .with_context(|| {
                    format!("reading {}; run yalive or pass --vault", state.display())
                })?
                .trim(),
        )
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("opening vault {}", path.display()))?;
    anyhow::ensure!(
        path.is_dir(),
        "vault is not a directory: {}",
        path.display()
    );
    Ok(path)
}

struct GraphApp {
    vault: PathBuf,
    layout: LayoutGraph,
    renderer: Option<Renderer>,
}

impl GraphApp {
    fn new(vault: PathBuf, graph: GraphData) -> Self {
        Self {
            vault,
            layout: LayoutGraph::new(graph),
            renderer: None,
        }
    }
}

impl ApplicationHandler for GraphApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("ygraphy")
            .with_inner_size(LogicalSize::new(1280, 800));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                match pollster::block_on(Renderer::new(Arc::new(window), &mut self.layout)) {
                    Ok(renderer) => self.renderer = Some(renderer),
                    Err(error) => {
                        eprintln!("could not initialize ygraphy: {error:#}");
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                eprintln!("could not create ygraphy window: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match code {
                KeyCode::Escape => event_loop.exit(),
                KeyCode::Space => {
                    self.layout.paused = !self.layout.paused;
                    renderer.window.request_redraw();
                }
                KeyCode::KeyF => {
                    renderer.fit(&self.layout);
                    renderer.window.request_redraw();
                }
                _ => {}
            },
            WindowEvent::CursorMoved { position, .. } => {
                renderer.pointer_moved(position, &mut self.layout);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(section) = renderer.pointer_button(state, &mut self.layout)
                    && let Err(error) = focus_in_tui(&self.vault, &self.layout.nodes[section].uid)
                {
                    eprintln!("could not focus section in yalive: {error:#}");
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                };
                renderer.zoom_at_pointer(amount);
                renderer.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.layout.tick();
                match renderer.render(&self.layout) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        renderer.resize(renderer.config.width, renderer.config.height)
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                    Err(wgpu::SurfaceError::Timeout) => {}
                }
                renderer.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn focus_in_tui(vault: &Path, uid: &str) -> Result<()> {
    let notes = vault.join(".notes");
    let pending = notes.join("ygraphy-open.pending");
    let command = notes.join("ygraphy-open.json");
    fs::write(&pending, serde_json::to_vec(uid)?)?;
    fs::rename(&pending, &command)?;
    Ok(())
}

#[derive(Clone)]
struct SectionNode {
    uid: String,
    heading: String,
    note: usize,
    position: [f32; 2],
    velocity: [f32; 2],
    fixed: bool,
}

struct NoteGroup {
    title: String,
    topic: usize,
    sections: Vec<usize>,
    center: [f32; 2],
    radius: f32,
}

struct TopicGroup {
    name: String,
    notes: Vec<usize>,
    center: [f32; 2],
    radius: f32,
}

struct LayoutGraph {
    nodes: Vec<SectionNode>,
    notes: Vec<NoteGroup>,
    topics: Vec<TopicGroup>,
    links: Vec<(usize, usize, [f32; 4])>,
    parent_links: Vec<(usize, usize)>,
    paused: bool,
    started: Instant,
}

impl LayoutGraph {
    fn new(graph: GraphData) -> Self {
        let mut topic_names = graph
            .notes
            .iter()
            .map(|note| {
                note.topic
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("Unsorted")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        topic_names.sort_by_key(|name| name.to_lowercase());
        topic_names.dedup();
        let topic_indices: HashMap<_, _> = topic_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect();
        let note_indices: HashMap<_, _> = graph
            .notes
            .iter()
            .enumerate()
            .map(|(index, note)| (note.id.clone(), index))
            .collect();
        let mut notes = graph
            .notes
            .iter()
            .map(|note| {
                let topic = note
                    .topic
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("Unsorted");
                NoteGroup {
                    title: note.title.clone(),
                    topic: topic_indices[topic],
                    sections: Vec::new(),
                    center: [0.0; 2],
                    radius: 45.0,
                }
            })
            .collect::<Vec<_>>();
        let mut topics = topic_names
            .into_iter()
            .map(|name| TopicGroup {
                name,
                notes: Vec::new(),
                center: [0.0; 2],
                radius: 90.0,
            })
            .collect::<Vec<_>>();
        for (index, note) in notes.iter().enumerate() {
            topics[note.topic].notes.push(index);
        }

        let mut nodes = Vec::with_capacity(graph.sections.len());
        let topic_count = topics.len().max(1) as f32;
        for (index, section) in graph.sections.iter().enumerate() {
            let note = note_indices[&section.note_id];
            notes[note].sections.push(index);
            let topic_angle = notes[note].topic as f32 / topic_count * std::f32::consts::TAU;
            let note_angle = hash_unit(&section.note_id) * std::f32::consts::TAU;
            let section_angle = hash_unit(&section.uid) * std::f32::consts::TAU;
            nodes.push(SectionNode {
                uid: section.uid.clone(),
                heading: section.heading.clone(),
                note,
                position: [
                    topic_angle.cos() * 340.0
                        + note_angle.cos() * 110.0
                        + section_angle.cos() * 30.0,
                    topic_angle.sin() * 340.0
                        + note_angle.sin() * 110.0
                        + section_angle.sin() * 30.0,
                ],
                velocity: [0.0; 2],
                fixed: false,
            });
        }
        let section_indices: HashMap<_, _> = graph
            .sections
            .iter()
            .enumerate()
            .map(|(index, section)| (section.uid.as_str(), index))
            .collect();
        let links = graph
            .links
            .iter()
            .filter_map(|link| {
                Some((
                    *section_indices.get(link.source.as_str())?,
                    *section_indices.get(link.target.as_str())?,
                    relation_color(&link.relation_type),
                ))
            })
            .collect();
        let parent_links = graph
            .sections
            .iter()
            .enumerate()
            .filter_map(|(child, section)| {
                Some((*section_indices.get(section.parent_uid.as_deref()?)?, child))
            })
            .collect();
        let mut layout = Self {
            nodes,
            notes,
            topics,
            links,
            parent_links,
            paused: false,
            started: Instant::now(),
        };
        layout.update_containers();
        layout
    }

    fn tick(&mut self) {
        if self.paused || self.nodes.is_empty() {
            return;
        }
        let cooling = (1.0 - self.started.elapsed().as_secs_f32() / 20.0).max(0.12);
        let mut forces = vec![[0.0f32; 2]; self.nodes.len()];
        for &(a, b, _) in &self.links {
            spring(&self.nodes, &mut forces, a, b, 150.0, 0.006 * cooling);
        }
        for &(a, b) in &self.parent_links {
            spring(&self.nodes, &mut forces, a, b, 65.0, 0.012 * cooling);
        }

        for a in 0..self.nodes.len() {
            for b in a + 1..self.nodes.len() {
                let delta = sub(self.nodes[a].position, self.nodes[b].position);
                let distance_sq = dot(delta, delta).max(16.0);
                if distance_sq < 90_000.0 {
                    let strength = 85.0 * cooling / distance_sq;
                    let direction = normalize_or_hash(delta, a, b);
                    add_scaled(&mut forces[a], direction, strength);
                    add_scaled(&mut forces[b], direction, -strength);
                }
            }
        }

        for note in &self.notes {
            if note.sections.is_empty() {
                continue;
            }
            let center = mean(
                note.sections
                    .iter()
                    .map(|&index| self.nodes[index].position),
            );
            for &index in &note.sections {
                add_scaled(
                    &mut forces[index],
                    sub(center, self.nodes[index].position),
                    0.003 * cooling,
                );
            }
        }
        self.update_containers();
        for topic in &self.topics {
            for &note_index in &topic.notes {
                let shift = sub(topic.center, self.notes[note_index].center);
                for &section in &self.notes[note_index].sections {
                    add_scaled(&mut forces[section], shift, 0.0007 * cooling);
                }
            }
        }
        for topic in &self.topics {
            for a in 0..topic.notes.len() {
                for b in a + 1..topic.notes.len() {
                    let note_a = topic.notes[a];
                    let note_b = topic.notes[b];
                    let delta = sub(self.notes[note_a].center, self.notes[note_b].center);
                    let distance = length(delta).max(1.0);
                    let overlap =
                        self.notes[note_a].radius + self.notes[note_b].radius + 22.0 - distance;
                    if overlap > 0.0 {
                        let direction = normalize_or_hash(delta, note_a, note_b);
                        translate_group(
                            &self.notes[note_a].sections,
                            &mut forces,
                            direction,
                            overlap * 0.002,
                        );
                        translate_group(
                            &self.notes[note_b].sections,
                            &mut forces,
                            direction,
                            -overlap * 0.002,
                        );
                    }
                }
            }
        }

        for (index, node) in self.nodes.iter_mut().enumerate() {
            if node.fixed {
                node.velocity = [0.0; 2];
                continue;
            }
            add_scaled(&mut forces[index], node.position, -0.00008);
            node.velocity[0] = (node.velocity[0] + forces[index][0]).clamp(-8.0, 8.0) * 0.88;
            node.velocity[1] = (node.velocity[1] + forces[index][1]).clamp(-8.0, 8.0) * 0.88;
            node.position[0] += node.velocity[0];
            node.position[1] += node.velocity[1];
        }
        self.update_containers();
    }

    fn update_containers(&mut self) {
        for note in &mut self.notes {
            if note.sections.is_empty() {
                continue;
            }
            note.center = mean(
                note.sections
                    .iter()
                    .map(|&index| self.nodes[index].position),
            );
            note.radius = note
                .sections
                .iter()
                .map(|&index| length(sub(self.nodes[index].position, note.center)) + SECTION_RADIUS)
                .fold(0.0, f32::max)
                .max(34.0)
                + 18.0;
        }
        for topic in &mut self.topics {
            let populated = topic
                .notes
                .iter()
                .copied()
                .filter(|&index| !self.notes[index].sections.is_empty());
            let indices = populated.collect::<Vec<_>>();
            if indices.is_empty() {
                continue;
            }
            topic.center = mean(indices.iter().map(|&index| self.notes[index].center));
            topic.radius = indices
                .iter()
                .map(|&index| {
                    length(sub(self.notes[index].center, topic.center)) + self.notes[index].radius
                })
                .fold(0.0, f32::max)
                + 28.0;
        }
    }

    fn bounds(&self) -> Option<([f32; 2], [f32; 2])> {
        let first = self.nodes.first()?.position;
        let mut min = first;
        let mut max = first;
        for node in &self.nodes {
            min[0] = min[0].min(node.position[0] - SECTION_RADIUS);
            min[1] = min[1].min(node.position[1] - SECTION_RADIUS);
            max[0] = max[0].max(node.position[0] + SECTION_RADIUS);
            max[1] = max[1].max(node.position[1] + SECTION_RADIUS);
        }
        Some((min, max))
    }
}

fn spring(
    nodes: &[SectionNode],
    forces: &mut [[f32; 2]],
    a: usize,
    b: usize,
    target: f32,
    strength: f32,
) {
    let delta = sub(nodes[b].position, nodes[a].position);
    let distance = length(delta).max(0.01);
    let force = (distance - target) * strength;
    add_scaled(&mut forces[a], delta, force / distance);
    add_scaled(&mut forces[b], delta, -force / distance);
}

fn translate_group(indices: &[usize], forces: &mut [[f32; 2]], direction: [f32; 2], amount: f32) {
    for &index in indices {
        add_scaled(&mut forces[index], direction, amount);
    }
}

fn mean(points: impl Iterator<Item = [f32; 2]>) -> [f32; 2] {
    let (sum, count) = points.fold(([0.0, 0.0], 0usize), |(mut sum, count), point| {
        sum[0] += point[0];
        sum[1] += point[1];
        (sum, count + 1)
    });
    if count == 0 {
        [0.0; 2]
    } else {
        [sum[0] / count as f32, sum[1] / count as f32]
    }
}

fn hash_unit(value: &str) -> f32 {
    let hash = value.bytes().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    hash as f32 / u32::MAX as f32
}

fn relation_color(kind: &str) -> [f32; 4] {
    match kind {
        "supports" => [0.30, 0.82, 0.55, 0.72],
        "contradicts" => [0.95, 0.34, 0.34, 0.76],
        "example-of" => [0.96, 0.68, 0.25, 0.72],
        "prerequisite" => [0.68, 0.48, 0.96, 0.76],
        _ => [0.40, 0.68, 0.92, 0.62],
    }
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
fn length(value: [f32; 2]) -> f32 {
    dot(value, value).sqrt()
}
fn normalize_or_hash(value: [f32; 2], a: usize, b: usize) -> [f32; 2] {
    let distance = length(value);
    if distance > 0.001 {
        [value[0] / distance, value[1] / distance]
    } else {
        let angle =
            ((a.wrapping_mul(31) + b.wrapping_mul(17)) % 360) as f32 * std::f32::consts::PI / 180.0;
        [angle.cos(), angle.sin()]
    }
}
fn add_scaled(target: &mut [f32; 2], value: [f32; 2], scale: f32) {
    target[0] += value[0] * scale;
    target[1] += value[1] * scale;
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    center: [f32; 2],
    viewport: [f32; 2],
    zoom: f32,
    padding: [f32; 7],
}

const _: () = assert!(std::mem::size_of::<CameraUniform>() == 48);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CircleInstance {
    center_radius: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LineInstance {
    endpoints: [f32; 4],
    color: [f32; 4],
    width: f32,
}

struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    camera: CameraUniform,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    circle_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    font_system: FontSystem,
    swash_cache: SwashCache,
    text_viewport: Viewport,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    pointer: PhysicalPosition<f64>,
    drag: Option<Drag>,
    last_click: Option<(Instant, usize)>,
    selected: Option<usize>,
    window: Arc<Window>,
}

enum Drag {
    Section(usize),
    Canvas {
        start: PhysicalPosition<f64>,
        center: [f32; 2],
    },
}

impl Renderer {
    async fn new(window: Arc<Window>, layout: &mut LayoutGraph) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("no compatible graphics adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let camera = CameraUniform {
            center: [0.0; 2],
            viewport: [config.width as f32, config.height as f32],
            zoom: 1.0,
            padding: [0.0; 7],
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera"),
            contents: bytemuck::bytes_of(&camera),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("graph.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("graph pipeline layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });
        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        let circle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("circle pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("circle_vertex"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CircleInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("circle_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("line_vertex"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 32,
                            shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("line_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let text_viewport = Viewport::new(&device, &cache);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );
        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            camera,
            camera_buffer,
            camera_bind_group,
            circle_pipeline,
            line_pipeline,
            font_system,
            swash_cache,
            text_viewport,
            text_atlas,
            text_renderer,
            pointer: PhysicalPosition::new(0.0, 0.0),
            drag: None,
            last_click: None,
            selected: None,
            window,
        };
        renderer.fit(layout);
        renderer.window.request_redraw();
        Ok(renderer)
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.camera.viewport = [width as f32, height as f32];
        self.surface.configure(&self.device, &self.config);
        self.window.request_redraw();
    }

    fn fit(&mut self, layout: &LayoutGraph) {
        let Some((min, max)) = layout.bounds() else {
            return;
        };
        self.camera.center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        let width = (max[0] - min[0] + 100.0).max(1.0);
        let height = (max[1] - min[1] + 100.0).max(1.0);
        self.camera.zoom = (self.config.width as f32 / width)
            .min(self.config.height as f32 / height)
            .clamp(0.08, 4.0);
    }

    fn screen_to_world(&self, position: PhysicalPosition<f64>) -> [f32; 2] {
        [
            self.camera.center[0]
                + (position.x as f32 - self.config.width as f32 * 0.5) / self.camera.zoom,
            self.camera.center[1]
                + (position.y as f32 - self.config.height as f32 * 0.5) / self.camera.zoom,
        ]
    }

    fn world_to_screen(&self, position: [f32; 2]) -> [f32; 2] {
        [
            (position[0] - self.camera.center[0]) * self.camera.zoom
                + self.config.width as f32 * 0.5,
            (position[1] - self.camera.center[1]) * self.camera.zoom
                + self.config.height as f32 * 0.5,
        ]
    }

    fn hit_section(&self, layout: &LayoutGraph) -> Option<usize> {
        let world = self.screen_to_world(self.pointer);
        layout
            .nodes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, node)| {
                (length(sub(node.position, world)) <= SECTION_RADIUS + 5.0 / self.camera.zoom)
                    .then_some(index)
            })
    }

    fn pointer_moved(&mut self, position: PhysicalPosition<f64>, layout: &mut LayoutGraph) {
        self.pointer = position;
        match self.drag {
            Some(Drag::Section(index)) => {
                layout.nodes[index].position = self.screen_to_world(position);
                layout.nodes[index].velocity = [0.0; 2];
                layout.update_containers();
            }
            Some(Drag::Canvas { start, center }) => {
                self.camera.center = [
                    center[0] - (position.x - start.x) as f32 / self.camera.zoom,
                    center[1] - (position.y - start.y) as f32 / self.camera.zoom,
                ];
            }
            None => self.selected = self.hit_section(layout),
        }
        self.window.request_redraw();
    }

    fn pointer_button(&mut self, state: ElementState, layout: &mut LayoutGraph) -> Option<usize> {
        match state {
            ElementState::Pressed => {
                if let Some(index) = self.hit_section(layout) {
                    layout.nodes[index].fixed = true;
                    self.drag = Some(Drag::Section(index));
                    self.selected = Some(index);
                } else {
                    self.drag = Some(Drag::Canvas {
                        start: self.pointer,
                        center: self.camera.center,
                    });
                }
                None
            }
            ElementState::Released => {
                let section = match self.drag.take() {
                    Some(Drag::Section(index)) => {
                        layout.nodes[index].fixed = false;
                        Some(index)
                    }
                    _ => None,
                };
                let now = Instant::now();
                let double_clicked = section.and_then(|index| {
                    self.last_click
                        .filter(|(at, previous)| {
                            *previous == index
                                && now.duration_since(*at) <= Duration::from_millis(450)
                        })
                        .map(|_| index)
                });
                self.last_click = section.map(|index| (now, index));
                double_clicked
            }
        }
    }

    fn zoom_at_pointer(&mut self, amount: f32) {
        let before = self.screen_to_world(self.pointer);
        self.camera.zoom = (self.camera.zoom * (amount * 0.12).exp()).clamp(0.04, 12.0);
        let after = self.screen_to_world(self.pointer);
        self.camera.center[0] += before[0] - after[0];
        self.camera.center[1] += before[1] - after[1];
    }

    fn render(&mut self, layout: &LayoutGraph) -> std::result::Result<(), wgpu::SurfaceError> {
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&self.camera));
        self.text_viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );

        let mut circles =
            Vec::with_capacity(layout.topics.len() + layout.notes.len() + layout.nodes.len());
        for topic in &layout.topics {
            circles.push(CircleInstance {
                center_radius: [topic.center[0], topic.center[1], topic.radius, 0.0],
                color: [0.19, 0.30, 0.40, 0.22],
            });
        }
        for (index, note) in layout.notes.iter().enumerate() {
            let color = palette(index);
            circles.push(CircleInstance {
                center_radius: [note.center[0], note.center[1], note.radius, 0.0],
                color: [color[0], color[1], color[2], 0.28],
            });
        }
        for (index, node) in layout.nodes.iter().enumerate() {
            let mut color = palette(node.note);
            if self.selected == Some(index) {
                color = [1.0, 0.83, 0.35, 1.0];
            }
            circles.push(CircleInstance {
                center_radius: [node.position[0], node.position[1], SECTION_RADIUS, 0.0],
                color,
            });
        }
        let lines = layout
            .links
            .iter()
            .map(|&(a, b, color)| LineInstance {
                endpoints: [
                    layout.nodes[a].position[0],
                    layout.nodes[a].position[1],
                    layout.nodes[b].position[0],
                    layout.nodes[b].position[1],
                ],
                color,
                width: 1.25,
            })
            .chain(layout.parent_links.iter().map(|&(a, b)| LineInstance {
                endpoints: [
                    layout.nodes[a].position[0],
                    layout.nodes[a].position[1],
                    layout.nodes[b].position[0],
                    layout.nodes[b].position[1],
                ],
                color: [0.55, 0.60, 0.66, 0.34],
                width: 0.8,
            }))
            .collect::<Vec<_>>();
        let circle_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("circle instances"),
                contents: bytemuck::cast_slice(&circles),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let line_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("line instances"),
                contents: bytemuck::cast_slice(&lines),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut labels = Vec::<(Buffer, f32, f32, Color)>::new();
        if self.camera.zoom >= 0.45 {
            for topic in &layout.topics {
                let position = self.world_to_screen([
                    topic.center[0] - topic.radius * 0.62,
                    topic.center[1] - topic.radius * 0.72,
                ]);
                labels.push(make_label(
                    &mut self.font_system,
                    &topic.name,
                    18.0,
                    position,
                    Color::rgb(142, 190, 218),
                ));
            }
        }
        if self.camera.zoom >= 0.72 {
            for note in &layout.notes {
                let position = self.world_to_screen([
                    note.center[0] - note.radius * 0.55,
                    note.center[1] - note.radius * 0.62,
                ]);
                labels.push(make_label(
                    &mut self.font_system,
                    &note.title,
                    14.0,
                    position,
                    Color::rgb(208, 220, 228),
                ));
            }
        }
        if self.camera.zoom >= 1.1 {
            for (index, node) in layout.nodes.iter().enumerate() {
                if self.camera.zoom >= 1.5 || self.selected == Some(index) {
                    let position = self.world_to_screen([
                        node.position[0] + SECTION_RADIUS + 4.0 / self.camera.zoom,
                        node.position[1] - 7.0 / self.camera.zoom,
                    ]);
                    labels.push(make_label(
                        &mut self.font_system,
                        &node.heading,
                        12.0,
                        position,
                        Color::rgb(231, 236, 239),
                    ));
                }
            }
        }
        let text_areas = labels
            .iter()
            .map(|(buffer, left, top, color)| TextArea {
                buffer,
                left: *left,
                top: *top,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: *color,
                custom_glyphs: &[],
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.text_viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            eprintln!("text preparation failed: {error}");
        }

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ygraphy frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("graph pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            if !lines.is_empty() {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, line_buffer.slice(..));
                pass.draw(0..6, 0..lines.len() as u32);
            }
            if !circles.is_empty() {
                pass.set_pipeline(&self.circle_pipeline);
                pass.set_vertex_buffer(0, circle_buffer.slice(..));
                pass.draw(0..6, 0..circles.len() as u32);
            }
            if let Err(error) =
                self.text_renderer
                    .render(&self.text_atlas, &self.text_viewport, &mut pass)
            {
                eprintln!("text rendering failed: {error}");
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text_atlas.trim();
        Ok(())
    }
}

fn make_label(
    font_system: &mut FontSystem,
    text: &str,
    size: f32,
    position: [f32; 2],
    color: Color,
) -> (Buffer, f32, f32, Color) {
    let mut buffer = Buffer::new(font_system, Metrics::new(size, size * 1.25));
    buffer.set_size(font_system, Some(420.0), Some(size * 1.5));
    buffer.set_text(
        font_system,
        text,
        Attrs::new().family(Family::SansSerif),
        Shaping::Advanced,
    );
    buffer.shape_until_scroll(font_system, false);
    (buffer, position[0], position[1], color)
}

fn palette(index: usize) -> [f32; 4] {
    const COLORS: [[f32; 4]; 8] = [
        [0.28, 0.72, 0.88, 0.94],
        [0.88, 0.48, 0.42, 0.94],
        [0.45, 0.78, 0.52, 0.94],
        [0.72, 0.54, 0.91, 0.94],
        [0.94, 0.69, 0.32, 0.94],
        [0.31, 0.78, 0.72, 0.94],
        [0.88, 0.43, 0.68, 0.94],
        [0.61, 0.67, 0.91, 0.94],
    ];
    COLORS[index % COLORS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use yalive::model::{GraphLink, GraphNote, GraphSection};

    #[test]
    fn containment_circles_cover_children() {
        let graph = GraphData {
            notes: vec![GraphNote {
                id: "n".into(),
                title: "Note".into(),
                topic: Some("Topic".into()),
                path: "n.md".into(),
            }],
            sections: vec![
                GraphSection {
                    uid: "n#a".into(),
                    note_id: "n".into(),
                    heading: "A".into(),
                    parent_uid: None,
                    level: 1,
                    start_line: 1,
                },
                GraphSection {
                    uid: "n#b".into(),
                    note_id: "n".into(),
                    heading: "B".into(),
                    parent_uid: Some("n#a".into()),
                    level: 2,
                    start_line: 2,
                },
            ],
            links: vec![GraphLink {
                source: "n#a".into(),
                target: "n#b".into(),
                relation_type: "supports".into(),
            }],
        };
        let layout = LayoutGraph::new(graph);
        let note = &layout.notes[0];
        assert!(note.sections.iter().all(|&index| length(sub(
            layout.nodes[index].position,
            note.center
        )) + SECTION_RADIUS
            <= note.radius));
        let topic = &layout.topics[0];
        assert!(length(sub(note.center, topic.center)) + note.radius <= topic.radius);
    }
}
