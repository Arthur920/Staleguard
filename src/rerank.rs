//! Layer 2 reranker (optional): a cross-encoder that rescores `(query, passage)`
//! pairs after the bi-encoder cosine retrieval, sharpening the top-k before it
//! reaches the Layer 3 judge.
//!
//! Unlike the embedding model this is *opt-in*: it loads only when
//! `STALEGUARD_RERANK_REPO` is set (e.g. a ms-marco MiniLM or bge-reranker ONNX),
//! so the default retrieval path stays a single model download. ONNX file and
//! repo are overridable via `STALEGUARD_RERANK_ONNX` / `STALEGUARD_RERANK_REPO`.
//!
//! A reranker is a single-logit cross-encoder (relevance score), so this reuses
//! the [`crate::judge`] ort plumbing but reads one output value instead of a
//! 3-class head.

use std::borrow::Cow;

use anyhow::{anyhow, Result};
use hf_hub::api::sync::Api;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

const DEFAULT_ONNX: &str = "onnx/model_quantized.onnx";
const MAX_TOKENS: usize = 256;

/// A loaded cross-encoder reranker. Constructed only when configured.
pub struct Reranker {
    session: Session,
    tokenizer: Tokenizer,
    needs_token_types: bool,
}

impl Reranker {
    /// Load the reranker iff `STALEGUARD_RERANK_REPO` is set; otherwise `None`.
    pub fn from_env() -> Result<Option<Reranker>> {
        let Ok(repo_name) = std::env::var("STALEGUARD_RERANK_REPO") else {
            return Ok(None);
        };
        let onnx_rel =
            std::env::var("STALEGUARD_RERANK_ONNX").unwrap_or_else(|_| DEFAULT_ONNX.to_string());

        let repo = Api::new()?.model(repo_name);
        let onnx = repo.get(&onnx_rel)?;
        let tok = repo.get("tokenizer.json")?;

        let session = Session::builder()?.commit_from_file(onnx)?;
        let mut tokenizer =
            Tokenizer::from_file(tok).map_err(|e| anyhow!("load reranker tokenizer: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("set truncation: {e}"))?;

        let needs_token_types = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        Ok(Some(Reranker {
            session,
            tokenizer,
            needs_token_types,
        }))
    }

    /// Relevance score for each passage against the query (higher = better).
    pub fn scores(&mut self, query: &str, passages: &[String]) -> Result<Vec<f32>> {
        passages.iter().map(|p| self.score_one(query, p)).collect()
    }

    fn score_one(&mut self, query: &str, passage: &str) -> Result<f32> {
        let enc = self
            .tokenizer
            .encode((query, passage), true)
            .map_err(|e| anyhow!("tokenize: {e}"))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
        let seq = ids.len() as i64;

        let mut inputs = ort::inputs![
            "input_ids" => Tensor::from_array((vec![1i64, seq], ids))?,
            "attention_mask" => Tensor::from_array((vec![1i64, seq], mask))?,
        ];
        if self.needs_token_types {
            let types: Vec<i64> = enc.get_type_ids().iter().map(|&x| x as i64).collect();
            inputs.push((
                Cow::from("token_type_ids"),
                SessionInputValue::from(Tensor::from_array((vec![1i64, seq], types))?),
            ));
        }

        let outputs = self.session.run(inputs)?;
        let (_, logits) = outputs[0].try_extract_tensor::<f32>()?;
        // Single-logit relevance head; some rerankers emit [neg, pos] — take the
        // last as the positive/relevance score.
        Ok(*logits.last().unwrap_or(&f32::MIN))
    }
}
