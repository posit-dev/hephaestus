//! [`PickIndexScene`] — the scene wrapper that builds a [`PickIndex`] as a
//! drawing goes past.
//!
//! It is a [`SceneBuilder`] wrapping a [`SceneBuilder`], and each renderer's
//! `Renderer::Scene` is one of these, so picking is something a scene has
//! rather than something a backend implements. That is what lets a vector
//! backend, or a build with no renderer at all, hit-test the same way a GPU
//! one does.

use crate::geometry::Affine;

use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::{Point, Rect};
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::{Hit, PickId, PickIndex, PickScope};
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

/// A scene that records a hit index alongside whatever it draws.
///
/// Every call is forwarded to the wrapped scene unchanged, so the picture is
/// identical whether or not indexing is on. When `enabled` is false the
/// wrapper costs one predictable branch per draw call and records nothing.
#[derive(Debug)]
pub struct PickIndexScene<S> {
    inner: S,
    index: PickIndex,
    enabled: bool,
}

impl<S: SceneBuilder> PickIndexScene<S> {
    /// Wrap `inner`. `enabled` decides whether anything is indexed —
    /// filling the index is not free, so a host that never queries should
    /// pass `false`.
    pub fn new(inner: S, enabled: bool) -> Self {
        Self {
            inner,
            index: PickIndex::new(),
            enabled,
        }
    }

    /// The index built by the most recent drawing.
    pub fn index(&self) -> &PickIndex {
        &self.index
    }

    /// Whether draws are being indexed.
    pub fn indexes(&self) -> bool {
        self.enabled
    }

    /// Turn indexing on or off. Takes effect from the next [`Self::clear`];
    /// the index keeps answering from what it already holds until then.
    pub fn set_indexing(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Borrow the wrapped scene.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Borrow the wrapped scene mutably.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Unwrap, discarding the index.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Every hit at `p`, topmost first. See [`PickIndex::hits_at`].
    pub fn hits_at(&self, p: Point) -> Vec<Hit<'_>> {
        self.index.hits_at(p)
    }

    /// The topmost authoring id at `p`. See [`PickIndex::pick_at`].
    pub fn pick_at(&self, p: Point) -> Option<u32> {
        self.index.pick_at(p)
    }

    /// Every hit whose bounds intersect `rect`. See [`PickIndex::hits_in`].
    pub fn hits_in(&self, rect: Rect) -> Vec<Hit<'_>> {
        self.index.hits_in(rect)
    }

    /// Every hit entirely inside `rect`. See [`PickIndex::hits_within`].
    pub fn hits_within(&self, rect: Rect) -> Vec<Hit<'_>> {
        self.index.hits_within(rect)
    }

    /// Lasso selection. See [`PickIndex::hits_in_path`].
    pub fn hits_in_path(&self, path: &Path, rule: FillRule) -> Vec<Hit<'_>> {
        self.index.hits_in_path(path, rule)
    }
}

impl<S: SceneBuilder> SceneBuilder for PickIndexScene<S> {
    fn clear(&mut self) {
        self.inner.clear();
        self.index.clear();
    }

    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        if self.enabled {
            self.index.record_fill(rule, transform, path, pick_id);
        }
        self.inner
            .fill(rule, transform, brush, brush_transform, path, pick_id);
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        if self.enabled {
            self.index.record_stroke(stroke, transform, path, pick_id);
        }
        self.inner
            .stroke(stroke, transform, brush, brush_transform, path, pick_id);
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        if self.enabled {
            self.index.record_image(image, transform, pick_id);
        }
        self.inner
            .draw_image(image, transform, sampling, alpha, pick_id);
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        if self.enabled {
            self.index.record_glyphs(run, pick_id);
        }
        self.inner.draw_glyphs(run, pick_id);
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        if self.enabled {
            self.index.record_mesh(mesh, transform, pick_id);
        }
        self.inner.draw_mesh(mesh, transform, pick_id);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        if self.enabled {
            self.index.push_clip(transform, clip);
        }
        self.inner.push_layer(blend, alpha, transform, clip);
    }

    fn pop_layer(&mut self) {
        if self.enabled {
            self.index.pop_clip();
        }
        self.inner.pop_layer();
    }

    fn push_pick_scope(&mut self, scope: &PickScope) {
        if self.enabled {
            self.index.push_scope(scope);
        }
        self.inner.push_pick_scope(scope);
    }

    fn pop_pick_scope(&mut self) {
        if self.enabled {
            self.index.pop_scope();
        }
        self.inner.pop_pick_scope();
    }
}
