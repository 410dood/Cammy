"""Constants for the Cammy NVR integration (v0 skeleton)."""

DOMAIN = "cammy"

# Config-entry keys.
CONF_HOST = "host"          # e.g. "http://192.168.1.10:8080" (no trailing slash)
CONF_TOKEN = "token"        # a Cammy API token: Settings -> API tokens ("zoomy_<hex>")
CONF_VERIFY_SSL = "verify_ssl"

# Platforms this integration sets up.
PLATFORMS = ["sensor", "binary_sensor"]

# The live event feed and a couple of read endpoints the entities use.
EVENTS_STREAM_PATH = "/api/events/stream"
CAMERAS_PATH = "/api/cameras"

# Dispatcher signal fired for every event pulled off the SSE feed. Payload is the
# decoded {event_id, camera, label, score, ts, snapshot} dict.
SIGNAL_EVENT = f"{DOMAIN}_event"

# How long a per-camera motion binary_sensor stays "on" after its last event
# before it auto-clears (seconds). Mirrors Cammy's own mqtt_state_timeout_secs.
MOTION_OFF_DELAY = 30
