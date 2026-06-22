//! Optional, bundled speech-to-text for audio events. whisper.cpp is compiled
//! into the binary (no separate server, no extra process), so when transcription
//! is enabled an audio event triggers a short capture from the camera's restream
//! which the whisper engine turns into text written back onto the event for
//! review + search. Fully local — audio never leaves the machine.
//!
//! Runs on its own worker thread (like the GenAI captioner) and loads the model
//! once, lazily, so a multi-second transcription never stalls audio detection.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::db::Db;
use crate::go2rtc::Go2Rtc;
use crate::proc::NoConsole as _;

const SAMPLE_RATE: u32 = 16_000;
/// Seconds of audio to capture (forward from the event) to transcribe.
const CAPTURE_SECS: u32 = 6;

/// A request to transcribe the audio around one event.
pub struct TranscribeJob {
    pub event_id: i64,
    pub camera: String,
}

/// Capture mono 16 kHz f32 audio from the camera's RTSP restream.
///
/// Bounded by a wall-clock watchdog: `-t` only limits the *output* duration, so
/// if the restream stalls on input (camera dropped, go2rtc mid-restart) ffmpeg
/// could block forever. We read stdout on a side thread and kill the child if
/// it overruns, so a stuck capture can never hang the worker / block shutdown.
fn capture(ffmpeg: &Path, rtsp_url: &str, secs: u32) -> Option<Vec<f32>> {
    let mut child = std::process::Command::new(ffmpeg)
        .args(["-loglevel", "error", "-rtsp_transport", "tcp", "-i"])
        .arg(rtsp_url)
        .args(["-t", &secs.to_string(), "-vn", "-ac", "1"])
        .args(["-ar", &SAMPLE_RATE.to_string(), "-f", "f32le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .no_console()
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Duration::from_secs(secs as u64 + 8);
    let bytes = match rx.recv_timeout(deadline) {
        Ok(b) => {
            let _ = child.wait();
            b
        }
        Err(_) => {
            tracing::warn!("transcription capture timed out; killing ffmpeg");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // Require at least ~1s of audio.
    if bytes.len() < 4 * SAMPLE_RATE as usize {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Transcribe 16 kHz mono f32 audio with a loaded model. `None` if the model
/// produced nothing usable (e.g. non-speech).
fn transcribe(ctx: &WhisperContext, audio: &[f32]) -> Option<String> {
    let mut state = ctx.create_state().ok()?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state.full(params, audio).ok()?;
    let mut text = String::new();
    for i in 0..state.full_n_segments() {
        if let Some(seg) = state.get_segment(i) {
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
            }
        }
    }
    clean_transcript(&text)
}

/// Normalize whisper output: collapse whitespace, treat a wholly-bracketed
/// result (whisper's non-speech markers like `[BLANK_AUDIO]`, `(silence)`) as
/// no speech, and cap the length for the UI.
fn clean_transcript(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = one_line.trim();
    let bracketed = t.starts_with(['[', '(']) && t.ends_with([']', ')']);
    let inner = t.trim_matches(|c| matches!(c, '[' | ']' | '(' | ')'));
    if t.is_empty()
        || bracketed
        || inner.eq_ignore_ascii_case("blank_audio")
        || inner.eq_ignore_ascii_case("silence")
        || inner.eq_ignore_ascii_case("inaudible")
    {
        return None;
    }
    // Cap length for the UI (char-safe: never slices inside a multibyte char).
    Some(if t.chars().count() > 500 {
        format!("{}…", t.chars().take(500).collect::<String>())
    } else {
        t.to_string()
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    ffmpeg_bin: Option<PathBuf>,
    snapshots_dir: PathBuf,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: crate::notify::AlarmThrottle,
    rx: Receiver<TranscribeJob>,
    shutdown: Arc<AtomicBool>,
) {
    let Ok(ffmpeg) = recorder::locate_ffmpeg(ffmpeg_bin.as_deref()) else {
        tracing::warn!("transcription disabled: ffmpeg not found");
        return;
    };
    // Route whisper.cpp / ggml's own logging through `log` -> tracing (off raw
    // stderr), so it respects RUST_LOG like everything else.
    static HOOKS: std::sync::Once = std::sync::Once::new();
    HOOKS.call_once(whisper_rs::install_logging_hooks);
    let mut ctx: Option<WhisperContext> = None;
    let mut loaded_model = String::new();
    // Remember a model path that failed to load so we don't re-attempt the
    // (potentially costly) load on every single event while it's misconfigured.
    let mut failed_model: Option<String> = None;

    while !shutdown.load(Ordering::Relaxed) {
        let job = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(j) => j,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let s = db.settings();
        if !s.transcription_enabled || s.transcription_model.trim().is_empty() {
            continue;
        }
        if failed_model.as_deref() == Some(s.transcription_model.as_str()) {
            continue; // known-bad model; wait for the setting to change.
        }
        // Lazily (re)load the model; reused across jobs since loading is costly.
        if ctx.is_none() || loaded_model != s.transcription_model {
            match WhisperContext::new_with_params(
                s.transcription_model.trim(),
                WhisperContextParameters::default(),
            ) {
                Ok(c) => {
                    tracing::info!("whisper STT model loaded: {}", s.transcription_model.trim());
                    ctx = Some(c);
                    loaded_model = s.transcription_model.clone();
                    failed_model = None;
                }
                Err(e) => {
                    tracing::warn!("whisper model load failed ({}): {e}", s.transcription_model);
                    failed_model = Some(s.transcription_model.clone());
                    continue;
                }
            }
        }
        let ctx = ctx.as_ref().expect("loaded above");
        let Some(audio) = capture(&ffmpeg, &go2rtc.rtsp_url(&job.camera), CAPTURE_SECS) else {
            continue;
        };
        match transcribe(ctx, &audio) {
            Some(text) => {
                if let Err(e) = db.set_event_transcript(job.event_id, &text) {
                    tracing::debug!("transcript save failed: {e}");
                } else {
                    // Metadata at info; the transcript body (speech content) stays
                    // at debug so spoken words aren't persisted to the logs.
                    tracing::info!(
                        event = job.event_id,
                        chars = text.chars().count(),
                        "transcript saved"
                    );
                    tracing::debug!(event = job.event_id, "transcript: {text}");
                    fire_transcript_alarms(
                        &db,
                        &snapshots_dir,
                        &mqtt_tx,
                        &throttle,
                        job.event_id,
                        &text,
                    );
                }
            }
            None => tracing::debug!(event = job.event_id, "no speech transcribed"),
        }
    }
}

/// Fire alarm rules whose `transcript_like` phrase the transcript matched. Only
/// transcript rules fire here — others already evaluated when the event was
/// created (with no transcript), so this never double-fires non-spoken rules.
fn fire_transcript_alarms(
    db: &Db,
    snapshots_dir: &Path,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: &crate::notify::AlarmThrottle,
    event_id: i64,
    transcript: &str,
) {
    let Ok(Some(ev)) = db.get_event(event_id) else {
        return;
    };
    let s = db.settings();
    let now = chrono::Local::now().timestamp();
    let snap_url = ev
        .snapshot
        .as_deref()
        .map(|f| format!("/api/snapshots/{f}"))
        .unwrap_or_default();
    let snap_abs = ev.snapshot.as_deref().map(|f| snapshots_dir.join(f));
    let alarm_ev = crate::notify::AlarmEvent {
        event_id: ev.id,
        camera: &ev.camera,
        label: &ev.label,
        score: ev.score,
        ts: ev.ts,
        snapshot_url: &snap_url,
        snapshot_path: snap_abs.as_deref(),
        face: ev.face.as_deref(),
        plate: ev.plate.as_deref(),
        gesture: ev.gesture.as_deref(),
        transcript: Some(transcript),
        speed: None,
        base_url: &s.public_base_url,
        webhook_template: &s.webhook_template,
        smtp: crate::notify::smtp_cfg(&s),
        duress: false,
    };
    for rule in db.list_alarms().unwrap_or_default().iter().filter(|r| {
        // Only transcript rules fire here; an empty phrase is not a transcript
        // rule (it would otherwise match every transcript).
        r.transcript_like
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
            && r.matches(
                ev.camera_id,
                &ev.label,
                ev.score,
                ev.face.as_deref(),
                ev.plate.as_deref(),
                ev.gesture.as_deref(),
                Some(transcript),
            )
            && r.zone_ok(ev.zone.as_deref())
            && crate::notify::armed_in_mode(&r.modes, &s.arm_mode)
            && crate::notify::ready(r, throttle, now)
    }) {
        crate::notify::fire(rule, &alarm_ev, mqtt_tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_transcript_normalizes_and_filters() {
        assert_eq!(
            clean_transcript("  Help   me\n please  ").as_deref(),
            Some("Help me please")
        );
        // whisper non-speech markers → treated as no speech.
        assert!(clean_transcript("[BLANK_AUDIO]").is_none());
        assert!(clean_transcript(" (silence) ").is_none());
        assert!(clean_transcript("[ Inaudible ]").is_none());
        assert!(clean_transcript("   ").is_none());
        // real speech survives.
        assert_eq!(
            clean_transcript("Get out of my yard").as_deref(),
            Some("Get out of my yard")
        );
    }

    /// Live model check — runs only when the model + the bundled jfk.wav sample
    /// are present (locally), skipped in CI where they aren't committed.
    #[test]
    fn transcribes_known_sample_when_model_present() {
        // Models/samples live at the workspace root (gitignored); cargo test's
        // CWD is the crate dir, so resolve relative to the manifest.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let model = root.join("ggml-tiny.en.bin");
        let wav = root.join("jfk.wav");
        if !model.exists() || !wav.exists() {
            eprintln!("skipping: model / jfk.wav not present");
            return;
        }
        let model = model.to_str().unwrap();
        let wav = wav.to_str().unwrap();
        // jfk.wav is 16 kHz mono 16-bit PCM; decode to f32 (skip the 44-byte header).
        let raw = std::fs::read(wav).unwrap();
        let pcm = &raw[44..];
        let audio: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        let ctx =
            WhisperContext::new_with_params(model, WhisperContextParameters::default()).unwrap();
        let text = transcribe(&ctx, &audio).expect("should transcribe speech");
        let lower = text.to_lowercase();
        assert!(
            lower.contains("country") || lower.contains("fellow americans"),
            "unexpected transcript: {text}"
        );
    }
}
