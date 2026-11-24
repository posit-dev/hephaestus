//! Reproducer: with `theme.text.markdown = true`, a table with a rowspanned
//! cell loses its natural size — the rowspanned cell's column stretches to
//! fill the patch instead of sizing to content.
use hephaestus::backend::svg::{encode_svg, write_svg, SvgConfig, SvgScene};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{PlotComposition, Table, TableCell, TableCellKind};

fn table(composition: &hephaestus::composition::Composition, patch: &str) -> Table {
    use TableCellKind::*;
    let cell = |kind, row, col, text: &str| TableCell::new(kind, row, col, text);
    Table::new(composition, patch)
        .cells(vec![
            cell(ColumnLabel, 0, 0, ""),
            cell(ColumnLabel, 0, 1, "y"),
            cell(Stub { row: 0 }, 1, 0, "a").span(2, 1),
            cell(Body { row: 0 }, 1, 1, "10.5"),
            cell(Body { row: 1 }, 2, 1, "20.25"),
            cell(Stub { row: 2 }, 3, 0, "b").span(2, 1),
            cell(Body { row: 2 }, 3, 1, "30"),
            cell(Body { row: 3 }, 4, 1, "41.75"),
        ])
        .stub_column(0)
}

fn second_column_x(svg: &str) -> f64 {
    let el = svg
        .split("<text")
        .find(|el| el.contains(">y<"))
        .expect("no y header");
    for needle in ["transform=\"translate(", "<tspan x=\""] {
        if let Some(rest) = el.split(needle).nth(1) {
            return rest
                .split([' ', '"'])
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(f64::NAN);
        }
    }
    f64::NAN
}

fn main() {
    let comp = || beside(Patch::new("plain"), Patch::new("markdown"));
    let mut plain = PlotComposition::new(&comp()).with_table(table(&comp(), "plain"));
    let mut theme = Theme::default();
    theme.text.markdown = Some(true);
    let mut markdown = PlotComposition::new(&comp())
        .theme(theme)
        .with_table(table(&comp(), "markdown"));
    let size = Size::new(800.0, 200.0);
    for (name, view) in [("plain", &mut plain), ("markdown", &mut markdown)] {
        let mut svg = SvgScene::with_config(size, 96.0, SvgConfig::new());
        view.render(&mut svg, size, 96.0);
        let out = encode_svg(&svg);
        println!(
            "{name}: the second column starts at x = {:.0} (its patch starts at 400)",
            second_column_x(&out)
        );
        write_svg(format!("examples/markdown_measure_{name}.svg"), &svg).unwrap();
    }
}
