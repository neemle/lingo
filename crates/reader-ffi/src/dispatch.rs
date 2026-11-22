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

pub fn execute(store: &mut Store, command: Command) -> Result<Value> {
    match command {
        Command::Open { .. } => Err(Error::new("invalid_handle", "Open requires handle zero")),
        Command::Snapshot => value(store.snapshot()),
        Command::Import {
            name,
            bytes,
            language,
        } => value(store.import(&name, &bytes, &language)),
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
        } => value(store.set_speech_resume(&id, &sentence_id, &text_hash, offset_utf16)),
        Command::SetBookmark {
            id,
            sentence_id,
            saved,
        } => value(store.set_bookmark(&id, &sentence_id, saved)),
        Command::SetReviewedTranslation {
            id,
            sentence_id,
            target,
            text,
        } => value(store.set_reviewed_translation(&id, &sentence_id, &target, &text)),
        Command::TranslationPackages => value(Ok(store.translation_packages())),
        Command::TranslationReadiness { source, target } => {
            value(store.translation_readiness(&source, &target))
        }
        Command::KnowledgePackages => value(Ok(store.knowledge_packages())),
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
        Command::TranslateSentence {
            id,
            sentence_id,
            target,
        } => value(
            store
                .translate_installed_sentence(&id, &sentence_id, &target)
                .map(|translated| {
                    translated.map(|result| {
                        serde_json::json!({
                            "text": result.segments[0],
                            "pivotLanguage": result.route.pivot_language(),
                            "joinedLines": result.joined_lines,
                        })
                    })
                }),
        ),
        Command::SaveWord { entry } => value(store.save_word(entry)),
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
        Command::Restore { backup } => value(store.restore(backup)),
    }
}
