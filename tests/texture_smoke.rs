//! Smoke test for the windowing path: render directly into a caller-owned
//! storage texture via `WgpuRenderer::render_to_texture` and verify the
//! pixels round-trip identically to `render_to_buffer`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::{Renderer, WgpuRenderer};

#[test]
fn render_to_texture_matches_render_to_buffer() {
    let mut r = VelloRenderer::new().expect("vello renderer init");

    let w = 64u32;
    let h = 64u32;
    let bg = rgb8(255, 64, 32);

    // Reference path: render via the buffer API.
    let mut reference = vec![0u8; (w * h * 4) as usize];
    r.render_to_buffer(w, h, bg, &mut reference)
        .expect("buffer render");

    // Reach into the renderer's device to allocate a host-side target
    // matching the `Rgba8Unorm` storage texture contract documented on
    // `WgpuRenderer::render_to_texture`. In a real windowing host this
    // texture would be the offscreen target that the host then blits to
    // its swap chain.
    //
    // We use the same device by constructing a second renderer with
    // `with_device` against a fresh wgpu device created here, so we
    // exercise the `with_device` path too.
    let (device, queue) = make_device();
    let mut r2 = VelloRenderer::with_device(&device, &queue).expect("with_device init");

    let bytes_per_row = w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = bytes_per_row.div_ceil(align) * align;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("texture_smoke.target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("texture_smoke.readback"),
        size: (padded_bytes_per_row as u64) * (h as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    r2.render_to_texture(&view, w, h, bg)
        .expect("texture render");

    // Copy out the texture so we can compare pixels.
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("texture_smoke.copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    pollster::block_on(rx.receive())
        .expect("map_async sender dropped")
        .expect("map_async");

    let row_bytes = (w as usize) * 4;
    let mut from_texture = vec![0u8; (w * h * 4) as usize];
    {
        let data = slice.get_mapped_range();
        for y in 0..h as usize {
            let src = &data
                [y * padded_bytes_per_row as usize..y * padded_bytes_per_row as usize + row_bytes];
            let dst = &mut from_texture[y * row_bytes..y * row_bytes + row_bytes];
            dst.copy_from_slice(src);
        }
    }
    readback.unmap();

    assert_eq!(
        from_texture, reference,
        "render_to_texture output diverged from render_to_buffer"
    );
}

/// Spin up an isolated wgpu device for the windowing-host emulation half
/// of the test.
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
                label: Some("texture_smoke.device"),
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
