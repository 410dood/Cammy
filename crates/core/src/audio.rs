//! Audio event detection (Frigate/UniFi style): short waveform captures from
//! each audio-enabled camera are classified with YAMNet (AudioSet, 521
//! classes); security-relevant sounds — glass, gunshots, sirens, screams —
//! become events with snapshots and flow through Alarm Manager and MQTT like
//! any detection.
//!
//! Runs on its own worker thread. Capture is one short ffmpeg invocation per
//! camera per cycle against go2rtc's RTSP restream (16 kHz mono f32), so the
//! cost scales with the number of audio-enabled cameras only.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use detector::ort::session::Session;
use detector::ort::value::Tensor;

use crate::db::Db;
use crate::go2rtc::Go2Rtc;
use crate::proc::NoConsole as _;

pub const MODEL: &str = "yamnet.onnx";
pub const CLASS_MAP: &str = "yamnet_class_map.csv";

const SAMPLE_RATE: u32 = 16_000;
const CAPTURE_SECS: u32 = 1;
/// Per-(camera, class) event cooldown.
const COOLDOWN_SECS: i64 = 30;

pub fn models_present() -> bool {
    std::path::Path::new(MODEL).exists() && std::path::Path::new(CLASS_MAP).exists()
}

struct Engine {
    session: Session,
    classes: Vec<String>,
}

impl Engine {
    fn try_new() -> Result<Self> {
        let classes = std::fs::read_to_string(CLASS_MAP)
            .context("reading yamnet class map")?
            .lines()
            .skip(1) // header
            .map(|l| {
                // index,mid,display_name — display name may be quoted with commas.
                let after = l.splitn(3, ',').nth(2).unwrap_or("").trim();
                after.trim_matches('"').to_string()
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(classes.len() > 500, "unexpected yamnet class map");
        Ok(Self {
            session: detector::build_ort_session(MODEL, true)?,
            classes,
        })
    }

    /// Mean score per class across YAMNet's internal frames.
    fn classify(&mut self, waveform: Vec<f32>) -> Result<Vec<f32>> {
        let n = waveform.len();
        let input = Tensor::from_array(([n], waveform))?;
        let outputs = self
            .session
            .run(detector::ort::inputs!["waveform" => input])?;
        let (_name, value) = outputs.iter().next().context("no scores output")?;
        let (shape, data) = value.try_extract_tensor::<f32>()?;
        let frames = shape[0] as usize;
        let classes = shape[1] as usize;
        let mut mean = vec![0.0f32; classes];
        for f in 0..frames {
            for c in 0..classes {
                mean[c] += data[f * classes + c];
            }
        }
        for m in &mut mean {
            *m /= frames.max(1) as f32;
        }
        Ok(mean)
    }
}

/// Capture ~1s of 16 kHz mono audio from the camera's restream.
fn capture(ffmpeg: &std::path::Path, rtsp_url: &str) -> Result<Vec<f32>> {
    let mut child = std::process::Command::new(ffmpeg)
        .args(["-loglevel", "error", "-rtsp_transport", "tcp", "-i"])
        .arg(rtsp_url)
        .args(["-t", &CAPTURE_SECS.to_string(), "-vn", "-ac", "1"])
        .args(["-ar", &SAMPLE_RATE.to_string(), "-f", "f32le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .no_console()
        .spawn()
        .context("spawning ffmpeg audio capture")?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .context("no stdout")?
        .read_to_end(&mut bytes)?;
    let _ = child.wait();
    anyhow::ensure!(bytes.len() >= 4 * 1600, "no audio in stream");
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    ffmpeg_bin: Option<PathBuf>,
    snapshots_dir: PathBuf,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: crate::notify::AlarmThrottle,
    transcribe_tx: std::sync::mpsc::Sender<crate::transcribe::TranscribeJob>,
    shutdown: Arc<AtomicBool>,
) {
    let Ok(ffmpeg) = recorder::locate_ffmpeg(ffmpeg_bin.as_deref()) else {
        tracing::warn!("audio detection disabled: ffmpeg not found");
        return;
    };
    let mut engine: Option<Engine> = None;
    let mut last_fire: HashMap<(i64, String), i64> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let cameras = db.list_cameras().unwrap_or_default();
        let targets: Vec<_> = cameras
            .iter()
            .filter(|c| c.enabled && c.detect_config.audio_detect)
            .collect();

        if targets.is_empty() || !models_present() {
            sleep_responsive(Duration::from_secs(3), &shutdown);
            continue;
        }
        if engine.is_none() {
            match Engine::try_new() {
                Ok(e) => {
                    tracing::info!("audio detection (YAMNet) ready");
                    engine = Some(e);
                }
                Err(e) => {
                    tracing::warn!("audio engine unavailable: {e:#}");
                    sleep_responsive(Duration::from_secs(30), &shutdown);
                    continue;
                }
            }
        }
        let engine = engine.as_mut().expect("built above");
        let alarms = db.list_alarms().unwrap_or_default();

        for cam in &targets {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            let waveform = match capture(&ffmpeg, &go2rtc.rtsp_url(&cam.name)) {
                Ok(w) => w,
                Err(e) => {
                    tracing::debug!(camera = %cam.name, "audio capture: {e:#}");
                    continue;
                }
            };
            let scores = match engine.classify(waveform) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(camera = %cam.name, "audio classify: {e:#}");
                    continue;
                }
            };

            let now = chrono::Local::now().timestamp();
            for monitored in &settings.audio_labels {
                let Some(idx) = engine
                    .classes
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(monitored))
                else {
                    continue;
                };
                let score = scores[idx];
                if score < settings.audio_threshold {
                    continue;
                }
                let label = monitored.to_lowercase();
                let key = (cam.id, label.clone());
                if last_fire
                    .get(&key)
                    .map(|t| now - t < COOLDOWN_SECS)
                    .unwrap_or(false)
                {
                    continue;
                }
                last_fire.insert(key, now);

                // Grab a frame for visual context where possible.
                let snap_rel = format!("{}-{}-audio.jpg", cam.name, now);
                let snapshot = fetch_snapshot(&go2rtc.api_base(), &cam.name)
                    .and_then(|bytes| {
                        std::fs::create_dir_all(&snapshots_dir).ok();
                        std::fs::write(snapshots_dir.join(&snap_rel), bytes).ok()
                    })
                    .map(|_| snap_rel.clone());

                match db.add_event(
                    cam.id,
                    now,
                    &label,
                    score,
                    [0.0; 4],
                    snapshot.as_deref(),
                    None,
                    None,
                    None,
                    None,
                ) {
                    Ok(id) => {
                        tracing::info!(
                            camera = %cam.name,
                            sound = %label,
                            score = format!("{score:.2}"),
                            event = id,
                            "audio event"
                        );
                        let _ = mqtt_tx.send(crate::mqtt::EventMsg {
                            event_id: id,
                            camera: cam.name.clone(),
                            label: label.clone(),
                            score,
                            ts: now,
                            snapshot: format!("/api/snapshots/{snap_rel}"),
                            topic: None,
                        });
                        // Opt-in speech-to-text: hand the event to the transcriber,
                        // which captures a short clip and writes back a transcript.
                        if settings.transcription_enabled {
                            let _ = transcribe_tx.send(crate::transcribe::TranscribeJob {
                                event_id: id,
                                camera: cam.name.clone(),
                            });
                        }
                        let snap_abs = snapshots_dir.join(&snap_rel);
                        let alarm_ev = crate::notify::AlarmEvent {
                            event_id: id,
                            camera: &cam.name,
                            label: &label,
                            score,
                            ts: now,
                            snapshot_url: &format!("/api/snapshots/{snap_rel}"),
                            snapshot_path: snapshot.is_some().then_some(snap_abs.as_path()),
                            face: None,
                            plate: None,
                            gesture: None,
                            transcript: None,
                            base_url: &settings.public_base_url,
                            webhook_template: &settings.webhook_template,
                            smtp: crate::notify::smtp_cfg(&settings),
                            duress: false,
                        };
                        for rule in alarms.iter().filter(|r| {
                            r.matches(cam.id, &label, score, None, None, None, None)
                                && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
                                && crate::notify::ready(r, &throttle, now)
                        }) {
                            crate::notify::fire(rule, &alarm_ev, &mqtt_tx);
                        }
                    }
                    Err(e) => tracing::warn!("audio event insert failed: {e:#}"),
                }
            }
        }

        sleep_responsive(Duration::from_secs(2), &shutdown);
    }
}

fn fetch_snapshot(api_base: &str, camera: &str) -> Option<Vec<u8>> {
    let url = format!("{api_base}/api/frame.jpeg?src={camera}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(5))
        .call()
        .ok()?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn sleep_responsive(total: Duration, shutdown: &AtomicBool) {
    let start = std::time::Instant::now();
    while start.elapsed() < total && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}
