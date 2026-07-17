# Cammy — Home Assistant custom integration (v0 skeleton)

> **Status: v0 skeleton — NOT tested on a live Home Assistant instance.**
> This code was written against Cammy's real, shipped HTTP API, but it has not
> been run inside Home Assistant. Treat it as a **starting point**: read it,
> try it on a test HA instance, and fix anything that doesn't behave. It is not
> production-tested and makes no such claim.

It gives Home Assistant a **local push** view of your Cammy NVR by consuming
Cammy's live event feed (Server-Sent Events) over an API token — no cloud, no
polling.

## What it creates

| Entity | Platform | Behaviour |
| --- | --- | --- |
| `sensor.cammy_last_event` | `sensor` | State = the label of the most recent event; attributes carry the full `{event_id, camera, label, score, ts, snapshot}`. |
| `binary_sensor.<camera>` | `binary_sensor` (device_class `motion`) | One per camera. Turns **on** when an event for that camera arrives and auto-clears after 30 s of quiet. |

Cameras are discovered once at setup from `GET /api/cameras`.

## Cammy endpoints it uses

- **`GET /api/events/stream`** — the live event feed. A Server-Sent-Events (SSE)
  stream; each event is a `data:` line of compact JSON
  `{event_id, camera, label, score, ts, snapshot}`. Sends `:` keep-alive
  comments so proxies don't drop the connection. Requires `Authorization: Bearer <token>`
  (Viewer role). Results are RBAC-scoped exactly like `GET /api/events` — a
  token/user scoped to a subset of cameras only sees those cameras' events.
- **`GET /api/cameras`** — to enumerate camera names at setup (Viewer role).

Get an API token in Cammy under **Settings → API tokens** (it looks like
`zoomy_<hex>` and is shown once).

### Try the feed by hand

```bash
curl -N -H "Authorization: Bearer zoomy_XXXX" http://192.168.1.10:8080/api/events/stream
# → data: {"event_id":123,"camera":"Front Door","label":"person","score":0.91,"ts":1720000000,"snapshot":"/api/snapshots/..."}
```

## Optional: control Cammy from Home Assistant over MQTT

Cammy can also accept **inbound commands over MQTT** (opt-in, default **off** —
enable it in Cammy under **Settings → Notifications → "Accept commands over
MQTT"**). With Cammy and HA on the same MQTT broker:

- Publish to `<prefix>/cmd/arm` with payload `home`, `away`, or `disarmed` to set
  Cammy's security mode.
- Publish to `<prefix>/cmd/trigger` with payload = a camera id or exact name to
  log a soft-trigger event (and fire matching alarm rules) on that camera.

`<prefix>` is Cammy's MQTT topic prefix (default `zoomy`). Cammy already publishes
its arm mode retained to `<prefix>/mode` and Home-Assistant-discovery entities via
the standard MQTT integration.

> **Security:** the command topic is a control surface. Anyone who can publish to
> your broker can arm/disarm and trigger cameras, so only enable it on a broker
> you trust. Every accepted command is written to Cammy's security audit log.

## Install

**Manual (recommended for a skeleton):**

1. Copy the `cammy/` folder into your Home Assistant config directory at
   `config/custom_components/cammy/`.
2. Restart Home Assistant.
3. **Settings → Devices & Services → Add Integration → “Cammy NVR”**, then enter
   your Cammy base URL (e.g. `http://192.168.1.10:8080`) and an API token.

**Via HACS (custom repository):**

1. HACS → ⋮ → **Custom repositories** → add `https://github.com/410dood/Cammy`
   with category **Integration**.
2. HACS installs from this repo using `integrations/homeassistant/hacs.json`
   (`content_in_root: false`, the integration lives under
   `integrations/homeassistant/cammy/`). If your HACS/version expects the classic
   `custom_components/<domain>/` layout, use the manual install above instead.
3. Restart Home Assistant and add the integration as above.

## Files

```
integrations/homeassistant/
├── hacs.json                 # HACS custom-repo descriptor
├── README.md                 # this file
└── cammy/
    ├── manifest.json         # HA integration manifest (config_flow, local_push)
    ├── const.py              # domain, config keys, endpoints, dispatcher signal
    ├── __init__.py           # SSE client + setup/unload; dispatches events
    ├── config_flow.py        # host + API token, verified against /api/cameras
    ├── sensor.py             # "last event" sensor
    ├── binary_sensor.py      # per-camera motion binary_sensor
    ├── strings.json          # config-flow strings
    └── translations/en.json  # English translations
```

## Known limitations of this v0

- Not run on a live HA instance — verify entity registration, the config flow,
  and reconnect behaviour yourself.
- Cameras are enumerated once at setup; a camera added in Cammy afterwards needs
  a reload of the integration to get its motion sensor.
- Only "last event" + per-camera motion are modelled. Snapshots, clips, PTZ,
  arming, and per-object sensors are left as future work.
