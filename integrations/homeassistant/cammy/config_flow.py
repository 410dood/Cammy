"""Config flow for the Cammy NVR integration (v0 skeleton).

Collects the NVR base URL and an API token, verifies them against
``GET /api/cameras``, and creates the config entry.
"""

from __future__ import annotations

from typing import Any

import aiohttp
import voluptuous as vol
from homeassistant.config_entries import ConfigFlow, ConfigFlowResult
from homeassistant.helpers.aiohttp_client import async_get_clientsession

from .const import CAMERAS_PATH, CONF_HOST, CONF_TOKEN, CONF_VERIFY_SSL, DOMAIN

STEP_USER_SCHEMA = vol.Schema(
    {
        vol.Required(CONF_HOST, default="http://192.168.1.10:8080"): str,
        vol.Required(CONF_TOKEN): str,
        vol.Optional(CONF_VERIFY_SSL, default=True): bool,
    }
)


class CammyConfigFlow(ConfigFlow, domain=DOMAIN):
    """Handle the Cammy config flow."""

    VERSION = 1

    async def async_step_user(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        errors: dict[str, str] = {}
        if user_input is not None:
            host = user_input[CONF_HOST].rstrip("/")
            try:
                await self._verify(
                    host, user_input[CONF_TOKEN], user_input[CONF_VERIFY_SSL]
                )
            except aiohttp.ClientResponseError as err:
                errors["base"] = "invalid_auth" if err.status in (401, 403) else "cannot_connect"
            except Exception:  # noqa: BLE001
                errors["base"] = "cannot_connect"
            else:
                await self.async_set_unique_id(host)
                self._abort_if_unique_id_configured()
                return self.async_create_entry(
                    title=f"Cammy ({host})",
                    data={**user_input, CONF_HOST: host},
                )

        return self.async_show_form(
            step_id="user", data_schema=STEP_USER_SCHEMA, errors=errors
        )

    async def _verify(self, host: str, token: str, verify_ssl: bool) -> None:
        """Raise if the host/token can't list cameras."""
        session = async_get_clientsession(self.hass, verify_ssl=verify_ssl)
        async with session.get(
            f"{host}{CAMERAS_PATH}",
            headers={"Authorization": f"Bearer {token}"},
            timeout=15,
        ) as resp:
            resp.raise_for_status()
