//! Phase 0 — Spike 2: prove cross-platform, GPU-accelerated AI object detection.
//!
//! This runs a YOLOv8 model (exported to ONNX) on a single image via ONNX
//! Runtime, and prints the detected objects. The whole point is portability:
//! the SAME `.onnx` file runs with a GPU backend chosen for the host OS —
//! DirectML on Windows, CoreML on macOS, CUDA on Linux — with automatic CPU
//! fallback. This is the capability Frigate can't offer natively on Windows/Mac
//! and is the core of Cammy's AI story.
//!
//! Pipeline: load image -> letterbox to 640x640 -> NCHW f32 tensor ->
//! ONNX Runtime inference -> decode [1,84,8400] output -> NMS -> print.

use anyhow::{Context, Result};
use clap::Parser;
use image::GenericImageView;
use ort::execution_providers::ExecutionProvider;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

/// Run YOLOv8 object detection on an image.
#[derive(Parser, Debug)]
#[command(name = "spike-detect", version, about)]
struct Args {
    /// Path to the YOLOv8 ONNX model (see README for the export command).
    #[arg(long, default_value = "yolov8n.onnx")]
    model: String,

    /// Path to an input image (jpg/png/...).
    #[arg(long)]
    image: String,

    /// Confidence threshold (0..1).
    #[arg(long, default_value_t = 0.25)]
    conf: f32,

    /// IoU threshold for non-max suppression (0..1).
    #[arg(long, default_value_t = 0.45)]
    iou: f32,

    /// Force CPU even if a GPU backend is available (useful for A/B timing).
    #[arg(long)]
    cpu: bool,
}

/// Square input size YOLOv8 expects.
const IMGSZ: u32 = 640;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let mut session =
        build_session(&args.model, args.cpu).context("failed to build ONNX Runtime session")?;

    // --- Preprocess ------------------------------------------------------
    let img = image::open(&args.image).with_context(|| format!("opening image {}", args.image))?;
    let (orig_w, orig_h) = img.dimensions();
    let (input, scale, pad_x, pad_y) = letterbox_to_tensor(&img);
    tracing::info!(
        orig_w,
        orig_h,
        "preprocessed image (letterboxed to {IMGSZ})"
    );

    // --- Inference -------------------------------------------------------
    let start = std::time::Instant::now();
    let outputs = session
        .run(ort::inputs!["images" => input])
        .context("inference failed")?;
    let elapsed = start.elapsed();

    // YOLOv8 output is typically named "output0", shape [1, 84, 8400].
    let (_name, output) = outputs.iter().next().context("model produced no outputs")?;
    let (shape, data) = output
        .try_extract_tensor::<f32>()
        .context("output was not an f32 tensor")?;
    tracing::info!(?shape, "got raw detections in {elapsed:?}");

    // --- Postprocess (decode + NMS) -------------------------------------
    let dets = decode_yolov8(
        data,
        shape,
        args.conf,
        scale,
        pad_x,
        pad_y,
        orig_w as f32,
        orig_h as f32,
    );
    let dets = non_max_suppression(dets, args.iou);

    // --- Report ----------------------------------------------------------
    println!();
    if dets.is_empty() {
        println!("  No objects above confidence {:.2}.", args.conf);
    } else {
        println!("  Detected {} object(s) in {elapsed:?}:", dets.len());
        for d in &dets {
            println!(
                "    {:<14} {:>5.1}%   box=[{:.0}, {:.0}, {:.0}, {:.0}]",
                coco_label(d.class),
                d.score * 100.0,
                d.x1,
                d.y1,
                d.x2,
                d.y2
            );
        }
    }
    println!();
    Ok(())
}

/// Build an ONNX Runtime session, registering the best execution provider for
/// this OS first and letting it fall back to CPU. Registration is best-effort:
/// if the GPU EP isn't available at runtime, ORT silently uses the next one.
fn build_session(model_path: &str, force_cpu: bool) -> Result<Session> {
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(4)?;

    if !force_cpu {
        // Order matters: most-preferred first. We only register the EP that was
        // compiled in for this target (see Cargo.toml per-OS features).
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
    } else {
        tracing::info!("--cpu set: skipping GPU execution providers");
    }

    let session = builder
        .commit_from_file(model_path)
        .with_context(|| format!("loading model {model_path}"))?;
    Ok(session)
}

fn log_ep(name: &str, available: bool) {
    if available {
        tracing::info!("using GPU execution provider: {name}");
    } else {
        tracing::warn!("{name} not available at runtime; falling back to CPU");
    }
}

/// Resize an image into a 640x640 letterbox (preserve aspect, pad with gray)
/// and produce a [1,3,640,640] f32 NCHW tensor normalized to 0..1. Returns the
/// tensor plus the scale and padding needed to map boxes back to original coords.
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
        chw[idx] = px[0] as f32 / 255.0; // R plane
        chw[plane + idx] = px[1] as f32 / 255.0; // G plane
        chw[2 * plane + idx] = px[2] as f32 / 255.0; // B plane
    }

    let tensor = Tensor::from_array(([1usize, 3, IMGSZ as usize, IMGSZ as usize], chw))
        .expect("failed to build input tensor");
    (tensor, scale, pad_x, pad_y)
}

#[derive(Clone, Debug)]
struct Det {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    score: f32,
    class: usize,
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
) -> Vec<Det> {
    // shape = [1, 84, num_anchors]
    let features = shape[1] as usize; // 84
    let anchors = shape[2] as usize; // 8400
    let num_classes = features - 4;

    // data[f * anchors + a] gives feature f for anchor a.
    let at = |f: usize, a: usize| data[f * anchors + a];

    let mut dets = Vec::new();
    for a in 0..anchors {
        // Find best class for this anchor.
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

        // Box is in 640-letterbox space; undo padding + scale back to original.
        let cx = at(0, a);
        let cy = at(1, a);
        let bw = at(2, a);
        let bh = at(3, a);

        let x1 = ((cx - bw / 2.0) - pad_x) / scale;
        let y1 = ((cy - bh / 2.0) - pad_y) / scale;
        let x2 = ((cx + bw / 2.0) - pad_x) / scale;
        let y2 = ((cy + bh / 2.0) - pad_y) / scale;

        dets.push(Det {
            x1: x1.clamp(0.0, orig_w),
            y1: y1.clamp(0.0, orig_h),
            x2: x2.clamp(0.0, orig_w),
            y2: y2.clamp(0.0, orig_h),
            score: best_s,
            class: best_c,
        });
    }
    dets
}

/// Standard greedy non-max suppression, per class.
fn non_max_suppression(mut dets: Vec<Det>, iou_thresh: f32) -> Vec<Det> {
    dets.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Det> = Vec::new();
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

fn iou(a: &Det, b: &Det) -> f32 {
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
fn coco_label(i: usize) -> &'static str {
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
