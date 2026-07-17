//! Stage 1 of two-stage detection: a cheap pixel-diff motion gate.
//!
//! Frames are downscaled to a small grayscale thumbnail and compared to the
//! previous frame. If the fraction of meaningfully-changed pixels crosses a
//! threshold, the frame is worth sending to the (expensive) AI detector.
//! Never run YOLO on every frame of every camera — that's the whole point.

use image::{imageops::FilterType, DynamicImage, GrayImage};

/// Thumbnail edge used for diffing. Small enough to be ~free, large enough to
/// catch a person-sized object in a 4K frame. [`MotionGate::motion_regions`]
/// reports at this resolution (as 0..1 frame fractions).
const DIFF_SIZE: u32 = 64;

/// Side of the square diff grid that [`MotionGate::packed_mask`] indexes —
/// public so consumers can map region coordinates onto mask bits.
pub const GRID: u32 = DIFF_SIZE;

/// Per-pixel luma delta (0-255) below which a change is treated as noise.
const PIXEL_NOISE_FLOOR: u8 = 25;

/// A connected blob of changed cells smaller than this is dropped as noise when
/// computing [`MotionGate::motion_regions`] (a single flickering cell is not a
/// real moving object).
const MIN_REGION_CELLS: usize = 2;

/// Cap on the number of motion regions returned, largest first — keeps a busy
/// frame (rain, foliage) from drawing dozens of boxes.
const MAX_REGIONS: usize = 8;

/// Result of feeding one frame to the gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// First frame ever seen — nothing to compare against.
    Baseline,
    /// Below threshold: don't bother running AI.
    Still { changed: f32 },
    /// At/above threshold: run AI on this frame.
    Motion { changed: f32 },
}

impl Verdict {
    pub fn is_motion(&self) -> bool {
        matches!(self, Verdict::Motion { .. })
    }
}

/// Stateful per-camera motion gate.
pub struct MotionGate {
    prev: Option<GrayImage>,
    /// Fraction of pixels (0..1) that must change to call it motion.
    threshold: f32,
    /// Changed-cell mask from the most recent [`MotionGate::update`], row-major
    /// over the `DIFF_SIZE × DIFF_SIZE` thumbnail (`true` = changed past the
    /// noise floor). Empty until the first diff. Drives [`motion_regions`] so the
    /// snapshot can highlight *where* the motion that fired detection actually was.
    mask: Vec<bool>,
}

impl MotionGate {
    pub fn new(threshold: f32) -> Self {
        Self {
            prev: None,
            threshold,
            mask: Vec::new(),
        }
    }

    /// Feed the next frame; returns whether it differs enough from the last one.
    pub fn update(&mut self, frame: &DynamicImage) -> Verdict {
        let thumb = frame
            .resize_exact(DIFF_SIZE, DIFF_SIZE, FilterType::Triangle)
            .to_luma8();

        let verdict = match &self.prev {
            None => {
                self.mask.clear();
                Verdict::Baseline
            }
            Some(prev) => {
                let total = (DIFF_SIZE * DIFF_SIZE) as f32;
                self.mask.clear();
                self.mask.extend(
                    prev.pixels()
                        .zip(thumb.pixels())
                        .map(|(a, b)| a.0[0].abs_diff(b.0[0]) > PIXEL_NOISE_FLOOR),
                );
                let changed = self.mask.iter().filter(|&&c| c).count() as f32 / total;
                if changed >= self.threshold {
                    Verdict::Motion { changed }
                } else {
                    Verdict::Still { changed }
                }
            }
        };

        self.prev = Some(thumb);
        verdict
    }

    /// The changed-cell mask from the most recent [`MotionGate::update`], packed
    /// row-major into `DIFF_SIZE²/8` bytes (bit i = cell i changed). `None` when
    /// the last frame was a baseline or fewer than 2 cells changed (single-cell
    /// flicker is sensor noise, not motion worth indexing). This feeds the
    /// retroactive region-motion index: OR these per minute and you can later ask
    /// "was there ever motion inside this rectangle?" without re-decoding video.
    pub fn packed_mask(&self) -> Option<[u8; (DIFF_SIZE * DIFF_SIZE / 8) as usize]> {
        let n = (DIFF_SIZE * DIFF_SIZE) as usize;
        if self.mask.len() != n || self.mask.iter().filter(|&&c| c).count() < 2 {
            return None;
        }
        let mut out = [0u8; (DIFF_SIZE * DIFF_SIZE / 8) as usize];
        for (i, &c) in self.mask.iter().enumerate() {
            if c {
                out[i / 8] |= 1 << (i % 8);
            }
        }
        Some(out)
    }

    /// Bounding boxes (0..1 frame fractions) of the connected blobs of changed
    /// cells from the most recent [`MotionGate::update`], largest first and
    /// capped at [`MAX_REGIONS`]. Single-cell noise blobs are dropped. Empty when
    /// the last frame was a baseline or had no meaningful change. The caller burns
    /// these onto the snapshot so a viewer can see what tripped the gate.
    pub fn motion_regions(&self) -> Vec<[f32; 4]> {
        let n = DIFF_SIZE as usize;
        if self.mask.len() != n * n {
            return Vec::new();
        }
        // 4-connectivity flood fill: label each unvisited changed cell's blob and
        // accumulate its cell bounding box.
        let mut visited = vec![false; self.mask.len()];
        let mut stack: Vec<usize> = Vec::new();
        // (min_x, min_y, max_x, max_y, cells)
        let mut regions: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
        for start in 0..self.mask.len() {
            if !self.mask[start] || visited[start] {
                continue;
            }
            let (mut minx, mut miny, mut maxx, mut maxy, mut cells) =
                (n, n, 0usize, 0usize, 0usize);
            stack.push(start);
            visited[start] = true;
            while let Some(idx) = stack.pop() {
                let (x, y) = (idx % n, idx / n);
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
                cells += 1;
                // 4-connected neighbors, inlined (a closure here would need to
                // borrow both `stack` and `visited` mutably alongside the loop).
                let mut neigh = [None; 4];
                if x > 0 {
                    neigh[0] = Some(idx - 1);
                }
                if x + 1 < n {
                    neigh[1] = Some(idx + 1);
                }
                if y > 0 {
                    neigh[2] = Some(idx - n);
                }
                if y + 1 < n {
                    neigh[3] = Some(idx + n);
                }
                for ni in neigh.into_iter().flatten() {
                    if self.mask[ni] && !visited[ni] {
                        visited[ni] = true;
                        stack.push(ni);
                    }
                }
            }
            if cells >= MIN_REGION_CELLS {
                regions.push((minx, miny, maxx, maxy, cells));
            }
        }
        // Largest blobs first, then map cell boxes to frame fractions. A cell at
        // index x spans [x, x+1) of the grid, so the box runs to (max+1)/n.
        regions.sort_by_key(|r| std::cmp::Reverse(r.4));
        regions.truncate(MAX_REGIONS);
        let d = DIFF_SIZE as f32;
        regions
            .into_iter()
            .map(|(minx, miny, maxx, maxy, _)| {
                [
                    minx as f32 / d,
                    miny as f32 / d,
                    (maxx + 1) as f32 / d,
                    (maxy + 1) as f32 / d,
                ]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn solid(rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(128, 128, Rgb(rgb)))
    }

    #[test]
    fn first_frame_is_baseline() {
        let mut gate = MotionGate::new(0.02);
        assert_eq!(gate.update(&solid([0, 0, 0])), Verdict::Baseline);
    }

    #[test]
    fn identical_frames_are_still() {
        let mut gate = MotionGate::new(0.02);
        gate.update(&solid([10, 10, 10]));
        assert!(!gate.update(&solid([10, 10, 10])).is_motion());
    }

    #[test]
    fn full_frame_change_is_motion() {
        let mut gate = MotionGate::new(0.02);
        gate.update(&solid([0, 0, 0]));
        assert!(gate.update(&solid([255, 255, 255])).is_motion());
    }

    #[test]
    fn small_noise_is_not_motion() {
        let mut gate = MotionGate::new(0.02);
        gate.update(&solid([100, 100, 100]));
        // 10 luma levels of uniform drift — below the per-pixel noise floor.
        assert!(!gate.update(&solid([110, 110, 110])).is_motion());
    }

    #[test]
    fn localized_change_crosses_threshold() {
        let mut gate = MotionGate::new(0.02);
        let mut img = RgbImage::from_pixel(128, 128, Rgb([0, 0, 0]));
        gate.update(&DynamicImage::ImageRgb8(img.clone()));
        // Paint a bright 32x32 block (~6% of the 128x128 frame).
        for y in 0..32 {
            for x in 0..32 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        assert!(gate.update(&DynamicImage::ImageRgb8(img)).is_motion());
    }

    #[test]
    fn baseline_frame_has_no_regions() {
        let mut gate = MotionGate::new(0.02);
        gate.update(&solid([0, 0, 0]));
        assert!(gate.motion_regions().is_empty());
    }

    #[test]
    fn localized_change_yields_one_region_over_that_corner() {
        let mut gate = MotionGate::new(0.02);
        let mut img = RgbImage::from_pixel(128, 128, Rgb([0, 0, 0]));
        gate.update(&DynamicImage::ImageRgb8(img.clone()));
        // Bright top-left quadrant (the upper-left ~quarter of the frame).
        for y in 0..64 {
            for x in 0..64 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        gate.update(&DynamicImage::ImageRgb8(img));
        let regions = gate.motion_regions();
        assert_eq!(regions.len(), 1, "one connected blob");
        let [x1, y1, x2, y2] = regions[0];
        // The blob sits in the top-left and covers roughly the upper-left quarter.
        assert!(x1 < 0.1 && y1 < 0.1, "anchored at the top-left corner");
        assert!(
            x2 > 0.4 && x2 <= 0.6 && y2 > 0.4 && y2 <= 0.6,
            "extends to about the frame center, got x2={x2} y2={y2}"
        );
    }

    #[test]
    fn two_separated_changes_yield_two_regions() {
        let mut gate = MotionGate::new(0.0);
        let mut img = RgbImage::from_pixel(128, 128, Rgb([0, 0, 0]));
        gate.update(&DynamicImage::ImageRgb8(img.clone()));
        // Two well-separated bright blocks: top-left and bottom-right corners.
        for y in 0..24 {
            for x in 0..24 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
                img.put_pixel(127 - x, 127 - y, Rgb([255, 255, 255]));
            }
        }
        gate.update(&DynamicImage::ImageRgb8(img));
        assert_eq!(gate.motion_regions().len(), 2, "two disjoint blobs");
    }

    #[test]
    fn packed_mask_maps_changed_corner_to_low_bits() {
        let mut gate = MotionGate::new(0.0);
        let mut img = RgbImage::from_pixel(128, 128, Rgb([0, 0, 0]));
        assert!(gate.packed_mask().is_none(), "baseline has no mask");
        gate.update(&DynamicImage::ImageRgb8(img.clone()));
        // Brighten the top-left 32x32 quarter-quadrant -> cells in rows 0..~16,
        // cols 0..~16 of the 64x64 grid.
        for y in 0..32 {
            for x in 0..32 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        gate.update(&DynamicImage::ImageRgb8(img));
        let mask = gate.packed_mask().expect("mask after change");
        // Cell (0,0) = bit 0 set; a cell in the untouched bottom-right is not.
        assert_eq!(mask[0] & 1, 1, "top-left cell changed");
        let far = (63 * 64 + 63) as usize;
        assert_eq!(
            mask[far / 8] & (1 << (far % 8)),
            0,
            "bottom-right untouched"
        );
    }
}
