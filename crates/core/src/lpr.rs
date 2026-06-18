//! License plate recognition: YOLOS plate detector (DETR-style decode) +
//! PaddleOCR PP-OCRv5 recognition with CTC decoding. Both optional downloads;
//! the feature sleeps until the three files exist (see README).
//!
//! Runs on CPU (quantized det model; the rec model is 7 MB) — invoked only on
//! vehicle events, which are already cooldown-throttled.

use anyhow::{Context, Result};
use detector::ort::session::Session;
use detector::ort::value::Tensor;
use image::DynamicImage;

pub const DET_MODEL: &str = "plate_det.onnx";
pub const REC_MODEL: &str = "plate_rec.onnx";
pub const DICT_FILE: &str = "plate_dict.txt";

/// ImageNet normalization (YOLOS/DETR preprocessing).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

pub fn models_present() -> bool {
    [DET_MODEL, REC_MODEL, DICT_FILE]
        .iter()
        .all(|p| std::path::Path::new(p).exists())
}

/// Where a read plate sits relative to the watch lists. Deny wins over allow.
/// Matching is case-insensitive substring (handles partial/uncertain OCR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateStatus {
    /// On the deny list — a "vehicle of interest".
    Deny,
    /// On the allow list — known/expected.
    Allow,
    /// Not on any list.
    Unlisted,
}

pub fn plate_status(plate: &str, allow: &[String], deny: &[String]) -> PlateStatus {
    let p = plate.to_uppercase();
    let hit = |list: &[String]| {
        list.iter()
            .any(|e| !e.trim().is_empty() && p.contains(e.trim().to_uppercase().as_str()))
    };
    if hit(deny) {
        PlateStatus::Deny
    } else if hit(allow) {
        PlateStatus::Allow
    } else {
        PlateStatus::Unlisted
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn deny_beats_allow_and_substring_matches() {
        let allow = vec!["ABC123".to_string()];
        let deny = vec!["xyz".to_string()];
        assert_eq!(plate_status("8XYZ99", &allow, &deny), PlateStatus::Deny);
        assert_eq!(plate_status("abc123", &allow, &deny), PlateStatus::Allow);
        assert_eq!(plate_status("QQQ000", &allow, &deny), PlateStatus::Unlisted);
        // Deny wins even if also on allow.
        assert_eq!(
            plate_status("ABC123", &["ABC123".to_string()], &["ABC".to_string()]),
            PlateStatus::Deny
        );
    }
}

#[derive(Clone, Debug)]
pub struct Plate {
    /// Absolute pixel box in the analyzed image.
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
}

pub struct PlateEngine {
    det: Session,
    rec: Session,
    dict: Vec<String>,
}

impl PlateEngine {
    pub fn try_new() -> Result<Self> {
        let dict: Vec<String> = std::fs::read_to_string(DICT_FILE)
            .context("reading plate dict")?
            .lines()
            .map(str::to_string)
            .collect();
        anyhow::ensure!(!dict.is_empty(), "empty plate dict");
        Ok(Self {
            det: detector::build_ort_session(DET_MODEL, true)?,
            rec: detector::build_ort_session(REC_MODEL, true)?,
            dict,
        })
    }

    /// Find the most confident license plate in the image (usually a vehicle
    /// crop). DETR decode: softmax over (classes + no-object); empirically the
    /// plate class of this export is index 1 (verified via debug_class_maxima
    /// on the training dataset: class 1 fires at 0.999, class 0 never).
    pub fn detect(&mut self, img: &DynamicImage, conf: f32) -> Result<Option<Plate>> {
        // ViT patches are 16px; round dims down to multiples of 16, cap 864.
        let (w, h) = (img.width(), img.height());
        let scale = (864.0 / w.max(h) as f32).min(1.0);
        let tw = (((w as f32 * scale) as u32) / 16 * 16).max(64);
        let th = (((h as f32 * scale) as u32) / 16 * 16).max(64);
        let rgb = img
            .resize_exact(tw, th, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let plane = (tw * th) as usize;
        let mut chw = vec![0.0f32; 3 * plane];
        for (x, y, px) in rgb.enumerate_pixels() {
            let idx = (y * tw + x) as usize;
            for c in 0..3 {
                chw[c * plane + idx] = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        let input = Tensor::from_array(([1usize, 3, th as usize, tw as usize], chw))?;
        let outputs = self
            .det
            .run(detector::ort::inputs!["pixel_values" => input])?;

        let (_n, logits_v) = outputs.iter().next().context("no logits")?;
        let (lshape, logits) = logits_v.try_extract_tensor::<f32>()?;
        let (_n2, boxes_v) = outputs.iter().nth(1).context("no boxes")?;
        let (_bshape, boxes) = boxes_v.try_extract_tensor::<f32>()?;

        let classes = lshape[2] as usize; // labels + no-object
        let queries = lshape[1] as usize;
        let mut best: Option<Plate> = None;
        for q in 0..queries {
            let row = &logits[q * classes..(q + 1) * classes];
            // Softmax; class 0 = license plate, last = no-object.
            let max = row.iter().cloned().fold(f32::MIN, f32::max);
            let exps: Vec<f32> = row.iter().map(|v| (v - max).exp()).collect();
            let sum: f32 = exps.iter().sum();
            let p_plate = exps[1] / sum;
            if p_plate < conf {
                continue;
            }
            let (cx, cy, bw, bh) = (
                boxes[q * 4],
                boxes[q * 4 + 1],
                boxes[q * 4 + 2],
                boxes[q * 4 + 3],
            );
            let plate = Plate {
                x1: ((cx - bw / 2.0) * w as f32).max(0.0),
                y1: ((cy - bh / 2.0) * h as f32).max(0.0),
                x2: ((cx + bw / 2.0) * w as f32).min(w as f32),
                y2: ((cy + bh / 2.0) * h as f32).min(h as f32),
                score: p_plate,
            };
            if best.as_ref().map(|b| plate.score > b.score).unwrap_or(true) {
                best = Some(plate);
            }
        }
        Ok(best)
    }

    /// Diagnostic: best softmax probability per class across all queries.
    #[doc(hidden)]
    pub fn debug_class_maxima(&mut self, img: &DynamicImage) -> Result<Vec<f32>> {
        let (tw, th, chw) = self.det_preprocess(img);
        let input = Tensor::from_array(([1usize, 3, th as usize, tw as usize], chw))?;
        let outputs = self
            .det
            .run(detector::ort::inputs!["pixel_values" => input])?;
        let (_n, logits_v) = outputs.iter().next().context("no logits")?;
        let (lshape, logits) = logits_v.try_extract_tensor::<f32>()?;
        let classes = lshape[2] as usize;
        let queries = lshape[1] as usize;
        let mut maxima = vec![0.0f32; classes];
        for q in 0..queries {
            let row = &logits[q * classes..(q + 1) * classes];
            let max = row.iter().cloned().fold(f32::MIN, f32::max);
            let exps: Vec<f32> = row.iter().map(|v| (v - max).exp()).collect();
            let sum: f32 = exps.iter().sum();
            for (c, e) in exps.iter().enumerate() {
                maxima[c] = maxima[c].max(e / sum);
            }
        }
        Ok(maxima)
    }

    fn det_preprocess(&self, img: &DynamicImage) -> (u32, u32, Vec<f32>) {
        let (w, h) = (img.width(), img.height());
        let scale = (864.0 / w.max(h) as f32).min(1.0);
        let tw = (((w as f32 * scale) as u32) / 16 * 16).max(64);
        let th = (((h as f32 * scale) as u32) / 16 * 16).max(64);
        let rgb = img
            .resize_exact(tw, th, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let plane = (tw * th) as usize;
        let mut chw = vec![0.0f32; 3 * plane];
        for (x, y, px) in rgb.enumerate_pixels() {
            let idx = (y * tw + x) as usize;
            for c in 0..3 {
                chw[c * plane + idx] = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        (tw, th, chw)
    }

    /// OCR a detected plate region: crop with margin, resize to height 48,
    /// PaddleOCR normalization, CTC-decode keeping plate-plausible characters.
    pub fn read(&mut self, img: &DynamicImage, plate: &Plate) -> Result<String> {
        let margin_x = (plate.x2 - plate.x1) * 0.06;
        let margin_y = (plate.y2 - plate.y1) * 0.15;
        let x = (plate.x1 - margin_x).max(0.0) as u32;
        let y = (plate.y1 - margin_y).max(0.0) as u32;
        let cw = ((plate.x2 - plate.x1 + margin_x * 2.0) as u32).min(img.width() - x);
        let ch = ((plate.y2 - plate.y1 + margin_y * 2.0) as u32).min(img.height() - y);
        anyhow::ensure!(cw >= 16 && ch >= 8, "plate crop too small");
        let crop = img.crop_imm(x, y, cw, ch);

        let rec_h = 48u32;
        let rec_w = ((cw as f32 * rec_h as f32 / ch as f32) as u32).clamp(32, 640);
        let rgb = crop
            .resize_exact(rec_w, rec_h, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let plane = (rec_w * rec_h) as usize;
        let mut chw = vec![0.0f32; 3 * plane];
        for (px_x, px_y, px) in rgb.enumerate_pixels() {
            let idx = (px_y * rec_w + px_x) as usize;
            for c in 0..3 {
                chw[c * plane + idx] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        let input = Tensor::from_array(([1usize, 3, rec_h as usize, rec_w as usize], chw))?;
        let outputs = self.rec.run(detector::ort::inputs!["x" => input])?;
        let (_n, value) = outputs.iter().next().context("no rec output")?;
        let (shape, data) = value.try_extract_tensor::<f32>()?;

        // CTC: argmax per step, collapse repeats, drop blank (index 0);
        // dict entries map indices 1..=len.
        let steps = shape[1] as usize;
        let classes = shape[2] as usize;
        let mut text = String::new();
        let mut prev = 0usize;
        for t in 0..steps {
            let row = &data[t * classes..(t + 1) * classes];
            let (idx, _) = row
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .expect("non-empty row");
            if idx != 0 && idx != prev {
                if let Some(ch) = self.dict.get(idx - 1) {
                    text.push_str(ch);
                }
            }
            prev = idx;
        }
        // Plates are alphanumeric; strip everything else and uppercase.
        let cleaned: String = text
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        Ok(cleaned)
    }
}
