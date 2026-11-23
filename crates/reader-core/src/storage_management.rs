use crate::{Result, Store};
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const CACHE_TABLES: [&str; 3] = ["translation_cache", "linguistic_annotations", "model_hints"];

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageScope {
    Cache,
    Downloads,
    All,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    database_bytes: u64,
    original_bytes: u64,
    download_bytes: u64,
    database_cache_bytes: u64,
    audio_cache_bytes: u64,
}

impl Store {
    fn download_roots(&self) -> [PathBuf; 4] {
        [
            self.model_directory.clone(),
            self.optional_model_directory.clone(),
            self.knowledge_directory.clone(),
            self.optional_model_directory
                .with_file_name("model-downloads"),
        ]
    }

    pub fn storage_usage(&self) -> Result<StorageUsage> {
        let database_bytes = self
            .connection
            .path()
            .filter(|path| !path.is_empty())
            .map_or(Ok(0), |path| {
                Ok::<u64, String>(
                    measure(Path::new(path), false)?.0
                        + measure(Path::new(&format!("{path}-wal")), false)?.0
                        + measure(Path::new(&format!("{path}-shm")), false)?.0,
                )
            })?;
        let cache = self.connection.query_row(
            "SELECT coalesce(sum(pgsize), 0) FROM dbstat WHERE name IN (
                SELECT name FROM sqlite_schema WHERE
                    (tbl_name IN ('translation_cache', 'translation_cache_owners')
                        AND EXISTS(SELECT 1 FROM translation_cache))
                    OR (tbl_name='linguistic_annotations' AND EXISTS(SELECT 1 FROM linguistic_annotations))
                    OR (tbl_name='model_hints' AND EXISTS(SELECT 1 FROM model_hints))
            )",
            [], |row| row.get::<_, i64>(0),
        ).or_else(|_| self.connection.query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(body AS BLOB)) + length(key)), 0) FROM translation_cache)
                  + (SELECT coalesce(sum(length(CAST(annotation_blob AS BLOB)) + length(cache_key)), 0) FROM linguistic_annotations)
                  + (SELECT coalesce(sum(length(CAST(annotation_blob AS BLOB)) + length(cache_key)), 0) FROM model_hints)",
            [], |row| row.get::<_, i64>(0),
        )).map_err(storage_error)?;
        let mut usage = StorageUsage {
            database_bytes,
            original_bytes: measure(&self.file_original_directory, false)?.0,
            database_cache_bytes: (cache as u64).min(database_bytes),
            ..StorageUsage::default()
        };
        for path in self.download_roots() {
            let (download, cache) = measure(&path, path == self.optional_model_directory)?;
            usage.download_bytes += download;
            usage.audio_cache_bytes += cache;
        }
        Ok(usage)
    }

    pub fn clear_storage(&mut self, scope: StorageScope) -> Result<()> {
        match scope {
            StorageScope::Cache => {
                let tx = self
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(storage_error)?;
                tx.execute("DELETE FROM translation_windows", [])
                    .map_err(storage_error)?;
                for table in CACHE_TABLES {
                    tx.execute(&format!("DELETE FROM {table}"), [])
                        .map_err(storage_error)?;
                }
                tx.commit().map_err(storage_error)?;
                clear_audio_cache(&self.optional_model_directory)?;
            }
            StorageScope::Downloads => self.clear_downloads()?,
            StorageScope::All => {
                let tx = self
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(storage_error)?;
                tx.pragma_update(None, "defer_foreign_keys", true)
                    .map_err(storage_error)?;
                let tables = {
                    let mut query = tx.prepare(
                        "SELECT name FROM sqlite_schema WHERE type='table' AND substr(name,1,7)<>'sqlite_'",
                    ).map_err(storage_error)?;
                    let rows = query
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(storage_error)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(storage_error)?
                };
                for table in tables {
                    let table = table.replace('"', "\"\"");
                    tx.execute(&format!("DELETE FROM \"{table}\""), [])
                        .map_err(storage_error)?;
                }
                tx.commit().map_err(storage_error)?;
                self.active_pdf_ocr = None;
                remove(&self.file_original_directory)?;
                self.clear_downloads()?;
            }
        }
        if !matches!(scope, StorageScope::Downloads) {
            self.connection
                .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(storage_error)?;
        }
        Ok(())
    }

    fn clear_downloads(&self) -> Result<()> {
        for path in self.download_roots() {
            remove(&path)?;
        }
        Ok(())
    }
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage_error(error)),
    }
}

fn measure(path: &Path, split_audio: bool) -> Result<(u64, u64)> {
    let Some(entry) = metadata(path)? else {
        return Ok((0, 0));
    };
    if entry.is_file() {
        return Ok((entry.len(), 0));
    }
    if !entry.is_dir() {
        return Ok((0, 0));
    }
    if split_audio && path.file_name().is_some_and(|name| name == ".speech-cache") {
        return Ok((0, measure(path, false)?.0));
    }
    let mut total = (0, 0);
    for child in fs::read_dir(path).map_err(storage_error)? {
        let (bytes, audio) = measure(&child.map_err(storage_error)?.path(), split_audio)?;
        total.0 += bytes;
        total.1 += audio;
    }
    Ok(total)
}

fn clear_audio_cache(path: &Path) -> Result<()> {
    if !metadata(path)?.is_some_and(|entry| entry.is_dir()) {
        return Ok(());
    }
    for child in fs::read_dir(path).map_err(storage_error)? {
        let child = child.map_err(storage_error)?;
        if child.file_name() == ".speech-cache" {
            remove(&child.path())?;
        } else if child.file_type().map_err(storage_error)?.is_dir() {
            clear_audio_cache(&child.path())?;
        }
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    let Some(entry) = metadata(path)? else {
        return Ok(());
    };
    if entry.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(storage_error)
}

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Помилка локального сховища. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader_preferences::ReaderPreferenceChange;

    #[test]
    fn cache_downloads_and_full_reset_delete_only_their_own_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let mut store = Store::open(&path).unwrap();
        store
            .import("reader.txt", b"Bonjour le monde.", "fr")
            .unwrap();
        store
            .update_reader_preference(ReaderPreferenceChange::BackgroundTranslation { value: true })
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO translation_cache VALUES ('test', '[\"cached\"]')",
                [],
            )
            .unwrap();

        let model = store.optional_model_directory.join("voice");
        let audio = model.join(".speech-cache");
        fs::create_dir_all(&audio).unwrap();
        fs::write(model.join("model.onnx"), [0; 128]).unwrap();
        fs::write(audio.join("sample.wav"), [0; 64]).unwrap();
        fs::write(root.path().join("unrelated.txt"), "keep").unwrap();
        let usage = store.storage_usage().unwrap();
        assert_eq!(usage.download_bytes, 128);
        assert_eq!(usage.audio_cache_bytes, 64);
        assert!(usage.database_cache_bytes > 0);

        store.clear_storage(StorageScope::Cache).unwrap();
        assert_eq!(store.snapshot().unwrap().documents.len(), 1);
        let usage = store.storage_usage().unwrap();
        assert_eq!(usage.database_cache_bytes, 0);
        assert_eq!(usage.audio_cache_bytes, 0);
        assert_eq!(usage.download_bytes, 128);

        store.clear_storage(StorageScope::Downloads).unwrap();
        assert_eq!(store.snapshot().unwrap().documents.len(), 1);
        assert_eq!(store.storage_usage().unwrap().download_bytes, 0);

        store.clear_storage(StorageScope::All).unwrap();
        assert!(store.snapshot().unwrap().documents.is_empty());
        assert!(store.reader_preferences().unwrap().is_empty());
        assert!(root.path().join("unrelated.txt").is_file());
        drop(store);
        assert!(Store::open(&path)
            .unwrap()
            .snapshot()
            .unwrap()
            .documents
            .is_empty());
    }
}
