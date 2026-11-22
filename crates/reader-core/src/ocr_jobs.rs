use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{now, ocr::OcrLine, valid_language, Document, Result, Store};

const MAX_PAGE_RESULT: usize = 32 * 1024 * 1024;
const JOB_COLUMNS: &str = "id, kind, input_hash, name, language, engine_version, page_count,
    missing_pages, attempts, state, document_id,
    CASE WHEN state = 'completed' THEN json_array_length(missing_pages)
        ELSE (SELECT COUNT(*) FROM ocr_job_pages WHERE job_id = ocr_jobs.id) END";

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JobRequest {
    List {
        #[serde(default)]
        include_terminal: bool,
    },
    Create {
        hash: String,
        name: String,
        language: String,
        engine_version: String,
        pages: u16,
        missing_pages: Vec<u16>,
        #[serde(default)]
        revive: bool,
    },
    SetState {
        id: String,
        state: JobState,
        document_id: Option<String>,
    },
    ReadPage {
        id: String,
        page: u16,
    },
    SavePage {
        id: String,
        page: u16,
        lines: Vec<OcrLine>,
        #[serde(default)]
        adopt: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Completed,
    Failed,
    Canceled,
}

impl JobState {
    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    kind: String,
    hash: String,
    name: String,
    language: String,
    engine_version: String,
    pages: u16,
    missing_pages: Vec<u16>,
    attempts: u32,
    state: String,
    document_id: Option<String>,
    saved_pages: u16,
}

pub(crate) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS ocr_jobs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            input_hash TEXT NOT NULL,
            name TEXT NOT NULL,
            language TEXT NOT NULL,
            engine_version TEXT NOT NULL,
            page_count INTEGER NOT NULL,
            missing_pages TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','running','completed','failed','canceled')),
            updated_at INTEGER NOT NULL,
            document_id TEXT REFERENCES documents(id) ON DELETE SET NULL,
            UNIQUE(kind, input_hash, language)
        );
        CREATE TABLE IF NOT EXISTS ocr_job_pages (
            job_id TEXT NOT NULL REFERENCES ocr_jobs(id) ON DELETE CASCADE,
            page INTEGER NOT NULL,
            result TEXT NOT NULL,
            PRIMARY KEY(job_id, page)
        );",
    ).map_err(|error| error.to_string())
}

impl Store {
    pub fn pdf_ocr_job(&mut self, request: JobRequest) -> Result<Value> {
        let connection = &mut self.connection;
        match request {
            JobRequest::List { include_terminal } => {
                let mut statement = connection.prepare(&format!(
                    "SELECT {JOB_COLUMNS} FROM ocr_jobs WHERE ?1 OR state NOT IN ('completed', 'canceled') ORDER BY name, id"
                )).map_err(|error| error.to_string())?;
                let jobs = statement
                    .query_map([include_terminal], decode_job)
                    .map_err(|error| error.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|error| error.to_string())?;
                json(jobs)
            }
            JobRequest::Create {
                hash,
                name,
                language,
                engine_version,
                pages,
                missing_pages,
                revive,
            } => {
                valid_language(&language)?;
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || name.len() > 4096
                    || !name.to_ascii_lowercase().ends_with(".pdf")
                    || name.contains(['\0', '/', '\\'])
                    || engine_version.is_empty()
                    || engine_version.len() > 128
                    || pages == 0
                    || missing_pages.is_empty()
                    || missing_pages.iter().any(|&page| page >= pages)
                    || missing_pages.windows(2).any(|pair| pair[0] >= pair[1])
                {
                    return Err("Некоректні параметри завдання OCR".into());
                }
                connection.execute(
                    "INSERT INTO ocr_jobs(id, kind, input_hash, name, language, engine_version, page_count, missing_pages, updated_at)
                    VALUES (?1, 'pdf_ocr', ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(kind, input_hash, language) DO UPDATE SET name = excluded.name,
                    engine_version = excluded.engine_version, page_count = excluded.page_count, missing_pages = excluded.missing_pages,
                    state = 'pending', updated_at = excluded.updated_at WHERE ocr_jobs.state = 'canceled' AND ?9",
                    params![uuid::Uuid::new_v4().to_string(), hash, name, language, engine_version, pages,
                        serde_json::to_string(&missing_pages).map_err(|error| error.to_string())?, now(), revive],
                ).map_err(|error| error.to_string())?;
                json(connection.query_row(&format!(
                    "SELECT {JOB_COLUMNS} FROM ocr_jobs WHERE kind = 'pdf_ocr' AND input_hash = ?1 AND language = ?2"
                ), params![hash, language], decode_job).map_err(|error| error.to_string())?)
            }
            JobRequest::ReadPage { id, page } => {
                let result = connection
                    .query_row(
                        "SELECT result FROM ocr_job_pages WHERE job_id = ?1 AND page = ?2",
                        params![id, page],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| error.to_string())?;
                result
                    .map(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))
                    .transpose()
                    .map(|value| value.unwrap_or(Value::Null))
            }
            JobRequest::SavePage {
                id,
                page,
                lines,
                adopt,
            } => {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|error| error.to_string())?;
                let job = read_job(&tx, &id)?;
                if !job.missing_pages.contains(&page)
                    || (!adopt && job.state != "running")
                    || matches!(job.state.as_str(), "completed" | "canceled")
                    || lines.iter().any(|line| line.page != page)
                {
                    return Err("Сторінка не відповідає поточному завданню OCR".into());
                }
                if !lines.is_empty() {
                    if let Err(error) =
                        crate::ocr::recognized_text(&job.name, b"%PDF-", &lines, Some(job.pages))
                    {
                        // Legacy checkpoints can be missing or invalid; leave the page eligible for OCR.
                        if adopt {
                            return Ok(Value::Null);
                        }
                        return Err(error);
                    }
                }
                let result = serde_json::to_string(&lines).map_err(|error| error.to_string())?;
                if result.len() > MAX_PAGE_RESULT {
                    return Err("Результат OCR сторінки завеликий".into());
                }
                tx.execute(
                    "INSERT OR IGNORE INTO ocr_job_pages (job_id, page, result) VALUES (?1, ?2, ?3)",
                    params![id, page, result],
                ).map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())?;
                Ok(Value::Null)
            }
            JobRequest::SetState {
                id,
                state,
                document_id,
            } => {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|error| error.to_string())?;
                let job = read_job(&tx, &id)?;
                if job.state == state.name() && state != JobState::Running {
                    return Ok(Value::Null);
                }
                if matches!(job.state.as_str(), "completed" | "canceled") {
                    return Err("Завдання OCR уже завершено".into());
                }
                if state == JobState::Completed {
                    let document_id = document_id
                        .as_deref()
                        .ok_or("Для завершення OCR потрібен імпортований документ")?;
                    let body = tx
                        .query_row(
                            "SELECT body FROM documents WHERE id = ?1 AND source_hash = ?2",
                            params![document_id, job.hash],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|error| error.to_string())?;
                    let document: Document =
                        serde_json::from_str(&body).map_err(|error| error.to_string())?;
                    if document.ocr_pdf_pages != Some(job.pages)
                        || document.pdf_page_starts.is_none()
                    {
                        return Err("Імпорт PDF не підтверджено".into());
                    }
                } else if document_id.is_some() {
                    return Err("Документ можна прив’язати лише до завершеного завдання OCR".into());
                }
                tx.execute(
                    "UPDATE ocr_jobs SET state = ?1, document_id = ?2, updated_at = ?3,
                    attempts = attempts + ?4 WHERE id = ?5",
                    params![
                        state.name(),
                        document_id,
                        now(),
                        u8::from(state == JobState::Running),
                        id
                    ],
                )
                .map_err(|error| error.to_string())?;
                if matches!(state, JobState::Completed | JobState::Canceled) {
                    tx.execute("DELETE FROM ocr_job_pages WHERE job_id = ?1", [&id])
                        .map_err(|error| error.to_string())?;
                }
                tx.commit().map_err(|error| error.to_string())?;
                Ok(Value::Null)
            }
        }
    }
}

fn read_job(connection: &Connection, id: &str) -> Result<Job> {
    connection
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM ocr_jobs WHERE id = ?1"),
            [id],
            decode_job,
        )
        .map_err(|error| error.to_string())
}

fn decode_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let data: String = row.get(7)?;
    let missing_pages = serde_json::from_str(&data).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Job {
        id: row.get(0)?,
        kind: row.get(1)?,
        hash: row.get(2)?,
        name: row.get(3)?,
        language: row.get(4)?,
        engine_version: row.get(5)?,
        pages: row.get(6)?,
        missing_pages,
        attempts: row.get(8)?,
        state: row.get(9)?,
        document_id: row.get(10)?,
        saved_pages: row.get(11)?,
    })
}

fn json(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ocr_checkpoints_survive_store_restart_and_retries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reader.db");
        let mut store = Store::open(&path).unwrap();
        let request = |store: &mut Store, value| {
            store
                .pdf_ocr_job(serde_json::from_value(value).unwrap())
                .unwrap()
        };
        let create = json!({"action":"create", "hash":"a".repeat(64), "name":"scan.pdf",
            "language":"uk", "engineVersion":"native/1", "pages":42, "missingPages":[0,41]});
        let job = request(&mut store, create.clone());
        let id = &job["id"];
        assert_eq!(job["state"], "pending");
        request(
            &mut store,
            json!({"action":"set_state", "id":id,"state":"running"}),
        );
        request(
            &mut store,
            json!({"action":"save_page", "id":id,"page":41,"lines":[]}),
        );
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let jobs = request(&mut store, json!({"action":"list"}));
        assert_eq!(jobs[0]["savedPages"], 1);
        assert_eq!(jobs[0]["attempts"], 1);
        assert_eq!(
            request(&mut store, json!({"action":"read_page","id":id,"page":41})),
            json!([])
        );
        assert_eq!(
            request(&mut store, json!({"action":"read_page","id":id,"page":0})),
            Value::Null
        );
        request(
            &mut store,
            json!({"action":"set_state", "id":id,"state":"running"}),
        );
        request(
            &mut store,
            json!({"action":"set_state", "id":id,"state":"failed"}),
        );
        let job = request(&mut store, create.clone());
        assert_eq!(job["attempts"], 2);
        assert_eq!(job["state"], "failed");
        assert_eq!(job["savedPages"], 1);
        request(
            &mut store,
            json!({"action":"set_state", "id":id,"state":"canceled"}),
        );
        assert_eq!(request(&mut store, create)["state"], "canceled");
        assert_eq!(
            request(&mut store, json!({"action":"read_page","id":id,"page":41})),
            Value::Null
        );
    }
}
