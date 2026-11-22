use crate::{file_originals, language_detection, ocr, Result, Store, MAX_IMPORT};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IntakeInput {
    Text { name: String, text: String },
    Link { url: String },
    File { name: String, bytes: Vec<u8> },
    PdfFile { name: String, path: PathBuf },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IntakeImport {
    Document { language: Option<String> },
    Article { name: String, text: String, language: Option<String> },
    Photo { lines: Vec<ocr::OcrLine>, language: Option<String> },
    Pdf {
        #[serde(rename = "pageTexts")]
        page_texts: Vec<String>,
        language: String,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum IntakeKind {
    Text, Link, Txt, Docx, Photo, Pdf, TextPdf, ScannedPdf, MixedPdf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum IntakeAction { ImportDocument, ExtractArticle, RecognizePhoto, ReadPdfPages, Complete }

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    name: String,
    kind: IntakeKind,
    bytes: u64,
    received_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeItem {
    pub id: String,
    #[serde(flatten)]
    metadata: Metadata,
    next_action: IntakeAction,
    document_id: Option<String>,
}

impl IntakeItem {
    fn new(id: String, metadata: Metadata, document_id: Option<String>) -> Self {
        let next_action = if document_id.is_some() {
            IntakeAction::Complete
        } else {
            match metadata.kind {
                IntakeKind::Text | IntakeKind::Txt | IntakeKind::Docx => IntakeAction::ImportDocument,
                IntakeKind::Link => IntakeAction::ExtractArticle,
                IntakeKind::Photo => IntakeAction::RecognizePhoto,
                _ => IntakeAction::ReadPdfPages,
            }
        };
        Self { id, metadata, next_action, document_id }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IntakePayload {
    Bytes { bytes: Vec<u8> },
    Link { url: String },
    File { path: PathBuf },
}

pub struct PreparedIntake {
    metadata: Metadata,
    payload: Option<Vec<u8>>,
    file: Option<NamedTempFile>,
}

pub fn prepare(root: &Path, input: IntakeInput) -> Result<PreparedIntake> {
    let (kind, name, payload, file, bytes) = match input {
        IntakeInput::Text { name, text } => {
            let text = text_bytes(text)?;
            (IntakeKind::Text, text_name(&name)?, Some(text), None, 0)
        }
        IntakeInput::Link { url } => {
            validate_article_url(&url)?;
            let size = url.len() as u64;
            (IntakeKind::Link, url, None, None, size)
        }
        IntakeInput::File { name, bytes } => {
            if bytes.is_empty() || bytes.len() > MAX_IMPORT {
                return Err("Файл має містити від 1 байта до 20 МіБ. Для великого PDF передайте шлях.".into());
            }
            let name = safe_name(&name)?;
            let extension = Path::new(name).extension().and_then(|value| value.to_str())
                .unwrap_or_default().to_ascii_lowercase();
            if bytes.starts_with(b"%PDF-") && extension == "pdf" {
                file_originals::ensure_root(root)?;
                let mut file = file_originals::stage_original(root)?;
                let (size, _) = file_originals::copy_pdf(bytes.as_slice(), file.as_file_mut())?;
                file.as_file().sync_all().map_err(intake_error)?;
                (IntakeKind::Pdf, name.to_owned(), None, Some(file), size)
            } else {
                let kind = match extension.as_str() {
                    "txt" if std::str::from_utf8(&bytes).is_ok_and(|text| !text.trim().is_empty()) => IntakeKind::Txt,
                    "docx" if bytes.starts_with(b"PK\x03\x04") => IntakeKind::Docx,
                    ext if ocr::valid_image_source(ext, &bytes, None) => IntakeKind::Photo,
                    _ => return Err("Формат вхідного файлу пошкоджено або він не підтримується.".into()),
                };
                (kind, name.to_owned(), Some(bytes), None, 0)
            }
        }
        IntakeInput::PdfFile { name, path } => {
            let name = file_originals::pdf_name(&name)?.to_owned();
            if path.as_os_str().is_empty() || path.as_os_str().len() > 4096 {
                return Err("Некоректний шлях до PDF.".into());
            }
            let (source, expected) = file_originals::open_pdf_source(&path)?;
            file_originals::ensure_root(root)?;
            let mut file = file_originals::stage_original(root)?;
            let (size, _) = file_originals::copy_pdf(source, file.as_file_mut())?;
            if size != expected {
                return Err("Розмір PDF змінився під час прийому.".into());
            }
            file.as_file().sync_all().map_err(intake_error)?;
            (IntakeKind::Pdf, name, None, Some(file), size)
        }
    };
    let bytes = payload.as_ref().map_or(bytes, |data| data.len() as u64);
    Ok(PreparedIntake {
        metadata: Metadata { name, kind, bytes, received_at: crate::now() },
        payload,
        file,
    })
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS intake (
            id TEXT PRIMARY KEY,
            metadata TEXT NOT NULL,
            payload BLOB,
            document_id TEXT REFERENCES documents(id) ON DELETE CASCADE
        );",
    ).map_err(intake_error)
}

impl Store {
    pub fn stage_intake(&mut self, input: IntakeInput) -> Result<IntakeItem> {
        let prepared = prepare(&self.file_original_directory, input)?;
        self.insert_intake(prepared)
    }

    pub fn insert_intake(&mut self, prepared: PreparedIntake) -> Result<IntakeItem> {
        let id = Uuid::new_v4();
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(intake_error)?;
        let serialized = serde_json::to_string(&prepared.metadata).map_err(intake_error)?;
        tx.execute(
            "INSERT INTO intake (id,metadata,payload) VALUES (?1,?2,?3)",
            params![id.to_string(), serialized, prepared.payload],
        ).map_err(intake_error)?;
        if let Some(file) = prepared.file {
            file_originals::verify_root(&self.file_original_directory)?;
            let path = pdf_path(&self.file_original_directory, id);
            file.persist_noclobber(&path).map_err(intake_error)?;
            if let Err(error) = file_originals::sync_directory(&self.file_original_directory)
                .and_then(|_| tx.commit().map_err(intake_error))
            {
                let _ = fs::remove_file(path);
                return Err(error);
            }
        } else {
            tx.commit().map_err(intake_error)?;
        }
        Ok(IntakeItem::new(id.to_string(), prepared.metadata, None))
    }

    pub fn list_intake(&self, start: usize) -> Result<Vec<IntakeItem>> {
        let start = i64::try_from(start).map_err(intake_error)?;
        let mut query = self.connection.prepare(
            "SELECT id,metadata FROM intake WHERE document_id IS NULL ORDER BY rowid DESC LIMIT 100 OFFSET ?1",
        ).map_err(intake_error)?;
        let rows = query.query_map([start], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(intake_error)?;
        rows.map(|row| {
            let (id, metadata) = row.map_err(intake_error)?;
            Ok(IntakeItem::new(id, serde_json::from_str(&metadata).map_err(intake_error)?, None))
        }).collect()
    }

    pub fn intake_payload(&self, id: &str) -> Result<IntakePayload> {
        let (item, data) = self.load_intake(id)?;
        if item.document_id.is_some() {
            return Err("Матеріал уже імпортовано.".into());
        }
        match item.metadata.kind {
            IntakeKind::Pdf => Ok(IntakePayload::File { path: self.intake_pdf_path(id, item.metadata.bytes)? }),
            IntakeKind::Link => Ok(IntakePayload::Link { url: item.metadata.name }),
            _ => Ok(IntakePayload::Bytes { bytes: data.ok_or("Відсутній вміст матеріалу.")? }),
        }
    }

    pub fn intake_item(&self, id: &str) -> Result<IntakeItem> {
        let (metadata, document): (String, Option<String>) = self.connection.query_row(
            "SELECT metadata,document_id FROM intake WHERE id=?1", [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(intake_error)?.ok_or("Вхідний матеріал не знайдено.")?;
        let metadata = serde_json::from_str(&metadata).map_err(intake_error)?;
        Ok(IntakeItem::new(id.to_owned(), metadata, document))
    }

    pub fn complete_intake(&mut self, id: &str, result: IntakeImport) -> Result<IntakeItem> {
        let _guard = self.lock_intake()?;
        let (mut item, data) = self.load_intake(id)?;
        if item.document_id.is_some() {
            return Ok(item);
        }
        let payload = || data.as_deref().ok_or("Відсутній вміст матеріалу.");
        let mut pdf = None;
        let document = match (item.metadata.kind, result) {
            (IntakeKind::Text | IntakeKind::Txt | IntakeKind::Docx, IntakeImport::Document { language }) => {
                let bytes = payload()?;
                let language = choose_language(language, || language_detection::detect_document(&item.metadata.name, bytes))?;
                self.import(&item.metadata.name, bytes, &language)?
            }
            (IntakeKind::Link, IntakeImport::Article { name, text, language }) => {
                let bytes = text_bytes(text)?;
                let name = text_name(&name)?;
                let language = choose_language(language, || language_detection::detect_document(&name, &bytes))?;
                self.import_article(&name, &bytes, &language, &item.metadata.name)?
            }
            (IntakeKind::Photo, IntakeImport::Photo { lines, language }) => {
                let sample = lines.iter().take(64).map(ocr::OcrLine::reading_text).collect::<Vec<_>>().join(" ");
                let language = choose_language(language, || language_detection::detect(&sample))?;
                self.import_image(&item.metadata.name, payload()?, &language, lines)?
            }
            (IntakeKind::Pdf, IntakeImport::Pdf { page_texts, language }) => {
                let path = self.intake_pdf_path(id, item.metadata.bytes)?;
                let text_pages = page_texts.iter().filter(|page| !page.trim().is_empty()).count();
                item.metadata.kind = if text_pages == 0 { IntakeKind::ScannedPdf }
                    else if text_pages == page_texts.len() { IntakeKind::TextPdf }
                    else { IntakeKind::MixedPdf };
                let prepared = file_originals::prepare_pdf_import(
                    &self.file_original_directory, &item.metadata.name, &path, &language, page_texts, Vec::new(),
                )?;
                pdf = Some(path);
                self.import_pdf_file(prepared)?
            }
            _ => return Err("Результат обробки не відповідає типу матеріалу.".into()),
        };
        let metadata = serde_json::to_string(&item.metadata).map_err(intake_error)?;
        self.connection.execute(
            "UPDATE intake SET metadata=?2,document_id=?3,payload=NULL WHERE id=?1",
            params![id, metadata, document.id],
        ).map_err(intake_error)?;
        if let Some(path) = pdf {
            let _ = fs::remove_file(path);
        }
        Ok(IntakeItem::new(id.to_owned(), item.metadata, Some(document.id)))
    }

    pub fn discard_intake(&self, id: &str) -> Result<bool> {
        let uuid = Uuid::parse_str(id).map_err(intake_error)?;
        let _guard = self.lock_intake()?;
        let removed = self.connection.execute("DELETE FROM intake WHERE id=?1 AND document_id IS NULL", [id])
            .map_err(intake_error)?;
        if removed != 0 {
            let _ = fs::remove_file(pdf_path(&self.file_original_directory, uuid));
        }
        Ok(removed != 0)
    }

    fn lock_intake(&self) -> Result<Option<fs::File>> {
        if self.connection.path().is_none_or(str::is_empty) {
            return Ok(None);
        }
        file_originals::ensure_root(&self.file_original_directory)?;
        crate::model_storage::lock_package(&self.file_original_directory.join(".intake.lock"), true)
            .map(Some)
    }

    fn load_intake(&self, id: &str) -> Result<(IntakeItem, Option<Vec<u8>>)> {
        let (metadata, payload, document) = self.connection.query_row(
            "SELECT metadata,payload,document_id FROM intake WHERE id=?1", [id],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(intake_error)?.ok_or("Вхідний матеріал не знайдено.")?;
        let metadata = serde_json::from_str(&metadata).map_err(intake_error)?;
        Ok((IntakeItem::new(id.to_owned(), metadata, document), payload))
    }

    fn intake_pdf_path(&self, id: &str, size: u64) -> Result<PathBuf> {
        file_originals::verify_root(&self.file_original_directory)?;
        let path = pdf_path(&self.file_original_directory, Uuid::parse_str(id).map_err(intake_error)?);
        let metadata = path.symlink_metadata().map_err(intake_error)?;
        if !metadata.is_file() || metadata.len() != size {
            return Err("Збережений PDF пошкоджено.".into());
        }
        path.canonicalize().map_err(intake_error)
    }
}

pub(crate) fn validate_article_url(url: &str) -> Result<()> {
    let authority = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    if url.len() > 4096 || url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        || authority.is_none_or(|host| host.is_empty() || host.contains('@'))
    {
        return Err("Некоректна адреса джерела статті.".into());
    }
    Ok(())
}

fn text_bytes(text: String) -> Result<Vec<u8>> {
    if text.trim().is_empty() || text.len() > MAX_IMPORT {
        return Err("Потрібне непорожнє текстове джерело до 20 МіБ.".into());
    }
    Ok(text.into_bytes())
}

fn text_name(name: &str) -> Result<String> {
    let name = safe_name(name)?;
    Ok(if name.to_ascii_lowercase().ends_with(".txt") { name.to_owned() } else { format!("{name}.txt") })
}

fn safe_name(name: &str) -> Result<&str> {
    Path::new(name).file_name().and_then(|name| name.to_str())
        .filter(|name| name.len() <= 4092 && !name.trim().is_empty() && !name.chars().any(char::is_control))
        .ok_or_else(|| "Некоректна назва вхідного матеріалу.".into())
}

fn choose_language(
    selected: Option<String>, detect: impl FnOnce() -> Option<&'static str>,
) -> Result<String> {
    match selected {
        Some(language) => { crate::valid_language(&language)?; Ok(language) }
        None => detect().map(str::to_owned).ok_or_else(|| "Оберіть мову вхідного матеріалу.".into()),
    }
}

fn pdf_path(root: &Path, id: Uuid) -> PathBuf {
    root.join(format!(".intake-{id}.pdf"))
}

fn intake_error(error: impl std::fmt::Display) -> String {
    format!("Помилка прийому матеріалу. {error}")
}
