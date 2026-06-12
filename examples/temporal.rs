//! Calendar-aware temporal ticks (Phase E.2).
//!
//! Two side-by-side plots show the same dated time-series rendered
//! with different x-axis configurations:
//!
//! - Left: `scale::continuous(Date..=Date)` — the legacy path. Tick
//!   labels are calendar-formatted (`YYYY-MM-DD`) because the formatter
//!   recognises [`Value::Date`], but tick *positions* are Wilkinson
//!   "nice numbers" in days-since-epoch — so they land on awkward
//!   mid-month dates.
//! - Right: `scale::temporal(Date..=Date)` — opt-in calendar-aware
//!   ticks. The picker chooses month-aligned majors over a one-year
//!   span, so tick positions are the first of each month, with weekly
//!   sub-ticks.
//!
//! Produces `examples/temporal.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Date;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    beside(Patch::new("numeric"), Patch::new("calendar"))
}

fn main() {
    let (w, h) = (1400u32, 500u32);
    let dpi = 96.0;

    // One sample per week across 2024.
    let mut xs: Vec<i32> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let start = Date::from_ymd(2024, 1, 1).to_days();
    let end = Date::from_ymd(2024, 12, 31).to_days();
    let mut day = start;
    while day <= end {
        xs.push(day);
        // Synthetic seasonal pattern: a year-long sine + drift.
        let t = (day - start) as f64 / (end - start) as f64;
        ys.push(50.0 + 30.0 * (t * std::f64::consts::TAU).sin());
        day += 7;
    }
    let dot = rgb8(40, 100, 200);

    let start_d = Date::from_ymd(2024, 1, 1);
    let end_d = Date::from_ymd(2024, 12, 31);

    let mut view = PlotComposition::new(comp_shape())
        // Legacy: continuous-with-Date-endpoints — numeric breaks.
        .add_scale("x_numeric", scale::continuous(start_d..=end_d))
        // Calendar-aware: month-aligned breaks.
        .add_scale("x_calendar", scale::temporal(start_d..=end_d))
        .add_scale("y", scale::continuous(0.0..=100.0));

    for (id, x_scale_name, title) in [
        ("numeric", "x_numeric", "continuous — Wilkinson ticks"),
        ("calendar", "x_calendar", "temporal — month-aligned ticks"),
    ] {
        let mut p = Plot::new(&comp_shape(), id)
            .title(title)
            .bind("x", x_scale_name)
            .bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            x_scale_name,
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/temporal.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
