use crate::optional_models::OptionalModelJob;
use crate::{
    onnx_runtime, resources::InferencePriority,
    speech::{RenderedSpeech, SpeechAdapter, SpeechSource},
    Result, Store,
};
use ort::{session::Session, value::Tensor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) mod cache;
pub(crate) mod uk_numbers;

const MODEL: &str = "piper-uk";
const SAMPLE_RATE: u32 = 22_050;
const MAX_INPUT_BYTES: usize = 4096;
const MAX_PHONEMES_PER_CHUNK: usize = 150;
pub(crate) const MAX_SECONDS: usize = 75;
const MAX_SAMPLES: usize = SAMPLE_RATE as usize * MAX_SECONDS;
const SPEAKERS: [(&str, u32); 3] = [("lada", 0), ("mykyta", 1), ("tetiana", 2)];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechAudio {
    pub sample_rate: u32,
    pub samples: u32,
    pub wav_bytes: Vec<u8>,
    pub playback_rate: f32,
}

pub struct SpeechJob {
    model: OptionalModelJob,
    text: String,
    speaker: u32,
    rate: f32,
}

impl Store {
    pub fn prepare_speech(
        &self,
        document_id: &str,
        sentence_id: &str,
        speaker: u32,
        rate: f32,
    ) -> Result<SpeechJob> {
        let (_, source) = self.speech_source(document_id, sentence_id, false)?
            .ok_or("Речення для озвучення не знайдено")?;
        self.prepare_piper_source(source, speaker, rate)
    }

    pub(crate) fn prepare_piper_source(
        &self, source: SpeechSource, speaker: u32, rate: f32,
    ) -> Result<SpeechJob> {
        if !SPEAKERS.iter().any(|voice| voice.1 == speaker) {
            return Err("Невідомий голос українського мовного пакета".into());
        }
        if !(0.4..=2.0).contains(&rate) {
            return Err("Швидкість озвучення має бути від 0,4 до 2".into());
        }

        if source.language != "uk" {
            return Err("Цей голос підтримує українську мову".into());
        }
        let text = source.text;
        if text.len() > MAX_INPUT_BYTES {
            return Err("Речення завелике для локального озвучення".into());
        }
        Ok(SpeechJob {
            model: self.prepare_optional_model(MODEL)?,
            text,
            speaker,
            rate,
        })
    }
}

impl SpeechJob {
    pub fn run(self) -> Result<SpeechAudio> {
        self.render(InferencePriority::Foreground, None).map(|result| result.audio)
    }

    fn render(self, priority: InferencePriority, canceled: Option<&dyn Fn() -> bool>) -> Result<RenderedSpeech> {
        let normalized = uk_numbers::expand(&self.text);
        let spoken = ukrainian_stress(&normalized);
        let synthesis_rate = model_synthesis_rate(self.rate);
        let cache = cache::SpeechCacheKey::new(
            "piper-uk-narration-v3",
            self.model.revision(),
            "uk",
            &self.speaker.to_string(),
            synthesis_rate,
            &spoken,
            SAMPLE_RATE,
        );
        let mut rendered = self.model.run_cached(
            priority,
            canceled,
            |directory| cache.read(directory).map(|audio| RenderedSpeech { audio, cached: true }),
            |directory, should_yield| {
                let audio = synthesize_at(
                    directory,
                    &spoken,
                    self.speaker,
                    synthesis_rate,
                    MAX_SAMPLES,
                    should_yield,
                )?;
                if should_yield() { return Err("Локальне озвучення перервано".into()); }
                let cached = cache.write(directory, &audio);
                Ok(RenderedSpeech { audio, cached })
            },
        )?;
        rendered.audio.playback_rate = self.rate / synthesis_rate;
        Ok(rendered)
    }
}

// Very long model durations can repeat sounds. The native player applies the
// remaining slowdown after synthesis without lowering pitch.
pub(crate) fn model_synthesis_rate(requested: f32) -> f32 {
    requested.max(0.8)
}

pub(crate) fn spoken_grapheme(text: &str, compatibility: bool, supports: impl Fn(char) -> bool) -> Result<String> {
    let decomposed: String = if compatibility { text.nfkd().collect() } else { text.nfd().collect() };
    if decomposed.chars().all(&supports) {
        return Ok(decomposed);
    }
    let composed: String = decomposed.nfc().collect();
    if composed.chars().all(supports) {
        return Ok(composed);
    }
    Err("Локальний голос не підтримує всі літери або наголоси в цьому тексті.".into())
}

impl SpeechAdapter for SpeechJob {
    fn render(self: Box<Self>, priority: InferencePriority, canceled: Option<&dyn Fn() -> bool>) -> Result<RenderedSpeech> {
        SpeechJob::render(*self, priority, canceled)
    }
}

fn synthesize_at(
    directory: &Path,
    spoken: &str,
    speaker: u32,
    rate: f32,
    sample_limit: usize,
    should_yield: &dyn Fn() -> bool,
) -> Result<SpeechAudio> {
    let config = PiperConfig::load(directory)?;
    let chunks = config.chunks(spoken)?;
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(3));
    let mut session = Session::builder(onnx_runtime::environment()?)
        .map_err(speech_error)?
        .with_intra_threads(threads)
        .map_err(speech_error)?
        .with_inter_threads(1)
        .map_err(speech_error)?
        .commit_from_file(directory.join("piper-uk.onnx"))
        .map_err(speech_error)?;

    let mut waveform = Vec::new();
    for chunk in chunks {
        if should_yield() {
            return Err("Локальне озвучення перервано".into());
        }
        let ids = config.encode(&chunk)?;
        let length = ids.len();
        let input = Tensor::from_array(([1, length], ids)).map_err(speech_error)?;
        let input_lengths = Tensor::from_array(([1], vec![length as i64])).map_err(speech_error)?;
        let scales = Tensor::from_array((
            [3],
            vec![
                config.inference.noise_scale,
                config.inference.length_scale / rate,
                config.inference.noise_w,
            ],
        ))
        .map_err(speech_error)?;
        let sid = Tensor::from_array(([1], vec![i64::from(speaker)])).map_err(speech_error)?;
        let outputs = session
            .run(ort::inputs! {
            "input" => input, "input_lengths" => input_lengths, "scales" => scales, "sid" => sid
                })
            .map_err(speech_error)?;
        let (shape, samples) = outputs
            .get("output")
            .ok_or("Голосовий пакет не повернув аудіо")?
            .try_extract_tensor::<f32>()
            .map_err(speech_error)?;
        match shape.as_ref() {
            [1, 1, 1, count] if *count > 0 && *count as usize == samples.len() => {}
            _ => return Err("Некоректний формат аудіо голосового пакета".into()),
        }
        let silence = if waveform.is_empty() {
            0
        } else {
            SAMPLE_RATE as usize / 20
        };
        if waveform.len() + silence + samples.len() > sample_limit {
            return Err(format!(
                "Озвучення перевищує допустимі {} секунд",
                sample_limit / SAMPLE_RATE as usize
            ));
        }
        waveform.resize(waveform.len() + silence, 0.0);
        waveform.extend_from_slice(samples);
    }
    to_wav(&waveform, SAMPLE_RATE)
}

pub(crate) fn smoke(directory: &Path, interrupted: &dyn Fn() -> bool) -> Result<()> {
    if interrupted() {
        return Err("Перевірку Piper відкладено.".into());
    }
    synthesize_at(
        directory,
        "Тест.",
        0,
        1.0,
        SAMPLE_RATE as usize * 4,
        interrupted,
    )?;
    if interrupted() {
        return Err("Перевірку Piper відкладено.".into());
    }
    Ok(())
}

pub(crate) const STRESS_MARK: char = '\u{301}';

/// Adds contextual acute marks for local Ukrainian voices without changing source text.
pub fn ukrainian_stress(text: &str) -> std::borrow::Cow<'_, str> {
    let mut spoken = String::new();
    let mut copied = 0;
    let mut previous = (0, "");
    let mut before_previous = (0, "");
    let phrase_space =
        |gap: &str| gap.chars().all(char::is_whitespace) && gap.matches('\n').nth(1).is_none();
    let cue = |word: &str| {
        word.chars()
            .filter(|&character| character != STRESS_MARK)
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let mut words = text.unicode_word_indices().peekable();
    while let Some((offset, word)) = words.next() {
        let (earlier_end, earlier_word) = before_previous;
        let (previous_end, previous_word) = previous;
        before_previous = previous;
        previous = (offset + word.len(), word);
        if word.contains(STRESS_MARK) {
            continue;
        }
        let lower = word.to_lowercase();
        if !matches!(lower.as_str(), "вікна" | "замок" | "руки" | "ноги" | "голови" | "води") {
            continue;
        }

        let has_prior = phrase_space(&text[previous_end..offset]);
        let prior = if has_prior { cue(previous_word) } else { String::new() };
        let earlier = if has_prior &&
            phrase_space(&text[earlier_end..previous_end - previous_word.len()])
        {
            cue(earlier_word)
        } else {
            String::new()
        };
        let next = words.peek()
            .filter(|(start, _)| phrase_space(&text[previous.0..*start]))
            .map_or_else(String::new, |(_, word)| cue(word));
        let vowel_index = match lower.as_str() {
            "вікна" => window_stress(&earlier, &prior, &next),
            "замок" => castle_or_lock_stress(&prior, &next),
            "руки" | "ноги" | "голови" | "води" => feminine_noun_stress(&lower, &earlier, &prior, &next),
            _ => None,
        };
        let Some(vowel_index) = vowel_index else { continue };
        let Some((index, vowel)) = word.char_indices().nth(vowel_index) else { continue };
        let stressed_end = offset + index + vowel.len_utf8();
        spoken.push_str(&text[copied..stressed_end]);
        spoken.push(STRESS_MARK);
        copied = stressed_end;
    }
    if copied == 0 {
        return text.into();
    }
    spoken.push_str(&text[copied..]);
    spoken.into()
}

fn window_stress(earlier: &str, prior: &str, next: &str) -> Option<usize> {
    // Two, three, and four retain the genitive singular stress in this plural form.
    let count =
        |token: &str| matches!(token, "два" | "обидва" | "три" | "чотири" | "2" | "3" | "4");
    let plural_modifier = |token: &str| {
        matches!(
            token,
            "відчинені"
                | "зачинені"
                | "великі"
                | "малі"
                | "нові"
                | "старі"
                | "ці"
                | "ті"
                | "усі"
                | "всі"
                | "мої"
                | "твої"
                | "свої"
                | "наші"
                | "ваші"
                | "їхні"
                | "інші"
                | "відкриті"
                | "закриті"
        )
    };
    let plural_predicate = |token: &str| {
        matches!(
            token,
            "відчинені" | "зачинені" | "відкриті" | "закриті" | "відчинилися" | "зачинилися"
                | "відчиняються" | "зачиняються" | "запітніли" | "виходять"
        )
    };

    match (earlier, prior) {
        (_, token) if genitive_preposition(token) || count(token) => Some(4),
        (preposition, modifier)
            if genitive_preposition(preposition) && modifier.ends_with("ого") => Some(4),
        (number, token) if count(number) && plural_modifier(token) => Some(4),
        (_, token) if matches!(
            token,
            "цього" | "того" | "одного" | "мого" | "твого" | "свого" | "нашого" | "вашого"
                | "рама" | "рами" | "раму" | "рамі" | "рамою" | "скло" | "скла" | "склі"
                | "стулка" | "стулки" | "стулку" | "стулок" | "підвіконня"
        ) => Some(4),
        (_, token) if plural_modifier(token) || matches!(
            token, "відчинив" | "відчинила" | "відчинили" | "зачинив" | "зачинила" | "зачинили"
                | "відчини" | "зачини" | "помив" | "помила" | "помили" | "помий"
        ) => Some(1),
        // At a clause boundary there is no preceding noun that can govern a genitive.
        (_, "") if plural_predicate(next) => Some(1),
        _ => None,
    }
}

fn castle_or_lock_stress(prior: &str, next: &str) -> Option<usize> {
    let castle = matches!(
        prior, "середньовічний" | "королівський" | "князівський" | "лицарський" | "готичний"
    ) || matches!(next, "височів" | "височіє" | "височить");
    let lock = matches!(
        prior,
        "дверний" | "навісний" | "врізний" | "кодовий"
            | "відімкнути" | "відімкнув" | "відімкнула" | "замкнути" | "замкнув" | "замкнула"
    ) || matches!(next, "дверей");
    match (castle, lock) {
        (true, false) => Some(1),
        (false, true) => Some(3),
        _ => None,
    }
}

fn genitive_preposition(token: &str) -> bool {
    matches!(
        token,
        "біля" | "поблизу" | "неподалік" | "навколо" | "до" | "від" | "без" | "коло"
            | "після" | "з" | "із" | "зі"
    )
}

fn feminine_noun_stress(noun: &str, earlier: &str, prior: &str, next: &str) -> Option<usize> {
    let genitive_index = if noun == "голови" { 5 } else { 3 };
    if genitive_preposition(prior)
        || matches!(prior, "моєї" | "твоєї" | "своєї" | "нашої" | "вашої" | "цієї" | "тієї" | "лівої" | "правої")
        || (genitive_preposition(earlier) && (prior.ends_with("ої") || prior.ends_with("єї")))
        || matches!((noun, prior), ("води", "склянка" | "склянку" | "літр" | "пляшка" | "пляшку" | "ковток"))
    {
        return Some(genitive_index);
    }
    if matches!(
        prior,
        "мої" | "твої" | "свої" | "наші" | "ваші" | "ці" | "ті" | "усі" | "всі"
    ) || matches!((noun, prior),
        ("руки" | "ноги", "обидві" | "підняв" | "підняла" | "підніми" | "помий" | "мити")
        | ("води", "мінеральні" | "територіальні" | "підземні")
    ) || (prior.is_empty() && matches!((noun, next),
        ("руки" | "ноги", "тремтять" | "змерзли" | "втомилися" | "опустилися")
        | ("голови", "схилилися" | "повернулися")
    )) {
        Some(1)
    } else {
        None
    }
}

#[derive(Deserialize)]
struct PiperConfig {
    audio: PiperAudio,
    inference: PiperInference,
    phoneme_type: String,
    num_symbols: u32,
    num_speakers: usize,
    speaker_id_map: BTreeMap<String, u32>,
    phoneme_id_map: BTreeMap<char, Vec<i64>>,
}

#[derive(Deserialize)]
struct PiperAudio {
    sample_rate: u32,
}

#[derive(Deserialize)]
struct PiperInference {
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
}

impl PiperConfig {
    fn load(directory: &Path) -> Result<Self> {
        let contents = fs::read(directory.join("piper-uk.json")).map_err(speech_error)?;
        let config: Self = serde_json::from_slice(&contents).map_err(speech_error)?;
        if config.audio.sample_rate != SAMPLE_RATE
            || config.phoneme_type != "text"
            || config.num_symbols != 256
            || config.num_speakers != SPEAKERS.len()
            || config.speaker_id_map.len() != SPEAKERS.len()
            || SPEAKERS
                .iter()
                .any(|(name, id)| config.speaker_id_map.get(*name) != Some(id))
            || !(0.0..=1.0).contains(&config.inference.noise_scale)
            || !(0.5..=2.0).contains(&config.inference.length_scale)
            || !(0.0..=1.0).contains(&config.inference.noise_w)
            || [('_', 0), ('^', 1), ('$', 2)]
                .into_iter()
                .any(|(symbol, id)| {
                    config.phoneme_id_map.get(&symbol).map(Vec::as_slice) != Some(&[id])
                })
            || config.phoneme_id_map.values().any(|ids| {
                ids.is_empty()
                    || ids.len() > 8
                    || ids
                        .iter()
                        .any(|id| !(0..i64::from(config.num_symbols)).contains(id))
            })
        {
            return Err("Некоректна конфігурація українських голосів".into());
        }
        Ok(config)
    }

    fn chunks(&self, text: &str) -> Result<Vec<String>> {
        let mut normalized = String::new();
        let lower = text.to_lowercase();
        for grapheme in lower.graphemes(true) {
            if grapheme.chars().any(char::is_alphanumeric) && !grapheme.bytes().all(|byte| byte.is_ascii_digit()) {
                normalized.push_str(&spoken_grapheme(grapheme, false, |ch| self.phoneme_id_map.contains_key(&ch))?);
                continue;
            }
            let symbol = grapheme.chars().next().unwrap_or(' ');
            match symbol {
                '0' => normalized.push_str(" нуль "),
                '1' => normalized.push_str(" один "),
                '2' => normalized.push_str(" два "),
                '3' => normalized.push_str(" три "),
                '4' => normalized.push_str(" чотири "),
                '5' => normalized.push_str(" п'ять "),
                '6' => normalized.push_str(" шість "),
                '7' => normalized.push_str(" сім "),
                '8' => normalized.push_str(" вісім "),
                '9' => normalized.push_str(" дев'ять "),
                '’' | '‘' | 'ʼ' | '`' => normalized.push('\''),
                '–' => normalized.push('—'),
                '‐' | '‑' | '−' => normalized.push('-'),
                '…' => normalized.push('.'),
                '_' | '^' | '$' => normalized.push(' '),
                value if self.phoneme_id_map.contains_key(&value) => normalized.push(value),
                _ => normalized.push(' '),
            }
        }
        if !normalized.chars().any(char::is_alphabetic) {
            return Err("У реченні немає тексту, який можна озвучити цим голосом".into());
        }

        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut count = 0;
        for word in normalized.split_whitespace() {
            let length = word.chars().count();
            if count + 1 + length > MAX_PHONEMES_PER_CHUNK && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                count = 0;
            }
            if !current.is_empty() {
                current.push(' ');
                count += 1;
            }
            for grapheme in word.graphemes(true) {
                let size = grapheme.chars().count();
                if size > MAX_PHONEMES_PER_CHUNK {
                    return Err("Речення містить надто довгий символ для озвучення".into());
                }
                if count + size > MAX_PHONEMES_PER_CHUNK {
                    chunks.push(std::mem::take(&mut current));
                    count = 0;
                }
                current.push_str(grapheme);
                count += size;
            }
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        Ok(chunks)
    }

    fn encode(&self, text: &str) -> Result<Vec<i64>> {
        let mut ids = vec![1];
        for symbol in text.chars() {
            ids.extend(
                self.phoneme_id_map
                    .get(&symbol)
                    .ok_or("Немає фонеми для озвучення")?,
            );
            ids.push(0);
        }
        ids.push(2);
        Ok(ids)
    }
}

pub(crate) fn to_wav(samples: &[f32], sample_rate: u32) -> Result<SpeechAudio> {
    if samples.is_empty()
        || samples.len() > sample_rate as usize * MAX_SECONDS
        || samples.iter().any(|x| !x.is_finite())
    {
        return Err("Некоректні зразки голосового пакета".into());
    }
    let (minimum, maximum) = samples.iter().fold((f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), &sample| (minimum.min(sample), maximum.max(sample)));
    if maximum - minimum <= 0.00001 {
        return Err("Локальний голос повернув аудіо без мовлення.".into());
    }
    let peak = 0.01_f32.max(minimum.abs()).max(maximum.abs());
    let scale = f32::from(i16::MAX) / peak;
    let sample_bytes = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + sample_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + sample_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&sample_bytes.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&((sample * scale).round() as i16).to_le_bytes());
    }
    Ok(SpeechAudio {
        sample_rate,
        samples: samples.len() as u32,
        wav_bytes: wav,
        playback_rate: 1.0,
    })
}

fn speech_error(error: impl std::fmt::Display) -> String {
    format!("Локальне озвучення Piper: {error}")
}

#[cfg(test)]
mod tests {
    use super::ukrainian_stress;

    #[test]
    fn marks_contextual_window_stress_without_rewriting_source() {
        for (source, spoken) in [
            (
                "Увечері ми читаємо книжку біля вікна.",
                "Увечері ми читаємо книжку біля вікна́.",
            ),
            ("У кімнаті відчинені вікна.", "У кімнаті відчинені ві́кна."),
            (
                "БІ́ЛЯ ВІКНА; ВІДЧИНЕНІ\tВІКНА.",
                "БІ́ЛЯ ВІКНА́; ВІДЧИНЕНІ\tВІ́КНА.",
            ),
            (
                "Усі вікна; біля, вікна; біля «Вікна».",
                "Усі ві́кна; біля, вікна; біля «Вікна».",
            ),
            ("біля ві́кна; відчинені вікна́", "біля ві́кна; відчинені вікна́"),
            (
                "біля цього вікна; відчинені два вікна",
                "біля цього вікна́; відчинені два вікна́",
            ),
            (
                "Є чотири великі вікна, а ці вікна старі.",
                "Є чотири великі вікна́, а ці ві́кна старі.",
            ),
            ("Є 3 вікна; біля\n\nВікна", "Є 3 вікна́; біля\n\nВікна"),
            ("Вікна відчинені. Зі старого вікна дме.", "Ві́кна відчинені. Зі старого вікна́ дме."),
            ("Скло вікна запітніло. Інші вікна зачинені.", "Скло вікна́ запітніло. Інші ві́кна зачинені."),
            ("Вікна, відчинені навстіж.", "Вікна, відчинені навстіж."),
            ("Стулки вікна відчинилися.", "Стулки вікна́ відчинилися."),
            ("Руки тремтять. Книжка випала з руки.", "Ру́ки тремтять. Книжка випала з руки́."),

            (
                "Наші вікна виходять у сад. Стіл біля великого вікна.",
                "Наші ві́кна виходять у сад. Стіл біля великого вікна́.",
            ),
            (
                "До нашого вікна; біля, великого вікна; якого вікна?",
                "До нашого вікна́; біля, великого вікна; якого вікна?",
            ),
        ] {
            assert_eq!(ukrainian_stress(source), spoken, "{source}");
        }
    }
}
