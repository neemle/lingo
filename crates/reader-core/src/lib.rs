use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    path::Path,
};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

pub mod translation;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_IMPORT: usize = 20 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sentence {
    pub id: String,
    pub text: String,
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub id: String,
    pub title: String,
    pub language: String,
    pub text: String,
    pub sentences: Vec<Sentence>,
    pub position: usize,
    #[serde(default)]
    pub bookmarks: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reviewed_translations: BTreeMap<String, BTreeMap<String, String>>,
    pub created_at: i64,
    pub source_hash: String,
    pub format: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReviewState {
    pub due_at: i64,
    pub interval_days: u32,
    pub reviews: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vocabulary {
    pub id: String,
    pub word: String,
    pub translation: String,
    pub language: String,
    pub target_language: String,
    pub context: String,
    pub document_id: String,
    pub forward: ReviewState,
    pub reverse: ReviewState,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub documents: Vec<Document>,
    pub vocabulary: Vec<Vocabulary>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub version: u32,
    pub snapshot: Snapshot,
    pub originals: Vec<Original>,
    #[serde(default)]
    pub reviews: Vec<ReviewEvent>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEvent {
    pub entry_id: String,
    pub direction: String,
    pub grade: u8,
    pub reviewed_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Original {
    pub document_id: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn segment(text: &str, id: &str) -> Vec<Sentence> {
    let mut previous_end = 0;
    let mut utf16_end = 0;
    text.split_sentence_bound_indices()
        .filter_map(|(offset, part)| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            let start = offset + part.len() - part.trim_start().len();
            let end = start + trimmed.len();
            let start_utf16 = utf16_end + text[previous_end..start].encode_utf16().count();
            utf16_end = start_utf16 + trimmed.encode_utf16().count();
            previous_end = end;
            Some(Sentence {
                id: format!("{id}:{start}"),
                text: trimmed.into(),
                start_utf8: start,
                end_utf8: end,
                start_utf16,
                end_utf16: utf16_end,
            })
        })
        .collect()
}
fn valid_language(language: &str) -> Result<()> {
    if [
        "en", "uk", "fr", "de", "es", "it", "pt", "pl", "nl", "cs", "ro", "sv",
    ]
    .contains(&language)
    {
        Ok(())
    } else {
        Err("Непідтримуваний код мови".into())
    }
}
fn validate_translation(text: &str) -> Result<()> {
    if text.encode_utf16().take(4001).count() > 4000 {
        return Err("Переклад речення задовгий".into());
    }
    Ok(())
}

pub fn parse_file(name: &str, bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_IMPORT {
        return Err("Файл завеликий. Максимум — 20 МБ.".into());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let text = match extension.as_str() {
        "txt" => String::from_utf8(bytes.to_vec())
            .map_err(|_| "TXT має бути в кодуванні UTF-8".to_string())?
            .trim_start_matches('\u{feff}')
            .to_owned(),
        "pdf" => {
            if !bytes.starts_with(b"%PDF-") {
                return Err("Некоректний PDF".into());
            }
            pdf_extract::extract_text_from_mem(bytes)
                .map_err(|e| format!("Не вдалося прочитати PDF: {e}"))?
        }
        "docx" => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|_| "Некоректний DOCX".to_string())?;
            let mut member = archive
                .by_name("word/document.xml")
                .map_err(|_| "У DOCX немає текстового документа".to_string())?;
            if member.size() > MAX_IMPORT as u64 {
                return Err("Розпакований документ завеликий".into());
            }
            let mut xml = String::new();
            member
                .by_ref()
                .take(MAX_IMPORT as u64 + 1)
                .read_to_string(&mut xml)
                .map_err(|e| e.to_string())?;
            if xml.len() > MAX_IMPORT {
                return Err("Розпакований документ завеликий".into());
            }
            let mut reader = quick_xml::Reader::from_str(&xml);
            let mut result = String::new();
            let mut in_text = false;
            loop {
                use quick_xml::events::Event;
                match reader.read_event() {
                    Ok(Event::Start(e)) if e.name().as_ref() == b"w:t" => in_text = true,
                    Ok(Event::End(e)) if e.name().as_ref() == b"w:t" => in_text = false,
                    Ok(Event::End(e)) if e.name().as_ref() == b"w:p" => result.push('\n'),
                    Ok(Event::Empty(e)) if e.name().as_ref() == b"w:tab" => result.push('\t'),
                    Ok(Event::Empty(e)) if e.name().as_ref() == b"w:br" => result.push('\n'),
                    Ok(Event::Text(e)) if in_text => {
                        result.push_str(&e.decode().map_err(|e| e.to_string())?)
                    }
                    Ok(Event::GeneralRef(e)) if in_text => {
                        let r = e.decode().map_err(|e| e.to_string())?;
                        if let Some(c) = quick_xml::escape::resolve_predefined_entity(&r) {
                            result.push_str(c);
                        } else if let Some(n) = r
                            .strip_prefix("#x")
                            .and_then(|x| u32::from_str_radix(x, 16).ok())
                            .or_else(|| r.strip_prefix('#').and_then(|x| x.parse::<u32>().ok()))
                        {
                            if let Some(c) = char::from_u32(n) {
                                result.push(c);
                            }
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => return Err(format!("Некоректний XML: {e}")),
                    _ => {}
                }
            }
            result
        }
        _ => {
            return Err("Доступний імпорт TXT, DOCX і текстових PDF. OCR ще не встановлено.".into())
        }
    };
    if text.trim().is_empty() {
        return Err("Тексту не знайдено. Для сканів потрібен OCR, який ще не встановлено.".into());
    }
    if text.len() > MAX_IMPORT {
        return Err("Витягнутий текст завеликий".into());
    }
    Ok(text)
}

pub struct Store {
    connection: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS documents (id TEXT PRIMARY KEY, body TEXT NOT NULL, source_hash TEXT UNIQUE NOT NULL); CREATE TABLE IF NOT EXISTS originals (document_id TEXT PRIMARY KEY REFERENCES documents(id), name TEXT NOT NULL, bytes BLOB NOT NULL); CREATE TABLE IF NOT EXISTS vocabulary (id TEXT PRIMARY KEY, word_key TEXT UNIQUE NOT NULL, body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS review_events (id INTEGER PRIMARY KEY, entry_id TEXT NOT NULL, direction TEXT NOT NULL, grade INTEGER NOT NULL, reviewed_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS translation_cache (key TEXT PRIMARY KEY, body TEXT NOT NULL); PRAGMA user_version=1;").map_err(|e| e.to_string())?;
        Ok(Self { connection })
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        fn rows<T: serde::de::DeserializeOwned>(
            connection: &Connection,
            sql: &str,
        ) -> Result<Vec<T>> {
            let mut statement = connection.prepare(sql).map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.map(|r| {
                serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
            })
            .collect()
        }
        Ok(Snapshot {
            documents: rows(
                &self.connection,
                "SELECT body FROM documents ORDER BY rowid DESC",
            )?,
            vocabulary: rows(
                &self.connection,
                "SELECT body FROM vocabulary ORDER BY rowid DESC",
            )?,
        })
    }
    pub fn import(&mut self, name: &str, bytes: &[u8], language: &str) -> Result<Document> {
        valid_language(language)?;
        let hash = format!("{:x}", Sha256::digest(bytes));
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM documents WHERE source_hash=?",
                [&hash],
                |r| r.get(0),
            )
            .ok();
        if let Some(body) = existing {
            return serde_json::from_str(&body).map_err(|e| e.to_string());
        }
        let text = parse_file(name, bytes)?;
        let id = Uuid::new_v4().to_string();
        let name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Document.txt");
        let document = Document {
            sentences: segment(&text, &id),
            id,
            title: Path::new(name)
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or(name)
                .into(),
            language: language.into(),
            text,
            position: 0,
            bookmarks: Vec::new(),
            reviewed_translations: BTreeMap::new(),
            created_at: now(),
            source_hash: hash,
            format: Path::new(name)
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("txt")
                .to_lowercase(),
        };
        let tx = self.connection.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO documents VALUES (?1,?2,?3)",
            params![
                document.id,
                serde_json::to_string(&document).unwrap(),
                document.source_hash
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO originals VALUES (?1,?2,?3)",
            params![document.id, name, bytes],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(document)
    }
    fn edit_document(
        &mut self,
        id: &str,
        edit: impl FnOnce(&mut Document) -> Result<bool>,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let body: String = tx
            .query_row("SELECT body FROM documents WHERE id=?", [id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut document: Document = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        if edit(&mut document)? {
            let body = serde_json::to_string(&document).map_err(|e| e.to_string())?;
            tx.execute("UPDATE documents SET body=? WHERE id=?", params![body, id])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn position(&mut self, id: &str, position: usize) -> Result<()> {
        self.edit_document(id, |document| {
            if position >= document.sentences.len() {
                return Err("Некоректна позиція".into());
            }
            let changed = document.position != position;
            document.position = position;
            Ok(changed)
        })
    }

    pub fn set_bookmark(&mut self, id: &str, sentence_id: &str, saved: bool) -> Result<()> {
        self.edit_document(id, |document| {
            if !document
                .sentences
                .iter()
                .any(|sentence| sentence.id == sentence_id)
            {
                return Err("Речення для закладки не знайдено".into());
            }
            let existing = document.bookmarks.iter().position(|id| id == sentence_id);
            match (saved, existing) {
                (true, None) => document.bookmarks.push(sentence_id.into()),
                (false, Some(index)) => {
                    document.bookmarks.remove(index);
                }
                _ => return Ok(false),
            }
            Ok(true)
        })
    }

    pub fn set_reviewed_translation(
        &mut self,
        id: &str,
        sentence_id: &str,
        target: &str,
        text: &str,
    ) -> Result<()> {
        valid_language(target)?;
        let text = text.trim();
        validate_translation(text)?;
        self.edit_document(id, |document| {
            if target == document.language {
                return Err("Мова перекладу має відрізнятися від мови джерела".into());
            }
            if !document
                .sentences
                .iter()
                .any(|sentence| sentence.id == sentence_id)
            {
                return Err("Речення для перекладу не знайдено".into());
            }
            let translations = &mut document.reviewed_translations;
            if text.is_empty() {
                let Some(segments) = translations.get_mut(target) else {
                    return Ok(false);
                };
                let removed = segments.remove(sentence_id).is_some();
                if segments.is_empty() {
                    translations.remove(target);
                }
                return Ok(removed);
            }
            translations
                .entry(target.into())
                .or_default()
                .insert(sentence_id.into(), text.into());
            Ok(true)
        })
    }

    pub fn save_word(&mut self, mut entry: Vocabulary) -> Result<Vocabulary> {
        valid_language(&entry.language)?;
        valid_language(&entry.target_language)?;
        entry.word = entry.word.trim().to_owned();
        entry.translation = entry.translation.trim().to_owned();
        if entry.word.is_empty()
            || entry.translation.is_empty()
            || entry.word.len() > 500
            || entry.translation.len() > 2000
        {
            return Err("Вкажіть слово й переклад (до 500 / 2000 символів)".into());
        }
        // Distinct translations remain distinct senses; exact repetitions preserve learning states.
        let key = format!(
            "{}|{}|{}|{}",
            entry.language,
            entry.target_language,
            entry.word.to_lowercase(),
            entry.translation.to_lowercase()
        );
        if let Ok(body) = self.connection.query_row(
            "SELECT body FROM vocabulary WHERE word_key=?",
            [&key],
            |r| r.get::<_, String>(0),
        ) {
            return serde_json::from_str(&body).map_err(|e| e.to_string());
        }
        entry.id = Uuid::new_v4().to_string();
        entry.forward = ReviewState::default();
        entry.reverse = ReviewState::default();
        self.connection
            .execute(
                "INSERT INTO vocabulary VALUES (?1,?2,?3)",
                params![entry.id, key, serde_json::to_string(&entry).unwrap()],
            )
            .map_err(|e| e.to_string())?;
        Ok(entry)
    }
    pub fn review(&mut self, id: &str, direction: &str, grade: u8) -> Result<()> {
        if !["forward", "reverse"].contains(&direction) || grade > 3 {
            return Err("Некоректна відповідь".into());
        }
        let body: String = self
            .connection
            .query_row("SELECT body FROM vocabulary WHERE id=?", [id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut entry: Vocabulary = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let state = if direction == "forward" {
            &mut entry.forward
        } else {
            &mut entry.reverse
        };
        state.interval_days = match grade {
            0 => 0,
            1 => 1,
            2 => {
                if state.interval_days == 0 {
                    2
                } else {
                    state.interval_days.saturating_mul(2).min(365)
                }
            }
            _ => {
                if state.interval_days == 0 {
                    4
                } else {
                    state.interval_days.saturating_mul(3).min(365)
                }
            }
        };
        let time = now();
        state.due_at = time
            + if grade == 0 {
                60
            } else {
                state.interval_days as i64 * 86400
            };
        state.reviews += 1;
        let tx = self.connection.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE vocabulary SET body=? WHERE id=?",
            params![serde_json::to_string(&entry).unwrap(), id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO review_events(entry_id,direction,grade,reviewed_at) VALUES (?1,?2,?3,?4)",
            params![id, direction, grade, time],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn original(&self, id: &str) -> Result<Original> {
        self.connection
            .query_row(
                "SELECT name,bytes FROM originals WHERE document_id=?",
                [id],
                |r| {
                    Ok(Original {
                        document_id: id.into(),
                        name: r.get(0)?,
                        bytes: r.get(1)?,
                    })
                },
            )
            .map_err(|e| e.to_string())
    }
    pub fn export(&self) -> Result<Backup> {
        let snapshot = self.snapshot()?;
        let originals = snapshot
            .documents
            .iter()
            .map(|d| self.original(&d.id))
            .collect::<Result<Vec<_>>>()?;
        let mut stmt = self
            .connection
            .prepare("SELECT entry_id,direction,grade,reviewed_at FROM review_events ORDER BY id")
            .map_err(|e| e.to_string())?;
        let reviews = stmt
            .query_map([], |r| {
                Ok(ReviewEvent {
                    entry_id: r.get(0)?,
                    direction: r.get(1)?,
                    grade: r.get(2)?,
                    reviewed_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(Backup {
            version: 1,
            snapshot,
            originals,
            reviews,
        })
    }
    pub fn restore(&mut self, backup: Backup) -> Result<()> {
        if backup.version != 1 {
            return Err("Невідома версія архіву".into());
        }
        // Validate and reparse originals rather than trusting imported derived offsets.
        let mut validated = Vec::new();
        for doc in &backup.snapshot.documents {
            valid_language(&doc.language)?;
            let original = backup
                .originals
                .iter()
                .find(|o| o.document_id == doc.id)
                .ok_or("В архіві немає оригіналу")?;
            if format!("{:x}", Sha256::digest(&original.bytes)) != doc.source_hash {
                return Err("Пошкоджений оригінал в архіві".into());
            }
            let text = parse_file(&original.name, &original.bytes)?;
            let mut d = doc.clone();
            d.text = text;
            d.sentences = segment(&d.text, &d.id);
            d.position = d.position.min(d.sentences.len().saturating_sub(1));
            if !d.bookmarks.is_empty() || !d.reviewed_translations.is_empty() {
                let known: std::collections::HashSet<_> = d
                    .sentences
                    .iter()
                    .map(|sentence| sentence.id.as_str())
                    .collect();
                if d.bookmarks.iter().any(|id| !known.contains(id.as_str())) {
                    return Err("В архіві є закладка для невідомого речення".into());
                }
                d.bookmarks.sort_unstable();
                d.bookmarks.dedup();
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
            validated.push((d, original));
        }
        for v in &backup.snapshot.vocabulary {
            valid_language(&v.language)?;
            valid_language(&v.target_language)?;
            if v.word.trim().is_empty() || v.translation.trim().is_empty() {
                return Err("Некоректна словникова картка".into());
            }
        }
        let tx = self.connection.transaction().map_err(|e| e.to_string())?;
        for (d, o) in validated {
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO documents VALUES (?1,?2,?3)",
                    params![d.id, serde_json::to_string(&d).unwrap(), d.source_hash],
                )
                .map_err(|e| e.to_string())?;
            if inserted > 0 {
                tx.execute(
                    "INSERT INTO originals VALUES (?1,?2,?3)",
                    params![d.id, o.name, o.bytes],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        for v in backup.snapshot.vocabulary {
            let key = format!(
                "{}|{}|{}|{}",
                v.language,
                v.target_language,
                v.word.to_lowercase(),
                v.translation.to_lowercase()
            );
            tx.execute(
                "INSERT OR IGNORE INTO vocabulary VALUES (?1,?2,?3)",
                params![v.id, key, serde_json::to_string(&v).unwrap()],
            )
            .map_err(|e| e.to_string())?;
        }
        for r in backup.reviews {
            if !["forward", "reverse"].contains(&r.direction.as_str()) || r.grade > 3 {
                return Err("Некоректна історія повторень".into());
            }
            tx.execute("INSERT INTO review_events(entry_id,direction,grade,reviewed_at) SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (SELECT 1 FROM review_events WHERE entry_id=?1 AND direction=?2 AND grade=?3 AND reviewed_at=?4)",params![r.entry_id,r.direction,r.grade,r.reviewed_at]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_pdf_import_retains_original() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(&dir.path().join("pdf.db")).unwrap();
        let bytes = b"%PDF-1.4\n\
        1 0 obj\n\
        << /Type /Catalog /Pages 2 0 R >>\n\
        endobj\n\
        2 0 obj\n\
        << /Type /Pages /Kids [3 0 R] /Count 1 >>\n\
        endobj\n\
        3 0 obj\n\
        << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\n\
        endobj\n\
        4 0 obj\n\
        << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\n\
        endobj\n\
        5 0 obj\n\
        << /Length 65 >>\n\
        stream\n\
        BT /F1 18 Tf 72 700 Td (Hello reader. This is a local PDF.) Tj ET\n\
        endstream\n\
        endobj\n\
        xref\n\
        0 6\n\
        0000000000 65535 f\x20\n\
        0000000009 00000 n\x20\n\
        0000000058 00000 n\x20\n\
        0000000115 00000 n\x20\n\
        0000000241 00000 n\x20\n\
        0000000311 00000 n\x20\n\
        trailer\n\
        << /Size 6 /Root 1 0 R >>\n\
        startxref\n\
        426\n\
        %%EOF\n";
        let doc = db.import("sample_en.pdf", bytes, "en").unwrap();
        assert!(doc.text.contains("Hello reader."));
        assert!(!doc.sentences.is_empty());
        assert_eq!(db.original(&doc.id).unwrap().bytes, bytes);
    }
    #[test]
    fn unicode_offsets_roundtrip() {
        for text in [
            "  Bonjour 👋🏽 ! Україна — це дім.\nNext.",
            "Cafe\u{301}. 你好。مرحبا بالعالم.",
            "L’homme lit. C’est bien !",
            "\r\n  😀 Café !\r\n\r\n  Next.\tThird.  中。\n",
        ] {
            let s = segment(text, "a");
            assert!(!s.is_empty());
            let utf16: Vec<u16> = text.encode_utf16().collect();
            for sentence in &s {
                assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
                assert_eq!(sentence.id, format!("a:{}", sentence.start_utf8));
                assert_eq!(
                    sentence.start_utf16,
                    text[..sentence.start_utf8].encode_utf16().count()
                );
                assert_eq!(
                    sentence.end_utf16,
                    text[..sentence.end_utf8].encode_utf16().count()
                );
                assert_eq!(
                    String::from_utf16(&utf16[sentence.start_utf16..sentence.end_utf16]).unwrap(),
                    sentence.text
                );
            }
            assert_eq!(segment(text, "a")[0].id, s[0].id);
        }
    }
    #[test]
    fn restart_dedup_and_directional_review() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Store::open(&path).unwrap();
        let doc = db
            .import("story.txt", "Bonjour. Ça va ?".as_bytes(), "fr")
            .unwrap();
        db.position(&doc.id, 1).unwrap();
        assert_eq!(
            db.import("other.txt", doc.text.as_bytes(), "fr")
                .unwrap()
                .id,
            doc.id
        );
        let entry = db
            .save_word(Vocabulary {
                id: "".into(),
                word: "Bonjour".into(),
                translation: "Привіт".into(),
                language: "fr".into(),
                target_language: "uk".into(),
                context: doc.text.clone(),
                document_id: doc.id.clone(),
                forward: ReviewState::default(),
                reverse: ReviewState::default(),
            })
            .unwrap();
        db.review(&entry.id, "forward", 2).unwrap();
        assert!(db.position(&doc.id, 999).is_err());
        assert!(db.review(&entry.id, "oops", 2).is_err());
        drop(db);
        let db = Store::open(&path).unwrap();
        let s = db.snapshot().unwrap();
        assert_eq!(s.documents[0].position, 1);
        assert_eq!(s.vocabulary[0].forward.reviews, 1);
        assert_eq!(s.vocabulary[0].reverse.reviews, 0);
        assert_eq!(db.original(&doc.id).unwrap().bytes, doc.text.as_bytes());
        let mut restored = Store::open(&dir.path().join("restored.db")).unwrap();
        restored.restore(db.export().unwrap()).unwrap();
        assert_eq!(restored.snapshot().unwrap().documents[0].position, 1);
        assert_eq!(
            restored.snapshot().unwrap().vocabulary[0].forward.reviews,
            1
        );
        assert_eq!(restored.export().unwrap().reviews.len(), 1);
        restored.restore(db.export().unwrap()).unwrap();
        assert_eq!(restored.export().unwrap().reviews.len(), 1);
    }
    #[test]
    fn rejects_corruption_and_unsupported_inputs() {
        assert!(parse_file("x.png", b"test").is_err());
        assert!(parse_file("x.pdf", b"test").is_err());
        assert!(parse_file("x.txt", &[255]).is_err());
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(&dir.path().join("a.db")).unwrap();
        db.import("x.txt", b"Hello.", "en").unwrap();
        let mut b = db.export().unwrap();
        b.originals[0].bytes[0] = 0;
        assert!(db.restore(b).is_err());
        assert_eq!(db.snapshot().unwrap().documents.len(), 1);
    }
    #[test]
    fn docx_entities_and_paragraphs() {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(br#"<w:document xmlns:w="test"><w:p><w:r><w:t>Tom &amp; Jerry &#x1F600;.</w:t></w:r></w:p><w:p><w:r><w:t>Next.</w:t></w:r></w:p></w:document>"#).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(
            parse_file("a.docx", &bytes).unwrap(),
            "Tom & Jerry 😀.\nNext.\n"
        );
    }
}
