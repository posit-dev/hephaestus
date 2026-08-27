//! End-to-end coverage for the composition surface: patch anatomy, nesting,
//! chrome alignment, aspect locks, and error reporting.

use super::*;
use crate::geometry::Size;
use crate::layout::{Cell, Extent, Inset, Measure, Track, WidthHint};

fn approx_eq(a: f64, b: f64, tol: f64, msg: &str) {
    assert!((a - b).abs() <= tol, "{msg}: {a} ≠ {b} (tol {tol})");
}

/// A fake leaf with a fixed intrinsic width and height. `width_hint`
/// drives any containing Auto column; `height_at` drives any containing
/// Auto row.
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

fn sized(w: f64, h: f64) -> Cell {
    Cell::measured(FixedSize { w, h })
}

// ─── Single-patch tests (step 2) ────────────────────────────────────

#[test]
fn patch_single_panel_fills_viewport() {
    let p = Patch::new("p").slot(Slot::Panel, Cell::empty());
    let layout = p.solve(Size::new(400.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    approx_eq(panel.x0, 0.0, 0.5, "panel x0");
    approx_eq(panel.y0, 0.0, 0.5, "panel y0");
    approx_eq(panel.x1, 400.0, 0.5, "panel x1");
    approx_eq(panel.y1, 200.0, 0.5, "panel y1");
}

#[test]
fn patch_axes_consume_intrinsic_width() {
    let p = Patch::new("p")
        .slot(Slot::AxisLeft, sized(50.0, 0.0))
        .slot(Slot::Panel, Cell::empty());
    let layout = p.solve(Size::new(400.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let axis = layout.get("p", Slot::AxisLeft).unwrap();
    approx_eq(panel.x0, 50.0, 0.5, "panel x0 == axis width");
    approx_eq(axis.x0, 0.0, 0.5, "axis x0 at left edge");
    approx_eq(axis.x1, 50.0, 0.5, "axis x1 = 50");
}

#[test]
fn patch_axes_consume_intrinsic_height() {
    let p = Patch::new("p")
        .slot(Slot::AxisBottom, sized(0.0, 30.0))
        .slot(Slot::Panel, Cell::empty());
    let layout = p.solve(Size::new(400.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let axis = layout.get("p", Slot::AxisBottom).unwrap();
    approx_eq(panel.y1, 170.0, 0.5, "panel ends 30 above bottom");
    approx_eq(axis.y0, 170.0, 0.5, "axis row starts at 170");
    approx_eq(axis.y1, 200.0, 0.5, "axis row ends at 200");
}

#[test]
fn aspect_locks_panel_per_patch() {
    let p = Patch::new("p")
        .aspect(16.0, 9.0)
        .slot(Slot::Panel, Cell::empty());
    let layout = p.solve(Size::new(400.0, 400.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let w = panel.x1 - panel.x0;
    let h = panel.y1 - panel.y0;
    approx_eq(w / h, 16.0 / 9.0, 0.01, "aspect ratio 16:9");
}

#[test]
fn patch_aspect_lets_flex_sibling_absorb_slack() {
    // The central regression that drove the layout-level rewrite:
    // `beside(fixed.aspect(1, 1), flex)` should let flex absorb the
    // horizontal slack instead of leaving an empty square next to a
    // centred fixed plot. In 800×400 viewport, fixed's row is 400
    // (binding) → fixed panel = 400×400; flex panel absorbs the
    // remaining 400 width → 400×400.
    let fixed = Patch::new("fixed")
        .aspect(1.0, 1.0)
        .slot(Slot::Panel, Cell::empty());
    let flex = Patch::new("flex").slot(Slot::Panel, Cell::empty());
    let layout = beside(fixed, flex).solve(Size::new(800.0, 400.0), 96.0);
    let fp = layout.get("fixed", Slot::Panel).unwrap();
    let xp = layout.get("flex", Slot::Panel).unwrap();
    approx_eq(fp.x1 - fp.x0, 400.0, 0.5, "fixed panel width");
    approx_eq(fp.y1 - fp.y0, 400.0, 0.5, "fixed panel height");
    approx_eq(xp.x1 - xp.x0, 400.0, 0.5, "flex panel absorbs slack");
    approx_eq(xp.y1 - xp.y0, 400.0, 0.5, "flex panel shares row height");
}

#[test]
fn composition_aspect_propagates_to_each_facet() {
    // 2×2 facet composition with title chrome and .aspect(1, 1).
    // Each facet panel ends up square. With viewport 800×600 and a
    // 40px title row, the per-facet panel area is min((800/2),
    // (600-40)/2) = min(400, 280) = 280 → each panel 280×280.
    let facet = |id: &str| Patch::new(id).slot(Slot::Panel, Cell::empty());
    let comp = beside(
        stack(facet("q1"), facet("q3")),
        stack(facet("q2"), facet("q4")),
    )
    .id("outer")
    .aspect(1.0, 1.0);
    let layout = comp.solve(Size::new(800.0, 600.0), 96.0);
    for id in &["q1", "q2", "q3", "q4"] {
        let r = layout.get(id, Slot::Panel).unwrap();
        let w = r.x1 - r.x0;
        let h = r.y1 - r.y0;
        assert!(w > 0.0, "{id} non-zero width");
        assert!(h > 0.0, "{id} non-zero height");
        approx_eq(w / h, 1.0, 0.02, &format!("{id} panel is square"));
    }
}

#[test]
fn composition_aspect_does_not_override_explicit_patch_aspect() {
    // Outer .aspect(16, 9); child has its own .aspect(4, 3). The
    // explicit child aspect blocks propagation past it. Single-facet
    // composition so siblings don't compete for the shared row fr
    // (the multi-aspect-conflict case is a documented limitation
    // matching patchwork's "if one fixed aspect plot conflicts with
    // another one, one of them will end up not using the full space"
    // behaviour).
    let a = Patch::new("a")
        .aspect(4.0, 3.0)
        .slot(Slot::Panel, Cell::empty());
    let comp = Composition::empty(1, 1)
        .place(1, 1, Span::cell(), a)
        .aspect(16.0, 9.0);
    let layout = comp.solve(Size::new(800.0, 800.0), 96.0);
    let ap = layout.get("a", Slot::Panel).unwrap();
    approx_eq(
        (ap.x1 - ap.x0) / (ap.y1 - ap.y0),
        4.0 / 3.0,
        0.02,
        "a keeps its own 4:3 despite outer 16:9",
    );
}

#[test]
fn composition_aspect_blocked_by_inner_aspect() {
    // Outer .aspect(16, 9) propagates to an immediate-child
    // composition WITHOUT its own aspect; that child propagates
    // further. But an inner composition with its own .aspect(1, 1)
    // wins and blocks propagation past it.
    let leaf_outer = Patch::new("outer_leaf").slot(Slot::Panel, Cell::empty());
    let leaf_inner_a = Patch::new("inner_a").slot(Slot::Panel, Cell::empty());
    let leaf_inner_b = Patch::new("inner_b").slot(Slot::Panel, Cell::empty());
    let inner = beside(leaf_inner_a, leaf_inner_b).aspect(1.0, 1.0);
    let outer = beside(leaf_outer, inner).id("outer").aspect(16.0, 9.0);
    let layout = outer.solve(Size::new(1200.0, 400.0), 96.0);
    let outer_leaf = layout.get("outer_leaf", Slot::Panel).unwrap();
    approx_eq(
        (outer_leaf.x1 - outer_leaf.x0) / (outer_leaf.y1 - outer_leaf.y0),
        16.0 / 9.0,
        0.02,
        "outer leaf receives propagated 16:9",
    );
    let ia = layout.get("inner_a", Slot::Panel).unwrap();
    let ib = layout.get("inner_b", Slot::Panel).unwrap();
    approx_eq(
        (ia.x1 - ia.x0) / (ia.y1 - ia.y0),
        1.0,
        0.02,
        "inner_a from inner .aspect(1,1)",
    );
    approx_eq(
        (ib.x1 - ib.x0) / (ib.y1 - ib.y0),
        1.0,
        0.02,
        "inner_b from inner .aspect(1,1)",
    );
}

#[test]
fn composition_aspect_plus_tall_axis_grows_chrome() {
    // A composition with .aspect(1, 1) on facets that carry a tall
    // axis_bottom. The chrome row grows (forward sizer fires) AND
    // each facet panel remains square in any viewport — the
    // solver's second iteration picks up the resolved Auto-row
    // heights from iter 0's pass 2 and reshapes the respected fr
    // distribution to the actual ratio. Any slack appears as empty
    // space around the grid; chrome doesn't fight the lock.
    let facet = |id: &str| {
        Patch::new(id)
            .slot(Slot::Panel, Cell::empty())
            .slot(Slot::AxisBottom, sized(0.0, 40.0))
    };
    let comp = beside(facet("a"), facet("b")).aspect(1.0, 1.0);
    // 800w × 400h: height binds. Panel row = 400 - 40 axis = 360 per side.
    let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
    for id in &["a", "b"] {
        let panel = layout.get(id, Slot::Panel).unwrap();
        let axis = layout.get(id, Slot::AxisBottom).unwrap();
        approx_eq(
            (panel.x1 - panel.x0) / (panel.y1 - panel.y0),
            1.0,
            0.02,
            &format!("{id} panel is square under chrome"),
        );
        approx_eq(axis.y1 - axis.y0, 40.0, 0.5, &format!("{id} axis 40px"));
    }
}

#[test]
fn place_at_escape_hatch() {
    let p = Patch::new("p").slot(Slot::Panel, Cell::empty()).place_at(
        "overlay",
        2,
        PANEL_COL,
        Span::cols(3),
        sized(0.0, 30.0),
    );
    let layout = p.solve(Size::new(400.0, 400.0), 96.0);
    let overlay = layout.get("p", "overlay").unwrap();
    approx_eq(overlay.y1 - overlay.y0, 30.0, 0.5, "title row 30px");
}

#[test]
fn slot_lookup_by_string_and_typed_slot_agree() {
    let p = Patch::new("p")
        .slot(Slot::Panel, Cell::empty())
        .slot(Slot::Title, sized(0.0, 25.0));
    let layout = p.solve(Size::new(400.0, 200.0), 96.0);
    let typed = layout.get("p", Slot::Title).unwrap();
    let stringy = layout.get("p", "title").unwrap();
    assert_eq!(typed.x0, stringy.x0);
    assert_eq!(typed.y0, stringy.y0);
    assert_eq!(typed.x1, stringy.x1);
    assert_eq!(typed.y1, stringy.y1);
}

#[test]
fn missing_lookup_returns_none() {
    let p = Patch::new("p").slot(Slot::Panel, Cell::empty());
    let layout = p.solve(Size::new(400.0, 200.0), 96.0);
    assert!(layout.get("p", Slot::Title).is_none());
    assert!(layout.get("nope", Slot::Panel).is_none());
    assert!(layout.get("p", "unregistered").is_none());
}

// ─── Composition tests (step 3) ─────────────────────────────────────

/// Build a patch with `panel` and the given left-axis width / bottom-axis
/// height.
fn axis_patch(id: &str, axis_left_w: f64, axis_bottom_h: f64) -> Patch {
    Patch::new(id)
        .slot(Slot::AxisLeft, sized(axis_left_w, 0.0))
        .slot(Slot::AxisBottom, sized(0.0, axis_bottom_h))
        .slot(Slot::Panel, Cell::empty())
}

#[test]
fn beside_aligns_panels_with_different_axis_widths() {
    // p1 has a 20px y-axis, p2 has 80px. Their panels should align — both
    // start at x = max(20, 80) = 80 (since stack-wise, both block 0 and
    // block 1's AxisLeft cols merge under the same... wait — beside
    // doesn't merge cols across blocks. The headline alignment in beside
    // is the ROW (y-axis: panels share y range).
    //
    // For "panels share x0 from the left edge of each block", we need
    // each block's AxisLeft Auto col to take its own max. Block 0's
    // AxisLeft col → 20. Block 1's AxisLeft col → 80. Panels start
    // at distinct positions within their blocks.
    //
    // What does align in `beside`: the rows. Both panels share y0/y1.
    let p1 = axis_patch("p1", 20.0, 30.0);
    let p2 = axis_patch("p2", 80.0, 30.0);
    let comp = beside(p1, p2);
    let layout = comp.solve(Size::new(1000.0, 400.0), 96.0);

    let panel1 = layout.get("p1", Slot::Panel).unwrap();
    let panel2 = layout.get("p2", Slot::Panel).unwrap();
    approx_eq(panel1.y0, panel2.y0, 0.5, "panels share y0");
    approx_eq(panel1.y1, panel2.y1, 0.5, "panels share y1");
    // Block 0 panel starts after a 20px axis. Block 1 panel starts after a
    // 80px axis. Both panels have equal Fr(1) widths.
    approx_eq(panel1.x0, 20.0, 0.5, "p1.panel.x0 after 20px y-axis");
}

#[test]
fn stack_aligns_panels_with_different_x_axis_heights() {
    let p1 = axis_patch("p1", 30.0, 20.0);
    let p2 = axis_patch("p2", 30.0, 80.0);
    let comp = stack(p1, p2);
    let layout = comp.solve(Size::new(400.0, 1000.0), 96.0);

    let panel1 = layout.get("p1", Slot::Panel).unwrap();
    let panel2 = layout.get("p2", Slot::Panel).unwrap();
    // In stack, the y-axes (column) merge: max(30, 30) = 30. Both panels
    // share x0/x1.
    approx_eq(panel1.x0, panel2.x0, 0.5, "panels share x0");
    approx_eq(panel1.x1, panel2.x1, 0.5, "panels share x1");
    approx_eq(panel1.x0, 30.0, 0.5, "both panels start at 30 (max axis)");
}

#[test]
fn stack_y_axes_merge_to_max() {
    // y-axes in different rows but same column: AxisLeft Auto col width
    // = max(50, 100) = 100.
    let p1 = axis_patch("p1", 50.0, 0.0);
    let p2 = axis_patch("p2", 100.0, 0.0);
    let comp = stack(p1, p2);
    let layout = comp.solve(Size::new(400.0, 600.0), 96.0);
    let a1 = layout.get("p1", Slot::AxisLeft).unwrap();
    let a2 = layout.get("p2", Slot::AxisLeft).unwrap();
    approx_eq(a1.x1 - a1.x0, 100.0, 0.5, "AxisLeft col = max width");
    approx_eq(a2.x1 - a2.x0, 100.0, 0.5, "both axes occupy the merged col");
}

#[test]
fn grid_2x2_aligns_per_row_and_per_column() {
    // 2x2:
    //   p1 (axis 20 wide, axis 10 tall)   p2 (axis 80 wide, axis 10 tall)
    //   p3 (axis 20 wide, axis 40 tall)   p4 (axis 80 wide, axis 40 tall)
    // p1.AxisLeft and p3.AxisLeft merge in composition col 1 → 20.
    // p2.AxisLeft and p4.AxisLeft merge in composition col 2 → 80.
    // p1.AxisBottom and p2.AxisBottom merge in composition row 1 → 10.
    // p3.AxisBottom and p4.AxisBottom merge in composition row 2 → 40.
    let p1 = axis_patch("p1", 20.0, 10.0);
    let p2 = axis_patch("p2", 80.0, 10.0);
    let p3 = axis_patch("p3", 20.0, 40.0);
    let p4 = axis_patch("p4", 80.0, 40.0);
    let comp = grid(2, 2, vec![p1.into(), p2.into(), p3.into(), p4.into()]);
    let layout = comp.solve(Size::new(800.0, 800.0), 96.0);

    let pan1 = layout.get("p1", Slot::Panel).unwrap();
    let pan2 = layout.get("p2", Slot::Panel).unwrap();
    let pan3 = layout.get("p3", Slot::Panel).unwrap();
    let pan4 = layout.get("p4", Slot::Panel).unwrap();

    // Per composition row, the panels share y range.
    approx_eq(pan1.y0, pan2.y0, 0.5, "p1/p2 panels share y0");
    approx_eq(pan3.y0, pan4.y0, 0.5, "p3/p4 panels share y0");

    // Per composition column, the panels share x range.
    approx_eq(pan1.x0, pan3.x0, 0.5, "p1/p3 panels share x0");
    approx_eq(pan2.x0, pan4.x0, 0.5, "p2/p4 panels share x0");

    // Within composition col 1: panel.x0 = 20 (the AxisLeft width).
    approx_eq(pan1.x0, 20.0, 0.5, "col 1 panels start at 20");
}

#[test]
fn spacer_takes_no_chrome() {
    // A spacer next to a real plot. Spacer has no chrome → its block's
    // axis cols are all 0; both panels split the Fr space equally.
    let p1 = axis_patch("p1", 30.0, 0.0);
    let comp = beside(p1, spacer());
    let layout = comp.solve(Size::new(1000.0, 200.0), 96.0);
    let panel = layout.get("p1", Slot::Panel).unwrap();
    // Width allotted to p1's panel: (1000 - 30) / 2 = 485. (1 Fr out of 2,
    // minus the 30 left axis applied only to block 0.)
    approx_eq(panel.x1 - panel.x0, 485.0, 0.5, "panel takes 1 of 2 Fr");
}

#[test]
fn wrap_aligns_at_panel_row() {
    let p1 = axis_patch("p1", 30.0, 0.0);
    let comp = beside(p1, wrap("w", sized(0.0, 0.0)));
    let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
    let p1_panel = layout.get("p1", Slot::Panel).unwrap();
    let w_panel = layout.get("w", Slot::Panel).unwrap();
    approx_eq(
        p1_panel.y0,
        w_panel.y0,
        0.5,
        "wrap panel.y0 == plot panel.y0",
    );
    approx_eq(
        p1_panel.y1,
        w_panel.y1,
        0.5,
        "wrap panel.y1 == plot panel.y1",
    );
}

#[test]
fn duplicate_ids_caught() {
    let p1 = Patch::new("dup")
        .slot(Slot::Panel, Cell::empty())
        .slot(Slot::Title, sized(0.0, 20.0));
    let p2 = Patch::new("dup")
        .slot(Slot::Panel, Cell::empty())
        .slot(Slot::Title, sized(0.0, 20.0));
    let comp = beside(p1, p2);
    let result = comp.try_solve(Size::new(400.0, 200.0), 96.0);
    assert!(
        matches!(result, Err(CompositionError::DuplicateId(_))),
        "duplicate id not caught (got {})",
        if result.is_ok() { "Ok" } else { "wrong-error" }
    );
}

#[test]
fn widths_relative_ratio() {
    // 2:1 panel ratio. Subtract 30+30 chrome → 740 split 2:1 → ~493 / 247.
    let p1 = axis_patch("p1", 30.0, 0.0);
    let p2 = axis_patch("p2", 30.0, 0.0);
    let comp = beside(p1, p2).widths(vec![Track::Fr(2.0), Track::Fr(1.0)]);
    let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
    let panel1 = layout.get("p1", Slot::Panel).unwrap();
    let panel2 = layout.get("p2", Slot::Panel).unwrap();
    let w1 = panel1.x1 - panel1.x0;
    let w2 = panel2.x1 - panel2.x0;
    approx_eq(w1 / w2, 2.0, 0.01, "panel width ratio 2:1");
}

#[test]
fn widths_absolute() {
    let p1 = Patch::new("p1").slot(Slot::Panel, Cell::empty());
    let p2 = Patch::new("p2").slot(Slot::Panel, Cell::empty());
    let comp = beside(p1, p2).widths(vec![
        Track::Fixed(Extent::px(120.0)),
        Track::Fixed(Extent::px(60.0)),
    ]);
    let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
    let panel1 = layout.get("p1", Slot::Panel).unwrap();
    let panel2 = layout.get("p2", Slot::Panel).unwrap();
    approx_eq(panel1.x1 - panel1.x0, 120.0, 0.5, "p1 = 120px");
    approx_eq(panel2.x1 - panel2.x0, 60.0, 0.5, "p2 = 60px");
}

#[test]
fn widths_mixed_fixed_and_fr() {
    let p1 = Patch::new("p1").slot(Slot::Panel, Cell::empty());
    let p2 = Patch::new("p2").slot(Slot::Panel, Cell::empty());
    let comp = beside(p1, p2).widths(vec![Track::Fixed(Extent::px(120.0)), Track::Fr(1.0)]);
    let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
    let panel1 = layout.get("p1", Slot::Panel).unwrap();
    let panel2 = layout.get("p2", Slot::Panel).unwrap();
    approx_eq(panel1.x1 - panel1.x0, 120.0, 0.5, "p1 fixed at 120px");
    approx_eq(panel2.x1 - panel2.x0, 680.0, 0.5, "p2 absorbs the rest");
}

#[test]
fn composition_place_with_col_span() {
    // p1 spans (row 1, cols 1-2), p2 in (row 2, col 1), p3 in (row 2, col 2).
    let p1 = axis_patch("p1", 0.0, 0.0);
    let p2 = axis_patch("p2", 0.0, 0.0);
    let p3 = axis_patch("p3", 0.0, 0.0);
    let comp = Composition::empty(2, 2)
        .place(1, 1, Span::cols(2), p1)
        .place(2, 1, Span::cell(), p2)
        .place(2, 2, Span::cell(), p3);
    let layout = comp.solve(Size::new(800.0, 400.0), 96.0);

    let pan1 = layout.get("p1", Slot::Panel).unwrap();
    let pan2 = layout.get("p2", Slot::Panel).unwrap();
    let pan3 = layout.get("p3", Slot::Panel).unwrap();

    // p1's panel spans from p2's panel left to p3's panel right
    // (including interior chrome between them).
    assert!(
        pan1.x0 <= pan2.x0 + 0.5 && pan1.x1 >= pan3.x1 - 0.5,
        "p1 panel spans across p2/p3 panels (pan1: {pan1:?}, pan2: {pan2:?}, pan3: {pan3:?})"
    );
    // p2 and p3 share the same y range (both in composition row 2).
    approx_eq(pan2.y0, pan3.y0, 0.5, "p2/p3 share y0");
    approx_eq(pan2.y1, pan3.y1, 0.5, "p2/p3 share y1");
}

// ─── Margin + padding tests ─────────────────────────────────────────

#[test]
fn margin_pushes_panel_inward_uniformly() {
    // 200×200 viewport with margin = 10pt (= ~13.33 px at 96 dpi).
    // No padding, no chrome → panel fills viewport minus 2*margin
    // on each axis.
    let p = Patch::new("p")
        .slot(Slot::Panel, Cell::empty())
        .margin_all(Extent::pt(10.0));
    let layout = p.solve(Size::new(200.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let margin_px = 10.0 * 96.0 / 72.0;
    approx_eq(panel.x0, margin_px, 0.5, "panel starts after margin_left");
    approx_eq(panel.y0, margin_px, 0.5, "panel starts after margin_top");
    approx_eq(
        panel.x1,
        200.0 - margin_px,
        0.5,
        "panel ends before margin_right",
    );
    approx_eq(
        panel.y1,
        200.0 - margin_px,
        0.5,
        "panel ends before margin_bottom",
    );
}

#[test]
fn padding_pushes_panel_inward_too() {
    // Padding has the same effect on chrome+panel position as margin —
    // both ring tracks contribute to pushing the panel inward.
    // Difference: bg covers padding area; bg does not cover margin
    // (verified in a separate test).
    let p = Patch::new("p")
        .slot(Slot::Panel, Cell::empty())
        .padding_all(Extent::pt(6.0));
    let layout = p.solve(Size::new(200.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let padding_px = 6.0 * 96.0 / 72.0;
    approx_eq(panel.x0, padding_px, 0.5, "panel starts after padding_left");
    approx_eq(
        panel.x1,
        200.0 - padding_px,
        0.5,
        "panel ends before padding_right",
    );
}

#[test]
fn margin_and_padding_combine() {
    // margin = 5pt, padding = 3pt → chrome offset = 8pt on each side.
    let p = Patch::new("p")
        .slot(Slot::Panel, Cell::empty())
        .margin_all(Extent::pt(5.0))
        .padding_all(Extent::pt(3.0));
    let layout = p.solve(Size::new(200.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    let combined_px = (5.0 + 3.0) * 96.0 / 72.0;
    approx_eq(
        panel.x0,
        combined_px,
        0.5,
        "panel starts after margin + padding",
    );
    approx_eq(
        panel.x1,
        200.0 - combined_px,
        0.5,
        "panel ends before margin + padding",
    );
}

#[test]
fn background_covers_padding_but_not_margin() {
    // With margin = 5pt, padding = 3pt, the bg should be drawn from
    // offset 5pt (margin only) to (size - 5pt), so its area covers the
    // padding ring + chrome+panel area.
    let p = Patch::new("p")
        .slot(Slot::Background, sized(0.0, 0.0)) // bg present, intrinsic 0
        .slot(Slot::Panel, Cell::empty())
        .margin_all(Extent::pt(5.0))
        .padding_all(Extent::pt(3.0));
    let layout = p.solve(Size::new(200.0, 200.0), 96.0);
    let bg = layout.get("p", Slot::Background).unwrap();
    let margin_px = 5.0 * 96.0 / 72.0;
    approx_eq(
        bg.x0,
        margin_px,
        0.5,
        "bg starts after margin (not padding)",
    );
    approx_eq(bg.x1, 200.0 - margin_px, 0.5, "bg ends before margin");
    approx_eq(bg.y0, margin_px, 0.5, "bg top after margin_top");
    approx_eq(
        bg.y1,
        200.0 - margin_px,
        0.5,
        "bg bottom before margin_bottom",
    );
    // Sanity: padding-sized space between bg edge and panel.
    let panel = layout.get("p", Slot::Panel).unwrap();
    let padding_px = 3.0 * 96.0 / 72.0;
    approx_eq(
        panel.x0 - bg.x0,
        padding_px,
        0.5,
        "padding-sized gap between bg.left and panel.left",
    );
}

#[test]
fn adjacent_patches_have_margin_gap_between_backgrounds() {
    // Two patches side-by-side, each with margin = 4pt. The bgs should
    // be separated by 8pt (margin_a.right + margin_b.left).
    let p1 = Patch::new("p1")
        .slot(Slot::Background, sized(0.0, 0.0))
        .slot(Slot::Panel, Cell::empty())
        .margin_all(Extent::pt(4.0));
    let p2 = Patch::new("p2")
        .slot(Slot::Background, sized(0.0, 0.0))
        .slot(Slot::Panel, Cell::empty())
        .margin_all(Extent::pt(4.0));
    let comp = beside(p1, p2);
    let layout = comp.solve(Size::new(400.0, 200.0), 96.0);
    let bg1 = layout.get("p1", Slot::Background).unwrap();
    let bg2 = layout.get("p2", Slot::Background).unwrap();
    let margin_px = 4.0 * 96.0 / 72.0;
    approx_eq(
        bg2.x0 - bg1.x1,
        2.0 * margin_px,
        0.5,
        "gap between bgs = margin_a.right + margin_b.left",
    );
}

#[test]
fn asymmetric_margin_per_side() {
    // Different margin on each side — verify each is applied independently.
    let p = Patch::new("p").slot(Slot::Panel, Cell::empty()).margin(
        Inset::default()
            .left(Extent::pt(2.0))
            .right(Extent::pt(8.0))
            .top(Extent::pt(3.0))
            .bottom(Extent::pt(6.0)),
    );
    let layout = p.solve(Size::new(200.0, 200.0), 96.0);
    let panel = layout.get("p", Slot::Panel).unwrap();
    approx_eq(panel.x0, 2.0 * 96.0 / 72.0, 0.5, "left margin");
    approx_eq(panel.x1, 200.0 - 8.0 * 96.0 / 72.0, 0.5, "right margin");
    approx_eq(panel.y0, 3.0 * 96.0 / 72.0, 0.5, "top margin");
    approx_eq(panel.y1, 200.0 - 6.0 * 96.0 / 72.0, 0.5, "bottom margin");
}

// ─── Nesting tests ──────────────────────────────────────────────────

#[test]
fn composition_in_composition_cell_solves() {
    // Nesting a 1×2 inner composition directly inside a 1×1 outer's
    // single cell. With the recursive flatten this is well-defined —
    // the outer's cell footprint expands to accommodate the inner.
    let inner = beside(
        Patch::new("a").slot(Slot::Panel, Cell::empty()),
        Patch::new("b").slot(Slot::Panel, Cell::empty()),
    );
    let outer = Composition::empty(1, 1).place(1, 1, Span::cell(), inner);
    let layout = outer.solve(Size::new(400.0, 200.0), 96.0);
    let a = layout.get("a", Slot::Panel).unwrap();
    let b = layout.get("b", Slot::Panel).unwrap();
    // Two inner panels split the 400px-wide viewport evenly.
    approx_eq(a.x0, 0.0, 0.5, "a starts at left");
    approx_eq(a.x1, 200.0, 0.5, "a ends at midpoint");
    approx_eq(b.x0, 200.0, 0.5, "b starts at midpoint");
    approx_eq(b.x1, 400.0, 0.5, "b ends at right");
    // Both panels share y bounds.
    approx_eq(a.y0, b.y0, 0.5, "panels share y0");
    approx_eq(a.y1, b.y1, 0.5, "panels share y1");
}

#[test]
fn nested_composition_in_composition_cell_with_axis_chrome() {
    // Outer 1×2 composition: cell (1,1) is a plain patch with a 20px
    // axis_left; cell (1,2) is a nested 1×2 composition with two inner
    // patches. The plain block's axis_left contributes 20px to its
    // outer block's axis_left col. The nested block's axis_left col
    // has no content (inner_a has no axis_left), so it stays 0.
    let plain = Patch::new("plain")
        .slot(Slot::AxisLeft, sized(20.0, 0.0))
        .slot(Slot::Panel, Cell::empty());
    let inner = beside(
        Patch::new("inner_a").slot(Slot::Panel, Cell::empty()),
        Patch::new("inner_b").slot(Slot::Panel, Cell::empty()),
    );
    let comp = beside(plain, inner);
    let layout = comp.solve(Size::new(800.0, 300.0), 96.0);

    let plain_axis = layout.get("plain", Slot::AxisLeft).unwrap();
    approx_eq(plain_axis.x1 - plain_axis.x0, 20.0, 0.5, "plain axis width");

    // Nested cell contains both inner panels side-by-side.
    let inner_a_panel = layout.get("inner_a", Slot::Panel).unwrap();
    let inner_b_panel = layout.get("inner_b", Slot::Panel).unwrap();
    approx_eq(
        inner_a_panel.y0,
        inner_b_panel.y0,
        0.5,
        "inner panels share y0",
    );
    approx_eq(
        inner_a_panel.x1,
        inner_b_panel.x0,
        0.5,
        "inner_a's right edge meets inner_b's left edge",
    );
    // Plain panel y range matches inner panels.
    let plain_panel = layout.get("plain", Slot::Panel).unwrap();
    approx_eq(
        plain_panel.y0,
        inner_a_panel.y0,
        0.5,
        "plain and inner share y0",
    );
}

#[test]
fn stack_of_1x3_and_1x2_compositions() {
    // User's stated "would cause havoc" case: a 1×3 stacked over a 1×2.
    // Each row should fill its half of the viewport: row_a's 3 panels
    // tile its 200px height, row_b's 2 panels tile its 200px height.
    // Both rows should consume the full viewport width.
    let row_a = grid(
        1,
        3,
        vec![
            Patch::new("a1").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("a2").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("a3").slot(Slot::Panel, Cell::empty()).into(),
        ],
    );
    let row_b = grid(
        1,
        2,
        vec![
            Patch::new("b1").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("b2").slot(Slot::Panel, Cell::empty()).into(),
        ],
    );
    let stacked = stack(row_a, row_b);
    let layout = stacked.solve(Size::new(600.0, 400.0), 96.0);
    let a1 = layout.get("a1", Slot::Panel).unwrap();
    let a2 = layout.get("a2", Slot::Panel).unwrap();
    let a3 = layout.get("a3", Slot::Panel).unwrap();
    let b1 = layout.get("b1", Slot::Panel).unwrap();
    let b2 = layout.get("b2", Slot::Panel).unwrap();

    // row_a: 3 panels tile the 600 px row width.
    approx_eq(a1.x0, 0.0, 0.5, "a1 starts at left edge");
    approx_eq(a3.x1, 600.0, 0.5, "a3 ends at right edge");
    approx_eq(a1.x1, a2.x0, 0.5, "a1.x1 meets a2.x0");
    approx_eq(a2.x1, a3.x0, 0.5, "a2.x1 meets a3.x0");

    // row_b: 2 panels tile the 600 px row width.
    approx_eq(b1.x0, 0.0, 0.5, "b1 starts at left edge");
    approx_eq(b2.x1, 600.0, 0.5, "b2 ends at right edge");
    approx_eq(b1.x1, b2.x0, 0.5, "b1.x1 meets b2.x0");
    approx_eq(
        b2.x1 - b2.x0,
        300.0,
        0.5,
        "b2 fills its half of the row (300 px)",
    );

    // Rows are vertically separated.
    approx_eq(a1.y1, b1.y0, 0.5, "row_a's bottom meets row_b's top");
}

#[test]
fn nested_composition_panels_align_with_sibling_panel() {
    // A 1×2 outer where cell (1,1) is plain and cell (1,2) is nested
    // 1×2. Both blocks share rows → all panels share y bounds.
    let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
    let inner = beside(
        Patch::new("a").slot(Slot::Panel, Cell::empty()),
        Patch::new("b").slot(Slot::Panel, Cell::empty()),
    );
    let comp = beside(plain, inner);
    let layout = comp.solve(Size::new(600.0, 200.0), 96.0);
    let plain_panel = layout.get("plain", Slot::Panel).unwrap();
    let a_panel = layout.get("a", Slot::Panel).unwrap();
    let b_panel = layout.get("b", Slot::Panel).unwrap();
    approx_eq(plain_panel.y0, a_panel.y0, 0.5, "plain & a share y0");
    approx_eq(plain_panel.y1, a_panel.y1, 0.5, "plain & a share y1");
    approx_eq(a_panel.y0, b_panel.y0, 0.5, "a & b share y0");
}

// ─── Cross-grid sizer coupling tests ────────────────────────────────

#[test]
fn nested_sibling_grows_inner_chrome() {
    // Outer 1×2: plain patch with AxisTop=80 in cell (1,1); nested
    // 1×2 composition in cell (1,2) whose inner patches have no
    // AxisTop. The outer-grid row band 1..16 is shared across both
    // blocks; outer row 8 (AxisTop) resolves from the plain patch's
    // 80px content cell. The nested block contributes 0 (inner has
    // no axis_top); back-sizer in the sub-Grid reads outer row 8 and
    // forces sub's inner row 8 to also be 80. Both panels start at
    // y = 80 (the resolved row band height above panel).
    let plain = Patch::new("plain")
        .slot(Slot::AxisTop, sized(0.0, 80.0))
        .slot(Slot::Panel, Cell::empty());
    let inner = beside(
        Patch::new("c1").slot(Slot::Panel, Cell::empty()),
        Patch::new("c2").slot(Slot::Panel, Cell::empty()),
    );
    let comp = beside(plain, inner);
    let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
    let plain_panel = layout.get("plain", Slot::Panel).unwrap();
    let c1_panel = layout.get("c1", Slot::Panel).unwrap();
    let c2_panel = layout.get("c2", Slot::Panel).unwrap();
    approx_eq(plain_panel.y0, 80.0, 0.5, "plain panel below 80px axis_top");
    approx_eq(
        c1_panel.y0,
        80.0,
        0.5,
        "c1 panel also below 80 via coupling",
    );
    approx_eq(
        c2_panel.y0,
        80.0,
        0.5,
        "c2 panel also below 80 via coupling",
    );
}

#[test]
fn nested_inner_grows_sibling_chrome() {
    // Symmetric: plain patch has no AxisTop; nested inner patches do
    // (60px). The sub-Grid's inner row 8 resolves to 60 from its
    // content. The forward sizer in the outer reads 60 and grows
    // outer row 8 to 60. Plain side now starts its panel at y=60.
    let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
    let inner = beside(
        Patch::new("c1")
            .slot(Slot::AxisTop, sized(0.0, 60.0))
            .slot(Slot::Panel, Cell::empty()),
        Patch::new("c2")
            .slot(Slot::AxisTop, sized(0.0, 60.0))
            .slot(Slot::Panel, Cell::empty()),
    );
    let comp = beside(plain, inner);
    let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
    let plain_panel = layout.get("plain", Slot::Panel).unwrap();
    let c1_panel = layout.get("c1", Slot::Panel).unwrap();
    approx_eq(
        plain_panel.y0,
        60.0,
        0.5,
        "plain panel grown by inner chrome",
    );
    approx_eq(c1_panel.y0, 60.0, 0.5, "c1 panel below own axis_top");
}

#[test]
fn nested_axis_left_width_propagates() {
    // Sibling plain patch has no axis_left; nested has c1 with
    // axis_left=70. Outer block col 6 (axis_left col) of the nested
    // block resolves via forward sizer to 70. The plain panel starts
    // at x=0; nested c1 panel starts at x = plain_block_total + 70
    // (start of nested block + axis_left).
    let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
    let inner = beside(
        Patch::new("c1")
            .slot(Slot::AxisLeft, sized(70.0, 0.0))
            .slot(Slot::Panel, Cell::empty()),
        Patch::new("c2").slot(Slot::Panel, Cell::empty()),
    );
    let comp = beside(plain, inner);
    let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
    let c1_axis = layout.get("c1", Slot::AxisLeft).unwrap();
    approx_eq(c1_axis.x1 - c1_axis.x0, 70.0, 0.5, "c1 axis_left = 70");
    let c1_panel = layout.get("c1", Slot::Panel).unwrap();
    approx_eq(
        c1_panel.x0 - c1_axis.x0,
        70.0,
        0.5,
        "panel sits right of axis",
    );
}

#[test]
fn three_level_nesting_converges() {
    // Composition-of-composition-of-composition. Deepest inner
    // patches have non-trivial chrome (axis_top, axis_left). The
    // bidirectional sizer pair at each boundary needs ~3 iterations
    // to propagate sizes through the 3-level chain. Just verify
    // finite rects and panel alignment.
    let leaf_row = beside(
        Patch::new("l1")
            .slot(Slot::AxisTop, sized(0.0, 25.0))
            .slot(Slot::Panel, Cell::empty()),
        Patch::new("l2").slot(Slot::Panel, Cell::empty()),
    );
    let mid_row = beside(Patch::new("m1").slot(Slot::Panel, Cell::empty()), leaf_row);
    let outer = beside(Patch::new("o1").slot(Slot::Panel, Cell::empty()), mid_row);
    let layout = outer.solve(Size::new(1200.0, 400.0), 96.0);
    let l1 = layout.get("l1", Slot::Panel).unwrap();
    let l2 = layout.get("l2", Slot::Panel).unwrap();
    let m1 = layout.get("m1", Slot::Panel).unwrap();
    let o1 = layout.get("o1", Slot::Panel).unwrap();
    approx_eq(l1.y0, l2.y0, 0.5, "leaf siblings share y0");
    approx_eq(l1.y0, m1.y0, 0.5, "leaf and mid sibling share y0");
    approx_eq(l1.y0, o1.y0, 0.5, "leaf and outer sibling share y0");
    approx_eq(
        l1.y0,
        25.0,
        0.5,
        "all panels below 25px axis_top from deepest leaf",
    );
    assert!(l1.x1 - l1.x0 > 0.0, "l1 panel has positive width");
    assert!(l2.x1 - l2.x0 > 0.0, "l2 panel has positive width");
}

#[test]
fn try_solve_reports_construction_errors() {
    // These used to panic inside the builder, which meant
    // `try_solve` could not be used to validate a composition —
    // half its failure modes had already aborted.
    let size = Size::new(400.0, 300.0);
    let cases: Vec<(&str, Composition)> = vec![
        ("zero extent", Composition::empty(0, 2)),
        (
            "off-grid placement",
            Composition::empty(1, 1).place(1, 5, Span::cell(), Patch::new("p")),
        ),
        (
            "zero-indexed placement",
            Composition::empty(1, 1).place(0, 1, Span::cell(), Patch::new("p")),
        ),
        (
            "track count",
            Composition::empty(1, 2).widths(vec![Track::Fr(1.0)]),
        ),
        (
            "not appendable",
            Composition::empty(2, 2).append_col(Patch::new("p")),
        ),
        ("cell count", grid(2, 2, vec![Patch::new("p").into()])),
    ];
    for (label, comp) in cases {
        assert!(
            comp.error().is_some(),
            "{label}: expected the builder to record an error"
        );
        assert!(
            comp.try_solve(size, 96.0).is_err(),
            "{label}: the recorded error must reach try_solve"
        );
    }

    // And the error survives into `try_solve`, including from a
    // nested composition several levels down.
    let nested = Composition::empty(1, 1).place(
        1,
        1,
        Span::cell(),
        Composition::empty(1, 1).place(1, 5, Span::cell(), Patch::new("p")),
    );
    assert!(
        matches!(
            nested.try_solve(size, 96.0),
            Err(CompositionError::PlacementOverflow { .. })
        ),
        "a nested construction error must reach the root's try_solve"
    );
}

#[test]
fn aspect_only_composition_cascades_without_wrapping() {
    // An aspect is not chrome: it cascades into the descendants
    // rather than locking a cell of the composition's own, so
    // setting it on the parent resolves identically to setting it
    // on each leaf.
    let leaf = |id: &str| {
        Patch::new(id)
            .slot(Slot::Background, Cell::empty())
            .slot(Slot::Panel, Cell::empty())
    };
    let size = Size::new(800.0, 500.0);

    let cascaded = beside(leaf("a"), leaf("b")).aspect(1.0, 1.0);
    let cascaded = cascaded.solve(size, 96.0);

    let per_leaf = beside(leaf("a").aspect(1.0, 1.0), leaf("b").aspect(1.0, 1.0));
    let per_leaf = per_leaf.solve(size, 96.0);

    for id in ["a", "b"] {
        let c = cascaded.get(id, Slot::Panel).unwrap();
        let p = per_leaf.get(id, Slot::Panel).unwrap();
        approx_eq(c.x1 - c.x0, c.y1 - c.y0, 0.5, &format!("{id} panel square"));
        approx_eq(c.x0, p.x0, 0.5, &format!("{id} x0 matches per-leaf"));
        approx_eq(c.y0, p.y0, 0.5, &format!("{id} y0 matches per-leaf"));
        approx_eq(c.x1, p.x1, 0.5, &format!("{id} x1 matches per-leaf"));
    }
}

#[test]
fn aspect_slack_pools_outside_the_composition() {
    // A cascading aspect lock leaves the composition narrower than the
    // canvas. That slack belongs at the composition's outer edges, not
    // stranded between siblings: a nested grid counts as aspect-bearing
    // (its leaves are locked by the same cascade), so its panel track is
    // respected instead of soaking up the whole axis.
    // Axis chrome on every patch matters here: the facet block pays for
    // it per facet (two left rails, two bottom rails) while its sibling
    // pays once, which is the asymmetry that used to strand the slack.
    let leaf = |id: &str| {
        Patch::new(id)
            .slot(Slot::Background, Cell::empty())
            .slot(Slot::AxisLeft, sized(26.0, 0.0))
            .slot(Slot::AxisBottom, sized(0.0, 20.0))
            .slot(Slot::Panel, Cell::empty())
    };
    let facets = grid(
        2,
        2,
        vec![
            leaf("q1").into(),
            leaf("q2").into(),
            leaf("q3").into(),
            leaf("q4").into(),
        ],
    );
    let comp = beside(facets, leaf("summary")).aspect(1.0, 1.0);
    let layout = comp.solve(Size::new(1800.0, 600.0), 96.0);

    let q1 = layout.get("q1", Slot::Panel).unwrap();
    let q2 = layout.get("q2", Slot::Panel).unwrap();
    let summary = layout.get("summary", Slot::Panel).unwrap();

    // Every leaf panel honors the cascaded 1:1 lock.
    for (id, r) in [("q1", q1), ("q2", q2), ("summary", summary)] {
        approx_eq(r.x1 - r.x0, r.y1 - r.y0, 0.5, &format!("{id} panel square"));
    }

    // Compare whole blocks, not panels — the sibling's own axis rail
    // legitimately sits between the two panels. `Slot::Background`
    // spans the block (margin excluded), so block edges meet when
    // there is no dead space.
    let q1_block = layout.get("q1", Slot::Background).unwrap();
    let q2_block = layout.get("q2", Slot::Background).unwrap();
    let summary_block = layout.get("summary", Slot::Background).unwrap();

    // No dead space between the facet grid and its sibling.
    let interior_gap = summary_block.x0 - q2_block.x1;
    assert!(
        interior_gap < 1.0,
        "facet grid and summary should sit flush, got a {interior_gap}px gap"
    );

    // The slack shows up as balanced gutters at the outer edges.
    let (left, right) = (q1_block.x0, 1800.0 - summary_block.x1);
    assert!(
        left > 100.0 && right > 100.0,
        "expected outer gutters to absorb the slack, got left {left} right {right}"
    );
    approx_eq(left, right, 1.0, "outer gutters balanced");
}

// ─── Composition-level chrome tests ─────────────────────────────────

#[test]
fn composition_with_title_spans_facets() {
    // A 2×3 facet composition with a composition-level Title slot.
    // The Title rect should span across all facet panels (since the
    // facets fill the panel cell of the simplified canonical block,
    // and Title at anatomical row 3 cols 3..11 stretches across the
    // composition's full plot-area width).
    let facets = grid(
        2,
        3,
        vec![
            Patch::new("f1").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f2").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f3").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f4").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f5").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f6").slot(Slot::Panel, Cell::empty()).into(),
        ],
    )
    .id("plot")
    .slot(Slot::Title, sized(0.0, 30.0))
    .slot(Slot::Caption, sized(0.0, 15.0));
    let layout = facets.solve(Size::new(900.0, 400.0), 96.0);

    let title = layout.get("plot", Slot::Title).expect("title rect");
    let caption = layout.get("plot", Slot::Caption).expect("caption rect");
    let f1 = layout.get("f1", Slot::Panel).unwrap();
    let f3 = layout.get("f3", Slot::Panel).unwrap();
    let f4 = layout.get("f4", Slot::Panel).unwrap();
    let f6 = layout.get("f6", Slot::Panel).unwrap();

    // Title sits above all facet panels.
    assert!(
        title.y1 <= f1.y0 + 0.5,
        "title.y1 ({}) above facet panels",
        title.y1
    );
    approx_eq(title.y1 - title.y0, 30.0, 0.5, "title height = 30");
    // Title spans the full width of the panel band.
    assert!(title.x0 <= f1.x0 + 0.5, "title reaches first facet left");
    assert!(title.x1 >= f3.x1 - 0.5, "title reaches last facet right");

    // Caption sits below all facet panels.
    assert!(caption.y0 >= f4.y1 - 0.5, "caption below all facets");
    approx_eq(caption.y1 - caption.y0, 15.0, 0.5, "caption height = 15");

    // Facet rows align: f1/f2/f3 share y; f4/f5/f6 share y.
    approx_eq(f1.y0, f3.y0, 0.5, "row 1 facets share y0");
    approx_eq(f4.y0, f6.y0, 0.5, "row 2 facets share y0");
}

#[test]
fn composition_chrome_axis_left_title_spans_facet_rows() {
    // A 1×2 facet composition with a left-axis-title at the
    // canonical (panel_row, axis_left_title_col) position. The
    // title sits to the left of BOTH facet panels (since they fill
    // the canonical panel cell).
    let facets = grid(
        1,
        2,
        vec![
            Patch::new("f1").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("f2").slot(Slot::Panel, Cell::empty()).into(),
        ],
    )
    .id("plot")
    .slot(Slot::AxisLeftTitle, sized(40.0, 0.0));
    let layout = facets.solve(Size::new(800.0, 200.0), 96.0);

    let axis_title = layout.get("plot", Slot::AxisLeftTitle).unwrap();
    let f1 = layout.get("f1", Slot::Panel).unwrap();
    let f2 = layout.get("f2", Slot::Panel).unwrap();
    approx_eq(
        axis_title.x1 - axis_title.x0,
        40.0,
        0.5,
        "axis title width 40",
    );
    assert!(
        axis_title.x1 <= f1.x0 + 0.5,
        "axis title sits left of facet panels"
    );
    approx_eq(f1.y0, f2.y0, 0.5, "facet panels share y0");
    // Facets together occupy the panel cell.
    assert!(f2.x1 - f1.x0 > 0.0, "facets span the panel area");
}

#[test]
fn composition_chrome_nested_inside_another_composition() {
    // A wrapped composition (with chrome) placed in another
    // composition's cell behaves like a single Patch with chrome:
    // its Title aligns to outer block row 3 and propagates to the
    // sibling row via the existing Auto + sizer mechanism.
    let plain = Patch::new("plain")
        .slot(Slot::Title, sized(0.0, 60.0))
        .slot(Slot::Panel, Cell::empty());
    let facets = grid(
        1,
        2,
        vec![
            Patch::new("c1").slot(Slot::Panel, Cell::empty()).into(),
            Patch::new("c2").slot(Slot::Panel, Cell::empty()).into(),
        ],
    )
    .id("nested")
    .slot(Slot::Title, sized(0.0, 60.0));
    let comp = beside(plain, facets);
    let layout = comp.solve(Size::new(800.0, 400.0), 96.0);

    let plain_title = layout.get("plain", Slot::Title).unwrap();
    let nested_title = layout.get("nested", Slot::Title).unwrap();
    let plain_panel = layout.get("plain", Slot::Panel).unwrap();
    let c1_panel = layout.get("c1", Slot::Panel).unwrap();

    // Both titles at the same y range (shared outer-grid title row).
    approx_eq(plain_title.y0, nested_title.y0, 0.5, "titles share y0");
    approx_eq(plain_title.y1, nested_title.y1, 0.5, "titles share y1");
    approx_eq(
        plain_title.y1 - plain_title.y0,
        60.0,
        0.5,
        "title row = 60px",
    );

    // Panels share y0.
    approx_eq(
        plain_panel.y0,
        c1_panel.y0,
        0.5,
        "plain and inner panel share y0",
    );
}

#[test]
fn composition_panel_slot_resolves_to_the_facet_band() {
    let size = Size::new(400.0, 300.0);
    let build = |panel: Option<Cell>| {
        let mut c = Composition::empty(1, 2)
            .id("c")
            .place(1, 1, Span::cell(), wrap("a", sized(30.0, 30.0)))
            .place(1, 2, Span::cell(), wrap("b", sized(30.0, 30.0)))
            .slot(Slot::Title, sized(10.0, 20.0));
        if let Some(cell) = panel {
            c = c.slot(Slot::Panel, cell);
        }
        c.try_solve(size, 96.0).expect("solves")
    };

    // The composition's panel rect covers the facets and nothing else.
    let layout = build(Some(Cell::empty()));
    let panel = layout
        .get("c", Slot::Panel)
        .expect("composition panel rect");
    let a = layout.get("a", Slot::Panel).expect("first facet panel");
    let b = layout.get("b", Slot::Panel).expect("second facet panel");
    approx_eq(panel.x0, a.x0, 0.01, "panel band starts at the first facet");
    approx_eq(panel.x1, b.x1, 0.01, "panel band ends at the last facet");
    approx_eq(panel.y0, a.y0, 0.01, "panel band top matches the facets");
    approx_eq(panel.y1, a.y1, 0.01, "panel band bottom matches the facets");

    // The panel track is Fr, so a measure there cannot move a track:
    // every other rect is identical with and without it.
    let base = build(None);
    let huge = build(Some(sized(9000.0, 9000.0)));
    for (id, region) in [("a", "panel"), ("b", "panel"), ("c", "title")] {
        assert_eq!(
            format!("{:?}", base.get(id, region)),
            format!("{:?}", huge.get(id, region)),
            "{id}/{region} moved when the panel cell was populated"
        );
    }
}

#[test]
fn composition_place_at_may_cover_the_panel() {
    let size = Size::new(400.0, 300.0);
    let layout = Composition::empty(1, 2)
        .id("c")
        .place(1, 1, Span::cell(), wrap("a", sized(30.0, 30.0)))
        .place(1, 2, Span::cell(), wrap("b", sized(30.0, 30.0)))
        // Rows 8–10 × cols 6–8: the panel plus the axis track on each side.
        .place_at("overlay", 8, 6, Span::rc(3, 3), sized(40.0, 40.0))
        .try_solve(size, 96.0)
        .expect("a placement over the panel is valid");
    let overlay = layout.get("c", "overlay").expect("overlay rect");
    let a = layout.get("a", Slot::Panel).expect("first facet panel");
    assert!(
        overlay.x0 <= a.x0 + 0.01 && overlay.y0 <= a.y0 + 0.01,
        "the overlay reaches at least the facet band"
    );
}
