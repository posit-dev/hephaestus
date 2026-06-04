//! 2D triangle meshes with per-vertex colour.
//!
//! [`Mesh`] is a flat triangle list (`indices.len() % 3 == 0`); each
//! consecutive index triple defines one triangle. Vertices are in path
//! coordinates — typically panel-pixel space after the caller has
//! resolved pt → px and applied the panel transform. The `transform`
//! passed to [`SceneBuilder::draw_mesh`](crate::scene::SceneBuilder)
//! applies to vertex positions but not to colours.
//!
//! The mesh op is the foundation for Phase C's ribbon primitive (and
//! any future Voronoi / surface / heightmap rendering). No backend
//! currently has a native indexed-mesh primitive — every backend
//! decomposes a `Mesh` into its native draw ops (e.g. Vello emits one
//! `fill` per triangle with a linear-gradient brush).
//!
//! # Storage
//!
//! - [`Self::vertices`]: positions in path coordinates.
//! - [`Self::colors`]: same length as `vertices` — one colour per
//!   vertex. Per-vertex colour is the *whole point* of the mesh op; if
//!   every vertex has the same colour, a plain `fill` with a solid
//!   brush is cheaper.
//! - [`Self::indices`]: flat `u32` triples. `len() % 3 == 0`. Each
//!   triple `(i, j, k)` references vertices `vertices[i]`,
//!   `vertices[j]`, `vertices[k]` (and their matching colours).
//!
//! Construction validates the invariants and panics on violation —
//! mismatched array lengths or a non-multiple-of-3 index count
//! indicates a caller bug.

use crate::color::Color;
use crate::geometry::Point;

/// A 2D triangle list with per-vertex colour.
#[derive(Clone, Debug)]
pub struct Mesh {
    /// Vertex positions in path coordinates.
    pub vertices: Vec<Point>,
    /// One colour per vertex; same length as `vertices`.
    pub colors: Vec<Color>,
    /// Flat triangle index list (`len() % 3 == 0`).
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Construct a mesh. Panics if `vertices.len() != colors.len()` or
    /// `indices.len() % 3 != 0` or if any index references a vertex
    /// outside `0..vertices.len()`.
    pub fn new(vertices: Vec<Point>, colors: Vec<Color>, indices: Vec<u32>) -> Self {
        assert_eq!(
            vertices.len(),
            colors.len(),
            "Mesh::new: vertices and colors must have the same length \
             ({} vs {})",
            vertices.len(),
            colors.len(),
        );
        assert!(
            indices.len().is_multiple_of(3),
            "Mesh::new: indices length must be a multiple of 3, got {}",
            indices.len(),
        );
        let n = vertices.len();
        for (i, idx) in indices.iter().enumerate() {
            assert!(
                (*idx as usize) < n,
                "Mesh::new: indices[{i}] = {idx} out of bounds (vertices.len() = {n})",
            );
        }
        Self {
            vertices,
            colors,
            indices,
        }
    }

    /// Number of vertices in the mesh.
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Number of triangles in the mesh.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// `true` when the mesh has no triangles. (May still hold vertex
    /// data — but with no indices, nothing is rendered.)
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Axis-aligned bounding box of every vertex position. Returns
    /// `Rect::ZERO` when the mesh is empty.
    pub fn bounding_box(&self) -> kurbo::Rect {
        if self.vertices.is_empty() {
            return kurbo::Rect::ZERO;
        }
        let mut x0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y0 = f64::INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        for p in &self.vertices {
            if p.x < x0 {
                x0 = p.x;
            }
            if p.x > x1 {
                x1 = p.x;
            }
            if p.y < y0 {
                y0 = p.y;
            }
            if p.y > y1 {
                y1 = p.y;
            }
        }
        kurbo::Rect::new(x0, y0, x1, y1)
    }

    /// Iterate over triangles, yielding `([p0, p1, p2], [c0, c1, c2])`
    /// for each triple in the index list.
    pub fn iter_triangles(&self) -> impl Iterator<Item = ([Point; 3], [Color; 3])> + '_ {
        self.indices.chunks_exact(3).map(move |tri| {
            let i = tri[0] as usize;
            let j = tri[1] as usize;
            let k = tri[2] as usize;
            (
                [self.vertices[i], self.vertices[j], self.vertices[k]],
                [self.colors[i], self.colors[j], self.colors[k]],
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }
    fn green() -> Color {
        Color::new([0.0, 1.0, 0.0, 1.0])
    }
    fn blue() -> Color {
        Color::new([0.0, 0.0, 1.0, 1.0])
    }

    #[test]
    fn new_single_triangle() {
        let m = Mesh::new(
            vec![pt(0.0, 0.0), pt(10.0, 0.0), pt(0.0, 10.0)],
            vec![red(), green(), blue()],
            vec![0, 1, 2],
        );
        assert_eq!(m.vertex_count(), 3);
        assert_eq!(m.triangle_count(), 1);
        assert!(!m.is_empty());
    }

    #[test]
    fn bounding_box_spans_all_vertices() {
        let m = Mesh::new(
            vec![pt(-1.0, 2.0), pt(5.0, -3.0), pt(2.0, 8.0)],
            vec![red(); 3],
            vec![0, 1, 2],
        );
        let b = m.bounding_box();
        assert_eq!(b.x0, -1.0);
        assert_eq!(b.x1, 5.0);
        assert_eq!(b.y0, -3.0);
        assert_eq!(b.y1, 8.0);
    }

    #[test]
    fn empty_mesh_bounding_box_is_zero() {
        let m = Mesh::new(Vec::new(), Vec::new(), Vec::new());
        assert_eq!(m.bounding_box(), kurbo::Rect::ZERO);
        assert!(m.is_empty());
        assert_eq!(m.triangle_count(), 0);
    }

    #[test]
    fn iter_triangles_yields_per_vertex_data() {
        let m = Mesh::new(
            vec![pt(0.0, 0.0), pt(1.0, 0.0), pt(0.0, 1.0), pt(1.0, 1.0)],
            vec![red(), green(), blue(), red()],
            vec![0, 1, 2, 1, 3, 2],
        );
        let tris: Vec<_> = m.iter_triangles().collect();
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0].0[0], pt(0.0, 0.0));
        assert_eq!(tris[0].1[1], green());
        assert_eq!(tris[1].0[2], pt(0.0, 1.0));
    }

    #[test]
    #[should_panic(expected = "must have the same length")]
    fn mismatched_lengths_panic() {
        let _ = Mesh::new(vec![pt(0.0, 0.0)], vec![red(), green()], vec![]);
    }

    #[test]
    #[should_panic(expected = "must be a multiple of 3")]
    fn non_multiple_of_three_indices_panic() {
        let _ = Mesh::new(vec![pt(0.0, 0.0); 3], vec![red(); 3], vec![0, 1]);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn out_of_bounds_index_panics() {
        let _ = Mesh::new(vec![pt(0.0, 0.0); 3], vec![red(); 3], vec![0, 1, 5]);
    }
}
