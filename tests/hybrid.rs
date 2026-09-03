//! End-to-end tests for the Hybrid backend.
//!
//! The picking cases check that hit testing survives every render path —
//! buffer, texture, and both device-sharing constructors — and that turning
//! it on changes no drawn pixel.

use hephaestus::backend::hybrid::HybridRenderer;
use hephaestus::color::rgb8;
use hephaestus::geometry::Point;
use hephaestus::{Affine, Brush, FillRule, PickId, Rect, Renderer, SceneBuilder};
use kurbo::Shape;

/// The topmost id at a point. Renderers expose the index rather than
/// forwarding every query, so a test asks it the same way a host would.
fn pick(r: &HybridRenderer, p: Point) -> Option<u32> {
    r.pick_index()?.pick_at(p)
}

const W: u32 = 100;
const H: u32 = 100;

fn buf() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn a_scene_can_be_rendered_at_two_sizes_in_a_row() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    fill(
        r.scene(),
        Rect::new(0.0, 0.0, 10.0, 10.0),
        [0, 255, 0],
        PickId::Skip,
    );
    let mut small = vec![0u8; 40 * 40 * 4];
    r.render_to_buffer(40, 40, rgb8(0, 0, 0), &mut small)
        .expect("small render");
    let mut large = vec![0u8; 120 * 90 * 4];
    r.render_to_buffer(120, 90, rgb8(0, 0, 0), &mut large)
        .expect("large render");

    assert_eq!(&small[0..4], &[0, 255, 0, 255], "fill survives resize");
    assert_eq!(&large[0..4], &[0, 255, 0, 255]);
}

/// Fill `rect` with `color`, tagged `pick`.
fn fill(scene: &mut impl SceneBuilder, rect: Rect, color: [u8; 3], pick: PickId) {
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Solid(rgb8(color[0], color[1], color[2])),
        None,
        &rect.to_path(0.1),
        pick,
    );
}

#[test]
fn renders_a_solid_fill_over_the_background() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let mut out = buf();
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Skip,
    );
    r.render_to_buffer(W, H, rgb8(255, 255, 255), &mut out)
        .expect("render");

    assert_eq!(px(&out, 50, 50), [255, 0, 0, 255], "inside the fill");
    assert_eq!(px(&out, 5, 5), [255, 255, 255, 255], "background");
}

// ─── Alpha convention ───────────────────────────────────────────────────────

/// `render_to_buffer` hands out straight (un-premultiplied) alpha, same as
/// every other backend. The rasteriser composites premultiplied, so this is
/// the assertion that catches the conversion going missing.
#[test]
fn output_is_straight_alpha() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let transparent = hephaestus::color::Color::new([0.0, 0.0, 0.0, 0.0]);
    r.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &hephaestus::color::Color::new([1.0, 0.0, 0.0, 0.5]).into(),
        None,
        &Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1),
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, transparent, &mut out)
        .expect("render");

    let [red, _, _, alpha] = px(&out, 50, 50);
    // Premultiplied would report red ≈ 128 here; straight keeps it at full.
    assert!(
        red > 250,
        "red channel {red} looks premultiplied, not straight"
    );
    assert!(
        (120..=136).contains(&alpha),
        "alpha {alpha} off half-coverage"
    );
}

// ─── Brushes and layers ─────────────────────────────────────────────────────

#[test]
fn gradient_brush_varies_across_the_fill() {
    use hephaestus::brush::{Brush as B, Gradient};

    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let gradient = Gradient::new_linear((0.0, 0.0), (W as f64, 0.0))
        .with_stops(&[rgb8(0, 0, 0), rgb8(255, 255, 255)][..]);
    r.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &B::Gradient(gradient),
        None,
        &Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1),
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let left = px(&out, 5, 50)[0];
    let right = px(&out, 95, 50)[0];
    assert!(
        right > left + 100,
        "gradient did not ramp: {left} -> {right}"
    );
}

#[test]
fn a_clip_layer_confines_what_it_contains() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    {
        let scene = r.scene();
        scene.push_layer(
            hephaestus::blend::BlendMode::NORMAL,
            1.0,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 50.0, 100.0).to_path(0.1),
        );
        fill(
            scene,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            [255, 0, 0],
            PickId::Skip,
        );
        scene.pop_layer();
    }
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 255), &mut out)
        .expect("render");

    assert_eq!(px(&out, 25, 50), [255, 0, 0, 255], "inside the clip");
    assert_eq!(px(&out, 75, 50), [0, 0, 255, 255], "outside the clip");
}

// ─── Meshes ─────────────────────────────────────────────────────────────────

#[test]
fn a_mesh_triangle_rasterises() {
    use hephaestus::mesh::Mesh;

    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    let green = rgb8(0, 200, 0);
    let mesh = Mesh::new(
        vec![
            hephaestus::geometry::Point::new(10.0, 10.0),
            hephaestus::geometry::Point::new(90.0, 10.0),
            hephaestus::geometry::Point::new(50.0, 90.0),
        ],
        vec![green, green, green],
        vec![0, 1, 2],
    );
    r.scene().draw_mesh(&mesh, Affine::IDENTITY, PickId::Id(5));

    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    assert_eq!(px(&out, 50, 40)[1], 200, "mesh interior");
    assert_eq!(
        pick(&r, Point::new(50.0, 40.0)),
        Some(5),
        "mesh carries its pick id"
    );
}

// ─── Images ─────────────────────────────────────────────────────────────────

/// A 2x2 opaque image: red, green / blue, white.
fn quad_image() -> hephaestus::brush::Image {
    use hephaestus::brush::{Blob, ImageAlphaType, ImageFormat};
    hephaestus::brush::Image {
        data: Blob::from(vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ]),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 2,
        height: 2,
    }
}

#[test]
fn an_image_is_uploaded_and_sampled() {
    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    // Scale the 2x2 up so each source pixel covers a 50x50 block.
    r.scene().draw_image(
        &quad_image(),
        Affine::scale(50.0),
        hephaestus::brush::Sampling::Nearest,
        1.0,
        PickId::Id(9),
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    assert_eq!(px(&out, 25, 25), [255, 0, 0, 255], "top-left source pixel");
    assert_eq!(px(&out, 75, 25), [0, 255, 0, 255], "top-right");
    assert_eq!(px(&out, 25, 75), [0, 0, 255, 255], "bottom-left");
    assert_eq!(
        pick(&r, Point::new(50.0, 50.0)),
        Some(9),
        "image carries its pick id"
    );
}

/// Image opacity cannot ride on the sampler — the shared paint encoder
/// rejects any value but 1.0 — so the backend turns it into a layer. Without
/// that, this panics rather than fading.
#[test]
fn a_translucent_image_fades_instead_of_panicking() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    r.scene().draw_image(
        &quad_image(),
        Affine::scale(50.0),
        hephaestus::brush::Sampling::Nearest,
        0.5,
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let [red, _, _, _] = px(&out, 25, 25);
    assert!(
        (110..=145).contains(&red),
        "half-opacity red over black came back {red}"
    );
}

// ─── Text ───────────────────────────────────────────────────────────────────

/// Glyph runs need a `Resources` threaded through the rasteriser's glyph
/// builder, which is the one part of the port with no counterpart in the
/// compute-shader backend. This drives the real text pipeline — shaping
/// included — and asserts ink landed where the block was placed.
#[test]
fn text_draws_ink_inside_its_block() {
    use hephaestus::style_vocab::{HAlign, Palette};
    use hephaestus::text::rich::{RichAnchor, RichTextRun, RichTextStyleSheet};
    use hephaestus::text::TextStyle;

    let sheet = RichTextStyleSheet::new();
    let palette = Palette::default();
    let style = TextStyle::new(16.0);
    let run = RichTextRun::new(
        "Hybrid renders text",
        &style,
        rgb8(255, 255, 255),
        &sheet,
        &palette,
        96.0,
    );
    let height = run.set_max_width(180.0, HAlign::Start);
    assert!(height > 0.0, "a shaped block must have height");

    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    {
        let scene = r.scene();
        scene.clear();
        hephaestus::text::rich::draw_rich_text(
            scene,
            &run,
            10.0,
            10.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let lit = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|(x, y)| px(&out, *x, *y)[0] > 40)
        .count();
    assert!(lit > 20, "expected glyph ink, found {lit} lit pixels");
}

// ─── The windowing path ─────────────────────────────────────────────────────

/// `render_to_texture` must agree with `render_to_buffer` pixel for pixel.
///
/// Opaque content only, deliberately: the buffer path normalises to straight
/// alpha while the texture path leaves whatever the rasteriser composited
/// (`TARGET_IS_PREMULTIPLIED`), and the two conventions only coincide at alpha
/// 255. That is also the condition the window host presents under.
#[test]
fn render_to_texture_matches_render_to_buffer() {
    use hephaestus::{wgpu, WgpuRenderer};

    let (w, h) = (64u32, 64u32);
    let bg = rgb8(255, 64, 32);
    let mark = Rect::new(8.0, 12.0, 40.0, 52.0);

    let mut reference = vec![0u8; (w * h * 4) as usize];
    {
        let mut r = HybridRenderer::new().expect("hybrid renderer init");
        fill(r.scene(), mark, [10, 200, 120], PickId::Skip);
        r.render_to_buffer(w, h, bg, &mut reference)
            .expect("buffer render");
    }

    // Emulate a windowing host: our own device, a target carrying exactly the
    // usage the backend asks for, and `with_device` so both share it.
    let (device, queue) = make_device();
    let mut r = HybridRenderer::with_device(&device, &queue).expect("with_device init");
    fill(r.scene(), mark, [10, 200, 120], PickId::Skip);

    let bytes_per_row = w * 4;
    let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_texture.target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: HybridRenderer::REQUIRED_TARGET_USAGE | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hybrid_texture.readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    r.render_to_texture(&view, w, h, bg)
        .expect("texture render");

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("hybrid_texture.copy"),
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
                bytes_per_row: Some(padded),
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
            from_texture[y * row_bytes..(y + 1) * row_bytes]
                .copy_from_slice(&data[y * padded as usize..y * padded as usize + row_bytes]);
        }
    }
    readback.unmap();

    assert_eq!(
        from_texture, reference,
        "render_to_texture output diverged from render_to_buffer"
    );
}

/// Picking keeps working on the texture path: display goes straight to the
/// host's view, but the pick pass still rasterises and reads back.
#[test]
fn picking_survives_the_texture_path() {
    use hephaestus::{wgpu, WgpuRenderer};

    let (device, queue) = make_device();
    let mut r = HybridRenderer::with_device_and_picking(&device, &queue).expect("init");
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Id(77),
    );

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_pick.target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: HybridRenderer::REQUIRED_TARGET_USAGE,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    r.render_to_texture(&view, W, H, rgb8(0, 0, 0))
        .expect("texture render");

    assert_eq!(pick(&r, Point::new(50.0, 50.0)), Some(77));
    assert_eq!(pick(&r, Point::new(5.0, 5.0)), None);
}

/// An isolated wgpu device, standing in for the one a window's swap chain
/// would own.
fn make_device() -> (hephaestus::wgpu::Device, hephaestus::wgpu::Queue) {
    use hephaestus::wgpu;
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
                label: Some("hybrid_texture.device"),
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

/// The full presentation path, headless: render into the intermediate texture
/// the window surface allocates, blit it onto a swap-chain-format texture, and
/// check the result against the headless render.
///
/// Pins that the backend-supplied usage flags are the ones the blit needs. The
/// content is opaque, so the two alpha conventions coincide — see
/// `render_to_texture_matches_render_to_buffer`.
#[test]
fn the_presented_frame_matches_the_headless_render() {
    use hephaestus::{wgpu, WgpuRenderer};

    let (w, h) = (64u32, 48u32);
    let bg = rgb8(18, 22, 30);
    let mark = Rect::new(8.0, 8.0, 48.0, 40.0);
    let extent = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };

    let (device, queue) = make_device();
    let mut r = HybridRenderer::with_device(&device, &queue).expect("with_device init");
    fill(r.scene(), mark, [200, 90, 40], PickId::Skip);

    // Exactly what `WindowSurface` allocates: the backend's requirement plus
    // `TEXTURE_BINDING` for the blit to sample.
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_blit.target"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: HybridRenderer::REQUIRED_TARGET_USAGE | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    r.render_to_texture(&target_view, w, h, bg)
        .expect("texture render");

    let swapchain = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_blit.swapchain"),
        size: extent,
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
        label: Some("hybrid_blit.blit"),
    });
    blitter.copy(&device, &mut encoder, &target_view, &swapchain_view);
    queue.submit([encoder.finish()]);

    let bytes_per_row = w * 4;
    let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hybrid_blit.readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("hybrid_blit.copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &swapchain,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        extent,
    );
    queue.submit([encoder.finish()]);

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
    let mut presented = vec![0u8; (w * h * 4) as usize];
    {
        let data = slice.get_mapped_range();
        for y in 0..h as usize {
            presented[y * row_bytes..(y + 1) * row_bytes]
                .copy_from_slice(&data[y * padded as usize..y * padded as usize + row_bytes]);
        }
    }
    readback.unmap();

    let mut reference = vec![0u8; (w * h * 4) as usize];
    {
        let mut r = HybridRenderer::new().expect("hybrid renderer init");
        fill(r.scene(), mark, [200, 90, 40], PickId::Skip);
        r.render_to_buffer(w, h, bg, &mut reference)
            .expect("buffer render");
    }

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

// ─── Picking must not disturb the display ───────────────────────────────────

/// A scene whose pixels depend on coverage, paint and glyph data all three:
/// an antialiased circle, a gradient fill, and a line of text.
fn coverage_sensitive_scene(scene: &mut dyn SceneBuilder) {
    use hephaestus::brush::Gradient;
    use hephaestus::style_vocab::{HAlign, Palette};
    use hephaestus::text::rich::{RichAnchor, RichTextRun, RichTextStyleSheet};
    use hephaestus::text::TextStyle;

    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(
            Gradient::new_linear((0.0, 0.0), (W as f64, 0.0))
                .with_stops(&[rgb8(20, 30, 60), rgb8(180, 200, 240)][..]),
        ),
        None,
        &Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1),
        PickId::Id(1),
    );
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Solid(rgb8(240, 120, 60)),
        None,
        &kurbo::Circle::new((44.0, 52.0), 26.0).to_path(0.1),
        PickId::Id(2),
    );
    let style = TextStyle::new(14.0);
    let run = RichTextRun::new(
        "Ag",
        &style,
        rgb8(255, 255, 255),
        &RichTextStyleSheet::new(),
        &Palette::default(),
        96.0,
    );
    run.set_max_width(80.0, HAlign::Start);
    hephaestus::text::rich::draw_rich_text(
        scene,
        &run,
        6.0,
        4.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Id(3),
    );
}

/// Turning picking on must not change a single display pixel.
///
/// It did: both passes go through one renderer whose per-frame coverage,
/// paint and glyph uploads happen while a pass is recorded, so sharing a
/// command buffer let the pick pass overwrite the display pass's data before
/// the GPU consumed it. The display came back aliased and textless.
#[test]
fn picking_does_not_change_the_buffered_display() {
    let mut plain = buf();
    let mut r = HybridRenderer::new().expect("init");
    coverage_sensitive_scene(r.scene());
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut plain)
        .expect("render");

    let mut picked = buf();
    let mut r = HybridRenderer::with_picking().expect("init");
    coverage_sensitive_scene(r.scene());
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut picked)
        .expect("render");

    assert_eq!(
        first_difference(&plain, &picked),
        None,
        "enabling picking altered the display output"
    );
    // And the hitmap is still populated, so the split did not cost the pick.
    assert_eq!(
        pick(&r, Point::new(44.0, 52.0)),
        Some(2),
        "circle should be hittable"
    );
}

/// Same invariant on the windowing path, which had its own copy of the bug.
#[test]
fn picking_does_not_change_the_textured_display() {
    let (device, queue) = make_device();
    let plain = render_via_texture(&device, &queue, false);
    let picked = render_via_texture(&device, &queue, true);
    assert_eq!(
        first_difference(&plain, &picked),
        None,
        "enabling picking altered the display output on the texture path"
    );
}

/// Index of the first differing byte, or `None` when the two agree.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Draw [`coverage_sensitive_scene`] through `render_to_texture` and read the
/// target back.
fn render_via_texture(
    device: &hephaestus::wgpu::Device,
    queue: &hephaestus::wgpu::Queue,
    picking: bool,
) -> Vec<u8> {
    use hephaestus::{wgpu, WgpuRenderer};

    let mut r = if picking {
        HybridRenderer::with_device_and_picking(device, queue)
    } else {
        HybridRenderer::with_device(device, queue)
    }
    .expect("init");
    coverage_sensitive_scene(r.scene());

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_pick_parity.target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: HybridRenderer::REQUIRED_TARGET_USAGE | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    r.render_to_texture(&view, W, H, rgb8(0, 0, 0))
        .expect("texture render");

    let padded =
        (W * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    pollster::block_on(rx.receive())
        .expect("sender dropped")
        .expect("map_async");
    let row = (W as usize) * 4;
    let mut out = vec![0u8; (W * H * 4) as usize];
    {
        let data = slice.get_mapped_range();
        for y in 0..H as usize {
            out[y * row..(y + 1) * row]
                .copy_from_slice(&data[y * padded as usize..y * padded as usize + row]);
        }
    }
    readback.unmap();
    out
}

// ─── The retired capacity limits ────────────────────────────────────────────

/// Draw `n` small circles, each its own solid fill.
///
/// One fill per mark is what makes the count meaningful: it is the shape that
/// runs into a per-draw budget rather than a geometry one.
fn many_marks(scene: &mut dyn SceneBuilder, n: usize, radius: f64) {
    let cols = 700.0;
    for i in 0..n {
        let x = ((i % 700) as f64) + 0.5;
        let y = ((i / 700) as f64 % 700.0) + 0.5;
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(rgb8(200, 40, 40)),
            None,
            &kurbo::Circle::new((x % cols, y), radius).to_path(0.2),
            PickId::Skip,
        );
    }
}

/// A scene past the compute-shader backend's draw budget renders here.
///
/// That backend caps a scene at `MAX_DRAW_INFO_WORDS` (`1 << 18`) draw-info
/// words, one per solid fill, and rejects anything longer outright. Sparse
/// strips size their GPU buffers to the scene's actual content, so there is no
/// equivalent ceiling — this is the regression test for that claim.
#[test]
fn a_scene_past_the_compute_backend_draw_budget_still_renders() {
    /// Above the other backend's `MAX_DRAW_INFO_WORDS` (`1 << 18`), which is
    /// the whole point of the count.
    const MARKS: usize = 300_000;
    const _: () = assert!(MARKS > (1usize << 18));

    let mut r = HybridRenderer::new().expect("init");
    many_marks(r.scene(), MARKS, 0.4);
    let mut out = vec![0u8; (700 * 500 * 4) as usize];
    r.render_to_buffer(700, 500, rgb8(0, 0, 0), &mut out)
        .expect("a scene this size must still render");

    // The compute-shader backend's failure mode is an all-zero target, so
    // "did it draw anything" is the assertion that distinguishes them.
    let lit = out.chunks_exact(4).filter(|px| px[0] > 40).count();
    assert!(
        lit > 10_000,
        "expected a dense scatter, found {lit} lit pixels"
    );

    // And the same scene is a hard error on the other backend, which is what
    // makes the number above worth asserting.
    #[cfg(feature = "vello")]
    {
        use hephaestus::backend::vello::VelloRenderer;
        let mut v = VelloRenderer::new().expect("init");
        many_marks(v.scene(), MARKS, 0.4);
        let mut vout = vec![0u8; (700 * 500 * 4) as usize];
        assert!(
            matches!(
                v.render_to_buffer(700, 500, rgb8(0, 0, 0), &mut vout),
                Err(hephaestus::BackendError::SceneTooLarge { .. })
            ),
            "the compute-shader backend is expected to reject this scene"
        );
    }
}

/// Deep clip and blend nesting stays correct and stays within the layer
/// budget.
///
/// Intermediate layer textures are a finite, configurable resource
/// (`LayersConfig::max_textures`) surfaced as `RenderError::IntermediateTexture`,
/// and deep nesting is what consumes them — which matters here because the
/// composition layer nests patches. Measured to 256 deep without a budget
/// failure, so the textures are evidently pooled rather than one per layer.
#[test]
fn deep_layer_nesting_neither_errors_nor_distorts() {
    use hephaestus::blend::BlendMode;

    for depth in [1usize, 16, 64, 256] {
        let mut r = HybridRenderer::new().expect("init");
        {
            let scene = r.scene();
            // One outer clip, then the rest re-clipping to the same rect, so
            // depth is the only variable.
            for i in 0..depth {
                let inset = if i == 0 { 0.0 } else { 5.0 };
                scene.push_layer(
                    BlendMode::NORMAL,
                    1.0,
                    Affine::IDENTITY,
                    &Rect::new(inset, inset, 100.0 - inset, 100.0 - inset).to_path(0.1),
                );
            }
            fill(
                scene,
                Rect::new(0.0, 0.0, 100.0, 100.0),
                [255, 0, 0],
                PickId::Skip,
            );
            for _ in 0..depth {
                scene.pop_layer();
            }
        }
        let mut out = buf();
        r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
            .unwrap_or_else(|e| panic!("nesting {depth} deep failed: {e}"));

        let lit = out.chunks_exact(4).filter(|px| px[0] > 40).count();
        // 90 x 90 once the 5px inset applies; the un-inset depth-1 case fills.
        let expected = if depth == 1 { 100 * 100 } else { 90 * 90 };
        assert_eq!(
            lit, expected,
            "nesting {depth} deep changed the clipped area"
        );
    }
}

// ─── Presenting straight into the swap chain ────────────────────────────────

/// Render into a target of `format` and read it back as RGBA bytes.
fn render_into_format(
    device: &hephaestus::wgpu::Device,
    queue: &hephaestus::wgpu::Queue,
    format: hephaestus::wgpu::TextureFormat,
    picking: bool,
) -> (Vec<u8>, HybridRenderer) {
    use hephaestus::{wgpu, WgpuRenderer};

    let mut r = if picking {
        HybridRenderer::with_device_and_picking(device, queue)
    } else {
        HybridRenderer::with_device(device, queue)
    }
    .expect("init");
    r.set_target_format(format);
    coverage_sensitive_scene(r.scene());

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hybrid_direct.target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // Exactly what a swap-chain texture carries.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    r.render_to_texture(&view, W, H, rgb8(0, 0, 0))
        .expect("direct render");

    let padded =
        (W * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    pollster::block_on(rx.receive())
        .expect("sender dropped")
        .expect("map_async");
    let row = (W as usize) * 4;
    let mut out = vec![0u8; (W * H * 4) as usize];
    {
        let data = slice.get_mapped_range();
        for y in 0..H as usize {
            out[y * row..(y + 1) * row]
                .copy_from_slice(&data[y * padded as usize..y * padded as usize + row]);
        }
    }
    readback.unmap();
    // Normalise to RGBA so the two formats are comparable.
    if matches!(format, wgpu::TextureFormat::Bgra8Unorm) {
        for px in out.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    (out, r)
}

/// Rasterising straight into a `Bgra8Unorm` swap-chain texture must produce
/// the same image as rendering into the `Rgba8Unorm` intermediate.
///
/// This is what lets the window host skip the intermediate texture and its
/// per-frame blit entirely on this backend.
#[test]
fn presenting_directly_matches_the_intermediate_format() {
    use hephaestus::wgpu;

    let (device, queue) = make_device();
    let (rgba, _) = render_into_format(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, false);
    let (bgra, _) = render_into_format(&device, &queue, wgpu::TextureFormat::Bgra8Unorm, false);
    assert_eq!(
        first_difference(&rgba, &bgra),
        None,
        "the swap-chain format changed the rendered image"
    );
}

/// Picking survives a swap-chain-format target.
///
/// One renderer targets one format, so the pick target takes the display's —
/// which means a `Bgra8Unorm` surface puts the encoded id's channels in the
/// wrong order until `read_hitmap` swaps them back. Without that, every id
/// comes back with its red and blue bytes transposed.
#[test]
fn picking_survives_a_bgra_target() {
    use hephaestus::wgpu;

    let (device, queue) = make_device();
    // Distinct low and high bytes, so a red/blue transposition cannot go
    // unnoticed: 0x0000FF would survive a swap, 0x0000A1 would not.
    let id = 0x0000A1;
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ] {
        let mut r = HybridRenderer::with_device_and_picking(&device, &queue).expect("init");
        r.set_target_format(format);
        fill(
            r.scene(),
            Rect::new(20.0, 20.0, 80.0, 80.0),
            [255, 0, 0],
            PickId::Id(id),
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            use hephaestus::WgpuRenderer;
            r.render_to_texture(&view, W, H, rgb8(0, 0, 0))
                .expect("render");
        }
        assert_eq!(
            pick(&r, Point::new(50.0, 50.0)),
            Some(id),
            "id came back wrong on a {format:?} target"
        );
        assert_eq!(
            pick(&r, Point::new(5.0, 5.0)),
            None,
            "empty space on {format:?}"
        );
    }
}

// ─── Redrawing without querying ─────────────────────────────────────────────

/// The index is rebuilt from scratch each frame, so a redraw that changes an
/// id changes the answer — there is no stale-hitmap window to reason about.
#[test]
fn a_redraw_replaces_the_previous_index() {
    let mut out = buf();
    let mut r = HybridRenderer::with_picking().expect("init");
    let square = Rect::new(20.0, 20.0, 80.0, 80.0);

    fill(r.scene(), square, [255, 0, 0], PickId::Id(11));
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");
    assert_eq!(pick(&r, Point::new(50.0, 50.0)), Some(11));

    r.scene().clear();
    fill(r.scene(), square, [0, 255, 0], PickId::Id(22));
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");
    assert_eq!(px(&out, 50, 50), [0, 255, 0, 255], "display updated");
    assert_eq!(
        pick(&r, Point::new(50.0, 50.0)),
        Some(22),
        "and so did the index"
    );
}

/// A renderer built without picking answers nothing and says so.
#[test]
fn a_renderer_without_picking_holds_no_index() {
    let mut r = HybridRenderer::new().expect("init");
    let mut out = buf();
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Id(11),
    );
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");
    assert!(r.pick_index().is_none());
    assert!(r.pick_index().is_none());
    assert_eq!(pick(&r, Point::new(50.0, 50.0)), None);
}

// ─── Color glyphs ───────────────────────────────────────────────────────────

/// A run of one emoji, and the size to draw it at, or `None` on a machine
/// with no emoji font — where the codepoint resolves to the same face as
/// plain text, and asserting anything about color glyphs would be
/// asserting about the machine.
fn emoji_run() -> Option<(hephaestus::text::TextRun, f32)> {
    use hephaestus::text::{run_layout_glyphs, TextRun, TextStyle};

    let size = 48.0;
    let style = TextStyle::new(size);
    let plain = TextRun::new("A", &style, 96.0);
    let emoji = TextRun::new("\u{1F600}", &style, 96.0);
    let plain_glyphs = run_layout_glyphs(&plain);
    let emoji_glyphs = run_layout_glyphs(&emoji);
    match (plain_glyphs.first(), emoji_glyphs.first()) {
        (Some(a), Some(b)) if a.font != b.font => Some((emoji, size)),
        _ => {
            println!("no emoji font resolved for U+1F600; skipping");
            None
        }
    }
}

/// Draw `run` at (10, 20) under `transform` and return the frame.
fn render_text(
    r: &mut HybridRenderer,
    run: &hephaestus::text::TextRun,
    transform: Affine,
    pick: PickId,
) -> Vec<u8> {
    hephaestus::text::draw_text(
        r.scene(),
        run,
        10.0,
        20.0,
        &Brush::Solid(rgb8(0, 0, 0)),
        transform,
        pick,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(255, 255, 255), &mut out)
        .expect("render");
    out
}

/// Pixels that are not the background, and the box around them.
fn inked(out: &[u8]) -> (usize, (u32, u32, u32, u32)) {
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (W, H, 0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if px(out, x, y) != [255, 255, 255, 255] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                n += 1;
            }
        }
    }
    (n, (x0, y0, x1, y1))
}

/// Apple Color Emoji and most Android emoji carry their glyphs as PNG
/// strikes, and the outline beside a strike is empty — so a backend that
/// ignores strikes draws the glyph as nothing at all.
#[test]
fn a_bitmap_color_glyph_draws_ink() {
    let Some((emoji, _)) = emoji_run() else {
        return;
    };
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let out = render_text(&mut r, &emoji, Affine::IDENTITY, PickId::Skip);
    let (n, _) = inked(&out);
    assert!(n > 100, "expected emoji ink, found {n} pixels");
}

/// Text either side of an emoji still draws: the run is split, not
/// replaced.
#[test]
fn a_run_mixing_text_and_a_bitmap_color_glyph_draws_both() {
    use hephaestus::text::{TextRun, TextStyle};

    if emoji_run().is_none() {
        return;
    }
    let style = TextStyle::new(24.0);
    let mixed = TextRun::new("Hi \u{1F600} yes", &style, 96.0);
    let plain = TextRun::new("Hi  yes", &style, 96.0);

    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let with_emoji = render_text(&mut r, &mixed, Affine::IDENTITY, PickId::Skip);
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let without = render_text(&mut r, &plain, Affine::IDENTITY, PickId::Skip);

    let (n, _) = inked(&with_emoji);
    let (baseline, _) = inked(&without);
    assert!(baseline > 20, "expected text ink, found {baseline} pixels");
    assert!(
        n > baseline,
        "the emoji must add ink, not replace it: {n} against {baseline}"
    );
}

/// The case the rasteriser's own strike path cannot serve: its glyph atlas
/// takes no rotation, and what it falls back to reaches the render pipeline
/// as CPU pixels it rejects. Drawn as an image there is nothing special
/// about a rotated strike.
#[test]
fn a_rotated_bitmap_color_glyph_draws_ink() {
    let Some((emoji, _)) = emoji_run() else {
        return;
    };
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let out = render_text(&mut r, &emoji, Affine::rotate(0.3), PickId::Skip);
    let (n, _) = inked(&out);
    assert!(n > 100, "expected rotated emoji ink, found {n} pixels");
}

/// A colour glyph picks as the caller's id, whole. The rasteriser splits it
/// into a bitmap strike drawn as an image, which must not become a pick
/// target of its own.
#[test]
fn a_bitmap_color_glyph_picks_as_one_id() {
    let Some((emoji, _)) = emoji_run() else {
        return;
    };
    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    render_text(&mut r, &emoji, Affine::IDENTITY, PickId::Id(7));

    let mut ids: Vec<u32> = Vec::new();
    for y in 0..H {
        for x in 0..W {
            if let Some(id) = pick(&r, Point::new(f64::from(x), f64::from(y))) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    assert_eq!(ids, vec![7], "the emoji must pick as its own id alone");
}

/// The compute-shader backend resolves strikes itself, so it is the
/// reference for placement: the same emoji, the same size, the same
/// origin, and the two frames should agree.
#[cfg(feature = "vello")]
#[test]
fn a_bitmap_color_glyph_lands_where_the_other_backend_puts_it() {
    use hephaestus::backend::vello::VelloRenderer;

    let Some((emoji, _)) = emoji_run() else {
        return;
    };
    let mut h = HybridRenderer::new().expect("hybrid renderer init");
    let hybrid = render_text(&mut h, &emoji, Affine::IDENTITY, PickId::Skip);

    let mut v = VelloRenderer::new().expect("vello renderer init");
    hephaestus::text::draw_text(
        v.scene(),
        &emoji,
        10.0,
        20.0,
        &Brush::Solid(rgb8(0, 0, 0)),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let mut compute = buf();
    v.render_to_buffer(W, H, rgb8(255, 255, 255), &mut compute)
        .expect("render");

    assert_eq!(inked(&hybrid).1, inked(&compute).1, "ink box");
    let differing = hybrid
        .chunks_exact(4)
        .zip(compute.chunks_exact(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    assert_eq!(
        differing, 0,
        "{differing} pixels disagree with the reference"
    );
}
