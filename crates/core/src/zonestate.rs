//! P3.5 — zero-shot zone-state classifier (EXPERIMENTAL, best-effort). Watch a
//! named zone and classify a **binary** state (garage door open vs closed, gate
//! open/closed, pool cover on/off) from two CLIP text prompts — no training, no
//! new model. This module is the pure, unit-testable core; the pipeline drives
//! it (see `pipeline.rs`), reusing the single shared CLIP session so a
//! state-watched zone adds one occasional image embedding, not a second model.
//!
//! ## v0 scope (deliberately narrow — stated here and in the UI)
//!   - Exactly **two** fixed emitted labels, `zone_open` / `zone_closed`. Users
//!     distinguish instances (garage vs gate vs pool) by the zone name via an
//!     alarm rule's `zone_like`, NOT arbitrary custom state names.
//!   - One binary state per zone. No live prompt-preview endpoint.
//!   - Classified only on **detection-enabled** cameras (it piggybacks the frame
//!     the detect loop already fetches). A state-classify zone therefore needs
//!     its camera's detection turned on.
//!   - Silently no-ops when the CLIP models are absent — never a fake event.
//!
//! It is a scene-*reading* aid, not a security sensor: a low-contrast frame, an
//! occluding object, or an ambiguous prompt pair yields no reading rather than a
//! wrong one, and the pipeline debounces (two consecutive same readings) before
//! it flips the known state.

/// Cosine-margin by which one prompt must beat the other for a confident reading.
/// CLIP text↔image cosines are small and the two prompts differ by ~one word, so
/// their scores against a given crop sit close together; a reading below this
/// margin is treated as ambiguous (`None`) so the state never flaps on a borderline
/// frame. Deliberately small (v0) — likely needs live tuning, like the prompt-rule
/// [`crate::pipeline`] `PROMPT_FIRE_COSINE`.
pub const STATE_MARGIN: f32 = 0.01;

/// Axis-aligned pixel bounding box of a 0..1 polygon zone, clamped to the frame.
///
/// Returns `(x, y, w, h)` in pixels (top-left origin), or `None` when the polygon
/// is degenerate (< 3 vertices) or its clamped box has zero width/height — so the
/// caller never tries to crop an empty region. Pure so it can be unit-tested.
pub fn zone_pixel_bbox(points: &[[f32; 2]], fw: f32, fh: f32) -> Option<(u32, u32, u32, u32)> {
    if points.len() < 3 || fw < 1.0 || fh < 1.0 {
        return None;
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for p in points {
        min_x = min_x.min(p[0]);
        max_x = max_x.max(p[0]);
        min_y = min_y.min(p[1]);
        max_y = max_y.max(p[1]);
    }
    // Clamp to the frame in fractions, then convert to pixels (floor the top-left,
    // ceil the bottom-right so a thin zone still yields at least a 1px box).
    let x0 = (min_x.clamp(0.0, 1.0) * fw).floor().max(0.0);
    let y0 = (min_y.clamp(0.0, 1.0) * fh).floor().max(0.0);
    let x1 = (max_x.clamp(0.0, 1.0) * fw).ceil().max(0.0);
    let y1 = (max_y.clamp(0.0, 1.0) * fh).ceil().max(0.0);
    let x = x0 as u32;
    let y = y0 as u32;
    let w = ((x1 - x0).max(0.0) as u32).min((fw as u32).saturating_sub(x));
    let h = ((y1 - y0).max(0.0) as u32).min((fh as u32).saturating_sub(y));
    if w == 0 || h == 0 {
        return None;
    }
    Some((x, y, w, h))
}

/// Classify a zone crop as open (`Some(true)`) or closed (`Some(false)`) from two
/// prompt embeddings, or `None` when the two prompts score within [`STATE_MARGIN`]
/// of each other (ambiguous frame — don't guess). All three embeddings must be the
/// L2-normalized CLIP vectors [`crate::smart`] produces; a length mismatch (e.g. a
/// stale/short embedding) scores 0 via [`crate::smart::cosine`], collapsing the
/// margin to 0 → `None` (fail-safe, never a wrong reading). Pure + unit-tested.
pub fn classify_state(crop_emb: &[f32], open_emb: &[f32], closed_emb: &[f32]) -> Option<bool> {
    let open = crate::smart::cosine(crop_emb, open_emb);
    let closed = crate::smart::cosine(crop_emb, closed_emb);
    let diff = open - closed;
    if diff.abs() < STATE_MARGIN {
        None
    } else {
        Some(diff > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_normal_rectangle() {
        // A centred quarter-frame square on a 100×100 frame → (25,25,50,50).
        let pts = [[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]];
        let (x, y, w, h) = zone_pixel_bbox(&pts, 100.0, 100.0).expect("valid box");
        assert_eq!((x, y, w, h), (25, 25, 50, 50));
    }

    #[test]
    fn bbox_clamps_out_of_range_points_into_the_frame() {
        // Points spilling past [0,1] must clamp to the frame, never overflow it.
        let pts = [[-0.5, -0.2], [1.5, 0.5], [0.5, 1.9]];
        let (x, y, w, h) = zone_pixel_bbox(&pts, 200.0, 100.0).expect("valid box");
        assert!(x < 200 && y < 100, "origin inside frame");
        assert!(x + w <= 200, "width stays in frame: {x}+{w}");
        assert!(y + h <= 100, "height stays in frame: {y}+{h}");
        // Clamped box should span the full frame here.
        assert_eq!((x, y, w, h), (0, 0, 200, 100));
    }

    #[test]
    fn bbox_degenerate_returns_none() {
        // Fewer than 3 vertices has no area.
        assert_eq!(
            zone_pixel_bbox(&[[0.1, 0.1], [0.2, 0.2]], 100.0, 100.0),
            None
        );
        // A collapsed polygon (all vertices on one integer-aligned point) → 0px.
        let dot = [[0.5, 0.5], [0.5, 0.5], [0.5, 0.5]];
        assert_eq!(zone_pixel_bbox(&dot, 100.0, 100.0), None);
        // A zero-size frame is rejected too.
        assert_eq!(
            zone_pixel_bbox(&[[0.1, 0.1], [0.9, 0.1], [0.5, 0.9]], 0.0, 0.0),
            None
        );
    }

    #[test]
    fn classify_picks_the_nearer_prompt() {
        // crop aligns with the "open" prompt → Some(true).
        let crop = [1.0, 0.0];
        let open = [1.0, 0.0];
        let closed = [0.0, 1.0];
        assert_eq!(classify_state(&crop, &open, &closed), Some(true));
        // crop aligns with the "closed" prompt → Some(false).
        let crop = [0.0, 1.0];
        assert_eq!(classify_state(&crop, &open, &closed), Some(false));
    }

    #[test]
    fn classify_ambiguous_frame_returns_none() {
        // Equidistant from both prompts (45°) → within margin → no reading.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let crop = [s, s];
        let open = [1.0, 0.0];
        let closed = [0.0, 1.0];
        assert_eq!(classify_state(&crop, &open, &closed), None);
    }

    #[test]
    fn classify_length_mismatch_is_ambiguous_not_a_guess() {
        // A stale/short embedding scores 0 on both sides → margin 0 → None.
        let crop = [1.0, 0.0, 0.0];
        let open = [1.0, 0.0];
        let closed = [0.0, 1.0];
        assert_eq!(classify_state(&crop, &open, &closed), None);
    }
}
