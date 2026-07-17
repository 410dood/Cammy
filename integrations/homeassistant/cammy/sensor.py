"""Cammy "last event" sensor (v0 skeleton).

One sensor per config entry whose state is the label of the most recent event
and whose attributes carry the full decoded event.
"""

from __future__ import annotations

from homeassistant.components.sensor import SensorEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN, SIGNAL_EVENT


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    async_add_entities([CammyLastEventSensor(entry.entry_id)])


class CammyLastEventSensor(SensorEntity):
    """State = last event label; attributes = the full event payload."""

    _attr_has_entity_name = True
    _attr_name = "Last event"
    _attr_icon = "mdi:cctv"
    _attr_should_poll = False

    def __init__(self, entry_id: str) -> None:
        self._attr_unique_id = f"{entry_id}_last_event"
        self._state: str | None = None
        self._attrs: dict = {}

    async def async_added_to_hass(self) -> None:
        self.async_on_remove(
            async_dispatcher_connect(self.hass, SIGNAL_EVENT, self._handle_event)
        )

    @callback
    def _handle_event(self, event: dict) -> None:
        self._state = event.get("label")
        self._attrs = event
        self.async_write_ha_state()

    @property
    def native_value(self) -> str | None:
        return self._state

    @property
    def extra_state_attributes(self) -> dict:
        return self._attrs
