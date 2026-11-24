use crate::{Result, Store};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const CACHE_TABLES: [&str; 5] = [
    "translation_cache",
    "linguistic_annotations",
    "model_hints",
    "translation_windows",
    "background_translation_progress",
];

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
    translation_cache_bytes: u64,
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
        let (translations, other_cache) = self.connection.query_row(
            "SELECT coalesce(sum(CASE WHEN tbl_name IN ('translation_cache', 'translation_cache_owners')
                                     THEN pgsize ELSE 0 END), 0),
                    coalesce(sum(CASE WHEN tbl_name NOT IN ('translation_cache', 'translation_cache_owners')
                                     THEN pgsize ELSE 0 END), 0)
             FROM sqlite_schema CROSS JOIN dbstat
             WHERE dbstat.name = sqlite_schema.name AND (
                   (tbl_name IN ('translation_cache', 'translation_cache_owners')
                        AND EXISTS(SELECT 1 FROM translation_cache))
                OR (tbl_name='linguistic_annotations' AND EXISTS(SELECT 1 FROM linguistic_annotations))
                OR (tbl_name='model_hints' AND EXISTS(SELECT 1 FROM model_hints))
                OR (tbl_name='translation_windows' AND EXISTS(SELECT 1 FROM translation_windows))
                OR (tbl_name='background_translation_progress' AND EXISTS(SELECT 1 FROM background_translation_progress)))",
            [], |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        ).or_else(|_| self.connection.query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(body AS BLOB)) + length(key)), 0) FROM translation_cache),
                    (SELECT coalesce(sum(length(CAST(annotation_blob AS BLOB)) + length(cache_key)), 0) FROM linguistic_annotations)
                  + (SELECT coalesce(sum(length(CAST(annotation_blob AS BLOB)) + length(cache_key)), 0) FROM model_hints)
                  + (SELECT coalesce(sum(length(document_id) + length(sentence_id) + length(revision_id)
                              + length(source) + length(target) + coalesce(length(lease), 0)), 0)
                        FROM translation_windows)
                  + (SELECT coalesce(sum(length(document_id) + length(target) + length(revision_id)
                              + coalesce(length(CAST(route_identity AS BLOB)), 0)), 0)
                        FROM background_translation_progress)",
            [], |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )).map_err(storage_error)?;
        let mut usage = StorageUsage {
            database_bytes,
            original_bytes: measure(&self.file_original_directory, false)?.0,
            database_cache_bytes: (translations + other_cache).min(database_bytes),
            translation_cache_bytes: translations.min(database_bytes),
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
                crate::reader_preferences::disable_background_translation(&tx)?;
                for table in CACHE_TABLES {
                    tx.execute(&format!("DELETE FROM {table}"), [])
                        .map_err(storage_error)?;
                }
                tx.commit().map_err(storage_error)?;
                crate::e5::clear_cache();
                clear_audio_cache(&self.optional_model_directory)?;
                self.connection.execute_batch("VACUUM;").map_err(storage_error)?;
                self.checkpoint_storage()?;
            }
            StorageScope::Downloads => {
                self.clear_downloads()?;
                crate::e5::clear_cache();
            }
            StorageScope::All => {
                self.reset_database()?;
                crate::e5::clear_cache();
                self.active_pdf_ocr = None;
                remove(&self.file_original_directory)?;
                self.clear_downloads()?;
                self.checkpoint_storage()?;
            }
        }
        Ok(())
    }

    fn reset_database(&mut self) -> Result<()> {
        // A SQLite backup replaces the main database transactionally while keeping
        // the existing path and open connections valid.
        let page_size: u32 = self
            .connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .map_err(storage_error)?;
        let mut fresh = Connection::open_in_memory().map_err(storage_error)?;
        fresh
            .pragma_update(None, "page_size", page_size)
            .map_err(storage_error)?;
        Self::initialize_database(&mut fresh)?;
        let backup = Backup::new(&fresh, &mut self.connection).map_err(storage_error)?;
        match backup.step(-1).map_err(storage_error)? {
            StepResult::Done => Ok(()),
            _ => Err(storage_error("База зайнята. Спробуйте ще раз.")),
        }
    }

    fn checkpoint_storage(&self) -> Result<()> {
        let busy: bool = self.connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(storage_error)?;
        if busy {
            return Err(storage_error("Дані очищено, але база зайнята. Спробуйте ще раз, щоб звільнити місце."));
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
        let document = store
            .import("reader.txt", b"Bonjour le monde.", "fr")
            .unwrap();
        store
            .update_reader_preference(ReaderPreferenceChange::BackgroundTranslation { value: true })
            .unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        store
            .advance_background_translation(&document.id, revision, "uk")
            .unwrap();
        let progress_count = |store: &Store| {
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM background_translation_progress",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap()
        };
        assert_eq!(progress_count(&store), 1);
        assert!(store.storage_usage().unwrap().database_cache_bytes > 0);
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
        assert!(usage.translation_cache_bytes > 0);
        assert!(usage.translation_cache_bytes <= usage.database_cache_bytes);

        store.connection.execute_batch(
            "CREATE TEMP TRIGGER reject_cache_delete BEFORE DELETE ON translation_cache
             BEGIN SELECT RAISE(ABORT, 'test cache deletion failure'); END;",
        ).unwrap();
        assert!(store.clear_storage(StorageScope::Cache).is_err());
        assert_eq!(store.reader_preferences().unwrap().background_translation, Some(true));
        assert_eq!(progress_count(&store), 1);
        store.connection.execute_batch("DROP TRIGGER reject_cache_delete;").unwrap();

        store.clear_storage(StorageScope::Cache).unwrap();
        assert_eq!(progress_count(&store), 0);
        assert_eq!(store.reader_preferences().unwrap().background_translation, Some(false));
        store.advance_background_translation(&document.id, revision, "uk").unwrap();
        assert_eq!(progress_count(&store), 0);
        assert_eq!(store.snapshot().unwrap().documents.len(), 1);
        let usage = store.storage_usage().unwrap();
        assert_eq!(usage.database_cache_bytes, 0);
        assert_eq!(usage.translation_cache_bytes, 0);
        assert_eq!(usage.audio_cache_bytes, 0);
        assert_eq!(usage.download_bytes, 128);

        store.clear_storage(StorageScope::Downloads).unwrap();
        assert_eq!(store.snapshot().unwrap().documents.len(), 1);
        assert_eq!(store.storage_usage().unwrap().download_bytes, 0);

        store.connection.execute_batch(
            "CREATE TABLE retired_feature (value TEXT); INSERT INTO retired_feature VALUES ('old');"
        ).unwrap();
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("model.onnx"), [0; 128]).unwrap();
        let other_store = Store::open(&path).unwrap();
        store.clear_storage(StorageScope::All).unwrap();
        assert!(store.snapshot().unwrap().documents.is_empty());
        assert!(store.reader_preferences().unwrap().is_empty());
        assert_eq!(store.storage_usage().unwrap().download_bytes, 0);
        assert!(other_store.snapshot().unwrap().documents.is_empty());
        assert!(!store.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='retired_feature')",
            [],
            |row| row.get::<_, bool>(0),
        ).unwrap());
        assert!(root.path().join("unrelated.txt").is_file());
        drop(store);
        assert!(Store::open(&path)
            .unwrap()
            .snapshot()
            .unwrap()
            .documents
            .is_empty());
        let mut reopened = Store::open(&path).unwrap();
        reopened.import("new.txt", b"A new document.", "en").unwrap();
        assert_eq!(reopened.snapshot().unwrap().documents.len(), 1);
    }
}
