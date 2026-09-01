//! Meshes as `ShadingType 4` free-form Gouraud triangle shadings.
//!
//! This is the one backend that draws [`Mesh`] natively rather than
//! through [`backend::mesh::decompose`](crate::backend::mesh) — the
//! shared decomposition exists because no rasterizing backend has an
//! indexed-mesh primitive, and everything in it is a workaround for
//! that. A Type 4 shading needs none of it: adjacent triangles are
//! interpolated inside one shading object with no antialiased edge
//! between them, and a triangle with three distinct colors is exactly
//! what Gouraud shading is.

use super::fnv1a;
use super::res::{ResKind, Resources};
use crate::mesh::Mesh;

/// Which channel a vertex record carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// Three `DeviceRGB` bytes — what the mesh paints.
    Rgb,
    /// One `DeviceGray` byte holding the vertex's alpha, for the soft
    /// mask that carries what the colour shading cannot.
    Alpha,
}

/// Intern `mesh` as a shading and return the name that refers to it,
/// or `None` when there is nothing to paint.
pub(crate) fn intern(mesh: &Mesh, res: &mut Resources) -> Option<String> {
    let (dict, payload) = vertex_stream(mesh, Channel::Rgb)?;
    let key = format!("mesh:{dict}|{}|{}", fnv1a(&payload), payload.len());
    Some(res.intern_stream(ResKind::Shading, &key, &dict, payload, None))
}

/// The shading dictionary and vertex records for `mesh` in `channel`.
///
/// Emits triangles in `mesh.indices` order and never reorders them:
/// overlapping triangles in a Type 4 shading paint in stream order, and
/// `primitives::ribbon` relies on that to let a self-intersecting
/// polyline's tail occlude its head.
fn vertex_stream(mesh: &Mesh, channel: Channel) -> Option<(String, Vec<u8>)> {
    if mesh.indices.is_empty() {
        return None;
    }
    let b = mesh.bounding_box();
    // A zero-width decode range would divide by zero on the way in and
    // give a viewer nothing to invert on the way out.
    let (x0, x1) = span(b.x0, b.x1);
    let (y0, y1) = span(b.y0, b.y1);

    // Ten or twelve bytes per vertex: one flag byte, two 32-bit
    // coordinates and one or three channel bytes, all byte-aligned at
    // these bit widths.
    let stride = if channel == Channel::Rgb { 12 } else { 10 };
    let mut payload = Vec::with_capacity(mesh.indices.len() * stride);
    for i in &mesh.indices {
        let v = mesh.vertices[*i as usize];
        let c = mesh.colors[*i as usize];
        // Flag 0 begins a new triangle, and the two vertices after it
        // carry 0 as well — a plain triangle list needs no other value.
        payload.push(0u8);
        payload.extend_from_slice(&quantize(v.x, x0, x1).to_be_bytes());
        payload.extend_from_slice(&quantize(v.y, y0, y1).to_be_bytes());
        let byte = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
        match channel {
            Channel::Rgb => {
                for ch in &c.components[..3] {
                    payload.push(byte(*ch));
                }
            }
            Channel::Alpha => payload.push(byte(c.components[3])),
        }
    }

    let mut dict = format!(
        "/ShadingType 4 /ColorSpace {} /BitsPerCoordinate 32 \
         /BitsPerComponent 8 /BitsPerFlag 8 /Decode [",
        match channel {
            Channel::Rgb => "/DeviceRGB",
            Channel::Alpha => "/DeviceGray",
        }
    );
    for v in [x0, x1, y0, y1] {
        super::writer::num(&mut dict, v, 6);
        dict.push(' ');
    }
    dict.push_str(match channel {
        Channel::Rgb => "0 1 0 1 0 1]",
        Channel::Alpha => "0 1]",
    });
    Some((dict, payload))
}

/// One alpha for a whole mesh, and the soft mask that carries it when
/// the vertices disagree.
///
/// A Type 4 shading carries color and no alpha, but a *second* Type 4
/// shading in `DeviceGray` carries the alpha ramp exactly: the same
/// triangles, the same interpolation, one gray byte per vertex instead
/// of three color ones.
pub(crate) fn alpha(
    mesh: &Mesh,
    matrix: crate::geometry::Affine,
    page: crate::geometry::Rect,
    res: &mut Resources,
    decimals: u8,
) -> (Option<f32>, Option<String>) {
    let stops: Vec<(f32, crate::color::Color)> = mesh.colors.iter().map(|c| (0.0, *c)).collect();
    if stops.is_empty() {
        return (None, None);
    }
    if let Some(a) = super::paint::uniform_alpha(&stops) {
        return ((a < 1.0).then_some(a), None);
    }
    let Some((dict, payload)) = vertex_stream(mesh, Channel::Alpha) else {
        return (None, None);
    };
    let key = format!("meshmask:{dict}|{}|{}", fnv1a(&payload), payload.len());
    let shading = res.intern_stream(ResKind::Shading, &key, &dict, payload, None);
    // The mask is set in default user space, so its content carries
    // the whole way from mesh space to there — the same composition the
    // `cm` before the colour `sh` makes, so the two land on top of each
    // other.
    let mut content = String::new();
    super::content::write_placement(&mut content, matrix, decimals);
    content.push_str(&format!("/{shading} sh\n"));
    let mask = super::paint::mask_form(content.into_bytes(), page, res, decimals);
    (None, Some(mask))
}

/// A non-degenerate `[min, max]` for one axis of a `/Decode` array.
fn span(lo: f64, hi: f64) -> (f64, f64) {
    if hi > lo {
        (lo, hi)
    } else {
        (lo, lo + 1.0)
    }
}

/// A coordinate as the 32-bit sample its `/Decode` range inverts.
fn quantize(v: f64, lo: f64, hi: f64) -> u32 {
    let t = (v - lo) / (hi - lo);
    (t * f64::from(u32::MAX))
        .round()
        .clamp(0.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::geometry::{Affine, Point, Rect};

    fn page() -> Rect {
        Rect::new(0.0, 0.0, 450.0, 150.0)
    }

    fn tri() -> Mesh {
        Mesh::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(0.0, 10.0),
            ],
            vec![
                Color::from_rgba8(255, 0, 0, 255),
                Color::from_rgba8(0, 255, 0, 255),
                Color::from_rgba8(0, 0, 255, 255),
            ],
            vec![0, 1, 2],
        )
    }

    #[test]
    fn a_triangle_interns_as_a_shading() {
        let mut res = Resources::default();
        let name = intern(&tri(), &mut res).unwrap();
        assert!(name.starts_with("Sh"), "{name}");
    }

    #[test]
    fn an_empty_mesh_paints_nothing() {
        let mut res = Resources::default();
        let empty = Mesh::new(Vec::new(), Vec::new(), Vec::new());
        assert!(intern(&empty, &mut res).is_none());
    }

    #[test]
    fn a_degenerate_axis_gets_a_usable_decode_range() {
        assert_eq!(span(5.0, 5.0), (5.0, 6.0));
        assert_eq!(span(1.0, 4.0), (1.0, 4.0));
    }

    #[test]
    fn quantization_spans_the_whole_range() {
        assert_eq!(quantize(0.0, 0.0, 1.0), 0);
        assert_eq!(quantize(1.0, 0.0, 1.0), u32::MAX);
    }

    fn varying() -> Mesh {
        Mesh::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 0.0),
                Point::new(0.0, 1.0),
            ],
            vec![
                Color::from_rgba8(0, 0, 0, 0),
                Color::from_rgba8(0, 0, 0, 255),
                Color::from_rgba8(0, 0, 0, 255),
            ],
            vec![0, 1, 2],
        )
    }

    #[test]
    fn vertices_agreeing_about_alpha_need_only_a_constant() {
        let mut res = Resources::default();
        let m = Mesh::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 0.0),
                Point::new(0.0, 1.0),
            ],
            vec![Color::from_rgba8(0, 0, 0, 128); 3],
            vec![0, 1, 2],
        );
        let (a, mask) = alpha(&m, Affine::IDENTITY, page(), &mut res, 3);
        assert!((a.unwrap() - 128.0 / 255.0).abs() < 1e-6);
        assert!(mask.is_none(), "a constant alpha needs no soft mask");
    }

    /// The gap this backend used to have: a ribbon whose opacity
    /// encodes something would print at a flat mid alpha.
    #[test]
    fn vertices_disagreeing_about_alpha_get_a_gray_shading_behind_a_soft_mask() {
        let mut res = Resources::default();
        let (a, mask) = alpha(&varying(), Affine::IDENTITY, page(), &mut res, 3);
        assert!(a.is_none(), "the mask carries it, not a constant");
        let mask = mask.expect("a soft mask");
        assert!(mask.contains("/S /Luminosity"), "{mask}");
        assert!(mask.contains("/BC [0]"), "{mask}");
    }

    /// A gray record drops the two colour bytes the RGB one carries.
    #[test]
    fn an_alpha_vertex_record_is_ten_bytes_to_the_colour_ones_twelve() {
        let (_, rgb) = vertex_stream(&varying(), Channel::Rgb).unwrap();
        let (dict, gray) = vertex_stream(&varying(), Channel::Alpha).unwrap();
        assert_eq!(rgb.len(), 3 * 12);
        assert_eq!(gray.len(), 3 * 10);
        assert!(dict.contains("/ColorSpace /DeviceGray"), "{dict}");
        assert!(dict.ends_with("0 1]"), "one decode pair, not three: {dict}");
        // The alpha byte is the vertex's own, in vertex order.
        assert_eq!(gray[9], 0, "the first vertex is transparent");
        assert_eq!(gray[19], 255, "the second is opaque");
    }

    /// Both shadings have to sample the same geometry, or the mask
    /// slides off the colour it is masking.
    #[test]
    fn the_colour_and_alpha_streams_share_a_decode_range() {
        let (rgb, _) = vertex_stream(&varying(), Channel::Rgb).unwrap();
        let (gray, _) = vertex_stream(&varying(), Channel::Alpha).unwrap();
        let cut = |d: &str| d[d.find("/Decode [").unwrap()..].to_string();
        assert!(cut(&rgb).starts_with(&cut(&gray)[.."/Decode [0 1 0 1 ".len()]));
    }
}
