//! YOLOv8 object detection as a library — the productized form of the
//! `spike-detect` Phase 0 spike (which remains the standalone CLI validation).
//!
//! One exported `.onnx` runs everywhere: DirectML on Windows, CoreML on macOS,
//! CUDA on Linux, CPU fallback. `Detector` owns the ONNX Runtime session and is
//! `Send`, so a service can park it in a worker thread per pipeline.

use anyhow::{Context, Result};
use image::GenericImageView;
use ort::execution_providers::ExecutionProvider;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

/// Square input size YOLOv8 expects.
const IMGSZ: u32 = 640;

/// One detected object, in original-image pixel coordinates.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Detection {
    pub label: &'static str,
    pub class: usize,
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// A loaded YOLOv8 model ready for inference.
pub struct Detector {
    session: Session,
    conf: f32,
    iou: f32,
}

/// Build an ONNX Runtime session with the best execution provider for this OS
/// (DirectML / CoreML / CUDA) and CPU fallback. Shared by every model we run
/// (YOLO objects, face detection, face embeddings).
pub fn build_ort_session(model_path: &str, force_cpu: bool) -> Result<Session> {
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(4)?;

    if !force_cpu {
        #[cfg(target_os = "windows")]
        {
            use ort::execution_providers::DirectMLExecutionProvider;
            let ep = DirectMLExecutionProvider::default();
            log_ep("DirectML", ep.is_available().unwrap_or(false));
            builder = builder.with_execution_providers([ep.build()])?;
        }
        #[cfg(target_os = "macos")]
        {
            use ort::execution_providers::CoreMLExecutionProvider;
            let ep = CoreMLExecutionProvider::default();
            log_ep("CoreML", ep.is_available().unwrap_or(false));
            builder = builder.with_execution_providers([ep.build()])?;
        }
        #[cfg(target_os = "linux")]
        {
            use ort::execution_providers::CUDAExecutionProvider;
            let ep = CUDAExecutionProvider::default();
            log_ep("CUDA", ep.is_available().unwrap_or(false));
            builder = builder.with_execution_providers([ep.build()])?;
        }
    }

    builder
        .commit_from_file(model_path)
        .with_context(|| format!("loading model {model_path}"))
}

impl Detector {
    /// Load the model with the best execution provider for this OS (or CPU when
    /// `force_cpu` is set). `conf` / `iou` are the confidence and NMS thresholds.
    pub fn new(model_path: &str, force_cpu: bool, conf: f32, iou: f32) -> Result<Self> {
        let session = build_ort_session(model_path, force_cpu)?;
        Ok(Self { session, conf, iou })
    }

    /// Run detection on one image. Returns boxes in original-image coordinates.
    pub fn detect(&mut self, img: &image::DynamicImage) -> Result<Vec<Detection>> {
        let (orig_w, orig_h) = img.dimensions();
        let (input, scale, pad_x, pad_y) = letterbox_to_tensor(img);

        let outputs = self
            .session
            .run(ort::inputs!["images" => input])
            .context("inference failed")?;
        let (_name, output) = outputs.iter().next().context("model produced no outputs")?;
        let (shape, data) = output
            .try_extract_tensor::<f32>()
            .context("output was not an f32 tensor")?;

        let dets = decode_yolov8(
            data,
            shape,
            self.conf,
            scale,
            pad_x,
            pad_y,
            orig_w as f32,
            orig_h as f32,
        );
        Ok(non_max_suppression(dets, self.iou))
    }
}

fn log_ep(name: &str, available: bool) {
    if available {
        tracing::info!("using GPU execution provider: {name}");
    } else {
        tracing::warn!("{name} not available at runtime; falling back to CPU");
    }
}

/// Resize an image into a 640x640 letterbox (preserve aspect, pad with gray)
/// and produce a [1,3,640,640] f32 NCHW tensor normalized to 0..1.
fn letterbox_to_tensor(img: &image::DynamicImage) -> (Tensor<f32>, f32, f32, f32) {
    let (w, h) = img.dimensions();
    let scale = (IMGSZ as f32 / w as f32).min(IMGSZ as f32 / h as f32);
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;
    let pad_x = (IMGSZ - new_w) as f32 / 2.0;
    let pad_y = (IMGSZ - new_h) as f32 / 2.0;

    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle);
    let resized = resized.to_rgb8();

    // Gray canvas (114/255 is the YOLO convention).
    let mut chw = vec![114.0f32 / 255.0; (3 * IMGSZ * IMGSZ) as usize];
    let plane = (IMGSZ * IMGSZ) as usize;
    for (x, y, px) in resized.enumerate_pixels() {
        let cx = x + pad_x as u32;
        let cy = y + pad_y as u32;
        let idx = (cy * IMGSZ + cx) as usize;
        chw[idx] = px[0] as f32 / 255.0;
        chw[plane + idx] = px[1] as f32 / 255.0;
        chw[2 * plane + idx] = px[2] as f32 / 255.0;
    }

    let tensor = Tensor::from_array(([1usize, 3, IMGSZ as usize, IMGSZ as usize], chw))
        .expect("failed to build input tensor");
    (tensor, scale, pad_x, pad_y)
}

/// Decode raw YOLOv8 output [1, 84, 8400] into detections in ORIGINAL image
/// coordinates. Layout: 84 = 4 box (cx,cy,w,h) + 80 class scores; 8400 anchors.
#[allow(clippy::too_many_arguments)]
fn decode_yolov8(
    data: &[f32],
    shape: &[i64],
    conf: f32,
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    orig_w: f32,
    orig_h: f32,
) -> Vec<Detection> {
    let features = shape[1] as usize; // 84
    let anchors = shape[2] as usize; // 8400
    let num_classes = features - 4;

    let at = |f: usize, a: usize| data[f * anchors + a];

    let mut dets = Vec::new();
    for a in 0..anchors {
        let mut best_c = 0usize;
        let mut best_s = 0.0f32;
        for c in 0..num_classes {
            let s = at(4 + c, a);
            if s > best_s {
                best_s = s;
                best_c = c;
            }
        }
        if best_s < conf {
            continue;
        }

        let cx = at(0, a);
        let cy = at(1, a);
        let bw = at(2, a);
        let bh = at(3, a);

        let x1 = ((cx - bw / 2.0) - pad_x) / scale;
        let y1 = ((cy - bh / 2.0) - pad_y) / scale;
        let x2 = ((cx + bw / 2.0) - pad_x) / scale;
        let y2 = ((cy + bh / 2.0) - pad_y) / scale;

        dets.push(Detection {
            label: coco_label(best_c),
            class: best_c,
            score: best_s,
            x1: x1.clamp(0.0, orig_w),
            y1: y1.clamp(0.0, orig_h),
            x2: x2.clamp(0.0, orig_w),
            y2: y2.clamp(0.0, orig_h),
        });
    }
    dets
}

/// Standard greedy non-max suppression, per class.
fn non_max_suppression(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Detection> = Vec::new();
    'outer: for d in dets {
        for k in &keep {
            if k.class == d.class && iou(&d, k) > iou_thresh {
                continue 'outer;
            }
        }
        keep.push(d);
    }
    keep
}

fn iou(a: &Detection, b: &Detection) -> f32 {
    let ix1 = a.x1.max(b.x1);
    let iy1 = a.y1.max(b.y1);
    let ix2 = a.x2.min(b.x2);
    let iy2 = a.y2.min(b.y2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = (a.x2 - a.x1).max(0.0) * (a.y2 - a.y1).max(0.0);
    let area_b = (b.x2 - b.x1).max(0.0) * (b.y2 - b.y1).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// COCO 80-class labels (YOLOv8 default training set).
pub fn coco_label(i: usize) -> &'static str {
    const LABELS: [&str; 80] = [
        "person",
        "bicycle",
        "car",
        "motorcycle",
        "airplane",
        "bus",
        "train",
        "truck",
        "boat",
        "traffic light",
        "fire hydrant",
        "stop sign",
        "parking meter",
        "bench",
        "bird",
        "cat",
        "dog",
        "horse",
        "sheep",
        "cow",
        "elephant",
        "bear",
        "zebra",
        "giraffe",
        "backpack",
        "umbrella",
        "handbag",
        "tie",
        "suitcase",
        "frisbee",
        "skis",
        "snowboard",
        "sports ball",
        "kite",
        "baseball bat",
        "baseball glove",
        "skateboard",
        "surfboard",
        "tennis racket",
        "bottle",
        "wine glass",
        "cup",
        "fork",
        "knife",
        "spoon",
        "bowl",
        "banana",
        "apple",
        "sandwich",
        "orange",
        "broccoli",
        "carrot",
        "hot dog",
        "pizza",
        "donut",
        "cake",
        "chair",
        "couch",
        "potted plant",
        "bed",
        "dining table",
        "toilet",
        "tv",
        "laptop",
        "mouse",
        "remote",
        "keyboard",
        "cell phone",
        "microwave",
        "oven",
        "toaster",
        "sink",
        "refrigerator",
        "book",
        "clock",
        "vase",
        "scissors",
        "teddy bear",
        "hair drier",
        "toothbrush",
    ];
    LABELS.get(i).copied().unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(class: usize, score: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> Detection {
        Detection {
            label: coco_label(class),
            class,
            score,
            x1,
            y1,
            x2,
            y2,
        }
    }

    #[test]
    fn iou_identical_boxes_is_one() {
        let a = det(0, 0.9, 0.0, 0.0, 10.0, 10.0);
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_boxes_is_zero() {
        let a = det(0, 0.9, 0.0, 0.0, 10.0, 10.0);
        let b = det(0, 0.9, 20.0, 20.0, 30.0, 30.0);
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn nms_suppresses_overlapping_same_class() {
        let dets = vec![
            det(0, 0.9, 0.0, 0.0, 10.0, 10.0),
            det(0, 0.8, 1.0, 1.0, 11.0, 11.0), // heavy overlap, same class -> dropped
            det(2, 0.7, 1.0, 1.0, 11.0, 11.0), // different class -> kept
        ];
        let kept = non_max_suppression(dets, 0.45);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9);
        assert_eq!(kept[1].class, 2);
    }

    #[test]
    fn decode_maps_letterbox_back_to_original_coords() {
        // One anchor, one class, perfect score. 84-feature layout collapsed to 5.
        // shape [1, 5, 1]: box cx,cy,w,h = (320, 320, 100, 100) in 640-space.
        let data = vec![320.0, 320.0, 100.0, 100.0, 0.99];
        let shape = [1i64, 5, 1];
        // Original image 1280x640 -> scale 0.5, pad_y = (640 - 320)/2 = 160.
        let dets = decode_yolov8(&data, &shape, 0.5, 0.5, 0.0, 160.0, 1280.0, 640.0);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert!((d.x1 - 540.0).abs() < 1e-3);
        assert!((d.y1 - 220.0).abs() < 1e-3);
        assert!((d.x2 - 740.0).abs() < 1e-3);
        assert!((d.y2 - 420.0).abs() < 1e-3);
    }
}
