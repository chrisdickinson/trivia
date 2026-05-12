use anyhow::Result;
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

const ONNX_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/model.onnx"));
const TOKENIZER_JSON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tokenizer.json"));
const CONFIG_JSON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/config.json"));
const SPECIAL_TOKENS_MAP_JSON: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/special_tokens_map.json"));
const TOKENIZER_CONFIG_JSON: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/tokenizer_config.json"));

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: TOKENIZER_JSON.to_vec(),
            config_file: CONFIG_JSON.to_vec(),
            special_tokens_map_file: SPECIAL_TOKENS_MAP_JSON.to_vec(),
            tokenizer_config_file: TOKENIZER_CONFIG_JSON.to_vec(),
        };
        let user_model = UserDefinedEmbeddingModel::new(ONNX_BYTES.to_vec(), tokenizer_files)
            .with_pooling(Pooling::Mean);
        let model =
            TextEmbedding::try_new_from_user_defined(user_model, InitOptionsUserDefined::new())?;
        Ok(Self { model })
    }

    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let embeddings = self.model.embed(vec![text], None)?;
        Ok(embeddings
            .into_iter()
            .next()
            .expect("single input should produce single output"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_produces_384_dims() -> Result<()> {
        let mut embedder = Embedder::new()?;
        let emb = embedder.embed("hello world")?;
        assert_eq!(emb.len(), 384);
        Ok(())
    }
}
