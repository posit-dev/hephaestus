//! The presentation path the `window` feature uses, exercised without a
//! window: render into an intermediate storage texture, blit it into a
//! swap-chain-shaped texture, and check the pixels survive intact.
//!
//! Two things this pins that a headless render alone does not:
//! `STORAGE_BINDING | TEXTURE_BINDING` is enough for `render_to_texture`
//! (the blit samples the target, it does not copy it), and blitting an
//! `Rgba8Unorm` intermediate into a non-sRGB `Bgra8Unorm` swap chain is a
//! plain channel reorder — no transfer function applied twice.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::geometry::Rect;
use hephaestus::{Affine, Brush, FillRule, PickId, Renderer, SceneBuilder, WgpuRenderer};
use kurbo::Shape;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn blitting_the_render_target_into_a_bgra_surface_preserves_the_image() {
    let bg: Color = rgb8(20, 22, 28);
    let fill: Color = rgb8(200, 90, 40);

    let (device, queue) = make_device();
    let mut renderer = VelloRenderer::with_device(&device, &queue).expect("with_device init");
    draw(&mut renderer, fill);

    // The intermediate texture, with exactly the usage flags the window
    // surface allocates: what the backend asks for, plus `TEXTURE_BINDING`
    // for the blit to sample it.
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("window_blit.target"),
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: VelloRenderer::REQUIRED_TARGET_USAGE | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    renderer
        .render_to_texture(&target_view, W, H, bg)
        .expect("texture render");

    // Stand-in for the swap chain: the same format the surface picks.
    let swapchain = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("window_blit.swapchain"),
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let swapchain_view = swapchain.create_view(&wgpu::TextureViewDescriptor::default());

    let blitter = wgpu::util::TextureBlitter::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("window_blit.blit"),
    });
    blitter.copy(&device, &mut encoder, &target_view, &swapchain_view);
    queue.submit([encoder.finish()]);

    let presented = read_back(&device, &queue, &swapchain);

    // Reference: the same scene through the headless path.
    let mut reference_renderer = VelloRenderer::new().expect("vello renderer init");
    draw(&mut reference_renderer, fill);
    let mut reference = vec![0u8; (W * H * 4) as usize];
    reference_renderer
        .render_to_buffer(W, H, bg, &mut reference)
        .expect("buffer render");

    for (i, (rgba, bgra)) in reference
        .chunks_exact(4)
        .zip(presented.chunks_exact(4))
        .enumerate()
    {
        assert_eq!(
            [rgba[2], rgba[1], rgba[0], rgba[3]],
            [bgra[0], bgra[1], bgra[2], bgra[3]],
            "pixel {i} diverged between the presented frame and the headless render"
        );
    }
}

/// A shape big enough that both the fill and the background are sampled.
fn draw(renderer: &mut VelloRenderer, fill: Color) {
    let scene = renderer.scene();
    scene.clear();
    let brush: Brush = fill.into();
    let rect = Rect::new(8.0, 8.0, 48.0, 40.0).to_path(0.1);
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &brush,
        None,
        &rect,
        PickId::Skip,
    );
}

fn extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: W,
        height: H,
        depth_or_array_layers: 1,
    }
}

/// Copy a texture back to CPU as tightly packed rows.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row_bytes = W * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = row_bytes.div_ceil(align) * align;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("window_blit.readback"),
        size: (padded as u64) * (H as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("window_blit.copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        extent(),
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    pollster::block_on(rx.receive())
        .expect("map_async sender dropped")
        .expect("map_async");

    let mut out = vec![0u8; (W * H * 4) as usize];
    {
        let data = slice.get_mapped_range();
        for y in 0..H as usize {
            let start = y * padded as usize;
            out[y * row_bytes as usize..(y + 1) * row_bytes as usize]
                .copy_from_slice(&data[start..start + row_bytes as usize]);
        }
    }
    buffer.unmap();
    out
}

/// An isolated device, standing in for the one a window surface would open.
fn make_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("window_blit.device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("device")
    })
}
