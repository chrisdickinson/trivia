use std::path::PathBuf;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

pub struct Embedder {
    model: TextEmbedding,
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("trivia")
        .join("fastembed")
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(cache_dir())
                .with_show_download_progress(true),
        )?;
        Ok(Self { model })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let embeddings = self.model.embed(vec![text], None)?;
        Ok(embeddings.into_iter().next().expect("single input should produce single output"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_produces_384_dims() -> Result<()> {
        let embedder = Embedder::new()?;
        let emb = embedder.embed("hello world")?;
        assert_eq!(emb.len(), 384);
        Ok(())
    }
}
