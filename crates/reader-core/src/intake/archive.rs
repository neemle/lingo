use super::{
    application::{
        classify_file, safe_name, validate_article_draft, validate_article_url, validate_text,
    },
    persistence::{publish_pdf, validate_pdf_draft},
    ArticleDraft, IntakeKind, Metadata, PdfTextDraft,
};
use crate::{backup_files::archive_error, file_originals, ocr, Result, Store, MAX_IMPORT};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

// A length-prefixed JSON checkpoint followed by the original bytes. PDFs are streamed.
const MAX_HEADER_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Header {
    metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<Checkpoint>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Checkpoint {
    Article(ArticleDraft),
    Photo {
        language: String,
        lines: Vec<ocr::OcrLine>,
    },
    Pdf(PdfTextDraft),
}

fn invalid() -> String {
    "Некоректний вхідний матеріал у резервній копії.".into()
}

pub(crate) fn entry_name(id: &str) -> String {
    format!("intake/{id}")
}

pub(crate) fn validate_ids(ids: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    if ids.iter().any(|id| {
        !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == *id) || !seen.insert(id)
    }) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn pending_ids(db: &Connection, max: usize) -> Result<Vec<String>> {
    let mut query = db
        .prepare("SELECT id FROM intake WHERE document_id IS NULL ORDER BY rowid LIMIT ?1")
        .map_err(archive_error)?;
    let ids = query
        .query_map([(max + 1) as i64], |row| row.get(0))
        .map_err(archive_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(archive_error)?;
    if ids.len() > max {
        return Err("У резервній копії забагато записів.".into());
    }
    Ok(ids)
}

pub(crate) fn write(
    store: &Store,
    db: &Connection,
    id: &str,
    mut output: impl Write,
) -> Result<()> {
    let (metadata, mut payload): (String, Option<Vec<u8>>) = db
        .query_row(
            "SELECT metadata,payload FROM intake WHERE id=?1 AND document_id IS NULL",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(archive_error)?;
    let metadata: Metadata = serde_json::from_str(&metadata).map_err(archive_error)?;
    let checkpoint = match metadata.kind {
        IntakeKind::Link => payload
            .take()
            .map(|bytes| {
                serde_json::from_slice(&bytes)
                    .map(Checkpoint::Article)
                    .map_err(archive_error)
            })
            .transpose()?,
        IntakeKind::Photo => {
            let saved: Option<(String, Vec<u8>)> = db
                .query_row(
                    "SELECT language,lines FROM intake_photo_ocr WHERE intake_id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(archive_error)?;
            saved
                .map(|(language, bytes)| {
                    let lines = serde_json::from_slice(&bytes).map_err(archive_error)?;
                    Ok::<_, String>(Checkpoint::Photo { language, lines })
                })
                .transpose()?
        }
        IntakeKind::Pdf => {
            let saved: Option<Vec<u8>> = db
                .query_row(
                    "SELECT body FROM intake_pdf_text WHERE intake_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(archive_error)?;
            saved
                .map(|bytes| {
                    serde_json::from_slice(&bytes)
                        .map(Checkpoint::Pdf)
                        .map_err(archive_error)
                })
                .transpose()?
        }
        _ => None,
    };
    let header = Header {
        metadata,
        checkpoint,
    };
    let length = header.payload_size()?;
    let encoded = serde_json::to_vec(&header).map_err(archive_error)?;
    if encoded.len() as u64 > MAX_HEADER_BYTES {
        return Err(invalid());
    }
    output
        .write_all(&(encoded.len() as u32).to_le_bytes())
        .map_err(archive_error)?;
    output.write_all(&encoded).map_err(archive_error)?;
    if matches!(header.metadata.kind, IntakeKind::Pdf) {
        let path = store.intake_pdf_path(id, length)?;
        let (source, _) = file_originals::open_pdf_source(&path)?;
        if file_originals::stream_pdf(source, output, |_| {})? != length {
            return Err(invalid());
        }
    } else {
        let bytes = payload.as_deref().unwrap_or_default();
        header.validate_inline(bytes)?;
        output.write_all(bytes).map_err(archive_error)?;
    }
    Ok(())
}

pub(crate) fn inspect(size: u64, input: impl Read) -> Result<()> {
    let (header, mut input) = open(size, input)?;
    if matches!(header.metadata.kind, IntakeKind::Pdf) {
        copy_pdf(&header, input, io::sink())?;
    } else {
        read_inline(&header, &mut input)?;
    }
    Ok(())
}

pub(crate) fn restore(
    db: &Connection,
    root: &Path,
    id: &str,
    size: u64,
    input: impl Read,
) -> Result<Option<PathBuf>> {
    let (header, mut input) = open(size, input)?;
    let changed = db
        .execute(
            "INSERT INTO intake (id,metadata) VALUES (?1,?2) ON CONFLICT(id) DO NOTHING",
            params![
                id,
                serde_json::to_string(&header.metadata).map_err(archive_error)?
            ],
        )
        .map_err(archive_error)?;
    let mut staged = None;
    let payload = if matches!(header.metadata.kind, IntakeKind::Pdf) {
        if changed == 0 {
            copy_pdf(&header, input, io::sink())?;
        } else {
            file_originals::ensure_root(root)?;
            let mut file = file_originals::stage_original(root)?;
            copy_pdf(&header, input, file.as_file_mut())?;
            file.as_file().sync_all().map_err(archive_error)?;
            staged = Some(file);
        }
        None
    } else {
        let bytes = read_inline(&header, &mut input)?;
        (!bytes.is_empty()).then_some(bytes)
    };
    if changed == 0 {
        return Ok(None);
    }
    if let Some(bytes) = payload {
        set_payload(db, id, bytes)?;
    }
    match header.checkpoint {
        Some(Checkpoint::Article(article)) => {
            set_payload(db, id, serde_json::to_vec(&article).map_err(archive_error)?)?;
        }
        Some(Checkpoint::Photo { language, lines }) => {
            db.execute(
                "INSERT INTO intake_photo_ocr VALUES (?1,?2,?3)",
                params![
                    id,
                    language,
                    serde_json::to_vec(&lines).map_err(archive_error)?
                ],
            )
            .map_err(archive_error)?;
        }
        Some(Checkpoint::Pdf(draft)) => {
            let sample = validate_pdf_draft(&draft)?;
            db.execute(
                "INSERT INTO intake_pdf_text VALUES (?1,?2,?3)",
                params![
                    id,
                    sample,
                    serde_json::to_vec(&draft).map_err(archive_error)?
                ],
            )
            .map_err(archive_error)?;
        }
        None => {}
    }
    let Some(file) = staged else { return Ok(None) };
    publish_pdf(root, Uuid::parse_str(id).map_err(archive_error)?, file).map(Some)
}

fn set_payload(db: &Connection, id: &str, bytes: Vec<u8>) -> Result<()> {
    db.execute(
        "UPDATE intake SET payload=?2 WHERE id=?1",
        params![id, bytes],
    )
    .map_err(archive_error)?;
    Ok(())
}

fn open(size: u64, mut input: impl Read) -> Result<(Header, impl Read)> {
    let mut prefix = [0; 4];
    input.read_exact(&mut prefix).map_err(archive_error)?;
    let length = u64::from(u32::from_le_bytes(prefix));
    if length == 0 || length > MAX_HEADER_BYTES || length > size.saturating_sub(4) {
        return Err(invalid());
    }
    let mut encoded = BufReader::new((&mut input).take(length));
    let header: Header = serde_json::from_reader(&mut encoded).map_err(archive_error)?;
    if encoded.into_inner().limit() != 0 || header.payload_size()? != size - 4 - length {
        return Err(invalid());
    }
    Ok((header, input))
}

fn read_inline(header: &Header, input: &mut impl Read) -> Result<Vec<u8>> {
    let length = header.payload_size()? as usize;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).map_err(archive_error)?;
    if input.read(&mut [0; 1]).map_err(archive_error)? != 0 {
        return Err(invalid());
    }
    header.validate_inline(&bytes)?;
    Ok(bytes)
}

fn copy_pdf(header: &Header, mut input: impl Read, output: impl Write) -> Result<()> {
    let mut signature = [0; 5];
    input.read_exact(&mut signature).map_err(archive_error)?;
    if signature != *b"%PDF-"
        || file_originals::stream_pdf(signature.as_slice().chain(input), output, |_| {})?
            != header.metadata.bytes
    {
        return Err(invalid());
    }
    Ok(())
}

impl Header {
    fn payload_size(&self) -> Result<u64> {
        let metadata = &self.metadata;
        if let Some(language) = &metadata.source_language {
            crate::valid_language(language)?;
        }
        match &self.checkpoint {
            Some(Checkpoint::Article(article)) if matches!(metadata.kind, IntakeKind::Link) => {
                validate_article_draft(article)?;
            }
            Some(Checkpoint::Photo { language, lines })
                if matches!(metadata.kind, IntakeKind::Photo) =>
            {
                crate::valid_language(language)?;
                ocr::recognized_lines(lines, 1)?;
            }
            Some(Checkpoint::Pdf(draft)) if matches!(metadata.kind, IntakeKind::Pdf) => {
                validate_pdf_draft(draft)?;
            }
            Some(_) => return Err(invalid()),
            None => {}
        }
        match metadata.kind {
            IntakeKind::Link if metadata.bytes == metadata.name.len() as u64 => {
                validate_article_url(&metadata.name)?;
                Ok(0)
            }
            IntakeKind::Pdf
                if (5..=file_originals::MAX_PDF_BYTES).contains(&metadata.bytes)
                    && safe_name(&metadata.name, 4096)? == metadata.name
                    && Path::new(&metadata.name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) =>
            {
                Ok(metadata.bytes)
            }
            IntakeKind::Text | IntakeKind::Txt | IntakeKind::Docx | IntakeKind::Photo
                if (1..=MAX_IMPORT as u64).contains(&metadata.bytes)
                    && safe_name(&metadata.name, 4096)? == metadata.name =>
            {
                Ok(metadata.bytes)
            }
            _ => Err(invalid()),
        }
    }

    fn validate_inline(&self, bytes: &[u8]) -> Result<()> {
        match self.metadata.kind {
            IntakeKind::Link if bytes.is_empty() => Ok(()),
            IntakeKind::Text if bytes.len() as u64 == self.metadata.bytes => {
                validate_text(std::str::from_utf8(bytes).map_err(archive_error)?)
            }
            kind @ (IntakeKind::Txt | IntakeKind::Docx | IntakeKind::Photo)
                if bytes.len() as u64 == self.metadata.bytes
                    && classify_file(&self.metadata.name, bytes)? == kind =>
            {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}
