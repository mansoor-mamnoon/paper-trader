//! FinBERT: a small BERT model trained on financial text that reads one headline and says
//! whether it is positive, negative or neutral for the company. It runs in Rust on the CPU
//! in milliseconds, so the desk can score every headline of every screened stock, not only
//! the few the language model has time to read.
//!
//! The weights are ProsusAI/finbert (CC BY-NC-SA 4.0), pinned to a commit and a SHA-256.

use crate::llm::{cache_dir, fetch_file};
use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use candle_transformers::models::bert::{BertModel, Config};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const REVISION: &str = "0bc2ac10dda6929bcb1f77ccc43c7ec5c424f031";
const FILES: [(&str, &str, u64); 2] = [
    (
        "model.safetensors",
        "e5897858ff819aad7629b96ce521ae5477952d03634ef5dd30ed2d76357a9f00",
        437_965_908,
    ),
    (
        "vocab.txt",
        "07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3",
        231_508,
    ),
];
const CONFIG: &str = r#"{"vocab_size":30522,"hidden_size":768,"num_hidden_layers":12,
    "num_attention_heads":12,"intermediate_size":3072,"hidden_act":"gelu",
    "hidden_dropout_prob":0.1,"max_position_embeddings":512,"type_vocab_size":2,
    "initializer_range":0.02,"layer_norm_eps":1e-12,"pad_token_id":0,
    "position_embedding_type":"absolute","model_type":"bert"}"#;
/// Longest input in tokens. Headlines are far shorter.
const MAX_TOKENS: usize = 64;
const BATCH: usize = 16;
const CLS: u32 = 101;
const SEP: u32 = 102;
const UNK: u32 = 100;

/// What FinBERT made of one piece of text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tone {
    /// `positive`, `negative` or `neutral`.
    pub label: String,
    /// Probability of positive minus probability of negative, from -1 to 1.
    pub score: f64,
}

/// BERT's uncased tokenizer: lower-case, split on spaces and punctuation, then break each
/// word into the longest pieces the vocabulary knows.
#[derive(Debug)]
pub struct WordPiece {
    vocab: HashMap<String, u32>,
}

impl WordPiece {
    pub fn new(vocab_text: &str) -> Self {
        WordPiece {
            vocab: vocab_text
                .lines()
                .enumerate()
                .map(|(i, token)| (token.to_string(), i as u32))
                .collect(),
        }
    }

    fn words(text: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        for c in text.to_lowercase().chars() {
            if c.is_alphanumeric() {
                current.push(c);
                continue;
            }
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            if !c.is_whitespace() && !c.is_control() {
                words.push(c.to_string());
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        words
    }

    fn pieces(&self, word: &str, out: &mut Vec<u32>) {
        let chars: Vec<char> = word.chars().collect();
        let mut found = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut id = None;
            while end > start {
                let mut piece: String = chars[start..end].iter().collect();
                if start > 0 {
                    piece.insert_str(0, "##");
                }
                if let Some(known) = self.vocab.get(&piece) {
                    id = Some(*known);
                    break;
                }
                end -= 1;
            }
            let Some(id) = id else {
                out.push(UNK);
                return;
            };
            found.push(id);
            start = end;
        }
        out.extend(found);
    }

    /// Token ids for `text`, with the start and end markers, at most `max` long.
    pub fn encode(&self, text: &str, max: usize) -> Vec<u32> {
        let mut ids = vec![CLS];
        for word in Self::words(text) {
            self.pieces(&word, &mut ids);
        }
        ids.truncate(max.saturating_sub(1));
        ids.push(SEP);
        ids
    }
}

/// The loaded model.
pub struct FinBert {
    model: BertModel,
    pooler: Linear,
    classifier: Linear,
    tokenizer: WordPiece,
    device: Device,
}

impl std::fmt::Debug for FinBert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FinBert")
    }
}

/// Something that reads the tone of headlines.
pub trait Reader {
    fn read(&self, texts: &[String]) -> Result<Vec<Tone>>;
}

impl FinBert {
    /// Load the model, downloading its two files on first use.
    pub fn load() -> Result<Self> {
        let dir = cache_dir().join("finbert");
        std::fs::create_dir_all(&dir)?;
        for (file, sha256, bytes) in FILES {
            let path = dir.join(file);
            if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes) {
                continue;
            }
            let url = format!("https://huggingface.co/ProsusAI/finbert/resolve/{REVISION}/{file}");
            fetch_file(&format!("finbert {file}"), &url, &path, sha256, bytes)?;
        }
        Self::from_dir(&dir)
    }

    fn from_dir(dir: &Path) -> Result<Self> {
        let device = Device::Cpu;
        let config: Config = serde_json::from_str(CONFIG)?;
        let tensors = candle_core::safetensors::load(dir.join("model.safetensors"), &device)
            .context("could not read the FinBERT weights")?;
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);
        let model = BertModel::load(vb.clone(), &config).map_err(|e| anyhow!("{e}"))?;
        let pooler = candle_nn::linear(768, 768, vb.pp("bert.pooler.dense"))?;
        let classifier = candle_nn::linear(768, 3, vb.pp("classifier"))?;
        let tokenizer = WordPiece::new(&std::fs::read_to_string(dir.join("vocab.txt"))?);
        Ok(FinBert {
            model,
            pooler,
            classifier,
            tokenizer,
            device,
        })
    }

    fn batch(&self, texts: &[String]) -> Result<Vec<Tone>> {
        let encoded: Vec<Vec<u32>> = texts
            .iter()
            .map(|t| self.tokenizer.encode(t, MAX_TOKENS))
            .collect();
        let width = encoded.iter().map(Vec::len).max().unwrap_or(2);
        let mut ids = Vec::with_capacity(texts.len() * width);
        let mut mask = Vec::with_capacity(texts.len() * width);
        for row in &encoded {
            for i in 0..width {
                ids.push(row.get(i).copied().unwrap_or(0));
                mask.push(u32::from(i < row.len()));
            }
        }
        let shape = (texts.len(), width);
        let ids = Tensor::from_vec(ids, shape, &self.device)?;
        let mask = Tensor::from_vec(mask, shape, &self.device)?;
        let hidden = self.model.forward(&ids, &ids.zeros_like()?, Some(&mask))?;
        let pooled = self.pooler.forward(&hidden.i((.., 0))?)?.tanh()?;
        let logits = self.classifier.forward(&pooled)?;
        let probabilities = candle_nn::ops::softmax(&logits, 1)?.to_vec2::<f32>()?;
        Ok(probabilities
            .into_iter()
            .map(|p| {
                // The model's classes, in order: positive, negative, neutral.
                let (positive, negative, neutral) =
                    (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
                let label = if positive >= negative && positive >= neutral {
                    "positive"
                } else if negative >= neutral {
                    "negative"
                } else {
                    "neutral"
                };
                Tone {
                    label: label.into(),
                    score: positive - negative,
                }
            })
            .collect())
    }
}

impl Reader for FinBert {
    fn read(&self, texts: &[String]) -> Result<Vec<Tone>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH) {
            out.extend(self.batch(chunk)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_split_into_known_pieces() {
        let vocab =
            "[PAD]\n[UNK]\n[CLS]\n[SEP]\nacme\nbeat\n##s\n,\nq3\nprofit\nun\n##believ\n##able";
        let tokenizer = WordPiece {
            vocab: WordPiece::new(vocab).vocab,
        };
        let id = |t: &str| tokenizer.vocab[t];
        assert_eq!(
            tokenizer.encode("ACME beats, Q3 profit unbelievable zzz", 64),
            [
                CLS,
                id("acme"),
                id("beat"),
                id("##s"),
                id(","),
                id("q3"),
                id("profit"),
                id("un"),
                id("##believ"),
                id("##able"),
                UNK,
                SEP
            ]
        );
        let short = tokenizer.encode("acme acme acme acme", 4);
        assert_eq!(short.len(), 4);
        assert_eq!(short[3], SEP);
    }

    /// Needs the 438 MB weights, so it only runs when they are already downloaded.
    #[test]
    fn finbert_reads_obvious_headlines_when_the_weights_are_present() {
        let dir = cache_dir().join("finbert");
        if !dir.join("model.safetensors").exists() {
            return;
        }
        let model = FinBert::from_dir(&dir).unwrap();
        let tones = model
            .read(&[
                "Acme shares soar after profit beats estimates and guidance is raised".into(),
                "Acme plunges as regulators open fraud probe and revenue misses".into(),
                "Acme to present at the annual technology conference".into(),
            ])
            .unwrap();
        assert_eq!(tones[0].label, "positive", "{tones:?}");
        assert_eq!(tones[1].label, "negative", "{tones:?}");
        assert_eq!(tones[2].label, "neutral", "{tones:?}");
        assert!(tones[0].score > 0.5 && tones[1].score < -0.5);
    }
}
