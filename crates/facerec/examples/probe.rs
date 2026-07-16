//! Standalone validation: detect faces in one or two images and print the
//! cross-similarity matrix of their embeddings. Same person across images
//! should score >0.4; different people <0.3.
//!
//!   cargo run -p facerec --example probe -- imgA.jpg [imgB.jpg]

use anyhow::Result;
use facerec::{cosine, FaceEngine};

fn main() -> Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!paths.is_empty(), "usage: probe <imgA> [imgB]");
    let mut eng = FaceEngine::new("det_10g.onnx", "w600k_r50.onnx", "auto")?;

    let mut all: Vec<(String, Vec<f32>)> = Vec::new();
    for path in &paths {
        let img = image::open(path)?;
        let faces = eng.detect(&img, 0.5)?;
        println!("{path}: {} face(s)", faces.len());
        for (i, f) in faces.iter().enumerate() {
            println!(
                "  face{i} score {:.2} box=[{:.0},{:.0},{:.0},{:.0}]",
                f.score, f.x1, f.y1, f.x2, f.y2
            );
            all.push((format!("{path}#{i}"), eng.embed(&img, f)?));
        }
    }

    println!("\ncross-similarity:");
    for (na, ea) in &all {
        for (nb, eb) in &all {
            if na < nb {
                println!("  {na} vs {nb}: {:.3}", cosine(ea, eb));
            }
        }
    }
    Ok(())
}
