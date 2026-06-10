# Quick YAMNet sanity probe: classify a raw f32le 16kHz capture.
import sys

import numpy as np
import onnxruntime as rt

w = np.fromfile(sys.argv[1] if len(sys.argv) > 1 else "audio-test.raw", dtype=np.float32)
print(f"samples: {len(w)}, rms: {np.sqrt((w**2).mean()):.4f}")
s = rt.InferenceSession("yamnet.onnx")
scores = s.run(None, {"waveform": w})[0].mean(axis=0)
names = [l.split(",", 2)[2].strip().strip('"') for l in open("yamnet_class_map.csv", encoding="utf-8").read().splitlines()[1:]]
for i in scores.argsort()[::-1][:6]:
    print(f"{names[i]}: {scores[i]:.3f}")
