//! The pick index, end to end, against a scene with no renderer behind it.
//!
//! Needs no cargo features at all: hit testing is something a scene does, so
//! it is testable without a GPU, a rasteriser or a codec. The suite is the
//! successor to the GPU hitmap's round-trip tests, and its oracle is a naive
//! scan over the recorded primitives rather than a rendered image.

use hephaestus::geometry::{Affine, Point, Rect};
use hephaestus::path::FillRule;
use hephaestus::pick::{PickId, PickIndexScene, PickScope};
use hephaestus::scene::recording::RecordingScene;
use hephaestus::{primitives, Brush, SceneBuilder};

fn rgb() -> Brush {
    Brush::Solid(hephaestus::color::rgb8(10, 20, 30))
}

fn scene() -> PickIndexScene<RecordingScene> {
    PickIndexScene::new(RecordingScene::new(), true)
}

/// Fill an axis-aligned rect tagged with `id`.
fn fill_rect(s: &mut PickIndexScene<RecordingScene>, r: Rect, id: PickId) {
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &rgb(),
        None,
        &primitives::rect(r),
        id,
    );
}

// ── Ported from the deleted GPU round-trip suite ────────────────────────

#[test]
fn an_index_nothing_was_drawn_into_reports_nothing() {
    let s = scene();
    assert!(s.index().is_empty());
    assert_eq!(s.pick_at(Point::new(5.0, 5.0)), None);
    assert!(s.hits_at(Point::new(5.0, 5.0)).is_empty());
}

#[test]
fn ids_are_reported_at_known_positions() {
    let mut s = scene();
    fill_rect(&mut s, Rect::new(10.0, 10.0, 60.0, 60.0), PickId::Id(7));
    fill_rect(&mut s, Rect::new(100.0, 10.0, 150.0, 60.0), PickId::Id(42));
    fill_rect(
        &mut s,
        Rect::new(10.0, 100.0, 60.0, 150.0),
        PickId::Id(9000),
    );

    assert_eq!(s.pick_at(Point::new(35.0, 35.0)), Some(7));
    assert_eq!(s.pick_at(Point::new(125.0, 35.0)), Some(42));
    assert_eq!(s.pick_at(Point::new(35.0, 125.0)), Some(9000));
    // The gaps between them are misses.
    assert_eq!(s.pick_at(Point::new(80.0, 35.0)), None);
    assert_eq!(s.pick_at(Point::new(200.0, 200.0)), None);
}

#[test]
fn block_occludes_what_is_under_it() {
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 100.0, 100.0), PickId::Id(7));
    fill_rect(&mut s, Rect::new(40.0, 40.0, 60.0, 60.0), PickId::Block);

    assert_eq!(s.pick_at(Point::new(10.0, 10.0)), Some(7));
    assert_eq!(
        s.pick_at(Point::new(50.0, 50.0)),
        None,
        "Block reports nothing and hides what is beneath"
    );
    // `Block` is an occluder, not a target: it truncates the walk and is
    // itself absent from the result, so a caller sees empty space.
    assert!(s.hits_at(Point::new(50.0, 50.0)).is_empty());
    // The mark beneath really is a candidate — it is the Block that hides it.
    assert_eq!(s.hits_at(Point::new(10.0, 10.0)).len(), 1);
}

#[test]
fn no_id_value_is_reserved_and_block_is_the_only_occluder() {
    // `Id(0)` is an ordinary id. It was the no-hit sentinel only while ids
    // were packed into a texture's colour channels.
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 100.0, 100.0), PickId::Id(7));
    fill_rect(&mut s, Rect::new(40.0, 40.0, 60.0, 60.0), PickId::Id(0));
    assert_eq!(s.pick_at(Point::new(50.0, 50.0)), Some(0), "0 is an id");
    assert_eq!(
        s.hits_at(Point::new(50.0, 50.0)).len(),
        2,
        "and occludes nothing"
    );

    // `Block` is the variant that occludes, and it still does.
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 100.0, 100.0), PickId::Id(7));
    fill_rect(&mut s, Rect::new(40.0, 40.0, 60.0, 60.0), PickId::Block);
    assert_eq!(s.pick_at(Point::new(50.0, 50.0)), None);
    assert!(s.hits_at(Point::new(50.0, 50.0)).is_empty());
}

#[test]
fn skip_is_absent_rather_than_transparent() {
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 100.0, 100.0), PickId::Id(7));
    fill_rect(&mut s, Rect::new(40.0, 40.0, 60.0, 60.0), PickId::Skip);

    assert_eq!(s.pick_at(Point::new(50.0, 50.0)), Some(7));
    // Stronger than the hitmap could assert: the skipped fill was never
    // recorded, so it cannot occlude, blend or cost anything at query time.
    assert_eq!(s.index().len(), 1);
}

#[test]
fn ids_past_24_bits_survive_intact() {
    // The hitmap packed ids into RGB and truncated the high byte. With no
    // encoding there is nothing to truncate.
    let mut s = scene();
    fill_rect(
        &mut s,
        Rect::new(0.0, 0.0, 10.0, 10.0),
        PickId::Id(0x0100_0001),
    );
    assert_eq!(s.pick_at(Point::new(5.0, 5.0)), Some(0x0100_0001));

    let mut s = scene();
    fill_rect(
        &mut s,
        Rect::new(0.0, 0.0, 10.0, 10.0),
        PickId::Id(u32::MAX),
    );
    assert_eq!(s.pick_at(Point::new(5.0, 5.0)), Some(u32::MAX));
}

#[test]
fn a_point_beyond_every_primitive_misses() {
    // The index has no canvas bounds — it answers about geometry, not about
    // a framebuffer — so this is "outside everything", not "off-canvas".
    let mut s = scene();
    fill_rect(&mut s, Rect::new(10.0, 10.0, 20.0, 20.0), PickId::Id(1));
    assert_eq!(s.pick_at(Point::new(-500.0, -500.0)), None);
    assert_eq!(s.pick_at(Point::new(1e6, 1e6)), None);
}

#[test]
fn a_mesh_is_picked_inside_its_triangle() {
    use hephaestus::color::rgb8;
    use hephaestus::mesh::Mesh;
    let mut s = scene();
    let mesh = Mesh::new(
        vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(0.0, 100.0),
        ],
        vec![rgb8(1, 2, 3); 3],
        vec![0, 1, 2],
    );
    s.draw_mesh(&mesh, Affine::IDENTITY, PickId::Id(42));

    assert_eq!(s.pick_at(Point::new(10.0, 10.0)), Some(42));
    // Inside the bounding box, outside the triangle.
    assert_eq!(s.pick_at(Point::new(90.0, 90.0)), None);
}

// ── Z order, coalescing, transforms ─────────────────────────────────────

#[test]
fn overlapping_marks_report_topmost_first_and_all_hits_on_request() {
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 100.0, 100.0), PickId::Id(1));
    fill_rect(&mut s, Rect::new(50.0, 50.0, 150.0, 150.0), PickId::Id(2));

    let hits = s.hits_at(Point::new(75.0, 75.0));
    assert_eq!(hits.len(), 2, "both are under the point");
    assert_eq!(hits[0].id(), Some(2), "later draw is on top");
    assert_eq!(hits[1].id(), Some(1));
    assert!(hits[0].order > hits[1].order);
    assert_eq!(s.pick_at(Point::new(75.0, 75.0)), Some(2));
}

#[test]
fn a_filled_and_stroked_mark_is_one_hit() {
    let mut s = scene();
    let circle = primitives::circle(Point::new(50.0, 50.0), 20.0);
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &rgb(),
        None,
        &circle,
        PickId::Id(5),
    );
    s.stroke(
        &hephaestus::stroke::Stroke::new(3.0),
        Affine::IDENTITY,
        &rgb(),
        None,
        &circle,
        PickId::Id(5),
    );
    // Two primitives were recorded...
    assert!(s.index().len() >= 2);
    // ...but a caller hovering the mark sees one thing.
    let hits = s.hits_at(Point::new(50.0, 50.0));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id(), Some(5));
}

#[test]
fn a_rotated_mark_is_hit_in_its_own_frame() {
    let mut s = scene();
    // A tall thin rect, rotated a quarter turn about its centre.
    let r = Rect::new(-40.0, -5.0, 40.0, 5.0);
    let xf = Affine::translate((100.0, 100.0)) * Affine::rotate(std::f64::consts::FRAC_PI_2);
    s.fill(
        FillRule::NonZero,
        xf,
        &rgb(),
        None,
        &primitives::rect(r),
        PickId::Id(3),
    );

    // Along the rotated long axis (vertical in device space).
    assert_eq!(s.pick_at(Point::new(100.0, 130.0)), Some(3));
    // Along the unrotated long axis, which is now the short one.
    assert_eq!(s.pick_at(Point::new(130.0, 100.0)), None);
}

#[test]
fn a_singular_transform_records_nothing() {
    let mut s = scene();
    s.fill(
        FillRule::NonZero,
        Affine::scale(0.0),
        &rgb(),
        None,
        &primitives::rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Id(1),
    );
    assert!(s.index().is_empty());
    assert_eq!(s.pick_at(Point::new(0.0, 0.0)), None);
}

// ── Clipping ────────────────────────────────────────────────────────────

#[test]
fn a_rounded_clip_rejects_the_cut_corner_and_accepts_it_unclipped() {
    let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
    let corner = Point::new(1.0, 1.0);

    let mut clipped = scene();
    clipped.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &primitives::rounded_rect(panel, 30.0),
    );
    fill_rect(&mut clipped, panel, PickId::Id(1));
    clipped.pop_layer();
    assert_eq!(
        clipped.pick_at(corner),
        None,
        "the rounding cuts this corner away"
    );
    assert_eq!(clipped.pick_at(Point::new(50.0, 50.0)), Some(1));

    let mut plain = scene();
    fill_rect(&mut plain, panel, PickId::Id(1));
    assert_eq!(plain.pick_at(corner), Some(1));
}

#[test]
fn a_nested_clip_rejects_what_either_level_rejects() {
    let mut s = scene();
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &primitives::rect(Rect::new(0.0, 0.0, 100.0, 100.0)),
    );
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &primitives::rect(Rect::new(50.0, 0.0, 200.0, 100.0)),
    );
    fill_rect(&mut s, Rect::new(0.0, 0.0, 200.0, 200.0), PickId::Id(1));
    s.pop_layer();
    s.pop_layer();

    assert_eq!(s.pick_at(Point::new(75.0, 50.0)), Some(1), "inside both");
    assert_eq!(s.pick_at(Point::new(25.0, 50.0)), None, "outside the inner");
    assert_eq!(
        s.pick_at(Point::new(150.0, 50.0)),
        None,
        "outside the outer"
    );
}

#[test]
fn a_primitive_entirely_outside_its_clip_is_never_recorded() {
    let mut s = scene();
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &primitives::rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
    );
    fill_rect(&mut s, Rect::new(500.0, 500.0, 600.0, 600.0), PickId::Id(1));
    s.pop_layer();
    assert!(s.index().is_empty());
}

#[test]
fn an_unbalanced_pop_layer_does_not_panic() {
    let mut s = scene();
    s.pop_layer();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 10.0, 10.0), PickId::Id(1));
    assert_eq!(s.pick_at(Point::new(5.0, 5.0)), Some(1));
}

// ── Scopes ──────────────────────────────────────────────────────────────

#[test]
fn a_hit_carries_the_scope_chain_it_was_drawn_inside() {
    let mut s = scene();
    s.push_pick_scope(&PickScope::group("plot").with_name("a").with_index(0));
    s.push_pick_scope(&PickScope::group("region").with_name("panel"));
    fill_rect(&mut s, Rect::new(0.0, 0.0, 10.0, 10.0), PickId::Id(1));
    s.pop_pick_scope();
    s.pop_pick_scope();

    let hits = s.hits_at(Point::new(5.0, 5.0));
    assert_eq!(hits.len(), 1);
    let kinds: Vec<&str> = hits[0].path.frames().iter().map(|f| f.kind()).collect();
    assert_eq!(kinds, vec!["plot", "region"]);
    assert_eq!(hits[0].path.find("plot").and_then(|f| f.name()), Some("a"));
}

#[test]
fn a_target_scope_makes_an_unidentified_primitive_pickable() {
    let mut s = scene();
    // Chrome draws with `Skip` and has no id of its own.
    s.push_pick_scope(&PickScope::target("part").with_name("axis_tick_label"));
    fill_rect(&mut s, Rect::new(0.0, 0.0, 10.0, 10.0), PickId::Skip);
    s.pop_pick_scope();

    let hits = s.hits_at(Point::new(5.0, 5.0));
    assert_eq!(hits.len(), 1, "a Target scope indexes what it contains");
    assert_eq!(hits[0].pick_id, PickId::Skip);
    assert_eq!(hits[0].id(), None, "chrome carries no authoring id");
    assert_eq!(
        hits[0].path.find("part").and_then(|f| f.name()),
        Some("axis_tick_label")
    );
}

#[test]
fn a_group_scope_leaves_an_unidentified_primitive_alone() {
    let mut s = scene();
    // The default, and what a dense geom with no `pick_id` channel gets.
    s.push_pick_scope(&PickScope::group("geom").with_index(0));
    for i in 0..1000 {
        fill_rect(
            &mut s,
            Rect::new(i as f64, 0.0, i as f64 + 1.0, 1.0),
            PickId::Skip,
        );
    }
    s.pop_pick_scope();
    assert!(
        s.index().is_empty(),
        "Group must preserve the pre-scope behaviour of Skip"
    );
}

// ── Brushing and lasso ──────────────────────────────────────────────────

fn marks() -> PickIndexScene<RecordingScene> {
    let mut s = scene();
    // A 5x5 grid of 10x10 marks on a 40px pitch, ids 1..=25.
    let mut id = 1u32;
    for row in 0..5 {
        for col in 0..5 {
            let (x, y) = (col as f64 * 40.0, row as f64 * 40.0);
            fill_rect(&mut s, Rect::new(x, y, x + 10.0, y + 10.0), PickId::Id(id));
            id += 1;
        }
    }
    s
}

#[test]
fn a_marquee_selects_exactly_what_it_encloses() {
    let s = marks();
    // Covers the marks at cols 0-1, rows 0-1 => ids 1, 2, 6, 7.
    let rect = Rect::new(-5.0, -5.0, 55.0, 55.0);
    let mut got: Vec<u32> = s.hits_within(rect).iter().filter_map(|h| h.id()).collect();
    got.sort_unstable();
    assert_eq!(got, vec![1, 2, 6, 7]);
}

#[test]
fn hits_in_is_a_superset_of_hits_within() {
    let s = marks();
    // An edge cutting through the marks at col 1.
    let rect = Rect::new(-5.0, -5.0, 45.0, 55.0);
    let within: Vec<u32> = s.hits_within(rect).iter().filter_map(|h| h.id()).collect();
    let inside: Vec<u32> = s.hits_in(rect).iter().filter_map(|h| h.id()).collect();

    assert!(within.iter().all(|id| inside.contains(id)));
    assert!(
        inside.len() > within.len(),
        "a straddling mark is in `hits_in` only"
    );
}

#[test]
fn a_marquee_selects_through_an_occluder() {
    let mut s = marks();
    fill_rect(
        &mut s,
        Rect::new(-100.0, -100.0, 500.0, 500.0),
        PickId::Block,
    );
    // A region query is not a ray, so `Block` does not truncate it.
    let rect = Rect::new(-5.0, -5.0, 55.0, 55.0);
    let got: Vec<u32> = s.hits_within(rect).iter().filter_map(|h| h.id()).collect();
    assert_eq!(got.len(), 4);
}

#[test]
fn a_clipped_away_mark_is_not_brushable() {
    let mut s = scene();
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &primitives::rect(Rect::new(0.0, 0.0, 20.0, 20.0)),
    );
    fill_rect(&mut s, Rect::new(100.0, 100.0, 110.0, 110.0), PickId::Id(1));
    s.pop_layer();
    assert!(s.hits_in(Rect::new(0.0, 0.0, 500.0, 500.0)).is_empty());
}

#[test]
fn a_concave_lasso_excludes_marks_sitting_in_its_notch() {
    let s = marks();
    // A C shape opening to the right: the notch spans the middle rows over
    // the right-hand columns. Its bounding box covers the whole grid, which
    // is exactly why a bbox test would get this wrong.
    let mut c = hephaestus::path::Path::new();
    c.move_to((-10.0, -10.0));
    c.line_to((200.0, -10.0));
    c.line_to((200.0, 30.0));
    c.line_to((30.0, 30.0));
    c.line_to((30.0, 130.0));
    c.line_to((200.0, 130.0));
    c.line_to((200.0, 200.0));
    c.line_to((-10.0, 200.0));
    c.close_path();

    let got: Vec<u32> = s
        .hits_in_path(&c, FillRule::NonZero)
        .iter()
        .filter_map(|h| h.id())
        .collect();

    // Top row is inside the upper arm; ids 1..=5.
    assert!(got.contains(&1) && got.contains(&5));
    // The notch swallows the right-hand columns of the middle rows.
    assert!(!got.contains(&8), "id 8 sits in the notch");
    assert!(!got.contains(&15), "id 15 sits in the notch");
    // The left column runs down the spine and survives.
    assert!(got.contains(&6) && got.contains(&11));
}

#[test]
fn an_even_odd_lasso_excludes_marks_in_its_hole() {
    let s = marks();
    let mut ring = primitives::rect(Rect::new(-10.0, -10.0, 200.0, 200.0));
    ring.extend(primitives::rect(Rect::new(30.0, 30.0, 130.0, 130.0)).iter());

    let got: Vec<u32> = s
        .hits_in_path(&ring, FillRule::EvenOdd)
        .iter()
        .filter_map(|h| h.id())
        .collect();
    // id 1 is at (0,0), outside the hole; id 13 is at (80,80), inside it.
    assert!(got.contains(&1));
    assert!(!got.contains(&13), "the hole is not selected");
}

// ── The decorator is transparent ────────────────────────────────────────

/// Draw the same thing into a bare recording and a wrapped one; the two
/// recordings must be identical. This is the cheapest possible guarantee
/// that indexing never changes what gets drawn.
#[test]
fn wrapping_a_scene_does_not_change_what_it_records() {
    fn draw(s: &mut dyn SceneBuilder) {
        s.push_layer(
            Default::default(),
            1.0,
            Affine::IDENTITY,
            &primitives::rect(Rect::new(0.0, 0.0, 100.0, 100.0)),
        );
        s.fill(
            FillRule::EvenOdd,
            Affine::translate((5.0, 5.0)),
            &Brush::Solid(hephaestus::color::rgb8(1, 2, 3)),
            None,
            &primitives::circle(Point::new(10.0, 10.0), 4.0),
            PickId::Id(11),
        );
        s.stroke(
            &hephaestus::stroke::Stroke::new(2.0),
            Affine::IDENTITY,
            &Brush::Solid(hephaestus::color::rgb8(4, 5, 6)),
            None,
            &primitives::rect(Rect::new(1.0, 1.0, 9.0, 9.0)),
            PickId::Skip,
        );
        s.pop_layer();
    }

    let mut bare = RecordingScene::new();
    draw(&mut bare);

    for enabled in [true, false] {
        let mut wrapped = PickIndexScene::new(RecordingScene::new(), enabled);
        draw(&mut wrapped);
        assert_eq!(
            wrapped.inner(),
            &bare,
            "enabled = {enabled}: the recording must be identical"
        );
    }
}

#[test]
fn disabling_indexing_draws_the_same_and_records_nothing() {
    let mut off = PickIndexScene::new(RecordingScene::new(), false);
    fill_rect(&mut off, Rect::new(0.0, 0.0, 10.0, 10.0), PickId::Id(1));
    assert!(off.index().is_empty());
    assert_eq!(off.pick_at(Point::new(5.0, 5.0)), None);
    assert!(!off.inner().ops.is_empty(), "but it still drew");
}

#[test]
fn clearing_the_scene_clears_the_index() {
    let mut s = scene();
    fill_rect(&mut s, Rect::new(0.0, 0.0, 10.0, 10.0), PickId::Id(1));
    assert_eq!(s.index().len(), 1);
    s.clear();
    assert!(s.index().is_empty());
    assert_eq!(s.pick_at(Point::new(5.0, 5.0)), None);
}
