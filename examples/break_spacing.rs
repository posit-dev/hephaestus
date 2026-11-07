//! Break spacing: `BreakTarget::Spacing` responding to a live resize,
//! on a cartesian panel and a polar one side by side.
//!
//! Every axis on both plots targets a fixed *physical* spacing between
//! adjacent major ticks rather than a tick count. Drag the window
//! corner: as a panel grows, more ticks appear to hold the spacing;
//! shrink it and ticks drop out. Gridlines follow, since they resolve
//! against the same extent the axis draws into.
//!
//! ```sh
//! cargo run --example break_spacing --features window
//! ```

use hephaestus::color::rgb8;
use hephaestus::composition::{beside, Patch};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::Projection;
use hephaestus::plot::{scale, BreakTarget, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::window::{run, Event, EventCtx, Frame, WindowApp, WindowConfig};

/// Spacing every axis on both plots targets, in pt.
const SPACING_PT: f64 = 60.0;

struct Demo {
    view: PlotComposition,
}

impl WindowApp for Demo {
    fn draw(&mut self, frame: &mut Frame<'_>) {
        let (scene, size, dpi) = frame.parts();
        self.view.render(scene, size, dpi);
    }

    fn event(&mut self, ctx: &mut EventCtx<'_>, event: Event) {
        if matches!(event, Event::CloseRequested) {
            ctx.exit();
        }
    }
}

fn main() {
    let comp = || beside(Patch::new("cart"), Patch::new("polar"));

    // Cartesian scatter: both position scales target `SPACING_PT` —
    // the x axis answers to the panel's width, the y to its height.
    let cart = Plot::new(&comp(), "cart").bind("x", "cx").bind("y", "cy");
    let mut cart = cart;
    cart.set_title(format!("cartesian — a tick every {SPACING_PT:.0} pt"));
    cart.add_axis(Axis::rail("cx", AxisPlacement::Cartesian(AxisSide::Bottom)));
    cart.add_axis(Axis::rail("cy", AxisPlacement::Cartesian(AxisSide::Left)));
    cart.add_geom(
        PointGeom::builder()
            .set("x", (0..80).map(|i| i as f64 * 1.27).collect::<Vec<_>>())
            .set(
                "y",
                (0..80)
                    .map(|i| 50.0 + 40.0 * (i as f64 * 0.31).sin())
                    .collect::<Vec<_>>(),
            )
            .set("fill", vec![rgb8(70, 120, 220); 80])
            .build(),
    );

    // Polar scatter: the angular axis answers to the swept arc length
    // at the outer ring, the radius axis to the radial span.
    let mut polar = Plot::new(&comp(), "polar")
        .projection(Projection::polar())
        .bind("x", "theta_deg")
        .bind("y", "radius");
    polar.set_title("polar — same spacing, arc + spoke extents");
    polar.add_axis(Axis::rail(
        "theta_deg",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    polar.add_axis(Axis::rail(
        "radius",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    polar.add_geom(
        PointGeom::builder()
            .set("x", (0..60).map(|i| i as f64 * 6.0).collect::<Vec<_>>())
            .set(
                "y",
                (0..60)
                    .map(|i| 0.2 + 0.8 * ((i as f64 * 0.137).sin() * 0.5 + 0.5))
                    .collect::<Vec<_>>(),
            )
            .set("fill", vec![rgb8(200, 110, 60); 60])
            .build(),
    );

    let spacing = || BreakTarget::Spacing(SPACING_PT);
    let view = PlotComposition::new(&comp())
        .add_scale(
            "cx",
            scale::continuous(0.0..=100.0).with_break_target(spacing()),
        )
        .add_scale(
            "cy",
            scale::continuous(0.0..=100.0).with_break_target(spacing()),
        )
        .add_scale(
            "theta_deg",
            scale::continuous(0.0..=360.0).with_break_target(spacing()),
        )
        .add_scale(
            "radius",
            scale::continuous(0.0..=1.0).with_break_target(spacing()),
        )
        .with_plot(cart)
        .with_plot(polar);

    let config = WindowConfig::new("hephaestus — break spacing")
        .size(1000, 520)
        .background(rgb8(248, 248, 252));

    if let Err(err) = run(config, Demo { view }) {
        eprintln!("window: {err}");
        std::process::exit(1);
    }
}
