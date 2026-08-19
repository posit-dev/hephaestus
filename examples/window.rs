//! Live window: the same `PlotComposition` an export example builds, shown on
//! screen instead of written to a PNG.
//!
//! Three things worth watching:
//!
//! - **Resize.** Nothing in the app handles it. The composition is re-solved
//!   at whatever size the frame reports, so axes, ticks and titles re-lay-out
//!   rather than stretching.
//! - **Scale factor.** `frame.dpi()` tracks the window's scale factor, so
//!   dragging between displays of different densities keeps text crisp and
//!   physical sizes constant.
//! - **Picking.** Each point carries a `pick_id`; hovering reports the row
//!   under the cursor and redraws it enlarged.
//!
//! The point count is the first CLI argument, so the same scene scales up for
//! a rough feel of how the pipeline copes:
//!
//! ```sh
//! cargo run --release --example window --features window -- 100000
//! ```
//!
//! Two things bound how far that goes. The Vello backend rasterises at most
//! [`MAX_DRAW_INFO_WORDS`] flat-coloured objects per scene, and a per-row
//! `"fill"` makes every point one of them — past that the render returns
//! `BackendError::SceneTooLarge` and `run` exits with it. Well before that,
//! hovering dominates: every crossing rebuilds all five channels for all rows.

use hephaestus::backend::vello::MAX_DRAW_INFO_WORDS;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::window::{run, Event, EventCtx, Frame, WindowApp, WindowConfig};

const BASE_SIZE: f64 = 1.0;
const HOVER_SIZE: f64 = 7.0;
/// Points drawn when no count is given on the command line.
const DEFAULT_POINTS: usize = 40;
/// Domain of the `"time"` scale; the scatter fills it edge to edge.
const X_RANGE: (f64, f64) = (0.0, 100.0);
/// Domain of the `"price"` scale.
const Y_RANGE: (f64, f64) = (40.0, 90.0);

/// Deterministic scatter source, so a given point count always lays out the
/// same way and repeat runs stay comparable.
struct Xorshift(u64);

impl Xorshift {
    fn new() -> Self {
        Self(0x2545_f491_4f6c_dd1d)
    }

    /// Next value in `0.0..1.0`.
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Demo {
    view: PlotComposition,
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Row currently under the cursor, as a `pick_id` (row index + 1).
    hovered: Option<u32>,
}

impl WindowApp for Demo {
    fn draw(&mut self, frame: &mut Frame<'_>) {
        let (scene, size, dpi) = frame.parts();
        self.view.render(scene, size, dpi);
    }

    fn event(&mut self, ctx: &mut EventCtx<'_>, event: Event) {
        match event {
            Event::CursorMoved { position } => {
                let hit = ctx.pick_at(position.x.max(0.0) as u32, position.y.max(0.0) as u32);
                if hit != self.hovered {
                    self.hovered = hit;
                    self.rebuild_geom();
                    ctx.request_redraw();
                }
            }
            Event::CursorLeft if self.hovered.take().is_some() => {
                self.rebuild_geom();
                ctx.request_redraw();
            }
            Event::MouseDown { button } => {
                println!("{button:?} press over {:?}", self.hovered);
            }
            Event::CloseRequested => ctx.exit(),
            _ => {}
        }
    }
}

impl Demo {
    /// Build the demo with `n` points scattered over the panel.
    fn new(n: usize) -> Self {
        let mut rng = Xorshift::new();
        let xs: Vec<f64> = (0..n)
            .map(|_| X_RANGE.0 + rng.unit() * (X_RANGE.1 - X_RANGE.0))
            .collect();
        let ys: Vec<f64> = (0..n)
            .map(|_| Y_RANGE.0 + rng.unit() * (Y_RANGE.1 - Y_RANGE.0))
            .collect();

        let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "time")
            .bind("y", "price");
        plot.set_title("Hover a point");
        plot.add_axis(Axis::rail(
            "time",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "price",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

        let view = PlotComposition::new(&comp())
            .add_scale("time", scale::continuous(X_RANGE.0..=X_RANGE.1))
            .add_scale("price", scale::continuous(Y_RANGE.0..=Y_RANGE.1))
            .with_plot(plot);

        let mut demo = Self {
            view,
            xs,
            ys,
            hovered: None,
        };
        demo.rebuild_geom();
        demo
    }

    /// Rebuild the point geom so the hovered row draws larger and in the
    /// highlight color.
    fn rebuild_geom(&mut self) {
        // Ids start at 1: `pick_id` 0 means "occlude but report no hit".
        let ids: Vec<f64> = (1..=self.xs.len()).map(|i| i as f64).collect();
        let sizes: Vec<f64> = ids
            .iter()
            .map(|id| {
                if Some(*id as u32) == self.hovered {
                    HOVER_SIZE
                } else {
                    BASE_SIZE
                }
            })
            .collect();
        let fills: Vec<Color> = ids
            .iter()
            .map(|id| {
                if Some(*id as u32) == self.hovered {
                    rgb8(220, 90, 70)
                } else {
                    rgb8(70, 120, 220)
                }
            })
            .collect();

        let xs = self.xs.clone();
        let ys = self.ys.clone();
        self.view.update_plot("panel", |plot| {
            let existing: Vec<_> = plot.geom_ids().collect();
            for id in existing {
                plot.remove_geom(id);
            }
            plot.add_geom(
                PointGeom::builder()
                    .set("x", xs)
                    .set("y", ys)
                    .set("fill", fills)
                    .set("size", sizes)
                    .set("pick_id", ids)
                    .build(),
            );
        });
    }
}

fn main() {
    let points = match std::env::args().nth(1) {
        Some(arg) => arg.parse().expect("point count must be a positive integer"),
        None => DEFAULT_POINTS,
    };
    println!("drawing {points} points; backend cap is {MAX_DRAW_INFO_WORDS} draws per scene");

    let config = WindowConfig::new("hephaestus — live plot")
        .size(900, 560)
        .background(rgb8(248, 248, 252))
        .picking(true);

    if let Err(err) = run(config, Demo::new(points)) {
        eprintln!("window: {err}");
        std::process::exit(1);
    }
}
