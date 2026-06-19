# spike-detect — cross-platform YOLO object detection

**Goal:** prove that a single exported YOLO model runs with **GPU acceleration on
both Windows and macOS** (and Linux) through ONNX Runtime — the capability that
makes Cammy's AI portable in a way Frigate isn't. This underpins Phase 4
(AI object detection).

## How portability works

The crate compiles a different ONNX Runtime execution provider per OS, selected at
runtime with automatic CPU fallback:

| OS | GPU backend | Hardware |
|---|---|---|
| Windows | **DirectML** | Any DirectX 12 GPU (Nvidia / AMD / Intel) |
| macOS | **CoreML** | Apple Silicon GPU + Neural Engine |
| Linux | **CUDA** | Nvidia GPU |
| any | CPU | fallback |

Same `.onnx`, same code — only the backend differs.

## Get a model

Export YOLOv8-nano to ONNX (one-time, needs Python + ultralytics):

```bash
pip install ultralytics
yolo export model=yolov8n.pt format=onnx imgsz=640 opset=12
# produces yolov8n.onnx
```

(Or download a pre-exported `yolov8n.onnx` from any YOLOv8 ONNX release.)

## Run

```bash
cargo run -p spike-detect -- --model yolov8n.onnx --image sample.jpg
```

The first run downloads a prebuilt ONNX Runtime (via the `download-binaries`
feature), so no system ONNX install is required.

Flags:

| Flag | Default | Purpose |
|---|---|---|
| `--model <path>` | `yolov8n.onnx` | ONNX model |
| `--image <path>` | (required) | Image to analyze |
| `--conf <f>` | `0.25` | Confidence threshold |
| `--iou <f>` | `0.45` | NMS IoU threshold |
| `--cpu` | off | Force CPU (handy to A/B the GPU speedup) |

## Expected output

```
  Detected 3 object(s) in 18.2ms:
    person           92.4%   box=[412, 96, totally real coords ...]
    dog              81.7%   box=[...]
    car              63.0%   box=[...]
```

The startup log prints which execution provider was selected, e.g.
`using GPU execution provider: DirectML`.

## Success criteria

- Detections are correct on a known test image (e.g. the classic bus/people photo).
- The log shows the **GPU** EP active on Windows and macOS (not CPU fallback).
- Compare `--cpu` vs default to confirm a real speedup.

If that holds, the cross-platform accelerated-AI risk is retired and Phase 4 becomes
"wire this into the motion gate and stream loop."

## Notes

- `ort` is pinned to `2.0.0-rc.10`. The execution-provider API has churned across
  pre-1.0 releases; if you bump the version, re-check the EP registration calls in
  `build_session`.
- YOLOv8 output is decoded assuming the `[1, 84, 8400]` layout (4 box + 80 COCO
  classes). YOLOv5/older exports use a different layout and would need a tweak.
