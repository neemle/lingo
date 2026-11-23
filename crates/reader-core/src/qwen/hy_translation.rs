use super::{
    backend, context_parameters, decode_prompt, load_model, runtime_error, BATCH, CONTEXT_TOKENS,
    GENERATION_INPUT_TOO_LONG, RESPONSE_BYTES,
};
use crate::Result;
use llama_cpp_2::{
    context::LlamaContext, llama_batch::LlamaBatch, model::LlamaModel, sampling::LlamaSampler,
};
use std::path::Path;

pub(crate) const MODEL_ID: &str = "hy-mt2-7b";
pub(super) const MODEL_NAME: &str = "Hy-MT2 7B";
pub(super) const PROMPT: &str = "Translate the following text into Ukrainian. Note that you should only output the translated result without any additional explanation:";
const NUMBERED_PROMPT: &str = "Translate the following text into Ukrainian and preserve all digits. Output only the translated result:";

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
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    source: &str,
    regenerate: bool,
    check: impl Fn() -> Result<()>,
) -> Result<String> {
    let vocab = model.vocab();
    // Match the pinned checkpoint's single-user template. Only the fixed delimiters may tokenize as special tokens.
    let mut tokens = vocab.tokenize(b"<|startoftext|>", false, true);
    let instruction = if source.bytes().any(|byte| byte.is_ascii_digit()) {
        NUMBERED_PROMPT
    } else {
        PROMPT
    };
    let prompt = format!("{instruction}\n{source}");
    tokens.extend(vocab.tokenize(prompt.as_bytes(), false, false));
    tokens.extend(vocab.tokenize(b"<|extra_0|>", false, true));
    let max_output = (CONTEXT_TOKENS as usize)
        .checked_sub(tokens.len())
        .filter(|&free| free >= 224)
        .ok_or(GENERATION_INPUT_TOO_LONG)?
        .min(320);

    let mut batch = LlamaBatch::new(BATCH, 1);
    decode_prompt(context, &mut batch, &tokens, &check)?;
    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::penalties(model.n_vocab() as i32, 64, 1.05, 0.0, 0.0),
        LlamaSampler::top_k(20),
        LlamaSampler::top_p(0.6, 1),
        LlamaSampler::temp(0.7),
        LlamaSampler::dist(if regenerate { u32::MAX } else { 42 }),
    ]);
    let mut output = Vec::new();
    for position in tokens.len()..tokens.len() + max_output {
        check()?;
        let token = sampler.sample(context, -1);
        if vocab.is_eog(token) {
            return String::from_utf8(output).map_err(runtime_error);
        }
        vocab.token_to_piece_into(token, &mut output, false, None);
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

pub(crate) fn smoke(directory: &Path, interrupted: &dyn Fn() -> bool) -> Result<()> {
    let check = || {
        if interrupted() {
            Err("Контрольний переклад Hy-MT2 перервано.".into())
        } else {
            Ok(())
        }
    };
    with_context(directory, check, |model, context| {
        let source = "Bonjour, je lis en français.";
        let text = translate(model, context, source, false, check)?;
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
