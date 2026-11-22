use std::collections::HashMap;

use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{now, Document, Result, MAX_IMPORT};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentRevision {
    pub id: String,
    pub document_id: String,
    pub parent_id: Option<String>,
    pub source_hash: String,
    pub parser_version: String,
    pub text_hash: String,
    pub canonical_bytes: u32,
    pub created_at: i64,
}

impl DocumentRevision {
    fn identity(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"lingo-document-revision-v1");
        for value in [
            self.document_id.as_str(),
            self.parent_id.as_deref().unwrap_or(""),
            &self.source_hash,
            &self.parser_version,
            &self.text_hash,
        ] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        hash.update((self.canonical_bytes as u64).to_be_bytes());
        format!("{:x}", hash.finalize())
    }

    pub(crate) fn capture(document: &mut Document, parser_version: &str, created_at: i64) -> Self {
        let mut revision = Self {
            id: String::new(),
            document_id: document.id.clone(),
            parent_id: document.current_revision_id.clone(),
            source_hash: document.source_hash.clone(),
            parser_version: parser_version.into(),
            text_hash: text_hash(&document.text),
            canonical_bytes: document.text.len() as u32,
            created_at,
        };
        revision.id = revision.identity();
        document.current_revision_id = Some(revision.id.clone());
        revision
    }

    pub(crate) fn matches_text(&self, text: &str) -> bool {
        self.canonical_bytes as usize == text.len() && self.text_hash == text_hash(text)
    }

    pub(crate) fn insert(&self, connection: &Connection) -> Result<()> {
        connection.execute(
            "INSERT INTO document_revisions (id, document_id, parent_id, source_hash, parser_version, text_hash, canonical_bytes, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![self.id, self.document_id, self.parent_id, self.source_hash, self.parser_version,
                self.text_hash, self.canonical_bytes, self.created_at],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn text_hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(crate) fn parser_version(document: &Document) -> &'static str {
    match (
        document.format.as_str(),
        document.ocr_lines.is_some(),
        document.pdf_page_starts.is_some(),
    ) {
        ("docx", _, _) => "docx-reflow/1",
        ("pdf", true, true) => "pdf-mixed-reflow/1",
        ("pdf", true, false) => "pdf-ocr-reflow/1",
        ("pdf", false, _) => "pdf-text-reflow/1",
        (_, true, _) => "image-ocr-reflow/1",
        _ => "text-reflow/1",
    }
}

pub(crate) fn append(document: &mut Document, connection: &Connection) -> Result<()> {
    let version = parser_version(document);
    DocumentRevision::capture(document, version, now()).insert(connection)
}

pub(crate) fn initialize(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS document_revisions (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            parent_id TEXT UNIQUE REFERENCES document_revisions(id) ON DELETE CASCADE,
            source_hash TEXT NOT NULL,
            parser_version TEXT NOT NULL,
            text_hash TEXT NOT NULL,
            canonical_bytes INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS document_revisions_owner ON document_revisions(document_id);
        CREATE UNIQUE INDEX IF NOT EXISTS document_revisions_root ON document_revisions(document_id) WHERE parent_id IS NULL;",
    ).map_err(|error| error.to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let missing = {
        let mut query = tx
            .prepare(
                "SELECT id FROM documents WHERE NOT EXISTS
            (SELECT 1 FROM document_revisions WHERE document_id = documents.id)",
            )
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.to_string())?
    };
    for id in missing {
        let body: String = tx
            .query_row("SELECT body FROM documents WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let mut document: Document =
            serde_json::from_str(&body).map_err(|error| error.to_string())?;
        document.current_revision_id = None;
        DocumentRevision::capture(&mut document, "legacy-canonical/1", now()).insert(&tx)?;
        let body = serde_json::to_string(&document).map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE documents SET body = ?1 WHERE id = ?2",
            params![body, id],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.commit().map_err(|error| error.to_string())
}

pub(crate) fn export(connection: &Connection) -> Result<Vec<DocumentRevision>> {
    let mut query = connection
        .prepare(
            "SELECT id, document_id, parent_id, source_hash, parser_version,
        text_hash, canonical_bytes, created_at FROM document_revisions ORDER BY rowid",
        )
        .map_err(|error| error.to_string())?;
    let rows = query
        .query_map([], |row| {
            Ok(DocumentRevision {
                id: row.get(0)?,
                document_id: row.get(1)?,
                parent_id: row.get(2)?,
                source_hash: row.get(3)?,
                parser_version: row.get(4)?,
                text_hash: row.get(5)?,
                canonical_bytes: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<rusqlite::Result<_>>()
        .map_err(|error| error.to_string())
}

pub(crate) fn validate_chain(
    document: &Document,
    revisions: Vec<DocumentRevision>,
) -> Result<Vec<DocumentRevision>> {
    let invalid = || "Некоректна історія ревізій документа в архіві".to_owned();
    let mut indexed = HashMap::with_capacity(revisions.len());
    for revision in revisions {
        if revision.document_id != document.id
            || revision.source_hash != document.source_hash
            || revision.parser_version.is_empty()
            || revision.parser_version.len() > 128
            || revision.parser_version.chars().any(char::is_control)
            || revision.canonical_bytes as usize > MAX_IMPORT
            || revision.text_hash.len() != 64
            || !revision
                .text_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || revision.id != revision.identity()
            || indexed.insert(revision.id.clone(), revision).is_some()
        {
            return Err(invalid());
        }
    }
    let mut chain = Vec::with_capacity(indexed.len());
    let mut next = Some(document.current_revision_id.clone().ok_or_else(invalid)?);
    while let Some(id) = next {
        let revision = indexed.remove(&id).ok_or_else(invalid)?;
        next = revision.parent_id.clone();
        chain.push(revision);
    }
    if !indexed.is_empty() {
        return Err(invalid());
    }
    chain.reverse();
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use std::path::Path;

    #[test]
    fn revision_chain_survives_ocr_corrections_reopen_and_backup_restore() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reader.db");
        let mut store = Store::open(&path).unwrap();
        let original = b"\x89PNG\r\n\x1a\n";
        let lines = serde_json::from_value(serde_json::json!([
            {"text":"First sentence.","x":0.1,"y":0.1,"width":0.8,"height":0.1,"confidence":0.9},
            {"text":"Keep this note.","x":0.1,"y":0.3,"width":0.8,"height":0.1,"confidence":0.9},
        ]))
        .unwrap();
        let document = store
            .import_image("scan.png", original, "en", lines)
            .unwrap();
        let initial = document.current_revision_id.as_ref().unwrap();
        let sentence = &document.sentences[1].id;
        store.set_bookmark(&document.id, sentence, true).unwrap();
        store
            .set_sentence_note(&document.id, sentence, "keep")
            .unwrap();
        for _ in 0..2 {
            store
                .correct_ocr_line(
                    &document.id,
                    0,
                    Some("New sentence. Another sentence.".into()),
                )
                .unwrap();
        }
        drop(store);

        let store = Store::open(&path).unwrap();
        let archive = store.export().unwrap();
        let [first, current] = archive.document_revisions.as_slice() else {
            panic!("only genuine canonical changes should create revisions");
        };
        let updated = &archive.snapshot.documents[0];
        assert_eq!(first.id, *initial);
        assert_eq!(current.parent_id.as_deref(), Some(initial.as_str()));
        assert_eq!(
            updated.current_revision_id.as_deref(),
            Some(current.id.as_str())
        );
        assert_eq!(current.source_hash, document.source_hash);
        assert_ne!(current.text_hash, first.text_hash);
        assert!(current.matches_text(&updated.text));
        assert_eq!(updated.notes[&updated.sentences[2].id], "keep");
        assert_eq!(updated.bookmarks, [updated.sentences[2].id.clone()]);

        let mut restored = Store::open(Path::new(":memory:")).unwrap();
        restored.restore(archive.clone()).unwrap();
        assert_eq!(
            restored.export().unwrap().document_revisions[1].id,
            current.id
        );
        let mut legacy = archive;
        legacy.document_revisions.clear();
        legacy.snapshot.documents[0].current_revision_id = None;
        let mut restored_legacy = Store::open(Path::new(":memory:")).unwrap();
        restored_legacy.restore(legacy).unwrap();
        let baseline = restored_legacy.export().unwrap();
        assert_eq!(baseline.document_revisions.len(), 1);
        assert!(baseline.document_revisions[0].matches_text(&baseline.snapshot.documents[0].text));
    }
}
