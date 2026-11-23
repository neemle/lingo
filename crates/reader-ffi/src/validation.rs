use crate::wire::{Command, Error, Request, Result, VERSION};

pub fn decode(input: &[u8]) -> Result<Command> {
    let request: Request = serde_json::from_slice(input).map_err(|_| {
        Error::new(
            "invalid_request",
            "Expected valid UTF-8 and ABI v1 JSON fields",
        )
    })?;
    if request.version != VERSION {
        return Err(Error::new(
            "unsupported_version",
            "Supported ABI version is 1",
        ));
    }
    validate(&request.command)?;
    Ok(request.command)
}

fn validate(command: &Command) -> Result<()> {
    match command {
        Command::CancelModelTranslation { request_id: 0 }
        | Command::CancelBackgroundTranslation { request_id: 0 }
        | Command::AdvanceBackgroundTranslation { request_id: Some(0), .. }
        | Command::CancelPdfPageTranslation { request_id: 0 }
        | Command::PdfPageTranslation { request_id: Some(0), .. }
        | Command::CancelSpeech { request_id: 0, .. }
        | Command::SynthesizeSpeech { request_id: Some(0), .. }
        | Command::SynthesizeSpeechText { request_id: Some(0), .. }
        | Command::PrefetchSpeech { request_id: Some(0), .. }
        | Command::TranslateWithModel { request_id: Some(0), .. } => {
            Err(Error::new("invalid_argument", "Model request ID must be positive"))
        }
        Command::ModelTransfer {
            action: reader_core::model_transfer::TransferAction::AcceptRange { path, .. }, ..
        }
        | Command::ModelTransfer {
            action: reader_core::model_transfer::TransferAction::AdoptPrefix { path, .. }, ..
        } if path.as_os_str().is_empty() || path.as_os_str().len() > 4096 => {
            Err(Error::new("invalid_argument", "Invalid transfer response path"))
        }
        Command::IntakeItem { id }
        | Command::FindIntake { id }
        | Command::IntakePayload { id }
        | Command::IntakeArticle { id }
        | Command::SaveIntakeArticle { id, .. }
        | Command::IntakePhotoOcrLanguage { id }
        | Command::SaveIntakePhotoOcr { id, .. }
        | Command::IntakePdfTextSample { id }
        | Command::SaveIntakePdfText { id, .. }
        | Command::CompleteIntake { id, .. }
        | Command::DiscardIntake { id }
            if id.len() != 36 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') =>
        {
            Err(Error::new("invalid_argument", "Invalid intake identifier"))
        }
        Command::Open { path } if path.is_empty() || path.len() > 4096 || path.contains('\0') => {
            Err(Error::new(
                "invalid_argument",
                "Database path must be nonempty and bounded",
            ))
        }
        Command::ExportBackupFile { path }
        | Command::InspectBackupFile { path }
        | Command::RestoreBackupFile { path }
            if path.is_empty() || path.len() > 4096 || path.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Backup file path must be nonempty and bounded",
            ))
        }
        Command::ExportTranslationPackage {
            source,
            target,
            path,
        } if source.is_empty()
            || source.len() > 32
            || target.is_empty()
            || target.len() > 32
            || path.is_empty()
            || path.len() > 4096
            || path.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Translation package export fields must be nonempty and bounded",
            ))
        }
        Command::ExportOptionalModelArchive { id, path }
        | Command::InstallOptionalModelArchive { id, path }
            if id.is_empty()
                || id.len() > 64
                || path.is_empty()
                || path.len() > 4096
                || path.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Optional model archive fields must be nonempty and bounded",
            ))
        }
        Command::ImportPdfFile {
            path,
            name,
            page_texts,
            lines,
            empty_pages,
            source_hash,
            ..
        } if path.is_empty()
            || path.len() > 4096
            || path.contains('\0')
            || name.is_empty()
            || name.len() > 4096
            || page_texts.is_empty()
            || page_texts.len() > usize::from(u16::MAX)
            || page_texts.iter().map(String::len).sum::<usize>() > reader_core::MAX_IMPORT
            || lines.len() > 8192
            || empty_pages.len() > page_texts.len()
            || source_hash.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) =>
        {
            Err(Error::new(
                "invalid_argument",
                "PDF import path or transcript exceeds its wire bound",
            ))
        }
        Command::Import { bytes, name, .. }
        | Command::DetectDocumentLanguage { bytes, name }
        | Command::ImportImage { bytes, name, .. }
        | Command::ImportScannedPdf { bytes, name, .. }
        | Command::ImportMixedPdf { bytes, name, .. }
        | Command::PdfPagesNeedingOcr { bytes, name, .. }
            if bytes.is_empty()
                || bytes.len() > reader_core::MAX_IMPORT
                || name.is_empty()
                || name.len() > 4096 =>
        {
            Err(Error::new(
                "invalid_argument",
                "Import must contain 1..20MiB and a bounded filename",
            ))
        }
        Command::PrepareUkrainianSpeech { text } if text.len() > 64 * 1024 => Err(Error::new(
            "invalid_argument",
            "Speech input exceeds 64 KiB",
        )),
        Command::DetectLanguage { text } if text.len() > 32_000 => Err(Error::new(
            "invalid_argument",
            "Language sample is too long",
        )),
        Command::FindTranslationPage {
            id, target, query, ..
        } if id.is_empty() || id.len() > 128 || target.len() > 32 || query.len() > 1024 => {
            Err(Error::new(
                "invalid_argument",
                "Translation search fields exceed their wire bounds",
            ))
        }
        Command::SemanticVocabulary { query, .. } if query.len() > 960 => Err(Error::new(
            "invalid_argument",
            "Semantic vocabulary query exceeds its wire bound",
        )),
        Command::SynthesizeSpeechText { text, language, engine, .. }
            if text.len() > 4096 || language.len() > 8 || engine.len() > 32 =>
        {
            Err(Error::new("invalid_argument", "Speech text exceeds its wire bounds"))
        }
        Command::SynthesizeSpeech { id, sentence_id, engine, .. }
        | Command::PrefetchSpeech { id, sentence_id, engine, .. }
            if !valid_token_address(id, sentence_id)
                || engine.as_ref().is_some_and(|engine| engine.len() > 32) =>
        {
            Err(Error::new("invalid_argument", "Invalid speech address"))
        }
        Command::AnalyzeDetachedPdf {
            id, request: reader_core::knowledge::DetachedPageAnalysis::Sentence { sentence_id, .. }, ..
        } if !valid_token_address(id, sentence_id) =>
        {
            Err(Error::new("invalid_argument", "Invalid detached PDF sentence address"))
        }
        Command::AnalyzeDetachedPdf {
            id,
            request: reader_core::knowledge::DetachedPageAnalysis::Expression {
                sentence_id, revision_id, member_starts_utf16,
            },
            ..
        } if !valid_token_address(id, sentence_id)
            || revision_id.len() != 64
            || !revision_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !(2..=8).contains(&member_starts_utf16.len()) =>
        {
            Err(Error::new("invalid_argument", "Invalid detached PDF expression address"))
        }
        Command::SentenceTokens { id, sentence_id, limit, .. }
            if !valid_token_address(id, sentence_id)
                || limit.is_some_and(|limit| !(1..=reader_core::document_tokens::MAX_TOKEN_PAGE_SIZE).contains(&limit)) =>
        {
            Err(Error::new("invalid_argument", "Invalid sentence token page"))
        }
        Command::LookupSentenceToken { id, sentence_id, revision_id, .. }
        | Command::SetReadingCursor { id, sentence_id, revision_id, .. }
        | Command::ScheduleTranslationWindow { id, sentence_id, revision_id, .. }
        | Command::AnalyzeLinguistics { id, sentence_id, revision_id, .. }
        | Command::PrefetchLinguistics { id, sentence_id, revision_id, .. }
            if !valid_token_address(id, sentence_id)
                || revision_id.len() != 64
                || !revision_id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Err(Error::new("invalid_argument", "Invalid sentence token address"))
        }
        Command::AdvanceBackgroundTranslation { id, revision: revision_id, .. }
        | Command::WarmTranslationStep { id, revision: revision_id, .. }
        | Command::OpenPdfReadingPage { id, revision_id: Some(revision_id), .. }
        | Command::ReplacePdfTextLayer { id, revision_id, .. }
        | Command::ReconcilePdfOcrPage { id, revision_id, .. }
        | Command::RepairStoredPdfTextOrder { id, revision_id, .. }
        | Command::ReconcilePhotoOcr { id, revision_id }
        | Command::AnchorReadingSelection { id, revision_id, .. }
            if !valid_token_address(id, revision_id)
                || revision_id.len() != 64
                || !revision_id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Err(Error::new("invalid_argument", "Invalid reading address"))
        }
        Command::ReviewSentence { model, document, sentence, focus, revision_id, .. }
            if model.len() > 32 || !valid_token_address(document, sentence.as_deref().unwrap_or("selection"))
                || revision_id.as_ref().is_some_and(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                || (sentence.is_none() && !matches!(focus, Some(reader_core::qwen::ReviewFocus::Selection { .. }))) =>
        {
            Err(Error::new("invalid_argument", "Invalid local sentence review request"))
        }
        Command::ReviewOcrWord { id, revision_id, word, .. }
            if !valid_token_address(id, revision_id) || word.len() > 128
                || revision_id.len() != 64 || !revision_id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Err(Error::new("invalid_argument", "Invalid OCR word review address"))
        }
        Command::ExplainWord { model, document, sentence, word, start_utf16, revision_id, .. }
            if model.len() > 32 || document.len() > 128 || sentence.len() > 128 || word.len() > 320
                || start_utf16.is_some() != revision_id.is_some()
                || revision_id.as_ref().is_some_and(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())) =>
        {
            Err(Error::new("invalid_argument", "Invalid word explanation address"))
        }
        Command::ExplainDetachedWord { model, document, word, revision_id, .. }
            if model.len() > 32 || !valid_token_address(document, "selection") || word.len() > 320
                || revision_id.as_ref().is_some_and(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())) =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF word explanation request"))
        }
        Command::CachePdfOcrPage { lines, .. } | Command::ReplacePdfTextLayer { lines, .. } if lines.len() > 8192 => {
            Err(Error::new("invalid_argument", "Too many OCR lines for one PDF page"))
        }
        Command::RetryPdfOcrPage { token, .. }
        | Command::CachePdfOcrPage { token: Some(token), .. }
            if token.len() != 36 =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF OCR request token"))
        }
        Command::DetachedPdfSentence { id, sentence_id: Some(sentence_id), .. }
            if !valid_token_address(id, sentence_id) =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF sentence address"))
        }
        Command::SetDetachedPdfSpeechResume { id, sentence_id, .. }
        | Command::SpeechNarration { id, sentence_id, .. } if !valid_token_address(id, sentence_id) =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF speech checkpoint address"))
        }
        Command::UpdateDetachedPdfAnnotation { id, sentence_id, .. } if !valid_token_address(id, sentence_id) =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF annotation address"))
        }
        Command::TranslateDetachedPdfSentence { id, sentence_id, target, .. }
        | Command::TranslateWithModel { id, sentence_id, target, .. }
        | Command::TranslateOfflinePassage { id, sentence_id, target, .. }
        | Command::DetachedPdfPassage { id, sentence_id, target, .. }
        | Command::DetachedPdfTranslation { id, sentence_id, target, .. }
            if !valid_token_address(id, sentence_id) || target.len() > 8 =>
        {
            Err(Error::new("invalid_argument", "Invalid sentence translation address"))
        }
        Command::CorrectDetachedPdfOcr { revision, .. } | Command::FocusDetachedPdfOcrWord { revision, .. }
            if revision.len() != 64 || !revision.bytes().all(|ch| ch.is_ascii_hexdigit()) =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF OCR revision"))
        }
        Command::DeleteDocument { id }
        | Command::ReadingCursor { id }
        | Command::PendingTranslationWindow { id }
        | Command::RunTranslationWindow { id, .. }
        | Command::CancelTranslationWindow { id }
        | Command::PdfReadingPage { id }
        | Command::SetPdfReadingPage { id, .. }
        | Command::OpenPdfReadingPage { id, .. }
        | Command::PdfReadWindow { id, .. }
        | Command::DetachedPdfSentence { id, .. }
        | Command::FindDetachedPdfText { id, .. }
        | Command::LookupDetachedPdfWord { id, .. }
        | Command::TranslateDictionaryGloss { id, .. }
        | Command::TranslateDictionaryGlosses { id, .. }
        | Command::TranslateSelection { id, .. }
        | Command::TranslateWithModel { id, .. }
        | Command::TranslateOfflinePassage { id, .. }
        | Command::SaveDetachedPdfWord { id, .. }
        | Command::DetachedPdfOcr { id, .. }
        | Command::CorrectDetachedPdfOcr { id, .. }
        | Command::FocusDetachedPdfOcrWord { id, .. }
        | Command::DetachedPdfAnnotations { id, .. }
        | Command::AnalyzeDetachedPdf { id, .. }
        | Command::PdfOcrNeededPages { id, .. }
        | Command::ClaimPdfOcrPage { id, .. }
        | Command::RetryPdfOcrPage { id, .. }
        | Command::CachePdfOcrPage { id, .. }
        | Command::CacheBackgroundPdfTextLayer { id, .. }
            if id.is_empty() || id.len() > 256 || id.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Document ID must be nonempty and bounded",
            ))
        }
        Command::DeleteVocabularyCard { id }
            if id.is_empty() || id.len() > 256 || id.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Vocabulary card ID must be nonempty and bounded",
            ))
        }
        Command::Review {
            direction, grade, ..
        } if !["forward", "reverse"].contains(&direction.as_str()) || *grade > 3 => Err(
            Error::new("invalid_argument", "Review direction or grade is invalid"),
        ),
        _ => Ok(()),
    }
}

fn valid_token_address(id: &str, sentence_id: &str) -> bool {
    [id, sentence_id].into_iter().all(|value| {
        !value.is_empty() && value.len() <= 256 && !value.contains('\0')
    })
}
