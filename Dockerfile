# syntax=docker/dockerfile:1
#
# Community starting point — NOT part of release CI. See DEPLOYMENT.md §4.
# Model files (*.onnx / *.bin / yamnet_class_map.csv) are NOT baked in; they're
# mounted at runtime (large + license-bound). go2rtc is mounted too.

# ---- web UI build ----
FROM node:20-bookworm-slim AS web
WORKDIR /web
COPY web/package*.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ---- rust build ----
FROM rust:1-bookworm AS build
# whisper.cpp (compiled into the binary) needs cmake + a C toolchain. On glibc
# x86_64 we use whisper-rs's shipped FFI bindings to avoid needing libclang.
ENV WHISPER_DONT_GENERATE_BINDINGS=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p zoomy
# Collect the binary plus the onnxruntime shared lib that `ort` fetched at build
# time, so the runtime image doesn't need the build toolchain.
RUN mkdir -p /out \
 && cp target/release/zoomy /out/ \
 && find target -name 'libonnxruntime*.so*' -exec cp {} /out/ \; || true

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /app/
COPY --from=web   /web/dist /app/web/dist
# onnxruntime.so sits next to the binary.
ENV LD_LIBRARY_PATH=/app
# Working directory holds the model files (mounted at /models). go2rtc is found
# via the default ./bin lookup (mount your binary at /models/bin/go2rtc).
WORKDIR /models
EXPOSE 8080
ENTRYPOINT ["/app/zoomy", "--ui-dir", "/app/web/dist", "--data-dir", "/data"]
# Drop --trusted-proxy if you expose this port directly (no reverse proxy).
CMD ["--port", "8080", "--trusted-proxy"]
