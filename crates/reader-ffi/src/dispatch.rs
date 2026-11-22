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
        Command::Snapshot => value(store.snapshot()),
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
        Command::PdfReadWindow { id, center } => value(store.pdf_read_window(&id, center)),
        Command::SentenceTokens { id, sentence_id, start, limit } => value(store.sentence_tokens(
            &id, &sentence_id, start,
            limit.unwrap_or(reader_core::document_tokens::DEFAULT_TOKEN_PAGE_SIZE),
        )),
        Command::LookupSentenceToken { id, sentence_id, revision_id, start_utf16 } => {
            value(store.lookup_sentence_token(&id, &sentence_id, &revision_id, start_utf16))
        }
        Command::PdfOcrNeededPages { id, center } => value(store.pdf_ocr_needed_pages(&id, center)),
        Command::ClaimPdfOcrPage { id } => value(store.claim_pdf_ocr_page(&id)),
        Command::RetryPdfOcrPage { id, page, token } => {
            value(store.retry_pdf_ocr_page(&id, page, &token))
        }
        Command::CachePdfOcrPage { id, page, lines, token } => {
            value(store.complete_pdf_ocr_page(&id, page, lines, token.as_deref()))
        }
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
        } => value(store.correct_ocr_line(&id, index, correction)),
        Command::Position { id, position } => value(store.position(&id, position)),
        Command::SetDocumentLanguage { id, language } => {
            value(store.set_document_language(&id, &language))
        }
        Command::DeleteDocument { id } => value(store.delete_document(&id)),
        Command::RenameDocument { id, title } => value(store.rename_document(&id, &title)),
        Command::SpeechResume { id } => value(store.speech_resume(&id)),
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
        | Command::SemanticVocabulary { .. }
        | Command::InstallLexicalPackage { .. }
        | Command::SynthesizeSpeech { .. } => Err(Error::new(
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
        Command::TranslationReadiness { source, target } => {
            value(store.translation_readiness(&source, &target))
        }
        Command::KnowledgePackages => value(Ok(store.knowledge_packages())),
        Command::LexicalPackages => value(Ok(store.lexical_packages())),
        Command::DictionarySources => value(store.dictionary_sources()),
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
        Command::SaveWord { entry } => value(store.save_word(entry)),
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
        Command::NextPracticeQuestion { revision } => value(store.next_practice_question(revision)),
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
