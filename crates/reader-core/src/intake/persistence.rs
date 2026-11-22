use super::{
    application::{validate_input, IntakeRepository, StoredIntake, ValidatedAsset},
    IntakeInput, IntakeItem, IntakeKind, IntakePayload, Metadata,
};
use crate::{file_originals, Result, Store};
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
            let (file, size) = stage_pdf(root, source)?;
            if size != expected {
                return Err("Розмір PDF змінився під час прийому.".into());
            }
            (None, Some(file), size)
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

fn stage_pdf(root: &Path, source: impl Read) -> Result<(NamedTempFile, u64)> {
    file_originals::ensure_root(root)?;
    let mut file = file_originals::stage_original(root)?;
    let (bytes, _) = file_originals::copy_pdf(source, file.as_file_mut())?;
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
        );",
    ).map_err(intake_error)
}

impl Store {
    pub fn insert_intake(&mut self, prepared: PreparedIntake, request_id: Option<&str>) -> Result<IntakeItem> {
        let id = request_id.map(Uuid::parse_str).transpose().map_err(intake_error)?
            .unwrap_or_else(Uuid::new_v4);
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
        self.load_for_import(id)?.payload.ok_or_else(|| "Матеріал уже імпортовано.".into())
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

    pub(super) fn lock_intake(&self) -> Result<Option<fs::File>> {
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
        let payload = if item.document_id.is_some() {
            None
        } else {
            Some(match item.metadata.kind {
                IntakeKind::Pdf => IntakePayload::File {
                    path: self.intake_pdf_path(id, item.metadata.bytes)?,
                },
                IntakeKind::Link => IntakePayload::Link {
                    url: item.metadata.name.clone(),
                },
                _ => IntakePayload::Bytes {
                    bytes: bytes.ok_or("Відсутній вміст матеріалу.")?,
                },
            })
        };
        Ok(StoredIntake { item, payload })
    }

    fn record_imported(&self, item: &IntakeItem) -> Result<()> {
        let metadata = serde_json::to_string(&item.metadata).map_err(intake_error)?;
        self.connection.execute(
            "UPDATE intake SET metadata=?2,document_id=?3,payload=NULL WHERE id=?1",
            params![item.id, metadata, item.document_id],
        ).map_err(intake_error)?;
        if matches!(item.metadata.kind, IntakeKind::Pdf | IntakeKind::TextPdf | IntakeKind::ScannedPdf | IntakeKind::MixedPdf) {
            let uuid = Uuid::parse_str(&item.id).map_err(intake_error)?;
            let _ = fs::remove_file(pdf_path(&self.file_original_directory, uuid));
        }
        Ok(())
    }
}

fn pdf_path(root: &Path, id: Uuid) -> PathBuf {
    root.join(format!(".intake-{id}.pdf"))
}

fn intake_error(error: impl std::fmt::Display) -> String {
    format!("Помилка прийому матеріалу. {error}")
}
