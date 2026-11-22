use std::{
    fs::{self, File},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::{
    document_layout, document_revisions, now, ocr, pdf_layers, segment, valid_language, Document,
    Result, Store,
};

pub const MAX_PDF_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct OriginalFile {
    pub name: String,
    pub path: PathBuf,
}

/// Private source data copied and hashed before taking the SQLite write lock.
pub struct PreparedPdfImport {
    original: NamedTempFile,
    name: String,
    language: String,
    size: u64,
    hash: String,
    page_text: String,
    page_count: u16,
    lines: Vec<ocr::OcrLine>,
    empty_pages: Vec<u16>,
    needs_cache_merge: bool,
    extracted: document_layout::Extracted,
}

pub fn prepare_pdf_import(
    root: &Path,
    name: &str,
    path: &Path,
    language: &str,
    page_text: Vec<String>,
    lines: Vec<ocr::OcrLine>,
) -> Result<PreparedPdfImport> {
    prepare_pdf_import_with_empty_pages(root, name, path, language, page_text, lines, Vec::new())
}

pub fn prepare_pdf_import_with_empty_pages(
    root: &Path,
    name: &str,
    path: &Path,
    language: &str,
    page_text: Vec<String>,
    lines: Vec<ocr::OcrLine>,
    mut empty_pages: Vec<u16>,
) -> Result<PreparedPdfImport> {
    valid_language(language)?;
    let name = pdf_name(name)?;
    let page_count = u16::try_from(page_text.len()).map_err(|_| "PDF містить забагато сторінок")?;
    let text_bytes = page_text
        .iter()
        .fold(0usize, |total, page| total.saturating_add(page.len()));
    if text_bytes > crate::MAX_IMPORT {
        return Err("Текст PDF завеликий".into());
    }
    if empty_pages.iter().any(|&page| page >= page_count) {
        return Err("Збережений результат OCR має некоректний номер сторінки.".into());
    }
    empty_pages.retain(|&page| page_text[usize::from(page)].trim().is_empty());
    empty_pages.sort_unstable();
    empty_pages.dedup();
    let page_text_json = serde_json::to_string(&page_text).map_err(storage_error)?;
    let (text, starts) = pdf_layers::merge_page_text(page_text, &lines)?;
    let extracted = document_layout::from_transcript(text, Some(starts));
    let (input, expected_size) = open_pdf_source(path)?;
    ensure_root(root)?;
    let mut original = stage_original(root)?;
    let (size, hash) = copy_pdf(input, original.as_file_mut())?;
    if size != expected_size {
        return Err("Розмір файлу PDF змінився під час імпорту".into());
    }
    original.as_file().sync_all().map_err(storage_error)?;
    Ok(PreparedPdfImport {
        original,
        name: name.to_owned(),
        language: language.to_owned(),
        size,
        hash,
        page_text: page_text_json,
        page_count,
        lines,
        empty_pages,
        needs_cache_merge: false,
        extracted,
    })
}

impl PreparedPdfImport {
    pub fn validate_checkpoint_source(&mut self, expected: &str) -> Result<()> {
        if self.hash == expected {
            self.needs_cache_merge = true;
            Ok(())
        } else {
            Err("Збережений оригінал PDF змінився під час перенесення.".into())
        }
    }
}

pub(crate) fn open_pdf_source(path: &Path) -> Result<(File, u64)> {
    if !path.symlink_metadata().map_err(storage_error)?.is_file() {
        return Err("Виберіть звичайний файл PDF".into());
    }
    let mut input = File::open(path).map_err(storage_error)?;
    let metadata = input.metadata().map_err(storage_error)?;
    if !metadata.is_file() || metadata.len() > MAX_PDF_BYTES {
        return Err("Потрібен звичайний файл PDF розміром до 4 ГіБ".into());
    }
    let mut header = [0; 5];
    if input.read_exact(&mut header).is_err() || &header != b"%PDF-" {
        return Err("Файл PDF пошкоджено або він не є PDF".into());
    }
    input.rewind().map_err(storage_error)?;
    Ok((input, metadata.len()))
}

pub(crate) fn pdf_name(name: &str) -> Result<&str> {
    let name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.len() <= 4_096 && !name.chars().any(char::is_control))
        .ok_or("Некоректна назва файлу PDF")?;
    if !Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    {
        return Err("Виберіть файл PDF".into());
    }
    Ok(name)
}

pub(crate) fn copy_pdf(mut input: impl Read, output: &mut File) -> Result<(u64, String)> {
    let mut buffer = vec![0; 64 * 1024];
    let mut digest = Sha256::new();
    let mut size = 0;
    loop {
        let count = input.read(&mut buffer).map_err(storage_error)?;
        if count == 0 {
            return Ok((size, format!("{:x}", digest.finalize())));
        }
        let chunk = &buffer[..count];
        size += count as u64;
        if size > MAX_PDF_BYTES {
            return Err("Файл PDF перевищує 4 ГіБ".into());
        }
        digest.update(chunk);
        output.write_all(chunk).map_err(storage_error)?;
    }
}

pub(crate) fn ensure_root(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {
            let parent = path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            sync_directory(parent)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_root(path),
        Err(error) => Err(storage_error(error)),
    }
}

fn staging_gate(root: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".staging.lock"))
}

pub(crate) fn stage_original(root: &Path) -> Result<NamedTempFile> {
    // Serialize name creation with the orphan scan until this individual file is locked.
    let gate = staging_gate(root).map_err(storage_error)?;
    gate.lock().map_err(storage_error)?;
    let staged = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(root)
        .map_err(storage_error)?;
    staged.as_file().lock().map_err(storage_error)?;
    Ok(staged)
}

pub(crate) fn verify_root(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        _ => Err("Каталог оригіналів PDF відсутній або пошкоджений".into()),
    }
}

pub(crate) fn original_path(root: &Path, id: &str) -> Result<PathBuf> {
    let valid_id = Uuid::parse_str(id).map_err(storage_error)?;
    Ok(root.join(format!("{valid_id}.pdf")))
}

pub(crate) fn directory(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".originals");
    path.with_file_name(name)
}

pub(crate) fn initialize(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS file_originals (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            size INTEGER NOT NULL,
            pdf_page_text TEXT NOT NULL
        );",
        )
        .map_err(storage_error)
}

impl Store {
    pub fn pdf_original_root(&self) -> &Path {
        &self.file_original_directory
    }

    pub fn import_pdf_file(&mut self, prepared: PreparedPdfImport) -> Result<Document> {
        let PreparedPdfImport {
            original,
            name,
            language,
            size,
            hash,
            page_text,
            page_count,
            lines,
            empty_pages,
            needs_cache_merge,
            extracted,
        } = prepared;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let existing = tx
            .query_row(
                "SELECT body FROM documents WHERE source_hash=?1",
                [&hash],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_error)?;
        if let Some(mut body) = existing {
            if !lines.is_empty() || !empty_pages.is_empty() {
                let incoming_pages: Vec<String> = serde_json::from_str(&page_text).map_err(storage_error)?;
                let merged = crate::pdf_page_ocr::merge_cached_pages(
                    &tx, &hash, &language, &lines, &incoming_pages, &empty_pages,
                )?;
                if needs_cache_merge && !merged {
                    return Err("Наявна копія PDF несумісна з результатами OCR. Чернетку збережено.".into());
                }
                body = tx.query_row(
                    "SELECT body FROM documents WHERE source_hash=?1", [&hash], |row| row.get(0),
                ).map_err(storage_error)?;
            }
            let document = serde_json::from_str(&body).map_err(storage_error)?;
            tx.commit().map_err(storage_error)?;
            return Ok(document);
        }
        let id = Uuid::new_v4().to_string();
        let path = original_path(&self.file_original_directory, &id)?;
        let title = Path::new(&name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&name);
        let text = extracted.text;
        let mut document = Document {
            sentences: segment(&text, &id),
            id,
            title: title.into(),
            language,
            text,
            current_revision_id: None,
            blocks: extracted.blocks,
            position: 0,
            reading_token_utf16: None,
            speech_resume: None,
            bookmarks: Vec::new(),
            notes: Default::default(),
            speech_text_overrides: Default::default(),
            reviewed_translations: Default::default(),
            ocr_lines: Some(lines),
            ocr_pdf_pages: Some(page_count),
            pdf_page_starts: extracted.pdf_page_starts,
            created_at: now(),
            source_hash: hash,
            format: "pdf".into(),
            source_url: None,
        };
        ocr::refresh_provenance(&mut document);
        let created_at = document.created_at;
        let revision = document_revisions::DocumentRevision::capture(
            &mut document,
            "pdf-native-reflow/1",
            created_at,
        );
        let body = serde_json::to_string(&document).map_err(storage_error)?;
        tx.execute(
            "INSERT INTO documents VALUES (?1,?2,?3)",
            params![document.id, body, document.source_hash],
        )
        .map_err(storage_error)?;
        tx.execute(
            "INSERT INTO file_originals VALUES (?1,?2,?3,?4)",
            params![document.id, name, size as i64, page_text],
        )
        .map_err(storage_error)?;
        for page in empty_pages {
            crate::pdf_page_ocr::mark_completed(&tx, &document.id, page)?;
        }
        revision.insert(&tx)?;
        // After publishing the source, synchronize the entry before committing its reference.
        verify_root(&self.file_original_directory)?;
        original.persist_noclobber(&path).map_err(storage_error)?;
        if let Err(error) = sync_directory(&self.file_original_directory)
            .and_then(|_| tx.commit().map_err(storage_error))
        {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(document)
    }

    pub fn original_file(&self, id: &str) -> Result<Option<OriginalFile>> {
        let metadata = self
            .connection
            .query_row(
                "SELECT name,size FROM file_originals WHERE document_id=?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(storage_error)?;
        metadata
            .map(|(name, size)| {
                verify_root(&self.file_original_directory)?;
                let path = original_path(&self.file_original_directory, id)?;
                let metadata = path.symlink_metadata().map_err(storage_error)?;
                if !metadata.is_file() || i64::try_from(metadata.len()).ok() != Some(size) {
                    return Err("Приватний оригінал PDF пошкоджено".into());
                }
                Ok(OriginalFile {
                    name,
                    path: path.canonicalize().map_err(storage_error)?,
                })
            })
            .transpose()
    }

    pub(crate) fn native_pdf_page_text(&self, id: &str) -> Result<Option<Vec<String>>> {
        native_pdf_page_text(&self.connection, id)
    }
}

pub(crate) fn native_pdf_page_text(db: &Connection, id: &str) -> Result<Option<Vec<String>>> {
    db.query_row(
        "SELECT pdf_page_text FROM file_originals WHERE document_id=?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(storage_error)?
    .map(|json| serde_json::from_str(&json).map_err(storage_error))
    .transpose()
}

pub(crate) fn remove(root: &Path, id: &str) {
    if verify_root(root).is_ok() {
        if let Ok(path) = original_path(root, id) {
            let _ = fs::remove_file(path);
        }
    }
}

// If the process stopped between publishing and committing, or after deleting a row,
// remove the resulting unreachable source on the next open. The write transaction
// ensures another process cannot publish a source while this scan is running.
pub(crate) fn cleanup_orphans(connection: &mut Connection, root: &Path) {
    if verify_root(root).is_err() {
        return;
    }
    let Ok(tx) = connection.transaction_with_behavior(TransactionBehavior::Immediate) else {
        return;
    };
    let pending_gate = staging_gate(root)
        .ok()
        .filter(|gate| gate.try_lock().is_ok());
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if pending_gate.is_some()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".pending-"))
            && entry.file_type().is_ok_and(|file_type| file_type.is_file())
        {
            if let Ok(staged) = File::options().read(true).write(true).open(&path) {
                if staged.try_lock().is_ok() {
                    let _ = fs::remove_file(&path);
                }
            }
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("pdf") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let intake_id = stem.strip_prefix(".intake-");
        let id = intake_id.unwrap_or(stem);
        if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
            continue;
        }
        let exists = tx.query_row(
            if intake_id.is_some() {
                "SELECT EXISTS(SELECT 1 FROM intake WHERE id=?1 AND document_id IS NULL)"
            } else {
                "SELECT EXISTS(SELECT 1 FROM file_originals WHERE document_id=?1)"
            },
            [id],
            |row| row.get::<_, bool>(0),
        );
        if matches!(exists, Ok(false)) {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(storage_error)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Помилка збереження PDF. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_pdf_keeps_its_original_and_native_text_across_restarts() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("reader.db");
        let source = root.path().join("import.pdf");
        let mut input = File::create(&source).unwrap();
        input.write_all(b"%PDF-1.7\n").unwrap();
        // The native platform supplies page text. This fixture tests the file-storage boundary.
        input.set_len(32 * 1024 * 1024).unwrap();
        drop(input);
        let mut store = Store::open(&database).unwrap();
        let lines = vec![ocr::OcrLine {
            page: 1,
            text: "Deuxième page.".into(),
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.1,
            confidence: None,
            correction: None,
            words: Vec::new(),
            source_span: None,
        }];
        let stage = |store: &Store| {
            prepare_pdf_import(
                store.pdf_original_root(),
                "French.PDF",
                &source,
                "fr",
                vec![" Première page.\n".into(), "".into()],
                lines.clone(),
            )
            .unwrap()
        };
        let prepared = stage(&store);
        let document = store.import_pdf_file(prepared).unwrap();
        assert_eq!(document.text, "Première page.\n\nDeuxième page.");
        let prepared = stage(&store);
        assert_eq!(store.import_pdf_file(prepared).unwrap().id, document.id);
        let id = &document.id;
        let original = store.original_file(id).unwrap().unwrap();
        assert_eq!(original.name, "French.PDF");
        assert_eq!(original.path.metadata().unwrap().len(), 32 * 1024 * 1024);
        assert_ne!(original.path, source);
        assert!(store.original(id).unwrap_err().contains("окремим файлом"));
        assert!(store.export().is_err());
        drop(store);

        let mut store = Store::open(&database).unwrap();
        assert!(original.path.is_file());
        store
            .correct_ocr_line(id, 0, Some("Page corrigée.".into()))
            .unwrap();
        let restored = store.snapshot().unwrap().documents.pop().unwrap();
        assert_eq!(restored.text, "Première page.\n\nPage corrigée.");
        assert_ne!(restored.current_revision_id, document.current_revision_id);
        assert!(store.delete_document(id).unwrap());
        assert!(!original.path.exists());
        assert!(store.original_file(id).unwrap().is_none());
        assert!(source.exists());
    }

    #[test]
    fn above_four_gib_is_rejected_before_copying() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("oversized.pdf");
        let mut input = File::create(&source).unwrap();
        input.write_all(b"%PDF-1.7\n").unwrap();
        input.set_len(MAX_PDF_BYTES + 1).unwrap();
        drop(input);
        let destination = root.path().join("originals");
        let result = prepare_pdf_import(
            &destination,
            "oversized.pdf",
            &source,
            "en",
            vec!["Page.".into()],
            Vec::new(),
        );
        assert!(matches!(result, Err(error) if error.contains("4 ГіБ")));
        assert!(!destination.exists());
    }

    #[test]
    fn opening_library_cleans_abandoned_files_but_preserves_live_staging() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("reader.db");
        let store = Store::open(&database).unwrap();
        let originals = store.pdf_original_root();
        ensure_root(originals).unwrap();
        let incomplete = originals.join(".pending-abandoned");
        let old_publication = originals.join(format!("{}.pdf", Uuid::new_v4()));
        File::create(&incomplete).unwrap();
        File::create(&old_publication).unwrap();
        let live = stage_original(originals).unwrap();
        let reopened = Store::open(&database).unwrap();
        assert_eq!(reopened.pdf_original_root(), originals);
        assert!(live.path().is_file());
        assert!(!incomplete.exists());
        assert!(!old_publication.exists());
    }
}
