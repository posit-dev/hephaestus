//! Re-breaking and vertical stacking.
//!
//! [`stack_blocks`] is the single margin-collapse walk both the
//! initial shaping pass and [`RichTextRun::set_max_width`] run, so a
//! re-broken run stacks exactly the way a freshly shaped one does.

use parley::AlignmentOptions;

use super::image::slice_object_layouts;
use super::run::{BlockLayout, RichTextRun};
use super::shape::{hal_to_alignment, shape_block_layout, slice_block};
use crate::style_vocab::HAlign;

// ─── Vertical spacing ───────────────────────────────────────────────────────

/// One box's contribution to the vertical space on one side of a leaf.
///
/// Marquee follows CSS margin collapsing: adjacent margins merge
/// (two positives take the max, two negatives the most negative, a
/// mixed pair sums), and anything drawn between them stops the merge.
/// `barrier` is that "anything drawn": padding, a background, or a
/// border edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EdgeSpacing {
    /// The box's margin on this side (px). Collapses with neighbours.
    pub(crate) margin_px: f32,
    /// True when the box paints or pads on this side, which stops the
    /// margins on either side of it from collapsing together.
    pub(crate) barrier: bool,
    /// The box's padding on this side (px). Always adds space.
    pub(crate) padding_px: f32,
}

/// Running state for the collapse walk down a stack of blocks.
struct MarginAccumulator {
    /// Largest positive margin seen since the last barrier.
    pending_pos: f32,
    /// Most negative margin seen since the last barrier.
    pending_neg: f32,
    /// True until the first commit, while the collapse run still
    /// reaches the document's top edge. See [`MarginAccumulator::flush`].
    at_document_edge: bool,
}

impl MarginAccumulator {
    /// A fresh walk, positioned at the document's top edge.
    fn at_document_top() -> Self {
        Self {
            pending_pos: 0.0,
            pending_neg: 0.0,
            at_document_edge: true,
        }
    }

    fn fold(&mut self, margin_px: f32) {
        if margin_px >= 0.0 {
            self.pending_pos = self.pending_pos.max(margin_px);
        } else {
            self.pending_neg = self.pending_neg.min(margin_px);
        }
    }

    /// Commit the collapsed margin into `y` and start a fresh run.
    ///
    /// A run that reaches the document's top edge has collapsed out of
    /// the document — CSS puts it outside the body box, so it is
    /// dropped rather than committed. The first commit is by
    /// construction the one at that edge: anything before it has no
    /// barrier or content above it.
    fn flush(&mut self, y: &mut f32) {
        if self.at_document_edge {
            self.at_document_edge = false;
        } else {
            *y += self.pending_pos + self.pending_neg;
        }
        self.pending_pos = 0.0;
        self.pending_neg = 0.0;
    }
}

/// Walk one leaf's top chain: for each box, its margin joins the
/// collapse run, then a barrier commits the run and adds the box's
/// padding.
fn apply_top_chain(chain: &[EdgeSpacing], y: &mut f32, acc: &mut MarginAccumulator) {
    for e in chain {
        acc.fold(e.margin_px);
        if e.barrier {
            acc.flush(y);
            *y += e.padding_px;
        }
    }
}

/// Walk one leaf's bottom chain. Mirror image of [`apply_top_chain`]:
/// padding sits inside the box, so it lands before the box's own
/// margin joins the run.
fn apply_bottom_chain(chain: &[EdgeSpacing], y: &mut f32, acc: &mut MarginAccumulator) {
    for e in chain {
        if e.barrier {
            acc.flush(y);
            *y += e.padding_px;
        }
        acc.fold(e.margin_px);
    }
}

/// Position every block vertically, collapsing margins across the
/// whole stack. Returns the total height — a tight box around the
/// document, excluding the margins that collapse out through its top
/// and bottom edges.
///
/// Marquee calls that exclusion `force_body_margin` and turns it on
/// for every label-like use (`geom_marquee`, `element_marquee`). It
/// matters because the paragraph style carries a bottom margin: a
/// one-paragraph label whose box absorbed it would hang a blank line
/// below the text, and every caller anchoring on the box bottom would
/// place the label higher than the same string shaped as plain text.
pub(crate) fn stack_blocks(blocks: &mut [BlockLayout]) -> f32 {
    let mut y: f32 = 0.0;
    let mut acc = MarginAccumulator::at_document_top();
    for bl in blocks.iter_mut() {
        apply_top_chain(&bl.top_chain, &mut y, &mut acc);
        // The block's own content is itself a barrier — a margin
        // above it can't collapse with one below it.
        acc.flush(&mut y);
        bl.y_px = y;
        y += bl.height_px;
        apply_bottom_chain(&bl.bottom_chain, &mut y, &mut acc);
    }
    // Whatever is still pending reaches the document's bottom edge and
    // collapses out of the box, so it is dropped — the mirror of the
    // top-edge rule in `MarginAccumulator::flush`.
    y
}

impl RichTextRun {
    /// Re-break every block at the given outer width, propagating the
    /// wrap constraint into each block's effective shape width
    /// (`outer - left - right - max(first_line, continuation)`).
    /// Returns the new stacked total height.
    pub fn set_max_width(&self, max_width_px: f32, alignment: HAlign) -> f32 {
        // The layout solver probes the same width repeatedly while it
        // converges, and the draw pass asks once more for the width it
        // settled on. Re-breaking every block for a request we already
        // answered is pure waste.
        if *self.last_break.borrow() == Some((max_width_px, alignment)) {
            return *self.current_height_px.borrow();
        }
        let mut blocks = self.blocks.borrow_mut();
        for bl in blocks.iter_mut() {
            let block_avail = (max_width_px - bl.left_px - bl.right_inset_px).max(1.0);
            // Fallback for blocks without an `align` override uses
            // the caller-supplied HAlign resolved against THIS block's
            // resolved direction — so an Rtl block sees Start map to
            // physical Right even when the caller passed HAlign::Start.
            let effective_align = bl
                .alignment_override
                .unwrap_or_else(|| hal_to_alignment(alignment, bl.is_rtl));
            if bl.first_line_shift_px == bl.continuation_shift_px {
                // Symmetric — single layout at (block - shift).
                let target = (block_avail - bl.first_line_shift_px).max(1.0);
                if bl.source_objects.iter().any(|o| o.block) {
                    // A block image fills the column, and its box is
                    // baked into the layout the shaper built — so a
                    // new width means re-shaping, not re-breaking.
                    for object in bl.source_objects.iter_mut() {
                        object.resize_to_block(target);
                    }
                    let mut relaid = shape_block_layout(
                        &bl.source_text,
                        &bl.source_inlines,
                        &bl.source_objects,
                        &self.base_style,
                        self.base_brush,
                        &self.palette,
                        self.dpi,
                    );
                    relaid.break_all_lines(Some(target));
                    relaid.align(effective_align, AlignmentOptions::default());
                    bl.layout = relaid;
                } else {
                    bl.layout.break_all_lines(Some(target));
                    bl.layout
                        .align(effective_align, AlignmentOptions::default());
                }
                bl.shape_width_px = target;
                bl.height_px = bl.layout.height();
                bl.continuation_layout = None;
                bl.continuation_baseline_shifts.clear();
                bl.continuation_inlines.clear();
                bl.continuation_text.clear();
                bl.continuation_links.clear();
                bl.objects = bl.source_objects.clone();
                bl.continuation_objects.clear();
                bl.first_line_height_px = 0.0;
            } else {
                // Asymmetric — two-layout dance so both first-line
                // and continuation lines reach the right edge.
                let usable_first = (block_avail - bl.first_line_shift_px).max(1.0);
                let usable_cont = (block_avail - bl.continuation_shift_px).max(1.0);
                // Re-shape from cached source at first-line's usable
                // width. Parley may have wrapped natural single-line
                // shape; re-shaping produces a fresh line-break.
                for object in bl.source_objects.iter_mut() {
                    object.resize_to_block(usable_first);
                }
                let mut first_layout = shape_block_layout(
                    &bl.source_text,
                    &bl.source_inlines,
                    &bl.source_objects,
                    &self.base_style,
                    self.base_brush,
                    &self.palette,
                    self.dpi,
                );
                first_layout.break_all_lines(Some(usable_first));
                first_layout.align(effective_align, AlignmentOptions::default());
                // Find first line's byte end. If content fits on the
                // first line, no continuation needed.
                let first_line_end = first_layout
                    .lines()
                    .next()
                    .map(|l| l.text_range().end)
                    .unwrap_or(bl.source_text.len());
                let first_line_height = first_layout
                    .lines()
                    .next()
                    .map(|l| l.metrics().line_height)
                    .unwrap_or(0.0);
                if first_line_end >= bl.source_text.len() {
                    // Single line — no continuation.
                    bl.layout = first_layout;
                    bl.shape_width_px = usable_first;
                    bl.height_px = bl.layout.height();
                    bl.continuation_layout = None;
                    bl.continuation_baseline_shifts.clear();
                    bl.continuation_inlines.clear();
                    bl.continuation_text.clear();
                    bl.continuation_links.clear();
                    bl.objects = bl.source_objects.clone();
                    bl.continuation_objects.clear();
                    bl.first_line_height_px = 0.0;
                } else {
                    // Slice remaining text + rebase inline/baseline
                    // ranges to (start = 0 at first_line_end).
                    let (rest_text, rest_inlines, rest_baselines) = slice_block(
                        &bl.source_text,
                        &bl.source_inlines,
                        &bl.source_baselines,
                        &(first_line_end..bl.source_text.len()),
                    );
                    let rest_objects = slice_object_layouts(
                        &bl.source_objects,
                        &(first_line_end..bl.source_text.len()),
                    );
                    let mut cont_layout = shape_block_layout(
                        &rest_text,
                        &rest_inlines,
                        &rest_objects,
                        &self.base_style,
                        self.base_brush,
                        &self.palette,
                        self.dpi,
                    );
                    cont_layout.break_all_lines(Some(usable_cont));
                    cont_layout.align(effective_align, AlignmentOptions::default());
                    let cont_height = cont_layout.height();
                    bl.layout = first_layout;
                    bl.continuation_layout = Some(cont_layout);
                    bl.continuation_baseline_shifts = rest_baselines;
                    bl.continuation_inlines = rest_inlines;
                    bl.continuation_text = rest_text.clone();
                    bl.continuation_links = super::shape::slice_links(
                        &bl.source_links,
                        &(first_line_end..bl.source_text.len()),
                    );
                    bl.objects = slice_object_layouts(&bl.source_objects, &(0..first_line_end));
                    bl.continuation_objects = rest_objects;
                    bl.first_line_height_px = first_line_height;
                    // Shape width for paint: use the wider of the
                    // two so the outer_rect matches the CONTINUATION
                    // (which fills the block's right edge).
                    bl.shape_width_px = usable_cont;
                    bl.height_px = first_line_height + cont_height;
                }
            }
        }
        let total_height = stack_blocks(&mut blocks);
        *self.current_height_px.borrow_mut() = total_height;
        *self.last_break.borrow_mut() = Some((max_width_px, alignment));
        // The blocks moved, so anything derived from their positions
        // has to be recomputed on next use.
        *self.derived.borrow_mut() = None;
        total_height
    }
}
