use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

mod backup_resources;
pub use backup_resources::BackupResource;
mod card_learning;
pub use card_learning::SenseSnapshot;
pub mod dictionary_transfer;
pub mod document_layout;
pub mod e5;
pub mod knowledge;
pub mod language_detection;
mod marian_provider;
mod model_storage;
pub mod ocr;
mod onnx_runtime;
pub mod optional_models;
pub mod pdf_layers;
pub mod practice;
pub mod qwen;
mod review_schedule;
pub use review_schedule::FsrsMemory;
mod resources;
mod space;
pub mod supertonic;
pub mod translation;
pub mod tts;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<document_layout::DocumentBlock>,
    pub sentences: Vec<Sentence>,
    pub position: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_resume: Option<SpeechResume>,
    #[serde(default)]
    pub bookmarks: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reviewed_translations: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_lines: Option<Vec<ocr::OcrLine>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_pdf_pages: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_page_starts: Option<Vec<Option<usize>>>,
    pub created_at: i64,
    pub source_hash: String,
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechResume {
    pub sentence_id: String,
    pub text_hash: String,
    pub offset_utf16: usize,
}

impl SpeechResume {
    fn matches(&self, sentence: &Sentence) -> bool {
        if self.sentence_id != sentence.id
            || self.text_hash != format!("{:x}", Sha256::digest(sentence.text.as_bytes()))
        {
            return false;
        }
        let mut offset = 0;
        for grapheme in sentence.text.graphemes(true) {
            if offset >= self.offset_utf16 {
                return offset == self.offset_utf16;
            }
            offset += grapheme.encode_utf16().count();
        }
        false
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReviewState {
    pub due_at: i64,
    pub interval_days: u32,
    pub reviews: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsrs: Option<FsrsMemory>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<Box<LemmaSnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<Box<ExpressionSnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sense: Option<Box<SenseSnapshot>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_translations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionSnapshot {
    pub canonical: String,
    pub surface: String,
    pub kind: String,
    pub observations: u32,
    pub source: String,
    pub source_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LemmaSnapshot {
    pub lemma: String,
    pub surface: String,
    pub part_of_speech: String,
    pub features: String,
    pub source: String,
    pub source_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
}

impl Vocabulary {
    fn surface_key(&self, word: &str) -> String {
        format!(
            "{}|{}|{}|{}",
            self.language,
            self.target_language,
            word.to_lowercase(),
            self.translation.to_lowercase()
        )
    }

    fn word_key(&self) -> String {
        if let Some(sense) = &self.sense {
            return format!(
                "sense:{}",
                serde_json::json!([self.language, self.target_language, sense.id])
            );
        }
        if let Some(expression) = &self.expression {
            return format!(
                "expression:{}",
                serde_json::json!([
                    self.language,
                    self.target_language,
                    knowledge::word_key(&expression.canonical),
                    expression.kind,
                    knowledge::word_key(&self.translation),
                ])
            );
        }
        match &self.analysis {
            None => self.surface_key(&self.word),
            Some(analysis) => format!(
                "lemma:{}",
                serde_json::json!([
                    self.language,
                    self.target_language,
                    knowledge::word_key(&analysis.lemma),
                    analysis.part_of_speech,
                    knowledge::word_key(&self.translation),
                ])
            ),
        }
    }

    fn validate_annotations(&self) -> Result<()> {
        if let Some(expression) = &self.expression {
            if self.analysis.is_some()
                || self.word != expression.canonical
                || !["fixed", "particle"].contains(&expression.kind.as_str())
                || expression.observations == 0
            {
                return Err("Некоректний вираз словникової картки".into());
            }
            for (value, limit) in [
                (&expression.canonical, 500),
                (&expression.surface, 500),
                (&expression.source, 120),
                (&expression.source_version, 80),
            ] {
                if value.trim().is_empty() || value.len() > limit {
                    return Err("Некоректний запис виразу словникової картки".into());
                }
            }
        }
        if let Some(analysis) = &self.analysis {
            for (value, limit) in [
                (&analysis.lemma, 500),
                (&analysis.surface, 500),
                (&analysis.part_of_speech, 20),
                (&analysis.source, 120),
                (&analysis.source_version, 80),
            ] {
                if value.trim().is_empty() || value.len() > limit {
                    return Err("Некоректний граматичний запис словникової картки".into());
                }
            }
            if self.word != analysis.lemma || analysis.features.len() > 1024 {
                return Err("Некоректні граматичні ознаки словникової картки".into());
            }
        }
        self.validate_learning()
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dictionary_sources: Vec<dictionary_transfer::DictionarySource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dictionary_review_origins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practice: Option<practice::PracticeProgress>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<BackupResource>,
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
    document_layout::parse(name, bytes).map(|parsed| parsed.text)
}

pub struct Store {
    connection: Connection,
    model_directory: std::path::PathBuf,
    optional_model_directory: std::path::PathBuf,
    knowledge_directory: std::path::PathBuf,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS documents (id TEXT PRIMARY KEY, body TEXT NOT NULL, source_hash TEXT UNIQUE NOT NULL); CREATE TABLE IF NOT EXISTS originals (document_id TEXT PRIMARY KEY REFERENCES documents(id), name TEXT NOT NULL, bytes BLOB NOT NULL); CREATE TABLE IF NOT EXISTS vocabulary (id TEXT PRIMARY KEY, word_key TEXT UNIQUE NOT NULL, body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS review_events (id INTEGER PRIMARY KEY, entry_id TEXT NOT NULL, direction TEXT NOT NULL, grade INTEGER NOT NULL, reviewed_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS translation_cache (key TEXT PRIMARY KEY, body TEXT NOT NULL); PRAGMA user_version=1;").map_err(|e| e.to_string())?;
        connection.execute("CREATE TABLE IF NOT EXISTS speech_resume (document_id TEXT PRIMARY KEY REFERENCES documents(id), body TEXT NOT NULL)", [])
            .map_err(|e| e.to_string())?;
        connection.execute("CREATE TABLE IF NOT EXISTS practice (id INTEGER PRIMARY KEY CHECK(id=1), body TEXT NOT NULL)", [])
            .map_err(|e| e.to_string())?;
        connection
            .execute(
                "CREATE INDEX IF NOT EXISTS review_card_direction_time
             ON review_events(entry_id, direction, reviewed_at)",
                [],
            )
            .map_err(|error| error.to_string())?;
        dictionary_transfer::initialize(&connection)?;
        Ok(Self {
            connection,
            model_directory: path.with_file_name("translation-models"),
            optional_model_directory: path.with_file_name("optional-models"),
            knowledge_directory: path.with_file_name("languages"),
        })
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
        let mut documents = self
            .connection
            .prepare("SELECT d.body, s.body FROM documents d LEFT JOIN speech_resume s ON s.document_id=d.id ORDER BY d.rowid DESC")
            .map_err(|e| e.to_string())?;
        let documents = documents
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .map(|row| {
                let (body, resume) = row.map_err(|e| e.to_string())?;
                let mut document: Document =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                if document
                    .ocr_lines
                    .as_ref()
                    .is_some_and(|lines| lines.iter().any(|line| line.source_span.is_none()))
                {
                    ocr::refresh_provenance(&mut document);
                }
                document.speech_resume = resume
                    .as_deref()
                    .and_then(|body| serde_json::from_str::<SpeechResume>(body).ok())
                    .filter(|resume| {
                        document
                            .sentences
                            .iter()
                            .any(|sentence| resume.matches(sentence))
                    });
                Ok(document)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Snapshot {
            documents,
            vocabulary: rows(
                &self.connection,
                "SELECT body FROM vocabulary ORDER BY rowid DESC",
            )?,
        })
    }
    pub fn import(&mut self, name: &str, bytes: &[u8], language: &str) -> Result<Document> {
        self.import_document(name, bytes, language, None, None)
    }

    pub fn import_article(
        &mut self,
        name: &str,
        bytes: &[u8],
        language: &str,
        url: &str,
    ) -> Result<Document> {
        let address = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"));
        if !name.to_ascii_lowercase().ends_with(".txt")
            || url.len() > 4096
            || url.chars().any(|c| c.is_whitespace() || c.is_control())
            || address.is_none_or(|address| address.is_empty() || address.starts_with('/'))
        {
            return Err("Некоректна адреса джерела статті.".into());
        }
        self.import_document(name, bytes, language, None, Some(url))
    }

    pub fn import_image(
        &mut self,
        name: &str,
        bytes: &[u8],
        language: &str,
        lines: Vec<ocr::OcrLine>,
    ) -> Result<Document> {
        self.import_document(name, bytes, language, Some((lines, None, false)), None)
    }

    pub fn import_scanned_pdf(
        &mut self,
        name: &str,
        bytes: &[u8],
        language: &str,
        lines: Vec<ocr::OcrLine>,
        pages: u16,
    ) -> Result<Document> {
        self.import_document(
            name,
            bytes,
            language,
            Some((lines, Some(pages), false)),
            None,
        )
    }

    pub fn import_mixed_pdf(
        &mut self,
        name: &str,
        bytes: &[u8],
        language: &str,
        lines: Vec<ocr::OcrLine>,
        pages: u16,
    ) -> Result<Document> {
        self.import_document(
            name,
            bytes,
            language,
            Some((lines, Some(pages), true)),
            None,
        )
    }

    fn import_document(
        &mut self,
        name: &str,
        bytes: &[u8],
        language: &str,
        ocr: Option<(Vec<ocr::OcrLine>, Option<u16>, bool)>,
        source_url: Option<&str>,
    ) -> Result<Document> {
        let (ocr_lines, ocr_pdf_pages, mixed) = ocr
            .map_or((None, None, false), |(lines, pages, mixed)| {
                (Some(lines), pages, mixed)
            });
        valid_language(language)?;
        let parsed = match (ocr_lines.as_deref(), ocr_pdf_pages, mixed) {
            (Some(lines), Some(pages), true) => {
                let (text, offsets) = pdf_layers::merge(name, bytes, pages, lines)?;
                Some(document_layout::from_transcript(text, Some(offsets)))
            }
            (Some(lines), pages, false) => Some(document_layout::from_ocr(
                ocr::recognized_text(name, bytes, lines, pages)?,
                lines,
                pages,
            )),
            (None, None, false) => None,
            _ => return Err("Некоректні параметри імпорту PDF".into()),
        };
        let hash = format!("{:x}", Sha256::digest(bytes));
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM documents WHERE source_hash=?",
                [&hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(body) = existing {
            let mut existing: Document = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            if mixed && (existing.pdf_page_starts.is_none() || existing.ocr_pdf_pages.is_none()) {
                if existing.language != language {
                    return Err("Виберіть мову вже імпортованого PDF перед розпізнаванням".into());
                }
                let (Some(parsed), Some(lines), Some(pages)) = (parsed, ocr_lines, ocr_pdf_pages)
                else {
                    return Err("Некоректні параметри імпорту PDF".into());
                };
                let mut updated = existing.clone();
                self.edit_document(&existing.id, |document, tx| {
                    ocr::update_transcript(document, parsed.text)?;
                    Self::discard_changed_speech_resume(tx, document)?;
                    document.ocr_lines = Some(lines);
                    document.ocr_pdf_pages = Some(pages);
                    document.pdf_page_starts = parsed.pdf_page_starts;
                    document.blocks = parsed.blocks;
                    ocr::refresh_provenance(document);
                    updated = document.clone();
                    Ok(true)
                })?;
                return Ok(updated);
            }
            if existing.blocks.is_empty() {
                // Reimports may rebuild the new derived layout without touching
                // the stored canonical text, sentence IDs or user state.
                let parsed = if let Some(parsed) = parsed {
                    Some(parsed)
                } else {
                    document_layout::parse(name, bytes).ok()
                };
                if let Some(parsed) = parsed.filter(|parsed| parsed.text == existing.text) {
                    let id = existing.id.clone();
                    self.edit_document(&id, |document, _| {
                        if !document.blocks.is_empty() || parsed.text != document.text {
                            existing = document.clone();
                            return Ok(false);
                        }
                        document.pdf_page_starts = parsed.pdf_page_starts;
                        document.blocks = parsed.blocks;
                        ocr::refresh_provenance(document);
                        existing = document.clone();
                        Ok(true)
                    })?;
                }
            }
            if let Some(incoming) = ocr_lines {
                let id = existing.id.clone();
                self.edit_document(&id, |document, _| {
                    let Some(stored) = document.ocr_lines.as_mut() else {
                        return Ok(false);
                    };
                    let mut changed = false;
                    if stored.len() == incoming.len()
                        && stored.iter().zip(&incoming).all(|(old, new)| {
                            old.page == new.page && old.text.trim() == new.text.trim()
                        })
                    {
                        for (old, new) in stored.iter_mut().zip(incoming) {
                            if old.words.is_empty() && !new.words.is_empty() {
                                old.words = new.words;
                                changed = true;
                            }
                        }
                    }
                    ocr::refresh_provenance(document);
                    existing = document.clone();
                    Ok(changed)
                })?;
            } else if existing.ocr_lines.is_some() {
                ocr::refresh_provenance(&mut existing);
            }
            if let Some(url) = source_url.filter(|_| existing.source_url.is_none()) {
                self.edit_document(&existing.id.clone(), |document, _| {
                    if document.source_url.is_none() {
                        document.source_url = Some(url.to_owned());
                        existing = document.clone();
                        return Ok(true);
                    }
                    existing = document.clone();
                    Ok(false)
                })?;
            }
            return Ok(existing);
        }
        let document_layout::Extracted {
            text,
            blocks,
            pdf_page_starts,
        } = match parsed {
            Some(parsed) => parsed,
            None => document_layout::parse(name, bytes)?,
        };
        let id = Uuid::new_v4().to_string();
        let name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Document.txt");
        let mut document = Document {
            sentences: segment(&text, &id),
            id,
            title: Path::new(name)
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or(name)
                .into(),
            language: language.into(),
            text,
            blocks,
            position: 0,
            speech_resume: None,
            bookmarks: Vec::new(),
            notes: BTreeMap::new(),
            reviewed_translations: BTreeMap::new(),
            ocr_lines,
            ocr_pdf_pages,
            pdf_page_starts,
            created_at: now(),
            source_hash: hash,
            source_url: source_url.map(str::to_owned),
            format: Path::new(name)
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("txt")
                .to_lowercase(),
        };
        ocr::refresh_provenance(&mut document);
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
        edit: impl FnOnce(&mut Document, &Connection) -> Result<bool>,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let body: String = tx
            .query_row("SELECT body FROM documents WHERE id=?", [id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut document: Document = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        if edit(&mut document, &tx)? {
            let body = serde_json::to_string(&document).map_err(|e| e.to_string())?;
            tx.execute("UPDATE documents SET body=? WHERE id=?", params![body, id])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn correct_ocr_line(
        &mut self,
        id: &str,
        index: usize,
        correction: Option<String>,
    ) -> Result<()> {
        let original = self.original(id)?;
        self.edit_document(id, |document, tx| {
            let lines = document
                .ocr_lines
                .as_mut()
                .ok_or("У документа немає розпізнаних рядків")?;
            let line = lines
                .get_mut(index)
                .ok_or("Розпізнаний рядок не знайдено")?;
            let correction = correction.map(|text| text.trim().to_owned());
            if line.correction == correction {
                return Ok(false);
            }
            line.correction = correction;
            let parsed = if let (Some(pages), Some(_)) =
                (document.ocr_pdf_pages, &document.pdf_page_starts)
            {
                let (text, starts) =
                    pdf_layers::merge(&original.name, &original.bytes, pages, lines)?;
                document_layout::from_transcript(text, Some(starts))
            } else {
                let text = ocr::recognized_text(
                    &original.name,
                    &original.bytes,
                    lines,
                    document.ocr_pdf_pages,
                )?;
                document_layout::from_ocr(text, lines, document.ocr_pdf_pages)
            };
            ocr::update_transcript(document, parsed.text)?;
            document.pdf_page_starts = parsed.pdf_page_starts;
            document.blocks = parsed.blocks;
            ocr::refresh_provenance(document);
            Self::discard_changed_speech_resume(tx, document)?;
            Ok(true)
        })
    }

    pub fn set_document_language(&mut self, id: &str, language: &str) -> Result<()> {
        valid_language(language)?;
        self.edit_document(id, |document, _| {
            if document.language == language {
                return Ok(false);
            }
            if document.reviewed_translations.contains_key(language) {
                return Err("Для нової мови джерела вже збережено переклади. Збережіть їх окремо і видаліть із документа перед зміною мови.".into());
            }
            document.language = language.to_owned();
            Ok(true)
        })
    }

    pub fn position(&mut self, id: &str, position: usize) -> Result<()> {
        self.edit_document(id, |document, _| {
            if position >= document.sentences.len() {
                return Err("Некоректна позиція".into());
            }
            let changed = document.position != position;
            document.position = position;
            Ok(changed)
        })
    }

    fn read_speech_resume(connection: &Connection, id: &str) -> Result<Option<SpeechResume>> {
        let body: Option<String> = connection
            .query_row(
                "SELECT body FROM speech_resume WHERE document_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        body.as_deref()
            .map(|body| serde_json::from_str(body).map_err(|e| e.to_string()))
            .transpose()
    }

    fn discard_changed_speech_resume(connection: &Connection, document: &Document) -> Result<()> {
        if Self::read_speech_resume(connection, &document.id)?.is_some_and(|resume| {
            !document
                .sentences
                .iter()
                .any(|sentence| resume.matches(sentence))
        }) {
            connection
                .execute(
                    "DELETE FROM speech_resume WHERE document_id=?1",
                    [&document.id],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn speech_resume(&self, id: &str) -> Result<Option<SpeechResume>> {
        Self::read_speech_resume(&self.connection, id)
    }

    pub fn set_speech_resume(
        &mut self,
        id: &str,
        sentence_id: &str,
        text_hash: &str,
        offset_utf16: Option<usize>,
    ) -> Result<()> {
        if let Some(offset_utf16) = offset_utf16 {
            let resume = SpeechResume {
                sentence_id: sentence_id.into(),
                text_hash: text_hash.into(),
                offset_utf16,
            };
            Self::write_speech_resume(&self.connection, id, &resume)
        } else {
            self.connection.execute(
                "DELETE FROM speech_resume WHERE document_id=?1 AND json_extract(body, '$.sentenceId')=?2 AND json_extract(body, '$.textHash')=?3",
                params![id, sentence_id, text_hash],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }
    }

    fn write_speech_resume(connection: &Connection, id: &str, resume: &SpeechResume) -> Result<()> {
        let body = serde_json::to_string(resume).map_err(|e| e.to_string())?;
        connection.execute(
            "INSERT INTO speech_resume VALUES (?1,?2) ON CONFLICT(document_id) DO UPDATE SET body=excluded.body WHERE body!=excluded.body",
            params![id, body],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_bookmark(&mut self, id: &str, sentence_id: &str, saved: bool) -> Result<()> {
        self.edit_document(id, |document, _| {
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

    pub fn set_sentence_note(&mut self, id: &str, sentence_id: &str, note: &str) -> Result<()> {
        let note = note.trim();
        validate_sentence_note(note)?;
        self.edit_document(id, |document, _| {
            if !document
                .sentences
                .iter()
                .any(|sentence| sentence.id == sentence_id)
            {
                return Err("Речення для нотатки не знайдено".into());
            }
            if note.is_empty() {
                return Ok(document.notes.remove(sentence_id).is_some());
            }
            if document
                .notes
                .get(sentence_id)
                .is_some_and(|saved| saved == note)
            {
                return Ok(false);
            }
            document.notes.insert(sentence_id.into(), note.into());
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
        self.edit_document(id, |document, _| {
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
        if let Some(sense) = &mut entry.sense {
            sense.surface.clone_from(&entry.word);
        }
        if let Some(analysis) = &mut entry.analysis {
            analysis.lemma = analysis.lemma.trim().to_owned();
            analysis.surface = entry.word;
            analysis.context = None;
            analysis.document_id = None;
            entry.word = analysis.lemma.clone();
        }
        if let Some(expression) = &mut entry.expression {
            expression.canonical = expression
                .canonical
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            expression.context = None;
            expression.document_id = None;
            entry.word = expression.canonical.clone();
        }
        if let Some(sense) = &entry.sense {
            entry.word.clone_from(&sense.lemma);
            if let Some(analysis) = &mut entry.analysis {
                if knowledge::word_key(&analysis.lemma) == knowledge::word_key(&sense.lemma) {
                    analysis.lemma.clone_from(&sense.lemma);
                }
            }
        }
        entry.accepted_translations =
            card_learning::prepare_variants(&entry.translation, entry.accepted_translations)?;
        entry.validate_annotations()?;

        let key = entry.word_key();
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT body FROM vocabulary WHERE word_key=?",
                [&key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(body) = existing {
            let mut card: Vocabulary = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            card.merge_accepted_translations(&entry)?;
            tx.execute(
                "UPDATE vocabulary SET body=?1 WHERE id=?2",
                params![
                    serde_json::to_string(&card).map_err(|e| e.to_string())?,
                    card.id
                ],
            )
            .map_err(|e| e.to_string())?;
            if card.sense.is_some() && card.document_id != entry.document_id {
                dictionary_transfer::link_card_document(&tx, &card.id, &entry.document_id)?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(card);
        }
        if entry.sense.is_none() && (entry.analysis.is_some() || entry.expression.is_some()) {
            let legacy_key = entry.surface_key(&entry.word);
            let legacy: Option<String> = tx
                .query_row(
                    "SELECT body FROM vocabulary WHERE word_key=?1",
                    [&legacy_key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if let Some(body) = legacy {
                let mut card: Vocabulary =
                    serde_json::from_str(&body).map_err(|error| error.to_string())?;
                card.word.clone_from(&entry.word);
                if entry.context != card.context || entry.document_id != card.document_id {
                    if let Some(analysis) = &mut entry.analysis {
                        analysis.context = Some(entry.context.clone());
                        analysis.document_id = Some(entry.document_id.clone());
                    }
                    if let Some(expression) = &mut entry.expression {
                        expression.context = Some(entry.context.clone());
                        expression.document_id = Some(entry.document_id.clone());
                    }
                }
                card.merge_accepted_translations(&entry)?;
                card.analysis = entry.analysis;
                card.expression = entry.expression;
                tx.execute(
                    "UPDATE vocabulary SET word_key=?1, body=?2 WHERE id=?3",
                    params![
                        key,
                        serde_json::to_string(&card).map_err(|error| error.to_string())?,
                        card.id
                    ],
                )
                .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())?;
                return Ok(card);
            }
        }
        entry.id = Uuid::new_v4().to_string();
        entry.forward = ReviewState::default();
        entry.reverse = ReviewState::default();
        tx.execute(
            "INSERT INTO vocabulary VALUES (?1,?2,?3)",
            params![entry.id, key, serde_json::to_string(&entry).unwrap()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(entry)
    }
    pub fn review(&mut self, id: &str, direction: &str, grade: u8) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        Self::record_review(&tx, id, direction, grade)?;
        tx.commit().map_err(|error| error.to_string())
    }

    fn record_review(connection: &Connection, id: &str, direction: &str, grade: u8) -> Result<()> {
        if !["forward", "reverse"].contains(&direction) || grade > 3 {
            return Err("Некоректна відповідь".into());
        }
        let body: String = connection
            .query_row("SELECT body FROM vocabulary WHERE id=?", [id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut entry: Vocabulary = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let state = if direction == "forward" {
            &mut entry.forward
        } else {
            &mut entry.reverse
        };
        let time = now();
        review_schedule::review(connection, id, direction, state, grade, time)?;
        connection
            .execute(
                "UPDATE vocabulary SET body=? WHERE id=?",
                params![serde_json::to_string(&entry).unwrap(), id],
            )
            .map_err(|e| e.to_string())?;
        connection.execute(
            "INSERT INTO review_events(entry_id,direction,grade,reviewed_at) VALUES (?1,?2,?3,?4)",
            params![id, direction, grade, time],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
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
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        let snapshot = self.snapshot()?;
        let originals = snapshot
            .documents
            .iter()
            .map(|d| self.original(&d.id))
            .collect::<Result<Vec<_>>>()?;
        let (dictionary_sources, review_records) =
            dictionary_transfer::export_details(&tx, &snapshot.vocabulary)?;
        let (dictionary_review_origins, reviews) = review_records
            .into_iter()
            .map(|record| (record.id, record.event))
            .unzip();
        let backup = Backup {
            version: 1,
            snapshot,
            originals,
            reviews,
            dictionary_sources,
            dictionary_review_origins,
            practice: practice::read(&tx)?,
            resources: self.backup_resources(),
        };
        tx.commit().map_err(|error| error.to_string())?;
        Ok(backup)
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
            let parsed = match (
                &doc.ocr_lines,
                doc.ocr_pdf_pages,
                doc.pdf_page_starts.is_some(),
            ) {
                (Some(lines), Some(pages), true) => {
                    let (text, offsets) =
                        pdf_layers::merge(&original.name, &original.bytes, pages, lines)?;
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
            let mut d = doc.clone();
            d.text = parsed.text;
            d.blocks = parsed.blocks;
            d.pdf_page_starts = parsed.pdf_page_starts;
            ocr::refresh_provenance(&mut d);
            d.sentences = segment(&d.text, &d.id);
            d.position = d.position.min(d.sentences.len().saturating_sub(1));
            d.speech_resume = d
                .speech_resume
                .filter(|resume| d.sentences.iter().any(|sentence| resume.matches(sentence)));
            if !d.bookmarks.is_empty() || !d.reviewed_translations.is_empty() || !d.notes.is_empty()
            {
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
                for (sentence_id, note) in &d.notes {
                    if !known.contains(sentence_id.as_str()) || note.trim().is_empty() {
                        return Err(
                            "В архіві є нотатка для невідомого речення або порожня нотатка".into(),
                        );
                    }
                    validate_sentence_note(note)?;
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
            validated.push((d, original));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (d, o) in validated {
            let mut d = d;
            let speech_resume = d.speech_resume.take();
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO documents VALUES (?1,?2,?3)",
                    params![d.id, serde_json::to_string(&d).unwrap(), d.source_hash],
                )
                .map_err(|e| e.to_string())?;
            if inserted > 0 {
                if let Some(resume) = speech_resume {
                    Self::write_speech_resume(&tx, &d.id, &resume)?;
                }
                tx.execute(
                    "INSERT INTO originals VALUES (?1,?2,?3)",
                    params![d.id, o.name, o.bytes],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let review_cards = dictionary_transfer::restore_backup_dictionary(
            &tx,
            backup.snapshot.vocabulary,
            backup.reviews,
            backup.dictionary_review_origins,
            backup.dictionary_sources,
            &backup.snapshot.documents,
        )?;
        if let Some(progress) = backup.practice {
            practice::restore_if_fresh(&tx, progress, &review_cards)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

fn validate_sentence_note(note: &str) -> Result<()> {
    if note.len() > 4000
        || note
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("Нотатка задовга або містить керівні символи".into());
    }
    Ok(())
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
                analysis: None,
                expression: None,
                sense: None,
                accepted_translations: Vec::new(),
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
    fn correcting_document_language_keeps_progress_and_checks_reviewed_targets() {
        let mut db = Store::open(Path::new(":memory:")).unwrap();
        let bytes = "Перше речення. Друге речення.".as_bytes();
        let doc = db.import("hello.txt", bytes, "en").unwrap();
        let sentence = &doc.sentences[1];
        db.position(&doc.id, 1).unwrap();
        db.set_bookmark(&doc.id, &sentence.id, true).unwrap();
        db.set_reviewed_translation(&doc.id, &sentence.id, "fr", "Deuxième phrase.")
            .unwrap();
        db.set_document_language(&doc.id, "uk").unwrap();
        assert!(db.set_document_language(&doc.id, "fr").is_err());
        assert!(db.set_document_language(&doc.id, "zz").is_err());
        let saved = &db.snapshot().unwrap().documents[0];
        assert_eq!(saved.language, "uk");
        assert_eq!(saved.position, 1);
        assert_eq!(saved.sentences[1].id, sentence.id);
        assert_eq!(
            saved.bookmarks.as_slice(),
            std::slice::from_ref(&sentence.id)
        );
        assert_eq!(
            saved.reviewed_translations["fr"][&sentence.id],
            "Deuxième phrase."
        );
        assert_eq!(db.original(&doc.id).unwrap().bytes, bytes);
    }
    #[test]
    fn selected_lemma_preserves_learning_across_forms_and_backup() {
        let mut db = Store::open(Path::new(":memory:")).unwrap();
        let make_card =
            |word: &str, translation: &str, pos: Option<&str>, context: &str| Vocabulary {
                id: String::new(),
                word: word.into(),
                translation: translation.into(),
                language: "en".into(),
                target_language: "uk".into(),
                context: context.into(),
                document_id: "story".into(),
                forward: ReviewState::default(),
                reverse: ReviewState::default(),
                expression: None,
                analysis: pos.map(|pos| {
                    Box::new(LemmaSnapshot {
                        lemma: "read".into(),
                        surface: word.into(),
                        part_of_speech: pos.into(),
                        features: "Tense=Pres".into(),
                        source: "UD English EWT".into(),
                        source_version: "2.18".into(),
                        context: None,
                        document_id: None,
                    })
                }),
                sense: None,
                accepted_translations: Vec::new(),
            };
        let legacy = db
            .save_word(make_card("read", "читати", None, "I read every day."))
            .unwrap();
        db.review(&legacy.id, "forward", 2).unwrap();
        let first = db
            .save_word(make_card("reads", "читати", Some("VERB"), "She reads."))
            .unwrap();
        let second = db
            .save_word(make_card(
                "reading",
                "читати",
                Some("VERB"),
                "He is reading.",
            ))
            .unwrap();
        assert_eq!(first.id, legacy.id);
        assert_eq!(second.id, first.id);
        assert_eq!(second.word, "read");
        assert_eq!(second.forward.reviews, 1);
        assert_eq!(second.reverse.reviews, 0);
        let snapshot = second.analysis.unwrap();
        assert_eq!(snapshot.surface, "reads");
        assert_eq!(second.context, legacy.context);
        assert_eq!(snapshot.context.as_deref(), Some("She reads."));
        assert_ne!(
            db.save_word(make_card("reads", "читати", Some("NOUN"), ""))
                .unwrap()
                .id,
            first.id
        );
        assert_ne!(
            db.save_word(make_card("read", "прочитати", Some("VERB"), ""))
                .unwrap()
                .id,
            first.id
        );

        let mut restored = Store::open(Path::new(":memory:")).unwrap();
        let archive = serde_json::to_vec(&db.export().unwrap()).unwrap();
        restored
            .restore(serde_json::from_slice(&archive).unwrap())
            .unwrap();
        let again = restored
            .save_word(make_card("reading", "читати", Some("VERB"), ""))
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.forward.reviews, 1);
        assert_eq!(restored.snapshot().unwrap().vocabulary.len(), 3);
        assert_eq!(restored.export().unwrap().reviews[0].entry_id, first.id);
    }
    #[test]
    fn expression_cards_reuse_canonical_identity_and_preserve_evidence() {
        let mut db = Store::open(Path::new(":memory:")).unwrap();
        let make_card = |surface: &str, translation: &str, context: &str| Vocabulary {
            id: String::new(),
            word: "off".into(),
            translation: translation.into(),
            language: "en".into(),
            target_language: "uk".into(),
            context: context.into(),
            document_id: "story".into(),
            forward: ReviewState::default(),
            reverse: ReviewState::default(),
            analysis: None,
            expression: Some(Box::new(ExpressionSnapshot {
                canonical: "turn off".into(),
                surface: surface.into(),
                kind: "particle".into(),
                observations: 2,
                source: "UD English EWT".into(),
                source_version: "2.18".into(),
                context: None,
                document_id: None,
            })),
            sense: None,
            accepted_translations: Vec::new(),
        };
        let mut manual = make_card("turn off", "вимикати", "Turn off the fan.");
        manual.word = "turn off".into();
        manual.expression = None;
        let legacy = db.save_word(manual).unwrap();
        db.review(&legacy.id, "forward", 2).unwrap();
        let reviewed = db
            .save_word(make_card(
                "turned … off",
                "вимикати",
                "She turned the light off.",
            ))
            .unwrap();
        let again = db
            .save_word(make_card(
                "turn off",
                "вимикати",
                "Please turn off the fan.",
            ))
            .unwrap();
        assert_eq!(reviewed.id, legacy.id);
        assert_eq!(again.id, reviewed.id);
        assert_eq!(again.word, "turn off");
        assert_eq!(again.forward.reviews, 1);
        assert_eq!(again.context, "Turn off the fan.");
        let different = db
            .save_word(make_card("turned … off", "відвертати", ""))
            .unwrap();
        assert_ne!(different.id, again.id);

        let mut clean = Store::open(Path::new(":memory:")).unwrap();
        clean.restore(db.export().unwrap()).unwrap();
        let entry = clean
            .save_word(make_card("turn off", "вимикати", ""))
            .unwrap();
        let snapshot = entry.expression.unwrap();
        assert_eq!(snapshot.surface, "turned … off");
        assert_eq!(
            snapshot.context.as_deref(),
            Some("She turned the light off.")
        );
        assert_eq!(snapshot.source, "UD English EWT");
        assert_eq!(clean.export().unwrap().reviews[0].entry_id, again.id);
    }

    #[test]
    fn speech_checkpoint_restarts_and_survives_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speech.db");
        let mut db = Store::open(&path).unwrap();
        let text = "😀 Cafe\u{301} continue.";
        let doc = db.import("speech.txt", text.as_bytes(), "en").unwrap();
        let sentence = &doc.sentences[0];
        let hash = format!("{:x}", Sha256::digest(sentence.text.as_bytes()));
        let offset = "😀 Cafe\u{301} ".encode_utf16().count();
        db.set_speech_resume(&doc.id, &sentence.id, &hash, Some(offset))
            .unwrap();
        // A completion for another sentence must leave the current checkpoint intact.
        db.set_speech_resume(&doc.id, "other", &hash, None).unwrap();
        drop(db);

        let db = Store::open(&path).unwrap();
        assert_eq!(
            db.speech_resume(&doc.id).unwrap().unwrap().offset_utf16,
            offset
        );
        let mut restored = Store::open(&dir.path().join("restored-speech.db")).unwrap();
        let backup = db.export().unwrap();
        let archive = serde_json::to_vec(&backup).unwrap();
        restored
            .restore(serde_json::from_slice(&archive).unwrap())
            .unwrap();
        assert_eq!(
            restored
                .speech_resume(&doc.id)
                .unwrap()
                .unwrap()
                .offset_utf16,
            offset
        );
        restored
            .set_speech_resume(&doc.id, &sentence.id, &hash, None)
            .unwrap();
        assert!(restored.speech_resume(&doc.id).unwrap().is_none());

        for invalid in [1, offset - 2, sentence.end_utf16] {
            // Reject progress that splits a surrogate or combining sequence, or points past speech.
            let mut corrupt = backup.clone();
            corrupt.snapshot.documents[0]
                .speech_resume
                .as_mut()
                .unwrap()
                .offset_utf16 = invalid;
            let mut clean = Store::open(Path::new(":memory:")).unwrap();
            clean.restore(corrupt).unwrap();
            assert!(clean.speech_resume(&doc.id).unwrap().is_none());
        }
    }
    #[test]
    fn changing_recognized_sentence_discards_its_speech_checkpoint() {
        let mut db = Store::open(Path::new(":memory:")).unwrap();
        let doc = db
            .import_image(
                "photo.png",
                b"\x89PNG\r\n\x1a\n",
                "en",
                vec![ocr::OcrLine {
                    page: 0,
                    text: "This is the original sentence.".into(),
                    x: 0.0,
                    y: 0.0,
                    width: 0.9,
                    height: 0.2,
                    confidence: None,
                    correction: None,
                    words: Vec::new(),
                    source_span: None,
                }],
            )
            .unwrap();
        let sentence = &doc.sentences[0];
        let hash = format!("{:x}", Sha256::digest(sentence.text.as_bytes()));
        db.set_speech_resume(&doc.id, &sentence.id, &hash, Some(5))
            .unwrap();
        db.correct_ocr_line(&doc.id, 0, Some("This is the corrected sentence.".into()))
            .unwrap();
        assert_eq!(
            db.snapshot().unwrap().documents[0].sentences[0].id,
            sentence.id
        );
        assert!(db.speech_resume(&doc.id).unwrap().is_none());
        db.correct_ocr_line(&doc.id, 0, None).unwrap();
        assert!(db.speech_resume(&doc.id).unwrap().is_none());
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
        use std::io::{Cursor, Write};
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
