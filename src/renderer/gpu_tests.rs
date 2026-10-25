//! Explicit, offscreen GPU correctness check; never part of headless CI.

use super::*;
use cosmic_text::{Attrs, AttrsList, BufferLine, Family, LineEnding, Shaping, Style, Weight};

const WIDTH: u32 = 2048;
const HEIGHT: u32 = 512;

fn gpu_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    eprintln!("offscreen adapter: {:?}", adapter.get_info());
    pollster::block_on(adapter.request_device(&Default::default()))
        .or_else(|_| {
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            }))
        })
        .expect("offscreen check requires a working GPU device (including downlevel fallback)")
}

fn pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &TextRenderer,
    atlas: &TextAtlas,
    viewport: &Viewport,
) -> Vec<u8> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("retained glyph test"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("retained glyph readback"),
        size: (WIDTH * HEIGHT * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.render(atlas, viewport, &mut pass).unwrap();
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH * 4),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range().unwrap().to_vec();
    readback.unmap();
    bytes
}

#[test]
#[ignore = "requires a GPU; offscreen pixel/atlas correctness, not a timing gate"]
fn retained_glyphs_match_fresh_preparation_after_atlas_churn() {
    let (device, queue) = gpu_device();
    let cache = Cache::new(&device);
    let mut viewport = Viewport::new(&device, &cache);
    viewport.update(
        &queue,
        Resolution {
            width: WIDTH,
            height: HEIGHT,
        },
    );
    let mut atlas = TextAtlas::new(&device, &queue, &cache, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut renderer = TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
    let mut fonts = FontSystem::new();
    let mut swash = SwashCache::new();
    let mut buffer = Buffer::new(&mut fonts, Metrics::new(64.0, 80.0));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(Some(WIDTH as f32), Some(HEIGHT as f32));
    for frame in 0..6 {
        buffer.lines.clear();
        // Large, varied glyphs force growth and subsequent eviction/reuse
        // in the initially 256px atlas. Different fonts and sets each pass.
        for line in 0..4 {
            let text: String = (0..64)
                .map(|i| char::from(33 + ((i + frame * 17) % 94) as u8))
                .collect();
            let attrs = Attrs::new()
                .family(Family::Monospace)
                .weight(if line % 2 == 0 {
                    Weight::NORMAL
                } else {
                    Weight::BOLD
                })
                .style(if line < 2 {
                    Style::Normal
                } else {
                    Style::Italic
                });
            buffer.lines.push(BufferLine::new(
                text,
                LineEnding::None,
                AttrsList::new(&attrs),
                Shaping::Advanced,
            ));
            buffer.line_layout(&mut fonts, line);
        }
        let top = (frame % 2) as f32 * 5.0;
        let areas = || {
            [TextArea {
                buffer: &buffer,
                left: 0.0,
                top,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: WIDTH as i32,
                    bottom: HEIGHT as i32,
                },
                default_color: glyphon::Color::rgb(220, 190, 170),
                custom_glyphs: &[],
            }]
        };
        atlas.trim();
        renderer
            .prepare(
                &device,
                &queue,
                &mut fonts,
                &mut atlas,
                &viewport,
                areas(),
                &mut swash,
            )
            .unwrap();
        let initial = pixels(&device, &queue, &renderer, &atlas, &viewport);
        assert!(
            initial.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
            "must actually draw text"
        );
        for _ in 0..3 {
            // Same atlas policy as production: no trim/prepare on reuse.
            assert_eq!(
                initial,
                pixels(&device, &queue, &renderer, &atlas, &viewport)
            );
        }
        atlas.trim();
        renderer
            .prepare(
                &device,
                &queue,
                &mut fonts,
                &mut atlas,
                &viewport,
                areas(),
                &mut swash,
            )
            .unwrap();
        assert_eq!(
            initial,
            pixels(&device, &queue, &renderer, &atlas, &viewport)
        );
    }
}

/// Real wgpu/egui operations through the same boundary as Renderer, without a
/// winit window or surface. Failure points below are injected, not OS failures.
struct UploadProbe {
    device: wgpu::Device,
    queue: wgpu::Queue,
    egui: egui_wgpu::Renderer,
    submissions: usize,
}

impl frame::Backend for UploadProbe {
    type CommandBuffer = wgpu::CommandBuffer;

    fn is_device_lost(&self) -> bool {
        false // A lost device fails the explicit GPU test, not a recovery case.
    }

    fn submit(&mut self, commands: impl IntoIterator<Item = wgpu::CommandBuffer>) {
        self.queue.submit(commands);
        self.submissions += 1;
    }

    fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
        self.egui
            .update_texture(&self.device, &self.queue, id, delta);
    }

    fn free_texture(&mut self, id: egui::TextureId) {
        self.egui.free_texture(&id);
    }
}

fn wait_for_uploads(device: &wgpu::Device) {
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("offscreen GPU work must complete within 10s");
}

fn native_resources(device: &wgpu::Device) -> [isize; 4] {
    let counters = device.get_internal_counters().hal;
    [
        counters.buffers.read(),
        counters.buffer_memory.read(),
        counters.textures.read(),
        counters.texture_memory.read(),
    ]
}

fn probe_delta(image: &Arc<egui::ColorImage>, partial: bool, free: bool) -> egui::TexturesDelta {
    let mut delta = egui::TexturesDelta::default();
    delta.push(
        egui::TextureId::Managed(7),
        egui::epaint::ImageDelta {
            image: egui::ImageData::Color(Arc::clone(image)),
            options: egui::TextureOptions::LINEAR,
            pos: partial.then_some([0, 0]),
        },
    );
    if free {
        delta.free(egui::TextureId::Managed(7));
    }
    delta
}

fn read_probe_buffer(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    wait_for_uploads(device);
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().unwrap().to_vec();
    buffer.unmap();
    bytes
}

#[test]
#[ignore = "requires a GPU; native staging counters and recovery pixels, not headless CI"]
fn failed_frame_uploads_are_reclaimed_and_recover() {
    let (device, queue) = gpu_device();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let egui = egui_wgpu::Renderer::new(&device, format, Default::default());
    let mut probe = UploadProbe {
        device,
        queue,
        egui,
        submissions: 0,
    };

    // A reusable 200x60 colored-screen geometry payload, 120 bytes per cell.
    // No giant input corpus or per-attempt CPU payload allocation is needed.
    let vertices = vec![[0.0_f32, 0.0, 0.0, 1.0, 0.0]; 200 * 60 * 6];
    let payload = bytemuck::cast_slice::<_, u8>(&vertices);
    assert_eq!(
        payload.len(),
        200 * 60 * 6 * std::mem::size_of::<BgVertex>()
    );
    let buffer = probe.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("failed frame background upload"),
        size: payload.len() as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let image = Arc::new(egui::ColorImage::filled([64, 64], egui::Color32::RED));
    frame::run(&mut probe, |probe, frame| {
        frame.update_textures(probe, probe_delta(&image, false, false));
    });
    wait_for_uploads(&probe.device);
    let baseline = native_resources(&probe.device);
    assert!(
        baseline[0] > 0 && baseline[1] >= payload.len() as isize,
        "this probe requires backend buffer counters (wgpu counters dev feature)"
    );

    // Positive control: polling alone cannot submit never-submitted writes.
    for _ in 0..8 {
        probe.queue.write_buffer(&buffer, 0, payload);
    }
    probe.device.poll(wgpu::PollType::Poll).unwrap();
    let unsubmitted = native_resources(&probe.device);
    assert!(unsubmitted[0] >= baseline[0] + 8);
    assert!(unsubmitted[1] >= baseline[1] + 8 * payload.len() as isize);
    probe.queue.submit([]);
    wait_for_uploads(&probe.device);
    assert_eq!(native_resources(&probe.device), baseline);
    eprintln!(
        "native [buffers, buffer_bytes, textures, texture_bytes]: baseline={baseline:?}, eight unsubmitted={unsubmitted:?}"
    );

    let start_submissions = probe.submissions;
    for attempt in 0..4096 {
        let outcome: Result<(), &str> = frame::run(&mut probe, |probe, frame| {
            frame.update_textures(probe, probe_delta(&image, true, false));
            if attempt % 3 == 0 {
                return Err("acquire");
            }
            frame.upload(probe, |probe| probe.queue.write_buffer(&buffer, 0, payload));
            // Models errors after the upload phase has begun, including a
            // dependency returning Err after partial atlas/instance writes.
            Err(if attempt % 3 == 1 {
                "prepare"
            } else {
                "encode"
            })
        });
        assert!(outcome.is_err());
        if attempt % 64 == 63 {
            // Completion waits are test-only. Counters are checked BEFORE any
            // readback mapping, which could otherwise implicitly flush writes.
            wait_for_uploads(&probe.device);
            assert_eq!(
                native_resources(&probe.device),
                baseline,
                "attempt {attempt}"
            );
        }
    }
    assert_eq!(probe.submissions - start_submissions, 4096);

    // Recovery: a successful offscreen egui frame must still see the texture
    // uploaded during failures, and a same-frame free must follow submission.
    let target = probe.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("failed frame recovery pixels"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let pixels = probe.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("failed frame recovery readback"),
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bg_readback = probe.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("failed frame geometry readback"),
        size: payload.len() as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let rect = egui::Rect::from_min_max(egui::pos2(8.0, 8.0), egui::pos2(24.0, 24.0));
    let mut mesh = egui::epaint::Mesh::with_texture(egui::TextureId::Managed(7));
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let jobs = [egui::ClippedPrimitive {
        clip_rect: egui::Rect::EVERYTHING,
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }];
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [64, 64],
        pixels_per_point: 1.0,
    };
    frame::run(&mut probe, |probe, frame| {
        let mut frees = egui::TexturesDelta::default();
        frees.free(egui::TextureId::Managed(7));
        frame.update_textures(probe, frees);
        let mut encoder = probe.device.create_command_encoder(&Default::default());
        let callbacks = frame
            .upload(probe, |probe| {
                probe
                    .egui
                    .update_buffers(&probe.device, &probe.queue, &mut encoder, &jobs, &screen)
            })
            .unwrap();
        let view = target.create_view(&Default::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("failed frame recovery"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            probe
                .egui
                .render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &pixels,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
        encoder.copy_buffer_to_buffer(&buffer, 0, &bg_readback, 0, payload.len() as u64);
        assert!(frame.submit(probe, callbacks.into_iter().chain([encoder.finish()])));
    });
    assert_eq!(probe.submissions - start_submissions, 4097);
    let bytes = read_probe_buffer(&probe.device, &pixels);
    let red = (16 * 64 + 16) * 4;
    let green = (48 * 64 + 48) * 4;
    assert_eq!(&bytes[red..red + 4], &[255, 0, 0, 255]);
    assert_eq!(&bytes[green..green + 4], &[0, 255, 0, 255]);
    assert_eq!(read_probe_buffer(&probe.device, &bg_readback), payload);
    eprintln!(
        "4096 failed attempts: native resources returned to {baseline:?} at every 64-attempt completion checkpoint; recovery pixels/geometry matched"
    );
}
