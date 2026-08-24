//! Images in rich text, end to end: `![](name)` in a geom row, in a
//! chrome slot, and as a break label, plus the placeholder a location
//! that gives nothing draws.
//!
//! Everything asserts against `RecordingScene`, so the whole file runs
//! without a GPU. What it checks is the two things a caller can see:
//! whether a `DrawImage` op came out at the size the rules say, and
//! whether the slot reserved room for it.

use hephaestus::brush::Image;
use hephaestus::color::rgb8;
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::{Rect, Shape, Size};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{
    scale, Axis, AxisPlacement, ImageRegistry, Plot, PlotComposition, TextGeom,
};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::Value;
use hephaestus::scene::recording::{Op, RecordingScene};
use hephaestus::style_vocab::Palette;
use hephaestus::text::rich::{draw_rich_text, RichAnchor, RichTextRun, RichTextStyleSheet};
use hephaestus::text::TextStyle;

const DPI: f64 = 96.0;
const BASE_PT: f32 = 12.0;

/// A solid opaque image of the given pixel size.
fn solid(width: u32, height: u32) -> Image {
    let px = vec![255u8; (width as usize) * (height as usize) * 4];
    hephaestus::image::from_rgba8(width, height, px).expect("valid buffer")
}

/// A register holding one 40×20 image under `"wide"` and one 20×20
/// under `"square"`.
fn register() -> ImageRegistry {
    let mut r = ImageRegistry::new();
    r.insert("wide", solid(40, 20));
    r.insert("square", solid(20, 20));
    r
}

/// Shape `source` against `images` at the base size.
fn shape(source: &str, images: &ImageRegistry) -> RichTextRun {
    RichTextRun::new_with_images(
        source,
        &TextStyle::new(BASE_PT),
        rgb8(0, 0, 0),
        &RichTextStyleSheet::new(),
        &Palette::default(),
        DPI,
        images,
    )
}

/// Draw a shaped run and hand back the ops it emitted.
fn record(run: &RichTextRun) -> RecordingScene {
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        run,
        0.0,
        0.0,
        RichAnchor::default(),
        hephaestus::geometry::Affine::IDENTITY,
        hephaestus::pick::PickId::Skip,
    );
    scene
}

/// Every image blit in the scene, as `(width, height)` in pixels after
/// the op's own transform.
fn image_boxes(scene: &RecordingScene) -> Vec<(f64, f64)> {
    scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawImage {
                image, transform, ..
            } => {
                let c = transform.as_coeffs();
                Some((
                    f64::from(image.width) * c[0],
                    f64::from(image.height) * c[3],
                ))
            }
            _ => None,
        })
        .collect()
}

/// pt → px at the test dpi.
fn px(pt: f64) -> f64 {
    pt * DPI / 72.0
}

// ─── Sizing ─────────────────────────────────────────────────────────────────

#[test]
fn an_inline_image_is_one_em_tall_and_keeps_its_aspect() {
    let run = shape("text ![](wide) text", &register());
    let boxes = image_boxes(&record(&run));
    assert_eq!(boxes.len(), 1, "one tag, one blit");
    let (w, h) = boxes[0];
    assert!(
        (h - px(f64::from(BASE_PT))).abs() < 0.5,
        "an inline image stands one em tall: {h} vs {}",
        px(f64::from(BASE_PT))
    );
    assert!(
        (w - h * 2.0).abs() < 0.5,
        "a 40×20 image is twice as wide as it is tall: {w}×{h}"
    );
}

#[test]
fn a_size_span_scales_an_inline_image() {
    let small = image_boxes(&record(&shape("a ![](square) b", &register())));
    let large = image_boxes(&record(&shape("a {.24 ![](square)} b", &register())));
    assert!(
        (large[0].1 - small[0].1 * 2.0).abs() < 1.0,
        "24pt is twice 12pt: {:?} vs {:?}",
        large[0],
        small[0]
    );
}

#[test]
fn a_lone_image_paragraph_fills_the_width_it_is_broken_to() {
    let run = shape("![](wide)", &register());
    run.set_max_width(300.0, hephaestus::style_vocab::HAlign::Start);
    let boxes = image_boxes(&record(&run));
    assert_eq!(boxes.len(), 1);
    let (w, h) = boxes[0];
    assert!(
        (w - 300.0).abs() < 1.0,
        "a block image fills its column: {w}"
    );
    assert!(
        (h - 150.0).abs() < 1.0,
        "and keeps the 2:1 aspect ratio: {w}×{h}"
    );
}

#[test]
fn a_lone_image_paragraph_takes_its_own_size_at_natural_width() {
    let run = shape("![](wide)", &register());
    let boxes = image_boxes(&record(&run));
    assert!(
        (boxes[0].0 - px(40.0)).abs() < 1.0,
        "with no column to fill, pixels read as pt: {:?}",
        boxes[0]
    );
}

#[test]
fn a_tall_image_grows_the_box_the_run_reports() {
    let plain = shape("text", &register());
    let with_image = shape("text ![](square)", &register());
    assert!(
        with_image.inked_height() > plain.inked_height(),
        "an image taller than the text has to be measured: {} vs {}",
        with_image.inked_height(),
        plain.inked_height()
    );
}

// ─── The placeholder ────────────────────────────────────────────────────────

#[test]
fn an_unresolvable_location_draws_a_framed_cross() {
    let scene = record(&shape("a ![](no/such/file.png) b", &register()));
    assert!(
        image_boxes(&scene).is_empty(),
        "there are no pixels to blit"
    );
    let strokes: Vec<_> = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .collect();
    assert_eq!(
        strokes.len(),
        2,
        "the placeholder is one cross and one frame"
    );
}

#[test]
fn a_registered_name_wins_over_the_location_it_spells() {
    // The name looks like a path, and nothing is there — but it is
    // registered, so the picture draws and the placeholder does not.
    let mut images = ImageRegistry::new();
    images.insert("assets/logo.png", solid(30, 30));
    let scene = record(&shape("![](assets/logo.png) x", &images));
    assert_eq!(image_boxes(&scene).len(), 1);
}

#[test]
fn a_broken_inline_image_is_square() {
    let run = shape("a ![](nope) b", &register());
    let scene = record(&run);
    let boxes: Vec<Rect> = scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Stroke { path, .. } => Some(Shape::bounding_box(path)),
            _ => None,
        })
        .collect();
    let cross = boxes.first().expect("a cross");
    assert!(
        (cross.width() - cross.height()).abs() < 1.0,
        "an unknown aspect ratio makes it square: {cross:?}"
    );
}

#[test]
fn a_broken_block_image_stays_a_square_rather_than_filling_the_column() {
    // Nothing behind the tag means no aspect ratio, and a column-wide
    // box would need one to get a height. A missing picture also has no
    // claim on the space a real one would have taken.
    let run = shape("![](nope)", &register());
    run.set_max_width(330.0, hephaestus::style_vocab::HAlign::Start);
    let frame = record(&run)
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Stroke { path, .. } => Some(Shape::bounding_box(path)),
            _ => None,
        })
        .next_back()
        .expect("a frame");
    assert!(
        (frame.width() - frame.height()).abs() < 1.0,
        "square: {frame:?}"
    );
    // One em, less the frame stroke it insets by.
    assert!(
        frame.width() < px(f64::from(BASE_PT)) + 1.0,
        "at text size, not column width: {frame:?}"
    );
}

#[test]
fn a_block_image_centres_in_its_column() {
    // The `img` selector carries marquee's `align = "center"`, and a
    // block image's paragraph inherits it — visible once the image is
    // narrower than the column, which is what a fixed-size sheet entry
    // would do. Here the natural width is the image's own.
    let run = shape("![](wide)", &register());
    let natural = run.natural_width() as f32;
    run.set_max_width(natural, hephaestus::style_vocab::HAlign::Start);
    let boxes = image_boxes(&record(&run));
    assert!(
        (boxes[0].0 - f64::from(natural)).abs() < 1.0,
        "a column exactly its own width leaves nothing to centre in: {:?} in {natural}",
        boxes[0]
    );
}

// ─── Reading a location ─────────────────────────────────────────────────────

/// Write `image` to a uniquely named PNG under the temp dir and hand
/// back the path. Named per test so a parallel run cannot collide.
fn temp_png(tag: &str, image: &Image) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("hephaestus-rich-image-{tag}.png"));
    hephaestus::image::write_png(&path, image.width, image.height, image.data.as_ref())
        .expect("write the fixture");
    path
}

#[test]
fn a_path_no_one_registered_is_read_from_disk() {
    let path = temp_png("from-disk", &solid(40, 20));
    let source = format!("a ![]({}) b", path.display());
    let images = ImageRegistry::new();
    let boxes = image_boxes(&record(&shape(&source, &images)));
    assert_eq!(boxes.len(), 1, "the file on disk supplied the pixels");
    assert!(
        (boxes[0].0 - boxes[0].1 * 2.0).abs() < 0.5,
        "and its own aspect ratio: {:?}",
        boxes[0]
    );
    // Reading it also files it under the name that spelled it, which is
    // what a document embeds.
    assert!(images.loaded_names().contains(&path.display().to_string()));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_that_is_not_an_image_draws_the_placeholder() {
    let path = std::env::temp_dir().join("hephaestus-rich-image-garbage.png");
    std::fs::write(&path, b"not a png").expect("write the fixture");
    let source = format!("![]({})", path.display());
    let scene = record(&shape(&source, &ImageRegistry::new()));
    assert!(image_boxes(&scene).is_empty(), "nothing decoded");
    assert!(
        scene.ops.iter().any(|op| matches!(op, Op::Stroke { .. })),
        "so the placeholder is what draws"
    );
    std::fs::remove_file(&path).ok();
}

// ─── Through the plot layer ─────────────────────────────────────────────────

fn panel() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

/// A theme whose chrome text parses markdown, so titles and break
/// labels take the rich path.
fn markdown_theme() -> Theme {
    let mut theme = Theme::default();
    theme.text.markdown = Some(true);
    theme
}

/// Render a composition into a recording scene at a fixed size.
fn render(view: &mut PlotComposition) -> RecordingScene {
    let mut scene = RecordingScene::default();
    view.render(&mut scene, Size::new(400.0, 300.0), DPI);
    scene
}

/// A plot on the single panel, bound to both axes.
fn plot(images: ImageRegistry) -> Plot {
    Plot::new(&panel(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .image_registry(images)
}

/// A composition holding `plot`, with continuous scales unless the
/// caller replaces them.
fn view(plot: Plot) -> PlotComposition {
    PlotComposition::new(&panel())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .with_plot(plot)
}

/// One text row at the middle of the panel.
fn text_row(text: &str, markdown: bool) -> TextGeom {
    TextGeom::builder()
        .set("x", vec![5.0_f64])
        .set("y", vec![5.0_f64])
        .set("text", vec![text.to_string()])
        .set("markdown", markdown)
        .build()
}

#[test]
fn a_text_geom_row_draws_its_image() {
    let mut p = plot(register());
    p.add_geom(text_row("before ![](wide) after", true));
    assert_eq!(
        image_boxes(&render(&mut view(p))).len(),
        1,
        "the row's image"
    );
}

#[test]
fn a_plain_text_row_still_shows_the_markup() {
    // Markdown off means the tag is text, so nothing is blitted.
    let mut p = plot(register());
    p.add_geom(text_row("before ![](wide) after", false));
    assert!(image_boxes(&render(&mut view(p))).is_empty());
}

#[test]
fn an_image_break_label_draws_once_per_tick() {
    let mut p = plot(register());
    p.add_geom(text_row("", false));
    p.add_axis(Axis::rail(
        "x_axis",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    let mut view = PlotComposition::new(&panel())
        .theme(markdown_theme())
        .add_scale(
            "x_axis",
            scale::discrete([Value::from("![](square)"), Value::from("![](wide)")]),
        )
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .with_plot(p);
    assert_eq!(
        image_boxes(&render(&mut view)).len(),
        2,
        "one image per break label"
    );
}

#[test]
fn an_image_break_label_reserves_room_for_itself() {
    // Straight through the axis measure, which is what the layout
    // solver asks: a 20pt-tall picture needs more of the band than one
    // line of 12pt text.
    let images = std::sync::Arc::new(register());
    let band = |label: &str| {
        let axis_scale = scale::discrete([Value::from(label.to_string())]);
        hephaestus::plot::chrome::axis::measure(
            &axis_scale,
            AxisSide::Bottom,
            DPI,
            &markdown_theme(),
            &images,
        )
        .height_at(400.0, DPI)
    };
    let text = band("x");
    let image = band("![](square)");
    assert!(
        image > text,
        "an image label has to reserve more than a text one: {image} vs {text}"
    );
}

#[test]
fn a_title_image_resolves_against_the_plot_register() {
    let mut p = plot(register()).title("A plot ![](square)");
    p.add_geom(text_row("", false));
    let mut view = view(p).theme(markdown_theme());
    assert_eq!(
        image_boxes(&render(&mut view)).len(),
        1,
        "the title's image"
    );
}

#[test]
fn a_composition_title_resolves_against_the_composition_register() {
    let mut p = plot(ImageRegistry::new());
    p.add_geom(text_row("", false));
    let mut view = view(p)
        .theme(markdown_theme())
        .image_registry(register())
        .title("Figure ![](square)");
    assert_eq!(
        image_boxes(&render(&mut view)).len(),
        1,
        "the composition's own image"
    );
}

#[test]
fn two_registers_do_not_share_one_cached_label() {
    // Same markdown, same style, different pixels behind the name: the
    // cache key has to carry the register, or the second plot draws the
    // first one's image.
    let boxes = |image: Image| {
        let mut images = ImageRegistry::new();
        images.insert("logo", image);
        let mut p = plot(images).title("![](logo) t");
        p.add_geom(text_row("", false));
        let mut view = view(p).theme(markdown_theme());
        image_boxes(&render(&mut view))
    };
    let wide = boxes(solid(40, 20));
    let tall = boxes(solid(20, 40));
    assert!(
        wide[0].0 > wide[0].1 && tall[0].1 > tall[0].0,
        "each plot draws its own register's image: {wide:?} then {tall:?}"
    );
}
