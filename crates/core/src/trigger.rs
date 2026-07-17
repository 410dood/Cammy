//! Shared soft-trigger machinery: record a bookmarked, snapshot-backed event and
//! fire matching alarm rules. Used by BOTH the HTTP soft-trigger endpoint
//! (`POST /api/cameras/{id}/trigger`, an Nx Witness-style "Log event" button) and
//! the inbound MQTT command surface (`<mqtt_prefix>/cmd/trigger`), so a trigger
//! from either source produces an identical first-class event that rides the
//! normal MQTT / webhook / alarm machinery.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::db::{Camera, Db};
use crate::go2rtc::Go2Rtc;
use crate::mqtt::EventMsg;
use crate::notify::AlarmThrottle;

/// Fetch the camera's current frame from go2rtc and write it to `path`, with the
/// camera's privacy masks burned in — this is a raw frame grab (unlike the
/// detection pipeline, which masks before analysis), so without this a
/// gesture/soft-trigger snapshot would leak masked regions into pushes. Fails
/// CLOSED on a decode error (better no snapshot than an unmasked one).
pub(crate) fn save_masked_snapshot(
    api_base: &str,
    camera: &str,
    masks: &[Vec<[f32; 2]>],
    path: &Path,
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
    if !masks.is_empty() {
        let mut img = image::load_from_memory(&bytes)?;
        crate::pipeline::apply_privacy_masks(&mut img, masks);
        img.save(path)?;
        return Ok(());
    }
    std::fs::write(path, &bytes)?;
    Ok(())
}

/// Everything a soft trigger needs besides the (camera, label, timestamp). Cheap
/// to clone (every field is an `Arc`/handle/path), so a request handler can hand
/// one copy to the blocking record task and another to the detached alarm task.
#[derive(Clone)]
pub(crate) struct TriggerCtx {
    pub db: Db,
    pub go2rtc: Arc<Go2Rtc>,
    pub snapshots_dir: PathBuf,
    pub mqtt_tx: Sender<EventMsg>,
    pub alarm_throttle: AlarmThrottle,
}

/// The outcome of [`record_event`], carried into [`fire_alarms`].
pub(crate) struct Recorded {
    pub event_id: i64,
    /// Web path to the context snapshot ("" when the grab failed).
    pub snapshot_url: String,
    /// Absolute path to the snapshot file, for attaching to a push (`None` when
    /// the grab failed).
    pub snapshot_abs: Option<PathBuf>,
}

/// Record a soft-trigger event: best-effort context snapshot → `add_event` →
/// bookmark (a deliberate trigger is a moment to keep, so it survives event
/// retention) → base MQTT/SSE publish (`topic: None`). Fully synchronous
/// (blocking snapshot fetch + DB); from an async handler, run it on a blocking
/// task. Returns the new event id.
pub(crate) fn record_event(
    ctx: &TriggerCtx,
    cam: &Camera,
    label: &str,
    now: i64,
) -> anyhow::Result<Recorded> {
    // Best-effort context snapshot of what the camera sees right now.
    let snap_rel = format!("{}-trigger-{}.jpg", cam.name, now);
    let snap_abs = ctx.snapshots_dir.join(&snap_rel);
    let ok = save_masked_snapshot(
        &ctx.go2rtc.api_base(),
        &cam.name,
        &cam.detect_config.privacy_masks,
        &snap_abs,
    )
    .is_ok();
    let snapshot_rel = ok.then(|| snap_rel.clone());
    let snapshot_abs = ok.then(|| snap_abs.clone());

    let id = ctx.db.add_event(
        cam.id,
        now,
        label,
        1.0,
        [0.0; 4],
        snapshot_rel.as_deref(),
        None,
        None,
        None,
        None,
    )?;
    let _ = ctx.db.set_event_bookmark(id, true, None);
    tracing::info!(camera = %cam.name, label, event = id, "soft trigger recorded");

    let snapshot_url = snapshot_rel
        .as_ref()
        .map(|s| format!("/api/snapshots/{s}"))
        .unwrap_or_default();
    // Base publish (topic None): goes to `{prefix}/events` + the SSE feed.
    let _ = ctx.mqtt_tx.send(EventMsg {
        event_id: id,
        camera: cam.name.clone(),
        label: label.to_string(),
        score: 1.0,
        ts: now,
        snapshot: snapshot_url.clone(),
        topic: None,
    });

    Ok(Recorded {
        event_id: id,
        snapshot_url,
        snapshot_abs,
    })
}

/// Fire every alarm rule that matches a just-recorded soft-trigger event,
/// respecting the shared per-rule cooldown, zone/confirm gates and arm mode —
/// exactly like a pipeline-detected event. Synchronous (blocking webhook/relay
/// I/O inside `notify::fire`); from an async handler, run detached on a blocking
/// task so the HTTP response isn't held up.
pub(crate) fn fire_alarms(ctx: &TriggerCtx, cam: &Camera, label: &str, now: i64, rec: &Recorded) {
    let settings = ctx.db.settings();
    let rules: Vec<(crate::db::AlarmRule, u32)> = match ctx.db.list_alarms() {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!("soft trigger: list_alarms failed: {e:#}");
            return;
        }
    }
    .into_iter()
    .filter(|r| {
        r.matches(cam.id, label, 1.0, None, None, None, None)
            && r.zone_ok(None)
            && r.confirm_ok(&ctx.db, cam.id, now)
            && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
            && crate::notify::ready(r, &ctx.alarm_throttle, now)
    })
    .map(|r| {
        let suppressed = crate::notify::take_suppressed(&ctx.alarm_throttle, r.id);
        (r, suppressed)
    })
    .collect();
    if rules.is_empty() {
        return;
    }
    let ev = crate::notify::AlarmEvent {
        event_id: rec.event_id,
        camera: &cam.name,
        label,
        score: 1.0,
        ts: now,
        snapshot_url: &rec.snapshot_url,
        snapshot_path: rec.snapshot_abs.as_deref(),
        face: None,
        plate: None,
        gesture: None,
        transcript: None,
        speed: None,
        base_url: &settings.public_base_url,
        webhook_template: &settings.webhook_template,
        smtp: crate::notify::smtp_cfg(&settings),
        duress: false,
        severity: crate::severity::severity_for(label, None, None),
        min_push_severity: settings.notify_min_severity,
        caption: None,
    };
    for (rule, suppressed) in &rules {
        crate::notify::fire(rule, &ev, &ctx.mqtt_tx, *suppressed, &ctx.db);
    }
}
