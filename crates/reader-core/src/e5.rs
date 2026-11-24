use crate::{optional_models::OptionalModelJob, resources::InferencePriority, Result, Store};
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
const RANKING_CACHE_LIMIT: usize = 64;
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
        let mut passages: Vec<_> = self
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
        passages.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        Ok(SemanticVocabularyJob {
            model,
            query: format!("query: {query}"),
            passages,
        })
    }
}

impl SemanticVocabularyJob {
    pub fn run(self) -> Result<Vec<String>> {
        let Self { model, query, passages } = self;
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let key = ranking_key(model.revision(), &query, &passages);
        let generation = cache().lock().map_err(e5_error)?.generation;
        model.run_cached(InferencePriority::Background, None, |_| {
            cache().lock().ok()?.ranking(&key)
        }, |directory, should_yield| {
            let mut encoder = E5::open(directory)?;
            check_priority(should_yield)?;
            let query = encoder.encode(&[query.as_str()])?[0];
            let mut ranking = Vec::with_capacity(passages.len());
            for batch in passages.chunks(BATCH_SIZE) {
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
                        if generation == cache.generation { cache.insert(passage.key, vector); }
                    }
                }
            }
            check_priority(should_yield)?;
            ranking.sort_unstable_by(|(a, x), (b, y)| y.total_cmp(x).then_with(|| a.cmp(b)));
            let entries: Vec<_> = ranking
                .into_iter()
                .take(RESULT_LIMIT)
                .map(|(id, _)| id.clone())
                .collect();
            let mut cache = cache().lock().map_err(e5_error)?;
            if generation == cache.generation {
                cache.insert_ranking(key, entries.clone());
            }
            Ok(entries)
        })
    }
}

fn ranking_key(revision: &str, query: &str, passages: &[Passage]) -> Key {
    let mut hash = Sha256::new();
    for value in [revision, query] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    for passage in passages {
        hash.update((passage.id.len() as u64).to_le_bytes());
        hash.update(passage.id.as_bytes());
        hash.update(passage.key);
    }
    hash.finalize().into()
}

pub(crate) fn smoke(directory: &Path, interrupted: &dyn Fn() -> bool) -> Result<()> {
    check_priority(interrupted)?;
    let mut encoder = E5::open(directory)?;
    check_priority(interrupted)?;
    let vectors = encoder.encode(&["query: book"])?;
    if vectors.len() != 1 || vectors[0].iter().any(|value| !value.is_finite()) {
        return Err("E5 повернув некоректний контрольний вектор.".into());
    }
    check_priority(interrupted)
}

#[derive(Default)]
struct VectorCache {
    vectors: HashMap<Key, Embedding>,
    order: VecDeque<Key>,
    rankings: VecDeque<(Key, Vec<String>)>,
    generation: u64,
}

impl VectorCache {
    fn ranking(&self, key: &Key) -> Option<Vec<String>> {
        self.rankings.iter().find(|(entry, _)| entry == key).map(|(_, ids)| ids.clone())
    }

    fn insert_ranking(&mut self, key: Key, ids: Vec<String>) {
        if self.rankings.iter().any(|(entry, _)| *entry == key) {
            return;
        }
        if self.rankings.len() == RANKING_CACHE_LIMIT {
            self.rankings.pop_front();
        }
        self.rankings.push_back((key, ids));
    }

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

pub(crate) fn clear_cache() {
    let mut cache = cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = cache.generation.wrapping_add(1);
    *cache = VectorCache { generation, ..VectorCache::default() };
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
    fn cached_ranking_requires_the_same_query_collection_and_model() {
        let card = |id: &str, content| Passage {
            id: id.into(),
            text: String::new(),
            key: [content; 32],
        };
        let original = [card("one", 1), card("two", 2)];
        let key = ranking_key("model-v1", "query: livre", &original);
        let mut cache = VectorCache::default();
        cache.insert_ranking(key, vec!["two".to_owned(), "one".to_owned()]);
        assert_eq!(cache.ranking(&key).unwrap(), ["two", "one"]);
        assert!(cache.ranking(&ranking_key("model-v1", "query: jardin", &original)).is_none());
        assert!(cache.ranking(&ranking_key("model-v2", "query: livre", &original)).is_none());
        assert!(cache.ranking(&ranking_key("model-v1", "query: livre", &original[..1])).is_none());
        let edited = [card("one", 1), card("two", 3)];
        assert!(cache.ranking(&ranking_key("model-v1", "query: livre", &edited)).is_none());
        for index in 0..RANKING_CACHE_LIMIT {
            cache.insert_ranking([index as u8; 32], Vec::new());
        }
        assert!(cache.ranking(&key).is_none());
        assert_eq!(cache.rankings.len(), RANKING_CACHE_LIMIT);
    }

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
