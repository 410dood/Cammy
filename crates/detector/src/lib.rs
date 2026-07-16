//! YOLOv8 object detection as a library — the productized form of the
//! `spike-detect` Phase 0 spike (which remains the standalone CLI validation).
//!
//! One exported `.onnx` runs everywhere: DirectML on Windows, CoreML on macOS,
//! CUDA on Linux, CPU fallback. `Detector` owns the ONNX Runtime session and is
//! `Send`, so a service can park it in a worker thread per pipeline.

use anyhow::{Context, Result};
use image::GenericImageView;
use ort::execution_providers::ExecutionProvider;
use ort::session::{
    builder::{GraphOptimizationLevel, SessionBuilder},
    Session,
};
use ort::value::Tensor;

// Re-exported so downstream crates (core's smart-search module) can build
// tensors and run sessions without declaring the pinned ort dep themselves.
pub use ort;

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

/// Number of COCO body keypoints a YOLOv8-pose model emits.
pub const POSE_KEYPOINTS: usize = 17;

/// One detected person with their body keypoints (YOLOv8-pose). Box in
/// original-image pixels; `keypoints[i] = [x, y, conf]` (x,y also pixels).
#[derive(Clone, Debug, serde::Serialize)]
pub struct PersonPose {
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub keypoints: [[f32; 3]; POSE_KEYPOINTS],
}

/// A loaded YOLOv8-**pose** model (single "person" class + 17 keypoints). Same
/// ONNX-Runtime / execution-provider machinery as [`Detector`]; used by the
/// server-side pose worker for the residential safety tier (fall, crib posture).
pub struct PoseEstimator {
    session: Session,
    conf: f32,
    iou: f32,
}

impl PoseEstimator {
    /// `accelerator` is the resolved accelerator string (`""`/`"auto"` = best
    /// per-OS EP, `"cpu"`, `"openvino"`) — see [`build_ort_session`].
    pub fn new(model_path: &str, accelerator: &str, conf: f32, iou: f32) -> Result<Self> {
        let session = build_ort_session(model_path, accelerator)?;
        Ok(Self { session, conf, iou })
    }

    /// Run pose estimation on one image. Returns people (box + keypoints) in
    /// original-image pixel coordinates, after NMS.
    pub fn estimate(&mut self, img: &image::DynamicImage) -> Result<Vec<PersonPose>> {
        let (orig_w, orig_h) = img.dimensions();
        let (input, scale, pad_x, pad_y) = letterbox_to_tensor(img);
        let outputs = self
            .session
            .run(ort::inputs!["images" => input])
            .context("pose inference failed")?;
        let (_name, output) = outputs.iter().next().context("model produced no outputs")?;
        let (shape, data) = output
            .try_extract_tensor::<f32>()
            .context("output was not an f32 tensor")?;
        let poses = decode_yolov8_pose(
            data,
            shape,
            self.conf,
            scale,
            pad_x,
            pad_y,
            orig_w as f32,
            orig_h as f32,
        );
        Ok(pose_nms(poses, self.iou))
    }
}

/// Build an ONNX Runtime session for the requested `accelerator`, with CPU as
/// the automatic fallback. Shared by every model we run (YOLO objects, face
/// detection, face embeddings, pose).
///
/// `accelerator` selects the execution provider:
/// - `""` or `"auto"` — the best per-OS EP (DirectML on Windows, CoreML on
///   macOS, CUDA on Linux). This is the historical default behavior.
/// - `"cpu"` — no GPU/accelerator EP; run on the CPU EP.
/// - `"openvino"` — the OpenVINO EP (Intel iGPU/NPU), registered before the CPU
///   fallback. Only actually available in a build with the `openvino` cargo
///   feature AND an ONNX Runtime that bundles OpenVINO; otherwise ONNX Runtime
///   silently falls back to CPU (see [`openvino_available`] for honest gating).
///
/// Any unrecognized value is treated as `"auto"`. ONNX Runtime registers each
/// requested EP with `error_on_failure = false`, so a requested EP that can't
/// load degrades to CPU rather than failing the session build.
pub fn build_ort_session(model_path: &str, accelerator: &str) -> Result<Session> {
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(4)?;

    let accel = accelerator.trim();
    if accel.eq_ignore_ascii_case("cpu") {
        // Explicit CPU: register no GPU/accelerator EP.
    } else if accel.eq_ignore_ascii_case("openvino") {
        builder = register_openvino(builder)?;
    } else {
        // "" / "auto" / anything else: the best per-OS execution provider.
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

/// Resolve a stored accelerator `choice` plus the legacy `force_cpu` flag into
/// the canonical accelerator string [`build_ort_session`] understands.
///
/// Precedence (documented contract): an explicit `choice` (`"cpu"` /
/// `"openvino"`) wins; when `choice` is empty or `"auto"`, `force_cpu` decides
/// (`"cpu"` when set, else `"auto"`). So the default — `choice == ""` and
/// `force_cpu == false` — yields `"auto"`, i.e. today's exact per-OS EP
/// behavior; `force_cpu == true` yields `"cpu"`, unchanged from before.
pub fn effective_accelerator(choice: &str, force_cpu: bool) -> &str {
    let c = choice.trim();
    if c.is_empty() || c.eq_ignore_ascii_case("auto") {
        if force_cpu {
            "cpu"
        } else {
            "auto"
        }
    } else {
        c
    }
}

/// Whether the OpenVINO execution provider is actually usable in this build.
///
/// Returns `false` when the `openvino` cargo feature is off (the default), with
/// no FFI probe at all. When the feature is on, it asks ONNX Runtime whether the
/// linked runtime advertises the OpenVINO provider — so it reports `true` only
/// when OpenVINO can genuinely run, never as a silent no-op. The UI keys the
/// OpenVINO option off this (via `/api/capabilities`), exactly like the optional
/// model-presence gates.
pub fn openvino_available() -> bool {
    #[cfg(feature = "openvino")]
    {
        use ort::execution_providers::OpenVINOExecutionProvider;
        OpenVINOExecutionProvider::default()
            .is_available()
            .unwrap_or(false)
    }
    #[cfg(not(feature = "openvino"))]
    {
        false
    }
}

/// Register the OpenVINO EP (built into this crate via the `openvino` feature),
/// before the CPU fallback.
#[cfg(feature = "openvino")]
fn register_openvino(builder: SessionBuilder) -> Result<SessionBuilder> {
    use ort::execution_providers::OpenVINOExecutionProvider;
    let ep = OpenVINOExecutionProvider::default();
    log_ep("OpenVINO", ep.is_available().unwrap_or(false));
    Ok(builder.with_execution_providers([ep.build()])?)
}

/// No-op OpenVINO registration for the default build (feature off): there is no
/// OpenVINO EP compiled in, so the session runs on the per-OS/CPU EPs. We never
/// pretend otherwise — `openvino_available()` reports false so the UI hides the
/// option.
#[cfg(not(feature = "openvino"))]
fn register_openvino(builder: SessionBuilder) -> Result<SessionBuilder> {
    tracing::warn!(
        "OpenVINO accelerator was requested, but this build has no OpenVINO EP; running on CPU"
    );
    Ok(builder)
}

impl Detector {
    /// Load the model with the requested `accelerator` (`""`/`"auto"` = best
    /// per-OS EP, `"cpu"`, `"openvino"`; see [`build_ort_session`]). `conf` /
    /// `iou` are the confidence and NMS thresholds.
    pub fn new(model_path: &str, accelerator: &str, conf: f32, iou: f32) -> Result<Self> {
        let session = build_ort_session(model_path, accelerator)?;
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

/// Decode raw YOLOv8-pose output [1, 56, 8400] into people in ORIGINAL image
/// coordinates. Layout: 56 = 4 box (cx,cy,w,h) + 1 person score + 17*3 keypoints
/// (x,y,conf each); 8400 anchors. Keypoint x,y are in the same 640 letterbox
/// space as the box, so they map back identically.
#[allow(clippy::too_many_arguments)]
fn decode_yolov8_pose(
    data: &[f32],
    shape: &[i64],
    conf: f32,
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    orig_w: f32,
    orig_h: f32,
) -> Vec<PersonPose> {
    let features = shape[1] as usize; // 56
    let anchors = shape[2] as usize; // 8400
                                     // Guard: must have at least the box + score + 17*3 keypoint channels.
    if features < 5 + POSE_KEYPOINTS * 3 {
        return Vec::new();
    }
    let at = |f: usize, a: usize| data[f * anchors + a];
    let unpad = |v: f32, pad: f32| (v - pad) / scale;

    let mut out = Vec::new();
    for a in 0..anchors {
        let score = at(4, a);
        if score < conf {
            continue;
        }
        let (cx, cy, bw, bh) = (at(0, a), at(1, a), at(2, a), at(3, a));
        let x1 = unpad(cx - bw / 2.0, pad_x).clamp(0.0, orig_w);
        let y1 = unpad(cy - bh / 2.0, pad_y).clamp(0.0, orig_h);
        let x2 = unpad(cx + bw / 2.0, pad_x).clamp(0.0, orig_w);
        let y2 = unpad(cy + bh / 2.0, pad_y).clamp(0.0, orig_h);

        let mut keypoints = [[0.0f32; 3]; POSE_KEYPOINTS];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            let base = 5 + k * 3;
            kp[0] = unpad(at(base, a), pad_x).clamp(0.0, orig_w);
            kp[1] = unpad(at(base + 1, a), pad_y).clamp(0.0, orig_h);
            kp[2] = at(base + 2, a);
        }
        out.push(PersonPose {
            score,
            x1,
            y1,
            x2,
            y2,
            keypoints,
        });
    }
    out
}

/// Greedy NMS over people (single class) by their boxes.
fn pose_nms(mut poses: Vec<PersonPose>, iou_thresh: f32) -> Vec<PersonPose> {
    poses.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<PersonPose> = Vec::new();
    'outer: for p in poses {
        for k in &keep {
            if box_iou((p.x1, p.y1, p.x2, p.y2), (k.x1, k.y1, k.x2, k.y2)) > iou_thresh {
                continue 'outer;
            }
        }
        keep.push(p);
    }
    keep
}

/// IoU of two `(x1,y1,x2,y2)` boxes.
fn box_iou(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> f32 {
    let ix1 = a.0.max(b.0);
    let iy1 = a.1.max(b.1);
    let ix2 = a.2.min(b.2);
    let iy2 = a.3.min(b.3);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let area_a = (a.2 - a.0).max(0.0) * (a.3 - a.1).max(0.0);
    let area_b = (b.2 - b.0).max(0.0) * (b.3 - b.1).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
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
    fn decode_pose_maps_box_and_keypoints() {
        // One anchor, person score 0.99. 56 features = 4 box + 1 score + 17*3 kpts.
        // Box cx,cy,w,h = (320,320,100,100) in 640-space; set the NOSE keypoint
        // (index 0) at (320, 300) conf 0.9, leave the rest zero.
        let mut data = vec![0.0f32; 56];
        data[0] = 320.0;
        data[1] = 320.0;
        data[2] = 100.0;
        data[3] = 100.0;
        data[4] = 0.99; // person score
        data[5] = 320.0; // nose x
        data[6] = 300.0; // nose y
        data[7] = 0.9; // nose conf
        let shape = [1i64, 56, 1];
        // Original 1280x640 -> scale 0.5, pad_y = 160.
        let poses = decode_yolov8_pose(&data, &shape, 0.5, 0.5, 0.0, 160.0, 1280.0, 640.0);
        assert_eq!(poses.len(), 1);
        let p = &poses[0];
        assert!((p.x1 - 540.0).abs() < 1e-3 && (p.y1 - 220.0).abs() < 1e-3);
        // Nose maps back: x = (320-0)/0.5 = 640; y = (300-160)/0.5 = 280.
        assert!((p.keypoints[0][0] - 640.0).abs() < 1e-3);
        assert!((p.keypoints[0][1] - 280.0).abs() < 1e-3);
        assert!((p.keypoints[0][2] - 0.9).abs() < 1e-6);
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
