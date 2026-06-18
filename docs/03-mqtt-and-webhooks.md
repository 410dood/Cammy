# MQTT, Home Assistant & webhook integration

How ZoomyZoomyCamCam publishes detections to home-automation systems. Everything
here is opt-in via **Settings → Notifications**.

## MQTT topics

With an MQTT broker configured (`mqtt_url`) and a prefix `P` (`mqtt_prefix`,
default `zoomy`):

| Topic | Retained | Payload | Meaning |
|---|---|---|---|
| `P/available` | yes | `online` / `offline` | NVR liveness (backed by an MQTT last-will, so a crash flips it to `offline`). |
| `P/events` | no | event JSON (below) | Every detection event. |
| `P/{camera}/{label}` | no | `"0.92"` | Confidence of the latest detection of `label` on `camera`. |
| `P/{camera}/{slug}/state` | no | `ON` / `OFF` | Occupancy flag per (camera, object). Goes `ON` on detection, auto-clears to `OFF` after `mqtt_state_timeout_secs`. `slug` is the label with non-alphanumerics replaced by `_` (e.g. `traffic_light`). |
| `P/{camera}/event` | yes | event JSON | Latest event on that camera (drives the HA "last detection" sensor). |
| `P/alarms/{suffix}` | no | event JSON | Published by Alarm Manager rules whose action is `mqtt` with target `suffix`. |

### Event JSON

```json
{
  "type": "detection",
  "event_id": 1234,
  "camera": "front-door",
  "label": "person",
  "score": 0.92,
  "ts": 1718000000,
  "snapshot": "/api/snapshots/front-door-1718000000.jpg"
}
```

`ts` is unix seconds. `snapshot` is a path on this NVR; prefix it with the
server's base URL to fetch the annotated still.

## Home Assistant auto-discovery

When **Home Assistant discovery** is on (`mqtt_ha_discovery`, default on), the NVR
publishes retained discovery configs under `mqtt_ha_prefix` (default
`homeassistant`) on every connect, so HA creates entities with no YAML:

- **`binary_sensor.zoomy_{camera}_{label}`** — `ON` while that object is present
  (cleared after the ON timeout). `device_class` is mapped per label
  (`person`→`occupancy`, vehicles→`moving`, pets→`presence`, else `motion`).
- **`sensor.zoomy_{camera}_event`** — state is the last object label; the full
  event JSON is attached as attributes (`json_attributes_topic`).

All of a camera's entities group under one HA **device** (`Zoomy {camera}`).
Discovery is re-published automatically when you add/remove/enable cameras or
change the detected-object list. These entities are ready-made automation
triggers (e.g. *person on front-door after sunset → turn on the porch light*).

## Webhooks

Set a per-event webhook URL (`webhook_url`) and/or use Alarm Manager rules with a
`webhook` action. By default the body is the event JSON above (alarm webhooks add
`"type":"alarm"` plus `face`, `plate`, `gesture` fields when present).

### Templating

Set **webhook body template** (`webhook_template`) to send a custom shape — useful
for Slack/Discord/Teams or any service expecting specific fields. The template is
sent verbatim with `application/json` after substituting these placeholders
(values are JSON-escaped, so a JSON template stays valid):

| Placeholder | Value |
|---|---|
| `{{event_id}}` | numeric event id |
| `{{camera}}` | camera name |
| `{{label}}` | object label (or `gesture`) |
| `{{score}}` | confidence, 3 decimals |
| `{{ts}}` | unix seconds |
| `{{snapshot}}` | snapshot path on this NVR |
| `{{face}}` | recognized name, if any |
| `{{plate}}` | OCR'd plate, if any |
| `{{gesture}}` | recognized hand signal, if any |

Unknown placeholders are left untouched. Example (Slack):

```json
{"text":"📷 {{label}} on {{camera}} ({{score}})"}
```

### Push tap-through links

Set **public base URL** (`public_base_url`) to the address this NVR is reachable
at. ntfy pushes then carry tap-through **View clip** / **Snapshot** action buttons
that deep-link into `/api/events/{id}/clip` and the snapshot.
