//! True GPU drop shadows.
//!
//! An instanced wgpu pipeline that renders analytic rounded-rect shadows
//! (signed-distance field + smoothstep falloff — the same technique iced's
//! quad shader uses for widget shadows, which canvas geometry cannot access).
//! Drawn as the bottom layer of the view stack, under the board canvas.

use iced::Rectangle;
use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Viewport};

const SHADER: &str = r#"
struct Globals {
    resolution: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) radius: f32,
    @location(3) blur: f32,
    @location(4) alpha: f32,
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) point: vec2<f32>,
    @location(1) center: vec2<f32>,
    @location(2) half_size: vec2<f32>,
    @location(3) radius: f32,
    @location(4) blur: f32,
    @location(5) alpha: f32,
}

@vertex
fn vs_main(@builtin(vertex_index) v: u32, instance: Instance) -> Out {
    let corner = vec2<f32>(f32(v & 1u), f32(v >> 1u));
    let p = instance.pos - vec2<f32>(instance.blur)
        + (instance.size + vec2<f32>(instance.blur * 2.0)) * corner;

    var out: Out;
    out.position = vec4<f32>(
        p / globals.resolution * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0),
        0.0,
        1.0,
    );
    out.point = p;
    out.center = instance.pos + instance.size / 2.0;
    out.half_size = instance.size / 2.0;
    out.radius = instance.radius;
    out.blur = instance.blur;
    out.alpha = instance.alpha;
    return out;
}

fn rounded_box_sdf(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - half_size + vec2<f32>(radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - radius;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let d = rounded_box_sdf(in.point - in.center, in.half_size, in.radius);
    let a = (1.0 - smoothstep(-in.blur, in.blur, d)) * in.alpha;
    return vec4<f32>(0.0, 0.0, 0.0, a);
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    /// Shadow rect (node rect already translated by the light offset),
    /// logical px in window coordinates.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub corner_radius: f32,
    pub blur: f32,
    pub alpha: f32,
    pub _pad: f32,
}

/// The widget program: carries this frame's shadow instances.
#[derive(Debug, Clone)]
pub struct ShadowLayer {
    pub instances: Vec<Instance>,
}

impl<Message> shader::Program<Message> for ShadowLayer {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive {
            instances: self.instances.clone(),
        }
    }
}

#[derive(Debug)]
pub struct Primitive {
    instances: Vec<Instance>,
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
}

impl Pipeline {
    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("thinkboard.shadows.instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thinkboard.shadows.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("thinkboard.shadows.globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("thinkboard.shadows.bind_group_layout"),
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thinkboard.shadows.bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thinkboard.shadows.layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thinkboard.shadows.pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32,
                        3 => Float32,
                        4 => Float32,
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let capacity = 64;
        let instances = Self::instance_buffer(device, capacity);

        Self {
            pipeline,
            globals,
            bind_group,
            instances,
            capacity,
        }
    }
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        if self.instances.len() > pipeline.capacity {
            pipeline.capacity = self.instances.len().next_power_of_two();
            pipeline.instances = Pipeline::instance_buffer(device, pipeline.capacity);
        }

        queue.write_buffer(
            &pipeline.globals,
            0,
            bytemuck::bytes_of(&[bounds.width, bounds.height, 0.0, 0.0]),
        );
        if !self.instances.is_empty() {
            queue.write_buffer(
                &pipeline.instances,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
    }

    fn draw(&self, pipeline: &Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if self.instances.is_empty() {
            return true;
        }

        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &pipeline.bind_group, &[]);
        render_pass.set_vertex_buffer(0, pipeline.instances.slice(..));
        render_pass.draw(0..4, 0..self.instances.len() as u32);

        true
    }
}
