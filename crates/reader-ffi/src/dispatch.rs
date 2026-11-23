use crate::{
    gateway::{self, Store},
    wire::{Command, Error, Result},
};
use serde::Serialize;
use serde_json::Value;

fn value<T: Serialize>(result: std::result::Result<T, String>) -> Result<Value> {
    serde_json::to_value(result.map_err(gateway::domain)?)
        .map_err(|_| Error::new("serialization_error", "Cannot encode core response"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LibrarySnapshot {
    #[serde(flatten)]
    snapshot: reader_core::Snapshot,
    pdf_reading_positions: std::collections::BTreeMap<String, u16>,
}

fn sentence_translation_body(result: reader_core::translation::AlignedTranslation) -> Value {
    serde_json::json!({
        "text": result.segments[0],
        "pivotLanguage": result.route.pivot_language(),
        "joinedLines": result.joined_lines,
        "alignment": result.alignment,
    })
}

pub fn sentence_translation(
    result: reader_core::Result<Option<reader_core::translation::AlignedTranslation>>,
) -> Result<Value> {
    value(result.map(|translated| translated.map(sentence_translation_body)))
}

pub fn selected_translation(
    result: reader_core::Result<Option<reader_core::translation::SelectedTranslation>>,
) -> Result<Value> {
    use reader_core::translation::SelectedTranslation;
    value(result.map(|selected| selected.map(|selected| match selected {
        SelectedTranslation::Model { translation, source_text } => {
            let mut response = sentence_translation_body(translation);
            response["sourceText"] = Value::String(source_text);
            response
        }
        SelectedTranslation::Cardinal { text, source_text } =>
            serde_json::json!({ "text": text, "sourceText": source_text, "joinedLines": false }),
    })))
}

pub fn cached_translation(
    result: reader_core::Result<reader_core::translation::CachedTranslation>,
) -> Result<Value> {
    value(result.map(|cached| {
        serde_json::json!({
            "paragraph": cached.paragraph,
            "sentence": cached.sentence.map(sentence_translation_body),
        })
    }))
}

pub fn execute(store: &mut Store, command: Command) -> Result<Value> {
    match command {
        Command::Open { .. } => Err(Error::new("invalid_handle", "Open requires handle zero")),
        Command::SamplePdfBackgrounds { .. } | Command::SeparatePdfBlocks { .. } | Command::ParseOcrTsv { .. } =>
            unreachable!("Stateless command handled before store dispatch"),
        Command::Snapshot => {
            let snapshot = store.snapshot().map_err(gateway::domain)?;
            let pdf_reading_positions = store.pdf_reading_positions().map_err(gateway::domain)?;
            value(Ok(LibrarySnapshot { snapshot, pdf_reading_positions }))
        }
        Command::StorageUsage => value(store.storage_usage()),
        Command::ClearStorage { scope } => value(store.clear_storage(scope)),
        Command::AdvanceBackgroundTranslation { id, revision, target, .. } =>
            value(store.advance_background_translation(&id, &revision, &target)),
        Command::WarmTranslationStep { id, revision, target, start } =>
            value(store.warm_translation_step(&id, &revision, &target, start)),
        Command::StageIntake { .. } => Err(Error::new("invalid_state", "Intake needs staging")),
        Command::ListIntake { start } => value(store.list_intake(start)),
        Command::IntakeItem { id } => value(store.intake_item(&id)),
        Command::FindIntake { id } => value(store.intake_item_for_request(&id)),
        Command::IntakePayload { id } => value(store.intake_payload(&id)),
        Command::IntakeArticle { id } => value(store.intake_article(&id)),
        Command::SaveIntakeArticle { id, draft } => value(store.save_intake_article(&id, draft)),
        Command::IntakePhotoOcrLanguage { id } => value(store.intake_photo_ocr_language(&id)),
        Command::SaveIntakePhotoOcr { id, language, lines } =>
            value(store.save_intake_photo_ocr(&id, &language, &lines)),
        Command::IntakePdfTextSample { id } => value(store.intake_pdf_text_sample(&id)),
        Command::SaveIntakePdfText { id, draft } => value(store.save_intake_pdf_text(&id, draft)),
        Command::CompleteIntake { id, result } => value(store.complete_intake(&id, result)),
        Command::DiscardIntake { id } => value(store.discard_intake(&id)),
        Command::ReaderPreferences => value(store.reader_preferences()),
        Command::MergeReaderPreferences { preferences } => value(store.merge_reader_preferences(preferences)),
        Command::UpdateReaderPreference { change } => value(store.update_reader_preference(change)),
        Command::RecentReadingSessions => value(store.recent_reading_sessions()),
        Command::CheckpointReading { session } => value(store.checkpoint_reading(session)),
        Command::PrepareUkrainianSpeech { text } => {
            value(Ok(reader_core::tts::ukrainian_stress(&text)))
        }
        Command::DetectLanguage { text } => {
            value(Ok(reader_core::language_detection::detect(&text)))
        }
        Command::DetectDocumentLanguage { name, bytes } => value(Ok(
            reader_core::language_detection::detect_document(&name, &bytes),
        )),
        Command::Import {
            name,
            bytes,
            language,
            source_url,
        } => value(match source_url {
            Some(url) => store.import_article(&name, &bytes, &language, &url),
            None => store.import(&name, &bytes, &language),
        }),
        Command::ImportPdfFile { .. } => Err(Error::new(
            "invalid_state",
            "PDF import requires staging outside the bridge lock",
        )),
        Command::ImportImage {
            name,
            bytes,
            language,
            lines,
        } => value(store.import_image(&name, &bytes, &language, lines)),
        Command::PdfPagesNeedingOcr { name, bytes, pages } => value(
            reader_core::pdf_layers::text_pages(&name, &bytes, pages).map(|pages| {
                pages
                    .iter()
                    .enumerate()
                    .filter_map(|(index, page)| page.is_empty().then_some(index))
                    .collect::<Vec<_>>()
            }),
        ),
        Command::PdfReadingPage { id } => value(store.pdf_reading_page(&id)),
        Command::SetPdfReadingPage { id, page } => value(store.set_pdf_reading_page(&id, page)),
        Command::OpenPdfReadingPage { id, revision_id, page } => {
            match revision_id {
                Some(revision) => value(store.open_pdf_reading_page(&id, &revision, page)),
                None => value(store.focus_pdf_reading_page(&id, None, page)),
            }
        }
        Command::PdfReadWindow { id, center } => value(store.pdf_read_window(&id, center)),
        Command::DetachedPdfSentence { id, page, position, sentence_id, source_offset } =>
            value(store.select_detached_pdf_sentence_at(&id, page, position, sentence_id.as_deref(), source_offset)),
        Command::UnreadPdfTextPages { id } => value(store.unread_pdf_text_pages(&id)),
        Command::FindDetachedPdfText { id, query, start, limit, target } =>
            value(store.find_detached_pdf_text(&id, &query, start, limit, target.as_deref())),
        Command::SetDetachedPdfSpeechResume { id, page, sentence_id, resume } =>
            value(store.set_detached_pdf_speech_resume(&id, page, &sentence_id, resume)),
        Command::DetachedPdfOcr { id, page } => value(store.detached_pdf_ocr(&id, page)),
        Command::FocusDetachedPdfOcrWord { id, page, revision, start_utf16, word } =>
            value(store.focus_detached_pdf_ocr_word(&id, page, &revision, start_utf16, &word)),
        Command::CorrectDetachedPdfOcr { id, page, revision, index, correction } =>
            value(store.correct_detached_pdf_ocr(&id, page, &revision, index, correction)),
        Command::DetachedPdfAnnotations { id, page } => value(store.detached_pdf_annotations(&id, page)),
        Command::UpdateDetachedPdfAnnotation { id, page, sentence_id, change } =>
            value(store.update_detached_pdf_annotation(&id, page, &sentence_id, change)),
        Command::DetachedPdfPassage { id, page, sentence_id, target, cached_only, background } =>
            value(store.detached_pdf_passage(&id, page, &sentence_id, &target, cached_only, background)),
        Command::PdfPageTranslation { id, page, target, cached_only, background, .. } =>
            value(store.pdf_page_translation_with_priority(&id, page, &target, cached_only, background)),
        Command::DetachedPdfTranslation { id, page, sentence_id, target, cached_only, background } =>
            value(store.detached_pdf_translation(&id, page, &sentence_id, &target, cached_only, background)),
        Command::TranslateDetachedPdfSentence { id, page, sentence_id, target } =>
            sentence_translation(store.translate_detached_pdf_sentence(&id, page, &sentence_id, &target)),
        Command::SentenceTokens { id, sentence_id, start, limit } => value(store.sentence_tokens(
            &id, &sentence_id, start,
            limit.unwrap_or(reader_core::document_tokens::DEFAULT_TOKEN_PAGE_SIZE),
        )),
        Command::LookupSentenceToken { id, sentence_id, revision_id, start_utf16 } => {
            value(store.lookup_sentence_token(&id, &sentence_id, &revision_id, start_utf16))
        }
        Command::AnalyzeLinguistics { id, sentence_id, revision_id, scope } => {
            value(store.cached_linguistic_analysis(&id, &sentence_id, &revision_id, scope))
        }
        Command::AnalyzeDetachedPdf { id, page, request } =>
            value(store.cached_detached_pdf_analysis(&id, page, request)),
        Command::PrefetchLinguistics { id, sentence_id, revision_id, step, page } => {
            value(store.prefetch_linguistics(&id, &sentence_id, &revision_id, step, page))
        }
        Command::PdfOcrNeededPages { id, center } => value(store.pdf_ocr_needed_pages(&id, center)),
        Command::ClaimPdfOcrPage { id, center, retry_now } =>
            value(store.claim_pdf_ocr_page_with_retry(&id, center, retry_now)),
        Command::CacheBackgroundPdfTextLayer { id, revision, page, text } =>
            value(store.cache_background_pdf_text_layer(&id, &revision, page, &text)),
        Command::RetryPdfOcrPage { id, page, token, canceled } => {
            if canceled {
                value(store.cancel_pdf_ocr_page(&id, page, &token))
            } else {
                value(store.retry_pdf_ocr_page(&id, page, &token))
            }
        }
        Command::CachePdfOcrPage { id, page, lines, token, text_layer } => {
            value(store.complete_pdf_page(&id, page, lines, token.as_deref(), text_layer))
        }
        Command::ReplacePdfTextLayer { id, page, revision_id, language, lines } =>
            value(store.replace_pdf_text_layer(&id, page, &revision_id, &language, lines)),
        Command::ReconcilePdfOcrPage { id, page, revision_id, language, reference, detached } =>
            value(store.reconcile_pdf_ocr_page(&id, page, &revision_id, &language, &reference, detached)),
        Command::RepairStoredPdfTextOrder { id, page, revision_id, reference, changes } =>
            value(store.repair_stored_pdf_text_order(&id, page, &revision_id, &reference, &changes)),
        Command::UpgradeStoredOcrLayout { id, page, revision_id, reference, detached } =>
            value(store.upgrade_stored_ocr_layout(&id, page, &revision_id, &reference, detached)),
        Command::ReconcilePhotoOcr { id, revision_id } => value(store.reconcile_photo_ocr(&id, &revision_id)),
        Command::PdfOcrJob { job } => value(store.pdf_ocr_job(job)),
        Command::ImportMixedPdf {
            name,
            bytes,
            language,
            lines,
            pages,
        } => value(store.import_mixed_pdf(&name, &bytes, &language, lines, pages)),
        Command::ImportScannedPdf {
            name,
            bytes,
            language,
            lines,
            pages,
        } => value(store.import_scanned_pdf(&name, &bytes, &language, lines, pages)),
        Command::CorrectOcrLine {
            id,
            index,
            correction,
            revision,
        } => value(store.correct_ocr_line_at_revision(&id, index, correction, revision.as_deref())),
        Command::ApplyOcrPageReview { id, page, detached, revision_id, edits } =>
            value(store.apply_ocr_page_review(&id, page, detached, &revision_id, edits)),
        Command::Position { id, position } => value(store.position(&id, position)),
        Command::ReadingCursor { id } => value(store.reading_cursor(&id)),
        Command::SetReadingCursor { id, sentence_id, revision_id, start_utf16, page } => {
            value(store.set_reading_cursor_for_page(&id, &sentence_id, &revision_id, start_utf16, page))
        }
        Command::AnchorReadingSelection { id, revision_id, start_utf16, length_utf16 } => {
            value(store.anchor_reading_selection(&id, &revision_id, start_utf16, length_utf16))
        }
        Command::SetDocumentLanguage { id, language } => {
            value(store.set_document_language(&id, &language))
        }
        Command::DeleteDocument { id } => value(store.delete_document(&id)),
        Command::RenameDocument { id, title } => value(store.rename_document(&id, &title)),
        Command::SpeechResume { id } => value(store.speech_resume(&id)),
        Command::SpeechNarration { id, sentence_id, pronunciation_only } =>
            value(if pronunciation_only { store.speech_pronunciation(&id, &sentence_id) }
                else { store.speech_narration(&id, &sentence_id) }),
        Command::SetSpeechResume {
            id,
            sentence_id,
            text_hash,
            offset_utf16,
            model,
        } => match model {
            Some(model) => {
                value(store.set_model_speech_resume(&id, &sentence_id, &text_hash, model))
            }
            None => value(store.set_speech_resume(&id, &sentence_id, &text_hash, offset_utf16)),
        },
        Command::SetSpeechText {
            id,
            sentence_id,
            text,
        } => value(store.set_speech_text(&id, &sentence_id, &text)),
        Command::SetBookmark {
            id,
            sentence_id,
            saved,
        } => value(store.set_bookmark(&id, &sentence_id, saved)),
        Command::SetSentenceNote {
            id,
            sentence_id,
            note,
        } => value(store.set_sentence_note(&id, &sentence_id, &note)),
        Command::SetReviewedTranslation {
            id,
            sentence_id,
            target,
            text,
        } => value(store.set_reviewed_translation(&id, &sentence_id, &target, &text)),
        Command::ExplainWord { .. }
        | Command::ExplainDetachedWord { .. }
        | Command::CancelWordExplanation { .. }
        | Command::ReviewSentence { .. }
        | Command::ReviewOcrWord { .. }
        | Command::ReviewOcrLine { .. }
        | Command::ReviewOcrPage { .. }
        | Command::CancelOcrPageReview { .. }
        | Command::TranslateWithModel { .. }
        | Command::CancelModelTranslation { .. }
        | Command::CancelPdfPageTranslation { .. }
        | Command::CancelBackgroundTranslation { .. }
        | Command::CancelSpeech { .. }
        | Command::SemanticVocabulary { .. }
        | Command::InstallLexicalPackage { .. }
        | Command::ModelTransfer { .. }
        | Command::SynthesizeSpeechText { .. }
        | Command::SynthesizeSpeech { .. }
        | Command::PrefetchSpeech { .. } => Err(Error::new(
            "internal_error",
            "Optional inference requires unlocked dispatch",
        )),
        Command::OptionalModels => value(Ok(store.optional_models())),
        Command::CheckOptionalModelSpace { id, staging_root } => {
            value(store.check_optional_model_space(&id, std::path::Path::new(&staging_root)))
        }
        Command::InstallOptionalModel { .. }
        | Command::ExportOptionalModelArchive { .. }
        | Command::InstallOptionalModelArchive { .. } => Err(Error::new(
            "internal_error",
            "Optional model installation requires unlocked dispatch",
        )),
        Command::RollbackOptionalModel { id } => value(store.rollback_optional_model(&id)),
        Command::VerifyOptionalModel { id } => value(store.verify_optional_model(&id)),
        Command::RemoveOptionalModel { id } => value(store.remove_optional_model(&id)),
        Command::TranslationPackages => value(Ok(store.translation_packages())),
        Command::TranslationRoutes => value(store.translation_routes()),
        Command::TranslationReadiness { source, target } => {
            value(store.translation_readiness(&source, &target))
        }
        Command::KnowledgePackages => value(Ok(store.knowledge_packages())),
        Command::LexicalPackages => value(Ok(store.lexical_packages())),
        Command::DictionarySources => value(store.dictionary_sources()),
        Command::DictionarySourceLocations { card_id } => value(store.dictionary_source_locations(&card_id)),
        Command::DictionaryCollection { document_id } => {
            value(store.dictionary_collection(document_id.as_deref()))
        }
        Command::ExportDictionary { document_id } => {
            value(store.export_dictionary(document_id.as_deref()))
        }
        Command::ImportDictionary { archive } => value(store.import_dictionary(archive)),
        Command::InstallKnowledgePackage { .. } => Err(Error::new(
            "internal_error",
            "Knowledge package installation must run outside the store lock",
        )),
        Command::LookupWord {
            language,
            word,
            context,
        } => value(store.lookup_word(&language, &word, context.as_ref())),
        Command::SuggestOcr { language, text } => value(store.suggest_ocr(&language, &text)),
        Command::InstallTranslationPackage { .. }
        | Command::ExportTranslationPackage { .. }
        | Command::RemoveTranslationPackage { .. }
        | Command::RollbackTranslationPackage { .. }
        | Command::VerifyTranslationPackage { .. }
        | Command::ValidateTranslationPackage { .. } => Err(Error::new(
            "internal_error",
            "Translation package maintenance must run outside the store lock",
        )),
        Command::CheckTranslationDownloadSpace {
            source,
            target,
            staging_root,
            archive_directory,
        } => value(store.check_translation_download_space(
            &source,
            &target,
            std::path::Path::new(&staging_root),
            std::path::Path::new(&archive_directory),
        )),
        Command::CheckTranslationArchiveSpace { directory, bytes } => {
            value(store.check_translation_archive_space(std::path::Path::new(&directory), bytes))
        }
        Command::TranslateParagraph {
            id,
            sentence_id,
            target,
            background,
        } => {
            if background {
                return Err(gateway::domain(
                    "Фоновий переклад потребує файлової бібліотеки.".into(),
                ));
            }
            value(store.translate_installed_paragraph(&id, &sentence_id, &target))
        }
        Command::TranslateSentence {
            id,
            sentence_id,
            target,
        } => sentence_translation(store.translate_installed_sentence(&id, &sentence_id, &target)),
        Command::TranslateDictionaryGloss { id, source, gloss, target } =>
            sentence_translation(store.translate_dictionary_gloss_to(&id, &source, &gloss, target.as_deref().unwrap_or("uk"))),
        Command::TranslateDictionaryGlosses { id, source, target, glosses } =>
            value(store.translate_dictionary_glosses(&id, &source, &target, &glosses)),
        Command::TranslateSelection { id, target, selection } =>
            selected_translation(store.translate_selected_text(&id, &target, &selection)),
        Command::TranslateOfflinePassage { id, page, sentence_id, target } =>
            value(store.translate_offline_passage(&id, page, &sentence_id, &target)),
        Command::PassageSource { id, page, sentence_id } =>
            value(store.passage_source(&id, page, &sentence_id)),
        Command::ScheduleTranslationWindow { id, sentence_id, revision_id, target } => {
            value(store.schedule_translation_window(&id, &sentence_id, &revision_id, &target))
        }
        Command::PendingTranslationWindow { id } => value(store.pending_translation_window(&id)),
        Command::CancelTranslationWindow { id } => value(store.cancel_translation_window(&id)),
        Command::RunTranslationWindow { .. } => Err(gateway::domain(
            "Фоновий переклад потребує файлової бібліотеки.".into(),
        )),
        Command::FindTranslationPage {
            id,
            target,
            query,
            start,
        } => value(store.find_translation_page(&id, &target, &query, start)),
        Command::CachedTranslation {
            id,
            sentence_id,
            target,
        } => cached_translation(store.cached_installed_translation(&id, &sentence_id, &target)),
        Command::LookupDetachedPdfWord { id, page, start_utf16, text } =>
            value(store.lookup_detached_pdf_word(&id, page, start_utf16, &text)),
        Command::SaveDetachedPdfWord { id, page, input } =>
            value(store.save_detached_pdf_word(&id, page, input)),
        Command::SaveWord { entry, selection } => value(match selection {
            Some(selection) => store.save_selected_word(entry, selection),
            None => store.save_word(entry),
        }),
        Command::DeleteVocabularyCard { id } => value(store.delete_vocabulary_card(&id)),
        Command::SetVocabularyVariants {
            id,
            translation,
            accepted_translations,
        } => value(store.edit_vocabulary_translation(
            &id,
            translation.as_deref(),
            accepted_translations,
        )),
        Command::VocabularyReviewHistory { id } => value(store.vocabulary_review_history(&id)),
        Command::Review {
            id,
            direction,
            grade,
        } => value(store.review(&id, &direction, grade)),
        Command::Practice => value(store.practice()),
        Command::NextPracticeQuestion { revision, settings } => value(store.practice_queue(revision, settings)),
        Command::AnswerPracticeQuestion { revision, answer, elapsed_ms } => {
            value(store.answer_practice_question(revision, answer.as_deref(), elapsed_ms))
        }
        Command::SetPractice { progress } => value(store.set_practice(progress)),
        Command::GradePractice { revision, grade } => value(store.grade_practice(revision, grade)),
        Command::Original { id } => value(store.original(&id)),
        Command::OriginalFile { id } => value(store.original_file(&id)),
        Command::Export => value(store.export()),
        Command::ExportBackupFile { path } => {
            value(store.export_backup_file(std::path::Path::new(&path)))
        }
        Command::InspectBackupFile { path } => value(reader_core::inspect_backup_file(
            std::path::Path::new(&path),
        )),
        Command::RestoreBackupFile { path } => {
            value(store.restore_backup_file(std::path::Path::new(&path)))
        }
        Command::Restore { backup } => value(store.restore(backup)),
    }
}
