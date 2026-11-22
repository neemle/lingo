use super::{
    application::{validate_article_draft, validate_input, IntakeRepository, StoredIntake, ValidatedAsset},
    ArticleDraft, IntakeInput, IntakeItem, IntakeKind, IntakePayload, Metadata, PdfTextDraft,
};
use crate::{file_originals, ocr, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{fs, io::Read, path::{Path, PathBuf}};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub struct PreparedIntake {
    metadata: Metadata,
    payload: Option<Vec<u8>>,
    file: Option<NamedTempFile>,
}

pub fn prepare(root: &Path, input: IntakeInput) -> Result<PreparedIntake> {
    prepare_for_request(root, input, None)
}

pub fn prepare_for_request(
    root: &Path, input: IntakeInput, request_id: Option<&str>,
) -> Result<PreparedIntake> {
    let material = validate_input(input)?;
    let (payload, file, bytes) = match material.asset {
        ValidatedAsset::Link => (None, None, material.name.len() as u64),
        ValidatedAsset::Bytes(bytes) if matches!(material.kind, IntakeKind::Pdf) => {
            let (file, size) = stage_pdf(root, bytes.as_slice())?;
            (None, Some(file), size)
        }
        ValidatedAsset::Bytes(bytes) => {
            let size = bytes.len() as u64;
            (Some(bytes), None, size)
        }
        ValidatedAsset::PdfFile(path) => {
            let (source, expected) = file_originals::open_pdf_source(&path)?;
            let file = stage_pdf_file(root, &path, request_id, source, expected)?;
            (None, Some(file), expected)
        }
    };
    Ok(PreparedIntake {
        metadata: Metadata {
            name: material.name,
            kind: material.kind,
            bytes,
            received_at: crate::now(),
        },
        payload,
        file,
    })
}

fn stage_pdf_file(
    root: &Path, path: &Path, request_id: Option<&str>, source: fs::File, expected: u64,
) -> Result<NamedTempFile> {
    // The picker and Finder adapters copy sources into this private inbox first.
    // Only reuse that per-request asset; an ordinary path may remain writable by its owner.
    #[cfg(target_vendor = "apple")]
    if let Some(request) = request_id.and_then(|id| Uuid::parse_str(id).ok()) {
        if let Some(parent) = root.parent() {
            let inbox = parent.join("share-inbox");
            let entry = inbox.join(request.simple().to_string());
            if path == entry.join("payload")
                && file_originals::verify_root(&inbox).is_ok()
                && file_originals::verify_root(&entry).is_ok()
            {
                if let Some(file) = file_originals::stage_inbox_original(root, path, &source)? {
                    return Ok(file);
                }
            }
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = (path, request_id);

    let (file, size) = stage_pdf(root, source)?;
    if size != expected {
        return Err("Розмір PDF змінився під час прийому.".into());
    }
    Ok(file)
}

fn stage_pdf(root: &Path, source: impl Read) -> Result<(NamedTempFile, u64)> {
    file_originals::ensure_root(root)?;
    let mut file = file_originals::stage_original(root)?;
    let bytes = file_originals::stream_pdf(source, file.as_file_mut(), |_| {})?;
    file.as_file().sync_all().map_err(intake_error)?;
    Ok((file, bytes))
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS intake (
            id TEXT PRIMARY KEY,
            metadata TEXT NOT NULL,
            payload BLOB,
            document_id TEXT REFERENCES documents(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS intake_photo_ocr (
            intake_id TEXT PRIMARY KEY REFERENCES intake(id) ON DELETE CASCADE,
            language TEXT NOT NULL,
            lines BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS intake_pdf_text (
            intake_id TEXT PRIMARY KEY REFERENCES intake(id) ON DELETE CASCADE,
            sample TEXT NOT NULL,
            body BLOB NOT NULL
        );",
    ).map_err(intake_error)
}

impl Store {
    pub fn insert_intake(&mut self, prepared: PreparedIntake, request_id: Option<&str>) -> Result<IntakeItem> {
        let id = request_id.map(Uuid::parse_str).transpose().map_err(intake_error)?
            .unwrap_or_else(Uuid::new_v4);
        let _guard = self.lock_intake()?;
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(intake_error)?;
        if request_id.is_some() {
            if let Some(existing) = intake_item(&tx, &id.to_string())? {
                return Ok(existing);
            }
        }
        let serialized = serde_json::to_string(&prepared.metadata).map_err(intake_error)?;
        tx.execute(
            "INSERT INTO intake (id,metadata,payload) VALUES (?1,?2,?3)",
            params![id.to_string(), serialized, prepared.payload],
        ).map_err(intake_error)?;
        if let Some(file) = prepared.file {
            let path = publish_pdf(&self.file_original_directory, id, file)?;
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

    pub fn intake_article(&self, id: &str) -> Result<Option<ArticleDraft>> {
        load_article(&self.connection, id)
    }

    pub fn save_intake_article(&mut self, id: &str, draft: ArticleDraft) -> Result<ArticleDraft> {
        validate_article_draft(&draft)?;
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(intake_error)?;
        if let Some(saved) = load_article(&tx, id)? {
            return Ok(saved);
        }
        let body = serde_json::to_vec(&draft).map_err(intake_error)?;
        let changed = tx.execute(
            "UPDATE intake SET payload=?2 WHERE id=?1 AND document_id IS NULL",
            params![id, body],
        ).map_err(intake_error)?;
        if changed == 0 {
            return Err("Матеріал уже імпортовано.".into());
        }
        tx.commit().map_err(intake_error)?;
        Ok(draft)
    }

    pub fn intake_payload(&self, id: &str) -> Result<IntakePayload> {
        self.load_for_import(id)?.payload.ok_or_else(|| "Матеріал уже імпортовано.".into())
    }

    pub fn intake_pdf_text_sample(&self, id: &str) -> Result<Option<String>> {
        pdf_text_sample(&self.connection, id)
    }

    pub fn save_intake_pdf_text(&mut self, id: &str, draft: PdfTextDraft) -> Result<String> {
        let sample = validate_pdf_draft(&draft)?;
        let body = serde_json::to_vec(&draft).map_err(intake_error)?;
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(intake_error)?;
        let item = intake_item(&tx, id)?.ok_or("Вхідний матеріал не знайдено.")?;
        if !matches!(item.metadata.kind, IntakeKind::Pdf) || item.document_id.is_some() {
            return Err("Текстовий шар можна зберегти лише для PDF у черзі імпорту.".into());
        }
        if let Some(existing) = pdf_text_sample(&tx, id)? {
            return Ok(existing);
        }
        tx.execute(
            "INSERT INTO intake_pdf_text (intake_id,sample,body) VALUES (?1,?2,?3)",
            params![id, sample, body],
        ).map_err(intake_error)?;
        tx.commit().map_err(intake_error)?;
        Ok(sample)
    }

    pub fn intake_photo_ocr_language(&self, id: &str) -> Result<Option<String>> {
        self.connection.query_row(
            "SELECT language FROM intake_photo_ocr WHERE intake_id=?1",
            [id],
            |row| row.get(0),
        ).optional().map_err(intake_error)
    }

    pub fn save_intake_photo_ocr(&mut self, id: &str, language: &str, lines: &[ocr::OcrLine]) -> Result<()> {
        crate::valid_language(language)?;
        ocr::recognized_lines(lines, 1)?;
        let body = serde_json::to_vec(lines).map_err(intake_error)?;
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(intake_error)?;
        let item = intake_item(&tx, id)?.ok_or("Вхідний матеріал не знайдено.")?;
        if !matches!(item.metadata.kind, IntakeKind::Photo) || item.document_id.is_some() {
            return Err("Розпізнавання можна зберегти лише для фотографії в черзі імпорту.".into());
        }
        tx.execute(
            "INSERT INTO intake_photo_ocr (intake_id,language,lines) VALUES (?1,?2,?3)
             ON CONFLICT(intake_id) DO UPDATE SET language=excluded.language,lines=excluded.lines",
            params![id, language, body],
        ).map_err(intake_error)?;
        tx.commit().map_err(intake_error)
    }

    pub fn intake_item(&self, id: &str) -> Result<IntakeItem> {
        intake_item(&self.connection, id)?.ok_or_else(|| "Вхідний матеріал не знайдено.".into())
    }

    pub fn intake_item_for_request(&self, id: &str) -> Result<Option<IntakeItem>> {
        let id = Uuid::parse_str(id).map_err(intake_error)?.to_string();
        intake_item(&self.connection, &id)
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

    pub(crate) fn lock_intake(&self) -> Result<Option<fs::File>> {
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

    pub(super) fn intake_pdf_path(&self, id: &str, size: u64) -> Result<PathBuf> {
        file_originals::verify_root(&self.file_original_directory)?;
        let path = pdf_path(&self.file_original_directory, Uuid::parse_str(id).map_err(intake_error)?);
        let metadata = path.symlink_metadata().map_err(intake_error)?;
        if !metadata.is_file() || metadata.len() != size {
            return Err("Збережений PDF пошкоджено.".into());
        }
        path.canonicalize().map_err(intake_error)
    }
}

fn load_article(db: &Connection, id: &str) -> Result<Option<ArticleDraft>> {
    let item = intake_item(db, id)?.ok_or("Вхідний матеріал не знайдено.")?;
    if !matches!(item.metadata.kind, IntakeKind::Link) {
        return Err("Цей матеріал не є вебпосиланням.".into());
    }
    if item.document_id.is_some() {
        return Ok(None);
    }
    let body: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM intake WHERE id=?1", [id], |row| row.get(0),
    ).optional().map_err(intake_error)?.flatten();
    decode_article(body.as_deref())
}

pub(super) fn validate_pdf_draft(draft: &PdfTextDraft) -> Result<String> {
    file_originals::validate_pdf_page_text(&draft.page_texts)?;
    if let Some(from) = draft.deferred_from {
        file_originals::validate_deferred_text_layers(&draft.page_texts, from)?;
    }
    Ok(draft.page_texts.iter().map(|page| page.trim())
        .filter(|page| !page.is_empty() && !crate::pdf_layers::damaged_text_layer(page)).take(4)
        .map(|page| page.chars().take(2000).collect::<String>())
        .collect::<Vec<_>>().join("\n"))
}

fn pdf_text_sample(db: &Connection, id: &str) -> Result<Option<String>> {
    db.query_row(
        "SELECT sample FROM intake_pdf_text WHERE intake_id=?1",
        [id],
        |row| row.get(0),
    ).optional().map_err(intake_error)
}

fn decode_article(body: Option<&[u8]>) -> Result<Option<ArticleDraft>> {
    body.map(|body| serde_json::from_slice(body).map_err(intake_error)).transpose()
}

fn intake_item(db: &Connection, id: &str) -> Result<Option<IntakeItem>> {
    let row: Option<(String, Option<String>)> = db.query_row(
        "SELECT metadata,document_id FROM intake WHERE id=?1", [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(intake_error)?;
    row.map(|(metadata, document)| {
        let metadata = serde_json::from_str(&metadata).map_err(intake_error)?;
        Ok(IntakeItem::new(id.to_owned(), metadata, document))
    }).transpose()
}

impl IntakeRepository for Store {
    fn load_for_import(&self, id: &str) -> Result<StoredIntake> {
        let (item, bytes) = self.load_intake(id)?;
        let mut article = None;
        let payload = if item.document_id.is_some() {
            None
        } else {
            Some(match item.metadata.kind {
                IntakeKind::Pdf => IntakePayload::File {
                    path: self.intake_pdf_path(id, item.metadata.bytes)?,
                },
                IntakeKind::Link => {
                    article = decode_article(bytes.as_deref())?;
                    IntakePayload::Link { url: item.metadata.name.clone() }
                },
                _ => IntakePayload::Bytes {
                    bytes: bytes.ok_or("Відсутній вміст матеріалу.")?,
                },
            })
        };
        Ok(StoredIntake { item, payload, article })
    }

    fn load_photo_lines(&self, id: &str, language: &str) -> Result<Vec<ocr::OcrLine>> {
        let body: Vec<u8> = self.connection.query_row(
            "SELECT lines FROM intake_photo_ocr WHERE intake_id=?1 AND language=?2",
            params![id, language],
            |row| row.get(0),
        ).optional().map_err(intake_error)?
            .ok_or("Розпізнаний текст цією мовою відсутній.")?;
        serde_json::from_slice(&body).map_err(intake_error)
    }

    fn load_pdf_text(&self, id: &str) -> Result<PdfTextDraft> {
        let body: Vec<u8> = self.connection.query_row(
            "SELECT body FROM intake_pdf_text WHERE intake_id=?1",
            [id],
            |row| row.get(0),
        ).optional().map_err(intake_error)?
            .ok_or("Збережений текстовий шар PDF відсутній.")?;
        serde_json::from_slice(&body).map_err(intake_error)
    }

    fn record_imported(&mut self, item: &IntakeItem) -> Result<()> {
        let metadata = serde_json::to_string(&item.metadata).map_err(intake_error)?;
        let tx = self.connection.transaction().map_err(intake_error)?;
        tx.execute(
            "UPDATE intake SET metadata=?2,document_id=?3,payload=NULL WHERE id=?1",
            params![item.id, metadata, item.document_id],
        ).map_err(intake_error)?;
        if matches!(item.metadata.kind, IntakeKind::Photo) {
            tx.execute("DELETE FROM intake_photo_ocr WHERE intake_id=?1", [&item.id])
                .map_err(intake_error)?;
        }
        let is_pdf = matches!(item.metadata.kind, IntakeKind::Pdf | IntakeKind::TextPdf | IntakeKind::ScannedPdf | IntakeKind::MixedPdf);
        if is_pdf {
            tx.execute("DELETE FROM intake_pdf_text WHERE intake_id=?1", [&item.id])
                .map_err(intake_error)?;
        }
        tx.commit().map_err(intake_error)?;
        if is_pdf {
            let uuid = Uuid::parse_str(&item.id).map_err(intake_error)?;
            let _ = fs::remove_file(pdf_path(&self.file_original_directory, uuid));
        }
        Ok(())
    }
}

// Call after inserting a new UUID in the SQLite write transaction. Any file
// already at this path is an orphan from an interrupted publication.
pub(super) fn publish_pdf(root: &Path, id: Uuid, file: NamedTempFile) -> Result<PathBuf> {
    file_originals::verify_root(root)?;
    let path = pdf_path(root, id);
    file.persist(&path).map_err(intake_error)?;
    Ok(path)
}

fn pdf_path(root: &Path, id: Uuid) -> PathBuf {
    root.join(format!(".intake-{id}.pdf"))
}

fn intake_error(error: impl std::fmt::Display) -> String {
    format!("Помилка прийому матеріалу. {error}")
}
