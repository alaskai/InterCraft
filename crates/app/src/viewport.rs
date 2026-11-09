// 3D viewport. Renders the part meshes to an offscreen texture (4x MSAA + depth)
// and shows that texture inside an egui panel. Z is up, units are mm.

use std::collections::BTreeMap;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3, Vec4};
use intercraft_geo::Mesh;
use wgpu::util::DeviceExt;

// Plain Unorm: the shader writes display-space colours and egui shows them as they are.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;

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
    bounds: (Vec3, Vec3),
    visible: bool,
}

struct Targets {
    size: [u32; 2],
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
}

pub struct Viewport {
    rs: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    targets: Option<Targets>,
    texture_id: Option<egui::TextureId>,
    meshes: BTreeMap<String, GpuMesh>,

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

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("intercraft mesh pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
                })],
            },
            // No culling, so a part with flipped faces still shows up.
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(COLOR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("intercraft globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("intercraft globals"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });

        Self {
            rs,
            pipeline,
            globals,
            bind_group,
            targets: None,
            texture_id: None,
            meshes: BTreeMap::new(),
            target: Vec3::ZERO,
            yaw: -0.9,
            pitch: 0.5,
            distance: 150.0,
        }
    }

    pub fn set_mesh(&mut self, name: &str, mesh: &Mesh, color: [f32; 3]) {
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

        let device = &self.rs.device;
        let first = self.meshes.is_empty();
        let visible = self.meshes.get(name).map_or(true, |m| m.visible);
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
                bounds: (lo.into(), hi.into()),
                visible,
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

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        if rect.width() < 2.0 || rect.height() < 2.0 {
            return;
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
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            let d = response.drag_delta();
            let mm_per_point = self.distance * 0.82 / rect.height();
            self.target += (-right * d.x + up * d.y) * mm_per_point;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            self.distance = (self.distance * (-scroll * 0.002).exp()).clamp(0.5, 100_000.0);
        }
        if response.double_clicked() {
            self.fit();
        }

        let ppp = ui.ctx().pixels_per_point();
        let size = [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ];
        self.ensure_targets(size);
        self.render(rect.width() / rect.height(), ui.visuals().dark_mode);

        if let Some(id) = self.texture_id {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
        }
        self.draw_triad(ui, rect, right, up);
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
            resolve_view: make(
                "viewport colour",
                COLOR_FORMAT,
                1,
                attach | wgpu::TextureUsages::TEXTURE_BINDING,
            ),
        };

        let mut renderer = self.rs.renderer.write();
        match self.texture_id {
            Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                device,
                &targets.resolve_view,
                wgpu::FilterMode::Linear,
                id,
            ),
            None => {
                self.texture_id = Some(renderer.register_native_texture(
                    device,
                    &targets.resolve_view,
                    wgpu::FilterMode::Linear,
                ));
            }
        }
        drop(renderer);
        self.targets = Some(targets);
    }

    fn render(&mut self, aspect: f32, dark: bool) {
        let Some(targets) = &self.targets else { return };

        let offset = self.eye_offset();
        let eye = self.target + offset;
        let view = look_at(eye, self.target, Vec3::Z);
        let near = (self.distance * 0.01).max(0.01);
        let far = self.distance * 20.0 + 1000.0;
        let proj = perspective(45f32.to_radians(), aspect, near, far);

        // Lights follow the camera so the part never goes dark while orbiting.
        let back = offset.normalize();
        let right = Vec3::Z.cross(back).normalize();
        let up = back.cross(right);
        let light_a = (back + right * 0.5 + up * 0.7).normalize();
        let light_b = (back - right * 0.8 - up * 0.3).normalize();

        let globals = Globals {
            view_proj: (proj * view).to_cols_array_2d(),
            light_a: light_a.extend(0.0).to_array(),
            light_b: light_b.extend(0.0).to_array(),
        };
        self.rs.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        let bg = if dark { 0.105 } else { 0.80 };
        let mut encoder = self.rs.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("intercraft viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: bg, g: bg, b: bg * 1.1, a: 1.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            for mesh in self.meshes.values().filter(|m| m.visible) {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
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
            painter.text(
                tip + (tip - origin) * 0.3,
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(11.0),
                color,
            );
        }
    }
}
