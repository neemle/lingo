use crate::{
    backup_files::{
        archive_error, assemble, inspection, prepare_document, prepare_layout, restore_document,
        restore_library_data, revision_groups, validate_resources, MAX_BACKUP_FILE_BYTES,
    },
    backup_limits::{LimitedReader, LimitedWriter},
    document_layout,
    document_revisions::DocumentRevision,
    file_originals, intake::archive as intake_archive, pdf_detached_pages, pdf_layers, pdf_page_ocr, Backup, BackupInspection, BackupResource, Document,
    Original,
    Result, Store, MAX_IMPORT,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;
use zip::{
    read::{ArchiveOffset, Config, ZipFile},
    write::SimpleFileOptions,
    CompressionMethod, ZipArchive, ZipWriter,
};

mod detached_archive;
mod model_hint_archive;

const VERSION: u32 = 2;
const PAGE_CACHE_VERSION: u32 = 3;
const MODEL_HINT_VERSION: u32 = 4;
const INTAKE_VERSION: u32 = 5;
const MANIFEST: &str = "manifest.json";
const MAX_DOCUMENTS: usize = 32_768;
const MAX_ARCHIVE_ENTRIES: usize = u16::MAX as usize - 1;
const MAX_DIRECTORY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OriginalEntry {
    document_id: String,
    name: String,
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pdf_page_text: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ocr_completed_pages: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detached_cache: Option<detached_archive::Metadata>,
}

type Manifest = Backup<Vec<OriginalEntry>>;

fn entry_name(index: usize) -> String {
    format!("originals/{index}")
}

pub(crate) fn write(store: &Store, output: &mut File) -> Result<()> {
    let _intake = store.lock_intake()?;
    let tx = store
        .connection
        .unchecked_transaction()
        .map_err(archive_error)?;
    let snapshot = store.snapshot()?;
    if snapshot.documents.len() > MAX_DOCUMENTS {
        return Err("Забагато документів для резервної копії.".into());
    }
    let mut sources = Vec::with_capacity(snapshot.documents.len());
    let mut paths = Vec::with_capacity(snapshot.documents.len());
    for doc in &snapshot.documents {
        let (name, size, page_text, path) = if let Some(original) = store.original_file(&doc.id)? {
            let size = original.path.metadata().map_err(archive_error)?.len();
            let page_text = store
                .native_pdf_page_text(&doc.id)?
                .ok_or("У PDF немає тексту сторінок")?;
            (original.name, size, Some(page_text), Some(original.path))
        } else {
            let (name, size): (String, i64) = tx
                .query_row(
                    "SELECT name,length(bytes) FROM originals WHERE document_id=?1",
                    [&doc.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(archive_error)?;
            (
                name,
                u64::try_from(size).map_err(archive_error)?,
                None,
                None,
            )
        };
        let ocr_completed_pages = if page_text.is_some() {
            pdf_page_ocr::completed_pages(&tx, &doc.id)?
        } else {
            Vec::new()
        };
        sources.push(OriginalEntry {
            document_id: doc.id.clone(),
            name,
            size,
            pdf_page_text: page_text,
            ocr_completed_pages,
            detached_cache: None,
        });
        paths.push(path);
    }
    let mut backup = assemble(store, &tx, snapshot, sources)?;
    backup.version = VERSION;
    backup.translation_cache = crate::translation::export_cache(&tx)?;
    let mut zip = ZipWriter::new(BufWriter::with_capacity(64 * 1024, output));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (index, (entry, document)) in backup.originals.iter_mut().zip(&backup.snapshot.documents).enumerate() {
        if entry.pdf_page_text.is_some() && pdf_detached_pages::has_cached_pages(&tx, &document.id)? {
            zip.start_file(detached_archive::entry_name(index), stored.large_file(true)).map_err(archive_error)?;
            entry.detached_cache = Some(detached_archive::write(&tx, document, &mut zip)?);
            backup.version = PAGE_CACHE_VERSION;
        }
    }
    if model_hint_archive::present(&tx)? {
        zip.start_file(model_hint_archive::ENTRY, stored).map_err(archive_error)?;
        backup.model_hints = Some(model_hint_archive::write(&tx, &mut zip)?);
        backup.version = MODEL_HINT_VERSION;
    }
    backup.pending_intake = intake_archive::pending_ids(
        &tx, MAX_ARCHIVE_ENTRIES.saturating_sub(archive_entry_count(&backup)),
    )?;
    if !backup.pending_intake.is_empty() {
        backup.version = INTAKE_VERSION;
    }
    validate_manifest(&backup)?;
    for id in &backup.pending_intake {
        zip.start_file(intake_archive::entry_name(id), stored.large_file(true))
            .map_err(archive_error)?;
        intake_archive::write(store, &tx, id, &mut zip)?;
    }
    zip.start_file(MANIFEST, stored).map_err(archive_error)?;
    serde_json::to_writer(LimitedWriter::new(&mut zip, MAX_BACKUP_FILE_BYTES), &backup)
        .map_err(archive_error)?;
    for (index, ((entry, doc), path)) in backup
        .originals
        .iter()
        .zip(&backup.snapshot.documents)
        .zip(paths)
        .enumerate()
    {
        zip.start_file(
            entry_name(index),
            stored.large_file(entry.size >= u32::MAX as u64),
        )
        .map_err(archive_error)?;
        if let Some(path) = path {
            let input = File::open(path).map_err(archive_error)?;
            copy_original(input, &mut zip, entry.size, &doc.source_hash, true)?;
        } else {
            let original = store.original(&doc.id)?;
            copy_original(
                &original.bytes[..],
                &mut zip,
                entry.size,
                &doc.source_hash,
                false,
            )?;
        }
    }
    zip.finish()
        .map_err(archive_error)?
        .flush()
        .map_err(archive_error)?;
    tx.commit().map_err(archive_error)?;
    Ok(())
}

pub(crate) fn inspect(file: File) -> Result<BackupInspection> {
    let (mut zip, backup) = open(file)?;
    validate_entries(&mut zip, &backup, None)?;
    for id in &backup.pending_intake {
        let entry = checked_entry(&mut zip, &intake_archive::entry_name(id))?;
        intake_archive::inspect(entry.size(), entry)?;
    }
    Ok(inspection(&backup))
}

pub(crate) fn restore(store: &mut Store, file: File) -> Result<Vec<BackupResource>> {
    let (mut zip, backup) = open(file)?;
    let _intake = (!backup.pending_intake.is_empty()).then(|| store.lock_intake()).transpose()?;
    let root = &store.file_original_directory;
    let documents = validate_entries(&mut zip, &backup, Some(root))?;
    let resources = backup.resources.clone();

    // The SQLite write transaction is dropped before this guard on every failure.
    let mut published = PublishedFiles::default();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(archive_error)?;
    for (index, ((document, history, staged), entry)) in
        documents.into_iter().zip(&backup.originals).enumerate()
    {
        let hash = document.source_hash.clone();
        if let Some(pages) = entry.pdf_page_text.as_deref() {
            pdf_page_ocr::merge_backup_cache(&tx, &document, pages, &entry.ocr_completed_pages)?;
        }
        let Some(id) = restore_document(&tx, document, history)? else {
            if entry.detached_cache.is_some() {
                let local = existing_file_pdf(&tx, &hash)?;
                restore_detached_cache(&mut zip, &tx, index, entry, &backup.snapshot.documents[index], local.as_ref())?;
            }
            continue;
        };
        if let Some(staged) = staged {
            let content = serde_json::to_string(&entry.pdf_page_text).map_err(archive_error)?;
            tx.execute(
                "INSERT INTO file_originals VALUES (?1,?2,?3,?4)",
                params![id, entry.name, entry.size as i64, content],
            )
            .map_err(archive_error)?;
            for &page in &entry.ocr_completed_pages {
                pdf_page_ocr::mark_completed(&tx, &id, page)?;
            }
            let archive_document = &backup.snapshot.documents[index];
            if entry.detached_cache.is_some() {
                restore_detached_cache(&mut zip, &tx, index, entry, archive_document, Some(archive_document))?;
            }
            file_originals::verify_root(root)?;
            let path = file_originals::original_path(root, &id)?;
            staged.persist_noclobber(&path).map_err(archive_error)?;
            published.paths.push(path);
        } else {
            let bytes = read_inline(&mut zip, index, entry, &hash)?;
            tx.execute(
                "INSERT INTO originals VALUES (?1,?2,?3)",
                params![id, entry.name, bytes],
            )
            .map_err(archive_error)?;
        }
    }
    restore_model_hints(&mut zip, &tx, &backup)?;
    for id in &backup.pending_intake {
        let entry = checked_entry(&mut zip, &intake_archive::entry_name(id))?;
        if let Some(path) = intake_archive::restore(&tx, root, id, entry.size(), entry)? {
            published.paths.push(path);
        }
    }
    restore_library_data(&tx, backup)?;
    if !published.paths.is_empty() {
        file_originals::sync_directory(root)?;
    }
    tx.commit().map_err(archive_error)?;
    published.paths.clear();
    Ok(resources)
}

fn restore_model_hints(
    zip: &mut ZipArchive<BufReader<File>>, db: &Connection, backup: &Manifest,
) -> Result<()> {
    let mut local_ids = HashMap::new();
    let mut find = db.prepare_cached("SELECT id FROM documents WHERE source_hash=?1").map_err(archive_error)?;
    if backup.model_hints.is_some() {
        for document in &backup.snapshot.documents {
            let id: String = find.query_row([&document.source_hash], |row| row.get(0)).map_err(archive_error)?;
            local_ids.insert(document.id.as_str(), id);
        }
    }
    read_model_hints(zip, backup, |row| {
        let local = local_ids.get(row.document_id.as_str()).ok_or("Документ для підказки відсутній.")?;
        row.restore(db, local)
    })
}

fn read_model_hints(
    zip: &mut ZipArchive<BufReader<File>>, backup: &Manifest,
    mut visit: impl FnMut(model_hint_archive::Row) -> Result<()>,
) -> Result<()> {
    let Some(metadata) = &backup.model_hints else { return Ok(()); };
    let data = checked_entry(zip, model_hint_archive::ENTRY)?;
    if data.size() != metadata.bytes {
        return Err("Розмір архіву підказок не відповідає маніфесту.".into());
    }
    let documents: HashSet<_> = backup.snapshot.documents.iter().map(|doc| doc.id.as_str()).collect();
    model_hint_archive::read(data, metadata, |row| {
        if !documents.contains(row.document_id.as_str()) {
            return Err("Документ для підказки відсутній у резервній копії.".into());
        }
        visit(row)
    })
}

fn existing_file_pdf(db: &Connection, hash: &str) -> Result<Option<Document>> {
    let body: Option<String> = db.query_row(
        "SELECT documents.body FROM documents JOIN file_originals ON file_originals.document_id=documents.id
         WHERE documents.source_hash=?1",
        [hash],
        |row| row.get(0),
    ).optional().map_err(archive_error)?;
    body.as_deref().map(serde_json::from_str).transpose().map_err(archive_error)
}

fn restore_detached_cache(
    zip: &mut ZipArchive<BufReader<File>>,
    db: &Connection,
    index: usize,
    entry: &OriginalEntry,
    archive_document: &Document,
    local: Option<&Document>,
) -> Result<()> {
    read_detached_cache(zip, index, entry, archive_document, |page| {
        if let Some(document) = local.filter(|document| document.language == archive_document.language) {
            pdf_detached_pages::adopt_cached_page(db, document, page)?;
        }
        Ok(())
    })
}

#[derive(Default)]
struct PublishedFiles {
    paths: Vec<PathBuf>,
}

impl Drop for PublishedFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}

fn open(mut file: File) -> Result<(ZipArchive<BufReader<File>>, Manifest)> {
    let (directory, count) = check_directory(&mut file)?;
    let mut zip = ZipArchive::with_config(
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
        BufReader::new(file),
    )
    .map_err(archive_error)?;
    if zip.central_directory_start() != directory
        || zip.len() != count
        || zip.has_overlapping_files().map_err(archive_error)?
    {
        return Err("Некоректний вміст ZIP.".into());
    }
    let backup: Manifest = {
        let manifest = checked_entry(&mut zip, MANIFEST)?;
        if manifest.size() > MAX_BACKUP_FILE_BYTES {
            return Err("Маніфест резервної копії завеликий.".into());
        }
        serde_json::from_reader(BufReader::with_capacity(
            64 * 1024,
            LimitedReader::new(manifest, MAX_BACKUP_FILE_BYTES),
        ))
        .map_err(archive_error)?
    };
    validate_manifest(&backup)?;
    if zip.len() != archive_entry_count(&backup) {
        return Err("Файли в ZIP не відповідають маніфесту.".into());
    }
    Ok((zip, backup))
}

fn archive_entry_count(backup: &Manifest) -> usize {
    1 + backup.originals.len()
        + backup.originals.iter().filter(|entry| entry.detached_cache.is_some()).count()
        + usize::from(backup.model_hints.is_some())
        + backup.pending_intake.len()
}

fn validate_manifest(backup: &Manifest) -> Result<()> {
    if !matches!(backup.version, VERSION | PAGE_CACHE_VERSION | MODEL_HINT_VERSION | INTAKE_VERSION) {
        return Err("Невідома версія резервної копії.".into());
    }
    if archive_entry_count(backup) > MAX_ARCHIVE_ENTRIES {
        return Err("У резервній копії забагато записів.".into());
    }
    if (backup.version == MODEL_HINT_VERSION && backup.model_hints.is_none())
        || (backup.version < MODEL_HINT_VERSION && backup.model_hints.is_some())
        || backup.model_hints.as_ref().is_some_and(|metadata| !metadata.valid())
    {
        return Err("Некоректний опис архіву збережених підказок.".into());
    }
    if (backup.version == INTAKE_VERSION) == backup.pending_intake.is_empty() {
        return Err("Некоректний опис черги імпорту в резервній копії.".into());
    }
    intake_archive::validate_ids(&backup.pending_intake)?;
    crate::translation::validate_cache(&backup.translation_cache, &backup.snapshot.documents)?;
    validate_resources(&backup.resources)?;
    backup.reader_preferences.validate()?;
    crate::pdf_reading_position::validate(&backup.pdf_reading_positions, &backup.snapshot.documents)?;
    if backup.originals.len() != backup.snapshot.documents.len()
        || backup.originals.len() > MAX_DOCUMENTS
    {
        return Err("Оригінали не відповідають документам резервної копії.".into());
    }
    let mut ids = HashSet::new();
    let mut hashes = HashSet::new();
    for (entry, doc) in backup.originals.iter().zip(&backup.snapshot.documents) {
        if entry.document_id != doc.id
            || !ids.insert(doc.id.as_str())
            || !hashes.insert(doc.source_hash.as_str())
            || doc.source_hash.len() != 64
            || !doc
                .source_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || entry.name.is_empty()
            || entry.name.len() > 4096
            || entry.name.chars().any(char::is_control)
            || entry.size == 0
        {
            return Err("Некоректний запис оригіналу в резервній копії.".into());
        }
        if let Some(cache) = &entry.detached_cache {
            if backup.version < PAGE_CACHE_VERSION || entry.pdf_page_text.is_none()
                || cache.count == 0 || u16::try_from(doc.pdf_page_starts.as_ref().map_or(0, Vec::len))
                    .map_or(true, |pages| cache.count > pages)
                || cache.bytes == 0 || cache.sha256.len() != 64
                || !cache.sha256.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err("Некоректний опис кешу сторінок PDF.".into());
            }
        }
        if let Some(pages) = &entry.pdf_page_text {
            if entry.size > file_originals::MAX_PDF_BYTES
                || doc.format != "pdf"
                || file_originals::pdf_name(&entry.name)? != entry.name
                || !uuid::Uuid::parse_str(&doc.id).is_ok_and(|id| id.to_string() == doc.id)
                || u16::try_from(pages.len()).ok() != doc.ocr_pdf_pages
                || pages.is_empty()
                || doc.pdf_pending_text_pages.iter().any(|&page| {
                    !pages.get(usize::from(page)).is_some_and(|text| text.is_empty())
                        || entry.ocr_completed_pages.binary_search(&page).is_ok()
                })
                || doc.ocr_lines.iter().flatten().any(|line| doc.pdf_pending_text_pages.contains(&line.page))
                || entry.ocr_completed_pages.windows(2).any(|pair| pair[0] >= pair[1])
                || entry.ocr_completed_pages.iter().chain(&doc.pdf_ocr_refresh_pages).any(|&page| {
                    !pages.get(usize::from(page)).is_some_and(|text| text.trim().is_empty())
                })
            {
                return Err("Некоректний запис великого PDF у резервній копії.".into());
            }
        } else if entry.size > MAX_IMPORT as u64 || !entry.ocr_completed_pages.is_empty()
            || !doc.pdf_ocr_refresh_pages.is_empty() || !doc.pdf_pending_text_pages.is_empty()
        {
            return Err("Оригінал у резервній копії перевищує допустимий розмір.".into());
        }
    }
    Ok(())
}

type ValidatedDocument = (Document, Vec<DocumentRevision>, Option<NamedTempFile>);

fn validate_entries(
    zip: &mut ZipArchive<BufReader<File>>,
    backup: &Manifest,
    staging: Option<&Path>,
) -> Result<Vec<ValidatedDocument>> {
    read_model_hints(zip, backup, |_| Ok(()))?;
    let mut revisions = revision_groups(
        backup,
        backup.snapshot.documents.iter().map(|doc| doc.id.as_str()),
    )?;
    let mut documents = Vec::with_capacity(backup.originals.len());
    for (index, (entry, doc)) in backup
        .originals
        .iter()
        .zip(&backup.snapshot.documents)
        .enumerate()
    {
        let history = revisions.remove(doc.id.as_str());
        let prepared = if let Some(pages) = &entry.pdf_page_text {
            let mut staged = staging
                .map(|root| {
                    file_originals::ensure_root(root)?;
                    file_originals::stage_original(root)
                })
                .transpose()?;
            let data = original_entry(zip, index, entry)?;
            if let Some(file) = &mut staged {
                copy_original(data, file.as_file_mut(), entry.size, &doc.source_hash, true)?;
                file.as_file().sync_all().map_err(archive_error)?;
            } else {
                copy_original(data, io::sink(), entry.size, &doc.source_hash, true)?;
            }
            let lines = doc.ocr_lines.as_deref().ok_or("У PDF немає даних OCR")?;
            let (text, offsets) = pdf_layers::merge_page_text(pages.clone(), lines)?;
            let parsed = document_layout::from_transcript(text, Some(offsets));
            let (document, history) = prepare_layout(doc, parsed, history)?;
            (document, history, staged)
        } else {
            let original = Original {
                document_id: doc.id.clone(),
                name: entry.name.clone(),
                bytes: read_inline(zip, index, entry, &doc.source_hash)?,
            };
            let prepared = prepare_document(doc, &original, history)?;
            (prepared.document, prepared.history, None)
        };
        if staging.is_none() && entry.detached_cache.is_some() {
            read_detached_cache(zip, index, entry, &prepared.0, |_| Ok(()))?;
        }
        // Preview validates every document but does not retain staged copies or documents.
        if staging.is_some() {
            documents.push(prepared);
        }
    }
    Ok(documents)
}

fn read_detached_cache(
    zip: &mut ZipArchive<BufReader<File>>,
    index: usize,
    entry: &OriginalEntry,
    document: &Document,
    mut visit: impl FnMut(pdf_detached_pages::ArchivedPage) -> Result<()>,
) -> Result<()> {
    let Some(metadata) = &entry.detached_cache else { return Ok(()); };
    let pages = entry.pdf_page_text.as_deref().ok_or("У PDF немає розмітки сторінок.")?;
    let data = checked_entry(zip, &detached_archive::entry_name(index))?;
    if data.size() != metadata.bytes {
        return Err("Розмір кешу сторінок PDF не відповідає маніфесту.".into());
    }
    let ocr_pages: HashSet<_> = document.ocr_lines.iter().flatten().map(|line| line.page).collect();
    detached_archive::validate(data, metadata, document, |page| {
        if !pages.get(usize::from(page.page)).is_some_and(|text| text.trim().is_empty())
            || ocr_pages.contains(&page.page)
        {
            return Err("Кешована сторінка PDF дублює текст документа.".into());
        }
        visit(page)
    })
}

fn checked_entry<'a>(
    zip: &'a mut ZipArchive<BufReader<File>>,
    name: &str,
) -> Result<ZipFile<'a, BufReader<File>>> {
    let entry = zip.by_name(name).map_err(archive_error)?;
    if !entry.is_file()
        || entry.is_symlink()
        || entry.encrypted()
        || entry.compression() != CompressionMethod::Stored
        || entry.compressed_size() != entry.size()
    {
        return Err("Некоректний файл усередині ZIP.".into());
    }
    Ok(entry)
}

fn original_entry<'a>(
    zip: &'a mut ZipArchive<BufReader<File>>,
    index: usize,
    metadata: &OriginalEntry,
) -> Result<ZipFile<'a, BufReader<File>>> {
    let entry = checked_entry(zip, &entry_name(index))?;
    if entry.size() != metadata.size {
        return Err("Розмір оригіналу не відповідає резервній копії.".into());
    }
    Ok(entry)
}

fn read_inline(
    zip: &mut ZipArchive<BufReader<File>>,
    index: usize,
    entry: &OriginalEntry,
    hash: &str,
) -> Result<Vec<u8>> {
    let data = original_entry(zip, index, entry)?;
    let mut bytes = Vec::with_capacity(entry.size as usize);
    copy_original(data, &mut bytes, entry.size, hash, false)?;
    Ok(bytes)
}

fn copy_original(
    mut input: impl Read,
    mut output: impl Write,
    expected_size: u64,
    expected_hash: &str,
    pdf: bool,
) -> Result<()> {
    let corrupted = || "Пошкоджений оригінал у резервній копії.".to_owned();
    let mut digest = Sha256::new();
    let mut length = 0;
    if pdf {
        let mut signature = [0; 5];
        input.read_exact(&mut signature).map_err(archive_error)?;
        if signature != *b"%PDF-" {
            return Err(corrupted());
        }
        output.write_all(&signature).map_err(archive_error)?;
        digest.update(signature);
        length = signature.len() as u64;
    }
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(archive_error)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > expected_size {
            return Err(corrupted());
        }
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count]).map_err(archive_error)?;
    }
    if length != expected_size || format!("{:x}", digest.finalize()) != expected_hash {
        return Err(corrupted());
    }
    Ok(())
}

// Our writer emits single-disk archives with no comments or extension records.
// Bound the directory before passing the archive to the ZIP parser.
fn check_directory(file: &mut File) -> Result<(u64, usize)> {
    let invalid = || "Некоректний ZIP резервної копії.".to_owned();
    let length = file.metadata().map_err(archive_error)?.len();
    let end = length.checked_sub(22).ok_or_else(invalid)?;
    let footer = read_at::<22>(file, end)?;
    if footer[..8] != *b"PK\x05\x06\0\0\0\0" || footer[20..] != [0; 2] {
        return Err(invalid());
    }
    let inline_count = le::<2>(&footer, 10);
    if le::<2>(&footer, 8) != inline_count {
        return Err(invalid());
    }
    let size = le::<4>(&footer, 12);
    let offset = le::<4>(&footer, 16);
    let (start, bytes, count, directory_end) = if size == u32::MAX as u64 {
        let footer64_at = end.checked_sub(76).ok_or_else(invalid)?;
        let record = read_at::<56>(file, footer64_at)?;
        let locator = read_at::<20>(file, footer64_at + 56)?;
        if record[..4] != *b"PK\x06\x06"
            || le::<8>(&record, 4) != 44
            || record[16..24] != [0; 8]
            || locator[..8] != *b"PK\x06\x07\0\0\0\0"
            || le::<8>(&locator, 8) != footer64_at
            || le::<4>(&locator, 16) != 1
            || le::<8>(&record, 24) != inline_count
            || le::<8>(&record, 32) != inline_count
        {
            return Err(invalid());
        }
        (
            le::<8>(&record, 48),
            le::<8>(&record, 40),
            inline_count,
            footer64_at,
        )
    } else {
        (offset, size, inline_count, end)
    };
    if count == 0
        || count > MAX_ARCHIVE_ENTRIES as u64
        || !(1..=MAX_DIRECTORY_BYTES).contains(&bytes)
        || start.checked_add(bytes) != Some(directory_end)
        || read_at::<4>(file, start)? != *b"PK\x01\x02"
    {
        return Err(invalid());
    }
    file.rewind().map_err(archive_error)?;
    Ok((start, count as usize))
}

fn read_at<const N: usize>(file: &mut File, offset: u64) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    file.seek(SeekFrom::Start(offset)).map_err(archive_error)?;
    file.read_exact(&mut bytes).map_err(archive_error)?;
    Ok(bytes)
}

fn le<const N: usize>(source: &[u8], offset: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes[..N].copy_from_slice(&source[offset..offset + N]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect_backup_file;

    #[test]
    fn portable_archive_round_trips_large_pdf_and_legacy_originals() {
        let root = tempfile::tempdir().unwrap();
        let mut source = Store::open(&root.path().join("source.db")).unwrap();
        let input = root.path().join("large.pdf");
        let mut file = File::create(&input).unwrap();
        file.write_all(b"%PDF-1.7\n").unwrap();
        file.set_len(32 * 1024 * 1024).unwrap();
        drop(file);
        let prepared = file_originals::prepare_pdf_import(
            source.pdf_original_root(),
            "large.pdf",
            &input,
            "en",
            vec!["First page.".into(), "Second page.".into()],
            Vec::new(),
        )
        .unwrap();
        let large = source.import_pdf_file(prepared).unwrap();
        source.position(&large.id, 1).unwrap();
        source
            .set_bookmark(&large.id, &large.sentences[1].id, true)
            .unwrap();
        source
            .set_sentence_note(&large.id, &large.sentences[1].id, "Retain this note.")
            .unwrap();
        let small = source
            .import("old.txt", b"An older original.", "en")
            .unwrap();
        assert!(source
            .export_backup_file(&root.path().join("legacy.json"))
            .is_err());

        let exported = root.path().join("library.zip");
        source.export_backup_file(&exported).unwrap();
        let archive = root.path().join("picker.backup");
        fs::rename(exported, &archive).unwrap();
        assert_eq!(inspect_backup_file(&archive).unwrap().documents, 2);
        let mut target = Store::open(&root.path().join("target.db")).unwrap();
        target.restore_backup_file(&archive).unwrap();
        target.restore_backup_file(&archive).unwrap();
        let documents = target.snapshot().unwrap().documents;
        assert_eq!(documents.len(), 2);
        let restored = documents.iter().find(|d| d.id == large.id).unwrap();
        assert_eq!(restored.text, large.text);
        assert_eq!(restored.position, 1);
        assert_eq!(restored.bookmarks, [large.sentences[1].id.clone()]);
        assert_eq!(restored.notes[&large.sentences[1].id], "Retain this note.");
        assert_eq!(
            target.original(&small.id).unwrap().bytes,
            b"An older original."
        );
        let path = target.original_file(&large.id).unwrap().unwrap().path;
        copy_original(
            File::open(path).unwrap(),
            io::sink(),
            32 * 1024 * 1024,
            &large.source_hash,
            true,
        )
        .unwrap();

        let offset = {
            let (mut zip, backup) = open(File::open(&archive).unwrap()).unwrap();
            let index = backup
                .originals
                .iter()
                .position(|entry| entry.document_id == large.id)
                .unwrap();
            let entry = zip.by_name(&entry_name(index)).unwrap();
            entry.data_start().unwrap()
        };
        let mut damaged = fs::OpenOptions::new().write(true).open(&archive).unwrap();
        damaged.seek(SeekFrom::Start(offset + 100)).unwrap();
        damaged.write_all(b"x").unwrap();
        drop(damaged);
        assert!(inspect_backup_file(&archive).is_err());
        let mut empty = Store::open(&root.path().join("empty.db")).unwrap();
        assert!(empty.restore_backup_file(&archive).is_err());
        assert!(empty.snapshot().unwrap().documents.is_empty());
        assert!(fs::read_dir(empty.pdf_original_root())
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == ".staging.lock"));
    }

    #[test]
    fn bounds_zip64_directory_offsets_before_opening() {
        let mut file = tempfile::tempfile().unwrap();
        file.seek(SeekFrom::Start(u32::MAX as u64 + 1)).unwrap();
        let mut zip = ZipWriter::new(&mut file);
        zip.start_file(
            MANIFEST,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        let (position, count) = check_directory(&mut file).unwrap();
        assert!(position > u32::MAX as u64);
        assert_eq!(count, 1);
    }
}
