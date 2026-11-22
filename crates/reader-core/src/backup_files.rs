use crate::{
    backup_limits::{LimitedReader, LimitedWriter},
    dictionary_transfer, document_revisions, practice, reading_history, Backup, BackupResource,
    Result, Snapshot, Store,
};
use serde::{
    ser::{Error as _, SerializeSeq},
    Serialize, Serializer,
};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::Path,
};

pub const MAX_BACKUP_FILE_BYTES: u64 = 1024 * 1024 * 1024;

fn archive_error(error: impl std::fmt::Display) -> String {
    format!("Помилка резервної копії: {error}")
}

fn validate_path(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) || path.file_name().is_none() {
        return Err("Некоректний шлях резервної копії.".into());
    }
    Ok(())
}

fn validate_resources(resources: &[BackupResource]) -> Result<()> {
    if resources.len() > 1024
        || resources.iter().any(|resource| {
            [&resource.kind, &resource.id, &resource.revision]
                .iter()
                .any(|field| field.len() > 1024)
        })
    {
        return Err("Маніфест ресурсів резервної копії завеликий.".into());
    }
    Ok(())
}

struct StreamingOriginals<'a> {
    store: &'a Store,
    ids: Vec<String>,
}

impl Serialize for StreamingOriginals<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.ids.len()))?;
        for id in &self.ids {
            let original = self.store.original(id).map_err(S::Error::custom)?;
            sequence.serialize_element(&original)?;
        }
        sequence.end()
    }
}

fn assemble<O>(
    store: &Store,
    tx: &rusqlite::Transaction<'_>,
    snapshot: Snapshot,
    originals: O,
) -> Result<Backup<O>> {
    let (dictionary_sources, records) =
        dictionary_transfer::export_details(tx, &snapshot.vocabulary)?;
    let (dictionary_review_origins, reviews) = records.into_iter().map(|r| (r.id, r.event)).unzip();
    Ok(Backup {
        version: 1,
        snapshot,
        originals,
        reviews,
        dictionary_sources,
        dictionary_review_origins,
        practice: practice::read(tx)?,
        resources: store.backup_resources(),
        reading_sessions: reading_history::export(tx)?,
        document_revisions: document_revisions::export(tx)?,
    })
}

impl Store {
    pub fn export(&self) -> Result<Backup> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(archive_error)?;
        let snapshot = self.snapshot()?;
        let originals = snapshot
            .documents
            .iter()
            .map(|d| self.original(&d.id))
            .collect::<Result<Vec<_>>>()?;
        let backup = assemble(self, &tx, snapshot, originals)?;
        tx.commit().map_err(archive_error)?;
        Ok(backup)
    }

    /// Publish complete private JSON without overwriting any existing destination.
    pub fn export_backup_file(&self, path: &Path) -> Result<()> {
        validate_path(path)?;
        match path.symlink_metadata() {
            Ok(_) => return Err("Файл резервної копії вже існує.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(archive_error(error)),
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut output = tempfile::Builder::new()
            .prefix(".reader-backup-")
            .tempfile_in(parent)
            .map_err(archive_error)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(archive_error)?;
        let snapshot = self.snapshot()?;
        let originals = StreamingOriginals {
            store: self,
            ids: snapshot.documents.iter().map(|d| d.id.clone()).collect(),
        };
        let backup = assemble(self, &tx, snapshot, originals)?;
        {
            let bounded = LimitedWriter::new(output.as_file_mut(), MAX_BACKUP_FILE_BYTES);
            let mut writer = BufWriter::with_capacity(64 * 1024, bounded);
            serde_json::to_writer(&mut writer, &backup).map_err(archive_error)?;
            writer.flush().map_err(archive_error)?;
        }
        output.as_file().sync_all().map_err(archive_error)?;
        tx.commit().map_err(archive_error)?;
        output.persist_noclobber(path).map_err(archive_error)?;
        Ok(())
    }

    /// Parse capped JSON and retain the existing all-or-nothing Store restore.
    pub fn restore_backup_file(&mut self, path: &Path) -> Result<Vec<BackupResource>> {
        validate_path(path)?;
        if !fs::symlink_metadata(path).map_err(archive_error)?.is_file() {
            return Err("Резервна копія має бути звичайним файлом.".into());
        }
        let file = File::open(path).map_err(archive_error)?;
        let metadata = file.metadata().map_err(archive_error)?;
        if !metadata.is_file() || metadata.len() > MAX_BACKUP_FILE_BYTES {
            return Err("Резервна копія перевищує 1 ГіБ або не є звичайним файлом.".into());
        }
        let mut reader =
            BufReader::with_capacity(64 * 1024, LimitedReader::new(&file, MAX_BACKUP_FILE_BYTES));
        let backup: Backup = serde_json::from_reader(&mut reader).map_err(archive_error)?;
        if file.metadata().map_err(archive_error)?.len() > MAX_BACKUP_FILE_BYTES {
            return Err("Резервна копія перевищує 1 ГіБ.".into());
        }
        // Keep the success response within the small ABI envelope before committing data.
        validate_resources(&backup.resources)?;
        let resources = backup.resources.clone();
        self.restore(backup)?;
        Ok(resources)
    }
}
