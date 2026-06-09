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
        // Cover-resize + center crop to 224, CLIP normalization, NCHW.
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
        let outputs = self
            .session
            .run(detector::ort::inputs!["pixel_values" => input])?;
        let (_name, value) = outputs.iter().next().context("no image_embeds")?;
        let (_shape, data) = value.try_extract_tensor::<f32>()?;
        Ok(l2(data))
    }
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
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}
