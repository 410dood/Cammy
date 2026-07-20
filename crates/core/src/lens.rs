//! Near-lens obstruction suppressor — the perennial "insect / spider / cobweb on
//! the lens at night" false alarm that every NVR community complains about
//! (Frigate discussions #11017 "cobwebs tricking Frigate into thinking my sofa is
//! 82% human" and #17882, ipcamtalk "Spiders Bugs and Alarms", Reolink false-alarm
//! guides). Under IR illumination a bug or web sitting *on the glass* reads as a
//! large, uniformly bright, low-texture blob, which the object model happily
//! hallucinates into a "person" / "animal" — and it happens at night, exactly when
//! monitoring matters. No mainstream product handles it well.
//!
//! Signature of a near-lens obstruction, all of which must hold: (1) large — the
//! crop fills a big fraction of the frame (it's right on the lens); (2) bright —
//! high mean luma (IR bloom / overexposure); (3) flat — very low luma variance (a
//! featureless blob; a real subject has internal texture and contrast even when
//! backlit).
//!
//! Deliberately conservative: it favours *missing* a suppression over hiding a
//! real event, and continuous packet-copy recording keeps the footage regardless
//! — so the worst case is one extra low-value alert, never lost footage. Pure and
//! unit-tested; the pipeline computes the crop's luma stats and calls in here.

/// Tunable thresholds. Defaults are intentionally strict — a legitimate subject
/// almost never fills ~half the frame *and* is near-white *and* is texture-free.
#[derive(Clone, Copy, Debug)]
pub struct ObstructionConfig {
    /// Crop area as a fraction of the whole frame must be at or above this.
    pub min_area_frac: f32,
    /// Crop mean luma (0..1) at or above this looks like IR bloom / overexposure.
    pub min_luma: f32,
    /// Crop luma variance at or below this is a featureless (real objects vary).
    pub max_var: f32,
}

impl Default for ObstructionConfig {
    fn default() -> Self {
        Self {
            min_area_frac: 0.45,
            min_luma: 0.80,
            max_var: 0.010,
        }
    }
}

/// Mean and variance of a luma thumbnail (values in 0..1). Mirrors the tamper
/// gate's per-frame stats, applied to a detection crop instead of the whole frame.
pub fn luma_stats(thumb: &[f32]) -> (f32, f32) {
    if thumb.is_empty() {
        return (0.0, 0.0);
    }
    let mean = thumb.iter().sum::<f32>() / thumb.len() as f32;
    let var = thumb.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / thumb.len() as f32;
    (mean, var)
}

/// Is a detection whose crop has this area fraction and luma stats a near-lens
/// obstruction (bug / web) rather than a real object? All three gates must hold.
pub fn is_obstruction(area_frac: f32, mean_luma: f32, luma_var: f32, cfg: &ObstructionConfig) -> bool {
    area_frac >= cfg.min_area_frac && mean_luma >= cfg.min_luma && luma_var <= cfg.max_var
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bright_flat_large_blob_is_an_obstruction() {
        let cfg = ObstructionConfig::default();
        // A spider on the glass under IR: fills half the frame, near-white, uniform.
        let thumb = vec![0.92f32; 256];
        let (mean, var) = luma_stats(&thumb);
        assert!(is_obstruction(0.55, mean, var, &cfg));
    }

    #[test]
    fn textured_subject_is_not_suppressed() {
        let cfg = ObstructionConfig::default();
        // A real (even large, even bright-ish) subject has internal contrast —
        // alternating light/dark pixels give high variance.
        let thumb: Vec<f32> = (0..256).map(|i| if i % 2 == 0 { 0.85 } else { 0.25 }).collect();
        let (mean, var) = luma_stats(&thumb);
        assert!(!is_obstruction(0.6, mean, var, &cfg), "textured crop must survive");
    }

    #[test]
    fn small_bright_blob_is_not_suppressed() {
        let cfg = ObstructionConfig::default();
        // A distant light / reflection: bright and flat but small — a real thing
        // far away, not on the lens.
        let thumb = vec![0.95f32; 256];
        let (mean, var) = luma_stats(&thumb);
        assert!(!is_obstruction(0.10, mean, var, &cfg));
    }

    #[test]
    fn dark_large_blob_is_not_suppressed() {
        let cfg = ObstructionConfig::default();
        // A close, dark object (a person's coat against the lens) — large and flat
        // but NOT bright, so it isn't IR bloom.
        let thumb = vec![0.15f32; 256];
        let (mean, var) = luma_stats(&thumb);
        assert!(!is_obstruction(0.7, mean, var, &cfg));
    }

    #[test]
    fn luma_stats_empty_is_zero() {
        assert_eq!(luma_stats(&[]), (0.0, 0.0));
    }
}
