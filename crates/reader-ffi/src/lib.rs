mod dispatch;
mod gateway;
mod registry;
mod validation;
mod wire;

use std::panic::{catch_unwind, AssertUnwindSafe};
use wire::{Command, Error, Result, MAX_WIRE, VERSION};

#[repr(C)]
#[derive(Default)]
pub struct ReaderBuffer {
    pub data: *const u8,
    pub len: u64,
    pub allocation: u64,
}

fn publish(result: Result<serde_json::Value>) -> Result<ReaderBuffer> {
    let envelope = match result {
        Ok(data) => serde_json::json!({ "version": VERSION, "ok": true, "data": data }),
        Err(error) => serde_json::json!({ "version": VERSION, "ok": false, "error": error }),
    };
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|_| Error::new("serialization_error", "Cannot encode response"))?
        .into_boxed_slice();
    if bytes.len() > MAX_WIRE {
        return publish(Err(Error::new(
            "response_too_large",
            "Response exceeds 256MiB wire bound",
        )));
    }
    let mut state = registry::lock()?;
    let allocation = state.next_id()?;
    let result = ReaderBuffer {
        data: bytes.as_ptr(),
        len: bytes.len() as u64,
        allocation,
    };
    state.buffers.insert(allocation, bytes);
    Ok(result)
}

unsafe fn invoke(handle: u64, input: *const u8, len: u64) -> Result<serde_json::Value> {
    if len > MAX_WIRE as u64 || len > isize::MAX as u64 || (input.is_null() && len != 0) {
        return Err(Error::new(
            "invalid_argument",
            "Invalid input pointer/length or wire bound",
        ));
    }
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, len as usize) }
    };
    let command = validation::decode(bytes)?;
    match &command {
        Command::TranslateParagraph {
            id,
            sentence_id,
            target,
            ..
        }
        | Command::TranslateSentence {
            id,
            sentence_id,
            target,
        } => {
            let worker = registry::lock()?
                .store(handle)?
                .prepare_translation_worker();
            if let Some(worker) = worker {
                return if let Command::TranslateParagraph { background, .. } = &command {
                    encode_job(worker.translate_paragraph(id, sentence_id, target, *background))
                } else {
                    dispatch::sentence_translation(worker.translate_sentence(
                        id,
                        sentence_id,
                        target,
                    ))
                };
            }
        }
        Command::FindTranslationPage {
            id,
            target,
            query,
            start,
        } => {
            let worker = registry::lock()?
                .store(handle)?
                .prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.find_translation_page(id, target, query, *start));
            }
        }
        Command::CachedTranslation {
            id,
            sentence_id,
            target,
        } => {
            let worker = registry::lock()?
                .store(handle)?
                .prepare_translation_worker();
            if let Some(worker) = worker {
                return dispatch::cached_translation(worker.cached_translation(
                    id,
                    sentence_id,
                    target,
                ));
            }
        }
        Command::SemanticVocabulary { query, document_id } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_semantic_vocabulary_in(query, document_id.as_deref())
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::ExplainWord {
            model,
            document,
            sentence,
            word,
        } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_word_explanation(model, document, sentence, word)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::InstallTranslationPackage { path } => {
            let files = registry::lock()?.store(handle)?.translation_package_files();
            return encode_job(files.install(std::path::Path::new(path)));
        }
        Command::ExportTranslationPackage {
            source,
            target,
            path,
        } => {
            let files = registry::lock()?.store(handle)?.translation_package_files();
            return encode_job(files.export(source, target, std::path::Path::new(path)));
        }
        Command::RemoveTranslationPackage { source, target } => {
            let files = registry::lock()?.store(handle)?.translation_package_files();
            return encode_job(files.remove(source, target));
        }
        Command::VerifyTranslationPackage { source, target } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_translation_package_verification(source, target)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::ValidateTranslationPackage { source, target } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_translation_package_validation(source, target)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::InstallOptionalModel { id, staging_root } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_optional_model_install(id, std::path::Path::new(staging_root))
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::ExportOptionalModelArchive { id, path }
        | Command::InstallOptionalModelArchive { id, path } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_optional_model_archive(id)
                .map_err(gateway::domain)?;
            let path = std::path::Path::new(path);
            return encode_job(
                if matches!(command, Command::ExportOptionalModelArchive { .. }) {
                    job.export(path)
                } else {
                    job.install(path)
                },
            );
        }
        Command::InstallKnowledgePackage { language, path } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_knowledge_package(language, std::path::Path::new(path))
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::InstallLexicalPackage { language, path } => {
            let job = registry::lock()?
                .store(handle)?
                .prepare_lexical_package(language, std::path::Path::new(path))
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::SynthesizeSpeech {
            id,
            sentence_id,
            engine,
            speaker,
            rate,
        } => match engine.as_deref().unwrap_or("piper-uk") {
            "piper-uk" => {
                let job = registry::lock()?
                    .store(handle)?
                    .prepare_speech(id, sentence_id, *speaker, *rate)
                    .map_err(gateway::domain)?;
                return encode_job(job.run());
            }
            "supertonic-3" => {
                let job = registry::lock()?
                    .store(handle)?
                    .prepare_multilingual_speech(id, sentence_id, *speaker, *rate)
                    .map_err(gateway::domain)?;
                return encode_job(job.run());
            }
            _ => return Err(Error::new("invalid_argument", "Невідомий рушій озвучення.")),
        },
        _ => {}
    }
    let mut state = registry::lock()?;
    catch_unwind(AssertUnwindSafe(|| state.call(handle, command))).unwrap_or_else(|_| {
        state.close(handle);
        Err(Error::new(
            "internal_error",
            "Rust operation panicked; reopen the store",
        ))
    })
}

fn encode_job<T: serde::Serialize>(result: reader_core::Result<T>) -> Result<serde_json::Value> {
    serde_json::to_value(result.map_err(gateway::domain)?)
        .map_err(|_| Error::new("serialization_error", "Cannot encode core response"))
}

#[no_mangle]
pub extern "C" fn reader_abi_version() -> u32 {
    VERSION
}

/// # Safety
/// Non-null input must point to len readable bytes for the entire synchronous call.
#[no_mangle]
pub unsafe extern "C" fn reader_call(handle: u64, input: *const u8, len: u64) -> ReaderBuffer {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe { invoke(handle, input, len) }))
        .unwrap_or_else(|_| Err(Error::new("internal_error", "Rust operation panicked")));
    catch_unwind(AssertUnwindSafe(|| publish(result).unwrap_or_default())).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn reader_close(handle: u64) -> u32 {
    catch_unwind(|| {
        registry::lock()
            .map(|mut state| u32::from(!state.close(handle)))
            .unwrap_or(2)
    })
    .unwrap_or(2)
}

#[no_mangle]
pub extern "C" fn reader_release(allocation: u64) -> u32 {
    catch_unwind(|| {
        registry::lock()
            .map(|mut state| u32::from(state.buffers.remove(&allocation).is_none()))
            .unwrap_or(2)
    })
    .unwrap_or(2)
}
