# Deploying Cammy (self-hosted, production)

Cammy ships as a single headless binary (`zoomy`) that supervises `go2rtc` and
`ffmpeg` as child processes. The README covers building and the desktop app; this
guide covers running it as a long-lived **server** behind a reverse proxy.

There are three supported ways to run it, in order of how battle-tested they are:

1. **Bare-metal + systemd** (Linux) or a Windows service / the Tauri desktop app —
   the primary, validated path. Start here.
2. **Behind a reverse proxy** (Caddy / nginx) for TLS + a real hostname.
3. **Docker / Compose** — a community starting point (see the caveat in
   [Docker](#docker)).

> The whole product is **local-first**: no cloud account, no mandatory external
> service. Everything below runs on your own box/LAN.

---

## 1. What the server needs at runtime

| Thing | How it's found | Notes |
|---|---|---|
| `zoomy` binary | `cargo build --release -p zoomy` → `target/release/zoomy` | one static-ish binary |
| Web UI | `--ui-dir` (default `web/dist`) | build once: `cd web && npm ci && npm run build` |
| `go2rtc` | `--go2rtc-bin`, `GO2RTC_BIN`, `./bin/go2rtc`, or `PATH` | [releases](https://github.com/AlexxIT/go2rtc/releases) |
| `ffmpeg` | `--ffmpeg-bin`, `FFMPEG_BIN`, `./bin`, or `PATH` | distro package is fine |
| AI models | the **working directory** (`yolov8n.onnx`, `clip_*`, `yamnet*`, …) | see the README "Prerequisites"; optional models silently stay off when absent — check **Settings → Models & capabilities** |
| Data dir | `--data-dir` (default `./data`) | SQLite db, recordings, snapshots, generated `go2rtc.yaml`, self-signed `tls/` |

Because models are resolved from the **current working directory**, run the server
*from* the directory that holds them (the systemd unit below sets `WorkingDirectory`).

### Key flags

```
zoomy --port 8080 \
      --data-dir /var/lib/cammy/data \
      --ui-dir   /opt/cammy/web/dist \
      [--tls-self-signed | --tls-cert cert.pem --tls-key key.pem] \
      [--trusted-proxy]
```

- `--trusted-proxy` — **set this when (and only when) Cammy is reachable solely
  through your reverse proxy.** It makes auth + the brute-force throttle key off the
  right-most `X-Forwarded-For` hop, so the proxy's loopback connection doesn't
  inherit the local-access exemption (and a spoofed `XFF: 127.0.0.1` can't bypass
  the password). Do **not** set it if the port is also directly reachable.
- TLS: either let Cammy mint a reusable self-signed cert under `<data_dir>/tls`
  (`--tls-self-signed`), pass your own PEM (`--tls-cert`/`--tls-key`, or the
  `ZOOMY_TLS_CERT`/`ZOOMY_TLS_KEY` env vars), or terminate TLS at the proxy (below).
- `RUST_LOG=info,zoomy=info` controls logging.

### First-run security

Open the UI, finish onboarding, and **set a password** (Settings → Remote access).
Loopback is exempt so you can never lock yourself out locally. For off-LAN exposure
also enable **2FA** (Settings → Two-factor authentication) and serve over HTTPS.

---

## 2. systemd (Linux)

A ready-to-edit unit lives at [`deploy/zoomy.service`](deploy/zoomy.service). Install:

```bash
sudo useradd --system --home /var/lib/cammy --shell /usr/sbin/nologin cammy
sudo mkdir -p /opt/cammy /var/lib/cammy/data
# place the binary, web/dist, bin/go2rtc, and the model files under /opt/cammy
sudo chown -R cammy:cammy /opt/cammy /var/lib/cammy

sudo cp deploy/zoomy.service /etc/systemd/system/cammy.service
sudo systemctl daemon-reload
sudo systemctl enable --now cammy
journalctl -u cammy -f
```

The unit pins `WorkingDirectory=/opt/cammy` (so models resolve) and a separate
`--data-dir=/var/lib/cammy/data` (so recordings live on your data volume).

---

## 3. Reverse proxy + TLS

Run Cammy on loopback with `--trusted-proxy` and let the proxy own the certificate
and hostname. Cammy already proxies go2rtc internally and exposes a single port, so
the proxy only needs **one** upstream — but it **must forward WebSocket upgrades**
(`/api/ws` carries live video).

### Caddy (recommended — automatic HTTPS)

[`deploy/Caddyfile.example`](deploy/Caddyfile.example):

```caddyfile
nvr.example.com {
    reverse_proxy 127.0.0.1:8080
    # Caddy forwards X-Forwarded-For and upgrades WebSockets automatically.
}
```

Run Cammy with `--trusted-proxy` (and plain HTTP on loopback — Caddy does TLS):

```bash
zoomy --port 8080 --trusted-proxy --data-dir /var/lib/cammy/data --ui-dir /opt/cammy/web/dist
```

### nginx

```nginx
server {
    listen 443 ssl http2;
    server_name nvr.example.com;
    ssl_certificate     /etc/letsencrypt/live/nvr.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/nvr.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host              $host;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # WebSocket upgrade for live view (/api/ws):
        proxy_http_version 1.1;
        proxy_set_header Upgrade    $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
    }
}
```

Set **Settings → public base URL** to `https://nvr.example.com` so push
notifications carry working tap-through clip links. (Cammy now warns in Settings if
that URL is `http://` or a private/LAN host that won't resolve away from home.)

---

## 4. Docker

> **Caveat:** Cammy's first-class distribution is the native binary and the Tauri
> desktop installer. The [`Dockerfile`](Dockerfile) + [`docker-compose.yml`](docker-compose.yml)
> here are a **starting point** for self-hosters who prefer containers — they follow
> the documented Linux build path but are not part of the release CI, so treat them
> as community config and pin/verify before relying on them. Model files are **not**
> baked into the image — you mount them (they're large and license-bound).

```bash
# Put your downloaded *.onnx / *.bin / *.csv model files in ./models
# and go2rtc in ./bin, then:
docker compose up -d --build
docker compose logs -f
```

The compose file mounts `./models` as the working directory, `./data` for the
database + recordings, and runs with `--trusted-proxy` so you can front it with the
same Caddy/nginx config above. ffmpeg is installed in the image; go2rtc is mounted
from `./bin` (or download it in your own build step).

---

## 5. Operations

- **Health/metrics:** `GET /api/health` (unauthenticated) and Prometheus
  `GET /api/metrics` (loopback or an API token via `Authorization: Bearer`). A
  starter Grafana scrape: point Prometheus at `https://nvr.example.com/api/metrics`
  with a Bearer token from Settings → API tokens.
- **Config backup:** `GET /api/backup` (cameras + settings + alarms). Recordings can
  mirror to any S3-compatible bucket (incl. MinIO/NAS) via Settings → Offsite backup.
- **Storage:** set the recordings location + retention (age + total-bytes cap) in
  Settings; watch free space on the Overview/Storage card.
- **Updates:** stop the service, replace the binary + `web/dist`, restart. The
  SQLite schema self-migrates on start. Take a `GET /api/backup` first.
