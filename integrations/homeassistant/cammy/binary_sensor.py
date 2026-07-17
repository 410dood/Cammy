"""Cammy per-camera motion binary_sensor (v0 skeleton).

One ``motion`` binary_sensor per camera (discovered at setup from
``GET /api/cameras``). It turns ON when an event for that camera arrives on the
live feed and auto-clears after ``MOTION_OFF_DELAY`` seconds of quiet.
"""

from __future__ import annotations

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.event import async_call_later

from .const import DOMAIN, MOTION_OFF_DELAY, SIGNAL_EVENT


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    client = hass.data[DOMAIN][entry.entry_id]
    async_add_entities(
        CammyMotionSensor(entry.entry_id, name) for name in client.cameras
    )


class CammyMotionSensor(BinarySensorEntity):
    """Motion for one camera, driven by the live event feed."""

    _attr_has_entity_name = True
    _attr_device_class = BinarySensorDeviceClass.MOTION
    _attr_should_poll = False

    def __init__(self, entry_id: str, camera: str) -> None:
        self._camera = camera
        self._attr_name = camera
        self._attr_unique_id = f"{entry_id}_{camera}_motion"
        self._on = False
        self._cancel_off = None

    async def async_added_to_hass(self) -> None:
        self.async_on_remove(
            async_dispatcher_connect(self.hass, SIGNAL_EVENT, self._handle_event)
        )

    @callback
    def _handle_event(self, event: dict) -> None:
        if event.get("camera") != self._camera:
            return
        self._on = True
        self.async_write_ha_state()
        if self._cancel_off:
            self._cancel_off()
        self._cancel_off = async_call_later(self.hass, MOTION_OFF_DELAY, self._clear)

    @callback
    def _clear(self, _now) -> None:
        self._on = False
        self._cancel_off = None
        self.async_write_ha_state()

    @property
    def is_on(self) -> bool:
        return self._on
