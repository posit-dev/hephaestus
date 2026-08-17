// Names a downstream crate reaches through `hephaestus::composition`.
// Compiling this proves each path still resolves after the split.
use hephaestus::composition::{
    beside, grid, spacer, stack, wrap, Composition, CompositionError, CompositionLayout, Element,
    Patch, Slot, Span, PANEL_COL, PANEL_ROW, TABLE_COLS, TABLE_ROWS,
};

#[test]
fn every_public_composition_path_still_resolves() {
    let _ = (TABLE_COLS, TABLE_ROWS, PANEL_ROW, PANEL_COL);
    let p: Patch = wrap("a", hephaestus::layout::Cell::empty());
    let c: Composition = beside(p, spacer());
    let c = stack(c, grid(1, 1, vec![Patch::new("b").into()]));
    let e: Element = c.into();
    let layout: CompositionLayout = e.solve(hephaestus::geometry::Size::new(100.0, 100.0), 96.0);
    assert!(layout.get("a", Slot::Panel).is_some() || layout.get("a", Slot::Panel).is_none());
    let _ = Span::rc(1, 1);
    fn _err(e: CompositionError) -> String {
        e.to_string()
    }
}
