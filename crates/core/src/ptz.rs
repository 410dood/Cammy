//! ONVIF PTZ control: ContinuousMove / Stop over SOAP with WS-UsernameToken
//! digest auth. Hand-rolled rather than pulling a SOAP stack — the protocol
//! surface we need is three fixed envelopes and a couple of string extractions.
//!
//! Camera identity (host + credentials) is parsed from the camera's go2rtc
//! source URL (onvif:// or rtsp://), so PTZ works for cameras added via the
//! ONVIF resolve flow and for hand-entered RTSP URLs alike.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use sha1::{Digest, Sha1};

pub struct CamTarget {
    pub host: String,
    pub username: String,
    pub password: String,
    /// Media profile token if the source URL carries one (onvif://...?subtype=X).
    pub profile_hint: Option<String>,
}

/// Pull host + credentials (+ profile token) out of a camera source URL.
pub fn parse_source(source: &str) -> Option<CamTarget> {
    let rest = source
        .strip_prefix("onvif://")
        .or_else(|| source.strip_prefix("rtsp://"))?;
    let (userinfo, after_at) = rest.split_once('@')?;
    let (user, pass) = userinfo.split_once(':')?;
    // host ends at first ':' (port), '/' (path) or '?' (query)
    let host_end = after_at.find([':', '/', '?']).unwrap_or(after_at.len());
    let host = &after_at[..host_end];
    let profile_hint = after_at
        .split_once('?')
        .map(|(_, q)| q)
        .and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("subtype=").map(str::to_string))
        })
        .filter(|t| !t.is_empty() && t.starts_with("MediaProfile"));
    Some(CamTarget {
        host: host.to_string(),
        username: urldecode(user),
        password: urldecode(pass),
        profile_hint,
    })
}

/// True when the device advertises a PTZ service.
pub fn supports_ptz(t: &CamTarget) -> bool {
    ptz_xaddr(t).is_ok()
}

pub fn continuous_move(t: &CamTarget, pan: f32, tilt: f32, zoom: f32) -> Result<()> {
    let xaddr = ptz_xaddr(t)?;
    let token = profile_token(t)?;
    let body = format!(
        r#"<ContinuousMove xmlns="http://www.onvif.org/ver20/ptz/wsdl">
             <ProfileToken>{token}</ProfileToken>
             <Velocity>
               <PanTilt x="{pan}" y="{tilt}" xmlns="http://www.onvif.org/ver10/schema"/>
               <Zoom x="{zoom}" xmlns="http://www.onvif.org/ver10/schema"/>
             </Velocity>
           </ContinuousMove>"#
    );
    soap_call(&xaddr, t, &body).map(|_| ())
}

pub fn stop(t: &CamTarget) -> Result<()> {
    let xaddr = ptz_xaddr(t)?;
    let token = profile_token(t)?;
    let body = format!(
        r#"<Stop xmlns="http://www.onvif.org/ver20/ptz/wsdl">
             <ProfileToken>{token}</ProfileToken>
             <PanTilt>true</PanTilt><Zoom>true</Zoom>
           </Stop>"#
    );
    soap_call(&xaddr, t, &body).map(|_| ())
}

/// PTZ service address from GetCapabilities; errors if the device has none.
fn ptz_xaddr(t: &CamTarget) -> Result<String> {
    let device_service = format!("http://{}/onvif/device_service", t.host);
    let body = r#"<GetCapabilities xmlns="http://www.onvif.org/ver10/device/wsdl">
                    <Category>All</Category>
                  </GetCapabilities>"#;
    let resp = soap_call(&device_service, t, body)?;
    // Cheap extraction: the XAddr inside the <PTZ> capability element.
    let ptz_section = resp
        .split_once(":PTZ>")
        .map(|(_, after)| after)
        .context("device reports no PTZ capability")?;
    let xaddr = extract_between(ptz_section, "XAddr>", "</").context("PTZ XAddr missing")?;
    Ok(xaddr.trim().to_string())
}

/// Media profile token: trust the source URL hint, else ask the device.
fn profile_token(t: &CamTarget) -> Result<String> {
    if let Some(hint) = &t.profile_hint {
        return Ok(hint.clone());
    }
    let media_service = format!("http://{}/onvif/media_service", t.host);
    let body = r#"<GetProfiles xmlns="http://www.onvif.org/ver10/media/wsdl"/>"#;
    let resp = soap_call(&media_service, t, body)
        .or_else(|_| soap_call(&format!("http://{}/onvif/device_service", t.host), t, body))?;
    let token = extract_between(&resp, "token=\"", "\"").context("no media profiles")?;
    Ok(token.to_string())
}

/// One SOAP 1.2 request with WS-UsernameToken digest auth.
fn soap_call(url: &str, t: &CamTarget, body: &str) -> Result<String> {
    let envelope = envelope(&t.username, &t.password, body);
    let resp = ureq::post(url)
        .timeout(Duration::from_secs(8))
        .set("Content-Type", "application/soap+xml; charset=utf-8")
        .send_string(&envelope);
    match resp {
        Ok(r) => Ok(r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            bail!(
                "ONVIF {code} from {url}: {}",
                extract_between(&detail, "<soap:Text", "</")
                    .map(|s| s.split_once('>').map(|(_, t)| t).unwrap_or(s))
                    .unwrap_or("(no fault detail)")
            )
        }
        Err(e) => bail!("ONVIF request to {url} failed: {e}"),
    }
}

/// SOAP 1.2 envelope with WS-Security UsernameToken (PasswordDigest =
/// base64(sha1(nonce || created || password))).
fn envelope(username: &str, password: &str, body: &str) -> String {
    let nonce: [u8; 16] = rand::random();
    let created = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let digest = Sha1::new()
        .chain_update(nonce)
        .chain_update(created.as_bytes())
        .chain_update(password.as_bytes())
        .finalize();
    let b64 = base64::engine::general_purpose::STANDARD;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope">
  <s:Header>
    <Security s:mustUnderstand="1" xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd">
      <UsernameToken>
        <Username>{username}</Username>
        <Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{}</Password>
        <Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{}</Nonce>
        <Created xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">{created}</Created>
      </UsernameToken>
    </Security>
  </s:Header>
  <s:Body>{body}</s:Body>
</s:Envelope>"#,
        b64.encode(digest),
        b64.encode(nonce),
    )
}

/// A camera found by WS-Discovery.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Discovered {
    pub host: String,
    pub name: Option<String>,
}

/// ONVIF WS-Discovery: multicast a Probe to 239.255.255.250:3702 and collect
/// responders for `timeout`. This is how Blue Iris "Find" and Synology's
/// camera search work under the hood.
///
/// One probe socket per local IPv4 interface: on multi-homed machines (WSL /
/// Hyper-V / VPN virtual adapters are near-universal on Windows) a 0.0.0.0
/// bind sends the multicast out whichever interface the OS picks, which is
/// often NOT the LAN where the cameras live.
pub fn ws_discover(timeout: Duration) -> Result<Vec<Discovered>> {
    use std::net::UdpSocket;

    let mut sockets = Vec::new();
    for ip in local_ipv4s() {
        let Ok(socket) = UdpSocket::bind((ip, 0)) else {
            continue;
        };
        let _ = socket.set_multicast_ttl_v4(2);
        socket.set_nonblocking(true)?;
        let msg_id: [u8; 16] = rand::random();
        let probe = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope"
            xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing"
            xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
            xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
  <e:Header>
    <w:MessageID>uuid:{}</w:MessageID>
    <w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>
    <w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>
  </e:Header>
  <e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body>
</e:Envelope>"#,
            hex16(&msg_id)
        );
        if socket
            .send_to(probe.as_bytes(), ("239.255.255.250", 3702))
            .is_ok()
        {
            sockets.push(socket);
        }
    }
    if sockets.is_empty() {
        bail!("no usable network interface for discovery");
    }

    let deadline = std::time::Instant::now() + timeout;
    let mut seen = std::collections::BTreeMap::<String, Option<String>>::new();
    let mut buf = [0u8; 16384];
    while std::time::Instant::now() < deadline {
        let mut idle = true;
        for socket in &sockets {
            let Ok((n, addr)) = socket.recv_from(&mut buf) else {
                continue;
            };
            idle = false;
            let body = String::from_utf8_lossy(&buf[..n]).into_owned();
            // Camera "name" scope: onvif://www.onvif.org/name/<urlencoded name>
            let name = body
                .split("onvif://www.onvif.org/name/")
                .nth(1)
                .map(|rest| {
                    let end = rest.find([' ', '<', '"']).unwrap_or(rest.len());
                    urldecode(&rest[..end]).replace('_', " ")
                })
                .filter(|s| !s.is_empty());
            seen.entry(addr.ip().to_string()).or_insert(name);
        }
        if idle {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(seen
        .into_iter()
        .map(|(host, name)| Discovered { host, name })
        .collect())
}

/// Local IPv4 addresses to probe from: the default-route interface (UDP
/// connect trick) plus everything the machine's hostname resolves to.
fn local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs, UdpSocket};

    let mut out = std::collections::BTreeSet::new();
    if let Ok(s) = UdpSocket::bind(("0.0.0.0", 0)) {
        if s.connect(("8.8.8.8", 80)).is_ok() {
            if let Ok(addr) = s.local_addr() {
                if let IpAddr::V4(v4) = addr.ip() {
                    out.insert(v4);
                }
            }
        }
    }
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    if !host.is_empty() {
        if let Ok(addrs) = (host.as_str(), 0u16).to_socket_addrs() {
            for addr in addrs {
                if let IpAddr::V4(v4) = addr.ip() {
                    if !v4.is_loopback() {
                        out.insert(v4);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.insert(Ipv4Addr::UNSPECIFIED);
    }
    out.into_iter().collect()
}

fn hex16(bytes: &[u8; 16]) -> String {
    let h = crate::util::hex(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

fn extract_between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = haystack.find(start)? + start.len();
    let len = haystack[from..].find(end)?;
    Some(&haystack[from..from + len])
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_onvif_source_with_profile() {
        let t = parse_source("onvif://admin:p%40ss@192.168.1.133?subtype=MediaProfile000").unwrap();
        assert_eq!(t.host, "192.168.1.133");
        assert_eq!(t.username, "admin");
        assert_eq!(t.password, "p@ss");
        assert_eq!(t.profile_hint.as_deref(), Some("MediaProfile000"));
    }

    #[test]
    fn parses_rtsp_source_without_profile() {
        let t = parse_source("rtsp://admin:secret@192.168.1.134:554/cam/realmonitor?channel=1")
            .unwrap();
        assert_eq!(t.host, "192.168.1.134");
        assert_eq!(t.password, "secret");
        assert!(t.profile_hint.is_none());
    }

    #[test]
    fn rejects_sources_without_credentials() {
        assert!(parse_source("exec:ffmpeg -i x -f rtsp {output}").is_none());
        assert!(parse_source("rtsp://192.168.1.10/stream").is_none());
    }

    #[test]
    fn extract_between_finds_first_span() {
        assert_eq!(extract_between("<a>hi</a>", "<a>", "</"), Some("hi"));
        assert_eq!(extract_between("nope", "<a>", "</"), None);
    }
}
