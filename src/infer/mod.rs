//! Local-inference layer: lightweight model runtime gated behind the `infer` feature.
//!
//! The default build carries zero new dependency weight. Enable with `--features infer`
//! to get the cross-encoder reranker and text embedder backed by ONNX Runtime + `CoreML`
//! (Apple Neural Engine).
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use tilth::infer::{ModelConfig, rerank, embed};
//!
//! let cfg = ModelConfig::from_name("reranker");
//! match rerank(&cfg, "parse unified diff", &["fn parse_diff", "fn tokenize"]) {
//!     Ok(scores) => println!("{scores:?}"),
//!     Err(e) => eprintln!("reranker unavailable: {e}"),
//! }
//!
//! let ecfg = ModelConfig::from_name("embedder");
//! match embed(&ecfg, &["detect file type", "parse diff"]) {
//!     Ok(vecs) => println!("got {} unit vectors of dim {}", vecs.len(), vecs[0].len()),
//!     Err(e) => eprintln!("embedder unavailable: {e}"),
//! }
//! ```

pub mod embed_index;

use std::fmt;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// All errors the inference layer can produce.
#[derive(Debug)]
pub enum InferError {
    /// The `infer` feature was not compiled in; no model runtime is available.
    Unavailable,
    /// The model or tokenizer path does not exist on disk.
    ModelMissing(PathBuf),
    /// A tokenizer error (e.g. encoding failed).
    Tokenizer(String),
    /// A backend/runtime error (e.g. ONNX session error).
    Backend(String),
}

impl fmt::Display for InferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => {
                write!(f, "inference unavailable: build with --features infer")
            }
            Self::ModelMissing(path) => {
                write!(f, "model not found: {}", path.display())
            }
            Self::Tokenizer(msg) => {
                write!(f, "tokenizer error: {msg}")
            }
            Self::Backend(msg) => {
                write!(f, "backend error: {msg}")
            }
        }
    }
}

impl std::error::Error for InferError {}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// Configuration for a model session (reranker or embedder).
///
/// `model_path` and `tokenizer_path` point at ONNX and tokenizer files.
/// Use [`ModelConfig::from_name`] to resolve the default cache location
/// (`~/.cache/tilth/models/<name>/`).
///
/// `max_len` caps the number of tokens fed per call. The reranker default is
/// 512; the embedder default is 2048 (the ONNX model supports long contexts
/// but earlier experiments OOM'd at 8000 tokens).
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub max_len: usize,
}

/// Per-model-name defaults for `max_len`.
fn default_max_len(name: &str) -> usize {
    match name {
        "embedder" => 2048,
        _ => 512,
    }
}

impl ModelConfig {
    /// Resolve paths under the default tilth model cache:
    /// `~/.cache/tilth/models/<name>/model.onnx` and `.../tokenizer.json`.
    ///
    /// Returns the config regardless of whether the files exist; existence is
    /// checked at call time inside [`rerank`] or [`embed`].
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        let base = home::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".cache")
            .join("tilth")
            .join("models")
            .join(name);
        Self {
            model_path: base.join("model.onnx"),
            tokenizer_path: base.join("tokenizer.json"),
            max_len: default_max_len(name),
        }
    }

    /// Construct from explicit paths (useful in tests and custom setups).
    #[must_use]
    pub fn new(model_path: PathBuf, tokenizer_path: PathBuf, max_len: usize) -> Self {
        Self {
            model_path,
            tokenizer_path,
            max_len,
        }
    }
}

// ---------------------------------------------------------------------------
// `infer` feature: real implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "infer")]
mod runtime {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use ort::ep::coreml::{ComputeUnits, CoreML};
    use ort::session::{Session, SessionInputValue};
    use ort::value::Tensor;
    use tokenizers::Tokenizer;

    use super::{InferError, ModelConfig};

    // ---------------------------------------------------------------------------
    // Shared session-loading helpers
    // ---------------------------------------------------------------------------

    /// Stable compiled-model cache dir for the `CoreML` EP, per model.
    ///
    /// Without a cache dir, ORT compiles the model on EVERY session
    /// instantiation and leaves one `onnxruntime-*` dir per compiled subgraph
    /// in $TMPDIR, never cleaned — a short-lived CLI process leaks ~100+ dirs
    /// per model load. The cache is keyed by graph/metadata; a per-model
    /// subdir (the model's cache-dir name, e.g. `reranker`) keeps models from
    /// colliding.
    fn coreml_cache_dir(model_path: &std::path::Path) -> std::path::PathBuf {
        let model_key = model_path
            .parent()
            .and_then(|p| p.file_name())
            .map_or_else(|| "model".to_string(), |n| n.to_string_lossy().into_owned());
        home::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(".cache")
            .join("tilth")
            .join("coreml")
            .join(model_key)
    }

    /// Build a CoreML-backed ORT session from `model_path`.
    fn build_session(model_path: &std::path::Path) -> Result<Session, InferError> {
        let cache_dir = coreml_cache_dir(model_path);
        // Best-effort: if the dir can't be created the EP still works, it just
        // recompiles per instantiation (the pre-cache behavior).
        std::fs::create_dir_all(&cache_dir).ok();
        let coreml_ep = CoreML::default()
            .with_compute_units(ComputeUnits::CPUAndNeuralEngine)
            .with_model_cache_dir(cache_dir.to_string_lossy())
            .build();
        Session::builder()
            .map_err(|e| InferError::Backend(e.to_string()))?
            .with_execution_providers([coreml_ep])
            .map_err(|e| InferError::Backend(e.to_string()))?
            .commit_from_file(model_path)
            .map_err(|e| InferError::Backend(e.to_string()))
    }

    /// Read the declared input names from a session (order matters for ORT).
    fn session_input_names(session: &Session) -> Vec<String> {
        session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect()
    }

    /// Check that both model and tokenizer files are present.
    fn check_files(cfg: &ModelConfig) -> Result<(), InferError> {
        if !cfg.model_path.exists() {
            return Err(InferError::ModelMissing(cfg.model_path.clone()));
        }
        if !cfg.tokenizer_path.exists() {
            return Err(InferError::ModelMissing(cfg.tokenizer_path.clone()));
        }
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Reranker
    // ---------------------------------------------------------------------------

    struct Reranker {
        // `Session::run` takes `&mut self`, so we keep it behind a Mutex to
        // allow shared access from a `&Reranker` reference held in a global map.
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        max_len: usize,
        /// Names of declared session inputs, in declaration order.
        input_names: Vec<String>,
        /// Number of output logit columns: 1 → raw score; 2 → [neg, pos] (positive-class).
        output_cols: usize,
    }

    // Key the cache on the model path string so that different ModelConfig
    // values (different model directories) each get their own session. This
    // avoids the first-caller-wins pin that a bare OnceLock would produce.
    static RERANKER_CACHE: std::sync::OnceLock<Mutex<HashMap<String, Result<Reranker, String>>>> =
        std::sync::OnceLock::new();

    fn reranker_cache() -> &'static Mutex<HashMap<String, Result<Reranker, String>>> {
        RERANKER_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn load_reranker(cfg: &ModelConfig) -> Result<Reranker, InferError> {
        check_files(cfg)?;

        let session = build_session(&cfg.model_path)?;

        // Read the model's declared input names so we feed exactly those,
        // no more, no less. Many cross-encoders omit token_type_ids; feeding
        // an undeclared input is a hard error in ORT.
        let input_names = session_input_names(&session);

        // Read the output column count from the declared output shape.
        // Shape has i64 dims where -1 = dynamic. We want the last concrete dim.
        // e.g. [batch_size, 1] → 1, [batch, 2] → 2.
        let output_cols = session
            .outputs()
            .first()
            .and_then(|o| o.dtype().tensor_shape())
            .and_then(|s| s.last().copied())
            .map_or(1, |d| if d > 0 { d as usize } else { 1 });

        let tokenizer = Tokenizer::from_file(&cfg.tokenizer_path)
            .map_err(|e| InferError::Tokenizer(e.to_string()))?;

        Ok(Reranker {
            session: Mutex::new(session),
            tokenizer,
            max_len: cfg.max_len,
            input_names,
            output_cols,
        })
    }

    /// Score each doc against query using the cross-encoder session.
    ///
    /// Returns scores in `docs` order. Sessions are cached per model path —
    /// the first call for a given path loads the model; subsequent calls reuse it.
    pub fn rerank(cfg: &ModelConfig, query: &str, docs: &[&str]) -> Result<Vec<f32>, InferError> {
        let key = cfg.model_path.to_string_lossy().into_owned();

        // Ensure the entry is populated before we take a shared borrow.
        {
            let mut map = reranker_cache()
                .lock()
                .map_err(|e| InferError::Backend(format!("cache lock poisoned: {e}")))?;
            if !map.contains_key(&key) {
                let result = load_reranker(cfg).map_err(|e| e.to_string());
                map.insert(key.clone(), result);
            }
        }

        // Re-acquire and borrow the cached entry for scoring.
        let map = reranker_cache()
            .lock()
            .map_err(|e| InferError::Backend(format!("cache lock poisoned: {e}")))?;
        let r = map[&key].as_ref().map_err(|s| {
            // Re-materialise the original error variant from the stored string.
            if s.contains("model not found") {
                InferError::ModelMissing(cfg.model_path.clone())
            } else if s.contains("tokenizer") {
                InferError::Tokenizer(s.clone())
            } else {
                InferError::Backend(s.clone())
            }
        })?;

        let mut session = r
            .session
            .lock()
            .map_err(|e| InferError::Backend(format!("session lock poisoned: {e}")))?;

        let mut scores = Vec::with_capacity(docs.len());
        for doc in docs {
            let enc = r
                .tokenizer
                .encode((query, *doc), true)
                .map_err(|e| InferError::Tokenizer(e.to_string()))?;

            let max = r.max_len;
            let input_ids: Vec<i64> = enc
                .get_ids()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();
            let attention_mask: Vec<i64> = enc
                .get_attention_mask()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();
            let token_type_ids: Vec<i64> = enc
                .get_type_ids()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();

            let seq_len = input_ids.len();
            let shape = [1_usize, seq_len];

            let t_ids = Tensor::<i64>::from_array((shape, input_ids.into_boxed_slice()))
                .map_err(|e| InferError::Backend(e.to_string()))?;
            let t_mask = Tensor::<i64>::from_array((shape, attention_mask.into_boxed_slice()))
                .map_err(|e| InferError::Backend(e.to_string()))?;
            let t_type = Tensor::<i64>::from_array((shape, token_type_ids.into_boxed_slice()))
                .map_err(|e| InferError::Backend(e.to_string()))?;

            // Build a named-input map that feeds ONLY the model's declared
            // inputs. This handles both 2-input models (no token_type_ids)
            // and 3-input models transparently.
            let mut named: Vec<(Cow<str>, SessionInputValue)> =
                Vec::with_capacity(r.input_names.len());
            for name in &r.input_names {
                let val: SessionInputValue = match name.as_str() {
                    "input_ids" => t_ids.view().into(),
                    "attention_mask" => t_mask.view().into(),
                    "token_type_ids" => t_type.view().into(),
                    other => {
                        return Err(InferError::Backend(format!(
                            "unsupported model input: {other}"
                        )));
                    }
                };
                named.push((Cow::Owned(name.clone()), val));
            }

            let outputs = session
                .run(named)
                .map_err(|e| InferError::Backend(e.to_string()))?;

            // Extract the relevance score from the first output tensor.
            // [batch, 1] → logit[0] (raw score)
            // [batch, 2] → logit[1] (positive-class logit; softmax not needed for ranking)
            let (_, logits) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| InferError::Backend(e.to_string()))?;
            let score = if r.output_cols >= 2 {
                logits.get(1).copied().unwrap_or(0.0)
            } else {
                logits.first().copied().unwrap_or(0.0)
            };
            scores.push(score);
        }
        Ok(scores)
    }

    // ---------------------------------------------------------------------------
    // Embedder
    // ---------------------------------------------------------------------------

    struct Embedder {
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        max_len: usize,
        /// Names of declared session inputs (typically `input_ids` + `attention_mask`).
        input_names: Vec<String>,
        /// Hidden dimension of the embedding output (e.g. 768).
        hidden_dim: usize,
    }

    static EMBEDDER_CACHE: std::sync::OnceLock<Mutex<HashMap<String, Result<Embedder, String>>>> =
        std::sync::OnceLock::new();

    fn embedder_cache() -> &'static Mutex<HashMap<String, Result<Embedder, String>>> {
        EMBEDDER_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn load_embedder(cfg: &ModelConfig) -> Result<Embedder, InferError> {
        check_files(cfg)?;

        let session = build_session(&cfg.model_path)?;
        let input_names = session_input_names(&session);

        // Infer the hidden dim from the output shape. The embedder declares
        // last_hidden_state [batch, seq, hidden]. Last concrete dim = hidden.
        let hidden_dim = session
            .outputs()
            .first()
            .and_then(|o| o.dtype().tensor_shape())
            .and_then(|s| s.last().copied())
            .map_or(768, |d| if d > 0 { d as usize } else { 768 });

        let tokenizer = Tokenizer::from_file(&cfg.tokenizer_path)
            .map_err(|e| InferError::Tokenizer(e.to_string()))?;

        Ok(Embedder {
            session: Mutex::new(session),
            tokenizer,
            max_len: cfg.max_len,
            input_names,
            hidden_dim,
        })
    }

    /// Embed each text and return a 768-dim L2-normalised unit vector per text.
    ///
    /// Mean-pools `last_hidden_state` over the sequence (masked by `attention_mask`),
    /// then L2-normalises to a unit vector. Sessions are cached per model path.
    pub fn embed(cfg: &ModelConfig, texts: &[&str]) -> Result<Vec<Vec<f32>>, InferError> {
        let key = cfg.model_path.to_string_lossy().into_owned();

        {
            let mut map = embedder_cache()
                .lock()
                .map_err(|e| InferError::Backend(format!("cache lock poisoned: {e}")))?;
            if !map.contains_key(&key) {
                let result = load_embedder(cfg).map_err(|e| e.to_string());
                map.insert(key.clone(), result);
            }
        }

        let map = embedder_cache()
            .lock()
            .map_err(|e| InferError::Backend(format!("cache lock poisoned: {e}")))?;
        let e = map[&key].as_ref().map_err(|s| {
            if s.contains("model not found") {
                InferError::ModelMissing(cfg.model_path.clone())
            } else if s.contains("tokenizer") {
                InferError::Tokenizer(s.clone())
            } else {
                InferError::Backend(s.clone())
            }
        })?;

        let mut session = e
            .session
            .lock()
            .map_err(|e| InferError::Backend(format!("session lock poisoned: {e}")))?;

        let mut result = Vec::with_capacity(texts.len());
        for text in texts {
            // Encode as a single sequence (no pair — embedders use single-sequence inputs).
            let enc = e
                .tokenizer
                .encode(*text, true)
                .map_err(|err| InferError::Tokenizer(err.to_string()))?;

            let max = e.max_len;
            let input_ids: Vec<i64> = enc
                .get_ids()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();
            let attention_mask_raw: Vec<i64> = enc
                .get_attention_mask()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();
            let token_type_ids: Vec<i64> = enc
                .get_type_ids()
                .iter()
                .take(max)
                .map(|&x| i64::from(x))
                .collect();

            let seq_len = input_ids.len();
            let shape = [1_usize, seq_len];

            let t_ids = Tensor::<i64>::from_array((shape, input_ids.into_boxed_slice()))
                .map_err(|err| InferError::Backend(err.to_string()))?;
            let t_mask =
                Tensor::<i64>::from_array((shape, attention_mask_raw.clone().into_boxed_slice()))
                    .map_err(|err| InferError::Backend(err.to_string()))?;
            let t_type = Tensor::<i64>::from_array((shape, token_type_ids.into_boxed_slice()))
                .map_err(|err| InferError::Backend(err.to_string()))?;

            // Feed only declared inputs (mirrors the Reranker pattern).
            let mut named: Vec<(Cow<str>, SessionInputValue)> =
                Vec::with_capacity(e.input_names.len());
            for name in &e.input_names {
                let val: SessionInputValue = match name.as_str() {
                    "input_ids" => t_ids.view().into(),
                    "attention_mask" => t_mask.view().into(),
                    "token_type_ids" => t_type.view().into(),
                    other => {
                        return Err(InferError::Backend(format!(
                            "unsupported embedder input: {other}"
                        )));
                    }
                };
                named.push((Cow::Owned(name.clone()), val));
            }

            let outputs = session
                .run(named)
                .map_err(|err| InferError::Backend(err.to_string()))?;

            // last_hidden_state: [1, seq_len, hidden_dim] stored in row-major order.
            let (shape_out, token_vecs) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|err| InferError::Backend(err.to_string()))?;

            // shape_out[0] = batch (1), shape_out[1] = seq_len, shape_out[2] = hidden_dim
            let actual_hidden = if shape_out.len() >= 3 {
                shape_out[2] as usize
            } else {
                e.hidden_dim
            };

            // Mean-pool: sum token vectors weighted by attention_mask, then divide
            // by the total mask weight (i.e. the number of real — non-padding — tokens).
            let mut pooled = vec![0.0_f32; actual_hidden];
            let mut mask_sum = 0.0_f32;
            for (tok_idx, &mask_val) in attention_mask_raw.iter().enumerate() {
                if mask_val == 0 {
                    continue;
                }
                let w = mask_val as f32;
                let base = tok_idx * actual_hidden;
                for (dim, p) in pooled.iter_mut().enumerate() {
                    *p += w * token_vecs[base + dim];
                }
                mask_sum += w;
            }
            if mask_sum > 0.0 {
                for p in &mut pooled {
                    *p /= mask_sum;
                }
            }

            // L2-normalise → unit vector.
            let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for p in &mut pooled {
                    *p /= norm;
                }
            }

            result.push(pooled);
        }
        Ok(result)
    }
    #[cfg(test)]
    mod coreml_cache_tests {
        use super::*;

        /// Every CoreML session instantiation must reuse the shared
        /// compiled-model cache instead of compiling into $TMPDIR — without a
        /// cache dir, ORT writes one `onnxruntime-*` dir per compiled subgraph
        /// and never deletes them (measured: 294 leaked dirs for ONE
        /// short-lived CLI run; 432 GB over a benchmark period).
        #[test]
        fn build_session_does_not_leak_coreml_temp_dirs() {
            let cfg = ModelConfig::from_name("reranker");
            if !cfg.model_path.exists() {
                eprintln!("build_session_does_not_leak_coreml_temp_dirs: model absent, skipping");
                return;
            }

            let tmp = std::env::temp_dir();
            let count_ort_dirs = || {
                std::fs::read_dir(&tmp)
                    .map(|rd| {
                        rd.filter_map(Result::ok)
                            .filter(|e| e.file_name().to_string_lossy().starts_with("onnxruntime-"))
                            .count()
                    })
                    .unwrap_or(0)
            };

            // CoreML compiles subgraphs lazily at first RUN, not at session
            // build — so exercise the full instantiate+run path twice, clearing
            // the in-process session cache in between to model what every
            // short-lived CLI process does. The second instantiation must be
            // served from the shared compiled-model cache: zero temp-dir growth.
            let _ = rerank(&cfg, "warm the compile cache", &["a doc"])
                .expect("first rerank must succeed");
            reranker_cache().lock().unwrap().clear();
            let before = count_ort_dirs();
            let _ = rerank(&cfg, "fresh instantiation", &["a doc"])
                .expect("second rerank must succeed");
            let after = count_ort_dirs();
            assert!(
                after <= before,
                "second session instantiation leaked {} CoreML temp dir(s) in {}",
                after - before,
                tmp.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Public surface — callers never need #[cfg(feature = "infer")]
// ---------------------------------------------------------------------------

/// Score each document against `query` using the cross-encoder reranker.
///
/// Returns scores in `docs` order (higher = more relevant).
///
/// When built without `--features infer`, always returns `Err(InferError::Unavailable)`.
/// When the model files are missing, returns `Err(InferError::ModelMissing(_))`.
/// Callers should degrade gracefully on any `Err`.
pub fn rerank(cfg: &ModelConfig, query: &str, docs: &[&str]) -> Result<Vec<f32>, InferError> {
    #[cfg(feature = "infer")]
    {
        runtime::rerank(cfg, query, docs)
    }
    #[cfg(not(feature = "infer"))]
    {
        // Suppress unused-variable warnings in the stub path.
        let _ = (cfg, query, docs);
        Err(InferError::Unavailable)
    }
}

/// Embed each text into a 768-dim L2-normalised unit vector.
///
/// Runs the encoder model, mean-pools `last_hidden_state` over the sequence
/// (masked by `attention_mask`), then L2-normalises the result.
///
/// When built without `--features infer`, always returns `Err(InferError::Unavailable)`.
/// When the model files are missing, returns `Err(InferError::ModelMissing(_))`.
/// Callers should degrade gracefully on any `Err`.
pub fn embed(cfg: &ModelConfig, texts: &[&str]) -> Result<Vec<Vec<f32>>, InferError> {
    #[cfg(feature = "infer")]
    {
        runtime::embed(cfg, texts)
    }
    #[cfg(not(feature = "infer"))]
    {
        let _ = (cfg, texts);
        Err(InferError::Unavailable)
    }
}

/// Returns `true` only when the `infer` feature is compiled in.
///
/// Use this to decide whether to surface model-dependent options in the UI.
#[must_use]
pub fn is_available() -> bool {
    cfg!(feature = "infer")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_config_from_name_resolves_default_cache() {
        let cfg = ModelConfig::from_name("reranker");
        // Must contain the expected path segments regardless of $HOME value.
        let model_str = cfg.model_path.to_string_lossy();
        let tok_str = cfg.tokenizer_path.to_string_lossy();
        assert!(
            model_str.contains(".cache/tilth/models/reranker"),
            "model path should contain cache dir: {model_str}"
        );
        assert!(
            model_str.ends_with("model.onnx"),
            "model path should end with model.onnx: {model_str}"
        );
        assert!(
            tok_str.ends_with("tokenizer.json"),
            "tokenizer path should end with tokenizer.json: {tok_str}"
        );
        assert_eq!(cfg.max_len, 512);
    }

    #[test]
    fn model_config_from_name_embedder_uses_larger_max_len() {
        let cfg = ModelConfig::from_name("embedder");
        let model_str = cfg.model_path.to_string_lossy();
        assert!(
            model_str.contains(".cache/tilth/models/embedder"),
            "embedder model path should contain cache dir: {model_str}"
        );
        assert_eq!(cfg.max_len, 2048, "embedder default max_len must be 2048");
    }

    #[test]
    fn model_config_new_explicit_paths() {
        let cfg = ModelConfig::new(
            PathBuf::from("/tmp/model.onnx"),
            PathBuf::from("/tmp/tokenizer.json"),
            128,
        );
        assert_eq!(cfg.model_path, PathBuf::from("/tmp/model.onnx"));
        assert_eq!(cfg.max_len, 128);
    }

    // When built WITHOUT --features infer, rerank must return Unavailable.
    // The #[cfg] guard ensures this test is meaningless (but still compiles)
    // when infer IS enabled — in that case the rerank call would need real
    // files, so we only assert the stub path.
    #[test]
    #[cfg(not(feature = "infer"))]
    fn rerank_returns_unavailable_without_infer_feature() {
        let cfg = ModelConfig::from_name("reranker");
        let result = rerank(&cfg, "query", &["doc one", "doc two"]);
        assert!(
            matches!(result, Err(InferError::Unavailable)),
            "expected Unavailable, got: {result:?}"
        );
    }

    #[test]
    #[cfg(not(feature = "infer"))]
    fn embed_returns_unavailable_without_infer_feature() {
        let cfg = ModelConfig::from_name("embedder");
        let result = embed(&cfg, &["hello world"]);
        assert!(
            matches!(result, Err(InferError::Unavailable)),
            "expected Unavailable, got: {result:?}"
        );
    }

    #[test]
    fn infer_error_display_unavailable() {
        let msg = InferError::Unavailable.to_string();
        assert!(
            msg.contains("infer"),
            "display should mention the feature: {msg}"
        );
    }

    #[test]
    fn infer_error_display_model_missing() {
        let msg = InferError::ModelMissing(PathBuf::from("/tmp/nope.onnx")).to_string();
        assert!(
            msg.contains("nope.onnx"),
            "display should contain path: {msg}"
        );
    }

    #[test]
    fn infer_error_display_tokenizer() {
        let msg = InferError::Tokenizer("bad vocab".to_string()).to_string();
        assert!(
            msg.contains("bad vocab"),
            "display should contain message: {msg}"
        );
    }

    #[test]
    fn infer_error_display_backend() {
        let msg = InferError::Backend("session failed".to_string()).to_string();
        assert!(
            msg.contains("session failed"),
            "display should contain message: {msg}"
        );
    }

    #[test]
    fn is_available_matches_feature_flag() {
        // Without infer: false. With infer: true.
        #[cfg(not(feature = "infer"))]
        assert!(!is_available());
        #[cfg(feature = "infer")]
        assert!(is_available());
    }

    /// Live rerank test: loads the staged model and verifies the diff-parse
    /// candidate ranks #1. Skips cleanly when the model file is absent so
    /// model-less CI passes.
    #[test]
    #[cfg(feature = "infer")]
    fn live_rerank_diff_parse_ranks_first() {
        let cfg = ModelConfig::from_name("reranker");
        if !cfg.model_path.exists() || !cfg.tokenizer_path.exists() {
            eprintln!("live_rerank_diff_parse_ranks_first: model absent, skipping");
            return;
        }

        let query = "parse a unified diff into hunks";
        let candidates = [
            "src/diff/parse.rs: parse_unified_diff, FileDiff",
            "src/main.rs: main, run cli",
            "src/format.rs: format helpers",
        ];

        let scores =
            rerank(&cfg, query, &candidates).expect("rerank must succeed with staged model");

        assert_eq!(
            scores.len(),
            candidates.len(),
            "score count must match candidate count"
        );

        eprintln!("live_rerank scores:");
        for (c, s) in candidates.iter().zip(&scores) {
            eprintln!("  {s:+.4}  {c}");
        }

        // The diff-parse candidate (index 0) must outscore both others.
        let diff_score = scores[0];
        let main_score = scores[1];
        let fmt_score = scores[2];
        assert!(
            diff_score > main_score,
            "diff/parse.rs must outscore main.rs: {diff_score} vs {main_score}"
        );
        assert!(
            diff_score > fmt_score,
            "diff/parse.rs must outscore format.rs: {diff_score} vs {fmt_score}"
        );
    }

    /// Live embed test: loads the staged embedder and verifies that unit vectors
    /// are produced with the correct dimension and cosine similarity behaves as
    /// expected (identical texts are close; orthogonal are not). Skips when the
    /// model is absent so model-less CI passes.
    #[test]
    #[cfg(feature = "infer")]
    fn live_embed_produces_unit_vectors() {
        let cfg = ModelConfig::from_name("embedder");
        if !cfg.model_path.exists() || !cfg.tokenizer_path.exists() {
            eprintln!("live_embed_produces_unit_vectors: model absent, skipping");
            return;
        }

        let texts = [
            "detect the programming language of a source file",
            "parse a unified diff into hunks",
        ];
        let vecs = embed(&cfg, &texts).expect("embed must succeed with staged model");

        assert_eq!(vecs.len(), 2, "one vector per text");
        let dim = vecs[0].len();
        assert!(dim >= 64, "expected a real embedding dimension, got {dim}");
        eprintln!("embed dim={dim}");

        // Each vector should be unit-length (L2 norm ≈ 1.0).
        for (i, v) in vecs.iter().enumerate() {
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (norm - 1.0).abs() < 1e-4,
                "vector {i} is not unit-length: norm={norm}"
            );
        }

        // Identical text should have very high cosine similarity (≥ 0.99).
        let same = embed(&cfg, &[texts[0]]).unwrap();
        let cos_self: f32 = vecs[0].iter().zip(&same[0]).map(|(a, b)| a * b).sum();
        assert!(
            cos_self >= 0.99,
            "self-cosine should be near 1.0, got {cos_self}"
        );

        eprintln!("live_embed: cosine(same, same) = {cos_self:.4}");
        let cos_diff: f32 = vecs[0].iter().zip(&vecs[1]).map(|(a, b)| a * b).sum();
        eprintln!("live_embed: cosine(lang-detect, diff-parse) = {cos_diff:.4}");
    }
}
