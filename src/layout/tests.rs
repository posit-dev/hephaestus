use super::*;

fn approx_eq(a: f64, b: f64, tol: f64, msg: &str) {
    assert!((a - b).abs() <= tol, "{msg}: {a} ≠ {b} (tol {tol})");
}

#[test]
fn simple_2x2_fr_grid() {
    let mut root = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    );
    root.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    root.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));
    root.place_mut(Placement::at(2, 1), Grid::cell().id(CellId(3)));
    root.place_mut(Placement::at(2, 2), Grid::cell().id(CellId(4)));

    let layout = root.solve(Size::new(200.0, 200.0), 96.0);

    let r1 = layout.rect(CellId(1)).unwrap();
    approx_eq(r1.x0, 0.0, 0.5, "r1.x0");
    approx_eq(r1.y0, 0.0, 0.5, "r1.y0");
    approx_eq(r1.x1, 100.0, 0.5, "r1.x1");
    approx_eq(r1.y1, 100.0, 0.5, "r1.y1");

    let r4 = layout.rect(CellId(4)).unwrap();
    approx_eq(r4.x0, 100.0, 0.5, "r4.x0");
    approx_eq(r4.y0, 100.0, 0.5, "r4.y0");
    approx_eq(r4.x1, 200.0, 0.5, "r4.x1");
    approx_eq(r4.y1, 200.0, 0.5, "r4.y1");
}

#[test]
fn headline_example_inset() {
    // 5 columns × 3 rows in an 800×600 viewport at 96 DPI.
    // Inner cell placed at (row 2, cols 3..=5) with a 1 cm left inset and
    // a 25% right inset — should end at 75% of the cell-area width.
    let mut root = Grid::new(vec![Track::Fr(1.0); 5], vec![Track::Fr(1.0); 3]);
    let inner = Grid::cell().id(CellId(42));
    root.place_mut(
        Placement::at(2, 3).span(1, 3).inset(
            Inset::default()
                .left(Extent::cm(1.0))
                .right(Extent::percent(0.25)),
        ),
        inner,
    );

    let layout = root.solve(Size::new(800.0, 600.0), 96.0);
    let r = layout.rect(CellId(42)).unwrap();

    let cell_left = 320.0; // 2/5 × 800
    let cell_width = 480.0; // 3/5 × 800
    let one_cm_px = 96.0 / 2.54;
    approx_eq(r.x0, cell_left + one_cm_px, 0.5, "inner left edge");
    approx_eq(
        r.x1,
        cell_left + cell_width - 0.25 * cell_width,
        0.5,
        "inner right edge",
    );
}

#[test]
fn respect_square_in_wide_viewport() {
    // 1 fr × 1 fr cell with `respect` in a 200×100 viewport →
    // 100×100, centered horizontally (50 px slack on each side).
    let root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)])
        .respect()
        .id(CellId(1));
    let layout = root.solve(Size::new(200.0, 100.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.x0, 50.0, 0.5, "respect x0");
    approx_eq(r.y0, 0.0, 0.5, "respect y0");
    approx_eq(r.x1, 150.0, 0.5, "respect x1");
    approx_eq(r.y1, 100.0, 0.5, "respect y1");
}

#[test]
fn respect_aspect_via_fr_weights() {
    // A 2:1 single cell expressed as `[Fr(2.0)]` × `[Fr(1.0)]` with respect
    // → in a 200×200 viewport, becomes 200×100 centered vertically.
    let root = Grid::new([Track::Fr(2.0)], [Track::Fr(1.0)])
        .respect()
        .id(CellId(1));
    let layout = root.solve(Size::new(200.0, 200.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.x0, 0.0, 0.5, "aspect x0");
    approx_eq(r.x1, 200.0, 0.5, "aspect x1");
    approx_eq(r.y0, 50.0, 0.5, "aspect y0");
    approx_eq(r.y1, 150.0, 0.5, "aspect y1");
}

#[test]
fn respect_children_lay_out_inside() {
    // A 2x2 grid with respect inside a 400×200 viewport: per-fr clamps to
    // min(200, 100) = 100, so the grid is 200×200 centered horizontally.
    // Each of the four placed children is 100×100, inside the grid.
    let mut root = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    )
    .respect();
    root.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(11)));
    root.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(12)));
    root.place_mut(Placement::at(2, 1), Grid::cell().id(CellId(21)));
    root.place_mut(Placement::at(2, 2), Grid::cell().id(CellId(22)));

    let layout = root.solve(Size::new(400.0, 200.0), 96.0);

    // Grid is 200×200, centered horizontally: x in [100, 300], y in [0, 200].
    let r11 = layout.rect(CellId(11)).unwrap();
    approx_eq(r11.x0, 100.0, 0.5, "r11.x0");
    approx_eq(r11.y0, 0.0, 0.5, "r11.y0");
    approx_eq(r11.x1, 200.0, 0.5, "r11.x1");
    approx_eq(r11.y1, 100.0, 0.5, "r11.y1");

    let r22 = layout.rect(CellId(22)).unwrap();
    approx_eq(r22.x0, 200.0, 0.5, "r22.x0");
    approx_eq(r22.y0, 100.0, 0.5, "r22.y0");
    approx_eq(r22.x1, 300.0, 0.5, "r22.x1");
    approx_eq(r22.y1, 200.0, 0.5, "r22.y1");
}

#[test]
fn inset_width_right_anchored() {
    // 1×1 grid in a 400×200 viewport. Child placed at (1,1) with
    // explicit width = 2 cm anchored to the right edge.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(
            Inset::default()
                .right(Extent::px(0.0))
                .width(Extent::cm(2.0)),
        ),
        Grid::cell().id(CellId(7)),
    );
    let layout = root.solve(Size::new(400.0, 200.0), 96.0);
    let r = layout.rect(CellId(7)).unwrap();
    let two_cm_px = 2.0 * 96.0 / 2.54;
    approx_eq(r.x1, 400.0, 0.5, "right edge at cell right");
    approx_eq(r.x0, 400.0 - two_cm_px, 0.5, "left edge = right - 2cm");
    approx_eq(r.y0, 0.0, 0.5, "top edge");
    approx_eq(r.y1, 200.0, 0.5, "bottom edge");
}

#[test]
fn inset_width_no_edges_anchors_left() {
    // width set, no edges: child anchors to the left edge of the cell.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::cm(2.0))),
        Grid::cell().id(CellId(7)),
    );
    let layout = root.solve(Size::new(400.0, 200.0), 96.0);
    let r = layout.rect(CellId(7)).unwrap();
    let two_cm_px = 2.0 * 96.0 / 2.54;
    approx_eq(r.x0, 0.0, 0.5, "left edge at cell left");
    approx_eq(r.x1, two_cm_px, 0.5, "right edge = 2cm");
}

#[test]
fn inset_width_with_leading_edge() {
    // width + left: child starts at left offset, takes explicit width.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(
            Inset::default()
                .left(Extent::cm(1.0))
                .width(Extent::cm(2.0)),
        ),
        Grid::cell().id(CellId(7)),
    );
    let layout = root.solve(Size::new(400.0, 200.0), 96.0);
    let r = layout.rect(CellId(7)).unwrap();
    let one_cm = 96.0 / 2.54;
    let two_cm = 2.0 * 96.0 / 2.54;
    approx_eq(r.x0, one_cm, 0.5, "x0 = 1cm");
    approx_eq(r.x1, one_cm + two_cm, 0.5, "x1 = 1cm + 2cm");
}

#[test]
fn auto_col_sizes_to_fixed_child() {
    // [Auto] × [Fr(1)] in 400×200; the child declares an explicit 100px
    // width via Inset, so the auto col resolves to 100.
    let mut root = Grid::new([Track::Auto], [Track::Fr(1.0)]).id(CellId(0));
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::px(100.0))),
        Grid::cell().id(CellId(1)),
    );
    let layout = root.solve(Size::new(400.0, 200.0), 96.0);
    let root_rect = layout.rect(CellId(0)).unwrap();
    approx_eq(root_rect.x1 - root_rect.x0, 100.0, 0.5, "root width");
    approx_eq(root_rect.y1 - root_rect.y0, 200.0, 0.5, "root height");
}

#[test]
fn auto_row_sizes_to_nested_grid() {
    // Outer rows are [Auto, Fr(1)]. The child in row 1 is itself a grid
    // with a single fixed 30px row → outer row 1 resolves to 30,
    // outer row 2 takes the remainder.
    let inner = Grid::new([Track::Fr(1.0)], [Track::Fixed(Extent::px(30.0))]).id(CellId(11));
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Auto, Track::Fr(1.0)]);
    root.place_mut(Placement::at(1, 1), inner);
    root.place_mut(Placement::at(2, 1), Grid::cell().id(CellId(22)));

    let layout = root.solve(Size::new(200.0, 200.0), 96.0);
    let r11 = layout.rect(CellId(11)).unwrap();
    approx_eq(r11.y0, 0.0, 0.5, "auto row top");
    approx_eq(r11.y1, 30.0, 0.5, "auto row bottom");
    let r22 = layout.rect(CellId(22)).unwrap();
    approx_eq(r22.y0, 30.0, 0.5, "fr row top");
    approx_eq(r22.y1, 200.0, 0.5, "fr row bottom");
}

#[test]
fn auto_includes_absolute_insets() {
    // Auto col, child placed with 1cm left + 1cm right insets, child grid
    // has a 3cm fixed col → auto col = 5cm.
    let inner = Grid::new([Track::Fixed(Extent::cm(3.0))], [Track::Fr(1.0)]);
    let mut root = Grid::new([Track::Auto], [Track::Fr(1.0)]).id(CellId(0));
    root.place_mut(
        Placement::at(1, 1).inset(
            Inset::default()
                .left(Extent::cm(1.0))
                .right(Extent::cm(1.0)),
        ),
        inner,
    );
    let layout = root.solve(Size::new(800.0, 200.0), 96.0);
    let root_rect = layout.rect(CellId(0)).unwrap();
    let five_cm = 5.0 * 96.0 / 2.54;
    approx_eq(
        root_rect.x1 - root_rect.x0,
        five_cm,
        0.5,
        "root width = 5cm",
    );
}

#[test]
fn auto_with_explicit_width_inset() {
    // Auto col, child placed with Inset.width(2cm) but containing a
    // 10cm-wide cell → auto col = 2cm (explicit inset wins).
    let huge = Grid::new([Track::Fixed(Extent::cm(10.0))], [Track::Fr(1.0)]);
    let mut root = Grid::new([Track::Auto], [Track::Fr(1.0)]).id(CellId(0));
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::cm(2.0))),
        huge,
    );
    let layout = root.solve(Size::new(800.0, 200.0), 96.0);
    let root_rect = layout.rect(CellId(0)).unwrap();
    let two_cm = 2.0 * 96.0 / 2.54;
    approx_eq(root_rect.x1 - root_rect.x0, two_cm, 0.5, "root width = 2cm");
}

#[test]
fn auto_max_over_children() {
    // Auto col with three children at the same column; the largest
    // (120 px) wins.
    let mut root = Grid::new([Track::Auto], vec![Track::Fr(1.0); 3]).id(CellId(0));
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::px(50.0))),
        Grid::cell(),
    );
    root.place_mut(
        Placement::at(2, 1).inset(Inset::default().width(Extent::px(120.0))),
        Grid::cell(),
    );
    root.place_mut(
        Placement::at(3, 1).inset(Inset::default().width(Extent::px(80.0))),
        Grid::cell(),
    );
    let layout = root.solve(Size::new(800.0, 300.0), 96.0);
    let r = layout.rect(CellId(0)).unwrap();
    approx_eq(r.x1 - r.x0, 120.0, 0.5, "auto col = max");
}

#[test]
fn auto_multi_span_skipped() {
    // Two Auto cols, one child spanning both with width 100 → contributes
    // 0 to both cols (multi-span children are skipped in the width pass).
    let mut root = Grid::new([Track::Auto, Track::Auto], [Track::Fr(1.0)]).id(CellId(0));
    root.place_mut(
        Placement::at(1, 1)
            .span(1, 2)
            .inset(Inset::default().width(Extent::px(100.0))),
        Grid::cell(),
    );
    let layout = root.solve(Size::new(400.0, 100.0), 96.0);
    let r = layout.rect(CellId(0)).unwrap();
    approx_eq(r.x1 - r.x0, 0.0, 0.5, "both auto cols zero");
}

#[test]
fn auto_with_fr_split() {
    // [Auto, Fr(1)] in 200×100. Auto child needs 30px → col 1 = 30,
    // col 2 = 170.
    let mut root = Grid::new([Track::Auto, Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::px(30.0))),
        Grid::cell().id(CellId(1)),
    );
    root.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));

    let layout = root.solve(Size::new(200.0, 100.0), 96.0);
    let r1 = layout.rect(CellId(1)).unwrap();
    approx_eq(r1.x0, 0.0, 0.5, "col1 left");
    approx_eq(r1.x1, 30.0, 0.5, "col1 right");
    let r2 = layout.rect(CellId(2)).unwrap();
    approx_eq(r2.x0, 30.0, 0.5, "col2 left");
    approx_eq(r2.x1, 200.0, 0.5, "col2 right");
}

#[test]
fn length_scalar_multiplication() {
    // `cm(5) * 2` in a 1cm-wide track produces a 10cm width.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::cm(5.0) * 2.0)),
        Grid::cell().id(CellId(1)),
    );
    let layout = root.solve(Size::new(800.0, 200.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    let ten_cm = 10.0 * 96.0 / 2.54;
    approx_eq(r.x1 - r.x0, ten_cm, 0.5, "5cm × 2 = 10cm");
}

#[test]
fn length_relative_minus_absolute() {
    // "5mm to the left of center" → percent(0.5) - mm(5).
    // In a 400px-wide viewport the center is at 200; 5mm @ 96 DPI ≈ 18.898 px;
    // result ≈ 181.102 px from the left.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(Extent::percent(0.5) - Extent::mm(5.0))),
        Grid::cell().id(CellId(1)),
    );
    let layout = root.solve(Size::new(400.0, 100.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    let five_mm = 5.0 * 96.0 / 25.4;
    approx_eq(r.x1 - r.x0, 200.0 - five_mm, 0.5, "50% − 5mm");
}

#[test]
fn length_min_chooses_smaller_at_resolution() {
    // min(cm(2), percent(0.25)) — pick whichever is smaller for the axis.
    // viewport 200px: 25% = 50, 2cm ≈ 75.59 → min = 50.
    // viewport 600px: 25% = 150, 2cm ≈ 75.59 → min ≈ 75.59.
    let make_root = || {
        let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
        root.place_mut(
            Placement::at(1, 1)
                .inset(Inset::default().width(Extent::min(Extent::cm(2.0), Extent::percent(0.25)))),
            Grid::cell().id(CellId(1)),
        );
        root
    };

    let narrow = make_root().solve(Size::new(200.0, 100.0), 96.0);
    let r = narrow.rect(CellId(1)).unwrap();
    approx_eq(r.x1 - r.x0, 50.0, 0.5, "200px: percent wins");

    let wide = make_root().solve(Size::new(600.0, 100.0), 96.0);
    let r = wide.rect(CellId(1)).unwrap();
    let two_cm = 2.0 * 96.0 / 2.54;
    approx_eq(r.x1 - r.x0, two_cm, 0.5, "600px: cm wins");
}

#[test]
fn length_addition_distributes_through_min() {
    // (min(cm(1), percent(0.1))) + cm(1) → min(cm(2), percent(0.1) + cm(1)).
    // viewport 200px: cm(2) ≈ 75.59, percent(0.1) + cm(1) ≈ 20 + 37.80 ≈ 57.80
    //   → min ≈ 57.80.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0)]);
    let expr = Extent::min(Extent::cm(1.0), Extent::percent(0.1)) + Extent::cm(1.0);
    root.place_mut(
        Placement::at(1, 1).inset(Inset::default().width(expr)),
        Grid::cell().id(CellId(1)),
    );
    let layout = root.solve(Size::new(200.0, 100.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    let one_cm: f64 = 96.0 / 2.54;
    let candidate_a = 2.0 * one_cm; // ≈ 75.59
    let candidate_b = 0.1 * 200.0 + one_cm; // ≈ 57.80
    let expected = candidate_a.min(candidate_b);
    approx_eq(r.x1 - r.x0, expected, 0.5, "add distributes through min");
}

#[test]
fn length_is_absolute() {
    assert!(Extent::cm(5.0).is_absolute());
    assert!(Extent::px(10.0).is_absolute());
    assert!(!Extent::percent(0.5).is_absolute());
    assert!((Extent::cm(1.0) + Extent::px(3.0)).is_absolute());
    assert!(!(Extent::cm(1.0) + Extent::percent(0.5)).is_absolute());
    assert!(Extent::min(Extent::cm(1.0), Extent::px(2.0)).is_absolute());
    assert!(!Extent::max(Extent::cm(1.0), Extent::percent(0.5)).is_absolute());
}

// ─── Measure / Content fixtures ──────────────────────────────────────

/// A Cell with a constant intrinsic size, independent of the width it
/// is offered. Models fixed chrome (a rule, an icon, a rail).
struct FixedSize {
    w: f64,
    h: f64,
}
impl Measure for FixedSize {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.w)
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        self.h
    }
}

/// A Cell that returns `height_at(width) = width * factor`. Models a
/// chart with a fixed aspect ratio.
struct AspectContent {
    factor: f64,
}
impl Measure for AspectContent {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(0.0)
    }
    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        width * self.factor
    }
}

/// A text-wrap stub: `height_at(w) = line_h * ceil(total_text_w / w)`.
struct WrappedTextStub {
    total_text_w: f64,
    line_h: f64,
}
impl Measure for WrappedTextStub {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.line_h) // safe lower bound — one char's worth
    }
    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        let w = width.max(self.line_h);
        let lines = (self.total_text_w / w).ceil().max(1.0);
        self.line_h * lines
    }
}

/// A cell whose width depends on height: width = height * factor.
/// Models a rotated wrapped textbox.
struct HeightDrivenContent {
    seed: f64,
    factor: f64,
}
impl Measure for HeightDrivenContent {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::NeedsHeight { seed: self.seed }
    }
    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        // For testing: height = constant / width (inverse relationship).
        // Combined with width_at = height * factor below, this is a
        // contracting map and should converge.
        200.0 / width.max(1.0)
    }
    fn width_at(&self, height: f64, _dpi: f64) -> f64 {
        height * self.factor
    }
}

/// Width oscillates between two values based on height.
struct OscillatingContent {
    small_h: f64,
    small_w: f64,
    large_w: f64,
}
impl Measure for OscillatingContent {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::NeedsHeight { seed: self.small_w }
    }
    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        if width > (self.small_w + self.large_w) * 0.5 {
            self.small_h
        } else {
            self.small_h * 2.0
        }
    }
    fn width_at(&self, height: f64, _dpi: f64) -> f64 {
        if height > self.small_h * 1.5 {
            self.large_w
        } else {
            self.small_w
        }
    }
}

#[test]
fn cell_fixed_aspect_drives_auto_row() {
    // 200-wide fixed col, Auto row containing an AspectContent (factor 0.5).
    // Expected: row height = 200 × 0.5 = 100.
    let mut root = Grid::new(
        [Track::Fixed(Extent::px(200.0))],
        [Track::Auto, Track::Fr(1.0)],
    );
    root.place_mut(
        Placement::at(1, 1),
        Cell::measured(AspectContent { factor: 0.5 }).id(CellId(1)),
    );
    let layout = root.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.x1 - r.x0, 200.0, 0.5, "fixed col width");
    approx_eq(r.y1 - r.y0, 100.0, 0.5, "content height = width × 0.5");
}

#[test]
fn cell_text_wrap_stub() {
    // 200 wide column, Auto row, text of 600 width and line height 20.
    //   600 / 200 = 3 lines → 60 px.
    let mut root_wide = Grid::new(
        [Track::Fixed(Extent::px(200.0))],
        [Track::Auto, Track::Fr(1.0)],
    );
    root_wide.place_mut(
        Placement::at(1, 1),
        Cell::measured(WrappedTextStub {
            total_text_w: 600.0,
            line_h: 20.0,
        })
        .id(CellId(1)),
    );
    let layout = root_wide.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.y1 - r.y0, 60.0, 0.5, "3-line height");

    // 100 wide column → 6 lines → 120 px.
    let mut root_narrow = Grid::new(
        [Track::Fixed(Extent::px(100.0))],
        [Track::Auto, Track::Fr(1.0)],
    );
    root_narrow.place_mut(
        Placement::at(1, 1),
        Cell::measured(WrappedTextStub {
            total_text_w: 600.0,
            line_h: 20.0,
        })
        .id(CellId(1)),
    );
    let layout = root_narrow.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.y1 - r.y0, 120.0, 0.5, "6-line height");
}

#[test]
fn cell_height_at_zero_for_empty() {
    // Empty cells in an Auto row should size the row to 0.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Auto, Track::Fr(1.0)]);
    root.place_mut(Placement::at(1, 1), Cell::empty().id(CellId(10)));
    root.place_mut(Placement::at(2, 1), Cell::empty().id(CellId(20)));
    let layout = root.solve(Size::new(200.0, 200.0), 96.0);
    let r10 = layout.rect(CellId(10)).unwrap();
    approx_eq(r10.y1 - r10.y0, 0.0, 0.5, "empty cell auto row height = 0");
    // The fr row takes everything.
    let r20 = layout.rect(CellId(20)).unwrap();
    approx_eq(r20.y1 - r20.y0, 200.0, 0.5, "fr row gets all the space");
}

#[test]
fn iteration_converges_within_cap() {
    // HeightDrivenContent: height = 200/width; width = height * factor.
    // Fixed point: w = (200/w) * factor → w² = 200 * factor → w = sqrt(200f).
    // With factor = 0.5: w = sqrt(100) = 10. height = 20.
    let mut root = Grid::new([Track::Auto], [Track::Auto, Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1),
        Cell::measured(HeightDrivenContent {
            seed: 1.0,
            factor: 0.5,
        })
        .id(CellId(1)),
    );
    let layout = root.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    // Within iteration cap (5), damped: tolerance is loose — just check
    // we landed in a sensible neighborhood of the fixed point.
    let w = r.x1 - r.x0;
    let h = r.y1 - r.y0;
    assert!(w > 5.0 && w < 20.0, "width converged near 10: got {w}");
    assert!(h > 10.0 && h < 40.0, "height converged near 20: got {h}");
}

#[test]
fn iteration_oscillates_terminates_at_cap() {
    // OscillatingContent: width flips between small and large based on
    // height; height flips based on width. Damping at 0.5 should pull
    // the system toward the midpoint or one of the values; the cap
    // guarantees termination.
    let mut root = Grid::new([Track::Auto], [Track::Auto, Track::Fr(1.0)]);
    root.place_mut(
        Placement::at(1, 1),
        Cell::measured(OscillatingContent {
            small_h: 20.0,
            small_w: 40.0,
            large_w: 200.0,
        })
        .id(CellId(1)),
    );
    // The test asserts only that solve terminates with a finite rect —
    // exact value depends on damping trajectory, which we don't pin.
    let layout = root.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    assert!(r.x1.is_finite() && r.y1.is_finite(), "rect is finite");
    assert!(r.x1 >= r.x0 && r.y1 >= r.y0, "rect is non-degenerate");
}

#[test]
fn respect_with_content_growth() {
    // A respect grid with [Fr(1)] cols × [Auto, Fr(1)] rows. Content
    // pushes the auto row to 100. The grid's height = 100 (auto) +
    // remaining (fr). With viewport 200×400 and respect: per_fr_w = 200,
    // per_fr_h_provisional = 400/1 = 400 (for the fr row only — auto
    // counts as 0 in pass 1). Width clamp: min(200, 400) = 200.
    // Pass 2: auto row = 100, free_h = 300, per_fr_h_default = 300.
    // respect re-clamp: min(per_fr_w=200, per_fr_h_default=300) = 200.
    // Total: cols all 200; rows = [100 (auto), 200 (fr)] = 300 total
    // height. Slack of 100 top/bottom.
    let mut root = Grid::new([Track::Fr(1.0)], [Track::Auto, Track::Fr(1.0)])
        .respect()
        .id(CellId(0));
    root.place_mut(
        Placement::at(1, 1),
        Cell::measured(WrappedTextStub {
            total_text_w: 100.0,
            line_h: 100.0,
        })
        .id(CellId(1)),
    );
    let layout = root.solve(Size::new(200.0, 400.0), 96.0);
    let r0 = layout.rect(CellId(0)).unwrap();
    let r1 = layout.rect(CellId(1)).unwrap();
    // The grid's resolved height = 100 (auto) + 200 (fr clamped) = 300.
    approx_eq(r0.y1 - r0.y0, 300.0, 0.5, "grid total height after respect");
    approx_eq(r1.y1 - r1.y0, 100.0, 0.5, "auto row from content");
}

#[test]
fn respect_matrix_all_true_matches_respect_all() {
    // A 2x2 Fr grid with a fully-true respect matrix should produce the
    // same layout as the same grid with `.respect()` (all).
    let m = vec![vec![true, true], vec![true, true]];
    let mut a = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    )
    .respect_matrix(m);
    a.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    a.place_mut(Placement::at(2, 2), Grid::cell().id(CellId(2)));
    let la = a.solve(Size::new(400.0, 200.0), 96.0);

    let mut b = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    )
    .respect();
    b.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    b.place_mut(Placement::at(2, 2), Grid::cell().id(CellId(2)));
    let lb = b.solve(Size::new(400.0, 200.0), 96.0);

    let ra1 = la.rect(CellId(1)).unwrap();
    let rb1 = lb.rect(CellId(1)).unwrap();
    approx_eq(ra1.x0, rb1.x0, 0.5, "cell1 x0");
    approx_eq(ra1.x1, rb1.x1, 0.5, "cell1 x1");
    approx_eq(ra1.y0, rb1.y0, 0.5, "cell1 y0");
    approx_eq(ra1.y1, rb1.y1, 0.5, "cell1 y1");
}

#[test]
fn respect_matrix_none_matches_no_respect() {
    // Empty matrix (no cells marked) behaves like no respect.
    let mut a = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    )
    .respect_matrix(vec![vec![false, false], vec![false, false]]);
    a.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    let la = a.solve(Size::new(400.0, 200.0), 96.0);

    let mut b = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    );
    b.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    let lb = b.solve(Size::new(400.0, 200.0), 96.0);

    let ra = la.rect(CellId(1)).unwrap();
    let rb = lb.rect(CellId(1)).unwrap();
    approx_eq(ra.x1 - ra.x0, rb.x1 - rb.x0, 0.5, "col 1 width");
    approx_eq(ra.y1 - ra.y0, rb.y1 - rb.y0, 0.5, "row 1 height");
}

#[test]
fn respect_matrix_single_cell_locks_one_pair() {
    // A 1x2 grid `[Fr(1), Fr(1)] × [Fr(1)]` with respect_at(0, 0) in
    // 800×400. Respected col 0 + row 0 lock to a uniform per-fr scale;
    // the binding axis is height (400/1 = 400 vs 800/1 = 800 → 400 wins).
    // So col 0 width = 1*400 = 400 (locked square at 400×400).
    // Unrespected col 1 absorbs the remaining 800 - 400 = 400 →
    // col 1 width = 400.
    let mut g = Grid::new([Track::Fr(1.0), Track::Fr(1.0)], [Track::Fr(1.0)]).respect_at(0, 0);
    g.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    g.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));
    let layout = g.solve(Size::new(800.0, 400.0), 96.0);

    let r1 = layout.rect(CellId(1)).unwrap();
    approx_eq(r1.x1 - r1.x0, 400.0, 0.5, "fixed col 0 width");
    approx_eq(r1.y1 - r1.y0, 400.0, 0.5, "row 0 height");

    let r2 = layout.rect(CellId(2)).unwrap();
    approx_eq(r2.x1 - r2.x0, 400.0, 0.5, "flex col 1 absorbs slack");
    approx_eq(r2.y1 - r2.y0, 400.0, 0.5, "row 0 height shared");
}

#[test]
fn respect_matrix_locks_under_fixed_chrome() {
    // [Fixed(100px), Fr(1), Fr(1)] cols × [Fixed(50px), Fr(1)] rows in
    // 600×450. Fixed pre-allocation: 100 col + 50 row = 100 col left,
    // 400 row left after fixed. Free width for Fr = 600 - 100 = 500.
    // respect_at(1, 1) marks cell (row 1, col 1). Respected col 1 fr=1,
    // unrespected col 2 fr=1 (total col Fr = 2), respected row 1 fr=1
    // (total row Fr = 1). resp_scale divides by *total* Fr in each
    // axis so unrespected siblings retain their share before respect
    // consumes the axis: width 500/2 = 250 vs height 400/1 = 400 →
    // 250 binds. col 1 = 1*250 = 250 (locked square at 250×250); col 2
    // absorbs the remaining 500 - 250 = 250 width.
    let mut g = Grid::new(
        [
            Track::Fixed(Extent::px(100.0)),
            Track::Fr(1.0),
            Track::Fr(1.0),
        ],
        [Track::Fixed(Extent::px(50.0)), Track::Fr(1.0)],
    )
    .respect_at(1, 1);
    g.place_mut(Placement::at(2, 2), Grid::cell().id(CellId(1)));
    g.place_mut(Placement::at(2, 3), Grid::cell().id(CellId(2)));
    let layout = g.solve(Size::new(600.0, 450.0), 96.0);

    let r1 = layout.rect(CellId(1)).unwrap();
    approx_eq(r1.x1 - r1.x0, 250.0, 0.5, "respected col width");
    approx_eq(r1.y1 - r1.y0, 250.0, 0.5, "respected row height");

    let r2 = layout.rect(CellId(2)).unwrap();
    approx_eq(r2.x1 - r2.x0, 250.0, 0.5, "unrespected col gets its share");
}

#[test]
fn nested_respected_grid_sees_full_window_height() {
    // A nested grid spanning an Auto row must be handed the Auto row's
    // resolved height in its width-pass window. Understating the window
    // makes the height axis look tighter than it is, so a respected
    // child locks its cell too small and leaves the surplus as
    // centering slack.
    //
    // Outer: 500x300, rows [Fr(1), Auto]. A standalone 40px cell sizes
    // the Auto row. The inner grid spans both rows, so its window is
    // the full 300 tall. Inner: rows [Fr(1), Auto] with a 30px cell in
    // the Auto row, respect_at(0, 0). Height binds:
    // (300 - 30) / 1 = 270 vs width 500 / 1 = 500.
    let mut inner = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0), Track::Auto]).respect_at(0, 0);
    inner = inner.id(CellId(10));
    inner.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    inner.place_mut(
        Placement::at(2, 1),
        Cell::measured(FixedSize { w: 0.0, h: 30.0 }),
    );

    let mut outer = Grid::new([Track::Fr(1.0)], [Track::Fr(1.0), Track::Auto]);
    outer.place_mut(Placement::at(1, 1).span(2, 1), inner);
    outer.place_mut(
        Placement::at(2, 1),
        Cell::measured(FixedSize { w: 0.0, h: 40.0 }),
    );
    let layout = outer.solve(Size::new(500.0, 300.0), 96.0);

    let panel = layout.rect(CellId(1)).unwrap();
    approx_eq(panel.x1 - panel.x0, 270.0, 0.5, "respected panel width");
    approx_eq(panel.y1 - panel.y0, 270.0, 0.5, "respected panel height");
}

#[test]
fn respect_matrix_two_respected_cols_share_scale() {
    // [Fr(1), Fr(2)] cols × [Fr(1)] rows in 600×100. Both cols
    // respected via the matrix; one row respected.
    // resp_scale: width 600 / (1+2) = 200 vs height 100/1 = 100 → 100
    // binds. col 0 = 1*100 = 100, col 1 = 2*100 = 200; row 0 = 100.
    // Remaining width 600 - 300 = 300 has no unrespected fr to absorb,
    // so the grid centres at the 300px total width.
    let m = vec![vec![true, true]];
    let mut g = Grid::new([Track::Fr(1.0), Track::Fr(2.0)], [Track::Fr(1.0)]).respect_matrix(m);
    g.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    g.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));
    let layout = g.solve(Size::new(600.0, 100.0), 96.0);

    let r1 = layout.rect(CellId(1)).unwrap();
    let r2 = layout.rect(CellId(2)).unwrap();
    approx_eq(r1.x1 - r1.x0, 100.0, 0.5, "respected col fr=1");
    approx_eq(r2.x1 - r2.x0, 200.0, 0.5, "respected col fr=2");
    // Ratio preserved
    approx_eq(
        (r2.x1 - r2.x0) / (r1.x1 - r1.x0),
        2.0,
        0.05,
        "respected cols share resp_scale (ratio = fr weights)",
    );
}

#[test]
fn respect_matrix_width_binding_uses_smaller_scale() {
    // Same shape as `respect_matrix_single_cell_locks_one_pair` but
    // with the viewport flipped so width is the binding axis.
    // 1×2 grid `[Fr(1), Fr(1)] × [Fr(1)]` in 200×800 viewport
    // (tall narrow) with respect_at(0, 0). Total col Fr = 2,
    // total row Fr = 1.
    // resp_scale: width 200/2 = 100 vs height 800/1 = 800 → 100 binds.
    // col 0 = 100 (locked square at 100×100); unrespected col 1
    // absorbs the remaining 200 - 100 = 100 width; row 0 = 100.
    let mut g = Grid::new([Track::Fr(1.0), Track::Fr(1.0)], [Track::Fr(1.0)]).respect_at(0, 0);
    g.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    g.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));
    let layout = g.solve(Size::new(200.0, 800.0), 96.0);
    let r1 = layout.rect(CellId(1)).unwrap();
    approx_eq(r1.x1 - r1.x0, 100.0, 0.5, "width-bound col 0");
    approx_eq(r1.y1 - r1.y0, 100.0, 0.5, "row 0 height matches");
    let r2 = layout.rect(CellId(2)).unwrap();
    approx_eq(r2.x1 - r2.x0, 100.0, 0.5, "unrespected col gets its share");
}

#[test]
fn multi_span_contributes_to_auto_row_in_pass_2() {
    // [Fr(1), Fr(1)] cols × [Auto, Fr(1)] rows in 400×400.
    // A single child spans both cols with a width-based factor.
    // Auto row should size to width × 0.25 = (200 + 200) × 0.25 = 100.
    let mut root = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Auto, Track::Fr(1.0)],
    );
    root.place_mut(
        Placement::at(1, 1).span(1, 2),
        Cell::measured(AspectContent { factor: 0.25 }).id(CellId(1)),
    );
    let layout = root.solve(Size::new(400.0, 400.0), 96.0);
    let r = layout.rect(CellId(1)).unwrap();
    approx_eq(r.x1 - r.x0, 400.0, 0.5, "multi-span spans both cols");
    approx_eq(r.y1 - r.y0, 100.0, 0.5, "auto row from multi-span content");
}

#[test]
fn recursive_tiling_no_gaps() {
    // 2×2 outer; each outer cell holds a 2×2 inner. 16 leaves tile the viewport.
    let mut root = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    );
    let mut leaf_id = 0u64;
    for outer_r in 1..=2 {
        for outer_c in 1..=2 {
            let mut inner = Grid::new(
                [Track::Fr(1.0), Track::Fr(1.0)],
                [Track::Fr(1.0), Track::Fr(1.0)],
            );
            for r in 1..=2 {
                for c in 1..=2 {
                    leaf_id += 1;
                    inner.place_mut(Placement::at(r, c), Grid::cell().id(CellId(leaf_id)));
                }
            }
            root.place_mut(Placement::at(outer_r, outer_c), inner);
        }
    }
    let layout = root.solve(Size::new(400.0, 400.0), 96.0);
    let leaves: Vec<_> = (1..=16).map(|i| layout.rect(CellId(i)).unwrap()).collect();
    for (i, r) in leaves.iter().enumerate() {
        approx_eq(r.x1 - r.x0, 100.0, 0.5, &format!("leaf {i} width"));
        approx_eq(r.y1 - r.y0, 100.0, 0.5, &format!("leaf {i} height"));
    }
    let total_area: f64 = leaves.iter().map(|r| (r.x1 - r.x0) * (r.y1 - r.y0)).sum();
    approx_eq(total_area, 160_000.0, 1.0, "tiling total area");
}

#[test]
fn track_of_width_one_iteration() {
    // A 2-col root with a tagged inner grid in col 1 whose first column
    // resolves to 50 px (Fixed). The root's col 2 is sized to `TrackOf`
    // of inner's first column → after the iteration loop, col 2 is 50 px.
    let inner_grid_id = CellId(101);
    let mut inner = Grid::new([Track::Fixed(Extent::px(50.0))], [Track::Fr(1.0)]).id(inner_grid_id);
    inner.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));

    let mut root = Grid::new(
        [
            Track::Fixed(Extent::px(100.0)),
            Track::Fixed(Extent::track_of(inner_grid_id, Axis::Width, 1)),
        ],
        [Track::Fr(1.0)],
    );
    root.place_mut(Placement::at(1, 1), inner);
    root.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));

    let layout = root.solve(Size::new(200.0, 100.0), 96.0);
    let r = layout.rect(CellId(2)).unwrap();
    approx_eq(r.x1 - r.x0, 50.0, 0.5, "col 2 sized to inner col 1 (50 px)");
}

#[test]
fn track_of_height_picks_up_inner_size() {
    // Tagged inner grid has two Fixed rows (40 px each, gap 0); an outer
    // row references both via `tracks_of(inner_id, Axis::Height, 1, 2)` →
    // outer row resolves to 80 px.
    let inner_id = CellId(7);
    let mut inner = Grid::new(
        [Track::Fr(1.0)],
        [
            Track::Fixed(Extent::px(40.0)),
            Track::Fixed(Extent::px(40.0)),
        ],
    )
    .id(inner_id);
    inner.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(10)));

    let mut root = Grid::new(
        [Track::Fr(1.0)],
        [
            Track::Fr(1.0),
            Track::Fixed(Extent::tracks_of(inner_id, Axis::Height, 1, 2)),
        ],
    );
    root.place_mut(Placement::at(1, 1), inner);
    root.place_mut(Placement::at(2, 1), Grid::cell().id(CellId(99)));

    let layout = root.solve(Size::new(200.0, 200.0), 96.0);
    let r = layout.rect(CellId(99)).unwrap();
    approx_eq(
        r.y1 - r.y0,
        80.0,
        0.5,
        "outer row picks up sum of inner rows",
    );
}

#[test]
fn track_of_unknown_id_is_zero() {
    // Reference to a CellId not present in the tree resolves to 0 every
    // iteration — solver doesn't panic, just treats the reference as 0.
    let bogus = CellId(999);
    let mut root = Grid::new(
        [
            Track::Fixed(Extent::px(100.0)),
            Track::Fixed(Extent::track_of(bogus, Axis::Width, 1)),
        ],
        [Track::Fr(1.0)],
    );
    root.place_mut(Placement::at(1, 1), Grid::cell().id(CellId(1)));
    root.place_mut(Placement::at(1, 2), Grid::cell().id(CellId(2)));

    let layout = root.solve(Size::new(200.0, 100.0), 96.0);
    let r = layout.rect(CellId(2)).unwrap();
    approx_eq(r.x1 - r.x0, 0.0, 0.5, "unknown reference → 0 px");
}
