mod dispatch;
mod gateway;
mod registry;
mod validation;
mod wire;

use std::panic::{catch_unwind, AssertUnwindSafe};
use registry::ModelRequestKind;
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
    publish_bytes(bytes)
}

fn publish_bytes(bytes: Box<[u8]>) -> Result<ReaderBuffer> {
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
    if let Command::ParseOcrTsv { tsv, width, height, sparse_tsv, image_rotation } = command {
        if let Some(angle) = image_rotation {
            return encode_job(reader_core::ocr::parse_tesseract_quarter_turn(&tsv, width, height, angle));
        }
        let lines = reader_core::ocr::parse_tesseract_tsv(&tsv, width, height);
        return encode_job(lines.map(|lines| match sparse_tsv {
            Some(tsv) => reader_core::ocr::recover_tesseract_lines(lines, &tsv, width, height),
            None => lines,
        }));
    }
    if let Command::MergeRotatedOcr { primary, native, page_size } = command {
        return encode_job(reader_core::ocr::merge_rotated_lines(primary, native, page_size));
    }
    if let Command::SeparatePdfBlocks { text, words, page_width } = command {
        return encode_job(reader_core::pdf_text_blocks::separate_blocks(&text, &words, page_width));
    }
    if let Command::PlanPdfTranslationLayout { rows, geometry, offset, styles, preserve_artwork } = command {
        return encode_job(reader_core::pdf_translation_layout::project(
            &rows, &geometry, offset, &styles, preserve_artwork));
    }
    if let Command::FindPdfLigatureJoins { text, words } = command {
        return encode_job(reader_core::pdf_text_blocks::ligature_joins(&text, &words));
    }
    if let Command::SamplePdfBackgrounds { rgba, width, height, boxes, with_ink } = command {
        if rgba.len() > 5_600_000 {
            return Err(Error::new("invalid_argument", "PDF preview exceeds the sampling bound"));
        }
        let pixels = base64::decode(rgba)
            .map_err(|_| Error::new("invalid_argument", "Invalid PDF preview image data"))?;
        if with_ink {
            return encode_job(reader_core::pdf_appearance::sample_colors(&pixels, width, height, &boxes));
        }
        return encode_job(reader_core::pdf_appearance::sample_backgrounds(&pixels, width, height, &boxes));
    }
    let cancellation = match &command {
        Command::CancelModelTranslation { request_id } => Some((ModelRequestKind::Passage, *request_id)),
        Command::CancelOcrPageReview { request_id } => Some((ModelRequestKind::OcrPageReview, *request_id)),
        Command::CancelSelectionTranslation { request_id } => Some((ModelRequestKind::SelectionTranslation, *request_id)),
        Command::CancelOfflinePassage { request_id } => Some((ModelRequestKind::OfflinePassage, *request_id)),
        Command::CancelSentenceReview { request_id } => Some((ModelRequestKind::SentenceReview, *request_id)),
        Command::CancelWordExplanation { request_id } => Some((ModelRequestKind::WordExplanation, *request_id)),
        Command::CancelPdfPageTranslation { request_id } => Some((ModelRequestKind::PdfPageTranslation, *request_id)),
        Command::CancelBackgroundTranslation { request_id } => Some((ModelRequestKind::BackgroundTranslation, *request_id)),
        Command::CancelSpeech { request_id, prefetch } => Some((speech_request_kind(*prefetch), *request_id)),
        _ => None,
    };
    if let Some((kind, request_id)) = cancellation {
        // Cancellation must reach inference even when storage maintenance is waiting on it.
        registry::lock()?.model_cancellation(handle, kind)?.cancel_through(request_id);
        return Ok(serde_json::Value::Null);
    }
    // Maintenance waits for independent worker connections as well as registry calls.
    static STORAGE_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());
    let exclusive = matches!(&command, Command::ClearStorage { .. });
    let _write = exclusive.then(|| STORAGE_GATE.write().unwrap_or_else(|error| error.into_inner()));
    let _read = (!exclusive).then(|| STORAGE_GATE.read().unwrap_or_else(|error| error.into_inner()));
    if let Command::AdvanceBackgroundTranslation { id, revision, target, request_id } = &command {
        let canceled = request_cancellation(handle, *request_id, ModelRequestKind::BackgroundTranslation)?;
        let canceled_error = || Error::new("canceled", "Фоновий переклад скасовано.");
        if canceled() { return Err(canceled_error()); }
        let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
        if let Some(worker) = worker {
            let result = worker.advance_background_translation_until_canceled(id, revision, target, &canceled);
            return if canceled() { Err(canceled_error()) } else { encode_job(result) };
        }
    }
    if let Command::WarmTranslationStep { id, revision, target, start } = &command {
        let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
        if let Some(worker) = worker {
            return encode_job(worker.warm_translation_step(id, revision, target, *start));
        }
    }
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
        let prepared = reader_core::intake::prepare_for_request(&root, input, request_id.as_deref(), |bytes| {
            registry::lock().and_then(|state| state.store(handle)?.has_pdf_of_size(bytes).map_err(gateway::domain))
                .map_err(|error| error.message)
        }).map_err(gateway::domain)?;
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
        Command::AnalyzeDetachedPdf { id, page, request } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.analyze_detached_pdf(&id, page, request));
            }
            Command::AnalyzeDetachedPdf { id, page, request }
        }
        Command::PrefetchLinguistics { id, sentence_id, revision_id, step, page } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.prefetch_linguistics(&id, &sentence_id, &revision_id, step, page));
            }
            Command::PrefetchLinguistics { id, sentence_id, revision_id, step, page }
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
        empty_pages,
        source_hash,
        deferred_from,
    } = command
    {
        let root = registry::lock()?
            .store(handle)?
            .pdf_original_root()
            .to_owned();
        let mut prepared = reader_core::file_originals::prepare_pdf_import_with_empty_pages(
            &root,
            &name,
            std::path::Path::new(&path),
            &language,
            page_texts,
            lines,
            empty_pages,
        )
        .map_err(gateway::domain)?;
        if let Some(expected) = source_hash {
            prepared.validate_checkpoint_source(&expected).map_err(gateway::domain)?;
        }
        if let Some(first_page) = deferred_from {
            prepared.defer_text_layers_from(first_page).map_err(gateway::domain)?;
        }
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
        Command::LookupDetachedPdfWord { id, page, start_utf16, text } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.lookup_detached_pdf_word(id, *page, *start_utf16, text));
            }
        }
        Command::FindDetachedPdfText { id, query, start, limit, target } => {
            let worker = registry::lock()?.store(handle)?.prepare_store_worker();
            if let Some(worker) = worker {
                return encode_job(worker.find_detached_pdf_text(id, query, *start, *limit, target.as_deref()));
            }
        }
        Command::DetachedPdfTranslation { id, page, sentence_id, target, cached_only, background } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.detached_pdf_translation(id, *page, sentence_id, target, *cached_only, *background));
            }
        }
        Command::DetachedPdfPassage { id, page, sentence_id, target, cached_only, background } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.detached_pdf_passage(id, *page, sentence_id, target, *cached_only, *background));
            }
        }
        Command::PdfPageTranslation { id, page, target, cached_only, background, refresh_at, refresh, source, request_id } => {
            let kind = if *background { ModelRequestKind::BackgroundTranslation } else { ModelRequestKind::PdfPageTranslation };
            let canceled = request_cancellation(handle, *request_id, kind)?;
            let canceled_error = || Error::new("canceled", "Переклад сторінки скасовано.");
            if canceled() { return Err(canceled_error()); }
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                let legacy = refresh_at.map(reader_core::PdfSentenceRefresh::at_offset);
                let result = worker.pdf_page_translation_until_canceled(
                    id, *page, target, *cached_only, *background, refresh.as_ref().or(legacy.as_ref()), source.as_ref(), &canceled,
                );
                return if canceled() { Err(canceled_error()) } else { encode_job(result) };
            }
        }
        Command::TranslateDetachedPdfSentence { id, page, sentence_id, target } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return dispatch::sentence_translation(
                    worker.translate_detached_pdf_sentence(id, *page, sentence_id, target),
                );
            }
        }
        Command::TranslateDictionaryGloss { id, source, gloss, target } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return dispatch::sentence_translation(worker.translate_dictionary_gloss_to(id, source, gloss, target.as_deref().unwrap_or("uk")));
            }
        }
        Command::TranslateDictionaryGlosses { id, source, target, glosses } => {
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.translate_dictionary_glosses(id, source, target, glosses));
            }
        }
        Command::TranslateSelection { id, target, selection, request_id } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::SelectionTranslation)?;
            let canceled_error = || Error::new("canceled", "Переклад виділення скасовано.");
            if canceled() { return Err(canceled_error()); }
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                let result = worker.translate_selection(id, target, selection, &canceled);
                return if canceled() { Err(canceled_error()) } else { dispatch::selected_translation(result) };
            }
        }
        Command::TranslateOfflinePassage { id, page, sentence_id, target, revision_id, source_language, request_id, marian_only } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::OfflinePassage)?;
            let canceled_error = || Error::new("canceled", "Офлайн-переклад уривка скасовано.");
            if canceled() { return Err(canceled_error()); }
            let worker = registry::lock()?.store(handle)?.prepare_translation_worker();
            if let Some(worker) = worker {
                let expected = revision_id.as_deref().zip(source_language.as_deref());
                let result = worker.translate_passage_until_canceled(id, *page, sentence_id, target, expected, *marian_only, &canceled);
                return if canceled() { Err(canceled_error()) } else { encode_job(result) };
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
            ..
        } => {
            let worker = registry::lock()?
                .store(handle)?
                .prepare_translation_worker();
            if let Some(worker) = worker {
                return if let Command::TranslateParagraph { background, .. } = &command {
                    encode_job(worker.translate_paragraph(id, sentence_id, target, *background))
                } else if let Command::TranslateSentence { regenerate, .. } = &command {
                    dispatch::sentence_translation(worker.translate_sentence(
                        id,
                        sentence_id,
                        target,
                        *regenerate,
                    ))
                } else {
                    unreachable!("translation command matched above")
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
            start_offset,
        } => {
            let worker = registry::lock()?
                .store(handle)?
                .prepare_translation_worker();
            if let Some(worker) = worker {
                return encode_job(worker.find_translation_page(id, target, query, *start, *start_offset));
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
        Command::TranslateWithModel { id, page, sentence_id, target, revision_id, source_language, regenerate, cached_only, request_id } => {
            let expected = revision_id.as_deref().zip(source_language.as_deref());
            if *cached_only {
                return encode_job(registry::lock()?.store(handle)?
                    .cached_passage_translation_for_source(id, *page, sentence_id, target, expected));
            }
            let state = registry::lock()?;
            let cancellation = state.model_cancellation(handle, ModelRequestKind::Passage)?;
            let canceled = || request_id.is_some_and(|id| cancellation.is_canceled(id));
            let canceled_error = || Error::new("canceled", "Переклад скасовано.");
            if canceled() {
                return Err(canceled_error());
            }
            let job = state.store(handle)?
                .prepare_passage_translation_for_source(id, *page, sentence_id, target, *regenerate, expected)
                .map_err(gateway::domain)?;
            drop(state);
            let result = if request_id.is_some() { job.run_until_canceled(canceled) } else { job.run() };
            return if canceled() { Err(canceled_error()) } else { encode_job(result) };
        }
        Command::ReviewSentence { model, document, page, sentence, focus, revision_id, cached_only, explain_syntax, request_id } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::SentenceReview)?;
            let canceled_error = || Error::new("canceled", "Перевірку тексту скасовано.");
            if canceled() { return Err(canceled_error()); }
            let job = registry::lock()?
                .store(handle)?
                .prepare_sentence_review(model, document, *page, sentence.as_deref(), focus.as_ref(), revision_id.as_deref(),
                    *cached_only, *explain_syntax)
                .map_err(gateway::domain)?;
            let result = job.run_until_canceled(&canceled);
            return if canceled() { Err(canceled_error()) } else { encode_job(result) };
        }
        Command::ReviewOcrWord { id, page, revision_id, start_utf16, word, cached_only } => {
            let job = registry::lock()?.store(handle)?
                .prepare_ocr_word_review(id, *page, revision_id, *start_utf16, word, *cached_only)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::ReviewOcrLine { id, page, revision_id, index, cached_only } => {
            let job = registry::lock()?.store(handle)?
                .prepare_ocr_line_review(id, *page, revision_id, *index, *cached_only)
                .map_err(gateway::domain)?;
            return encode_job(job.run());
        }
        Command::ReviewOcrPage { id, page, detached, revision_id, cached_only, reviewed_indices, search_cached_page, request_id } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::OcrPageReview)?;
            let canceled_error = || Error::new("canceled", "Звірку OCR скасовано.");
            if canceled() { return Err(canceled_error()); }
            let job = registry::lock()?.store(handle)?
                .prepare_ocr_page_review(id, *page, *detached, revision_id, *cached_only, reviewed_indices, *search_cached_page)
                .map_err(gateway::domain)?;
            let result = job.run_until_canceled(&canceled);
            return if canceled() { Err(canceled_error()) } else { encode_job(result) };
        }
        Command::ExplainWord {
            model, document, sentence, word, start_utf16, revision_id, cached_only, request_id,
        } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::WordExplanation)?;
            if canceled() { return Err(word_explanation_canceled()); }
            let state = registry::lock()?;
            let store = state.store(handle)?;
            let job = if let Some((start, revision)) = start_utf16.zip(revision_id.as_deref()) {
                store.prepare_positioned_word_explanation(
                    model, document, sentence, start, word, revision, *cached_only,
                )
            } else {
                store.prepare_word_explanation(model, document, sentence, word, *cached_only)
            }.map_err(gateway::domain)?;
            drop(state);
            let result = job.run_until_canceled(&canceled);
            return if canceled() { Err(word_explanation_canceled()) } else { encode_job(result) };
        }
        Command::ExplainDetachedWord {
            model, document, page, start_utf16, word, revision_id, cached_only, request_id,
        } => {
            let canceled = request_cancellation(handle, *request_id, ModelRequestKind::WordExplanation)?;
            if canceled() { return Err(word_explanation_canceled()); }
            let job = registry::lock()?
                .store(handle)?
                .prepare_detached_word_explanation(
                    model, document, *page, *start_utf16, word, revision_id.as_deref(), *cached_only,
                )
                .map_err(gateway::domain)?;
            let result = job.run_until_canceled(&canceled);
            return if canceled() { Err(word_explanation_canceled()) } else { encode_job(result) };
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
        Command::RollbackTranslationPackage { source, target } => {
            let files = registry::lock()?.store(handle)?.translation_package_files();
            return encode_job(files.rollback(source, target));
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
        Command::SynthesizeSpeechText { text, language, engine, speaker, rate, request_id, compact_audio } => {
            let canceled = speech_cancellation(handle, *request_id, false)?;
            if canceled() { return Err(speech_canceled()); }
            let engine = reader_core::speech::ModelSpeechEngine::from_id(engine)
                .ok_or_else(|| Error::new("invalid_argument", "Невідомий рушій озвучення."))?;
            let job = registry::lock()?
                .store(handle)?
                .prepare_speech_text(text, language, engine, *speaker, *rate)
                .map_err(gateway::domain)?;
            let checkpoint = request_id.map(|_| &canceled as &dyn Fn() -> bool);
            return encode_playback_audio(&canceled, job.run_cancellable(checkpoint), *compact_audio);
        }
        Command::SynthesizeSpeech {
            id,
            page,
            sentence_id,
            engine,
            speaker,
            rate,
            request_id,
            ..
        }
        | Command::PrefetchSpeech {
            id,
            page,
            sentence_id,
            engine,
            speaker,
            rate,
            request_id,
        } => {
            let engine = reader_core::speech::ModelSpeechEngine::from_id(
                engine.as_deref().unwrap_or("piper-uk"),
            )
            .ok_or_else(|| Error::new("invalid_argument", "Невідомий рушій озвучення."))?;
            let background = matches!(&command, Command::PrefetchSpeech { .. });
            let compact = matches!(&command, Command::SynthesizeSpeech { compact_audio: true, .. });
            let canceled = speech_cancellation(handle, *request_id, background)?;
            if canceled() { return Err(speech_canceled()); }
            let checkpoint = request_id.map(|_| &canceled as &dyn Fn() -> bool);
            if let Some(page) = page {
                let job = registry::lock()?.store(handle)?
                    .prepare_detached_pdf_speech(id, (*page, sentence_id), engine, *speaker, *rate, background)
                    .map_err(gateway::domain)?;
                return if background {
                    encode_speech_job(&canceled, job.map(|job| job.prefetch_cancellable(checkpoint)).transpose())
                } else {
                    encode_playback_audio(&canceled, job.ok_or_else(|| "Речення PDF для озвучення не знайдено.".to_owned())
                        .and_then(|job| job.run_cancellable(checkpoint)), compact)
                };
            }
            if background {
                let job = registry::lock()?
                    .store(handle)?
                    .prepare_speech_prefetch(id, sentence_id, engine, *speaker, *rate)
                    .map_err(gateway::domain)?;
                return encode_speech_job(&canceled, job.map(|job| job.prefetch_cancellable(checkpoint)).transpose());
            }
            let job = registry::lock()?
                .store(handle)?
                .prepare_model_speech(id, sentence_id, engine, *speaker, *rate)
                .map_err(gateway::domain)?;
            return encode_playback_audio(&canceled, job.run_cancellable(checkpoint), compact);
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

fn speech_request_kind(prefetch: bool) -> ModelRequestKind {
    if prefetch { ModelRequestKind::SpeechPrefetch } else { ModelRequestKind::Speech }
}

fn speech_cancellation(handle: u64, request_id: Option<u64>, prefetch: bool) -> Result<impl Fn() -> bool> {
    request_cancellation(handle, request_id, speech_request_kind(prefetch))
}

fn request_cancellation(handle: u64, request_id: Option<u64>, kind: ModelRequestKind) -> Result<impl Fn() -> bool> {
    let request = request_id.map(|id| {
        let cancellation = registry::lock()?.model_cancellation(handle, kind)?;
        Ok::<_, Error>((id, cancellation))
    }).transpose()?;
    Ok(move || request.as_ref().is_some_and(|(id, cancellation)| cancellation.is_canceled(*id)))
}

fn speech_canceled() -> Error {
    Error::new("canceled", "Озвучення скасовано.")
}

fn word_explanation_canceled() -> Error {
    Error::new("canceled", "Пояснення слова скасовано.")
}

fn encode_speech_job<T: serde::Serialize>(
    canceled: &dyn Fn() -> bool, result: reader_core::Result<T>,
) -> Result<serde_json::Value> {
    if canceled() { Err(speech_canceled()) } else { encode_job(result) }
}

fn encode_playback_audio(
    canceled: &dyn Fn() -> bool, result: reader_core::Result<reader_core::tts::SpeechAudio>, compact: bool,
) -> Result<serde_json::Value> {
    if !compact {
        return encode_speech_job(canceled, result);
    }
    if canceled() { return Err(speech_canceled()); }
    let audio = result.map_err(gateway::domain)?;
    Ok(serde_json::json!({
        "sampleRate": audio.sample_rate,
        "samples": audio.samples,
        "playbackRate": audio.playback_rate,
        "wavBytes": base64::encode(&audio.wav_bytes),
    }))
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

type FontMeasure = unsafe extern "C" fn(*mut std::ffi::c_void, u32, u32, f32) -> f32;

/// # Safety
/// Text and callback context must remain valid for the entire call, and the callback must not unwind.
#[no_mangle]
pub unsafe extern "C" fn reader_fit_pdf_text(
    input: *const u8, len: u64, width: f32, height: f32, preferred: f32, minimum: f32,
    context: *mut std::ffi::c_void, measure: Option<FontMeasure>,
) -> ReaderBuffer {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(measure) = measure else {
            return Err(Error::new("invalid_argument", "A font measurement callback is required"));
        };
        if len == 0 || len > 65_536 {
            return Ok(serde_json::Value::Null);
        }
        if input.is_null() {
            return Err(Error::new("invalid_argument", "Missing PDF text buffer"));
        }
        let input = unsafe { std::slice::from_raw_parts(input, len as usize) };
        let text = std::str::from_utf8(input)
            .map_err(|_| Error::new("invalid_argument", "Invalid UTF-8 in PDF text"))?;
        encode_job(Ok(reader_core::pdf_translation_layout::text::fit(
            text, width, height, preferred, minimum,
            |start, length, size| unsafe { measure(context, start, length, size) },
        )))
    })).unwrap_or_else(|_| Err(Error::new("internal_error", "PDF text fitting panicked")));
    catch_unwind(AssertUnwindSafe(|| publish(result).unwrap_or_default())).unwrap_or_default()
}

/// # Safety
/// The input should point to `len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn reader_rotate_ocr_png(input: *const u8, len: u64, angle: u16) -> ReaderBuffer {
    catch_unwind(|| {
        if input.is_null() || len == 0 || len > 64 * 1024 * 1024 {
            return ReaderBuffer::default();
        }
        let bytes = unsafe { std::slice::from_raw_parts(input, len as usize) };
        reader_core::ocr::rotate_ocr_png(bytes, angle).ok()
            .and_then(|png| publish_bytes(png.into_boxed_slice()).ok())
            .unwrap_or_default()
    }).unwrap_or_default()
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
