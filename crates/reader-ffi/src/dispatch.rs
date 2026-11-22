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
        Command::InstallOptionalModel { .. } => Err(Error::new(
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
        Command::InstallKnowledgePackage { language, path } => {
            value(store.install_knowledge_package(&language, std::path::Path::new(&path)))
        }
        Command::LookupWord {
            language,
            word,
            context,
        } => value(store.lookup_word(&language, &word, context.as_ref())),
        Command::SuggestOcr { language, text } => value(store.suggest_ocr(&language, &text)),
        Command::InstallTranslationPackage { path } => {
            value(store.install_translation_package(std::path::Path::new(&path)))
        }
        Command::RemoveTranslationPackage { source, target } => {
            value(store.remove_translation_package(&source, &target))
        }
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
        Command::CachedTranslation {
            id,
            sentence_id,
            target,
        } => cached_translation(store.cached_installed_translation(&id, &sentence_id, &target)),
        Command::SaveWord { entry } => value(store.save_word(entry)),
        Command::SetVocabularyVariants {
            id,
            accepted_translations,
        } => value(store.set_vocabulary_variants(&id, accepted_translations)),
        Command::VocabularyReviewHistory { id } => value(store.vocabulary_review_history(&id)),
        Command::Review {
            id,
            direction,
            grade,
        } => value(store.review(&id, &direction, grade)),
        Command::Practice => value(store.practice()),
        Command::SetPractice { progress } => value(store.set_practice(progress)),
        Command::GradePractice { revision, grade } => value(store.grade_practice(revision, grade)),
        Command::Original { id } => value(store.original(&id)),
        Command::Export => value(store.export()),
        Command::ExportBackupFile { path } => {
            value(store.export_backup_file(std::path::Path::new(&path)))
        }
        Command::RestoreBackupFile { path } => {
            value(store.restore_backup_file(std::path::Path::new(&path)))
        }
        Command::Restore { backup } => value(store.restore(backup)),
    }
}
