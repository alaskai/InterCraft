// 3D viewport. Renders the part meshes and their edges to an offscreen texture
// (4x MSAA + depth) and shows that texture inside an egui panel. Also projects
// points to the screen and picks corners, edges and surface points under the
// cursor for the measuring tool. Z is up, units are mm.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec2, Vec3, Vec4};
use intercraft_geo::Mesh;
use wgpu::util::DeviceExt;

// Plain Unorm: the shader writes display-space colours and egui shows them as they are.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;
const EDGE_COLOR: [f32; 3] = [0.05, 0.06, 0.08];

/// How close (in points) the cursor must be to snap to a corner or an edge.
const CORNER_SNAP: f32 = 9.0;
const EDGE_SNAP: f32 = 7.0;

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    light_a: vec4<f32>,
    light_b: vec4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>) -> VsOut {
    var out: VsOut;
    out.pos = globals.view_proj * vec4<f32>(pos, 1.0);
    out.normal = normal;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    if (!front) { n = -n; }
    let key = max(dot(n, globals.light_a.xyz), 0.0);
    let fill = max(dot(n, globals.light_b.xyz), 0.0);
    let shade = 0.30 + 0.70 * key + 0.20 * fill;
    return vec4<f32>(in.color * shade, 1.0);
}

@fragment
fn fs_line(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
"#;

/// Right-handed view matrix.
fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    let f = (target - eye).normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols(
        Vec4::new(s.x, u.x, -f.x, 0.0),
        Vec4::new(s.y, u.y, -f.y, 0.0),
        Vec4::new(s.z, u.z, -f.z, 0.0),
        Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
    )
}

/// Right-handed perspective with wgpu's 0..1 depth range.
fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    let r = far / (near - far);
    Mat4::from_cols(
        Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        Vec4::new(0.0, f, 0.0, 0.0),
        Vec4::new(0.0, 0.0, r, -1.0),
        Vec4::new(0.0, 0.0, r * near, 0.0),
    )
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_a: [f32; 4],
    light_b: [f32; 4],
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    edges: Option<wgpu::Buffer>,
    edge_vertex_count: u32,
    bounds: (Vec3, Vec3),
    visible: bool,
    /// CPU copy for picking and highlighting.
    cpu: Arc<Mesh>,
}

struct Targets {
    size: [u32; 2],
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
}

/// The camera as last drawn, for projecting points and casting rays.
#[derive(Clone, Copy)]
struct Frame {
    view_proj: Mat4,
    inverse: Mat4,
    rect: egui::Rect,
}

impl Frame {
    fn project(&self, p: Vec3) -> Option<egui::Pos2> {
        let clip = self.view_proj * p.extend(1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(egui::pos2(
            self.rect.left() + (ndc.x + 1.0) * 0.5 * self.rect.width(),
            self.rect.top() + (1.0 - ndc.y) * 0.5 * self.rect.height(),
        ))
    }

    fn ray(&self, cursor: egui::Pos2) -> (Vec3, Vec3) {
        let x = (cursor.x - self.rect.left()) / self.rect.width() * 2.0 - 1.0;
        let y = 1.0 - (cursor.y - self.rect.top()) / self.rect.height() * 2.0;
        let near = self.inverse * Vec4::new(x, y, 0.0, 1.0);
        let far = self.inverse * Vec4::new(x, y, 1.0, 1.0);
        let near = near.truncate() / near.w;
        let far = far.truncate() / far.w;
        (near, (far - near).normalize())
    }
}

/// What the cursor is over, snapped to the nearest corner or edge when close enough.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub part: String,
    pub target: Target,
    /// The snapped point in model space (mm).
    pub pos: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Corner(usize),
    Edge(usize),
    Surface,
}

/// Mouse input the viewport didn't use itself (clicks become picks).
pub struct ViewportInput {
    pub rect: egui::Rect,
    pub clicked: Option<egui::Pos2>,
    pub hover: Option<egui::Pos2>,
}

pub struct Viewport {
    rs: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    targets: Option<Targets>,
    texture_id: Option<egui::TextureId>,
    meshes: BTreeMap<String, GpuMesh>,
    pub show_edges: bool,
    frame: Option<Frame>,

    // Orbit camera
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Viewport {
    pub fn new(rs: egui_wgpu::RenderState) -> Self {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("intercraft mesh shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        // One explicit layout, so both pipelines can share the same bind group.
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("intercraft globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("intercraft pipeline layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let vertex_layout = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
        })];
        let make = |label, topology, fragment, bias| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &vertex_layout,
                },
                // No culling, so a part with flipped faces still shows up.
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias,
                }),
                multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(COLOR_FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // Faces are pushed slightly back so the edges drawn on them always win.
        let face_bias = wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 };
        let pipeline = make("intercraft faces", wgpu::PrimitiveTopology::TriangleList, "fs_main", face_bias);
        let line_pipeline =
            make("intercraft edges", wgpu::PrimitiveTopology::LineList, "fs_line", wgpu::DepthBiasState::default());

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("intercraft globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("intercraft globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });

        Self {
            rs,
            pipeline,
            line_pipeline,
            globals,
            bind_group,
            targets: None,
            texture_id: None,
            meshes: BTreeMap::new(),
            show_edges: true,
            frame: None,
            target: Vec3::ZERO,
            yaw: -0.9,
            pitch: 0.5,
            distance: 150.0,
        }
    }

    pub fn set_mesh(&mut self, name: &str, mesh: Mesh, color: [f32; 3]) {
        let Some((lo, hi)) = mesh.bounds() else {
            self.meshes.remove(name);
            return;
        };
        let vertices: Vec<Vertex> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&pos, &normal)| Vertex { pos, normal, color })
            .collect();
        let edge_vertices: Vec<Vertex> = mesh
            .edges
            .iter()
            .flat_map(|line| line.windows(2))
            .flat_map(|pair| pair.iter().map(|&pos| Vertex { pos, normal: [0.0; 3], color: EDGE_COLOR }))
            .collect();

        let device = &self.rs.device;
        let first = self.meshes.is_empty();
        let visible = self.meshes.get(name).is_none_or(|m| m.visible);
        let edges = (!edge_vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(name),
                contents: bytemuck::cast_slice(&edge_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.meshes.insert(
            name.to_owned(),
            GpuMesh {
                vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(name),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(name),
                    contents: bytemuck::cast_slice(&mesh.indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
                index_count: mesh.indices.len() as u32,
                edges,
                edge_vertex_count: edge_vertices.len() as u32,
                bounds: (lo.into(), hi.into()),
                visible,
                cpu: Arc::new(mesh),
            },
        );
        if first {
            self.fit();
        }
    }

    pub fn retain_meshes(&mut self, keep: impl Fn(&str) -> bool) {
        self.meshes.retain(|name, _| keep(name));
    }

    pub fn set_visible(&mut self, name: &str, visible: bool) {
        if let Some(mesh) = self.meshes.get_mut(name) {
            mesh.visible = visible;
        }
    }

    /// Frame everything that is visible.
    pub fn fit(&mut self) {
        let mut bounds: Option<(Vec3, Vec3)> = None;
        for mesh in self.meshes.values().filter(|m| m.visible) {
            bounds = Some(match bounds {
                None => mesh.bounds,
                Some((lo, hi)) => (lo.min(mesh.bounds.0), hi.max(mesh.bounds.1)),
            });
        }
        if let Some((lo, hi)) = bounds {
            self.target = (lo + hi) * 0.5;
            self.distance = ((hi - lo).length() * 1.4).max(1.0);
        }
    }

    fn eye_offset(&self) -> Vec3 {
        Vec3::new(
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        ) * self.distance
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> ViewportInput {
        // Not focusable: the viewport takes mouse input only, so egui draws no focus ring.
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::CLICK | egui::Sense::DRAG);
        let mut input = ViewportInput { rect, clicked: None, hover: None };
        if rect.width() < 2.0 || rect.height() < 2.0 {
            return input;
        }

        // Camera basis, used for panning and the axis triad.
        let back = self.eye_offset().normalize();
        let right = Vec3::Z.cross(back).normalize();
        let up = back.cross(right);

        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.yaw -= d.x * 0.008;
            self.pitch = (self.pitch + d.y * 0.008).clamp(-1.55, 1.55);
        }
        if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
            let d = response.drag_delta();
            let mm_per_point = self.distance * 0.82 / rect.height();
            self.target += (-right * d.x + up * d.y) * mm_per_point;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            self.distance = (self.distance * (-scroll * 0.002).exp()).clamp(0.5, 100_000.0);
            input.hover = response.hover_pos();
        }
        if response.double_clicked() {
            self.fit();
        } else if response.clicked() {
            input.clicked = response.interact_pointer_pos();
        }

        let ppp = ui.ctx().pixels_per_point();
        let size = [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ];
        self.ensure_targets(size);
        self.render(rect, ui.visuals().dark_mode);

        if let Some(id) = self.texture_id {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
        }
        self.draw_triad(ui, rect, right, up);
        input
    }

    fn ensure_targets(&mut self, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let device = &self.rs.device;
        let make = |label, format, sample_count, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let targets = Targets {
            size,
            msaa_view: make("viewport msaa", COLOR_FORMAT, SAMPLES, attach),
            depth_view: make("viewport depth", DEPTH_FORMAT, SAMPLES, attach),
            resolve_view: make("viewport colour", COLOR_FORMAT, 1, attach | wgpu::TextureUsages::TEXTURE_BINDING),
        };

        let mut renderer = self.rs.renderer.write();
        match self.texture_id {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &targets.resolve_view, wgpu::FilterMode::Linear, id)
            }
            None => {
                self.texture_id =
                    Some(renderer.register_native_texture(device, &targets.resolve_view, wgpu::FilterMode::Linear));
            }
        }
        drop(renderer);
        self.targets = Some(targets);
    }

    fn render(&mut self, rect: egui::Rect, dark: bool) {
        let Some(targets) = &self.targets else { return };

        let offset = self.eye_offset();
        let eye = self.target + offset;
        let view = look_at(eye, self.target, Vec3::Z);
        let near = (self.distance * 0.01).max(0.01);
        let far = self.distance * 20.0 + 1000.0;
        let proj = perspective(45f32.to_radians(), rect.width() / rect.height(), near, far);
        let view_proj = proj * view;
        self.frame = Some(Frame { view_proj, inverse: view_proj.inverse(), rect });

        // Lights follow the camera so the part never goes dark while orbiting.
        let back = offset.normalize();
        let right = Vec3::Z.cross(back).normalize();
        let up = back.cross(right);
        let light_a = (back + right * 0.5 + up * 0.7).normalize();
        let light_b = (back - right * 0.8 - up * 0.3).normalize();

        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            light_a: light_a.extend(0.0).to_array(),
            light_b: light_b.extend(0.0).to_array(),
        };
        self.rs.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        let bg = if dark { 0.11 } else { 0.86 };
        let mut encoder = self.rs.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("intercraft viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: bg, g: bg, b: bg, a: 1.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_pipeline(&self.pipeline);
            for mesh in self.meshes.values().filter(|m| m.visible) {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
            if self.show_edges {
                pass.set_pipeline(&self.line_pipeline);
                for mesh in self.meshes.values().filter(|m| m.visible) {
                    if let Some(edges) = &mesh.edges {
                        pass.set_vertex_buffer(0, edges.slice(..));
                        pass.draw(0..mesh.edge_vertex_count, 0..1);
                    }
                }
            }
        }
        self.rs.queue.submit([encoder.finish()]);
    }

    /// Small XYZ triad in the corner so the frame is always visible.
    fn draw_triad(&self, ui: &egui::Ui, rect: egui::Rect, right: Vec3, up: Vec3) {
        let origin = rect.left_bottom() + egui::vec2(36.0, -36.0);
        let painter = ui.painter_at(rect);
        let axes = [
            (Vec3::X, "X", egui::Color32::from_rgb(220, 70, 70)),
            (Vec3::Y, "Y", egui::Color32::from_rgb(70, 180, 80)),
            (Vec3::Z, "Z", egui::Color32::from_rgb(80, 120, 230)),
        ];
        for (axis, name, color) in axes {
            let tip = origin + egui::vec2(axis.dot(right), -axis.dot(up)) * 24.0;
            painter.line_segment([origin, tip], egui::Stroke::new(2.0, color));
            painter.text(tip + (tip - origin) * 0.3, egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(11.0), color);
        }
    }

    // ---------- projection, picking and overlay drawing ----------

    /// Where a model-space point appears on screen, if it's in front of the camera.
    pub fn project(&self, p: [f32; 3]) -> Option<egui::Pos2> {
        self.frame?.project(Vec3::from(p))
    }

    /// The corner, edge or surface point under the cursor, snapping to corners and
    /// edges within a few points. Hidden geometry is ignored.
    pub fn pick(&self, cursor: egui::Pos2) -> Option<Hit> {
        let frame = self.frame?;
        if !frame.rect.contains(cursor) {
            return None;
        }
        let (origin, dir) = frame.ray(cursor);
        let visible = || self.meshes.iter().filter(|(_, m)| m.visible);

        // Nearest surface along the ray, used both as a fallback pick and to hide
        // corners and edges that sit behind other geometry.
        let mut surface: Option<(f32, &str)> = None;
        for (name, mesh) in visible() {
            let p = &mesh.cpu.positions;
            for tri in mesh.cpu.indices.chunks_exact(3) {
                let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| Vec3::from(p[i as usize]));
                if let Some(t) = ray_triangle(origin, dir, a, b, c) {
                    if surface.is_none_or(|(best, _)| t < best) {
                        surface = Some((t, name));
                    }
                }
            }
        }
        let in_view = |p: Vec3| {
            let along = (p - origin).dot(dir);
            surface.is_none_or(|(t, _)| along <= t + 0.002 * t + 0.05)
        };
        let cursor_v = Vec2::new(cursor.x, cursor.y);
        let to_screen = |p: Vec3| frame.project(p).map(|s| Vec2::new(s.x, s.y));

        let mut best: Option<(f32, Hit)> = None;
        for (name, mesh) in visible() {
            for (i, &corner) in mesh.cpu.corners.iter().enumerate() {
                let p = Vec3::from(corner);
                let Some(s) = to_screen(p) else { continue };
                let d = s.distance(cursor_v);
                if d < CORNER_SNAP && in_view(p) && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((d, Hit { part: name.clone(), target: Target::Corner(i), pos: corner }));
                }
            }
        }
        if let Some((_, hit)) = best {
            return Some(hit);
        }

        for (name, mesh) in visible() {
            for (i, line) in mesh.cpu.edges.iter().enumerate() {
                for pair in line.windows(2) {
                    let (p0, p1) = (Vec3::from(pair[0]), Vec3::from(pair[1]));
                    let (Some(s0), Some(s1)) = (to_screen(p0), to_screen(p1)) else { continue };
                    let seg = s1 - s0;
                    let t = if seg.length_squared() > 0.0 {
                        ((cursor_v - s0).dot(seg) / seg.length_squared()).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = (s0 + seg * t).distance(cursor_v);
                    let p = p0.lerp(p1, t);
                    if d < EDGE_SNAP && in_view(p) && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                        best = Some((d, Hit { part: name.clone(), target: Target::Edge(i), pos: p.to_array() }));
                    }
                }
            }
        }
        if let Some((_, hit)) = best {
            return Some(hit);
        }

        surface.map(|(t, name)| Hit { part: name.to_owned(), target: Target::Surface, pos: (origin + dir * t).to_array() })
    }

    /// The polyline of one edge, for highlighting.
    pub fn edge_points(&self, part: &str, edge: usize) -> Option<&[[f32; 3]]> {
        self.meshes.get(part)?.cpu.edges.get(edge).map(Vec::as_slice)
    }

    /// Draw a highlighted pick on top of the 3D view.
    pub fn draw_hit(&self, painter: &egui::Painter, hit: &Hit, color: egui::Color32) {
        match hit.target {
            Target::Edge(i) => {
                if let Some(line) = self.edge_points(&hit.part, i) {
                    let points: Vec<egui::Pos2> = line.iter().filter_map(|&p| self.project(p)).collect();
                    painter.add(egui::Shape::line(points, egui::Stroke::new(3.0, color)));
                }
            }
            Target::Corner(_) => {
                if let Some(s) = self.project(hit.pos) {
                    painter.circle(s, 5.0, color, egui::Stroke::new(1.5, egui::Color32::WHITE));
                }
            }
            Target::Surface => {
                if let Some(s) = self.project(hit.pos) {
                    let r = 5.0;
                    let stroke = egui::Stroke::new(2.0, color);
                    painter.line_segment([s - egui::vec2(r, 0.0), s + egui::vec2(r, 0.0)], stroke);
                    painter.line_segment([s - egui::vec2(0.0, r), s + egui::vec2(0.0, r)], stroke);
                }
            }
        }
    }
}

/// Möller-Trumbore ray/triangle test. Returns the distance along the ray.
fn ray_triangle(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 1e-6).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_hits_a_triangle_in_front() {
        let (a, b, c) = (Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        let t = ray_triangle(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, -1.0), a, b, c);
        assert!((t.unwrap() - 5.0).abs() < 1e-5);
        assert!(ray_triangle(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 1.0), a, b, c).is_none(), "hit behind");
        assert!(ray_triangle(Vec3::new(5.0, 5.0, 5.0), Vec3::new(0.0, 0.0, -1.0), a, b, c).is_none(), "hit outside");
    }

    #[test]
    fn projection_and_ray_agree() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
        let view_proj = perspective(45f32.to_radians(), 800.0 / 600.0, 0.1, 1000.0)
            * look_at(Vec3::new(50.0, -80.0, 60.0), Vec3::ZERO, Vec3::Z);
        let frame = Frame { view_proj, inverse: view_proj.inverse(), rect };
        let point = Vec3::new(3.0, -4.0, 7.0);
        let screen = frame.project(point).unwrap();
        let (origin, dir) = frame.ray(screen);
        // The ray through the projected point passes through the point.
        let closest = origin + dir * (point - origin).dot(dir);
        assert!(closest.distance(point) < 1e-2, "off by {}", closest.distance(point));
    }
}
