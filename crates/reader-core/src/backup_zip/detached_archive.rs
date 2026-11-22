use super::archive_error;
use crate::{pdf_detached_pages, Document, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, BufReader, Read, Write};

// One page is held in memory at a time. JSON escaping can expand a text layer.
const MAX_ROW_BYTES: u64 = 192 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Metadata {
    pub count: u16,
    pub bytes: u64,
    pub sha256: String,
}

pub(super) fn entry_name(index: usize) -> String {
    format!("pdf-page-cache/{index}.jsonl")
}

pub(super) fn write(
    db: &Connection,
    document: &Document,
    output: &mut impl Write,
) -> Result<Metadata> {
    let page_count = document.ocr_pdf_pages.ok_or("У резервній копії відсутня розмітка PDF.")?;
    let mut writer = DigestWriter { output, digest: Sha256::new(), bytes: 0 };
    let mut count = 0u16;
    pdf_detached_pages::visit_cached_pages(db, &document.id, |page| {
        page.validate(document, page_count)?;
        let start = writer.bytes;
        serde_json::to_writer(&mut writer, &page).map_err(archive_error)?;
        writer.write_all(b"\n").map_err(archive_error)?;
        if writer.bytes - start > MAX_ROW_BYTES {
            return Err("Запис кешу сторінки PDF завеликий.".into());
        }
        count = count.checked_add(1).ok_or("Забагато кешованих сторінок PDF.")?;
        Ok(())
    })?;
    Ok(Metadata { count, bytes: writer.bytes, sha256: format!("{:x}", writer.digest.finalize()) })
}

pub(super) fn validate(
    input: impl Read,
    metadata: &Metadata,
    document: &Document,
    mut visit: impl FnMut(pdf_detached_pages::ArchivedPage) -> Result<()>,
) -> Result<()> {
    let page_count = document.ocr_pdf_pages.ok_or("У резервній копії відсутня розмітка PDF.")?;
    let invalid = || "Некоректний кеш сторінок PDF у резервній копії.".to_owned();
    let mut input = BufReader::new(input);
    let mut line = Vec::new();
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut previous = None;
    for _ in 0..metadata.count {
        line.clear();
        let bytes = (&mut input).take(MAX_ROW_BYTES + 1)
            .read_until(b'\n', &mut line).map_err(archive_error)? as u64;
        if bytes > MAX_ROW_BYTES || line.last() != Some(&b'\n') {
            return Err(invalid());
        }
        size += bytes;
        digest.update(&line);
        let page: pdf_detached_pages::ArchivedPage = serde_json::from_slice(&line).map_err(archive_error)?;
        if previous.is_some_and(|previous| previous >= page.page) {
            return Err(invalid());
        }
        page.validate(document, page_count)?;
        previous = Some(page.page);
        visit(page)?;
    }
    if !input.fill_buf().map_err(archive_error)?.is_empty() || size != metadata.bytes
        || format!("{:x}", digest.finalize()) != metadata.sha256
    {
        return Err(invalid());
    }
    Ok(())
}

struct DigestWriter<'a, W> {
    output: &'a mut W,
    digest: Sha256,
    bytes: u64,
}

impl<W: Write> Write for DigestWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let size = self.output.write(buf)?;
        self.digest.update(&buf[..size]);
        self.bytes += size as u64;
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
