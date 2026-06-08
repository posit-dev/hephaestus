//! Legend rendering stub.
//!
//! The trait surface ([`Scale::legend_measure`] / [`Scale::draw_legend`])
//! lets callers (the [`Plot`](crate::plot) orchestrator, geom legend
//! collectors) wire legend slots into a
//! [`composition::Patch`](crate::composition::Patch) without conditional
//! compilation. Both methods are no-ops: `legend_measure` returns a
//! zero-sized cell and `draw_legend` emits nothing.
//!
//! Gated behind `feature = "text"` to match [`super::axis`].

use crate::geometry::Rect;
use crate::layout::{Measure, WidthHint};
use crate::plot::scale::Scale;
use crate::scene::SceneBuilder;

use crate::scales::chrome::LegendSide;

/// Stub legend Measure — reports zero width / height.
struct LegendStub;

impl Measure for LegendStub {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(0.0)
    }

    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        0.0
    }
}

impl Scale {
    /// Legend-cell measure. Returns a zero-sized stub.
    pub fn legend_measure(&self, _side: LegendSide, _dpi: f64) -> Box<dyn Measure> {
        Box::new(LegendStub)
    }

    /// Legend drawer. No-op.
    pub fn draw_legend(
        &self,
        _scene: &mut dyn SceneBuilder,
        _slot_rect: Rect,
        _side: LegendSide,
        _dpi: f64,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::scale;
    use crate::scene::recording::RecordingScene;

    #[test]
    fn legend_measure_reports_zero() {
        let s = scale::ordinal(["a", "b", "c"]);
        let m = s.legend_measure(LegendSide::Right, 96.0);
        assert_eq!(m.width_hint(96.0), WidthHint::Min(0.0));
        assert_eq!(m.height_at(100.0, 96.0), 0.0);
    }

    #[test]
    fn draw_legend_is_no_op() {
        let s = scale::ordinal(["a", "b", "c"]);
        let mut scene = RecordingScene::default();
        s.draw_legend(
            &mut scene,
            Rect::new(0.0, 0.0, 100.0, 200.0),
            LegendSide::Right,
            96.0,
        );
        assert!(scene.ops.is_empty());
    }
}
