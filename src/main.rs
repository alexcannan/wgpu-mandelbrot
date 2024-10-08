use std::sync::{Arc, Condvar, Mutex};

use bytemuck::{Pod, Zeroable};
use log::{debug, trace};
use rayon::ThreadPoolBuilder;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

use wgpu_mandelbrot::{
    colour::{ColourRange, HistogramColouring},
    command_buffer,
    command_encoder::CommandEncoderExt,
    compute,
    pixel::{Complex, Pixel},
    screen, typed_buffer,
};

fn create_pixels(size: screen::Size) -> Vec<Pixel> {
    (0..size.height)
        .flat_map(move |y| {
            (0..size.width).map(move |x| Pixel {
                x: x as u32,
                y: y as u32,
                current_value: Complex::ZERO,
                escaped: 0,
                iteration_count: 0,
            })
        })
        .collect::<Vec<_>>()
}

fn create_pixels_buffers(
    device: &wgpu::Device,
    size: screen::Size,
) -> typed_buffer::DoubleBuffer<Pixel> {
    let pixels = create_pixels(size);

    typed_buffer::DoubleBuffer {
        input: typed_buffer::Builder::from(pixels.as_slice())
            .with_label("pixels_buffer_1")
            .with_usage(wgpu::BufferUsages::STORAGE)
            .with_usage(wgpu::BufferUsages::COPY_SRC)
            .create(device),

        output: typed_buffer::Builder::from(pixels.as_slice())
            .with_label("pixels_buffer_2")
            .with_usage(wgpu::BufferUsages::STORAGE)
            .with_usage(wgpu::BufferUsages::COPY_SRC)
            .create(device),
    }
}

#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
struct Vec2 {
    x: f32,
    y: f32,
}

fn main() {
    env_logger::init();

    ThreadPoolBuilder::new()
        .num_threads(num_cpus::get_physical())
        .build_global()
        .unwrap();

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new().build(&event_loop).unwrap();

    let instance = wgpu::Instance::new(wgpu::Backends::all());

    let mut size = window.inner_size();
    let surface = unsafe { instance.create_surface(&window) };

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: Default::default(),
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
    }))
    .unwrap();

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("device"),
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
        },
        None,
    ))
    .unwrap();

    let mut surface_configuration = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface.get_supported_formats(&adapter)[0],
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
    };
    surface.configure(&device, &surface_configuration);

    let compute_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("compute-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("compute.wgsl").into()),
    });

    let compute_bind_group_layout_1 =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute-bind-group-layout-1"),
            entries: &[
                // compute.wgsl#screen_size
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // compute.wgsl#zoom
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // compute.wgsl#origin
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

    let compute_bind_group_layout_2 =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute-bind-group-layout-2"),
            entries: &[
                // compute.wgsl#input
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // compute.wgsl#output
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

    let compute_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("compute-pipeline-layout"),
        bind_group_layouts: &[&compute_bind_group_layout_1, &compute_bind_group_layout_2],
        push_constant_ranges: &[],
    });

    let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("compute-pipeline"),
        layout: Some(&compute_pipeline_layout),
        module: &compute_shader_module,
        entry_point: "mandelbrot",
    });

    let render_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("render-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("render.wgsl").into()),
    });

    let render_bind_group_layout_1 =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render-bind-group-layout"),
            entries: &[
                // render.wgsl#screen_size
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

    let render_bind_group_layout_2 =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render-bind-group-layout-2"),
            entries: &[
                // render.wgsl#colour_ranges
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

    let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("render-pipeline-layout"),
        bind_group_layouts: &[&render_bind_group_layout_1, &render_bind_group_layout_2],
        push_constant_ranges: &[],
    });

    let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("render-pipeline"),
        layout: Some(&render_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &render_shader_module,
            entry_point: "vertex_main",
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &render_shader_module,
            entry_point: "fragment_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_configuration.format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
    });

    let mut screen_size = screen::Size {
        width: size.width as u32,
        height: size.height as u32,
    };
    let screen_size_buffer = typed_buffer::var::Builder::new(screen_size)
        .with_label("screen-size-buffer")
        .with_usage(wgpu::BufferUsages::UNIFORM)
        .create(&device);

    let mut zoom: f32 = 1.0;
    let zoom_buffer = typed_buffer::var::Builder::new(zoom)
        .with_label("zoom-buffer")
        .with_usage(wgpu::BufferUsages::UNIFORM)
        .create(&device);

    let mut origin: Vec2 = Vec2 {
        x: -0.74529,
        y: 0.113075,
    };
    let origin_buffer = typed_buffer::var::Builder::new(origin)
        .with_label("origin-buffer")
        .with_usage(wgpu::BufferUsages::UNIFORM)
        .create(&device);

    let mut pixels_staging_buffer: typed_buffer::Buffer<Pixel> =
        typed_buffer::Builder::new(screen_size.width as u64 * screen_size.height as u64)
            .with_label("pixels_staging_buffer")
            .with_usage(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ)
            .create(&device);

    let mut pixels_buffers = create_pixels_buffers(&device, screen_size);

    let mut compute_bind_group_1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute-bind-group-1"),
        layout: &compute_bind_group_layout_1,
        entries: &[
            // compute.wgsl#screen_size
            wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_size_buffer.binding_resource(),
            },
            // compute.wgsl#zoom
            wgpu::BindGroupEntry {
                binding: 1,
                resource: zoom_buffer.binding_resource(),
            },
            // compute.wgsl#origin
            wgpu::BindGroupEntry {
                binding: 2,
                resource: origin_buffer.binding_resource(),
            },
        ],
    });

    let mut render_bind_group_1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("render-bind-group"),
        layout: &render_pipeline.get_bind_group_layout(0),
        entries: &[
            // render.wgsl#screen_size
            wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_size_buffer.binding_resource(),
            },
        ],
    });

    let mut cursor_position = Vec2 { x: 0.0, y: 0.0 };
    let mut zoom_changed = false;
    let mut origin_changed = false;

    let mut colour_ranges_buffer: typed_buffer::Buffer<ColourRange> = typed_buffer::Builder::from(
        std::iter::repeat(ColourRange::default())
            .take((screen_size.width * screen_size.height) as usize)
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .with_usage(wgpu::BufferUsages::STORAGE)
    .create(&device);

    let mut colour_ranges: Vec<ColourRange> = std::iter::repeat(ColourRange::default())
        .take((screen_size.width * screen_size.height) as usize)
        .collect();

    let mut histogram_colouring = HistogramColouring::new();

    let mut all_pixels: Vec<Pixel> = create_pixels(screen_size);
    let mut unescaped_pixels: Vec<Pixel> = create_pixels(screen_size);
    let mut newly_escaped_pixels: Vec<Pixel> = Vec::new();

    let device = Arc::new(device);

    event_loop.run(move |event, _, control_flow| {
        // To present frames in realtime, *don't* set `control_flow` to `Wait`.
        // control_flow.set_wait();
        match event {
            Event::MainEventsCleared => {
                // And `request_redraw` once we've cleared all events for the frame.
                window.request_redraw();
            }
            Event::WindowEvent { window_id, event } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => {
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_position.x = position.x as f32;
                    cursor_position.y = position.y as f32;
                }
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                } => {
                    debug!("mouse pressed at {:?}", cursor_position);

                    /*
                    when `zoom = 1.0`, we're viewing (-2, -2) to (2, 2).

                    (0, 0) corresponds to (size.width / 2, size.height / 2)

                    A click at (cursor_x, cursor_y) corresponds to (4 * cursor_x / size.width - 2, 4 * cursor_y / size.height - 2)
                     */

                    let zoom_inv = 2.0 / zoom;
                    origin = Vec2 {
                        x: origin.x
                            + (2.0 * zoom_inv * cursor_position.x / (size.width as f32) - zoom_inv),
                        y: origin.y
                            + (2.0 * zoom_inv * cursor_position.y / (size.height as f32)
                                - zoom_inv),
                    };
                    debug!("origin set to {:?}", origin);
                    origin_changed = true;
                    origin_buffer.write(&queue, origin);
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    zoom += zoom
                        * 0.1
                        * match delta {
                            winit::event::MouseScrollDelta::LineDelta(_, delta) => delta,
                            winit::event::MouseScrollDelta::PixelDelta(position) => (position.y / 1000.0 as f64) as f32,
                        };
                    zoom_changed = true;
                    zoom_buffer.write(&queue, zoom);
                }
                WindowEvent::Resized(new_size) => {
                    debug!("resizing to {:?}", new_size);
                    size = new_size;
                    screen_size = screen::Size {
                        width: size.width as u32,
                        height: size.height as u32,
                    };

                    surface_configuration.width = size.width;
                    surface_configuration.height = size.height;

                    surface.configure(&device, &surface_configuration);

                    colour_ranges.clear();
                    colour_ranges.extend(
                        std::iter::repeat(ColourRange::default())
                            .take((screen_size.width * screen_size.height) as usize),
                    );
                    histogram_colouring.reset();

                    screen_size_buffer.write(&queue, screen_size);

                    pixels_staging_buffer = typed_buffer::Builder::new(
                        screen_size.width as u64 * screen_size.height as u64,
                    )
                    .with_label("pixels_staging_buffer")
                    .with_usage(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ)
                    .create(&device);

                    std::mem::replace(
                        &mut pixels_buffers,
                        create_pixels_buffers(&device, screen_size),
                    )
                    .destroy();
                    all_pixels = create_pixels(screen_size);
                    unescaped_pixels = create_pixels(screen_size);

                    std::mem::replace(
                        &mut colour_ranges_buffer,
                        typed_buffer::Builder::from(
                            std::iter::repeat(ColourRange::default())
                                .take((screen_size.width * screen_size.height) as usize)
                                .collect::<Vec<_>>()
                                .as_slice(),
                        )
                        .with_usage(wgpu::BufferUsages::STORAGE)
                        .create(&device),
                    )
                    .destroy();

                    compute_bind_group_1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("compute-bind-group"),
                        layout: &compute_bind_group_layout_1,
                        entries: &[
                            // compute.wgsl#screen_size
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: screen_size_buffer.binding_resource(),
                            },
                            // compute.wgsl#zoom
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: zoom_buffer.binding_resource(),
                            },
                            // compute.wgsl#origin
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: origin_buffer.binding_resource(),
                            },
                        ],
                    });

                    render_bind_group_1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("render-bind-group"),
                        layout: &render_pipeline.get_bind_group_layout(0),
                        entries: &[
                            // render.wgsl#screen_size
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: screen_size_buffer.binding_resource(),
                            },
                        ],
                    });

                    window.request_redraw();
                }
                _ => {}
            },
            Event::RedrawRequested(window_id) if window_id == window.id() => {
                debug_assert!(
                    unescaped_pixels.len()
                        <= screen_size.width as usize * screen_size.height as usize
                );
                if cfg!(debug_assertions) {
                    for pixel in unescaped_pixels.iter() {
                        debug_assert!(pixel.escaped < 2);
                    }
                }

                let reset_buffers = zoom_changed || origin_changed;
                zoom_changed = false;
                origin_changed = false;

                if reset_buffers {
                    colour_ranges.clear();
                    colour_ranges.extend(
                        std::iter::repeat(ColourRange::default())
                            .take((screen_size.width * screen_size.height) as usize),
                    );
                    histogram_colouring.reset();

                    let pixels = create_pixels(screen_size);
                    pixels_buffers.input.write(&queue, &pixels);
                    pixels_buffers.output.write(&queue, &pixels);
                    all_pixels = pixels.clone();
                    unescaped_pixels = pixels;
                }

                let surface_texture = surface.get_current_texture().unwrap();
                let surface_texture_view = surface_texture
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                let compute_bind_group_2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("compute-bind-group-2"),
                    layout: &compute_bind_group_layout_2,
                    entries: &[
                        // compute.wgsl#input
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: pixels_buffers.input.binding_resource(0, None),
                        },
                        // compute.wgsl#output
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: pixels_buffers.output.binding_resource(0, None),
                        },
                    ],
                });

                let render_bind_group_2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("render-bind-group-2"),
                    layout: &render_pipeline.get_bind_group_layout(1),
                    entries: &[
                        // render.wgsl#colour_ranges
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: colour_ranges_buffer.binding_resource(0, None),
                        },
                    ],
                });

                let compute_command_buffer = command_buffer::create(
                    &device,
                    &wgpu::CommandEncoderDescriptor::default(),
                    |command_encoder| {
                        pixels_buffers.input.write(&queue, &unescaped_pixels);

                        command_encoder.push_debug_group("compute-pass");
                        command_encoder.with_compute_pass(
                            &wgpu::ComputePassDescriptor {
                                label: Some("compute-pass"),
                            },
                            |compute_pass| {
                                compute_pass.set_pipeline(&compute_pipeline);

                                compute_pass.set_bind_group(0, &compute_bind_group_1, &[]);
                                compute_pass.set_bind_group(1, &compute_bind_group_2, &[]);

                                compute_pass.insert_debug_marker("mandelbrot");

                                let total_work = unescaped_pixels.len();

                                let (x, y, z) = compute::mandelbrot_dispatch_size(total_work);

                                compute_pass.dispatch_workgroups(x, y, z);
                            },
                        );
                        command_encoder.pop_debug_group();

                        typed_buffer::copy_buffer_to_buffer(
                            command_encoder,
                            &pixels_buffers.output,
                            0,
                            &pixels_staging_buffer,
                            0,
                            unescaped_pixels.len().try_into().unwrap(),
                        );
                    },
                );

                queue.submit([compute_command_buffer]);

                let pixels_staging_buffer_slice = pixels_staging_buffer.slice(..);

                {
                    trace!("waiting for staging buffer");
                    let mapped = Arc::new((Mutex::new(true), Condvar::new()));

                    pixels_staging_buffer_slice.map_async(wgpu::MapMode::Read, {
                        let mapped = mapped.clone();
                        move |map_result| {
                            debug!("map_async callback called");
                            map_result.unwrap_or_else(|err| panic!("buffer async error: {}", err));
                            let mut guard = mapped.0.lock().unwrap();
                            *guard = false;
                            mapped.1.notify_all();
                        }
                    });

                    {
                        let device = device.clone();
                        std::thread::spawn(move || while !device.poll(wgpu::Maintain::Poll) {});
                    }

                    debug!("waiting for condition");
                    let _guard = mapped
                        .1
                        .wait_while(mapped.0.lock().unwrap(), |pending| *pending)
                        .unwrap();
                    debug!("staging buffer mapped");
                }

                {
                    let pixels_staging_buffer_view: typed_buffer::View<Pixel> =
                        pixels_staging_buffer_slice.get_mapped_range();

                    let unescaped_pixels_len = unescaped_pixels.len();
                    unescaped_pixels.clear();
                    newly_escaped_pixels.clear();

                    pixels_staging_buffer_view
                        .iter()
                        /*
                        This caused a bug for me: even though I copy `unescaped_pixels.len()`
                        worth of data into the staging buffer, the buffer is still the size
                        of the screen.
                        Without the `take`, I was iterating over every pixel in the buffer.
                        Everything after `unescaped_pixels.len()` in the buffer is effectively
                        garbage (leftover from previous runs), but I was including it in the
                        `newly_escaped` array anyway.
                        */
                        .take(unescaped_pixels_len)
                        .for_each(|pixel| {
                            let pixel = *pixel;

                            debug_assert!(pixel.x < screen_size.width);
                            debug_assert!(pixel.y < screen_size.height);
                            debug_assert!(pixel.escaped < 2);

                            if pixel.escaped == 1 {
                                all_pixels[pixel.y as usize * screen_size.width as usize
                                    + pixel.x as usize] = pixel;
                                newly_escaped_pixels.push(pixel);
                            } else {
                                unescaped_pixels.push(pixel);
                            }
                        });
                }

                pixels_staging_buffer.buffer().unmap();

                histogram_colouring.update_colours(
                    screen_size,
                    &all_pixels,
                    &newly_escaped_pixels,
                    &mut colour_ranges,
                );
                debug_assert!(
                    colour_ranges.len() == screen_size.width as usize * screen_size.height as usize,
                    "colour_ranges.len() == {}, expected {}",
                    colour_ranges.len(),
                    screen_size.width * screen_size.height,
                );

                colour_ranges_buffer.write(&queue, &colour_ranges);

                let render_command_buffer = command_buffer::create(
                    &device,
                    &wgpu::CommandEncoderDescriptor::default(),
                    |command_encoder| {
                        command_encoder.push_debug_group("render-pass");
                        command_encoder.with_render_pass(
                            &wgpu::RenderPassDescriptor {
                                label: Some("render-pass"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &surface_texture_view,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 0.5,
                                            g: 0.5,
                                            b: 0.0,
                                            a: 1.0,
                                        }),
                                        store: true,
                                    },
                                })],
                                depth_stencil_attachment: None,
                            },
                            |render_pass| {
                                render_pass.set_pipeline(&render_pipeline);
                                render_pass.set_bind_group(0, &render_bind_group_1, &[]);
                                render_pass.set_bind_group(1, &render_bind_group_2, &[]);
                                render_pass.draw(0..4, 0..1);
                            },
                        );
                        command_encoder.pop_debug_group();
                    },
                );

                trace!("submitting render commands");
                queue.submit([render_command_buffer]);

                surface_texture.present();

                pixels_buffers.swap();
            }
            _ => {}
        }
    });
}
