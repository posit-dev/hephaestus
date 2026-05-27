//! Per-row resolution helpers shared across geom impls.
//!
//! Every geom maps the same kind of raw `(Channel, Option<&Scale>, row_idx)`
//! triple to a typed visual output (color, pt size, dash pattern, etc.).
//! These helpers centralise that machinery so each geom's draw loop reads
//! as the geom-specific logic only.
//!
//! The helpers all share one principle: scale mapping is applied to the
//! raw `Value` *before* the typed extraction, so a `"size"` column of
//! categorical strings can flow through an ordinal scale to a numeric
//! output, an `"x"` column of dates can flow through a continuous scale
//! to a `[0, 1]` panel fraction, etc.

use std::sync::Arc;

use crate::color::Color;
use crate::plot::scale::Scale;
use crate::plot::value::Value;
use crate::stroke::{Cap, Join};

use super::Channel;

/// Convert pt to px at the given dpi. The same convention is used for
/// every absolute graphical size (point diameter, stroke linewidth,
/// dash lengths, dash offset).
#[inline]
pub(crate) fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

/// Project a row's raw `Value` through an optional position scale to a
/// `[0, 1]` panel fraction, with an optional band-fraction offset folded
/// in. With no scale the input must itself project to a finite f64
/// (numeric or temporal); other variants return `NaN` so the caller
/// skips the row. Without a scale, the band offset is ignored — "band"
/// is a scale-defined concept.
pub(crate) fn resolve_position(raw: Value, scale: Option<&Scale>, band_offset: f64) -> f64 {
    let mapped = match scale {
        Some(s) => s.map_with_offset(&raw, band_offset),
        None => raw,
    };
    mapped.as_number().unwrap_or(f64::NAN)
}

/// Read the raw `Value` at row `i` from a channel and run it through an
/// optional scale. Returns `None` if `channel` itself is `None` (channel
/// unset) — distinct from the scale producing `Value::Null`.
fn resolve_value(channel: Option<&Channel>, scale: Option<&Scale>, i: usize) -> Option<Value> {
    let raw = match channel? {
        Channel::Constant(v) => v.clone(),
        Channel::Data(col) => col.get(i),
    };
    let mapped = match scale {
        Some(s) => s.map(&raw),
        None => raw,
    };
    Some(mapped)
}

/// Resolve a colour channel. Returns `None` when unset or when the
/// resolved value isn't a colour. Used for `"fill"` / `"stroke"`.
pub(crate) fn resolve_color_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Option<Color> {
    resolve_value(channel, scale, i)?.as_color()
}

/// Resolve an optional numeric channel. Returns `None` when the channel
/// is unset or the resolved value isn't numeric; the caller decides
/// what absence means.
pub(crate) fn resolve_number_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Option<f64> {
    resolve_value(channel, scale, i)?.as_number()
}

/// Resolve a numeric channel with a fallback default. Equivalent to
/// `resolve_number_channel(...).unwrap_or(default)`.
pub(crate) fn resolve_number_channel_or(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: f64,
) -> f64 {
    resolve_number_channel(channel, scale, i).unwrap_or(default)
}

/// Resolve a linetype channel to an `Arc<[f64]>` of pt dash/gap lengths.
/// Falls back to solid (empty array) when the channel is unset or the
/// resolved value isn't a `Value::Linetype`.
pub(crate) fn resolve_linetype_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Arc<[f64]> {
    match resolve_value(channel, scale, i) {
        Some(Value::Linetype(p)) => p,
        _ => Arc::from(Vec::<f64>::new()),
    }
}

/// Resolve a string channel with a fallback `'static` default. Used by
/// shape-name lookups; returns a freshly-allocated `String` for matched
/// names.
pub(crate) fn resolve_str_channel_or(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: &'static str,
) -> String {
    match resolve_value(channel, scale, i).and_then(|v| v.as_str().map(str::to_owned)) {
        Some(s) => s,
        None => default.to_string(),
    }
}

/// Resolve a cap channel from a string-named value. Recognises `"butt"`
/// / `"round"` / `"square"`; falls back to `default` otherwise.
pub(crate) fn resolve_cap_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: Cap,
) -> Cap {
    let v = match resolve_value(channel, scale, i) {
        Some(v) => v,
        None => return default,
    };
    match v.as_str() {
        Some("butt") => Cap::Butt,
        Some("round") => Cap::Round,
        Some("square") => Cap::Square,
        _ => default,
    }
}

/// Resolve a join channel from a string-named value. Recognises
/// `"miter"` / `"round"` / `"bevel"`; falls back to `default` otherwise.
pub(crate) fn resolve_join_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: Join,
) -> Join {
    let v = match resolve_value(channel, scale, i) {
        Some(v) => v,
        None => return default,
    };
    match v.as_str() {
        Some("miter") => Join::Miter,
        Some("round") => Join::Round,
        Some("bevel") => Join::Bevel,
        _ => default,
    }
}

/// Override the alpha channel of `color` with `alpha` (in `0..=1`).
/// `None` color → `None`; `None` alpha → color unchanged.
pub(crate) fn override_alpha(color: Option<Color>, alpha: Option<f64>) -> Option<Color> {
    let c = color?;
    match alpha {
        None => Some(c),
        Some(a) => {
            let [r, g, b, _] = c.components;
            Some(Color::new([r, g, b, a as f32]))
        }
    }
}

/// Look up the band width (in `[0, 1]` panel fraction) for `raw` on
/// `scale`. Continuous scales return 0 (no bands → no contribution).
/// Discrete / Ordinal / Binned return the band width at the value.
/// Used by geoms that scale a dimension by band fraction (e.g.
/// WedgeGeom's `radius_x_band`).
pub(crate) fn band_width_at(scale: Option<&Scale>, raw: &Value) -> f64 {
    match scale {
        Some(s) => s.scale_type().band_width_at(s, raw),
        None => 0.0,
    }
}

/// Return the smallest non-zero value among two non-negative inputs.
/// Treats 0 as "this axis isn't banded" — picks the other axis. If both
/// are 0 (both continuous), returns 0.
///
/// Shared by geoms whose `*_band` channel scales a single dimension
/// against whichever discrete axis offers a band — `WedgeGeom::radius_band`
/// and `PointGeom::size_band`. The semantics match: both-discrete picks
/// the smaller band so the geom fits the cell on both axes;
/// single-discrete uses that axis's band; both-continuous drops the
/// band contribution.
#[inline]
pub(crate) fn smallest_nonzero(a: f64, b: f64) -> f64 {
    match (a > 0.0, b > 0.0) {
        (true, true) => a.min(b),
        (true, false) => a,
        (false, true) => b,
        (false, false) => 0.0,
    }
}
