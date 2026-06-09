//! Face recognition: SCRFD detection + ArcFace 512-d embeddings, both via
//! ONNX Runtime with the same per-OS GPU execution providers as the object
//! detector. Models: InsightFace "buffalo_l" pair (det_10g + w600k_r50).
//!
//! Identity matching is cosine similarity between L2-normalized embeddings;
//! same-person scores typically land 0.4-0.7, different people below ~0.3.

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, RgbImage};
use ort::session::Session;
use ort::value::Tensor;

/// SCRFD input edge (square letterbox).
const DET_SIZE: u32 = 640;
/// ArcFace input edge.
const REC_SIZE: u32 = 112;

/// The canonical ArcFace 112x112 landmark positions (left eye, right eye,
/// nose, left mouth, right mouth).
const ARCFACE_DST: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

#[derive(Clone, Debug, serde::Serialize)]
pub struct Face {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    /// 5 landmarks in original-image coordinates.
    pub landmarks: [[f32; 2]; 5],
}

pub struct FaceEngine {
    det: Session,
    rec: Session,
}

impl FaceEngine {
    pub fn new(det_path: &str, rec_path: &str, force_cpu: bool) -> Result<Self> {
        Ok(Self {
            det: detector::build_ort_session(det_path, force_cpu)?,
            rec: detector::build_ort_session(rec_path, force_cpu)?,
        })
    }

    /// Detect faces in an image (original-image coordinates).
    pub fn detect(&mut self, img: &DynamicImage, conf: f32) -> Result<Vec<Face>> {
        let (w, h) = img.dimensions();
        let scale = (DET_SIZE as f32 / w as f32).min(DET_SIZE as f32 / h as f32);
        let new_w = (w as f32 * scale).round() as u32;
        let new_h = (h as f32 * scale).round() as u32;
        let resized = img
            .resize_exact(new_w, new_h, image::imageops::FilterType::Triangle)
            .to_rgb8();

        // (x - 127.5) / 127.5, zero-padded canvas (no centering: SCRFD anchors
        // are absolute, easier to map back without padding offsets).
        let mut chw = vec![-1.0f32; (3 * DET_SIZE * DET_SIZE) as usize];
        let plane = (DET_SIZE * DET_SIZE) as usize;
        for (x, y, px) in resized.enumerate_pixels() {
            let idx = (y * DET_SIZE + x) as usize;
            chw[idx] = (px[0] as f32 - 127.5) / 127.5;
            chw[plane + idx] = (px[1] as f32 - 127.5) / 127.5;
            chw[2 * plane + idx] = (px[2] as f32 - 127.5) / 127.5;
        }
        let input = Tensor::from_array(([1usize, 3, DET_SIZE as usize, DET_SIZE as usize], chw))?;

        let outputs = self.det.run(ort::inputs!["input.1" => input])?;
        // det_10g emits 9 outputs ordered scores(s8,s16,s32), boxes(...), kps(...).
        let mut grabbed: Vec<(Vec<i64>, Vec<f32>)> = Vec::with_capacity(9);
        for (_name, value) in outputs.iter() {
            let (shape, data) = value.try_extract_tensor::<f32>()?;
            grabbed.push((shape.to_vec(), data.to_vec()));
        }
        anyhow::ensure!(grabbed.len() == 9, "unexpected SCRFD output count");

        let mut faces = Vec::new();
        for (i, stride) in [8u32, 16, 32].iter().enumerate() {
            let scores = &grabbed[i].1;
            let boxes = &grabbed[i + 3].1;
            let kps = &grabbed[i + 6].1;
            let cells = DET_SIZE / stride; // anchor grid edge
            for a in 0..scores.len() {
                let s = scores[a];
                if s < conf {
                    continue;
                }
                // 2 anchors per cell, row-major over the grid.
                let cell = (a / 2) as u32;
                let cx = (cell % cells) as f32 * *stride as f32;
                let cy = (cell / cells) as f32 * *stride as f32;
                let st = *stride as f32;
                let (l, t, r, b) = (
                    boxes[a * 4] * st,
                    boxes[a * 4 + 1] * st,
                    boxes[a * 4 + 2] * st,
                    boxes[a * 4 + 3] * st,
                );
                let mut lm = [[0.0f32; 2]; 5];
                for (k, point) in lm.iter_mut().enumerate() {
                    point[0] = (cx + kps[a * 10 + k * 2] * st) / scale;
                    point[1] = (cy + kps[a * 10 + k * 2 + 1] * st) / scale;
                }
                faces.push(Face {
                    x1: ((cx - l) / scale).max(0.0),
                    y1: ((cy - t) / scale).max(0.0),
                    x2: ((cx + r) / scale).min(w as f32),
                    y2: ((cy + b) / scale).min(h as f32),
                    score: s,
                    landmarks: lm,
                });
            }
        }
        Ok(nms(faces, 0.4))
    }

    /// 512-d L2-normalized identity embedding for a detected face.
    pub fn embed(&mut self, img: &DynamicImage, face: &Face) -> Result<Vec<f32>> {
        let aligned = align_face(&img.to_rgb8(), &face.landmarks);
        let mut chw = vec![0.0f32; (3 * REC_SIZE * REC_SIZE) as usize];
        let plane = (REC_SIZE * REC_SIZE) as usize;
        for (x, y, px) in aligned.enumerate_pixels() {
            let idx = (y * REC_SIZE + x) as usize;
            chw[idx] = (px[0] as f32 - 127.5) / 127.5;
            chw[plane + idx] = (px[1] as f32 - 127.5) / 127.5;
            chw[2 * plane + idx] = (px[2] as f32 - 127.5) / 127.5;
        }
        let input = Tensor::from_array(([1usize, 3, REC_SIZE as usize, REC_SIZE as usize], chw))?;
        let outputs = self.rec.run(ort::inputs!["input.1" => input])?;
        let (_name, value) = outputs.iter().next().context("no embedding output")?;
        let (_shape, data) = value.try_extract_tensor::<f32>()?;
        let norm = data.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        Ok(data.iter().map(|v| v / norm).collect())
    }
}

/// Cosine similarity of two L2-normalized embeddings.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Greedy IoU NMS.
fn nms(mut faces: Vec<Face>, iou_thresh: f32) -> Vec<Face> {
    faces.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Face> = Vec::new();
    'outer: for f in faces {
        for k in &keep {
            let ix1 = f.x1.max(k.x1);
            let iy1 = f.y1.max(k.y1);
            let ix2 = f.x2.min(k.x2);
            let iy2 = f.y2.min(k.y2);
            let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
            let area_f = (f.x2 - f.x1).max(0.0) * (f.y2 - f.y1).max(0.0);
            let area_k = (k.x2 - k.x1).max(0.0) * (k.y2 - k.y1).max(0.0);
            let union = area_f + area_k - inter;
            if union > 0.0 && inter / union > iou_thresh {
                continue 'outer;
            }
        }
        keep.push(f);
    }
    keep
}

/// Umeyama similarity transform (scale+rotation+translation) from the detected
/// landmarks to the canonical ArcFace layout, then inverse-warp a 112x112 crop.
fn align_face(img: &RgbImage, src: &[[f32; 2]; 5]) -> RgbImage {
    // Means.
    let (mut sx, mut sy, mut dx, mut dy) = (0.0f32, 0.0, 0.0, 0.0);
    for k in 0..5 {
        sx += src[k][0];
        sy += src[k][1];
        dx += ARCFACE_DST[k][0];
        dy += ARCFACE_DST[k][1];
    }
    let (sx, sy, dx, dy) = (sx / 5.0, sy / 5.0, dx / 5.0, dy / 5.0);

    // Covariance terms for the 2D similarity solution.
    let (mut a, mut b, mut var) = (0.0f32, 0.0, 0.0);
    for k in 0..5 {
        let (px, py) = (src[k][0] - sx, src[k][1] - sy);
        let (qx, qy) = (ARCFACE_DST[k][0] - dx, ARCFACE_DST[k][1] - dy);
        a += px * qx + py * qy;
        b += px * qy - py * qx;
        var += px * px + py * py;
    }
    let var = var.max(1e-6);
    let scale = (a * a + b * b).sqrt() / var;
    let theta = b.atan2(a);
    let (cos_t, sin_t) = (theta.cos() * scale, theta.sin() * scale);
    // Forward: q = R*(p - s_mean) + d_mean. Inverse map for sampling:
    // p = R^-1*(q - d_mean) + s_mean, where R^-1 has scale 1/scale.
    let inv_scale = 1.0 / (scale * scale);
    let (icos, isin) = (cos_t * inv_scale, sin_t * inv_scale);

    let mut out = RgbImage::new(REC_SIZE, REC_SIZE);
    let (w, h) = (img.width() as i64, img.height() as i64);
    for y in 0..REC_SIZE {
        for x in 0..REC_SIZE {
            let qx = x as f32 - dx;
            let qy = y as f32 - dy;
            let px = icos * qx + isin * qy + sx;
            let py = -isin * qx + icos * qy + sy;
            let (xi, yi) = (px as i64, py as i64);
            if xi >= 0 && xi < w && yi >= 0 && yi < h {
                out.put_pixel(x, y, *img.get_pixel(xi as u32, yi as u32));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_identical_unit_vectors_is_one() {
        let v = vec![0.6, 0.8];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
        let w = vec![0.8, -0.6];
        assert!(cosine(&v, &w).abs() < 1e-6); // orthogonal
    }

    #[test]
    fn nms_keeps_highest_and_distinct() {
        let f = |s: f32, x: f32| Face {
            x1: x,
            y1: 0.0,
            x2: x + 10.0,
            y2: 10.0,
            score: s,
            landmarks: [[0.0; 2]; 5],
        };
        let kept = nms(vec![f(0.9, 0.0), f(0.8, 1.0), f(0.7, 100.0)], 0.4);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9);
    }

    #[test]
    fn align_identity_when_landmarks_match_canon() {
        // If source landmarks are exactly the canonical layout, alignment is
        // the identity transform: pixel (50,50) must map to itself.
        let mut img = RgbImage::new(112, 112);
        img.put_pixel(50, 50, image::Rgb([255, 0, 0]));
        let out = align_face(&img, &ARCFACE_DST);
        assert_eq!(out.get_pixel(50, 50), &image::Rgb([255, 0, 0]));
    }
}
