//! Supertonic 3 inference using the published ONNX graph interface.
//!
//! Inference stages and tensor layout follow the Supertonic reference from
//! https://github.com/supertone-oss-archive/supertonic (Supertone Inc., MIT).
//! The independently distributed model weights are licensed under OpenRAIL.

use crate::optional_models::OptionalModelJob;
use crate::tts::{cache::SpeechCacheKey, to_wav, ukrainian_stress, SpeechAudio, STRESS_MARK};
use crate::{
    onnx_runtime, resources::InferencePriority,
    speech::{RenderedSpeech, SpeechAdapter, SpeechSource},
    Result, Store,
};
use ort::session::Session;
use ort::value::Tensor;
use serde::{de::DeserializeOwned, Deserialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::BufReader, path::Path};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

const MODEL: &str = "supertonic-3";
const CORE_LANGUAGES: &[&str] = &[
    "uk", "fr", "en", "de", "es", "it", "pt", "pl", "nl", "cs", "ro", "sv",
];
const VOICES: &[&str] = &["M1.json", "F1.json"];
const SAMPLE_RATE: u32 = 44_100;
const LATENT_CHANNELS: usize = 24 * 6;
const LATENT_HOP: usize = 512 * 6;
const MAX_BYTES: usize = 4096;
const TEXT_WINDOW: usize = 110;
const MAX_SAMPLES: usize = SAMPLE_RATE as usize * 45;
const STEPS: usize = 5;

pub struct MultilingualSpeechJob {
    model: OptionalModelJob,
    language: String,
    text: String,
    voice: &'static str,
    rate: f32,
}

impl Store {
    pub fn prepare_multilingual_speech(
        &self,
        document_id: &str,
        sentence_id: &str,
        speaker: u32,
        rate: f32,
    ) -> Result<MultilingualSpeechJob> {
        let (_, source) = self.speech_source(document_id, sentence_id, false)?
            .ok_or("Речення для озвучення не знайдено")?;
        self.prepare_supertonic_source(source, speaker, rate)
    }

    pub(crate) fn prepare_supertonic_source(
        &self, source: SpeechSource, speaker: u32, rate: f32,
    ) -> Result<MultilingualSpeechJob> {
        let voice = *VOICES
            .get(speaker as usize)
            .ok_or("Невідомий голос Supertonic")?;
        if !(0.5..=2.0).contains(&rate) {
            return Err("Швидкість озвучення має бути від 0,5 до 2".into());
        }
        if !CORE_LANGUAGES.contains(&source.language.as_str()) {
            return Err("Цей голосовий пакет не підтримує мову документа".into());
        }
        let text = source.text;
        if text.len() > MAX_BYTES {
            return Err("Речення завелике для локального озвучення".into());
        }
        Ok(MultilingualSpeechJob {
            model: self.prepare_optional_model(MODEL)?,
            language: source.language,
            text,
            voice,
            rate,
        })
    }
}

impl MultilingualSpeechJob {
    pub fn run(self) -> Result<SpeechAudio> {
        self.render(InferencePriority::Foreground).map(|result| result.audio)
    }

    fn render(self, priority: InferencePriority) -> Result<RenderedSpeech> {
        let spoken = if self.language == "uk" {
            ukrainian_stress(&self.text)
        } else {
            self.text.as_str().into()
        };
        let cache = SpeechCacheKey::new(
            MODEL,
            self.model.revision(),
            &self.language,
            self.voice,
            self.rate,
            &spoken,
            SAMPLE_RATE,
        );
        self.model.run_cached(
            priority,
            |directory| cache.read(directory).map(|audio| RenderedSpeech { audio, cached: true }),
            |directory, should_yield| {
                let mut engine = Engine::load(directory, self.voice)?;
                let chunks = text_chunks(&spoken, &engine.indexer)?;
                let mut waveform = Vec::new();
                for chunk in chunks {
                    check_interrupted(should_yield)?;
                    let pause = if waveform.is_empty() {
                        0
                    } else {
                        SAMPLE_RATE as usize / 10
                    };
                    let remaining = MAX_SAMPLES - waveform.len();
                    if pause >= remaining {
                        return Err("Озвучення перевищує 45 секунд".into());
                    }
                    let audio = engine.synthesize(
                        &chunk,
                        &self.language,
                        self.rate,
                        remaining - pause,
                        should_yield,
                    )?;
                    waveform.resize(waveform.len() + pause, 0.0);
                    waveform.extend(audio);
                }
                let audio = to_wav(&waveform, SAMPLE_RATE)?;
                let cached = cache.write(directory, &audio);
                Ok(RenderedSpeech { audio, cached })
            },
        )
    }
}

impl SpeechAdapter for MultilingualSpeechJob {
    fn render(self: Box<Self>, priority: InferencePriority) -> Result<RenderedSpeech> {
        MultilingualSpeechJob::render(*self, priority)
    }
}

pub(crate) fn smoke(directory: &Path, interrupted: &dyn Fn() -> bool) -> Result<()> {
    for voice in VOICES {
        check_interrupted(interrupted)?;
        let mut engine = Engine::load(directory, voice)?;
        check_interrupted(interrupted)?;
        let samples =
            engine.synthesize("Тест.", "uk", 1.0, SAMPLE_RATE as usize * 4, interrupted)?;
        to_wav(&samples, SAMPLE_RATE)?;
    }
    check_interrupted(interrupted)
}

#[derive(Deserialize)]
struct Configuration {
    ae: AudioConfiguration,
    ttl: LatentConfiguration,
}

#[derive(Deserialize)]
struct AudioConfiguration {
    sample_rate: u32,
    base_chunk_size: usize,
}

#[derive(Deserialize)]
struct LatentConfiguration {
    latent_dim: usize,
    chunk_compress_factor: usize,
}

#[derive(Deserialize)]
struct Style {
    style_dp: StyleTensor,
    style_ttl: StyleTensor,
}

#[derive(Deserialize)]
struct StyleTensor {
    data: Vec<Vec<Vec<f32>>>,
    dims: [usize; 3],
    #[serde(rename = "type")]
    dtype: String,
}

impl StyleTensor {
    fn into_tensor(self, expected: [usize; 3]) -> Result<Tensor<f32>> {
        if self.dims != expected
            || self.dtype != "float32"
            || self.data.len() != expected[0]
            || self.data.iter().any(|rows| {
                rows.len() != expected[1]
                    || rows.iter().any(|row| {
                        row.len() != expected[2] || row.iter().any(|value| !value.is_finite())
                    })
            })
        {
            return Err("Некоректний формат голосу Supertonic".into());
        }
        tensor(
            expected,
            self.data.into_iter().flatten().flatten().collect(),
        )
    }
}

struct Engine {
    indexer: Vec<i64>,
    duration_style: Tensor<f32>,
    text_style: Tensor<f32>,
    duration: Session,
    text: Session,
    estimator: Session,
    vocoder: Session,
}

impl Engine {
    fn load(directory: &Path, voice: &str) -> Result<Self> {
        let cfg: Configuration = read_json(&directory.join("tts.json"))?;
        if cfg.ae.sample_rate != SAMPLE_RATE
            || cfg.ae.base_chunk_size != 512
            || cfg.ttl.latent_dim != 24
            || cfg.ttl.chunk_compress_factor != 6
        {
            return Err("Некоректна конфігурація Supertonic 3".into());
        }
        let indexer: Vec<i64> = read_json(&directory.join("unicode_indexer.json"))?;
        if indexer.len() != 65_536 || indexer.iter().any(|&id| !(-1..=8320).contains(&id)) {
            return Err("Некоректний алфавіт Supertonic 3".into());
        }
        let style: Style = read_json(&directory.join(voice))?;
        let env = onnx_runtime::environment()?;
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(3));
        let session = |name: &str| {
            Session::builder(env)
                .map_err(supertonic_error)?
                .with_intra_threads(threads)
                .map_err(supertonic_error)?
                .with_inter_threads(1)
                .map_err(supertonic_error)?
                .commit_from_file(directory.join(name))
                .map_err(supertonic_error)
        };
        Ok(Self {
            indexer,
            duration_style: style.style_dp.into_tensor([1, 8, 16])?,
            text_style: style.style_ttl.into_tensor([1, 50, 256])?,
            duration: session("duration_predictor.onnx")?,
            text: session("text_encoder.onnx")?,
            estimator: session("vector_estimator.onnx")?,
            vocoder: session("vocoder.onnx")?,
        })
    }

    fn synthesize(
        &mut self,
        chunk: &str,
        language: &str,
        rate: f32,
        sample_limit: usize,
        should_yield: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>> {
        let tagged = format!("<{language}>{chunk}</{language}>");
        let ids: Vec<i64> = tagged.chars().map(|c| index(&self.indexer, c)).collect();
        if ids.iter().any(|&id| id < 0) {
            return Err("Алфавіт Supertonic не містить мовного маркера".into());
        }
        let text_len = ids.len();
        let ids = tensor([1, text_len], ids)?;
        let mask = tensor([1, 1, text_len], vec![1.0_f32; text_len])?;

        let duration = self
            .duration
            .run(ort::inputs! {
                "text_ids" => &ids, "style_dp" => &self.duration_style, "text_mask" => &mask
            })
            .map_err(supertonic_error)?;
        let (_, seconds) = duration
            .get("duration")
            .ok_or("Supertonic не повернув тривалість")?
            .try_extract_tensor::<f32>()
            .map_err(supertonic_error)?;
        let [seconds] = seconds else {
            return Err("Некоректний формат тривалості Supertonic".into());
        };
        let predicted = seconds * SAMPLE_RATE as f32 / rate;
        if !predicted.is_finite() || predicted < 1.0 || predicted > sample_limit as f32 {
            return Err("Некоректна або завелика тривалість озвучення".into());
        }
        let sample_count = predicted as usize;

        check_interrupted(should_yield)?;
        let embedding = self
            .text
            .run(ort::inputs! {
                "text_ids" => &ids, "style_ttl" => &self.text_style, "text_mask" => &mask
            })
            .map_err(supertonic_error)?;
        let embedding = embedding
            .get("text_emb")
            .ok_or("Supertonic не повернув закодований текст")?;

        let latent_len = sample_count.div_ceil(LATENT_HOP);
        let latent_shape = [1, LATENT_CHANNELS, latent_len];
        // An explicit Ukrainian stress mark changes pronunciation, not the
        // initial noise used to choose prosody for the same text.
        let seed = if language == "uk" && tagged.contains(STRESS_MARK) {
            tagged.replace(STRESS_MARK, "").into()
        } else {
            std::borrow::Cow::Borrowed(tagged.as_str())
        };
        let mut latent = tensor(
            latent_shape,
            initial_noise(seed.as_bytes(), latent_len * LATENT_CHANNELS),
        )?;
        let latent_mask = tensor([1, 1, latent_len], vec![1.0_f32; latent_len])?;
        let total_steps = tensor([1], vec![STEPS as f32])?;
        for step in 0..STEPS {
            check_interrupted(should_yield)?;
            let current_step = tensor([1], vec![step as f32])?;
            let estimated = self
                .estimator
                .run(ort::inputs! {
                    "noisy_latent" => &latent,
                    "text_emb" => embedding,
                    "style_ttl" => &self.text_style,
                    "latent_mask" => &latent_mask,
                    "text_mask" => &mask,
                    "current_step" => current_step,
                    "total_step" => &total_steps,
                })
                .map_err(supertonic_error)?;
            let (shape, values) = estimated
                .get("denoised_latent")
                .ok_or("Supertonic не повернув прихований сигнал")?
                .try_extract_tensor::<f32>()
                .map_err(supertonic_error)?;
            if shape.as_ref() != [1, LATENT_CHANNELS as i64, latent_len as i64] {
                return Err("Некоректний формат прихованого сигналу Supertonic".into());
            }
            latent = tensor(latent_shape, values.to_vec())?;
        }

        check_interrupted(should_yield)?;
        let audio = self
            .vocoder
            .run(ort::inputs! { "latent" => latent })
            .map_err(supertonic_error)?;
        let (_, samples) = audio
            .get("wav_tts")
            .ok_or("Supertonic не повернув аудіо")?
            .try_extract_tensor::<f32>()
            .map_err(supertonic_error)?;
        let samples = samples
            .get(..sample_count)
            .ok_or("Supertonic повернув неповне аудіо")?;
        Ok(samples.to_vec())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path).map_err(supertonic_error)?;
    serde_json::from_reader(BufReader::new(file)).map_err(supertonic_error)
}

fn tensor<
    T: ort::value::PrimitiveTensorElementType + std::fmt::Debug + Copy + 'static,
    const N: usize,
>(
    shape: [usize; N],
    data: Vec<T>,
) -> Result<Tensor<T>> {
    Tensor::from_array((shape, data)).map_err(supertonic_error)
}

fn check_interrupted(should_yield: &dyn Fn() -> bool) -> Result<()> {
    if should_yield() {
        return Err("Локальне озвучення перервано".into());
    }
    Ok(())
}

fn index(indexer: &[i64], c: char) -> i64 {
    indexer.get(c as usize).copied().unwrap_or(-1)
}

fn text_chunks(text: &str, indexer: &[i64]) -> Result<Vec<String>> {
    let normalized: String = text
        .nfkd()
        .map(|c| match c {
            '–' | '—' | '‑' | '‐' => '-',
            '„' | '«' | '»' | '“' | '”' => '"',
            '‘' | '’' | 'ʼ' | '`' => '\'',
            '[' | ']' | '<' | '>' | '_' | '|' | '/' | '#' | '\\' => ' ',
            c if c.is_whitespace() || index(indexer, c) < 0 => ' ',
            c => c,
        })
        .collect();

    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut width = 0;
    for word in normalized.split_whitespace() {
        if !chunk.is_empty() && width + 1 + word.chars().count() > TEXT_WINDOW {
            chunks.push(std::mem::take(&mut chunk));
            width = 0;
        }
        if !chunk.is_empty() {
            chunk.push(' ');
            width += 1;
        }
        for grapheme in word.graphemes(true) {
            let size = grapheme.chars().count();
            if size > TEXT_WINDOW {
                return Err("Надто довгий символ у тексті для озвучення".into());
            }
            if width + size > TEXT_WINDOW {
                chunks.push(std::mem::take(&mut chunk));
                width = 0;
            }
            chunk.push_str(grapheme);
            width += size;
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    if !chunks
        .iter()
        .any(|chunk| chunk.chars().any(char::is_alphanumeric))
    {
        return Err("У реченні немає тексту для озвучення".into());
    }
    for chunk in &mut chunks {
        if !chunk.ends_with(['.', '!', '?', ':', ';', ',', '\'', '"', ')']) {
            chunk.push('.');
        }
    }
    Ok(chunks)
}

fn initial_noise(seed: &[u8], count: usize) -> Vec<f32> {
    // A local deterministic seed keeps repeated reads consistent. Box–Muller
    // converts pairs of uniform samples to the standard normal expected by ONNX.
    let digest = Sha256::digest(seed);
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&digest[..8]);
    let mut state = u64::from_le_bytes(bytes).max(1);
    let mut uniform = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 40) as f32 + 0.5) / 16_777_216.0
    };
    let mut noise = Vec::with_capacity(count);
    while noise.len() < count {
        let radius = (-2.0 * uniform().ln()).sqrt();
        let phase = std::f32::consts::TAU * uniform();
        let (sin, cos) = phase.sin_cos();
        noise.push(radius * cos);
        if noise.len() < count {
            noise.push(radius * sin);
        }
    }
    noise
}

fn supertonic_error(error: impl std::fmt::Display) -> String {
    format!("Локальне озвучення Supertonic: {error}")
}
