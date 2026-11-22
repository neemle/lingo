use crate::{optional_models::OptionalModelJob, Result, Store};
use ort::{session::Session, value::Tensor};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Mutex, OnceLock},
};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

const DIMENSIONS: usize = 384;
const TOKEN_LIMIT: usize = 160;
const BATCH_SIZE: usize = 4;
const RESULT_LIMIT: usize = 25;
const CACHE_LIMIT: usize = 4096;
type Embedding = [f32; DIMENSIONS];
type Key = [u8; 32];

pub struct SemanticVocabularyJob {
    model: OptionalModelJob,
    query: String,
    passages: Vec<Passage>,
}

struct Passage {
    id: String,
    text: String,
    key: Key,
}

impl Store {
    pub fn prepare_semantic_vocabulary(&self, query: &str) -> Result<SemanticVocabularyJob> {
        self.prepare_semantic_vocabulary_in(query, None)
    }

    pub fn prepare_semantic_vocabulary_in(
        &self,
        query: &str,
        document_id: Option<&str>,
    ) -> Result<SemanticVocabularyJob> {
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 240 || query.chars().any(char::is_control) {
            return Err("Введіть пошуковий запит до 240 символів.".into());
        }
        let model = self.prepare_optional_model("multilingual-e5-small")?;
        let passages = self
            .dictionary_collection(document_id)?
            .into_iter()
            .map(|card| {
                let word: String = card.word.chars().take(100).collect();
                let translation: String = card.translation.chars().take(160).collect();
                let context: String = card.context.chars().take(400).collect();
                let details: String = card
                    .sense
                    .iter()
                    .map(|sense| sense.gloss.as_str())
                    .chain(card.accepted_translations.iter().map(String::as_str))
                    .flat_map(|value| value.chars().chain(['.', ' ']))
                    .take(200)
                    .collect();
                let text = format!("passage: {word} — {translation}. {details}{context}");
                let mut hash = Sha256::new();
                hash.update(model.revision().as_bytes());
                hash.update([0]);
                hash.update(text.as_bytes());
                let key = hash.finalize().into();
                Passage {
                    id: card.id,
                    text,
                    key,
                }
            })
            .collect();
        Ok(SemanticVocabularyJob {
            model,
            query: format!("query: {query}"),
            passages,
        })
    }
}

impl SemanticVocabularyJob {
    pub fn run(self) -> Result<Vec<String>> {
        if self.passages.is_empty() {
            return Ok(Vec::new());
        }
        self.model.run(|directory, should_yield| {
            let mut encoder = E5::open(directory)?;
            check_priority(should_yield)?;
            let query = encoder.encode(&[self.query.as_str()])?[0];
            let mut ranking = Vec::with_capacity(self.passages.len());
            for batch in self.passages.chunks(BATCH_SIZE) {
                check_priority(should_yield)?;
                let mut uncached = Vec::new();
                {
                    let cache = cache().lock().map_err(e5_error)?;
                    for passage in batch {
                        if let Some(vector) = cache.vectors.get(&passage.key) {
                            ranking.push((&passage.id, cosine(&query, vector)));
                        } else {
                            uncached.push(passage);
                        }
                    }
                }
                if !uncached.is_empty() {
                    let input: Vec<_> = uncached
                        .iter()
                        .map(|passage| passage.text.as_str())
                        .collect();
                    let vectors = encoder.encode(&input)?;
                    let mut cache = cache().lock().map_err(e5_error)?;
                    for (passage, vector) in uncached.into_iter().zip(vectors) {
                        ranking.push((&passage.id, cosine(&query, &vector)));
                        cache.insert(passage.key, vector);
                    }
                }
            }
            check_priority(should_yield)?;
            ranking.sort_unstable_by(|(a, x), (b, y)| y.total_cmp(x).then_with(|| a.cmp(b)));
            Ok(ranking
                .into_iter()
                .take(RESULT_LIMIT)
                .map(|(id, _)| id.clone())
                .collect())
        })
    }
}

#[derive(Default)]
struct VectorCache {
    vectors: HashMap<Key, Embedding>,
    order: VecDeque<Key>,
}

impl VectorCache {
    fn insert(&mut self, key: Key, vector: Embedding) {
        if self.vectors.contains_key(&key) {
            return;
        }
        if self.vectors.len() == CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.vectors.remove(&oldest);
            }
        }
        self.order.push_back(key);
        self.vectors.insert(key, vector);
    }
}

fn cache() -> &'static Mutex<VectorCache> {
    static CACHE: OnceLock<Mutex<VectorCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

struct E5 {
    tokenizer: Tokenizer,
    session: Session,
}

impl E5 {
    fn open(directory: &Path) -> Result<Self> {
        let env = crate::onnx_runtime::environment()?;
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(3));
        let session = Session::builder(env)
            .map_err(e5_error)?
            .with_intra_threads(threads)
            .map_err(e5_error)?
            .with_inter_threads(1)
            .map_err(e5_error)?
            .commit_from_file(directory.join("model.onnx"))
            .map_err(e5_error)?;
        let mut tokenizer =
            Tokenizer::from_file(directory.join("tokenizer.json")).map_err(e5_error)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: TOKEN_LIMIT,
                ..TruncationParams::default()
            }))
            .map_err(e5_error)?;
        tokenizer.with_padding(Some(PaddingParams {
            pad_id: 1,
            pad_token: "<pad>".into(),
            ..PaddingParams::default()
        }));
        Ok(Self { tokenizer, session })
    }

    fn encode(&mut self, text: &[&str]) -> Result<Vec<Embedding>> {
        let encoded = self
            .tokenizer
            .encode_batch(text.to_vec(), true)
            .map_err(e5_error)?;
        let tokens = encoded
            .first()
            .map(|input| input.get_ids().len())
            .ok_or("Порожній запит E5.")?;
        let shape = [encoded.len(), tokens];
        let tensor = |get: fn(&tokenizers::Encoding) -> &[u32]| {
            let values: Vec<i64> = encoded
                .iter()
                .flat_map(get)
                .map(|&value| i64::from(value))
                .collect();
            Tensor::from_array((shape, values)).map_err(e5_error)
        };
        let ids = tensor(tokenizers::Encoding::get_ids)?;
        let attention = tensor(tokenizers::Encoding::get_attention_mask)?;
        let types = tensor(tokenizers::Encoding::get_type_ids)?;
        let outputs = self
            .session
            .run(ort::inputs! {
                "input_ids" => ids, "attention_mask" => attention, "token_type_ids" => types
            })
            .map_err(e5_error)?;
        let (shape, hidden) = outputs
            .get("last_hidden_state")
            .ok_or("Модель E5 повернула невідомий вихід.")?
            .try_extract_tensor::<f32>()
            .map_err(e5_error)?;
        if shape.as_ref() != [encoded.len() as i64, tokens as i64, DIMENSIONS as i64] {
            return Err("Модель E5 повернула вектори неочікуваної форми.".into());
        }
        encoded
            .iter()
            .zip(hidden.chunks_exact(tokens * DIMENSIONS))
            .map(|(input, vectors)| pool(vectors, input.get_attention_mask()))
            .collect()
    }
}

fn pool(vectors: &[f32], mask: &[u32]) -> Result<Embedding> {
    let mut result = [0.0; DIMENSIONS];
    for token in vectors
        .chunks_exact(DIMENSIONS)
        .zip(mask)
        .filter_map(|(row, &active)| (active != 0).then_some(row))
    {
        for (element, &value) in result.iter_mut().zip(token) {
            *element += value;
        }
    }
    // Mean pooling followed by L2 normalization cancels the positive token-count divisor.
    let norm = cosine(&result, &result).sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err("Модель E5 повернула порожній або пошкоджений вектор.".into());
    }
    for value in &mut result {
        *value /= norm;
    }
    Ok(result)
}

fn cosine(left: &Embedding, right: &Embedding) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn check_priority(should_yield: &dyn Fn() -> bool) -> Result<()> {
    if should_yield() {
        return Err("Пошук призупинено, щоб виконати переклад. Повторіть запит.".into());
    }
    Ok(())
}

fn e5_error(error: impl std::fmt::Display) -> String {
    format!("Помилка локального семантичного пошуку. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooling_ignores_padding_and_normalizes() {
        let mut rows = [0.0; DIMENSIONS * 3];
        rows[0] = 3.0;
        rows[DIMENSIONS + 1] = 4.0;
        rows[2 * DIMENSIONS + 2] = 99.0;
        let vector = pool(&rows, &[1, 1, 0]).unwrap();
        assert_eq!(&vector[..3], &[0.6, 0.8, 0.0]);
        assert!(pool(&rows, &[0, 0, 0]).is_err());
    }
}
