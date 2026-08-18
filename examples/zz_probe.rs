use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::{Renderer, Size};

struct Xorshift(u64);
impl Xorshift {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

const W: u32 = 900;
const H: u32 = 560;

fn main() {
    let n: usize = std::env::var("N").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let size: f64 = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(7.0);
    let mut rng = Xorshift(0x2545_f491_4f6c_dd1d);
    let xs: Vec<f64> = (0..n).map(|_| rng.unit() * 100.0).collect();
    let ys: Vec<f64> = (0..n).map(|_| 40.0 + rng.unit() * 50.0).collect();
    let ids: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    let fills: Vec<Color> = ids.iter().map(|_| rgb8(70, 120, 220)).collect();

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
    let mut plot = Plot::new(&comp(), "panel").bind("x", "time").bind("y", "price");
    plot.add_axis(Axis::rail("time", AxisPlacement::Cartesian(AxisSide::Bottom)));
    plot.add_axis(Axis::rail("price", AxisPlacement::Cartesian(AxisSide::Left)));
    plot.add_geom(PointGeom::builder().set("x", xs).set("y", ys).set("fill", fills).set("size", size).set("pick_id", ids).build());
    let mut view = PlotComposition::new(&comp())
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("price", scale::continuous(40.0..=90.0))
        .with_plot(plot);

    let mut renderer = VelloRenderer::new().expect("renderer");
    view.render(renderer.scene(), Size::new(W as f64, H as f64), 96.0);

    let est = renderer.scene().raw().bump_estimate(None);
    let mut buf = vec![0u8; (W * H * 4) as usize];
    renderer.render_to_buffer(W, H, rgb8(248, 248, 252), &mut buf).expect("render");
    // A skipped fine stage leaves the target texture untouched: all zeroes.
    let blank = buf.chunks(4).all(|p| p == [0, 0, 0, 0]);
    println!(
        "N={n} size={size}: lines={} tiles={} seg_counts={} segments={} ptcl={} binning={} -> {}",
        est.lines.len(), est.tile.len(), est.seg_counts.len(), est.segments.len(), est.ptcl.len(), est.binning.len(),
        if blank { "BLANK" } else { "drew" }
    );
}
