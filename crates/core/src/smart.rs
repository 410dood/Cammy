//! Smart search (UniFi AI Key style): CLIP ViT-B/32 embeddings make event
//! snapshots searchable by natural language — "person in a red shirt",
//! "delivery truck at night". The pipeline embeds each event snapshot once;
//! a text query embeds in ~50 ms and ranks events by cosine similarity.
//!
//! Models are optional downloads (quantized int8, run on CPU — DirectML int8
//! support is spotty and CPU is plenty at event rate). Feature is inactive
//! until all three files exist (see README).

use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use detector::ort::session::Session;
use detector::ort::value::Tensor;
use image::DynamicImage;

pub const VISION_MODEL: &str = "clip_vision.onnx";
pub const TEXT_MODEL: &str = "clip_text.onnx";
pub const TOKENIZER: &str = "clip_tokenizer.json";

/// CLIP's image normalization constants.
const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];
const EDGE: u32 = 224;

pub fn models_present() -> bool {
    [VISION_MODEL, TEXT_MODEL, TOKENIZER]
        .iter()
        .all(|p| std::path::Path::new(p).exists())
}

/// Image side, owned by the detection pipeline thread.
pub struct ImageEmbedder {
    session: Session,
}

impl ImageEmbedder {
    pub fn try_new() -> Result<Self> {
        Ok(Self {
            // int8-quantized: run on CPU regardless of the GPU settings.
            session: detector::build_ort_session(VISION_MODEL, true)?,
        })
    }

    pub fn embed(&mut self, img: &DynamicImage) -> Result<Vec<f32>> {
        run_vision(&mut self.session, img)
    }
}

/// CLIP image preprocessing (cover-resize + center crop to 224, CLIP
/// normalization, NCHW) + forward pass → L2-normalized embedding. Shared by the
/// pipeline's `ImageEmbedder` and the lazy API-side `embed_image`.
fn run_vision(session: &mut Session, img: &DynamicImage) -> Result<Vec<f32>> {
    let rgb = img
        .resize_to_fill(EDGE, EDGE, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let plane = (EDGE * EDGE) as usize;
    let mut chw = vec![0.0f32; 3 * plane];
    for (x, y, px) in rgb.enumerate_pixels() {
        let idx = (y * EDGE + x) as usize;
        for c in 0..3 {
            chw[c * plane + idx] = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    let input = Tensor::from_array(([1usize, 3, EDGE as usize, EDGE as usize], chw))?;
    let outputs = session.run(detector::ort::inputs!["pixel_values" => input])?;
    let (_name, value) = outputs.iter().next().context("no image_embeds")?;
    let (_shape, data) = value.try_extract_tensor::<f32>()?;
    Ok(l2(data))
}

/// Vision side, shared by API handlers (lazy global). Lets a handler embed an
/// arbitrary uploaded image for appearance search without owning the detection
/// pipeline's `ImageEmbedder`. A query is rare; one shared session is plenty.
static VISION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

/// True when the CLIP vision model exists — enough to embed an uploaded image
/// and rank it against the stored crop corpus. (Image→image search needs neither
/// the text model nor the tokenizer, so it can work where `models_present()` —
/// which also gates text search — is false.)
pub fn vision_present() -> bool {
    std::path::Path::new(VISION_MODEL).exists()
}

/// Embed an arbitrary image with the shared CLIP vision session (lazy-loaded on
/// first use). Used by the upload-a-reference-photo appearance search.
pub fn embed_image(img: &DynamicImage) -> Result<Vec<f32>> {
    let cell = VISION.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().expect("clip vision mutex poisoned");
    if guard.is_none() {
        *guard = Some(detector::build_ort_session(VISION_MODEL, true)?);
    }
    let session = guard.as_mut().expect("initialized above");
    run_vision(session, img)
}

/// Text side, shared by API handlers (lazy global; a query is rare and fast).
static TEXT: OnceLock<Mutex<Option<(Session, tokenizers::Tokenizer)>>> = OnceLock::new();

pub fn embed_text(query: &str) -> Result<Vec<f32>> {
    let cell = TEXT.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().expect("clip text mutex poisoned");
    if guard.is_none() {
        let session = detector::build_ort_session(TEXT_MODEL, true)?;
        let tokenizer = tokenizers::Tokenizer::from_file(TOKENIZER)
            .map_err(|e| anyhow::anyhow!("loading {TOKENIZER}: {e}"))?;
        *guard = Some((session, tokenizer));
    }
    let (session, tokenizer) = guard.as_mut().expect("initialized above");

    let enc = tokenizer
        .encode(query, true)
        .map_err(|e| anyhow::anyhow!("tokenizing query: {e}"))?;
    let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
    anyhow::ensure!(!ids.is_empty(), "empty query");
    let len = ids.len().min(77); // CLIP context limit
    let input = Tensor::from_array(([1usize, len], ids[..len].to_vec()))?;
    let outputs = session.run(detector::ort::inputs!["input_ids" => input])?;
    let (_name, value) = outputs.iter().next().context("no text_embeds")?;
    let (_shape, data) = value.try_extract_tensor::<f32>()?;
    Ok(l2(data))
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // Dot product of L2-normalized vectors. Different lengths can't be compared
    // (e.g. embeddings from two different models) — `zip` would silently score on
    // the shorter prefix, so return "no match" instead of a garbage similarity.
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine at/above which a firing object crop is judged "the same thing" as a
/// thumbs-downed one and its alert is quieted (P2.8b feedback learning).
/// Deliberately conservative: the #61 Re-ID data put true same-object crop pairs
/// at 0.91–0.97 and unrelated pairs at 0.11–0.22, so 0.90 suppresses only a near-
/// identical re-detection and never a merely similar object. May need live tuning.
pub const FEEDBACK_SUPPRESS_COSINE: f32 = 0.90;

/// True if any vector in `corpus` is cosine-similar to `query` at/above
/// `threshold`. **Fails OPEN**: an empty query or empty corpus returns `false`
/// (nothing to suppress), so a missing/mismatched embedding can never swallow a
/// real alert. Length mismatches score 0 (via `cosine`) and so never suppress.
pub fn any_similar(query: &[f32], corpus: &[Vec<f32>], threshold: f32) -> bool {
    if query.is_empty() || corpus.is_empty() {
        return false;
    }
    corpus.iter().any(|c| cosine(query, c) >= threshold)
}

/// How well `query` matches free text (an event's transcript + caption), in
/// `[0, 1]`, case-insensitive: `1.0` if the whole query is a substring, else
/// the fraction of query words (≥2 chars) found in the text. `0` if no overlap.
/// Used to fold spoken words / captions into smart search alongside CLIP.
pub fn text_match_score(query: &str, text: &str) -> f32 {
    let text = text.to_lowercase();
    let query = query.trim().to_lowercase();
    // Tokenize on non-alphanumeric boundaries; keep tokens of ≥2 chars so a
    // stray single character can't match.
    let toks = |s: &str| -> std::collections::HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() >= 2)
            .map(str::to_string)
            .collect()
    };
    let q_words = toks(&query);
    if text.trim().is_empty() || q_words.is_empty() {
        return 0.0;
    }
    // A multi-word query that appears verbatim is a full phrase match.
    if q_words.len() >= 2 && text.contains(&query) {
        return 1.0;
    }
    // Otherwise match whole words only — "car" must not match "scared".
    let t_words = toks(&text);
    let hits = q_words.iter().filter(|w| t_words.contains(*w)).count();
    hits as f32 / q_words.len() as f32
}

fn l2(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}

#[cfg(test)]
mod tests {
    use super::{any_similar, text_match_score};

    #[test]
    fn any_similar_fails_open_and_matches() {
        // Empty corpus / empty query → false (fail open, never suppresses).
        assert!(!any_similar(&[1.0, 0.0], &[], 0.9));
        assert!(!any_similar(&[], &[vec![1.0, 0.0]], 0.9));
        // A near-identical (normalized) vector clears a high threshold.
        assert!(any_similar(&[1.0, 0.0], &[vec![0.99, 0.01]], 0.9));
        // An orthogonal vector never matches.
        assert!(!any_similar(&[1.0, 0.0], &[vec![0.0, 1.0]], 0.9));
        // Any one matching corpus vector is enough.
        assert!(any_similar(
            &[1.0, 0.0],
            &[vec![0.0, 1.0], vec![0.98, 0.0]],
            0.9
        ));
        // Length mismatch scores 0 → never suppresses (fail open).
        assert!(!any_similar(&[1.0, 0.0], &[vec![1.0, 0.0, 0.0]], 0.9));
    }

    #[test]
    fn text_match_scoring() {
        // Whole-query substring (case-insensitive) → full match.
        assert_eq!(
            text_match_score("help me", "Someone yelling HELP ME outside"),
            1.0
        );
        // Partial word overlap → fraction (one of three words present).
        let partial = text_match_score("help fire truck", "the word help appears");
        assert!((partial - 1.0 / 3.0).abs() < 1e-4, "got {partial}");
        // Whole-word matching: a query word must not partial-match a longer word.
        assert_eq!(text_match_score("car", "i was so scared"), 0.0);
        assert_eq!(text_match_score("scared", "i was so scared"), 1.0);
        // Punctuation around words is ignored.
        assert_eq!(text_match_score("help", "stop! help! now."), 1.0);
        // No overlap / empty text or query → 0.
        assert_eq!(text_match_score("delivery", "good morning everyone"), 0.0);
        assert_eq!(text_match_score("anything", ""), 0.0);
        assert_eq!(text_match_score("", "some text"), 0.0);
        // 1-char query is ignored (no stray single-letter matches).
        assert_eq!(text_match_score("a", "a cat sat"), 0.0);
    }
}
