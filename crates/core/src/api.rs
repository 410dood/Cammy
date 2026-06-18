//! HTTP API consumed by the web UI (and anything else — it's plain JSON).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use tower::ServiceExt as _;
use tower_http::services::ServeFile;

use crate::db::{Camera, Db, Settings};
use crate::go2rtc::Go2Rtc;
use crate::status::StatusBoard;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub go2rtc: Arc<Go2Rtc>,
    pub snapshots_dir: PathBuf,
    pub clips_dir: PathBuf,
    pub faces_dir: PathBuf,
    pub recordings_dir_default: PathBuf,
    pub ffmpeg_bin: Option<PathBuf>,
    pub status: StatusBoard,
    pub sessions: crate::auth::Sessions,
    /// Per-IP login brute-force throttle (off-LAN hardening).
    pub login_throttle: crate::auth::LoginThrottle,
    /// True when the server is reachable over HTTPS, so session cookies get the
    /// `Secure` attribute.
    pub tls: bool,
    /// Trust `X-Forwarded-For` from a same-host reverse proxy for client-IP
    /// identification (auth exemption + throttle keying).
    pub behind_proxy: bool,
    /// Lets request handlers (the hand-signal recognizer) publish events and
    /// fire alarm actions on the same channel the detection pipeline uses.
    pub mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    /// Shared per-rule cooldown clock, so API-fired alarms respect the same
    /// throttle as pipeline/audio-fired ones.
    pub alarm_throttle: crate::notify::AlarmThrottle,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route("/api/auth", get(auth_status))
        .route("/api/auth/password", axum::routing::post(set_password))
        .route("/api/login", axum::routing::post(login))
        .route("/api/status", get(camera_status))
        .route("/api/cameras", get(list_cameras).post(add_camera))
        .route("/api/discover", axum::routing::post(discover))
        .route("/api/discover/scan", get(discover_scan))
        .route(
            "/api/cameras/{id}",
            get(get_camera).patch(patch_camera).delete(delete_camera),
        )
        .route("/api/cameras/{id}/ptz", get(ptz_caps).post(ptz_command))
        .route("/api/cameras/{id}/frame.jpg", get(camera_frame))
        .route("/api/events", get(list_events))
        .route("/api/gesture", axum::routing::post(record_gesture))
        .route("/api/events/{id}/clip", get(event_clip))
        .route("/api/search", get(smart_search))
        .route("/api/alarms", get(list_alarms_api).post(add_alarm_api))
        .route(
            "/api/alarms/{id}",
            axum::routing::patch(patch_alarm_api).delete(delete_alarm_api),
        )
        .route("/api/faces", get(faces_overview).post(enroll_face))
        .route(
            "/api/faces/{id}",
            axum::routing::patch(rename_face_api).delete(delete_face_api),
        )
        .route("/api/faces/unknown/{file}", get(unknown_face_img))
        .route("/api/snapshots/{file}", get(snapshot))
        .route("/api/recordings", get(list_recordings))
        .route("/api/recordings/at", get(recording_at))
        .route("/api/recordings/{id}/video", get(segment_video))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/stats", get(stats))
        .route("/api/backup", get(backup))
        .route("/api/restore", axum::routing::post(restore))
        .route("/api/player/{file}", get(go2rtc_player))
        .with_state(state)
}

/// anyhow -> 500 with the error chain in the body (it's a self-hosted LAN app;
/// surfacing real errors beats opaque codes).
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

type ApiResult<T> = Result<T, ApiError>;

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// Tells the UI where go2rtc's WebRTC endpoints live.
async fn config(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "go2rtc_base": st.go2rtc.api_base() }))
}

/// Per-camera health: frame freshness from the detection pipeline + recorder
/// liveness. `online` means a frame arrived within the last 3 poll intervals,
/// or (for detect-off cameras) the recorder is alive.
async fn camera_status(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let now = chrono::Local::now().timestamp();
    let window = (st.db.settings().poll_ms as i64 * 3) / 1000 + 5;
    let mut out = serde_json::Map::new();
    for cam in st.db.list_cameras()? {
        let h = st
            .status
            .snapshot()
            .get(&cam.id)
            .cloned()
            .unwrap_or_default();
        let fresh_frame = h.last_frame_ts.map(|t| now - t <= window).unwrap_or(false);
        let online = if cam.detect { fresh_frame } else { h.recording };
        out.insert(
            cam.id.to_string(),
            serde_json::json!({
                "online": online && cam.enabled,
                "recording": h.recording,
                "last_frame_ts": h.last_frame_ts,
                "last_error": h.last_error,
                "inference_ms": h.inference_ms,
                "accelerator": h.accelerator,
                "model": h.model,
            }),
        );
    }
    Ok(Json(serde_json::Value::Object(out)))
}

// --- auth -------------------------------------------------------------------

async fn auth_status(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "enabled": st.db.get_kv(crate::auth::KV_PASSWORD).is_some() }))
}

#[derive(Deserialize)]
struct PasswordReq {
    password: String,
}

/// Set (or clear, with an empty string) the remote-access password. Existing
/// sessions are invalidated either way.
async fn set_password(
    State(st): State<AppState>,
    Json(req): Json<PasswordReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let pw = req.password.trim();
    if pw.is_empty() {
        st.db.delete_kv(crate::auth::KV_PASSWORD)?;
    } else {
        if pw.len() < 6 {
            return Err(bad_request("password must be at least 6 characters"));
        }
        st.db
            .set_kv(crate::auth::KV_PASSWORD, &crate::auth::hash_password(pw))?;
    }
    st.sessions.clear();
    Ok(Json(serde_json::json!({ "enabled": !pw.is_empty() })))
}

async fn login(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PasswordReq>,
) -> ApiResult<Response> {
    let Some(stored) = st.db.get_kv(crate::auth::KV_PASSWORD) else {
        return Ok(
            Json(serde_json::json!({ "ok": true, "note": "auth disabled" })).into_response(),
        );
    };
    // Identify the peer the same way the auth middleware does, so the throttle
    // keys on the real client even behind a trusted reverse proxy.
    let (peer_ip, _) = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy);
    // Brute-force lockout (loopback is exempt inside the throttle).
    if let Some(remaining) = st.login_throttle.locked_for(peer_ip) {
        let secs = remaining.as_secs().max(1);
        let mut resp = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many attempts; try again later" })),
        )
            .into_response();
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            secs.to_string().parse().expect("numeric retry-after"),
        );
        return Ok(resp);
    }
    if !crate::auth::verify_password(&stored, &req.password) {
        st.login_throttle.record_failure(peer_ip);
        return Err(ApiError(StatusCode::UNAUTHORIZED, "wrong password".into()));
    }
    st.login_throttle.record_success(peer_ip);
    // Upgrade legacy SHA-256 records to argon2id now that we have the plaintext.
    if crate::auth::needs_rehash(&stored) {
        let _ = st.db.set_kv(
            crate::auth::KV_PASSWORD,
            &crate::auth::hash_password(&req.password),
        );
    }
    let token = crate::auth::new_token();
    st.sessions.insert(token.clone());
    let mut resp = Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        crate::auth::session_cookie(&token, st.tls)
            .parse()
            .expect("valid cookie header"),
    );
    Ok(resp)
}

#[derive(Deserialize)]
struct DiscoverReq {
    host: String,
    username: String,
    password: String,
}

/// Resolve a camera's stream profiles from IP + credentials via go2rtc's
/// ONVIF client ("reuse, don't rebuild"). The returned onvif:// URLs are
/// valid go2rtc sources; by convention profile 0 is the main stream and
/// profile 1 the low-res sub-stream.
async fn discover(
    State(st): State<AppState>,
    Json(req): Json<DiscoverReq>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.host.trim().is_empty() {
        return Err(bad_request("host required"));
    }
    let onvif_src = format!(
        "onvif://{}:{}@{}",
        urlencode(&req.username),
        urlencode(&req.password),
        req.host.trim()
    );
    let url = format!(
        "{}/api/onvif?src={}",
        st.go2rtc.api_base(),
        urlencode(&onvif_src)
    );
    let body: serde_json::Value = tokio::task::spawn_blocking(move || {
        ureq::get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .map_err(|e| anyhow::anyhow!("ONVIF probe failed: {e}"))?
            .into_json()
            .map_err(|e| anyhow::anyhow!("bad ONVIF response: {e}"))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(body))
}

/// Scan the LAN for ONVIF cameras (WS-Discovery multicast, ~2.5s).
async fn discover_scan(State(_st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let found = tokio::task::spawn_blocking(|| {
        crate::ptz::ws_discover(std::time::Duration::from_millis(2500))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(serde_json::json!({ "cameras": found })))
}

/// Percent-encode credential characters that would break URL parsing.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// --- cameras --------------------------------------------------------------

async fn list_cameras(State(st): State<AppState>) -> ApiResult<Json<Vec<Camera>>> {
    Ok(Json(st.db.list_cameras()?))
}

async fn get_camera(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Camera>> {
    Ok(Json(st.db.get_camera(id)?.ok_or_else(not_found)?))
}

#[derive(Deserialize)]
struct NewCamera {
    name: String,
    source: String,
    #[serde(default)]
    detect_source: Option<String>,
    #[serde(default = "yes")]
    detect: bool,
    #[serde(default = "yes")]
    record: bool,
    #[serde(default)]
    group: Option<String>,
}

fn yes() -> bool {
    true
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A camera group label is free-form display text, but bounded so it can't
/// bloat the DB or the live-view tab bar.
fn valid_group(group: &str) -> bool {
    group.len() <= 64
}

/// True if `s` carries no control characters. A go2rtc source/sub-stream is
/// interpolated verbatim into the generated go2rtc YAML, so a newline (or other
/// control char) could inject an extra stream key — including an `exec:` source
/// that runs a command. Rejecting control chars closes that injection while
/// still allowing the legitimate `exec:`/`ffmpeg:`/`rtsp:` sources we document.
fn no_control(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// A primary camera source: non-empty after trimming and control-char free.
fn valid_source(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && no_control(s)
}

async fn add_camera(
    State(st): State<AppState>,
    Json(body): Json<NewCamera>,
) -> ApiResult<(StatusCode, Json<Camera>)> {
    if !valid_name(&body.name) {
        return Err(bad_request(
            "camera name must be 1-32 chars of a-z, 0-9, '-', '_'",
        ));
    }
    if !valid_source(&body.source) {
        return Err(bad_request(
            "source must be non-empty and free of control characters",
        ));
    }
    let detect_source = body
        .detect_source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if detect_source.is_some_and(|s| !no_control(s)) {
        return Err(bad_request(
            "sub-stream source must be free of control characters",
        ));
    }
    let mut cam = st
        .db
        .add_camera(
            &body.name,
            body.source.trim(),
            detect_source,
            body.detect,
            body.record,
        )
        .map_err(|e| bad_request(format!("could not add camera: {e}")))?;
    if let Some(g) = body
        .group
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !valid_group(g) {
            return Err(bad_request("group label must be 64 characters or fewer"));
        }
        cam.group = Some(g.to_string());
        st.db.update_camera(&cam)?;
    }
    st.go2rtc.restart_with(&st.db)?;
    Ok((StatusCode::CREATED, Json(cam)))
}

#[derive(Deserialize)]
struct CameraPatch {
    name: Option<String>,
    source: Option<String>,
    /// `Some("")` clears the sub-stream; `None` leaves it unchanged.
    detect_source: Option<String>,
    enabled: Option<bool>,
    detect: Option<bool>,
    record: Option<bool>,
    detect_config: Option<crate::db::DetectConfig>,
    /// `Some("")` clears the group; `None` leaves it unchanged.
    group: Option<String>,
}

async fn patch_camera(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<CameraPatch>,
) -> ApiResult<Json<Camera>> {
    let mut cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    // go2rtc's config depends only on name/source/detect_source/enabled, so a
    // metadata-only patch (group, detect, record, zones) must NOT restart it —
    // restarting needlessly drops every live stream.
    let needs_go2rtc = patch.name.is_some()
        || patch.source.is_some()
        || patch.detect_source.is_some()
        || patch.enabled.is_some();
    if let Some(name) = patch.name {
        if !valid_name(&name) {
            return Err(bad_request("invalid camera name"));
        }
        cam.name = name;
    }
    if let Some(source) = patch.source {
        if !valid_source(&source) {
            return Err(bad_request(
                "source must be non-empty and free of control characters",
            ));
        }
        cam.source = source.trim().to_string();
    }
    if let Some(ds) = patch.detect_source {
        let ds = ds.trim();
        if !no_control(ds) {
            return Err(bad_request(
                "sub-stream source must be free of control characters",
            ));
        }
        cam.detect_source = (!ds.is_empty()).then(|| ds.to_string());
    }
    cam.enabled = patch.enabled.unwrap_or(cam.enabled);
    cam.detect = patch.detect.unwrap_or(cam.detect);
    cam.record = patch.record.unwrap_or(cam.record);
    if let Some(dc) = patch.detect_config {
        for z in &dc.ignore_zones {
            if !(0.0..=1.0).contains(&z.x)
                || !(0.0..=1.0).contains(&z.y)
                || !(0.0..=1.0).contains(&z.w)
                || !(0.0..=1.0).contains(&z.h)
            {
                return Err(bad_request("zone coordinates must be fractions 0..1"));
            }
        }
        let in_unit = |p: &[f32; 2]| (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]);
        for z in &dc.zones {
            if z.points.len() < 3 {
                return Err(bad_request("a polygon zone needs at least 3 points"));
            }
            if !z.points.iter().all(in_unit) {
                return Err(bad_request("zone points must be fractions 0..1"));
            }
        }
        for m in &dc.privacy_masks {
            if m.len() < 3 || !m.iter().all(in_unit) {
                return Err(bad_request("a privacy mask needs ≥3 points in 0..1"));
            }
        }
        for a in [dc.min_area, dc.max_area].into_iter().flatten() {
            if !(0.0..=1.0).contains(&a) {
                return Err(bad_request("object-size bounds must be fractions 0..1"));
            }
        }
        cam.detect_config = dc;
    }
    if let Some(g) = patch.group {
        let g = g.trim();
        if !valid_group(g) {
            return Err(bad_request("group label must be 64 characters or fewer"));
        }
        cam.group = (!g.is_empty()).then(|| g.to_string());
    }
    st.db.update_camera(&cam)?;
    if needs_go2rtc {
        st.go2rtc.restart_with(&st.db)?;
    }
    Ok(Json(cam))
}

async fn delete_camera(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    st.db.get_camera(id)?.ok_or_else(not_found)?;
    st.db.delete_camera(id)?;
    st.go2rtc.restart_with(&st.db)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- config backup / restore -----------------------------------------------

/// A portable snapshot of the *configuration* (not recordings/events/faces):
/// cameras, the global settings blob, and alarm rules. Lets a self-hoster move
/// to a new machine without re-entering everything. NOTE: camera sources and
/// settings can contain credentials — treat the file as a secret.
#[derive(serde::Serialize, Deserialize)]
struct Backup {
    version: u32,
    exported_ts: i64,
    settings: Settings,
    cameras: Vec<Camera>,
    alarms: Vec<crate::db::AlarmRule>,
}

const BACKUP_VERSION: u32 = 1;

async fn backup(State(st): State<AppState>) -> ApiResult<Response> {
    let snapshot = Backup {
        version: BACKUP_VERSION,
        exported_ts: chrono::Local::now().timestamp(),
        settings: st.db.settings(),
        cameras: st.db.list_cameras()?,
        alarms: st.db.list_alarms()?,
    };
    let body = serde_json::to_string_pretty(&snapshot).map_err(anyhow::Error::from)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"zoomy-backup.json\"",
            ),
        ],
        body,
    )
        .into_response())
}

/// Restore is *additive and non-destructive*: settings are replaced wholesale,
/// but a camera/alarm whose name already exists is left untouched (so importing
/// into a populated instance can't clobber it). Designed for a fresh machine.
async fn restore(
    State(st): State<AppState>,
    Json(b): Json<Backup>,
) -> ApiResult<Json<serde_json::Value>> {
    if b.version > BACKUP_VERSION {
        return Err(bad_request(format!(
            "backup version {} is newer than this build supports ({BACKUP_VERSION})",
            b.version
        )));
    }
    st.db.save_settings(&b.settings)?;

    // Map the source instance's camera ids -> names up front, so per-camera
    // alarm scopes can be re-pointed at this instance's (new) ids by name.
    let backup_id_to_name: std::collections::HashMap<i64, String> =
        b.cameras.iter().map(|c| (c.id, c.name.clone())).collect();

    // Names already present (in the DB or added earlier this pass) are skipped,
    // so a duplicate *within* the file can't hit the UNIQUE constraint mid-loop.
    let mut seen_cams: std::collections::HashSet<String> =
        st.db.list_cameras()?.into_iter().map(|c| c.name).collect();
    let (mut cams_added, mut cams_skipped) = (0u32, 0u32);
    for cam in b.cameras {
        let ok = !seen_cams.contains(&cam.name)
            && valid_name(&cam.name)
            && valid_source(&cam.source)
            && cam.group.as_deref().is_none_or(valid_group)
            && cam.detect_source.as_deref().is_none_or(no_control);
        if !ok {
            cams_skipped += 1;
            continue;
        }
        let detect_source = cam
            .detect_source
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut created = st.db.add_camera(
            &cam.name,
            cam.source.trim(),
            detect_source,
            cam.detect,
            cam.record,
        )?;
        // add_camera defaults enabled=true and an empty config; carry over the rest.
        created.enabled = cam.enabled;
        created.detect_config = cam.detect_config;
        created.group = cam.group;
        st.db.update_camera(&created)?;
        seen_cams.insert(cam.name);
        cams_added += 1;
    }

    // Resolve names -> this instance's camera ids (including the ones just added)
    // for alarm re-pointing.
    let name_to_id: std::collections::HashMap<String, i64> = st
        .db
        .list_cameras()?
        .into_iter()
        .map(|c| (c.name, c.id))
        .collect();
    let existing_alarms: std::collections::HashSet<String> =
        st.db.list_alarms()?.into_iter().map(|a| a.name).collect();
    let mut alarms_added = 0u32;
    for mut rule in b.alarms {
        if existing_alarms.contains(&rule.name) {
            continue;
        }
        // Re-point a per-camera scope by NAME (the backup's camera_id is an id
        // from the *other* instance); drop the scope if that camera isn't here.
        if let Some(old_id) = rule.camera_id {
            rule.camera_id = backup_id_to_name
                .get(&old_id)
                .and_then(|name| name_to_id.get(name))
                .copied();
        }
        st.db.add_alarm(&rule)?;
        alarms_added += 1;
    }

    // Cameras changed → regenerate go2rtc config once.
    st.go2rtc.restart_with(&st.db)?;
    Ok(Json(serde_json::json!({
        "settings_applied": true,
        "cameras_added": cams_added,
        "cameras_skipped": cams_skipped,
        "alarms_added": alarms_added,
    })))
}

/// Same-origin proxy for go2rtc's embeddable player JS (`video-stream.js` +
/// its `video-rtc.js` import). go2rtc serves these without CORS headers, so a
/// cross-origin ES-module import from our UI would be blocked; proxying them
/// through our own origin sidesteps that while staying version-matched to the
/// running go2rtc. The streaming WebSocket itself is not CORS-restricted and
/// still connects straight to go2rtc.
async fn go2rtc_player(
    State(st): State<AppState>,
    Path(file): Path<String>,
) -> ApiResult<Response> {
    if !matches!(file.as_str(), "video-stream.js" | "video-rtc.js") {
        return Err(not_found());
    }
    let url = format!("{}/{}", st.go2rtc.api_base(), file);
    let js: anyhow::Result<String> = tokio::task::spawn_blocking(move || {
        let body = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .call()?
            .into_string()?;
        Ok(body)
    })
    .await
    .map_err(|e| anyhow::anyhow!("player fetch task: {e}"))?;
    let js = js.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("go2rtc player: {e}")))?;
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "public, max-age=300"),
        ],
        js,
    )
        .into_response())
}

// --- PTZ --------------------------------------------------------------------

fn ptz_target(st: &AppState, id: i64) -> Result<crate::ptz::CamTarget, ApiError> {
    let cam = st
        .db
        .get_camera(id)
        .map_err(ApiError::from)?
        .ok_or_else(not_found)?;
    crate::ptz::parse_source(&cam.source)
        .ok_or_else(|| bad_request("camera source has no host/credentials for ONVIF"))
}

/// Does this camera answer ONVIF PTZ? (Used by the UI to decide whether to
/// draw the control pad.)
async fn ptz_caps(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let target = match ptz_target(&st, id) {
        Ok(t) => t,
        Err(_) => return Ok(Json(serde_json::json!({ "supported": false }))),
    };
    let supported = tokio::task::spawn_blocking(move || crate::ptz::supports_ptz(&target))
        .await
        .unwrap_or(false);
    Ok(Json(serde_json::json!({ "supported": supported })))
}

#[derive(Deserialize)]
struct PtzReq {
    action: String, // "move" | "stop"
    #[serde(default)]
    pan: f32,
    #[serde(default)]
    tilt: f32,
    #[serde(default)]
    zoom: f32,
}

async fn ptz_command(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PtzReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let target = ptz_target(&st, id)?;
    let clamp = |v: f32| v.clamp(-1.0, 1.0);
    let action = req.action.clone();
    let result = tokio::task::spawn_blocking(move || match action.as_str() {
        "move" => {
            crate::ptz::continuous_move(&target, clamp(req.pan), clamp(req.tilt), clamp(req.zoom))
        }
        "stop" => crate::ptz::stop(&target),
        other => anyhow::bail!("unknown ptz action {other:?}"),
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    result.map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Proxy the camera's current decoded frame from go2rtc as a same-origin JPEG.
/// The zone/mask editor draws on top of this still; serving it through the core
/// API avoids the cross-origin taint that blocks reading go2rtc pixels directly.
async fn camera_frame(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    let url = format!("{}/api/frame.jpeg?src={}", st.go2rtc.api_base(), cam.name);
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        use std::io::Read as _;
        let resp = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .call()?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(32 * 1024 * 1024)
            .read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

// --- events ----------------------------------------------------------------

#[derive(Deserialize)]
struct EventQuery {
    camera_id: Option<i64>,
    label: Option<String>,
    gesture: Option<String>,
    zone: Option<String>,
    after: Option<i64>,
    before: Option<i64>,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    100
}

async fn list_events(
    State(st): State<AppState>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Json<Vec<crate::db::Event>>> {
    Ok(Json(st.db.list_events(
        q.camera_id,
        q.label.as_deref(),
        q.gesture.as_deref(),
        q.zone.as_deref(),
        q.after,
        q.before,
        q.limit.min(1000),
    )?))
}

#[derive(Deserialize)]
struct GestureReq {
    /// Registered camera to attribute the signal to; its current frame becomes
    /// the event's context snapshot. Optional when exactly one camera exists.
    camera: Option<String>,
    gesture: String,
    #[serde(default)]
    score: Option<f32>,
}

/// Record a hand signal recognized by the browser-side recognizer as a
/// first-class event, then fire matching alarm rules (webhook / ntfy / MQTT).
/// This is what turns "raise an open palm at the door" into a real, silent
/// trigger: the detection runs on-device (portable, GPU-accelerated), but the
/// surveillance semantics — events, snapshots, alarms — live here.
async fn record_gesture(
    State(st): State<AppState>,
    Json(req): Json<GestureReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let settings = st.db.settings();
    if !settings.gesture_recognition {
        return Err(bad_request("gesture recognition is disabled in Settings"));
    }
    let canonical = gesture::canonical(&req.gesture)
        .ok_or_else(|| bad_request(format!("unknown gesture {:?}", req.gesture)))?;
    // The duress/help signal always fires (even if not in the armed list) —
    // it's a panic button, so it must never be filtered out.
    let is_duress = !settings.gesture_duress.is_empty() && canonical == settings.gesture_duress;
    // Otherwise honor the armed-gesture filter (empty = any recognized signal).
    if !is_duress
        && !settings.gesture_labels.is_empty()
        && !settings.gesture_labels.iter().any(|g| g == canonical)
    {
        return Ok(Json(
            serde_json::json!({ "recorded": false, "reason": "gesture not armed" }),
        ));
    }

    // Attribute the signal to a camera (its current view is the snapshot).
    let cameras = st.db.list_cameras()?;
    let cam = match req.camera.as_deref() {
        Some(name) => cameras.iter().find(|c| c.name == name).cloned(),
        None if cameras.len() == 1 => cameras.into_iter().next(),
        None => None,
    }
    .ok_or_else(|| bad_request("no camera to attribute the signal to — register or select one"))?;

    let now = chrono::Local::now().timestamp();
    let score = req.score.unwrap_or(1.0).clamp(0.0, 1.0);

    // Best-effort: grab what that camera currently sees as context.
    let snap_rel = format!("{}-gesture-{}.jpg", cam.name, now);
    let snap_abs = st.snapshots_dir.join(&snap_rel);
    let snapshot = {
        let api_base = st.go2rtc.api_base();
        let key = cam.name.clone();
        let abs = snap_abs.clone();
        tokio::task::spawn_blocking(move || save_gesture_snapshot(&api_base, &key, &abs))
            .await
            .ok()
            .and_then(|r| r.ok())
            .map(|_| snap_rel.clone())
    };

    let id = st.db.add_event(
        cam.id,
        now,
        "gesture",
        score,
        [0.0; 4],
        snapshot.as_deref(),
        None,
        None,
        Some(canonical),
        None,
    )?;
    tracing::info!(camera = %cam.name, gesture = canonical, event = id, "hand signal recorded");

    let snap_url = snapshot
        .as_ref()
        .map(|s| format!("/api/snapshots/{s}"))
        .unwrap_or_default();
    // Publish to MQTT subscribers on the normal event channel.
    let _ = st.mqtt_tx.send(crate::mqtt::EventMsg {
        event_id: id,
        camera: cam.name.clone(),
        label: "gesture".to_string(),
        score,
        ts: now,
        snapshot: snap_url.clone(),
        topic: None,
    });

    // Fire webhook + matching alarm actions off-thread (blocking I/O), so a
    // slow listener never stalls the response.
    let rules: Vec<crate::db::AlarmRule> = st
        .db
        .list_alarms()?
        .into_iter()
        .filter(|r| {
            r.matches(cam.id, "gesture", score, None, None, Some(canonical))
                && crate::notify::ready(r, &st.alarm_throttle, now)
        })
        .collect();
    let mqtt_tx = st.mqtt_tx.clone();
    let webhook_url = settings.webhook_url.clone();
    let base_url = settings.public_base_url.clone();
    let webhook_template = settings.webhook_template.clone();
    let health_ntfy = settings.health_ntfy_url.clone();
    let camera = cam.name.clone();
    let gesture_owned = canonical.to_string();
    let snap_path = snapshot.as_ref().map(|_| snap_abs.clone());
    tokio::task::spawn_blocking(move || {
        let ev = crate::notify::AlarmEvent {
            event_id: id,
            camera: &camera,
            label: "gesture",
            score,
            ts: now,
            snapshot_url: &snap_url,
            snapshot_path: snap_path.as_deref(),
            face: None,
            plate: None,
            gesture: Some(&gesture_owned),
            base_url: &base_url,
            webhook_template: &webhook_template,
            duress: is_duress,
        };
        // Guaranteed panic path: a duress signal pushes straight to the health
        // ntfy topic at max urgency, even if no alarm rule is configured.
        if is_duress && !health_ntfy.is_empty() {
            crate::notify::ntfy_text(
                &health_ntfy,
                &format!("🚨 DURESS signal on {camera}"),
                &format!("Hand-signal panic button triggered on {camera}"),
                "warning,rotating_light,sos",
            );
        }
        if !webhook_url.is_empty() {
            let body = if webhook_template.is_empty() {
                serde_json::json!({
                    "type": "gesture",
                    "event_id": id,
                    "camera": camera,
                    "label": "gesture",
                    "gesture": gesture_owned,
                    "score": score,
                    "ts": now,
                    "snapshot": ev.snapshot_url,
                })
                .to_string()
            } else {
                crate::notify::render_template(&webhook_template, &ev)
            };
            if let Err(e) = ureq::post(&webhook_url)
                .timeout(std::time::Duration::from_secs(3))
                .set("Content-Type", "application/json")
                .send_string(&body)
            {
                tracing::debug!("gesture webhook failed: {e}");
            }
        }
        for rule in &rules {
            crate::notify::fire(rule, &ev, &mqtt_tx);
        }
    });

    Ok(Json(serde_json::json!({
        "recorded": true,
        "event_id": id,
        "gesture": canonical,
        "camera": cam.name,
        "duress": is_duress,
    })))
}

/// Fetch the camera's current frame from go2rtc and write it to `path`.
fn save_gesture_snapshot(
    api_base: &str,
    camera: &str,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let url = format!("{api_base}/api/frame.jpeg?src={camera}");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .call()?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    std::fs::write(path, &bytes)?;
    Ok(())
}

#[derive(Deserialize)]
struct ClipQuery {
    /// Seconds of context before the event (default 5, max 30).
    pre: Option<u32>,
    /// Seconds after (default 10, max 60).
    post: Option<u32>,
}

/// Export a short MP4 around an event, packet-copied out of the containing
/// segment (no re-encode) and cached under data/clips. Frigate-style clips.
async fn event_clip(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<ClipQuery>,
    req: Request,
) -> ApiResult<Response> {
    let ev = st.db.get_event(id)?.ok_or_else(not_found)?;
    let seg = st
        .db
        .find_segment_at(ev.camera_id, ev.ts)?
        .ok_or_else(not_found)?;

    let pre = q.pre.unwrap_or(5).min(30);
    let post = q.post.unwrap_or(10).min(60);
    // Clamp to the containing segment (v1: clips do not span segments).
    let offset = (ev.ts - seg.start_ts - i64::from(pre)).max(0);
    let duration = pre + post;

    let clip_name = format!("event-{id}-{pre}-{post}.mp4");
    let clip_path = st.clips_dir.join(&clip_name);
    if !clip_path.exists() {
        std::fs::create_dir_all(&st.clips_dir).ok();
        let ffmpeg = recorder::locate_ffmpeg(st.ffmpeg_bin.as_deref())?;
        let seg_path = seg.path.clone();
        let out = clip_path.clone();
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new(ffmpeg)
                .args(["-loglevel", "error", "-ss", &offset.to_string(), "-i"])
                .arg(&seg_path)
                .args(["-t", &duration.to_string(), "-c", "copy"])
                .args(["-movflags", "+faststart", "-y"])
                .arg(&out)
                .status()
        })
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if !status.success() {
            return Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "clip extraction failed".into(),
            ));
        }
    }

    let mut resp = ServeFile::new(clip_path).oneshot(req).await.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!(
            "attachment; filename=\"{}-{}-{}.mp4\"",
            ev.camera, ev.label, ev.ts
        )
        .parse()
        .expect("valid header"),
    );
    Ok(resp)
}

#[derive(Deserialize)]
struct ThumbQuery {
    /// Resize the snapshot to this width (px) for grid thumbnails. Cached under
    /// snapshots/thumbs. Clamped to 64..=1280.
    w: Option<u32>,
}

async fn snapshot(
    State(st): State<AppState>,
    Path(file): Path<String>,
    Query(q): Query<ThumbQuery>,
    req: Request,
) -> ApiResult<Response> {
    // Snapshot names are generated by us ({camera}-{ts}.jpg); reject traversal.
    if file.contains(['/', '\\']) || file.contains("..") {
        return Err(bad_request("bad snapshot name"));
    }
    let path = st.snapshots_dir.join(&file);
    if !path.exists() {
        return Err(not_found());
    }
    // Thumbnail request: serve (and cache) a width-resized JPEG.
    if let Some(w) = q.w {
        let w = w.clamp(64, 1280);
        let thumb_dir = st.snapshots_dir.join("thumbs");
        let thumb_path = thumb_dir.join(format!("{w}-{file}"));
        if !thumb_path.exists() {
            let src = path.clone();
            let out = thumb_path.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                std::fs::create_dir_all(&thumb_dir).ok();
                let img = image::open(&src)?;
                let h = (w as f32 * img.height() as f32 / img.width().max(1) as f32) as u32;
                img.resize(w, h.max(1), image::imageops::FilterType::Triangle)
                    .save_with_format(&out, image::ImageFormat::Jpeg)?;
                Ok(())
            })
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            // If resizing fails, fall back to the full image below.
            .ok();
        }
        if thumb_path.exists() {
            return Ok(ServeFile::new(thumb_path)
                .oneshot(req)
                .await
                .into_response());
        }
    }
    Ok(ServeFile::new(path).oneshot(req).await.into_response())
}

// --- alarm manager -----------------------------------------------------------

async fn list_alarms_api(State(st): State<AppState>) -> ApiResult<Json<Vec<crate::db::AlarmRule>>> {
    Ok(Json(st.db.list_alarms()?))
}

async fn add_alarm_api(
    State(st): State<AppState>,
    Json(rule): Json<crate::db::AlarmRule>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if rule.name.trim().is_empty() {
        return Err(bad_request("rule name required"));
    }
    if !matches!(rule.action.as_str(), "webhook" | "mqtt" | "ntfy") {
        return Err(bad_request("action must be webhook, mqtt or ntfy"));
    }
    if rule.target.trim().is_empty() {
        return Err(bad_request("target required (URL or MQTT topic suffix)"));
    }
    if rule.days.iter().any(|d| *d > 6) {
        return Err(bad_request("days must be 0 (Sunday) through 6 (Saturday)"));
    }
    if rule.priority > 5 {
        return Err(bad_request("priority must be 0 (default) through 5"));
    }
    if rule.cooldown_secs < 0 {
        return Err(bad_request("cooldown must be ≥ 0 seconds"));
    }
    for t in [&rule.start_hhmm, &rule.end_hhmm].into_iter().flatten() {
        let ok = t.split_once(':').is_some_and(|(h, m)| {
            h.parse::<u8>().is_ok_and(|h| h < 24) && m.parse::<u8>().is_ok_and(|m| m < 60)
        });
        if !ok {
            return Err(bad_request("schedule times must be HH:MM (24h)"));
        }
    }
    let id = st.db.add_alarm(&rule)?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

#[derive(Deserialize)]
struct AlarmPatch {
    enabled: Option<bool>,
    /// Snooze the rule for this many seconds from now; 0 clears the snooze.
    snooze_secs: Option<i64>,
}

async fn patch_alarm_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(p): Json<AlarmPatch>,
) -> ApiResult<StatusCode> {
    if let Some(enabled) = p.enabled {
        st.db.set_alarm_enabled(id, enabled)?;
    }
    if let Some(secs) = p.snooze_secs {
        let until = if secs <= 0 {
            0
        } else {
            chrono::Local::now().timestamp() + secs
        };
        st.db.set_alarm_snooze(id, until)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_alarm_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    st.db.delete_alarm(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- smart search ------------------------------------------------------------

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    24
}

/// Natural-language event search (UniFi AI Key style): CLIP text embedding of
/// the query ranked against the stored snapshot embeddings.
async fn smart_search(
    State(st): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    if !crate::smart::models_present() {
        return Err(bad_request(
            "smart search models not installed (see README: clip_vision.onnx, \
             clip_text.onnx, clip_tokenizer.json)",
        ));
    }
    let query = q.q.trim().to_string();
    if query.is_empty() {
        return Err(bad_request("empty query"));
    }
    let qe = tokio::task::spawn_blocking(move || crate::smart::embed_text(&query))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let mut scored: Vec<(f32, i64)> = st
        .db
        .all_event_embeddings()?
        .into_iter()
        .map(|(id, emb)| (crate::smart::cosine(&qe, &emb), id))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut results = Vec::new();
    for (sim, id) in scored.into_iter().take(q.limit.min(100)) {
        if let Some(ev) = st.db.get_event(id)? {
            results.push(serde_json::json!({ "similarity": sim, "event": ev }));
        }
    }
    Ok(Json(serde_json::json!({ "results": results })))
}

// --- faces -------------------------------------------------------------------

fn safe_file(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && !name.contains("..")
}

/// Enrolled identities + unknown face crops waiting to be named.
async fn faces_overview(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let enrolled = st.db.list_faces()?;
    let mut unknown: Vec<String> = std::fs::read_dir(st.faces_dir.join("unknown"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".jpg"))
                .collect()
        })
        .unwrap_or_default();
    unknown.sort();
    unknown.reverse(); // newest first (timestamped names)
    Ok(Json(
        serde_json::json!({ "enrolled": enrolled, "unknown": unknown }),
    ))
}

#[derive(Deserialize)]
struct EnrollReq {
    name: String,
    unknown_file: String,
}

/// Name an unknown face: ingest the embedding sidecar saved by the pipeline,
/// then remove the crop from the unknown queue.
async fn enroll_face(
    State(st): State<AppState>,
    Json(req): Json<EnrollReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(bad_request("name must be 1-64 characters"));
    }
    if !safe_file(&req.unknown_file) {
        return Err(bad_request("bad file name"));
    }
    let dir = st.faces_dir.join("unknown");
    let sidecar = dir.join(format!("{}.json", req.unknown_file));
    let json = std::fs::read_to_string(&sidecar)
        .map_err(|_| bad_request("embedding sidecar missing for that crop"))?;
    let embedding: Vec<f32> =
        serde_json::from_str(&json).map_err(|_| bad_request("corrupt embedding sidecar"))?;
    if embedding.len() != 512 {
        return Err(bad_request("unexpected embedding size"));
    }
    let id = st.db.add_face(name, &embedding)?;
    let _ = std::fs::remove_file(dir.join(&req.unknown_file));
    let _ = std::fs::remove_file(sidecar);
    Ok(Json(serde_json::json!({ "id": id, "name": name })))
}

async fn delete_face_api(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    st.db.delete_face(id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RenameReq {
    name: String,
}

async fn rename_face_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<RenameReq>,
) -> ApiResult<StatusCode> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(bad_request("name must be 1-64 characters"));
    }
    st.db.rename_face(id, name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unknown_face_img(
    State(st): State<AppState>,
    Path(file): Path<String>,
    req: Request,
) -> ApiResult<Response> {
    if !safe_file(&file) {
        return Err(bad_request("bad file name"));
    }
    let path = st.faces_dir.join("unknown").join(&file);
    if !path.exists() {
        return Err(not_found());
    }
    Ok(ServeFile::new(path).oneshot(req).await.into_response())
}

// --- recordings -------------------------------------------------------------

#[derive(Deserialize)]
struct RecordingQuery {
    camera_id: Option<i64>,
    #[serde(default = "default_limit")]
    limit: u32,
}

async fn list_recordings(
    State(st): State<AppState>,
    Query(q): Query<RecordingQuery>,
) -> ApiResult<Json<Vec<crate::db::SegmentRow>>> {
    Ok(Json(st.db.list_segments(q.camera_id, q.limit.min(1000))?))
}

#[derive(Deserialize)]
struct AtQuery {
    camera_id: i64,
    ts: i64,
}

/// Find the recording segment that contains a moment in time (used to jump
/// from an event straight into playback at the right offset).
async fn recording_at(
    State(st): State<AppState>,
    Query(q): Query<AtQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let seg = st
        .db
        .find_segment_at(q.camera_id, q.ts)?
        .ok_or_else(not_found)?;
    let offset = q.ts - seg.start_ts;
    // Generous slack: ffmpeg cuts segments on keyframes, so real duration can
    // exceed the configured length by a GOP.
    let max_len = i64::from(st.db.settings().segment_seconds) + 15;
    if offset > max_len {
        return Err(not_found());
    }
    Ok(Json(
        serde_json::json!({ "segment": seg, "offset_secs": offset }),
    ))
}

/// Stream a recording segment with HTTP range support (so <video> can seek).
async fn segment_video(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    req: Request,
) -> ApiResult<Response> {
    let seg = st.db.get_segment(id)?.ok_or_else(not_found)?;
    Ok(ServeFile::new(seg.path).oneshot(req).await.into_response())
}

// --- stats -----------------------------------------------------------------

/// Storage + event totals for the dashboard: per-camera disk usage from the
/// segment index, overall event count, and snapshot footprint.
async fn stats(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let cameras = st.db.storage_stats()?;
    let total_bytes: u64 = cameras.iter().map(|c| c.bytes).sum();
    let snapshots_bytes: u64 = std::fs::read_dir(&st.snapshots_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    // Free space on the volume holding new recordings.
    let settings = st.db.settings();
    let rec_root = if settings.recordings_dir.trim().is_empty() {
        st.recordings_dir_default.clone()
    } else {
        PathBuf::from(settings.recordings_dir.trim())
    };
    let disk_free = fs2::available_space(&rec_root)
        .or_else(|_| fs2::available_space(std::path::Path::new(".")))
        .unwrap_or(0);
    Ok(Json(serde_json::json!({
        "cameras": cameras,
        "total_bytes": total_bytes,
        "snapshots_bytes": snapshots_bytes,
        "events_total": st.db.count_events()?,
        "disk_free_bytes": disk_free,
        "recordings_root": rec_root.to_string_lossy(),
    })))
}

// --- settings ----------------------------------------------------------------

async fn get_settings(State(st): State<AppState>) -> Json<Settings> {
    Json(st.db.settings())
}

async fn put_settings(
    State(st): State<AppState>,
    Json(s): Json<Settings>,
) -> ApiResult<Json<Settings>> {
    if !(0.0..=1.0).contains(&s.confidence)
        || !(0.0..=1.0).contains(&s.nms_iou)
        || !(0.0..=1.0).contains(&s.motion_threshold)
    {
        return Err(bad_request("thresholds must be within 0..1"));
    }
    if s.poll_ms < 100 {
        return Err(bad_request("poll_ms must be at least 100"));
    }
    // A custom recordings root must be creatable + writable before we accept
    // it — the recorder thread would otherwise fail silently every cycle.
    let rec_root = s.recordings_dir.trim();
    if !rec_root.is_empty() {
        let p = PathBuf::from(rec_root);
        std::fs::create_dir_all(&p)
            .map_err(|e| bad_request(format!("recordings dir not creatable: {e}")))?;
        let probe = p.join(".zoomy-write-test");
        std::fs::write(&probe, b"ok")
            .map_err(|e| bad_request(format!("recordings dir not writable: {e}")))?;
        let _ = std::fs::remove_file(&probe);
    }
    st.db.save_settings(&s)?;
    Ok(Json(st.db.settings()))
}

#[cfg(test)]
mod tests {
    use super::{no_control, valid_group, valid_source};

    #[test]
    fn source_validation_blocks_yaml_injection_but_allows_real_sources() {
        // Legitimate go2rtc sources — all accepted.
        assert!(valid_source("rtsp://user:pass@192.168.1.50:554/stream1"));
        assert!(valid_source(
            "exec:ffmpeg -re -stream_loop -1 -i a.mp4 -f rtsp {output}"
        ));
        assert!(valid_source("ffmpeg:device?video=0"));
        assert!(valid_source("  onvif://admin:pw@cam  ")); // trimmed

        // Empty / whitespace-only — rejected.
        assert!(!valid_source(""));
        assert!(!valid_source("   "));

        // Newline/CR injection (the RCE vector) — rejected.
        assert!(!valid_source("rtsp://x\n  pwn:\n    - exec:calc"));
        assert!(!valid_source("rtsp://x\r\n  pwn:"));
        assert!(!no_control("a\tb"));
        assert!(no_control("rtsp://ok/stream"));
    }

    #[test]
    fn group_length_capped() {
        assert!(valid_group(""));
        assert!(valid_group(&"a".repeat(64)));
        assert!(!valid_group(&"a".repeat(65)));
    }
}
