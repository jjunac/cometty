//! Explicit, offscreen GPU correctness check; never part of headless CI.

use super::*;
use cosmic_text::{Attrs, AttrsList, BufferLine, Family, LineEnding, Shaping, Style, Weight};

const WIDTH: u32 = 2048;
const HEIGHT: u32 = 512;

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
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
        .or_else(|_| {
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            }))
        })
        .expect("offscreen check requires a working GPU device (including downlevel fallback)");
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
