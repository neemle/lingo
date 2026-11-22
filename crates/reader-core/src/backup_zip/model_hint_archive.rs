use super::{archive_error, MAX_BACKUP_FILE_BYTES};
use crate::{BackupHintManifest as Metadata, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};

pub(super) const ENTRY: &str = "model-hints.jsonl";
const MAX_ROW_BYTES: u64 = 8192;

impl Metadata {
    pub(super) fn valid(&self) -> bool {
        self.count > 0
            && self.count <= self.bytes
            && self.bytes <= MAX_BACKUP_FILE_BYTES
            && is_hex_digest(&self.sha256)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Row {
    pub document_id: String,
    key: String,
    hint: Value,
}

impl Row {
    fn valid(&self) -> bool {
        if self.document_id.is_empty() || self.document_id.len() > 256 || !is_hex_digest(&self.key)
        {
            return false;
        }
        let Some(hint) = self.hint.as_object().filter(|fields| fields.len() == 2) else {
            return false;
        };
        if let Some(explanation) = hint.get("explanation") {
            return hint.get("cached").is_some_and(Value::is_boolean)
                && explanation
                    .as_str()
                    .is_some_and(|text| valid_note(text, 240));
        }
        ["spelling", "syntax"].into_iter().all(|field| {
            hint.get(field).is_some_and(|value| {
                value.is_null() || value.as_str().is_some_and(|text| valid_note(text, 240))
            })
        })
    }

    pub(super) fn restore(&self, db: &Connection, local_document_id: &str) -> Result<()> {
        let mut key = [0u8; 32];
        for (slot, pair) in key.iter_mut().zip(self.key.as_bytes().chunks_exact(2)) {
            let pair = std::str::from_utf8(pair).map_err(archive_error)?;
            *slot = u8::from_str_radix(pair, 16).map_err(archive_error)?;
        }
        db.execute(
            "INSERT OR IGNORE INTO model_hints (document_id, cache_key, annotation_blob)
             VALUES (?1,?2,?3)",
            rusqlite::params![local_document_id, key.as_slice(), self.hint.to_string()],
        )
        .map_err(archive_error)?;
        Ok(())
    }
}

pub(super) fn present(db: &Connection) -> Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM model_hints)", [], |row| {
        row.get(0)
    })
    .map_err(archive_error)
}

pub(super) fn write(db: &Connection, output: &mut impl Write) -> Result<Metadata> {
    let mut rows = db
        .prepare(
            "SELECT document_id, lower(hex(cache_key)), annotation_blob
         FROM model_hints ORDER BY document_id, cache_key",
        )
        .map_err(archive_error)?;
    let mut rows = rows.query([]).map_err(archive_error)?;
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(archive_error)? {
        let body: String = row.get(2).map_err(archive_error)?;
        let row = Row {
            document_id: row.get(0).map_err(archive_error)?,
            key: row.get(1).map_err(archive_error)?,
            hint: serde_json::from_str(&body).map_err(archive_error)?,
        };
        if !row.valid() {
            return Err(invalid());
        }
        let mut encoded = serde_json::to_vec(&row).map_err(archive_error)?;
        encoded.push(b'\n');
        bytes += encoded.len() as u64;
        if encoded.len() as u64 > MAX_ROW_BYTES || bytes > MAX_BACKUP_FILE_BYTES {
            return Err(invalid());
        }
        output.write_all(&encoded).map_err(archive_error)?;
        digest.update(&encoded);
        count += 1;
    }
    Ok(Metadata {
        count,
        bytes,
        sha256: format!("{:x}", digest.finalize()),
    })
}

pub(super) fn read(
    input: impl Read,
    metadata: &Metadata,
    mut visit: impl FnMut(Row) -> Result<()>,
) -> Result<()> {
    let mut input = BufReader::new(input);
    let mut buffer = Vec::new();
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut previous = None;
    for _ in 0..metadata.count {
        buffer.clear();
        let read = (&mut input)
            .take(MAX_ROW_BYTES + 1)
            .read_until(b'\n', &mut buffer)
            .map_err(archive_error)? as u64;
        bytes += read;
        if read > MAX_ROW_BYTES || bytes > metadata.bytes || buffer.last() != Some(&b'\n') {
            return Err(invalid());
        }
        let row: Row = serde_json::from_slice(&buffer).map_err(archive_error)?;
        let identity = (row.document_id.as_str(), row.key.as_str());
        if !row.valid()
            || previous
                .as_ref()
                .is_some_and(|(doc, key): &(String, String)| {
                    (doc.as_str(), key.as_str()) >= identity
                })
        {
            return Err(invalid());
        }
        previous = Some((row.document_id.clone(), row.key.clone()));
        digest.update(&buffer);
        visit(row)?;
    }
    if bytes != metadata.bytes
        || !input.fill_buf().map_err(archive_error)?.is_empty()
        || format!("{:x}", digest.finalize()) != metadata.sha256
    {
        return Err(invalid());
    }
    Ok(())
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_note(text: &str, limit: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().take(limit + 1).count() <= limit
        && !text.chars().any(char::is_control)
}

fn invalid() -> String {
    "Некоректний архів збережених підказок локальної моделі.".into()
}
