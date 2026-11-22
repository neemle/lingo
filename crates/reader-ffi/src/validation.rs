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
        Command::IntakeItem { id }
        | Command::IntakePayload { id }
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
            ..
        } if path.is_empty()
            || path.len() > 4096
            || path.contains('\0')
            || name.is_empty()
            || name.len() > 4096
            || page_texts.is_empty()
            || page_texts.len() > usize::from(u16::MAX)
            || page_texts.iter().map(String::len).sum::<usize>() > reader_core::MAX_IMPORT
            || lines.len() > 8192 =>
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
            if !valid_token_address(id, sentence_id)
                || revision_id.len() != 64
                || !revision_id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Err(Error::new("invalid_argument", "Invalid sentence token address"))
        }
        Command::ExplainWord {
            model,
            document,
            sentence,
            word,
        } if model.len() > 32
            || document.len() > 128
            || sentence.len() > 128
            || word.len() > 320 =>
        {
            Err(Error::new(
                "invalid_argument",
                "Word explanation fields exceed their wire bounds",
            ))
        }
        Command::CachePdfOcrPage { lines, .. } if lines.len() > 8192 => {
            Err(Error::new("invalid_argument", "Too many OCR lines for one PDF page"))
        }
        Command::RetryPdfOcrPage { token, .. }
        | Command::CachePdfOcrPage { token: Some(token), .. }
            if token.len() != 36 =>
        {
            Err(Error::new("invalid_argument", "Invalid PDF OCR request token"))
        }
        Command::DeleteDocument { id }
        | Command::ReadingCursor { id }
        | Command::PendingTranslationWindow { id }
        | Command::RunTranslationWindow { id }
        | Command::CancelTranslationWindow { id }
        | Command::PdfReadingPage { id }
        | Command::SetPdfReadingPage { id, .. }
        | Command::PdfReadWindow { id, .. }
        | Command::PdfOcrNeededPages { id, .. }
        | Command::ClaimPdfOcrPage { id }
        | Command::RetryPdfOcrPage { id, .. }
        | Command::CachePdfOcrPage { id, .. }
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
