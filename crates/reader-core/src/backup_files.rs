use crate::{
    backup_limits::{LimitedReader, LimitedWriter},
    dictionary_transfer, document_layout, document_revisions, now, ocr, pdf_layers, pdf_reading_position, practice,
    reading_history, segment, valid_language, validate_sentence_note, validate_speech_text,
    validate_translation, Backup, BackupResource, Document, Original, Result, Snapshot,
    SpeechResume, Store,
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
    io::{BufReader, BufWriter, Read, Seek, Write},
    path::Path,
};

pub const MAX_BACKUP_FILE_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) fn archive_error(error: impl std::fmt::Display) -> String {
    format!("Помилка резервної копії: {error}")
}

fn validate_path(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) || path.file_name().is_none() {
        return Err("Некоректний шлях резервної копії.".into());
    }
    Ok(())
}

pub(crate) fn validate_resources(resources: &[BackupResource]) -> Result<()> {
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
    if local.language != incoming.language {
        return Ok(true);
    }
    let identical_sentences = local.text == incoming.text
        && local.sentences.len() == incoming.sentences.len()
        && local.sentences.iter().zip(&incoming.sentences).all(|(local, incoming)| {
            local.start_utf8 == incoming.start_utf8
                && local.end_utf8 == incoming.end_utf8
                && local.text == incoming.text
        });
    let ids: HashMap<_, _> = if identical_sentences {
        incoming
            .sentences
            .iter()
            .zip(&local.sentences)
            .map(|(incoming, local)| (incoming.id.as_str(), local.id.as_str()))
            .collect()
    } else if let Some(ids) = ocr::corresponding_pdf_sentences(incoming, &local.format, local.pdf_page_starts.as_deref(), &local.sentences) {
        ids
    } else {
        return Ok(true);
    };
    if identical_sentences && local.format == "docx" && incoming.docx_layout_version > local.docx_layout_version {
        local.blocks.clone_from(&incoming.blocks);
        local.docx_layout_version = incoming.docx_layout_version;
    }
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
    if local.position == 0 && local.reading_token_utf16.is_none() {
        if let Some(position) = incoming
            .sentences
            .get(incoming.position)
            .and_then(|sentence| ids.get(sentence.id.as_str()))
            .and_then(|id| local.sentences.iter().position(|sentence| sentence.id == *id))
        {
            local.position = position;
            local.reading_token_utf16 = incoming.reading_token_utf16;
        }
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

/// Insert a validated document and its metadata, or merge it into a matching source.
pub(crate) fn restore_document(
    db: &Connection,
    mut document: Document,
    history: Vec<document_revisions::DocumentRevision>,
) -> Result<Option<String>> {
    let speech_resume = document.speech_resume.take();
    if merge_existing_document(db, &document, speech_resume.clone())? {
        return Ok(None);
    }
    let body = serde_json::to_string(&document).map_err(archive_error)?;
    let inserted = db
        .execute(
            "INSERT INTO documents VALUES (?1,?2,?3) ON CONFLICT(id) DO NOTHING",
            params![document.id, body, document.source_hash],
        )
        .map_err(archive_error)?;
    if inserted == 0 {
        return Err("Ідентифікатор документа з копії вже належить іншому документу.".into());
    }
    if let Some(resume) = speech_resume {
        Store::write_speech_resume(db, &document.id, &resume)?;
    }
    for revision in history {
        revision.insert(db)?;
    }
    Ok(Some(document.id))
}

pub(crate) fn restore_library_data<O>(db: &Connection, backup: Backup<O>) -> Result<()> {
    crate::translation::validate_cache(&backup.translation_cache, &backup.snapshot.documents)?;
    let review_cards = dictionary_transfer::restore_backup_dictionary(
        db,
        backup.snapshot.vocabulary,
        backup.reviews,
        backup.dictionary_review_origins,
        backup.dictionary_sources,
        &backup.snapshot.documents,
    )?;
    reading_history::restore(db, backup.reading_sessions, &backup.snapshot.documents)?;
    pdf_reading_position::restore(db, backup.pdf_reading_positions, &backup.snapshot.documents)?;
    crate::reader_preferences::restore(db, backup.reader_preferences)?;
    crate::translation::restore_cache(db, backup.translation_cache)?;
    if let Some(progress) = backup.practice {
        practice::restore_if_fresh(db, progress, &review_cards)?;
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

pub(crate) fn assemble<O>(
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
        pdf_reading_positions: pdf_reading_position::export(tx)?,
        reader_preferences: crate::reader_preferences::read(tx)?,
        model_hints: None,
        pending_intake: Vec::new(),
        translation_cache: Vec::new(),
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

    /// Publish a complete archive without overwriting any existing destination.
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
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            crate::backup_zip::write(self, output.as_file_mut())?;
        } else {
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
            tx.commit().map_err(archive_error)?;
        }
        output.as_file().sync_all().map_err(archive_error)?;
        output.persist_noclobber(path).map_err(archive_error)?;
        Ok(())
    }

    /// Restore legacy JSON or ZIP transactionally.
    pub fn restore_backup_file(&mut self, path: &Path) -> Result<Vec<BackupResource>> {
        let (file, zip) = open_backup_file(path)?;
        if zip {
            return crate::backup_zip::restore(self, file);
        }
        let backup = read_backup_json(file)?;
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
    pub pending_intake: usize,
    pub has_practice: bool,
}

/// Read archive counts and validate its documents without changing a library.
pub fn inspect_backup_file(path: &Path) -> Result<BackupInspection> {
    let (file, zip) = open_backup_file(path)?;
    if zip {
        return crate::backup_zip::inspect(file);
    }
    let backup = read_backup_json(file)?;
    validate_resources(&backup.resources)?;
    for document in prepare_documents(&backup)? {
        document?;
    }
    Ok(inspection(&backup))
}

pub(crate) fn inspection<O>(backup: &Backup<O>) -> BackupInspection {
    BackupInspection {
        documents: backup.snapshot.documents.len(),
        vocabulary: backup.snapshot.vocabulary.len(),
        reviews: backup.reviews.len(),
        reading_sessions: backup.reading_sessions.len(),
        resources: backup.resources.len(),
        pending_intake: backup.pending_intake.len(),
        has_practice: backup
            .practice
            .as_ref()
            .is_some_and(|progress| progress.question.is_some()),
    }
}

fn checked_originals<'a>(
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
    let mut source_hashes = HashSet::new();
    for document in documents {
        if !seen.insert(document.id.as_str()) {
            return Err("В архіві є кілька документів з однаковим ідентифікатором".into());
        }
        if !source_hashes.insert(document.source_hash.as_str()) {
            return Err("В архіві є кілька документів з однаковим оригіналом".into());
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

pub(crate) struct PreparedDocument<'a> {
    pub document: Document,
    pub original: &'a Original,
    pub history: Vec<document_revisions::DocumentRevision>,
}

/// Reparse originals and validate their revision and annotation references.
pub(crate) fn prepare_documents(
    backup: &Backup,
) -> Result<impl Iterator<Item = Result<PreparedDocument<'_>>>> {
    backup.reader_preferences.validate()?;
    let documents = &backup.snapshot.documents;
    pdf_reading_position::validate(&backup.pdf_reading_positions, documents)?;
    let originals = checked_originals(backup.version, documents, &backup.originals)?;
    let mut revisions = revision_groups(backup, originals.keys().copied())?;
    Ok(documents.iter().map(move |document| {
        prepare_document(
            document,
            originals[document.id.as_str()],
            revisions.remove(document.id.as_str()),
        )
    }))
}

pub(crate) fn revision_groups<'a, O>(
    backup: &'a Backup<O>,
    document_ids: impl IntoIterator<Item = &'a str>,
) -> Result<HashMap<&'a str, Vec<document_revisions::DocumentRevision>>> {
    let document_ids: HashSet<_> = document_ids.into_iter().collect();
    let mut revisions: HashMap<&str, Vec<_>> = HashMap::new();
    for revision in &backup.document_revisions {
        if !document_ids.contains(revision.document_id.as_str()) {
            return Err("В архіві є ревізії без документа".into());
        }
        revisions
            .entry(&revision.document_id)
            .or_default()
            .push(revision.clone());
    }
    Ok(revisions)
}

pub(crate) fn prepare_document<'a>(
    doc: &Document,
    original: &'a Original,
    history: Option<Vec<document_revisions::DocumentRevision>>,
) -> Result<PreparedDocument<'a>> {
    let parsed = match (
        &doc.ocr_lines,
        doc.ocr_pdf_pages,
        doc.pdf_page_starts.is_some(),
    ) {
        (Some(lines), Some(pages), true) => {
            let (text, offsets) = pdf_layers::merge(&original.name, &original.bytes, pages, lines)?;
            document_layout::from_transcript(text, Some(offsets))
        }
        (Some(lines), pages, false) => document_layout::from_ocr(
            ocr::recognized_text(&original.name, &original.bytes, lines, pages)?,
            lines,
            pages,
        ),
        (None, None, _) => document_layout::parse(&original.name, &original.bytes)?,
        _ => return Err("В архіві PDF немає потрібних даних сторінок".into()),
    };
    let (document, history) = prepare_layout(doc, parsed, history)?;
    Ok(PreparedDocument {
        document,
        original,
        history,
    })
}

pub(crate) fn prepare_layout(
    doc: &Document,
    parsed: document_layout::Extracted,
    history: Option<Vec<document_revisions::DocumentRevision>>,
) -> Result<(Document, Vec<document_revisions::DocumentRevision>)> {
    valid_language(&doc.language)?;
    let mut d = doc.clone();
    let mut history = match history {
        Some(history) => document_revisions::validate_chain(&d, history)?,
        None => Vec::new(),
    };
    let text_changed = if let Some(current) = history
        .last()
        .filter(|current| !current.matches_text(&parsed.text))
    {
        if !current.matches_text(&d.text) {
            return Err("Архів не містить підтвердженого тексту поточної ревізії".into());
        }
        d.sentences = segment(&d.text, &d.id);
        ocr::update_transcript(&mut d, parsed.text)?;
        true
    } else {
        if d.text != parsed.text {
            d.reading_token_utf16 = None;
        }
        d.text = parsed.text;
        d.sentences = segment(&d.text, &d.id);
        false
    };
    d.blocks = parsed.blocks;
    d.docx_layout_version = (d.format == "docx").then_some(document_layout::DOCX_LAYOUT_VERSION);
    d.pdf_page_starts = parsed.pdf_page_starts;
    ocr::refresh_provenance(&mut d);
    if history.is_empty() || text_changed {
        let parser = document_revisions::parser_version(&d);
        let created_at = if history.is_empty() {
            d.current_revision_id = None;
            d.created_at
        } else {
            now()
        };
        history.push(document_revisions::DocumentRevision::capture(
            &mut d, parser, created_at,
        ));
    }
    d.reading_token_utf16 = crate::document_tokens::validated_cursor_offset(&d);
    d.position = d.position.min(d.sentences.len().saturating_sub(1));
    d.speech_resume = d
        .speech_resume
        .take()
        .filter(|resume| d.matches_speech_resume(resume));
    if !d.bookmarks.is_empty()
        || !d.reviewed_translations.is_empty()
        || !d.notes.is_empty()
        || !d.speech_text_overrides.is_empty()
    {
        let known: HashSet<_> = d
            .sentences
            .iter()
            .map(|sentence| sentence.id.as_str())
            .collect();
        if d.bookmarks.iter().any(|id| !known.contains(id.as_str())) {
            return Err("В архіві є закладка для невідомого речення".into());
        }
        d.bookmarks.sort_unstable();
        d.bookmarks.dedup();
        for (sentence_id, note) in &d.notes {
            if !known.contains(sentence_id.as_str()) || note.trim().is_empty() {
                return Err("В архіві є нотатка для невідомого речення або порожня нотатка".into());
            }
            validate_sentence_note(note)?;
        }
        for (sentence_id, text) in &d.speech_text_overrides {
            if !known.contains(sentence_id.as_str()) || text.trim().is_empty() {
                return Err("Некоректний текст озвучення в архіві".into());
            }
            validate_speech_text(text)?;
        }
        for (target, segments) in &d.reviewed_translations {
            valid_language(target)?;
            if target == &d.language {
                return Err("Некоректна мова перекладу в архіві".into());
            }
            for (sentence_id, text) in segments {
                if !known.contains(sentence_id.as_str()) || text.trim().is_empty() {
                    return Err("Некоректний переклад речення в архіві".into());
                }
                validate_translation(text)?;
            }
        }
    }
    Ok((d, history))
}

fn open_backup_file(path: &Path) -> Result<(File, bool)> {
    validate_path(path)?;
    if !fs::symlink_metadata(path).map_err(archive_error)?.is_file() {
        return Err("Резервна копія має бути звичайним файлом.".into());
    }
    let mut file = File::open(path).map_err(archive_error)?;
    if !file.metadata().map_err(archive_error)?.is_file() {
        return Err("Резервна копія має бути звичайним файлом.".into());
    }
    let mut magic = [0; 4];
    let is_zip = file.read_exact(&mut magic).is_ok() && magic == *b"PK\x03\x04";
    file.rewind().map_err(archive_error)?;
    Ok((file, is_zip))
}

fn read_backup_json(file: File) -> Result<Backup> {
    let metadata = file.metadata().map_err(archive_error)?;
    if metadata.len() > MAX_BACKUP_FILE_BYTES {
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

        let mut backup = read_backup_json(File::open(&archive_path).unwrap()).unwrap();
        backup.originals.push(backup.originals[0].clone());
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("кілька оригіналів"));

        backup.originals.pop();
        backup.snapshot.documents[0]
            .bookmarks
            .push("missing-sentence".into());
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("закладка для невідомого речення"));

        backup.snapshot.documents[0].bookmarks.clear();
        backup.document_revisions[0].document_id = "missing-document".into();
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("ревізії без документа"));

        backup.document_revisions.clear();
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert_eq!(inspect_backup_file(&archive_path).unwrap().documents, 1);

        backup.originals[0].bytes[0] ^= 1;
        fs::write(&archive_path, serde_json::to_vec(&backup).unwrap()).unwrap();
        assert!(inspect_backup_file(&archive_path)
            .unwrap_err()
            .contains("Пошкоджений оригінал"));

        let mut destination = Store::open(&root.path().join("destination.db")).unwrap();
        assert!(destination.restore_backup_file(&archive_path).is_err());
        assert!(destination.snapshot().unwrap().documents.is_empty());
    }

    #[test]
    fn restore_rolls_back_when_a_document_id_belongs_to_different_content() {
        let root = tempfile::tempdir().unwrap();
        let mut destination = Store::open(&root.path().join("destination.db")).unwrap();
        let local = destination
            .import("local.txt", b"Local original.", "en")
            .unwrap();
        let mut source = Store::open(&root.path().join("source.db")).unwrap();
        let collision = source
            .import("collision.txt", b"Different original.", "en")
            .unwrap();
        let other = source
            .import("other.txt", b"Should roll back.", "en")
            .unwrap();

        let mut backup = source.export().unwrap();
        // Model a legacy archive with a copied ID and otherwise consistent source hashes.
        backup.document_revisions.clear();
        assert_eq!(backup.snapshot.documents[0].id, other.id);
        backup.snapshot.documents[1].id.clone_from(&local.id);
        let original = backup
            .originals
            .iter_mut()
            .find(|original| original.document_id == collision.id)
            .unwrap();
        original.document_id.clone_from(&local.id);

        let error = destination.restore(backup).unwrap_err();
        assert!(error.contains("вже належить іншому документу"));
        let documents = destination.snapshot().unwrap().documents;
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].id, local.id);
        assert_eq!(
            destination.original(&local.id).unwrap().bytes,
            b"Local original."
        );
    }
}
