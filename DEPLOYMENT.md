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

## 2b. Windows service (headless 24/7 recording)

The desktop app runs in your login session — it keeps recording when the window
is closed (tray), but **stops at sign-out**. For a true appliance that records
at the lock screen and with nobody signed in, install the headless engine as a
Windows service (runs as LocalSystem, auto-starts at boot, and the OS restarts
it on a crash):

```powershell
# From an ELEVATED (Administrator) prompt, in the folder that holds
# zoomy.exe alongside .\bin\go2rtc.exe, .\web\dist and the model files:
.\zoomy.exe --install-service --data-dir D:\CammyData --port 8080

# Manage it like any service:
net stop cammy
net start cammy

# Remove it:
.\zoomy.exe --uninstall-service
```

The install captures the **absolute** data/UI paths and the working directory,
so `./bin` and relative model paths resolve exactly as a terminal run. Logs go
to `<data-dir>\service.log`.

**Service vs. desktop app:** they are mutually exclusive **per data folder** —
an exclusive lock on `<data-dir>\.cammy.lock` makes whichever starts second
fail fast with a clear message instead of double-recording into the same files.
Run the service for 24/7 capture and use a browser at `http://localhost:8080/`
as the UI; don't point the desktop app at the same data folder.

### Reaching the UI from phones on your LAN

Windows Firewall blocks inbound connections by default. Allow Cammy's port
(elevated prompt; pick your service port):

```powershell
netsh advfirewall firewall add rule name="Cammy NVR" dir=in action=allow protocol=TCP localport=8080
```

Then browse to `http://<this-PC's-LAN-IP>:8080/` (find it with `ipconfig`) and
set a password in Settings → Remote access first.

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

## 3b. Remote access WITHOUT port-forwarding (recommended for phones)

Opening a router port is the #1 way home NVRs end up on Shodan. Two free,
zero-port-forward options work with Cammy out of the box — both keep the NVR
entirely local and need **no Cammy flags at all** (the connection arrives as a
normal LAN/localhost client):

### Tailscale (simplest — private VPN)

```bash
# on the NVR box (once):
tailscale up
# on your phone: install Tailscale, sign in to the same tailnet
```

Open `http://<nvr-hostname>:8080` from anywhere. Live WebRTC video, pushes and
clip links all work; the tailnet is already end-to-end encrypted, so no TLS
setup is needed. Set **Settings → public base URL** to the Tailscale URL so
push notifications carry tap-through clip links that resolve on your phone.
(Tailscale Funnel can additionally publish it at a public HTTPS URL — treat
that like Cloudflare Tunnel below and set a password + 2FA first.)

### Cloudflare Tunnel (public HTTPS URL, no VPN app on the phone)

```bash
cloudflared tunnel create cammy
cloudflared tunnel route dns cammy nvr.example.com
cloudflared tunnel run --url http://localhost:8080 cammy
```

The URL is on the public internet, so **set a password + enable 2FA first**,
run zoomy with `--trusted-proxy` (cloudflared connects from localhost, and the
throttle/audit must see real client IPs), and set the public base URL to
`https://nvr.example.com`. WebSockets (live video) are proxied by default.

> Don't build or rent a relay: a VPN/tunnel you control is safer than any
> vendor cloud relay, and it's the reason Cammy will never need a subscription.

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
database + recordings, and runs with `--trusted-proxy`. It publishes the port on
**loopback only** (`127.0.0.1:8080:8080`) so that `--trusted-proxy` default is
safe — only a same-host reverse proxy (the Caddy/nginx config above) can reach it.
Do **not** publish the port to the network while `--trusted-proxy` is on: the
brute-force throttle and loopback exemption would then key off an attacker-spoofable
`X-Forwarded-For`. ffmpeg is installed in the image; go2rtc is mounted from `./bin`
(or download it in your own build step).

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
- **Updates:** the **desktop app self-updates** — it checks GitHub Releases on
  launch, and tray → "Check for updates" / "Install update vX" applies one (it
  never installs without your click; recording resumes after the restart).
  Headless/server installs: stop the service, replace the binary + `web/dist`,
  restart. The SQLite schema self-migrates on start. Take a `GET /api/backup` first.

---

## 5b. Home Assistant integration (P3.3)

Cammy talks to Home Assistant three ways — pick any combination:

**1. MQTT discovery (outbound, already shipped).** Point Settings → Notifications
→ MQTT at your broker; with "Home Assistant discovery" on, Cammy auto-creates a
`binary_sensor` per (camera, object) and a "last detection" sensor per camera,
and publishes its arm mode retained to `<prefix>/mode`. No YAML.

**2. Live SSE event feed (outbound, new).** `GET /api/events/stream` is a
Server-Sent-Events stream of new events — one `data:` line of compact JSON
`{event_id, camera, label, score, ts, snapshot}` per event, plus `:` keep-alive
comments. Viewer-role; authenticate with an API token
(`Authorization: Bearer zoomy_<hex>` from Settings → API tokens). It is
**RBAC-scoped identically to `GET /api/events`** (a scoped token only sees its
cameras), and works even when outbound MQTT is disabled. Verify by hand:

```bash
curl -N -H "Authorization: Bearer zoomy_XXXX" http://NVR:8080/api/events/stream
```

**3. Inbound MQTT commands (control surface, opt-in, default OFF, new).** Turn on
Settings → Notifications → "Accept commands over MQTT". Cammy then subscribes to
`<prefix>/cmd/#` and accepts:

| Topic | Payload | Effect |
| --- | --- | --- |
| `<prefix>/cmd/arm` | `home` \| `away` \| `disarmed` | Set the system security mode (like `PUT /api/arm`). |
| `<prefix>/cmd/trigger` | camera id or exact name | Log a bookmarked soft-trigger event on that camera and fire matching alarm rules (like `POST /api/cameras/{id}/trigger`). |

> ⚠️ **This is a control surface.** Anyone who can publish to your MQTT broker can
> arm/disarm and trigger cameras. Only enable it on a broker you control, and keep
> the broker itself authenticated. Every accepted command is written to Cammy's
> security audit log; malformed/unknown commands are ignored.

**Custom component (v0 skeleton).** A Home Assistant `custom_component` that
consumes the SSE feed (last-event sensor + per-camera motion binary_sensors)
lives under [`integrations/homeassistant/`](integrations/homeassistant/). It is a
documented **starting point, not tested on a live HA instance** — see its README
for install (manual copy to `config/custom_components/cammy/`, or HACS custom
repository).

---

## 5c. Apple HomeKit bridge (P3.4 — live view + motion sensors)

Cammy can expose selected cameras to Apple **Home**: live view in the Home app
and on Apple TV (via the already-supervised go2rtc streamer's HAP server), plus
— since v1a — **one HomeKit motion sensor per exposed camera** (via Cammy's own
in-process "Cammy Sensors" HAP bridge), which is what unlocks Home automations
("when motion at the front door, turn on the porch light"). **No extra
software.**

**Two pairings, honestly.** Each camera is its own HomeKit accessory with the
*camera* code; all motion sensors arrive together as ONE extra "Cammy Sensors"
bridge accessory with its *own* code (both codes are on the Settings card). This
is a HomeKit protocol limitation — go2rtc's camera accessory cannot carry sensor
services — so the Home app shows the sensor as a separate accessory; you can
still reference both in the same automation/room. A motion sensor turns on when
Cammy detects motion-driven events (person/vehicle/animal, tripwire, loitering,
zone entry) on that camera and clears ~45 s after the last one.

**Doorbell button (v1b).** A camera with **"HomeKit doorbell button"** enabled
(Cameras → Detection tuning, needs "Expose to HomeKit") also appears on the
Cammy Sensors bridge as a programmable switch that fires a *single press* when
the camera's audio detection hears a doorbell chime (YAMNet "Doorbell") or a
soft trigger labeled `doorbell` arrives. It is deliberately **not** a full
HomeKit doorbell accessory — the Home app rejects doorbell accessories that
don't carry their own camera stream, and go2rtc's camera accessory can't carry
sensor services — so use it as an automation/notification trigger.

**Pairing must be done on a real Apple device — it cannot be verified from the
server side.** Default is OFF: when off, the generated `go2rtc.yaml` is
byte-for-byte unchanged AND the sensor bridge never binds a socket or announces
itself on mDNS.

**Network requirement.** HomeKit discovers accessories over **mDNS/Bonjour** on
the local network, so the NVR host and your Apple hub (HomePod / Apple TV / an
iPad kept at home) **must be on the same LAN / broadcast domain**. It does not
traverse subnets, VLANs, or a VPN without an mDNS reflector, and it is not
reachable remotely (use the Home hub for that). The HAP server binds a port on
all interfaces; keep the NVR on a trusted LAN.

**Windows Firewall.** The sensor bridge answers mDNS itself and serves HAP on
TCP `32180`, so on Windows allow both in (the §2 rule only covered the web UI):

```powershell
netsh advfirewall firewall add rule name="Cammy HomeKit mDNS" dir=in action=allow protocol=UDP localport=5353
netsh advfirewall firewall add rule name="Cammy HomeKit sensors" dir=in action=allow protocol=TCP localport=32180
```

If "Cammy Sensors" never appears in the Home app's nearby-accessory list, the
mDNS rule (UDP 5353) is the usual culprit; a multi-homed NVR (several NICs) can
also announce on the wrong interface — disable unused adapters or add the code
manually via **More options… → My accessory isn't shown here**.

**Enable + pair:**

1. **Settings → Access & security → Apple HomeKit → "Run the HomeKit bridge"**,
   then **Save** (this restarts the streamer once, briefly blipping live views).
   Enabling the bridge and reading the pairing code are **Admin-only** — the code
   is a pairing secret.
2. For each camera you want in Home, open **Cameras → Detection tuning → Stream &
   recording → "Expose to HomeKit"** and save. A sensitive / no-clip camera stays
   **off** HomeKit unless you explicitly expose it here.
3. Back on the Settings HomeKit card, note the **two pairing codes** (shown as
   `XXX-XX-XXX`): the camera code and the "Cammy Sensors" code.
4. On your iPhone/iPad on the same Wi-Fi: **Home app → + → Add Accessory → More
   options…**, pick the Cammy camera, and enter the camera code. Repeat per
   camera.
5. Add **Cammy Sensors** the same way with its own code — one pairing brings the
   motion sensor for every exposed camera.

**Pairing management (v1c).** The Settings HomeKit card lists each exposed
camera with its paired-device count and an **Unpair** button (drops that
camera's pairing records and restarts the streamer), plus the Cammy Sensors
bridge's own count. **Reset camera pairings** rotates the camera code and every
per-camera identity — all Apple devices lose all Cammy cameras until re-added.
**Reset sensor bridge** wipes the sensor bridge's identity/pairings and mints a
new sensor code. Both resets are deliberately loud in the UI; there is no undo.

**Identity / pairing persistence.** Cammy generates and persists the pairing PIN
and a stable per-camera HomeKit identity (device id + private key) in its
settings store, so a paired controller keeps trusting the accessory across
restarts. Controller pairing records that go2rtc writes are carried forward
best-effort when the config is regenerated; if Home ever shows an accessory as
"No Response" after a reconfiguration, remove and re-add it.

---

## 6. Releasing (maintainers): auto-update artifacts + code signing

Pushing a `v*` tag runs `.github/workflows/release.yml`: it fetches go2rtc/
ffmpeg/models, builds the NSIS installer + Tauri **updater artifacts**, and
attaches them plus `latest.json` to a draft GitHub Release. Installed desktop
apps poll `releases/latest/download/latest.json` (endpoint + Ed25519-style
pubkey pinned in `crates/desktop/tauri.conf.json`).

Repo secrets the owner supplies (all optional; builds succeed unsigned without
them):

| Secret | Purpose |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` (+ `_PASSWORD`) | Updater signature. Generate once with `npx @tauri-apps/cli signer generate -w ~/.tauri/cammy_updater.key`; the matching pubkey is committed in `tauri.conf.json`. **Without it, releases are installable but existing apps will refuse to auto-update to them.** Never commit the private key. |
| `CAMMY_SIGN_THUMBPRINT` **or** `CAMMY_SIGN_COMMAND` | Windows Authenticode (SmartScreen trust). Consumed by `crates/desktop/sign.ps1`, which is a no-op when unset. Thumbprint = a code-signing cert in the machine's store; command = a full custom `signtool`/Azure Trusted Signing invocation with `%1` as the artifact path. |

Local installer builds now also want the updater key —
`TAURI_SIGNING_PRIVATE_KEY` takes either the key **contents or its path**
(there is no separate `_PATH` variable):

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = "$HOME\.tauri\cammy_updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
npx @tauri-apps/cli build   # from crates/desktop
```
