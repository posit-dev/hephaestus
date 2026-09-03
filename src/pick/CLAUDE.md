# src/pick/CLAUDE.md

Hit testing: what a drawing records about itself, and how a point, rectangle
or lasso is answered against it.

## What this module does

A scene wrapped in [`PickIndexScene`] forwards every call to the scene beneath
it unchanged and records each primitive's geometry on the way past. The result
is a [`PickIndex`] — entries in draw order, an R-tree over their bounding
boxes, and the clip and scope stacks each entry was drawn under. Queries run
entirely on the CPU: no second rasterisation, no readback, and no way for the
answer to describe a different frame from the one on screen.

Nothing here knows what a chart is. A [`PickScope`] carries a `&'static str`
kind plus an optional name and index; the vocabulary that gives those meaning
lives in `plot::pick`. That split is deliberate and matches the one
`composition` already uses between `Slot::name` and the `Region` trait — see
the layering rule in `src/CLAUDE.md`.

## Files

- `mod.rs` — `PickId`, `raw_id`, and the module docs stating the known limits.
- `scene.rs` — `PickIndexScene<S>`: the owning decorator that *is* each
  renderer's `Renderer::Scene`.
- `index.rs` — `PickIndex`, `Entry`, `Hit`. Recording and query orchestration.
- `rtree.rs` — the packed Hilbert R-tree.
- `hilbert.rs` — the Hilbert d-index the tree sorts on.
- `clip.rs` — the clip stack, and the axis-aligned-rect recogniser both it and
  `index.rs` use.
- `geom.rs` — per-primitive hit geometry: the shared arena, interning,
  flattening, chunking, and the exact tests.
- `scope.rs` — `PickScope`, `ScopeMode`, the hash-consed `ScopeTree`, and the
  `PickPath` view over it.

## Things worth knowing before changing this

- **Geometry is stored in the primitive's own frame, and the query point is
  inverse-transformed.** That is the whole reason a hundred thousand scatter
  markers cost one stored path: the plot layer draws them all from one
  `ShapeRegistry` entry, varying only the transform. Storing geometry in
  device space would defeat interning entirely.

- **Paths live in one flat `Vec<PathEl>` arena, not a `Vec<BezPath>`.** kurbo
  implements `Shape` for `&[PathEl]`, so slices are hit-tested directly. A
  low-level caller placing every mark in absolute coordinates shares nothing,
  and a `BezPath` per mark would be a hundred thousand allocations.

- **A stored path's bounds are cached with it.** A tight box around a cubic
  means solving for the curve's extrema; computing it per *mark* rather than
  per distinct *shape* was worth ~6 ms at 100k marks.

- **The intern map uses a pass-through hasher.** Its key is already a content
  hash, so the default SipHash would be a second hash over a good one.

- **Chunking at 64 points is what keeps a long primitive from degenerating.**
  A ten-thousand-point line is one primitive with a panel-sized bounding box;
  unchunked, every hover inside the panel would walk the whole polyline.
  Fills are *not* chunked — winding needs the whole ring.

- **Clips only subtract, which is why they are nearly free.** A clip's bounds
  are intersected into every entry's box at insert, so a primitive clipped
  away entirely is never recorded. The exact test runs only for a candidate
  that already passed its own geometry test, and only once per distinct clip
  per query — the memo is what stops a panel clip being evaluated a hundred
  thousand times. `is_rect` is a fast path, not a limitation: an arbitrary
  Bézier clip is tested exactly.

- **The tree is built lazily and invalidated on insert.** A window redrawing
  faster than it is queried never builds one. Below `LINEAR_SCAN_MAX` no
  levels are built at all and the query scans — which is also the shape a
  future off-thread build would degrade to while the tree is in flight.

- **`RefCell` guards the lazy tree and the query scratch buffers**, because
  queries take `&self`. Recording takes `&mut self`, so it uses `get_mut` and
  pays no borrow check. `PickIndex` is `Send` but not `Sync`; there is a test
  asserting the former, because it is what any off-thread build would need.

- **Entry order is draw order**, so an entry's index *is* its z-order and
  nothing is sorted at insert. A query sorts its candidates descending.

- **Hits coalesce on `(pick_id, scope)`.** A fill-plus-stroke mark, a chunked
  stroke and a chunked mesh are several entries a caller should see as one
  thing; the first kept is topmost and fixes the order, and the rest only
  widen the reported bounds.

## Cross-references

- `src/CLAUDE.md` — the authoritative picking model: the indexing rule, the
  scope grammar, what it costs, and the known limits.
- `src/plot/pick.rs` — the chart vocabulary: `PlotPart`, the scope
  constructors, and the typed `PlotPath` view a consumer reads a hit through.
- `src/backend/svg/CLAUDE.md` — the one backend that surfaces both `PickId`
  and scopes, as `data-pick-id` and `<g data-pick-kind=…>`.
