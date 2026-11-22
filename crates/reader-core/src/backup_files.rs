use crate::{
    backup_limits::{LimitedReader, LimitedWriter},
    dictionary_transfer, document_revisions, practice, reading_history, Backup, BackupResource,
    Document, Original, Result, Snapshot, SpeechResume, Store,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{
    ser::{Error as _, SerializeSeq},
    Serialize, Serializer,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
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

/// A destination can already contain this source under a different document ID.
pub(crate) fn merge_existing_document(
    db: &Connection,
    incoming: &Document,
    speech_resume: Option<SpeechResume>,
) -> Result<bool> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM documents WHERE source_hash=?1",
            [&incoming.source_hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(archive_error)?;
    let Some(body) = body else {
        return Ok(false);
    };
    let mut local: Document = serde_json::from_str(&body).map_err(archive_error)?;
    if local.language != incoming.language
        || local.text != incoming.text
        || local.sentences.len() != incoming.sentences.len()
        || local
            .sentences
            .iter()
            .zip(&incoming.sentences)
            .any(|(local, incoming)| {
                local.start_utf8 != incoming.start_utf8
                    || local.end_utf8 != incoming.end_utf8
                    || local.text != incoming.text
            })
    {
        // A different OCR transcript or sentence segmentation can refer to different text.
        return Ok(true);
    }

    let ids: HashMap<_, _> = incoming
        .sentences
        .iter()
        .zip(&local.sentences)
        .map(|(incoming, local)| (incoming.id.as_str(), local.id.as_str()))
        .collect();
    if !incoming.bookmarks.is_empty() {
        local.bookmarks.extend(
            incoming
                .bookmarks
                .iter()
                .filter_map(|id| ids.get(id.as_str()).map(|id| (*id).to_owned())),
        );
        local.bookmarks.sort_unstable();
        local.bookmarks.dedup();
    }
    merge_sentence_text(&mut local.notes, &incoming.notes, &ids);
    merge_sentence_text(
        &mut local.speech_text_overrides,
        &incoming.speech_text_overrides,
        &ids,
    );
    for (target, translations) in &incoming.reviewed_translations {
        merge_sentence_text(
            local
                .reviewed_translations
                .entry(target.clone())
                .or_default(),
            translations,
            &ids,
        );
    }
    if local.position == 0 {
        local.position = incoming.position;
    }
    let speech_resume = speech_resume.and_then(|mut resume| {
        resume.sentence_id = (*ids.get(resume.sentence_id.as_str())?).to_owned();
        Some(resume)
    });
    if let Some(resume) = speech_resume.filter(|resume| local.matches_speech_resume(resume)) {
        if !Store::read_speech_resume(db, &local.id)?
            .is_some_and(|current| local.matches_speech_resume(&current))
        {
            Store::write_speech_resume(db, &local.id, &resume)?;
        }
    }
    let body = serde_json::to_string(&local).map_err(archive_error)?;
    db.execute(
        "UPDATE documents SET body=?1 WHERE id=?2 AND body<>?1",
        params![body, local.id],
    )
    .map_err(archive_error)?;
    Ok(true)
}

fn merge_sentence_text(
    local: &mut BTreeMap<String, String>,
    incoming: &BTreeMap<String, String>,
    sentence_ids: &HashMap<&str, &str>,
) {
    for (id, text) in incoming {
        if let Some(&local_id) = sentence_ids.get(id.as_str()) {
            local
                .entry(local_id.to_owned())
                .or_insert_with(|| text.clone());
        }
    }
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
        let backup = read_backup_file(path)?;
        // Keep the success response within the small ABI envelope before committing data.
        validate_resources(&backup.resources)?;
        let resources = backup.resources.clone();
        self.restore(backup)?;
        Ok(resources)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspection {
    pub documents: usize,
    pub vocabulary: usize,
    pub reviews: usize,
    pub reading_sessions: usize,
    pub resources: usize,
    pub has_practice: bool,
}

/// Read archive counts and verify every original without changing a library.
pub fn inspect_backup_file(path: &Path) -> Result<BackupInspection> {
    let backup = read_backup_file(path)?;
    validate_resources(&backup.resources)?;
    checked_originals(
        backup.version,
        &backup.snapshot.documents,
        &backup.originals,
    )?;
    Ok(BackupInspection {
        documents: backup.snapshot.documents.len(),
        vocabulary: backup.snapshot.vocabulary.len(),
        reviews: backup.reviews.len(),
        reading_sessions: backup.reading_sessions.len(),
        resources: backup.resources.len(),
        has_practice: backup
            .practice
            .is_some_and(|progress| progress.question.is_some()),
    })
}

pub(crate) fn checked_originals<'a>(
    version: u32,
    documents: &[Document],
    originals: &'a [Original],
) -> Result<HashMap<&'a str, &'a Original>> {
    if version != 1 {
        return Err("Невідома версія архіву".into());
    }

    let mut indexed = HashMap::new();
    for original in originals {
        if indexed
            .insert(original.document_id.as_str(), original)
            .is_some()
        {
            return Err("В архіві є кілька оригіналів для одного документа".into());
        }
    }

    let mut seen = HashSet::new();
    for document in documents {
        if !seen.insert(document.id.as_str()) {
            return Err("В архіві є кілька документів з однаковим ідентифікатором".into());
        }
        let original = indexed
            .get(document.id.as_str())
            .ok_or("В архіві немає оригіналу")?;
        if format!("{:x}", Sha256::digest(&original.bytes)) != document.source_hash {
            return Err("Пошкоджений оригінал в архіві".into());
        }
    }
    if seen.len() != indexed.len() {
        return Err("В архіві є оригінал без документа".into());
    }
    Ok(indexed)
}

fn read_backup_file(path: &Path) -> Result<Backup> {
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
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_counts_originals_and_rejects_ambiguous_or_corrupt_data() {
        let root = tempfile::tempdir().unwrap();
        let database_path = root.path().join("reader.db");
        let archive_path = root.path().join("backup.json");
        let mut source = Store::open(&database_path).unwrap();
        source.import("note.txt", b"One sentence.", "en").unwrap();
        source.export_backup_file(&archive_path).unwrap();

        let summary = inspect_backup_file(&archive_path).unwrap();
        assert_eq!(summary.documents, 1);
        assert_eq!(summary.vocabulary, 0);
        assert!(!summary.has_practice);

        let mut backup = read_backup_file(&archive_path).unwrap();
        backup.originals.push(backup.originals[0].clone());
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("кілька оригіналів"));

        backup.originals.pop();
        backup.originals[0].bytes[0] ^= 1;
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("Пошкоджений оригінал"));

        let mut destination = Store::open(&root.path().join("destination.db")).unwrap();
        assert!(destination.restore_backup_file(&archive_path).is_err());
        assert!(destination.snapshot().unwrap().documents.is_empty());
    }
}
