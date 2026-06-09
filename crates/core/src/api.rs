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
    pub status: StatusBoard,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route("/api/status", get(camera_status))
        .route("/api/cameras", get(list_cameras).post(add_camera))
        .route(
            "/api/cameras/{id}",
            get(get_camera).patch(patch_camera).delete(delete_camera),
        )
        .route("/api/events", get(list_events))
        .route("/api/snapshots/{file}", get(snapshot))
        .route("/api/recordings", get(list_recordings))
        .route("/api/recordings/at", get(recording_at))
        .route("/api/recordings/{id}/video", get(segment_video))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/stats", get(stats))
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
            }),
        );
    }
    Ok(Json(serde_json::Value::Object(out)))
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

async fn add_camera(
    State(st): State<AppState>,
    Json(body): Json<NewCamera>,
) -> ApiResult<(StatusCode, Json<Camera>)> {
    if !valid_name(&body.name) {
        return Err(bad_request(
            "camera name must be 1-32 chars of a-z, 0-9, '-', '_'",
        ));
    }
    if body.source.trim().is_empty() {
        return Err(bad_request("source must not be empty"));
    }
    let detect_source = body
        .detect_source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let cam = st
        .db
        .add_camera(
            &body.name,
            body.source.trim(),
            detect_source,
            body.detect,
            body.record,
        )
        .map_err(|e| bad_request(format!("could not add camera: {e}")))?;
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
}

async fn patch_camera(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<CameraPatch>,
) -> ApiResult<Json<Camera>> {
    let mut cam = st.db.get_camera(id)?.ok_or_else(not_found)?;
    if let Some(name) = patch.name {
        if !valid_name(&name) {
            return Err(bad_request("invalid camera name"));
        }
        cam.name = name;
    }
    if let Some(source) = patch.source {
        cam.source = source;
    }
    if let Some(ds) = patch.detect_source {
        let ds = ds.trim();
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
        cam.detect_config = dc;
    }
    st.db.update_camera(&cam)?;
    st.go2rtc.restart_with(&st.db)?;
    Ok(Json(cam))
}

async fn delete_camera(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    st.db.get_camera(id)?.ok_or_else(not_found)?;
    st.db.delete_camera(id)?;
    st.go2rtc.restart_with(&st.db)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- events ----------------------------------------------------------------

#[derive(Deserialize)]
struct EventQuery {
    camera_id: Option<i64>,
    label: Option<String>,
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
        q.before,
        q.limit.min(1000),
    )?))
}

async fn snapshot(
    State(st): State<AppState>,
    Path(file): Path<String>,
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
    Ok(Json(serde_json::json!({
        "cameras": cameras,
        "total_bytes": total_bytes,
        "snapshots_bytes": snapshots_bytes,
        "events_total": st.db.count_events()?,
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
    st.db.save_settings(&s)?;
    Ok(Json(st.db.settings()))
}
