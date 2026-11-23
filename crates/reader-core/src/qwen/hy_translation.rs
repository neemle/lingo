use super::{
    backend, context_parameters, decode_prompt, load_model, runtime_error, BATCH, CONTEXT_TOKENS,
    GENERATION_INPUT_TOO_LONG, RESPONSE_BYTES,
};
use crate::{
    optional_models::OptionalModelJob,
    protected_translation::ProtectedText,
    resources::RESOURCES,
    translation::{ModelEdge, TranslationBatchError},
    Result, Store,
};
use llama_cpp_2::{
    context::LlamaContext, llama_batch::LlamaBatch, model::LlamaModel, sampling::LlamaSampler,
};
use std::{cell::Cell, path::Path, time::{Duration, Instant}};

pub(crate) const LARGE_MODEL_ID: &str = "hy-mt2-7b";
pub(crate) const SMALL_MODEL_ID: &str = "hy-mt2-1.8b";
pub(super) const PROMPT: &str = "Translate the following text into Ukrainian. Note that you should only output the translated result without any additional explanation:";
const NUMBERED_PROMPT: &str = "Translate the following text into Ukrainian and preserve all digits. Output only the translated result:";
const AUTO_REVISION_PREFIX: &str = "hy-mt2-1.8b-auto-v1/";

pub(crate) struct AutomaticTranslation(OptionalModelJob);

impl AutomaticTranslation {
    pub(crate) fn prepare(store: &Store, source: &str, target: &str) -> Result<Option<Self>> {
        if (source, target) != ("fr", "uk") || store.reader_preferences()?.hy_auto_translation != Some(true)
            || !store.has_optional_model_files(SMALL_MODEL_ID)
        {
            return Ok(None);
        }
        store.prepare_optional_model(SMALL_MODEL_ID).map(Self).map(Some)
    }

    pub(crate) fn edge(&self) -> ModelEdge {
        ModelEdge {
            source: "fr".into(), target: "uk".into(),
            revision: format!("{AUTO_REVISION_PREFIX}{}", self.0.revision()),
            quality: None,
        }
    }

    pub(crate) fn matches(edge: &ModelEdge) -> bool {
        edge.source == "fr" && edge.target == "uk" && edge.revision.starts_with(AUTO_REVISION_PREFIX)
    }

    pub(crate) fn translate(
        &self, segments: &[String], regenerate: bool, background: bool, canceled: &dyn Fn() -> bool,
    ) -> std::result::Result<Vec<Result<String>>, TranslationBatchError> {
        let mut completed = Vec::with_capacity(segments.len());
        let deferred = Cell::new(false);
        let inference = |directory: &Path, should_yield: &dyn Fn() -> bool| {
            let interrupt = || {
                if canceled() || should_yield() {
                    deferred.set(true);
                    return Err("Переклад відкладено або скасовано.".into());
                }
                Ok(())
            };
            with_context(directory, interrupt, |model, context| {
                for source in segments {
                    interrupt()?;
                    let protected = ProtectedText::new(source, "uk");
                    let text = if protected.only_protected() {
                        protected.restore(protected.input().to_owned())
                    } else {
                        context.clear_kv_cache();
                        let until = Instant::now() + Duration::from_secs(30);
                        translate(Model::Small, model, context, protected.input(), regenerate, || {
                            interrupt()?;
                            if Instant::now() >= until {
                                return Err("Час перекладу Hy-MT2 вичерпано.".into());
                            }
                            Ok(())
                        }).and_then(|text| protected.restore(text))
                            .and_then(|text| super::passage_translation::validate_hy_output(text, source))
                    };
                    interrupt()?;
                    completed.push(text);
                }
                Ok(())
            })
        };
        let result = if background {
            let permit = RESOURCES.optional().map_err(TranslationBatchError::deferred)?;
            self.0.run_checked(inference, &|| {
                let yield_now = canceled() || permit.should_yield();
                if yield_now { deferred.set(true); }
                yield_now
            })
        } else {
            self.0.run_requested_with_cache(Some(canceled), || Ok(None), inference)
        };
        match result {
            Ok(()) => Ok(completed),
            Err(message) if deferred.get() || canceled() =>
                Err(TranslationBatchError::Deferred { message, completed }),
            Err(message) => Err(TranslationBatchError::Failed(message)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Model {
    Large,
    Small,
}

impl Model {
    pub(super) fn installed(store: &Store) -> Option<Self> {
        let large = store.has_optional_model_files(LARGE_MODEL_ID);
        if store.has_optional_model_files(SMALL_MODEL_ID)
            && !(large && store.optional_model_memory_ready(LARGE_MODEL_ID))
        {
            Some(Self::Small)
        } else {
            large.then_some(Self::Large)
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Large => LARGE_MODEL_ID,
            Self::Small => SMALL_MODEL_ID,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Large => "Hy-MT2 7B",
            Self::Small => "Hy-MT2 1.8B",
        }
    }

    fn sampler(self, model: &LlamaModel, regenerate: bool) -> LlamaSampler {
        if matches!(self, Self::Small) && !regenerate {
            return LlamaSampler::greedy();
        }
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(model.n_vocab() as i32, 64, 1.05, 0.0, 0.0),
            LlamaSampler::top_k(20),
            LlamaSampler::top_p(0.6, 1),
            LlamaSampler::temp(0.7),
            LlamaSampler::dist(if regenerate { u32::MAX } else { 42 }),
        ])
    }
}

pub(super) fn with_context<T>(
    directory: &Path,
    check: impl Fn() -> Result<()>,
    run: impl FnOnce(&LlamaModel, &mut LlamaContext<'_>) -> Result<T>,
) -> Result<T> {
    check()?;
    let backend = backend()?;
    let model = load_model(backend, &directory.join("model.gguf"), 0)?;
    check()?;
    let mut context = model
        .new_context(backend, context_parameters(false))
        .map_err(runtime_error)?;
    run(&model, &mut context)
}

pub(super) fn translate(
    variant: Model,
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    source: &str,
    regenerate: bool,
    check: impl Fn() -> Result<()>,
) -> Result<String> {
    let vocab = model.vocab();
    // Match the pinned checkpoint's single-user template. Only the fixed delimiters may tokenize as special tokens.
    let (prefix, suffix) = match variant {
        Model::Large => ("<|startoftext|>", "<|extra_0|>"),
        Model::Small => ("<｜hy_begin▁of▁sentence｜><｜hy_User｜>", "<｜hy_Assistant｜>"),
    };
    let mut tokens = vocab.tokenize(prefix.as_bytes(), false, true);
    let instruction = if source.bytes().any(|byte| byte.is_ascii_digit()) {
        NUMBERED_PROMPT
    } else {
        PROMPT
    };
    let prompt = format!("{instruction}\n{source}");
    tokens.extend(vocab.tokenize(prompt.as_bytes(), false, false));
    tokens.extend(vocab.tokenize(suffix.as_bytes(), false, true));
    let max_output = (CONTEXT_TOKENS as usize)
        .checked_sub(tokens.len())
        .filter(|&free| free >= 224)
        .ok_or(GENERATION_INPUT_TOO_LONG)?
        .min(320);

    let mut batch = LlamaBatch::new(BATCH, 1);
    decode_prompt(context, &mut batch, &tokens, &check)?;
    let mut sampler = variant.sampler(model, regenerate);
    let mut output = Vec::new();
    for position in tokens.len()..tokens.len() + max_output {
        check()?;
        let token = sampler.sample(context, -1);
        if vocab.is_eog(token) {
            return String::from_utf8(output).map_err(runtime_error);
        }
        vocab.token_to_piece_into(token, &mut output, false, None);
        // Some GGUF revisions emit the assistant terminator as visible text.
        if let Some(marker) = [b"<|extra_1|>".as_slice(), b"<|extra_1>".as_slice()]
            .into_iter()
            .find(|marker| output.ends_with(marker))
        {
            output.truncate(output.len() - marker.len());
            return String::from_utf8(output).map_err(runtime_error);
        }
        if output.len() > RESPONSE_BYTES {
            break;
        }
        batch.clear();
        batch
            .add(token, position as i32, &[0], true)
            .map_err(runtime_error)?;
        context.decode(&mut batch).map_err(runtime_error)?;
    }
    Ok(String::new())
}

pub(crate) fn smoke(directory: &Path, small: bool, interrupted: &dyn Fn() -> bool) -> Result<()> {
    let check = || {
        if interrupted() {
            Err("Контрольний переклад Hy-MT2 перервано.".into())
        } else {
            Ok(())
        }
    };
    with_context(directory, check, |model, context| {
        let source = "Bonjour, je lis en français.";
        let variant = if small { Model::Small } else { Model::Large };
        let text = translate(variant, model, context, source, false, check)?;
        if text
            .chars()
            .any(|ch| matches!(ch, '\u{0400}'..='\u{04ff}') && ch.is_alphabetic())
        {
            Ok(())
        } else {
            Err("Hy-MT2 не переклав контрольне речення українською.".into())
        }
    })
}
