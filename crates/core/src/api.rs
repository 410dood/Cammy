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
use crate::proc::NoConsole as _;
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
        .route("/api/events/export.csv", get(export_events_csv))
        .route(
            "/api/events/{id}/bookmark",
            axum::routing::post(bookmark_event),
        )
        .route("/api/gesture", axum::routing::post(record_gesture))
        .route("/api/events/{id}/clip", get(event_clip))
        .route("/api/events/{id}/similar", get(event_similar))
        .route("/api/search", get(smart_search))
        .route("/api/alarms", get(list_alarms_api).post(add_alarm_api))
        .route(
            "/api/alarms/{id}",
            axum::routing::patch(patch_alarm_api).delete(delete_alarm_api),
        )
        .route("/api/tokens", get(list_tokens).post(create_token))
        .route("/api/tokens/{id}", axum::routing::delete(delete_token))
        .route("/api/audit", get(list_audit))
        .route("/api/me", get(me))
        .route("/api/me/password", axum::routing::post(change_my_password))
        .route("/api/users", get(list_users_api).post(create_user_api))
        .route(
            "/api/users/{id}",
            axum::routing::patch(patch_user_api).delete(delete_user_api),
        )
        .route(
            "/api/users/{id}/cameras",
            get(get_user_cameras).put(put_user_cameras),
        )
        .route("/api/faces", get(faces_overview).post(enroll_face))
        .route(
            "/api/faces/{id}",
            axum::routing::patch(rename_face_api).delete(delete_face_api),
        )
        .route("/api/faces/unknown/{file}", get(unknown_face_img))
        .route("/api/plates", get(list_plates_api).post(add_plate_api))
        .route(
            "/api/plates/{id}",
            axum::routing::patch(update_plate_api).delete(delete_plate_api),
        )
        .route("/api/snapshots/{file}", get(snapshot))
        .route("/api/recordings", get(list_recordings))
        .route("/api/recordings/at", get(recording_at))
        .route("/api/recordings/{id}/video", get(segment_video))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/stats", get(stats))
        .route("/api/overview", get(overview))
        .route("/api/analytics/counts", get(analytics_counts))
        .route("/api/analytics/occupancy", get(analytics_occupancy))
        .route("/api/analytics/heatmap", get(analytics_heatmap))
        .route("/api/arm", get(get_arm_mode).put(set_arm_mode))
        .route("/api/notifications", get(list_notifications_api))
        .route(
            "/api/notifications/read-all",
            axum::routing::post(mark_all_notifications_read_api),
        )
        .route(
            "/api/notifications/{id}/read",
            axum::routing::post(mark_notification_read_api),
        )
        .route("/api/digests", get(list_digests_api))
        .route("/api/digests/run", axum::routing::post(run_digest_api))
        .route("/api/metrics", get(metrics))
        .route("/api/backup", get(backup))
        .route("/api/restore", axum::routing::post(restore))
        .route("/api/player/{file}", get(go2rtc_player))
        .route("/api/ws", get(stream_ws))
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

fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
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
async fn camera_status(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let now = chrono::Local::now().timestamp();
    let window = crate::status::freshness_window(st.db.settings().poll_ms);
    let allow = allowed_cameras(&st, &p)?;
    let mut out = serde_json::Map::new();
    for cam in st.db.list_cameras()? {
        if !camera_allowed(&allow, cam.id) {
            continue;
        }
        let h = st
            .status
            .snapshot()
            .get(&cam.id)
            .cloned()
            .unwrap_or_default();
        let online = h.is_online(cam.detect, now, window);
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
    let users = st.db.count_users().unwrap_or(0);
    Json(serde_json::json!({
        "enabled": st.db.get_kv(crate::auth::KV_PASSWORD).is_some() || users > 0,
        "users": users,
    }))
}

#[derive(Deserialize)]
struct PasswordReq {
    password: String,
}

/// Set (or clear, with an empty string) the remote-access password. Existing
/// sessions are invalidated either way.
async fn set_password(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
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
    let (ip, _) = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy);
    st.db.add_audit(
        chrono::Local::now().timestamp(),
        Some(&ip.to_string()),
        if pw.is_empty() {
            "password_cleared"
        } else {
            "password_set"
        },
        None,
    );
    Ok(Json(serde_json::json!({ "enabled": !pw.is_empty() })))
}

#[derive(Deserialize)]
struct LoginReq {
    /// Optional for the legacy single-password mode; required for named users.
    #[serde(default)]
    username: Option<String>,
    password: String,
}

async fn login(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginReq>,
) -> ApiResult<Response> {
    let users_exist = st.db.count_users().unwrap_or(0) > 0;
    let kv_password = st.db.get_kv(crate::auth::KV_PASSWORD);
    if !users_exist && kv_password.is_none() {
        return Ok(
            Json(serde_json::json!({ "ok": true, "note": "auth disabled" })).into_response(),
        );
    }
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
    let now = chrono::Local::now().timestamp();
    let ip = peer_ip.to_string();

    // Resolve the credentials into a principal: a named user (C5) when a username
    // is supplied, otherwise the legacy single-password admin.
    let username = req
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let principal: Option<crate::auth::Principal> = if let Some(uname) = username {
        match st.db.user_by_name(uname)? {
            Some((id, hash, role)) if crate::auth::verify_password(&hash, &req.password) => {
                if crate::auth::needs_rehash(&hash) {
                    let _ = st
                        .db
                        .set_user_password(id, &crate::auth::hash_password(&req.password));
                }
                Some(crate::auth::Principal {
                    user_id: Some(id),
                    username: Some(uname.to_string()),
                    role: crate::auth::Role::parse(&role),
                })
            }
            _ => None,
        }
    } else if let Some(stored) = &kv_password {
        if crate::auth::verify_password(stored, &req.password) {
            if crate::auth::needs_rehash(stored) {
                let _ = st.db.set_kv(
                    crate::auth::KV_PASSWORD,
                    &crate::auth::hash_password(&req.password),
                );
            }
            Some(crate::auth::Principal::admin())
        } else {
            None
        }
    } else {
        None
    };

    let Some(principal) = principal else {
        st.login_throttle.record_failure(peer_ip);
        // The username is attacker-controlled on the reject path; cap it so a
        // multi-megabyte value can't bloat or spam the audit log.
        let uname_audit = username.map(|u| u.chars().take(64).collect::<String>());
        st.db
            .add_audit(now, Some(&ip), "login_failed", uname_audit.as_deref());
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        ));
    };
    st.login_throttle.record_success(peer_ip);
    st.db.add_audit(
        now,
        Some(&ip),
        "login_success",
        principal.username.as_deref(),
    );
    let token = crate::auth::new_token();
    st.sessions.insert(token.clone(), principal);
    let mut resp = Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        crate::auth::session_cookie(&token, st.tls)
            .parse()
            .expect("valid cookie header"),
    );
    Ok(resp)
}

// --- C5: current user + user/role management ---------------------------------

/// Who the caller is (role + username), for the frontend to gate UI. Reachable
/// only once authenticated; the middleware injects the principal.
async fn me(
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> Json<serde_json::Value> {
    // Reaching this handler means the caller already passed auth (a session, the
    // local box, an API token, or open mode), so they are authenticated. `named`
    // distinguishes a real user account from the legacy/loopback/token admin.
    Json(serde_json::json!({
        "authenticated": true,
        "named": p.user_id.is_some(),
        "username": p.username,
        "role": p.role,
    }))
}

#[derive(Deserialize)]
struct ChangePwReq {
    old_password: String,
    new_password: String,
}

/// A logged-in *named* user changes their own password (any role — gated to
/// Viewer in `min_role_for`). Verifies the current password first. Loopback /
/// legacy / token admins have no per-user password here; they manage the shared
/// password under Settings → Remote access.
async fn change_my_password(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    Json(req): Json<ChangePwReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (Some(uid), Some(uname)) = (p.user_id, p.username.as_deref()) else {
        return Err(bad_request(
            "no user account to change — set the shared password under Settings",
        ));
    };
    if req.new_password.len() < 6 {
        return Err(bad_request("new password must be at least 6 characters"));
    }
    let Some((id, hash, _role)) = st.db.user_by_name(uname)? else {
        return Err(not_found());
    };
    if id != uid || !crate::auth::verify_password(&hash, &req.old_password) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "current password is wrong".into(),
        ));
    }
    st.db
        .set_user_password(uid, &crate::auth::hash_password(&req.new_password))?;
    let now = chrono::Local::now().timestamp();
    st.db.add_audit(
        now,
        None,
        "user_password_changed",
        Some(&format!("#{uid} (self)")),
    );
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn valid_username(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

async fn list_users_api(State(st): State<AppState>) -> ApiResult<Json<Vec<crate::db::UserRow>>> {
    Ok(Json(st.db.list_users()?))
}

#[derive(Deserialize)]
struct NewUserReq {
    username: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

async fn create_user_api(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<NewUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let username = req.username.trim();
    if !valid_username(username) {
        return Err(bad_request(
            "username must be 1-64 chars of a-z, 0-9, '.', '_', '-'",
        ));
    }
    if req.password.len() < 6 {
        return Err(bad_request("password must be at least 6 characters"));
    }
    let role = crate::auth::Role::parse(req.role.as_deref().unwrap_or("viewer")).as_str();
    let now = chrono::Local::now().timestamp();
    let id = st
        .db
        .add_user(
            username,
            &crate::auth::hash_password(&req.password),
            role,
            now,
        )
        .map_err(|_| bad_request("could not add user (is the username already taken?)"))?;
    let (ip, _) = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy);
    st.db
        .add_audit(now, Some(&ip.to_string()), "user_created", Some(username));
    Ok(Json(
        serde_json::json!({ "id": id, "username": username, "role": role }),
    ))
}

#[derive(Deserialize)]
struct PatchUserReq {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

async fn patch_user_api(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    axum::Extension(me): axum::Extension<crate::auth::Principal>,
    Json(req): Json<PatchUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let ip = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy)
        .0
        .to_string();
    let now = chrono::Local::now().timestamp();
    let mut changed = false;

    if let Some(role) = &req.role {
        let role = crate::auth::Role::parse(role).as_str();
        // An admin can't demote their own account (a self-lockout the delete
        // guard already prevents); the last-admin check is atomic in the DB.
        if me.user_id == Some(id) && role != "admin" {
            return Err(bad_request("you can't change your own role"));
        }
        match st.db.set_user_role_guarded(id, role)? {
            crate::db::SetRole::Ok => {}
            crate::db::SetRole::NotFound => return Err(not_found()),
            crate::db::SetRole::LastAdmin => {
                return Err(bad_request("can't demote the last admin"))
            }
        }
        st.sessions.clear_user(id); // the new role takes effect on next request
        st.db.add_audit(
            now,
            Some(&ip),
            "user_role_changed",
            Some(&format!("#{id} -> {role}")),
        );
        changed = true;
    }
    if let Some(pw) = &req.password {
        if pw.len() < 6 {
            return Err(bad_request("password must be at least 6 characters"));
        }
        if !st
            .db
            .set_user_password(id, &crate::auth::hash_password(pw))?
        {
            return Err(not_found());
        }
        st.sessions.clear_user(id); // force just that user to re-login
        st.db.add_audit(
            now,
            Some(&ip),
            "user_password_changed",
            Some(&format!("#{id}")),
        );
        changed = true;
    }
    if !changed {
        return Err(bad_request("nothing to update"));
    }
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn delete_user_api(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    axum::Extension(me): axum::Extension<crate::auth::Principal>,
) -> ApiResult<StatusCode> {
    if me.user_id == Some(id) {
        return Err(bad_request("you can't delete your own account"));
    }
    match st.db.delete_user_guarded(id)? {
        crate::db::DeleteUser::Deleted => {}
        crate::db::DeleteUser::NotFound => return Err(not_found()),
        crate::db::DeleteUser::LastAdmin => return Err(bad_request("can't delete the last admin")),
    }
    st.sessions.clear_user(id); // invalidate the deleted user's sessions
    let ip = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy)
        .0
        .to_string();
    st.db.add_audit(
        chrono::Local::now().timestamp(),
        Some(&ip),
        "user_deleted",
        Some(&format!("#{id}")),
    );
    Ok(StatusCode::NO_CONTENT)
}

// --- per-camera RBAC scoping (#66) -------------------------------------------

/// The set of camera ids the caller is allowed to see, or `None` when
/// unrestricted. Unrestricted = an Admin, the loopback/legacy/token/SSO-unmatched
/// caller (`user_id == None`), or a named user with an empty allow-list. A
/// non-admin named user with a non-empty allow-list is scoped to exactly it.
fn allowed_cameras(
    st: &AppState,
    p: &crate::auth::Principal,
) -> ApiResult<Option<std::collections::HashSet<i64>>> {
    if p.role == crate::auth::Role::Admin {
        return Ok(None);
    }
    let Some(uid) = p.user_id else {
        return Ok(None);
    };
    let ids = st.db.list_user_cameras(uid)?;
    if ids.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ids.into_iter().collect()))
    }
}

/// Whether the caller may access camera `camera_id`.
fn camera_allowed(allowed: &Option<std::collections::HashSet<i64>>, camera_id: i64) -> bool {
    allowed
        .as_ref()
        .map(|s| s.contains(&camera_id))
        .unwrap_or(true)
}

/// Guard a single-camera route: `not_found` (404, not 403, to avoid camera-id
/// enumeration) when a scoped caller may not access `camera_id`.
fn require_camera(
    allowed: &Option<std::collections::HashSet<i64>>,
    camera_id: i64,
) -> ApiResult<()> {
    if camera_allowed(allowed, camera_id) {
        Ok(())
    } else {
        Err(not_found())
    }
}

/// An Admin lists a user's camera allow-list (empty = unrestricted).
async fn get_user_cameras(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<i64>>> {
    Ok(Json(st.db.list_user_cameras(id)?))
}

#[derive(Deserialize)]
struct SetUserCamerasReq {
    camera_ids: Vec<i64>,
}

/// An Admin sets a user's camera allow-list. An empty list = unrestricted.
async fn put_user_cameras(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetUserCamerasReq>,
) -> ApiResult<Json<serde_json::Value>> {
    if !st.db.set_user_cameras(id, &req.camera_ids)? {
        return Err(not_found());
    }
    // The scope is read live per request, so no session invalidation is needed.
    let ip = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy)
        .0
        .to_string();
    st.db.add_audit(
        chrono::Local::now().timestamp(),
        Some(&ip),
        "user_cameras_set",
        Some(&format!("#{id} -> {} camera(s)", req.camera_ids.len())),
    );
    Ok(Json(
        serde_json::json!({ "id": id, "cameras": req.camera_ids.len() }),
    ))
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
pub(crate) fn urlencode(s: &str) -> String {
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

async fn list_cameras(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<Camera>>> {
    let mut cams = st.db.list_cameras()?;
    if let Some(set) = &allowed_cameras(&st, &p)? {
        cams.retain(|c| set.contains(&c.id));
    }
    Ok(Json(cams))
}

async fn get_camera(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Camera>> {
    let cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, id)?;
    Ok(Json(cam))
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
    // A brand-new stream: reconcile PUTs it without restarting, so other
    // cameras' live views keep playing.
    st.go2rtc.sync_streams(&st.db, false)?;
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    Json(patch): Json<CameraPatch>,
) -> ApiResult<Json<Camera>> {
    let mut cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, id)?;
    // go2rtc's config depends only on name/source/detect_source/enabled, so a
    // metadata-only patch (group, detect, record, zones) must NOT touch it —
    // restarting needlessly drops every live stream.
    let needs_go2rtc = patch.name.is_some()
        || patch.source.is_some()
        || patch.detect_source.is_some()
        || patch.enabled.is_some();
    // Snapshot the stream-defining fields so we can tell a *same-name source
    // edit* (which the name-only live reconcile can't propagate, so it needs a
    // restart) from an add/remove/rename (which it handles without a restart).
    let (old_name, old_source, old_detect_source) = (
        cam.name.clone(),
        cam.source.clone(),
        cam.detect_source.clone(),
    );
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
        // A same-name edit of a live source string (main or sub) can't be
        // reconciled by name alone — the stream stays but its producer would
        // be stale — so force a restart for those. Add/remove/rename/enable
        // toggles reconcile live without dropping unrelated streams.
        let name_same = old_name == cam.name && cam.enabled;
        let main_src_edit = name_same && old_source != cam.source;
        let sub_src_edit = name_same
            && matches!(
                (&old_detect_source, &cam.detect_source),
                (Some(a), Some(b)) if a != b
            );
        st.go2rtc
            .sync_streams(&st.db, main_src_edit || sub_src_edit)?;
    }
    Ok(Json(cam))
}

async fn delete_camera(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<StatusCode> {
    st.db.get_camera(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, id)?;
    st.db.delete_camera(id)?;
    // Reconcile DELETEs just this camera's stream(s); other live views hold.
    st.go2rtc.sync_streams(&st.db, false)?;
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

    // Restore is additive (existing-named cameras are kept untouched), so the
    // live reconcile only PUTs the newly-imported streams and never blips
    // cameras that were already running.
    st.go2rtc.sync_streams(&st.db, false)?;
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

/// Reverse-proxy the live-stream WebSocket (player signaling + MSE/MJPEG media)
/// browser ⇄ zoomy ⇄ go2rtc. The browser only ever talks to our own origin, so
/// (a) go2rtc stays loopback-only with its default same-origin protection
/// intact (no `origin: "*"` needed), (b) live streams ride our auth middleware
/// like every other `/api/*` route, and (c) MSE/MJPEG work for remote viewers
/// since that media flows over this proxied socket.
async fn stream_ws(
    State(st): State<AppState>,
    ws: axum::extract::ws::WebSocketUpgrade,
    Query(q): Query<std::collections::HashMap<String, String>>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> Response {
    // Only forward the stream selector; build the upstream URL ourselves so a
    // client can't redirect us elsewhere.
    let src = q.get("src").cloned().unwrap_or_default();
    if src.trim().is_empty() {
        return bad_request("a stream name (?src=) is required").into_response();
    }
    // Per-camera RBAC: ?src is a client-supplied camera NAME forwarded verbatim
    // upstream, so authorize the camera it ACTUALLY streams. Match the exact name
    // first (covers a camera literally named "x_sub"); only if there's no exact
    // camera treat it as the "{base}_sub" sub-stream and authorize the base. This
    // avoids authorizing "x" while streaming a distinct "x_sub".
    match allowed_cameras(&st, &p) {
        Ok(Some(allow)) => {
            let raw = src.trim();
            let cid = match st.db.camera_by_name(raw) {
                Ok(Some(id)) => Some(id),
                Ok(None) => raw
                    .strip_suffix("_sub")
                    .and_then(|base| st.db.camera_by_name(base).ok().flatten()),
                // DB error -> fail closed (deny) rather than leak.
                Err(_) => return not_found().into_response(),
            };
            match cid {
                Some(id) if allow.contains(&id) => {}
                _ => return not_found().into_response(),
            }
        }
        Ok(None) => {}
        Err(e) => return e.into_response(),
    }
    let upstream = format!(
        "{}/api/ws?src={}",
        st.go2rtc.api_base().replacen("http", "ws", 1),
        urlencode(&src)
    );
    ws.on_upgrade(move |client| proxy_ws(client, upstream))
}

/// Pump messages both directions until either side closes or errors.
async fn proxy_ws(mut client: axum::extract::ws::WebSocket, upstream_url: String) {
    use futures_util::{SinkExt, StreamExt};

    // Bound the upstream connect so a wedged go2rtc can't pile up hung tasks.
    let connect = tokio_tungstenite::connect_async(&upstream_url);
    let upstream = match tokio::time::timeout(std::time::Duration::from_secs(8), connect).await {
        Ok(Ok((stream, _resp))) => stream,
        Ok(Err(e)) => {
            tracing::warn!("live-stream upstream connect failed: {e}");
            let _ = client.send(axum::extract::ws::Message::Close(None)).await;
            return;
        }
        Err(_) => {
            tracing::warn!("live-stream upstream connect timed out: {upstream_url}");
            let _ = client.send(axum::extract::ws::Message::Close(None)).await;
            return;
        }
    };
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut cl_tx, mut cl_rx) = client.split();

    // browser -> go2rtc
    let to_upstream = async {
        while let Some(Ok(msg)) = cl_rx.next().await {
            if up_tx.send(axum_to_tungstenite(msg)).await.is_err() {
                break;
            }
        }
        let _ = up_tx.close().await;
    };
    // go2rtc -> browser
    let to_client = async {
        while let Some(Ok(msg)) = up_rx.next().await {
            if let Some(m) = tungstenite_to_axum(msg) {
                if cl_tx.send(m).await.is_err() {
                    break;
                }
            }
        }
        let _ = cl_tx.close().await;
    };
    tokio::select! {
        _ = to_upstream => {}
        _ = to_client => {}
    }
}

fn axum_to_tungstenite(m: axum::extract::ws::Message) -> tokio_tungstenite::tungstenite::Message {
    use axum::extract::ws::Message as A;
    use tokio_tungstenite::tungstenite::Message as T;
    match m {
        A::Text(t) => T::Text(t.as_str().into()),
        A::Binary(b) => T::Binary(b),
        A::Ping(b) => T::Ping(b),
        A::Pong(b) => T::Pong(b),
        A::Close(_) => T::Close(None),
    }
}

fn tungstenite_to_axum(
    m: tokio_tungstenite::tungstenite::Message,
) -> Option<axum::extract::ws::Message> {
    use axum::extract::ws::Message as A;
    use tokio_tungstenite::tungstenite::Message as T;
    Some(match m {
        T::Text(t) => A::Text(t.as_str().into()),
        T::Binary(b) => A::Binary(b),
        T::Ping(b) => A::Ping(b),
        T::Pong(b) => A::Pong(b),
        T::Close(_) => A::Close(None),
        // Raw frames are an internal tungstenite detail; nothing to forward.
        T::Frame(_) => return None,
    })
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    // Scope check first, propagated as 404 (not swallowed into supported:false),
    // so an out-of-scope camera doesn't leak its PTZ capability.
    require_camera(&allowed_cameras(&st, &p)?, id)?;
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    Json(req): Json<PtzReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_camera(&allowed_cameras(&st, &p)?, id)?;
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
async fn camera_frame(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Response> {
    let cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, id)?;
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
    /// When true, return only bookmarked (flagged) events.
    #[serde(default)]
    flagged: bool,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    100
}

async fn list_events(
    State(st): State<AppState>,
    Query(q): Query<EventQuery>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<crate::db::Event>>> {
    let allow = allowed_cameras(&st, &p)?;
    if let Some(cid) = q.camera_id {
        require_camera(&allow, cid)?;
    }
    let mut events = st.db.list_events(
        q.camera_id,
        q.label.as_deref(),
        q.gesture.as_deref(),
        q.zone.as_deref(),
        q.after,
        q.before,
        q.flagged,
        q.limit.min(1000),
    )?;
    // Scoped users see only their cameras' events (the LIMIT applies pre-scope,
    // so a page may under-fill — acceptable; never leaks another camera's event).
    if let Some(set) = &allow {
        events.retain(|e| set.contains(&e.camera_id));
    }
    Ok(Json(events))
}

/// Quote a CSV field per RFC 4180, and neutralize spreadsheet formula injection
/// (a field starting with `= + - @` is prefixed with `'` so Excel/Sheets treats
/// it as text, since transcripts/notes/captions are partly attacker-influenced).
fn csv_field(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// Render events as an RFC 4180 CSV (one header row + a row per event).
fn events_to_csv(events: &[crate::db::Event]) -> String {
    let mut out = String::from(
        "id,time,camera,label,score,face,plate,gesture,zone,direction,speed,flagged,note,caption,transcript\n",
    );
    for e in events {
        let time = chrono::DateTime::from_timestamp(e.ts, 0)
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let fields = [
            e.id.to_string(),
            time,
            e.camera.clone(),
            e.label.clone(),
            format!("{:.3}", e.score),
            e.face.clone().unwrap_or_default(),
            e.plate.clone().unwrap_or_default(),
            e.gesture.clone().unwrap_or_default(),
            e.zone.clone().unwrap_or_default(),
            e.direction.clone().unwrap_or_default(),
            e.speed.map(|s| format!("{s:.0}")).unwrap_or_default(),
            if e.flagged {
                "yes".into()
            } else {
                String::new()
            },
            e.note.clone().unwrap_or_default(),
            e.caption.clone().unwrap_or_default(),
            e.transcript.clone().unwrap_or_default(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// Download matching events as a CSV (same filters as the events list, up to a
/// generous cap). Useful for record-keeping / insurance / sharing.
async fn export_events_csv(
    State(st): State<AppState>,
    Query(q): Query<EventQuery>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<impl IntoResponse> {
    let allow = allowed_cameras(&st, &p)?;
    if let Some(cid) = q.camera_id {
        require_camera(&allow, cid)?;
    }
    let mut events = st.db.list_events(
        q.camera_id,
        q.label.as_deref(),
        q.gesture.as_deref(),
        q.zone.as_deref(),
        q.after,
        q.before,
        q.flagged,
        100_000,
    )?;
    if let Some(set) = &allow {
        events.retain(|e| set.contains(&e.camera_id));
    }
    let csv = events_to_csv(&events);
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"zoomy-events.csv\"".to_string(),
            ),
        ],
        csv,
    ))
}

const NOTE_MAX_CHARS: usize = 500;

/// Deserialize a *present* field (including an explicit JSON `null`) as
/// `Some(_)`. Combined with `#[serde(default)]`, this lets an absent field
/// (→ `None`) be told apart from `null` (→ `Some(None)`) — plain
/// `Option<Option<T>>` collapses both to `None`.
fn de_some<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct BookmarkReq {
    flagged: bool,
    /// Note handling: omit the field to leave the existing note unchanged; send
    /// `null` or `""` to clear it; send a string (≤500 chars) to set it.
    #[serde(default, deserialize_with = "de_some")]
    note: Option<Option<String>>,
}

/// Bookmark / annotate an event (flag it to keep past retention + attach a note).
async fn bookmark_event(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    Json(req): Json<BookmarkReq>,
) -> ApiResult<Json<serde_json::Value>> {
    // Load the event first so a scoped user can't flag/annotate (and so pin past
    // retention) an event on a camera they can't see. 404 for missing OR forbidden.
    let ev = st.db.get_event(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, ev.camera_id)?;
    let existed = match req.note {
        // Field omitted → only update the flag, leave the note as-is.
        None => st.db.set_event_flag(id, req.flagged)?,
        // Field present → set/clear the note alongside the flag.
        Some(n) => {
            if n.as_deref().map(|s| s.chars().count()).unwrap_or(0) > NOTE_MAX_CHARS {
                return Err(bad_request("note too long (max 500 chars)"));
            }
            let note = n.as_deref().map(str::trim).filter(|s| !s.is_empty());
            st.db.set_event_bookmark(id, req.flagged, note)?
        }
    };
    if !existed {
        return Err(not_found());
    }
    Ok(Json(
        serde_json::json!({ "id": id, "flagged": req.flagged }),
    ))
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
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
    // A scoped user can't create events / fire alarms on a camera they can't see.
    require_camera(&allowed_cameras(&st, &p)?, cam.id)?;

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
            r.matches(cam.id, "gesture", score, None, None, Some(canonical), None)
                // Panic/duress gestures fire regardless of arm mode.
                && (is_duress || crate::notify::armed_in_mode(&r.modes, &settings.arm_mode))
                && crate::notify::ready(r, &st.alarm_throttle, now)
        })
        .collect();
    let mqtt_tx = st.mqtt_tx.clone();
    let webhook_url = settings.webhook_url.clone();
    let base_url = settings.public_base_url.clone();
    let webhook_template = settings.webhook_template.clone();
    // Clone SMTP config into owned strings for the spawned task (the AlarmEvent
    // there borrows from these). None when email isn't configured.
    let smtp_owned = (!settings.smtp_url.trim().is_empty()).then(|| {
        (
            settings.smtp_url.clone(),
            settings.smtp_user.clone(),
            settings.smtp_pass.clone(),
            settings.smtp_from.clone(),
            settings.smtp_to.clone(),
        )
    });
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
            transcript: None,
            speed: None,
            base_url: &base_url,
            webhook_template: &webhook_template,
            smtp: smtp_owned
                .as_ref()
                .map(|(u, us, p, f, t)| crate::notify::SmtpConfig {
                    url: u,
                    user: us,
                    pass: p,
                    from: f,
                    to: t,
                }),
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    req: Request,
) -> ApiResult<Response> {
    let ev = st.db.get_event(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, ev.camera_id)?;
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
                .no_console()
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    req: Request,
) -> ApiResult<Response> {
    // Snapshot names are generated by us ({camera}-{ts}.jpg); reject traversal.
    if file.contains(['/', '\\']) || file.contains("..") {
        return Err(bad_request("bad snapshot name"));
    }
    // Per-camera RBAC: a scoped user may only fetch snapshots of their cameras.
    // Camera names allow '-', so resolve the owning camera via the authoritative
    // events table (not by parsing the filename); deny (404) if it maps to no
    // event or a forbidden camera (fail-closed).
    if let Some(allow) = &allowed_cameras(&st, &p)? {
        match st.db.camera_for_snapshot(&file)? {
            Some(cid) if allow.contains(&cid) => {}
            _ => return Err(not_found()),
        }
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

async fn list_alarms_api(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<crate::db::AlarmRule>>> {
    let mut alarms = st.db.list_alarms()?;
    // A scoped user sees only global rules (camera_id = None) + rules for their
    // cameras — not forbidden cameras' rules (which carry their ids, watch
    // strings, and webhook/MQTT targets).
    if let Some(set) = &allowed_cameras(&st, &p)? {
        alarms.retain(|a| a.camera_id.is_none_or(|cid| set.contains(&cid)));
    }
    Ok(Json(alarms))
}

async fn add_alarm_api(
    State(st): State<AppState>,
    Json(rule): Json<crate::db::AlarmRule>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if rule.name.trim().is_empty() {
        return Err(bad_request("rule name required"));
    }
    // Validate the action list (a "scene"). effective_actions() falls back to
    // the legacy single action for older clients, so this covers both shapes.
    let actions = rule.effective_actions();
    for a in &actions {
        if !matches!(a.kind.as_str(), "webhook" | "mqtt" | "ntfy" | "email") {
            return Err(bad_request(
                "each action must be webhook, mqtt, ntfy or email",
            ));
        }
        // An email action may leave target blank (uses the default smtp_to);
        // every other kind needs an explicit target.
        if a.kind != "email" && a.target.trim().is_empty() {
            return Err(bad_request(
                "each action needs a target (URL or MQTT topic)",
            ));
        }
        if a.priority > 5 {
            return Err(bad_request("action priority must be 0 (default) through 5"));
        }
    }
    for m in &rule.modes {
        if !matches!(m.as_str(), "home" | "away" | "disarmed") {
            return Err(bad_request("modes must be home, away or disarmed"));
        }
    }
    if rule.days.iter().any(|d| *d > 6) {
        return Err(bad_request("days must be 0 (Sunday) through 6 (Saturday)"));
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let query = q.q.trim().to_string();
    if query.is_empty() {
        return Err(bad_request("empty query"));
    }
    let allow = allowed_cameras(&st, &p)?;
    // Hybrid search: CLIP visual similarity on the snapshot (when the models are
    // present) PLUS a text match on the event's transcript + caption — so you
    // can search what was *said* / described, not only what was seen. With no
    // CLIP models it degrades to a pure transcript/caption text search.
    let clip = crate::smart::models_present();
    let qe: Option<Vec<f32>> = if clip {
        let query = query.clone();
        Some(
            tokio::task::spawn_blocking(move || crate::smart::embed_text(&query))
                .await
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??,
        )
    } else {
        None
    };

    let mut scored: Vec<(f32, bool, i64)> = st
        .db
        .search_corpus(clip)?
        .into_iter()
        .map(|row| {
            // cosine of L2-normalized vectors ∈ [-1,1]; clamp to ≥0 (also keeps
            // the sort NaN-free).
            let visual = match (&qe, &row.embedding) {
                (Some(qe), Some(emb)) => crate::smart::cosine(qe, emb).max(0.0),
                _ => 0.0,
            };
            let blob = format!(
                "{} {}",
                row.transcript.as_deref().unwrap_or(""),
                row.caption.as_deref().unwrap_or("")
            );
            let text = crate::smart::text_match_score(&query, &blob);
            // A text hit always ranks above a pure-visual match; visual orders
            // within each band.
            let combined = if text > 0.0 {
                1.0 + text + visual * 0.1
            } else {
                visual
            };
            (combined, text > 0.0, row.id)
        })
        .collect();
    // Only return events with an actual signal — a text hit or non-zero visual
    // similarity — so events that match neither (e.g. audio events with no
    // snapshot embedding) aren't padded in as bogus "visual" results.
    scored.retain(|(score, is_text, _)| *is_text || *score > 0.0);
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Materialize up to `limit` results, skipping events on cameras a scoped
    // user can't see (filter as we go, not after take(), so the count stays honest
    // and no forbidden transcript/caption/snapshot leaks).
    let limit = q.limit.min(100);
    let mut results = Vec::new();
    for (score, is_text, id) in scored.into_iter() {
        if results.len() >= limit {
            break;
        }
        if let Some(ev) = st.db.get_event(id)? {
            if !camera_allowed(&allow, ev.camera_id) {
                continue;
            }
            results.push(serde_json::json!({
                "similarity": score,
                "match": if is_text { "speech" } else { "visual" },
                "event": ev,
            }));
        }
    }
    Ok(Json(serde_json::json!({
        "results": results,
        "mode": if clip { "hybrid" } else { "text" },
    })))
}

/// Cross-camera appearance search ("find this person/vehicle elsewhere"): rank
/// every other event by CLIP cosine similarity of its object crop against this
/// event's crop. `available: false` when the event has no crop embedding (it
/// wasn't an object detection, or smart-search models aren't installed).
async fn event_similar(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(24)
        .min(100);
    let allow = allowed_cameras(&st, &p)?;
    // The seed event must itself be visible to the caller (404 otherwise), so a
    // scoped user can't probe forbidden cameras' crops.
    let seed = st.db.get_event(id)?.ok_or_else(not_found)?;
    require_camera(&allow, seed.camera_id)?;
    let Some(query_emb) = st.db.crop_embedding_for(id)? else {
        return Ok(Json(
            serde_json::json!({ "results": [], "available": false }),
        ));
    };
    // The whole-corpus cosine scan is CPU-bound and unbounded in size, so run it
    // off the async runtime's worker threads (mirrors smart_search's text embed).
    let db = st.db.clone();
    let scored: Vec<(f32, i64)> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let mut scored: Vec<(f32, i64)> = db
            .crop_embeddings()?
            .into_iter()
            .filter(|(eid, _)| *eid != id)
            .map(|(eid, emb)| (crate::smart::cosine(&query_emb, &emb).max(0.0), eid))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let mut results = Vec::new();
    for (score, eid) in scored.into_iter() {
        if results.len() >= limit {
            break;
        }
        if let Some(ev) = st.db.get_event(eid)? {
            if !camera_allowed(&allow, ev.camera_id) {
                continue;
            }
            results.push(serde_json::json!({ "similarity": score, "event": ev }));
        }
    }
    Ok(Json(
        serde_json::json!({ "results": results, "available": true }),
    ))
}

// --- faces -------------------------------------------------------------------

fn safe_file(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && !name.contains("..")
}

/// Enrolled identities + unknown face crops waiting to be named.
async fn faces_overview(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let enrolled = st.db.list_faces()?;
    // Unknown-face crops are camera-derived images with no per-camera tag, so a
    // scoped user must not browse them (they could show people on forbidden
    // cameras). Hide the crop queue for scoped users (leak-safe v1); the enrolled
    // identity library carries no camera data, so it stays visible.
    if allowed_cameras(&st, &p)?.is_some() {
        return Ok(Json(
            serde_json::json!({ "enrolled": enrolled, "unknown": [] }),
        ));
    }
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
    if name == crate::db::UNKNOWN_FACE {
        return Err(bad_request("that name is reserved"));
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
    if name == crate::db::UNKNOWN_FACE {
        return Err(bad_request("that name is reserved"));
    }
    st.db.rename_face(id, name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unknown_face_img(
    State(st): State<AppState>,
    Path(file): Path<String>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    req: Request,
) -> ApiResult<Response> {
    if !safe_file(&file) {
        return Err(bad_request("bad file name"));
    }
    // Unknown-face crops aren't camera-tagged; deny them to scoped users (they
    // could depict people captured on a forbidden camera).
    if allowed_cameras(&st, &p)?.is_some() {
        return Err(not_found());
    }
    let path = st.faces_dir.join("unknown").join(&file);
    if !path.exists() {
        return Err(not_found());
    }
    Ok(ServeFile::new(path).oneshot(req).await.into_response())
}

// --- license-plate library ---------------------------------------------------

fn valid_plate_category(c: &str) -> bool {
    matches!(c, "known" | "watch")
}

async fn list_plates_api(State(st): State<AppState>) -> ApiResult<Json<Vec<crate::db::PlateRow>>> {
    Ok(Json(st.db.list_plates()?))
}

#[derive(Deserialize)]
struct NewPlateReq {
    plate: String,
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

async fn add_plate_api(
    State(st): State<AppState>,
    Json(req): Json<NewPlateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let key = crate::db::normalize_plate(&req.plate);
    if key.is_empty() {
        return Err(bad_request("plate must contain letters or digits"));
    }
    if key.len() > 16 {
        return Err(bad_request("plate too long (max 16 chars)"));
    }
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(bad_request("name must be 1-64 characters"));
    }
    let category = req.category.as_deref().unwrap_or("known");
    if !valid_plate_category(category) {
        return Err(bad_request("category must be 'known' or 'watch'"));
    }
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if note.map(|n| n.chars().count()).unwrap_or(0) > 500 {
        return Err(bad_request("note too long (max 500 characters)"));
    }
    let id = st.db.add_plate(&req.plate, name, category, note)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "plate": key })),
    ))
}

#[derive(Deserialize)]
struct PatchPlateReq {
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

async fn update_plate_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PatchPlateReq>,
) -> ApiResult<StatusCode> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(bad_request("name must be 1-64 characters"));
    }
    let category = req.category.as_deref().unwrap_or("known");
    if !valid_plate_category(category) {
        return Err(bad_request("category must be 'known' or 'watch'"));
    }
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if note.map(|n| n.chars().count()).unwrap_or(0) > 500 {
        return Err(bad_request("note too long (max 500 characters)"));
    }
    if !st.db.update_plate(id, name, category, note)? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_plate_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if !st.db.delete_plate(id)? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<crate::db::SegmentRow>>> {
    let allow = allowed_cameras(&st, &p)?;
    if let Some(cid) = q.camera_id {
        require_camera(&allow, cid)?;
    }
    let mut segs = st.db.list_segments(q.camera_id, q.limit.min(1000))?;
    if let Some(set) = &allow {
        segs.retain(|s| set.contains(&s.camera_id));
    }
    Ok(Json(segs))
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    require_camera(&allowed_cameras(&st, &p)?, q.camera_id)?;
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
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    req: Request,
) -> ApiResult<Response> {
    let seg = st.db.get_segment(id)?.ok_or_else(not_found)?;
    require_camera(&allowed_cameras(&st, &p)?, seg.camera_id)?;
    Ok(ServeFile::new(seg.path).oneshot(req).await.into_response())
}

// --- stats -----------------------------------------------------------------

/// Storage + event totals for the dashboard: per-camera disk usage from the
/// segment index, overall event count, and snapshot footprint.
async fn stats(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let allow = allowed_cameras(&st, &p)?;
    let mut cameras = st.db.storage_stats()?;
    if let Some(set) = &allow {
        cameras.retain(|c| set.contains(&c.camera_id));
    }
    // Scoped users get an event total over only their cameras (the global
    // count_events() would leak the volume on cameras they can't see).
    let events_total: i64 = match &allow {
        Some(set) => st
            .db
            .count_events_in(&set.iter().copied().collect::<Vec<_>>())?,
        None => st.db.count_events()?,
    };
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

    // Storage forecast (UniFi-style): a naive linear extrapolation from the
    // footage on disk. Per camera, rate = bytes / span; summed to a total
    // write rate, then days-until-full and a projected fill date. `None` until a
    // camera has accumulated enough span to estimate (avoids div-by-zero and
    // wild numbers on a camera added minutes ago).
    let now = chrono::Local::now().timestamp();
    let mut write_per_day: f64 = 0.0;
    for c in &cameras {
        if let (Some(o), Some(n)) = (c.oldest_ts, c.newest_ts) {
            // Need at least an hour of span before the rate means anything.
            let span_days = (n - o) as f64 / 86_400.0;
            if span_days >= 1.0 / 24.0 {
                write_per_day += c.bytes as f64 / span_days;
            }
        }
    }
    let write_per_day = write_per_day.round() as u64;
    let days_until_full = (write_per_day > 0).then(|| disk_free as f64 / write_per_day as f64);
    let est_full_ts = days_until_full.map(|d| {
        // Clamp to ~100 years and saturate: a near-zero write rate would
        // otherwise project past i64::MAX and overflow (panic in debug/tests).
        now.saturating_add((d.min(36_500.0) * 86_400.0) as i64)
    });
    // Where retention caps history first: min(retention_days, the GB budget at
    // the current write rate).
    let by_days = (settings.retention_days > 0).then_some(settings.retention_days as f64);
    let by_gb = (settings.retention_gb > 0 && write_per_day > 0)
        .then(|| settings.retention_gb as f64 * 1e9 / write_per_day as f64);
    let retention_horizon_days = match (by_days, by_gb) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };

    Ok(Json(serde_json::json!({
        "cameras": cameras,
        "total_bytes": total_bytes,
        "snapshots_bytes": snapshots_bytes,
        "events_total": events_total,
        "disk_free_bytes": disk_free,
        "recordings_root": rec_root.to_string_lossy(),
        "write_bytes_per_day": write_per_day,
        "days_until_full": days_until_full,
        "est_full_ts": est_full_ts,
        "retention_horizon_days": retention_horizon_days,
    })))
}

// --- A1 overview / A4 notifications / B1 digests -----------------------------

/// Home dashboard aggregator: camera health, today's counts by label, storage,
/// and the unread-notification count — everything the Overview page needs in one
/// round-trip. The online rule mirrors `camera_status` / `metrics`.
/// Tracker-analytics roll-up: true in/out crossing counts (per tripwire +
/// direction) and loiter counts (per zone) over an optional time range.
async fn analytics_counts(
    State(st): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let from = q.get("from").and_then(|s| s.parse::<i64>().ok());
    let to = q.get("to").and_then(|s| s.parse::<i64>().ok());
    let allow = allowed_cameras(&st, &p)?;
    Ok(Json(st.db.analytics_counts(from, to, allow.as_ref())?))
}

/// Live per-camera, per-zone occupancy from the status board — the current count
/// of confirmed tracks inside each zone (cameras with no occupancy are omitted).
/// The gauge is only published while a camera is being ticked, so a stale count
/// would otherwise linger after a camera goes offline or has its zones removed.
/// Guard against that: report only enabled, online cameras and only zones that
/// still exist in the camera's current config.
async fn analytics_occupancy(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let board = st.status.snapshot();
    let cameras = st.db.list_cameras()?;
    let now = chrono::Local::now().timestamp();
    let window = crate::status::freshness_window(st.db.settings().poll_ms);
    let allow = allowed_cameras(&st, &p)?;
    let rows: Vec<serde_json::Value> = cameras
        .iter()
        .filter_map(|c| {
            if !camera_allowed(&allow, c.id) {
                return None;
            }
            let h = board.get(&c.id)?;
            if !c.enabled || !h.is_online(c.detect, now, window) {
                return None;
            }
            // Drop any cached zone whose name is no longer in the live config.
            let current: std::collections::HashSet<&str> = c
                .detect_config
                .zones
                .iter()
                .map(|z| z.name.as_str())
                .collect();
            let zones: std::collections::HashMap<&String, &u32> = h
                .occupancy
                .iter()
                .filter(|(name, _)| current.contains(name.as_str()))
                .collect();
            if zones.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "camera_id": c.id,
                "camera": c.name,
                "zones": zones,
            }))
        })
        .collect();
    Ok(Json(serde_json::json!({ "cameras": rows })))
}

/// Activity heatmap for one camera: a `grid`×`grid` row-major density map of
/// object ground-anchors over an optional time range (`from`/`to` unix secs),
/// plus the peak cell value for client-side normalisation.
async fn analytics_heatmap(
    State(st): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let camera = q
        .get("camera")
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| bad_request("camera id required"))?;
    require_camera(&allowed_cameras(&st, &p)?, camera)?;
    let from = q.get("from").and_then(|s| s.parse::<i64>().ok());
    let to = q.get("to").and_then(|s| s.parse::<i64>().ok());
    let grid = q
        .get("grid")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(32);
    let cells = st.db.heatmap(camera, from, to, grid)?;
    let max = cells.iter().copied().max().unwrap_or(0);
    // The DB clamps grid, so report the actual side length back to the client.
    let side = (cells.len() as f64).sqrt().round() as usize;
    Ok(Json(
        serde_json::json!({ "grid": side, "cells": cells, "max": max }),
    ))
}

async fn overview(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<serde_json::Value>> {
    let allow = allowed_cameras(&st, &p)?;
    let mut cameras = st.db.list_cameras()?;
    if let Some(set) = &allow {
        cameras.retain(|c| set.contains(&c.id));
    }
    let board = st.status.snapshot();
    let settings = st.db.settings();
    let now_dt = chrono::Local::now();
    let now = now_dt.timestamp();
    let window = crate::status::freshness_window(settings.poll_ms);

    let mut online = 0u32;
    let mut recording = 0u32;
    for cam in &cameras {
        if !cam.enabled {
            continue;
        }
        let h = board.get(&cam.id).cloned().unwrap_or_default();
        if h.is_online(cam.detect, now, window) {
            online += 1;
        }
        if h.recording {
            recording += 1;
        }
    }
    let enabled = cameras.iter().filter(|c| c.enabled).count() as u32;

    let today_start = {
        use chrono::Timelike;
        now - now_dt.num_seconds_from_midnight() as i64
    };
    let mut today = st.db.list_events(
        None,
        None,
        None,
        None,
        Some(today_start),
        None,
        false,
        20_000,
    )?;
    if let Some(set) = &allow {
        today.retain(|e| set.contains(&e.camera_id));
    }
    let mut by_label: std::collections::BTreeMap<String, u32> = Default::default();
    for e in &today {
        *by_label.entry(e.label.clone()).or_default() += 1;
    }
    let mut today_by_label: Vec<(String, u32)> = by_label.into_iter().collect();
    today_by_label.sort_by_key(|x| std::cmp::Reverse(x.1));

    let mut storage = st.db.storage_stats()?;
    if let Some(set) = &allow {
        storage.retain(|c| set.contains(&c.camera_id));
    }
    let total_bytes: u64 = storage.iter().map(|c| c.bytes).sum();
    // Scoped users get camera-restricted totals; the global count_events() /
    // count_unread_notifications() would leak volumes on cameras they can't see.
    let events_total: i64 = match &allow {
        Some(set) => st
            .db
            .count_events_in(&set.iter().copied().collect::<Vec<_>>())?,
        None => st.db.count_events()?,
    };
    let unread_notifications: i64 = if allow.is_some() {
        0
    } else {
        st.db.count_unread_notifications()?
    };
    let rec_root = if settings.recordings_dir.trim().is_empty() {
        st.recordings_dir_default.clone()
    } else {
        PathBuf::from(settings.recordings_dir.trim())
    };
    let disk_free = fs2::available_space(&rec_root)
        .or_else(|_| fs2::available_space(std::path::Path::new(".")))
        .unwrap_or(0);

    Ok(Json(serde_json::json!({
        "cameras_total": enabled,
        "cameras_online": online,
        "recording": recording,
        "events_total": events_total,
        "events_today": today.len(),
        "disk_free_bytes": disk_free,
        "total_bytes": total_bytes,
        "today_by_label": today_by_label,
        "unread_notifications": unread_notifications,
        "arm_mode": settings.arm_mode,
    })))
}

async fn get_arm_mode(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "arm_mode": st.db.settings().arm_mode }))
}

#[derive(Deserialize)]
struct ArmReq {
    mode: String,
}

/// Set the system-wide security mode (UniFi-style Home / Away / Disarmed). Gates
/// which alarm rules fire (see `notify::armed_in_mode`). Audited, raises an
/// in-app notification, and publishes the new mode to MQTT for HA / keypad
/// automations to read.
async fn set_arm_mode(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ArmReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let mode = req.mode.trim().to_lowercase();
    if !matches!(mode.as_str(), "home" | "away" | "disarmed") {
        return Err(bad_request("mode must be home, away or disarmed"));
    }
    if st.db.settings().arm_mode == mode {
        return Ok(Json(serde_json::json!({ "arm_mode": mode })));
    }
    // Single-key write (no read-modify-write of the settings blob), so a
    // concurrent Settings-page save can't clobber it.
    st.db.set_kv("arm_mode", &mode)?;

    let ip = crate::auth::client_ip(&headers, addr.ip(), st.behind_proxy)
        .0
        .to_string();
    let now = chrono::Local::now().timestamp();
    st.db
        .add_audit(now, Some(&ip), "arm_mode_changed", Some(&mode));
    let _ = st.db.add_notification(
        now,
        "mode",
        &format!("System {}", mode_phrase(&mode)),
        Some(&format!("Security mode set to {mode}.")),
        None,
    );
    // Publish to MQTT (retained-style state topic) for inbound automations.
    let _ = st.mqtt_tx.send(crate::mqtt::EventMsg {
        event_id: 0,
        camera: String::new(),
        label: mode.clone(),
        score: 0.0,
        ts: now,
        snapshot: String::new(),
        topic: Some("mode".to_string()),
    });
    Ok(Json(serde_json::json!({ "arm_mode": mode })))
}

fn mode_phrase(mode: &str) -> &'static str {
    match mode {
        "home" => "armed (Home)",
        "disarmed" => "disarmed",
        _ => "armed (Away)",
    }
}

#[derive(Deserialize)]
struct NotificationsQuery {
    #[serde(default)]
    unread: bool,
    #[serde(default = "default_limit")]
    limit: u32,
}

async fn list_notifications_api(
    State(st): State<AppState>,
    Query(q): Query<NotificationsQuery>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<crate::db::Notification>>> {
    // Notifications aren't yet per-camera attributed, and they reference cameras
    // (offline/anomaly/stranger) a scoped user may not see — so a scoped user
    // gets none (leak-safe). Per-camera notification scoping is a follow-up.
    if allowed_cameras(&st, &p)?.is_some() {
        return Ok(Json(vec![]));
    }
    Ok(Json(st.db.list_notifications(q.unread, q.limit.min(1000))?))
}

async fn mark_notification_read_api(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !st.db.mark_notification_read(id)? {
        return Err(not_found());
    }
    Ok(Json(serde_json::json!({ "id": id, "read": true })))
}

async fn mark_all_notifications_read_api(
    State(st): State<AppState>,
) -> ApiResult<Json<serde_json::Value>> {
    let updated = st.db.mark_all_notifications_read()?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

#[derive(Deserialize)]
struct DigestsQuery {
    #[serde(default = "default_limit")]
    limit: u32,
}

async fn list_digests_api(
    State(st): State<AppState>,
    Query(q): Query<DigestsQuery>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<Vec<crate::db::Digest>>> {
    // Digests are pre-rendered cross-camera recaps with no per-camera field, so
    // they can't be retro-scoped — a scoped user gets none (leak-safe).
    if allowed_cameras(&st, &p)?.is_some() {
        return Ok(Json(vec![]));
    }
    Ok(Json(st.db.list_digests(q.limit.min(366))?))
}

/// Generate a digest for the last 24 hours immediately (manual "run now").
async fn run_digest_api(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<Json<crate::db::Digest>> {
    // Digests are whole-system cross-camera recaps (identities, plates, busiest
    // camera). A scoped user must not generate/receive one — mirror the empty
    // GET /api/digests behavior (leak-safe v1).
    if allowed_cameras(&st, &p)?.is_some() {
        return Err(forbidden(
            "digests aren't available for camera-scoped accounts",
        ));
    }
    let now = chrono::Local::now().timestamp();
    let events = st.db.list_events(
        None,
        None,
        None,
        None,
        Some(now - 86_400),
        None,
        false,
        20_000,
    )?;
    let text = crate::digest::summarize(&events);
    let id = st.db.add_digest(now, &text)?;
    Ok(Json(crate::db::Digest { id, ts: now, text }))
}

// --- Prometheus metrics ------------------------------------------------------

/// Per-camera figures the metrics endpoint exposes (gathered from the DB +
/// status board, then rendered by the pure `render_metrics`).
struct CamMetric {
    name: String,
    online: bool,
    recording: bool,
    inference_ms: Option<f32>,
    last_frame_age: Option<i64>,
    bytes: u64,
    segments: i64,
}

/// Escape a Prometheus label value (backslash, double-quote, newline).
fn esc_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Render the metrics exposition text (Prometheus 0.0.4). Pure so it's unit-
/// testable without a server.
fn render_metrics(version: &str, events: i64, disk_free: u64, cams: &[CamMetric]) -> String {
    let online = cams.iter().filter(|c| c.online).count();
    let mut out = String::new();
    let family = |out: &mut String, name: &str, help: &str, kind: &str| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
    };
    family(&mut out, "zoomy_build_info", "Build information.", "gauge");
    out.push_str(&format!(
        "zoomy_build_info{{version=\"{}\"}} 1\n",
        esc_label(version)
    ));
    family(&mut out, "zoomy_cameras", "Configured cameras.", "gauge");
    out.push_str(&format!("zoomy_cameras {}\n", cams.len()));
    family(
        &mut out,
        "zoomy_cameras_online",
        "Cameras currently online.",
        "gauge",
    );
    out.push_str(&format!("zoomy_cameras_online {online}\n"));
    family(
        &mut out,
        "zoomy_events",
        "Events currently stored.",
        "gauge",
    );
    out.push_str(&format!("zoomy_events {events}\n"));
    family(
        &mut out,
        "zoomy_disk_free_bytes",
        "Free space on the recordings volume.",
        "gauge",
    );
    out.push_str(&format!("zoomy_disk_free_bytes {disk_free}\n"));

    family(
        &mut out,
        "zoomy_camera_online",
        "Camera online (1) or offline (0).",
        "gauge",
    );
    for c in cams {
        out.push_str(&format!(
            "zoomy_camera_online{{camera=\"{}\"}} {}\n",
            esc_label(&c.name),
            c.online as u8
        ));
    }
    family(
        &mut out,
        "zoomy_camera_recording",
        "Recorder process alive (1/0).",
        "gauge",
    );
    for c in cams {
        out.push_str(&format!(
            "zoomy_camera_recording{{camera=\"{}\"}} {}\n",
            esc_label(&c.name),
            c.recording as u8
        ));
    }
    family(
        &mut out,
        "zoomy_camera_storage_bytes",
        "Recorded bytes on disk per camera.",
        "gauge",
    );
    for c in cams {
        out.push_str(&format!(
            "zoomy_camera_storage_bytes{{camera=\"{}\"}} {}\n",
            esc_label(&c.name),
            c.bytes
        ));
    }
    family(
        &mut out,
        "zoomy_camera_segments",
        "Recorded segments per camera.",
        "gauge",
    );
    for c in cams {
        out.push_str(&format!(
            "zoomy_camera_segments{{camera=\"{}\"}} {}\n",
            esc_label(&c.name),
            c.segments
        ));
    }
    family(
        &mut out,
        "zoomy_camera_inference_ms",
        "Last detector inference latency (ms).",
        "gauge",
    );
    for c in cams {
        if let Some(ms) = c.inference_ms {
            out.push_str(&format!(
                "zoomy_camera_inference_ms{{camera=\"{}\"}} {ms:.1}\n",
                esc_label(&c.name)
            ));
        }
    }
    family(
        &mut out,
        "zoomy_camera_last_frame_age_seconds",
        "Seconds since the last decoded frame.",
        "gauge",
    );
    for c in cams {
        if let Some(age) = c.last_frame_age {
            out.push_str(&format!(
                "zoomy_camera_last_frame_age_seconds{{camera=\"{}\"}} {age}\n",
                esc_label(&c.name)
            ));
        }
    }
    out
}

/// Prometheus metrics exposition. Gated by the same auth as the rest of `/api`,
/// so a scraper authenticates with a Bearer token (or runs on the loopback box).
async fn metrics(
    State(st): State<AppState>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> ApiResult<impl IntoResponse> {
    let now = chrono::Local::now().timestamp();
    let settings = st.db.settings();
    let window = crate::status::freshness_window(settings.poll_ms);
    // Per-camera RBAC: a scoped user (or its matching SSO account) sees only their
    // cameras' series; a token/admin/loopback scraper (user_id=None) is unrestricted.
    let allow = allowed_cameras(&st, &p)?;
    let cameras: Vec<_> = st
        .db
        .list_cameras()?
        .into_iter()
        .filter(|c| camera_allowed(&allow, c.id))
        .collect();
    let storage = st.db.storage_stats()?;
    let health = st.status.snapshot();
    let store: std::collections::HashMap<i64, &crate::db::CamStorage> =
        storage.iter().map(|s| (s.camera_id, s)).collect();
    let cams: Vec<CamMetric> = cameras
        .iter()
        .map(|cam| {
            let h = health.get(&cam.id).cloned().unwrap_or_default();
            let s = store.get(&cam.id);
            CamMetric {
                name: cam.name.clone(),
                online: cam.enabled && h.is_online(cam.detect, now, window),
                recording: h.recording,
                inference_ms: h.inference_ms,
                last_frame_age: h.last_frame_ts.map(|t| (now - t).max(0)),
                bytes: s.map(|s| s.bytes).unwrap_or(0),
                segments: s.map(|s| s.segments).unwrap_or(0),
            }
        })
        .collect();
    let rec_root = if settings.recordings_dir.trim().is_empty() {
        st.recordings_dir_default.clone()
    } else {
        PathBuf::from(settings.recordings_dir.trim())
    };
    let disk_free = fs2::available_space(&rec_root)
        .or_else(|_| fs2::available_space(std::path::Path::new(".")))
        .unwrap_or(0);
    let events_total = match &allow {
        Some(set) => st
            .db
            .count_events_in(&set.iter().copied().collect::<Vec<_>>())?,
        None => st.db.count_events()?,
    };
    let body = render_metrics(env!("CARGO_PKG_VERSION"), events_total, disk_free, &cams);
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    ))
}

// --- API tokens --------------------------------------------------------------

async fn list_tokens(State(st): State<AppState>) -> ApiResult<Json<Vec<crate::db::ApiToken>>> {
    Ok(Json(st.db.list_api_tokens()?))
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default = "default_audit_limit")]
    limit: u32,
}
fn default_audit_limit() -> u32 {
    200
}

/// Recent security-audit entries (logins, password changes, token changes),
/// newest first. Gated like the rest of `/api`.
async fn list_audit(
    State(st): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Vec<crate::db::AuditEntry>>> {
    Ok(Json(st.db.list_audit(q.limit.min(1000))?))
}

#[derive(Deserialize)]
struct NewTokenReq {
    name: String,
    /// Scope for the token: "viewer" (read-only), "operator" (read+mutate, the
    /// default), or "admin" (incl. backup/restore). Unknown values fall back to
    /// viewer (least privilege). Token-management + password endpoints stay
    /// blocked for every token regardless of role (`token_forbidden`).
    #[serde(default)]
    role: Option<String>,
}

/// Mint an API token. The raw token is returned exactly once here and never
/// stored or shown again — only its hash is kept. A token grants its assigned
/// role's API access (minus the interactive-only endpoints), so it's only useful
/// once a password or user account exists.
async fn create_token(
    State(st): State<AppState>,
    Json(req): Json<NewTokenReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(bad_request("token name required"));
    }
    if name.chars().count() > 64 {
        return Err(bad_request("token name too long (max 64 chars)"));
    }
    // Default new tokens to operator (a secure, useful default); the creator can
    // pick viewer for a dashboard token or admin for a backup script.
    let role = match req.role.as_deref() {
        Some(r) if !r.is_empty() => crate::auth::Role::parse(r).as_str(),
        _ => crate::auth::Role::Operator.as_str(),
    };
    let raw = format!("zoomy_{}", crate::auth::new_token());
    let now = chrono::Local::now().timestamp();
    let id = st
        .db
        .add_api_token(name, &crate::auth::token_hash(&raw), role, now)?;
    st.db.add_audit(
        now,
        None,
        "token_created",
        Some(&format!("{name} ({role})")),
    );
    Ok(Json(
        serde_json::json!({ "id": id, "name": name, "role": role, "token": raw }),
    ))
}

async fn delete_token(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !st.db.delete_api_token(id)? {
        return Err(not_found());
    }
    st.db.add_audit(
        chrono::Local::now().timestamp(),
        None,
        "token_revoked",
        Some(&format!("id {id}")),
    );
    Ok(Json(serde_json::json!({ "deleted": id })))
}

// --- settings ----------------------------------------------------------------

async fn get_settings(State(st): State<AppState>) -> Json<Settings> {
    let mut s = st.db.settings();
    // GET /api/settings is Viewer-reachable; never hand the SMTP password back.
    // The field is write-only: a blank smtp_pass on save preserves the stored one
    // (see put_settings), so blanking it here doesn't lose it.
    if !s.smtp_pass.is_empty() {
        s.smtp_pass = String::new();
    }
    Json(s)
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
    // SMTP password is write-only (get_settings blanks it): a blank incoming
    // value means "unchanged", so restore the stored one rather than wipe it.
    let mut s = s;
    if s.smtp_pass.is_empty() {
        s.smtp_pass = st.db.settings().smtp_pass;
    }
    st.db.save_settings(&s)?;
    let mut out = st.db.settings();
    out.smtp_pass = String::new(); // never echo the secret back
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::{
        csv_field, events_to_csv, no_control, render_metrics, valid_group, valid_source,
        BookmarkReq, CamMetric,
    };

    #[test]
    fn csv_field_quotes_and_guards_against_formula_injection() {
        // Plain values pass through.
        assert_eq!(csv_field("person"), "person");
        // Comma / quote / newline force quoting; internal quotes are doubled.
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("line1\nline2"), "\"line1\nline2\"");
        // Formula-injection leads get a `'` guard (and then quoting if needed).
        assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(csv_field("@cmd"), "'@cmd");
        assert_eq!(csv_field("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn events_to_csv_has_header_and_rows() {
        let ev = crate::db::Event {
            id: 7,
            camera_id: 1,
            camera: "porch".into(),
            ts: 0,
            label: "person".into(),
            score: 0.91234,
            bbox: [0.0; 4],
            snapshot: None,
            face: Some("Bob".into()),
            plate: None,
            gesture: None,
            zone: None,
            caption: None,
            transcript: Some("help, fire".into()), // comma → must be quoted
            flagged: true,
            note: None,
            anomaly_score: None,
            direction: Some("a_to_b".into()),
            speed: Some(32.4),
        };
        let csv = events_to_csv(std::slice::from_ref(&ev));
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "id,time,camera,label,score,face,plate,gesture,zone,direction,speed,flagged,note,caption,transcript"
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("7,"));
        assert!(row.contains(",porch,person,0.912,Bob,"));
        assert!(row.contains(",a_to_b,32,")); // direction + rounded speed
        assert!(row.contains(",yes,")); // flagged
        assert!(row.ends_with(",\"help, fire\"")); // transcript quoted
    }

    #[test]
    fn metrics_render_format_and_label_escaping() {
        let cams = vec![
            CamMetric {
                name: "porch".into(),
                online: true,
                recording: true,
                inference_ms: Some(8.74),
                last_frame_age: Some(2),
                bytes: 1024,
                segments: 5,
            },
            // A name with characters that must be escaped in a Prometheus label.
            CamMetric {
                name: "a\"b\\c".into(),
                online: false,
                recording: false,
                inference_ms: None,
                last_frame_age: None,
                bytes: 0,
                segments: 0,
            },
        ];
        let m = render_metrics("0.1.0", 42, 9999, &cams);
        // Global gauges.
        assert!(m.contains("zoomy_build_info{version=\"0.1.0\"} 1\n"));
        assert!(m.contains("\nzoomy_cameras 2\n"));
        assert!(m.contains("\nzoomy_cameras_online 1\n"));
        assert!(m.contains("\nzoomy_events 42\n"));
        assert!(m.contains("\nzoomy_disk_free_bytes 9999\n"));
        // Per-camera, with HELP/TYPE headers and escaped labels.
        assert!(m.contains("# TYPE zoomy_camera_online gauge\n"));
        assert!(m.contains("zoomy_camera_online{camera=\"porch\"} 1\n"));
        assert!(m.contains("zoomy_camera_storage_bytes{camera=\"porch\"} 1024\n"));
        assert!(m.contains("zoomy_camera_inference_ms{camera=\"porch\"} 8.7\n"));
        assert!(m.contains("zoomy_camera_last_frame_age_seconds{camera=\"porch\"} 2\n"));
        // Escaped name: " -> \" and \ -> \\.
        assert!(m.contains("zoomy_camera_online{camera=\"a\\\"b\\\\c\"} 0\n"));
        // The offline camera has no inference/last-frame lines (None skipped).
        assert!(!m.contains("zoomy_camera_inference_ms{camera=\"a\\\"b\\\\c\"}"));
    }

    #[test]
    fn bookmark_note_distinguishes_absent_null_and_value() {
        // Absent note → outer None → preserve the stored note.
        let r: BookmarkReq = serde_json::from_str(r#"{"flagged":true}"#).unwrap();
        assert!(r.flagged);
        assert!(r.note.is_none());
        // Explicit null → Some(None) → clear the note.
        let r: BookmarkReq = serde_json::from_str(r#"{"flagged":false,"note":null}"#).unwrap();
        assert!(!r.flagged);
        assert_eq!(r.note, Some(None));
        // String → Some(Some(_)) → set the note.
        let r: BookmarkReq = serde_json::from_str(r#"{"flagged":true,"note":"hi"}"#).unwrap();
        assert_eq!(r.note, Some(Some("hi".to_string())));
    }

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
