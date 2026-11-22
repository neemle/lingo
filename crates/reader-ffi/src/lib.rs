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
    if let Command::StageIntake { input, request_id } = command {
        let root = {
            let state = registry::lock()?;
            let store = state.store(handle)?;
            if let Some(id) = &request_id {
                if let Some(item) = store.intake_item_for_request(id).map_err(gateway::domain)? {
                    drop(state);
                    return encode_job(Ok(item));
                }
            }
            store.pdf_original_root().to_owned()
        };
        let prepared = reader_core::intake::prepare(&root, input).map_err(gateway::domain)?;
        let item = registry::lock()?.insert_intake(handle, prepared, request_id.as_deref())?;
        return encode_job(Ok(item));
    }
    let command = match command {
        Command::AnalyzeLinguistics { id, sentence_id, revision_id, scope } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.analyze_linguistics(&id, &sentence_id, &revision_id, scope));
            }
            Command::AnalyzeLinguistics { id, sentence_id, revision_id, scope }
        }
        Command::PrefetchLinguistics { id, sentence_id, revision_id, step } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.prefetch_linguistics(&id, &sentence_id, &revision_id, step));
            }
            Command::PrefetchLinguistics { id, sentence_id, revision_id, step }
        }
        Command::ModelTransfer { package, action } => {
            let transfer = registry::lock()?.store(handle)?
                .prepare_model_transfer(package).map_err(gateway::domain)?;
            return encode_job(transfer.run(action));
        }
        Command::CompleteIntake { id, result } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.complete_intake(&id, result));
            }
            Command::CompleteIntake { id, result }
        }
        other => other,
    };
    if let Command::ImportPdfFile {
        path,
        name,
        language,
        page_texts,
        lines,
    } = command
    {
        let root = registry::lock()?
            .store(handle)?
            .pdf_original_root()
            .to_owned();
        let prepared = reader_core::file_originals::prepare_pdf_import(
            &root,
            &name,
            std::path::Path::new(&path),
            &language,
            page_texts,
            lines,
        )
        .map_err(gateway::domain)?;
        let document = {
            let mut state = registry::lock()?;
            catch_unwind(AssertUnwindSafe(|| state.import_pdf_file(handle, prepared)))
                .unwrap_or_else(|_| {
                    state.close(handle);
                    Err(Error::new(
                        "internal_error",
                        "Rust operation panicked; reopen the store",
                    ))
                })?
        };
        return encode_job(Ok(document));
    }

    match &command {
        Command::DiscardIntake { id } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.discard_intake(id));
            }
        }
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
        Command::RunTranslationWindow { id, expected } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.run_translation_window(id, expected.as_ref()));
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
        Command::InspectBackupFile { path } => {
            registry::lock()?.store(handle)?;
            return encode_job(reader_core::inspect_backup_file(std::path::Path::new(path)));
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
        }
        | Command::PrefetchSpeech {
            id,
            sentence_id,
            engine,
            speaker,
            rate,
        } => {
            let engine = reader_core::speech::ModelSpeechEngine::from_id(
                engine.as_deref().unwrap_or("piper-uk"),
            )
            .ok_or_else(|| Error::new("invalid_argument", "Невідомий рушій озвучення."))?;
            if matches!(&command, Command::PrefetchSpeech { .. }) {
                let job = registry::lock()?
                    .store(handle)?
                    .prepare_speech_prefetch(id, sentence_id, engine, *speaker, *rate)
                    .map_err(gateway::domain)?;
                return encode_job(job.map(|job| job.prefetch()).transpose());
            }
            let job = registry::lock()?
                .store(handle)?
                .prepare_model_speech(id, sentence_id, engine, *speaker, *rate)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
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
