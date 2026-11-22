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
        Command::Open { path } if path.is_empty() || path.len() > 4096 || path.contains('\0') => {
            Err(Error::new(
                "invalid_argument",
                "Database path must be nonempty and bounded",
            ))
        }
        Command::ExportBackupFile { path } | Command::RestoreBackupFile { path }
            if path.is_empty() || path.len() > 4096 || path.contains('\0') =>
        {
            Err(Error::new(
                "invalid_argument",
                "Backup file path must be nonempty and bounded",
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
        Command::Review {
            direction, grade, ..
        } if !["forward", "reverse"].contains(&direction.as_str()) || *grade > 3 => Err(
            Error::new("invalid_argument", "Review direction or grade is invalid"),
        ),
        _ => Ok(()),
    }
}
