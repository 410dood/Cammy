//! P2.9 — deterrence actions (relay-only v0).
//!
//! A camera's ONVIF **relay output** drives an external siren / strobe / light
//! wired to the camera's alarm-out terminal. This module speaks the two ONVIF
//! DeviceIO operations we need — `GetRelayOutputs` (capability probe) and
//! `SetRelayOutputState` (pulse the relay on/off) — reusing the hand-rolled
//! WS-Security SOAP client in [`crate::ptz`]. Nothing here is a general SOAP
//! stack: the surface is two fixed envelopes plus a couple of tolerant string
//! extractions, mirroring `ptz.rs` and `onvif_events.rs`.
//!
//! SAFETY / HONESTY (the whole point of the feature):
//! - The feature is OFF unless the global `deterrence_enabled` master switch is
//!   on — the alarm-dispatch path checks it and short-circuits (see notify.rs).
//! - We probe for REAL relay tokens before offering the action, so the UI never
//!   presents a blind text box that would fire nothing.
//! - Every trigger is fail-soft: a dead / absent / mis-wired relay logs a
//!   warning and is swallowed — it must NEVER break an alarm or stall detection.
//! - There is NO escalation ladder: a rule fires the relay once for a fixed,
//!   capped hold and then releases it.
//!
//! The "play a WAV over the two-way-audio backchannel" half of the original
//! P2.9 spec is deliberately deferred: this codebase has no server-side audio
//! push path (two-way audio is 100% browser-mic WebRTC), so building it blind
//! would be unvalidatable. Relay-only ships now; audio when there's a real path.

use std::time::Duration;

use crate::ptz::{extract_between, soap_call, CamTarget};

/// GetCapabilities with `Category=All` — the DeviceIO service XAddr rides in the
/// `<Extension>` of the "All" capabilities set.
const GET_CAPS_ALL: &str = r#"<GetCapabilities xmlns="http://www.onvif.org/ver10/device/wsdl">
    <Category>All</Category>
  </GetCapabilities>"#;

/// One relay output the camera advertises. `token` is the ONVIF
/// `RelayOutputToken`; `mode` is Bistable/Monostable when the camera reports it.
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct RelayOutput {
    pub token: String,
    pub mode: Option<String>,
}

/// Result of probing a camera for relay-output deterrence capability. Either a
/// non-empty `relays` list (ready to arm) or an honest human-readable `error`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DeterCaps {
    pub relays: Vec<RelayOutput>,
    pub error: Option<String>,
}

impl DeterCaps {
    /// A capability result carrying only an honest failure reason.
    fn err(reason: impl Into<String>) -> Self {
        DeterCaps {
            relays: Vec::new(),
            error: Some(reason.into()),
        }
    }
}

/// Probe a camera for ONVIF relay outputs. Best-effort / never panics: any
/// failure returns an empty relay list plus a human-readable reason so the UI
/// can be honest ("this camera reports no relay output") rather than offer a
/// control that does nothing. Blocking (SOAP over the network) — call it from a
/// `spawn_blocking` in async contexts.
pub fn probe(target: &CamTarget) -> DeterCaps {
    let device_service = format!("http://{}/onvif/device_service", target.host);
    let caps = match soap_call(&device_service, target, GET_CAPS_ALL) {
        Ok(x) => x,
        Err(e) => return DeterCaps::err(format!("could not reach the camera over ONVIF: {e}")),
    };
    // Try the advertised DeviceIO XAddr first, then the device service itself —
    // Hikvision/older cameras serve GetRelayOutputs on device_service with no
    // distinct DeviceIO XAddr. First endpoint with relays wins.
    let mut last_err: Option<String> = None;
    let mut any_ok = false;
    for ep in relay_endpoints(&caps, &device_service) {
        match soap_call(&ep, target, GET_RELAY_OUTPUTS) {
            Ok(resp) => {
                any_ok = true;
                let relays = parse_relay_outputs(&resp);
                if !relays.is_empty() {
                    return DeterCaps {
                        relays,
                        error: None,
                    };
                }
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    if any_ok {
        // The camera answered but advertised no relay outputs.
        DeterCaps::err("camera has no ONVIF relay outputs")
    } else {
        DeterCaps::err(format!(
            "camera rejected the relay-output query: {}",
            last_err.unwrap_or_else(|| "no DeviceIO endpoint".into())
        ))
    }
}

/// `GetRelayOutputs` on the DeviceIO service.
const GET_RELAY_OUTPUTS: &str =
    r#"<GetRelayOutputs xmlns="http://www.onvif.org/ver10/deviceIO/wsdl"/>"#;

/// Set one relay output active/inactive (`SetRelayOutputState`, LogicalState
/// active|inactive). Re-discovers the relay endpoint each call (cheap, and keeps
/// the fn self-contained / stateless like the PTZ commands). Blocking.
pub fn trigger_relay(target: &CamTarget, token: &str, active: bool) -> anyhow::Result<()> {
    let device_service = format!("http://{}/onvif/device_service", target.host);
    let caps = soap_call(&device_service, target, GET_CAPS_ALL)?;
    let xaddr = relay_endpoints(&caps, &device_service)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("camera has no ONVIF DeviceIO relay service"))?;
    let state = if active { "active" } else { "inactive" };
    // The token (and defensively the fixed state) is interpolated into the SOAP
    // body — XML-escape so an injected value can't break out of the element.
    let body = format!(
        r#"<SetRelayOutputState xmlns="http://www.onvif.org/ver10/deviceIO/wsdl">
             <RelayOutputToken>{}</RelayOutputToken>
             <LogicalState>{}</LogicalState>
           </SetRelayOutputState>"#,
        xml_escape(token),
        xml_escape(state),
    );
    soap_call(&xaddr, target, &body).map(|_| ())
}

/// Minimal XML escaping for a value interpolated into a SOAP body element. Legit
/// ONVIF relay tokens carry no metacharacters, so this is a no-op for them — it
/// is defense-in-depth against an injected token breaking the envelope.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Longest a single deterrence pulse may hold the relay closed. Caps a
/// misconfigured `hold` so a bug can't leave a siren wailing.
pub const MAX_HOLD: Duration = Duration::from_secs(10);

/// How hard we try to turn a relay back OFF — a stuck-ON siren is worse than a
/// missed alarm, so retry a few times before giving up (and log at ERROR).
const RELEASE_RETRIES: u32 = 3;
const RELEASE_BACKOFF: Duration = Duration::from_millis(500);

/// Turn a relay OFF, retrying before giving up. Escalates to ERROR if every
/// attempt fails so an operator knows a siren/light may be stuck energized.
pub fn release_relay(target: &CamTarget, token: &str) {
    for attempt in 1..=RELEASE_RETRIES {
        match trigger_relay(target, token, false) {
            Ok(()) => {
                tracing::info!(relay = %token, "deterrence relay released");
                return;
            }
            Err(e) => {
                tracing::warn!(relay = %token, attempt, "deterrence relay OFF failed, retrying: {e:#}");
                if attempt < RELEASE_RETRIES {
                    std::thread::sleep(RELEASE_BACKOFF);
                }
            }
        }
    }
    tracing::error!(
        relay = %token,
        "deterrence relay OFF failed after {RELEASE_RETRIES} tries — a siren/light may be stuck ON"
    );
}

/// Pulse a relay: fire ON, hold for `hold` (capped at [`MAX_HOLD`]), then release
/// OFF (with retries). ALL of it — including the ON call — runs on a detached
/// thread, so the ~16s worst-case ON (GetCapabilities + Set against a slow
/// camera) never blocks `notify::fire` on the single hot detection thread; the
/// few-ms delay to siren activation is acceptable.
///
/// Fully fail-soft: an ON failure is `warn!`-logged and swallowed so a dead or
/// mis-wired relay never breaks an alarm; a persistent OFF failure escalates to
/// ERROR (see [`release_relay`]). Only the relay `token` id is logged directly —
/// never the credentials (mirrors ptz.rs; a host IP may still surface inside a
/// warn-logged transport error chain).
pub fn pulse_relay_async(target: CamTarget, token: String, hold: Duration) {
    let hold = hold.min(MAX_HOLD);
    std::thread::spawn(move || {
        if let Err(e) = trigger_relay(&target, &token, true) {
            tracing::warn!(relay = %token, "deterrence relay ON failed: {e:#}");
            return; // nothing to release if it never engaged
        }
        tracing::info!(relay = %token, hold_secs = hold.as_secs(), "deterrence relay pulsed on");
        std::thread::sleep(hold);
        release_relay(&target, &token);
    });
}

/// The DeviceIO service address from a `GetCapabilities` (Category=All) body.
/// Mirrors `ptz::ptz_xaddr`: the XAddr inside the DeviceIO capability element.
/// Matches both a namespaced (`<tt:DeviceIO>`) and a prefix-less (`<DeviceIO>`)
/// opening tag — vendors differ. Tolerant / best-effort; returns `None` when the
/// camera advertises no DeviceIO service OR the XAddr is empty. Pure →
/// unit-tested.
pub(crate) fn parse_deviceio_xaddr(caps_xml: &str) -> Option<String> {
    let section = caps_xml
        .split_once(":DeviceIO>")
        .or_else(|| caps_xml.split_once("<DeviceIO>"))
        .map(|(_, after)| after)?;
    let xaddr = extract_between(section, "XAddr>", "</")?.trim().to_string();
    (!xaddr.is_empty()).then_some(xaddr)
}

/// Ordered candidate endpoints for `GetRelayOutputs`: the advertised DeviceIO
/// XAddr first (when present + non-empty), then the device service itself as a
/// fallback — Hikvision/older cameras serve the relay operations there with no
/// distinct DeviceIO XAddr. Pure → unit-tested.
pub(crate) fn relay_endpoints(caps_xml: &str, device_service: &str) -> Vec<String> {
    let mut endpoints = Vec::new();
    if let Some(xaddr) = parse_deviceio_xaddr(caps_xml) {
        endpoints.push(xaddr);
    }
    if !endpoints.iter().any(|e| e == device_service) {
        endpoints.push(device_service.to_string());
    }
    endpoints
}

/// Every relay output token (+ mode when present) in a `GetRelayOutputsResponse`.
/// Splitting on the `token="` attribute bounds each relay's slice, so the
/// following `<...:Mode>…</…>` belongs to that relay. Vendor-tolerant and never
/// panics (a zero-relay or garbage response yields an empty Vec). Pure →
/// unit-tested.
pub(crate) fn parse_relay_outputs(xml: &str) -> Vec<RelayOutput> {
    let mut out = Vec::new();
    for chunk in xml.split("token=\"").skip(1) {
        let Some(token) = chunk.split('"').next().filter(|t| !t.is_empty()) else {
            continue;
        };
        // `chunk` runs up to (but not including) the next relay's token attribute,
        // so a Mode found here is this relay's — or absent (best-effort).
        let mode = extract_between(chunk, "Mode>", "</")
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        out.push(RelayOutput {
            token: token.to_string(),
            mode,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A Category=All GetCapabilities response with DeviceIO carrying its XAddr in
    // the Extension (where relay-capable cameras advertise it).
    const CAPS_WITH_DEVICEIO: &str = r#"<tds:GetCapabilitiesResponse>
      <tds:Capabilities>
        <tt:Device><tt:XAddr>http://192.168.1.50/onvif/device_service</tt:XAddr></tt:Device>
        <tt:Media><tt:XAddr>http://192.168.1.50/onvif/media_service</tt:XAddr></tt:Media>
        <tt:PTZ><tt:XAddr>http://192.168.1.50/onvif/ptz_service</tt:XAddr></tt:PTZ>
        <tt:Extension>
          <tt:DeviceIO>
            <tt:XAddr>http://192.168.1.50/onvif/deviceIO_service</tt:XAddr>
            <tt:VideoSources>1</tt:VideoSources>
            <tt:RelayOutputs>2</tt:RelayOutputs>
          </tt:DeviceIO>
        </tt:Extension>
      </tds:Capabilities>
    </tds:GetCapabilitiesResponse>"#;

    #[test]
    fn finds_the_deviceio_xaddr_in_capabilities() {
        assert_eq!(
            parse_deviceio_xaddr(CAPS_WITH_DEVICEIO).as_deref(),
            Some("http://192.168.1.50/onvif/deviceIO_service")
        );
    }

    #[test]
    fn no_deviceio_capability_yields_none() {
        // A camera without relay hardware simply omits the DeviceIO element.
        let caps = r#"<tds:Capabilities>
          <tt:Device><tt:XAddr>http://h/onvif/device_service</tt:XAddr></tt:Device>
          <tt:PTZ><tt:XAddr>http://h/onvif/ptz_service</tt:XAddr></tt:PTZ>
        </tds:Capabilities>"#;
        assert_eq!(parse_deviceio_xaddr(caps), None);
    }

    #[test]
    fn finds_a_prefix_less_deviceio_xaddr() {
        // Some cameras emit an un-namespaced <DeviceIO> element.
        let caps = r#"<Capabilities>
          <DeviceIO>
            <XAddr>http://10.0.0.9/onvif/deviceIO_service</XAddr>
            <RelayOutputs>1</RelayOutputs>
          </DeviceIO>
        </Capabilities>"#;
        assert_eq!(
            parse_deviceio_xaddr(caps).as_deref(),
            Some("http://10.0.0.9/onvif/deviceIO_service")
        );
    }

    #[test]
    fn relay_endpoints_prefers_deviceio_then_falls_back_to_device_service() {
        let device_service = "http://192.168.1.50/onvif/device_service";
        // DeviceIO advertised → try it first, then the device service.
        assert_eq!(
            relay_endpoints(CAPS_WITH_DEVICEIO, device_service),
            vec![
                "http://192.168.1.50/onvif/deviceIO_service".to_string(),
                device_service.to_string(),
            ]
        );
        // No DeviceIO (Hikvision/older) → fall back to the device service only.
        let no_deviceio = r#"<tds:Capabilities>
          <tt:Device><tt:XAddr>http://192.168.1.50/onvif/device_service</tt:XAddr></tt:Device>
        </tds:Capabilities>"#;
        assert_eq!(
            relay_endpoints(no_deviceio, device_service),
            vec![device_service.to_string()]
        );
    }

    #[test]
    fn xml_escapes_metacharacters_but_is_a_noop_for_real_tokens() {
        // A legitimate ONVIF token is unchanged.
        assert_eq!(xml_escape("RelayOutputToken_1"), "RelayOutputToken_1");
        // Injected markup is neutralized so it can't break out of the element.
        assert_eq!(
            xml_escape(r#"</RelayOutputToken><Evil>&"'"#),
            "&lt;/RelayOutputToken&gt;&lt;Evil&gt;&amp;&quot;&apos;"
        );
    }

    #[test]
    fn parses_relay_output_tokens_and_modes() {
        let xml = r#"<tds:GetRelayOutputsResponse>
          <tds:RelayOutputs token="RelayOutputToken_0">
            <tt:Properties>
              <tt:Mode>Bistable</tt:Mode>
              <tt:IdleState>closed</tt:IdleState>
            </tt:Properties>
          </tds:RelayOutputs>
          <tds:RelayOutputs token="RelayOutputToken_1">
            <tt:Properties>
              <tt:Mode>Monostable</tt:Mode>
              <tt:DelayTime>PT5S</tt:DelayTime>
            </tt:Properties>
          </tds:RelayOutputs>
        </tds:GetRelayOutputsResponse>"#;
        let relays = parse_relay_outputs(xml);
        assert_eq!(relays.len(), 2);
        assert_eq!(relays[0].token, "RelayOutputToken_0");
        assert_eq!(relays[0].mode.as_deref(), Some("Bistable"));
        assert_eq!(relays[1].token, "RelayOutputToken_1");
        assert_eq!(relays[1].mode.as_deref(), Some("Monostable"));
    }

    #[test]
    fn zero_relay_response_is_empty_not_a_panic() {
        let xml = r#"<tds:GetRelayOutputsResponse></tds:GetRelayOutputsResponse>"#;
        assert!(parse_relay_outputs(xml).is_empty());
    }

    #[test]
    fn a_relay_without_a_mode_still_parses() {
        // Some cameras omit <Mode>; the token must still come through.
        let xml = r#"<tds:GetRelayOutputsResponse>
          <tds:RelayOutputs token="Relay_A"/>
        </tds:GetRelayOutputsResponse>"#;
        let relays = parse_relay_outputs(xml);
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].token, "Relay_A");
        assert!(relays[0].mode.is_none());
    }

    #[test]
    fn garbage_response_yields_no_relays() {
        assert!(parse_relay_outputs("not xml at all, no token here").is_empty());
        assert!(parse_relay_outputs("").is_empty());
    }
}
