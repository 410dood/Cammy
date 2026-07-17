"""Cammy NVR — Home Assistant custom integration (v0 SKELETON).

This is a STARTING POINT, not a finished, HA-tested integration. It has been
written against Cammy's real, shipped HTTP API but has NOT been run on a live
Home Assistant instance — treat every entity/flow below as a template to verify
and adjust. See README.md.

What it does:
  * Opens the Cammy live event feed (Server-Sent Events, ``GET /api/events/stream``)
    with a Bearer API token and keeps it connected, reconnecting on drop.
  * Re-dispatches each decoded event to entities via the HA dispatcher.
  * Exposes a "last event" sensor and a motion binary_sensor per camera.
"""

from __future__ import annotations

import asyncio
import json
import logging

import aiohttp
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.dispatcher import async_dispatcher_send

from .const import (
    CAMERAS_PATH,
    CONF_HOST,
    CONF_TOKEN,
    CONF_VERIFY_SSL,
    DOMAIN,
    EVENTS_STREAM_PATH,
    PLATFORMS,
    SIGNAL_EVENT,
)

_LOGGER = logging.getLogger(__name__)


class CammyClient:
    """Owns the SSE connection and the list of cameras for one config entry."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry) -> None:
        self.hass = hass
        self.entry = entry
        self.host: str = entry.data[CONF_HOST].rstrip("/")
        self.token: str = entry.data[CONF_TOKEN]
        self.verify_ssl: bool = entry.data.get(CONF_VERIFY_SSL, True)
        self.cameras: list[str] = []
        self.last_event: dict | None = None
        self._task: asyncio.Task | None = None
        self._stop = asyncio.Event()

    @property
    def _headers(self) -> dict[str, str]:
        return {"Authorization": f"Bearer {self.token}"}

    async def async_load_cameras(self) -> None:
        """Fetch camera names so we can create one motion sensor per camera."""
        session = async_get_clientsession(self.hass, verify_ssl=self.verify_ssl)
        async with session.get(
            f"{self.host}{CAMERAS_PATH}", headers=self._headers, timeout=15
        ) as resp:
            resp.raise_for_status()
            data = await resp.json()
        self.cameras = [c["name"] for c in data if "name" in c]

    def start(self) -> None:
        self._task = self.entry.async_create_background_task(
            self.hass, self._run(), name=f"{DOMAIN}_sse"
        )

    async def async_stop(self) -> None:
        self._stop.set()
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass

    async def _run(self) -> None:
        """Consume the SSE feed forever, reconnecting with backoff on error."""
        session = async_get_clientsession(self.hass, verify_ssl=self.verify_ssl)
        backoff = 2
        while not self._stop.is_set():
            try:
                await self._consume(session)
                backoff = 2  # a clean end (server closed) resets the backoff
            except asyncio.CancelledError:
                raise
            except Exception as err:  # noqa: BLE001 — skeleton: log + retry anything
                _LOGGER.warning("Cammy SSE connection error: %s", err)
            if self._stop.is_set():
                break
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 60)

    async def _consume(self, session: aiohttp.ClientSession) -> None:
        url = f"{self.host}{EVENTS_STREAM_PATH}"
        async with session.get(url, headers=self._headers, timeout=None) as resp:
            resp.raise_for_status()
            _LOGGER.debug("Cammy SSE connected: %s", url)
            async for raw in resp.content:
                if self._stop.is_set():
                    return
                line = raw.decode("utf-8", "replace").rstrip("\n").rstrip("\r")
                # SSE: only "data:" lines carry the JSON payload; ":" comments are
                # keep-alives and blank lines terminate an event — both ignored.
                if not line.startswith("data:"):
                    continue
                payload = line[len("data:") :].strip()
                if not payload:
                    continue
                try:
                    event = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                self.last_event = event
                async_dispatcher_send(self.hass, SIGNAL_EVENT, event)


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Set up Cammy from a config entry."""
    client = CammyClient(hass, entry)
    try:
        await client.async_load_cameras()
    except Exception as err:  # noqa: BLE001
        _LOGGER.error("Cammy: could not reach %s: %s", client.host, err)
        raise

    hass.data.setdefault(DOMAIN, {})[entry.entry_id] = client
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    client.start()
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Unload a config entry."""
    unload_ok = await hass.config_entries.async_unload_platforms(entry, PLATFORMS)
    client: CammyClient = hass.data[DOMAIN].pop(entry.entry_id)
    await client.async_stop()
    return unload_ok
